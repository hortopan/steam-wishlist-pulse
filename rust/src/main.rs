mod anomaly;
mod common;
mod config;
mod crypto;
mod db;
mod discord;
mod error;
mod steam;
mod telegram;
mod web;

use chrono::Utc;
use chrono_tz::US::Pacific;
use colored::Colorize;

use config::AppConfig;
use db::{Database, SnapshotChange};
use steam::SteamClient;
use tracing_subscriber::EnvFilter;
use web::AppState;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("wishlist_pulse=info".parse().unwrap()),
        )
        .with_target(false)
        .init();

    let config = AppConfig::load();

    tracing::info!("Database: {}", config.database_path.display());
    tracing::info!("Web interface: {}", config.bind_web_interface);
    tracing::info!("Poll interval: {} minutes", config.poll_interval_minutes);
    tracing::info!("Backfill rate: {} req/sec", config.backfill_rate);

    let db = Database::open(&config.database_path).unwrap_or_else(|e| {
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

    // ── Encryption secret rotation check ──────────────────────────────
    const CONFIG_ENCRYPTION_SECRET_HASH: &str = "encryption_secret_hash";
    const ENCRYPTED_CONFIG_KEYS: &[&str] =
        &["steam_api_key", "telegram_bot_token", "discord_bot_token"];

    if let Some(ref secret) = config.encryption_secret {
        let new_hash = crypto::hash_secret(secret);
        let stored_hash = db
            .get_config(CONFIG_ENCRYPTION_SECRET_HASH)
            .await
            .ok()
            .flatten();
        if let Some(ref old_hash) = stored_hash {
            if *old_hash != new_hash {
                tracing::warn!(
                    "Encryption secret has changed — removing all encrypted credentials (they were encrypted with the old secret). Please re-enter them via the admin panel."
                );
                for key in ENCRYPTED_CONFIG_KEYS {
                    let _ = db.delete_config(key).await;
                }
                let _ = db
                    .set_config(CONFIG_ENCRYPTION_SECRET_HASH, &new_hash)
                    .await;
            }
        } else {
            // First time setting an encryption secret — remove any existing plaintext credentials
            // since we can't know if they're encrypted or not
            let mut removed = false;
            for key in ENCRYPTED_CONFIG_KEYS {
                if db.get_config(key).await.ok().flatten().is_some() {
                    let _ = db.delete_config(key).await;
                    removed = true;
                }
            }
            if removed {
                tracing::warn!(
                    "Encryption secret set for the first time — removed existing plaintext credentials. Please re-enter them via the admin panel."
                );
            }
            let _ = db
                .set_config(CONFIG_ENCRYPTION_SECRET_HASH, &new_hash)
                .await;
        }
    } else {
        tracing::warn!(
            "No ENCRYPTION_SECRET set — API keys and bot tokens will be stored unencrypted in the database. Set ENCRYPTION_SECRET environment variable for encryption at rest."
        );
        // If encryption was previously enabled but now removed, stored credentials are unusable
        let stored_hash = db
            .get_config(CONFIG_ENCRYPTION_SECRET_HASH)
            .await
            .ok()
            .flatten();
        if stored_hash.is_some() {
            tracing::warn!(
                "Encryption was previously enabled — removing encrypted credentials (cannot decrypt without secret). Please re-enter them via the admin panel."
            );
            for key in ENCRYPTED_CONFIG_KEYS {
                let _ = db.delete_config(key).await;
            }
            let _ = db.delete_config(CONFIG_ENCRYPTION_SECRET_HASH).await;
        }
    }

    // Load Steam API key from database (configured via admin panel)
    let steam_api_key = match db.get_config("steam_api_key").await.ok().flatten() {
        Some(stored) => {
            if let Some(ref secret) = config.encryption_secret {
                match crypto::decrypt(secret, &stored) {
                    Ok(key) => Some(key),
                    Err(e) => {
                        tracing::error!(
                            "Failed to decrypt Steam API key: {e} — removing corrupted entry"
                        );
                        let _ = db.delete_config("steam_api_key").await;
                        None
                    }
                }
            } else {
                Some(stored)
            }
        }
        None => None,
    };
    let steam = steam_api_key.map(|key| {
        tracing::info!("Steam API key configured");
        SteamClient::new(key, config.backfill_rate)
    });

    // Build shared application state
    let app_state = AppState::new(
        db,
        steam,
        config.insecure,
        config.encryption_secret.clone(),
        config.backfill_rate,
    );

    // Kick off version check so the cache is warm before the first request
    {
        let state = app_state.clone();
        tokio::spawn(async move {
            state.get_latest_version().await;
        });
    }

    // Backfill full history for all tracked games on startup
    {
        let state = app_state.clone();
        tokio::spawn(async move {
            backfill_all_games(&state).await;
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

/// Backfill full history for all tracked games.
async fn backfill_all_games(state: &AppState) {
    let steam = match state.get_steam().await {
        Some(s) => s,
        None => {
            tracing::debug!("No Steam API key configured, skipping backfill.");
            return;
        }
    };

    let app_ids = match state.db.get_tracked_game_ids().await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::error!("Failed to get tracked game IDs for backfill: {e}");
            return;
        }
    };

    if app_ids.is_empty() {
        return;
    }

    println!(
        "{}",
        format!(
            "Starting full history backfill for {} game(s)...",
            app_ids.len()
        )
        .cyan()
    );

    for app_id in app_ids {
        let token = state.start_backfill(app_id).await;
        backfill_game_history(state, &steam, app_id, token, "auto", "system").await;
    }

    println!("{}", "History backfill complete.".cyan());
}

/// Backfill full history for a single game, from app_min_date to yesterday.
pub async fn backfill_game_history(
    state: &AppState,
    steam: &SteamClient,
    app_id: u32,
    token: tokio_util::sync::CancellationToken,
    sync_type: &str,
    requested_by: &str,
) {
    use chrono::NaiveDate;

    // 1. Get app_min_date (from DB cache or by fetching current data)
    let min_date_str = match state.db.get_app_min_date(app_id).await {
        Ok(Some(d)) => d,
        _ => {
            // Not cached yet — fetch current data to discover it
            match steam.fetch_wishlist(app_id).await {
                Ok(report) => {
                    if let Some(ref d) = report.app_min_date {
                        let _ = state.db.store_app_min_date(app_id, d).await;
                        d.clone()
                    } else {
                        tracing::debug!("No app_min_date for app {app_id}, skipping backfill");
                        state.finish_backfill(app_id).await;
                        return;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to fetch data for app {app_id} to discover min_date: {e}"
                    );
                    state.finish_backfill(app_id).await;
                    return;
                }
            }
        }
    };

    let min_date = match NaiveDate::parse_from_str(&min_date_str, "%Y-%m-%d") {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("Invalid app_min_date '{min_date_str}' for app {app_id}: {e}");
            state.finish_backfill(app_id).await;
            return;
        }
    };

    let yesterday = Utc::now().with_timezone(&Pacific).date_naive() - chrono::Duration::days(1);
    if min_date > yesterday {
        tracing::debug!(
            "app {app_id}: min_date {min_date} is not before yesterday, nothing to backfill"
        );
        state.finish_backfill(app_id).await;
        return;
    }

    // 2. Load already-crawled and failed dates
    let crawled_dates = match state.db.get_crawled_dates_for_game(app_id).await {
        Ok(dates) => dates,
        Err(e) => {
            tracing::error!("Failed to get crawled dates for app {app_id}: {e}");
            state.finish_backfill(app_id).await;
            return;
        }
    };
    let failed_dates = state.db.get_failed_dates(app_id).await.unwrap_or_default();

    // 3. Generate dates to backfill (newest to oldest — recent data first)
    let mut dates_to_fetch: Vec<String> = Vec::new();
    let mut date = yesterday;
    while date >= min_date {
        let date_str = date.format("%Y-%m-%d").to_string();
        if !crawled_dates.contains(&date_str) || failed_dates.contains(&date_str) {
            dates_to_fetch.push(date_str);
        }
        date -= chrono::Duration::days(1);
    }

    if dates_to_fetch.is_empty() {
        tracing::debug!("app {app_id}: full history already backfilled");
        state.finish_backfill(app_id).await;
        return;
    }

    let total = dates_to_fetch.len();

    // Record sync start in DB
    let _ = state
        .db
        .start_sync(app_id, sync_type, requested_by, total as u64)
        .await;
    println!(
        "{}",
        format!("  app {app_id}: backfilling {total} day(s) from {min_date_str}...").cyan()
    );

    let mut backfilled = 0u32;
    let mut consecutive_failures = 0u32;

    for (i, date_str) in dates_to_fetch.iter().enumerate() {
        // Check cancellation
        if token.is_cancelled() {
            println!(
                "{}",
                format!("  app {app_id}: backfill cancelled (game untracked)").yellow()
            );
            return; // Don't call finish_backfill — game is being removed
        }

        match steam.fetch_wishlist_for_backfill(app_id, date_str).await {
            Ok(report) => {
                let fetched_at = format!("{date_str}T23:59:59Z");
                if let Err(e) = state
                    .db
                    .insert_backfill_snapshot(&report, &fetched_at)
                    .await
                {
                    tracing::error!("Failed to store backfill for app {app_id} on {date_str}: {e}");
                } else {
                    backfilled += 1;
                }
                if let Err(e) = state.db.mark_date_crawled(app_id, date_str).await {
                    tracing::error!("Failed to mark {date_str} crawled for app {app_id}: {e}");
                }
                // Clear from failed dates if it was a retry
                if failed_dates.contains(date_str) {
                    let _ = state.db.clear_failed_date(app_id, date_str).await;
                }
                consecutive_failures = 0;
            }
            Err(e) => {
                // "No data" errors (date before app existed, etc.) are expected — mark crawled
                let err_str = e.to_string();
                if err_str.contains("No data for app") || err_str.contains("No wishlist data") {
                    if let Err(e2) = state.db.mark_date_crawled(app_id, date_str).await {
                        tracing::error!("Failed to mark {date_str} crawled for app {app_id}: {e2}");
                    }
                    if failed_dates.contains(date_str) {
                        let _ = state.db.clear_failed_date(app_id, date_str).await;
                    }
                    consecutive_failures = 0;
                } else {
                    // Actual API failure
                    let _ = state.db.mark_date_failed(app_id, date_str).await;
                    consecutive_failures += 1;
                    tracing::warn!("Backfill failed for app {app_id} on {date_str}: {e}");

                    if consecutive_failures >= 5 {
                        println!("{}", format!("  app {app_id}: {consecutive_failures} consecutive failures, pausing 60s...").yellow());
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;

                        // Check cancellation after pause
                        if token.is_cancelled() {
                            println!(
                                "{}",
                                format!("  app {app_id}: backfill cancelled during pause").yellow()
                            );
                            return;
                        }

                        // Try one more time
                        match steam.fetch_wishlist_for_backfill(app_id, date_str).await {
                            Ok(report) => {
                                let fetched_at = format!("{date_str}T23:59:59Z");
                                let _ = state
                                    .db
                                    .insert_backfill_snapshot(&report, &fetched_at)
                                    .await;
                                let _ = state.db.mark_date_crawled(app_id, date_str).await;
                                let _ = state.db.clear_failed_date(app_id, date_str).await;
                                backfilled += 1;
                                consecutive_failures = 0;
                            }
                            Err(_) => {
                                println!("{}", format!("  app {app_id}: still failing after pause, aborting backfill (will resume on next startup)").yellow());
                                let _ = state.db.fail_sync(app_id).await;
                                state.finish_backfill(app_id).await;
                                return;
                            }
                        }
                    }
                }
            }
        }

        // Log progress every 50 dates
        if (i + 1) % 50 == 0 {
            println!(
                "{}",
                format!("  app {app_id}: backfilled {}/{total} dates...", i + 1).cyan()
            );
        }
    }

    if backfilled > 0 {
        println!(
            "{}",
            format!("  app {app_id}: backfill complete — {backfilled} new day(s)").cyan()
        );
    } else {
        tracing::debug!("app {app_id}: no new data during backfill");
    }

    state.finish_backfill(app_id).await;
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

                            // Run anomaly detection
                            let anomaly_config = state.get_anomaly_config().await;
                            let anomaly_result = anomaly::detect_anomalies(
                                &state.db,
                                report.app_id,
                                &report,
                                &previous,
                                &anomaly_config,
                            )
                            .await;

                            // Check notification mode
                            let notification_mode = state.get_notification_mode().await;
                            let is_real_anomaly = anomaly_result.is_anomalous
                                && !anomaly_result.insufficient_data
                                && !anomaly_result.error;
                            let should_notify = if notification_mode == "anomalies_only" {
                                if anomaly_result.error {
                                    tracing::warn!(
                                        "Anomaly detection failed for app {} — skipping notification (transient error)",
                                        report.app_id,
                                    );
                                    false
                                } else if is_real_anomaly {
                                    tracing::info!(
                                        "Anomaly detected for app {} — sending notification",
                                        report.app_id,
                                    );
                                    true
                                } else if anomaly_result.insufficient_data {
                                    tracing::info!(
                                        "Insufficient data for anomaly detection on app {} — sending notification as fallback",
                                        report.app_id,
                                    );
                                    true
                                } else {
                                    tracing::info!(
                                        "No anomaly for app {} — skipping notification (anomalies_only mode)",
                                        report.app_id,
                                    );
                                    false
                                }
                            } else {
                                true
                            };

                            if should_notify {
                                // Only attach anomaly context when we have a real detection
                                let anomaly_ref = if is_real_anomaly {
                                    Some(&anomaly_result)
                                } else {
                                    None
                                };
                                telegram::notify_change(
                                    &state.db,
                                    report.app_id,
                                    &report,
                                    &previous,
                                    anomaly_ref,
                                )
                                .await;
                                discord::notify_change(
                                    &state.db,
                                    report.app_id,
                                    &report,
                                    &previous,
                                    anomaly_ref,
                                )
                                .await;
                            }
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
    }
}
