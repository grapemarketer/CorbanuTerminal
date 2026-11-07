# `rusty_v8` provenance (D317)

Upstream `rust-v0.167.0` patches the `v8` crate to a git revision of a personal fork, `seanh-oai/rusty_v8` (#57644).
Travis decided (D317, #1532, 2026-11-07) that `main` must never reference a personal fork, so the sync points the
patch at a CorbanuCore-owned copy of the same revision.

| Item | Value |
| --- | --- |
| Copy | [`CorbanuCore/rusty_v8`](https://github.com/CorbanuCore/rusty_v8), a GitHub fork of `denoland/rusty_v8` created 2026-11-07 |
| Pinned revision | `350c5a859a9b3c84ab8701b3ad51e3b7699b30c0` (tree `93a3f41c6a002749736281380842ba93a5e20151`) |
| Refs holding it | branch `corbanu/v152.2.0-cppgc-align`, annotated tag `corbanu-v152.2.0-cppgc-align` |
| Ref protection | rulesets on `refs/heads/corbanu/**` and `refs/tags/corbanu-*`: no deletion, no update, no force-push; Actions and issues off |
| Source | `seanh-oai/rusty_v8@350c5a85`, itself a fork of `denoland/rusty_v8` |
| Content vs `denoland/rusty_v8` `v152.2.0` (`2768994f`) | 2 commits, `src/cppgc.rs` only (+46/−1): `981143af` "align cppgc Member and WeakMember storage to their C++ representation" and `350c5a85` "simplify opaque cppgc member storage and layout regression"; the same change is proposed upstream as denoland/rusty_v8#2091 |
| Submodules | pins identical to `v152.2.0` (`git diff v152.2.0 350c5a85` shows only `src/cppgc.rs`); URLs are denoland's and chromium.googlesource.com's own (`denoland/v8`, `denoland/chromium_build`, `chromium/src/...`), so no personal fork is involved and no mirror is needed |
| Cargo | `codex-rs/Cargo.toml` `[patch.crates-io] v8 = { git = "https://github.com/CorbanuCore/rusty_v8.git", rev = "350c5a85…" }`; `deny.toml` `allow-git` lists the CorbanuCore URL instead of the personal fork |
| Bazel | `MODULE.bazel` `v8_crate_152_2_0` downloads `CorbanuCore/rusty_v8/archive/350c5a85….tar.gz`; its SHA-256 `e2e7f9961af59dbb749a60ac0a3c34c9680f6b86dd04277f2715356b5a52feab` is byte-identical to the personal fork's archive and to upstream's pin (checked 2026-11-07) |
| Prebuilt V8 | unchanged: `denoland/rusty_v8` release `v152.2.0` archives, checked against `.sha256` manifests on macOS, Linux and Windows ([SG1](sg1-supply-chain.md)) |

Verification on 2026-11-07: the commit SHA and tree fetched from `seanh-oai/rusty_v8` and from the CorbanuCore copy
match; the skeleton's regenerated `Cargo.lock` resolves `v8 152.2.0` from
`git+https://github.com/CorbanuCore/rusty_v8.git?rev=350c5a85…`; `cargo deny --locked check` passes on the skeleton
tree (advisories, bans, licenses, sources).

When denoland/rusty_v8#2091 is released, switch the patch back to the release (or drop it if upstream Codex does)
in a cadence sync. To re-verify: `git fetch https://github.com/CorbanuCore/rusty_v8.git corbanu-v152.2.0-cppgc-align`
and compare `rev-parse` of the commit and its tree with the values above.
