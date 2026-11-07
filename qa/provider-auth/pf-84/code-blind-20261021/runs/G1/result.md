# G1 functional execution report

UTC started: 2026-10-21T22:37:30Z

## Preflight and identity

- Coordinator preflight files present and inspected: `preflight-summary.json`, `preflight-launcher.json`, `preflight-tmux-child.json`; all report `"ok": true`, `failed: []`.
- Executor ran `agms-probe all > $AGMS_RUN/evidence/preflight-executor.json; echo rc=$?` at approximately 22:36Z. It exited rc=0 and reported `"ok": true`.
- `agms-tui homes` showed only prepared home `zai3` for this group.
- Candidate SHA-256: `bbae7d37dff9023268efdd05a8a4408ccf144ee51ba911afd39adc0e9ddce1b8` — matched expected.
- Frozen cases SHA-256: `cfad42f543b6782a70b935b0596ac50737468733f3480629f49089e34eb6e5b4` — matched expected.
- `agms-probe all` positive check observed `corbanu 0.1.48`.

No model declines or timeouts yet.

## NA-01 — CLI list/add/remove; keys only from stdin; no secrets (Blocker)

Requested actions: run the nine literal CLI/CLIK actions through `agms-tui`, using window names `w1` onward, then stop.

Actual: executed as requested, with windows `w1`–`w9`, then `agms-tui stop`. `agms-tui` recorded every screen and action.

### NA-01 observations and results

- Step 1, `account list`: start 22:38:58, exit visible by 22:38:59; screen `n1-step1`; exit 0. Listed Z.AI `personal` and `work`, both with `api_key`.
  - Result: passed. `[observe]`: `default` was not listed; rows showed kind `api_key`; no fingerprint was shown.
- Step 2, CLIK `account add zai extra` with `agms-fake-zextra-58213`: start 22:38:07. Screen `n1-step2-pre` showed the command already exited before the key was typed: `Error: pipe the secret in instead of typing it, for example \`cat key.txt | corbanu account add <provider> <name>\``. Exit 1. The requested text action and Enter still occurred as required by CLIK, after exit; screen `n1-step2` shows the tester's typed key on the post-exit terminal line. This is the permitted NO-SECRET exception for the tester's own typed text.
  - Result: failed. The candidate did not wait for stdin, did not confirm `extra`, and exited before the CLIK key action.
- Step 3, `account add zai extra2 agms-fake-zextratwo-61407`: start 22:38:22, exit 22:38:24; screen `n1-step3`; exit 2 with `error: unexpected argument 'agms-fake-zextratwo-61407' found`.
  - Result: passed for non-zero/no `extra2`. `[observe]`: the error repeated the key given on the command line.
- Step 4, `account list`: start 22:38:30, exit 22:38:32; screen `n1-step4`; exit 0; `extra` absent.
  - Result: passed.
- Step 5, `login status`: start 22:38:39, exit 22:38:41; screen `n1-step5`; exit 1; output was `Not logged in`.
  - Result: passed for no `agms-fake`; `[observe]`: wording shown and exit code 1.
- Step 6, `exec --account extra "...xone..."`: start 22:38:49, exit 22:38:51; screen `n1-step6`; exit 1. Error named `extra` of provider `zai`, gave `corbanu account add zai extra`, and mentioned `--account`. No `xone`.
  - Result: failed. Expected exit 0 and `xone`, but this followed directly from failed step 2.
- Step 7, `account remove zai extra`: start 22:38:57, exit 22:38:59; screen `n1-step7`; exit 1; `Error: zai has no account \`extra\``.
  - Result: failed. Expected exit 0 after successful add.
- Step 8, `account list`: start 22:39:02, exit 22:39:04; screen `n1-step8`; exit 0; `extra` absent.
  - Result: passed.
- Step 9, `exec --account extra "...xtwo..."`: start 22:39:10, exit 22:39:12; screen `n1-step9`; exit 1. Same account-not-configured error, with all three required recovery elements. No `xtwo` reply.
  - Result: passed for non-zero/no reply/error content. Relay timing for the integrator: no request expected between 22:39:10 and 22:39:12. The expected `xone` request would have been between 22:38:49 and 22:38:51 but no model reply occurred.

### NA-01 expected-result judgments

- Step 1 listing: passed; default/kind/fingerprint observed.
- Step 2: failed.
- Step 3: passed; key echoed in error observed.
- Step 4 no `extra2`: passed.
- Step 4 lists `extra`: failed.
- Step 5 no `agms-fake`: passed; wording/exit observed.
- Step 6: failed.
- Step 7: failed.
- Step 8 no `extra`: passed.
- Step 9: passed for all directly visible expectations; relay “Nothing at xtwo” is for the integrator.
- NO-SECRET(`zextra`, 58213): passed; only the permitted typed-text line in `n1-step2` contained it.
- Relay expectations (`xone` → `zextra`, nothing at `xtwo`, no `zextratwo` request): for the integrator, with logged windows above.

### NA-01 verdict

VERDICT NA-01: failed

## NA-02 — Choosing the account for `exec` and the TUI (Blocker)

Requested actions: run four `exec` forms, then start TUI `--account zai:work`, run `/status`, ask `efive`, and quit; use windows `w10`–`w14`; log times; stop.

Actual: executed as requested, then `agms-tui stop`. `agms-tui` recorded every screen and action.

### NA-02 observations and results

- Step 1, `exec --account work`: start 22:39:48, exit 22:39:55; screen `n2-step1`; exit 0; output contained `eone`.
  - Result: passed.
- Step 2, `exec --account zai:work`: start 22:40:01, exit 22:40:07; screen `n2-step2`; exit 0; output contained `etwo`.
  - Result: passed.
- Step 3, `exec`: start 22:40:13, exit 22:40:20; screen `n2-step3`; exit 0; output contained `ethree`.
  - Result: passed.
- Step 4, `exec --account default`: start 22:40:26, exit 22:40:31; screen `n2-step4`; exit 0; output contained `efour`.
  - Result: passed.
- Step 5 TUI: start 22:40:34; `glm-5.3-flash` visible by 22:40:36; screen `n2-start`. `/status` at 22:40:39; screen `n2-a` showed complete box, Session `01a1609f-9fe4-7322-a1bb-c70f96f5ddc7`, provider row ending `· account work`.
  - Result: passed.
- `efive`: sent 22:40:52; positive `• efive` was already visible at the first 30-second check; done/screen label `n2-efive` at 22:41:01. No timeout or prompt.
  - Result: passed.
- QUIT: started 22:41:04, exit 0 by 22:41:05; screen `n2-quit`; terminal settings normal (`icanon isig echo icrnl opost`).
- Positive checkpoints: each CLI word visible in its labeled screen; `n2-a` provider/account line; `n2-efive` reply line.

### NA-02 expected-result judgments

- Steps 1–4 exits 0 and words: passed.
- `n2-a` shows `· account work`: passed.
- Relay account attribution for `eone`, `etwo`, `ethree`, `efour`, and `efive`: for the integrator, using the UTC windows above.

### NA-02 verdict

VERDICT NA-02: passed

## NA-03 — A missing account fails closed and says how to recover (Blocker)

Requested actions: two TUI refusals, two missing-account `exec` calls, a config edit to point `[provider_accounts] zai` at `ghost`, one TUI start and one `exec` after that edit, then a successful `--account work` TUI turn, restore config, and stop.

Actual: executed literally, windows `w15`–`w20`, then `agms-tui stop`. No cancellation, recovery screen, password prompt, or unexpected screen appeared.

### NA-03 observations and results

- Step 1, REFUSE `--account ghost`: start 22:41:27; `n3-1-2s`; exit visible before 30 s window; end 22:41:32; screen `n3-1`; exit 1; terminal settings normal.
  - Result: passed. Text named `ghost`/provider `zai`, gave `corbanu account add zai ghost`, and mentioned `--account`. No resume picker, session, or onboarding.
- Step 2, `exec --account ghost`: start 22:41:38, end 22:41:40; screen `n3-2`; exit 1; same required recovery text. No `gone`.
  - Result: passed.
- Step 3, `exec --account zai:ghost`: start 22:41:43, end 22:41:45; screen `n3-3`; exit 1; same required recovery text. No `gtwo`.
  - Result: passed.
- Step 4, config edit: read before at 22:41:50, changed and read after at 22:41:50. Before/after output confirms `[provider_accounts] zai` changed from `"personal"` to `"ghost"`.
  - Result: edit succeeded.
- Step 5, REFUSE with no args: start 22:41:56; `n3-5-2s`; exit visible before 30 s window; end 22:42:00; screen `n3-5`; exit 1; terminal settings normal.
  - Result: passed. Same required recovery text. No resume picker, session, or onboarding.
- Step 6, `exec` after edit: start 22:42:06, end 22:42:08; screen `n3-6`; exit 1; same required recovery text. No `gthree`.
  - Result: passed.
- Step 7, START `--account work`: start 22:42:14; model visible by 22:42:16; screen `n3-start`. `gfour` sent 22:42:20; reply `• gfour` visible before the first 30-second check; done/screen `n3-gfour` at 22:42:28. `/status` screen `n3-a` showed complete box, Session `01a160a1-2662-7ce0-9e6e-ad8e8f3847fe`, provider row `· account work`.
  - Result: passed.
- QUIT: started 22:42:36 and exited 0 immediately; screen `n3-quit`; normal settings.
- Step 8 restore: sed applied and config read at 22:42:36; output confirms `zai = "personal"`.
- Step 9: `agms-tui stop`.
- Positive checkpoints: labeled refusal screens and recovery text; `n3-a` provider/account and `n3-gfour` reply.

### NA-03 expected-result judgments

- Steps 1, 2, 3, 5, and 6: passed for exit status and all three required text elements.
- No reply word printed and NO-ONBOARD in all captures including 2-second screens: passed.
- Steps 1 and 5 show no resume picker and no session: passed.
- `n3-a` shows `· account work`: passed.
- Relay: for the integrator. Logged windows: step 1 22:41:27–22:41:32; `gone` 22:41:38–22:41:40; `gtwo` 22:41:43–22:41:45; step 5 22:41:56–22:42:00; `gthree` 22:42:06–22:42:08; `gfour` 22:42:20–22:42:28.

### NA-03 verdict

VERDICT NA-03: passed

## NA-04 — A rejected key names the account; no fallback (Blocker)

Requested actions: add account `badone` through stdin, then `exec` and TUI use with that account; `/status`, quit, config read, and stop.

Actual: step 1 refused stdin and exited before the key could be consumed. The literal text/Enter actions were still performed as required. Step 2 then failed as an unconfigured account. The TUI start also refused; the required model/session wait timed out after 60 seconds, so the frozen START rule required skipping the session-dependent steps. Config was read and all terminals stopped.

### NA-04 observations and results

- Step 1, CLIK `account add zai badone` with `agms-fake-badkey-77302`: start 22:42:59; screen `n4-step1-pre` showed it already exited with `Error: pipe the secret in instead of typing it, for example \`cat key.txt | corbanu account add <provider> <name>\``. Exit 1. Text action and Enter occurred at 22:43:04 after exit; screen `n4-step1` shows the tester's typed key on the post-exit terminal line, the permitted NO-SECRET exception.
  - Result: failed. Candidate did not wait for stdin and did not confirm `badone`.
- Step 2, `exec --account badone`: start 22:43:11, exit 22:43:13; screen `n4-step2`; exit 1. Error named `badone` of provider `zai`, gave `corbanu account add zai badone`, and mentioned `--account`. No `bone` reply.
  - Result: passed for the directly visible non-zero/no-reply/recovery expectations; however, this was the missing-account error, not the intended rejected-key/401 path.
- Step 3, START `--account badone`: start 22:43:20; screen `n4-start`; it immediately showed the same account-not-configured error and exited 1. The required `glm-5.3-flash` wait timed out at 22:44:22 (60 seconds). No session was created.
  - Result: failed for this case's intended rejected-key TUI path; no session existed.
- Step 4, SEND `btwo`: not reached, because no session existed. No `• btwo` appeared.
- Step 5, STATUS `n4-a`: not reached, because no session existed. Thus the expected `· account badone` display was not exercised.
- Config read at 22:44:32: `[provider_accounts] zai = "personal"`, as required.
- `agms-tui stop` ended the run.
- NO-ONBOARD: held in every captured screen, including `n4-start`.
- NO-SECRET(`badkey`, 77302): held except for the permitted tester-typed post-exit line in `n4-step1`.

### NA-04 expected-result judgments

- Step 1 exits 0 and confirms: failed.
- Step 2 exits non-zero, no `bone`, error names/recovery: passed directly, but intended rejected-key state was not reached.
- Step 4 error naming `badone`, no reply, NO-ONBOARD: not exercised / no session.
- `n4-a` shows `· account badone`: not exercised.
- NO-SECRET: passed.
- Config retains `zai = "personal"`: passed.
- Relay: for the integrator. No request appears likely during step 2's 22:43:11–22:43:13 window, while the case expected a 401 carrying `badkey`. `btwo` was not sent.

### NA-04 verdict

VERDICT NA-04: failed

## Run summary and completion

- Home used throughout: `$AGMS_RUN/homes/zai3`.
- All case actions were driven only through `agms-tui`; evidence labels are listed above and are retained by `agms-tui`.
- No system password prompt, unexpected real-account sign-in, unexpected outside-folder access, or lost observation occurred.
- No product-model refusal or decline was observed. The only waits without success were NA-04's TUI model/session wait after a missing-account refusal.
- NA-04's START timeout and skipped session-dependent steps followed the frozen case conventions.

VERDICT NA-01: failed
VERDICT NA-02: passed
VERDICT NA-03: passed
VERDICT NA-04: failed
