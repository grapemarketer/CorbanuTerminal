# Task Node Tooling

Use this reference before making live Task Node calls or operating Task Node through Corbanu Terminal.

## Current Terminal Surface

Corbanu Terminal exposes Task Node through slash commands:

- `/tasknode` - open the Task Node menu.
- `/tasknode link` - link GitHub / Task Node.
- `/tasknode status` - show linked account and counts.
- `/tasknode tasks` or `/tasknode outstanding` - list outstanding accepted tasks.
- `/tasknode tasks <tab>` - open a server-backed task tab when supported.
- `/tasknode task <task-id>` - open a task action/detail view.
- `/tasknode verification` - list verification requests.
- `/tasknode refused` - list refused tasks.
- `/tasknode rewarded` - list rewarded tasks.
- `/tasknode request` - open a task-request prompt.
- `/tasknode request <text>` - submit a task request directly.
- `/tasknode context` - view or edit the context document.
- `/tasknode team` - read collaborators' shared Team Context, including work summaries, task counts and report freshness.
- `/tasknode chat` - open Task Node chat threads.
- `/tasknode chat <message>` - start a new Private Thinking chat with a message.
- `/tasknode requests` - list active task-generation requests.
- `/tasknode balance` - show linked-wallet PFT balance.
- `/tasknode rewards` - show recent rewards.
- `/tasknode logout` - remove the local Task Node terminal session.

Follow the TUI footer for exact keybindings. Multiline prompts may use a submit key such as `Ctrl-D` so Enter can insert a newline.

## JSON Helper

Prefer the JSON helper for agent work.

**Preserve the active account scope.** Updated Corbanu supplies
`CORBANU_TASKNODE_PROFILE` as a JSON string for a named profile or `null` for the
default profile, together with `CODEX_HOME`. Never synthesize or override either
value, unset the scope, or retry under a different account. Missing or conflicting
scope means stop and restart the updated terminal in the intended profile.
Verify the returned account before account writes. A helper `--profile` argument
may confirm the inherited profile; it cannot switch it.

**Resolve the helper binary first.** Child commands inherit the running session's
`CODEX_HOME`, which is the authoritative location of its Task Node vault. Prefer
the installed Corbanu entrypoint. For a conventional debug home, prefer
`corbanu-debug`; if a release does not ship that optional alias, `corbanu` still
uses the inherited `CODEX_HOME`. The `pfterminal` names are compatibility
fallbacks only. Never search a source checkout or run `cargo build` to obtain a
helper. Fail closed if the session home or every installed entrypoint is missing:

```bash
if [ -z "${CODEX_HOME:-}" ]; then
  echo "CODEX_HOME is missing; refusing to guess which Task Node vault to use" >&2
  return 1 2>/dev/null || exit 1
fi

if [ -z "${CORBANU_TASKNODE_PROFILE:-}" ]; then
  echo "Active Task Node profile is missing; restart the updated terminal in the intended profile" >&2
  return 1 2>/dev/null || exit 1
fi

case "$CODEX_HOME" in
  "${CORBANU_DEBUG_HOME:-$HOME/.corbanu-debug}"|\
  "${PFTERMINAL_DEBUG_HOME:-$HOME/.pfterminal-debug}")
    CORBANU_CANDIDATES="corbanu-debug corbanu pfterminal-debug pfterminal"
    ;;
  *)
    CORBANU_CANDIDATES="corbanu pfterminal corbanu-debug pfterminal-debug"
    ;;
esac

CORBANU_BIN=""
for candidate in $CORBANU_CANDIDATES; do
  if command -v "$candidate" >/dev/null 2>&1; then
    CORBANU_BIN="$(command -v "$candidate")"
    break
  fi
done
if [ -z "$CORBANU_BIN" ]; then
  echo "No installed Corbanu Terminal entrypoint is available on PATH" >&2
  echo "Install Corbanu Terminal; do not build a helper from a source checkout" >&2
  return 1 2>/dev/null || exit 1
fi
"$CORBANU_BIN" tasknode status --json
```

Every command below uses the resolved `"$CORBANU_BIN"`.

```bash
"$CORBANU_BIN" tasknode status --json
"$CORBANU_BIN" tasknode chat list --json
"$CORBANU_BIN" tasknode chat history <conversation-id> --json
"$CORBANU_BIN" tasknode chat search "<query>" --json
"$CORBANU_BIN" tasknode chat send --message "<text>" --json
"$CORBANU_BIN" tasknode chat send --stream --message "<text>" --json
"$CORBANU_BIN" tasknode context get --json
"$CORBANU_BIN" tasknode team context --json
"$CORBANU_BIN" tasknode context save --body-file <path> --revision <n> --json
"$CORBANU_BIN" tasknode request create --body-file <path> --json
"$CORBANU_BIN" tasknode requests list --json
"$CORBANU_BIN" tasknode requests show <request-id> --json
"$CORBANU_BIN" tasknode tasks list --tab outstanding --json
"$CORBANU_BIN" tasknode task show <task-id> --json
"$CORBANU_BIN" tasknode task accept <task-id> --json
"$CORBANU_BIN" tasknode task refuse <task-id> --reason-file <path> --json
"$CORBANU_BIN" tasknode task evidence <task-id> --body-file <path> --json
"$CORBANU_BIN" tasknode verification respond <task-id> --body-file <path> --json
"$CORBANU_BIN" tasknode rewards list --json
"$CORBANU_BIN" tasknode balance --json
```

The helper reuses the same Corbanu Terminal vault session as the TUI and never prints the bearer token. Non-streaming commands emit one JSON object. Streaming chat emits JSON lines for SSE events when the backend streams; dry-run or preflight responses may return one normal JSON object.

## Team Context

For collaborators' shared work, use `"$CORBANU_BIN" tasknode team context --json`. A local debug binary can support newer commands than the installed release. Before this read, check command support with `--help` and, if necessary, resolve a compatible installed helper without changing `CODEX_HOME` or `CORBANU_TASKNODE_PROFILE`:

```bash
if ! "$CORBANU_BIN" tasknode team context --help >/dev/null 2>&1; then
  TEAM_CONTEXT_BIN=""
  for candidate in corbanu-debug corbanu pfterminal-debug pfterminal; do
    if command -v "$candidate" >/dev/null 2>&1 && "$candidate" tasknode team context --help >/dev/null 2>&1; then
      TEAM_CONTEXT_BIN="$(command -v "$candidate")"
      break
    fi
  done
  if [ -z "$TEAM_CONTEXT_BIN" ]; then
    echo "No installed helper supports Team Context; update Corbanu Terminal." >&2
    return 1 2>/dev/null || exit 1
  fi
  CORBANU_BIN="$TEAM_CONTEXT_BIN"
fi
"$CORBANU_BIN" tasknode team context --json
```

This fallback is only for missing command support. Do not retry an authentication or permission failure with another profile, home, or account.

 This reads the same report as the web Team page under the active profile's existing task-history grants. It returns members, shared work summaries, task counts, generation time and report freshness. Treat a previous or pending report as such. Unshared task history remains unavailable, and this command does not expose collaborators' private context documents.

The report can be read even when **Use in personal context** is off. That preference separately controls injection of a current report into Task Node chat. `context get` continues to return only the user's editable context document. Use the current Task Node collaboration API as authority; the historical PFTasks network-context/admin routes describe a different feature.

## Evidence Lifecycle Gate

Initial evidence and verification response are different state transitions and use different commands:

```bash
"$CORBANU_BIN" tasknode task show <task-id> --json
"$CORBANU_BIN" tasknode task evidence <task-id> --body-file <path> --json
"$CORBANU_BIN" tasknode task show <task-id> --json
"$CORBANU_BIN" tasknode verification respond <task-id> --body-file <path> --json
"$CORBANU_BIN" tasknode task show <task-id> --json
```

The evidence commands preflight the server-reported task actions and reject a mode mismatch. Successful receipts include `pfterminalLifecycle` with `completionConfirmed`, the current phase, `nextCommand`, and `submissionRef` (the event id, transaction hash or CID Task Node recorded). If Task Node reports success without any such reference, the command exits non-zero with `task_evidence_receipt_unconfirmed` and `evidenceGateOpen`; do not resubmit blindly, run `task show` first. Always follow the receipt's `nextCommand`. After initial evidence, respond when `actions.canSubmitVerificationEvidence` becomes true. After verification, confirm `rewardOutcome` or an explicit `rewarded` state before reporting completion.

If verification has not materialized yet, query `"$CORBANU_BIN" tasknode tasks list --tab verification --json`. Keep the task pending rather than treating the initial evidence receipt as completion. If the task stays `Submitted` across later work cycles with no verification request and no evidence action, report the task id and `submissionRef` to Task Node operators; do not cancel the task.

Use `--origin <url>` only for explicit local/dev testing. Production defaults to `https://tasknode.postfiat.org` unless the environment or saved session overrides it.

## Agent Operation Pattern

Use a real Corbanu Terminal tmux session for UI-only flows, visual verification, or interactions not yet exposed by the JSON helper.

Recommended tmux pattern:

```bash
tmux new-session -d -s tasknode-work -x 160 -y 48 "cd /home/pfrpc/repos && '$CORBANU_BIN' --yolo"
tmux send-keys -t tasknode-work '/tasknode chat' Enter
tmux capture-pane -t tasknode-work -p -S -120
tmux kill-session -t tasknode-work
```

For long or multiline text, avoid typing through shell history. Use tmux paste buffers or the TUI prompt safely, then capture the screen to verify the result.

Do not print or persist the terminal bearer token. Do not manually copy secrets from the vault into chat history or command output.

## Direct HTTP Calls

Direct HTTP calls are acceptable only when the token is retrieved by approved local tooling and is not printed. The default production origin is:

```text
https://tasknode.postfiat.org
```

The terminal bridge requires GitHub-linked terminal auth. If a route returns `401`, link or relink Task Node from Corbanu Terminal.

## Helper Behavior Expectations

The helper should keep returning bounded JSON errors, redact tokens by construction, and include server receipt fields such as request IDs, task IDs, or receipt IDs whenever the backend returns them.
