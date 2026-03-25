use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, Uri, header};
use axum::middleware;
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use rand::Rng;
use rust_embed::Embed;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use time::Duration;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use crate::db::Database;
use crate::steam::SteamClient;

const SESSION_COOKIE: &str = "wpb_session";
const CSRF_COOKIE: &str = "wpb_csrf";
const CSRF_HEADER: &str = "x-csrf-token";
const CONFIG_ADMIN_PASSWORD_HASH: &str = "admin_password_hash";
const CONFIG_READ_PASSWORD_HASH: &str = "read_password_hash";
const CONFIG_STEAM_API_KEY: &str = "steam_api_key";
const CONFIG_TELEGRAM_BOT_TOKEN: &str = "telegram_bot_token";
const CONFIG_TELEGRAM_ADMIN_IDS: &str = "telegram_admin_ids";
const CONFIG_TELEGRAM_ENABLED: &str = "telegram_enabled";
const CONFIG_DISCORD_BOT_TOKEN: &str = "discord_bot_token";
const CONFIG_DISCORD_ADMIN_IDS: &str = "discord_admin_ids";
const CONFIG_DISCORD_ENABLED: &str = "discord_enabled";
const CONFIG_JWT_SECRET: &str = "jwt_secret";
const CONFIG_NOTIFICATION_MODE: &str = "notification_mode";
const CONFIG_ANOMALY_LOOKBACK_DAYS: &str = "anomaly_lookback_days";
const CONFIG_ANOMALY_SENSITIVITY_UP: &str = "anomaly_sensitivity_up";
const CONFIG_ANOMALY_SENSITIVITY_DOWN: &str = "anomaly_sensitivity_down";
const CONFIG_ANOMALY_MIN_ABSOLUTE: &str = "anomaly_min_absolute";
const CONFIG_ANOMALY_MAD_FLOOR_PCT: &str = "anomaly_mad_floor_pct";

#[derive(Embed)]
#[folder = "../web/dist"]
struct Assets;

// ── Session management (JWT) ────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessLevel {
    Admin,
    ReadOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JwtClaims {
    access_level: AccessLevel,
    exp: u64,
}

fn generate_jwt_secret() -> String {
    let mut rng = rand::rng();
    let bytes: [u8; 32] = rng.random();
    hex::encode(bytes)
}

fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("Argon2 hashing should not fail")
        .to_string()
}

fn verify_password(password: &str, hash: &str) -> bool {
    let parsed = match PasswordHash::new(hash) {
        Ok(h) => h,
        Err(_) => return false,
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

// ── Rate limiting ───────────────────────────────────────────────────

const MAX_LOGIN_ATTEMPTS: usize = 5;
const RATE_LIMIT_WINDOW_SECS: u64 = 300; // 5 minutes

type RateLimiter = Arc<tokio::sync::Mutex<HashMap<String, Vec<Instant>>>>;

/// Check the rate limit and, if under the limit, **preemptively record** the
/// attempt in one atomic operation (single lock hold). On successful login the
/// caller must call `clear_attempts` to remove the preemptive record.
fn check_and_record_attempt(
    attempts: &mut HashMap<String, Vec<Instant>>,
    key: &str,
) -> Result<(), u64> {
    let now = Instant::now();
    let window = std::time::Duration::from_secs(RATE_LIMIT_WINDOW_SECS);

    // Prune all stale entries to prevent unbounded memory growth
    attempts.retain(|_, v| {
        v.retain(|t| now.duration_since(*t) < window);
        !v.is_empty()
    });

    let entry = attempts.entry(key.to_string()).or_default();

    if entry.len() >= MAX_LOGIN_ATTEMPTS {
        let oldest = entry[0];
        let retry_after = window
            .as_secs()
            .saturating_sub(now.duration_since(oldest).as_secs());
        return Err(retry_after);
    }

    // Record attempt preemptively — cleared on success
    entry.push(now);
    Ok(())
}

fn clear_attempts(attempts: &mut HashMap<String, Vec<Instant>>, key: &str) {
    attempts.remove(key);
}

// ── Shared application state ────────────────────────────────────────

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub steam: Arc<RwLock<Option<SteamClient>>>,
    pub telegram_handle: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
    pub discord_handle: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
    login_attempts: RateLimiter,
    cached_jwt_secret: Arc<tokio::sync::Mutex<Option<String>>>,
    insecure: bool,
    latest_version: Arc<tokio::sync::Mutex<Option<(String, Instant)>>>,
    encryption_secret: Option<SecretString>,
    backfill_tokens: Arc<tokio::sync::Mutex<HashMap<u32, tokio_util::sync::CancellationToken>>>,
    pub backfill_rate: f64,
}

impl AppState {
    pub fn new(
        db: Database,
        steam: Option<SteamClient>,
        insecure: bool,
        encryption_secret: Option<SecretString>,
        backfill_rate: f64,
    ) -> Self {
        if insecure {
            tracing::warn!("Running with --insecure: cookies will not require HTTPS");
        }
        Self {
            db,
            steam: Arc::new(RwLock::new(steam)),
            telegram_handle: Arc::new(tokio::sync::Mutex::new(None)),
            discord_handle: Arc::new(tokio::sync::Mutex::new(None)),
            login_attempts: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            cached_jwt_secret: Arc::new(tokio::sync::Mutex::new(None)),
            insecure,
            latest_version: Arc::new(tokio::sync::Mutex::new(None)),
            encryption_secret,
            backfill_tokens: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            backfill_rate,
        }
    }

    /// Encrypt a value if an encryption secret is configured, otherwise return plaintext.
    fn encrypt_value(&self, plaintext: &str) -> Result<String, String> {
        match &self.encryption_secret {
            Some(secret) => crate::crypto::encrypt(secret, plaintext),
            None => Ok(plaintext.to_string()),
        }
    }

    /// Decrypt a value if an encryption secret is configured, otherwise return as-is.
    fn decrypt_value(&self, stored: &str) -> Result<String, String> {
        match &self.encryption_secret {
            Some(secret) => crate::crypto::decrypt(secret, stored),
            None => Ok(stored.to_string()),
        }
    }

    pub fn encryption_enabled(&self) -> bool {
        self.encryption_secret.is_some()
    }

    /// Create a cancellation token for a backfill task and store it.
    /// If a backfill is already running for this app, it is cancelled first.
    pub async fn start_backfill(&self, app_id: u32) -> tokio_util::sync::CancellationToken {
        let token = tokio_util::sync::CancellationToken::new();
        let mut tokens = self.backfill_tokens.lock().await;
        if let Some(old) = tokens.remove(&app_id) {
            old.cancel();
        }
        tokens.insert(app_id, token.clone());
        token
    }

    /// Cancel an in-progress backfill for a game.
    pub async fn cancel_backfill(&self, app_id: u32) {
        if let Some(token) = self.backfill_tokens.lock().await.remove(&app_id) {
            token.cancel();
        }
    }

    /// Remove the backfill token without updating sync status.
    /// Use this for early exits before `start_sync` has been called.
    pub async fn cancel_backfill_token(&self, app_id: u32) {
        self.backfill_tokens.lock().await.remove(&app_id);
    }

    /// Remove a backfill token and mark the sync as completed in the DB.
    pub async fn finish_backfill(&self, app_id: u32) {
        self.backfill_tokens.lock().await.remove(&app_id);
        let _ = self.db.complete_sync(app_id).await;
    }

    pub async fn get_notification_mode(&self) -> String {
        self.db
            .get_config(CONFIG_NOTIFICATION_MODE)
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "every_update".to_string())
    }

    pub async fn get_anomaly_config(&self) -> crate::anomaly::AnomalyConfig {
        let lookback_days = self
            .db
            .get_config(CONFIG_ANOMALY_LOOKBACK_DAYS)
            .await
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(14);
        // Support legacy single "anomaly_sensitivity" key: if up/down are not set,
        // fall back to the legacy key, then to the default.
        let legacy_sensitivity: Option<f64> = self
            .db
            .get_config("anomaly_sensitivity")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok());
        let default_sens = legacy_sensitivity.unwrap_or(2.0);
        let sensitivity_up = self
            .db
            .get_config(CONFIG_ANOMALY_SENSITIVITY_UP)
            .await
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default_sens);
        let sensitivity_down = self
            .db
            .get_config(CONFIG_ANOMALY_SENSITIVITY_DOWN)
            .await
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default_sens);
        let min_absolute = self
            .db
            .get_config(CONFIG_ANOMALY_MIN_ABSOLUTE)
            .await
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5);
        let mad_floor_pct = self
            .db
            .get_config(CONFIG_ANOMALY_MAD_FLOOR_PCT)
            .await
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.05);
        crate::anomaly::AnomalyConfig {
            lookback_days,
            sensitivity_up,
            sensitivity_down,
            min_absolute,
            mad_floor_pct,
        }
    }

    /// Return the latest known version from cache. If the cache is stale or empty,
    /// spawn a background task to refresh it — never block the caller.
    pub async fn get_latest_version(&self) -> Option<String> {
        const CACHE_DURATION: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

        let cached = self.latest_version.lock().await;
        let stale = match *cached {
            Some((_, ref fetched_at)) => fetched_at.elapsed() >= CACHE_DURATION,
            None => true,
        };
        let current = cached.as_ref().map(|(v, _)| v.clone());
        drop(cached);

        if stale {
            let cache = Arc::clone(&self.latest_version);
            tokio::spawn(async move {
                if let Some(version) = Self::fetch_latest_github_version().await {
                    let current_version = env!("CARGO_PKG_VERSION");
                    if version != current_version {
                        use colored::Colorize;
                        println!(
                            "\n  {} {} → {} {}\n",
                            "UPDATE AVAILABLE:".bold().yellow(),
                            format!("v{current_version}").dimmed(),
                            format!("v{version}").green().bold(),
                            "https://github.com/hortopan/steam-wishlist-pulse/releases/latest"
                                .cyan()
                                .underline()
                        );
                    }
                    *cache.lock().await = Some((version, Instant::now()));
                }
            });
        }

        current
    }

    async fn fetch_latest_github_version() -> Option<String> {
        #[derive(Deserialize)]
        struct GithubRelease {
            tag_name: String,
        }

        let client = reqwest::Client::builder()
            .user_agent("wishlist-pulse")
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .ok()?;

        let release: GithubRelease = client
            .get("https://api.github.com/repos/hortopan/steam-wishlist-pulse/releases/latest")
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;

        Some(release.tag_name.trim_start_matches('v').to_string())
    }

    /// Get or create the JWT signing secret (cached to avoid DB hits on every request).
    /// The secret is stored encrypted in the DB when encryption is enabled.
    async fn jwt_secret(&self) -> String {
        let mut cached = self.cached_jwt_secret.lock().await;
        if let Some(ref secret) = *cached {
            return secret.clone();
        }
        if let Ok(Some(stored)) = self.db.get_config(CONFIG_JWT_SECRET).await {
            let secret = self.decrypt_value(&stored).unwrap_or(stored);
            *cached = Some(secret.clone());
            return secret;
        }
        let secret = generate_jwt_secret();
        let store_value = self
            .encrypt_value(&secret)
            .unwrap_or_else(|_| secret.clone());
        let _ = self.db.set_config(CONFIG_JWT_SECRET, &store_value).await;
        *cached = Some(secret.clone());
        secret
    }

    /// Rotate the JWT secret, invalidating all existing tokens.
    async fn rotate_jwt_secret(&self) {
        let secret = generate_jwt_secret();
        let store_value = self
            .encrypt_value(&secret)
            .unwrap_or_else(|_| secret.clone());
        let _ = self.db.set_config(CONFIG_JWT_SECRET, &store_value).await;
        *self.cached_jwt_secret.lock().await = Some(secret);
    }

    /// Create a signed JWT for the given access level and duration.
    async fn create_token(&self, access_level: AccessLevel, duration_days: i64) -> String {
        let secret = self.jwt_secret().await;
        let exp = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs())
            + (duration_days as u64 * 86400);
        let claims = JwtClaims { access_level, exp };
        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .expect("JWT encoding should not fail")
    }

    /// Build a session cookie with the appropriate flags.
    fn session_cookie(&self, token: String, max_age_days: i64) -> Cookie<'static> {
        let mut builder = Cookie::build((SESSION_COOKIE, token))
            .path("/")
            .http_only(true)
            .same_site(SameSite::Lax)
            .max_age(Duration::days(max_age_days));
        if !self.insecure {
            builder = builder.secure(true);
        }
        builder.build()
    }

    /// Build a CSRF cookie with a fresh random token.
    fn csrf_cookie(&self) -> Cookie<'static> {
        let mut rng = rand::rng();
        let bytes: [u8; 16] = rng.random();
        let token = hex::encode(bytes);
        let mut builder = Cookie::build((CSRF_COOKIE, token))
            .path("/")
            .http_only(false) // JS must be able to read this
            .same_site(SameSite::Lax)
            .max_age(Duration::days(7));
        if !self.insecure {
            builder = builder.secure(true);
        }
        builder.build()
    }

    /// Validate the session cookie and return claims if valid.
    async fn get_session(&self, jar: &CookieJar) -> Option<JwtClaims> {
        let token = jar.get(SESSION_COOKIE)?.value().to_string();
        let secret = self.jwt_secret().await;
        let token_data = decode::<JwtClaims>(
            &token,
            &DecodingKey::from_secret(secret.as_bytes()),
            &Validation::default(),
        )
        .ok()?;
        Some(token_data.claims)
    }

    async fn passwords_configured(&self) -> bool {
        self.db
            .get_config(CONFIG_ADMIN_PASSWORD_HASH)
            .await
            .ok()
            .flatten()
            .is_some()
    }

    pub async fn get_steam(&self) -> Option<SteamClient> {
        self.steam.read().await.clone()
    }

    /// Build a SyncStatusResponse for a single game by combining DB state with live progress.
    async fn build_sync_status(&self, sync_row: &crate::db::GameSyncRow) -> SyncStatusResponse {
        let is_syncing = sync_row.status == "in_progress";
        let progress_crawled = if is_syncing {
            self.db
                .get_crawled_dates_count(sync_row.app_id)
                .await
                .unwrap_or(0)
        } else {
            sync_row.total_dates
        };

        let cooldown_active = sync_row.status == "completed"
            && sync_row.sync_type == "full"
            && is_within_cooldown(sync_row.completed_at.as_deref());

        let last_completed_at = if sync_row.status == "completed" {
            sync_row.completed_at.clone()
        } else {
            None
        };

        SyncStatusResponse {
            app_id: sync_row.app_id,
            is_syncing,
            sync_type: Some(sync_row.sync_type.clone()),
            started_at: Some(sync_row.started_at.clone()),
            completed_at: sync_row.completed_at.clone(),
            progress_crawled,
            progress_total: sync_row.total_dates,
            last_completed_at,
            cooldown_active,
            requested_by: sync_row.requested_by.clone(),
        }
    }

    /// Ensure a SteamClient exists with the given key. Creates one or updates existing.
    async fn ensure_steam(&self, key: &str) {
        let mut guard = self.steam.write().await;
        match *guard {
            Some(ref client) => client.set_api_key(key.to_string()).await,
            None => *guard = Some(SteamClient::new(key.to_string(), self.backfill_rate)),
        }
    }

    /// (Re)start the telegram bot with current DB config.
    pub async fn restart_telegram(&self) {
        let mut handle = self.telegram_handle.lock().await;

        // Abort existing bot if running
        if let Some(h) = handle.take() {
            h.abort();
            tracing::info!("Stopped previous Telegram bot instance");
        }

        let token_raw = self
            .db
            .get_config(CONFIG_TELEGRAM_BOT_TOKEN)
            .await
            .ok()
            .flatten();
        let ids_str = self
            .db
            .get_config(CONFIG_TELEGRAM_ADMIN_IDS)
            .await
            .ok()
            .flatten();
        let enabled = self
            .db
            .get_config(CONFIG_TELEGRAM_ENABLED)
            .await
            .ok()
            .flatten();

        if enabled.as_deref() != Some("true") {
            tracing::info!("Telegram bot is disabled");
            return;
        }

        let token = match token_raw {
            Some(t) if !t.is_empty() => match self.decrypt_value(&t) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("Failed to decrypt Telegram bot token: {e}");
                    return;
                }
            },
            _ => {
                tracing::info!("Telegram bot token not configured");
                return;
            }
        };

        let admin_ids: Vec<u64> = ids_str
            .map(|h| {
                h.split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| s.parse::<u64>().ok())
                    .collect()
            })
            .unwrap_or_default();

        if admin_ids.is_empty() {
            tracing::warn!(
                "Telegram admin IDs not configured — bot will start but admin commands are disabled. Use /whoami to find your ID."
            );
        }

        let steam = self.get_steam().await;
        if steam.is_none() {
            tracing::warn!(
                "Steam API key not configured — Telegram bot will start with limited functionality"
            );
        }

        let db = self.db.clone();
        tracing::info!("Starting Telegram bot with admin IDs: {:?}", admin_ids);
        let h = tokio::spawn(async move {
            crate::telegram::run_bot(token, steam, db, admin_ids).await;
        });
        *handle = Some(h);
    }

    /// (Re)start the Discord bot with current DB config.
    pub async fn restart_discord(&self) {
        let mut handle = self.discord_handle.lock().await;

        // Abort existing bot if running
        if let Some(h) = handle.take() {
            h.abort();
            tracing::info!("Stopped previous Discord bot instance");
        }

        let token_raw = self
            .db
            .get_config(CONFIG_DISCORD_BOT_TOKEN)
            .await
            .ok()
            .flatten();
        let ids_str = self
            .db
            .get_config(CONFIG_DISCORD_ADMIN_IDS)
            .await
            .ok()
            .flatten();
        let enabled = self
            .db
            .get_config(CONFIG_DISCORD_ENABLED)
            .await
            .ok()
            .flatten();

        if enabled.as_deref() != Some("true") {
            tracing::info!("Discord bot is disabled");
            return;
        }

        let token = match token_raw {
            Some(t) if !t.is_empty() => match self.decrypt_value(&t) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("Failed to decrypt Discord bot token: {e}");
                    return;
                }
            },
            _ => {
                tracing::info!("Discord bot token not configured");
                return;
            }
        };

        let admin_ids: Vec<u64> = ids_str
            .map(|h| {
                h.split(',')
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| s.parse::<u64>().ok())
                    .collect()
            })
            .unwrap_or_default();

        if admin_ids.is_empty() {
            tracing::warn!(
                "Discord admin IDs not configured — bot will start but admin commands are disabled"
            );
        }

        let steam = self.get_steam().await;
        if steam.is_none() {
            tracing::warn!(
                "Steam API key not configured — Discord bot will start with limited functionality"
            );
        }

        let db = self.db.clone();
        tracing::info!("Starting Discord bot with admin IDs: {:?}", admin_ids);
        let h = tokio::spawn(async move {
            crate::discord::run_bot(token, steam, db, admin_ids).await;
        });
        *handle = Some(h);
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Parse a UTC timestamp string and return whether it falls within the last 30 minutes.
/// Accepts RFC 3339 (which includes the `%Y-%m-%dT%H:%M:%SZ` format produced by SQLite).
fn is_within_cooldown(completed_at: Option<&str>) -> bool {
    let Some(completed_at) = completed_at else {
        return false;
    };
    chrono::DateTime::parse_from_rfc3339(completed_at)
        .map(|completed| {
            let elapsed = chrono::Utc::now() - completed.with_timezone(&chrono::Utc);
            elapsed < chrono::TimeDelta::minutes(30)
        })
        .unwrap_or(false)
}

/// Normalize a date string to a full ISO 8601 timestamp for JS compatibility.
/// Passes through values that already contain a `T` separator (i.e. full timestamps),
/// and appends `T00:00:00Z` to bare `YYYY-MM-DD` dates.
fn normalize_date_to_iso(date: &str) -> String {
    if date.is_empty() {
        String::new()
    } else if date.contains('T') {
        date.to_owned()
    } else {
        format!("{date}T00:00:00Z")
    }
}

// ── API types ───────────────────────────────────────────────────────

#[derive(Serialize)]
struct AuthStatus {
    authenticated: bool,
    access_level: Option<AccessLevel>,
    setup_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    latest_version: Option<String>,
}

#[derive(Deserialize)]
struct LoginRequest {
    password: String,
}

#[derive(Serialize)]
struct LoginResponse {
    success: bool,
    access_level: Option<AccessLevel>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct SetupRequest {
    admin_password: String,
    read_password: Option<String>,
}

#[derive(Serialize)]
struct SetupResponse {
    success: bool,
    error: Option<String>,
}

#[derive(Serialize)]
struct GameReport {
    app_id: u32,
    name: String,
    image_url: String,
    date: String,
    adds: i64,
    deletes: i64,
    purchases: i64,
    gifts: i64,
    adds_windows: i64,
    adds_mac: i64,
    adds_linux: i64,
    countries: Vec<crate::steam::CountryReport>,
    changed_at: Option<String>,
    total_adds: i64,
    total_deletes: i64,
    total_purchases: i64,
    total_gifts: i64,
}

#[derive(Serialize)]
struct ApiResponse {
    games: Vec<GameReport>,
}

// ── API response types ──────────────────────────────────────────────

#[derive(Serialize, Default, Clone)]
struct AnomalyMetrics {
    adds: bool,
    deletes: bool,
    purchases: bool,
    gifts: bool,
    /// Human-readable descriptions for each anomalous metric.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    descriptions: Vec<String>,
}

/// Lightweight game detail (metadata + latest snapshot only, no history).
#[derive(Serialize)]
struct GameDetailResponse {
    app_id: u32,
    name: String,
    image_url: String,
    latest: Option<GameReport>,
    total_snapshots: usize,
}

/// A single aggregated chart data point.
#[derive(Serialize)]
struct ChartPointResponse {
    label: String,
    adds: i64,
    deletes: i64,
    purchases: i64,
    gifts: i64,
    adds_windows: i64,
    adds_mac: i64,
    adds_linux: i64,
    is_anomaly: bool,
    anomaly_metrics: AnomalyMetrics,
}

/// Chart data response with resolution metadata.
#[derive(Serialize)]
struct ChartResponse {
    resolution: String,
    points: Vec<ChartPointResponse>,
}

/// A history entry without countries (lightweight).
#[derive(Serialize)]
struct HistoryEntry {
    snapshot_id: i64,
    date: String,
    adds: i64,
    deletes: i64,
    purchases: i64,
    gifts: i64,
    adds_windows: i64,
    adds_mac: i64,
    adds_linux: i64,
    fetched_at: String,
    is_anomaly: bool,
    anomaly_metrics: AnomalyMetrics,
}

/// Paginated history response.
#[derive(Serialize)]
struct PaginatedHistoryResponse {
    entries: Vec<HistoryEntry>,
    total: usize,
    page: usize,
    per_page: usize,
}

/// Country data for a single snapshot.
#[derive(Serialize)]
struct SnapshotCountriesResponse {
    snapshot_id: i64,
    countries: Vec<crate::steam::CountryReport>,
}

#[derive(Deserialize)]
struct AdminConfigUpdate {
    steam_api_key: Option<String>,
    telegram_bot_token: Option<String>,
    telegram_admin_ids: Option<String>,
    telegram_enabled: Option<bool>,
    discord_bot_token: Option<String>,
    discord_admin_ids: Option<String>,
    discord_enabled: Option<bool>,
    notification_mode: Option<String>,
    anomaly_lookback_days: Option<u32>,
    anomaly_sensitivity_up: Option<f64>,
    anomaly_sensitivity_down: Option<f64>,
    anomaly_min_absolute: Option<i64>,
    anomaly_mad_floor_pct: Option<f64>,
}

#[derive(Serialize)]
struct AdminConfigResponse {
    has_steam_api_key: bool,
    encryption_enabled: bool,
    has_telegram_bot_token: bool,
    has_discord_bot_token: bool,
    telegram_admin_ids: Option<String>,
    telegram_enabled: bool,
    discord_admin_ids: Option<String>,
    discord_enabled: bool,
    notification_mode: String,
    anomaly_lookback_days: u32,
    anomaly_sensitivity_up: f64,
    anomaly_sensitivity_down: f64,
    anomaly_min_absolute: i64,
    anomaly_mad_floor_pct: f64,
}

#[derive(Deserialize)]
struct ChangePasswordRequest {
    current_password: String,
    new_admin_password: Option<String>,
    new_read_password: Option<String>,
}

#[derive(Deserialize)]
struct TrackGameRequest {
    /// Steam app ID or a Steam store URL (e.g. https://store.steampowered.com/app/123456/...)
    input: String,
}

#[derive(Deserialize)]
struct UntrackGameRequest {
    app_id: u32,
}

#[derive(Serialize)]
struct TrackedGameEntry {
    app_id: u32,
    name: String,
    image_url: String,
    tracked_since: String,
    is_syncing: bool,
    sync_type: Option<String>,
    sync_progress_crawled: u64,
    sync_progress_total: u64,
    last_sync_completed_at: Option<String>,
    cooldown_active: bool,
}

#[derive(Serialize, Clone)]
struct SyncStatusResponse {
    app_id: u32,
    is_syncing: bool,
    sync_type: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
    progress_crawled: u64,
    progress_total: u64,
    last_completed_at: Option<String>,
    cooldown_active: bool,
    requested_by: Option<String>,
}

#[derive(Deserialize)]
struct SyncRequest {
    app_id: u32,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "snake_case")]
enum ServiceStatus {
    Ok,
    Disabled,
    NotConfigured,
    Error,
}

#[derive(Serialize, Clone)]
struct ServiceHealth {
    status: ServiceStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

#[derive(Serialize)]
struct HealthResponse {
    steam: ServiceHealth,
    telegram: ServiceHealth,
    discord: ServiceHealth,
}

/// Try to extract an app ID from a raw string that is either a numeric ID
/// or a Steam store URL like `https://store.steampowered.com/app/123456/...`.
fn parse_app_id(input: &str) -> Result<u32, String> {
    let input = input.trim();

    // Try plain numeric ID first
    if let Ok(id) = input.parse::<u32>() {
        return Ok(id);
    }

    // Try to extract from a Steam URL: .../app/<id>/...
    if let Some(pos) = input.find("/app/") {
        let after = &input[pos + 5..];
        let id_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !id_str.is_empty()
            && let Ok(id) = id_str.parse::<u32>()
        {
            return Ok(id);
        }
    }

    Err(format!(
        "Could not parse app ID from \"{input}\". Enter a numeric ID or a Steam store URL."
    ))
}

// ── Route handlers ──────────────────────────────────────────────────

async fn api_auth_status(
    State(state): State<AppState>,
    jar: CookieJar,
) -> (CookieJar, Json<AuthStatus>) {
    let setup_required = !state.passwords_configured().await;
    let session = state.get_session(&jar).await;
    let is_authenticated = session.is_some();

    // Only expose version info to authenticated users
    let (version, latest_version) = if is_authenticated {
        let current_version = env!("CARGO_PKG_VERSION");
        let latest = state
            .get_latest_version()
            .await
            .filter(|v| v != current_version);
        (Some(current_version), latest)
    } else {
        (None, None)
    };

    // Always set/refresh a CSRF cookie so the frontend can read it
    let csrf_cookie = state.csrf_cookie();
    let jar = jar.add(csrf_cookie);

    (
        jar,
        Json(AuthStatus {
            authenticated: is_authenticated,
            access_level: session.map(|s| s.access_level),
            setup_required,
            version,
            latest_version,
        }),
    )
}

async fn api_login(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
    jar: CookieJar,
    Json(req): Json<LoginRequest>,
) -> (CookieJar, Json<LoginResponse>) {
    let ip = addr.ip().to_string();

    // Check rate limit and preemptively record the attempt (single lock hold)
    {
        let mut attempts = state.login_attempts.lock().await;
        if let Err(retry_after) = check_and_record_attempt(&mut attempts, &ip) {
            return (
                jar,
                Json(LoginResponse {
                    success: false,
                    access_level: None,
                    error: Some(format!(
                        "Too many login attempts. Try again in {retry_after} seconds."
                    )),
                }),
            );
        }
    }

    let password = req.password.trim();
    if password.is_empty() {
        return (
            jar,
            Json(LoginResponse {
                success: false,
                access_level: None,
                error: Some("Password cannot be empty".into()),
            }),
        );
    }

    let admin_hash = state
        .db
        .get_config(CONFIG_ADMIN_PASSWORD_HASH)
        .await
        .ok()
        .flatten();
    let read_hash = state
        .db
        .get_config(CONFIG_READ_PASSWORD_HASH)
        .await
        .ok()
        .flatten();

    // Try admin password first
    if let Some(ref hash) = admin_hash
        && verify_password(password, hash)
    {
        clear_attempts(&mut *state.login_attempts.lock().await, &ip);
        let token = state.create_token(AccessLevel::Admin, 7).await;
        let cookie = state.session_cookie(token, 7);
        return (
            jar.add(cookie),
            Json(LoginResponse {
                success: true,
                access_level: Some(AccessLevel::Admin),
                error: None,
            }),
        );
    }

    // Try read password
    if let Some(ref hash) = read_hash
        && verify_password(password, hash)
    {
        clear_attempts(&mut *state.login_attempts.lock().await, &ip);
        let token = state.create_token(AccessLevel::ReadOnly, 365).await;
        let cookie = state.session_cookie(token, 365);
        return (
            jar.add(cookie),
            Json(LoginResponse {
                success: true,
                access_level: Some(AccessLevel::ReadOnly),
                error: None,
            }),
        );
    }

    // Attempt was already recorded preemptively by check_and_record_attempt

    (
        jar,
        Json(LoginResponse {
            success: false,
            access_level: None,
            error: Some("Invalid password".into()),
        }),
    )
}

async fn api_logout(jar: CookieJar) -> CookieJar {
    jar.remove(Cookie::build(SESSION_COOKIE).path("/").build())
}

async fn api_setup(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(req): Json<SetupRequest>,
) -> (CookieJar, Json<SetupResponse>) {
    // Only allow setup if no passwords are configured
    if state.passwords_configured().await {
        return (
            jar,
            Json(SetupResponse {
                success: false,
                error: Some(
                    "Passwords are already configured. Use the admin panel to change them.".into(),
                ),
            }),
        );
    }

    let admin_pw = req.admin_password.trim();
    let read_pw = req
        .read_password
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(admin_pw);

    if admin_pw.is_empty() {
        return (
            jar,
            Json(SetupResponse {
                success: false,
                error: Some("Password is required".into()),
            }),
        );
    }

    if admin_pw.len() < 4 {
        return (
            jar,
            Json(SetupResponse {
                success: false,
                error: Some("Password must be at least 4 characters".into()),
            }),
        );
    }

    if read_pw.len() < 4 {
        return (
            jar,
            Json(SetupResponse {
                success: false,
                error: Some("Read-only password must be at least 4 characters".into()),
            }),
        );
    }

    // Store hashed passwords (same hash for both if single password mode)
    if let Err(e) = state
        .db
        .set_config(CONFIG_ADMIN_PASSWORD_HASH, &hash_password(admin_pw))
        .await
    {
        return (
            jar,
            Json(SetupResponse {
                success: false,
                error: Some(e.to_string()),
            }),
        );
    }
    if let Err(e) = state
        .db
        .set_config(CONFIG_READ_PASSWORD_HASH, &hash_password(read_pw))
        .await
    {
        return (
            jar,
            Json(SetupResponse {
                success: false,
                error: Some(e.to_string()),
            }),
        );
    }

    // Auto-login as admin after setup
    let token = state.create_token(AccessLevel::Admin, 7).await;
    let cookie = state.session_cookie(token, 7);

    (
        jar.add(cookie),
        Json(SetupResponse {
            success: true,
            error: None,
        }),
    )
}

async fn api_wishlist(State(state): State<AppState>, jar: CookieJar) -> Response {
    if state.get_session(&jar).await.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Read latest data from DB snapshots (populated by background polling)
    let snapshots = state.db.get_latest_snapshots().await.unwrap_or_default();
    let app_info = state.db.get_all_app_info().await.unwrap_or_default();
    let totals = state.db.get_all_game_totals().await.unwrap_or_default();

    let games = snapshots
        .into_iter()
        .map(|report| {
            let (name, image_url) = match app_info.get(&report.app_id) {
                Some((n, img)) => (n.clone(), img.clone()),
                None => (format!("App {}", report.app_id), String::new()),
            };
            let date = normalize_date_to_iso(&report.date);
            let game_totals = totals.get(&report.app_id);
            GameReport {
                app_id: report.app_id,
                image_url,
                name,
                date,
                adds: report.adds,
                deletes: report.deletes,
                purchases: report.purchases,
                gifts: report.gifts,
                adds_windows: report.adds_windows,
                adds_mac: report.adds_mac,
                adds_linux: report.adds_linux,
                countries: report.countries,
                changed_at: report.fetched_at,
                total_adds: game_totals.map_or(0, |t| t.adds),
                total_deletes: game_totals.map_or(0, |t| t.deletes),
                total_purchases: game_totals.map_or(0, |t| t.purchases),
                total_gifts: game_totals.map_or(0, |t| t.gifts),
            }
        })
        .collect();

    Json(ApiResponse { games }).into_response()
}

// ── Game detail endpoints (split for scalability) ───────────────────

/// GET /api/wishlist/{app_id}/detail — metadata + latest snapshot only
async fn api_game_detail(
    State(state): State<AppState>,
    jar: CookieJar,
    axum::extract::Path(app_id): axum::extract::Path<u32>,
) -> Response {
    if state.get_session(&jar).await.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !state.db.is_tracked(app_id).await.unwrap_or(false) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let app_info = state.db.get_all_app_info().await.unwrap_or_default();
    let (name, image_url) = match app_info.get(&app_id) {
        Some((n, img)) => (n.clone(), img.clone()),
        None => (format!("App {}", app_id), String::new()),
    };

    let game_totals = state.db.get_game_totals(app_id).await.unwrap_or(None);

    let latest = state
        .db
        .get_latest_snapshot(app_id)
        .await
        .ok()
        .flatten()
        .map(|report| {
            let date = normalize_date_to_iso(&report.date);
            GameReport {
                app_id: report.app_id,
                name: name.clone(),
                image_url: image_url.clone(),
                date,
                adds: report.adds,
                deletes: report.deletes,
                purchases: report.purchases,
                gifts: report.gifts,
                adds_windows: report.adds_windows,
                adds_mac: report.adds_mac,
                adds_linux: report.adds_linux,
                countries: report.countries,
                changed_at: report.fetched_at,
                total_adds: game_totals.as_ref().map_or(0, |t| t.adds),
                total_deletes: game_totals.as_ref().map_or(0, |t| t.deletes),
                total_purchases: game_totals.as_ref().map_or(0, |t| t.purchases),
                total_gifts: game_totals.as_ref().map_or(0, |t| t.gifts),
            }
        });

    // Get total count efficiently
    let total_snapshots = state.db.get_snapshot_count(app_id).await.unwrap_or(0);

    Json(GameDetailResponse {
        app_id,
        name,
        image_url,
        latest,
        total_snapshots,
    })
    .into_response()
}

#[derive(Deserialize)]
struct ChartQuery {
    range: Option<String>,
}

/// GET /api/wishlist/{app_id}/chart?range=7d|1m|3m|1y|5y
async fn api_game_chart(
    State(state): State<AppState>,
    jar: CookieJar,
    axum::extract::Path(app_id): axum::extract::Path<u32>,
    axum::extract::Query(query): axum::extract::Query<ChartQuery>,
) -> Response {
    if state.get_session(&jar).await.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !state.db.is_tracked(app_id).await.unwrap_or(false) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let range = query.range.as_deref().unwrap_or("7d");

    // Calculate the "since" timestamp and pick resolution
    let now = chrono::Utc::now();
    let (since, resolution) = match range {
        "1d" => {
            let since = now - chrono::TimeDelta::days(1);
            (since, "raw")
        }
        "2d" => {
            let since = now - chrono::TimeDelta::days(2);
            (since, "raw")
        }
        "3d" => {
            let since = now - chrono::TimeDelta::days(3);
            (since, "raw")
        }
        "7d" => {
            let since = now - chrono::TimeDelta::days(7);
            (since, "daily")
        }
        "1m" => {
            let since = now - chrono::TimeDelta::days(30);
            (since, "daily")
        }
        "3m" => {
            let since = now - chrono::TimeDelta::days(90);
            (since, "daily")
        }
        "1y" => {
            let since = now - chrono::TimeDelta::days(365);
            (since, "weekly")
        }
        "5y" => {
            let since = now - chrono::TimeDelta::days(5 * 365);
            (since, "monthly")
        }
        "all" => {
            // Far enough back to cover any realistic dataset
            let since = now - chrono::TimeDelta::days(20 * 365);
            (since, "monthly")
        }
        _ => {
            let since = now - chrono::TimeDelta::days(7);
            (since, "daily")
        }
    };

    let since_str = since.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let now_str = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let chart_points = state
        .db
        .get_chart_data(app_id, &since_str, resolution)
        .await
        .unwrap_or_default();

    let anomaly_config = state.get_anomaly_config().await;

    let anomaly_by_bucket: std::collections::HashMap<String, AnomalyMetrics> =
        if resolution == "raw" {
            // For raw resolution, compute anomalies on individual raw snapshots (original behavior)
            let lookback_secs = anomaly_config.lookback_days as i64 * 86400;
            let lookback_since = (since - chrono::TimeDelta::seconds(lookback_secs))
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string();

            let raw_snapshots = state
                .db
                .get_raw_snapshots_between(app_id, &lookback_since, &now_str)
                .await
                .unwrap_or_default();

            let context_with_secs: Vec<(f64, &crate::db::ChartPoint)> = raw_snapshots
                .iter()
                .map(|p| (crate::db::label_to_epoch_secs(&p.label), p))
                .collect();

            let raw_anomalies: Vec<(&crate::db::ChartPoint, AnomalyMetrics)> = raw_snapshots
                .iter()
                .map(|p| {
                    let metrics = compute_anomaly_for_chart_point(
                        p,
                        &context_with_secs,
                        &anomaly_config,
                        lookback_secs as f64,
                    );
                    (p, metrics)
                })
                .collect();

            let mut map: std::collections::HashMap<String, AnomalyMetrics> =
                std::collections::HashMap::new();
            for (p, metrics) in &raw_anomalies {
                let entry = map.entry(p.label.clone()).or_default();
                entry.adds = entry.adds || metrics.adds;
                entry.deletes = entry.deletes || metrics.deletes;
                entry.purchases = entry.purchases || metrics.purchases;
                entry.gifts = entry.gifts || metrics.gifts;
                for desc in &metrics.descriptions {
                    if !entry.descriptions.contains(desc) {
                        entry.descriptions.push(desc.clone());
                    }
                }
            }
            map
        } else {
            // For aggregated resolutions, compute anomalies directly on aggregated chart points.
            // Scale lookback so lookback_days means "number of context data points" at this resolution.
            let secs_per_period: i64 = match resolution {
                "weekly" => 7 * 86400,
                "monthly" => 30 * 86400,
                _ => 86400, // daily
            };
            let lookback_secs = anomaly_config.lookback_days as i64 * secs_per_period;
            let lookback_since = (since - chrono::TimeDelta::seconds(lookback_secs))
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string();

            // Fetch aggregated points with extended lookback for context
            let context_points = state
                .db
                .get_chart_data(app_id, &lookback_since, resolution)
                .await
                .unwrap_or_default();

            let context_with_secs: Vec<(f64, &crate::db::ChartPoint)> = context_points
                .iter()
                .map(|p| (crate::db::label_to_epoch_secs(&p.label), p))
                .collect();

            let mut map: std::collections::HashMap<String, AnomalyMetrics> =
                std::collections::HashMap::new();
            for p in &chart_points {
                let metrics = compute_anomaly_for_chart_point(
                    p,
                    &context_with_secs,
                    &anomaly_config,
                    lookback_secs as f64,
                );
                map.insert(p.label.clone(), metrics);
            }
            map
        };

    let points = chart_points
        .into_iter()
        .map(|p| {
            let anomaly = anomaly_by_bucket.get(&p.label).cloned().unwrap_or_default();
            let is_anomaly = anomaly.adds || anomaly.deletes || anomaly.purchases || anomaly.gifts;
            ChartPointResponse {
                label: p.label,
                adds: p.adds,
                deletes: p.deletes,
                purchases: p.purchases,
                gifts: p.gifts,
                adds_windows: p.adds_windows,
                adds_mac: p.adds_mac,
                adds_linux: p.adds_linux,
                is_anomaly,
                anomaly_metrics: anomaly,
            }
        })
        .collect();

    Json(ChartResponse {
        resolution: resolution.to_string(),
        points,
    })
    .into_response()
}

#[derive(Deserialize)]
struct HistoryQuery {
    page: Option<usize>,
    per_page: Option<usize>,
}

/// GET /api/wishlist/{app_id}/history?page=1&per_page=24
async fn api_game_history(
    State(state): State<AppState>,
    jar: CookieJar,
    axum::extract::Path(app_id): axum::extract::Path<u32>,
    axum::extract::Query(query): axum::extract::Query<HistoryQuery>,
) -> Response {
    if state.get_session(&jar).await.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !state.db.is_tracked(app_id).await.unwrap_or(false) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(24).clamp(1, 100);

    let paginated = state
        .db
        .get_snapshots_paginated(app_id, page, per_page)
        .await
        .unwrap_or(crate::db::PaginatedSnapshots {
            snapshots: Vec::new(),
            total: 0,
        });

    // Compute anomaly flags for each snapshot in this page.
    // We fetch a bounded lookback window of raw data around this page only.
    let anomaly_config = state.get_anomaly_config().await;
    let lookback_secs = anomaly_config.lookback_days as i64 * 86400;

    // Page is ordered newest-first: first() = newest, last() = oldest
    let newest_in_page = paginated
        .snapshots
        .first()
        .and_then(|(_, r)| r.fetched_at.clone());
    let oldest_in_page = paginated
        .snapshots
        .last()
        .and_then(|(_, r)| r.fetched_at.clone());

    let parse_ts = |s: &str| {
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ")
            .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S"))
            .ok()
    };

    // Fetch bounded context: from (oldest - lookback) to newest
    let context_snapshots = match (&oldest_in_page, &newest_in_page) {
        (Some(oldest_ts), Some(newest_ts)) => {
            if let Some(oldest_dt) = parse_ts(oldest_ts) {
                let lookback_start = oldest_dt - chrono::TimeDelta::seconds(lookback_secs);
                let since_str = lookback_start.format("%Y-%m-%dT%H:%M:%SZ").to_string();
                state
                    .db
                    .get_raw_snapshots_between(app_id, &since_str, newest_ts)
                    .await
                    .unwrap_or_default()
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    };

    // Pre-parse all context timestamps once to avoid repeated chrono parsing
    let context_with_secs: Vec<(f64, &crate::db::ChartPoint)> = context_snapshots
        .iter()
        .map(|p| (crate::db::label_to_epoch_secs(&p.label), p))
        .collect();

    let entries: Vec<HistoryEntry> = paginated
        .snapshots
        .iter()
        .map(|(snapshot_id, s)| {
            let anomaly_metrics = compute_anomaly_for_snapshot(
                s,
                &context_with_secs,
                &anomaly_config,
                lookback_secs as f64,
            );
            let is_anomaly = anomaly_metrics.adds
                || anomaly_metrics.deletes
                || anomaly_metrics.purchases
                || anomaly_metrics.gifts;

            HistoryEntry {
                snapshot_id: *snapshot_id,
                date: if s.date.contains('T') {
                    s.date.clone()
                } else {
                    format!("{}T00:00:00Z", s.date)
                },
                adds: s.adds,
                deletes: s.deletes,
                purchases: s.purchases,
                gifts: s.gifts,
                adds_windows: s.adds_windows,
                adds_mac: s.adds_mac,
                adds_linux: s.adds_linux,
                fetched_at: s.fetched_at.clone().unwrap_or_default(),
                is_anomaly,
                anomaly_metrics,
            }
        })
        .collect();

    Json(PaginatedHistoryResponse {
        entries,
        total: paginated.total,
        page,
        per_page,
    })
    .into_response()
}

/// Compute anomaly flags for a single snapshot given pre-parsed context data.
/// `context` is a slice of (epoch_secs, ChartPoint) pairs, sorted by time ascending.
fn compute_anomaly_for_snapshot(
    snapshot: &crate::steam::WishlistReport,
    context: &[(f64, &crate::db::ChartPoint)],
    config: &crate::anomaly::AnomalyConfig,
    lookback_secs: f64,
) -> AnomalyMetrics {
    let curr_ts = snapshot.fetched_at.as_deref().unwrap_or("");
    if curr_ts.is_empty() {
        return AnomalyMetrics::default();
    }
    let curr_secs = crate::db::label_to_epoch_secs(curr_ts);
    compute_anomaly_inner(
        curr_secs,
        snapshot.adds,
        snapshot.deletes,
        snapshot.purchases,
        snapshot.gifts,
        context,
        config,
        lookback_secs,
    )
}

/// Compute anomaly flags for a raw ChartPoint (used by the chart endpoint).
fn compute_anomaly_for_chart_point(
    point: &crate::db::ChartPoint,
    context: &[(f64, &crate::db::ChartPoint)],
    config: &crate::anomaly::AnomalyConfig,
    lookback_secs: f64,
) -> AnomalyMetrics {
    let curr_secs = crate::db::label_to_epoch_secs(&point.label);
    compute_anomaly_inner(
        curr_secs,
        point.adds,
        point.deletes,
        point.purchases,
        point.gifts,
        context,
        config,
        lookback_secs,
    )
}

/// Shared anomaly detection logic.
#[allow(clippy::too_many_arguments)]
fn compute_anomaly_inner(
    curr_secs: f64,
    adds: i64,
    deletes: i64,
    purchases: i64,
    gifts: i64,
    context: &[(f64, &crate::db::ChartPoint)],
    config: &crate::anomaly::AnomalyConfig,
    lookback_secs: f64,
) -> AnomalyMetrics {
    if context.len() < 4 {
        return AnomalyMetrics::default();
    }

    // Find the previous snapshot (the one just before this one in time)
    let prev_idx = context.iter().rposition(|(s, _)| *s < curr_secs);

    let prev_idx = match prev_idx {
        Some(i) => i,
        None => return AnomalyMetrics::default(),
    };
    let (prev_secs, prev) = context[prev_idx];

    // Find lookback window: points from (curr - lookback) up to (but not including) curr
    let window_start_secs = curr_secs - lookback_secs;
    let window: Vec<(f64, &crate::db::ChartPoint)> = context
        .iter()
        .filter(|(s, _)| *s >= window_start_secs && *s < curr_secs)
        .copied()
        .collect();

    if window.len() < 4 {
        return AnomalyMetrics::default();
    }

    let days_elapsed = (curr_secs - prev_secs) / 86400.0;
    let days_elapsed = if days_elapsed <= 0.0 {
        1.0
    } else {
        days_elapsed
    };

    let check_metric = |name: &str,
                        curr_val: i64,
                        prev_val: i64,
                        get_val: &dyn Fn(&crate::db::ChartPoint) -> i64|
     -> (bool, Option<String>) {
        let raw_delta = curr_val - prev_val;
        if raw_delta == 0 {
            return (false, None);
        }
        if raw_delta.abs() < config.min_absolute {
            return (false, None);
        }
        let current_rate = raw_delta as f64 / days_elapsed;

        let mut rates: Vec<f64> = window
            .windows(2)
            .filter_map(|w| {
                let d = (w[1].0 - w[0].0) / 86400.0;
                if d <= 0.0 {
                    return None;
                }
                let raw = get_val(w[1].1) - get_val(w[0].1);
                Some(raw as f64 / d)
            })
            .collect();
        if rates.len() < 3 {
            return (false, None);
        }
        let median = crate::anomaly::f64_median_pub(&mut rates);
        let mad = crate::anomaly::f64_mad_pub(&mut rates, median);
        let effective_mad = crate::anomaly::apply_mad_floor_pub(mad, median, config.mad_floor_pct);
        let is_anomalous = if effective_mad == 0.0 {
            (current_rate - median).abs() > f64::EPSILON
        } else {
            let deviation = current_rate - median;
            let z = deviation.abs() / effective_mad;
            if deviation >= 0.0 {
                z > config.sensitivity_up
            } else {
                z > config.sensitivity_down
            }
        };
        if is_anomalous {
            let desc = format_anomaly_description(name, current_rate, median);
            (true, Some(desc))
        } else {
            (false, None)
        }
    };

    let (adds_flag, adds_desc) = check_metric("Adds", adds, prev.adds, &|p| p.adds);
    let (deletes_flag, deletes_desc) =
        check_metric("Deletes", deletes, prev.deletes, &|p| p.deletes);
    let (purchases_flag, purchases_desc) =
        check_metric("Purchases", purchases, prev.purchases, &|p| p.purchases);
    let (gifts_flag, gifts_desc) = check_metric("Gifts", gifts, prev.gifts, &|p| p.gifts);

    let descriptions: Vec<String> = [adds_desc, deletes_desc, purchases_desc, gifts_desc]
        .into_iter()
        .flatten()
        .collect();

    AnomalyMetrics {
        adds: adds_flag,
        deletes: deletes_flag,
        purchases: purchases_flag,
        gifts: gifts_flag,
        descriptions,
    }
}

/// Generate a human-readable anomaly description.
fn format_anomaly_description(metric_name: &str, current_rate: f64, median: f64) -> String {
    let abs_rate = current_rate.abs();
    let abs_median = median.abs();

    if abs_median < 0.01 {
        // Baseline was essentially zero
        if current_rate > 0.0 {
            format!(
                "{metric_name} surged to {:.0}/day from near-zero baseline",
                abs_rate
            )
        } else {
            format!(
                "{metric_name} dropped to {:.0}/day from near-zero baseline",
                abs_rate
            )
        }
    } else {
        let ratio = abs_rate / abs_median;
        let direction = if current_rate > median {
            "above"
        } else {
            "below"
        };
        if ratio >= 2.0 {
            format!(
                "{metric_name} {:.0}× {direction} normal ({:.0}/day vs ~{:.0}/day)",
                ratio, abs_rate, abs_median
            )
        } else {
            format!(
                "{metric_name} unusual at {:.0}/day ({direction} ~{:.0}/day typical)",
                abs_rate, abs_median
            )
        }
    }
}

/// GET /api/wishlist/{app_id}/countries/{snapshot_id}
async fn api_snapshot_countries(
    State(state): State<AppState>,
    jar: CookieJar,
    axum::extract::Path((app_id, snapshot_id)): axum::extract::Path<(u32, i64)>,
) -> Response {
    if state.get_session(&jar).await.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !state.db.is_tracked(app_id).await.unwrap_or(false) {
        return StatusCode::NOT_FOUND.into_response();
    }

    let countries = match state.db.get_snapshot_countries(app_id, snapshot_id).await {
        Ok(Some(c)) => c,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    Json(SnapshotCountriesResponse {
        snapshot_id,
        countries,
    })
    .into_response()
}

async fn api_admin_config_get(State(state): State<AppState>, jar: CookieJar) -> Response {
    match state.get_session(&jar).await {
        Some(s) if s.access_level == AccessLevel::Admin => {}
        _ => return StatusCode::UNAUTHORIZED.into_response(),
    }

    let config = state.db.get_all_config().await.unwrap_or_default();

    let has_steam_key = config
        .get(CONFIG_STEAM_API_KEY)
        .map(|v| !v.is_empty() && state.decrypt_value(v).is_ok())
        .unwrap_or(false);
    let has_tg_token = config
        .get(CONFIG_TELEGRAM_BOT_TOKEN)
        .map(|v| !v.is_empty() && state.decrypt_value(v).is_ok())
        .unwrap_or(false);
    let tg_ids = config.get(CONFIG_TELEGRAM_ADMIN_IDS).cloned();
    let tg_enabled = config
        .get(CONFIG_TELEGRAM_ENABLED)
        .map(|v| v == "true")
        .unwrap_or(false);
    let has_dc_token = config
        .get(CONFIG_DISCORD_BOT_TOKEN)
        .map(|v| !v.is_empty() && state.decrypt_value(v).is_ok())
        .unwrap_or(false);
    let dc_ids = config.get(CONFIG_DISCORD_ADMIN_IDS).cloned();
    let dc_enabled = config
        .get(CONFIG_DISCORD_ENABLED)
        .map(|v| v == "true")
        .unwrap_or(false);
    let notification_mode = config
        .get(CONFIG_NOTIFICATION_MODE)
        .cloned()
        .unwrap_or_else(|| "every_update".to_string());
    let anomaly_lookback_days = config
        .get(CONFIG_ANOMALY_LOOKBACK_DAYS)
        .and_then(|v| v.parse().ok())
        .unwrap_or(14);
    let legacy_sensitivity: Option<f64> = config
        .get("anomaly_sensitivity")
        .and_then(|v| v.parse().ok());
    let default_sens = legacy_sensitivity.unwrap_or(2.0);
    let anomaly_sensitivity_up = config
        .get(CONFIG_ANOMALY_SENSITIVITY_UP)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default_sens);
    let anomaly_sensitivity_down = config
        .get(CONFIG_ANOMALY_SENSITIVITY_DOWN)
        .and_then(|v| v.parse().ok())
        .unwrap_or(default_sens);
    let anomaly_min_absolute = config
        .get(CONFIG_ANOMALY_MIN_ABSOLUTE)
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let anomaly_mad_floor_pct = config
        .get(CONFIG_ANOMALY_MAD_FLOOR_PCT)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.05);

    Json(AdminConfigResponse {
        has_steam_api_key: has_steam_key,
        encryption_enabled: state.encryption_enabled(),
        has_telegram_bot_token: has_tg_token,
        has_discord_bot_token: has_dc_token,
        telegram_admin_ids: tg_ids,
        telegram_enabled: tg_enabled,
        discord_admin_ids: dc_ids,
        discord_enabled: dc_enabled,
        notification_mode,
        anomaly_lookback_days,
        anomaly_sensitivity_up,
        anomaly_sensitivity_down,
        anomaly_min_absolute,
        anomaly_mad_floor_pct,
    })
    .into_response()
}

async fn api_admin_health(State(state): State<AppState>, jar: CookieJar) -> Response {
    match state.get_session(&jar).await {
        Some(s) if s.access_level == AccessLevel::Admin => {}
        _ => return StatusCode::UNAUTHORIZED.into_response(),
    }

    let config = state.db.get_all_config().await.unwrap_or_default();

    // ── Steam health ──
    let has_steam_key = config
        .get(CONFIG_STEAM_API_KEY)
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let steam = if has_steam_key {
        ServiceHealth {
            status: ServiceStatus::Ok,
            message: None,
        }
    } else {
        ServiceHealth {
            status: ServiceStatus::NotConfigured,
            message: Some("Steam API key is not configured".into()),
        }
    };

    // ── Telegram health ──
    let tg_enabled = config
        .get(CONFIG_TELEGRAM_ENABLED)
        .map(|v| v == "true")
        .unwrap_or(false);
    let telegram = if !tg_enabled {
        ServiceHealth {
            status: ServiceStatus::Disabled,
            message: None,
        }
    } else {
        let has_token = config
            .get(CONFIG_TELEGRAM_BOT_TOKEN)
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        if !has_token {
            ServiceHealth {
                status: ServiceStatus::Error,
                message: Some("Telegram is enabled but bot token is not configured".into()),
            }
        } else {
            // Check if the bot task is still running
            let handle = state.telegram_handle.lock().await;
            match &*handle {
                Some(h) if !h.is_finished() => ServiceHealth {
                    status: ServiceStatus::Ok,
                    message: None,
                },
                Some(_) => ServiceHealth {
                    status: ServiceStatus::Error,
                    message: Some("Telegram bot has stopped unexpectedly".into()),
                },
                None => ServiceHealth {
                    status: ServiceStatus::Error,
                    message: Some("Telegram bot is not running".into()),
                },
            }
        }
    };

    // ── Discord health ──
    let dc_enabled = config
        .get(CONFIG_DISCORD_ENABLED)
        .map(|v| v == "true")
        .unwrap_or(false);
    let discord = if !dc_enabled {
        ServiceHealth {
            status: ServiceStatus::Disabled,
            message: None,
        }
    } else {
        let has_token = config
            .get(CONFIG_DISCORD_BOT_TOKEN)
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        if !has_token {
            ServiceHealth {
                status: ServiceStatus::Error,
                message: Some("Discord is enabled but bot token is not configured".into()),
            }
        } else {
            let handle = state.discord_handle.lock().await;
            match &*handle {
                Some(h) if !h.is_finished() => ServiceHealth {
                    status: ServiceStatus::Ok,
                    message: None,
                },
                Some(_) => ServiceHealth {
                    status: ServiceStatus::Error,
                    message: Some("Discord bot has stopped unexpectedly".into()),
                },
                None => ServiceHealth {
                    status: ServiceStatus::Error,
                    message: Some("Discord bot is not running".into()),
                },
            }
        }
    };

    Json(HealthResponse {
        steam,
        telegram,
        discord,
    })
    .into_response()
}

async fn api_admin_config_update(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(req): Json<AdminConfigUpdate>,
) -> Response {
    match state.get_session(&jar).await {
        Some(s) if s.access_level == AccessLevel::Admin => {}
        _ => return StatusCode::UNAUTHORIZED.into_response(),
    }

    // Track what changed so we know what to reload
    let mut steam_changed = false;
    let mut telegram_changed = false;
    let mut discord_changed = false;

    if let Some(key) = req.steam_api_key {
        let key = key.trim().to_string();
        if key.is_empty() {
            let _ = state.db.delete_config(CONFIG_STEAM_API_KEY).await;
            *state.steam.write().await = None;
            steam_changed = true;
        } else {
            // Validate key with Steam API before saving
            if let Err(e) = crate::steam::validate_api_key(&key).await {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": e.to_string() })),
                )
                    .into_response();
            }
            let store_value = match state.encrypt_value(&key) {
                Ok(v) => v,
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({ "error": format!("Encryption failed: {e}") })),
                    )
                        .into_response();
                }
            };
            if let Err(e) = state
                .db
                .set_config(CONFIG_STEAM_API_KEY, &store_value)
                .await
            {
                return e.into_response();
            }
            state.ensure_steam(&key).await;
            steam_changed = true;
        }
    }

    if let Some(token) = req.telegram_bot_token {
        let token = token.trim().to_string();
        if token.is_empty() {
            let _ = state.db.delete_config(CONFIG_TELEGRAM_BOT_TOKEN).await;
        } else {
            // Validate token with Telegram API before saving
            if let Err(e) = crate::telegram::validate_token(&token).await {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": e.to_string() })),
                )
                    .into_response();
            }
            let store_value = match state.encrypt_value(&token) {
                Ok(v) => v,
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({ "error": format!("Encryption failed: {e}") })),
                    )
                        .into_response();
                }
            };
            if let Err(e) = state
                .db
                .set_config(CONFIG_TELEGRAM_BOT_TOKEN, &store_value)
                .await
            {
                return e.into_response();
            }
        }
        telegram_changed = true;
    }

    if let Some(ids) = req.telegram_admin_ids {
        let ids: String = ids
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .filter(|s| s.parse::<u64>().is_ok())
            .collect::<Vec<_>>()
            .join(",");
        if ids.is_empty() {
            let _ = state.db.delete_config(CONFIG_TELEGRAM_ADMIN_IDS).await;
        } else if let Err(e) = state.db.set_config(CONFIG_TELEGRAM_ADMIN_IDS, &ids).await {
            return e.into_response();
        }
        telegram_changed = true;
    }

    if let Some(enabled) = req.telegram_enabled {
        if let Err(e) = state
            .db
            .set_config(
                CONFIG_TELEGRAM_ENABLED,
                if enabled { "true" } else { "false" },
            )
            .await
        {
            return e.into_response();
        }
        telegram_changed = true;
    }

    if let Some(token) = req.discord_bot_token {
        let token = token.trim().to_string();
        if token.is_empty() {
            let _ = state.db.delete_config(CONFIG_DISCORD_BOT_TOKEN).await;
        } else {
            // Validate token with Discord API before saving
            if let Err(e) = crate::discord::validate_token(&token).await {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": e.to_string() })),
                )
                    .into_response();
            }
            let store_value = match state.encrypt_value(&token) {
                Ok(v) => v,
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({ "error": format!("Encryption failed: {e}") })),
                    )
                        .into_response();
                }
            };
            if let Err(e) = state
                .db
                .set_config(CONFIG_DISCORD_BOT_TOKEN, &store_value)
                .await
            {
                return e.into_response();
            }
        }
        discord_changed = true;
    }

    if let Some(ids) = req.discord_admin_ids {
        let ids: String = ids
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .filter(|s| s.parse::<u64>().is_ok())
            .collect::<Vec<_>>()
            .join(",");
        if ids.is_empty() {
            let _ = state.db.delete_config(CONFIG_DISCORD_ADMIN_IDS).await;
        } else if let Err(e) = state.db.set_config(CONFIG_DISCORD_ADMIN_IDS, &ids).await {
            return e.into_response();
        }
        discord_changed = true;
    }

    if let Some(enabled) = req.discord_enabled {
        if let Err(e) = state
            .db
            .set_config(
                CONFIG_DISCORD_ENABLED,
                if enabled { "true" } else { "false" },
            )
            .await
        {
            return e.into_response();
        }
        discord_changed = true;
    }

    if let Some(mode) = req.notification_mode {
        let mode = mode.trim().to_string();
        if mode != "every_update" && mode != "anomalies_only" {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Invalid notification mode. Must be 'every_update' or 'anomalies_only'." })),
            )
                .into_response();
        }
        if let Err(e) = state.db.set_config(CONFIG_NOTIFICATION_MODE, &mode).await {
            return e.into_response();
        }
    }

    if let Some(days) = req.anomaly_lookback_days {
        let days = days.max(1);
        if let Err(e) = state
            .db
            .set_config(CONFIG_ANOMALY_LOOKBACK_DAYS, &days.to_string())
            .await
        {
            return e.into_response();
        }
    }

    if let Some(sensitivity_up) = req.anomaly_sensitivity_up {
        if sensitivity_up <= 0.0 {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Anomaly sensitivity (up) must be greater than 0." })),
            )
                .into_response();
        }
        if let Err(e) = state
            .db
            .set_config(CONFIG_ANOMALY_SENSITIVITY_UP, &sensitivity_up.to_string())
            .await
        {
            return e.into_response();
        }
    }

    if let Some(sensitivity_down) = req.anomaly_sensitivity_down {
        if sensitivity_down <= 0.0 {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Anomaly sensitivity (down) must be greater than 0." })),
            )
                .into_response();
        }
        if let Err(e) = state
            .db
            .set_config(
                CONFIG_ANOMALY_SENSITIVITY_DOWN,
                &sensitivity_down.to_string(),
            )
            .await
        {
            return e.into_response();
        }
    }

    if let Some(mad_floor_pct) = req.anomaly_mad_floor_pct {
        if !(0.0..=1.0).contains(&mad_floor_pct) {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::json!({ "error": "MAD floor percentage must be between 0 and 1." }),
                ),
            )
                .into_response();
        }
        if let Err(e) = state
            .db
            .set_config(CONFIG_ANOMALY_MAD_FLOOR_PCT, &mad_floor_pct.to_string())
            .await
        {
            return e.into_response();
        }
    }

    if let Some(min_abs) = req.anomaly_min_absolute {
        if min_abs < 1 {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Minimum absolute change must be at least 1." })),
            )
                .into_response();
        }
        if let Err(e) = state
            .db
            .set_config(CONFIG_ANOMALY_MIN_ABSOLUTE, &min_abs.to_string())
            .await
        {
            return e.into_response();
        }
    }

    // Restart bots if related config changed (or steam key changed, since bots need it)
    if telegram_changed || steam_changed {
        state.restart_telegram().await;
    }
    if discord_changed || steam_changed {
        state.restart_discord().await;
    }

    Json(serde_json::json!({ "success": true })).into_response()
}

async fn api_admin_change_password(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(req): Json<ChangePasswordRequest>,
) -> Response {
    match state.get_session(&jar).await {
        Some(s) if s.access_level == AccessLevel::Admin => {}
        _ => return StatusCode::UNAUTHORIZED.into_response(),
    }

    // Verify current admin password
    let admin_hash = match state
        .db
        .get_config(CONFIG_ADMIN_PASSWORD_HASH)
        .await
        .ok()
        .flatten()
    {
        Some(h) => h,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "No admin password configured",
            )
                .into_response();
        }
    };

    if !verify_password(&req.current_password, &admin_hash) {
        return Json(serde_json::json!({
            "success": false,
            "error": "Current password is incorrect"
        }))
        .into_response();
    }

    if let Some(ref new_admin) = req.new_admin_password {
        let new_admin = new_admin.trim();
        if new_admin.len() < 4 {
            return Json(serde_json::json!({
                "success": false,
                "error": "Admin password must be at least 4 characters"
            }))
            .into_response();
        }
        if let Err(e) = state
            .db
            .set_config(CONFIG_ADMIN_PASSWORD_HASH, &hash_password(new_admin))
            .await
        {
            return e.into_response();
        }
    }

    if let Some(ref new_read) = req.new_read_password {
        let new_read = new_read.trim();
        if new_read.len() < 4 {
            return Json(serde_json::json!({
                "success": false,
                "error": "Read-only password must be at least 4 characters"
            }))
            .into_response();
        }
        if let Err(e) = state
            .db
            .set_config(CONFIG_READ_PASSWORD_HASH, &hash_password(new_read))
            .await
        {
            return e.into_response();
        }
    }

    // Rotate JWT secret to invalidate all existing sessions
    state.rotate_jwt_secret().await;

    Json(serde_json::json!({ "success": true })).into_response()
}

// ── Game tracking management ────────────────────────────────────────

async fn api_admin_tracked_games(State(state): State<AppState>, jar: CookieJar) -> Response {
    match state.get_session(&jar).await {
        Some(s) if s.access_level == AccessLevel::Admin => {}
        _ => return StatusCode::UNAUTHORIZED.into_response(),
    }

    let ids = state.db.get_tracked_game_ids().await.unwrap_or_default();

    // Get tracked_since timestamps
    let since_map = state
        .db
        .get_tracked_games_with_dates()
        .await
        .unwrap_or_default();

    // Get app info from DB (persisted by polling & track handler)
    let db_info = state.db.get_all_app_info().await.unwrap_or_default();

    // Also check in-memory cache as fallback for recently added games
    let mem_info = match state.get_steam().await {
        Some(s) => s.app_info().await,
        None => Default::default(),
    };

    // Load sync statuses for all games
    let sync_rows = state.db.get_all_sync_statuses().await.unwrap_or_default();

    let mut games: Vec<TrackedGameEntry> = Vec::with_capacity(ids.len());
    for &id in &ids {
        let (name, image_url) = if let Some((n, img)) = db_info.get(&id) {
            (n.clone(), img.clone())
        } else if let Some(app) = mem_info.get(&id) {
            (app.name.clone(), app.image_url.clone().unwrap_or_default())
        } else {
            (format!("App {id}"), String::new())
        };

        let (
            is_syncing,
            sync_type,
            sync_progress_crawled,
            sync_progress_total,
            last_sync_completed_at,
            cooldown_active,
        ) = if let Some(row) = sync_rows.iter().find(|r| r.app_id == id) {
            let status = state.build_sync_status(row).await;
            (
                status.is_syncing,
                status.sync_type,
                status.progress_crawled,
                status.progress_total,
                status.last_completed_at,
                status.cooldown_active,
            )
        } else {
            (false, None, 0, 0, None, false)
        };

        games.push(TrackedGameEntry {
            app_id: id,
            name,
            image_url,
            tracked_since: since_map.get(&id).cloned().unwrap_or_default(),
            is_syncing,
            sync_type,
            sync_progress_crawled,
            sync_progress_total,
            last_sync_completed_at,
            cooldown_active,
        });
    }

    Json(games).into_response()
}

async fn api_admin_track_game(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(req): Json<TrackGameRequest>,
) -> Response {
    match state.get_session(&jar).await {
        Some(s) if s.access_level == AccessLevel::Admin => {}
        _ => return StatusCode::UNAUTHORIZED.into_response(),
    }

    let app_id = match parse_app_id(&req.input) {
        Ok(id) => id,
        Err(e) => {
            return Json(serde_json::json!({ "success": false, "error": e.to_string() }))
                .into_response();
        }
    };

    // Try to resolve the name via Steam store API and persist to DB
    let name = if let Some(steam) = state.get_steam().await {
        match steam.fetch_app_name(app_id).await {
            Ok(n) => {
                let info = steam.app_info().await;
                let image_url = info
                    .get(&app_id)
                    .and_then(|a| a.image_url.as_deref())
                    .unwrap_or("");
                let _ = state.db.upsert_app_info(app_id, &n, image_url).await;
                Some(n)
            }
            Err(_) => None,
        }
    } else {
        None
    };

    match state.db.add_tracked_game(app_id).await {
        Ok(true) => {
            let display = name.unwrap_or_else(|| format!("App {app_id}"));

            // Immediately fetch wishlist data so the dashboard has something to show
            if let Some(steam) = state.get_steam().await {
                match steam.fetch_wishlist(app_id).await {
                    Ok(report) => {
                        // Store app_min_date from the first API response for backfill
                        if let Some(ref min_date) = report.app_min_date {
                            let _ = state.db.store_app_min_date(app_id, min_date).await;
                        }
                        let _ = state.db.insert_snapshot_if_changed(&report).await;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to fetch initial data for {app_id}: {e}");
                    }
                }

                // Backfill full history in background
                let bg_state = state.clone();
                let bg_steam = steam.clone();
                let token = state.start_backfill(app_id).await;
                tokio::spawn(async move {
                    crate::backfill_game_history(
                        &bg_state, &bg_steam, app_id, token, "initial", "system",
                    )
                    .await;
                });
            }

            Json(serde_json::json!({
                "success": true,
                "app_id": app_id,
                "name": display,
                "message": format!("Now tracking {display}")
            }))
            .into_response()
        }
        Ok(false) => {
            let display = name.unwrap_or_else(|| format!("App {app_id}"));
            Json(serde_json::json!({
                "success": true,
                "app_id": app_id,
                "name": display,
                "message": format!("{display} is already being tracked")
            }))
            .into_response()
        }
        Err(e) => {
            Json(serde_json::json!({ "success": false, "error": e.to_string() })).into_response()
        }
    }
}

async fn api_admin_untrack_game(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(req): Json<UntrackGameRequest>,
) -> Response {
    match state.get_session(&jar).await {
        Some(s) if s.access_level == AccessLevel::Admin => {}
        _ => return StatusCode::UNAUTHORIZED.into_response(),
    }

    // Cancel any in-progress backfill before removing the game
    state.cancel_backfill(req.app_id).await;

    match state.db.remove_tracked_game(req.app_id).await {
        Ok(true) => Json(serde_json::json!({
            "success": true,
            "message": format!("Stopped tracking app {}", req.app_id)
        }))
        .into_response(),
        Ok(false) => Json(serde_json::json!({
            "success": false,
            "error": format!("App {} was not being tracked", req.app_id)
        }))
        .into_response(),
        Err(e) => {
            Json(serde_json::json!({ "success": false, "error": e.to_string() })).into_response()
        }
    }
}

// ── Sync status endpoints ────────────────────────────────────────────

/// GET /api/sync/status — returns sync status for all tracked games (ReadOnly+).
async fn api_sync_status(State(state): State<AppState>, jar: CookieJar) -> Response {
    match state.get_session(&jar).await {
        Some(_) => {} // Any authenticated user can see sync status
        None => return StatusCode::UNAUTHORIZED.into_response(),
    }

    let sync_rows = state.db.get_all_sync_statuses().await.unwrap_or_default();
    let tracked_ids = state.db.get_tracked_game_ids().await.unwrap_or_default();

    let mut statuses: Vec<SyncStatusResponse> = Vec::new();
    for id in &tracked_ids {
        if let Some(row) = sync_rows.iter().find(|r| r.app_id == *id) {
            statuses.push(state.build_sync_status(row).await);
        } else {
            // No sync row — game has never been synced or was synced before this feature
            statuses.push(SyncStatusResponse {
                app_id: *id,
                is_syncing: false,
                sync_type: None,
                started_at: None,
                completed_at: None,
                progress_crawled: 0,
                progress_total: 0,
                last_completed_at: None,
                cooldown_active: false,
                requested_by: None,
            });
        }
    }

    Json(statuses).into_response()
}

/// POST /api/admin/sync — trigger a full re-sync for a game (Admin only).
async fn api_admin_sync(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(req): Json<SyncRequest>,
) -> Response {
    match state.get_session(&jar).await {
        Some(s) if s.access_level == AccessLevel::Admin => {}
        _ => return StatusCode::UNAUTHORIZED.into_response(),
    }

    let app_id = req.app_id;

    // 1. Verify game is tracked
    let tracked_ids = state.db.get_tracked_game_ids().await.unwrap_or_default();
    if !tracked_ids.contains(&app_id) {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "success": false, "error": "Game is not being tracked" })),
        )
            .into_response();
    }

    // 2. Check if already syncing and atomically acquire the backfill slot.
    //    Hold the lock across both the check and token insertion to prevent
    //    two concurrent requests from both passing the check.
    let token = {
        let mut tokens = state.backfill_tokens.lock().await;
        if tokens.contains_key(&app_id) {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "success": false,
                    "error": "A sync is already in progress for this game"
                })),
            )
                .into_response();
        }

        if let Ok(Some(row)) = state.db.get_sync_status(app_id).await {
            if row.status == "in_progress" {
                // Stale DB row from a crash — clean it up
                let _ = state.db.fail_sync(app_id).await;
            }

            // 3. Cooldown check — reject if a full sync completed < 30 min ago
            if row.status == "completed"
                && row.sync_type == "full"
                && is_within_cooldown(row.completed_at.as_deref())
            {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(serde_json::json!({
                        "success": false,
                        "error": "A sync was recently completed. Please wait before requesting another."
                    })),
                )
                    .into_response();
            }
        }

        // Reserve the slot while still holding the lock
        let token = tokio_util::sync::CancellationToken::new();
        tokens.insert(app_id, token.clone());
        token
    };

    // 4. Need Steam client
    let steam = match state.get_steam().await {
        Some(s) => s,
        None => {
            // Release the token we just acquired since we can't proceed
            state.backfill_tokens.lock().await.remove(&app_id);
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "success": false,
                    "error": "Steam API key not configured"
                })),
            )
                .into_response();
        }
    };

    // 5. Start backfill — clear_sync_progress is handled inside backfill_game_history
    //    for "full" syncs, so we don't risk data loss on early exit.
    let bg_state = state.clone();
    let bg_steam = steam.clone();
    tokio::spawn(async move {
        crate::backfill_game_history(&bg_state, &bg_steam, app_id, token, "full", "admin").await;
    });

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "success": true,
            "message": "Full sync started"
        })),
    )
        .into_response()
}

// ── Static file serving ─────────────────────────────────────────────

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Try the exact path first
    if let Some(file) = Assets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        let cache = cache_control_for(path);
        return (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, mime.as_ref()),
                (header::CACHE_CONTROL, cache),
            ],
            file.data,
        )
            .into_response();
    }

    // Fall back to index.html for SPA routing — always revalidate so new
    // deployments are picked up immediately (hashed asset URLs will change).
    match Assets::get("index.html") {
        Some(file) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "text/html"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            file.data,
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Html("<h1>Frontend not built</h1><p>Run <code>npm run build</code> in the <code>web/</code> directory.</p>".to_string()),
        )
            .into_response(),
    }
}

/// Return an appropriate Cache-Control value for the given asset path.
/// `index.html` and other HTML files are never cached (so new deploys take
/// effect immediately). Everything else (JS, CSS, images, fonts) carries a
/// content-hash in its filename from the bundler, so it can be cached
/// immutably for a long time.
fn cache_control_for(path: &str) -> &'static str {
    if path == "index.html" || path.ends_with(".html") {
        "no-cache"
    } else {
        "public, max-age=31536000, immutable"
    }
}

// ── Debug routes ────────────────────────────────────────────────────

#[allow(dead_code)]
async fn debug_test_change(
    State(state): State<AppState>,
    axum::extract::Path(app_id): axum::extract::Path<u32>,
) -> Response {
    use crate::db::SnapshotChange;
    use crate::steam::WishlistReport;
    use rand::Rng;

    // Generate random deltas upfront (ThreadRng is !Send, can't hold across .await)
    let (d_adds, d_deletes, d_purchases, d_gifts) = {
        let mut rng = rand::rng();
        (
            rng.random_range(1..=50i64),
            rng.random_range(0..=10i64),
            rng.random_range(0..=5i64),
            rng.random_range(0..=3i64),
        )
    };

    // Check that the app is tracked
    match state.db.is_tracked(app_id).await {
        Ok(false) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": format!("App {app_id} is not tracked") })),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
        Ok(true) => {}
    }

    // Get the latest snapshot or create a baseline
    let previous = state.db.get_latest_snapshot(app_id).await.ok().flatten();

    let base = previous.clone().unwrap_or(WishlistReport {
        app_id,
        date: "2025-01-01".to_string(),
        adds: 1000,
        deletes: 100,
        purchases: 50,
        gifts: 10,
        adds_windows: 0,
        adds_mac: 0,
        adds_linux: 0,
        countries: Vec::new(),
        fetched_at: None,
        app_min_date: None,
    });

    // Generate a fake report with small random deltas
    let fake_report = WishlistReport {
        app_id,
        date: base.date.clone(),
        adds: base.adds + d_adds,
        deletes: base.deletes + d_deletes,
        purchases: base.purchases + d_purchases,
        gifts: base.gifts + d_gifts,
        adds_windows: 0,
        adds_mac: 0,
        adds_linux: 0,
        countries: Vec::new(),
        fetched_at: None,
        app_min_date: None,
    };

    // Insert the fake snapshot
    match state.db.insert_snapshot_if_changed(&fake_report).await {
        Ok(SnapshotChange::Changed { previous }) => {
            crate::telegram::notify_change(&state.db, app_id, &fake_report, &previous, None).await;
            crate::discord::notify_change(&state.db, app_id, &fake_report, &previous, None).await;
            Json(serde_json::json!({
                "status": "changed",
                "app_id": app_id,
                "previous": {
                    "adds": previous.adds,
                    "deletes": previous.deletes,
                    "purchases": previous.purchases,
                    "gifts": previous.gifts,
                },
                "current": {
                    "adds": fake_report.adds,
                    "deletes": fake_report.deletes,
                    "purchases": fake_report.purchases,
                    "gifts": fake_report.gifts,
                },
            }))
            .into_response()
        }
        Ok(SnapshotChange::FirstSnapshot) => Json(serde_json::json!({
            "status": "first_snapshot",
            "app_id": app_id,
            "message": "First snapshot inserted, no notification sent",
        }))
        .into_response(),
        Ok(SnapshotChange::NoChange) => Json(serde_json::json!({
            "status": "no_change",
            "app_id": app_id,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

// ── CSRF middleware ──────────────────────────────────────────────────

/// Validates CSRF token on state-changing requests (POST/PUT/DELETE/PATCH).
///
/// Uses the double-submit cookie pattern: the `wpb_csrf` cookie value must
/// match the `X-CSRF-Token` request header. Both are opaque random tokens
/// set by the server — a cross-origin attacker cannot read the cookie to
/// forge the header.
async fn csrf_middleware(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let dominated = matches!(
        *req.method(),
        axum::http::Method::POST
            | axum::http::Method::PUT
            | axum::http::Method::DELETE
            | axum::http::Method::PATCH
    );

    // Auth endpoints are exempt: login requires a password (natural CSRF barrier),
    // logout is low-impact, and setup only works when unconfigured.
    let path = req.uri().path();
    let exempt = path.starts_with("/api/auth/") || path == "/api/setup";

    if dominated && !exempt {
        // Extract the CSRF cookie value
        let cookie_token = req
            .headers()
            .get(axum::http::header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(|cookies| {
                cookies.split(';').find_map(|c| {
                    let c = c.trim();
                    c.strip_prefix(&format!("{CSRF_COOKIE}="))
                })
            })
            .map(|s| s.to_string());

        let header_token = req
            .headers()
            .get(CSRF_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        match (cookie_token, header_token) {
            (Some(c), Some(h)) if !c.is_empty() && c == h => {
                // Valid CSRF token — proceed
            }
            _ => {
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({ "error": "CSRF token missing or invalid" })),
                )
                    .into_response();
            }
        }
    }

    next.run(req).await
}

// ── API response headers middleware ─────────────────────────────────

/// Adds security and caching headers to all `/api/` responses:
/// - `Cache-Control: no-store` — prevents browsers from caching API data
/// - `X-Content-Type-Options: nosniff` — prevents MIME-type sniffing
async fn api_headers_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let is_api = req.uri().path().starts_with("/api/");
    let mut resp = next.run(req).await;
    if is_api {
        let headers = resp.headers_mut();
        headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
        headers.insert(
            header::X_CONTENT_TYPE_OPTIONS,
            "nosniff".parse().unwrap(),
        );
    }
    resp
}

// ── Router and server ───────────────────────────────────────────────

pub async fn run_web(bind_addr: String, state: AppState) {
    let app = Router::new()
        // Auth routes (no auth required)
        .route("/api/auth/status", get(api_auth_status))
        .route("/api/auth/login", post(api_login))
        .route("/api/auth/logout", post(api_logout))
        .route("/api/setup", post(api_setup))
        // Authenticated routes
        .route("/api/wishlist", get(api_wishlist))
        // Game detail endpoints (split for scalability)
        .route("/api/wishlist/{app_id}/detail", get(api_game_detail))
        .route("/api/wishlist/{app_id}/chart", get(api_game_chart))
        .route("/api/wishlist/{app_id}/history", get(api_game_history))
        .route(
            "/api/wishlist/{app_id}/countries/{snapshot_id}",
            get(api_snapshot_countries),
        )
        // Admin routes
        .route("/api/admin/config", get(api_admin_config_get))
        .route("/api/admin/config", post(api_admin_config_update))
        .route("/api/admin/health", get(api_admin_health))
        .route(
            "/api/admin/change-password",
            post(api_admin_change_password),
        )
        // Game tracking management
        .route("/api/admin/games", get(api_admin_tracked_games))
        .route("/api/admin/track", post(api_admin_track_game))
        .route("/api/admin/untrack", post(api_admin_untrack_game))
        .route("/api/admin/sync", post(api_admin_sync))
        // Sync status (available to all authenticated users)
        .route("/api/sync/status", get(api_sync_status))
        // Debug routes — uncomment for local testing only
        // .route("/debug/test/{app_id}", get(debug_test_change))
        .layer(middleware::from_fn(csrf_middleware))
        .layer(middleware::from_fn(api_headers_middleware))
        .with_state(state)
        .fallback(static_handler);

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .unwrap_or_else(|e| panic!("Failed to bind web interface to {bind_addr}: {e}"));

    tracing::info!("Web interface listening on {bind_addr}");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .expect("Web server error");
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to listen for Ctrl+C");
    tracing::info!("Shutting down...");
}

/// Initialize passwords from CLI/env args into the database (if provided and not already set).
pub async fn init_passwords_from_config(
    db: &Database,
    admin_password: Option<&str>,
    read_password: Option<&str>,
) {
    if let Some(pw) = admin_password
        && !pw.trim().is_empty()
    {
        let existing = db
            .get_config(CONFIG_ADMIN_PASSWORD_HASH)
            .await
            .ok()
            .flatten();
        if existing.is_none() {
            if let Err(e) = db
                .set_config(CONFIG_ADMIN_PASSWORD_HASH, &hash_password(pw.trim()))
                .await
            {
                tracing::error!("Failed to set admin password: {e}");
            } else {
                tracing::info!("Admin password set from CLI/env");
            }
        }
    }

    if let Some(pw) = read_password
        && !pw.trim().is_empty()
    {
        let existing = db
            .get_config(CONFIG_READ_PASSWORD_HASH)
            .await
            .ok()
            .flatten();
        if existing.is_none() {
            if let Err(e) = db
                .set_config(CONFIG_READ_PASSWORD_HASH, &hash_password(pw.trim()))
                .await
            {
                tracing::error!("Failed to set read password: {e}");
            } else {
                tracing::info!("Read-only password set from CLI/env");
            }
        }
    }
}
