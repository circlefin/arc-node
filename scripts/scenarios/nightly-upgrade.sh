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

echo "=== Nightly Upgrade Test Script ==="
echo "Repository root: $REPO_ROOT"

# Configuration
SCENARIO="${1:-crates/quake/scenarios/nightly-upgrade.toml}"
LOAD_DURATION="${2:-90}"
LOAD_RATE="${3:-100}"
NEXT_HARDFORK_NAME="${4:-zero6}"
UPGRADE_TO_HARDFORK_BLOCKS=60
UPGRADE_TO_HARDFORK_TIMESTAMP=300
HARDFORK_TIMESTAMP=0
GENESIS_FILE=.quake/nightly-upgrade/assets/genesis.json

echo "Configuration:"
echo "  Scenario: $SCENARIO"
echo "  Load duration: ${LOAD_DURATION}s"
echo "  Load rate: ${LOAD_RATE} tx/s"
echo "  Next hardfork name: ${NEXT_HARDFORK_NAME:-none}"

# Step 1: Build
echo "[1/9] Building (genesis, Docker images, quake)..."
make genesis
make build-docker
cargo build -p quake
QUAKE="./target/debug/quake"

# Step 2: Clean up using quake
echo "[2/9] Running quake cleanup..."
$QUAKE -f "$SCENARIO" clean 2>/dev/null || true

# Step 3: Create test results directory
echo "[3/9] Creating test results directory..."
mkdir -p target/test-results

# Step 4: Setup testnet
echo "[4/9] Setting up testnet..."
$QUAKE -f "$SCENARIO" setup

# Set directory permissions for non-root containers
# Published images (ghcr.io) run as 'arc' user (UID 999), need writable directories
# Required on Linux CI where bind mount permissions are strict (unlike macOS Docker Desktop)
TESTNET_NAME=$(basename "$SCENARIO" .toml)
QUAKE_DIR=".quake/${TESTNET_NAME}"
echo "Setting directory permissions in $QUAKE_DIR..."
# Create data directories and set permissions on all
for node_dir in "$QUAKE_DIR"/validator*; do
  echo "Creating directories in $node_dir..."
  mkdir -p "$node_dir/reth" "$node_dir/malachite"
done
echo "Running chmod -R 777 $QUAKE_DIR..."
chmod -R 777 "$QUAKE_DIR"
echo "Directory permissions after chmod:"
ls -la "$QUAKE_DIR"/validator1/

# Step 5: Start testnet
echo "[5/9] Starting testnet..."
$QUAKE start

# Step 6: Wait for network to stabilize
# Timeout needs to be generous because CL nodes may crash-loop while EL finishes
# its latency emulation setup, then need time to catch up once EL is ready.
echo "[6/9] Waiting for network to stabilize..."
$QUAKE wait height 10 --timeout 120

# Step 7: Run parallel tests
echo "[7/9] Running parallel load testing and rolling upgrade..."

# Start load test in background
echo "Starting load test (${LOAD_DURATION}s @ ${LOAD_RATE} tx/s)..."
$QUAKE load -t "$LOAD_DURATION" -r "$LOAD_RATE" --show-pool-status --reconnect-attempts=5 --reconnect-period=10s 2>&1 | tee target/test-results/load_results.txt &
LOAD_PID=$!
echo "Load test PID: $LOAD_PID"

# Give load test time to start
sleep 10

# Start rolling upgrade
echo "Starting rolling upgrade of all validators..."
{
  for node in validator1 validator2 validator3 validator4 validator5; do
    echo "[$(date)] ===== Upgrading $node ====="

    if $QUAKE perturb upgrade "$node"; then
      echo "[$(date)] ✓ Successfully upgraded $node"
    else
      echo "[$(date)] ✗ ERROR: Failed to upgrade $node"
      exit 1
    fi

    echo "[$(date)] Waiting for consensus to settle after upgrading $node..."
    if $QUAKE wait rounds --timeout 60; then
      echo "[$(date)] ✓ Consensus rounds settled after upgrading $node"
    else
      echo "[$(date)] ⚠ WARNING: Consensus rounds did not settle after upgrading $node"
    fi

    echo "[$(date)] Waiting for nodes to sync after upgrading $node..."
    if $QUAKE wait sync --timeout 60 --max-retries 10; then
      echo "[$(date)] ✓ Nodes synced after upgrading $node"
    else
      echo "[$(date)] ⚠ WARNING: Nodes did not sync after upgrading $node"
    fi
  done
  echo "[$(date)] ===== All upgrades completed ====="
} 2>&1 | tee target/test-results/upgrade_results.txt &
UPGRADE_PID=$!
echo "Upgrade test PID: $UPGRADE_PID"

# Monitor upgrade processes
MONITOR_COUNT=0
while kill -0 $UPGRADE_PID 2>/dev/null; do
  sleep 10
  MONITOR_COUNT=$((MONITOR_COUNT + 1))
  echo "=== Progress Check #$MONITOR_COUNT ($(date)) ==="

  if kill -0 $UPGRADE_PID 2>/dev/null; then
    echo "» Upgrade test still running"
  else
    echo "— Upgrade test completed"
  fi
done
wait $UPGRADE_PID
UPGRADE_EXIT=$?

# Step 8: Set the next hardfork height to a future height and restart EL
echo "[8/9] Setting hardfork height and restarting execution nodes..."
get_block_height() {
  cast block-number --rpc-url http://localhost:8545
}

if [ -n "$NEXT_HARDFORK_NAME" ]; then
  HARDFORK_HEIGHT=$(( $(get_block_height) + $UPGRADE_TO_HARDFORK_BLOCKS ))
  HARDFORK_TIMESTAMP=$(( $(date +%s) + $UPGRADE_TO_HARDFORK_TIMESTAMP ))
  echo "Patch $GENESIS_FILE: ${NEXT_HARDFORK_NAME}Block=$HARDFORK_HEIGHT, osakaTime=$HARDFORK_TIMESTAMP"
  cp "$GENESIS_FILE" "$GENESIS_FILE.bak"
  jq ".config.${NEXT_HARDFORK_NAME}Block=$HARDFORK_HEIGHT | .config.osakaTime=$HARDFORK_TIMESTAMP" \
    "$GENESIS_FILE.bak" > "$GENESIS_FILE"
else
  echo "No next hardfork name provided, skipping hardfork application"
  HARDFORK_HEIGHT=0
fi

# Stop ALL CL nodes first, then restart all EL nodes, then start all CL nodes.
# Engine API calls (forkchoice_updated, new_payload) use NoRetry, so any
# transient connection error during an EL restart — even on a *different*
# validator — crashes the CL. Stopping every CL upfront avoids this.
echo "Restarting all nodes..."
{
  echo "[$(date)] ===== Stopping all CL nodes ====="
  $QUAKE stop validator1_cl_u validator2_cl_u validator3_cl_u validator4_cl_u validator5_cl_u
  echo "[$(date)] ✓ All CL nodes stopped"

  echo "[$(date)] ===== Restarting all EL nodes ====="
  for node in validator1_el_u validator2_el_u validator3_el_u validator4_el_u validator5_el_u; do
    echo "[$(date)] ===== Restarting $node ====="
    if $QUAKE perturb restart "$node"; then
      echo "[$(date)] ✓ Successfully restarted $node"
    else
      echo "[$(date)] ✗ ERROR: Failed to restart $node"
      exit 1
    fi
  done
  echo "[$(date)] ✓ All EL nodes restarted"

  echo "[$(date)] ===== Restarting all CL nodes ====="
  for node in validator1_cl_u validator2_cl_u validator3_cl_u validator4_cl_u validator5_cl_u; do
    echo "[$(date)] Restarting $node..."
    if $QUAKE perturb restart "$node"; then
      echo "[$(date)] ✓ Successfully restarted $node"
    else
      echo "[$(date)] ✗ ERROR: Failed to restart $node"
      exit 1
    fi

    echo "[$(date)] Waiting for nodes to sync after restarting $node..."
    if $QUAKE wait sync --timeout 60 --max-retries 10; then
      echo "[$(date)] ✓ Nodes synced after restarting $node"
    else
      echo "[$(date)] ⚠ WARNING: Nodes did not sync after restarting $node"
    fi
  done
  echo "[$(date)] ✓ All CL nodes restarted"

  echo "[$(date)] Waiting for consensus to settle after restarting all nodes..."
  if $QUAKE wait rounds --timeout 60; then
    echo "[$(date)] ✓ Consensus rounds settled after restarting all nodes"
  else
    echo "[$(date)] ⚠ WARNING: Consensus rounds did not settle after restarting all nodes"
  fi

  echo "[$(date)] Waiting for nodes to sync..."
  if $QUAKE wait sync --timeout 120 --max-retries 10; then
    echo "[$(date)] ✓ All nodes are synced"
  else
    echo "[$(date)] ⚠ WARNING: Timeout waiting for nodes to sync"
  fi
  echo "[$(date)] ===== All restarts completed ====="
} 2>&1 | tee target/test-results/restart_results.txt &
RESTART_PID=$!
echo "Restart test PID: $RESTART_PID"

# Step 9: Wait for tests to complete
echo "[9/9] Waiting for tests to complete..."
MONITOR_COUNT=0
while kill -0 $LOAD_PID 2>/dev/null || kill -0 $RESTART_PID 2>/dev/null ; do
  sleep 10
  MONITOR_COUNT=$((MONITOR_COUNT + 1))
  echo "=== Progress Check #$MONITOR_COUNT ($(date)) ==="

  if kill -0 $LOAD_PID 2>/dev/null; then
    echo "» Load test still running"
  else
    echo "— Load test completed"
  fi

  if kill -0 $RESTART_PID 2>/dev/null; then
    echo "» Restart test still running"
  else
    echo "— Restart test completed"
  fi
done
# Wait for both to complete
wait $LOAD_PID
LOAD_EXIT=$?
wait $RESTART_PID
RESTART_EXIT=$?

# Make sure the hardfork is applied
HARDFORK_APPLIED=1
if [ -n "$NEXT_HARDFORK_NAME" ]; then
  TARGET_BLOCK_HEIGHT=$(( $HARDFORK_HEIGHT + 10 ))
  CURR_BLOCK_HEIGHT=$(get_block_height)
  START_BLOCK_HEIGHT=$CURR_BLOCK_HEIGHT
  for i in `seq $(( ($START_BLOCK_HEIGHT + $TARGET_BLOCK_HEIGHT) / 2 + 1 ))`; do
    if [ $CURR_BLOCK_HEIGHT -gt $TARGET_BLOCK_HEIGHT ]; then
      HARDFORK_APPLIED=0 # success
      break
    fi
    if [ $i -gt 10 ] && [ $CURR_BLOCK_HEIGHT -eq $START_BLOCK_HEIGHT ]; then
      echo "Block height stuck for 10 seconds"
      HARDFORK_APPLIED=1 # failed
      break
    fi
    echo "Waiting for hardfork applied... current: $CURR_BLOCK_HEIGHT, hardfork: $HARDFORK_HEIGHT, target: $TARGET_BLOCK_HEIGHT"
    sleep 1
    CURR_BLOCK_HEIGHT=$(get_block_height)
  done
fi

echo "=== Test Completion Summary ==="
echo "Load test exit code: $LOAD_EXIT"
echo "Upgrade test exit code: $UPGRADE_EXIT"
echo "Restart test exit code: $RESTART_EXIT"
if [ -n "$NEXT_HARDFORK_NAME" ]; then
  echo "Hardfork $NEXT_HARDFORK_NAME applied: $HARDFORK_APPLIED"
fi

# Collect final state
echo "=== Final Network State ==="
$QUAKE info heights --number 3 | tee target/test-results/final_heights.txt

# Show results summary
echo "=== Load Test Summary ==="
grep -E "SUCCESS|FAILED|total|rate|transactions" target/test-results/load_results.txt | tail -20 || true

echo "=== Upgrade Test Summary ==="
grep -E "Successfully upgraded" target/test-results/upgrade_results.txt || true
SUCCESSFUL_UPGRADES=$(grep -c "Successfully upgraded" target/test-results/upgrade_results.txt || echo "0")
echo "Successful upgrades: $SUCCESSFUL_UPGRADES / 5"

echo "=== Restart Test Summary ==="
grep -E "Successfully restarted" target/test-results/restart_results.txt || true
SUCCESSFUL_RESTARTS=$(grep -c "Successfully restarted" target/test-results/restart_results.txt || echo "0")
echo "Successful restarts: $SUCCESSFUL_RESTARTS / 10"

# Final status
if [ $LOAD_EXIT -eq 0 ] && [ $UPGRADE_EXIT -eq 0 ] && [ $RESTART_EXIT -eq 0 ] && [ -z "$NEXT_HARDFORK_NAME" -o "$HARDFORK_APPLIED" -eq 0 ]; then
  echo "✓ SUCCESS: All tests completed successfully!"
  exit 0
else
  echo "✗ FAILURE: One or more tests failed"
  exit 1
fi
