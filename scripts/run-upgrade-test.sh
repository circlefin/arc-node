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

# run-upgrade-test.sh triggers the nightly-upgrade workflow with specific
# image version overrides via workflow_dispatch inputs. No branches or commits
# needed — the workflow applies the overrides at runtime.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

WORKFLOW="nightly-upgrade.yml"

FROM_VERSION=""
TO_VERSION=""
HARDFORK=""
BRANCH="master"
REMOTE="upstream"
DRY_RUN=false

usage() {
  cat <<EOF
Usage: $(basename "$0") [--from <version>] [--to <version>] [--hardfork <name>] [--dry-run]

Trigger the nightly-upgrade workflow with image version overrides.

Version values:
  <version>  Published GHCR image ghcr.io/<org>/<repo>/<image>:<version> (e.g. 0.6.0-dev)
  latest     Published GHCR image ghcr.io/<org>/<repo>/<image>:latest
  local      Locally built image <image>:latest

Options:
  --from       Starting version for EL and CL images
  --to         Upgrade target version for EL and CL images
  --hardfork   Set el_init_hardfork (e.g. zero4, zero5)
  --branch     Branch to run the workflow on (default: master)
  --remote     Git remote to resolve the repository from (default: upstream)
  --dry-run    Print the command without executing it
  --help       Show this help message
EOF
  exit 0
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --from)
      [[ $# -lt 2 ]] && { echo "Error: --from requires a value" >&2; exit 1; }
      FROM_VERSION="$2"
      shift 2
      ;;
    --to)
      [[ $# -lt 2 ]] && { echo "Error: --to requires a value" >&2; exit 1; }
      TO_VERSION="$2"
      shift 2
      ;;
    --hardfork)
      [[ $# -lt 2 ]] && { echo "Error: --hardfork requires a value" >&2; exit 1; }
      HARDFORK="$2"
      shift 2
      ;;
    --branch)
      [[ $# -lt 2 ]] && { echo "Error: --branch requires a value" >&2; exit 1; }
      BRANCH="$2"
      shift 2
      ;;
    --remote)
      [[ $# -lt 2 ]] && { echo "Error: --remote requires a value" >&2; exit 1; }
      REMOTE="$2"
      shift 2
      ;;
    --dry-run)
      DRY_RUN=true
      shift
      ;;
    --help)
      usage
      ;;
    *)
      echo "Error: unknown option '$1'" >&2
      echo "Run '$(basename "$0") --help' for usage." >&2
      exit 1
      ;;
  esac
done

# Derive OWNER/REPO from the specified remote.
url_to_repo() {
  sed -E 's|.*[:/]([^/]+/[^/]+)(\.git)$|\1|; s|.*[:/]([^/]+/[^/]+)$|\1|'
}
TARGET_REPO=$(git -C "$REPO_ROOT" remote get-url "$REMOTE" 2>/dev/null | url_to_repo)
if [[ -z "$TARGET_REPO" ]]; then
  echo "Error: could not resolve remote '${REMOTE}'" >&2
  exit 1
fi

# Build the gh workflow run command with input flags.
CMD=(gh workflow run "$WORKFLOW" -R "$TARGET_REPO" --ref "$BRANCH")
[[ -n "$FROM_VERSION" ]] && CMD+=(-f "from_version=${FROM_VERSION}")
[[ -n "$TO_VERSION" ]]   && CMD+=(-f "to_version=${TO_VERSION}")
[[ -n "$HARDFORK" ]]     && CMD+=(-f "hardfork=${HARDFORK}")

if $DRY_RUN; then
  echo "${CMD[@]}"
else
  "${CMD[@]}"
  echo ""
  echo "Workflow triggered. View runs at:"
  echo "  https://github.com/${TARGET_REPO}/actions/workflows/${WORKFLOW}"
fi
