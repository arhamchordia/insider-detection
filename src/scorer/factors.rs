// src/scorer/factors.rs
//
// Pure factor computation functions. No I/O — all inputs are pre-fetched data.
// Each function returns a score in [0.0, 1.0]. Higher = more suspicious.

use crate::data_api::client::{Position, Trade};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use std::collections::HashMap;

// ── Factor weights ────────────────────────────────────────────────────────────

// Weights tuned against 7 known insider wallets (target: catch 60-70%).
// wallet_age dominates: new wallet + any supporting signal → flag.
// entry_timing now functional via Gamma API closedTime enrichment.
pub const W_ENTRY_TIMING: f64 = 0.15;
pub const W_CONCENTRATION: f64 = 0.25;
pub const W_SIZE: f64 = 0.10;
pub const W_WALLET_AGE: f64 = 0.45;
pub const W_WIN_RATE: f64 = 0.05;

pub const FLAG_THRESHOLD: f64 = 0.70;
/// Minimum total USDC volume for a wallet to be flagged.
/// Filters out dust/test wallets — real insider trades involve meaningful size.
pub const MIN_FLAG_VOLUME_USDC: f64 = 4_000.0;

// ── Factor 1: Entry Timing ────────────────────────────────────────────────────

/// Score based on how close the wallet's most suspicious trade was to a market's
/// end time. "Most suspicious" = closest to end_time across all positions.
///
/// Scoring tiers (hours before end_time):
///   < 1h   → 1.00
///   < 6h   → 0.85
///   < 24h  → 0.70
///   < 72h  → 0.40
///   ≥ 72h  → 0.05
///   no end_time available → 0.05
#[must_use]
pub fn entry_timing_score(trades: &[Trade], positions: &[Position]) -> f64 {
    if positions.is_empty() {
        return 0.05;
    }

    let now = Utc::now();
    let mut best: f64 = 0.0;

    for position in positions {
        let Some(end_ts) = parse_end_date(position.end_date.as_deref()) else {
            continue;
        };

        let market_trades = trades
            .iter()
            .filter(|t| t.condition_id == position.condition_id);

        for trade in market_trades {
            let trade_dt = DateTime::from_timestamp(trade.timestamp, 0).unwrap_or(now);
            #[allow(clippy::cast_precision_loss)]
            let hours_before = (end_ts - trade_dt).num_seconds() as f64 / 3600.0;
            let score = timing_tier(hours_before);
            if score > best {
                best = score;
            }
        }
    }

    // If we found no matching trades for any position, default to 0.05.
    if best == 0.0 { 0.05 } else { best }
}

#[must_use]
pub fn timing_tier(hours_before: f64) -> f64 {
    if hours_before <= 0.0 {
        // Post-resolution trade — no advance knowledge signal.
        return 0.0;
    }
    if hours_before < 1.0 {
        1.00
    } else if hours_before < 6.0 {
        0.85
    } else if hours_before < 24.0 {
        0.70
    } else if hours_before < 72.0 {
        0.40
    } else {
        0.05
    }
}

/// Parse end date from either `YYYY-MM-DD` or `YYYY-MM-DDTHH:MM:SSZ` format.
/// Returns UTC midnight for date-only strings; full datetime otherwise.
#[must_use]
pub fn parse_end_date(date_str: Option<&str>) -> Option<DateTime<Utc>> {
    let s = date_str?;

    // Try full datetime first (ISO 8601 / RFC 3339).
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }

    // Fall back to date-only "YYYY-MM-DD" → UTC midnight.
    if let Ok(naive) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        if let Some(naive_dt) = naive.and_hms_opt(0, 0, 0) {
            return Some(Utc.from_utc_datetime(&naive_dt));
        }
    }

    None
}

// ── Market category filter ────────────────────────────────────────────────────

/// Returns true if the market title suggests insider knowledge is possible.
/// Filters out crypto price prediction and sports markets where "insider
/// advantage" doesn't apply — anyone can have a view on ETH price or a
/// cricket match outcome.
#[must_use]
pub fn is_insider_susceptible(title: Option<&str>) -> bool {
    let title = match title {
        Some(t) => t.to_lowercase(),
        None => return true, // unknown → include conservatively
    };

    // Crypto & commodity price prediction — public information, not insider susceptible
    let crypto_price = [
        "price of", "above $", "below $", "will bitcoin", "will ethereum",
        "bitcoin up", "ethereum up", "xrp up", "megaeth",
        "will eth ", "will btc", "will sol ", "will bnb ", "will xrp",
        "will doge", "will ada ", "will avax", "bitcoin price", "ethereum price",
        "crypto price", "altcoin", "will the price",
        // "Up or Down" crypto micro-bet markets (15-min windows)
        "up or down",
        // Crypto price target/dip patterns not covered by "above $"
        "dip to $", "reach $", "price of ethereum", "price of solana", "price of xrp",
        // Commodity prices
        "will gold ", "will silver ", "will oil ", "will crude",
        "gold close", "silver close", "oil close",
        "gold at $", "silver at $", "oil at $",
        "price of gold", "price of silver", "price of oil",
        // Stock / equity markets — not politically insider-susceptible
        "beat quarterly earnings", "beat earnings", "most valuable company",
        "close above $", "close below $",
        // Crypto asset milestones/records not caught by price patterns above
        "dogecoin", "all time high", "unban bitcoin", "ban bitcoin",
        "china ban", "china unban", "bitcoin etf",
    ];
    if crypto_price.iter().any(|p| title.contains(p)) {
        return false;
    }

    // Social media / content creator markets — not insider susceptible
    let social_media = [
        "million views", "million subscribers", "youtube", "twitch",
        "next video", "get views", "will reach", "pewdiepie", "mrbeast",
        "tiktok views", "instagram followers", "twitter followers",
        "will go viral",
        // Social media activity counts (tweet counts etc.)
        "tweets from ", "post 200-", "post 100-",
    ];
    if social_media.iter().any(|p| title.contains(p)) {
        return false;
    }

    // Entertainment / box office — not insider susceptible
    let entertainment = [
        "top grossing movie", "top-grossing movie", "grossing film",
        "box office", "opening weekend",
    ];
    if entertainment.iter().any(|p| title.contains(p)) {
        return false;
    }

    // Sports outcomes — not insider susceptible
    let sports = [
        "who wins the match", "who will win the match", "who will win the game",
        "will win the series", "t20", "cricket", " nba ", " nfl ", " nhl ", " mlb ",
        "premier league", "champions league", "la liga", "bundesliga", "serie a",
        "world cup", "super bowl", "ufc ", "boxing match", "tennis",
        "golf tournament", "formula 1", "f1 race", "most sixes", "who wins the toss",
        "completed match", "poker championship", "poker tournament", "heads-up poker",
        "wsop", "world series of poker", "esports", "league of legends", "dota",
        "will win the championship", "will win the tournament", "will win the race",
        "will win the game", "nascar", "wimbledon", "the masters", "stanley cup",
        // Missing sports from previous analysis
        "world series", "nba finals", "lead the nba", "drivers champion", "drivers' champion",
        // Single-match date-specific patterns ("Will [Team] win on 2025-MM-DD?")
        "win on 2025-", "win on 2026-",
        // Esports prefixes not caught by "league of legends" / "dota" / "esports"
        "lol:", "counter-strike:", "cs2:",
        // Team matchup patterns — covers any "Team A vs. Team B" or "Team A vs Team B"
        " vs. ", " vs ",
        // NBA team names (individual matchups not caught by " nba ")
        "lakers", "celtics", "heat", "bulls", "knicks", "warriors", "nets", "bucks",
        "suns", "nuggets", "76ers", "sixers", "clippers", "raptors", "mavericks",
        "rockets", "thunder", "spurs", "pelicans", "grizzlies", "jazz", "timberwolves",
        "blazers", "kings", "hornets", "hawks", "wizards", "magic", "pistons",
        "cavaliers", "pacers",
        // NHL team names
        "penguins", "bruins", "maple leafs", "canadiens", "rangers", "blackhawks",
        "red wings", "flyers", "capitals", "hurricanes", "lightning", "panthers",
        "avalanche", "oilers", "flames", "canucks", "sharks", "ducks", "kings",
        "blues", "predators", "wild", "jets", "coyotes", "senators", "sabres",
        "islanders", "devils", "blue jackets",
        // NFL team names
        "patriots", "eagles", "49ers", "packers", "cowboys", "steelers", "ravens",
        "chiefs", "bengals", "broncos", "chargers", "raiders", "dolphins",
        "bills", "jets", "titans", "colts", "texans", "jaguars", "bears",
        "lions", "vikings", "falcons", "saints", "buccaneers", "seahawks",
        "cardinals", "rams", "giants", "commanders", "browns",
        // MLB team names
        "yankees", "dodgers", "red sox", "cubs", "mets", "braves", "astros",
        "cardinals", "giants", "phillies", "nationals", "padres", "brewers",
        "tigers", "mariners", "athletics", "orioles", "white sox", "royals",
        "twins", "guardians", "pirates", "reds", "rockies", "diamondbacks",
        "marlins", "rays",
    ];
    if sports.iter().any(|p| title.contains(p)) {
        return false;
    }

    true
}

// ── Factor 2: Trade Concentration ────────────────────────────────────────────

/// max_trades_in_single_market / total_trades.
/// Returns 0.0 when there are no trades.
#[must_use]
pub fn concentration_score(trades: &[Trade]) -> f64 {
    if trades.is_empty() {
        return 0.0;
    }

    let mut counts: HashMap<&str, usize> = HashMap::new();
    for trade in trades {
        *counts.entry(trade.condition_id.as_str()).or_insert(0) += 1;
    }

    let max = counts.values().copied().max().unwrap_or(0);
    #[allow(clippy::cast_precision_loss)]
    let result = max as f64 / trades.len() as f64;
    result
}

// ── Factor 3: Size ────────────────────────────────────────────────────────────

/// Total USDC volume across all trades, mapped to tiers.
#[must_use]
pub fn size_score(trades: &[Trade]) -> f64 {
    let total: f64 = trades.iter().map(|t| t.size * t.price).sum();
    volume_tier(total)
}

#[must_use]
pub fn volume_tier(usdc: f64) -> f64 {
    if usdc > 100_000.0 {
        1.0
    } else if usdc > 50_000.0 {
        0.8
    } else if usdc > 10_000.0 {
        0.6
    } else if usdc > 1_000.0 {
        0.4
    } else if usdc > 100.0 {
        0.2
    } else {
        0.05
    }
}

// ── Factor 4: Wallet Age ──────────────────────────────────────────────────────

/// Time between first-ever activity and the wallet's earliest trade in
/// insider-susceptible markets. Short gap = wallet was created specifically
/// to trade in these markets.
///
/// Uses earliest trade, not the trade closest to resolution — that's
/// entry_timing's job. wallet_age answers: "was this wallet brand new when
/// it first entered political markets?"
///
/// Tiers (days between first activity and earliest insider trade):
///   < 1   → 1.0
///   < 7   → 0.8
///   < 30  → 0.5
///   < 90  → 0.2
///   ≥ 90  → 0.05
#[must_use]
pub fn wallet_age_score(
    trades: &[Trade],
    _positions: &[Position],
    first_activity_ts: Option<i64>,
) -> f64 {
    let Some(first_ts) = first_activity_ts else {
        return 0.05;
    };
    let Some(first_dt) = DateTime::from_timestamp(first_ts, 0) else {
        return 0.05;
    };

    // Earliest trade in the (already-filtered) insider markets.
    let Some(earliest_ts) = trades.iter().map(|t| t.timestamp).min() else {
        return 0.05;
    };
    let Some(earliest_dt) = DateTime::from_timestamp(earliest_ts, 0) else {
        return 0.05;
    };

    #[allow(clippy::cast_precision_loss)]
    let gap_days = (earliest_dt - first_dt).num_seconds() as f64 / 86400.0;
    age_tier(gap_days.max(0.0))
}

#[must_use]
pub fn age_tier(days: f64) -> f64 {
    if days < 1.0 {
        1.0
    } else if days < 7.0 {
        0.8
    } else if days < 30.0 {
        0.5
    } else if days < 90.0 {
        0.2
    } else {
        0.05
    }
}

// ── Factor 5: Win Rate ────────────────────────────────────────────────────────

/// wins / closed_positions where closed = `endDate` in the past and `totalBought` > 0.
/// Returns 0.05 if there are no qualifying positions.
#[must_use]
pub fn win_rate_score(positions: &[Position]) -> f64 {
    let now = Utc::now();
    let mut total = 0usize;
    let mut wins = 0usize;

    for pos in positions {
        if pos.total_bought <= 0.0 {
            continue;
        }
        let Some(end_ts) = parse_end_date(pos.end_date.as_deref()) else {
            continue;
        };
        if end_ts > now {
            continue; // Market not yet ended.
        }
        total += 1;
        if pos.realized_pnl > 0.0 {
            wins += 1;
        }
    }

    if total == 0 {
        0.05
    } else {
        #[allow(clippy::cast_precision_loss)]
        let rate = wins as f64 / total as f64;
        rate
    }
}

// ── Composite score ───────────────────────────────────────────────────────────

pub struct Factors {
    pub entry_timing: f64,
    pub concentration: f64,
    pub size: f64,
    pub wallet_age: f64,
    pub win_rate: f64,
}

impl Factors {
    #[must_use]
    pub fn composite(&self) -> f64 {
        W_ENTRY_TIMING * self.entry_timing
            + W_CONCENTRATION * self.concentration
            + W_SIZE * self.size
            + W_WALLET_AGE * self.wallet_age
            + W_WIN_RATE * self.win_rate
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timing_tiers_are_correct() {
        assert_eq!(timing_tier(0.5), 1.0);
        assert_eq!(timing_tier(3.0), 0.85);
        assert_eq!(timing_tier(12.0), 0.70);
        assert_eq!(timing_tier(48.0), 0.40);
        assert_eq!(timing_tier(100.0), 0.05);
        assert_eq!(timing_tier(-1.0), 0.0);
    }

    #[test]
    fn volume_tiers_are_correct() {
        assert_eq!(volume_tier(150_000.0), 1.0);
        assert_eq!(volume_tier(75_000.0), 0.8);
        assert_eq!(volume_tier(15_000.0), 0.6);
        assert_eq!(volume_tier(1_500.0), 0.4);
        assert_eq!(volume_tier(150.0), 0.2);
        assert_eq!(volume_tier(50.0), 0.05);
    }

    #[test]
    fn concentration_all_one_market() {
        let trades = vec![
            Trade {
                proxy_wallet: "a".into(),
                condition_id: "mkt1".into(),
                side: "BUY".into(),
                price: 0.5,
                size: 10.0,
                timestamp: 0,
                title: None,
            },
            Trade {
                proxy_wallet: "a".into(),
                condition_id: "mkt1".into(),
                side: "BUY".into(),
                price: 0.6,
                size: 20.0,
                timestamp: 1,
                title: None,
            },
        ];
        assert_eq!(concentration_score(&trades), 1.0);
    }

    #[test]
    fn composite_weights_sum_to_one() {
        let sum = W_ENTRY_TIMING + W_CONCENTRATION + W_SIZE + W_WALLET_AGE + W_WIN_RATE;
        assert!((sum - 1.0).abs() < 1e-10);
    }

    #[test]
    fn age_tiers_are_correct() {
        assert_eq!(age_tier(0.5), 1.0);
        assert_eq!(age_tier(3.0), 0.8);
        assert_eq!(age_tier(15.0), 0.5);
        assert_eq!(age_tier(60.0), 0.2);
        assert_eq!(age_tier(120.0), 0.05);
    }

    #[test]
    fn parse_end_date_handles_both_formats() {
        assert!(parse_end_date(Some("2024-11-05")).is_some());
        assert!(parse_end_date(Some("2024-11-05T22:00:00Z")).is_some());
        assert!(parse_end_date(None).is_none());
        assert!(parse_end_date(Some("not-a-date")).is_none());
    }

    // ── is_insider_susceptible filter tests ───────────────────────────────────

    #[test]
    fn crypto_up_or_down_markets_are_excluded() {
        // Exact title from the live API as reported.
        assert!(!is_insider_susceptible(Some(
            "Ethereum Up or Down - February 25, 11-11:15PM ET"
        )));
        assert!(!is_insider_susceptible(Some(
            "XRP Up or Down - March 1, 9-9:15AM ET"
        )));
        // Case variants.
        assert!(!is_insider_susceptible(Some("ethereum up or down - march 5")));
        assert!(!is_insider_susceptible(Some("BTC UP OR DOWN - 12PM")));
    }

    #[test]
    fn none_title_is_conservatively_included() {
        // None titles are included because we can't rule them out.
        // This is intentional — see condition_id exclusion fix in model.rs.
        assert!(is_insider_susceptible(None));
    }

    #[test]
    fn political_titles_are_included() {
        assert!(is_insider_susceptible(Some("Who will win the 2024 US election?")));
        assert!(is_insider_susceptible(Some("Will Trump pardon CZ before March?")));
        assert!(is_insider_susceptible(Some("Will the Fed cut rates in May?")));
        assert!(is_insider_susceptible(Some("Will NATO expand in 2025?")));
    }

    #[test]
    fn sports_titles_are_excluded() {
        assert!(!is_insider_susceptible(Some("NBA Finals Game 7 winner")));
        assert!(!is_insider_susceptible(Some("World Series 2025 champion")));
        assert!(!is_insider_susceptible(Some("UFC 300 main event winner")));
        // Team matchup titles
        assert!(!is_insider_susceptible(Some("Lakers vs. Heat")));
        assert!(!is_insider_susceptible(Some("Penguins vs. Hurricanes")));
        assert!(!is_insider_susceptible(Some("Heat vs. Hornets")));
        assert!(!is_insider_susceptible(Some("Pistons vs. Wizards")));
        assert!(!is_insider_susceptible(Some("Bulls vs. Celtics")));
    }

    #[test]
    fn crypto_price_titles_are_excluded() {
        assert!(!is_insider_susceptible(Some("Will Bitcoin be above $100k?")));
        assert!(!is_insider_susceptible(Some("Ethereum price above $5000?")));
        assert!(!is_insider_susceptible(Some("Will XRP reach $3?")));
    }
}
