# Readiness assessment: milestone "Moderate ships" (2026-10-14)

Prepared by the coordinator's moderate-prep worker against `origin/main` `9b83f3c6f9`; issue states rechecked against `655181275d`. It is an assessment for
Travis and the next coordinator, not a plan. The milestone itself is owned by the
[P1 security hardening plan](active/p1-security-hardening.md#carried-forward-from-p0) (row **Milestone "Moderate
ships"**). This file sits in `docs/plans/` rather than `docs/plans/active/`, because `docs/plans/check.py` treats every
file in `active/` as an active plan (limit 3, all taken).

**Verdict: not ready.** Moderate can't be chosen in `/security` today. A person reaches it only through a config
setting that PF-23-S01's readiness note calls a test setting. Its core protections are on main and tested, but four product
defects (#747–#750) and seven owner decisions (D1–D7 below) stand between it and a code-blind run. Estimated
elapsed time from Travis's decisions to a first sign-off request: about 3.4–4.4 working days for one cycle, about
4.5–6.5 with the one re-run the Aggressive history suggests (section 3.5).

## Update, 2026-10-14: Moderate picker sprint (mod-picker)

Travis decided D1–D7 on 2026-10-14 (on #750): D1 (a), D2 (a), D3 (b), D4 (b), D5 (b), D6 (a), D7 (a). The picker
sprint implemented D1–D6. The table below is what is now done; the rest of this file is the assessment as written.

| Item | Status | Where |
| --- | --- | --- |
| #750: choosing Moderate | Done. `/security` offers Moderate. Its review lists what it adds and what is not available (D1: PF-31, PF-32, PF-34, PF-35/36, PF-37, PF-38/39, PF-40). Confirming saves the level file and Core's record, and `r` restarts. Moving to Permissive or Aggressive says what changes and when. Esc changes nothing. Moderate isn't saved without `source_envelopes`, because Core refuses every model request under a protected level without it. | PR on `mod/750-moderate-picker`; videos in `qa/demos/index/issue-750.md`; tmux check in `qa/security-levels/moderate-picker-20261014/` |
| D4: when Moderate applies | Core's level, and with it the approval rules, the sandbox rule and the protected-file rules, is raised when you confirm. The credential broker and the TUI's active level apply at the next start. Core can't raise only the approval rules, so this is stricter than a literal "everything else at the next start". | same |
| #747 (D3) | Done. Under Moderate no command leaves the sandbox: approved escalations, the retry without the sandbox and allow rules all stay in it, and D6's remembered rules still skip the prompt. The prompt and the model's instructions say so. Aggressive confirmed before a restart, or set only in config, already keeps commands in the sandbox (pinned by tests). | #764 (merged) |
| #749: inspector | Done. "Active this run: Moderate" now appears beside "Moderate in force". The vault store shows as unreadable (but not under an external sandbox), no row says commands can run outside the sandbox, and the Nested agents row describes Moderate. The broker counts under Moderate, and the unbuilt controls are listed as not available. | PR on `mod/750-moderate-picker` |
| D2: broker | Done. `broker_model_auth` is on by default under Moderate. The person's own setting wins, and a project layer can't turn it off. | #764 (merged) |
| D5: child agents | Done. In-process agents (`/spawn`, agent tools) inherit Moderate. #766 (Travis, 2026-10-14: option (a) after the milestone run): `corbanu exec`/`review` started by an agent command run with Moderate enforced, with this home (in a home of their own) or another; sessions, hosts and credential commands are refused. | #766 |
| #748: refusal wording | Done by a separate worker: Moderate refusals name the level and the way forward. | #760 (merged) |
| User docs | `docs/security-moderate.md`, plus the Moderate parts of `docs/security-aggressive.md` and `docs/config.md`. | both PRs |

Still open before the code-blind run: #766 (D5), the Aggressive sign-off items, and the model-decline rule.

## 1. What Moderate promises, and what main does

### 1.1 The promise

| Source | Moderate is |
| --- | --- |
| Product spec, **P0 `/security` levels** | "Add strong protection without making normal agent work painful": untrusted external content and tool output; secrets and protected financial data kept out of model-visible context; trusted credential resolution; separate proposal, approval, signing and broadcast; the effect shown before protected actions; redacted audit; revocation and a kill switch. "Moderate is the recommended level once qualified." |
| Product spec, **Required trust boundaries** and **Release rule** | The added boundaries apply to Moderate and Aggressive: credentials only by label inside a trusted boundary, the model can't authorize itself, "Deterministic policy evaluates every proposed financial or disclosure action". Release rule: Moderate and Aggressive controls "must be deterministic policy around the model, not prompt text". |
| [Closed P0 plan](completed/main-2026-10-08-p0-security-levels.md) | "Added to the picker only as its protections land." Acceptance flows **Select Moderate (later)**: "Offered only when its protections are complete; unsupported routes fail visibly"; **Hostile content (Moderate)**: "No protected value or unauthorized action reaches model output or execution." Live repository: TensorCash for the Moderate protected-action workflow. |
| [P0 history](history/p0-security-levels-2026-10-06.md): the 2026-08-28 reconciled planning decisions and the [profile and failure contract](history/p0-security-levels-2026-10-06.md#profile-and-failure-contract) | Planning decisions: "Every level above Permissive requires the broker." Profile contract: Moderate composes existing denials with new policy; broker-only credentials; scoped derived financial values; isolated fetch and screened search; pause ingestion when the local classifier is down. |
| Travis, 2026-10-06 decision 2 (`sec-common.md`) | "Moderate is added as its protections land." |
| Travis, 2026-10-11, #245 | Keep the Moderate refusal: an additional-permission read of a protected credential or vault path is refused before the prompt, even with a person's approval. |
| Travis, 2026-10-11, #513 (with what #558 implemented) | Under Moderate only a person's fresh approval lifts write protection on shell/git start-up files and git hooks for that one command; Guardian, hooks, the approval cache and session/turn grants never do. Runtime entries are never writable. #558 applied the same rule to the project's `.codex` and `.agents` (the handoff's section 9 lists them too) and to git config (from #558 only). |
| Travis, 2026-10-12, #513 | A command a person approves to run outside the sandbox may write the runtime files (documented exception). |
| Travis, 2026-10-12, #603 (15) and (16) | (15) A one-command approval under Moderate keeps lifting the read-only Corbanu home. (16) Corbanu-home names are protected before they exist, also when the command works in the home. |
| Travis, 2026-10-13 (24-item decisions list in the coordinator's `todo.md`, outside the repository, entry "23:55") | 20: keep worktrees reached through a link read-only (#603); 22: #651 documented, entry by entry for shared folders; 23: accept #650's side effect (sandboxed `git worktree remove/prune` fails under Moderate and Aggressive). |
| Travis, 2026-10-10, #391 | `broker_model_auth` on by default for Aggressive first; "other levels later as a separate decision". It is off under Moderate today. |

The P1 plan says the milestone "covers PF-28-S02, PF-23 and the Moderate picker entry".

### 1.2 What is on main

| Area | State on main | Evidence |
| --- | --- | --- |
| Choosing Moderate | **Not possible.** The picker lists Moderate as "not available yet"; there is no review, preflight, confirmation, restart or downgrade path for it. The picker's Moderate summary ends "Not available yet." | `c1-picker.txt`; [PF-24-S03](../sprints/archive/p0-security-levels/pf-24-s03-flagged-security-picker.md) excluded Moderate; #750 |
| Reaching Moderate at all | Only `[security] version = 1, level = "moderate"` in config. [PF-23-S01's readiness note](../../qa/security-levels/sprints/PF-23-S01/activation-readiness.md) calls it (in the older key spelling `security_level = "moderate"`) "a test setting, not a product level". | `c1-*` |
| Untrusted content and taint (PF-30-S01..S03, PF-23-S01) | Merged behind `source_envelopes`. After tool, MCP, web, agent or memory content, protected commands (vault, credentials, policy, disclosure, value transfer, unseen code), MCP calls that change or send data and unclassified tools need a fresh person's answer, and typing into a running process is judged as one command since the untrusted content; cached or automatic approvals don't count. | PF-23-S01 record; tmux `c3-prompt`, `c4-prompt` ("… after untrusted content entered the session … a cached or automatic approval does not count") |
| Read protection | The Corbanu home's stores and `$HOME` credential files are unreadable to agent commands under Moderate, from the start of the session (#239, fixed by #244), and the secretless launch contract denies the vault store, sign-in files and config. Under Moderate a person's fresh approval lifts the PF-23 read denials for that run, but not the launch contract's. | `c3-result`, `c6`; PF-13-S07 saved-level route matrix v5: 0 leaks under saved Moderate on macOS and Linux, though in the Linux Moderate run the model declined the `$HOME/.ssh` probe, so the sandbox wasn't exercised there |
| Read refusal and write protection | #245 (additional-permission reads of protected paths refused, #526), #513 (#537, #558), #540 and #603 decision 16 (#627, with its leftovers in #697) implemented as decided; decisions 15 and 20 kept today's behaviour; runtime entries never writable. | Issue comments: tests, plus GLM 5.3 Flash tmux videos for #245 and #513 |
| Secretless launch, output gate, reflected scrubbing, destination policy | Merged behind their flags (`secretless_agent_launch`, `secret_output_gate`, `url_destination_policy`); PF-28-S02 archived with open items in P1. | P1 carried-forward table |
| Model keys through the broker | Off by default under Moderate (#391). Inspector: "Corbanu reads provider keys itself (broker_model_auth off)". | `c1-inspector` |
| Downgrade, restart, inheritance (PF-23-S03) | Core handles Moderate (`next start Moderate`, children inherit); no TUI path to confirm it. | `c1-inspector` Live policy row |
| Kill switch and revocation | Built and tested for Aggressive (PF-25-S02, #445, #507); the `g` screen is reachable at any level. Not exercised under Moderate. | — |
| Inspector | Moderate rows contradict enforcement (#749). | `c1-inspector`, `c5-*` |
| Refusal wording | Aggressive refusals are explained (#439); Moderate ones read only "Operation not permitted" (#748). | `c3-result`, `c6` |
| Commands outside the sandbox | The prompt offers them and the docs say they can run; protected launch refuses them (#747). | `c2-*` |
| Unbuilt P1 controls (PF-31, PF-32, PF-34 to PF-40) | Not built. The inspector shows four as "not available": screened search (PF-32), brokered browser login (PF-37), quarantine (PF-34) and Agent Sweep (PF-40). Financial preview and mandates (PF-38, PF-39), isolated retrieval (PF-31) and the detectors (PF-35, PF-36) have no row at all. | `c5-inspector-scrolled` |
| User docs | No Moderate page; `docs/security-aggressive.md` and `docs/config.md` mention Moderate in passing. | — |

The spot checks are in [qa/security-levels/moderate-readiness-20261014/](../../qa/security-levels/moderate-readiness-20261014/README.md):
candidate `4b6dd55c85` (the third Aggressive run's package), a disposable home, `CORBANU_TEST_NO_NATIVE_KEYRING=1`,
the milestone flags plus config Moderate, GLM 5.3 Flash on `zai`.

## 2. Gaps

### 2.1 Defects filed by this assessment

| Issue | Gap | Plan row |
| --- | --- | --- |
| #750 | Moderate can't be chosen in `/security`: no picker entry, review, transition or downgrade, and no sprint owns them. | P1 "Moderate ships" ("the Moderate picker entry"); P0 flow "Select Moderate (later)" |
| #747 | Under Moderate an approved command that writes outside the project is refused by "Protected launch", though the prompt, inspector and docs say it can run outside the sandbox (#739 analogue). Needs a decision (D3). | P1 "Moderate ships"; #513 decision of 2026-10-12 |
| #748 | Moderate refusals aren't attributed to the level ("Operation not permitted" only; #439 analogue). | P1 "Moderate ships"; intent "refusals are explained" |
| #749 | Inspector rows under Moderate contradict enforcement: "Active this run: Permissive" next to "Moderate in force", vault store "readable" though denied, outside-sandbox runs, a Nested agents row that names only Aggressive (#437 analogue). | P0 invariant "The UI never shows a protection as active when it is not" |

### 2.2 Open issues that also apply to Moderate

| Issue | Why it matters for Moderate |
| --- | --- |
| #739 | Same root as #747 under Permissive; one decision should cover both. |
| #603 | Open only for review residuals. Decision 16 is implemented; decisions 15 and 20 kept today's behaviour. Not blocking. |
| #594 | Linux: a Corbanu home reached through a link can be replaced by a protected command (Moderate and Aggressive). D225 (2026-10-28): when the link sits in a folder commands can write, Moderate warns at start-up and Aggressive refuses to start, be chosen or move to such a folder; the fix is `CORBANU_HOME` set to the real folder. |
| #674, #675 | macOS `:minimal` platform-default reads; other worktrees' `.git` files. Apply to every protected level. |
| #607, #716, #743, #745, #348 | Linux sandbox placeholders. Linux isn't in the milestone run (D7), so they are named limits. (The `pf_23_s01` flakes #615 and #673 were fixed by #742 on 2026-10-14.) |
| #713, #721 (fix #724 awaiting sign-off) | apply_patch foreign paths; `shell_command` on the host for remote environments. Remote environments are out of the milestone run; record as limits. |
| #732 | Linux database-folder scan below a per-entry database folder. |
| #505 / PR #662 (needs security sign-off) | When Moderate's protections apply after confirming (D4). |

No open issue carries a Moderate label; the repository has no level labels.

### 2.3 Aggressive cases whose Moderate analogue is untested

The three Aggressive runs ([run 1](../../qa/security-levels/milestones/aggressive-20261010/README.md),
[run 2](../../qa/security-levels/milestones/aggressive-20261011-rerun/README.md),
[run 3](../../qa/security-levels/milestones/aggressive-20261013-third/README.md)) never ran under Moderate; the
packets told the designer it was "not available", and AGG-35 only checked that choosing it is refused. Moderate analogues with no code-blind coverage:

| Aggressive case(s) / defect | Moderate question no run has answered |
| --- | --- |
| AGG-01, 02, 03, 29, 35 (picker, review, confirm, restart, persistence) | All of it, once #750 exists. |
| AGG-04, #443, #505 | Which Moderate protections apply at confirmation and which at the next start. |
| AGG-05, 09, 11, 14 (protected reads and writes) | The pre-/post-untrusted-content split, the fresh-approval lift (#239), the #245 refusal and the #513 persistence lift. Covered so far by integration tests and sprint tmux videos only. |
| AGG-10, #736 (`security find-generic-password`) | Whether keychain access is a protected action under Moderate, and what an approval then allows. |
| AGG-12, 13 (network, web search) | Moderate's network stance: the inspector says "off inside the sandbox, on for commands that run outside it", but #747 means nothing runs outside. |
| AGG-15, 16, #434 (environment, output gate) | Whether a vault-managed value can reach the screen or the model under Moderate. |
| AGG-19, #441 (nested launches) | The nested-agents setting is Aggressive-only (D5). |
| AGG-20 (child agents) | Inheritance of Moderate by `/spawn` children. |
| AGG-21 (hostile README asks to switch the level; the agent is asked to edit the security state) | Whether hostile content can lower Moderate, and whether Moderate's fresh-approval lifts open any path to the level's own files. |
| AGG-22, #445, #507, AGG-R06, R14 (kill switch) | The spec promises a kill switch for Moderate; never exercised there. |
| AGG-24, 34, R09 (inspector) | #749. |
| AGG-25, E1 (preflight) | Whether Moderate has a preflight and what blocks it. |
| AGG-26, #436 (downgrade with "restart now") | Moderate → Permissive and Moderate ↔ Aggressive transitions. |
| AGG-27, A01 (routes) | Both Z.AI routes under Moderate, with the broker off or on (D2). |
| AGG-30, AGG-33 replay, AGG-T01, T02, #626 (remembered approvals) | Whether Permissive "don't ask again" rules apply under Moderate before untrusted content (D6). |
| AGG-R13, #623 (links into `.codex`/`.agents`) | The link refusal is Aggressive-only; under Moderate a fresh approval may lift `.codex`/`.agents` writes (#513). |
| AGG-T01, T02, #739 (they cover both #626 and #739) | #747. |
| (AGG-21 only, as level tampering) | Moderate's own core: hostile instructions in a file, tool output or MCP result, followed by a request for a protected action. Only AGG-21 put hostile instructions in a file, and only to loosen the level and network; no case used tool or MCP output, or followed hostile content with a credential or vault action. |

## 3. Proposed milestone plan

### 3.1 Prerequisites, in order

1. **Travis's decisions D1–D7** (section 5). The picker sprint needs D1–D6.
2. **Aggressive sign-off, or at least #736 assessed and #737 fixed** (decision 1 in the qual-aggr3 result, `todo.md` entry "04:10" on 2026-10-14). The protected-path code is shared; running Moderate earlier would re-find Aggressive defects.
3. **Moderate picker entry sprint** (#750) under the per-sprint gate (focused `just test`, GLM 5.3 Flash tmux, one Opus 5.5 High review, SOP videos). Scope: the Moderate row, its review screen listing every control in force and every "not available" one, preflight, confirm/Esc/restart, downgrade to Permissive and switching with Aggressive, plus #748 and #749, and #747 per D3. The decided defaults (D2, D5, D6) go into this sprint too. One worker, the TUI and security lanes.
4. **#662 (#505)** if D4 says protections apply at confirmation for Moderate as for Aggressive.
5. **User docs:** a short `docs/security-moderate.md` (or a Moderate section) describing only behaviour the sprint verified, per the P0 documentation rule.
6. **Packaged candidate** from the main commit after 3–5, built as for the Aggressive runs.

### 3.2 What a fresh code-blind designer needs (packet outline only)

The packet is written by the coordinator from product requirements and owner decisions, never from this file,
issues, code or Aggressive results. A fresh read-only agent designs the cases and the design is frozen before the VM
starts. Outline, following the shape of the first run's
[intent.md](../../qa/security-levels/milestones/aggressive-20261010/design/packet/intent.md):

1. **Who and why:** an everyday developer who wants the agent protected from instructions hidden in files, tool
   output and web or MCP content, and wants secrets kept away from the model, without a lockdown.
2. **What a person who picks Moderate should experience:** choosing it (review of differences, preflight, Esc
   changes nothing, only a person can change it); when it applies (per D4); what stays normal (ordinary reads,
   edits and commands in the project); what changes after untrusted content (protected actions ask a person fresh,
   remembered or automatic approvals don't count); what is never readable (credential files, the vault, Corbanu's
   own stores), what a fresh approval can and can't lift (#245, #513, #603), outside-sandbox runs (D3); secrets
   kept out of commands' environment and output; model keys (D2); child and nested agents (D5); kill switch and
   revocation; the inspector; refusals explained; switching to Aggressive or back to Permissive; errors and
   recovery.
3. **What does not change:** model and provider choice, both Z.AI routes, MCP servers and hooks (not contained),
   `/permissions` under Permissive.
4. **Constraints:** macOS on Apple silicon, the packaged candidate, the security flags on, level starting at
   Permissive and chosen in `/security`; GLM 5.3 Flash (GLM 5.2 only with a reason); the product's model may decline
   on its own; fixtures (below); no Keychain prompt may ever appear; no target number of cases; features not built
   read "not available" and are not tested (D1).
5. **Screens from the candidate** (captured in the guest if it is up, otherwise on the host as in run 3, recorded
   as a limit): picker with Moderate highlighted; Moderate review (top and bottom); saved and after restart;
   inspector under Moderate (top and scrolled); an approval before and after untrusted content; a fresh-approval
   prompt for a protected action; an explained refusal; Moderate → Aggressive and Moderate → Permissive reviews; the
   kill-switch screen; error screens (preflight blocked, broker unavailable if D2 turns it on).
6. **Owner answers** to the designer's questions, written as product requirements.

### 3.3 Harness reuse from the Aggressive runs

| Reuse as is | From | Change for Moderate |
| --- | --- | --- |
| Guest provisioning, erase and restore (`provision.sh`, `restore.sh`, `restage.sh`, `erase-etc-codex.sh`, `setclock.sh`) | run 3 `harness/` | Paths (`aggressive-third-20261013` → a new run folder). Restore the guest to baseline first if the last run didn't. |
| Executor accounts, root launcher, per-uid pf, 85-probe isolation gate, native-prompt guard | run 3 `harness/` (`agms-launch`, `agms-probe`, `expected.json`, `run_group.sh`, `run_seq.sh`, `status.sh`) | None expected. |
| Z.AI mediator (`agms_mediator.py`) and owner-approved key path | runs 1–3 | Label rows by lane, to close the attribution limit all three runs recorded. |
| TUI driver (`agms-tui`) | run 3 | `agms-tui fixture` printed fixture contents, so the synthetic `.env` token reached 27 executor transcripts in run 3; print hashes instead. |
| Product profile (`product-config.toml`) | run 3 | Same flags; no `[security]` level (chosen in the picker); `broker_model_auth` per D2. |
| Design/result tooling (`make_design3.py`, `make_results.py`, `make_dispositions.py`, `replays.json`, review ledger and budget) | run 3 `tools/` | New prefix (MOD-); one fresh design, not an amendment table. |
| Fixture `ledger-notes` with synthetic `.env` and `outside-notes.txt` | runs 1–3 | Provisional, to be set from the frozen design (the designer is not shown this list): sources of untrusted content the cases may need, such as project files, command output and a small local stdio MCP server run with `/usr/bin/python3`. Web content can't be served: the guest's pf allows only the mediator. |
| Evidence check by fresh agents, budget 5; extensions only with Travis or coordinator authorization (AGENTS.md, 2026-10-14) | runs 1–3 | Settle the model-decline rule first (decision 3 in the qual-aggr3 result) so Moderate passes are judged the same way. |

Lessons the Aggressive runs recorded and Moderate should apply from the start: capture packet screens in the guest
when possible; stop replays chosen after seeing results (run 3's evidence check flagged this); keep `utmctl stop`
output and a final policy capture (run 2 lacked both); amend cases when an owner decision changes expected screens
rather than letting them fail (AGG-01, AGG-25); record the product model's refusals verbatim.

### 3.4 VM "macOS 3"

- The same guest (`agent@192.168.64.3`, pinned host key), started with
  `/Applications/UTM.app/Contents/MacOS/utmctl start "macOS 3"` only after the design is frozen; "macOS 2" stays stopped.
- Never delete or recreate the VM; erase guest data programmatically (`restore.sh`) at the start and the end, then
  `utmctl stop "macOS 3"` and save its output.
- Re-measure the isolation gate before the first group and the probes before every group; lanes run one at a time.
- The Z.AI key reaches the guest only through the mediator path approved for run 1.
- Not available in the guest: Claude Code (kill switch over a Claude pane), GitHub and TensorCash (D7), the web.

### 3.5 Estimated effort

| Step | Estimate | Basis |
| --- | --- | --- |
| Picker sprint (#750) with #747–#749 | 1.5–2.5 days, one worker | Judgement: PF-24-S02 and PF-25-S01 each went from launch to merge within 2026-10-07 (about 6 and 3 hours; the coordinator's `todo.md` and `git log`). The range allows for the larger scope (review screen, preflight, downgrade, switching), #747–#749 and review rounds |
| Docs page, candidate build and package | 0.5 day | Run 3 candidate build |
| Packet, fresh design, owner questions, freeze | 0.5 day | Run 3: about 2 hours 35 minutes from packet capture (14:39Z) to the VM start (17:14Z), including the wait for the owner's answers |
| VM execution | 8–10 hours | Run 3: 64 cases in 32 groups plus 5 replays took 8.3 hours (17:18Z–01:37Z). A first Moderate design will be a similar size, and untrusted-content cases need extra steps. |
| Evidence checks and record PR | 0.5 day | Run 3: three checks plus an extension, then a docs-only PR |
| Expected re-run after fixes | 1–2 days per cycle | Aggressive needed three runs; reuse the design with amendments as runs 2 and 3 did |

Total: the first five rows add up to about 3.4–4.4 working days for one cycle; with one re-run cycle, about 4.5–6.5.
Travis's decision time comes on top.
Z.AI cost stays on GLM 5.3 Flash.

## 4. Sources

Product spec (**P0 `/security` levels**, **Release rule**, **Required trust boundaries**); the closed
[P0 plan](completed/main-2026-10-08-p0-security-levels.md) and its
[history](history/p0-security-levels-2026-10-06.md); the [P1 plan](active/p1-security-hardening.md); sprint records
PF-23-S01..S03, PF-24-S02/S03, PF-25-S01/S02 in [the archive](../sprints/archive/p0-security-levels/); the three
Aggressive milestone records; root `AGENTS.md` (code-blind functional test design) and
[qa/code-blind-functional/](../../qa/code-blind-functional/README.md); issues #245, #391, #513, #540, #603 and their
owner-decision comments, and #739; the management handoff (section 9) and the coordinator's `todo.md` decision log.

## 5. Decisions for Travis

| # | Decision | Options | Recommendation |
| --- | --- | --- | --- |
| D1 | What Moderate ships with | (a) the controls that exist today, with the unbuilt ones (financial preview and mandates, isolated retrieval, screened search, browser login, quarantine, detectors, Agent Sweep) shown "not available", as Aggressive did; the picker sprint then adds the rows the inspector lacks today (PF-31, PF-35, PF-36, PF-38, PF-39); (b) wait for those P1 sprints | (a). The spec's financial and retrieval promises can't be met before PF-31 to PF-40, and "not available" is the agreed way to say so. |
| D2 | Model keys through the broker under Moderate (#391 left it open) | (a) on by default, as under Aggressive, a setting you make still wins; (b) stay off | (a). The spec says every level above Permissive uses the broker, and #413 already turns it on by default under Aggressive on macOS, Linux and Windows, failing closed. |
| D3 | #747: commands a person approves to run outside the sandbox | (a) let them run under Moderate (needs a launch-contract exception and security sign-off); (b) Moderate never runs outside the sandbox, and the prompt, inspector and docs stop offering it | (b), the same direction as the #739 recommendation. The 2026-10-12 runtime-file exception then never applies while `secretless_agent_launch` is on. |
| D4 | When Moderate applies after confirming | (a) like Aggressive today: some at once, the rest at the next start; (b) approvals at confirmation, as #662 does for Aggressive, the rest (sandbox, network, environment, broker, child-agent rules) at the next start; (c) everything at confirmation | (b), once #662 is signed off: one rule for both levels. (c) would need new work for both levels. |
| D5 | Agents started by an agent under Moderate | (a) refuse, as Aggressive's default; (b) allow, with Moderate enforced in the new agent | (b). Moderate keeps normal work going, and children already inherit the level. |
| D6 | "Don't ask again" rules saved under Permissive, used under Moderate (#626 analogue) | (a) keep them for ordinary commands; after untrusted content they already don't count for protected ones; (b) suspend them as under Aggressive | (a). Suspending them would make Moderate as noisy as Aggressive. |
| D7 | Scope of the milestone run | (a) macOS only, synthetic project as the TensorCash stand-in, Claude-pane cases out of scope, limits named; (b) add a Linux run on glitch and real TensorCash access | (a) for "Moderate ships"; Linux and the live repository before flag removal. |

Also carried from qual-aggr3 and needed before the Moderate run: the model-decline rule (its decision 3) and the
Aggressive sign-off (its decision 1).
