#!/bin/sh
set -eu

# Usage: assert-reorg-complete.sh --node <addr>:<port> ... --expected-height <N>

NODES=""
EXPECTED_HEIGHT=""
EXPECTED_HASH=""
MAX_ATTEMPTS=60
SLEEP_SEC=3

while [ $# -gt 0 ]; do
  case "$1" in
    --node) NODES="$NODES $2"; shift 2 ;;
    --expected-height) EXPECTED_HEIGHT="$2"; shift 2 ;;
    --expected-hash) EXPECTED_HASH="$2"; shift 2 ;;
    *) echo "[assert-reorg] Unknown arg: $1"; exit 1 ;;
  esac
done

if [ -z "$NODES" ] || [ -z "$EXPECTED_HEIGHT" ]; then
  echo "[assert-reorg] --node and --expected-height are required"
  exit 1
fi

i=1
while [ "$i" -le "$MAX_ATTEMPTS" ]; do
  echo "[assert-reorg] attempt $i/$MAX_ATTEMPTS"

  ALL_READY=true
  COLLECTED_HASHES=""
  COLLECTED_HEIGHTS=""

  for NODE in $NODES; do
    URL="http://${NODE}/api/nodestate"

    if ! STATE="$(curl -sf "$URL" 2>/dev/null)"; then
      echo "[assert-reorg] $NODE not reachable yet"
      ALL_READY=false
      break
    fi

    HEIGHT="$(echo "$STATE" | jq -r '.height')"
    HASH="$(echo "$STATE" | jq -r '.latest_block_hash')"

    echo "[assert-reorg] $NODE -> height=$HEIGHT hash=$HASH"

    if ! echo "$HEIGHT" | grep -Eq '^[0-9]+$' || [ "$HEIGHT" -le 0 ]; then
      echo "[assert-reorg] $NODE chain not initialized yet"
      ALL_READY=false
      break
    fi

    if [ "$HEIGHT" -lt "$EXPECTED_HEIGHT" ]; then
      echo "[assert-reorg] $NODE still at height $HEIGHT, waiting for $EXPECTED_HEIGHT"
      ALL_READY=false
      break
    fi

    COLLECTED_HASHES="$COLLECTED_HASHES $HASH"
    COLLECTED_HEIGHTS="$COLLECTED_HEIGHTS $HEIGHT"
  done

  if [ "$ALL_READY" = true ] && [ -n "$COLLECTED_HASHES" ]; then
    FIRST_HASH=$(echo "$COLLECTED_HASHES" | tr ' ' '\n' | grep . | head -1)
    ALL_MATCH=true

    for HASH in $COLLECTED_HASHES; do
      if [ "$HASH" != "$FIRST_HASH" ]; then
        ALL_MATCH=false
        break
      fi
    done

    if [ "$ALL_MATCH" = true ]; then
      if [ -n "$EXPECTED_HASH" ] && [ "$FIRST_HASH" != "$EXPECTED_HASH" ]; then
        echo "[assert-reorg] FATAL: converged hash mismatch"
        echo "[assert-reorg] expected=$EXPECTED_HASH"
        echo "[assert-reorg] got     =$FIRST_HASH"
        exit 1
      fi
      echo "[assert-reorg] SUCCESS: All nodes converged at height>=$EXPECTED_HEIGHT with hash=$FIRST_HASH"
      exit 0
    else
      echo "[assert-reorg] FATAL: Nodes at expected height but hashes differ!"
      echo "[assert-reorg] Hashes:$COLLECTED_HASHES"
      exit 1
    fi
  fi

  sleep "$SLEEP_SEC"
  i=$((i + 1))
done

echo "[assert-reorg] TIMEOUT: nodes did not converge within $((MAX_ATTEMPTS * SLEEP_SEC))s"
exit 1
