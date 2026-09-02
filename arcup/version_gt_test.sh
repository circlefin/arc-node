#!/usr/bin/env bash
#
# Tests for version_gt() in ./arcup (SemVer precedence, semver.org section 11).
#
# Usage:
#   bash arcup/version_gt_test.sh
#
# It sources ./arcup with ARCUP_SKIP_MAIN=1 so only the function definitions load
# (the installer's main flow is skipped) and then exercises version_gt().

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export ARCUP_SKIP_MAIN=1
# shellcheck source=./arcup disable=SC1091
source "$HERE/arcup"
# arcup runs under `set -euo pipefail`; relax it so the harness controls flow.
set +eu +o pipefail 2>/dev/null || true

pass=0
fail=0

# assert_gt A B  -> expect version_gt to report A > B (exit 0)
assert_gt() {
    if version_gt "$1" "$2"; then
        pass=$((pass + 1))
    else
        echo "FAIL: expected '$1' > '$2'"
        fail=$((fail + 1))
    fi
}

# assert_le A B  -> expect version_gt to report NOT A > B (exit non-zero)
assert_le() {
    if version_gt "$1" "$2"; then
        echo "FAIL: expected NOT '$1' > '$2'"
        fail=$((fail + 1))
    else
        pass=$((pass + 1))
    fi
}

# Reported bug (#205): prerelease vs stable and prerelease vs prerelease
assert_gt "0.3.0" "0.3.0-rc.1"
assert_le "0.3.0-rc.1" "0.3.0"
assert_gt "0.3.0-rc.2" "0.3.0-rc.1"
assert_le "0.3.0-rc.1" "0.3.0-rc.2"

# Core major.minor.patch comparisons (with and without 'v' prefix)
assert_gt "1.0.0" "0.9.9"
assert_le "0.9.9" "1.0.0"
assert_gt "v1.2.4" "v1.2.3"
assert_le "1.2.3" "1.2.3"
assert_gt "1.3.0" "1.2.9"
assert_gt "2.0.0-rc.1" "1.9.9"

# Full SemVer 11.4 precedence chain:
# 1.0.0-alpha < 1.0.0-alpha.1 < 1.0.0-alpha.beta < 1.0.0-beta
#            < 1.0.0-beta.2 < 1.0.0-beta.11 < 1.0.0-rc.1 < 1.0.0
assert_gt "1.0.0-alpha.1" "1.0.0-alpha"
assert_gt "1.0.0-alpha.beta" "1.0.0-alpha.1"
assert_gt "1.0.0-beta" "1.0.0-alpha.beta"
assert_gt "1.0.0-beta.2" "1.0.0-beta"
assert_gt "1.0.0-beta.11" "1.0.0-beta.2"
assert_gt "1.0.0-rc.1" "1.0.0-beta.11"
assert_gt "1.0.0" "1.0.0-rc.1"

# Build metadata is ignored for precedence
assert_le "1.0.0+build.9" "1.0.0+build.1"
assert_le "1.0.0" "1.0.0+build.1"

# Build metadata containing a hyphen must NOT be treated as a prerelease
assert_le "1.0.0+build-123" "1.0.0"
assert_le "1.0.0" "1.0.0+build-123"
assert_gt "1.0.1+build-1" "1.0.0"

# Prerelease combined with build metadata: build metadata is ignored, so
# precedence is decided purely by the prerelease identifiers.
assert_le "1.0.0-rc.1+build-123" "1.0.0-rc.1"
assert_le "1.0.0-rc.1" "1.0.0-rc.1+build-123"
assert_gt "1.0.0-rc.2+build-1" "1.0.0-rc.1+build-99"

echo "version_gt: ${pass} passed, ${fail} failed"
[ "${fail}" -eq 0 ]
