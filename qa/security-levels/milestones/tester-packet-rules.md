# Tester packets: rules every new packet carries

Every milestone, re-run or parity run builds its own tester packet and freezes it with the run. Packets already frozen
are not changed. This page lists the owner-approved rules that the **next** packet's builder must put into the
tester's prompt, and into any amendment that sets waiting rules, before that packet is frozen. Each rule gives the
exact tester text to use. The generic [executor prompt](../../code-blind-functional/executor-prompt.md) carries the
D114 rule as well.

## Verify setup before the first request (D185, 2026-10-22)

Include a short checklist derived from every case's starting conditions. Before the first product request, the tester must record completion of each required fixture, file, checksum, security-level setting, and exact request text. If any starting condition is missing, stop and correct the disposable fixture before starting; if the run has already started, record the deviation and follow the packet's restart rule. Do not silently repair setup midway or substitute a different request. Verify that the checklist covers all starting conditions before freezing the packet. Existing frozen packets remain unchanged.

## Check about every 30 seconds while a long turn runs (D114, 2026-10-21)

- **Decision (owner-approved, 2026-10-21):** D114, tell testers in the tester packet templates to check about every
  30 s while a long turn runs.
- **Why:** in the second local-model re-run ([record](localmodel-rerun2-20261021/README.md), #1191) testers waited
  up to 300 s at a time without looking at the screen. One left a prompt unanswered for about 5 minutes and pressed the
  long-turn Esc late (R01 at 11 min 6 s). Another approved a prompt after the 10-minute cap and pressed Esc at
  12 min 22 s (R04). No result depended on these delays, but the records had to disclose them.
- **Tester text** (Environment section, next to the description of `agms-tui wait`, or the equivalent harness
  command):

  > While a turn is running, check the screen about every 30 seconds: use `agms-tui wait REGEX 30` (or
  > `agms-tui screen LABEL`) again and again, not one long wait. After each check, answer any prompt that has
  > appeared, as the case says, and note the time against the case's time limits. A long `wait` sees only the text it
  > waits for, so it can miss a prompt or run past a time limit.

- **In a waiting rule** (such as L-3 of [amendment L2](localmodel-rerun2-20261021/executor-packet/v8/amendments-L2.md)):
  keep the rule's own limits (for example, Esc 10 minutes after the request) and add one line: "While waiting,
  check the screen about every 30 seconds (see the Environment section)."
- **Check before the freeze:** the packet's prompts and amendments contain the tester text above, and no instruction
  tells testers to use a single wait longer than 30 s while a turn may stop at a prompt. A long wait is still fine
  when nothing can appear meanwhile, for example while a VM starts.
