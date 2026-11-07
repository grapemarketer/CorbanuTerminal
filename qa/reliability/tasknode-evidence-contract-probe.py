"""Build a small test target from exact production helpers and regression tests.

This tests input serialization without compiling the whole terminal. It does
not replace the codex-cli package tests. Use scripts/isolated_rust_tests.py to
run the generated target. --baseline selects old helpers with the current tests.
"""
import argparse
from pathlib import Path
import subprocess
import tomllib


ROOT = Path(__file__).resolve().parents[2]
SOURCE_PATH = "codex-rs/cli/src/tasknode_cmd.rs"
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--baseline", help="Git commit supplying the pre-fix helpers")
parser.add_argument("--tui", action="store_true", help="Reproduce the unchanged TUI classification defect")
args = parser.parse_args()
current = (ROOT / SOURCE_PATH).read_text(encoding="utf-8")
source = (
    subprocess.check_output(
        ["git", "show", f"{args.baseline}:{SOURCE_PATH}"], cwd=ROOT, text=True,
    )
    if args.baseline else current
)
functions = source.split("fn evidence_items_from_summary_and_artifacts(", 1)[1]
functions = "fn evidence_items_from_summary_and_artifacts(" + functions.split(
    "#[cfg(test)]\nfn tasknode_sse_separator", 1
)[0]
tests = current.split("    #[test]\n    fn bare_artifact_urls_preserve_equals_signs", 1)[1]
tests = "    #[test]\n    fn bare_artifact_urls_preserve_equals_signs" + tests.split(
    "    #[test]\n    fn parses_sse_delta_and_done_blocks", 1
)[0]
packages = tomllib.loads((ROOT / "codex-rs/Cargo.lock").read_text())["package"]
versions = {
    name: next(p["version"] for p in packages if p["name"] == name)
    for name in ("serde_json", "pretty_assertions", "url")
}
variant = "baseline" if args.baseline else "fixed"
if args.tui:
    tui = (ROOT / "codex-rs/tui/src/chatwidget/tasknode_menu.rs").read_text(encoding="utf-8")
    functions = "fn evidence_items_from_summary(" + tui.split(
        "fn evidence_items_from_summary(", 1
    )[1].split("#[derive(Debug)]\nenum TaskNodeClientError", 1)[0]
    tests = '''
    #[test]
    fn tui_artifact_type_uses_actual_github_host() {
        let value = "https://notgithub.com/owner/repo/pull/42";
        assert_eq!(evidence_items_from_summary(value), vec![json!({"type":"url","url":value})]);
    }
    '''
    variant = "tui-unfixed"
out = ROOT / "qa/artifacts/tasknode-evidence-contract" / variant
(out / "src").mkdir(parents=True, exist_ok=True)
(out / ".config").mkdir(exist_ok=True)
(out / ".config/nextest.toml").write_text(
    '[profile.local]\ninherits = "default"\nretries = 0\n', encoding="utf-8",
)
(out / "Cargo.toml").write_text(
    '[package]\nname = "tasknode-evidence-contract-probe"\nversion = "0.0.0"\n'
    'edition = "2024"\n[workspace]\n[dependencies]\n' +
    "".join(f'{name} = "={version}"\n' for name, version in versions.items()),
    encoding="utf-8",
)
(out / "src/lib.rs").write_text(
    "use serde_json::{Value, json};\n" + functions +
    "\n#[cfg(test)]\nmod tests {\nuse super::*;\nuse pretty_assertions::assert_eq;\n" +
    tests + "}\n", encoding="utf-8",
)
print(out / "Cargo.toml")
