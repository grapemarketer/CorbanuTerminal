#!/usr/bin/env python3
"""Slice classifier for the upstream sync (docs/management/upstreamSyncPlan.md section 5).

This classifier is authoritative: every conflicted path maps to exactly one slice.

Usage: slices.py <conflicts.json> [--list SLICE_PREFIX] [--adj]
"""

import collections
import json
import re
import sys

SEC_CRATES = (
    "linux-sandbox",
    "windows-sandbox-rs",
    "network-proxy",
    "sandboxing",
    "process-hardening",
    "execpolicy",
    "execpolicy-legacy",
    "shell-escalation",
    "secret-broker",
    "secret-broker-service",
    "security-level",
    "security-policy",
    "protected-state",
    "content-security",
    "browser-isolation",
    "linux-pidfd-spawn",
)
SEC_CORE = re.compile(
    r"^core/(src/(tools/(approvals|orchestrator|sandboxing|network_approval|runtimes/"
    r"|handlers/(shell|unified_exec|apply_patch|request_permissions))"
    r"|unified_exec/|exec_policy|security/|exec\.rs|exec_tests|sandbox|spawn\.rs|landlock|seatbelt)"
    r"|tests/suite/(.*sandbox|exec_policy|approvals|unified_exec|shell|seatbelt|landlock|network|security|permissions))"
)
ABOVE_CORE = (
    "app-server",
    "app-server-client",
    "app-server-daemon",
    "app-server-transport",
    "chatgpt",
    "cli",
    "core-api",
    "exec",
    "ext/image-generation",
    "ext/memories",
    "ext/web-search",
    "memories/write",
    "thread-manager-sample",
    "tui",
    "mcp-server",
)
# Conflicted paths outside S3a that carry permission, sandbox, secret or approval logic (named security paths).
SEC_ADJ = re.compile(
    r"^(protocol/src/permissions|app-server-protocol/.*permissions"
    r"|core/src/(config/(permissions|network_proxy_spec|mod\.rs)|session/mod\.rs|mcp_tool_call|safety|guardian/)"
    r"|login/src/auth/manager|arg0/|secrets/|cli/src/debug_sandbox|apply-patch/"
    r"|tui/src/.*(approval_overlay|permission_popups|windows_sandbox)|ext/guardian)"
)

# slice -> (tier, owner role, contributor-friendly)
SLICES = {
    "S0 manifests/locks/CI": ("0", "integrator", False),
    "S1a protocol": ("1", "unassigned", False),
    "S1b config/providers/auth": ("1", "unassigned", True),
    "S1c other crates": ("1", "unassigned", True),
    "S1d tooling/repo": ("1", "unassigned", True),
    "S2a core session/client/turn": ("2", "unassigned", False),
    "S2b core tools (non-shell)": ("2", "unassigned", True),
    "S2c core agents": ("2", "unassigned", False),
    "S3a security resolution": ("1+2", "security owner", False),
    "S4a tui app/lib/onboarding/status": ("3", "unassigned", False),
    "S4b tui chatwidget/bottom_pane": ("3", "unassigned", False),
    "S4c app-server": ("3", "unassigned", True),
    "S4d exec": ("3", "unassigned", True),
    "S4e other crates above core": ("3", "unassigned", True),
}

TIER0 = re.compile(
    r"(^|/)(Cargo\.toml|Cargo\.lock|MODULE\.bazel(\.lock)?|BUILD\.bazel|rust-toolchain\.toml)$"
)


def slice_of(p):
    if p.startswith((".github/workflows/", ".github/actions/", ".github/scripts/")):
        return "S0 manifests/locks/CI"
    # Every Cargo/Bazel manifest and lockfile is Tier 0, including those under tools/ or sdk/.
    if (
        TIER0.search(p)
        or p.startswith(("codex-rs/.cargo/", "codex-rs/.config/", "patches/"))
        or p
        in (
            "codex-rs/deny.toml",
            "codex-rs/.gitignore",
        )
    ):
        return "S0 manifests/locks/CI"
    if (
        p.startswith(
            (
                ".github/",
                "scripts/",
                "tools/",
                ".codex/",
                ".devcontainer/",
                "codex-cli/",
                "docs/",
                "sdk/",
            )
        )
        or "/" not in p
    ):
        return "S1d tooling/repo"
    q = p.removeprefix("codex-rs/")
    top = q.split("/")[0]
    if top in SEC_CRATES or SEC_CORE.match(q):
        return "S3a security resolution"
    if top == "protocol" or top.startswith("app-server-protocol"):
        return "S1a protocol"
    two = "/".join(q.split("/")[:2])
    if top in ABOVE_CORE or two in ABOVE_CORE:
        if top == "tui":
            if re.match(r"tui/src/(chatwidget|bottom_pane)", q):
                return "S4b tui chatwidget/bottom_pane"
            return "S4a tui app/lib/onboarding/status"
        if top.startswith("app-server"):
            return "S4c app-server"
        if top == "exec":
            return "S4d exec"
        return "S4e other crates above core"
    if top in (
        "config",
        "model-provider",
        "model-provider-info",
        "models-manager",
        "login",
        "codex-api",
        "backend-client",
        "provider-auth",
        "vault",
        "aws-auth",
        "keyring-store",
    ):
        return "S1b config/providers/auth"
    if top in ("core", "core-api"):
        if re.match(
            r"core/(src/agent/|src/tools/handlers/multi_agents|assets/agent"
            r"|tests/suite/(subagent|spawn|agent|multi_agent))",
            q,
        ):
            return "S2c core agents"
        if q.startswith("core/src/tools/"):
            return "S2b core tools (non-shell)"
        return "S2a core session/client/turn"
    if top in ("agent-roles", "message-board", "runtime-agent-instance"):
        return "S2c core agents"
    return "S1c other crates"


def is_named_security(p):
    """True for paths the plan names as security paths (S3a plus SEC_ADJ)."""
    return slice_of(p).startswith("S3") or bool(
        SEC_ADJ.match(p.removeprefix("codex-rs/"))
    )


def main():
    rows = json.load(open(sys.argv[1]))
    agg = collections.defaultdict(lambda: [0, 0, 0])
    for r in rows:
        s = slice_of(r["path"])
        r["slice"] = s
        r["sec_review"] = is_named_security(r["path"])
        a = agg[s]
        a[0] += 1
        a[1] += r["hunks"]
        a[2] += r["lines"]
    if len(sys.argv) > 3 and sys.argv[2] == "--list":
        for r in rows:
            if r["slice"].startswith(sys.argv[3]):
                print(r["code"], r["hunks"], r["path"])
        return
    for s in sorted(agg):
        print(f"{s:36} files {agg[s][0]:4} hunks {agg[s][1]:5} lines {agg[s][2]:6}")
    print("total", sum(a[0] for a in agg.values()))
    adj = [
        r["path"] for r in rows if r["sec_review"] and not r["slice"].startswith("S3")
    ]
    print("security-review paths outside S3:", len(adj))
    if "--adj" in sys.argv:
        print("\n".join(adj))


if __name__ == "__main__":
    main()
