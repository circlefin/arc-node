#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  release-refs.sh --tag vX.Y.Z[-rcN] [--tag-prefix v] [--release-branch-prefix release/]

Outputs shell-style key=value metadata for the release tag.
USAGE
}

TAG=""
TAG_PREFIX="${TAG_PREFIX:-v}"
RELEASE_BRANCH_PREFIX="${RELEASE_BRANCH_PREFIX:-release/}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag) TAG="${2:?missing tag}"; shift 2 ;;
    --tag-prefix) TAG_PREFIX="${2:?missing tag prefix}"; shift 2 ;;
    --release-branch-prefix) RELEASE_BRANCH_PREFIX="${2:?missing release branch prefix}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage; exit 1 ;;
  esac
done

if [[ -z "${TAG}" ]]; then
  echo "Set --tag" >&2
  exit 1
fi

if [[ "${TAG}" != "${TAG_PREFIX}"* ]]; then
  echo "Invalid release tag: ${TAG} does not start with tag prefix ${TAG_PREFIX}" >&2
  exit 1
fi

VERSION="${TAG:${#TAG_PREFIX}}"
if [[ ! "${VERSION}" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)(-rc([0-9]+))?$ ]]; then
  echo "Invalid release tag: ${TAG}" >&2
  exit 1
fi

MAJOR="${BASH_REMATCH[1]}"
MINOR="${BASH_REMATCH[2]}"
PATCH="${BASH_REMATCH[3]}"
RC="${BASH_REMATCH[5]:-}"
IS_RELEASE_CANDIDATE=false
if [[ -n "${RC}" ]]; then
  IS_RELEASE_CANDIDATE=true
fi
COPYBARA_RELEASE_LINE="${MAJOR}_${MINOR}"

{
  echo "tag=${TAG}"
  echo "version=${VERSION}"
  echo "major=${MAJOR}"
  echo "minor=${MINOR}"
  echo "patch=${PATCH}"
  echo "rc=${RC}"
  echo "is_release_candidate=${IS_RELEASE_CANDIDATE}"
  echo "release_branch=${RELEASE_BRANCH_PREFIX}${MAJOR}.${MINOR}"
  echo "copybara_pr_branch=sync/copybara-export-${RELEASE_BRANCH_PREFIX//\//-}${COPYBARA_RELEASE_LINE}"
}
