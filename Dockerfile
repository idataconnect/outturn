# syntax=docker/dockerfile:1
FROM rust:1 AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
# Cargo validates every [[test]] path when parsing the manifest, even for a
# binary-only build. Empty stand-ins satisfy that without copying the real
# tests, which would invalidate this layer whenever a test changed.
RUN mkdir -p tests && \
    touch tests/api.rs tests/queue_and_events.rs \
          tests/agent_component.rs tests/agent_component_fake.rs
# Embedded by sqlx::migrate! at compile time.
COPY migrations/ migrations/
# Read by wasmtime's bindgen! macro at compile time.
COPY wit/ wit/
# The agent component the runtime executes. Committed as a build artifact so
# the image does not need the wasm toolchain.
COPY assets/agent_default.wasm /agent_default.wasm
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --bin api --bin gateway --bin runtime --bin mockllm && \
    cp /app/target/release/api /usr/local/bin/api && \
    cp /app/target/release/gateway /usr/local/bin/gateway && \
    cp /app/target/release/runtime /usr/local/bin/runtime && \
    cp /app/target/release/mockllm /usr/local/bin/mockllm

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
COPY --from=builder /agent_default.wasm /usr/local/share/outturn/agent_default.wasm
ENV LISTEN_ADDR=0.0.0.0:8082
EXPOSE 8082
ENTRYPOINT ["runtime"]

# A model that never was. Runs at zero replicas until a load test wants it.
FROM debian:trixie-slim AS mockllm
COPY --from=builder /usr/local/bin/mockllm /usr/local/bin/mockllm
EXPOSE 8083
ENTRYPOINT ["mockllm"]
