#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  release-refs.sh --tag vX.Y.Z[-rcN] [--release-ref-prefix test-]

Outputs shell-style key=value metadata for the release tag.
USAGE
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/release-config.sh
source "${SCRIPT_DIR}/release-config.sh"

TAG=""
RELEASE_REF_PREFIX="${RELEASE_REF_PREFIX:-}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag) TAG="${2:?missing tag}"; shift 2 ;;
    --release-ref-prefix) RELEASE_REF_PREFIX="${2-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage; exit 1 ;;
  esac
done

if [[ -z "${TAG}" ]]; then
  echo "Set --tag" >&2
  exit 1
fi

TAG_PREFIX="$(release_tag_prefix_from_ref_prefix "${RELEASE_REF_PREFIX}")"
RELEASE_BRANCH_PREFIX="$(release_branch_prefix_from_ref_prefix "${RELEASE_REF_PREFIX}")"

if [[ -z "${TAG_PREFIX}" ]]; then
  echo "tag prefix cannot be empty" >&2
  exit 1
fi
if [[ -z "${RELEASE_BRANCH_PREFIX}" ]]; then
  echo "release branch prefix cannot be empty" >&2
  exit 1
fi
if ! git check-ref-format "refs/tags/${TAG_PREFIX}0.0.0" >/dev/null 2>&1; then
  echo "Invalid tag prefix: ${TAG_PREFIX}" >&2
  exit 1
fi
if ! git check-ref-format "refs/heads/${RELEASE_BRANCH_PREFIX}0.0" >/dev/null 2>&1; then
  echo "Invalid release branch prefix: ${RELEASE_BRANCH_PREFIX}" >&2
  exit 1
fi

if [[ "${TAG:0:${#TAG_PREFIX}}" != "${TAG_PREFIX}" ]]; then
  echo "Invalid release tag: ${TAG} does not start with ${TAG_PREFIX}" >&2
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
RELEASE_BRANCH="${RELEASE_BRANCH_PREFIX}${MAJOR}.${MINOR}"
COPYBARA_BRANCH_VERSION="${TAG//\//-}"
COPYBARA_BRANCH_VERSION="${COPYBARA_BRANCH_VERSION//./_}"

{
  echo "tag=${TAG}"
  echo "version=${VERSION}"
  echo "major=${MAJOR}"
  echo "minor=${MINOR}"
  echo "patch=${PATCH}"
  echo "rc=${RC}"
  echo "is_release_candidate=${IS_RELEASE_CANDIDATE}"
  echo "release_branch=${RELEASE_BRANCH}"
  echo "copybara_pr_branch=sync/${COPYBARA_BRANCH_VERSION}"
}
