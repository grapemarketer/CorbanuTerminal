#!/bin/bash
W=/Volumes/CorbanuDrive/Corbanu/.codex-work/acct-rerun-20261022; cd $W
for G in A-R1 C-R1; do
  export CLONE=$G-$(date -u +%Y%m%dT%H%MZ); echo $CLONE > current-clone
  echo "$(date -u +%FT%TZ) start $CLONE" >> logs/resume-driver.log
  bash h/tart-run.sh up && bash h/tart-run.sh seal && bash h/tart-run.sh tunnel && bash h/tart-run.sh stage \
    && bash h/tart-run.sh gate && python3 h/gate_ok.py common/$CLONE \
    && bash h/tart-run.sh group agmx1 $G
  echo "$(date -u +%FT%TZ) $CLONE chain rc=$?" >> logs/resume-driver.log
  LOCK_KEEP=$([ $G = A-R1 ] && echo 1 || echo 0) bash h/tart-run.sh down
  echo "$(date -u +%FT%TZ) $CLONE down rc=$?" >> logs/resume-driver.log
done
echo "$(date -u +%FT%TZ) DONE" >> logs/resume-driver.log
