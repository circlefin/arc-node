#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  create-release-tag.sh --source <ref> --release-kind patch|minor|major [--as-release-candidate] [--tag-prefix v] [--baseline-tag-prefix v] [--release-branch-prefix release/] [--main-branch main]
  create-release-tag.sh --tag <existing tag> [--tag-prefix v] [--baseline-tag-prefix v] [--release-branch-prefix release/] [--main-branch main]

Environment:
  PUSH_TAG=false  Create the tag locally but do not push it to origin.
  TAG_PREFIX=v    Prefix used when parsing and creating release tags.
  BASELINE_TAG_PREFIX
                  Prefix used as a read-only SemVer baseline. Defaults to
                  TAG_PREFIX.
  RELEASE_BRANCH_PREFIX=release/
                  Prefix used when parsing and creating release branches.
  MAIN_BRANCH=main
                  Mainline branch used for minor and major releases.
  ALLOW_STALE_RELEASE_REFS=true
                  Continue if fetching origin refs fails. Intended only for
                  local/offline testing.
USAGE
}

SOURCE_REF="latest"
RELEASE_KIND=""
TAG=""
AS_RELEASE_CANDIDATE=false
PUSH_TAG="${PUSH_TAG:-true}"
CREATED_NEW_TAG=false
CREATED_RELEASE_BRANCH=false
RESOLVED_SOURCE_REF=""
SOURCE_BRANCH=""
TAG_PREFIX="${TAG_PREFIX:-v}"
BASELINE_TAG_PREFIX="${BASELINE_TAG_PREFIX:-${TAG_PREFIX}}"
RELEASE_BRANCH_PREFIX="${RELEASE_BRANCH_PREFIX:-release/}"
MAIN_BRANCH="${MAIN_BRANCH:-main}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --source) SOURCE_REF="${2:?missing source ref}"; shift 2 ;;
    --release-kind) RELEASE_KIND="${2:?missing release kind}"; shift 2 ;;
    --tag) TAG="${2:?missing tag}"; shift 2 ;;
    --as-release-candidate) AS_RELEASE_CANDIDATE=true; shift ;;
    --tag-prefix) TAG_PREFIX="${2:?missing tag prefix}"; shift 2 ;;
    --baseline-tag-prefix) BASELINE_TAG_PREFIX="${2:?missing baseline tag prefix}"; shift 2 ;;
    --release-branch-prefix) RELEASE_BRANCH_PREFIX="${2:?missing release branch prefix}"; shift 2 ;;
    --main-branch) MAIN_BRANCH="${2:?missing main branch}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown argument: $1" >&2; usage; exit 1 ;;
  esac
done

if [[ -n "${RELEASE_KIND}" && -n "${TAG}" ]]; then
  echo "Set either --release-kind or tag, not both" >&2
  exit 1
fi

if [[ -z "${RELEASE_KIND}" && -z "${TAG}" ]]; then
  echo "Set release_kind or tag" >&2
  exit 1
fi

validate_namespace_ref() {
  local label="$1"
  local ref="$2"

  if ! git check-ref-format "${ref}" >/dev/null 2>&1; then
    echo "Invalid ${label}: ${ref}" >&2
    exit 1
  fi
}

if [[ -z "${TAG_PREFIX}" ]]; then
  echo "tag prefix cannot be empty" >&2
  exit 1
fi
if [[ -z "${BASELINE_TAG_PREFIX}" ]]; then
  echo "baseline tag prefix cannot be empty" >&2
  exit 1
fi
if [[ -z "${RELEASE_BRANCH_PREFIX}" ]]; then
  echo "release branch prefix cannot be empty" >&2
  exit 1
fi
if [[ -z "${MAIN_BRANCH}" ]]; then
  echo "main branch cannot be empty" >&2
  exit 1
fi
validate_namespace_ref "tag prefix" "refs/tags/${TAG_PREFIX}0.0.0"
validate_namespace_ref "baseline tag prefix" "refs/tags/${BASELINE_TAG_PREFIX}0.0.0"
validate_namespace_ref "release branch prefix" "refs/heads/${RELEASE_BRANCH_PREFIX}0.0"
validate_namespace_ref "main branch" "refs/heads/${MAIN_BRANCH}"

if ! git fetch origin '+refs/heads/*:refs/remotes/origin/*' --tags --prune >/dev/null 2>&1; then
  if [[ "${ALLOW_STALE_RELEASE_REFS:-false}" != true ]]; then
    echo "Unable to fetch origin refs; refusing to calculate a release from stale local state" >&2
    exit 1
  fi
  echo "Warning: unable to fetch origin; continuing with local refs because ALLOW_STALE_RELEASE_REFS=true." >&2
fi

tag_version() {
  local tag="$1"
  local prefix="${2:-${TAG_PREFIX}}"
  if [[ "${tag:0:${#prefix}}" != "${prefix}" ]]; then
    return 1
  fi
  printf '%s\n' "${tag:${#prefix}}"
}

tag_version_from_configured_prefix() {
  local tag="$1"

  tag_version "${tag}" "${TAG_PREFIX}" || tag_version "${tag}" "${BASELINE_TAG_PREFIX}"
}

validate_tag() {
  local tag="$1"
  local version

  version="$(tag_version "${tag}")" || return 1
  [[ "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-rc[0-9]+)?$ ]]
}

format_final_tag() {
  printf '%s%s.%s.%s\n' "${TAG_PREFIX}" "$1" "$2" "$3"
}

format_final_tag_with_prefix() {
  printf '%s%s.%s.%s\n' "$1" "$2" "$3" "$4"
}

format_rc_tag() {
  printf '%s%s.%s.%s-rc%s\n' "${TAG_PREFIX}" "$1" "$2" "$3" "$4"
}

release_line_from_branch() {
  local branch="$1"
  branch="${branch#origin/}"
  if [[ "${branch:0:${#RELEASE_BRANCH_PREFIX}}" != "${RELEASE_BRANCH_PREFIX}" ]]; then
    return 1
  fi

  local line="${branch:${#RELEASE_BRANCH_PREFIX}}"
  [[ "${line}" =~ ^([0-9]+)\.([0-9]+)(\.([0-9]+|x))?$ ]]
}

tag_points_to_ref() {
  local tag="$1"
  local ref="$2"

  if ! git rev-parse --verify --quiet "refs/tags/${tag}" >/dev/null; then
    return 1
  fi

  local tag_sha ref_sha
  tag_sha="$(git rev-list -n1 "${tag}")"
  ref_sha="$(git rev-parse "${ref}^{commit}")"
  [[ "${tag_sha}" == "${ref_sha}" ]]
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

latest_release_branch() {
  local branches latest
  branches="$(
    while IFS= read -r branch; do
      branch="${branch#origin/}"
      if release_line_from_branch "${branch}"; then
        printf '%s\t%s\n' "${branch:${#RELEASE_BRANCH_PREFIX}}" "${branch}"
      fi
    done < <(git for-each-ref --format='%(refname:short)' refs/heads refs/remotes/origin 2>/dev/null) |
      sort -k1,1V |
      awk -F'\t' '!seen[$2]++ {print $2}' || true
  )"
  latest="$(printf '%s\n' "${branches}" | tail -n1)"
  if [[ -z "${latest}" ]]; then
    echo "No release branch found matching ${RELEASE_BRANCH_PREFIX}X.Y, ${RELEASE_BRANCH_PREFIX}X.Y.Z, or ${RELEASE_BRANCH_PREFIX}X.Y.x" >&2
    exit 1
  fi
  echo "${latest}"
}

resolve_ref() {
  local ref="$1"
  local kind="$2"
  if [[ "${ref}" == "latest" ]]; then
    if [[ "${kind}" == "patch" ]]; then
      ref="$(latest_release_branch)"
      echo "Resolved latest release branch to ${ref}" >&2
    else
      ref="${MAIN_BRANCH}"
      echo "Resolved latest source branch to ${ref} for ${kind} release" >&2
    fi
  fi
  if git rev-parse --verify --quiet "${ref}^{commit}" >/dev/null; then
    echo "${ref}"
  elif git rev-parse --verify --quiet "origin/${ref}^{commit}" >/dev/null; then
    echo "origin/${ref}"
  else
    echo "Cannot resolve ref: ${ref}" >&2
    exit 1
  fi
}

normalize_branch_name() {
  local ref="$1"
  ref="${ref#origin/}"
  if [[ "${ref}" == "${MAIN_BRANCH}" || "${ref:0:${#RELEASE_BRANCH_PREFIX}}" == "${RELEASE_BRANCH_PREFIX}" ]]; then
    echo "${ref}"
    return
  fi

  local containing
  containing="$(
    while IFS= read -r branch; do
      branch="${branch#origin/}"
      if [[ "${branch}" == "${MAIN_BRANCH}" || "${branch:0:${#RELEASE_BRANCH_PREFIX}}" == "${RELEASE_BRANCH_PREFIX}" ]]; then
        echo "${branch}"
        break
      fi
    done < <(git branch -r --contains "${ref}" --format='%(refname:short)' 2>/dev/null) || true
  )"
  if [[ -z "${containing}" ]]; then
    containing="$(
      while IFS= read -r branch; do
        if [[ "${branch}" == "${MAIN_BRANCH}" || "${branch:0:${#RELEASE_BRANCH_PREFIX}}" == "${RELEASE_BRANCH_PREFIX}" ]]; then
          echo "${branch}"
          break
        fi
      done < <(git branch --contains "${ref}" --format='%(refname:short)' 2>/dev/null) || true
    )"
  fi
  echo "${containing}"
}

latest_global_tag() {
  local prefix="${1:-${TAG_PREFIX}}"
  local tag version final_tag

  while IFS= read -r tag; do
    [[ -z "${tag}" ]] && continue

    version="$(tag_version "${tag}" "${prefix}")" || continue
    if [[ "${version}" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)-rc[0-9]+$ ]]; then
      final_tag="$(format_final_tag_with_prefix "${prefix}" "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}" "${BASH_REMATCH[3]}")"
      if git rev-parse --verify --quiet "refs/tags/${final_tag}" >/dev/null; then
        continue
      fi
    fi

    echo "${tag}"
    return
  done < <(git tag --list "${prefix}[0-9]*" --sort=-v:refname)
}

latest_tag() {
  local source="$1"
  local tag_filter="$2"
  local prefix="${3:-${TAG_PREFIX}}"
  local tag version final_tag final_sha

  while IFS= read -r tag; do
    [[ -z "${tag}" ]] && continue

    version="$(tag_version "${tag}" "${prefix}")" || continue
    if [[ "${version}" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)-rc[0-9]+$ ]]; then
      final_tag="$(format_final_tag_with_prefix "${prefix}" "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}" "${BASH_REMATCH[3]}")"
      if git rev-parse --verify --quiet "refs/tags/${final_tag}" >/dev/null; then
        final_sha="$(git rev-list -n1 "${final_tag}")"
        if git merge-base --is-ancestor "${final_sha}" "${source}"; then
          continue
        fi
      fi
    fi

    echo "${tag}"
    return
  done < <(git tag --merged "${source}" --list "${tag_filter}" --sort=-v:refname)
}

latest_matching_rc_tag() {
  local source="$1"
  local final_tag="$2"

  git tag --merged "${source}" --list "${final_tag}-rc[0-9]*" --sort=-v:refname | head -n1
}

latest_final_tag_for_candidate() {
  local candidate_tag="$1"
  local candidate_version major minor tag tag_filter

  candidate_version="$(tag_version "${candidate_tag}")" || return
  if [[ ! "${candidate_version}" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)(-rc[0-9]+)?$ ]]; then
    return
  fi
  major="${BASH_REMATCH[1]}"
  minor="${BASH_REMATCH[2]}"

  tag_filter="${TAG_PREFIX}[0-9]*"
  if [[ "${RELEASE_KIND}" == "patch" ]]; then
    tag_filter="${TAG_PREFIX}${major}.${minor}.*"
  fi

  if tag="$(latest_final_tag_for_prefix "${candidate_tag}" "${tag_filter}" "${TAG_PREFIX}")"; then
    echo "${tag}"
    return
  fi

  if [[ "${BASELINE_TAG_PREFIX}" != "${TAG_PREFIX}" ]]; then
    tag_filter="${BASELINE_TAG_PREFIX}[0-9]*"
    if [[ "${RELEASE_KIND}" == "patch" ]]; then
      tag_filter="${BASELINE_TAG_PREFIX}${major}.${minor}.*"
    fi
    if tag="$(latest_final_tag_for_prefix "" "${tag_filter}" "${BASELINE_TAG_PREFIX}")"; then
      echo "${tag}"
    fi
  fi
}

latest_final_tag_for_prefix() {
  local candidate_tag="$1"
  local tag_filter="$2"
  local prefix="$3"
  local tag version

  while IFS= read -r tag; do
    [[ -z "${tag}" ]] && continue
    [[ -n "${candidate_tag}" && "${tag}" == "${candidate_tag}" ]] && continue

    version="$(tag_version "${tag}" "${prefix}")" || continue
    if [[ "${version}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
      echo "${tag}"
      return
    fi
  done < <(git tag --list "${tag_filter}" --sort=-v:refname)

  return 1
}

ensure_candidate_has_diff_from_previous_final() {
  local candidate_tag="$1"
  local target_sha="$2"
  local previous_final_tag

  previous_final_tag="$(latest_final_tag_for_candidate "${candidate_tag}")"
  if [[ -z "${previous_final_tag}" ]]; then
    return
  fi

  if git diff --quiet --no-ext-diff "${previous_final_tag}" "${target_sha}" -- .; then
    echo "No changes since ${previous_final_tag}; refusing to create ${candidate_tag} at ${target_sha}" >&2
    exit 1
  fi
}

continues_rc_line() {
  local kind="$1"
  local minor="$2"
  local patch="$3"

  case "${kind}" in
    patch) return 0 ;;
    minor) [[ "${patch}" == 0 ]] ;;
    major) [[ "${minor}" == 0 && "${patch}" == 0 ]] ;;
    *) echo "Invalid release kind: ${kind}" >&2; exit 1 ;;
  esac
}

next_tag() {
  local kind="$1"
  local source="$2"
  local latest latest_version major minor patch rc latest_is_rc tag_filter branch_name base_tag latest_uses_release_prefix

  tag_filter="${TAG_PREFIX}[0-9]*"
  branch_name="$(normalize_branch_name "${source}")"
  if release_line_from_branch "${branch_name}"; then
    tag_filter="${TAG_PREFIX}${BASH_REMATCH[1]}.${BASH_REMATCH[2]}.*"
  fi

  if [[ "${kind}" == "patch" ]]; then
    latest="$(latest_tag "${source}" "${tag_filter}")"
    if [[ -z "${latest}" && "${BASELINE_TAG_PREFIX}" != "${TAG_PREFIX}" ]]; then
      tag_filter="${tag_filter/#${TAG_PREFIX}/${BASELINE_TAG_PREFIX}}"
      latest="$(latest_tag "${source}" "${tag_filter}" "${BASELINE_TAG_PREFIX}")"
    fi
  else
    latest="$(latest_global_tag)"
    if [[ -z "${latest}" && "${BASELINE_TAG_PREFIX}" != "${TAG_PREFIX}" ]]; then
      latest="$(latest_global_tag "${BASELINE_TAG_PREFIX}")"
    fi
  fi
  base_tag="$(format_final_tag 0 0 0)"
  latest="${latest:-${base_tag}}"
  latest_version="$(tag_version_from_configured_prefix "${latest}")"
  latest_uses_release_prefix=false
  if tag_version "${latest}" "${TAG_PREFIX}" >/dev/null 2>&1; then
    latest_uses_release_prefix=true
  fi

  latest_is_rc=false
  rc=0
  if [[ "${latest_version}" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)-rc([0-9]+)$ ]]; then
    major="${BASH_REMATCH[1]}"
    minor="${BASH_REMATCH[2]}"
    patch="${BASH_REMATCH[3]}"
    rc="${BASH_REMATCH[4]}"
    latest_is_rc=true
  elif [[ "${latest_version}" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
    major="${BASH_REMATCH[1]}"
    minor="${BASH_REMATCH[2]}"
    patch="${BASH_REMATCH[3]}"
  else
    echo "Latest tag has unsupported format: ${latest}" >&2
    exit 1
  fi

  if [[ "${AS_RELEASE_CANDIDATE}" == true ]]; then
    if [[ "${latest_uses_release_prefix}" == true && "${latest_is_rc}" == true ]] && continues_rc_line "${kind}" "${minor}" "${patch}"; then
      if tag_points_to_ref "${latest}" "${source}"; then
        echo "Reusing ${latest}; source commit is already tagged" >&2
        printf '%s\n' "${latest}"
        return
      fi

      format_rc_tag "${major}" "${minor}" "${patch}" "$((rc + 1))"
      return
    fi

    case "${kind}" in
      patch) patch=$((patch + 1)) ;;
      minor) minor=$((minor + 1)); patch=0 ;;
      major) major=$((major + 1)); minor=0; patch=0 ;;
      *) echo "Invalid release kind: ${kind}" >&2; exit 1 ;;
    esac
    format_rc_tag "${major}" "${minor}" "${patch}" 1
    return
  fi

  if [[ "${latest_uses_release_prefix}" == true && "${kind}" == "patch" && "${latest}" != "${base_tag}" && "${latest_is_rc}" == false ]] && tag_points_to_ref "${latest}" "${source}"; then
    echo "Reusing ${latest}; source commit is already tagged" >&2
    printf '%s\n' "${latest}"
    return
  fi

  if [[ "${latest_uses_release_prefix}" == true && "${latest_is_rc}" == true ]] && continues_rc_line "${kind}" "${minor}" "${patch}"; then
    echo "Promoting ${latest} to $(format_final_tag "${major}" "${minor}" "${patch}")" >&2
    format_final_tag "${major}" "${minor}" "${patch}"
    return
  fi

  case "${kind}" in
    patch) patch=$((patch + 1)) ;;
    minor) minor=$((minor + 1)); patch=0 ;;
    major) major=$((major + 1)); minor=0; patch=0 ;;
    *) echo "Invalid release kind: ${kind}" >&2; exit 1 ;;
  esac

  format_final_tag "${major}" "${minor}" "${patch}"
}

next_available_rc_tag() {
  local tag="$1"
  local version major minor patch rc

  while git rev-parse --verify "refs/tags/${tag}" >/dev/null 2>&1; do
    version="$(tag_version "${tag}")" || {
      echo "Tag already exists: ${tag}" >&2
      exit 1
    }
    if [[ ! "${version}" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)-rc([0-9]+)$ ]]; then
      echo "Tag already exists: ${tag}" >&2
      exit 1
    fi
    major="${BASH_REMATCH[1]}"
    minor="${BASH_REMATCH[2]}"
    patch="${BASH_REMATCH[3]}"
    rc="${BASH_REMATCH[4]}"
    tag="$(format_rc_tag "${major}" "${minor}" "${patch}" "$((rc + 1))")"
  done

  echo "${tag}"
}

ensure_next_line_release_branch() {
  local release_branch="$1"
  local target_sha="$2"

  if [[ -z "${RELEASE_KIND}" || "${RELEASE_KIND}" == "patch" ]]; then
    return
  fi
  if [[ -z "${release_branch}" || "${SOURCE_BRANCH}" == "${release_branch}" ]]; then
    return
  fi

  if git rev-parse --verify --quiet "refs/remotes/origin/${release_branch}^{commit}" >/dev/null; then
    if git merge-base --is-ancestor "${target_sha}" "origin/${release_branch}"; then
      echo "Release branch already exists for ${TAG}: ${release_branch}" >&2
      return
    fi

    echo "Release branch ${release_branch} exists but does not contain ${TAG}@${target_sha}" >&2
    exit 1
  fi

  if git rev-parse --verify --quiet "refs/heads/${release_branch}^{commit}" >/dev/null; then
    if ! git merge-base --is-ancestor "${target_sha}" "${release_branch}"; then
      echo "Local release branch ${release_branch} exists but does not contain ${TAG}@${target_sha}" >&2
      exit 1
    fi
  else
    git branch "${release_branch}" "${target_sha}"
  fi

  CREATED_RELEASE_BRANCH=true
}

validate_release_source() {
  local kind="$1"
  local source_ref="$2"
  local source_branch="$3"
  local source_ref_branch="${source_ref#origin/}"

  if [[ "${kind}" == "patch" ]]; then
    return
  fi

  if release_line_from_branch "${source_branch}"; then
    echo "${kind} releases must start from ${MAIN_BRANCH} or an explicit ${MAIN_BRANCH} commit, not ${source_branch}" >&2
    exit 1
  fi

  if [[ "${source_ref_branch:0:${#RELEASE_BRANCH_PREFIX}}" == "${RELEASE_BRANCH_PREFIX}" ]]; then
    echo "${kind} releases must not use release branch source_ref=${source_ref}" >&2
    exit 1
  fi
}

push_created_refs() {
  local refs=()

  if [[ "${PUSH_TAG}" != true ]]; then
    return
  fi

  if [[ "${CREATED_NEW_TAG}" == true ]]; then
    refs+=("refs/tags/${TAG}")
  fi
  if [[ "${CREATED_RELEASE_BRANCH}" == true ]]; then
    refs+=("refs/heads/${RELEASE_BRANCH}")
  fi

  if [[ ${#refs[@]} -gt 0 ]]; then
    git push origin "${refs[@]}"
  fi
}

if [[ -n "${RELEASE_KIND}" ]]; then
  RESOLVED_SOURCE_REF="$(resolve_ref "${SOURCE_REF}" "${RELEASE_KIND}")"
  SOURCE_BRANCH="$(normalize_branch_name "${RESOLVED_SOURCE_REF}")"
  validate_release_source "${RELEASE_KIND}" "${SOURCE_REF}" "${SOURCE_BRANCH}"
  TAG="$(next_tag "${RELEASE_KIND}" "${RESOLVED_SOURCE_REF}")"
  TAG_TARGET_REF="${RESOLVED_SOURCE_REF}"
  if [[ "${AS_RELEASE_CANDIDATE}" == true ]]; then
    if ! tag_points_to_ref "${TAG}" "${RESOLVED_SOURCE_REF}"; then
      TAG="$(next_available_rc_tag "${TAG}")"
    fi
    TAG_TARGET_REF="${RESOLVED_SOURCE_REF}"
  fi
  validate_tag "${TAG}" || { echo "Generated invalid tag: ${TAG}" >&2; exit 1; }
  if git rev-parse --verify "refs/tags/${TAG}" >/dev/null 2>&1; then
    TAG_TARGET_SHA="$(git rev-parse "${TAG_TARGET_REF}^{commit}")"
    ensure_candidate_has_diff_from_previous_final "${TAG}" "${TAG_TARGET_SHA}"
    if tag_points_to_ref "${TAG}" "${TAG_TARGET_REF}"; then
      echo "Tag already exists at target commit: ${TAG}" >&2
    else
      echo "Tag already exists: ${TAG}" >&2
      exit 1
    fi
  else
    TAG_VERSION="$(tag_version "${TAG}")"
    if [[ "${AS_RELEASE_CANDIDATE}" != true && "${TAG_VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
      PROMOTED_RC_TAG="$(latest_matching_rc_tag "${RESOLVED_SOURCE_REF}" "${TAG}")"
      if [[ -n "${PROMOTED_RC_TAG}" ]]; then
        TAG_TARGET_REF="${PROMOTED_RC_TAG}"
      fi
    fi
    configure_tag_identity
    TAG_TARGET_SHA="$(git rev-parse "${TAG_TARGET_REF}^{commit}")"
    ensure_candidate_has_diff_from_previous_final "${TAG}" "${TAG_TARGET_SHA}"
    git -c tag.gpgSign=false tag -a "${TAG}" -m "Release ${TAG}" "${TAG_TARGET_SHA}"
    CREATED_NEW_TAG=true
  fi
else
  validate_tag "${TAG}" || { echo "Invalid tag: ${TAG}" >&2; exit 1; }
  if ! git rev-parse --verify "refs/tags/${TAG}" >/dev/null 2>&1; then
    echo "Explicit tag does not exist: ${TAG}" >&2
    exit 1
  fi
  RESOLVED_SOURCE_REF="${TAG}"
  SOURCE_BRANCH="$(normalize_branch_name "${TAG}")"
fi

SHA="$(git rev-list -n1 "${TAG}")"
SHORT_SHA="$(git rev-parse --short=8 "${SHA}")"
VERSION="$(tag_version "${TAG}")"
IS_RELEASE_CANDIDATE=false
RELEASE_BRANCH=""
if [[ "${TAG}" =~ -rc[0-9]+$ ]]; then
  IS_RELEASE_CANDIDATE=true
fi
if [[ "${VERSION}" =~ ^([0-9]+)\.([0-9]+)\.[0-9]+(-rc[0-9]+)?$ ]]; then
  RELEASE_BRANCH="${RELEASE_BRANCH_PREFIX}${BASH_REMATCH[1]}.${BASH_REMATCH[2]}"
fi

if [[ -n "${RELEASE_KIND}" ]]; then
  ensure_next_line_release_branch "${RELEASE_BRANCH}" "${SHA}"
  push_created_refs
fi

CHANNEL="preview"
if git merge-base --is-ancestor "${SHA}" "origin/${MAIN_BRANCH}" 2>/dev/null || git merge-base --is-ancestor "${SHA}" "${MAIN_BRANCH}" 2>/dev/null; then
  CHANNEL="latest"
elif git branch -r --contains "${SHA}" --format='%(refname:short)' 2>/dev/null | grep -qF "origin/${RELEASE_BRANCH_PREFIX}"; then
  CHANNEL="release"
elif git branch --contains "${SHA}" --format='%(refname:short)' 2>/dev/null | grep -qF "${RELEASE_BRANCH_PREFIX}"; then
  CHANNEL="release"
fi

{
  echo "tag=${TAG}"
  echo "sha=${SHA}"
  echo "short_sha=${SHORT_SHA}"
  echo "version=${VERSION}"
  echo "is_release_candidate=${IS_RELEASE_CANDIDATE}"
  echo "channel=${CHANNEL}"
  echo "created_new_tag=${CREATED_NEW_TAG}"
  echo "created_release_branch=${CREATED_RELEASE_BRANCH}"
  echo "source_ref=${RESOLVED_SOURCE_REF}"
  echo "source_branch=${SOURCE_BRANCH}"
  echo "release_branch=${RELEASE_BRANCH}"
} > release.env

cat release.env
