"""Tests for check_public_malachite_release.py."""

from __future__ import annotations

import subprocess
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_public_malachite_release as checker  # noqa: E402

COMMIT = "a" * 40
OTHER_COMMIT = "b" * 40
TAG = "v0.8.0"


def manifest(
    repository: str = checker.PUBLIC_REPOSITORY,
    ref_type: str = "tag",
    ref_value: str = TAG,
    second_tag: str | None = None,
) -> str:
    return f"""
[workspace]

[workspace.dependencies.malachitebft-app]
package = "arc-malachitebft-app"
git = "{repository}"
{ref_type} = "{ref_value}"

[workspace.dependencies.malachitebft-sync]
package = "arc-malachitebft-sync"
git = "{repository}"
tag = "{second_tag or ref_value}"
"""


def lockfile(
    repository: str = checker.PUBLIC_REPOSITORY,
    tag: str = TAG,
    commit: str = COMMIT,
) -> str:
    source = f"git+{repository}?tag={tag}#{commit}"
    return f"""
version = 4

[[package]]
name = "arc-malachitebft-app"
version = "0.8.0"
source = "{source}"

[[package]]
name = "arc-malachitebft-sync"
version = "0.8.0"
source = "{source}"

[[package]]
name = "arc-malachitebft-wal"
version = "0.8.0"
source = "{source}"
"""


class DependencyResolutionTests(unittest.TestCase):
    def test_valid_dependency_resolution(self) -> None:
        resolution = checker.validate_dependency_resolution(manifest(), lockfile())

        self.assertEqual(resolution.tag, TAG)
        self.assertEqual(resolution.commit, COMMIT)
        self.assertEqual(
            resolution.packages,
            (
                "arc-malachitebft-app",
                "arc-malachitebft-sync",
                "arc-malachitebft-wal",
            ),
        )

    def test_private_repository_is_rejected(self) -> None:
        private_repository = "https://github.com/example/private-malachite.git"

        with self.assertRaisesRegex(
            checker.CheckError, "must use.*circlefin/malachite"
        ):
            checker.validate_dependency_resolution(
                manifest(repository=private_repository),
                lockfile(repository=private_repository),
            )

    def test_revision_is_rejected(self) -> None:
        with self.assertRaisesRegex(checker.CheckError, "must use a tag"):
            checker.validate_dependency_resolution(
                manifest(ref_type="rev", ref_value=COMMIT), lockfile()
            )

    def test_registry_dependency_is_rejected(self) -> None:
        registry_manifest = """
[workspace]

[workspace.dependencies]
malachitebft-app = "0.8.0"
"""
        with self.assertRaisesRegex(checker.CheckError, "tagged public Git dependency"):
            checker.validate_dependency_resolution(registry_manifest, lockfile())

    def test_prerelease_tag_is_rejected(self) -> None:
        with self.assertRaisesRegex(checker.CheckError, "final vX.Y.Z tag"):
            checker.validate_dependency_resolution(
                manifest(ref_value="v0.8.0-rc1"), lockfile(tag="v0.8.0-rc1")
            )

    def test_mixed_tags_are_rejected(self) -> None:
        with self.assertRaisesRegex(checker.CheckError, "must use one tag"):
            checker.validate_dependency_resolution(
                manifest(second_tag="v0.8.1"), lockfile()
            )

    def test_non_public_lockfile_source_is_rejected(self) -> None:
        with self.assertRaisesRegex(checker.CheckError, "lockfile source must use"):
            checker.validate_dependency_resolution(
                manifest(),
                lockfile(repository="https://github.com/example/malachite.git"),
            )

    def test_empty_lockfile_is_rejected(self) -> None:
        with self.assertRaisesRegex(checker.CheckError, "contains no Malachite packages"):
            checker.validate_dependency_resolution(manifest(), "version = 4")

    def test_duplicate_package_with_non_public_source_is_rejected(self) -> None:
        duplicate = lockfile() + "[[package]]" + lockfile(
            repository="https://github.com/example/malachite.git"
        ).split("[[package]]", 1)[1]

        with self.assertRaisesRegex(checker.CheckError, "lockfile source must use"):
            checker.validate_dependency_resolution(manifest(), duplicate)


class RemoteReleaseTests(unittest.TestCase):
    def test_annotated_public_tag_resolves_to_peeled_commit(self) -> None:
        output = (
            f"{OTHER_COMMIT}\trefs/tags/{TAG}\n"
            f"{COMMIT}\trefs/tags/{TAG}^{{}}\n"
        )
        result = subprocess.CompletedProcess([], 0, output, "")

        with patch.object(checker.subprocess, "run", return_value=result):
            self.assertEqual(checker.resolve_public_tag(TAG), COMMIT)

    def test_missing_public_tag_is_rejected(self) -> None:
        result = subprocess.CompletedProcess([], 0, "", "")

        with patch.object(checker.subprocess, "run", return_value=result):
            with self.assertRaisesRegex(checker.CheckError, "does not exist"):
                checker.resolve_public_tag(TAG)

    def test_lock_commit_must_match_public_tag(self) -> None:
        with (
            patch.object(
                checker,
                "git_show",
                side_effect=[manifest(), lockfile()],
            ),
            patch.object(checker, "resolve_public_tag", return_value=OTHER_COMMIT),
            patch.object(checker, "fetch_github_release") as fetch_release,
        ):
            with self.assertRaisesRegex(checker.CheckError, "Cargo.lock resolves"):
                checker.check_ref("arc-final-source")
        fetch_release.assert_not_called()

    def test_unpublished_github_release_is_rejected(self) -> None:
        cases = [
            ({"draft": True}, "still a draft"),
            ({"prerelease": True}, "is a prerelease"),
            ({"published_at": None}, "is not published"),
        ]
        for overrides, message in cases:
            with self.subTest(overrides=overrides):
                release = {
                    "tag_name": TAG,
                    "draft": False,
                    "prerelease": False,
                    "published_at": "2026-08-27T00:00:00Z",
                    **overrides,
                }
                with self.assertRaisesRegex(checker.CheckError, message):
                    checker.validate_github_release(release, TAG)

    def test_valid_github_release(self) -> None:
        checker.validate_github_release(
            {
                "tag_name": TAG,
                "draft": False,
                "prerelease": False,
                "published_at": "2026-08-27T00:00:00Z",
            },
            TAG,
        )


if __name__ == "__main__":
    unittest.main()
