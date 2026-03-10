FROM rust:1.86.0-bullseye AS builder

WORKDIR /build

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    ca-certificates \
    clang \
 && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# Warm the dependency cache before copying the full workspace.
RUN cargo build --release -p stryi_devkit || true

COPY . .
RUN cargo build --release -p stryi_devkit
FROM debian:bullseye-slim AS runner

RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder \
  /build/target/release/stryi-devkit \
  /usr/local/bin/stryi-devkit

ENTRYPOINT ["stryi-devkit"]
