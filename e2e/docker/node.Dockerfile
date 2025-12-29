FROM rust:1.85.0-bullseye AS builder

WORKDIR /build

# --- System deps
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    clang \
    ca-certificates \
    protobuf-compiler \
 && rm -rf /var/lib/apt/lists/*

# --- Cache deps
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# Dummy main to warm up dependency cache
RUN mkdir -p src && echo "fn main() {}" > src/main.rs
RUN cargo build --release || true

# --- Build real node binary
COPY . .
RUN cargo build --release --bin stryi_node

# Runner
FROM debian:bullseye-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
 && rm -rf /var/lib/apt/lists/*


WORKDIR /app

COPY --from=builder /build/target/release/stryi_node /usr/local/bin/stryi_node

ENTRYPOINT ["stryi_node"]