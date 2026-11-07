# Test environment for re-run R1 (facility facts only, integrator, 2026-10-22)

These are facts about the environment; they add no expected results. `s10-frozen/answers.md` stays true except where
this file says otherwise.

- **Machine:** each group runs in its own fresh, isolated macOS virtual machine, one at a time, with a fresh product
  profile and a fresh copy of the test project (`ledger-notes`). Nothing carries over between groups.
- **Candidate:** a newer developer build of Corbanu Terminal than the first run's (the exact digest is given to each
  tester).
- **Tester:** a fresh code-blind tester (GLM 5.3 Flash) per group. It sees only its packet: the frozen design, the
  first run's amendments, amendment R1 (yours, after its review), these environment facts, the navigation note, the
  reference screens and one prompt naming its cases in order. It drives the product only through `agms-tui` (same
  controls as in the first run; `s10-frozen/navigation.md`). Each tester has a hard time limit of 90 minutes.
- **Routes available:** `zai` (`glm-5.3-flash`, real) and `openai` (`gpt-6-luna` and the other listed OpenAI models,
  real). The OpenAI route uses the same connection type as in the first run, so reopen warm-ups happen as there.
  **Not available:** Moonshot (no spend granted) and realtime. The stand-in routes still exist but no re-run case
  needs them.
- **Spend:** real turns cost real money. In the first run, group C's OpenAI turns and warm-ups cost a few cents in
  total (each warm-up roughly $0.001-0.002 at `/cost`'s estimate); group A's Z.AI turns cost a few cents. Send only
  the turns the cases ask for.
- **Automatic reviewer and `/side`:** as in `s10-frozen/answers.md` (Ctrl+C leaves `/side`).
- **`/review`:** in the first run, `/review` offered a choice to review uncommitted changes; the review then ran and
  finished on screen.
- **Screens:** every `agms-tui screen LABEL` call is kept as evidence with its label. A result counts as observed only
  if a captured screen shows it (or the tester copies the exact line from a captured screen into its report). Lines
  the screen scrolled past without a capture do not count.
- **Request pages:** a request's page shows `Request: <ID>` (see `screens/s08-restart-reopen-warmup-request.txt`); its
  technical page shows the `Turn:` line (see `screens/s06-01-side-review-technical.txt`).
