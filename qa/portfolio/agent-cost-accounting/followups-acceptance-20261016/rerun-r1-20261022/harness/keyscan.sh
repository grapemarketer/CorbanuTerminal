#!/bin/bash
# usage: keyscan.sh DIR...  -> prints only matching file names / classes and counts, never values
for D in "$@"; do
rg -l --no-messages -uu -z -e 'sk-[A-Za-z0-9_-]{20,}' -e 'sk-ant-[A-Za-z0-9_-]{20,}' -e 'AKIA[0-9A-Z]{16}' -e 'ghp_[A-Za-z0-9]{30,}' -e '-----BEGIN [A-Z ]*PRIVATE KEY-----' -e '[0-9a-f]{32}\.[A-Za-z0-9]{16}' "$D" | sed 's/^/PATTERN-HIT: /'
for L in provider/zai_api_key openai-api-key kimi_api3; do
  K="$(corbanu vault auth-helper "$L")" bash -c 'test -n "$K" || { echo "key $1 unavailable"; exit; }; n=$(rg -l --no-messages -uu -z -F -- "$K" "$2" | tee /dev/stderr | wc -l | tr -d " "); echo "$1: $n files with the key value in $2"' _ "$L" "$D" 2>&1 | sed 's/^\//KEY-HIT-FILE: \//'
done
echo "files scanned in $D: $(find "$D" -type f | wc -l | tr -d ' ')"
done
