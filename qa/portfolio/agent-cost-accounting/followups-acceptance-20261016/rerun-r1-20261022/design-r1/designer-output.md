**What I changed and why**

1. **Scope.** Replaced the full text of ACC-06, ACC-11, ACC-14, ACC-15 and ACC-18 only. Where the first-run amendments changed these cases (amendments 1, 2, 9, 12 and 16), their expected results are carried over unchanged. Every other case keeps its text, and its first-run result stands.
2. **Evidence rule.** Every judged result now names the screen label(s) that must show it. A result no captured screen shows is NOT OBSERVED, which never counts as a PASS. The first run lost ACC-15's `side:` line exactly this way.
3. **Request IDs.** Requests are matched only by Request ID, never by link number. Request pages are read before each reopen and the same requests are re-read after it. The checks for ACC-11, ACC-14 and ACC-18 now include "each Request ID appears once". These were the unjudged results.
4. **Money.** Amendment 1's exact-USD rule is kept. I added a defined meaning of "same cost" and one rounding tolerance, used only for the other-conversations cost. On the reference screens that figure appears only rounded, and it is needed to judge "seen from another conversation".
5. **ACC-18 across conversations.**
   - Every conversation is now read in full: request IDs, a per-conversation sum, and the check from the other conversations.
   - The plan leaves exactly two conversations (ALPHA and CHARLIE), so "seen from the other" becomes a direct comparison.
   - Amendment 16's general form is kept and restated so it can still be judged when a residual exists.
6. **Residuals.**
   - The product is started without `resume --last` only for the two "new conversation" starts, and each is followed at once by its turn.
   - A recovery step (a STRAY turn) covers an accidental new conversation.
   - A conversation that can't be opened is still a residual, not a FAIL (amendment 16). What is recorded about it, and what is still judged, is now spelled out.
7. **Set-up steps.**
   - Group A repeats ACC-01's turn and ACC-03's restart with the automatic-reviewer settings, so ACC-06 runs in the state it failed in.
   - Group C repeats ACC-10's ALPHA turn and ACC-36's config step; ACC-14's CHARLIE turn is part of ACC-14.
   - Not repeated: ACC-02–05, 07, 12, 13, 16 and 17. None of the five depends on them. Without ACC-12, ACC-14's count of 2 still holds, because ACC-12 added no warm-up. This saves several real turns.
8. **ACC-15.** Exact captures of the `Turn:` line are now required. "Shown once" is now a count across the captures; the side-view versus saved-view ambiguity is kept for the integrator to decide.
9. **ACC-06.** Before and after readings were added. "Listed once" is now observable: distinct new IDs, and a count rise equal to their number.
10. **Group plan.** Two fresh machines with times and spend estimates.

---

# Amendment R1 — re-run of ACC-06, ACC-11, ACC-14, ACC-15, ACC-18

## R1.0 Precedence and scope

- **What R1 replaces.** R1 replaces the full text of ACC-06, ACC-11, ACC-14, ACC-15 and ACC-18 in `design-cases.md` and `amendments.md`. Amendments 1, 2, 9, 12 and 16, as they apply to these cases, are folded into the text below. For these five cases R1 governs. Everything else in the frozen design and the first run's amendments is unchanged, and those cases are not run.
- **What is judged.** Only these five cases are judged.
  - Steps borrowed from other cases are marked **SET-UP**. They are not judged; their checkpoints only say whether set-up worked.
  - ACC-35, ACC-04, ACC-22 and all other cases are not run.
- **Environment.** The facts in `environment-r1.md` apply.

## R1.1 Conventions for the re-run

These replace amendment 1 for this re-run, restated in full; G1–G5 keep amendment 1's rules.

- **G1 Waiting and refresh.** After each turn, wait for the reply using `wait` with a generous timeout. If `/cost` doesn't yet list the expected new requests, choose `Refresh` (or reopen `/cost`) up to 3 times, 5 s apart, before judging.
- **G2 Money.** Compare money using the exact USD lines (`Known subtotal exact USD`, `exact USD`), not rounded displays. The only exception is G12.
- **G3 UTC day.** Note the UTC time at the start and end of the group and at the start of each case. If the group crosses 00:00 UTC, compare earlier requests with `/cost <earlier UTC date>`. Don't fail a case only because earlier requests left today's view.
- **G4 Leaving `/side`.** Press Ctrl+C once (`agms-tui key C-c`) and wait for the on-screen confirmation that you are back in the saved conversation. Esc does not leave `/side`.
- **G5 Names.** Match provider and model names regardless of display formatting.
- **G6 Moonshot.** Moonshot is not used in this re-run.
- **G7 Evidence and labels.**
  - A result counts as observed only if a captured screen (`agms-tui screen LABEL`) shows it, or the report copies the exact line from a captured screen.
  - Labels are lowercase letters, digits and hyphens. They start with `r1a-` (group A) or `r1c-` (group C) and use the names given in each step. `<k>` is the link number at the time of reading.
  - If a capture misses the line it was taken for, move the selection (Up/Down) until the line is inside the window, then capture again under the same label with `-b`, `-c` and so on.
  - After every capture, check that the captured text contains the line it was taken for.
- **G8 Reading a `/cost` first screen.** Take three captures with base label `<base>`:
  - **`<base>-first`:** this conversation's provider line(s) with request count and cost; the `Reopen warm-up:` line, if any; and the other-conversations lines. Those are the requests/conversations count, the per-provider lines and the other-conversations cost, or "No other conversation recorded requests on this day".
  - **`<base>-details`:** the `Known subtotal exact USD:` line. Below, this value is the conversation's **own total**.
  - **`<base>-links`:** every `→ Request …` link and the `→ Descendant attempts (…)` line. Use `-links-2` if they don't fit in one window.
- **G9 Reading a request.**
  - Open its link and capture the request page as `<base>-req<k>`. The capture must show the cost line, the `Kind:` line (if any) and `Request: <ID>`.
  - When a step asks for the technical page, open `→ Technical details (attempt n)` and capture `<base>-req<k>-tech` showing `Request:` and `Turn:`.
  - When money from that page is needed, also capture `-tech-money` (and `-tech-money-2` if needed). These must show the token lines, the component cost lines, `Known subtotal exact USD` and the rate lines.
  - If a request has more than one attempt, read every attempt. The request's exact cost is the sum of its attempts' known subtotals.
  - If the first screen's request count for the conversation is greater than the number of `→ Request` links, open `→ Descendant attempts` and read every request it lists in the same way (`<base>-desc-req<k>`). Those requests are under this conversation.
- **G10 Identity.** A request is identified only by its Request ID; link numbers may change between readings. For each conversation, report a table with:
  - the ID;
  - the cost line as shown;
  - the exact cost, if read;
  - the Kind;
  - the `Turn:` prefix, if read;
  - the label(s).
- **G11 Same cost.** Two readings of one Request ID have the same cost if the request-page cost lines show the same figure and the same marks. Where both readings include an exact figure, the exact figures must be equal.
- **G12 Rounded figures.**
  - A figure marked `(rounded)` is an exact amount rounded to 6 decimal places. It matches an exact amount E if |figure − E| ≤ 0.0000005.
  - A figure without `(rounded)` matches only if it is equal; ignore trailing zeros.
  - Sums and totals are computed from exact figures. A rounded figure is used only where the screen shows no exact one (the other-conversations cost), and the tolerance then applies once per such figure.
  - Show all arithmetic.
- **G13 Starting the product.**
  - Start a **new conversation** only where a step says so. The step's prompt is then the very next action, before any `/cost`, stop or other command.
  - Every other start uses `resume --last`. Never stop the product while a new conversation has no turn.
  - Stop the product with the harness's stop control, as in the first run.
- **G14 Recovery.**
  - If a start produced a new conversation where the step did not ask for one, capture it (`<group>-deviation-<n>`). Then type `Reply with the single word STRAY.` there, wait for the reply, record the deviation and continue the step. ACC-18 reads that conversation like any other.
  - After a crash, restart with `resume --last` and check the transcript is the expected conversation; otherwise switch to it with `/resume`.
- **G15 Conversations and residuals.**
  - The conversations of the profile are the open one plus every entry in `/resume`. Their number must match the counts on first screens: this one + Y other conversations.
  - A **residual** is a conversation that is listed or counted but can't be opened. That covers: it isn't in `/resume`, selecting it fails, or selecting it opens a different conversation. ACC-18 says how a residual is treated.
  - No R1 step leaves a conversation without a turn, so any residual is also reported as a deviation for the integrator, with the step that created it if known.
- **G16 Outcomes.**
  - Each expected result is PASS, FAIL, BLOCKED (a required control or precondition is missing, per the navigation note) or NOT OBSERVED (no captured screen shows the needed line).
  - NOT OBSERVED is never a PASS. Retake the reading (it costs nothing) before reporting it.
  - A case passes only if every expected result is PASS.
- **G17 Time.** The hard limit is 90 minutes. At 80 minutes, finish the reading in progress and report the remaining results as NOT OBSERVED (time).

## R1.2 Re-run group plan

Each group runs on its own fresh machine, with a fresh profile and a fresh copy of `ledger-notes`.

| Group | Route / model | Order (SET-UP = not judged) | Real turns | Time |
|---|---|---|---|---|
| **A-R1** | `zai` / `glm-5.3-flash` | A-S1 SET-UP (ACC-01 turn) → A-S2 SET-UP (ACC-03 restart) → **ACC-06** | 1 turn + the review | ≈ 35–45 min (one case; not padded) |
| **C-R1** | `openai` / `gpt-6-luna` | C-S1 SET-UP (ACC-10 ALPHA) → **ACC-11** → **ACC-14** (includes the CHARLIE turn) → **ACC-15** → C-S2 SET-UP (ACC-36 config step) → **ACC-18** | 3 turns (ALPHA, CHARLIE, SIDE), 2 new-conversation warm-ups, 3 reopen warm-ups | ≈ 70–80 min |

Expected spend: group C about $0.01–0.02 at `/cost`'s estimate; group A a few cents. ACC-18 sends nothing.

## R1.3 Group A-R1

**A-S1 — SET-UP from ACC-01 (not judged; uses the frozen ACC-01 action 3 text)**
1. Fresh profile. Start a new conversation with `--route zai --model glm-5.3-flash` and wait for the composer.
2. Type `Reply with the single word OK.`, press Enter and wait for `OK`. Capture `r1a-s1-ok`.

Checkpoint: the reply is shown. If there is no reply after one unchanged resubmission, ACC-06 is BLOCKED.

**A-S2 — SET-UP from ACC-03 (not judged; uses the first two sentences of amendment 3's starting condition; ACC-03's actions are not run)**
1. Stop the product.
2. Start with `resume --last -c 'approvals_reviewer="auto_review"' -c 'approval_policy="on-request"' -c 'sandbox_mode="read-only"'`.
3. Confirm the restored transcript shows the `OK` turn (`r1a-s2-resumed`). If it doesn't, pick that conversation with `/resume`.

This puts the conversation in the settings it had when ACC-06 ran in the first run.

**ACC-06 (blocker) — `/review` is labelled `code-review:`**
- **Purpose:** work done by `/review` is recorded once, under the conversation that ran it, labelled `code-review:`.
- **Start (after A-S2, same conversation):**
  1. Run `/cost` and read it per G8 with base `r1a-acc06-before`. Record:
     - R0, the request count;
     - T0, the own total;
     - the other-conversations line (count O0 and cost, or "No other conversation…").
  2. Read every request page per G9 (`r1a-acc06-before-req<k>`), recording each ID and cost. Then press Esc until the composer shows.
  3. Edit a tracked file in the workspace by appending one line, `// review me`, and leave it uncommitted. Do it outside the product as in the first run, not by asking the product's model, and record the file name.
- **Actions:**
  1. Type `/review` and press Enter. Capture `r1a-acc06-review-menu`.
  2. Choose the option to review uncommitted changes. Wait for the review to finish (up to 10 min) without typing; any approvals go to the automatic reviewer. Capture the end of the review as `r1a-acc06-review-done`.
  3. Run `/cost`, applying G1, and read it per G8 with base `r1a-acc06-after`.
  4. Read every request page per G9 (`r1a-acc06-after-req<k>`). A **new request** is one whose ID is not in the start list. For each new request, also capture its technical page as `r1a-acc06-after-req<k>-tech`, showing `Request:` and `Turn:`.
- **Expected (frozen):**
  1. **The new request(s) have a `Turn:` line starting with `code-review:`.**
     - At least one new request is listed, and every new request's tech capture shows `Turn: code-review:…`.
     - No new request after G1 is a FAIL.
     - Evidence: `r1a-acc06-after-links`, `r1a-acc06-after-req<k>-tech`.
  2. **They are listed once, under this conversation.**
     - Each new ID is on exactly one request page of this conversation.
     - The request count on `r1a-acc06-after-first` equals R0 plus the number of distinct new IDs.
     - Evidence: `-after-first`, `-after-req<k>`.
  3. **The other-conversations count is unchanged.**
     - The other-conversations line on `-after-first` shows the same count as on `-before-first`, and no review request is listed there.
     - Evidence: `r1a-acc06-before-first`, `r1a-acc06-after-first`.
  4. **Earlier costs are unchanged.**
     - Every start ID is found again with the same cost (G11).
     - Evidence: `-before-req<k>`, `-after-req<k>`.
- **Observation only:** the number of review requests, any automatic reviews shown, and the file edited.

## R1.4 Group C-R1

**C-S1 — SET-UP from ACC-10 (not judged; uses amendment 8's actions 2–3; action 1's P10 reading is left out because ACC-13 is not re-run)**
1. Fresh profile. Start a new conversation with `--route openai --model gpt-6-luna` and no `-c` (skip off). Wait for the composer.
2. Immediately (G13) type `Reply with the single word ALPHA.`, press Enter and wait for `ALPHA`. Capture `r1c-s1-alpha`.

Checkpoint: the reply is shown, with no ⚠ warm-up notice. If a notice appears, record it and continue. This conversation is **ALPHA**.

**ACC-11 (blocker) — a restart sends one labelled warm-up and shows a notice**
- **Purpose:** restarting on a saved OpenAI conversation sends one labelled, announced warm-up, counted once, without re-pricing history.
- **Start (ALPHA, after C-S1):**
  1. Run `/cost` (G1) and read it per G8 with base `r1c-acc11-before`. Record T (the own total) and the request count n.
  2. Read every request page per G9 (`r1c-acc11-before-req<k>`). Their costs are C1…Cn; record them with their IDs.
     - Checkpoint: no `Reopen warm-up` line and no link containing it.
  3. Press Esc until the composer shows, then stop the product.
- **Actions:**
  1. Start with `resume --last` and no `-c` (skip off). Without typing, wait up to 60 s for `skip_reopen_warmup`. Capture `r1c-acc11-notice` showing the notice and the ALPHA transcript, and note the seconds since start.
  2. Run `/cost` (G1) and read it per G8 with base `r1c-acc11-after`.
  3. Open the request whose link contains `Reopen warm-up`:
     - its request page: `r1c-acc11-warmup-req`;
     - its technical page: `r1c-acc11-warmup-tech`;
     - its money lines: `r1c-acc11-warmup-tech-money` (`-2` if needed).
  4. Read every other request page per G9 (`r1c-acc11-after-req<k>`).
- **Expected (amendment 9 unchanged; item 7 is added by R1):**
  1. **Notice.**
     - Within 60 s of start, without typing, a ⚠ notice appears that names `skip_reopen_warmup` and says the warm-up is billed.
     - A reopen without a warm-up, or a warm-up without a notice, is a FAIL.
     - Evidence: `r1c-acc11-notice`, `r1c-acc11-after-first`.
  2. **Warm-up line.** The first screen has a `Reopen warm-up:` line counting 1 request, with a cost.
     - Evidence: `r1c-acc11-after-first`.
  3. **Warm-up link.** Exactly one request link contains `Reopen warm-up`, and its request page contains `Reopen warm-up`.
     - Evidence: `r1c-acc11-after-links`, `r1c-acc11-warmup-req`.
  4. **Warm-up arithmetic.**
     - On its technical page, each component cost = tokens × rate / 1,000,000, and the known subtotal equals its cost. Show the arithmetic.
     - Observation only: the input token count.
     - Evidence: `r1c-acc11-warmup-tech-money`.
  5. **New total.** The new total = T + the warm-up cost, counted once: the own total on `-after-details` equals the own total on `-before-details` plus the warm-up's `Known subtotal exact USD`, exactly.
  6. **History unchanged.** C1…Cn and their Request IDs are unchanged: every ID from `-before-req<k>` is found again with the same cost (G11).
     - Evidence: `r1c-acc11-after-req<k>`.
  7. **Each Request ID appears once.**
     - The IDs on all request pages read in action 3–4 are all different, and their number equals the request count on `-after-first`.
     - The warm-up's ID is not one of the start IDs.
- **Observation only:** the warm-up's `Turn:` prefix.

**ACC-14 (blocker) — `/resume` to a different saved conversation sends a warm-up**
- **Purpose:** switching to a different saved conversation with `/resume` sends a labelled, announced warm-up without re-pricing or duplicating history.
- **Start:**
  1. Press Esc until the composer shows, then stop the product.
  2. Start a **new conversation** with `--route openai --model gpt-6-luna` and no `-c` (skip off). Wait for the composer.
  3. Immediately (G13) type `Reply with the single word CHARLIE.`, press Enter and wait for `CHARLIE`. Capture `r1c-acc14-charlie-reply`. This conversation is **CHARLIE**.
  4. Run `/cost` (G1) and read it per G8 with base `r1c-acc14-charlie`. Read every request page per G9 (`r1c-acc14-charlie-req<k>`), recording IDs, costs and the own total.
     - Set-up checkpoint: at least 1 request is listed and there is no `Reopen warm-up` line. If nothing is listed after G1, record it and continue.
  5. ALPHA's "before" record is ACC-11's last reading (`r1c-acc11-after-*`, `r1c-acc11-warmup-req`): its IDs, costs, own total and warm-up count 1. Nothing has happened in ALPHA since.
- **Actions:**
  1. Press Esc until the composer shows. Type `/resume` and press Enter; capture `r1c-acc14-picker`.
  2. Select the conversation whose first message is `Reply with the single word ALPHA.` and press Enter.
  3. Without typing, wait up to 60 s. Capture `r1c-acc14-notice`, showing the ALPHA transcript.
  4. Run `/cost` (G1) and read it per G8 with base `r1c-acc14-after`.
  5. Read every request page per G9 (`r1c-acc14-after-req<k>`), including the new warm-up's.
- **Expected (amendment 12 unchanged; item 4 spells out "each ID appears once"):**
  1. **Notice.** A notice appears.
     - Evidence: `r1c-acc14-notice`.
  2. **Warm-up count.** The `Reopen warm-up` line counts 2.
     - Evidence: `r1c-acc14-after-first`.
  3. **History unchanged.** Earlier request costs and IDs are unchanged: every ID on `r1c-acc11-after-req<k>` and `r1c-acc11-warmup-req` is found again with the same cost (G11).
     - Evidence: `r1c-acc14-after-req<k>`.
  4. **Each ID appears once.**
     - ALPHA's request-page IDs are all different, and their number equals the request count on `-after-first`.
     - No CHARLIE ID (`r1c-acc14-charlie-req<k>`) appears among ALPHA's.
- **Observation only:** the other-conversations lines on `r1c-acc14-after-first`. They are expected to show CHARLIE and are ACC-15's baseline.

**ACC-15 (blocker) — `/side` on an OpenAI conversation sends a labelled warm-up**
- **Purpose:** opening `/side` on an OpenAI conversation sends one labelled, announced warm-up. The side turn's work is labelled `side:` and recorded under the saved conversation.
- **Start:** ALPHA is open after ACC-14, with `/cost` closed and the composer showing. The baseline is ACC-14's reading:
  - warm-up count W = 2;
  - ALPHA's IDs;
  - the other-conversations lines on `r1c-acc14-after-first`.
- **Actions:**
  1. Type `/side`, press Enter, wait about 10 s and capture `r1c-acc15-side-open`.
  2. Type `Reply with the single word SIDE.`, press Enter, wait for `SIDE` and capture `r1c-acc15-side-reply`.
  3. Press Ctrl+C once (`agms-tui key C-c`) and wait for the product's confirmation that you are back in the saved conversation. Capture `r1c-acc15-back`.
  4. Run `/cost` (G1) and read it per G8 with base `r1c-acc15-after`.
  5. Read every request page per G9 (`r1c-acc15-after-req<k>`). New requests are IDs that were not in ACC-14's list.
     - The new request whose page contains `Reopen warm-up`: capture it again as `r1c-acc15-warmup-req`, and its technical page as `r1c-acc15-warmup-tech`.
     - Every other new request (the side turn's): capture its technical page as `r1c-acc15-side-req<k>-tech`.
  6. Before each technical-page capture, press Down until the `Turn:` line is inside the window. Retake the capture (G7) if it doesn't show `Turn:`.
- **Expected (frozen; amendment 2 governs leaving `/side`):**
  1. **The notice is shown once.**
     - Across `-side-open`, `-side-reply` and `-back`, exactly one new ⚠ warm-up notice appears after `/side` is typed. A notice still visible on a later capture of the same view is the same notice.
     - If one appears in the side view and another in the saved conversation after return, record both lines exactly and mark this result "ambiguous" for the integrator.
  2. **The warm-up count goes up by 1.** The `Reopen warm-up:` line on `r1c-acc15-after-first` counts 3.
  3. **That warm-up's request page says Reopen warm-up.** Exactly one new request's page contains `Reopen warm-up`.
     - Evidence: `r1c-acc15-warmup-req`.
  4. **The side turn's requests have `side:` labels and sit under this conversation.**
     - At least one new request other than the warm-up is listed in this conversation's `/cost`.
     - Each of them shows `Turn: side:…` on its capture.
     - None listed, while `SIDE` was shown, is a FAIL.
     - Evidence: `r1c-acc15-after-links`, `r1c-acc15-side-req<k>-tech`.
  5. **Nothing is listed under other conversations because of this step.** On `r1c-acc15-after-first`, the other-conversations request count, conversation count and cost are identical to those on `r1c-acc14-after-first`.
- **Observation only:** the warm-up's `Turn:` prefix (`r1c-acc15-warmup-tech`).

**C-S2 — SET-UP from ACC-36 (not judged; uses amendment 15's starting condition; ACC-36's actions are not run)**
1. Press Esc until the composer shows, then stop the product.
2. Run `agms-tui config 'skip_reopen_warmup = true'`. Do not start the product.

ALPHA's warm-up count (3) and total are those recorded in ACC-15.

**ACC-18 (blocker) — history is not re-priced, and nothing is counted twice, across the group**
- **Purpose:** after all the group's reopens, every earlier request keeps its cost and ID, and every conversation's figures agree with each other.
- **Start:** after C-S2. Skip is on through `config.toml` and the product is stopped. The records from ACC-11, ACC-14 and ACC-15 are at hand.
- **Actions:**
  1. Start with `resume --last` and no `-c`. Without typing, wait 60 s and capture `r1c-acc18-open-1`. Note which conversation opened; ALPHA is expected.
  2. Read the open conversation, with `<c>` = `alpha`, `charlie` or `conv<n>` for any other:
     - Run `/cost` (G1) and read it per G8 with base `r1c-acc18-<c>`.
     - Read every request page per G9 (`r1c-acc18-<c>-req<k>`).
     - For every request whose cost line is marked `(rounded)`, or shows no plain figure, open its technical page(s) and capture `Known subtotal exact USD` as `r1c-acc18-<c>-req<k>-exact`.
     - Press Esc until the composer shows.
  3. Type `/resume` and press Enter; capture `r1c-acc18-picker-<n>`. Select a conversation not yet read (by its first message) and press Enter. Without typing, wait 60 s and capture `r1c-acc18-open-<n>`. Repeat action 2.
  4. Repeat action 3 until every conversation of the profile (G15) has been read. ALPHA and CHARLIE are expected, plus any STRAY.
  5. If a listed or counted conversation can't be opened, capture the evidence as `r1c-acc18-residual-<n>` (G15).
- **Expected (amendment 16 restated; item 3 now covers every conversation; item 5 adds "once within a conversation"):**
  1. **No notice, and no warm-up count rises.**
     - No `r1c-acc18-open-<n>` capture shows a new ⚠ notice.
     - ALPHA's `Reopen warm-up` line counts 3, as on `r1c-acc15-after-first`.
     - CHARLIE has no `Reopen warm-up` line, as on `r1c-acc14-charlie-first`.
  2. **Every cost and Request ID noted earlier is identical.**
     - Every ID recorded in ACC-11, ACC-14 (ALPHA and CHARLIE) and ACC-15 is found again in the same conversation with the same cost (G11).
     - ALPHA's own total equals `r1c-acc15-after-details`, and CHARLIE's equals `r1c-acc14-charlie-details`, exactly.
  3. **Each conversation's request costs sum to its total, for every conversation of the profile.**
     - For each conversation, the sum of the exact costs of all its requests equals its own total on `r1c-acc18-<c>-details`, exactly. Use request-page figures not marked `(rounded)`, otherwise the `-exact` captures, and show the arithmetic.
  4. **A conversation's total as seen from the others equals its own total.**
     - On each read conversation's `-first`: own total + other-conversations cost = the sum of the own totals of all conversations of the profile (G12 tolerance on the rounded figure).
     - The other-conversations request count equals the sum of the other conversations' request counts, and the other-conversations conversation count equals the number of the others.
     - With exactly two conversations, CHARLIE's other-conversations cost matches ALPHA's own total, and ALPHA's matches CHARLIE's; request counts likewise.
  5. **No Request ID appears twice.**
     - Within each conversation, the IDs are all different and their number equals its request count.
     - No ID appears in two conversations.
- **Residual (a conversation that can't be opened):** it is recorded, not a FAIL (amendment 16).
  - Results 3 and 5 are judged over the conversations that were read.
  - Result 4 is judged in this form: own total + other-conversations cost is the same on every read conversation's `-first` (G12). The conversation counts include the residual.
  - Record the residual's derived total (that common value minus the read own totals) and its derived request count.
  - Report it as a deviation (G15), with the step that created it if known.
  - A residual adds no PASS for itself.

## R1.5 Limits of this re-run

- ACC-18 covers only the conversations this re-run creates: ALPHA (with its `/side` work) and CHARLIE.
  - Not covered: BRAVO (ACC-13), the edit branch (ACC-16) and reviewer or sub-agent requests (ACC-17).
  - Those cases' first-run results stand and are not re-judged.
- The money comparison against the other-conversations cost depends on a rounded figure (G12), unless the build shows an exact one. If it does, use the exact figure.

---

**Open questions**

1. **ACC-18 coverage.** Is ACC-18 over ALPHA and CHARLIE only acceptable? Alternatively, I could repeat ACC-16 as set-up so ACC-18 also covers an edit-created conversation. That costs about one turn and one warm-up and adds a conversation, and group C's time would then sit near 85 minutes.
2. **Group A set-up.** Should A-S2's restart with the automatic-reviewer settings stay, to reproduce the first run's state at ACC-06? Or should ACC-06 run in a plain conversation?
3. **Editing the file for ACC-06.** How does the tester append `// review me` to a tracked file? I assumed the same means as in the first run; if the harness has a named control, it should be stated.
4. **Notice in two views.** If the `/side` warm-up notice appears both in the side view and in the saved conversation, does that count as "shown once"? I left it as "ambiguous" for the integrator.
5. **Descendant attempts.** If `/review`'s or `/side`'s requests are reachable only through `→ Descendant attempts`, do they count as "under this conversation"? G9 assumes they do.
6. **Group A length.** Group A will take about 35–45 minutes, below the 60–80 band, because it holds only ACC-06. Is that acceptable, rather than padding it?