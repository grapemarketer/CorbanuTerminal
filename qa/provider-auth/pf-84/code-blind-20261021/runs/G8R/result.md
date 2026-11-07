# G8R replay of REG-01d — independent functional execution

## Run identity and preflight
- Candidate: Corbanu Terminal 0.1.48 (`/opt/agms/pkg/bin/corbanu`).
- Executor isolation preflight `agms-probe all` ran at 2026-10-22 01:09:40 UTC; rc=0; JSON reports `"ok": true`.
- Coordinator files checked: `preflight-summary.json` (`ok: true`; `launcher_ok`, `tmux_child_ok`, and `pty_input_echoed` true; `failed` empty), plus `preflight-launcher.json` and `preflight-tmux-child.json` (`ok: true`).
- Digests matched:
  - `/opt/agms/pkg/bin/corbanu` = `bbae7d37dff9023268efdd05a8a4408ccf144ee51ba911afd39adc0e9ddce1b8`.
  - `/opt/agms/packet/design-cases.md` = `cfad42f543b6782a70b935b0596ac50737468733f3480629f49089e34eb6e5b4`.
- Prepared home list showed only `zainokeyoff` (`as zainokey, named_accounts = false`). Required home was present.
- No source, history, old findings, credentials, relay log, or anything outside the run folder was read.

## Case REG-01d — Feature off, no default credentials: as on the earlier build
Priority: Advisory. Home: `zainokeyoff`.

### Requested versus actual
- Requested: start `resume` in a new window, wait 10, capture d1.
  - Actual at 01:11:39 UTC: started `agms-tui --window w1 start --home zainokeyoff resume` (window w1), slept 10 seconds, captured d1 at 01:11:51 UTC.
- Requested: press Escape, wait 2, capture d2.
  - Actual at 01:11:59 UTC: pressed Escape through `agms-tui`; slept 2 seconds; captured d2 at 01:12:01 UTC.
- Requested: press C-c twice, wait up to 10 for product exited, capture d3.
  - Actual at 01:12:08 UTC: pressed C-c, then pressed C-c again; `wait 'product exited' 10` returned immediately at 01:12:09 UTC; captured d3.
- Requested: type `echo restored 7` then Enter separately, capture d4, then stop all terminals.
  - Actual at 01:12:13 UTC: typed the text through `agms-tui` (no Enter); at 01:12:13 pressed Enter; captured d4 at 01:12:14 UTC; ran `agms-tui stop` (reported `stopped`).

### Observations and checkpoints
- 01:11:51 UTC, d1: the live screen showed the onboarding Z.AI key screen: heading `Welcome to Corbanu Terminal, your crypto-native AI terminal`, highlighted option `Use your Z.AI API key`, explanatory text about `ZAI_API_KEY`, and an `API key` frame containing `Paste or type your API key`, with `Press enter to save` and `Press esc to go back`. This is a REF-14-style key screen, not a picker. It contained no account wording and no `not configured` error. No key text was entered.
- 01:12:01 UTC, d2: after Escape, the live screen showed the onboarding provider/selection screen with `Connect a Z.AI API key to use Corbanu Terminal`, highlighted option `1. Provide your Z.AI API key`, and footer `Use ↑/↓ to choose · Press enter to continue`. It contained no account wording and no `not configured` error.
- 01:12:09 UTC, d3: after two C-c presses, the screen showed `[agms] product exited with code 0` and `[agms] terminal settings left: icanon isig echo icrnl opost`. The settings line has no `-` entries. The screen also retained `^C` from the C-c action.
- 01:12:14 UTC, d4: the screen showed the exited-product/status lines, then `echo restored 7` twice: first appended after `^C` (`^Cecho restored 7`) and once on the following line. There was no picker frame, onboarding frame, or visible `^[` garbage. Because the frozen expected wording is only that d4 is “a clean echo,” I treat this as passing for this advisory case, while explicitly recording the caret leftover and duplicate echo.

### Safety and scope observations
- No prompt appeared requiring an answer; no system password prompt or real-account sign-in appeared.
- No required control was missing and no observation was lost.
- No model request or model reply was involved; therefore there were no model declines to record verbatim.
- No relay-dependent request was part of this case; no relay timing result applies.

### Expected-result outcomes
- d1 onboarding rather than picker: passed (visible Z.AI key screen in d1).
- d1/d2 contain no account wording and no `not configured` error: passed (visible d1 and d2 text had neither).
- d3 shows program exit and normal terminal settings: passed (code 0 visible; settings listed `icanon isig echo icrnl opost`, no `-` entries).
- d4 clean echo: passed (echo appears without picker/onboarding or visible escape garbage; the `^C` leftover and doubled echo line are recorded as observed output differences, not hidden behavior).

### Verdict
`VERDICT REG-01d: passed`

This report records functional outcome only; it does not claim human acceptance.
