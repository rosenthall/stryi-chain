#!/usr/bin/env bash
set -euo pipefail

DC="docker compose -f docker-compose.e2e.yml"
OUTPUT_PATH="${BENCHMARK_OUTPUT_PATH:-/tmp/benchmark.bmf.json}"
BENCHMARK_JSON_PATH="${BENCHMARK_JSON_PATH:-/tmp/benchmark.json}"
BENCHMARK_SUMMARY_PATH="${BENCHMARK_SUMMARY_PATH:-/tmp/benchmark-summary.md}"
LOG_DIR="${BENCHMARK_LOG_DIR:-}"

timestamp_pattern='[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]+Z'
number_pattern='^([0-9]+([.][0-9]+)?|[.][0-9]+)$'

logs_for() {
  local service="$1"

  if [ -n "$LOG_DIR" ]; then
    cat "${LOG_DIR}/${service}.log"
    return
  fi

  $DC logs "$service" 2>/dev/null || true
}

first_match() {
  local service="$1" pattern="$2"
  (
    logs_for "$service" | grep -m1 -E "$pattern" || true
  ) | head -n1
}

logs_until_ts() {
  local service="$1" end_ts="$2"

  if [ -z "$end_ts" ]; then
    logs_for "$service"
    return
  fi

  logs_for "$service" | awk -v end_ts="$end_ts" '
    match($0, /[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]+Z/) {
      line_ts = substr($0, RSTART, RLENGTH)
      if (line_ts > end_ts) {
        exit
      }
    }
    { print }
  '
}

service_finished_at() {
  local service="$1"

  if [ -n "$LOG_DIR" ] && [ -f "${LOG_DIR}/${service}.finished_at" ]; then
    head -n1 "${LOG_DIR}/${service}.finished_at"
    return
  fi

  local cid=""
  cid="$($DC ps -a -q "$service" 2>/dev/null | head -n1)"
  if [ -z "$cid" ]; then
    return
  fi

  docker inspect -f '{{.State.FinishedAt}}' "$cid" 2>/dev/null \
    | sed '/^0001-01-01T00:00:00Z$/d' \
    | head -n1
}

extract_ts() {
  first_match "$1" "$2" \
    | sed -nE "s/.*(${timestamp_pattern}).*/\\1/p" \
    | head -n1
}

extract_capture() {
  local service="$1" pattern="$2" capture="$3"
  (
    logs_for "$service" | grep -m1 -E "$pattern" | sed -nE "s/${capture}/\\1/p" || true
  ) | head -n1
}

sum_captures_until() {
  local service="$1" end_ts="$2" pattern="$3" capture="$4"
  local values=""

  values="$(
    logs_until_ts "$service" "$end_ts" \
      | grep -E "$pattern" \
      | sed -nE "s/${capture}/\\1/p" || true
  )"

  if [ -z "$values" ]; then
    echo ""
    return
  fi

  printf '%s\n' "$values" | paste -sd+ - | bc
}

count_matches_until() {
  local service="$1" end_ts="$2" pattern="$3"
  logs_until_ts "$service" "$end_ts" | grep -c -E "$pattern" || true
}

diff_seconds() {
  local t1="$1" t2="$2"

  if [ -z "$t1" ] || [ -z "$t2" ]; then
    echo ""
    return
  fi

  local s1 s2
  s1=$(to_epoch_seconds "$t1") || {
    echo ""
    return
  }
  s2=$(to_epoch_seconds "$t2") || {
    echo ""
    return
  }

  echo "$s2 - $s1" | bc
}

to_epoch_seconds() {
  local ts="$1"

  if date -d "$ts" +%s.%N >/dev/null 2>&1; then
    date -d "$ts" +%s.%N
    return
  fi

  python3 -c 'from datetime import datetime, timezone; import sys; ts=sys.argv[1]; dt=datetime.fromisoformat(ts.replace("Z","+00:00")); print(f"{dt.timestamp():.9f}")' "$ts"
}

duration_measure() {
  local value="$1"
  if [ -n "$value" ] && echo "$value" | grep -Eq "$number_pattern"; then
    if [ "${value#*.}" != "$value" ] && [ "${value#.}" != "$value" ]; then
      value="0${value}"
    fi
    printf "%.1f" "$value"
  fi
}

is_number() {
  echo "$1" | grep -Eq "$number_pattern"
}

METRICS_FILE="$(mktemp)"
cleanup() {
  rm -f "$METRICS_FILE"
}
trap cleanup EXIT

add_measure() {
  local benchmark="$1" measure="$2" value="$3" required="$4"

  if is_number "$value"; then
    printf '%s\t%s\t%s\n' "$benchmark" "$measure" "$value" >> "$METRICS_FILE"
    return
  fi

  if [ "$required" = "required" ]; then
    echo "Missing required metric: ${benchmark}.${measure}" >&2
    exit 1
  fi
}

json_number() {
  if is_number "$1"; then
    echo "$1"
  else
    echo "null"
  fi
}

CG_START=$(extract_ts chaingen "Starting chain generation and persistence")
CG_END=$(extract_ts chaingen "Done. Persisted")
CG_DUR=$(duration_measure "$(diff_seconds "$CG_START" "$CG_END")")
CG_BLOCKS=$(extract_capture chaingen "Done. Persisted" '.*Persisted ([0-9]+).*')
CG_TXS=$(extract_capture chaingen "Done. Persisted" '.*transactions created: ([0-9]+).*')

SN_START=$(extract_ts server-node "Start mode")
SN_READY=$(extract_ts server-node "HTTP API listening")
SN_DUR=$(duration_measure "$(diff_seconds "$SN_START" "$SN_READY")")

CN_BOOT=$(extract_ts client-node-1 "Start mode")
CN_SYNC_START=$(extract_ts client-node-1 "Start synchronizing with the network")
CN_PEER_FOUND=$(extract_ts client-node-1 "Best peer selected: height=")
CN_GENESIS=$(extract_ts client-node-1 "Genesis block successfully stored")
CN_IBD=$(extract_ts client-node-1 "IBD complete")
CN_READY=$(extract_ts client-node-1 "HTTP API listening")

CN_PEER_DISCOVERY=$(duration_measure "$(diff_seconds "$CN_SYNC_START" "$CN_PEER_FOUND")")
CN_GENESIS_DUR=$(duration_measure "$(diff_seconds "$CN_PEER_FOUND" "$CN_GENESIS")")
CN_IBD_DUR=$(duration_measure "$(diff_seconds "$CN_GENESIS" "$CN_IBD")")
CN_SYNC_TOTAL=$(duration_measure "$(diff_seconds "$CN_SYNC_START" "$CN_IBD")")
CN_STARTUP_TOTAL=$(duration_measure "$(diff_seconds "$CN_BOOT" "$CN_READY")")

PEER_HEIGHT=$(extract_capture client-node-1 "Best peer selected: height=" '.*Best peer selected: height=([0-9]+).*')
IBD_BATCHES=$(count_matches_until client-node-1 "$CN_IBD" "IBD batch done")
IBD_TOTAL_MS=$(sum_captures_until client-node-1 "$CN_IBD" "IBD batch done" '.*elapsed_ms=([0-9]+).*')
IBD_APPLIED=$(sum_captures_until client-node-1 "$CN_IBD" "IBD batch done" '.*applied=([0-9]+).*')
IBD_BLOCKS_PER_SEC=""
if is_number "${IBD_TOTAL_MS:-}" && [ "$IBD_TOTAL_MS" -gt 0 ] 2>/dev/null && is_number "${IBD_APPLIED:-}"; then
  IBD_BLOCKS_PER_SEC=$(echo "scale=0; $IBD_APPLIED * 1000 / $IBD_TOTAL_MS" | bc)
fi

CGR_START=$(extract_ts chaingen-reorg "Starting chain generation and persistence")
CGR_END=$(extract_ts chaingen-reorg "Done. Persisted")
CGR_DUR=$(duration_measure "$(diff_seconds "$CGR_START" "$CGR_END")")
CGR_BLOCKS=$(extract_capture chaingen-reorg "Done. Persisted" '.*Persisted ([0-9]+).*')
CGR_TXS=$(extract_capture chaingen-reorg "Done. Persisted" '.*transactions created: ([0-9]+).*')

CN2_START=$(extract_ts client-node-2 "Start mode")
CN2_READY=$(extract_ts client-node-2 "HTTP API listening")
CN2_TIP=$(extract_ts client-node-2 "Publishing initial chain tip")
CN2_DUR=$(duration_measure "$(diff_seconds "$CN2_START" "$CN2_READY")")

SN_REORG_TRIGGER=$(extract_ts server-node "Remote chain heavier")
SN_LCA=$(extract_ts server-node "LCA found at height")
SN_REORG_START=$(extract_ts server-node "Performing reorg")
SN_REORG_DONE=$(extract_ts server-node "Reorg complete")
SN_REORG_SYNC=$(extract_ts server-node "Peer sync complete, synced to height")
SN_REORG_TOTAL=$(duration_measure "$(diff_seconds "$SN_REORG_TRIGGER" "$SN_REORG_SYNC")")
SN_LCA_DUR=$(duration_measure "$(diff_seconds "$SN_REORG_TRIGGER" "$SN_LCA")")
SN_REORG_ENGINE=$(duration_measure "$(diff_seconds "$SN_REORG_START" "$SN_REORG_DONE")")
SN_REWOUND_BLOCKS=$(extract_capture server-node "Rewound" '.*Rewound ([0-9]+).*')
SN_APPLIED_FORKS=$(extract_capture server-node "Reorg complete" '.*applied ([0-9]+).*')

CN_REORG_TRIGGER=$(extract_ts client-node-1 "Remote chain heavier")
CN_FALLBACK=$(extract_ts client-node-1 "Fallback: connected to peer")
CN_REORG_START=$(extract_ts client-node-1 "Performing reorg")
CN_REORG_DONE=$(extract_ts client-node-1 "Reorg complete")
CN_REORG_SYNC=$(extract_ts client-node-1 "Peer sync complete, synced to height")
CN_REORG_TOTAL=$(duration_measure "$(diff_seconds "$CN_REORG_TRIGGER" "$CN_REORG_SYNC")")
CN_FALLBACK_DUR=$(duration_measure "$(diff_seconds "$CN_REORG_TRIGGER" "$CN_FALLBACK")")
CN_REORG_ENGINE=$(duration_measure "$(diff_seconds "$CN_REORG_START" "$CN_REORG_DONE")")

CONVERGE_END=$(service_finished_at wait-for-reorg)
if [ -z "$CONVERGE_END" ]; then
  CONVERGE_END=$(extract_ts wait-for-reorg "SUCCESS.*converged")
fi
CONVERGE_DUR=$(duration_measure "$(diff_seconds "$CN2_TIP" "$CONVERGE_END")")

TOTAL_DUR=$(duration_measure "$(diff_seconds "$CG_START" "$CONVERGE_END")")
if [ -z "$TOTAL_DUR" ]; then
  TOTAL_DUR=$(duration_measure "$(diff_seconds "$CG_START" "$CN_REORG_SYNC")")
fi

add_measure "e2e.chaingen" "duration_s" "$CG_DUR" required
add_measure "e2e.chaingen" "blocks" "$CG_BLOCKS" optional
add_measure "e2e.chaingen" "transactions" "$CG_TXS" optional

add_measure "e2e.server_node" "startup_s" "$SN_DUR" required

add_measure "e2e.client_node" "peer_discovery_s" "$CN_PEER_DISCOVERY" optional
add_measure "e2e.client_node" "genesis_fetch_s" "$CN_GENESIS_DUR" optional
add_measure "e2e.client_node" "ibd_s" "$CN_IBD_DUR" optional
add_measure "e2e.client_node" "sync_total_s" "$CN_SYNC_TOTAL" required
add_measure "e2e.client_node" "startup_total_s" "$CN_STARTUP_TOTAL" optional
add_measure "e2e.client_node" "peer_height" "$PEER_HEIGHT" optional

add_measure "e2e.ibd" "batches" "$IBD_BATCHES" optional
add_measure "e2e.ibd" "blocks_applied" "$IBD_APPLIED" optional
add_measure "e2e.ibd" "consensus_ms" "$IBD_TOTAL_MS" optional
add_measure "e2e.ibd" "blocks_per_sec" "$IBD_BLOCKS_PER_SEC" optional

add_measure "e2e.chaingen_reorg" "duration_s" "$CGR_DUR" required
add_measure "e2e.chaingen_reorg" "blocks" "$CGR_BLOCKS" optional
add_measure "e2e.chaingen_reorg" "transactions" "$CGR_TXS" optional

add_measure "e2e.client_node_2" "startup_s" "$CN2_DUR" required

add_measure "e2e.server_node_reorg" "total_s" "$SN_REORG_TOTAL" required
add_measure "e2e.server_node_reorg" "lca_discovery_s" "$SN_LCA_DUR" optional
add_measure "e2e.server_node_reorg" "engine_s" "$SN_REORG_ENGINE" optional
add_measure "e2e.server_node_reorg" "blocks_rewound" "$SN_REWOUND_BLOCKS" optional
add_measure "e2e.server_node_reorg" "fork_blocks_applied" "$SN_APPLIED_FORKS" optional

add_measure "e2e.client_node_reorg" "total_s" "$CN_REORG_TOTAL" required
add_measure "e2e.client_node_reorg" "fallback_discovery_s" "$CN_FALLBACK_DUR" optional
add_measure "e2e.client_node_reorg" "engine_s" "$CN_REORG_ENGINE" optional

add_measure "e2e.convergence" "duration_s" "$CONVERGE_DUR" optional
add_measure "e2e.total" "wall_clock_s" "$TOTAL_DUR" required

jq -Rn '
  reduce inputs as $line ({};
    if $line == "" then
      .
    else
      ($line | split("\t")) as $parts
      | .[$parts[0]] += {
          ($parts[1]): {
            "value": ($parts[2] | tonumber)
          }
        }
    end
  )
' "$METRICS_FILE" > "$OUTPUT_PATH"

cat > "$BENCHMARK_SUMMARY_PATH" <<ENDSUMMARY
## E2E Benchmark Results

### Overview

| # | Phase | Duration |
|---|-------|----------|
| 1 | Chain generation (${CG_BLOCKS:-?} blocks) | ${CG_DUR:-n/a} |
| 2 | Server node bootstrap | ${SN_DUR:-n/a} |
| 3 | Client node 1 IBD sync | ${CN_SYNC_TOTAL:-n/a} |
| 4 | Chain generation reorg (${CGR_BLOCKS:-?} blocks) | ${CGR_DUR:-n/a} |
| 5 | Client node 2 bootstrap (heavier chain) | ${CN2_DUR:-n/a} |
| 6 | Server node reorg | ${SN_REORG_TOTAL:-n/a} |
| 7 | Client node 1 reorg (fallback) | ${CN_REORG_TOTAL:-n/a} |
| 8 | Network convergence | ${CONVERGE_DUR:-n/a} |
| | **Total wall-clock** | **${TOTAL_DUR:-n/a}** |

### Stage 3 - Client Node 1
| Metric | Value |
|--------|-------|
| Peer discovery | ${CN_PEER_DISCOVERY:-n/a} |
| Genesis fetch & store | ${CN_GENESIS_DUR:-n/a} |
| IBD | ${CN_IBD_DUR:-n/a} |
| Startup total | ${CN_STARTUP_TOTAL:-n/a} |
| Peer height | ${PEER_HEIGHT:-n/a} |

### IBD Performance
| Metric | Value |
|--------|-------|
| Batches | ${IBD_BATCHES:-n/a} |
| Blocks applied | ${IBD_APPLIED:-n/a} |
| Consensus processing | ${IBD_TOTAL_MS:-n/a}ms |
| Throughput | ${IBD_BLOCKS_PER_SEC:-n/a} blocks/s |
ENDSUMMARY

cat > "$BENCHMARK_JSON_PATH" <<ENDJSON
{
  "chaingen": {
    "blocks": $(json_number "${CG_BLOCKS:-}"),
    "transactions": $(json_number "${CG_TXS:-}"),
    "duration_s": $(json_number "${CG_DUR:-}")
  },
  "server_node": {
    "startup_s": $(json_number "${SN_DUR:-}")
  },
  "client_node": {
    "peer_discovery_s": $(json_number "${CN_PEER_DISCOVERY:-}"),
    "genesis_fetch_s": $(json_number "${CN_GENESIS_DUR:-}"),
    "ibd_s": $(json_number "${CN_IBD_DUR:-}"),
    "sync_total_s": $(json_number "${CN_SYNC_TOTAL:-}"),
    "startup_total_s": $(json_number "${CN_STARTUP_TOTAL:-}"),
    "peer_height": $(json_number "${PEER_HEIGHT:-}")
  },
  "ibd": {
    "batches": $(json_number "${IBD_BATCHES:-}"),
    "blocks_applied": $(json_number "${IBD_APPLIED:-}"),
    "consensus_ms": $(json_number "${IBD_TOTAL_MS:-}"),
    "blocks_per_sec": $(json_number "${IBD_BLOCKS_PER_SEC:-}")
  },
  "chaingen_reorg": {
    "blocks": $(json_number "${CGR_BLOCKS:-}"),
    "transactions": $(json_number "${CGR_TXS:-}"),
    "duration_s": $(json_number "${CGR_DUR:-}")
  },
  "client_node_2": {
    "startup_s": $(json_number "${CN2_DUR:-}")
  },
  "server_node_reorg": {
    "total_s": $(json_number "${SN_REORG_TOTAL:-}"),
    "lca_discovery_s": $(json_number "${SN_LCA_DUR:-}"),
    "engine_s": $(json_number "${SN_REORG_ENGINE:-}"),
    "blocks_rewound": $(json_number "${SN_REWOUND_BLOCKS:-}"),
    "fork_blocks_applied": $(json_number "${SN_APPLIED_FORKS:-}")
  },
  "client_node_reorg": {
    "total_s": $(json_number "${CN_REORG_TOTAL:-}"),
    "fallback_discovery_s": $(json_number "${CN_FALLBACK_DUR:-}"),
    "engine_s": $(json_number "${CN_REORG_ENGINE:-}")
  },
  "convergence": {
    "duration_s": $(json_number "${CONVERGE_DUR:-}")
  },
  "total_wall_clock_s": $(json_number "${TOTAL_DUR:-}")
}
ENDJSON
