"""Tests for project_metadata_fixtures.py (D322, #1542)."""

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import project_metadata_fixtures as fixtures  # noqa: E402

REPO = Path(__file__).resolve().parents[2]
G27 = (
    REPO
    / "qa/security-levels/milestones/aggressive-20261013-third/runs/G27/collected/fixture/workspace"
)


class ProjectMetadataFixturesTest(unittest.TestCase):
    def test_the_repository_tracks_no_nested_project_metadata(self):
        self.assertEqual(fixtures.nested_project_metadata(fixtures.tracked_paths(REPO)), [])

    def test_nested_entries_of_any_case_are_found_and_top_level_ones_are_not(self):
        paths = [
            ".codex/skills/x/SKILL.md",
            ".agents/skills/y",
            "qa/run/workspace/.codex/config.toml",
            "qa/run/workspace/.AGENTS/notes.md",
            "src/.codex",
            "a/.agent\u017f/x",
            "qa/run/workspace/dot-codex/config.toml",
            "docs/.codexrc",
            "logs/.codex-tui.log",
        ]
        self.assertEqual(
            fixtures.nested_project_metadata(paths),
            [
                "qa/run/workspace/.codex/config.toml",
                "qa/run/workspace/.AGENTS/notes.md",
                "src/.codex",
                "a/.agent\u017f/x",
            ],
        )

    def test_restore_generates_the_fixture_with_its_original_names(self):
        with tempfile.TemporaryDirectory() as temp:
            dest = fixtures.restore(G27, Path(temp) / "workspace")
            for name in (".codex", ".agents"):
                stored = G27 / ("dot-" + name[1:])
                self.assertTrue(stored.is_dir())
                for file in stored.rglob("*"):
                    if file.is_file():
                        restored = dest / name / file.relative_to(stored)
                        self.assertEqual(restored.read_bytes(), file.read_bytes())
            self.assertFalse((dest / "dot-codex").exists())
            self.assertFalse((dest / "dot-agents").exists())


if __name__ == "__main__":
    unittest.main()
