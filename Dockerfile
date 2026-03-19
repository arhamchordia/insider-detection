# ── Stage 1: Install cargo-chef + system deps once ────────────────────────────
FROM rust:1.88-slim AS chef

RUN cargo install cargo-chef --locked
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# ── Stage 2: Compute the dependency recipe ────────────────────────────────────
FROM chef AS planner

COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ── Stage 3: Build and cache all dependencies ──────────────────────────────────
# This layer is invalidated only when Cargo.lock / Cargo.toml change, not when
# source files change — solving the timestamp-based false-cache hit entirely.
FROM chef AS cacher

COPY --from=planner /app/recipe.json recipe.json
ENV SQLX_OFFLINE=true
RUN cargo chef cook --release --recipe-path recipe.json

# ── Stage 4: Build real binaries ───────────────────────────────────────────────
FROM chef AS builder

COPY . .
COPY --from=cacher /app/target target

ENV SQLX_OFFLINE=true
RUN cargo build --release --bin api --bin scorer --bin score-wallet

# ── Stage 5: Minimal runtime image ────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

RUN apt-get update && \
    apt-get install -y ca-certificates libssl3 curl && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/api          ./api
COPY --from=builder /app/target/release/scorer       ./scorer
COPY --from=builder /app/target/release/score-wallet ./score-wallet

EXPOSE 8080

CMD ["./api"]
