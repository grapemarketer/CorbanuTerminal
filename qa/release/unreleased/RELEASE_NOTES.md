# Corbanu Terminal: notes for the next release

Notes for changes on `main` that are not released yet. The release worker moves
them into `qa/release/<version>/RELEASE_NOTES.md` and empties this file.

## Protocol change: `turn.completed` usage can be `null`

- **`corbanu exec --json` reports `"usage": null` when exec receives no
  matching usage report** (#290). Unknown usage is no longer reported as an
  all-zero usage object, so consumers can distinguish "not reported" from a
  measured zero. When usage is reported, the object is unchanged.
- **A non-null usage does not prove the provider reported this turn.** On a
  resumed thread, if this turn's response reports no usage, `usage` still
  holds the thread's earlier totals. When only some of a turn's requests report
  usage, the totals include only those.
- **Usage includes observed descendant work** (#290). Repeated cumulative
  reports are counted once. Every history-forked descendant makes the total
  `null`, as do a descendant with no report,
  a failed or timed-out ancestry read, or dropped events (`Lagged`). The selected
  thread's own totals are not a difference from the start of the invocation.
  Detached children can incur further usage after this snapshot.
- **Default multi-agent spawns copy history.** In MultiAgentV2, `spawn_agent`
  defaults to `fork_turns: "all"` when omitted, so these default multi-agent runs
  report `usage: null`. A non-null total can be larger than in earlier versions
  because it includes descendant work rather than only the root's own total.
- **Context estimates are not provider reports.** Compaction, rollback and
  full-window estimates keep the last measured report, or `null` if none exists.

### Compatibility

- **Consumers that read token counts from `turn.completed.usage` without a
  null check** will fail on turns with no reported usage and must be updated.
  The TypeScript SDK in `sdk/typescript` handles `null` from this release:
  `TurnCompletedEvent.usage` is `Usage | null`, and `Thread.run()` returns
  `usage: null` when exec receives no usage report.

## Protocol change: `refused` command status

- **A refused command now says "Refused", not "Approval declined".** When a
  policy or setup check stops a command without anyone being asked to approve
  it, the command's cell shows `└ Refused: <reason>` (the transcript says
  `✗ Refused`), and `corbanu exec` prints `refused:` followed by the reason
  (#822, #851, #900).
- **New status value.** The app-server protocol's `commandExecution` item can
  now have `status: "refused"` (alongside `inProgress`, `completed`, `failed`
  and `declined`); `corbanu exec --json` reports `"refused"` too. `declined`
  now means only that an approval request ended without permission (denied,
  timed out or cancelled). `refused` usually means the command did not run,
  but a network-policy denial can arrive after it started, so don't assume it
  never ran. The refusal reason is in `aggregatedOutput` (`aggregated_output`
  in `exec --json`); the exit code is a placeholder, not a process exit. File
  changes (`fileChange`) have no `refused` status.

### Compatibility

- **App-server clients and SDKs generated before this change** don't know
  `refused` and can fail to read an `item/completed` that carries it. Regenerate
  from this release's schema (`codex-rs/app-server-protocol/schema`). The
  TypeScript SDK in `sdk/typescript` (which reads `corbanu exec --json`)
  already includes it.
- **The Python SDK (`sdk/python`) does not include it.** It stays on its pinned
  upstream runtime (`openai-codex-cli-bin==0.144.4`), which has no `refused`.
  A Python client pointed at a Corbanu binary (`CodexConfig.codex_bin`) can
  fail on a refused command.
- **Saved history.** Default sessions don't save command status, so older
  builds resume them as before. Threads started with the experimental
  app-server `historyMode: "paginated"` (and their subagents) do: an older
  build can't read the history of such a thread once it has a refused command
  (`thread/resume` with turns, `thread/turns/list` with full items,
  `thread/items/list` and `thread/turns/items/list` fail with "failed to
  deserialize stored thread item"). Newer builds read older history unchanged.

## `/cost`

- **Codex Auto Review is named as unpriced.** On an OpenAI API key, requests
  from the automatic approval reviewer still show `no price available` (no
  estimate, never $0), and the next step now adds "OpenAI doesn't publish a
  price for Codex Auto Review." (#913).
- **Editing the first message counts as a reopen.** Editing the first message
  of a saved conversation (Esc Esc) now sends its warm-up as a reopen warm-up,
  as editing a later message already did: `/cost` lists it as "Reopen
  warm-up", Corbanu says once it has been sent, and `skip_reopen_warmup = true`
  skips it (#1038).
- **Realtime-only threads can skip the warm-up.** `skip_startup_warmup = true`
  (in `config.toml`, with `-c`, or in `thread/start`'s `config`) sends no
  startup warm-up, new or reopened. App-server clients that start a thread
  only for `thread/realtime/start` should set it: the warm-up on the default
  model could cost several times the realtime session (#1045).
- **Realtime sessions show their tokens.** A realtime WebSocket session now
  records the input, cached-input, output and total tokens OpenAI reports at
  the end of each response, and `/cost` shows them. Its cost stays unknown
  (`no price available`, never $0): Corbanu has no realtime price table, so
  no rate is applied even when the session's model has a catalogue price.
  Sessions recorded earlier, and WebRTC calls, still show their tokens as
  unknown (#889, D223).
- **A day with an unrecorded request says its totals may be incomplete.**
  When accounting fails to record a request (the turn already warns, for
  example with a clock far behind the ledger), Corbanu now keeps a small
  marker for that conversation and the UTC days the request could have been
  sent on, and `/cost` for such a day says "Totals for this day may be
  incomplete" under its heading (a range or period that includes it says so
  too). A home's ledger moves to format 7 the first time this happens, so
  older builds then treat it as written by a newer build and leave
  collection off. A home with no ledger yet gets none: its `/cost` already
  shows as unavailable (#314, D228).

## `/security` wording

- **Revoke all names its real scope.** After "Revoke all active authority",
  the confirmation now says it was revoked "in every session of this Corbanu
  Terminal on this Corbanu home", as its review says, instead of "in this
  session". A saved kill switch, on or off, also says that other Corbanu
  Terminals on the home take it within about a second (#1035).
- **A stale inspector view is not called degraded.** When the inspector's facts
  are more than 30 seconds old and nothing else is wrong, its top line reads
  "◌ Moderate set; this view is stale (status observed 33s ago)" instead of
  "▲ Moderate degraded"; press `r` to refresh. A stale view still never shows
  the green "Protected" badge (#1036).

## `/security`: windows without a session yet, and remote app servers

- **Moderate before the window's session starts.** Confirmed in a window whose
  session hasn't started, Moderate's Saved screen no longer says the session
  stays Permissive until a restart: that session starts under Core's Moderate
  rules; the credential broker still waits for the restart (#1047).
- **Remote app server windows.** In a window whose sessions run on an app
  server outside Corbanu Terminal (a remote server or the local daemon),
  `/security` no longer says the session starts here, says the level reaches
  that server only if it runs on this Corbanu home, and the level picker
  doesn't offer "restart now", since Corbanu Terminal starts against such a
  server only under Permissive (#1047).

## Aggressive: contained Claude panes and the macOS keychain

- **Contained Claude panes follow the stricter level right away** (#807). A
  contained pane is now kept away from the macOS keychain services as soon as
  Aggressive is saved in `/security` or takes effect in the live session,
  before any restart, as agent commands already were. On Linux, a pane's
  user-namespace rule (#891) follows the live level the same way.
- **Approved commands stay in the sandbox under the keychain rule.** On macOS
  the keychain rule by itself now keeps escalated and allow-listed commands in
  the sandbox. Aggressive already kept them there through its other rules, so
  nothing you see changes. Permissive and Moderate are unchanged.
- **Claude panes and `/permissions` also follow Core's live level** (#1468).
  Claude panes (outside containment) and permission changes are refused while
  the live session runs a protected level (Aggressive, for `/permissions`),
  not only when it is active or saved. On macOS, `/status` no longer says
  approved commands can leave the sandbox when a permission profile keeps the
  keychain away (they can't).

## Named accounts (behind `named_accounts`)

- **App servers in homes with only the default login no longer wait for a
  conversation.** With named accounts on, a standalone app server or daemon
  held back its own ChatGPT requests (apps, plugins, rate limits, the model
  catalog) until a conversation started, even in homes without named
  accounts. It now does that only when the home has a named OpenAI sign-in
  account or `[provider_accounts]` selects one. A server started without one
  starts holding them from the first conversation on an account added while it
  runs, and never goes back to the default login (#1085).
- **`/status` names the account only where named accounts exist.** With named
  accounts on, the provider line read "OpenAI · account default" (or
  "Z.AI · account default") in every home. It now names the account only when
  that provider has a named account in the home, or the session is on or used
  one; otherwise the line is what it was without the feature (D86).

## `corbanu login status`

- **No part of an API key is shown.** With an API-key login it used to print
  the key's first 8 and last 5 characters. It now prints
  `Logged in using an API key - API key configured`, plus
  `(fingerprint <12 hex>)` when the home's vault has its fingerprint salt. That
  is the same per-home fingerprint a named OpenAI account holding the key
  shows (REG-02).

## Custom providers on OpenAI's regional API hosts

- **Multi-agent turns work on more OpenAI hosts.** A custom
  `wire_api = "responses"` provider whose `base_url` is
  `https://<region>.api.openai.com/v1` (`ae`, `au`, `ca`, `eu`, `gb`, `in`,
  `jp`, `kr`, `sg`, `us`) or `https://api.openai.com./v1` no longer gets every
  turn refused with HTTP 400 `collaboration.followup_task is reserved` (#863,
  #820). Its provider id, credentials and billing are unchanged. Proxies and
  gateways in front of OpenAI still get the provider-neutral tools.

## Task Node evidence receipts

- **An evidence receipt that confirms nothing now fails.** If Task Node answers
  `corbanu tasknode task evidence` or `verification respond` with success but
  no event id, transaction hash or CID, the command exits 1 with
  `task_evidence_receipt_unconfirmed`, the original receipt and
  `evidenceGateOpen` (whether the task still accepts that evidence). It used to
  exit 0 (#1280, reported by @0xzoz).
- **Confirmed receipts name what was recorded.** `pfterminalLifecycle` now has
  `submissionRef` (event id, transaction hash and/or CID), reports an empty
  `currentVerificationRequest` (`{}`) as `null`, and only says the initial
  evidence gate is closed when the refreshed task says so. If a task stays
  submitted with no verification request, report the task id and
  `submissionRef` to Task Node operators rather than cancelling it.

## Moderate and Aggressive: other git worktrees' `.git` files stay read-only

- **Agent commands can no longer rewrite another linked worktree's own `.git`
  file** (#675). The protected-path rules already kept every linked worktree's
  folder in the shared repository read-only (#650). Each worktree's own `.git`
  file, which says where its git folder is, now stays read-only too, so it
  can't be pointed at a planted git folder whose config would run code in your
  later git commands there. Git in the current worktree and in the main
  repository works as before. `git worktree repair` of another worktree now
  fails for agent commands as well, as `git worktree move` and `remove` already
  did. Permissive is unchanged.
- **The same holds for a submodule's linked worktrees** (#1468): the `gitdir`
  in `.git/modules/<module>/worktrees/<name>` and the `.git` file it names stay
  read-only too, and a `gitdir` spelled `.GIT` counts where the volume ignores
  case.

## GLM 5.2 on Z.AI and Ambient: "medium" now runs as Standard

- **A GLM 5.2 session set to `medium` now runs at Standard, as its label
  says** (#1266, #1270, thanks to @citadelculture). On the Z.AI (`zai`)
  `glm-5.2` and Ambient `z-ai/glm-5.2` routes, an explicit
  `model_reasoning_effort = "medium"` (the catalogue's **Standard**, chosen in
  Spawn or set in config) used to be rewritten to `xhigh` when the config
  loaded or a session resumed, so it quietly ran as Deep. It now stays
  Standard. Deep and an unset effort behave as before. A session that already
  ran as Deep under an earlier build resumes as Deep; pick Standard again. The
  Corbanu Plan route for GLM 5.2 still maps `medium` to `xhigh` (#1476).

## OpenRouter Owl Alpha is retired

- **`openrouter/owl-alpha` no longer works** (#128, #1486). OpenRouter lists no
  endpoints for it (checked 2026-10-28), so a request to it is expected to fail
  at OpenRouter. A config that saved this ID still resolves to its own hidden
  entry and is never switched to another model; its description now says it is
  retired. Pick a current model with `/model`. The earlier "explicit legacy IDs
  remain compatible" note no longer covers this ID. The other retained IDs
  (Grok 4.6 and 4.5, DeepSeek V4 Pro and V4 Flash 0731 on OpenRouter, and
  GPT-5.5 on OpenAI) are still served and unchanged.

## Chat Completions and Anthropic: earlier MCP calls keep their own name

- **An MCP tool's earlier calls no longer replay as a built-in's** (#688). On
  providers without tool namespaces, a call recorded for an MCP tool that the
  request no longer offers (its server was removed, or the 128-tool limit left
  it out) used to replay under its bare name, such as `shell`, which a
  built-in tool can share. It now replays under the MCP tool's flat name
  (`mcp__server__tool`), or a hashed form if another tool has that name.

## `NO_PROXY` IP ranges apply to WebSocket connections

- **WebSocket connections now honour IP-range entries in `NO_PROXY`** (#722,
  D247), such as `172.16.0.0/12`. A connection to an IP address inside a
  listed range now goes direct, as HTTP requests already did, instead of
  through `HTTP_PROXY`/`HTTPS_PROXY`. Host names are matched by name only (they
  are not resolved to check ranges), as before.

## A timed-out network prompt for remote work closes

- **The approval prompt for a remote network request now closes when the
  request times out** (#722, D279). An unanswered request is still refused
  after 10 minutes (plus the hook timeout) and the command still ends as
  failed; before, its prompt stayed on screen and answering it did nothing.
- **App-server clients:** the server resolves such a pending
  `item/commandExecution/requestApproval` request itself and sends
  `serverRequest/resolved`, as it already does when a turn ends. Any command
  approval whose waiter in the core goes away unanswered is resolved the same
  way.

## Interrupting a tool on a busy machine keeps its elapsed time

- **The model now always sees how long an interrupted tool ran** (#1467,
  #1448). On a loaded machine, an interrupted turn could miss its 100 ms grace
  period. The interrupted tool's output (`Wall time: N seconds` /
  `aborted by user`) was then replaced by a bare `aborted`. That output is now
  recorded in this case too, timed to the moment of the interrupt.
- **A tool that had already finished keeps its real result** (#1536, D244).
  In the same rare case, a tool that completed just before the interrupt (for
  example a patch that was applied) was reported to the model as aborted. Its
  real output is now recorded instead.

## Windows non-admin sandbox warning

- **The warning now matches what the non-admin sandbox does** (#1506, D264). It
  no longer says commands can delete or move files you own outside your
  workspace; since #705 they can't, except in low-integrity places. It now says
  what it doesn't stop: reading your files, network access by programs that
  ignore proxy settings, and a few ways of starting programs outside its job.

## `corbanu resume --last` reopens the conversation you quit from

- **`resume --last` now opens the conversation that was on screen when you
  last quit** (#1412, D243). Opening an older conversation with `/resume`
  doesn't change its "updated" time until you send a turn, so `resume --last`
  used to reopen the conversation with the newest turn instead. Corbanu now
  records the conversation on screen when you quit and reopens it, as long as
  it is in the same scope (folder, provider, not archived) and no conversation
  there got a newer turn after you quit. Otherwise it still picks the newest
  turn, as before. The `/resume` list order and `fork --last` are unchanged.

## Corbanu API with no key: `/status` and first-run setup

- **`/status` no longer claims a missing Corbanu API key.** Under Moderate or
  Aggressive, with Corbanu Plan chosen but no key, `/status` said
  `Corbanu Plan API key configured` while `/providers` said not configured. It
  now says `Corbanu API not configured (set it up in /providers)` (#1563).
- **First-run setup for a chosen Corbanu Plan opens the provider list.** It used
  to open an OpenAI-worded key prompt that ended in
  `Provider API-key selection is no longer available`. The list now starts on
  **Corbanu API**, whose wallet and API-key setup runs after **Done**.

## A force-stopped turn still reports its file changes

- **The turn's file-change summary is sent when an interrupt force-stops a
  turn** (#1604, D282). When a turn misses the interrupt grace period and is
  stopped, it now sends the summary of the files it changed, as a turn that
  ends normally does, so the status line stays current.
- **A turn stopped while recording a search result still marks the
  conversation as using outside content** (#1536). The check now runs before
  the result is recorded instead of after it.

## Telegram `/status`: token estimates are not a thread total

- **`/status` no longer shows unknown usage as a free thread.** After
  compaction or rollback on a thread whose provider never reported usage, it
  said `Thread total: 0 tokens (0 in, 0 out)`. It now says
  `Thread total: not reported by the provider`, and `Context` shows the token
  estimate (#1570).
- **A full context window is shown as context, not as the thread's cost.**
  When the window is exceeded, `Context` shows the full window and
  `Thread total` stays the provider-reported usage, computed from the
  provider's counters, which the fill now keeps (#1583). After compaction,
  `Context` shows the new, smaller estimate rather than a full window, and
  `/new` starts the new thread without the previous thread's total.
