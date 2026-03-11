#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
FIXTURE_DIR="${ROOT_DIR}/fixtures/benchmark-logs"
OUTPUT_PATH="$(mktemp)"
BENCHMARK_JSON_PATH="$(mktemp)"
BENCHMARK_SUMMARY_PATH="$(mktemp)"

cleanup() {
  rm -f "$OUTPUT_PATH"
  rm -f "$BENCHMARK_JSON_PATH"
  rm -f "$BENCHMARK_SUMMARY_PATH"
}
trap cleanup EXIT

BENCHMARK_LOG_DIR="$FIXTURE_DIR" \
BENCHMARK_OUTPUT_PATH="$OUTPUT_PATH" \
BENCHMARK_JSON_PATH="$BENCHMARK_JSON_PATH" \
BENCHMARK_SUMMARY_PATH="$BENCHMARK_SUMMARY_PATH" \
bash "${ROOT_DIR}/scripts/collect-benchmark.sh"

jq -e '.["e2e.client_node"].peer_height.value == 250' "$OUTPUT_PATH" >/dev/null
jq -e '.["e2e.ibd"].batches.value == 3' "$OUTPUT_PATH" >/dev/null
jq -e '.["e2e.ibd"].blocks_applied.value == 250' "$OUTPUT_PATH" >/dev/null
jq -e '.["e2e.total"].wall_clock_s.value == 25.4' "$OUTPUT_PATH" >/dev/null
jq -e '.client_node.peer_height == 250' "$BENCHMARK_JSON_PATH" >/dev/null
jq -e '.ibd.batches == 3' "$BENCHMARK_JSON_PATH" >/dev/null
grep -q "Stage 3 - Client Node 1" "$BENCHMARK_SUMMARY_PATH"
