#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  publish-crates.sh --tag vX.Y.Z --packages crate-a,crate-b [--dry-run]

Environment:
  CARGO_REGISTRY_TOKEN  crates.io token. Required unless --dry-run is set.
USAGE
}

TAG=""
PACKAGES=""
DRY_RUN=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag) TAG="${2:?missing tag}"; shift 2 ;;
    --packages) PACKAGES="${2-}"; shift 2 ;;
    --dry-run) DRY_RUN=true; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage; exit 1 ;;
  esac
done

if [[ -z "${TAG}" ]]; then
  echo "Set --tag" >&2
  exit 1
fi

if [[ ! "${TAG}" =~ ^v([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
  echo "crates.io publishing only supports final public tags, got ${TAG}" >&2
  exit 1
fi

VERSION="${TAG#v}"

if [[ -z "${PACKAGES}" ]]; then
  echo "No crates configured for crates.io publishing; skipping"
  exit 0
fi

if [[ "${DRY_RUN}" != true && -z "${CARGO_REGISTRY_TOKEN:-}" ]]; then
  echo "CARGO_REGISTRY_TOKEN is required for crates.io publishing" >&2
  exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required to inspect Cargo metadata" >&2
  exit 1
fi

METADATA="$(cargo metadata --locked --format-version 1)"
IFS=',' read -r -a PACKAGE_LIST <<< "${PACKAGES}"

for package in "${PACKAGE_LIST[@]}"; do
  package="${package//[[:space:]]/}"
  [[ -z "${package}" ]] && continue

  package_version="$(
    jq -r --arg name "${package}" '.packages[] | select(.name == $name) | .version' <<< "${METADATA}" |
      head -n1
  )"

  if [[ -z "${package_version}" || "${package_version}" == "null" ]]; then
    echo "Unknown Cargo package: ${package}" >&2
    exit 1
  fi

  if [[ "${package_version}" != "${VERSION}" ]]; then
    echo "Package ${package} is version ${package_version}, expected ${VERSION} from ${TAG}" >&2
    exit 1
  fi

  if [[ "${DRY_RUN}" == true ]]; then
    cargo publish --locked --dry-run -p "${package}"
    continue
  fi

  output_file="$(mktemp)"
  if cargo publish --locked -p "${package}" --token "${CARGO_REGISTRY_TOKEN}" >"${output_file}" 2>&1; then
    cat "${output_file}"
    rm -f "${output_file}"
    continue
  fi

  if grep -qiE 'already uploaded|already exists|previously uploaded' "${output_file}"; then
    cat "${output_file}"
    echo "Package ${package} ${VERSION} already exists on crates.io; treating as complete"
    rm -f "${output_file}"
    continue
  fi

  cat "${output_file}" >&2
  rm -f "${output_file}"
  exit 1
done
