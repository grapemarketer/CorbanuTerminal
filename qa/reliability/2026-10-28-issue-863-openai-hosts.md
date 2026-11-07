# #863 items 3–4: custom OpenAI Responses provider, live check (2026-10-28)

Config (disposable home, `features.multi_agent_v2 = true`, real OpenAI API key from the vault, model `gpt-6-luna`):

```toml
[model_providers.myopenai]
base_url = "<host below>"
env_key = "OPENAI_API_KEY"
wire_api = "responses"
billing = "pay_per_use"
```

Prompt: spawn one sub-agent that replies `PONG`, wait, report it.

| Base URL | Installed v0.1.48 (before) | This branch (after) |
| --- | --- | --- |
| `https://api.openai.com/v1` | (fixed by #852/#871) | native `collaboration.spawn_agent` on `myopenai`, child replied PONG, exit 0 |
| `https://api.openai.com./v1` | HTTP 400 `collaboration.followup_task is reserved …` (#820) | native `collaboration.spawn_agent` on `myopenai`, child replied PONG, exit 0 |
| `https://eu.api.openai.com/v1` | 401: endpoint needs a project with geography restrictions | not testable with this key |
| `https://us.api.openai.com/v1` | 401 `incorrect_hostname` | not testable with this key |

Regional domains are covered by unit tests only (the vault key's project has no regional access). The list of
prefixes (`ae au ca eu gb in jp kr sg us`) is OpenAI's data-controls guide, checked 2026-10-28.
Proxies and gateways still get the provider-neutral tools.
