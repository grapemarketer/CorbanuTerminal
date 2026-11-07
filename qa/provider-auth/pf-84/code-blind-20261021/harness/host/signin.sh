#!/bin/bash
# pf84-cb1 (D136): coordinator-run device-code sign-in of the live ChatGPT test account as named OpenAI account `t1` in
# home oai1, inside the guest lane. Only the device URL and one-time code leave the guest: they are written to the
# handoff file for the coordinator, who relays them to Travis. Up to 3 attempts (each code lives 15 minutes).
# usage: signin.sh LANE RUN OUT      (called by tart-run.sh group; uses its ip/ssh helpers via PAR_COMMON)
set -u
LANE="$1"; RUN="$2"; OUT="$3"
Q=/Volumes/CorbanuDrive/Corbanu/.codex-work/pf84-cb1-20261021
HAND=/Volumes/CorbanuDrive/Corbanu/.codex-work/workers-20261002/pf84-cb1.signin.md
C=$Q/${PAR_COMMON:?}
export TART_QA_WORK=/Volumes/CorbanuDrive/Corbanu/.codex-work/tart-eval
# shellcheck source=/dev/null
. /Volumes/CorbanuDrive/Corbanu/worktrees/pf84-cb1/scripts/qa/tart/lib.sh
ag() { ssh -n "${SSH_OPTS[@]}" "agent@$(cat "$C/vm-ip.txt")" "$@"; }
L() { ag "cd /tmp; sudo -n -u $LANE -H /opt/agms/bin/agms-launch $1 $RUN"; }
# Host pass-through for chatgpt.com/auth.openai.com (this group only).
TM=(/opt/homebrew/bin/tmux -S "$Q/tmux.sock")
"${TM[@]}" has-session -t passthrough 2>/dev/null || \
  "${TM[@]}" new-session -d -s passthrough "exec /usr/bin/python3 $Q/h/host_passthrough.py --port 11248 --log $Q/logs/host-passthrough.jsonl"
sleep 1
for attempt in 1 2 3; do
  L signin-stop > /dev/null 2>&1
  L signin-start > /dev/null 2>&1
  url=""; code=""
  for _ in $(seq 1 60); do
    scr="$(L signin-screen 2>/dev/null)"
    url="$(printf '%s\n' "$scr" | grep -o -E 'https://auth\.openai\.com/[A-Za-z0-9/_.-]+' | head -1)"
    code="$(printf '%s\n' "$scr" | grep -o -E '^ *[A-Z0-9]{4}-[A-Z0-9]{4,6} *$' | tr -d ' ' | head -1)"
    [ -n "$url" ] && [ -n "$code" ] && break
    printf '%s\n' "$scr" | grep -q 'SIGNIN-EXIT' && break
    sleep 2
  done
  if [ -z "$code" ]; then
    echo "$(date -u +%FT%TZ) attempt $attempt: no device code shown" >> "$OUT/signin.txt"
    L signin-screen >> "$OUT/signin.txt" 2>&1
    continue
  fi
  {
    echo "# PF-84 code-blind run: ChatGPT test account sign-in (pf84-cb1) — attempt $attempt"
    echo
    echo "Please approve with the FREE ChatGPT TEST account (not your own)."
    echo
    echo "URL: $url"
    echo "Code: $code"
    echo
    echo "Posted $(date -u +%FT%TZ); expires 15 minutes later."
  } > "$HAND"
  echo "$(date -u +%FT%TZ) attempt $attempt: code posted to the handoff file" >> "$OUT/signin.txt"
  ok=""
  for _ in $(seq 1 95); do
    scr="$(L signin-screen 2>/dev/null)"
    if printf '%s\n' "$scr" | grep -q 'Signed in'; then ok=1; break; fi
    printf '%s\n' "$scr" | grep -q 'SIGNIN-EXIT' && break
    sleep 10
  done
  # Record the outcome lines only, with the code masked.
  L signin-screen 2>/dev/null | sed -E 's/[A-Z0-9]{4}-[A-Z0-9]{4,6}/<CODE>/g; s/[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+/<email>/g' >> "$OUT/signin.txt"
  if [ -n "$ok" ]; then
    printf '\nDONE: sign-in approved and completed at %s. Code no longer needed.\n' "$(date -u +%H:%MZ)" >> "$HAND"
    echo "$(date -u +%FT%TZ) signed in" >> "$OUT/signin.txt"
    L signin-stop > /dev/null 2>&1
    exit 0
  fi
  printf '\nEXPIRED/FAILED at %s (attempt %s); a new code follows if attempts remain.\n' "$(date -u +%H:%MZ)" "$attempt" >> "$HAND"
done
printf '\nWITHDRAWN %s: no sign-in completed; the run continues and the t1 case is recorded as blocked.\n' "$(date -u +%H:%MZ)" >> "$HAND"
echo "$(date -u +%FT%TZ) NOT signed in" >> "$OUT/signin.txt"
ag "cd /tmp; sudo -n -u $LANE -H /usr/bin/touch /Users/$LANE/runs/$RUN/evidence/t1-not-signed-in"
L signin-stop > /dev/null 2>&1
exit 1
