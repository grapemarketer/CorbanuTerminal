# Group A-R1 functional execution report (not human acceptance)

## Preflight and identity
- UTC start of group: 2026-10-22T12:11:32Z.
- Ran `agms-probe all` before any case work. It returned `"ok": true`; evidence: `preflight-executor.json`.
- Coordinator preflight summary: `preflight-summary.json` reports `launcher_ok: true`, `tmux_child_ok: true`, `pty_input_echoed: true`, `failed: []`, `probe_count: 110`, `ok: true`.
- Coordinator child preflights beside it: `preflight-launcher.json` (`ok: true`) and `preflight-tmux-child.json` (`ok: true`); `preflight-pty-control.txt` also present.
- Candidate identity observed: `/opt/agms/pkg/bin/corbanu --version` printed `corbanu 0.1.48`.
- SHA-256 observed for `/opt/agms/pkg/bin/corbanu`: `e942b095de975f95ca3fa1f09b9576f6bb408d91499e05235e35ab37b9d6aa42` (matches expected).
- Packet SHA-256 values observed:
  - amendment-r1.md `c03044a22f6d4c695a749b700186c0cbb288ef7ee6426bd73577e3717bdd2e0b` (matches)
  - design-cases.md `f750b006046cb5036f92fd6f4f8d910ccd8aa33f88b33c72ffefd0f6cc01bfd6` (matches)
  - amendments.md `2a73b6503429dc5a6d989b313e996a9041f5dc60b2c130e329dd39e1d71f105e` (matches)
  - answers.md `dc4044d36ac3868205ff2dc71cee36805969c6896c683852a6f9d705dc2d7c0c` (matches)
  - environment-r1.md `b7c82e3c73332c0f6d23956f6f2cc029d5f9ec118e4c99a6f82ba9c1217ba168` (matches)

## Group state before execution
- Route/model for group: `zai` / `glm-5.3-flash`.

## A-S1 — SET-UP
- Requested: `agms-tui help`; determine whether it has a control that writes to a file in the test project. If not, start a new zai conversation, send the literal append-file prompt, approve the displayed one-time option if approval is requested, wait for reply naming a file, capture `r1a-s1-turn`.
- Actual:
  - `agms-tui help` was run. It listed fixture, config, standin, text, key, screen, wait, stop and exec controls. No listed control writes to a file in the test project. Therefore the second prompt form was used.
  - Fixture before product start (`r1a-s1-before`): `workspace/notes.txt` ended with `review budget`; it did not contain `// review me`.
  - Started a new product conversation on `zai` / `glm-5.3-flash` at 2026-10-22T12:13:00Z; captured `r1a-s1-start` showing the composer.
  - Typed exactly: `Append one new last line containing exactly \`// review me\` to an existing tracked text file in this workspace, then reply with only that file's name.`
  - Captured `r1a-s1-typed-before-enter` showing the two-line composer text before Enter.
  - Pressed Enter separately at 2026-10-22T12:13:12Z.
  - Captured `r1a-s1-turn` showing product output `• Ran printf '%s\n' '// review me' >> notes.txt && tail -n 1 notes.txt`, output `// review me`, and the final reply `• notes.txt`.
  - Captured `r1a-s1-fixture`: `workspace/notes.txt` now ends with `// review me`.
  - No approval prompt appeared and no approval choice was needed.
- Set-up checkpoint: reached. The reply is shown and names a file (`notes.txt`).

## A-S2 — SET-UP
- Requested: stop; restart with `resume --last -c 'approvals_reviewer="auto_review"' -c 'approval_policy="on-request"' -c 'sandbox_mode="read-only"'`; capture `r1a-s2-resumed`; check transcript shows A-S1 turn.
- Actual:
  - Stopped the product.
  - Restarted as requested at 2026-10-22T12:13:55Z.
  - Captured `r1a-s2-resumed`. It shows the A-S1 prompt and reply `• notes.txt`, plus the model/directory header. The composer is shown.
- Set-up checkpoint: reached.

## ACC-06 — `/review` is labelled `code-review:`
- UTC case start: 2026-10-22T12:14:06Z.

### Start reading
- Requested: `/cost`, G8 captures `r1a-acc06-before-first`, `-details`, `-links`; every request page per G9 with exact costs; Esc back to composer.
- Actual:
  - Captured `r1a-acc06-before-first`. Relevant exact screen lines:
    - `  Cost — this conversation`
    - `  Requested UTC day: 2026-10-22`
    - `› Today (UTC) in this conversation:`
    - `  • Z.AI · Z.AI GLM 5.3 Flash — Pay per use. 4 requests, 35,323 tokens. Estimated cost: $0.003094 (rounded).`
    - `  No other conversation recorded requests on this day.`
    - `  Estimated token cost for recorded attempts: $0.003094 (rounded)`
  - Thus R0 = 4; conversation own count = 4; O0 = 0 and no other-conversations cost.
  - Captured `r1a-acc06-before-details`; it shows `  Estimated token cost for recorded attempts: $0.003094 (rounded)`. It does not show `Known subtotal exact USD:` in the terminal window at the initial scroll position, because the page is taller. This is a reading navigation ambiguity to be avoided in later captures; exact costs are taken from request technical pages below.
  - Captured `r1a-acc06-before-links`. Relevant exact screen lines:
    - `  —— Open ——`
    - `  → Z.AI · Z.AI GLM 5.3 Flash — estimated $0.003094 (rounded) (4 requests)`
    - `  → Request 1 · Z.AI · Z.AI GLM 5.3 Flash · estimated $0.001262 (rounded)`
    - `  → Request 2 · Z.AI · Z.AI GLM 5.3 Flash · estimated $0.001204 (rounded)`
    - `  → Request 3 · Z.AI · Z.AI GLM 5.3 Flash · estimated $0.000339`
    - `  → Request 4 · Z.AI · Z.AI GLM 5.3 Flash · estimated $0.000289 (rounded)`
    - `  → Root's own attempts (4 attempts)`
    - `› → Descendant attempts (none)`
  - Calibration: Request 3's request-page figure `$0.000339` is unmarked. Its technical money capture `r1a-acc06-before-req3-tech-money` shows `  Known subtotal exact USD: 0.000339`. Equal, so unmarked request-page figures are treated as exact for the rest of this group.
- Start request table (IDs copied from captured screens; exact costs from technical pages):

| ID | Cost line as shown | Exact cost | Kind | Turn prefix | Labels |
|---|---|---|---|---|---|
| 3bb2ed87-e9cc-499b-bab1-059922ab8761 | `  Estimated cost: $0.001262 (rounded)` | 0.00126183 | not shown | not read (not required at start) | `r1a-acc06-before-req1`, `-req1-tech`, `-req1-tech-money`, `-req1-tech-money-2`, `-req1-tech-tokens`, `-req1-tech-tokens-2` |
| 839a83d1-10a9-4d00-98b1-706a7813d02d | `  Estimated cost: $0.001204 (rounded)` | 0.00120435 | not shown | not read (not required at start) | `r1a-acc06-before-req2`, `-req2-tech`, `-req2-tech-money`, `-req2-tech-money-exact` |
| ea69523b-4af6-4958-a520-d94a955e1fe9 | `  Estimated cost: $0.000339` | 0.000339 | not shown | not read (not required at start) | `r1a-acc06-before-req3`, `-req3-tech`, `-req3-tech-money`, `-req3-tech-tokens` |
| 7c7db7ed-9825-4605-a22f-a3ce67823bc5 | `  Estimated cost: $0.000289 (rounded)` | 0.00028871 | not shown | not read (not required at start) | `r1a-acc06-before-req4`, `-req4-tech`, `-req4-tech-money` |

- Exact-cost arithmetic:
  - 0.00126183 + 0.00120435 + 0.000339 + 0.00028871 = 0.00309389, which agrees with the rounded display $0.003094.
- Esc navigation: three Esc presses returned to composer; captured `r1a-acc06-before-composer` at 2026-10-22T12:22:12Z.

### Review action
- Requested: type `/review`, press Enter, capture menu; choose option for uncommitted changes; wait without typing up to 10 min; any approvals go to automatic reviewer; capture done. Set-up checkpoint if no uncommitted changes.
- Actual: pending.
