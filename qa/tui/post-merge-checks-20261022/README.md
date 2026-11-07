# Post-merge true-TUI checks, 2026-10-22 (#1327, #1322, #1320, #1319)

Interactive checks, keys sent through tmux, of four merged fixes that had only unit/snapshot coverage.
Not code-blind and not a milestone result.

| # | Fix | Result |
| --- | --- | --- |
| 1 | #1327 (#1082): history inserted while the viewport fills the screen | **Pass.** Follow-up filed: #1340 (copies of the popup land in tmux scrollback) |
| 2 | #1322 (#1038): first-message edit warm-up is a reopen warm-up | **Pass** |
| 3 | #1320 (#867): kill switch names a stopped model-spawned sub-agent | **Pass** |
| 4 | #1319 (#1312): note for a denied write hidden by a later `exit 0` | **Pass** |

## Setup

- Binaries:
  - **installed**: the signed `~/.local/bin/corbanu` build of main `b0fa02105f` (#1322), `corbanu 0.1.48`. Used for checks 1–3.
  - **before**: the previously installed signed build `0e2c02529d`, which predates #1327, #1322 and #1320.
    Used for the before runs of 1 and 2.
  - **main**: #1319 merged after `b0fa02105f`, so the installed build doesn't have it. Check 4 ran on an unsigned
    dev build of main `d88d018ab2` (#1319); its before run used the installed `b0fa02105f`.
- Disposable `HOME`/`CORBANU_HOME` per run, `CORBANU_TEST_NO_NATIVE_KEYRING=1`, no vault in the test home. GLM 5.3 Flash on
  zai; the key came from the installed vault helper (`zai_key_3`) on the launch command only ([launch.sh](scripts/launch.sh)).
- Checks 3 and 4: Aggressive saved in `/security`, then `r` to restart; `/security` showed "Active in this session:
  Aggressive" ([aggsetup.sh](scripts/aggsetup.sh)).
- Check 2: Z.AI sends no warm-up (warm-ups are WebSocket-only), so this check used a loopback stand-in,
  [mockws.py](scripts/mockws.py): a custom provider with `supports_websockets = true` that answers `response.create`
  and logs whether each request was a warm-up (`generate: false`). Flow in [i2flow.sh](scripts/i2flow.sh).
- Leak scan: the captures and the run folders contain no Z.AI key (exact-value and key-shape scans: 0 files).

## 1. #1327: history goes to scrollback when the viewport fills the screen

A 100x12 pane. Prompt: "Write the numbers 1 to 60, one per line, each followed by a short fruit name. No other text."
Then `/` three seconds later, so the slash-command popup fills the screen while the reply is inserted.

- **Before** (`0e2c02529d`): reply lines are painted over the composer and popup
  ([before-screen-popup-open.txt](i1/before-screen-popup-open.txt)); lines 7–18 are lost from scrollback
  ([before-scrollback-popup-open.txt](i1/before-scrollback-popup-open.txt)).
- **After** (installed): the composer `› /` and the popup stay intact ([after-screen-popup-open.txt](i1/after-screen-popup-open.txt));
  all 60 lines are in scrollback ([after-scrollback-popup-open.txt](i1/after-scrollback-popup-open.txt)).
- Side effect, filed as #1340: in tmux's default `scroll-on-clear on`, each history batch inserted this way also
  leaves a copy of the popup in scrollback (three copies in this run, between lines 0/1, 17/18 and 44/45). With
  `scroll-on-clear off` there are no copies and all 60 lines are there
  ([after-scrollback-scroll-on-clear-off.txt](i1/after-scrollback-scroll-on-clear-off.txt)). The raw-append path clears from
  the viewport's top (row 0), which tmux treats as a screen clear and saves into history.
- Not changed by the fix: after Esc closes the popup, the composer stays at the top of the pane with blank rows below, and
  the end of the reply is only in scrollback, before and after ([before](i1/before-screen-popup-closed.txt),
  [after](i1/after-screen-popup-closed.txt)).

## 2. #1322: editing the first message

Flow: "first message alpha", reply; Esc Esc, Enter on the first message ("You're continuing from this point in a new
conversation"); append " edited", Enter; `/cost`. Stand-in log: [mock-requests.log](i2/mock-requests.log),
[mock-requests-skip.log](i2/mock-requests-skip.log).

| Run | Warm-ups sent (stand-in log) | Notice "Reopening or forking this conversation sent a warm-up request…" | `/cost` |
| --- | --- | --- | --- |
| before (`0e2c02529d`) | 2 (start, edit) | none | no reopen warm-up line ([before-05-cost.txt](i2/before-05-cost.txt)) |
| after (installed) | 2 (start, edit) | once, right after the edit ([after-03-editing.txt](i2/after-03-editing.txt)); not repeated after the reply ([after-04](i2/after-04-edited-reply.txt)) | "Reopen warm-up: 1 request … Sent when this conversation was reopened or forked" ([after-05-cost.txt](i2/after-05-cost.txt)) |
| after, repeat (`after-repeat-*`) | 2 | once | reopen warm-up line |
| after, `skip_reopen_warmup = true` (`after-skipb-*`) | 1 (start only) | none | 1 request, no reopen warm-up line ([after-skipb-05-cost.txt](i2/after-skipb-05-cost.txt)) |

The `after-repeat` run was meant as the skip run, but the setting never reached its config (the tmux server didn't pass
the variable), so it is a second "after" run. A further attempt reused that home and its stand-in log lines were lost
(the log file was replaced while the stand-in held it open); it is discarded, and its `after-skip start/end` markers at the
end of `mock-requests.log` belong to it. `after-skipb` is the valid skip run, on a fresh home and fresh log.

## 3. #1320: kill switch names a stopped sub-agent

Installed build, Aggressive, 150x45. Prompt (as AGG-R14): "Spawn a child agent and have it run: sleep 60; echo finished
> ks-child.txt". Allowed `spawn_agent_v1` once ([after-02](i3/after-02-spawn-question.txt)); child **Sagan** first asked
to read a skill file, which I declined ([after-03](i3/after-03-child-asks-skill-read.txt)); in Sagan's thread I asked it to
run only the command and approved it ([after-04](i3/after-04-child-command-prompt.txt)); back in Main, `/security`, `g`,
"Turn the kill switch on", Enter, while `sleep 60` ran.

- Main showed: "The security kill switch also stopped sub-agent Sagan (thread 01a1638b-dc50-…) and the commands it was
  running. /agent lists it as stopped by the kill switch; open it there to see its thread."
  ([after-05-kill-switch-on.txt](i3/after-05-kill-switch-on.txt))
- `/agent`: "2. • Sagan 01a1638b-dc50-… · stopped by the security kill switch" ([after-06-agent-list.txt](i3/after-06-agent-list.txt)).
- Sagan's thread: "Stopped: the security kill switch was turned on…" and "Turn stopped by the security kill switch."
  ([after-07](i3/after-07-child-thread.txt)). `ks-child.txt` was absent 75 s after the `sleep 60` would have finished
  ([after-08](i3/after-08-fixture-90s.txt)).
- No before run (the PR's snapshot test fails on the previous main).

## 4. #1319: note for a denied write hidden by a later exit 0

Aggressive. Prompt: "Run exactly this one shell command once and show me its output: echo x >> ../outside-notes.txt; echo
"exit=$?"". Approved with "Yes, proceed". Output in both runs: `/bin/bash: ../outside-notes.txt: Operation not permitted`,
`exit=1` (the command as a whole exits 0). `../outside-notes.txt` was not created.

- **Before** (installed `b0fa02105f`): no note after the run ([before-02-result.txt](i4/before-02-result.txt)).
- **After** (main `d88d018ab2`): "⚠ Security level Aggressive blocks writing `../outside-notes.txt`: it is outside the current
  folder…" right after the output ([after-02-result.txt](i4/after-02-result.txt)); the model's answer repeats it
  ([after-03-turn-end.txt](i4/after-03-turn-end.txt)).

No model step was declined in any run.
