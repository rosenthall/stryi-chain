#!/bin/sh
set -eu

NODE_URL=""
SENDER_ADDRESS=""
SENDER_KEY=""
RECIPIENT_ADDRESS=""
AMOUNT=""
WALLET_PATH="${WALLET_PATH:-/tmp/tx-lifecycle-wallet.json}"
MAX_ATTEMPTS="${MAX_ATTEMPTS:-30}"
SLEEP_SEC="${SLEEP_SEC:-1}"
LAST_NODESTATE=""
LAST_TX_RESPONSE=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --node-url) NODE_URL="$2"; shift 2 ;;
    --sender-address) SENDER_ADDRESS="$2"; shift 2 ;;
    --sender-key) SENDER_KEY="$2"; shift 2 ;;
    --recipient-address) RECIPIENT_ADDRESS="$2"; shift 2 ;;
    --amount) AMOUNT="$2"; shift 2 ;;
    *) echo "[assert-tx] Unknown arg: $1"; exit 1 ;;
  esac
done

if [ -z "$NODE_URL" ] || [ -z "$SENDER_ADDRESS" ] || [ -z "$SENDER_KEY" ] || [ -z "$RECIPIENT_ADDRESS" ] || [ -z "$AMOUNT" ]; then
  echo "[assert-tx] --node-url, --sender-address, --sender-key, --recipient-address, and --amount are required"
  exit 1
fi

wait_for_node() {
  i=1
  while [ "$i" -le "$MAX_ATTEMPTS" ]; do
    echo "[assert-tx] waiting for node ($i/$MAX_ATTEMPTS)"
    if curl -fsS "${NODE_URL}/api/nodestate" >/dev/null 2>&1; then
      return 0
    fi
    sleep "$SLEEP_SEC"
    i=$((i + 1))
  done

  echo "[assert-tx] TIMEOUT: node did not become reachable"
  exit 1
}

query_status() {
  curl -fsS "${NODE_URL}/api/tx/$1"
}

get_nodestate() {
  curl -fsS "${NODE_URL}/api/nodestate"
}

get_balance() {
  curl -fsS "${NODE_URL}/api/address/$1/balance"
}

read_height() {
  LAST_NODESTATE="$(get_nodestate)"
  printf '%s' "$LAST_NODESTATE" | jq -r '.height'
}

read_balance() {
  get_balance "$1" | jq -r '.balance'
}

read_latest_block_hash() {
  if [ -z "$LAST_NODESTATE" ]; then
    LAST_NODESTATE="$(get_nodestate)"
  fi
  printf '%s' "$LAST_NODESTATE" | jq -r '.latest_block_hash'
}

read_last_update_time() {
  if [ -z "$LAST_NODESTATE" ]; then
    LAST_NODESTATE="$(get_nodestate)"
  fi
  printf '%s' "$LAST_NODESTATE" | jq -r '.last_update_time'
}

print_last_context() {
  if [ -n "$LAST_TX_RESPONSE" ]; then
    echo "[assert-tx] last tx response: $LAST_TX_RESPONSE"
  fi
  if [ -n "$LAST_NODESTATE" ]; then
    echo "[assert-tx] last nodestate: $LAST_NODESTATE"
  fi
}

wait_for_pending() {
  tx_hash="$1"
  i=1
  while [ "$i" -le "$MAX_ATTEMPTS" ]; do
    echo "[assert-tx] waiting for pending status ($i/$MAX_ATTEMPTS)"
    if response="$(query_status "$tx_hash" 2>/dev/null)"; then
      LAST_TX_RESPONSE="$response"
      status="$(printf '%s' "$response" | jq -r '.status // empty')"
      echo "[assert-tx] current status=$status"
      if [ "$status" = "pending" ]; then
        echo "[assert-tx] observed pending state"
        return 0
      fi
      if [ "$status" = "confirmed" ]; then
        echo "[assert-tx] transaction reached confirmed before pending was observed"
        return 1
      fi
    fi
    sleep "$SLEEP_SEC"
    i=$((i + 1))
  done

  echo "[assert-tx] transaction never exposed a pending state before confirmation polling continued"
  print_last_context
  return 1
}

wait_for_confirmed() {
  tx_hash="$1"
  i=1
  while [ "$i" -le "$MAX_ATTEMPTS" ]; do
    echo "[assert-tx] waiting for confirmed status ($i/$MAX_ATTEMPTS)"
    if response="$(query_status "$tx_hash" 2>/dev/null)"; then
      LAST_TX_RESPONSE="$response"
      status="$(printf '%s' "$response" | jq -r '.status // empty')"
      block_height="$(printf '%s' "$response" | jq -r '.block_height // empty')"
      echo "[assert-tx] current status=$status block_height=${block_height:-n/a}"
      if [ "$status" = "confirmed" ] && [ -n "$block_height" ] && [ "$block_height" -ge 1 ]; then
        return 0
      fi
    fi
    sleep "$SLEEP_SEC"
    i=$((i + 1))
  done

  echo "[assert-tx] TIMEOUT: transaction was not confirmed"
  print_last_context
  exit 1
}

wait_for_node

rm -f "$WALLET_PATH"
export NO_COLOR=1

initial_height="$(read_height)"
initial_block_hash="$(read_latest_block_hash)"
initial_last_update_time="$(read_last_update_time)"
sender_balance_before="$(read_balance "$SENDER_ADDRESS")"
recipient_balance_before="$(read_balance "$RECIPIENT_ADDRESS")"

echo "[assert-tx] tx-lifecycle scenario"
echo "[assert-tx] node=$NODE_URL"
echo "[assert-tx] sender=$SENDER_ADDRESS"
echo "[assert-tx] recipient=$RECIPIENT_ADDRESS"
echo "[assert-tx] amount=$AMOUNT"
echo "[assert-tx] initial height=$initial_height"
echo "[assert-tx] initial block hash=$initial_block_hash"
echo "[assert-tx] initial last update=$initial_last_update_time"
echo "[assert-tx] sender balance before=$sender_balance_before"
echo "[assert-tx] recipient balance before=$recipient_balance_before"

if [ "$initial_height" -gt 0 ]; then
  echo "[assert-tx] note: scenario volume already contains chain state"
fi

stryi-wallet --wallet-path "$WALLET_PATH" import --key "$SENDER_KEY" --label funded >/tmp/tx-wallet-import.log 2>&1
cat /tmp/tx-wallet-import.log

set +e
send_output="$(stryi-wallet \
  --wallet-path "$WALLET_PATH" \
  --node "$NODE_URL" \
  send \
  --from "$SENDER_ADDRESS" \
  --to "$RECIPIENT_ADDRESS" \
  --amount "$AMOUNT" 2>&1)"
send_status=$?
set -e

printf '%s\n' "$send_output"

if [ "$send_status" -ne 0 ]; then
  echo "[assert-tx] FATAL: wallet send failed"
  exit "$send_status"
fi

tx_hash="$(printf '%s\n' "$send_output" | grep -Eo 'Tx[0-9a-f]{64}' | head -n 1 || true)"
if [ -z "$tx_hash" ]; then
  echo "[assert-tx] FATAL: failed to parse transaction hash from wallet output"
  exit 1
fi

echo "[assert-tx] submitted transaction $tx_hash"

wait_for_pending "$tx_hash" || true
wait_for_confirmed "$tx_hash"

final_height="$(read_height)"
final_block_hash="$(read_latest_block_hash)"
final_last_update_time="$(read_last_update_time)"
sender_balance_after="$(read_balance "$SENDER_ADDRESS")"
recipient_balance_after="$(read_balance "$RECIPIENT_ADDRESS")"
expected_height=$((initial_height + 1))
sender_spent=$((sender_balance_before - sender_balance_after))
recipient_gain=$((recipient_balance_after - recipient_balance_before))
implied_fee=$((sender_spent - recipient_gain))

echo "[assert-tx] final height=$final_height expected=$expected_height"
echo "[assert-tx] final block hash=$final_block_hash"
echo "[assert-tx] final last update=$final_last_update_time"
echo "[assert-tx] sender balance after=$sender_balance_after spent=$sender_spent"
echo "[assert-tx] recipient balance after=$recipient_balance_after gained=$recipient_gain"
echo "[assert-tx] implied fee=$implied_fee"

if [ "$final_height" -ne "$expected_height" ]; then
  echo "[assert-tx] FATAL: expected node height to advance by exactly one block"
  print_last_context
  exit 1
fi

if [ "$sender_spent" -lt "$AMOUNT" ]; then
  echo "[assert-tx] FATAL: sender balance did not decrease by the transfer amount"
  print_last_context
  exit 1
fi

if [ "$recipient_gain" -lt "$AMOUNT" ]; then
  echo "[assert-tx] FATAL: recipient balance did not increase by the transfer amount"
  print_last_context
  exit 1
fi

stryi-wallet --wallet-path "$WALLET_PATH" --node "$NODE_URL" tx --id "$tx_hash" >/tmp/tx-wallet-query.log 2>&1
cat /tmp/tx-wallet-query.log

echo "[assert-tx] SUCCESS: transaction moved from pending to confirmed"
