#!/bin/sh
set -eu

CHAIN_DIR="$1"
MIN_SIZE_KB=256

if [ -z "$CHAIN_DIR" ]; then
  echo "[check-chaingen] ERROR: no chain directory argument provided"
  exit 2
fi

echo "[check-chaingen] Checking chain directory: $CHAIN_DIR"

if [ ! -d "$CHAIN_DIR" ]; then
  echo "[check-chaingen] ERROR: directory does not exist"
  exit 3
fi

SIZE_KB=$(du -sk "$CHAIN_DIR" | awk '{print $1}')

echo "[check-chaingen] Chain directory size: ${SIZE_KB} KB"

if [ "$SIZE_KB" -lt "$MIN_SIZE_KB" ]; then
  echo "[check-chaingen] ERROR: chain directory too small (expected >= ${MIN_SIZE_KB} KB)"
  exit 4
fi

echo "[check-chaingen] OK: chaingen output looks valid"
