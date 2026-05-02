#!/usr/bin/env bash
# Restore the cargo cache for the current coverage shard from S3.
#
# Reads SHARD and KEY from the environment. On any failure (miss, tiny
# object, download error, archive corruption, partial extract), exits 0
# with the touched paths wiped so the surrounding workflow proceeds with
# a clean cold build instead of a half-restored cache.
#
# Extracts with absolute-path tar (-P) so entries land at their original
# locations: $GITHUB_WORKSPACE/target/... and $HOME/.cargo/{registry,git}/...
set -euo pipefail

BUCKET=circle-cicd-artifacts
OBJECT_KEY="rust-cache/arc-node-${SHARD}-${KEY}.tar.zst"

# Paths the archive will populate. Kept in sync with s3-cache-save.sh; used
# for cleanup if extraction aborts mid-way.
RESTORE_PATHS=(
  "$GITHUB_WORKSPACE/target/llvm-cov-target"
  "$HOME/.cargo/registry/index"
  "$HOME/.cargo/registry/cache"
  "$HOME/.cargo/git/db"
)

wipe_partial_restore() {
  for p in "${RESTORE_PATHS[@]}"; do
    rm -rf "$p"
  done
}

echo "HOME=$HOME"
echo "GITHUB_WORKSPACE=$GITHUB_WORKSPACE"

if ! aws s3api head-object --bucket "$BUCKET" --key "$OBJECT_KEY" >/dev/null 2>&1; then
  echo "S3 cache miss (cold build): $OBJECT_KEY"
  exit 0
fi

SIZE=$(aws s3api head-object --bucket "$BUCKET" --key "$OBJECT_KEY" --query ContentLength --output text)
echo "S3 cache hit: $OBJECT_KEY (size=$SIZE bytes)"

# A valid cache is hundreds of MB; reject obviously-truncated objects.
# A prior bug once saved a near-0-byte object that fooled the restore path.
if [ "$SIZE" -lt 10485760 ]; then
  echo "object smaller than 10 MB, treating as cold miss"
  exit 0
fi

# Download first, validate, then extract. Streaming directly into / is faster
# but would leave partially-restored cache dirs if zstd or tar fails mid-way,
# which would poison the next cargo build with ghost .rlib / fingerprint files.
TMP_TAR=$(mktemp --tmpdir="${RUNNER_TEMP:-/tmp}" s3cache.XXXXXXXX.tar.zst)
trap 'rm -f "$TMP_TAR"' EXIT

echo "downloading archive to $TMP_TAR..."
if ! aws s3 cp "s3://$BUCKET/$OBJECT_KEY" "$TMP_TAR"; then
  echo "download failed, treating as cold miss"
  exit 0
fi

echo "validating archive integrity..."
if ! zstd -t "$TMP_TAR" >/dev/null 2>&1; then
  echo "archive failed zstd integrity check, treating as cold miss"
  exit 0
fi

echo "extracting..."
if ! zstd -dc "$TMP_TAR" | tar -x -P -C /; then
  echo "extract failed after validation; wiping partial restore and falling back to cold build"
  wipe_partial_restore
  exit 0
fi

echo "restored target + cargo registry from S3"
du -sh "$GITHUB_WORKSPACE/target/llvm-cov-target" 2>/dev/null || true
du -sh "$HOME/.cargo/registry" "$HOME/.cargo/git" 2>/dev/null || true
