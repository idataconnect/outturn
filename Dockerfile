# syntax=docker/dockerfile:1
FROM rust:1 AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock build.rs ./
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
# The agent component the runtime executes. Committed as a build artifact so
# the image does not need the wasm toolchain.
COPY assets/agent_default.wasm /agent_default.wasm
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
ENTRYPOINT ["api"]

FROM gcr.io/distroless/cc-debian13 AS gateway
COPY --from=builder /usr/local/bin/gateway /usr/local/bin/gateway
ENV LISTEN_ADDR=0.0.0.0:8081
EXPOSE 8081
ENTRYPOINT ["gateway"]

FROM gcr.io/distroless/cc-debian13 AS runtime
COPY --from=builder /usr/local/bin/runtime /usr/local/bin/runtime
COPY --from=builder /agent_default.wasm /usr/local/share/outturn/agent_default.wasm
ENV LISTEN_ADDR=0.0.0.0:8082
EXPOSE 8082
ENTRYPOINT ["runtime"]

# A model that never was. Runs at zero replicas until a load test wants it.
FROM gcr.io/distroless/cc-debian13 AS mockllm
COPY --from=builder /usr/local/bin/mockllm /usr/local/bin/mockllm
EXPOSE 8083
ENTRYPOINT ["mockllm"]

# A guesthouse that never was. Sends webhooks, answers a REST API, and serves
# its own OpenAPI document, so the seams between tiers can be tested across
# real sockets. Zero replicas until something wants it.
FROM gcr.io/distroless/cc-debian13 AS hollowbrook
COPY --from=builder /usr/local/bin/hollowbrook /usr/local/bin/hollowbrook
EXPOSE 8084
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
