**PF-84 evidence review: changes needed**

The record is mostly accurate, but three claims go beyond the evidence. The integrator probe behind NA-05's "owner decision" left nothing in the record that shows its result. PR-01 passes only because amendment v2 moved a frozen check that had failed into `[observe]`. And NA-10 and ST-02 count as passes for checks that never actually ran.

**What checks out**
- **Frozen texts:** `design-cases.md` is word for word the reviewer's section (b) in `design/review-output.md` (713 lines). `amendments-v2.md` is word for word section (b) of `amend-v2/review-output.md`, plus a header. Both FROZEN.sha256 files and both packet hash lists match.
- **Spot checks:** the README rows for NA-02, PR-01, NA-04, NA-05, NA-09, NA-12, D136 and ST-03 match each tester's `result.md` and `relay-product-requests.txt` (labels and times).
- **Totals:** 28 cases; 25 passed, 1 not complete, 2 failed. Tester verdicts were 22 passed, 6 failed.
- **Secrets:** no email address, device code, `eyJ`, `sk-` or `rt_` string appears anywhere in the record; device codes are redacted to `<CODE>`.
- **Clone safety:** all 15 run clones show 0 `SECURITYAGENT-SEEN` lines, 0 synthetic credential files and 0 home entries left.
- **Fair reclassifications:** ESC-01 is justified: `agms-tui` runs `exec /bin/cat` after the program exits, so a typed line appears twice on a restored terminal. NA-06 "not complete" and the D136 failure are stated honestly.

**Findings**

1. **High – `integrator-probe-na05/`.** `probe-transcript.txt` and `attempt1-exec-conversation-not-in-picker.txt` are both empty. In `probe-na05.sh`, the `> $O/probe-transcript.txt 2>&1` redirect sits on its own line, so it only creates an empty file and the probe's output was never saved. No screens or `collected.tar` were kept either.
   - `relay.txt` (qzero→`zwork`, qone→`zpersonal`) fits both a refusal and a silent switch to another account, so it can't tell them apart.
   - So "started without `--account`, the same resume is refused with REF-10's message" has no support in the record. #1287 quotes that message anyway.
   - Fix: re-run the probe with its transcript and screens saved, or say its result is unrecorded and leave NA-05 as plainly **failed**.
   - The probe also had two attempts, not "one".

2. **High – PR-01 and the amendment summary.**
   - Frozen PR-01 expected `· account work` and `ptwo → zwork` from a no-flag `resume <id>` after a turn on `personal`. G2 got `personal`, which is a direct failure of that check, not a premise problem.
   - Amendment v2 moved that exact situation into `[observe]` pq (owner question Q1) and added a re-anchor step, so the new p2 tests something easier.
   - Other expected sections changed too:
     - new `pb` and `g0` checks;
     - p2 becomes `[observe]` if pb fails;
     - a new NO-SECRET exception 2;
     - an "integrator may reclassify" clause in NA-06;
     - new fail rules for ROW and ESC-04.
   - The README should say: "PR-01's frozen check for a no-flag resume after a turn on another account failed in G2; the amendment made it `[observe]` pending owner Q1; PR-01's pass does not settle it."
   - It should also say the expected sections changed as listed, with no expected result weakened except as listed. The "no weakening" rule is stated in `amend-v2/prompt.md` and `review-prompt.md`, not in the frozen amendment itself.

3. **Medium – ST-02 "passed (premise)" overstates.**
   - d5-2 never set up the D86 "session has used a named account" clause, which ST-02 was added to cover (reviewer item 9). Each "Use … for this session" starts a new session, so that clause is untested, not passed.
   - The tester piped the literal `k` as both keys (`actions.jsonl` lines 19 and 49). That makes NO-SECRET(zextra 29044) and NO-SECRET(zextrb 30117) vacuous.
   - "dtwo → zextrb" is an inference: the relay label is `other:8254c329`, which the integrator matched to `sha256("k")`.
   - The tester also opened an unplanned Deactivate dialog.
   - Should say: "passed except d5-2 not reached; NO-SECRET untested; tester deviated from the action."

4. **Medium – NA-10.**
   - s2 is "not reached", not "passed": a multi-row table after an in-session switch can't be set up through `/providers`.
   - "multi-account tables were shown in PR-01/NA-05" covers resumed conversations, not a switch. The integrator notes say PR-01/NA-08, so the two disagree.
   - That "Use for this session" starts a new session, leaving the current conversation, should go to the owner as a question.
   - Report NA-10 and ST-02 in their own category, not inside the "25 passed".

5. **Medium – NA-05 doc reading.** The README says the behaviour follows "Choosing an account". But the "Fail closed" section of `docs/provider-accounts.md` says: "Inside a session, `/resume` shows it and stays on the current conversation". Cite both sections and call it an ambiguity in the docs.

6. **Medium – open owner questions are not listed.** None of these appear in the README:
   - Q1: may a session keep using credentials held in memory after its account is removed? This decides how #1286 should be fixed.
   - v2 Q1: which account a no-flag resume uses after a conversation has run on two accounts.
   - Q3 / v2 Q4: the key passed as a command-line argument is repeated in the usage error. The v2 reviewer says AC6 suggests a failure; the README calls it only `[observe]`.
   - Q6: the footer reads `via zai default` while on a named account (seen again in R1 and R2).
   - v2 Q5 (the extra "Done" step in ESC-04) and Q8 (ST-03 running after the removal).

   Add an "Open owner decisions" section.

7. **Medium – file checks.** The frozen **Files** checks for NA-08, NA-09 and PR-01 in R2 are not recorded. The integrator notes cover the relay only, and no session files are in the record. Record those checks or mark them not checked.

8. **Low – "nothing sent".** Every Z.AI group shows the product making `passthrough_error chatgpt.com:443` attempts, including inside refusal windows (G1 at 22:41:38, 22:41:43 and 22:42:06). Say "no model request" and note or explain these attempts in homes with no OpenAI sign-in.

9. **Low – REG-01c.** "Esc behaves as before" isn't supported. Esc started a new session (footer `esc start new`, not REF-06's `esc quit`), and the frozen line is `[observe]`. State what actually happened.

10. **Low – D136.**
    - The frozen relay check runs "to the end of the group"; the README narrows it to 22:23:43–22:28:54 without saying so. The narrowing is sensible, since ST-03 sends `odefault` by design, but it should be disclosed.
    - "Model provider · account t1" is listed under **Failed**, but the tester passed the x3 "not healthy" check, with an ambiguity. The only failed line is the missing recovery text.

11. **Low – record completeness.**
    - "Safety checks per clone": the shakedown clone has no `clone-*` files, and the probe clone has only `erase-test-data`.
    - Several claims point to files outside the record: `provision.out`, the codesign/TeamIdentifier check, and Travis's approval in `workers-20261002/pf84-cb1.signin.md`.
    - `signin.txt` shows `SI<CODE>=0` (cosmetic over-redaction).
    - The README links `REVIEW.md`, which doesn't exist yet.

12. **Low – unmentioned observations.**
    - G3: an unexplained "Choose replacement" dialog appeared on `/quit`.
    - R3 NA-07: window B's `/status` kept `· account work` after the removal, the same pattern as #1286, though it did refuse with recovery text.

13. **Medium – "What this run does not show" is incomplete.** Add:
    - the D86 "has used a named account" clause;
    - NA-10's table after an in-session switch;
    - PR-01's frozen p2 situation;
    - ST-02's NO-SECRET checks;
    - the NA-05 probe result, which is not in the record;
    - the D3 approvals-off leg (plan item 4);
    - the file checks from finding 7.

**Readiness for the Windows gate and sign-off:** not ready. D136 fails a blocker. NA-05 needs a decision and a probe that is actually recorded. NA-06's rename needs a re-run. Owner Q1 (D136) and v2 Q1 (resume after two accounts) are still open.

VERDICT: CHANGES
