# PF-84 isolated code-blind acceptance run (pf84-cb1, 2026-10-21/22)

Prerequisite 6 of the [named-accounts flag-removal plan](../../../../docs/plans/proposed/named-accounts-flag-removal.md)
(owner decision D137): a code-blind acceptance run from a frozen design, in isolated VMs, on a build with #1192 and
#1250, with `named_accounts` ON as draft #1080 will make it. It also re-runs the partial #555 legs and #426's two
remaining legs, covers D136 (removing an account while a session runs on it, live, with the free ChatGPT test account)
and the D86/D124/D125 `/status` behaviour. Out of scope by owner decision: live Bedrock (D99), a live refresh saved back
(D135), #1205/#1208 (D138).

**Result: 28 cases: 23 passed (one, ESC-01, after the integrator reclassified a harness artifact), 2 passed in part
(NA-10: one check could not be set up because "Use … for this session" starts a new session; ST-02: d5-2 not reached
for the same reason, and its NO-SECRET checks untested because of a tester error), 1 not complete (NA-06: rename not
exercised), 2 failed.** The failures: **D136** blocks (after the account is removed the running session shows no
recovery text; it also kept answering on the removed sign-in, recorded as `[observe]` pending decision 1); **NA-05** (in-app `/resume` silently re-accounts a
conversation whose account is gone when the TUI was started with `--account`; the docs are ambiguous, owner
decision). Per case: [Results](#results). Open owner questions: [below](#open-owner-decisions).

> **Re-run, 2026-10-22 ([code-blind-rerun-20261022/](../code-blind-rerun-20261022/README.md)):** after the fixes for
> D171–D175 (#1299, #1300, #1301), D136, NA-05 and NA-06 (including its rename step) were re-run on a signed build of
> `main` `f61397ca41`, from a frozen amendment v3. **All three passed**, with the integrator's relay and file checks.
> With them, the 28 cases stand at 26 passed and 2 passed in part (NA-10, ST-02, unchanged); the other 25 cases count as
> run on this run's build `31b9f3cad8`. The tables below keep the
> first run's results as they were.

## Build

- `/Volumes/CorbanuDrive/Corbanu/.codex-work/integration-build-20260922a/integration-package-dev/bin/corbanu.31b9f3ca`,
  copied privately into each guest as `/opt/agms/pkg/bin/corbanu` (read-only, root-owned). SHA-256
  `bbae7d37dff9023268efdd05a8a4408ccf144ee51ba911afd39adc0e9ddce1b8` (checked by the coordinator's probe and by each
  tester; also in every clone's `provision.out`). `codesign --verify --strict` passes; TeamIdentifier `K5N2T25C23`,
  signed 2026-10-21 12:55 (local). `--version`: `corbanu 0.1.48`.
- Source: `main` `31b9f3cad8`, which contains #1192 (`76d99da427`) and #1250 (`d6c15422a9`) (`git merge-base
  --is-ancestor`). The installed binary was not used or changed (the designer/reviewer sessions used it as a client
  only, as every worker does).
- Companion binaries in the guest package (`codex-code-mode-host`, `corbanu-acp`, `corbanu-walletd`, `rg`) are the ones
  staged by the utm-par3 run; [harness/package-files.txt](harness/package-files.txt) lists every digest.
- `named_accounts = true` in every test home's `config.toml` except the two feature-off homes (the candidate's default
  is still off; #1080 flips it).

## Method

### Design (frozen before any run)

- **Designer:** fresh read-only Opus 5.5 High session (`corbanu exec -s read-only`, claude-plan), packet only:
  [design/designer-packet/](design/designer-packet/) (intent written in user terms from the plan, the archived sprint
  "Done" lists, the owner decisions and the #555/#426 gaps; the user doc `docs/provider-accounts.md`; the 2026-10-12
  reference screens; [tools.md](design/designer-packet/tools.md), the test-machine facts). Output:
  [design/designer-output.md](design/designer-output.md).
- **Reviewer:** a separate fresh read-only Opus 5.5 High session with the same packet plus
  [review-facts.md](design/review-packet/review-facts.md) (harness answers to the designer's questions, no product
  answers). `VERDICT: CHANGES`; its revised design is frozen word for word as
  [design/design-cases.md](design/design-cases.md) (SHA-256 `cfad42f5…`, [FROZEN.sha256](design/FROZEN.sha256)); open
  questions in [design/open-questions.md](design/open-questions.md). No integrator edits.
- **Amendment v2** (after the first run; results-informed): the first run found steps that the design could not reach.
  A new fresh designer and a separate fresh reviewer, given [run-observations.md](design/amend-v2/run-observations.md),
  wrote [design/amend-v2/amendments-v2.md](design/amend-v2/amendments-v2.md) (frozen from the reviewer's
  `VERDICT: CHANGES`, SHA-256 `fe132491…`) for NA-01, NA-04, NA-10, PR-01, GH-02, NA-05, NA-06, NA-07, ST-02 and
  ESC-04. Both sessions were told not to weaken or change an expected result to match what was observed (the rule is in
  [prompt.md](design/amend-v2/prompt.md) and [review-prompt.md](design/amend-v2/review-prompt.md), not in the frozen
  text). Changes to expected sections, all disclosed here:
  - PR-01: the frozen check that a no-flag `resume <id>` after a turn on `personal` runs on `work` **failed in G2**
    (it ran on `personal`, the latest recorded account). The amendment made that situation `[observe]` (`pq`, owner
    question v2-Q1) and added a re-anchor turn on `work` before the no-flag resume, so PR-01's later pass does not
    settle it. New checks `pb` (PR-01) and `g0` (GH-02) were added; p2 becomes `[observe]` if `pb` fails.
  - New NO-SECRET exception for a key the tester typed into the wrong field; NA-06 lets the integrator reclassify from
    screens if the add does not complete; new fail rules for hidden rows (ROW) and for not finishing onboarding (ESC-04).
- Access records: [design/access-record.md](design/access-record.md), [design/amend-v2/access-record.md](design/amend-v2/access-record.md)
  (every command each session ran: packet files only).

### Isolation and execution

- **Machine:** Tart, `TART_HOME=/Volumes/CorbanuDrive/Corbanu/.codex-work/tart-home`, one fresh clone of
  `corbanu-qa-base` (macOS 26.6.2) per group, deleted after it; one VM at a time, holding
  `workers-20261002/vm-tart.lock` (taken after utm-par3 had released it and it had stayed free for 5 minutes). The
  harness is utm-par3's sealed-guest harness (pf: SSH from the host, DHCP and the pinned Z.AI addresses only; DNS
  refused; tester lanes `agmx1..3` reach only the loopback mediator), changed as listed under
  [Harness](#harness-changes-in-this-run). Memory stayed 45–46 % free (`memory-*.txt`).
- **No real key in the guest.** All test-home keys are synthetic (`agms-fake-<label>-<digits>`). The guest mediator
  records the label of the key each product request presents, drops it, and forwards Z.AI requests through an SSH
  reverse tunnel to a host relay that adds the real Z.AI key (fetched with `corbanu vault auth-helper` at relay start,
  never printed). It answers `api.openai.com` itself with 401, and Z.AI requests with no key or a `bad…` label with 401.
- **Live ChatGPT test account (D136), G9 only.** The coordinator ran `corbanu login --account t1 --device-auth` in the
  guest lane; only the device URL and code left the guest, in `workers-20261002/pf84-cb1.signin.md`, which the
  coordinator relays to Travis (D136). Attempt 1 was approved (per the coordinator; the record cannot show who
  approved): posted 22:02:44Z, signed in 22:14:54Z, "ChatGPT sign-in · <email-hint> · Free". ChatGPT traffic went
  through a blind pass-through (host `chatgpt.com:443`/`auth.openai.com:443` only, TLS not opened), so the test
  account's tokens existed only inside the guest. D136 itself removed `t1` ("Signed out and removed openai account
  `t1`"); the coordinator's sign-out step then found it already gone, and the clone's lane homes were deleted
  (0 entries left) before the clone was deleted. Collected evidence never contains vault, key or `auth.json` files.
- **Testers:** one fresh GLM 5.3 Flash (`zai`) `corbanu exec` session per group, in the lane, with the frozen cases and
  `agms-tui` only; each first re-ran the isolation probe and checked both digests.
- **Integrator checks** (not visible to testers): the relay table per group (`relay-product-requests.txt`), conversation
  files (`sessions-accounts.txt`), home file lists, and one integrator probe in three attempts (plus two discarded clones)
  ([integrator-probe-na05/](integrator-probe-na05/)), which is not a case and not code-blind tester evidence.
- **Background `chatgpt.com` attempts.** In every Z.AI home the product also opened `chatgpt.com:443` (and
  `github.com`/`api.github.com`, refused) around session start, even with no OpenAI sign-in; outside G9 the
  pass-through was off, so these show as `passthrough_error`/`denied_proxy_request`. "Nothing sent" below means no model
  request.
- **Order:** G9 ran first so that the human sign-in fell in working hours; groups are independent (fresh machines).
- **Safety checks per clone:** 0 `SECURITYAGENT-SEEN` lines (no system password prompt); synthetic credential files left:
  0; mediator leaf key and placeholder key file absent (`runs/*/clone-*`, `harness/shakedown/clone-*`,
  `integrator-probe-na05/clone-*`, `integrator-probe-na05/armB/clone-*`, `…/armB/discarded/*`; one discarded clone
  stopped at staging, so its guard recorded nothing). Per-clone binary and harness digests and the host-side signature
  check: [candidate-identity.md](candidate-identity.md). Sign-in approval (per the coordinator):
  [runs/G9/signin-handoff.md](runs/G9/signin-handoff.md) (code redacted). The real login keychain and the owner's home were not touched.

### Harness changes in this run

| Version | Change | Clones |
| --- | --- | --- |
| v1 | utm-par3 harness + prepared homes ([agms-prep](harness/guest/agms-prep)), key-label mediator, OpenAI 401, ChatGPT pass-through, `--home`, exit/terminal-settings line | shakedown, G9, G1 |
| v2 | prep writes a baseline file list/hashes (`homes-after-prep.txt`); sign-out no longer stops on an error | G2 + |
| v3 | `agms-tui start --stdin TEXT` (pipe a key) | G3–G8 (unused until amendment v2) |
| v4 | a `C-c` no longer ends the terminal wrapper before the exit line (lost REG-01d's window in G8) | G6R, G8R, R1–R4, probe |

Shakedown (no tester; a first clone stopped at once on a harness argument bug, `shakedown1-prep-bug-…` in
[candidate-identity.md](candidate-identity.md), and was deleted; the second ran): every home built, each account kind attributed correctly by the relay, OpenAI 401,
device-code screen reached through the pass-through and cancelled unapproved ([harness/shakedown/](harness/shakedown/)).

## Results

"Run" is the group whose result counts (G = frozen design, R = amendment v2, G6R/G8R = unchanged replays).
Relay/files lines were checked by the integrator; detail in [harness/host/integrator-notes.md](harness/host/integrator-notes.md).

| Case | Covers | Run | Tester | Final | Notes |
| --- | --- | --- | --- | --- | --- |
| NA-01 | AC1, AC6 CLI add/list/remove | R1 | passed | **passed** | G1 blocked by design: `account add` refuses a typed key ("pipe the secret in"). Relay: `xone`→`zextra`; after removal nothing sent. `[observe]` a key given as an argument is echoed by the usage error. |
| NA-02 | AC2 `--account` on exec/TUI | G1 | passed | **passed** | Relay: `work`, `zai:work`→`zwork`; no flag→`zpersonal`; `default`→`zenv`. |
| NA-03 | AC3 missing account | G1 | passed | **passed** | Exit 1 with `corbanu account add zai ghost`; nothing sent; no onboarding. |
| NA-04 | AC3/AC6 rejected key | R1 | passed | **passed** | Two 401s carrying only `badkey`; no fallback. |
| PR-01 | #555 leg, precedence | R2 | passed | **passed** | Amended run: `/status` and relay at every step: `work`, `personal`, `work`, `personal`, `default`; files: `<id>` records `work, personal, work, work`. The frozen "no-flag resume after a turn on `personal` → `work`" check **failed in G2** and is now `[observe]` (v2-Q1, see Method). |
| GH-02 | #555 leg | R2 | passed | **passed** | After removing `work`, `resume <gid>` exits 1 naming `work` with `corbanu account add zai work`. `[advisory]` `ghost` not mentioned (as in 10-13). G2's version never set up its situation (premise). |
| NA-05 | AC3/AC4 in-app resume | R2 | failed | **failed (owner decision)** | In a TUI started with `--account personal`, in-app `/resume` of a conversation recorded only on removed `work` resumed it on `personal` with no message (relay `rtwo`→`zpersonal`; session file: turns 1–2 `work`, turn 3 `personal`). Integrator probe: started **without** `--account`, the same resume is refused with REF-10's message and the session stays put; started **with** `--account personal` (arm B, `[provider_accounts]` = `personal`), it resumes silently on `personal` — so the launch `--account` is the likely cause (arm B's companion package differed; `corbanu` identical; see the probe README). `docs/provider-accounts.md` is ambiguous: "Choosing an account" lets an explicit `--account` win; "Fail closed" says in-session `/resume` shows the error and stays on the current conversation, and then "An explicit `--account` of the same provider still wins", without saying whether a launch `--account` counts for an in-session `/resume`. |
| NA-06 | AC1 rows, add, rename | R3 | failed | **not complete** | Rows, kinds, fingerprints and markers passed; add with masked key passed. Rename not exercised: the tester chose "Use for this session" by mistake; the product then rightly refused renaming the in-use account. |
| ESC-03 | #555 leg | G3 | passed | **passed** | Same session, account and transcript after Esc. Draft clause: no route found (`/resume <text>` searches); open question Q2. |
| NA-07 | AC1/AC2/AC3 use/make-default/remove in use | R3 | passed | **passed** | G3 blocked (row 10 hidden, below). |
| NA-08 | AC4 forks | G4 | passed | **passed** | `/fork`, edit, `/side` all `zwork`; files: the fork conversations record `work`. |
| NA-09 | AC4 `corbanu fork`, spawned agent | G4 | passed | **passed** | Fork and spawned agent all `zwork`; `fork --account personal`→`zpersonal`; files: the sub-agent conversation records `work`. |
| NA-10 | AC5/D86 `/status` format | R1 | failed | **passed in part** | `[advisory]` s1 omitted the cached-input count. All other checks passed except s2's "table with `work (active)` and `personal`" after "Use for this session": that action starts a **new** session ("New session: requests to zai use account `work`"), so the in-session table could not be set up (not reached). Multi-account tables were seen in resumed conversations (PR-01, NA-05). G4 blocked (row hidden). |
| ST-01 | D86/D125 no named accounts | G5 | passed | **passed** | Plain `/status` in `zaione`. |
| ST-02 | D124/D86/D125 | R4 | failed | **passed in part** | Passed: an account added by another process shows on the 2nd `/status` (D124); after removal `/status` is plain; no `/status` late or stalled. Not reached: d5-2, the D86 "session has used a named account" clause (each "Use … for this session" starts a new session, which then never used one). Not tested: NO-SECRET — the tester piped the literal `k` as both keys; `dtwo` carried a key the relay labels `other:8254c329` = `sha256("k")`, i.e. `extrb`'s stored key (inferred). The tester also opened an unplanned Deactivate dialog (cancelled). |
| ESC-04 | #555 leg onboarding | R4 | passed | **passed** | Onboarding list and masked Z.AI key screen as REF-12/14; finishing needs scrolling to row 17 "Done". `[observe]` the fresh home then used `glm-5.2`. G5 blocked (Done never reached). |
| NA-11 | AC2/AC3 no default credentials | G6R | passed | **passed** | `work` works without onboarding. G6's tester stopped (model output limit). |
| ESC-01 | #555 leg | G6R | failed | **passed (harness)** | Exit 0, no onboarding, terminal left `icanon isig echo icrnl opost` (the gap: restore now observed directly). The tester failed "echo once": the harness shows a typed line twice (terminal echo + `cat`), which is what a restored terminal does. |
| I426-A | #426 leg 1 | G6R | passed | **passed** | `resume --account ghost <id>`, `zai:ghost`, `--last`: exit 1 with recovery text, no onboarding even at 2 s, nothing sent. |
| NA-12 | AC2/AC4 custom provider | G7 | passed | **passed** | `zpx` default→`zenv`, `work`→`zpxwork`, resume and exec keep `work`. |
| I426-B | #426 leg 2 | G7 | passed | **passed** | Custom provider `--account ghost`: exit 1 with recovery, no picker, no OpenAI sign-in, nothing sent. |
| REG-01a | AC8 flag off | G8 | passed | **passed** | Plain `/status`; `[provider_accounts]` ignored with a warning; `/fork` plain. |
| REG-01b | AC8 flag off resume | G8 | passed | **passed** | Resume of a `work` conversation refused with the feature-off text (raw "Could not resume session…" wording, as #656); `--account zai:default` works. |
| REG-01c | AC8 picker Esc (gap) | G8 | passed | **passed** | The `corbanu resume` picker is reached with the feature off (usable credentials), which closes the gap. `[observe]` its footer says `esc start new` and Esc starts a new session (REF-06, a no-key home, says `esc quit`); the in-session `/resume` picker's Esc returns to the session. |
| REG-01d | AC8 no credentials | G8R | passed | **passed** | Onboarding as on the earlier build; exit 0; terminal normal. G8 lost its window (harness v4 fix). |
| NA-13 | AC6/AC7/REG-02 live ChatGPT | G9 | passed | **passed** | `t1` row "ChatGPT sign-in · <email-hint> · Free · fp …"; `login status` "API key configured (fingerprint …)"; live answer on `t1`; masked API-key entry; no `odefault` request in step 3. Baseline `auth.json` hash taken by the tester at group start equals the end hash; the prep-time hash was not captured (fixed from G2). |
| D136 | owner item | G9 | failed | **failed** | See below. |
| ST-03 | D86/D125 default OpenAI | G9 | passed | **passed** | No `Model provider` or `Account usage` row; `odefault` rejected with a provider-level recovery. |

Totals (final): passed 23 (ESC-01 reclassified: harness artifact), passed in part 2 (NA-10, ST-02), not complete 1
(NA-06 rename), failed 2 (D136; NA-05 pending an owner decision). Tester verdicts in the counted runs: 22 passed, 6
failed.

Other observations: in G3 a "Choose replacement" dialog appeared on `/quit` after the tester had opened
"Deactivate"; in R3 (NA-07) window B, whose account `work` was removed from window A, got a fatal error on its next
request ("■ Fatal error: account `work` of provider `zai` has no stored API key; add it with `corbanu account add zai
work` or choose another account") but its
`/status` still showed `· account work` — the same `/status` pattern as D136, with the refusal that D136 lacks.

### D136 in detail

Window A ran on `t1` and answered `beforeone`. At 22:24:07Z another terminal ran `corbanu account remove openai t1`
(exit 0, "Signed out and removed …"; the relay shows an `auth.openai.com` exchange at 22:24:08Z); `account list` then
showed no named accounts.

- Passed: no fallback (no `api.openai.com` request carrying `odefault` from D136's start, 22:23:43Z, to its end,
  22:28:54Z — the frozen check says "to the end of the group", but the next case, ST-03, sends `odefault` by design at
  22:29:30Z and 22:29:53Z, so the check is judged on D136's own window); no sign-in or onboarding screen; nothing
  re-created `t1`; `/providers` had no `t1` row; `config.toml` unchanged. The tester passed "x3 does not present the
  session as healthy" with an ambiguity: `/status` said "Account: account t1 is not signed in" but kept
  "Model provider: OpenAI · account t1" and the `t1` usage row.
- **Failed:** no recovery text (`corbanu login --account t1`, `/providers`, `--account`) appeared by the last `/status`.
- `[observe]` (open question Q1): both later prompts (`afterone` 22:24:46Z, `aftertwo` 22:26:33Z) were **answered**,
  over `chatgpt.com` (22:24:48Z, 22:26:35Z): the session kept working on the removed account's sign-in held in memory.

## Defects filed

| Issue | Severity | What |
| --- | --- | --- |
| [#1286](https://github.com/CorbanuCore/CorbanuTerminal/issues/1286) | blocks D136 | After a named account is removed elsewhere, the running session shows no recovery and `/status` still names it; it also kept using the removed sign-in (`[observe]`, decision 1). |
| [#1287](https://github.com/CorbanuCore/CorbanuTerminal/issues/1287) | needs decision | In-app `/resume` of a conversation whose recorded account is gone silently re-accounts it to the launch `--account`. |
| [#1288](https://github.com/CorbanuCore/CorbanuTerminal/issues/1288) | advisory | `/providers` (8-row window) and onboarding lists hide rows with no hint; a third account or "Done" is invisible until scrolled. |
| [#1289](https://github.com/CorbanuCore/CorbanuTerminal/issues/1289) | low | Add-account name field: Enter right after fast typing adds a newline; a key pasted next shows in clear in the name field. |
| [#1290](https://github.com/CorbanuCore/CorbanuTerminal/issues/1290) | low | Escape on the `/providers` device-code screen leaves the sign-in waiting (banner stays; the auth connection stayed open until quit). |

## Open owner decisions

1. **D136 / Q1:** may a session keep using credentials it holds in memory after its account is removed (G9: yes, for a
   ChatGPT sign-in; R3: an API-key account is refused at once)? This decides the fix for #1286.
2. **NA-05:** in-app `/resume` of a conversation whose account is gone, in a TUI started with `--account`: keep the
   launch account (with a notice) or refuse (#1287; the docs are ambiguous on this).
3. **v2-Q1 (PR-01):** after a conversation has run on two accounts, a no-flag resume uses the latest one (observed).
   Is that intended? The frozen PR-01 text expected the first.
4. **"Use … for this session" starts a new session** (NA-10, ST-02): intended? It leaves the current conversation.
5. **Q3:** a key passed as a command-line argument is echoed in the usage error (NA-01). The v2 reviewer reads AC6 as
   a failure; it is recorded as `[observe]`.
6. **Q6:** the footer reads `via zai default` while the session runs on a named account (G4, R1, R2).
7. **ESC-03 draft (Q2):** no route opens `/resume` with a draft in the composer; does the clause apply?
8. **ESC-04:** finishing onboarding needs scrolling to a hidden "Done" row (#1288), and the fresh home then used
   `glm-5.2`.
9. **Q8:** ST-03 ran after `t1` was removed (it needs a home without it); accepted by the coordinator.

## What this run does not show

- A live refresh saved back (D135), Bedrock (D99), the daemon/remote app-server path, Windows (Travis's gate), and the
  D3 approvals-off leg (plan item 4; unit tests only).
- NA-06's rename (tester error); ESC-03's draft clause (no route); D124's "vault cannot be read" path.
- D86's "session has used a named account" clause, NA-10's table after an in-session switch, and ST-02's NO-SECRET
  checks (see the table).
- PR-01's frozen "no-flag resume after a turn on another account" situation as the owner described it (failed in G2,
  `[observe]` since).
- `auth.json` unchanged from prep time in G9 (the tester's own start-of-group hash equals the end hash).
- Files checks: the integrator checked NA-08, NA-09 (`runs/G4/sessions-accounts.txt`), PR-01, GH-02, NA-05
  (`runs/G2/`, `runs/R2/sessions-accounts.txt`) and D136 (integrator notes, G9 `auth.json` hash); the tester checked
  NA-04's config itself; REG-01b's Files line ("CFG(zai3off) ends with `named_accounts = false`") was not checked.
- The tester model (GLM 5.3 Flash) slipped several times (wrong menu item, a literal placeholder key, a report in the
  wrong folder); every slip is recorded and none is counted as a product result.

## Readiness

Not ready for Travis's Windows gate and sign-off yet: D136 fails a blocker (#1286), and NA-05 (#1287) and decisions
1–4 above need the owner. Everything else the owner asked for passed: #426's two legs, the #555 legs (ESC-01's
terminal restore now observed directly, ESC-04 against the earlier build, GH-02, PR-01 as amended, REG-01 with its
picker gap closed; ESC-03's draft clause has no route), the live ChatGPT account display and use, and D86/D124/D125 in
homes with and without named accounts. After #1286 (and #1287 if the owner chooses a change), a short targeted re-run
of D136 (one more test-account sign-in), NA-05 and NA-06's rename would close the record.

Evidence notes: `runs/R3/screens.jsonl.gz` is gzip-compressed (613 KB uncompressed, above the repository's blob limit).

## Review

Independent read-only Opus 5.5 High review of this record: [REVIEW.md](REVIEW.md).
