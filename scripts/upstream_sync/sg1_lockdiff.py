#!/usr/bin/env python3
"""SG1 package comparison between two git revisions (third-party packages only).

Usage: sg1_lockdiff.py <old-rev> <new-rev> [--md]

Compares codex-rs/Cargo.lock (crates outside the workspace), MODULE.bazel (bazel_dep, http_archive/http_file URLs and
git_override/git_repository remotes) and pnpm-lock.yaml (package keys). Reports added / removed / changed packages
and every new non-registry source (git or URL), which SG1 reviews by hand.
"""

import argparse
import re
import subprocess
import tomllib

REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"


def show(rev, path):
    r = subprocess.run(["git", "show", f"{rev}:{path}"], capture_output=True, text=True)
    return r.stdout if r.returncode == 0 else ""


def cargo(rev):
    data = tomllib.loads(show(rev, "codex-rs/Cargo.lock") or "")
    pkgs = {}
    for p in data.get("package", []):
        src = p.get("source")
        if src is None:  # workspace member
            continue
        pkgs.setdefault(p["name"], set()).add((p["version"], src))
    return pkgs


def bazel(rev):
    text = show(rev, "MODULE.bazel")
    deps = dict(
        re.findall(
            r'bazel_dep\(\s*name\s*=\s*"([^"]+)",\s*version\s*=\s*"([^"]+)"', text
        )
    )
    urls = set(re.findall(r'"(https?://[^"]+)"', text))
    remotes = set(re.findall(r'remote\s*=\s*"([^"]+)"', text))
    return deps, urls, remotes


def pnpm(rev):
    text = show(rev, "pnpm-lock.yaml")
    m = re.search(r"^packages:\n(.*?)(?=^\S|\Z)", text, re.M | re.S)
    return set(re.findall(r"^  '?(/?[^\s:']+)'?:$", m.group(1), re.M)) if m else set()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("old")
    ap.add_argument("new")
    a = ap.parse_args()
    old, new = cargo(a.old), cargo(a.new)
    added = sorted(n for n in new if n not in old)
    removed = sorted(n for n in old if n not in new)
    changed = sorted(n for n in new if n in old and new[n] != old[n])
    print(f"## Cargo.lock ({a.old[:12]} -> {a.new[:12]})")
    print(
        f"third-party crates: {len(old)} -> {len(new)}; added {len(added)}, removed {len(removed)}, "
        f"version/source changed {len(changed)}"
    )
    old_src = {s for v in old.values() for _, s in v}
    new_src = sorted(
        {s.split("#")[0] for v in new.values() for _, s in v if s != REGISTRY}
        - {s.split("#")[0] for s in old_src}
    )
    print("new non-registry sources:")
    for s in new_src:
        users = sorted(
            n for n, v in new.items() if any(x.split("#")[0] == s for _, x in v)
        )
        print(f"  {s}  ({', '.join(users)})")
    print("added crates:")
    for n in added:
        print(f"  {n} {', '.join(sorted(v for v, _ in new[n]))}")
    print("changed crates:")
    for n in changed:
        o = ", ".join(
            sorted(
                v + ("" if s == REGISTRY else " " + s.split("?")[0]) for v, s in old[n]
            )
        )
        w = ", ".join(
            sorted(
                v + ("" if s == REGISTRY else " " + s.split("?")[0]) for v, s in new[n]
            )
        )
        print(f"  {n}: {o} -> {w}")
    print("removed crates:")
    for n in removed:
        print(f"  {n}")
    (old_deps, old_urls, old_remotes), (new_deps, new_urls, new_remotes) = (
        bazel(a.old),
        bazel(a.new),
    )
    print("## MODULE.bazel")
    for n in sorted(set(old_deps) | set(new_deps)):
        if old_deps.get(n) != new_deps.get(n):
            print(f"  bazel_dep {n}: {old_deps.get(n, '-')} -> {new_deps.get(n, '-')}")
    for u in sorted(new_urls - old_urls):
        print(f"  new URL {u}")
    for u in sorted(new_remotes - old_remotes):
        print(f"  new git remote {u}")
    op, np_ = pnpm(a.old), pnpm(a.new)
    print(
        f"## pnpm-lock.yaml: packages {len(op)} -> {len(np_)}; added {len(np_ - op)}, removed {len(op - np_)}"
    )
    for p in sorted(np_ - op):
        print(f"  + {p}")
    for p in sorted(op - np_):
        print(f"  - {p}")


if __name__ == "__main__":
    main()
