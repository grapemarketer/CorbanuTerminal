# Group C-R1 functional result (executor observations only)

## Preflight and identity

- Executor isolation preflight command run first: `agms-probe all > $AGMS_RUN/evidence/preflight-executor.json; echo rc=$?`; rc `0`; JSON says `"ok": true`.
- Coordinator preflight files present: `preflight-summary.json` says `"ok": true` (`launcher_ok`, `tmux_child_ok`, `pty_input_echoed` all true; failed empty), and both `preflight-launcher.json` and `preflight-tmux-child.json` say `"ok": true`.
- Candidate `/opt/agms/pkg/bin/corbanu` SHA-256 observed: `e942b095de975f95ca3fa1f09b9576f6bb408d91499e05235e35ab37b9d6aa42` (matches expected); `--version` was observed by preflight as `corbanu 0.1.48`.
- Packet SHA-256 observed values match: amendment R1 `c03044a22f6d4c695a749b700186c0cbb288ef7ee6426bd73577e3717bdd2e0b`; frozen design `f750b006046cb5036f92fd6f4f8d910ccd8aa33f88b33c72ffefd0f6cc01bfd6`; earlier amendments `2a73b6503429dc5a6d989b313e996a9041f5dc60b2c130e329dd39e1d71f105e`; answers `dc4044d36ac3868205ff2dc71cee36805969c6896c683852a6f9d705dc2d7c0c`; environment R1 `b7c82e3c73332c0f6d23956f6f2cc029d5f9ec118e4c99a6f82ba9c1217ba168`.

## Group timing

- Group start UTC: 2026-10-27T18:32:44Z.

## C-S1 — SET-UP: reached

- Requested: fresh profile; start new conversation route `openai`, model `gpt-6-luna`, no `-c`; wait for composer; type prompt and Enter separately; wait for `ALPHA`; capture `r1c-s1-alpha`.
- Actual: started at 2026-10-27T18:32:44Z; composer shown and captured as `r1c-s1-initial`; typed screen `r1c-s1-alpha-typed` shows exactly `Reply with the single word ALPHA.`; sent Enter at 2026-10-27T18:32:57Z; screen `r1c-s1-alpha` (about 18:33:00Z) shows bullet `• ALPHA`.
- Set-up reached: yes. No ⚠ warm-up notice observed in `r1c-s1-alpha`; an unrelated API-key notice was visible.

## ACC-11 — in progress

### ACC-11 start readings (UTC about 18:33:34–18:42:00Z)

- Requested: run `/cost` after ALPHA; read first screen, exact total, request links, every request page and technical exact money.
- Actual `/cost r1c-acc11-before-first`: `• OpenAI · GPT-6 Luna — Pay per use. 2 requests, 32,511 tokens. Estimated cost: $0.003700 (rounded).`; `No other conversation recorded requests on this day.`; no `Reopen warm-up:` line.
- Actual exact conversation total `r1c-acc11-before-details-2`: `Known subtotal exact USD: 0.0036999`.
- Request links `r1c-acc11-before-links-bottom`: Request 1 and Request 2 only; no descendant attempts link.
- Request 1 (`r1c-acc11-before-req1`): Request ID `6fe6601a-761e-4fdd-9ffd-913ec5db53db`; estimated `$0.001465 (rounded)`; no `Kind:` line observed. Technical page shows `Turn: prewarm:ad9f8cdb-726f-421b-8e63-a5ef820b96da`. Money captures: Input `14646`, exact subtotal `0.0014646`, rates 0.1 / 0.01 / 0.125 / 0.5 USD per million. Arithmetic: `14646 × 0.1 / 1,000,000 = 0.0014646`; all other components 0.
- Request 2 (`r1c-acc11-before-req2`): Request ID `57a2078d-5a97-411c-9347-513860799396`; estimated `$0.002235 (rounded)`; no `Kind:` line observed. Technical page shows `Turn: 01a17ea3-0d54-7fa1-b9f6-92784133a033`. Money captures: Input `17859`, noncached input `3`, cache write `17856`, output `6`, exact subtotal `0.0022353`, rates 0.1 / 0.01 / 0.125 / 0.5 USD per million. Arithmetic: `3 × 0.1 / 1,000,000 = 0.0000003`; `17856 × 0.125 / 1,000,000 = 0.002232`; `6 × 0.5 / 1,000,000 = 0.000003`; sum `0.0022353`.
- Before exact-cost sum check: `0.0014646 + 0.0022353 = 0.0036999`.
- Set-up checkpoint: no `Reopen warm-up` line on `r1c-acc11-before-first`; no link containing `Reopen warm-up` on `r1c-acc11-before-links-bottom`.

### ACC-11 actions after start readings

- 2026-10-27T18:42:?Z: stopped the product with `agms-tui stop` (stop control named by help). Then restarted with `resume --last`, no `-c`.
- Requested next: wait for `skip_reopen_warmup` without typing; capture notice and ALPHA transcript.

### ACC-11 after-reopen readings (UTC 18:39:40–18:47:00Z)

- Restart occurred at 2026-10-27T18:39:40Z; `r1c-acc11-notice` appeared within the wait (well under 60 s) and shows the two-line notice:
  `⚠ Reopening or forking this conversation sent a warm-up request to prime the model's cache. It is billed like any request; /cost lists it as a reopen`
  `warm-up. To skip it, set skip_reopen_warmup = true in config.toml.` It also shows `• ALPHA`.
- `/cost` read at 2026-10-27T18:39:52Z. `r1c-acc11-after-first` shows:
  `• OpenAI · GPT-6 Luna — Pay per use. 3 requests, 47,161 tokens. Estimated cost: $0.005165 (rounded).`
  `Reopen warm-up: 1 request, estimated $0.001465. Sent when this conversation was reopened or forked, to prime the model's cache.`
  `No other conversation recorded requests on this day.`
- `r1c-acc11-after-details` shows `Known subtotal exact USD: 0.0051649`.
- Links `r1c-acc11-after-links`: exactly Request 1, Request 2, Request 3 (`Reopen warm-up`), and `Root's own attempts (3 attempts)`; no descendant attempts.
- Warm-up request `r1c-acc11-warmup-req`: Request ID `ec90c7c5-ea90-4f7d-bdfd-060f32d0e325`; `Estimated cost: $0.001465`; `Kind: Reopen warm-up — sent when this conversation was reopened or forked, to prime the model's cache. Billed like any request; set skip_reopen_warmup = true in config.toml to skip it.` Technical page `r1c-acc11-warmup-tech-turn`: `Turn: prewarm:reopen:f15576fb-cbbb-49a4-b9f7-acf1e039bffa`. Money capture shows Input `14650`, subtotal `0.001465`; rates 0.1 / 0.01 / 0.125 / 0.5 USD per million. Arithmetic: `14650 × 0.1 / 1,000,000 = 0.001465`; other components 0.
- Request 1 after reading: ID `6fe6601a-761e-4fdd-9ffd-913ec5db53db`, exact subtotal `0.0014646`, unchanged.
- Request 2 after reading: ID `57a2078d-5a97-411c-9347-513860799396`, exact subtotal `0.0022353`, unchanged (money captures show Input `17859`, noncached input `3`, cache write `17856`, output `6`).
- New-total arithmetic: `T 0.0036999 + warm-up 0.001465 = 0.0051649`, equal to the after exact total.
- Request IDs after: the three above are distinct; warm-up ID is not one of the before IDs; count equals 3.

## ACC-11 outcome

1. Notice: **passed** — `r1c-acc11-notice` and `r1c-acc11-after-first`.
2. Warm-up line counts 1 with cost: **passed** — `r1c-acc11-after-first`.
3. Exactly one warm-up link and request page contains it: **passed** — `r1c-acc11-after-links`, `r1c-acc11-warmup-req`.
4. Warm-up arithmetic: **passed** — `14650 × 0.1 / 1,000,000 = 0.001465`.
5. New total exactly T + warm-up: **passed** — `0.0036999 + 0.001465 = 0.0051649`.
6. History unchanged: **passed** — both earlier IDs and exact costs found unchanged.
7. Each Request ID appears once: **passed** — three distinct IDs equal request count.
- Observation: warm-up `Turn:` prefix is `prewarm:reopen:f15576fb-cbbb-49a4-b9f7-acf1e039bffa`. No calibration discrepancy: the warm-up link's unrounded/unqualified `$0.001465` equals its exact technical subtotal.

## ACC-14 — in progress

