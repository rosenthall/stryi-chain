#!/usr/bin/env bash
set -euo pipefail

DC="docker compose -f docker-compose.e2e.yml"

logs_for() {
  $DC logs "$1" 2>/dev/null
}

extract_ts() {
  logs_for "$1" | grep -m1 "$2" \
    | grep -oP '\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z' | head -1
}

diff_seconds() {
  local t1="$1" t2="$2"
  if [ -z "$t1" ] || [ -z "$t2" ]; then
    echo "n/a"
    return
  fi

  local s1 s2
  s1=$(date -d "$t1" +%s.%N 2>/dev/null) || {
    echo "n/a"
    return
  }
  s2=$(date -d "$t2" +%s.%N 2>/dev/null) || {
    echo "n/a"
    return
  }
  printf "%.1fs" "$(echo "$s2 - $s1" | bc)"
}

status() {
  if [ "$1" = "n/a" ]; then
    echo "FAIL"
  else
    echo "OK"
  fi
}

raw_num() {
  # shellcheck disable=SC2001
  echo "$1" | sed 's/s$//' | grep -qP '^\d' && echo "$1" | sed 's/s$//' || echo "null"
}

CG_START=$(extract_ts chaingen "Starting chain generation and persistence")
CG_END=$(extract_ts chaingen "Done. Persisted")
CG_DUR=$(diff_seconds "$CG_START" "$CG_END")
CG_BLOCKS=$(logs_for chaingen | grep "Done. Persisted" | grep -oP 'Persisted \K\d+' || echo "?")
CG_TXS=$(logs_for chaingen | grep "Done. Persisted" | grep -oP 'transactions created: \K\d+' || echo "?")

SN_START=$(extract_ts server-node "Start mode")
SN_READY=$(extract_ts server-node "HTTP API listening")
SN_DUR=$(diff_seconds "$SN_START" "$SN_READY")

CN_BOOT=$(extract_ts client-node-1 "Start mode")
CN_SYNC_START=$(extract_ts client-node-1 "Start synchronizing with the network")
CN_PEER_FOUND=$(extract_ts client-node-1 "peer meta OK")
CN_GENESIS=$(extract_ts client-node-1 "Genesis block successfully stored")
CN_IBD=$(extract_ts client-node-1 "IBD complete")
CN_READY=$(extract_ts client-node-1 "HTTP API listening")

CN_PEER_DISCOVERY=$(diff_seconds "$CN_SYNC_START" "$CN_PEER_FOUND")
CN_GENESIS_DUR=$(diff_seconds "$CN_PEER_FOUND" "$CN_GENESIS")
CN_IBD_DUR=$(diff_seconds "$CN_GENESIS" "$CN_IBD")
CN_SYNC_TOTAL=$(diff_seconds "$CN_SYNC_START" "$CN_IBD")
CN_STARTUP_TOTAL=$(diff_seconds "$CN_BOOT" "$CN_READY")

IBD_BATCHES=$(logs_for client-node-1 | grep -c "IBD batch done" || echo "0")
IBD_TOTAL_MS=$(
  logs_for client-node-1 | grep "IBD batch done" \
    | grep -oP 'elapsed_ms=\K\d+' | paste -sd+ | bc 2>/dev/null || echo "0"
)
IBD_APPLIED=$(
  logs_for client-node-1 | grep "IBD batch done" \
    | grep -oP 'applied=\K\d+' | paste -sd+ | bc 2>/dev/null || echo "0"
)
IBD_BLOCKS_PER_SEC="n/a"
if [ "$IBD_TOTAL_MS" -gt 0 ] 2>/dev/null; then
  IBD_BLOCKS_PER_SEC=$(echo "scale=0; $IBD_APPLIED * 1000 / $IBD_TOTAL_MS" | bc)
fi

PEER_HEIGHT=$(logs_for client-node-1 | grep -m1 "peer meta OK" \
  | grep -oP 'remote_height=\K\d+' || echo "?")

CGR_START=$(extract_ts chaingen-reorg "Starting chain generation and persistence")
CGR_END=$(extract_ts chaingen-reorg "Done. Persisted")
CGR_DUR=$(diff_seconds "$CGR_START" "$CGR_END")
CGR_BLOCKS=$(logs_for chaingen-reorg | grep "Done. Persisted" | grep -oP 'Persisted \K\d+' || echo "?")
CGR_TXS=$(logs_for chaingen-reorg | grep "Done. Persisted" | grep -oP 'transactions created: \K\d+' || echo "?")

CN2_START=$(extract_ts client-node-2 "Start mode")
CN2_READY=$(extract_ts client-node-2 "HTTP API listening")
CN2_TIP=$(extract_ts client-node-2 "Publishing initial chain tip")
CN2_DUR=$(diff_seconds "$CN2_START" "$CN2_READY")

SN_REORG_TRIGGER=$(extract_ts server-node "Remote chain heavier")
SN_LCA=$(extract_ts server-node "LCA found at height")
SN_REORG_START=$(extract_ts server-node "Performing reorg")
SN_REWOUND=$(extract_ts server-node "Rewound.*canonical block")
SN_REORG_DONE=$(extract_ts server-node "Reorg complete")
SN_REORG_SYNC=$(extract_ts server-node "Peer sync complete, synced to height")

SN_REORG_TOTAL=$(diff_seconds "$SN_REORG_TRIGGER" "$SN_REORG_SYNC")
SN_LCA_DUR=$(diff_seconds "$SN_REORG_TRIGGER" "$SN_LCA")
SN_REORG_ENGINE=$(diff_seconds "$SN_REORG_START" "$SN_REORG_DONE")

SN_REWOUND_BLOCKS=$(logs_for server-node | grep -m1 "Rewound" \
  | grep -oP 'Rewound \K\d+' || echo "?")
SN_APPLIED_FORKS=$(logs_for server-node | grep -m1 "Reorg complete" \
  | grep -oP 'applied \K\d+' || echo "?")

CN_REORG_TRIGGER=$(extract_ts client-node-1 "Remote chain heavier")
CN_FALLBACK=$(extract_ts client-node-1 "Fallback: connected to peer")
CN_REORG_LCA=$(extract_ts client-node-1 "LCA found at height")
CN_REORG_START=$(extract_ts client-node-1 "Performing reorg")
CN_REORG_DONE=$(extract_ts client-node-1 "Reorg complete")
CN_REORG_SYNC=$(extract_ts client-node-1 "Peer sync complete, synced to height")

CN_REORG_TOTAL=$(diff_seconds "$CN_REORG_TRIGGER" "$CN_REORG_SYNC")
CN_FALLBACK_DUR=$(diff_seconds "$CN_REORG_TRIGGER" "$CN_FALLBACK")
CN_REORG_ENGINE=$(diff_seconds "$CN_REORG_START" "$CN_REORG_DONE")

CONVERGE_END=$(extract_ts wait-for-reorg "SUCCESS.*converged")
CONVERGE_DUR=$(diff_seconds "$CN2_TIP" "$CONVERGE_END")

TOTAL_DUR=$(diff_seconds "$CG_START" "$CONVERGE_END")
# Fall back to the last reorg-sync marker if convergence logging is missing.
if [ "$TOTAL_DUR" = "n/a" ]; then
  TOTAL_DUR=$(diff_seconds "$CG_START" "$CN_REORG_SYNC")
fi

{
  echo "## E2E Benchmark Results"
  echo ""
  echo "### Overview"
  echo ""
  echo "| # | Phase | Duration | Status |"
  echo "|---|-------|----------|--------|"
  echo "| 1 | Chain generation ($CG_BLOCKS blocks) | $CG_DUR | $(status "$CG_DUR") |"
  echo "| 2 | Server node bootstrap | $SN_DUR | $(status "$SN_DUR") |"
  echo "| 3 | Client node 1 IBD sync | $CN_SYNC_TOTAL | $(status "$CN_SYNC_TOTAL") |"
  echo "| 4 | Chain generation reorg ($CGR_BLOCKS blocks) | $CGR_DUR | $(status "$CGR_DUR") |"
  echo "| 5 | Client node 2 bootstrap (heavier chain) | $CN2_DUR | $(status "$CN2_DUR") |"
  echo "| 6 | Server node reorg | $SN_REORG_TOTAL | $(status "$SN_REORG_TOTAL") |"
  echo "| 7 | Client node 1 reorg (fallback) | $CN_REORG_TOTAL | $(status "$CN_REORG_TOTAL") |"
  echo "| 8 | Network convergence | $CONVERGE_DUR | $(status "$CONVERGE_DUR") |"
  echo "| | **Total wall-clock** | **$TOTAL_DUR** | |"
  echo ""
  echo "### Stage 1 - Chain Generation (canonical)"
  echo "| Metric | Value |"
  echo "|--------|-------|"
  echo "| Blocks | $CG_BLOCKS |"
  echo "| Transactions | $CG_TXS |"
  echo "| Duration | $CG_DUR |"
  echo ""
  echo "### Stage 2 - Server Node (bootstrap)"
  echo "| Metric | Value |"
  echo "|--------|-------|"
  echo "| Start -> API ready | $SN_DUR |"
  echo ""
  echo "### Stage 3 - Client Node 1 (join + IBD from $PEER_HEIGHT blocks)"
  echo "| Phase | Duration |"
  echo "|-------|----------|"
  echo "| Peer discovery | $CN_PEER_DISCOVERY |"
  echo "| Genesis fetch & store | $CN_GENESIS_DUR |"
  echo "| IBD (download + validation) | $CN_IBD_DUR |"
  echo "| **Total sync** | **$CN_SYNC_TOTAL** |"
  echo "| Boot -> API ready | $CN_STARTUP_TOTAL |"
  echo ""
  echo "### IBD Performance"
  echo "| Metric | Value |"
  echo "|--------|-------|"
  echo "| Batches | $IBD_BATCHES |"
  echo "| Blocks applied | $IBD_APPLIED |"
  echo "| Consensus processing | ${IBD_TOTAL_MS}ms |"
  echo "| Throughput | ${IBD_BLOCKS_PER_SEC} blocks/s |"
  echo ""
  echo "### Stage 4 - Chain Generation (divergent fork)"
  echo "| Metric | Value |"
  echo "|--------|-------|"
  echo "| Blocks | $CGR_BLOCKS |"
  echo "| Transactions | $CGR_TXS |"
  echo "| Duration | $CGR_DUR |"
  echo ""
  echo "### Stage 5 - Client Node 2 (bootstrap with heavier chain)"
  echo "| Metric | Value |"
  echo "|--------|-------|"
  echo "| Start -> API ready | $CN2_DUR |"
  echo ""
  echo "### Stage 6 - Server Node Reorg"
  echo "| Phase | Duration |"
  echo "|-------|----------|"
  echo "| Tip received -> LCA found | $SN_LCA_DUR |"
  echo "| Reorg engine (rewind + apply) | $SN_REORG_ENGINE |"
  echo "| Blocks rewound | $SN_REWOUND_BLOCKS |"
  echo "| Fork blocks applied | $SN_APPLIED_FORKS |"
  echo "| **Total (trigger -> sync done)** | **$SN_REORG_TOTAL** |"
  echo ""
  echo "### Stage 7 - Client Node 1 Reorg"
  echo "| Phase | Duration |"
  echo "|-------|----------|"
  echo "| Source peer -> fallback discovery | $CN_FALLBACK_DUR |"
  echo "| Reorg engine (rewind + apply) | $CN_REORG_ENGINE |"
  echo "| **Total (trigger -> sync done)** | **$CN_REORG_TOTAL** |"
  echo ""
  echo "### Stage 8 - Network Convergence"
  echo "| Metric | Value |"
  echo "|--------|-------|"
  echo "| Tip announcement -> all nodes at height 275 | $CONVERGE_DUR |"
  echo ""
  echo "---"
  echo "**Run:** [#${GITHUB_RUN_NUMBER}](${GITHUB_SERVER_URL}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}) | **Branch:** \`${GITHUB_REF_NAME}\` | **Commit:** \`${GITHUB_SHA:0:7}\`"
  echo ""
  echo "*$(date -u +'%Y-%m-%d %H:%M:%S UTC')*"
} >> "$GITHUB_STEP_SUMMARY"

cat > /tmp/benchmark.json <<ENDJSON
{
  "run_id": ${GITHUB_RUN_ID},
  "run_number": ${GITHUB_RUN_NUMBER},
  "branch": "${GITHUB_REF_NAME}",
  "sha": "${GITHUB_SHA}",
  "timestamp": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "chaingen": {
    "blocks": ${CG_BLOCKS:-0},
    "transactions": ${CG_TXS:-0},
    "duration_s": $(raw_num "$CG_DUR")
  },
  "server_node": {
    "startup_s": $(raw_num "$SN_DUR")
  },
  "client_node": {
    "peer_discovery_s": $(raw_num "$CN_PEER_DISCOVERY"),
    "genesis_fetch_s": $(raw_num "$CN_GENESIS_DUR"),
    "ibd_s": $(raw_num "$CN_IBD_DUR"),
    "sync_total_s": $(raw_num "$CN_SYNC_TOTAL"),
    "startup_total_s": $(raw_num "$CN_STARTUP_TOTAL"),
    "peer_height": ${PEER_HEIGHT:-0}
  },
  "ibd": {
    "batches": ${IBD_BATCHES:-0},
    "blocks_applied": ${IBD_APPLIED:-0},
    "consensus_ms": ${IBD_TOTAL_MS:-0},
    "blocks_per_sec": $([ "$IBD_BLOCKS_PER_SEC" = "n/a" ] && echo "null" || echo "$IBD_BLOCKS_PER_SEC")
  },
  "chaingen_reorg": {
    "blocks": ${CGR_BLOCKS:-0},
    "transactions": ${CGR_TXS:-0},
    "duration_s": $(raw_num "$CGR_DUR")
  },
  "client_node_2": {
    "startup_s": $(raw_num "$CN2_DUR")
  },
  "server_node_reorg": {
    "total_s": $(raw_num "$SN_REORG_TOTAL"),
    "lca_discovery_s": $(raw_num "$SN_LCA_DUR"),
    "engine_s": $(raw_num "$SN_REORG_ENGINE"),
    "blocks_rewound": ${SN_REWOUND_BLOCKS:-0},
    "fork_blocks_applied": ${SN_APPLIED_FORKS:-0}
  },
  "client_node_reorg": {
    "total_s": $(raw_num "$CN_REORG_TOTAL"),
    "fallback_discovery_s": $(raw_num "$CN_FALLBACK_DUR"),
    "engine_s": $(raw_num "$CN_REORG_ENGINE")
  },
  "convergence": {
    "duration_s": $(raw_num "$CONVERGE_DUR")
  },
  "total_wall_clock_s": $(raw_num "$TOTAL_DUR")
}
ENDJSON
