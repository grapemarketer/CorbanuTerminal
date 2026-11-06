#!/usr/bin/env python3
"""Conflict statistics for a worktree in the middle of a merge (after `git merge --no-commit`).

Usage: conflict_stats.py <worktree> <out-prefix>   (writes <out-prefix>.json, prints a summary)

"lines" counts lines between `<<<<<<<` and `>>>>>>>`, without separators and without the zdiff3 base section,
so numbers are comparable between the default and zdiff3 conflict styles.
"""

import collections
import json
import os
import re
import subprocess
import sys

UNMERGED = ("DD", "AU", "UD", "UA", "DU", "AA", "UU")


def unmerged_paths(wt):
    st = subprocess.run(
        ["git", "-C", wt, "status", "--porcelain=v1", "-z"], capture_output=True, text=True, check=True
    ).stdout
    out = []
    for rec in st.split("\0"):
        if len(rec) > 3 and rec[:2] in UNMERGED:
            out.append((rec[:2], rec[3:]))
    return out


def area(p):
    q = p.removeprefix("codex-rs/")
    if p.startswith(".github/"):
        return "CI/workflows"
    if re.search(r"(Cargo\.lock|MODULE\.bazel\.lock|\.schema\.json|models\.json)$", p) or "/schema/" in p:
        return "schemas/locks"
    top = q.split("/")[0]
    if top == "tui":
        return "tui"
    if top in ("core", "core-api"):
        return "core"
    if top.startswith("app-server-protocol") or top == "protocol":
        return "protocol"
    if top.startswith("app-server"):
        return "app-server"
    if top in ("linux-sandbox", "network-proxy", "shell-escalation", "sandboxing", "process-hardening", "execpolicy",
               "execpolicy-legacy"):
        return "sandboxing"
    if top.startswith("windows-sandbox"):
        return "windows-sandbox"
    if top in ("config", "model-provider", "model-provider-info", "models-manager", "login", "chatgpt", "codex-api",
               "backend-client"):
        return "config/providers/auth"
    if top in ("exec", "exec-server"):
        return "exec"
    return "other"


def count_markers(full):
    hunks = lines = 0
    if not os.path.isfile(full):
        return 0, 0
    state = None  # None, "ours", "base", "theirs"
    with open(full, "rb") as f:
        for raw in f:
            if raw.startswith(b"<<<<<<< "):
                state, hunks = "ours", hunks + 1
                continue
            if state and raw.startswith(b"||||||| "):
                state = "base"
                continue
            if state and raw.rstrip(b"\r\n") == b"=======":
                state = "theirs"
                continue
            if state and raw.startswith(b">>>>>>> "):
                state = None
                continue
            if state in ("ours", "theirs"):
                lines += 1
    return hunks, lines


def collect(wt):
    rows = []
    for code, p in unmerged_paths(wt):
        hunks, lines = count_markers(os.path.join(wt, p))
        rows.append({
            "code": code, "path": p, "area": area(p), "hunks": hunks, "lines": lines,
            "test": bool(re.search(r"(tests?/|_tests?\.rs$|/tests\.rs$|\.snap$)", p)), "snap": p.endswith(".snap"),
        })
    return rows


def main():
    wt, out = sys.argv[1], sys.argv[2]
    rows = collect(wt)
    with open(out + ".json", "w") as f:
        json.dump(rows, f, indent=0)
    by = collections.defaultdict(lambda: [0, 0, 0, 0, 0])
    for r in rows:
        b = by[r["area"]]
        b[0] += 1
        b[1] += r["hunks"]
        b[2] += r["lines"]
        b[3] += r["test"]
        b[4] += r["code"] != "UU"
    tot = [sum(b[i] for b in by.values()) for i in range(5)]
    print(f"files {tot[0]} hunks {tot[1]} lines {tot[2]} tests {tot[3]} snaps {sum(r['snap'] for r in rows)} "
          f"non-content {tot[4]}")
    print("codes", dict(collections.Counter(r["code"] for r in rows)))
    for a, b in sorted(by.items(), key=lambda kv: -kv[1][0]):
        print(f"{a:24} files {b[0]:4} hunks {b[1]:5} lines {b[2]:6} tests {b[3]:4} non-content {b[4]}")
    print("top hotspots by lines:")
    for r in sorted(rows, key=lambda r: -r["lines"])[:15]:
        print(f"  {r['path']} {r['hunks']} {r['lines']}")


if __name__ == "__main__":
    main()
