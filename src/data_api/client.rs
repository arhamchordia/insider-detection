// src/data_api/client.rs
//
// HTTP client for the Polymarket Data API (data-api.polymarket.com).
// Authenticated with a Bearer token. Rate-limited via a Tokio Semaphore
// (5 concurrent requests). Retries on 429 and 5xx with 2-second back-off,
// up to 3 attempts.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::time::{sleep, Duration};
use tracing::warn;

const DEFAULT_BASE_URL: &str = "https://data-api.polymarket.com";
// Data API: /trades 200 req/10s, /positions 150 req/10s — 30 concurrent is safe.
const MAX_CONCURRENT: usize = 30;
const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF_SECS: u64 = 2;

// ── Public structs ────────────────────────────────────────────────────────────

/// A single trade record returned from GET /trades.
/// Field names match the live API response exactly.
#[derive(Debug, Clone, Deserialize)]
pub struct Trade {
    #[serde(rename = "proxyWallet", default)]
    pub proxy_wallet: String,
    #[serde(rename = "conditionId", default)]
    pub condition_id: String,
    #[serde(default)]
    pub side: String,
    #[serde(default)]
    pub price: f64,
    #[serde(default)]
    pub size: f64,
    #[serde(default)]
    pub timestamp: i64,
    #[serde(default)]
    pub title: Option<String>,
}

/// A position record returned from GET /positions.
#[derive(Debug, Clone, Deserialize)]
pub struct Position {
    #[serde(rename = "proxyWallet", default)]
    pub proxy_wallet: String,
    #[serde(rename = "conditionId", default)]
    pub condition_id: String,
    #[serde(default)]
    pub size: f64,
    #[serde(rename = "avgPrice", default)]
    pub avg_price: f64,
    #[serde(rename = "realizedPnl", default)]
    pub realized_pnl: f64,
    #[serde(rename = "totalBought", default)]
    pub total_bought: f64,
    #[serde(rename = "cashPnl", default)]
    pub cash_pnl: f64,
    /// Date string: "YYYY-MM-DD" or "YYYY-MM-DDTHH:MM:SSZ"
    #[serde(rename = "endDate", default)]
    pub end_date: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

/// An activity record returned from GET /activity.
#[derive(Debug, Clone, Deserialize)]
pub struct Activity {
    #[serde(rename = "proxyWallet", default)]
    pub proxy_wallet: String,
    #[serde(default)]
    pub timestamp: i64,
    #[serde(rename = "type", default)]
    pub activity_type: String,
}

// ── Client ────────────────────────────────────────────────────────────────────

pub struct DataApiClient {
    http: reqwest::Client,
    semaphore: Arc<Semaphore>,
    base_url: String,
    api_key: String,
}

impl DataApiClient {
    /// Reads `POLYMARKET_API_KEY` (required) and `POLYMARKET_DATA_API_URL`
    /// (optional, defaults to `https://data-api.polymarket.com`) from the environment.
    pub fn new() -> Result<Self> {
        let api_key = std::env::var("POLYMARKET_API_KEY")
            .context("POLYMARKET_API_KEY environment variable not set")?;
        let base_url = std::env::var("POLYMARKET_DATA_API_URL")
            .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        Ok(Self {
            http: reqwest::Client::new(),
            semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT)),
            base_url,
            api_key,
        })
    }

    /// GET `/trades?user={addr}&limit=500&sortBy=TIMESTAMP&sortDirection=ASC`
    ///
    /// # Errors
    /// Returns an error if the HTTP request fails after all retries or
    /// the response cannot be deserialised.
    #[tracing::instrument(skip(self), fields(address))]
    pub async fn get_trades(&self, address: &str) -> Result<Vec<Trade>> {
        let url = format!(
            "{}/trades?user={address}&limit=500&sortBy=TIMESTAMP&sortDirection=ASC",
            self.base_url
        );
        let data: Vec<Trade> = self
            .get_json(&url)
            .await
            .with_context(|| format!("get_trades failed for {address}"))?;
        Ok(data)
    }

    /// GET `/positions?user={addr}&limit=500`
    ///
    /// # Errors
    /// Returns an error if the HTTP request fails after all retries or
    /// the response cannot be deserialised.
    #[tracing::instrument(skip(self), fields(address))]
    pub async fn get_positions(&self, address: &str) -> Result<Vec<Position>> {
        let url = format!("{}/positions?user={address}&limit=500", self.base_url);
        let data: Vec<Position> = self
            .get_json(&url)
            .await
            .with_context(|| format!("get_positions failed for {address}"))?;
        Ok(data)
    }

    /// GET `/activity?user={addr}&limit=1&sortBy=TIMESTAMP&sortDirection=ASC`
    ///
    /// Returns the earliest activity timestamp, or `None` if the wallet has no activity.
    ///
    /// # Errors
    /// Returns an error if the HTTP request fails after all retries or
    /// the response cannot be deserialised.
    #[tracing::instrument(skip(self), fields(address))]
    pub async fn get_first_activity(&self, address: &str) -> Result<Option<i64>> {
        let url = format!(
            "{}/activity?user={address}&limit=1&sortBy=TIMESTAMP&sortDirection=ASC",
            self.base_url
        );
        let data: Vec<Activity> = self
            .get_json(&url)
            .await
            .with_context(|| format!("get_first_activity failed for {address}"))?;
        Ok(data.into_iter().next().map(|a| a.timestamp))
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .context("Semaphore acquire failed")?;

        let mut attempts = 0u32;
        let mut backoff_secs = INITIAL_BACKOFF_SECS;

        loop {
            let result = self
                .http
                .get(url)
                .header("Authorization", format!("Bearer {api_key}", api_key = self.api_key))
                .send()
                .await;

            match result {
                Err(e) => {
                    attempts += 1;
                    if attempts >= MAX_RETRIES {
                        return Err(e).context(format!(
                            "HTTP request failed after {MAX_RETRIES} attempts: {url}"
                        ));
                    }
                    warn!(attempt = attempts, url, error = %e, wait_secs = backoff_secs, "HTTP request error, retrying");
                    sleep(Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(60);
                }
                Ok(resp) => {
                    let status = resp.status();

                    // 429 Too Many Requests — fixed 60s back-off (respect server's rate limit window).
                    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                        attempts += 1;
                        if attempts >= MAX_RETRIES {
                            anyhow::bail!(
                                "Data API rate-limited after {MAX_RETRIES} attempts: {url}"
                            );
                        }
                        warn!(attempt = attempts, url, "Data API 429, backing off 60s");
                        sleep(Duration::from_secs(60)).await;
                        continue;
                    }

                    // 5xx Server errors — exponential backoff separate from 429.
                    if status.is_server_error() {
                        attempts += 1;
                        if attempts >= MAX_RETRIES {
                            anyhow::bail!(
                                "Data API returned HTTP {status} after {MAX_RETRIES} attempts: {url}"
                            );
                        }
                        warn!(
                            attempt = attempts,
                            url,
                            %status,
                            wait_secs = backoff_secs,
                            "Data API server error, retrying"
                        );
                        sleep(Duration::from_secs(backoff_secs)).await;
                        backoff_secs = (backoff_secs * 2).min(60);
                        continue;
                    }

                    if !status.is_success() {
                        anyhow::bail!("Data API returned HTTP {status}: {url}");
                    }

                    let bytes = resp
                        .bytes()
                        .await
                        .with_context(|| format!("Failed to read response body from {url}"))?;

                    // Some endpoints return null for wallets with no activity.
                    // Treat null as an empty array for Vec<T> targets.
                    let text = std::str::from_utf8(&bytes)
                        .with_context(|| format!("Non-UTF8 response from {url}"))?;
                    let to_parse = if text.trim() == "null" { "[]" } else { text };

                    let parsed = serde_json::from_str::<T>(to_parse)
                        .with_context(|| format!("Failed to deserialize response from {url}"))?;

                    return Ok(parsed);
                }
            }
        }
    }
}
