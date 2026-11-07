---
sprint_id: "PF-60-S10"
title: "Isolated code-blind acceptance of the follow-ups"
status: completed
plan_file: "docs/plans/completed/main-2026-10-20-portfolio-agent-cost-accounting-followups.md"
plan_feature: "PF-60"
execution_order: 5
owner: "Codex worker pf60-s10 (integrator); fresh designer, reviewer, executors and evidence checkers; Travis Good accepts"
parallel_lane: "accounting"
write_scope: "qa/portfolio/agent-cost-accounting/followups-acceptance-20261016/, docs/sprints/current/portfolio-agent-cost-accounting-followups/pf-60-s10-isolated-code-blind-acceptance.md"
integration_gate: "Code-blind design frozen before the VM start; isolated run on VM macOS 3 under the enforced gate; independent evidence checks within a 5-round budget; record PR to main; Travis accepts"
worktree: "/Volumes/CorbanuDrive/Corbanu/worktrees/pf60-s10"
branch: "qa/pf60-s10-acceptance"
base_commit: "395ac0adcd96f4970220e569a1608141a3e748f3"
depends_on: "PF-60-S06, PF-60-S07, PF-60-S08, PF-60-S09"
created: 2026-10-10
updated: 2026-10-27
---

# PF-60-S10 — Isolated code-blind acceptance of the follow-ups

**Status: completed.** Signed off by Travis on 2026-10-27 (D208): **35 passed / 0 failed / 2 blocked** of 37, after
re-run R1 and owner decisions D113 and D207 (ACC-04, ACC-11 and ACC-14 passed by owner decision; on the record they stay
blocked). Named limits: ACC-22 (advisory) and ACC-35 (needs real Moonshot spend, not granted). First run, accepted
2026-10-16 as recorded: 29 / 1 / 7. See Final disposition and Closure.

## Final disposition — 2026-10-27 (D207–D209)

- **Re-run R1** ([record](../../../../qa/portfolio/agent-cost-accounting/followups-acceptance-20261016/rerun-r1-20261022/README.md),
  PR #1411): ACC-06, ACC-15 and ACC-18 passed; ACC-11 and ACC-14 stayed blocked, each on one result not captured in the
  case's own screens (the tester reported both passed). ACC-06's failure (#898) is confirmed fixed.
- **D207:** ACC-11 and ACC-14 recorded as passed by owner decision, as D113 (2026-10-21) did for ACC-04. Owner acceptance
  is not a clean pass: the record's dispositions for these three stay **blocked**, and the decisions are listed separately
  ([dispositions.md](../../../../qa/portfolio/agent-cost-accounting/followups-acceptance-20261016/dispositions.md), last
  section; `owner_decisions` in [results.json](../../../../qa/portfolio/agent-cost-accounting/followups-acceptance-20261016/results.json)).
- **D208:** S10 signed off by Travis. Final disposition **35 passed, 0 failed, 2 blocked** of 37 (32 passed on the record,
  3 by owner decision). Named limits: ACC-22 (advisory; starting condition not met) and ACC-35 (optional; needs real
  Moonshot spend, D3 not granted).
- **D209:** this page, the sprint index and the record (README, dispositions.md, results.json) carry these counts.

## Closure — 2026-10-16

Completed. PF-60-S10's isolated code-blind run ([record](../../../../qa/portfolio/agent-cost-accounting/followups-acceptance-20261016/README.md), PR #909, merged `223927bc26`) was accepted by Travis on 2026-10-16 as recorded, 29 passed, 1 failed, 7 blocked of 37 (decision D24, "all as recommended"; comment on [#898](https://github.com/CorbanuCore/CorbanuTerminal/issues/898)). With it he decided D25 (the S10 gaps are re-checked in #898's acceptance run, not a full S10 re-run) and D26 (the record's last evidence check, r3, failed on wording only; accepted without another round).

- **Record:** [README](../../../../qa/portfolio/agent-cost-accounting/followups-acceptance-20261016/README.md), [dispositions](../../../../qa/portfolio/agent-cost-accounting/followups-acceptance-20261016/dispositions.md), [results.json](../../../../qa/portfolio/agent-cost-accounting/followups-acceptance-20261016/results.json),
  [review ledger](../../../../qa/portfolio/agent-cost-accounting/followups-acceptance-20261016/review-ledger.md). Design frozen at `220c99ed59` before the VM start; candidate `origin/main`
  `395ac0adcd` (signed developer build 0.1.48); VM "macOS 3" only, lock held 03:10–08:06Z; isolation gate 105/105
  probes; key scan 0 hits.
- **Failed (1):** ACC-06 (#898), fixed by PR #917 (merged `d4831b7326`).
- **Blocked (7):** ACC-04 and ACC-22 (advisory; starting conditions not met), ACC-35 (optional Moonshot cache read; D3
  not granted during the run), ACC-11, ACC-14, ACC-18 (Request IDs not re-read after a reopen) and ACC-15 (`side:` label
  not captured).
- **Accepted with it:** the review budget was spent (5 of 5) and the last evidence check (r3) failed on written
  statements only, with no disposition changed (D26). `qa/code-blind-functional/check.py` stops at the first unresolved
  case (ACC-04), as expected; Travis's acceptance as recorded stands in for a clean checker pass.
- **Carried to the next isolated live run** (D25, with D3 and D16 of 2026-10-16): ACC-06 live, the Request-ID checks of
  ACC-11/14/18 and ACC-15's screen, one real Moonshot cache hit (under $0.01) and one real OpenAI realtime session
  (under $0.05; #882 merged after this run's freeze). Also named limits in the record: TensorCash, DeepSeek holidays,
  BigModel, Windows and Linux, `fork:`, same-conversation `/resume`.
- **Waiver (b) of PF-60-S05 is discharged:** this run executed under the enforced isolation gate (dated addendum in
  [PF-60-S05](../portfolio-agent-cost-accounting/pf-60-s05-collection-correctness-and-billing-basis.md)). #404 closes
  with this archive.

## Execution mandate

- Deliver: item (viii) from Travis's 2026-10-10 decision (#404). PF-60-S05 waived the enforced isolation gate
  (waiver (b)) and PF-60-S04's acceptance ran without it too. Run the acceptance of PF-60-S06 to PF-60-S09 inside the
  [isolated execution gate](../../../../qa/code-blind-functional/isolated-execution.md), so waiver (b) is discharged.
- Excludes: fixing defects found (they become issues); new product scope.

## Plan linkage

- Plan: [Accounting follow-ups after PF-60](../../../plans/completed/main-2026-10-20-portfolio-agent-cost-accounting-followups.md); feature `PF-60`.
- Acceptance advanced: all follow-up flows pass on one recorded binary under enforced isolation.

## Code boundaries

- Existing: `qa/code-blind-functional/` (contract, `RECORD_TEMPLATE.md`, `check.py`, executor and designer prompts).
- Planned: `qa/portfolio/agent-cost-accounting/followups-acceptance-<date>/`; reuse P1 security's isolated VM/runner
  if it exists by then rather than building a second one.

## Preconditions

- [x] Plan is active.
- [x] PF-60-S06 to PF-60-S09 accepted by Travis (S06 2026-10-14, S07–S09 2026-10-15). By his 2026-10-15 decision S10 ran
      on them before they were archived; all five are archived together on 2026-10-16.
- [x] Isolated runner available: VM "macOS 3" with separate unprivileged executor accounts and mediated provider
      credentials the executors can't read (the P1 security milestone harness, reused).

## Done

- [x] Draft sprint created 2026-10-10 from Travis's decision; linked to one plan feature.
- [x] 2026-10-16 (worker `pf60-s10`; Travis 2026-10-15: run on S06–S09 before they are archived, on "macOS 3"):
      isolated code-blind run recorded in
      [followups-acceptance-20261016](../../../../qa/portfolio/agent-cost-accounting/followups-acceptance-20261016/README.md):
      design frozen before the VM start (`220c99ed59`), 37 cases on `395ac0adcd`'s signed developer build:
      29 passed, 1 failed (ACC-06, #898), 7 blocked. Realtime (#824) is the tracked known gap.
- [x] 2026-10-16: Travis accepted S10 as recorded (D24), with D25 and D26 (Closure).

## Remaining

- [x] AC1: a fresh designer's 34 cases, amended by a separate fresh reviewer (22 amendments, 3 new cases), frozen at
      `220c99ed59` before the VM start.
- [x] AC2: fresh GLM 5.3 Flash executors, one per group (6 groups plus replays C2 and E2), ran every case on the
      signed `395ac0adcd` candidate inside the gate.
- [x] AC3: negative and positive probes recorded per group and per executor: 105/105 after two gate attempts that
      failed on harness faults; every isolation receipt is true.
- [x] AC4: real keys (Z.AI, OpenAI, Moonshot) reached the product only through the relay; key scan 0 hits.
- [x] AC5: every case has passed, failed or blocked; Travis accepted the 29/1/7 dispositions on 2026-10-16 (D24).
      Final after re-run R1, D113 and D207: 35/0/2, signed off 2026-10-27 (D208).
- [x] AC6: three independent evidence checks within the budget (the last, r3, failed on wording only; accepted as
      D26). `check.py` stops at the first unresolved case (ACC-04), as expected; accepted as recorded (Closure).

## Verification

- [x] Schema-2 execution and isolation receipts linked (`results.json`, `runs/*/isolation-*.json`).
- [x] Exact candidate commit and binary digest recorded (`395ac0adcd`, `bin/corbanu` SHA-256 `5b805b9c…0531a1`).
- [x] Integration: `python3 docs/plans/check.py; python3 docs/sprints/check.py`; `git diff --check` (record PR #909 and
      this archive).
- [x] Code-blind cases independently executed under enforced isolation; separate evidence checks linked (review ledger).

## Exit evidence

- [x] Acceptance record and review linked (Closure).
- [x] Travis accepted (2026-10-16, D24); waiver (b) recorded as discharged in the plan and in PF-60-S05's archived
      record (dated addendum).
- [x] Code-blind handoff checker: stops at ACC-04 as expected; Travis's acceptance as recorded is the agreement. No
      automatic human acceptance.
- [x] Parallel handoff: not applicable.
- [x] `Done` and `Remaining` ledgers reflect reality; #404 closed with this archive.
- [x] Completed record moved to `docs/sprints/archive/portfolio-agent-cost-accounting-followups/` (2026-10-16).
