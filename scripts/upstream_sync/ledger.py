#!/usr/bin/env python3
"""Write the sync ledger for a worktree in the middle of `git merge --no-commit <tag SHA>`.

Usage: ledger.py <worktree> <tag> <out.tsv> [--security-paths FILE]

Rows (one per path, each path exactly once):
  conflict          every unmerged path (content, modify/delete, delete/modify, add/add, file-location)
  security-review   upstream changed it since the merge base, it merged without a conflict, and it is on a
                    security path; reviewed diff-level in S3b
  migration         an upstream-added SQL migration whose number is at or below main's highest; renumbered in S1c

Columns: path kind code area slice tier owner contributor status resolution hunks lines test snap
         sec_named sec_footprint note
status is open|resolved; resolution is one of ours|theirs|merged|ported|deleted once resolved.
"""

import argparse
import fnmatch
import os
import re
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import conflict_stats  # noqa: E402
import footprint  # noqa: E402
import slices  # noqa: E402

KIND = {"UU": "content", "UD": "modify/delete", "DU": "delete/modify", "AA": "add/add", "AU": "file-location",
        "UA": "file-location", "DD": "delete/delete"}
COLS = ["path", "kind", "code", "area", "slice", "tier", "owner", "contributor", "status", "resolution", "hunks",
        "lines", "test", "snap", "sec_named", "sec_footprint", "note"]


def git(wt, *args):
    return subprocess.run(["git", "-C", wt, *args], capture_output=True, text=True, check=True).stdout


def load_paths(path):
    """Named pathspecs come from footprint.NAMED; every other line of security_paths.txt is a footprint file."""
    named = list(footprint.NAMED)
    lines = {line.strip() for line in open(path) if line.strip()}
    return named, lines - set(named)


def matches(p, specs):
    for s in specs:
        if s.endswith("/") and p.startswith(s):
            return True
        if "*" in s and fnmatch.fnmatchcase(p, s):
            return True
        if p == s:
            return True
    return False


def migration_rows(wt, base, tag):
    rows = []
    ours = git(wt, "ls-tree", "-r", "--name-only", "HEAD").split("\n")
    dirs = sorted({os.path.dirname(p) for p in ours if re.search(r"/migrations/\d{4}_[^/]+\.sql$", p)})
    for d in dirs:
        mine = {os.path.basename(p) for p in ours if os.path.dirname(p) == d and p.endswith(".sql")}
        at_tag = set(git(wt, "ls-tree", "--name-only", f"{tag}:{d}").split()) if d else set()
        at_base = set(git(wt, "ls-tree", "--name-only", f"{base}:{d}").split())
        added = sorted(f for f in at_tag - at_base - mine if re.match(r"\d{4}_", f))
        if not added:
            continue
        top = max(int(f[:4]) for f in mine if re.match(r"\d{4}_", f))
        nxt = top + 1
        for f in added:
            # Once one upstream migration moves past main's highest number, every later one moves too.
            if int(f[:4]) > top and nxt == top + 1:
                continue
            clash = sorted(m for m in mine if m[:4] == f[:4])
            new = f"{nxt:04d}{f[4:]}"
            nxt += 1
            rows.append({"path": f"{d}/{f}", "kind": "migration", "code": "", "area": "other",
                         "slice": "S1c other crates", "tier": "1", "owner": "unassigned", "contributor": "no",
                         "status": "open", "resolution": "", "hunks": 0, "lines": 0, "test": False, "snap": False,
                         "sec_named": False, "sec_footprint": False,
                         "note": f"renumber to {new}; collides with main {','.join(clash) or '(none)'}"})
    return rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("worktree")
    ap.add_argument("tag")
    ap.add_argument("out")
    ap.add_argument("--security-paths", default=os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                                             "security_paths.txt"))
    ap.add_argument("--tier0-dir", help="prepared Tier 0 resolutions; marks those rows resolved")
    a = ap.parse_args()
    wt = a.worktree
    tag = git(wt, "rev-parse", f"{a.tag}^{{commit}}").strip()
    base = git(wt, "merge-base", "HEAD", tag).strip()
    named, footprint = load_paths(a.security_paths)
    rows, seen = [], set()
    for r in conflict_stats.collect(wt):
        p = r["path"]
        s = slices.slice_of(p)
        tier, owner, friendly = slices.SLICES[s]
        note = ""
        if r["code"] == "UD":
            note = "deleted upstream; skeleton keeps ours"
        elif r["code"] in ("AU", "UA"):
            note = "added on main inside a directory upstream renamed; git placed it at this path"
        rows.append({**r, "kind": KIND.get(r["code"], r["code"]), "slice": s, "tier": tier, "owner": owner,
                     "contributor": "yes" if friendly else "no", "status": "open", "resolution": "",
                     "sec_named": slices.is_named_security(p) or matches(p, named),
                     "sec_footprint": p in footprint, "note": note})
        seen.add(p)
    changed = git(wt, "diff", "--name-only", base, tag).split("\n")
    for p in changed:
        if not p or p in seen or slices.TIER0.search(p):
            continue
        n, f = slices.is_named_security(p) or matches(p, named), p in footprint
        if n or f:
            rows.append({"path": p, "kind": "security-review", "code": "", "area": conflict_stats.area(p),
                         "slice": "S3b security review", "tier": "S3b", "owner": "security owner", "contributor": "no",
                         "status": "open", "resolution": "", "hunks": 0, "lines": 0, "test": False, "snap": False,
                         "sec_named": n, "sec_footprint": f, "note": "upstream change merged without conflict"})
            seen.add(p)
    rows += [r for r in migration_rows(wt, base, tag) if r["path"] not in seen]
    if a.tier0_dir:
        import tier0
        actions = {r["path"]: r["action"] for r in tier0.read_manifest(a.tier0_dir)}
        for r in rows:
            if r["slice"] == "S0 manifests/locks/CI" and actions.get(r["path"]) in ("file", "delete"):
                r["status"] = "resolved"
                r["resolution"] = "deleted" if actions[r["path"]] == "delete" else "merged"
    with open(a.out, "w") as out:
        out.write(f"# ledger for {a.tag} {tag} merged into {git(wt, 'rev-parse', 'HEAD').strip()} "
                  f"(merge base {base})\n")
        out.write("\t".join(COLS) + "\n")
        for r in rows:
            out.write("\t".join(str(r[c]).lower() if isinstance(r[c], bool) else str(r[c]) for c in COLS) + "\n")
    kinds = {}
    for r in rows:
        kinds[r["kind"]] = kinds.get(r["kind"], 0) + 1
    print(f"ledger rows {len(rows)} {kinds}")
    print(f"conflicted with security flag: named {sum(r['sec_named'] for r in rows if r['code'])}, "
          f"footprint {sum(r['sec_footprint'] for r in rows if r['code'])}")


if __name__ == "__main__":
    main()
