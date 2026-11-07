#!/bin/bash
# acct-rerun2 key scan: pattern classes, then the exact values of the vault labels used or historically used by this
# record; prints only file names / counts, never values. usage: keyscan-r1.sh DIR
D="$1"
echo "key scan $(date -u +%FT%TZ) (acct-rerun2): pattern classes, then exact values from vault labels; names/counts only"
rg -l --no-messages -uu -z -e 'sk-[A-Za-z0-9_-]{20,}' -e 'sk-ant-[A-Za-z0-9_-]{20,}' -e 'AKIA[0-9A-Z]{16}' -e 'ghp_[A-Za-z0-9]{30,}' \
  -e '-----BEGIN [A-Z ]*PRIVATE KEY-----' -e '[0-9a-f]{32}\.[A-Za-z0-9]{16}' "$D" | sed 's/^/PATTERN-HIT: /'
for L in zai_key_3 openai-api-key provider/zai_api_key kimi_api3; do
  K="$(/usr/bin/env -u CORBANU_HOME -u CODEX_HOME "$HOME/.local/bin/corbanu" vault auth-helper "$L" 2>/dev/null)" bash -c '
    test -n "$K" || { echo "$1: label unavailable"; exit; }
    n=$(rg -l --no-messages -uu -z -F -- "$K" "$2" | sed "s/^/KEY-HIT-FILE: /" | tee /dev/stderr | wc -l | tr -d " ")
    echo "$1: $n files with the key value in $2"' _ "$L" "$D" 2>&1
done
echo "files scanned in $D: $(find "$D" -type f | wc -l | tr -d ' ')"
