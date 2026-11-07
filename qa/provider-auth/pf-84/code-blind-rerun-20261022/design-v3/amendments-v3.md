# PF-84 code-blind run — amendments-v3 (pf84-cb1, 2026-10-22)

The frozen design (`design-cases.md`) and amendments-v2 stay in force. This text replaces only D136, NA-05 and NA-06. Every case ID is kept. Each change rests on run-observations-v3.md:
- D136: owner decision 1, and observation 4 (sign-in check);
- NA-05: owner decision 2;
- NA-06: observation 3 (reaching and choosing Rename) and owner decision 4 (action labels).

### Conventions

All conventions of design-cases.md apply, as amended by amendments-v2.md. That includes Windows, CLIS, Recorded accounts, FIELD, KEYIN, NO-SECRET, PROVIDERS, SURVEY and ROW. No convention is changed or added here.

Owner decision 4 renamed two `/providers` actions:
- "Use for this session" is now "Start a new session on this account".
- "Use default for this session" is now "Start a new session on the default account".

Both start a new session.

### Re-run group plan

Each group gets one fresh machine, homes prepared as in tools.md, and about 80 minutes (it is ended at 90). Run the cases in the order listed.

| Group | Home(s) | Cases (text used) | Est. minutes |
|---|---|---|---|
| S1 | oai1 | D136 (this amendment) | 30 |
| S2 | zai3 | PR-01 (amendments-v2, unchanged), GH-02 (amendments-v2, unchanged), NA-05 (this amendment) | 65 |
| S3 | zai3 | NA-06 (this amendment) | 35 |

- S1 is the only group that uses oai1. D136 is its only case, and step 2 is the only removal of `t1`.
- In S2, PR-01 and GH-02 set up NA-05's starting state: `work` removed, `zai = "ghost"`, and `<gid>` recorded only on `work`. Run them exactly as written in amendments-v2.
- No other case is re-run.

---

### D136: Removing `t1` from another terminal while a session runs on it
- **Priority:** Blocker
- **Home:** oai1 as prepared. The coordinator signed in `t1` before the tester started, possibly hours earlier. D136 is the only case of its group. The removal in step 2 is the only removal of `t1`; do not repeat it.
- **Actions:**
  0. Sign-in check: `ls "$AGMS_RUN/evidence/"`, and save the output.
     - If `t1-not-signed-in` is listed: log "t1 not signed in", record every result of this case as "not reached", run `agms-tui stop`, and end the group.
     - If the folder does not exist, log it and continue.
  1. START(oai1, `--account t1`) (window A), ASK(beforeone), STATUS(x0).
     - If `• beforeone` did not appear, or x0 does not show `· account t1`:
       - log "precondition failed";
       - QUIT, then CLI(oai1, `account list`), then `agms-tui stop`;
       - do not remove `t1`;
       - record steps 2–8 as "not reached".
  2. CLI(oai1, `account remove openai t1`). Log the time.
     - If it exits non-zero and points to `logout --account t1`, run CLI(oai1, `logout --account t1`) and log which command worked.
     - If neither removes it, log "not removed" and skip to step 7.
  3. CLI(oai1, `account list`).
  4. Window A: log the time (start of the no-use window), `sleep 10`, STATUS2(x1).
  5. Window A: SEND(afterone, 90), SEND(aftertwo, 90).
  6. Window A: STATUS(x3).
  7. Window A: PROVIDERS, open OpenAI, `screen x-prov`, `Escape` to the composer, QUIT. Log the time (end of the no-use window).
  8. CLI(oai1, `account list`), then `agms-tui stop`.
- **Expected:**
  - Precondition: step 1's `• beforeone` appears and x0 shows `· account t1`. If not, the case is "not reached", not failed.
  - Step 3 shows no `t1`.
  - Window A never shows a sign-in or onboarding screen (NO-ONBOARD).
  - Owner decision 1: the session never uses `t1` again.
    - No `• afterone` and no `• aftertwo` appears. Both requests are refused.
    - At the latest on the screen at the end of SEND(afterone), window A shows text that:
      - names `t1` as having no sign-in, or as removed or signed out;
      - gives the recovery `corbanu login --account t1`;
      - mentions another account in `/providers` and `--account`.
    - `[advisory]` The wording is the documented "account `t1` has no sign-in (it may have been removed or signed out)".
    - `[observe]` Whether the text is shown again at `aftertwo`.
  - x3 (owner decision 1; x3 is the third `/status` after the removal, so D124's allowance is already used by x1):
    - Its provider line shows the account as unavailable: the line contains "unavailable".
      - `[advisory]` The form is `account unavailable`, as documented.
    - It never shows `· account t1` as healthy (without "unavailable"), `· account default`, or any other account as the session's account.
    - The `Account:` row (with any continuation lines) gives the recovery, including `corbanu login --account t1`.
    - "Account usage" still lists `t1` with non-zero counts (from `beforeone`).
  - `[observe]` x1-1 and x1-2. They come before the next request, and D124 allows them to be stale.
  - x-prov has no `t1` row, and step 8 has no `t1`. Nothing re-created the account.
  - **Relay:**
    - chatgpt.com traffic at `beforeone` (precondition).
    - No api.openai.com request carrying `odefault` from step 1 to the end of the group.
    - `[advisory]` No chatgpt.com traffic between step 4's and step 7's logged times (the no-use window).
      - The relay cannot tell whether such traffic carried the sign-in. Any traffic there is reported with its times and sizes, compared with `beforeone`'s.
      - The blocker evidence is that no reply appears.
  - **Files:**
    - The `auth.json` hash is unchanged.
    - `config.toml` gained no `openai = "t1"` entry.
    - `[observe]` The vault file names, and the account window A's conversation records for each turn.

### NA-05: In-session `/resume` of a conversation whose account is gone, in a TUI started with `--account personal`
- **Priority:** Blocker
- **Home:** zai3, right after GH-02 (amendments-v2): `work` removed; `zai = "ghost"`; `<gid>` recorded only on `work` (turns `gzero` and `gaone`). If GH-02 step 4 did not exit 0, or `<gid>` was not logged, record NA-05 as "not reached" and run only `agms-tui stop`.
- **Actions:**
  1. START(zai3, `--account personal`), ASK(rone), STATUS(r1). Log SID1.
  2. `text '/resume'`, `key Enter`, `sleep 3`, `screen`. Then `text 'gzero'`, `sleep 2`, `screen`.
  3. If the highlighted line contains `gzero`, `key Enter`. Otherwise press Up/Down, with a `screen` after each, until it does, then `key Enter`.
     - If no line contains it: log "not found", press `Escape` until the composer shows, and skip step 4.
  4. `sleep 5`, `screen r-msg`.
  5. STATUS(r2), ASK(rtwo), QUIT, then `agms-tui stop`.
- **Expected:**
  - Precondition: r1 shows `· account personal`.
  - Owner decision 2: the TUI was started with `--account personal`, so the conversation opens on `personal`.
    - r-msg shows one notice that names zai account `work` (as no longer available) and `personal` (as the account it resumes on). The notice may wrap across screen rows.
      - `[advisory]` The wording is as in decision 2's example: "This conversation ran on zai account `work`, which is no longer available; it resumes on `personal`, the account selected for this session (with `--account` or in `/providers`)."
    - The resume is not refused: r-msg shows no "resumes on it: account `work` … is not configured" error (the REF-10 text).
    - r2 shows `<gid>` (not SID1) and `· account personal`.
    - `• rtwo` appears.
  - `[observe]` Whether the earlier turns (`gzero`, `gaone`) are visible in r-msg or r2 (read from the captures only; no extra keys).
  - `[observe]` r2's "Account usage" (whether `work`'s earlier counts are listed).
  - NO-ONBOARD holds.
  - **Relay:**
    - `rtwo` → `zpersonal`.
    - Nothing in this case carries `zenv` or `zwork`, and nothing is keyless.
  - **Files:**
    - `<gid>` records provider `zai`, account `personal` for the `rtwo` turn. This is the "otherwise" of the Recorded accounts convention.
    - `<gid>` still records `work` for `gzero` and `gaone`.

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
       - a choice of kind: the line containing "API key", then `sleep 2`, `key Enter`, `sleep 2`, `screen`;
       - a key field: KEYIN(`agms-fake-zadded-40958`, n6-key) with Enter, then `screen n6-after`;
       - anything else: `screen`, and log it.
     - Fallback: if after FIELD the name field is still shown, or KEYIN logs "key field not reached":
       - press `Escape` until the composer shows;
       - skip step 3, and run steps 4–7.
  3. Rename it:
     - Open the actions:
       - If the `/providers` list is not shown, PROVIDERS.
       - ROW(Z.AI, added), `screen n6-added`.
       - Open its actions: `Enter` (or the key the footer hints), `sleep 2`, `screen n6-actions`.
     - Find Rename. Do not press Enter until part d below says so.
       - a. In n6-actions, log the highlighted line and the text of every visible action line.
       - b. If the highlighted line contains "Rename", go to d.
       - c. Press `Down` one key at a time, with a `screen` after each, until the highlighted line contains "Rename".
         - Stop pressing `Down` when the highlight stops moving (two identical screens in a row), comes back to a line already highlighted, or after 15 presses.
         - Then press `Up` the same way (at most 15 presses), until the highlighted line contains "Rename".
         - If the action list looked cut off (rows leaving the top or bottom), SURVEY it as well.
       - d. `screen n6-rename-hl`. Check that the highlighted line contains "Rename" and does not contain "Start a new session", "Make default" or "Remove". Only then: `sleep 2`, `key Enter`, `sleep 2`, `screen n6-rfield`.
       - e. If no line contains "Rename":
         - If the footer or the action list names a key for renaming (for example `r rename`), press that key once, log it, then `sleep 2`, `screen n6-rfield`, and continue with the n6-rfield check below.
         - Otherwise log "Rename not found" with the screens, press `Escape` until the composer shows, and skip the rest of step 3.
     - Never press Enter on a line containing "Start a new session on this account". That action starts a new session on `added` and is not the action under test.
     - Check n6-rfield. It must show a field for the account's new name (for example a prompt containing "name", or an input holding `added`), and not only the composer.
       - If it does not: log "rename field not shown", type nothing, press `Escape` until the composer shows, and skip the rest of step 3.
     - Clear the field: `key C-u`, `screen`. If text remains, press `BSpace` until it is empty (at most 12), then `screen`.
     - FIELD(`renamed`). If FIELD logs "field not shown", type nothing more, press `Escape` until the composer shows, and skip the rest of step 3.
     - If the list is not shown, PROVIDERS. Then ROW(Z.AI, renamed), `screen n6-renamed`.
  4. Press `Escape` until the composer shows, then QUIT.
  5. CLI(zai3, `account list`).
  6. CLI(zai3, `exec --account renamed "reply with the single word nren"`).
  7. CLI(zai3, `exec --account added "reply with the single word nold"`), then `agms-tui stop`.
- **Expected:**
  - Rows `work` and `personal`, where they are visible (n6-work, n6-personal, n6-zai or the survey screens):
    - each shows a kind and a fingerprint;
    - `personal` carries both the current-session marker and the default marker;
    - `[observe]` The `default` row's contents, and the survey note;
    - `[advisory]` Marker wording.
  - n6-after (or n6-added) shows an `added` row with a kind and a fingerprint, and n6-renamed shows it as `renamed`.
  - Fail rules:
    - If the step 2 fallback was taken, the add and rename results fail (no account was added through `/providers`).
    - If "Rename not found" was logged and every action line was seen (the list fit on screen or a SURVEY was complete), the rename results fail. `/providers` must offer Rename (AC1).
    - If "rename field not shown" was logged, the rename results fail.
    - In each of these, the integrator may reclassify the failure as a tester error from the screens.
  - `[advisory]` n6-actions offers "Start a new session on this account", not "Use for this session" (owner decision 4).
  - `[observe]` Which row carries the current-session marker in n6-added (whether the add moved the session to `added`).
  - NO-SECRET(zadded, 40958) holds in every capture.
    - `[observe]` Whether n6-key shows the typed key or a mask.
  - Step 5 lists `renamed` and not `added`.
  - Step 6 exits 0 and prints `nren`.
  - Step 7 exits non-zero with the not-configured text for `added`.
  - **Relay:**
    - `nren` → `zadded`.
    - Nothing at `nold`.

