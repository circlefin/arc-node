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
  local image_version

  if [[ ! "${tag}" =~ ^([A-Za-z0-9._/-]*[-/])?v[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.-]+)?$ ]]; then
    return 1
  fi

  image_version="${tag//\//-}"
  if [[ ! "${image_version}" =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$ ]]; then
    return 1
  fi

  printf '%s\n' "${image_version}"
}
