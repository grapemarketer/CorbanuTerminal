# Screens of the candidate with the local model (2026-10-21)

Text copies of the terminal screen of the exact candidate for this re-run (Corbanu Terminal 0.1.48, the signed build
of main `bc1deda532`), captured by the integrator on 2026-10-21 between 01:27:02Z and 01:36:41Z in a throwaway copy of
the test machine (a Tart clone, deleted afterwards) with a fresh profile and the `ledger-notes` test project, the
product launched with `agms-tui start --route qwen` (the local model). Files 00-19: the milestone profile (security
features on, protected launch included), taken to Aggressive. Files A00-A13: the accounting record's neutral profile.
Folder paths are shortened to `/Users/tester/run`. These are examples of real behaviour and wording, not test cases or
results; the model's own words vary between runs. No credential or protected file was touched in this session.

| File | What it shows |
| --- | --- |
| 00-trust | First start in the project: trust prompt |
| 01-start-qwen | After Enter: the header names the local model; status line "... via qwen default" |
| 02-picker | `/security`, Enter, Down Down: Aggressive highlighted |
| 03-aggressive-review-top, 04-aggressive-review-bottom | Enter: the Aggressive review (top; bottom after scrolling) |
| 05-aggressive-saved | Enter: Saved: Aggressive |
| 06-after-restart | `r`: the restarted session (same model) |
| 07-command-prompt | "Run exactly this command and show its output: printf shakedown-ok > shakedown.txt": the metadata warning and the approval prompt every command gets under Aggressive |
| 08-command-done | Enter on "1. Yes, proceed": the command ran (inside the project) and the reply |
| 09-spawn-menu | `/spawn`: the managed-crew picker |
| 10-agent-tools | "Without calling any tool, list the names of your child-agent tools and ... every parameter ...": the model's list (its own words, not product text) |
| 11-spawn-troll-harness | `/spawn`, Down x3 (Troll), Enter: harness choice |
| 12-spawn-troll-model | `1`: model choice; the local model offered as current |
| 13-troll-spawned | Enter: "Spawned Corbanu Terminal Troll pane Burzum ... no task was started" |
| 14-panes, 15-panes-search | `/panes`: the child is not in the list until its name is typed |
| 16-child-pane | Enter on the child's row: the child pane |
| 17-child-pong | In the child: "Reply with exactly: pong" |
| 18-child-security, 19-child-inspector | In the child: `/security` (Aggressive active), then `i` |
| A00-start-neutral | Neutral profile, `--route qwen`, after the trust prompt |
| A01-cost-empty | `/cost` before any turn |
| A02-ok-turn, A03-cost-after-ok | "Reply with the single word OK.", then `/cost` |
| A04-side-open, A05-side-reply | `/side` (status line "Side from main thread · ctrl + / to switch · ctrl + c to close"); "Reply with the single word SIDE." |
| A06-side-closed | Ctrl+C: the side conversation closes and the saved conversation is shown again; the status line no longer says "Side from main thread" (no separate notice in this capture) |
| A07-cost-after-side | `/cost`: 2 requests |
| A08-cost-details, A09-cost-open-links | `/cost` scrolled with Down: details, then the `→ Request N` links under "—— Open ——" |
| A10-request-page, A11-request-page-links | Enter on a request link: its page; scrolled to "→ Technical details (attempt 1)" |
| A12-technical-top, A13-technical-ids | Enter: the technical page; scrolled to the Request, Attempt, Thread and `Turn:` lines (this request is the main conversation's, so `Turn:` has no prefix) |
