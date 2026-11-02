#!/usr/bin/env python3
"""Verify that an Arc source ref uses a published public Malachite release."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from typing import Any

PUBLIC_REPOSITORY = "https://github.com/circlefin/malachite.git"
GITHUB_REPOSITORY = "circlefin/malachite"
MALACHITE_PACKAGE_PREFIX = "arc-malachitebft-"
FINAL_TAG = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+")
COMMIT_SHA = re.compile(r"[0-9a-f]{40,64}")


class CheckError(RuntimeError):
    """A final release does not satisfy the public Malachite requirement."""


@dataclass(frozen=True)
class Resolution:
    tag: str
    commit: str
    packages: tuple[str, ...]


def git_show(ref: str, path: str) -> str:
    result = subprocess.run(
        ["git", "show", f"{ref}:{path}"],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise CheckError(f"{path} does not exist at Arc source ref {ref}")
    return result.stdout


def is_malachite_dependency(name: str, spec: Any) -> bool:
    if name.startswith("malachitebft-"):
        return True
    if not isinstance(spec, dict):
        return False
    package = spec.get("package", name)
    return str(package).startswith(MALACHITE_PACKAGE_PREFIX)


def parse_lock_source(source: str, tag: str) -> str:
    if not source.startswith("git+"):
        raise CheckError(f"Malachite lockfile source is not a Git dependency: {source}")

    parsed = urllib.parse.urlsplit(source.removeprefix("git+"))
    repository = urllib.parse.urlunsplit(
        (parsed.scheme, parsed.netloc, parsed.path, "", "")
    )
    if repository != PUBLIC_REPOSITORY:
        raise CheckError(
            "Malachite lockfile source must use "
            f"{PUBLIC_REPOSITORY}, found {repository}"
        )

    query = urllib.parse.parse_qs(parsed.query, keep_blank_values=True)
    if query != {"tag": [tag]}:
        raise CheckError(
            f"Malachite lockfile source must resolve tag {tag}, found {parsed.query}"
        )
    if not COMMIT_SHA.fullmatch(parsed.fragment):
        raise CheckError(f"Malachite lockfile source has invalid commit {parsed.fragment}")
    return parsed.fragment


def validate_dependency_resolution(manifest_text: str, lock_text: str) -> Resolution:
    try:
        manifest = tomllib.loads(manifest_text)
        lockfile = tomllib.loads(lock_text)
    except tomllib.TOMLDecodeError as error:
        raise CheckError(f"Unable to parse Arc dependency metadata: {error}") from error

    workspace_dependencies = manifest.get("workspace", {}).get("dependencies", {})
    dependencies = {
        name: spec
        for name, spec in workspace_dependencies.items()
        if is_malachite_dependency(name, spec)
    }
    if not dependencies:
        raise CheckError("Arc source declares no Malachite workspace dependencies")

    tags: set[str] = set()
    declared_packages: set[str] = set()
    for name, spec in dependencies.items():
        if not isinstance(spec, dict):
            raise CheckError(
                f"workspace dependency {name} must use a tagged public Git dependency"
            )
        repository = spec.get("git")
        if repository != PUBLIC_REPOSITORY:
            raise CheckError(
                f"workspace dependency {name} must use {PUBLIC_REPOSITORY}, "
                f"found {repository or 'no Git repository'}"
            )
        if "rev" in spec or "branch" in spec or "tag" not in spec:
            raise CheckError(
                f"workspace dependency {name} must use a tag, not a branch or revision"
            )
        tag = spec["tag"]
        if not isinstance(tag, str) or not FINAL_TAG.fullmatch(tag):
            raise CheckError(
                f"workspace dependency {name} must use a final vX.Y.Z tag, found {tag}"
            )
        tags.add(tag)
        declared_packages.add(str(spec.get("package", name)))

    if len(tags) != 1:
        raise CheckError(
            "all Malachite workspace dependencies must use one tag, found "
            + ", ".join(sorted(tags))
        )
    tag = tags.pop()

    locked_packages = [
        package
        for package in lockfile.get("package", [])
        if str(package.get("name", "")).startswith(MALACHITE_PACKAGE_PREFIX)
    ]
    if not locked_packages:
        raise CheckError("Cargo.lock contains no Malachite packages")

    locked_package_names = {str(package["name"]) for package in locked_packages}
    missing = declared_packages - locked_package_names
    if missing:
        raise CheckError(
            "Cargo.lock is missing declared Malachite packages: "
            + ", ".join(sorted(missing))
        )

    commits = {
        parse_lock_source(str(package.get("source", "")), tag)
        for package in locked_packages
    }
    if len(commits) != 1:
        raise CheckError(
            "Malachite lockfile packages resolve multiple commits: "
            + ", ".join(sorted(commits))
        )

    return Resolution(tag, commits.pop(), tuple(sorted(locked_package_names)))


def resolve_public_tag(tag: str) -> str:
    tag_ref = f"refs/tags/{tag}"
    peeled_ref = f"{tag_ref}^{{}}"
    result = subprocess.run(
        ["git", "ls-remote", "--tags", PUBLIC_REPOSITORY, tag_ref, peeled_ref],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise CheckError(f"unable to query public Malachite tag {tag}")

    refs: dict[str, str] = {}
    for line in result.stdout.splitlines():
        fields = line.split()
        if len(fields) == 2:
            refs[fields[1]] = fields[0]
    if tag_ref not in refs:
        raise CheckError(f"public Malachite tag does not exist: {tag}")

    commit = refs.get(peeled_ref, refs[tag_ref])
    if not COMMIT_SHA.fullmatch(commit):
        raise CheckError(f"public Malachite tag {tag} resolved invalid commit {commit}")
    return commit


def fetch_github_release(tag: str) -> dict[str, Any]:
    encoded_tag = urllib.parse.quote(tag, safe="")
    url = f"https://api.github.com/repos/{GITHUB_REPOSITORY}/releases/tags/{encoded_tag}"
    headers = {
        "Accept": "application/vnd.github+json",
        "User-Agent": "arc-release-preflight",
        "X-GitHub-Api-Version": "2022-11-28",
    }
    token = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    if token:
        headers["Authorization"] = f"Bearer {token}"

    try:
        with urllib.request.urlopen(
            urllib.request.Request(url, headers=headers), timeout=15
        ) as response:
            return json.loads(response.read())
    except urllib.error.HTTPError as error:
        if error.code == 404:
            raise CheckError(f"published Malachite GitHub Release not found: {tag}") from error
        raise CheckError(
            f"unable to query Malachite GitHub Release {tag}: HTTP {error.code}"
        ) from error
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as error:
        raise CheckError(f"unable to query Malachite GitHub Release {tag}") from error


def validate_github_release(release: dict[str, Any], tag: str) -> None:
    if release.get("tag_name") != tag:
        raise CheckError(
            f"Malachite GitHub Release tag mismatch: expected {tag}, "
            f"found {release.get('tag_name')}"
        )
    if release.get("draft") is not False:
        raise CheckError(f"Malachite GitHub Release is still a draft: {tag}")
    if release.get("prerelease") is not False:
        raise CheckError(f"Malachite GitHub Release is a prerelease: {tag}")
    if not release.get("published_at"):
        raise CheckError(f"Malachite GitHub Release is not published: {tag}")


def check_ref(ref: str) -> Resolution:
    resolution = validate_dependency_resolution(
        git_show(ref, "Cargo.toml"), git_show(ref, "Cargo.lock")
    )
    public_commit = resolve_public_tag(resolution.tag)
    if public_commit != resolution.commit:
        raise CheckError(
            f"public Malachite tag {resolution.tag} resolves {public_commit}, "
            f"but Cargo.lock resolves {resolution.commit}"
        )
    validate_github_release(fetch_github_release(resolution.tag), resolution.tag)
    return resolution


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ref", required=True, help="Arc Git ref to validate")
    args = parser.parse_args()

    try:
        resolution = check_ref(args.ref)
    except CheckError as error:
        print(f"::error::{error}", file=sys.stderr)
        return 1

    print(
        f"Validated public Malachite release {resolution.tag}@{resolution.commit} "
        f"for Arc source {args.ref}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
