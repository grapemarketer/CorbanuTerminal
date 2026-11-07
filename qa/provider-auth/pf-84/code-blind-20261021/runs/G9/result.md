# Group G9 functional execution report

- Executor: independent functional executor, code-blind.
- Cases in order: NA-13, D136, ST-03.
- Candidate: `/opt/agms/pkg/bin/corbanu`, Corbanu Terminal 0.1.48.
- Home: `oai1`.
- No `$AGMS_RUN/evidence/t1-not-signed-in` file existed at 2026-10-21T22:14:52Z.

## Isolation preflight and identity

- 2026-10-21T22:15:03Z: `agms-probe all` returned rc=0 and JSON reported `"ok": true`; output saved at `preflight-executor.json`.
- Coordinator files `preflight-summary.json`, `preflight-launcher.json`, and `preflight-tmux-child.json` were present. `preflight-summary.json` reported `"ok": true`, launcher and tmux child checks true, pty input echoed true, failed list empty.
- 2026-10-21T22:15:27Z: SHA-256 of candidate was `bbae7d37dff9023268efdd05a8a4408ccf144ee51ba911afd39adc0e9ddce1b8` (expected match).
- 2026-10-21T22:15:27Z: SHA-256 of frozen cases was `cfad42f543b6782a70b935b0596ac50737468733f3480629f49089e34eb6e5b4` (expected match).
- Prepared homes listed `oai1` only, matching assignment.
- Result: preflight passed; proceed.

## Window mapping

- To be recorded as opened.

## NA-13

- Home `oai1`; baseline `auth.json` SHA-256 at 2026-10-21T22:17:06Z: `76b93c8ea88062da141cd6f18335bfd54428e3dde6be0eab89d90403e6be3400`. Only the hash was recorded; credential contents were not read.

### Actions requested and actual

| NA-13 requested | Actual |
|---|---|
| 1. `CLI(oai1, account list)` | w1, 2026-10-21T22:17:06Z. Exited 0 by 22:17:08Z. Screen `na13-acct-before`. |
| 1. `CLI(oai1, login status)` | w2, 22:17:16Z. Exited 0 by 22:17:18Z. Screen `na13-login-status`. |
| 1. `CLI(oai1, login --account t1 status)` | w3, 22:17:16Z. Exited 0 by 22:17:20Z. Screen `na13-login-t1-status`. |
| 2. `START(oai1)` | w4, 22:17:31Z; `gpt-5.6-luna` matched at 22:17:33Z. Screen `na13-start-default`. |
| 2. `STATUS2(t-def)` | First `/status` 22:17:41Z, screen `na13-t-def-1` at 22:17:44Z; second at 22:17:49Z, screen `na13-t-def-2`. Complete boxes. |
| 2. `QUIT` | `/quit` at 22:17:56Z; exited code 0 immediately; terminal modes `icanon isig echo icrnl opost`. Screen `na13-default-quit`. |
| 3. `START(oai1, --account t1)` | w5, 22:18:02Z; `gpt-5.6-luna` matched at 22:18:05Z. Screen `na13-start-t1`. |
| 3. `STATUS2(t0)` | First at 22:18:11Z, screen `na13-t0-1` at 22:18:14Z; second at 22:18:19Z, screen `na13-t0-2`. Complete boxes. |
| 3. `ASK(tone)` | Sent 22:18:28Z; matched `• tone` at 22:18:30Z; screen `na13-tone-check-30s`. |
| 3. `STATUS(t1s)` | 22:18:36Z; complete box; screen `na13-t1s` at 22:18:40Z. |
| 4. `PROVIDERS`, open OpenAI, rows | `/providers` at 22:18:46Z; screen `na13-providers-initial` (this is the requested row screen). OpenAI line already highlighted; Enter at 22:18:55Z, screen `na13-openai-managed`. |
| 4. Choose Add another account | Down×3 with screens `na13-openai-managed-down1..3`; Enter 22:19:21Z showed name prompt `na13-add`. Blank Enter did not progress (`na13-add-enter-blank`). Typed the otherwise-unspecified name `oextra`, Enter, then Enter: screen `na13-add-options-before-api` at 22:21:05Z. |
| 4. Choose device-code ChatGPT option | Option 1 already highlighted; Enter at 22:20:04Z; screen `na13-dev` showed URL `https://auth.openai.com/codex/device`, code `<CODE>`, and “Waiting for the sign-in…”. Escape at 22:20:16Z returned to the composer; the sign-in banner remained (`na13-dev-escape-1`). A second Escape showed `esc again to edit previous message` (`na13-dev-escape-2`), so no more Escape was pressed. |
| 4. Choose API-key option | Re-entered `/providers`, OpenAI, Add another account, typed `oextra`, selected “OpenAI API key”. Masked prompt before typing: `na13-key-prompt-empty`; selected option screen: `na13-api-selected`. |
| 4. Type synthetic key, no Enter | At 22:21:12Z typed 22 characters; screen `na13-key` showed 22 mask characters and no plaintext. |
| 4. Escape until composer | Escape 1 to Providers (`na13-key-escape-1`); Escape 2 to composer, detected (`na13-key-escape-2`). Device sign-in banner still visible. |
| 5. `QUIT` then `CLI account list` | `/quit` at 22:22:00Z; exited code 0 immediately; terminal modes `icanon isig echo icrnl opost`; screen `na13-t1-quit`. w6 account list at 22:22:02Z exited 0; screen `na13-acct-after`. |

### Positive checkpoints / observations

- 22:17:08Z: `na13-acct-before` listed OpenAI `t1`, kind `chatgpt_auth`, “ChatGPT sign-in · <email-hint> · Free”.
- 22:17:18Z: `na13-login-status` said “Logged in using an API key - API key configured (fingerprint 1b16f382eb0c)”; no `agms-fake` or `odefault`.
- 22:17:20Z: `na13-login-t1-status` said “OpenAI account `t1`: ChatGPT sign-in · <email-hint> · Free”.
- 22:17:49Z: `na13-t-def-2` showed `Model provider: OpenAI · account default`, `Account: OpenAI API key configured (run corbanu login to use ChatGPT)`, and `Account usage: default · no usage yet`. This is `[observe]`.
- 22:18:19Z: `na13-t0-2` showed `Model provider: OpenAI · account t1` and `Account: ChatGPT sign-in · <email-hint> · Free (account t1)`.
- 22:18:30Z: `tone` was answered with the single visible word `tone`.
- 22:18:40Z: `na13-t1s` showed `Model provider: OpenAI · account t1` and `Account usage: t1 · 13,192 input · 5 output`.
- 22:20:09Z: provider rows included OpenAI `t1`: `Named account · ChatGPT sign-in · <email-hint> · Free · fp 52c02edfbb9a · this session`.
- 22:20:09Z: `na13-dev` showed the device-code first screen with URL/code.
- 22:21:13Z: `na13-key` showed only mask characters.
- No observed capture showed `eyJ`, `sk-`, or `agms-fake`; `na13-key` itself showed the requested typed value only as mask characters.
- Escape cancelled the device-code path without a visible error, but the banner remained on screen afterward; this is an ambiguity to preserve, not interpreted as completion.

### Expected-result assessment so far

- n13-rows t1 row: passed.
- Step 1 account list: observed as requested.
- `login status`: passed.
- `login --account t1 status`: observed.
- No secret exposure: passed in observed captures.
- n13-key masks: passed.
- n13-add options: passed.
- n13-dev: passed with observation that Escape returned to composer and banner persisted.
- Step 5 account list: pending.
- t0-2/t1s account display and tone: passed.
- t-def provider line: observed.
- NO-ONBOARD elsewhere: passed in observed captures.
- Relay/chatgpt traffic at tone and no OpenAI API request carrying odefault: for the integrator.
- auth.json hash unchanged: passed; after exit at 22:22:02Z it remained `76b93c8ea88062da141cd6f18335bfd54428e3dde6be0eab89d90403e6be3400`.
- Final account list showed `t1` and no other named account.
- NA-13 VERDICT: passed.

## D136

- Baseline before removal: `config.toml` had no `[provider_accounts]` and no `openai = "t1"` entry at 2026-10-21T22:22:09Z.
- Baseline file names observed before removal (for later `[observe]` comparison): `auth.json`, `config.toml`, `history.jsonl`, `installation_id`, `log/codex-login.log`, and `secrets/local.age`, plus product database/lock files.
- Window A will be `w7`.

### Actions requested and actual

| D136 requested | Actual |
|---|---|
| 1. `START(oai1, --account t1)` (window A=w7) | 22:23:43Z; `gpt-5.6-luna` matched at 22:23:45Z; screen `d136-start-t1`. |
| 1. `ASK(beforeone)` | Sent 22:23:52Z; matched at 22:23:54Z; screen `d136-beforeone-check-30s`. |
| 1. `STATUS(x0)` | 22:24:00Z; screen `d136-x0` at 22:24:03Z showed `· account t1` and t1 usage. |
| 2. `CLI(oai1, account remove openai t1)` | w8 at 22:24:07Z; exited 0 at 22:24:09Z: “Signed out and removed openai account `t1`.” Screen `d136-remove-attempt`. The fallback `logout` was not needed. |
| 3. `CLI(oai1, account list)` | w9 at 22:24:15Z; exited 0 by 22:24:17Z: “No named accounts. Every provider uses its default account.” Screen `d136-acct-after-remove`. |
| 4. Window A sleep 10, `STATUS2(x1)` | First status screen `d136-x1-1` at 22:24:35Z; second `d136-x1-2` at 22:24:40Z. Both newest boxes were complete and showed `Account: account t1 is not signed in`. They did not show a recovery command/path. |
| 5. `SEND(afterone, 90)` | Sent 22:24:46Z; monitored at 30s (`d136-afterone-30s`, 22:25:16Z), 60s (`d136-afterone-60s`, 22:25:59Z), end 90s (`d136-afterone-90s-end`, 22:26:29Z). `• afterone` appeared. |
| 5. `SEND(aftertwo, 90)` | Sent 22:26:33Z; monitored at 30s (`d136-aftertwo-30s`, 22:27:03Z), 60s (`d136-aftertwo-60s`, 22:27:44Z), end 90s (`d136-aftertwo-90s-end`, 22:28:14Z). `• aftertwo` appeared. |
| 6. `STATUS(x3)` | 22:28:20Z; screen `d136-x3` at 22:28:24Z. Newest box showed `Model provider: OpenAI · account t1`, `Account: account t1 is not signed in`, and t1 usage. No recovery command/path was visible. |
| 7. Window A PROVIDERS, open OpenAI, screen `x-prov`, Escape, QUIT | `/providers` 22:28:33Z; open OpenAI; `d136-x-prov` showed no `t1` row and options for API key/new OpenAI account; Escape to Providers (`d136-x-prov-escape-1`), Escape to composer (`d136-x-prov-composer`). `/quit` at 22:28:47Z exited code 0; terminal modes normal; screen `d136-quit`. |
| 8. `CLI(oai1, account list)` | w10 at 22:28:52Z; exited 0 by 22:28:54Z: “No named accounts.” Screen `d136-acct-final`. `agms-tui stop` run after checks. |

### Positive checkpoints / observations

- Removal succeeded on the first requested command.
- Step 3 and step 8 both showed no `t1`.
- No sign-in or onboarding screen appeared during window A.
- `x1-1`, `x1-2`, and `x3` all said `account t1 is not signed in`.
- `x3` did not switch to `· account default` or another account, but it retained `Model provider: OpenAI · account t1` and an `Account usage: t1` row.
- `[observe] Q1`: `• afterone` appeared by the 30-second check, and `• aftertwo` appeared by its 30-second check. Both model turns visibly completed despite the account being removed.
- `x-prov` had no `t1` row.
- `[observe]` vault names after removal: `secrets/.vault.lock` and `secrets/local.age`.
- `auth.json` hash after D136 remained `76b93c8ea88062da141cd6f18335bfd54428e3dde6be0eab89d90403e6be3400`.
- `config.toml` after D136 still had no `[provider_accounts]` and no `openai = "t1"` entry.

### Expected-result assessment

- Step 3 no `t1`: passed.
- Window A never sign-in/onboarding: passed.
- By x3 names `t1` as not signed in: passed.
- Recovery path by x3: failed; no `corbanu login --account t1`, `corbanu account add`, `/providers`, or `--account` text appeared in x1-2 or x3. The later x-prov screen offers add/replace actions but is after the case’s “by x3 at latest” limit.
- x3 not healthy/default: passed with ambiguity: it marks t1 not signed in and does not display default, but still displays provider/account t1 and t1 usage.
- afterone/aftertwo: observed.
- x-prov no t1 and final list no t1: passed.
- No OpenAI API request carrying odefault: for the integrator.
- ChatGPT traffic at afterone/aftertwo: for the integrator.
- auth.json unchanged and config unchanged: passed.
- Vault file names: observed.
- D136 VERDICT: failed.

## ST-03

- Home `oai1`, after D136; `t1` was not touched or restored.

### Actions requested and actual

| ST-03 requested | Actual |
|---|---|
| 1. `START(oai1)` | w11, 22:29:29Z; `gpt-5.6-luna` matched at 22:29:31Z; screen `st03-start-default`. |
| 1. `STATUS2(y1)` | First at 22:29:38Z, screen `st03-y1-1` at 22:29:41Z; second at 22:29:46Z, screen `st03-y1-2`. Both complete. |
| 2. `SEND(yone, 60)` | Sent 22:29:53Z; 30s check `st03-yone-30s` at 22:30:23Z already showed a 401 error and no `• yone`; 60s end screen `st03-yone-60s-end` at 22:31:05Z. |
| 3. `STATUS(y2)` | 22:31:12Z; screen `st03-y2` at 22:31:15Z. Complete. |
| 3. `QUIT` | 22:31:15Z; exited code 0; terminal modes `icanon isig echo icrnl opost`; screen `st03-quit`. |
| 4. `CLI(oai1, account list)` | w12 at 22:31:22Z; exited 0 by 22:31:24Z: “No named accounts. Every provider uses its default account.” Screen `st03-acct-final`. `agms-tui stop` run. |

### Positive checkpoints / observations

- `y1-1` and `y1-2` had no `Model provider:` row and no `Account usage:` row. They showed `Account: OpenAI API key configured (run corbanu login to use ChatGPT)`.
- `y2` likewise had no `Model provider:` row or `Account usage:` row.
- Step 2 showed `⚠ OpenAI (openai) credential was rejected. Open /providers, select OpenAI, and press r to recover. Other providers are unchanged.` and `■ unexpected status 401 Unauthorized: Incorrect API key provided (test relay)., url: wss://api.openai.com/v1/responses`. No `• yone` appeared.
- `[advisory]` The error did not name `t1`.
- Step 4 showed no `t1`.

### Expected-result assessment

- No provider/account-usage row in y1-2 and y2: passed.
- y1-1: observed; it also had neither row.
- Step 2 error and no reply: passed.
- Error does not name t1: passed.
- Step 4 no t1: passed.
- Relay: `yone` to api.openai.com carrying odefault with 401: for the integrator.
- ST-03 VERDICT: passed.

## Final verdicts

VERDICT NA-13: passed
VERDICT D136: failed
VERDICT ST-03: passed

No human acceptance is claimed. Relay-related expectations and file/provider-account facts not directly visible were marked for the integrator or separately checked only from permitted home metadata/config as allowed by the case.
