#!/bin/bash
# Host-side coordinator for one case group. Usage: run_group.sh LANE GROUP [LABEL]
# wipe lane -> preflight probes -> fresh executor -> cleanup -> collect -> wipe. Lanes run one at a time.
set -uo pipefail
LANE="$1"; GROUP="$2"; LABEL="${3:-}"
W=/Volumes/CorbanuDrive/Corbanu/.codex-work/pf60-s10-20261016
SSH=($W/h/vmssh.sh -n agent@192.168.64.3)
wipe() {
  "${SSH[@]}" "cd /tmp; sudo -n -u $LANE -H /bin/bash -c '
    pkill -u \$(id -u) -f \"^/opt/agms/(bin/tmux|pkg/)\" || true
    find /Users/$LANE -mindepth 1 -delete 2>/dev/null
    for d in \$(getconf DARWIN_USER_TEMP_DIR) \$(getconf DARWIN_USER_CACHE_DIR); do find \"\$d\" -mindepth 1 -user \$(id -u) -delete 2>/dev/null; done
    find /private/tmp /Users/Shared -maxdepth 3 -user \$(id -u) -delete 2>/dev/null
    echo wiped: \$(find /Users/$LANE -mindepth 1 | wc -l) entries left; ps -axo uid= | awk -v u=\$(id -u) \"\\\$1==u\" | wc -l | sed s/^/processes:/'"
}
RUN="acc-$GROUP${LABEL:+-$LABEL}-$LANE-$(date -u +%Y%m%dT%H%M%SZ)"
OUT="$W/runs/$RUN"; mkdir -p "$OUT"
echo "== $RUN"
"${SSH[@]}" 'tail -1 ~/agms/guard.log; grep -c SECURITYAGENT-SEEN ~/agms/guard.log' > "$OUT/guard-before.txt" 2>&1
wipe > "$OUT/wipe-before.txt" 2>&1
"${SSH[@]}" "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch probe $RUN" > "$OUT/preflight-summary.stdout" 2> "$OUT/preflight.stderr"
if ! grep -q '"ok": true' "$OUT/preflight-summary.stdout"; then
  echo "preflight failed for $RUN; group blocked" | tee "$OUT/BLOCKED"
else
  date -u +%FT%TZ > "$OUT/executor-start.txt"
  "${SSH[@]}" "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch executor $RUN $GROUP ${AGMS_EXECUTOR_MODEL:-glm-5.3-flash}" \
    > "$OUT/executor-events.jsonl" 2> "$OUT/executor.stderr"
  echo "exit=$?" > "$OUT/executor-exit.txt"
  date -u +%FT%TZ > "$OUT/executor-end.txt"
fi
"${SSH[@]}" "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch cleanup $RUN" > /dev/null 2>&1
"${SSH[@]}" "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch collect $RUN" > "$OUT/collected.tar" 2> "$OUT/collect.stderr"
"${SSH[@]}" "cat ~/agms/mediator/mediator.jsonl" > "$OUT/mediator-at-end.jsonl"
"${SSH[@]}" 'tail -1 ~/agms/guard.log; grep -c SECURITYAGENT-SEEN ~/agms/guard.log' > "$OUT/guard-after.txt" 2>&1
wipe > "$OUT/wipe-after.txt" 2>&1
echo "done $RUN $(cat "$OUT/executor-exit.txt" 2>/dev/null)"
