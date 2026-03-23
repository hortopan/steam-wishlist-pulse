use teloxide::prelude::*;
use teloxide::types::{BotCommand, InlineKeyboardButton, InlineKeyboardMarkup, Me, ParseMode};
use teloxide::utils::command::BotCommands;

use crate::common::{BotContext, ChangeMessage, is_admin, prepare_notification, resolve_app_name, resolve_app_name_short};
use crate::db::Database;
use crate::error::{AppError, AppResult};
use crate::steam::{SteamClient, WishlistReport};

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "Available commands:")]
pub enum Command {
    #[command(description = "Welcome message")]
    Start,
    #[command(description = "Show available commands")]
    Help,
    #[command(description = "List currently tracked games")]
    List,
    #[command(description = "Track a game by app ID")]
    Track,
    #[command(description = "Untrack a game")]
    Untrack,
    #[command(description = "Fetch current wishlist stats")]
    Status,
    #[command(description = "Subscribe this channel to a game")]
    Subscribe,
    #[command(description = "Unsubscribe this channel from a game")]
    Unsubscribe,
    #[command(description = "List this channel's subscriptions")]
    Subscriptions,
    #[command(description = "Show your Telegram user ID")]
    Whoami,
}

const NO_STEAM_MSG: &str =
    "⚠️ Steam API key is not configured. Please set it up in the admin panel first.";

async fn handle_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    state: BotContext,
) -> ResponseResult<()> {
    let user_id = msg.from.as_ref().map(|u| u.id.0).unwrap_or(0);

    if matches!(cmd, Command::Start) {
        bot.send_message(
            msg.chat.id,
            "👋 Welcome to Wishlist Pulse Bot!\n\n\
             I track Steam game wishlists and deliver updates \
             to Telegram channels.\n\n\
             🔗 https://github.com/hortopan/steam-wishlist-pulse",
        )
        .await?;
        return Ok(());
    }

    if matches!(cmd, Command::Whoami) {
        bot.send_message(
            msg.chat.id,
            format!("Your Telegram user ID is: {}", user_id),
        )
        .await?;
        return Ok(());
    }

    if !is_admin(user_id, &state.admin_ids) {
        bot.send_message(
            msg.chat.id,
            "⛔ This bot is managed by its admins.\n\n\
             👋 Wishlist Pulse Bot tracks Steam game wishlists \
             and delivers updates to Telegram channels.\n\n\
             🔗 https://github.com/hortopan/steam-wishlist-pulse",
        )
        .await?;
        return Ok(());
    }

    match cmd {
        Command::Start | Command::Whoami => unreachable!(),
        Command::Help => {
            bot.send_message(msg.chat.id, Command::descriptions().to_string())
                .await?;
        }
        Command::List => {
            let tracked = match state.db.get_tracked_game_ids().await {
                Ok(ids) => ids,
                Err(e) => {
                    tracing::error!("Failed to get tracked game IDs: {e}");
                    bot.send_message(
                        msg.chat.id,
                        "❌ Something went wrong. Please try again later.",
                    )
                    .await?;
                    return Ok(());
                }
            };
            if tracked.is_empty() {
                bot.send_message(msg.chat.id, "No games being tracked.")
                    .await?;
            } else {
                let lines = state.format_app_list(&tracked, "• ").await;
                bot.send_message(
                    msg.chat.id,
                    format!("🎮 *Tracked games:*\n{}", lines.join("\n")),
                )
                .await?;
            }
        }
        Command::Track => {
            let steam = match &state.steam {
                Some(s) => s,
                None => {
                    bot.send_message(msg.chat.id, NO_STEAM_MSG).await?;
                    return Ok(());
                }
            };

            let text = msg.text().unwrap_or_default();
            let arg = text.split_whitespace().nth(1);

            let app_id = match arg.and_then(|s| s.parse::<u32>().ok()) {
                Some(id) => id,
                None => {
                    bot.send_message(msg.chat.id, "Usage: /track <app_id>\n\nExample: /track 480")
                        .await?;
                    return Ok(());
                }
            };

            match state.db.is_tracked(app_id).await {
                Ok(true) => {
                    bot.send_message(msg.chat.id, format!("Already tracking app {app_id}."))
                        .await?;
                    return Ok(());
                }
                Err(e) => {
                    tracing::error!("Failed to check tracking status for app {app_id}: {e}");
                    bot.send_message(
                        msg.chat.id,
                        "❌ Something went wrong. Please try again later.",
                    )
                    .await?;
                    return Ok(());
                }
                Ok(false) => {}
            }

            let name = match steam.fetch_app_name(app_id).await {
                Ok(n) => {
                    let info = steam.app_info().await;
                    let image_url = info
                        .get(&app_id)
                        .and_then(|a| a.image_url.as_deref())
                        .unwrap_or("");
                    let _ = state.db.upsert_app_info(app_id, &n, image_url).await;
                    n
                }
                Err(e) => {
                    bot.send_message(msg.chat.id, format!("❌ {e}")).await?;
                    return Ok(());
                }
            };

            if let Err(e) = state.db.add_tracked_game(app_id).await {
                tracing::error!("Failed to add tracked game {app_id}: {e}");
                bot.send_message(msg.chat.id, "❌ Failed to save. Please try again later.")
                    .await?;
                return Ok(());
            }

            bot.send_message(msg.chat.id, format!("✅ Now tracking {name} ({app_id})"))
                .await?;
        }
        Command::Untrack => {
            let tracked = match state.db.get_tracked_game_ids().await {
                Ok(ids) => ids,
                Err(e) => {
                    tracing::error!("Failed to get tracked game IDs: {e}");
                    bot.send_message(
                        msg.chat.id,
                        "❌ Something went wrong. Please try again later.",
                    )
                    .await?;
                    return Ok(());
                }
            };
            if tracked.is_empty() {
                bot.send_message(msg.chat.id, "No games being tracked.")
                    .await?;
                return Ok(());
            }

            let (app_info, mem_names) = state.fetch_name_sources().await;
            let buttons: Vec<Vec<InlineKeyboardButton>> = tracked
                .iter()
                .map(|&id| {
                    let label = resolve_app_name(id, &app_info, &mem_names);
                    vec![InlineKeyboardButton::callback(
                        label,
                        format!("untrack:{}", id),
                    )]
                })
                .collect();

            bot.send_message(msg.chat.id, "Select a game to untrack:")
                .reply_markup(InlineKeyboardMarkup::new(buttons))
                .await?;
        }
        Command::Subscribe => {
            let chat_id_str = msg.chat.id.0.to_string();
            let tracked = match state.db.get_tracked_game_ids().await {
                Ok(ids) => ids,
                Err(e) => {
                    tracing::error!("Failed to get tracked game IDs: {e}");
                    bot.send_message(
                        msg.chat.id,
                        "❌ Something went wrong. Please try again later.",
                    )
                    .await?;
                    return Ok(());
                }
            };

            if tracked.is_empty() {
                bot.send_message(msg.chat.id, "No games being tracked. Use /track first.")
                    .await?;
                return Ok(());
            }

            let already_subscribed = match state
                .db
                .get_subscriptions_for_channel("telegram", &chat_id_str)
                .await
            {
                Ok(ids) => ids,
                Err(e) => {
                    tracing::warn!("Failed to get subscriptions: {e}");
                    Default::default()
                }
            };

            let available: Vec<u32> = tracked
                .into_iter()
                .filter(|id| !already_subscribed.contains(id))
                .collect();

            if available.is_empty() {
                bot.send_message(
                    msg.chat.id,
                    "This channel is already subscribed to all tracked games.",
                )
                .await?;
                return Ok(());
            }

            let (app_info, mem_names) = state.fetch_name_sources().await;
            let buttons: Vec<Vec<InlineKeyboardButton>> = available
                .iter()
                .map(|&id| {
                    let label = resolve_app_name(id, &app_info, &mem_names);
                    vec![InlineKeyboardButton::callback(label, format!("sub:{}", id))]
                })
                .collect();

            bot.send_message(msg.chat.id, "Select a game to subscribe to:")
                .reply_markup(InlineKeyboardMarkup::new(buttons))
                .await?;
        }
        Command::Unsubscribe => {
            let chat_id_str = msg.chat.id.0.to_string();
            let subs = match state
                .db
                .get_subscriptions_for_channel("telegram", &chat_id_str)
                .await
            {
                Ok(ids) => ids,
                Err(e) => {
                    tracing::error!("Failed to get subscriptions: {e}");
                    bot.send_message(
                        msg.chat.id,
                        "❌ Something went wrong. Please try again later.",
                    )
                    .await?;
                    return Ok(());
                }
            };

            if subs.is_empty() {
                bot.send_message(msg.chat.id, "This channel has no subscriptions.")
                    .await?;
                return Ok(());
            }

            let (app_info, mem_names) = state.fetch_name_sources().await;
            let buttons: Vec<Vec<InlineKeyboardButton>> = subs
                .iter()
                .map(|&id| {
                    let label = resolve_app_name(id, &app_info, &mem_names);
                    vec![InlineKeyboardButton::callback(
                        label,
                        format!("unsub:{}", id),
                    )]
                })
                .collect();

            bot.send_message(msg.chat.id, "Select a game to unsubscribe from:")
                .reply_markup(InlineKeyboardMarkup::new(buttons))
                .await?;
        }
        Command::Subscriptions => {
            let chat_id_str = msg.chat.id.0.to_string();
            let subs = match state
                .db
                .get_subscriptions_for_channel("telegram", &chat_id_str)
                .await
            {
                Ok(ids) => ids,
                Err(e) => {
                    tracing::error!("Failed to get subscriptions: {e}");
                    bot.send_message(
                        msg.chat.id,
                        "❌ Something went wrong. Please try again later.",
                    )
                    .await?;
                    return Ok(());
                }
            };

            if subs.is_empty() {
                bot.send_message(msg.chat.id, "This channel has no subscriptions.")
                    .await?;
            } else {
                let lines = state.format_app_list(&subs, "• ").await;
                bot.send_message(
                    msg.chat.id,
                    format!("📬 *Subscriptions for this channel:*\n{}", lines.join("\n")),
                )
                .await?;
            }
        }
        Command::Status => {
            let tracked = match state.db.get_tracked_game_ids().await {
                Ok(ids) => ids,
                Err(e) => {
                    tracing::error!("Failed to get tracked game IDs: {e}");
                    bot.send_message(
                        msg.chat.id,
                        "❌ Something went wrong. Please try again later.",
                    )
                    .await?;
                    return Ok(());
                }
            };

            if tracked.is_empty() {
                bot.send_message(
                    msg.chat.id,
                    "No games being tracked. Use /track to add games.",
                )
                .await?;
                return Ok(());
            }

            let snapshots = match state.db.get_latest_snapshots().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("Failed to get latest snapshots: {e}");
                    bot.send_message(
                        msg.chat.id,
                        "❌ Something went wrong. Please try again later.",
                    )
                    .await?;
                    return Ok(());
                }
            };

            if snapshots.is_empty() {
                bot.send_message(
                    msg.chat.id,
                    "No data yet. Waiting for the next background poll to fetch stats.",
                )
                .await?;
                return Ok(());
            }

            let app_info = state.db.get_all_app_info().await.unwrap_or_default();
            let mut lines = Vec::new();

            for report in snapshots {
                let name = resolve_app_name_short(report.app_id, &app_info);
                lines.push(format!(
                    "📊 {} ({}) ({})\n   +{} adds / -{} deletes / {} purchases / {} gifts",
                    name, report.app_id, report.date, report.adds, report.deletes, report.purchases, report.gifts,
                ));
            }

            bot.send_message(msg.chat.id, lines.join("\n")).await?;
        }
    }

    Ok(())
}

async fn handle_callback(bot: Bot, q: CallbackQuery, state: BotContext) -> ResponseResult<()> {
    let data = match q.data.as_deref() {
        Some(d) => d,
        None => return Ok(()),
    };

    let user_id = q.from.id.0;
    if !is_admin(user_id, &state.admin_ids) {
        bot.answer_callback_query(&q.id)
            .text("⛔ Unauthorized")
            .await?;
        return Ok(());
    }

    if let Some(id_str) = data.strip_prefix("sub:") {
        if let Ok(app_id) = id_str.parse::<u32>() {
            let chat_id_str = match q.message.as_ref() {
                Some(m) => m.chat().id.0.to_string(),
                None => {
                    bot.answer_callback_query(&q.id)
                        .text("❌ Message expired. Please use /subscribe again.")
                        .await?;
                    return Ok(());
                }
            };
            match state
                .db
                .subscribe_channel("telegram", &chat_id_str, app_id)
                .await
            {
                Ok(true) => {
                    let app_info = state.db.get_all_app_info().await.unwrap_or_default();
                    let name = resolve_app_name_short(app_id, &app_info);
                    bot.answer_callback_query(&q.id)
                        .text(format!("✅ Subscribed to {name}"))
                        .await?;
                    if let Some(ref msg) = q.message {
                        bot.edit_message_text(
                            msg.chat().id,
                            msg.id(),
                            format!("✅ Subscribed to {name} ({app_id})"),
                        )
                        .await
                        .ok();
                    }
                }
                Ok(false) => {
                    bot.answer_callback_query(&q.id)
                        .text("Already subscribed.")
                        .await?;
                }
                Err(e) => {
                    tracing::error!("Failed to subscribe channel to app {app_id}: {e}");
                    bot.answer_callback_query(&q.id)
                        .text("❌ Something went wrong. Please try again.")
                        .await?;
                }
            }
        }
        return Ok(());
    }

    if let Some(id_str) = data.strip_prefix("unsub:") {
        if let Ok(app_id) = id_str.parse::<u32>() {
            let chat_id_str = match q.message.as_ref() {
                Some(m) => m.chat().id.0.to_string(),
                None => {
                    bot.answer_callback_query(&q.id)
                        .text("❌ Message expired. Please use /unsubscribe again.")
                        .await?;
                    return Ok(());
                }
            };
            if let Err(e) = state
                .db
                .unsubscribe_channel("telegram", &chat_id_str, app_id)
                .await
            {
                tracing::error!("Failed to unsubscribe channel from app {app_id}: {e}");
                bot.answer_callback_query(&q.id)
                    .text("❌ Something went wrong. Please try again.")
                    .await?;
                return Ok(());
            }
            let app_info = state.db.get_all_app_info().await.unwrap_or_default();
            let name = resolve_app_name_short(app_id, &app_info);
            bot.answer_callback_query(&q.id)
                .text(format!("🗑 Unsubscribed from {name}"))
                .await?;
            if let Some(ref msg) = q.message {
                bot.edit_message_text(
                    msg.chat().id,
                    msg.id(),
                    format!("🗑 Unsubscribed from {name}"),
                )
                .await
                .ok();
            }
        }
        return Ok(());
    }

    if let Some(id_str) = data.strip_prefix("untrack:")
        && let Ok(app_id) = id_str.parse::<u32>()
    {
        if let Err(e) = state.db.remove_tracked_game(app_id).await {
            tracing::error!("Failed to remove tracked game {app_id}: {e}");
            bot.answer_callback_query(&q.id)
                .text("❌ Something went wrong. Please try again.")
                .await?;
            return Ok(());
        }
        let (app_info, mem_names) = state.fetch_name_sources().await;
        let name = resolve_app_name(app_id, &app_info, &mem_names);
        bot.answer_callback_query(&q.id)
            .text(format!("🗑 Untracked {name}"))
            .await?;
        if let Some(msg) = q.message {
            bot.edit_message_text(msg.chat().id, msg.id(), format!("🗑 Untracked {name}"))
                .await
                .ok();
        }
    }

    Ok(())
}

async fn handle_unknown(bot: Bot, msg: Message) -> ResponseResult<()> {
    if let Some(text) = msg.text()
        && text.starts_with('/')
    {
        let cmd_part = text.split_whitespace().next().unwrap_or(text);
        let truncated: String = cmd_part.chars().take(64).collect();
        bot.send_message(
            msg.chat.id,
            format!(
                "Unknown command: {}\n\n{}",
                truncated,
                Command::descriptions()
            ),
        )
        .await?;
    }
    Ok(())
}

async fn register_commands(bot: &Bot) {
    let commands = vec![
        BotCommand::new("start", "Welcome message"),
        BotCommand::new("help", "Show available commands"),
        BotCommand::new("list", "List currently tracked games"),
        BotCommand::new("track", "Track a game"),
        BotCommand::new("untrack", "Untrack a game"),
        BotCommand::new("status", "Fetch current wishlist stats"),
        BotCommand::new("subscribe", "Subscribe this channel to a game"),
        BotCommand::new("unsubscribe", "Unsubscribe this channel from a game"),
        BotCommand::new("subscriptions", "List this channel's subscriptions"),
        BotCommand::new("whoami", "Show your Telegram user ID"),
    ];

    match bot.set_my_commands(commands).await {
        Ok(_) => tracing::info!("Bot commands registered with Telegram"),
        Err(e) => tracing::warn!("Failed to register bot commands: {e}"),
    }
}

/// Validate a Telegram bot token by calling getMe.
/// Returns Ok(bot_username) on success, or an error message.
pub async fn validate_token(token: &str) -> AppResult<String> {
    let bot = Bot::new(token.to_string());
    match bot.get_me().await {
        Ok(Me { user: me, .. }) => Ok(me.username.unwrap_or_else(|| "unknown".to_string())),
        Err(e) => Err(AppError::other(format!("Invalid Telegram bot token: {e}"))),
    }
}

pub async fn run_bot(token: String, steam: Option<SteamClient>, db: Database, admin_ids: Vec<u64>) {
    let bot = Bot::new(token);

    let Me { user: me, .. } = match bot.get_me().await {
        Ok(me) => me,
        Err(e) => {
            tracing::error!("Failed to connect to Telegram API: {e}");
            return;
        }
    };
    tracing::info!(
        "Telegram bot started as @{}",
        me.username.as_deref().unwrap_or("unknown")
    );

    register_commands(&bot).await;

    // Pre-fetch names for tracked games
    if let Some(ref steam) = steam {
        let tracked_ids = db.get_tracked_game_ids().await.unwrap_or_default();
        for app_id in &tracked_ids {
            match steam.fetch_app_name(*app_id).await {
                Ok(name) => tracing::info!("Tracking: {name} ({app_id})"),
                Err(e) => tracing::warn!("Could not resolve name for app {app_id}: {e}"),
            }
        }
    }

    let state = BotContext {
        steam,
        db,
        admin_ids,
    };

    let command_handler = Update::filter_message()
        .filter_command::<Command>()
        .endpoint({
            let state = state.clone();
            move |bot: Bot, msg: Message, cmd: Command| {
                let state = state.clone();
                async move { handle_command(bot, msg, cmd, state).await }
            }
        });

    let callback_handler = Update::filter_callback_query().endpoint({
        let state = state.clone();
        move |bot: Bot, q: CallbackQuery| {
            let state = state.clone();
            async move { handle_callback(bot, q, state).await }
        }
    });

    let unknown_handler = Update::filter_message().endpoint(handle_unknown);

    let handler = dptree::entry()
        .branch(command_handler)
        .branch(callback_handler)
        .branch(unknown_handler);

    Dispatcher::builder(bot, handler).build().dispatch().await;
}

/// Send change notifications to all Telegram channels subscribed to a game.
pub async fn notify_change(
    db: &Database,
    app_id: u32,
    current: &WishlistReport,
    previous: &WishlistReport,
    anomaly: Option<&crate::anomaly::AnomalyResult>,
) {
    let ctx = match prepare_notification(db, "telegram", app_id).await {
        Some(ctx) => ctx,
        None => return,
    };

    let msg = ChangeMessage::new(ctx.app_name, current, previous, anomaly);

    let emoji = if msg.anomaly_flags.is_some() { "🚨" } else { "📊" };

    let fmt_metric = |label: &str, value: &str, flag: Option<&crate::common::MetricAnomalyFlag>| -> String {
        match flag {
            Some(f) if f.is_anomalous => format!("{label}: {value} ⚠️\n  <i>{}</i>", f.detail),
            _ => format!("{label}: {value}"),
        }
    };

    let (adds_line, deletes_line, purchases_line, gifts_line) = match &msg.anomaly_flags {
        Some(f) => (
            fmt_metric("Adds", &msg.adds, Some(&f.adds)),
            fmt_metric("Deletes", &msg.deletes, Some(&f.deletes)),
            fmt_metric("Purchases", &msg.purchases, Some(&f.purchases)),
            fmt_metric("Gifts", &msg.gifts, Some(&f.gifts)),
        ),
        None => (
            fmt_metric("Adds", &msg.adds, None),
            fmt_metric("Deletes", &msg.deletes, None),
            fmt_metric("Purchases", &msg.purchases, None),
            fmt_metric("Gifts", &msg.gifts, None),
        ),
    };

    let mut message = format!(
        "{emoji} <b>{}</b> ({app_id}) > {}\n\n{adds_line}\n{deletes_line}\n{purchases_line}\n{gifts_line}",
        msg.app_name, msg.header(),
    );

    if let Some(flags) = &msg.anomaly_flags
        && !flags.country_alerts.is_empty()
    {
        message.push_str("\n\n<b>Country anomalies:</b>");
        for alert in &flags.country_alerts {
            message.push_str(&format!("\n  {alert}"));
        }
    }

    let bot = Bot::new(ctx.token);

    for channel_id in &ctx.channels {
        let chat_id: i64 = match channel_id.parse() {
            Ok(id) => id,
            Err(_) => {
                tracing::warn!("Invalid Telegram chat ID: {channel_id}");
                continue;
            }
        };
        match bot
            .send_message(ChatId(chat_id), &message)
            .parse_mode(ParseMode::Html)
            .await
        {
            Ok(_) => {
                tracing::info!("Sent notification to Telegram chat {channel_id} for app {app_id}");
            }
            Err(e) => {
                tracing::error!("Failed to send notification to Telegram chat {channel_id}: {e}");
            }
        }
    }
}
