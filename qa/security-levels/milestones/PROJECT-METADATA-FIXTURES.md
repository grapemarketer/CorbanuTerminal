# Fixture `.codex` / `.agents` folders (D322, #1542)

Some runs here collected their fixture workspace, including the project's
`.codex` and `.agents` folders. A checkout of this repository then holds
about twenty nested `.codex`/`.agents` folders, and the non-admin Windows
sandbox protects and watches every one of them on every command (D283), which
made commands in this repository roughly twice as slow.

They are stored as `dot-codex` and `dot-agents` instead (contents unchanged).
To look at a fixture workspace as it was, regenerate it in a folder of your
choice:

    python3 .github/scripts/project_metadata_fixtures.py restore \
        qa/security-levels/milestones/<milestone>/runs/<run>/collected/fixture/workspace \
        /tmp/<run>-workspace

`python3 .github/scripts/project_metadata_fixtures.py check` (run by
repo-checks through `test_project_metadata_fixtures.py`) fails if a nested
`.codex`/`.agents` is committed again.
