You are a separate, independent reviewer of a proposed amendment to a frozen code-blind acceptance test design. You did
not write it. Do not read source code, tests, implementation notes or prior results beyond the files here. Read only
the files in the current directory: intent.md, tools.md, review-facts.md, provider-accounts-doc.md, screens/*.txt,
design-cases.md (the frozen design), amendments-v2.md (the frozen first amendment, which replaces the cases it
restates), run-observations-v2.md, run-observations-v3.md (what the second run found for D136, NA-05 and NA-06, and
the owner's decisions since) and proposed-amendment.md (the designer's amendment v3). Do not open any other path.

Review the amendment for:
1. Scope: it replaces only D136, NA-05 and NA-06 and keeps their case IDs; step changes are limited to what
   run-observations-v3.md shows was ambiguous or led away from the intended situation; nothing else is changed.
2. Expected results: each changed expectation follows an owner decision in run-observations-v3.md and names it; no
   other expected result is weakened or written to match what was observed; questions still unsettled are `[observe]`
   or open questions.
3. Feasibility: a literal-minded AI tester can follow every amended step with only the tools in tools.md,
   review-facts.md and run-observations-v2.md item 1 (`--stdin`), inside about 80 minutes per group; each case's
   starting state exists; the group plan keeps every dependency (D136 alone in the only oai1 group; NA-05 after PR-01
   and GH-02 as amended in v2).
4. Evidence: every expected result says how it is observed.

Reply with: (a) the problems you found and what you changed; (b) the COMPLETE revised amendment, ready to freeze word
for word (every amended case in full, and the re-run group plan); (c) open questions; and last, on its own line,
`VERDICT: APPROVE` (no change needed) or `VERDICT: CHANGES` (your text in (b) replaces the proposal).
