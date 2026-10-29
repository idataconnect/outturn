# syntax=docker/dockerfile:1

# The agent component the runtime executes, built from its source here so the
# image cannot ship a binary older than the code beside it. A copy is still
# committed under assets/ for the tests and for a runtime run outside a
# container; this stage is what makes that copy irrelevant to a release.
# rust-lld needs libLLVM, from llvm-tools, to link a component.
FROM rust:1 AS agent
RUN rustup target add wasm32-wasip2 && rustup component add llvm-tools
WORKDIR /agent
COPY agents/default/ ./
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/agent/target \
    cargo build --locked --release --target wasm32-wasip2 && \
    cp target/wasm32-wasip2/release/outturn_agent_default.wasm /agent_default.wasm

FROM rust:1 AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
# The tiers are crates of their own; the root package holds the tools and tests.
COPY crates/ crates/
COPY src/ src/
# Cargo validates every [[test]] path when parsing the manifest, even for a
# binary-only build and even for targets behind a feature nobody asked for.
# Empty stand-ins satisfy that without copying the real tests, which would
# invalidate this layer whenever a test changed.
#
# Read out of the manifest rather than listed here. A hand-kept list is a list
# that goes stale: adding a [[test]] and forgetting this line breaks every
# image build with an error about a missing file that is deliberately missing,
# a long way from the change that caused it.
RUN mkdir -p tests && \
    awk '/^\[\[test\]\]/ { want = 1; next } \
         /^\[/ { want = 0 } \
         want && /^name = / { gsub(/^name = "|"$/, ""); print }' Cargo.toml \
    | while read -r name; do touch "tests/$name.rs"; done
# Embedded by sqlx::migrate! at compile time.
COPY migrations/ migrations/
# Read by wasmtime's bindgen! macro at compile time.
COPY wit/ wit/
# The model catalog the API starts from, compiled into the core crate.
COPY assets/catalog.json.gz assets/catalog.json.gz
# `release` unless told otherwise; local development asks for `local`, which
# keeps incremental compilation and skips LTO (Cargo.toml says why).
ARG PROFILE=release
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --profile "$PROFILE" --bin api --bin gateway --bin runtime --bin mockllm --bin hollowbrook && \
    cp /app/target/$PROFILE/api /usr/local/bin/api && \
    cp /app/target/$PROFILE/gateway /usr/local/bin/gateway && \
    cp /app/target/$PROFILE/runtime /usr/local/bin/runtime && \
    cp /app/target/$PROFILE/mockllm /usr/local/bin/mockllm && \
    cp /app/target/$PROFILE/hollowbrook /usr/local/bin/hollowbrook

# Each service is its binary on distroless: glibc, libgcc, CA certificates and
# tzdata, with no shell or package manager -- about 28MB under the binary
# rather than the 90MB a slim Debian and its apt cache came to. Debian 13, to
# match the builder's glibc; a binary built against a newer one than it runs on
# does not start. Nothing here links OpenSSL (reqwest is on rustls), which is
# what keeps the `cc` image enough.
FROM gcr.io/distroless/cc-debian13 AS api
COPY --from=builder /usr/local/bin/api /usr/local/bin/api
ENV LISTEN_ADDR=0.0.0.0:8080
EXPOSE 8080
USER 65534:65534
ENTRYPOINT ["api"]

FROM gcr.io/distroless/cc-debian13 AS gateway
COPY --from=builder /usr/local/bin/gateway /usr/local/bin/gateway
ENV LISTEN_ADDR=0.0.0.0:8081
EXPOSE 8081
USER 65534:65534
ENTRYPOINT ["gateway"]

FROM gcr.io/distroless/cc-debian13 AS runtime
COPY --from=builder /usr/local/bin/runtime /usr/local/bin/runtime
COPY --from=agent /agent_default.wasm /usr/local/share/outturn/agent_default.wasm
ENV LISTEN_ADDR=0.0.0.0:8082
EXPOSE 8082
USER 65534:65534
ENTRYPOINT ["runtime"]

# A model that never was. Runs at zero replicas until a load test wants it.
FROM gcr.io/distroless/cc-debian13 AS mockllm
COPY --from=builder /usr/local/bin/mockllm /usr/local/bin/mockllm
EXPOSE 8083
USER 65534:65534
ENTRYPOINT ["mockllm"]

# A guesthouse that never was. Sends webhooks, answers a REST API, and serves
# its own OpenAPI document, so the seams between tiers can be tested across
# real sockets. Zero replicas until something wants it.
FROM gcr.io/distroless/cc-debian13 AS hollowbrook
COPY --from=builder /usr/local/bin/hollowbrook /usr/local/bin/hollowbrook
EXPOSE 8084
USER 65534:65534
ENTRYPOINT ["hollowbrook"]

# The PDF renderer, deployed only where an operator wants PDFs
# (k8s/components/pdf-renderer). A crate of its own, so its fonts and layout
# are in this image and in no other.
FROM rust:1 AS renderer-builder
WORKDIR /app
COPY renderers/pdf/ ./
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --bin outturn-renderer && \
    cp /app/target/release/outturn-renderer /usr/local/bin/outturn-renderer

FROM gcr.io/distroless/cc-debian13 AS pdf-renderer
COPY --from=renderer-builder /usr/local/bin/outturn-renderer /usr/local/bin/outturn-renderer
ENV LISTEN_ADDR=0.0.0.0:8090
EXPOSE 8090
USER 65534:65534
ENTRYPOINT ["outturn-renderer"]

# The HEIC converter, deployed only where an operator wants phone photos
# converted (k8s/components/heic). Its own crate and its own image: libheif
# and its HEVC decoder are in this image and in no other.
FROM rust:1 AS heic-builder
RUN apt-get update && \
    apt-get install -y --no-install-recommends libheif-dev && \
    rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY renderers/heic/ ./
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --bin outturn-heic && \
    cp /app/target/release/outturn-heic /usr/local/bin/outturn-heic

# Debian 13 rather than distroless: libheif finds its decoder as a plugin
# loaded at runtime, and the package manager is what puts that and every
# library under it in the right place.
FROM debian:trixie-slim AS heic
RUN apt-get update && \
    apt-get install -y --no-install-recommends libheif1 libheif-plugin-libde265 && \
    rm -rf /var/lib/apt/lists/*
COPY --from=heic-builder /usr/local/bin/outturn-heic /usr/local/bin/outturn-heic
ENV LISTEN_ADDR=0.0.0.0:8091
EXPOSE 8091
USER 65534:65534
ENTRYPOINT ["outturn-heic"]
