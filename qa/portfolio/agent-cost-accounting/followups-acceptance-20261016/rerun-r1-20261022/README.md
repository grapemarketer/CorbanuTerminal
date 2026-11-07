# PF-60-S10 re-run R1: ACC-06, ACC-11, ACC-14, ACC-15, ACC-18 (2026-10-22 to 2026-10-27)

A targeted code-blind re-run of the one failed and four blocked S10 cases ([../README.md](../README.md)). Decided QA
work; no new owner decision.

**Result: 3 passed (ACC-06, ACC-15, ACC-18), 2 blocked (ACC-11, ACC-14), none failed.**

- The executor reported all five passed.
- The integrator dispositions ACC-11 and ACC-14 as blocked. Each has one expected result that the executor judged
  without a captured screen showing it, and the frozen rules (G7, G11, G16) count such a result as NOT OBSERVED, never
  PASS. This is the same rule the first run applied to ACC-15.
- In both cases the first run's own gap, re-reading earlier Request IDs after the reopen, is now observed and passes.
- ACC-35 (needs a live Moonshot cache-read grant, D3) and the advisory cases ACC-04 and ACC-22 were not re-run.

| Case | First run (2026-10-16) | R1 raw | R1 disposition | Run |
| --- | --- | --- | --- | --- |
| ACC-06 | failed (#898) | passed | **passed** | [A-R1](runs/acc-A-R1-agmx1-20261027T175348Z/evidence/result.md) |
| ACC-11 | blocked | passed | **blocked** (result 3 not observed) | [C-R1](runs/acc-C-R1-agmx1-20261027T190341Z/evidence/result.md) |
| ACC-14 | blocked | passed | **blocked** (result 3: two exact costs not read) | C-R1 |
| ACC-15 | blocked | passed | **passed** | C-R1 |
| ACC-18 | blocked | passed | **passed** | C-R1 |

Per-case summaries are in [../dispositions.md](../dispositions.md) and [../results.json](../results.json) (each re-run
case keeps its first-run entry under `prior_run`).

## Candidate

- A private build of origin/main `63c9daca2629fe216d3887c53399a6fb7a75a334`, the newest main commit with a green
  `postmerge-ci` (run 39751860243) when the build was made on 2026-10-22.
- Signed with Developer ID K5N2T25C23. `corbanu` 0.1.48, SHA-256 `e942b095de975f95ca3fa1f09b9576f6bb408d91499e05235e35ab37b9d6aa42`
  ([candidate/](candidate/)).
- Used only inside Tart guests. It was never installed as the coordinator's binary.

## Design

- A fresh designer wrote amendment R1 from an intent packet ([design-r1/run-observations-r1.md](design-r1/run-observations-r1.md)).
  It replaces only these five cases and adds the steps needed to observe the results that were left unjudged.
- A separate fresh reviewer returned **`VERDICT: CHANGES`** together with a complete revised text, its section (b)
  ([design-r1/review-output.md](design-r1/review-output.md)).
- The integrator froze that revised text word for word on 2026-10-22T12:05:59Z
  ([design-r1/FROZEN.sha256](design-r1/FROZEN.sha256), `c03044a2…`), before the first VM started (12:07:16Z).
  - There was no second review of the revised text.
  - The reviewer's own section (c) holds five open questions "for the integrator; none blocks freezing"
    ([design-r1/open-questions-r1.md](design-r1/open-questions-r1.md)). They were not answered before the freeze.
  - In this run they resolved as follows:
    1. `agms-tui help` lists no file-writing control, so A-S1 used the fallback product turn.
    2. ACC-15's rule would count a notice in the side view and another in the saved view as two (a FAIL). That did not
       arise: the saved view's notice count stayed at 1, before (`-pre`) and after (`-back`) `/side`.
    3. No request was reached only through Descendant attempts.
    4. Calibration: the executors read exact costs from technical pages.
    5. Group A took 31 minutes.
- The executors got the staged packet in [executor-packet-r1/](executor-packet-r1/). Its `amendment-r1.md` is
  byte-identical to the frozen file.

## Harness

- acc-tart1's accounting harness ([harness/](harness/)) ran on one fresh Tart clone of `corbanu-qa-base` per group,
  one VM at a time, under the shared Tart lock.
- pf84-rerun's host-relay pattern was used: no real key entered a guest. Two host relays
  ([harness/host_key_relay.py](harness/host_key_relay.py)) added the Z.AI key (vault `zai_key_3`) and the OpenAI key.
  The guest reached them through one SSH reverse tunnel bound to its loopback, and only the coordinator account's
  mediator could use it.
- Every clone passed the 110-probe isolation gate before its group ran ([common/](common/)`<clone>/gate`).
- Testers: GLM 5.3 Flash (`corbanu exec`, code-blind, fresh context), executor account `agmx1`. No model declined a
  step, so the local-model fallback was not needed.

## Attempts

Every attempt is kept as evidence ([logs/resume-driver.log.txt](logs/resume-driver.log.txt), [logs/seq.log.txt](logs/seq.log.txt); `*.log` files are stored as `*.log.txt` because the repository ignores `*.log`).

1. **2026-10-22, A-R1 attempt 1** ([runs/acc-A-R1-agmx1-20261022T121009Z](runs/acc-A-R1-agmx1-20261022T121009Z)):
   **does not count.** The tester died on a Z.AI 429 with code 1113 (insufficient balance), after `/review` and before
   judging. The work paused until the balance was restored ([logs/zai-balance-poll.txt](logs/zai-balance-poll.txt)).
2. **2026-10-27, clones A-R1-…1744Z and C-R1-…1748Z: gate failed, no group ran and nothing was spent.**
   - Cause: the synthetic mediation CA and relay leaf inherited from acc-tart1 expired at 2026-10-27T01:48:51Z. The guest
     mediator reported `CERTIFICATE_EXPIRED`, and the lanes' mediator probes saw EPERM instead of 403.
   - Fix: both certificates were re-issued from the same keys, with new validity and the same extensions
     ([harness/ca-reissue-20261027.md](harness/ca-reissue-20261027.md)).
   - No design, packet, package or expected-result file changed.
3. **2026-10-27, A-R1** ([runs/acc-A-R1-agmx1-20261027T175348Z](runs/acc-A-R1-agmx1-20261027T175348Z)): gate 110/110;
   ACC-06 passed.
4. **2026-10-27, C-R1 attempt 1** ([runs/acc-C-R1-agmx1-20261027T182928Z](runs/acc-C-R1-agmx1-20261027T182928Z)):
   **does not count.**
   - The tester's Z.AI stream stalled twice, each time hitting the product's 180 s actionable-silence timeout
     (18:54 to 19:00Z, with a request near 625 KB). The executor turn failed in the middle of ACC-14.
   - The executor had already judged ACC-11, with no verdict line. ACC-18 needs the whole group's history, so C-R1 was re-run in full
     on a fresh clone.
5. **2026-10-27, C-R1 attempt 2** ([runs/acc-C-R1-agmx1-20261027T190341Z](runs/acc-C-R1-agmx1-20261027T190341Z)):
   gate 110/110. The executor reported ACC-11, ACC-14, ACC-15 and ACC-18 passed. The dispositions are ACC-15 and
   ACC-18 passed, and ACC-11 and ACC-14 blocked (see above).

## Integrator checks

The integrator checked the deciding observations against the captured screens (`evidence/screens.jsonl.gz`), not just
the executor's report.

- **ACC-06:**
  - `r1a-acc06-after-req5-tech` through `-req9-tech` each show `Turn: code-review:01a17e90-…`.
  - Requests 1–4 show the same Request IDs on the before and after pages.
- **ACC-11 and ACC-14.** The first run's gap is now observed:
  - After the restart, the earlier IDs `950fc3fe…` and `06d205ef…` are on `r1c-acc11-after-req1` and `-req2`.
  - After `/resume`, all four ALPHA IDs are on request pages captured between 19:19 and 19:21Z.
- **ACC-11 and ACC-14: two results not observed.** The independent review found these, and the integrator confirmed
  them on the screens.
  - ACC-11, expected result 3 ("exactly one request link contains `Reopen warm-up`"):
    - Every link list captured in ACC-11 shows either only the provider line (`r1c-acc11-after-links`, 19:13:10Z) or
      bare `→ Request 1/2/3` lines (`-after-links-2`, `-after-provider-list`).
    - The labelled list first appears in ACC-14 (`r1c-acc14-after-provider-list3`, 19:20:33Z).
    - The executor's report claims `r1c-acc11-after-links` shows the links. It does not.
  - ACC-14, expected result 3, first part (every earlier ID found again "with the same cost", G11):
    - `950fc3fe…` and `06d205ef…` were re-read in ACC-14 only on request pages showing rounded costs.
    - ACC-14's only technical money pages for ALPHA are those of its two warm-ups (19:20:19Z and 19:21:39Z). CHARLIE's
      money pages (19:18:29Z, 19:18:54Z) are CHARLIE's own requests.
- **ACC-15 (the `side:` label):**
  - `r1c-acc15-side-req6-tech` shows `Turn: side:01a17ed0-6a0d-70e3-8ad2-b455f7ffdad9`.
  - The warm-up page shows `Turn: side:prewarm:reopen:…`.
  - Notice placement:
    - The side view (`r1c-acc15-side-open`, and again `-side-reply`) shows one notice and none of ALPHA's transcript
      lines, so that notice is new.
    - The ALPHA view after Ctrl+C (`r1c-acc15-back`) shows one notice under `• ALPHA`, as `r1c-acc15-pre` did.
    - The product showed no return confirmation: the executor's wait for it timed out at 19:23:25Z. So the rule's third
      test could not apply.
    - The `-back` view is re-rendered without pre's `Token usage` and `To continue` lines, so calling its notice ACC-14's
      is an inference. The notice count in the saved view stayed at 1.
    - Result: exactly one new notice, as the executor read it.
- **ACC-18:**
  - All eight IDs from ACC-11, ACC-14 and ACC-15 are found again on `r1c-acc18-alpha-*` and `r1c-acc18-charlie-*`
    pages. Request 5's page was captured under the label `r1c-acc18-alpha-links-2`, and its money page is
    `r1c-acc18-alpha-req5-tech-money`.
  - Neither `r1c-acc18-open-1` nor `r1c-acc18-open-2` shows a notice.
  - Several captures carry the label of a different page; for example, some `-tech-money` labels show the conversation
    total. The exact cost of each of the eight IDs is still on a correctly identified capture.
- **Keys:**
  - Every clone's teardown reported the real key files absent (never placed), `auth.json` a synthetic placeholder, and
    the mediator leaf key removed.
  - [keyscan-r1.txt](keyscan-r1.txt) holds the key scan of the final tree, made by
    [harness/keyscan-r1.sh](harness/keyscan-r1.sh). It scans for key pattern classes and for the exact values under
    vault labels `zai_key_3`, `openai-api-key`, `provider/zai_api_key` and `kimi_api3`, and prints names and counts
    only. `harness/keyscan.sh` is the first run's script, with three labels.
- **Relays** ([logs/](logs/), 2026-10-27):
  - Z.AI: 402 chat completions, all 200, plus 3 model listings.
  - OpenAI: 8 WebSocket upgrades (101) and 3 model listings.
  - No 429s and no other non-success status.
- **Spend** (estimated at the published rates the product itself shows):
  - Testers on Z.AI GLM 5.3 Flash: about $0.28 for A-R1 and $0.52 for C-R1 attempt 2, from the executors' reported
    usage. C-R1 attempt 1 reported no usage before it failed; it was comparable.
  - Products under test: Z.AI $0.0048 (ACC-06); OpenAI about $0.014 (C-R1 attempt 2) plus a few thousandths in
    attempt 1.
- **Clean-up:** the Tart lock was released, every clone was deleted and the relays were stopped. Evidence:
  [logs/cleanup-state.txt](logs/cleanup-state.txt).

## Review

- R1 has its own budget, set by the brief: a fresh designer, a separate design review, and up to 3 independent
  read-only Opus 5.5 High review rounds for the record PR.
- The rounds are listed in [../review-ledger.md](../review-ledger.md#re-run-r1-acct-rerun--acct-rerun2-2026-10-22-to-2026-10-27).
- Round 1 returned CHANGES. Its findings are applied above: ACC-11 and ACC-14 are now blocked, and the disclosures and
  record fixes are in.

## Observations (not case failures)

- `/review` of one changed file made five requests on Z.AI, all labelled `code-review:` with the review's own thread
  (ACC-06).
- In ACC-18, `resume --last` opened CHARLIE, although ALPHA was used last: `/resume` to ALPHA, then a `/side` turn in
  ALPHA. ALPHA's last turn of its own came before CHARLIE's.
  - The first run saw the same in ACC-36: `resume --last` opened the edit branch.
  - It looks like "last" means the conversation with the latest turn of its own.
- On ALPHA's Request 2 technical page the executor did not find a `Turn:` line in its capture (C-R1, ACC-11). No
  expected result depends on it.
- The tester model's stream can stall at long contexts (C-R1 attempt 1). The executor's long-failure cap of 1 then ends
  the run.
