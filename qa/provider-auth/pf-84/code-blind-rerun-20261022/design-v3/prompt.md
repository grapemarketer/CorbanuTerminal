You are a fresh, code-blind test designer writing an amendment to a frozen acceptance test design. Do not read source
code, tests or implementation notes. Read only the files in the current directory: intent.md (the feature and the
owner's criteria), tools.md and review-facts.md (the test machine), provider-accounts-doc.md (user documentation for
the build under test), screens/*.txt (reference screens from earlier builds), design-cases.md (the frozen design),
amendments-v2.md (the frozen first amendment; for every convention and case it restates, its text replaces the frozen
one), run-observations-v2.md (why amendment v2 was written) and run-observations-v3.md (what the second run found for
D136, NA-05 and NA-06, and the owner's decisions since). Do not open any other path.

Write `amendments-v3`: replacement text for D136, NA-05 and NA-06 only, for a re-run of those three cases on a newer
build.

Rules:
- Change a step only where run-observations-v3.md shows it was ambiguous or led the tester away from the situation the
  case was written to test (for NA-06: reaching and choosing the Rename action; the tester must screen the action list,
  move the highlight until the highlighted line contains "Rename", screen it and check it before pressing Enter, and log
  "Rename not found" if no line contains it). Keep every case ID and every frozen step that worked.
- Where an owner decision in run-observations-v3.md now settles a question the frozen text left as `[observe]` or an
  open question, or decides the intended behaviour for the case's situation, write that expected result from the owner
  decision and name the decision. Do not otherwise weaken or change an expected result, and never write one to match
  what was observed.
- NA-05 keeps its situation: a TUI started with `--account personal` in zai3, right after GH-02 (as in amendments-v2),
  in-app `/resume` of the conversation recorded only on the removed `work`. Its expected result follows owner decision
  2. Add an `[observe]` step if you need one to show that the resumed conversation's earlier turns are present.
- D136 keeps its situation: window A runs on `t1`, `t1` is removed from another terminal, window A keeps going. Its
  expected result follows owner decision 1. Keep the relay and file checks.
- Write each amended case in full (no "as before" references) using the conventions of design-cases.md as amended by
  amendments-v2.md; restate in full any convention you change.
- Give a re-run group plan: one fresh machine per group, homes from tools.md, about 80 minutes each. D136 must be in
  the only group that uses home oai1 (where `t1` is signed in); include any frozen case the three cases depend on (for
  example the cases that set up NA-05's starting state, with their amendments-v2 text, unchanged), and say which text
  each included case uses.

Reply with: the list of what you changed and why; the complete amendment text; open questions; nothing else.
