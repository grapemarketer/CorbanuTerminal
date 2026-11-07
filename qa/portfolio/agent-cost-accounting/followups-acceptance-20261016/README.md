# PF-60-S10: isolated code-blind acceptance of the accounting follow-ups (2026-10-16)

PF-60-S10 (#404) runs the acceptance of PF-60-S06 to S09 inside the
[isolated execution gate](../../../code-blind-functional/isolated-execution.md), discharging PF-60-S05's waiver (b).
Owner decisions (Travis, 2026-10-15): S06–S09 accepted; S10 may run code-blind on all four before they are archived,
on VM "macOS 3"; #824 (realtime) is included only if merged before the freeze — it was not, so realtime is the tracked
known gap. **Signed off by Travis on 2026-10-27 (D208)**, with ACC-22 and ACC-35 as named limits.

## Final disposition (2026-10-27, D207–D209)

- **35 passed, 0 failed, 2 blocked** of 37. Blocked: ACC-22 (advisory; starting condition not met) and ACC-35
  (optional; needs real Moonshot spend, D3 not granted). These are S10's named limits (D208).
- Three of the 35 are **passed by owner decision**, not on the record: ACC-04 (D113) and ACC-11 and ACC-14 (D207).
  Owner acceptance is not a clean pass. On the record those three stay **blocked** ([dispositions.md](dispositions.md),
  [results.json](results.json) `cases`); the decisions are listed separately (dispositions.md's last section,
  results.json `owner_decisions`).
- On the record alone: 32 passed, 0 failed, 5 blocked.

## Status before D207–D209 (2026-10-27)

- **33 passed, 0 failed, 4 blocked** of 37 cases. Two dated results after the first run, below, produced this count:
  - **Re-run R1 (2026-10-27):** ACC-06, ACC-15 and ACC-18 passed. ACC-11 and ACC-14 stay blocked, each on one result
    that the tester judged but did not capture. Record: [rerun-r1-20261022/](rerun-r1-20261022/README.md).
  - **D113 (2026-10-21):** ACC-04 is recorded as passed.
- The 4 blocked cases:
  - ACC-11 and ACC-14 (blockers), see the next point;
  - ACC-22 (advisory; its starting condition was not met);
  - ACC-35 (optional; D3 was not granted).
- No case failed. ACC-11 and ACC-14 have every other result observed and passing:
  - ACC-11's missing result is "exactly one link contains Reopen warm-up". It shows only in a later capture.
  - ACC-14's missing result is the exact-cost comparison of its two oldest requests. Those were read only rounded in
    ACC-14, and exactly later, in ACC-18.
- [dispositions.md](dispositions.md) and [results.json](results.json) carry the R1 dispositions for the five re-run
  cases. Each keeps its first-run entry under `prior_run`. Their totals read 32 passed and 5 blocked, because ACC-04
  stays blocked there and D113 is recorded only in the addendum below (and, since D207, in their owner-decision parts).

## Result (first run, 2026-10-16)

37 cases: **29 passed, 1 failed, 7 blocked**. The first-run dispositions of the five re-run cases are under
`prior_run` in [results.json](results.json).

- **Failed (1):** ACC-06 (#898): after `/review`, `/cost` shows no `code-review:` request and no new request anywhere. The
  run's ledger (integrator's read, [runs/A/ledger-attempts.txt](runs/A/ledger-attempts.txt)) holds the review's six
  requests under the review's own thread with plain labels: recorded, but shown nowhere.
- **Blocked (7):**
  - ACC-35: the optional Moonshot cache-read turn; D3 was not granted, so not checked live (covered by test).
  - ACC-04 (advisory): the product's model declined to start a sub-agent inside `/side`, which the design allows.
  - ACC-15: everything passed except the `side:` label on the side turn, which the executor did not judge. The raw
    terminal output shows the line `Turn: side:…` rendered while it scrolled past it (it suggests the case would pass), but it is in no
    captured screen, so the case stays blocked on that one result.
  - ACC-11, ACC-14, ACC-18 (S08 reopen warm-up): every observed result passed (notices, warm-up counts, earlier costs
    unchanged, totals reconciling), but no earlier Request ID was re-read after the reopen, so "Request IDs unchanged
    / appear once" is unjudged; ACC-18 also checked per-conversation sums only for ALPHA. Blocked by the same rule.
  - ACC-22 (advisory): the product omits the priority tier for `gpt-6-luna` ("not advertised"), so the no-price branch
    was never reached.
- **Passed by sprint:**
  - S06: `/side` work and the automatic reviewer inside `/side` are recorded once under the saved conversation as `side:`
    and `side:review:` (ACC-02, 03); a sub-agent is counted once in the parent's total (ACC-05); `exec --ephemeral` stays out
    (ACC-07); basic `/cost` navigation and an empty day (ACC-01).
  - S07: real Moonshot K3 and K2.7 Code priced completely from Moonshot's rates, whatever the provider is named
    (ACC-08, 09); `gpt-6.1-sol` and GPT-5.6 Sol rates (ACC-19, 20); OpenRouter, Vercel and Corbanu API priced from
    their reported charge (ACC-23, 25, 26, 28); reasoning above output kept as not reported (ACC-24); without a charge,
    a labelled estimate, a lower bound or no price, never $0 (ACC-37).
  - S08: a new conversation shows no reopen label or notice but still warms up (ACC-10, 13); editing an earlier
    message sends a labelled, announced reopen warm-up (ACC-16); `skip_reopen_warmup` works by flag and by
    `config.toml` (ACC-12, 36); reviewers and sub-agents are not reopens (ACC-17); restart and `/resume` (ACC-11, 14)
    passed everything observed, see Blocked; no warm-up on a non-WebSocket reopen (ACC-29); unpriced and basis-only wording (ACC-27).
  - S09: ChatGPT sign-in (stand-in) covered by the subscription for catalogued, uncatalogued, priority and image
    (ACC-30–33); a real API-key image priced at `gpt-image-2` rates (ACC-21).
- No credential was printed (ACC-34, advisory).
- Raw executor verdicts differ from seven dispositions (ACC-04, 11, 14, 15, 18, 22, 34); reasons in [dispositions.md](dispositions.md).
- `qa/code-blind-functional/check.py` stops at the first unresolved case ([checker-output.txt](checker-output.txt)), as
  expected while ACC-04 is not passed.

## Observations (not case failures)

- Editing an earlier message continues in a **new conversation**; its warm-up is counted there (ACC-16).
- On the OpenAI route the automatic reviewer is "Codex Auto Review" and has **no price** (ACC-17/18 screens).
- A conversation that holds only a warm-up request (a new or forked conversation left without a turn) is billed and
  counted under "other conversations" but cannot be opened from `/resume` (ACC-18's two residuals).
- Executor deviations in C2, disclosed per case: ACC-14 skipped its CHARLIE turn (leaving one of ACC-18's residuals);
  ACC-16 made an accidental extra fork; ACC-36's restart opened the edit branch, not ALPHA.
- A `/side` warm-up is labelled `side:prewarm:reopen:` in the ledger ([runs/C2/ledger-attempts.txt](runs/C2/ledger-attempts.txt)).

## Design and freeze

- A fresh read-only Opus 5.5 High designer wrote 34 cases from an intent packet built from the S06–S09 records and 28
  screens of the sprints' captures ([design/](design/), [designer-session.md](design/designer-session.md)).
- A separate fresh reviewer returned CHANGES with 22 amendments, including new cases ACC-35 (optional, D3), ACC-36 and
  ACC-37; all applied as written ([amendments-final.md](design/amendments-final.md), [review-session.md](design/review-session.md)).
  The integrator's answers to the designer's questions are facts about the harness only ([answers-v1.md](design/answers-v1.md)).
- Frozen and pushed at `220c99ed59` (2026-10-16T02:54Z), before the VM start ([FROZEN.sha256](design/FROZEN.sha256)).
  Executor packet v1 pushed at `85da58b839` (03:09:56Z), before the lock was taken.

## Candidate

`origin/main` `395ac0adcd`, the coordinator's signed developer build (0.1.48, `bin/corbanu` SHA-256 `5b805b9c…0531a1`),
installed at this run's request because the earlier build lacked #859 ([candidate/](candidate/candidate-manifest.md)).

## Execution

- **Where:** UTM guest "macOS 3" only; "macOS 2" stayed stopped; no VM created or deleted. Lock held 03:10:03Z–08:06:54Z.
- **Isolation:** the Aggressive/Moderate harness with an accounting relay and stand-ins
  ([common/effective-policy.txt](common/effective-policy.txt), [harness/](harness/)). Real routes through the relay with
  the key injected outside the executor: Z.AI, OpenAI (WebSocket relayed), Moonshot (12-request cap; 2 used). Stand-ins:
  ChatGPT sign-in (S09's driver), OpenRouter, Vercel, Corbanu API, a person-configured provider.
- **Gate:** two failed attempts on harness faults, then 105/105 probes, PTY control and smoke turn ([common/gate/](common/gate/)).
  Every group re-ran the probes; every executor ran them itself; every isolation receipt is true. Guard: 0
  SecurityAgent prompts in 16,192 checks.
- **Groups** (one fresh GLM 5.3 Flash executor each, one at a time): B, A, C, D on packet v1; E, F on packet v2.
- **Replays** (fresh executors, fresh profiles; first attempts kept):
  - C2: group C's executor could not reach the `/cost` links (the page is taller than the screen). Packet v2 added a
    navigation note only (`868c3680ee`, installed before E and F).
  - E2: the harness's corbanu route failed to start under macOS bash 3.2 (harness fix 1 and packet v3, `92882a1382`).
- **Wrap-up:** guest data erased and verified on a separate boot; VM stopped; lock deleted ([common/vm-state-end.txt](common/vm-state-end.txt)).
- **Key scan:** the three real keys (Z.AI, OpenAI, Moonshot) found in 0 files of this record ([keyscan-record.txt](keyscan-record.txt))
  and of the run folder ([keyscan-run-folder.txt](keyscan-run-folder.txt); its one pattern hit is base64 text, explained there).
- **Spend (approximate):** Moonshot $0.047 (two real turns); OpenAI about $0.26 by `/cost`'s estimates (excluding the
  unpriced auto-reviewer); Z.AI about $3.2 at the catalogue's GLM 5.3 Flash rates, almost all of it the executors' own
  long contexts (95.4 M input tokens, 94.1 M cached, over 1,132 requests; [common/mediator-final.jsonl.gz](common/mediator-final.jsonl.gz)).

## Record format

- `runs/*/executor-events.jsonl` and `runs/*/collected/evidence/screens.jsonl` are gzipped (`.gz`, blob-size policy);
  `pty.raw` files are gzipped; `harness/agms-probe` is stored byte-identical as `harness/agms-probe.txt` (codespell).
- Not committed (git-ignored): each run's `collected/fixture/workspace/.env` (the fixture's synthetic canary) and
  `collected/product-logs/codex-tui.log`. The key scan covers the committed tree only.
- Product homes are not copied: each run keeps its `product-home-config.toml`, a size/SHA-256 manifest of the rest and
  the integrator's ledger extract (`ledger-attempts.txt`). Fixture `.git` folders are kept as `dot-git` without objects.

## Named limits

- Realtime (#824, fix #882 unmerged at the freeze), TensorCash, DeepSeek holiday calendar, BigModel, older records,
  Windows and Linux; `fork:` (no client makes one); Moonshot unlisted models and K2.7 HighSpeed/K2.6 rates; the 5-minute
  cache lifetime; same-conversation `/resume`; a real Moonshot cache hit (D3 not granted).
- Request-ID stability across reopens was not observed (ACC-11, 14, 18): the executors re-read costs, not IDs. Observed and passed in re-run R1.
- `harness/agms-product` was replaced (harness fix 1, corbanu route only) while group F's executor was running, before
  its first product start; packet v2 was installed while group D ran; the fix check used lane agmx0 concurrently with F.
- Gate attempts 1 and 2 each sent one real Z.AI smoke turn although they failed (see the addendum).
- The guest's pre-existing Z.AI transport key file stays on the guest, as in earlier campaigns.
- The same model family (GLM 5.3 Flash) drives the executor and most product turns.
- The relay logs no tool names, so model-visible tool sets are not inventoried ([common/tool-inventory.json](common/tool-inventory.json)).
- The ledger reads (`runs/*/ledger-attempts.txt`) are the integrator's, after the run; dispositions rest on the
  executors' screens.
- The guest's own :443 allowlist was widened by Moonshot's two addresses (for every guest account except the blocked
  executors) and restored.
- `tools/make_packet.py` gained a navigation-note hook after the freeze (to build packets v2/v3); v1 was built with the
  frozen script. Details: [common/effective-policy-addendum.txt](common/effective-policy-addendum.txt).

## Review budget

5 of 5 used ([review-ledger.md](review-ledger.md)): design, design review, evidence checks r1-r3. The last verdict (r3) is **fail** on written statements only; its corrections are applied but not independently re-checked; no extension requested.

## After this record: ACC-04 on the local model (2026-10-21)

Dated addendum; the record above is unchanged.

- ACC-04 was re-run with the owner's local model as the product's model (D74, D75) in the
  [second local-model re-run](../../../security-levels/milestones/localmodel-rerun2-20261021/README.md)
  ([#1191](https://github.com/CorbanuCore/CorbanuTerminal/pull/1191), candidate `bc1deda532`, the record's neutral
  profile). Inside `/side` the model started a sub-agent. Its request is labelled `Turn: side:agent:…`, listed once
  under the conversation, and no other conversation shows a request.
- **D113** (owner, 2026-10-21): ACC-04 is recorded as **passed**. The 37 cases are now **30 passed, 1 failed (ACC-06),
  6 blocked**. It ran on the local model only, with none of this record's routes or prices (named limit of that
  record).

## After this record: re-run R1 (2026-10-27)

Dated addendum. The first-run sections above are unchanged in substance. This PR adds a current-status section,
renames the result heading, and adds one note to a named limit.

- ACC-06 (#898 is closed as fixed), ACC-11, ACC-14, ACC-15 and ACC-18 were re-run under amendment R1.
  - A fresh designer wrote it. A separate reviewer returned CHANGES with a full revised text, and that revised text was
    frozen word for word, before the first VM started.
  - The candidate was a private signed build of `63c9daca26`.
  - Testers were GLM 5.3 Flash, running code-blind on fresh Tart clones. Host relays held the keys, so no real key
    entered a guest.
- **ACC-06 passed:** each of the review's five requests shows `Turn: code-review:…`.
- **ACC-15 passed:** the side turn shows `Turn: side:…`, and exactly one new notice appeared.
- **ACC-18 passed:** after all the group's reopens, every earlier Request ID comes back with its exact cost, and the per-conversation
  sums hold for both conversations.
- **ACC-11 and ACC-14 stay blocked**, each on one result the tester judged without a capture (dispositions.md).
  - Their first-run gap, re-reading earlier Request IDs after the reopen, is now observed and passes.
  - The tester reported both as passed.
- **Attempts that did not count:**
  - A-R1 on 2026-10-22 (Z.AI balance);
  - C-R1 attempt 1 (the tester's stream stalled mid-ACC-14).
- The expired synthetic mediation CA was re-issued.
- Details, evidence and integrator checks: [rerun-r1-20261022/README.md](rerun-r1-20261022/README.md).

## After this record: owner decisions D207–D209 (2026-10-27)

Dated addendum; the record above is unchanged except the opening status and the final-disposition section.

- **D207:** ACC-11 and ACC-14 are recorded as passed by owner decision, as D113 did for ACC-04. Their recorded
  disposition stays **blocked**, each on one result not captured in the case's own screens.
- **D208:** Travis signed off S10 with ACC-22 (advisory) and ACC-35 (needs real Moonshot spend, not granted) as named
  limits. Final disposition: 35 passed, 0 failed, 2 blocked, with ACC-04, ACC-11 and ACC-14 passed by owner decision.
- **D209:** this README, [dispositions.md](dispositions.md), [results.json](results.json), the
  [S10 sprint page](../../../../docs/sprints/archive/portfolio-agent-cost-accounting-followups/pf-60-s10-isolated-code-blind-acceptance.md)
  and the [sprint index](../../../../docs/sprints/index.md) carry these counts.
