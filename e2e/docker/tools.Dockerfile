FROM rust:1.86.0-bullseye AS builder

WORKDIR /build

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    ca-certificates \
    clang \
 && rm -rf /var/lib/apt/lists/*

COPY rust-toolchain.toml ./
RUN channel="$(awk -F'"' '/^channel = / { print $2 }' rust-toolchain.toml)" && \
    test -n "$channel" && \
    rustup toolchain install "$channel" --profile minimal && \
    rustup default "$channel"
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN cargo build --release --bin stryi-wallet || true

COPY . .
RUN cargo build --release --bin stryi-wallet

FROM debian:bullseye-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
      ca-certificates \
      curl \
      jq \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /scripts

COPY --from=builder /build/target/release/stryi-wallet /usr/local/bin/stryi-wallet
