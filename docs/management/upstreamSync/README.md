# Upstream sync records

Program: [upstreamSyncPlan.md](../upstreamSyncPlan.md) (#1532). Sprint S0: #1627. Tooling:
[scripts/upstream_sync](../../../scripts/upstream_sync/README.md).

## S0 status (2026-11-06)

Preparation is done. The skeleton merge commit waits for Travis's choice of target tag (`rust-v0.165.0` under D274
or `rust-v0.167.0` as recommended); after that it is one command (`skeleton_merge.sh`, see the scripts README).

| S0 item | State |
|---|---|
| Scripts promoted to `scripts/upstream_sync/` (trial merge, stats, slice classifier, ledger, security paths, Tier 0, skeleton, CI mode, baseline, SG1) | done |
| Conflict ledger | previews for both tags at `main` `23be0b3883b1` below; the final ledger is written by the skeleton commit |
| `security_paths.txt` with the fork-patch footprint | done: 66 named pathspecs + 986 footprint files from 371 security PRs |
| Baseline | [baseline-23be0b3883b1.md](baseline-23be0b3883b1.md); re-run on the SHA the branch is cut from |
| SG1 supply-chain review | [sg1-supply-chain.md](sg1-supply-chain.md): no blocker; four manifest/lockfile items for the skeleton |
| `rusty-v8-v152.2.0` artifacts on macOS, Linux, Windows | verified (SG1 record) |
| GStreamer 1.28 for `codex-voice-host` | options recorded in SG1; decision for Travis |
| Sync branch `sync/upstream-<tag>` | ruleset for `sync/**` prepared; branch is cut when the tag is chosen (its name contains the tag) |
| CI mode, package-scoped CI, sync-branch CODEOWNERS | `ci_mode.py` and `ci_mode/sync-package-ci.yml`, applied by the skeleton command; CODEOWNERS needs the security owner's GitHub handle |
| Governed records (active plan, sprint records) | wait for the active-plan slot (decision 2 on #1532) |
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
| Clean-merging upstream changes on security paths (S3b review) | 525 | 564 |
| Upstream migrations to renumber (0047–0062 → 0054–0069) | 16 | 16 |
| Total rows | 1,420 | 1,481 |

`main` moved since the plan's trial (`e6376b75`): 878 → 879 and 899 → 901 conflicted files. Hunk counts in the
previews use zdiff3 (the skeleton's style) and run about 10% above the plan's default-style figures.

## Baseline (`main` `23be0b3883b1`)

All lanes that ran on this commit passed: `blocking-ci`, `postmerge-ci` (Linux full suite 20,373 tests in 4
shards, 0 failed; core security tests 1,613 on Linux and 1,678 on macOS), `tmux smoke` (84 tests),
`corbanu-terminal-ci`. Path-filtered lanes that didn't run here (`windows-security-probes`,
`credential-boundary-canary`, `orchestrate-tui-matrix`, security contracts) are dispatched on the cut SHA before the
skeleton; the last `windows-security-probes` run on `main` (`d874e0e1`) passed 395 tests.
