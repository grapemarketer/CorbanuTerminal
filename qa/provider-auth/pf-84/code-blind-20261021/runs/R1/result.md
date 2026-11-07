# R1 amendment-v2 re-run — independent functional executor report

Actual outcome only; not human acceptance.

## Preflight

- UTC: 2026-10-22T01:18:24Z for executor `agms-probe all`.
- Executor preflight rc: 0. Summary `"ok": true`, `"failed": []`, probe_count 97.
- Coordinator preflight-summary and both preflight files beside it also reported `"ok": true`.
- Candidate SHA-256 matched: `bbae7d37dff9023268efdd05a8a4408ccf144ee51ba911afd39adc0e9ddce1b8`.
- Frozen cases SHA-256 matched: `cfad42f543b6782a70b935b0596ac50737468733f3480629f49089e34eb6e5b4`.
- Amendment v2 SHA-256 matched: `fe1324914155fc4d37a83a4ac3bd7bdd0ac67f636cdf31e1da0cfb7c6a0f7e77`.
- Prepared home listed: `zai3`.

## NA-01

### Actions: requested versus actual

- Home `zai3`; all ten requested actions were executed in order. No product or home changes beyond those requested.
- Window mapping: step 1 `w1`; step 2 `w2`; step 3 `w3`; step 4 `w4`; step 5 `w5`; step 6 `w6`; step 7 `w7`; step 8 `w8`; step 9 `w9`; step 10 global stop.
- Step 1: requested `CLI(zai3, account list)` → actual `agms-tui --window w1 start --home zai3 account list`; exited 0 at 01:20:32Z.
- Step 2: requested `CLIS(zai3, account add zai extra, synthetic zextra key)` → actual `agms-tui --window w2 start --home zai3 --stdin <synthetic key> account add zai extra`; exited 0 at 01:20:42Z. Key was piped, never typed with `text`.
- Step 3: requested `CLI(zai3, account add zai extra2 <literal synthetic zextratwo key from case>)` → actual same command; exited 2 at 01:20:54Z.
- Step 4: requested list → exited 0 at 01:21:04Z.
- Step 5: requested login status → exited 1 at 01:21:07Z.
- Step 6: requested exec on `extra` with prompt for `xone`; actual command run; exited 0 at 01:21:31Z.
- Step 7: requested remove `extra` → exited 0 at 01:21:44Z.
- Step 8: requested list → exited 0 at 01:21:46Z.
- Step 9: requested exec on removed `extra` with prompt for `xtwo`; actual command run; exited 1 at 01:21:55Z.
- Step 10: `agms-tui stop` at 01:22:08Z.

### Positive visible checkpoints

- 01:20:32Z (`na01-1-list`): `account list` showed `zai personal api_key` and `zai work api_key`; no `default` row; rows showed kind but no fingerprint.
- 01:20:42Z (`na01-2-add-extra`): `Saved zai account `extra` (api_key).`
- 01:20:54Z (`na01-3-add-extra2`): nonzero rejection. Error repeated the command-line key text: `error: unexpected argument '<literal synthetic zextratwo key from case>' found` (observe; the key is from the requested literal command).
- 01:21:04Z (`na01-4-list-extra`): list showed `extra`, `personal`, `work`; no `extra2`.
- 01:21:07Z (`na01-5-login-status`): output `Not logged in`; exit 1; no `agms-fake`.
- 01:21:31Z (`na01-6-xone`): exec output contained `corbanu` then `xone`.
- 01:21:44Z (`na01-7-remove-extra`): `Removed zai account `extra`.`
- 01:21:46Z (`na01-8-list-after-remove`): no `extra`; `personal` and `work` remained.
- 01:21:55Z (`na01-9-final`): error named `extra` and `zai`; gave `corbanu account add zai extra`; mentioned `--account`. No `xtwo` reply appeared.
- NO-SECRET(zextra,58213): no captured screen shows `agms-fake`, `zextra`, or `58213`. Step 3's screen does show the separately requested command-line key for `zextratwo`, which is an observed consequence of the literal case, not the zextra secret.

### Model refusals/declines

- None observed.

### Expected results

- Step 1 exits 0 and lists `work` and `personal`: **passed**.
- `[observe]` default listed: **observed** — not listed.
- `[observe]` kind/fingerprint: **observed** — kind `api_key` shown; fingerprint not shown.
- Step 2 exits 0 and confirms `extra`: **passed**.
- Step 3 exits nonzero and step 4 has no `extra2`: **passed**.
- `[observe]` step 3 error repeats key from command line: **observed** — yes.
- Step 4 lists `extra`: **passed**.
- Step 5 shows no `agms-fake`: **passed**.
- `[observe]` step 5 wording and exit code: **observed** — `Not logged in`; exit 1.
- Step 6 exits 0 and output contains `xone`: **passed**.
- Step 7 exits 0 and step 8 has no `extra`: **passed**.
- Step 9 exits nonzero, no reply, names account/provider, gives recovery, mentions `--account`: **passed**.
- NO-SECRET(zextra,58213): **passed** in captured screens.
- Relay: `xone` → `zextra`; nothing at `xtwo`; no request ever carries `zextratwo`: **for the integrator** (logged 01:21:19–01:21:31Z for xone and 01:21:53–01:21:55Z for xtwo).

VERDICT NA-01: passed

## NA-04

### Actions: requested versus actual

- Home `zai3` after NA-01. All five requested actions were executed in order.
- Window mapping: step 1 `w10`; step 2 `w11`; steps 3–5 START window `w12`.
- Step 1: requested `CLIS(zai3, account add zai badone, synthetic badkey)` → actual `agms-tui --window w10 start --home zai3 --stdin <synthetic badkey> account add zai badone`; exited 0 at 01:22:45Z. Key piped, never typed.
- Step 2: requested `CLI(zai3, exec --account badone "reply with the single word bone")` → actual same command; exited 1 at 01:22:55Z.
- Step 3: requested `START(zai3, --account badone)` → actual `agms-tui --window w12 start --home zai3 --account badone`; composer appeared at 01:23:07Z.
- Step 4: requested `SEND(btwo, 60)` → prompt text and Enter were sent separately at 01:23:19Z. A 30-second screen check occurred at 01:23:49Z; no prompt was shown. Final screen at 01:24:19Z (60 seconds after send).
- Step 5: requested `STATUS(n4-a)`, `QUIT`, `cat CFG(zai3)`, then `agms-tui stop`. Status screen captured at 01:24:27Z; QUIT completed at 01:24:34Z; config cat at 01:24:44Z; stop at 01:24:44Z.

### Positive visible checkpoints

- 01:22:45Z (`na04-1-add-badone`): `Saved zai account `badone` (api_key).`
- 01:22:55Z (`na04-2-final`): CLI exit 1, no `bone` reply, errors included:
  - `ERROR: Z.AI (zai) account `badone` was rejected. Replace its key with `corbanu account add zai badone` (reads stdin), or choose another account. The default Z.AI credential is unchanged.`
  - `ERROR: unexpected status 401 Unauthorized: Authorization Failure (test relay), url: https://api.z.ai/api/paas/v4/chat/completions`
- 01:23:07Z (`na04-3-start`): TUI session reached the composer. Footer showed `Z.AI GLM 5.3 Flash via zai default`.
- 01:23:49Z and 01:24:19Z (`na04-4-check30`, `na04-4-btwo`): warning named `badone`, recovery `corbanu account add zai badone`; no `• btwo` appeared; no onboarding wording.
- 01:24:27Z (`na04-5-n4-a`): complete status box; `Model provider:` row ended `· account badone`; `Account usage:` showed `badone · no usage yet`.
- 01:24:34Z (`na04-5-quit`): exit code 0; terminal settings left `icanon isig echo icrnl opost`.
- 01:24:44Z (`na04-config-after.txt`): CFG(zai3) still had `[provider_accounts]` and `zai = "personal"`.

### Model refusals/declines

- None. The visible transport/API errors were authorization errors, not model-text declines.

### Expected results

- Step 1 exits 0 (precondition): **passed**.
- Step 2 exits nonzero, no `bone` reply, names `badone`, gives recovery `corbanu account add zai badone`: **passed**.
- Step 4 window shows error naming `badone`; no `• btwo`; NO-ONBOARD: **passed**.
- n4-a shows `· account badone`: **passed**.
- NO-SECRET(badkey,77302): **passed** in captured screens.
- Relay: every request at `bone` and `btwo` carries badkey/401; none carries zenv/zwork/zpersonal and none keyless: **for the integrator** (bone logged 01:22:53–01:22:55Z; btwo sent 01:23:19–01:24:19Z).
- Files: CFG(zai3) still `zai = "personal"`: **passed**.

VERDICT NA-04: passed

## NA-10

### Actions: requested versus actual

- Home `zai3` after NA-04 (`work`, `personal`, `badone`). All three requested action groups were executed in order.
- Window mapping: START window `w13`.
- Step 1: START completed at 01:25:17Z; STATUS2(s0) completed at 01:25:36Z; ASK(sone) sent at 01:25:44Z and completed at 01:25:50Z; STATUS(s1) captured at 01:26:00Z.
- Step 2: `/providers` opened at 01:26:14Z. Before selecting, SURVEY was run on the provider list because it scrolled and initially hid `Z.AI · work`. `ROW(Z.AI, work)` highlighted at 01:27:53Z; actions opened at 01:28:02Z; `Use for this session` selected at 01:28:11Z; Escape returned to a composer at 01:28:14Z. ASK(stwo) sent at 01:28:23Z and completed at 01:28:29Z; STATUS(s2) captured at 01:28:38Z.
- Step 3: QUIT completed at 01:28:48Z; `agms-tui stop` at 01:28:48Z.

### Positive visible checkpoints

- 01:25:36Z (`na10-1-s0-2`): complete status box; provider row ended `· account personal`; `Account usage: personal · no usage yet`.
- 01:25:50Z (`na10-1-sone`): visible `• sone`.
- 01:26:00Z (`na10-1-s1`): complete status box; `Account usage: personal · 8,595 input · 15 output (11 reasoning)`.
- 01:27:53Z (`na10-2-rowwork-3`): provider list highlighted `Z.AI · work` and showed `Named account · API key · fp 8b6221c3f139`.
- 01:28:02Z (`na10-2-work-actions`): actions list offered `Use for this session`, `Make default`, `Rename`, `Remove`.
- 01:28:11Z (`na10-2-use-selected`): message `• New session: requests to zai use account `work`.` The displayed session ID changed from `01a16136-568c-7c61-bcbf-bc5b700b6c69` to a later status session `01a16139-0703-7342-9ea6-9e37a10153d0`.
- 01:28:29Z (`na10-2-stwo`): visible `• stwo`.
- 01:28:38Z (`na10-2-s2`): complete status box; provider row ended `· account work`; `Account usage: work · 19 input · 8,576 cached input · 13 output (10 reasoning)`. It was a single work row; neither an `(active)` marker nor a `personal` row appeared.
- 01:28:48Z (`na10-3-quit`): exit code 0; terminal settings left `icanon isig echo icrnl opost`.

### Survey note

- Provider list SURVEY: complete. It scrolled after moving below the initially visible rows. 18 distinct rows in order: OpenAI; Claude Account; Corbanu API; Anthropic; Ambient; Kimi Code; Z.AI · default; Z.AI · badone; Z.AI · personal; Z.AI · work; DeepSeek; OpenRouter; Meta; Baseten; Vercel; Amazon Bedrock; Ollama; LM Studio.
- No arrow, more-text, count, or scrollbar hinted at hidden rows before scrolling. Down eventually wrapped from LM Studio back to OpenAI. Footer: `r recover credentials · Enter manage · Esc back`, then `Press enter to confirm or esc to go back`.
- Visible Z.AI markers: default row showed `Enabled · configured · current (status of account `personal`)`; badone and work showed `Named account · API key` plus fingerprints; personal showed `Named account · API key`, a fingerprint, `this session`, and `default for new sessions`.

### Model refusals/declines

- None observed.

### Expected results

- s0-2 shows `· account personal` and Account usage contains `no usage yet`: **passed**.
- s1 Account usage is one `personal` row with input and output counts: **passed**.
- `[advisory]` s1 exact form includes a cached-input count: **failed** — the row omitted the cached-input component.
- s2 shows `· account work`: **passed**.
- s2 Account usage is a table with `work (active)` and `personal`: **failed** — only one work row was shown, without `(active)` and without a personal row.
- No capture shows `$`, `cost`, or `price`: **passed** in captured status/provider screens.
- Account row shows no `agms-fake`: **passed**.
- `[observe]` footer: **observed** — before and after account switch the visible footer still ended `via zai default`; s0/s1 had `personal` context while footer said default.
- `[observe]` survey: **observed** as above.
- Relay: `sone` → `zpersonal`, `stwo` → `zwork`: **for the integrator** (sone 01:25:44–01:25:50Z; stwo 01:28:23–01:28:29Z).

VERDICT NA-10: failed
