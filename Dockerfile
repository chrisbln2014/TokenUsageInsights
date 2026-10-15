FROM rust:1-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY static ./static
COPY pricing.csv ./
COPY build.rs ./
# .dockerignore 排除了 .git，build.rs 取不到 commit 日期；部署時用 --build-arg APP_BUILD_DATE=YYYY-MM-DD 傳入
ARG APP_BUILD_DATE
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
ENV TOKEN_USAGE_INSIGHTS_AUTO_UPDATE=0

EXPOSE 8080
CMD ["/app/token-usage-insights"]
