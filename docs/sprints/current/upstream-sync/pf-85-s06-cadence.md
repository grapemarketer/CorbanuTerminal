---
sprint_id: "PF-85-S06"
title: "Monthly upstream cadence"
status: draft
plan_file: "docs/plans/active/upstream-sync-rust-v0.167.0.md"
plan_feature: "PF-85"
execution_order: 7
owner: "Upstream-sync integrator (maintainer)"
worktree: "UNALLOCATED"
branch: "UNALLOCATED"
base_commit: "UNALLOCATED"
depends_on: "PF-85-S05"
created: 2026-11-07
updated: 2026-11-07
---

# PF-85-S06 — Monthly upstream cadence

## Execution mandate

- Deliver: Monthly trial-merge job and documented routine (D305).
- Excludes: The next sync itself.
- Tracking issue: #1633. Details: [program plan §6](../../../management/upstreamSyncPlan.md).

## Plan linkage

- Plan: [Upstream Codex sync to rust-v0.167.0](../../../plans/active/upstream-sync-rust-v0.167.0.md).
- Feature: `PF-85`.
- Product citation: **Product principles** — “Maintain continuous Codex parity without removing Corbanu-specific behavior.”

## Code boundaries

- Write scope: `scripts/upstream_sync/`, a cadence workflow, maintainers' docs.
- Branch target: `main` (after the final merge).

## Preconditions

- [ ] Plan is active.
- [ ] Dependencies are completed.
- [ ] Worktree, branch, and base commit are exact and match the plan.
- [ ] If parallel, owner/lane/scopes are disjoint and the receiving integration gate is recorded.

## Done

- [x] Sprint record created and linked to one plan feature.

## Remaining

- [ ] Cadence job posts conflict and security-path counts monthly.
- [ ] First cadence trial measured and posted.
- [ ] Code-blind functional design frozen before test-result disclosure, or non-user-facing N/A reason recorded.

## Verification

- [ ] Cadence job run on `main`.
- [ ] TUI applicability: none.

## Exit evidence

- [ ] Implementation commit recorded.
- [ ] Final-tree test output linked.
- [ ] `Done` and `Remaining` ledgers reflect reality.
- [ ] Completed record moved to `docs/sprints/archive/upstream-sync/`.
