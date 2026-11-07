# G3 independent functional execution report

Run start: 2026-10-21T23:05:26Z. No human acceptance claimed.

## Isolation and identity preflight
- Requested: coordinator preflight files, executor {
 "context": {
  "mode": "all",
  "uid": 611,
  "pid": 4054,
  "ppid": 4053,
  "run": "<RUN>",
  "utc": "2026-10-21T23:05:26Z"
 },
 "ok": true,
 "probes": [
  {
   "group": "repo",
   "probe": "list canary checkout",
   "expect": "EACCES",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "repo",
   "probe": "read canary source file",
   "expect": "EACCES/EPERM",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "repo",
   "probe": "list canary .git",
   "expect": "EACCES",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "repo",
   "probe": "list home /Users/agent",
   "expect": "EACCES",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "repo",
   "probe": "list home /Users/neo",
   "expect": "EACCES",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "repo",
   "probe": "list home /Users/neo_1",
   "expect": "EACCES",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "history",
   "probe": "read canary prior findings",
   "expect": "EACCES/EPERM",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "history",
   "probe": "read coordinator product history db",
   "expect": "EACCES/EPERM",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "history",
   "probe": "list prior case runs",
   "expect": "EACCES",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "symlink",
   "probe": "read canary credential via symlink",
   "expect": "EACCES/EPERM",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "symlink",
   "probe": "open real transport key via symlink (no read)",
   "expect": "EACCES/EPERM",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "credentials",
   "probe": "read canary credential",
   "expect": "EACCES/EPERM",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "credentials",
   "probe": "open real transport key (no read)",
   "expect": "EACCES/EPERM",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "credentials",
   "probe": "open real auth.json (no read)",
   "expect": "EACCES/EPERM",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "credentials",
   "probe": "open mediator leaf key (no read)",
   "expect": "EACCES/EPERM",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "credentials",
   "probe": "open campaign restore state (no read)",
   "expect": "EACCES/EPERM",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "credentials",
   "probe": "list Homebrew tools (PATH removal)",
   "expect": "EACCES",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "credentials",
   "probe": "environment credential names",
   "expect": "only synthetic placeholders",
   "observed": {
    "names": [],
    "all_placeholder": true
   },
   "ok": true
  },
  {
   "group": "ipc",
   "probe": "connect coordinator canary tmux socket",
   "expect": "EACCES",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "ipc",
   "probe": "signal coordinator canary process",
   "expect": "EPERM",
   "observed": "EPERM",
   "ok": true
  },
  {
   "group": "ipc",
   "probe": "list other lane home agmx0",
   "expect": "EACCES",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "ipc",
   "probe": "list other lane home agmx2",
   "expect": "EACCES",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "ipc",
   "probe": "list other lane home agmx3",
   "expect": "EACCES",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "ipc",
   "probe": "connect unrelated local database socket",
   "expect": "EACCES or absent",
   "observed": "ENOENT",
   "ok": true
  },
  {
   "group": "network",
   "probe": "direct TCP to inference IP bypassing mediator",
   "expect": "pf drop (timeout)",
   "observed": "timeout after 4.0s",
   "ok": true
  },
  {
   "group": "network",
   "probe": "direct TCP to OpenAI IP",
   "expect": "pf drop (timeout)",
   "observed": "timeout after 4.0s",
   "ok": true
  },
  {
   "group": "network",
   "probe": "TCP to host SSH",
   "expect": "pf drop (timeout)",
   "observed": "timeout after 4.0s",
   "ok": true
  },
  {
   "group": "network",
   "probe": "loopback unrelated database",
   "expect": "pf drop (timeout)",
   "observed": "timeout after 4.0s",
   "ok": true
  },
  {
   "group": "network",
   "probe": "loopback resolver",
   "expect": "pf drop (timeout)",
   "observed": "timeout after 4.0s",
   "ok": true
  },
  {
   "group": "network",
   "probe": "loopback tunnel to host Z.AI relay",
   "expect": "pf drop (timeout)",
   "observed": "timeout after 4.0s",
   "ok": true
  },
  {
   "group": "network",
   "probe": "loopback tunnel to host local-model gate",
   "expect": "pf drop (timeout)",
   "observed": "timeout after 4.0s",
   "ok": true
  },
  {
   "group": "network",
   "probe": "resolve github.com",
   "expect": "resolution failure",
   "observed": "gaierror 8",
   "ok": true
  },
  {
   "group": "network",
   "probe": "mediator CONNECT github.com:443",
   "expect": "403",
   "observed": "HTTP/1.1 403 Forbidden",
   "ok": true
  },
  {
   "group": "network",
   "probe": "mediator CONNECT raw.githubusercontent.com:443",
   "expect": "403",
   "observed": "HTTP/1.1 403 Forbidden",
   "ok": true
  },
  {
   "group": "network",
   "probe": "mediator CONNECT codeload.github.com:443",
   "expect": "403",
   "observed": "HTTP/1.1 403 Forbidden",
   "ok": true
  },
  {
   "group": "network",
   "probe": "mediator non-inference path GET /",
   "expect": "403",
   "observed": "HTTP/1.1 403 Forbidden",
   "ok": true
  },
  {
   "group": "network",
   "probe": "mediator CONNECT qwen.agms.test:443",
   "expect": "403",
   "observed": "HTTP/1.1 403 Forbidden",
   "ok": true
  },
  {
   "group": "network",
   "probe": "mediator CONNECT qwen.agms.test:80",
   "expect": "403",
   "observed": "HTTP/1.1 403 Forbidden",
   "ok": true
  },
  {
   "group": "network",
   "probe": "mediator CONNECT 127.0.0.1:18602",
   "expect": "403",
   "observed": "HTTP/1.1 403 Forbidden",
   "ok": true
  },
  {
   "group": "network",
   "probe": "mediator CONNECT 127.0.0.1:18601",
   "expect": "403",
   "observed": "HTTP/1.1 403 Forbidden",
   "ok": true
  },
  {
   "group": "package",
   "probe": "open package binary for write",
   "expect": "EACCES/EPERM",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "package",
   "probe": "chmod package binary",
   "expect": "EPERM",
   "observed": "EPERM",
   "ok": true
  },
  {
   "group": "package",
   "probe": "create file in package dir",
   "expect": "EACCES",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "package",
   "probe": "open frozen packet for write",
   "expect": "EACCES/EPERM",
   "observed": "EACCES",
   "ok": true
  },
  {
   "group": "child",
   "probe": "sudo -n",
   "expect": "refused",
   "observed": {
    "rc": 1,
    "stderr": "sudo: a password is required"
   },
   "ok": true
  },
  {
   "group": "child",
   "probe": "uid",
   "expect": "unprivileged run uid",
   "observed": {
    "uid": 611,
    "euid": 611
   },
   "ok": true
  },
  {
   "group": "positive",
   "probe": "packet readable and frozen",
   "expect": "cfad42f543b6782a70b935b0596ac50737468733f3480629f49089e34eb6e5b4",
   "observed": "cfad42f543b6782a70b935b0596ac50737468733f3480629f49089e34eb6e5b4",
   "ok": true
  },
  {
   "group": "positive",
   "probe": "package binary digest",
   "expect": "bbae7d37dff9023268efdd05a8a4408ccf144ee51ba911afd39adc0e9ddce1b8",
   "observed": "bbae7d37dff9023268efdd05a8a4408ccf144ee51ba911afd39adc0e9ddce1b8",
   "ok": true
  },
  {
   "group": "positive",
   "probe": "package launch --version",
   "expect": "rc 0",
   "observed": {
    "rc": 0,
    "out": "corbanu 0.1.48"
   },
   "ok": true
  },
  {
   "group": "positive",
   "probe": "private evidence dir writable",
   "expect": "writable",
   "observed": true,
   "ok": true
  },
  {
   "group": "child",
   "probe": "grandchild via /bin/sh repeats all negatives",
   "expect": "all denied",
   "observed": {
    "ok": true,
    "pid": 4316,
    "failed": []
   },
   "ok": true
  }
 ]
}, package and frozen-case SHA-256 comparison.
- Actual: `agms-probe all` exited 0 and reported `"ok": true`; output saved at `<RUN>/evidence/preflight-executor.json` (UTC 2026-10-21T23:03:52Z context). Coordinator preflight summary also `ok: true`.
- SHA-256: package `bbae7d37dff9023268efdd05a8a4408ccf144ee51ba911afd39adc0e9ddce1b8`; design cases `cfad42f543b6782a70b935b0596ac50737468733f3480629f49089e34eb6e5b4`. Both matched required values.
- Prepared home `zai3` was listed. No source, history, old findings, credentials, or relay log accessed.

## NA-06: /providers rows, adding with a key, rename
- Home check: `agms-tui homes` listed `zai3` as prepared. Requested and actual actions are recorded below.
- 23:06:28 requested START(zai3); actual window w1 started. 23:06:30 wait matched `glm-5.3-flash`; screen `n6-start` showed v0.1.48.
- 23:06:37 requested `/providers` and Enter; screen `n6-list` showed the providers list with `Z.AI · default` and `Z.AI · personal`; highlighted OpenAI.
- 23:06:45–23:06:47 pressed Down six times with screen after each (`n6-nav1`–`n6-nav6`); highlighted row `Z.AI · default`.
- 23:06:47 pressed Enter to open Z.AI; 23:06:49 `n6-zai` showed only Z.AI provider-level management actions, not account rows. In the immediately preceding `n6-list`, `Z.AI · personal` was shown as `Named account · API key · fp ce7a29d31a8a · this session · default for new sessions`; no separate `work` row appeared on either screen. The `default` row's status was `Enabled · configured · current (status of account personal)`; no kind or fingerprint was shown for `work`. [advisory] marker wording was as quoted.
- 23:07:01 selected action `3. Add another account`, pressed Enter. 23:07:03 screen `n6-add-prompt` requested `Account name`.
- 23:07:10 typed `added`, pressed Enter; screen showed key entry. 23:07:19 typed `agms-fake-zadded-40958`, captured `n6-key` (typed key visibly unmasked in the field), pressed Enter. After 2 seconds `n6-after` still showed the add form and key; after 5 more seconds `n6-after-late` (23:07:46) was unchanged; no `added` row or completion appeared.
- 23:07:57 pressed the Enter shown by the screen hint (`Press enter to confirm`); screen `n6-after-second-enter` returned to the providers list with `invalid account name: use 1-32 lowercase letters, digits or '-', starting with a letter or digit`, despite the typed name `added`. No `added` account was created.
- Consequence: the requested move to an `added` row and Rename action was not reached. Continuing only to observe the remaining independent product commands/screens; dependent account-presence checks are judged accordingly.
- 23:08:38 Escape returned to composer, but the prior add-form validation error remained visible. `/quit` exited with code 0; `n6-quit` showed normal terminal settings (`icanon isig echo icrnl opost`).
- 23:08:45–23:08:47 CLI `account list` in w2 exited 0; it listed only `personal` and `work`, not `renamed` or `added`.
- 23:08:51–23:08:53 CLI `exec --account renamed ... nren` in w3 exited 1 with a not-configured error naming `renamed`, provider `zai`, `corbanu account add zai renamed`, and `--account`; no `nren` reply.
- 23:08:57–23:08:59 CLI `exec --account added ... nold` in w4 exited 1 with a not-configured error naming `added`, provider `zai`, `corbanu account add zai added`, and `--account`; no `nold` reply.
- 23:09 UTC stopped all windows (`agms-tui stop`).
- No system password prompt, real-account sign-in, unexpected external access, missing required control, or lost observation occurred. The model did not issue any refusal/decline; all failures were local CLI/UI behavior.

### NA-06 expected-result outcomes
- `n6-zai` shows `work` and `personal` with kind and fingerprint: **failed** — `personal` showed kind/fingerprint, but no `work` row was present.
- `personal` carries current-session and default markers: **passed** — screen text quoted above.
- Default row contents: **observed** — default status was shown, without a kind or fingerprint.
- Marker wording: **observed** — `this session` and `default for new sessions`.
- `n6-after` shows `added` with kind/fingerprint: **failed** — add form remained; second Enter produced invalid-account-name and no account row.
- `n6-renamed` shows `renamed`: **not exercised** — no `added` row existed.
- Key entry visibility: **observed** — typed synthetic key was visible in the field; it remained visible on later captures after Enter.
- NO-SECRET for zadded key after entry: **failed** — the synthetic key remained visible in `n6-after`/`n6-after-late` rather than being masked or cleared.
- Step 5 list shows `renamed` and not `added`: **failed** — neither was listed.
- Step 6 exits 0 and prints `nren`: **failed** — exit 1, not-configured error, no reply.
- Step 7 exits nonzero with not-configured text for `added`: **passed** — required account/provider/recovery/`--account` elements appeared.
- Relay `nren` -> `zadded`: **for the integrator** — request window 23:08:51–23:08:53 UTC.
- Nothing at `nold`: **for the integrator** — request window 23:08:57–23:08:59 UTC.

VERDICT NA-06: failed

## ESC-03: Esc in the in-session /resume picker
- 23:09:31–23:09:34 START(zai3, `--account personal`) in w5. Footer showed `zai default`; no `/status` was requested at this step. 23:09:41–23:09:47 ASK(cprev): sent, `• cprev` appeared.
- 23:09:56 cprev session quit code 0 (`esc-setup-quit`), settings normal.
- 23:09:56–23:09:58 START(zai3, `--account personal`) in w6. Footer again showed `zai default`.
- 23:10:01–23:10:06 ASK(cone): sent, `• cone` appeared.
- 23:10:13 STATUS c1 complete; `Session:` `01a160ba-8095-7cb3-a1cd-f049a0de93c1` (SIDc); provider line ended `· account personal`; account usage `personal`.
- 23:10:17 route search: typed `?`, screen showed command shortcut help; no shortcut for opening the resume picker or preserving draft. BSpace cleared the `?`.
- Route 2 requested: typed `keep draft 314`, C-a, typed `/resume `. Screen showed `/resume keep draft 314`. 23:10:28 Enter did not open a picker; it ran a search and showed `No saved chat found matching 'keep draft 314'.`
- Logged: **no route** from the draft to the picker. The no-route fallback then cleared the composer, sent `/resume`, and 23:10:36 opened the actual `Resume a previous session` picker.
- 23:10:39 pressed Escape; `c-after` returned to the same session composer; no fork/new-session banner or onboarding text appeared.
- 23:10:51 and 23:10:59 STATUS c2-1/c2-2 both showed SIDc and `· account personal`; transcript still showed `cone`; no "Thread forked".
- 23:11 UTC quit/stopped. No model refusal/decline occurred.

### ESC-03 expected-result outcomes
- c2 shows SIDc and `· account personal`: **passed**.
- Transcript still shows `cone`: **passed**.
- No fork/new-session banner: **passed**.
- NO-ONBOARD: **passed** — no onboarding phrases appeared in captured screens.
- Route 1 draft preservation: **not exercised** — no route-1 shortcut was found.
- Route 2 composer after picker Escape: **observed** — composer was empty after the no-route fallback; the attempted draft route executed `/resume keep draft 314` as a saved-chat search and did not reach the picker.
- No-route finding: **observed** — logged as no draft-preserving route.

VERDICT ESC-03: passed

## NA-07: Use for this session, make default, and removing the account in use
- 23:11:24–23:11:26 START(zai3) in window A = w7.
- 23:11:29 STATUS u0 complete: `Session:` `01a160bb-d89a-7471-b662-74b026986685`; provider line ended `· account personal`. This satisfied the u0 checkpoint.
- 23:11:38 requested `/providers`; `n7-list` showed `Z.AI · default` and `Z.AI · personal`, but no `Z.AI · work` row.
- 23:11:38–23:11:42 pressed Down through the visible provider rows with a screen after each (`n7-zai-nav1`–`n7-zai-nav6`) and pressed Enter on `Z.AI · default`. `n7-zai` offered only Deactivate, Replace with API key, Add another account, Use default account for this session, and Make `default` the default for new sessions. It did not offer or list `work`, so the requested action to select `work` and choose "Use for this session" was missing.
- Because a required control was missing, the case was stopped. At 23:12:09 the attempted `/quit` did not exit: screen `n7-blocked-quit` unexpectedly showed a `Choose replacement` dialog listing Ollama and LM Studio. `agms-tui stop` then ended all windows; no exit code was obtained. No later steps were attempted.
- Correction: this replacement prompt was the actual stop-path observation; no replacement was selected, and the window was force-stopped through `agms-tui stop`.
- No system password prompt, real-account sign-in, unexpected external access, or lost observation occurred. No model request/refusal was involved in the blocked step.

### NA-07 expected-result outcomes
- u0 shows `· account personal`: **passed**.
- u1 shows `· account work`: **blocked/not reached** — `work` selection was unavailable.
- Step 3 Make default and config observation: **not reached**.
- Step 4 new session default and `utwo`: **not reached**.
- Step 5 replacement-before-removal behavior, u3 and `uthree`: **not reached**.
- Step 6 `ufour`/u4 behavior: **not reached**.
- Step 7 account list and config: **not reached**.
- Relay mappings for uone/utwo/uthree/ufour: **for the integrator / not reached**.
- The missing `work` UI row is the blocking root observation; product account list before ESC-03 had shown `work`, so this is an ambiguity in the candidate's provider-screen representation rather than evidence that the home lacked the account.

VERDICT NA-07: blocked

## Group summary
- VERDICT NA-06: failed
- VERDICT ESC-03: passed
- VERDICT NA-07: blocked
- Run end: 2026-10-21T23:12:09Z wall-clock execution ended; all agms-tui windows were stopped. No human acceptance is claimed.
