#!/usr/bin/env python3
"""Every test that takes the Windows sandbox's legacy process lock must be in
the `windows_sandbox_legacy_sessions` nextest group (#1790).

The lock (`legacy_process_test_guard()`) serializes these tests only within
one process. Nextest runs each test in its own process, so it serializes
them only through the test group, which selects tests by name.
"""

import re
import tomllib
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
CODEX_RS = REPO_ROOT / "codex-rs"
NEXTEST_TOML = CODEX_RS / ".config" / "nextest.toml"
CRATE_DIR = CODEX_RS / "windows-sandbox-rs"
PACKAGE = "codex-windows-sandbox"
GROUP = "windows_sandbox_legacy_sessions"
GUARD = "legacy_process_test_guard"

FN_RE = re.compile(r"\bfn\s+([A-Za-z_]\w*)")
TEST_ATTR_RE = re.compile(r"^\s*#\[(?:\w+::)?test\b")
# The only filter shape the check understands, so it cannot misjudge one:
# `package(codex-windows-sandbox) & (test(a) | test(b) | ...)`.
FILTER_RE = re.compile(
    rf"^package\({re.escape(PACKAGE)}\)\s*&\s*\((?P<terms>[^()]*(?:\([^()]*\)[^()]*)*)\)$"
)
TERM_RE = re.compile(r"^test\(([A-Za-z0-9_:]+)\)$")


def enclosing_fn(lines: list[str], index: int) -> tuple[str, int] | None:
    for i in range(index, -1, -1):
        match = FN_RE.search(lines[i])
        if match:
            return match.group(1), i
    return None


def is_test_fn(lines: list[str], fn_line: int) -> bool:
    i = fn_line - 1
    while i >= 0:
        stripped = lines[i].strip()
        if TEST_ATTR_RE.match(lines[i]):
            return True
        if stripped.startswith(("#[", "//")) or stripped == "" or stripped.endswith(")]"):
            i -= 1
            continue
        return False
    return False


def guard_users(sources: dict[str, str]) -> dict[str, str]:
    """Maps each test that takes the guard, directly or through helpers, to
    its file. Helpers are followed by name."""
    files = {path: text.splitlines() for path, text in sources.items()}
    tests: dict[str, str] = {}
    seen: set[str] = set()
    pending = [GUARD]
    while pending:
        callee = pending.pop()
        if callee in seen:
            continue
        seen.add(callee)
        call = re.compile(rf"\b{re.escape(callee)}\s*\(")
        definition = re.compile(rf"\bfn\s+{re.escape(callee)}\b")
        for path, lines in files.items():
            for index, line in enumerate(lines):
                if not call.search(line) or definition.search(line):
                    continue
                found = enclosing_fn(lines, index)
                if found is None:
                    continue
                name, fn_line = found
                if is_test_fn(lines, fn_line):
                    tests[name] = path
                else:
                    pending.append(name)
    return tests


def crate_sources() -> dict[str, str]:
    return {
        str(path.relative_to(REPO_ROOT)): path.read_text(encoding="utf-8")
        for path in sorted(CRATE_DIR.rglob("*.rs"))
    }


def other_groups_claiming_the_crate(config: dict) -> list[str]:
    """Overrides that could put the crate's tests in another group first.

    Nextest uses the first matching override that sets a test group, and
    evaluates a profile's own overrides before the default profile's.
    """
    claims = []
    for profile, settings in config.get("profile", {}).items():
        overrides = settings.get("overrides", [])
        if profile == "default":
            # Later default overrides never win over the group's own one.
            ours = [
                i
                for i, override in enumerate(overrides)
                if override.get("test-group") == GROUP
                and f"package({PACKAGE})" in override.get("filter", "")
            ]
            overrides = overrides[: ours[0]] if ours else overrides
        for override in overrides:
            group = override.get("test-group")
            filter_ = override.get("filter", "")
            if group is None or group == GROUP:
                continue
            scoped = re.match(r"^\s*package\(([\w-]+)\)\s*&", filter_)
            if scoped is None or scoped.group(1) == PACKAGE:
                claims.append(f"profile.{profile}: {group}: {filter_}")
    return claims


def group_terms(config: dict) -> list[str]:
    """The name substrings that put a test of the crate into the group."""
    filters = [
        override["filter"]
        for override in config.get("profile", {}).get("default", {}).get("overrides", [])
        if override.get("test-group") == GROUP
        and "platform" not in override
        and f"package({PACKAGE})" in override.get("filter", "")
    ]
    if len(filters) != 1:
        raise ValueError(f"expected one {PACKAGE} override for {GROUP}, found {filters!r}")
    match = FILTER_RE.match(filters[0].strip())
    if match is None:
        raise ValueError(f"unsupported {GROUP} filter shape: {filters[0]!r}")
    terms = []
    for term in match.group("terms").split("|"):
        term_match = TERM_RE.match(term.strip())
        if term_match is None:
            raise ValueError(f"unsupported {GROUP} filter term: {term!r}")
        terms.append(term_match.group(1))
    return terms


def missing_from_group(tests: dict[str, str], terms: list[str]) -> list[str]:
    # nextest's `test(x)` matches names that contain `x`; a test's full name
    # contains its function name.
    return sorted(
        f"{path}: {name}"
        for name, path in tests.items()
        if not any(term in name for term in terms)
    )


class WindowsSandboxNextestGroupTest(unittest.TestCase):
    maxDiff = None

    def test_every_guard_user_is_in_the_legacy_sessions_group(self) -> None:
        tests = guard_users(crate_sources())
        config = tomllib.loads(NEXTEST_TOML.read_text(encoding="utf-8"))
        self.assertEqual(other_groups_claiming_the_crate(config), [])
        terms = group_terms(config)
        self.assertEqual(
            missing_from_group(tests, terms),
            [],
            f"tests that take {GUARD}() are missing from the {GROUP} filter in "
            f"{NEXTEST_TOML.relative_to(REPO_ROOT)}",
        )

    def test_scan_finds_direct_helper_and_nested_helper_users(self) -> None:
        tests = guard_users(crate_sources())
        # Direct, outside unified_exec, through run_legacy_delete_fixture, and
        # through com_launch -> run_powershell_launch.
        for name in (
            "d311_cloud_placeholder_folders_match_the_tree_walk",
            "sec_win_1648_racing_command_cannot_open_a_starting_runner",
            "legacy_read_only_after_workspace_write_cannot_delete",
            "legacy_launch_through_com_shell_windows_stays_in_session",
        ):
            with self.subTest(name=name):
                self.assertIn(name, tests)
        self.assertNotIn("run_powershell_launch", tests)

    def test_check_reports_a_guard_user_outside_the_group(self) -> None:
        source = """
#[test]
fn legacy_one() { let _guard = legacy_process_test_guard(); }

fn helper() { let _guard = legacy_process_test_guard(); }

#[tokio::test]
// comment
#[ignore = "reason"]
async fn sec_win_1_elevated_two() { helper(); }

#[test]
fn unrelated() {}
"""
        tests = guard_users({"x.rs": source})
        self.assertEqual(sorted(tests), ["legacy_one", "sec_win_1_elevated_two"])
        self.assertEqual(missing_from_group(tests, ["legacy_"]), ["x.rs: sec_win_1_elevated_two"])
        self.assertEqual(missing_from_group(tests, ["legacy_", "elevated_"]), [])

    def test_check_reports_another_group_that_could_claim_the_crate(self) -> None:
        config = {
            "profile": {
                "default": {
                    "overrides": [
                        {"filter": "package(codex-core) & test(x)", "test-group": "a"},
                        {"filter": "test(elevated_)", "test-group": "b"},
                        {"filter": f"package({PACKAGE}) & test(y)", "test-group": "c"},
                        {"filter": f"package({PACKAGE}) & (test(z))", "test-group": GROUP},
                        {"filter": "test(later)", "test-group": "d"},
                    ]
                },
                "local": {"overrides": [{"filter": "test(local)", "test-group": "e"}]},
            }
        }
        self.assertEqual(
            other_groups_claiming_the_crate(config),
            [
                "profile.default: b: test(elevated_)",
                f"profile.default: c: package({PACKAGE}) & test(y)",
                "profile.local: e: test(local)",
            ],
        )

    def test_unsupported_filter_shapes_are_rejected(self) -> None:
        for filter_ in (
            f"package({PACKAGE}) & test(legacy_)",
            f"package({PACKAGE}) & (test(legacy_) | test(/elevated_/))",
            f"package({PACKAGE}) & (test(legacy_) & !test(slow))",
        ):
            config = {
                "profile": {
                    "default": {"overrides": [{"filter": filter_, "test-group": GROUP}]}
                }
            }
            with self.subTest(filter=filter_), self.assertRaises(ValueError):
                group_terms(config)


if __name__ == "__main__":
    unittest.main()
