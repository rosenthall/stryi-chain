FROM rust:1.85.0-bullseye AS builder

WORKDIR /build

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    ca-certificates \
    clang \
 && rm -rf /var/lib/apt/lists/*

# Workspace manifests
COPY Cargo.toml Cargo.lock ./

# Workspace crates
COPY crates ./crates

# Warm up deps cache
RUN cargo build --release -p stryi_devkit || true

# Copy the rest of the workspace
COPY . .

# Build devkit only
RUN cargo build --release -p stryi_devkit



# Runner
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
