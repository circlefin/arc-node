#!/usr/bin/env bash
#
# Queries Prometheus HTTP API and outputs a JSON metrics object for one load stage.
#
# Usage: collect-metrics.sh [PROMETHEUS_URL] [SCRAPE_WINDOW]
#   PROMETHEUS_URL  default: http://localhost:9090
#   SCRAPE_WINDOW   default: 2m (PromQL range for rate calculations)
#
# Outputs a JSON object to stdout. All latencies in milliseconds.
# Requires: curl, jq, bc

set -euo pipefail

PROM_URL="${1:-http://localhost:9090}"
WINDOW="${2:-2m}"

# shellcheck source=scripts/perf/prom-lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/prom-lib.sh"

# Query a histogram quantile, take max across all jobs, convert seconds to milliseconds.
prom_histogram_ms() {
  local metric="$1"
  local quantile="$2"
  local val

  val=$(prom_query "max(histogram_quantile(${quantile}, rate(${metric}[${WINDOW}])))")
  if [[ "$val" == "null" ]]; then
    echo "null"
  else
    echo "$val * 1000" | bc -l | xargs printf '%.2f'
  fi
}

# Query a rate metric, take max across all jobs.
prom_rate_max() {
  local metric="$1"
  local val

  val=$(prom_query "max(rate(${metric}[${WINDOW}]))")
  if [[ "$val" == "null" ]]; then
    echo "null"
  else
    printf '%.2f' "$val"
  fi
}

# Query an instant metric (counter/gauge), aggregate with sum.
prom_instant_sum() {
  local metric="$1"
  local val

  val=$(prom_query "sum(${metric})")
  if [[ "$val" == "null" ]]; then
    echo "null"
  else
    printf '%.0f' "$val"
  fi
}

# --- Block time ---
block_time_p50=$(prom_histogram_ms "arc_malachite_app_block_time_bucket" "0.5")
block_time_p95=$(prom_histogram_ms "arc_malachite_app_block_time_bucket" "0.95")
block_time_p99=$(prom_histogram_ms "arc_malachite_app_block_time_bucket" "0.99")

# --- Block finalize time ---
block_finalize_p50=$(prom_histogram_ms "arc_malachite_app_block_finalize_time_bucket" "0.5")
block_finalize_p95=$(prom_histogram_ms "arc_malachite_app_block_finalize_time_bucket" "0.95")

# --- Block build time ---
block_build_p50=$(prom_histogram_ms "arc_malachite_app_block_build_time_bucket" "0.5")
block_build_p95=$(prom_histogram_ms "arc_malachite_app_block_build_time_bucket" "0.95")

# --- Payload total duration ---
payload_total_p50=$(prom_histogram_ms "arc_payload_total_duration_seconds_bucket" "0.5")
payload_total_p95=$(prom_histogram_ms "arc_payload_total_duration_seconds_bucket" "0.95")

# --- Payload stages (p95 per stage) ---
# Stage labels from crates/execution-payload/src/metrics.rs
payload_state_setup_p95=$(prom_histogram_ms 'arc_payload_stage_duration_seconds_bucket{stage="state_setup"}' "0.95")
payload_pre_execution_p95=$(prom_histogram_ms 'arc_payload_stage_duration_seconds_bucket{stage="pre_execution"}' "0.95")
payload_tx_execution_p95=$(prom_histogram_ms 'arc_payload_stage_duration_seconds_bucket{stage="tx_execution"}' "0.95")
payload_post_execution_p95=$(prom_histogram_ms 'arc_payload_stage_duration_seconds_bucket{stage="post_execution"}' "0.95")
payload_assembly_p95=$(prom_histogram_ms 'arc_payload_stage_duration_seconds_bucket{stage="assembly_and_sealing"}' "0.95")

# --- Engine API (aggregate p95) ---
engine_api_p95=$(prom_histogram_ms "arc_malachite_app_engine_api_time_bucket" "0.95")

# --- Throughput (tx/s) ---
throughput=$(prom_rate_max "arc_malachite_app_total_transactions_count")

# --- Counters ---
height_restart_count=$(prom_instant_sum "arc_malachite_app_height_restart_count")
sync_fell_behind_count=$(prom_instant_sum "arc_malachite_app_sync_fell_behind_count")

# --- Output JSON ---
jq -n \
  --arg block_time_p50 "$block_time_p50" \
  --arg block_time_p95 "$block_time_p95" \
  --arg block_time_p99 "$block_time_p99" \
  --arg block_finalize_p50 "$block_finalize_p50" \
  --arg block_finalize_p95 "$block_finalize_p95" \
  --arg block_build_p50 "$block_build_p50" \
  --arg block_build_p95 "$block_build_p95" \
  --arg payload_total_p50 "$payload_total_p50" \
  --arg payload_total_p95 "$payload_total_p95" \
  --arg payload_state_setup_p95 "$payload_state_setup_p95" \
  --arg payload_pre_execution_p95 "$payload_pre_execution_p95" \
  --arg payload_tx_execution_p95 "$payload_tx_execution_p95" \
  --arg payload_post_execution_p95 "$payload_post_execution_p95" \
  --arg payload_assembly_p95 "$payload_assembly_p95" \
  --arg engine_api_p95 "$engine_api_p95" \
  --arg throughput "$throughput" \
  --arg height_restart_count "$height_restart_count" \
  --arg sync_fell_behind_count "$sync_fell_behind_count" \
  '{
    block_time_ms: { p50: ($block_time_p50 | if . == "null" then null else tonumber end), p95: ($block_time_p95 | if . == "null" then null else tonumber end), p99: ($block_time_p99 | if . == "null" then null else tonumber end) },
    block_finalize_ms: { p50: ($block_finalize_p50 | if . == "null" then null else tonumber end), p95: ($block_finalize_p95 | if . == "null" then null else tonumber end) },
    block_build_ms: { p50: ($block_build_p50 | if . == "null" then null else tonumber end), p95: ($block_build_p95 | if . == "null" then null else tonumber end) },
    payload_total_ms: { p50: ($payload_total_p50 | if . == "null" then null else tonumber end), p95: ($payload_total_p95 | if . == "null" then null else tonumber end) },
    payload_stages_ms: { state_setup_p95: ($payload_state_setup_p95 | if . == "null" then null else tonumber end), pre_execution_p95: ($payload_pre_execution_p95 | if . == "null" then null else tonumber end), tx_execution_p95: ($payload_tx_execution_p95 | if . == "null" then null else tonumber end), post_execution_p95: ($payload_post_execution_p95 | if . == "null" then null else tonumber end), assembly_and_sealing_p95: ($payload_assembly_p95 | if . == "null" then null else tonumber end) },
    engine_api_ms: { p95: ($engine_api_p95 | if . == "null" then null else tonumber end) },
    throughput_tps: ($throughput | if . == "null" then null else tonumber end),
    height_restart_count: ($height_restart_count | if . == "null" then null else tonumber end),
    sync_fell_behind_count: ($sync_fell_behind_count | if . == "null" then null else tonumber end)
  }'
