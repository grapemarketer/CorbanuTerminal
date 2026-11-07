**(c) Open questions**

- **Q1 (owner): resume after two accounts.** A conversation ran on `work`, then on `personal` through `resume --account personal`. Which account should a later `corbanu resume <id>` with no flag use: the latest turn's (as the first run saw), or the account it was first recorded on?
  - provider-accounts-doc.md says a fork uses "the conversation's current account … as a resume does", which points to the latest turn.
  - The 2026-10-12 PR-01 expected `work`, the first account.
  - PR-01's pq records this mixed case.
  - For GH-02 step 6, PR-01's `<id>` ends on `work` (pback and ptwo), so the first and latest readings both give `work`. If the owner confirms either reading, g6 can be judged against GH-02's step 5 expectation.
- **Q2 (owner): missing `/providers` rows.** If a complete SURVEY shows that `work` is not reachable, NA-06/07/10 now fail. Is a list capped at 8 rows with no hint also a defect of its own?
- **Q3 (coordinator/owner): NA-13.** Check whether the G9 n13-key capture was taken in the API-key field or in a name field (observation 4 was seen in the OpenAI add). If it was a name field, the masking check was never set up. provider-accounts-doc.md says *Add another account* on OpenAI offers a masked API key "whatever provider the session runs on". So the n13-key leg alone could be re-run in a Z.AI home, with FIELD and KEYIN and without `t1`, if the owner agrees to that change of home.
- **Q4 (owner/integrator): NA-01 step 3.** Without a pipe it is refused whatever the argument. The integrator should check the first run's step-3 text. If the refusal was "pipe the secret in", then "a key given on the command line is refused" was never tested. Should a variant that pipes stdin and also puts a key on the command line be added? A related point: a step-3 error that repeats `agms-fake-zextratwo-…` is `[observe]` by the frozen text, but it also matches NO-SECRET(zextra…)'s label. The owner should say which rule wins (AC6 suggests a failure).
- **Q5 (owner): ESC-04.** If saving the key returns to the provider list instead of starting the session (REF-12/14 show nothing after the key screen), is that extra "Done"-style step itself a change from before? Not finishing at all now fails as frozen.
- **Q6 (integrator): the first run's Z.AI add.** Check whether the "invalid account name" error repeated the key. Under the restated rule that would be a NO-SECRET failure; a key visible only in the name field the tester typed it into is `[observe]`.
- **Q7 (owner): name field timing.** If text and Enter arrive within a second (as with a paste), Enter adds a blank line instead of submitting. Is that a defect, or acceptable paste handling?

VERDICT: CHANGES
