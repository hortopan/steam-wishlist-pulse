use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use chrono::Utc;
use chrono_tz::US::Pacific;
use reqwest::Client;
use serde::Deserialize;
use tokio::sync::{Mutex, RwLock};

use crate::error::{AppError, AppResult};

/// Token-bucket rate limiter for Steam API calls.
///
/// Allows up to `capacity` requests, refilling at `refill_rate` tokens/second.
struct RateLimiter {
    tokens: f64,
    capacity: f64,
    refill_rate: f64,
    last_refill: Instant,
}

impl RateLimiter {
    fn new(capacity: f64, refill_rate: f64) -> Self {
        Self {
            tokens: capacity,
            capacity,
            refill_rate,
            last_refill: Instant::now(),
        }
    }

    /// Wait until a token is available, then consume it.
    async fn acquire(&mut self) {
        loop {
            let now = Instant::now();
            let elapsed = now.duration_since(self.last_refill).as_secs_f64();
            self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity);
            self.last_refill = now;

            if self.tokens >= 1.0 {
                self.tokens -= 1.0;
                return;
            }

            // Sleep until at least one token is available
            let wait = (1.0 - self.tokens) / self.refill_rate;
            tokio::time::sleep(std::time::Duration::from_secs_f64(wait)).await;
        }
    }
}

const WISHLIST_API_URL: &str =
    "https://partner.steam-api.com/IPartnerFinancialsService/GetAppWishlistReporting/v1/";

const STORE_API_URL: &str = "https://store.steampowered.com/api/appdetails";

#[derive(Debug, Clone)]
pub struct AppInfo {
    pub name: String,
    pub image_url: Option<String>,
}

#[derive(Clone)]
pub struct SteamClient {
    http: Client,
    api_key: Arc<RwLock<String>>,
    app_info: Arc<RwLock<HashMap<u32, AppInfo>>>,
    /// Shared rate limiter: 10 requests burst, refilling at 2/sec.
    rate_limiter: Arc<Mutex<RateLimiter>>,
    /// Separate rate limiter for backfill operations so they don't starve normal polling.
    backfill_rate_limiter: Arc<Mutex<RateLimiter>>,
}

#[derive(Debug, Deserialize)]
struct WishlistApiResponse {
    response: Option<WishlistResponseBody>,
}

#[derive(Debug, Deserialize)]
struct WishlistResponseBody {
    date: Option<String>,
    wishlist_summary: Option<WishlistSummary>,
    country_summary: Option<Vec<CountrySummaryEntry>>,
    app_min_date: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WishlistSummary {
    wishlist_adds: Option<i64>,
    wishlist_deletes: Option<i64>,
    wishlist_purchases: Option<i64>,
    wishlist_gifts: Option<i64>,
    wishlist_adds_windows: Option<i64>,
    wishlist_adds_mac: Option<i64>,
    wishlist_adds_linux: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct CountrySummaryEntry {
    country_code: Option<String>,
    #[allow(dead_code)]
    country_name: Option<String>,
    #[allow(dead_code)]
    region: Option<String>,
    summary_actions: Option<CountrySummaryActions>,
}

#[derive(Debug, Deserialize)]
struct CountrySummaryActions {
    wishlist_adds: Option<i64>,
    wishlist_deletes: Option<i64>,
    wishlist_purchases: Option<i64>,
    wishlist_gifts: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct WishlistReport {
    pub app_id: u32,
    pub date: String,
    pub adds: i64,
    pub deletes: i64,
    pub purchases: i64,
    pub gifts: i64,
    /// Platform breakdown for adds.
    pub adds_windows: i64,
    pub adds_mac: i64,
    pub adds_linux: i64,
    /// Per-country breakdown.
    pub countries: Vec<CountryReport>,
    /// ISO 8601 timestamp of when this snapshot was saved to the DB.
    pub fetched_at: Option<String>,
    /// Earliest date for which Steam has data for this app.
    pub app_min_date: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CountryReport {
    pub country_code: String,
    pub adds: i64,
    pub deletes: i64,
    pub purchases: i64,
    pub gifts: i64,
}

/// Validate a Steam Web API key by making a lightweight partner API call
/// using Spacewar (app 480), Valve's universal test app.
pub async fn validate_api_key(key: &str) -> AppResult<()> {
    let client = Client::new();
    let resp = client
        .get(WISHLIST_API_URL)
        .query(&[("key", key), ("appid", "480"), ("date", "2020-01-01")])
        .send()
        .await?;

    let status = resp.status();
    if status == reqwest::StatusCode::FORBIDDEN {
        return Err(AppError::other("Invalid Steam API key"));
    }
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(AppError::other("Steam API key is not authorized"));
    }
    Ok(())
}

impl SteamClient {
    pub fn new(api_key: String, backfill_rate: f64) -> Self {
        Self {
            http: Client::new(),
            api_key: Arc::new(RwLock::new(api_key)),
            app_info: Arc::new(RwLock::new(HashMap::new())),
            // Allow bursts of 10 requests, refill at 2 per second
            rate_limiter: Arc::new(Mutex::new(RateLimiter::new(10.0, 2.0))),
            // Backfill uses a slower, configurable rate to avoid starving normal polling
            backfill_rate_limiter: Arc::new(Mutex::new(RateLimiter::new(3.0, backfill_rate))),
        }
    }

    pub async fn set_api_key(&self, key: String) {
        *self.api_key.write().await = key;
    }

    /// Fetch app details from Steam's public store API and cache them.
    pub async fn fetch_app_name(&self, app_id: u32) -> AppResult<String> {
        // Return cached name if available
        if let Some(info) = self.app_info.read().await.get(&app_id) {
            return Ok(info.name.clone());
        }

        self.rate_limiter.lock().await.acquire().await;

        let resp = self
            .http
            .get(STORE_API_URL)
            .query(&[("appids", &app_id.to_string())])
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            return Err(AppError::other(format!(
                "Steam store API returned {status} for app {app_id}"
            )));
        }

        let body: serde_json::Value = resp.json().await?;

        let data = body
            .get(app_id.to_string())
            .and_then(|entry| entry.get("data"))
            .ok_or_else(|| AppError::other(format!("App {app_id} not found on the Steam store")))?;

        let name = data
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or_else(|| AppError::other(format!("App {app_id} has no name")))?
            .to_string();

        let image_url = data
            .get("header_image")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        self.app_info.write().await.insert(
            app_id,
            AppInfo {
                name: name.clone(),
                image_url,
            },
        );
        Ok(name)
    }

    /// Get a snapshot of the cached app info.
    pub async fn app_info(&self) -> HashMap<u32, AppInfo> {
        self.app_info.read().await.clone()
    }

    pub async fn fetch_wishlist(&self, app_id: u32) -> AppResult<WishlistReport> {
        // Steam uses Pacific Time for daily reporting boundaries.
        let now = Utc::now().with_timezone(&Pacific);
        let date = now.format("%Y-%m-%d").to_string();
        self.fetch_wishlist_for_date(app_id, &date).await
    }

    pub async fn fetch_wishlist_for_date(
        &self,
        app_id: u32,
        date: &str,
    ) -> AppResult<WishlistReport> {
        tracing::debug!("Requesting wishlist: {WISHLIST_API_URL}?appid={app_id}&date={date}");
        self.rate_limiter.lock().await.acquire().await;
        self.fetch_wishlist_inner(app_id, date).await
    }

    /// Shared implementation for fetching wishlist data (called after rate limiting).
    async fn fetch_wishlist_inner(
        &self,
        app_id: u32,
        date: &str,
    ) -> AppResult<WishlistReport> {
        let api_key = self.api_key.read().await.clone();
        let resp = self
            .http
            .get(WISHLIST_API_URL)
            .query(&[
                ("key", api_key.as_str()),
                ("appid", &app_id.to_string()),
                ("date", date),
            ])
            .send()
            .await?;

        tracing::debug!(
            "Response status: {} for app {app_id} date {date}",
            resp.status()
        );

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::other(format!(
                "Steam API returned {status} for app {app_id}: {body}"
            )));
        }

        let body = resp.text().await?;
        tracing::debug!("Got wishlist response from steam for app {app_id}");

        let data: WishlistApiResponse = serde_json::from_str(&body)?;

        let resp_body = data
            .response
            .ok_or_else(|| AppError::other(format!("No wishlist data for app {app_id} on {date}")))?;

        let summary = match resp_body.wishlist_summary {
            Some(s) => s,
            None => {
                return Err(AppError::other(match resp_body.app_min_date {
                    Some(d) => format!("No data for app {app_id} on {date} (earliest available: {d})"),
                    None => format!("No wishlist data for app {app_id} on {date}"),
                }));
            }
        };

        let countries = resp_body
            .country_summary
            .unwrap_or_default()
            .into_iter()
            .filter_map(|c| {
                let actions = c.summary_actions?;
                Some(CountryReport {
                    country_code: c.country_code.unwrap_or_default(),
                    adds: actions.wishlist_adds.unwrap_or(0),
                    deletes: actions.wishlist_deletes.unwrap_or(0),
                    purchases: actions.wishlist_purchases.unwrap_or(0),
                    gifts: actions.wishlist_gifts.unwrap_or(0),
                })
            })
            .collect();

        Ok(WishlistReport {
            app_id,
            date: resp_body.date.unwrap_or_else(|| date.to_string()),
            adds: summary.wishlist_adds.unwrap_or(0),
            deletes: summary.wishlist_deletes.unwrap_or(0),
            purchases: summary.wishlist_purchases.unwrap_or(0),
            gifts: summary.wishlist_gifts.unwrap_or(0),
            adds_windows: summary.wishlist_adds_windows.unwrap_or(0),
            adds_mac: summary.wishlist_adds_mac.unwrap_or(0),
            adds_linux: summary.wishlist_adds_linux.unwrap_or(0),
            countries,
            fetched_at: None,
            app_min_date: resp_body.app_min_date,
        })
    }

    /// Fetch wishlist data for backfill purposes, using the slower backfill rate limiter.
    pub async fn fetch_wishlist_for_backfill(
        &self,
        app_id: u32,
        date: &str,
    ) -> AppResult<WishlistReport> {
        tracing::debug!("Backfill request: appid={app_id}&date={date}");
        self.backfill_rate_limiter.lock().await.acquire().await;
        self.fetch_wishlist_inner(app_id, date).await
    }

    /// Fetch wishlist data for multiple apps, up to 5 at a time.
    pub async fn fetch_all(&self, app_ids: &[u32]) -> Vec<AppResult<WishlistReport>> {
        let mut results = Vec::with_capacity(app_ids.len());
        for chunk in app_ids.chunks(5) {
            let batch = futures::future::join_all(chunk.iter().map(|&id| self.fetch_wishlist(id)));
            results.extend(batch.await);
        }
        results
    }
}
