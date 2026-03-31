FROM rust:1.86.0-bullseye AS chef

WORKDIR /build

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    clang \
    ca-certificates \
    protobuf-compiler \
 && rm -rf /var/lib/apt/lists/*

COPY rust-toolchain.toml ./
RUN channel="$(awk -F'"' '/^channel = / { print $2 }' rust-toolchain.toml)" && \
    test -n "$channel" && \
    rustup toolchain install "$channel" --profile minimal && \
    rustup default "$channel" && \
    cargo install --locked cargo-chef --version 0.1.77

FROM chef AS planner

COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder

COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

COPY . .
RUN cargo build --release --bin stryi-node

FROM debian:bullseye-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
 && rm -rf /var/lib/apt/lists/*


WORKDIR /app

COPY --from=builder /build/target/release/stryi-node /usr/local/bin/stryi-node

ENTRYPOINT ["stryi-node"]
