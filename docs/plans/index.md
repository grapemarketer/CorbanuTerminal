# Corbanu Terminal plans

This file is the sole authority for product-initiative plan lifecycle and
work-in-progress. Use the change classes in repository-root `AGENTS.md` to decide
whether work needs a plan.

## Directory contract

| Directory | Allowed status | Purpose |
| --- | --- | --- |
| [`proposed/`](proposed/index.md) | `draft` | Product initiatives being shaped; implementation is not authorized |
| [`active/`](active/index.md) | `active` | Authorized product initiatives currently consuming a WIP slot |
| [`completed/`](completed/index.md) | `completed` | Released initiatives with final evidence and release linkage |
| [`cancelled/`](cancelled/index.md) | `cancelled` | Initiatives stopped by an explicit product decision |

`PLAN_TEMPLATE.md` is the source template. Files in lifecycle directories are
the records. A lifecycle directory index is explanatory and never counts as a
plan.

Plan filenames use a stable lowercase slug, such as
`p0-security-levels.md`. On completion, prefix the filename
with the shipped release as required below.

## Current portfolio

The machine source of truth is the front matter in each lifecycle directory.

| Slot | Initiative | Priority | Deadline | Owner |
| ---: | --- | --- | --- | --- |
| 1 of 3 | [P1 security hardening](active/p1-security-hardening.md): deferred features plus P0's carried-forward milestones, 3 lanes; activated 2026-10-08 after [P0 security](completed/main-2026-10-08-p0-security-levels.md) closed | P1 | TBD | Jim Ricketts; Codex security coordinator integrates |
| 2 of 3 | [Upstream Codex sync to rust-v0.167.0](active/upstream-sync-rust-v0.167.0.md): one two-parent merge of upstream `rust-v0.167.0`, 7 sprints (PF-85); activated 2026-11-07 (D304 on #1532) after the [accounting follow-ups plan](completed/main-2026-10-20-portfolio-agent-cost-accounting-followups.md) freed the slot on 2026-10-20 | P1 | TBD | Travis accountable; upstream-sync integrator |
| 3 of 3 | [Task Node integration](active/initiative-delivery-control.md) | P1 | TBD | Codex Task Node lane; Travis accepts |

Run `python3 docs/plans/check.py` to validate lifecycle placement, required active
metadata, the three-plan limit and per-plan lane declarations. CI runs the same check.

## Work-in-progress limit

Corbanu may have at most **three active product-initiative plans**. Each reserves
one sprint, or up to three when it declares `parallel_sprint_limit` and matching
`parallel_lanes` (Travis, 2026-10-06; the security plan uses three lanes). Travis authorized this operating model September 10 and selected
PF-13, accounting and Task Node on September 11. On 2026-10-08 he closed P0 security and gave its slot to P1 security hardening. See the [main handoff](main-workstreams-2026-09-11.md).

A plan counts toward the limit only when it:

- is a Markdown file under `docs/plans/active/`;
- declares `status: active`; and
- covers a product initiative as defined by the root policy.

Routine work, bounded fixes, and release records do not consume a plan slot.
They also cannot introduce new product scope. A product initiative may not be
reclassified to evade the limit.

Before activation, run the checker and inspect every active plan. If three exist,
finish, cancel, or explicitly replace one through the product decision process.
Never activate a fourth.

## Activation requirements

Create the plan from [`PLAN_TEMPLATE.md`](PLAN_TEMPLATE.md). It is ready to
activate when it records:

- the exact product-spec heading and a short requirement excerpt;
- priority, deadline, activation authority, and the authoritative decision
  already authorizing the outcome;
- user pain and the ideal end-to-end flow;
- scope and non-goals;
- owner, target release, and exact implementation worktree paths, branches, and
  base commits;
- cross-initiative parallel allocation, integration owner and disjoint work ownership
  under the [sprint concurrency rules](../sprints/index.md#bounded-parallel-implementation);
- useful code references;
- an upstream-touch record under the [upstream integration contract](upstream-integration.md),
  including verified baseline, adapter ownership, and compatibility evidence;
- a sprint execution map that assigns every implementation unit to exactly one
  plan feature;
- a hard dependency graph, requirement-to-evidence traceability, and, when
  opting into sprint concurrency, an integration owner and lane allocations
  under `docs/sprints/index.md`;
- measurable success, failure, recovery, and resume flows;
- applicable automated, true-TUI, live-repository, human, documentation, and
  benchmark evidence fields; and
- dependencies, open decisions, and hard blockers.

Set `status: active` only after those fields are complete. Draft notes and
legacy files elsewhere in the repository do not authorize implementation.

Migration rule: an already-active plan found without a complete sprint map
continues to consume its active slot, but implementation pauses. It may remain
active while it is mechanically decomposed; no sprint may become executable
until the map is complete and the sprint checker passes.

## Lifecycle

1. **Propose:** Create `docs/plans/proposed/<slug>.md` with `status: draft`.
   Implementation may not begin under the proposal.
2. **Activate:** Confirm product authority, the WIP limit, ownership,
   worktrees, scope, and acceptance criteria. Move the file to `active/` and
   set `status: active`.
3. **Sprint:** Create or select the next single-feature execution record under
   `docs/sprints/current/`, validate it with `python3 docs/sprints/check.py`, and
   set it to `ready` or `in_progress` only when this plan is active and its exact
   worktree coordinates are recorded.
4. **Build:** Work only in the selected sprint's remaining checklist and the
   plan's declared worktree. Update the plan and sprint map before scope changes.
5. **Qualify:** Complete sprint evidence, archive accepted sprint records, and
   fill the plan evidence fields required by the root policy.
6. **Release:** Link the versioned record under `qa/release/<version>/`.
7. **Close:** After release, set `status: completed` and move the record to
   `docs/plans/completed/<release>-<slug>.md`. If stopped, set
   `status: cancelled`, record the decision, and move it to
   `docs/plans/cancelled/<slug>.md`.

A release record may aggregate the active initiatives without
becoming another active plan. A plan cannot be marked completed merely because
code was merged: all required evidence and the release linkage must be present.

Sprint lifecycle and file requirements are canonical in
[`docs/sprints/index.md`](../sprints/index.md). A plan authorizes feature scope;
it does not replace a sprint execution mandate.
