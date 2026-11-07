#!/bin/bash
# pf84-cb1 (D137): one fresh Tart clone per group: up, seal, tunnel, stage, gate, group, down. One VM at a time.
# usage: run-tart.sh [GROUP...]   (default: every group in groups.txt, in order; groups.txt lines: GROUP HOME...)
Q=/Volumes/CorbanuDrive/Corbanu/.codex-work/pf84-cb1-20261021
cd $Q
LIST="${*:-$(awk '!/^#/ && NF {print $1}' groups.txt)}"
lanes=(agmx1 agmx2 agmx3); i=${LANE_OFFSET:-0}
/Applications/UTM.app/Contents/MacOS/utmctl list | grep -q 'started' && { echo "$(date -u +%FT%TZ) a UTM VM is running; not starting Tart" >> runs/seq.log; exit 1; }
for G in $LIST; do
  export GROUP_HOMES="$(awk -v g="$G" '$1 == g {$1=""; sub(/^ /, ""); print}' groups.txt)"
  [ -n "$GROUP_HOMES" ] || { echo "$(date -u +%FT%TZ) no homes for $G" >> runs/seq.log; exit 1; }
  LANE=${lanes[$((i % 3))]}; i=$((i + 1))
  [ ! -e "$Q/STOP" ] || { echo "$(date -u +%FT%TZ) STOP file present; stopping before $G" >> runs/seq.log; exit 0; }
  while [ -e /Volumes/CorbanuDrive/Corbanu/.codex-work/workers-20261002/vm-tart.lock ] && ! grep -q '^holder: pf84-cb1$' /Volumes/CorbanuDrive/Corbanu/.codex-work/workers-20261002/vm-tart.lock; do
    echo "$(date -u +%FT%TZ) waiting for the Tart lock ($(head -1 /Volumes/CorbanuDrive/Corbanu/.codex-work/workers-20261002/vm-tart.lock))" >> runs/seq.log; sleep 120; done
  export PAR_COMMON=common/$G-$(date -u +%Y%m%dT%H%M%SZ)
  while :; do f=$(memory_pressure -Q | awk '/free percentage/ {gsub("%","",$NF); print $NF}'); [ "${f:-0}" -ge 25 ] && break; echo "$(date -u +%FT%TZ) paused: memory free $f%" >> runs/seq.log; sleep 60; done
  echo "$(date -u +%FT%TZ) group $G on $LANE (homes: $GROUP_HOMES; memory free $f%)" >> runs/seq.log
  ./ht/tart-run.sh up > /dev/null 2>&1 || { echo "$(date -u +%FT%TZ) up failed for $G" >> runs/seq.log; ./ht/tart-run.sh down > /dev/null 2>&1; exit 1; }
  ./ht/tart-run.sh seal > /dev/null 2>&1
  ./ht/tart-run.sh tunnel > /dev/null 2>&1
  ./ht/tart-run.sh stage > /dev/null 2>&1 || { echo "$(date -u +%FT%TZ) stage failed for $G" >> runs/seq.log; ./ht/tart-run.sh down > /dev/null 2>&1; exit 1; }
  ./ht/tart-run.sh gate > $Q/$PAR_COMMON/gate-out.txt 2>&1
  if python3 $Q/h/gate_ok.py $Q/$PAR_COMMON >> runs/seq.log 2>&1; then
    echo "$(date -u +%FT%TZ) gate ok for $G ($PAR_COMMON)" >> runs/seq.log
    ./ht/tart-run.sh group $LANE $G
  else
    echo "$(date -u +%FT%TZ) gate FAILED for $G; group blocked" >> runs/seq.log
  fi
  ./ht/tart-run.sh down > /dev/null 2>&1
  echo "$(date -u +%FT%TZ) group $G clone deleted (done)" >> runs/seq.log
done
/opt/homebrew/bin/tmux -S "$Q/tmux.sock" kill-session -t passthrough 2> /dev/null && echo "$(date -u +%FT%TZ) host pass-through stopped" >> runs/seq.log
echo "$(date -u +%FT%TZ) ALL-DONE $LIST" >> runs/seq.log
