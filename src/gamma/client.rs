// src/gamma/client.rs
//
// HTTP client for the Polymarket Gamma API (gamma-api.polymarket.com).
// No authentication required. Rate-limited via exponential backoff on 429.
//
// Primary use: fetch the effective end time for condition_ids that the Data API
// does not return (resolved/closed markets disappear from the positions endpoint).
//
// Date priority for resolved markets:
//   1. closedTime  — actual resolution timestamp (most accurate for entry_timing)
//   2. endDate     — scheduled end date (fallback for open/future markets)
//
// Performance: results are cached in a shared in-memory HashMap. Within the
// Sep 28–Oct 24 scan window there are only ~20-50 unique markets, so after the
// first few wallets are scored the cache is fully warm and no further Gamma
// requests are needed.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};
use tokio::time::{sleep, Duration};
use tracing::{debug, warn};

const DEFAULT_BASE_URL: &str = "https://gamma-api.polymarket.com";
const MAX_RETRIES: u32 = 5;
// Max concurrent in-flight Gamma HTTP requests.
// With double-checked locking after semaphore acquire, at most
// GLOBAL_GAMMA_CONCURRENCY duplicate fetches per condition_id occur at startup.
// 2 concurrent @ ~200ms latency = 10 req/s, well under the 30 req/s limit.
// After cache warms (~50 unique condition_ids fetched), zero further requests.
const GLOBAL_GAMMA_CONCURRENCY: usize = 2;

// ── Response types ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct GammaMarket {
    #[serde(rename = "conditionId", default)]
    condition_id: String,
    /// Scheduled end date (may be far in the future even for resolved markets).
    #[serde(rename = "endDate")]
    end_date: Option<String>,
    /// Actual resolution timestamp — present only for closed/resolved markets.
    /// Format: `YYYY-MM-DD HH:MM:SS+00` (Postgres-style UTC).
    #[serde(rename = "closedTime")]
    closed_time: Option<String>,
    #[serde(default)]
    closed: bool,
}

impl GammaMarket {
    /// Returns the best available end time string:
    /// - `closedTime` for resolved markets (actual resolution time)
    /// - `endDate` for open/future markets (scheduled end)
    #[must_use]
    fn effective_end_date(&self) -> Option<String> {
        if self.closed {
            if let Some(ct) = &self.closed_time {
                if !ct.is_empty() {
                    return normalise_closed_time(ct);
                }
            }
        }
        self.end_date.clone().filter(|s| !s.is_empty())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Normalise a `closedTime` string from the Gamma API to an RFC 3339 string.
///
/// The Gamma API returns `closedTime` in Postgres-style UTC format:
///   "YYYY-MM-DD HH:MM:SS+00"   (missing the minutes part of the offset)
///   "YYYY-MM-DD HH:MM:SS+00:00" (full RFC 3339 offset — also handled)
///
/// `replacen("+00", "Z", 1)` used to break the second variant because it
/// would produce "YYYY-MM-DDTHH:MM:SSZ:00".  This function handles both by
/// trying a direct RFC 3339 parse first, then performing the minimal fixup
/// needed for the Postgres variant.
fn normalise_closed_time(ct: &str) -> Option<String> {
    // Try parsing directly first — handles full ISO 8601 / RFC 3339 variants
    // including "+00:00", "Z", and any other valid offsets.
    if let Ok(dt) = DateTime::parse_from_rfc3339(ct) {
        return Some(dt.with_timezone(&Utc).to_rfc3339());
    }
    // Handle Postgres-style "YYYY-MM-DD HH:MM:SS+00" by replacing the space
    // with "T" and appending ":00" to turn the bare "+00" into "+00:00".
    let normalised = ct.replacen(' ', "T", 1);
    let normalised = if normalised.ends_with("+00") {
        format!("{normalised}:00")
    } else {
        normalised
    };
    DateTime::parse_from_rfc3339(&normalised)
        .ok()
        .map(|dt| dt.with_timezone(&Utc).to_rfc3339())
}

// ── Client ────────────────────────────────────────────────────────────────────

/// HTTP client for the Gamma API. `Send + Sync` — safe to wrap in `Arc`.
///
/// Maintains a shared cache of condition_id → effective_end_date so that
/// each unique market is only fetched once across all wallet scorings.
pub struct GammaClient {
    http: reqwest::Client,
    semaphore: Arc<Semaphore>,
    /// Cache: condition_id → Some(date) if found, None if not found/no date.
    cache: Arc<Mutex<HashMap<String, Option<String>>>>,
    base_url: String,
}

impl GammaClient {
    /// Reads `POLYMARKET_GAMMA_API_URL` (optional, defaults to
    /// `https://gamma-api.polymarket.com`) from the environment.
    ///
    /// # Panics
    /// Panics if the TLS backend fails to initialise (unrecoverable at startup).
    #[must_use]
    pub fn new() -> Self {
        let base_url = std::env::var("POLYMARKET_GAMMA_API_URL")
            .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        Self {
            // reqwest::ClientBuilder::build() only fails if TLS initialisation
            // fails — unrecoverable at startup. expect() is intentional here.
            #[allow(clippy::expect_used)]
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("Failed to build reqwest client for Gamma API"),
            semaphore: Arc::new(Semaphore::new(GLOBAL_GAMMA_CONCURRENCY)),
            cache: Arc::new(Mutex::new(HashMap::new())),
            base_url,
        }
    }

    /// Fetch the effective end time for each condition_id.
    ///
    /// Cache-first: condition_ids already resolved are returned immediately
    /// without any HTTP requests. Unknown ids are fetched one-at-a-time with
    /// the global semaphore capping concurrency.
    ///
    /// For resolved markets returns `closedTime` (actual resolution); for open
    /// markets returns `endDate` (scheduled). Silently skips markets not found
    /// or where no date is available.
    ///
    /// # Errors
    /// Returns an error if an individual Gamma fetch fails after all retries.
    pub async fn get_end_dates(
        &self,
        condition_ids: &[&str],
    ) -> Result<HashMap<String, String>> {
        if condition_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut result: HashMap<String, String> = HashMap::new();
        let mut uncached: Vec<&str> = Vec::new();

        // Check cache first (single lock acquisition).
        {
            let cache = self.cache.lock().await;
            for &cid in condition_ids {
                match cache.get(cid) {
                    Some(Some(date)) => {
                        result.insert(cid.to_string(), date.clone());
                    }
                    Some(None) => {} // known miss — skip
                    None => uncached.push(cid),
                }
            }
        }

        if uncached.is_empty() {
            return Ok(result);
        }

        debug!(
            cached = condition_ids.len() - uncached.len(),
            fetching = uncached.len(),
            "Gamma cache status"
        );

        // Fetch uncached ids sequentially under the semaphore.
        // Sequential is fine: after the first few wallets the cache is warm.
        for cid in uncached {
            let outcome = self.fetch_single_cached(cid).await;
            match outcome {
                Ok(Some(date)) => {
                    result.insert(cid.to_string(), date);
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(error = %e, cid, "Gamma fetch failed, skipping");
                }
            }
        }

        Ok(result)
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Fetch a single condition_id, updating the cache regardless of outcome.
    /// Uses double-checked locking: check cache → acquire semaphore → check
    /// cache again. This ensures that once the first fetch completes, all
    /// queued wallets waiting on the semaphore get a cache hit instead of
    /// firing duplicate requests.
    async fn fetch_single_cached(&self, condition_id: &str) -> Result<Option<String>> {
        // First check — fast path before queuing on semaphore.
        {
            let cache = self.cache.lock().await;
            if let Some(cached) = cache.get(condition_id) {
                return Ok(cached.clone());
            }
        }

        // The semaphore is only closed if we drop it, which never happens while
        // `self` is alive. `expect` here is equivalent to an unreachable assertion.
        #[allow(clippy::expect_used)]
        let _permit = self.semaphore.acquire().await.expect("semaphore closed");

        // Second check after acquiring semaphore — another wallet that held
        // the semaphore before us may have already fetched and cached this id.
        {
            let cache = self.cache.lock().await;
            if let Some(cached) = cache.get(condition_id) {
                return Ok(cached.clone());
            }
        }

        let base_url = &self.base_url;
        let url = format!("{base_url}/markets?condition_ids={condition_id}");
        let markets = self.fetch_markets_with_retry(&url).await?;

        let date = markets
            .into_iter()
            .find(|m| m.condition_id.eq_ignore_ascii_case(condition_id))
            .and_then(|m| m.effective_end_date());

        // Store in cache (Some(date) or None for miss).
        {
            let mut cache = self.cache.lock().await;
            cache.insert(condition_id.to_string(), date.clone());
        }

        Ok(date)
    }

    async fn fetch_markets_with_retry(&self, url: &str) -> Result<Vec<GammaMarket>> {
        let mut attempts = 0u32;
        let mut backoff_secs = 2u64;

        loop {
            let result = self.http.get(url).send().await;

            match result {
                Err(e) => {
                    attempts += 1;
                    if attempts >= MAX_RETRIES {
                        return Err(e).with_context(|| {
                            format!("Gamma API request failed after {MAX_RETRIES} attempts: {url}")
                        });
                    }
                    warn!(attempt = attempts, error = %e, wait_secs = backoff_secs, "Gamma API connection error, retrying");
                    sleep(Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(60);
                }
                Ok(resp) => {
                    let status = resp.status();

                    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                        attempts += 1;
                        if attempts >= MAX_RETRIES {
                            anyhow::bail!("Gamma API rate-limited after {MAX_RETRIES} attempts: {url}");
                        }
                        warn!(attempt = attempts, "Gamma API 429, backing off 60s");
                        sleep(Duration::from_secs(60)).await;
                        continue;
                    }

                    if status.is_server_error() {
                        attempts += 1;
                        if attempts >= MAX_RETRIES {
                            anyhow::bail!("Gamma API HTTP {status} after {MAX_RETRIES} attempts: {url}");
                        }
                        warn!(attempt = attempts, %status, wait_secs = backoff_secs, "Gamma API server error, retrying");
                        sleep(Duration::from_secs(backoff_secs)).await;
                        backoff_secs = (backoff_secs * 2).min(60);
                        continue;
                    }

                    if !status.is_success() {
                        anyhow::bail!("Gamma API returned HTTP {status}: {url}");
                    }

                    let text = resp
                        .text()
                        .await
                        .with_context(|| format!("Failed to read Gamma API response body from {url}"))?;

                    let to_parse = if text.trim() == "null" { "[]" } else { &text };

                    let markets = serde_json::from_str::<Vec<GammaMarket>>(to_parse)
                        .with_context(|| format!("Failed to deserialize Gamma API response from {url}"))?;

                    return Ok(markets);
                }
            }
        }
    }
}

impl Default for GammaClient {
    fn default() -> Self {
        Self::new()
    }
}
