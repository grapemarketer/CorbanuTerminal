# Example Config

Create or edit:

```text
$CODEX_HOME/config.toml
```

Recommended home for Corbanu Terminal:

```bash
export CORBANU_HOME="$HOME/.corbanu"
```

## Ambient Default

```toml
model_provider = "ambient"
model = "zai-org/GLM-5.2-FP8"
```

Store the key through onboarding or `/vault`; temporary shell fallback:

```bash
export AMBIENT_API_KEY="..."
```

## Z.AI API (pay per use)

```toml
model_provider = "zai"
model = "glm-5.2"
```

Temporary shell fallback:

```bash
export ZAI_API_KEY="..."
```

## OpenRouter Metered Models

```toml
model_provider = "openrouter"
model = "z-ai/glm-5.2"
```

Other visible OpenRouter models:

```toml
model = "minimax/minimax-m3"
model = "google/gemini-3.5-flash"
```

Temporary shell fallback:

```bash
export OPENROUTER_API_KEY="..."
```

## Baseten GLM

```toml
model_provider = "baseten"
model = "zai-org/GLM-5.2"
```

Temporary shell fallback:

```bash
export BASETEN_API_KEY="..."
```

## Vercel AI Gateway GLM

```toml
model_provider = "vercel"
model = "zai/glm-5.2"
```

Fast variant:

```toml
model = "zai/glm-5.2-fast"
```

Temporary shell fallback:

```bash
export AI_GATEWAY_API_KEY="..."
```

## Sandbox And Logging Example

```toml
model_provider = "zai"
model = "glm-5.2"
sandbox_mode = "workspace-write"
log_dir = "./.corbanu-log"
```

Provider credentials do not belong in this file for normal use. Store them in
the vault:

```text
/vault
/vault credential add
```

## Telegram Connector

See [Telegram Connector](config.md#telegram) for the canonical `[telegram]`
table, setup script, workspace `AGENTS.md` seeding, sandbox caveat, and systemd
service instructions.
