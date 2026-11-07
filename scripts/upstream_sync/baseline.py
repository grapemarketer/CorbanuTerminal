#!/usr/bin/env python3
"""Record the CI baseline for one main commit: every workflow run, every job conclusion and test counts.

Usage: baseline.py <sha> [--repo OWNER/NAME] [--out FILE.md]

For each workflow run on <sha> (latest attempt per workflow), lists job conclusions. For test jobs it reads the job
log and extracts nextest summaries ("N tests run: N passed (F flaky), S skipped") and cargo-test result lines, so S5
can compare pass counts lane by lane. Cancelled/superseded runs are listed as such; re-run or pick a SHA whose
lanes completed.
"""

import argparse
import json
import re
import subprocess

ANSI = re.compile(r"\x1b\[[0-9;]*m")
NEXTEST = re.compile(r"Summary\s+\[\s*([\d.]+)s\]\s+(\d+) tests? run: (\d+) passed[^\n]*")
BAZEL = re.compile(r"Executed (\d+) out of (\d+) tests?: ([^\n]*)")
CARGO = re.compile(r"test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored")
TEST_JOB = re.compile(r"test|nextest|shard|probe|canary|contract|smoke|matrix", re.I)


def gh(*args):
    return subprocess.run(["gh", *args], capture_output=True, text=True, check=True).stdout


def counts(repo, job_id):
    try:
        log = ANSI.sub("", gh("api", f"repos/{repo}/actions/jobs/{job_id}/logs"))
    except subprocess.CalledProcessError:
        return "log unavailable"
    m = list(NEXTEST.finditer(log))
    if m:
        line = m[-1].group(0)

        def n(word):
            x = re.search(rf"(\d+) {word}", line)
            return x.group(1) if x else "0"
        return (f"nextest {m[-1].group(2)} run, {m[-1].group(3)} passed, {n('flaky')} flaky, {n('failed')} failed, "
                f"{n('skipped')} skipped ({float(m[-1].group(1)):.0f}s)")
    b = BAZEL.findall(log)
    if b:
        return f"bazel executed {b[-1][0]} of {b[-1][1]} tests: {b[-1][2].strip()}"
    c = CARGO.findall(log)
    if c:
        p = sum(int(x[1]) for x in c)
        f = sum(int(x[2]) for x in c)
        i = sum(int(x[3]) for x in c)
        return f"cargo test {len(c)} binaries, {p} passed, {f} failed, {i} ignored"
    return ""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("sha")
    ap.add_argument("--repo", default="CorbanuCore/CorbanuTerminal")
    ap.add_argument("--out")
    a = ap.parse_args()
    runs = [json.loads(line) for line in gh("api", "--paginate",
                                            f"repos/{a.repo}/actions/runs?head_sha={a.sha}&per_page=100",
                                            "-q", ".workflow_runs[]").splitlines() if line.strip()]
    latest = {}
    for r in runs:
        if r["event"] not in ("push", "workflow_dispatch", "schedule"):
            continue
        k = r["name"]
        if k not in latest or r["run_attempt"] > latest[k]["run_attempt"] or r["id"] > latest[k]["id"]:
            latest[k] = r
    lines = [f"# CI baseline for {a.sha}", "",
             "| Workflow | Run | Status | Job | Conclusion | Tests |", "|---|---|---|---|---|---|"]
    for name in sorted(latest):
        r = latest[name]
        if name in ("cache-prune",) or name.startswith("Issue "):
            continue
        jobs = [json.loads(line) for line in gh("api", "--paginate",
                                                f"repos/{a.repo}/actions/runs/{r['id']}/jobs?per_page=100",
                                                "-q", ".jobs[]").splitlines() if line.strip()]
        status = r["conclusion"] or r["status"]
        lines.append(f"| {name} | {r['id']} | {status} | | | |")
        for j in sorted(jobs, key=lambda j: j["name"]):
            t = counts(a.repo, j["id"]) if TEST_JOB.search(j["name"]) and j["conclusion"] in ("success", "failure") \
                else ""
            lines.append(f"| | | | {j['name'].replace('|', '/')} | {j['conclusion'] or j['status']} | {t} |")
    text = "\n".join(lines) + "\n"
    if a.out:
        open(a.out, "w").write(text)
    print(text)


if __name__ == "__main__":
    main()
