## What I changed and why

1. **CLIK is replaced by CLIS, which pipes the key with `--stdin`.** This affects NA-01 step 2, NA-04 step 1, and ST-02 steps 2 and 5. Run observation 1: `account add` refuses a key typed in a terminal, so none of these adds ever happened. Piped keys are never typed, so NO-SECRET's typed-text exception no longer covers them.
2. **PR-01: a re-anchor turn on `work` now comes before the no-flag resume.** Observation 2: the `--account personal` turn re-recorded `<id>` on `personal`. So "resume with no flag of a conversation recorded on `work`" was never tested. After `work` → `personal` → `work`, `<id>` is on `work` however "recorded" is read. The frozen expectations stay, including the p2 table. I added an `[observe]` resume with no flag and no prompt right after the `personal` turn (owner question Q1).
3. **GH-02 now makes its own conversation `<gid>`, run only on `work`.** Observation 2: the old `<id>` had moved to `personal`, so "the recorded account was removed" was never set up. I added an `[observe]` REFUSE of PR-01's mixed `<id>` (Q1).
4. **NA-05 now resumes `<gid>` (search `gzero`) instead of `pzero`.** Same reason as GH-02.
5. **PROVIDERS is rewritten, with new SURVEY and ROW conventions.** Observation 3: only 8 rows showed and testers never went past the last one. The tester now scrolls past the last visible row and takes a screen after every key. It records as `[observe]` whether the list scrolls, how many rows there are, whether any hint shows more rows, and whether it wraps. Then it carries on with the row it finds. This applies to NA-06, NA-07, NA-10 and ST-02.
6. **New FIELD and KEYIN conventions.** Observation 4: the tester now waits 2 s between typing and Enter, and never types a key unless the screen is asking for a key. This applies to the NA-06 add and rename, and to the ESC-04 key.
7. **NO-SECRET is rewritten.** A key shown in a field that isn't masked is now a NO-SECRET failure only if the product put it there. If the tester typed it there, it is `[observe]` with the steps that led to it.
8. **ESC-04 step 5 is rewritten so onboarding finishes.** Observation 5: the tester never chooses the Z.AI row again, surveys the list to its end, and looks for a "Done"-like row or a footer key.
9. **Windows is restated so it includes CLIS.** I also added a "Recorded accounts" convention.
10. **Starting conditions changed because groups were regrouped:**
    - NA-07 now runs after NA-06; ESC-03 is not re-run.
    - ST-02 now runs on `zaione` as prepared; ST-01 is not re-run.
    - NA-10 now runs after NA-04.

**Not amended:** every other case, G6 and REG-01d (both replayed unchanged), and NA-13 (see Q3).

---

## amendments-v2 (PF-84 pf84-cb1)

The frozen design stays in force. This text replaces only the cases and conventions given here.

### Conventions

All frozen conventions apply. Those restated below replace the frozen ones of the same name; those marked *new* are added.

**Windows** (restated)
- Every START, CLI, CLIS and REFUSE opens a **new** window. Name windows `w1`, `w2`, `w3` … in order for the whole group, and never reuse a name.
- Every later command for that program is `agms-tui --window <its name> …`.
- "Window A" or "window B" in a case is a nickname for the window that the case's START opened. Log the mapping, for example "A = w4".
- Every case ends with `agms-tui stop`.

**CLIS(H, cmd, k)** (replaces CLIK; the key is piped on stdin)
1. `agms-tui --window <new> start --home H --stdin k cmd`.
2. `wait 'product exited' 30`, then `screen`. Log the exit code and the time.
3. If it has not exited:
   - `screen`, and log "stuck: <what the screen shows>";
   - `key C-d`, `wait 'product exited' 10`;
   - if it still has not exited: `key C-c`, `wait 'product exited' 10`;
   - `screen`.
4. Never type the key with `agms-tui text`.

**Recorded accounts** (new)
- A conversation's files record the account of every turn.
- A case that needs "a conversation recorded on X" creates it itself with START(H, `--account X`) and an ASK, and logs its SID.
- After that, unless the case says otherwise, turns in it run only on X.

**FIELD(t)** (new; text for a field that is not secret, such as an account name)
1. `screen`. Check that the screen asks for the field the case names. If it does not, log "field not shown" and follow the case's fallback.
2. `text 't'`, `sleep 2`, `screen`.
3. `key Enter`, `sleep 2`, `screen`.
4. If the same field is still shown and still holds `t` (for example with an empty line added):
   - log "Enter did not advance";
   - `key Enter` once more, `sleep 2`, `screen`, and log what it shows;
   - do not press Enter a third time.
- Never press Enter within 2 s of typing into any field.

**KEYIN(k, l)** (new; a key typed into the product's key field)
1. Type the key only when the current screen asks for a key: the focused field or its prompt contains "key", "API key", "secret" or "Paste", and no name field is shown with the cursor in it.
2. Otherwise do not type it. Log "key field not reached", `screen`, and follow the case's fallback.
3. `text 'k'`, `sleep 2`, `screen l`.
4. Press Enter only where the case says so: `key Enter`, `sleep 2`, `screen`.

**NO-SECRET(label, digits)** (restated)
- No capture shows `agms-fake`, the key's label or its 5-digit number.
- The one `[observe]` exception: a key the tester typed with `agms-tui text`, shown in the field it was typed into, before Enter. If that field is not masked, record it as `[observe]` with the steps that led to it: the screen that asked, the field it went into, and the keys pressed before and after.
- A key visible in a field or anywhere else is a NO-SECRET failure only if the product put it there. Examples:
  - it shows in a field, row, message, list or transcript the tester did not type it into;
  - it is still shown after Enter has moved to another screen.
- A key that is visible only because the tester typed it into that field (even the wrong field) is `[observe]` with the steps.
- Keys given through CLIS are never typed, so the exception never covers them. A line starting `[agms]` that repeats the harness's own start command is `[observe]`.
- Account names, fingerprints and email hints are not secrets.

**PROVIDERS** (restated)
1. `text '/providers'`, `key Enter`, `sleep 2`, `screen`.
2. Move with `Up`/`Down`, open with `Enter`, go back with `Escape`, and take a `screen` after every key.
3. "Open the Z.AI entry" (or OpenAI) means: press `Down` until the highlighted line contains "Z.AI" (or "OpenAI"), then `Enter`. If no line contains it, SURVEY the list first. If there is still none, log "not found".
4. "Select account X" means ROW(Z.AI, X).
5. "Open its actions" means `Enter` (or the key the footer hints) on the highlighted row, then `screen`. If the named action is not visible in the actions list, SURVEY that list.
6. Spend at most 4 minutes looking for a named action, not counting SURVEY. If it is not found, log "not found" with the screens.
7. When done, press `Escape` until the composer (`›`) shows.

**SURVEY** (new; for any list that may have hidden rows; once per list per case, the first time the case needs a row from it)
1. `screen`. Note the highlighted line and the first and last visible rows.
2. Press `Down` one key at a time, with a `screen` after each. Stop when the highlight stops moving (two identical screens in a row) or comes back to a row already highlighted; at most 30 presses.
3. Press `Down` once more, then `screen`, to see whether it wraps.
4. Press `Up` one key at a time, with a `screen` after each, until the highlight is back on the row from step 1; at most 30 presses.
5. Record as `[observe]` in a "survey" note:
   - whether the list scrolled (rows left the top and new ones appeared at the bottom);
   - how many distinct rows exist, with their text, in order;
   - whether any hint showed, before scrolling, that more rows exist (an arrow, "more", a count such as `8/10`, a scrollbar);
   - whether `Down` at the end wraps;
   - the footer text.
6. Continue the case with the row it finds.

**ROW(P, X)** (new)
1. If this list has not been surveyed in this case, SURVEY it.
2. The target is the line containing provider P and account X as a separate word, for example `Z.AI · work`. Inside an opened provider entry, it is the line containing X.
3. Press `Up`/`Down` one key at a time, with a `screen` after each, until the target is highlighted. Then `screen`.
4. If the list has no such line: open the P entry, SURVEY what it shows, and look there.
5. If it is found nowhere: log "not found: P X" with the survey screens, and record the steps that need it as "not reached".

### Re-run group plan

Each group gets one fresh machine, homes prepared as in tools.md, and about 80 minutes (it is ended at 90). Run the cases in the order listed.

| Group | Home(s) | Cases | Est. minutes |
|---|---|---|---|
| R1 | zai3 | NA-01, NA-04, NA-10 | 45 |
| R2 | zai3 | PR-01, GH-02, NA-05 | 60 |
| R3 | zai3 | NA-06, NA-07 | 55 |
| R4 | zaione, fresh | ST-02, ESC-04 | 55 |

G6 and REG-01d are replayed unchanged, separately.

---

### NA-01: CLI list/add/remove; keys only from stdin; no secrets (AC1, AC3, AC6)
- **Priority:** Blocker
- **Home:** zai3 as prepared; no TUI running.
- **Actions:**
  1. CLI(zai3, `account list`).
  2. CLIS(zai3, `account add zai extra`, `agms-fake-zextra-58213`).
  3. CLI(zai3, `account add zai extra2 agms-fake-zextratwo-61407`).
  4. CLI(zai3, `account list`).
  5. CLI(zai3, `login status`).
  6. CLI(zai3, `exec --account extra "reply with the single word xone"`). Log the start and exit times.
  7. CLI(zai3, `account remove zai extra`).
  8. CLI(zai3, `account list`).
  9. CLI(zai3, `exec --account extra "reply with the single word xtwo"`). Log the start and exit times.
  10. `agms-tui stop`.
- **Expected:**
  - Step 1 exits 0 and lists `work` and `personal` for Z.AI.
    - `[observe]` Whether `default` is listed, and whether rows show a kind and a fingerprint.
  - Step 2 exits 0 and confirms `extra`.
  - Step 3 exits non-zero, and step 4 has no `extra2`.
    - `[observe]` Whether step 3's error repeats the key given on the command line.
  - Step 4 lists `extra`.
  - Step 5 shows no `agms-fake`.
    - `[observe]` Its wording and exit code.
  - Step 6 exits 0, and the output contains `xone`.
  - Step 7 exits 0, and step 8 has no `extra`.
  - Step 9 exits non-zero, and no `xtwo` reply is printed. Its text:
    - names account `extra` and provider `zai`;
    - gives `corbanu account add zai extra`;
    - mentions `--account`.
  - NO-SECRET(zextra, 58213) holds in every capture.
  - **Relay:**
    - `xone` → `zextra`.
    - Nothing at `xtwo`.
    - No request ever carries `zextratwo`.

### NA-04: A rejected key names the account; no fallback (AC3, AC6)
- **Priority:** Blocker
- **Home:** zai3, after NA-01.
- **Actions:**
  1. CLIS(zai3, `account add zai badone`, `agms-fake-badkey-77302`).
  2. CLI(zai3, `exec --account badone "reply with the single word bone"`). Log the times.
  3. START(zai3, `--account badone`).
  4. SEND(btwo, 60).
  5. STATUS(n4-a), QUIT, then `cat CFG(zai3)`, then `agms-tui stop`.
- **Expected:**
  - Step 1 exits 0. This is a precondition: if it does not, log it and record the rest as "not reached".
  - Step 2 exits non-zero and no `bone` reply is printed. The error:
    - names `badone`;
    - gives the recovery `corbanu account add zai badone`.
  - In step 4, the window shows an error naming `badone`. No `• btwo` appears, and NO-ONBOARD holds.
  - n4-a shows `· account badone`.
  - NO-SECRET(badkey, 77302) holds.
  - **Relay:**
    - Every request at `bone` and `btwo` carries `badkey` (401).
    - None carries `zenv`, `zwork` or `zpersonal`, and none is keyless.
  - **Files:** CFG(zai3) still has `zai = "personal"`.

### NA-10: `/status` format with named accounts (AC5, D86)
- **Priority:** Blocker (exact layout is advisory)
- **Home:** zai3, after NA-04 (`zai = "personal"`; accounts `work`, `personal`, `badone`).
- **Actions:**
  1. START(zai3), STATUS2(s0), ASK(sone), STATUS(s1).
  2. PROVIDERS, ROW(Z.AI, work), open its actions, choose "Use for this session", then `Escape` to the composer. ASK(stwo), STATUS(s2).
  3. QUIT, then `agms-tui stop`.
- **Expected:**
  - s0-2 shows `· account personal`, and its "Account usage" contains `no usage yet`.
  - s1's "Account usage" is one `personal` row with input and output counts.
    - `[advisory]` The form is `personal · N input · N cached input · N output (N reasoning)`.
  - s2 shows `· account work`, and "Account usage" is a table with `work (active)` and `personal`.
  - No capture shows `$`, "cost" or "price".
  - The "Account:" row, if present, shows no `agms-fake`.
  - `[observe]` The footer line in each capture (for example `via zai default` while on another account).
  - `[observe]` The survey note.
  - **Relay:** `sone` → `zpersonal`, and `stwo` → `zwork`.

### PR-01: Full precedence order, confirmed by `/status` and the relay at every step
- **Priority:** Blocker
- **Home:** zai3 as prepared (`[provider_accounts] zai = "personal"`).
- **Actions:**
  1. Setup: START(zai3, `--account work`), ASK(pzero), STATUS(p0). Log the SID as `<id>`. QUIT.
  2. START(zai3, `resume --account personal <id>`), STATUS(p1-pre), ASK(pone), STATUS(p1), QUIT.
  3. `[observe]` leg (Q1): START(zai3, `resume <id>`), STATUS(pq), QUIT. Send no prompt. Log the times.
  4. Re-anchor on `work`: START(zai3, `resume --account work <id>`), STATUS(pb), ASK(pback), QUIT.
  5. START(zai3, `resume <id>`), STATUS(p2-pre), ASK(ptwo), STATUS(p2), QUIT.
  6. START(zai3), STATUS(p3-pre), ASK(pthree), STATUS(p3), QUIT.
  7. Remove the section: `sed -i '' -E '/^\[provider_accounts\]/,/^zai[[:space:]]*=/d' CFG(zai3)`, then `cat`. There must be no `provider_accounts`.
  8. START(zai3), STATUS(p4-pre), ASK(pfour), STATUS(p4), QUIT, then `agms-tui stop`.
- **Expected:**
  - The provider line reads, in order:
    - `· account work` in p0;
    - `· account personal` in p1;
    - `· account work` in p2;
    - `· account personal` in p3;
    - `· account default` in p4 (the home still has named accounts).
    - The `-pre` captures show the same accounts.
  - Precondition: pb shows `· account work`. If it does not, log "re-anchor failed"; p2 is still judged.
  - In p1 and p2, "Account usage" is a table:
    - the session's account is marked `(active)`;
    - the conversation's other account appears with its earlier counts.
    - `[observe]` p1-pre and p2-pre.
  - `[observe]` pq's provider line and "Account usage" (Q1).
  - **Relay:**
    - `pzero` → `zwork`.
    - `pone` → `zpersonal`.
    - `pback` → `zwork`.
    - `ptwo` → `zwork`.
    - `pthree` → `zpersonal`.
    - `pfour` → `zenv`.
    - `[observe]` Any request during step 3.
  - **Files:**
    - `<id>` records provider `zai` and account `work` for its first turn.
    - `[observe]` What it records for `pone`, `pback` and `ptwo`, and whether step 3 changed anything.

### GH-02: The recorded account was removed while `[provider_accounts]` names a missing one
- **Priority:** Blocker (only the mention of `ghost` is advisory)
- **Home:** zai3, right after PR-01 (no `[provider_accounts]`; `work` and `personal` exist).
- **Actions:**
  1. Setup, a conversation recorded only on `work`: START(zai3, `--account work`), ASK(gzero), STATUS(g0). Log the SID as `<gid>`. QUIT.
  2. `printf '\n[provider_accounts]\nzai = "ghost"\n' >> CFG(zai3)`, then `cat`.
  3. START(zai3, `resume <gid>`), STATUS(g1), ASK(gaone), QUIT.
  4. CLI(zai3, `account remove zai work`). Log the time.
  5. REFUSE(zai3, `resume <gid>`) (label g5). Log the time.
  6. `[observe]` leg (Q1): REFUSE(zai3, `resume <id>`) (label g6), with PR-01's `<id>`. Log the time, then `agms-tui stop`.
- **Expected:**
  - Precondition: g0 shows `· account work`.
  - In step 3, g1 shows `· account work`, as in REF-05.
  - Step 4 exits 0.
  - Step 5 exits non-zero. Its text says "This conversation ran on zai account `work`" and gives `corbanu account add zai work`. NO-ONBOARD holds, and there is no picker or session.
    - `[advisory]` The text also mentions `ghost`.
  - `[observe]` Step 6: the exit code, the text, and the account shown if a session opens (Q1).
  - **Relay:**
    - `gzero` → `zwork`.
    - `gaone` → `zwork`.
    - Nothing during step 5.
    - `[observe]` Any request during step 6.
  - **Files:** `[observe]` `<gid>` records `work` for every turn.

### NA-05: In-session `/resume` of a conversation whose account is gone (as in REF-10)
- **Priority:** Blocker
- **Home:** zai3, right after GH-02 (`work` removed; `zai = "ghost"`; `<gid>` recorded only on `work`).
- **Actions:**
  1. START(zai3, `--account personal`), ASK(rone), STATUS(r1). Log SID1.
  2. `text '/resume'`, `key Enter`, `sleep 3`, `screen`. Then `text 'gzero'`, `sleep 2`, `screen`.
  3. If the highlighted line contains `gzero`, `key Enter`. Otherwise press Up/Down, with a `screen` after each, until it does, then `key Enter`. If no line contains it, log "not found", press `Escape` until the composer shows, and skip step 4.
  4. `sleep 5`, `screen r-msg`.
  5. STATUS(r2), ASK(rtwo), QUIT, then `agms-tui stop`.
- **Expected:**
  - r-msg shows a message that the conversation ran on zai account `work`, which is not configured. The recovery mentions adding it or picking another account in `/providers`.
  - The session stays put: r2 shows SID1 and `· account personal`, and the transcript still shows `rone`.
  - NO-ONBOARD holds.
  - **Relay:** `rtwo` → `zpersonal`. Nothing in this case carries `zenv`.

### NA-06: `/providers` rows, adding with a key, rename (AC1, AC6)
- **Priority:** Blocker
- **Home:** zai3 as prepared.
- **Actions:**
  1. Look at the rows:
     - START(zai3). PROVIDERS, then SURVEY the list (screens `n6-s1`, `n6-s2` …). `screen n6-list`.
     - ROW(Z.AI, work), `screen n6-work`. Do not press Enter.
     - ROW(Z.AI, personal), `screen n6-personal`.
     - Open the Z.AI entry, `screen n6-zai`.
  2. Add an account:
     - Choose the action whose label contains "Add" (for example "Add another account"): highlight it, `sleep 2`, `key Enter`, `sleep 2`, `screen`.
     - Handle the add screens in the order they come (at most 6):
       - a name field: FIELD(`added`);
       - a choice of kind: the line containing "API key", then `key Enter`, `sleep 2`, `screen`;
       - a key field: KEYIN(`agms-fake-zadded-40958`, n6-key) with Enter, then `screen n6-after`;
       - anything else: `screen`, and log it.
     - Fallback: if FIELD logs "Enter did not advance" twice, or KEYIN logs "key field not reached", press `Escape` until the composer shows. Skip step 3, and run steps 4–7.
  3. Rename it:
     - If not already on the list, PROVIDERS. ROW(Z.AI, added), `screen n6-added`. Open its actions.
     - Choose the action containing "Rename", then `screen`.
     - Clear the name with `key C-u`, then `BSpace` until it is empty, then `screen`.
     - FIELD(`renamed`). If the list is not shown, ROW(Z.AI, renamed). Then `screen n6-renamed`.
  4. Press `Escape` until the composer shows, then QUIT.
  5. CLI(zai3, `account list`).
  6. CLI(zai3, `exec --account renamed "reply with the single word nren"`).
  7. CLI(zai3, `exec --account added "reply with the single word nold"`), then `agms-tui stop`.
- **Expected:**
  - On the screens where they are visible (n6-work, n6-personal, n6-zai or the survey screens), rows `work` and `personal` each show a kind and a fingerprint.
    - `personal` carries both the current-session marker and the default marker.
    - `[observe]` The `default` row's contents, and the survey note (Q2).
    - `[advisory]` Marker wording.
  - n6-after (or n6-added) shows an `added` row with a kind and a fingerprint, and n6-renamed shows it as `renamed`.
  - NO-SECRET(zadded, 40958) holds in every capture after the key was entered.
    - `[observe]` Whether n6-key shows the typed key or a mask.
    - `[observe]` Any capture showing the key in a field that is not masked, with the steps (NO-SECRET rule).
  - Step 5 lists `renamed` and not `added`.
  - Step 6 exits 0 and prints `nren`.
  - Step 7 exits non-zero with the not-configured text for `added`.
  - **Relay:**
    - `nren` → `zadded`.
    - Nothing at `nold`.

### NA-07: Use for this session, make default, and removing the account in use (AC1, AC2, AC3)
- **Priority:** Blocker
- **Home:** zai3, after NA-06.
- **Actions:**
  1. START(zai3) (window A), then STATUS(u0).
  2. Window A: PROVIDERS, ROW(Z.AI, work), open its actions, choose the action like "Use for this session", then `Escape` to the composer. STATUS(u1), ASK(uone).
  3. Window A: PROVIDERS, ROW(Z.AI, work), open its actions, choose the action like "Make default", then `Escape` to the composer. Run `grep -n -A2 'provider_accounts' CFG(zai3)` and save the output.
  4. START(zai3) (window B), STATUS(u2), ASK(utwo).
  5. Window A: PROVIDERS, ROW(Z.AI, work), open its actions, choose the action like "Remove". Take a `screen` at every prompt.
     - If asked for a replacement, move to the line containing `personal` (with a `screen` after each key; SURVEY it if `personal` is not visible), then choose it and confirm.
     - If asked to confirm the removal, confirm.
     - `Escape` to the composer, then STATUS(u3), ASK(uthree).
  6. Window B: SEND(ufour, 60), STATUS(u4).
  7. CLI(zai3, `account list`), `cat CFG(zai3)`, then `agms-tui stop`.
- **Expected:**
  - u0 shows `· account personal`, and u1 shows `· account work`.
  - u2 shows `· account work`: a new session uses the new default.
    - `[observe]` The `[provider_accounts]` lines from step 3.
  - Step 5 asks for a replacement before removing: `work` is still listed on that screen. After the removal, u3 shows `· account personal`.
  - Step 7 has no `work`.
    - `[observe]` What `[provider_accounts]` now says.
  - `[observe]` Whether `• ufour` appears, and what u4 shows.
    - `[advisory]` If an error appears, it names `work` and gives a recovery.
  - `[observe]` The survey notes.
  - **Relay:**
    - `uone` → `zwork`.
    - `utwo` → `zwork`.
    - `uthree` → `zpersonal`.
    - At `ufour`, no request carries `zenv` or `zpersonal`, and none is keyless.
    - `[observe]` Whether a request on `zwork` was made.

### ST-02: Accounts added or removed by another process; "has used" rule; `/status` never stalls (D124, D86, D125)
- **Priority:** Blocker
- **Home:** zaione as prepared (no named accounts).
- **Actions:**
  1. START(zaione) (window A), ASK(dzero), STATUS(d0).
  2. CLIS(zaione, `account add zai extra`, `agms-fake-zextra-29044`). Log the time.
  3. Window A: STATUS(d1), `sleep 5`, STATUS(d2). If d2 is plain: PROVIDERS (no survey), `Escape` to the composer, STATUS(d3).
  4. CLI(zaione, `account remove zai extra`). Window A: STATUS2(d4).
  5. CLIS(zaione, `account add zai extrb`, `agms-fake-zextrb-30117`). Then in window A:
     - PROVIDERS, ROW(Z.AI, extrb), open its actions, choose "Use for this session", `Escape` to the composer. ASK(dtwo).
     - PROVIDERS, ROW(Z.AI, default), open its actions, choose "Use for this session", `Escape` to the composer. ASK(dthree).
  6. CLI(zaione, `account remove zai extrb`). Window A: STATUS2(d5), QUIT, then `agms-tui stop`.
- **Expected:**
  - Steps 2 and 5 exit 0. This is a precondition: if one does not, log it and record the steps that depend on it as "not reached".
  - d0 is plain.
  - `[observe]` d1.
  - d2, or at the latest d3, shows `· account default` and an "Account usage" row.
  - d4-2 is plain: the session never used `extra`.
  - d5-2 shows `· account default` and an "Account usage" row: the session has used a named account.
    - `[observe]` Whether `extrb` is listed there.
  - No STATUS is "late" or "stalled".
  - NO-SECRET(zextra, 29044) and NO-SECRET(zextrb, 30117) hold.
  - `[observe]` The survey note.
  - **Relay:**
    - `dzero` → `zenv`.
    - `dtwo` → `zextrb`.
    - `dthree` → `zenv`.

### ESC-04: Normal onboarding in a fresh home (compared with REF-12/REF-14)
- **Priority:** Blocker (wording differences are `[observe]`)
- **Home:** fresh.
- **Actions:**
  1. `agms-tui --window <new> start --home fresh` (window A), `sleep 10`, `screen e4-1`.
  2. Press `Down` one key at a time, with a `screen` after each (at most 20), until the line with `>` contains "Z.AI API Key". `key Enter`, `sleep 2`, `screen e4-2`.
  3. `key Escape`, `sleep 2`, `screen e4-3`. Return to the Z.AI key screen as in step 2 (Up is allowed).
  4. KEYIN(`agms-fake-zfresh-30517`, e4-4) with Enter, then `sleep 3`, `screen e4-5`. If the key field is not reached, record steps 5–6 as "not reached" and run step 7.
  5. Finish onboarding: at most 10 minutes and 12 screens. Log every key and the path that finished it.
     - a. If the composer (`›`) shows, go to step 6.
     - b. A model choice: move one key at a time to the line containing `glm-5.3-flash`. If there is none, accept the highlighted default and log it. Then `sleep 2`, `key Enter`, `sleep 3`, `screen`. Back to a.
     - c. The Z.AI actions ("Use configured credentials", "Add another account", "Replace with a new API key"): highlight "Use configured credentials", `key Enter`, `sleep 3`, `screen`. Back to a.
     - d. The provider list: do not press Enter on any provider row (that includes Z.AI and Corbanu API).
       - SURVEY it (screens `e4-s1`, `e4-s2` …).
       - Look for a row containing "Done", "Continue", "Finish" or "Start". If one is found: highlight it, `key Enter`, `sleep 3`, `screen`. Back to a.
     - e. If d found no such row: look in the footer and the list text for a key named for finishing, press it once, `sleep 3`, `screen`, and go back to a. If none is named, log "no way to finish found" and record step 6 as "not reached".
     - f. Any other screen: `key Enter` on the highlighted default, `sleep 3`, `screen`. Back to a.
     - Never choose "Add another account", "Replace with a new API key" or another provider.
  6. ASK(frone), STATUS2(fr1), QUIT.
  7. CLI(fresh, `account list`), then `agms-tui stop`.
- **Expected:**
  - e4-1 is the onboarding provider list, and e4-2 is the Z.AI key screen.
    - `[observe]` Every difference from REF-12/REF-14 in headings, option names or order.
  - e4-1 to e4-4 show no account wording: no "account `", "not configured" or `--account`.
  - `[advisory]` e4-3 is back on the provider list.
  - `[observe]` Whether e4-4 masks the key.
  - NO-SECRET(zfresh, 30517) holds in every capture after Enter.
  - `[observe]` The survey of the onboarding list, and step 5's path (Q5).
  - fr1-2 is plain.
  - Step 7 lists no named account (only `default`, or nothing).
  - `[observe]` The model in use.
  - **Relay:** `frone` → `zfresh`.

---

## Open questions

- **Q1 (owner): resume after two accounts.** A conversation ran on `work`, then on `personal` through `resume --account personal`. Which account should a later `corbanu resume <id>` with no flag use: the latest turn's (`personal`, as the first run saw), or the account it was first recorded on? If the latest turn's account is removed while an earlier one still exists, should resume refuse? Amended PR-01 tests only the unambiguous "recorded on `work`" situation. PR-01's pq and GH-02 step 6 record the mixed case, with no pass or fail until this is settled.
- **Q2 (owner): missing `/providers` rows.** If SURVEY shows that the list does not scroll and `work` is not in it, NA-06/07/10 fail as frozen. Is a list capped at 8 rows with no hint a defect of its own?
- **Q3 (coordinator): NA-13.** The name-field timing problem (observation 4) was seen in the OpenAI add. If the G9 captures show that n13-key was not taken in the key field, NA-13 step 4 needs FIELD and KEYIN. I left NA-13 out because run-observations doesn't report it as unreachable, and re-running G9 needs another human-approved `t1` sign-in.
- **Q4 (owner): NA-01 step 3.** It is now refused for the missing pipe, whatever the argument. So it no longer shows whether a key on the command line is refused when a pipe is present. Should a variant with both be added? I did not add one.
- **Q5 (owner): ESC-04.** If no row or key finishes onboarding after the key is saved, does that count as "onboarding broken" (an ESC-04 failure) or only "not reached"? The frozen expectations are left as they were.
- **Q6 (integrator): the first run's Z.AI add.** Check whether the "invalid account name" error repeated the key. If it did, the product put the key there, which is a NO-SECRET failure under the restated rule. A key shown only in the name field the tester typed it into is `[observe]`.
