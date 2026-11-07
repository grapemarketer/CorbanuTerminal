#!/bin/bash
# pf84-cb1 integrator probe (not a case; not code-blind tester evidence): separate "the launch --account wins" from
# "silent fallback" for in-session /resume of a conversation whose recorded account was removed (NA-05). One clone.
set -u
Q=/Volumes/CorbanuDrive/Corbanu/.codex-work/pf84-cb1-20261021
cd $Q
export PAR_COMMON=common/probe-na05B-$(date -u +%Y%m%dT%H%M%SZ)
O=$Q/$PAR_COMMON/probe; L=agmx1; R=probe-na05b-$(date -u +%Y%m%dT%H%M%SZ)
./ht/tart-run.sh up > /dev/null 2>&1 || { echo up failed; ./ht/tart-run.sh down; exit 1; }
./ht/tart-run.sh seal > /dev/null 2>&1; ./ht/tart-run.sh tunnel > /dev/null 2>&1
./ht/tart-run.sh stage > /dev/null 2>&1 || { echo stage failed; ./ht/tart-run.sh down; exit 1; }
./ht/tart-run.sh gate > $Q/$PAR_COMMON/gate-out.txt 2>&1; python3 h/gate_ok.py $Q/$PAR_COMMON
mkdir -p $O
export TART_QA_WORK=/Volumes/CorbanuDrive/Corbanu/.codex-work/tart-eval
. /Volumes/CorbanuDrive/Corbanu/worktrees/pf84-cb1/scripts/qa/tart/lib.sh
ag() { ssh -n "${SSH_OPTS[@]}" "agent@$(cat $Q/$PAR_COMMON/vm-ip.txt)" "$@"; }
asl() { ag "cd /tmp; sudo -n -u $L -H /bin/bash -c 'export AGMS_RUN=\$HOME/runs/$R AGMS_PROXY=http://127.0.0.1:18443 PATH=/opt/agms/bin:/usr/bin:/bin; cd \$AGMS_RUN/work; $1'"; }
T() { asl "agms-tui $*"; }
ag "cd /tmp; sudo -n -u $L -H /opt/agms/bin/agms-launch probe $R" > $O/probe.txt 2>&1
ag "cd /tmp; sudo -n -u $L -H /opt/agms/bin/agms-launch prep $R x zai3" > $O/prep.txt 2>&1
sed "s/@RUN@/$R/" $Q/ht/probe-na05-guest-armB.sh | ssh "${SSH_OPTS[@]}" "agent@$(cat $Q/$PAR_COMMON/vm-ip.txt)" "cd /tmp; sudo -n -u $L -H /bin/bash -s"
  > $O/probe-transcript.txt 2>&1
ag "cd /tmp; sudo -n -u $L -H /opt/agms/bin/agms-launch collect $R" > $O/collected.tar 2>/dev/null
ag 'cat ~/agms/mediator/mediator.jsonl' > $O/mediator.jsonl 2>&1
./ht/tart-run.sh down > $O/down.txt 2>&1
python3 ht/relay-table.py $O/mediator.jsonl | grep -v -E 'github|qwen|1860|denied|chatgpt' > $O/relay.txt
echo probe done
