# Configuration

Corbanu Terminal inherits Codex configuration but ships product-specific provider
defaults. Most users do not need to define model providers manually.

## Config Location

Corbanu Terminal reads config from `CODEX_HOME/config.toml`.

To select a product home explicitly:

```bash
export CORBANU_HOME="$HOME/.corbanu"
```

`PFTERMINAL_HOME` remains supported for legacy automation. If neither product
override is set, an explicit `CODEX_HOME` wins; otherwise Corbanu Terminal
prefers an existing `.corbanu`, reuses a lone `.pfterminal`, and defaults fresh
installs to `.corbanu`. This keeps product state separate from stock Codex.

## Built-In Providers

These providers are compiled into Corbanu Terminal:

| Provider id      | Display name   | Base URL                                | Env key              | Wire API         |
| ---------------- | -------------- | --------------------------------------- | -------------------- | ---------------- |
| `openai`         | OpenAI         | `https://chatgpt.com/backend-api/codex` | Account login        | Responses        |
| `anthropic`      | Anthropic      | `https://api.anthropic.com/v1`          | `ANTHROPIC_API_KEY`  | Messages         |
| `claude-plan`    | Claude Plan    | Claude Code account route               | Claude Code login    | Messages         |
| `ambient`        | Ambient        | `https://api.ambient.xyz/v1`            | `AMBIENT_API_KEY`    | Chat Completions |
| `kimi-code`      | Kimi Code      | `https://api.kimi.com/coding/v1`        | `KIMI_API_KEY`       | Chat Completions |
| `zai`            | Z.AI           | `https://api.z.ai/api/paas/v4`          | `ZAI_API_KEY`        | Chat Completions |
| `deepseek`       | DeepSeek       | `https://api.deepseek.com`              | `DEEPSEEK_API_KEY`   | Responses        |
| `openrouter`     | OpenRouter     | `https://openrouter.ai/api/v1`          | `OPENROUTER_API_KEY` | Chat Completions |
| `meta`           | Meta           | `https://api.meta.ai/v1`                | `MODEL_API_KEY`      | Responses        |
| `baseten`        | Baseten        | `https://inference.baseten.co/v1`       | `BASETEN_API_KEY`    | Chat Completions |
| `vercel`         | Vercel         | `https://ai-gateway.vercel.sh/v1`       | `AI_GATEWAY_API_KEY` | Responses        |
| `corbanu-plan`   | Corbanu API    | Corbanu API gateway                     | API credential       | Multiple         |
| `amazon-bedrock` | Amazon Bedrock | AWS Bedrock endpoint                    | AWS or Bedrock auth  | Responses        |
| `ollama`         | Ollama         | Configured local endpoint               | None                 | Responses        |
| `lmstudio`       | LM Studio      | Configured local endpoint               | None                 | Responses        |

OpenAI uses Codex account login from `/providers` or `corbanu login`.
Provider API keys should normally be stored through onboarding, `/providers`,
or `/vault`. Corbanu API is funded and its keys are managed through `/wallet`.
Amazon Bedrock uses cloud authentication, while Ollama and LM Studio use local
servers. Environment variables remain supported for temporary sessions and
automation.

Custom compatible providers can be added under `[model_providers.<id>]` in
`config.toml`. See [Models and providers](features/model-providers.md) for the
complete one-row-per-provider product inventory.

## Common Model Configs

Set a default provider and model in `$CODEX_HOME/config.toml`.

Ambient:

```toml
model_provider = "ambient"
model = "z-ai/glm-5.2"
```

OpenAI Codex account:

```toml
model_provider = "openai"
model = "gpt-5.6-luna"
```

DeepSeek direct:

```toml
model_provider = "deepseek"
model = "deepseek-flash"
```

Z.AI:

```toml
model_provider = "zai"
model = "glm-5.2"
```

OpenRouter pinned DeepSeek Flash:

```toml
model_provider = "openrouter"
model = "deepseek/deepseek-v4-flash-0731"
```

OpenRouter MiniMax:

```toml
model_provider = "openrouter"
model = "minimax/minimax-m3"
```

Baseten GLM:

```toml
model_provider = "baseten"
model = "zai-org/GLM-5.2"
```

Vercel GLM:

```toml
model_provider = "vercel"
model = "zai/glm-5.2"
```

Vercel GLM Fast:

```toml
model_provider = "vercel"
model = "zai/glm-5.2-fast"
```

Current Vercel AI Gateway models can use the same provider:

```toml
model_provider = "vercel"
model = "zai/glm-5.3-flash" # or zai/glm-5.3
```

The Vercel Kimi K3 and DeepSeek V4 Pro picker entries use provider-qualified
catalog identities so they remain distinct from the same slugs on OpenRouter:

```toml
model_provider = "vercel"
model = "vercel/moonshotai/kimi-k3"
# model = "vercel/deepseek/deepseek-v4-pro"
```

You can also select a model per run:

```bash
corbanu -m glm-5.2
corbanu -m gpt-5.6-luna
corbanu -m deepseek-flash
corbanu -m deepseek/deepseek-v4-flash-0731
corbanu -m zai-org/GLM-5.2
corbanu -m zai/glm-5.2
corbanu -m zai/glm-5.2-fast
```

The model picker maps these model slugs to the correct built-in provider.

## Reopen Warm-Up

When you reopen a conversation (`/resume`, restarting Corbanu on it) or fork it (`/side`, editing an earlier
message, including the first one) on a provider that uses a WebSocket connection, such as OpenAI, Corbanu sends one warm-up request to prime
the model's cache, so the first reply is faster. It is billed like any request (the session's instructions and
tools, typically 10,000 to 15,000 input tokens), and Corbanu says so once it has been sent. To skip it, set this at
the top level of `config.toml` (or pass `-c skip_reopen_warmup=true`):

```toml
skip_reopen_warmup = true
```

The first reply after reopening may then be slower, and when approvals go to the auto-reviewer, its session starts at
the first review instead of on reopen. A new conversation still warms up, and so do sub-agents.

App-server clients that start a thread only for realtime voice (`thread/start`, then `thread/realtime/start`) get
the new-conversation warm-up too, on the thread's text model, which realtime uses only for handoffs (and the
optional end-of-session transcript turn). Such a client should pass `config: {"skip_startup_warmup": true}` on
`thread/start` so no warm-up is sent; see "Realtime-only threads" in `codex-rs/app-server/README.md` (#1045).
`skip_startup_warmup = true` in `config.toml` skips every warm-up, new or reopened, for all conversations.

## Vault And Secrets

Provider API keys saved by Corbanu Terminal are stored in the encrypted vault, not in
`config.toml`.

Vault labels:

```text
provider/anthropic_api_key
provider/ambient_api_key
provider/kimi_api_key
provider/zai_api_key
provider/deepseek_api_key
provider/openrouter_api_key
provider/model_api_key
provider/baseten_api_key
provider/ai_gateway_api_key
```

Do not put long-lived provider keys in `experimental_bearer_token` unless you
are intentionally running an automation-only setup. For interactive use, use
onboarding or `/vault`.

<a id="model-key-broker"></a>

### Model key broker (`broker_model_auth`)

With `broker_model_auth` on, provider API keys are held by a separate
credential broker process, and the broker sends every model request for
Corbanu. Keys saved in the vault are read only by the broker. Keys in the
environment are moved to the broker when the session starts and removed from
Corbanu's environment. If the broker
can't start, model requests are refused, never sent directly, and the error
message names the setting and how to turn it off.

Limits:

- Sign-in tokens (ChatGPT sign-in, API-key login, `experimental_bearer_token`)
  are still read by Corbanu and then passed to the broker.
- Command, AWS and header-based provider sign-ins aren't brokered.
- Realtime conversations and websockets are off.
- Provider URLs that use plain HTTP, an IPv6 address or a query string can't be
  brokered, so those requests are refused.

The default depends on the security level:

| Level                                                                           | Default                         |
| ------------------------------------------------------------------------------- | ------------------------------- |
| Permissive                                                                      | off                             |
| Moderate or Aggressive (`[security] level`, or the level chosen in `/security`) | on, on macOS, Linux and Windows |

On other systems it stays off, because the broker isn't supported there. A value
you set yourself always wins over the level's default; only a managed policy
outranks it. A project's `.codex/config.toml` doesn't change it under
Moderate or Aggressive:

```toml
[features]
broker_model_auth = false   # or true, at any level
```

`-c features.broker_model_auth=false` and `--disable broker_model_auth` work
the same way. A level you choose in `/security` while Corbanu is running takes
effect when Corbanu restarts; a `[security] level` in config applies from the
next new session. Under Moderate or Aggressive with the broker off, Corbanu
shows a startup warning and `/security` reports the level as partial. What
the levels protect and their limits are on
[Moderate security level](security-moderate.md) and
[Aggressive security level: protected files](security-aggressive.md).

### Protected launch (`secretless_agent_launch`)

With `secretless_agent_launch` on, every agent command runs inside the OS
sandbox, with only allowlisted environment variables, and can't read these
files:

- Corbanu's own: its vault (`secrets`), sign-in files (`auth.json`,
  `.credentials.json`, `provider_auth.json`, `.env`), config (`config.toml`,
  `managed_config.toml`), `wallet`, history (`history.jsonl`), session
  transcripts (`sessions`, `archived_sessions`), logs (`log`), shell
  snapshots (`shell_snapshots`), its runtime folder (`run`), its databases
  (`*.sqlite*` at the top of its home), the database init lock
  (`pfterminal_db_init.lock`), the GPU controller lock
  (`gpu-controller.lock`), pending accounting records
  (`accounting-pending`), databases recovery set aside (`db-backups` in its
  home, `<home>.db-backups` beside it), and the credential broker's runtime
  folder. If you keep the databases in a separate folder (`sqlite_home` or
  `CODEX_SQLITE_HOME`), commands can't read that folder's databases, init
  lock, GPU controller lock, pending accounting records and backups
  (`db-backups` in it, `<folder>.db-backups` beside it) either. On Linux a
  backup folder beside a folder is covered only when it exists at the
  command's start.
- CLI tokens: the files `~/.netrc`, `~/.git-credentials`,
  `~/.config/gh/hosts.yml`, `~/.aws/credentials`, `~/.docker/config.json`,
  `~/.npmrc` and Claude's sign-in (`~/.claude/.credentials.json`, and
  `.credentials.json` in `CLAUDE_CONFIG_DIR` when set); on Windows also
  `_netrc` in your profile folder, the GitHub CLI's `hosts.yml` in
  `%APPDATA%`, the whole Google Cloud CLI folder `%APPDATA%\gcloud` (its
  credentials and config), and Cargo's `credentials.toml` and `credentials`
  (in `CARGO_HOME`, by default `%USERPROFILE%\.cargo`).
- On macOS, your keychains (`~/Library/Keychains`).

On Windows, one of these that doesn't exist when a command starts isn't
covered for that command (such as a backup recovery creates while it runs),
except Corbanu's own folders inside its home (`secrets`, `wallet`, `run`,
`shell_snapshots`, `log`, `sessions`, `archived_sessions`,
`accounting-pending`, `db-backups`) and files created directly in its home.

Approving a command never unlocks these, at any level; under Moderate the
approval prompt names such a file and says so. Nothing runs outside the
sandbox at any level. At Permissive, Corbanu therefore doesn't offer it
(#739):

- A command that asks to run outside the sandbox isn't run, and you aren't
  asked. Corbanu says it isn't possible here. (If your own permission profile
  denies reads, you're asked as before and the command runs in the sandbox.)
- After a sandbox denial, "retry without sandbox?" isn't offered.
- A command that a remembered approval or allow rule ("don't ask again")
  matches still skips the prompt, but runs inside the sandbox. Corbanu says so
  the first time in a session.

Moderate and Aggressive keep their own handling: Moderate still asks you about
such a request, because approving lifts its file protections for that command,
and runs it inside the sandbox. Without the feature, Permissive works as
configured.

### Kill switch

`/security` → `g` → "Turn the kill switch on" stops the work running in every
pane, Claude panes included, and refuses model turns until a person turns it
off there. Messages and tasks from other agents that arrive meanwhile wait (a
Claude pane keeps only the newest task; a task for a Corbanu pane you opened
yourself in `/panes` is refused instead, like a typed message), and
`/orchestrate` assignments and whips are paused. Turning the switch off
restarts nothing: no pane starts a turn, and paused assignments stay paused
until you run `/orchestrate resume <id>`. Waiting messages and tasks go with
each pane's next turn, when someone types in it or it gets new work.

<a id="shell-environment"></a>

## Shell Environment

Commands the model runs (shell commands and long-running exec sessions, at
every `/security` level) start with Corbanu's environment, filtered by
`[shell_environment_policy]`. Provider API key variables are then removed, so
a command can't read the key Corbanu uses to talk to the model. That covers
every built-in provider's key variable (`OPENAI_API_KEY`, `CODEX_API_KEY`,
`ANTHROPIC_API_KEY`, `ZAI_API_KEY`, `OPENROUTER_API_KEY`, `KIMI_API_KEY`,
`DEEPSEEK_API_KEY` and the rest of the table above), a few other credentials
Corbanu reads (`CODEX_ACCESS_TOKEN`, `AZURE_OPENAI_API_KEY`,
`ANTHROPIC_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`, `AWS_BEARER_TOKEN_BEDROCK`)
and the `env_key` of any provider you configure. Corbanu itself still reads
them, and the session's shell snapshot leaves them out too, even when a shell
startup file such as `~/.zshrc` exports them.

To hand Corbanu's value of one of these variables to commands on purpose, name
it exactly in `include_only` (keep `"*"` to leave the rest of the environment
alone), or the equivalent `filters = { "*" = "include", OPENROUTER_API_KEY = "include" }`:

```toml
[shell_environment_policy]
include_only = ["*", "OPENROUTER_API_KEY"]
```

Wildcards such as `"*_API_KEY"` never pass a provider key through. A `set`
entry gives commands the value written in `config.toml` instead, never the
inherited one. When the default excludes are on
(`ignore_default_excludes = false`), names containing `KEY`, `SECRET` or
`TOKEN` are removed before `include_only` is applied, so for those only `set`
works. For a one-off command, prefer
fetching the key from the vault inside that command:
`OPENROUTER_API_KEY="$(corbanu vault auth-helper provider/openrouter_api_key)" my-tool`.

MCP servers started over stdio only receive a short list of basic variables
plus the ones named in that server's `env_vars` or `env`, so a provider key
reaches one only if you list it there. A shell profile that exports a key
itself (for example a login shell reading `~/.zprofile`) is outside this
filter.

<a id="telegram"></a>

## Telegram Connector

`corbanu telegram` runs a Telegram long-polling connector that drives the
same in-process app-server harness as the terminal UI and `corbanu exec`.
Telegram-specific configuration is read locally by the connector from the
`[telegram]` table. Core accepts this table during strict config validation,
but the connector owns the individual settings.

For interactive setup, run `/telegram` in the TUI. Corbanu Terminal validates a
masked BotFather token into the encrypted vault, waits automatically for the
user to message the bot, asks which exact chat and sender to authorize, captures
the current model/workspace/permission settings, and starts the connector. The
bot remains silent until that authorization completes. Leaving the discovery
screen cancels its polling, and stale results cannot reopen the screen. The
same screen reports health and supports restart, stop, token replacement, and
full disconnect. A configured connector is restored when Corbanu Terminal starts;
an operation lock prevents multiple Corbanu Terminal processes from racing into two
pollers. The setup script below remains available for unattended hosts.

```toml
[telegram]
enabled = true
bot_token_env = "PFTERMINAL_TELEGRAM_TOKEN"
allowed_chat_ids = [21000038, -1001941234987]
allowed_user_ids = [21000038]
max_attachment_bytes = 10485760
media_retention_days = 7
max_media_store_bytes = 268435456
mode = "polling"
default_model = "glm-5.2"
approval_policy = "on-request"
sandbox_mode = "workspace-write"
default_cwd = "/home/alice/corbanu-telegram"
webhook_url = ""
```

The bot token is never read from `config.toml`. Resolution order is:

1. The environment variable named by `bot_token_env`.
2. The encrypted vault label `telegram/bot_token`.
3. Startup error.

Connectors created through `/telegram` remove the token environment variable
from their child process so the just-validated vault credential cannot be
silently replaced by a stale shell value. `sandbox_mode` is connector-specific:
the TUI copies the permission mode shown at authorization time without changing
the global Corbanu Terminal sandbox.

Chats are default-deny. Only numeric Telegram chat IDs in `allowed_chat_ids`
can start turns. Private chats use that list directly. Group and supergroup chat
IDs are negative and additionally require the initiating user in
`allowed_user_ids`; group membership alone grants no authority. Approval buttons
are accepted only from an allowed user in the exact chat and forum topic that
owns the pending request. The setup script refuses a group chat without at least
one `--user-id`.

The connector stores recovered thread IDs and delivered item markers in
`$CODEX_HOME/telegram/state.json` so a restarted poller can resume the same
app-server threads without replaying the full transcript after lag recovery.
Per-conversation model and approval-policy overrides are stored in the same
file. A conversation is a chat plus its optional forum-topic ID, so two topics
in one group cannot share turns, model settings, or approvals.

Incoming updates first enter a bounded, bot-specific durable inbox at
`$CODEX_HOME/telegram/updates-<bot-id>.json`. The connector marks an update
complete only after app-server acceptance. Unapplied updates replay after a
restart with a deterministic client message ID, while completed IDs provide a
bounded duplicate filter.

Telegram messages use HTML parse mode, split outbound text at Telegram's
4096-character raw-text limit, and surface sensitive operations through inline
approval buttons rather than auto-approving them.

The Telegram command surface is:

- `/new` starts a fresh app-server thread.
- `/cancel` or `/stop` interrupts the active turn.
- `/status` shows the active thread and turn.
- `/model` shows the chat's active model/provider and the available model list.
- `/model <alias-or-slug>` saves the model for the chat and updates the current
  thread with `thread/settings/update` so subsequent turns keep the same
  history. Built-in aliases include `fable`, `opus`, `gpt`, and `gpt-5.5`.

  **`/model` selects a model, not a provider.** `model/list` does not report which
  provider serves each model, so the chat keeps the provider it already had except
  for the few model families whose provider is unambiguous from the slug (the
  Claude plan models, `gpt-…`, `glm-…`, `zai/…`). When the provider does not
  change, the reply says `Provider unchanged: <id>` rather than implying a switch.
  To change provider, set `model_provider` in `config.toml` and restart.

  Two selections are refused up front rather than failing at turn time: a model
  that is neither in the catalog nor a known alias, and a model whose provider has
  no reachable API key (checked against both the environment and the stored
  provider keys).

- `/approvals` shows the chat's active approval policy.
- `/approvals <untrusted|on-failure|on-request|never>` saves the approval
  policy for the chat and updates the current thread with
  `thread/settings/update`.
- `/compact` starts compaction for the active thread.
- `/diff` shows the git diff from `default_cwd` to the remote branch.
- `/skills` lists discovered skill names.

Ordinary follow-ups sent during a running turn use app-server steering. If a
turn temporarily cannot be steered, the connector holds up to 16 messages for
that conversation in FIFO order and shows the queue once. `/status` reports
`Idle`, `Working`, `Working · follow-ups queued`, `Awaiting approval`,
`Recovering queued input`, or `Blocked`, plus model, workspace, topic, queue
depth, the most recent successful Telegram contact or error, and the next useful
action.

Photos and image documents remain native image inputs. Supported non-image
documents (text/source, JSON, PDF, and XML/YAML) are downloaded into the
connector-owned media directory and described to the agent with their local
path, original name, MIME type, byte count, and SHA-256. The defaults accept up
to 10 MiB per file, retain media for seven days, and cap the media store at
256 MiB; configure those bounds with `max_attachment_bytes`,
`media_retention_days`, and `max_media_store_bytes`. Cleanup runs at startup and
during ingestion. Archives, executables, and unrelated binary media are
rejected rather than unpacked or implied to be available.

`default_cwd` is the workspace used for Telegram-created turns. Set it to the
directory where the agent should work, not to the Corbanu Terminal source tree or all
of `$HOME`. Codex automatically loads `AGENTS.md` from that workspace. The setup
script defaults fresh installs to `~/corbanu-telegram`, reuses an existing
legacy `~/pfterminal-telegram`, creates the selected directory, and seeds
`AGENTS.md` there from `codex-rs/telegram/dist/AGENTS.md.template` when the
workspace does not already have one. Use `--workspace "$HOME"` only when you
intentionally want a home-rooted remote agent workspace.

The recommended setup path is:

```bash
corbanu telegram --setup
```

Installed Corbanu Terminal packages bundle the setup script and service templates;
`--setup` locates and runs that exact packaged copy. From a source checkout,
`codex-rs/scripts/setup-telegram.sh --chat-id 21000038` remains available for
scripted setup.

The script prompts for the bot token without echoing it when neither the
process environment nor configured environment file provides one. For
unattended setup,
provide `PFTERMINAL_TELEGRAM_TOKEN` through the process environment without
placing the value in a command argument or shell-history line.

The script resolves `CODEX_HOME` the same way `corbanu telegram` does,
writes the token to `~/.config/corbanu/telegram.env` on fresh installs (or reuses
an existing legacy `~/.config/pfterminal/telegram.env`), writes or merges the
`[telegram]` block, sets `default_cwd`, and backs up an existing `config.toml`
before editing it. On reruns, the script only changes `[telegram]` settings that
were explicitly passed on that invocation or are missing from the existing file,
so operator tuning such as `approval_policy = "on-failure"` is preserved. Do not
pass the bot token on the command line; use `PFTERMINAL_TELEGRAM_TOKEN`, an
existing env-file entry, or the interactive prompt.

`corbanu -c telegram.foo=... telegram` and profile overrides do not override
`[telegram]` connector settings today. The connector reads this table directly
from `CODEX_HOME/config.toml`; use the config file or setup script for Telegram
settings. Core settings such as model, cwd, approval policy, and sandbox posture
still resolve through the normal core config after the connector has loaded its
own table.

On Linux, the connector emits one advisory startup warning when the resolved
sandbox policy is not `danger-full-access` and cheap host probes indicate the
sandbox is unlikely to launch: `bwrap` is missing from `PATH`,
`/proc/sys/user/max_user_namespaces` reads as `0`, or
`/proc/sys/kernel/unprivileged_userns_clone` exists and reads as `0`, or
`/proc/sys/kernel/apparmor_restrict_unprivileged_userns` exists and reads as
`1`. The setup script also runs a live `bwrap --ro-bind / / true` probe when
`bwrap` exists. In a failing state, even simple shell commands can require
manual Telegram approval because the sandboxed launch fails before command
execution.

On a trusted single-user host where unprivileged user namespaces are unavailable,
set top-level `sandbox_mode = "danger-full-access"` so the always-on connector
can execute commands without sandbox-launch approval churn. This disables the
filesystem sandbox, so do not use it on shared or untrusted hosts; install
`bwrap` and enable unprivileged user namespaces instead. The setup script never
writes this setting silently: pass `--allow-danger-full-access` or, on an
interactive TTY, confirm the prompt that states this disables the sandbox
globally for all Corbanu Terminal surfaces. In non-interactive mode without the flag,
preflight failure exits non-zero and does not write `sandbox_mode`.

At startup, the connector emits loud warnings when the effective approval policy
resolves to `never` or the effective sandbox resolves to `danger-full-access`.

To keep the poller always on, install the user service:

```bash
codex-rs/scripts/setup-telegram.sh --chat-id 21000038 --install-systemd
systemctl --user daemon-reload
systemctl --user enable --now corbanu-terminal-telegram.service
```

The setup script runs `corbanu telegram --health` before installing a managed
service, then installs a concrete user unit: `ExecStart` is rewritten to the
absolute path returned by `command -v corbanu`, and `EnvironmentFile` is
rewritten to the actual `--env-file` path. The checked-in template remains
generic for review. The unit reads `CODEX_HOME` and `PFTERMINAL_TELEGRAM_TOKEN`
from the selected Telegram environment file, restarts automatically, and uses
`StartLimitIntervalSec=300` with `StartLimitBurst=5` so a persistent 409 conflict
from a second poller stops instead of restart-fighting forever. Run only one
poller per Telegram bot token.

Before enabling any service, run the local readiness probe:

```bash
corbanu telegram --health
```

It verifies the Bot API identity, non-empty authorization policy, group user
policy, writable state and workspace paths, the selected provider's known
credential requirement, and sandbox viability. It exits non-zero with the
failed boundary named.

On macOS, install the checked-in LaunchAgent through the same setup script:

```bash
codex-rs/scripts/setup-telegram.sh --chat-id 21000038 --install-launchd
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/org.corbanu.terminal.telegram.plist
```

On Windows, configure the connector first, verify it with
`corbanu telegram --health`, then install a current-user Scheduled Task:

```powershell
.\codex-rs\scripts\install-telegram-task.ps1
Start-ScheduledTask -TaskName 'Corbanu Terminal Telegram'
```

The task command contains no bot token; credentials remain in the Corbanu Terminal
vault. The task refuses duplicate instances and has bounded restart settings.

## Provider Overrides

Advanced users can still define custom providers under `[model_providers]`.
Prefer built-ins unless you need a different base URL, custom headers, or an
external bearer-token command.

Example custom OpenAI-compatible Chat provider:

```toml
model_provider = "custom-chat"
model = "some/model"

[model_providers.custom-chat]
name = "Custom Chat Provider"
base_url = "https://example.com/v1"
env_key = "CUSTOM_PROVIDER_API_KEY"
wire_api = "chat"
```

For inherited Codex configuration details, see:

- Basic configuration: <https://developers.openai.com/codex/config-basic>
- Advanced configuration: <https://developers.openai.com/codex/config-advanced>
- Full reference: <https://developers.openai.com/codex/config-reference>

## Lifecycle Hooks

Admins can set top-level `allow_managed_hooks_only = true` in
`requirements.toml` to ignore user, project, and session hook configs while
still allowing managed hooks from requirements and managed config layers. This
setting is only supported in `requirements.toml`; putting it in `config.toml`
does not enable managed-hooks-only mode.
