#!/usr/bin/env bash
set -euo pipefail

# Creates a release archive with checksums from compiled binaries.
# Usage: ./scripts/release-package.sh <TAG> [TARGET]
# Output: release-assets/arc-node-<TAG>-<TARGET>.tar.gz{,.sha256}

TAG="${1:?Usage: release-package.sh <TAG> [TARGET]}"
TARGET="${2:-$(rustc -vV | awk '/^host:/ {print $2}')}"

BINARIES=(arc-node-execution arc-node-consensus arc-snapshots)
BUILD_DIR="target/release"
OUT_DIR="release-assets"
ARCHIVE_NAME="arc-node-${TAG}-${TARGET}.tar.gz"

mkdir -p "$OUT_DIR"

for bin in "${BINARIES[@]}"; do
    if [[ ! -f "$BUILD_DIR/$bin" ]]; then
        echo "error: $BUILD_DIR/$bin not found — run 'cargo build --release' first" >&2
        exit 1
    fi
done

# Create archive with flat layout (no nested directories)
tar -czf "$OUT_DIR/$ARCHIVE_NAME" -C "$BUILD_DIR" "${BINARIES[@]}"

# Generate checksum in GNU coreutils format: "<hash>  <filename>"
cd "$OUT_DIR"
if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$ARCHIVE_NAME" > "${ARCHIVE_NAME}.sha256"
else
    shasum -a 256 "$ARCHIVE_NAME" > "${ARCHIVE_NAME}.sha256"
fi

echo "Packaged: $OUT_DIR/$ARCHIVE_NAME"
echo "Checksum: $OUT_DIR/${ARCHIVE_NAME}.sha256"
cat "${ARCHIVE_NAME}.sha256"
