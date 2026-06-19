#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  test-release-scenario.sh prepare-first-rc
  test-release-scenario.sh post-rc-backports
  test-release-scenario.sh validate-public-pr
  test-release-scenario.sh cleanup-scenario-files

Environment:
  CIRCLE_EMU_API_TOKEN      Token for the private repository.
  CIRCLEFIN_API_TOKEN       Token for the public repository.
  FINAL_TAG                 Final release tag, required for validate-public-pr.
USAGE
}

PHASE="${1:-}"
if [[ -z "${PHASE}" ]]; then
  usage
  exit 1
fi

PRIVATE_REPO="${PRIVATE_REPO:-crcl-main/circle-chain-reth}"
PUBLIC_REPO="${PUBLIC_REPO:-circlefin/arc-node}"
PRIVATE_TOKEN="${CIRCLE_EMU_API_TOKEN:?missing CIRCLE_EMU_API_TOKEN}"
PUBLIC_TOKEN="${CIRCLEFIN_API_TOKEN:?missing CIRCLEFIN_API_TOKEN}"
TEST_MAIN_BRANCH="${TEST_MAIN_BRANCH:-test-main}"
TEST_RELEASE_BRANCH="${TEST_RELEASE_BRANCH:-test-release/0.7}"
TEST_RELEASE_BRANCH_PREFIX="${TEST_RELEASE_BRANCH_PREFIX:-test-release/}"
TEST_TAG_PREFIX="${TEST_TAG_PREFIX:-test/v}"
RELEASE_LABEL="${RELEASE_LABEL:-add-to-current-release}"
RUN_ID="${GITHUB_RUN_ID:-local}"
SCENARIO_MARKER="[test-release-scenario]"
SCENARIO_DIR="docs/internal/release-scenario/${RUN_ID}"
SCENARIO_BRANCH_ROOT="test/release-scenario-${RUN_ID}"
BACKPORT_BRANCH_ROOT="test/backport-scenario-${RUN_ID}"

gh_private() {
  GH_TOKEN="${PRIVATE_TOKEN}" gh "$@"
}

gh_public() {
  GH_TOKEN="${PUBLIC_TOKEN}" gh "$@"
}

list_matching_refs() {
  local token="$1"
  local repo="$2"
  local prefix="$3"

  GH_TOKEN="${token}" gh api "repos/${repo}/git/matching-refs/${prefix}" --jq '.[].ref' 2>/dev/null || true
}

ref_sha() {
  local token="$1"
  local repo="$2"
  local ref="$3"

  GH_TOKEN="${token}" gh api "repos/${repo}/git/ref/${ref}" --jq '.object.sha'
}

parent_sha_for_ref() {
  local token="$1"
  local repo="$2"
  local ref="$3"
  local sha parent_sha

  sha="$(ref_sha "${token}" "${repo}" "${ref}")"
  parent_sha="$(GH_TOKEN="${token}" gh api "repos/${repo}/commits/${sha}" --jq '.parents[0].sha // empty')"
  if [[ -z "${parent_sha}" ]]; then
    echo "Cannot find parent commit for ${repo}:refs/${ref}@${sha}" >&2
    exit 1
  fi
  printf '%s\n' "${parent_sha}"
}

delete_matching_refs() {
  local token="$1"
  local repo="$2"
  local prefix="$3"
  local ref short_ref

  while IFS= read -r ref; do
    [[ -z "${ref}" ]] && continue
    short_ref="${ref#refs/}"
    echo "Deleting ${repo}:${ref}"
    GH_TOKEN="${token}" gh api -X DELETE "repos/${repo}/git/refs/${short_ref}" >/dev/null
  done < <(list_matching_refs "${token}" "${repo}" "${prefix}")
}

ensure_ref_from_sha() {
  local token="$1"
  local repo="$2"
  local ref="$3"
  local source_label="$4"
  local sha="$5"

  if GH_TOKEN="${token}" gh api "repos/${repo}/git/ref/${ref}" >/dev/null 2>&1; then
    echo "${repo}:refs/${ref} already exists"
    return
  fi

  echo "Creating ${repo}:refs/${ref} from ${source_label}@${sha}"
  GH_TOKEN="${token}" gh api -X POST "repos/${repo}/git/refs" \
    -f ref="refs/${ref}" \
    -f sha="${sha}" \
    >/dev/null
}

ensure_ref_from_ref() {
  local token="$1"
  local repo="$2"
  local ref="$3"
  local source_ref="$4"
  local sha

  sha="$(ref_sha "${token}" "${repo}" "${source_ref}")"
  ensure_ref_from_sha "${token}" "${repo}" "${ref}" "refs/${source_ref}" "${sha}"
}

close_scenario_prs() {
  local repo="$1"
  local token="$2"
  local pr

  while IFS= read -r pr; do
    [[ -z "${pr}" ]] && continue
    echo "Closing ${repo}#${pr}"
    GH_TOKEN="${token}" gh api -X PATCH "repos/${repo}/pulls/${pr}" -f state=closed >/dev/null
  done < <(
    GH_TOKEN="${token}" gh api "repos/${repo}/pulls" \
      -X GET \
      -f state=open \
      -f per_page=100 \
      --paginate \
      --jq '.[] | select(.title | startswith("[test-release-scenario]")) | .number'
  )
}

close_stale_public_release_prs() {
  local pr

  while IFS= read -r pr; do
    [[ -z "${pr}" ]] && continue
    echo "Closing ${PUBLIC_REPO}#${pr}"
    gh_public api -X PATCH "repos/${PUBLIC_REPO}/pulls/${pr}" -f state=closed >/dev/null
  done < <(
    gh_public api "repos/${PUBLIC_REPO}/pulls" \
      -X GET \
      -f state=open \
      -f per_page=100 \
      --paginate \
      --jq '.[] | select((.head.ref | startswith("sync/copybara-export-test-")) or (.base.ref == "test-main") or (.base.ref | startswith("test-release/"))) | .number'
  )
}

delete_test_releases() {
  local release

  while IFS= read -r release; do
    [[ -z "${release}" ]] && continue
    echo "Deleting ${PRIVATE_REPO} release ${release#* }"
    gh_private api -X DELETE "repos/${PRIVATE_REPO}/releases/${release%% *}" >/dev/null
  done < <(
    gh_private api "repos/${PRIVATE_REPO}/releases" \
      -X GET \
      -f per_page=100 \
      --paginate \
      --jq '.[] | select(.tag_name | startswith("test/")) | "\(.id) \(.tag_name)"'
  )
}

ensure_release_label() {
  if gh_private api "repos/${PRIVATE_REPO}/labels/${RELEASE_LABEL}" >/dev/null 2>&1; then
    return
  fi

  gh_private api -X POST "repos/${PRIVATE_REPO}/labels" \
    -f name="${RELEASE_LABEL}" \
    -f color=0E8A16 \
    -f description="Backport this PR to the current release branch" \
    >/dev/null
}

seed_test_refs() {
  ensure_ref_from_ref "${PRIVATE_TOKEN}" "${PRIVATE_REPO}" "heads/${TEST_MAIN_BRANCH}" "heads/main"
  ensure_ref_from_sha \
    "${PRIVATE_TOKEN}" \
    "${PRIVATE_REPO}" \
    "heads/${TEST_RELEASE_BRANCH}" \
    "parent of refs/heads/${TEST_MAIN_BRANCH}" \
    "$(parent_sha_for_ref "${PRIVATE_TOKEN}" "${PRIVATE_REPO}" "heads/${TEST_MAIN_BRANCH}")"

  ensure_ref_from_ref "${PUBLIC_TOKEN}" "${PUBLIC_REPO}" "heads/${TEST_MAIN_BRANCH}" "heads/main"
  ensure_ref_from_sha \
    "${PUBLIC_TOKEN}" \
    "${PUBLIC_REPO}" \
    "heads/${TEST_RELEASE_BRANCH}" \
    "parent of refs/heads/${TEST_MAIN_BRANCH}" \
    "$(parent_sha_for_ref "${PUBLIC_TOKEN}" "${PUBLIC_REPO}" "heads/${TEST_MAIN_BRANCH}")"
}

cleanup_test_state() {
  close_scenario_prs "${PRIVATE_REPO}" "${PRIVATE_TOKEN}"
  close_stale_public_release_prs
  delete_test_releases

  delete_matching_refs "${PRIVATE_TOKEN}" "${PRIVATE_REPO}" "heads/${TEST_RELEASE_BRANCH_PREFIX}"
  delete_matching_refs "${PRIVATE_TOKEN}" "${PRIVATE_REPO}" "heads/test/release-scenario"
  delete_matching_refs "${PRIVATE_TOKEN}" "${PRIVATE_REPO}" "heads/test/backport-scenario"
  delete_matching_refs "${PRIVATE_TOKEN}" "${PRIVATE_REPO}" "tags/test"
  delete_matching_refs "${PUBLIC_TOKEN}" "${PUBLIC_REPO}" "heads/${TEST_RELEASE_BRANCH_PREFIX}"
  delete_matching_refs "${PUBLIC_TOKEN}" "${PUBLIC_REPO}" "heads/sync/copybara-export-test"
  delete_matching_refs "${PUBLIC_TOKEN}" "${PUBLIC_REPO}" "tags/test"
}

configure_git() {
  git config user.name "circle-github-action-bot"
  git config user.email "circle-github-actions@circle.com"
  git config commit.gpgsign false
}

wait_for_mergeable() {
  local repo="$1"
  local token="$2"
  local pr="$3"
  local mergeable

  for _ in {1..30}; do
    mergeable="$(GH_TOKEN="${token}" gh api "repos/${repo}/pulls/${pr}" --jq '.mergeable')" || mergeable="null"
    if [[ "${mergeable}" != "null" ]]; then
      return
    fi
    sleep 2
  done
}

merge_pr() {
  local repo="$1"
  local token="$2"
  local pr="$3"
  local description="$4"
  local merge_json

  wait_for_mergeable "${repo}" "${token}" "${pr}"
  merge_json="$(
    GH_TOKEN="${token}" gh api -X PUT "repos/${repo}/pulls/${pr}/merge" \
      -f merge_method=merge \
      -f commit_title="${SCENARIO_MARKER} merge ${description} PR #${pr}" \
      -f commit_message="Automated test release scenario merge." \
  )"
  jq -r '.sha' <<< "${merge_json}"
}

create_and_merge_test_main_pr() {
  local slot="$1"
  local should_label="$2"
  local branch="${SCENARIO_BRANCH_ROOT}/${slot}"
  local path="${SCENARIO_DIR}/${slot}.txt"
  local title="${SCENARIO_MARKER} ${slot}"
  local url pr merge_sha

  git fetch origin "+refs/heads/${TEST_MAIN_BRANCH}:refs/remotes/origin/${TEST_MAIN_BRANCH}"
  git switch --force-create "${branch}" "origin/${TEST_MAIN_BRANCH}"
  mkdir -p "$(dirname "${path}")"
  {
    echo "scenario=${RUN_ID}"
    echo "slot=${slot}"
    echo "label=${should_label}"
  } > "${path}"
  git add "${path}"
  git commit -m "${SCENARIO_MARKER} ${slot}" >/dev/null
  git push --force-with-lease origin "HEAD:refs/heads/${branch}" >/dev/null

  url="$(
    gh_private pr create \
      --repo "${PRIVATE_REPO}" \
      --base "${TEST_MAIN_BRANCH}" \
      --head "${branch}" \
      --title "${title}" \
      --body "Temporary PR for test release validation run ${RUN_ID}."
  )"
  pr="${url##*/}"
  if [[ "${should_label}" == "true" ]]; then
    gh_private pr edit "${pr}" --repo "${PRIVATE_REPO}" --add-label "${RELEASE_LABEL}" >/dev/null
  fi

  merge_sha="$(merge_pr "${PRIVATE_REPO}" "${PRIVATE_TOKEN}" "${pr}" "${slot}")"
  echo "${pr} ${merge_sha}"
}

create_and_merge_backport_pr() {
  local source_pr="$1"
  local merge_sha="$2"
  local target_slug="${TEST_RELEASE_BRANCH//\//-}"
  local branch="${BACKPORT_BRANCH_ROOT}/pr-${source_pr}-to-${target_slug}"
  local url backport_pr backport_merge_sha parent_count

  git fetch origin \
    "+refs/heads/${TEST_MAIN_BRANCH}:refs/remotes/origin/${TEST_MAIN_BRANCH}" \
    "+refs/heads/${TEST_RELEASE_BRANCH}:refs/remotes/origin/${TEST_RELEASE_BRANCH}"
  git switch --force-create "${branch}" "origin/${TEST_RELEASE_BRANCH}"

  parent_count="$(git cat-file -p "${merge_sha}" | grep -c '^parent ')"
  cherry_pick_args=(-x)
  if (( parent_count > 1 )); then
    cherry_pick_args+=(-m 1)
  fi
  cherry_pick_args+=("${merge_sha}")
  git cherry-pick "${cherry_pick_args[@]}" >/dev/null
  git push --force-with-lease origin "HEAD:refs/heads/${branch}" >/dev/null

  url="$(
    gh_private pr create \
      --repo "${PRIVATE_REPO}" \
      --base "${TEST_RELEASE_BRANCH}" \
      --head "${branch}" \
      --title "${SCENARIO_MARKER} backport #${source_pr} to ${TEST_RELEASE_BRANCH}" \
      --body "Temporary backport PR for test release validation run ${RUN_ID}."
  )"
  backport_pr="${url##*/}"
  backport_merge_sha="$(merge_pr "${PRIVATE_REPO}" "${PRIVATE_TOKEN}" "${backport_pr}" "backport ${source_pr}")"
  echo "${backport_pr} ${backport_merge_sha}"
}

assert_path_in_ref() {
  local ref="$1"
  local path="$2"

  if ! git cat-file -e "${ref}:${path}" 2>/dev/null; then
    echo "Expected ${path} to exist in ${ref}" >&2
    exit 1
  fi
}

assert_path_not_in_ref() {
  local ref="$1"
  local path="$2"

  if git cat-file -e "${ref}:${path}" 2>/dev/null; then
    echo "Did not expect ${path} to exist in ${ref}" >&2
    exit 1
  fi
}

validate_release_branch_markers() {
  git fetch origin "+refs/heads/${TEST_RELEASE_BRANCH}:refs/remotes/origin/${TEST_RELEASE_BRANCH}" --tags
  assert_path_in_ref "origin/${TEST_RELEASE_BRANCH}" "${SCENARIO_DIR}/first-labeled.txt"
  assert_path_not_in_ref "origin/${TEST_RELEASE_BRANCH}" "${SCENARIO_DIR}/second-unlabeled.txt"
}

prepare_first_rc() {
  local created first_pr first_merge_sha

  configure_git
  cleanup_test_state
  seed_test_refs
  ensure_release_label

  created="$(create_and_merge_test_main_pr first-labeled true)"
  first_pr="${created%% *}"
  first_merge_sha="${created##* }"
  create_and_merge_backport_pr "${first_pr}" "${first_merge_sha}" >/dev/null
  validate_release_branch_markers
}

post_rc_backports() {
  local created third_pr third_merge_sha

  configure_git
  ensure_release_label

  create_and_merge_test_main_pr second-unlabeled false >/dev/null
  validate_release_branch_markers

  created="$(create_and_merge_test_main_pr third-labeled true)"
  third_pr="${created%% *}"
  third_merge_sha="${created##* }"
  create_and_merge_backport_pr "${third_pr}" "${third_merge_sha}" >/dev/null

  validate_release_branch_markers
  assert_path_in_ref "origin/${TEST_RELEASE_BRANCH}" "${SCENARIO_DIR}/third-labeled.txt"
}

validate_public_pr() {
  local final_tag="${FINAL_TAG:?missing FINAL_TAG}"
  local refs_file copybara_pr_branch pr

  git fetch origin "+refs/heads/${TEST_RELEASE_BRANCH}:refs/remotes/origin/${TEST_RELEASE_BRANCH}" --tags
  assert_path_in_ref "${final_tag}" "${SCENARIO_DIR}/first-labeled.txt"
  assert_path_not_in_ref "${final_tag}" "${SCENARIO_DIR}/second-unlabeled.txt"
  assert_path_in_ref "${final_tag}" "${SCENARIO_DIR}/third-labeled.txt"

  refs_file="$(mktemp)"
  bash scripts/release-refs.sh \
    --tag "${final_tag}" \
    --tag-prefix "${TEST_TAG_PREFIX}" \
    --release-branch-prefix "${TEST_RELEASE_BRANCH_PREFIX}" \
    > "${refs_file}"
  copybara_pr_branch="$(awk -F= '$1 == "copybara_pr_branch" {print $2}' "${refs_file}")"
  if [[ -z "${copybara_pr_branch}" ]]; then
    echo "Could not resolve Copybara PR branch for ${final_tag}" >&2
    exit 1
  fi

  pr="$(
    gh_public pr list \
      --repo "${PUBLIC_REPO}" \
      --base "${TEST_RELEASE_BRANCH}" \
      --head "${copybara_pr_branch}" \
      --state open \
      --json number \
      --jq '.[0].number // empty'
  )"
  if [[ -z "${pr}" ]]; then
    echo "Expected an open public PR from ${copybara_pr_branch} to ${TEST_RELEASE_BRANCH}" >&2
    exit 1
  fi

  bash scripts/check-public-tree-equivalence.sh \
    --private-ref "${final_tag}" \
    --public-ref "${copybara_pr_branch}"

  gh_public pr view "${pr}" --repo "${PUBLIC_REPO}" --json number,url,state,baseRefName,headRefName,mergeable
}

cleanup_scenario_files() {
  local branch="${SCENARIO_BRANCH_ROOT}/cleanup"
  local url pr

  configure_git
  git fetch origin "+refs/heads/${TEST_MAIN_BRANCH}:refs/remotes/origin/${TEST_MAIN_BRANCH}"
  git switch --force-create "${branch}" "origin/${TEST_MAIN_BRANCH}"
  if [[ ! -e "${SCENARIO_DIR}" ]]; then
    echo "No scenario files to clean up."
    return
  fi

  rm -rf "${SCENARIO_DIR}"
  git add -A docs/internal
  if git diff --cached --quiet; then
    echo "No scenario cleanup changes."
    return
  fi

  git commit -m "${SCENARIO_MARKER} cleanup scenario files" >/dev/null
  git push --force-with-lease origin "HEAD:refs/heads/${branch}" >/dev/null
  url="$(
    gh_private pr create \
      --repo "${PRIVATE_REPO}" \
      --base "${TEST_MAIN_BRANCH}" \
      --head "${branch}" \
      --title "${SCENARIO_MARKER} cleanup scenario files" \
      --body "Remove temporary files from test release validation run ${RUN_ID}."
  )"
  pr="${url##*/}"
  merge_pr "${PRIVATE_REPO}" "${PRIVATE_TOKEN}" "${pr}" "cleanup" >/dev/null
}

case "${PHASE}" in
  prepare-first-rc) prepare_first_rc ;;
  post-rc-backports) post_rc_backports ;;
  validate-public-pr) validate_public_pr ;;
  cleanup-scenario-files) cleanup_scenario_files ;;
  -h|--help) usage ;;
  *) echo "Unknown phase: ${PHASE}" >&2; usage; exit 1 ;;
esac
