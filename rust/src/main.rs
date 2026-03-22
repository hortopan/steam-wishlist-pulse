mod common;
mod config;
mod db;
mod discord;
mod error;
mod steam;
mod telegram;
mod web;

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
    let app_state = AppState::new(db, steam, config.insecure);

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
