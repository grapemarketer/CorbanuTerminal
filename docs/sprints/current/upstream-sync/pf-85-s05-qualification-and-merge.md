---
sprint_id: "PF-85-S05"
title: "Qualification and merge to main"
status: draft
plan_file: "docs/plans/active/upstream-sync-rust-v0.167.0.md"
plan_feature: "PF-85"
execution_order: 6
owner: "Upstream-sync integrator (maintainer)"
worktree: "UNALLOCATED"
branch: "UNALLOCATED"
base_commit: "UNALLOCATED"
depends_on: "PF-85-S03, PF-85-S04"
created: 2026-11-07
updated: 2026-11-07
---

# PF-85-S05 — Qualification and merge to main

## Execution mandate

- Deliver: Final PR merged to `main` with all lanes green, SG3 and the qualification record; canary.
- Excludes: New upstream tags.
- Tracking issue: #1632. Details: [program plan §6](../../../management/upstreamSyncPlan.md).

## Plan linkage

- Plan: [Upstream Codex sync to rust-v0.167.0](../../../plans/active/upstream-sync-rust-v0.167.0.md).
- Feature: `PF-85`.
- Product citation: **Product principles** — “Maintain continuous Codex parity without removing Corbanu-specific behavior.”

## Code boundaries

- Write scope: Whole workspace (integrator).
- Branch target: the sync branch, then the final PR `sync/upstream-rust-v0.167.0` → `main`.

## Preconditions

- [ ] Plan is active.
- [ ] Dependencies are completed.
- [ ] Worktree, branch, and base commit are exact and match the plan.
- [ ] If parallel, owner/lane/scopes are disjoint and the receiving integration gate is recorded.

## Done

- [x] Sprint record created and linked to one plan feature.

## Remaining

- [ ] Main merge #3; revert the CODEOWNERS block; full run; SG1 re-run; SG3.
- [ ] Rollback: synced-build vault files and the upgraded state DB open in the last released binary (D319; state-DB upgrade test from S1c).
- [ ] Final review (up to 8 rounds, D307); merge; 48-hour merge hold on `main` (D306) and canary.
- [ ] Code-blind functional design frozen before test-result disclosure, or non-user-facing N/A reason recorded.

## Verification

- [ ] Full CI, `just test` on macOS, Windows PC gate, release dry run.
- [ ] TUI applicability: yes, TensorCash and Isometric Game.

## Exit evidence

- [ ] Implementation commit recorded.
- [ ] Final-tree test output linked.
- [ ] `Done` and `Remaining` ledgers reflect reality.
- [ ] Completed record moved to `docs/sprints/archive/upstream-sync/`.
