#!/usr/bin/env python3
"""D322 (#1542): keep nested `.codex`/`.agents` folders out of the repository.

The non-admin Windows sandbox protects and watches every `.codex`/`.agents`
folder below the top of a writable root (D283), which costs time on every
command in a checkout that contains them. QA evidence used to keep copies of
fixture workspaces with such folders; they are stored as `dot-codex` and
`dot-agents` instead, and `restore` regenerates a fixture workspace with its
original names in a folder of your choice (a temporary one for tests).

    project_metadata_fixtures.py check
    project_metadata_fixtures.py restore <collected/fixture/workspace> <dest>
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path, PurePosixPath

METADATA_NAMES = (".codex", ".agents")
STORED_PREFIX = "dot-"


def nested_project_metadata(paths: list[str]) -> list[str]:
    """Tracked paths with a `.codex`/`.agents` component below the top."""
    found = []
    for path in paths:
        parts = PurePosixPath(path).parts
        # Any kind of entry counts; names match up to case (`.Codex`).
        if any(part.lower() in METADATA_NAMES for part in parts[1:]):
            found.append(path)
    return found


def tracked_paths(repo: Path) -> list[str]:
    output = subprocess.run(
        ["git", "-C", str(repo), "ls-files", "-z"],
        check=True,
        capture_output=True,
    ).stdout
    return [path for path in output.decode("utf-8").split("\0") if path]


def restore(workspace: Path, dest: Path) -> Path:
    """Copies the stored fixture `workspace` to `dest`, giving `dot-codex`
    and `dot-agents` (at any depth) their original names back."""
    shutil.copytree(workspace, dest, symlinks=True)
    stored = {STORED_PREFIX + name[1:]: name for name in METADATA_NAMES}
    # Deepest first, so renaming a folder never moves one still to rename.
    for path in sorted(dest.rglob("*"), key=lambda p: len(p.parts), reverse=True):
        if path.is_dir() and not path.is_symlink() and path.name in stored:
            path.rename(path.with_name(stored[path.name]))
    return dest


def main(argv: list[str]) -> int:
    if argv[:1] == ["check"] and len(argv) == 1:
        repo = Path(__file__).resolve().parents[2]
        found = nested_project_metadata(tracked_paths(repo))
        for path in found:
            print(f"nested project metadata: {path}", file=sys.stderr)
        return 1 if found else 0
    if argv[:1] == ["restore"] and len(argv) == 3:
        print(restore(Path(argv[1]), Path(argv[2])))
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
