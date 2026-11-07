Round 3 needs one more set of fixes. Every round-2 finding is resolved, and arm B does isolate the cause. But the decision framing still misreads the docs, and the arm-B disclosure has gaps.

**Round-2 findings, checked against the files**
- **Finding 1 (resolved):** the only difference between the arm-B scripts and attempt 2's is `--account personal` at TUI start (I diffed both the host and guest scripts).
  - The evidence matches the README. `screens.jsonl` shows no message after `/resume`, and `qone` is answered. The relay shows `zpersonal` at 03:27:15. `session-accounts.txt` shows `8273…` recording `['work','personal']`, and `/status` shows `· account personal` with Session `8273…`.
  - The probe README and the NA-05 row now disclose the `ghost`/`personal` difference from R2. #1287 drops "Control"/"same", says the probe homes differ in that entry, cites both doc sections and links arm B.
- **Finding 2 (resolved):** the ST-02 wording in the headline is fixed.
- **Finding 3 (resolved):** the advisory s1 cached-input failure is in the NA-10 row.
- **Finding 4 (resolved):** the files bullet now says NA-04 was checked by the tester and REG-01b was not checked.
- **Finding 5 (resolved):** a correction line was appended to `integrator-notes.md`; the old line is kept.
- **Finding 6 (resolved):** the headline and the #1286 row mark in-memory use as `[observe]`, decision 1. The issue body leaves it as an owner question.
- **Finding 7 (resolved):** shakedown1 is disclosed, approval is attributed "per the coordinator", and the probe files' origin is given.
- **Finding 8 (resolved):** R3 now says "fatal error", and the quote matches `runs/R3/result.md`.

**New findings**

1. **Medium – README NA-05 row and owner decision 2; #1287 "The docs disagree".** The record says the docs contradict each other, but it leaves out one sentence that bears on exactly this question.
   - `docs/provider-accounts.md` line 91, inside "Fail closed", comes right after the missing-account bullets. It says: "An explicit `--account` of the same provider still wins." The next sentence ("With the feature off, the resume is refused…") shows it is about resume.
   - So "Fail closed" is ambiguous rather than flatly opposed to "Choosing an account". It says a missing recorded account makes `/resume` "show it and stay", and also that an explicit `--account` still wins. It never says whether a *launch* `--account` counts for an in-session `/resume`.
   - **Fix:** quote line 91 in the README and in #1287, and say "the docs are ambiguous (line 91 vs the `/resume … stays on the current conversation` bullet)" instead of "contradict each other".
   - #1287's option (a) says to "fix the 'Fail closed' text". It should say the fix is to qualify that bullet, since the "explicit `--account` wins" rule is already in that section.

2. **Low – `integrator-probe-na05/README.md` (arm B package).** The package change is understated. The README says arm B used "other companion binaries (`codex-code-mode-host` …)".
   - `armB/package-files-probe-armB.txt` compared with `harness/package-files.txt`:
     - **Absent:** `corbanu-acp`, `corbanu-walletd` and `codex-path/rg`.
     - **Different digests:** `codex-code-mode-host`, `codex-package.json` and `setup-telegram.sh`.
     - Only `corbanu` (`bbae7d37…`) and a few resource files are identical.
   - Strictly, arm A and arm B therefore differ in two ways: the flag and the package contents.
   - **Fix:** list what was missing or different, and say why it cannot affect the result (the same `corbanu` binary does account resolution; the missing companions are not on the `/resume` path). Alternatively, soften "so the cause is the launch `--account`" to note that the package also differed.

3. **Low – `candidate-identity.md` and the README "Safety checks per clone" bullet.**
   - `candidate-identity.md` has no lines for the arm-B clone (`probe-na05b-20261022T032245Z`) or the two discarded arm-B clones. The README points there for "per-clone binary and harness digests", and the harness table says probe = v4, but nothing records arm B's `agms-tui`/`agms-prep`/`agms-launch` digests.
   - The README's safety glob `integrator-probe-na05/clone-*` does not match `armB/clone-*` or `armB/discarded/*`.
   - **Fix:** add the arm-B clone(s), or state that the digests were not captured. Extend the path list.

4. **Low – `armB/discarded/failed-stage-probeB-0315-guard-final.txt`.** This file is empty: "SECURITYAGENT-SEEN lines:" and "checks:" have no values.
   - The README's claim of "0 `SECURITYAGENT-SEEN` lines" per clone is therefore not shown for this clone.
   - **Fix:** say its guard recorded nothing because staging failed before any product step.

5. **Low – `integrator-probe-na05/README.md` (transcript provenance).**
   - The README says the "Attempt 1 and 2 transcripts were written to the host run log" because of the redirect-on-its-own-line bug.
   - `armB/probe-na05-armB.sh` has the same bug: line 22 is a bare `> $O/probe-transcript.txt`. `armB/probe-transcript.txt` starts with the host's `gate … PASS` line, so it also came from the host log.
   - **Fix:** say this for arm B too.

6. **Low – README Method, "Safety checks" bullet.** It still says "Travis's approval: runs/G9/signin-handoff.md". The handoff file does not name an approver; the README states this elsewhere ("the record cannot show who approved").
   - **Fix:** change the label to "Sign-in approval (per the coordinator)".

**Other checks:** totals (28; 23/2/1/2; tester verdicts 22/6) are unchanged and correct. #1286 matches the record, and its comment matches R3. No secrets were found in the arm-B files. Readiness is stated correctly: D136 blocks, and NA-05 needs an owner decision.

VERDICT: CHANGES
