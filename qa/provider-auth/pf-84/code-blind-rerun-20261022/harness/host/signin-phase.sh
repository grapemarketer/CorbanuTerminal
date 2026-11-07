#!/bin/bash
Q=<Q>
cd $Q
export PAR_COMMON=common/D136-$(date -u +%Y%m%dT%H%M%SZ)
echo "$PAR_COMMON" > $Q/d136-common.txt
echo "$(date -u +%FT%TZ) signin phase start ($PAR_COMMON)" >> runs/seq.log
./ht/tart-run.sh up > $Q/logs/up.out 2>&1 || { echo "$(date -u +%FT%TZ) up failed" >> runs/seq.log; exit 1; }
./ht/tart-run.sh seal > $Q/logs/seal.out 2>&1
./ht/tart-run.sh tunnel > $Q/logs/tunnel.out 2>&1
./ht/tart-run.sh stage > $Q/logs/stage.out 2>&1 || { echo "$(date -u +%FT%TZ) stage failed" >> runs/seq.log; exit 1; }
./ht/tart-run.sh signin agmx1 D136 > $Q/logs/signin.out 2>&1
echo "$(date -u +%FT%TZ) signin phase end rc=$?" >> runs/seq.log
