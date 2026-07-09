FROM rust:1-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY static ./static
COPY pricing.csv ./
RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/token-usage-insights /app/token-usage-insights
COPY static ./static
COPY pricing.csv ./pricing.csv

ENV PORT=8080
ENV TOKEN_USAGE_INSIGHTS_DATA_SOURCE=snapshot

EXPOSE 8080
CMD ["/app/token-usage-insights"]
