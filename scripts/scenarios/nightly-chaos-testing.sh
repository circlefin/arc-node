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

# Change to repository root (script is in scripts/scenarios/)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

echo "=== Nightly Chaos Testing ($(date)) ==="
echo "Repository root: $REPO_ROOT"

# Inputs
SCENARIO="${1:-crates/quake/scenarios/nightly-chaos-testing.toml}"
SPAM_DURATION_SECS="${2:-3600}"
SPAM_RATE="${3:-1000}"
SEED="${4:-}"

# Auto-generate seed if not provided, so every run is reproducible from CI logs.
# Quake uses u64 seeds, so we generate a 64-bit value to match.
if [[ -z "$SEED" ]]; then
  SEED=$(od -An -tu8 -N8 /dev/urandom | tr -d ' ')
fi
# Bash RANDOM uses a Park-Miller LCG with a 31-bit seed. Assigning a u64 value
# directly causes silent overflow and seed collisions. Mask to 31 bits so the
# bash-level PRNG (wait heights, validator selection) is properly seeded.
# The full u64 SEED is still passed to quake via --seed.
RANDOM=$((SEED & 0x7FFFFFFF))

TESTNET_NAME="$(basename "$SCENARIO" .toml)"
QUAKE_DIR=".quake/${TESTNET_NAME}"
NODES_CONFIG="${QUAKE_DIR}/nodes.json"
COMPOSE_FILE="${QUAKE_DIR}/compose.yaml"
RESULTS_DIR="target/nightly-chaos-testing-results"

echo "Configuration:"
echo "  Scenario:     $SCENARIO"
echo "  Nodes config: $NODES_CONFIG"
echo "  Duration:     ${SPAM_DURATION_SECS}s"
echo "  Rate:         $SPAM_RATE tx/s"
echo "  Seed:         $SEED"
echo "  Quake dir:    $QUAKE_DIR"
echo ""
echo "To reproduce this run locally:"
echo "  bash scripts/scenarios/nightly-chaos-testing.sh $SCENARIO $SPAM_DURATION_SECS $SPAM_RATE $SEED"
echo ""

mkdir -p "$RESULTS_DIR"

QUAKE="./target/debug/quake"

echo "[1/6] Building (genesis, Docker images, quake)..."
make genesis
make build-docker

cargo build --bin quake

echo "[2/6] Running quake cleanup..."
"$QUAKE" --seed "$SEED" -f "$SCENARIO" clean --all 2>/dev/null

echo "[3/6] Setting up testnet..."
"$QUAKE" --seed "$SEED" -f "$SCENARIO" setup --num-extra-accounts 1000

echo "[4/6] Starting testnet..."
"$QUAKE" --seed "$SEED" -f "$SCENARIO" start

echo "[5/6] Waiting for network to stabilize..."
"$QUAKE" --seed "$SEED" -f "$SCENARIO" wait height 10 --timeout 60

echo "[6/6] Running spammer and chaos loop in parallel..."


current_height() {
  # Print a single round of heights and take the max numeric value from the first data row.
  #
  # Example:
  # validator1 | validator2 | ...
  # 0 | 0 | ...
  #
  # If some nodes are down it can print "conn refused" in place of numbers; ignore those.
  local line max
  line="$("$QUAKE" --seed "$SEED" -f "$SCENARIO" info heights -n 1 2>/dev/null | tail -n 1)"
  max="$(
    printf '%s\n' "$line" |
      tr '|' ' ' |
      awk 'BEGIN { max = -1 }
      {
        for (i=1;i<=NF;i++) {
          if ($i ~ /^[0-9]+$/ && $i+0 > max) max=$i+0
        }
      }
      END { if (max < 0) exit 1; print max }' 2>/dev/null
  )"

  if [[ -z "${max:-}" ]]; then
    echo "ERROR: failed to determine current height from: $line" >&2
    return 1
  fi
  echo "$max"
}

random_wait_height() {
  local h target
  h="$(current_height)"
  # Choose a random height in [h+5, h+50]
  target=$((h + 5 + (RANDOM % 46)))
  echo "$target"
}

random_valset_args() {
  local n power v i j tmp
  local -a selected args validators

  # Build validators array
  validators=()
  while IFS= read -r v; do
    [ -n "$v" ] && validators+=("$v")
  done <<< "${VALIDATORS_LIST:-}"

  if [ "${#validators[@]}" -lt 1 ]; then
    echo "ERROR: validators array is empty" >&2
    return 1
  fi

  # Pick random number of validators: [1 .. len(validators)]
  n=$((1 + (RANDOM % ${#validators[@]})))

  # Copy validators and shuffle in-place
  selected=( "${validators[@]}" )
  for ((i=${#selected[@]}-1; i>0; i--)); do
    j=$((RANDOM % (i + 1)))
    tmp=${selected[i]}
    selected[i]=${selected[j]}
    selected[j]=$tmp
  done

  # Keep only first n elements
  selected=( "${selected[@]:0:n}" )

  # Build valset args: validator:power
  args=()
  for v in "${selected[@]}"; do
    power=$((RANDOM % 41)) # [0, 40]
    args+=("${v}:${power}")
  done

  printf '%s\n' "${args[@]}"
}

validators_list_from_nodes_json() {
  if [ ! -f "$NODES_CONFIG" ]; then
    echo "ERROR: nodes config not found at $NODES_CONFIG (run 'quake setup' first)" >&2
    return 1
  fi
  if ! command -v jq >/dev/null 2>&1; then
    echo "ERROR: jq is required to parse $NODES_CONFIG" >&2
    return 1
  fi
  jq -r '.[] | select(.name | startswith("validator")) | .name' "$NODES_CONFIG"
}

# exit early if we can't get the validators list
# quake needs it to properly run valset updates in the chaos loop
if ! VALIDATORS_LIST="$(validators_list_from_nodes_json)"; then
  echo "ERROR: failed to get validators list from $NODES_CONFIG" >&2
  exit 1
fi

echo "Starting load test (${SPAM_DURATION_SECS}s @ ${SPAM_RATE} tx/s)..."
echo "Running $QUAKE --seed $SEED -f $SCENARIO load -t $SPAM_DURATION_SECS -r $SPAM_RATE --show-pool-status --preinit-accounts --reconnect-attempts=5 --reconnect-period=10s"

# Logs written to:
# <project-root>/target/nightly-chaos-testing-results/spammer.log
echo "=== Spammer Start ($(date)) ===" > "$RESULTS_DIR/spammer.log"
"$QUAKE" --seed "$SEED" -f "$SCENARIO" load \
  -t "$SPAM_DURATION_SECS" \
  -r "$SPAM_RATE" \
  --show-pool-status \
  --preinit-accounts \
  --reconnect-attempts=5 --reconnect-period=10s \
  &> "$RESULTS_DIR/spammer.log" &

SPAMMER_PID="$!"

# Logs written to:
# <project-root>/target/nightly-chaos-testing-results/chaos_loop.log
chaos_loop() {
  local iter target_height rand_valset
  local -a updates

  iter=0
  while kill -0 "$SPAMMER_PID" 2>/dev/null; do
    iter=$((iter + 1))
    echo "=== Chaos testing iteration #$iter ($(date -u)) ==="

    # 1) perturb chaos (runs ~5 minutes)
    echo "Running: $QUAKE --seed $SEED -f $SCENARIO perturb chaos -d 5m"

    "$QUAKE" --seed "$SEED" -f "$SCENARIO" perturb chaos -d 5m

    # 2) wait for a random height a bit in the future
    target_height="$(random_wait_height)"
    echo "Waiting for height $target_height..."
    echo "Running: $QUAKE --seed $SEED -f $SCENARIO wait height $target_height --timeout 180"

    "$QUAKE" --seed "$SEED" -f "$SCENARIO" wait height "$target_height" --timeout 180

    # 3) random validator set update
    updates=()
    if ! rand_valset="$(random_valset_args)"; then
      echo "ERROR: failed to generate random valset updates" >&2
      return 1
    fi
    while IFS= read -r v; do
      [ -n "$v" ] && updates+=("$v")
    done <<< "$rand_valset"

    echo "Valset updates: ${updates[*]}"
    echo "Running: $QUAKE --seed $SEED -f $SCENARIO valset ${updates[@]}"
    "$QUAKE" --seed "$SEED" -f "$SCENARIO" valset "${updates[@]}"

    sleep 5
  done
}

# Run chaos loop in its own session so we can terminate any in-flight `quake`
# command even if it spawns new process groups.
chaos_loop &> "$RESULTS_DIR/chaos_loop.log" &
CHAOS_PID="$!"

# Monitor both processes
echo "Spammer PID: $SPAMMER_PID"
echo "Quake chaos loop PID: $CHAOS_PID"

MONITOR_COUNT=0
while kill -0 "$SPAMMER_PID" 2>/dev/null && kill -0 "$CHAOS_PID" 2>/dev/null; do

  MONITOR_COUNT=$((MONITOR_COUNT + 1))
  echo "=== Progress Check #$MONITOR_COUNT ($(date)) ==="
  echo "» Spammer still running"
  echo "» Quake chaos loop still running"

  sleep 5
done

# One of them exited, kill the other
if ! kill -0 "$SPAMMER_PID" 2>/dev/null; then
  echo "✗ Spammer exited, killing chaos loop"
  kill -KILL "$CHAOS_PID" 2>/dev/null || true
elif ! kill -0 "$CHAOS_PID" 2>/dev/null; then
  echo "✗ Quake chaos loop exited, killing spammer"
  kill -KILL "$SPAMMER_PID" 2>/dev/null || true
fi

set +e
wait "$SPAMMER_PID"; SPAMMER_EXIT=$?
wait "$CHAOS_PID"; CHAOS_EXIT=$?
set -e

echo "=== Test Completion Summary ==="
echo "Spammer exit code: $SPAMMER_EXIT"
echo "Quake chaos loop exit code: $CHAOS_EXIT"

echo "=== Nightly Chaos Testing End ($(date)) ==="

# Final status
if [ $SPAMMER_EXIT -eq 0 ] && [ $CHAOS_EXIT -eq 0 ]; then
  echo "✓ SUCCESS: All tests completed successfully!"
  exit 0
elif [ $SPAMMER_EXIT -eq 0 ] && [ $CHAOS_EXIT -eq 137 ]; then
  # Spammer finished successfully and chaos was intentionally killed (SIGKILL).
  # Exit code 137 is expected when we terminate the chaos process.
  echo "✓ SUCCESS: All tests completed successfully!"
  exit 0
else
  echo "✗ FAILURE: One or more tests failed"
  exit 1
fi
