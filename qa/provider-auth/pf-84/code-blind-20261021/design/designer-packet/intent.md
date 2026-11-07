# Feature packet: named provider accounts (PF-84), acceptance before the feature flag is removed

Corbanu Terminal is a terminal (TUI) coding agent for macOS, Linux and Windows. The `named_accounts` feature lets one
model provider (for example Z.AI, or OpenAI) hold several accounts besides `default`, such as `work` and `personal`.
Until now it has been off by default; the owner is about to turn it on for everyone. This acceptance run tests the build
that will ship, with the feature switched on in each test home's `config.toml` exactly as it will be by default. The
user documentation for the feature is `provider-accounts-doc.md` in this packet (it still says "off by default"; that
is the only part about to change).

## What the feature must do (the owner's acceptance criteria, in user terms)

1. **Accounts per provider.** `corbanu account list|add|remove` (keys read from stdin, never from the command line) and
   `/providers` in the TUI (account rows showing the kind and a fingerprint, the current session's and the default's
   markers; add, rename, remove, use for this session, make default) manage several accounts per provider. Removing the
   account the current session uses asks for a replacement first.
2. **Choosing an account.** `--account <name>` (or `<provider>:<name>`) on the TUI, `corbanu exec` and `corbanu resume`
   picks the account; `[provider_accounts] <provider> = "<name>"` in `config.toml` picks the one new sessions use.
   Precedence for a provider: an explicit `--account` wins, then the account a resumed conversation recorded (or a
   fork's source conversation runs on), then `[provider_accounts]`, then `default`.
3. **Fail closed, with recovery.** A missing or unusable account is an error that names the account and says how to
   recover (`corbanu account add …`, `--account`, or `/providers` inside the TUI). It never silently falls back to
   `default`, and it never shows the sign-in (onboarding) screens of the home's provider instead.
4. **Conversations keep their account.** Resume uses the account the conversation recorded; forks (`/fork`, editing an
   earlier prompt, `/side`, `corbanu fork <id>`) and spawned agents run on the account of their source.
5. **`/status` and usage.** `/status` names the account on the "Model provider" line and has an "Account usage" line
   with tokens per account. Owner decisions (2026-10-20/21):
   - D86: with the feature on, the "Model provider" line names the account only when that provider has a named account
     in this home (or the session is on, or has used, a named account of it). Otherwise the line is exactly as with the
     feature off; for the default OpenAI login there is no such line at all.
   - D125: the "Account usage" line follows the same rule. A home with no named accounts looks exactly as it does with
     the feature off.
   - D124: `/status` must never stall reading the encrypted vault. Corbanu works the answer out in the background (at
     session start, after any account change, when `/providers` opens and after each `/status`) and `/status` shows the
     last answer. Until the answer is known, or if the vault cannot be read, the account is shown rather than hidden. So
     an account added by another process appears on a later `/status`, not necessarily the very next one.
6. **Secrets never shown.** No key, token or part of one appears on screen or in command output. `corbanu login status`
   says "API key configured" (plus a fingerprint) and shows no fragment of the key (decision REG-02).
7. **Named ChatGPT sign-in accounts (OpenAI).** A ChatGPT account can be added as a named OpenAI account by device code
   (`corbanu login --account <name> --device-auth`, or `/providers` → add another account). Its row shows "ChatGPT
   sign-in", an email hint, the plan and a fingerprint. The default OpenAI login is never read or changed by a named
   account.
8. **Feature off = unchanged.** With `named_accounts = false`, behaviour and text are as they were before the feature:
   no account suffix and no "Account usage" line in `/status`.

## Items the owner asked this run to cover specifically

- **D136: removing an account while a session is running on it.** A TUI session runs on a named ChatGPT sign-in
  account (a disposable, free test account, real and live). While that session is still open, the account is removed
  from another terminal. What should happen next in the running session: its next request must not succeed on, or fall
  back to, any other account (in particular not the default login); the user must be told which account is gone and how
  to recover; `/status` must not claim the session is still healthy on that account. Nothing the session does may
  re-create the removed account.
- **#426 (missing account at TUI start), two legs never checked end to end:**
  - `corbanu resume --account ghost <id>`, where `ghost` is not an account of the conversation's provider. Earlier
    builds opened the provider's API-key onboarding loop instead of refusing; the fix is that the TUI exits with the
    "not configured … `corbanu account add …`" text before any onboarding.
  - The same `--account ghost` on a **custom provider** (one the user defined under `[model_providers.<id>]` in
    `config.toml`). On an earlier build this opened the full provider picker, and leaving it landed on the OpenAI
    device-code sign-in screen.
- **Earlier acceptance legs with evidence gaps** (from the 2026-10-12 code-blind design for forks/resume/recovery; the
  original case text is quoted, then what was missing):
  - ESC-01 "Esc in the `corbanu resume` picker when there are no credentials: the process exits cleanly with code 0 and
    the terminal is restored; no sign-in or onboarding screen appears." Missing: that the terminal was restored was
    never observed directly.
  - ESC-03 "Esc in the in-session `/resume` picker, with a draft in the composer and at least one turn: back in the same
    session; session id, account and transcript unchanged, and the draft is still there." Missing: the draft part;
    the earlier tester found no way to have a draft in the composer while opening `/resume`.
  - ESC-04 "Normal onboarding is not broken: in a fresh home with no credentials, `corbanu` (not a resume) still shows
    onboarding as before." Missing: no comparison with the earlier build (reference screens 12 and 14 show it).
  - GH-02 "`[provider_accounts]` names `ghost`, and the conversation's recorded account `work` has been removed;
    `corbanu resume <id>`: the error names `work` and gives `corbanu account add zai work`; it ideally also mentions
    `ghost`; nothing falls back to `default`; non-zero exit." Missing: `ghost` was not mentioned (advisory).
  - PR-01 "Full precedence order, with a conversation recorded on `work` and `[provider_accounts] zai = "personal"`:
    resume with `--account personal`; resume with no flag; new session with no flag; new session with no flag after
    removing `[provider_accounts]` → accounts used `personal`, `work`, `personal`, `default`, each confirmed in
    `/status`." Missing: `/status` was not captured for every step, and two steps could not be told apart by the
    provider's response.
  - REG-01 "With `named_accounts` off, `/fork`, resume of a `work` conversation, ESC-01 and ESC-03 behave and read as
    on the build from before the change; `/status` shows no account suffix or per-account usage." Missing: with the
    feature off, `corbanu resume` in a home without usable default credentials opened onboarding (as the earlier build
    did), so the picker-Esc path itself was not reached.
- **Not in scope** (owner decisions): live Amazon Bedrock (mock-only, D99); a live token refresh being saved back to the
  account (accepted on mock tests, D135); two known wording issues (#1205, already fixed, and #1208: a misleading
  "legacy /spawn hierarchy" note on resume) are not blockers (D138).

## Users

Developers running the TUI in a terminal, in fresh or existing homes, sometimes with several providers configured;
most have no named accounts at all, so for them nothing may change when the feature is switched on.

## Reference screens (`screens/`)

Text captures of the real TUI on builds from 2026-10-12, made for the earlier design (not the build under test):
REF-01 `/fork` then `/status`; REF-02 `/side` then `/status`; REF-03–04 resume of a Z.AI conversation in an
OpenAI-configured home, then `/status`; REF-05 resume with `[provider_accounts] zai = "ghost"`, then `/status`;
REF-06–07 the `corbanu resume` picker and Esc in a home with no default key; REF-08 Esc in `/resume` inside a session;
REF-09 in-app `/resume` of a `work` conversation then `/status`; REF-10 in-app `/resume` of a conversation whose account
was removed. REF-11–15 are the same situations on the build from before those fixes (REF-12 and REF-14 show the
onboarding screens of that build). The build under test is newer; wording may differ.
