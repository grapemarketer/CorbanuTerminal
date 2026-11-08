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
- **Usage includes observed descendant work** (#290, #1601, thanks to
  @white-guy-01). Repeated cumulative reports are counted once. Every
  history-forked descendant makes the total `null`, as do a descendant with no
  report, a failed or timed-out ancestry read, or dropped events (`Lagged`).
  The selected thread's own totals are not a difference from the start of the
  invocation, so its first report needs no inherited baseline: a resumed
  thread whose first report follows a compaction estimate keeps a known total
  (#1637). A history-forked subagent that is not saved to disk yet is now also
  recognized as a fork (the app server's `thread/read` returns its
  `forkedFromId`), so it too makes the total `null` (#1637).
  A resumed (non-fork) descendant whose first report carries in earlier usage
  (its total differs from its last request) makes the total `null` too
  (#1637, #1750, thanks to @citadelculture).
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

## Slash commands typed while the session starts

- **A slash command entered during startup always waits for it** (D376,
  #1748). While the header still reads `model: loading`, a command such as
  `/model` is queued and runs once startup completes, like an ordinary prompt.
  Before, it was queued only when its keys arrived in one quick burst; typed
  a little more slowly (so the command list opened before Enter), it ran at
  once and was refused with "Model selection is disabled until startup
  completes."
- **`/quit` and `/exit` don't wait** (D390, #1833). Typed during startup,
  they exit at once instead of joining the queue, so a startup that hangs can
  still be left without Ctrl+C. Every other command still waits.

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

## Moderate: waiting on and starting agents after untrusted content

- **Waiting on an agent never asks** (D320, #1653). On Z.AI and other
  providers without namespace tools, `wait_agent_v1` used to ask (or, with
  approvals off as in `corbanu exec`, was refused) as "a tool the security
  level has not classified" once untrusted content had entered the session,
  so a parent couldn't wait for its child. It now goes ahead, as `wait_agent`
  already did.
- **Starting an agent asks, on every route.** After untrusted content under
  Moderate (and Aggressive), `spawn_agent` (and `spawn_agent_v1`,
  `spawn_agent_plaintext`) asks for your approval, and the prompt says it is
  "a request to start a new agent". With approvals off it is refused. This is
  new on the OpenAI Responses route, where `spawn_agent` used to go ahead.
- **The `_v1` agent aliases act like their usual names** (D334, #1702). On
  Z.AI, Chat Completions and Claude-style routes, `send_input_v1` and
  `close_agent_v1` now go ahead after untrusted content, as `send_input` and
  `close_agent` do; before, they asked (or were refused with approvals off) as
  unclassified tools.
- **Restarting a finished agent asks, like starting one** (D334). After
  untrusted content, `resume_agent` (and `resume_agent_v1`) asks for your
  approval on every route, and so does a MultiAgentV2 `followup_task` (and
  `followup_task_plaintext`) to an agent that isn't running, since it starts a
  new turn there; the prompt says it is "a request to restart a finished
  agent", and with approvals off it is refused. A follow-up to an agent with a
  turn running goes ahead. Before, `resume_agent` and `followup_task` went
  ahead on every route that offers them, and `resume_agent_v1` asked as an
  unclassified tool.
- **The same rules apply under Aggressive** (D335).

## Agent tools act only on the caller's own agents

- **A sub-agent's `list_agents` shows only itself and its own agents**
  (D320, #1756, #1767, thanks to @citadelculture). With MultiAgentV2,
  `list_agents` used to list every live agent, including the caller's parent
  and siblings and other root threads' agents; it now lists the caller and
  the agents it started (and theirs), loaded or not. This is the intended
  tightening of D320. `close_agent` and `send_input` (and their `_v1`
  aliases) likewise refuse an agent the caller didn't start, directly or
  through its own agents.
- **`wait_agent` waits only for the caller's own agents** (#1744, #1701,
  thanks to @citadelculture). `wait_agent` (and `wait_agent_v1`) could wait
  for any loaded agent, including another root thread's, and return its final
  status. It now refuses an agent the caller didn't start, directly or
  through its own agents, with "wait_agent can only wait for agents spawned
  by this agent or by its sub-agents". With MultiAgentV2, `wait_agent` used
  to report every live agent with its last result; it now reports only the
  caller's agents.

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
- **Revoking there no longer offers a restart that is refused.** After "Revoke
  all active authority" or a kill-switch change in such a window, the grants
  and kill-switch view no longer offers `r restart now`, and says why:
  `Restart now is not offered: this window's sessions run in an app server
  outside this Corbanu Terminal.` (#1284, #1094, thanks to @citadelculture).

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
- **`/status` no longer reads the vault itself.** The summary of a named
  ChatGPT sign-in account (kind, email hint, plan) is now read in the
  background and cached. Until it is known, `/status` shows just
  `account <name>` and doesn't treat the account as removed. A sign-in change
  made by another process shows on the next `/status` (each one starts a
  refresh) (#1572, #1251).

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

## OpenRouter Tencent Hy3 Free is retired

- **`tencent/hy3:free` is no longer listed in `/model`** (#1658, #1662, thanks
  to @bradsbots). OpenRouter lists no endpoints for it (checked 2026-11-07), so
  a request to it is expected to fail at OpenRouter, and it is no longer
  allocated automatically. A config that saved this ID still resolves to its
  own hidden entry and is never switched to another model; its description now
  says it is retired. Pick a current model with `/model`. The paid
  `tencent/hy3` is not in the catalogue.

## Remote work: every network refusal ends the command as failed

- **A network request the remote environment's proxy refuses on its own now
  ends the command as failed**, with the reason, as it does locally (#722,
  D278). This covers blocked sites, local or private addresses, requests
  limited mode doesn't allow, and requests to sites not on the allowlist when
  no managed network rules are set. Before, these were refused but the
  command could still end normally.
- **Only stricter:** the remote side can only report a refusal; nothing it
  sends can allow a request. Remote environments running an older version
  don't send these reports, and behave as before.
- **Hidden characters can't disguise a remote site's name** (#722): bidi
  controls and zero-width characters are removed from the site named in a
  remote refusal, and a remote request whose site name contains them is
  refused without asking.

## Chat Completions and Anthropic: earlier MCP calls keep their own name

- **An MCP tool's earlier calls no longer replay as a built-in's** (#688). On
  providers without tool namespaces, a call recorded for an MCP tool that the
  request no longer offers (its server was removed, or the 128-tool limit left
  it out) used to replay under its bare name, such as `shell`, which a
  built-in tool can share. It now replays under the MCP tool's flat name
  (`mcp__server__tool`), or a hashed form if another tool has that name.
  MCP servers configured without the `mcp__` prefix get the same treatment
  (`server__tool`); only built-in tool namespaces keep the bare name.
- **MCP servers without the prefix can't take a built-in's namespace** (#1655).
  With `features.non_prefixed_mcp_tool_names`, a server that would be exposed
  without `mcp__` under a built-in tool namespace (`web`, `clock`, `skills`,
  `collaboration`, `image_gen`, `memories`, `multi_agent_v1`, or the configured
  multi-agent namespace) is now rejected when the config loads and by
  `corbanu mcp add`, with a message saying how to rename it. Skill dependency
  install skips such a server, and plugin servers keep the `mcp__` prefix.
  This includes servers from a trusted project's `.codex/config.toml`: with the
  feature on, such a project's config doesn't load until the server is renamed
  or left out of `server_names`. Disabled servers are skipped unless they are
  listed in `server_names`.
- **Nor can they take a namespace the Responses API reserves** (#1666). The
  same check now also refuses `api_tool`, `browser`, `computer`, `container`,
  `file_search`, `functions`, `multi_tool_use`, `python`,
  `python_user_visible`, `submodel_delegator`, `terminal` and `tool_search`,
  the names `features.multi_agent_v2.tool_namespace` already can't use.
  App-server config writes and MCP imports from other agents apply it too.
  `corbanu mcp add` now suggests another name to add (#1679) instead of
  `corbanu mcp remove` for a server that isn't configured. The warning for a
  skipped skill dependency now says which kind of reserved namespace it would
  take and no longer suggests "limiting" `server_names`.
  An MCP import from another agent that skips such a server now says it was
  not imported and suggests adding it under another name, instead of
  `corbanu mcp remove` (#1681, #1738, thanks to @citadelculture).

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

## Windows sandbox: commands get a temp folder of their own (D377, #304)

- **The default (elevated) Windows sandbox no longer lets sandboxed commands
  write anywhere in your `%TEMP%`.** Each Corbanu session makes a folder of
  its own there (`corbanu-sandbox-tmp-…`), open only to you, SYSTEM and
  Administrators, and its sandboxed commands get it as `TEMP` and `TMP` (and
  as `TMPDIR` when that named `%TEMP%`). Only that folder is granted to the
  sandbox. Before, the sandbox's accounts kept lasting write access to all of
  `%TEMP%`: other programs' temp files, and other sessions'.
- **Upgrading needs no setup rerun.** The first time a session uses
  `%TEMP%`, it removes the grant older versions left there, as you (no UAC
  prompt).
- **Cleanup.** The folder is removed when the session ends; one a crashed
  session left behind is removed by a later session.
- On native Windows CI, in an administrator's and a normal session, Python
  (temp files, venv and pip), cargo, Node and npm, Windows PowerShell and
  PowerShell 7, git and Git Bash, and MSBuild work as before.
- Not changed: the non-admin sandbox still uses `%TEMP%` itself.

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
- **The same holds with `forced_login_method = "api"`.** With Corbanu Plan as
  the provider, setup now opens on the **Corbanu API** row (only that row and
  **Done**) instead of the OpenAI key prompt (#1584, #1571, thanks to
  @bradsbots).

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
  estimate (#1570, #1586, thanks to @sendoeth).
- **A full context window is shown as context, not as the thread's cost.**
  When the window is exceeded, `Context` shows the full window and
  `Thread total` stays the provider-reported usage, computed from the
  provider's counters, which the fill now keeps (#1583; #1586, thanks to
  @sendoeth). When the first update the bridge sees for a thread is a fill
  (for example after a bridge restart), `Context` shows the full window if
  the thread had already reached it (#1638), and otherwise what the fill
  added.
  After compaction, `Context` shows core's token estimate, and `/new` starts
  the new thread without the previous thread's total.

## Usage totals survive a full context window

- **A full context window no longer resets the thread's usage totals**
  (#1583, #1577, thanks to @bradsbots). When a request or a compaction hit the
  context window, the thread's input, cached-input, cache-write, output and
  reasoning counters were reset to zero, so `corbanu exec --json` usage, the
  TUI's `/status` and the app server's `thread/tokenUsage/updated` totals then
  counted only usage after the fill. They now keep the earlier reports. The
  total token count is still set to the full window at a fill, and the context
  indicator is unchanged.

## Chat Completions: replies stay messages

- **A JSON answer that mentions `"function_call"` is a message** (#1582,
  #1581, thanks to @bradsbots). Such a reply used to be read as a broken tool
  call and fail with `malformed function-call JSON as assistant text`, then be
  retried, even after it had been shown. Only JSON whose top-level `type` is
  `function_call` or `custom_tool_call` counts as a call.
- **A reply that opens with such JSON and goes on in prose is a message**
  (#1659, #1654). It used to fail with `trailing characters` after five
  retries.
- **A reply that opens with JSON-like text that does not parse is a message**
  (#1728, #1660, thanks to @bradsbots), for example `{"function_call": auto} …`.
  It used to fail with `malformed function-call JSON` after five retries. A
  broken reply that names a tool-call `type` still counts as a broken call and
  is retried.
- **Text already shown stays a message** (#1656, #1608, thanks to
  @bradsbots), even if it turns out to parse as a tool call.
- **A `null` usage counter no longer drops its chunk** (#1589, #1588, thanks
  to @bradsbots). When that chunk was the last one, the reply lost its last
  text, its finish reason and its usage, so a reply cut off at the output limit
  ended as if it had finished normally. A `null` counter now reads as 0 for display.

## Vercel GLM 5.3 output caps

- **`zai/glm-5.3` and `zai/glm-5.3-flash` on Vercel now cap output at
  128,000 tokens**, Z.AI's documented maximum, as the Z.AI and OpenRouter rows
  already do (#1585, #1567, thanks to @bradsbots). They were 1,000,000 and
  131,000.

## Agents and reviews

- **A message handed to a running agent is delivered once** (#1579, #1545,
  thanks to @bradsbots). If recovery ran just after a live sender handed a
  message over, it could send the same message again.
- **A review cancelled before it starts stays cancelled** (#1574). A `/review`
  or automatic approval review whose cancellation came before its session
  started could still start it.

## Moderate and Aggressive: more command lines ask after untrusted content

After the agent has read untrusted content, these lines used to run without
asking and now ask first:

- `rg --hostname-bin=PROG`, which runs a program (#1600, #1573, thanks to
  @bradsbots).
- zsh's `=name`, which names the program `name` on `PATH`, as a redirection
  target (`echo x >> =mytool`; #1602, #1573, thanks to @citadelculture), and
  as a writer's operand (`tee -a =mytool`) or in `eval`, `trap` and `alias`
  text (#1651, #1620).
- Writes to a relative file such as `git log --output=.zshenv` or
  `git config -f .zshenv …` when the folder it lands in can't be known: after
  `cd "$X"` or another `cd` the check can't follow, after a wrapper's folder
  option (`sudo -D ~`, `env -C ~`, `docker exec -w ~`), or in a function, trap
  or loop that runs after a later `cd` (#1599, #1309, thanks to @bradsbots;
  #1651, #1606, when an option value spelled `git` comes before the wrapper's
  folder option).
- `git grep -O<pager>` and `--open-files-in-pager`, which run a program on the
  matching files (#1527, #1516). On Windows, also git named by its full path
  (#1527) and git's dashed helpers (`git-grep.exe`, `git-clone.exe`) (#1598,
  #1528).
- `-O`, `-u`, `-c`, `--open-files-in-pager`, `--exec`, `--template` and
  `--config` after a git subcommand that isn't one of git's own commands,
  since an alias or an autocorrected typo can be `grep`, `clone`, `init` or
  `push` (#1598, #1528, thanks to @Holic101, with parts of
  @wizbubba1's #1597), and program-running options after built-ins that older
  gits don't have, such as `refs` or `hook` (#1647, #1635).
- git commands cut short by an unquoted bracket the shell reads as text
  (`git -C { -c core.pager=sh log`; #1525, #1515), and git options that brace
  expansion produces (`git clone r d {-q,--upload-pack=./x}`; #1644, #1636).
- zsh's `>!`, `>>!`, `&>!` and `>&!` clobber redirections, read as writes
  (`echo x >! ~/.zshrc`; #1640, #1607).
- Commands in `eval`, `trap` and `alias` text, which now get every check
  the same command gets on a line of its own (D341, #1698, #1685): writes
  to shell start-up files (`trap 'tee -a ~/.zshrc' EXIT`,
  `eval 'tee -a ~/.zshrc'`), output options (`curl -o`, `time -o`), sending
  a file (`curl -d @f`), recursive reads of your home, scripts it runs
  (`./x.sh`, `source`, `python3 x.py`), a wrapper's folder option
  (`eval 'sudo -D sub ./run.sh'`), and, in an alias, git settings that run
  code. Before, only some of these were checked there. A `cd` in such text
  isn't followed: the folder it leaves counts as unknown. Ordinary text such
  as `trap 'rm -f x' EXIT` or `alias ll='ls -l'` stays quiet.
- Later uses of variables set in such text
  (`eval 'F=~/.zshrc'; tee -a "$F"`), settings there of variables that run
  code (`eval 'export GIT_PAGER=sh'`), and code written into the shells'
  tables of code run by name (zsh's `functions`, `commands` and alias
  tables, bash's `BASH_ALIASES` and `BASH_CMDS`), which is now checked as a
  line of its own (`functions[f]='tee -a ~/.zshrc'; f`; D341, #1698,
  #1685, #1704). Ordinary settings such as `eval "x=1"` or
  `aliases[ll]='ls -l'` stay quiet.
- A word the check can't see into (`$o`, `$(cat f)`) where a command that
  can write code into those tables still reads its options:
  `o=$(cat f); zstyle $o :x y functions`, `zstyle -a $(cat f)`,
  `zselect -t 0 -r 0 -A $n`, `zsystem flock -t 0 -f $n file` (#1704).
  `print`, `printf` and `getopts` are now read this way too. So an
  unquoted variable, `$(…)` or pattern where `print`, `printf`, `set`,
  `read`, `mapfile`, `vared` or `getopts` read their options or the
  name they write can now ask (`print -r $(cat f)`,
  `set ?A commands ls ./x`, `read -d $(cat f)`,
  `getopts $(cat f)`), and so can a quoted one with more words after it
  (`printf "$fmt" x`). `print -r -- $x`, `set -- $(cat f)`,
  `printf "$fmt"`, `print -l *.png` and `wait $pid` stay
  quiet.
- zsh's `stat` builtin, which can write those tables (`stat -A functions
  x`), now asks only when the same action may load it: a line of it runs
  `zmodload` or names `zsh/stat` (`zmodload zsh/stat; stat -f %m $(cat f)`
  asks, before or after the `stat`), holds a brace range, or runs bash's
  `enable` or a command named by a variable, command output or a pattern.
  zsh starts without it. Elsewhere `stat` is the system program, so macOS
  `stat -f %m $f` stays quiet. A module loaded by your zsh start-up files,
  or by an earlier command in the same long-running shell session, isn't
  seen (D410, #1704). `zstat` is always checked.
- A program named by a path, or a shell or interpreter, run after a leading
  redirection (`>log ./x`, `2>log sh x`, `2>&1 ./x`, `<<EOF ./x`,
  `sudo >log ./x`, `env >log ./x`), also in `eval` text (`eval '>log ./x'`),
  where the check used to read the redirection as the command (#1719,
  #1482, thanks to @darkmatter-sudo). `ls >log` and `2>/dev/null ls` are
  read as before.
- `sudo -A`/`--askpass`, `sudo -e`/`--edit` and `sudoedit`, which run a
  helper or editor named by the environment, also in `eval` text
  (`eval sudo -A ./x`) (#1719, #1482, thanks to @darkmatter-sudo).
- A command given as an argument list whose elements up to the program
  don't each read as one word (`["true ./x"]`, `["true;./x"]`,
  `["env", "A=1 ./y", "./x"]`) (#1719, #1482, thanks to @darkmatter-sudo).
  A shell's `-c` script is read as before. This doesn't apply under Windows
  path rules, where program paths hold spaces.

One line asks less: `git add src/{a,b}.rs && git commit -m "msg"` no longer
asks just because a quoted message follows the brace word (#1580, #1514,
thanks to @grapemarketer). A line with many `sed` commands and long output
names is now checked within a fixed size budget; past it, its output files
count as one unknown file, the cautious reading (#1587, #1132, thanks to
@0xzoz).

## Bare git repositories a command plants

- **New advice in `docs/sandbox.md`** (#1775). A sandboxed command can make a
  bare repository in any folder it may write to, and git run there or in a
  folder below it later runs what its config names. Corbanu Terminal
  doesn't check for these, on Windows or elsewhere. The guide recommends
  `git config --global safe.bareRepository explicit` (git 2.45 or later)
  and explains what it doesn't cover.

## Windows sandbox

- **The default (elevated) sandbox's commands can't break away from their
  job** (#1639, D256, #1507). Cancelling or a timeout now ends every process
  they start in their job; a program that asks to break away gets "access
  denied" unless it retries without the flag. The non-admin sandbox already
  worked this way.
- **Default (elevated) sandbox: each command gets its own identity** (D324,
  #1717, #1648), as non-admin sandbox commands already do. Commands running
  at the same time, for example in different projects, can no longer write
  to each other's processes or the helper processes that start them, or
  start programs under them. Projects are still not fully kept apart: a
  command can still borrow another command's access and write in its
  folders (#1560), or, by racing another command's start-up, run code
  outside its own sandbox restrictions (#1721), and can still read and end
  the other's processes. So still avoid running commands
  in several projects at once (see `docs/sandbox.md`).
- **Commands in the default sandbox can no longer read a non-admin sandbox
  command's memory** (D429, #1803). A default (elevated) sandbox command
  could read the memory of a non-admin sandbox command running at the same
  time, such as secrets in its environment, its output and what was typed at
  its prompts. Other accounts (except SYSTEM and administrators using debug
  rights) can now only wait for a non-admin command's processes. One
  non-admin command can still read another's memory (#1838), so still avoid
  running commands in several projects at once.
- **Profiles the Windows sandbox can't enforce are refused, and say why**
  (D331, #1762). A profile with a writable folder inside a read-only one is
  now refused for the agent's commands in the non-admin sandbox too, as it
  already was in the default sandbox and for app-server `command/exec`; in
  our measurements this changes the outcome only under Permissive. On both
  sandboxes the message now names both folders, notes that the protected
  `.git`, `.codex` and `.agents` folders count as read-only, and says to
  change the profile so that no writable folder lies in a read-only one. In
  the non-admin sandbox, `command/exec` (and the exec server) now refuse a
  profile that blocks or limits reading files with a clear message that
  says how to switch to the default sandbox, instead of an unclear error
  from the sandbox.
- **Non-admin sandbox: a working folder reached through a junction no
  longer bypasses a read-only folder** (#1780, #1792). With the working
  folder given as a junction, a read-only folder given by its real path was
  writable, and so was a read-only folder spelled through a junction inside
  a working folder given by its real path. Read-only paths are now matched
  to writable folders by their real (link-resolved) path too. When one
  profile marks the same folder read-only under one spelling and writable
  under another, read-only wins.
- **A writable folder inside a read-only one is refused through links too**
  (#1801, D331). D331's refusal compared folders by spelling, so a
  writable folder that lay inside a read-only one only through a junction
  or folder symbolic link (on either folder, or on the working folder)
  wasn't refused, and the non-admin sandbox let commands write there. It
  now compares real paths too (`subst` drives included), for the agent's
  commands in both sandboxes and for `command/exec`. The same folder
  marked read-only and writable under two spellings is still not refused;
  in the non-admin sandbox read-only wins (#1780). Newly refused for the
  same reason: a protected `.git`, `.codex` or `.agents` folder that is a
  link to a folder holding another writable folder of the profile. Replace
  the link with a folder; see "A writable folder inside a read-only one" in
  `docs/sandbox.md`.
- **Non-admin sandbox: a writable folder inside a protected `.git`, `.codex`
  or `.agents` folder is refused for older workspace-write profiles too**
  (#1810, D331). A profile in the older workspace-write format, for example
  with a writable root at the workspace's `.git\hooks`, skipped D331's
  check, so the agent's commands and `command/exec` ran and could write
  there. Such profiles are now refused, as the default sandbox already
  refused them. Naming `.git` itself writable is still allowed.
- **Non-admin sandbox: nested `.codex` and `.agents` folders are watched**
  (#1642, D283, #1541). One that appears or is replaced deeper in your
  workspace while a command runs isn't loaded until you confirm it, like one
  at the top. Commands can't rename, move or delete existing ones or the
  folders above them, so a `git checkout` or `npm ci` that does is refused
  there.
- **Non-admin sandbox: a `.git` that appears or changes is reported**
  (D332, #1682). When a `.git` at the top of a writable folder is new, or
  was replaced, when a command ends, Corbanu Terminal warns at startup and
  the TUI asks you to confirm it once you have checked its hooks and config:
  git runs them outside the sandbox. Commands can still create a `.git`
  (`git init` works). A `.git` that is a link is reported after every
  command. `.git` folders deeper inside aren't checked.
- **Default (elevated) sandbox: a `.git` made for a read-only `.git\hooks`
  is protected from the first command** (#1758). At the top of a writable
  folder (other than `%TEMP%`) with no `.git`, a configured read-only
  `.git\hooks` makes the sandbox create `.git` before the command starts.
  The command that caused this could still write `.git\config`, `HEAD` or
  `objects`; only later commands found `.git` protected. Now that first
  command can't either, as in the non-admin sandbox. The same holds for a
  `.codex` or `.agents` folder made this way. A deeper one, such as
  `sub\.git` for `sub\.git\hooks`, still has only `hooks` protected.
- **Default (elevated) sandbox setup no longer fails now and then where
  Windows requires complex passwords** (#1776). Setup gives the sandbox's
  user accounts a fresh random password, and about one setup in 4,500 drew
  one without the mix of characters that Windows' "Password must meet
  complexity requirements" policy needs (the Windows Server default, and
  common under domain policy), so setup failed with
  `helper_user_create_or_update_failed`. Passwords now always hold
  upper-case and lower-case letters, digits and symbols.
- **Non-admin sandbox: links can't undo a read-only path** (#1645, D293,
  #1542; #1670, #1667). A command doesn't start when a configured read-only
  path, or a folder above it, is a link (junction, symbolic link or mount
  point) inside a writable folder, which a command could replace; the message
  says to configure the link's target instead. A read-only path spelled
  through a writable folder's link gets the same denies (#1645). Cloud
  placeholder folders (OneDrive) at or above a read-only path, or as one, now
  get the same denies as plain folders, and a writable root that is a
  placeholder gets its label back (#1671, #1668).
- **Non-admin sandbox: a configured read-only path that doesn't exist is
  created and protected** (#1687, #1686, #1705; D333, D342), as the default
  sandbox already did. Before the command starts, it's made as an empty
  folder, so a command can't create it and fill it. A path meant to be a
  file (`.env`, say) therefore becomes an empty folder, and a read-only
  `.git\hooks` in a folder with no `.git` creates a protected `.git` with
  only an empty `hooks` folder, which git doesn't treat as a repository. To
  keep a file there, create it before you start a session. See "Read-only
  paths that don't exist yet" in `docs/sandbox.md`.
- **Non-admin sandbox commands start faster in large workspaces** (#1612,
  D285). Their delete protections are now written without rewriting the
  permissions of every file below (each is still checked, and anything
  unexpected falls back to the old method). In the PR's measurement on a
  101,000-file repository, a command took about 20 s instead of 31 s.
- **Non-admin sandbox: the faster protection walks don't start at a link**
  (D352, #1754, #1722). When the folder a walk starts at is itself a
  junction or symbolic link, or is swapped for one as the walk starts, it
  now leaves that folder to Windows' own walk, so the result always matches
  what Windows does. Corbanu doesn't normally start a walk at a link, so
  nothing changes in normal use.
- **Known limitation, now documented: the non-admin sandbox breaks tools that
  use named pipes** (#1797, #1561, #1778). Its commands can't write to
  named pipes they create, so Node.js child processes with piped output
  (`EPERM`), Git Bash (`couldn't create signal pipe`), a local `git clone`
  and Python `multiprocessing` fail there. This is not new in this release.
  The default (elevated) sandbox runs all of them; see "Known limitations"
  in `docs/sandbox.md`.
- **Known limitation, now documented: non-admin sandbox commands can write
  in `C:\ProgramData\Microsoft\DeviceSync`** (#1770). Windows labels that
  folder Low and lets everyone write to it. This is not new in this
  release, and we accept it; don't keep files there that you rely on.
- **Known limitation, now documented: default (elevated) sandbox commands
  can read other projects' running commands** (#1803). A command can read
  the memory of another project's command running at the same time
  (secrets in its environment, what it prints) and, for one run in a
  terminal, its console host (what you type and what it prints). It can't
  change them. This is not new in this release. A change awaiting security
  sign-off (#1800) stops the memory read, and a follow-up stops the console
  read; until then, avoid running commands in several projects at once.
- **A default (elevated) sandbox command now starts while a non-admin
  sandbox command is running** (#1819). Before, when the non-admin command's
  writable folders held the Corbanu home (for example `%TEMP%`, or a project
  folder above it), the elevated command failed after 15 seconds with
  `timed out after 15000ms connecting runner pipe-out`. The folder with the
  sandbox's helper programs now keeps its normal (Medium) integrity level.
  If that folder is a link, it is left alone and the sandbox log says so. If
  copying the helper fails and the one next to `codex.exe` is used instead,
  a Low label on it is fixed the same way, or the log names it as the cause.
- **Windows PowerShell's `Start-Process` works in both sandboxes** (#1805).
  Corbanu passes commands your environment, where Windows spells the search
  path `Path`, and both sandboxes added a second copy spelled `PATH`. Windows
  PowerShell 5.1's `Start-Process` then failed with "Item has already been
  added", and the copy that counted could miss folders added by
  `shell_environment_policy.set`. Each variable now reaches a command once,
  and a value you set wins over the inherited spelling; no folder is added
  to or removed from what you configured. Where Corbanu sets a proxy
  variable in both cases (`HTTP_PROXY` and `http_proxy`), sandboxed
  commands get only the capitals, so tools that read only the lower-case
  name (MSYS2 or Git Bash `curl`) no longer see that proxy.
