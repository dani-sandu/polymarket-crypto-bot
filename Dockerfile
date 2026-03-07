# Build stage — must match deploy stage Debian version to avoid GLIBC mismatch
FROM rust:bookworm as builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY btc-5min-bot ./btc-5min-bot
COPY eth-5min-bot ./eth-5min-bot
COPY core-shared ./core-shared

RUN cargo build --release

# Deploy stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates procps && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/eth-5min-bot /app/eth-5min-bot
COPY --from=builder /app/target/release/btc-5min-bot /app/btc-5min-bot

# Environment variables are injected at runtime via docker-compose env_file
ENV RUST_LOG=info

# Default command (overridden by docker-compose per service)
CMD ["./btc-5min-bot"]