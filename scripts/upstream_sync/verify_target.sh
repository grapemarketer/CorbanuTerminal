#!/usr/bin/env bash
# Resolve an upstream tag and check it against the pinned commit in targets.tsv (upstream tags are unsigned).
# Prints the pinned commit SHA; exits non-zero if the tag is unknown, missing locally, or points elsewhere.
#
# Usage: verify_target.sh <tag>
set -euo pipefail

tag=${1:?usage: verify_target.sh <tag>}
here=$(cd "$(dirname "$0")" && pwd)
repo=${UPSTREAM_SYNC_REPO:-$here}  # any path inside the repository
pinned=$(awk -F'\t' -v t="$tag" '$1 == t { print $2 }' "$here/targets.tsv")
if [ -z "$pinned" ]; then
  echo "error: $tag is not in targets.tsv" >&2
  exit 2
fi
if ! git -C "$repo" cat-file -e "$pinned^{commit}" 2>/dev/null; then
  git -C "$repo" fetch --quiet upstream "refs/tags/$tag:refs/tags/$tag" >&2 || true
fi
actual=$(git -C "$repo" rev-parse --verify --quiet "refs/tags/$tag^{commit}" || true)
if [ "$actual" != "$pinned" ]; then
  echo "error: tag $tag resolves to '${actual:-nothing}', pinned $pinned" >&2
  exit 3
fi
echo "$pinned"
