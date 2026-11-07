#!/usr/bin/env python3

import re
import subprocess
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]


class CiTimeoutBudgetTest(unittest.TestCase):
    def workflow(self, name: str) -> str:
        return (REPO_ROOT / ".github" / "workflows" / name).read_text()

    def test_sdk_cold_build_has_release_headroom(self) -> None:
        sdk = self.workflow("sdk.yml")
        self.assertRegex(sdk, r"(?ms)^  sdks:.*?^    timeout-minutes: 360$")

    def test_linux_bazel_cold_build_jobs_have_release_headroom(self) -> None:
        bazel = self.workflow("bazel.yml")
        for job in ("test", "clippy", "verify-release-build"):
            with self.subTest(job=job):
                self.assertRegex(
                    bazel,
                    rf"(?ms)^  {re.escape(job)}:.*?^    timeout-minutes: 360$",
                )

    def test_argument_lint_platforms_have_release_headroom(self) -> None:
        rust_ci = self.workflow("rust-ci.yml")
        # Linux and macOS run nightly only (cold_bazel), so they get the
        # 360-minute hosted-runner maximum; Windows still runs on PRs.
        for platform, minutes in (("Linux", 360), ("macOS", 360), ("Windows", 180)):
            with self.subTest(platform=platform):
                self.assertRegex(
                    rust_ci,
                    rf"(?m)^          - name: {platform}\n"
                    rf"            runner: .+\n"
                    rf"            timeout_minutes: {minutes}$",
                )


class PrLinuxClippyTest(unittest.TestCase):
    def workflow(self, name: str) -> str:
        return (REPO_ROOT / ".github" / "workflows" / name).read_text()

    def test_pr_runs_linux_gnu_clippy_within_budget(self) -> None:
        self.assertRegex(
            self.workflow("rust-ci.yml"),
            r"(?m)^  lint_build_linux:\n(?:    .*\n)*?"
            r"    uses: \./\.github/workflows/rust-ci-lint-build\.yml\n"
            r"    with:\n"
            r"      runner: ubuntu-24\.04\n"
            r"      target: x86_64-unknown-linux-gnu\n"
            r"      profile: dev\n"
            r"      timeout_minutes: 20$",
        )

    def test_pr_linux_clippy_is_required(self) -> None:
        rust_ci = self.workflow("rust-ci.yml")
        self.assertRegex(rust_ci, r"(?ms)^  results:\n.*?^        lint_build_linux,$")
        self.assertIn(
            "[[ '${{ needs.lint_build_linux.result }}' == 'success' ]]", rust_ci
        )

    def test_pr_runs_windows_clippy_within_budget(self) -> None:
        # #717: same leg (and caches) as postmerge rust-ci-full's Windows clippy.
        self.assertRegex(
            self.workflow("rust-ci.yml"),
            r"(?m)^  lint_build_windows:\n(?:    .*\n)*?"
            r"    if: \$\{\{ needs\.changed\.outputs\.codex == 'true' \|\| needs\.changed\.outputs\.workflows == 'true' \}\}\n"
            r"    uses: \./\.github/workflows/rust-ci-lint-build\.yml\n"
            r"    with:\n"
            r"      runner: windows-2022\n"
            r"      target: x86_64-pc-windows-msvc\n"
            r"      profile: dev\n"
            r"      timeout_minutes: 60\n"
            r"      save_caches: false$",
        )

    def test_pr_windows_clippy_is_required(self) -> None:
        rust_ci = self.workflow("rust-ci.yml")
        self.assertRegex(rust_ci, r"(?ms)^  results:\n.*?^        lint_build_windows,$")
        self.assertIn(
            "[[ '${{ needs.lint_build_windows.result }}' == 'success' ]]", rust_ci
        )

    def test_only_postmerge_lint_legs_save_caches(self) -> None:
        lint_build = self.workflow("rust-ci-lint-build.yml")
        for step in ("Save cargo home cache", "Save sccache cache (fallback)"):
            with self.subTest(step=step):
                self.assertRegex(
                    lint_build,
                    rf"(?m)^      - name: {re.escape(step)}\n"
                    r"        if: .* && inputs\.save_caches && github\.ref == 'refs/heads/main'$",
                )
        self.assertRegex(
            lint_build,
            r"(?m)^      save_caches:\n(?:        .*\n)*?        default: true$",
        )
        # Postmerge (and so main's saved caches) keeps the default; both
        # blocking-ci legs restore them without saving duplicates (#636).
        self.assertNotIn("save_caches", self.workflow("rust-ci-full.yml"))
        self.assertEqual(self.workflow("rust-ci.yml").count("save_caches: false"), 2)

    def test_pr_ci_skips_linux_test_suite(self) -> None:
        self.assertNotIn("nextest", self.workflow("rust-ci.yml"))

    def test_lint_build_definition_is_shared(self) -> None:
        full = self.workflow("rust-ci-full.yml")
        self.assertIn("uses: ./.github/workflows/rust-ci-lint-build.yml", full)
        # Linux legs default to 30 minutes; a leg may set its own budget.
        self.assertRegex(
            full,
            r"(?m)^      timeout_minutes: \$\{\{ matrix\.timeout_minutes \|\| 30 \}\}$",
        )
        self.assertIn(
            "timeout-minutes: ${{ inputs.timeout_minutes }}",
            self.workflow("rust-ci-lint-build.yml"),
        )

    def test_postmerge_runs_windows_clippy_within_budget(self) -> None:
        self.assertRegex(
            self.workflow("rust-ci-full.yml"),
            r"(?m)^          - runner: windows-2022\n"
            r"            target: x86_64-pc-windows-msvc\n"
            r"            profile: dev\n"
            r"            timeout_minutes: 60$",
        )


class PrArgumentCommentLintTest(unittest.TestCase):
    """#636: PRs lint the crates they change, test code included."""

    def rust_ci(self) -> str:
        return (REPO_ROOT / ".github" / "workflows" / "rust-ci.yml").read_text()

    def job(self, name: str) -> str:
        match = re.search(rf"(?ms)^  {re.escape(name)}:\n.*?(?=^  \S|\Z)", self.rust_ci())
        self.assertIsNotNone(match, name)
        return match.group(0)

    def test_changed_job_lists_changed_crates_on_prs(self) -> None:
        changed = self.job("changed")
        self.assertIn(
            "argument_comment_lint_packages: ${{ steps.detect.outputs.argument_comment_lint_packages }}",
            changed,
        )
        self.assertIn(
            'python3 .github/scripts/argument_comment_lint_packages.py "$BASE_SHA" "$HEAD_SHA"',
            changed,
        )

    def test_lint_runs_on_changed_crates_with_tests_within_budget(self) -> None:
        job = self.job("argument_comment_lint_changed")
        self.assertIn("runs-on: ubuntu-24.04", job)
        self.assertIn("timeout-minutes: 30", job)
        self.assertIn(
            "if: ${{ needs.changed.outputs.argument_comment_lint_packages != '' }}", job
        )
        # run.py adds --all-targets (tests included) when no target is given.
        self.assertRegex(
            job,
            r'(?m)python3 tools/argument-comment-lint/run\.py "\$\{args\[@\]\}" -- --keep-going$',
        )
        # #1030: the lint nightly is newer than the stable toolchain, so any
        # crate that builds on stable meets its rust-version on the nightly too.
        self.assertNotIn("--ignore-rust-version", job)
        self.assertNotRegex(job, r"--(lib|bins|tests|all-targets)\b")

    def test_lint_is_required(self) -> None:
        rust_ci = self.rust_ci()
        self.assertRegex(rust_ci, r"(?ms)^  results:\n.*?^        argument_comment_lint_changed,$")
        self.assertIn(
            "[[ '${{ needs.argument_comment_lint_changed.result }}' == 'success' ]]",
            rust_ci,
        )
        # The package list is an output of `changed`, so it must have run.
        self.assertIn("[[ '${{ needs.changed.result }}' == 'success' ]]", rust_ci)
        self.assertIn(
            "NEEDS_CHANGED_OUTPUTS_ARGUMENT_COMMENT_LINT_PACKAGES: "
            "${{ needs.changed.outputs.argument_comment_lint_packages }}",
            rust_ci,
        )

    def test_lint_restores_mains_cargo_home_cache(self) -> None:
        # The path list is part of the cache version: it must match the save.
        def cargo_home_restore(text: str) -> tuple[str, str]:
            match = re.search(
                r"(?ms)^      - name: Restore cargo home cache\n.*?path: \|\n(.*?)^          key: ([^\n]+)",
                text,
            )
            self.assertIsNotNone(match)
            return match.group(1), match.group(2)

        lint_build = (REPO_ROOT / ".github" / "workflows" / "rust-ci-lint-build.yml").read_text()
        paths, key = cargo_home_restore(self.job("argument_comment_lint_changed"))
        main_paths, main_key = cargo_home_restore(lint_build)
        self.assertEqual(paths, main_paths)
        self.assertEqual(
            key,
            main_key.replace("${{ inputs.runner }}", "ubuntu-24.04")
            .replace("${{ inputs.target }}", "x86_64-unknown-linux-gnu")
            .replace("${{ inputs.profile }}", "dev"),
        )

    def test_lint_only_restores_caches(self) -> None:
        job = self.job("argument_comment_lint_changed")
        self.assertIn("actions/cache/restore@", job)
        self.assertNotIn("actions/cache/save@", job)
        self.assertNotRegex(job, r"uses: actions/cache@")


class CacheSavePolicyTest(unittest.TestCase):
    """#636: only main saves Actions caches; PRs restore main's."""

    MAIN = "github.ref == 'refs/heads/main'"
    # Release workflows run on tags or by hand, never on PRs.
    EXEMPT = {"rusty-v8-release.yml"}

    def ci_files(self) -> list[Path]:
        files = sorted((REPO_ROOT / ".github" / "workflows").glob("*.yml"))
        files += sorted((REPO_ROOT / ".github" / "actions").glob("*/action.yml"))
        return [path for path in files if path.name not in self.EXEMPT]

    @staticmethod
    def step_containing(lines: list[str], index: int) -> str:
        start = index
        while not re.match(r"^\s*- ", lines[start]):
            start -= 1
        indent = len(lines[start]) - len(lines[start].lstrip())
        end = index + 1
        while end < len(lines) and (
            not lines[end].strip() or len(lines[end]) - len(lines[end].lstrip()) > indent
        ):
            end += 1
        return "\n".join(lines[start:end])

    def test_every_cache_save_is_main_only(self) -> None:
        saves = 0
        for path in self.ci_files():
            lines = path.read_text().splitlines()
            for index, line in enumerate(lines):
                if "uses: actions/cache/save@" in line:
                    saves += 1
                    with self.subTest(file=path.name, line=index + 1):
                        self.assertIn(self.MAIN, self.step_containing(lines, index))
        self.assertGreater(saves, 10)

    def test_no_combined_restore_and_save_cache_steps(self) -> None:
        for path in self.ci_files():
            with self.subTest(file=path.name):
                self.assertNotRegex(path.read_text(), r"uses: actions/cache@")

    def test_third_party_caches_save_only_on_main(self) -> None:
        # Actions with their own cache: each use must be guarded (save-if,
        # save-cache, cache-save, use-cache, setup-* `cache:`) or disable it.
        caching_actions = re.compile(
            r"uses: (Swatinem/rust-cache|astral-sh/setup-uv|bazel-contrib/setup-bazel|mlugg/setup-zig)@"
        )
        setup_with_cache = re.compile(r"uses: actions/setup-(node|python|go|java)@")
        checked = 0
        for path in self.ci_files():
            lines = path.read_text().splitlines()
            for index, line in enumerate(lines):
                step = None
                if caching_actions.search(line):
                    step = self.step_containing(lines, index)
                elif setup_with_cache.search(line):
                    step = self.step_containing(lines, index)
                    if not re.search(r"(?m)^\s+cache:", step):
                        continue
                if step is None:
                    continue
                checked += 1
                with self.subTest(file=path.name, line=index + 1):
                    self.assertTrue(
                        self.MAIN in step or "use-cache: false" in step, step
                    )
        self.assertGreaterEqual(checked, 6)


class RunScopedCachePruneTest(unittest.TestCase):
    """#636: main keeps only the newest copy of each run-ID-keyed cache."""

    WORKFLOWS = REPO_ROOT / ".github" / "workflows"

    @staticmethod
    def triggers(text: str) -> str:
        return text.split("\njobs:")[0]

    def runs_on_main_automatically(self, text: str) -> bool:
        triggers = self.triggers(text)
        if re.search(r"(?m)^  schedule:", triggers):
            return True
        push = re.search(r"(?ms)^  push:\n((?:    .*\n)*)", triggers + "\n")
        if push is None:
            return False
        filters = push.group(1)
        return "main" in filters or not re.search(r"\b(branches|tags):", filters)

    def prune_workflow_names(self) -> list[str]:
        prune = (self.WORKFLOWS / "cache-prune.yml").read_text()
        names = re.search(r"(?m)^    workflows: \[(.*)\]$", prune)
        self.assertIsNotNone(names)
        return [name.strip() for name in names.group(1).split(",")]

    def test_every_main_workflow_saving_run_scoped_caches_triggers_prune(self) -> None:
        texts = {path.name: path.read_text() for path in self.WORKFLOWS.glob("*.yml")}
        callers: dict[str, set[str]] = {name: set() for name in texts}
        for name, text in texts.items():
            for callee in re.findall(r"uses: \./\.github/workflows/([\w.-]+\.yml)", text):
                callers[callee].add(name)

        def entrypoints(name: str, seen: set[str]) -> set[str]:
            found = {name} if self.runs_on_main_automatically(texts[name]) else set()
            for caller in callers[name] - seen:
                found |= entrypoints(caller, seen | {caller})
            return found

        prune_names = self.prune_workflow_names()
        checked = set()
        for name, text in texts.items():
            if not re.search(r"(?m)^\s+key: .*\$\{\{ github\.run_id \}\}", text):
                continue
            for entry in entrypoints(name, {name}):
                checked.add(entry)
                workflow_name = re.search(r"(?m)^name: (.+)$", texts[entry]).group(1)
                with self.subTest(saver=name, entrypoint=entry):
                    self.assertIn(workflow_name, prune_names)
        self.assertGreaterEqual(
            checked, {"blocking-ci.yml", "postmerge-ci.yml", "nightly-ci.yml", "tmux-smoke.yml"}
        )

    def test_macos_core_security_cache_is_saved_once_per_push(self) -> None:
        # blocking-ci and postmerge both run it on main; only postmerge saves.
        core = (self.WORKFLOWS / "rust-ci-core-security.yml").read_text()
        self.assertRegex(core, r"(?m)^      save_cache:\n(?:        .*\n)*?        default: true$")
        self.assertIn(
            "if: always() && !cancelled() && inputs.save_cache && github.ref == 'refs/heads/main'",
            core,
        )
        self.assertRegex(
            (self.WORKFLOWS / "rust-ci.yml").read_text(),
            r"(?m)^    uses: \./\.github/workflows/rust-ci-core-security\.yml\n(?:    .*\n|      .*\n)*?"
            r"      save_cache: \$\{\{ matrix\.runner != 'macos-15' \}\}$",
        )
        full = (self.WORKFLOWS / "rust-ci-full.yml").read_text()
        macos = re.search(r"(?ms)^  core_security_tests_macos:\n(.*?)^    secrets:", full)
        self.assertIsNotNone(macos)
        self.assertNotIn("save_cache", macos.group(1))

    def test_prune_job_alone_holds_actions_write(self) -> None:
        prune = (self.WORKFLOWS / "cache-prune.yml").read_text()
        self.assertRegex(prune, r"(?m)^permissions: \{\}$")
        self.assertRegex(prune, r"(?m)^      actions: write$")
        self.assertIn("branches: [main]", self.triggers(prune))
        self.assertIn("head_repository.full_name == github.repository", prune)
        self.assertIn("prune_actions_caches.py", prune)
        # Nothing but the checkout runs third-party code next to the token.
        self.assertEqual(re.findall(r"uses: ([\w/-]+)@", prune), ["actions/checkout"])

    def test_no_other_workflow_holds_actions_write(self) -> None:
        holders = {
            path.name
            for path in self.WORKFLOWS.glob("*.yml")
            if re.search(r"(?m)^\s+actions: write$", path.read_text())
        }
        self.assertEqual(holders, {"cache-prune.yml"})


class PrAppServerSchemaFixturesTest(unittest.TestCase):
    """#1165: app-server-protocol schema fixture tests on PRs that change a
    crate whose types are in the exported schemas (or rust-ci.yml)."""

    SCHEMA_CRATES = (
        "protocol",
        "app-server-protocol",
        "security-policy",
        "utils/absolute-path",
        "utils/path-uri",
        "ext/items",
    )

    def workflow(self, name: str) -> str:
        return (REPO_ROOT / ".github" / "workflows" / name).read_text()

    def job(self, text: str, name: str) -> str:
        match = re.search(rf"(?ms)^  {re.escape(name)}:\n.*?(?=^  \S|\Z)", text)
        self.assertIsNotNone(match, name)
        return match.group(0)

    def detect(self, files: list[str]) -> bool:
        """Runs the `changed` job's path loop on `files`."""
        script = self.job(self.workflow("rust-ci.yml"), "changed")
        loop = re.search(r'(?ms)^ +for f in "\$\{files\[@\]\}"; do\n.*?^ +done$', script)
        self.assertIsNotNone(loop)
        program = "\n".join(
            [
                "set -euo pipefail",
                "files=(" + " ".join(f"'{f}'" for f in files) + ")",
                "codex=false argument_comment_lint=false argument_comment_lint_package=false",
                "workflows=false windows_native_tests=false app_server_schema=false",
                loop.group(0),
                'echo "$app_server_schema"',
            ]
        )
        result = subprocess.run(
            ["bash", "-c", program], capture_output=True, text=True, check=True
        )
        return result.stdout.strip() == "true"

    def test_runs_for_the_schema_crates_and_this_workflow(self) -> None:
        for crate in self.SCHEMA_CRATES:
            with self.subTest(crate=crate):
                self.assertTrue(self.detect(["README.md", f"codex-rs/{crate}/src/lib.rs"]))
        self.assertTrue(self.detect([".github/workflows/rust-ci.yml"]))
        for files in (
            ["codex-rs/core/src/lib.rs", "README.md", "docs/ci.md"],
            ["codex-rs/protocol-extra/src/lib.rs", "codex-rs/app-server/src/lib.rs"],
            ["codex-rs/utils/path-utils/src/lib.rs", "codex-rs/ext/web-search/src/lib.rs"],
            [".github/workflows/rust-ci-full.yml", "codex-rs/Cargo.lock.bak"],
            ["codex-rs/Cargo.toml.bak"],
            ["codex-rs/codex-experimental-api-macros-extra/src/lib.rs"],
        ):
            with self.subTest(files=files):
                self.assertFalse(self.detect(files))

    def test_runs_for_schema_generation_inputs(self) -> None:
        # #1298: the macro controls experimental-field filtering, and a
        # schemars/ts-rs feature or lockfile update can change the output.
        for path in (
            "codex-rs/codex-experimental-api-macros/src/lib.rs",
            "codex-rs/codex-experimental-api-macros/Cargo.toml",
            "codex-rs/Cargo.toml",
            "codex-rs/Cargo.lock",
        ):
            with self.subTest(path=path):
                self.assertTrue(self.detect([path]))

    def test_watches_every_schema_deriving_dependency(self) -> None:
        # Workspace crates that codex-app-server-protocol depends on
        # (transitively, normal and build dependencies) and that derive
        # JsonSchema or TS can change the exported schemas.
        import tomllib

        codex_rs = REPO_ROOT / "codex-rs"
        workspace = tomllib.loads((codex_rs / "Cargo.toml").read_text())
        paths = {
            name: spec["path"]
            for name, spec in workspace["workspace"]["dependencies"].items()
            if isinstance(spec, dict) and "path" in spec
        }
        seen, todo = set(), ["codex-app-server-protocol"]
        while todo:
            manifest = tomllib.loads((codex_rs / paths[todo.pop()] / "Cargo.toml").read_text())
            for section in ("dependencies", "build-dependencies"):
                for dep in manifest.get(section, {}):
                    if dep in paths and dep not in seen:
                        seen.add(dep)
                        todo.append(dep)
        derive = re.compile(r"#\[derive\([^\]]*\b(JsonSchema|TS)\b")
        exporting = sorted(
            paths[dep]
            for dep in seen
            if any(derive.search(rs.read_text()) for rs in (codex_rs / paths[dep]).rglob("*.rs"))
        )
        self.assertEqual(sorted(set(self.SCHEMA_CRATES) - {"app-server-protocol"}), exporting)
        for path in exporting:
            with self.subTest(path=path):
                self.assertTrue(self.detect([f"codex-rs/{path}/src/lib.rs"]))

    def test_skipped_on_main_pushes_and_nightly(self) -> None:
        # Non-PR events get the forced file list; postmerge rust-ci-full
        # runs the schema fixture tests on main.
        forced = re.search(
            r'(?m)^            files=\((.*)\)$', self.job(self.workflow("rust-ci.yml"), "changed")
        )
        self.assertIsNotNone(forced)
        files = re.findall(r'"([^"]+)"', forced.group(1))
        self.assertIn("codex-rs/force", files)
        self.assertFalse(self.detect(files))

    def test_pr_job_runs_the_schema_fixture_tests(self) -> None:
        rust_ci = self.workflow("rust-ci.yml")
        self.assertIn(
            "app_server_schema: ${{ steps.detect.outputs.app_server_schema }}",
            self.job(rust_ci, "changed"),
        )
        job = self.job(rust_ci, "app_server_schema_fixtures")
        self.assertRegex(
            job,
            r"(?m)^    needs: changed\n"
            r"    if: \$\{\{ needs\.changed\.outputs\.app_server_schema == 'true' \}\}$",
        )
        self.assertIn(
            "run: cargo test --locked -p codex-app-server-protocol --lib schema_fixtures", job
        )
        self.assertIn("uses: ./.github/actions/check-clean-worktree", job)

    def test_required_when_it_runs(self) -> None:
        rust_ci = self.workflow("rust-ci.yml")
        self.assertRegex(rust_ci, r"(?ms)^  results:\n.*?^        app_server_schema_fixtures,$")
        self.assertIn(
            "if [[ \"${NEEDS_CHANGED_OUTPUTS_APP_SERVER_SCHEMA}\" == 'true' ]]; then\n"
            "            [[ '${{ needs.app_server_schema_fixtures.result }}' == 'success' ]]",
            rust_ci,
        )
        self.assertIn(
            "NEEDS_CHANGED_OUTPUTS_APP_SERVER_SCHEMA: "
            "${{ needs.changed.outputs.app_server_schema }}",
            rust_ci,
        )

class PrWindowsNativeTestsTest(unittest.TestCase):
    """#636 (owner decision 2026-10-15): native Windows tests on PRs that
    change codex-tui, codex-core, codex-state or codex-login."""

    CRATES = ("tui", "core", "state", "login")
    REUSABLE = "rust-ci-windows-native-tests.yml"

    def workflow(self, name: str) -> str:
        return (REPO_ROOT / ".github" / "workflows" / name).read_text()

    def job(self, text: str, name: str) -> str:
        match = re.search(rf"(?ms)^  {re.escape(name)}:\n.*?(?=^  \S|\Z)", text)
        self.assertIsNotNone(match, name)
        return match.group(0)

    def detect(self, files: list[str]) -> bool:
        """Runs the `changed` job's path loop on `files`."""
        script = self.job(self.workflow("rust-ci.yml"), "changed")
        loop = re.search(r'(?ms)^ +for f in "\$\{files\[@\]\}"; do\n.*?^ +done$', script)
        self.assertIsNotNone(loop)
        program = "\n".join(
            [
                "set -euo pipefail",
                "files=(" + " ".join(f"'{f}'" for f in files) + ")",
                "codex=false argument_comment_lint=false argument_comment_lint_package=false",
                "workflows=false windows_native_tests=false",
                loop.group(0),
                'echo "$windows_native_tests"',
            ]
        )
        result = subprocess.run(
            ["bash", "-c", program], capture_output=True, text=True, check=True
        )
        return result.stdout.strip() == "true"

    def test_runs_only_for_the_four_crates_and_its_workflow(self) -> None:
        for crate in self.CRATES:
            with self.subTest(crate=crate):
                self.assertTrue(self.detect(["README.md", f"codex-rs/{crate}/src/lib.rs"]))
        self.assertTrue(self.detect([f".github/workflows/{self.REUSABLE}"]))
        for files in (
            ["codex-rs/core-api/src/lib.rs", "codex-rs/tui-markdown/x.rs"],
            ["codex-rs/cli/src/main.rs", "codex-rs/Cargo.lock", "docs/ci.md"],
            [".github/workflows/rust-ci.yml", ".bazelrc"],
        ):
            with self.subTest(files=files):
                self.assertFalse(self.detect(files))

    def test_skipped_on_main_pushes_and_nightly(self) -> None:
        # Non-PR events get the forced file list; nightly runs every crate's
        # native Windows tests in bazel.yml instead.
        forced = re.search(
            r'(?m)^            files=\((.*)\)$', self.job(self.workflow("rust-ci.yml"), "changed")
        )
        self.assertIsNotNone(forced)
        files = re.findall(r'"([^"]+)"', forced.group(1))
        self.assertIn("codex-rs/force", files)
        self.assertFalse(self.detect(files))

    def test_pr_job_calls_the_reusable_workflow(self) -> None:
        rust_ci = self.workflow("rust-ci.yml")
        self.assertIn(
            "windows_native_tests: ${{ steps.detect.outputs.windows_native_tests }}",
            self.job(rust_ci, "changed"),
        )
        self.assertRegex(
            self.job(rust_ci, "windows_native_tests"),
            r"(?m)^    needs: changed\n"
            r"    if: \$\{\{ needs\.changed\.outputs\.windows_native_tests == 'true' \}\}\n"
            rf"    uses: \./\.github/workflows/{re.escape(self.REUSABLE)}\n"
            r"    secrets: inherit$",
        )

    def test_required_when_it_runs(self) -> None:
        rust_ci = self.workflow("rust-ci.yml")
        self.assertRegex(rust_ci, r"(?ms)^  results:\n.*?^        windows_native_tests,$")
        self.assertIn(
            "if [[ \"${NEEDS_CHANGED_OUTPUTS_WINDOWS_NATIVE_TESTS}\" == 'true' ]]; then\n"
            "            [[ '${{ needs.windows_native_tests.result }}' == 'success' ]]",
            rust_ci,
        )
        self.assertIn(
            "NEEDS_CHANGED_OUTPUTS_WINDOWS_NATIVE_TESTS: "
            "${{ needs.changed.outputs.windows_native_tests }}",
            rust_ci,
        )

    def test_scoped_native_bazel_test_within_budget(self) -> None:
        reusable = self.workflow(self.REUSABLE)
        triggers = reusable.split("\njobs:")[0]
        self.assertRegex(triggers, r"(?m)^  workflow_call:$")
        self.assertNotRegex(triggers, r"(?m)^  (pull_request|push|schedule):")
        self.assertIn("runs-on: windows-2022", reusable)
        self.assertIn("timeout-minutes: 90", reusable)
        targets = re.search(r"(?ms)^ +bazel_targets=\(\n(.*?)^ +\)$", reusable)
        self.assertIsNotNone(targets)
        self.assertEqual(
            targets.group(1).split(), [f"//codex-rs/{crate}/..." for crate in self.CRATES]
        )
        self.assertIn("--build_tests_only", reusable)
        # Native, like nightly's test-windows-native-main, not the gnullvm
        # cross build that needs BuildBuddy.
        self.assertNotIn("--windows-cross-compile", reusable)

    def test_restores_nightlys_cache_and_saves_none(self) -> None:
        reusable = self.workflow(self.REUSABLE)
        self.assertIn("cache-scope: bazel-test-windows-native-main", reusable)
        # bazel.yml scopes that job's cache by its job ID.
        self.assertIn("cache-scope: bazel-${{ github.job }}", self.job(self.workflow("bazel.yml"), "test-windows-native-main"))
        self.assertNotIn("actions/cache/save@", reusable)
        self.assertNotRegex(reusable, r"uses: actions/cache@")

    def test_login_unit_tests_get_one_rerun_on_windows(self) -> None:
        # #691: one automatic re-run in both native Windows jobs, and only
        # there (not in .bazelrc, which every platform reads).
        flag = "--flaky_test_attempts=//codex-rs/login:login-unit-tests@2"
        native_main = self.job(self.workflow("bazel.yml"), "test-windows-native-main")
        for name, text in (("pr", self.workflow(self.REUSABLE)), ("nightly", native_main)):
            with self.subTest(job=name):
                self.assertEqual(re.findall(r"--flaky_test_attempts=\S+", text), [flag])
        self.assertEqual(self.workflow("bazel.yml").count("--flaky_test_attempts"), 1)
        self.assertNotIn("flaky_test_attempts", (REPO_ROOT / ".bazelrc").read_text())


class NightlyHeavyJobsTest(unittest.TestCase):
    def workflow(self, name: str) -> str:
        return (REPO_ROOT / ".github" / "workflows" / name).read_text()

    def test_cold_linux_bazel_jobs_are_gated(self) -> None:
        bazel = self.workflow("bazel.yml")
        for job in ("test", "clippy", "verify-release-build"):
            with self.subTest(job=job):
                self.assertRegex(
                    bazel,
                    rf"(?m)^  {re.escape(job)}:\n    if: \$\{{\{{ inputs\.cold_bazel \}}\}}$",
                )

    def test_native_windows_main_is_nightly(self) -> None:
        self.assertRegex(
            self.workflow("bazel.yml"),
            r"(?ms)^  test-windows-native-main:\n(?:    #.*?\n)*"
            r"    if: \$\{\{ inputs\.cold_bazel \}\}\n    timeout-minutes: 360$",
        )

    def test_blocking_ci_leaves_heavy_jobs_off(self) -> None:
        self.assertNotIn("cold_bazel", self.workflow("blocking-ci.yml"))

    def test_nightly_runs_heavy_jobs(self) -> None:
        nightly = self.workflow("nightly-ci.yml")
        self.assertIn("schedule:", nightly)
        for workflow in ("bazel.yml", "rust-ci.yml", "sdk.yml"):
            with self.subTest(workflow=workflow):
                self.assertRegex(
                    nightly,
                    rf"(?m)^    uses: \./\.github/workflows/{re.escape(workflow)}\n"
                    rf"    with:\n      cold_bazel: true$",
                )

    def test_nightly_runs_core_developer_accounting_tests_only_nightly(self) -> None:
        self.assertRegex(
            self.workflow("nightly-ci.yml"),
            r"(?m)^    uses: \./\.github/workflows/rust-ci-core-developer-accounting\.yml$",
        )
        triggers = self.workflow("rust-ci-core-developer-accounting.yml").split("\njobs:")[0]
        for trigger in ("pull_request", "push"):
            with self.subTest(trigger=trigger):
                self.assertNotRegex(triggers, rf"(?m)^  {trigger}:")

    def test_slow_argument_lint_platforms_are_gated(self) -> None:
        rust_ci = self.workflow("rust-ci.yml")
        self.assertIn('if [[ "$PLATFORM" != "Windows" && "$COLD_BAZEL" != "true" ]]', rust_ci)

    def test_sdk_bazel_job_is_gated(self) -> None:
        self.assertRegex(
            self.workflow("sdk.yml"),
            r"(?m)^  sdks:\n    if: \$\{\{ inputs\.cold_bazel \}\}$",
        )


if __name__ == "__main__":
    unittest.main()
