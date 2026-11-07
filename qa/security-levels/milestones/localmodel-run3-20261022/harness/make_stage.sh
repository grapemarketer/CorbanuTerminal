#!/bin/bash
# Host: stage the candidate (private copy of the installed signed build bc1deda532)
# and the executor packet; writes h/expected.json.   usage: make_stage.sh PACKET_DIR
set -euo pipefail
Q=/Volumes/CorbanuDrive/Corbanu/.codex-work/qa-local3-20261022
PKT="$1"
ST=$Q/stage; C=$Q/candidate
[ ! -e "$ST" ] || mv "$ST" "$ST.old-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$ST"
cp -Rp "$C/package" "$ST/package"
want=$(python3 -c "import json;print(json.load(open('$C/INSTALL.json'))['corbanu_sha256'])")
got=$(shasum -a 256 "$ST/package/bin/corbanu" | cut -d' ' -f1)
[ "$want" = "$got" ] || { echo "binary digest $got != INSTALL.json $want"; exit 4; }
codesign --verify --strict "$ST/package/bin/corbanu" || { echo "candidate not validly signed"; exit 5; }
(cd "$ST/package" && find . -type f | LC_ALL=C sort | while read -r f; do
  printf '%s %s %s\n' "$(stat -f %Lp "$f")" "$(shasum -a 256 "$f" | cut -d' ' -f1)" "$f"; done) > "$ST/package-files.txt"
cp "$ST/package-files.txt" "$C/package-files.txt"
cp -Rp "$PKT" "$ST/packet"
pkt=$(shasum -a 256 "$ST/packet/design-cases.md" | cut -d' ' -f1)
printf '{"binary_sha256": "%s", "packet_sha256": "%s"}\n' "$got" "$pkt" > "$Q/h/expected.json"
(cd "$ST/packet" && find . -type f | sort | xargs shasum -a 256) > "$C/packet-files.txt"
printf 'commit %s\nversion %s\ncorbanu sha256 %s\npacket %s\nstaged %s\n' \
  "$(python3 -c "import json;print(json.load(open('$C/INSTALL.json'))['commit'])")" \
  "$("$ST/package/bin/corbanu" --version | awk '{print $2}')" "$got" "$PKT" "$(date -u +%FT%TZ)" | tee "$C/build-head.txt"
