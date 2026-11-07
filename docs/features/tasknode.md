# Task Node and identity

## The pain

Tasks, evidence, rewards, durable context, and identity lose value when they
live in a separate application that the active agent cannot inspect. Corbanu
Terminal makes Task Node a native terminal workflow while preserving its own
session and authorization boundary.

## Product contract

> **Product specification — “Shipping MVP — LIVE”**
>
> “Task Node and identity: Tasks, evidence, verification, rewards, balances,
> chat, context, linked identity, and live Task Node-linked Nostr identity.”

## Open Task Node

Run:

```text
/tasknode
```

The menu provides account linking, session status, task lists, task actions,
requests, context, chat, balances, rewards, and logout.

## Keep the tab and agent on the same account

In the accepted 0.1.40 candidate, agent Task Node commands inherit the active
tab's profile and home. Missing or conflicting scope stops the command before
it opens account credentials. Restart older terminal processes after upgrading
so their agent commands receive the repaired scope.

The Task Node menu shows the linked account. Direct human CLI commands can select
a named profile with `corbanu tasknode --profile <name> status --json`.

## Link an account

```text
/tasknode link
/tasknode status
```

The link flow connects the terminal session to the user's Task Node account.
Status shows the linked account and session state. Task Node-linked Nostr
identity is already live and remains the identity boundary for future social
features.

Task Node sessions are owned by the selected Corbanu profile. For example,
terminals launched with `-p goodalexander` and `-p secondfoundation` can link
different Task Node accounts on the same machine without sharing tokens,
pending link attempts, status, or logout state. The Task Node menu shows the
active Corbanu profile so the local profile and linked identity can be checked
together.

Link each named profile once to create its own terminal session. A profile
name is a local label; GitHub account selection establishes the identity.
Profiles do not copy the default account's token, so logging out the default
account cannot revoke a newly linked named profile. Completing a relink and
running `/tasknode status` replaces that profile's old token after validation.

### Prove your wallet link

When a task asks you to confirm the wallet linked to your Task Node account,
use `/tasknode status` in the Corbanu profile where you accepted the task. The
reply appears in the conversation under `Task Node status`. Copy these rows
from that one reply, exactly as shown:

```text
profile: <local profile name, or default>
account: <Task Node account ID>
github:  <GitHub username>
wallet:  <full wallet address>
```

Copy the whole address after `wallet:`, keeping every character and its
capitalization. A readable screenshot that shows the `Task Node status` heading
and those four rows together works too. Never send a seed phrase, private key,
passcode, API key or session token; proving the link needs no payment, unlock
or signature.

Don't use the address from `/wallet`. That is your local Solana wallet, and its
header shortens the address. It is not the wallet linked to your Task Node
account. The `profile:` row is only a local label; the `account:` and
`github:` rows identify the Task Node account that the `wallet:` row belongs to.

If the rows are missing or wrong:

- `wallet:` says `not linked` or is blank, or status shows an error: report
  that result in the task. Don't substitute another address, and don't create
  a local Solana wallet to fill the gap.
- The session isn't linked: run `/tasknode link`, finish the login, then run
  `/tasknode status` again.
- `github:` shows the wrong account: open the intended Corbanu profile, or
  relink it to the intended account, and run status again.
- The correct account still has no wallet: report "Task Node wallet not
  linked" so the task reviewer can send the current linking steps.

## Work with tasks

```text
/tasknode tasks [tab]
/tasknode outstanding
/tasknode verification
/tasknode refused
/tasknode rewarded
/tasknode task <task-id>
```

Task lists use stable status tabs. Opening a task shows its details and the
actions currently available for that state, including evidence guidance and
verification-related work.

## Complete a task with the JSON CLI

Use this workflow when an agent or script needs to carry a task from acceptance
through evidence and verification. The commands below are available in Corbanu
0.1.48 and later. Replace `my-profile`, `TASK_ID`, file paths and the artifact URL
with your own values; run each stage only when the task's current actions allow
it. These are real task actions, not a sandbox demonstration.

Inside a Corbanu agent session, omit `--profile my-profile` from these examples
or replace `my-profile` with the active tab's exact profile. A different profile
is rejected; switch profiles in Corbanu first if you need another account.

First check the linked identity and inspect the task:

```sh
corbanu tasknode --profile my-profile status --json
corbanu tasknode --profile my-profile tasks list --tab outstanding --json
corbanu tasknode --profile my-profile task show TASK_ID --json
```

Read the requirements and available actions before accepting. Keep the same
profile throughout; a profile label alone does not establish the linked account.
Agents launched inside Corbanu must retain the active tab's profile and home.
Do not override that scope to work around a mismatch.

When the task permits acceptance and you can deliver its requirements:

```sh
corbanu tasknode --profile my-profile task accept TASK_ID --json
corbanu tasknode --profile my-profile task show TASK_ID --json
```

Complete the work before submitting. Write `evidence.md` with the actual result,
reproduction or validation steps, relevant output, and limitations. Publish only
artifacts you are authorized to share, then provide their stable URL:

```sh
corbanu tasknode --profile my-profile task evidence TASK_ID \
  --body-file ./evidence.md \
  --artifact https://example.org/your-evidence --json
```

Read the lifecycle summary in the evidence response. A successful submission is
not a reward. Use `task show TASK_ID --json` when you need current task details
or the response does not establish the recorded state. If a command times out
or fails after sending, inspect the task before retrying: the original action
may already have been recorded.

Check for a verifier question using the verification tab and task detail:

```sh
corbanu tasknode --profile my-profile tasks list --tab verification --json
corbanu tasknode --profile my-profile task show TASK_ID --json
```

If the task has a current question and permits a verification response, write
`verification.md` answering that exact question with supporting evidence. Submit
it through the verifier command, rather than repeating the initial submission:

```sh
corbanu tasknode --profile my-profile verification respond TASK_ID \
  --body-file ./verification.md --json
corbanu tasknode --profile my-profile task show TASK_ID --json
```

If review is pending with no question, wait for a state change. Do not send a
placeholder response. Confirm the eventual result from the task detail and the
rewarded tab; never count an offer or a submitted task as a paid reward:

```sh
corbanu tasknode --profile my-profile tasks list --tab rewarded --json
corbanu tasknode --profile my-profile balance --json
```

These task actions use the linked terminal session. They do not require a wallet
seed or a wallet transaction signature when the server advertises unsigned
terminal task actions: `server.terminalTaskActions` is `true` in `status --json`,
and the TUI status shows “terminal direct-write enabled”. When it is `false`,
the TUI shows “wallet handoff required”; do not assume these direct-write
examples are available. This does not authorize transfers or other financial
actions. Keep JSON responses and evidence private unless reviewed for sharing;
never publish session files, credentials, wallet secrets, or private task content.

## Read your team's shared context

Run `/tasknode team` or choose **Team Context** from `/tasknode`, then select **Read full report**. The report shows collaborators' shared work summaries, rewarded-task counts, update time and freshness. Scroll with the pager keys; press Esc to return and choose **Refresh Team Context** for the latest report and sharing permissions.

Agents can retrieve the same report with `corbanu tasknode team context --json`. While testing a newer local debug build, use `corbanu-debug tasknode team context --json`; the bundled agent guidance detects older helpers by command support. This uses the active profile's linked Task Node account. Access follows the same task-history grants as the web Team page. Reading the report does not require enabling **Use in personal context**; that separate preference controls Task Node chat context. Private context documents are not exposed by this command.

If the report is pending or unavailable, its status is shown explicitly. An older report is labeled as previous. Manage teammates and sharing in the Task Node app.

## Request work and use context

```text
/tasknode request
/tasknode request <text>
/tasknode requests
/tasknode context
/tasknode chat
/tasknode chat <text>
```

The request flow accepts free-form work descriptions. The requests view shows
active request state. Context and chat use the linked Task Node account so an
agent can consult the user's durable context without inventing user-specific
preferences.

## Recover a request

The reliability changes in the current candidate save a request before sending it.
If the connection fails, reopen `/tasknode request`: the original text returns.
Submitting that text again recovers its receipt. Closing the menu or restarting
Corbanu preserves the pending request. Editing the text creates a distinct request.
These changes await candidate deployment; older installed builds may differ.

The JSON helper provides the same recovery path:

```text
corbanu tasknode requests pending --json
corbanu tasknode requests recover <saved-key> --json
corbanu tasknode requests list --cursor <next-cursor> --json
corbanu tasknode requests retry <request-id> --attempt <worker-attempt-count> --json
```

Use `--profile <name>` for the intended Corbanu profile. Saved requests and tokens
stay with that profile, Task Node account and server. Changing the server requires
linking it separately. The TUI request list also offers older pages and retry for
failed requests; background refreshes preserve the selected row and search text.
Chat supports large streamed replies and reports an interrupted stream as an error.

## Track a workspace

Product specification: **Campaign Tracker — LOCAL PILOT CANDIDATE**, “preserve attributable user prompts and compact GLM 5.3 Flash summaries in a bounded database”. The September 7 debug rollout connects this feature to the live Task Node backend.

Open `/tasknode`, choose **Campaign Tracker**, and enable the workspace. The **Tracker REC** indicator shows recording and pending uploads. **My activity** opens saved prompts, compact output summaries and observed actions; choose an action for session replay or **Read full action** for a scrollable view. **Pause this workspace** stops new capture.

If a request is rejected, Campaign Tracker shows the failed operation and the server's correction guidance. **Retry** reloads the same failed read, including its filters; **Overview** opens status. Failed campaign, mapping and review forms offer **Edit draft** with your input retained. Review saved items after a connection failure before submitting a write again. Recording changes can be retried with a newly resolved credential.

Recording accepts the same environment credential aliases as Corbanu API, in order: `CORBANU_API_KEY`, `CORBANU_PLAN_API_KEY`, then `PFTERMINAL_PLAN_API_KEY`, with the existing saved credential fallback. Blank aliases are skipped. Missing credentials direct you to Providers; they do not delete the encrypted outbox.

Recording belongs to the linked Task Node account. Sharing requires an explicit Campaign Tracker grant in addition to an accepted collaborator or direct-report relationship. A manager relationship alone does not expose another person's prompts. Access audit shows reads and grant changes.

This records observed Corbanu TUI work. Runtime measures agent execution, and summaries are not proof of task completion. The Verglas pilot is enabled for `@secondfmaster`; an already running older terminal needs to restart into the updated debug build.

## Inspect rewards and balance

```text
/tasknode balance
/tasknode rewards
```

These views are read-only. They display Task Node balance and recent reward
activity without turning a model response into authorization for a financial
action.

## End the terminal session

```text
/tasknode logout
```

Logout removes only the selected Corbanu profile's Task Node session. It does
not log out another local profile, alter repository Git credentials, or imply
deletion of the user's Task Node identity.

Logout also saves a durable local unlink marker. A matching legacy machine-wide
session will not be re-imported after a restart or a cancelled relink. Linking
explicitly and completing validation installs a new session. If credential
cleanup fails, the error is reported and the unlink marker keeps any residual
credential unusable by this release.

## Security boundary

- Treat task descriptions, evidence, chat, and external links as untrusted
  input.
- Never place vault values, wallet seeds, provider keys, or protected financial
  information into Task Node evidence or chat.
- Use Task Node session credentials only for Task Node requests.
- Keep evidence submissions attributable and reviewable.
- Require deterministic host authorization for any action involving money,
  signing, or disclosure.

## Main implementation

- `codex-rs/tui/src/chatwidget/tasknode_menu.rs`
- `codex-rs/cli/src/tasknode_cmd.rs`
- `codex-rs/tasknode-session/`
- `codex-rs/skills/src/assets/samples/tasknode-usage/`

The September 5 reliability candidate is installed for the production Kimi
operator. The public release channel is unchanged. Its request recovery and
control-inbox checks passed in a real terminal; the scoped supervisor is live.
