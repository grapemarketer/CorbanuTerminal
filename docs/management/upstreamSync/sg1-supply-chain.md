# SG1 supply-chain review (S0 preparation, 2026-11-06)

Gate SG1 of [the sync plan](../upstreamSyncPlan.md) (§6, S0). This is the preparation pass: it compares `main` at
`23be0b3883b1` with each candidate tag's own lockfiles. The merged lockfiles only exist after the skeleton commit, so
SG1 is recorded against them then (`scripts/upstream_sync/sg1_lockdiff.py <main> <skeleton>` plus cargo-deny), and
re-run at S5. Commands are in `scripts/upstream_sync/README.md`.

## Result

No blocker for either tag. The merge can satisfy our cargo-deny policy once the S0 manifest resolution makes these
changes (each listed in the ledger's Tier 0 rows):

1. `deny.toml` `allow-git` adds upstream's four new git sources (0.165.0 needs the first three):
   `hyperium/h3`, `microsoft/mxc`, `modelcontextprotocol/rust-sdk` and, at 0.167.0 only, `seanh-oai/rusty_v8`.
   All are pinned to a full revision (`required-git-spec = "rev"` stays).
2. `deny.toml` `[bans]` wrapper lists for `async-trait` and `reqwest` take the union of ours and upstream's (upstream's
   new crates are listed in its own copy).
3. The merged `Cargo.lock` must keep `main`'s newer `zstd` 0.14.3 / `zstd-safe` 8.2.0 / `zstd-sys` 2.1.2 line
   (upstream still has 0.13.3 / 7.2.4 / 2.0.16, flagged by RUSTSEC-2026-0391, -0398, -0403 and -0410) and must take
   `libz-sys` >= 1.1.31 (RUSTSEC-2026-0418, build script prints the environment). `libz-sys` is new: upstream's
   `codex-http-client` now depends on `curl` with `static-curl` (added in #55994), which pulls `curl-sys` and
   `libz-sys`.
4. Keep the `RUSTSEC-2026-0441` (`pin-utils`, unmaintained) ignore `main` already has. Upstream's own policy fails on
   it at both tags.

## cargo-deny

cargo-deny 0.19.0, the same checksum-verified binary and arguments as `.github/workflows/cargo-deny.yml`
(`--all-features check`), on glitch with Rust 1.99.1.

| Tree | Policy | Result |
|---|---|---|
| `main` `23be0b3883b1` | ours | pass |
| `rust-v0.167.0` | upstream's | fail: RUSTSEC-2026-0441 only |
| `rust-v0.167.0` | ours | fail: 6 git sources not allowed (item 1), 2 wrapper bans (item 2), 5 advisories (item 3) |
| `rust-v0.165.0` | upstream's | fail: RUSTSEC-2026-0387, -0418, -0441, -0391, -0398, -0403, -0410 |
| `rust-v0.165.0` | ours | fail: 5 git sources, 2 wrapper bans, 5 advisories |

No licence finding with either policy.

## Packages

`sg1_lockdiff.py` (third-party crates only; registry, git and URL sources):

| | `rust-v0.165.0` | `rust-v0.167.0` |
|---|---:|---:|
| Third-party crates (`main` has 1,153) | 1,168 | 1,168 |
| Crates upstream has that `main` doesn't | 132 | 132 |
| Same crate, different version or source | 129 | 129 |
| New non-registry sources | 4 | 5 |
| `pnpm-lock.yaml` packages added / removed | 24 / 8 | 24 / 8 |

The same 132 crate names are new at both tags. Notable ones: the voice stack (`gstreamer*`, `glib*`, `gio-sys`,
`gobject-sys`, `system-deps`, `alsa`/`alsa-sys`, `cpal`, `coreaudio-rs`, `opus`/`opusic-sys`, `sonora*`, `rubato`),
WebRTC (`rtc*`, `webrtc`), `curl`/`curl-sys`/`libz-sys`, `tikv-jemallocator`/`tikv-jemalloc-sys`, `rkyv`, `h3`/`h3-quinn`,
`mxc-sdk`, `hpke`, `p384` and `protoc-bin-vendored*`. The last ships prebuilt `protoc` executables for nine platforms
inside the crates, so it is the one new crate family that carries third-party binaries. It's a build dependency;
the binaries come from crates.io with the lockfile checksums.

New non-registry sources: `hyperium/h3` (`h3`, `h3-quinn`), `microsoft/mxc` (`mxc-sdk`),
`modelcontextprotocol/rust-sdk` (`rmcp`, `rmcp-macros`), and at 0.167.0 `seanh-oai/rusty_v8` (`v8`). `crossterm`
stays on `openai-oss-forks/crossterm` (already allowed); upstream moved it from `f69a4a04` (the merge base and
`main`) to `ed1cdab3`, and the merge takes that without a conflict.

`MODULE.bazel` (each tag's own file against `main`'s; the merged file differs where `main` removed a module, e.g.
`xz`): new `bazel_dep`s `bazel_lib` 3.2.2, `protobuf` 34.0.bcr.1, `rules_foreign_cc` 0.15.1,
`rules_license` 1.0.0, `rules_pkg` 1.0.1, `rules_python` 2.4.2, `windows_support` 0.2.0, `xz` 5.4.5.bcr.8; `rules_rs`
is 0.0.102 on `main` and 0.0.96 upstream (keep ours); `bazel_dep v8` is replaced by an `http_archive` of
`v8/v8` tag 15.2.124.1 plus the `rusty-v8-v152.2.0` release files; one new Visual Studio manifest URL.

## rusty_v8 at 0.167.0 (#57644)

- Cargo `[patch.crates-io] v8 = { git = "https://github.com/seanh-oai/rusty_v8.git", rev = "350c5a85…" }` and Bazel
  `http_archive v8_crate_152_2_0` with that revision's GitHub archive, `sha256 = e2e7f996…`. The archive downloaded
  today has that SHA-256.
- `seanh-oai/rusty_v8` is a personal fork of `denoland/rusty_v8`, created 2026-10-20. The pinned revision is
  2 commits ahead of `denoland/rusty_v8` `v152.2.0` and 0 behind. Both commits change only `src/cppgc.rs` (+46/−1):
  `cppgc` `Member`/`WeakMember` storage gets the alignment of its C++ representation (`MaybeUninit<u32>` or
  `MaybeUninit<u64>` instead of a byte array), plus a layout test. Same change proposed upstream as
  denoland/rusty_v8#2091 (open, by the fork's author). Reviewed: no build-script, download or FFI-surface change.
- Risk: a personal fork can disappear or be force-pushed. The pin is a full revision and Cargo records it in
  `Cargo.lock`, so a force-push can't change what we build, but deletion would break fresh builds. Mitigation
  (recommended in the S0 report): mirror the revision to a CorbanuCore-owned fork and point the patch there, or
  return to `denoland/rusty_v8` once #2091 ships.
- Cost: a crates.io package excludes git submodules, but a git dependency doesn't. With the patch, `cargo metadata`
  on a fresh `CARGO_HOME` clones the fork and its submodules (V8, ICU, highway, FP16 and others from
  chromium.googlesource.com), several GB; on the Mac it was still fetching after 20 minutes, with transient
  `early EOF` retries. Every CI runner without a warm cargo git cache pays this, and so does every contributor.
- `rust-v0.165.0` uses crates.io `v8 = "=152.2.0"` without the patch.

## V8 artifacts (`rusty-v8-v152.2.0`)

Both tags pin the same artifact set: `third_party/v8/rusty_v8_152_2_0_release_manifests.sha256` (the trust root
that `.github/actions/setup-rusty-v8` checks) and `rusty_v8_152_2_0.sha256` are identical in 0.165.0 and 0.167.0.
The release assets were uploaded on 2026-09-16. `scripts/upstream_sync/verify_rusty_v8.py` repeats the action's
checks (per-target manifest against the trust root, exactly two entries, both files against the manifest), natively
on each OS:

| Host | Target | Result |
|---|---|---|
| Mac (Darwin arm64) | `aarch64-apple-darwin` | pass (archive 24,581,993 bytes) |
| glitch (Linux x86_64) | `x86_64-unknown-linux-gnu` | pass (30,220,449 bytes) |
| glitch (Linux x86_64) | `x86_64-unknown-linux-musl` | pass (30,406,203 bytes) |
| Windows PC (Windows 11 AMD64) | `x86_64-pc-windows-msvc` | pass (41,630,799 bytes) |

`.github/actions/setup-rusty-v8` is unchanged on `main` since the merge base, so upstream's version (which sets
`RUSTY_V8_ARCHIVE` and `RUSTY_V8_SRC_BINDING_PATH` after these checks) merges in without a conflict. The plan's
"wire them through setup-rusty-v8" step is therefore making sure every cargo lane calls the action, which the Tier 0
workflow resolution does.

## New crates (0.167.0)

| Crate | What it is | Third-party additions |
|---|---|---|
| `exec-server-cli` | standalone `codex-exec-server` binary (#57589) | none: workspace dependencies only |
| `plugin-host` | plugin process host with bounded IPC (#57290, #57508) | none: workspace dependencies only (`futures`, `tokio-util`, `libc`, `serde`) |
| `message-board-mcp` | MCP adapter for message-board tools (#58341) | none new; uses the workspace `rmcp` (git source above) |

All three are upstream-only and compile in at upstream defaults (plan §6, S2/S4). How `plugin-host` confines plugin
processes is an S3b classification item, not an SG1 one.

## GStreamer for `codex-voice-host`

`codex-voice-host` (both tags) is a leaf binary: no workspace crate depends on it. On macOS, glibc Linux and MSVC
Windows it needs GStreamer 1.28 (`gstreamer` 0.25 with `v1_28`) through `pkg-config`.

- Upstream's CI doesn't install GStreamer. `rust-ci-full` runs a `cargo_voice_inputs` job per target that builds a
  private GStreamer SDK and runtime from the 11 source archives pinned in `third_party/voice/sources.json` with Bazel
  (`third_party/voice/cargo-ci/produce`); the cargo jobs then consume it (`third_party/voice/cargo-ci/setup`).
  Upstream runs this with a BuildBuddy remote cache and its own Windows runner groups.
- System packages: Homebrew has 1.28.8 (macOS dev machines can use it); Ubuntu 24.04 (glitch and the hosted
  runners) only has 1.24.2, which is too old; Windows would need GStreamer's MSVC installer, which none of our runners or the Windows PC has.
- Recommendation (decision for Travis in the S0 report): exclude `codex-voice-host` from our cargo lanes and local
  workspace checks (`--exclude codex-voice-host`) through S4, keep the crate and upstream's `third_party/voice`
  in the tree, and decide separately whether Corbanu ships voice; if it does, adopt upstream's
  `cargo_voice_inputs` job in S5 with an Actions cache instead of BuildBuddy.
