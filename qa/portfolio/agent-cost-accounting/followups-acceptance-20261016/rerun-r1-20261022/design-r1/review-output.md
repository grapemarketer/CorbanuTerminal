**(a) Problems found and what I changed**

1. **Money comparisons were weakened.** Amendment 1 says money is compared on the exact USD lines, and ACC-18's frozen text requires costs to be "identical". The proposal's G11 accepted two matching *rounded* figures as "same cost". I made G11 strict: a request's exact cost comes from its technical page (`Known subtotal exact USD`). To save time, one calibration per group can establish that a request-page figure without `(rounded)` is exact. If a comparison lacks exact figures on both sides, it is NOT OBSERVED, never a PASS.
2. **ACC-06 had no way to edit the file.** No control in answers.md, environment-r1.md or the navigation note writes to a workspace file. "As in the first run" means nothing to a fresh tester. I moved the edit into set-up, before any reading, so it can't count as a "new request". The tester uses a file control from `agms-tui help` if there is one; otherwise one product turn makes the edit and replaces ACC-01's OK turn, so spend doesn't grow. I added a set-up checkpoint: if `/review` finds no uncommitted changes, ACC-06 is BLOCKED, not FAIL.
3. **ACC-15 "the notice is shown once" was left unjudged.** The proposal marked two notices "ambiguous". I made it judged: exactly one new notice is a PASS; none, or two or more, is a FAIL. "New" is defined by where the line sits relative to the `/side` line, the return confirmation, and a new `r1c-acc15-pre` capture. I also replaced the fixed 10 s wait with `wait 'skip_reopen_warmup' 60`.
4. **Side conversations could create false residuals.** A `/side` conversation may be listed in `/resume` even though its requests are recorded under ALPHA. The proposal's G15 treated every `/resume` entry as a conversation of the profile. I split three things apart: conversations that have recorded requests (these are what get reconciled), empty entries (read and recorded) and residuals.
5. **ACC-14 left out CHARLIE in "all earlier request costs".** I added the one observable form: after the switch, ALPHA's other-conversations figures match CHARLIE's own total and request count.
6. **Unobservable or incomplete checks.**
   - ACC-06 item 3 said "no review request listed there", but other conversations are shown only as totals. I replaced it with "count and cost unchanged".
   - A conversation's request count is now the sum over all its provider lines. The automatic reviewer may use another Z.AI model, which gives a second line.
7. **Missing evidence labels.** I added them to ACC-11 item 7, ACC-14 items 3–4, ACC-18 items 4–5, and the set-up checkpoints.
8. **Time.** Strict exact readings add navigation. To offset that, ACC-15 identifies new requests by count rise, starting from the newest links, instead of re-reading all of them. Group C's estimate is restated as 70–80 min. Stop is the control named in `agms-tui help`, not "as in the first run". R1.1 now *restates and extends* amendment 1 rather than "replacing" it.

Scope check: the proposal was otherwise sound. It touches only the five cases, with their IDs, purposes and situations kept. SET-UP steps are marked. ACC-35, ACC-04 and ACC-22 are not run. All frozen and first-run-amended expected results are kept.

---

**(b) Complete revised amendment R1**

# Amendment R1 — re-run of ACC-06, ACC-11, ACC-14, ACC-15, ACC-18

## R1.0 Precedence and scope

- R1 replaces the full text of ACC-06, ACC-11, ACC-14, ACC-15 and ACC-18 in `design-cases.md` and `amendments.md`. Amendments 1, 2, 9, 12 and 16, as they apply to these cases, are folded into the text below. For these five cases R1 governs. Everything else in the frozen design and the first run's amendments is unchanged, and those other cases are not run.
- Only these five cases are judged. Steps taken from other cases are marked **SET-UP**. They are not judged; their checkpoints only say whether set-up worked. A failed set-up checkpoint makes the dependent case BLOCKED, not FAIL. ACC-35, ACC-04, ACC-22 and all other cases are not run.
- The facts in `environment-r1.md` apply.

## R1.1 Conventions for the re-run

These restate amendment 1 (G1–G6) and add rules G7–G17. None of them loosens amendment 1.

- **G1 Waiting and refresh.** After each turn, wait for the reply with `wait` and a generous timeout. If `/cost` doesn't yet list the expected new requests, choose `Refresh` (or reopen `/cost`) up to 3 times, 5 s apart, before judging.
- **G2 Money.** Compare money with the exact USD lines (`Known subtotal exact USD`, `exact USD`), per G11. The only exception is G12.
- **G3 UTC day.** Note the UTC time at the start and end of the group and at the start of each case. If the group crosses 00:00 UTC, compare earlier requests with `/cost <earlier UTC date>`. Don't fail a case only because earlier requests left today's view.
- **G4 Leaving `/side`.** Press Ctrl+C once (`agms-tui key C-c`) and wait for the on-screen confirmation that you are back in the saved conversation. Esc does not leave `/side`.
- **G5 Names.** Match provider and model names regardless of display formatting.
- **G6 Moonshot.** Moonshot is not used.
- **G7 Evidence and labels.**
  - A result counts as observed only if a captured screen (`agms-tui screen LABEL`) shows it, or the report copies the exact line from a captured screen.
  - Labels start with `r1a-` (group A) or `r1c-` (group C) and use the names given in the steps. `<k>` is the link number at the time of reading.
  - After every capture, check that it contains the line it was taken for. If it doesn't, move the selection (Up/Down) until the line is inside the window, then capture again under the same label with `-b`, `-c` and so on.
- **G8 Reading a `/cost` first screen.** Take three captures with base label `<base>`:
  - **`<base>-first`** must show:
    - this conversation's provider line(s), each with its request count and cost, or "no recorded requests";
    - the `Reopen warm-up:` line, if any;
    - the other-conversations lines: "Other conversations on this day, not included above: N requests in M conversations.", their provider lines and "Other conversations' cost: …"; or "No other conversation recorded requests on this day."
  - **`<base>-details`** must show `Known subtotal exact USD:`. That value is the conversation's **own total**.
  - **`<base>-links`** must show every `→ Request …` link and the `→ Descendant attempts (…)` line. Use `-links-2` if they don't fit in one window.
  - The conversation's **request count** is the sum of the request counts on its own provider lines.
- **G9 Reading a request.**
  - Open its link and capture `<base>-req<k>`. The capture must show the cost line, the `Kind:` line (if any) and `Request: <ID>`.
  - Where a step asks for the technical page, open `→ Technical details (attempt n)` and capture `<base>-req<k>-tech` showing `Request:` and `Turn:`.
  - Where an exact cost is needed (G11), also capture `<base>-req<k>-tech-money`, plus `-2` if needed. These must show the token lines, the component cost lines, the rate lines and `Known subtotal exact USD`.
  - If a request has more than one attempt, read every attempt.
  - If the request count (G8) is greater than the number of `→ Request` links, open `→ Descendant attempts` and read every request listed there the same way (`<base>-desc-req<k>`). Those requests are under this conversation.
- **G10 Identity.** A request is identified only by its Request ID; link numbers may change. For each conversation, report a table: ID, cost line as shown, exact cost (if read), Kind, `Turn:` prefix (if read), label(s).
- **G11 Exact cost and same cost.**
  - A request's **exact cost** is its technical page's `Known subtotal exact USD`, summed over its attempts.
  - **Calibration.** The first time in a group that a request-page cost figure with no `(rounded)` mark and no other qualifier would be used as an exact cost, also capture that request's technical-page money lines (`<label>-calib`).
    - If the two are equal, such unmarked figures count as exact costs for the rest of the group.
    - If they are not, record both, and read every exact cost from the technical page for the rest of the group.
  - Two readings of one Request ID have the **same cost** only if their exact costs are equal.
  - A comparison for which either reading lacks an exact cost is NOT OBSERVED. Read the missing exact cost (it costs nothing) before reporting.
- **G12 Rounded figures.**
  - A figure marked `(rounded)` matches an exact amount E if |figure − E| ≤ 0.0000005. An unmarked figure matches only if it is equal; ignore trailing zeros.
  - Sums are computed from exact costs. A rounded figure is used only where the screen shows no exact one (the other-conversations cost), and the tolerance then applies once per such figure.
  - Show all arithmetic.
- **G13 Starting and stopping.**
  - Start a **new conversation** only where a step says so. The step's prompt is then the next product action, before any `/cost`, stop or other command.
  - Every other start uses `resume --last`. Never stop the product while a new conversation has no turn.
  - Stop the product with the stop control listed in `agms-tui help`.
- **G14 Recovery.**
  - If a start produced a new conversation that the step did not ask for, capture it (`<group>-deviation-<n>`). Type `Reply with the single word STRAY.` there, wait for the reply, record the deviation and continue the step. ACC-18 reads that conversation like any other.
  - After a crash, restart with `resume --last` and check that the transcript is the expected conversation; otherwise switch to it with `/resume`.
- **G15 Conversations, empty entries and residuals.**
  - A **conversation with recorded requests** is one that a first screen counts: the open conversation if its request count is ≥ 1, and the M conversations in "N requests in M conversations".
  - An **empty entry** is a `/resume` entry whose own `/cost` shows no recorded requests (for example, a side conversation whose work is recorded under its saved conversation). It is read, recorded and reported as an observation. It is neither a conversation with recorded requests nor a residual.
  - A **residual** is a conversation with recorded requests that can't be opened: it isn't in `/resume`, selecting it fails, or selecting it opens a different conversation.
  - No R1 step leaves a new conversation without a turn, so report every residual as a deviation, with the step that created it if known.
- **G16 Outcomes.**
  - Each expected result is PASS, FAIL, BLOCKED or NOT OBSERVED.
    - BLOCKED: a required control or precondition is missing (navigation note), or a set-up checkpoint failed.
    - NOT OBSERVED: no captured screen shows the needed line. Retake the reading before reporting it.
  - NOT OBSERVED is never a PASS. A case passes only if every expected result is PASS.
- **G17 Time.** The hard limit is 90 minutes. At 80 minutes, finish the reading in progress and report the remaining results as NOT OBSERVED (time).

## R1.2 Re-run group plan

Each group runs on its own fresh machine, with a fresh profile and a fresh copy of `ledger-notes`.

| Group | Route / model | Order (SET-UP = not judged) | Real turns | Time |
|---|---|---|---|---|
| **A-R1** | `zai` / `glm-5.3-flash` | A-S1 SET-UP (file edit + one turn, in place of ACC-01's turn) → A-S2 SET-UP (ACC-03 restart) → **ACC-06** | 1 turn + the review | ≈ 40–55 min |
| **C-R1** | `openai` / `gpt-6-luna` | C-S1 SET-UP (ACC-10 ALPHA) → **ACC-11** → **ACC-14** (includes the CHARLIE turn) → **ACC-15** → C-S2 SET-UP (ACC-36 config step) → **ACC-18** | 3 turns (ALPHA, CHARLIE, SIDE); 2 new-conversation warm-ups; 3 reopen warm-ups | ≈ 70–80 min |

Expected spend: group C about $0.01–0.02 at `/cost`'s estimate; group A a few cents. ACC-18 sends nothing.

## R1.3 Group A-R1

**A-S1 — SET-UP (not judged; replaces ACC-01's turn as the conversation's earlier history)**
1. Run `agms-tui help`. If it lists a control that writes to a file in the test project, use it now, before starting the product: append one line, `// review me`, to an existing tracked text file at the project's top level. Copy that help line and the file name into the report.
2. Start a new conversation with `--route zai --model glm-5.3-flash` and wait for the composer.
3. Type the turn (G13: this is the next product action) and press Enter:
   - **If step 1 edited the file:** type `Reply with the single word OK.` and wait for `OK`.
   - **If no such control exists:** type ``Append one new last line containing exactly `// review me` to an existing tracked text file in this workspace, then reply with only that file's name.`` If the product asks for approval, choose the displayed option that allows this one action once, and record the choice. Wait for the reply.
4. Capture `r1a-s1-turn`.

Checkpoint: the reply is shown, and in the second form it names a file. If not, resubmit the same wording once, unchanged. If that also fails, ACC-06 is BLOCKED.

**A-S2 — SET-UP from ACC-03 (not judged; uses the first two sentences of amendment 3's starting condition; ACC-03's actions are not run)**
1. Stop the product.
2. Start with `resume --last -c 'approvals_reviewer="auto_review"' -c 'approval_policy="on-request"' -c 'sandbox_mode="read-only"'`.
3. Capture `r1a-s2-resumed`.

Checkpoint: the transcript shows the A-S1 turn. If it doesn't, pick that conversation with `/resume`.

**ACC-06 (blocker) — `/review` is labelled `code-review:`**
- **Purpose:** work done by `/review` is recorded once, under the conversation that ran it, labelled `code-review:`.
- **Start (after A-S2, same conversation):**
  1. Run `/cost` (G1) and read it per G8 with base `r1a-acc06-before`. Record R0 (the request count), T0 (the own total), and the other-conversations line: count O0 and cost, or "No other conversation…".
  2. Read every request page per G9 (`r1a-acc06-before-req<k>`), with exact costs per G11. Record each ID and exact cost. Press Esc until the composer shows.
- **Actions:**
  1. Type `/review` and press Enter. Capture `r1a-acc06-review-menu`.
  2. Choose the option to review uncommitted changes. Without typing, wait for the review to finish (up to 10 min); any approvals go to the automatic reviewer. Capture `r1a-acc06-review-done`.
     - Set-up checkpoint: if the product says there are no uncommitted changes to review, ACC-06 is BLOCKED.
  3. Run `/cost` (G1) and read it per G8 with base `r1a-acc06-after`.
  4. Read every request page per G9 (`r1a-acc06-after-req<k>`), with exact costs per G11.
     - A **new request** is one whose ID is not in the start list.
     - For each new request, also capture `r1a-acc06-after-req<k>-tech` showing `Request:` and `Turn:`.
- **Expected (frozen):**
  1. **The new request(s) have a `Turn:` line starting with `code-review:`.**
     - At least one new request is listed, and every new request's tech capture shows `Turn: code-review:…`.
     - No new request after G1 is a FAIL.
     - Evidence: `r1a-acc06-after-links`, `r1a-acc06-after-req<k>-tech`.
  2. **They are listed once, under this conversation.**
     - Each new ID appears on exactly one request page of this conversation.
     - The request count on `r1a-acc06-after-first` (G8: the sum over all its provider lines) equals R0 plus the number of distinct new IDs.
     - Evidence: `r1a-acc06-after-first`, `r1a-acc06-after-req<k>`.
  3. **The other-conversations count is unchanged.**
     - The other-conversations request count, conversation count and cost on `r1a-acc06-after-first` are identical to those on `r1a-acc06-before-first` (or both read "No other conversation…").
     - Evidence: `r1a-acc06-before-first`, `r1a-acc06-after-first`.
  4. **Earlier costs are unchanged.**
     - Every start ID is found again with the same cost (G11).
     - Evidence: `r1a-acc06-before-req<k>(-tech-money)`, `r1a-acc06-after-req<k>(-tech-money)`.
- **Observation only:** the number of review requests, any automatic reviews shown, the file edited and how it was edited.

## R1.4 Group C-R1

**C-S1 — SET-UP from ACC-10 (not judged; uses amendment 8's actions 2–3)**
1. Fresh profile. Start a new conversation with `--route openai --model gpt-6-luna` and no `-c` (skip off). Wait for the composer.
2. Immediately (G13) type `Reply with the single word ALPHA.`, press Enter and wait for `ALPHA`. Capture `r1c-s1-alpha`.

Checkpoint: the reply is shown. If a ⚠ warm-up notice appears, record it and continue. This conversation is **ALPHA**.

**ACC-11 (blocker) — a restart sends one labelled warm-up and shows a notice**
- **Purpose:** restarting on a saved OpenAI conversation sends one labelled, announced warm-up, counted once, without re-pricing history.
- **Start (ALPHA, after C-S1):**
  1. Run `/cost` (G1) and read it per G8 with base `r1c-acc11-before`. Record T (the own total) and n (the request count).
  2. Read every request page per G9 (`r1c-acc11-before-req<k>`), with exact costs per G11. These are C1…Cn; record them with their IDs.
     - Set-up checkpoint: no `Reopen warm-up` line, and no link containing it.
  3. Press Esc until the composer shows, then stop the product.
- **Actions:**
  1. Start with `resume --last` and no `-c` (skip off). Without typing, run `agms-tui wait 'skip_reopen_warmup' 60`. Capture `r1c-acc11-notice`, showing the notice and the ALPHA transcript, and note the seconds since start.
  2. Run `/cost` (G1) and read it per G8 with base `r1c-acc11-after`.
  3. Open the request whose link contains `Reopen warm-up` and capture:
     - its request page as `r1c-acc11-warmup-req`;
     - its technical page as `r1c-acc11-warmup-tech`;
     - its money lines as `r1c-acc11-warmup-tech-money` (`-2` if needed).
  4. Read every other request page per G9 (`r1c-acc11-after-req<k>`), with exact costs per G11.
- **Expected (amendment 9 unchanged; item 7 is the unjudged "appears once" result):**
  1. **Notice.**
     - Within 60 s of start, without typing, a ⚠ notice appears that names `skip_reopen_warmup` and says the warm-up is billed.
     - A reopen without a warm-up, or a warm-up without a notice, is a FAIL.
     - Evidence: `r1c-acc11-notice`, `r1c-acc11-after-first`.
  2. **Warm-up line.** The `/cost` first screen has a `Reopen warm-up:` line counting 1 request, with a cost.
     - Evidence: `r1c-acc11-after-first`.
  3. **Warm-up link.** Exactly one request link contains `Reopen warm-up`, and its request page contains `Reopen warm-up`.
     - Evidence: `r1c-acc11-after-links`, `r1c-acc11-warmup-req`.
  4. **Warm-up arithmetic.**
     - On its technical page, each component cost = tokens × rate / 1,000,000, and the known subtotal equals its cost. Show the arithmetic.
     - Observation only: the input token count.
     - Evidence: `r1c-acc11-warmup-tech-money`.
  5. **New total.** The new total = T + the warm-up cost, counted once: the own total on `r1c-acc11-after-details` equals the own total on `r1c-acc11-before-details` plus the warm-up's `Known subtotal exact USD`, exactly.
  6. **History unchanged.** C1…Cn and their Request IDs are unchanged: every ID from `r1c-acc11-before-req<k>` is found again with the same cost (G11).
     - Evidence: `r1c-acc11-before-req<k>(-tech-money)`, `r1c-acc11-after-req<k>(-tech-money)`.
  7. **Each Request ID appears once.**
     - The IDs on all request pages read in actions 3–4 are all different, and their number equals the request count on `r1c-acc11-after-first`.
     - The warm-up's ID is not one of the start IDs.
     - Evidence: `r1c-acc11-after-first`, `r1c-acc11-after-req<k>`, `r1c-acc11-warmup-req`.
- **Observation only:** the warm-up's `Turn:` prefix; the calibration result (G11), if taken here.

**ACC-14 (blocker) — `/resume` to a different saved conversation sends a warm-up**
- **Purpose:** switching to a different saved conversation with `/resume` sends a labelled, announced warm-up, without re-pricing or duplicating history.
- **Start:**
  1. Press Esc until the composer shows, then stop the product.
  2. Start a **new conversation** with `--route openai --model gpt-6-luna` and no `-c` (skip off). Wait for the composer.
  3. Immediately (G13) type `Reply with the single word CHARLIE.`, press Enter and wait for `CHARLIE`. Capture `r1c-acc14-charlie-reply`. This conversation is **CHARLIE**.
     - Set-up checkpoint: the reply is shown. Do not continue until it is.
  4. Run `/cost` (G1) and read it per G8 with base `r1c-acc14-charlie`. Read every request page per G9 (`r1c-acc14-charlie-req<k>`), with exact costs per G11. Record the IDs, exact costs, own total and request count.
     - Set-up checkpoint: at least 1 request, and no `Reopen warm-up` line. If nothing is listed after G1, record it and continue.
  5. ALPHA's "before" record is ACC-11's last reading (`r1c-acc11-after-*`, `r1c-acc11-warmup-*`): its IDs, exact costs, own total and warm-up count 1. Nothing has happened in ALPHA since.
- **Actions:**
  1. Press Esc until the composer shows. Type `/resume` and press Enter; capture `r1c-acc14-picker`.
  2. Select the conversation whose first message is `Reply with the single word ALPHA.` and press Enter.
  3. Without typing, run `agms-tui wait 'skip_reopen_warmup' 60`. Capture `r1c-acc14-notice`, showing the ALPHA transcript.
  4. Run `/cost` (G1) and read it per G8 with base `r1c-acc14-after`.
  5. Read every request page per G9 (`r1c-acc14-after-req<k>`), including the new warm-up's, with exact costs per G11.
- **Expected (amendment 12 unchanged; items 3–4 spell out the unjudged results):**
  1. **Notice.** A notice appears.
     - Evidence: `r1c-acc14-notice`.
  2. **Warm-up count.** The `Reopen warm-up` line counts 2.
     - Evidence: `r1c-acc14-after-first`.
  3. **History unchanged.** Earlier request costs and IDs are unchanged:
     - every ID on `r1c-acc11-after-req<k>` and `r1c-acc11-warmup-req` is found again in ALPHA with the same cost (G11);
     - on `r1c-acc14-after-first`, the other-conversations request count equals CHARLIE's request count, and the other-conversations cost matches CHARLIE's own total (G12).
     - Evidence: `r1c-acc14-after-req<k>(-tech-money)`, `r1c-acc14-after-first`, `r1c-acc14-charlie-details`.
  4. **Each ID appears once.**
     - ALPHA's request-page IDs are all different, and their number equals the request count on `r1c-acc14-after-first`.
     - No CHARLIE ID (`r1c-acc14-charlie-req<k>`) appears among ALPHA's.
     - Evidence: `r1c-acc14-after-first`, `r1c-acc14-after-req<k>`, `r1c-acc14-charlie-req<k>`.
- **Observation only:** the other-conversations lines on `r1c-acc14-after-first`. They are ACC-15's baseline.

**ACC-15 (blocker) — `/side` on an OpenAI conversation sends a labelled warm-up**
- **Purpose:** opening `/side` on an OpenAI conversation sends one labelled, announced warm-up. The side turn's work is labelled `side:` and recorded under the saved conversation.
- **Start:** ALPHA is open after ACC-14, with `/cost` closed and the composer showing. Skip is still off. The baseline is ACC-14's reading:
  - warm-up count W = 2;
  - ALPHA's request count and IDs;
  - the other-conversations lines on `r1c-acc14-after-first`.
- **Actions:**
  1. Capture `r1c-acc15-pre`.
  2. Type `/side` and press Enter. Run `agms-tui wait 'skip_reopen_warmup' 60`, then capture `r1c-acc15-side-open`, whether or not the wait found the text.
  3. Type `Reply with the single word SIDE.`, press Enter and wait for `SIDE`. Capture `r1c-acc15-side-reply`.
  4. Press Ctrl+C once (`agms-tui key C-c`) and wait for the product's confirmation that you are back in the saved conversation. Wait 15 s more and capture `r1c-acc15-back`.
  5. Run `/cost` (G1) and read it per G8 with base `r1c-acc15-after`.
  6. Find the new requests:
     - Let m = (request count on `r1c-acc15-after-first`) − (ACC-14's request count).
     - Read request pages per G9 (`r1c-acc15-after-req<k>`), starting from the highest-numbered link, then any `→ Descendant attempts` entries, and working down, until m IDs not in ACC-14's list have been found. If m new IDs aren't found among all links, read every request page.
     - For the new request whose page contains `Reopen warm-up`: capture it again as `r1c-acc15-warmup-req`, and capture its technical page as `r1c-acc15-warmup-tech` with `-tech-money`.
     - For every other new request (the side turn's): capture `r1c-acc15-side-req<k>-tech` with `-tech-money`.
     - Before each technical-page capture, make sure the `Turn:` line is inside the window (G7).
- **Expected (frozen; amendment 2 governs leaving `/side`):**
  1. **The notice is shown once.**
     - A ⚠ warm-up notice line is **new** if, on its capture, it is either:
       - below the line where `/side` was typed;
       - on a side-view capture that shows none of the transcript lines visible on `r1c-acc15-pre`;
       - below the product's return confirmation.
     - The same line seen again on a later capture of the same view is not counted again.
     - Exactly one new notice across `r1c-acc15-side-open`, `-side-reply` and `-back` is a PASS. None, or two or more, is a FAIL.
     - If a notice line on these captures can't be placed by this rule, record it with its neighbouring lines and mark this result NOT OBSERVED.
  2. **The warm-up count goes up by 1.** The `Reopen warm-up:` line on `r1c-acc15-after-first` counts W + 1 = 3.
  3. **That warm-up's request page says Reopen warm-up.** Exactly one new request's page contains `Reopen warm-up`.
     - Evidence: `r1c-acc15-warmup-req`.
  4. **The side turn's requests have `side:` labels and sit under this conversation.**
     - At least one new request other than the warm-up is listed in this conversation's `/cost` (links or descendant attempts).
     - Each of them shows `Turn: side:…` on its capture.
     - None listed, while `SIDE` was shown, is a FAIL.
     - Evidence: `r1c-acc15-after-links`, `r1c-acc15-side-req<k>-tech`.
  5. **Nothing is listed under other conversations because of this step.** The other-conversations request count, conversation count and cost on `r1c-acc15-after-first` are identical to those on `r1c-acc14-after-first`.
- **Observation only:** the warm-up's `Turn:` prefix (`r1c-acc15-warmup-tech`).

**C-S2 — SET-UP from ACC-36 (not judged; uses amendment 15's starting condition; ACC-36's actions are not run)**
1. Press Esc until the composer shows, then stop the product.
2. Run `agms-tui config 'skip_reopen_warmup = true'`. Do not start the product.

ALPHA's warm-up count (3) and own total are those recorded in ACC-15.

**ACC-18 (blocker) — history is not re-priced, and nothing is counted twice, across the group**
- **Purpose:** after all the group's reopens, every earlier request keeps its cost and ID, and every conversation's figures agree with each other.
- **Start:** after C-S2. Skip is on through `config.toml` and the product is stopped. The records from ACC-11, ACC-14 and ACC-15 are at hand.
- **Actions:**
  1. Start with `resume --last` and no `-c`. Without typing, wait 60 s and capture `r1c-acc18-open-1`. Note which conversation opened; ALPHA is expected.
  2. Read the open conversation, with `<c>` = `alpha`, `charlie`, or `conv<n>` for any other:
     - run `/cost` (G1) and read it per G8 with base `r1c-acc18-<c>`;
     - read every request page per G9 (`r1c-acc18-<c>-req<k>`), with exact costs per G11;
     - press Esc until the composer shows.
  3. Type `/resume` and press Enter; capture `r1c-acc18-picker-<n>`. Select an entry not yet read (by its first message) and press Enter. Without typing, wait 60 s and capture `r1c-acc18-open-<n>`. Repeat action 2. An entry with no recorded requests of its own is an empty entry (G15): capture its first screen only.
  4. Repeat action 3 until every `/resume` entry has been read. ALPHA and CHARLIE are expected, plus any STRAY and empty entries.
  5. If a conversation with recorded requests can't be opened, capture the evidence as `r1c-acc18-residual-<n>` (G15).
- **Expected (amendment 16 restated; item 3 now covers every conversation; item 5 adds "once within a conversation"):**
  1. **No notice, and no warm-up count rises.**
     - No `r1c-acc18-open-<n>` capture shows a ⚠ warm-up notice.
     - ALPHA's `Reopen warm-up` line counts 3, as on `r1c-acc15-after-first`.
     - CHARLIE has no `Reopen warm-up` line, as on `r1c-acc14-charlie-first`.
     - Evidence: `r1c-acc18-open-<n>`, `r1c-acc18-alpha-first`, `r1c-acc18-charlie-first`.
  2. **Every cost and Request ID noted earlier is identical.**
     - Every ID recorded in ACC-11, ACC-14 (ALPHA and CHARLIE) and ACC-15 is found again in the same conversation with the same cost (G11).
     - ALPHA's own total equals the one on `r1c-acc15-after-details`, and CHARLIE's equals the one on `r1c-acc14-charlie-details`, exactly.
     - Evidence: `r1c-acc18-<c>-req<k>(-tech-money)`, `r1c-acc18-<c>-details`.
  3. **Each conversation's request costs sum to its total, for every conversation with recorded requests.** The sum of the exact costs of all its requests equals its own total on `r1c-acc18-<c>-details`, exactly. Show the arithmetic.
  4. **A conversation's total as seen from the others equals its own total.**
     - On each read conversation's `-first`: own total + other-conversations cost = the sum of the own totals of all conversations with recorded requests (G12).
     - The other-conversations request count equals the sum of the other conversations' request counts.
     - The other-conversations conversation count equals the number of other conversations with recorded requests.
     - With exactly two such conversations: CHARLIE's other-conversations cost matches ALPHA's own total and ALPHA's matches CHARLIE's (G12); the request counts likewise.
     - Evidence: `r1c-acc18-<c>-first`, `r1c-acc18-<c>-details`.
  5. **No Request ID appears twice.**
     - Within each conversation, the IDs are all different and their number equals its request count.
     - No ID appears in two conversations.
     - Evidence: `r1c-acc18-<c>-first`, `r1c-acc18-<c>-req<k>`.
- **Residual (a conversation that can't be opened):** it is recorded, not a FAIL (amendment 16).
  - Results 3 and 5 are judged over the conversations that were read.
  - Result 4 is judged in this form: own total + other-conversations cost is the same on every read conversation's `-first` (G12), and the conversation counts include the residual.
  - Record the residual's derived total (that common value minus the read own totals) and its derived request count, and report it as a deviation (G15).
  - Empty entries are reported as observations and don't enter the sums.

## R1.5 Limits of this re-run

- ACC-18 covers only the conversations this re-run creates: ALPHA (with its `/side` work) and CHARLIE.
  - Not covered: BRAVO (ACC-13), the edit branch (ACC-16), and reviewer or sub-agent requests (ACC-17).
  - Those cases' first-run results stand and are not re-judged.
- Comparing totals against the other-conversations cost depends on a rounded figure (G12), unless the build shows an exact one. If it does, use the exact figure.
- In ACC-14, CHARLIE's unchanged costs are checked only through the other-conversations totals. ACC-18 checks them per request.

---

**(c) Open questions (for the integrator; none blocks freezing)**

1. **File edit for ACC-06.** Does `agms-tui help` list a control that writes to a workspace file? If not, A-S1 falls back to a product turn. Is `ledger-notes` a git repository with tracked text files at its top level?
2. **ACC-15 notice counting.** The rule counts a notice shown in the side view and again in the saved view as two, which is a FAIL. Is that what the intent means by "shown once"?
3. **Descendant attempts.** G9 counts requests reached only through `→ Descendant attempts` as "under this conversation". Please confirm that this matches the intent for `/review` and `/side`.
4. **Calibration.** G11 treats request-page figures without `(rounded)` as exact once one technical page agrees. Is that acceptable, or must every exact cost come from a technical page? The second option adds about 10 minutes to group C.
5. **Group A length.** Group A comes out at about 40–55 minutes. That is shorter than the 60–80 band, but within the limit, and nothing is padded.

VERDICT: CHANGES