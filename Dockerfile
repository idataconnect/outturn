# syntax=docker/dockerfile:1
FROM rust:1 AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
# Embedded by sqlx::migrate! at compile time.
COPY migrations/ migrations/
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --bin api --bin gateway --bin runtime && \
    cp /app/target/release/api /usr/local/bin/api && \
    cp /app/target/release/gateway /usr/local/bin/gateway && \
    cp /app/target/release/runtime /usr/local/bin/runtime

FROM debian:trixie-slim AS api
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/bin/api /usr/local/bin/api
ENV LISTEN_ADDR=0.0.0.0:8080
EXPOSE 8080
ENTRYPOINT ["api"]

FROM debian:trixie-slim AS gateway
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/bin/gateway /usr/local/bin/gateway
ENV LISTEN_ADDR=0.0.0.0:8081
EXPOSE 8081
ENTRYPOINT ["gateway"]

FROM debian:trixie-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates poppler-utils && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/bin/runtime /usr/local/bin/runtime
ENV LISTEN_ADDR=0.0.0.0:8082
EXPOSE 8082
ENTRYPOINT ["runtime"]
