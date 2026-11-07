---
sprint_id: "PF-85-S02"
title: "Tier 2: core"
status: draft
plan_file: "docs/plans/active/upstream-sync-rust-v0.167.0.md"
plan_feature: "PF-85"
execution_order: 3
owner: "Slice owners per ledger; integrator merges"
worktree: "UNALLOCATED"
branch: "UNALLOCATED"
base_commit: "UNALLOCATED"
depends_on: "PF-85-S01"
created: 2026-11-07
updated: 2026-11-07
---

# PF-85-S02 — Tier 2: core

## Execution mandate

- Deliver: `codex-core` compiles with tests on macOS, Linux and Windows (S2a–S2c and S3a core paths); config schema regenerated; main merge #2.
- Excludes: S3b port behaviour (listed as S3b ledger rows).
- Tracking issue: #1629. Details: [program plan §6](../../../management/upstreamSyncPlan.md).

## Plan linkage

- Plan: [Upstream Codex sync to rust-v0.167.0](../../../plans/active/upstream-sync-rust-v0.167.0.md).
- Feature: `PF-85`.
- Product citation: **Product principles** — “Maintain continuous Codex parity without removing Corbanu-specific behavior.”

## Code boundaries

- Write scope: Ledger rows with tier 2.
- Branch target: `sync/upstream-rust-v0.167.0` (PRs only).

## Preconditions

- [ ] Plan is active.
- [ ] Dependencies are completed.
- [ ] Worktree, branch, and base commit are exact and match the plan.
- [ ] If parallel, owner/lane/scopes are disjoint and the receiving integration gate is recorded.

## Done

- [x] Sprint record created and linked to one plan feature.

## Remaining

- [ ] Slice PRs S2a, S2b, S2c and S3a core paths.
- [ ] Security-owner approval of `turn.rs`/`client.rs` call sites.
- [ ] Main merge #2.
- [ ] Code-blind functional design frozen before test-result disclosure, or non-user-facing N/A reason recorded.

## Verification

- [ ] `cargo test -p codex-core` on macOS and glitch; Windows via package CI.
- [ ] TUI applicability: none.

## Exit evidence

- [ ] Implementation commit recorded.
- [ ] Final-tree test output linked.
- [ ] `Done` and `Remaining` ledgers reflect reality.
- [ ] Completed record moved to `docs/sprints/archive/upstream-sync/`.
