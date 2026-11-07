#!/usr/bin/env python3
"""Raise merged Cargo.lock entries to the upstream tag's version where the tag is newer (same semver line).

Usage: lock_max.py <codex-rs dir> <tag> [--dry-run]

The skeleton starts from main's Cargo.lock and lets cargo add what the merged manifests need, so main's newer
versions (security bumps) stay. Upstream bumps crates too; without this step the merged lock would keep main's
older copy of a crate upstream had already updated (e.g. quinn-proto 0.11.14 vs 0.11.18). For each registry crate
present in both, if the tag has a higher version on the same semver line, run `cargo update -p name@cur --precise
tagver`. Never lowers a version: crates new to main keep the newest compatible release cargo picked when the
skeleton ran (often newer than upstream's, e.g. with security fixes); SG1 records the merged lock against the tag's
lock (`sg1_lockdiff.py <tag> <skeleton>`) so those differences are reviewed.
"""

import subprocess
import sys
import tomllib

REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"


def parse(v):
    core = v.split("+")[0]
    main, _, pre = core.partition("-")
    nums = tuple(int(x) for x in main.split("."))
    return nums, pre


def line(nums):
    # semver-compatible line: first non-zero component and everything before it
    out = []
    for n in nums:
        out.append(n)
        if n != 0:
            break
    return tuple(out)


def versions(text):
    out = {}
    for p in tomllib.loads(text).get("package", []):
        if p.get("source") == REGISTRY:
            out.setdefault(p["name"], set()).add(p["version"])
    return out


def main():
    root, tag = sys.argv[1], sys.argv[2]
    dry = "--dry-run" in sys.argv
    tag_lock = subprocess.run(
        ["git", "-C", root, "show", f"{tag}:codex-rs/Cargo.lock"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    theirs = versions(tag_lock)
    merged = versions(open(f"{root}/Cargo.lock").read())
    todo = []
    for name, cur_set in merged.items():
        for cur in cur_set:
            cn, cp = parse(cur)
            best = None
            for tv in theirs.get(name, ()):
                tn, tp = parse(tv)
                if (
                    line(tn) == line(cn)
                    and not tp
                    and (tn, tp) > (cn, cp)
                    and tv not in cur_set
                ):
                    best = tv if best is None or parse(tv) > parse(best) else best
            if best:
                todo.append((name, cur, best))
    for name, cur, new in sorted(todo):
        print(f"{name} {cur} -> {new}")
        if not dry:
            r = subprocess.run(
                ["cargo", "update", "--quiet", "-p", f"{name}@{cur}", "--precise", new],
                cwd=root,
                capture_output=True,
                text=True,
            )
            if r.returncode:
                print(
                    f"  skipped: {r.stderr.strip().splitlines()[-1] if r.stderr.strip() else 'cargo update failed'}"
                )
    print(
        f"{len(todo)} crates moved to the tag's version" + (" (dry run)" if dry else "")
    )


if __name__ == "__main__":
    main()
