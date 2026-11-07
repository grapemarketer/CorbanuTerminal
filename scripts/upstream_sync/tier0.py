#!/usr/bin/env python3
"""Prepare, save, apply and check Tier 0 resolutions (manifests, lockfiles, CI files) for the skeleton merge.

  tier0.py prep  <tag> <dir> [--ref origin/main]  start a merge in a new worktree <repo>/../tier0-<tag> and record
                                                  each Tier 0 conflicted path with its base/ours/theirs blobs
  tier0.py save  <worktree> <dir>                 copy the hand-resolved Tier 0 files into <dir>/files
                                                  (a path removed from the worktree is recorded as a deletion)
  tier0.py apply <worktree> <tag> <dir>           during the skeleton merge: use each prepared file if both sides'
                                                  blobs still match, else stop and name the file
  tier0.py extra <worktree> <dir> <path>...      record non-conflicted Tier 0 files the resolution also changes
                                                  (e.g. a workflow that must match a resolved caller)
  tier0.py check <worktree>                       no Tier 0 path unmerged or with markers; during the merge each keeps
                                                  the three-way merged file mode; workflows/actions parse;
                                                  actionlint reports nothing that HEAD's own files don't

Optional <dir>/resolutions.tsv (path, resolution, note) labels ledger rows (ours|theirs|merged|deleted); unlisted
prepared files are labelled merged. Lockfiles (codex-rs/Cargo.lock, MODULE.bazel.lock) are never copied: apply takes our side and skeleton_merge.sh
regenerates them (cargo metadata + lock_max.py; bazel mod deps).
"""

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import conflict_stats  # noqa: E402
import slices  # noqa: E402

LOCKS = ("codex-rs/Cargo.lock", "MODULE.bazel.lock")
MARKER = re.compile(rb"^(<<<<<<<|\|\|\|\|\|\|\||=======|>>>>>>>)( |$)", re.M)


def git(wt, *args, check=True):
    return subprocess.run(
        ["git", "-C", wt, *args], capture_output=True, text=True, check=check
    ).stdout


def is_tier0(p):
    return slices.slice_of(p) == "S0 manifests/locks/CI"


def stage_blobs(wt, p):
    out = {"1": "", "2": "", "3": ""}
    for line in git(wt, "ls-files", "-u", "--", p).splitlines():
        meta, _ = line.split("\t", 1)
        _, blob, stage = meta.split()
        out[stage] = blob
    return out


def stage_modes(wt, p):
    out = {"1": "", "2": "", "3": ""}
    for line in git(wt, "ls-files", "-u", "--", p).splitlines():
        meta, _ = line.split("\t", 1)
        mode, _, stage = meta.split()
        out[stage] = mode
    return out


def tree_mode(wt, rev, p):
    line = git(wt, "ls-tree", rev, "--", p, check=False).strip()
    return line.split()[0] if line else ""


def merged_mode(base, ours, theirs):
    """Three-way merge of a file mode: take theirs only when ours didn't change it."""
    if not ours:
        return theirs
    return theirs if theirs and base == ours else ours


def copy_prepared(a, wt, p, mode):
    """Copy a prepared file and give it the mode git recorded (copy2 keeps the saved copy's mode, not git's)."""
    dst = os.path.join(wt, p)
    shutil.copy2(os.path.join(a.dir, "files", p), dst)
    if mode in ("100755", "100644"):
        os.chmod(dst, 0o755 if mode == "100755" else 0o644)
    git(wt, "add", "--", p)


def cmd_prep(a):
    here = os.path.dirname(os.path.abspath(__file__))
    tag_sha = subprocess.run(
        [os.path.join(here, "verify_target.sh"), a.tag],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    repo = git(here, "rev-parse", "--show-toplevel").strip()
    wt = os.path.join(os.path.dirname(repo), f"tier0-{a.tag}")
    ref = git(repo, "rev-parse", f"{a.ref}^{{commit}}").strip()
    git(repo, "worktree", "add", "--quiet", "--detach", wt, ref)
    subprocess.run(
        [
            "git",
            "-C",
            wt,
            "-c",
            "merge.conflictStyle=zdiff3",
            "-c",
            "merge.renameLimit=0",
            "merge",
            "--no-ff",
            "--no-commit",
            tag_sha,
        ],
        capture_output=True,
    )
    if (
        subprocess.run(
            ["git", "-C", wt, "rev-parse", "-q", "--verify", "MERGE_HEAD"],
            capture_output=True,
        ).returncode
        != 0
    ):
        sys.exit(f"error: the merge did not start in {wt}")
    os.makedirs(a.dir, exist_ok=True)
    rows = [r for r in conflict_stats.collect(wt) if is_tier0(r["path"])]
    with open(os.path.join(a.dir, "manifest.tsv"), "w") as f:
        f.write(
            f"# tier0 {a.tag} {tag_sha} ours {ref}\npath\tcode\tbase\tours\ttheirs\taction\n"
        )
        for r in rows:
            b = stage_blobs(wt, r["path"])
            f.write(
                f"{r['path']}\t{r['code']}\t{b['1']}\t{b['2']}\t{b['3']}\tpending\n"
            )
    print(
        f"worktree {wt}: {len(rows)} Tier 0 paths to resolve; then run: tier0.py save {wt} {a.dir}"
    )


def read_resolutions(d):
    out = {}
    path = os.path.join(d, "resolutions.tsv")
    if os.path.exists(path):
        for line in open(path):
            if line.strip() and not line.startswith("#"):
                cols = line.rstrip("\n").split("\t") + ["", ""]
                out[cols[0]] = (cols[1], cols[2])
    return out


def read_manifest(d):
    rows = []
    with open(os.path.join(d, "manifest.tsv")) as f:
        header = None
        for line in f:
            if line.startswith("#"):
                continue
            cols = line.rstrip("\n").split("\t")
            if header is None:
                header = cols
                continue
            rows.append(dict(zip(header, cols)))
    return rows


def write_manifest(d, rows):
    path = os.path.join(d, "manifest.tsv")
    first = open(path).readline()
    with open(path, "w") as f:
        f.write(first + "path\tcode\tbase\tours\ttheirs\taction\n")
        for r in rows:
            f.write(
                "\t".join(
                    r[c] for c in ("path", "code", "base", "ours", "theirs", "action")
                )
                + "\n"
            )


def cmd_save(a):
    rows, bad = read_manifest(a.dir), []
    for r in rows:
        src = os.path.join(a.worktree, r["path"])
        dst = os.path.join(a.dir, "files", r["path"])
        if not os.path.exists(src):
            r["action"] = "delete"
            continue
        if r["path"] in LOCKS:
            r["action"] = "file"  # regenerated by skeleton_merge.sh; contents not used
            continue
        data = open(src, "rb").read()
        if MARKER.search(data):
            bad.append(r["path"])
            continue
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        shutil.copy2(src, dst)
        r["action"] = "file"
    write_manifest(a.dir, rows)
    if bad:
        sys.exit("still has conflict markers:\n  " + "\n  ".join(bad))
    print(
        f"saved {sum(r['action'] == 'file' for r in rows)} files, {sum(r['action'] == 'delete' for r in rows)} "
        f"deletions"
    )


def cmd_apply(a):
    prepared = {r["path"]: r for r in read_manifest(a.dir)}
    problems = []
    current = [
        r["path"] for r in conflict_stats.collect(a.worktree) if is_tier0(r["path"])
    ]
    for r in prepared.values():
        if r["code"] != "extra":
            continue
        p = r["path"]
        head = git(
            a.worktree, "rev-parse", "--verify", "--quiet", f"HEAD:{p}", check=False
        ).strip()
        tag = git(
            a.worktree,
            "rev-parse",
            "--verify",
            "--quiet",
            f"MERGE_HEAD:{p}",
            check=False,
        ).strip()
        if head != r["ours"] or tag != r["theirs"]:
            problems.append(f"{p}: changed since it was prepared (extra file)")
        elif r["action"] == "delete":
            git(a.worktree, "rm", "--quiet", "--", p)
        else:
            staged = git(a.worktree, "ls-files", "-s", "--", p).split()
            copy_prepared(a, a.worktree, p, staged[0] if staged else "")
    # Lockfiles always start from our side, whether or not they conflicted.
    for p in LOCKS:
        if (
            subprocess.run(
                ["git", "-C", a.worktree, "cat-file", "-e", f"HEAD:{p}"]
            ).returncode
            == 0
        ):
            git(a.worktree, "checkout", "HEAD", "--", p)
    for p in current:
        if p in LOCKS:
            continue  # reset to our side above; regenerated by skeleton_merge.sh
        r = prepared.get(p)
        if r is None or r["action"] == "pending":
            problems.append(f"{p}: no prepared resolution")
            continue
        b = stage_blobs(a.worktree, p)
        m = stage_modes(a.worktree, p)
        if b["3"] != r["theirs"] or b["2"] != r["ours"]:
            problems.append(
                f"{p}: changed since it was prepared (ours {r['ours'][:10]} -> {b['2'][:10]})"
            )
            continue
        if r["action"] == "delete":
            git(a.worktree, "rm", "--quiet", "--", p)
        else:
            copy_prepared(a, a.worktree, p, merged_mode(m["1"], m["2"], m["3"]))
    for p in sorted(
        p for p, r in prepared.items() if r["code"] != "extra" and p not in current
    ):
        print(f"note: {p} no longer conflicts; prepared resolution not used")
    if problems:
        sys.exit(
            "Tier 0 resolution needed (resolve, then rerun tier0.py save and the skeleton):\n  "
            + "\n  ".join(problems)
        )
    print(f"applied {len(current)} Tier 0 resolutions")


def cmd_extra(a):
    rows = read_manifest(a.dir)
    have = {r["path"] for r in rows}
    for p in a.paths:
        if p in have:
            continue
        head = git(
            a.worktree, "rev-parse", "--verify", "--quiet", f"HEAD:{p}", check=False
        ).strip()
        tag = git(
            a.worktree,
            "rev-parse",
            "--verify",
            "--quiet",
            f"MERGE_HEAD:{p}",
            check=False,
        ).strip()
        rows.append(
            {
                "path": p,
                "code": "extra",
                "base": "",
                "ours": head,
                "theirs": tag,
                "action": "pending",
            }
        )
    write_manifest(a.dir, rows)
    print(f"{len(a.paths)} extra paths recorded; run tier0.py save after editing them")


def actionlint_messages(root, files):
    r = subprocess.run(
        ["actionlint", "-shellcheck=", "-pyflakes=", "-oneline", *files],
        cwd=root,
        capture_output=True,
        text=True,
    )
    return {
        re.sub(r":\d+:\d+:", ":", line)
        for line in r.stdout.splitlines()
        if line.strip()
    }


def cmd_check(a):
    wt, problems = a.worktree, []
    base = git(wt, "merge-base", "HEAD", "MERGE_HEAD", check=False).strip()
    unmerged = {
        line.split("\t", 1)[1] for line in git(wt, "ls-files", "-u").splitlines()
    }
    for p in git(wt, "ls-files").splitlines():
        if not is_tier0(p):
            continue
        if p in unmerged:
            problems.append(f"unmerged: {p}")
        full = os.path.join(wt, p)
        if os.path.isfile(full) and MARKER.search(open(full, "rb").read()):
            problems.append(f"markers: {p}")
        # A resolved file keeps the three-way merged mode (e.g. an executable CI script stays executable).
        staged = git(wt, "ls-files", "-s", "--", p).split()
        if base and staged:
            want = merged_mode(
                tree_mode(wt, base, p),
                tree_mode(wt, "HEAD", p),
                tree_mode(wt, "MERGE_HEAD", p),
            )
            if want and staged[0] != want:
                problems.append(f"mode: {p} is {staged[0]}, the merged mode is {want}")
    try:
        import yaml
    except ImportError:
        yaml = None
    ymls = [
        p
        for p in git(
            wt, "ls-files", ".github/workflows", ".github/actions"
        ).splitlines()
        if p.endswith((".yml", ".yaml"))
    ]
    if yaml:
        for p in ymls:
            try:
                yaml.safe_load(open(os.path.join(wt, p)))
            except Exception as e:  # noqa: BLE001
                problems.append(f"yaml: {p}: {e}")
    else:
        print("warning: PyYAML missing; YAML parse check skipped", file=sys.stderr)
    if shutil.which("actionlint"):
        wf = [p for p in ymls if p.startswith(".github/workflows/")]
        with tempfile.TemporaryDirectory() as head_dir:
            subprocess.run(
                f"git -C '{wt}' archive HEAD .github | tar -x -C '{head_dir}'",
                shell=True,
                check=True,
            )
            head_wf = [
                p
                for p in os.listdir(os.path.join(head_dir, ".github/workflows"))
                if p.endswith(".yml")
            ]
            known = actionlint_messages(
                head_dir, [f".github/workflows/{p}" for p in head_wf]
            )
        new = sorted(actionlint_messages(wt, wf) - known)
        if new:
            problems.append(
                "actionlint (not present on HEAD):\n    " + "\n    ".join(new)
            )
    if problems:
        sys.exit("Tier 0 check failed:\n  " + "\n  ".join(problems))
    print(
        f"Tier 0 check passed ({len(ymls)} workflow/action files; YAML {'parsed' if yaml else 'not parsed'}, "
        f"actionlint {'clean against HEAD' if shutil.which('actionlint') else 'not installed'})"
    )


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("prep")
    p.add_argument("tag")
    p.add_argument("dir")
    p.add_argument("--ref", default="origin/main")
    p = sub.add_parser("save")
    p.add_argument("worktree")
    p.add_argument("dir")
    p = sub.add_parser("apply")
    p.add_argument("worktree")
    p.add_argument("tag")
    p.add_argument("dir")
    p = sub.add_parser("extra")
    p.add_argument("worktree")
    p.add_argument("dir")
    p.add_argument("paths", nargs="+")
    p = sub.add_parser("check")
    p.add_argument("worktree")
    a = ap.parse_args()
    a.dir = os.path.abspath(a.dir) if getattr(a, "dir", None) else None
    {
        "prep": cmd_prep,
        "save": cmd_save,
        "apply": cmd_apply,
        "extra": cmd_extra,
        "check": cmd_check,
    }[a.cmd](a)


if __name__ == "__main__":
    main()
