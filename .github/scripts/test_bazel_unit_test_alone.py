#!/usr/bin/env python3
"""Every test a BUILD.bazel lists in `unit_test_alone` must exist (#1881).

The `<crate>-unit-tests-alone` target runs those names with `--exact`, and
the unit-test target skips them. A renamed test would no longer run alone:
libtest passes a run that matches no test. The check finds the test function
by its name (the last path segment) in the crate's sources.
"""

import ast
import re
import unittest
from pathlib import Path


CODEX_RS = Path(__file__).resolve().parents[2] / "codex-rs"
LIST_RE = re.compile(r"^\s*unit_test_alone\s*=\s*(\[[^\]]*\])", re.MULTILINE)
TEST_FN_RE = r"#\[\s*(?:\w+::)*test\b[^\]]*\]\s*(?:#\[[^\]]*\]\s*)*(?:async\s+)?fn\s+{}\s*\("


def alone_lists() -> dict[Path, list[str]]:
    found = {}
    for build in sorted(CODEX_RS.rglob("BUILD.bazel")):
        match = LIST_RE.search(build.read_text(encoding="utf-8"))
        if match:
            names = ast.literal_eval(re.sub(r"#[^\n]*", "", match.group(1)))
            found[build] = names
    return found


class UnitTestAloneTest(unittest.TestCase):
    def test_lists_are_found(self) -> None:
        self.assertIn(CODEX_RS / "state" / "BUILD.bazel", alone_lists())

    def test_every_listed_test_exists_once(self) -> None:
        for build, names in alone_lists().items():
            sources = [
                path.read_text(encoding="utf-8")
                for path in (build.parent / "src").rglob("*.rs")
            ]
            for name in names:
                with self.subTest(build=str(build), name=name):
                    fn = name.rsplit("::", 1)[-1]
                    pattern = re.compile(TEST_FN_RE.format(re.escape(fn)))
                    hits = sum(len(pattern.findall(text)) for text in sources)
                    self.assertEqual(hits, 1, f"{name}: {hits} test functions named {fn}")


if __name__ == "__main__":
    unittest.main()
