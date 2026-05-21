#!/usr/bin/env bash

# Copyright 2026 Circle Internet Group, Inc. All rights reserved.
#
# SPDX-License-Identifier: Apache-2.0
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#      http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

GITHUB_ENV_FILE=""
if [[ "${1:-}" == "--write-github-env" ]]; then
  GITHUB_ENV_FILE="${2:?github env file path is required}"
  shift 2
fi

SCENARIO="${1:-${SCENARIO:-crates/quake/scenarios/nightly-tx-propagation.toml}}"
QUIET_PERIOD_SECS="${2:-${QUIET_PERIOD_SECS:-300}}"
WARMUP_DURATION_SECS="${3:-${WARMUP_DURATION_SECS:-60}}"
WARMUP_RATE="${4:-${WARMUP_RATE:-50}}"
RECEIPT_TIMEOUT_SECS="${RECEIPT_TIMEOUT_SECS:-30}"
RPC_TIMEOUT="${RPC_TIMEOUT:-3s}"
FULLNODE_TARGETS="${FULLNODE_TARGETS:-full-blue,full-green,full-purple}"
WARMUP_TARGETS="${WARMUP_TARGETS:-FULLNODES}"
RECEIPT_NODE="${RECEIPT_NODE:-validator1}"
SETUP_EXTRA_ACCOUNTS="${SETUP_EXTRA_ACCOUNTS:-100}"
WARMUP_MAX_ACCOUNTS="${WARMUP_MAX_ACCOUNTS:-90}"
PROBE_ACCOUNT_START="${PROBE_ACCOUNT_START:-90}"
COLLECT_FINAL_STATE="${COLLECT_FINAL_STATE:-true}"
RESULTS_DIR="${RESULTS_DIR:-target/nightly-tx-propagation-results}"

TESTNET_NAME="$(basename "$SCENARIO" .toml)"
QUAKE_DIR=".quake/${TESTNET_NAME}"
COMPOSE_FILE="${QUAKE_DIR}/compose.yaml"
QUAKE="./target/debug/quake"
COMMIT_SHA="$(git rev-parse --short HEAD 2>/dev/null || echo 'unknown')"
STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "ERROR: required command '$1' was not found" >&2
    exit 1
  fi
}

require_uint() {
  local name="$1"
  local value="$2"
  if [[ ! "$value" =~ ^[0-9]+$ ]]; then
    echo "ERROR: $name must be an unsigned integer, got '$value'" >&2
    exit 1
  fi
}

require_uint_between() {
  local name="$1"
  local value="$2"
  local min="$3"
  local max="$4"
  require_uint "$name" "$value"
  if (( value < min || value > max )); then
    echo "ERROR: $name must be between $min and $max, got '$value'" >&2
    exit 1
  fi
}

require_selector_list() {
  local name="$1"
  local value="$2"
  if [[ -z "${value//[ ,]/}" ]]; then
    echo "ERROR: $name is empty" >&2
    exit 1
  fi
  if [[ "$value" =~ [^A-Za-z0-9_.,\ -] ]]; then
    echo "ERROR: $name must contain only node or group names separated by commas or spaces, got '$value'" >&2
    exit 1
  fi
}

require_name() {
  local name="$1"
  local value="$2"
  if [[ ! "$value" =~ ^[A-Za-z0-9_.-]+$ ]]; then
    echo "ERROR: $name must be a single node name, got '$value'" >&2
    exit 1
  fi
}

require_bool() {
  local name="$1"
  local value="$2"
  if [[ "$value" != true && "$value" != false ]]; then
    echo "ERROR: $name must be true or false, got '$value'" >&2
    exit 1
  fi
}

require_workflow_scenario_path() {
  local value="$1"
  if [[ ! "$value" =~ ^crates/quake/scenarios/[A-Za-z0-9._/-]+\.toml$ ]] || [[ "$value" == *..* ]]; then
    echo "ERROR: scenario must be a TOML file under crates/quake/scenarios/, got '$value'" >&2
    exit 1
  fi
}

selector_list_to_csv() {
  local value="$1"
  local values
  read -r -a values <<< "${value//,/ }"
  local IFS=,
  echo "${values[*]}"
}

if [[ ! -f "$SCENARIO" ]]; then
  echo "ERROR: scenario file not found: $SCENARIO" >&2
  exit 1
fi

if [[ -n "$GITHUB_ENV_FILE" ]]; then
  require_workflow_scenario_path "$SCENARIO"
fi

require_uint_between QUIET_PERIOD_SECS "$QUIET_PERIOD_SECS" 0 900
require_uint_between WARMUP_DURATION_SECS "$WARMUP_DURATION_SECS" 1 600
require_uint_between WARMUP_RATE "$WARMUP_RATE" 1 1000
require_uint_between RECEIPT_TIMEOUT_SECS "$RECEIPT_TIMEOUT_SECS" 1 120
require_uint_between SETUP_EXTRA_ACCOUNTS "$SETUP_EXTRA_ACCOUNTS" 1 10000
require_uint_between WARMUP_MAX_ACCOUNTS "$WARMUP_MAX_ACCOUNTS" 0 10000
require_uint_between PROBE_ACCOUNT_START "$PROBE_ACCOUNT_START" 0 10000
require_selector_list FULLNODE_TARGETS "$FULLNODE_TARGETS"
require_selector_list WARMUP_TARGETS "$WARMUP_TARGETS"
require_name RECEIPT_NODE "$RECEIPT_NODE"
require_bool COLLECT_FINAL_STATE "$COLLECT_FINAL_STATE"

read -r -a FULLNODES <<< "${FULLNODE_TARGETS//,/ }"
WARMUP_TARGETS_CSV="$(selector_list_to_csv "$WARMUP_TARGETS")"

needed_accounts=$((PROBE_ACCOUNT_START + ${#FULLNODES[@]}))
if (( needed_accounts > SETUP_EXTRA_ACCOUNTS )); then
  echo "ERROR: SETUP_EXTRA_ACCOUNTS=$SETUP_EXTRA_ACCOUNTS is too small for probe accounts up to index $((needed_accounts - 1))" >&2
  exit 1
fi

if (( WARMUP_MAX_ACCOUNTS > PROBE_ACCOUNT_START )); then
  echo "ERROR: WARMUP_MAX_ACCOUNTS=$WARMUP_MAX_ACCOUNTS overlaps probe account start $PROBE_ACCOUNT_START" >&2
  exit 1
fi

print_config() {
  echo "Configuration:"
  echo "  Scenario:              $SCENARIO"
  echo "  Testnet name:          $TESTNET_NAME"
  echo "  Quake dir:             $QUAKE_DIR"
  echo "  Compose file:          $COMPOSE_FILE"
  echo "  Warmup:                ${WARMUP_DURATION_SECS}s @ ${WARMUP_RATE} tx/s"
  echo "  Warmup targets:        $WARMUP_TARGETS_CSV"
  echo "  Quiet period:          ${QUIET_PERIOD_SECS}s"
  echo "  Fullnodes:             ${FULLNODES[*]}"
  echo "  Receipt node:          $RECEIPT_NODE"
  echo "  Receipt timeout:       ${RECEIPT_TIMEOUT_SECS}s"
  echo "  Probe account start:   $PROBE_ACCOUNT_START"
  echo "  Collect final state:   $COLLECT_FINAL_STATE"
  echo "  Results dir:           $RESULTS_DIR"
}

write_github_env() {
  {
    printf 'SCENARIO=%s\n' "$SCENARIO"
    printf 'TESTNET_NAME=%s\n' "$TESTNET_NAME"
    printf 'QUAKE_DIR=%s\n' "$QUAKE_DIR"
    printf 'COMPOSE_FILE=%s\n' "$COMPOSE_FILE"
    printf 'QUIET_PERIOD_SECS=%s\n' "$QUIET_PERIOD_SECS"
    printf 'WARMUP_DURATION_SECS=%s\n' "$WARMUP_DURATION_SECS"
    printf 'WARMUP_RATE=%s\n' "$WARMUP_RATE"
    printf 'WARMUP_TARGETS=%s\n' "$WARMUP_TARGETS"
    printf 'FULLNODE_TARGETS=%s\n' "$FULLNODE_TARGETS"
    printf 'RECEIPT_TIMEOUT_SECS=%s\n' "$RECEIPT_TIMEOUT_SECS"
  } >> "$GITHUB_ENV_FILE"
}

if [[ -n "$GITHUB_ENV_FILE" ]]; then
  write_github_env
  print_config
  exit 0
fi

require_cmd jq
mkdir -p "$RESULTS_DIR/probes" "$RESULTS_DIR/final-state"

echo "=== Nightly Tx Propagation Test ($(date)) ==="
echo "Repository root: $REPO_ROOT"
print_config
echo ""
echo "To reproduce locally with a short quiet period:"
echo "  bash scripts/scenarios/nightly-tx-propagation.sh $SCENARIO 30"
echo ""

write_probe_result() {
  local node="$1"
  local account_index="$2"
  local success="$3"
  local exit_code="$4"
  local started_at="$5"
  local completed_at="$6"
  local log_path="$7"
  local output_path="$RESULTS_DIR/probes/${node}.json"

  jq -n \
    --arg node "$node" \
    --arg receipt_node "$RECEIPT_NODE" \
    --arg started_at "$started_at" \
    --arg completed_at "$completed_at" \
    --arg log_path "$log_path" \
    --argjson account_index "$account_index" \
    --argjson success "$success" \
    --argjson exit_code "$exit_code" \
    --argjson receipt_timeout_seconds "$RECEIPT_TIMEOUT_SECS" \
    '{
      node: $node,
      receipt_node: $receipt_node,
      account_index: $account_index,
      success: $success,
      exit_code: $exit_code,
      receipt_timeout_seconds: $receipt_timeout_seconds,
      started_at: $started_at,
      completed_at: $completed_at,
      log_path: $log_path
    }' > "$output_path"
}

run_with_timeout() {
  local duration="$1"
  shift

  if command -v timeout >/dev/null 2>&1; then
    timeout -k 10s "$duration" "$@"
  elif command -v gtimeout >/dev/null 2>&1; then
    gtimeout -k 10s "$duration" "$@"
  else
    "$@"
  fi
}

collect_final_state() {
  echo "Collecting final state..."
  mkdir -p "$RESULTS_DIR/final-state"
  if [[ -x "$QUAKE" ]]; then
    run_with_timeout 90s "$QUAKE" -f "$SCENARIO" info heights -n 1 > "$RESULTS_DIR/final-state/heights.txt" 2>&1 || true
  else
    echo "quake binary not found at $QUAKE" > "$RESULTS_DIR/final-state/heights.txt"
  fi
  if [[ -f "$COMPOSE_FILE" ]]; then
    run_with_timeout 30s docker compose -f "$COMPOSE_FILE" ps > "$RESULTS_DIR/final-state/containers.txt" 2>&1 || true
  fi
}

final_state_enabled=false
final_state_collected=false
collect_final_state_once() {
  if [[ "$final_state_enabled" != true ]]; then
    return
  fi
  if [[ "$final_state_collected" == true ]]; then
    return
  fi
  final_state_collected=true
  collect_final_state
}

on_exit() {
  local status=$?
  if [[ "$final_state_enabled" == true && "$final_state_collected" != true ]]; then
    collect_final_state_once
  fi
  exit "$status"
}
trap on_exit EXIT

echo "[1/8] Building (genesis, Docker images, quake)..."
make genesis
make build-docker
cargo build --bin quake
final_state_enabled="$COLLECT_FINAL_STATE"

echo "[2/8] Cleaning previous state..."
"$QUAKE" -f "$SCENARIO" clean --all 2>/dev/null || true

echo "[3/8] Setting up testnet..."
"$QUAKE" -f "$SCENARIO" setup --num-extra-accounts "$SETUP_EXTRA_ACCOUNTS"

echo "[4/8] Starting testnet..."
"$QUAKE" -f "$SCENARIO" start

echo "[5/8] Waiting for network to stabilize..."
"$QUAKE" -f "$SCENARIO" wait height 30 --timeout 120

echo "[6/8] Running warmup load..."
"$QUAKE" -f "$SCENARIO" load \
  --targets "$WARMUP_TARGETS_CSV" \
  -t "$WARMUP_DURATION_SECS" \
  -r "$WARMUP_RATE" \
  --max-num-accounts "$WARMUP_MAX_ACCOUNTS" \
  --show-pool-status \
  --preinit-accounts \
  --reconnect-attempts=5 \
  --reconnect-period=10s \
  2>&1 | tee "$RESULTS_DIR/warmup.log"

echo "[7/8] Quiet period (${QUIET_PERIOD_SECS}s with zero tx load)..."
sleep "$QUIET_PERIOD_SECS"

echo "[8/8] Probing each fullnode..."
failed_probes=0
probe_index=0
for node in "${FULLNODES[@]}"; do
  account_index=$((PROBE_ACCOUNT_START + probe_index))
  probe_index=$((probe_index + 1))
  probe_log="$RESULTS_DIR/probes/${node}.log"
  probe_started_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

  echo "Probing $node with account index $account_index..."
  set +e
  "$QUAKE" -f "$SCENARIO" test tx:transfer \
    --rpc-timeout "$RPC_TIMEOUT" \
    --set "target_node=$node" \
    --set "receipt_node=$RECEIPT_NODE" \
    --set "account_index=$account_index" \
    --set "receipt_timeout_s=$RECEIPT_TIMEOUT_SECS" \
    > "$probe_log" 2>&1
  exit_code=$?
  set -e

  probe_completed_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  if (( exit_code == 0 )); then
    echo "PASS: $node"
    write_probe_result "$node" "$account_index" true "$exit_code" "$probe_started_at" "$probe_completed_at" "$probe_log"
  else
    echo "FAIL: $node (exit code $exit_code)"
    failed_probes=$((failed_probes + 1))
    write_probe_result "$node" "$account_index" false "$exit_code" "$probe_started_at" "$probe_completed_at" "$probe_log"
  fi
done

COMPLETED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
jq -s \
  --arg commit "$COMMIT_SHA" \
  --arg scenario "$SCENARIO" \
  --arg started_at "$STARTED_AT" \
  --arg completed_at "$COMPLETED_AT" \
  --argjson quiet_period_seconds "$QUIET_PERIOD_SECS" \
  --argjson warmup_duration_seconds "$WARMUP_DURATION_SECS" \
  --argjson warmup_rate "$WARMUP_RATE" \
  --argjson receipt_timeout_seconds "$RECEIPT_TIMEOUT_SECS" \
  '{
    commit: $commit,
    scenario: $scenario,
    started_at: $started_at,
    completed_at: $completed_at,
    quiet_period_seconds: $quiet_period_seconds,
    warmup: {
      duration_seconds: $warmup_duration_seconds,
      rate: $warmup_rate
    },
    receipt_timeout_seconds: $receipt_timeout_seconds,
    probes: .,
    probe_count: length,
    all_passed: (map(.success) | all)
  }' "$RESULTS_DIR"/probes/*.json > "$RESULTS_DIR/probe-results.json"

collect_final_state_once

echo ""
echo "=== Probe Results ==="
jq -r '.probes[] | "\(.node): \(if .success then "PASS" else "FAIL" end) (account \(.account_index))"' "$RESULTS_DIR/probe-results.json"
echo "Report: $RESULTS_DIR/probe-results.json"

if (( failed_probes > 0 )); then
  echo "=== Nightly Tx Propagation Test Failed: $failed_probes failure(s) ==="
  exit 1
fi

echo "=== Nightly Tx Propagation Test Complete ($(date)) ==="
