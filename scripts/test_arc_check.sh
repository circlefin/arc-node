#!/usr/bin/env bash
#
# test_arc_check.sh — exercises scripts/arc-check.sh against a mock RPC server.
#
# Env vars controlling scenario (read by the inline python mock):
#   MOCK_IN_SET=1|0          node address is present in validator_set
#   MOCK_ADMIN_DISABLED=1|0  admin_peers returns the -32601 error body
#   MOCK_PM_MISSING="7 8 9"  heights that 404 on /proposal-monitor
#   MOCK_HEIGHT=12           current height (default 12)
#
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ARC_CHECK="${ARC_CHECK_BIN:-$SCRIPT_DIR/arc-check.sh}"

PASS=0
FAIL=0

ok()  { echo "PASS: $1"; PASS=$((PASS + 1)); }
bad() { echo "FAIL: $1"; FAIL=$((FAIL + 1)); }

strip_ansi() { sed -E $'s/\x1b\\[[0-9;]*m//g' <<<"$1"; }

assert_contains() {
  local haystack needle="$2" label="$3"
  haystack=$(strip_ansi "$1")
  if grep -qF -- "$needle" <<<"$haystack"; then ok "$label"; else bad "$label (expected to contain: $needle)"; fi
}

assert_not_contains() {
  local haystack needle="$2" label="$3"
  haystack=$(strip_ansi "$1")
  if grep -qF -- "$needle" <<<"$haystack"; then bad "$label (expected NOT to contain: $needle)"; else ok "$label"; fi
}

MOCK_PID=""
MOCK_PORT=""

start_mock() {
  local port
  port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')

  python3 - "$port" <<'PYEOF' &
import http.server, json, os, sys, re

PORT = int(sys.argv[1])
HEIGHT = int(os.environ.get("MOCK_HEIGHT", "12"))
IN_SET = os.environ.get("MOCK_IN_SET", "1") == "1"
ADMIN_DISABLED = os.environ.get("MOCK_ADMIN_DISABLED", "0") == "1"
MISSING = set(int(x) for x in os.environ.get("MOCK_PM_MISSING", "").split() if x)

MY_ADDR = "arc1myaddress"
OTHER_ADDR = "arc1otheraddress"

VALIDATORS = [
    {"address": OTHER_ADDR, "voting_power": 10,
     "public_key": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
     "public_key_hex": "0x00"},
]
if IN_SET:
    VALIDATORS.append({
        "address": MY_ADDR, "voting_power": 5,
        "public_key": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        "public_key_hex": "0x00",
    })

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _json(self, obj, code=200):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/health":
            self._json({"status": "ok"})
        elif self.path == "/status":
            self._json({
                "height": HEIGHT, "round": 0, "address": MY_ADDR,
                "public_key": "0xaa", "proposer": MY_ADDR,
                "db_latest_height": HEIGHT, "db_earliest_height": 1,
                "sync_state": "synced",
                "validator_set": {
                    "total_voting_power": sum(v["voting_power"] for v in VALIDATORS),
                    "count": len(VALIDATORS),
                    "validators": VALIDATORS,
                },
            })
        elif self.path == "/network-state":
            self._json({
                "local_node": {}, "peers": [
                    {"peer_id": "p1", "p2p_address": "a", "consensus_address": "b",
                     "moniker": "peer1", "peer_type": "full", "connection_direction": "outbound",
                     "score": 1.0, "topics": []},
                ], "persistent_peer_ids": [],
                "persistent_peer_addrs": [],
                "validator_set": {"total_voting_power": 0, "count": 0, "validators": []},
            })
        elif self.path.startswith("/proposal-monitor"):
            m = re.search(r"height=(\d+)", self.path)
            h = int(m.group(1)) if m else -1
            if h in MISSING:
                self._json({"error": "Proposal monitor data not found"}, code=404)
                return
            proposer = MY_ADDR if (IN_SET and h % 4 == 0) else OTHER_ADDR
            self._json({
                "height": h, "proposer": proposer,
                "start_time": "t", "proposal_receive_time": "t",
                "value_id": "v", "successful": True, "synced": True,
                "proposal_delay_ms": 12,
            })
        else:
            self._json({"error": "not found"}, code=404)

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length) if length else b"{}"
        try:
            req = json.loads(body)
        except Exception:
            req = {}
        if req.get("method") == "admin_peers":
            if ADMIN_DISABLED:
                self._json({"jsonrpc": "2.0", "id": req.get("id", 1),
                             "error": {"code": -32601, "message": "Method not found"}})
            else:
                self._json({"jsonrpc": "2.0", "id": req.get("id", 1), "result": [
                    {"name": "peer1", "network": {"inbound": True},
                     "enode": "enode://abc@1.2.3.4:30303"},
                ]})
        else:
            self._json({"jsonrpc": "2.0", "id": req.get("id", 1), "result": None})

http.server.HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
PYEOF
  MOCK_PID=$!
  MOCK_PORT="$port"

  for _ in $(seq 1 50); do
    curl -sf "http://127.0.0.1:${port}/health" >/dev/null 2>&1 && return 0
    sleep 0.1
  done
  echo "mock server failed to start" >&2
  return 1
}

stop_mock() {
  [[ -n "$MOCK_PID" ]] && kill "$MOCK_PID" >/dev/null 2>&1
  wait "$MOCK_PID" 2>/dev/null
  MOCK_PID=""
}

trap 'stop_mock' EXIT

# --- T1: admin namespace disabled -------------------------------------------

echo "=== T1: admin disabled ==="
MOCK_IN_SET=1 MOCK_ADMIN_DISABLED=1 MOCK_HEIGHT=5 MOCK_PM_MISSING="" start_mock
OUT=$("$ARC_CHECK" --cl "http://127.0.0.1:${MOCK_PORT}" --el "http://127.0.0.1:${MOCK_PORT}" 2>&1)
RC=$?
stop_mock

assert_contains "$OUT" "WARN: admin_peers unavailable" "T1 warns about admin_peers"
assert_contains "$OUT" "Proposal History" "T1 report continues to Proposal History"
assert_not_contains "$OUT" "Connected: 0 peers" "T1 does not print misleading 0 peers"
if [[ $RC -eq 0 ]]; then ok "T1 exit code 0"; else bad "T1 exit code 0 (got $RC)"; fi

# --- T2: missing heights -----------------------------------------------------

echo "=== T2: missing heights ==="
MOCK_IN_SET=1 MOCK_ADMIN_DISABLED=0 MOCK_HEIGHT=12 MOCK_PM_MISSING="7 8 9" start_mock
OUT=$("$ARC_CHECK" --cl "http://127.0.0.1:${MOCK_PORT}" 2>&1)
stop_mock

assert_contains "$OUT" "unavailable for 3/12" "T2 reports 3/12 unavailable"
assert_not_contains "$OUT" "All 12 checked" "T2 does not claim all 12 checked"
assert_contains "$OUT" "All 9 checked proposals decided successfully" "T2 reports 9 checked"

# --- T3: not in validator set -------------------------------------------------

echo "=== T3: not in validator set ==="
MOCK_IN_SET=0 MOCK_ADMIN_DISABLED=0 MOCK_HEIGHT=8 MOCK_PM_MISSING="" start_mock
OUT=$("$ARC_CHECK" --cl "http://127.0.0.1:${MOCK_PORT}" 2>&1)
stop_mock

assert_contains "$OUT" "not in validator set (expected" "T3 explains non-validator status"
assert_not_contains "$OUT" "never selected as proposer" "T3 does not print red never-selected verdict"

# --- T4: in set, proposer sometimes us ---------------------------------------

echo "=== T4: in set, some heights ours ==="
MOCK_IN_SET=1 MOCK_ADMIN_DISABLED=0 MOCK_HEIGHT=8 MOCK_PM_MISSING="" start_mock
OUT=$("$ARC_CHECK" --cl "http://127.0.0.1:${MOCK_PORT}" 2>&1)
stop_mock

assert_contains "$OUT" "OURS" "T4 marks our proposed heights"
assert_contains "$OUT" "— ok" "T4 shows ok verdict"

# --- T5: progress bar padding at the boundaries -------------------------------

echo "=== T5: progress bar padding ==="
zero_pad="$(printf '%*s' 0 '' | tr ' ' '#')"
full_pad="$(printf '%*s' 30 '' | tr ' ' '#')"
if [[ -z "$zero_pad" ]]; then ok "T5 zero padding is empty"; else bad "T5 zero padding is empty (got '${zero_pad}')"; fi
if [[ ${#full_pad} -eq 30 ]]; then ok "T5 full padding has length 30"; else bad "T5 full padding has length 30 (got ${#full_pad})"; fi

echo ""
echo "===================="
echo "PASS: $PASS  FAIL: $FAIL"
if [[ $FAIL -gt 0 ]]; then exit 1; fi
exit 0
