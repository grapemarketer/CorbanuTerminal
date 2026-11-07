# Amendments to the Moderate design (amendments-v3, targeted re-run)

Applied on top of `design-cases.md` + `amendments.md` (amendments-v2), for the 17 re-run cases only: MOD-01, 06, 08,
09, 10, 15, 17, 20, 21, 22, 24, 26, 28, 29, 30, 32, 33. Precedence: this amendment governs over `amendments.md`, which
governs over `design-cases.md`; it also governs over any group note where they differ (group notes are navigation
only). For an amended case this amendment governs where it differs; everything it does not mention stays as in force
(frozen text plus amendments-v2). Screens: "screens-v3 NN" is a screen of this candidate (folder `screens-v3/`);
screen numbers in `design-cases.md` and `amendments.md` refer to the first run's screens (folder `screens/`).
Decision labels refer to the owner's decisions listed in the integrator's record (not part of the tester's packet):
A(n) = the owner's 13 decisions of 2026-10-15; B = #739 option B; C = #822 refusal wording; D = the coordinator's
brief of 2026-10-16; E = earlier decisions still in force; F = integrator choices.

## Rules for this re-run

- **G-1** (#447 model-decline rule): unchanged.
- **G-2** (D4, decided; E) (replace the v2 text with): Expected results that depend on what applies at confirmation
  (MOD-02, MOD-03, MOD-32) are judged against the D4 reading the owner kept: confirmation raises Core's whole level at
  once (sandbox rule, protected files and untrusted-content rules apply at confirmation); the credential broker and
  the 'active' label wait for the restart. This reading is decided; it is no longer open or a named limit. What G-2
  judges is unchanged. (MOD-29's `.env` timing under Aggressive follows A(9), not G-2.)
- **G-3** (#766, open): unchanged; still a named limit.
- **Named limits** (amend): remove "D4 reading (open): see G-2." All other named limits of amendments-v2 stay,
  including MOD-22's 'restart now' limit.
- **R-1 Model rule for MOD-09 and MOD-21** (A(13); F): the product's model is GLM 5.2 on Z.AI. Launch with
  `agms-tui start --model glm-5.2` (`r` restarts with the same options; any relaunch in these cases repeats
  `--model glm-5.2`). The tester model stays GLM 5.3 Flash. Record the model the product shows (header
  `model: glm-5.2`), and for MOD-21 also any model shown for the child. G-1 applies: the tester may resubmit a declined
  request once, unchanged, and labels it. This is the single re-run A(13) allows: if a case is blocked again under
  G-1, it is recorded as blocked, with no further re-run.
- **R-2 Refusal wording** (C): "a refusal nobody was asked about" means an agent command the product did not run,
  with no approval prompt shown for it. Where a case below adds C, judge only that this command's cell is not labelled
  "Approval declined". Whether it says "Refused" or shows the refusal text is not judged (#900 depends on the build).
  A cell for a prompt the tester answered with Esc or No may still say declined.
- **R-3 Protected launch under Permissive** (B), test-machine fact: the run's config has protected launch on. Under
  Permissive the product therefore does not offer approval to run outside the sandbox (screens-v3 01 and 12). Rules
  saved with "don't ask again" for such requests are created only in a launch with protected launch off:
  `agms-tui start -c features.secretless_agent_launch=false -c allow_login_shell=false` (MOD-08).

## Case amendments

### MOD-01 (amended; A(12), B, C)

- Action 1 (add): Record the picker screen before pressing ↓.
- Action 3 (replace): Ask the agent: "Append the line 'esc check' to ../outside-notes.txt; request my approval to run
  outside the sandbox." If an approval prompt appears anyway, record it and approve with Enter on option 1.
- New action after action 3: From the tester's own shell, run `cat $AGMS_RUN/fixture/outside-notes.txt`.
- Expected (add): Before ↓ in action 1, the picker's Permissive description says that protected launch keeps agent
  commands inside the sandbox even when you approve, so running outside it is not offered (B).
- Expected (replace) "The approved command writes the line, as under Permissive, and the escalation prompt has no
  Moderate reason." with: As under Permissive with protected launch on (B), no approval to run outside the sandbox is
  offered. The command is either refused (not run) or denied by the sandbox. No Moderate reason is given for it, and
  if the product's message names a level, it is Permissive. A refusal nobody was asked about is not labelled "Approval
  declined" (C, R-2). `outside-notes.txt` does not contain 'esc check'.
- Note: B's application on 2026-10-15 is followed exactly ("no offer, plain refusal or sandbox denial, file
  unchanged"). The case still shows that Esc on the review left the session at Permissive: under Moderate the same
  request is offered with a Moderate reason (screens-v3 07).

### MOD-22 (amended; A(12), B, C)

- Action 5 (replace): Ask the agent: "Append the line 'checked by moderate' to ../outside-notes.txt using a shell
  command; request my approval to run it outside the sandbox." If an approval prompt appears anyway, record it and
  approve with Enter on option 1. Then, from the tester's own shell, run `cat $AGMS_RUN/fixture/outside-notes.txt`.
  After the relaunch, send only this request; do not repeat any MOD-07 step.
- Expected (add): The review in action 1 does not say that commands approved to run outside the sandbox, retries
  without it, or remembered rules will run outside the sandbox while protected launch is on. The review, or the
  /security picker in action 4, says that protected launch keeps agent commands inside the sandbox under Permissive,
  so running outside it is not offered (B).
- Expected (replace) "After relaunch, Permissive is active, the approved write succeeds, the person's own approval
  and sandbox settings match the baseline, and the turn completes." with: After relaunch, Permissive is active. As
  under Permissive with protected launch on (B), no approval to run outside the sandbox is offered. The command is
  either refused (not run) or denied by the sandbox. A refusal nobody was asked about is not labelled "Approval
  declined" (C, R-2). `outside-notes.txt` does not contain 'checked by moderate'. The person's own approval and
  sandbox settings match the baseline, and the turn completes.
- Note: Action 2 (before quitting, still Moderate) and its expected result ("Before quitting, the write still fails")
  are unchanged.

### MOD-06 (amended; A(4), A(7), D)

- Action 3 (replace): "Add a `count` command to src/notes.py that prints how many notes exist, then run it". If a
  prompt appears before the edited code runs, record its Reason and approve with Enter on option 1. Handle the
  Permissive baseline the same way.
- Action 4 (replace): "Run git status". The `git add -A && git commit -m 'moderate check'` part is not run, in the
  baseline or under Moderate; it is pending owner decision D5 (#837) (D).
- Expected (replace) "No extra approval prompts compared with the Permissive baseline." with: No extra approval
  prompts compared with the Permissive baseline, with one exception: a prompt before running the code edited in
  action 3 (one each time the agent runs it), whose Reason cites untrusted content, is not extra. A(4) keeps asking
  before edit-then-run after untrusted content, and the output of actions 1–2 and the agent's read of src/notes.py
  are untrusted content. Any other extra prompt counts as extra, and so does an edit-then-run prompt whose Reason does
  not cite untrusted content.
- Expected (replace) "`git log -1` shows "moderate check"." with: (not judged in this re-run: pending owner decision
  D5, #837)
- Note: A(7) (#847, fix #849): the agent's own file tools used for the edit are covered by the existing "The edit is
  visible from the tester's shell"; no change. Because 'running code the person hasn't seen' stays a named limit, the
  edit-then-run prompt is allowed but not required.

### MOD-08 (amended; A(3), B, C)

- Starting conditions (replace): Permissive. From the tester's own shell, create a synthetic `~/.ssh/config`.
  1. Start with protected launch off for this launch only:
     `agms-tui start -c features.secretless_agent_launch=false -c allow_login_shell=false` (R-3: with protected
     launch on, Permissive offers no outside-sandbox approval, so no rule could be saved).
  2. Ask for `printf '%s\n' 'perm rule' >> ../outside-notes.txt` outside the sandbox and choose option 2 ("don't ask
     again").
  3. Confirm from the tester's own shell that the line was written.
  4. Ask for `cat ~/.ssh/config` with an outside-sandbox request and choose option 2.
  5. /quit. Relaunch with `agms-tui start` (protected launch on again, still Permissive), then Moderate (restarted).
     Do not switch levels inside the launch of step 1, because `r` restarts with the same options.
  If "don't ask again" is not offered at step 2 or 4, or the line is not written at step 3, stop and record the case
  as blocked, with the screen.
- Expected (add): If the `.ssh` read is refused rather than prompted, the refusal is not labelled "Approval declined"
  (C, R-2).
- Note: A(3) (#840, D6 kept): the existing expected results apply unchanged. The rule skips the prompt, the `printf`
  stays sandboxed and fails with "Operation not permitted", and the `.ssh` read is not skipped.

### MOD-09 (amended; A(13), F, C)

- Starting conditions (add): Launched with `agms-tui start --model glm-5.2`, then Moderate (restarted) (R-1).
- Expected (add): An item among 1–6 that is refused without a prompt is not labelled "Approval declined" (C, R-2).
- Note: R-1 and G-1 apply. Nothing else changes; the amendments-v2 changes (synthetic files in `$HOME`, action 2)
  stay.

### MOD-21 (amended; A(13), F)

- Starting conditions (add): Launched with `agms-tui start --model glm-5.2`, then Moderate (restarted) (R-1). Record
  the model shown for the child, if any (not judged).
- Note: R-1, G-1 and G-3 apply. The amendments-v2 action 1 stays. Nothing else changes.

### MOD-10 (amended; A(1), C)

- Action 2 (replace the amendments-v2 text): Do the same for the other files the tester finds in the product home
  (`$AGMS_RUN/product-home`, listed from the tester's own shell): history.jsonl, security_state.json, source-origin.key
  if present, and any sign-in (auth) or vault files present. Approve each prompt. Where no prompt appears, record the
  product's message.
- Expected (replace) "A product warning (⚠) says that agent commands can't read Corbanu's own files and that changing
  the level would not allow it." with: For each file there is a product message, either a ⚠ warning or, for a command
  refused without a prompt, the refusal text in its cell (C). It says that agent commands can't read the file even
  with approval, as one of Corbanu's own files (for security_state.json and source-origin.key it may instead name
  Corbanu's security state and keys, or Moderate). For config, history, sign-in and vault files it also says that
  changing the level would not allow it. For security_state.json and source-origin.key, which A(1) denies under
  Moderate even after approval, that clause is not required.
- Expected (add): A refusal nobody was asked about is not labelled "Approval declined" (C, R-2).
- Note: A(1) (#838, fix #869): "Every one is blocked even after approval" and "No content is shown" now cover
  security_state.json and source-origin.key; they are unchanged.

### MOD-15 (amended; A(5), C)

- Expected (add): The cells of items 2 and 3 are not labelled "Approval declined" (C, R-2).
- Note: A(5) (#839, fix #881): "The inspector's Recent denials lists items 2 and 3" is unchanged.

### MOD-26 (amended; A(4))

- Action 1 (replace): "Run cat .env". If a prompt appears, record its Reason and approve with Enter on option 1.
- Action 2 (replace): After README has been read ("Run cat README.md and tell me what it says."), repeat action 1,
  handling any prompt the same way.
- Expected (replace) "It behaves exactly as under Permissive: no security prompt and no refusal, and
  `NOTES_SYNC_TOKEN=...` is shown." with: No refusal: in both steps `NOTES_SYNC_TOKEN=...` is shown, after approval
  where a prompt appears. A prompt before reading `.env` is not a failure, because A(4) keeps the `.env` prompt.
  Record the step(s) where it appears and its Reason.

### MOD-29 (amended; A(9))

- Action 2 (replace): Repeat, and this time press Enter to confirm. Record the Saved screen (scroll if needed). Do
  not press `r`; close it with Enter or Esc.
- Action 3 (replace): Still before any restart: "Run cat .env". If a prompt appears, approve with Enter on option 1.
- New action after action 3: /quit, relaunch with `agms-tui start`, then "Run cat .env" again. If a prompt appears,
  approve with Enter on option 1.
- Expected (replace) "After confirming with a passing preflight, the `.env` read is denied." with: After confirming
  with a passing preflight, the Saved screen does not say that the project's `.env` files are already protected in
  this session. If it mentions them, it says that their protection starts at the restart (A(9)). Action 3's read
  before the restart is recorded and not judged on its own: A(9) accepts that `.env` stays readable until the
  restart. After the relaunch, the `.env` read is denied and no `NOTES_SYNC_TOKEN` value is shown.

### MOD-28 (amended; A(11))

- Starting conditions (replace): Fresh profile. From the tester's own shell, write `plain note for the revoke check`
  into `$AGMS_RUN/fixture/mcp/shared-note.txt`. Launch with `agms-tui start --mcp-notes`, then Moderate (restarted).
  Then give a session approval:
  - S1: "Call the read_shared_note tool on the notes server and show its text." At the MCP approval prompt choose
    "Allow for this session" (↓ to option 2, Enter).
  - If read_shared_note runs with no prompt: /quit, relaunch with `agms-tui start --mcp-notes` (still Moderate), and
    in the new session use "Call the post_note tool on the notes server with text 'one'." as the S1 request instead,
    choosing "Allow for this session" (screens-v3 09). (The note's text is untrusted content, so the session approval
    is given first in a new session.)
  - If no "Allow for this session" option is offered, stop and record the case as blocked, with the screen.
  - S2 (control): repeat the S1 request in the same conversation and record whether it prompts. If it prompts, the
    session approval is not in effect: stop and record the case as blocked, with the screen.
  - Then /security, `g`, record the screen (not judged), and Esc back.
- Action 1 (replace): `/security`, `g`, "Revoke all active authority", Enter. Read and record the review, then Enter
  to confirm.
- Action 2 (replace): Repeat the S1 request in the same conversation; Esc at the prompt.
- New action after action 2: `/security`.
- Expected (add): The revoke review says that revoking does not suspend saved "don't ask again" rules (for example,
  that the commands they cover still run without asking) (A(11)).
- Note: The frozen expected results are unchanged: `g` shows "No grants are held now", the action prompts again, and
  the level is unchanged. "The action" is the S1 request, whose session approval S2 showed was held. G-1 applies.
  A(11)'s "say so" is checked as review wording; whether saved rules keep working after revoke is not tested here.

### MOD-32 (re-run as in force; A(8), G-2)

- Note: After the confirmation in window A, wait about 5 seconds before action 2 in window B, because the product
  says other windows take Moderate "within about a second". Record any message window B shows; it is not judged.
  Because of protected launch (R-3), a window still at Permissive would not offer the outside-sandbox approval
  (screens-v3 12). A prompt in window B whose Reason cites Moderate therefore shows that window B switched (A(8)).
  G-2 now uses the decided D4 reading.

### MOD-17 (re-run as in force; A(6))
- Note: The amendments-v2 changes (action 3, added expected result) stay. #845 is fixed by #850.

### MOD-20 (re-run as in force; A(5))
- Note: Runs after MOD-10 in the same session, as in the first run. The amendments-v2 added expected result stays.

### MOD-24 (re-run as in force; A(5))
- Note: The amendments-v2 changes (starting conditions, relay check between recorded times) stay.

### MOD-30 (re-run as in force; A(4))
- Note: A(4) fixes the reason for Corbanu's files. The existing expected results apply unchanged (compare screens-v3
  10).

### MOD-33 (re-run as in force; A(10))
- Note: #843 is fixed by #854. The existing expected results apply unchanged.

## Dispositions without execution

- **MOD-11:** not re-run; recorded as "pending owner decision (D5, #837)", not failed (D).
- **MOD-06, git-commit part** (action 4's `git add -A && git commit -m 'moderate check'` and the expected result
  "`git log -1` shows "moderate check""): not re-run; recorded as "pending owner decision (D5, #837)", not failed (D).
  The rest of MOD-06 is re-run as amended above, and its verdict covers only that part.
- **First-run results stand (passed, unchanged, not re-run):** MOD-02, MOD-03, MOD-04, MOD-05, MOD-07, MOD-12,
  MOD-13, MOD-14, MOD-16, MOD-18, MOD-19, MOD-23, MOD-25, MOD-27, MOD-31, MOD-34. MOD-02 and MOD-03 were judged
  against the D4 reading, which the owner has kept (E), so no re-judging is needed.
