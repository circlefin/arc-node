#!/usr/bin/env bash
set -euo pipefail

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
  RELEASE_NAMESPACE      Optional: auto, production, or test. Defaults to auto.
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
  git ls-remote --exit-code origin "${ref}" >/dev/null 2>&1
}

remote_ref_sha() {
  local ref="$1"
  git ls-remote origin "${ref}" | awk 'NR == 1 {print $1}'
}

ensure_valid_ref() {
  local ref="$1"
  git check-ref-format "${ref}" >/dev/null 2>&1 || die "Invalid Git ref: ${ref}"
}

resolve_namespace() {
  local requested="${RELEASE_NAMESPACE:-auto}"

  case "${requested}" in
    auto)
      if [[ "${TAG}" == test/v* || "${RELEASE_BRANCH}" == test-release/* || "${PR_BASE_REF}" == test-main || "${PR_BASE_REF}" == test-release/* ]]; then
        NAMESPACE="test"
      else
        NAMESPACE="production"
      fi
      ;;
    production|test)
      NAMESPACE="${requested}"
      ;;
    *)
      die "Invalid RELEASE_NAMESPACE: ${requested}"
      ;;
  esac

  case "${NAMESPACE}" in
    production)
      TAG_PREFIX="v"
      RELEASE_BRANCH_PREFIX="release/"
      MAIN_BRANCH="main"
      ;;
    test)
      TAG_PREFIX="test/v"
      RELEASE_BRANCH_PREFIX="test-release/"
      MAIN_BRANCH="test-main"
      ;;
  esac
}

parse_release_metadata() {
  require_env PR_BODY
  require_env PR_BASE_REF
  require_env PR_HEAD_REF

  [[ "${PR_HEAD_REF}" == sync/copybara-export-* ]] || die "Release finalizer only accepts Copybara export branches"

  TAG="$(extract_body_field "Release tag")" || die "PR body is missing 'Release tag: ...'"
  RELEASE_BRANCH="$(extract_body_field "Release branch")" || die "PR body is missing 'Release branch: ...'"
  RELEASE_KIND="$(extract_body_field "Release kind" || true)"
  RELEASE_KIND="${RELEASE_KIND:-patch}"

  case "${RELEASE_KIND}" in
    patch|minor|major) ;;
    *) die "Invalid Release kind: ${RELEASE_KIND}" ;;
  esac

  resolve_namespace

  [[ "${TAG}" == "${TAG_PREFIX}"* ]] || die "${NAMESPACE} release tags must start with ${TAG_PREFIX}: ${TAG}"
  [[ "${RELEASE_BRANCH}" == "${RELEASE_BRANCH_PREFIX}"* ]] || die "${NAMESPACE} release branches must start with ${RELEASE_BRANCH_PREFIX}: ${RELEASE_BRANCH}"

  ensure_valid_ref "refs/tags/${TAG}"
  ensure_valid_ref "refs/heads/${RELEASE_BRANCH}"
  ensure_valid_ref "refs/heads/${MAIN_BRANCH}"

  VERSION="${TAG:${#TAG_PREFIX}}"
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

  if [[ "${BASE_SHA}" != "${TARGET_SHA}" ]]; then
    die "Base branch ${PR_BASE_REF} is at ${BASE_SHA}, expected merge commit ${TARGET_SHA}"
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
