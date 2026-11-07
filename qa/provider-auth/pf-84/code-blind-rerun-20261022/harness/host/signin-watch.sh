#!/bin/bash
# Stop signin.sh after attempt 1 if it expires: a fresh code may be posted only after 16:30Z (owner instruction).
Q=<Q>
HAND=/Volumes/CorbanuDrive/Corbanu/.codex-work/workers-20261002/pf84-rerun.signin.md
while :; do
  grep -q '^DONE' "$HAND" && { echo "$(date -u +%FT%TZ) watcher: DONE seen" >> $Q/runs/seq.log; exit 0; }
  if grep -q 'EXPIRED/FAILED' "$HAND"; then
    for p in $(pgrep -f "$Q/ht/signin.sh"); do kill "$p"; done
    echo "$(date -u +%FT%TZ) watcher: attempt 1 expired; signin.sh stopped (next code only after 16:30Z)" >> $Q/runs/seq.log
    exit 0
  fi
  sleep 1
done
