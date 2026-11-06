#!/usr/bin/env bash
# S0 skeleton merge (docs/management/upstreamSyncPlan.md section 4), as one command.
#
# Usage: skeleton_merge.sh <tag> --resolutions DIR [--security-owner @handle] [--push]
#
# Run from a clean worktree whose HEAD is the tip of origin/sync/upstream-<tag>. The script:
#   1. verifies the tag against targets.tsv and merges it with --no-ff --no-commit (zdiff3);
#   2. writes docs/management/upstreamSync/ledger-<tag>.tsv, then applies the prepared Tier 0 resolutions from DIR
#      (tier0.py prep/save): each prepared file is used only if
#      both sides' blobs still match the ones it was prepared from; otherwise it stops and names the file;
#   3. regenerates codex-rs/Cargo.lock (cargo metadata) and MODULE.bazel.lock (bazel mod deps);
#   4. checks that no Tier 0 path is unmerged or carries markers, `cargo metadata` resolves and every workflow and
#      action file parses (actionlint when installed);
#   5. stages everything else with its conflict markers and commits the two-parent merge;
#   6. checks the parents are exactly (HEAD before, pinned tag SHA);
#   7. adds two ordinary commits on top: the sync-branch CI mode (ci_mode.py apply) and, with --security-owner,
#      the sync-branch CODEOWNERS block.
# With --push it pushes the commit to sync-skeleton/<tag> and prints the PR command; without it nothing leaves
# the machine. --rehearse runs everything on the current local branch without the origin checks (never pushes).
set -euo pipefail

tag=${1:?usage: skeleton_merge.sh <tag> --resolutions DIR [--security-owner @handle] [--push]}
shift
res="" push=0 owner="" rehearse=0
while [ $# -gt 0 ]; do
  case $1 in
    --resolutions) res=$(cd "$2" && pwd); shift 2 ;;
    --push) push=1; shift ;;
    --security-owner) owner=$2; shift 2 ;;
    --rehearse) rehearse=1; shift ;;
    *) echo "unknown argument $1" >&2; exit 2 ;;
  esac
done
[ -n "$res" ] || { echo "error: --resolutions DIR is required" >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)
top=$(git rev-parse --show-toplevel)
cd "$top"
branch="sync/upstream-$tag"

tag_sha=$("$here/verify_target.sh" "$tag")
head_sha=$(git rev-parse HEAD)
if [ "$rehearse" = 1 ]; then
  push=0
  echo "rehearsal on $(git branch --show-current) ($head_sha); nothing will be pushed"
else
  git fetch --quiet origin "$branch"
  [ "$head_sha" = "$(git rev-parse "origin/$branch")" ] || { echo "error: HEAD is not origin/$branch" >&2; exit 3; }
fi
[ -z "$(git status --porcelain)" ] || { echo "error: worktree not clean" >&2; exit 3; }
if git merge-base --is-ancestor "$tag_sha" HEAD; then echo "error: $tag is already merged" >&2; exit 3; fi
gitdir=$(git rev-parse --git-dir)
log="$gitdir/skeleton-merge-$tag.log"

echo "merging $tag ($tag_sha) into $branch ($head_sha); log: $log"
set +e
git -c merge.conflictStyle=zdiff3 -c merge.renameLimit=0 merge --no-ff --no-commit "$tag_sha" > "$log" 2>&1
set -e
[ -f "$gitdir/MERGE_HEAD" ] || { echo "error: merge did not start; see $log" >&2; exit 4; }

ledger="docs/management/upstreamSync/ledger-$tag.tsv"
mkdir -p "$(dirname "$ledger")"
python3 "$here/ledger.py" "$top" "$tag" "$ledger" --tier0-dir "$res"
python3 "$here/tier0.py" apply "$top" "$tag" "$res"

echo "regenerating codex-rs/Cargo.lock from the resolved manifests (cargo metadata)"
(cd codex-rs && cargo metadata --format-version 1 > /dev/null)
git add codex-rs/Cargo.lock
if [ "${SKIP_BAZEL_LOCK:-0}" != 1 ]; then
  echo "regenerating MODULE.bazel.lock (bazel mod deps --lockfile_mode=update)"
  bazel mod deps --lockfile_mode=update > /dev/null
  git add MODULE.bazel.lock
else
  echo "warning: MODULE.bazel.lock not regenerated (SKIP_BAZEL_LOCK=1)" >&2
fi
python3 "$here/tier0.py" check "$top"
git add -A
if git ls-files -u | grep -q .; then echo "error: unmerged index entries remain" >&2; exit 5; fi
git commit --quiet -F - <<EOF
Upstream sync skeleton: merge $tag into $branch

Merges upstream OpenAI Codex $tag ($tag_sha) into $branch ($head_sha).
Tier 0 (Cargo/Bazel manifests and lockfiles, .github/{workflows,actions,scripts}) is resolved.
Every other conflicted path is committed with its conflict markers on purpose and is tracked in
$ledger; slice PRs resolve them (docs/management/upstreamSyncPlan.md section 4).
EOF
parents=$(git rev-list --parents -n 1 HEAD | cut -d' ' -f2-)
[ "$parents" = "$head_sha $tag_sha" ] || { echo "error: unexpected parents: $parents" >&2; exit 6; }
skeleton=$(git rev-parse HEAD)
echo "skeleton commit $skeleton parents $parents"
python3 "$here/ci_mode.py" apply "$top"
git add -A .github
git commit --quiet -m "Upstream sync: sync-branch CI mode (reverted in S4)"
if [ -n "$owner" ]; then
  python3 "$here/ci_mode.py" codeowners apply "$top" "$owner"
  git add .github/CODEOWNERS
  git commit --quiet -m "Upstream sync: sync-branch CODEOWNERS for security paths (reverted before the final PR)"
else
  echo "note: no --security-owner; CODEOWNERS block not added" >&2
fi
if [ "$push" = 1 ]; then
  git push origin "HEAD:refs/heads/sync-skeleton/$tag"
  echo "next: gh pr create --base $branch --head sync-skeleton/$tag --title 'Upstream sync S0: skeleton merge of $tag'"
fi
