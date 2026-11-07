---
sprint_id: "PF-85-S04"
title: "Tier 3: TUI, app-server, exec, CLI"
status: draft
plan_file: "docs/plans/active/upstream-sync-rust-v0.167.0.md"
plan_feature: "PF-85"
execution_order: 5
owner: "Slice owners per ledger; integrator merges"
worktree: "UNALLOCATED"
branch: "UNALLOCATED"
base_commit: "UNALLOCATED"
depends_on: "PF-85-S02"
created: 2026-11-07
updated: 2026-11-07
---

# PF-85-S04 — Tier 3: TUI, app-server, exec, CLI

## Execution mandate

- Deliver: Workspace compiles everywhere; S4a–S4e resolved; CI mode reverted; true-TUI QA and demo reels.
- Excludes: S3b port.
- Tracking issue: #1631. Details: [program plan §6](../../../management/upstreamSyncPlan.md).

## Plan linkage

- Plan: [Upstream Codex sync to rust-v0.167.0](../../../plans/active/upstream-sync-rust-v0.167.0.md).
- Feature: `PF-85`.
- Product citation: **Product principles** — “Maintain continuous Codex parity without removing Corbanu-specific behavior.”

## Code boundaries

- Write scope: Ledger rows with tier 3.
- Branch target: `sync/upstream-rust-v0.167.0` (PRs only).

## Preconditions

- [ ] Plan is active.
- [ ] Dependencies are completed.
- [ ] Worktree, branch, and base commit are exact and match the plan.
- [ ] If parallel, owner/lane/scopes are disjoint and the receiving integration gate is recorded.

## Done

- [x] Sprint record created and linked to one plan feature.

## Remaining

- [ ] Slice PRs S4a–S4e; snapshots re-accepted after reading each diff.
- [ ] Exclude the `//codex-rs/voice-host` Bazel targets (D316) before reverting the sync-branch CI mode; then revert it.
- [ ] True-TUI QA and demo reels.
- [ ] Code-blind functional design frozen before test-result disclosure, or non-user-facing N/A reason recorded.

## Verification

- [ ] `blocking-ci`, `tmux-smoke`, `orchestrate-tui-matrix`.
- [ ] TUI applicability: yes, plan §9 list in tmux.

## Exit evidence

- [ ] Implementation commit recorded.
- [ ] Final-tree test output linked.
- [ ] `Done` and `Remaining` ledgers reflect reality.
- [ ] Completed record moved to `docs/sprints/archive/upstream-sync/`.
