# Re-run R1 of five PF-60-S10 accounting cases: what the first run found, and what this re-run must settle

Written by the integrator (2026-10-22) for a fresh designer. User terms only; it describes what the testers saw on
screen in the first run (PF-60-S10, 2026-10-16), not how the product is built.

## What this re-run is

The first run's frozen design (`s10-frozen/design-cases.md`) and its review amendments (`s10-frozen/amendments.md`)
stay in force. Of its 37 cases, five are re-run now, on a newer build, by fresh code-blind testers:

- **ACC-06** (group A, route `zai`): failed in the first run.
- **ACC-15** (group C, route `openai`): blocked on one unjudged result.
- **ACC-11, ACC-14, ACC-18** (group C, route `openai`): blocked on unjudged results.

No other case is re-run or re-judged. In particular, not ACC-35 (needs real Moonshot spend that is not granted),
and not the advisory cases ACC-04 and ACC-22. Earlier cases of groups A and C may be repeated only as set-up for
these five (their results are then not judged, or judged only as set-up checkpoints).

The design question for you: write an amendment (call it **amendment R1**) that replaces the text of only these five
cases, says exactly which set-up steps each needs, and makes sure the results that were left unjudged are observed
and judged this time. Keep each case's purpose and its frozen expected results; add the steps needed to observe
them. Do not weaken any expected result.

## Per case: first-run observations

### ACC-06 — `/review` is labelled `code-review:` (blocker)

First run: after `/review` of uncommitted changes, `/cost` (re-read four times over three minutes) showed no new
request and no `code-review:` label anywhere, and no other conversations. The case failed and was filed as issue
#898.

Since then: issue #898 is closed as fixed. The frozen expected results of ACC-06 stand unchanged: the review's
request(s) are listed once, under the conversation that ran `/review`, with a `Turn:` line starting with
`code-review:`; the other-conversations count is unchanged; earlier costs are unchanged.

### ACC-11 — a restart sends one labelled warm-up and shows a notice (blocker)

First run (replay C2): everything observed passed: the notice naming `skip_reopen_warmup` appeared; `Reopen warm-up:
1 request`; one link and the request page said Reopen warm-up; the technical page's arithmetic was exact; earlier
request costs were unchanged.

Unjudged: whether the earlier requests' **Request IDs** are unchanged after the restart. No earlier request's page
was re-opened after the restart, so no Request ID was compared.

### ACC-14 — `/resume` to a different saved conversation sends a warm-up (blocker)

First run: the notice appeared, `Reopen warm-up: 2 requests`, earlier request costs unchanged.

Unjudged: that earlier Request IDs are unchanged and each **appears once** (no Request ID was on screen in this case).

The tester also skipped this case's starting step (a turn in the new conversation), which left a conversation holding
only a warm-up request (see ACC-18).

### ACC-15 — `/side` on an OpenAI conversation sends a labelled warm-up (blocker)

First run: the notice appeared, the warm-up count went from 2 to 3, the new warm-up's request page said Reopen
warm-up, nothing was added under other conversations.

Unjudged: the **`side:` label** of the side turn's requests. The tester scrolled past a technical page whose `Turn:`
line started with `side:` but captured no screen showing it, and reported it as not found. This re-run must put that
line on a captured screen and judge it.

### ACC-18 — history is not re-priced, and nothing is counted twice, across the group (blocker)

First run: with skip on, no warm-up rise; the main conversation's request costs were unchanged and summed to its
total; the first screen reconciled (this conversation + other conversations = the sum of all totals; counts add up).

Unjudged:
- "Every Request ID identical" and "no Request ID appears twice": no request page was opened, so no ID was compared.
- Per-conversation sums were checked only for the main conversation. The other conversations' request lists were not
  opened. This re-run must check, **for every conversation of the profile**, that its request costs add up to its
  total and that its total as seen from the others equals its own total.

Two conversations could not be opened from `/resume` in the first run: each held only a warm-up request (a new or
forked conversation left without a turn). The first run's amendment allowed such residuals. Your design should avoid
creating them where it can (for example by never leaving a new conversation without a turn), and say how to treat one
if it appears.

## Other first-run observations that matter for the design

- `/cost` pages are taller than the terminal; reaching the `→` links can take 20–40 presses of Down (the navigation
  note in `s10-frozen/navigation.md`). A tester in the first run gave up on navigation; the replay succeeded with the
  note.
- Editing an earlier message continued in a new conversation (product: "You're continuing from this point in a new
  conversation"); its warm-up is counted there. ACC-16 is not re-run.
- On the OpenAI route the automatic reviewer has no price ("Codex Auto Review"). ACC-17 is not re-run; your design
  need not trigger the reviewer.
- Each request page shows a Request ID; the technical page shows the `Turn:` line (see `screens/`).
