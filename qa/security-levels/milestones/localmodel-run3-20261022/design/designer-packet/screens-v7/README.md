# Screens of the candidate with the local model (screens-v7, 2026-10-22)

Text copies of the terminal screen of the exact candidate for this run (Corbanu Terminal 0.1.48, the signed build of
main `11443dc2b5`), captured by the integrator on 2026-10-22 between 05:20Z and 05:40Z in a throwaway copy of the test
machine (a Tart clone, deleted afterwards), with fresh profiles and the `ledger-notes` test project, the product
launched with `agms-tui start --route qwen` (the local model) on the milestone profile. Folder paths are shortened to
`/Users/tester/run` and `~/run`. These are examples of real behaviour and wording, not test cases or results; the
model's own words vary between runs. No credential or protected file was touched.

Files P01-P19 (custom roles, as amendment 7 defines them): the tester's shell ran amendment 7's ADD-PLAIN and ADD-WIDE
(W1) before the first start.

| File | What it shows |
| --- | --- |
| P01-picker-roles-defined | First start (Permissive) with p20plain and p20wide (W1) defined, after the trust prompt: `/security`, Enter |
| P02-aggressive-review-top-w1, P03-aggressive-review-bottom-w1 | Down Down, Enter: the Aggressive review (top: "Child agents now: ... except custom roles that change them: `p20wide`"; bottom after scrolling) |
| P04-aggressive-saved-w1 | Enter: "Saved: Aggressive" (W1 still defined) |
| P05-start-refused-w1 | With Aggressive saved and W1 defined: `agms-tui start --route qwen`. The product does not start: it prints its refusal and exits; the pane then stays on screen with "Pane is dead" (see below). Pressing `r` on P04 gave the same refusal text and exit (captured from the raw terminal output: "Restarting Corbanu Terminal to apply the saved security level…", "Security level Aggressive could not be fully applied:", "  Child agents: role `p20wide` sets sandbox_mode", "Fix the setting, or choose Permissive by editing …/product-home/security_level.toml.") |
| P06-start-refused-w2, P07-start-refused-w3 | The same start with W2 (`approval_policy`), then W3 (`sandbox_workspace_write`) |
| P08-after-remove-security, P09-after-remove-inspector | After amendment 7's REMOVE-WIDE (it printed `0` then `1`): start, `/security` ("Active in this session: Aggressive"), then `i` (the Level rows) |
| P10-agent-types-plain | The question of screens-v6 10, with p20plain defined: the model's list includes `p20plain` (its own words, not product text) |
| P11-spawn-search-p20 | `/spawn`, Enter, typing `p20`: "no matches" |
| P12-req-plain-spawned-wait-prompt | REQ-PLAIN: the "Spawned <id> (<model> medium)" cell (no role shown) and the parent's "Allow `wait_agent_v1`?" question |
| P13-req-plain-child-curl-prompt, P14-req-plain-child-echo-prompt | The sub-agent's command prompts, shown in the main pane, with "Thread: Agent (<id>)" |
| P15-req-plain-close-agent-prompt | The parent's "Allow `close_agent_v1`?" question |
| P16-security-with-subagent, P17-inspector-agents, P18-inspector-recent-denials | After the turn: `/security`, `i`, scrolled to the "Agents" section ("agent <id>: Aggressive") and to "Recent denials" |
| P19-panes-with-subagent | `/panes`: the sub-agent is not listed as a pane |

Files M00-M16 (Moderate, a fresh profile, `--route qwen`): Moderate (restarted), then one Troll pane with `/spawn` and
a harmless task ("Reply with exactly: pong") sent from Spawn status.

| File | What it shows |
| --- | --- |
| M00-picker-moderate | `/security`, Enter, Down: Moderate highlighted |
| M01-moderate-active-after-r | After Enter, Enter, `r`: `/security` shows "Active in this session: Moderate" |
| M02-spawn-menu | `/spawn`, Enter: the menu (Quick start, Roles, Status) |
| M03-troll-harness | Down x3 (Troll), Enter: harness choice |
| M04-troll-model | Enter on "1. Harness: Corbanu Terminal": "Configured custom providers" with the local model "(current)"; a "Z.AI [Other]" tab (Left/Right) |
| M05-troll-spawned | Enter on the local model: the pane is created at once ("Spawned Corbanu Terminal Troll pane Burzum. Qwen3.8-…; no task was started."). With the local model there is no reasoning screen and no task box |
| M06-status-before-send | `/spawn`, Enter; the menu now shows Roles, Status ("Spawn status"), Lifecycle ("Remove managed crew"); "Spawn status", Enter: Spawn Status with "• Burzum [troll] idle" and "Send task to Burzum [troll]" |
| M07-send-task-box, M08-send-task-typed | Enter on "Send task to Burzum [troll]": the "Send Task to Burzum [troll]" box, then with text typed |
| M09-after-send | Enter: back in the main pane; no message shown |
| M10-status-after-send | Spawn Status: "Burzum [troll] running; current task: …" |
| M11-child-view-running, M12-child-view-ended | Enter on "• Burzum [troll]": the child's conversation (header as the main pane's; status line ends with the child's path); then its reply |
| M13-spawn-menu-in-child-view | `/spawn`, Enter in the child's view: the list starts at "Quick start" again; "Spawn status" is below "Orc" (Down x5 from the first row) |
| M14-status-from-child-view | Spawn Status from the child's view ("Burzum [troll] (current) completed; …; latest result: …") |
| M15-main-pane | Enter on "Nazgul: Corbanu Terminal - Main": the main pane ("Main [de…" in the status line) |
| M16-inspector-agents-moderate | `/security`, Enter, `i`, scrolled: the "Agents" section ("main session: Moderate", "agent <id>: Moderate") |

**When the product exits** (P05-P07): the product's last screen stays shown, ending with the line
"Pane is dead (status N, <time>)". Only the last lines of the exit text may be visible (here the first line,
"Security level Aggressive could not be fully applied:", had scrolled off). `agms-tui start` refuses while that screen
is shown ("a product session is already running; use 'agms-tui stop' first"): run `agms-tui stop` first.
