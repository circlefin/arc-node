#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
# shellcheck source=scripts/release-config.sh
source "${REPO_ROOT}/scripts/release-config.sh"

usage() {
  cat <<'USAGE'
Usage:
  finalize-release.sh validate
  finalize-release.sh finalize

Environment:
  PR_BODY                Pull request body containing release metadata.
  PR_BASE_REF            Pull request base branch.
  PR_HEAD_REF            Pull request head branch.
  MERGE_COMMIT_SHA       Required for finalize mode.
USAGE
}

die() {
  echo "::error::$*" >&2
  exit 1
}

note() {
  echo "$*" >&2
}

require_env() {
  local name="$1"
  if [[ -z "${!name:-}" ]]; then
    die "${name} is required"
  fi
}

extract_body_field() {
  local label="$1"
  awk -v label="${label}: " '
    index($0, label) == 1 {
      sub(label, "")
      print
      found = 1
      exit
    }
    END {
      if (!found) {
        exit 1
      }
    }
  ' <<< "${PR_BODY}"
}

remote_ref_exists() {
  local ref="$1"
  local output status

  if output="$(git ls-remote --exit-code origin "${ref}" 2>&1)"; then
    return 0
  else
    status=$?
  fi

  if [[ "${status}" -eq 2 ]]; then
    return 1
  fi

  die "Unable to query remote ref ${ref}: ${output}"
}

remote_ref_sha() {
  local ref="$1"
  local output

  output="$(git ls-remote origin "${ref}" 2>&1)" || die "Unable to query remote ref ${ref}: ${output}"
  awk 'NR == 1 {print $1}' <<< "${output}"
}

ensure_valid_ref() {
  local ref="$1"
  git check-ref-format "${ref}" >/dev/null 2>&1 || die "Invalid Git ref: ${ref}"
}

tag_version() {
  local prefix

  for prefix in "${TAG_PREFIXES[@]}"; do
    if [[ "${TAG:0:${#prefix}}" == "${prefix}" ]]; then
      printf '%s\n' "${TAG:${#prefix}}"
      return 0
    fi
  done

  return 1
}

tag_prefixes_for_error() {
  local joined="" prefix

  for prefix in "${TAG_PREFIXES[@]}"; do
    if [[ -z "${joined}" ]]; then
      joined="${prefix}"
    else
      joined="${joined} or ${prefix}"
    fi
  done

  printf '%s\n' "${joined}"
}

resolve_namespace() {
  NAMESPACE="production"
  RELEASE_REF_PREFIX=""
  TAG_PREFIXES=("$(release_tag_prefix_from_ref_prefix "${RELEASE_REF_PREFIX}")")
  RELEASE_BRANCH_PREFIX="$(release_branch_prefix_from_ref_prefix "${RELEASE_REF_PREFIX}")"
  MAIN_BRANCH="main"
}

parse_release_metadata() {
  require_env PR_BODY
  require_env PR_BASE_REF
  require_env PR_HEAD_REF

  [[ "${PR_HEAD_REF}" =~ ^sync/[A-Za-z0-9._-]+$ ]] || die "Release finalizer only accepts Copybara sync branches"
  git check-ref-format --branch "${PR_HEAD_REF}" >/dev/null 2>&1 || die "Invalid Copybara sync branch: ${PR_HEAD_REF}"

  TAG="$(extract_body_field "Release tag")" || die "PR body is missing 'Release tag: ...'"
  RELEASE_BRANCH="$(extract_body_field "Release branch")" || die "PR body is missing 'Release branch: ...'"
  RELEASE_KIND="$(extract_body_field "Release kind" || true)"
  RELEASE_KIND="${RELEASE_KIND:-patch}"

  case "${RELEASE_KIND}" in
    patch|minor|major) ;;
    *) die "Invalid Release kind: ${RELEASE_KIND}" ;;
  esac

  resolve_namespace

  VERSION="$(tag_version)" || die "Release tags must start with $(tag_prefixes_for_error): ${TAG}"
  [[ "${RELEASE_BRANCH}" == "${RELEASE_BRANCH_PREFIX}"* ]] || die "Release branches must start with ${RELEASE_BRANCH_PREFIX}: ${RELEASE_BRANCH}"

  ensure_valid_ref "refs/tags/${TAG}"
  ensure_valid_ref "refs/heads/${RELEASE_BRANCH}"
  ensure_valid_ref "refs/heads/${MAIN_BRANCH}"

  if [[ ! "${VERSION}" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
    die "Public finalization only supports final SemVer tags, got ${TAG}"
  fi

  MAJOR="${BASH_REMATCH[1]}"
  MINOR="${BASH_REMATCH[2]}"
  PATCH="${BASH_REMATCH[3]}"
  EXPECTED_RELEASE_BRANCH="${RELEASE_BRANCH_PREFIX}${MAJOR}.${MINOR}"
  [[ "${RELEASE_BRANCH}" == "${EXPECTED_RELEASE_BRANCH}" ]] || die "Release branch must be ${EXPECTED_RELEASE_BRANCH}, got ${RELEASE_BRANCH}"

  EFFECTIVE_RELEASE_KIND="${RELEASE_KIND}"
  if [[ "${RELEASE_KIND}" == "patch" && "${PATCH}" == "0" ]]; then
    EFFECTIVE_RELEASE_KIND="minor"
  fi

  case "${EFFECTIVE_RELEASE_KIND}" in
    patch)
      [[ "${PR_BASE_REF}" == "${RELEASE_BRANCH}" ]] || die "Patch release PRs must target ${RELEASE_BRANCH}, got ${PR_BASE_REF}"
      ;;
    minor)
      [[ "${PATCH}" == "0" ]] || die "Minor release PRs require an X.Y.0 tag, got ${TAG}"
      [[ "${PR_BASE_REF}" == "${MAIN_BRANCH}" ]] || die "Minor release PRs must target ${MAIN_BRANCH}, got ${PR_BASE_REF}"
      ;;
    major)
      [[ "${MINOR}" == "0" && "${PATCH}" == "0" ]] || die "Major release PRs require an X.0.0 tag, got ${TAG}"
      [[ "${PR_BASE_REF}" == "${MAIN_BRANCH}" ]] || die "Major release PRs must target ${MAIN_BRANCH}, got ${PR_BASE_REF}"
      ;;
  esac
}

validate_remote_preconditions() {
  if remote_ref_exists "refs/tags/${TAG}"; then
    die "Release tag already exists: ${TAG}"
  fi

  case "${EFFECTIVE_RELEASE_KIND}" in
    patch)
      remote_ref_exists "refs/heads/${RELEASE_BRANCH}" || die "Patch release branch does not exist: ${RELEASE_BRANCH}"
      ;;
    minor|major)
      if remote_ref_exists "refs/heads/${RELEASE_BRANCH}"; then
        die "Release branch already exists: ${RELEASE_BRANCH}"
      fi
      ;;
  esac
}

fetch_base_and_target() {
  require_env MERGE_COMMIT_SHA

  git fetch --no-tags origin "+refs/heads/${PR_BASE_REF}:refs/remotes/origin/${PR_BASE_REF}"
  TARGET_SHA="$(git rev-parse --verify "${MERGE_COMMIT_SHA}^{commit}")" || die "Merge commit does not exist locally: ${MERGE_COMMIT_SHA}"
  BASE_SHA="$(git rev-parse --verify "origin/${PR_BASE_REF}^{commit}")" || die "Base branch does not exist locally: ${PR_BASE_REF}"

  if ! git merge-base --is-ancestor "${TARGET_SHA}" "${BASE_SHA}"; then
    die "Merge commit ${TARGET_SHA} is not reachable from base branch ${PR_BASE_REF}@${BASE_SHA}"
  fi
}

ensure_release_branch() {
  case "${EFFECTIVE_RELEASE_KIND}" in
    patch)
      remote_ref_exists "refs/heads/${RELEASE_BRANCH}" || die "Patch release branch does not exist: ${RELEASE_BRANCH}"
      ;;
    minor|major)
      local existing_sha
      existing_sha="$(remote_ref_sha "refs/heads/${RELEASE_BRANCH}")"
      if [[ -n "${existing_sha}" ]]; then
        [[ "${existing_sha}" == "${TARGET_SHA}" ]] || die "Release branch ${RELEASE_BRANCH} exists at ${existing_sha}, expected ${TARGET_SHA}"
        note "Release branch already exists at ${TARGET_SHA}: ${RELEASE_BRANCH}"
        return
      fi

      git update-ref "refs/heads/${RELEASE_BRANCH}" "${TARGET_SHA}"
      git push origin "refs/heads/${RELEASE_BRANCH}:refs/heads/${RELEASE_BRANCH}"
      note "Created release branch ${RELEASE_BRANCH} at ${TARGET_SHA}"
      ;;
  esac
}

ensure_release_tag() {
  local existing_commit

  if remote_ref_exists "refs/tags/${TAG}"; then
    git fetch --no-tags origin "refs/tags/${TAG}:refs/tags/${TAG}"
    existing_commit="$(git rev-list -n 1 "${TAG}")"
    [[ "${existing_commit}" == "${TARGET_SHA}" ]] || die "Release tag ${TAG} points to ${existing_commit}, expected ${TARGET_SHA}"
    note "Release tag already exists at ${TARGET_SHA}: ${TAG}"
    return
  fi

  git config user.name "${GIT_COMMITTER_NAME:-github-actions[bot]}"
  git config user.email "${GIT_COMMITTER_EMAIL:-41898282+github-actions[bot]@users.noreply.github.com}"
  git tag -a "${TAG}" "${TARGET_SHA}" -m "Release ${TAG}"
  git push origin "refs/tags/${TAG}:refs/tags/${TAG}"
  note "Created release tag ${TAG} at ${TARGET_SHA}"
}

write_outputs() {
  [[ -n "${GITHUB_OUTPUT:-}" ]] || return 0
  {
    echo "namespace=${NAMESPACE}"
    echo "tag=${TAG}"
    echo "release_branch=${RELEASE_BRANCH}"
    echo "release_kind=${EFFECTIVE_RELEASE_KIND}"
  } >> "${GITHUB_OUTPUT}"
}

main() {
  local mode="${1:-}"
  case "${mode}" in
    validate|finalize) ;;
    -h|--help) usage; exit 0 ;;
    *) usage; exit 1 ;;
  esac

  parse_release_metadata
  write_outputs

  if [[ "${mode}" == "validate" ]]; then
    validate_remote_preconditions
    note "Validated ${NAMESPACE} ${EFFECTIVE_RELEASE_KIND} release ${TAG}"
    exit 0
  fi

  fetch_base_and_target
  ensure_release_branch
  ensure_release_tag
}

main "$@"
