# Upstream sync records

Program: [upstreamSyncPlan.md](../upstreamSyncPlan.md) (#1532). Sprint S0: #1627. Tooling:
[scripts/upstream_sync](../../../scripts/upstream_sync/README.md).

## S0 status (2026-11-07)

Target `rust-v0.167.0` (D303). The sync branch `sync/upstream-rust-v0.167.0` was cut from `main` `781b935f` and the
skeleton merge is up as #1690 into it.

| S0 item | State |
|---|---|
| Scripts in `scripts/upstream_sync/` | done (#1643) |
| Sync branch | `sync/upstream-rust-v0.167.0` at `781b935ff783`; ruleset `upstream-sync branches (sync/**)`: no deletion, no force-push, changes by PR (merge commits), 0 approvals, no code-owner requirement |
| Tier 0 resolutions | re-prepared against `781b935f` (5 workflow files changed on `main` since `15ab4699`; each keeps ours as before); D317 and D316 applied |
| Skeleton commit | `1d2e8ca07d`, parents `781b935f` + `086ccc83` (verified by the script), run on glitch and pushed from the Mac as a bundle; 771 files committed with markers |
| Ledger | `ledger-rust-v0.167.0.tsv` on the sync branch: 1,570 data rows; 915 conflicted paths exactly once (content 813, modify/delete 63, file-location 32, add/add 7), 561 `security-review`, 78 `ci-review`, 16 `migration`; 53 rows resolved in the skeleton (52 Tier 0 plus the `r2-release.yml` `ci-review` row) |
| `cargo metadata --locked`, `bazel mod deps`, workflows parse | pass (glitch); actionlint on the Mac adds only two shellcheck infos in `sync-package-ci.yml` |
| SG1 | cargo-deny on the skeleton tree: advisories, bans, licenses, sources ok. Package diff vs `main`: 1,153 → 1,153 third-party crates (44 added, 44 removed, 83 changed); new git sources `CorbanuCore/rusty_v8`, `hyperium/h3`, `microsoft/mxc`; no personal fork |
| `rusty_v8` (D317) | [`CorbanuCore/rusty_v8`](rusty_v8-provenance.md) at `350c5a85`, refs protected; archive SHA-256 unchanged |
| `codex-voice-host` (D316) | `voice-host` in the Cargo workspace `exclude` list and in `.bazelignore`, so neither Cargo nor Bazel lanes build it |
| CI mode and CODEOWNERS | applied by the skeleton command (commits `0def6a25`, `0ae27125`); CODEOWNERS is request-only until D318. #1690 also carries `a6ebe265` (stale `code-mode-runtime` feature exception dropped from `verify_cargo_workspace_manifests.py`, Tier 0) and `c14aaa86` (review fixes: `run-bazel-ci.sh` mode 100755, `.bazelignore` for `voice-host`, CODEOWNERS covers the dependency-policy files, unused `juberti-oai/rust-sdks` source and duplicate ignores dropped) |
| Governed records (D304) | [active plan, slot 2](../../plans/active/upstream-sync-rust-v0.167.0.md); sprints PF-85-S00..S06 |

Known red lanes on sync PRs until their slices land (expected; a red lane on a slice whose own paths are
marker-free is real): `Blob size policy` (`tui/src/bottom_pane/chat_composer.rs` is 548 KiB with markers, S4b);
`repo-checks` (markers in `scripts/codex_package`, `scripts/install`, S1d; upstream's new release workflows fail
our cache-save and schema-watch policy tests and `test_verify_pypi_release` needs `packaging`, `ci-review` rows,
S4); `corbanu-terminal-ci` and the `v8-canary` builds (crates with markers). Upstream's new
`python-sdk-cli-release.yml` (`workflow_run` on `rust-release`) and `python-sdk-release.yml` (`python-v*` tags)
are inert until they reach `main`; S4 decides whether we keep them.

## Ledger previews (`main` `23be0b3883b1`; superseded by the skeleton ledger)

[ledger-preview-rust-v0.167.0.tsv](ledger-preview-rust-v0.167.0.tsv) and
[ledger-preview-rust-v0.165.0.tsv](ledger-preview-rust-v0.165.0.tsv): every conflicted path exactly once, plus the
clean-merging security-review rows and the migration rows. Columns are described in the scripts README.

| Rows | `rust-v0.165.0` | `rust-v0.167.0` |
|---|---:|---:|
| Conflicted paths | 879 | 901 |
| content / modify-delete / file-location / add-add | 779 / 62 / 32 / 6 | 799 / 63 / 32 / 7 |
| Conflicted paths with the named-security flag / footprint flag | 115 / 322 | 116 / 323 |
| Clean-merging upstream changes on security paths (S3b review), of which on named paths | 525 / 466 | 564 / 506 |
| Clean-merging upstream changes under `.github/` (`ci-review`, integrator, S4) | 77 | 78 |
| Upstream migrations to renumber (0047–0062 → 0054–0069) | 16 | 16 |
| Total rows | 1,497 | 1,559 |

`main` moved since the plan's trial (`e6376b75`): 878 → 879 and 899 → 901 conflicted files. Hunk counts in the
previews use zdiff3 (the skeleton's style) and run about 10% above the plan's default-style figures.

## Baseline (`main` `23be0b3883b1`)

All lanes that ran on this commit passed (CodeQL was cancelled; it runs on `main` and nightly, not as a gate): `blocking-ci`, `postmerge-ci` (Linux full suite 20,373 tests in 4
shards, 0 failed; core security tests 1,613 on Linux and 1,678 on macOS), `tmux smoke` (84 tests),
`corbanu-terminal-ci`. The last `windows-security-probes` run on `main` (`d874e0e1`) passed 395 tests.

Baseline on the cut SHA `781b935f` (results to be recorded on #1627): `main`'s push lanes for that commit, plus
`windows-security-probes`, `credential-boundary-canary` and both security contracts dispatched on
`sync/upstream-rust-v0.167.0` while it still pointed at `781b935f`, before the skeleton merge (runs 42354273115,
42354275358, 42354277670, 42354280028). `orchestrate-tui-matrix` has no manual trigger;
its last `main` run stands in.

## Tier 0 resolution decisions (`rust-v0.167.0`, prepared 2026-11-06)

The skeleton keeps `main`'s behaviour wherever a manifest choice could change it; upstream's version is taken where
the change is mechanical. Each choice is a ledger row the owning slice can revisit.

- **CI files** (`.github/workflows`, `actions`, `scripts` with conflicts): our whole file, plus our
  `r2-release.yml` so `rust-release.yml` still matches it. The 78 upstream changes under `.github/` that merge cleanly
  come in as they are and each has a `ci-review` ledger row (e.g. `blocking-ci` drops `bazel` from `CI required`;
  `setup-ci` enables unprivileged user namespaces on Linux, which our core-security job already does, and moves the
  DotSlash action to `meta/install-dotslash` at the same commit; `.github/dotslash-zsh-config.json` is deleted while
  our kept `rust-release-zsh.yml` still uses it, which S3b's zsh removal (D275) resolves). Upstream's CI changes (voice SDK jobs, BuildBuddy, custom
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
