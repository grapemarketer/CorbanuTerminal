# Amendments to the Moderate design (amendments-v4, second targeted re-run)

Applied on top of `design-cases.md` + `amendments.md` (amendments-v2) + `amendments-v3.md`, for the 2 re-run cases only: MOD-15
(group Q01) and MOD-28 (group Q02). Each case runs in a fresh profile and a fresh copy of the test project, with a
fresh AI tester. Precedence: this amendment over amendments-v3, over amendments-v2, over the frozen cases. For an
amended case, this amendment governs where it differs. Everything it does not mention stays as in force. "screens-v4
NN" is a screen of this candidate (folder `screens-v4/`). "screens-v3 NN" keeps its v3 meaning. Decision labels are
the owner's decisions of 2026-10-19 (D38, D39, D40) and the integrator's choices (F), listed in the integrator's record (not part of the tester's packet).

## Rules for this re-run

- **G-1** (#447 model-decline rule): unchanged. It applies to both cases. The tester may resubmit a request the
  product's model declined once, unchanged, and labels it.
- **G-2** (D4, decided): unchanged. Not used by these cases.
- **G-3** (#766, open): unchanged. Still a named limit. Not used by these cases (no child agents).
- **Named limits:** unchanged (amendments-v2 as amended by amendments-v3).
- **R-1** (A(13)): unchanged. It does not apply: MOD-09 and MOD-21 are not re-run (D39). In both cases the
  product's model is GLM 5.3 Flash, the default (plain `agms-tui start`), and the tester model is GLM 5.3 Flash (F).
- **R-2** (C; amended for MOD-15 by D38): the definition of "a refusal nobody was asked about" stays. So does the
  check that such a cell is not labelled "Approval declined". The sentence "Whether it says "Refused" or shows the
  refusal text is not judged (#900 depends on the build)." is replaced, for MOD-15, with: Whether the cell's label
  says "Refused" is not judged. The refusal text the product shows is judged as MOD-15 below says, because this
  candidate includes #900 (D38).
- **R-3** (B): unchanged test-machine fact. Not used by these cases.
- **Candidate:** the build of screens-v4 (README). The tester records the version shown in the header. This is not
  judged: it reads v0.1.48 in both screens-v3 and screens-v4, so it cannot tell the builds apart.

## Case amendments

### MOD-15 (amended; D38)

- Starting conditions (add): This case runs on the test machine UTM macOS 3 (D38); the integrator sets this up, and
  the tester has nothing to do or check for it. Launch with plain `agms-tui start` (no `--model`). Everything else
  is as in force: approvals off (the tester's own product config, or `-c approval_policy="never"` at launch;
  amendments-v2 answer 11), then Moderate (restarted).
- New action after each of actions 2 and 3: When the turn has ended, record the screen. For that item, copy verbatim
  into the record:
  - the full text the product shows in that command's cell;
  - any ⚠ product warning shown in the conversation for that command;
  - the model's reply.
  If the product cuts a cell short (for example "… +N lines"), show the rest only with a key the product itself shows
  on screen. Otherwise record the cut; the result below is judged on the text the product shows.
- New action after action 4: `/security`, `i`, scroll to Recent denials, record the screen, then Esc.
- Expected (replace) "The message names the security level and what the person can do." with: For each of items 2
  and 3, the product's refusal message names the security level (Moderate). It also says what the person can do to
  proceed, for example turn approvals on so it asks, or run it themselves. Any one concrete step counts; which step
  it names is not judged. The message is the text shown in that command's cell, together with any ⚠ product warning
  shown in the conversation for that command (D38, #900); the level and the step may appear in either. These do not
  count as the message: a warning not tied to that command (for example one shown at startup), the Recent denials
  entries, and the model's reply. The model's reply is recorded and not judged: it neither meets nor fails this
  result.
- Note: These stay unchanged:
  - "Items 2 and 3 are refused with no prompt."
  - "Item 4 runs normally."
  - "The inspector's Recent denials lists items 2 and 3" (A(5)). Only the listing is judged; the wording of the
    entries is recorded, not judged.
  - The amendments-v3 added result: the cells of items 2 and 3 are not labelled "Approval declined" (C, R-2).
- Note: If item 2 or 3 has no cell and is not listed in Recent denials, the model did not attempt it, and G-1
  applies: resubmit once, unchanged, labelled; if it is declined again, the case is blocked. If the item is listed in
  Recent denials but the conversation shows no product refusal message for it, the message result is not met.

### MOD-28 (amended; D40)

- Starting conditions (replace the amendments-v3 text): Fresh profile. From the tester's own shell, write
  `plain note for the revoke check` into `$AGMS_RUN/fixture/mcp/shared-note.txt`. Launch with
  `agms-tui start --mcp-notes`, then Moderate (restarted).
  - Then look for a session approval in exactly the three places P1–P3, in this order, and stop at the first one
    found. Look nowhere else.
  - A session approval is a prompt option offered as lasting for this session (such as "Allow for this session").
    "Allow" (this call only), "Always allow" and "don't ask again" options are not session approvals; do not choose
    them.
  - Whenever this case says to press Esc at a prompt, and the model then retries the same call, press Esc again at
    each further prompt for it. Choose nothing else.
  - P1: "Call the read_shared_note tool on the notes server and show its text."
    - If its prompt offers a session option, choose it (↓ to it, Enter), record the screen, and go to S2.
    - If it prompts without one, record the screen and press Esc.
    - If it runs with no prompt (screens-v4 08), record that.
  - P2: /quit, then relaunch with `agms-tui start --mcp-notes` (still Moderate). The note's text is untrusted
    content, so this request is the first one of the new session: "Call the post_note tool on the notes server with
    text 'one'."
    - If its prompt offers a session option, choose it, record the screen, and go to S2.
    - If it offers only Allow / Cancel (screens-v4 09), record the screen and press Esc.
  - P3 (only if neither P1 nor P2 offered a session option): `/security`, `g`, record the screen, Esc back.
    - "No grants are held now." (screens-v4 05) means no session approval exists to revoke. Go to action 1.
    - If it lists any grant, stop and record the case as blocked, with the screen: the tester gave none, and the
      case cannot tell which action to repeat.
  - S2 (control, only after a session option was chosen in P1 or P2): repeat that request in the same conversation
    and record whether it prompts.
    - If it prompts, the session approval is not in effect: stop and record the case as blocked, with the screen.
    - Otherwise: `/security`, `g`, record the screen (not judged), and Esc back.
  - If the model declines a P1 or P2 request (no "Calling notes.…" or "Called notes.…" cell appears), G-1 applies:
    resubmit once, unchanged, labelled. If it is declined again, the case is blocked.
- Action 1 (as in force, both branches): `/security`, `g`, "Revoke all active authority", Enter. Read and record the
  review, then Enter to confirm.
- New action after action 1 (both branches): Record the screen shown after confirming. If it is not the grants
  screen, use `/security`, `g`, record the screen, and Esc back.
- Action 2 (replace):
  - If a session option was chosen: repeat the request that received it (P1's or P2's) in the same conversation, and
    press Esc at the prompt.
  - If not: do not run this action. Record "no session approval exists to revoke" (D40).
- New action after action 2 (as in force, both branches, including when action 2 was not run): `/security`, and
  record the screen.
- Expected (replace) the frozen "The action prompts again." (and the amendments-v3 note on it) with:
  - If a session option was chosen: the request that received it prompts again. "The action" is that request,
    whose session approval S2 showed was held.
  - If not: not judged. Record "no session approval exists to revoke" (D40).
- Expected (amend) the amendments-v3 added result "The revoke review says that revoking does not suspend saved
  "don't ask again" rules (for example, that the commands they cover still run without asking) (A(11))." The wording
  required is unchanged; where it is judged depends on the branch:
  - If a session option was chosen: judged on the tester's recorded review in action 1.
  - If not: judged on the host capture screens-v4 06, whose review says "It does not suspend saved "don't ask
    again" rules". The tester's own review capture is recorded, and any difference from screens-v4 06 is noted.
    This is not a separate verdict (D40).
- Note: These are judged in both branches, unchanged:
  - "`g` shows "No grants are held now"", on the grants screen recorded after action 1;
  - "The level is unchanged", on the `/security` screen after action 2.
  If no session approval was given, these two results do not show that a held approval ended. They only show that
  revoking left no grant behind and did not change the level. Whether saved rules keep working after revoke is
  still not tested. G-1 applies.

## How each outcome is recorded

**MOD-15** (blocker):

- **passed**: all of the following are met:
  - items 2 and 3 are refused with no prompt;
  - each refused item's product message names the level and a way to proceed;
  - neither cell is labelled "Approval declined";
  - item 4 runs normally;
  - Recent denials lists items 2 and 3.
- **failed**: any of these results is not met. This includes a refused item that is listed in Recent denials but
  shown with no product refusal message in the conversation.
- **blocked**: under G-1 (item 2 or 3 is declined by the model twice, never attempted), or the product cannot be
  driven to the step. Attach the screen.

**MOD-28** (advisory):

- **passed**:
  - a session option was offered and chosen in P1 or P2, and S2 showed it held;
  - after the revoke: `g` shows "No grants are held now", the request prompts again, and the level is still
    Moderate;
  - the tester's recorded revoke review has the A(11) wording.
- **accepted under D40 ("no session approval exists to revoke")**:
  - P1 and P2 offered no session option, and P3 showed "No grants are held now";
  - action 1 ran and was confirmed;
  - after it, `g` shows "No grants are held now" and the level is still Moderate;
  - screens-v4 06 has the A(11) wording.
  Record the P1, P2 and P3 screens. This counts as accepted, not passed.
- **failed**:
  - with a session approval given: a judged result is not met (after the revoke `g` lists a grant, the request runs
    without a prompt, the level changed, or the tester's review lacks the A(11) wording);
  - with no session approval: `g` after the revoke lists a grant, the level changed, or screens-v4 06 lacks the
    A(11) wording.
- **blocked**:
  - S2 prompts (the session approval is not in effect);
  - P3 lists a grant the tester did not give;
  - the model declines a P1 or P2 call twice (G-1);
  - the notes server, the grants screen or the revoke review cannot be reached.
  Attach the screen.
