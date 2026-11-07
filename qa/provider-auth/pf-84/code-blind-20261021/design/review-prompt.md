You are a separate, independent reviewer of a proposed code-blind acceptance test design. You did not write it. Do not
read source code, tests, implementation notes or prior results. Read only the files in the current directory:
intent.md (the feature and the owner's acceptance criteria and decisions), tools.md and review-facts.md (the test
machine), provider-accounts-doc.md (the user documentation), screens/*.txt (reference screens of earlier builds) and
proposed-cases.md (the designer's proposal). Do not open any other path.

Review the proposal for:
1. Coverage: every item in intent.md (the eight criteria, D136, both #426 legs, ESC-01, ESC-03, ESC-04, GH-02, PR-01,
   REG-01 with its stated gap closed, and D86/D124/D125 including a home with no named accounts) is tested by at least
   one case, and blockers are marked as intent.md and the proposal's own rules require.
2. Correctness against the packet: expected results follow from intent.md, the owner decisions and the user
   documentation, not from guesses about the implementation. Where the packet does not settle a result, the case must
   record what happens and mark that result "observe" (not pass/fail), or say which owner decision it needs.
3. Feasibility: a literal-minded AI tester can follow every action with only the tools in tools.md and review-facts.md,
   inside each group's time budget; the starting state each case needs exists (from the home, or from an earlier case
   in the same group); live-account steps respect review-facts.md items 6 and 7.
4. Evidence: every expected result says how it is observed (screen, exit line, relay, files), and nothing depends on
   the tester seeing the relay or the vault.

Settle the designer's "Missing information" items where the packet allows; leave the rest as named open questions.

Reply with: (a) a short list of the problems you found and what you changed; (b) the COMPLETE revised design, ready to
freeze word for word: the conventions, the group plan and every case in full (do not abbreviate or refer back to the
proposal; unchanged cases are repeated in full); (c) the open questions that remain; and last, on its own line, either
`VERDICT: APPROVE` (the proposal needed no change) or `VERDICT: CHANGES` (your revised design in (b) replaces it).
