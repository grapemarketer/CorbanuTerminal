# Upstream OpenAI Codex sync: sprint plan

Status: plan, 2026-11-06. Program issue: #1532. Starting point: #94 by @goodalexander (OpenAI Codex `rust-v0.147.0`
sync, closed 2026-10-29 with thanks; Task Node `task_6853a901e19885ca5aa6c75c9a44bee8`). Owner decisions: D273 (do it
now as a dedicated, owned program), D274 (target `rust-v0.165.0`, fixed at the start, after re-scoping), D275 (follow
upstream's removals, rebuild our permission and sandbox behaviour on the new path in a "needs security sign-off" PR,
keep our `AGENTS.md` and skills), D276 (afterwards, sync per upstream minor release or monthly), D277 (invite
@goodalexander; done on #94).

## 1. Goal

Merge one fixed upstream release tag into `main` with both histories preserved, keep every Corbanu feature working on
all three platforms, and leave behind a routine so the gap never grows back to thousands of commits.

## 2. Re-scope (2026-11-06)

Both tags were trial-merged into `origin/main` at `e6376b759229` in throwaway worktrees on local-only branches
(`git merge --no-commit --no-ff`, then `git merge --abort` and `git worktree remove`). Merge base for both:
`413492cd6c` (2026-07-30), the same base #94 started from. Counts use git's default conflict style; "lines" are lines
between `<<<<<<<` and `>>>>>>>` excluding separators, so they read slightly lower than #1532's 134k for the same tag.

| | `rust-v0.165.0` | `rust-v0.167.0` |
|---|---:|---:|
| Published | 2026-10-27 | 2026-11-06 (latest stable) |
| Tag commit | `360319d8dee6` | `086ccc833bc9` |
| Upstream commits since base | 3,807 | 4,190 |
| `main` commits since base | 6,910 | 6,910 |
| Upstream diff since base (files) | 7,847 | 8,239 |
| Conflicted files | **878** (873 on 10-29) | **899** |
| content / modify-delete / file-location / add-add | 778 / 62 / 32 / 6 | 797 / 63 / 32 / 7 |
| Conflict hunks | 2,916 | 3,060 |
| Lines in conflict | 127.6k | 135.9k |
| Test files (of which insta snapshots) | 328 (135) | 332 (136) |

Between 0.165.0 and 0.167.0: 387 upstream commits, 2,181 files changed (+122k/−29k). `rust-v0.165.0` sits on a release
branch: its 4 release-only commits (GPT-6.1 Terra catalog entry, parent tool declarations in prediction forks,
`SystemRoot` for Windows sandbox helpers, release notes) have equivalents on the 0.167.0 line.

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
**no conflicts** with either tag. What conflicts is the upstream code that calls them. The only modify/delete hits on
crates we carry are the three upstream removed: `mcp-server` (6 files), `core-skills` (4), `shell-escalation` (3).

Hotspots (0.167.0, lines in conflict): `tui/src/app/tests.rs` 9.0k, `models-manager/models.json` 4.3k (regenerate,
then re-apply our catalog), `linux-sandbox/src/bwrap.rs` 3.8k, `core/src/session/tests.rs` 3.6k, `tui/src/lib.rs` 3.3k,
`core/src/client_tests.rs` 3.0k, `core/src/session/mod.rs` 2.4k, `core/src/client.rs` 2.4k,
`tui/src/app/event_dispatch.rs` 2.0k, `network-proxy/src/credential_broker.rs` 1.9k.

### Structural changes since #1532 (0.165.0 → 0.167.0)

- **New crates:** `exec-server-cli` (standalone `codex-exec-server` executable, #57589), `plugin-host` (plugin process
  host with bounded IPC and a bounded V8 runtime for UI plugins, #57290/#57508), `message-board-mcp` (#58341). Guardian
  context moved into `ext/guardian-reviewer`; the head-tail buffer moved to `utils/head-tail-buffer`.
- **Exec and permissions path:** 9 more commits on `core/src/unified_exec` and 40 on `core/src/tools`, including
  "carry prepared commands through unified execution" (#57438), "managed command checkpoints requiring fresh human
  approval" (#58045), "host-inherited thread instructions and execution policies" (#58406), "preserve command cwd in
  permission and post-tool hooks" (#57203) and opt-in Unicode canonical matching for exec-policy rules (#59535). These
  land on exactly the surface the security port (S3) rebuilds, so they change the port's target, not its size: the
  sandboxing and windows-sandbox conflict counts are identical for both tags.
- **Network proxy:** scoped request-body secret substitution in MITM hooks (#57174) and masked AWS credentials for
  proxy signing (#56554) overlap our `credential_broker`.
- **Providers/auth:** runtime authentication in model requests (#57171), model-metadata origin through runtime
  resolution (#58361); `tui/src/onboarding/provider_setup.rs` newly conflicts.
- **Build:** upstream re-pinned Rust from 1.99.1 to 1.95.0 (#57373). The merge base was already 1.95.0 and `main`
  moved to 1.99.1 on its own, so the merge keeps our 1.99.1 without a conflict. `v8` stays `=152.2.0` but is patched
  to a pinned git revision of `seanh-oai/rusty_v8` (#57644, bindings matching Bazel-built archives): a new git source
  for cargo-deny and supply-chain review.
- Unchanged from #1532: shell tools moved onto unified exec (#39757; `tools/handlers/shell.rs`, `runtimes/shell.rs`,
  `unix_escalation.rs` deleted), zsh backend / `shell-escalation` removed (#52160), `codex mcp-server` removed
  (#42993), legacy remote compaction removed (#44273), repo `AGENTS.md`/`.codex/skills` removed (#49713), 24+ new
  upstream crates, some overlapping ours (`agent-roles`, `message-board`, `runtime-agent-instance`, `mxc-sandbox`,
  `windows-sandbox-service`, `user-verification`, `workload-identity`, `guardian-context`).

### Target recommendation (decision for Travis; D274 stands until he answers)

Recommend switching the fixed target to **`rust-v0.167.0`**. It costs +21 conflicted files (+2.4%), +144 hunks (+4.9%)
and +8.3k conflict lines (+6.5%), with no extra sandbox or Windows-sandbox conflicts. Staying on 0.165.0 means the
first D276 cadence sync, right after this program, starts 387 commits behind with 0.166.0 and 0.167.0 already out,
and the security port would be redone against #57438/#58045 a month later. Costs of 0.167.0: the rusty_v8 git source
(review in S0) and the newer exec-permission features to classify in S3. Whichever tag is chosen is fixed at S0's
skeleton commit; newer releases (0.168.0 alphas are already out) wait for the cadence.

## 3. Principles

1. **History-preserving merge, like #94.** One two-parent merge of the signed upstream tag; the tag commit becomes an
   ancestor of `main`. No squash, no rebase, no force-push, no cherry-picking upstream piecemeal.
2. **Fixed target.** The tag is fixed at S0 and not changed mid-program.
3. **Follow upstream by default (D275).** Take upstream's removals, moves and new crates; keep upstream defaults unless
   they change a Corbanu behaviour listed below. Keep our `AGENTS.md` and skills.
4. **Keep every Corbanu feature:** providers and the model catalog (including the OpenAI-style route default in the
   `/model` connection choice), vault, Task Node, `/spawn` and the agent graph/mailbox, wallet and wallet daemon, GPU
   market, the permissions classifier and security levels, sandboxes and the secret/credential brokers, branding and the
   Corbanu home directory, Telegram, usage/accounting. #94's retained-behaviour table plus §8 here is the acceptance
   list.
5. **Permissive stays exactly today's behaviour.** Any change to Permissive, or any sandbox boundary that moves in a
   new way, stops at a PR titled "… — needs security sign-off".
6. **Disjoint write sets.** Every conflicted path belongs to exactly one slice (the conflict ledger, §5). A slice
   owner edits only its own paths; call-site changes in another slice go through that slice's owner.
7. **No blanket acceptance.** Snapshots are re-accepted by the owning slice after reading each diff; generated files
   (schemas, lockfiles, `models.json`) are regenerated, never hand-merged.

## 4. Integration-branch strategy

- **Sync branch:** `sync/upstream-<tag>` on origin, cut from `main` in S0, protected (PRs only, no force-push).
- **Skeleton commit (end of S0):** `git merge --no-ff --no-commit <tag>` with `merge.conflictStyle=zdiff3`; resolve the
  workspace manifests and lockfiles (S0 slice) so `cargo metadata` works; keep our side of every modify/delete path for
  now; commit everything else *with* conflict markers as the two-parent merge commit. Commit with it a ledger,
  `docs/management/upstreamSync/ledger-<tag>.tsv`: path, conflict type, slice, owner, status. Markers are committed on
  purpose: it is the only way to keep one real merge commit while several people resolve slices in parallel.
- **Slice PRs** target the sync branch, one per slice or sub-slice, and may touch only ledger paths of that slice plus
  new files inside the slice's crates. Gate per slice PR: no markers left in its paths (`rg -n '^(<<<<<<<|>>>>>>>)'`),
  crate-scoped `cargo check`/`cargo test` for crates whose dependencies are already marker-free, one independent
  review to APPROVE, ledger rows updated.
- **CI mode on the sync branch:** S0 also commits, on the sync branch only, a change that skips the heavy
  `blocking-ci` jobs for PRs whose base is `sync/**` until the workspace compiles (otherwise ~40 red jobs per push).
  S4 reverts it; "reverted" is an S5 entry criterion so it can never reach `main`.
- **Main merges:** at every sprint end the integrator merges `origin/main` into the sync branch (normal merge). New
  conflicts are assigned by the ledger's slice owners and resolved in that merge PR. The last main merge happens
  within 48 hours of the final merge.
- **Final merge:** PR `sync/upstream-<tag>` → `main`, merged with `gh pr merge N --merge --match-head-commit <sha>`
  only when every check is green and security sign-off is recorded. Before it, tag `main` as
  `pre-upstream-sync-<tag>`.

## 5. Slices and write sets

Conflicted-file counts are for 0.167.0 (0.165.0 in brackets). The ledger is generated from the trial-merge data by the
classifier in S0, so every path lands in exactly one row.

| Slice | Write set (conflicted paths) | Files | Hunks | Corbanu features to keep |
|---|---|---:|---:|---|
| S0 manifests/locks | all `Cargo.toml`, `Cargo.lock`, `MODULE.bazel(.lock)`, `BUILD.bazel`, `.cargo/`, `.config/`, `deny.toml`, `patches/` | 32 (31) | 154 | crate list, product binaries, deny policy |
| S1a protocol | `protocol/`, `app-server-protocol/` incl. generated schemas | 48 (47) | 119 | Corbanu protocol extensions |
| S1b config/providers/auth | `config/`, `model-provider(-info)/`, `models-manager/`, `login/`, `chatgpt/`, `codex-api/`, `backend-client/`, `aws-auth/`, `keyring-store/` | 42 (42) | 231 | provider catalog, OpenAI-style route default, vault-backed login, Corbanu home |
| S1c other crates | `state/`, `rollout/`, `ext/`, `cli/`, `rmcp-client/`, `otel/`, `http-client/`, `core-plugins/`, `thread-store/`, `hooks/`, `memories/`, `analytics/`, `utils/`, removed `mcp-server/`, `core-skills/`, … | 119 (118) | 309 | `corbanu` CLI names, queue DB, Telegram wiring |
| S1d CI/tooling/repo | `.github/`, `scripts/`, `tools/`, `.codex/`, `AGENTS.md`, `justfile`, `codex-cli/`, root files | 52 (43) | 140 | our CI lanes, release workflows, AGENTS.md, skills |
| S2a core session/client/turn | `core/` except tools, agents and the S3 paths | 125 (124) | 657 | session wiring, accounting, classifier call sites (cross-review by S3) |
| S2b core tools (non-shell) | `core/src/tools/{registry,router,parallel,spec_plan,code_mode,handlers/<other>}` | 19 (19) | 67 | namespaced/flattened tools per provider |
| S2c core agents | `core/src/agent/`, `tools/handlers/multi_agents*`, `core/assets/agent/`, `agent-roles/` | 35 (35) | 163 | `/spawn`, agent graph, mailbox, roles (Nazgul/Troll/Orc) |
| S3 security port | `core/src/{unified_exec,security,exec_policy*}`, `core/src/tools/{approvals,orchestrator,sandboxing,network_approval,runtimes,handlers/{shell,unified_exec,apply_patch,request_permissions}}`, security test suites, `linux-sandbox/`, `windows-sandbox-rs/`, `network-proxy/`, `sandboxing/`, `execpolicy/`, `shell-escalation/` | 92 (91) | 335 | classifier, security levels, sandboxes, credential/secret brokers |
| S4a tui app/lib/onboarding/status | `tui/` except `chatwidget/`, `bottom_pane/` | 136 (133) | 387 | branding, home dir, startup, onboarding/provider setup, status line |
| S4b tui chatwidget/bottom_pane | `tui/src/chatwidget/`, `tui/src/bottom_pane/` | 128 (126) | 265 | `/vault`, `/tasknode`, `/spawn`, `/gpu`, wallet, `/model` popups, `/security` picker |
| S4c app-server | `app-server*/` | 58 (56) | 197 | catalog default, provider capability profiles, permission-profile resume |
| S4d exec/exec-server | `exec/`, `exec-server/`, `exec-server-cli/` | 13 (13) | 36 | `corbanu exec` (reviewers depend on it) |

Shared hot files (`core/src/session/turn.rs`, `core/src/client.rs`) belong to S2a; the S3 owner reviews every hunk
there that touches classifier, approval or sandbox call sites. Snapshots belong to the slice that owns their directory.

## 6. Sprints

Sprints are about one week. Dates assume S0 starts Monday 2026-11-09; the US holiday on 2026-11-26 is inside S3/S4,
so the plan keeps one buffer week. Roles: **integrator** (one owner for the whole program: skeleton, ledger, main
merges, final PR), **slice workers** (one per slice, disjoint write sets), **security owner** (S3), **reviewer**
(independent read-only Opus 5.5 High per PR), **QA** (true-TUI tmux runs, demo reels).

| Sprint | Weeks | Scope | Owner roles | Exit |
|---|---|---|---|---|
| S0 Setup and skeleton | W1 (3 days) | tooling, ledger, sync branch, manifests, supply chain | integrator | skeleton merge commit on sync branch; `cargo metadata` resolves; cargo-deny reviewed |
| S1 Foundation | W1–W2 | S1a–S1d | 4 workers | protocol/config/other crates compile and test; schemas regenerated |
| S2 Core | W2–W3 | S2a–S2c | 3 workers | `codex-core` compiles with tests on macOS, Linux, Windows |
| S3 Security port | W2–W4 | S3 | security owner + 1 worker | "needs security sign-off" PR open with parity matrix green |
| S4 TUI, app-server, exec | W3–W4 | S4a–S4d | 4 workers + QA | workspace compiles; true-TUI QA and demo reels done |
| S5 Qualification and merge | W4–W5 | whole workspace | integrator + QA | all lanes green, sign-off recorded, merged to `main`, canary passed |
| S6 Cadence | after merge | D276 routine | integrator | first cadence trial measured; routine documented |

### S0 Setup and skeleton (start now)

- **Scope:** promote the trial-merge scripts (conflict stats, slice classifier) to `scripts/upstream_sync/`; record the
  baseline (test lists and pass status for each CI lane on the `main` SHA the skeleton is cut from, so "no regression"
  is measurable); build the acceptance list (#94 table plus features shipped since 08-08: security levels and
  `/security`, classifier, secret/credential brokers, browser isolation, developer accounting, route picker); verify
  rusty_v8 152.2.0 archives build on macOS, Linux (glitch) and Windows; create and protect the sync branch; skeleton
  commit and ledger; CI mode change; resolve S0 manifests; cargo-deny on the new lockfile (24 new upstream crates at 0.165.0,
  27 at 0.167.0, which also adds the `seanh-oai/rusty_v8` git source).
- **Entry:** owner named for the program. Target tag: D274 (`rust-v0.165.0`) unless Travis picks 0.167.0 before the
  skeleton step; the preparation work above doesn't depend on the tag.
- **Exit:** skeleton commit on the sync branch with two parents (`main`, tag); ledger covers every conflicted path
  exactly once; `cargo metadata` succeeds; supply-chain notes recorded on the S0 issue; sprint issues list owners.
- **Tests/CI:** `cargo metadata`, cargo-deny, script unit tests; nothing else compiles yet by design.
- **Security gate SG1:** supply-chain review of new dependencies and git sources (reviewer + deny policy).
- **Rollback:** delete nothing on `main`; the sync branch can be abandoned or re-cut from a newer `main`.

### S1 Foundation

- **Scope:** S1a protocol (regenerate app-server and config schemas with `just write-app-server-schema` and `just
  write-config-schema`); S1b config/providers/auth (regenerate `models.json`, then re-apply the Corbanu catalog; keep
  the OpenAI-style route default; vault-backed login and Corbanu home); S1c other crates (follow upstream removals of
  `mcp-server`, `core-skills` split; fix the callers in `cli`, `arg0`, `core-plugins`); S1d CI/tooling (keep our lanes
  and release workflows, adopt upstream's deletions where we don't use the workflow, keep `AGENTS.md` and skills).
- **Entry:** S0 exit.
- **Exit:** no markers in S1 paths; `cargo check -p` and `cargo test -p` green for protocol, app-server-protocol,
  config, model-provider(-info), models-manager, login, state, rollout and the other S1c crates whose dependency closure
  is marker-free, on macOS (local, `-j8`) and Linux (glitch, `-j10`); schemas regenerated and checked; first `main`
  merge into the sync branch done.
- **Contributor-friendly:** S1b, S1c, S1d.

### S2 Core

- **Scope:** S2a session, client, turn, config, state, tasks, context manager, compaction (follow upstream's removal of
  legacy remote compaction), core tests; S2b non-shell tools; S2c agents: our agent graph and mailbox stay the backend
  for `/spawn`; upstream `agent-roles`, `message-board` and `runtime-agent-instance` compile in at upstream defaults.
- **Entry:** S1a and S1b merged (core depends on them).
- **Exit:** `cargo check -p codex-core --all-targets` clean and `cargo test -p codex-core` green outside S3 tests on
  macOS and glitch; Windows check via a manual `rust-ci` dispatch on the sync branch (it calls the lint/build and Windows lanes); S3 owner approved the
  classifier/approval call sites in `turn.rs` and `client.rs`; `main` merged again.
- **Contributor-friendly:** S2b, core test-suite resolution.

### S3 Security port (needs security sign-off)

- **Scope:** rebuild on unified exec what the deleted shell handlers did for us: classifier decisions, approval
  policy, security levels (Permissive exactly as today, Moderate, Aggressive), sandbox selection, vault/secret-broker
  isolation, provenance/taint. Resolve `linux-sandbox` (bwrap, Landlock), `windows-sandbox-rs`, `network-proxy`
  (`credential_broker` against upstream's MITM secret substitution and AWS signing), `sandboxing`, `execpolicy`,
  `exec_policy.rs`; remove `shell-escalation` and the zsh backend per D275. Classify each new upstream permission
  feature (managed command checkpoints, host-inherited execution policies, Unicode canonical matching, guardian
  reviews) as "on at upstream default", "off" or "changes Permissive → sign-off".
- **Entry:** S0 exit; the files can be resolved during S2, testing starts when `codex-core` compiles.
- **Exit:** a PR into the sync branch titled "Upstream sync S3: security port onto unified exec — needs security
  sign-off", with a parity matrix (each security level × each tool path × each platform) and a fail-before/pass-after
  test for every boundary that moved. Green: the core-security lane (via `rust-ci` dispatch), `security-platform-contract`,
  `security-ingress-contract`, `credential-boundary-canary`, `windows-security-probes` (dispatch), Linux sandbox
  tests on glitch, seatbelt tests on macOS, Windows sandbox tests on native Windows CI and on the Windows PC.
- **Security gate SG2:** Travis's sign-off on that PR. It does not merge into the sync branch before it, and S5 can't
  finish without it.
- **Contributor role:** tests, reviews and reproduction only; implementation stays with maintainers.

### S4 TUI, app-server, exec

- **Scope:** S4a app/lib/onboarding/status/history (branding, home directory, startup, upstream's provider setup next
  to our provider picker); S4b chatwidget/bottom_pane (`/vault`, `/tasknode`, `/spawn`, `/gpu`, wallet, `/model` and
  route picker, `/security` picker, model catalog popups); S4c app-server (catalog default, capability profiles,
  permission-profile resume); S4d exec and exec-server (keep `corbanu exec`, adopt `exec-server-cli`). Revert the CI
  mode change. Re-accept snapshots after reading each diff.
- **Entry:** S2 exit.
- **Exit:** workspace builds on macOS, Linux and Windows; `blocking-ci` fully green on the sync branch; `tmux-smoke`
  and `orchestrate-tui-matrix` green; true-TUI QA in tmux (GLM 5.3 Flash, disposable home,
  `CORBANU_TEST_NO_NATIVE_KEYRING=1`) for providers and route default, `/vault`, `/tasknode`, `/spawn`, wallet, `/gpu`
  (stop before any paid step), `/security`; demo reels with the demo-video SOP (`qa/demos/README.md`) for every
  user-visible change, including new upstream UI.
- **Contributor-friendly:** S4c, S4d, snapshot review.

### S5 Qualification and merge

- **Entry:** S3 signed off and merged into the sync branch; S4 exit; CI mode change reverted.
- **Scope:** last `main` merge (≤48 hours before the final merge, with a soft freeze on the hotspot files); full
  run: `blocking-ci` (incl. Windows clippy `-D warnings --tests` and the Windows native tests), `rust-ci-full` (Linux full
  suite, dispatch), `windows-security-probes`, `bazel`, cargo-deny, codespell, nightly lanes (dispatch);
  `just test` on macOS; Linux clippy on glitch; the Windows PC gate; release dry run of `corbanu-terminal-release`;
  acceptance list re-run against the S0 baseline; repository-wide marker scan; final independent review of the merge PR.
- **Exit:** merged to `main` with `--match-head-commit`; installed-build canary on the Mac and the Windows PC for 48
  hours before a release; upgrade record written (like #94's) and linked from #1532.
- **Security gate SG3:** sign-off from SG2 still valid on the final head (no security-path changes since, or re-signed).
- **Rollback:** before merge, nothing to roll back. After merge: a P0 regression in the canary window means a revert PR
  `git revert -m 1 <merge>` (re-syncing later then needs the revert reverted); anything smaller is fixed forward. The
  previous release stays installable; `pre-upstream-sync-<tag>` marks the last pre-sync `main`.

### S6 Cadence (D276)

- **Scope:** a monthly job (and on demand) that runs the trial merge of the newest stable upstream tag into `main`,
  posts the conflict table on a cadence issue, and opens the next sync when it's due. A small sync (under ~150
  conflicted files) runs as one PR with a security slice only if the sandbox/permission paths conflict; anything over
  ~300 files gets a plan like this one.
- **Exit:** first cadence trial measured and posted; the routine is in the maintainers' docs.

## 7. Contributors and Task Node credit

- Each sprint issue lists its slices, marked contributor-friendly or maintainer-only. Claim a slice with a comment on
  that issue; a maintainer confirms and records the owner in the ledger, so nobody duplicates work.
- Contributor PRs target the sync branch and follow the same slice gate and independent review.
- Credit: for every merged or finished-with-credit contribution we post the factual Task Node reviewer note on the PR
  (task id if named, what was done, what it led to). @goodalexander's 0.147.0 work in #94 is the starting point of this
  program: its upgrade record is the template for our retained-behaviour checks. He resolved the provider/model,
  agent and app-server plumbing in #94, so S1b, S2c or S4c are natural fits if he wants one.

## 8. Acceptance list (per slice owner; S5 re-runs all)

Provider catalog and `/model` (OpenAI-style route default) · vault (`/vault`, vault-backed login, secrets never in
logs) · Task Node (`/tasknode`, encrypted session) · `/spawn`, agent graph, mailbox, pane restore · wallet and wallet
daemon socket rules · GPU market (no paid steps) · Telegram · `/usage` and developer accounting · `/security` picker
and the three levels · permissions classifier and taint/provenance · Linux, macOS and Windows sandboxes · secret and
credential brokers · browser isolation · branding, `corbanu` binary names, Corbanu home and test-home isolation ·
`corbanu exec` (reviewer path) · release packaging.

## 9. Risks and mitigations

| Risk | Mitigation |
|---|---|
| `main` keeps moving (~180 commits since 10-29; 0.165.0 conflicts went 873 → 878) | weekly main merges with ledger ownership; 48-hour soft freeze on hotspot files before the final merge |
| Security regression in the unified-exec port | S3 as its own sign-off PR, parity matrix, fail-before/pass-after per boundary, security CI lanes, Windows PC |
| 136 snapshots hide regressions | owner reads every snapshot diff; no blanket accept |
| Upstream features overlap ours (agent roles, message board, mxc/Windows sandbox service, user verification, workload identity, command checkpoints) | compile in at upstream defaults; ours stay authoritative for `/spawn` and security levels; anything that changes Permissive goes to SG2 |
| V8 152.2.0 archives or the rusty_v8 git source fail on a platform (#94 hit a 404) | verified on all three OSes in S0 before the skeleton |
| Upstream 0.167.0 code written for Rust 1.95.0 meets our 1.99.1 lints | compile and clippy check of the upstream tag on 1.99.1 in S0 |
| Windows PC offline (down since 10-27 per the handoff; owner action D212) | native Windows CI is the required lane; Windows PC gate scheduled with Travis for S3 and S5 |
| Review budget (5 rounds per PR) on the S3 and final PRs | ask in advance for a higher budget for those two PRs |
| CI cost on red slice PRs | sync-branch-only CI mode until the workspace compiles |
| Disk (Mac drive, glitch 90% used) | build dirs on CorbanuDrive only, one target dir per worker, removed after each slice |
| Branch stalls half-done | one named owner, weekly status on #1532, explicit abort if S2 slips more than a week |

## 10. Links

#94 (0.147.0 sync, @goodalexander) · #1532 (scoping report, decisions D273–D277) · sprint issues S0–S6 (linked from
#1532).
