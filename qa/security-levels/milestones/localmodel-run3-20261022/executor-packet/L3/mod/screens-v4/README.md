# Screens of the candidate under test (2026-10-20)

Text copies of the terminal screen of the exact candidate for this re-run (Corbanu Terminal 0.1.48, the signed build
of main `8bf182de43`), captured by the integrator on 2026-10-20 between 03:20:05Z and 03:22:28Z on a Mac with a
disposable profile and the `ledger-notes` test project, GLM 5.3 Flash on `zai`, the milestone feature flags on
(protected launch included). Folder paths are shortened to `/Users/tester/run`. These are examples of real behaviour
and wording, not test cases or results; the model's own words vary between runs. `screens-v3/` holds the previous
re-run's design-time screens (an older candidate, 2026-10-16) that amendment v3 cites as "screens-v3 NN";
`screens/` holds the first run's.

Session 1 (starts at Permissive):

| File | What it shows |
| --- | --- |
| 00-start | First start in the project: trust prompt |
| 01-picker | `/security` while Permissive (protected launch on) |
| 02-moderate-review | Down, Enter: the Moderate review (bottom part) |
| 03-moderate-saved | Enter: Saved: Moderate |
| 04-after-restart | `r`: the restarted session |
| 05-grants | `/security`, `g` (nothing approved yet in this session) |
| 06-revoke-review | Enter on "Revoke all active authority": its review (then Esc, Esc, Esc) |

Session 2 (same profile and folder, quit and started again with the fixture MCP server `notes`; Moderate active;
`shared-note.txt` holds `plain note for the revoke check`):

| File | What it shows |
| --- | --- |
| 07-mcp-start | The new session |
| 08-mcp-read-prompt | Asking the agent to call `read_shared_note`: it ran with no prompt |

Session 3 (same profile, quit and started again with `notes`; Moderate active; a brand-new conversation):

| File | What it shows |
| --- | --- |
| 09-mcp-post-prompt | The first request of the session: call `post_note` with text 'one': the MCP tool approval prompt |
| 09b-mcp-post-cancelled | Esc at that prompt |
