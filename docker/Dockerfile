# Multi-stage build for minimum image size
FROM rust:1.95-slim AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev cmake && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src/ src/

RUN cargo build --release --bin aave-liquidation-bot

# Runtime image
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/aave-liquidation-bot /usr/local/bin/
COPY config.toml /app/config.toml

WORKDIR /app
ENV CONFIG_PATH=/app/config.toml
ENV LOG_FORMAT=json
ENV RUST_LOG=info

CMD ["aave-liquidation-bot"]
