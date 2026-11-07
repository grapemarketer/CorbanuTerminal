#!/bin/bash
# shk.sh RUN tui ARGS... | RUN sh 'shell as agmx0 with AGMS_RUN set'   (shakedown capture lane agmx0 only)
Q=/Volumes/CorbanuDrive/Corbanu/.codex-work/qa-local3-20261022
export TART_QA_WORK=/Volumes/CorbanuDrive/Corbanu/.codex-work/tart-eval
. /Volumes/CorbanuDrive/Corbanu/worktrees/qa-local3/scripts/qa/tart/lib.sh
R="$1"; m="$2"; shift 2
ip=$(cat $Q/shakedown/vm-ip.txt)
if [ "$m" = tui ]; then
  q=""; for a in "$@"; do q="$q '${a//\'/\'\\\'\'}'"; done
  ssh -n "${SSH_OPTS[@]}" "agent@$ip" "cd /tmp; sudo -n -u agmx0 -H /opt/agms/bin/agms-launch tui $R$q"
else
  printf 'export AGMS_RUN=$HOME/runs/%s; cd $AGMS_RUN 2>/dev/null || true\n%s\n' "$R" "$1" | ssh "${SSH_OPTS[@]}" "agent@$ip" "cd /tmp; sudo -n -u agmx0 -H /bin/bash -s"
fi
