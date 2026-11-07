# S3R functional execution report — NA-06 only

Scope: independent functional executor; no source, history, prior findings, relay log, or credentials read. Live packet text, tool help, prepared home listing, and product screens only. No human acceptance claimed.

## Preflight

- Candidate command: `/opt/agms/pkg/bin/corbanu`.
- Candidate SHA-256 requested/actual: expected `ba9feaa443ec18f29abc73a27137cc8f4696899ea4a1c3dada84c8d2df280006`; actual same.
- Frozen cases SHA-256 requested/actual: expected `cfad42f543b6782a70b935b0596ac50737468733f3480629f49089e34eb6e5b4`; actual same.
- Amendment v2 SHA-256 actual (assignment value matches): `fe1324914155fc4d37a83a4ac3bd7bdd0ac67f636cdf31e1da0cfb7c6a0f7e77`.
- Amendment v3 SHA-256 actual (assignment value matches): `ff365c23e584d67d678af2aed535904416af408805de5ab4d903a116d148dee4`.
- `agms-probe all > $AGMS_RUN/evidence/preflight-executor.json; echo rc=$?` requested and actual; rc `0`; JSON reports `"ok": true`.
- Coordinator files present and inspected: `$AGMS_RUN/evidence/preflight-summary.json`, `preflight-launcher.json`, `preflight-tmux-child.json`. Summary reports `launcher_ok: true`, `tmux_child_ok: true`, `pty_input_echoed: true`, `failed: []`, `ok: true`.
- Prepared home present: `zai3` (from `agms-tui homes`). Home path: `$AGMS_RUN/homes/zai3`.
- Preflight outcome: OK; case may proceed.

## NA-06 — `/providers` rows, adding with a key, rename

Priority: Blocker. Home: `zai3` as prepared. Text authority: amendments-v3 NA-06, with conventions as amended by amendments-v2.

### Requested actions and running actual record

Times are UTC (`date -u +%H:%M:%S`).

Actual execution to date (all literal actions; prompt text and Enter separate):

- START(zai3) requested. Actual: started window `w1` at 09:32:36 UTC; wait for `glm-5.3-flash` succeeded; label `start` captured at 09:32:39. Composer and v0.1.48 model line visible. Window mapping: A = `w1`.
- PROVIDERS requested. Actual: `/providers` typed and Enter pressed separately at 09:32:47–09:32:49; label `n6-prov0` captured. Initial visible list rows 1–8 (through `Z.AI · personal`).
- SURVEY requested. Actual started 09:33:28. Pressed Down one at a time with a screen after each, through 30 presses (ended 09:33:36). Extra Down and screen `n6-s-wrap` at 09:33:37. Pressed Up one at a time with a screen after each, returning to original OpenAI row on press 14 (ended 09:34:05). Label `n6-list` captured 09:34:10.
  - Survey stop reason: maximum 30 Down presses reached; therefore survey is incomplete under the convention.
  - Observed 15 distinct rows, in order: OpenAI; Claude Account; Corbanu API; Anthropic; Ambient; Kimi Code; `Z.AI · default`; `Z.AI · personal`; `Z.AI · work`; DeepSeek; OpenRouter; Meta; Baseten; Vercel; Amazon Bedrock.
  - Scrolling occurred: initial rows 1–8; after 30 Downs final visible rows 8–15. No visible pre-scroll arrow, "more", count, or scrollbar in the initial list.
  - Extra Down at the end did not wrap to row 1.
  - Footer text: `r recover credentials · Enter manage · Esc back`, followed by `Press enter to confirm or esc to go back`.
- ROW(Z.AI, work) requested. Actual: from row 1, eight Downs with screens after each reached `Z.AI · work`; label `n6-work` captured 09:34:28. No Enter pressed. Row content: `Named account · API key · fp 8c89e2955d76`.
- ROW(Z.AI, personal) requested. Actual: one Up reached `Z.AI · personal`; label `n6-personal` captured 09:34:37. Row content: `Named account · API key · fp 460d2edab8c6 · this session · default for new sessions`.
- Open Z.AI entry requested. Actual: with the Z.AI personal row highlighted, Enter at 09:34:43 opened personal account actions; label `n6-zai` captured. It showed `Rename` and `Remove`, not Add.
- To reach the requested action containing "Add", Escape returned to the provider list at 09:35:05; Up selected `Z.AI · default`; Enter at 09:35:06 opened provider-level actions (`n6-zai-default`), which included `Add another account`.
- Add action navigation: two Downs with screens selected `Add another account` at 09:35:17. After sleep 2, Enter at 09:35:19; label `n6-add1` at 09:35:21 showed the account-name field.
- FIELD(`added`) requested. Actual: screen asked for account name; `added` typed at 09:35:28, screen after 2 s, Enter at 09:35:31; screen after 2 s advanced to key entry. No extra Enter needed.
- Expected kind-choice screen: not encountered. After the name field, the next screen directly showed `Z.AI account added` / `API key — masked`. This is recorded as actual order difference; no key was typed until the screen asked for a key.
- KEYIN requested. Actual: key field prompt contained API key/Paste and showed a masked focused field. Typed the prescribed synthetic key at 09:35:48; label `n6-key` after 2 s showed only 22 dot mask characters. Enter at 09:35:50; label `n6-after` at 09:35:52 showed success and provider list row `Z.AI · added` with `Named account · API key · fp 687351a308a1`.
- NO-SECRET(zadded, prescribed digits): so far holds; the key is not shown in n6-key, n6-after, or any capture after Enter. The typed-key observe exception applies only to the field content, and that field was masked.

No system password prompt, real-account sign-in, missing required control, lost observation, refusal, or unexpected outside access encountered so far.

Continued actual execution:

- Rename setup: with the `/providers` list shown, Down selected `Z.AI · added`; label `n6-added` captured 09:36:57. Enter at 09:37:02 opened its actions; label `n6-actions` captured 09:37:04.
  - Highlighted first action: `Start a new session on this account`.
  - Visible actions in order: `Start a new session on this account`; `Make default`; `Rename`; `Remove`.
  - Two Downs with screens selected `Rename` at 09:37:16. Label `n6-rename-hl` captured; highlighted line was `Rename` and did not contain any prohibited action.
  - After sleep 2, Enter at 09:37:25 opened the rename field; label `n6-rfield` captured 09:37:27. It showed `Rename Z.AI account added` and the account-name field.
- Field clear: `C-u` pressed and label `n6-cleared` captured at 09:37:38. The visible prompt did not retain old text; no BSpace was needed.
- FIELD(`renamed`) requested. Actual: screen was a rename field; `renamed` typed at 09:37:38, screen at 09:37:40 showed it, Enter at 09:37:41. Screen at 09:37:42 showed success and returned to the provider list.
- ROW(Z.AI, renamed) requested. Actual: list was shown; two Downs with screens reached `Z.AI · renamed`; label `n6-renamed` captured 09:37:58. Row: `Named account · API key · fp 687351a308a1`.
- Escape until composer requested. Actual: one Escape at 09:38:09 returned to the composer; label `n6-escape-to-composer` captured.
- QUIT requested. Actual: `/quit` typed and Enter pressed separately at 09:38:09. Product exited code 0; terminal settings line: `[agms] terminal settings left: icanon isig echo icrnl opost`. Label `n6-quit` captured.
- CLI(zai3, `account list`) requested. Actual: window `w2` opened at 09:38:17, exited code 0 by 09:38:19. Label `n6-clilist` showed `zai personal`, `zai renamed`, `zai work`; no `added`.
- CLI(zai3, `exec --account renamed ...nren`) requested. Actual: window `w3` opened 09:38:27, exited code 0 at 09:38:35. Label `n6-clinren` printed `nren`.
- CLI(zai3, `exec --account added ...nold`) requested. Actual: window `w4` opened 09:38:44, product exited code 1 at 09:38:46. Label `n6-cliadded` showed the not-configured error. No `nold` reply.
- `agms-tui stop` requested and actual at 09:39:00.

## Expected-result outcomes

- Rows `work` and `personal` show kind and fingerprint where visible: **passed** (`n6-work`, `n6-personal`, and `n6-zai`).
- `personal` carries both current-session and default markers: **passed** (`n6-personal` / provider list).
- `[observe]` default row contents: observed. `n6-prov0` and related screens show `Z.AI · default  Enabled · configured · current (status of account personal)`. Opening it (`n6-zai-default`) showed provider-level actions.
- `[observe]` survey note: observed. Survey was incomplete only because the allowed 30 Down presses were exhausted, but 15 distinct rows were seen in order, scrolling occurred, and no pre-scroll hint or end wrap was seen. Full details above.
- `n6-after` or `n6-added` shows `added` with kind and fingerprint: **passed** (both do: `Named account · API key · fp 687351a308a1`).
- `n6-renamed` shows the account as `renamed`: **passed** (`Z.AI · renamed  Named account · API key · fp 687351a308a1`).
- Add fail rule not triggered: no add fallback was used; the account was added through `/providers`.
- Rename fail rules not triggered: Rename was found, all visible action lines were logged, and the rename field was shown.
- `[advisory]` actions offer `Start a new session on this account`, not `Use for this session`: **passed** (`n6-actions`).
- `[observe]` current-session marker in `n6-added`: observed. `n6-added` shows `added` without `this session`; the visible default row still identifies the current status as account `personal`. The add did not move the session to `added`.
- NO-SECRET(zadded, prescribed digits): **passed**. Every reviewed screen after Enter shows no key, label, digits, or `agms-fake`; `n6-key` showed only dots.
- `[observe]` key masking: observed. `n6-key` showed a 22-character dot mask.
- Step 5 lists `renamed` and not `added`: **passed**.
- Step 6 exits 0 and prints `nren`: **passed**.
- Step 7 exits non-zero with not-configured text for `added`: **passed**. Product code 1; error names `added`, provider `zai`, `corbanu account add zai added`, and `--account`.
- Relay checks for `nren` → `zadded` and nothing at `nold`: **for the integrator**. Logged start/end times above: `nren` 09:38:27–09:38:35 UTC; `nold` 09:38:44–09:38:46 UTC.
- No model refusal/decline occurred.
- No timeout, system password prompt, real-account sign-in, missing required control, lost observation, or unexpected outside access occurred.

VERDICT NA-06: passed

### Evidence-integrity note

- The product screens reviewed for NO-SECRET showed only a dot mask. The harness's own `evidence/actions.jsonl` records the literal `text` action, as `agms-tui` is designed to record every action; that is a harness action log, not a product screen. It was not edited or suppressed.
- The report intentionally does not repeat the synthetic key value, label, or digits.
