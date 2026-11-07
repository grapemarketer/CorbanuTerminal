# Test machine facts (what the tester can do and what is observed)

The cases will be run by a separate, code-blind tester (an AI model, GLM 5.3 Flash) inside an isolated macOS virtual
machine, one fresh machine per group of cases. Write every case so that such a tester can follow it literally with the
tools below; anything else it cannot do.

## The build and the network

- One build only: the release candidate, Corbanu Terminal 0.1.48. No earlier build is available: comparisons with "the
  earlier build" can use only the reference screens in `screens/`.
- The product's model is GLM 5.3 Flash on Z.AI (OpenAI-style connection) in every home except `oai1` (below). Replies
  take 10–60 seconds.
- The machine has no general internet. A relay outside the tester's reach handles the product's traffic:
  - **Z.AI:** for each request it records the UTC time, path, HTTP status, model, and **which account's key the product
    presented** (every key in the run is synthetic and of the form `agms-fake-<label>-<digits>`; the relay records
    `<label>`), then forwards it to the real Z.AI with one real key. So every Z.AI account "works", except: a key whose
    label starts with `bad` is answered `401` (rejected key), and a request with no key is answered `401`.
  - **OpenAI API (api.openai.com):** every request is answered `401 invalid API key` (there is no real OpenAI API key
    in the run); the relay records the key's label.
  - **ChatGPT (chatgpt.com, auth.openai.com):** only in the group that uses home `oai1`, passed through unopened to the
    real service; the relay records only the host, time and size.
  - Anything else is refused, and recorded.
- The relay log is not visible to the tester. A case can ask the tester to note UTC times; the integrator then checks
  the relay afterwards. Expected results may refer to it ("the relay shows the request carried `work`'s key").
- Amazon Bedrock is not reachable.

## Homes (prepared by the coordinator before the tester starts)

Each home is a separate Corbanu home (settings, vault, conversations) under `$AGMS_RUN/homes/<name>`. All keys are
synthetic. Unless noted, `config.toml` has `[features] named_accounts = true`, `model_provider = "zai"`,
`model = "glm-5.3-flash"`, the test project marked trusted, and file-based credential storage.

| Home | Contents |
| --- | --- |
| `zai3` | Z.AI accounts `default` (its key comes from the `ZAI_API_KEY` environment variable, label `zenv`), `work` (label `zwork`) and `personal` (label `zpersonal`); `[provider_accounts] zai = "personal"`. No conversations yet. |
| `zai3off` | Same accounts as `zai3`, but `named_accounts = false`. |
| `zaione` | Z.AI `default` only (environment key, label `zenv`); no named accounts, no `[provider_accounts]`. |
| `zainokey` | Z.AI configured, **no** `ZAI_API_KEY` and no stored default key (no usable default credentials); named account `work` (label `zwork`). No conversations yet. |
| `zainokeyoff` | Same as `zainokey`, but `named_accounts = false`. |
| `fresh` | Only a `config.toml` with `named_accounts = true`; no provider chosen, no keys, no `ZAI_API_KEY`. |
| `custom` | A custom provider `zpx` defined under `[model_providers.zpx]` (name "Z.AI proxy test", Z.AI's address, key from `ZAI_API_KEY`), `model_provider = "zpx"`; `ZAI_API_KEY` is set (label `zenv`); named account `work` of `zpx` (label `zpxwork`). |
| `oai1` | `model_provider = "openai"`, `model = "gpt-5.6-luna"`. Default OpenAI login: a synthetic API key (label `odefault`), which the relay always rejects. Named OpenAI account `t1`: a **real, disposable, free ChatGPT test account**, signed in by the coordinator (device code, approved by a person) just before the group starts. Only `gpt-5.6-luna` works for it. `t1` is the only live account in the whole run; the tester must not sign in any other ChatGPT account. After the group the coordinator signs `t1` out and deletes its credentials. |

A case may also have the tester change a home itself (edit `config.toml` with the shell, add or remove accounts with
the product's own commands), as a person would. New synthetic keys the tester types must follow
`agms-fake-<label>-<digits>`, using letters and digits only in `<label>`, for example `agms-fake-zextra-0001`; a label
starting with `bad` makes the relay reject it.

## Driving the product

The tester drives the product only through `agms-tui`, which records every action and screen:

- `agms-tui start --home <name> [--env NAME=value] [PRODUCT ARGS...]` runs `corbanu [PRODUCT ARGS...]` with that home,
  in a private terminal of 150×45, in the test project (a small git project). PRODUCT ARGS may start with a
  subcommand, for example `resume --last`, `resume --account ghost <id>`, `fork <id>`, `account list`,
  `account add zai extra`, `account remove zai work`, `login status`, `exec --account work "..."`; with none it starts
  the TUI.
- When the program exits, the terminal stays open: it shows `[agms] product exited with code N`, and text typed
  afterwards is echoed back on its own line (so a tester can check the terminal was left in a normal state).
- `agms-tui text '...'` types text (no Enter); `agms-tui key Enter|Escape|Up|Down|Tab|BSpace|C-c|C-d|...` presses
  keys; `agms-tui screen <label>` prints the screen; `agms-tui wait <regex> <seconds>` waits for text.
- `agms-tui --window B <command>` drives a second, separate terminal (for example a second TUI, or a CLI command
  while the first TUI keeps running). Several windows may use the same home at once.
- `agms-tui stop` ends every terminal.
- The tester also has a shell in its own run folder (`$AGMS_RUN`): it can read and edit a home's `config.toml` and list
  files, where a case says so. It cannot read the relay log or anything outside the virtual machine.
- Budget: about 80 minutes per group of cases (the session is ended at 90). Keep cases short; a group will hold
  roughly 3–6 cases.

## What the integrator can check afterwards

- The relay log (above).
- Every home's files after the run: `config.toml`, the conversation (session) files, including the provider and
  account each conversation recorded, the names of vault files and whether a default login file (`auth.json`) exists
  and whether its contents changed (its hash). Vault contents are encrypted and are not read.
- Every screen the tester captured and every key it pressed, with times.
