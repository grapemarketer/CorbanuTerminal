#!/bin/bash
Q=<Q>
cd $Q; export PAR_COMMON=$(cat d136-common.txt)
L() { echo "$(date -u +%FT%TZ) $*" >> runs/seq.log; }
L "S1: re-stage after clearing the guest stage (old packet files had stayed)"
./ht/tart-run.sh stage > logs/stage3.out 2>&1 || { L "S1 stage failed"; exit 1; }
[ ! -e $Q/$PAR_COMMON/gate ] || mv $Q/$PAR_COMMON/gate $Q/$PAR_COMMON/gate.aborted-0838
./ht/tart-run.sh gate > $Q/$PAR_COMMON/gate-out.txt 2>&1
if python3 $Q/h/gate_ok.py $Q/$PAR_COMMON >> runs/seq.log 2>&1; then
  L "gate ok for S1"; ./ht/tart-run.sh group-continue S1
else L "gate FAILED for S1"; fi
L "S1 executor finished; VM left up for checks (down is separate)"
