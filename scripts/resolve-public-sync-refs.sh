#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  resolve-public-sync-refs.sh --source-ref <ref> [--target-branch <branch>]

Derives and validates the private sanitized mirror branch for the existing main public sync.
USAGE
}

SOURCE_REF=""
TARGET_BRANCH=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --source-ref) SOURCE_REF="${2:?missing source ref}"; shift 2 ;;
    --target-branch) TARGET_BRANCH="${2:?missing target branch}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage; exit 1 ;;
  esac
done

if [[ -z "${SOURCE_REF}" ]]; then
  echo "Set --source-ref" >&2
  exit 1
fi

normalize_ref() {
  local ref="$1"
  ref="${ref#refs/heads/}"
  ref="${ref#refs/tags/}"
  ref="${ref#origin/}"
  echo "${ref}"
}

SOURCE_REF="$(normalize_ref "${SOURCE_REF}")"
SOURCE_KIND=""

if [[ "${SOURCE_REF}" == "main" ]]; then
  SOURCE_KIND="main"
else
  echo "Unsupported source ref for public sync: ${SOURCE_REF}" >&2
  exit 1
fi

if [[ -z "${TARGET_BRANCH}" ]]; then
  TARGET_BRANCH="public-repo"
fi

TARGET_BRANCH="$(normalize_ref "${TARGET_BRANCH}")"

if [[ "${TARGET_BRANCH}" == "public-repo" ]]; then
  if [[ "${SOURCE_KIND}" != "main" ]]; then
    echo "public-repo target can only sync from main, got ${SOURCE_REF}" >&2
    exit 1
  fi
else
  echo "Unsupported target branch: ${TARGET_BRANCH}" >&2
  exit 1
fi

{
  echo "source_ref=${SOURCE_REF}"
  echo "source_kind=${SOURCE_KIND}"
  echo "target_branch=${TARGET_BRANCH}"
} > public-sync.env

cat public-sync.env
