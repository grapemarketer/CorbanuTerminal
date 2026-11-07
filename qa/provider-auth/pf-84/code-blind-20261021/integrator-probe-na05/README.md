# Integrator probe for NA-05 (not a case; not code-blind tester evidence)

Question: does in-session `/resume` of a conversation whose recorded account was removed fall back silently, or does
the TUI's launch `--account` win (NA-05 started the TUI with `--account personal`)? Three attempts, each on its own fresh
clone, same build and harness v4, driven by the integrator through `agms-tui` as the lane user. NA-05 itself (R2) had
`[provider_accounts] zai = "ghost"` and started `--account personal`; the probe homes have `zai = "personal"`.

- **Attempt 1** ([attempt1-exec-conversation-not-in-picker.txt](attempt1-exec-conversation-not-in-picker.txt)): the
  conversation was made with `corbanu exec --account work`; the TUI picker did not list it ("No results for your
  search"), so nothing was learned.
- **Attempt 2** ([probe-transcript.txt](probe-transcript.txt), [screens.jsonl](screens.jsonl),
  [actions.jsonl](actions.jsonl), [session-accounts.txt](session-accounts.txt), [relay.txt](relay.txt)): conversation
  `qzero` made in the TUI with `--account work` (relay `zwork`); `corbanu account remove zai work`; TUI started
  **without** `--account` (session on `personal` via `[provider_accounts]`); `/resume`, search `qzero`, Enter →
  "■ This conversation ran on zai account `work` and resumes on it: account `work` of provider `zai` is not configured;
  add it or pick another account in `/providers`". The next prompt `qone` ran in the current session on `personal`;
  the `qzero` conversation still records only `work`.

- **Arm B** ([armB/](armB/), added after review round 2 to isolate the cause): same as attempt 2, but the TUI is
  started **with `--account personal`** (and `[provider_accounts] zai = "personal"`). The in-app `/resume` of `qzero`
  opened the conversation with no message; `qone` was answered in it on `personal` (relay 03:27:15Z `zpersonal`);
  the conversation now records `work, personal`; `/status` shows `· account personal`. Compared with attempt 2, the
  flag is the only scripted difference (host and guest scripts differ only in `--account personal`), so the launch
  `--account`, not the `ghost` entry, is the likely cause. **Caveat:** the arm-B package also differed
  ([armB/package-files-probe-armB.txt](armB/package-files-probe-armB.txt) vs [../harness/package-files.txt](../harness/package-files.txt)):
  `corbanu` was identical (`bbae7d37…`), but `corbanu-acp`, `corbanu-walletd` and `codex-path/rg` were absent and
  `codex-code-mode-host`, `codex-package.json` and `setup-telegram.sh` had other digests (the original stage folder had
  been deleted; another run's stage was used). The integrator expects none of these to be on the `/resume` path, but the
  record cannot prove it.
  Two earlier arm-B clones were discarded: the first stopped at staging (the package folder had been deleted, no
  product step ran); the second ran with the host Z.AI relay stopped (its gate failed, so no model replied) and its
  observations were not used. Both clones were deleted; their safety files are in [armB/discarded/](armB/discarded/)
  (the staging failure left an empty guard file: no checks had started).

`screens.jsonl`, `actions.jsonl` and `session-accounts.txt` come from each clone's `collect` (`collected.tar`;
`session-accounts.txt` is an integrator extract of the conversation files). Attempt 1, 2 and arm-B transcripts were written to the host run log (`probe-na05.sh` and `armB/probe-na05-armB.sh` put their `> probe-transcript.txt` redirect on its
own line, so the file inside the clone folder stayed empty); the copies here come from that log, redacted.
Clone safety files: `clone-*`.
