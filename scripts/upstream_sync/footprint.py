#!/usr/bin/env python3
"""Build scripts/upstream_sync/security_paths.txt: named security paths plus the fork-patch security footprint.

Usage: footprint.py [--repo OWNER/NAME] [--ref REF] [--out-dir DIR]

Named layer: the plan's S3a write set and the conflicted files outside S3a that carry permission, secret, approval
or sandbox logic (docs/management/upstreamSyncPlan.md section 5), plus upstream's new sandbox crates.

Footprint layer: files changed by Corbanu security pull requests merged into main. A PR is a security PR when its
head branch starts with a security-lane prefix or its title names a PF-23..PF-41 sprint or a security surface (see
SEC_BRANCH / SEC_TITLE). The file set is each merge commit's first-parent diff, limited to codex-rs/ and scripts/,
without Markdown and without Cargo manifests/lockfiles, and to paths that exist at REF. Workflows are left out so SG3
measures code, not CI wiring.

Output (one git pathspec per line, so `git diff A B -- $(cat security_paths.txt)` works):
  security_paths.txt        named pathspecs followed by footprint files, de-duplicated
  security_footprint_prs.tsv the PRs the footprint came from (number, branch, title)
"""

import argparse
import json
import os
import re
import subprocess

NAMED = [
    # S3a crates (Corbanu security crates included) and upstream's new sandbox crates.
    *(
        f"codex-rs/{c}/"
        for c in (
            "linux-sandbox",
            "windows-sandbox-rs",
            "network-proxy",
            "sandboxing",
            "execpolicy",
            "execpolicy-legacy",
            "process-hardening",
            "linux-pidfd-spawn",
            "shell-escalation",
            "secret-broker",
            "secret-broker-service",
            "secret-broker-fixture",
            "security-level",
            "security-policy",
            "security-audit",
            "protected-state",
            "content-security",
            "browser-isolation",
            "mxc-sandbox",
            "windows-sandbox-service",
        )
    ),
    # S3a core paths.
    "codex-rs/core/src/unified_exec/",
    "codex-rs/core/src/security/",
    "codex-rs/core/src/exec_policy*",
    "codex-rs/core/src/exec.rs",
    "codex-rs/core/src/exec_tests*",
    "codex-rs/core/src/spawn.rs",
    "codex-rs/core/src/sandbox*",
    "codex-rs/core/src/landlock*",
    "codex-rs/core/src/seatbelt*",
    "codex-rs/core/src/tools/approvals*",
    "codex-rs/core/src/tools/orchestrator*",
    "codex-rs/core/src/tools/sandboxing*",
    "codex-rs/core/src/tools/network_approval*",
    "codex-rs/core/src/tools/runtimes/",
    "codex-rs/core/src/tools/handlers/shell*",
    "codex-rs/core/src/tools/handlers/unified_exec*",
    "codex-rs/core/src/tools/handlers/apply_patch*",
    "codex-rs/core/src/tools/handlers/request_permissions*",
    *(
        f"codex-rs/core/tests/suite/{p}"
        for p in (
            "*sandbox*",
            "exec_policy*",
            "approvals*",
            "unified_exec*",
            "shell*",
            "seatbelt*",
            "landlock*",
            "network*",
            "security*",
            "permissions*",
        )
    ),
    # Named paths owned by other slices (security owner approves); mirrors slices.SEC_ADJ.
    "codex-rs/protocol/src/permissions*",
    "codex-rs/app-server-protocol/*permissions*",
    "codex-rs/core/src/config/permissions*",
    "codex-rs/core/src/config/network_proxy_spec*",
    "codex-rs/core/src/config/mod.rs",
    "codex-rs/core/src/session/mod.rs",
    "codex-rs/core/src/mcp_tool_call*",
    "codex-rs/core/src/safety*",
    "codex-rs/core/src/guardian/",
    "codex-rs/ext/guardian*",
    "codex-rs/login/src/auth/manager*",
    "codex-rs/arg0/",
    "codex-rs/secrets/",
    "codex-rs/cli/src/debug_sandbox*",
    "codex-rs/apply-patch/",
    "codex-rs/tui/src/*approval_overlay*",
    "codex-rs/tui/src/*permission_popups*",
    "codex-rs/tui/src/*windows_sandbox*",
]

SEC_BRANCH = re.compile(
    r"^(sec|sbx|sf\d+|ss\d+|csec|secb|secf|sech|kc|pf84)[-_/]", re.I
)
SEC_TITLE = re.compile(
    r"\bPF-?(2[3-9]|3\d|4[01])\b|security|sandbox|seatbelt|landlock|bwrap|permission|classifier|taint|provenance"
    r"|secret.broker|credential|vault|approval|exec.?policy|network.proxy|protected.state|browser.isolation",
    re.I,
)
EXCLUDE = re.compile(r"(\.md$|(^|/)Cargo\.(toml|lock)$)")


def git(*args):
    return subprocess.run(
        ["git", *args], capture_output=True, text=True, check=True
    ).stdout


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", default="CorbanuCore/CorbanuTerminal")
    ap.add_argument("--ref", default="origin/main")
    ap.add_argument("--out-dir", default=os.path.dirname(os.path.abspath(__file__)))
    a = ap.parse_args()
    prs = json.loads(
        subprocess.run(
            [
                "gh",
                "pr",
                "list",
                "--repo",
                a.repo,
                "--state",
                "merged",
                "-L",
                "5000",
                "--base",
                "main",
                "--json",
                "number,title,headRefName,mergeCommit",
            ],
            capture_output=True,
            text=True,
            check=True,
        ).stdout
    )
    sel = sorted(
        (
            p
            for p in prs
            if p.get("mergeCommit")
            and (SEC_BRANCH.match(p["headRefName"]) or SEC_TITLE.search(p["title"]))
        ),
        key=lambda p: p["number"],
    )
    present = set(git("ls-tree", "-r", "--full-tree", "--name-only", a.ref).split("\n"))
    files = set()
    for p in sel:
        m = p["mergeCommit"]["oid"]
        for f in git("diff", "--name-only", f"{m}^1", m).split("\n"):
            if (
                f.startswith(("codex-rs/", "scripts/"))
                and not EXCLUDE.search(f)
                and f in present
            ):
                files.add(f)
    named = list(dict.fromkeys(NAMED))
    with open(os.path.join(a.out_dir, "security_paths.txt"), "w") as out:
        out.write("\n".join(named + sorted(files - set(named))) + "\n")
    with open(os.path.join(a.out_dir, "security_footprint_prs.tsv"), "w") as out:
        out.write("number\tbranch\ttitle\n")
        for p in sel:
            out.write(
                f"{p['number']}\t{p['headRefName']}\t{p['title'].replace(chr(9), ' ')}\n"
            )
    print(
        f"named {len(named)} footprint files {len(files - set(named))} from {len(sel)} PRs"
    )


if __name__ == "__main__":
    main()
