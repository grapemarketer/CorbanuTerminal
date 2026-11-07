---
title: "1. P1 security hardening"
status: active
change_class: product-initiative
priority: P1
owner: "Jim Ricketts"
parallel_sprint_limit: 3
parallel_lanes: "broker, windows-host, qualification"
integration_owner: "Codex /root security round-five coordinator"
activation_authority: "Product authority defined in the product specification"
activation_basis: "Travis Good's 2026-10-06 decision 3 (deferred security features form this plan) and his 2026-10-08 decision to close the P0 plan and activate this one in slot 1."
target_release: "TBD"
deadline: "TBD"
created: 2026-10-06
updated: 2026-10-16
product_spec:
  file: docs/corbanu-product-spec.md
  heading: "P0 /security levels"
  requirement_excerpt: "Existing approval, sandbox, vault, wallet, tool, network, and agent policies are unchanged."
implementation_worktrees:
  - path: "/Volumes/CorbanuDrive/Corbanu/worktrees/pf13-s07-20261007"
    branch: "feat/pf-13-s07-qualification-20261007"
    base_commit: "64137b71894fb15fb9d6bf754dc69c41d4cb0406"
  - path: "/Volumes/CorbanuDrive/Corbanu/worktrees/aggressive-ms-20261010"
    branch: "qa/aggressive-milestone-20261010"
    base_commit: "9ead7a57558f8f0639511cc5cbcc8ec726756ed3"
---

# P1 security hardening

Policy: repository-root `AGENTS.md`. Plan lifecycle: `docs/plans/index.md`.

Holds the security features Travis deferred on 2026-10-06 (decision 3) from the
[closed P0 `/security` plan](../completed/main-2026-10-08-p0-security-levels.md), plus that plan's unfinished
milestones and open items (carried forward when Travis closed it on 2026-10-08). The full feature contracts are in the
[P0 history file](../history/p0-security-levels-2026-10-06.md#expanded-feature-contracts).

## Activation record

| Field | Value |
| --- | --- |
| Status | **Active**, slot 1 of 3 (from 2026-10-08, replacing P0 `/security` levels) |
| Authoritative decision | Travis, 2026-10-08: close P0, activate this plan in its slot; 2026-10-06 decision 3 defines its scope |
| Delivery owner / integration owner | Jim Ricketts / Codex /root security coordinator |
| Delivery gate | Unchanged from P0: per-sprint gate (focused tests, GLM tmux run on `glm-5.3-flash` since 2026-10-10, Opus 5.5 High review, SOP videos); code-blind VM run and human sign-off at milestones |
| Target release / deadline | TBD / TBD (none set by Travis) |

## Carried forward from P0

Open when P0 closed on 2026-10-08. Nothing here is done; each item names where it goes next.

| Item | From | Next step |
| --- | --- | --- |
| [PF-13-S07](../../sprints/current/p1-security-hardening/pf-13-s07-integrated-credential-boundary-qualification.md) final archive | P0 convergence | Rerun the Linux canary on the current candidate; archive after the Aggressive milestone sign-off |
| Milestone **Aggressive ships** | P0 delivery gate | Code-blind VM run done 2026-10-10/11 on `9ead7a5755` ([record](../../../qa/security-levels/milestones/aggressive-20261010/README.md)): 17 passed, 15 failed, 7 blocked of 39; defects #434–#445. Re-run 2026-10-12 on `6ebfa1f47c` after the fixes and the 2026-10-11 decisions ([record](../../../qa/security-levels/milestones/aggressive-20261011-rerun/README.md)): 41 passed, 10 failed, 6 blocked of 57; defects #620–#626; last independent evidence check r3 failed on mechanical points, corrected but unverified (review budget used). Third run 2026-10-13/14 on `4b6dd55c85` after #637–#664 and the 2026-10-13 decisions ([record](../../../qa/security-levels/milestones/aggressive-20261013-third/README.md)): 51 passed, 6 failed, 7 blocked of 64; defects #736 #737 #739, advisory #740. Targeted re-run 2026-10-14 on `4440d2d4ce` after #752 and #662 and the 2026-10-14 decisions ([record](../../../qa/security-levels/milestones/aggressive-20261014-targeted/README.md)): 20 cases re-run, 12 passed, 1 failed (AGG-04, #792), 7 blocked; combined 53 passed, 3 failed (AGG-04; T01/T02, #739), 8 blocked of 64; advisory #787 #789. Second targeted re-run 2026-10-14/15 on `df3d8c5d1c` ([record](../../../qa/security-levels/milestones/aggressive-20261014-targeted2/README.md), #819): combined 54 passed, 2 failed, 4 blocked, 4 accepted by Travis (AGG-18, R07, R10, 20); defects #816 #817 #818. Third targeted re-run 2026-10-15 on `4c9b0a0cf0` after #813, #834 and #835 ([record](../../../qa/security-levels/milestones/aggressive-20261015-targeted3/README.md), #868): combined **58 passed, 0 failed, 2 blocked (AGG-R14 result 2, #867; AGG-R06), 4 accepted** of 64. **Signed off by Travis on 2026-10-16** (decision D10, "all as recommended"; [comment on #867](https://github.com/CorbanuCore/CorbanuTerminal/issues/867)), with known limits accepted: #867 (no on-screen status for a model-spawned child agent the kill switch stopped; D8) and D9 (the second AGG-R14 attempt counts); AGG-R06 (no Claude Code in the guest); #838's Linux create-only limit (D7); the Linux user-namespace breakage list of #876 (rootless containers, `bwrap`/`unshare`, Flatpak, Chrome/Electron with their sandbox on, Nix's build sandbox, 32-bit programs; D13). macOS only, as every run; sign-off record in the [third targeted run's README](../../../qa/security-levels/milestones/aggressive-20261015-targeted3/README.md#sign-off-2026-10-16). Follow-up after the sign-off: second local-model re-run 2026-10-21 on `bc1deda532` ([record](../../../qa/security-levels/milestones/localmodel-rerun2-20261021/README.md), [#1191](https://github.com/CorbanuCore/CorbanuTerminal/pull/1191)) of the four accepted cases: AGG-18 and AGG-R10 passed, AGG-R07 failed on the model naming the home's protected entries from the product's own permission text (#1181), AGG-20 blocked under its named limit; counted by D113 (2026-10-21): 60 passed, 1 failed, 3 blocked. D110 (2026-10-21) accepts AGG-R07's behaviour as intended and amends its pass rule ([amendment 7](../../../qa/security-levels/milestones/aggressive-amendment7-20261021/README.md)); judged against it, AGG-R07 passed. Combined now **61 passed, 0 failed, 3 blocked (AGG-R06; AGG-R14 result 2, #867; AGG-20, named limit)** of 64; the sign-off (D10) is unchanged |
| Milestone **Moderate ships** | P0 delivery gate | Same; covers PF-28-S02, PF-23 and the Moderate picker entry. Readiness: [readiness assessment](../moderate-ships-readiness.md) (2026-10-14; gaps #747–#750, owner decisions D1–D7). First code-blind run 2026-10-15 on `df3d8c5d1c` ([record](../../../qa/security-levels/milestones/moderate-20261014/README.md), #844): 16 passed, 15 failed, 3 blocked of 34, not passed; defects #837–#847, fixes merged 2026-10-15/16. Targeted re-run 2026-10-16 ([record](../../../qa/security-levels/milestones/moderate-targeted-20261016/README.md), #971): 17 cases re-run, 12 passed, 2 failed (MOD-15, #941; MOD-28, #942), 3 blocked; owner dispositions D38–D43 (2026-10-19). Second targeted re-run 2026-10-20 on `8bf182de43` ([record](../../../qa/security-levels/milestones/moderate-targeted2-20261020/README.md), #1037): MOD-15 passed, MOD-28 accepted (D40); combined **30 passed, 4 accepted (MOD-09, MOD-21: D39; MOD-11: D43; MOD-28: D40), 0 failed, 0 blocked** of 34. **Signed off by Travis on 2026-10-20** (decision D61; [comment on #447](https://github.com/CorbanuCore/CorbanuTerminal/pull/447#issuecomment-6285047513)), with known limits accepted: MOD-09/21's product credential handling not reached in those runs (the hosted models declined; D39); MOD-28's revoke not testable (Moderate offers no session approval to revoke; D40); `.git` read-only under Moderate, so agents can't commit (D5 (c); MOD-11 and MOD-06's git part, D43; post-command `.git` check #926 as the follow-up); #838's Linux create-only limit (D7); the Linux user-namespace breakage list of #876 (D13; eased for one approved command by D27/D48, #975 and #1024). macOS only, stand-in project, no Claude panes (D7 (a)). Follow-up after the sign-off: local-model re-run of MOD-09/MOD-21 ([record](../../../qa/security-levels/milestones/moderate-localmodel-20261020/README.md), #1078): MOD-21 passed; MOD-09 failed on item 7 (protected launch keeps CLI-token files closed even after approval; wording #1077), every protection held. D72 (2026-10-20) accepts MOD-09's behaviour as intended (wording fixed by #1091 and #1095), so MOD-09 counts accepted: combined **31 passed, 0 failed, 3 accepted (MOD-09: D72; MOD-11: D43; MOD-28: D40)** of 34. Second local-model re-run 2026-10-21 on `bc1deda532` ([record](../../../qa/security-levels/milestones/localmodel-rerun2-20261021/README.md), [#1191](https://github.com/CorbanuCore/CorbanuTerminal/pull/1191)): MOD-09 items 5 and 7 meet D72 (D113); item 6 has never been exercised, so MOD-09 stays accepted. Sign-off record in the [second targeted re-run's README](../../../qa/security-levels/milestones/moderate-targeted2-20261020/README.md#sign-off-2026-10-20) |
| Milestone **flag removal** | P0 delivery gate | Code-blind run, sign-off, PF-21 Permissive compatibility against `3c1b2f6cbe11657ff4e3b72b11db029c9e7a92eb`, no critical finding open; then PF-26 |
| [PF-28-S02](../../sprints/archive/p0-security-levels/pf-28-s02-reflected-secret-response-scrubbing.md) items | archived 2026-10-08 | Latency targets (need product authority); per-type scrubbing; per-session stream state; recorded encoding/HEAD/HTTP2 limits |
| [PF-33-S02](../../sprints/archive/p0-security-levels/pf-33-s02-connection-pinning-and-bypass.md) items | archived 2026-10-08 | SearXNG private-service grant (with PF-32-S05); revoke runtime-approved hosts on open tunnels; live rebinding fixture |
| [PF-29-S01](../../sprints/archive/p0-security-levels/pf-29-s01-protected-mode-inventory.md) items | archived 2026-10-08 | MCP servers, hooks and notify "not contained"; Claude panes not inventoried; exec-provider sign-in consent; `.env` in another `-C` folder |
| [PF-29-S02](../../sprints/archive/p0-security-levels/pf-29-s02-human-secret-migration.md) items | archived 2026-10-08 | Migrate config literals, env variables and memories; revoke broker leases on migration |
| [PF-41-S01](../../sprints/archive/p0-security-levels/pf-41-s01-effective-security-inspector.md) items | archived 2026-10-08 | Full core/TUI suites in post-merge CI; observe both broker healths (never shown green until then); thread for launch-contract denials |
| PF-83 TUI bug: typed text can approve an open prompt with "don't ask again" | P0 TUI lane | Fixed by #174 (PR #187); verified on `9ead7a5755` by AGG-07 ([record](../../../qa/security-levels/milestones/aggressive-20261010/README.md)) |
| PF-83 TUI bug: "Enable full access?" defaults to Cancel, no message when cancelled | P0 TUI lane | Verified on `9ead7a5755` by AGG-31: Cancel is the default, and cancelling says "Full access was not enabled; permissions are unchanged." ([record](../../../qa/security-levels/milestones/aggressive-20261010/README.md)) |
| PF-83 TUI bug: declined request renders "You canceled…" and "Ran … (no output)" | P0 TUI lane | Verified fixed on `9ead7a5755` by AGG-08: "Did not run … / Approval declined", no "Ran" line ([record](../../../qa/security-levels/milestones/aggressive-20261010/README.md)) |
| PF-83 TUI bug: `zai-anthropic` `glm-5.2` lacked a max-output limit | P0 TUI lane | Verified fixed on `9ead7a5755` by AGG-A01: turns complete under Permissive and Aggressive ([record](../../../qa/security-levels/milestones/aggressive-20261010/README.md)) |
| PF-27-S06 unplaced follow-ups | [PF-27-S06](../../sprints/archive/p1-security-hardening/pf-27-s06-windows-broker-and-launch.md) (archived 2026-10-08) | File tools other than patches under the Windows launch contract; elevated-sandbox profile reads (`~/.git-credentials`, `.ssh`, `.npmrc`, `.config/gh`) |
| PF-27-S06/S07 Windows follow-ups | [S06](../../sprints/archive/p1-security-hardening/pf-27-s06-windows-broker-and-launch.md), [S07](../../sprints/archive/p1-security-hardening/pf-27-s07-windows-hardening-follow-ups.md) (archived 2026-10-08, accepted with known limits) | #300 decided fail closed, being implemented; #301/#304 decided rule-driven removal, draft PR #326; #323 open; #307 and #320 fixed (PRs #321, #327); PF-27-S08/S09 planned |
| PF-27-S09 follow-ups | [PF-27-S09](../../sprints/archive/p1-security-hardening/pf-27-s09-windows-model-client-auth.md) (archived 2026-10-10, accepted with known limits) | #389 passed with limits (PR #395); #390 fixed (PR #399); #391: `broker_model_auth` is on by default under Aggressive on macOS, Linux and Windows, an explicit setting wins, and it fails closed ([evidence](../../../qa/security/issue-391/README.md)). The flag stays default off for Permissive and Moderate, a separate later decision |
| PF-27-S08 follow-ups | [PF-27-S08](../../sprints/archive/p1-security-hardening/pf-27-s08-windows-broker-restricted-token.md) (archived 2026-10-09, accepted with known limits) | PF-27-S09 (stored-key reader, model auth) completed and archived 2026-10-10; Core creates `secrets/.vault.lock` and the broker locks it read-only (PR #363) |

## User pain

The P0 controls exist on main behind flags, but nobody has qualified them as a whole, Windows lags macOS and
Linux, and web retrieval, browser login, financial actions and audit still lack the protections the `/security`
levels promise. Until this plan lands, a user cannot rely on Moderate or Aggressive for those surfaces.

## Product intent and ideal flow

Same `/security` flow as P0: the user picks a level, sees the differences, and confirms. Each feature here extends
what Moderate and Aggressive actually protect; anything unbuilt reads "not available". Permissive stays today's
behaviour.

## Product linkage

| Field | Value |
| --- | --- |
| Product-spec heading | **P0 `/security` levels** — “Existing approval, sandbox, vault, wallet, tool, network, and agent policies are unchanged.” |
| Credential heading | **Required trust boundaries** — “Credentials are referenced by label and resolved only inside a trusted execution boundary.” |

## Scope

In: the P0 milestones and items carried forward above, PF-13-S07, and the features below (PF-26, PF-27-S06 and its
Windows follow-ups, PF-31, PF-32, PF-34 to PF-40, PF-41-S02).

Out: replacing `/permissions`; changing Permissive; letting a model choose or downgrade a level; conformance
claims to external standards.

## Invariants

The P0 invariants hold unchanged: Permissive and flag-off are today's product; only a human changes the level;
protected levels are deterministic; corrupt state fails visibly; downgrades invalidate incompatible authority;
children inherit the same or a stricter level; managed secrets stay out of model, env, argv, logs and artifacts;
unbuilt controls read "not available".

## Ownership and implementation worktrees

| Owner | Worktree | Branch | Base commit | Scope |
| --- | --- | --- | --- | --- |
| Jim Ricketts (qualification lane) | `/Volumes/CorbanuDrive/Corbanu/worktrees/pf13-s07-20261007` | `feat/pf-13-s07-qualification-20261007` | `64137b71894f` | PF-13-S07 (`ready`) |
| Windows-host gate owner | released | released | released | PF-27-S06 completed and archived 2026-10-08 |
| broker lane worker | released | released | released | PF-27-S07 completed and archived 2026-10-08 |
| broker lane worker (2026-10-08) | released | released | released | PF-27-S08 completed and archived 2026-10-09 |
| broker lane worker (2026-10-09) | released | released | released | PF-27-S09 completed and archived 2026-10-10 |

Lanes: **broker** (free; Windows hardening PF-27-S06–S09 archived), **windows-host** (real-Windows gate runs; free),
**qualification** (PF-13-S07 and the milestone runs). Lanes and coordinates are revised by the integration owner
as work starts; each executable record's coordinates must appear in front matter.

## Useful code references

| Path | Why |
| --- | --- |
| `codex-rs/network-proxy/src/credential_broker/`, `codex-rs/secret-broker-service/` | Broker (PF-27) |
| `codex-rs/process-hardening/`, `codex-rs/windows-sandbox-rs/` | Windows containment (PF-27-S06 to S09) |
| `codex-rs/core/src/security/` | Effective policy, launch contract, taint, protected surface |
| `codex-rs/tui/src/security/`, `codex-rs/tui/src/bottom_pane/security_view.rs` | `/security` picker and inspector |
| `scripts/security-credential-canary`, `qa/security-levels/` | Qualification harness and evidence |

## Features

| Feature | Contract (one line) |
| --- | --- |
| PF-13 | S07 only: integrated credential boundary qualification, carried forward from P0 on 2026-10-08. |
| PF-27 | Windows only: broker and secretless launch (S06, moved from P0 on 2026-10-06), its hardening follow-ups (S07), the broker's own token (S08) and model-client auth through the broker (S09), added 2026-10-08 when Travis approved fixing S06's four documented limits. |
| PF-26 | Final whole-program qualification: automated (S04), true-TUI and live repositories (S02), human acceptance and finished docs (S03). The P0 milestone gates replace it for the core. |
| PF-31 | Isolated public retrieval with no host-browser fallback; sealed downloads until exact human promotion. |
| PF-32 | Screened web facade: existing search, Exa, Brave and SearXNG adapters, private routing and bounded failover. |
| PF-34 | Render-aware sanitization, encrypted quarantine and safe human review. |
| PF-35 | Licensed offline CPU injection detector with leakage-free blind qualification (targets below). |
| PF-36 | Optional hosted detector: consent contract, bakeoff, safe local fallback; no vendor assumed. |
| PF-37 | Origin-bound brokered browser login with human MFA/CAPTCHA handoff and revocation. |
| PF-38 | Typed financial executor, full-effect preview and exact mandate, separate sign and broadcast. |
| PF-39 | Purpose-limited derived financial views and outbound disclosure, clipboard and export controls. |
| PF-40 | Agent Sweep: sanitized events, deterministic rules, isolated advisory reviewer, alerts and recovery. |
| PF-41 | S02 only: tamper-evident audit and safe support export. PF-41-S01 (inspector) is archived under P0. |

## Sprint execution map

Order is topological, carried over from the P0 plan. Every P0 core dependency is
archived except PF-13-S07, which now lives here.

| Order | Sprint | Outcome | Depends on |
| ---: | --- | --- | --- |
| 20 | [PF-35-S01](../../sprints/current/p1-security-hardening/pf-35-s01-classifier-corpus-and-evaluation.md) | Classifier corpus and leakage-free evaluation | PF-34-S04 |
| 21 | [PF-35-S02](../../sprints/current/p1-security-hardening/pf-35-s02-local-cpu-detector-artifact.md) | Reproducible local CPU detector artifact | PF-35-S01 |
| 42 | [PF-27-S06](../../sprints/archive/p1-security-hardening/pf-27-s06-windows-broker-and-launch.md) | Windows broker and secretless launch (completed and archived 2026-10-08; accepted with known limits; PR #312) | PF-27-S02 |
| 43 | [PF-27-S07](../../sprints/archive/p1-security-hardening/pf-27-s07-windows-hardening-follow-ups.md) | Windows hardening follow-ups: new threads protected at creation, `CODEX_HOME` deny removed on flag-off (completed and archived 2026-10-08; accepted with known limits; PR #316) | PF-27-S06 |
| 44 | [PF-27-S08](../../sprints/archive/p1-security-hardening/pf-27-s08-windows-broker-restricted-token.md) | Windows broker confined by its own restricted token or AppContainer (completed and archived 2026-10-09; accepted with known limits; PRs #333, #360) | PF-27-S07 |
| 45 | [PF-27-S09](../../sprints/archive/p1-security-hardening/pf-27-s09-windows-model-client-auth.md) | Windows model-client auth through the broker (completed and archived 2026-10-10; accepted with known limits; PRs #363, #379, acceptance #384) | PF-27-S05, PF-27-S07 |
| 46 | [PF-31-S01](../../sprints/current/p1-security-hardening/pf-31-s01-pinned-retriever-isolation.md) | Pinned retriever artifact and sandbox | PF-33-S02, PF-27-S02, PF-31-S04 |
| 47 | [PF-31-S02](../../sprints/current/p1-security-hardening/pf-31-s02-bounded-fetch-no-fallback.md) | Bounded fetch adapter with no host fallback | PF-31-S01, PF-30-S01 |
| 48 | [PF-31-S03](../../sprints/current/p1-security-hardening/pf-31-s03-download-quarantine-promotion.md) | Download quarantine and human file promotion | PF-31-S02, PF-24-S01 |
| 49 | [PF-34-S01](../../sprints/current/p1-security-hardening/pf-34-s01-render-aware-sanitization.md) | Render-aware content sanitization | PF-31-S02, PF-30-S01, PF-34-S04 |
| 50 | [PF-35-S03](../../sprints/current/p1-security-hardening/pf-35-s03-calibration-and-ingress-gate.md) | Calibrated detector and ingress enforcement | PF-35-S02, PF-34-S01, PF-30-S03, PF-23-S01 |
| 51 | [PF-34-S02](../../sprints/current/p1-security-hardening/pf-34-s02-quarantine-state-and-store.md) | Quarantine state and encrypted retention | PF-35-S03, PF-41-S03 |
| 52 | [PF-34-S03](../../sprints/current/p1-security-hardening/pf-34-s03-safe-quarantine-review.md) | Safe quarantine review and recovery | PF-34-S02, PF-24-S01 |
| 53 | [PF-32-S01](../../sprints/current/p1-security-hardening/pf-32-s01-web-facade-and-registry.md) | Stable web facade and provider registry | PF-34-S03, PF-31-S03, PF-13-S05 |
| 54 | [PF-32-S02](../../sprints/current/p1-security-hardening/pf-32-s02-existing-search-and-native-bypass.md) | Existing search adapter and native bypass closure | PF-32-S01 |
| 55 | [PF-32-S03](../../sprints/current/p1-security-hardening/pf-32-s03-exa-search-adapter.md) | Exa brokered search adapter | PF-32-S02 |
| 56 | [PF-32-S04](../../sprints/current/p1-security-hardening/pf-32-s04-brave-search-adapter.md) | Brave brokered search adapter | PF-32-S02 |
| 57 | [PF-32-S05](../../sprints/current/p1-security-hardening/pf-32-s05-searxng-search-adapter.md) | SearXNG brokered search adapter | PF-32-S02 |
| 58 | [PF-32-S06](../../sprints/current/p1-security-hardening/pf-32-s06-privacy-routing-and-failover.md) | Private query routing and bounded failover | PF-32-S03, PF-32-S04, PF-32-S05 |
| 59 | [PF-36-S01](../../sprints/current/p1-security-hardening/pf-36-s01-hosted-detector-consent-contract.md) | Optional hosted detector consent contract | PF-35-S03, PF-33-S02 |
| 60 | [PF-36-S02](../../sprints/current/p1-security-hardening/pf-36-s02-hosted-bakeoff-and-local-fallback.md) | Hosted detector bakeoff and safe local fallback | PF-36-S01 |
| 61 | [PF-37-S01](../../sprints/current/p1-security-hardening/pf-37-s01-origin-bound-browser-login.md) | Origin-bound brokered browser login | PF-31-S03, PF-28-S02, PF-30-S03, PF-34-S03 |
| 62 | [PF-37-S02](../../sprints/current/p1-security-hardening/pf-37-s02-human-auth-handoff-lifecycle.md) | Human authentication handoff and session revocation | PF-37-S01, PF-25-S02 |
| 63 | [PF-38-S01](../../sprints/current/p1-security-hardening/pf-38-s01-typed-financial-executor.md) | Typed financial executor and deterministic limits | PF-30-S03, PF-27-S02, PF-18-S01 |
| 64 | [PF-38-S02](../../sprints/current/p1-security-hardening/pf-38-s02-full-effect-preview-and-mandate.md) | Full-effect financial preview and exact mandate | PF-38-S01, PF-25-S01 |
| 65 | [PF-38-S03](../../sprints/current/p1-security-hardening/pf-38-s03-sign-broadcast-and-receipts.md) | Separate signing broadcasting and idempotent receipts | PF-38-S02, PF-41-S03 |
| 66 | [PF-39-S01](../../sprints/current/p1-security-hardening/pf-39-s01-protected-financial-derived-views.md) | Protected financial derived views | PF-28-S01, PF-38-S01 |
| 67 | [PF-39-S02](../../sprints/current/p1-security-hardening/pf-39-s02-outbound-disclosure-controls.md) | Outbound disclosure clipboard and export controls | PF-39-S01, PF-30-S03, PF-32-S06 |
| 68 | [PF-40-S01](../../sprints/current/p1-security-hardening/pf-40-s01-sweep-events-and-rules.md) | Agent Sweep sanitized events and deterministic rules | PF-30-S03, PF-38-S03, PF-39-S02, PF-41-S03 |
| 69 | [PF-40-S02](../../sprints/current/p1-security-hardening/pf-40-s02-isolated-sweep-reviewer.md) | Isolated advisory Agent Sweep reviewer | PF-40-S01, PF-36-S01 |
| 70 | [PF-40-S03](../../sprints/current/p1-security-hardening/pf-40-s03-sweep-alerts-and-recovery.md) | Agent Sweep alerts revocation and recovery | PF-40-S02, PF-25-S02 |
| 72 | [PF-41-S02](../../sprints/current/p1-security-hardening/pf-41-s02-tamper-evident-security-audit.md) | Tamper-evident audit and safe support export | PF-41-S01, PF-41-S03 |
| 73 | [PF-13-S07](../../sprints/current/p1-security-hardening/pf-13-s07-integrated-credential-boundary-qualification.md) | Integrated credential boundary qualification (gate passed 2026-10-07; archive after the Aggressive milestone) | PF-13-S05, PF-13-S06, PF-27-S02, PF-28-S02, PF-29-S02, PF-33-S02 |
| 74 | [PF-26-S04](../../sprints/current/p1-security-hardening/pf-26-s04-final-automated-qualification.md) | Final integrated automated security qualification | PF-26-S01, PF-13-S07, PF-21-S02, PF-23-S03, PF-25-S02, PF-36-S02, PF-41-S02 |
| 75 | [PF-26-S02](../../sprints/current/p1-security-hardening/pf-26-s02-true-tui-and-live-repository-qualification.md) | True-TUI and live-repository qualification | PF-26-S04 |
| 76 | [PF-26-S03](../../sprints/current/p1-security-hardening/pf-26-s03-human-acceptance-finished-docs-and-release-evidence.md) | Human acceptance, finished docs, and release evidence | PF-26-S02 |

PF-35-S01 keeps its external-campaign status: DeepSeek API generation through
Corbanu Terminal (Travis, 2026-09-11), fine-tuning on the RTX PRO 6000, offline
CPU inference.

### Local classifier qualification targets

PF-35's qualification baseline, carried from the P0 plan. Changing a target needs product review.

| Measure | Target |
| --- | --- |
| Benign false positives | ≤0.1% on ≥100,000 held-out benign segments, with confidence interval |
| Known-family detection | ≥80% at that low-FPR operating point |
| Unseen-source/evasion detection | ≥65% at the same threshold; every miss remains policy-contained |
| Benign position/trigger perturbations | ≤2 percentage-point rejection increase |
| CPU envelope | p95 ≤50 ms per 2,048-token segment; peak RSS ≤512 MiB; model ≤300 MiB |
| Privacy | No real customer secrets/protected financial records in corpus, hosted payloads, metrics or artifacts |
| Deterministic safety | All critical unauthorized-disclosure/action cases deny, including forced detector misses |

## Acceptance flows

| Flow | Pass criterion |
| --- | --- |
| Flag off | Product behaves and renders exactly as before |
| Aggressive / Moderate milestones | Code-blind VM run passes and a named human signs off |
| Windows protected launch | No managed secret in agent env, argv or process memory; measured on a real Windows host |
| Hostile web content (PF-31, PF-34, PF-35) | No protected value or unauthorized action reaches output or execution |
| Financial action (PF-38, PF-39) | Full-effect preview, exact human mandate, separate sign and broadcast |
| Flag removal | Permissive compatibility proven; no critical finding open |

## Implementation sequence

1. Qualification lane: Aggressive milestone signed off 2026-10-16 (Travis, after the third targeted re-run, #868); then the PF-13-S07 archive (after the Linux canary re-run); Moderate milestone signed off 2026-10-20 (Travis, D61, after the second targeted re-run, #1037); then the flag-removal milestone.
2. Broker lane: PF-27-S06 to S09 completed (S06 and S07 2026-10-08, S08 2026-10-09, S09 2026-10-10); free.
3. The deferred features in the map order above, each under the per-sprint gate.
4. Flag removal milestone, then PF-26-S04 → S02 → S03.

## Automated evidence

| Check | Command | Result |
| --- | --- | --- |
| Governance | `python3 docs/plans/check.py && python3 docs/sprints/check.py` | per PR |
| Per sprint | `cd codex-rs && just test -p <affected-crate>` after `just fmt` / `just fix` | per sprint record |
| Windows | `windows-security-probes` workflow on `windows-2022` | per PR touching Windows code |
| Permissive compatibility | PF-21 harness against `3c1b2f6cbe11657ff4e3b72b11db029c9e7a92eb` | pending, each milestone |

## True-TUI evidence

Per sprint: GLM tmux run (`glm-5.3-flash` since 2026-10-10) and SOP videos (`qa/demos/README.md`). Per milestone: isolated code-blind execution
with schema-2 receipts and a separate evidence check ([workflow](../../../qa/code-blind-functional/README.md)).
Windows records need a real Windows machine for this step.

## Live-repository applicability

| Repository | Applicable | Use |
| --- | --- | --- |
| TensorCash | yes | Permissive compatibility and Moderate protected-action workflow |
| Isometric Game | yes | Aggressive, inheritance, downgrade and recovery workflow |

## Human acceptance

2026-10-22 owner update: [D182–D185](../../../qa/security-levels/milestones/localmodel-run3-20261022/owner-decisions-20261022.md) accept MOD-11's observed behavior with its setup deviations disclosed, accept AGG-20P-A's command-dependent wording differences, and record MOD-21's evidenced pass. The accepted Aggressive disposition is 62/0/2, including D183's exception; Moderate is 32 passed and 2 accepted of 34 when acceptance is counted separately, or 33 passed and 1 accepted under D182's owner disposition; MOD-11's deviations and D43 limit remain explicit. Original run outcomes and prior sign-off records below remain unchanged; no new release sign-off is asserted.

| Milestone | Tester | Result |
| --- | --- | --- |
| Aggressive ships | Travis Good | **Signed off 2026-10-16** (D10, comment on #867) on the third targeted re-run ([record](../../../qa/security-levels/milestones/aggressive-20261015-targeted3/README.md), #868: 58 passed, 0 failed, 2 blocked, 4 accepted of 64), with the known limits in the milestone row above; since then (D110, D113, [#1191](https://github.com/CorbanuCore/CorbanuTerminal/pull/1191)) 61 passed, 0 failed, 3 blocked of 64, sign-off unchanged |
| Moderate ships | Travis Good | **Signed off 2026-10-20** (D61, comment on #447) on the second targeted re-run ([record](../../../qa/security-levels/milestones/moderate-targeted2-20261020/README.md), #1037, resting on #971 and #844: 30 passed, 0 failed, 0 blocked, 4 accepted of 34), with the known limits in the milestone row above; follow-ups #1078 and [#1191](https://github.com/CorbanuCore/CorbanuTerminal/pull/1191): 31 passed, 0 failed, 3 accepted of 34 (D72, D113), sign-off unchanged |
| Flag removal | named by release owner | pending |

## Documentation

`docs/features/security.md`, `docs/slash_commands.md` and the vault/auth pages describe only behaviour verified at a
milestone, citing **P0 `/security` levels**.

## Dependencies, decisions, and blockers

| Item | State |
| --- | --- |
| Windows machine for PF-27-S08/S09 gates | Used for the PF-27-S06/S07 gates (2026-10-08); requirements in the [PF-27-S06 evidence](../../../qa/security-levels/sprints/PF-27-S06/README.md#windows-machine-needed-for-the-remaining-gate) |
| Target release and deadline | Not set; Travis to decide |
| PF-28-S02 latency targets | Need product authority |
| External inputs | Login origin and test account (PF-37); hosted vendor and data terms (PF-36); corpus, licences and hardware pins (PF-35) |
| Global reservation cap | 3 security lanes + 1 accounting + 1 Task Node = 5 (`docs/sprints/index.md`) |

## Release linkage

No release record yet. Each milestone links its `qa/release/<version>/` record; PF-26-S03 holds the final release
evidence. Remaining blockers: the carried-forward milestones and the sprints above.

## Completion

- [x] Activated in slot 1 with P0's open items carried forward (2026-10-08).
- [ ] Milestones Aggressive ships, Moderate ships and flag removal pass, with human sign-off (Aggressive ships signed
      off 2026-10-16; Moderate ships signed off 2026-10-20; flag removal pending).
- [ ] Every sprint above completed and archived; carried-forward items closed or re-placed.
- [ ] PF-26 final qualification passes; release and benchmark records linked.
