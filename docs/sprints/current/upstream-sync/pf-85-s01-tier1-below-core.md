---
sprint_id: "PF-85-S01"
title: "Tier 1: crates below core"
status: draft
plan_file: "docs/plans/active/upstream-sync-rust-v0.167.0.md"
plan_feature: "PF-85"
execution_order: 2
owner: "Slice owners per ledger; integrator merges"
worktree: "UNALLOCATED"
branch: "UNALLOCATED"
base_commit: "UNALLOCATED"
depends_on: "PF-85-S00"
created: 2026-11-07
updated: 2026-11-07
---

# PF-85-S01 — Tier 1: crates below core

## Execution mandate

- Deliver: Every Tier 1 ledger row resolved (S1a protocol, S1b config/providers/auth, S1c other crates incl. migration renumbering, S1d tooling, S3a Tier 1 sandbox crates); main merge #1.
- Excludes: core and crates above it.
- Tracking issue: #1628. Details: [program plan §6](../../../management/upstreamSyncPlan.md).

## Plan linkage

- Plan: [Upstream Codex sync to rust-v0.167.0](../../../plans/active/upstream-sync-rust-v0.167.0.md).
- Feature: `PF-85`.
- Product citation: **Product principles** — “Maintain continuous Codex parity without removing Corbanu-specific behavior.”

## Code boundaries

- Write scope: Ledger rows with tier 1 (one slice owner per path).
- Branch target: `sync/upstream-rust-v0.167.0` (PRs only).

## Preconditions

- [ ] Plan is active.
- [ ] Dependencies are completed.
- [ ] Worktree, branch, and base commit are exact and match the plan.
- [ ] If parallel, owner/lane/scopes are disjoint and the receiving integration gate is recorded.

## Done

- [x] Sprint record created and linked to one plan feature.

## Remaining

- [ ] Slice PRs per the ledger; security-owner review on security paths.
- [ ] D319 fixtures (plan §6 S1) generated from `main` and committed before any `age`/`zip` change; then the security owner's decision.
- [ ] Migration renumbering with an upgrade test from the latest released state DB.
- [ ] Main merge #1.
- [ ] Code-blind functional design frozen before test-result disclosure, or non-user-facing N/A reason recorded.

## Verification

- [ ] `cargo check -p`/`cargo test -p` per Tier 1 crate on macOS and glitch; Linux clippy for sandbox crates.
- [ ] TUI applicability: none.

## Exit evidence

- [ ] Implementation commit recorded.
- [ ] Final-tree test output linked.
- [ ] `Done` and `Remaining` ledgers reflect reality.
- [ ] Completed record moved to `docs/sprints/archive/upstream-sync/`.
