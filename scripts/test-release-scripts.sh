#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEST_MAIN_BRANCH="test-main"
TEST_RELEASE_BRANCH_PREFIX="test-release/"
TEST_TAG_PREFIX="test/v"
RELEASE_NAMESPACE_ARGS=(
  --tag-prefix "${TEST_TAG_PREFIX}"
  --baseline-tag-prefix "v"
  --release-branch-prefix "${TEST_RELEASE_BRANCH_PREFIX}"
  --main-branch "${TEST_MAIN_BRANCH}"
)
COPYBARA_NAMESPACE_ARGS=(
  --tag-prefix "${TEST_TAG_PREFIX}"
  --release-branch-prefix "${TEST_RELEASE_BRANCH_PREFIX}"
  --main-branch "${TEST_MAIN_BRANCH}"
)

test_tag() {
  printf '%s%s\n' "${TEST_TAG_PREFIX}" "$1"
}

test_release_branch() {
  printf '%s%s\n' "${TEST_RELEASE_BRANCH_PREFIX}" "$1"
}

assert_eq() {
  local expected="$1"
  local actual="$2"
  local message="$3"

  if [[ "${expected}" != "${actual}" ]]; then
    echo "FAIL: ${message}: expected '${expected}', got '${actual}'" >&2
    exit 1
  fi
}

assert_contains() {
  local needle="$1"
  local file="$2"
  local message="$3"

  if ! grep -Fq "${needle}" "${file}"; then
    echo "FAIL: ${message}: '${needle}' not found in ${file}" >&2
    exit 1
  fi
}

new_git_repo() {
  local tmp="$1"
  local repo="${tmp}/repo"
  local origin="${tmp}/origin.git"

  git init --bare "${origin}" >/dev/null
  git clone "${origin}" "${repo}" >/dev/null 2>&1
  cd "${repo}"
  git config user.name "Release Test"
  git config user.email "release-test@example.invalid"
  git config commit.gpgsign false
  git config tag.gpgSign true
}

create_commit() {
  local message="$1"

  printf '%s\n' "${message}" >> file.txt
  git add file.txt
  git commit -m "${message}" >/dev/null
}

create_annotated_tag() {
  local tag="$1"

  git -c tag.gpgSign=false tag -a "${tag}" -m "Release ${tag}"
}

test_release_refs_namespace() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' RETURN

  bash "${REPO_ROOT}/scripts/release-refs.sh" \
    --tag "$(test_tag 0.8.0)" \
    --tag-prefix "${TEST_TAG_PREFIX}" \
    --release-branch-prefix "${TEST_RELEASE_BRANCH_PREFIX}" \
    > "${tmp}/release-refs.env"

  assert_contains "release_branch=$(test_release_branch 0.8)" \
    "${tmp}/release-refs.env" \
    "release refs use configured release branch prefix"
  assert_contains "copybara_pr_branch=sync/copybara-export-test-release-0_8" \
    "${tmp}/release-refs.env" \
    "release refs slug configured release branch"

  cd "${REPO_ROOT}"
  rm -rf "${tmp}"
  trap - RETURN
}

test_prefixed_tags_use_baseline_versions() {
  local tmp patch_rc_tag minor_rc_tag

  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' RETURN
  new_git_repo "${tmp}"

  create_commit base
  git branch -M "${TEST_MAIN_BRANCH}"
  create_commit zero-seven-six
  create_annotated_tag v0.7.6
  git checkout -b "$(test_release_branch 0.7)" >/dev/null 2>&1
  create_commit zero-seven-seven
  git push origin "${TEST_MAIN_BRANCH}" "$(test_release_branch 0.7)" --tags >/dev/null 2>&1

  PUSH_TAG=false "${REPO_ROOT}/scripts/create-release-tag.sh" \
    "${RELEASE_NAMESPACE_ARGS[@]}" \
    --source latest \
    --release-kind patch \
    --as-release-candidate \
    > "${tmp}/patch-rc.env"
  patch_rc_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/patch-rc.env")"
  assert_eq "$(test_tag 0.7.7-rc1)" "${patch_rc_tag}" "patch RC falls back to baseline tag version"

  git checkout "${TEST_MAIN_BRANCH}" >/dev/null 2>&1
  create_commit zero-eight-start
  PUSH_TAG=false "${REPO_ROOT}/scripts/create-release-tag.sh" \
    "${RELEASE_NAMESPACE_ARGS[@]}" \
    --source latest \
    --release-kind minor \
    --as-release-candidate \
    > "${tmp}/minor-rc.env"
  minor_rc_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/minor-rc.env")"
  assert_eq "$(test_tag 0.8.0-rc1)" "${minor_rc_tag}" "minor RC falls back to baseline tag version"

  cd "${REPO_ROOT}"
  rm -rf "${tmp}"
  trap - RETURN
}

test_copybara_body_escaping() {
  local tmp config main_config
  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' RETURN
  config="${tmp}/copybara.sky"
  main_config="${tmp}/copybara-main.sky"

  bash "${REPO_ROOT}/scripts/render-copybara-export-config.sh" \
    "${COPYBARA_NAMESPACE_ARGS[@]}" \
    --origin-ref "$(test_tag 0.8.0)" \
    --destination-ref "$(test_release_branch 0.8)" \
    --pr-branch sync/copybara-export-test-release-0_8 \
    --title "chore: sync $(test_tag 0.8.0) to arc-node" \
    --body $'Automated final release sync.\n\nRelease tag: test/v0.8.0\nRelease branch: test-release/0.8' \
    > "${config}"

  assert_contains 'body = "Automated final release sync.\n\nRelease tag: test/v0.8.0\nRelease branch: test-release/0.8"' \
    "${config}" \
    "Copybara body escapes newlines"
  assert_contains '".github/**"' "${config}" "Copybara preserves public-owned GitHub workflows"
  assert_contains '"scripts/render-copybara-export-config.sh"' "${config}" "Copybara excludes internal render helper"
  assert_contains '"scripts/check-public-tree-equivalence.sh"' "${config}" "Copybara excludes internal tree equivalence helper"
  assert_contains '"docs/internal"' "${config}" "Copybara excludes publicignore paths"
  assert_contains '"docs/internal/**"' "${config}" "Copybara excludes publicignore directories"

  if bash "${REPO_ROOT}/scripts/render-copybara-export-config.sh" \
    "${COPYBARA_NAMESPACE_ARGS[@]}" \
    --origin-ref "$(test_tag 0.8.0)" \
    --destination-ref "$(test_release_branch 0.8)" \
    --pr-branch sync/copybara-export..bad \
    >/dev/null 2>&1; then
    echo "FAIL: invalid Copybara PR branch should fail" >&2
    exit 1
  fi

  if bash "${REPO_ROOT}/scripts/render-copybara-export-config.sh" \
    "${COPYBARA_NAMESPACE_ARGS[@]}" \
    --origin-ref "$(test_tag 0.8.0-rc1)" \
    --destination-ref "$(test_release_branch 0.8)" \
    --pr-branch sync/copybara-export-test-release-0_8 \
    >/dev/null 2>&1; then
    echo "FAIL: RC tags should not export to the public repo" >&2
    exit 1
  fi

  if bash "${REPO_ROOT}/scripts/render-copybara-export-config.sh" \
    "${COPYBARA_NAMESPACE_ARGS[@]}" \
    --origin-ref public-repo \
    --destination-ref "${TEST_MAIN_BRANCH}" \
    --pr-branch sync/copybara-export \
    >/dev/null 2>&1; then
    echo "FAIL: public-repo mirror exports should not be supported" >&2
    exit 1
  fi

  bash "${REPO_ROOT}/scripts/render-copybara-export-config.sh" \
    "${COPYBARA_NAMESPACE_ARGS[@]}" \
    --origin-ref "$(test_tag 0.8.0)" \
    --destination-ref "${TEST_MAIN_BRANCH}" \
    --pr-branch sync/copybara-export-test-main-test-v0_8_0 \
    > "${main_config}"
  assert_contains 'destination_ref = "test-main"' "${main_config}" "minor final exports can target configured mainline"

  if bash "${REPO_ROOT}/scripts/render-copybara-export-config.sh" \
    "${COPYBARA_NAMESPACE_ARGS[@]}" \
    --origin-ref "$(test_tag 0.8.1)" \
    --destination-ref "${TEST_MAIN_BRANCH}" \
    --pr-branch sync/copybara-export-test-main-test-v0_8_1 \
    >/dev/null 2>&1; then
    echo "FAIL: patch final tags should not export to configured mainline" >&2
    exit 1
  fi
}

test_public_tree_equivalence() {
  local tmp public_origin public_work private_sha status

  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' RETURN
  new_git_repo "${tmp}"

  git branch -M "${TEST_MAIN_BRANCH}"
  mkdir -p docs/internal .github
  printf 'docs/internal\n.github\n.publicignore\n' > .publicignore
  printf 'visible\n' > README.md
  printf 'private\n' > docs/internal/runbook.md
  printf 'private workflow\n' > .github/workflow.yml
  git add .publicignore README.md docs/internal/runbook.md .github/workflow.yml
  git commit -m "private public-visible tree" >/dev/null
  private_sha="$(git rev-parse HEAD)"

  public_origin="${tmp}/public.git"
  public_work="${tmp}/public-work"
  git init --bare "${public_origin}" >/dev/null
  git clone "${public_origin}" "${public_work}" >/dev/null 2>&1
  (
    cd "${public_work}"
    git config user.name "Release Test"
    git config user.email "release-test@example.invalid"
    git config commit.gpgsign false
    git branch -M "${TEST_MAIN_BRANCH}"
    mkdir -p .github
    printf 'visible\n' > README.md
    printf 'public workflow\n' > .github/workflow.yml
    git add README.md .github/workflow.yml
    git commit -m "public tree" >/dev/null
    git push origin "${TEST_MAIN_BRANCH}" >/dev/null 2>&1
  )

  "${REPO_ROOT}/scripts/check-public-tree-equivalence.sh" \
    --private-ref "${private_sha}" \
    --public-ref "${TEST_MAIN_BRANCH}" \
    --public-repo "${public_origin}" \
    > "${tmp}/tree-match.out"

  printf 'private only\n' > docs/internal/extra.md
  git add docs/internal/extra.md
  git commit -m "private skipped change" >/dev/null
  "${REPO_ROOT}/scripts/check-public-tree-equivalence.sh" \
    --private-ref HEAD \
    --public-ref "${TEST_MAIN_BRANCH}" \
    --public-repo "${public_origin}" \
    > "${tmp}/tree-match-skipped.out"

  (
    cd "${public_work}"
    printf 'changed\n' > README.md
    git add README.md
    git commit -m "public visible mismatch" >/dev/null
    git push origin "${TEST_MAIN_BRANCH}" >/dev/null 2>&1
  )

  set +e
  "${REPO_ROOT}/scripts/check-public-tree-equivalence.sh" \
    --private-ref HEAD \
    --public-ref "${TEST_MAIN_BRANCH}" \
    --public-repo "${public_origin}" \
    > "${tmp}/tree-mismatch.out" 2>&1
  status="$?"
  set -e
  if [[ "${status}" -eq 0 ]]; then
    echo "FAIL: public-visible tree mismatches should fail" >&2
    exit 1
  fi

  cd "${REPO_ROOT}"
  rm -rf "${tmp}"
  trap - RETURN
}

test_minor_bootstrap_and_finalized_rc_ordering() {
  local tmp repo origin
  local minor_rc_tag minor_rc_type minor_rc_created_branch
  local release_branch_sha minor_rc_sha minor_rc_rerun_tag
  local main_sha release_branch_head_after_rc minor_final_tag minor_final_sha patch_after_final_tag

  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' RETURN
  new_git_repo "${tmp}"
  repo="${tmp}/repo"
  origin="${tmp}/origin.git"

  create_commit base
  git branch -M "${TEST_MAIN_BRANCH}"
  create_commit zero-seven
  main_sha="$(git rev-parse HEAD)"
  create_annotated_tag "$(test_tag 0.7.0)"
  git checkout -b "$(test_release_branch 0.7)" >/dev/null 2>&1
  create_commit zero-seven-one
  create_annotated_tag "$(test_tag 0.7.1)"
  git push origin "${TEST_MAIN_BRANCH}" "$(test_release_branch 0.7)" --tags >/dev/null 2>&1

  "${REPO_ROOT}/scripts/create-release-tag.sh" \
    "${RELEASE_NAMESPACE_ARGS[@]}" \
    --source latest \
    --release-kind minor \
    --as-release-candidate \
    > "${tmp}/minor-rc.env"

  minor_rc_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/minor-rc.env")"
  minor_rc_type="$(git cat-file -t "${minor_rc_tag}")"
  minor_rc_created_branch="$(awk -F= '$1 == "created_release_branch" {print $2}' "${tmp}/minor-rc.env")"
  release_branch_sha="$(git ls-remote --heads origin "$(test_release_branch 0.8)" | awk '{print $1}')"
  minor_rc_sha="$(git rev-list -n1 "${minor_rc_tag}")"

  assert_eq "$(test_tag 0.8.0-rc1)" "${minor_rc_tag}" "minor RC tag"
  assert_eq "tag" "${minor_rc_type}" "release tags are annotated"
  assert_eq "true" "${minor_rc_created_branch}" "minor RC creates private release branch"
  assert_eq "${main_sha}" "${minor_rc_sha}" "minor RC tags main source commit"
  assert_eq "${minor_rc_sha}" "${release_branch_sha}" "new private release branch points at RC commit"

  "${REPO_ROOT}/scripts/create-release-tag.sh" \
    "${RELEASE_NAMESPACE_ARGS[@]}" \
    --source latest \
    --release-kind minor \
    --as-release-candidate \
    > "${tmp}/minor-rc-rerun.env"

  minor_rc_rerun_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/minor-rc-rerun.env")"
  assert_eq "$(test_tag 0.8.0-rc1)" "${minor_rc_rerun_tag}" "same-commit RC rerun reuses tag"

  git checkout "$(test_release_branch 0.8)" >/dev/null 2>&1
  create_commit post-rc-change
  release_branch_head_after_rc="$(git rev-parse HEAD)"

  "${REPO_ROOT}/scripts/create-release-tag.sh" \
    "${RELEASE_NAMESPACE_ARGS[@]}" \
    --source latest \
    --release-kind minor \
    > "${tmp}/minor-final.env"
  minor_final_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/minor-final.env")"
  minor_final_sha="$(git rev-list -n1 "${minor_final_tag}")"
  assert_eq "$(test_tag 0.8.0)" "${minor_final_tag}" "minor final promotes active RC"
  assert_eq "${minor_rc_sha}" "${minor_final_sha}" "minor final tags the promoted RC commit"
  if [[ "${minor_final_sha}" == "${release_branch_head_after_rc}" ]]; then
    echo "FAIL: minor final should not tag release branch HEAD after the RC has been tested" >&2
    exit 1
  fi

  "${REPO_ROOT}/scripts/create-release-tag.sh" \
    "${RELEASE_NAMESPACE_ARGS[@]}" \
    --source latest \
    --release-kind patch \
    --as-release-candidate \
    > "${tmp}/patch-after-final.env"
  patch_after_final_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/patch-after-final.env")"
  assert_eq "$(test_tag 0.8.1-rc1)" "${patch_after_final_tag}" "finalized RC does not remain the latest release"

  cd "${REPO_ROOT}"
  rm -rf "${tmp}"
  trap - RETURN
}

test_patch_release_requires_diff_from_latest_final() {
  local tmp patch_rc_tag status

  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' RETURN
  new_git_repo "${tmp}"

  create_commit base
  git branch -M "${TEST_MAIN_BRANCH}"
  git checkout -b "$(test_release_branch 0.7)" >/dev/null 2>&1
  create_commit zero-seven-one
  create_annotated_tag "$(test_tag 0.7.1)"
  git push origin "${TEST_MAIN_BRANCH}" "$(test_release_branch 0.7)" --tags >/dev/null 2>&1

  set +e
  PUSH_TAG=false "${REPO_ROOT}/scripts/create-release-tag.sh" \
    "${RELEASE_NAMESPACE_ARGS[@]}" \
    --source latest \
    --release-kind patch \
    --as-release-candidate \
    > "${tmp}/no-diff-patch.out" 2>&1
  status="$?"
  set -e
  if [[ "${status}" -eq 0 ]]; then
    echo "FAIL: new patch RC without source diff should fail" >&2
    exit 1
  fi
  assert_contains "No changes since $(test_tag 0.7.1); refusing to create $(test_tag 0.7.2-rc1)" \
    "${tmp}/no-diff-patch.out" \
    "new patch RC requires a diff from the latest final tag"

  create_commit zero-seven-two
  PUSH_TAG=false "${REPO_ROOT}/scripts/create-release-tag.sh" \
    "${RELEASE_NAMESPACE_ARGS[@]}" \
    --source latest \
    --release-kind patch \
    --as-release-candidate \
    > "${tmp}/patch-rc.env"
  patch_rc_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/patch-rc.env")"
  assert_eq "$(test_tag 0.7.2-rc1)" "${patch_rc_tag}" "patch RC with a source diff is allowed"

  cd "${REPO_ROOT}"
  rm -rf "${tmp}"
  trap - RETURN
}

test_final_release_rerun_reuses_existing_candidate_tag() {
  local tmp release_sha final_tag final_sha final_created rerun_tag rerun_sha rerun_created

  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' RETURN
  new_git_repo "${tmp}"

  create_commit base
  git branch -M "${TEST_MAIN_BRANCH}"
  git checkout -b "$(test_release_branch 0.7)" >/dev/null 2>&1
  create_commit zero-seven-one
  create_annotated_tag "$(test_tag 0.7.1)"
  create_commit zero-seven-two
  release_sha="$(git rev-parse HEAD)"
  git push origin "${TEST_MAIN_BRANCH}" "$(test_release_branch 0.7)" --tags >/dev/null 2>&1

  PUSH_TAG=false "${REPO_ROOT}/scripts/create-release-tag.sh" \
    "${RELEASE_NAMESPACE_ARGS[@]}" \
    --source latest \
    --release-kind patch \
    > "${tmp}/final.env"

  final_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/final.env")"
  final_sha="$(awk -F= '$1 == "sha" {print $2}' "${tmp}/final.env")"
  final_created="$(awk -F= '$1 == "created_new_tag" {print $2}' "${tmp}/final.env")"
  assert_eq "$(test_tag 0.7.2)" "${final_tag}" "patch final tag"
  assert_eq "${release_sha}" "${final_sha}" "patch final target"
  assert_eq "true" "${final_created}" "first patch final creates tag"

  PUSH_TAG=false "${REPO_ROOT}/scripts/create-release-tag.sh" \
    "${RELEASE_NAMESPACE_ARGS[@]}" \
    --source latest \
    --release-kind patch \
    > "${tmp}/final-rerun.env" 2> "${tmp}/final-rerun.err"

  rerun_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/final-rerun.env")"
  rerun_sha="$(awk -F= '$1 == "sha" {print $2}' "${tmp}/final-rerun.env")"
  rerun_created="$(awk -F= '$1 == "created_new_tag" {print $2}' "${tmp}/final-rerun.env")"
  assert_eq "$(test_tag 0.7.2)" "${rerun_tag}" "patch final rerun reuses candidate tag"
  assert_eq "${release_sha}" "${rerun_sha}" "patch final rerun target"
  assert_eq "false" "${rerun_created}" "patch final rerun does not create a new tag"
  assert_contains "Tag already exists at target commit: $(test_tag 0.7.2)" \
    "${tmp}/final-rerun.err" \
    "patch final rerun reports existing tag reuse"

  cd "${REPO_ROOT}"
  rm -rf "${tmp}"
  trap - RETURN
}

test_direct_minor_honors_release_kind() {
  local tmp minor_tag

  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' RETURN
  new_git_repo "${tmp}"

  create_commit base
  git branch -M "${TEST_MAIN_BRANCH}"
  create_commit zero-eight-start
  git checkout -b "$(test_release_branch 0.7)" >/dev/null 2>&1
  create_commit zero-seven-one
  create_annotated_tag "$(test_tag 0.7.1)"
  git push origin "${TEST_MAIN_BRANCH}" "$(test_release_branch 0.7)" --tags >/dev/null 2>&1

  "${REPO_ROOT}/scripts/create-release-tag.sh" \
    "${RELEASE_NAMESPACE_ARGS[@]}" \
    --source "${TEST_MAIN_BRANCH}" \
    --release-kind minor \
    > "${tmp}/minor-final.env"

  minor_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/minor-final.env")"
  assert_eq "$(test_tag 0.8.0)" "${minor_tag}" "direct minor final honors release kind"

  cd "${REPO_ROOT}"
  rm -rf "${tmp}"
  trap - RETURN
}

test_major_after_finalized_rc() {
  local tmp major_tag major_branch created_branch

  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' RETURN
  new_git_repo "${tmp}"

  create_commit base
  git branch -M "${TEST_MAIN_BRANCH}"
  git checkout -b "$(test_release_branch 0.8)" >/dev/null 2>&1
  create_annotated_tag "$(test_tag 0.8.0-rc1)"
  create_annotated_tag "$(test_tag 0.8.0)"
  git checkout "${TEST_MAIN_BRANCH}" >/dev/null 2>&1
  create_commit one-zero-start
  git push origin "${TEST_MAIN_BRANCH}" "$(test_release_branch 0.8)" --tags >/dev/null 2>&1

  "${REPO_ROOT}/scripts/create-release-tag.sh" \
    "${RELEASE_NAMESPACE_ARGS[@]}" \
    --source latest \
    --release-kind major \
    --as-release-candidate \
    > "${tmp}/major-rc.env"

  major_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/major-rc.env")"
  major_branch="$(awk -F= '$1 == "release_branch" {print $2}' "${tmp}/major-rc.env")"
  created_branch="$(awk -F= '$1 == "created_release_branch" {print $2}' "${tmp}/major-rc.env")"

  assert_eq "$(test_tag 1.0.0-rc1)" "${major_tag}" "major RC after finalized RC"
  assert_eq "$(test_release_branch 1.0)" "${major_branch}" "major RC release branch"
  assert_eq "true" "${created_branch}" "major RC creates private release branch"

  cd "${REPO_ROOT}"
  rm -rf "${tmp}"
  trap - RETURN
}

test_release_refs_namespace
test_prefixed_tags_use_baseline_versions
test_copybara_body_escaping
test_public_tree_equivalence
test_minor_bootstrap_and_finalized_rc_ordering
test_patch_release_requires_diff_from_latest_final
test_final_release_rerun_reuses_existing_candidate_tag
test_direct_minor_honors_release_kind
test_major_after_finalized_rc

echo "release script tests passed"
