#!/usr/bin/env bash
# Guard against workspace crates falling out of the coverage sharding matrix.
#
# Every crate in `cargo metadata` workspace_members should appear in some
# shard's `-p` list in .github/workflows/ci.yml, OR in the allowlist below.
#
# When adding a new workspace crate, either:
#   1. Add `-p <name>` to one of the shards in ci.yml, or
#   2. Add its name to ALLOWLIST with a one-line reason.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CI_YAML="$ROOT/.github/workflows/ci.yml"

# Crates that intentionally don't appear in any shard.
# Keep comments aligned with reason so the next reader can audit at a glance.
ALLOWLIST=(
  quake-macros   # proc-macro crate, no tests of its own; exercised transitively via quake
)

workspace_crates=$(
  cargo metadata --manifest-path "$ROOT/Cargo.toml" --no-deps --format-version 1 \
    | jq -r '.packages[].name' \
    | sort -u
)

# Extract every `-p <name>` token from the coverage-shard job block.
# Awk slices the file to the block, grep picks out each -p token, awk emits
# the crate name. Avoids a yq dependency (not installed on every runner).
sharded_crates=$(
  awk '
    /^  coverage-shard:$/        { in_block = 1; next }
    in_block && /^  [a-z][a-z_-]*:$/ { exit }
    in_block
  ' "$CI_YAML" \
    | grep -oE -- '-p [a-zA-Z0-9_-]+' \
    | awk '{print $2}' \
    | sort -u
)

allowlist_crates=$(printf '%s\n' "${ALLOWLIST[@]}" | sort -u)

# Crates present in the workspace but absent from both shards and allowlist.
missing=$(
  comm -23 \
    <(echo "$workspace_crates") \
    <(cat <(echo "$sharded_crates") <(echo "$allowlist_crates") | sort -u)
)

# Crates referenced in the matrix that no longer exist in the workspace.
stale=$(
  comm -23 \
    <(echo "$sharded_crates") \
    <(echo "$workspace_crates")
)

status=0

if [ -n "$missing" ]; then
  echo "error: workspace crates missing from coverage sharding matrix:" >&2
  echo "$missing" | sed 's/^/  - /' >&2
  echo "" >&2
  echo "Either add '-p <crate>' to a shard in $CI_YAML, or add the crate to" >&2
  echo "the ALLOWLIST in $(basename "$0") with a one-line reason." >&2
  status=1
fi

if [ -n "$stale" ]; then
  echo "error: shard '-p' arguments reference crates that no longer exist:" >&2
  echo "$stale" | sed 's/^/  - /' >&2
  echo "" >&2
  echo "Remove these from the matrix in $CI_YAML." >&2
  status=1
fi

if [ "$status" -eq 0 ]; then
  echo "coverage sharding ok: $(echo "$workspace_crates" | wc -l | tr -d ' ') workspace crates, $(echo "$sharded_crates" | wc -l | tr -d ' ') sharded, $(echo "$allowlist_crates" | wc -l | tr -d ' ') allowlisted."
fi

exit "$status"
