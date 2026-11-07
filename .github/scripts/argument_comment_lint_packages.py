#!/usr/bin/env python3

"""List the codex-rs packages a PR's argument-comment lint should check.

PR CI lints only the workspace crates whose Rust sources changed, test code
included (the nightly Bazel lint still covers everything). A change to the
lint job itself adds a small canary crate so the job exercises its own setup.
Prints one package name per line.
"""

import argparse
import os
import subprocess
import tomllib
from pathlib import Path, PurePosixPath


ROOT = Path(__file__).resolve().parents[2]
WORKSPACE = "codex-rs"
CANARY_PACKAGE = "codex-utils-string"
# Editing any of these lints the canary so the PR proves the job still works.
LINT_JOB_PATHS = (
    ".github/actions/setup-ci/",
    ".github/actions/setup-rusty-v8/",
    ".github/scripts/argument_comment_lint_packages.py",
    ".github/workflows/rust-ci.yml",
    "tools/argument-comment-lint/",
)


def workspace_packages(root: Path) -> dict[str, str]:
    """Map each workspace member directory (relative to the repo) to its package."""
    manifest = tomllib.loads((root / WORKSPACE / "Cargo.toml").read_text())
    packages = {}
    for member in manifest["workspace"]["members"]:
        member_manifest = tomllib.loads(
            (root / WORKSPACE / member / "Cargo.toml").read_text()
        )
        packages[f"{WORKSPACE}/{member}"] = member_manifest["package"]["name"]
    return packages


def package_for(path: str, packages: dict[str, str]) -> str | None:
    """Return the package owning `path`, preferring the most deeply nested crate."""
    for parent in PurePosixPath(path).parents:
        package = packages.get(str(parent))
        if package is not None:
            return package
    return None


def lint_packages(files: list[str], packages: dict[str, str]) -> list[str]:
    selected = set()
    for path in files:
        if path.endswith(".rs"):
            package = package_for(path, packages)
            if package is not None:
                selected.add(package)
        if path.startswith(LINT_JOB_PATHS):
            selected.add(CANARY_PACKAGE)
    return sorted(selected)


def changed_files(base: str, head: str) -> list[str]:
    output = subprocess.run(
        # Three dots: only what the PR changed since it left `base`.
        [
            "git",
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            f"{base}...{head}",
        ],
        capture_output=True,
        check=True,
        cwd=ROOT,
    ).stdout
    # NUL records avoid Git's path quoting and preserve embedded newlines.
    return [os.fsdecode(path) for path in output.split(b"\0") if path]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base", help="base commit of the PR")
    parser.add_argument("head", help="head commit of the PR")
    args = parser.parse_args()
    for package in lint_packages(
        changed_files(args.base, args.head), workspace_packages(ROOT)
    ):
        print(package)


if __name__ == "__main__":
    main()
