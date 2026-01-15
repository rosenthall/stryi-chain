#!/bin/sh
set -eu

ADDR=""
HTTP_PORT=""
EXPECTED_HASH=""
EXPECTED_HEIGHT=""

# Argument parsing
while [ $# -gt 0 ]; do
  case "$1" in
    --address) ADDR="$2"; shift 2 ;;
    --http) HTTP_PORT="$2"; shift 2 ;;
    --expected-hash) EXPECTED_HASH="$2"; shift 2 ;;
    --expected-height) EXPECTED_HEIGHT="$2"; shift 2 ;;
    *) echo "[assert-node] Unknown arg: $1"; exit 1 ;;
  esac
done

if [ -z "$ADDR" ] || [ -z "$HTTP_PORT" ]; then
  echo "[assert-node] address and http port are required"
  exit 1
fi

URL="http://${ADDR}:${HTTP_PORT}/api/nodestate"

MAX_ATTEMPTS=30
SLEEP_SEC=2

i=1
while [ "$i" -le "$MAX_ATTEMPTS" ]; do
  echo "[assert-node] attempt $i/$MAX_ATTEMPTS"

  # Fetch nodestate
  if ! STATE="$(curl -sf "$URL" 2>/dev/null)"; then
    echo "[assert-node] nodestate not reachable yet"
    sleep "$SLEEP_SEC"
    i=$((i+1))
    continue
  fi

  HEIGHT="$(echo "$STATE" | jq -r '.height')"
  HASH="$(echo "$STATE" | jq -r '.latest_block_hash')"

  echo "[assert-node] height=$HEIGHT hash=$HASH"

  # Height sanity check
  if ! echo "$HEIGHT" | grep -Eq '^[0-9]+$' || [ "$HEIGHT" -le 0 ]; then
    echo "[assert-node] chain not initialized yet"
    sleep "$SLEEP_SEC"
    i=$((i+1))
    continue
  fi

  # Hash check (FATAL)
  if [ -n "$EXPECTED_HASH" ] && [ "$HASH" != "$EXPECTED_HASH" ]; then
    echo "[assert-node] FATAL: hash mismatch"
    echo "[assert-node] expected=$EXPECTED_HASH"
    echo "[assert-node] got     =$HASH"
    exit 1
  fi

  # Height expectation check
  if [ -n "$EXPECTED_HEIGHT" ] && [ "$HEIGHT" -lt "$EXPECTED_HEIGHT" ]; then
    echo "[assert-node] waiting for height $EXPECTED_HEIGHT"
    sleep "$SLEEP_SEC"
    i=$((i+1))
    continue
  fi

  # TODO: Add grpcurl checks here


  echo "[assert-node] node is ready"
  exit 0
done

echo "[assert-node] timeout waiting for node readiness"
exit 1
