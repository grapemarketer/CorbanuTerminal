# CI Coverage

Corbanu Terminal CI currently runs only checks backed by available GitHub-hosted Linux
x64 runners. This is intentional: prior Mac, Windows, ARM, and self-hosted
runner jobs were failing before checkout with no executed steps, which created
red status noise without testing the code.

## Active Push/PR Coverage

- Formatting, spelling, manifest, and dependency checks.
- Rust cargo checks on Linux x64.
- Linux clippy (`rust-ci / Lint/Build — ubuntu-24.04 - x86_64-unknown-linux-gnu`):
  `cargo clippy --tests -D warnings` for the gnu dev target, so Linux-only
  lint errors fail the PR instead of landing on `main`. It runs when
  `codex-rs/` or `.github/` changes, is part of `CI results (required)`, uses
  the shared sccache/cargo-home caches (typically 6-10 minutes) and has a
  20-minute timeout. The job is `rust-ci-lint-build.yml`, shared with
  postmerge `rust-ci-full`, which also runs the musl dev and musl release legs.
  The full Linux test suite stays postmerge only.
- Windows clippy (`rust-ci / Lint/Build — windows-2022 - x86_64-pc-windows-msvc`):
  the same `cargo clippy --tests -D warnings` for the Windows msvc dev target
  (#717), so `cfg(unix)`-gated imports, helpers and `mut`s that are unused on
  Windows fail the PR instead of breaking postmerge. Same triggers, required
  status and shared job as the Linux leg. It restores the caches postmerge
  `rust-ci-full` saves for this leg (about 12 minutes warm, 15 cold; 60-minute
  timeout).
- Native Windows tests on PRs that change one of these groups of crates
  (`rust-ci / Windows native tests / Bazel test (changed crates)`): `tui`,
  `core`, `state` and `login` (#636); `windows-sandbox-rs`, `network-proxy`,
  `process-hardening` and `sandboxing`, whose Windows tests use one another
  (#1813); `exec-server` (#1813). It runs the nightly native Windows Bazel
  tests (`test-windows-native-main`), limited to the changed groups' test
  targets (`--build_tests_only`). Other PRs skip it; a change to its workflow
  (`rust-ci-windows-native-tests.yml`) runs every group, but changes to
  `.bazelrc`, other Bazel files or the crates these depend on don't (nightly
  covers those). So a `windows-sandbox-rs` change doesn't run core's Windows
  tests, which would add core's cold build. When it runs it is part of `CI results (required)`. It restores the
  Bazel repository cache that nightly saves on `main` and saves nothing. With
  no build cache it compiles cold: about 43 minutes for the #636 group, about
  as long as the Windows argument comment lint that already runs on PRs
  (90-minute timeout).
  Bazel gives the sharded core and tui unit tests its usual three attempts.
  This job and the nightly one also give `//codex-rs/login:login-unit-tests`
  one re-run (`--flaky_test_attempts`) for the intermittent #691 test. If that
  test fails both attempts again, add it to the skip list in `.bazelrc`'s
  `windows-native-tests` config.
- Argument comment lint on changed crates
  (`rust-ci / Argument comment lint - changed crates`, #636): runs the
  `/*param*/` lint, test code included, on every `codex-rs` crate whose `.rs`
  files the PR changes (`.github/scripts/argument_comment_lint_packages.py`;
  a change to the lint job itself lints the small `codex-utils-string` crate).
  It uses `tools/argument-comment-lint/run.py` (Cargo, `--all-targets`) on
  Linux and is part of `CI results (required)`. Run it locally with
  `tools/argument-comment-lint/run.py -p <crate> -- --ignore-rust-version`.
  The Windows Bazel lint below skips test targets, and the full lints run
  nightly.
- Rust nextest on Linux x64.
- SDK checks on hosted Linux x64.
- V8 canary coverage on hosted Linux x64 only.

## Actions Cache

The repository's Actions cache is limited to 10 GB and GitHub evicts the least
recently used entries beyond that. Only `main` saves caches (#636): every save
step is guarded by `github.ref == 'refs/heads/main'`, as are Swatinem
`save-if`, setup-uv `save-cache` and setup-bazel `cache-save`. Pull requests,
and workflows dispatched on other branches, restore the caches `main` saved and
save none, so PR churn can't evict them. setup-node and setup-zig can't restore
without saving, so their caches are on only for `main` (both are used only by
postmerge or nightly jobs). Where blocking-ci and postmerge run the same
clippy leg or the macOS core security tests on `main`, only postmerge
`rust-ci-full` saves the cache.

Build caches such as sccache are keyed by run ID, so every `main` run saves a
fresh copy (up to 2.1 GB each). The `cache-prune` workflow keeps only the newest
copy of each such cache: it runs after blocking-ci, postmerge-ci, nightly-ci,
tmux smoke and CodeQL finish on `main`, and hourly, and deletes older `main`
copies with `.github/scripts/prune_actions_caches.py` (`--dry-run` lists them).
It is the only job here that holds `actions: write`. Caches without a run ID in
the key (cargo-home, apt) are saved only on a key miss and aren't pruned.
`.github/scripts/test_ci_timeout_budgets.py` enforces this; a new workflow that
saves a run-ID-keyed cache on `main` must be added to `cache-prune`'s trigger
list. Check usage with
`gh api repos/CorbanuCore/CorbanuTerminal/actions/cache/usage`.

## Nightly Coverage

`nightly-ci.yml` runs daily at 09:17 UTC and on manual dispatch. It does not
block merges. Without a BuildBuddy remote cache these cold Bazel jobs take two
to five hours, so they no longer run on every PR and get the 360-minute
hosted-runner maximum:

- Bazel test (Linux gnu and musl, and native Windows for every crate), clippy,
  and release-build verification.
- Argument comment lint on Linux and macOS (Windows still runs on PRs;
  `rust-ci-full.yml` does not repeat it).
- The Bazel-built SDK job (`sdk / sdks`; the Python SDK test still runs on PRs).

CodeQL (`codeql.yml`) also runs only on pushes to `main`, daily at 08:41 UTC
and on manual dispatch, not on pull requests: its Rust analysis takes over an
hour. Alerts appear on `main`. GitHub's default code-scanning setup must stay
disabled, because it conflicts with this workflow.

Check the latest nightly run before cutting a release. To run these jobs on
every PR again, pass `cold_bazel: true` to the `bazel`, `rust-ci` and `sdk` calls in
`blocking-ci.yml` (and add a `BUILDBUDDY_API_KEY` secret so they finish in time).

## Rust toolchain bumps

See [Rust toolchain bump](rust-toolchain-bump.md) for what to update and when.

## Disabled Until Runners Exist

- macOS Rust and Bazel checks outside the manual release builder.
- Windows Rust and Bazel checks.
- Linux ARM64 Rust, Bazel, and V8 push/PR checks outside the manual release
  builder.
- Self-hosted runner jobs for unavailable Linux, macOS, Windows, and ARM64
  platform pools.

These disabled jobs were not real failing tests. They failed before checkout
because GitHub could not assign a runner.

## Restoration Requirements

Before re-enabling platform checks:

1. Confirm the runner label exists and is attached to this repository or org.
2. Confirm a trivial workflow using that exact `runs-on` value reaches checkout.
3. Re-enable one platform leg at a time.
4. Make the platform check required only after it has passed on at least one
   fresh push.

Until then, CI should stay green and honest rather than displaying checks that
never run.

Manual release builds are separate from push/PR CI. The Corbanu Terminal release
workflow uses GitHub-hosted macOS runners and the hosted `ubuntu-24.04-arm`
runner for the Linux ARM64 release artifact, so it does not depend on the
disabled self-hosted runner labels above.
