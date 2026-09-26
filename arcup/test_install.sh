#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_TMP="$(mktemp -d)"
export ARCUP_INSTALL_SKIP_MAIN=1
export ARC_DIR="$TEST_TMP/arc"
export ARC_BIN_DIR="$ARC_DIR/bin"

# shellcheck source=install
source "$SCRIPT_DIR/install"
trap 'rm -rf "$TEST_TMP"' EXIT

# curl 8.14.0/8.14.1 can print an HTTP error but return success when
# --retry and --fail are combined. Reproduce that contract violation by
# returning 0 without writing the requested output file.
curl_with_headers() {
    return 0
}

out="$TEST_TMP/install.out"
if (main) >"$out" 2>&1; then
    echo "FAIL: empty bootstrap download was accepted" >&2
    cat "$out" >&2
    exit 1
fi

if ! grep -q "failed to download arcup" "$out"; then
    echo "FAIL: expected bootstrap download error was not reported" >&2
    cat "$out" >&2
    exit 1
fi

if [[ -e "$ARC_BIN_DIR/arcup" ]]; then
    echo "FAIL: empty arcup executable was installed" >&2
    exit 1
fi

echo "PASS: empty bootstrap download is rejected"
