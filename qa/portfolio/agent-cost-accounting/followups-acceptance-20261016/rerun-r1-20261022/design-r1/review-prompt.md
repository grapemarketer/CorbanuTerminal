You are a separate, independent reviewer of a proposed amendment to a frozen code-blind acceptance test design. You did
not write it. Do not read source code, tests, implementation notes or prior results beyond the files here. Read only
the files in the current directory: run-observations-r1.md, environment-r1.md, s10-frozen/design-cases.md (the frozen
design), s10-frozen/amendments.md (the first run's frozen review amendments), s10-frozen/answers.md,
s10-frozen/navigation.md, s10-frozen/executor-prompt.md, screens/*.txt and proposed-amendment.md (the designer's
amendment R1). Do not open any other path.

Review the amendment for:
1. Scope: it replaces only ACC-06, ACC-11, ACC-14, ACC-15 and ACC-18 and keeps their IDs, purposes and situations;
   any repeated earlier case is set-up for one of the five and marked as such; nothing else is changed; ACC-35,
   ACC-04 and ACC-22 are not re-run.
2. Expected results: every frozen (or first-run-amended) expected result of the five cases is kept; none is weakened
   or written to match what was observed; the results run-observations-r1.md lists as unjudged (Request IDs unchanged
   and appearing once for ACC-11/14/18; per-conversation sums for every conversation in ACC-18; the `side:` label on a
   captured screen for ACC-15; ACC-06 as frozen) are now observed and judged.
3. Feasibility: a literal-minded AI tester can follow every step with only the controls in s10-frozen/answers.md,
   environment-r1.md and the navigation note, within about 60-80 minutes per group (hard limit 90); each case's
   starting state exists; the group plan keeps every dependency; real spend is limited to what the cases need.
4. Evidence: every judged result names the screen label that must show it; nothing relies on lines the screen
   scrolled past.

Reply with: (a) the problems you found and what you changed; (b) the COMPLETE revised amendment, ready to freeze word
for word (every amended case in full, and the re-run group plan); (c) open questions; and last, on its own line,
`VERDICT: APPROVE` (no change needed) or `VERDICT: CHANGES` (your text in (b) replaces the proposal).
