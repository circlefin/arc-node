#!/usr/bin/env bash
#
# arc-check.sh — All-in-one validator diagnostic for Arc consensus nodes.
#
# Queries the consensus node's RPC and prints: health, identity,
# consensus state, proposal history (last 50 heights), and peer status.
#
# Dependencies: curl, jq, base64, xxd, bc
#
# Usage: arc-check.sh --cl <CL_RPC_URL> [--el <EL_RPC_URL>] [--live] [--insecure]
# Example: arc-check.sh --cl http://localhost:26658
# Example: arc-check.sh --cl http://localhost:26658 --el http://localhost:8545
# Example: arc-check.sh --cl http://localhost:26658 --live
# Example: arc-check.sh --cl https://node.internal:26658 --insecure

set -euo pipefail

HISTORY_DEPTH=50

# --- helpers ----------------------------------------------------------------

die() { echo "ERROR: $*" >&2; exit 1; }

require() {
  command -v "$1" >/dev/null 2>&1 || die "'$1' is required but not found in PATH"
}

rpc_get() {
  local path="$1"
  curl -sf ${CURL_INSECURE:+"-k"} --max-time 5 "${RPC_URL}${path}" || return 1
}

b64decode() {
  base64 -d 2>/dev/null || base64 -D 2>/dev/null
}

is_positive_int() {
  [[ "$1" =~ ^[0-9]+$ ]] && [[ "$1" -gt 0 ]]
}

bold()  { printf '\033[1m%s\033[0m' "$*"; }
red()   { printf '\033[31m%s\033[0m' "$*"; }
green() { printf '\033[32m%s\033[0m' "$*"; }
yellow(){ printf '\033[33m%s\033[0m' "$*"; }

section() { printf '\n%s\n' "$(bold "=== $1 ===")"; }

# --- args -------------------------------------------------------------------

usage() {
  echo "Usage: arc-check.sh --cl <CL_RPC_URL> [--el <EL_RPC_URL>] [--live] [--insecure]"
  echo "Example: arc-check.sh --cl http://localhost:26658"
  echo "Example: arc-check.sh --cl http://localhost:26658 --el http://localhost:8545"
  echo "Example: arc-check.sh --cl http://localhost:26658 --live"
  echo "Example: arc-check.sh --cl https://node.internal:26658 --insecure"
  echo ""
  echo "Options:"
  echo "  --cl <URL>     Consensus layer RPC URL (required)"
  echo "  --el <URL>     Execution layer RPC URL (optional)"
  echo "  --live         After the report, continuously monitor new proposals"
  echo "  --insecure     Skip TLS certificate verification (for self-signed certs)"
  exit 1
}

RPC_URL=""
EL_RPC_URL=""
LIVE=false
CURL_INSECURE=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --cl) [[ $# -lt 2 ]] && usage; RPC_URL="${2%/}"; shift 2 ;;
    --el) [[ $# -lt 2 ]] && usage; EL_RPC_URL="${2%/}"; shift 2 ;;
    --live) LIVE=true; shift ;;
    --insecure) CURL_INSECURE=1; shift ;;
    *)    usage ;;
  esac
done

[[ -z "$RPC_URL" ]] && usage
require curl
require jq
require base64
require xxd
require bc

# --- health -----------------------------------------------------------------

section "Health"

health=$(rpc_get /health) || die "Cannot reach node at ${RPC_URL}/health"
status=$(echo "$health" | jq -r '.status')
if [[ "$status" == "ok" ]]; then
  echo "Status: $(green "ok")"
else
  echo "Status: $(red "$status")"
fi

# --- status (identity + consensus) ------------------------------------------

status_json=$(rpc_get /status) || die "Failed to fetch /status"

my_address=$(echo "$status_json" | jq -r '.address')
height=$(echo "$status_json" | jq -r '.height')
round=$(echo "$status_json" | jq -r '.round')
proposer=$(echo "$status_json" | jq -r '.proposer')
total_vp=$(echo "$status_json" | jq -r '.validator_set.total_voting_power')
val_count=$(echo "$status_json" | jq -r '.validator_set.count')

# find this node in the validator set
my_vp=$(echo "$status_json" | jq -r --arg addr "$my_address" \
  '.validator_set.validators[] | select(.address == $addr) | .voting_power // empty')
my_pk_raw=$(echo "$status_json" | jq -r --arg addr "$my_address" \
  '.validator_set.validators[] | select(.address == $addr) | .public_key | if type == "object" then .value else . end // empty')
my_pk=""
if [[ -n "$my_pk_raw" ]]; then
  hex=$(echo "$my_pk_raw" | b64decode | xxd -p -c256) && [[ -n "$hex" ]] && my_pk="0x${hex}"
fi

section "Identity"
echo "Address:            $my_address"
if [[ -n "$my_pk" ]]; then
  echo "Public Key:         $my_pk"
  echo "In Validator Set:   $(green "yes")"
  pct=$(echo "scale=1; $my_vp * 100 / $total_vp" | bc)
  echo "Voting Power:       ${my_vp} / ${total_vp} (${pct}%)"
else
  echo "Public Key:         (not in validator set)"
  echo "In Validator Set:   $(red "no")"
  echo "Voting Power:       0 / ${total_vp}"
fi

section "Consensus"
echo "Height:             $height"
if is_positive_int "$round"; then
  echo "Round:              $(red "$round")  ← consensus not settling at round 0"
else
  echo "Round:              $(green "$round")"
fi
echo "Proposer:           $proposer"
if [[ "$proposer" == "$my_address" ]]; then
  echo "Is Proposer:        $(green "yes")"
else
  echo "Is Proposer:        no"
fi
echo "Validators:         $val_count (total power: $total_vp)"

# --- consensus peers --------------------------------------------------------

section "Consensus Layer Peers"

net_json=$(rpc_get /network-state) || die "Failed to fetch /network-state"

peer_count=$(echo "$net_json" | jq '.peers | length')
persistent_count=$(echo "$net_json" | jq '.persistent_peer_ids | length')

echo "Connected: ${peer_count} peers"
echo "$net_json" | jq -r '.peers[] |
  "  \(.moniker // .peer_id)  \(.connection_direction // "unknown")  score=\(.score)  topics=\(.topics)"'

echo ""
echo "Persistent peers: ${persistent_count} configured"

# --- EL peers ---------------------------------------------------------------

if [[ -n "$EL_RPC_URL" ]]; then
  section "Execution Layer Peers"

  el_rpc() {
    local method="$1"
    curl -sf ${CURL_INSECURE:+"-k"} --max-time 5 -X POST -H "Content-Type: application/json" \
      -d "{\"jsonrpc\":\"2.0\",\"method\":\"${method}\",\"params\":[],\"id\":1}" \
      "$EL_RPC_URL" || return 1
  }

  el_peers_json=$(el_rpc "admin_peers") || die "Failed to fetch admin_peers from ${EL_RPC_URL}"
  el_peer_count=$(echo "$el_peers_json" | jq '.result | length')

  echo "Connected: ${el_peer_count} peers"
  echo "$el_peers_json" | jq -r '.result[] |
    "  \(.name // "unknown")  \(if .network.inbound then "inbound" else "outbound" end)  \(.enode | split("@")[1])"'
fi

# --- proposal history -------------------------------------------------------

section "Proposal History (last $HISTORY_DEPTH heights)"

proposed_count=0
failed_count=0
start_height=$((height - HISTORY_DEPTH + 1))
if [[ $start_height -lt 1 ]]; then start_height=1; fi
total_checked=$((height - start_height + 1))

BAR_WIDTH=30

progress_bar() {
  local done=$1 total=$2
  local pct=$((done * 100 / total))
  local filled=$((done * BAR_WIDTH / total))
  local empty=$((BAR_WIDTH - filled))
  printf '\r  [%s%s] %3d%% (%d/%d)' \
    "$(printf '#%.0s' $(seq 1 "$filled") 2>/dev/null)" \
    "$(printf '.%.0s' $(seq 1 "$empty") 2>/dev/null)" \
    "$pct" "$done" "$total" >&2
}

# print a result line, clearing the progress bar first then redrawing it
emit() {
  printf '\r\033[K' >&2
  echo "$1"
  progress_bar "$scan_i" "$total_checked"
}

for (( h = start_height; h <= height; h++ )); do
  scan_i=$((h - start_height + 1))
  progress_bar "$scan_i" "$total_checked"

  pm=$(rpc_get "/proposal-monitor?height=$h" 2>/dev/null) || continue

  pm_proposer=$(echo "$pm" | jq -r '.proposer')
  pm_success=$(echo "$pm" | jq -r '.successful // "null"')
  pm_delay=$(echo "$pm" | jq -r '.proposal_delay_ms // "null"')
  pm_synced=$(echo "$pm" | jq -r '.synced')

  if [[ "$pm_success" != "true" && "$pm_success" != "null" ]]; then
    failed_count=$((failed_count + 1))
    emit "  $(red "Height ${h}"): proposer=${pm_proposer} successful=${pm_success}"
  fi

  if [[ "$pm_proposer" == "$my_address" ]]; then
    proposed_count=$((proposed_count + 1))
    delay_str="delay=${pm_delay}ms"
    [[ "$pm_delay" == "null" ]] && delay_str="delay=n/a"
    synced_str=""
    [[ "$pm_synced" == "true" ]] && synced_str=" (synced)"
    emit "  $(green "Height ${h}"): ${delay_str} successful=${pm_success}${synced_str}  ← $(bold "OURS")"
  fi
done
printf '\r\033[K' >&2

echo ""
if [[ $total_checked -gt 0 ]]; then
  pct=$(echo "scale=1; $proposed_count * 100 / $total_checked" | bc)
else
  pct="0.0"
fi

if [[ $proposed_count -eq 0 ]]; then
  echo "Proposed ${proposed_count}/${total_checked} blocks — $(red "never selected as proposer")"
else
  echo "Proposed ${proposed_count}/${total_checked} blocks (${pct}%) — $(green "ok")"
fi

if [[ $failed_count -eq 0 ]]; then
  echo "All ${total_checked} proposals decided successfully: $(green "yes")"
else
  echo "All ${total_checked} proposals decided successfully: $(red "no") (${failed_count} failed)"
fi

# --- live monitoring --------------------------------------------------------

if $LIVE; then
  section "Live Proposal Monitor"
  echo "Watching for new proposals... (Ctrl+C to stop)"
  echo ""

  last_height="$height"

  while true; do
    sleep 1

    status_json=$(rpc_get /status 2>/dev/null) || continue
    cur_height=$(echo "$status_json" | jq -r '.height')

    # wait for height to advance
    [[ "$cur_height" =~ ^[0-9]+$ ]] || continue
    [[ "$cur_height" -le "$last_height" ]] && continue

    cur_address=$(echo "$status_json" | jq -r '.address')
    cur_round=$(echo "$status_json" | jq -r '.round')

    # print each new height since we last checked
    for (( h = last_height + 1; h <= cur_height; h++ )); do
      pm=$(rpc_get "/proposal-monitor?height=$h" 2>/dev/null) || continue

      pm_proposer=$(echo "$pm" | jq -r '.proposer')
      pm_success=$(echo "$pm" | jq -r '.successful // "null"')
      pm_delay=$(echo "$pm" | jq -r '.proposal_delay_ms // "null"')

      delay_str="${pm_delay}ms"
      [[ "$pm_delay" == "null" ]] && delay_str="n/a"

      ours=""
      [[ "$pm_proposer" == "$cur_address" ]] && ours=" $(bold "[OURS]")"

      if [[ "$pm_success" == "true" ]]; then
        status_str="$(green "ok")"
      elif [[ "$pm_success" == "false" ]]; then
        status_str="$(red "FAILED")"
      else
        status_str="$(yellow "pending")"
      fi

      round_str=""
      [[ "$h" == "$cur_height" ]] && is_positive_int "$cur_round" && round_str=" $(red "round=$cur_round")"

      echo "  Height ${h}: proposer=${pm_proposer} delay=${delay_str} ${status_str}${round_str}${ours}"
    done

    last_height="$cur_height"
  done
fi
