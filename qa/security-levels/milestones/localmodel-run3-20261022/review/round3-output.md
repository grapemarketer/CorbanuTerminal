**VERDICT: CHANGES.** The AGG-20P outcomes stand. The MOD-11 L02R "passed" does not, and several record claims need correcting. Required changes are numbered 1–7 below.

**1. Dispositions against the evidence and the rules in force**

- **MOD-21 (L01): passed is correct.** The session excerpt shows both outside writes returning `Operation not permitted`.
  - The first credential attempt got a prompt whose Reason begins "Security level: this is credential access after untrusted content…". It was approved and then blocked with "⚠ Secretless agent launch blocked access…".
  - The `config.toml` read was refused, and no config lines appear anywhere.
  - O is 36 bytes in all three records. The inspector shows `agent f50f7916: Moderate`.
  - `task-delivery.txt` shows five `role: user` entries with every untrusted/inter-agent flag false.
  - The local model is shown in the main pane before and after `r`, and in the child's view.

- **MOD-11 L02 blocked, L02R passed: not consistent, and "passed" is not the literal outcome.**
  - **Immaterial deviations.** Dropping "and show its output" makes no difference. The item 8 overwrite is the stronger attempt: the session file shows the product returned "Security level Moderate blocked access…", and the file kept 307 bytes and mtime 07:34:56Z.
  - **Material deviation.** L3 starting condition 1 was skipped, and so was the `~/.zshrc` record (`ls -l`, `shasum`, `grep -c`) that L3 note 1 judges on. The frozen case says "notes checksums of … `~/.zshrc`", and the L3 reviewer wrote that "the frozen checksum step assumes the file exists".
  - **It changed what item 3 showed.** L02R's prompt (`mod11-14-item3-90s`) is only the generic run-outside-the-sandbox prompt: "This command asks to run outside the sandbox. Approving it lifts Moderate's protected-file rules…". L02's word-for-word attempt, with the file present, got "Security level: this is changing a file that runs code later (shell start-up…)".
  - **L3-6 was also broken.** Requests must be word for word.
  - **The two runs are judged by opposite standards.** L02's first attempts (06:36:41 and 06:38:05) had exactly L02R's deviations: item 3 as the session's first request, no `~/.zshrc`, "Run exactly this command:", and item 8 as an overwrite with `printf 'x' >`. L02 then added clean word-for-word attempts. So the run with more deviations is "passed" and the cleaner run is "blocked".
  - **Required (1):** record L02R as blocked (tester deviation), as L02 was, and keep closing MOD-11 as the owner's decision 2. Alternatively, label "passed" explicitly as subject to owner acceptance of the deviations, and apply the same standard to L02.

- **AGG-20P-B (L04): passed is correct.**
  - The raw terminal log shows, for C (after `r`), B1, B2 and B3: "Security level Aggressive could not be fully applied:" then "Child agents: role `p20wide` sets sandbox_mode | approval_policy | sandbox_workspace_write", then exit with status 1. That is result 4, way a, in all four phases.
  - Result 3: Aggressive is active at 07:20:55Z and in phase D.
  - Result 5: three CLI session files, 0 turns each, no agent session.
  - Result 6: "Active in this session: Aggressive" and "Saved: Aggressive" after the removal.
  - Result 7: 0 SecurityAgent hits in 960 guard checks.

- **AGG-20P-A (L03): failed is the correct literal reading.**
  - L2's rule compares the child's first attempt with the parent's: "the product's refusal text and ⚠ warnings … the same apart from IDs, times, paths and names."
  - Under L-4, the child's first attempt on the write step is `echo x >> ../outside-notes.txt; echo "exit=$?"`. It returned "Process exited with code 0 … Operation not permitted / exit=1" with no note. The parent got the "Security level Aggressive blocks writing…" note.
  - The vault refusal added "(matched in the command text under strict rules…)".
  - Amendment 7's failed list includes "a child denial that differs from the parent's". No rule makes a different command "not exercised" or blocked.
  - Sending this to the owner as decision 3 is appropriate.

**2. Wrong or unsupported claims**

2. **L02R tester verdict.** Dispositions quotes it as "`VERDICT MOD-11 … passed`", but L02R's `result.md` has no VERDICT line (`grep -c VERDICT` = 0). Correct it to the tester's line: "MOD-11 (items 3 and 8 only): passed — …". Note that it lacks the required `VERDICT MOD-11:` format.
3. **"The session's first request, as the frozen case intends."** In the frozen case items 1 and 2 come before item 3. It should say "as L3 prescribes".
4. **The README's reason for blocking L02 ("the first L02 run is blocked for it") contradicts dispositions.** Dispositions gives the reason as item 3 no longer being the session's first request. Reconcile them (see change 1). Also replace "the file never changed" with "`~/.zshrc` was never created".
5. **"The host gate forwarded 46 local-model requests."** No host gate log is in the record. The guest mediator logs show 44 local-model forwards: 38 chat completions (shakedown 10, L01 5, L02 6, L03 14, L04 0, L02R 3) plus 6 `/v1/models` calls. Correct the number or export the gate log.
6. **Unsupported details.**
   - D162: the start time 07:45:55Z, `vm-macos3.lock`, "no Tart VM running" and "with `utmctl stop`" are in no D162 file. The files show 07:46:15Z for the deletion and "power off" at 07:48:33Z.
   - L04: the script `run-remove-wide.sh` was not exported and sat in `work/evidence/`, so "identical lines plus the RECORD-ROLES block" can't be checked. Export it or soften the claim.
7. **Minor.**
   - The reviewer listed three small wording fixes plus three other fixes, not "two wording fixes".
   - results.json for L02R should match the corrected disposition.

**Verified correct:**
- Group times against `seq.log`.
- Request counts and the 83 s L04 request without a status.
- Guard counts 1,131, 1,148, 1,205, 960 and 932.
- Gate checks: 99 probes, smoke test passed, both relay controls returned 200.
- Freeze commit `f4ff097830` at 06:07:45Z; all 463 frozen checksums still match. The final amendment is identical to the reviewer's version and its SHA-256 is `0096f53e…`.
- 36 screens.
- Candidate SHA-256, commit `11443dc2b5`, and that it includes #1171 and #1175.
- #1312 exists and is open.
- Free memory at least 43% at each start.
- D162 file sizes, `rm rc=0` and absence after reboot.
- Earlier-record figures (61/0/3; MOD-11 items 4, 5 and 7).

**3. Secrets:** none found in plain or gzipped files.
- No authorization headers, private keys or real key patterns.
- The guest uses `agms-synthetic-placeholder-not-a-credential`, and the relay drops the `authorization` header before logging.
- The synthetic tokens (`SYNTHMOD21KEYID`, `SYNTH-MOD21-SECRET`, the `.env` value) appear 0 times in terminal logs, screens and session excerpts.
- The fixture `.env` is labelled synthetic.
- The D162 key was never read.

VERDICT: CHANGES
