You are a fresh, code-blind test designer writing an amendment to a frozen acceptance test design. Do not read source
code, tests or implementation notes. Read only the files in the current directory: run-observations-r1.md (what this
re-run is for and what the first run found for the five cases), environment-r1.md (the test machine for this re-run),
s10-frozen/design-cases.md (the frozen design), s10-frozen/amendments.md (the first run's frozen review amendments;
where an amendment changes a case, it governs), s10-frozen/answers.md (environment facts of the first run),
s10-frozen/navigation.md and s10-frozen/executor-prompt.md (what the tester is told), and screens/*.txt (reference
screens from earlier builds: layout only, not expected results). Do not open any other path.

Write `amendment R1`: replacement text for ACC-06, ACC-11, ACC-14, ACC-15 and ACC-18 only, for a re-run of those five
cases on a newer build.

Rules:
- Keep each case's ID, purpose and situation, and every frozen (or first-run-amended) expected result. Never weaken an
  expected result and never write one to match what was observed.
- Add the steps needed so that the results run-observations-r1.md lists as unjudged are observed on captured screens
  and judged this time:
  - ACC-11, ACC-14, ACC-18: record Request IDs from request pages before each reopen, re-read the same requests'
    pages after the reopen, and compare; check each Request ID appears once.
  - ACC-18: for every conversation of the profile, open its request list and check that its request costs add up to
    its total, and that its total as seen from another conversation equals its own total. Say how a conversation that
    cannot be opened is treated.
  - ACC-15: the side turn's technical page with its `Turn:` line on a captured screen.
  - ACC-06: as frozen, with every expected result observed on a captured screen.
  Name the screen label the tester must capture for each judged result.
- Repeat earlier frozen cases (ACC-01.., ACC-10, ACC-12, ACC-13, ...) only where one of the five depends on the state
  they create; for each repeated case say whether it is set-up only (not judged) and which text it uses. Do not add
  cases or re-run ACC-35, ACC-04 or ACC-22. Keep real turns to what the cases need (environment-r1.md: spend).
- Avoid creating conversations that hold only a warm-up request where you can.
- Write each amended case in full (no "as before" references) using the conventions of s10-frozen/design-cases.md
  as amended; restate in full any convention you change.
- Give a re-run group plan: one fresh machine per group, a route per group from environment-r1.md, about 60-80
  minutes each (hard limit 90), the cases in order with set-up steps marked.

Reply with: the list of what you changed and why; the complete amendment text; open questions; nothing else.
