FROM rust:1.94-bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential cmake libopus-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY config.toml ./config.toml
RUN cargo build --release --locked

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libopus0 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --user-group --uid 10001 \
        --home-dir /app --shell /usr/sbin/nologin recorder \
    && mkdir -p /app/data \
    && chown recorder:recorder /app/data

WORKDIR /app
COPY --from=builder /build/target/release/call-recorder-bot /usr/local/bin/call-recorder-bot
COPY --chown=recorder:recorder --chmod=0644 config.toml ./config.toml

USER recorder
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/call-recorder-bot"]
