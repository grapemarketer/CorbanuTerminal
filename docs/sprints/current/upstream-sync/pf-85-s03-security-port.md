---
sprint_id: "PF-85-S03"
title: "Security port onto unified exec"
status: draft
plan_file: "docs/plans/active/upstream-sync-rust-v0.167.0.md"
plan_feature: "PF-85"
execution_order: 4
owner: "Slice owners per ledger; integrator merges"
worktree: "UNALLOCATED"
branch: "UNALLOCATED"
base_commit: "UNALLOCATED"
depends_on: "PF-85-S02"
created: 2026-11-07
updated: 2026-11-07
---

# PF-85-S03 — Security port onto unified exec

## Execution mandate

- Deliver: S3b PR "Upstream sync S3: security port onto unified exec — needs security sign-off" with the parity matrix; SG2.
- Excludes: Changing Permissive behaviour without sign-off.
- Tracking issue: #1630. Details: [program plan §6](../../../management/upstreamSyncPlan.md).

## Plan linkage

- Plan: [Upstream Codex sync to rust-v0.167.0](../../../plans/active/upstream-sync-rust-v0.167.0.md).
- Feature: `PF-85`.
- Product citation: **Product principles** — “Maintain continuous Codex parity without removing Corbanu-specific behavior.”

## Code boundaries

- Write scope: S3a/S3b paths in the ledger and `security_paths.txt`.
- Branch target: `sync/upstream-rust-v0.167.0` (PRs only).

## Preconditions

- [ ] Plan is active.
- [ ] Dependencies are completed.
- [ ] Worktree, branch, and base commit are exact and match the plan.
- [ ] If parallel, owner/lane/scopes are disjoint and the receiving integration gate is recorded.

## Done

- [x] Sprint record created and linked to one plan feature.

## Remaining

- [ ] Port classifier, approvals, security levels, sandbox selection, broker isolation and taint onto unified exec.
- [ ] Diff-level review of the clean-merging security-path rows.
- [ ] SG2: Travis's sign-off on the exact head (up to 8 review rounds, D307).
- [ ] Code-blind functional design frozen before test-result disclosure, or non-user-facing N/A reason recorded.

## Verification

- [ ] Security lanes, Linux/macOS/Windows sandbox tests, Windows PC run.
- [ ] TUI applicability: `/security` picker, covered in S04 QA.

## Exit evidence

- [ ] Implementation commit recorded.
- [ ] Final-tree test output linked.
- [ ] `Done` and `Remaining` ledgers reflect reality.
- [ ] Completed record moved to `docs/sprints/archive/upstream-sync/`.
