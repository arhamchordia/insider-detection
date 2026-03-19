// src/subgraph/orderbook.rs
//
// Enumerates all unique maker addresses from the Goldsky orderbook subgraph
// using cursor-based pagination (id_gt) so we never skip records and can
// handle the full history without time-range filters.

use anyhow::{Context, Result};
use serde::Deserialize;
use sqlx::PgPool;
use std::collections::HashSet;
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use tracing::{info, warn};

const DEFAULT_GOLDSKY_URL: &str =
    "https://api.goldsky.com/api/public/project_cl6mb8i9h0003e201j6li0diw/subgraphs/orderbook-subgraph/0.0.1/gn";

const PAGE_SIZE: usize = 1000;

pub struct SubgraphClient {
    http: reqwest::Client,
    url: String,
}

impl SubgraphClient {
    /// Reads `GOLDSKY_SUBGRAPH_URL` from env (optional, defaults to the
    /// public Goldsky endpoint). Scan window is passed per-call, not stored here.
    #[must_use]
    pub fn new() -> Self {
        let url = std::env::var("GOLDSKY_SUBGRAPH_URL")
            .unwrap_or_else(|_| DEFAULT_GOLDSKY_URL.to_string());
        Self {
            http: reqwest::Client::new(),
            url,
        }
    }

    /// Streams each page's new maker addresses through `tx` as they arrive so
    /// the scorer can start immediately. Sends deduplicated addresses (across all
    /// pages within this window) per batch. The channel is closed when enumeration
    /// of the window finishes.
    ///
    /// `window_from` / `window_to` are Unix timestamps filtering the subgraph events.
    /// Pass the same value for both to get an empty result (no events yet).
    ///
    /// # Errors
    /// Returns an error if a subgraph page fetch fails after all retries.
    #[tracing::instrument(skip(self, pool, tx), fields(window_from, window_to))]
    pub async fn enumerate_wallets_streaming(
        &self,
        pool: &PgPool,
        tx: mpsc::Sender<Vec<String>>,
        window_from: u64,
        window_to: u64,
    ) -> Result<usize> {
        let mut seen: HashSet<String> = HashSet::new();

        // Clear any leftover cursor from a previous sequential run.
        if let Err(e) = sqlx::query!("DELETE FROM scorer_state WHERE key = 'enum_cursor'")
            .execute(pool)
            .await
        {
            warn!(error = %e, "Failed to clear enum cursor — pagination may start from stale position");
        }

        let mut cursor = String::new();
        let mut pages = 0usize;

        loop {
            let events = self
                .fetch_page(&cursor, window_from, window_to)
                .await
                .context("Failed to fetch subgraph page")?;

            if events.is_empty() {
                break;
            }

            // Collect addresses that are new this page.
            let mut new_this_page: Vec<String> = Vec::new();
            for event in &events {
                if let Some(maker) = &event.maker {
                    if !maker.is_empty() {
                        let addr = maker.to_lowercase();
                        if seen.insert(addr.clone()) {
                            new_this_page.push(addr);
                        }
                    }
                }
            }

            cursor = events.last().map(|e| e.id.clone()).unwrap_or_default();
            pages += 1;

            if pages.is_multiple_of(10) {
                info!(
                    pages,
                    unique_wallets = seen.len(),
                    "Subgraph enumeration progress"
                );
            }

            if !new_this_page.is_empty() {
                // If receiver hung up we stop — scorer was cancelled.
                if tx.send(new_this_page).await.is_err() {
                    info!("Scorer channel closed, stopping enumeration");
                    break;
                }
            }

            if events.len() < PAGE_SIZE {
                break;
            }

            sleep(Duration::from_millis(200)).await;
        }

        if let Err(e) = sqlx::query!("DELETE FROM scorer_state WHERE key = 'enum_cursor'")
            .execute(pool)
            .await
        {
            warn!(error = %e, "Failed to clear enum cursor — pagination may start from stale position");
        }

        info!(
            total_pages = pages,
            unique_wallets = seen.len(),
            "Subgraph enumeration complete"
        );

        Ok(seen.len())
    }

    async fn fetch_page(
        &self,
        after_id: &str,
        window_from: u64,
        window_to: u64,
    ) -> Result<Vec<OrderFilledEvent>> {
        // Sanitise cursor: subgraph IDs are hex digits + hyphens only (e.g. "0xabc123-1234").
        // Strip double-quotes and backslashes before interpolating into the GraphQL body
        // to prevent injection via a malformed cursor value stored in the DB.
        let safe_after_id = after_id.replace('"', "").replace('\\', "");

        let query = format!(
            r#"{{
              orderFilledEvents(
                first: {PAGE_SIZE},
                orderBy: id,
                orderDirection: asc,
                where: {{ id_gt: "{safe_after_id}", timestamp_gte: "{window_from}", timestamp_lte: "{window_to}" }}
              ) {{
                id
                maker
              }}
            }}"#
        );

        let body = serde_json::json!({ "query": query });

        let mut attempts = 0u32;
        loop {
            let resp = self
                .http
                .post(&self.url)
                .json(&body)
                .send()
                .await
                .context("HTTP request to Goldsky failed")?;

            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                attempts += 1;
                warn!(attempt = attempts, "Goldsky rate limited, waiting 60s");
                sleep(Duration::from_secs(60)).await;
                continue;
            }

            if !resp.status().is_success() {
                let status = resp.status();
                attempts += 1;
                if attempts >= 5 {
                    anyhow::bail!("Goldsky returned HTTP {status} after {attempts} attempts");
                }
                let wait = 2u64.pow(attempts).min(30);
                warn!(attempt = attempts, wait_secs = wait, %status, "Goldsky error, retrying");
                sleep(Duration::from_secs(wait)).await;
                continue;
            }

            let gql: GraphQLResponse = resp
                .json()
                .await
                .context("Failed to parse Goldsky GraphQL response")?;

            if !gql.errors.is_empty() {
                attempts += 1;
                if attempts >= 5 {
                    anyhow::bail!("Goldsky GraphQL errors: {:?}", gql.errors);
                }
                warn!(attempt = attempts, errors = ?gql.errors, "Goldsky GraphQL errors, retrying");
                sleep(Duration::from_secs(2u64.pow(attempts).min(30))).await;
                continue;
            }

            return Ok(gql.data.map(|d| d.order_filled_events).unwrap_or_default());
        }
    }
}

impl Default for SubgraphClient {
    fn default() -> Self {
        Self::new()
    }
}

// ── Deserialization types ─────────────────────────────────────────────────────

#[derive(Deserialize)]
struct GraphQLResponse {
    data: Option<ResponseData>,
    #[serde(default)]
    errors: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResponseData {
    order_filled_events: Vec<OrderFilledEvent>,
}

#[derive(Deserialize)]
struct OrderFilledEvent {
    id: String,
    maker: Option<String>,
}
