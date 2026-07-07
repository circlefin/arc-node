#!/usr/bin/env bash

release_tag_prefix_from_ref_prefix() {
  local ref_prefix="${1:-}"

  printf '%sv\n' "${ref_prefix}"
}

release_branch_prefix_from_ref_prefix() {
  local ref_prefix="${1:-}"

  printf '%srelease/\n' "${ref_prefix}"
}

release_docker_image_version_from_tag() {
  local tag="$1"
  local version

  if [[ ! "${tag}" =~ ^v([0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.-]+)?)$ ]]; then
    return 1
  fi

  version="${BASH_REMATCH[1]}"
  if [[ ! "${version}" =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$ ]]; then
    return 1
  fi

  printf '%s\n' "${version}"
}
