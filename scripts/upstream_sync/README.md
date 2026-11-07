# Upstream sync scripts

Tooling for the upstream OpenAI Codex sync ([plan](../../docs/management/upstreamSyncPlan.md), records in
[docs/management/upstreamSync](../../docs/management/upstreamSync/README.md)). Python 3.11+ standard library only,
plus `git`, `gh`, `cargo`, `bazel` and `actionlint` where noted. Nothing here pushes unless a flag says so.

| Script | Use |
|---|---|
| `targets.tsv`, `verify_target.sh <tag>` | Pinned commit per candidate tag (upstream tags are unsigned); every other script checks the tag through it. |
| `trial_merge.sh <tag> [ref] [out-dir]` | Throwaway worktree, `git merge --no-commit` (zdiff3), stats + slices + ledger, then abort and remove. Used for re-scoping, ledger previews and the S6 cadence. |
| `conflict_stats.py <worktree> <prefix>` | Conflict counts per area for a worktree mid-merge. Lines exclude the zdiff3 base section; hunk counts are zdiff3's (about 10% more than the default style the plan's tables used). |
| `slices.py <conflicts.json>` | The authoritative path → slice classifier (plan §5). |
| `ledger.py <worktree> <tag> <out.tsv>` | The ledger: one row per conflicted path, per clean-merging upstream change on a security path, per colliding upstream migration. |
| `footprint.py` | Rebuilds `security_paths.txt` (named paths, then the fork-patch footprint) and `security_footprint_prs.tsv` from merged security PRs (needs `gh`). |
| `tier0.py prep/save/extra/apply/check` | Prepares and checks the Tier 0 resolutions (manifests, lockfiles, CI files) the skeleton commit needs. |
| `lock_max.py <codex-rs> <tag>` | After cargo regenerates the merged `Cargo.lock` from `main`'s, raises crates to the tag's newer patch versions. |
| `skeleton_merge.sh <tag> --resolutions DIR [--security-owner @h] [--push]` | The S0 skeleton merge as one command (below). |
| `ci_mode.py apply/revert/check`, `ci_mode.py codeowners apply/revert/check` | Sync-branch CI mode and CODEOWNERS block; `ci_mode/sync-package-ci.yml` is the package-scoped workflow it adds. |
| `baseline.py <sha> [--out f.md]` | CI baseline for one `main` commit: every lane's job conclusions and test counts. |
| `sg1_lockdiff.py <old> <new>` | SG1 package comparison across `Cargo.lock`, `MODULE.bazel` and `pnpm-lock.yaml`. |
| `verify_rusty_v8.py --trusted F --version V --target T` | Downloads and checks one target's V8 release files against the in-repo trust root. |

## Ledger columns

`path kind code area slice tier owner contributor status resolution hunks lines test snap sec_named sec_footprint
note`. `kind` is `content`, `modify/delete` (deleted upstream, the skeleton keeps ours), `file-location` (added on
`main` inside a directory upstream renamed; git placed it at the new path), `add/add`, `security-review` (merged
cleanly, on a security path, reviewed diff-level in S3b), `ci-review` (an upstream change or deletion under
`.github/` that merged cleanly; integrator, S4) or `migration` (renumbering proposed in `note`). `status` is
`open` or `resolved`; `resolution` is one of `ours`, `theirs`, `merged`, `ported`, `deleted`. Slice PRs update their
rows; the integrator owns the file.

## Skeleton merge (S0 exit)

```sh
# once, per tag: prepare the Tier 0 resolutions against the main commit the branch is cut from
python3 scripts/upstream_sync/tier0.py prep rust-v0.167.0 ~/sync-tier0-0.167   # opens ../tier0-rust-v0.167.0
#   resolve the listed files in that worktree, then:
python3 scripts/upstream_sync/tier0.py save ../tier0-rust-v0.167.0 ~/sync-tier0-0.167
# then, from a clean worktree of origin/sync/upstream-<tag>:
scripts/upstream_sync/skeleton_merge.sh rust-v0.167.0 --resolutions ~/sync-tier0-0.167 \
  --security-owner @<handle> --push
```

At `rust-v0.167.0` cargo has to clone the `rusty_v8` fork with its submodules, which the Mac couldn't do (SG1). Run
the command on glitch with `--bundle FILE` instead of `--push` (and `UPSTREAM_SYNC_REPO=$PWD` when the scripts are
outside the checkout), then on the Mac `git fetch FILE HEAD:refs/heads/sync-skeleton/<tag>` and push that branch.
`--rehearse` runs the whole thing on a local branch without the origin checks.

The script refuses to use a prepared file if either side changed since it was prepared, so a `main` move between
preparation and the merge shows up as a named list of files to redo. Lockfiles are never copied: it starts from
`main`'s, regenerates `Cargo.lock` with `cargo metadata` plus `lock_max.py` (checked with `--locked`) and
`MODULE.bazel.lock` with `bazel mod deps --lockfile_mode=update` (`SKIP_BAZEL_LOCK=1` to skip), checks that no Tier 0 path is unmerged or has markers and that every workflow and
action parses (`actionlint` when installed), writes `docs/management/upstreamSync/ledger-<tag>.tsv`, commits every
other conflicted path with its markers as the two-parent merge, verifies the parents, then adds the CI-mode commit
and (with `--security-owner`) the CODEOWNERS commit. `--push` pushes to `sync-skeleton/<tag>` and prints the PR
command for the sync branch.

## Package-scoped CI on the sync branch

GitHub only runs `workflow_dispatch` workflows whose file also exists on the default branch, so the sync-only
`sync-package-ci.yml` runs on pushes instead: write `.github/sync-package-ci.json`
(`{"packages": ["codex-core"], "platforms": ["linux", "macos", "windows"], "test": true}`) on a branch cut from the
sync branch and push it as `sync-ci/<name>`.

The CI mode skips only the heavy `blocking-ci` jobs (`bazel`, `rust-ci`, `sdk`) and the whole-workspace
`tmux-smoke` and `orchestrate-tui-matrix` on PRs into `sync/**`. The security lanes (`credential-boundary-canary`,
`security-ingress-contract`, `security-platform-contract`, `windows-security-probes`) and `v8-canary` still run on
those PRs when their paths change; they exist on `main` with `workflow_dispatch`, so they can also be run on the
sync branch with `gh workflow run <file> --ref sync/upstream-<tag>`. They are expected to fail while the crates
they build still have conflict markers; a red security lane on a slice PR whose own crates are marker-free is a
real failure. Package CI tests exactly the packages listed;
to cover "codex-core and its dependencies", list them.

Skipping `rust-ci` also skips `rust-ci-core-security`, which has no manual trigger. Until the workspace compiles,
S2 runs the `codex-core` tests (security tests included) through package CI; the lane itself runs through
`gh workflow run rust-ci.yml --ref sync/upstream-<tag>` once the workspace compiles, before SG2 (plan §6, S3).
`corbanu-terminal-ci` runs on every branch push, `sync/**` and `sync-ci/**` included, and stays red until the
workspace compiles.

The CODEOWNERS block lists the named security paths plus the dependency-policy files (`codex-rs/Cargo.toml`,
`deny.toml`, `.cargo/audit.toml`, `MODULE.bazel`, `cargo-deny.yml`).

## Security paths

`security_paths.txt` is one git pathspec per line, so `git diff A B -- $(cat scripts/upstream_sync/security_paths.txt)`
works for SG3. The first block is `footprint.NAMED` (the plan's named paths plus upstream's new sandbox crates), the
rest is the footprint: files that Corbanu security PRs changed (branch prefix `sec-`, `sbx-`, `sfN-`, … or a
PF-23..PF-41 / security-surface title; 371 PRs on 2026-11-06), limited to `codex-rs/` and `scripts/`, without Markdown
and Cargo manifests. `.github/` is left out so SG3 measures code, not CI wiring.
