#!/usr/bin/env bash

RELEASE_DOMAIN_GRAMMAR='[a-z0-9]+([_-][a-z0-9]+)*'

release_validate_domain() {
  local domain="${1:-}"

  if [[ -z "${domain}" ]]; then
    return 0
  fi
  # A domain that looks like "rc5" would be indistinguishable from a plain
  # release candidate suffix once attached to a tag (vX.Y.Z-rc5).
  if [[ "${domain}" =~ ^rc[0-9]+$ ]]; then
    return 1
  fi
  [[ "${domain}" =~ ^${RELEASE_DOMAIN_GRAMMAR}$ ]]
}

release_branch_prefix_from_domain() {
  local domain="${1:-}"

  release_validate_domain "${domain}" || return 1
  if [[ -z "${domain}" ]]; then
    printf 'release/\n'
  else
    printf 'release/%s/\n' "${domain}"
  fi
}

release_effective_branch_prefix() {
  local domain="${1:-}"

  release_branch_prefix_from_domain "${domain}"
}

release_tag_prefix_from_ref_prefix() {
  local ref_prefix="${1:-}"

  printf '%sv\n' "${ref_prefix}"
}

release_branch_prefix_from_ref_prefix() {
  local ref_prefix="${1:-}"

  printf '%srelease/\n' "${ref_prefix}"
}

# Domain-scoped tags carry the domain as a semver pre-release identifier
# (vX.Y.Z-domain, vX.Y.Z-domain.rcN) so the raw tag stays valid semver, unlike
# a path-style domain/vX.Y.Z prefix.
release_tag_version_for_domain() {
  local tag="$1"
  local domain="${2:-}"

  release_validate_domain "${domain}" || return 1

  if [[ -z "${domain}" ]]; then
    [[ "${tag}" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)(-rc[0-9]+)?$ ]] || return 1
    printf '%s%s\n' "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}"
    return 0
  fi

  [[ "${tag}" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)-(${RELEASE_DOMAIN_GRAMMAR})(\.rc([0-9]+))?$ ]] || return 1
  [[ "${BASH_REMATCH[2]}" == "${domain}" ]] || return 1

  if [[ -n "${BASH_REMATCH[5]}" ]]; then
    printf '%s-rc%s\n' "${BASH_REMATCH[1]}" "${BASH_REMATCH[5]}"
  else
    printf '%s\n' "${BASH_REMATCH[1]}"
  fi
}

# Same suffix grammar as release_tag_version_for_domain, for callers (Docker
# builds) that must recover the domain from a tag without already knowing it.
# A bare "-rcN" is always treated as a domain-less release candidate; domain
# names matching rc[0-9]+ are rejected by release_validate_domain to keep that
# unambiguous.
release_docker_parse_tag() {
  local tag="$1"
  local core domain rc

  if [[ "${tag}" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)-rc([0-9]+)$ ]]; then
    core="${BASH_REMATCH[1]}"; domain=""; rc="${BASH_REMATCH[2]}"
  elif [[ "${tag}" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)-(${RELEASE_DOMAIN_GRAMMAR})\.rc([0-9]+)$ ]]; then
    core="${BASH_REMATCH[1]}"; domain="${BASH_REMATCH[2]}"; rc="${BASH_REMATCH[4]}"
  elif [[ "${tag}" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)-(${RELEASE_DOMAIN_GRAMMAR})$ ]]; then
    core="${BASH_REMATCH[1]}"; domain="${BASH_REMATCH[2]}"; rc=""
  elif [[ "${tag}" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
    core="${BASH_REMATCH[1]}"; domain=""; rc=""
  else
    return 1
  fi

  release_validate_domain "${domain}" || return 1

  printf '%s|%s|%s\n' "${core}" "${domain}" "${rc}"
}

release_docker_image_version_from_tag() {
  local tag="$1"
  local core domain rc image_version

  IFS='|' read -r core domain rc < <(release_docker_parse_tag "${tag}") || return 1

  image_version="${core}"
  [[ -n "${rc}" ]] && image_version="${image_version}-rc${rc}"

  if [[ ! "${image_version}" =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}$ ]]; then
    return 1
  fi

  printf '%s\n' "${image_version}"
}

release_docker_image_name_prefix_from_tag() {
  local tag="$1"
  local core domain rc

  IFS='|' read -r core domain rc < <(release_docker_parse_tag "${tag}") || return 1

  if [[ -n "${domain}" ]]; then
    printf '%s-\n' "${domain}"
  else
    printf '\n'
  fi
}

release_docker_image_metadata_from_tag() {
  local tag="$1"
  local image_version image_name_prefix

  if ! image_version="$(release_docker_image_version_from_tag "${tag}")"; then
    return 1
  fi
  if ! image_name_prefix="$(release_docker_image_name_prefix_from_tag "${tag}")"; then
    return 1
  fi

  printf 'image_version=%s\n' "${image_version}"
  printf 'image_name_prefix=%s\n' "${image_name_prefix}"
}
