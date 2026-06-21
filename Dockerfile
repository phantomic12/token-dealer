# syntax=docker/dockerfile:1.7
# ---- build stage ----
FROM rust:1.88-slim-bookworm AS builder

WORKDIR /build

# Full source copy + single clean build. The dep-cache trick
# (empty stub + rm + real copy + rebuild) was leaving the stub
# binary in the final image because cargo didn't see the
# real source as invalidating the cached artifact.
COPY . .
RUN cargo build --release --bin token-dealer

# ---- runtime stage ----
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates tini \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -r -u 1000 -m -d /data -s /usr/sbin/nologin dealer

RUN mkdir -p /data && chown -R dealer:dealer /data
WORKDIR /app
COPY --from=builder /build/target/release/token-dealer /app/token-dealer
# Default config is empty — the user is expected to mount their own
# config at /data/token-dealer.toml (TOKEN_DEALER_CONFIG below) OR
# pass --config /path/to/config.toml. Without a mounted config, the
# server starts with an empty provider list and the manifest defaults
# fill in the model names for /v1/models.

ENV TOKEN_DEALER_CONFIG=/data/token-dealer.toml \
    RUST_LOG=info \
    PORT=8080

USER dealer
EXPOSE 8080

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/app/token-dealer"]

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD ["/app/token-dealer", "--healthcheck"] || exit 1
