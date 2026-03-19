// src/main_scorer.rs
//
// Entry point for the `scorer` binary.
//
// Modes:
//   Fixed window  — SCAN_WINDOW_FROM and SCAN_WINDOW_TO both set.
//                   Scores all wallets in the window and exits.
//   Perpetual     — SCAN_WINDOW_TO not set (default).
//                   Polls every POLL_INTERVAL_SECS seconds, advancing the
//                   window forward so new on-chain activity is picked up
//                   in real time. Never exits unless killed.
//
// Env vars (all optional except DATABASE_URL and POLYMARKET_API_KEY):
//   SCAN_WINDOW_FROM     Unix timestamp — start of first window (default: now)
//   SCAN_WINDOW_TO       Unix timestamp — end of window; omit for perpetual mode
//   POLL_INTERVAL_SECS   Seconds between polls in perpetual mode (default: 60)

use anyhow::{Context, Result};
use insider_detection::{
    data_api::client::DataApiClient,
    scorer::model::score_all,
    subgraph::orderbook::SubgraphClient,
};
use sqlx::postgres::PgPoolOptions;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::time::{sleep, Duration};
use tracing::info;

// Channel buffer: how many pages of wallets can queue up before enumeration
// blocks waiting for the scorer to catch up.
const CHANNEL_BUFFER: usize = 8;

fn now_unix() -> u64 {
    // duration_since(UNIX_EPOCH) only errors if the system clock is set before
    // 1970-01-01 UTC, which is a fatal misconfiguration. expect() is intentional.
    #[allow(clippy::expect_used)]
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before Unix epoch")
        .as_secs()
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let database_url =
        std::env::var("DATABASE_URL").context("DATABASE_URL environment variable not set")?;

    // ── Scan window config ────────────────────────────────────────────────────
    let mut window_from: u64 = std::env::var("SCAN_WINDOW_FROM")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(now_unix);

    let fixed_window_to: Option<u64> = std::env::var("SCAN_WINDOW_TO")
        .ok()
        .and_then(|v| v.parse().ok());

    let perpetual = fixed_window_to.is_none();

    let poll_interval_secs: u64 = std::env::var("POLL_INTERVAL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);

    if perpetual {
        info!(
            window_from,
            poll_interval_secs, "Starting in perpetual mode — will poll indefinitely"
        );
    } else if let Some(wt) = fixed_window_to {
        info!(window_from, window_to = wt, "Starting in fixed-window mode");
    }

    // ── Infrastructure setup ─────────────────────────────────────────────────
    info!("Connecting to database");
    let pool = Arc::new(
        PgPoolOptions::new()
            .max_connections(10)
            .connect(&database_url)
            .await
            .context("Failed to connect to PostgreSQL")?,
    );

    let data_api = Arc::new(
        DataApiClient::new().context("Failed to initialise Data API client")?,
    );
    let subgraph = Arc::new(SubgraphClient::new());

    // ── Main loop ─────────────────────────────────────────────────────────────
    loop {
        let window_to = fixed_window_to.unwrap_or_else(now_unix);

        info!(window_from, window_to, "Starting window scan");

        // Reload freshness filter from DB each iteration so wallets scored in a
        // previous window aren't re-scored until 24 hours have passed.
        let scored: Arc<HashSet<String>> = Arc::new(
            sqlx::query_scalar!(
                "SELECT address FROM wallet_scores WHERE scored_at > NOW() - INTERVAL '24 hours'"
            )
            .fetch_all(&*pool)
            .await
            .context("Failed to load scored-wallet freshness filter")?
            .into_iter()
            .collect(),
        );

        info!(already_scored = scored.len(), "Loaded freshness filter");

        // Pipeline: enumeration sends pages → scorer receives and scores concurrently.
        let (tx, mut rx) = mpsc::channel::<Vec<String>>(CHANNEL_BUFFER);

        let pool_enum = Arc::clone(&pool);
        let subgraph_enum = Arc::clone(&subgraph);
        let enum_handle = tokio::spawn(async move {
            subgraph_enum
                .enumerate_wallets_streaming(&pool_enum, tx, window_from, window_to)
                .await
                .context("Subgraph enumeration failed")
        });

        let pool_score = Arc::clone(&pool);
        let data_api_score = Arc::clone(&data_api);
        let score_handle = tokio::spawn(async move {
            let mut total_scored = 0usize;
            while let Some(batch) = rx.recv().await {
                let to_score: Vec<String> = batch
                    .into_iter()
                    .filter(|a| !scored.contains(a))
                    .collect();

                if to_score.is_empty() {
                    continue;
                }

                total_scored += to_score.len();
                if let Err(e) = score_all(&pool_score, &data_api_score, &to_score).await {
                    tracing::error!(error = %e, "score_all failed for batch");
                }
            }
            total_scored
        });

        let (enum_result, score_result) = tokio::join!(enum_handle, score_handle);

        let total_wallets = enum_result.context("Enumerator task panicked")??;
        let total_scored = score_result.context("Scorer task panicked")?;

        info!(window_from, window_to, total_wallets, total_scored, "Window complete");

        // Print per-window summary.
        let flagged_count: i64 =
            sqlx::query_scalar!("SELECT COUNT(*) FROM wallet_scores WHERE flagged = TRUE")
                .fetch_one(&*pool)
                .await
                .unwrap_or(Some(0))
                .unwrap_or(0);

        info!(flagged = flagged_count, "Flagged wallets in DB");

        if !perpetual {
            // Fixed-window mode: print top suspects and exit.
            let top5 = sqlx::query!(
                r#"
                SELECT address, CAST(score AS FLOAT8) as score
                FROM wallet_scores
                WHERE flagged = TRUE
                ORDER BY score DESC
                LIMIT 5
                "#
            )
            .fetch_all(&*pool)
            .await
            .context("Failed to query top suspects")?;

            info!("Top 5 flagged suspects:");
            for row in top5 {
                info!(
                    address = %row.address,
                    score = row.score.unwrap_or(0.0),
                    "  suspect"
                );
            }

            break;
        }

        // Perpetual mode: advance window and sleep until next poll.
        window_from = window_to;
        info!(
            next_window_from = window_from,
            sleep_secs = poll_interval_secs,
            "Sleeping until next poll"
        );
        sleep(Duration::from_secs(poll_interval_secs)).await;
    }

    Ok(())
}
