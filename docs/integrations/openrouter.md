# OpenRouter Integration

OpenRouter is a built-in metered provider for Corbanu Terminal.

## Current Provider

The built-in OpenRouter provider is defined in
`codex-rs/model-provider-info/src/lib.rs`:

| Field                | Value                          |
| -------------------- | ------------------------------ |
| Provider id          | `openrouter`                   |
| Display name         | `OpenRouter`                   |
| Base URL             | `https://openrouter.ai/api/v1` |
| API key env var      | `OPENROUTER_API_KEY`           |
| Wire API             | `chat`                         |
| OpenAI auth required | `false`                        |
| WebSockets           | `false`                        |

Auth guidance shown to users:

```text
Set OPENROUTER_API_KEY to your OpenRouter API key.
```

## Current Models

OpenRouter models bundled in `codex-rs/models-manager/models.json`. Grok 4.6,
Grok 4.5, DeepSeek V4 Pro, DeepSeek V4 Pro 0813 and DeepSeek V4 Flash 0731 are
hidden from `/model` and kept for saved configs; the others are listed:

| Slug                                     | Display name                      | Listed pricing text                                            |
| ---------------------------------------- | --------------------------------- | -------------------------------------------------------------- |
| `x-ai/grok-4.7`                          | OpenRouter Grok 4.7               | `$1.60/M input`, `$4.8/M output`                               |
| `deepseek/deepseek-v4.1-flash`           | OpenRouter DeepSeek V4.1 Flash    | `$0.15/M input`, `$0.6/M output`                               |
| `xiaomi/mimo-v2.6-pro`                   | OpenRouter MiMo V2.6 Pro          | `$0.435/M input`, `$0.87/M output`                             |
| `z-ai/glm-5.3-flash`                     | OpenRouter GLM 5.3 Flash          | `$0.15/M input`, `$0.5/M output`                               |
| `nvidia/nemotron-3-ultra-550b-a55b:free` | OpenRouter Nemotron 3 Ultra Free  | `$0/M input`, `$0/M output`                                    |
| `x-ai/grok-4.6`                          | OpenRouter Grok 4.6               | `$2.00/M input`, `$0.50/M cached input`, `$6.00/M output`      |
| `deepseek/deepseek-v4-flash-0731`        | OpenRouter DeepSeek V4 Flash 0731 | `$0.14/M input`, `$0.0028/M cached input`, `$0.28/M output`    |
| `deepseek/deepseek-v4-pro-0813`          | OpenRouter DeepSeek V4 Pro 0813   | `$0.435/M input`, `$0.003625/M cached input`, `$0.87/M output` |
| `deepseek/deepseek-v4-pro`               | OpenRouter DeepSeek V4 Pro        | `$0.435/M input`, `$0.87/M output`                             |
| `moonshotai/kimi-k3`                     | OpenRouter Kimi K3                | `$3.00/M input`, `$0.30/M cached input`, `$15.00/M output`     |
| `x-ai/grok-4.5`                          | OpenRouter Grok 4.5               | `$2.00/M input`, `$6.00/M output`                              |
| `minimax/minimax-m3`                     | OpenRouter MiniMax M3             | `$0.60/M input`, `$2.40/M output`                              |
| `google/gemini-3.5-flash`                | OpenRouter Gemini 3.5 Flash       | `$1.50/M input`, `$9.00/M output`                              |
| `tencent/hy3:free`                       | OpenRouter Tencent Hy3 Free       | `$0/M input`, `$0/M output`                                    |

Grok 4.7's and Grok 4.6's listed rates double when the prompt exceeds 200,000
tokens. DeepSeek V4.1 Flash and MiMo V2.6 Pro rates vary by provider and time
of day, and peak prices are higher. Nemotron 3 Ultra Free is a free endpoint;
rate limits and provider data-use terms apply.

`openrouter/owl-alpha` is retired: OpenRouter lists no endpoints for it
(checked 2026-10-28), so requests to it are expected to fail. The hidden
catalogue row stays only so a saved config resolves to its own entry instead
of another model; pick a current model with `/model`.

DeepSeek V4 Flash is intentionally pinned to the exact
`deepseek/deepseek-v4-flash-0731` slug. Direct DeepSeek uses provider
`deepseek` and model `deepseek-v4-flash`; the two billing and routing paths are
distinct and must remain visible as such.

## Catalogue Limits And Request Limits

Catalogue metadata does not promise that every OpenRouter endpoint accepts
the same request. Check the provider and wire route before treating an output
value as a request limit.

The [maintainer decision dated 2026-10-21](https://github.com/CorbanuCore/CorbanuTerminal/issues/129#issuecomment-6322464954)
retains these values as conservative catalogue policy until a route is
qualified. The [bundled catalogue checked on 2026-10-22](https://github.com/CorbanuCore/CorbanuTerminal/blob/2469e554dd1760917eb3594621abc152ce29c184/codex-rs/models-manager/models.json)
contains:

| OpenRouter model               | Catalogue field                           | Value, in tokens | Scope                                                        |
| ------------------------------ | ----------------------------------------- | ---------------- | ------------------------------------------------------------ |
| `deepseek/deepseek-v4.1-flash` | `max_output_tokens`                       | 384,000          | Bundled output-limit metadata retained by application policy |
| `z-ai/glm-5.3-flash`           | `context_window` and `max_context_window` | 1,048,576 each   | Bundled context metadata retained by application policy      |

These numbers do not establish the capacity of every OpenRouter endpoint.
The [dated 2026-10-04 provider reading](https://github.com/CorbanuCore/CorbanuTerminal/issues/129#issuecomment-5985201184)
records different endpoint limits; that historical reading is not a guarantee
of current availability or request acceptance. A model-wide maximum or a
`top_provider` value is not evidence that a particular fallback endpoint will
accept the same request.

An output value in the catalogue also does not mean every wire sends that
value. The built-in `openrouter` Chat Completions route's
[request type](https://github.com/CorbanuCore/CorbanuTerminal/blob/2469e554dd1760917eb3594621abc152ce29c184/codex-rs/codex-api/src/common.rs#L596)
has no output-limit field. The `openrouter-anthropic` route instead
[sends the effective model metadata's output limit as `max_tokens`](https://github.com/CorbanuCore/CorbanuTerminal/blob/2469e554dd1760917eb3594621abc152ce29c184/codex-rs/core/src/client.rs#L2654).
Discovery can supply model metadata; a
[missing output value is filled from the bundle](https://github.com/CorbanuCore/CorbanuTerminal/blob/2469e554dd1760917eb3594621abc152ce29c184/codex-rs/models-manager/src/manager.rs#L524),
which is not a clamp on an output value that discovery does supply.

The OpenRouter GLM context policy is separate from the native Z.AI
`glm-5.3-flash` row. Check the provider and exact model slug when comparing
limits.

Product scope: [Shipping MVP — LIVE](../corbanu-product-spec.md#shipping-mvp--live),
**Multi-provider inference**, whose provider list includes “Z.AI, DeepSeek,
OpenRouter”.

## Model And Provider Selection

Corbanu Terminal maps these model slugs to provider `openrouter` in
`codex-rs/tui/src/chatwidget/model_popups.rs`.

Examples:

```bash
corbanu -m deepseek/deepseek-v4-flash-0731
corbanu -m deepseek/deepseek-v4-pro-0813
corbanu -m deepseek/deepseek-v4-pro
corbanu -m moonshotai/kimi-k3
corbanu -m x-ai/grok-4.6
corbanu -m x-ai/grok-4.5
corbanu -m minimax/minimax-m3
corbanu -m google/gemini-3.5-flash
```

## Vault Behavior

OpenRouter keys saved through onboarding are stored in the encrypted vault at:

```text
provider/openrouter_api_key
```

The environment variable `OPENROUTER_API_KEY` is still supported for temporary
shells and automation.

## Source

- `codex-rs/model-provider-info/src/lib.rs`
- `codex-rs/models-manager/models.json`
- `codex-rs/tui/src/onboarding/auth.rs`
- `codex-rs/tui/src/chatwidget/model_popups.rs`
- `codex-rs/login/src/auth/provider_key_vault.rs`
