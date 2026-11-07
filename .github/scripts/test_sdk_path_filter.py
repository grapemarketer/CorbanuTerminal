#!/usr/bin/env python3
"""D287/D314: run sdk.yml's own "Detect changed paths" script against real
git diffs and check which pull requests run the TypeScript SDK job."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path


WORKFLOW = Path(__file__).resolve().parents[1] / "workflows" / "sdk.yml"


def detect_script() -> str:
    """The `run: |` body of the "Detect changed paths" step, dedented."""
    lines = WORKFLOW.read_text(encoding="utf-8").splitlines()
    start = lines.index("      - name: Detect changed paths")
    run = next(i for i in range(start, len(lines)) if lines[i].strip() == "run: |")
    body = []
    for line in lines[run + 1 :]:
        if line.strip() and not line.startswith("          "):
            break
        body.append(line[10:])
    return "\n".join(body) + "\n"


def run_for(changed: list[str]) -> str:
    """The job's `run` output for a pull request changing `changed`."""
    with tempfile.TemporaryDirectory() as repo:
        env = {
            **os.environ,
            "GIT_AUTHOR_NAME": "t",
            "GIT_AUTHOR_EMAIL": "t@example.com",
            "GIT_COMMITTER_NAME": "t",
            "GIT_COMMITTER_EMAIL": "t@example.com",
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_CONFIG_NOSYSTEM": "1",
        }

        def git(*args: str) -> str:
            return subprocess.run(
                ["git", *args], cwd=repo, env=env, check=True, capture_output=True, text=True
            ).stdout.strip()

        git("init", "-q")
        git("commit", "-q", "--allow-empty", "-m", "base")
        base = git("rev-parse", "HEAD")
        for path in changed:
            file = Path(repo, path)
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_text("changed\n", encoding="utf-8")
        git("add", "-A")
        git("commit", "-q", "-m", "head")
        head = git("rev-parse", "HEAD")
        output = Path(repo, ".gh-output")
        subprocess.run(
            ["bash", "-c", detect_script()],
            cwd=repo,
            env={**env, "BASE_SHA": base, "HEAD_SHA": head, "GITHUB_OUTPUT": str(output)},
            check=True,
            capture_output=True,
        )
        return output.read_text(encoding="utf-8").strip()


# (label, changed files, whether the TypeScript SDK job runs)
CASES = [
    # D314: lockfile-only pull requests.
    ("pnpm-lock.yaml only", ["pnpm-lock.yaml"], True),
    ("Cargo.lock only", ["codex-rs/Cargo.lock"], True),
    ("both lockfiles", ["pnpm-lock.yaml", "codex-rs/Cargo.lock"], True),
    # D287: existing triggers.
    ("TypeScript SDK", ["sdk/typescript/src/index.ts"], True),
    ("codex exec", ["codex-rs/exec/src/lib.rs"], True),
    ("this workflow", [".github/workflows/sdk.yml"], True),
    ("SDK with unrelated", ["sdk/typescript/package.json", "README.md"], True),
    ("lockfile with unrelated", ["pnpm-lock.yaml", "docs/x.md"], True),
    # Unrelated-only pull requests still skip.
    ("unrelated docs", ["README.md", "docs/x.md"], False),
    ("unrelated Rust", ["codex-rs/core/src/lib.rs", "codex-rs/core/Cargo.toml"], False),
    ("Python SDK", ["sdk/python/pyproject.toml"], False),
    ("other workflow", [".github/workflows/rust-ci.yml"], False),
    # Other lockfiles are not the SDK's.
    ("devcontainer pnpm lock", [".devcontainer/codex-install/pnpm-lock.yaml"], False),
    ("lint tool Cargo.lock", ["tools/argument-comment-lint/Cargo.lock"], False),
    ("nested pnpm lock name", ["sdk/python/pnpm-lock.yaml.bak"], False),
]


class SdkPathFilterTest(unittest.TestCase):
    def test_case_matrix(self) -> None:
        for label, changed, runs in CASES:
            with self.subTest(label):
                self.assertEqual(run_for(changed), f"run={'true' if runs else 'false'}", changed)


if __name__ == "__main__":
    unittest.main(verbosity=2)
