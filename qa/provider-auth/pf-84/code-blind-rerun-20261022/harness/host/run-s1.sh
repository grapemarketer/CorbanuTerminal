#!/bin/bash
Q=<Q>
cd $Q; export PAR_COMMON=$(cat d136-common.txt)
L() { echo "$(date -u +%FT%TZ) $*" >> runs/seq.log; }
L "S1: resume $PAR_COMMON"
./ht/tart-run.sh resume > logs/resume.out 2>&1 || { L "resume failed"; exit 1; }
./ht/tart-run.sh reseal > logs/reseal.out 2>&1
./ht/tart-run.sh tunnel > logs/tunnel2.out 2>&1
./ht/tart-run.sh stage > logs/stage2.out 2>&1 || { L "S1 stage failed"; exit 1; }
./ht/tart-run.sh gate > $Q/$PAR_COMMON/gate-out.txt 2>&1
if python3 $Q/h/gate_ok.py $Q/$PAR_COMMON >> runs/seq.log 2>&1; then
  L "gate ok for S1"; ./ht/tart-run.sh group-continue S1
else L "gate FAILED for S1"; fi
L "S1 executor finished; VM left up for checks (down is separate)"
