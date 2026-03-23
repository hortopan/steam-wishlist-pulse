mod common;
mod config;
mod db;
mod discord;
mod error;
mod steam;
mod telegram;
mod web;

use chrono::Utc;
use chrono_tz::US::Pacific;

use config::AppConfig;
use db::{Database, SnapshotChange};
use steam::SteamClient;
use tracing_subscriber::EnvFilter;
use web::AppState;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive("wishlist_pulse=info".parse().unwrap()),
        )
        .with_target(false)
        .init();

    let config = AppConfig::load();

    tracing::info!("Database: {}", config.database_path.display());
    tracing::info!("Web interface: {}", config.bind_web_interface);
    tracing::info!("Poll interval: {} minutes", config.poll_interval_minutes);
    tracing::info!("Auto-populate days: {}", config.auto_populate_days);

    let db = Database::open(&config.database_path)
        .unwrap_or_else(|e| {
            eprintln!("Failed to open database: {e}");
            std::process::exit(1);
        });

    // Initialize passwords from CLI/env if provided
    web::init_passwords_from_config(
        &db,
        config.admin_password.as_deref(),
        config.read_password.as_deref(),
    )
    .await;

    // Load Steam API key from database (configured via admin panel)
    let steam_api_key = db.get_config("steam_api_key").await.ok().flatten();
    let steam = steam_api_key.map(|key| {
        tracing::info!("Steam API key configured");
        SteamClient::new(key)
    });

    // Build shared application state
    let app_state = AppState::new(db, steam, config.insecure, config.auto_populate_days);

    // Auto-populate historical data on startup
    if config.auto_populate_days > 0 {
        let state = app_state.clone();
        tokio::spawn(async move {
            auto_populate_all_games(&state).await;
        });
    }

    // Spawn the background polling loop (reads steam client from shared state each tick)
    {
        let state = app_state.clone();
        let poll_interval_minutes = config.poll_interval_minutes;
        tokio::spawn(async move {
            polling_loop(state, poll_interval_minutes).await;
        });
    }

    // Start telegram bot if configured
    app_state.restart_telegram().await;

    // Start discord bot if configured
    app_state.restart_discord().await;

    // Web interface always runs (blocking)
    web::run_web(config.bind_web_interface, app_state).await;
}

/// Auto-populate historical data for all tracked games.
async fn auto_populate_all_games(state: &AppState) {
    let steam = match state.get_steam().await {
        Some(s) => s,
        None => {
            tracing::debug!("No Steam API key configured, skipping auto-populate.");
            return;
        }
    };

    let app_ids = match state.db.get_tracked_game_ids().await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::error!("Failed to get tracked game IDs for auto-populate: {e}");
            return;
        }
    };

    if app_ids.is_empty() {
        return;
    }

    tracing::info!("Auto-populating historical data for {} game(s), {} day(s) back...", app_ids.len(), state.auto_populate_days);

    for app_id in app_ids {
        auto_populate_game(state, &steam, app_id).await;
    }

    tracing::info!("Auto-populate complete.");
}

/// Auto-populate historical data for a single game, backfilling missing days.
pub async fn auto_populate_game(state: &AppState, steam: &SteamClient, app_id: u32) {
    let days = state.auto_populate_days;
    if days == 0 {
        return;
    }

    let crawled_dates = match state.db.get_crawled_dates_for_game(app_id).await {
        Ok(dates) => dates,
        Err(e) => {
            tracing::error!("Failed to get crawled dates for app {app_id}: {e}");
            return;
        }
    };

    let today = Utc::now().with_timezone(&Pacific).date_naive();
    let mut backfilled = 0u32;

    for days_ago in (1..=days).rev() {
        let date = today - chrono::Duration::days(days_ago as i64);
        let date_str = date.format("%Y-%m-%d").to_string();

        if crawled_dates.contains(&date_str) {
            continue;
        }

        match steam.fetch_wishlist_for_date(app_id, &date_str).await {
            Ok(report) => {
                // Set fetched_at to end-of-day for the historical date so ordering is correct
                let fetched_at = format!("{date_str}T23:59:59Z");
                if let Err(e) = state.db.insert_backfill_snapshot(&report, &fetched_at).await {
                    tracing::error!("Failed to backfill snapshot for app {app_id} on {date_str}: {e}");
                } else {
                    backfilled += 1;
                }
            }
            Err(e) => {
                tracing::debug!("No data for app {app_id} on {date_str}: {e}");
            }
        }

        // Mark as crawled regardless of whether data was available
        if let Err(e) = state.db.mark_date_crawled(app_id, &date_str).await {
            tracing::error!("Failed to mark date {date_str} as crawled for app {app_id}: {e}");
        }
    }

    if backfilled > 0 {
        tracing::info!("Backfilled {backfilled} day(s) of data for app {app_id}");
    }
}

async fn polling_loop(state: AppState, poll_interval_minutes: u64) {
    use std::time::Duration;
    use tokio::time;

    let mut interval = time::interval(Duration::from_secs(poll_interval_minutes * 60));

    loop {
        interval.tick().await;

        // Get the current steam client from shared state (may have been updated via admin)
        let steam = match state.get_steam().await {
            Some(s) => s,
            None => {
                tracing::debug!("No Steam API key configured, skipping poll.");
                continue;
            }
        };

        tracing::info!("Polling Steam wishlist data...");

        let app_ids = match state.db.get_tracked_game_ids().await {
            Ok(ids) => ids,
            Err(e) => {
                tracing::error!("Failed to get tracked game IDs: {e}");
                continue;
            }
        };

        if app_ids.is_empty() {
            tracing::info!("No games tracked, skipping poll.");
            continue;
        }

        // Refresh app info (names & images) for all tracked games
        for &app_id in &app_ids {
            if let Ok(name) = steam.fetch_app_name(app_id).await {
                let info = steam.app_info().await;
                let image_url = info
                    .get(&app_id)
                    .and_then(|a| a.image_url.as_deref())
                    .unwrap_or("");
                if let Err(e) = state.db.upsert_app_info(app_id, &name, image_url).await {
                    tracing::error!("Failed to store app info for {app_id}: {e}");
                }
            }
        }

        let results = steam.fetch_all(&app_ids).await;
        for result in results {
            match result {
                Ok(report) => {
                    tracing::info!(
                        "Fetched data for app {} on {}: +{} adds",
                        report.app_id,
                        report.date,
                        report.adds,
                    );
                    match state.db.insert_snapshot_if_changed(&report).await {
                        Ok(SnapshotChange::Changed { previous }) => {
                            tracing::info!(
                                "Data changed for app {} on {} — snapshot saved",
                                report.app_id,
                                report.date,
                            );
                            telegram::notify_change(
                                &state.db,
                                report.app_id,
                                &report,
                                &previous,
                            )
                            .await;
                            discord::notify_change(
                                &state.db,
                                report.app_id,
                                &report,
                                &previous,
                            )
                            .await;
                        }
                        Ok(SnapshotChange::FirstSnapshot) => {
                            tracing::info!(
                                "First snapshot for app {} on {} — saved (no notification)",
                                report.app_id,
                                report.date,
                            );
                        }
                        Ok(SnapshotChange::NoChange) => {
                            tracing::debug!(
                                "No change for app {} on {}",
                                report.app_id,
                                report.date,
                            );
                        }
                        Err(e) => {
                            tracing::error!("Failed to store snapshot: {e}");
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Poll fetch error: {e}");
                }
            }
        }

        // Purge old data
        let retention_days = state.get_retention_days().await;
        match state.db.purge_old_snapshots(retention_days).await {
            Ok(0) => {}
            Ok(n) => tracing::info!("Purged {n} old snapshot(s)"),
            Err(e) => tracing::error!("Failed to purge old snapshots: {e}"),
        }
    }
}
