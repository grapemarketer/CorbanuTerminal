#!/bin/bash
# qa-local3 (from qwen-rerun2): one fresh Tart clone per group: up, seal, tunnel, stage, gate, group, down. One VM at a time.
Q=/Volumes/CorbanuDrive/Corbanu/.codex-work/qa-local3-20261022
cd $Q
for spec in ${GROUP_SPECS:-"L01 agmx1" "L02 agmx2" "L03 agmx3" "L04 agmx1"}; do
  set -- $spec; G=$1; LANE=$2
  export QWEN_COMMON=common/$G
  while :; do f=$(memory_pressure -Q | awk '/free percentage/ {gsub("%","",$NF); print $NF}'); [ "${f:-0}" -ge 20 ] && break; echo "$(date -u +%FT%TZ) paused: memory free $f%" >> runs/seq.log; sleep 60; done
  echo "$(date -u +%FT%TZ) group $G on $LANE (clone)" >> runs/seq.log
  ./h/tart-run.sh up > /dev/null 2>&1 || { echo "$(date -u +%FT%TZ) up failed for $G" >> runs/seq.log; ./h/tart-run.sh down > /dev/null 2>&1; exit 1; }
  ./h/tart-run.sh seal > /dev/null 2>&1
  ./h/tart-run.sh tunnel > /dev/null 2>&1
  ./h/tart-run.sh stage > /dev/null 2>&1 || { echo "$(date -u +%FT%TZ) stage failed for $G" >> runs/seq.log; ./h/tart-run.sh down > /dev/null 2>&1; exit 1; }
  ./h/tart-run.sh gate > $QWEN_COMMON/gate-out.txt 2>&1
  ./h/tart-run.sh group $LANE $G
  ./h/tart-run.sh down > /dev/null 2>&1
  echo "$(date -u +%FT%TZ) group $G clone deleted" >> runs/seq.log
done
echo "$(date -u +%FT%TZ) ALL-DONE" >> runs/seq.log
