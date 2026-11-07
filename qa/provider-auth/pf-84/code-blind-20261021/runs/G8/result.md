# G8 independent functional execution report

- Run began UTC 2026-10-22T00:33:39Z.
- Executor `agms-probe all` completed with rc 0 and `"ok": true`; saved as `preflight-executor.json`.
- Coordinator `preflight-summary.json`: launcher_ok true, tmux_child_ok true, pty_input_echoed true, failed [], probe_count 97, ok true. Both `preflight-launcher.json` and `preflight-tmux-child.json` have `"ok": true`.
- Binary SHA-256: `bbae7d37dff9023268efdd05a8a4408ccf144ee51ba911afd39adc0e9ddce1b8` (expected).
- Frozen cases SHA-256: `cfad42f543b6782a70b935b0596ac50737468733f3480629f49089e34eb6e5b4` (expected).
- Prepared homes listed: `zai3off` (`as zai3, named_accounts = false`) and `zainokeyoff` (`as zainokey, named_accounts = false`).
- Product identity through packaged launch observed in preflight: `corbanu 0.1.48`.

Status: complete. Case outcomes are REG-01a passed, REG-01b passed, REG-01c passed, REG-01d blocked. Final verification/report time 00:45:39Z.

## REG-01a

- Home: `zai3off`.
- Requested: START; STATUS o0; ASK oone; STATUS2 o1; `/fork`; ASK otwo; STATUS o2; QUIT; CLI exec `--account work`; CLI account list; stop.
- Actual:
  - START window w1 at 00:36:02; session appeared by 00:36:04 (o0-start). Visible warning: "`[provider_accounts]` is ignored because the `named_accounts` feature is off; every provider uses its default account."
  - o0 at 00:36:19: complete status; provider line exactly `Z.AI - https://api.z.ai/api/paas/v4`; no `· account` suffix; no `Account usage:` row. Session `01a16109-5442-7411-ba91-e98acec3267e`.
  - `oone` sent 00:36:26, done 00:36:36; visible bullet `• oone`.
  - o1-1 00:36:47 and o1-2 00:36:55: both plain; no account suffix or Account usage row.
  - `/fork` at 00:37:07: visible `• Thread forked from 01a16109-5442-7411-ba91-e98acec3267e`.
  - `otwo` sent 00:37:07, done 00:37:09; visible bullet `• otwo`.
  - o2 at 00:37:18: plain status, with `Forked from: 01a16109-5442-7411-ba91-e98acec3267e`; no account suffix or Account usage row. Fork session `01a1610a-3f5f-7212-81d0-2b82b3c4ead4`.
  - QUIT began 00:37:22; exited by 00:37:23 with code 0; settings `icanon isig echo icrnl opost`.
  - Step 3 at 00:37:29–00:37:31: CLI refused `--account zai:work` because named accounts need `named_accounts`; exit code 1.
  - Step 4 at 00:37:38–00:37:41: CLI `account list` refused because named accounts are off; exit code 1. Stop at 00:37:41.
- Positive checkpoints: o0, o1-1, o1-2, o2 all plain; o2 has `Forked from`; `/fork` visible.
- Expected-result outcomes:
  - Plain status before/after and o2 Forked from: passed.
  - Advisory wording versus REF-01/REF-11: observed compatible in structure; no exact wording failure.
  - Results steps 3 and 4: observed refusals as quoted above.
  - Relay oone/otwo/oexec: for the integrator.
- No product model refusal/decline occurred.

VERDICT REG-01a: passed

## REG-01b

- Home: `zai3off`.
- Requested: START with `--enable named_accounts --account work`; ASK wone; STATUS w0; SID; QUIT; REFUSE resume SID; START resume with `--account zai:default`; ASK wtwo; STATUS w2; QUIT; stop.
- Actual:
  - START window w4 at 00:38:15; session appeared by 00:38:18; fallback config edit was not needed.
  - `wone` sent 00:38:25, done 00:38:31; visible bullet `• wone`.
  - w0 at 00:38:35: provider line ends `· account work`; Account usage row `work · 8,587 input · 18 output (14 reasoning)`; SID `01a1610b-5e6b-7f22-b281-86a3e7d8269b`.
  - QUIT began 00:38:45 and exited code 0.
  - REFUSE window w5 at 00:38:45; 2 s screen and final screen at 00:38:49 show the quoted error; exit code 1; settings normal. Error text: `ERROR: Could not resume session 01a1610b-5e6b-7f22-b281-86a3e7d8269b: failed to load configuration: the thread's recorded account zai:work: named accounts need the \`named_accounts\` feature (\`--enable named_accounts\`); \`--account zai:default\` (on a resume or fork) uses the default credentials instead`.
  - Resume window w6 at 00:38:57; session by 00:38:59; transcript visibly includes `› reply with the single word wone` and `• wone`.
  - `wtwo` sent 00:39:07, done 00:39:12; visible bullet `• wtwo`.
  - w2 at 00:39:16: plain; provider line has no `· account`; no Account usage row; same SID.
  - QUIT began 00:39:27 and exited code 0. Stop at 00:39:27.
- Positive checkpoints: w0 `· account work`; refusal names work/provider and `--account zai:default`; resume transcript wone; w2 plain.
- Expected-result outcomes:
  - Precondition and resume behavior: passed.
  - Relay wone/step2/wtwo: for the integrator.
  - File `named_accounts = false`: for the integrator (no config edit was performed).
- No product model refusal/decline occurred; the CLI refusal quoted above is product navigation/validation, not a model reply.

VERDICT REG-01b: passed

## REG-01c

- Home: `zai3off`, after prior cases.
- Requested: CLI resume picker; Escape; START and ASK hone; STATUS rc1; route search; route 2; if no picker, `/resume`; Escape from picker; STATUS rc2; QUIT; stop.
- Actual:
  - Window w7 START at 00:39:53; picker visible by 00:39:55 in r1. Footer was `enter resume   esc start new   ctrl+c quit ...`; three saved threads listed. NO-ONBOARD and no account wording visible.
  - Escape at 00:40:03; r2 at 00:40:06 started a new session, did not exit. The new session showed the feature-off warning and normal composer. It was then `/quit` by 00:40:16, exit code 0.
  - Window w8 START at 00:40:22. `hone` sent 00:40:24, done 00:40:31; visible `• hone`.
  - rc1 at 00:40:34: plain status, SIDh `01a1610d-4955-79b0-951d-d83ca4854f0b`.
  - Route search 00:40:42: `?` showed shortcuts (`/`, `!`, `ctrl + r search history`, etc.); no shortcut for opening resume picker. BSpace cleared `?`.
  - Route 2 at 00:40:51 produced `/resume keep draft 271`; Enter at 00:40:53 gave `■ No saved chat found matching 'keep draft 271'.`, not the picker. Observation: in this build `C-a` placed the cursor at line start but did not clear the existing draft, so the literal route produced a search string combining the draft with `/resume`.
  - No-route fallback: cleared composer, `/resume` at 00:41:03 reached the picker by 00:41:07; footer now `enter resume   esc exit   ctrl+c exit ...`.
  - Escape from picker at 00:41:14; rc-after at 00:41:16 returned to composer; no draft text was visible. The draft observation is therefore observed as absent for the no-route path.
  - rc2 at 00:41:27: plain status and SIDh unchanged. Earlier screen `hone` at 00:40:31 establishes the session transcript contained `hone`.
  - QUIT began 00:41:56 and exited code 0; stop at 00:41:57.
- Positive checkpoints: r1 picker; picker escape returns to composer; rc2 plain with SIDh.
- Expected-result outcomes:
  - r1 picker, no onboarding/account wording: passed.
  - Escape behavior: observed (started new session from first picker; second picker exit returned to composer).
  - Exit-specific checks: not exercised (Escape did not exit either time).
  - rc2 SIDh/plain/transcript: passed.
  - Draft rule: observed as above.
  - Advisory picker wording: observed; differs from REF-06 footer in candidate (`esc start new` before starting a session vs `esc quit` in REF-06; `esc exit` in fallback picker).
- No product model refusal/decline occurred.

VERDICT REG-01c: passed

## REG-01d

- Home: `zainokeyoff`.
- Requested: START resume; screen d1; Escape; d2; C-c twice; wait exit; d3; clean echo; d4; stop.
- Actual:
  - Window w9 START at 00:44:30. d1 at 00:44:42 showed onboarding: `Welcome to Corbanu Terminal`, `Use your Z.AI API key`, key prompt `Paste your ZAI_API_KEY API key`, and the API-key field. No account wording or "not configured" error visible.
  - Escape at 00:44:54. d2 at 00:44:56 showed the Z.AI onboarding provider screen with `Connect a Z.AI API key to use Corbanu Terminal` and `> 1. Provide your Z.AI API key`. No account wording or "not configured" error visible.
  - First `C-c` at 00:44:57 was followed by `tmux error: no server running ...`; the terminal disappeared before the second C-c could be observed or d3/d4 captured. The wait returned rc 1 without a `product exited` checkpoint.
  - Observation was lost; per the run rules, the case was stopped at 00:45:15. d3 and d4 were not exercised.
- Positive checkpoints: d1 and d2 show onboarding and no account wording / not-configured error.
- Expected-result outcomes:
  - d1 onboarding, no account wording: passed (advisory case).
  - d2 no account wording / no not-configured error: passed (advisory case).
  - d3 exit/settings and d4 clean echo: blocked due lost observation after C-c.
- No product model refusal/decline occurred.

VERDICT REG-01d: blocked
