#!/usr/bin/env bash
#
# Run end-state health checks after load drains.
# Outputs a JSON health report to stdout.
# Exit 0 if all checks pass, exit 1 if any fail.
#
# Usage: check-end-state.sh [PROMETHEUS_URL] [SCENARIO_TOML]
#   PROMETHEUS_URL  default: http://localhost:9090
#   SCENARIO_TOML   default: crates/quake/scenarios/nightly-perf.toml
#
# Requires: curl, jq, quake binary at ./target/release/quake

set -euo pipefail

PROM_URL="${1:-http://localhost:9090}"
SCENARIO="${2:-crates/quake/scenarios/nightly-perf.toml}"

QUAKE="./target/release/quake"

# shellcheck source=scripts/perf/prom-lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/prom-lib.sh"

all_pass=true

# --- Queued transactions ---
queued_txs=$(prom_query_int "max(reth_transaction_pool_queued_transactions)")
queued_pass=true
if [[ "$queued_txs" == "null" ]]; then
  queued_pass=false
  all_pass=false
elif [[ "$queued_txs" -ne 0 ]]; then
  queued_pass=false
  all_pass=false
fi

# --- Pending transactions ---
pending_txs=$(prom_query_int "max(reth_transaction_pool_pending_transactions)")
pending_pass=true
if [[ "$pending_txs" == "null" ]]; then
  pending_pass=false
  all_pass=false
elif [[ "$pending_txs" -ne 0 ]]; then
  pending_pass=false
  all_pass=false
fi

# --- Heights in sync (via quake CLI) ---
heights_in_sync=true
height_line=$("$QUAKE" -f "$SCENARIO" info heights -n 1 2>/dev/null | tail -n 1) || height_line=""

if [[ -n "$height_line" ]]; then
  # Parse pipe-separated heights, extract numeric values, check all equal
  heights=()
  while IFS='|' read -ra parts; do
    for part in "${parts[@]}"; do
      trimmed=$(echo "$part" | tr -d ' ')
      if [[ "$trimmed" =~ ^[0-9]+$ ]]; then
        heights+=("$trimmed")
      fi
    done
  done <<< "$height_line"

  if [[ ${#heights[@]} -gt 1 ]]; then
    first="${heights[0]}"
    for h in "${heights[@]:1}"; do
      if [[ "$h" != "$first" ]]; then
        heights_in_sync=false
        all_pass=false
        break
      fi
    done
  else
    heights_in_sync=false
    all_pass=false
  fi
else
  heights_in_sync=false
  all_pass=false
fi

# --- Height restart count ---
# Counter may not exist if no restarts occurred — treat null as zero.
height_restarts=$(prom_query_int "sum(arc_malachite_app_height_restart_count)")
[[ "$height_restarts" == "null" ]] && height_restarts=0
restarts_pass=true
if [[ "$height_restarts" -ne 0 ]]; then
  restarts_pass=false
  all_pass=false
fi

# --- Sync fell behind count ---
# Counter may not exist if no sync-fell-behind events occurred — treat null as zero.
sync_fell_behind=$(prom_query_int "sum(arc_malachite_app_sync_fell_behind_count)")
[[ "$sync_fell_behind" == "null" ]] && sync_fell_behind=0
sync_pass=true
if [[ "$sync_fell_behind" -ne 0 ]]; then
  sync_pass=false
  all_pass=false
fi

# --- Output JSON ---
jq -n \
  --arg queued_txs "$queued_txs" \
  --argjson queued_pass "$queued_pass" \
  --arg pending_txs "$pending_txs" \
  --argjson pending_pass "$pending_pass" \
  --argjson heights_in_sync "$heights_in_sync" \
  --arg height_restarts "$height_restarts" \
  --argjson restarts_pass "$restarts_pass" \
  --arg sync_fell_behind "$sync_fell_behind" \
  --argjson sync_pass "$sync_pass" \
  --argjson all_pass "$all_pass" \
  '{
    queued_txs: { value: ($queued_txs | if . == "null" then null else tonumber end), pass: $queued_pass },
    pending_txs: { value: ($pending_txs | if . == "null" then null else tonumber end), pass: $pending_pass },
    heights_in_sync: { value: $heights_in_sync, pass: $heights_in_sync },
    height_restarts: { value: ($height_restarts | if . == "null" then null else tonumber end), pass: $restarts_pass },
    sync_fell_behind: { value: ($sync_fell_behind | if . == "null" then null else tonumber end), pass: $sync_pass },
    all_pass: $all_pass
  }'

if [[ "$all_pass" == "false" ]]; then
  exit 1
fi
