---
title: "2. Upstream Codex sync to rust-v0.167.0"
status: active
change_class: product-initiative
priority: P1
owner: "Travis Good accountable; upstream-sync integrator (maintainer)"
parallel_sprint_limit: 1
integration_owner: "Upstream-sync integrator (maintainer); merges slice PRs into the sync branch and the final PR into main"
activation_authority: "Travis Good"
activation_basis: "Travis Good's #1532 decisions D273 (dedicated owned program, 2026-11-06), D303 (target rust-v0.167.0) and D304 (active-plan slot 2), both 2026-11-07"
target_release: "TBD (first release after the final merge and its 48-hour canary)"
deadline: "TBD"
created: 2026-11-07
updated: 2026-11-07
product_spec:
  file: docs/corbanu-product-spec.md
  heading: "Product principles"
  requirement_excerpt: "Maintain continuous Codex parity without removing Corbanu-specific behavior."
implementation_worktrees:
  - path: "/Volumes/CorbanuDrive/Corbanu/worktrees/sync-upstream-rust-v0.167.0"
    branch: "sync/upstream-rust-v0.167.0"
    base_commit: "781b935ff783178f146d02815f8fc01332ae21a4"
---

# 2. Upstream Codex sync to rust-v0.167.0

Policy: repository-root `AGENTS.md`

Plan lifecycle: `docs/plans/index.md`

The program plan with tiers, slices, gates and risks is
[upstreamSyncPlan.md](../../management/upstreamSyncPlan.md); this record is its governed form. Records and
evidence: [docs/management/upstreamSync](../../management/upstreamSync/README.md). Program issue #1532.

## Activation record

| Field | Value |
| --- | --- |
| Status | active |
| Active-plan slot | 2 of 3 (D304, Travis, 2026-11-07) |
| Product authority | Travis Good |
| Authoritative decision | #1532: D273, D275, D303 (`rust-v0.167.0`, tag commit `086ccc833bc9cf01024ad2dd6af7dbd421b6bf9a`), D304, D305–D307, D316, D317, D319 |
| Target release | TBD |
| Deadline | TBD (about five weeks from S0; explicit abort review if S2 slips more than a week) |

## User pain

Corbanu Terminal is about 4,190 upstream commits behind OpenAI Codex. Users miss upstream fixes and features, and
every month of delay makes the catch-up larger and riskier.

## Product intent and ideal flow

After the sync, a user upgrades and gets upstream's improvements with every Corbanu feature working as before:
same `corbanu` commands, home directory, providers, vault, Task Node, `/spawn`, wallet, GPU market and security
levels. Existing state, vault files and archives open without migration steps. Afterwards the gap is kept small by a
monthly sync (D305).

## Product linkage

| Field | Value |
| --- | --- |
| Exact product-spec heading | Product principles |
| Requirement excerpt | "Maintain continuous Codex parity without removing Corbanu-specific behavior." |
| Product outcome advanced | Row 11, Upstream Codex parity |
| North-star criterion advanced | Relevant upstream harness improvements adopted without regressions to Corbanu-specific behavior |

## Scope

### In

- One two-parent merge of `rust-v0.167.0` into `main` through `sync/upstream-rust-v0.167.0` (plan §4).
- Slice resolution, the S3 security port onto unified exec (needs security sign-off), qualification, cadence.

### Out

- Newer upstream tags (wait for the cadence); `codex-voice-host` in our lanes (D316); new product features.

## Invariants

- Permissive stays exactly today's behaviour; any moved sandbox boundary stops at a "needs security sign-off" PR.
- History preserved: no squash, rebase or force-push; merge commits only.
- `main` never references a personal fork: `rusty_v8` comes from `CorbanuCore/rusty_v8` (D317).
- Existing vault files, archives and state DBs keep working (D319; migration renumbering keeps released versions).
- The sync branch is not a security-qualified build until SG2; nobody installs or ships it.

## Ownership and implementation worktrees

| Owner | Worktree | Branch | Base commit | Scope |
| --- | --- | --- | --- | --- |
| Integrator | `/Volumes/CorbanuDrive/Corbanu/worktrees/sync-upstream-rust-v0.167.0` | `sync/upstream-rust-v0.167.0` | `781b935ff783178f146d02815f8fc01332ae21a4` | skeleton, ledger, manifests, CI files, main merges, final PR |

Slice workers branch from the sync branch, one slice each, and are recorded in the ledger's `owner` column.

## Useful code references

| Path or symbol | Why it matters |
| --- | --- |
| `scripts/upstream_sync/` | skeleton, ledger, slice classifier, CI mode, SG1 checks |
| `docs/management/upstreamSync/ledger-rust-v0.167.0.tsv` (sync branch) | one row per conflicted path, owner and status |
| `scripts/upstream_sync/security_paths.txt` | SG2/SG3 and CODEOWNERS scope |

## Sprint execution map

| Feature ID | Current sprint records | Completion evidence |
| --- | --- | --- |
| PF-85 | [PF-85-S00](../../sprints/current/upstream-sync/pf-85-s00-setup-and-skeleton.md), [PF-85-S01](../../sprints/current/upstream-sync/pf-85-s01-tier1-below-core.md), [PF-85-S02](../../sprints/current/upstream-sync/pf-85-s02-tier2-core.md), [PF-85-S03](../../sprints/current/upstream-sync/pf-85-s03-security-port.md), [PF-85-S04](../../sprints/current/upstream-sync/pf-85-s04-tier3-tui-app-server-exec-cli.md), [PF-85-S05](../../sprints/current/upstream-sync/pf-85-s05-qualification-and-merge.md), [PF-85-S06](../../sprints/current/upstream-sync/pf-85-s06-cadence.md) | pending |

PF-85 is the sync of `rust-v0.167.0` into `main`. S3a's paths are resolved inside S01 and S02; the S03 port starts at
the S02 exit and runs alongside S04 (plan §6). That needs `parallel_sprint_limit` 2, `parallel_lanes` naming two lanes
and disjoint S03/S04 write scopes (S04's tier-3 rows include security-path files) when S03 starts; until then one
sprint is in progress at a time.

## Acceptance flows

| Flow | Starting state | User action | Expected visible result | Pass criterion |
| --- | --- | --- | --- | --- |
| Primary success | previous Corbanu release with state, vault and Task Node session | install the synced build, start `corbanu` | same home, providers, `/model` route default, `/vault`, `/tasknode`, `/spawn`, `/security` | plan §9 acceptance list passes on macOS, Linux, Windows |
| Failure/cancel | synced build | cancel a turn, deny an approval | same cancel and approval behaviour as before | true-TUI cancel runs pass |
| Recovery/resume | session from the previous release | resume it in the synced build; open the upgraded state DB with the previous release | session resumes; old release still opens the DB | S1c upgrade test and S5 canary pass |

## Implementation sequence

1. S0 setup and skeleton (PF-85-S00).
2. S1 Tier 1 crates below core; S2 core; S3 security port (SG2); S4 TUI, app-server, exec, CLI.
3. S5 qualification and final merge (SG3); S6 cadence.

## Automated evidence

Code-blind functional gate: designed and executed in S5 per `qa/code-blind-functional/README.md`; manager: integrator.

| Check | Final-tree command | Result | Artifact |
| --- | --- | --- | --- |
| Focused | per-slice `cargo check -p`/`cargo test -p` (plan §4) | pending | slice PRs |
| Integration | `blocking-ci`, `rust-ci-full`, `just test` (S5) | pending | final PR |
| Snapshot, if applicable | owner reads every snapshot diff | pending | slice PRs |
| Adversarial or security, if applicable | S3 parity matrix and security lanes; SG1–SG3 | SG1 recorded in S0 | `docs/management/upstreamSync/` |

## True-TUI evidence

| Flow | Candidate binary | Test repo/worktree | Keys/actions | Visible checkpoints | Result | Artifact |
| --- | --- | --- | --- | --- | --- | --- |
| Primary | S4/S5 build | TensorCash, Isometric Game | plan §9 list in tmux | per feature | pending | S4/S5 |
| Failure/cancel | S5 build | same | cancel, deny | per feature | pending | S5 |
| Recovery/resume | S5 build | same | resume | per feature | pending | S5 |

## Live-repository applicability

| Repository | Applicable to this initiative? | Resolved checkout/test worktree | Base commit | Reason or result |
| --- | --- | --- | --- | --- |
| TensorCash | yes | resolved in S5 | pending | success, cancel, recovery, resume |
| Isometric Game | yes | resolved in S5 | pending | success, cancel, recovery, resume |

## Human acceptance

| Tester | Date | Candidate version/commit | Flow | Result | Evidence |
| --- | --- | --- | --- | --- | --- |
| Travis Good | pending | final PR head | SG2 sign-off; installed-build canary | pending | S3/S5 |

## Documentation

| Finished-feature doc | Product-spec citation present | Verified candidate |
| --- | --- | --- |
| upgrade record per `docs/plans/upstream-integration.md` | yes | pending |

## Dependencies, decisions, and blockers

| Item | Type | Owner | Needed by | State / decision |
| --- | --- | --- | --- | --- |
| Separate security-owner reviewer account | decision | Travis | S3 | D318 pending; CODEOWNERS block is request-only |
| `age` 0.11 and reduced `zip` decoders | decision | security owner | S1 | D319: decide in S1 with hard backward compatibility |
| Review budget | decision | Travis | S3, S5 | D307: up to 8 rounds for the S3 PR and the final PR |

## Release linkage

- Release record: `qa/release/<version>/` after the final merge and the 48-hour merge hold (D306).
- Benchmark tracker row: product-spec row 11 (coding benchmark catalog if due).
- Remaining blocker: S0–S5.

## Completion

- [ ] Product linkage, scope, invariants, and worktrees are current.
- [ ] Every implementation unit is represented by a valid single-feature sprint.
- [ ] Required final-tree automated evidence passes.
- [ ] Code-blind cases independently executed under enforced isolation; schema-2 handoff check and independent evidence review pass (or accepted non-user-facing N/A); review ledger linked.
- [ ] Required true-TUI and live-repository evidence passes.
- [ ] Human acceptance passes.
- [ ] Finished documentation matches the candidate.
- [ ] Release and benchmark records are linked.
- [ ] No hard release gate remains pending.
