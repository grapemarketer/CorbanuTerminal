# G6R functional execution report (UTC 2026-10-22)

## Preflight
- Coordinator files inspected: `$AGMS_RUN/evidence/preflight-summary.json`, `preflight-launcher.json`, `preflight-tmux-child.json`. The launcher and tmux-child files each reported `"ok": true`.
- `agms-probe all` completed rc=0 at 2026-10-22T00:55:46Z and reported `"ok": true`; summary also showed `launcher_ok`, `tmux_child_ok`, and `pty_input_echoed` true with no failed probes.
- SHA-256 `/opt/agms/pkg/bin/corbanu` = `bbae7d37dff9023268efdd05a8a4408ccf144ee51ba911afd39adc0e9ddce1b8` (expected: match).
- SHA-256 `/opt/agms/packet/design-cases.md` = `cfad42f543b6782a70b935b0596ac50737468733f3480629f49089e34eb6e5b4` (expected: match).
- Prepared home listed: `zainokey`. No product source/history/credentials were read.

## Case NA-11 — completed
- Home: `$AGMS_RUN/homes/zainokey` as prepared; `ZAI_API_KEY` was not passed.
- Requested actions: START(zainokey, `--account work`), ASK(kaone), STATUS(ka1), log SID `<kid>`, QUIT; START resume `<kid>`, STATUS(ka2), ASK(katwo), QUIT; CLI exec `kathree`; stop.
- Actual actions:
  - 00:57:28 START w1 (`--account work`). 00:57:35 session visible. 00:57:38 screen `ka-start`; no onboarding.
  - 00:57:46 typed kaone prompt; 00:57:52 pressed Enter. 00:57:55 reply `• kaone` appeared; 00:58:05 screen `ka-reply`.
  - 00:58:09 typed `/status`; 00:58:15 Enter. 00:58:24 screen `ka1`: complete status; provider row `Z.AI - https://api.z.ai/api/paas/v4 · account work`; SID `01a1611c-f4b6-7ee2-8220-43d824963b0c` (`<kid>`); Account usage `work`.
  - 00:58:30 typed `/quit`; 00:58:35 Enter; 00:58:38 exited code 0. 00:58:44 screen `ka1-quit`.
  - 00:58:51 START w2 `resume <kid>`. 00:58:58 session visible; 00:59:04 screen `ka2-start`; transcript shows kaone; no onboarding.
  - 00:59:07 typed `/status`; 00:59:13 Enter. 00:59:20 screen `ka2`: same SID and `· account work`.
  - 00:59:26 typed katwo prompt; 00:59:30 Enter. 00:59:38 reply `• katwo`; 00:59:40 screen `ka2-reply`.
  - 00:59:43 typed `/quit`; 00:59:49 Enter; 00:59:55 exited code 0.
  - 01:00:01 START w3 CLI `exec "reply with the single word kathree"`; 01:00:09 exited code 1; 01:00:15 screen `w3-final`; 01:00:15 `agms-tui stop`.
- Positive checkpoints:
  - `ka1` and `ka2` each show `· account work` and a complete `Session:` row; NO-ONBOARD holds in all step 1–2 captures.
  - Replies `• kaone` and `• katwo` visible.
  - Step 3 exit is code 1; no `kathree` reply. Error wording verbatim (first occurrence): `ERROR: Missing environment variable: \`ZAI_API_KEY\`. Run \`/providers\` and select the matching provider key:` followed by a Providers key-management list.
- Relay results: for the integrator. `kaone` 00:57:46–00:57:55; `katwo` 00:59:26–00:59:38; expected both `zwork`. `kathree` 01:00:01–01:00:09, expected no `zwork`; whether a keyless request was made is for the integrator.
- Expected-result judgments:
  - NO-ONBOARD steps 1–2: passed.
  - ka1/ka2 `· account work`: passed.
  - Step 3 non-zero/no reply: passed.
  - Relay assertions: for the integrator.

## Case ESC-01 — completed
- Home: `$AGMS_RUN/homes/zainokey` after NA-11.
- Requested actions: START resume picker, capture e1, Escape, capture e2 within 10 s, type `echo restored 9` + Enter, capture e3, stop.
- Actual actions:
  - 01:00:37 START w4. 01:00:44 picker visible. 01:00:48 screen `e1`.
  - 01:00:53 pressed Escape. 01:00:59 product-exit text appeared. 01:01:06 screen `e2`.
  - 01:01:12 typed `echo restored 9`; 01:01:22 pressed Enter. 01:01:25 screen `e3`; 01:01:25 `agms-tui stop`.
- Positive checkpoints:
  - `e1` shows the `Resume a previous session` picker with one session and footer `enter resume   esc quit   ctrl+c quit`; no onboarding.
  - `e2` shows `[agms] product exited with code 0` and `[agms] terminal settings left: icanon isig echo icrnl opost`; no leading `-` modes; no onboarding.
- Observations:
  - `e3` shows `echo restored 9` **twice**, each on its own line, with no picker frame or `^[` garbage. No shell prompt is visible.
- Expected-result judgments:
  - e1 picker: passed.
  - NO-ONBOARD e1/e2: passed.
  - Footer `esc quit`: passed.
  - e2 code 0 within 10 s and normal terminal settings: passed.
  - e3 shows `echo restored 9` once: failed (it appears twice).

## Case I426-A — completed
- Home: `$AGMS_RUN/homes/zainokey` after NA-11; `<kid>` = `01a1611c-f4b6-7ee2-8220-43d824963b0c`.
- Requested actions: three REFUSE starts with ghost-account resume variants, capture each at 2 s and after exit, then stop.
- Actual actions:
  - 01:02:00 START w5 `resume --account ghost <kid>`; 01:02:10 screen `ia1-2s`; 01:02:16 exited; 01:02:19 screen `ia1`.
  - 01:02:22 START w6 `resume --account zai:ghost <kid>`; 01:02:32 screen `ia2-2s`; 01:02:38 exited; 01:02:44 screen `ia2`.
  - 01:02:48 START w7 `resume --account ghost --last`; 01:02:57 screen `ia3-2s`; 01:03:03 exited; 01:03:09 screen `ia3`; 01:03:09 `agms-tui stop`.
- Positive checkpoints:
  - All three final screens show exit code 1 and normal terminal settings (`icanon isig echo icrnl opost`).
  - Every capture, including each 2 s screen, shows only the ghost-account error and no onboarding, key screen, or picker. NO-ONBOARD holds.
  - Error wording visible on all three, with screen wrap after `corbanu accoun`: `account \`ghost\` of provider \`zai\` is not configured; add it with \`corbanu account add zai ghost\` or pick another with \`--account\` (see \`corbanu accoun` / `t list\`)`.
- Relay results: for the integrator; expected no requests during steps 1–3 (01:02:00–01:03:09).
- Expected-result judgments:
  - Each starts exits non-zero with required ghost/provider/add/account wording: passed (for all three).
  - No onboarding/key screen/picker; NO-ONBOARD: passed.
  - Relay assertion: for the integrator.

## Cross-case notes
- No system password prompt, unexpected sign-in, unexpected outside access, missing required control, lost observation, or budget exhaustion occurred.
- No reply or refusal from the product's model was observed. The `kathree` error occurred before a model turn and is recorded separately.

## Final verdicts
VERDICT NA-11: passed
VERDICT ESC-01: failed
VERDICT I426-A: passed
