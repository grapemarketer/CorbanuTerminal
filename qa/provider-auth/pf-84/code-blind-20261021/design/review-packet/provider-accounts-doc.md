# Named provider accounts (`named_accounts`, off by default)

One key per provider is not enough when you keep a work and a personal account,
or want a worker on a different account than its coordinator. Named accounts
let a provider hold several accounts besides `default`, each a named key or
token in the vault (`corbanu account add <provider> <name>`, `corbanu account
list`). Turn the feature on with `--enable named_accounts` or
`[features] named_accounts = true`.

## Choosing an account

For a provider, the first of these wins: an explicit `--account` for that
provider (or a spawn's `account`), the account a resumed thread recorded (or a
worker's parent, or a fork's source conversation, runs on),
`[provider_accounts] <provider> = "<name>"`, `default`.

A fork (`/fork`, editing an earlier prompt, `/side`, `corbanu fork`) runs on
the account its source conversation runs on, and `/status` names it. A fork
cut at an earlier turn also uses the conversation's current account, not the
one that turn ran on, as a resume does.

- `--account <name>` selects an account of the provider you launch with. On
  `resume`, write `<provider>:<name>` if the thread runs on another provider.
- `--account <provider>:<name>` for **another** provider selects that
  provider's account for whatever in the session runs on it: spawned agents
  and TUI workers given that provider, and a TUI switch to it. The session's
  own provider keeps its own selection. `corbanu exec` cannot switch
  providers, so it refuses such an account when no spawned agent can run on
  that provider (`agents.enabled = false`, or `agents.provider_allowlist`
  excludes it). It cannot see the model's catalog, so with both
  `multi_agent` features off it accepts the account even when the model
  enables no spawn tool.
- `--account default` always means today's credentials, also with the feature
  off.

## Fail closed

A missing account is an error, never the default account. The session
provider's account and the account `--account` names (of any provider) are
checked before the session starts; another provider's `[provider_accounts]`
entry is checked when something starts on that provider:

```
account `ghost` of provider `zai` is not configured; add it with
`corbanu account add zai ghost` or pick another with `--account`
(see `corbanu account list`)
```

`corbanu exec` and resume fail at thread start. The TUI exits with this text
before any sign-in screen; with a remote app server, the server refuses at
thread start.

Accounts are selected per connection. `zai` (OpenAI-style) and
`zai-anthropic` (Claude-style) share `ZAI_API_KEY`, but `[provider_accounts]
zai = "work"` or `--account work` on `zai` selects the account for `zai`
only; a spawn that names `zai-anthropic` gets that connection's own selection.
(A spawn that only changes the model, and is moved to the other connection
because the model lives there, keeps its account, since both use the same
key.) A spawned agent the model starts on a connection that runs on the
default key while the other connection (or the parent) runs on a named
account is refused before it starts when no default key is set or stored,
instead of failing at its first request. Resuming an existing agent, or
reloading a stored one to deliver a message, is checked the same way. The
TUI's `/model` and spawns check the same.

A resumed thread runs on its recorded provider and account. `corbanu resume`
(an id, `--last` or the picker) and `/resume` use them without `--account`.
With `-m`, `-c model_provider` or a profile model, the thread runs on the
session's provider instead, and another provider's recorded account does not
apply.

- **No default key, or a home configured for another provider:** the TUI
  picks the thread first, so no sign-in screen appears. Esc in that picker
  quits, because a new session would need a sign-in.
- **`[provider_accounts]` names a missing account:** that entry does not
  apply to the thread, so resume works on the recorded account.
- **The recorded account no longer exists:** the TUI exits with the text
  above, prefixed with "This conversation ran on zai account `ghost` and
  resumes on it:". Inside a session, `/resume` shows it and stays on the
  current conversation; there the recovery is "add it or pick another account
  in `/providers`".
- **The recorded provider has no credentials in this home:** the TUI exits
  and says how to set them up (`corbanu -c model_provider="zai"`), instead of
  the configured provider's sign-in. If this home does not define that
  provider at all, the TUI names it instead of showing the sign-in.

`corbanu fork` keeps the launch check: a missing `[provider_accounts]` entry
refuses it before the fork is chosen, even though the fork would run on the
source conversation's account; `--account` picks another one.

An explicit `--account` of the same provider still wins. With the feature off,
the resume is refused and names the recorded account (`--account
<provider>:default` resumes on the default credentials). A fork of a saved
conversation follows the same rule, decided by the fork's own configuration
(including a client's per-request overrides), not the app server's; a fork of
an open conversation keeps that conversation's live account. A rejected key names
the account and its `corbanu account add` recovery; the default credential is
unchanged.

## Usage per account in `/status`

With the feature on, `/status` names the session's account on the provider
line and adds an **Account usage** line: the tokens this session used on each
account of the current provider, as the provider reported them. The provider
line names the account only when that provider has a named account in this
home (or the vault can't be read), or the session is on or used one; otherwise
it reads as it does without the feature. The Account usage line covers the
whole session: spawned agents (each toward its own account), side
conversations, automatic and `/review` reviews, and memory extraction. Only
token counts are shown: no prices and no cost records.

- One account is one line, such as
  `work · 35,100 input · 12,000 cached input · 6,247 output (300 reasoning)`.
  Before the first reply it reads `no usage yet`.
- Several accounts are a short table with the session's account marked
  `(active)`.
- **Input** is the uncached part, as on the **Token usage** line; cached input
  and reasoning appear only when the provider reports them, and reasoning is
  part of the output. Reviews are counted here but not on **Token usage**, so
  the rows can add up to more.
- Accounts appear by name only. Other providers' accounts are not listed; switch
  to that provider to see them.
- A new session starts from zero. A resumed conversation shows its earlier
  counts again, its spawned agents' included once the TUI reattaches them.
  Memory extraction outside a turn shows with the next count.

Developer-accounting builds also have `/cost`, which shows the same requests by
provider, account and model for one UTC day. For a session that stays within
one day, its token counts per account match these, except for background
memory consolidation, which `/cost` records under the conversation and
`/status` does not show.

`/cost` prices a request on OpenRouter, Vercel or the Corbanu API from the
charge that provider reported with the response (OpenRouter's `usage.cost`,
Vercel's `gatewayCost`, the Corbanu API's `usage.corbanu_charge_usd`), shown as
`Cost: $…, charged by OpenRouter` (or Vercel, or Corbanu API). If no charge was
reported (for example with your own upstream key on OpenRouter), the published
price is shown as `Estimated cost` (a lower bound if only part of it is known);
with neither, the request shows `no price available`. Request and attempt pages
say which was used on their `Price basis` line. Codex Auto Review, the
automatic approval reviewer, is one of these on an OpenAI API key: OpenAI
doesn't publish a price for it, and `/cost` says so.

A day with nothing recorded for the conversation says "no requests recorded"
only when the whole UTC day ends before the conversation started (as dated by
the clock that also dates its requests). Otherwise it
says "nothing recorded" (or "not recorded" for a day before this home's
accounting began): a request that failed to record, or one sent from a build
without developer accounting, leaves no record, so an empty day is not proof
that none was sent.

A provider you configure at Moonshot's Kimi Open Platform
(`base_url = "https://api.moonshot.ai/v1"`, Chat Completions, API key) is priced
from Moonshot's published rates for that route, whatever you name the
provider: `kimi-k3`, `kimi-k2.7-code`, `kimi-k2.7-code-highspeed` and
`kimi-k2.6`. K3 cache writes are priced at the five-minute rate, the lifetime
Moonshot applies when a request names none. A key sent in a configured header
or as a bearer token gets no price, as on any route. Other providers' costs are
estimates from published prices.

## Amazon Bedrock: AWS profile accounts

A Bedrock account stores only the name of an AWS profile (not secret, shown in
`corbanu account list` and `/providers`; up to 64 letters, digits and `-_.+@`,
starting with a letter or digit):

```
corbanu account add amazon-bedrock work --kind aws-profile --value corp-dev
corbanu account rename amazon-bedrock work prod
corbanu exec --account prod "..."
```

Its requests are signed with that profile's credentials from `~/.aws/config` and
`~/.aws/credentials` (or the files `AWS_CONFIG_FILE` and
`AWS_SHARED_CREDENTIALS_FILE` name), and nothing else: not the managed Bedrock
key, `AWS_BEARER_TOKEN_BEDROCK`, `AWS_PROFILE`, `AWS_ACCESS_KEY_ID` or
instance or container credentials (a profile whose `credential_source` asks for
them is refused). The region is `model_providers.amazon-bedrock.aws.region`
if set, else the profile's `region`. SSO profiles work once you have run
`aws sso login --profile <p>` yourself. A profile that is missing, has no
region or whose credentials fail is refused with that reason before any
request; Corbanu never falls back to another profile. The `default` account
keeps today's order (managed key, then `AWS_BEARER_TOKEN_BEDROCK`, then the
AWS SDK chain).

While the vault holds a named Bedrock account, `/model` in a session on
another provider also offers Bedrock's models, so an account chosen with
_Use for this session_ (or `--account amazon-bedrock:<name>`) can be reached.
Picking one without choosing an account (there, or with a
`[provider_accounts]` default) uses Bedrock's `default` account.

## OpenAI: ChatGPT and API-key sign-in accounts

A named OpenAI account holds its own sign-in, ChatGPT or OpenAI API key, in the
encrypted vault (no keychain item, never `auth.json`):

```
corbanu login --account work                  # browser sign-in
corbanu login --account work --device-auth    # device code
printenv WORK_OPENAI_KEY | corbanu login --account work --with-api-key
corbanu login --account work status
corbanu logout --account work                 # revokes and clears only `work`
corbanu exec --account work "..."
```

In `/providers`, _Add another account_ on OpenAI offers a device-code ChatGPT
sign-in or a masked OpenAI API key, whatever provider the session runs on.
Rows show only the kind, an email hint (`w…@example.com`), the plan and a
fingerprint.

- The default login stays where it is (`auth.json` or the `Codex Auth`
  keychain item) and is never read, refreshed, replaced or logged out by a
  named account. `OPENAI_API_KEY`, `CODEX_API_KEY` and externally provided
  tokens are never used for a named account.
- A refreshed token is saved back to that account only. One process shares
  one sign-in and refresh lock per account; other processes pick a refresh
  up from the vault.
- A session on a named account uses it for everything that needs the
  sign-in: model requests, refresh, connectors, plugin installs and listings,
  usage and rate limits (also before the first reply), the `/apps` directory,
  ChatGPT settings, analytics, the model list and the `/status` account line.
  The default login is never sent. A request the account cannot serve (for
  example, a plugin listing on an API-key account, or any request on an
  account without a usable sign-in) goes out without it or not at all, never
  on the default login. A mid-session switch to another provider moves all of
  them to that provider's account (or the default login). Requests the process
  makes outside a conversation follow its open sessions: sessions on one named
  account use it; sessions only on the default login, or on providers without
  OpenAI sign-in, use the default login; sessions on different OpenAI accounts
  (for example a pane on `work` and one on the default login) send none of
  them and say why. A conversation you left (it stays loaded for a while)
  does not count while no turn runs in it. A `resume` or `fork`, and an app
  server or daemon with named accounts on in a home that has a named OpenAI
  sign-in account (or selects one in `[provider_accounts]`), send none until
  a conversation starts, because its account is known only then. A server
  in a home with only the default login uses it, as before, until a
  conversation on a named account added while it runs; from then on it sends
  none outside a conversation.
- `forced_login_method` and `forced_chatgpt_workspace_id` apply to named
  accounts too: a sign-in of the other kind or another workspace is refused.
  A session on a named account checks them against that account only and
  leaves the default login alone even when they rule it out; so do `resume`,
  `fork` and `exec resume`, whose thread's account is known only later. Such
  a default login is then not used by that process, and a thread on the
  default account (a switch to it, a spawned agent, a resumed default thread)
  is refused with the reason instead of logging the login out. With
  `named_accounts` on, an app server (`corbanu app-server`, or the local
  daemon a TUI reuses) does the same for every client: it never logs out a
  default login these settings rule out and refuses to use it. Such a TUI
  launch never reuses a running local daemon, which may already serve
  another session on another account: it starts its own app server and
  says so. A default session still logs a ruled-out default login
  out at startup, daemon or not. While a forced login rules the default
  out, `corbanu fork` of a named thread without `--account` shows the
  default sign-in screen; pass `--account <name>` to fork it on its
  account. Only a
  configured `forced_login_method` counts: a session on another provider (such
  as Z.AI) can still sign in, select and use a named ChatGPT account.
- A named account with no usable sign-in is refused with the reason and its
  recovery; a request is never sent without its credentials.
- Signing the same ChatGPT user in under two accounts (or as `default` and a
  named account) is allowed with a warning: they share that user's limits,
  and signing out of one can end the other's session.
- A refresh the server rejects ends the turn and names the recovery,
  `corbanu login --account <name>`; the default login is unchanged.
