---
sprint_id: "PF-85-S00"
title: "Setup and skeleton merge"
status: in_progress
plan_file: "docs/plans/active/upstream-sync-rust-v0.167.0.md"
plan_feature: "PF-85"
execution_order: 1
owner: "Upstream-sync integrator (maintainer)"
parallel_lane: "upstream-sync"
write_scope: "codex-rs/Cargo.toml, codex-rs/Cargo.lock, codex-rs/deny.toml, MODULE.bazel, MODULE.bazel.lock, .github/workflows/sync-package-ci.yml, .github/CODEOWNERS, docs/management/upstreamSync/ledger-rust-v0.167.0.tsv"
integration_gate: "Lands only on sync/upstream-rust-v0.167.0 by PR (ruleset); reaches main only through the PF-85-S05 final PR with SG1-SG3. Lockfile and manifest edits are the merged tree's, not main's."
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

- Write scope: Every Cargo/Bazel manifest and lockfile, `.github/{workflows,actions,scripts}`, `docs/management/upstreamSync/`, `scripts/upstream_sync/`.
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
- [x] Skeleton run on glitch: parents verified, ledger written, `cargo metadata --locked`, `bazel mod deps`, cargo-deny pass.

## Remaining

- [ ] Merge the skeleton PR into the sync branch; confirm the CI mode loads on the sync branch.
- [ ] Baseline lanes on the cut SHA; S1 kick-off checklist on #1628.
- [ ] Code-blind functional design frozen before test-result disclosure, or non-user-facing N/A reason recorded.

## Verification

- [ ] `git rev-list --parents -n1` of the skeleton shows `781b935f 086ccc83`.
- [ ] `cargo metadata --locked` and `cargo deny --locked check` on the skeleton tree (glitch).
- [ ] TUI applicability: none (no runnable build until S4).

## Exit evidence

- [ ] Implementation commit recorded.
- [ ] Final-tree test output linked.
- [ ] `Done` and `Remaining` ledgers reflect reality.
- [ ] Completed record moved to `docs/sprints/archive/upstream-sync/`.
