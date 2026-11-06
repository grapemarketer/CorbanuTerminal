#!/usr/bin/env python3
"""Download and verify upstream's prebuilt rusty_v8 artifacts for one target (SG1; standard library only).

Usage: verify_rusty_v8.py --trusted MANIFESTS.sha256 --version 152.2.0 --target TRIPLE [--dest DIR]

MANIFESTS.sha256 is the in-repo trust root at the target tag
(`git show <tag>:third_party/v8/rusty_v8_<ver>_release_manifests.sha256`). The script mirrors
.github/actions/setup-rusty-v8: the per-target manifest from the release must match the trusted hash, it must list
exactly two files, and both downloaded files must match it. Prints one PASS/FAIL line per check.
"""

import argparse
import hashlib
import os
import sys
import tempfile
import urllib.request

BASE = "https://github.com/openai/codex/releases/download/rusty-v8-v{version}/{name}"


def fetch(url, path):
    with urllib.request.urlopen(url, timeout=600) as r, open(path, "wb") as f:
        while chunk := r.read(1 << 20):
            f.write(chunk)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(1 << 20):
            h.update(chunk)
    return h.hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--trusted", required=True)
    ap.add_argument("--version", required=True)
    ap.add_argument("--target", required=True)
    ap.add_argument("--profile", default="ptrcomp_sandbox_release")
    ap.add_argument("--dest")
    a = ap.parse_args()
    dest = a.dest or tempfile.mkdtemp(prefix="rusty_v8_")
    os.makedirs(dest, exist_ok=True)
    manifest = f"rusty_v8_{a.profile}_{a.target}.sha256"
    trusted = {}
    for line in open(a.trusted, encoding="utf-8"):
        parts = line.split()
        if len(parts) == 2:
            trusted[parts[1]] = parts[0]
    ok = True

    def report(cond, what):
        nonlocal ok
        ok &= cond
        print(f"{'PASS' if cond else 'FAIL'} {a.target} {what}")

    if manifest not in trusted:
        report(False, f"{manifest} not in trusted manifests")
        sys.exit(1)
    mpath = os.path.join(dest, manifest)
    fetch(BASE.format(version=a.version, name=manifest), mpath)
    report(sha256(mpath) == trusted[manifest], f"manifest {manifest} sha256 {sha256(mpath)[:16]}... matches trust root")
    entries = [line.split() for line in open(mpath, encoding="utf-8").read().replace("\r", "").splitlines()
               if line.strip()]
    report(len(entries) == 2, f"manifest lists {len(entries)} files (expect 2)")
    for digest, name in entries:
        name = name.lstrip("*")
        path = os.path.join(dest, name)
        fetch(BASE.format(version=a.version, name=name), path)
        got = sha256(path)
        report(got == digest, f"{name} ({os.path.getsize(path)} bytes) sha256 {got[:16]}...")
    print(f"{'ALL PASS' if ok else 'FAILED'} {a.target} files in {dest}")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
