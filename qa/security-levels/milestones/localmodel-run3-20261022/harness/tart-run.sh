#!/bin/bash
# qa-local3 (from qwen-rerun2) host-side driver: the milestone harness on one fresh Tart clone of corbanu-qa-base per group, with the
# owner's local model (mlx-serve on the host, 127.0.0.1:11235) as the product's model. From qwen-1052's tart-run.sh,
# with tart-par1's review fixes (one evidence folder per clone, environment manifest, this clone's gate checked).
#   tart-run.sh up | seal | tunnel | stage | gate | group LANE GROUP [LABEL] | status | down
# One Tart VM at a time (scripts/qa/tart/lib.sh guard_slot); UTM is never touched. No real key enters the guest: the
# host relay (host_zai_relay.py, 127.0.0.1:11237) adds the Z.AI key and the host gate (qwen_gate.py,
# 127.0.0.1:11236) fronts the local model; the guest reaches both only through one SSH reverse tunnel bound to the
# guest's loopback (18601, 18602), which only the coordinator's mediator may use (pf).
set -euo pipefail
Q=/Volumes/CorbanuDrive/Corbanu/.codex-work/qa-local3-20261022
WT=/Volumes/CorbanuDrive/Corbanu/worktrees/qa-local3
WORKERS=/Volumes/CorbanuDrive/Corbanu/.codex-work/workers-20261002
export TART_QA_WORK=/Volumes/CorbanuDrive/Corbanu/.codex-work/tart-eval   # keys + known_hosts of the base
export TART_HOME=/Volumes/CorbanuDrive/Corbanu/.codex-work/tart-home
export TART=/Volumes/CorbanuDrive/Corbanu/.codex-work/tart-bin/tart.app/Contents/MacOS/tart
# shellcheck source=/dev/null
. "$WT/scripts/qa/tart/lib.sh"
VM=qa-local3-run
LOCK=$WORKERS/vm-tart.lock
TM=(/opt/homebrew/bin/tmux -S "$Q/tmux.sock")
C=$Q/${QWEN_COMMON:?set QWEN_COMMON to the evidence folder of this clone, e.g. common/R01}
mkdir -p "$Q/runs"   # one evidence folder per clone: created by `up`, which refuses an existing one
ip_of() { cat "$C/vm-ip.txt"; }
ag() { ssh -n "${SSH_OPTS[@]}" "agent@$(ip_of)" "$@"; }         # coordinator account, no stdin
agi() { ssh "${SSH_OPTS[@]}" "agent@$(ip_of)" "$@"; }           # coordinator account, stdin
root() { gssh "$(ip_of)" "sudo bash -s"; }                       # image admin -> root, script on stdin
# The tunnel: SSH_OPTS without ClearAllForwardings; remote listeners on the guest's loopback only.
TUN_OPTS=(-i "$TART_QA_KEY" -o IdentitiesOnly=yes -o UserKnownHostsFile="$TART_QA_KNOWN_HOSTS" -o StrictHostKeyChecking=yes
  -o HostKeyAlias=tart-qa-base -o BatchMode=yes -o ConnectTimeout=5 -o ForwardAgent=no -o ExitOnForwardFailure=yes
  -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -o LogLevel=ERROR -N
  -R 127.0.0.1:18601:127.0.0.1:11237 -R 127.0.0.1:18602:127.0.0.1:11236)

case "${1:-}" in
  up)
    [ ! -e "$LOCK" ] || grep -q '^holder: qa-local3$' "$LOCK" || die "Tart lock held: $(cat "$LOCK")"
    [ ! -e "$C" ] || die "evidence folder $C already exists; each clone needs its own (QWEN_COMMON)"
    mkdir -p "$C"
    printf 'holder: qa-local3\ntaken: %s\nvm: Tart %s (clone of %s)\n' "$(date -u +%FT%TZ)" "$VM" "$TART_QA_BASE" > "$LOCK"
    {
      date -u +%FT%TZ; "$TART" list; "$UTMCTL" list 2>&1 || true
      memory_pressure -Q | tail -1
      "$TART" clone "$TART_QA_BASE" "$VM"
      guard_slot
      "${TM[@]}" new-session -d -s vm \
        "TART_HOME='$TART_HOME' TMPDIR='$TMPDIR' TART_NO_AUTO_PRUNE=1 exec '$TART' run --no-graphics --no-audio --no-clipboard $VM > '$LOGS/$VM.run.log' 2>&1"
      sleep 2
      { pgrep -f "tart run --no-graphics --no-audio --no-clipboard $VM" || true; } | head -1 > "$LOGS/$VM.pid"
      [ -s "$LOGS/$VM.pid" ] || die "tart run did not start (see $LOGS/$VM.run.log)"
      echo "tart run pid $(cat "$LOGS/$VM.pid")"
      ip="$(wait_ip "$VM" 180)"; echo "$ip" > "$C/vm-ip.txt"
      wait_ssh "$ip" 180 || die "no SSH on $ip"
      echo "ssh ready $(date -u +%FT%TZ) $ip"
      gssh "$ip" 'sw_vers; uptime; csrutil status; spctl --status'
      gssh "$ip" "bash -s" < "$Q/h/env-manifest.sh"
      "$TART" list
    } 2>&1 | tee "$C/vm-start.txt"
    ;;
  seal)
    A=$Q/seal-assets; [ ! -e "$A" ] || rm -r "$A" < /dev/null; mkdir -p "$A"
    cp "$WT/scripts/qa/tart/seal-guest.sh" "$WT/scripts/qa/tart/dns-refuse.py" "$A/"
    cp /Volumes/CorbanuDrive/Corbanu/.codex-work/corbanu-terminal/tools/tmux-source/3.7c/bin/tmux "$A/"
    cp /opt/homebrew/opt/libevent/lib/libevent_core-2.1.7.dylib "$A/"
    cp "$TART_QA_KEY.pub" "$A/coordinator.pub"
    tar -C "$A" -czf - . | gssh "$(ip_of)" '{ [ ! -e /tmp/qa-seal ] || rm -r /tmp/qa-seal; } && mkdir -m 700 /tmp/qa-seal && tar -xzf - -C /tmp/qa-seal'
    gssh "$(ip_of)" 'sudo bash /tmp/qa-seal/seal-guest.sh /tmp/qa-seal && rm -r /tmp/qa-seal' 2>&1 | tee "$C/seal.out"
    ;;
  tunnel)
    # Restarts itself if the SSH session drops; every (re)connect is logged.
    "${TM[@]}" kill-session -t tunnel 2> /dev/null || true
    q=""; for a in "${TUN_OPTS[@]}"; do q="$q '${a//\'/\'\\\'\'}'"; done
    "${TM[@]}" new-session -d -s tunnel \
      "while :; do echo \"\$(date -u +%FT%TZ) connect agent@$(ip_of)\" >> '$C/tunnel.log'; ssh $q agent@$(ip_of) >> '$C/tunnel.log' 2>&1; echo \"\$(date -u +%FT%TZ) ssh exited rc=\$?\" >> '$C/tunnel.log'; sleep 3; done"
    sleep 4
    ag 'lsof -nP -iTCP:18601 -iTCP:18602 -sTCP:LISTEN | awk "NR>1 {print \$1, \$3, \$9}"' | tee "$C/tunnel-listeners.txt"
    ;;
  stage)
    ag 'mkdir -p -m 700 ~/agms ~/agms/stage ~/agms/mediator; echo ok'
    COPYFILE_DISABLE=1 tar --no-xattrs -C "$Q/stage" -cf - package package-files.txt packet | agi 'cd ~/agms/stage && tar -xf - && echo staged-package-packet'
    items="agms-launch agms-probe agms-product agms-tui agms-mcp-notes agms_mediator.py tmux.conf mediation-ca.pem product-config.toml product-config-acc.toml neutral-groups.txt writable-config-groups.txt fixture.tar expected.json rg"
    COPYFILE_DISABLE=1 tar --no-xattrs -C "$Q/h" -czf - $items | agi 'mkdir -p ~/agms/stage/h && cd ~/agms/stage/h && tar -xzf - && echo staged-harness'
    agi 'umask 077; cat > ~/agms/mediator/leaf.pem' < "$Q/ca/leaf.pem"
    agi 'umask 077; cat > ~/agms/mediator/leaf.key' < "$Q/ca/leaf.key"
    agi 'bash -s' < "$Q/h/setup-agent.sh" > "$C/setup-agent.out" 2>&1 || die "setup-agent failed (see $C/setup-agent.out)"
    root < "$Q/h/provision.sh" > "$C/provision.out" 2>&1 || die "provision failed (see $C/provision.out)"
    root < "$Q/h/capture-policy.sh" > "$C/policy-root-start.txt" 2>&1 || true
    tail -4 "$C/setup-agent.out"; tail -3 "$C/provision.out"
    ;;
  gate)
    O=$C/gate; mkdir -p "$O"
    R=gate-agmx0-$(date -u +%Y%m%dT%H%M%SZ); echo "$R" > "$O/current"   # the checks read only this run
    ag 'for h in github.com raw.githubusercontent.com codeload.github.com example.com api.z.ai; do curl -sS -m 8 -o /dev/null https://$h 2>&1; echo "rc=$? $h"; done' > "$O/agent-egress.txt" 2>&1 || true
    ag "cd /tmp; sudo -n -u agmx0 -H /opt/agms/bin/agms-launch probe $R" > "$O/$R.summary" 2> "$O/$R.stderr" || true
    ag "cd /tmp; sudo -n -u agmx0 -H /opt/agms/bin/agms-launch smoke $R" > "$O/smoke-$R.jsonl" 2> "$O/smoke-$R.stderr" || true
    # Relay positive controls as the coordinator account (model listings only; no inference).
    ag 'for u in https://api.z.ai/api/paas/v4/models https://qwen.agms.test/v1/models; do
          printf "%s " "$u"; curl -sS -m 20 -o /dev/null -w "%{http_code}\n" --proxy http://127.0.0.1:18443 --cacert ~/agms/stage/h/mediation-ca.pem "$u"; done
        tail -3 ~/agms/mediator/mediator.jsonl | cut -c1-300' > "$O/relay-positive-controls.txt" 2>&1 || true
    cat "$O/$R.summary"; grep -o 'AGMS-SMOKE-OK' "$O/smoke-$R.jsonl" | head -1; cat "$O/relay-positive-controls.txt"
    ;;
  group)
    LANE="$2"; GROUP="$3"; LABEL="${4:-}"
    wipe() {
      ag "cd /tmp; sudo -n -u $LANE -H /bin/bash -c '
        pkill -u \$(id -u) -f \"^/opt/agms/(bin/tmux|pkg/)\" || true
        find /Users/$LANE -mindepth 1 -delete 2>/dev/null
        for d in \$(getconf DARWIN_USER_TEMP_DIR) \$(getconf DARWIN_USER_CACHE_DIR); do find \"\$d\" -mindepth 1 -user \$(id -u) -delete 2>/dev/null; done
        find /private/tmp /Users/Shared -maxdepth 3 -user \$(id -u) -delete 2>/dev/null
        echo wiped: \$(find /Users/$LANE -mindepth 1 | wc -l) entries left; ps -axo uid= | awk -v u=\$(id -u) \"\\\$1==u\" | wc -l | sed s/^/processes:/'"
    }
    RUN="agms-$GROUP${LABEL:+-$LABEL}-$LANE-$(date -u +%Y%m%dT%H%M%SZ)"
    OUT="$Q/runs/$RUN"; mkdir -p "$OUT"
    echo "== $RUN (evidence folder $C)" | tee -a "$Q/runs/seq.log"
    # This clone's own gate must have passed: probes ok, smoke turn, both relay positive controls 200.
    if ! python3 "$Q/h/gate_ok.py" "$C" > "$OUT/gate-check.txt" 2>&1; then
      echo "gate not passed in this clone ($C); group blocked" | tee "$OUT/BLOCKED" | tee -a "$Q/runs/seq.log"
      exit 0
    fi
    memory_pressure -Q | tail -1 > "$OUT/memory-before.txt"
    ag 'tail -1 ~/agms/guard.log; grep -c SECURITYAGENT-SEEN ~/agms/guard.log' > "$OUT/guard-before.txt" 2>&1 || true
    wipe > "$OUT/wipe-before.txt" 2>&1 || true
    ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch probe $RUN" > "$OUT/preflight-summary.stdout" 2> "$OUT/preflight.stderr" || true
    if ! grep -q '"ok": true' "$OUT/preflight-summary.stdout"; then
      echo "preflight failed for $RUN; group blocked" | tee "$OUT/BLOCKED"
    else
      date -u +%FT%TZ > "$OUT/executor-start.txt"
      set +e
      ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch executor $RUN $GROUP ${AGMS_EXECUTOR_MODEL:-glm-5.3-flash}" \
        > "$OUT/executor-events.jsonl" 2> "$OUT/executor.stderr"
      echo "exit=$?" > "$OUT/executor-exit.txt"
      set -e
      date -u +%FT%TZ > "$OUT/executor-end.txt"
    fi
    ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch cleanup $RUN" > /dev/null 2>&1 || true
    ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch collect $RUN" > "$OUT/collected.tar" 2> "$OUT/collect.stderr" || true
    ag 'cat ~/agms/mediator/mediator.jsonl' > "$OUT/mediator-at-end.jsonl" 2>&1 || true
    ag 'tail -1 ~/agms/guard.log; grep -c SECURITYAGENT-SEEN ~/agms/guard.log' > "$OUT/guard-after.txt" 2>&1 || true
    memory_pressure -Q | tail -1 > "$OUT/memory-after.txt"
    wipe > "$OUT/wipe-after.txt" 2>&1 || true
    echo "done $RUN $(cat "$OUT/executor-exit.txt" 2>/dev/null)" | tee -a "$Q/runs/seq.log"
    ;;
  status)
    ag 'tail -2 ~/agms/mediator/mediator.jsonl | cut -c1-300; tail -1 ~/agms/guard.log; grep -c SECURITYAGENT-SEEN ~/agms/guard.log'
    memory_pressure -Q | tail -1
    ;;
  down)
    # Closing: logs and root policy, then delete the synthetic test data and placeholder files and verify, stop the
    # tunnel, stop and delete the clone, free the lock. (No real key was ever placed in this guest.)
    {
      ag 'cat ~/agms/mediator/mediator.jsonl' > "$C/mediator-final.jsonl" 2>/dev/null || true
      ag 'cat ~/agms/guard-baseline.txt' > "$C/guard-baseline.txt" 2>/dev/null || true
      ag 'tail -1 ~/agms/guard.log; echo "SECURITYAGENT-SEEN lines: $(grep -c SECURITYAGENT-SEEN ~/agms/guard.log)"; echo "checks: $(wc -l < ~/agms/guard.log)"' > "$C/guard-final.txt" 2>/dev/null || true
      root < "$Q/h/capture-policy.sh" > "$C/policy-root-end.txt" 2>&1 || true
      printf '%s\n' 'for u in agmx0 agmx1 agmx2 agmx3; do pkill -u $u || true; done; sleep 1; for u in agmx0 agmx1 agmx2 agmx3; do echo "$u processes: $(ps -axo user= | grep -c "^$u$")"; done' | root > "$C/kill-lane-procs-end.out" 2>&1 || true
      # Test data: every lane home (synthetic ~/.aws, ~/.ssh, ~/.netrc, run folders), the placeholder key file, the
      # mediator's synthetic leaf key; then check none is left.
      printf '%s\n' 'for u in agmx0 agmx1 agmx2 agmx3; do [ ! -d /Users/$u ] || find /Users/$u -mindepth 1 -delete; done' \
        '[ ! -e /Users/agent/.pf83-auth/zai_api_key ] || rm /Users/agent/.pf83-auth/zai_api_key' \
        '[ ! -e /Users/agent/agms/mediator/leaf.key ] || rm /Users/agent/agms/mediator/leaf.key' \
        'for u in agmx0 agmx1 agmx2 agmx3; do echo "$u home entries left: $(find /Users/$u -mindepth 1 2>/dev/null | wc -l | tr -d " ")"; done' \
        'test ! -e /Users/agent/.pf83-auth/zai_api_key && echo "placeholder key file: absent"' \
        'test ! -e /Users/agent/agms/mediator/leaf.key && echo "mediator leaf key: absent"' \
        'echo "synthetic credential files left anywhere: $(find /Users -path /Users/Shared -prune -o \( -name credentials -path "*/.aws/*" -o -name .netrc \) -print 2>/dev/null | wc -l | tr -d " ")"' \
        | root > "$C/erase-test-data.out" 2>&1 || echo "TEST DATA DELETION FAILED" >> "$C/erase-test-data.out"
      cat "$C/erase-test-data.out"
      "${TM[@]}" kill-session -t tunnel 2> /dev/null && echo "tunnel stopped" || echo "tunnel not running"
      date -u +%FT%TZ; stop_vm "$VM" 10; "$TART" delete "$VM"; echo "deleted $VM"; "$TART" list
      date -u +%FT%TZ
    } 2>&1 | tee "$C/vm-state-end.txt"
    rm "$LOCK" && echo "lock released"
    ;;
  *) echo "usage: tart-run.sh up|seal|tunnel|stage|gate|group LANE GROUP [LABEL]|status|down" >&2; exit 64 ;;
esac
