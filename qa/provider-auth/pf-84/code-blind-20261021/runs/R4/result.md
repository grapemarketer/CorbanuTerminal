<!-- integrator: the tester wrote this report to $AGMS_RUN/work/evidence/result.md (its working directory) instead of $AGMS_RUN/evidence/ -->
# R4 amendment v2 re-run — independent functional executor

Session started (UTC): 2026-10-22. This is not human acceptance.

## Preflight

- Coordinator preflight summary: `ok: true`; both `preflight-*.json` reports `ok: true`, no failures.
- Assigned homes requirement: `zaione`, `fresh` — confirmed by `agms-tui homes` (see case notes).
- Candidate/package and frozen-case hashes: verified against the assignment.
- Executor isolation probe `agms-probe all`: completed at 02:17:22 UTC, rc 0, parsed `"ok": true`, no failed probes recorded.
- Hashes observed:
  - `/opt/agms/pkg/bin/corbanu` = `bbae7d37dff9023268efdd05a8a4408ccf144ee51ba911afd39adc0e9ddce1b8` (matches).
  - `/opt/agms/packet/design-cases.md` = `cfad42f543b6782a70b935b0596ac50737468733f3480629f49089e34eb6e5b4` (matches).
  - `/opt/agms/packet/amendments-v2.md` = `fe1324914155fc4d37a83a4ac3bd7bdd0ac67f636cdf31e1da0cfb7c6a0f7e77` (matches).

## ST-02 — Accounts added or removed by another process; "has used" rule; `/status` never stalls

Window mapping: A = w1.

### ST-02 actions and observations

- 02:17:22 — Required homes confirmed via `agms-tui homes`: `fresh`, `zaione`.
- w1 START(zaione) succeeded; TUI showed `glm-5.3-flash` and the composer.
- 02:18:00 — "dzero sent"; 02:18:01 — Enter sent.
- 02:18:08 — wait found `• dzero`; screen label captured (unlabeled initial checkpoint). Positive checkpoint: reply `• dzero`.
- d0 at 02:18:52–55 — complete status box, `Session: 01a16166-638c-74e3-a7f1-b65e7b88236d`. Plain: `Model provider:` had no `· account`; no `Account usage:` row.
- 02:18:36 — CLIS(zaione, `account add zai extra`) on w2: product exited code 0; screen said `Saved zai account \`extra\` (api_key).` No typed key was displayed.
- d1 at 02:18:52–55 — complete status box, still plain.
- d2 at 02:19:02–05 — complete status box; `Model provider:` ended `· account default`; `Account usage: default · 8,595 input · 27 output (23 reasoning)`. No d3 needed because d2 was not plain.
- 02:19:17 — CLI(zaione, `account remove zai extra`) on w3: product exited code 0; `Removed zai account \`extra\`.`
- d4-1 at 02:19:25–28 — complete status box still showed `· account default` and default usage row.
- d4-2 at 02:19:33–36 — complete status box plain: no `· account`; no `Account usage:` row.

No status was late or stalled through d4-2. Neither synthetic key's full material nor its label-specific digits appeared in captured screens.

### ST-02 navigation and continuation

- Survey started 02:20:40 on `/providers` (`st2-prov-initial`). Initial visible rows, in order:
  1. OpenAI; 2. Claude Account; 3. Corbanu API; 4. Anthropic; 5. Ambient; 6. Kimi Code; 7. `Z.AI · default` (`Enabled · configured · current · this session · default for new sessions`); 8. `Z.AI · extrb` (`Named account · API key · fp 5ba4e0bb9e24`).
- `Down` repeatedly revealed additional rows 9. DeepSeek, 10. OpenRouter, 11. Meta, 12. Baseten, 13. Vercel, 14. Amazon Bedrock, 15. Ollama, 16. LM Studio. The last `Down` (after row 16) wrapped to row 1 (`st2-s16`). Survey completed because the highlight returned to the starting row. Before scrolling, the screen showed no arrow/count/scrollbar hint that more rows existed. Footer: `r recover credentials · Enter manage · Esc back`.
- `ROW(Z.AI, extrb)` highlighted row 8 (`st2-extrb-row`). Enter opened its account actions (`st2-extrb-actions`): `Use for this session`, `Make default`, `Rename`, `Remove`. `Use for this session` was already highlighted and was chosen. The result was a new session banner: `• New session: requests to zai use account \`extrb\`.` (`st2-use-extrb`).
- 02:24:38 — "dtwo sent"; 02:24:45 — `• dtwo` found.
- For `ROW(Z.AI, default)`, row 7 was highlighted (`st2-default-row`). Enter opened Z.AI's provider-level actions, not the default account's own actions. In an unintended intermediate step, a second Enter chose highlighted `Deactivate`, which showed a replacement chooser; Escape returned to the provider list without completing deactivation. This unplanned action is recorded and did not choose a replacement.
- On reopening provider actions, the actual offered action was `Use default account for this session` (`st2-use-default-action`); it was selected. It produced a new session banner: `• New session: requests to zai use account \`default\`.` (`st2-use-default`). This label differs from the case's shorthand wording.
- 02:27:18 — "dthree sent"; 02:27:26 — `• dthree` found.
- 02:27:34 — CLI(zaione, `account remove zai extrb`) on w5: product exited code 0; `Removed zai account \`extrb\`.`
- d5-1 at 02:27:41–44 — complete status box showed `· account default` and `Account usage: default · 8,595 input · 17 output (13 reasoning)`.
- d5-2 at 02:27:46–49 — complete status box showed **no** `· account` and **no** `Account usage:` row. This is the judged result for the "has used" rule and does not match the expected d5-2.
- 02:28:19 — QUIT succeeded with product exit code 0; terminal settings line was normal. `agms-tui stop` returned `stopped`.

### ST-02 expected-result outcomes

- Steps 2 and 5 exit 0 and confirm accounts: **passed**.
- d0 plain: **passed**.
- d1: **observed** — still plain immediately after adding `extra`.
- d2/d3 shows `· account default` and an Account usage row: **passed** (d2; no d3 needed).
- d4-2 plain: **passed**.
- d5-2 shows `· account default` and an Account usage row: **failed** — it was plain.
- No STATUS late/stalled: **passed**; all captured status boxes were complete and current.
- `extrb` listed in d5 usage: **not exercised** — d5-2 had no usage row; d5-1 did not list `extrb`.
- NO-SECRET for the `extra` and `extrb` synthetic keys: **passed** in captured screens; stdin key text was not displayed.
- Survey note: **observed** as above.
- Relay outcomes are **for the integrator**: `dzero` 02:18:00–02:18:08; `dtwo` 02:24:38–02:24:45; `dthree` 02:27:18–02:27:26.

**VERDICT ST-02: failed** (the d5-2 blocker check failed; no case was blocked).

## ESC-04 — Normal onboarding in a fresh home

Window mapping: A = w6.

### ESC-04 actions and observations

- 02:29:08 — START(fresh). After 10 seconds, `e4-1` was the onboarding provider list: welcome heading, `Choose a provider account for Corbanu Terminal`, rows 1–8 with row 1 `OpenAI Codex Account` highlighted and row 8 `Z.AI API Key` visible. Footer: `Use ↑/↓ to choose · Press enter to continue`.
- Pressed `Down` seven times, with a screen after each (`e4-down1` through `e4-down7`), until row 8 was highlighted. Enter opened the Z.AI key screen `e4-2`: heading `Use your Z.AI API key`, masked API-key field `Paste or type your API key`, `Press enter to save`, `Press esc to go back`.
- Escape returned to the provider list (`e4-3`) with row 8 still highlighted. Enter returned to the Z.AI key screen (`e4-key-again`).
- KEYIN: typed the requested synthetic key; `e4-4` showed only 20 mask characters (`•`), not the literal key. Pressed Enter; the next screen showed `✓ Z.AI API key configured` and `Corbanu Terminal will use Z.AI with your API key.` After 3 seconds, `e4-5` remained that confirmation screen.
- Finish path:
  - `e4-6`: provider list, row 8 now `Provider: Z.AI · Configured · active · ready`; highlight on row 8.
  - Per the provider-list rule, SURVEY began at 02:31:31 without pressing Enter on a provider. Down revealed rows 9. DeepSeek API Key, 10. OpenRouter API Key, 11. Meta API Key, 12. Baseten API Key, 13. Vercel API Key, 14. Amazon Bedrock, 15. Ollama, 16. LM Studio, and 17. Done — `Finish provider setup`. Another Down wrapped from row 17 to row 1. Up keys then returned the highlight to the survey-start row, row 8 (`e4-up10`). Survey completed because it wrapped and returned; before scrolling there was no visible more/count/scrollbar hint. Footer remained `Use ↑/↓ to choose · Press enter to continue`.
  - Down moved from row 8 back to Done (`e4-done-row`), and Enter was pressed on Done.
  - `e4-after-done` showed the directory-trust prompt with `1. Yes, continue` highlighted and `2. No, quit`.
  - Enter accepted the highlighted `Yes, continue`. The composer then appeared, with header `model: glm-5.2` and footer `Z.AI GLM 5.2 via zai standard`. No model-choice screen was offered.
- 02:35:52 — "frone sent"; Enter sent separately; 02:35:56 — `• frone` found.
- fr1-1 at 02:36:04–07 and fr1-2 at 02:36:12–15 — both complete, plain status boxes: no `· account` and no `Account usage:` row; session `01a16176-d096-7180-9168-edff51966edb`; model `Z.AI GLM 5.2`.
- 02:36:28 — QUIT succeeded, product exit code 0, normal terminal-settings line.
- 02:36:37 — CLI(fresh, `account list`) on w7: product exit code 0; screen said `No named accounts. Every provider uses its default account.` `agms-tui stop` returned `stopped`.

### ESC-04 expected-result outcomes

- e4-1 onboarding provider list and e4-2 Z.AI key screen: **passed**.
- No account wording (`account \``, `not configured`, `--account`) in e4-1 through e4-4: **passed** based on visible text.
- e4-3 returned to provider list: **passed** (advisory).
- e4-4 key mask: **observed** — the key appeared as mask characters before Enter.
- NO-SECRET for the fresh-home synthetic key: **passed** in every captured screen after Enter; the literal typed value was not shown.
- Onboarding leads to a composer within step 5: **passed** via Done → trust confirmation → composer.
- fr1-2 plain: **passed**.
- `account list` has no named account: **passed**.
- Model in use: **observed** — `glm-5.2`; no `glm-5.3-flash` model choice was shown.
- Survey/path: **observed** as above.
- Relay outcome is **for the integrator**: `frone` 02:35:52–02:35:56.

**VERDICT ESC-04: passed**.

## Group summary

- `VERDICT ST-02: failed`
- `VERDICT ESC-04: passed`

The group run ended at 02:36:37 UTC after the final `agms-tui stop`. No live account outside the assigned test scope was used, and this report is not a claim of human acceptance.
