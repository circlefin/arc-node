#!/usr/bin/env bash

# Copyright 2026 Circle Internet Group, Inc. All rights reserved.
#
# SPDX-License-Identifier: Apache-2.0
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#      http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.


# Nightly job: generate random quake manifests, run full test suite on each,
# and produce a report for any error that is not expected for a valid manifest
# (i.e. setup/start failures; test assertion failures are considered expected).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

GENERATED_DIR="target/nightly-random-manifests/manifests"
REPORT_DIR="target/nightly-random-manifests"
QUAKE="./target/debug/quake"
COUNT="${QUAKE_GENERATE_COUNT:-3}"

echo "=== Nightly Random Manifests ($(date)) ==="
echo "Repository root: $REPO_ROOT"
echo "Generated manifests dir: $GENERATED_DIR"
echo "Seed: ${QUAKE_GENERATE_SEED:-<random>}"
echo "Count: $COUNT"

mkdir -p "$REPORT_DIR"
mkdir -p "$GENERATED_DIR"

record_failure() {
  local manifest="$1"
  local phase="$2"
  local log_file="$3"

  echo ""
  echo "--- Failure ---"
  echo "Manifest: $manifest"
  echo "Phase: $phase"
  echo "Last 80 lines of log:"
  echo "-------------------"
  tail -n 80 "$log_file" || echo "(no log)"
  echo "-------------------"
  echo ""
}

errors_occurred=0

run_phase() {
  local manifest="$1"
  local phase="$2"
  echo "PHASE $phase: quake ${@:3}"
  shift 2
  local log_file="${REPORT_DIR}/phase_$(basename "$manifest" .toml)_${phase}.log"
  if "$QUAKE" -f "$manifest" "$@" > "$log_file" 2>&1; then
    return 0
  fi
  # `((...))` exits with status 1 when the result is 0 (i.e. first increment),
  # which would trigger set -e. The `|| true` suppresses that spurious failure.
  ((errors_occurred++)) || true
  record_failure "$manifest" "$phase" "$log_file"
  return 1
}

echo "[1/3] Building (genesis, Docker images, quake)..."
make genesis build-docker
cargo build --bin quake

echo "[2/3] Generating random manifests..."
if [[ -n "${QUAKE_GENERATE_SEED:-}" ]]; then
  "$QUAKE" --seed "$QUAKE_GENERATE_SEED" generate --output-dir "$GENERATED_DIR" --count "$COUNT"
else
  "$QUAKE" generate --output-dir "$GENERATED_DIR" --count "$COUNT"
fi

shopt -s nullglob
manifests=("$GENERATED_DIR"/*.toml)
shopt -u nullglob

if [[ ${#manifests[@]} -eq 0 ]]; then
  echo "ERROR: No manifests generated in $GENERATED_DIR"
  exit 1
fi

echo "Generated ${#manifests[@]} manifest(s). Will test all of them."

tested=0
for manifest in "${manifests[@]}"; do
  name="$(basename "$manifest" .toml)"
  echo ""
  echo "[3/3] Testing manifest $name ($((tested + 1)) / ${#manifests[@]})..."
  QUAKE_DIR=".quake/${name}"
  COMPOSE_FILE="${QUAKE_DIR}/compose.yaml"

  failed_phase=""
  if ! run_phase "$manifest" "setup" setup --num-extra-accounts 100; then
    failed_phase="setup"
  fi
  if [[ -z "$failed_phase" ]] && ! run_phase "$manifest" "start" start; then
    failed_phase="start"
  fi
  if [[ -z "$failed_phase" ]] && ! run_phase "$manifest" "wait" wait height 140 --timeout 90; then  # timeout in seconds
    failed_phase="wait"
  fi
  if [[ -z "$failed_phase" ]] && ! run_phase "$manifest" "test" test --rpc-timeout 15s; then
    failed_phase="test"
  fi

  run_phase "$manifest" "clean" clean --all

  ((tested++)) || true
done

if [[ $errors_occurred -gt 0 ]]; then
  echo "FAILED: $errors_occurred errors occurred for valid manifest(s)."
  exit 1
fi
