The round-1 changes went in, and most are correct. But the NA-05 conclusion still claims more than the probe shows, and #1287 repeats that claim.

**Round-1 findings that are now resolved**
- **Finding 1 (probe):** `probe-transcript.txt` now has attempt 2's output. `screens.jsonl`, `actions.jsonl`, `relay.txt` and `session-accounts.txt` back it up: REF-10's exact message at 02:50:32; `qone`→`zpersonal` at 02:50:39 in a new session `3d15…` showing `· account personal`; `qzero` still records only `work`. The record now says there were two attempts.
- **Finding 2 (PR-01 and the amendment):** the G2 failure, the `[observe]` move under v2-Q1, the `pb`/`g0` checks and the changes to other expected sections are all disclosed. These match `amendments-v2.md` lines 58, 100, 215, 309 and 406.
- **Totals:** 23 + 2 + 1 + 2 = 28, which matches the table. Tester verdicts are 22 passed and 6 failed, which is correct.
- **Finding 5 (docs):** the README cites both doc sections ("Choosing an account" and "Fail closed"). I checked the quotes against `docs/provider-accounts.md`.
- **Finding 10 (D136):** the narrowed time window is disclosed, and only the missing recovery text is listed as failed.
- **Findings 6, 9, 12 and 13:** the open owner decisions, REG-01c's Esc behaviour, the other observations and the "What this run does not show" list are all present and match the files.
- **Finding 11:** `candidate-identity.md`, `signin-handoff.md` and `REVIEW.md` now exist.
- **Secrets:** a scan found no tokens, keys or email addresses.

**Findings**

1. **Medium – README NA-05 row, headline and owner decision 2; `integrator-probe-na05/README.md`; #1287.** The probe changed two things at once, so it doesn't show what is claimed.
   - **What differed:** NA-05 (R2) ran in a home with `[provider_accounts] zai = "ghost"` and started `--account personal` (`runs/R2/result.md` line 48). The probe ran in a fresh `zai3` home, where `zai = "personal"` (`agms-prep` line 39), and started with no flag. So both the launch flag and the configured default changed. Note that the docs' rule "`[provider_accounts]` names a missing account: that entry does not apply to the thread" touches exactly the second variable.
   - **Why it matters:** "the launch `--account` wins" is plausible but not shown, and owner decision 2 is framed on it.
   - **#1287:** it lists `zai = "ghost"` in its steps, then calls the probe a "Control" running "the same in-app resume". It concludes "So the launch `--account` wins" and cites only "Choosing an account", not the contradicting "Fail closed" text.
   - **Fix:** in the README, the probe README and #1287, say the probe also differed in `[provider_accounts]` (`personal` vs `ghost`), so the cause is not isolated. Correct #1287's "Control"/"same" wording and add the doc contradiction to it. Better still, add a probe arm: home `zai = "personal"`, start `--account personal`, `/resume` `qzero`.

2. **Low – README headline, "passed in part".** "One check each could not be set up, because 'Use … for this session' starts a new session" is true for NA-10. For ST-02 it misses that the NO-SECRET checks were also untested, because of a tester error (the literal `k` key). Say: "ST-02: d5-2 not reached (new session); NO-SECRET untested (tester error)."

3. **Low – README NA-10 row.** "All checks passed except s2's table" isn't quite right. The advisory s1 cached-input check failed (`runs/R1/result.md` line 144; integrator notes, R1). Add "`[advisory]` s1 omitted the cached-input count".

4. **Low – "What this run does not show", files bullet.** "The other cases rely on the relay and the screens" is wrong for two cases:
   - NA-04's frozen Files line was checked by the tester (`cat CFG`, `runs/R1/result.md` line 104).
   - REG-01b's frozen Files line ("CFG(zai3off) ends with `named_accounts = false`") was left "for the integrator" (`runs/G8/result.md` line 56), and nothing records it being checked.
   - It should say: "NA-04 checked by the tester; REG-01b not checked." D136's Files lines are covered by the integrator notes and the G9 `auth.json` hash.

5. **Low – `harness/host/integrator-notes.md`, R1 NA-10.** The notes still say the multi-row table "is checked in PR-01/NA-08"; the README now says PR-01 and NA-05. NA-08 shows no such table. Add a correction line to the notes (append, don't overwrite).

6. **Low – README headline and D136.**
   - "Keeps answering on it" is presented as part of D136's failure, but it is `[observe]` pending owner decision 1, which the README itself states. Phrase it as: "shows no recovery text (failed); it also kept answering on the removed sign-in (`[observe]`, decision 1)."
   - The same applies to the README's "Defects filed" row for #1286.

7. **Low – record completeness.**
   - `candidate-identity.md` lists a second shakedown clone, `shakedown1-prep-bug-20261021T214835Z`. The README says one shakedown clone, and that clone has no safety files. Disclose it.
   - `runs/G9/signin-handoff.md` shows the request and a "DONE … approved and completed" note, but nothing in it identifies Travis as the approver. Say where the approval is attributed, or word it as "approved (per the coordinator)".
   - `integrator-probe-na05/README.md` doesn't say where `screens.jsonl`, `actions.jsonl` and `session-accounts.txt` came from (presumably `collected.tar`). Say so.

8. **Low – README "Other observations" (R3).** Window B's refusal was shown as "■ Fatal error: …" (`runs/R3/result.md` line 87). Say "fatal error"; "was refused" understates it.

**Readiness:** D136 still fails a blocker. NA-05 needs the owner decision, and also either the isolating probe arm or an honest statement that the cause is not isolated.

VERDICT: CHANGES
