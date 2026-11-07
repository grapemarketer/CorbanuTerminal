#!/bin/bash
# pf84-cb1 (D137, PF-84 code-blind run): utm-par3's Tart driver with this run's paths, lock holder and VM name; the
# tunnel also carries 18602 -> host blind pass-through (11248, running only for the t1 group); groups get prepared
# product homes (prep) and, for home oai1, the coordinator-run t1 device-code sign-in and the t1 sign-out.
# utm-par3 (D139/D141) host-side driver: utm-par2's Tart driver, unchanged except paths, the lock holder, the VM name and
# one guest-file set (h/) and one stage shared with the UTM driver. tart-par1: the Moderate milestone harness on one fresh Tart clone of
# corbanu-qa-base per group, for the D34/D68 parity run. From qwen-1052's tart-run.sh with the local-model gate removed.
#   tart-run.sh up | seal | tunnel | stage | gate | group LANE GROUP [LABEL] | status | down
# One Tart VM at a time (scripts/qa/tart/lib.sh guard_slot); UTM is never touched. No real key enters the guest: the
# host relay (host_zai_relay.py, 127.0.0.1:11237) adds the Z.AI key; the guest reaches it only through one SSH reverse
# tunnel bound to the guest's loopback (18601), which only the coordinator's mediator may use (pf). No local model.
set -euo pipefail
Q=<Q>
WT=<Q>
WORKERS=/Volumes/CorbanuDrive/Corbanu/.codex-work/workers-20261002
export TART_QA_WORK=/Volumes/CorbanuDrive/Corbanu/.codex-work/tart-eval   # keys + known_hosts of the base
export TART_HOME=/Volumes/CorbanuDrive/Corbanu/.codex-work/tart-home
export TART=/Volumes/CorbanuDrive/Corbanu/.codex-work/tart-bin/tart.app/Contents/MacOS/tart
# shellcheck source=/dev/null
. "$WT/scripts/qa/tart/lib.sh"
VM=pf84-rerun-d136
LOCK=$WORKERS/vm-tart.lock
TM=(/opt/homebrew/bin/tmux -S "$Q/tmux.sock")
C=$Q/${PAR_COMMON:?set PAR_COMMON to the evidence folder of this clone, e.g. common/T01-20261020T120000Z}
mkdir -p "$Q/runs"   # one evidence folder per clone: created by `up`, which refuses an existing one (review r2)
ip_of() { cat "$C/vm-ip.txt"; }
ag() { ssh -n "${SSH_OPTS[@]}" "agent@$(ip_of)" "$@"; }         # coordinator account, no stdin
agi() { ssh "${SSH_OPTS[@]}" "agent@$(ip_of)" "$@"; }           # coordinator account, stdin
root() { gssh "$(ip_of)" "sudo bash -s"; }                       # image admin -> root, script on stdin
# The tunnel: SSH_OPTS without ClearAllForwardings; remote listeners on the guest's loopback only.
TUN_OPTS=(-i "$TART_QA_KEY" -o IdentitiesOnly=yes -o UserKnownHostsFile="$TART_QA_KNOWN_HOSTS" -o StrictHostKeyChecking=yes
  -o HostKeyAlias=tart-qa-base -o BatchMode=yes -o ConnectTimeout=5 -o ForwardAgent=no -o ExitOnForwardFailure=yes
  -o ServerAliveInterval=15 -o ServerAliveCountMax=4 -o LogLevel=ERROR -N
  -R 127.0.0.1:18601:127.0.0.1:11247 -R 127.0.0.1:18602:127.0.0.1:11248)

case "${1:-}" in
  up)
    [ ! -e "$LOCK" ] || grep -q '^holder: pf84-rerun$' "$LOCK" || die "Tart lock held: $(cat "$LOCK")"
    [ ! -e "$C" ] || die "evidence folder $C already exists; each clone needs its own (PAR_COMMON)"
    mkdir -p "$C"
    printf 'holder: pf84-rerun\ntaken: %s\nvm: Tart %s (clone of %s)\n' "$(date -u +%FT%TZ)" "$VM" "$TART_QA_BASE" > "$LOCK"
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
      # Environment manifest (review r1): tool versions, console/login-window state, hardware.
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
    # pf84-rerun: a re-stage (resumed clone) must not keep files of the earlier package or packet.
    ag 'cd ~/agms/stage && for d in package packet h; do [ ! -e "$d" ] || rm -r "$d"; done; echo cleared-stage'
    COPYFILE_DISABLE=1 tar --no-xattrs -C "$Q/stage" -cf - package package-files.txt packet | agi 'cd ~/agms/stage && tar -xf - && echo staged-package-packet'
    items="agms-launch agms-probe agms-product agms-tui agms-mcp-notes agms-prep agms_mediator.py tmux.conf mediation-ca.pem product-config.toml fixture.tar expected.json"
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
    R=gate-agmx0-$(date -u +%Y%m%dT%H%M%SZ); echo "$R" > "$O/current"   # the checks read only this run (review r2)
    ag 'for h in github.com raw.githubusercontent.com codeload.github.com example.com api.z.ai; do curl -sS -m 8 -o /dev/null https://$h 2>&1; echo "rc=$? $h"; done' > "$O/agent-egress.txt" 2>&1 || true
    ag "cd /tmp; sudo -n -u agmx0 -H /opt/agms/bin/agms-launch probe $R" > "$O/$R.summary" 2> "$O/$R.stderr" || true
    ag "cd /tmp; sudo -n -u agmx0 -H /opt/agms/bin/agms-launch smoke $R" > "$O/smoke-$R.jsonl" 2> "$O/smoke-$R.stderr" || true
    # Relay positive controls as the coordinator account (model listings only; no inference).
    ag 'for u in https://api.z.ai/api/paas/v4/models; do
          printf "%s " "$u"; curl -sS -m 20 -o /dev/null -H "Authorization: Bearer agms-synthetic-placeholder-not-a-credential" -w "%{http_code}\n" --proxy http://127.0.0.1:18443 --cacert ~/agms/stage/h/mediation-ca.pem "$u"; done
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
    # This clone's own gate must have passed: probes ok, smoke turn, relay positive control 200 (review r1).
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
      HOMES="${GROUP_HOMES:?set GROUP_HOMES (space-separated home names)}"
      ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch prep $RUN x $HOMES" > "$OUT/prep.txt" 2>&1 || echo "prep rc=$?" >> "$OUT/prep.txt"
      case " $HOMES " in *" oai1 "*) "$Q/ht/signin.sh" "$LANE" "$RUN" "$OUT" ;; esac
      date -u +%FT%TZ > "$OUT/executor-start.txt"
      set +e
      ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch executor $RUN $GROUP ${AGMS_EXECUTOR_MODEL:-glm-5.3-flash}" \
        > "$OUT/executor-events.jsonl" 2> "$OUT/executor.stderr"
      echo "exit=$?" > "$OUT/executor-exit.txt"
      set -e
      date -u +%FT%TZ > "$OUT/executor-end.txt"
    fi
    ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch cleanup $RUN" > /dev/null 2>&1 || true
    ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch manifest $RUN" > "$OUT/homes-manifest.txt" 2>&1 || true
    ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch signin-stop $RUN" > /dev/null 2>&1 || true
    ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch signout $RUN" > "$OUT/signout.txt" 2>&1 || echo "SIGNOUT FAILED rc=$?" >> "$OUT/signout.txt"
    "${TM[@]}" kill-session -t passthrough 2> /dev/null && echo "$(date -u +%FT%TZ) host pass-through stopped" >> "$OUT/signout.txt" || true
    ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch collect $RUN" > "$OUT/collected.tar" 2> "$OUT/collect.stderr" || true
    ag 'cat ~/agms/mediator/mediator.jsonl' > "$OUT/mediator-at-end.jsonl" 2>&1 || true
    ag 'tail -1 ~/agms/guard.log; grep -c SECURITYAGENT-SEEN ~/agms/guard.log' > "$OUT/guard-after.txt" 2>&1 || true
    memory_pressure -Q | tail -1 > "$OUT/memory-after.txt"
    wipe > "$OUT/wipe-after.txt" 2>&1 || true
    echo "done $RUN $(cat "$OUT/executor-exit.txt" 2>/dev/null)" | tee -a "$Q/runs/seq.log"
    ;;
  signin)
    # pf84-rerun: D136 sign-in ahead of the run, in the clone that the D136 group later runs in (the VM is stopped,
    # not deleted, in between; `group-continue` then runs the executor in this same RUN without wiping or re-prepping).
    LANE="$2"; GROUP="$3"
    RUN="agms-$GROUP-$LANE-$(date -u +%Y%m%dT%H%M%SZ)"
    OUT="$Q/runs/$RUN"; mkdir -p "$OUT"
    printf '%s %s %s\n' "$LANE" "$GROUP" "$RUN" > "$Q/d136-run.txt"
    echo "== $RUN signin (evidence folder $C)" | tee -a "$Q/runs/seq.log"
    ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch probe $RUN" > "$OUT/preflight-signin.stdout" 2> "$OUT/preflight-signin.stderr" || true
    grep -q '"ok": true' "$OUT/preflight-signin.stdout" || die "preflight failed for $RUN (see $OUT)"
    ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch prep $RUN x oai1" > "$OUT/prep.txt" 2>&1 || echo "prep rc=$?" >> "$OUT/prep.txt"
    "$Q/ht/signin.sh" "$LANE" "$RUN" "$OUT"
    ;;
  reseal)
    # pf84-rerun: after `resume`, re-apply the sealed network that a reboot drops (pf is not enabled at boot; the DNS
    # responder's LaunchDaemon reloads by itself). `stage` then re-runs setup-agent (mediator, guard) and provision
    # (per-user pf anchor, read-only package and packet); lane homes are left as they are.
    printf '%s\n' 'pfctl -f /etc/pf.conf 2>&1 | grep -v -e ALTQ -e "flushing of rules" -e "present in the main" -e "pf.conf for further" -e "^$" || true' \
      'pfctl -E 2>&1 | grep -v -e ALTQ -e "^$" || true' \
      'for _ in $(seq 1 20); do lsof -nP -iUDP@127.0.0.1:53 > /dev/null 2>&1 && break; sleep 0.5; done' \
      'echo "resealed: pf $(pfctl -s info 2>/dev/null | awk "/Status/ {print \$2}"); rules $(pfctl -a corbanu.pf83 -sr | wc -l | tr -d " "); dns $(lsof -nP -iUDP@127.0.0.1:53 > /dev/null 2>&1 && echo listening || echo MISSING)"' \
      'curl -sS -m 6 -o /dev/null https://example.com 2>&1; echo "example.com rc=$?"' \
      | root 2>&1 | tee "$C/reseal.out"
    ;;
  group-continue)
    # pf84-rerun: the D136 group in the RUN that `signin` prepared (no wipe, no prep); otherwise as `group`.
    read -r LANE _SG RUN < "$Q/d136-run.txt"; GROUP="$2"
    OUT="$Q/runs/$RUN"
    echo "== $RUN continue as $GROUP (evidence folder $C)" | tee -a "$Q/runs/seq.log"
    if ! python3 "$Q/h/gate_ok.py" "$C" > "$OUT/gate-check.txt" 2>&1; then
      echo "gate not passed in this clone ($C); group blocked" | tee "$OUT/BLOCKED" | tee -a "$Q/runs/seq.log"; exit 0
    fi
    memory_pressure -Q | tail -1 > "$OUT/memory-before.txt"
    ag 'tail -1 ~/agms/guard.log; grep -c SECURITYAGENT-SEEN ~/agms/guard.log' > "$OUT/guard-before.txt" 2>&1 || true
    ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch probe $RUN" > "$OUT/preflight-summary.stdout" 2> "$OUT/preflight.stderr" || true
    if ! grep -q '"ok": true' "$OUT/preflight-summary.stdout"; then
      echo "preflight failed for $RUN; group blocked" | tee "$OUT/BLOCKED"
    else
      TM2=(/opt/homebrew/bin/tmux -S "$Q/tmux.sock")
      "${TM2[@]}" has-session -t passthrough 2>/dev/null || \
        "${TM2[@]}" new-session -d -s passthrough "exec /usr/bin/python3 $Q/h/host_passthrough.py --port 11248 --log $Q/logs/host-passthrough.jsonl"
      sleep 1
      date -u +%FT%TZ > "$OUT/executor-start.txt"
      set +e
      ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch executor $RUN $GROUP ${AGMS_EXECUTOR_MODEL:-glm-5.3-flash}" \
        > "$OUT/executor-events.jsonl" 2> "$OUT/executor.stderr"
      echo "exit=$?" > "$OUT/executor-exit.txt"
      set -e
      date -u +%FT%TZ > "$OUT/executor-end.txt"
    fi
    ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch cleanup $RUN" > /dev/null 2>&1 || true
    ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch manifest $RUN" > "$OUT/homes-manifest.txt" 2>&1 || true
    ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch signin-stop $RUN" > /dev/null 2>&1 || true
    ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch signout $RUN" > "$OUT/signout.txt" 2>&1 || echo "SIGNOUT FAILED rc=$?" >> "$OUT/signout.txt"
    "${TM[@]}" kill-session -t passthrough 2> /dev/null && echo "$(date -u +%FT%TZ) host pass-through stopped" >> "$OUT/signout.txt" || true
    ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch collect $RUN" > "$OUT/collected.tar" 2> "$OUT/collect.stderr" || true
    ag 'cat ~/agms/mediator/mediator.jsonl' > "$OUT/mediator-at-end.jsonl" 2>&1 || true
    ag 'tail -1 ~/agms/guard.log; grep -c SECURITYAGENT-SEEN ~/agms/guard.log' > "$OUT/guard-after.txt" 2>&1 || true
    memory_pressure -Q | tail -1 > "$OUT/memory-after.txt"
    echo "done $RUN $(cat "$OUT/executor-exit.txt" 2>/dev/null)" | tee -a "$Q/runs/seq.log"
    ;;
  pause)
    # pf84-rerun: stop (not delete) the D136 clone so t1's sign-in survives until the run; the lock stays held.
    {
      "${TM[@]}" kill-session -t passthrough 2> /dev/null && echo "host pass-through stopped" || true
      "${TM[@]}" kill-session -t tunnel 2> /dev/null && echo "tunnel stopped" || true
      gssh "$(ip_of)" 'sync; sudo shutdown -h now' > /dev/null 2>&1 || true
      for _ in $(seq 1 60); do "$TART" list | awk -v v="$VM" '$2 == v {print $NF}' | grep -q stopped && break; sleep 2; done
      "${TM[@]}" kill-session -t vm 2> /dev/null || true
      date -u +%FT%TZ; "$TART" list
    } 2>&1 | tee -a "$C/vm-pause.txt"
    ;;
  resume)
    # pf84-rerun: boot the stopped D136 clone again (same evidence folder; the IP may change).
    {
      date -u +%FT%TZ; memory_pressure -Q | tail -1
      guard_slot
      "${TM[@]}" new-session -d -s vm \
        "TART_HOME='$TART_HOME' TMPDIR='$TMPDIR' TART_NO_AUTO_PRUNE=1 exec '$TART' run --no-graphics --no-audio --no-clipboard $VM > '$LOGS/$VM.run.log' 2>&1"
      sleep 2
      ip="$(wait_ip "$VM" 180)"; echo "$ip" > "$C/vm-ip.txt"
      wait_ssh "$ip" 180 || die "no SSH on $ip"
      echo "ssh ready $(date -u +%FT%TZ) $ip"; "$TART" list
    } 2>&1 | tee -a "$C/vm-resume.txt"
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
