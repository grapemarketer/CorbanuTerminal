#!/usr/bin/env python3

import os
import re
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]


class PrChangedPathsTest(unittest.TestCase):
    """Run the workflow's Git path reader and area classifier together."""

    FLAGS = (
        "argument_comment_lint",
        "argument_comment_lint_package",
        "app_server_schema",
        "codex",
        "windows_native_tests",
        "windows_native_core",
        "windows_native_sandbox",
        "windows_native_other",
        "workflows",
    )
    NAMES = (
        "ordinary",
        "with space",
        'with"quote',
        "with\\backslash",
        "with\ttab",
        "with\nnewline",
        "with\rcarriage-return",
        "café",
    )

    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.repo = Path(self.directory.name)
        self.env = {
            **os.environ,
            "BASH_ENV": os.devnull,
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_AUTHOR_NAME": "CI fixture",
            "GIT_AUTHOR_EMAIL": "ci@example.invalid",
            "GIT_COMMITTER_NAME": "CI fixture",
            "GIT_COMMITTER_EMAIL": "ci@example.invalid",
        }
        self.git("init", "-q")
        self.blob = self.git("hash-object", "-w", "--stdin", data=b"fixture\n").strip()
        self.base = self.commit([])

    def git(self, *args: str, data: bytes | None = None) -> bytes:
        return subprocess.run(
            ["git", *args],
            input=data,
            cwd=self.repo,
            env=self.env,
            capture_output=True,
            check=True,
        ).stdout

    def commit(self, paths: list[str]) -> str:
        # The real Git index can represent these names on every test host;
        # no filename containing control characters needs to exist on disk.
        self.git("read-tree", "--empty")
        entries = b"".join(
            b"100644 " + self.blob + b"\t" + os.fsencode(path) + b"\0" for path in paths
        )
        self.git("update-index", "-z", "--index-info", data=entries)
        tree = self.git("write-tree").decode().strip()
        return self.git("commit-tree", tree, "-m", "CI fixture").decode().strip()

    def detect(
        self, paths: list[str], *, base: str | None = None, event: str = "pull_request"
    ) -> dict[str, bool]:
        workflow = (REPO_ROOT / ".github/workflows/rust-ci.yml").read_text()
        # Include the actual diff reader as well as the loop: supplying a
        # pre-decoded Bash array would miss Git's quoting/record-boundary bug.
        match = re.search(
            r"(?ms)^          set -euo pipefail\n.*?^          done$", workflow
        )
        self.assertIsNotNone(match)
        program = textwrap.dedent(match.group(0))
        program = program.replace("${{ github.event_name }}", event)
        program = program.replace(
            "${{ github.event.pull_request.base.sha }}", base or self.base
        )
        program = program.replace(
            "${{ github.event.pull_request.head.sha }}", self.commit(paths)
        )
        program += "\n" + "\n".join(
            f'printf "{flag}=%s\\n" "${{{flag}}}"' for flag in self.FLAGS
        )
        result = subprocess.run(
            ["bash", "-c", program],
            cwd=self.repo,
            env=self.env,
            capture_output=True,
            text=True,
            check=True,
        )
        return {
            key: value == "true"
            for line in result.stdout.splitlines()
            if "=" in line
            for key, value in [line.split("=", 1)]
        }

    def expected(self, *enabled: str) -> dict[str, bool]:
        return {flag: flag in enabled for flag in self.FLAGS}

    def test_git_quoted_paths_keep_the_same_area_flags(self) -> None:
        areas = {
            "codex-rs/core/src": (
                "codex",
                "argument_comment_lint",
                "windows_native_tests",
                "windows_native_core",
            ),
            "codex-rs/windows-sandbox-rs/src": (
                "codex",
                "argument_comment_lint",
                "windows_native_tests",
                "windows_native_sandbox",
            ),
            "codex-rs/network-proxy/tests": (
                "codex",
                "argument_comment_lint",
                "windows_native_tests",
                "windows_native_sandbox",
            ),
            "codex-rs/process-hardening/src": (
                "codex",
                "argument_comment_lint",
                "windows_native_tests",
                "windows_native_sandbox",
            ),
            "codex-rs/sandboxing/src": (
                "codex",
                "argument_comment_lint",
                "windows_native_tests",
                "windows_native_sandbox",
            ),
            "codex-rs/exec-server/tests": (
                "codex",
                "argument_comment_lint",
                "windows_native_tests",
                "windows_native_other",
            ),
            "codex-rs/protocol/src": (
                "codex",
                "argument_comment_lint",
                "app_server_schema",
            ),
            "tools/argument-comment-lint": (
                "argument_comment_lint",
                "argument_comment_lint_package",
            ),
            ".github/scripts": ("workflows",),
            "docs": (),
        }
        for area, enabled in areas.items():
            for name in self.NAMES:
                with self.subTest(area=area, name=name):
                    self.assertEqual(
                        self.detect([f"{area}/{name}.rs"]), self.expected(*enabled)
                    )

    def test_newline_in_unrelated_path_does_not_create_another_area(self) -> None:
        self.assertEqual(
            self.detect(["docs/note\ncodex-rs/core/src/lib.rs"]), self.expected()
        )

    def test_git_diff_failure_does_not_report_a_clean_diff(self) -> None:
        with self.assertRaises(subprocess.CalledProcessError):
            self.detect([], base="0" * 40)

    def test_empty_diff_has_no_area_flags(self) -> None:
        self.assertEqual(self.detect([]), self.expected())

    def test_deleted_and_renamed_paths_keep_the_old_area(self) -> None:
        base = self.commit(['codex-rs/core/src/old"name.rs'])
        expected = self.expected(
            "codex",
            "argument_comment_lint",
            "windows_native_tests",
            "windows_native_core",
        )
        self.assertEqual(self.detect([], base=base), expected)
        self.assertEqual(self.detect(["docs/renamed.rs"], base=base), expected)

    def test_manual_runs_keep_the_forced_bundle(self) -> None:
        self.assertEqual(
            self.detect([], event="workflow_dispatch"),
            self.expected(
                "codex",
                "argument_comment_lint",
                "argument_comment_lint_package",
                "workflows",
            ),
        )

    def test_windows_native_workflow_change_runs_every_group(self) -> None:
        self.assertEqual(
            self.detect([".github/workflows/rust-ci-windows-native-tests.yml"]),
            self.expected(
                "workflows",
                "windows_native_tests",
                "windows_native_core",
                "windows_native_sandbox",
                "windows_native_other",
            ),
        )

    def targets(self, core: bool, sandbox: bool, other: bool) -> str:
        workflow = (REPO_ROOT / ".github/workflows/rust-ci.yml").read_text()
        match = re.search(
            r'(?ms)^          windows_native_targets=""\n.*?^          windows_native_targets="\$\{windows_native_targets# \}"$',
            workflow,
        )
        self.assertIsNotNone(match)
        program = "set -euo pipefail\n"
        program += f"windows_native_core={str(core).lower()}\n"
        program += f"windows_native_sandbox={str(sandbox).lower()}\n"
        program += f"windows_native_other={str(other).lower()}\n"
        program += textwrap.dedent(match.group(0))
        program += '\nprintf "%s" "$windows_native_targets"'
        return subprocess.run(
            ["bash", "-c", program],
            env=self.env,
            capture_output=True,
            text=True,
            check=True,
        ).stdout

    def test_windows_native_targets_follow_the_changed_groups(self) -> None:
        core = "//codex-rs/tui/... //codex-rs/core/... //codex-rs/state/... //codex-rs/login/..."
        sandbox = (
            "//codex-rs/windows-sandbox-rs/... //codex-rs/network-proxy/... "
            "//codex-rs/process-hardening/... //codex-rs/sandboxing/..."
        )
        other = "//codex-rs/exec-server/..."
        self.assertEqual(self.targets(False, False, False), "")
        self.assertEqual(self.targets(True, False, False), core)
        self.assertEqual(self.targets(False, True, False), sandbox)
        self.assertEqual(self.targets(False, False, True), other)
        self.assertEqual(self.targets(True, True, True), f"{core} {sandbox} {other}")


if __name__ == "__main__":
    unittest.main()
