#!/usr/bin/env bash
#
# Nightly performance test driver.
# Runs staged load tests (100, 500, 1000 tx/s), collects Prometheus metrics,
# runs end-state health checks, and assembles a JSON report.
#
# Usage: nightly-perf.sh [SCENARIO_TOML]
#
# Environment:
#   RESULTS_DIR   Output directory (default: target/nightly-perf-results)

set -euo pipefail

# Change to repository root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

echo "=== Nightly Performance Test ($(date)) ==="
echo "Repository root: $REPO_ROOT"

# Inputs
SCENARIO="${1:-crates/quake/scenarios/nightly-perf.toml}"

TESTNET_NAME="$(basename "$SCENARIO" .toml)"
RESULTS_DIR="${RESULTS_DIR:-target/nightly-perf-results}"
PROM_URL="${PROM_URL:-http://localhost:9090}"
COMMIT_SHA="$(git rev-parse --short HEAD 2>/dev/null || echo 'unknown')"
TIMESTAMP="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

echo "Configuration:"
echo "  Scenario:       $SCENARIO"
echo "  Results dir:    $RESULTS_DIR"
echo "  Prometheus:     $PROM_URL"
echo "  Commit:         $COMMIT_SHA"
echo ""

mkdir -p "$RESULTS_DIR"

QUAKE="./target/release/quake"

STAGES=(100 500 1000)
STAGE_DURATION=120   # seconds per stage
COOLDOWN=15          # seconds after stage for Prometheus to scrape final data points
DRAIN_PERIOD=30      # seconds after all stages before health checks
SCRAPE_WINDOW="2m"   # PromQL rate() window — covers ~105s of load + 15s cooldown per stage
# No inter-stage bleed: each stage (120s) + cooldown (15s) = 135s > SCRAPE_WINDOW (120s),
# so the 2m window at collection time contains only the completed stage's data.

# --- Build ---
echo "[1/7] Building (genesis, Docker images, quake)..."
make genesis
make build-docker
cargo build --release --bin quake

# --- Clean ---
echo "[2/7] Cleaning previous state..."
"$QUAKE" -f "$SCENARIO" clean --all 2>/dev/null || true

# --- Setup ---
echo "[3/7] Setting up testnet..."
"$QUAKE" -f "$SCENARIO" setup --num-extra-accounts 1000

# --- Start ---
echo "[4/7] Starting testnet..."
"$QUAKE" -f "$SCENARIO" start

# --- Wait for stability ---
echo "[5/7] Waiting for network to stabilize..."
"$QUAKE" -f "$SCENARIO" wait height 10 --timeout 120

# --- Verify Prometheus ---
echo "Verifying Prometheus is responding..."
if ! curl --retry 5 --retry-delay 2 -sf "${PROM_URL}/-/healthy" > /dev/null 2>&1; then
  echo "WARNING: Prometheus health check failed at ${PROM_URL}/-/healthy"
  echo "Continuing anyway -- metrics may be unavailable"
fi

# --- Staged load tests ---
echo "[6/7] Running staged load tests..."

for rate in "${STAGES[@]}"; do
  echo ""
  echo "--- Stage: ${rate} tx/s (${STAGE_DURATION}s) ---"
  stage_log="$RESULTS_DIR/load-stage-${rate}.log"

  set +e
  "$QUAKE" -f "$SCENARIO" load \
    -t "$STAGE_DURATION" \
    -r "$rate" \
    --pools --preinit-accounts \
    &> "$stage_log"
  load_exit=$?
  set -e

  if [[ $load_exit -ne 0 ]]; then
    echo "WARNING: quake load exited with code $load_exit for stage ${rate}"
  fi

  # Parse load summary from log (POSIX-compatible, no grep -P)
  success=$(grep -Eo 'Total sent [0-9]+' "$stage_log" 2>/dev/null | grep -Eo '[0-9]+' || echo "0")
  errors=$(grep -Eo '[0-9]+ failed' "$stage_log" 2>/dev/null | awk '{s+=$1}END{print s+0}' || echo "0")

  echo "Stage ${rate}: sent=${success}, errors=${errors}, exit_code=${load_exit}"

  # Cooldown for Prometheus to scrape final data points
  echo "Cooldown ${COOLDOWN}s..."
  sleep "$COOLDOWN"

  # Collect metrics
  echo "Collecting metrics (window=${SCRAPE_WINDOW})..."
  metrics_file="$RESULTS_DIR/metrics-stage-${rate}.json"

  set +e
  bash scripts/perf/collect-metrics.sh "$PROM_URL" "$SCRAPE_WINDOW" > "$metrics_file"
  metrics_exit=$?
  set -e

  if [[ $metrics_exit -ne 0 ]]; then
    echo "WARNING: Metric collection failed for stage ${rate}"
    echo '{}' > "$metrics_file"
  fi

  # Annotate metrics with load stats
  tmp=$(mktemp)
  jq --arg rate "$rate" \
     --arg success "$success" \
     --arg errors "$errors" \
     --argjson exit_code "$load_exit" \
     '. + {
       target_rate: ($rate | tonumber),
       txs_sent: ($success | tonumber),
       txs_errors: ($errors | tonumber),
       load_exit_code: $exit_code
     }' "$metrics_file" > "$tmp" && mv "$tmp" "$metrics_file"
done

# --- Drain period ---
echo ""
echo "Drain period (${DRAIN_PERIOD}s)..."
sleep "$DRAIN_PERIOD"

# --- End-state health checks ---
echo "[7/7] Running end-state health checks..."
end_state_file="$RESULTS_DIR/end-state.json"

set +e
bash scripts/perf/check-end-state.sh "$PROM_URL" "$SCENARIO" > "$end_state_file"
end_state_exit=$?
set -e

if [[ $end_state_exit -ne 0 ]]; then
  echo "WARNING: One or more end-state checks failed"
fi

# --- Assemble final report ---
echo ""
echo "Assembling final report..."

# Build stages object from individual metric files
stages_json="{}"
for rate in "${STAGES[@]}"; do
  metrics_file="$RESULTS_DIR/metrics-stage-${rate}.json"
  if [[ -f "$metrics_file" ]]; then
    stages_json=$(echo "$stages_json" | jq --arg rate "$rate" --slurpfile m "$metrics_file" \
      '. + {($rate): $m[0]}')
  fi
done

# Assemble report
jq -n \
  --arg commit "$COMMIT_SHA" \
  --arg timestamp "$TIMESTAMP" \
  --arg scenario "$SCENARIO" \
  --argjson stages "$stages_json" \
  --slurpfile end_state "$end_state_file" \
  '{
    commit: $commit,
    timestamp: $timestamp,
    scenario: $scenario,
    stages: $stages,
    end_state: $end_state[0]
  }' > "$RESULTS_DIR/report.json"

# --- Summary ---
echo ""
echo "=== Summary ==="
echo "Report: $RESULTS_DIR/report.json"

for rate in "${STAGES[@]}"; do
  tp=$(jq -r ".stages.\"${rate}\".throughput_tps // \"N/A\"" "$RESULTS_DIR/report.json")
  bt95=$(jq -r ".stages.\"${rate}\".block_time_ms.p95 // \"N/A\"" "$RESULTS_DIR/report.json")
  sent=$(jq -r ".stages.\"${rate}\".txs_sent // \"N/A\"" "$RESULTS_DIR/report.json")
  echo "  Stage ${rate} tx/s: throughput=${tp} tps, block_time_p95=${bt95} ms, sent=${sent} txs"
done

end_pass=$(jq -r '.end_state.all_pass' "$RESULTS_DIR/report.json")
echo "  End-state: all_pass=${end_pass}"

echo ""
echo "=== Nightly Performance Test Complete ($(date)) ==="
