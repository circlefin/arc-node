#!/usr/bin/env bash
#
# Shared Prometheus query helpers.
# Source this file; expects PROM_URL to be set by the caller.
#
# Requires: curl, jq

# Query Prometheus and extract the numeric result.
# Returns "null" if the query fails or returns no data.
prom_query() {
  local query="$1"
  local result

  result=$(curl -s --fail --retry 3 --retry-delay 2 \
    --data-urlencode "query=${query}" \
    "${PROM_URL}/api/v1/query" 2>/dev/null) || { echo "null"; return 0; }

  # Prometheus instant query returns { "data": { "result": [ { "value": [timestamp, "value"] } ] } }
  # For aggregated queries (max, sum), there's one result element.
  # For per-job queries, there may be multiple — we take the first.
  local val
  val=$(echo "$result" | jq -r '.data.result[0].value[1] // empty' 2>/dev/null) || { echo "null"; return 0; }

  if [[ -z "$val" || "$val" == "null" || "$val" == "NaN" ]]; then
    echo "null"
  else
    echo "$val"
  fi
}

# Query Prometheus and return an integer (truncated). Returns "null" on failure.
prom_query_int() {
  local val
  val=$(prom_query "$1")
  if [[ "$val" == "null" ]]; then
    echo "null"
  else
    printf '%.0f' "$val"
  fi
}
