# Owner decisions behind amendment L3 (verbatim from the coordinator's brief), background and integrator choices

## Decisions (Travis, owner, 2026-10-22)

- **D161:** "milestone runs now use Tart."
- **MODEL-DECLINE rule (standing, 2026-10-22):** "whenever a hosted product model declines a test step (so a
  protection is never exercised), re-run that step with the local model harness ... Do not accept a declined step as
  the final result without trying the local model."
- **D163:** "MOD-21 (amendment v9, via Spawn status) so results 1–3 are actually exercised." Run with the local model
  as the product's model.
- **D165:** "MOD-11 items 3 and 8." Run with the local model as the product's model.
- **D111 / D143:** "AGG-20P (person-defined role) as designed in next-localmodel-run.md — if it passes, AGG-20's named
  limit lifts."
- The brief also says: "Run with the LOCAL model as the product model"; "Code-blind testers GLM 5.3 Flash on zai drive
  the TUI (check ~every 30 s)".

Earlier decisions still in force and referred to: D140 (amendment v9: MOD-21's task is sent from Spawn status), D114
(testers check about every 30 s during a long turn; `tester-packet-rules.md`), D74/D75 (the local-model harness is the
standard way to test protections a hosted model refuses to attempt), D43 (MOD-11 item 6: the project's `.git` stays
read-only even with a fresh approval; accepted), #447 / G-1 (the model-decline rule).

## Background

- **MOD-21.** In every run so far the hosted child model (GLM 5.2, per amendments-v3 R-1) declined the task before
  running any command, so expected results 1–3 (outside write fails even with approval; credential read prompts with a
  security reason; Corbanu config read denied) were never exercised. Six attempts on build `9750861c2a` used
  amendment v9 (task sent from Spawn status) and were accepted under D39. This run uses v9 with the local model as
  both the parent's and the child's model, so the child actually attempts the three commands.
- **MOD-11.** On build `9750861c2a` (Tart, GLM 5.3 Flash) items 1, 2, 4 and 5 showed the product's level message, item 6
  is D43's accepted limit, item 7 offered no "don't ask again", and items 3 (`~/.zshrc`) and 8
  (`<Corbanu home>/security_state.json`) were declined twice by the model with no tool call, so the case stayed
  blocked under G-1. This run runs items 3 and 8 only, with the local model.
- **AGG-20P.** Designed in amendment 7 (`agg/amendment7.md`, owner-approved D111) for "the next local-model run";
  it asks that run to capture screens of its candidate before its freeze. Those captures are `screens-v7/` P01-P19.
  Amendment 7 says anything that conflicts with an expected result goes to the owner before the freeze; the integrator
  found no conflict (see `tools.md`), but you should check.

## Integrator choices

- One amendment, **L3**, on top of the documents in force, for these four groups only: MOD-21 (one group), MOD-11 items
  3 and 8 (one group), AGG-20P-A and AGG-20P-B (one group each). It plays the role L2 played for the second
  local-model run: how each case is launched and run on the local model, keys at prompts, waiting, recording, what is
  run, and how results are judged when the model does more than the literal step. It replaces no expected result, and
  each case's own pass rule (MOD cases: their frozen expected results with G-1; AGG-20P: amendment 7's) stays.
- L2's rules L-1..L-5 are the model: reuse them by reference where they fit, and say exactly where L3 differs.
  Amendment 7 already says AGG-20P runs under "the next local-model run's rules (as L-1..L-5)"; L3 is those rules for
  this run.
- Testers get: the Moderate packet (`mod/`, as in the last Moderate runs, with amendments v2–v4, v6, v7 and v9), the
  Aggressive packet (`agg/`, with amendment 7), `amendments-L2.md` (for reference: L3 names which of its rules apply),
  `screens-v6/`, `screens-v7/`, and L3, which governs. Amendment v5 (an older local-model rerun of MOD-09) is not in
  their packet.
- Each group has about 110 minutes (session terminated at 120). One request at a time reaches the local model.
