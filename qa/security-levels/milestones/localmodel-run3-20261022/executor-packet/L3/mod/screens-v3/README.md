# Screens of the candidate under test (2026-10-16)

Text copies of the terminal screen of the exact candidate for this re-run (Corbanu Terminal 0.1.48, the signed build
of 2026-10-16), captured by the integrator on 2026-10-16 between 10:05Z and 10:20Z on a Mac with a disposable
profile and the `ledger-notes` test project, GLM 5.3 Flash on `zai`, the milestone feature flags on (protected launch
included). Folder paths are shortened to `/Users/tester/run`. These are examples of real behaviour and wording, not
test cases or results; the model's own words vary between runs. `screens/` holds the first run's design-time screens
(an older candidate, 2026-10-14) that the frozen cases refer to by number.

Session 1 (starts at Permissive):

| File | What it shows |
| --- | --- |
| 00-start | First start in the project: trust prompt |
| 01-picker | `/security` while Permissive (protected launch on) |
| 02-moderate-review | Down, Enter: the Moderate review (bottom part) |
| 03-moderate-saved | Enter: Saved: Moderate |
| 04-after-restart | `r`: the restarted session |
| 05-grants | `/security`, `g` |
| 06-revoke-review | Enter on "Revoke all active authority": its review (then Esc, Esc, Esc) |
| 07-outside-prompt | Asking for an append to `../outside-notes.txt` with a request to run outside the sandbox: the prompt (then Esc) |

Session 2 (same profile and folder, quit and started again with the fixture MCP server `notes`; Moderate active):

| File | What it shows |
| --- | --- |
| 09-mcp-post-prompt | Asking the agent to call `post_note` with text 'hello': the MCP tool approval prompt (then Esc) |
| 10-corbanu-config | Asking for `cat` of the product home's `config.toml`: the prompt |
| 11-corbanu-config-approved | Enter on option 1: the result |

Session 3 (fresh profile and project, Permissive, protected launch on):

| File | What it shows |
| --- | --- |
| 12-permissive-outside | Asking for an append to `../outside-notes.txt` with a request to run outside the sandbox |
