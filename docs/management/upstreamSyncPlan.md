# Upstream OpenAI Codex sync: sprint plan

Status: active (slot 2, D304), updated 2026-11-07. Program issue: #1532. Starting point: #94 by @goodalexander (OpenAI Codex `rust-v0.147.0`
sync, closed 2026-10-29 with thanks; Task Node `task_6853a901e19885ca5aa6c75c9a44bee8`). Owner decisions on #1532:
D273 (do it now as a dedicated, owned program), D274 (target `rust-v0.165.0`, fixed at the start, after re-scoping),
D275 (follow upstream's removals; rebuild our permission and sandbox behaviour on the new path in a "needs security
sign-off" PR; keep our `AGENTS.md` and skills), D276 (afterwards, sync per upstream minor release or monthly), D277
(invite @goodalexander; done on #94).
**2026-11-07 decisions (#1532):** D303 target `rust-v0.167.0` (replaces D274); D304 active-plan slot 2 (governed
record: [`docs/plans/active/upstream-sync-rust-v0.167.0.md`](../plans/active/upstream-sync-rust-v0.167.0.md), sprints
PF-85-S00..S06); D305 monthly cadence afterwards (replaces D276; security-file changes get their own reviewed slice);
D306 48-hour merge hold on `main` after the final merge; D307 up to 8 review rounds for the S3 security PR and the
final merge PR; D316 `codex-voice-host` stays out of our lanes (workspace `exclude`); D317 `rusty_v8` from a
CorbanuCore-owned copy ([provenance](upstreamSync/rusty_v8-provenance.md)); D318 security-owner reviewer account
pending (CODEOWNERS block request-only); D319 the security owner decides `age` 0.11 and the reduced `zip` decoders in
S1, with hard backward compatibility for existing vault files and archives.

Product basis: `docs/corbanu-product-spec.md`, principle "7. **Upstream velocity:** Maintain continuous Codex parity
without removing Corbanu-specific behavior", and row 11 "Upstream Codex parity". Engineering contract:
[`docs/plans/upstream-integration.md`](../plans/upstream-integration.md) (baseline, footprint, retain/adapt/remove per
fork patch, qualification record). This file is the program plan Travis asked for; the governed plan and sprint
records are created in S0 (§7).

## 1. Goal

Merge one fixed upstream release tag into `main` with both histories preserved, keep every Corbanu feature working on
macOS, Linux and Windows, and leave a routine so the gap doesn't grow back to thousands of commits.

## 2. Re-scope (2026-11-06)

Both tags were trial-merged into `origin/main` at `e6376b759229` in throwaway worktrees on local-only branches
(`git merge --no-commit --no-ff`, then `git merge --abort` and `git worktree remove`). Merge base for both:
`413492cd6c` (2026-07-30), the same base #94 started from. Counts use git's default conflict style; "lines" are lines
between `<<<<<<<` and `>>>>>>>` without separators, so they read a little lower than #1532's 134k for the same tag.

| | `rust-v0.165.0` | `rust-v0.167.0` |
|---|---:|---:|
| GitHub release published (UTC) | 2026-10-27 20:09 | 2026-11-06 01:40 (11-05 Pacific; "Latest") |
| Tag commit (pin and verify; the tags are unsigned) | `360319d8dee62d87e2d59fc0c21805d79519cfed` | `086ccc833bc9cf01024ad2dd6af7dbd421b6bf9a` |
| Upstream commits since base | 3,807 | 4,190 |
| `main` commits since base | 6,910 | 6,910 |
| Upstream diff since base (files) | 7,847 | 8,239 |
| Conflicted files | **878** (873 on 10-29) | **899** |
| content / modify-delete / file-location / add-add | 778 / 62 / 32 / 6 | 797 / 63 / 32 / 7 |
| Conflict hunks | 2,916 | 3,060 |
| Lines in conflict | 127.6k | 135.9k |
| Test files (of which insta snapshots) | 328 (135) | 332 (136) |
| New workspace members vs `main` / members only `main` has | 38 / 24 | 42 / 24 |
| Upstream-changed files on named security paths (§5, manifests excluded; grows with the S0 footprint) that merge **without** conflict | 418 | 458 |

Between 0.165.0 and 0.167.0: 387 upstream commits, 2,181 files changed (+122k/−29k). `rust-v0.165.0` sits on a release
branch; its 4 release-only commits (GPT-6.1 Terra catalog entry, parent tool declarations in prediction forks,
`SystemRoot` for Windows sandbox helpers, release notes) have equivalents on the 0.167.0 line. Upstream has since released
0.166.0 (10-29) and has 0.168.0 alphas out.

### Conflicts by area

| Area | 0.165.0 files / hunks | 0.167.0 files / hunks |
|---|---:|---:|
| tui | 260 / 624 | 265 / 656 |
| core | 213 / 966 | 215 / 1,001 |
| other crates and tooling | 168 / 405 | 175 / 441 |
| app-server | 56 / 195 | 58 / 197 |
| config / providers / auth | 41 / 168 | 42 / 187 |
| sandboxing (linux-sandbox, network-proxy, sandboxing, execpolicy, shell-escalation) | 38 / 144 | 38 / 144 |
| schemas / locks (regenerated) | 37 / 229 | 37 / 229 |
| windows-sandbox | 20 / 64 | 20 / 64 |
| CI / workflows | 16 / 44 | 19 / 59 |
| protocol | 15 / 40 | 16 / 43 |
| exec / exec-server | 14 / 37 | 14 / 39 |

The Corbanu-only crates (vault, tasknode-session, wallet, wallet-daemon, telegram, gpu-market, product-brand,
security-level/-policy/-audit, secret-broker, provider-auth, content-security, browser-isolation, protected-state) have
**no conflicts** with either tag; what conflicts is the upstream code that calls them. The 63 modify/delete paths
(0.167.0) are: tui 21, core 16 (shell handlers, `unix_escalation`, `compact_remote`), `mcp-server` 6, `.codex` 4,
`.github` 4, `core-skills` 4, `shell-escalation` 3, `.devcontainer` 2, `AGENTS.md`, `analytics`, `app-server` 1 each.
Upstream also removed `ext/guardian` (replaced by `ext/guardian-reviewer`, `guardian-v2`, `guardian-v3`).

Hotspots (0.167.0, lines in conflict): `tui/src/app/tests.rs` 9.0k, `models-manager/models.json` 4.3k (regenerate, then
re-apply our catalog), `linux-sandbox/src/bwrap.rs` 3.8k, `core/src/session/tests.rs` 3.6k, `tui/src/lib.rs` 3.3k,
`core/src/client_tests.rs` 3.0k, `app-server/README.md` 2.6k, `core/src/session/mod.rs` 2.4k, `core/src/client.rs`
2.4k, `tui/src/app/event_dispatch.rs` 2.0k, `network-proxy/src/credential_broker.rs` 1.9k.

**Silent collision (no conflict, must be handled): state DB migrations.** `main` renumbered upstream's migrations
0040–0046 to 0046–0052 and added Corbanu migrations 0040–0045 and 0053. Upstream adds 0047–0062 (16 files) with
different contents under numbers we already use (e.g. upstream `0047_rollout_migration_state.sql` vs our
`0047_threads_name.sql`). Only `state/src/migrations.rs` conflicts; the SQL files merge silently.

### Structural changes since #1532 (0.165.0 → 0.167.0)

- **New crates:** `exec-server-cli` (standalone `codex-exec-server`, #57589), `plugin-host` (plugin process host with
  bounded IPC and a bounded V8 runtime for UI plugins, #57290/#57508), `message-board-mcp` (#58341). Guardian context
  moved to `ext/guardian-reviewer`; the head-tail buffer moved to `utils/head-tail-buffer`.
- **Exec and permissions path:** 9 more commits on `core/src/unified_exec` and 40 on `core/src/tools`, including
  "carry prepared commands through unified execution" (#57438), "managed command checkpoints requiring fresh human
  approval" (#58045), "host-inherited thread instructions and execution policies" (#58406), "preserve command cwd in
  permission and post-tool hooks" (#57203) and opt-in Unicode canonical matching for exec-policy rules (#59535, merges
  without a conflict). They change what the security port targets, not the conflict count: the sandboxing and
  windows-sandbox numbers are identical for both tags.
- **Network proxy:** scoped request-body secret substitution in MITM hooks (#57174) and masked AWS credentials for proxy
  signing (#56554) overlap our `credential_broker`.
- **Providers/auth:** runtime authentication in model requests (#57171), model-metadata origin through runtime
  resolution (#58361); `tui/src/onboarding/provider_setup.rs` newly conflicts.
- **Build:** the merge bumps `v8` from our `=150.4.0` to `=152.2.0` (both tags). At 0.167.0 `v8` is also patched to a
  pinned git revision of `seanh-oai/rusty_v8` (#57644): a new git source for cargo-deny and supply-chain review.
  Upstream re-pinned Rust from 1.99.1 to 1.95.0 (#57373); the merge base was 1.95.0 and `main` moved to 1.99.1 on its
  own, so the merge keeps our 1.99.1 without a conflict.
- **Build check on glitch (2026-11-06):** unmodified `rust-v0.167.0`, `cargo +1.99.1 check --workspace --all-targets`
  with `RUSTY_V8_ARCHIVE`/`RUSTY_V8_SRC_BINDING_PATH` set to the `ptrcomp_sandbox` assets of upstream's
  `rusty-v8-v152.2.0` release (both SHA-256 sums matched the release's `.sha256` manifest): **0 errors**, 10 warning
  instances (deprecated `fetch_update`, trailing semicolon in macro, one unused import) that `-D warnings` will need fixed. Without the
  overrides the build script 404s on `denoland/rusty_v8` (as #94 hit). `codex-voice-host` (present in both tags)
  needs system GStreamer 1.28 dev packages, which glitch lacks; it was excluded.
- Unchanged from #1532: shell tools moved onto unified exec (#39757; `tools/handlers/shell.rs`, `runtimes/shell.rs`,
  `unix_escalation.rs` deleted), zsh backend / `shell-escalation` removed (#52160), `codex mcp-server` removed
  (#42993), legacy remote compaction removed (#44273), repo `AGENTS.md`/`.codex/skills` removed (#49713), and new
  upstream crates that overlap ours (`agent-roles`, `message-board`, `runtime-agent-instance`, `mxc-sandbox`,
  `windows-sandbox-service`, `user-verification`, `workload-identity`, `guardian-context`).

### Target recommendation (decided: D303, `rust-v0.167.0`)

Travis chose **`rust-v0.167.0`** (D303) on this analysis. It costs +21 conflicted files (+2.4%), +144 hunks (+4.9%)
and +8.3k conflict lines (+6.5%), with no extra sandbox or Windows-sandbox conflicts, and it compiles on our toolchain.
Staying on 0.165.0 would have meant the first cadence sync starts 387 commits behind, with 0.166.0 and 0.167.0 already out,
and the security port gets redone against #57438/#58045 a month later. Costs of 0.167.0: the `rusty_v8` git source
(SG1, D317) and 40 more clean-merging security-path files to review. The tag is fixed by SHA at the skeleton commit;
newer releases wait for the cadence.

## 3. Principles

1. **History-preserving merge, like #94.** One two-parent merge of the pinned tag commit; it becomes an ancestor of
   `main`. No squash, no rebase, no force-push, no piecemeal cherry-picks.
2. **Fixed target.** The tag SHA is fixed at the skeleton commit and not changed mid-program.
3. **Follow upstream by default (D275).** Take upstream's removals, moves and new crates; keep upstream defaults unless
   they change a Corbanu behaviour listed below. Keep our `AGENTS.md` and skills.
4. **Keep every Corbanu feature:** providers and the model catalog (including the OpenAI-style route default in the
   `/model` connection choice), vault, Task Node, `/spawn` with the agent graph and mailbox, wallet and wallet daemon,
   GPU market, the permissions classifier and security levels, sandboxes and the secret/credential brokers, branding
   and the Corbanu home directory, Telegram, usage/accounting. §9 is the acceptance list.
5. **Permissive stays exactly today's behaviour.** Any change to Permissive, or any sandbox boundary that moves in a
   new way, stops at a PR titled "… — needs security sign-off".
6. **Single owner per path.** Every conflicted path belongs to exactly one slice in the ledger; the integrator alone
   owns manifests, lockfiles, workflow files and the ledger for the whole program (as `upstream-integration.md`
   requires for shared files). Slice owners request manifest changes; the integrator lands them one at a time.
7. **No blanket acceptance.** Snapshots are re-accepted by the owning slice after reading each diff. Generated files
   (schemas, lockfiles, `models.json`) are regenerated, never hand-merged.

## 4. Integration-branch strategy

- **Sync branch:** `sync/upstream-<tag>` on origin, cut from `main` in S0, protected (PRs only, no force-push). A
  sync-branch-only `CODEOWNERS` entry makes the security owner a required reviewer on the security paths (§5).
- **Skeleton commit (end of S0):** `git merge --no-ff --no-commit <tag SHA>` with `merge.conflictStyle=zdiff3`. The
  integrator resolves, before committing, every Cargo/Bazel manifest and lockfile and every file under
  `.github/workflows/`, `.github/actions/` and `.github/scripts/` (so `cargo metadata` works and the workflows parse).
  Everything else is committed **with** conflict markers as the two-parent merge commit; modify/delete and
  file-location paths keep our side for now. Markers are committed on purpose: it is the only way to keep one real
  merge commit while several people resolve slices in parallel. Committed with it:
  `docs/management/upstreamSync/ledger-<tag>.tsv`, one row per conflicted path (type, slice, owner, status,
  resolution: ours / theirs / merged / ported / deleted), plus rows for the clean-merging security-path files (§5) and
  for the migration renumbering.
- **The sync branch isn't a security-qualified build** until SG2 (S3b). Nobody installs or ships it; live runs use
  disposable homes only.
- **Slice PRs** target the sync branch and touch only that slice's ledger paths plus new files inside its crates. Gate
  per slice PR: no markers left in its paths (`rg -n '^(<<<<<<<|\|\|\|\|\|\|\||=======|>>>>>>>)( |$)'`); ledger rows
  set to resolved with a resolution; crate-scoped `cargo check`/`cargo test` for crates whose dependency closure is
  marker-free (others record "verified at tier exit"); one independent review to APPROVE; security-owner review for
  any security path.
- **CI mode on the sync branch:** S0 commits, on the sync branch only, a change that skips the heavy `blocking-ci`
  jobs for PRs whose base is `sync/**` until the workspace compiles (otherwise ~40 red jobs per push), and adds a
  package-scoped dispatch (Windows clippy and tests for `codex-core` and its dependencies, plus the security lanes)
  for S2 and S3b, since `rust-ci` covers the whole workspace and can't pass while tui has markers. S4 reverts all
  of it (an S5 entry criterion). The sync-branch CODEOWNERS entry stays until main merge #3 is done and is reverted as
  the last sync-branch commit before the final PR opens, and before the S5 full run and the SG3 diff, so neither
  reaches `main` and the full run tests the final head. `.github/CODEOWNERS` isn't in `security_paths.txt`, so the
  revert doesn't change the SG3 diff.
- **Main merges** happen at tier exits (end of S1, end of S2) and in S5. The integrator merges `origin/main` into the
  sync branch. For a path that still carries skeleton markers, the integrator keeps the sync side and adds a ledger
  row "re-apply `git diff <old-main>..<new-main> -- <path>`" for the path's owner; this avoids nested markers and keeps
  one owner per path. S5 entry requires every re-apply row done. New main changes to security paths (including the
  P1 security hardening plan's work) get rows for the security owner.
- **Final merge:** PR `sync/upstream-<tag>` → `main`, merged with `gh pr merge N --merge --match-head-commit <sha>`
  only when every check is green and SG2/SG3 are recorded. Before it, tag `main` as `pre-upstream-sync-<tag>`.

## 5. Tiers, slices and write sets

Compile order (crate dependency graph at 0.167.0): core's dependency closure contains 43 conflicted crates, including
the sandbox crates, `exec-server` and `app-server-protocol`. `codex-protocol` itself depends on `network-proxy` and
`http-client`; `codex-config` on `network-proxy` and `windows-sandbox-rs`. So the tiers are: **Tier 0** manifests and
CI (S0) → **Tier 1** every conflicted crate below core, sandbox crates included (S1 + S3a) → **Tier 2** core (S2 +
S3a's core paths) → **Tier 3** crates that depend on core (S4). Within Tier 1 the critical chain is network-proxy and
http-client → protocol → windows-sandbox, codex-api, otel → config → login, models-manager → rollout, secrets →
app-server-protocol.

The ledger is generated by the slice classifier written for this re-scope (kept with the trial data and promoted to
`scripts/upstream_sync/` in S0); **the classifier is authoritative** and the column below summarises it. Counts are
0.167.0 (0.165.0 in brackets).

| Slice | Tier | Write set (summary) | Files | Hunks | Corbanu features to keep |
|---|---|---|---:|---:|---|
| S0 manifests/locks/CI | 0 | every `Cargo.toml`, `Cargo.lock`, `MODULE.bazel(.lock)`, `BUILD.bazel`, `rust-toolchain.toml`, `.cargo/`, `.config/`, `deny.toml`, `patches/`, `codex-rs/.gitignore`, `.github/{workflows,actions,scripts}/` | 52 (48) | 225 | crate list, binaries, deny policy, our CI lanes and release workflows |
| S1a protocol | 1 | `protocol/`, `app-server-protocol/` (generated schemas regenerated) | 48 (47) | 119 | Corbanu protocol extensions |
| S1b config/providers/auth | 1 | `config/`, `model-provider(-info)/`, `models-manager/`, `login/`, `codex-api/`, `backend-client/`, `aws-auth/`, `keyring-store/`, `vault/`, `provider-auth/` | 40 (40) | 228 | provider catalog, OpenAI-style route default, vault-backed login, Corbanu home |
| S1c other crates below core | 1 | `state/` (incl. migration renumbering), `rollout/`, `tools/`, `hooks/`, `otel/`, `http-client/`, `rmcp-client/`, `codex-mcp/`, `core-plugins/`, `thread-store/`, `ext/{skills,extension-api}`, `utils/`, `exec-server/`, `secrets/`, `apply-patch/`, `arg0/`, removed `core-skills/`, … | 102 (102) | 246 | queue DB, Telegram wiring, skills |
| S1d tooling/repo | 1 | `scripts/`, `tools/`, `.codex/`, `AGENTS.md`, `justfile`, `codex-cli/`, `docs/`, `sdk/`, `.devcontainer/`, root files | 32 (26) | 69 | AGENTS.md, skills, packaging scripts |
| S2a core session/client/turn | 2 | `core/` except tools, agents and S3a paths | 124 (123) | 656 | session wiring, accounting, classifier call sites |
| S2b core tools (non-shell) | 2 | `core/src/tools/{registry,router,parallel,spec_plan,code_mode,handlers/<other>}` | 19 (19) | 67 | namespaced/flattened tools per provider |
| S2c core agents | 2 | `core/src/agent/`, `tools/handlers/multi_agents*`, `core/assets/agent/`, `agent-roles/` | 35 (35) | 163 | `/spawn`, agent graph, mailbox, roles |
| S3a security resolution | 1 + 2 | `linux-sandbox/`, `windows-sandbox-rs/`, `network-proxy/`, `sandboxing/`, `execpolicy/`, `process-hardening/`, `linux-pidfd-spawn/`, `shell-escalation/` (removed), Corbanu security crates; `core/src/{unified_exec,security,exec_policy*,exec.rs,spawn.rs}`, `core/src/tools/{approvals,orchestrator,sandboxing,network_approval,runtimes,handlers/{shell,unified_exec,apply_patch,request_permissions}}`, security test suites | 92 (91) | 335 | classifier, security levels, sandboxes, brokers |
| S4a tui app/lib/onboarding/status | 3 | `tui/` except `chatwidget/`, `bottom_pane/` | 136 (133) | 387 | branding, home dir, startup, provider setup, status line |
| S4b tui chatwidget/bottom_pane | 3 | `tui/src/{chatwidget,bottom_pane}/` | 128 (126) | 265 | `/vault`, `/tasknode`, `/spawn`, `/gpu`, wallet, `/model`, `/security` |
| S4c app-server | 3 | `app-server*/` | 58 (56) | 197 | catalog default, capability profiles, permission-profile resume |
| S4d exec | 3 | `exec/` (+ adopt new `exec-server-cli`) | 7 (7) | 24 | `corbanu exec` (the reviewer path) |
| S4e other crates above core | 3 | `cli/`, `chatgpt/`, `core-api/`, `ext/{image-generation,memories,web-search}`, `memories/write/`, removed `mcp-server/` | 26 (25) | 79 | `corbanu` CLI names, subcommands |

**Security paths** (`scripts/upstream_sync/security_paths.txt`, used by SG2/SG3, CODEOWNERS and the cadence) have two
layers:

- **Named paths:** everything in S3a, plus 21 conflicted files owned by other slices that carry Corbanu permission,
  secret, approval or sandbox logic: `protocol/src/permissions.rs`, `app-server-protocol/src/protocol/v2/permissions.rs`,
  `core/src/config/{permissions,network_proxy_spec,mod}.rs` (guarded dials, isolated broker, revocation fan-out,
  project Permissive tables, Moderate sandboxing), `core/src/session/mod.rs` (grants end at a security transition),
  `core/src/mcp_tool_call{,_tests}.rs` (MCP approvals after taint), `core/src/safety.rs`, `core/src/guardian/*` (3),
  `login/src/auth/manager.rs`, `secrets/src/local.rs`, `arg0/src/lib.rs`, `cli/src/debug_sandbox.rs`,
  `apply-patch/src/lib.rs`, and tui `approval_overlay.rs` (+1 snapshot), `permission_popups.rs`, `windows_sandbox.rs`.
  The owning slice resolves them; the security owner must approve.
- **Footprint:** S0 extends the list from the fork-patch footprint, i.e. the files changed by Corbanu's security
  commits (PF-23 to PF-41 sprints, security-plan PRs), not from path names alone. A keyword match over `main`'s
  commit messages touches 492 of the 899 conflicted files, so S0 curates by commit. Each footprint file gets a ledger
  flag, and its owner asks the security owner to review the hunks that come from those commits.

The 458 (418) named-path files that upstream changed and that merge cleanly get ledger rows for a diff-level security
review in S3b. The count grows in S0 for two reasons. The named set is wider in `security_paths.txt` than in the
trial (it adds upstream's new sandbox crates and all of `ext/guardian*`). And S0 adds the footprint files: those
that upstream changed and that merge cleanly get the same rows (conflicted footprint files already have a row and
carry a footprint flag). The S0 ledger previews at `main` `23be0b38` have 564 (525) such rows, 506 (466) of them on
named paths.

Shared hot files (`core/src/session/turn.rs`, `core/src/client.rs`) belong to S2a; the security owner reviews every hunk
there that touches classifier, approval or sandbox call sites.

## 6. Sprints

Sprints are about one week. Dates assume S0 starts Monday 2026-11-09; the US holiday on 2026-11-26 falls in W3, so
the plan keeps one buffer week. Roles: **integrator** (one owner for the program: skeleton, ledger, manifests, CI
files, main merges, final PR), **slice workers** (one per slice, disjoint write sets), **security owner** (S3a/S3b and
all security-path reviews), **reviewer** (independent read-only Opus 5.5 High per PR), **QA** (true-TUI runs, demo
reels, code-blind functional runs).

| Sprint | Weeks | Scope | Owner roles | Exit |
|---|---|---|---|---|
| S0 Setup and skeleton | W1 (3 days) | records, tooling, baseline, skeleton, manifests, CI files, supply chain | integrator | skeleton merge commit; ledger complete; `cargo metadata` resolves; workflows parse; SG1 |
| S1 Tier 1: below core | W1–W2 | S1a–S1d, S3a's Tier 1 crates | 4 workers + security owner | every Tier 1 crate checks and tests on macOS and Linux |
| S2 Tier 2: core | W2–W3 | S2a–S2c, S3a's core paths | 3 workers + security owner | `codex-core` compiles with tests on macOS, Linux, Windows; config schema regenerated |
| S3 Security port | W2–W4 | S3a (merged with S1/S2), S3b port | security owner + 1 worker | S3b "needs security sign-off" PR with parity matrix green; SG2 |
| S4 Tier 3: TUI, app-server, exec, CLI | W3–W4 | S4a–S4e | 5 workers + QA | workspace compiles everywhere; true-TUI QA and demo reels done |
| S5 Qualification and merge | W4–W5 | whole workspace | integrator + QA + security owner | all lanes green; qualification record; SG3; merged to `main`; canary passed |
| S6 Cadence | after merge | D305 routine | integrator | first cadence trial measured; routine documented |

### S0 Setup and skeleton (can start now)

- **Scope:** governed records (§7); promote the trial-merge scripts (now `conflict_stats.py`, `slices.py`) and `security_paths.txt`
  to `scripts/upstream_sync/`; record the baseline on the `main` SHA the skeleton is cut from (test lists and pass
  status per CI lane, so "no regression" is measurable); build the acceptance list (§9) and the fork-patch footprint
  (retain/adapt/remove per `upstream-integration.md`); verify the `rusty-v8-v152.2.0` archives on macOS, Linux and
  Windows and wire them through `.github/actions/setup-rusty-v8`; keep `codex-voice-host` out of our lanes (D316); create and protect the sync branch; skeleton
  commit; ledger; CI mode change; sync-branch CODEOWNERS.
- **Entry:** program owner named; active-plan slot 2 (D304); target `rust-v0.167.0` (D303). Met 2026-11-07.
- **Exit:** skeleton commit with two parents (`main`, pinned tag SHA, verified); ledger covers every conflicted path
  exactly once plus security-review and migration rows; `cargo metadata` succeeds; workflow files parse; SG1 recorded.
- **Security gate SG1 (supply chain):** compare third-party packages before and after across `Cargo.lock`,
  `MODULE.bazel(.lock)` and `pnpm-lock.yaml` (new crates, new git sources such as the `rusty_v8` fork at 0.167.0,
  now `CorbanuCore/rusty_v8` per D317, licences, advisories via cargo-deny). Re-run the same comparison at S5.
- **Rollback:** nothing on `main` changes; the sync branch can be abandoned or re-cut.

### S1 Tier 1: below core

- **Kept-ours manifest choices (D319):** the security owner decides `age` 0.11 (vault file format) and our reduced
  `zip` decoder set (#968) against upstream's versions. Hard requirement: existing vault files and archives keep
  working, so the S1 owner adds checked-in fixtures that must pass on the synced build whatever is decided. They are
  generated once from `main`'s code (`age` 0.11.1, our current `zip` features) and committed before any `age` or
  `zip` change, and never regenerated:
  - vault files of every kind (`local.age`, `codex_auth.age`, `mcp_oauth.age`) at scrypt work factor 16 (what
    current builds write), 20 (the decrypt limit; older builds calibrated up to it and files are re-encrypted only
    after a successful open) and one value in between, each made with a known fixture passphrase through a
    throwaway keyring or fallback store (no native Keychain; test-isolation rules apply);
  - one zip archive per compression method we accept today (stored, deflate, deflate64, bzip2) and one with a
    method we reject (it must fail with a clean error unless the security owner approves that decoder; then only its
    expected result changes, not the fixture); these stand in for the untrusted skill and plugin bundles `zip` reads.

  Tests copy each fixture to a temp dir, check the decrypted or extracted contents against stored expected values
  (not only that the file opens), and check that a vault file above work factor 16 comes back re-encrypted at 16.
  The accepted decoder set may grow only with the security owner's approval and must not shrink. Rollback (vault
  files written by the synced build open in the last released build) is tested in S5 against that release's binary,
  next to the state-DB rollback test.
- **Scope:** S1a protocol (regenerate app-server schemas with `just write-app-server-schema`); S1b config/providers/
  auth (regenerate `models.json`, re-apply the Corbanu catalog, keep the OpenAI-style route default, vault-backed login,
  Corbanu home); S1c other crates (follow upstream's `core-skills` split; **renumber upstream migrations 0047–0062
  after our 0053**, keeping every applied version stable, with a mapping table and an upgrade test from the latest
  released Corbanu state DB); S1d tooling (keep our `AGENTS.md` and skills, adopt upstream deletions we don't use);
  S3a's Tier 1 crates (network-proxy, linux-sandbox, windows-sandbox-rs, sandboxing, execpolicy: behaviour-preserving
  resolution, done by or with the security owner).
- **Entry:** S0 exit.
- **Exit:** every Tier 1 ledger row resolved; `cargo check -p` green for each Tier 1 crate and `cargo test -p` green
  for each one whose dev-dependencies are below core (`codex-linux-sandbox` and `codex-login` dev-depend on core
  test support, so their tests move to the S2 exit) on macOS (local, `-j8`) and Linux (glitch, `-j10`); Linux clippy for the sandbox crates on glitch; app-server schemas
  regenerated and checked; main merge #1 done.
- **Contributor-friendly:** S1b, S1c (except `state` migrations), S1d.

### S2 Tier 2: core

- **Scope:** S2a session, client, turn, config, state, tasks, context manager, compaction (follow upstream's removal of
  legacy remote compaction), core tests; S2b non-shell tools; S2c agents (our agent graph and mailbox stay the backend
  for `/spawn`; upstream `agent-roles`, `message-board` and `runtime-agent-instance` compile in at upstream defaults);
  S3a's core paths resolved so core compiles: follow upstream's deletion of the shell handlers, keep the classifier and
  security-level hooks wired where unified exec already has a seam, and list every behaviour not yet ported as an S3b
  ledger row.
- **Entry:** S1 exit.
- **Exit:** `cargo check -p codex-core --all-targets` clean and `cargo test -p codex-core` green, except tests listed in
  S3b rows, plus the `codex-linux-sandbox` and `codex-login` tests, on macOS and glitch; Windows via the
  package-scoped dispatch (§4); config schema regenerated
  (`just write-config-schema` runs `codex-core`); security owner approved the call sites in `turn.rs`/`client.rs`;
  main merge #2 done.
- **Contributor-friendly:** S2b and core test-suite resolution.

### S3 Security port (S3b needs security sign-off)

- **S3a** (security resolution) merges with S1 and S2 as described there.
- **S3b scope:** rebuild on unified exec what the deleted shell handlers did for us: classifier decisions, approval
  policy, security levels (Permissive exactly as today, Moderate, Aggressive), sandbox selection, vault/secret-broker
  isolation, provenance/taint; reconcile `credential_broker` with upstream's MITM secret substitution and AWS signing;
  remove `shell-escalation` and the zsh backend (D275). Classify each new upstream permission feature (managed command
  checkpoints, host-inherited execution policies, Unicode canonical matching, guardian reviews) as "upstream default,
  no Corbanu change", "kept off" or "changes Permissive → sign-off". Diff-level review of the clean-merging
  security-path files and of security-path changes brought in by the main merges (P1 security hardening).
- **Entry:** S2 exit (core compiles). Work starts then; the whole-workspace lanes below run once S4 compiles
  (order: S2 exit → S4 compile and S3b work in parallel → S3b lanes → SG2 → S3b merges → `blocking-ci` fully green).
- **Exit:** a PR into the sync branch titled "Upstream sync S3: security port onto unified exec — needs security
  sign-off", with a parity matrix (security level × tool path × platform) compared with the S0 baseline, a
  fail-before/pass-after test for every boundary that moved, and the adversarial permission cases from `AGENTS.md`'s
  functional-test model. Green: the core-security lane (via `rust-ci` dispatch), `security-platform-contract`,
  `security-ingress-contract`, `credential-boundary-canary`, `windows-security-probes` (dispatch), Linux sandbox tests
  on glitch, seatbelt tests on macOS, Windows sandbox tests on native Windows CI and on the Windows PC.
- **Security gate SG2:** Travis's sign-off on that PR, recorded against its exact head SHA. It doesn't merge into the
  sync branch before then, and S5 can't start without it.
- **Contributor role:** tests, reviews and reproduction only; implementation stays with maintainers.

### S4 Tier 3: TUI, app-server, exec, CLI

- **Scope:** S4a app/lib/onboarding/status/history (branding, home directory, startup, upstream's provider setup next
  to our provider picker); S4b chatwidget/bottom_pane (`/vault`, `/tasknode`, `/spawn`, `/gpu`, wallet, `/model` and
  the route picker, `/security`, model catalog popups); S4c app-server (catalog default, capability profiles,
  permission-profile resume); S4d `corbanu exec` and the new `exec-server-cli`; S4e `cli` (drop `mcp-server` per
  D275), `chatgpt`, `ext/*` above core. Revert the CI mode change. Re-accept snapshots after reading each diff.
- **Entry:** S2 exit. (S4 can run alongside S3b; security-path files in S4 still need security-owner approval.)
- **Exit:** workspace builds on macOS, Linux and Windows; `blocking-ci` green except the tests listed in S3b rows
  (fully green comes after S3b merges, as an S5 entry criterion); `tmux-smoke`
  and `orchestrate-tui-matrix` green; true-TUI QA in tmux (GLM 5.3 Flash, disposable home,
  `CORBANU_TEST_NO_NATIVE_KEYRING=1`) for providers and route default, `/vault`, `/tasknode`, `/spawn`, wallet, `/gpu`
  (stop before any paid step) and `/security`; demo reels per `qa/demos/README.md` for every user-visible change,
  including new upstream UI.
- **Contributor-friendly:** S4c, S4d, S4e, snapshot review.

### S5 Qualification and merge

- **Entry:** SG2 signed and S3b merged; S4 exit; `blocking-ci` fully green; CI mode change reverted (the sync-branch
  CODEOWNERS entry goes after main merge #3, before the full run and the final PR); every ledger row resolved,
  including re-apply and migration rows; repository-wide marker scan clean.
- **Scope:** main merge #3 (≤48 hours before the final merge, with a soft freeze on hotspot files); full run:
  `blocking-ci` (incl. Windows clippy `-D warnings --tests` and Windows native tests), `rust-ci-full` (Linux full
  suite, dispatch), `windows-security-probes`, `bazel`, cargo-deny, codespell, nightly lanes (dispatch); `just test` on
  macOS; Linux clippy on glitch; the Windows PC gate; release dry run of `corbanu-terminal-release`; the
  `upstream-integration.md` qualification record (baseline SHAs, footprint dispositions, adapter contract tests,
  provenance through compaction/memory/resume, credential and browser backend evidence per OS); true-TUI success,
  cancel, recovery and resume in both live repositories (TensorCash and Isometric Game); the code-blind functional
  test design and execution gate; the coding benchmark catalog if due under product-spec row 11; SG1 re-run; final
  independent review of the merge PR.
- **Security gate SG3:** `git diff <SG2-signed SHA> <final head> -- $(cat scripts/upstream_sync/security_paths.txt)`
  is empty, or Travis re-signs. `main`'s CODEOWNERS governs the final PR, so the gate is manual: the integrator
  posts the SG3 diff on the PR, and the PR needs Travis's approving review before it merges.
- **Exit:** merged to `main` with `--match-head-commit`; installed-build canary on the Mac and the Windows PC for 48
  hours before a release; upgrade record (like #94's) linked from #1532.
- **Rollback:** before the merge there's nothing to roll back. After it, a P0 regression in the canary window means a
  revert PR (`git revert -m 1 <merge>`; a later re-sync then reverts the revert); anything smaller is fixed forward.
  The runtime state migrator ignores applied versions newer than the binary, so the previous release can open the
  upgraded state DB, but only if the renumbering keeps every already-released version and checksum unchanged (an S1c
  test). Upstream's new tables and columns are then unknown to the old binary, so the canary backs up the state DB
  first and S5 tests opening the upgraded DB with the previous release. `main` holds merges during the 48-hour canary
  (except fixes for the sync) so a revert stays clean. `pre-upstream-sync-<tag>` marks the last pre-sync `main`.

### S6 Cadence (D305)

- **Scope:** a monthly job (and on demand) that trial-merges the newest stable upstream tag into `main`, posts the
  conflict table and the security-path change count on a cadence issue, and opens the next sync when due. A sync under
  ~150 conflicted files runs as one PR; any upstream change to the security paths (conflicted or not) adds a security
  slice with sign-off; anything over ~300 files gets a plan like this one.
- **Exit:** first cadence trial measured and posted; the routine is in the maintainers' docs.

## 7. Governance records (created in S0)

- This is a product initiative under `AGENTS.md` (it touches authorization and sandbox boundaries, persistent state
  and several worktrees). S0 creates the active plan `docs/plans/active/upstream-sync-<tag>.md` from
  `PLAN_TEMPLATE.md`, citing the product-spec principle and row 11 above, and one sprint record per sprint under
  `docs/sprints/current/upstream-sync/`. It holds active-plan slot 2 (D304):
  [`upstream-sync-rust-v0.167.0.md`](../plans/active/upstream-sync-rust-v0.167.0.md), sprints PF-85-S00..S06.
- The upgrade record follows `upstream-integration.md` (required upstream-touch record and qualification).
- Status goes on #1532 weekly; each sprint has its own tracking issue.

## 8. Contributors and Task Node credit

- Each sprint issue lists its slices, marked contributor-friendly or maintainer-only. Claim a slice by commenting on
  the issue; a maintainer confirms and records the owner in the ledger, so nobody duplicates work.
- Contributor PRs target the sync branch and follow the same slice gate and independent review.
- Credit: for every merged or finished-with-credit contribution we post the factual Task Node reviewer note on the PR
  (task id if named, what was done, what it led to). @goodalexander's 0.147.0 work in #94 is this program's starting
  point: its upgrade record is the template for our retained-behaviour checks. He resolved the provider/model, agent
  and app-server plumbing there, so S1b, S2c or S4c are natural fits if he'd like one.

## 9. Acceptance list (each slice owner checks theirs; S5 re-runs all)

Provider catalog and `/model` (OpenAI-style route default) · vault (`/vault`, vault-backed login, secrets never in
logs) · Task Node (`/tasknode`, encrypted session) · `/spawn`, agent graph, mailbox, pane restore · wallet and wallet
daemon socket rules · GPU market (no paid steps) · Telegram · `/usage` and developer accounting · `/security` picker
and the three levels · permissions classifier and taint/provenance · Linux, macOS and Windows sandboxes · secret and
credential brokers · browser isolation · branding, `corbanu` binary names, Corbanu home and test-home isolation ·
`corbanu exec` (reviewer path) · state DB upgrade from the latest release · release packaging.

## 10. Risks and mitigations

| Risk | Mitigation |
|---|---|
| `main` keeps moving (~180 commits since 10-29; 0.165.0 conflicts went 873 → 878) | main merges at tier exits with re-apply rows; 48-hour soft freeze on hotspot files before the final merge; merge hold during the 48-hour canary |
| Security regression in the unified-exec port | S3b as its own sign-off PR; parity matrix; fail-before/pass-after per boundary; security CI lanes; Windows PC; SG3 diff check |
| Security changes that merge cleanly go unreviewed (458 named-path files at 0.167.0; more with the footprint) | ledger rows and diff-level review in S3b; CODEOWNERS on the sync branch |
| P1 security hardening changes the same area on `main` meanwhile | its changes arrive through main merges as security-owner rows; P1 PRs that touch files upstream deleted are flagged to the integrator |
| Migration numbers collide silently | renumbering task in S1c, upgrade test from the latest release, DB backup before canary |
| 136 snapshots hide regressions | owner reads every snapshot diff; no blanket accept |
| Upstream features overlap ours (agent roles, message board, mxc/Windows sandbox service, user verification, workload identity, command checkpoints) | compile in at upstream defaults; ours stay authoritative for `/spawn` and security levels; anything that changes Permissive goes to SG2 |
| V8 152.2.0 artifacts, the `rusty_v8` git source, GStreamer for `voice-host` | verified per OS in S0; SG1; `rusty_v8` from CorbanuCore (D317); `voice-host` out of our lanes (D316) |
| New compiler warnings under `-D warnings` on 1.99.1 | 10 instances found on glitch; fixed by the owning slices |
| Windows PC offline (down since 10-27 per the handoff; owner action D212) | native Windows CI is the required lane; Windows PC sessions scheduled with Travis for S3b and S5 |
| Review budget (5 rounds per PR) on S3b and the final PR | D307: up to 8 rounds for those two PRs |
| CI cost on red slice PRs | sync-branch-only CI mode until the workspace compiles |
| Disk (Mac drive, glitch 90% used) | build dirs on CorbanuDrive only, one target dir per worker, removed after each slice |
| Branch stalls half-done | one named owner; weekly status on #1532; explicit abort if S2 slips more than a week |

## 11. Links

#94 (0.147.0 sync, @goodalexander) · #1532 (scoping report, D273–D277) · sprint issues S0–S6 (linked from #1532) ·
[`docs/plans/upstream-integration.md`](../plans/upstream-integration.md).
