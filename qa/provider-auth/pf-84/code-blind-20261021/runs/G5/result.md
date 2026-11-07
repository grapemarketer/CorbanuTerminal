# G5 functional execution report

Executor: independent functional executor. No human acceptance is claimed.

## Preflight

- 2026-10-21T23:32:59Z started required `agms-probe all`; it completed with rc=0 and `"ok": true` (51 probes in `preflight-executor.json`; `failed` field absent, not a failure marker).
- Coordinator files present: `preflight-summary.json` (`"ok": true`), `preflight-launcher.json` (`"ok": true`), `preflight-tmux-child.json` (`"ok": true`).
- Candidate SHA-256: `bbae7d37dff9023268efdd05a8a4408ccf144ee51ba911afd39adc0e9ddce1b8` — matches.
- Frozen cases SHA-256: `cfad42f543b6782a70b935b0596ac50737468733f3480629f49089e34eb6e5b4` — matches.
- Prepared homes listed: `fresh`, `zaione`.

Window map: ST-01 window A = `w1`.

## ST-01 — A home with no named accounts looks as it does with the feature off

### Actions and observations

- 23:34:15Z — START(`zaione`) in `w1` succeeded; wait for `glm-5.3-flash` rc=0. Screen showed model `glm-5.3-flash`, composer, and status line `Z.AI GLM 5.3 Flash via zai default`.
- 23:34:20Z sent `/status`; 23:34:23Z captured `z0`. Complete status box with `Session: 01a160d0-ba59-7f20-b5bb-7c09ee7c2ccd`; provider line `Model provider:       Z.AI - https://api.z.ai/api/paas/v4`; `Account:              Z.AI API key configured`; no `Account usage:` row and no `· account` text.

- 23:34:45Z sent ASK prompt text; 23:34:46Z pressed Enter; 23:34:51Z first 30 s poll matched `• zone` (`zone-poll` not needed). Positive checkpoint `zone`: reply `• zone`, status footer `via zai default`, no prompt.
- STATUS2(z1): `/status` sent 23:34:56Z, `z1-1` captured 23:34:58–23:34:59Z; second `/status` sent 23:35:05Z, `z1-2` captured 23:35:08Z. Both complete with `Session:`; both plain (no `· account`, no `Account usage:`); provider exactly `Z.AI - https://api.z.ai/api/paas/v4`.
- PROVIDERS: `/providers` sent 23:35:16Z; `providers-list` captured 23:35:18Z. It listed Z.AI row 7 as `Enabled · configured · current`. Escape at 23:35:19Z returned to composer (`providers-escape`); no extra second Escape needed.
- STATUS2(z2): first `/status` sent 23:35:26Z, `z2-1` complete and plain; second sent 23:35:35Z, `z2-2` captured 23:35:38Z, complete and plain, provider line exactly `Z.AI - https://api.z.ai/api/paas/v4`.
- QUIT: `/quit` text at 23:35:43Z, Enter, product exited by 23:35:44Z with code 0. Terminal settings left: `icanon isig echo icrnl opost`.
- CLI zexec in `w2`: started 23:35:54Z; output contained `zexec`; exited code 0 by 23:36:05Z; settings left normal.
- 23:36:13Z ran `agms-tui stop`, rc=0.

### Expected-result outcomes

- `z1-2` plain — **passed**: complete status, no account suffix or Account usage row.
- `z2-2` plain — **passed**: same observed facts.
- Provider line exactly `Z.AI - https://api.z.ai/api/paas/v4` — **passed** in z0, z1-1, z1-2, z2-1, z2-2.
- z0 account display — **observed**: shows `Account: Z.AI API key configured`; no explicit account name.
- No STATUS late/stalled — **passed**: all status boxes complete immediately.
- Integration comparison with REG-01a — **for the integrator** (advisory; relay/files not tester-visible).
- Relay `zone`/`zexec` → `zenv` — **for the integrator**: relay log not visible.

**VERDICT ST-01: passed**

## ST-02 — Accounts added/removed by another process; “has used”; /status never stalls

Window map: ST-02 window A = `w3`; other action windows `w4` (`account add extra`), `w5` (`account remove extra`), `w6` (`account add extrb`), `w7` (`account remove extrb`).

### Actions and observations

- 23:36:41Z — START(`zaione`) in `w3` succeeded; wait rc=0. Footer `via zai default`.
- ASK(dzero): sent 23:36:46Z, Enter 23:36:46Z, reply matched `• dzero` at 23:36:52Z. Positive checkpoint `dzero`.
- STATUS(d0): sent 23:36:56Z, captured 23:36:59Z; complete with SID `01a160d2-f2bc-7851-b4a0-b0c029de9466`, plain, provider line exact Z.AI URL.
- CLIK `account add zai extra` in `w4`: started 23:37:10Z. After 3 s the product had already exited code 1 with `Error: pipe the secret in instead of typing it, for example \`cat key.txt | corbanu account add <provider> <name>\``. Typed key at 23:37:15Z; Enter; remained exited. Thus the product never accepted the key and `extra` was not added. The typed synthetic key was echoed after product exit in `extra-added`.
- STATUS(d1): sent 23:37:38Z; complete and plain. STATUS(d2): sent 23:37:47Z, captured 23:37:50Z; complete and plain.
- Because d2 was plain, conditional PROVIDERS path ran: list captured 23:37:57Z, Escape at 23:37:59Z, STATUS(d3) captured 23:38:02Z; complete and plain.
- CLI `account remove zai extra` in `w5`: 23:38:12–23:38:14Z, code 1, `Error: zai has no account \`extra\``.
- STATUS2(d4): d4-1 sent 23:38:14Z, d4-2 sent 23:38:23Z and captured 23:38:26Z; both complete and plain.
- CLIK `account add zai extrb` in `w6`: 23:38:38–23:38:44Z, same immediate code-1 typed-secret refusal; `extrb` not added. Typed synthetic key echoed after exit in `extrb-added`.
- Attempted to open Z.AI: first Enter opened row 1 (`OpenAI`) because that was highlighted; immediately corrected with Escape and six Down keys to row 7 Z.AI, then Enter at 23:39:13Z. `d5-zai-correct` offered only Deactivate, Replace with API key, and Add another account; no `extrb` or `default` row and no “Use for this session” action. Requested selections were logged **not found** at 23:39:26Z; dependent ASK(dtwo) and ASK(dthree) were not exercised. Escape returned to the composer.
- CLI `account remove zai extrb` in `w7`: 23:39:26–23:39:28Z, code 1, `Error: zai has no account \`extrb\``.
- First STATUS2(d5) attempt started from the provider/Z.AI screen rather than composer; Enter selected Z.AI then Deactivate and reached “Choose replacement” (Ollama and LM Studio choices). No replacement was selected; three Escapes restored the composer by 23:40:00Z. Retried STATUS2(d5): d5-1-retry sent 23:40:00Z; d5-2-retry sent 23:40:09Z, captured 23:40:12Z; both complete and plain.
- QUIT at 23:40:20–23:40:21Z, code 0, terminal settings normal. `agms-tui stop` rc=0 at 23:40:21Z.

### Expected-result outcomes

- d0 plain — **passed**.
- d1 — **observed**: complete and plain.
- d2, or at latest d3, shows `· account default` and Account usage — **failed**: both remained plain; no account suffix or usage row. This followed the product’s refusal of the specified CLIK add path.
- d4-2 plain — **passed**.
- d5-2 shows `· account default` and Account usage — **failed**: d5-2-retry remained plain. Whether `extrb` is listed — **not exercised** because it was never added and had no row.
- No STATUS late/stalled — **passed** for all completed status captures; invalid navigation attempts were recorded, not status stalls.
- NO-SECRET(zextra,29044) — **failed** on the literal capture `extra-added`: the synthetic key was visible after Enter, though only as the tester’s echoed typed text after product exit. Same for NO-SECRET(zextrb,30117) in `extrb-added`.
- Relay dzero → zenv — **for the integrator**. Requests corresponding to `dtwo` and `dthree` — **not exercised**, also for the integrator if they infer nothing from the logged window.

**VERDICT ST-02: failed**

## ESC-04 — Normal onboarding in a fresh home

Window map: ESC-04 window A = `w8`.

### Actions and observations

- 23:41:26Z — START(`fresh`) in `w8`. At 23:41:38Z captured `e4-1`: onboarding provider list with options 1–8; row 8 was Z.AI API Key. Wording differed from the reference build but is not judged.
- Pressed Down seven times, screen after each (`e4-nav1` … `e4-nav7`), ending with row 8 highlighted at 23:41:52Z. Enter at 23:42:01Z produced `e4-2`: “Use your Z.AI API key”, masked entry `Paste or type your API key`, “Press enter to save”, “Press esc to go back”. No account wording appeared in e4-1/e4-2.
- Escape at 23:42:12Z produced `e4-3`, back on the provider list with Z.AI API Key still highlighted.
- Return to key screen: I first pressed Down, which selected DeepSeek, and entered DeepSeek’s key screen; immediately pressed Escape, then Up to restore Z.AI, and Enter at 23:42:28Z (`e4-return-zai-key`). The mistake and recovery are recorded; no data was entered.
- Typed key at 23:42:33Z. Before Enter, `e4-4` showed only 22 mask dots; no cleartext. Enter at 23:42:36Z produced `e4-after-key`: `✓ Z.AI API key configured` and “Corbanu Terminal will use Z.AI with your API key.”
- Further Enter screens within the six-screen allowance:
  - `next1` confirmation, Enter → `next2` at 23:42:46Z, provider list with Z.AI `Configured · active · ready`.
  - Enter → `next3` at 23:42:54Z, Z.AI actions: `Use configured credentials`, `Add another account`, `Replace with a new API key`.
  - Enter → `next4` at 23:43:00Z, back to the same provider list.
  - Enter → `next5` at 23:43:06Z, the same Z.AI actions again.
  - Enter → `next6` at 23:43:19Z, the same provider list again.
- At 23:43:31Z, the allowed six further screens were exhausted with no model choice and no composer (`e4-blocked-final`). No onboarding completion control was visible. `agms-tui stop` rc=0.

### Expected-result outcomes

- e4-1 onboarding provider list and e4-2 Z.AI key screen — **passed**. All wording differences — **observed**.
- e4-1 to e4-4 no account wording (`account \``, “not configured”, `--account`) — **passed**.
- e4-3 back on provider list — **passed**.
- e4-4 masks key — **observed**: 22 dots, no cleartext.
- NO-SECRET(zfresh,30517) after Enter — **passed**: e4-4 was pre-Enter and showed only mask; e4-after-key and later captures showed no synthetic key.
- fr1-2 plain — **blocked/not reached**: onboarding did not complete within its screen budget.
- Account list result — **blocked/not reached**.
- Model in use — **blocked/not reached**; no model choice was offered in the observed flow.
- Relay frone → zfresh — **not exercised**; ASK(frone) was not reached. Mark for the integrator only if any matching request must be ruled out.

**VERDICT ESC-04: blocked**

---

## Final verdicts

- VERDICT ST-01: passed
- VERDICT ST-02: failed
- VERDICT ESC-04: blocked
