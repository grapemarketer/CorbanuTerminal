# Upstream sync records

Program: [upstreamSyncPlan.md](../upstreamSyncPlan.md) (#1532). Sprint S0: #1627. Tooling:
[scripts/upstream_sync](../../../scripts/upstream_sync/README.md).

## S0 status (2026-11-06)

Preparation is done. The skeleton merge commit waits for Travis's choice of target tag (`rust-v0.165.0` under D274
or `rust-v0.167.0` as recommended); after that it is one command (`skeleton_merge.sh`, see the scripts README). For
`rust-v0.167.0` the Tier 0 resolutions are prepared and the command was rehearsed end to end; for `rust-v0.165.0`
they still have to be prepared the same way (about half a day).

| S0 item | State |
|---|---|
| Scripts promoted to `scripts/upstream_sync/` (trial merge, stats, slice classifier, ledger, security paths, Tier 0, skeleton, CI mode, baseline, SG1) | done |
| Conflict ledger | previews for both tags at `main` `23be0b3883b1` below; the final ledger is written by the skeleton commit |
| `security_paths.txt` with the fork-patch footprint | done: 66 named pathspecs + 982 footprint files from 371 security PRs |
| Baseline | [baseline-23be0b3883b1.md](baseline-23be0b3883b1.md); re-run on the SHA the branch is cut from |
| SG1 supply-chain review | [sg1-supply-chain.md](sg1-supply-chain.md): no blocker; four manifest/lockfile items for the skeleton |
| `rusty-v8-v152.2.0` artifacts on macOS, Linux, Windows | verified (SG1 record) |
| GStreamer 1.28 for `codex-voice-host` | options recorded in SG1; decision for Travis |
| Sync branch `sync/upstream-<tag>` | ruleset `upstream-sync branches (sync/**)` is active: no deletion, no force-push, changes by PR (merge commits). It requires 0 approvals and no code-owner review, so the CODEOWNERS block only requests the security owner's review; requiring it needs a reviewer account other than the PR author (decision for Travis). The branch is cut when the tag is chosen (its name contains the tag) |
| CI mode, package-scoped CI, sync-branch CODEOWNERS | `ci_mode.py` and `ci_mode/sync-package-ci.yml`, applied by the skeleton command; CODEOWNERS needs the security owner's GitHub handle |
| Governed records (active plan, sprint records) | wait for the active-plan slot (decision 2 on #1532) |
| Tier 0 resolutions (manifests, lockfiles, CI files) | `rust-v0.167.0`: prepared against `main` `15ab4699`, decisions below; `rust-v0.165.0`: not yet |
| Skeleton rehearsal (`rust-v0.167.0`, local branch on glitch, not pushed) | passed: parents `15ab4699` + `086ccc83`; 757 files committed with markers; ledger 1,554 rows; `cargo metadata --locked`, cargo-deny (merged lock, our policy) and `bazel mod deps` pass; CI-mode and CODEOWNERS commits apply |
| Skeleton two-parent merge commit | waits for the tag decision |

## Ledger previews (`main` `23be0b3883b1`)

[ledger-preview-rust-v0.167.0.tsv](ledger-preview-rust-v0.167.0.tsv) and
[ledger-preview-rust-v0.165.0.tsv](ledger-preview-rust-v0.165.0.tsv): every conflicted path exactly once, plus the
clean-merging security-review rows and the migration rows. Columns are described in the scripts README.

| Rows | `rust-v0.165.0` | `rust-v0.167.0` |
|---|---:|---:|
| Conflicted paths | 879 | 901 |
| content / modify-delete / file-location / add-add | 779 / 62 / 32 / 6 | 799 / 63 / 32 / 7 |
| Conflicted paths with the named-security flag / footprint flag | 115 / 322 | 116 / 323 |
| Clean-merging upstream changes on security paths (S3b review), of which on named paths | 525 / 466 | 564 / 506 |
| Clean-merging upstream changes to CI files (`ci-review`, integrator, S4) | 72 | 73 |
| Upstream migrations to renumber (0047–0062 → 0054–0069) | 16 | 16 |
| Total rows | 1,492 | 1,554 |

`main` moved since the plan's trial (`e6376b75`): 878 → 879 and 899 → 901 conflicted files. Hunk counts in the
previews use zdiff3 (the skeleton's style) and run about 10% above the plan's default-style figures.

## Baseline (`main` `23be0b3883b1`)

All lanes that ran on this commit passed (CodeQL was cancelled; it runs on `main` and nightly, not as a gate): `blocking-ci`, `postmerge-ci` (Linux full suite 20,373 tests in 4
shards, 0 failed; core security tests 1,613 on Linux and 1,678 on macOS), `tmux smoke` (84 tests),
`corbanu-terminal-ci`. Path-filtered lanes that didn't run here (`windows-security-probes`,
`credential-boundary-canary`, `orchestrate-tui-matrix`, security contracts) are dispatched on the cut SHA before the
skeleton; the last `windows-security-probes` run on `main` (`d874e0e1`) passed 395 tests.

## Tier 0 resolution decisions (`rust-v0.167.0`, prepared 2026-11-06)

The skeleton keeps `main`'s behaviour wherever a manifest choice could change it; upstream's version is taken where
the change is mechanical. Each choice is a ledger row the owning slice can revisit.

- **CI files** (`.github/workflows`, `actions`, `scripts` with conflicts): our whole file, plus our
  `r2-release.yml` so `rust-release.yml` still matches it. The 73 upstream CI changes that merge cleanly come in as
  they are and each has a `ci-review` ledger row (e.g. `blocking-ci` drops `bazel` from `CI required`; `setup-ci`
  enables unprivileged user namespaces on Linux, which our core-security job already does, and moves the DotSlash
  action to `meta/install-dotslash`). Upstream's CI changes (voice SDK jobs, BuildBuddy, custom
  runners, nightly 2026-08-20, dylint 6.1.0) are ported by the integrator in S4 where the merged code needs them.
  `tools/argument-comment-lint/Cargo.{toml,lock}` stay ours for the same reason.
- **Workspace:** union of members and dependencies; version stays `0.1.48`, Rust 1.99.1. `mcp-server` and
  `shell-escalation` leave the workspace, following upstream's removal (D275): upstream deleted their sources, so
  only our modified files remain, as ledger rows for S4e and S3b. `codex-core` drops the `codex-write-config-schema`
  binary (upstream moved it to `codex-config-schema`) and keeps the `developer-accounting` feature.
- **Kept ours (owner decides in its slice):** `age` 0.11 (vault file format; S1b/S1c), `ctor` 1.x, the `hyper =1.8.1`
  pin, and `zip` with our reduced decoder set (#968); upstream adds `aes-crypto`, `lzma`, `xz` and `zstd`, which
  is a security-owner call in S1c. `zstd` stays 0.14.3.
- **Taken from upstream:** `v8 =152.2.0` with the fork patch, `deno_core_icudata` 0.78, `windows-sys` 0.61 from the
  workspace (union of features) in `core`, `uds`, `network-proxy`, `mxc-sdk`, `syntect`/`two-face` features,
  `crossterm` revision.
- **Bazel:** `rules_rs` stays 0.0.102 with our rules_rust patch set and toolchain versions; upstream's new modules
  (`rules_foreign_cc`, `rules_python`, voice toolchains) and `MSVC_RUSTC_FLAGS` are added.
- **cargo-deny:** advisory ignores are exactly `main`'s; two (RUSTSEC-2024-0320, RUSTSEC-2026-0173) were dropped by
  the clean merge and are restored, and upstream's zstd/libz-sys ignores aren't needed with the merged lock.
- **Lockfiles:** regenerated from `main`'s, then raised to the tag's newer patch versions (`lock_max.py`: 17 crates,
  including `quinn-proto` 0.11.18 and `webpki-root-certs` 1.0.10, which clear RUSTSEC-2026-0185/-0338/-0339/-0340/-0342
  and -0383 that the new `h3-quinn` dependency would otherwise bring in).

The prepared files live outside the repository (`.codex-work/sync-s0/tier0-rust-v0.167.0/` on the Mac and the same
directory on glitch). Run the skeleton on glitch: the Mac couldn't fetch the `rusty_v8` fork's submodules from
chromium.googlesource.com (SG1), so the Mac fetches glitch's bundle and pushes.
