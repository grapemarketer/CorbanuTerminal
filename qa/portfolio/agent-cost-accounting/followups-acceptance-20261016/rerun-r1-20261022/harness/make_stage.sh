#!/bin/bash
# acct-rerun host: stage the candidate (this run's private signed build of main 63c9daca26, candidate/corbanu) and the
# R1 tester packet. Package = acc-tart1's package (package-base: codex-code-mode-host, codex-resources,
# codex-package.json) with bin/corbanu replaced, as in pf84-rerun. From acc-tart1's make_stage.sh.
set -euo pipefail
W=/Volumes/CorbanuDrive/Corbanu/.codex-work/acct-rerun-20261022
COMMIT=63c9daca2629fe216d3887c53399a6fb7a75a334
ST=$W/stage; C=$W/candidate
[ ! -e "$ST" ] || mv "$ST" "$ST.old-$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$ST"
cp -Rp "$W/package-base" "$ST/package"
cp -p "$C/corbanu" "$ST/package/bin/corbanu"
codesign --verify --strict "$ST/package/bin/corbanu" || { echo "candidate not validly signed"; exit 5; }
got=$(shasum -a 256 "$ST/package/bin/corbanu" | cut -d' ' -f1)
(cd "$ST/package" && find . -type f | sort | xargs shasum -a 256) > "$ST/package-files.txt"
cp "$ST/package-files.txt" "$C/package-files.txt"
codesign -dvv "$ST/package/bin/corbanu" > "$C/codesign.txt" 2>&1 || true
grep -c CORBANU_DEVELOPER_ACCOUNTING_NOT_FOR_DISTRIBUTION "$ST/package/bin/corbanu" > "$C/developer-marker-count.txt" || true
version=$(env -u CORBANU_HOME -u CODEX_HOME CORBANU_TEST_NO_NATIVE_KEYRING=1 HOME=/tmp "$ST/package/bin/corbanu" --version | awk '{print $2}')
python3 "$W/tools/make_packet_r1.py" "$W" "$ST/packet" "$got" "$version" | tee "$C/packet-build.txt"
dc=$(shasum -a 256 "$ST/packet/design-cases.md" | cut -d' ' -f1)
am=$(shasum -a 256 "$ST/packet/amendment-r1.md" | cut -d' ' -f1)
printf '{"binary_sha256": "%s", "packet_sha256": "%s", "amendment_sha256": "%s"}\n' "$got" "$dc" "$am" > "$W/h/expected.json"
(cd "$ST/packet" && find . -type f | sort | xargs shasum -a 256) > "$C/packet-files.txt"
printf 'commit %s\nversion %s\ncorbanu sha256 %s\nstaged %s\n' "$COMMIT" "$version" "$got" "$(date -u +%FT%TZ)" | tee "$C/build-head.txt"
