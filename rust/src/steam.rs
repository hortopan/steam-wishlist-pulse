use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use chrono_tz::US::Pacific;
use reqwest::Client;
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::error::{AppError, AppResult};

const WISHLIST_API_URL: &str =
    "https://partner.steam-api.com/IPartnerFinancialsService/GetAppWishlistReporting/v1/";

const STORE_API_URL: &str = "https://store.steampowered.com/api/appdetails";

#[derive(Debug, Clone)]
pub struct AppInfo {
    pub name: String,
    pub image_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SteamClient {
    http: Client,
    api_key: Arc<RwLock<String>>,
    app_info: Arc<RwLock<HashMap<u32, AppInfo>>>,
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
    wishlist_adds: Option<u64>,
    wishlist_deletes: Option<u64>,
    wishlist_purchases: Option<u64>,
    wishlist_gifts: Option<u64>,
    wishlist_adds_windows: Option<u64>,
    wishlist_adds_mac: Option<u64>,
    wishlist_adds_linux: Option<u64>,
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
    wishlist_adds: Option<u64>,
    wishlist_deletes: Option<u64>,
    wishlist_purchases: Option<u64>,
    wishlist_gifts: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct WishlistReport {
    pub app_id: u32,
    pub date: String,
    pub adds: u64,
    pub deletes: u64,
    pub purchases: u64,
    pub gifts: u64,
    /// Platform breakdown for adds.
    pub adds_windows: u64,
    pub adds_mac: u64,
    pub adds_linux: u64,
    /// Per-country breakdown.
    pub countries: Vec<CountryReport>,
    /// ISO 8601 timestamp of when this snapshot was saved to the DB.
    pub fetched_at: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CountryReport {
    pub country_code: String,
    pub adds: u64,
    pub deletes: u64,
    pub purchases: u64,
    pub gifts: u64,
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
    pub fn new(api_key: String) -> Self {
        Self {
            http: Client::new(),
            api_key: Arc::new(RwLock::new(api_key)),
            app_info: Arc::new(RwLock::new(HashMap::new())),
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
        tracing::info!("Requesting wishlist: {WISHLIST_API_URL}?appid={app_id}&date={date}");

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

        tracing::info!(
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
        tracing::info!("Got wishlist response from steam for app {app_id}");

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
        })
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
