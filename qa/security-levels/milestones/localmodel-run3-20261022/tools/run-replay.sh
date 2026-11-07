#!/bin/bash
# qa-local3: replay of group L02 (tester setup slip in the first L02 run) on a fresh clone; evidence folder common/L02R.
Q=/Volumes/CorbanuDrive/Corbanu/.codex-work/qa-local3-20261022
cd $Q; export QWEN_COMMON=common/L02R; G=L02; LANE=agmx2
while :; do f=$(memory_pressure -Q | awk '/free percentage/ {gsub("%","",$NF); print $NF}'); [ "${f:-0}" -ge 20 ] && break; echo "$(date -u +%FT%TZ) paused: memory free $f%" >> runs/seq.log; sleep 60; done
echo "$(date -u +%FT%TZ) group $G replay on $LANE (clone)" >> runs/seq.log
./h/tart-run.sh up > /dev/null 2>&1 || { echo "$(date -u +%FT%TZ) up failed for $G replay" >> runs/seq.log; ./h/tart-run.sh down > /dev/null 2>&1; exit 1; }
./h/tart-run.sh seal > /dev/null 2>&1
./h/tart-run.sh tunnel > /dev/null 2>&1
./h/tart-run.sh stage > /dev/null 2>&1 || { echo "$(date -u +%FT%TZ) stage failed for $G replay" >> runs/seq.log; ./h/tart-run.sh down > /dev/null 2>&1; exit 1; }
./h/tart-run.sh gate > $QWEN_COMMON/gate-out.txt 2>&1
./h/tart-run.sh group $LANE $G replay
./h/tart-run.sh down > /dev/null 2>&1
echo "$(date -u +%FT%TZ) group $G replay clone deleted" >> runs/seq.log
echo "$(date -u +%FT%TZ) REPLAY-DONE" >> runs/seq.log
