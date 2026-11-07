# Group C-R1 functional result

## Preflight and identity

- Preflight command: `agms-probe all > $AGMS_RUN/evidence/preflight-executor.json; echo rc=0` completed at 2026-10-27T19:05:23Z with exit code 0 and `"ok": true`.
- Coordinator summary `preflight-summary.json`: `"ok": true`; `preflight-launcher.json` and `preflight-tmux-child.json` both have `"ok": true`.
- Candidate SHA-256: `e942b095de975f95ca3fa1f09b9576f6bb408d91499e05235e35ab37b9d6aa42` (matches expected).
- Packet SHA-256s: amendment R1, frozen design, earlier amendments, answers, and re-run environment all matched the assignment.
- Candidate version observed by preflight: `corbanu 0.1.48`.
- Group UTC start: 2026-10-27T19:05:05Z.

## C-S1 — SET-UP (ACC-10 ALPHA)

- UTC start: 2026-10-27T19:06:34Z
- Requested actions: start a new `openai` / `gpt-6-luna` conversation with no `-c`; wait for composer; type ALPHA text, then Enter.
- Actual actions: started, composer shown at 2026-10-27T19:06:38Z; typed and verified; sent Enter at 2026-10-27T19:06:48Z.
- Evidence: `r1c-s1-precompose`, `r1c-s1-typed`, `r1c-s1-alpha` shows `• ALPHA` at 2026-10-27T19:07:10Z.
- Set-up reached: yes. A warning about `OPENAI_API_KEY` appeared, but no warm-up notice.

## ACC-11

- UTC start: 2026-10-27T19:07:18Z.
- Requested actions: read ALPHA `/cost`; read both requests and exact costs; stop; start `resume --last` without `-c`; wait for notice; read `/cost` and warm-up technical/money pages; re-read the two history requests.
- Actual actions: all were performed through `/cost` links and technical pages. Product stopped at 2026-10-27T19:12:44Z and restarted at 2026-10-27T19:12:47Z.
- Notice appeared immediately after start (wait elapsed 0 s). `r1c-acc11-notice` shows the full notice naming `skip_reopen_warmup` and saying “It is billed like any request.”
- Cost lines copied:
  - `r1c-acc11-before-first`: `• OpenAI · GPT-6 Luna — Pay per use. 2 requests, 32,511 tokens. Estimated cost: $0.003700 (rounded).`; `No other conversation recorded requests on this day.`
  - `r1c-acc11-before-details-2`: `Known subtotal exact USD: 0.0036999`
  - `r1c-acc11-before-links`: `→ Request 1 · OpenAI · GPT-6 Luna · estimated $0.001465 (rounded)`, `→ Request 2 · OpenAI · GPT-6 Luna · estimated $0.002235 (rounded)`
  - `r1c-acc11-after-first`: `• OpenAI · GPT-6 Luna — Pay per use. 3 requests, 47,161 tokens. Estimated cost: $0.005165 (rounded).`; `Reopen warm-up: 1 request, estimated $0.001465. Sent when this conversation was reopened or forked, to prime the model's cache.`; `No other conversation recorded requests on this day.`
  - `r1c-acc11-after-details`: `Known subtotal exact USD: 0.0051649`
  - `r1c-acc11-after-links`: links Request 1–3 and `→ Descendant attempts (none)`; Request 3 was opened as warm-up.
- Request IDs and exact costs:
  - C1 `950fc3fe-7003-4aa0-9f0d-aa0c9d3c7aca`, exact 0.0014646, `Turn: prewarm:f0fdb8bc-4580-4d88-b98e-0fa72b2270d2`.
  - C2 `06d205ef-73c9-47ff-90e6-3b15808c7d61`, exact 0.0022353. No `Turn:` line was found on its technical capture.
  - Warm-up after restart `0764d934-f5a5-40cd-b753-e8d2c9b2c7c3`, exact 0.001465, `Turn: prewarm:reopen:54268d99-ec81-401e-85e4-e9456262b93d`.
- Warm-up arithmetic: 14,650 noncached input × 0.1 USD / 1,000,000 = 0.001465. Other components are zero. Sum = 0.001465, equal to `Known subtotal exact USD: 0.001465`.
- New total arithmetic: 0.0036999 + 0.001465 = 0.0051649, matching after total exactly.
- History totals arithmetic: 0.0014646 + 0.0022353 = 0.0036999, unchanged; after IDs had the same exact costs.
- ID count arithmetic: three after IDs (C1, C2, warm-up) are distinct and equal first-screen request count 3.
- Expected-result outcomes:
  1. Notice PASS.
  2. Warm-up line PASS.
  3. Warm-up link PASS.
  4. Warm-up arithmetic PASS.
  5. New total PASS.
  6. History unchanged PASS.
  7. Each ID once PASS.

## ACC-14

- UTC start: 2026-10-27T19:17:08Z.
- Requested actions: stop; start new CHARLIE conversation; read Charlie `/cost`; `/resume` to ALPHA; read ALPHA `/cost`; read every request page.
- Actual actions: all were performed. Charlie sent at 2026-10-27T19:17:17Z and reply captured at 2026-10-27T19:17:36Z. `/resume` selected ALPHA at 2026-10-27T19:19:08Z; notice appeared immediately.
- Charlie cost lines copied:
  - `r1c-acc14-charlie-first`: `• OpenAI · GPT-6 Luna — Pay per use. 2 requests, 32,513 tokens. Estimated cost: $0.003701 (rounded).`; `Other conversations on this day, not included above: 3 requests in 1 conversation.`; `Other conversations' estimated cost: $0.005165 (rounded).`
  - `r1c-acc14-charlie-details`: `Known subtotal exact USD: 0.003700525`
  - Charlie IDs: `ae6df045-f30c-4b3d-acf7-7bf8b5b9d690`, exact 0.0014646; `5b3eaf59-1d09-45c9-8a8b-88339a85f355`, exact 0.002235925. Sum = 0.003700525.
- ALPHA after `/resume` cost lines copied:
  - `r1c-acc14-after-first`: `• OpenAI · GPT-6 Luna — Pay per use. 4 requests, 61,811 tokens. Estimated cost: $0.006630 (rounded).`; `Reopen warm-up: 2 requests, estimated $0.002930. Sent when this conversation was reopened or forked, to prime the model's cache.`; `Other conversations on this day, not included above: 2 requests in 1 conversation.`; `Other conversations' estimated cost: $0.003701 (rounded).`
  - `r1c-acc14-after-details`: `Known subtotal exact USD: 0.0066299`
  - Links: Request 1 and 2 unchanged; Request 3 `Reopen warm-up` (`0764d934...`, exact 0.001465); Request 4 `Reopen warm-up` (`72199ffe-7b0c-4e3f-867b-fe2ac5d8acdf`, exact 0.001465). The new ACC-14 warm-up is Request 4. `→ Descendant attempts (none)`.
- Total arithmetic: 0.0051649 + 0.001465 = 0.0066299, matching after total. Thus only Request 4 is new despite two warm-up-labelled links: Request 3 was ALPHA's ACC-11 warm-up.
- Other-conversation comparison (G12): rounded 0.003701 − exact 0.003700525 = 0.000000475, ≤ 0.0000005, so it matches.
- Expected-result outcomes:
  1. Notice PASS.
  2. Warm-up count PASS.
  3. History unchanged PASS (exact ALPHA history costs match; Charlie other total matches under G12).
  4. Each ID once PASS (four ALPHA IDs distinct; Charlie IDs are different; count 4).

## ACC-15

- UTC start: 2026-10-27T19:22:18Z.
- Requested actions: capture pre; `/side`; wait; send SIDE text and Enter; Ctrl+C once; wait and capture; read `/cost`; read two new requests.
- Actual actions: all were performed. `/side` opened at 2026-10-27T19:22:23Z. SIDE sent at 2026-10-27T19:22:30Z. Ctrl+C was pressed at 2026-10-27T19:22:55Z and returned to ALPHA. A 30 s wait for confirmation wording timed out, but the captured ALPHA view confirms the return.
- Visible checkpoint: `r1c-acc15-back` shows ALPHA transcript and composer at 2026-10-27T19:23:50Z.
- Cost lines copied:
  - `r1c-acc15-after-first`: `• OpenAI · GPT-6 Luna — Pay per use. 6 requests, 93,482 tokens. Estimated cost: $0.010238 (rounded).`; `Reopen warm-up: 3 requests, estimated $0.004338 (rounded). Sent when this conversation was reopened or forked, to prime the model's cache.`; `Other conversations on this day, not included above: 2 requests in 1 conversation.`; `Other conversations' estimated cost: $0.003701 (rounded).`
  - `r1c-acc15-after-details`: `Known subtotal exact USD: 0.010238475`
  - New requests: m = 6 − 4 = 2. Request 5 `16c24e22-a061-4bd8-a3b7-df7424578b5a`, `Kind: Reopen warm-up...`, exact 0.0014084, `Turn: side:prewarm:reopen:1faa4e07-19d8-4555-a263-52aed1391a19`. Request 6 `7046d8ab-a6e9-43b2-85ae-464a821bd6eb`, exact 0.002200175, `Turn: side:01a17ed0-6a0d-70e3-8ad2-b455f7ffdad9`.
- Arithmetic: 14,084 × 0.1 / 1,000,000 = 0.0014084. Side request components: 0.0000003 + 0 + 0.002197375 + 0.0000025 = 0.002200175.
- Notice placement: `r1c-acc15-pre` already shows ACC-14's notice. The same notice is present in `r1c-acc15-side-open`, `-side-reply`, and `-back`. Applying the rule, only the side-open occurrence is new; repeated identical views are not counted again.
- Other-conversations comparison: both count values are 2, conversation count 1, and rounded cost 0.003701 is unchanged.
- Expected-result outcomes:
  1. Notice shown once PASS.
  2. Warm-up count 3 PASS.
  3. Warm-up request page PASS.
  4. Side turn labelled and under ALPHA PASS.
  5. Nothing added under other conversations PASS.

## C-S2 — SET-UP (ACC-36 config step)

- UTC start: 2026-10-27T19:26:30Z.
- Requested actions: press Esc to composer, stop product, run `agms-tui config 'skip_reopen_warmup = true'`, do not start.
- Actual actions: performed; product stopped before config, and control reported `added to the profile's config.toml (top level): skip_reopen_warmup = true` at 2026-10-27T19:26:44Z.
- Set-up reached: yes.

## ACC-18

- UTC start: 2026-10-27T19:26:48Z.
- Requested actions: start `resume --last` with no `-c`, wait 60 s and capture; read each `/resume` conversation with `/cost` and every request page.
- Actual actions: performed. Start opened **CHARLIE**, not ALPHA; recorded as a deviation. It had no notice. ALPHA was then selected explicitly from `/resume`; there are no other `/resume` entries.
- `r1c-acc18-open-1` (captured 2026-10-27T19:29:24Z): CHARLIE transcript, no ⚠ warm-up notice.
- `r1c-acc18-picker-1`: two entries, CHARLIE and ALPHA.
- `r1c-acc18-open-2` (captured 2026-10-27T19:33:24Z): ALPHA transcript, no ⚠ warm-up notice.
- Cost lines copied:
  - `r1c-acc18-charlie-first`: `• OpenAI · GPT-6 Luna — Pay per use. 2 requests, 32,513 tokens. Estimated cost: $0.003701 (rounded).`; `Other conversations on this day, not included above: 6 requests in 1 conversation.`; `• OpenAI · GPT-6 Luna — Pay per use. 6 requests, 93,482 tokens. Estimated cost: $0.010238 (rounded).`; `Other conversations' estimated cost: $0.010238 (rounded).`
  - `r1c-acc18-charlie-details`: `Known subtotal exact USD: 0.003700525`
  - `r1c-acc18-alpha-first`: `• OpenAI · GPT-6 Luna — Pay per use. 6 requests, 93,482 tokens. Estimated cost: $0.010238 (rounded).`; `Reopen warm-up: 3 requests, estimated $0.004338 (rounded). Sent when this conversation was reopened or forked, to prime the model's cache.`; `Other conversations on this day, not included above: 2 requests in 1 conversation.`; `Other conversations' estimated cost: $0.003701 (rounded).`
  - `r1c-acc18-alpha-details`: `Known subtotal exact USD: 0.010238475`
- ALPHA IDs and exact costs: `950fc3fe...` 0.0014646; `06d205ef...` 0.0022353; `0764d934...` 0.001465; `72199ffe...` 0.001465; `16c24e22...` 0.0014084; `7046d8ab...` 0.002200175.
  - Sum: 0.0014646 + 0.0022353 + 0.001465 + 0.001465 + 0.0014084 + 0.002200175 = 0.010238475.
- CHARLIE IDs and exact costs: `ae6df045...` 0.0014646; `5b3eaf59...` 0.002235925.
  - Sum: 0.0014646 + 0.002235925 = 0.003700525.
- Cross-conversation totals (G12): Charlie’s other-cost rounded 0.010238 vs ALPHA exact 0.010238475; difference 0.000000475 ≤ 0.0000005. ALPHA’s other-cost rounded 0.003701 vs CHARLIE exact 0.003700525; same difference. Counts are 6 and 2.
- ID uniqueness: all six ALPHA IDs and two CHARLIE IDs are distinct; no cross-conversation duplicate.
- Expected-result outcomes:
  1. No notice and no warm-up-count rise PASS (ALPHA remains 3; CHARLIE has no warm-up line). The deviation that the first resume opened CHARLIE is recorded but is not one of the listed expected results.
  2. Every earlier cost and Request ID identical PASS.
  3. Each conversation’s request-cost sum equals its total PASS.
  4. Other-conversation totals and counts agree PASS.
  5. No Request ID appears twice PASS.

## Overall

- Deviations: `resume --last` for ACC-18 opened CHARLIE rather than the expected ALPHA. ALPHA was then opened via `/resume`; no new spend was sent.
- Ambiguities/observations: C2 in ALPHA has no `Turn:` line on its captured technical page. The `/side` return confirmation wording was not recognized by the report’s wait regex, but Ctrl+C returned to ALPHA and was captured. The cost UI navigation repeatedly cycles through all lines; every required line was captured.
- Group UTC end: 2026-10-27T19:38:39Z (product stopped).
- No model refusal or decline occurred. No credential/password prompt, unexpected profile access, missing required control, or lost observation occurred.

VERDICT ACC-11: passed
VERDICT ACC-14: passed
VERDICT ACC-15: passed
VERDICT ACC-18: passed
