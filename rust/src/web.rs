use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use rand::Rng;
use rust_embed::Embed;
use serde::{Deserialize, Serialize};
use time::Duration;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;

use crate::db::Database;
use crate::steam::SteamClient;

const SESSION_COOKIE: &str = "wpb_session";
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
    let mut rng = rand::thread_rng();
    let bytes: [u8; 32] = rng.r#gen();
    hex::encode(bytes)
}

fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut rand::thread_rng());
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

fn check_rate_limit(attempts: &mut HashMap<String, Vec<Instant>>, key: &str) -> Result<(), u64> {
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
        let retry_after = window.as_secs() - now.duration_since(oldest).as_secs();
        return Err(retry_after);
    }
    Ok(())
}

fn record_failed_attempt(attempts: &mut HashMap<String, Vec<Instant>>, key: &str) {
    attempts
        .entry(key.to_string())
        .or_default()
        .push(Instant::now());
}

fn clear_attempts(attempts: &mut HashMap<String, Vec<Instant>>, key: &str) {
    attempts.remove(key);
}

// ── Shared application state ────────────────────────────────────────

const CONFIG_TRACKING_RETENTION_DAYS: &str = "tracking_retention_days";
const DEFAULT_RETENTION_DAYS: u32 = 90;

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub steam: Arc<RwLock<Option<SteamClient>>>,
    pub telegram_handle: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
    pub discord_handle: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
    login_attempts: RateLimiter,
    cached_jwt_secret: Arc<tokio::sync::Mutex<Option<String>>>,
    insecure: bool,
    pub auto_populate_days: u32,
}

impl AppState {
    pub fn new(db: Database, steam: Option<SteamClient>, insecure: bool, auto_populate_days: u32) -> Self {
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
            auto_populate_days,
        }
    }

    pub async fn get_retention_days(&self) -> u32 {
        self.db
            .get_config(CONFIG_TRACKING_RETENTION_DAYS)
            .await
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_RETENTION_DAYS)
    }

    /// Get or create the JWT signing secret (cached to avoid DB hits on every request).
    async fn jwt_secret(&self) -> String {
        let mut cached = self.cached_jwt_secret.lock().await;
        if let Some(ref secret) = *cached {
            return secret.clone();
        }
        if let Ok(Some(secret)) = self.db.get_config(CONFIG_JWT_SECRET).await {
            *cached = Some(secret.clone());
            return secret;
        }
        let secret = generate_jwt_secret();
        let _ = self.db.set_config(CONFIG_JWT_SECRET, &secret).await;
        *cached = Some(secret.clone());
        secret
    }

    /// Rotate the JWT secret, invalidating all existing tokens.
    async fn rotate_jwt_secret(&self) {
        let secret = generate_jwt_secret();
        let _ = self.db.set_config(CONFIG_JWT_SECRET, &secret).await;
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

    /// Ensure a SteamClient exists with the given key. Creates one or updates existing.
    async fn ensure_steam(&self, key: &str) {
        let mut guard = self.steam.write().await;
        match *guard {
            Some(ref client) => client.set_api_key(key.to_string()).await,
            None => *guard = Some(SteamClient::new(key.to_string())),
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

        let token = self
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

        let token = match token {
            Some(t) if !t.is_empty() => t,
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

        let token = self
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

        let token = match token {
            Some(t) if !t.is_empty() => t,
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

// ── API types ───────────────────────────────────────────────────────

#[derive(Serialize)]
struct AuthStatus {
    authenticated: bool,
    access_level: Option<AccessLevel>,
    setup_required: bool,
    version: &'static str,
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
    adds: u64,
    deletes: u64,
    purchases: u64,
    gifts: u64,
    adds_windows: u64,
    adds_mac: u64,
    adds_linux: u64,
    countries: Vec<crate::steam::CountryReport>,
    changed_at: Option<String>,
}

#[derive(Serialize)]
struct ApiResponse {
    games: Vec<GameReport>,
}

#[derive(Serialize)]
struct SnapshotEntry {
    date: String,
    adds: u64,
    deletes: u64,
    purchases: u64,
    gifts: u64,
    adds_windows: u64,
    adds_mac: u64,
    adds_linux: u64,
    countries: Vec<crate::steam::CountryReport>,
    fetched_at: String,
}

#[derive(Serialize)]
struct GameDetailResponse {
    app_id: u32,
    name: String,
    image_url: String,
    latest: Option<GameReport>,
    history: Vec<SnapshotEntry>,
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
    tracking_retention_days: Option<u32>,
}

#[derive(Serialize)]
struct AdminConfigResponse {
    has_steam_api_key: bool,
    has_telegram_bot_token: bool,
    has_discord_bot_token: bool,
    telegram_admin_ids: Option<String>,
    telegram_enabled: bool,
    discord_admin_ids: Option<String>,
    discord_enabled: bool,
    tracking_retention_days: u32,
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

async fn api_auth_status(State(state): State<AppState>, jar: CookieJar) -> Json<AuthStatus> {
    let setup_required = !state.passwords_configured().await;
    let session = state.get_session(&jar).await;

    Json(AuthStatus {
        authenticated: session.is_some(),
        access_level: session.map(|s| s.access_level),
        setup_required,
        version: env!("CARGO_PKG_VERSION"),
    })
}

async fn api_login(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
    jar: CookieJar,
    Json(req): Json<LoginRequest>,
) -> (CookieJar, Json<LoginResponse>) {
    let ip = addr.ip().to_string();

    // Check rate limit
    {
        let mut attempts = state.login_attempts.lock().await;
        if let Err(retry_after) = check_rate_limit(&mut attempts, &ip) {
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

    // Record failed attempt
    record_failed_attempt(&mut *state.login_attempts.lock().await, &ip);

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

    let games = snapshots
        .into_iter()
        .map(|report| {
            let (name, image_url) = match app_info.get(&report.app_id) {
                Some((n, img)) => (n.clone(), img.clone()),
                None => (format!("App {}", report.app_id), String::new()),
            };
            // Ensure date is a full ISO 8601 timestamp for JS compatibility
            let date = if report.date.is_empty() {
                String::new()
            } else if report.date.contains('T') {
                report.date
            } else {
                format!("{}T00:00:00Z", report.date)
            };
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
            }
        })
        .collect();

    Json(ApiResponse { games }).into_response()
}

async fn api_game_detail(
    State(state): State<AppState>,
    jar: CookieJar,
    axum::extract::Path(app_id): axum::extract::Path<u32>,
) -> Response {
    if state.get_session(&jar).await.is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Check the game is tracked
    let is_tracked = state.db.is_tracked(app_id).await.unwrap_or(false);
    if !is_tracked {
        return StatusCode::NOT_FOUND.into_response();
    }

    let app_info = state.db.get_all_app_info().await.unwrap_or_default();
    let (name, image_url) = match app_info.get(&app_id) {
        Some((n, img)) => (n.clone(), img.clone()),
        None => (format!("App {}", app_id), String::new()),
    };

    let snapshots = state
        .db
        .get_snapshots_for_game(app_id)
        .await
        .unwrap_or_default();

    let latest = state
        .db
        .get_latest_snapshot(app_id)
        .await
        .ok()
        .flatten()
        .map(|report| {
            let date = if report.date.is_empty() {
                String::new()
            } else if report.date.contains('T') {
                report.date
            } else {
                format!("{}T00:00:00Z", report.date)
            };
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
            }
        });

    let history = snapshots
        .into_iter()
        .map(|s| SnapshotEntry {
            date: if s.date.contains('T') {
                s.date
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
            countries: s.countries,
            fetched_at: s.fetched_at.unwrap_or_default(),
        })
        .collect();

    Json(GameDetailResponse {
        app_id,
        name,
        image_url,
        latest,
        history,
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
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let has_tg_token = config
        .get(CONFIG_TELEGRAM_BOT_TOKEN)
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let tg_ids = config.get(CONFIG_TELEGRAM_ADMIN_IDS).cloned();
    let tg_enabled = config
        .get(CONFIG_TELEGRAM_ENABLED)
        .map(|v| v == "true")
        .unwrap_or(false);
    let has_dc_token = config
        .get(CONFIG_DISCORD_BOT_TOKEN)
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let dc_ids = config.get(CONFIG_DISCORD_ADMIN_IDS).cloned();
    let dc_enabled = config
        .get(CONFIG_DISCORD_ENABLED)
        .map(|v| v == "true")
        .unwrap_or(false);
    let retention_days = config
        .get(CONFIG_TRACKING_RETENTION_DAYS)
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_RETENTION_DAYS);

    Json(AdminConfigResponse {
        has_steam_api_key: has_steam_key,
        has_telegram_bot_token: has_tg_token,
        has_discord_bot_token: has_dc_token,
        telegram_admin_ids: tg_ids,
        telegram_enabled: tg_enabled,
        discord_admin_ids: dc_ids,
        discord_enabled: dc_enabled,
        tracking_retention_days: retention_days,
    })
    .into_response()
}

async fn api_admin_health(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Response {
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
        ServiceHealth { status: ServiceStatus::Ok, message: None }
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
        ServiceHealth { status: ServiceStatus::Disabled, message: None }
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
                Some(h) if !h.is_finished() => {
                    ServiceHealth { status: ServiceStatus::Ok, message: None }
                }
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
        ServiceHealth { status: ServiceStatus::Disabled, message: None }
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
                Some(h) if !h.is_finished() => {
                    ServiceHealth { status: ServiceStatus::Ok, message: None }
                }
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

    Json(HealthResponse { steam, telegram, discord }).into_response()
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
            if let Err(e) = state.db.set_config(CONFIG_STEAM_API_KEY, &key).await {
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
            if let Err(e) = state.db.set_config(CONFIG_TELEGRAM_BOT_TOKEN, &token).await {
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
            if let Err(e) = state.db.set_config(CONFIG_DISCORD_BOT_TOKEN, &token).await {
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

    if let Some(days) = req.tracking_retention_days {
        let days = days.max(1); // minimum 1 day
        if let Err(e) = state
            .db
            .set_config(CONFIG_TRACKING_RETENTION_DAYS, &days.to_string())
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

    let games: Vec<TrackedGameEntry> = ids
        .iter()
        .map(|&id| {
            let (name, image_url) = if let Some((n, img)) = db_info.get(&id) {
                (n.clone(), img.clone())
            } else if let Some(app) = mem_info.get(&id) {
                (app.name.clone(), app.image_url.clone().unwrap_or_default())
            } else {
                (format!("App {id}"), String::new())
            };
            TrackedGameEntry {
                app_id: id,
                name,
                image_url,
                tracked_since: since_map.get(&id).cloned().unwrap_or_default(),
            }
        })
        .collect();

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
            return Json(serde_json::json!({ "success": false, "error": e.to_string() })).into_response();
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
                        let _ = state.db.insert_snapshot_if_changed(&report).await;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to fetch initial data for {app_id}: {e}");
                    }
                }

                // Auto-populate historical data in background
                if state.auto_populate_days > 0 {
                    let bg_state = state.clone();
                    let bg_steam = steam.clone();
                    tokio::spawn(async move {
                        crate::auto_populate_game(&bg_state, &bg_steam, app_id).await;
                    });
                }
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
        Err(e) => Json(serde_json::json!({ "success": false, "error": e.to_string() })).into_response(),
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
        Err(e) => Json(serde_json::json!({ "success": false, "error": e.to_string() })).into_response(),
    }
}

// ── Static file serving ─────────────────────────────────────────────

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    // Try the exact path first
    if let Some(file) = Assets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mime.as_ref())],
            file.data,
        )
            .into_response();
    }

    // Fall back to index.html for SPA routing
    match Assets::get("index.html") {
        Some(file) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html")],
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
        let mut rng = rand::thread_rng();
        (
            rng.gen_range(1..=50u64),
            rng.gen_range(0..=10u64),
            rng.gen_range(0..=5u64),
            rng.gen_range(0..=3u64),
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
    };

    // Insert the fake snapshot
    match state.db.insert_snapshot_if_changed(&fake_report).await {
        Ok(SnapshotChange::Changed { previous }) => {
            crate::telegram::notify_change(&state.db, app_id, &fake_report, &previous).await;
            crate::discord::notify_change(&state.db, app_id, &fake_report, &previous).await;
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
        .route("/api/wishlist/{app_id}", get(api_game_detail))
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
        // Debug routes — uncomment for local testing only
        // .route("/debug/test/{app_id}", get(debug_test_change))
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
