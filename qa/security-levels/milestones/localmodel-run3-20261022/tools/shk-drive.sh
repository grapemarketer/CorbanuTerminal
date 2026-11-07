#!/bin/bash
# shk-drive.sh RUN PREFIX MAXSEC: answer prompts with the one-time option (Cancel for close_agent_v1), saving each prompt screen.
R=$1; P=$2; MAX=${3:-900}; T="./shk.sh $R tui"; n=0; end=$(( $(date +%s) + MAX ))
while [ $(date +%s) -lt $end ]; do
  s=$($T screen "$P-poll")
  if echo "$s" | grep -qE '^ *› 1\. '; then
    n=$((n+1)); echo "$s" > shakedown/scr/$P-prompt-$n.txt; echo "== prompt $n $(date -u +%T)"; echo "$s" | grep -E 'Would you like|Allow `|^\s+\$ |Reason|Security level' | head -6
    if echo "$s" | grep -q 'Allow `close_agent_v1`'; then $T key Down >/dev/null; sleep 0.3; fi
    $T key Enter >/dev/null; sleep 4; continue
  fi
  if echo "$s" | grep -qE 'Worked for' && ! echo "$s" | grep -qE 'esc to interrupt|Working'; then echo "$s" > shakedown/scr/$P-end.txt; echo "== ended $(date -u +%T)"; exit 0; fi
  sleep 20
done
echo "== max time"; $T screen "$P-max" > shakedown/scr/$P-max.txt
