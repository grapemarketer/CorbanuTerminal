# G7 functional execution report — candidate Corbanu Terminal 0.1.48

## Isolation and identity preflight

- Executor preflight: `agms-probe all > evidence/preflight-executor.json; echo rc=$?` — rc=0; JSON reports `"ok": true`. UTC 00:19:52Z.
- Coordinator preflight files present: `preflight-summary.json`, `preflight-launcher.json`, `preflight-tmux-child.json`; summary `"ok": true`.
- Digest `/opt/agms/pkg/bin/corbanu`: `bbae7d37dff9023268efdd05a8a4408ccf144ee51ba911afd39adc0e9ddce1b8` — matches.
- Digest `/opt/agms/packet/design-cases.md`: `cfad42f543b6782a70b935b0596ac50737468733f3480629f49089e34eb6e5b4` — matches.
- Prepared home listed: `custom` only. Its config was read before testing: custom provider `zpx`, name `Z.AI proxy test`, `model_provider = "zpx"`, `named_accounts = true`; no `[provider_accounts]` section was present.

## Window mapping

- To be filled as windows are opened.

---

## NA-12 — Named account on a custom provider

Home: custom.

Status: completed.

### Window mapping

- w1 = NA-12 step 1 TUI
- w2 = NA-12 step 2 TUI with `--account work`

### Requested actions / actual actions / observations

1. START(custom): requested. Actual started in `w1` at 00:21:33Z, exited cleanly on QUIT at 00:22:11Z with code 0 and normal terminal settings (`icanon isig echo icrnl opost`). Screens: `w1-start`, `w1-cone`, `c1`, `w1-quit`.
   - ASK(cone): `cone sent` 00:21:45Z; text and Enter separately; `• cone` visible in check at 00:21:53Z; `cone done` 00:21:59Z.
   - STATUS(c1) at 00:22:03Z: complete status box. Provider row: `Z.AI proxy test - https://api.z.ai/api/paas/v4 · account default`. SID `<c1>` = `01a160fc-1176-7e70-a3e7-dac26bec6d18`. `Account usage: default · 8,559 input · 3 output`.
2. START(custom, `--account work`): requested. Actual started in `w2` at 00:22:22Z, exited cleanly on QUIT at 00:23:25Z with code 0 and normal terminal settings. Screens: `w2-start`, `w2-ctwo`, `c2`, `w2-quit`.
   - ASK(ctwo): `ctwo sent` 00:22:29Z; Enter 00:22:30Z; checked at 00:23:00Z and `• ctwo` visible; `ctwo done` 00:23:13Z.
   - STATUS(c2) at 00:23:17Z: complete status box. Provider row: `Z.AI proxy test - https://api.z.ai/api/paas/v4 · account work`. SID `<c2>` = `01a160fc-d04e-7e72-be13-a1651aa75e20`. `Account usage: work · 48 input · 8,512 cached input · 4 output`.
   - Observation: before sending `ctwo`, the `w2-start` footer still said `default`; the completed `c2` status shows `work`.

### Positive checkpoints

- `w1-start` / `w1-cone`: TUI running, model `glm-5.3-flash`, provider footer `Z.AI GLM 5.3 Flash via zpx default`; `• cone` visible by 00:21:53Z.
- `c1` at 00:22:03Z: `Model provider:` names `Z.AI proxy test` and ends `· account default`.
- `w2-ctwo`: `• ctwo` visible by 00:23:00Z.
- `c2` at 00:23:17Z: `Model provider:` names `Z.AI proxy test` and ends `· account work`.

### Expected-result assessment

- Provider line names “Z.AI proxy test”: **passed** in `c1` and `c2`.
- `c1` shows `· account default`: **passed**.
- `c2` shows `· account work`: **passed**.
- Relay `cone → zenv` and `ctwo → zpxwork`: **for the integrator**. UTC request windows: cone sent 00:21:45Z / done 00:21:59Z; ctwo sent 00:22:29Z / done 00:23:13Z.

### Remaining case actions

- None; case completed.

3. START(custom, `resume <c2>`): requested. Actual started in `w3` at 00:23:58Z using SID `<c2>`; exited cleanly on QUIT at 00:25:09Z with code 0 and normal terminal settings. Screens: `w3-resume-start`, `w3-cthree`, `c3`, `w3-quit`.
   - ASK(cthree): `cthree sent` 00:24:14Z; Enter 00:24:14Z; checked at 00:24:44Z and `• cthree` visible; `cthree done` 00:24:59Z.
   - STATUS(c3) at 00:25:02Z: complete status box; provider row ends `· account work`; Session is `<c2>`; `Account usage: work · 109 input · 17,024 cached input · 32 output (24 reasoning)`.
   - Observation: before sending `cthree`, `w3-resume-start` footer still said `default`; `c3` status shows `work`.
4. CLI(custom, `exec --account work "reply with the single word cfour"`): requested. Actual started in `w4` at 00:25:20Z, printed `cfour`, and exited by 00:25:26Z with code 0 and normal terminal settings. Screen: `w4-cfour-30`. CLI session id: `01a160ff-853f-7a40-a78c-4117851a1ffd`.
5. `agms-tui stop`: requested and actual at 00:25:39Z.

### Positive checkpoints

- `w1-start` / `w1-cone`: TUI running, model `glm-5.3-flash`, provider footer `Z.AI GLM 5.3 Flash via zpx default`; `• cone` visible by 00:21:53Z.
- `c1` at 00:22:03Z: `Model provider:` names `Z.AI proxy test` and ends `· account default`.
- `w2-ctwo`: `• ctwo` visible by 00:23:00Z.
- `c2` at 00:23:17Z: `Model provider:` names `Z.AI proxy test` and ends `· account work`.
- `w3-resume-start`: resumed transcript visibly contains `• ctwo`; `• cthree` visible by 00:24:44Z.
- `c3` at 00:25:02Z: provider row ends `· account work`, Session `<c2>`.
- `w4-cfour-30`: CLI output contains `cfour`, provider `zpx`, exit code 0.

### Expected-result assessment

- Provider line names “Z.AI proxy test”: **passed** in `c1`, `c2`, and `c3`.
- `c1` shows `· account default`: **passed**.
- `c2` shows `· account work`: **passed**.
- `c3` shows `· account work`: **passed**.
- CLI `exec --account work` exits 0 and prints `cfour`: **passed** (visible expectation from action; relay identity is below).
- Relay `cone → zenv`; `ctwo`, `cthree`, `cfour → zpxwork`: **for the integrator**. UTC request windows:
  - `cone`: sent 00:21:45Z / done 00:21:59Z.
  - `ctwo`: sent 00:22:29Z / done 00:23:13Z.
  - `cthree`: sent 00:24:14Z / done 00:24:59Z.
  - `cfour`: CLI start 00:25:20Z / exit by 00:25:26Z.

---

## I426-B — `--account ghost` on a custom provider

Home: custom, after NA-12.

Status: completed.

### Window mapping

- w5 = I426-B step 1 REFUSE
- w6 = I426-B step 2 REFUSE
- w7 = I426-B step 3 REFUSE

### Requested actions / actual actions / observations

1. REFUSE(custom, `resume --account ghost <c1>`) as `ib1`: requested. Actual started in `w5` at 00:26:17Z; at 00:26:19Z, `ib1-2s` showed only the error and exited banner; wait returned by 00:26:21Z. `ib1` showed the same. Exit code 1; terminal settings `icanon isig echo icrnl opost`. No session or picker appeared.
2. REFUSE(custom, `resume --account zpx:ghost <c1>`) as `ib2`: requested. Actual started in `w6` at 00:26:28Z; at 00:26:30Z, `ib2-2s` showed only the error and exited banner; wait returned by 00:26:32Z. `ib2` showed the same. Exit code 1; terminal settings `icanon isig echo icrnl opost`. No session or picker appeared.
3. REFUSE(custom, `--account ghost`) as `ib3`: requested. Actual started in `w7` at 00:26:44Z; at 00:26:46Z, `ib3-2s` showed only the error and exited banner; wait returned by 00:26:48Z. `ib3` showed the same. Exit code 1; terminal settings `icanon isig echo icrnl opost`. No session or picker appeared.
4. `agms-tui stop`: requested and actual at 00:26:48Z.

### Verbatim refusal/error text (as captured; terminal wrap shown with the line break)

```text
account `ghost` of provider `zpx` is not configured; add it with `corbanu account add zpx ghost` or pick another with `--account` (see `corbanu accoun
t list`)
```

This same visible wording appeared for `ib1`, `ib2`, and `ib3`. This was a CLI/start refusal, not a model reply.

### Positive checkpoints

- `ib1-2s` / `ib1` at 00:26:19–00:26:21Z: error names `ghost` and provider `zpx`; gives `corbanu account add zpx ghost`; mentions `--account`; product exited code 1; normal terminal settings.
- `ib2-2s` / `ib2` at 00:26:30–00:26:32Z: same positive text and exit behavior.
- `ib3-2s` / `ib3` at 00:26:46–00:26:48Z: same positive text and exit behavior.
- No capture showed onboarding phrases, `Choose a provider account`, or an OpenAI device-code screen.

### Expected-result assessment

- Step 1 exits non-zero with text naming `ghost` and provider `zpx` and giving `corbanu account add zpx ghost`: **passed**.
- Step 2 exits non-zero with the same required content: **passed**.
- Step 3 exits non-zero with the same required content: **passed**.
- No provider picker or OpenAI device-code screen; NO-ONBOARD holds in captured screens: **passed**.
- Relay nothing during steps 1–3, including no OpenAI or ChatGPT traffic: **for the integrator**. UTC refusal windows:
  - `ib1`: 00:26:17Z through exited by 00:26:21Z.
  - `ib2`: 00:26:28Z through exited by 00:26:32Z.
  - `ib3`: 00:26:44Z through exited by 00:26:48Z.

---

## Overall

- Isolation and candidate identity checks passed before testing.
- NA-12 was executed fully and all directly judgeable expectations passed.
- I426-B was executed fully after NA-12 and all directly judgeable expectations passed.
- Relay-label expectations are marked for the integrator with request windows above.
- No system password prompt, unexpected real-account sign-in, unexpected outside access, missing required control, or lost observation occurred.
- No model decline/refusal occurred; the three I426-B errors were product CLI/start refusals and are recorded verbatim.
- This report records executed outcomes and does not claim human acceptance.

VERDICT NA-12: passed
VERDICT I426-B: passed
