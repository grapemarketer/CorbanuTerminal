# PF-84 functional executor report — group S1, case D136

Run started: 2026-10-22T08:42:26Z. Case ended: 2026-10-22T08:48:41Z. Tester: independent functional executor, code-blind. This report does not claim human acceptance.

Home prepared and used: `oai1`, located at `$AGMS_RUN/homes/oai1`. Coordinator sign-in marker was missing, so `t1` was treated as signed in before the run as directed.

## Isolation and identity preflight

- Requested: run `agms-probe all`, require `"ok": true`, and hash the packaged binary and frozen cases file.
- Actual: `agms-probe all > $AGMS_RUN/evidence/preflight-executor.json; echo rc=$?` completed with `rc=0`. Its JSON reports `"ok": true`.
- Coordinator summary `preflight-summary.json` also reports `"ok": true`, with `launcher_ok: true`, `tmux_child_ok: true`, `pty_input_echoed: true`, `failed: []`, and 97 probes. Both `preflight-*.json` sidecars reported `ok: true`.
- SHA-256 `/opt/agms/pkg/bin/corbanu` = `ba9feaa443ec18f29abc73a27137cc8f4696899ea4a1c3dada84c8d2df280006` — matched the expected candidate identity.
- SHA-256 `/opt/agms/packet/design-cases.md` = `cfad42f543b6782a70b935b0596ac50737468733f3480629f49089e34eb6e5b4` — matched.
- SHA-256 amendment v2 = `fe1324914155fc4d37a83a4ac3bd7bdd0ac67f636cdf31e1da0cfb7c6a0f7e77` — matched.
- SHA-256 amendment v3 = `ff365c23e584d67d678af2aed535904416af408805de5ab4d903a116d148dee4` — matched.
- Outcome: preflight and candidate identity checks passed; proceed with assigned D136 only.

## D136 — requested versus actual actions

Window mapping: window A = `w1`. Subsequent windows were `w2`, `w3`, and `w4`, used only as specified. All navigation used displayed TUI controls; prompt text and Enter were separate `agms-tui` actions. Screens and actions were captured by `agms-tui` in `actions.jsonl` and `screens.jsonl`.

| Step | Requested | Actual and UTC |
|---|---|---|
| 0 | List evidence and stop if `t1-not-signed-in` exists. | 2026-10-22T08:42:26Z: listing saved to `d136-step0-ls.txt`. The marker did not exist. Continued. Evidence: `d136-signin-check.txt`. |
| 1 START | Start `oai1 --account t1` in window A, wait for `gpt-5.6-luna`, screen. | 2026-10-22T08:43:00Z: `w1` showed Corbanu Terminal v0.1.48, model `gpt-5.6-luna`, composer. Wait succeeded. Screen: `d136-w1-start`. |
| 1 ASK | Send `reply with the single word beforeone` and Enter; wait up to 90s for `• beforeone`; screen. | Text sent at 2026-10-22T08:43:10Z; Enter pressed separately at the same second. `• beforeone` appeared by 2026-10-22T08:43:11Z. Screen: `d136-beforeone`. |
| 1 STATUS | `/status`, wait/observe, label `x0`. | 2026-10-22T08:43:22Z: complete `x0` status box appeared with Session `01a162c7-2076-71c3-a563-4223cc2562b6`, provider row `Model provider: OpenAI · account t1`, and Account usage `t1 · 13,193 input · 6 output`. Screen: `d136-x0`. |
| 2 CLI remove | `account remove openai t1`, log time. | Start 2026-10-22T08:43:31Z; end 2026-10-22T08:43:33Z. `w2` exited code 0 and printed: “Signed out and removed openai account `t1`.” Terminal settings: `icanon isig echo icrnl opost`. Screen: `d136-w2-remove`. |
| 3 CLI list | `account list`. | Start 2026-10-22T08:43:40Z; end 2026-10-22T08:43:42Z. `w3` exited code 0 and showed: “No named accounts. Every provider uses its default account.” No `t1`. Screen: `d136-w3-list-after-remove`. |
| 4 STATUS2 | In window A, log start of no-use window, sleep 10, then STATUS `x1-1`, sleep 5, STATUS `x1-2`. | No-use start 2026-10-22T08:43:50Z. `x1-1` captured at 2026-10-22T08:44:03Z: this first status still showed healthy `· account t1`. `x1-2` captured at 2026-10-22T08:44:12Z: provider row changed to `OpenAI · account unavailable`; Account row said `account t1 has no sign-in: `corbanu login --account t1` or `/providers``; Account usage still showed `t1 · 13,193 input · 6 output`. Screens: `d136-x1-1`, `d136-x1-2`. |
| 5 SEND afterone | Send `afterone` prompt and Enter, wait/sleep 90 seconds, screen. | Sent and Enter at 2026-10-22T08:44:28Z. Checked at 30s (08:44:58), 60s (08:45:28), and end (08:45:58). No `• afterone`. Screen showed fatal error naming `t1`, recovery `corbanu login --account t1`, and alternatives `/providers` and `--account`. Screens: `d136-afterone-30s`, `d136-afterone-60s`, `d136-afterone`. |
| 5 SEND aftertwo | Send `aftertwo` prompt and Enter, wait/sleep 90 seconds, screen. | Sent and Enter at 2026-10-22T08:46:07Z. Checked at 30s (08:46:37), 60s (08:47:07), and end (08:47:37). No `• aftertwo`; the same fatal error appeared again. Screens: `d136-aftertwo-30s`, `d136-aftertwo-60s`, `d136-aftertwo`. |
| 6 STATUS | `/status`, label `x3`. | 2026-10-22T08:47:49Z: complete status box showed `OpenAI · account unavailable`; Account row gave `corbanu login --account t1` and `/providers`; Account usage still listed `t1 · 13,193 input · 6 output`. Screen: `d136-x3`. |
| 7 PROVIDERS | `/providers`, open OpenAI, screen `x-prov`, Escape to composer, QUIT, log no-use end. | 08:47:49–08:48:31Z. `/providers` showed OpenAI highlighted: “OpenAI Enabled · configured · current”, with no `t1` row. Enter opened OpenAI actions: Deactivate, Replace with OpenAI account, Replace with API key, Add another account, Start a new session on the default account; no `t1` row. Screen: `d136-x-prov`. Escape returned to provider list (`d136-providers-escape1`), second Escape returned to composer (`d136-providers-composer`). `/quit` exited code 0. No-use end 2026-10-22T08:48:31Z. Screen: `d136-w1-quit`; settings `icanon isig echo icrnl opost`. |
| 8 final list and stop | `account list`, then stop all terminals. | Start 2026-10-22T08:48:38Z; end 2026-10-22T08:48:41Z. `w4` exited code 0 and again showed only “No named accounts. Every provider uses its default account.” No `t1`. Screen: `d136-w4-list-final`. `agms-tui stop` completed at 2026-10-22T08:48:41Z. |

No system password prompt, unexpected outside-folder access, missing required control, sign-in screen for a live account, lost observation, or exhausted budget occurred.

## Positive visible checkpoints

- 2026-10-22T08:43:00Z — `w1` loaded with model `gpt-5.6-luna` (`d136-w1-start`).
- 2026-10-22T08:43:11Z — visible `• beforeone` (`d136-beforeone`).
- 2026-10-22T08:43:22Z — complete `x0` box with `OpenAI · account t1` (`d136-x0`).
- 2026-10-22T08:43:33Z — removal command printed successful sign-out/removal and exited 0 (`d136-w2-remove`).
- 2026-10-22T08:43:42Z — `account list` had no `t1` (`d136-w3-list-after-remove`).
- 2026-10-22T08:44:12Z — `x1-2` showed `account unavailable` (`d136-x1-2`).
- 2026-10-22T08:45:58Z — `afterone` ended with no model reply and the expected unavailable-account recovery text (`d136-afterone`).
- 2026-10-22T08:47:37Z — `aftertwo` ended with no model reply and the same recovery text (`d136-aftertwo`).
- 2026-10-22T08:47:49Z — `x3` showed unavailable account, recovery and preserved `t1` usage (`d136-x3`).
- 2026-10-22T08:48:31Z — TUI quit cleanly with code 0 (`d136-w1-quit`).
- 2026-10-22T08:48:41Z — final list had no `t1`, then all terminals stopped (`d136-w4-list-final`).

## Product model refusal / decline text (verbatim)

For `afterone` and again for `aftertwo`, no bullet reply appeared. The screen showed this fatal error (line wrap as displayed, continuation indented):

```text
■ Fatal error: OpenAI: account `t1` has no sign-in (it may have been removed or signed out); sign in with `corbanu login --account t1` (add `--with-
api-key` for an API key), or pick another account (`/providers` in the TUI, `--account` on the command line)
```

## Outcome by expected result

- **Precondition** (`• beforeone` and `x0` shows `· account t1`): **passed** — both were visible.
- **Step 3 shows no `t1`**: **passed** — only the no-named-accounts message appeared.
- **Window A never shows a sign-in or onboarding screen (NO-ONBOARD)**: **passed** — no captured screen showed the prohibited onboarding/sign-in headings or controls.
- **No `• afterone` / no `• aftertwo`; both requests refused**: **passed** — neither bullet appeared; both produced the same fatal unavailable-account error.
- **Error text names `t1` as no sign-in/removed/signed out, gives `corbanu login --account t1`, and mentions `/providers` and `--account`**: **passed** — visible by `d136-afterone-30s` and at the end of the first SEND.
- **[advisory] Exact documented wording “account `t1` has no sign-in (it may have been removed or signed out)”**: **passed** — the quoted phrase appeared in the fatal error.
- **[observe] Whether the error is shown again at `aftertwo`**: **observed** — yes, the same error appeared.
- **x3 provider line contains “unavailable”**: **passed** — it showed `OpenAI · account unavailable`.
- **[advisory] x3 form `account unavailable`**: **passed**.
- **x3 never shows healthy `· account t1`, `· account default`, or another session account**: **passed**.
- **x3 Account row gives recovery including `corbanu login --account t1`**: **passed**.
- **x3 Account usage still lists `t1` with non-zero counts**: **passed** — `t1 · 13,193 input · 6 output`.
- **[observe] x1-1 and x1-2 may be stale**: **observed** — `x1-1` still showed healthy `t1`; `x1-2` showed unavailable.
- **x-prov has no `t1` row; final list has no `t1`; nothing re-created it**: **passed** from visible screens.
- **Relay: chatgpt.com traffic at `beforeone`**: **for the integrator** — `beforeone` sent 08:43:10Z, done 08:43:11Z.
- **Relay: no api.openai.com request carrying `odefault` from step 1 to end**: **for the integrator** — relevant interval 08:43:10Z through 08:48:41Z.
- **Relay/advisory: no chatgpt.com traffic between no-use start and end**: **for the integrator** — window 08:43:50Z through 08:48:31Z.
- **Files: `auth.json` hash unchanged; config gained no `openai = "t1"`**: **for the integrator** — not inspected by the executor.
- **Files: [observe] vault file names and conversation records per turn**: **for the integrator** — not inspected by the executor.

## Evidence

- Raw PTY captures: `pty-w1.raw`, `pty-w2.raw`, `pty-w3.raw`, `pty-w4.raw`.
- Action and screen logs: `actions.jsonl`, `screens.jsonl`.
- Timestamp notes: `d136-notes.txt`.
- Preflight evidence: `preflight-executor.json`, `preflight-summary.json`, `preflight-launcher.json`, `preflight-tmux-child.json`, `preflight-pty-control.txt`.

## Verdict

D136 was fully executed. Every expected result that could be judged from the live screens passed. Relay and home-file checks remain for the integrator.

VERDICT D136: passed
