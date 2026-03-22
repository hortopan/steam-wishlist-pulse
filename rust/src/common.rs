use std::cmp::Ordering;
use std::collections::HashMap;

use crate::db::Database;
use crate::steam::{AppInfo, SteamClient, WishlistReport};

/// Shared state for bot handlers (Telegram & Discord).
#[derive(Clone)]
pub struct BotContext {
    pub db: Database,
    pub steam: Option<SteamClient>,
    pub admin_ids: Vec<u64>,
}

/// Check whether a user ID is in the admin list.
pub fn is_admin(user_id: u64, admin_ids: &[u64]) -> bool {
    admin_ids.contains(&user_id)
}

/// Format a signed delta value for display (e.g. "+5", "-3", "0").
pub fn fmt_delta(new: u64, old: u64) -> String {
    match new.cmp(&old) {
        Ordering::Greater => format!("+{}", new - old),
        Ordering::Less => format!("-{}", old - new),
        Ordering::Equal => "0".to_string(),
    }
}

/// Resolve a display name for an app, checking DB app_info first, then in-memory steam cache.
pub fn resolve_app_name(
    app_id: u32,
    app_info: &HashMap<u32, (String, String)>,
    mem_names: &HashMap<u32, AppInfo>,
) -> String {
    if let Some((name, _)) = app_info.get(&app_id) {
        format!("{name} ({app_id})")
    } else if let Some(info) = mem_names.get(&app_id) {
        format!("{} ({app_id})", info.name)
    } else {
        format!("App {app_id}")
    }
}

/// Resolve just the short name (no app_id suffix).
pub fn resolve_app_name_short(
    app_id: u32,
    app_info: &HashMap<u32, (String, String)>,
) -> String {
    app_info
        .get(&app_id)
        .map(|(n, _)| n.clone())
        .unwrap_or_else(|| format!("App {app_id}"))
}

impl BotContext {
    /// Fetch both DB app_info and in-memory steam names.
    pub async fn fetch_name_sources(
        &self,
    ) -> (HashMap<u32, (String, String)>, HashMap<u32, AppInfo>) {
        let app_info = self.db.get_all_app_info().await.unwrap_or_default();
        let mem_names = match &self.steam {
            Some(s) => s.app_info().await,
            None => Default::default(),
        };
        (app_info, mem_names)
    }

    /// Build display lines for a list of app IDs (e.g. "• Name (id)").
    pub async fn format_app_list(&self, ids: &[u32], prefix: &str) -> Vec<String> {
        let (app_info, mem_names) = self.fetch_name_sources().await;
        ids.iter()
            .map(|&id| format!("{prefix}{}", resolve_app_name(id, &app_info, &mem_names)))
            .collect()
    }
}

/// Data needed to send a notification, shared across providers.
pub struct NotificationContext {
    pub token: String,
    pub app_name: String,
    pub channels: Vec<String>,
}

/// Prepare notification data for a given provider. Returns None if notifications
/// should not be sent (disabled, no token, no channels).
pub async fn prepare_notification(
    db: &Database,
    provider: &str,
    app_id: u32,
) -> Option<NotificationContext> {
    let token_key = format!("{provider}_bot_token");
    let enabled_key = format!("{provider}_enabled");

    let token = match db.get_config(&token_key).await.ok().flatten() {
        Some(t) if !t.is_empty() => t,
        _ => return None,
    };

    let enabled = db.get_config(&enabled_key).await.ok().flatten();
    if enabled.as_deref() != Some("true") {
        return None;
    }

    let channels = match db.get_subscribed_channels(app_id).await {
        Ok(ch) => ch,
        Err(e) => {
            tracing::error!("Failed to get subscribed channels for app {app_id}: {e}");
            return None;
        }
    };

    let filtered: Vec<String> = channels
        .into_iter()
        .filter(|(p, _)| p == provider)
        .map(|(_, id)| id)
        .collect();

    if filtered.is_empty() {
        return None;
    }

    let app_info = db.get_all_app_info().await.unwrap_or_default();
    let app_name = app_info
        .get(&app_id)
        .map(|(n, _)| n.clone())
        .unwrap_or_else(|| "Unknown".to_string());

    Some(NotificationContext {
        token,
        app_name,
        channels: filtered,
    })
}

/// Format the four delta fields for a notification.
pub fn format_deltas(current: &WishlistReport, previous: &WishlistReport) -> [String; 4] {
    [
        format!(
            "{} → {} ({})",
            previous.adds,
            current.adds,
            fmt_delta(current.adds, previous.adds)
        ),
        format!(
            "{} → {} ({})",
            previous.deletes,
            current.deletes,
            fmt_delta(current.deletes, previous.deletes)
        ),
        format!(
            "{} → {} ({})",
            previous.purchases,
            current.purchases,
            fmt_delta(current.purchases, previous.purchases)
        ),
        format!(
            "{} → {} ({})",
            previous.gifts,
            current.gifts,
            fmt_delta(current.gifts, previous.gifts)
        ),
    ]
}
