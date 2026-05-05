#!/usr/bin/env bash
# Save the cargo cache for the current coverage shard to S3.
#
# Reads SHARD and KEY from the environment. Bundles compiled artifacts and
# the cargo registry/git caches so cargo's mtime-based fingerprint check
# stays consistent across runs. Omitting the registry forces cargo to
# re-fetch sources with fresh mtimes and rebuild every dep on the next run.
#
# Excludes mirror Swatinem/rust-cache: registry/src and git/checkouts are
# derived from cache/ and db/ respectively, so cargo recreates them.
# target/**/incremental is unused (CARGO_INCREMENTAL=0 is the cargo default
# in CI) and only bloats the tarball.
#
# *.profraw / *.profdata / *.info are coverage profile artifacts produced by
# this run. They must NOT be saved: cargo llvm-cov uses --no-clean, so a
# restored profraw from a previous run would fold into the next run's lcov
# and silently bias coverage numbers.
set -euo pipefail

BUCKET=circle-cicd-artifacts
OBJECT_KEY="rust-cache/arc-node-${SHARD}-${KEY}.tar.zst"

TARGETS=()
[ -d "$GITHUB_WORKSPACE/target/llvm-cov-target" ] && TARGETS+=("$GITHUB_WORKSPACE/target/llvm-cov-target")
[ -d "$HOME/.cargo/registry/index" ]              && TARGETS+=("$HOME/.cargo/registry/index")
[ -d "$HOME/.cargo/registry/cache" ]              && TARGETS+=("$HOME/.cargo/registry/cache")
[ -d "$HOME/.cargo/git/db" ]                      && TARGETS+=("$HOME/.cargo/git/db")
if [ ${#TARGETS[@]} -eq 0 ]; then
  echo "nothing to save, skipping"
  exit 0
fi
echo "saving: ${TARGETS[*]}"
du -sh "${TARGETS[@]}" 2>/dev/null || true

tar -c -P \
  --exclude='*/incremental' \
  --exclude='*.profraw' \
  --exclude='*.profdata' \
  --exclude='*.info' \
  "${TARGETS[@]}" \
  | zstd -3 -T0 \
  | aws s3 cp - "s3://$BUCKET/$OBJECT_KEY"

echo "saved cache to S3: $OBJECT_KEY"
