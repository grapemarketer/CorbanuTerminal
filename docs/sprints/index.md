# Sprints

Sprints are Corbanu's mechanical execution records. A plan defines a feature
contract. A sprint turns one feature into an exact code-and-evidence checklist.

## Record hierarchy

| Record | Owns |
| --- | --- |
| Product specification | Authorized outcome and decision roles |
| Plan | Feature contract, scope, sequencing, acceptance, and worktrees |
| Sprint | One feature's bounded code tasks and final-tree evidence |
| Release record | Candidate-wide TUI, live-repository, human, and release evidence |
| Feature documentation | Finished behavior only |

## Directory contract

| Directory | Allowed status | Documentation visibility |
| --- | --- | --- |
| `current/<plan-slug>/` | `draft`, `ready`, `in_progress`, `blocked` | Visible; unfinished work only |
| `archive/<plan-slug>/` | `completed`, `cancelled` | Excluded from MkDocs navigation and build |

## Non-negotiable sprint shape

- One sprint links one plan file and one feature id.
- One sprint cannot implement multiple plan features.
- Dependencies may cross features; implementation scope may not.
- Tasks name exact existing or planned code boundaries.
- `Done` contains checked items only; `Remaining` contains unchecked items only.
- Tests, TUI applicability, and exit evidence are checklists, not promises in prose.
- A `draft` sprint may use `UNALLOCATED` worktree coordinates.
- A `ready` or `in_progress` sprint requires an active plan plus an exact
  worktree, branch, and 40-character base commit matching that plan.
- Parallel implementation must satisfy the allocation rules below; executable
  dependencies must already be completed and archived, or (coordinator,
  2026-10-06) be merged to main behind a default-off flag with the record's
  `merged_behind_flag` set and its `gate_evidence` file present.
- A sprint never grants authority beyond its active plan.

## Bounded parallel implementation

Each of at most three active initiatives names an `integration_owner` and
declares `parallel_sprint_limit` from 1 to 3. A limit above 1 requires
`parallel_lanes` naming at least that many lanes; each reserved sprint's
`parallel_lane` must be one of them, and concurrent `write_scope`s must not
overlap. Travis's 2026-10-06 decision replaced the single executable security
sprint with three security lanes (broker, untrusted content, TUI); the other
plans keep a limit of 1. Across all plans at most **five** sprints may be
`in_progress` or `blocked` (three security lanes plus one per other plan);
blocked work keeps its reservation until explicitly returned to draft with a
recorded handoff.

Before a parallel allocation starts:

- Every dependency is completed and archived. `execution_order` is a
  topological reading order, not a serial scheduling lock. No dependency on
  an unfinished interface, draft, fixture-only integration or cancelled record
  is executable; use a completed single-feature contract sprint as a freeze point.
- Each worker has a distinct named owner, `parallel_lane`, exact worktree and
  branch. Worktree coordinates must appear in its active plan. Do not invent
  allocations or share a checkout merely because two tasks look independent.
- Each active record declares `write_scope`: comma-separated repository-relative
  file paths or directory prefixes (trailing `/`), without globs, `..` or root
  reservations. Concurrent scopes must not overlap, even across plans. Include
  manifests, lockfiles, shared registries and tests, not just the main module.
- Each record declares an `integration_gate` naming the receiving owner, merge
  boundary and tests to rerun on the combined tree. The plan's integration owner
  serializes shared Core/protocol/schema/lockfile edits. If these overlap, stop
  concurrency and schedule the changes sequentially; do not omit ownership.
  Concurrent plans each name their integration owner, even with one worker each.
- A blocked or completed lane's handoff records its commit, contract versions,
  test evidence and outstanding integration. Update coordinates/base commits and
  rerun checks before reallocation. An early artifact or interface pass does not
  enable protected behavior or replace PF-13/PF-26 qualification.

The checker enforces counts, concrete allocation fields, distinct owners/lanes/
worktrees/branches, disjoint declared paths, dependency order and cycles. It does
not inspect remote checkouts or prove the declarations truthful: the integration
owner must compare actual diffs to scope and record final combined-tree evidence.

## Execution loop

### Manager-owned continuation

Travis's September 11 request to fix stalled orchestration authorizes the named
integration owner to perform local receiving-branch integration, reconcile
evidence, and revise exact worker allocations within already-approved active
features. This is not permission to change a product contract or a hard gate.
Do not ask the human to do routine allocation, branch preparation or bookkeeping.

Travis's September 12 proactive-testing instruction: if an authorized test or
qualification step holds up another task, start it now or dispatch a concrete
bounded execution to an eligible subagent. Manager ownership is an assignment,
not a reason to wait. Inspect actual tools and signed-in sessions and attempt
safe preflight checks before requesting human execution. Preserve independent,
code-blind acceptance requirements by assigning the right executor; an operator
smoke test is not that acceptance. Record the attempt and evidence, or the exact
missing prerequisite, owner and smallest actionable question. Continue other
approved work while waiting. Do not leave executable tests in a holding pattern,
repeat unchanged clean tests or infer broader external-action authority.

Before a worker finishes, prepare the next bounded assignment and human test
plan. On return, inspect its literal diff and independent review, integrate
reviewed changes on the recorded local receiving branch, and run the combined
tree's affected tests. Update Done/Remaining without equating a worker return,
a passing test or a local merge with human acceptance. Preserve feature-OFF
boundaries. Shared registration and policy edits remain manager-owned.

Then either dispatch remaining work in the same sprint, or complete/archive
the sprint and activate exactly one dependency-complete successor once all
required evidence and decisions actually exist. A changed literal file
allocation within approved feature scope is manager work: record it in plan
and sprint, check overlap and coordinates, run both checkers, then dispatch.
Preparing a successor while its predecessor is unfinished is allowed; starting
its implementation is not. Do not split or waive dependencies merely to stay busy.

Each reserved lane must have a running assignment, an executable next action,
or a concrete blocker with owner, exact question, recommendation, affected scope
and evidence. Manager-owned missing work stays in the manager queue, not marked
as waiting on the human. Deliver newly required decisions to the human; a note
hidden in a private packet does not count as asking. Continue independent work
inside the approved sprint while waiting, without manufacturing busywork.

When a decision stops implementation, also give a plain-language escalation in
the user-facing response: stopped lane, running-worker count, exact question,
recommendation, owner and consequence of waiting. A question card alone is not
sufficient, especially when Travis is away. Maintain a stable manager-owned
decision record for dashboard/approved alert projection; distinguish requested,
sent, failed, delivery-uncertain, acknowledged and resolved. Missing alert
integration must be explicit, never reported as delivered. Dashboard maintenance
success does not mean product implementation is running. Process explicit answers
into canonical records and the unlocked next action without another continue ask.

Going over a review budget needs explicit authorization from Travis or the
coordinator (root AGENTS.md "Review budget extensions", October 14, 2026, which
supersedes the September 12 integrator delegation); integrators and workers
never self-authorize. Preserve prior usage and record each extension.
One fresh independent review of a material candidate plus scoped corrections is
normal, not repeated reviews of unchanged clean code. Move on when reviews have
no substantive unresolved issues. The security owner retains sole implementation
ownership; applicable functional acceptance is independently executed under the
root policy. Such an authorization lifts only review-budget/time holds.
Main merges/pushes, releases, deployments, live Task Node
actions, new product contracts and paid-service commitments still need their
separate authority. Current assignments/evidence are in the
[manager handoff](../plans/workstream-manager-handoff-2026-09-11.md).

1. Select the next dependency-complete sprint linked from the active plan.
2. Resolve its exact worktree coordinates and set it to `ready`.
3. Set it to `in_progress` before code changes.
4. Execute only `Remaining` items; move verified work to `Done` with `[x]`.
5. Run formatting before final affected tests and true-TUI QA.
   For user-facing work, collect the root policy's code-blind functional design
   before disclosing test results, then assign independent permission-isolated
   execution and reconcile every disposition before human handoff. Link schema-2
   execution/isolation receipts, separate evidence review and shared review budget.
6. Complete every verification and exit-evidence checkbox.
7. Set status to `completed`, move the file to `archive/<plan-slug>/`, and remove
   it from current MkDocs navigation.
8. Replace the plan's current-sprint link with release or completion evidence.

## Current sprint portfolio

| Plan | Plan status | Current sprints | Execution authority |
| --- | --- | ---: | --- |
| [P1 security hardening — workstream 1](../plans/active/p1-security-hardening.md) | Active (2026-10-08) | [32 current sprints](current/p1-security-hardening/index.md), 4 archives | PF-13-S07 `ready` (carried from P0); PF-27-S06 and PF-27-S07 completed and archived 2026-10-08, PF-27-S08 2026-10-09, PF-27-S09 2026-10-10 (accepted with known limits); broker lane free; the rest draft |
| [P0 security](../plans/completed/main-2026-10-08-p0-security-levels.md) | Completed (closed by decision 2026-10-08) | [2 hosted drafts](current/p0-security-levels/index.md), 53 archives | 20 of 21 core records archived; PF-13-S07 and the milestones moved to P1 |
| [Accounting — workstream 2](../plans/completed/main-2026-10-10-portfolio-agent-cost-accounting.md) | Completed (closed by decision 2026-10-10) | 0 current; S01–S05 archived | S04 accepted 2026-10-10 with known limits; S05 waiver (a) lifted, (b) carried; collection developer-only |
| [Accounting follow-ups — workstream 2](../plans/completed/main-2026-10-20-portfolio-agent-cost-accounting-followups.md) | Completed (2026-10-20, without a release) | 0 current; S06–S10 archived 2026-10-16 after Travis's acceptance (S10, #909: 29 passed, 1 failed, 7 blocked of 37). S10 signed off 2026-10-27 (D208) at 35 passed, 0 failed, 2 blocked after re-run R1 (#1411) and owner decisions D113 and D207 (ACC-04, ACC-11, ACC-14 passed by owner decision; blocked on the record); named limits ACC-22 and ACC-35 | Follow-up live run (D25, D3, D16) recorded 2026-10-20 on Tart: 18 passed, 2 failed, 5 blocked of 25; Travis's acceptance pending; new issues #1038, #1039, #1045 |
| [Task Node — workstream 3](../plans/active/initiative-delivery-control.md) | Active | PF-80-S01, two PF-79 beta drafts, PF-81-S01 visual QA draft | Offline native increments integrated locally; latest validity worker returned for manager review; beta/harness dependencies unchanged |
| [Unified provider onboarding](../plans/completed/main-2026-10-14-unified-provider-auth.md) | Completed (closed 2026-10-14 to free slot 2; flag removal in a [draft plan](../plans/proposed/named-accounts-flag-removal.md)) | [1 current sprint](current/unified-provider-auth/index.md) (PF-58-S01) | PF-84-S01..S06 completed and archived (S06 2026-10-13, limited testing; S02..S06 behind the `named_accounts` flag, flag-removal prerequisites in the draft flag-removal plan); PF-58 residual qualification unchanged |
| [Arbitrary-model Autoreview](../plans/proposed/arbitrary-model-autoreview.md) | Proposed | [7 draft sprints](current/arbitrary-model-autoreview/index.md) | None until plan activation and sprint worktree allocation |
| [Prompt-injection firewall and brokered authority](../plans/proposed/prompt-injection-firewall.md) | Proposed | 0 | Historical 72-sprint decomposition remains cancelled; every record maps into the P0 plan (closed 2026-10-08) and its P1 successor |

## Machine check

September 10 planning publication and call follow-up add [16 proposals and 52 draft sprints](../plans/portfolio-2026-09-09.md)
(PF-60–PF-75), with no execution authority or allocations. The receiving `main`
baseline had 60 current and 121 archived records; the initial 50 plus two new PF-70 drafts yield 112 current
and 121 archived records. The seven inherited sprint-link/ID errors are resolved;
see the [identity reconciliation](identity-reconciliation-2026-09-10.md).
September 11 adds PF-80-S01 (renamed operations PF-76) and PF-79-S01/S02:
115 current, 121 archived. The subsequent PF-81-S01 visual-QA planning amendment
adds one dependent draft: 116 current, 121 archived, still only three reserved.
S01 accounting closeout then transfers its reservation to S02: 115 current,
122 archived, still three reserved and no extra initiative.
The three-initiative policy is now explicit; all
identity/dependency regression checks remain, with no error exceptions.
Each proposal's sprint execution map links every draft directly.

```bash
python3 docs/sprints/check.py
```

The checker validates lifecycle placement, one-feature linkage, plan backlinks,
required checkbox ledgers, line limits, status authorization, exact plan/worktree
agreement, dependency completion/order/cycles, bounded parallel allocations and
archive completion.
