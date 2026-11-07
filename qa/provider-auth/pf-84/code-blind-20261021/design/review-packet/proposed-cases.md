I've proposed 28 cases below, run in 9 groups, one fresh machine per group. They cover all eight acceptance criteria, D136, both #426 legs, the earlier legs ESC-01/03/04, GH-02, PR-01 and REG-01, and D86/D124/D125. I read only the four packet items you listed (intent.md, tools.md, provider-accounts-doc.md and the screens) and nothing else. The packet doesn't settle some expected results, notably what "not healthy" looks like in D136 and whether ESC-03's draft clause can be reached; those are in the missing-information list at the end.

## Conventions used in every case

| Short form | What the tester does |
|---|---|
| **START(H, args)** | `agms-tui start --home H args`, then `agms-tui wait 'glm-5.3-flash' 60` (in `oai1`: `wait 'gpt-5.6-luna' 60`). |
| **ASK(w)** | `agms-tui text 'reply with the single word w'`, `agms-tui key Enter`, `agms-tui wait '• w' 90`, then in the shell `date -u +%H:%M:%S`; log "w = time". |
| **STATUS(l)** | `agms-tui text '/status'`, `agms-tui key Enter` (press Enter again if a command list still shows `/status` highlighted), `agms-tui wait 'Session:' 3`, `agms-tui screen l`. Record whether the wait finished within 3 s. |
| **STATUS2(l)** | STATUS(l-1); `sleep 5`; STATUS(l-2). Judge l-2: D124 allows the first `/status` after a change to be stale. |
| **QUIT** | `agms-tui text '/quit'`, Enter, `agms-tui wait 'product exited' 5`. If nothing happens, press `C-c` twice. Record the exit code. |
| **CLI(H, cmd)** | `agms-tui --window <new letter> start --home H cmd`, `agms-tui --window <letter> wait 'product exited' 30`, then `screen`. Record the exit code. Use a new window letter for every command (B, C, D…). |
| **KEYIN(k)** | Right after an `account add` CLI start: `screen`, `text 'k'`, `key Enter`. If it hasn't exited after 5 s, `key C-d`. |
| **SID** | The `Session:` value from `/status`. |
| **NO-ONBOARD** | No capture contains "Welcome to Corbanu Terminal", "Choose a provider account", "Paste or type your API key" or "Sign in with device code". |
| **NO-SECRET(k)** | No capture contains `agms-fake`, the key's label or the key's digits. |
| **CFG(H)** | `$AGMS_RUN/homes/H/config.toml`. After every edit, `cat` it and save the output. |

Lines starting **Relay:** or **Files:** are checked by the integrator afterwards. The tester only logs the UTC times.

## Group plan (one fresh machine each, in this order)

| Group | Home | Cases |
|---|---|---|
| G1 | zai3 | NA-01, NA-02, NA-03, NA-04 |
| G2 | zai3 | PR-01, GH-02, NA-05 |
| G3 | zai3 | NA-06, NA-07, ESC-03 |
| G4 | zai3 | NA-08, NA-09, NA-10 |
| G5 | zaione, fresh | ST-01, ST-02, ESC-04 |
| G6 | zainokey | NA-11, ESC-01, I426-A |
| G7 | custom | NA-12, I426-B |
| G8 | zai3off, zainokeyoff | REG-01a, REG-01b, REG-01c, REG-01d |
| G9 | oai1 | NA-13, D136, ST-03 |

---

### NA-01: CLI list/add/remove; keys only from stdin; no secrets (AC1, AC6)
- **Priority:** Blocker
- **Home:** zai3, unchanged, no TUI running.
- **Actions:**
  1. CLI(zai3, `account list`).
  2. CLI(zai3, `account add zai extra`), then KEYIN(`agms-fake-zextra-58213`).
  3. CLI(zai3, `account add zai extra2 agms-fake-zextratwo-61407`).
  4. CLI(zai3, `account list`).
  5. CLI(zai3, `login status`).
  6. CLI(zai3, `exec --account extra "reply with the single word xone"`), then log the time.
  7. CLI(zai3, `account remove zai extra`).
  8. CLI(zai3, `account list`).
  9. CLI(zai3, `exec --account extra "reply with the single word xtwo"`), then log the time.
- **Expected:**
  - Step 1 exits 0 and lists Z.AI `default`, `work` and `personal`, each with a kind and a fingerprint.
  - Step 2 exits 0 and confirms `extra`. Advisory: note whether the typed key was echoed on screen.
  - Step 3 is refused (non-zero exit, usage error), and step 4 has no `extra2`.
  - Step 4 shows `extra`.
  - Step 5 says "API key configured" plus a fingerprint, with no key fragment.
  - Step 6 prints `xone`.
  - Step 7 exits 0, and step 8 has no `extra`.
  - Step 9 exits non-zero with "account `extra` of provider `zai` is not configured; add it with `corbanu account add zai extra` or pick another with `--account`…". No `xtwo` is printed.
  - NO-SECRET(zextra / 58213; zextratwo / 61407) holds in every capture.
  - **Relay:** the `xone` request carries `zextra`. No request at the `xtwo` time. No request ever carries `zextratwo`.

### NA-02: Choosing the account for `exec` and the TUI (AC2)
- **Priority:** Blocker
- **Home:** zai3 (`[provider_accounts] zai = "personal"`).
- **Actions:**
  1. CLI(zai3, `exec --account work "reply with the single word eone"`).
  2. CLI(zai3, `exec --account zai:work "reply with the single word etwo"`).
  3. CLI(zai3, `exec "reply with the single word ethree"`).
  4. CLI(zai3, `exec --account default "reply with the single word efour"`).
  5. START(zai3, `--account zai:work`), STATUS(n2-a), ASK(efive), QUIT.
  6. Log the time of every step.
- **Expected:**
  - Every `exec` exits 0 and prints its word.
  - Step 5's `/status` "Model provider" line ends `· account work`.
  - **Relay:** eone→`zwork`, etwo→`zwork`, ethree→`zpersonal`, efour→`zenv`, efive→`zwork`.

### NA-03: Missing account fails closed and says how to recover (AC3, AC2 precedence)
- **Priority:** Blocker
- **Home:** zai3.
- **Actions:**
  1. `agms-tui start --home zai3 --account ghost`, `wait 'product exited' 30`, screen.
  2. CLI(zai3, `exec --account ghost "reply with the single word gone"`).
  3. CLI(zai3, `exec --account zai:ghost "reply with the single word gtwo"`).
  4. `sed -i '' 's/^zai = "personal"/zai = "ghost"/' CFG(zai3)`.
  5. `agms-tui --window C start --home zai3`, `wait 'product exited' 30`, screen.
  6. CLI(zai3, `exec "reply with the single word gthree"`).
  7. START(zai3, `--account work`) in a new window, ASK(gfour), STATUS(n3-a), QUIT.
  8. Change `ghost` back to `personal` with sed.
  9. Log the times throughout.
- **Expected:**
  - Steps 1–3, 5 and 6 each exit non-zero.
  - Each names `ghost` and provider `zai`, gives `corbanu account add zai ghost` and mentions `--account` (wording may differ from REF-13).
  - No reply word is printed, and NO-ONBOARD holds; no picker and no TUI session appear.
  - Step 7 works and `/status` shows `· account work`.
  - **Relay:** no request at the gone, gtwo or gthree times (none keyless, none on `zenv`). gfour→`zwork`.

### NA-04: A rejected key names the account; no fallback (AC3, AC6)
- **Priority:** Blocker
- **Home:** zai3.
- **Actions:**
  1. CLI(zai3, `account add zai badone`), then KEYIN(`agms-fake-badone-77302`).
  2. CLI(zai3, `exec --account badone "reply with the single word bone"`), log the time.
  3. START(zai3, `--account badone`).
  4. `text 'reply with the single word btwo'`, Enter, `sleep 60`, screen, log the time.
  5. STATUS(n4-a), QUIT.
- **Expected:**
  - Step 2 exits non-zero. The error names `badone` and the recovery `corbanu account add zai badone`, and no `bone` is printed.
  - Step 4 shows an in-session error naming `badone`. No `• btwo` appears, and NO-ONBOARD holds.
  - `/status` names `badone`, not `default` or `personal`.
  - NO-SECRET(badone / 77302) holds.
  - **Relay:** every request at those times carries `badone` (401). None carries `zenv`, `zwork` or `zpersonal`, and none is keyless.
  - **Files:** CFG(zai3) still has `zai = "personal"`.

### PR-01: Full precedence order, confirmed by `/status` and the relay at every step
- **Priority:** Blocker
- **Home:** zai3 (`[provider_accounts] zai = "personal"`).
- **Actions:**
  1. Setup: START(zai3, `--account work`), ASK(pzero), STATUS(p0), note the SID as `<id>`, QUIT.
  2. Step 1: START(zai3, `resume --account personal <id>`), STATUS(p1-pre), ASK(pone), STATUS(p1), QUIT.
  3. Step 2: START(zai3, `resume <id>`), STATUS(p2-pre), ASK(ptwo), STATUS(p2), QUIT.
  4. Step 3: START(zai3), STATUS(p3-pre), ASK(pthree), STATUS(p3), QUIT.
  5. Step 4: `sed -i '' '/^\[provider_accounts\]/,/^zai = /d' CFG(zai3)`, then START(zai3), STATUS(p4-pre), ASK(pfour), STATUS(p4), QUIT.
- **Expected:**
  - The `/status` provider line reads, in order: work (p0), personal (p1), work (p2), personal (p3), default (p4). In p4 it shows `· account default`, because the home has named accounts.
  - In p1 and p2, "Account usage" is a table with the session's account marked `(active)` and the earlier counts of the other account.
  - **Relay:** pzero→`zwork`, pone→`zpersonal`, ptwo→`zwork`, pthree→`zpersonal`, pfour→`zenv`.
  - **Files:** `<id>` records provider `zai`, account `work`.

### GH-02: The recorded account was removed while `[provider_accounts]` names a missing one
- **Priority:** Blocker (the mention of `ghost` is advisory)
- **Home:** zai3, right after PR-01 (no `[provider_accounts]` section; conversation `<id>` is on `work`).
- **Actions:**
  1. `printf '\n[provider_accounts]\nzai = "ghost"\n' >> CFG(zai3)`.
  2. START(zai3, `resume <id>`), STATUS(g1), ASK(gaone), QUIT.
  3. CLI(zai3, `account remove zai work`), log the time.
  4. `agms-tui --window E start --home zai3 resume <id>`, `wait 'product exited' 30`, screen, log the time.
- **Expected:**
  - Step 2 resumes on `work`, as in REF-05: `/status` shows `· account work`.
  - Step 4 exits non-zero with "This conversation ran on zai account `work`… `corbanu account add zai work`…". NO-ONBOARD holds.
  - Advisory: the step 4 text also mentions `ghost`.
  - **Relay:** gaone→`zwork`; nothing at the step 4 time.

### NA-05: In-session `/resume` of a conversation whose account is gone (as in REF-10)
- **Priority:** Blocker
- **Home:** zai3, right after GH-02 (`work` removed, `zai = "ghost"`).
- **Actions:**
  1. START(zai3, `--account personal`), ASK(rone), STATUS(r1), note SID1.
  2. `/resume` + Enter. Highlight the conversation whose preview contains `pzero` (use Up/Down) and press Enter. Screen.
  3. STATUS(r2), ASK(rtwo), QUIT.
- **Expected:**
  - Step 2 shows the message "…ran on zai account `work`… not configured; add it or pick another account in `/providers`".
  - The session stays put: r2 shows SID1 and `· account personal`, and the transcript still shows `rone`.
  - **Relay:** rtwo→`zpersonal`; nothing carries `zenv`.

### NA-06: `/providers` rows, adding with a masked key, rename (AC1, AC6)
- **Priority:** Advisory
- **Home:** zai3.
- **Actions:**
  1. START(zai3), `/providers` + Enter, screen. Move to the Z.AI entry (look for "Z.AI") and open it. Screen.
  2. Find the action labelled like "Add another account". Name it `added`, type `agms-fake-zadded-40958` where a key is asked for, and screen before pressing Enter. Confirm.
  3. Select `added`, find "Rename", rename it to `renamed`, screen.
  4. Escape out of the menu, then QUIT.
  5. CLI(zai3, `account list`).
  6. CLI(zai3, `exec --account renamed "reply with the single word nren"`).
  7. CLI(zai3, `exec --account added "reply with the single word nold"`).
- **Expected:**
  - The rows `default`, `work` and `personal` each show a kind and a fingerprint. `personal` carries both the current-session marker and the default marker.
  - The key field shows only mask characters. NO-SECRET(zadded / 40958) holds in all captures.
  - After the rename, the row shows `renamed`, and `account list` has `renamed` but not `added`.
  - Step 6 prints `nren`. Step 7 fails with the not-configured text for `added`.
  - **Relay:** nren→`zadded`; nothing at the nold time.

### NA-07: Use for this session, make default, and removing the account in use (AC1, AC2)
- **Priority:** Blocker
- **Home:** zai3.
- **Actions:**
  1. START(zai3) in window A, then STATUS(u0) (expect personal).
  2. Window A: in `/providers` → Z.AI, select `work` → "Use for this session". Escape back, then STATUS(u1) and ASK(uone).
  3. Window A: in `/providers`, select `work` → "Make default". Escape, then `grep -A1 provider_accounts CFG(zai3)`.
  4. Window B: START(zai3), then STATUS(u2) and ASK(utwo).
  5. Window A: in `/providers`, select `work` → "Remove". Screen at each prompt. If asked for a replacement, choose `personal` and confirm. Then STATUS(u3) and ASK(uthree).
  6. Window B (still open on `work`): `text 'reply with the single word ufour'`, Enter, `sleep 60`, screen, log the time.
  7. CLI(zai3, `account list`), then `cat CFG(zai3)`.
- **Expected:**
  - u1 shows `· account work`.
  - After step 3, the config reads `zai = "work"`, and u2 shows `· account work`.
  - Step 5 asks for a replacement **before** removing. u3 shows `· account personal`.
  - In step 6, no `• ufour` appears and window B shows an error naming `work` with a recovery.
  - `account list` has no `work`. Record what `[provider_accounts]` now says.
  - **Relay:** uone→`zwork`, utwo→`zwork`, uthree→`zpersonal`. At the ufour time, nothing on `zenv` or `zpersonal`.

### ESC-03: Esc in the in-session `/resume` picker keeps the session and the draft
- **Priority:** Advisory
- **Home:** zai3 with at least one earlier conversation (run START(zai3), ASK(cprev), QUIT first).
- **Actions:**
  1. START(zai3), ASK(cone), STATUS(c1), note the SID.
  2. With the composer empty, `agms-tui text '?'` and screen. Look for any shortcut that opens the resume picker, or that keeps a draft while a command runs. Clear with `BSpace`.
  3. If such a shortcut exists, type `keep draft 314` (no Enter) and use it.
  4. Otherwise, type `keep draft 314`, press `C-a` (or `Home`), type `/resume `, screen, then Enter and screen.
  5. If the picker (REF-08) shows, press Escape and screen.
  6. STATUS(c2).
- **Expected:**
  - After Escape, you're back in the same session: c2 has the same SID and `· account personal`, and `cone` is still in the transcript.
  - The composer still holds `keep draft 314`.
  - If no route opens the picker with a draft present, record "no route". The draft clause then needs an owner decision (see missing information).

### NA-08: `/fork`, editing an earlier prompt and `/side` keep the account (AC4)
- **Priority:** Blocker
- **Home:** zai3 (`[provider_accounts] zai = "personal"`).
- **Actions:**
  1. START(zai3, `--account work`), ASK(fone), STATUS(f0), note SID0.
  2. `/fork` + Enter, then ASK(ftwo) and STATUS(f1).
  3. Edit an earlier prompt: with the composer empty, press Escape and look for a hint about editing a previous message. Press Escape again, select the `ftwo` prompt with Up/Down and press Enter. Delete the text (`C-u` or `BSpace`), then ASK(fthree) by typing its sentence and pressing Enter. Then STATUS(f2).
  4. `/side` + Enter. Look for "Side from main thread" (REF-02). ASK(fside), STATUS(f3), then `C-c` to close the side conversation.
  5. QUIT.
- **Expected:**
  - f1, f2 and f3 all show `· account work`, and f1 shows "Forked from SID0".
  - "Account usage" shows `work` with non-zero counts. No `personal` row with counts appears (that was the REF-11 defect).
  - **Relay:** fone, ftwo, fthree and fside all carry `zwork`.

### NA-09: `corbanu fork <id>` and spawned agents keep the source's account (AC4)
- **Priority:** Blocker
- **Home:** zai3, after NA-08 (SID0 is on `work`).
- **Actions:**
  1. START(zai3, `fork SID0`). If a turn or fork picker shows, choose the newest item and press Enter. Then STATUS(k1) and ASK(kone).
  2. `text 'Use your sub-agent tool to start one agent whose only task is to reply with the single word child, then tell me its reply.'`, Enter, `wait 'child' 180`, screen, log the start and end times. QUIT.
  3. START(zai3, `fork SID0 --account personal`), STATUS(k2), ASK(ktwo), QUIT.
- **Expected:**
  - k1 shows `· account work`.
  - The agent's reply `child` appears. If the screen says spawning isn't available, record that.
  - k2 shows `· account personal`.
  - **Relay:** kone→`zwork`; every request in the spawn window carries `zwork`; ktwo→`zpersonal`.
  - **Files:** the child agent's session records `work`.

### NA-10: `/status` format with named accounts (AC5, D86)
- **Priority:** Advisory
- **Home:** zai3.
- **Actions:**
  1. START(zai3), STATUS2(s0), ASK(sone), STATUS(s1).
  2. Switch to `work` using `/providers` → "Use for this session", then ASK(stwo) and STATUS(s2).
  3. Capture the footer line each time. QUIT.
- **Expected:**
  - s0: the provider line ends `· account personal`, and "Account usage" reads `personal · no usage yet`.
  - s1: a single line in the form `personal · N input · N cached input · N output (N reasoning)`.
  - s2: a table with `work (active)` and `personal`.
  - No `$`, "cost" or "price" anywhere. The "Account:" line says "API key configured" with no fragment of the key.
  - Advisory: note whether the footer says `via zai default` while the session is on another account (REF-01/02/09 show this).

### ST-01: A home with no named accounts looks as it does with the feature off (D86/D125)
- **Priority:** Blocker (most users are in this situation)
- **Home:** zaione.
- **Actions:**
  1. START(zaione), STATUS(z0) immediately, ASK(zone), STATUS2(z1).
  2. `/providers` + Enter, screen, Escape.
  3. STATUS2(z2), QUIT.
  4. CLI(zaione, `exec "reply with the single word zexec"`).
- **Expected:**
  - In z1-2 and z2-2, the provider line is exactly `Z.AI - https://api.z.ai/api/paas/v4`, with no `· account`, and there is no "Account usage" line.
  - The set and order of lines matches REG-01a's feature-off `/status` (the integrator compares them).
  - z0 may show the account (D124: shown until known).
  - Every `/status` renders within 3 s.
  - **Relay:** zone and zexec carry `zenv`.

### ST-02: An account added by another process appears on a later `/status`; `/status` never stalls (D124)
- **Priority:** Advisory (escalate to blocker if any `/status` stalls)
- **Home:** zaione, after ST-01.
- **Actions:**
  1. Window A: START(zaione), ASK(done), STATUS(d0).
  2. CLI(zaione, `account add zai extra`), then KEYIN(`agms-fake-zextra-29044`), log the time.
  3. Window A: STATUS(d1), `sleep 5`, STATUS(d2). If d2 is still plain, open `/providers`, Escape, then STATUS(d3).
  4. CLI(zaione, `account remove zai extra`). Window A: STATUS2(d4), then QUIT.
- **Expected:**
  - d0 is plain.
  - d1 may be plain or show the account. By d2, or at the latest d3, the line shows `· account default` and an "Account usage" line appears.
  - In d4-2 the line is plain again (the session never used `extra`).
  - Every `/status` renders within 3 s (record the wait results). NO-SECRET(zextra / 29044) holds.

### ESC-04: Normal onboarding in a fresh home (compared with REF-12/REF-14)
- **Priority:** Blocker
- **Home:** fresh.
- **Actions:**
  1. `agms-tui start --home fresh`, `sleep 10`, screen.
  2. Press Down until `>` is on "Provider: Z.AI API Key", press Enter, screen.
  3. Escape, screen. Return to Z.AI the same way.
  4. `text 'agms-fake-zfresh-30517'` and screen. Press Enter.
  5. Follow any further screens (if asked for a model, pick `glm-5.3-flash`), capturing each one.
  6. ASK(frone), STATUS2(fr1), QUIT.
  7. CLI(fresh, `account list`).
- **Expected:**
  - Steps 1 and 2 show the same headings, option names and order as REF-12/REF-14. Record every difference; "Ollama" status text may differ.
  - No account or error text appears.
  - Escape returns to the provider list.
  - fr1 shows no account suffix and no "Account usage" line.
  - `account list` shows only `default`.
  - Advisory: note whether the key was masked in step 4.
  - **Relay:** frone→`zfresh`.

### NA-11: No default credentials; the named account works without onboarding (AC2, AC3)
- **Priority:** Blocker
- **Home:** zainokey.
- **Actions:**
  1. START(zainokey, `--account work`), ASK(kaone), STATUS(ka1), note SID as `<kid>`, QUIT.
  2. START(zainokey, `resume <kid>`), STATUS(ka2), ASK(katwo), QUIT.
  3. CLI(zainokey, `exec "reply with the single word kathree"`).
- **Expected:**
  - NO-ONBOARD holds throughout steps 1–2, and ka1/ka2 show `· account work`.
  - Step 3 exits non-zero with an error that says the default credentials are missing and how to set them up. No `kathree` is printed.
  - **Relay:** kaone and katwo carry `zwork`. Nothing at the kathree time: no keyless request and none on `zwork`.

### ESC-01: Esc in the `corbanu resume` picker with no default credentials; terminal restored
- **Priority:** Advisory
- **Home:** zainokey, after NA-11.
- **Actions:**
  1. `agms-tui start --home zainokey resume`, `wait 'Resume a previous session' 30`, screen.
  2. `key Escape`, `wait 'product exited' 5`, screen.
  3. `text 'echo restored 9'`, `key Enter`, screen.
- **Expected:**
  - Step 1 shows the picker (as in REF-06, footer `esc quit`). NO-ONBOARD holds.
  - Step 2 shows `[agms] product exited with code 0` within 5 s.
  - Step 3 echoes `echo restored 9` once, on its own clean line, with no picker frame left over it and no `^[`/escape garbage. This is the direct evidence that the terminal was restored.

### I426-A: `resume --account ghost <id>` exits before any onboarding
- **Priority:** Blocker
- **Home:** zainokey, after NA-11 (`<kid>` on `work`).
- **Actions:**
  1. `agms-tui start --home zainokey resume --account ghost <kid>`, then screen at 2 s and at 10 s (`sleep`), then `wait 'product exited' 30`, screen, log the time.
  2. Repeat with `resume --account zai:ghost <kid>` in a new window.
  3. Repeat with `resume --account ghost --last` in a new window.
- **Expected:**
  - Each exits non-zero with text naming `ghost`, provider `zai`, and `corbanu account add zai ghost`.
  - At no capture is there onboarding or the key screen (REF-14) — NO-ONBOARD holds — and no picker.
  - **Relay:** nothing at those times.

### NA-12: Named account on a custom provider (AC2, AC4)
- **Priority:** Blocker
- **Home:** custom.
- **Actions:**
  1. START(custom), ASK(cone), STATUS(c1), note SID as `<c1>`, QUIT.
  2. START(custom, `--account work`), ASK(ctwo), STATUS(c2), note SID as `<c2>`, QUIT.
  3. START(custom, `resume <c2>`), ASK(cthree), STATUS(c3), QUIT.
  4. CLI(custom, `exec --account work "reply with the single word cfour"`).
- **Expected:**
  - The provider line names "Z.AI proxy test". c1 shows `· account default`; c2 and c3 show `· account work`.
  - **Relay:** cone→`zenv`; ctwo, cthree and cfour carry `zpxwork`.

### I426-B: `--account ghost` on a custom provider
- **Priority:** Blocker
- **Home:** custom, after NA-12.
- **Actions:**
  1. `agms-tui start --home custom resume --account ghost <c1>`, screens at 2 s and 10 s, `wait 'product exited' 30`.
  2. Same with `resume --account zpx:ghost <c1>`.
  3. Same with `--account ghost` (new session).
  4. If anything other than an error appears, screen it, press Escape once, screen again (to document the old defect), then `C-c` twice.
- **Expected:**
  - Each exits non-zero, naming `ghost`, provider `zpx` and `corbanu account add zpx ghost`.
  - There is never a provider picker ("Choose a provider account") and never an OpenAI device-code screen. NO-ONBOARD holds.
  - **Relay:** nothing at those times, including no OpenAI or ChatGPT traffic.

### REG-01a: Feature off: `/status`, `[provider_accounts]` and `/fork` unchanged (AC8)
- **Priority:** Blocker (it decides which account requests run on)
- **Home:** zai3off.
- **Actions:**
  1. START(zai3off), STATUS(o0), ASK(oone), STATUS2(o1).
  2. `/fork` + Enter, ASK(otwo), STATUS(o2), QUIT.
- **Expected:**
  - All `/status` captures show no `· account` and no "Account usage". o2 has a "Forked from" line.
  - The fork screens read like REF-01/REF-11 apart from the account parts.
  - **Relay:** oone and otwo carry `zenv`, not `zpersonal`.

### REG-01b: Feature off: resuming a conversation recorded on `work`
- **Priority:** Blocker
- **Home:** zai3off.
- **Actions:**
  1. `sed -i '' 's/^named_accounts = false/named_accounts = true/' CFG(zai3off)`, then START(zai3off, `--account work`), ASK(wone), STATUS, note SID as `<w>`, QUIT. Change it back to `false` with sed.
  2. `agms-tui start --home zai3off resume <w>`, `wait 'product exited' 30`, screen.
  3. START(zai3off, `resume --account zai:default <w>`), ASK(wtwo), STATUS(w2), QUIT.
- **Expected:**
  - Step 2 exits non-zero, names `work` and suggests `--account zai:default`. It doesn't silently resume.
  - Step 3 resumes; w2 has no suffix and no "Account usage".
  - **Relay:** wone→`zwork`; nothing at the step 2 time; wtwo→`zenv`.

### REG-01c: Feature off: Esc in both pickers (the gap left in REG-01)
- **Priority:** Advisory
- **Home:** zai3off, which has usable default credentials and conversations from REG-01a/b.
- **Actions:**
  1. As in ESC-01 steps 1–3, with `--home zai3off resume`.
  2. Then repeat ESC-03 steps 1–6 in zai3off.
- **Expected:**
  - The `resume` picker shows (as in REF-06), and Escape exits with code 0 and the terminal is restored (clean echo).
  - Esc in the in-session `/resume` returns to the same session with the same SID and the draft (if a route exists).
  - Wording matches REF-06/REF-08. `/status` has no suffix.

### REG-01d: Feature off, no default credentials: same as the earlier build
- **Priority:** Advisory
- **Home:** zainokeyoff.
- **Actions:**
  1. `agms-tui start --home zainokeyoff resume`, `sleep 10`, screen.
  2. Escape, screen.
  3. `C-c` twice, `wait 'product exited' 10`.
  4. `text 'echo restored 7'`, Enter, screen.
- **Expected:**
  - The screens match the earlier build (REF-12/REF-14). There is no account wording and no account error.
  - The process exits and the echo line is clean. Record the exit code.

### NA-13: Named ChatGPT account display and live use; default login untouched (AC6, AC7)
- **Priority:** Blocker
- **Home:** oai1, with `t1` freshly signed in. Do not sign in any other ChatGPT account.
- **Actions:**
  1. CLI(oai1, `account list`), CLI(oai1, `login status`), CLI(oai1, `login --account t1 status`).
  2. START(oai1, `--account t1`), STATUS2(t0), ASK(tone), STATUS(t1s).
  3. `/providers` + Enter and screen the OpenAI rows. Open "Add another account" on OpenAI, screen the options, then press Escape until you're back in the composer, choosing nothing.
  4. QUIT.
- **Expected:**
  - The `t1` row shows "ChatGPT sign-in", an email hint like `x…@domain`, a plan and a fingerprint.
  - `login status` says "API key configured" plus a fingerprint, with no `agms-fake` and no `odefault`.
  - Nothing contains a token: no `eyJ` and no long random strings.
  - t0 and t1s show `· account t1`, and `tone` is answered.
  - The add menu offers a device-code ChatGPT sign-in and a masked API key.
  - **Relay:** chatgpt.com traffic at the tone time; no api.openai.com request carrying `odefault` during the session.
  - **Files:** the `auth.json` hash is unchanged.

### D136: Removing `t1` from another terminal while a session runs on it
- **Priority:** Blocker
- **Home:** oai1, after NA-13.
- **Actions:**
  1. Window A: START(oai1, `--account t1`), ASK(before), STATUS(x0).
  2. CLI(oai1, `account remove openai t1`) and log the time. If it's refused with a pointer to `logout`, run CLI(oai1, `logout --account t1`) instead and record which one worked.
  3. CLI(oai1, `account list`).
  4. Window A: `sleep 10`, STATUS(x1), `sleep 5`, STATUS(x2).
  5. Window A: `text 'reply with the single word after'`, Enter, `sleep 90`, screen, log the time. Repeat once with `aftertwo`.
  6. Window A: `/providers`, screen, Escape, then QUIT.
  7. CLI(oai1, `account list`).
- **Expected:**
  - Step 3 shows no `t1`.
  - x2 doesn't present `t1` as healthy (it marks it missing or unusable) and never shows `default`.
  - In step 5, no `• after` and no `• aftertwo` appear. The screen names `t1` as gone and gives a recovery (`corbanu login --account t1`, `/providers` or `--account`). No sign-in screen appears.
  - `/providers` has no `t1`, and step 7 still has no `t1`.
  - **Relay:** no api.openai.com request carrying `odefault` at any time.
  - **Files:** the `auth.json` hash is unchanged, and no `t1` vault file exists after the removal time.

### ST-03: A default OpenAI login in a home with no named accounts has no provider line (D86)
- **Priority:** Advisory
- **Home:** oai1, after D136 (no named accounts left).
- **Actions:**
  1. START(oai1), STATUS2(y1).
  2. `text 'reply with the single word yone'`, Enter, `sleep 60`, screen.
  3. STATUS(y2), QUIT.
- **Expected:**
  - y1-2 and y2 have no "Model provider" line and no "Account usage" line.
  - The prompt fails with the default login's invalid-key error, which doesn't name `t1`.
  - **Relay:** yone→api.openai.com carrying `odefault` (401).

---

## Missing information

1. **D136 wording:** what `/status` should show for a removed account. Also whether a reply that succeeds on `t1`'s still-held in-memory sign-in after removal counts as a failure, and whether removing a ChatGPT account is `account remove openai t1` or `logout --account t1`.
2. **Relay limits:** for ChatGPT the relay records only host, time and size. Fallback is detectable only because the default login is an API key (`odefault`); use of `t1` itself can't be proven.
3. **ESC-03:** there is no known way to open `/resume` with a draft in the composer. The owner should confirm the clause applies, or name the route.
4. **`ZAI_API_KEY`:** whether `agms-tui` sets it automatically for each home, or the tester must pass it with `--env`. The tester doesn't know the `zenv` key value.
5. **Spawned agents:** whether agents or the multi-agent features are enabled in the homes, so NA-09's spawn step can run.
6. **Feature off:** the expected behaviour of `--account work`, `account list` and `[provider_accounts]`. The intent says "as before", but the user doc says resuming a named conversation is refused. There are no feature-off reference screens for `/status`.
7. **PR-01:** whether resuming with `--account personal` re-records the conversation's account. If it does, the expected `work` in step 2 changes.
8. **Removing the default account:** what `[provider_accounts]` should contain after its account is removed through `/providers`.
9. **Fresh home (ESC-04):** which model onboarding picks, and whether onboarding should mask the typed key.
10. **Typed keys:** whether `account add` should hide a key typed at a terminal, and how the fingerprint is defined, so the tester can tell it isn't part of the key.
11. **Unreadable vault (D124):** the "vault cannot be read" path can't be set up with the available tools.
12. **Footer:** the footer shows `via zai default` while `/status` says `account work` in REF-01/02/04/09. Is that intended?
13. **Device-code add path (AC7):** it can't be completed by the tester, because only the coordinator's `t1` sign-in is permitted.
14. **Exit codes:** the expected exit code for TUI refusals. "Non-zero" is assumed from REF-13, which shows 1.
