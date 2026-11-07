#!/bin/bash
# acct-rerun (2026-10-22): acc-tart1's accounting harness driver (PF-60-S10 lineage) on one fresh Tart clone of
# corbanu-qa-base per group, with pf84-rerun's host-relay pattern: NO real key enters the guest. Two host relays
# (host_key_relay.py: Z.AI on 127.0.0.1:11261, OpenAI on 127.0.0.1:11262) add the keys; the guest reaches them only
# through one SSH reverse tunnel bound to the guest's loopback (18621, 18622), which pf lets only the coordinator
# account's mediator use (lanes may reach 127.0.0.1:18443 only).
#   tart-run.sh relays | up | seal | tunnel | stage | gate | group LANE GROUP [LABEL] | status | down | relays-stop
# One Tart VM at a time (lib.sh guard_slot, $WORKERS/vm-tart.lock); UTM is never touched.
# Each clone has its own evidence folder: CLONE=<name> (e.g. G1-20261022T1300Z) -> $W/common/$CLONE.
set -euo pipefail
W=/Volumes/CorbanuDrive/Corbanu/.codex-work/acct-rerun-20261022
WT=/Volumes/CorbanuDrive/Corbanu/worktrees/acct-rerun
WORKERS=/Volumes/CorbanuDrive/Corbanu/.codex-work/workers-20261002
export TART_QA_WORK=/Volumes/CorbanuDrive/Corbanu/.codex-work/tart-eval   # keys + known_hosts of the base
export TART_HOME=/Volumes/CorbanuDrive/Corbanu/.codex-work/tart-home
export TART=/Volumes/CorbanuDrive/Corbanu/.codex-work/tart-bin/tart.app/Contents/MacOS/tart
# shellcheck source=/dev/null
. "$WT/scripts/qa/tart/lib.sh"
VM=acct-rerun-run
LOCK=$WORKERS/vm-tart.lock
TM=(/opt/homebrew/bin/tmux -S "$W/tmux.sock")
mkdir -p "$W/runs" "$W/logs" "$W/common"
cmd="${1:-}"
case "$cmd" in relays|relays-stop) C="";; *) C=$W/common/${CLONE:?set CLONE to this clone evidence name};; esac
ip_of() { cat "$C/vm-ip.txt"; }
ag() { ssh -n "${SSH_OPTS[@]}" "agent@$(ip_of)" "$@"; }         # coordinator account, no stdin
agi() { ssh "${SSH_OPTS[@]}" "agent@$(ip_of)" "$@"; }           # coordinator account, stdin
root() { gssh "$(ip_of)" "sudo bash -s"; }                       # image admin -> root, script on stdin
TUN_OPTS=(-i "$TART_QA_KEY" -o IdentitiesOnly=yes -o UserKnownHostsFile="$TART_QA_KNOWN_HOSTS" -o StrictHostKeyChecking=yes
  -o HostKeyAlias=tart-qa-base -o BatchMode=yes -o ConnectTimeout=5 -o ForwardAgent=no -o ExitOnForwardFailure=yes
  -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -o LogLevel=ERROR -N
  -R 127.0.0.1:18621:127.0.0.1:11261 -R 127.0.0.1:18622:127.0.0.1:11262)

case "$cmd" in
  relays)
    # Keys fetched from the vault at use time inside each relay's own command; never printed (no tracing).
    "${TM[@]}" has-session -t relay-zai 2>/dev/null || "${TM[@]}" new-session -d -s relay-zai \
      "RELAY_KEY=\"\$(/usr/bin/env -u CORBANU_HOME -u CODEX_HOME $HOME/.local/bin/corbanu vault auth-helper zai_key_3)\" exec /usr/bin/python3 $W/h/host_key_relay.py --route zai --port 11261 --log $W/logs/host-relay-zai.jsonl"
    "${TM[@]}" has-session -t relay-openai 2>/dev/null || "${TM[@]}" new-session -d -s relay-openai \
      "RELAY_KEY=\"\$(/usr/bin/env -u CORBANU_HOME -u CODEX_HOME $HOME/.local/bin/corbanu vault auth-helper openai-api-key)\" exec /usr/bin/python3 $W/h/host_key_relay.py --route openai --port 11262 --log $W/logs/host-relay-openai.jsonl"
    sleep 2; tail -n1 "$W/logs/host-relay-zai.jsonl" "$W/logs/host-relay-openai.jsonl"
    ;;
  relays-stop)
    for s in relay-zai relay-openai; do "${TM[@]}" kill-session -t $s 2>/dev/null && echo "$s stopped" || true; done
    ;;
  up)
    [ ! -e "$LOCK" ] || grep -q '^holder: acct-rerun$' "$LOCK" || die "Tart lock held: $(cat "$LOCK")"
    [ ! -e "$C" ] || die "evidence folder $C already exists; each clone needs its own CLONE"
    mkdir -p "$C"
    printf 'holder: acct-rerun\ntaken: %s\nvm: Tart %s (clone of %s)\n' "$(date -u +%FT%TZ)" "$VM" "$TART_QA_BASE" > "$LOCK"
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
      "$TART" list
    } 2>&1 | tee "$C/vm-start.txt"
    ;;
  seal)
    A=$W/seal-assets; [ ! -e "$A" ] || rm -r "$A" < /dev/null; mkdir -p "$A"
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
    ag 'lsof -nP -iTCP:18621 -iTCP:18622 -sTCP:LISTEN | awk "NR>1 {print \$1, \$3, \$9}"' | tee "$C/tunnel-listeners.txt"
    ;;
  stage)
    . "$W/caps.env"   # MOONSHOT_CAP=0 REALTIME_CAP=0 (no Moonshot, no realtime in this re-run)
    ag 'mkdir -p -m 700 ~/agms ~/agms/stage ~/agms/mediator; echo ok'
    ag 'ls -la ~/.pf83-auth 2>&1 | sed "s/^/pf83-auth: /"' | tee "$C/guest-key-dir-start.txt"
    COPYFILE_DISABLE=1 tar --no-xattrs -C "$W/stage" -cf - package package-files.txt packet | agi 'cd ~/agms/stage && tar -xf - && echo staged-package-packet'
    items="agms-launch agms-probe agms-product agms-tui tmux.conf mediation-ca.pem product-config.toml fixture.tar expected.json acc_mediator.py acc_standin.py chatgpt_standin.py realtime_driver.py"
    COPYFILE_DISABLE=1 tar --no-xattrs -C "$W/h" -czf - $items | agi 'mkdir -p ~/agms/stage/h && cd ~/agms/stage/h && tar -xzf - && echo staged-harness'
    agi 'umask 077; cat > ~/agms/mediator/leaf.pem' < "$W/ca/leaf.pem"
    agi 'umask 077; cat > ~/agms/mediator/leaf.key' < "$W/ca/leaf.key"
    printf 'MOONSHOT_CAP=%s\nREALTIME_CAP=%s\n' "$MOONSHOT_CAP" "$REALTIME_CAP" | agi 'umask 077; cat > ~/agms/caps'
    agi 'bash -s' < "$W/h/setup-agent.sh" > "$C/setup-agent.out" 2>&1 || die "setup-agent failed (see $C/setup-agent.out)"
    root < "$W/h/provision.sh" > "$C/provision.out" 2>&1 || die "provision failed (see $C/provision.out)"
    root < "$W/h/capture-policy.sh" > "$C/policy-root-start.txt" 2>&1 || true
    tail -4 "$C/setup-agent.out"; tail -3 "$C/provision.out"
    ;;
  gate)
    O=$C/gate; mkdir -p "$O"
    R=gate-agmx0-$(date -u +%Y%m%dT%H%M%SZ); echo "$R" > "$O/current"
    ag 'for h in github.com raw.githubusercontent.com codeload.github.com example.com api.z.ai api.openai.com; do curl -sS -m 8 -o /dev/null https://$h 2>&1; echo "rc=$? $h"; done' > "$O/agent-egress.txt" 2>&1 || true
    ag "cd /tmp; sudo -n -u agmx0 -H /opt/agms/bin/agms-launch probe $R" > "$O/$R.summary" 2> "$O/$R.stderr" || true
    ag "cd /tmp; sudo -n -u agmx0 -H /opt/agms/bin/agms-launch smoke $R" > "$O/smoke-$R.jsonl" 2> "$O/smoke-$R.stderr" || true
    # Relay positive controls as the coordinator account (model listings only; no inference).
    ag 'for u in https://api.z.ai/api/paas/v4/models https://api.openai.com/v1/models; do
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
    RUN="acc-$GROUP${LABEL:+-$LABEL}-$LANE-$(date -u +%Y%m%dT%H%M%SZ)"
    OUT="$W/runs/$RUN"; mkdir -p "$OUT"
    echo "$C" > "$OUT/clone-evidence-folder.txt"
    echo "== $RUN (clone $CLONE)" | tee -a "$W/runs/seq.log"
    if ! python3 "$W/h/gate_ok.py" "$C" > "$OUT/gate-check.txt" 2>&1; then
      echo "gate not passed in this clone ($C); group blocked" | tee "$OUT/BLOCKED" | tee -a "$W/runs/seq.log"; exit 0
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
    echo "done $RUN $(cat "$OUT/executor-exit.txt" 2>/dev/null)" | tee -a "$W/runs/seq.log"
    ;;
  status)
    ag 'tail -2 ~/agms/mediator/mediator.jsonl | cut -c1-300; tail -1 ~/agms/guard.log; grep -c SECURITYAGENT-SEEN ~/agms/guard.log'
    memory_pressure -Q | tail -1
    ;;
  down)
    # Closing: logs and root policy, lane homes and synthetic files deleted and checked, the guest key folder checked
    # (no real key was ever placed), tunnel stopped, clone stopped and deleted. LOCK_KEEP=1 keeps the lock (next group).
    {
      ag 'cat ~/agms/mediator/mediator.jsonl' > "$C/mediator-final.jsonl" 2>/dev/null || true
      ag 'cat ~/agms/mediator/standin.jsonl' > "$C/standin-final.jsonl" 2>/dev/null || true
      ag 'cat ~/agms/mediator/chatgpt-standin.jsonl' > "$C/chatgpt-standin-final.jsonl" 2>/dev/null || true
      ag 'cat ~/agms/guard-baseline.txt' > "$C/guard-baseline.txt" 2>/dev/null || true
      ag 'tail -1 ~/agms/guard.log; echo "SECURITYAGENT-SEEN lines: $(grep -c SECURITYAGENT-SEEN ~/agms/guard.log)"; echo "checks: $(wc -l < ~/agms/guard.log)"' > "$C/guard-final.txt" 2>/dev/null || true
      root < "$W/h/capture-policy.sh" > "$C/policy-root-end.txt" 2>&1 || true
      printf '%s\n' 'for u in agmx0 agmx1 agmx2 agmx3; do pkill -u $u || true; done; sleep 1; for u in agmx0 agmx1 agmx2 agmx3; do echo "$u processes: $(ps -axo user= | grep -c "^$u$")"; done' | root > "$C/kill-lane-procs-end.out" 2>&1 || true
      printf '%s\n' 'for u in agmx0 agmx1 agmx2 agmx3; do [ ! -d /Users/$u ] || find /Users/$u -mindepth 1 -delete; done' \
        '[ ! -e /Users/agent/agms/mediator/leaf.key ] || rm /Users/agent/agms/mediator/leaf.key' \
        'for u in agmx0 agmx1 agmx2 agmx3; do echo "$u home entries left: $(find /Users/$u -mindepth 1 2>/dev/null | wc -l | tr -d " ")"; done' \
        'test ! -e /Users/agent/agms/mediator/leaf.key && echo "mediator leaf key: absent"' \
        'for k in zai_api_key openai_api_key kimi_api_key; do test ! -e /Users/agent/.pf83-auth/$k && echo "real key file $k: absent (never placed)"; done' \
        'grep -qx "{\"synthetic\": \"placeholder, not a credential\"}" /Users/agent/.pf83-auth/auth.json && echo "auth.json: synthetic placeholder only"' \
        | root > "$C/erase-test-data.out" 2>&1 || echo "ERASE FAILED" >> "$C/erase-test-data.out"
      cat "$C/erase-test-data.out"
      "${TM[@]}" kill-session -t tunnel 2> /dev/null && echo "tunnel stopped" || true
      date -u +%FT%TZ; stop_vm "$VM" 10; "${TM[@]}" kill-session -t vm 2> /dev/null || true
      "$TART" delete "$VM"; echo "deleted $VM"; "$TART" list
      date -u +%FT%TZ
    } 2>&1 | tee "$C/vm-state-end.txt"
    if [ "${LOCK_KEEP:-0}" = 1 ]; then echo "lock kept for the next clone"; else rm "$LOCK" && echo "lock released"; fi
    ;;
  *) echo "usage: CLONE=name tart-run.sh relays|up|seal|tunnel|stage|gate|group LANE GROUP [LABEL]|status|down|relays-stop" >&2; exit 64 ;;
esac
