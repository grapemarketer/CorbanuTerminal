The packet doesn't include enough to run four parts of the intent as written: the `fork:` label, the fallback when a provider reports no charge, renaming the Moonshot provider, and the `config.toml` form of `skip_reopen_warmup`. They're in questions 3, 4, 6 and 7 below. Everything else has a case: 34 cases in 6 groups, each group sharing one fresh product profile.

The screens come from earlier builds, so I treated them as showing the page layout, not as passing results. Two of them show behaviour I wouldn't assume is correct: questions 8 and 9.

Real spend for the whole run should be roughly $0.25:
- **Moonshot (B):** about $0.05.
- **OpenAI groups (C and D):** about $0.15–0.20 together.
- **Z.AI (A):** a few cents.
- **Stand-ins (E and F):** free.

## 1. Questions about missing intent

1. **How do you leave `/side`?** The packet doesn't say how to leave a `/side` conversation and get back to the saved one. *Assumption:* Esc (or whatever the harness documents) returns to the saved conversation, and `/cost` is run there.
2. **How is the automatic reviewer switched on?** No setting is given for sending approvals to the "auto-review" reviewer, or for an approval policy that makes a command need approval. *Assumption:* the harness supplies the `-c` keys, and a write such as `touch side-review-check.txt` with a read-only sandbox triggers an approval.
3. **What creates a `fork:` request?** The packet names no action in the text UI or in `corbanu exec` that makes a temporary fork other than `/side`. *Assumption:* it can't be exercised here, so `fork:` goes untested. Only "never labelled `side:` outside `/side`" is checked.
4. **No-charge fallback.** All three charging stand-ins always report a charge. The rule "catalogue rate as a labelled estimate, else no price" therefore can't be reached. *Assumption:* untested, unless a stand-in can be told to leave the charge out.
5. **Switching routes within one profile.** Can one profile be started on different routes, one after another? *Assumption:* yes. Groups E and F depend on this.
6. **"Whatever the person named the provider."** Only the name `kimi` is configured, and the tester can't edit the profile's config. *Assumption:* only `kimi` is tested, and name-independence is unverified.
7. **`skip_reopen_warmup` in `config.toml`.** The tester can't write the profile's config. *Assumption:* only `-c skip_reopen_warmup=true` is tested.
8. **`/resume` of the conversation already open.** `s08-resume-same-conversation` shows "Already viewing" and yet a warm-up was sent. Is that intended? *Assumption:* unknown. The tests use `/resume` to a *different* saved conversation, and same-conversation `/resume` gets no expected result.
9. **Warm-up shown while skip is on.** `s08-skip-first-screen` still shows "Reopen warm-up: 1 request" with skip on. *Assumption:* that warm-up came from an earlier reopen made without skip. The test therefore checks that the count doesn't go *up*.
10. **How is a brand-new conversation's warm-up shown?** *Assumption:* it appears as an ordinary request (on earlier screens, Request 1 costs about $0.001465). It is never labelled "Reopen warm-up" and produces no notice.
11. **How are sub-agent requests labelled?** The packet doesn't say how a sub-agent started from the main conversation is labelled. It could be `agent:`, or it could sit under "Descendant attempts" of its own thread. *Assumption:* the request is listed once in this conversation's `/cost` and is never labelled `side:` or as a reopen. The `agent:` label is checked only if the agent has no saved conversation.
12. **Rounding of sub-cent charges.** How is $0.0007125 displayed? *Assumption:* either `$0.000713 (rounded)` or `$0.0007125`, with `exact USD 0.0007125` on the technical page.
13. **Priority tier on an API key.** Is a priority-tier request on the `openai` route meant to be "no price"? The only evidence is a screen. *Assumption:* yes, so ACC-22 is advisory only.
14. **Will the model call the image tool?** Does `gpt-6-luna` reliably use the image tool when asked? *Assumption:* yes, with at most one retry.
15. **What the credential check looks for.** The tester doesn't know the real keys. *Assumption:* the check is a pattern scan (`sk-`, `Bearer `, `Authorization`, `api_key`, `token=`) plus any stand-in tokens the harness provides.

## 2. Functional cases

### Group A — `zai-s06` (one profile, route `zai`, model `glm-5.3-flash`, real; S06 attribution and basic navigation)

**ACC-01 (blocker) — navigation, empty day, basic totals**
- **Start:** a fresh profile. Start the product on `zai`.
- **Actions:**
  1. Type `/cost` and press Enter.
  2. Press Esc.
  3. Type `Reply with the single word OK.` and press Enter. Wait for `OK`.
  4. Run `/cost`, then:
     - press ↓ to `→ Request 1` and press Enter;
     - press ↓ to `→ Technical details (attempt 1)` and press Enter;
     - press Esc, Esc, Esc.
  5. Run `/cost 2026-10-01`.
- **Expected:**
  - Before any turn, `/cost` says there are no recorded requests. No `$0` figure appears.
  - After the turn:
    - the first screen lists `Z.AI · GLM 5.3 Flash` with N ≥ 1 requests and an estimated cost;
    - the request costs add up to the provider total;
    - the page titles appear in the order "Cost — this conversation" → "Request" → "Attempt, components and original price".
  - The technical page shows a `Turn:` line that does not start with `side:`, `fork:`, `agent:` or `code-review:`.
  - Each Esc goes back one level, and the last Esc closes `/cost`, returning to the composer.
  - `/cost 2026-10-01` shows no requests for that UTC day and no money.

**ACC-02 (blocker) — `/side` work is recorded under the saved conversation as `side:`**
- **Start:** the ACC-01 conversation. Note its request count R0, its total T0, each request's cost, and the "other conversations" count O0.
- **Actions:**
  1. Type `/side` and press Enter.
  2. Type `Reply with the single word SIDE.` and press Enter. Wait for `SIDE`.
  3. Leave the side conversation (see Q1).
  4. Run `/cost`. Open each new request's technical page.
- **Expected:**
  - The request count is above R0.
  - Every new request has a `Turn:` line starting with `side:`.
  - O0 is unchanged; the side conversation is not listed as another conversation.
  - Earlier requests' costs are unchanged.
  - Each Request ID appears once.

**ACC-03 (blocker) — automatic review inside `/side` is recorded once as `side:review:`**
- **Start:** the same profile, restarted with `resume --last`. Approvals are routed to the auto-review reviewer and commands need approval (Q2). Note the counts.
- **Actions:**
  1. Run `/side`.
  2. Type ``Run the shell command `touch side-review-check.txt` in the workspace.`` and press Enter.
  3. Wait for the reviewer's decision and the model's reply.
  4. Leave the side conversation and run `/cost`.
- **Expected:**
  - Exactly one new request has a `Turn:` line starting with `side:review:`. Its provider is Z.AI (the reviewer chooses its own Z.AI model).
  - The other new requests are `side:`.
  - No reviewer request appears a second time, either unprefixed or under another conversation; the other-conversations count is unchanged.
  - Whether `side-review-check.txt` exists in the workspace matches the reviewer's decision.

**ACC-04 (advisory) — a sub-agent from inside `/side` is labelled `side:agent:`**
- **Start:** the same conversation.
- **Actions:**
  1. Run `/side`.
  2. Type `Start one sub-agent that replies with the single word AGENT, then tell me its answer.`
  3. Leave the side conversation and run `/cost`.
- **Expected:**
  - If the model declines (allowed), the case is recorded as *not exercised*, not as a failure.
  - If a sub-agent runs, its requests:
    - appear once under this conversation (own or descendant attempts);
    - have a `Turn:` line starting with `side:agent:`;
    - do not appear among other conversations.

**ACC-05 (advisory) — a sub-agent outside `/side` is never labelled `side:`**
- **Start:** the same conversation, not in `/side`.
- **Actions:** type `Start one sub-agent that replies with the single word AGENT, then tell me its answer.`, then run `/cost`.
- **Expected:**
  - The sub-agent's requests are listed once.
  - None starts with `side:`.
  - If the agent has no conversation of its own, its `Turn:` line starts with `agent:` (Q11).

**ACC-06 (blocker) — `/review` is labelled `code-review:`**
- **Start:** the same conversation. Edit a tracked file in the workspace by appending one line (`// review me`), leaving it uncommitted.
- **Actions:**
  1. Type `/review` and press Enter.
  2. Choose the option to review uncommitted changes and wait for the review to finish.
  3. Run `/cost`.
- **Expected:**
  - The new request(s) have a `Turn:` line starting with `code-review:`.
  - They are listed once, under this conversation.
  - The other-conversations count is unchanged.
  - Earlier costs are unchanged.

**ACC-07 (blocker) — `exec --ephemeral` stays out of the record**
- **Start:** the same profile. Note the other-conversations count and cost in `/cost`, then stop the product.
- **Actions:**
  1. Run `corbanu exec --ephemeral "Reply with the single word OK."`.
  2. Start with `resume --last` and run `/cost`.
  3. Stop. Run `corbanu exec "Reply with the single word OK."` (a control, not ephemeral).
  4. Start with `resume --last` and run `/cost`.
- **Expected:**
  - Both `exec` runs print `OK`.
  - After the ephemeral run, the other-conversations count and cost and this conversation's totals are unchanged.
  - After the control run, there is one more other conversation, with ≥1 Z.AI request. This shows the ephemeral check isn't vacuous.

### Group B — `moonshot` (one profile, route `moonshot`, provider `kimi`; one first turn per model only)

**ACC-08 (blocker) — `kimi-k3` is priced from Moonshot's rates, and the price is complete**
- **Start:** a fresh profile and a new conversation on `kimi-k3`.
- **Actions:**
  1. Type `Reply with the single word OK.`
  2. Run `/cost`, then open each request and its technical page.
- **Expected:**
  - The first screen reads `kimi · kimi-k3 — Pay per use … Estimated cost: $…`, not "no price".
  - The technical page shows these rates (USD per million tokens):
    - noncached input 3;
    - cache read 0.3;
    - cache write 3;
    - output 15.
  - Noncached input = input − cache read − cache write.
  - Each component cost = tokens × rate, and the known subtotal = the headline cost.
  - Cache read and cache write are numbers (0 is allowed); never "unknown" or "not reported".
  - "at least", "lower bound" and "+ unknown" appear nowhere.

**ACC-09 (blocker) — `kimi-k2.7-code` writes are priced as input, and the price is complete**
- **Start:** the same profile and a new conversation on `kimi-k2.7-code`.
- **Actions:** type `Reply with the single word OK.`, run `/cost`, and open the technical pages.
- **Expected:**
  - The rates are 0.95, 0.19, 0.95 (cache write) and 4.
  - It passes the same arithmetic and completeness checks as ACC-08, including when Moonshot left out zero cache counts.
  - "Other conversations" lists the `kimi-k3` conversation once, at exactly the ACC-08 cost.

### Group C — `openai-warmup` (one profile, route `openai`, model `gpt-6-luna`, real; reopen warm-up)

**ACC-10 (blocker) — a brand-new conversation is not labelled a reopen**
- **Start:** a fresh profile and a new conversation.
- **Actions:** type `Reply with the single word OK.`, then run `/cost`.
- **Expected:**
  - No `Reopen warm-up:` line, and no request labelled "Reopen warm-up".
  - No ⚠ warm-up notice in the session.
  - Record each request's cost (C1…Cn) and the total T.

**ACC-11 (blocker) — a restart sends one labelled warm-up and shows a notice**
- **Start:** stop the product, then start it with `resume --last` (skip is off).
- **Actions:** wait for the text `skip_reopen_warmup`, then run `/cost` without typing a turn. Open the warm-up's request page and technical page.
- **Expected:**
  - **Session notice:**
    - a ⚠ notice names `skip_reopen_warmup` and says the warm-up is billed;
    - it appears only after the warm-up was sent.
  - **First screen:**
    - shows `Reopen warm-up: 1 request, estimated $…`;
    - exactly one link reads `Request k · Reopen warm-up · OpenAI · GPT-6 Luna`.
  - **Request page:** shows `Kind: Reopen warm-up`.
  - **Technical page:**
    - input is roughly 10k–15k tokens;
    - the rates times the tokens equal the cost.
  - **Totals:**
    - the new total = T + the warm-up cost, counted once inside the provider total;
    - C1…Cn are unchanged.

**ACC-12 (blocker) — the skip setting stops the warm-up on reopen, and the notice is not replayed**
- **Start:**
  1. In the ACC-11 session, type `Reply with the single word OK.`
  2. Stop the product.
  3. Start it with `resume --last -c skip_reopen_warmup=true`.
- **Actions:**
  1. Read the restored transcript.
  2. Run `/cost` before typing anything.
  3. Type `Reply with the single word OK.` and run `/cost` again.
- **Expected:**
  - The restored transcript shows the earlier `OK` turns but not the earlier ⚠ notice, and no new notice appears.
  - The `Reopen warm-up:` count is unchanged (still 1). The total is unchanged until the new turn.
  - After the turn, only the turn's requests are added.

**ACC-13 (blocker) — with skip on, a brand-new conversation still warms up**
- **Start:** stop the product, then start a new conversation (no resume) with `-c skip_reopen_warmup=true`.
- **Actions:** type `Reply with the single word OK.`, then run `/cost`.
- **Expected:**
  - The request pattern matches ACC-10, including the initial warm-up request (Q10).
  - No `Reopen warm-up` line and no notice.

**ACC-14 (blocker) — `/resume` to a different saved conversation sends a warm-up**
- **Start:** stop the product, then start a new conversation with skip off.
- **Actions:**
  1. Type `/resume`.
  2. Select the ACC-10 conversation and press Enter.
  3. Run `/cost`.
- **Expected:**
  - The notice appears.
  - `Reopen warm-up: 2 requests`.
  - All earlier request costs are unchanged, and each Request ID appears once.

**ACC-15 (blocker) — `/side` on an OpenAI conversation sends a labelled warm-up**
- **Start:** the ACC-14 conversation.
- **Actions:**
  1. Run `/side` and type `Reply with the single word SIDE.`
  2. Leave the side conversation and run `/cost`.
- **Expected:**
  - The notice is shown once.
  - The warm-up count goes up by 1.
  - That warm-up's request page says Reopen warm-up.
  - The side turn's requests have `side:` labels and sit under this conversation.
  - Nothing is listed under other conversations because of this step.

**ACC-16 (blocker) — editing an earlier message sends a warm-up**
- **Start:** the same conversation.
- **Actions:**
  1. Press Esc Esc to step back (Q-assumption).
  2. Select the last `Reply with the single word OK.` message.
  3. Replace it with `Reply with the single word EDITED.` and press Enter.
  4. Wait for `EDITED`, then run `/cost`.
- **Expected:**
  - The notice appears.
  - The warm-up count goes up by 1.
  - Requests from the abandoned branch stay listed, with costs unchanged; nothing is hidden or counted twice.

**ACC-17 (advisory) — sub-agents and the automatic reviewer with copied history are not reopens**
- **Start:** the same conversation. Optionally restart with auto-review approvals (Q2).
- **Actions:**
  1. Type `Start one sub-agent that replies with the single word AGENT, then tell me its answer.`
  2. Optionally, ask for ``Run `touch warmup-review.txt`.`` to trigger the reviewer.
  3. Run `/cost`.
- **Expected:**
  - The `Reopen warm-up` count is unchanged, and no notice appears.
  - The sub-agent's and reviewer's requests are listed once.
  - If the model declines, the step is marked *not exercised*.

**ACC-18 (blocker) — history is not re-priced, and nothing is counted twice, across the group**
- **Start:** the end of group C.
- **Actions:** open `/cost` in each conversation of the profile, using `/resume` with skip on (`-c skip_reopen_warmup=true`) to avoid new spend.
- **Expected:**
  - Every request cost noted in ACC-10 to ACC-16 is identical now.
  - In each conversation, the request costs add up to its total.
  - A conversation's total as seen from the others ("Other conversations") equals its own total.
  - No Request ID appears twice.

### Group D — `openai-pricing` (one profile, route `openai`, real)

**ACC-19 (blocker) — `gpt-6.1-sol` catalogue rates, including cache writes**
- **Start:** a fresh profile and a new conversation on `gpt-6.1-sol`.
- **Actions:** type `Reply with the single word OK.`, run `/cost`, and open the technical pages.
- **Expected:**
  - The rates are $2 input, $0.10 cached, $2.50 cache write and $10 output (per million).
  - "Rate unavailable for Cache write" does not appear.
  - Component costs recompute exactly.

**ACC-20 (blocker) — GPT-5.6 Sol promotional rates**
- **Start:** the same profile and a new conversation on `gpt-5.6-sol`.
- **Actions:** the same as ACC-19.
- **Expected:**
  - The rates are $4, $0.40, $5 and $20.
  - Costs recompute exactly.

**ACC-21 (blocker) — an image on an API key is priced at `gpt-image-2` rates**
- **Start:** the same profile and a new conversation on `gpt-6-luna`.
- **Actions:**
  1. Type `Use the image generation tool to create a small image of a plain red square, then reply DONE.`
  2. Run `/cost` and open the `gpt-image-2` request's technical page.
- **Expected:**
  - The first screen shows a separate `OpenAI · gpt-image-2 — Pay per use` line with 1 request.
  - The technical page:
    - has a `Turn:` line starting with `image:`;
    - prices at input rate 5 and output rate 30 (per million);
    - the cost = input × 5/M + output × 30/M, exactly.
  - Wording for zero-token components:
    - a component with no rate and 0 tokens reads `none — 0 tokens`;
    - a component with a rate and 0 tokens keeps its `$0.000000` figure.
  - The image request is counted once.

**ACC-22 (advisory) — no price for priority tier, with no invented money**
- **Start:** the same profile and a new conversation on `gpt-6-luna` with `-c service_tier="priority"`.
- **Actions:** type `Reply with the single word OK.`, then run `/cost` and open the technical pages.
- **Expected:**
  - If there is no published priority price:
    - the first screen shows `no price available` with the next-step text;
    - components with tokens read `unknown — rate unavailable`;
    - 0-token components read `none — 0 tokens`;
    - no `$0.000000` appears for any unpriced component;
    - `Known subtotal exact USD: none`.
  - Other conversations keep their prices.

### Group E — `stand-in-charges` (one profile; routes started one after another, a new conversation each; free)

**ACC-23 (blocker) — OpenRouter is priced from its reported charge**
- **Start:** a fresh profile, route `openrouter`, model `z-ai/glm-5.3-flash`.
- **Actions:** type `Reply with the single word OK.`, run `/cost`, and open each request and technical page.
- **Expected:**
  - Each request reads `Cost: $0.000520, charged by OpenRouter`.
  - `Price basis` names the `cost` field.
  - Tokens:
    - input 2,000;
    - cache read 500;
    - noncached 1,500;
    - output 40;
    - reasoning 12;
    - total 2,040.
  - The conversation total is N × 0.00052, exactly.
  - Any catalogue figure is labelled as an estimate that was not used.

**ACC-24 (blocker) — reasoning above output is kept as "not reported" (#786)**
- **Start:** the same profile, a new conversation on `deepseek/deepseek-v4.1-flash`.
- **Actions:** the same as ACC-23.
- **Expected:**
  - Every request is present; none is dropped.
  - Reasoning reads not reported or unknown (neither 60 nor 0).
  - Input is 1,000 and output is 40.
  - `Cost: $0.000310, charged by OpenRouter`, and the total is N × 0.00031.

**ACC-25 (blocker) — Vercel is priced from `gatewayCost`**
- **Start:** the same profile, route `vercel`, model `zai/glm-5.3-flash`.
- **Actions:** the same as ACC-23.
- **Expected:**
  - Each request shows a charge of $0.0007125 "charged by Vercel" (Q12).
  - The price basis names `gatewayCost`.
  - Tokens:
    - input 3,000;
    - cache read 1,000;
    - noncached 2,000;
    - output 25;
    - total 3,025.
  - The total is N × 0.0007125.

**ACC-26 (blocker) — Corbanu API is priced from `corbanu_charge_usd`**
- **Start:** the same profile, route `corbanu`, model `corbanu/deepseek-v4.1-flash`.
- **Actions:** the same as ACC-23.
- **Expected:**
  - Each request reads `Cost: $0.001850, charged by Corbanu API`.
  - The price basis names `usage.corbanu_charge_usd`.
  - Input 1,500, output 30, total 1,530.
  - The total is N × 0.00185.

**ACC-27 (blocker) — a configured pay-per-use provider with no price: wording**
- **Start:** the same profile, route `custom` (`mylab`), model `lab-model-1`.
- **Actions:** the same as ACC-23.
- **Expected:**
  - The first screen shows `no price available` and the next step (check mylab's bill).
  - The technical page reads:
    - `Billing basis: pay per use (set in your config)`;
    - `Price: none recorded — this request records only its billing basis: pay per use; set in your config`, with no nested parentheses.
  - Input is 800 and output is 20.
  - Every cost component reads unknown or `none — 0 tokens`; no `$` figure appears for any component.
  - `Known subtotal exact USD: none`.

**ACC-28 (blocker) — several charging providers on one screen**
- **Start:** the ACC-27 conversation.
- **Actions:** read the `/cost` first screen.
- **Expected:**
  - "Other conversations" shows 4 conversations, with each provider and model listed and its own "charged by OpenRouter / Vercel / Corbanu API".
  - Explanatory lines name each charging provider.
  - The other-conversations cost equals the sum of the charges from ACC-23 to ACC-26.
  - mylab is never counted as $0; any total that includes it is marked as incomplete.

**ACC-29 (advisory) — no warm-up on a non-WebSocket reopen**
- **Start:** stop the product, then start it with `resume --last` (custom route).
- **Actions:** run `/cost` before typing anything.
- **Expected:**
  - No notice and no `Reopen warm-up` line.
  - The totals are identical to before.

### Group F — `chatgpt` (one profile, route `chatgpt`, stand-in sign-in; free)

**ACC-30 (blocker) — catalogued `gpt-5.4` is covered by the subscription**
- **Start:** a fresh profile and a new conversation on `gpt-5.4`.
- **Actions:** type `Reply with the single word OK.`, run `/cost`, and open the request and technical pages.
- **Expected:**
  - Every level reads `Covered by your subscription (not billed per request)`.
  - No `Estimated cost: $`, `Cost: $` or `no price available` appears.
  - The known subtotal reads `none — subscription work…`.
  - Any API-price figure is labelled "Same work at API prices", not as cost.

**ACC-31 (blocker) — uncatalogued `gpt-mock-unlisted` is covered by the subscription**
- **Start:** a new conversation on `gpt-mock-unlisted`.
- **Actions:** the same as ACC-30.
- **Expected:**
  - The same as ACC-30.
  - The ACC-30 conversation appears under other conversations as covered, never as spend.

**ACC-32 (blocker) — priority tier is covered by the subscription**
- **Start:** a new conversation on `gpt-5.4` with `-c service_tier="priority"`.
- **Actions:** the same as ACC-30.
- **Expected:** the same as ACC-30, with no "no price".

**ACC-33 (blocker) — a ChatGPT image is covered by the subscription**
- **Start:** a new conversation on `gpt-5.4`. Set the stand-in's next answer to use the image tool.
- **Actions:** type `Make an image of a red square.`, then run `/cost`.
- **Expected:**
  - A line reads `OpenAI · gpt-image-2 — Covered by your subscription (not billed per request)` with 1 request.
  - No spend and no "no price".
  - The image request is counted once.

### Across all groups

**ACC-34 (blocker) — no credential is printed**
- **Start:** all screens captured in groups A–F, plus `exec` output.
- **Actions:** pattern-scan them (Q15).
- **Expected:** no API key, bearer token, sign-in token or Authorization header text appears anywhere.

## 3. JSON

```json
{"questions":[
"How is /side exited back to the saved conversation? Assumption: Esc (or harness-documented key) returns to the parent, where /cost is run.",
"Which -c settings route approvals to the auto-review reviewer and make a command require approval? Assumption: harness supplies them; a write like `touch side-review-check.txt` under a read-only sandbox triggers one.",
"What action in the text UI or exec creates a non-/side temporary fork (labelled fork:)? Assumption: none available here; fork: stays untested, only 'never side: outside /side' is checked.",
"Can any stand-in omit the reported charge so the 'catalogue estimate, else no price' fallback can be exercised? Assumption: no; fallback untested.",
"Can one profile be started on different routes in sequence? Assumption: yes (groups E and F rely on it).",
"Can the Moonshot provider be configured under another name to test 'whatever the person named it'? Assumption: no; only `kimi` is tested.",
"Can the tester write config.toml in the profile? Assumption: no; only -c skip_reopen_warmup=true is tested.",
"Is /resume of the already-open conversation meant to send a warm-up (s08-resume-same-conversation shows one with 'Already viewing')? Assumption: unspecified; tests use /resume to a different conversation.",
"s08-skip-first-screen shows a reopen warm-up with skip on: was it from an earlier unskipped reopen? Assumption: yes; tests check the count does not increase.",
"How does a brand-new conversation's warm-up appear in /cost? Assumption: an ordinary request, never labelled Reopen warm-up, no notice.",
"How are sub-agents from the main conversation labelled (agent: vs descendant of own thread)? Assumption: listed once, never side: or reopen; agent: only if no conversation of their own.",
"How are sub-cent charges like 0.0007125 shown? Assumption: $0.000713 (rounded) or $0.0007125, exact USD 0.0007125 on the technical page.",
"Is priority tier on an OpenAI API key meant to be 'no price'? Assumption: yes per screen evidence only; advisory.",
"Will gpt-6-luna reliably call the image tool when asked? Assumption: yes, at most one retry.",
"What credential patterns should the leak scan use? Assumption: sk-, Bearer, Authorization, api_key, token= plus any harness-supplied stand-in tokens."
],
"cases":[
{"id":"ACC-01","priority":"blocker","group":"A zai-s06","starting_conditions":"Fresh profile; route zai; model glm-5.3-flash; new conversation.","actions":["Type `/cost`, Enter; Esc","Type `Reply with the single word OK.` Enter; wait for OK","`/cost`; ↓ to `→ Request 1`, Enter; ↓ to `→ Technical details (attempt 1)`, Enter; Esc, Esc, Esc","`/cost 2026-10-01`"],"expected":["Before any turn: no recorded requests, no $0","First screen lists Z.AI · GLM 5.3 Flash, N≥1 requests, estimate; request costs sum to provider total","Pages in order Cost — this conversation → Request → Attempt, components and original price","Turn: has no side:/fork:/agent:/code-review: prefix","Each Esc goes back one level; last closes to composer","2026-10-01 shows no requests and no money"]},
{"id":"ACC-02","priority":"blocker","group":"A zai-s06","starting_conditions":"ACC-01 conversation; note request count R0, total, per-request costs, other-conversations count O0.","actions":["`/side` Enter","`Reply with the single word SIDE.` Enter; wait for SIDE","Leave side","`/cost`; open each new request's technical page"],"expected":["Request count > R0","New requests' Turn: starts with side:","O0 unchanged; side not a separate conversation","Earlier costs unchanged; each Request ID once"]},
{"id":"ACC-03","priority":"blocker","group":"A zai-s06","starting_conditions":"Same profile, restarted with resume --last, approvals routed to auto-review and commands needing approval (see questions); counts noted.","actions":["`/side`","``Run the shell command `touch side-review-check.txt` in the workspace.`` Enter","Wait for reviewer decision and reply; leave side","`/cost`; open new technical pages"],"expected":["Exactly one new request with Turn: side:review:, provider Z.AI","Other new requests are side:","No reviewer request repeated, unprefixed or in other conversations; other-conversations count unchanged","Workspace file presence matches reviewer decision"]},
{"id":"ACC-04","priority":"advisory","group":"A zai-s06","starting_conditions":"Same conversation.","actions":["`/side`","`Start one sub-agent that replies with the single word AGENT, then tell me its answer.`","Leave side; `/cost`"],"expected":["If model declines: not exercised (not a failure)","If a sub-agent ran: its requests are listed once under this conversation with Turn: side:agent:, none in other conversations"]},
{"id":"ACC-05","priority":"advisory","group":"A zai-s06","starting_conditions":"Same conversation, not in /side.","actions":["`Start one sub-agent that replies with the single word AGENT, then tell me its answer.`","`/cost`"],"expected":["Sub-agent requests listed once","None starts with side:","Turn: starts with agent: if the sub-agent has no conversation of its own"]},
{"id":"ACC-06","priority":"blocker","group":"A zai-s06","starting_conditions":"Same conversation; append `// review me` to a tracked workspace file, uncommitted.","actions":["`/review` Enter; choose uncommitted changes","Wait for review","`/cost`"],"expected":["New requests' Turn: starts with code-review:","Listed once under this conversation","Other-conversations count and earlier costs unchanged"]},
{"id":"ACC-07","priority":"blocker","group":"A zai-s06","starting_conditions":"Same profile; note other-conversations count and cost from /cost; stop product.","actions":["`corbanu exec --ephemeral \"Reply with the single word OK.\"`","Start with resume --last; `/cost`","Stop; `corbanu exec \"Reply with the single word OK.\"`","Start with resume --last; `/cost`"],"expected":["Both exec runs print OK","After ephemeral: other-conversations count/cost and this conversation unchanged","After the non-ephemeral control: one more other conversation with ≥1 Z.AI request"]},
{"id":"ACC-08","priority":"blocker","group":"B moonshot","starting_conditions":"Fresh profile; route moonshot (provider kimi); model kimi-k3; new conversation; one turn only.","actions":["`Reply with the single word OK.`","`/cost`; open each request and technical page"],"expected":["`kimi · kimi-k3 — Pay per use … Estimated cost: $…`, not no price","Rates 3 / 0.3 / 3 (cache write) / 15 per M","Noncached = input − cache read − cache write","Component cost = tokens × rate; known subtotal = headline","Cache read/write numeric (0 allowed), never unknown/not reported","No 'at least', 'lower bound' or '+ unknown'"]},
{"id":"ACC-09","priority":"blocker","group":"B moonshot","starting_conditions":"Same profile; new conversation on kimi-k2.7-code; one turn only.","actions":["`Reply with the single word OK.`","`/cost`; open technical pages"],"expected":["Rates 0.95 / 0.19 / 0.95 (write priced as input) / 4","Same arithmetic and completeness checks as ACC-08, even if Moonshot left out zero cache counts","Other conversations lists the kimi-k3 conversation once at exactly the ACC-08 cost"]},
{"id":"ACC-10","priority":"blocker","group":"C openai-warmup","starting_conditions":"Fresh profile; route openai; model gpt-6-luna; new conversation.","actions":["`Reply with the single word OK.`","`/cost`"],"expected":["No `Reopen warm-up:` line; no request labelled Reopen warm-up","No warm-up notice in session","Record per-request costs C1..Cn and total T"]},
{"id":"ACC-11","priority":"blocker","group":"C openai-warmup","starting_conditions":"Stop; start with resume --last, skip off.","actions":["Wait for text `skip_reopen_warmup`","`/cost` without typing a turn","Open the warm-up's request page and technical page"],"expected":["⚠ notice names skip_reopen_warmup and says the warm-up is billed","`Reopen warm-up: 1 request, estimated $…`","Exactly one link `Request k · Reopen warm-up · OpenAI · GPT-6 Luna`","Request page shows `Kind: Reopen warm-up`","Technical page: input roughly 10k–15k tokens; rates × tokens = cost","New total = T + warm-up cost, counted once inside the provider total","C1..Cn unchanged"]},
{"id":"ACC-12","priority":"blocker","group":"C openai-warmup","starting_conditions":"In the ACC-11 session type `Reply with the single word OK.`; stop; start with `resume --last -c skip_reopen_warmup=true`.","actions":["Read the restored transcript","`/cost` before typing","`Reply with the single word OK.`; `/cost`"],"expected":["Restored transcript shows earlier OK turns but not the earlier ⚠ notice; no new notice","Reopen warm-up count still 1; total unchanged before the turn","After the turn, only the turn's requests are added"]},
{"id":"ACC-13","priority":"blocker","group":"C openai-warmup","starting_conditions":"Stop; start a new conversation (no resume) with -c skip_reopen_warmup=true.","actions":["`Reply with the single word OK.`","`/cost`"],"expected":["Request pattern matches ACC-10, including the initial warm-up request","No Reopen warm-up line; no notice"]},
{"id":"ACC-14","priority":"blocker","group":"C openai-warmup","starting_conditions":"Stop; start a new conversation, skip off.","actions":["`/resume`; select the ACC-10 conversation; Enter","`/cost`"],"expected":["Notice appears","`Reopen warm-up: 2 requests`","Earlier request costs unchanged; each Request ID once"]},
{"id":"ACC-15","priority":"blocker","group":"C openai-warmup","starting_conditions":"ACC-14 conversation.","actions":["`/side`; `Reply with the single word SIDE.`","Leave side; `/cost`"],"expected":["Notice shown once","Warm-up count +1; its request page says Reopen warm-up","Side turn requests labelled side:, under this conversation","Nothing added to other conversations by this step"]},
{"id":"ACC-16","priority":"blocker","group":"C openai-warmup","starting_conditions":"Same conversation.","actions":["Esc Esc to step back; select the last `Reply with the single word OK.` message","Replace with `Reply with the single word EDITED.` Enter; wait for EDITED","`/cost`"],"expected":["Notice appears","Warm-up count +1","Abandoned-branch requests still listed with costs unchanged; nothing hidden or counted twice"]},
{"id":"ACC-17","priority":"advisory","group":"C openai-warmup","starting_conditions":"Same conversation; optionally restarted with auto-review approvals.","actions":["`Start one sub-agent that replies with the single word AGENT, then tell me its answer.`","Optionally ``Run `touch warmup-review.txt`.`` to trigger the reviewer","`/cost`"],"expected":["Reopen warm-up count unchanged; no notice","Sub-agent and reviewer requests listed once","If the model declines: not exercised"]},
{"id":"ACC-18","priority":"blocker","group":"C openai-warmup","starting_conditions":"End of group C.","actions":["Open `/cost` in each conversation of the profile via /resume with -c skip_reopen_warmup=true (no new spend)"],"expected":["Every request cost noted in ACC-10 to ACC-16 is identical","Each conversation's request costs sum to its total","A conversation's total seen from others equals its own total","No Request ID appears twice"]},
{"id":"ACC-19","priority":"blocker","group":"D openai-pricing","starting_conditions":"Fresh profile; route openai; model gpt-6.1-sol; new conversation.","actions":["`Reply with the single word OK.`","`/cost`; open technical pages"],"expected":["Rates $2 input / $0.10 cached / $2.50 cache write / $10 output per M","No 'Rate unavailable for Cache write'","Component costs recompute exactly"]},
{"id":"ACC-20","priority":"blocker","group":"D openai-pricing","starting_conditions":"Same profile; new conversation on gpt-5.6-sol.","actions":["`Reply with the single word OK.`","`/cost`; open technical pages"],"expected":["Rates $4 / $0.40 / $5 / $20 per M","Costs recompute exactly"]},
{"id":"ACC-21","priority":"blocker","group":"D openai-pricing","starting_conditions":"Same profile; new conversation on gpt-6-luna.","actions":["`Use the image generation tool to create a small image of a plain red square, then reply DONE.`","`/cost`; open the gpt-image-2 request's technical page"],"expected":["Separate `OpenAI · gpt-image-2 — Pay per use` line, 1 request","Turn: starts with image:; input rate 5, output rate 30 per M","Cost = input×5/M + output×30/M exactly","Component with no rate and 0 tokens reads `none — 0 tokens`; component with a rate and 0 tokens keeps `$0.000000`","Image request counted once"]},
{"id":"ACC-22","priority":"advisory","group":"D openai-pricing","starting_conditions":"Same profile; new conversation on gpt-6-luna with -c service_tier=\"priority\".","actions":["`Reply with the single word OK.`","`/cost`; open technical pages"],"expected":["If no published priority price: `no price available` plus next-step text","Components with tokens read `unknown — rate unavailable`; 0-token components read `none — 0 tokens`","No `$0.000000` on any unpriced component; `Known subtotal exact USD: none`","Other conversations keep their prices"]},
{"id":"ACC-23","priority":"blocker","group":"E stand-in-charges","starting_conditions":"Fresh profile; route openrouter; model z-ai/glm-5.3-flash; new conversation.","actions":["`Reply with the single word OK.`","`/cost`; open each request and technical page"],"expected":["Each request `Cost: $0.000520, charged by OpenRouter`","Price basis names `cost`","Tokens: input 2,000; cache read 500; noncached 1,500; output 40; reasoning 12; total 2,040","Conversation total = N × 0.00052 exactly","Any catalogue figure labelled as an estimate not used"]},
{"id":"ACC-24","priority":"blocker","group":"E stand-in-charges","starting_conditions":"Same profile; route openrouter; new conversation on deepseek/deepseek-v4.1-flash.","actions":["`Reply with the single word OK.`","`/cost`; open each request and technical page"],"expected":["Every request present, none dropped","Reasoning not reported/unknown (neither 60 nor 0)","Input 1,000; output 40","`Cost: $0.000310, charged by OpenRouter`; total = N × 0.00031"]},
{"id":"ACC-25","priority":"blocker","group":"E stand-in-charges","starting_conditions":"Same profile; route vercel; model zai/glm-5.3-flash; new conversation.","actions":["`Reply with the single word OK.`","`/cost`; open each request and technical page"],"expected":["Each request charge $0.0007125 'charged by Vercel'","Price basis names gatewayCost","Tokens: input 3,000; cache read 1,000; noncached 2,000; output 25; total 3,025","Total = N × 0.0007125"]},
{"id":"ACC-26","priority":"blocker","group":"E stand-in-charges","starting_conditions":"Same profile; route corbanu; model corbanu/deepseek-v4.1-flash; new conversation.","actions":["`Reply with the single word OK.`","`/cost`; open each request and technical page"],"expected":["Each request `Cost: $0.001850, charged by Corbanu API`","Price basis names usage.corbanu_charge_usd","Input 1,500; output 30; total 1,530","Total = N × 0.00185"]},
{"id":"ACC-27","priority":"blocker","group":"E stand-in-charges","starting_conditions":"Same profile; route custom (mylab); model lab-model-1; new conversation.","actions":["`Reply with the single word OK.`","`/cost`; open each request and technical page"],"expected":["First screen: `no price available` and next step to check mylab's bill","`Billing basis: pay per use (set in your config)`","`Price: none recorded — this request records only its billing basis: pay per use; set in your config` — no nested parentheses","Input 800; output 20","Every cost component reads unknown or `none — 0 tokens`; no $ figure on any component","`Known subtotal exact USD: none`"]},
{"id":"ACC-28","priority":"blocker","group":"E stand-in-charges","starting_conditions":"ACC-27 conversation.","actions":["Read the `/cost` first screen"],"expected":["Other conversations: 4 conversations, each provider/model with its own 'charged by OpenRouter / Vercel / Corbanu API'","Explanatory lines name each charging provider","Other-conversations cost = sum of the ACC-23 to ACC-26 charges","mylab never counted as $0; any total including it is marked incomplete"]},
{"id":"ACC-29","priority":"advisory","group":"E stand-in-charges","starting_conditions":"Stop; start with resume --last (custom route).","actions":["`/cost` before typing"],"expected":["No notice; no Reopen warm-up line","Totals identical to before"]},
{"id":"ACC-30","priority":"blocker","group":"F chatgpt","starting_conditions":"Fresh profile; route chatgpt (stand-in sign-in); model gpt-5.4; new conversation.","actions":["`Reply with the single word OK.`","`/cost`; open request and technical pages"],"expected":["Every level reads `Covered by your subscription (not billed per request)`","No `Estimated cost: $`, `Cost: $` or `no price available`","Known subtotal reads `none — subscription work…`","Any API-price figure labelled 'Same work at API prices', not as cost"]},
{"id":"ACC-31","priority":"blocker","group":"F chatgpt","starting_conditions":"Same profile; new conversation on gpt-mock-unlisted.","actions":["`Reply with the single word OK.`","`/cost`; open request and technical pages"],"expected":["Same as ACC-30","ACC-30 conversation appears under other conversations as covered, never as spend"]},
{"id":"ACC-32","priority":"blocker","group":"F chatgpt","starting_conditions":"Same profile; new conversation on gpt-5.4 with -c service_tier=\"priority\".","actions":["`Reply with the single word OK.`","`/cost`; open request and technical pages"],"expected":["Same as ACC-30, with no 'no price'"]},
{"id":"ACC-33","priority":"blocker","group":"F chatgpt","starting_conditions":"Same profile; new conversation on gpt-5.4; stand-in set so its next answer uses the image tool.","actions":["`Make an image of a red square.`","`/cost`"],"expected":["`OpenAI · gpt-image-2 — Covered by your subscription (not billed per request)`, 1 request","No spend; no 'no price'","Image request counted once"]},
{"id":"ACC-34","priority":"blocker","group":"cross-cutting","starting_conditions":"All screens captured in groups A–F, plus exec output.","actions":["Pattern-scan for sk-, Bearer, Authorization, api_key, token= and any harness-supplied stand-in tokens"],"expected":["No API key, bearer token, sign-in token or Authorization header text appears anywhere"]}
]}
```