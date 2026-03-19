// src/main_score_wallet.rs
//
// On-demand wallet scorer. Does NOT interfere with the running `scorer` binary.
//
// Usage:
//   cargo run --bin score-wallet -- <address> [address2] [address3] ...
//
// Or with env DATABASE_URL set:
//   DATABASE_URL=... cargo run --bin score-wallet -- 0xabc... 0xdef...
//
// Fetches Data API data, enriches end_dates via Gamma API, computes all 5
// factors, prints the result to stdout, and upserts to wallet_scores (same
// table the scorer uses).

use anyhow::{Context, Result};
use insider_detection::{
    data_api::client::{DataApiClient, Position},
    gamma::client::GammaClient,
    scorer::factors::{
        is_insider_susceptible, W_CONCENTRATION, W_ENTRY_TIMING, W_SIZE, W_WALLET_AGE, W_WIN_RATE,
    },
    scorer::model::{score_wallet, upsert_score},
};
use sqlx::postgres::PgPoolOptions;
use std::collections::HashSet;
use tracing::{error, info, warn};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let addresses: Vec<String> = std::env::args()
        .skip(1)
        .map(|a| a.to_lowercase())
        .collect();

    if addresses.is_empty() {
        error!("Usage: score-wallet <address> [address2] ...");
        std::process::exit(1);
    }

    let database_url =
        std::env::var("DATABASE_URL").context("DATABASE_URL environment variable not set")?;

    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(&database_url)
        .await
        .context("Failed to connect to PostgreSQL")?;

    let data_api = DataApiClient::new().context("Failed to initialise Data API client")?;
    let gamma = GammaClient::new();

    for address in &addresses {
        info!(address, "Scoring wallet");

        let (trades_res, positions_res, activity_res) = tokio::join!(
            data_api.get_trades(address),
            data_api.get_positions(address),
            data_api.get_first_activity(address),
        );

        let trades = match trades_res {
            Ok(t) => t,
            Err(e) => { error!(address, error = %e, "Failed to fetch trades"); continue; }
        };
        let positions = match positions_res {
            Ok(p) => p,
            Err(e) => { error!(address, error = %e, "Failed to fetch positions"); continue; }
        };
        let first_activity_ts = match activity_res {
            Ok(a) => a,
            Err(e) => { error!(address, error = %e, "Failed to fetch activity"); continue; }
        };

        info!(address, trades_raw = trades.len(), positions_raw = positions.len(), "Fetched raw data");

        // Filter to insider-susceptible markets only.
        let susceptible_ids: HashSet<&str> = trades
            .iter()
            .filter(|t| is_insider_susceptible(t.title.as_deref()))
            .map(|t| t.condition_id.as_str())
            .collect();
        let filtered_trades: Vec<_> = trades.iter()
            .filter(|t| susceptible_ids.contains(t.condition_id.as_str()))
            .cloned().collect();
        let filtered_positions: Vec<_> = positions.iter()
            .filter(|p| susceptible_ids.contains(p.condition_id.as_str()))
            .cloned().collect();

        info!(address, trades_insider = filtered_trades.len(), "Filtered to insider-susceptible markets");

        // Determine which condition_ids need end_date from Gamma.
        let have_end_date: HashSet<&str> = filtered_positions
            .iter()
            .filter(|p| p.end_date.is_some())
            .map(|p| p.condition_id.as_str())
            .collect();

        let missing_ids: Vec<&str> = filtered_trades
            .iter()
            .map(|t| t.condition_id.as_str())
            .filter(|id| !have_end_date.contains(id))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        let gamma_end_dates = if missing_ids.is_empty() {
            std::collections::HashMap::new()
        } else {
            info!(address, count = missing_ids.len(), "Fetching end_dates from Gamma");
            match gamma.get_end_dates(&missing_ids).await {
                Ok(m) => m,
                Err(e) => {
                    warn!(address, error = %e, "Gamma fetch failed, proceeding without end_dates");
                    std::collections::HashMap::new()
                }
            }
        };

        info!(address, resolved = gamma_end_dates.len(), "Gamma end_dates resolved");

        // Enrich positions with Gamma end_dates.
        let existing_ids: HashSet<String> = filtered_positions
            .iter()
            .map(|p| p.condition_id.clone())
            .collect();

        let mut enriched_positions: Vec<_> = filtered_positions
            .into_iter()
            .map(|mut p| {
                if p.end_date.is_none() {
                    if let Some(ed) = gamma_end_dates.get(&p.condition_id) {
                        p.end_date = Some(ed.clone());
                    }
                }
                p
            })
            .collect();

        // Synthesise minimal Position stubs for condition_ids that have trades
        // but no position record, so timing factors can reference end_dates.
        for (cid, end_date) in &gamma_end_dates {
            if !existing_ids.contains(cid) {
                enriched_positions.push(Position {
                    proxy_wallet: address.clone(),
                    condition_id: cid.clone(),
                    size: 0.0,
                    avg_price: 0.0,
                    realized_pnl: 0.0,
                    total_bought: 0.0,
                    cash_pnl: 0.0,
                    end_date: Some(end_date.clone()),
                    title: None,
                });
            }
        }

        if let Some(ts) = first_activity_ts {
            info!(address, first_activity_ts = ts, "First activity timestamp");
        }

        let ws = score_wallet(address, &filtered_trades, &enriched_positions, first_activity_ts);

        info!(
            address,
            score = ws.score,
            flagged = ws.flagged,
            entry_timing = ws.entry_timing_score,
            entry_timing_weight = W_ENTRY_TIMING,
            concentration = ws.concentration_score,
            concentration_weight = W_CONCENTRATION,
            size = ws.size_score,
            size_weight = W_SIZE,
            wallet_age = ws.wallet_age_score,
            wallet_age_weight = W_WALLET_AGE,
            win_rate = ws.win_rate_score,
            win_rate_weight = W_WIN_RATE,
            markets_traded = ws.markets_traded,
            total_volume_usdc = ws.total_volume_usdc,
            "Score breakdown"
        );

        if let Err(e) = upsert_score(&pool, &ws).await {
            warn!(address, error = %e, "Failed to upsert score to DB");
        } else {
            info!(address, "Upserted to wallet_scores");
        }
    }

    Ok(())
}
