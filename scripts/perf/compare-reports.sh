#!/usr/bin/env bash
#
# Compare current performance report against a rolling average of historical reports.
# Outputs a human-readable summary to stdout and writes comparison.json alongside the report.
#
# Usage: compare-reports.sh <current-report.json> <history-directory>
#
# Thresholds (applied per stage to each tracked metric):
#   REGRESSION (exit 1): any latency metric (p50, p95) >15% worse OR throughput >10% worse OR health check failed
#   WARN:                any latency metric >10% worse OR throughput >5% worse
#   OK:                  within thresholds
#
# Requires: jq, bc

set -euo pipefail

CURRENT_REPORT="${1:?Usage: compare-reports.sh <current-report.json> <history-directory>}"
HISTORY_DIR="${2:?Usage: compare-reports.sh <current-report.json> <history-directory>}"

RESULTS_DIR="$(dirname "$CURRENT_REPORT")"

# Collect up to 5 most recent historical reports
HISTORY_FILES=()
if [[ -d "$HISTORY_DIR" ]]; then
  while IFS= read -r f; do
    [[ -n "$f" ]] && HISTORY_FILES+=("$f")
  done < <(find "$HISTORY_DIR" -name 'report-*.json' -type f | sort | tail -5)
fi

if [[ ${#HISTORY_FILES[@]} -eq 0 ]]; then
  echo "No historical data -- skipping comparison"
  jq -n '{ status: "no_history", message: "No historical data available for comparison" }' \
    > "$RESULTS_DIR/comparison.json"
  exit 0
fi

NUM_HISTORY=${#HISTORY_FILES[@]}

# Extract a metric value from a report JSON. Returns "null" if missing.
extract_metric() {
  local file="$1"
  local path="$2"
  jq -r "$path // null" "$file" 2>/dev/null || echo "null"
}

# Compute average of a metric across historical files. Skips nulls.
compute_avg() {
  local path="$1"
  local sum=0 count=0

  for f in "${HISTORY_FILES[@]}"; do
    local val
    val=$(extract_metric "$f" "$path")
    if [[ "$val" != "null" && "$val" != "" ]]; then
      sum=$(echo "$sum + $val" | bc -l)
      count=$((count + 1))
    fi
  done

  if [[ $count -eq 0 ]]; then
    echo "null"
  else
    echo "$sum / $count" | bc -l | xargs printf '%.2f'
  fi
}

# Compute percent change: (current - avg) / avg * 100
# Positive = worse for latency, negative = worse for throughput
pct_change() {
  local current="$1"
  local avg="$2"

  if [[ "$current" == "null" || "$avg" == "null" || "$avg" == "0" || "$avg" == "0.00" ]]; then
    echo "null"
    return
  fi

  echo "($current - $avg) / $avg * 100" | bc -l | xargs printf '%.1f'
}

# Determine status for a latency metric (higher is worse)
latency_status() {
  local pct="$1"
  if [[ "$pct" == "null" ]]; then
    echo "SKIP"
    return
  fi
  # Positive pct = higher latency = worse
  if (( $(echo "$pct > 15" | bc -l) )); then
    echo "REGRESSION"
  elif (( $(echo "$pct > 10" | bc -l) )); then
    echo "WARN"
  else
    echo "OK"
  fi
}

# Determine status for a throughput metric (lower is worse)
throughput_status() {
  local pct="$1"
  if [[ "$pct" == "null" ]]; then
    echo "SKIP"
    return
  fi
  # Negative pct = lower throughput = worse
  if (( $(echo "$pct < -10" | bc -l) )); then
    echo "REGRESSION"
  elif (( $(echo "$pct < -5" | bc -l) )); then
    echo "WARN"
  else
    echo "OK"
  fi
}

# Track overall result
overall="PASS"
has_regression=false

# Stages in the report
STAGES=(100 500 1000)

# Metrics to compare: (json_path, display_name, type)
# type: "latency" or "throughput"
STAGE_METRICS=(
  ".block_time_ms.p50|block_time_p50|latency"
  ".block_time_ms.p95|block_time_p95|latency"
  ".block_finalize_ms.p95|block_finalize_p95|latency"
  ".block_build_ms.p95|block_build_p95|latency"
  ".payload_total_ms.p95|payload_total_p95|latency"
  ".engine_api_ms.p95|engine_api_p95|latency"
  ".throughput_tps|throughput|throughput"
)

# Get commit and date from current report
commit=$(jq -r '.commit // "unknown"' "$CURRENT_REPORT")
report_date=$(jq -r '.timestamp // "unknown"' "$CURRENT_REPORT" | cut -c1-10)

echo "=== Nightly Performance Report (${report_date}) ==="
echo "Commit: ${commit}"
echo "Compared against: rolling average of ${NUM_HISTORY} previous run(s)"
echo ""

# Build comparison JSON
comparison_entries="[]"

for stage in "${STAGES[@]}"; do
  echo "Stage: ${stage} tx/s"

  for entry in "${STAGE_METRICS[@]}"; do
    IFS='|' read -r json_path display_name metric_type <<< "$entry"

    current_val=$(extract_metric "$CURRENT_REPORT" ".stages.\"${stage}\"${json_path}")
    avg_val=$(compute_avg ".stages.\"${stage}\"${json_path}")
    pct=$(pct_change "$current_val" "$avg_val")

    if [[ "$metric_type" == "latency" ]]; then
      status=$(latency_status "$pct")
      unit="ms"
    else
      status=$(throughput_status "$pct")
      unit="tps"
    fi

    if [[ "$status" == "REGRESSION" ]]; then
      has_regression=true
      overall="FAIL"
    elif [[ "$status" == "WARN" && "$overall" != "FAIL" ]]; then
      overall="WARN"
    fi

    # Format output line
    if [[ "$current_val" == "null" || "$avg_val" == "null" ]]; then
      printf "  %-24s %8s (no data)                 %s\n" "${display_name}:" "${current_val}" "$status"
    else
      printf "  %-24s %8s${unit} (avg: %s${unit}, %+s%%)  %s\n" \
        "${display_name}:" "$current_val" "$avg_val" "$pct" "$status"
    fi

    # Append to comparison JSON
    comparison_entries=$(echo "$comparison_entries" | jq \
      --arg stage "$stage" \
      --arg metric "$display_name" \
      --arg current "$current_val" \
      --arg avg "$avg_val" \
      --arg pct "$pct" \
      --arg status "$status" \
      '. + [{stage: $stage, metric: $metric, current: $current, average: $avg, pct_change: $pct, status: $status}]')
  done
  echo ""
done

# End-state checks
echo "End-state:"
end_state_pass=$(jq -r '.end_state.all_pass // false' "$CURRENT_REPORT" 2>/dev/null || echo "false")

for check in queued_txs pending_txs heights_in_sync height_restarts sync_fell_behind; do
  val=$(jq -r ".end_state.${check}.value // \"null\"" "$CURRENT_REPORT")
  pass=$(jq -r ".end_state.${check}.pass // false" "$CURRENT_REPORT")

  if [[ "$pass" == "false" ]]; then
    status="FAIL"
    has_regression=true
    overall="FAIL"
  else
    status="OK"
  fi
  printf "  %-24s %-8s %s\n" "${check}:" "$val" "$status"
done

echo ""

if [[ "$has_regression" == "true" ]]; then
  echo "Result: FAIL (regression detected)"
else
  echo "Result: ${overall}"
fi

# Write comparison JSON
jq -n \
  --arg status "$overall" \
  --arg commit "$commit" \
  --arg date "$report_date" \
  --argjson num_history "$NUM_HISTORY" \
  --argjson entries "$comparison_entries" \
  --argjson end_state_pass "$end_state_pass" \
  '{
    status: $status,
    commit: $commit,
    date: $date,
    num_history: $num_history,
    end_state_pass: $end_state_pass,
    metrics: $entries
  }' > "$RESULTS_DIR/comparison.json"

if [[ "$has_regression" == "true" ]]; then
  exit 1
fi
