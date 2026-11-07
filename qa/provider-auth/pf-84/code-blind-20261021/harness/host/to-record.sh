#!/bin/bash
# Copy one group's evidence into the record, redacted: device codes -> <CODE>, email hints -> <email-hint>, the
# guest run path -> <RUN>. Refuses if anything token-like (JWT "eyJ", "sk-", refresh-token prefixes) is found.
# usage: to-record.sh GROUP
set -euo pipefail
G="$1"
Q=/Volumes/CorbanuDrive/Corbanu/.codex-work/pf84-cb1-20261021
R=/Volumes/CorbanuDrive/Corbanu/worktrees/pf84-cb1/qa/provider-auth/pf-84/code-blind-20261021/runs/$G
d=$(ls -d $Q/runs/agms-$G-* | tail -1); c=$(ls -d $Q/common/$G-* | tail -1)
[ -d "$d/x" ] || { mkdir -p "$d/x"; tar -xf "$d/collected.tar" -C "$d/x"; }
mkdir -p "$R"
red() { python3 "$Q/ht/redact.py"; }
E="$d/x/evidence"
if [ -f "$E/result.md" ]; then red < "$E/result.md" > "$R/result.md"; elif [ -f "$d/x/work/evidence/result.md" ]; then { echo "<!-- integrator: the tester wrote this report to \$AGMS_RUN/work/evidence/result.md (its working directory) instead of \$AGMS_RUN/evidence/ -->"; red < "$d/x/work/evidence/result.md"; } > "$R/result.md"; else echo "(the tester wrote no result.md; see executor-final.txt)" > "$R/result.md"; fi
python3 -c 'import json,sys
ev=[json.loads(l) for l in open(sys.argv[1]) if l.startswith("{")]
for e in ev:
    it=e.get("item",{})
    if e.get("type")=="item.completed" and it.get("type")=="agent_message": print(it.get("text",""))
    if e.get("type") in ("error","turn.failed"): print(json.dumps(e))
' "$d/executor-events.jsonl" | red > "$R/executor-final.txt"
for f in actions.jsonl screens.jsonl prep.txt homes-after-prep.txt; do [ ! -f "$E/$f" ] || red < "$E/$f" > "$R/$f"; done
python3 "$Q/ht/relay-table.py" "$d/mediator-at-end.jsonl" | grep -v -E 'github|qwen|127\.0\.0\.1:1860|denied_path' > "$R/relay-product-requests.txt" || true
for f in homes-manifest.txt signin.txt signout.txt executor-start.txt executor-end.txt executor-exit.txt gate-check.txt \
         guard-before.txt guard-after.txt memory-before.txt memory-after.txt preflight-summary.stdout; do
  [ ! -f "$d/$f" ] || red < "$d/$f" > "$R/$f"
done
[ ! -d "$E/oai1-saved" ] || { mkdir -p "$R/oai1-saved"; for f in "$E"/oai1-saved/*.txt "$E"/oai1-saved/config.toml; do red < "$f" > "$R/oai1-saved/$(basename "$f")"; done; }
for f in erase-test-data.out guard-final.txt vm-state-end.txt; do [ ! -f "$c/$f" ] || red < "$c/$f" > "$R/clone-$f"; done
red < "$c/gate-out.txt" > "$R/clone-gate-out.txt" 2>/dev/null || true
if grep -r -l -E 'eyJ[A-Za-z0-9_-]{20,}|sk-[A-Za-z0-9_-]{16,}|rt_[A-Za-z0-9_-]{16,}' "$R"; then echo "TOKEN-LIKE TEXT FOUND; not recorded" >&2; exit 1; fi
du -sh "$R"
