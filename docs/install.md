# Install And First Run

This page is the new-machine setup runbook for Corbanu Terminal. It covers the
binary, provider credentials, the encrypted vault, and model selection.

## System Requirements

| Requirement      | Details                                                          |
| ---------------- | ---------------------------------------------------------------- |
| Operating system | macOS 12+, Ubuntu 20.04+/Debian 10+, or Windows 11 (x64, native) |
| Git              | 2.23+ recommended                                                |
| RAM              | 4 GB minimum, 8 GB recommended                                   |
| Rust             | Required only for source builds                                  |
| Node.js          | Required only for npm/package development                        |
| Linux sandbox    | `bubblewrap` recommended on Linux                                |
| Linux keyring    | A Secret Service provider such as GNOME Keyring is recommended   |

On Ubuntu/Debian hosts, install the common runtime helpers:

```bash
sudo apt-get update
sudo apt-get install -y git curl ca-certificates bubblewrap libsecret-1-0
```

`bubblewrap` is used by the Linux sandbox. If it is missing, Corbanu Terminal can use
its bundled fallback, but installing the OS package removes the startup warning.

On macOS, the release installer only needs the system `curl`, `tar`, and shell
tools that ship with macOS. Source builds also need Apple's command line tools:

```bash
xcode-select --install
```

## Install Options

### Linux / Terminal Release Installer

The standalone installer downloads a release from `CorbanuCore/CorbanuTerminal` and
verifies the release artifact digest. This is the preferred path for Linux
users and for macOS users who prefer terminal install over a DMG.

```bash
curl -fsSL https://github.com/CorbanuCore/CorbanuTerminal/releases/latest/download/install.sh | sh
```

The release installer creates the `corbanu` launcher and leaves any existing
stock `codex` command alone. State resolution is deterministic: `CORBANU_HOME`, then
`PFTERMINAL_HOME`, then an explicit `CODEX_HOME` wins; otherwise
`$HOME/.corbanu` wins when present; otherwise an existing
`$HOME/.pfterminal` is reused in place; otherwise a fresh install creates
`$HOME/.corbanu`. The installer never copies, merges, or deletes either state
directory. Override the defaults only when you need a custom install location:

```bash
curl -fsSL https://github.com/CorbanuCore/CorbanuTerminal/releases/latest/download/install.sh |
  CORBANU_INSTALL_DIR="$HOME/.local/bin" \
  CORBANU_HOME="$HOME/.corbanu" \
  sh
```

The installer requires a published GitHub release. If a fresh clone has no
release yet, use the source build fallback below.

### Windows Release Installer

Corbanu Terminal runs natively on Windows; WSL2 is not required. In PowerShell:

```powershell
irm https://github.com/CorbanuCore/CorbanuTerminal/releases/latest/download/install.ps1 | iex
```

On x64 Windows the installer downloads the `x86_64-pc-windows-msvc` package,
verifies its SHA-256 digest, installs the `corbanu` command in
`%LOCALAPPDATA%\Programs\Corbanu Terminal\bin`, and adds that folder to your
user `PATH` (restart PowerShell windows that were already open). State
resolves as on Linux and macOS, with `%USERPROFILE%\.corbanu` as the default
home. `CORBANU_INSTALL_DIR` and `CORBANU_HOME` override the defaults. Releases
publish an x64 Windows package only: on Arm64 Windows the installer looks for an
`aarch64-pc-windows-msvc` package, finds none, and stops with an error.

For the Windows sandbox modes, see [Sandbox](sandbox.md). If an upgrade refuses
to retarget a junction or still runs an old version, see the
[Windows upgrade diagnostics](windows-upgrade-diagnostics.md). To run Corbanu
Terminal inside WSL2 instead, use the Linux installer above in the WSL2 shell.

### macOS DMG

Download the latest DMG from
[GitHub Releases](https://github.com/CorbanuCore/CorbanuTerminal/releases/latest):

- `CorbanuTerminal-aarch64-apple-darwin.dmg` for Apple Silicon Macs.
- `CorbanuTerminal-x86_64-apple-darwin.dmg` for Intel Macs.

Open the DMG and double-click `install.command`. The DMG contains the exact
standalone package archive plus `corbanu-terminal-package_SHA256SUMS`; the installer
uses the bundled archive and verifies it before installation.

### Release Build For Maintainers

Release artifacts are built by the manual `corbanu-terminal-release` GitHub Actions
workflow. It does not run on every push. Run it only when you want
installer-ready macOS, Linux and Windows artifacts for the current Cargo version.

Before dispatching, follow the [release-preparation checklist](../AGENTS.md#release-gate)
to carry pending notes into the versioned release notes.

The workflow builds and smoke-tests these Corbanu Terminal artifacts:

```text
corbanu-terminal-package-aarch64-apple-darwin.tar.gz
corbanu-terminal-package-x86_64-apple-darwin.tar.gz
corbanu-terminal-package-aarch64-unknown-linux-musl.tar.gz
corbanu-terminal-package-x86_64-unknown-linux-gnu.tar.gz
corbanu-terminal-package-x86_64-pc-windows-msvc.zip
CorbanuTerminal-aarch64-apple-darwin.dmg
CorbanuTerminal-x86_64-apple-darwin.dmg
corbanu-terminal-package_SHA256SUMS
corbanu-terminal-dmg_SHA256SUMS
```

Leave `publish_release` disabled to do a build-only validation. Enable it to
create or update the matching `rust-vX.Y.Z` GitHub release. Enable
`make_latest` only when that release should become the default target for the
installer's `latest` resolution.

### Source Build

```bash
git clone https://github.com/CorbanuCore/CorbanuTerminal.git
cd CorbanuTerminal/codex-rs

curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
rustup component add rustfmt clippy

cargo install --locked just
cargo install --locked dotslash
cargo install --locked cargo-nextest

CARGO_NET_GIT_FETCH_WITH_CLI=true cargo build -p codex-cli --bin corbanu
```

The first source build can take 10-20 minutes on a fresh Mac because Cargo has
to fetch git dependencies and compile the full workspace. The
`CARGO_NET_GIT_FETCH_WITH_CLI=true` setting avoids intermittent macOS libgit2
fetch stalls seen with nested git dependencies.

Run the source-built binary from the workspace you want Corbanu Terminal to inspect:

```bash
cd ~/repos
/path/to/CorbanuTerminal/codex-rs/target/debug/corbanu
```

The source-built `corbanu` binary defaults `CODEX_HOME` to
`$HOME/.corbanu`; set `CODEX_HOME` only when you need a custom state
directory.

For repeated local use, install a wrapper on your `PATH`:

```bash
mkdir -p "$HOME/.local/bin" "$HOME/.local/share/corbanu/bin"
install -m 0755 /path/to/CorbanuTerminal/codex-rs/target/debug/corbanu \
  "$HOME/.local/share/corbanu/bin/corbanu"
cat > "$HOME/.local/bin/corbanu" <<'EOF'
#!/bin/sh
export CODEX_HOME="${CORBANU_HOME:-${PFTERMINAL_HOME:-${CODEX_HOME:-$HOME/.corbanu}}}"
exec "$HOME/.local/share/corbanu/bin/corbanu" "$@"
EOF
chmod 0755 "$HOME/.local/bin/corbanu"
```

Using the default `CODEX_HOME=$HOME/.corbanu` keeps Corbanu Terminal credentials,
vault data, sessions, logs, plugins, and skills separate from a stock Codex
install.

### npm Package

The canonical npm package is `@corbanucore/terminal` and exposes the `corbanu`
command. The launcher prefers the bundled `corbanu` binary
and defaults `CODEX_HOME` to `$HOME/.corbanu`.

```bash
npm install -g @corbanucore/terminal
corbanu --version
```

## Provider Setup

Corbanu Terminal ships built-in providers. You do not need to define these providers
manually in `config.toml`; you only need a credential for the provider you plan
to use.

| Provider     | Provider id   | Credential           | Current model examples                                                                       |
| ------------ | ------------- | -------------------- | -------------------------------------------------------------------------------------------- |
| OpenAI Codex | `openai`      | Codex account login  | `gpt-5.6-sol`, `gpt-5.6-luna`, `gpt-5.6-terra`                                               |
| Claude Plan  | `claude-plan` | Claude Code login    | `claude-opus-5-5-plan`, `claude-opus-5-plan`, `claude-fable-5-1-plan`, `claude-fable-5-plan` |
| Anthropic    | `anthropic`   | `ANTHROPIC_API_KEY`  | `claude-opus-5`, `claude-fable-5-1`, `claude-fable-5`                                        |
| Ambient      | `ambient`     | `AMBIENT_API_KEY`    | `z-ai/glm-5.2`                                                                               |
| Kimi Code    | `kimi-code`   | `KIMI_API_KEY`       | `k3`                                                                                         |
| Z.AI         | `zai`         | `ZAI_API_KEY`        | `glm-5.2`                                                                                    |
| DeepSeek     | `deepseek`    | `DEEPSEEK_API_KEY`   | `deepseek-flash`, `deepseek-v4-flash`                                                        |
| OpenRouter   | `openrouter`  | `OPENROUTER_API_KEY` | `deepseek/deepseek-v4-flash-0731`, `moonshotai/kimi-k3`, and other catalogued routes         |
| Meta         | `meta`        | `MODEL_API_KEY`      | `muse-spark-1.1`                                                                             |
| Baseten      | `baseten`     | `BASETEN_API_KEY`    | `zai-org/GLM-5.2`                                                                            |
| Vercel       | `vercel`      | `AI_GATEWAY_API_KEY` | `zai/glm-5.3-flash`, `zai/glm-5.3`, Vercel Kimi K3 and DeepSeek V4 Pro routes                |

The first-run provider picker and `/providers` expose account/plan routes and
all of the API-key rows above. Provider keys entered through the Corbanu Terminal UI
are stored in the encrypted vault and are available from any working directory.

You can also provide keys through environment variables:

```bash
export ANTHROPIC_API_KEY="..."
export AMBIENT_API_KEY="..."
export KIMI_API_KEY="..."
export ZAI_API_KEY="..."
export DEEPSEEK_API_KEY="..."
export OPENROUTER_API_KEY="..."
export MODEL_API_KEY="..."
export BASETEN_API_KEY="..."
export AI_GATEWAY_API_KEY="..."
```

Environment variables are useful for CI and temporary shells. For a normal
desktop/server setup, prefer the UI or `/vault` so the key is encrypted at rest
and listed in the Corbanu Terminal vault.

## Vault Setup

Corbanu Terminal stores provider API keys in the encrypted vault. Provider keys use
stable labels derived from their key names:

| Provider key         | Vault label                   |
| -------------------- | ----------------------------- |
| `ANTHROPIC_API_KEY`  | `provider/anthropic_api_key`  |
| `AMBIENT_API_KEY`    | `provider/ambient_api_key`    |
| `KIMI_API_KEY`       | `provider/kimi_api_key`       |
| `ZAI_API_KEY`        | `provider/zai_api_key`        |
| `DEEPSEEK_API_KEY`   | `provider/deepseek_api_key`   |
| `OPENROUTER_API_KEY` | `provider/openrouter_api_key` |
| `MODEL_API_KEY`      | `provider/model_api_key`      |
| `BASETEN_API_KEY`    | `provider/baseten_api_key`    |
| `AI_GATEWAY_API_KEY` | `provider/ai_gateway_api_key` |

The vault backend is the Codex managed-secrets substrate:

- encrypted data: `$CODEX_HOME/secrets/local.age`;
- passphrase storage: the OS keyring when available;
- Linux fallback: a local `0600` keyring fallback file only for the vault
  passphrase when no Secret Service is available;
- legacy fallback: `provider_auth.json` is read for old installs and removed
  after a successful vault write.

Inside the TUI:

```text
/vault
```

opens the vault action menu.

Useful commands:

```text
/vault list
/vault show provider/zai_api_key
/vault credential add
/vault credential delete provider/openrouter_api_key
```

Raw secrets must not be typed into chat. `/vault credential add` opens a masked
entry flow so the secret does not enter the conversation transcript or model
context.

## Model Selection

Use `/model` in the TUI or pass `-m` at startup.

```bash
corbanu -m gpt-5.6-luna                         # OpenAI Codex account
corbanu -m claude-opus-5                        # direct Anthropic API
corbanu -m k3                                   # Kimi Code
corbanu -m glm-5.2                              # Z.AI GLM 5.2
corbanu -m deepseek-v4-flash                    # direct DeepSeek Responses API
corbanu -m deepseek-flash                       # direct DeepSeek V4.1 Flash
corbanu -m deepseek/deepseek-v4-flash-0731     # pinned OpenRouter route
corbanu -m muse-spark-1.1                       # Meta
corbanu -m zai/glm-5.2-fast                     # Vercel GLM 5.2 Fast
corbanu -m zai/glm-5.3-flash                    # Vercel GLM 5.3 Flash
corbanu -m zai/glm-5.3                          # Vercel GLM 5.3
corbanu -m vercel/moonshotai/kimi-k3            # Vercel Kimi K3
corbanu -m vercel/deepseek/deepseek-v4-pro      # Vercel DeepSeek V4 Pro
```

The `/model` picker groups models by account/provider route. Current categories
include:

- account and coding-plan routes such as OpenAI, Claude Plan, Ambient, Kimi
  Code, Z.AI, and Corbanu API; and
- metered API-key routes such as Anthropic, DeepSeek, OpenRouter, Meta,
  Baseten, and Vercel.

Current visible model metadata:

| Model                                 | Provider                | Notes                                                     |
| ------------------------------------- | ----------------------- | --------------------------------------------------------- |
| `gpt-5.6-sol/luna/terra`              | OpenAI                  | Codex account model family                                |
| `claude-opus-5[-plan]`                | Anthropic / Claude Plan | Direct API and plan-backed variants                       |
| `k3`                                  | Kimi Code               | Current direct Kimi coding route                          |
| `deepseek-v4-flash`                   | DeepSeek                | Legacy name; DeepSeek now serves V4.1 Flash on it         |
| `deepseek-flash`                      | DeepSeek                | Direct Responses route; DeepSeek V4.1 Flash               |
| `deepseek/deepseek-v4-flash-0731`     | OpenRouter              | Exact pinned OpenRouter DeepSeek Flash route              |
| `deepseek/deepseek-v4-pro`            | OpenRouter              | OpenRouter DeepSeek V4 Pro route                          |
| `moonshotai/kimi-k3`                  | OpenRouter              | Metered Kimi K3 route                                     |
| `zai/glm-5.3-flash`                   | Vercel                  | Vercel AI Gateway GLM 5.3 Flash route                     |
| `zai/glm-5.3`                         | Vercel                  | Vercel AI Gateway GLM 5.3 route                           |
| `vercel/moonshotai/kimi-k3`           | Vercel                  | Vercel route; sends official `moonshotai/kimi-k3` slug    |
| `vercel/deepseek/deepseek-v4-pro`     | Vercel                  | Vercel route; sends official DeepSeek V4 Pro slug         |
| `muse-spark-1.1`                      | Meta                    | Meta API route                                            |
| `glm-5.2` and provider-specific slugs | Multiple                | Ambient, Z.AI, Baseten, Vercel, and OpenRouter GLM routes |

## Basic Verification

After installing and adding a provider key:

```bash
corbanu --version
corbanu
```

In the TUI:

```text
/vault
/model
/skills
```

Expected setup signs:

- `/vault` shows the provider key label you added.
- `/providers` includes OpenAI Codex Account plus API-key provider rows.
- `/model` shows Coding Plans and Pay Per API Call sections.
- `/skills` includes bundled Corbanu Terminal system skills such as Frontend Design.

## Development Commands

From the repository root:

```bash
cd codex-rs
cargo build -p codex-cli --bin corbanu
just fmt
just test -p codex-tui
```

Avoid `--all-features` for routine local runs because it increases build time
and `target/` disk usage by compiling additional feature combinations.

Run tests with `just test`, for example `just test -p codex-core`. It runs
them against a disposable home, unsets provider credentials such as
`OPENAI_API_KEY`, turns off native credential-store calls (see
[test isolation](./development/test-isolation.md)), and sets
`RUST_MIN_STACK=8388608` (8 MiB). Plain `cargo test -p codex-core` does none of
that isolation; in a debug build it also needs `RUST_MIN_STACK=8388608` (as CI
and Bazel set), because many tests overflow the default 2 MiB test-thread stack
without it.

## Tracing

The TUI records diagnostics in bounded local stores by default. Set `log_dir`
explicitly to enable a plaintext TUI log for a run:

```bash
corbanu -c log_dir=./.corbanu-log
tail -F ./.corbanu-log/codex-tui.log
```

The non-interactive mode defaults to `RUST_LOG=error`, but messages are printed
inline, so there is no separate log file to monitor unless `log_dir` is set.
