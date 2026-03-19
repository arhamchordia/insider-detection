// src/main_api.rs
//
// Entry point for the `api` binary.
// Serves REST endpoints from the wallet_scores PostgreSQL table.

use anyhow::{Context, Result};
use insider_detection::api::routes::build_router;
use sqlx::postgres::PgPoolOptions;
use tracing::info;

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

    let port: u16 = std::env::var("PORT")
        .unwrap_or_else(|_| "8080".to_string())
        .parse()
        .context("PORT must be a valid u16")?;

    info!("Connecting to database");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .context("Failed to connect to PostgreSQL")?;

    let router = build_router(pool).context("Failed to build router")?;
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], port));

    info!(port, "API server listening");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("Failed to bind to port {port}"))?;

    axum::serve(listener, router)
        .await
        .context("API server error")?;

    Ok(())
}
