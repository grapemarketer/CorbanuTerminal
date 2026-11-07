# S3 amendment-v3 functional run — executor report

## Isolation preflight and identity

- `agms-probe all > $AGMS_RUN/evidence/preflight-executor.json; echo rc=$?` completed at 2026-10-22T09:13:44Z with `rc=0`; the JSON reports `"ok": true`.
- Coordinator preflight summary reports `"ok": true`; launcher and tmux-child preflight JSON files were present.
- Candidate digest matched `ba9feaa443ec18f29abc73a27137cc8f4696899ea4a1c3dada84c8d2df280006`.
- Frozen cases digest matched `cfad42f543b6782a70b935b0596ac50737468733f3480629f49089e34eb6e5b4`.
- Amendments-v2 digest matched `fe1324914155fc4d37a83a4ac3bd7bdd0ac67f636cdf31e1da0cfb7c6a0f7e77`.
- Amendments-v3 digest matched `ff365c23e584d67d678af2aed535904416af408805de5ab4d903a116d148dee4`.
- Prepared home listed: `zai3` only, as required.

# NA-06 — `/providers` rows, adding with a key, rename

- Start UTC: 2026-10-22T09:14:50Z.
- Window mapping: START window A = w1.
- 09:14:52Z: START succeeded; `n6-start` showed Corbanu Terminal 0.1.48, model glm-5.3-flash, composer, and status line `zai default`.

## Requested versus actual actions

### Step 1 — look at the rows

- 09:15:17Z: sent `/providers`, then Enter separately; `n6-providers` displayed the providers screen.
- SURVEY started 09:15:33Z. `Down` was pressed one key at a time with a screen after each (`n6-s2` through `n6-s26`). The list had 17 distinct rows, in this order:
  1. OpenAI — Not configured · unavailable
  2. Claude Account — Not configured · unavailable
  3. Corbanu API — Not configured · unavailable
  4. Anthropic — Not configured · unavailable
  5. Ambient — Not configured · unavailable
  6. Kimi Code — Not configured · unavailable
  7. Z.AI · default — Enabled · configured · current (status of account `personal`)
  8. Z.AI · personal — Named account · API key · fp d1a4d965456f · this session · default for new sessions
  9. Z.AI · work — Named account · API key · fp fbeba9005fd3
  10. DeepSeek — Not configured · unavailable
  11. OpenRouter — Not configured · unavailable
  12. Meta — Not configured · unavailable
  13. Baseten — Not configured · unavailable
  14. Vercel — Not configured · unavailable
  15. Amazon Bedrock — Not configured · unavailable
  16. Ollama — Enabled · configured
  17. LM Studio — Enabled · configured

- Survey observation: the list scrolled; before scrolling it showed rows 1–8 only and no arrow, count, “more” text or scrollbar. `Down` from row 17 wrapped back to row 1. Footer was `r recover credentials · Enter manage · Esc back`.
- Tester-method note: the scripted survey loop did not stop at the first wrap screen (`n6-s18`, row 1) and instead continued through `n6-s26`; the observations above were still read from captured screens. It was then returned to row 1 with eight `Up` presses/screens (`n6-u1`–`n6-u8`), and `n6-list` was captured.
- ROW(Z.AI, work): pressed `Down` eight times from row 1, with screens `n6-wd1`–`n6-wd8`; row 9 was highlighted. `n6-work` shows `Z.AI · work | Named account · API key · fp fbeba9005fd3`. No Enter pressed.
- ROW(Z.AI, personal): pressed `Up` once to row 8; `n6-personal` shows `Z.AI · personal | Named account · API key · fp d1a4d965456f · this session · default for new sessions`. No Enter pressed.
- Open Z.AI entry: pressed `Up` to row 7 (`n6-zai-pre`), then Enter; `n6-zai` displayed the Z.AI provider/action screen. Positive checkpoint: rows for `personal` and `work` both showed kind and fingerprint; `personal` showed both current-session and default-for-new-sessions markers.

### Step 2 — add an account

- Highlighted `Add another account` with two `Down` presses; screens `n6-add-d1` and `n6-add-hl`. After 2 s, pressed Enter; `n6-add-first` showed the name field “Add a Z.AI account”.
- FIELD(`added`): `n6-name-check` confirmed the name field; typed `added`, waited 2 s, captured `n6-name-typed` showing `added` in the field; pressed Enter and waited 2 s. The next screen was already the key-entry screen for `Z.AI account added / API key — masked`, captured `n6-kind`.
- The key-entry screen itself contained the visible text “API key” and was captured after an additional 2 s in `n6-kind-selected`; no separate kind-choice screen appeared. Because that screen was the key field, KEYIN proceeded.
- KEYIN(`agms-fake-zadded-40958`, n6-key): typed the key, waited 2 s. `n6-key` showed only 22 mask bullets. Pressed Enter separately after 2 s; `n6-after` showed `Saved zai account 'added'` and the providers list row `Z.AI · added | Named account · API key · fp f059991063b7`.
- Positive checkpoint: `added` was saved and displayed with kind and fingerprint.
- `[observe]` NO-SECRET: the typed key was masked in `n6-key`; the secret was not visible after Enter. The one-time expected test input remains known from the packet but no capture exposed it.
- `[observe]` After the add, the current-session marker remained on the `default` row (`current (status of account personal)`); `added` did not carry the current-session marker.
- Add fallback: not taken.

### Step 3 — rename `added`

- From the providers list, pressed `Down` once to row 8; `n6-added` showed `Z.AI · added | Named account · API key · fp f059991063b7`.
- Opened actions with Enter; `n6-actions` showed four visible actions:
  1. `Start a new session on this account` (highlighted)
  2. `Make default`
  3. `Rename`
  4. `Remove`

  The highlighted line was `Start a new session on this account`, satisfying the advisory wording change.
- Pressed `Down` twice, one at a time, until the third action (`Rename`) was highlighted. `n6-rename-hl` showed the highlighted line as `3. Rename`, and not `Start a new session`, `Make default` or `Remove`.
- After the required 2-second pause, pressed Enter on that highlighted `Rename` line. Product actual: instead of a rename field, it started a new session. `n6-rfield` showed `• New session: requests to zai use account 'added'.` and the composer `› Explain this codebase`. It did not show a rename/name field.
- Logged: **rename field not shown**.
- No text was typed into this unexpected session. No recovery or alternate rename path was attempted, as the case requires skipping the remainder of step 3.

### Step 4 — quit

- The unexpected screen already showed the composer, so the “Escape until the composer shows” condition was already met; no additional Escape was pressed.

### Steps 5–7 — CLI checks

- 09:20:14Z–09:20:15Z: QUIT succeeded. `n6-quit` shows `[agms] product exited with code 0` and `[agms] terminal settings left: icanon isig echo icrnl opost`. It also showed two generated session-continuation IDs:
  - `01a162e4-50e5-7571-bbc2-de0820d334d2`
  - `01a162e7-ead8-7c81-9f4d-ebc5af4a4954`

- Step 5, 09:20:37Z: CLI `account list` opened in w2, exited 0. It listed `zai added api_key`, `zai personal api_key`, and `zai work api_key`; it did not list `renamed`.
- Step 6 (`exec --account renamed ... nren`), 09:20:50Z–09:20:52Z, w3: exited 1 with the product's error: `Error: thread/start: thread/start failed: account `renamed` of provider `zai` is not configured; add it with `corbanu account add zai renamed` or pick another with `--account` (see `corbanu account list`) (code -32600)`. No `nren` reply.
- Step 7 (`exec --account added ... nold`), 09:20:59Z–09:21:15Z, w4: exited 0 and printed `nold`, using `provider: zai` and session id `01a162e9-ea68-70d0-ac9d-cda58edebc4f`.
- Final action: `agms-tui stop`.

## Positive visible checkpoints

- START: 0.1.48 and glm-5.3-flash shown in `n6-start` at 09:14:52Z.
- Providers rows: `personal` and `work` rows both showed kind and fingerprint in `n6-personal` and `n6-work`.
- Add: `n6-after` showed the saved `added` row with kind and fingerprint.
- Rename action: `n6-actions` offered `Rename`; `n6-rename-hl` had it highlighted.
- Quit: clean exit code 0 and normal terminal settings in `n6-quit`.

Exact capture times from the test harness record:

| Screen label | UTC time |
|---|---|
| `n6-start` | 09:14:53Z |
| `n6-providers` | 09:15:19Z |
| `n6-list` | 09:16:03Z |
| `n6-work` | 09:16:14Z |
| `n6-personal` | 09:16:18Z |
| `n6-zai` | 09:16:34Z |
| `n6-after` | 09:17:51Z |
| `n6-added` | 09:18:36Z |
| `n6-actions` | 09:18:39Z |
| `n6-rename-hl` | 09:18:46Z |
| `n6-rfield` | 09:18:50Z |
| `n6-quit` | 09:20:15Z |
| `n6-account-list` | 09:20:37Z |
| `n6-nren-30` | 09:20:52Z |
| `n6-nold-90` | 09:21:15Z |

## Ambiguities and product mismatches

- There was no separate choice screen for account kind. After the name field advanced, the next screen directly asked for the API key and displayed `API key — masked`.
- Although `n6-rename-hl` showed `Rename` highlighted, Enter started a new session on `added`. The case therefore required logging “rename field not shown” and skipping the remainder of step 3.
- After that Enter, the active session moved to `added`; `n6-rfield` showed the new-session notice. The original default row's `current (status of account personal)` marker had appeared before that Enter.
- The quit screen showed two session-continuation IDs. The screen itself does not state the cause of the second ID; recorded as an ambiguity without inferring internals.
- SURVEY method note: the scripted loop continued beyond the first wrap screen. All required observations were still captured; this is recorded rather than hidden.

## Expected-result outcomes

- Rows `work` and `personal` show kind and fingerprint where visible — **passed**: seen in `n6-work` and `n6-personal`.
- `personal` carries both current-session and default markers — **passed**: seen in `n6-personal` and `n6-list`.
- `[observe]` default row contents and survey note — **observed**: default row says `Enabled · configured · current (status of account personal)`; survey details above.
- `[advisory]` marker wording — **passed**: `current` and `default for new sessions` are clear; no exact expected wording was stated.
- `n6-after`/`n6-added` shows `added` with kind and fingerprint — **passed**.
- `n6-renamed` shows `added` as `renamed` — **failed**: rename field was not shown after pressing Enter on highlighted Rename, so no rename occurred.
- Fail rule for “rename field not shown” — **failed** for rename results, as required by the case's fail rule.
- `[advisory]` actions use “Start a new session on this account” rather than “Use for this session” — **passed**: `n6-actions` shows the new label.
- `[observe]` current-session marker in `n6-added` — **observed**: before opening actions, `added` did not carry current-session; `default` did.
- NO-SECRET(zadded, 40958) — **passed** on observed screen output: `n6-key` showed 22 mask bullets and no capture output displayed the secret or label/digits; the typed-text observation exception covers the field input.
- `[observe]` whether `n6-key` shows the key or a mask — **observed**: mask.
- Step 5 lists `renamed`, not `added` — **failed**: listed `added`, not `renamed`.
- Step 6 exits 0 and prints `nren` — **failed**: exited 1 and said `renamed` is not configured.
- Step 7 exits non-zero with not-configured text for `added` — **failed**: exited 0 and printed `nold`.
- Relay `nren` → `zadded` — **for the integrator**: no request during 09:20:50Z–09:20:52Z because the CLI refused locally; integrator should confirm none.
- Relay nothing at `nold` — **for the integrator**: a request was attempted during 09:20:59Z–09:21:15Z; integrator should check account label and response.

## Refusals and declines

- Step 6's model provider refused the session start before any model request, verbatim: `Error: thread/start: thread/start failed: account `renamed` of provider `zai` is not configured; add it with `corbanu account add zai renamed` or pick another with `--account` (see `corbanu account list`) (code -32600)`.

## Result log

- 09:14:50Z — NA-06 start.
- 09:15:17Z — `/providers` sent and Enter pressed.
- 09:15:33Z — SURVEY started.
- 09:20:14Z — QUIT start.
- 09:20:15Z — QUIT complete.
- 09:20:37Z — account list complete.
- 09:20:50Z — `nren` command start.
- 09:20:52Z — `nren` command exit.
- 09:20:59Z — `nold` command start.
- 09:21:15Z — `nold` command exit.
- `agms-tui stop` after the final CLI check.

VERDICT NA-06: failed
