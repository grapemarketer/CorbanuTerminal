#!/bin/bash
# usage: keyscan.sh DIR  -> prints only matching file names / pattern classes, never values
D="$1"
rg -l --no-messages -e 'sk-[A-Za-z0-9_-]{20,}' -e 'sk-ant-[A-Za-z0-9_-]{20,}' -e 'AKIA[0-9A-Z]{16}' -e 'ghp_[A-Za-z0-9]{30,}' -e 'github_pat_[A-Za-z0-9_]{30,}' -e 'xox[abpr]-[A-Za-z0-9-]{10,}' -e '-----BEGIN [A-Z ]*PRIVATE KEY-----' -e '[0-9a-f]{32}\.[A-Za-z0-9]{16}' "$D" | sed 's/^/PATTERN-HIT: /'
ZK="$(corbanu vault auth-helper provider/zai_api_key)" bash -c 'test -n "$ZK" && { rg -l --no-messages -F -- "$ZK" "$1" | sed "s/^/ZAI-KEY-HIT: /"; echo "zai key scan done"; } || echo "zai key unavailable"' _ "$D"
