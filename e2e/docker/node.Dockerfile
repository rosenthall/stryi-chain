FROM rust:1.86.0-bullseye AS builder

WORKDIR /build

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    clang \
    ca-certificates \
    protobuf-compiler \
 && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# Warm the dependency cache before copying the full workspace.
RUN mkdir -p src && echo "fn main() {}" > src/main.rs
RUN cargo build --release || true

COPY . .
RUN cargo build --release --bin stryi_node

FROM debian:bullseye-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
 && rm -rf /var/lib/apt/lists/*


WORKDIR /app

COPY --from=builder /build/target/release/stryi_node /usr/local/bin/stryi_node

ENTRYPOINT ["stryi_node"]
