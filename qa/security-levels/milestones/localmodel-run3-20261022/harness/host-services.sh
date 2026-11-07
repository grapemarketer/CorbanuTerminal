#!/bin/bash
# Host: start (or stop) the local-model gate (127.0.0.1:11236 -> mlx-serve 127.0.0.1:11235) and the Z.AI relay
# (127.0.0.1:11237), each in the run's private tmux server. The relay's key comes from the vault at use time.
set -euo pipefail
Q=/Volumes/CorbanuDrive/Corbanu/.codex-work/qa-local3-20261022
TM=(/opt/homebrew/bin/tmux -S "$Q/tmux.sock")
mkdir -p "$Q/common/host"
case "${1:-start}" in
  start)
    "${TM[@]}" new-session -d -s qgate "exec /usr/bin/python3 $Q/h/qwen_gate.py --port 11236 --upstream-port 11235 --min-free 20 --log $Q/common/host/qwen-gate.jsonl 2>> $Q/common/host/qwen-gate.stderr"
    "${TM[@]}" new-session -d -s zrelay "ZAI_RELAY_KEY=\"\$(corbanu vault auth-helper provider/zai_api_key)\" exec /usr/bin/python3 $Q/h/host_zai_relay.py --port 11237 --log $Q/common/host/host-zai-relay.jsonl 2>> $Q/common/host/host-zai-relay.stderr"
    sleep 2; lsof -nP -iTCP:11236 -iTCP:11237 -sTCP:LISTEN | awk 'NR>1 {print $1, $2, $9}'
    ;;
  stop) "${TM[@]}" kill-session -t qgate || true; "${TM[@]}" kill-session -t zrelay || true ;;
esac
