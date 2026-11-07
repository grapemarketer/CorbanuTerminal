# What the second run found for D136, NA-05 and NA-06, and what the owner decided since (coordinator, 2026-10-22)

The frozen design (`design-cases.md`) and its amendment (`amendments-v2.md`, which replaces the cases it restates) were
run on one fresh machine per group. Three cases are re-run on a newer build. These are observations of the test machine
and of what the screen showed, plus the owner's decisions taken after the run, in user terms.

## Observations

1. **D136 (group G9, home oai1).** Every step was reachable. `t1` was signed in by the coordinator before the tester
   started. Step 2's `account remove openai t1` printed "Signed out and removed openai account `t1`", and step 3 listed
   no `t1`. In window A, nothing said that `t1` had been removed: x1 and x3 still showed `· account t1`, and both
   `afterone` and `aftertwo` were answered, with chatgpt.com traffic at each. Nothing went to the default login.
2. **NA-05 (group R2, home zai3, after PR-01 and GH-02 as amended).** Every step was reachable. In the TUI started with
   `--account personal`, the in-app `/resume` of the `gzero` conversation (recorded only on the removed `work`) opened
   it with no message at all: the session id changed to the conversation's, `/status` showed `· account personal`, and
   `rtwo` went to `personal`. Outside the cases, the coordinator repeated the same `/resume` in a TUI started without
   `--account`; there it was refused with a message naming `work`. The frozen expectation (refuse, stay in the session)
   was written before the owner decided this situation (decision 2 below).
3. **NA-06 (group R3, home zai3).** The rows and the add steps were reachable, and `added` was saved with a kind and a
   fingerprint. At step 3, after "Open its actions", the tester pressed Enter on the action that was highlighted first
   (then labelled "Use for this session"), which started a new session on `added`; it then typed `renamed` into the
   composer, where it was sent as a prompt. The Rename action was never chosen. A later Rename was refused: "This
   session uses zai account `added`. Use another account for this session first, then rename it." Steps 5–7 then
   showed `added`, not `renamed`. Since then the actions are labelled differently (decision 4 below), so the frozen
   step text never named the action it had to avoid.
4. **Test machine.** Unchanged since the second run: `agms-tui start --home H --stdin TEXT ...` pipes TEXT into the
   program's standard input; a `C-c` leaves the terminal open. For D136, `t1` is again signed in by the coordinator
   before the tester starts, possibly several hours before (the machine is paused in between); if the file
   `$AGMS_RUN/evidence/t1-not-signed-in` exists, the sign-in did not complete.

## Owner decisions since the second run (2026-10-22)

1. **An account removed or signed out while a session runs on it is never used again by that session** (from any
   terminal, for example `corbanu account remove openai t1`). The session's next request is refused with text that
   names the account as having no sign-in (removed or signed out) and gives the recovery: `corbanu login --account
   <name>`, or another account in `/providers` or with `--account`. It is never sent on the sign-in the session still
   held, and never on the default login. `/status` then shows the account as unavailable on the provider line and the
   recovery on the Account line; Account usage still lists what the session used on that account. If the account is
   added or signed in again, the session uses the new sign-in.
2. **In a TUI started with `--account X`** (or after choosing "Start a new session on this account" for X in
   `/providers`), an in-app `/resume` of a conversation of the same provider whose recorded account is gone opens it on
   X and shows a one-line notice naming both accounts, for example: "This conversation ran on zai account `work`, which
   is no longer available; it resumes on `personal`, the account selected for this session (with `--account` or in
   `/providers`)." Without `--account` (and without that choice), the resume is refused with a message naming the
   missing account, as before.
3. A conversation that ran on more than one account resumes, with no flag, on the account of its most recent turn.
4. **`/providers` action labels:** an account's "Use for this session" is now "Start a new session on this account",
   and a provider's "Use default for this session" is now "Start a new session on the default account". Both start a
   new session, as before. Rename and Make default are unchanged.

`provider-accounts-doc.md` is the user documentation as it reads on the build under test.
