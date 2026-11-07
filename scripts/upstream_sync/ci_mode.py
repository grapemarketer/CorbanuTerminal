#!/usr/bin/env python3
"""Sync-branch CI mode (upstreamSyncPlan.md section 4): apply after the skeleton commit, revert in S4.

  ci_mode.py apply  <worktree>   heavy blocking-ci jobs skip PRs whose base is sync/**, other PR workflows ignore
                                 sync/** bases, and sync-package-ci.yml runs package-scoped clippy/tests on pushes
                                 to sync-ci/**
  ci_mode.py revert <worktree>   remove exactly what apply added (every added line ends with the TAG comment)
  ci_mode.py check  <worktree>   exit 1 if any CI-mode line or file is present (S5 entry check)
  ci_mode.py codeowners apply|revert|check <worktree> [@owner]
                                 sync-branch CODEOWNERS block for the named security paths (reverted after main
                                 merge #3, as the last sync-branch commit before the final PR)

Edits are textual so they apply on top of whatever Tier 0 resolution the skeleton produced.
"""

import os
import re
import shutil
import sys

TAG = "# sync-ci-mode"
RESTORE = "# sync-ci-mode-restore {}"
HEAVY = ("bazel", "rust-ci", "sdk")
SKIP_IF = "${{ !startsWith(github.base_ref, 'sync/') }}"
OK_ENV = "${{ startsWith(github.base_ref, 'sync/') && '" + ",".join(HEAVY) + "' || '' }}"
# Whole-workspace PR workflows that can't pass while the tree has markers. The security lanes
# (credential-boundary-canary, security-ingress-contract, security-platform-contract, windows-security-probes)
# and v8-canary keep running on sync PRs and can also be dispatched on the sync branch.
PR_WORKFLOWS = ("orchestrate-tui-matrix.yml", "tmux-smoke.yml")
HERE = os.path.dirname(os.path.abspath(__file__))
NEW_FILES = {".github/workflows/sync-package-ci.yml": os.path.join(HERE, "ci_mode", "sync-package-ci.yml")}
CHECK_SCRIPT = ".github/scripts/check_ci_results.py"
CHECK_OLD = '        if dependency["result"] != "success"\n'
CHECK_NEW = ('        if dependency["result"] != "success"\n'
             '        and not (dependency["result"] == "skipped" and name in os.environ.get("SYNC_SKIPPED_OK", "")'
             '.split(","))  ' + TAG + "\n")


def edit(path, fn):
    text = open(path).read()
    new = fn(text)
    if new == text:
        sys.exit(f"error: no change applied to {path}; update ci_mode.py for the resolved file")
    open(path, "w").write(new)


def blocking(text):
    for job in HEAVY:
        text, n = re.subn(rf"(?m)^(  {re.escape(job)}:\n    name: [^\n]*\n)",
                          lambda m: m.group(1) + f"    if: {SKIP_IF}  {TAG}\n", text)
        if n != 1:
            sys.exit(f"error: blocking-ci job {job} not found")
    text, n = re.subn(r"(?m)^(        env:\n          NEEDS: \$\{\{ toJSON\(needs\) \}\}\n)",
                      lambda m: m.group(1) + f"          SYNC_SKIPPED_OK: {OK_ENV}  {TAG}\n", text)
    if n != 1:
        sys.exit("error: blocking-ci required-job env not found")
    return text


def pr_ignore(text):
    m = re.search(r"(?m)^  pull_request:(?P<rest>[^\n]*)\n", text)
    if not m:
        sys.exit("error: no pull_request trigger found")
    block = []
    for line in text[m.end():].splitlines():
        if line.strip() and not line.startswith("    "):
            break
        block.append(line)
    if any(line.startswith("    branches:") for line in block):
        return text  # already restricted to listed bases
    line = f'    branches-ignore: ["sync/**"]  {TAG}\n'
    if m.group("rest").strip() == "{}":
        return text[:m.start()] + f"  pull_request:  {RESTORE}\n" + line + text[m.end():]
    return text[:m.end()] + line + text[m.end():]


def apply(wt):
    edit(os.path.join(wt, ".github/workflows/blocking-ci.yml"), blocking)
    edit(os.path.join(wt, CHECK_SCRIPT), lambda t: t.replace(CHECK_OLD, CHECK_NEW, 1))
    for wf in PR_WORKFLOWS:
        p = os.path.join(wt, ".github/workflows", wf)
        if os.path.exists(p):
            edit(p, pr_ignore)
    for dst, src in NEW_FILES.items():
        shutil.copyfile(src, os.path.join(wt, dst))
    print("CI mode applied")


def revert(wt):
    for root in (".github/workflows", ".github/scripts"):
        for name in os.listdir(os.path.join(wt, root)):
            p = os.path.join(wt, root, name)
            if not os.path.isfile(p):
                continue
            text = open(p).read()
            if TAG not in text or os.path.join(root, name) in NEW_FILES:
                continue
            out = [line for line in text.splitlines(keepends=True) if not line.rstrip("\n").endswith(TAG)]
            open(p, "w").write("".join(out).replace(f"  pull_request:  {RESTORE}\n", "  pull_request: {}\n"))
    for dst in NEW_FILES:
        p = os.path.join(wt, dst)
        if os.path.exists(p):
            os.remove(p)
    print("CI mode reverted")


def check(wt):
    hits = []
    for root in (".github/workflows", ".github/scripts"):
        for name in os.listdir(os.path.join(wt, root)):
            p = os.path.join(wt, root, name)
            if os.path.isfile(p) and TAG in open(p).read():
                hits.append(os.path.join(root, name))
    if hits:
        sys.exit("CI mode still present: " + ", ".join(hits))
    print("no CI-mode lines")


def codeowners_pattern(spec):
    """Git pathspec -> CODEOWNERS pattern. In CODEOWNERS `*` stops at `/`, so a leading wildcard in the last
    segment (e.g. tui/src/*approval_overlay*) must become `**/`."""
    head, _, last = spec.rpartition("/")
    if last.startswith("*"):
        return f"/{head}/**/{last}"
    return f"/{spec}"


CO_BEGIN = "# BEGIN sync-branch security owners (upstream sync; revert before the final PR)"
CO_END = "# END sync-branch security owners"


def codeowners(action, wt, owner=None):
    sys.path.insert(0, HERE)
    import footprint
    p = os.path.join(wt, ".github", "CODEOWNERS")
    text = open(p).read() if os.path.exists(p) else ""
    start, end = text.find(CO_BEGIN), text.find(CO_END)
    if (start == -1) != (end == -1) or end < start:
        sys.exit("error: CODEOWNERS block markers are incomplete; fix by hand")
    if action == "check":
        if start != -1:
            sys.exit("CODEOWNERS block still present")
        print("no CODEOWNERS block")
        return
    if start != -1:
        text = text[:start].rstrip("\n") + "\n" + text[end + len(CO_END):].lstrip("\n")
    if action == "apply":
        if not owner or not re.fullmatch(r"@[A-Za-z0-9][A-Za-z0-9-]*(/[A-Za-z0-9._-]+)?", owner):
            sys.exit("error: codeowners apply needs an @owner")
        # Last matching pattern wins, so this block goes at the end.
        block = [CO_BEGIN] + [f"{codeowners_pattern(spec)} {owner}" for spec in footprint.NAMED] + [CO_END]
        text = text.rstrip("\n") + "\n\n" + "\n".join(block) + "\n"
    open(p, "w").write(text)
    print(f"CODEOWNERS block {'added' if action == 'apply' else 'removed'}")


if __name__ == "__main__":
    if sys.argv[1] == "codeowners":
        codeowners(sys.argv[2], sys.argv[3], sys.argv[4] if len(sys.argv) > 4 else None)
    else:
        {"apply": apply, "revert": revert, "check": check}[sys.argv[1]](sys.argv[2])
