# R3 amendment-v2 re-run — independent functional execution report

## Preflight and identity

- Prepared home listed: `zai3`.
- Coordinator preflight files were present. `preflight-summary.json` reported `ok: true`; `preflight-launcher.json` and `preflight-tmux-child.json` each reported `ok: true` with no failed probes.
- Executor preflight command `agms-probe all` was run at **2026-10-22 01:52:14Z** (approximate command-completion UTC), returned rc=0, wrote `$AGMS_RUN/evidence/preflight-executor.json`, and reported `ok: true` with no failed probes.
- `shasum -a 256 /opt/agms/pkg/bin/corbanu /opt/agms/packet/design-cases.md /opt/agms/packet/amendments-v2.md`:
  - corbanu: `bbae7d37dff9023268efdd05a8a4408ccf144ee51ba911afd39adc0e9ddce1b8` (matched);
  - design cases: `cfad42f543b6782a70b935b0596ac50737468733f3480629f49089e34eb6e5b4` (matched);
  - amendment v2: `fe1324914155fc4d37a83a4ac3bd7bdd0ac67f636cdf31e1da0cfb7c6a0f7e77` (matched).

## Case NA-06 — `/providers` rows, adding with a key, rename

### Requested versus actual
- Requested: START(zai3). Actual: START(zai3) in window **w1** (window A); session appeared.
- Requested: PROVIDERS, SURVEY, ROW for `work` and `personal`, open the Z.AI entry. Actual: performed as requested.
- Requested: choose the action containing `Add`, then add `added` and key `zadded`. Actual: performed; the Add action was available on the `Z.AI` default-provider actions screen.
- Requested: ROW(Z.AI, added), open its actions, choose `Rename`, rename to `renamed`. Actual: **tester navigation error** — the first `Enter` on the actions screen selected the already-highlighted `Use for this session`; it created a session on `added`. I then sent the literal string `renamed` as a model prompt instead of typing it into a rename field. After returning to `/providers`, I reached `Rename` correctly, but the product blocked it because the session was using `added`. No rename occurred.
- Requested: escape to composer, QUIT, CLI account list, and the two `exec` calls. Actual: performed, with the outcomes below.

### Positive checkpoints and observations
- 01:52:57Z — w1 displayed the Corbanu Terminal v0.1.48 banner and GLM 5.3 Flash footer (`n6-start`).
- 01:53:07Z — `/providers` opened (`n6-providers-initial`).
- Survey start: 01:53:29Z. The 17-row provider list scrolled after the initial visible viewport. Distinct rows, in order: OpenAI, Claude Account, Corbanu API, Anthropic, Ambient, Kimi Code, Z.AI · default, Z.AI · personal, Z.AI · work, DeepSeek, OpenRouter, Meta, Baseten, Vercel, Amazon Bedrock, Ollama, LM Studio. No pre-scroll “more/count/scrollbar” hint was visible. At the bottom, a later Down wrapped back to row 1 (`n6-s17`). The survey was complete; footer was `r recover credentials · Enter manage · Esc back`.
- `n6-work`: `Z.AI · work` highlighted with `Named account · API key · fp 9a89ed076f30`.
- `n6-personal`: `Z.AI · personal` highlighted with `Named account · API key · fp 515b09511c47 · this session · default for new sessions`.
- `n6-zai` (opened from the highlighted `personal` row) showed Rename/Remove only. To find Add, I backed out, opened `Z.AI · default`, whose actions screen had `Add another account`; this satisfied the case’s action search within the allowed window.
- Add flow: name field shown; `added` typed and Enter advanced; key field said `Paste the value` and showed a masked line after typing (`n6-key`); Enter produced `• Saved zai account \`added\`` and `n6-after` showed `Z.AI · added    Named account · API key · fp 187d7863d4f1`.
- During the rename attempt, selecting `Use for this session` started session `01a1614f-bf83-7c62-a9b6-a1516d1366b3` on `added`. The prompt `renamed` produced this model reply verbatim:
  - `I checked the workspace and Git reports no changes—there’s currently nothing staged or unstaged, so I can’t tell what was renamed.`
  - `What should I rename, and to what name?`
- A later correct selection of Rename returned this product error verbatim: `■ This session uses zai account \`added\`. Use another account for this session first, then rename it.` The list still showed `added` (`n6-renamed`).
- A non-confirmed Remove prompt was observed while attempting recovery (`n6-rename-field`), then cancelled with Escape. It offered Replace with `default`, `personal`, or `work`, and `Keep it`.
- 01:58:44Z — `/quit`; 01:58:54Z — w1 exited 0; terminal settings left: `icanon isig echo icrnl opost`.
- 01:58:53–01:58:55Z — w2 `account list` exited 0 and printed `added`, `personal`, `work` (no `renamed`).
- 01:59:07–01:59:09Z — w3 `exec --account renamed ... nren` exited 1 with `Error: thread/start: ... account \`renamed\` of provider \`zai\` is not configured; add it with \`corbanu account add zai renamed\` or pick another with \`--account\` ...`.
- 01:59:13–01:59:20Z — w4 `exec --account added ... nold` exited 0 and printed `nold`.
- 01:59:20Z — `agms-tui stop`.
- NO-SECRET for the typed add-account key: **passed**; the key appeared only masked in the key field before Enter.

### Expected-result outcomes
- Rows show kind/fingerprint for `work` and `personal`: **passed** (`n6-work`, `n6-personal`, survey).
- `personal` has both current-session and default markers: **passed** (`n6-personal`).
- Default row contents: **observed** — `Z.AI · default   Enabled · configured · current (status of account \`personal\`)`.
- Survey note: **observed** — 17 distinct rows, scrolled, complete, wrap at bottom, no pre-scroll more/count/scrollbar hint.
- `n6-after` shows `added` with kind/fingerprint: **passed**.
- `n6-renamed` shows `renamed`: **failed** — rename was blocked after the account became the active session; the product’s error is quoted above. This occurred after the tester’s first Enter selected `Use for this session`.
- Account list shows `renamed` and not `added`: **failed** — it showed `added` and did not show `renamed`.
- `exec renamed` exits 0 and prints `nren`: **failed** — exited 1 before any model request.
- `exec added` exits non-zero with not-configured text: **failed** — exited 0 and printed `nold`.
- NO-SECRET: **passed**.
- Relay `nren` → `zadded`: **for the integrator** — request window 01:59:07–01:59:09Z; the visible error indicates no model request.
- Relay nothing at `nold`: **for the integrator** — request window 01:59:13–01:59:20Z; the visible screen contains the model reply `nold`.
- Any relay traffic from the unintended `renamed` prompt: **for the integrator** — visible prompt time was about 01:55:50–01:56:30Z; exact action timestamps are in the harness log.

VERDICT NA-06: failed

## Case NA-07 — use for this session, make default, and removing the account in use

### Window mapping
- Window A = **w5**; window B = **w6**; final CLI = **w7**.

### Requested versus actual
- Requested: START(zai3), STATUS(u0). Actual: performed.
- Requested: PROVIDERS, ROW(work), choose `Use for this session`, Escape, STATUS(u1), ASK(uone). Actual: performed.
- Requested: ROW(work), choose `Make default`, Escape, save grep output. Actual: first attempt was a **tester navigation error**: the actions screen already highlighted `Make default`, but I pressed Down and selected `Rename`; the product showed its rename-in-use error. I returned to `/providers`, reached `work`, opened actions, and pressed Enter directly on `Make default`. It succeeded. The successful grep output is saved.
- Requested: START new window B, STATUS(u2), ASK(utwo). Actual: performed.
- Requested: Window A, ROW(work), choose `Remove`, handle replacement. Actual: first Remove attempt was another **tester navigation error** — Down twice from Rename/Remove wrapped back to Rename. I recovered to the `work` actions screen with Remove highlighted and pressed Enter. The replacement prompt appeared. I selected `Replace with personal`; the product switched the session to `personal` and immediately removed `work`, without a separate confirmation prompt.
- Requested: Escape to composer, STATUS(u3), ASK(uthree). Actual: performed.
- Requested: Window B SEND(ufour, 60), STATUS(u4). Actual: sent and checked at 30 seconds and end; the product displayed a fatal account error.
- Requested: CLI account list, cat config, stop. Actual: performed.

### Positive checkpoints, timestamps and observations
- 02:00:50Z — w5 started (`n7-start`).
- 02:00:56Z — `u0` status: provider row ended `· account personal`; Account usage `personal · no usage yet`; Session `01a16156-f8f2-7271-8727-33104bdec76c`.
- 02:01:12Z — `/providers` survey started. The 18-row list scrolled after the initial viewport. Distinct rows: OpenAI, Claude Account, Corbanu API, Anthropic, Ambient, Kimi Code, Z.AI · default, Z.AI · added, Z.AI · personal, Z.AI · work, DeepSeek, OpenRouter, Meta, Baseten, Vercel, Amazon Bedrock, Ollama, LM Studio. No pre-scroll more/count/scrollbar hint was visible. At the bottom, Down wrapped to row 1. Survey complete.
- `n7-work`: `Z.AI · work     Named account · API key · fp 9a89ed076f30`.
- 02:01:59Z — `uone` sent. Later checkpoint showed `• uone`; `u1` had shown `· account work` and Account usage `work · no usage yet`.
- 02:03:21Z — successful Make default; product said `• New sessions use zai account \`work\`. This session is unchanged.` Saved grep output showed `[provider_accounts]` and `zai = "work"`.
- 02:03:31Z — w6 started; 02:03:37Z — `utwo` sent. `u2` showed `· account work`. Later checkpoint showed `• utwo`.
- 02:05:50Z — correct Remove prompt opened (`n7-remove-replacement-prompt-correct`). Its heading named `Remove Z.AI account \`work\``, and it offered Replace with `default`, `added`, `personal`, or `Keep it`.
- 02:06:00Z — selected Replace with `personal`. Product then showed `• New session: requests to zai use account \`personal\`` and `• Removed zai account \`work\`.` No separate confirmation was offered after selecting the replacement.
- `u3` showed `· account personal`, Session `01a1615b-acf3-7f20-aafe-1b7618325133`, and Account usage `personal · no usage yet`.
- 02:06:17Z — `uthree` sent; later checkpoint showed `• uthree`.
- 02:06:33Z — `ufour` sent from window B; 02:07:03Z — `ufour` end. Window B showed no `• ufour`; its fatal error was verbatim:
  - `■ Fatal error: account \`work\` of provider \`zai\` has no stored API key; add it with \`corbanu account add zai work\` or choose another account`
- 02:07:27Z — `u4` status still showed `· account work`, with Account usage `work · 19 input · 8,576 cached input · 16 output (12 reasoning)`.
- 02:07:44–02:07:46Z — w7 `account list` exited 0 and listed only `added` and `personal`; no `work`.
- Final config: `[provider_accounts]` was present with `zai = "personal"`.
- 02:07:46Z — `agms-tui stop`.
- NO-SECRET: passed; no key was typed in this case.

### Expected-result outcomes
- `u0` shows `· account personal`: **passed**.
- `u1` shows `· account work`: **passed**.
- `u2` shows `· account work` for a new session: **passed**.
- `[provider_accounts]` lines from step 3: **observed** — successful action changed it to `zai = "work"`.
- Step 5 asks for replacement before removing, `work` is named on that screen, and after removal `u3` shows `· account personal`: **passed**.
- Step 7 has no `work`: **passed**.
- What `[provider_accounts]` says now: **observed** — `zai = "personal"`.
- Whether `• ufour` appears and what u4 shows: **observed** — no `• ufour`; fatal error quoted above; u4 remained on `work` and showed work usage from the earlier `utwo` turn.
- Survey note: **observed** — 18 distinct rows, scrolled, complete, wrap at bottom, no pre-scroll more/count/scrollbar hint.
- Relay `uone` → `zwork`: **for the integrator** — sent 02:01:59Z, reply observed before the next command; integrator should match the harness/screen timestamps.
- Relay `utwo` → `zwork`: **for the integrator** — sent 02:03:37Z, reply observed by about 02:04:07Z.
- Relay `uthree` → `zpersonal`: **for the integrator** — sent 02:06:17Z, reply observed shortly thereafter.
- At `ufour`, no request carries `zenv` or `zpersonal`, and none is keyless: **for the integrator** — request window 02:06:33–02:07:03Z; the visible product error indicates it failed before a request.
- Whether a request on `zwork` was made: **for the integrator** — request window 02:06:33–02:07:03Z.

VERDICT NA-07: passed
