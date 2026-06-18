#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

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

test_copybara_body_escaping() {
  local tmp config main_config
  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' RETURN
  config="${tmp}/copybara.sky"
  main_config="${tmp}/copybara-main.sky"

  bash "${REPO_ROOT}/scripts/render-copybara-export-config.sh" \
    --origin-ref v0.8.0 \
    --destination-ref release/0.8 \
    --pr-branch sync/copybara-export-release-0.8 \
    --title "chore: sync v0.8.0 to arc-node" \
    --body $'Automated final release sync.\n\nRelease tag: v0.8.0\nRelease branch: release/0.8' \
    > "${config}"

  assert_contains 'body = "Automated final release sync.\n\nRelease tag: v0.8.0\nRelease branch: release/0.8"' \
    "${config}" \
    "Copybara body escapes newlines"
  assert_contains '".github/**"' "${config}" "Copybara preserves public-owned GitHub workflows"
  assert_contains '"scripts/render-copybara-export-config.sh"' "${config}" "Copybara excludes internal render helper"
  assert_contains '"scripts/check-public-tree-equivalence.sh"' "${config}" "Copybara excludes internal tree equivalence helper"
  assert_contains '"docs/internal"' "${config}" "Copybara excludes publicignore paths"
  assert_contains '"docs/internal/**"' "${config}" "Copybara excludes publicignore directories"

  if bash "${REPO_ROOT}/scripts/render-copybara-export-config.sh" \
    --origin-ref v0.8.0 \
    --destination-ref release/0.8 \
    --pr-branch sync/copybara-export..bad \
    >/dev/null 2>&1; then
    echo "FAIL: invalid Copybara PR branch should fail" >&2
    exit 1
  fi

  if bash "${REPO_ROOT}/scripts/render-copybara-export-config.sh" \
    --origin-ref v0.8.0-rc1 \
    --destination-ref release/0.8 \
    --pr-branch sync/copybara-export-release-0.8 \
    >/dev/null 2>&1; then
    echo "FAIL: RC tags should not export to the public repo" >&2
    exit 1
  fi

  bash "${REPO_ROOT}/scripts/render-copybara-export-config.sh" \
    --origin-ref v0.8.0 \
    --destination-ref main \
    --pr-branch sync/copybara-export-main-v0.8.0 \
    > "${main_config}"
  assert_contains 'destination_ref = "main"' "${main_config}" "minor final exports can target public main"

  if bash "${REPO_ROOT}/scripts/render-copybara-export-config.sh" \
    --origin-ref v0.8.1 \
    --destination-ref main \
    --pr-branch sync/copybara-export-main-v0.8.1 \
    >/dev/null 2>&1; then
    echo "FAIL: patch final tags should not export to public main" >&2
    exit 1
  fi
}

test_namespaced_release_refs_and_copybara_config() {
  local tmp env config
  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' RETURN
  env="${tmp}/release-refs.env"
  config="${tmp}/copybara.sky"

  bash "${REPO_ROOT}/scripts/release-refs.sh" \
    --tag test/v0.8.1-rc2 \
    --tag-prefix test/v \
    --release-branch-prefix test-release/ \
    > "${env}"

  assert_contains "version=0.8.1-rc2" "${env}" "namespaced tag strips configured prefix"
  assert_contains "release_branch=test-release/0.8" "${env}" "namespaced tag resolves configured release branch"
  assert_contains "copybara_pr_branch=sync/copybara-export-test-release-0_8" \
    "${env}" \
    "namespaced release branch resolves Copybara PR branch"

  bash "${REPO_ROOT}/scripts/render-copybara-export-config.sh" \
    --origin-ref test/v0.8.1 \
    --destination-ref test-release/0.8 \
    --pr-branch sync/copybara-export-test-release-0.8 \
    --tag-prefix test/v \
    --release-branch-prefix test-release/ \
    > "${config}"

  assert_contains 'ref = "test/v0.8.1"' "${config}" "namespaced Copybara origin ref"
  assert_contains 'destination_ref = "test-release/0.8"' "${config}" "namespaced Copybara destination ref"

  if bash "${REPO_ROOT}/scripts/render-copybara-export-config.sh" \
    --origin-ref test/v0.8.1-rc1 \
    --destination-ref test-release/0.8 \
    --pr-branch sync/copybara-export-test-release-0.8 \
    --tag-prefix test/v \
    --release-branch-prefix test-release/ \
    >/dev/null 2>&1; then
    echo "FAIL: namespaced RC tags should not export to the public repo" >&2
    exit 1
  fi

  cd "${REPO_ROOT}"
  rm -rf "${tmp}"
  trap - RETURN
}

test_resolve_public_sync_refs() {
  local tmp env
  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' RETURN
  env="${tmp}/public-sync.env"

  (
    cd "${tmp}"
    bash "${REPO_ROOT}/scripts/resolve-public-sync-refs.sh" \
      --source-ref main \
      > "${env}"
  )
  assert_contains "source_kind=main" "${env}" "main sync resolves source kind"
  assert_contains "target_branch=public-repo" "${env}" "main sync resolves public mirror"

  if (
    cd "${tmp}"
    bash "${REPO_ROOT}/scripts/resolve-public-sync-refs.sh" \
      --source-ref release/0.8
  ) >/dev/null 2>&1; then
    echo "FAIL: release refs should not sync through the main public mirror path" >&2
    exit 1
  fi
}

test_public_tree_equivalence() {
  local tmp public_origin public_work private_sha status

  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' RETURN
  new_git_repo "${tmp}"

  git branch -M main
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
    git branch -M main
    mkdir -p .github
    printf 'visible\n' > README.md
    printf 'public workflow\n' > .github/workflow.yml
    git add README.md .github/workflow.yml
    git commit -m "public tree" >/dev/null
    git push origin main >/dev/null 2>&1
  )

  "${REPO_ROOT}/scripts/check-public-tree-equivalence.sh" \
    --private-ref "${private_sha}" \
    --public-ref main \
    --public-repo "${public_origin}" \
    > "${tmp}/tree-match.out"

  printf 'private only\n' > docs/internal/extra.md
  git add docs/internal/extra.md
  git commit -m "private skipped change" >/dev/null
  "${REPO_ROOT}/scripts/check-public-tree-equivalence.sh" \
    --private-ref HEAD \
    --public-ref main \
    --public-repo "${public_origin}" \
    > "${tmp}/tree-match-skipped.out"

  (
    cd "${public_work}"
    printf 'changed\n' > README.md
    git add README.md
    git commit -m "public visible mismatch" >/dev/null
    git push origin main >/dev/null 2>&1
  )

  set +e
  "${REPO_ROOT}/scripts/check-public-tree-equivalence.sh" \
    --private-ref HEAD \
    --public-ref main \
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

test_namespaced_private_release_tagging() {
  local tmp main_sha minor_rc_tag release_branch_sha minor_rc_sha release_branch source_branch

  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' RETURN
  new_git_repo "${tmp}"

  create_commit base
  git branch -M main
  create_commit zero-eight-start
  main_sha="$(git rev-parse HEAD)"
  git checkout -b test-release/0.7 >/dev/null 2>&1
  create_commit zero-seven-one
  create_annotated_tag test/v0.7.1
  git push origin main test-release/0.7 --tags >/dev/null 2>&1

  "${REPO_ROOT}/scripts/create-release-tag.sh" \
    --source latest \
    --release-kind minor \
    --as-release-candidate \
    --tag-prefix test/v \
    --release-branch-prefix test-release/ \
    > "${tmp}/minor-rc.env"

  minor_rc_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/minor-rc.env")"
  release_branch="$(awk -F= '$1 == "release_branch" {print $2}' "${tmp}/minor-rc.env")"
  source_branch="$(awk -F= '$1 == "source_branch" {print $2}' "${tmp}/minor-rc.env")"
  release_branch_sha="$(git ls-remote --heads origin test-release/0.8 | awk '{print $1}')"
  minor_rc_sha="$(git rev-list -n1 "${minor_rc_tag}")"

  assert_eq "test/v0.8.0-rc1" "${minor_rc_tag}" "namespaced minor RC tag"
  assert_eq "test-release/0.8" "${release_branch}" "namespaced minor RC release branch"
  assert_eq "main" "${source_branch}" "namespaced minor RC uses main source branch"
  assert_eq "${main_sha}" "${minor_rc_sha}" "namespaced minor RC tags main source commit"
  assert_eq "${minor_rc_sha}" "${release_branch_sha}" "namespaced release branch points at RC commit"

  if "${REPO_ROOT}/scripts/create-release-tag.sh" \
    --source test-release/0.7 \
    --release-kind minor \
    --as-release-candidate \
    --tag-prefix test/v \
    --release-branch-prefix test-release/ \
    > "${tmp}/release-branch-minor.out" 2>&1; then
    echo "FAIL: minor releases should not start from an existing release branch" >&2
    exit 1
  fi

  cd "${REPO_ROOT}"
  rm -rf "${tmp}"
  trap - RETURN
}

test_namespaced_private_release_from_test_main() {
  local tmp major_tag channel source_branch created_branch

  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' RETURN
  new_git_repo "${tmp}"

  create_commit base
  git branch -M test-main
  git checkout -b test-release/0.7 >/dev/null 2>&1
  create_commit zero-seven-one
  create_annotated_tag test/v0.7.1
  git checkout test-main >/dev/null 2>&1
  create_commit zero-eight-start
  git push origin test-main test-release/0.7 --tags >/dev/null 2>&1

  "${REPO_ROOT}/scripts/create-release-tag.sh" \
    --source latest \
    --release-kind minor \
    --as-release-candidate \
    --tag-prefix test/v \
    --release-branch-prefix test-release/ \
    --main-branch test-main \
    > "${tmp}/minor-rc.env"

  major_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/minor-rc.env")"
  channel="$(awk -F= '$1 == "channel" {print $2}' "${tmp}/minor-rc.env")"
  source_branch="$(awk -F= '$1 == "source_branch" {print $2}' "${tmp}/minor-rc.env")"
  created_branch="$(awk -F= '$1 == "created_release_branch" {print $2}' "${tmp}/minor-rc.env")"

  assert_eq "test/v0.8.0-rc1" "${major_tag}" "test-main source uses configured tag prefix"
  assert_eq "latest" "${channel}" "test-main source is classified as latest"
  assert_eq "test-main" "${source_branch}" "test-main source branch is retained"
  assert_eq "true" "${created_branch}" "test-main minor creates namespaced release branch"

  cd "${REPO_ROOT}"
  rm -rf "${tmp}"
  trap - RETURN
}

test_namespaced_minor_from_explicit_main_commit_after_patch_rc() {
  local tmp main_sha minor_tag release_branch release_branch_sha source_branch

  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' RETURN
  new_git_repo "${tmp}"

  create_commit base
  git branch -M test-main
  create_commit zero-eight-start
  create_annotated_tag test/v0.8.0
  git checkout -b test-release/0.8 >/dev/null 2>&1
  create_commit zero-eight-one-rc
  create_annotated_tag test/v0.8.1-rc1
  git checkout test-main >/dev/null 2>&1
  create_commit zero-nine-start
  main_sha="$(git rev-parse HEAD)"
  git push origin test-main test-release/0.8 --tags >/dev/null 2>&1

  "${REPO_ROOT}/scripts/create-release-tag.sh" \
    --source "${main_sha}" \
    --release-kind minor \
    --as-release-candidate \
    --tag-prefix test/v \
    --release-branch-prefix test-release/ \
    --main-branch test-main \
    > "${tmp}/minor-rc.env"

  minor_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/minor-rc.env")"
  release_branch="$(awk -F= '$1 == "release_branch" {print $2}' "${tmp}/minor-rc.env")"
  source_branch="$(awk -F= '$1 == "source_branch" {print $2}' "${tmp}/minor-rc.env")"
  release_branch_sha="$(git ls-remote --heads origin test-release/0.9 | awk '{print $1}')"

  assert_eq "test/v0.9.0-rc1" "${minor_tag}" "explicit main commit starts next minor after active patch RC"
  assert_eq "test-release/0.9" "${release_branch}" "explicit main commit creates next minor release branch"
  assert_eq "test-main" "${source_branch}" "explicit main commit is associated with configured main branch"
  assert_eq "${main_sha}" "$(git rev-list -n1 "${minor_tag}")" "explicit main commit is tagged"
  assert_eq "${main_sha}" "${release_branch_sha}" "explicit main commit seeds next minor branch"

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
  git branch -M main
  create_commit zero-seven
  main_sha="$(git rev-parse HEAD)"
  create_annotated_tag v0.7.0
  git checkout -b release/0.7 >/dev/null 2>&1
  create_commit zero-seven-one
  create_annotated_tag v0.7.1
  git push origin main release/0.7 --tags >/dev/null 2>&1

  "${REPO_ROOT}/scripts/create-release-tag.sh" \
    --source latest \
    --release-kind minor \
    --as-release-candidate \
    > "${tmp}/minor-rc.env"

  minor_rc_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/minor-rc.env")"
  minor_rc_type="$(git cat-file -t "${minor_rc_tag}")"
  minor_rc_created_branch="$(awk -F= '$1 == "created_release_branch" {print $2}' "${tmp}/minor-rc.env")"
  release_branch_sha="$(git ls-remote --heads origin release/0.8 | awk '{print $1}')"
  minor_rc_sha="$(git rev-list -n1 "${minor_rc_tag}")"

  assert_eq "v0.8.0-rc1" "${minor_rc_tag}" "minor RC tag"
  assert_eq "tag" "${minor_rc_type}" "release tags are annotated"
  assert_eq "true" "${minor_rc_created_branch}" "minor RC creates private release branch"
  assert_eq "${main_sha}" "${minor_rc_sha}" "minor RC tags main source commit"
  assert_eq "${minor_rc_sha}" "${release_branch_sha}" "new private release branch points at RC commit"

  "${REPO_ROOT}/scripts/create-release-tag.sh" \
    --source latest \
    --release-kind minor \
    --as-release-candidate \
    > "${tmp}/minor-rc-rerun.env"

  minor_rc_rerun_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/minor-rc-rerun.env")"
  assert_eq "v0.8.0-rc1" "${minor_rc_rerun_tag}" "same-commit RC rerun reuses tag"

  git checkout release/0.8 >/dev/null 2>&1
  create_commit post-rc-change
  release_branch_head_after_rc="$(git rev-parse HEAD)"

  "${REPO_ROOT}/scripts/create-release-tag.sh" \
    --source latest \
    --release-kind minor \
    > "${tmp}/minor-final.env"
  minor_final_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/minor-final.env")"
  minor_final_sha="$(git rev-list -n1 "${minor_final_tag}")"
  assert_eq "v0.8.0" "${minor_final_tag}" "minor final promotes active RC"
  assert_eq "${minor_rc_sha}" "${minor_final_sha}" "minor final tags the promoted RC commit"
  if [[ "${minor_final_sha}" == "${release_branch_head_after_rc}" ]]; then
    echo "FAIL: minor final should not tag release branch HEAD after the RC has been tested" >&2
    exit 1
  fi

  "${REPO_ROOT}/scripts/create-release-tag.sh" \
    --source latest \
    --release-kind patch \
    --as-release-candidate \
    > "${tmp}/patch-after-final.env"
  patch_after_final_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/patch-after-final.env")"
  assert_eq "v0.8.1-rc1" "${patch_after_final_tag}" "finalized RC does not remain the latest release"

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
  git branch -M main
  git checkout -b release/0.7 >/dev/null 2>&1
  create_commit zero-seven-one
  create_annotated_tag v0.7.1
  git push origin main release/0.7 --tags >/dev/null 2>&1

  set +e
  PUSH_TAG=false "${REPO_ROOT}/scripts/create-release-tag.sh" \
    --source latest \
    --release-kind patch \
    > "${tmp}/no-diff-patch.out" 2>&1
  status="$?"
  set -e
  if [[ "${status}" -eq 0 ]]; then
    echo "FAIL: patch release without source diff should fail" >&2
    exit 1
  fi
  assert_contains "No changes since v0.7.1; refusing to create v0.7.1" \
    "${tmp}/no-diff-patch.out" \
    "patch release requires a diff from the latest final tag"

  create_commit zero-seven-two
  PUSH_TAG=false "${REPO_ROOT}/scripts/create-release-tag.sh" \
    --source latest \
    --release-kind patch \
    --as-release-candidate \
    > "${tmp}/patch-rc.env"
  patch_rc_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/patch-rc.env")"
  assert_eq "v0.7.2-rc1" "${patch_rc_tag}" "patch RC with a source diff is allowed"

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
  git branch -M main
  create_commit zero-eight-start
  git checkout -b release/0.7 >/dev/null 2>&1
  create_commit zero-seven-one
  create_annotated_tag v0.7.1
  git push origin main release/0.7 --tags >/dev/null 2>&1

  "${REPO_ROOT}/scripts/create-release-tag.sh" \
    --source main \
    --release-kind minor \
    > "${tmp}/minor-final.env"

  minor_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/minor-final.env")"
  assert_eq "v0.8.0" "${minor_tag}" "direct minor final honors release kind"

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
  git branch -M main
  git checkout -b release/0.8 >/dev/null 2>&1
  create_annotated_tag v0.8.0-rc1
  create_annotated_tag v0.8.0
  git checkout main >/dev/null 2>&1
  create_commit one-zero-start
  git push origin main release/0.8 --tags >/dev/null 2>&1

  "${REPO_ROOT}/scripts/create-release-tag.sh" \
    --source latest \
    --release-kind major \
    --as-release-candidate \
    > "${tmp}/major-rc.env"

  major_tag="$(awk -F= '$1 == "tag" {print $2}' "${tmp}/major-rc.env")"
  major_branch="$(awk -F= '$1 == "release_branch" {print $2}' "${tmp}/major-rc.env")"
  created_branch="$(awk -F= '$1 == "created_release_branch" {print $2}' "${tmp}/major-rc.env")"

  assert_eq "v1.0.0-rc1" "${major_tag}" "major RC after finalized RC"
  assert_eq "release/1.0" "${major_branch}" "major RC release branch"
  assert_eq "true" "${created_branch}" "major RC creates private release branch"

  cd "${REPO_ROOT}"
  rm -rf "${tmp}"
  trap - RETURN
}

test_public_tag_rerun_and_target_ref() {
  local tmp first_sha second_sha existing_sha existing_created target_sha new_sha mismatch_status

  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' RETURN
  new_git_repo "${tmp}"

  create_commit base
  git branch -M main
  git checkout -b release/0.8 >/dev/null 2>&1
  create_commit first-public-release
  first_sha="$(git rev-parse HEAD)"
  create_annotated_tag v0.8.0
  create_commit second-public-release
  second_sha="$(git rev-parse HEAD)"
  git push origin main release/0.8 --tags >/dev/null 2>&1
  git checkout main >/dev/null 2>&1

  PUSH_TAG=false "${REPO_ROOT}/scripts/create-public-release-tag.sh" \
    --tag v0.8.0 \
    --release-branch release/0.8 \
    > "${tmp}/existing-public-tag.env"

  existing_sha="$(awk -F= '$1 == "sha" {print $2}' "${tmp}/existing-public-tag.env")"
  existing_created="$(awk -F= '$1 == "created_new_tag" {print $2}' "${tmp}/existing-public-tag.env")"
  assert_eq "${first_sha}" "${existing_sha}" "manual public rerun reuses existing tag even after branch advances"
  assert_eq "false" "${existing_created}" "manual public rerun does not create a new tag"

  if PUSH_TAG=false "${REPO_ROOT}/scripts/create-public-release-tag.sh" \
    --tag v0.8.2 \
    --release-branch release/0.8 \
    > "${tmp}/missing-public-tag.out" 2>&1; then
    echo "FAIL: missing public tag without target-ref should fail" >&2
    exit 1
  fi

  PUSH_TAG=false "${REPO_ROOT}/scripts/create-public-release-tag.sh" \
    --tag v0.8.1 \
    --release-branch release/0.8 \
    --target-ref "${second_sha}" \
    > "${tmp}/target-public-tag.env"

  target_sha="$(awk -F= '$1 == "sha" {print $2}' "${tmp}/target-public-tag.env")"
  new_sha="$(git rev-list -n1 v0.8.1)"
  assert_eq "${second_sha}" "${target_sha}" "public tag target-ref output"
  assert_eq "${second_sha}" "${new_sha}" "public tag created at immutable target ref"

  set +e
  PUSH_TAG=false "${REPO_ROOT}/scripts/create-public-release-tag.sh" \
    --tag v0.8.1 \
    --release-branch release/0.8 \
    --target-ref "${first_sha}" \
    > "${tmp}/target-mismatch.out" 2>&1
  mismatch_status="$?"
  set -e
  if [[ "${mismatch_status}" -eq 0 ]]; then
    echo "FAIL: existing public tag at a different target-ref should fail" >&2
    exit 1
  fi

  cd "${REPO_ROOT}"
  rm -rf "${tmp}"
  trap - RETURN
}

test_namespaced_public_tag() {
  local tmp public_sha tag_sha version release_branch

  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' RETURN
  new_git_repo "${tmp}"

  create_commit base
  git branch -M main
  git checkout -b test-release/0.8 >/dev/null 2>&1
  create_commit public-release
  public_sha="$(git rev-parse HEAD)"
  git push origin main test-release/0.8 --tags >/dev/null 2>&1

  PUSH_TAG=false "${REPO_ROOT}/scripts/create-public-release-tag.sh" \
    --tag test/v0.8.0 \
    --release-branch test-release/0.8 \
    --target-ref "${public_sha}" \
    --tag-prefix test/v \
    --release-branch-prefix test-release/ \
    > "${tmp}/public-tag.env"

  tag_sha="$(git rev-list -n1 test/v0.8.0)"
  version="$(awk -F= '$1 == "version" {print $2}' "${tmp}/public-tag.env")"
  release_branch="$(awk -F= '$1 == "release_branch" {print $2}' "${tmp}/public-tag.env")"

  assert_eq "${public_sha}" "${tag_sha}" "namespaced public tag target"
  assert_eq "0.8.0" "${version}" "namespaced public tag version"
  assert_eq "test-release/0.8" "${release_branch}" "namespaced public tag release branch"

  cd "${REPO_ROOT}"
  rm -rf "${tmp}"
  trap - RETURN
}

test_publish_crates() {
  local tmp calls status

  tmp="$(mktemp -d)"
  trap 'rm -rf "${tmp}"' RETURN
  mkdir -p "${tmp}/bin"
  calls="${tmp}/cargo-calls"

  cat > "${tmp}/bin/cargo" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail

case "${1:-}" in
  metadata)
    printf '{"packages":[{"name":"arc-crate","version":"0.7.2"},{"name":"other-version","version":"0.7.1"}]}'
    ;;
  publish)
    echo "cargo $*" >> "${CARGO_CALLS}"
    if [[ "${CARGO_PUBLISH_FAIL:-}" == "already" ]]; then
      echo "crate version already uploaded" >&2
      exit 1
    fi
    ;;
  *)
    echo "unexpected cargo command: $*" >&2
    exit 1
    ;;
esac
STUB
  chmod +x "${tmp}/bin/cargo"

  cat > "${tmp}/bin/jq" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail

name=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --arg)
      if [[ "${2:-}" == "name" ]]; then
        name="${3:-}"
      fi
      shift 3
      ;;
    *) shift ;;
  esac
done
cat >/dev/null
case "${name}" in
  arc-crate) echo "0.7.2" ;;
  other-version) echo "0.7.1" ;;
  *) echo "null" ;;
esac
STUB
  chmod +x "${tmp}/bin/jq"

  PATH="${tmp}/bin:${PATH}" CARGO_CALLS="${calls}" "${REPO_ROOT}/scripts/publish-crates.sh" \
    --tag v0.7.2 \
    --packages arc-crate \
    --dry-run \
    > "${tmp}/dry-run.out"
  assert_contains "cargo publish --locked --dry-run -p arc-crate" "${calls}" "dry-run publishes selected package"

  set +e
  PATH="${tmp}/bin:${PATH}" CARGO_CALLS="${calls}" "${REPO_ROOT}/scripts/publish-crates.sh" \
    --tag v0.7.2-rc1 \
    --packages arc-crate \
    --dry-run \
    > "${tmp}/rc.out" 2>&1
  status="$?"
  set -e
  if [[ "${status}" -eq 0 ]]; then
    echo "FAIL: crates publishing should reject RC tags" >&2
    exit 1
  fi

  set +e
  PATH="${tmp}/bin:${PATH}" CARGO_CALLS="${calls}" "${REPO_ROOT}/scripts/publish-crates.sh" \
    --tag v0.7.2 \
    --packages other-version \
    --dry-run \
    > "${tmp}/version-mismatch.out" 2>&1
  status="$?"
  set -e
  if [[ "${status}" -eq 0 ]]; then
    echo "FAIL: crates publishing should reject package version mismatches" >&2
    exit 1
  fi

  set +e
  PATH="${tmp}/bin:${PATH}" CARGO_CALLS="${calls}" "${REPO_ROOT}/scripts/publish-crates.sh" \
    --tag v0.7.2 \
    --packages missing-crate \
    --dry-run \
    > "${tmp}/unknown-package.out" 2>&1
  status="$?"
  set -e
  if [[ "${status}" -eq 0 ]]; then
    echo "FAIL: crates publishing should reject unknown packages" >&2
    exit 1
  fi

  PATH="${tmp}/bin:${PATH}" CARGO_CALLS="${calls}" CARGO_REGISTRY_TOKEN=test-token CARGO_PUBLISH_FAIL=already \
    "${REPO_ROOT}/scripts/publish-crates.sh" \
    --tag v0.7.2 \
    --packages arc-crate \
    > "${tmp}/already-uploaded.out"
  assert_contains "already exists on crates.io; treating as complete" \
    "${tmp}/already-uploaded.out" \
    "already-uploaded crates are idempotent"

  cd "${REPO_ROOT}"
  rm -rf "${tmp}"
  trap - RETURN
}

test_copybara_body_escaping
test_namespaced_release_refs_and_copybara_config
test_resolve_public_sync_refs
test_public_tree_equivalence
test_namespaced_private_release_tagging
test_namespaced_private_release_from_test_main
test_namespaced_minor_from_explicit_main_commit_after_patch_rc
test_minor_bootstrap_and_finalized_rc_ordering
test_patch_release_requires_diff_from_latest_final
test_direct_minor_honors_release_kind
test_major_after_finalized_rc
test_public_tag_rerun_and_target_ref
test_namespaced_public_tag
test_publish_crates

echo "release script tests passed"
