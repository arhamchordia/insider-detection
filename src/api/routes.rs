// src/api/routes.rs
//
// Axum 0.8 router + all HTTP handlers for the insider detection API.
//
// All routes are mounted under /api:
//   GET  /api/health
//   GET  /api/stats
//   GET  /api/wallets?flagged_only=true&min_score=0&limit=100&offset=0
//   GET  /api/wallets/:address
//   GET  /api/wallets/:address/trades    (proxies Polymarket Data API)
//   GET  /api/known-insiders

use anyhow::{Context, Result};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;
use tracing::instrument;

use crate::data_api::client::DataApiClient;
use crate::gamma::client::GammaClient;
use crate::scorer::model::{score_one_wallet, upsert_score};

// ── Shared state ──────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct InnerState {
    pub pool: PgPool,
    pub http_client: reqwest::Client,
    pub polymarket_api_key: String,
    pub data_api: Arc<DataApiClient>,
    pub gamma: Arc<GammaClient>,
}

pub type AppState = Arc<InnerState>;

// ── Response types ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub scored_wallets: i64,
    pub flagged: i64,
}

#[derive(Debug, Serialize)]
pub struct StatsResponse {
    pub total_wallets: i64,
    pub flagged_wallets: i64,
    pub total_volume_usdc: f64,
    pub known_insiders_scored: i64,
    pub total_known_insiders: i64,
}

#[derive(Debug, Serialize)]
pub struct WalletRow {
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
    pub first_activity_ts: Option<DateTime<Utc>>,
    pub scored_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct WalletDetail {
    #[serde(flatten)]
    pub row: WalletRow,
    pub known_label: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TradeRow {
    pub condition_id: String,
    pub title: Option<String>,
    pub side: String,
    pub price: f64,
    pub size: f64,
    pub usdc_amount: f64,
    pub block_time: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct KnownInsiderResponse {
    pub address: String,
    pub label: String,
    pub market: String,
    pub source: Option<String>,
    pub score: Option<f64>,
    pub flagged: Option<bool>,
    pub scored_at: Option<DateTime<Utc>>,
}

// ── Query params ──────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct WalletsQuery {
    #[serde(default)]
    pub flagged_only: bool,
    #[serde(default)]
    pub min_score: f64,
    #[serde(default = "default_min_volume")]
    pub min_volume_usdc: f64,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    100
}

fn default_min_volume() -> f64 {
    1000.0
}

// ── Router ────────────────────────────────────────────────────────────────────

pub fn build_router(pool: PgPool) -> Result<Router> {
    let polymarket_api_key = std::env::var("POLYMARKET_API_KEY")
        .context("POLYMARKET_API_KEY not set")?;

    let state: AppState = Arc::new(InnerState {
        pool,
        // Single shared client — reuses connections via keep-alive.
        http_client: reqwest::Client::new(),
        polymarket_api_key,
        // Shared across all requests: DataApiClient has a rate-limit semaphore,
        // GammaClient has an in-memory cache of market end dates.
        data_api: Arc::new(DataApiClient::new().context("Failed to initialise Data API client")?),
        gamma: Arc::new(GammaClient::new()),
    });

    // CORS is intentionally permissive (allow any origin) for local/demo deployment.
    // In a production deployment this should be restricted to the dashboard's
    // specific origin (e.g. CorsLayer::new().allow_origin("https://dashboard.example.com".parse::<HeaderValue>()?))
    // to prevent cross-origin requests from arbitrary third-party sites.
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let api = Router::new()
        .route("/health", get(health))
        .route("/stats", get(get_stats))
        .route("/wallets", get(get_wallets))
        .route("/wallets/{address}", get(get_wallet))
        .route("/wallets/{address}/trades", get(get_wallet_trades))
        .route("/known-insiders", get(get_known_insiders))
        .route("/score", post(score_wallets))
        .with_state(state);

    Ok(Router::new()
        .nest("/api", api)
        .layer(cors)
        .layer(TraceLayer::new_for_http()))
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn dec_to_f64(d: Decimal) -> f64 {
    d.try_into().unwrap_or(0.0)
}

// ── Handlers ──────────────────────────────────────────────────────────────────

#[instrument(skip(state))]
async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let scored_wallets: i64 = sqlx::query_scalar!("SELECT COUNT(*) FROM wallet_scores")
        .fetch_one(&state.pool)
        .await
        .unwrap_or(Some(0))
        .unwrap_or(0);

    let flagged: i64 =
        sqlx::query_scalar!("SELECT COUNT(*) FROM wallet_scores WHERE flagged = TRUE")
            .fetch_one(&state.pool)
            .await
            .unwrap_or(Some(0))
            .unwrap_or(0);

    Json(HealthResponse {
        status: "ok",
        scored_wallets,
        flagged,
    })
}

#[instrument(skip(state))]
async fn get_stats(State(state): State<AppState>) -> impl IntoResponse {
    let total_wallets: i64 = sqlx::query_scalar!("SELECT COUNT(*) FROM wallet_scores")
        .fetch_one(&state.pool)
        .await
        .unwrap_or(Some(0))
        .unwrap_or(0);

    let flagged_wallets: i64 =
        sqlx::query_scalar!("SELECT COUNT(*) FROM wallet_scores WHERE flagged = TRUE")
            .fetch_one(&state.pool)
            .await
            .unwrap_or(Some(0))
            .unwrap_or(0);

    let total_volume_usdc: f64 = sqlx::query_scalar!(
        "SELECT COALESCE(SUM(total_volume_usdc), 0)::FLOAT8 FROM wallet_scores"
    )
    .fetch_one(&state.pool)
    .await
    .unwrap_or(Some(0.0))
    .unwrap_or(0.0);

    let total_known_insiders: i64 = sqlx::query_scalar!("SELECT COUNT(*) FROM known_insiders")
        .fetch_one(&state.pool)
        .await
        .unwrap_or(Some(0))
        .unwrap_or(0);

    let known_insiders_scored: i64 = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM known_insiders ki
         INNER JOIN wallet_scores ws ON ws.address = ki.address"
    )
    .fetch_one(&state.pool)
    .await
    .unwrap_or(Some(0))
    .unwrap_or(0);

    Json(StatsResponse {
        total_wallets,
        flagged_wallets,
        total_volume_usdc,
        known_insiders_scored,
        total_known_insiders,
    })
}

#[instrument(skip(state))]
async fn get_wallets(
    State(state): State<AppState>,
    Query(params): Query<WalletsQuery>,
) -> impl IntoResponse {
    // Validate min_score: must be finite and in [0.0, 1.0].
    if !params.min_score.is_finite() || params.min_score < 0.0 || params.min_score > 1.0 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    // Validate min_volume_usdc: must be finite and non-negative.
    if !params.min_volume_usdc.is_finite() || params.min_volume_usdc < 0.0 {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let limit = params.limit.clamp(1, 500);
    let min_score = Decimal::try_from(params.min_score).unwrap_or(Decimal::ZERO);
    let min_volume = Decimal::try_from(params.min_volume_usdc).unwrap_or(Decimal::ZERO);

    // Build query using query_as! with a named struct to avoid anonymous type mismatch
    // between the two sqlx::query! branches.
    #[derive(sqlx::FromRow)]
    struct WalletScoreRow {
        address: String,
        score: rust_decimal::Decimal,
        entry_timing_score: rust_decimal::Decimal,
        concentration_score: rust_decimal::Decimal,
        size_score: rust_decimal::Decimal,
        wallet_age_score: rust_decimal::Decimal,
        win_rate_score: rust_decimal::Decimal,
        total_volume_usdc: rust_decimal::Decimal,
        markets_traded: i32,
        flagged: bool,
        first_activity_ts: Option<DateTime<Utc>>,
        scored_at: DateTime<Utc>,
    }

    let rows: Result<Vec<WalletScoreRow>, _> = if params.flagged_only {
        sqlx::query_as!(
            WalletScoreRow,
            r#"
            SELECT address, score, entry_timing_score, concentration_score, size_score,
                   wallet_age_score, win_rate_score, total_volume_usdc, markets_traded,
                   flagged, first_activity_ts, scored_at
            FROM wallet_scores
            WHERE flagged = TRUE AND score >= $1 AND total_volume_usdc >= $4
            ORDER BY score DESC
            LIMIT $2 OFFSET $3
            "#,
            min_score,
            limit,
            params.offset,
            min_volume,
        )
        .fetch_all(&state.pool)
        .await
    } else {
        sqlx::query_as!(
            WalletScoreRow,
            r#"
            SELECT address, score, entry_timing_score, concentration_score, size_score,
                   wallet_age_score, win_rate_score, total_volume_usdc, markets_traded,
                   flagged, first_activity_ts, scored_at
            FROM wallet_scores
            WHERE score >= $1 AND total_volume_usdc >= $4
            ORDER BY score DESC
            LIMIT $2 OFFSET $3
            "#,
            min_score,
            limit,
            params.offset,
            min_volume,
        )
        .fetch_all(&state.pool)
        .await
    };

    match rows {
        Ok(rows) => {
            let resp: Vec<WalletRow> = rows
                .into_iter()
                .map(|r| WalletRow {
                    address: r.address,
                    score: dec_to_f64(r.score),
                    entry_timing_score: dec_to_f64(r.entry_timing_score),
                    concentration_score: dec_to_f64(r.concentration_score),
                    size_score: dec_to_f64(r.size_score),
                    wallet_age_score: dec_to_f64(r.wallet_age_score),
                    win_rate_score: dec_to_f64(r.win_rate_score),
                    total_volume_usdc: dec_to_f64(r.total_volume_usdc),
                    markets_traded: r.markets_traded,
                    flagged: r.flagged,
                    first_activity_ts: r.first_activity_ts,
                    scored_at: r.scored_at,
                })
                .collect();
            Json(resp).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to query wallets");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[instrument(skip(state))]
async fn get_wallet(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> impl IntoResponse {
    let addr = address.to_lowercase();

    let row = sqlx::query!(
        r#"
        SELECT ws.address, ws.score, ws.entry_timing_score, ws.concentration_score,
               ws.size_score, ws.wallet_age_score, ws.win_rate_score, ws.total_volume_usdc,
               ws.markets_traded, ws.flagged, ws.first_activity_ts, ws.scored_at,
               ki.label as "known_label?"
        FROM wallet_scores ws
        LEFT JOIN known_insiders ki ON ki.address = ws.address
        WHERE ws.address = $1
        "#,
        addr,
    )
    .fetch_optional(&state.pool)
    .await;

    match row {
        Ok(Some(r)) => Json(WalletDetail {
            row: WalletRow {
                address: r.address,
                score: dec_to_f64(r.score),
                entry_timing_score: dec_to_f64(r.entry_timing_score),
                concentration_score: dec_to_f64(r.concentration_score),
                size_score: dec_to_f64(r.size_score),
                wallet_age_score: dec_to_f64(r.wallet_age_score),
                win_rate_score: dec_to_f64(r.win_rate_score),
                total_volume_usdc: dec_to_f64(r.total_volume_usdc),
                markets_traded: r.markets_traded,
                flagged: r.flagged,
                first_activity_ts: r.first_activity_ts,
                scored_at: r.scored_at,
            },
            known_label: r.known_label,
        })
        .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!(error = %e, address = %addr, "Failed to query wallet");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[instrument(skip(state))]
async fn get_wallet_trades(
    State(state): State<AppState>,
    Path(address): Path<String>,
) -> impl IntoResponse {
    let addr = address.to_lowercase();
    let url = format!(
        "https://data-api.polymarket.com/trades?user={addr}&limit=500&sortBy=TIMESTAMP&sortDirection=DESC"
    );

    let resp = state
        .http_client
        .get(&url)
        .header(
            "Authorization",
            format!("Bearer {}", state.polymarket_api_key),
        )
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            #[derive(Deserialize)]
            struct RawTrade {
                #[serde(rename = "conditionId")]
                condition_id: String,
                title: Option<String>,
                side: String,
                price: f64,
                size: f64,
                timestamp: i64,
            }

            match r.json::<Vec<RawTrade>>().await {
                Ok(trades) => {
                    let rows: Vec<TradeRow> = trades
                        .into_iter()
                        .map(|t| TradeRow {
                            condition_id: t.condition_id,
                            title: t.title,
                            side: t.side,
                            price: t.price,
                            size: t.size,
                            usdc_amount: t.size * t.price,
                            block_time: DateTime::from_timestamp(t.timestamp, 0)
                                .unwrap_or_default(),
                        })
                        .collect();
                    Json(rows).into_response()
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to parse trades response");
                    StatusCode::BAD_GATEWAY.into_response()
                }
            }
        }
        Ok(r) => {
            tracing::warn!(status = %r.status(), "Data API returned non-200 for trades");
            StatusCode::BAD_GATEWAY.into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to fetch trades from Data API");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

// ── POST /api/score ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ScoreRequest {
    /// One or more wallet addresses to score. Max 10 per request.
    pub addresses: Vec<String>,
}

#[instrument(skip(state))]
async fn score_wallets(
    State(state): State<AppState>,
    Json(req): Json<ScoreRequest>,
) -> impl IntoResponse {
    if req.addresses.is_empty() || req.addresses.len() > 10 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "addresses must contain 1–10 entries" })),
        )
            .into_response();
    }

    let mut results: Vec<WalletRow> = Vec::new();

    for address in &req.addresses {
        let addr = address.to_lowercase();
        match score_one_wallet(addr.clone(), &state.data_api, Some(&state.gamma)).await {
            Ok(ws) => {
                if let Err(e) = upsert_score(&state.pool, &ws).await {
                    tracing::error!(address = %addr, error = %e, "Failed to upsert score");
                }
                results.push(WalletRow {
                    address: ws.address,
                    score: ws.score,
                    entry_timing_score: ws.entry_timing_score,
                    concentration_score: ws.concentration_score,
                    size_score: ws.size_score,
                    wallet_age_score: ws.wallet_age_score,
                    win_rate_score: ws.win_rate_score,
                    total_volume_usdc: ws.total_volume_usdc,
                    markets_traded: ws.markets_traded,
                    flagged: ws.flagged,
                    first_activity_ts: None,
                    scored_at: Utc::now(),
                });
            }
            Err(e) => {
                tracing::warn!(address = %addr, error = %e, "Failed to score wallet, skipping");
            }
        }
    }

    Json(results).into_response()
}

#[instrument(skip(state))]
async fn get_known_insiders(State(state): State<AppState>) -> impl IntoResponse {
    let rows = sqlx::query!(
        r#"
        SELECT ki.address, ki.label, ki.market, ki.source,
               ws.score as "score?", ws.flagged as "flagged?", ws.scored_at as "scored_at?"
        FROM known_insiders ki
        LEFT JOIN wallet_scores ws ON ws.address = ki.address
        ORDER BY ws.score DESC NULLS LAST
        "#,
    )
    .fetch_all(&state.pool)
    .await;

    match rows {
        Ok(rows) => {
            let resp: Vec<KnownInsiderResponse> = rows
                .into_iter()
                .map(|r| KnownInsiderResponse {
                    address: r.address,
                    label: r.label,
                    market: r.market,
                    source: r.source,
                    score: r.score.map(dec_to_f64),
                    flagged: r.flagged,   // Option<bool> from LEFT JOIN
                    scored_at: r.scored_at, // Option<DateTime<Utc>> from LEFT JOIN
                })
                .collect();
            Json(resp).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to query known insiders");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

