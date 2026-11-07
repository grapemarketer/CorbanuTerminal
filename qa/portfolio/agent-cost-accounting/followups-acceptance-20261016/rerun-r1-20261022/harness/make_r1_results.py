"""Write the R1 isolation receipts and update results.json for the five re-run cases.
usage: python3 make_r1_results.py RECORD_DIR   (RECORD_DIR = followups-acceptance-20261016)
"""
import gzip, hashlib, json, pathlib, sys

root = pathlib.Path(sys.argv[1])
R1 = "rerun-r1-20261022"
CAND = "e942b095de975f95ca3fa1f09b9576f6bb408d91499e05235e35ab37b9d6aa42"
sha = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
ref = lambda p: {"path": str(p.relative_to(root)), "sha256": sha(p)}
results_path = root / "results.json"
results = json.loads(results_path.read_text())
design_sha = results["design_sha256"]

RUNS = {
    "A-R1": ("acc-A-R1-agmx1-20261027T175348Z", "A-R1-20261027T1751Z", ["ACC-06"]),
    "C-R1": ("acc-C-R1-agmx1-20261027T190341Z", "C-R1-20261027T1901Z", ["ACC-11", "ACC-14", "ACC-15", "ACC-18"]),
}
SUMMARY = json.loads((pathlib.Path(__file__).parent / "r1-summaries.json").read_text())
GROUP_FIELD = {"source_denied": "repo", "history_denied": "history", "symlink_escape_denied": "symlink",
               "credentials_denied": "credentials", "cross_run_ipc_denied": "ipc", "network_restricted": "network",
               "package_readonly": "package", "children_confined": "child"}

cases = {c["id"]: c for c in results["cases"]}
for group, (run_id, clone, ids) in RUNS.items():
    run = root / R1 / "runs" / run_id
    ev = run / "evidence"
    thread = next(json.loads(l)["thread_id"] for l in gzip.open(run / "executor-events.jsonl.gz", "rt")
                  if l.startswith('{"type":"thread.started"'))
    probes = json.loads((ev / "preflight-executor.json").read_text())
    assert probes["ok"] is True
    by_group = {}
    for p in probes["probes"]:
        by_group.setdefault(p["group"], []).append(p["ok"])
    verdicts = (ev / "result.md").read_text()
    for cid in ids:
        iso = {
            "enforcement": "os-enforced", "agent": thread, "run_id": run_id, "case_id": cid, "group": group,
            "design_sha256": design_sha, "candidate_sha256": CAND,
            "policy": ref(root / R1 / "common" / clone / "policy-root-start.txt"),
            "tool_inventory": ref(root / R1 / "candidate" / "stage-package-files.txt"),
            "probe_evidence": ref(ev / "preflight-executor.json"),
            "gate": ref(run / "gate-check.txt"),
            "packet_readable": all(by_group["positive"]), "candidate_launchable": all(by_group["positive"]),
            "actual_input_available": True,
            "note": "Tart clone of corbanu-qa-base (one VM at a time); group run: the cases share one executor "
                    "session and one preflight; no real key in the guest (host relays)",
        }
        for field, g in GROUP_FIELD.items():
            iso[field] = all(by_group[g])
        p = run / f"isolation-{cid}.json"
        p.write_text(json.dumps(iso, indent=1) + "\n")
        raw = "passed" if f"VERDICT {cid}: passed" in verdicts else "other"
        old = cases[cid]
        prior = {k: v for k, v in old.items() if k not in ("id", "prior_run")}
        cases[cid] = {
            "id": cid, "priority": old["priority"], "disposition": SUMMARY[cid]["disposition"],
            "summary": SUMMARY[cid]["summary"], "candidate_sha256": CAND, "method": "tmux",
            "execution": {
                "agent": thread, "run_id": run_id,
                "model": "executor: candidate corbanu exec, glm-5.3-flash (zai via host relay, default effort); "
                         "product under test: the case's route/model",
                "machine": f"Tart clone {clone} of corbanu-qa-base (macOS arm64), executor account agmx1",
                "profile": "fresh per group: product-home/config.toml = harness/product-config.toml + trust entry",
                "launcher": "sudo -u agmx1 /opt/agms/bin/agms-launch executor RUN GROUP -> /opt/agms/pkg/bin/corbanu "
                            "exec; product via agms-tui -> agms-product -> /opt/agms/pkg/bin/corbanu",
                "fresh_context": True, "code_blind": True, "results_blind": True,
                "packet": ref(root / R1 / "executor-packet-r1" / "prompts" / f"{group}.md"),
                "amendment": ref(root / R1 / "design-r1" / "amendment-r1.md"),
                "access_record": ref(run / "executor-events.jsonl.gz"),
                "actions": ref(ev / "actions.jsonl.gz"),
                "isolation_record": ref(p),
                "raw_executor_verdict": raw,
            },
            "evidence": [ref(ev / n) for n in ("result.md", "screens.jsonl.gz", "fixture.jsonl", "pty.raw.gz")]
                        + [ref(run / n) for n in ("guard-after.txt", "executor-start.txt", "executor-end.txt")],
            "prior_run": prior,
        }
results["cases"] = [cases[c["id"]] for c in results["cases"]]
results["rerun_r1"] = {
    "scope": ["ACC-06", "ACC-11", "ACC-14", "ACC-15", "ACC-18"],
    "amendment": ref(root / R1 / "design-r1" / "amendment-r1.md"),
    "candidate": {"source": "origin/main 63c9daca2629fe216d3887c53399a6fb7a75a334 (postmerge-ci run 39751860243 "
                            "green), private signed developer build (Developer ID K5N2T25C23), used only in Tart guests",
                  "version": "0.1.48", "binary_sha256": CAND,
                  "build_head": ref(root / R1 / "candidate" / "build-head.txt"),
                  "codesign": ref(root / R1 / "candidate" / "codesign.txt")},
    "record": ref(root / R1 / "README.md"),
    "review_budget": {"limit": 3, "scope": "independent read-only Opus 5.5 High review of the record PR (acct-rerun brief); "
                      "designer and design review are separate sessions required by the brief",
                      "ledger": ref(root / "review-ledger.md")},
    "raw_vs_disposition": "executor: all five passed; integrator: ACC-11 and ACC-14 blocked (one result each NOT OBSERVED, G7/G11/G16)",
    "note": "Top-level candidate is the first run's build; the five re-run cases carry the R1 candidate hash, "
            "so check.py reports them as stale against the first-run candidate by design.",
}
results["review_budget"]["ledger"] = ref(root / "review-ledger.md")  # R1 section appended; first-run rows unchanged
dispo = root / "dispositions.md"
results["dispositions"] = ref(dispo)
results_path.write_text(json.dumps(results, indent=1, ensure_ascii=False) + "\n")
print({c["id"]: c["disposition"] for c in results["cases"] if c["id"] in SUMMARY})
from collections import Counter
print(Counter(c["disposition"] for c in results["cases"]))
