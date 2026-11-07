---
sprint_id: "PF-85-S00"
title: "Setup and skeleton merge"
status: in_progress
plan_file: "docs/plans/active/upstream-sync-rust-v0.167.0.md"
plan_feature: "PF-85"
execution_order: 1
owner: "Upstream-sync integrator (maintainer)"
parallel_lane: "upstream-sync"
write_scope: "codex-rs/Cargo.toml, codex-rs/Cargo.lock, codex-rs/deny.toml, MODULE.bazel, MODULE.bazel.lock, .github/workflows/, .github/actions/, .github/scripts/, .github/CODEOWNERS, docs/management/upstreamSync/, scripts/upstream_sync/"
integration_gate: "Merged-tree work (skeleton, Tier 0, CI mode, ledger) lands only on sync/upstream-rust-v0.167.0 by PR (ruleset) and reaches main only through the PF-85-S05 final PR with SG1-SG3. Governed records, docs/management/upstreamSync/ and scripts/upstream_sync/ also land on main through reviewed docs/scripts PRs. Received by the upstream-sync integrator, who reruns cargo metadata --locked, cargo deny --locked check and the workflow parse check on the combined tree."
worktree: "/Volumes/CorbanuDrive/Corbanu/worktrees/sync-upstream-rust-v0.167.0"
branch: "sync/upstream-rust-v0.167.0"
base_commit: "781b935ff783178f146d02815f8fc01332ae21a4"
depends_on: "none"
created: 2026-11-07
updated: 2026-11-07
---

# PF-85-S00 — Setup and skeleton merge

## Execution mandate

- Deliver: Two-parent skeleton commit (`main` + `rust-v0.167.0` `086ccc83`) on the sync branch with Tier 0 resolved, the ledger, the sync-branch CI mode and CODEOWNERS block, the CorbanuCore `rusty_v8` copy (D317), SG1.
- Excludes: Resolving any non-Tier-0 conflict; slice work.
- Tracking issue: #1627. Details: [program plan §6](../../../management/upstreamSyncPlan.md).

## Plan linkage

- Plan: [Upstream Codex sync to rust-v0.167.0](../../../plans/active/upstream-sync-rust-v0.167.0.md).
- Feature: `PF-85`.
- Product citation: **Product principles** — “Maintain continuous Codex parity without removing Corbanu-specific behavior.”

## Code boundaries

- Write scope: the front-matter `write_scope`, plus, on the sync branch only, the per-crate `Cargo.toml` and `BUILD.bazel` files of ledger slice S0.
- Branch target: `sync/upstream-rust-v0.167.0` (PRs only).

## Preconditions

- [x] Plan is active.
- [x] Dependencies are completed.
- [x] Worktree, branch, and base commit are exact and match the plan.
- [x] If parallel, owner/lane/scopes are disjoint and the receiving integration gate is recorded.

## Done

- [x] Sprint record created and linked to one plan feature.
- [x] Sync branch `sync/upstream-rust-v0.167.0` cut from `main` `781b935f` (ruleset `upstream-sync branches (sync/**)`).
- [x] Tier 0 re-prepared against `781b935f` (5 workflow files changed since `15ab4699`; all keep ours).
- [x] CorbanuCore/rusty_v8 pins `350c5a85` (D317); `voice-host` excluded from the workspace (D316).
- [x] Skeleton run on glitch and opened as #1690 (evidence in its description; logs `~/corbanu-rtx/sync-s0b/` on glitch).
- [x] Code-blind functional design: N/A, S00 changes no user-visible behaviour (no runnable build until S04); accepted by the integrator; the functional gate is PF-85-S05.
- [x] Baseline lanes dispatched on the sync branch at `781b935f` before the skeleton merge (runs 42354273115, 42354275358, 42354277670, 42354280028).

## Remaining

- [ ] Record the baseline results on #1627; merge the skeleton PR into the sync branch; confirm the CI mode loads there.
- [ ] S1 kick-off checklist on #1628.

## Verification

- [x] Skeleton parents: `git rev-list --parents -n1 1d2e8ca07d` shows `781b935f 086ccc83` (#1690).
- [x] `cargo metadata --locked` and `cargo deny --locked check` pass on the skeleton tree (glitch, #1690).
- [ ] CI mode loads on the sync branch after the merge (workflow runs start; skipped jobs skip).
- [x] TUI applicability: none (no runnable build until S04).

## Exit evidence

- [ ] Implementation commit recorded.
- [ ] Final-tree test output linked.
- [ ] `Done` and `Remaining` ledgers reflect reality.
- [ ] Completed record moved to `docs/sprints/archive/upstream-sync/`.
