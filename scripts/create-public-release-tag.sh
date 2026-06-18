#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  create-public-release-tag.sh --tag vX.Y.Z --release-branch release/X.Y [--target-ref <sha/ref>] [--tag-prefix v] [--release-branch-prefix release/]

Environment:
  PUSH_TAG=false  Create/reuse the tag locally but do not push it to origin.
USAGE
}

TAG=""
RELEASE_BRANCH=""
TARGET_REF=""
PUSH_TAG="${PUSH_TAG:-true}"
CREATED_NEW_TAG=false
TAG_PREFIX="${TAG_PREFIX:-v}"
RELEASE_BRANCH_PREFIX="${RELEASE_BRANCH_PREFIX:-release/}"

tag_version() {
  local tag="$1"
  if [[ "${tag}" != "${TAG_PREFIX}"* ]]; then
    return 1
  fi
  printf '%s\n' "${tag:${#TAG_PREFIX}}"
}

configure_tag_identity() {
  if [[ "${GITHUB_ACTIONS:-}" != "true" ]]; then
    return
  fi

  if ! git config user.name >/dev/null; then
    git config user.name "github-actions[bot]"
  fi
  if ! git config user.email >/dev/null; then
    git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
  fi
}

resolve_release_branch_ref() {
  local branch="$1"

  if git rev-parse --verify --quiet "refs/heads/${branch}^{commit}" >/dev/null; then
    echo "${branch}"
    return
  fi

  if git rev-parse --verify --quiet "refs/remotes/origin/${branch}^{commit}" >/dev/null; then
    echo "origin/${branch}"
    return
  fi

  echo "Cannot resolve release branch: ${branch}" >&2
  exit 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag) TAG="${2:?missing tag}"; shift 2 ;;
    --release-branch) RELEASE_BRANCH="${2:?missing release branch}"; shift 2 ;;
    --target-ref) TARGET_REF="${2:?missing target ref}"; shift 2 ;;
    --tag-prefix) TAG_PREFIX="${2:?missing tag prefix}"; shift 2 ;;
    --release-branch-prefix) RELEASE_BRANCH_PREFIX="${2:?missing release branch prefix}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage; exit 1 ;;
  esac
done

if [[ -z "${TAG}" || -z "${RELEASE_BRANCH}" ]]; then
  echo "Set --tag and --release-branch" >&2
  exit 1
fi

VERSION="$(tag_version "${TAG}")" || {
  echo "Public release tag ${TAG} does not start with tag prefix ${TAG_PREFIX}" >&2
  exit 1
}
if [[ ! "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Public release tags must be final SemVer tags, got ${TAG}" >&2
  exit 1
fi

release_refs="$(
  bash "$(dirname "$0")/release-refs.sh" \
    --tag "${TAG}" \
    --tag-prefix "${TAG_PREFIX}" \
    --release-branch-prefix "${RELEASE_BRANCH_PREFIX}"
)"
expected_release_branch="$(awk -F= '$1 == "release_branch" {print $2}' <<< "${release_refs}")"

if [[ "${RELEASE_BRANCH}" != "${expected_release_branch}" ]]; then
  echo "Tag ${TAG} belongs to ${expected_release_branch}, not ${RELEASE_BRANCH}" >&2
  exit 1
fi

RELEASE_BRANCH_REF="$(resolve_release_branch_ref "${RELEASE_BRANCH}")"
BRANCH_SHA="$(git rev-parse "${RELEASE_BRANCH_REF}^{commit}")"
if [[ -n "${TARGET_REF}" ]]; then
  TARGET_SHA="$(git rev-parse "${TARGET_REF}^{commit}")"
  if ! git merge-base --is-ancestor "${TARGET_SHA}" "${RELEASE_BRANCH_REF}"; then
    echo "Target ${TARGET_REF}@${TARGET_SHA} is not contained in ${RELEASE_BRANCH}@${BRANCH_SHA}" >&2
    exit 1
  fi
fi

if git rev-parse --verify --quiet "refs/tags/${TAG}" >/dev/null; then
  TAG_SHA="$(git rev-list -n1 "${TAG}")"
  if [[ -n "${TARGET_REF}" && "${TAG_SHA}" != "${TARGET_SHA}" ]]; then
    echo "Tag ${TAG} already exists at ${TAG_SHA}, not target ${TARGET_REF}@${TARGET_SHA}" >&2
    exit 1
  fi
  if ! git merge-base --is-ancestor "${TAG_SHA}" "${RELEASE_BRANCH_REF}"; then
    echo "Tag ${TAG} exists at ${TAG_SHA}, which is not contained in ${RELEASE_BRANCH}@${BRANCH_SHA}" >&2
    exit 1
  fi
  TARGET_SHA="${TAG_SHA}"
  echo "Tag already exists on ${RELEASE_BRANCH}: ${TAG}@${TARGET_SHA}" >&2
else
  if [[ -z "${TARGET_REF}" ]]; then
    echo "Tag ${TAG} does not exist; --target-ref is required to create a public release tag" >&2
    exit 1
  fi

  configure_tag_identity
  git -c tag.gpgSign=false tag -a "${TAG}" -m "Release ${TAG}" "${TARGET_SHA}"
  CREATED_NEW_TAG=true
  if [[ "${PUSH_TAG}" == true ]]; then
    git push origin "refs/tags/${TAG}"
  fi
fi

SHORT_SHA="$(git rev-parse --short=8 "${TARGET_SHA}")"
{
  echo "tag=${TAG}"
  echo "sha=${TARGET_SHA}"
  echo "short_sha=${SHORT_SHA}"
  echo "version=${VERSION}"
  echo "release_branch=${RELEASE_BRANCH}"
  echo "created_new_tag=${CREATED_NEW_TAG}"
} > public-release.env

cat public-release.env
