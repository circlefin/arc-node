#!/usr/bin/env bash
# V1 -> V2 storage migration smoke test. Runs against an already-started quake testnet:
# generate load, migrate one node to V2, verify root/balances, then verify the mixed
# V1/V2 network reaches consensus.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

QUAKE="${QUAKE:-cargo run --quiet --bin quake --}"
MANIFEST="${QUAKE_MANIFEST:-crates/quake/scenarios/storage-v2-migration.toml}"
TARGET="${TARGET_NODE:-validator3}"
RATE="${RATE:-100}"
TIME="${TIME:-30}"
NODES=(validator1 validator2 validator3 validator4 validator5)

TESTNET_NAME="$(basename "$MANIFEST" .toml)"
COMPOSE=".quake/${TESTNET_NAME}/compose.yaml"
RESULTS_DIR="${RESULTS_DIR:-target/smoke-results/${TESTNET_NAME}}"
mkdir -p "$RESULTS_DIR"

# Fee-recipient sample addresses; MUST match the manifest.
SAMPLE_ADDRS=(
  0x1111111111111111111111111111111111111111
  0x2222222222222222222222222222222222222222
  0x3333333333333333333333333333333333333333
)

log()  { printf '\n=== %s ===\n' "$*" >&2; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
dec()  { printf '%d' "$1"; }  # bash printf parses 0x.. as hex

# Run a command with a hard time limit; portable (macOS lacks coreutils `timeout`).
bounded() {
  local secs="$1"; shift
  "$@" &
  local pid=$!
  ( sleep "$secs"; kill -KILL "$pid" 2>/dev/null ) &
  local watcher=$!
  wait "$pid" 2>/dev/null
  local rc=$?
  kill -KILL "$watcher" 2>/dev/null
  wait "$watcher" 2>/dev/null
  return "$rc"
}

dump_diagnostics() {
  # shellcheck disable=SC2086
  bounded 30 $QUAKE -f "$MANIFEST" info heights >"$RESULTS_DIR/heights.txt" 2>&1 || true
  bounded 30 docker compose -f "$COMPOSE" ps    >"$RESULTS_DIR/ps.txt"      2>&1 || true
}
on_exit() {
  local rc=$?
  [ "$rc" -ne 0 ] && dump_diagnostics
  exit "$rc"
}
trap on_exit EXIT

# shellcheck disable=SC2086
quake_cmd() { $QUAKE -f "$MANIFEST" "$@"; }

# Published host RPC for a node's EL container.
el_rpc() {
  local node="$1" hostport
  hostport="$(docker compose -f "$COMPOSE" port "${node}_el" 8545 | cut -d: -f2)"
  [ -n "$hostport" ] || fail "could not resolve RPC port for ${node}_el"
  printf 'http://127.0.0.1:%s' "$hostport"
}

# rpc <url> <method> <params-json> -> raw JSON
rpc() {
  curl -s "$1" -H 'content-type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\",\"params\":$3}"
}
block_number() { dec "$(rpc "$1" eth_blockNumber '[]' | jq -r '.result')"; }

# Poll until a node reaches >= want, or fail after timeout seconds.
wait_height() {
  local url="$1" want="$2" deadline=$(( SECONDS + ${3:-180} )) cur=-1
  while (( SECONDS < deadline )); do
    cur="$(block_number "$url" 2>/dev/null || echo -1)"
    (( cur >= want )) && return 0
    sleep 2
  done
  fail "node did not reach height $want within ${3:-180}s (last=$cur)"
}

# Run an arc-node-execution `db` subcommand against the target's (stopped) datadir volume.
node_db() {
  docker compose -f "$COMPOSE" run --rm --no-deps -T \
    --entrypoint /usr/local/bin/arc-node-execution "${TARGET}_el" \
    db --datadir=/data/reth/execution-data --chain=/app/assets/genesis.json "$@"
}

# Persisted storage layout for the target datadir: "true" (V2) or "false" (V1).
storage_is_v2() {
  node_db settings get 2>/dev/null | sed -n -E 's/.*storage_v2: (true|false).*/\1/p'
}

RPC="$(el_rpc "$TARGET")"

log "Phase 1: wait for network to produce blocks"
wait_height "$RPC" 3 120
grep -q -- "--storage.v2=false" "$COMPOSE" || fail "rendered compose lacks --storage.v2=false; nodes would start in V2"

log "Phase 2: generate load (${RATE} tps for ${TIME}s)"
pre_load="$(block_number "$RPC")"
quake_cmd load -r "$RATE" -t "$TIME" 2>&1 | tee "$RESULTS_DIR/load.log"

log "Phase 3: confirm load produced state"
wait_height "$RPC" $(( pre_load + 10 )) 120
log "head advanced from ${pre_load} to $(block_number "$RPC")"

log "Phase 4: stop target CL so its EL freezes at N (the other validators keep producing)"
quake_cmd stop "${TARGET}_cl"
sleep 4  # let the last in-flight block settle; the target's head must be stable for the baseline

log "Phase 5: capture baseline"
head_json="$(rpc "$RPC" eth_getBlockByNumber '["latest",false]')"
N_HEX="$(jq -r '.result.number' <<<"$head_json")"
S="$(jq -r '.result.stateRoot' <<<"$head_json")"
N="$(dec "$N_HEX")"
OLD_HEX="$(printf '0x%x' $(( N / 2 )))"   # an older block to exercise history indices
[ "$N" -gt 1 ] || fail "head N=$N too low to sample an older block"
{
  echo "N=$N"; echo "N_HEX=$N_HEX"; echo "S=$S"; echo "OLD_HEX=$OLD_HEX"
  for a in "${SAMPLE_ADDRS[@]}"; do
    echo "bal_head_${a}=$(rpc "$RPC" eth_getBalance "[\"$a\",\"$N_HEX\"]"  | jq -r .result)"
    echo "bal_old_${a}=$(rpc  "$RPC" eth_getBalance "[\"$a\",\"$OLD_HEX\"]" | jq -r .result)"
  done
} | tee "$RESULTS_DIR/baseline.txt"

log "Phase 6: stop target EL; confirm it is V1 before migrating"
quake_cmd stop "${TARGET}_el"
[ "$(storage_is_v2)" = false ] || fail "target datadir is not V1 before migration"

log "Phase 7: migrate V1 -> V2"
# Exit code (via pipefail) catches failure; the settings check catches a silent no-op.
node_db migrate-v2 2>&1 | tee "$RESULTS_DIR/migrate-v2.log"
[ "$(storage_is_v2)" = true ] || fail "storage_v2 not set after migration"

log "Phase 8: re-run migrate-v2 is an idempotent no-op (exit 0, still V2)"
node_db migrate-v2 2>&1 | tee "$RESULTS_DIR/migrate-v2-noop.log"
[ "$(storage_is_v2)" = true ] || fail "storage_v2 flipped off after no-op re-run"

log "Phase 9: restart target EL alone; wait for rebuild to N"
quake_cmd perturb restart "${TARGET}_el"
quake_cmd wait height "$N" "$TARGET" --timeout 600   # EL rebuilds from local data; no CL needed

log "Phase 10: verify rebuilt state matches baseline"
S2="$(rpc "$RPC" eth_getBlockByNumber "[\"$N_HEX\",false]" | jq -r '.result.stateRoot')"
[ "$S2" = "$S" ] || fail "stateRoot mismatch at N=$N: pre=$S post=$S2"
for a in "${SAMPLE_ADDRS[@]}"; do
  exp_head="$(grep "^bal_head_${a}=" "$RESULTS_DIR/baseline.txt" | cut -d= -f2)"
  got_head="$(rpc "$RPC" eth_getBalance "[\"$a\",\"$N_HEX\"]" | jq -r .result)"
  [ "$got_head" = "$exp_head" ] || fail "balance@head mismatch $a: $exp_head != $got_head"
  exp_old="$(grep "^bal_old_${a}=" "$RESULTS_DIR/baseline.txt" | cut -d= -f2)"
  got_old="$(rpc "$RPC" eth_getBalance "[\"$a\",\"$OLD_HEX\"]" | jq -r .result)"
  [ "$got_old" = "$exp_old" ] || fail "balance@old mismatch $a: $exp_old != $got_old"
done
log "migration correctness verified: stateRoot and balances match at N=$N"

log "Phase 11: restart target CL; network resumes with mixed V1/V2 stores"
quake_cmd perturb restart "${TARGET}_cl"

log "Phase 12: verify all nodes advance past N and agree on stateRoot"
target_M=$(( N + 5 ))
for node in "${NODES[@]}"; do wait_height "$(el_rpc "$node")" "$target_M" 240; done
M_HEX="$(printf '0x%x' "$target_M")"
root_ref=""
for node in "${NODES[@]}"; do
  r="$(rpc "$(el_rpc "$node")" eth_getBlockByNumber "[\"$M_HEX\",false]" | jq -r '.result.stateRoot')"
  [ -n "$r" ] && [ "$r" != null ] || fail "$node missing block $target_M"
  [ -n "$root_ref" ] || root_ref="$r"
  [ "$r" = "$root_ref" ] || fail "stateRoot disagreement at M=$target_M: $node=$r vs $root_ref"
done
log "SUCCESS: mixed V1/V2 network agrees on stateRoot at M=$target_M"
