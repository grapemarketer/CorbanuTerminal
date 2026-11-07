#!/bin/bash
# Copy the re-run R1 evidence into the record folder (no product-home, no collected.tar; large files gzipped).
set -euo pipefail
# Note (acct-rerun2, review round 2): after assembly, every copied *.log file was renamed *.log.txt (the repository
# .gitignore excludes *.log).
W=/Volumes/CorbanuDrive/Corbanu/.codex-work/acct-rerun-20261022
D=/Volumes/CorbanuDrive/Corbanu/worktrees/acct-rerun/qa/portfolio/agent-cost-accounting/followups-acceptance-20261016/rerun-r1-20261022
mkdir -p "$D/candidate" "$D/harness" "$D/common" "$D/runs" "$D/logs"
cp "$W"/candidate/{build-head.txt,codesign.txt,developer-marker-count.txt,package-files.txt,packet-build.txt,packet-files.txt} "$D/candidate/"
for f in "$W"/h/*; do [ -f "$f" ] || continue; case "$(basename "$f")" in fixture.tar) ;; *) cp "$f" "$D/harness/";; esac; done
cp "$W/logs/ca-reissue-20261027.md" "$W/logs/resume-driver.sh" "$W/logs/resume-driver-c2.sh" "$W/tools/assemble_r1.sh" "$D/harness/"
cp "$W"/logs/{host-relay-zai.jsonl,host-relay-openai.jsonl,host-shakedown-mediator.jsonl,zai-balance-poll.txt,resume-driver.log} "$D/logs/"
cp "$W/runs/seq.log" "$D/logs/seq.log"
for c in "$W"/common/*/; do
  n=$(basename "$c"); mkdir -p "$D/common/$n"
  (cd "$c" && find . -type f ! -name 'vm-ip.txt' | while read -r f; do mkdir -p "$D/common/$n/$(dirname "$f")"; cp "$f" "$D/common/$n/$f"; done)
  [ ! -f "$D/common/$n/mediator-final.jsonl" ] || gzip -9f "$D/common/$n/mediator-final.jsonl"
done
for r in "$W"/runs/acc-*/; do
  n=$(basename "$r"); o="$D/runs/$n"; mkdir -p "$o/evidence"
  (cd "$r" && for f in *; do [ -f "$f" ] || continue; case "$f" in collected.tar) ;; *) cp "$f" "$o/";; esac; done)
  for f in executor-events.jsonl mediator-at-end.jsonl; do [ ! -f "$o/$f" ] || gzip -9f "$o/$f"; done
  [ -d "$r/x/evidence" ] && cp "$r"/x/evidence/* "$o/evidence/"
  for f in pty.raw screens.jsonl actions.jsonl; do [ ! -f "$o/evidence/$f" ] || gzip -9f "$o/evidence/$f"; done
  [ -d "$r/x/product-home" ] && (cd "$r/x/product-home" && find . -type f | sort) > "$o/product-home.manifest.txt" || true
done
du -sh "$D"
