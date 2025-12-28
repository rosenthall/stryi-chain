#!/bin/sh
set -eu

echo "[e2e] Starting StryiChain e2e"

cd "$(dirname "$0")/docker"

# Always start clean
docker compose down -v || true

# Run pipeline
docker compose up --build \
  --abort-on-container-exit \
  --exit-code-from check-chaingen

echo "[e2e] e2e finished successfully"
