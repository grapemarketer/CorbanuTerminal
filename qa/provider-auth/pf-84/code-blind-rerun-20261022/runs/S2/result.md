# Group S2 execution record

Run started UTC 2026-10-22T08:56:22Z (executor preflight context). Wall-clock deadline about 80 minutes.

## Preflight

- Executor command `agms-probe all` exited rc=0; `preflight-executor.json` reports `"ok": true`. UTC 2026-10-22T08:56:22Z context.
- Coordinator files `preflight-summary.json`, `preflight-launcher.json`, `preflight-tmux-child.json` are present; summary reports `ok: true`, no failed probes, 97 probes, launcher/tmux-child/PTY positive.
- Package SHA-256 matched `ba9feaa443ec18f29abc73a27137cc8f4696899ea4a1c3dada84c8d2df280006`.
- Design cases SHA-256 matched `cfad42f543b6782a70b935b0596ac50737468733f3480629f49089e34eb6e5b4`.
- Amendment v2 SHA-256 matched `fe1324914155fc4d37a83a4ac3bd7bdd0ac67f636cdf31e1da0cfb7c6a0f7e77`.
- Amendment v3 SHA-256 matched `ff365c23e584d67d678af2aed535904416af408805de5ab4d903a116d148dee4`.
- Prepared home listed: `zai3`. Candidate reports `corbanu 0.1.48`.
- Isolation preflight and candidate identity checks passed.

Window mapping and case logs follow. Relay/Files lines are for the integrator unless otherwise stated.

## PR-01

Notes:
- Step 1 start was executed at UTC 2026-10-22T08:58:31Z. Before sending, the executor initially invoked generic `agms-tui text/key/wait` without `--window w1`; those harness calls failed with `tmux error: can't find session: product`. No text or Enter reached the product at that time. The literal prompt and Enter were then sent correctly at 2026-10-22T08:58:58Z using `--window w1`; 2026-10-22T08:58:59Z start.
- Window mapping: PR-01 step 1 `A = w1`.

Step 1:
- Requested START(zai3, `--account work`), ASK(pzero), STATUS(p0), QUIT; log SID.
- Actual: w1 started at 2026-10-22T08:58:31Z and showed the composer. Prompt `reply with the single word pzero` was typed separately at 2026-10-22T08:58:58Z and Enter pressed at 2026-10-22T08:58:59Z. `• pzero` appeared by 2026-10-22T08:59:04Z. `/status` was typed and Enter pressed; p0 captured at 2026-10-22T08:59:14Z. `/quit` sent at 2026-10-22T08:59:21Z; product exited code 0.
- Positive checkpoints: p0 screen label; `Model provider:` row ends `· account work`; `Session: 01a162d5-5e06-7672-91eb-0a666615a08b` (`<id>`); `Account usage: work ...`; `• pzero`. Terminal settings on quit: `icanon isig echo icrnl opost`.
- Relay for the integrator: 2026-10-22T08:58:58Z–08:59:04Z `pzero`; expected `zwork`.
- Files for the integrator: `<id>` first turn expected `zai`/`work`.

Step 2:
- Requested START(zai3, `resume --account personal <id>`), STATUS(p1-pre), ASK(pone), STATUS(p1), QUIT.
- Actual: w2 started at 2026-10-22T08:59:44Z; p1-pre captured at 2026-10-22T08:59:59Z; prompt/Enter at 2026-10-22T08:59:59Z; `• pone` by 2026-10-22T09:00:06Z; p1 at 2026-10-22T09:00:21Z; quit code 0.
- Positive checkpoints: p1-pre and p1 both show `· account personal`; same Session `<id>`; p1 has table rows `personal (active)` (43 input, 8,576 cached, 3 output) and `work` (8,595 input, 16 output, 11 reasoning); `• pone`.
- Relay for the integrator: 2026-10-22T08:59:59Z–09:00:06Z `pone`; expected `zpersonal`.

Step 3 `[observe]`:
- Requested START(zai3, `resume <id>`), STATUS(pq), no prompt, QUIT; actual at 2026-10-22T09:00:29Z start, pq at 2026-10-22T09:00:35Z, quit code 0 at 2026-10-22T09:00:36Z. pq shows `· account personal`, Session `<id>`, account-usage table with personal active and work prior counts. No prompt sent, no visible model request.

Step 4:
- Requested START(zai3, `resume --account work <id>`), STATUS(pb), ASK(pback), QUIT.
- Actual: w4 start 2026-10-22T09:00:45Z; pb at 2026-10-22T09:00:52Z; prompt/Enter 09:00:52Z; `• pback` by 09:00:57Z; quit code 0 at 09:00:58Z.
- Positive checkpoints: pb and post-reply screen show `· account work`; `• pback`; account usage table work active and personal prior.
- Relay for the integrator: 2026-10-22T09:00:52Z–09:00:57Z `pback`; expected `zwork`.

Step 5:
- Requested START(zai3, `resume <id>`), STATUS(p2-pre), ASK(ptwo), STATUS(p2), QUIT.
- Actual: w5 start 2026-10-22T09:01:07Z; p2-pre at 09:01:13Z; prompt/Enter 09:01:13Z; `• ptwo` by 09:01:18Z; p2 at 09:01:29Z; quit code 0.
- Positive checkpoints: p2-pre and p2 show `· account work`; `• ptwo`; account-usage table work active and personal prior.
- Relay for the integrator: 2026-10-22T09:01:13Z–09:01:18Z `ptwo`; expected `zwork`.

Step 6:
- Requested START(zai3), STATUS(p3-pre), ASK(pthree), STATUS(p3), QUIT.
- Actual: w6 start 2026-10-22T09:01:38Z; p3-pre at 09:01:43Z; prompt/Enter 09:01:43Z; `• pthree` by 09:01:50Z; p3 at 09:02:00Z; quit code 0 at 09:02:01Z.
- Positive checkpoints: p3-pre and p3 show `· account personal`; new session `01a162d8-3750-7c70-b936-b491482fcd17`; `• pthree`; p3 usage `personal · 8,595 input · 17 output (13 reasoning)`.

Step 7:
- Requested remove `[provider_accounts]`, cat before/after. Actual before capture included `[provider_accounts] zai = "personal"` and also `service_tier = "default"` (the latter had appeared by this later cat; no prior product edit was requested). Sed completed at 2026-10-22T09:02:08Z. After capture has no `provider_accounts`; the edit succeeded.

Step 8:
- Requested START(zai3), STATUS(p4-pre), ASK(pfour), STATUS(p4), QUIT, then `agms-tui stop`.
- Actual: w7 start 2026-10-22T09:02:18Z; p4-pre at 09:02:23Z; prompt/Enter 09:02:23Z; `• pfour` by 09:02:38Z; p4 at 09:02:49Z; quit code 0; `agms-tui stop` at 09:02:56Z.
- Positive checkpoints: p4-pre and p4 show `· account default`; new Session `01a162d8-d29b-7480-b98c-79453479b3bb`; `• pfour`; p4 usage `default · 8,595 input · 4 output`.

PR-01 judged expected results:
- p0 work: passed.
- p1 personal: passed.
- pb work: passed.
- p2 work: passed.
- p3 personal: passed.
- p4 default: passed.
- p1/p2 account-usage tables with active and other counts: passed.
- p1-pre, p2-pre: passed (same accounts as p1/p2).
- pq: observed (`· account personal`, account usage unchanged by no prompt; no request visibly made).
- Relay results: for the integrator (timestamps above).
- Files results: for the integrator.

## NA-05 (amendment v3)

Window mapping: `A = w13`.

Step 1:
- Requested START(zai3, `--account personal`), ASK(rone), STATUS(r1), log SID1.
- Actual: w13 start 2026-10-22T09:05:30Z; prompt/Enter 09:05:32Z; `• rone` by 09:05:37Z; r1 at 09:05:49Z.
- Positive checkpoints: r1 shows `· account personal`; SID1 `01a162db-c18e-7b73-895c-93fdb5128f66`; `• rone`; usage personal.
- Relay for the integrator: 2026-10-22T09:05:32Z–09:05:37Z `rone`; expected `zpersonal`.

Step 2:
- Requested `/resume` + Enter, screen, then text `gzero`, screen. Actual at 09:05:57Z Enter; picker at 09:06:00Z; text at 09:06:02Z. The picker showed `rone`, `gzero`, `pfour`, `pthree`, `pzero`; after search only `gzero`, highlighted (`❯`).

Step 3:
- Requested Enter because highlighted line contains `gzero`. Actual Enter at 2026-10-22T09:06:06Z.

Step 4:
- Requested sleep 5 then r-msg. Actual r-msg at 2026-10-22T09:06:11Z. It shows the exact one-line notice, wrapped by the terminal:
  `• This conversation ran on zai account \`work\`, which is no longer available; it resumes on \`personal\`, the account selected for this session (with \`--account\` or in \`/providers\`).`
  It also shows earlier `gzero` and `gaone` transcript entries and a bottom `To continue this session...` line naming SID1, while the active session context later is `<gid>`.
- Positive checkpoints: owner-decision notice naming work and personal; no REF-10 refusal.

Step 5:
- Requested STATUS(r2), ASK(rtwo), QUIT, then stop.
- Actual r2 at 2026-10-22T09:06:21Z shows `Session: 01a162d9-ed8f-7b92-ad15-79609db12d4a` (`<gid>`, not SID1) and `· account personal`. Account usage table: `personal (active) 0 0 0 0`; `work 81 input, 17,152 cached, 57 output, 49 reasoning`. Prompt/Enter at 09:06:21Z; `• rtwo` by 09:06:27Z; quit code 0 at 09:06:34Z; stop at 09:06:34Z.
- Relay for the integrator: 2026-10-22T09:06:21Z–09:06:27Z `rtwo`; expected `zpersonal`; no `zenv`/`zwork`.

NA-05 judged:
- Precondition r1 personal: passed.
- Conversation opens on personal: passed.
- Required notice wording: passed.
- Not refused: passed.
- r2 shows `<gid>` and personal: passed.
- `• rtwo`: passed.
- Earlier turns visible: observed (both `gzero` and `gaone` visible in r-msg).
- r2 account usage: observed (work prior counts listed; personal active zero before `rtwo`).
- NO-ONBOARD: passed.
- Relay/Files: for the integrator.

## GH-02

Window mapping: step 1 `A = w8`; later CLI/refuse windows w9–w12.

Step 1:
- Requested setup START(zai3, `--account work`), ASK(gzero), STATUS(g0), QUIT; log `<gid>`.
- Actual: w8 start 2026-10-22T09:03:30Z; prompt/Enter 2026-10-22T09:03:32Z; `• gzero` by 09:03:38Z; g0 at 09:03:51Z; quit code 0.
- Positive checkpoints: g0 shows `· account work` and `Session: 01a162d9-ed8f-7b92-ad15-79609db12d4a` (`<gid>`); `• gzero`; usage on work.
- Relay for the integrator: 2026-10-22T09:03:32Z–09:03:38Z `gzero`; expected `zwork`.

Step 2:
- Requested append `[provider_accounts] zai = "ghost"` and cat. Actual at 2026-10-22T09:03:58Z; capture shows the appended section.

Step 3:
- Requested START(zai3, `resume <gid>`), STATUS(g1), ASK(gaone), QUIT.
- Actual: w9 start 2026-10-22T09:04:08Z; g1 at 09:04:14Z; prompt/Enter 09:04:14Z; `• gaone` by 09:04:20Z; quit code 0.
- Positive checkpoints: g1 shows `· account work`, same `<gid>`, usage work, and transcript `gzero`; `• gaone`.
- Relay for the integrator: 2026-10-22T09:04:14Z–09:04:20Z `gaone`; expected `zwork`.

Step 4:
- Requested CLI remove work, log time. Actual w10 at 2026-10-22T09:04:27Z, screen shows `Removed zai account \`work\`.`, product exited code 0 at 09:04:29Z. Step passed.

Step 5:
- Requested REFUSE(zai3, `resume <gid>`), label g5, log time. Actual w11 start 2026-10-22T09:04:37Z, screen within 2 s shows the exact refusal below, product exited code 1 at 09:04:41Z:
  `ERROR: This conversation ran on zai account \`work\` and resumes on it: account \`work\` of provider \`zai\` is not configured; add it with \`corbanu account add zai work\` or pick another with \`--account\` (see \`corbanu account list\`)` (screen line wrap retained; no line-break inserted into command text by product).
- Positive checkpoints: required phrase and recovery present; no picker/session/onboarding. It does not visibly mention `ghost`.
- Relay for the integrator: 2026-10-22T09:04:37Z–09:04:41Z, expect nothing.

Step 6 `[observe]`:
- Requested REFUSE(zai3, `resume <id>`) with PR-01 `<id>`. Actual w12 start 2026-10-22T09:04:49Z, same error text as step 5, product exited code 1 at 09:04:54Z; no session. `agms-tui stop` at 09:04:54Z.
- Relay for the integrator: expect/observe nothing during 09:04:49Z–09:04:54Z.

GH-02 judged:
- g0 work: passed.
- g1 work: passed.
- Step 4 remove exits 0: passed.
- Step 5 refusal wording/recovery: passed.
- `[advisory]` mention ghost: failed, but it does not block.
- NO-ONBOARD/no picker/session in steps 5: passed.
- Relay/Files: for the integrator; step 6 observed.

## Final verdicts

VERDICT PR-01: passed
VERDICT GH-02: passed
VERDICT NA-05: passed
