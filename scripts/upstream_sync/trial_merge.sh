#!/usr/bin/env bash
# Trial-merge an upstream tag into a ref in a throwaway worktree, write conflict stats and a ledger, then abort.
# Nothing is committed or pushed. Used for re-scoping, S0 ledger previews and the S6 cadence.
#
# Usage: trial_merge.sh <tag> [ref=origin/main] [out-dir=.]
set -euo pipefail

tag=${1:?usage: trial_merge.sh <tag> [ref] [out-dir]}
ref=${2:-origin/main}
out=$(cd "${3:-.}" && pwd)
here=$(cd "$(dirname "$0")" && pwd)
repo=$(git -C "$here" rev-parse --show-toplevel)

tag_sha=$("$here/verify_target.sh" "$tag")
ref_sha=$(git -C "$repo" rev-parse "$ref^{commit}")
wt="$(dirname "$repo")/trial-${tag}-${ref_sha:0:10}-$$"

git -C "$repo" worktree add --quiet --detach "$wt" "$ref_sha"
cleanup() {
  git -C "$wt" merge --abort >/dev/null 2>&1 || true
  git -C "$repo" worktree remove "$wt" || echo "leftover worktree: $wt" >&2
}
trap cleanup EXIT

set +e
git -C "$wt" -c merge.conflictStyle=zdiff3 -c merge.renameLimit=0 merge --no-commit --no-ff "$tag_sha" \
  > "$out/merge-$tag.log" 2>&1
rc=$?
set -e
echo "merge exit $rc (1 = conflicts); log: $out/merge-$tag.log"
echo "ref $ref_sha tag $tag $tag_sha merge-base $(git -C "$wt" merge-base HEAD "$tag_sha")"
python3 "$here/conflict_stats.py" "$wt" "$out/conflicts-$tag" | tee "$out/stats-$tag.txt"
python3 "$here/slices.py" "$out/conflicts-$tag.json" | tee -a "$out/stats-$tag.txt"
python3 "$here/ledger.py" "$wt" "$tag" "$out/ledger-$tag.tsv"
