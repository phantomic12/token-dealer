# syntax=docker/dockerfile:1.7
# ---- build stage ----
FROM rust:1.86-slim-bookworm AS builder

WORKDIR /build

# Cache deps separately
COPY Cargo.toml Cargo.lock* ./
RUN mkdir -p src && echo "fn main(){}" > src/main.rs && echo "" > src/lib.rs && \
    cargo build --release --bin token-dealer && \
    rm -rf src target/release/token-dealer target/release/deps/token-dealer-*

COPY . .
RUN cargo build --release --bin token-dealer

# ---- runtime stage ----
FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates tini \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -r -u 1000 -m -d /data -s /usr/sbin/nologin dealer

WORKDIR /app
COPY --from=builder /build/target/release/token-dealer /app/token-dealer
COPY token-dealer.toml.example /app/token-dealer.toml

ENV TOKEN_DEALER_CONFIG=/app/token-dealer.toml \
    RUST_LOG=info \
    PORT=8080

USER dealer
EXPOSE 8080

ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/app/token-dealer"]

HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD ["/app/token-dealer", "--healthcheck"] || exit 1
