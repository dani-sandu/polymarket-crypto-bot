# Build stage
FROM rust:latest as builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY btc-5min-bot ./btc-5min-bot
COPY eth-5min-bot ./eth-5min-bot
COPY core-shared ./core-shared

RUN cargo build --release

# Deploy stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/eth-5min-bot /app/eth-5min-bot
COPY --from=builder /app/target/release/btc-5min-bot /app/btc-5min-bot

# Create directory for .env file binding
RUN mkdir -p /app/config

# Set environment to load from .env file
ENV RUST_LOG=info

CMD ["./eth-5min-bot"]