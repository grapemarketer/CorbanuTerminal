# G4 functional execution report — actual outcomes only

Execution began: 2026-10-21T23:19:49Z. Tester is code-blind. No human-acceptance claim is made.

## Preflight and identity

- Coordinator files checked: `preflight-summary.json`, `preflight-launcher.json`, `preflight-tmux-child.json`; summary says `ok: true`, launcher and tmux-child pass.
- Executor ran `agms-probe all` at 2026-10-21T23:18:23Z: `ok: true`; rc=0; saved as `preflight-executor.json`.
- `shasum -a 256`:
  - `/opt/agms/pkg/bin/corbanu` = `bbae7d37dff9023268efdd05a8a4408ccf144ee51ba911afd39adc0e9ddce1b8` (match)
  - `/opt/agms/packet/design-cases.md` = `cfad42f543b6782a70b935b0596ac50737468733f3480629f49089e34eb6e5b4` (match)
- Prepared home list showed only `zai3`, matching the assigned cases.
- Window mapping for the group is logged as cases proceed.

# NA-08

Window mapping: main session = `w1`; later `agms-tui stop` ended all terminals at 23:23:04Z.

## Requested versus actual

1. START `zai3 --account work` in `w1`: completed; session visible by 23:20:18Z.
2. ASK `fone`: text and Enter sent separately at 23:20:23–24Z; visible `• fone` by 23:20:30Z. Actual actions matched.
3. STATUS `f0`: completed at 23:20:40Z. SID0 = `01a160c3-f815-70f3-8d98-f8cd4b0e81d4`.
4. `/fork`: text/Enter sent at 23:20:45–46Z; screen captured at 23:20:49Z.
5. ASK `ftwo`: sent 23:20:53–55Z; visible reply by 23:21:00Z.
6. STATUS `f1`: captured at 23:21:07Z.
7. Edit flow: Escape at 23:21:15Z showed `esc again to edit previous message`; second Escape at 23:21:22Z showed transcript controls. `Up` twice scrolled the transcript but the capture exposed no highlighted message. Using the displayed `esc/← to edit prev` control, then Enter at 23:21:58Z, produced `You’re continuing from this point in a new conversation`; composer was empty, not holding `ftwo`. `C-u` showed an empty composer. Continued with `fthree`.
8. ASK `fthree`: sent 23:22:07–08Z; visible reply by 23:22:14Z.
9. STATUS `f2`: captured at 23:22:22Z.
10. `/side`: sent 23:22:25–26Z; footer showed `Side from main thread · ctrl + / to switch · ctrl + c to close` at 23:22:29Z.
11. ASK `fside`: sent 23:22:36–37Z; visible reply by 23:22:43Z.
12. STATUS `f3`: captured at 23:22:50Z.
13. `C-c` at 23:22:59Z returned to the main-thread composer. `/quit` exited code 0; terminal settings line was normal.

## Positive checkpoints

- `f0` at 23:20:40Z: `Model provider:` row ends `· account work`; `Account usage: work · 8,595 input · 28 output (24 reasoning)`.
- `f0-fork` at 23:20:49Z: `Thread forked from 01a160c3-f815-70f3-8d98-f8cd4b0e81d4`.
- `f1` at 23:21:07Z: `Forked from:` SID0; provider row `· account work`; usage `work · 56 input · 8,576 cached input · 22 output (17 reasoning)`.
- `f2` at 23:22:22Z: provider row `· account work`; usage `work · 19 input · 8,576 cached input · 71 output (67 reasoning)`.
- `f-side` at 23:22:29Z: `Side from main thread`.
- `f3` at 23:22:50Z: provider row `· account work`; usage `work · 8,346 input · 8,576 cached input · 93 output (84 reasoning)`.
- No account row other than `work` had non-zero usage in `f0`, `f1`, `f2`, or `f3`.

## Logged relay-relevant times (for the integrator)

- `fone`: sent 23:20:23Z, done 23:20:30Z.
- `ftwo`: sent 23:20:53Z, done 23:21:00Z.
- `fthree`: sent 23:22:07Z, done 23:22:14Z.
- `fside`: sent 23:22:36Z, done 23:22:43Z.
- `/fork` action interval: 23:20:45–49Z.
- Edit-mode continuation point: 23:21:58Z.
- `/side` action interval: 23:22:25–29Z.

## Expected-result outcomes

- f1 shows Forked-from SID0 and `· account work`: **passed**.
- f2 and f3 show `· account work`: **passed**.
- f1–f3 account usage has only a non-zero `work` row: **passed** based on captures.
- Relay labels for the four prompts: **for the integrator**.
- Fork conversation provider/account files: **for the integrator**.
- Edit-mode behavior (`composer empty` and a continuation message rather than retaining `ftwo`): **observed**; all prescribed follow-up actions continued.
- QUIT exited code 0: **passed**.

# NA-09

Window mapping: CLI fork on SID0 (`work`) = `w2`; CLI fork with `--account personal` = `w3`; stopped at 23:25:32Z.

## Requested versus actual

1. START `zai3 fork SID0` in `w2`: completed at 23:23:45Z. No picker appeared; the screen already showed `Thread forked from SID0`.
2. Wait for `glm-5.3-flash`: passed at 23:23:51Z.
3. STATUS `k1`: captured at 23:23:56Z.
4. ASK `kone`: text/Enter sent 23:23:59–23:24:00Z; visible `• kone` by 23:24:06Z.
5. Spawn request: logged `spawn sent` 23:24:12Z; text and Enter sent separately by 23:24:13Z. After one 30-second check at 23:24:43Z, the screen no longer showed `esc to interrupt` or `Working`; it showed spawned/closed child messages and `kidword`. Logged `spawn end` 23:24:49Z and captured `k-spawn`.
6. QUIT `w2`: exited code 0 at 23:24:50Z.
7. START `zai3 fork --account personal SID0` in `w3`: completed at 23:25:06Z. No picker appeared; screen showed `Thread forked from SID0`.
8. Wait for `glm-5.3-flash`: passed at 23:25:09Z.
9. STATUS `k2`: captured at 23:25:14Z.
10. ASK `ktwo`: text/Enter sent 23:25:17–19Z; visible `• ktwo` by 23:25:24Z.
11. QUIT `w3`: exited code 0 at 23:25:31Z. `agms-tui stop` completed 23:25:32Z.

## Positive checkpoints

- `k-start` at 23:23:45Z: `Thread forked from 01a160c3-f815-70f3-8d98-f8cd4b0e81d4`.
- `k1` at 23:23:56Z: `Model provider:` ends `· account work`; `Forked from:` SID0; usage `work · no usage yet`.
- `k-spawn` at 23:24:49Z: child spawned as `glm-5.3-flash max · account work`; child status `Completed - kidword`; final answer `The agent finished and replied with: kidword`.
- `k2` at 23:25:14Z: `Model provider:` ends `· account personal`; `Forked from:` SID0; usage `personal · no usage yet`.
- `k2-after` at 23:25:24Z: visible `• ktwo`.

## Logged relay-relevant times (for the integrator)

- `kone`: sent 23:23:59Z, done 23:24:06Z.
- Spawn: sent 23:24:12Z, end 23:24:49Z. Agent was started.
- `ktwo`: sent 23:25:17Z, done 23:25:24Z.

## Expected-result outcomes

- `k1` shows `· account work`: **passed**.
- `k2` shows `· account personal`: **passed**.
- Agent-start observation: **observed** — an agent was started and completed with `kidword`.
- `k-spawn` final answer contains `kidword`: **passed**.
- Relay labels for `kone`, the spawn interval, and `ktwo`: **for the integrator**.
- Child conversation records provider/account: **for the integrator**.

# NA-10

Window mapping: main session = `w4`; stopped at 23:27:35Z.

## Requested versus actual

1. START `zai3`: completed; model visible by 23:25:54Z.
2. STATUS2 `s0`: `s0-1` captured 23:26:01Z; after five seconds, `s0-2` captured 23:26:16Z.
3. ASK `sone`: text/Enter sent 23:26:20–22Z; visible `• sone` by 23:26:27Z.
4. STATUS `s1`: captured 23:26:34Z.
5. PROVIDERS: `/providers` sent 23:26:38–39Z; list captured as `s-prov-0` at 23:26:42Z. Moved with `Down` and a screen after each press to the first Z.AI row by 23:26:56Z; opened it at 23:27:02Z (`s-zai-open`).
   - The visible rows were `Z.AI · default` and `Z.AI · personal`. No `work` row was offered.
   - The opened default screen offered Deactivate, Replace with API key, Add another account, Use default account for this session, and Make default the default for new sessions. No `work` selection.
   - Returned to the provider list, opened the `Z.AI · personal` row at 23:27:18Z (`s-zai-personal-open`); it offered only Rename and Remove. No `work` selection.
6. The case’s required “select `work`” control was absent. The tester stopped further case actions and pressed `Escape` back to the composer at 23:27:33Z (`s-cancel-composer`), then `/quit` exited code 0. No `stwo` was sent.

## Positive checkpoints

- `s0-2` at 23:26:16Z: `Model provider:` ends `· account personal`; `Account usage: personal · no usage yet`; complete Session row.
- `s1` at 23:26:34Z: one `personal` usage row: `personal · 19 input · 8,576 cached input · 52 output (48 reasoning)`.
- Visible captures through `s-cancel-composer` contained no `$`, “cost”, “price”, or `agms-fake`.
- Footer observations: before the first prompt, `via zai default`; after `sone`, still `via zai default`; side-related footer was not present in NA-10.

## Logged relay-relevant times (for the integrator)

- `sone`: sent 23:26:20Z, done 23:26:27Z.
- `stwo`: not sent because the required control was missing.

## Expected-result outcomes

- `s0-2` shows `· account personal` and `no usage yet`: **passed**.
- `s1` has one `personal` row with input/output counts: **passed**.
- Advisory wording for `s1`: **passed** — the form matched the quoted shape.
- No `$`, cost, price, or fake-key text in captures: **passed** based on captured screens.
- “Account:” row has no `agms-fake`: **passed**.
- Footer observations: **observed** as above.
- Selecting `work`, subsequent ASK `stwo`, STATUS `s2`, and related relay/file checks: **blocked/not exercised** — the provider UI offered only `default` and `personal`, with no path to select `work`.

## Product-model refusals

None were observed.

## Final verdicts

VERDICT NA-08: passed
VERDICT NA-09: passed
VERDICT NA-10: blocked
