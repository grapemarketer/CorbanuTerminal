# G2 functional executor report

## Preflight and identity

- UTC begin: 2026-10-21T22:50:34Z.
- `agms-probe all` exited 0; saved as `evidence/preflight-executor.json`; its top-level `"ok": true`.
- Coordinator artifacts present beside it: `preflight-summary.json` (`ok: true`, launcher_ok, tmux_child_ok, pty_input_echoed, failed=[]), plus `preflight-launcher.json` (`ok: true`) and `preflight-tmux-child.json` (`ok: true`).
- SHA-256 `/opt/agms/pkg/bin/corbanu`: `bbae7d37dff9023268efdd05a8a4408ccf144ee51ba911afd39adc0e9ddce1b8` (expected match).
- SHA-256 `/opt/agms/packet/design-cases.md`: `cfad42f543b6782a70b935b0596ac50737468733f3480629f49089e34eb6e5b4` (expected match).
- Prepared homes: `zai3` listed by `agms-tui homes`.
- No source, history, old findings, credentials, relay log, or non-packet screens were read.

## PR-01 — Full precedence order

### Window mapping and timing

- w1 = PR-01 setup TUI (`zai3 --account work`).

### Setup w1

- Before 22:50:48Z: requested/actual `agms-tui --window w1 start --home zai3 --account work`; `wait glm-5.3-flash` positive at 22:50:48Z; startup screen showed v0.1.48 and composer.
- 22:50:48Z: `wait glm-5.3-flash` positive; startup screen showed v0.1.48 and composer. (The harness command and check completed immediately after logging began; no prompt appeared.)
- 22:50:53Z: w sent (`pzero` text typed separately); 22:50:55Z Enter.
- 22:51:00Z: `wait • pzero` positive; screen showed `• pzero`.
- 22:51:03Z: `/status` text; 22:51:04Z Enter; 22:51:07Z `p0` complete.
- p0 positive: `Session: 01a160a8-f714-7183-8051-6eb5ee4a0f2f`; provider row `· account work`; account usage `work` only. `<id>` = `01a160a8-f714-7183-8051-6eb5ee4a0f2f`.
- 22:51:16Z QUIT: exited code 0; terminal settings `icanon isig echo icrnl opost` (order as displayed, no leading minus).

### Step 2 — resume forced personal (w2)

- 22:51:19Z: requested/actual `w2 start --home zai3 resume --account personal <id>`; wait positive at 22:51:22Z; transcript showed `pzero`.
- 22:51:29Z `p1-pre`: `· account personal`; usage table had `personal (active) 0...` and `work 8,595/21/16`.
- 22:51:33Z w sent (`pone` text); 22:51:35Z Enter; 22:51:40Z `wait • pone` positive.
- 22:51:50Z `p1`: `· account personal`; usage table: `personal (active) 8,560 / 64 cached / 17 / 14`, `work 8,595 / 0 cached / 21 / 16`. This matches the expected table.
- 22:51:52Z QUIT: exited code 0; terminal settings normal as above.

### Step 4 — new TUI with provider_accounts still naming personal (w4)

- 22:52:51Z: requested/actual `w4 start --home zai3`; wait positive at 22:52:53Z.
- 22:53:03Z `p3-pre`: `· account personal`; new Session `01a160aa-de34-7e61-bed8-36b0db48d016`.
- 22:53:09Z w sent (`pthree` text); 22:53:11Z Enter; 22:53:16Z `wait • pthree` positive.
- 22:53:26Z `p3`: `· account personal`; Account usage one `personal` row. Matches expected.
- 22:53:27Z QUIT: exited code 0; terminal settings normal as above.

### Step 5 — exact config edit

- 22:53:34Z: requested/actual sed removal command. `cat` output saved in session and confirmed no `provider_accounts` section. Edit succeeded.

### Step 6 — new TUI without provider_accounts (w5)

- 22:53:41Z: requested/actual `w5 start --home zai3`; wait positive at 22:53:43Z.
- 22:53:53Z `p4-pre`: `· account default`; Session `01a160ab-9e40-7423-981d-56681e59f541`.
- 22:53:57Z w sent (`pfour` text); 22:53:58Z Enter; 22:54:03Z `wait • pfour` positive.
- 22:54:14Z `p4`: `· account default`; Account usage one `default` row. Matches expected.
- 22:54:16Z QUIT: exited code 0; terminal settings normal as above. 22:54:16Z `agms-tui stop` reported `stopped`.

### PR-01 expected-result assessment

- p0 work: **passed**.
- p1 personal and table: **passed**.
- p1-pre personal: **passed**.
- p2 work and p2-pre work: **failed**; both showed `personal`.
- p2 expected usage table itself appeared, but under the wrong active account.
- p3 personal and p3-pre personal: **passed**.
- p4 default and p4-pre default: **passed**.
- Relay assignments (`pzero`, `pone`, `ptwo`, `pthree`, `pfour`) and Files first-turn/later-resume recordings: **for the integrator**; UTC request intervals are in timing above.
- No model refusal occurred.
- Overall case executable, with one judged failure.

## GH-02 — Recorded work removed while provider_accounts names ghost

### Window mapping and timing

- Continues `zai3` after PR-01. New windows only.

### Step 1 — append ghost provider preference

- 22:54:35Z: requested/actual append `\n[provider_accounts]\nzai = "ghost"\n`; `cat` confirmed the section and `zai = "ghost"`.

### Step 2 — resume after ghost provider preference (w6)

- 22:54:38Z: requested/actual `w6 start --home zai3 resume <id>`; wait positive at 22:54:42Z; transcript showed `pzero`, `pone`, `ptwo`.
- 22:54:52Z `g1`: **failed expected `work` checkpoint**—provider row showed `· account personal`. Session remained `<id>`.
- 22:54:58Z w sent (`gaone` text); 22:55:00Z Enter; 22:55:05Z `wait • gaone` positive.
- 22:55:10Z QUIT: exited code 0; terminal settings normal as above.

### Step 3 — remove work (w7)

- 22:55:14Z: requested/actual `w7 start --home zai3 account remove zai work`; product printed `Removed zai account \`work\`.` and exited code 0 at 22:55:16Z. Positive checkpoint.

### Step 4 — REFUSE resume (w8)

- 22:55:24Z: requested/actual `w8 start --home zai3 resume <id>`.
- 22:55:26Z `g4-2s`: product had started a full TUI session showing `<id>` transcript and composer; no refusal.
- 22:55:59Z `wait product exited 30` timed out (`wait_rc=2`), confirming it did not exit as expected.
- **Requested fallback (`screen`, Escape, `screen`, C-c, C-c, wait, screen) was not executed.** The preceding shell command then called `agms-tui stop`, which stopped the terminal while the product was still running. No fallback screen or forced-exit screen was captured. This is a tester deviation; preserve partial observation.
- No non-zero refusal text matching the case was observed. The product instead resumed a session.

### GH-02 expected-result assessment

- g1 `· account work`: **failed** (actual `personal`).
- Step 3 exit 0: **passed**.
- Step 4 non-zero and expected recovery text: **failed**; product did not exit within 30 seconds and showed a session.
- Advisory mention of `ghost`: **not exercised** in a refusal screen.
- NO-ONBOARD/no picker/no session: **failed** for step 4 because a session was shown. No onboarding wording observed.
- Relay `gaone`, and nothing at step 4: **for the integrator**. Note step 4 did not exit; integrator should check the interval 22:55:24–22:55:59Z.
- No model refusal or decline occurred.
- Overall: executable, judged failed.

### Tester deviation recorded

- At 22:58Z, I noticed the draft report had been created at `<tool-workdir>/evidence/result.md`, outside `$AGMS_RUN/evidence`. I moved that file to `$AGMS_RUN/evidence/result.md` and removed the empty outside evidence directory. No product or test data outside that file was written intentionally; subsequent edits target `$AGMS_RUN/evidence/result.md`.

## NA-05 — In-session /resume when conversation account is gone

### Window mapping and timing

- w9 = NA-05 TUI (`zai3 --account personal`).

### Step 1

- 22:56:26Z: requested/actual `w9 start --home zai3 --account personal`; wait positive at 22:56:28Z.
- 22:56:31Z w sent (`rone` text); 22:56:32Z Enter; 22:56:38Z `wait • rone` positive.
- 22:56:45Z `r1`: `· account personal`; Session/SID1 = `01a160ae-2567-7932-bef2-91a2662230ee`; transcript showed `rone`.

### Step 2

- 22:56:52Z: `/resume` text; 22:56:53Z Enter; 22:56:56Z screen showed the `Resume a previous session` picker with `rone` highlighted.
- 22:57:02Z after typing `pzero`: search was `pzero` and the single highlighted result was `reply with the single word pzero`.

### Step 3

- 22:57:05Z: pressed Enter on the highlighted `pzero` row.

### Step 4

- 22:57:10Z `r-msg`: **failed expected recovery-message checkpoint.** The screen showed the banner, the old `pzero`/`pone`/`ptwo`/`gaone` transcript, a token summary for SID1, and a composer. No message that `work` was not configured and no recovery advice was visible.

### Step 5

- 22:57:23Z: `/status` text; 22:57:24Z Enter; 22:57:27Z `r2`: **failed stay-put checkpoint.** Session changed to old `<id>` = `01a160a8-f714-7183-8051-6eb5ee4a0f2f`, not SID1. Provider row showed `· account personal`.
- 22:57:36Z w sent (`rtwo` text); 22:57:38Z Enter; 22:57:43Z `wait • rtwo` positive.
- 22:57:48Z QUIT: exited code 0; terminal settings normal as above.
- 22:57:51Z `agms-tui stop`: `stopped`.

### NA-05 expected-result assessment

- Expected not-configured/recovery message: **failed**; no such message in `r-msg`.
- Session stays on SID1: **failed**; `r2` showed old `<id>`.
- `rone` remains visible: **passed** in the pre-resume transcript, but not decisive after session switch.
- `r2` shows `personal`: **passed**, but with the wrong SID.
- NO-ONBOARD: **passed**; no listed onboarding wording appeared.
- Relay `rtwo` and nothing on `zenv`: **for the integrator**; timing above.
- No model refusal or decline occurred.
- Overall: executable, judged failed.

## General observations

- No system password prompt, real-account sign-in screen, unexpected outside access, lost observation, or budget exhaustion occurred.
- No model reply declined any request; all `p*`, `gaone`, `rone`, and `rtwo` requests produced the requested visible word.
- Product windows were driven through `agms-tui`; prompt text and Enter were separate in every request/status/quit action.
- Relay-related outcomes and Files outcomes are for the integrator because the relay log and encrypted/session internals were not read.
- This report records functional observations only and does not claim human acceptance.

## Final verdicts

VERDICT PR-01: failed
VERDICT GH-02: failed
VERDICT NA-05: failed
