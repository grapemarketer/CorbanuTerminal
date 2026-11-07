#!/bin/bash
# usage: aggsetup.sh <session> <bin> <rundir-name>  — start, switch to Aggressive, restart
. /Volumes/CorbanuDrive/Corbanu/.codex-work/tui-qa2/env.sh
S=$1; BIN=$2; N=$3; T="tmux -L tuiqa2"
$T new-session -d -s "$S" -x 150 -y 45 "$W/launch.sh $BIN $W/runs/$N zai-agg; sleep 900"
sleep 15
$T send-keys -t "$S" -l '/security'; sleep 1.5; $T send-keys -t "$S" Enter; sleep 4
$T send-keys -t "$S" Down; sleep 0.5; $T send-keys -t "$S" Down; sleep 1; $T send-keys -t "$S" Enter; sleep 4
$T send-keys -t "$S" Enter; sleep 4
$T capture-pane -p -t "$S" | grep -q 'Saved: Aggressive' && echo SAVED || echo NOT-SAVED
$T send-keys -t "$S" r; sleep 18
$T send-keys -t "$S" -l '/security'; sleep 1.5; $T send-keys -t "$S" Enter; sleep 4
$T capture-pane -p -t "$S" | grep 'Active in this session'
$T send-keys -t "$S" Escape; sleep 1
