use serenity::all::{
    ChannelId, Command, CommandInteraction, CommandOptionType, Context, CreateCommand,
    CreateCommandOption, CreateEmbed, CreateInteractionResponse,
    CreateMessage, EditInteractionResponse, EventHandler,
    GatewayIntents, Ready,
};
use serenity::async_trait;
use serenity::Client;

use crate::common::{BotContext, ChangeMessage, is_admin, prepare_notification, resolve_app_name_short};
use crate::db::Database;
use crate::error::{AppError, AppResult};
use crate::steam::{SteamClient, WishlistReport};

/// Shared state accessible from Discord event handlers.
struct Handler {
    ctx: BotContext,
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        tracing::info!("Discord bot connected as {}", ready.user.name);

        // Register slash commands globally
        let commands = vec![
            CreateCommand::new("help")
                .description("Show available commands"),
            CreateCommand::new("list")
                .description("List currently tracked games"),
            CreateCommand::new("track")
                .description("Track a game by app ID")
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::Integer,
                        "app_id",
                        "Steam app ID to track",
                    )
                    .required(true),
                ),
            CreateCommand::new("untrack")
                .description("Untrack a game by app ID")
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::Integer,
                        "app_id",
                        "Steam app ID to untrack",
                    )
                    .required(true),
                ),
            CreateCommand::new("status")
                .description("Fetch current wishlist stats"),
            CreateCommand::new("subscribe")
                .description("Subscribe this channel to a tracked game")
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::Integer,
                        "app_id",
                        "Steam app ID to subscribe to",
                    )
                    .required(true),
                ),
            CreateCommand::new("unsubscribe")
                .description("Unsubscribe this channel from a game")
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::Integer,
                        "app_id",
                        "Steam app ID to unsubscribe from",
                    )
                    .required(true),
                ),
            CreateCommand::new("subscriptions")
                .description("List this channel's subscriptions"),
            CreateCommand::new("whoami")
                .description("Show your Discord user ID"),
        ];

        match Command::set_global_commands(&ctx.http, commands).await {
            Ok(_) => tracing::info!("Discord slash commands registered"),
            Err(e) => tracing::warn!("Failed to register Discord slash commands: {e}"),
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: serenity::all::Interaction) {
        if let serenity::all::Interaction::Command(cmd) = interaction
            && let Err(e) = self.handle_command(&ctx, &cmd).await
        {
            tracing::error!("Error handling Discord command /{}: {e}", cmd.data.name);
        }
    }
}

impl Handler {
    async fn handle_command(
        &self,
        ctx: &Context,
        cmd: &CommandInteraction,
    ) -> Result<(), String> {
        let user_id = cmd.user.id.get();

        // Defer immediately to avoid Discord's 3-second interaction timeout.
        self.defer(ctx, cmd).await?;

        match cmd.data.name.as_str() {
            "help" => {
                self.edit_response(ctx, cmd, &self.help_text()).await?;
            }
            "whoami" => {
                self.edit_response(ctx, cmd, "Your Discord user ID is:")
                    .await?;
                cmd.channel_id
                    .say(&ctx.http, format!("`{user_id}`"))
                    .await
                    .map_err(|e| format!("Failed to send follow-up: {e}"))?;
            }
            "list" | "track" | "untrack" | "status" | "subscribe" | "unsubscribe"
            | "subscriptions" => {
                if !is_admin(user_id, &self.ctx.admin_ids) {
                    self.edit_response(
                        ctx,
                        cmd,
                        &format!(
                            "This bot is managed by its admins. You don't have permission to use this command.\n\nYour user ID is: `{user_id}`"
                        ),
                    )
                    .await?;
                    return Ok(());
                }
                match cmd.data.name.as_str() {
                    "list" => self.cmd_list(ctx, cmd).await?,
                    "track" => self.cmd_track(ctx, cmd).await?,
                    "untrack" => self.cmd_untrack(ctx, cmd).await?,
                    "status" => self.cmd_status(ctx, cmd).await?,
                    "subscribe" => self.cmd_subscribe(ctx, cmd).await?,
                    "unsubscribe" => self.cmd_unsubscribe(ctx, cmd).await?,
                    "subscriptions" => self.cmd_subscriptions(ctx, cmd).await?,
                    _ => unreachable!(),
                }
            }
            _ => {
                self.edit_response(ctx, cmd, "Unknown command. Use /help to see available commands.")
                    .await?;
            }
        }
        Ok(())
    }

    fn help_text(&self) -> String {
        "**Wishlist Pulse Bot — Commands**\n\
         `/help` — Show this message\n\
         `/list` — List tracked games\n\
         `/track <app_id>` — Track a game\n\
         `/untrack <app_id>` — Untrack a game\n\
         `/status` — Current wishlist stats\n\
         `/subscribe <app_id>` — Subscribe this channel\n\
         `/unsubscribe <app_id>` — Unsubscribe this channel\n\
         `/subscriptions` — List channel subscriptions\n\
         `/whoami` — Show your Discord user ID"
            .to_string()
    }

    async fn defer(
        &self,
        ctx: &Context,
        cmd: &CommandInteraction,
    ) -> Result<(), String> {
        cmd.create_response(&ctx.http, CreateInteractionResponse::Defer(Default::default()))
            .await
            .map_err(|e| format!("Failed to defer: {e}"))
    }

    async fn edit_response(
        &self,
        ctx: &Context,
        cmd: &CommandInteraction,
        content: &str,
    ) -> Result<(), String> {
        cmd.edit_response(&ctx.http, EditInteractionResponse::new().content(content))
            .await
            .map_err(|e| format!("Failed to edit response: {e}"))?;
        Ok(())
    }

    async fn cmd_list(&self, ctx: &Context, cmd: &CommandInteraction) -> Result<(), String> {
        let tracked = self.ctx.db.get_tracked_game_ids().await.map_err(|e| e.to_string())?;
        if tracked.is_empty() {
            self.edit_response(ctx, cmd, "No games being tracked.").await?;
            return Ok(());
        }

        let app_info = self.ctx.db.get_all_app_info().await.unwrap_or_default();
        let lines: Vec<String> = tracked
            .iter()
            .map(|&id| format!("• **{}** ({id})", resolve_app_name_short(id, &app_info)))
            .collect();

        self.edit_response(ctx, cmd, &format!("**Tracked games:**\n{}", lines.join("\n")))
            .await
    }

    async fn cmd_track(&self, ctx: &Context, cmd: &CommandInteraction) -> Result<(), String> {
        let steam = match &self.ctx.steam {
            Some(s) => s,
            None => {
                self.edit_response(ctx, cmd, "Steam API key is not configured. Please set it up in the admin panel first.").await?;
                return Ok(());
            }
        };

        let app_id = self.get_int_option(cmd, "app_id")? as u32;

        match self.ctx.db.is_tracked(app_id).await {
            Ok(true) => {
                self.edit_response(ctx, cmd, &format!("Already tracking app {app_id}."))
                    .await?;
                return Ok(());
            }
            Err(e) => {
                self.edit_response(ctx, cmd, "Something went wrong. Please try again later.")
                    .await?;
                tracing::error!("Failed to check tracking status: {e}");
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
                let _ = self.ctx.db.upsert_app_info(app_id, &n, image_url).await;
                n
            }
            Err(e) => {
                self.edit_response(ctx, cmd, &e.to_string()).await?;
                return Ok(());
            }
        };

        if let Err(e) = self.ctx.db.add_tracked_game(app_id).await {
            tracing::error!("Failed to add tracked game {app_id}: {e}");
            self.edit_response(ctx, cmd, "Failed to save. Please try again later.")
                .await?;
            return Ok(());
        }

        self.edit_response(ctx, cmd, &format!("Now tracking **{name}** ({app_id})"))
            .await
    }

    async fn cmd_untrack(&self, ctx: &Context, cmd: &CommandInteraction) -> Result<(), String> {
        let app_id = self.get_int_option(cmd, "app_id")? as u32;

        match self.ctx.db.remove_tracked_game(app_id).await {
            Ok(true) => {
                self.edit_response(ctx, cmd, &format!("Untracked app {app_id}."))
                    .await
            }
            Ok(false) => {
                self.edit_response(ctx, cmd, &format!("App {app_id} was not being tracked."))
                    .await
            }
            Err(e) => {
                tracing::error!("Failed to untrack {app_id}: {e}");
                self.edit_response(ctx, cmd, "Something went wrong. Please try again later.")
                    .await
            }
        }
    }

    async fn cmd_status(&self, ctx: &Context, cmd: &CommandInteraction) -> Result<(), String> {
        let tracked = self.ctx.db.get_tracked_game_ids().await.map_err(|e| e.to_string())?;
        if tracked.is_empty() {
            self.edit_response(ctx, cmd, "No games being tracked. Use /track to add games.")
                .await?;
            return Ok(());
        }

        let snapshots = self.ctx.db.get_latest_snapshots().await.map_err(|e| e.to_string())?;
        if snapshots.is_empty() {
            self.edit_response(
                ctx,
                cmd,
                "No data yet. Waiting for the next background poll to fetch stats.",
            )
            .await?;
            return Ok(());
        }

        let app_info = self.ctx.db.get_all_app_info().await.unwrap_or_default();
        let mut lines = Vec::new();

        for report in snapshots {
            let name = resolve_app_name_short(report.app_id, &app_info);
            lines.push(format!(
                "**{}** ({}) ({})\n+{} adds / -{} deletes / {} purchases / {} gifts",
                name, report.app_id, report.date, report.adds, report.deletes, report.purchases, report.gifts,
            ));
        }

        self.edit_response(ctx, cmd, &lines.join("\n\n")).await
    }

    async fn cmd_subscribe(&self, ctx: &Context, cmd: &CommandInteraction) -> Result<(), String> {
        let app_id = self.get_int_option(cmd, "app_id")? as u32;
        let channel_id = cmd.channel_id.get().to_string();

        match self.ctx.db.is_tracked(app_id).await {
            Ok(false) => {
                self.edit_response(ctx, cmd, &format!("App {app_id} is not being tracked. Use /track first."))
                    .await?;
                return Ok(());
            }
            Err(e) => {
                tracing::error!("Failed to check tracking: {e}");
                self.edit_response(ctx, cmd, "Something went wrong. Please try again later.")
                    .await?;
                return Ok(());
            }
            Ok(true) => {}
        }

        match self.ctx.db.subscribe_channel("discord", &channel_id, app_id).await {
            Ok(true) => {
                let app_info = self.ctx.db.get_all_app_info().await.unwrap_or_default();
                let name = resolve_app_name_short(app_id, &app_info);
                self.edit_response(ctx, cmd, &format!("Subscribed this channel to **{name}** ({app_id})"))
                    .await
            }
            Ok(false) => {
                self.edit_response(ctx, cmd, "This channel is already subscribed to that game.")
                    .await
            }
            Err(e) => {
                tracing::error!("Failed to subscribe: {e}");
                self.edit_response(ctx, cmd, "Something went wrong. Please try again.")
                    .await
            }
        }
    }

    async fn cmd_unsubscribe(
        &self,
        ctx: &Context,
        cmd: &CommandInteraction,
    ) -> Result<(), String> {
        let app_id = self.get_int_option(cmd, "app_id")? as u32;
        let channel_id = cmd.channel_id.get().to_string();

        match self
            .ctx
            .db
            .unsubscribe_channel("discord", &channel_id, app_id)
            .await
        {
            Ok(true) => {
                let app_info = self.ctx.db.get_all_app_info().await.unwrap_or_default();
                let name = resolve_app_name_short(app_id, &app_info);
                self.edit_response(
                    ctx,
                    cmd,
                    &format!("Unsubscribed this channel from **{name}** ({app_id})"),
                )
                .await
            }
            Ok(false) => {
                self.edit_response(ctx, cmd, "This channel was not subscribed to that game.")
                    .await
            }
            Err(e) => {
                tracing::error!("Failed to unsubscribe: {e}");
                self.edit_response(ctx, cmd, "Something went wrong. Please try again.")
                    .await
            }
        }
    }

    async fn cmd_subscriptions(
        &self,
        ctx: &Context,
        cmd: &CommandInteraction,
    ) -> Result<(), String> {
        let channel_id = cmd.channel_id.get().to_string();

        let subs = self
            .ctx
            .db
            .get_subscriptions_for_channel("discord", &channel_id)
            .await
            .map_err(|e| e.to_string())?;

        if subs.is_empty() {
            self.edit_response(ctx, cmd, "This channel has no subscriptions.")
                .await?;
            return Ok(());
        }

        let app_info = self.ctx.db.get_all_app_info().await.unwrap_or_default();
        let lines: Vec<String> = subs
            .iter()
            .map(|&id| format!("• **{}**", resolve_app_name_short(id, &app_info)))
            .collect();

        self.edit_response(
            ctx,
            cmd,
            &format!("**Subscriptions for this channel:**\n{}", lines.join("\n")),
        )
        .await
    }

    fn get_int_option(&self, cmd: &CommandInteraction, name: &str) -> Result<i64, String> {
        cmd.data
            .options
            .iter()
            .find(|o| o.name == name)
            .and_then(|o| o.value.as_i64())
            .ok_or_else(|| format!("Missing required option: {name}"))
    }
}

/// Validate a Discord bot token by attempting to get the current user.
/// Returns Ok(bot_username) on success, or an error message.
pub async fn validate_token(token: &str) -> AppResult<String> {
    let http = serenity::http::Http::new(token);
    match http.get_current_user().await {
        Ok(user) => Ok(user.name.clone()),
        Err(e) => Err(AppError::other(format!("Invalid Discord bot token: {e}"))),
    }
}

/// Run the Discord bot. This blocks until the bot disconnects.
pub async fn run_bot(
    token: String,
    steam: Option<SteamClient>,
    db: Database,
    admin_ids: Vec<u64>,
) {
    let handler = Handler {
        ctx: BotContext {
            db,
            steam,
            admin_ids,
        },
    };

    let intents = GatewayIntents::empty();

    let mut client = match Client::builder(&token, intents)
        .event_handler(handler)
        .await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to create Discord client: {e}");
            return;
        }
    };

    if let Err(e) = client.start().await {
        tracing::error!("Discord client error: {e}");
    }
}

/// Send change notifications to all Discord channels subscribed to a game.
pub async fn notify_change(
    db: &Database,
    app_id: u32,
    current: &WishlistReport,
    previous: &WishlistReport,
    anomaly: Option<&crate::anomaly::AnomalyResult>,
) {
    let ctx = match prepare_notification(db, "discord", app_id).await {
        Some(ctx) => ctx,
        None => return,
    };

    let msg = ChangeMessage::new(ctx.app_name, current, previous, anomaly);

    let http = serenity::http::Http::new(&ctx.token);

    let color = if msg.anomaly_flags.is_some() { 0xff4444 } else { 0x1b96f3 };

    let fmt_field = |name: &str, value: &str, flag: Option<&crate::common::MetricAnomalyFlag>| -> (String, String) {
        match flag {
            Some(f) if f.is_anomalous => (
                format!("{name} ⚠️"),
                format!("{value}\n*{}*", f.detail),
            ),
            _ => (name.to_string(), value.to_string()),
        }
    };

    let (adds_label, adds_value, deletes_label, deletes_value, purchases_label, purchases_value, gifts_label, gifts_value) =
        match &msg.anomaly_flags {
            Some(f) => {
                let (al, av) = fmt_field("Adds", &msg.adds, Some(&f.adds));
                let (dl, dv) = fmt_field("Deletes", &msg.deletes, Some(&f.deletes));
                let (pl, pv) = fmt_field("Purchases", &msg.purchases, Some(&f.purchases));
                let (gl, gv) = fmt_field("Gifts", &msg.gifts, Some(&f.gifts));
                (al, av, dl, dv, pl, pv, gl, gv)
            }
            None => {
                let (al, av) = fmt_field("Adds", &msg.adds, None);
                let (dl, dv) = fmt_field("Deletes", &msg.deletes, None);
                let (pl, pv) = fmt_field("Purchases", &msg.purchases, None);
                let (gl, gv) = fmt_field("Gifts", &msg.gifts, None);
                (al, av, dl, dv, pl, pv, gl, gv)
            }
        };

    for channel_id_str in &ctx.channels {
        let channel_id: u64 = match channel_id_str.parse() {
            Ok(id) => id,
            Err(_) => {
                tracing::warn!("Invalid Discord channel ID: {channel_id_str}");
                continue;
            }
        };

        let mut embed = CreateEmbed::new()
            .title(format!("{} ({app_id})", msg.app_name))
            .description(msg.header())
            .color(color)
            .field(&adds_label, &adds_value, true)
            .field(&deletes_label, &deletes_value, true)
            .field(&purchases_label, &purchases_value, true)
            .field(&gifts_label, &gifts_value, true);

        if let Some(flags) = &msg.anomaly_flags
            && !flags.country_alerts.is_empty()
        {
            let alerts = flags.country_alerts.join("\n");
            embed = embed.field("Country anomalies", &alerts, false);
        }

        let message = CreateMessage::new().embed(embed);

        match ChannelId::new(channel_id).send_message(&http, message).await {
            Ok(_) => {
                tracing::info!(
                    "Sent notification to Discord channel {channel_id_str} for app {app_id}"
                );
            }
            Err(e) => {
                tracing::error!(
                    "Failed to send notification to Discord channel {channel_id_str}: {e}"
                );
            }
        }
    }
}
