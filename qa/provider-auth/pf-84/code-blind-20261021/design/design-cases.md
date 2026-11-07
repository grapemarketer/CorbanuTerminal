# PF-84 code-blind acceptance design (pf84-cb1, 2026-10-21) — FROZEN

Frozen word for word from the independent reviewer's revised design (section (b) of design/review-output.md, VERDICT: CHANGES). Designer: fresh read-only Opus 5.5 High session; reviewer: a separate fresh read-only Opus 5.5 High session. No integrator edits.


## Conventions used in every case

**Tags.**
- In a **Blocker** case, an untagged expected line is a blocker check. In an **Advisory** case, untagged lines are advisory.
- `[advisory]` means a failure is reported but does not block. Wording issues #1205 and #1208 are never blockers (D138); ignore any "legacy /spawn hierarchy" note on resume.
- `[observe]` means the packet does not settle the result. The tester records exactly what happens and there is no pass or fail.
- A step whose starting state is not there is recorded as "not reached", with screens.

**Log.** Write in the case notes. Every time logged comes from `date -u +%H:%M:%S`.

**Windows.**
- Every START, CLI, CLIK and REFUSE opens a **new** window. Name windows `w1`, `w2`, `w3` … in order for the whole group and never reuse a name.
- Every later command for that program is `agms-tui --window <its name> …`.
- "Window A" or "window B" in a case is a nickname for the window that the case's START opened. Log the mapping, for example "A = w4".
- Every case ends with `agms-tui stop`.

**CFG(H)** is `"$AGMS_RUN/homes/H/config.toml"`.
- Before any edit, `cat CFG(H)`. After it, `cat CFG(H)` again and save the output.
- If the intended change is not visible, log "edit failed" and skip the steps that depend on it.

**Reading `/status`.** "The provider line" is the `Model provider:` row.
- "shows `· account X`" means that row ends with `· account X`.
- **Plain** means the row has no `· account` text and there is no `Account usage:` row.

**Steps**

- **START(H, args):**
  1. `agms-tui --window <new> start --home H args`.
  2. `agms-tui --window <it> wait 'glm-5.3-flash' 60` (in `oai1`: `wait 'gpt-5.6-luna' 60`).
  3. `screen`.
  4. If the wait times out, take a screen, log "no session", skip the steps that need that session, and continue with the case's other steps.
- **ASK(w):**
  1. Log "w sent".
  2. `text 'reply with the single word w'`, then `key Enter`.
  3. `wait '(?i)• w' 90`.
  4. Log "w done", then `screen`.
  5. If the wait times out: `screen`, and log "no reply to w".
- **SEND(w, s):**
  1. Log "w sent".
  2. `text 'reply with the single word w'`, then `key Enter`.
  3. `sleep s`, then `screen`.
  4. Log "w end".
- **STATUS(l):**
  1. `text '/status'`, `key Enter`, `sleep 3`, `screen l`.
  2. The newest status box (below the last `/status` line on screen) must be complete, with a `Session:` row.
  3. If a command list still shows `/status` highlighted and no box, press `key Enter` once and log it.
  4. If there is no complete newest box: `sleep 5`, `screen l-late`, and log "late".
  5. If there is still none, log "stalled".
- **STATUS2(l):** STATUS(l-1), `sleep 5`, STATUS(l-2). Judge l-2: D124 allows the first `/status` after a change to be stale.
- **PROVIDERS:**
  1. `text '/providers'`, `key Enter`, `sleep 2`, `screen`.
  2. Move with `Up`/`Down`, open with `Enter`, go back with `Escape`, and take a `screen` after every key.
  3. To "open the Z.AI entry" (or OpenAI), press `Down` until the highlighted line contains "Z.AI" (or "OpenAI"), then `Enter`.
  4. Spend at most 4 minutes looking for a named action. If it is not found, log "not found" with the screens.
  5. When done, press `Escape` until the composer (`›`) shows.
- **QUIT:**
  1. `text '/quit'`, `key Enter`, `wait 'product exited' 10`.
  2. If it has not exited: `key C-c`, `key C-c`, `wait 'product exited' 10`.
  3. `screen`, and log the exit code and the `[agms] terminal settings left:` line.
- **CLI(H, cmd):**
  1. `agms-tui --window <new> start --home H cmd`.
  2. `wait 'product exited' T`, where T is 120 for `exec` and 30 otherwise. Then `screen`, and log the exit code.
  3. If it has not exited and the screen asks a question:
     - If the question confirms the action the case asked for, `text 'y'`, `key Enter`, and wait again.
     - Otherwise `key C-c`, and log "stuck: <question>".
  4. If it has not exited and shows no question: `key C-d`, `wait 'product exited' 10`, and log it.
- **CLIK(H, cmd, k)** (a key typed on stdin):
  1. `agms-tui --window <new> start --home H cmd`, `sleep 3`, `screen`.
  2. `text 'k'`, `key Enter`, `wait 'product exited' 10`.
  3. If it has not exited: `key C-d`, `wait 'product exited' 10`.
  4. `screen`, and log the exit code.
  5. `[observe]` Whether the typed key is visible on the final screen.
- **REFUSE(H, args)** (a start that should be refused):
  1. `agms-tui --window <new> start --home H args`.
  2. `sleep 2`, `screen <l>-2s`.
  3. `wait 'product exited' 30`, `screen <l>`.
  4. If it has not exited: `screen`, `key Escape`, `screen`, `key C-c`, `key C-c`, `wait 'product exited' 10`, `screen`.
  5. Log the exit code, the terminal-settings line and the time.
- **SID:** the `Session:` value from `/status`.
- **NO-ONBOARD:** no capture contains "Welcome to Corbanu Terminal", "Choose a provider account", "Paste or type your API key", "Use your Z.AI API key" or "device code" (any case).
- **NO-SECRET(label, digits):**
  - No capture shows `agms-fake`, the key's label or its 5-digit number.
  - The one exception is `[observe]`: the tester's own typed text, on the line or field where it was typed before pressing Enter.
  - Account names, fingerprints and email hints are not secrets.
- **Relay:** and **Files:** lines are checked by the integrator afterwards. The tester only logs times. "Nothing at w" means no request between w's logged start and end times.

## Group plan (one fresh machine each, in this order)

| Group | Home(s) | Cases | Est. minutes |
|---|---|---|---|
| G1 | zai3 | NA-01, NA-02, NA-03, NA-04 | 45 |
| G2 | zai3 | PR-01, GH-02, NA-05 | 45 |
| G3 | zai3 | NA-06, ESC-03, NA-07 | 55 |
| G4 | zai3 | NA-08, NA-09, NA-10 | 55 |
| G5 | zaione, fresh | ST-01, ST-02, ESC-04 | 55 |
| G6 | zainokey | NA-11, ESC-01, I426-A | 25 |
| G7 | custom | NA-12, I426-B | 25 |
| G8 | zai3off, zainokeyoff | REG-01a, REG-01b, REG-01c, REG-01d | 50 |
| G9 | oai1 | NA-13, D136, ST-03 | 50 |

In G9, D136 is the only removal of `t1` and the last action that involves `t1`. ST-03 follows it only because it needs a home with no named account. It neither uses nor restores `t1` (see open question Q8).

---

### NA-01: CLI list/add/remove; keys only from stdin; no secrets (AC1, AC3, AC6)
- **Priority:** Blocker
- **Home:** zai3 as prepared; no TUI running.
- **Actions:**
  1. CLI(zai3, `account list`).
  2. CLIK(zai3, `account add zai extra`, `agms-fake-zextra-58213`).
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

### NA-02: Choosing the account for `exec` and the TUI (AC2)
- **Priority:** Blocker
- **Home:** zai3 (`[provider_accounts] zai = "personal"`).
- **Actions:**
  1. CLI(zai3, `exec --account work "reply with the single word eone"`).
  2. CLI(zai3, `exec --account zai:work "reply with the single word etwo"`).
  3. CLI(zai3, `exec "reply with the single word ethree"`).
  4. CLI(zai3, `exec --account default "reply with the single word efour"`).
  5. START(zai3, `--account zai:work`), STATUS(n2-a), ASK(efive), QUIT.
  6. Log the start and end times of every step, then `agms-tui stop`.
- **Expected:**
  - Steps 1–4 each exit 0 and print their word.
  - n2-a shows `· account work`.
  - **Relay:**
    - `eone` → `zwork`.
    - `etwo` → `zwork`.
    - `ethree` → `zpersonal`.
    - `efour` → `zenv`.
    - `efive` → `zwork`.

### NA-03: A missing account fails closed and says how to recover (AC3, AC2 precedence)
- **Priority:** Blocker
- **Home:** zai3.
- **Actions:**
  1. REFUSE(zai3, `--account ghost`) (label n3-1).
  2. CLI(zai3, `exec --account ghost "reply with the single word gone"`).
  3. CLI(zai3, `exec --account zai:ghost "reply with the single word gtwo"`).
  4. Edit the config: `sed -i '' -E 's/^(zai[[:space:]]*=[[:space:]]*)"personal"/\1"ghost"/' CFG(zai3)`.
  5. REFUSE(zai3, ``) — no arguments; a new TUI session (label n3-5).
  6. CLI(zai3, `exec "reply with the single word gthree"`).
  7. START(zai3, `--account work`), ASK(gfour), STATUS(n3-a), QUIT.
  8. Change it back: `sed -i '' -E 's/^(zai[[:space:]]*=[[:space:]]*)"ghost"/\1"personal"/' CFG(zai3)`, then `cat`.
  9. Log every step's times, then `agms-tui stop`.
- **Expected:**
  - Steps 1, 2, 3, 5 and 6 each exit non-zero. Each one's text:
    - names `ghost` and provider `zai`;
    - gives `corbanu account add zai ghost`;
    - mentions `--account`.
    - `[advisory]` Wording differences from REF-13 are allowed.
  - No reply word is printed, and NO-ONBOARD holds in every capture, including the 2 s screens.
  - Steps 1 and 5 show no resume picker and no session.
  - In step 7, n3-a shows `· account work`.
  - **Relay:**
    - Nothing at `gone`, `gtwo`, `gthree`, or during steps 1 and 5. That includes no keyless request and none on `zenv` or `zpersonal`.
    - `gfour` → `zwork`.

### NA-04: A rejected key names the account; no fallback (AC3, AC6)
- **Priority:** Blocker
- **Home:** zai3.
- **Actions:**
  1. CLIK(zai3, `account add zai badone`, `agms-fake-badkey-77302`).
  2. CLI(zai3, `exec --account badone "reply with the single word bone"`). Log the times.
  3. START(zai3, `--account badone`).
  4. SEND(btwo, 60).
  5. STATUS(n4-a), QUIT, then `cat CFG(zai3)`, then `agms-tui stop`.
- **Expected:**
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

### PR-01: Full precedence order, confirmed by `/status` and the relay at every step
- **Priority:** Blocker
- **Home:** zai3 (`[provider_accounts] zai = "personal"`).
- **Actions:**
  1. Setup: START(zai3, `--account work`), ASK(pzero), STATUS(p0). Log the SID as `<id>`. QUIT.
  2. START(zai3, `resume --account personal <id>`), STATUS(p1-pre), ASK(pone), STATUS(p1), QUIT.
  3. START(zai3, `resume <id>`), STATUS(p2-pre), ASK(ptwo), STATUS(p2), QUIT.
  4. START(zai3), STATUS(p3-pre), ASK(pthree), STATUS(p3), QUIT.
  5. Remove the section: `sed -i '' -E '/^\[provider_accounts\]/,/^zai[[:space:]]*=/d' CFG(zai3)`, then `cat` (there must be no `provider_accounts`).
  6. START(zai3), STATUS(p4-pre), ASK(pfour), STATUS(p4), QUIT, then `agms-tui stop`.
- **Expected:**
  - The provider line reads, in order:
    - `· account work` in p0;
    - `· account personal` in p1;
    - `· account work` in p2;
    - `· account personal` in p3;
    - `· account default` in p4 (the home still has named accounts).
    - The `-pre` captures show the same accounts.
  - In p1 and p2, "Account usage" is a table:
    - the session's account is marked `(active)`;
    - the conversation's other account appears with its earlier counts.
    - `[observe]` p1-pre and p2-pre.
  - **Relay:**
    - `pzero` → `zwork`.
    - `pone` → `zpersonal`.
    - `ptwo` → `zwork`.
    - `pthree` → `zpersonal`.
    - `pfour` → `zenv`.
  - **Files:**
    - `<id>` records provider `zai` and account `work` for its first turn.
    - `[observe]` What it records for the later resumes.

### GH-02: The recorded account was removed while `[provider_accounts]` names a missing one
- **Priority:** Blocker (only the mention of `ghost` is advisory)
- **Home:** zai3, right after PR-01. There is no `[provider_accounts]` section, and `<id>` is recorded on `work`.
- **Actions:**
  1. `printf '\n[provider_accounts]\nzai = "ghost"\n' >> CFG(zai3)`, then `cat`.
  2. START(zai3, `resume <id>`), STATUS(g1), ASK(gaone), QUIT.
  3. CLI(zai3, `account remove zai work`). Log the time.
  4. REFUSE(zai3, `resume <id>`) (label g4). Log the time, then `agms-tui stop`.
- **Expected:**
  - In step 2, g1 shows `· account work`, as in REF-05.
  - Step 3 exits 0.
  - Step 4 exits non-zero. Its text says "This conversation ran on zai account `work`" and gives `corbanu account add zai work`. NO-ONBOARD holds, and there is no picker or session.
    - `[advisory]` The text also mentions `ghost`.
  - **Relay:**
    - `gaone` → `zwork`.
    - Nothing during step 4.

### NA-05: In-session `/resume` of a conversation whose account is gone (as in REF-10)
- **Priority:** Blocker
- **Home:** zai3, right after GH-02 (`work` removed, `zai = "ghost"`).
- **Actions:**
  1. START(zai3, `--account personal`), ASK(rone), STATUS(r1). Log SID1.
  2. `text '/resume'`, `key Enter`, `sleep 3`, `screen`. `text 'pzero'`, `sleep 2`, `screen`.
  3. If the highlighted line contains `pzero`, `key Enter`; otherwise press Up/Down until it does, then `key Enter`.
  4. `sleep 5`, `screen r-msg`.
  5. STATUS(r2), ASK(rtwo), QUIT, then `agms-tui stop`.
- **Expected:**
  - r-msg shows a message that the conversation ran on zai account `work`, which is not configured. The recovery mentions adding it or picking another account in `/providers`.
  - The session stays put: r2 shows SID1 and `· account personal`, and the transcript still shows `rone`.
  - NO-ONBOARD holds.
  - **Relay:** `rtwo` → `zpersonal`. Nothing in this case carries `zenv`.

### NA-06: `/providers` rows, adding with a key, rename (AC1, AC6)
- **Priority:** Blocker
- **Home:** zai3.
- **Actions:**
  1. START(zai3). PROVIDERS, `screen n6-list`. Open the Z.AI entry, `screen n6-zai`.
  2. Choose the action whose label contains "Add" (for example "Add another account"), then `key Enter`, `screen`.
     - When asked for a name: `text 'added'`, `key Enter`, `screen`.
     - When asked for a key: `text 'agms-fake-zadded-40958'`, `screen n6-key`, `key Enter`, `sleep 2`, `screen n6-after`.
  3. Move to the `added` row and open its actions (`Enter`, or the key the screen hints).
     - Choose the action containing "Rename", `screen`.
     - Clear the name with `key C-u` (then `BSpace` until empty, if needed).
     - `text 'renamed'`, `key Enter`, `sleep 2`, `screen n6-renamed`.
  4. Press `Escape` until the composer shows, then QUIT.
  5. CLI(zai3, `account list`).
  6. CLI(zai3, `exec --account renamed "reply with the single word nren"`).
  7. CLI(zai3, `exec --account added "reply with the single word nold"`), then `agms-tui stop`.
- **Expected:**
  - In n6-zai, rows `work` and `personal` each show a kind and a fingerprint.
    - `personal` carries both the current-session marker and the default marker.
    - `[observe]` The `default` row's contents.
    - `[advisory]` Marker wording.
  - n6-after shows an `added` row with a kind and a fingerprint, and n6-renamed shows it as `renamed`.
  - NO-SECRET(zadded, 40958) holds in every capture after the key was entered.
    - `[observe]` Whether n6-key shows the typed key or a mask.
  - Step 5 lists `renamed` and not `added`.
  - Step 6 exits 0 and prints `nren`.
  - Step 7 exits non-zero with the not-configured text for `added`.
  - **Relay:**
    - `nren` → `zadded`.
    - Nothing at `nold`.

### ESC-03: Esc in the in-session `/resume` picker keeps the session, account, transcript and draft
- **Priority:** Blocker (the draft clause is `[observe]` unless route 1 exists)
- **Home:** zai3, after NA-06.
- **Actions:**
  1. Setup: START(zai3, `--account personal`), ASK(cprev), QUIT.
  2. START(zai3, `--account personal`), ASK(cone), STATUS(c1). Log SIDc.
  3. Route search (at most 5 minutes): with the composer empty, `text '?'`, `screen`, `key BSpace`, `screen`. Look for a shortcut that opens the resume picker, or keeps the composer text while a command runs.
  4. **Route 1** (step 3 found a shortcut that opens the resume picker): `text 'keep draft 314'`, use that shortcut, `sleep 2`, `screen`.
  5. **Route 2** (otherwise):
     - `text 'keep draft 314'`, `key C-a`, `text '/resume '`, `screen`.
     - `key Enter`, `sleep 2`, `screen`.
  6. If "Resume a previous session" is on screen: `key Escape`, `sleep 2`, `screen c-after`. Log which route reached the picker.
  7. If no route reached the picker, log "no route":
     - Clear the composer (`key C-u`, then `BSpace` until empty).
     - `text '/resume'`, `key Enter`, `sleep 3`, `screen`.
     - `key Escape`, `sleep 2`, `screen c-after`.
  8. STATUS(c2), QUIT, then `agms-tui stop`.
- **Expected:**
  - After Escape, you are back in the same session:
    - c2 shows SIDc and `· account personal`;
    - the transcript still shows `cone`;
    - no "Thread forked" or new-session banner appears;
    - NO-ONBOARD holds.
  - If route 1 was used, c-after's composer still holds `keep draft 314`.
  - `[observe]` With route 2, what c-after's composer holds. If there is no route, log "no route" (owner question Q2).

### NA-07: Use for this session, make default, and removing the account in use (AC1, AC2, AC3)
- **Priority:** Blocker
- **Home:** zai3, after ESC-03.
- **Actions:**
  1. START(zai3) (window A), then STATUS(u0).
  2. Window A: PROVIDERS, open Z.AI, select `work`, choose the action like "Use for this session", then `Escape` to the composer. STATUS(u1), ASK(uone).
  3. Window A: PROVIDERS, open Z.AI, select `work`, choose the action like "Make default", then `Escape` to the composer. Run `grep -n -A2 'provider_accounts' CFG(zai3)` and save the output.
  4. START(zai3) (window B), STATUS(u2), ASK(utwo).
  5. Window A: PROVIDERS, open Z.AI, select `work`, choose the action like "Remove", taking a `screen` at every prompt.
     - If asked for a replacement, choose `personal` and confirm.
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
  - **Relay:**
    - `uone` → `zwork`.
    - `utwo` → `zwork`.
    - `uthree` → `zpersonal`.
    - At `ufour`, no request carries `zenv` or `zpersonal`, and none is keyless.
    - `[observe]` Whether a request on `zwork` was made.

### NA-08: `/fork`, editing an earlier prompt and `/side` keep the account (AC4, AC5)
- **Priority:** Blocker
- **Home:** zai3 (`[provider_accounts] zai = "personal"`).
- **Actions:**
  1. START(zai3, `--account work`), ASK(fone), STATUS(f0). Log SID0.
  2. `text '/fork'`, `key Enter`, `sleep 3`, `screen` (look for "Thread forked from"). ASK(ftwo), STATUS(f1).
  3. Edit an earlier prompt, with the composer empty:
     - `key Escape`, `screen`, `key Escape`, `screen`.
     - If a message is highlighted, press Up/Down until it is the `ftwo` prompt, then `key Enter`, `screen`. The composer should now hold that prompt.
     - `key C-u`, `screen`; if text remains, press `BSpace` until it is empty, then `screen`.
     - ASK(fthree), STATUS(f2).
     - If no edit mode appears within 3 minutes, log "edit not reached", press `Escape` until the composer shows, and continue.
  4. `text '/side'`, `key Enter`, `sleep 3`, `screen` (look for "Side from main thread"). ASK(fside), STATUS(f3), `key C-c`, `sleep 2`, `screen`.
  5. QUIT, then `agms-tui stop`.
- **Expected:**
  - f1 shows "Forked from" SID0 and `· account work`.
  - f2 and f3 show `· account work`.
  - In f1–f3, "Account usage" has a `work` row with non-zero counts and no other account with non-zero counts (the REF-11 defect).
  - **Relay:** `fone`, `ftwo`, `fthree` and `fside` all carry `zwork`.
  - **Files:** the fork conversations record provider `zai`, account `work`.

### NA-09: `corbanu fork <id>` and spawned agents keep the source's account (AC4)
- **Priority:** Blocker (the spawn leg applies only if spawning is available)
- **Home:** zai3, after NA-08 (SID0 is on `work`).
- **Actions:**
  1. `agms-tui --window <new> start --home zai3 fork SID0`, `sleep 10`, `screen`.
     - If a picker shows, highlight the newest item, `key Enter`.
     - `wait 'glm-5.3-flash' 60`, STATUS(k1), ASK(kone).
  2. Ask for a spawn:
     - Log "spawn sent". `text 'Use your sub-agent tool to start one agent whose only task is to reply with the single word kidword. When it finishes, tell me what it replied.'`, `key Enter`.
     - Repeat up to 8 times: `sleep 30`, `screen`. Stop when the screen no longer shows "esc to interrupt" (or "Working").
     - Log "spawn end", `screen k-spawn`, then QUIT.
  3. START(zai3, `fork --account personal SID0`); handle a picker as in step 1. STATUS(k2), ASK(ktwo), QUIT, then `agms-tui stop`.
- **Expected:**
  - k1 shows `· account work`, and k2 shows `· account personal`.
  - `[observe]` Whether an agent was started, or the screen says spawning is not available.
  - If an agent was started, k-spawn's final answer contains `kidword`.
  - **Relay:**
    - `kone` → `zwork`.
    - If an agent was started, every Z.AI request between "spawn sent" and "spawn end" carries `zwork`.
    - `ktwo` → `zpersonal`.
  - **Files:** if an agent was started, its conversation records `zai`/`work`.

### NA-10: `/status` format with named accounts (AC5, D86)
- **Priority:** Blocker (exact layout is advisory)
- **Home:** zai3.
- **Actions:**
  1. START(zai3), STATUS2(s0), ASK(sone), STATUS(s1).
  2. PROVIDERS, open Z.AI, select `work`, choose "Use for this session", `Escape` to the composer. ASK(stwo), STATUS(s2).
  3. QUIT, then `agms-tui stop`.
- **Expected:**
  - s0-2 shows `· account personal`, and its "Account usage" contains `no usage yet`.
  - s1's "Account usage" is one `personal` row with input and output counts.
    - `[advisory]` The form is `personal · N input · N cached input · N output (N reasoning)`.
  - s2 shows `· account work`, and "Account usage" is a table with `work (active)` and `personal`.
  - No capture shows `$`, "cost" or "price".
  - The "Account:" row, if present, shows no `agms-fake`.
  - `[observe]` The footer line in each capture (for example `via zai default` while on another account).
  - **Relay:** `sone` → `zpersonal`, and `stwo` → `zwork`.

### ST-01: A home with no named accounts looks as it does with the feature off (D86, D125, D124)
- **Priority:** Blocker
- **Home:** zaione.
- **Actions:**
  1. START(zaione), STATUS(z0), ASK(zone), STATUS2(z1).
  2. PROVIDERS, `screen`, `Escape` to the composer.
  3. STATUS2(z2), QUIT.
  4. CLI(zaione, `exec "reply with the single word zexec"`), then `agms-tui stop`.
- **Expected:**
  - z1-2 and z2-2 are plain. The provider line is exactly `Z.AI - https://api.z.ai/api/paas/v4`.
  - `[observe]` z0 (D124 allows the account to be shown until it is known).
  - No STATUS is "late" or "stalled".
  - `[advisory]` The integrator compares z2-2's set and order of rows with REG-01a's o1-2.
  - **Relay:** `zone` and `zexec` carry `zenv`.

### ST-02: Accounts added or removed by another process; "has used" rule; `/status` never stalls (D124, D86, D125)
- **Priority:** Blocker
- **Home:** zaione, after ST-01.
- **Actions:**
  1. START(zaione) (window A), ASK(dzero), STATUS(d0).
  2. CLIK(zaione, `account add zai extra`, `agms-fake-zextra-29044`). Log the time.
  3. Window A: STATUS(d1), `sleep 5`, STATUS(d2). If d2 is plain: PROVIDERS, `Escape` to the composer, STATUS(d3).
  4. CLI(zaione, `account remove zai extra`). Window A: STATUS2(d4).
  5. CLIK(zaione, `account add zai extrb`, `agms-fake-zextrb-30117`). Then in window A:
     - PROVIDERS, open Z.AI, select `extrb`, "Use for this session", `Escape` to the composer. ASK(dtwo).
     - PROVIDERS, open Z.AI, select `default`, "Use for this session", `Escape` to the composer. ASK(dthree).
  6. CLI(zaione, `account remove zai extrb`). Window A: STATUS2(d5), QUIT, then `agms-tui stop`.
- **Expected:**
  - d0 is plain.
  - `[observe]` d1.
  - d2, or at the latest d3, shows `· account default` and an "Account usage" row.
  - d4-2 is plain: the session never used `extra`.
  - d5-2 shows `· account default` and an "Account usage" row: the session has used a named account.
    - `[observe]` Whether `extrb` is listed there.
  - No STATUS is "late" or "stalled".
  - NO-SECRET(zextra, 29044) and NO-SECRET(zextrb, 30117) hold.
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
  3. `key Escape`, `sleep 2`, `screen e4-3`. Return to the Z.AI key screen as in step 2.
  4. `text 'agms-fake-zfresh-30517'`, `screen e4-4`, `key Enter`, `sleep 3`, `screen`.
  5. For each further screen (at most 6), take a `screen`:
     - If a model choice is offered, pick the line containing `glm-5.3-flash`; if there is none, accept the highlighted default and log it.
     - Otherwise press `Enter` on the highlighted default.
     - Stop when the composer (`›`) shows.
  6. ASK(frone), STATUS2(fr1), QUIT.
  7. CLI(fresh, `account list`), then `agms-tui stop`.
- **Expected:**
  - e4-1 is the onboarding provider list, and e4-2 the Z.AI key screen.
    - `[observe]` Every difference from REF-12/REF-14 in headings, option names or order.
  - e4-1 to e4-4 show no account wording: no "account `", "not configured" or `--account`.
  - `[advisory]` e4-3 is back on the provider list.
  - `[observe]` Whether e4-4 masks the key.
  - NO-SECRET(zfresh, 30517) holds in every capture after Enter.
  - fr1-2 is plain.
  - Step 7 lists no named account (only `default`, or nothing).
  - `[observe]` The model in use.
  - **Relay:** `frone` → `zfresh`.

### NA-11: No default credentials; the named account works without onboarding (AC2, AC3, AC4)
- **Priority:** Blocker
- **Home:** zainokey (do not pass `ZAI_API_KEY`).
- **Actions:**
  1. START(zainokey, `--account work`), ASK(kaone), STATUS(ka1). Log the SID as `<kid>`. QUIT.
  2. START(zainokey, `resume <kid>`), STATUS(ka2), ASK(katwo), QUIT.
  3. CLI(zainokey, `exec "reply with the single word kathree"`). Log the times, then `agms-tui stop`.
- **Expected:**
  - NO-ONBOARD holds in steps 1–2, and ka1 and ka2 show `· account work`.
  - Step 3 exits non-zero and prints no `kathree` reply.
    - `[observe]` Its wording.
  - **Relay:**
    - `kaone` and `katwo` carry `zwork`.
    - At `kathree`, no request carries `zwork`.
    - `[observe]` Whether a keyless request was made.

### ESC-01: Esc in the `corbanu resume` picker with no default credentials; terminal restored
- **Priority:** Blocker
- **Home:** zainokey, after NA-11.
- **Actions:**
  1. `agms-tui --window <new> start --home zainokey resume`, `wait 'Resume a previous session' 30`, `screen e1`.
  2. `key Escape`, `wait 'product exited' 10`, `screen e2`.
  3. `text 'echo restored 9'`, `key Enter`, `screen e3`, then `agms-tui stop`.
- **Expected:**
  - e1 shows the picker. NO-ONBOARD holds in e1 and e2.
    - `[advisory]` Its footer reads `esc quit`, as in REF-06.
  - e2 shows `[agms] product exited with code 0` within 10 s. It also shows `[agms] terminal settings left:` with `echo icanon isig icrnl opost` and no entry starting with `-`.
  - e3 shows `echo restored 9` once, on its own clean line, with no picker frame left over and no `^[` garbage.

### I426-A: `resume --account ghost <id>` exits before any onboarding
- **Priority:** Blocker
- **Home:** zainokey, after NA-11 (`<kid>` is on `work`).
- **Actions:**
  1. REFUSE(zainokey, `resume --account ghost <kid>`) (label ia1).
  2. REFUSE(zainokey, `resume --account zai:ghost <kid>`) (label ia2).
  3. REFUSE(zainokey, `resume --account ghost --last`) (label ia3), then `agms-tui stop`.
- **Expected:**
  - Each exits non-zero with text that names `ghost` and provider `zai` and gives `corbanu account add zai ghost`.
  - No capture, including the 2 s screens, shows onboarding, the key screen (REF-14) or a picker. NO-ONBOARD holds.
  - **Relay:** nothing during steps 1–3.

### NA-12: Named account on a custom provider (AC2, AC4, D86)
- **Priority:** Blocker
- **Home:** custom.
- **Actions:**
  1. START(custom), ASK(cone), STATUS(c1). Log the SID as `<c1>`. QUIT.
  2. START(custom, `--account work`), ASK(ctwo), STATUS(c2). Log the SID as `<c2>`. QUIT.
  3. START(custom, `resume <c2>`), ASK(cthree), STATUS(c3), QUIT.
  4. CLI(custom, `exec --account work "reply with the single word cfour"`), then `agms-tui stop`.
- **Expected:**
  - The provider line names "Z.AI proxy test".
  - c1 shows `· account default`; c2 and c3 show `· account work`.
  - **Relay:**
    - `cone` → `zenv`.
    - `ctwo`, `cthree` and `cfour` carry `zpxwork`.

### I426-B: `--account ghost` on a custom provider
- **Priority:** Blocker
- **Home:** custom, after NA-12.
- **Actions:**
  1. REFUSE(custom, `resume --account ghost <c1>`) (label ib1).
  2. REFUSE(custom, `resume --account zpx:ghost <c1>`) (label ib2).
  3. REFUSE(custom, `--account ghost`) (label ib3), then `agms-tui stop`.
- **Expected:**
  - Each exits non-zero with text that names `ghost` and provider `zpx` and gives `corbanu account add zpx ghost`.
  - No capture shows a provider picker ("Choose a provider account") or any OpenAI device-code screen. NO-ONBOARD holds.
  - **Relay:** nothing during steps 1–3, including no OpenAI or ChatGPT traffic.

### REG-01a: Feature off: `/status`, `[provider_accounts]` and `/fork` unchanged (AC8)
- **Priority:** Blocker
- **Home:** zai3off.
- **Actions:**
  1. START(zai3off), STATUS(o0), ASK(oone), STATUS2(o1).
  2. `text '/fork'`, `key Enter`, `sleep 3`, `screen`. ASK(otwo), STATUS(o2), QUIT.
  3. CLI(zai3off, `exec --account work "reply with the single word oexec"`).
  4. CLI(zai3off, `account list`), then `agms-tui stop`.
- **Expected:**
  - o0, o1-1, o1-2 and o2 are all plain, and o2 has a "Forked from" row.
  - `[advisory]` The fork screens read like REF-01/REF-11 apart from the account parts.
  - `[observe]` The results of steps 3 and 4.
  - **Relay:**
    - `oone` and `otwo` carry `zenv`, not `zpersonal`.
    - `[observe]` What `oexec` carried.

### REG-01b: Feature off: resuming a conversation recorded on `work`
- **Priority:** Blocker
- **Home:** zai3off.
- **Actions:**
  1. START(zai3off, `--enable named_accounts --account work`). If no session starts:
     - log it and `agms-tui stop`;
     - run `sed -i '' -E 's/^(named_accounts[[:space:]]*=[[:space:]]*)false/\1true/' CFG(zai3off)`;
     - START(zai3off, `--account work`).

     Then ASK(wone), STATUS(w0). Log the SID as `<w>`. QUIT.

     If `sed` was used, change it back with `sed -i '' -E 's/^(named_accounts[[:space:]]*=[[:space:]]*)true/\1false/' CFG(zai3off)` and `cat`.
  2. REFUSE(zai3off, `resume <w>`) (label rb2).
  3. START(zai3off, `resume --account zai:default <w>`), ASK(wtwo), STATUS(w2), QUIT, then `agms-tui stop`.
- **Expected:**
  - w0 shows `· account work` (precondition).
  - Step 2 exits non-zero, names `work` and mentions `--account zai:default`. It shows no session and NO-ONBOARD holds.
  - In step 3, the transcript shows `wone` and w2 is plain.
  - **Relay:**
    - `wone` → `zwork`.
    - Nothing during step 2.
    - `wtwo` → `zenv`.
  - **Files:** CFG(zai3off) ends with `named_accounts = false`.

### REG-01c: Feature off: Esc in both pickers (closes REG-01's gap)
- **Priority:** Blocker (Esc's exact effect in the CLI picker is `[observe]`)
- **Home:** zai3off, after REG-01a/b (usable default credentials; conversations exist).
- **Actions:**
  1. `agms-tui --window <new> start --home zai3off resume`, `wait 'Resume a previous session' 30`, `screen r1`.
  2. `key Escape`, `sleep 3`, `screen r2`.
     - If `[agms] product exited` shows: `text 'echo restored 8'`, `key Enter`, `screen r3`.
     - Otherwise: `screen`, then QUIT.
  3. START(zai3off), ASK(hone), STATUS(rc1). Log SIDh.
  4. Route search (at most 5 minutes): with the composer empty, `text '?'`, `screen`, `key BSpace`, `screen`. Look for a shortcut that opens the resume picker.
  5. **Route 1** (a shortcut was found): `text 'keep draft 271'`, use the shortcut, `sleep 2`, `screen`.
  6. **Route 2** (otherwise):
     - `text 'keep draft 271'`, `key C-a`, `text '/resume '`, `screen`.
     - `key Enter`, `sleep 2`, `screen`.
  7. If "Resume a previous session" is on screen: `key Escape`, `sleep 2`, `screen rc-after`.
  8. If no route reached the picker, log "no route":
     - Clear the composer (`key C-u`, then `BSpace` until empty).
     - `text '/resume'`, `key Enter`, `sleep 3`, `screen`.
     - `key Escape`, `sleep 2`, `screen rc-after`.
  9. STATUS(rc2), QUIT, then `agms-tui stop`.
- **Expected:**
  - r1 shows the picker: the path REG-01 never reached. NO-ONBOARD holds, and there is no account wording.
  - `[observe]` Whether Escape exits or starts a session, and how the footer compares with REF-06.
  - If the program exited: the exit code is 0, the terminal-settings line is normal (no `-` entries), and r3 is a clean echo.
  - rc2 shows SIDh, is plain, and the transcript still shows `hone`.
  - The draft follows the same rule as ESC-03: if route 1 was used, rc-after's composer holds `keep draft 271`; otherwise `[observe]`.
  - `[advisory]` The picker wording matches REF-06/REF-08.

### REG-01d: Feature off, no default credentials: as on the earlier build
- **Priority:** Advisory
- **Home:** zainokeyoff.
- **Actions:**
  1. `agms-tui --window <new> start --home zainokeyoff resume`, `sleep 10`, `screen d1`.
  2. `key Escape`, `sleep 2`, `screen d2`.
  3. `key C-c`, `key C-c`, `wait 'product exited' 10`, `screen d3`.
  4. `text 'echo restored 7'`, `key Enter`, `screen d4`, then `agms-tui stop`.
- **Expected:**
  - `[observe]` d1 is onboarding (the REF-14 key screen or the REF-12 provider list), as intent.md says the pre-change build showed. A picker instead is recorded as a difference.
  - d1 and d2 show no account wording and no "not configured" error.
  - d3 shows that the program exited (log the code), and the terminal-settings line has no `-` entries.
  - d4 is a clean echo.

### NA-13: Named ChatGPT account display, add path, live use; default login untouched (AC6, AC7, REG-02, D86)
- **Priority:** Blocker
- **Home:** oai1, with `t1` freshly signed in. Never sign in any ChatGPT account, and never complete a sign-in.
- **Actions:**
  1. CLI(oai1, `account list`), CLI(oai1, `login status`), CLI(oai1, `login --account t1 status`).
  2. START(oai1), STATUS2(t-def), QUIT.
  3. START(oai1, `--account t1`), STATUS2(t0), ASK(tone), STATUS(t1s).
  4. PROVIDERS, open OpenAI, `screen n13-rows`. Choose the action like "Add another account", `screen n13-add`.
     - Choose the device-code ChatGPT option, `sleep 5`, `screen n13-dev`. Then `key Escape` (repeat until back on the add or account screen), `screen`.
     - Choose the API-key option, `text 'agms-fake-oextra-40411'`, `screen n13-key`. Do not press Enter.
     - Press `Escape` until the composer shows, `screen`.
  5. QUIT. CLI(oai1, `account list`), then `agms-tui stop`.
- **Expected:**
  - n13-rows shows a `t1` row with "ChatGPT sign-in", an email hint (`x…@domain`), a plan and a fingerprint.
    - `[observe]` The rows in step 1's `account list`.
  - `login status` says "API key configured" plus a fingerprint, and shows no `agms-fake` and no `odefault`.
    - `[observe]` The output of `login --account t1 status`.
  - No capture shows `eyJ`, `sk-` or `agms-fake`. The exception is n13-key, which is checked by the next point.
  - n13-key shows only mask characters for the typed key.
  - n13-add offers a device-code ChatGPT sign-in and an OpenAI API key.
  - `[advisory]` n13-dev shows a device-code first screen (URL and/or code). Escape cancels it without error.
  - Step 5's `account list` shows `t1` and no other named account.
  - t0-2 and t1s show `· account t1`, and `tone` is answered.
    - `[observe]` The t-def provider line (the home has a named OpenAI account).
  - NO-ONBOARD does not apply to n13-dev. Elsewhere, no sign-in screen appears.
  - **Relay:**
    - chatgpt.com traffic at `tone`.
    - No api.openai.com request carrying `odefault` during step 3.
  - **Files:** the `auth.json` hash is unchanged.

### D136: Removing `t1` from another terminal while a session runs on it
- **Priority:** Blocker
- **Home:** oai1, after NA-13. The removal in step 2 is the only removal of `t1`; do not repeat it.
- **Actions:**
  1. START(oai1, `--account t1`) (window A), ASK(beforeone), STATUS(x0).
  2. CLI(oai1, `account remove openai t1`). Log the time.
     - If it exits non-zero and points to `logout --account t1`, run CLI(oai1, `logout --account t1`) and log which command worked.
     - If neither removes it, log "not removed" and skip to step 7.
  3. CLI(oai1, `account list`).
  4. Window A: `sleep 10`, STATUS2(x1).
  5. Window A: SEND(afterone, 90), SEND(aftertwo, 90).
  6. Window A: STATUS(x3).
  7. Window A: PROVIDERS, open OpenAI, `screen x-prov`, `Escape` to the composer, QUIT.
  8. CLI(oai1, `account list`), then `agms-tui stop`.
- **Expected:**
  - Step 3 shows no `t1`.
  - Window A never shows a sign-in or onboarding screen (NO-ONBOARD).
  - By x3 at the latest, window A shows text that:
    - names `t1` as removed, not configured or unusable;
    - gives a recovery: `corbanu login --account t1`, `corbanu account add`, `/providers` or `--account`.
  - x3 does not present the session as healthy on `t1`:
    - `t1` is marked removed, missing or unusable, or an error is shown;
    - x3 never shows `· account default` or any other account as the session's account.
    - `[observe]` x1-1 and x1-2.
  - `[observe]` Whether `• afterone` or `• aftertwo` appears (open question Q1).
  - x-prov has no `t1` row, and step 8 has no `t1`.
  - **Relay:**
    - No api.openai.com request carrying `odefault` from step 1 to the end of the group.
    - `[observe]` chatgpt.com traffic at `afterone` and `aftertwo`.
  - **Files:**
    - The `auth.json` hash is unchanged.
    - `config.toml` gained no `openai = "t1"` entry.
    - `[observe]` The vault file names.

### ST-03: The default OpenAI login, in a home with no named accounts, has no provider line (D86, D125)
- **Priority:** Blocker
- **Home:** oai1, after D136 (no named accounts left; do not touch `t1`).
- **Actions:**
  1. START(oai1), STATUS2(y1).
  2. SEND(yone, 60).
  3. STATUS(y2), QUIT.
  4. CLI(oai1, `account list`), then `agms-tui stop`.
- **Expected:**
  - y1-2 and y2 have no `Model provider:` row and no `Account usage:` row.
    - `[observe]` y1-1.
  - Step 2 shows an error (an invalid key) and no `• yone`.
    - `[advisory]` The error does not name `t1`.
  - Step 4 shows no `t1`. This is the D136 check that nothing re-created it.
  - **Relay:** `yone` → api.openai.com carrying `odefault` (401).

