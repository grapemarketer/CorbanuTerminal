# More test-machine facts (from the coordinator, for the reviewer)

These answer the harness questions in the designer's "Missing information". They are facts about the test machine, not
about how the product behaves; product questions stay open for you to settle from the packet.

1. `ZAI_API_KEY`: `agms-tui start --home H` sets it automatically, with that home's synthetic key, for `zai3`,
   `zai3off`, `zaione` and `custom` (label `zenv`). It is not set for `zainokey`, `zainokeyoff`, `fresh` or `oai1`. The
   tester never sees the value and should not pass `--env ZAI_API_KEY=...` unless a case says so.
2. Settings: every home uses the product's default settings apart from those listed in tools.md (provider, model,
   `named_accounts`, `[provider_accounts]`, the trusted test project, file-based credential storage and
   `suppress_unstable_features_warning = true`). Whether spawned agents are available is whatever the product does by
   default; the tester can only observe it.
3. When a program started with `agms-tui start` exits, the terminal shows `[agms] product exited with code N` and a
   second line `[agms] terminal settings left: ...` listing the terminal modes the program left behind (a normal
   terminal shows `echo icanon isig icrnl opost`; a leading `-` means that mode was left switched off). Text typed after
   that is echoed.
4. The tester's shell is macOS bash with BSD tools: in-place edits need `sed -i '' ...`; `date -u +%H:%M:%S` and
   `sleep` work. `agms-tui wait` takes a Python regular expression and polls the screen every half second.
5. Window names for `--window` are any 1–8 letters or digits; `agms-tui stop` ends all of them.
6. In the `oai1` group the coordinator signs in `t1` (device code, approved by a person) before the tester starts, and
   after the group, whatever happened, signs `t1` out, removes it and deletes the whole `oai1` home. The tester cannot
   sign in any ChatGPT account itself, so the device-code add path in `/providers` can be opened and its first screen
   observed, but not completed (and must then be cancelled).
7. Removing `t1` in the product is a real removal of the live test account from this home; the coordinator can sign
   it in again only with another human approval, so a group should remove it at most once and do so last.
8. Time: each group gets about 80 minutes (ended at 90) and one fresh machine. Nine groups are acceptable; keep each
   group's cases within that budget for a careful tester that checks the screen about every 30 seconds while a turn
   runs.
9. The relay identifies Z.AI and OpenAI API keys by label only. ChatGPT traffic (`t1`) is not opened, so the relay can
   show that ChatGPT was used, and that no OpenAI API request (for example one carrying `odefault`) was made, but not
   which ChatGPT account was used.
10. Files the integrator can read after a group exclude vault contents and keys: for the `oai1` home it keeps the
    file list, `config.toml`, the conversation files and the `auth.json` hash, saved just before the coordinator's
    sign-out.
