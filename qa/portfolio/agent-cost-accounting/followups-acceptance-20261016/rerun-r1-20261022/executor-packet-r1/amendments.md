# Amendments to the frozen design (independent design review, 2026-10-16)

The independent reviewer (fresh read-only session, see `review-output.md`) returned `VERDICT: CHANGES` with the 22
amendments below. The integrator applied all of them exactly as written; nothing was added, dropped or reworded. Where an
amendment changes a case in `design-cases.md`, the amendment governs. New cases: ACC-35 (optional, owner grant D3),
ACC-36 and ACC-37. The JSON block at the end holds the same amendments with the full text of the new cases.

### Amendments

1. **ALL cases — starting conditions — add.**
   - Text: "General rules:
     - After each turn, wait for the reply. If `/cost` doesn't yet list the expected new requests, choose `Refresh` (or reopen `/cost`) up to 3 times, 5 s apart, before judging.
     - Compare money with the exact USD lines (`Known subtotal exact USD`, `exact USD`), not rounded displays.
     - Note the UTC time at the start and end of each group. If a group crosses 00:00 UTC, compare earlier requests with `/cost <earlier UTC date>`. Don't fail a case only because earlier requests moved off today's view.
     - To leave `/side`, press Ctrl+C once (`agms-tui key C-c`) and wait for the on-screen confirmation. Esc does not leave `/side`.
     - Match provider and model names regardless of display formatting.
     - If the Moonshot relay answers `test harness: Moonshot request limit reached`, record BLOCKED, never retry."
   - Why: the answers establish the timing lag, the Ctrl+C exit and the relay limit. Midnight crossings and rounding would otherwise cause false FAILs.

2. **ACC-02, ACC-03, ACC-04, ACC-15 — actions — replace.**
   - Replace "Leave the side conversation (see Q1)" with: "Press Ctrl+C once (`agms-tui key C-c`) and wait for the product's confirmation that you are back in the saved conversation."
   - Why: answer 1. The Esc assumption is wrong.

3. **ACC-03 — starting conditions and expected — replace.**
   - Starting conditions: "Same profile. Stop, then start with `resume --last -c 'approvals_reviewer="auto_review"' -c 'approval_policy="on-request"' -c 'sandbox_mode="read-only"'`. Confirm that the restored transcript is the ACC-01 conversation; otherwise pick it with `/resume`. Run `/cost` and note every Request ID, the request count and the other-conversations count."
   - Expected:
     - "The session shows ≥1 automatic review of the `touch` command. If none appears after one retry of the prompt, record BLOCKED."
     - "The number of new requests whose `Turn:` starts with `side:review:` equals the number of automatic reviews shown (normally 1), each on Z.AI."
     - "Every new request since the noted list has a `Turn:` starting with `side:`; none is unprefixed."
     - "Each Request ID appears once, and the other-conversations count is unchanged."
     - "Observation only: the reviewer's decision and whether `side-review-check.txt` exists."
   - Why: answer 2 gives the flags. "Exactly one" false-fails on a retried command. Requiring every new request to start with `side:` is a robust way to detect a duplicate. Whether the file exists is not accounting behaviour.

4. **ACC-07 — actions and expected — replace.**
   - Actions:
     1. "`agms-tui exec --ephemeral 'Reply with the single word EPHEMERAL.'`"
     2. "Start with `resume --last`. Confirm the transcript is the group-A conversation, not the EPHEMERAL prompt. Run `/cost`."
     3. "Stop. Run `agms-tui exec 'Reply with the single word CONTROL.'`"
     4. "Start with `resume --last`. Note which conversation opened, then run `/cost`."
   - Expected:
     - "Each exec output contains its word."
     - "After step 2: `resume --last` opened the group-A conversation. Its totals, Request IDs, other-conversations count and cost are unchanged."
     - "After step 4: the conversations with recorded requests (this one plus the others) number exactly one more than after step 2. The new one has ≥1 Z.AI request."
     - "The group-A total is unchanged, whether seen directly or under other conversations."
   - Why: the answers give the harness exec syntax. After the control run, `resume --last` probably opens the exec's conversation. Distinct prompts make the transcripts identifiable, and step 2 then also proves the ephemeral run wasn't saved.

5. **ACC-08 — starting conditions and actions — replace/add.**
   - Starting conditions: "Fresh profile; `--route moonshot --model kimi-k3` (default provider name `kimi`); new conversation; exactly one turn, never retried."
   - Actions, add: "Note the time the reply appears. Immediately capture the raw text of `/cost`, each request page and each technical page. Do the arithmetic afterwards, so that ACC-35a (if granted) can start within 4 minutes."
   - Why: the cache-write lifetime is 5 minutes, and the owner's under-$0.01 limit needs the second turn to hit the cache.

6. **ACC-09 — starting conditions and expected — replace.**
   - Starting conditions: "Same profile; stop; start `--route moonshot --provider-name moonlab --model kimi-k2.7-code`; new conversation; one turn only, never retried. Capture screens immediately, as in ACC-08."
   - Expected:
     - "The first-screen line names provider `moonlab` and model `kimi-k2.7-code`, Pay per use, with an estimated $ cost, not 'no price'."
     - "Technical page rates: noncached input 0.95, cache read 0.19, cache write 0.95, output 4 USD per million."
     - "The ACC-08 arithmetic and completeness checks hold, whether or not cache counts are marked (reported)."
     - "Other conversations lists `kimi · kimi-k3` once, at exactly the k3 conversation's last observed total (from ACC-08, or ACC-35a if run)."
   - Why: answer 6 makes "whatever the person named the provider" testable at no extra spend. The tester can't know whether Moonshot omitted a count.

7. **NEW ACC-35 (optional, gated on owner grant D3), group B.** Full text is in the JSON.
   - Part a runs in the ACC-08 conversation and part b in the ACC-09 conversation, each within 4 minutes of that conversation's first reply.
   - If D3 is not granted, record "not checked live; covered by test". If the window is missed, don't send the turn.
   - Expected: the cache-read rate and cost are correct where cache read > 0; if every cache read is 0, record "cache read not observed live". Completeness checks apply. First-turn costs are unchanged.
   - Why: required by the owner decision. Moonshot requests stay well under the 12-request relay limit.

8. **ACC-10 — actions — replace.**
   - Actions:
     1. "Before typing, wait 15 s, run `/cost`, choose Refresh once, and record the request count P10 and each request's input tokens. Press Esc."
     2. "Type `Reply with the single word ALPHA.` and wait."
     3. "Run `/cost`."
   - Expected, add: "Record P10, the total request count N10, C1…Cn and T."
   - Why: ACC-13 needs a baseline for "a brand-new conversation still warms up". The ALPHA prompt makes the conversation identifiable in `/resume`.

9. **ACC-11 — expected — replace.**
   - "Within 60 s of start, without typing, a ⚠ notice appears that names `skip_reopen_warmup` and says the warm-up is billed. A reopen without a warm-up, or a warm-up without a notice, is a FAIL."
   - "The `/cost` first screen has a `Reopen warm-up:` line counting 1 request, with a cost."
   - "Exactly one request link contains `Reopen warm-up`, and its request page contains `Reopen warm-up`."
   - "On its technical page, each component cost = tokens × rate, and the known subtotal = its cost. Observation only: the input token count."
   - "The new total = T + the warm-up cost, counted once."
   - "C1…Cn and their Request IDs are unchanged."
   - Why: "typically 10–15k" is not a requirement. The exact `Request k · …` link format and `Kind:` label come from screens, not from the intent. "Only after it was sent" is restated as something observable.

10. **ACC-12 — starting conditions and expected — replace.**
    - Starting conditions: "In the ACC-11 session, type `Reply with the single word OK.`, wait, run `/cost`, and record T12, the request list and the warm-up count. Stop, then start with `resume --last -c skip_reopen_warmup=true`."
    - Expected:
      - "The restored transcript shows the earlier turns but not ACC-11's ⚠ notice, and no new notice appears within 60 s."
      - "Before the new turn: the warm-up count is still 1, the total = T12, and the request list is unchanged."
      - "After the turn, only the turn's requests are added, and none is labelled Reopen warm-up."
    - Why: the baseline was undefined, because the start condition itself adds a turn.

11. **ACC-13 — actions and expected — replace.**
    - Actions:
      1. "Before typing, wait 15 s, run `/cost` and Refresh once, and record P13. Press Esc."
      2. "Type `Reply with the single word BRAVO.` and wait."
      3. "Run `/cost`."
    - Expected:
      - "No notice, no `Reopen warm-up` line, and no link containing `Reopen warm-up`."
      - "P13 = P10, and the request count after the turn = N10. If they differ, compare per-request input tokens. A missing large first request that ACC-10 had is a FAIL."
    - Why: "matches ACC-10" was not measurable, and Q10 was not answered.

12. **ACC-14 — starting conditions, actions and expected — replace.**
    - Starting conditions: "Stop. Start a new conversation (no resume, skip off), type `Reply with the single word CHARLIE.`, and wait."
    - Actions:
      1. "Run `/resume`, select the conversation whose first message is `Reply with the single word ALPHA.`, and press Enter."
      2. "Run `/cost`."
    - Expected: "A notice appears. The `Reopen warm-up` line counts 2. Earlier request costs and IDs are unchanged, and each ID appears once."
    - Why: without CHARLIE, the start conversation has no message and can't be identified or reopened in ACC-18. ALPHA makes the picker choice unambiguous.

13. **ACC-16 — actions — replace.**
    - Actions:
      1. "Record the warm-up count and the request list."
      2. "Press Esc twice in the empty composer to step back to an earlier message, following on-screen hints. If the product offers no way to edit an earlier message, record BLOCKED."
      3. "Select the most recent message you typed, replace it with `Reply with the single word EDITED.` and press Enter. Wait for `EDITED`."
      4. "Run `/cost`."
    - Why: the packet doesn't document the edit control, and there are two identical "OK" messages to choose between.

14. **ACC-17 — priority, starting conditions, actions and expected — replace.**
    - Priority: blocker for the reviewer part.
    - Starting conditions: "Stop. Start with `resume --last` plus the three auto-review `-c` options from ACC-03 (skip off). This restart is itself a reopen: wait for its notice, then run `/cost` and record the warm-up count W0 and the request list."
    - Actions:
      1. "Type ``Run the shell command `touch warmup-review.txt` in the workspace.`` and wait for the review and the reply."
      2. "Type the sub-agent prompt and wait."
      3. "Run `/cost`."
    - Expected:
      - "No new notice; the warm-up count is still W0."
      - "The reviewer and sub-agent requests are each listed once, and none is labelled Reopen warm-up."
      - "No review after one retry: the reviewer part is BLOCKED. Sub-agent declined: that part is not exercised."
    - Why: the optional restart changed the count the case checks. The answers make the reviewer reliably reachable.

15. **NEW ACC-36 (blocker), group C, after ACC-17 and before ACC-18 — `config.toml` skip.**
    - Starting conditions: "Stop. Run `agms-tui config 'skip_reopen_warmup = true'`. Note ALPHA's warm-up count W and its total."
    - Actions: "Start with `resume --last` and no `-c`. Confirm the conversation is ALPHA. Wait 60 s, then run `/cost`."
    - Expected: "No notice; the warm-up count is still W; the total and request list are unchanged."
    - Why: answer 7.

16. **ACC-18 — starting conditions, actions and expected — replace.**
    - Starting conditions: "After ACC-36, with skip on through `config.toml`."
    - Actions: "Start with `resume --last`. Open each conversation (ALPHA, BRAVO, CHARLIE and any others in `/resume`) with `/resume`, run `/cost`, and record its own exact total, its request IDs and costs, and its other-conversations count and cost."
    - Expected:
      - "No notice, and no warm-up count rises."
      - "Every cost and Request ID noted in ACC-10 to ACC-17 and ACC-36 is identical."
      - "Each conversation's request costs sum to its total."
      - "On each first screen, this conversation's total + the other-conversations cost = the sum of all own totals, within display rounding. The other-conversations count = the sum of the other conversations' request counts. A conversation that can't be opened is recorded as a residual, not a FAIL."
      - "No Request ID appears in two conversations."
    - Why: the first screen aggregates other conversations by provider and model, so a single conversation's total can't be seen from another.

17. **ACC-21 — expected — replace.**
    - "A separate `gpt-image-2` Pay-per-use line with 1 request."
    - "Rates on the technical page: input 5 and output 30 USD per million. Cost = input×5/M + output×30/M exactly; if cache read > 0, flag it and recompute."
    - "Observation only: the `Turn:` prefix."
    - "No rate and 0 tokens reads `none — 0 tokens`. A rate and 0 tokens keeps its $0 figure. Tokens with no rate show no money."
    - "One gpt-image-2 Request ID."
    - Why: `image:` and the cache-write rate status come from screens only.

18. **ACC-23 to ACC-26 — starting conditions and expected — add/replace.**
    - ACC-23 starting conditions, add: "Run `agms-tui standin no-charge off` first."
    - ACC-23 and ACC-25 expected: noncached input reads 1,500 (ACC-25: 2,000) **or** unknown/not reported, never another number.
    - ACC-23 to ACC-26 expected: the totals check is `Known subtotal exact USD` = N × charge.
    - ACC-24 to ACC-26 expected, add: "Any published-price figure is labelled as an estimate that was not used."
    - ACC-23 and ACC-24 expected: the Price-basis field name is `cost` or ends in `.cost`.
    - Why: the stand-ins don't report cache writes, so rule 1 allows "unknown". The answers leave the no-charge state undefined, so it must be set explicitly.

19. **NEW ACC-37 (blocker), group E, after ACC-29 — no-charge fallback.** Full text is in the JSON.
    - Why: answer 4 makes the fallback testable.

20. **ACC-30 to ACC-33 — expected — replace.** Wording that doesn't depend on screen labels. "Same work at API prices: not available" is explicitly allowed and is not "no price".
    - Why: the earlier wording would let an executor fail ACC-31 and ACC-32 on the screen's "not available".

21. **ACC-34 — priority and expected.**
    - Priority: advisory.
    - Expected: add the pattern `agms-synthetic`, scan the workspace files too, and report any hit.
    - Why: answer 15.

22. **NEW LIMITS — add.**
    - `fork:` (answer 3).
    - Moonshot unlisted model → "no price", and the `kimi-k2.7-code-highspeed` and `kimi-k2.6` rates (the route and spend limit allow only two first turns).
    - The 5-minute cache-write lifetime (not shown on screen).
    - Same-conversation `/resume` (Q8).
    - The label of a brand-new conversation's warm-up (beyond ACC-13).
    - Realtime #824, DeepSeek calendar, BigModel, TensorCash, older records, Windows/Linux.
    - Why: these limits must be named so coverage is complete.

```json
{"verdict":"CHANGES","amendments":[
{"n":1,"case":"ALL","field":"starting conditions","change":"add","text":["After each turn wait for the reply; if /cost doesn't yet list the expected new requests, choose Refresh (or reopen /cost) up to 3 times, 5 s apart, before judging.","Compare money using exact USD lines (Known subtotal exact USD / exact USD), not rounded displays.","Note UTC time at start and end of each group; if a group crosses 00:00 UTC, compare earlier requests with `/cost <earlier UTC date>` and don't fail a case only because earlier requests left today's view.","To leave /side press Ctrl+C once (`agms-tui key C-c`) and wait for the on-screen confirmation; Esc does not leave /side.","Match provider/model names regardless of display formatting.","If the Moonshot relay answers `test harness: Moonshot request limit reached`, record BLOCKED; never retry."],"why":"Timing lag, side exit and relay limit come from answers.md; midnight crossings and rounding otherwise cause false FAILs."},
{"n":2,"case":"ACC-02, ACC-03, ACC-04, ACC-15","field":"actions","change":"replace","text":"Replace 'Leave the side conversation (see Q1)' with: Press Ctrl+C once (`agms-tui key C-c`) and wait for the product's confirmation that you are back in the saved conversation.","why":"Answer 1; the Esc assumption is wrong."},
{"n":3,"case":"ACC-03","field":"starting conditions; expected","change":"replace","text":{"starting_conditions":"Same profile. Stop, then start with `resume --last -c 'approvals_reviewer=\"auto_review\"' -c 'approval_policy=\"on-request\"' -c 'sandbox_mode=\"read-only\"'`. Confirm the restored transcript is the ACC-01 conversation (else pick it with /resume). Run /cost; note every Request ID, the request count and the other-conversations count.","expected":["The session shows at least one automatic review of the touch command; if none after one retry of the prompt, record BLOCKED.","The number of new requests whose Turn: starts with side:review: equals the number of automatic reviews shown (normally 1); each is on Z.AI.","Every new request since the noted list has a Turn: starting with side: (including side:review:); none is unprefixed.","Each Request ID appears once; the other-conversations count is unchanged.","Observation only: the reviewer's decision and whether side-review-check.txt exists."]},"why":"Answer 2 gives the flags; 'exactly one' false-fails on a retried command; requiring all-side: prefixes detects a duplicate; file presence is not accounting behaviour."},
{"n":4,"case":"ACC-07","field":"actions; expected","change":"replace","text":{"actions":["`agms-tui exec --ephemeral 'Reply with the single word EPHEMERAL.'`","Start with `resume --last`; confirm the transcript is the group-A conversation, not the EPHEMERAL prompt; run /cost.","Stop. `agms-tui exec 'Reply with the single word CONTROL.'`","Start with `resume --last`; note which conversation opened; run /cost."],"expected":["Each exec output contains its word.","After step 2: resume --last opened the group-A conversation; its totals, Request IDs, other-conversations count and cost are unchanged.","After step 4: conversations with recorded requests (this one plus others) number exactly one more than after step 2, and the new one has at least 1 Z.AI request.","The group-A total is unchanged (seen directly or under other conversations)."]},"why":"Harness exec syntax; resume --last after the control likely opens the exec conversation; distinct prompts make transcripts identifiable and step 2 also proves the ephemeral run wasn't saved."},
{"n":5,"case":"ACC-08","field":"starting conditions; actions","change":"replace","text":{"starting_conditions":"Fresh profile; `--route moonshot --model kimi-k3` (default provider name kimi); new conversation; exactly one turn, never retried.","actions_add":"Note the time the reply appears. Immediately capture the raw text of /cost, each request page and each technical page; do the arithmetic afterwards so ACC-35a (if granted) can start within 4 minutes."},"why":"5-minute cache-write lifetime; the owner's under-$0.01 limit needs a cache hit."},
{"n":6,"case":"ACC-09","field":"starting conditions; expected","change":"replace","text":{"starting_conditions":"Same profile; stop; start `--route moonshot --provider-name moonlab --model kimi-k2.7-code`; new conversation; one turn only, never retried; capture screens immediately as in ACC-08.","expected":["The first-screen line names provider moonlab and model kimi-k2.7-code, Pay per use, with an estimated $ cost, not 'no price'.","Technical page rates: noncached input 0.95, cache read 0.19, cache write 0.95, output 4 USD per million.","The ACC-08 arithmetic and completeness checks hold, whether or not cache counts are marked (reported).","Other conversations lists `kimi · kimi-k3` once, at exactly the k3 conversation's last observed total (ACC-08, or ACC-35a if run)."]},"why":"Answer 6 makes name-independence testable at no extra spend; the tester can't know whether Moonshot omitted a count."},
{"n":7,"case":"NEW ACC-35","field":"all","change":"add","text":{"id":"ACC-35","priority":"optional — run only with owner grant D3","group":"B moonshot","starting_conditions":"Run only if the run brief records the owner's grant of D3 (one second turn per Kimi model); otherwise do not run and record 'not checked live; covered by test'. Part a: the ACC-08 conversation, still open, within 4 minutes of ACC-08's reply (run before ACC-09). Part b: the ACC-09 conversation, still open, within 4 minutes of ACC-09's reply. If the window has passed, do not send; record 'not checked live (cache window missed)'.","actions":["Close /cost (Esc). Type `Reply with the single word OK again.` and wait.","/cost; open the new request(s) and their technical pages.","Repeat for part b."],"expected":["If every new request shows Cache read 0, record 'cache read not observed live' (not a FAIL); the other checks still apply.","Where cache read > 0: cache read rate 0.3 (kimi-k3) or 0.19 (kimi-k2.7-code) USD per million, and cache read cost = cache read tokens × rate exactly.","Noncached input = input − cache read − cache write; each component cost = tokens × rate; known subtotal = headline; no 'at least', 'lower bound', '+ unknown', and no component cost 'unknown'.","The first turn's request costs and IDs are unchanged and appear once; the provider name stays kimi (a) / moonlab (b)."]},"why":"Owner decision D3 requires an optional, separately labelled case; spend stays under $0.01 only on a cache hit; Moonshot requests stay well under the 12-request relay limit."},
{"n":8,"case":"ACC-10","field":"actions; expected","change":"replace","text":{"actions":["Before typing, wait 15 s, run /cost, Refresh once; record the request count P10 and each request's input tokens; Esc.","Type `Reply with the single word ALPHA.` and wait.","/cost"],"expected_add":"Record P10, the total request count N10, C1…Cn and T."},"why":"Baseline for 'a brand-new conversation still warms up'; ALPHA makes the conversation identifiable in /resume."},
{"n":9,"case":"ACC-11","field":"expected","change":"replace","text":["Within 60 s of start, without typing, a ⚠ notice appears naming skip_reopen_warmup and saying the warm-up is billed; a reopen with no warm-up, or a warm-up with no notice, is a FAIL.","The /cost first screen has a `Reopen warm-up:` line counting 1 request, with a cost.","Exactly one request link contains `Reopen warm-up`; its request page contains `Reopen warm-up`.","On its technical page each component cost = tokens × rate and the known subtotal = its cost. Observation only: the input token count.","The new total = T + the warm-up cost, counted once.","C1…Cn and their Request IDs are unchanged."],"why":"'Typically 10–15k' is not a requirement; the exact link and Kind: formats come from screens; 'only after sent' restated observably."},
{"n":10,"case":"ACC-12","field":"starting conditions; expected","change":"replace","text":{"starting_conditions":"In the ACC-11 session type `Reply with the single word OK.`, wait, run /cost and record T12, the request list and the warm-up count. Stop; start with `resume --last -c skip_reopen_warmup=true`.","expected":["The restored transcript shows the earlier turns but not ACC-11's ⚠ notice; no new notice within 60 s.","Before the new turn: warm-up count still 1, total = T12, request list unchanged.","After the turn only the turn's requests are added; none is labelled Reopen warm-up."]},"why":"The baseline was undefined; the start condition itself adds a turn."},
{"n":11,"case":"ACC-13","field":"actions; expected","change":"replace","text":{"actions":["Before typing, wait 15 s, run /cost, Refresh once; record P13; Esc.","Type `Reply with the single word BRAVO.` and wait.","/cost"],"expected":["No notice, no `Reopen warm-up` line, no link containing `Reopen warm-up`.","P13 = P10 and the request count after the turn = N10; if they differ, compare per-request input tokens, and a missing large first request that ACC-10 had is a FAIL."]},"why":"'Matches ACC-10' was not measurable; Q10 was not answered."},
{"n":12,"case":"ACC-14","field":"starting conditions; actions; expected","change":"replace","text":{"starting_conditions":"Stop; start a new conversation (no resume, skip off); type `Reply with the single word CHARLIE.` and wait.","actions":["/resume; select the conversation whose first message is `Reply with the single word ALPHA.`; Enter.","/cost"],"expected":["A ⚠ notice appears.","The Reopen warm-up line counts 2.","Earlier request costs and IDs are unchanged; each ID appears once."]},"why":"Otherwise the start conversation has no message and can't be identified or reopened in ACC-18; ALPHA makes the picker choice unambiguous."},
{"n":13,"case":"ACC-16","field":"actions","change":"replace","text":["Record the warm-up count and the request list.","Press Esc twice in the empty composer to step back to an earlier message (follow on-screen hints); if the product offers no way to edit an earlier message, record BLOCKED.","Select the most recent message you typed, replace it with `Reply with the single word EDITED.`, press Enter and wait for EDITED.","/cost"],"why":"The edit control is undocumented; two identical OK messages were ambiguous."},
{"n":14,"case":"ACC-17","field":"priority; starting conditions; actions; expected","change":"replace","text":{"priority":"blocker (reviewer part); sub-agent part advisory","starting_conditions":"Stop; start with `resume --last -c 'approvals_reviewer=\"auto_review\"' -c 'approval_policy=\"on-request\"' -c 'sandbox_mode=\"read-only\"'` (skip off). This restart is a reopen: wait for its notice, then run /cost and record the warm-up count W0 and the request list.","actions":["Type ``Run the shell command `touch warmup-review.txt` in the workspace.`` and wait for the review and the reply.","Type `Start one sub-agent that replies with the single word AGENT, then tell me its answer.` and wait.","/cost"],"expected":["No new ⚠ notice; the warm-up count is still W0.","The reviewer and sub-agent requests are each listed once; none is labelled Reopen warm-up.","No review after one retry: reviewer part BLOCKED. Sub-agent declined: not exercised."]},"why":"The optional restart changed the checked count; the answers make the reviewer reliably reachable."},
{"n":15,"case":"NEW ACC-36","field":"all","change":"add","text":{"id":"ACC-36","priority":"blocker","group":"C openai-warmup (after ACC-17, before ACC-18)","starting_conditions":"Stop. Run `agms-tui config 'skip_reopen_warmup = true'`. Note ALPHA's warm-up count W and its total.","actions":["Start with `resume --last` and no -c; confirm the conversation is ALPHA.","Wait 60 s; /cost."],"expected":["No warm-up notice.","The warm-up count is still W; the total and request list are unchanged."]},"why":"Answer 7 makes the config.toml form testable; it also gives ACC-18 its skip."},
{"n":16,"case":"ACC-18","field":"starting conditions; actions; expected","change":"replace","text":{"starting_conditions":"After ACC-36 (skip on through config.toml).","actions":["Start with `resume --last`. Open each conversation (ALPHA, BRAVO, CHARLIE and any others in /resume) with /resume; run /cost; record its own exact total, request IDs and costs, and its other-conversations count and cost."],"expected":["No notice; no warm-up count rises.","Every cost and Request ID noted in ACC-10–ACC-17 and ACC-36 is identical.","Each conversation's request costs sum to its total.","On each first screen, this conversation's total + the other-conversations cost = the sum of all own totals (within display rounding), and the other-conversations count = the sum of the others' request counts; a conversation that can't be opened is recorded as a residual, not a FAIL.","No Request ID appears in two conversations."]},"why":"Other conversations are aggregated by provider/model; a single conversation's total isn't visible from another."},
{"n":17,"case":"ACC-21","field":"expected","change":"replace","text":["A separate gpt-image-2 Pay-per-use line with 1 request.","Rates on the technical page: input 5 and output 30 USD per million; cost = input×5/M + output×30/M exactly (if cache read > 0, flag it and recompute).","Observation only: the Turn: prefix.","No rate and 0 tokens reads `none — 0 tokens`; a rate and 0 tokens keeps its $0 figure; tokens with no rate show no money.","One gpt-image-2 Request ID."],"why":"`image:` and the cache-write rate status come from screens, not the intent."},
{"n":18,"case":"ACC-23, ACC-24, ACC-25, ACC-26","field":"starting conditions; expected","change":"replace","text":["ACC-23 start, add: Run `agms-tui standin no-charge off` first.","ACC-23/25: Noncached input reads 1,500 (ACC-25: 2,000) or unknown/not reported, never another number.","ACC-23–26: Known subtotal exact USD = N × the charge.","ACC-24–26: Any published-price figure is labelled as an estimate that was not used.","ACC-23/24: the Price-basis field name is `cost` or ends in `.cost`."],"why":"The stand-ins don't report cache writes, so rule 1 allows 'unknown'; the no-charge state must be set explicitly."},
{"n":19,"case":"NEW ACC-37","field":"all","change":"add","text":{"id":"ACC-37","priority":"blocker","group":"E stand-in-charges (after ACC-29)","starting_conditions":"Record the ACC-28 other-conversations figures. Run `agms-tui standin no-charge on`.","actions":["For each of openrouter xiaomi/mimo-v2.6-pro, openrouter standin/uncatalogued, openrouter z-ai/glm-5.3-flash, vercel zai/glm-5.3-flash, corbanu corbanu/deepseek-v4.1-flash: stop, start a new conversation on it, type `Reply with the single word OK.`, run /cost, open each request and technical page.","Run `agms-tui standin no-charge off`."],"expected":["No request in these five conversations reads 'charged by' or has a reported-charge price basis.","Each request shows exactly one of: (a) a cost labelled as an estimate from published prices whose technical-page rates × tokens equal it (exact USD); (b) such an estimate marked as a lower bound ('at least …') with the missing counts named; (c) 'no price available' with no money figure and a stated reason.","standin/uncatalogued shows (c).","Tokens are as the stand-in reports (mimo 1,200/30; uncatalogued 900/15; others as in ACC-23/25/26).","No $0 figure stands in for a missing price; any total including a (b) or (c) request is marked incomplete.","The ACC-23–26 charged amounts are unchanged.","If none of the five shows (a) or (b), record 'catalogue-estimate branch not observed' as a limit, not a FAIL."]},"why":"Answer 4 makes the fallback rule testable."},
{"n":20,"case":"ACC-30, ACC-31, ACC-32, ACC-33","field":"expected","change":"replace","text":["The first-screen line and the request page contain `Covered by your subscription (not billed per request)`.","Nowhere: `Estimated cost: $`, `Cost: $`, `charged by`, or the phrase `no price available`.","Known subtotal shows no dollar amount.","Any dollar figure is labelled as what the same work would cost at API prices; 'Same work at API prices: not available' is allowed and is not 'no price'.","ACC-31: ACC-30's conversation appears under other conversations as covered. ACC-33: one gpt-image-2 request, covered."],"why":"Avoids screen-label dependence and false FAILs on 'not available'."},
{"n":21,"case":"ACC-34","field":"priority; actions; expected","change":"replace","text":{"priority":"advisory","actions":["Pattern-scan every captured screen, all exec output and the workspace files for `agms-synthetic`, `sk-`, `Bearer `, `Authorization`, `api_key`, `token=`."],"expected":["No hit; report any hit with its location (the integrator's real-key scan is binding)."]},"why":"Answer 15."},
{"n":22,"case":"NEW LIMITS","field":"named limits","change":"add","text":["fork: — no client creates one (answer 3).","Moonshot unlisted model → no price, and kimi-k2.7-code-highspeed / kimi-k2.6 rates — the route and spend limit allow only two first turns.","The 5-minute cache-write lifetime is not shown on screen.","Same-conversation /resume (Q8) — no expected result.","Label of a brand-new conversation's warm-up — only non-suppression is checked (ACC-13).","Realtime #824, DeepSeek calendar, BigModel, TensorCash, older records, Windows/Linux."],"why":"Coverage requires every untested item to be a named limit with a reason."}
]}
```