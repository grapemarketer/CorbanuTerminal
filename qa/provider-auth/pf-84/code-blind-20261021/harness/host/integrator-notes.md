# Integrator notes (relay/files checks per case)
## G9 (oai1)
- NA-13: tester passed. Relay: chatgpt.com traffic at tone (22:18:28-30); no odefault during step 3 (22:18:02-22:22:00); odefault 401 at 22:17:32 = step 2 default session (expected). auth.json hash: tester's own hash 22:17:06 = end 76b93c8e (unchanged during the group); prep-time hash not captured (harness gap, fixed from G2). Device-code Escape left "Waiting for the sign-in" banner; auth.openai.com connection held until /quit 22:22:00 (33 KB) -> polling likely continued (advisory defect).
- D136: tester failed (no recovery text by x3). Relay: removal 22:24:07-09 (auth.openai.com 22:24:08 = revoke); afterone 22:24:46 -> chatgpt.com 22:24:48; aftertwo 22:26:33 -> chatgpt.com 22:26:35; both answered => session kept using t1's in-memory sign-in after removal (Q1 observe). No odefault 22:23:43-22:28:54 (no fallback). Frozen text "to end of group" includes ST-03 whose default session sends odefault by design (22:29:30, 22:29:53): judged within D136's window. Files: no t1 in final list, config unchanged, vault names .vault.lock + local.age.
- ST-03: passed; yone odefault 401 at 22:29:53 confirmed.
- Harness: signout script stopped at `account remove` (set -e) after logout said "not signed in"; t1 already removed by D136 ("Signed out and removed"); down then deleted all lane homes (0 entries left). Fixed for later groups.
## G1 (zai3)
- NA-01: failed by tester at CLIK: product refuses a typed key ("pipe the secret in instead of typing it") -> design assumption invalid; steps 2,4,6,7 not reachable. Step 3: clap echoes the key passed as an argument (observe Q3). Step 9 refusal text correct. -> amendment needed (pipe via --stdin).
- NA-02: passed; relay eone/etwo zwork, ethree zpersonal, efour zenv, efive zwork (22:39:54..22:40:58).
- NA-03: passed; relay nothing in refusal windows; gfour zwork 22:42:25.
- NA-04: failed by tester: same CLIK issue; `badone` never added. -> amendment.
## G2 (zai3) — one design premise cascades
- Session file of <id> 01a160a8: turn 1 provider_account work; every later turn (after `resume --account personal` + pone) personal. So "resume with no flag" followed the most recently recorded account (personal).
- PR-01: tester failed p2 (personal, expected work). Relay: pzero zwork 22:50:59; pone zpersonal 22:51:39; ptwo zpersonal 22:52:26; pthree zpersonal 22:53:15; pfour zenv 22:54:03. Steps 1,3,4 as expected; step 2 = latest-recorded semantics vs frozen "work" (designer Q7 flagged it; reviewer settled on work).
- GH-02: premise "<id> recorded on work" false after PR-01 -> resumed on personal; after removing work, resume <id> still worked on personal. Consistent with latest-recorded semantics; the GH-02 path (recorded account missing) not reached. gaone zpersonal 22:55:05.
- NA-05: same premise; in-app /resume of pzero conversation resumed on personal, no message. rtwo zpersonal 22:57:42 (after resume: session = <id>). 
- -> amendment A2: GH-02/NA-05 need a conversation whose last turn ran on work; PR-01 step 2 needs a conversation not re-recorded by step 1.
## G3 (zai3)
- NA-06: tester failed. (1) /providers list showed 8 rows: providers 1-6, `Z.AI · default`, `Z.AI · personal` (this session, default for new); no `work` row visible -> likely scrolled list (8-row window, no indicator; work = row 9?) — inconclusive; tester never moved past row 8. (2) Add-account name field: `text added` + `key Enter` in the same second -> Enter inserted a newline (name field stayed open, same in G9 NA-13 where a second Enter advanced); the key then typed went into the VISIBLE name field in clear; next Enter -> "invalid account name"; nothing saved. Probable paste-burst handling; a user who pastes a name and presses Enter at once gets the same. -> defect (secret shown in clear in a plausible flow) + amendment A3 (wait 2 s before Enter; scroll rows).
- ESC-03: passed (tester).
- NA-07: blocked (work row not found -> same scroll issue). On /quit a "Choose replacement" dialog (Ollama, LM Studio) appeared unexpectedly.
- Relay: only zpersonal 23:09:47, 23:10:06.
## G4 (zai3)
- NA-08: passed; relay fork/edit/side all zwork (23:20:30..23:22:42).
- NA-09: passed; fork + spawned agent (kidword) all zwork 23:24:06..23:24:39; ktwo zpersonal 23:25:23.
- NA-10: blocked: /providers shows exactly 8 rows (1-6 providers, Z.AI default, Z.AI personal); cursor moved 1..8, no scroll attempted past 8, no "more" indicator; `work` (alphabetically after personal) never visible. Same as G3. sone zpersonal 23:26:27.
## G5 (zaione, fresh)
- ST-01: passed (plain /status in zaione). Relay zenv 23:34:51.
- ST-02: failed by tester — every step depends on CLIK `account add` (refused: pipe required) -> amendment A1. Relay zenv 23:36:05, 23:36:51. NO-SECRET "failure" = tester's own echoed typing after product exit (cat echo), not product output.
- ESC-04: blocked — onboarding list (15 rows + logo) is taller than the 45-row terminal; tester configured Z.AI (masked entry, ok) then looped list <-> Z.AI actions; the list's last row ("Done", per the item 4 text "runs after Done") was never scrolled to. -> amendment: scroll to the last row / Done.
## G6 (zainokey) — tester stopped
- Tester (GLM) hit "model provider stopped generation at its output or context limit" at 23:53 after NA-11 and ESC-01 (22-min degenerate stream, 128k events), wrote no result.md. Screens show ESC-01: picker, Esc -> exit 0, terminal settings normal, echo restored. NA-11 relay zwork 23:50:52, 23:52:04. I426-A not run. -> replay G6 unchanged.
## G7 (custom)
- NA-12: passed; relay cone zenv 00:21:50, ctwo zpxwork 00:22:34, cthree zpxwork 00:24:19, cfour zpxwork 00:25:26.
- I426-B: passed; nothing after 00:25:39 (refusals sent nothing).
## G8 (zai3off, zainokeyoff)
- REG-01a passed (oone/otwo zenv 00:36:36, 00:37:09; exec --account and account list refused: feature off).
- REG-01b passed (wone zwork 00:38:31 with --enable; resume without it refused with the raw "Could not resume session … named accounts need …" error; resume --account zai:default -> wtwo zenv 00:39:12, plain status).
- REG-01c passed (picker Esc with feature off reached via zai3off; closes REG-01's gap). Relay 00:40:30 zenv.
- REG-01d blocked by harness: first C-c ended the wrapper/tmux server -> lost observation. Harness fixed (trap INT); replayed as G8R.
## G6R (replay of G6, harness unchanged for these cases)
- NA-11 passed (zwork 00:57:58, 00:59:35). I426-A passed (no request 01:02:00-01:03:09; exit 1, recovery text, no onboarding, at 2 s too).
- ESC-01: tester "failed" only on "echo restored 9 appears twice": the harness prints the typed line via tty echo AND via cat, so two copies = cooked mode with echo, i.e. a restored terminal (review-facts item 3 said only "Text typed after that is echoed"). e2: exit 0 within 10 s, settings `icanon isig echo icrnl opost`. Integrator: passed (harness artifact; the tester's check was mis-specified by the harness description).
- G8R REG-01d: passed (onboarding as earlier build; exit 0 after C-c; settings normal; echo clean).
## R1 (amendment v2: NA-01, NA-04, NA-10)
- NA-01 passed (CLIS adds; xone zextra 01:21:31; removed -> xtwo refused, nothing sent; key on argv echoed by clap (observe Q3)).
- NA-04 passed (badone/badkey: two 401s at 01:22:54, 01:23:19 carrying badkey only; no fallback).
- NA-10: tester failed one blocker line: s2 "Account usage table with work (active) and personal". The chosen "Use for this session" starts a NEW session (UI: "New session: requests to zai use account `work`", SID changed), so a one-row table for work is consistent with the product's own wording; the frozen expectation assumed the same session. Integrator: expectation not applicable (design premise), not a product defect; the per-account multi-row table is checked in PR-01/NA-08. Relay sone zpersonal 01:25:50, stwo zwork 01:28:28. Advisory failure: s1 row omitted the cached-input part (no cache that turn).
- Survey CONFIRMED: /providers has 18 rows, shows 8 at a time, scrolls with no hint; wraps. `work` is row 10. -> UX defect (advisory): hidden rows, no indicator.
## R2 (amendment v2: PR-01, GH-02, NA-05)
- PR-01 passed: relay pzero zwork 01:37:22, pone zpersonal 01:38:14, re-anchor zwork 01:39:36, ptwo zwork 01:40:17, pthree zpersonal 01:40:59, pfour zenv 01:41:49. Q1 observe: right after the personal turn, `resume <id>` with no flag opened on personal (latest recorded).
- GH-02 passed: gzero/gaone zwork 01:42:41, 01:43:22; after `account remove zai work`, `resume <gid>` refused (exit 1, names work, `corbanu account add zai work`); `ghost` not mentioned (advisory).
- NA-05 FAILED — real defect: in-session `/resume` of <gid> (all turns on removed `work`) silently resumed it on `personal`: no message, SID switched to <gid>, /status `· account personal`, Account usage `personal (active)` + `work`. Session file: turns 1-2 work, turn 3 (rtwo, 01:45:15) personal; relay rtwo zpersonal 01:45:21. The CLI path refuses (GH-02); the in-app path falls back to the current account. REF-10 (earlier build) showed the missing-account message. Silent account switch = blocker.
- NA-05 re-analysis: docs/provider-accounts.md "Choosing an account": an explicit `--account` wins over the account a resumed thread recorded. NA-05 starts the TUI with `--account personal`, so applying it to an in-app /resume matches the documented precedence; REF-10's situation (session not started with --account) is different. -> integrator probe planned (no --account) to separate "launch flag wins" from "silent fallback".
## R3 (amendment v2: NA-06, NA-07)
- NA-06: rows/fingerprints/markers passed; SURVEY confirms 17 rows, 8-row viewport, no hint, wraps. Add with FIELD (2 s wait) advanced with ONE Enter, key masked, saved `added` with fp -> confirms the G3/G9 double-Enter was fast typing (paste-like burst) in the name field. Rename NOT exercised: tester error (Enter on the highlighted "Use for this session" started a session on `added`; it then typed `renamed` as a model prompt; relay zadded 01:56:01-01:56:14). The later Rename was refused correctly ("This session uses zai account `added`. Use another account for this session first, then rename it."). The four rename-dependent "failed" lines follow from the tester error, not the product. exec --account added -> nold zadded 01:59:20 (consistent: not renamed).
- NA-07 passed (relay uone..: zwork 02:02:04, 02:03:42; zpersonal 02:06:24; nothing at ufour).
## R4 (amendment v2: ST-02, ESC-04) — tester wrote result.md under work/evidence
- ST-02: d0 plain; d1 plain (stale); d2 `· account default` + usage (D124 refresh on the next /status); d4-2 plain after removal; all /status complete, none late/stalled. Tester error: CLIS piped the literal placeholder `k` as the key for both adds (actions.jsonl), so dtwo's request carried key sha256 prefix 8254c329 = sha256("k") = extrb's stored key -> routing correct (dtwo on extrb). dzero zenv 02:18:08, dthree zenv 02:27:26. d5-2 "failed": each "Use … for this session" starts a NEW session (UI "New session: requests to zai use account …"); the session at d5 only ever used default, so plain is D86-consistent; frozen premise (one session) wrong — not a product defect.
- ESC-04 passed: SURVEY reached row 17 "Done — Finish provider setup" (16 providers; onboarding scrolls without hint), composer reached, fr1-2 plain, no named accounts; frone carried zfresh with model glm-5.2 (Q9 observe: fresh onboarding picked glm-5.2).
- Correction (review round 2): the R1 NA-10 note above says the multi-row table "is checked in PR-01/NA-08"; NA-08 shows no such table. Multi-account tables were seen in resumed conversations in PR-01 and NA-05 (R2).
