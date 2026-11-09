#!/usr/bin/env python3
"""Every test that takes the Windows sandbox's legacy process lock must be in
the `windows_sandbox_legacy_sessions` nextest group (#1790).

The lock (`legacy_process_test_guard()`) serializes these tests only within
one process. Nextest runs each test in its own process, so it serializes
them only through the test group, which selects tests by name.

codex-core's tests that run the elevated setup (they stage its helpers) must
be in the group too, through a Windows-only override (#1873).
"""

import functools
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
# Only checks whether the lock is held (#1827); it never holds it.
HELD_CHECK = "legacy_process_test_guard_is_held"
LOCK = "LEGACY_PROCESS_TEST_LOCK"
CORE_PACKAGE = "codex-core"
CORE_DIR = CODEX_RS / "core"
# A codex-core test runs the elevated setup only with its helpers staged.
CORE_ELEVATED_MARK = "stage_windows_sandbox_helpers"
WINDOWS = "cfg(windows)"

FN_RE = re.compile(
    r"^[ \t]*(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?"
    r"(?:extern\s+)?fn\s+([A-Za-z_]\w*)",
    re.MULTILINE,
)
IMPL_RE = re.compile(r"^[ \t]*(?:unsafe\s+)?impl\b", re.MULTILINE)
TEST_ATTR_RE = re.compile(r"#\[\s*(?:\w+::)*test\s*[\](]")
RAW_STRING_RE = re.compile(r'(?:b|c)?r(#*)"')
CHAR_RE = re.compile(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'\n])'")
BRACE_RE = re.compile(r"[{}]")
NEXT_SPECIAL_RE = re.compile(r"[/\"'rbc]")
USE_RE = re.compile(r"^[ \t]*(?:pub(?:\([^)]*\))?\s+)?use\s[^;]*;", re.MULTILINE | re.DOTALL)
# The only filter shape the check understands, so it cannot misjudge one:
# `package(<package>) & (test(a) | test(b) | ...)`.
FILTER_RE = r"^package\({}\)\s*&\s*\((?P<terms>[^()]*(?:\([^()]*\)[^()]*)*)\)$"
TERM_RE = re.compile(r"^test\(([A-Za-z0-9_:]+)\)$")


class ScanError(ValueError):
    """A guard use the scan cannot tie to a test; the check fails closed."""


def blank(text: str) -> str:
    return re.sub(r"[^\n]", " ", text)


def code_only(source: str) -> tuple[str, list[str]]:
    """Blanks comments, string and char literals and `use` items, keeping
    every other character (and so every line and column) in place. Also
    returns the `use` items."""
    out = []
    i = 0
    n = len(source)
    while i < n:
        if source.startswith("//", i):
            j = source.find("\n", i)
            j = n if j < 0 else j
        elif source.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if source.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif source.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
        elif raw := RAW_STRING_RE.match(source, i):
            close = '"' + raw.group(1)
            j = source.find(close, raw.end())
            j = n if j < 0 else j + len(close)
        elif (source[i] == '"' or source.startswith(('b"', 'c"'), i)) and not (
            i and (source[i - 1].isalnum() or source[i - 1] == "_")
        ):
            j = source.index('"', i) + 1
            while j < n and source[j] != '"':
                j += 2 if source[j] == "\\" else 1
            j += 1
        elif char := CHAR_RE.match(source, i):
            j = char.end()
        else:
            # Up to the next character that could start a comment or literal.
            j = NEXT_SPECIAL_RE.search(source, i + 1)
            j = n if j is None else j.start()
            out.append(source[i:j])
            i = j
            continue
        out.append(blank(source[i:j]))
        i = j
    code = "".join(out)
    uses = [match.group(0) for match in USE_RE.finditer(code)]
    return USE_RE.sub(lambda match: blank(match.group(0)), code), uses


def block_end(code: str, open_brace: int) -> int:
    depth = 0
    for brace in BRACE_RE.finditer(code, open_brace):
        depth += 1 if brace.group() == "{" else -1
        if depth == 0:
            return brace.start()
    return len(code)


def fn_spans(code: str) -> list[tuple[str, int, int, int]]:
    """(name, start of the `fn` line, end of its body, end of its signature)."""
    spans = []
    for match in FN_RE.finditer(code):
        body = re.compile(r"[{;]").search(code, match.end())
        if body is None or code[body.start()] == ";":
            continue
        spans.append((match.group(1), match.start(), block_end(code, body.start()), body.start()))
    return spans


def trait_impl_spans(code: str) -> list[tuple[int, int]]:
    """Bodies of `impl Trait for Type` blocks, whose methods (`drop`,
    `deref`, ...) can run without a call the scan could see."""
    spans = []
    for match in IMPL_RE.finditer(code):
        body = code.find("{", match.end())
        if body >= 0 and re.search(r"\bfor\b", code[match.end() : body]):
            spans.append((body, block_end(code, body)))
    return spans


def is_test_fn(code: str, fn_start: int) -> bool:
    """Whether the attributes just above the `fn` line include a test one."""
    before = code[:fn_start].rstrip()
    item_start = max(before.rfind("}"), before.rfind(";"), before.rfind("{")) + 1
    return TEST_ATTR_RE.search(code[item_start:fn_start]) is not None


def guard_users(sources: dict[str, str], root: str = GUARD) -> dict[str, str]:
    """Maps each test that takes the guard (or calls `root`), directly or
    through helpers or other tests, to its file. Callers are followed by name. Raises ScanError
    for a use it cannot follow: the lock used outside the guard, a mention
    that is not a call, a call outside a function, or a helper that nothing
    calls."""
    scanned = {path: code_only(text) for path, text in sources.items()}
    files = {path: code for path, (code, _) in scanned.items()}
    uses = [item for _, items in scanned.values() for item in items]
    spans = {path: fn_spans(code) for path, code in files.items()}
    trait_impls = {path: trait_impl_spans(code) for path, code in files.items()}
    for item in uses:
        if re.search(rf"\b{LOCK}\b", item):
            raise ScanError(f"{LOCK} is imported: {item!r}")
    for path, code in files.items():
        for match in re.finditer(rf"\b{LOCK}\b", code):
            inside = [span for span in spans[path] if span[3] < match.start() < span[2]]
            before = code[max(0, match.start() - 80) : match.start()]
            in_guard = inside and max(inside, key=lambda span: span[1])[0] in (GUARD, HELD_CHECK)
            if not re.search(r"\bstatic\s+$", before) and not in_guard:
                line = code.count("\n", 0, match.start()) + 1
                raise ScanError(f"{path}:{line}: {LOCK} is used outside {GUARD}")
    tests: dict[str, str] = {}
    seen: set[str] = set()
    pending = [root]
    while pending:
        callee = pending.pop()
        if callee in seen:
            continue
        seen.add(callee)
        mention = re.compile(rf"\b{re.escape(callee)}\b")
        alias = re.compile(rf"\b{re.escape(callee)}\s+as\b")
        for item in uses:
            if alias.search(item):
                raise ScanError(f"{callee} is imported under another name: {item!r}")
        callers = 0
        for path, code in files.items():
            for match in mention.finditer(code):
                line = code.count("\n", 0, match.start()) + 1
                where = f"{path}:{line}: {callee}"
                if re.search(r"\bfn\s+$", code[max(0, match.start() - 80) : match.start()]):
                    continue
                if not re.match(r"\s*\(", code[match.end() :]):
                    raise ScanError(f"{where} is used without being called")
                inside = [
                    span
                    for span in spans[path]
                    if span[3] < match.start() < span[2]
                ]
                if not inside:
                    raise ScanError(f"{where} is called outside a function body")
                name, fn_start, _, _ = max(inside, key=lambda span: span[1])
                if any(start < fn_start < end for start, end in trait_impls[path]):
                    raise ScanError(f"{where} is called in a trait method, {name}")
                callers += 1
                if is_test_fn(code, fn_start):
                    tests[name] = path
                # A test can call another test, so follow tests too.
                pending.append(name)
        if callers == 0 and callee not in tests:
            raise ScanError(f"{callee} takes the guard but nothing calls it")
    return tests


@functools.cache
def crate_guard_users() -> dict[str, str]:
    return guard_users(crate_sources())


@functools.cache
def core_elevated_tests() -> dict[str, str]:
    return guard_users(crate_sources(CORE_DIR), CORE_ELEVATED_MARK)


def crate_sources(crate_dir: Path = CRATE_DIR) -> dict[str, str]:
    return {
        str(path.relative_to(REPO_ROOT)): path.read_text(encoding="utf-8")
        for path in sorted(crate_dir.rglob("*.rs"))
    }


def is_ours(override: dict, package: str, platform: str | None) -> bool:
    return (
        override.get("test-group") == GROUP
        and override.get("platform") == platform
        and f"package({package})" in override.get("filter", "")
    )


def has_top_level_or(filter_: str) -> bool:
    depth = 0
    for char in filter_:
        depth += {"(": 1, ")": -1}.get(char, 0)
        if depth == 0 and char in "|+":
            return True
    # nextest also spells union `or`; any top-level `or` counts.
    return re.search(r"\bor\b", re.sub(r"\([^()]*\)", "", filter_)) is not None


def other_groups_claiming_the_crate(
    config: dict, package: str = PACKAGE, platform: str | None = None
) -> list[str]:
    """Overrides that could put the package's tests in another group first.

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
                if is_ours(override, package, platform)
            ]
            overrides = overrides[: ours[0]] if ours else overrides
        for override in overrides:
            group = override.get("test-group")
            filter_ = override.get("filter", "")
            if group is None or group == GROUP:
                continue
            scoped = re.match(r"^\s*package\(([\w-]+)\)\s*&", filter_)
            # `&` binds tighter than `|`, so a top-level `|` unscopes it.
            if scoped is None or scoped.group(1) == package or has_top_level_or(filter_):
                claims.append(f"profile.{profile}: {group}: {filter_}")
    return claims


def group_terms(
    config: dict, package: str = PACKAGE, platform: str | None = None
) -> list[str]:
    """The name substrings that put a test of the package into the group."""
    filters = [
        override["filter"]
        for override in config.get("profile", {}).get("default", {}).get("overrides", [])
        if is_ours(override, package, platform)
    ]
    if len(filters) != 1:
        raise ValueError(f"expected one {package} override for {GROUP}, found {filters!r}")
    match = re.match(FILTER_RE.format(re.escape(package)), filters[0].strip())
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
        tests = crate_guard_users()
        config = tomllib.loads(NEXTEST_TOML.read_text(encoding="utf-8"))
        self.assertEqual(config["test-groups"][GROUP]["max-threads"], 1)
        self.assertEqual(other_groups_claiming_the_crate(config), [])
        terms = group_terms(config)
        self.assertEqual(
            missing_from_group(tests, terms),
            [],
            f"tests that take {GUARD}() are missing from the {GROUP} filter in "
            f"{NEXTEST_TOML.relative_to(REPO_ROOT)}",
        )

    def test_every_core_elevated_test_is_in_the_group_on_windows(self) -> None:
        tests = core_elevated_tests()
        config = tomllib.loads(NEXTEST_TOML.read_text(encoding="utf-8"))
        self.assertEqual(other_groups_claiming_the_crate(config, CORE_PACKAGE, WINDOWS), [])
        terms = group_terms(config, CORE_PACKAGE, WINDOWS)
        self.assertEqual(
            missing_from_group(tests, terms),
            [],
            f"codex-core tests that call {CORE_ELEVATED_MARK}() are missing from the "
            f"Windows codex-core {GROUP} override in {NEXTEST_TOML.relative_to(REPO_ROOT)}",
        )

    def test_core_scan_finds_the_elevated_tests(self) -> None:
        # Both the integration tests' helper and the unit tests' one, which
        # another file imports.
        self.assertEqual(
            sorted(core_elevated_tests()),
            [
                "pf_27_s06_d2_vault_unreadable_through_tool_launch_under_workspace_write",
                "pf_27_s06_elevated_launch_cannot_read_protected_files_or_core_memory",
                "sec_win_1863_command_exec_localhost_unc_cwd_fails_closed",
                "sec_win_d398_command_exec_non_canonical_cwd",
                "windows_elevated_does_not_create_missing_workspace_metadata",
                "windows_elevated_enforces_deny_read_and_protects_setup_marker",
            ],
        )

    def test_scan_finds_direct_helper_and_nested_helper_users(self) -> None:
        tests = crate_guard_users()
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

    def test_scan_handles_layouts_rustfmt_and_tests_produce(self) -> None:
        source = """
use super::tests::legacy_process_test_guard;

/// Doc comment naming fn not_a_test() and legacy_process_test_guard.
#[test]
#[cfg_attr(
    not(windows),
    ignore = "a long reason that makes rustfmt split this attribute"
)]
fn split_attribute() {
    // fn not_a_test() { legacy_process_test_guard }
    let text = "fn quoted() { legacy_process_test_guard }";
    fn nested(x: u8) -> u8 { x }
    let _guard = legacy_process_test_guard();
    let _ = (text, nested(1), '{', r#"}"#);
}

#[test]
fn calls_a_test() {
    split_attribute();
}

#[test]
fn through_a_nested_helper() {
    fn inner() { let _guard = legacy_process_test_guard(); }
    inner();
}

extern "C" fn callback() {
    let _guard = legacy_process_test_guard();
}

#[test]
fn through_an_extern_helper() {
    callback();
}
"""
        self.assertEqual(
            sorted(guard_users({"x.rs": source})),
            [
                "calls_a_test",
                "split_attribute",
                "through_a_nested_helper",
                "through_an_extern_helper",
            ],
        )

    def test_scan_accepts_the_held_check(self) -> None:
        source = """
static LEGACY_PROCESS_TEST_LOCK: Mutex<()> = Mutex::new(());
fn legacy_process_test_guard() -> MutexGuard<'static, ()> { LEGACY_PROCESS_TEST_LOCK.lock().unwrap() }
fn legacy_process_test_guard_is_held() -> bool { LEGACY_PROCESS_TEST_LOCK.try_lock().is_err() }
fn setup() { assert!(legacy_process_test_guard_is_held()); }
#[test]
fn legacy_one() { let _guard = legacy_process_test_guard(); setup(); }
"""
        self.assertEqual(sorted(guard_users({"x.rs": source})), ["legacy_one"])

    def test_scan_fails_on_uses_it_cannot_follow(self) -> None:
        for case, source in {
            "pointer": "fn h() { let _g = legacy_process_test_guard(); }\n#[test]\nfn t() { run(h); }",
            "binding": "#[test]\nfn t() { let lock = legacy_process_test_guard; let _g = lock(); }",
            "uncalled helper": "fn h() { let _g = legacy_process_test_guard(); }",
            "macro": "macro_rules! m { () => { legacy_process_test_guard() }; }",
            "lock": "static LEGACY_PROCESS_TEST_LOCK: Mutex<()> = Mutex::new(());\n"
            "#[test]\nfn t() { let _g = super::LEGACY_PROCESS_TEST_LOCK.lock(); }",
            "imported lock": "use super::LEGACY_PROCESS_TEST_LOCK as L;\n#[test]\nfn t() { let _g = L.lock(); }",
            "renamed guard": "use super::legacy_process_test_guard as lock;\n#[test]\nfn t() { let _g = lock(); }",
            "renamed helper": "fn h() { let _g = legacy_process_test_guard(); }\n#[test]\nfn a() { h(); }\n"
            "use self::{h as other};\n#[test]\nfn b() { other(); }",
            "trait method": "struct S;\nimpl Drop for S {\n    fn drop(&mut self) {\n"
            "        let _g = legacy_process_test_guard();\n    }\n}\n#[test]\nfn t() { drop(S); }",
        }.items():
            with self.subTest(case=case), self.assertRaises(ScanError):
                guard_users({"x.rs": source})

    def test_check_reports_another_group_that_could_claim_the_crate(self) -> None:
        config = {
            "profile": {
                "default": {
                    "overrides": [
                        {"filter": "package(codex-core) & test(x)", "test-group": "a"},
                        {"filter": "test(elevated_)", "test-group": "b"},
                        {"filter": f"package({PACKAGE}) & test(y)", "test-group": "c"},
                        {"filter": "package(codex-core) & test(x) | test(y)", "test-group": "f"},
                        {"filter": "package(codex-core) & test(x) or test(y)", "test-group": "g"},
                        {"filter": "package(codex-core) & (test(x) | test(y))", "test-group": "h"},
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
                "profile.default: f: package(codex-core) & test(x) | test(y)",
                "profile.default: g: package(codex-core) & test(x) or test(y)",
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

    def test_core_override_must_be_windows_only_and_first(self) -> None:
        core = {
            "platform": WINDOWS,
            "filter": "package(codex-core) & (test(windows_elevated_x))",
            "test-group": GROUP,
        }
        apply_patch = {
            "filter": "package(codex-core) & kind(test) & test(apply_patch_cli)",
            "test-group": "core_apply_patch_cli_integration",
        }
        for overrides, claims in (
            ([core, apply_patch], []),
            ([apply_patch, core], [f"profile.default: {apply_patch['test-group']}: {apply_patch['filter']}"]),
        ):
            config = {"profile": {"default": {"overrides": overrides}}}
            with self.subTest(order=[o["test-group"] for o in overrides]):
                self.assertEqual(other_groups_claiming_the_crate(config, CORE_PACKAGE, WINDOWS), claims)
                self.assertEqual(group_terms(config, CORE_PACKAGE, WINDOWS), ["windows_elevated_x"])
        # Without the platform it is not the Windows override.
        config = {"profile": {"default": {"overrides": [{**core, "platform": None}]}}}
        with self.assertRaises(ValueError):
            group_terms(config, CORE_PACKAGE, WINDOWS)


if __name__ == "__main__":
    unittest.main()
