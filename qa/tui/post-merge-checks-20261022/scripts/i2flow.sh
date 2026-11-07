#!/bin/bash
# usage: i2flow.sh <session> <bin> <label> [SKIP_REOPEN=1 in env]
. /Volumes/CorbanuDrive/Corbanu/.codex-work/tui-qa2/env.sh
S=$1; BIN=$2; L=$3; E=$W/evidence/i2; T="tmux -L tuiqa2"
cap() { $T capture-pane -p -t "$S" > "$E/$L-$1.txt"; }
echo "== $L start $(date -u +%H:%M:%SZ)" >> "$E/${MOCKLOG:-mock-requests.log}"
$T new-session -d -s "$S" -x 120 -y 40 "${SKIP_REOPEN:+SKIP_REOPEN=1 }$W/launch.sh $BIN $W/runs/i2-$L mock; sleep 600"
sleep 12; cap 00-start
$T send-keys -t "$S" -l 'first message alpha'; sleep 1; $T send-keys -t "$S" Enter; sleep 6; cap 01-first-reply
$T send-keys -t "$S" Escape; sleep 0.3; $T send-keys -t "$S" Escape; sleep 2; cap 02-esc-esc
$T send-keys -t "$S" Enter; sleep 4; cap 03-editing
$T send-keys -t "$S" -l ' edited'; sleep 1; $T send-keys -t "$S" Enter; sleep 6; cap 04-edited-reply
$T send-keys -t "$S" -l '/cost'; sleep 1; $T send-keys -t "$S" Enter; sleep 4; cap 05-cost
echo "== $L end $(date -u +%H:%M:%SZ)" >> "$E/${MOCKLOG:-mock-requests.log}"
