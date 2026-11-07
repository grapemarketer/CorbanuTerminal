# R2 amendment-v2 functional run — executor report

Started UTC: 2026-10-22T01:36:16Z.
Preflight: `agms-probe all` rc=0 and `ok=true`; summary had `ok=true`, 97 probes, no failures.
Candidate SHA-256 matched: `bbae7d37dff9023268efdd05a8a4408ccf144ee51ba911afd39adc0e9ddce1b8`.
Design-cases SHA-256 matched: `cfad42f543b6782a70b935b0596ac50737468733f3480629f49089e34eb6e5b4`.
Amendment SHA-256 matched: `fe1324914155fc4d37a83a4ac3bd7bdd0ac67f636cdf31e1da0cfb7c6a0f7e77`.
Available prepared home: `zai3`.

## PR-01
- Requested: literal amendment-v2 PR-01 actions on `zai3`, using windows in order.
- 01:36:49 START w1 (`--account work`); 01:36:58 matched glm-5.3-flash; `pr01-start-w1`: new TUI, footer `via zai default`.
- 01:37:05 `/status` => `pr01-p0`: `· account work`, `Account usage: work · no usage yet`, Session `01a16140-fe46-7201-81c5-6defd783fe84` (SID `<id>`).
- 01:37:16 pzero sent; 01:37:22 `• pzero`; `pr01-pzero`. QUIT w1 01:37:38, exit 0, settings `icanon isig echo icrnl opost`.
- 01:37:46 START w2 `resume --account personal <id>`; 01:37:49 matched model; `pr01-start-w2`.
- 01:37:56 `pr01-p1-pre`: `· account personal`, Account usage table `personal (active)` 0 and `work` 8595/16/12.
- 01:38:06 pone sent; 01:38:15 `• pone`; `pr01-pone`.
- 01:38:26 `pr01-p1`: `· account personal`, table `personal (active)` 43 input/8576 cached/9 output/6 reasoning, `work` 8595/0/16/12.
- QUIT w2 01:38:32, exit 0, normal settings.
- 01:38:46 `[observe]` Q1 START w3 `resume <id>`; 01:38:59 `pr01-pq`: `· account personal`, Account usage table with `personal (active)` and `work`. No prompt sent. QUIT 01:39:07 exit 0.
- 01:39:14 START w4 `resume --account work <id>`; 01:39:24 `pr01-pb`: `· account work`, table `work (active)` and `personal`.
- 01:39:31 pback sent; 01:39:37 `• pback`; `pr01-pback`. QUIT 01:39:44 exit 0.
- 01:39:52 START w5 `resume <id>`; 01:40:05 `pr01-p2-pre`: `· account work`.
- 01:40:12 ptwo sent; 01:40:17 `• ptwo`. 01:40:27 `pr01-p2`: `· account work`, table `work (active)` and `personal`. QUIT 01:40:28 exit 0.
- 01:40:35 START w6; 01:40:43 `pr01-p3-pre`: `· account personal`.
- 01:40:54 pthree sent; 01:40:59 `• pthree`. 01:41:09 `pr01-p3`: `· account personal`, single `personal` usage row. QUIT 01:41:09 exit 0.
- 01:41:17-01:41:19 config before/after; exact sed removed `[provider_accounts]`; files saved as `pr01-config-before.txt`/`pr01-config-after.txt`.
- 01:41:27 START w7; 01:41:36 `pr01-p4-pre`: `· account default`.
- 01:41:44 pfour sent; 01:41:49 `• pfour`. 01:42:00 `pr01-p4`: `· account default`, single `default` usage row. QUIT exit 0; `agms-tui stop` 01:42:00.
- Relay/Files results for integrator: pzero/pone/pback/ptwo/pthree/pfour sent at logged times; Q1 had no prompt; check keys and `<id>` turn records.
- PR-01 visible outcomes: passed. All status boxes complete; no onboarding wording or secrets observed.

## GH-02
- Requested: literal amendment-v2 GH-02 actions. PR-01 left no `[provider_accounts]`; `<id>` = `01a16140-fe46-7201-81c5-6defd783fe84`.
- 01:42:25 START w8 `--account work`; gzero sent 01:42:34; 01:42:41 `• gzero`.
- 01:42:51 `gh02-g0`: `· account work`, Session `<gid>` = `01a16146-19a1-7310-876a-e932924807a6`. QUIT exit 0.
- 01:42:57 config append verified in `gh02-config-after-append.txt`; `[provider_accounts] zai = "ghost"`.
- 01:42:57 START w9 `resume <gid>`; 01:43:10 `gh02-g1`: `· account work`.
- gaone sent 01:43:17; 01:43:22 `• gaone`; `gh02-gaone`. QUIT 01:43:27 exit 0.
- 01:43:31 CLI w10 `account remove zai work`: exit 0, `Removed zai account \`work\`.`
- 01:43:41 REFUSE w11 `resume <gid>`, label g5; `gh02-g5-2s` and `gh02-g5`: exit 1. Verbatim screen: `ERROR: This conversation ran on zai account \`work\` and resumes on it: account \`work\` of provider \`zai\` is not configured; add it with \`corbanu account
 add zai work\` or pick another with \`--account\` (see \`corbanu account list\`)`. It names `work` and recovery; does not mention `ghost` (advisory only). No picker/session/onboarding. Stopped 01:43:47.
- Relay/Files for integrator: gzero/gaone times above; check `zwork`, and `<gid>` turn records.
- GH-02 visible outcomes: passed (ghost mention advisory absent).


## NA-05
- Requested: literal amendment-v2 NA-05 actions after GH-02. Home state was as required: `work` removed, `[provider_accounts] zai = "ghost"`, `<gid>` recorded on `work`.
- 01:44:09 START w12 `--account personal`; 01:44:18 rone sent; 01:44:24 `• rone`.
- 01:44:31 `na05-r1`: `· account personal`, Session SID1 = `01a16147-b0b7-7542-9f10-d9e4f90c42b9`.
- 01:44:38 `/resume`; 01:44:42 `na05-resume-open`: picker with rone, gzero, pfour, pthree, pzero.
- Typed `gzero`; 01:44:44 `na05-resume-filtered`: one highlighted line containing `gzero`. 01:44:51 pressed Enter.
- 01:44:56 `na05-r-msg`: no missing-account message. The product switched into `<gid>` and displayed gzero/gaone; footer and composer were normal.
- 01:45:06 `na05-r2`: Session is `<gid>` (`01a16146-19a1-7310-876a-e932924807a6`), not SID1; provider line is `· account personal`; Account usage table has `personal (active)` 0/0/0 and `work` 17240/45/36.
- 01:45:15 rtwo sent; 01:45:21 `• rtwo`; `na05-rtwo`. QUIT 01:45:28 exit 0, normal settings. Stopped 01:45:28.
- Relay/Files for integrator: rone/rtwo times above; check `zpersonal`; verify no `zenv` request during NA-05; check recorded account for rtwo.

## Expected-result outcomes
- PR-01:
  - p0 provider `work`: passed (`pr01-p0`).
  - p1-pre/p1 provider `personal` and usage table with active personal plus prior work: passed (`pr01-p1-pre`, `pr01-p1`).
  - Q1 `pq`: observed—provider `personal`, usage table, no prompt sent (`pr01-pq`).
  - pb provider `work`: passed; no re-anchor failure (`pr01-pb`).
  - p2-pre/p2 provider `work` and table: passed (`pr01-p2-pre`, `pr01-p2`).
  - p3-pre/p3 provider `personal`: passed (`pr01-p3-pre`, `pr01-p3`).
  - config removal: passed; no `provider_accounts` remained.
  - p4-pre/p4 provider `default`: passed (`pr01-p4-pre`, `pr01-p4`).
  - Relay mappings and session-file account records: for the integrator.
- GH-02:
  - g0 provider `work`: passed (`gh02-g0`).
  - g1 provider `work` after ghost default: passed (`gh02-g1`).
  - `account remove zai work` exit 0: passed (`gh02-remove-work`).
  - g5 refusal exit 1, conversation/work wording, recovery, no picker/session/onboarding: passed. It did not mention `ghost`; that wording is advisory only.
  - Relay and files: for the integrator.
- NA-05:
  - r1 new personal session and SID1: passed (`na05-r1`).
  - resume picker found gzero and Enter resumed it: actual action succeeded, but the required r-msg missing-account message did not appear: failed.
  - session stays put: failed. `na05-r2` has `<gid>`, not SID1. It did show `· account personal`; transcript evidence of `rone` was no longer visible on the final screen, but the SID change is dispositive.
  - NO-ONBOARD: passed in all NA-05 captures.
  - Relay `rtwo -> zpersonal` and no `zenv`: for the integrator.

No model decline or refusal wording appeared. No system password prompt, real sign-in screen, outside-run access, missing required navigation control, lost observation, or timeout was encountered. All actions went through `agms-tui` windows w1–w12 as noted; every case ended with `agms-tui stop`. Screen text and times above are from live captures.

VERDICT PR-01: passed
VERDICT GH-02: passed
VERDICT NA-05: failed
