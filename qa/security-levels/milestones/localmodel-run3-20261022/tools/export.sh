#!/bin/bash
# qa-local3 (from qwen-rerun2): export one group's run and its clone's common folder into the record (as qwen-1052 did):
# pty.raw, executor events and mediator logs gzipped; each fixture's .git renamed dot-git; product homes and product
# logs not exported; a result.md written under work/evidence is kept as work-result.md.
#   usage: export.sh GROUP [RUN_DIR_NAME DEST_NAME COMMON_NAME]   (replays: give the run folder and destination names)
set -euo pipefail
Q=/Volumes/CorbanuDrive/Corbanu/.codex-work/qa-local3-20261022
R=/Volumes/CorbanuDrive/Corbanu/worktrees/qa-local3/qa/security-levels/milestones/localmodel-run3-20261022
G="$1"
src=$(ls -d "$Q"/runs/${2:-agms-"$G"-agmx*})
D=${3:-$G}; CN=${4:-$G}
dst="$R/runs/$D"; [ ! -e "$dst" ] || rm -r "$dst" < /dev/null
mkdir -p "$dst/collected"
for f in "$src"/*; do
  b=$(basename "$f")
  case "$b" in
    collected|collected.tar) ;;
    executor-events.jsonl|mediator-at-end.jsonl) gzip -9 -c "$f" > "$dst/$b.gz" ;;
    *) cp -p "$f" "$dst/" ;;
  esac
done
cp -Rp "$src/collected/evidence" "$dst/collected/evidence"
gzip -9 "$dst/collected/evidence/pty.raw"
cp -Rp "$src/collected/fixture" "$dst/collected/fixture"
[ ! -d "$dst/collected/fixture/workspace/.git" ] || mv "$dst/collected/fixture/workspace/.git" "$dst/collected/fixture/workspace/dot-git"
[ ! -f "$src/collected/work/evidence/result.md" ] || cp -p "$src/collected/work/evidence/result.md" "$dst/collected/work-result.md"
c="$R/common/$CN"; [ ! -e "$c" ] || rm -r "$c" < /dev/null
mkdir -p "$c"
for f in "$Q/common/$CN"/*; do
  b=$(basename "$f")
  case "$b" in
    vm-ip.txt) ;;
    mediator-final.jsonl) gzip -9 -c "$f" > "$c/$b.gz" ;;
    *) cp -Rp "$f" "$c/" ;;
  esac
done
echo "exported $G: $(du -sh "$dst" | cut -f1) run, $(du -sh "$c" | cut -f1) common"
