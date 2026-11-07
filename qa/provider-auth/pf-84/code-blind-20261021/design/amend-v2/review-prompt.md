You are a separate, independent reviewer of a proposed amendment to a frozen code-blind acceptance test design. You did
not write it. Do not read source code, tests, implementation notes or prior results beyond the files here. Read only
the files in the current directory: intent.md, tools.md, review-facts.md, provider-accounts-doc.md, screens/*.txt,
design-cases.md (the frozen design, which stays in force), run-observations.md (what the first run found) and
proposed-amendment.md (the designer's amendment). Do not open any other path.

Review the amendment for:
1. Scope: it changes only what run-observations.md shows was unreachable or set up a different situation than intended,
   and keeps every case ID; nothing else in the frozen design is changed.
2. No weakening: no expected result is weakened or changed to match what was observed; questions the packet does not
   settle are `[observe]` with a named owner question, while the situation the owner described is still tested with
   its frozen expectation.
3. Feasibility: a literal-minded AI tester can follow every amended step with only the tools in tools.md,
   review-facts.md and run-observations.md item 1 (`--stdin`), inside about 80 minutes per group; each case's starting
   state exists; the regrouping keeps every dependency between cases.
4. Evidence: every expected result says how it is observed.

Reply with: (a) the problems you found and what you changed; (b) the COMPLETE revised amendment, ready to freeze word
for word (every amended convention and case in full, and the re-run group plan); (c) open questions; and last, on its
own line, `VERDICT: APPROVE` (no change needed) or `VERDICT: CHANGES` (your text in (b) replaces the proposal).
