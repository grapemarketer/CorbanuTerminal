#!/bin/bash
# pf84-cb1 harness shakedown on one fresh clone (never a case run; no tester): prepare every home, check account lists,
# one Z.AI exec per account kind (relay attribution), an OpenAI API exec (relay 401), the device-code screen reached
# through the pass-through (then cancelled; no code is posted or approved), then down.
set -u
Q=/Volumes/CorbanuDrive/Corbanu/.codex-work/pf84-cb1-20261021
cd $Q
export PAR_COMMON=common/shakedown-$(date -u +%Y%m%dT%H%M%SZ)
O=$Q/$PAR_COMMON/shakedown; L=agmx1; R=shake-$(date -u +%Y%m%dT%H%M%SZ)
./ht/tart-run.sh up > /dev/null 2>&1 || { echo up failed; ./ht/tart-run.sh down; exit 1; }
./ht/tart-run.sh seal > /dev/null 2>&1; ./ht/tart-run.sh tunnel > /dev/null 2>&1
./ht/tart-run.sh stage > /dev/null 2>&1 || { echo stage failed; ./ht/tart-run.sh down; exit 1; }
./ht/tart-run.sh gate > $Q/$PAR_COMMON/gate-out.txt 2>&1; python3 h/gate_ok.py $Q/$PAR_COMMON
mkdir -p $O
export TART_QA_WORK=/Volumes/CorbanuDrive/Corbanu/.codex-work/tart-eval
. /Volumes/CorbanuDrive/Corbanu/worktrees/pf84-cb1/scripts/qa/tart/lib.sh
ag() { ssh -n "${SSH_OPTS[@]}" "agent@$(cat $Q/$PAR_COMMON/vm-ip.txt)" "$@"; }
asl() { ag "cd /tmp; sudo -n -u $L -H /bin/bash -c 'export AGMS_RUN=\$HOME/runs/$R AGMS_PROXY=http://127.0.0.1:18443; $1'"; }
ag "cd /tmp; sudo -n -u $L -H /opt/agms/bin/agms-launch probe $R" > $O/probe.txt 2>&1
ag "cd /tmp; sudo -n -u $L -H /opt/agms/bin/agms-launch prep $R x zai3 zai3off zaione zainokey zainokeyoff fresh custom oai1" > $O/prep.txt 2>&1
asl '/opt/agms/bin/agms-product zai3 --version; /opt/agms/bin/agms-product zai3 login --help | head -30' > $O/version-help.txt 2>&1
for a in "zai3 exec --account work" "zai3 exec" "zaione exec" "custom exec --account work" "custom exec" "oai1 exec"; do
  echo "=== $a" >> $O/execs.txt
  asl "cd \$AGMS_RUN/fixture/workspace; /opt/agms/bin/agms-product $a --skip-git-repo-check \"Reply with exactly SHAKE-OK\" < /dev/null 2>&1 | tail -5" >> $O/execs.txt 2>&1
done
asl '/opt/agms/bin/agms-product zai3 login status; /opt/agms/bin/agms-product oai1 login status' > $O/login-status.txt 2>&1
# Device-code screen through the pass-through: start, read it, cancel at once (no code leaves this script's log).
/opt/homebrew/bin/tmux -S "$Q/tmux.sock" has-session -t passthrough 2>/dev/null || \
  /opt/homebrew/bin/tmux -S "$Q/tmux.sock" new-session -d -s passthrough "exec /usr/bin/python3 $Q/h/host_passthrough.py --port 11248 --log $Q/logs/host-passthrough.jsonl"
ag "cd /tmp; sudo -n -u $L -H /opt/agms/bin/agms-launch signin-start $R"; sleep 20
ag "cd /tmp; sudo -n -u $L -H /opt/agms/bin/agms-launch signin-screen $R" | sed -E 's/[A-Z0-9]{4}-[A-Z0-9]{4,6}/<CODE>/g' > $O/devicecode-screen.txt 2>&1
ag "cd /tmp; sudo -n -u $L -H /opt/agms/bin/agms-launch signin-stop $R"
/opt/homebrew/bin/tmux -S "$Q/tmux.sock" kill-session -t passthrough
ag "cd /tmp; sudo -n -u $L -H /opt/agms/bin/agms-launch manifest $R" > $O/manifest.txt 2>&1
ag 'cat ~/agms/mediator/mediator.jsonl' > $O/mediator.jsonl 2>&1
ag "cd /tmp; sudo -n -u $L -H /opt/agms/bin/agms-launch collect $R" > $O/collected.tar 2>/dev/null
./ht/tart-run.sh down > $O/down.txt 2>&1
echo shakedown done; ls $O
