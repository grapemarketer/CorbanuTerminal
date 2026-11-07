#!/bin/bash
# Wait until the Tart lock has been free for 5 minutes in a row (utm-par3 releases it between its own groups for
# seconds only), no UTM VM is started and no Tart VM runs. Then exit 0. Logs to runs/seq.log.
Q=/Volumes/CorbanuDrive/Corbanu/.codex-work/pf84-cb1-20261021
LOCK=/Volumes/CorbanuDrive/Corbanu/.codex-work/workers-20261002/vm-tart.lock
free=0
while :; do
  if [ ! -e "$LOCK" ] && ! /Applications/UTM.app/Contents/MacOS/utmctl list 2>/dev/null | grep -q started \
     && ! pgrep -f '(^|/)tart run( |$)' > /dev/null; then
    free=$((free + 1))
  else
    free=0
  fi
  [ "$free" -ge 10 ] && { echo "$(date -u +%FT%TZ) Tart lock free for 5 min; proceeding" >> $Q/runs/seq.log; exit 0; }
  sleep 30
done
