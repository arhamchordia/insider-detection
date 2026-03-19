// src/scorer/model.rs
//
// Scoring pipeline:
//   wallet list → data_api (parallel fetch per wallet) → gamma (end_date enrichment)
//   → factors → DB upsert
//
// score_wallet() is pure computation (no I/O).
// score_all() drives the full pipeline with batch concurrency.

use crate::data_api::client::{DataApiClient, Position, Trade};
use crate::gamma::client::GammaClient;
use crate::scorer::factors::{
    concentration_score, entry_timing_score, is_insider_susceptible, size_score,
    wallet_age_score, win_rate_score, Factors, FLAG_THRESHOLD, MIN_FLAG_VOLUME_USDC,
};
use anyhow::{Context, Result};
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::collections::HashSet;
use tracing::{error, info, warn};

const BATCH_SIZE: usize = 100;

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct WalletScore {
    pub address: String,
    pub score: f64,
    pub entry_timing_score: f64,
    pub concentration_score: f64,
    pub size_score: f64,
    pub wallet_age_score: f64,
    pub win_rate_score: f64,
    pub total_volume_usdc: f64,
    pub markets_traded: i32,
    pub flagged: bool,
}

// ── Pure computation ──────────────────────────────────────────────────────────

/// Compute a full score from pre-fetched API data. No I/O.
/// Positions must already have `end_date` enriched from Gamma before calling this.
#[must_use]
pub fn score_wallet(
    address: &str,
    trades: &[Trade],
    positions: &[Position],
    first_activity_ts: Option<i64>,
) -> WalletScore {
    let entry_timing = entry_timing_score(trades, positions);
    let concentration = concentration_score(trades);
    let size = size_score(trades);
    let wallet_age = wallet_age_score(trades, positions, first_activity_ts);
    let win_rate = win_rate_score(positions);

    let factors = Factors {
        entry_timing,
        concentration,
        size,
        wallet_age,
        win_rate,
    };

    let score = factors.composite();
    let total_volume_usdc: f64 = trades.iter().map(|t| t.size * t.price).sum();
    // Hard disqualification: a wallet that lost every closed trade (win_rate == 0.0
    // exactly, meaning it had closed positions but zero wins) is not an insider.
    // Insiders profit; consistent losers are noise.
    let flagged = score >= FLAG_THRESHOLD
        && total_volume_usdc >= MIN_FLAG_VOLUME_USDC
        && win_rate != 0.0;

    let market_set: HashSet<&str> = trades.iter().map(|t| t.condition_id.as_str()).collect();
    // Market counts fit comfortably in i32 (Polymarket has ~20k markets total).
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    let markets_traded = market_set.len() as i32;

    WalletScore {
        address: address.to_string(),
        score,
        entry_timing_score: entry_timing,
        concentration_score: concentration,
        size_score: size,
        wallet_age_score: wallet_age,
        win_rate_score: win_rate,
        total_volume_usdc,
        markets_traded,
        flagged,
    }
}

// ── Pipeline ──────────────────────────────────────────────────────────────────

/// Full scoring pipeline over a pre-enumerated wallet list.
/// Processes in batches of `BATCH_SIZE` with concurrent fetches per batch.
/// Rate limiting is handled by the semaphore inside `DataApiClient`.
/// Logs progress every 100 wallets.
///
/// # Errors
/// Returns an error only if a DB upsert fails fatally (individual wallet fetch
/// failures are logged and skipped).
#[tracing::instrument(skip(pool, data_api, wallets), fields(total = wallets.len()))]
pub async fn score_all(
    pool: &PgPool,
    data_api: &DataApiClient,
    wallets: &[String],
) -> Result<()> {
    let total = wallets.len();
    let mut processed = 0usize;
    let mut flagged_count = 0usize;

    for chunk in wallets.chunks(BATCH_SIZE) {
        // Launch all wallets in the chunk concurrently.
        let mut handles = Vec::with_capacity(chunk.len());
        for address in chunk {
            let address = address.clone();
            // We can't move data_api/gamma into the future, so we use local async blocks
            // that capture references — safe because score_all holds both for its entire
            // duration and all futures complete before we advance.
            handles.push(score_one_wallet(address, data_api, None));
        }

        // Await all concurrently using futures::future::join_all.
        let results = futures::future::join_all(handles).await;

        for result in results {
            match result {
                Ok(ws) => {
                    if ws.flagged {
                        flagged_count += 1;
                    }
                    if let Err(e) = upsert_score(pool, &ws).await {
                        error!(address = %ws.address, error = %e, "Failed to upsert wallet score");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to score wallet, skipping");
                }
            }

            processed += 1;
            if processed.is_multiple_of(100) {
                info!(
                    processed,
                    total,
                    flagged = flagged_count,
                    "Scoring progress"
                );
            }
        }
    }

    info!(processed, flagged = flagged_count, "Scoring complete");
    Ok(())
}

#[tracing::instrument(skip(data_api, gamma))]
pub async fn score_one_wallet(
    address: String,
    data_api: &DataApiClient,
    gamma: Option<&GammaClient>,
) -> Result<WalletScore> {
    let (trades_result, positions_result, activity_result) = tokio::join!(
        data_api.get_trades(&address),
        data_api.get_positions(&address),
        data_api.get_first_activity(&address),
    );

    let trades = trades_result
        .with_context(|| format!("Failed to fetch trades for {address}"))?;
    let positions = positions_result
        .with_context(|| format!("Failed to fetch positions for {address}"))?;
    let first_activity_ts = activity_result
        .with_context(|| format!("Failed to fetch activity for {address}"))?;

    // Filter to insider-susceptible markets only (excludes crypto price, sports).
    //
    // Two-pass approach to handle trades where title=None:
    // 1. Collect condition_ids that are *explicitly* excluded — any trade with a
    //    known non-susceptible title (e.g. "Ethereum Up or Down") marks the whole
    //    condition_id as excluded, even if other trades for that market have title=None.
    // 2. Susceptible set = all condition_ids NOT in the exclusion set.
    //    Trades with title=None are conservatively included unless their condition_id
    //    was already excluded by a trade with a known non-susceptible title.
    let excluded_condition_ids: HashSet<&str> = trades
        .iter()
        .filter(|t| t.title.is_some() && !is_insider_susceptible(t.title.as_deref()))
        .map(|t| t.condition_id.as_str())
        .collect();

    let susceptible_condition_ids: HashSet<&str> = trades
        .iter()
        .map(|t| t.condition_id.as_str())
        .filter(|id| !excluded_condition_ids.contains(id))
        .collect();

    let filtered_trades: Vec<Trade> = trades
        .iter()
        .filter(|t| susceptible_condition_ids.contains(t.condition_id.as_str()))
        .cloned()
        .collect();
    let filtered_positions: Vec<Position> = positions
        .iter()
        .filter(|p| susceptible_condition_ids.contains(p.condition_id.as_str()))
        .cloned()
        .collect();

    // Collect all unique condition_ids that are missing end_date in positions.
    // We also include condition_ids from trades that have no matching position at all,
    // so wallet_age_score can cross-reference them once positions are synthesised.
    let missing_end_date_ids: Vec<&str> = {
        let have_end_date: HashSet<&str> = filtered_positions
            .iter()
            .filter(|p| p.end_date.is_some())
            .map(|p| p.condition_id.as_str())
            .collect();

        filtered_trades
            .iter()
            .map(|t| t.condition_id.as_str())
            .filter(|id| !have_end_date.contains(id))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    };

    // Fetch end dates from Gamma for any condition_id that needs one.
    // Gamma is optional — bulk scorer passes None to avoid rate-limiting 259k wallets.
    // score-wallet CLI passes Some(&gamma) for accurate entry_timing on single wallets.
    let gamma_end_dates = if missing_end_date_ids.is_empty() {
        std::collections::HashMap::new()
    } else if let Some(gamma) = gamma {
        gamma
            .get_end_dates(&missing_end_date_ids)
            .await
            .with_context(|| format!("Gamma fetch failed for {address}"))?
    } else {
        std::collections::HashMap::new()
    };

    // Augment positions: fill in end_date from Gamma where the Data API left it null.
    let enriched_positions: Vec<Position> = {
        // Collect owned condition_ids before consuming filtered_positions.
        let existing_ids: HashSet<String> = filtered_positions
            .iter()
            .map(|p| p.condition_id.clone())
            .collect();

        let mut out: Vec<Position> = filtered_positions
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

        // For condition_ids that have trades but no position at all, synthesise a
        // minimal Position so that entry_timing_score and wallet_age_score can
        // cross-reference the end_date against trade timestamps.
        for (cid, end_date) in &gamma_end_dates {
            if !existing_ids.contains(cid) {
                out.push(Position {
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

        out
    };

    Ok(score_wallet(
        &address,
        &filtered_trades,
        &enriched_positions,
        first_activity_ts,
    ))
}

// ── DB persistence ────────────────────────────────────────────────────────────

/// Upsert a wallet score into the `wallet_scores` table.
/// The `flagged` column is sticky: once `true` it is never reset to `false`.
///
/// # Errors
/// Returns an error if the database query fails.
fn f64_to_decimal(value: f64, field: &str) -> Decimal {
    match Decimal::try_from(value) {
        Ok(d) => d,
        Err(e) => {
            warn!(field, value, error = %e, "Non-finite f64 in score field, substituting 0");
            Decimal::ZERO
        }
    }
}

pub async fn upsert_score(pool: &PgPool, s: &WalletScore) -> Result<()> {
    let score = f64_to_decimal(s.score, "score");
    let entry_timing = f64_to_decimal(s.entry_timing_score, "entry_timing_score");
    let concentration = f64_to_decimal(s.concentration_score, "concentration_score");
    let size = f64_to_decimal(s.size_score, "size_score");
    let wallet_age = f64_to_decimal(s.wallet_age_score, "wallet_age_score");
    let win_rate = f64_to_decimal(s.win_rate_score, "win_rate_score");
    let volume = f64_to_decimal(s.total_volume_usdc, "total_volume_usdc");

    sqlx::query!(
        r#"
        INSERT INTO wallet_scores (
            address,
            score,
            entry_timing_score,
            concentration_score,
            size_score,
            wallet_age_score,
            win_rate_score,
            total_volume_usdc,
            markets_traded,
            flagged,
            scored_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NOW())
        ON CONFLICT (address) DO UPDATE SET
            score               = EXCLUDED.score,
            entry_timing_score  = EXCLUDED.entry_timing_score,
            concentration_score = EXCLUDED.concentration_score,
            size_score          = EXCLUDED.size_score,
            wallet_age_score    = EXCLUDED.wallet_age_score,
            win_rate_score      = EXCLUDED.win_rate_score,
            total_volume_usdc   = EXCLUDED.total_volume_usdc,
            markets_traded      = EXCLUDED.markets_traded,
            flagged             = wallet_scores.flagged OR EXCLUDED.flagged,
            scored_at           = NOW()
        "#,
        s.address,
        score,
        entry_timing,
        concentration,
        size,
        wallet_age,
        win_rate,
        volume,
        s.markets_traded,
        s.flagged,
    )
    .execute(pool)
    .await
    .with_context(|| format!("Failed to upsert score for {}", s.address))?;

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_api::client::{Position, Trade};

    fn make_trade(condition_id: &str, title: Option<&str>, timestamp: i64) -> Trade {
        Trade {
            proxy_wallet: "0xtest".into(),
            condition_id: condition_id.into(),
            side: "BUY".into(),
            price: 0.8,
            size: 10_000.0,
            timestamp,
            title: title.map(String::from),
        }
    }

    fn make_position(condition_id: &str, end_date: Option<&str>, pnl: f64) -> Position {
        Position {
            proxy_wallet: "0xtest".into(),
            condition_id: condition_id.into(),
            size: 100.0,
            avg_price: 0.8,
            realized_pnl: pnl,
            total_bought: 1000.0,
            cash_pnl: pnl,
            end_date: end_date.map(String::from),
            title: None,
        }
    }

    // ── score_wallet: flagging logic ──────────────────────────────────────────

    /// A brand-new wallet (age < 1 day), concentrated in one market, large volume,
    /// and high win rate must be flagged when composite >= FLAG_THRESHOLD.
    #[test]
    fn score_wallet_flags_high_risk_profile() {
        let now_ts = chrono::Utc::now().timestamp();
        let yesterday = chrono::Utc::now() - chrono::Duration::hours(25);
        let end_date = yesterday.format("%Y-%m-%dT%H:%M:%SZ").to_string();

        let trades = vec![make_trade("mkt1", Some("Will Trump pardon CZ?"), now_ts - 3600)];
        let positions = vec![make_position("mkt1", Some(&end_date), 5000.0)];
        // First activity at same time as first trade → age < 1 day.
        let first_activity_ts = Some(now_ts - 3600);

        let ws = score_wallet("0xtest", &trades, &positions, first_activity_ts);

        // wallet_age=1.0, concentration=1.0, size=0.6 (>$8k), win_rate=1.0, timing≥0.05.
        // Min composite: 0.45+0.25+0.06+0.05+0.0075 = 0.8175 ≥ FLAG_THRESHOLD(0.70).
        assert!(ws.score >= crate::scorer::factors::FLAG_THRESHOLD);
        assert!(ws.flagged);
        assert_eq!(ws.markets_traded, 1);
        assert!((ws.total_volume_usdc - 8000.0).abs() < 0.01); // 0.8 * 10_000
    }

    /// A wallet with no trades has a very low composite score and must not be flagged.
    /// With empty inputs: concentration=0.0, all other factors return their 0.05 floor.
    /// Composite = 0.15*0.05 + 0.25*0.0 + 0.10*0.05 + 0.45*0.05 + 0.05*0.05 = 0.0375.
    #[test]
    fn score_wallet_empty_trades_not_flagged() {
        use crate::scorer::factors::FLAG_THRESHOLD;

        let ws = score_wallet("0xempty", &[], &[], None);
        assert!(ws.score < FLAG_THRESHOLD, "empty wallet must be below flag threshold");
        assert!(!ws.flagged);
        assert_eq!(ws.markets_traded, 0);
        assert_eq!(ws.total_volume_usdc, 0.0);
    }

    /// A wallet that lost every position (win_rate == 0.0 exactly) must not be
    /// flagged even if its composite score exceeds FLAG_THRESHOLD.
    #[test]
    fn score_wallet_zero_win_rate_never_flagged() {
        let now_ts = chrono::Utc::now().timestamp();
        let yesterday = chrono::Utc::now() - chrono::Duration::hours(25);
        let end_date = yesterday.format("%Y-%m-%dT%H:%M:%SZ").to_string();

        let trades = vec![make_trade("mkt1", Some("Will Trump pardon CZ?"), now_ts - 100)];
        // realized_pnl = -1000 → zero wins out of 1 closed position → win_rate exactly 0.0.
        let positions = vec![make_position("mkt1", Some(&end_date), -1000.0)];

        let ws = score_wallet("0xloser", &trades, &positions, Some(now_ts - 100));
        assert!(!ws.flagged, "zero win_rate should suppress flagging");
    }

    /// Volume below MIN_FLAG_VOLUME_USDC must not be flagged even with near-max score.
    #[test]
    fn score_wallet_low_volume_not_flagged() {
        let now_ts = chrono::Utc::now().timestamp();
        let yesterday = chrono::Utc::now() - chrono::Duration::hours(25);
        let end_date = yesterday.format("%Y-%m-%dT%H:%M:%SZ").to_string();

        let trade = Trade {
            proxy_wallet: "0xdust".into(),
            condition_id: "mkt1".into(),
            side: "BUY".into(),
            price: 0.9,
            size: 1.0, // 0.9 USDC — well below 4_000 threshold.
            timestamp: now_ts - 100,
            title: Some("Will Trump pardon CZ?".into()),
        };
        let position = make_position("mkt1", Some(&end_date), 0.5);

        let ws = score_wallet("0xdust", &[trade], &[position], Some(now_ts - 100));
        assert!(!ws.flagged, "dust volume should suppress flagging");
    }

    // ── Two-pass filter logic ─────────────────────────────────────────────────

    /// A condition_id that has *any* trade with a known non-susceptible title must
    /// exclude ALL trades for that condition_id — including those with title=None.
    #[test]
    fn two_pass_filter_excludes_condition_id_when_any_title_non_susceptible() {
        use crate::scorer::factors::is_insider_susceptible;
        use std::collections::HashSet;

        let trades = vec![
            // title=None for crypto market — would be included alone.
            Trade {
                proxy_wallet: "0xtest".into(),
                condition_id: "crypto_mkt".into(),
                side: "BUY".into(),
                price: 0.5,
                size: 100.0,
                timestamp: 1_000,
                title: None,
            },
            // Same condition_id, explicit non-susceptible title — marks whole id excluded.
            Trade {
                proxy_wallet: "0xtest".into(),
                condition_id: "crypto_mkt".into(),
                side: "BUY".into(),
                price: 0.5,
                size: 100.0,
                timestamp: 2_000,
                title: Some("Ethereum Up or Down - March 1".into()),
            },
            // Different condition_id, susceptible market — must survive.
            Trade {
                proxy_wallet: "0xtest".into(),
                condition_id: "political_mkt".into(),
                side: "BUY".into(),
                price: 0.9,
                size: 50.0,
                timestamp: 3_000,
                title: Some("Will Trump pardon CZ?".into()),
            },
        ];

        let excluded_condition_ids: HashSet<&str> = trades
            .iter()
            .filter(|t| t.title.is_some() && !is_insider_susceptible(t.title.as_deref()))
            .map(|t| t.condition_id.as_str())
            .collect();

        let susceptible_condition_ids: HashSet<&str> = trades
            .iter()
            .map(|t| t.condition_id.as_str())
            .filter(|id| !excluded_condition_ids.contains(id))
            .collect();

        let filtered: Vec<&Trade> = trades
            .iter()
            .filter(|t| susceptible_condition_ids.contains(t.condition_id.as_str()))
            .collect();

        assert!(
            filtered.iter().all(|t| t.condition_id != "crypto_mkt"),
            "crypto_mkt must be excluded because one trade has a non-susceptible title"
        );
        assert!(
            filtered.iter().any(|t| t.condition_id == "political_mkt"),
            "political_mkt must pass through"
        );
        assert_eq!(filtered.len(), 1);
    }

    /// A trade with title=None where no other trade in the same condition_id has a
    /// non-susceptible title must be conservatively included.
    #[test]
    fn two_pass_filter_includes_none_title_when_no_exclusion_signal() {
        use crate::scorer::factors::is_insider_susceptible;
        use std::collections::HashSet;

        let trades = vec![Trade {
            proxy_wallet: "0xtest".into(),
            condition_id: "unknown_mkt".into(),
            side: "BUY".into(),
            price: 0.7,
            size: 200.0,
            timestamp: 1_000,
            title: None,
        }];

        let excluded_condition_ids: HashSet<&str> = trades
            .iter()
            .filter(|t| t.title.is_some() && !is_insider_susceptible(t.title.as_deref()))
            .map(|t| t.condition_id.as_str())
            .collect();

        let susceptible_condition_ids: HashSet<&str> = trades
            .iter()
            .map(|t| t.condition_id.as_str())
            .filter(|id| !excluded_condition_ids.contains(id))
            .collect();

        assert!(
            susceptible_condition_ids.contains("unknown_mkt"),
            "unknown market with title=None should be conservatively included"
        );
    }

    // ── Composite arithmetic ──────────────────────────────────────────────────

    /// Verifies score_wallet assembles Factors correctly: expected weighted sum
    /// matches actual composite score with all factors at minimum tier (0.05),
    /// except concentration which is 1.0 (single market).
    #[test]
    fn score_wallet_composite_matches_manual_calculation() {
        use crate::scorer::factors::{
            W_CONCENTRATION, W_ENTRY_TIMING, W_SIZE, W_WALLET_AGE, W_WIN_RATE,
        };

        let first_ts = 0i64;            // epoch
        let trade_ts = 100 * 86400i64;  // 100 days later → age_tier 0.05

        let trades = vec![Trade {
            proxy_wallet: "0xold".into(),
            condition_id: "mkt1".into(),
            side: "BUY".into(),
            price: 0.5,
            size: 1.0, // 0.5 USDC → volume_tier 0.05
            timestamp: trade_ts,
            title: Some("Will Trump pardon CZ?".into()),
        }];

        let ws = score_wallet("0xold", &trades, &[], Some(first_ts));

        // entry_timing = 0.05 (no positions), concentration = 1.0 (one market),
        // size = 0.05 (< $1), wallet_age = 0.05 (≥90 days), win_rate = 0.05 (no positions).
        let expected = W_ENTRY_TIMING * 0.05
            + W_CONCENTRATION * 1.0
            + W_SIZE * 0.05
            + W_WALLET_AGE * 0.05
            + W_WIN_RATE * 0.05;

        assert!(
            (ws.score - expected).abs() < 1e-9,
            "Expected {expected:.6}, got {:.6}",
            ws.score
        );
    }
}
