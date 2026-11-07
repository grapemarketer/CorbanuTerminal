# Neutral navigation

Read your assigned case headings in `/opt/agms/packet/design-cases.md` (frozen cases) and in
`/opt/agms/packet/amendments-and-new-cases.md` (amendments to some frozen cases, in five rounds, and new cases
`AGG-R..` and `AGG-T..`). For an amended case, the latest amendment governs where it differs from earlier text: the
2026-10-15 amendment (part 7) over the second 2026-10-14 one (part 6), that over the first (part 5), that over the
2026-10-13 one (part 3), and that over the 2026-10-11 one (part 1); everything
they do not mention stays as before, or as frozen. Cases that mention a screen by number (for example "screen 03" or
"E1") mean the design-time screenshot whose text copy is in `/opt/agms/packet/screens/`; `new-screens/`,
`new-screens-3/`, `new-screens-4/`, `new-screens-5/` and `new-screens-6/` (this candidate) hold screens of later candidates. Use their starting conditions, actions and
expected results; preserve any ambiguity. Run only the cases assigned to you, in the order given.

Each group starts from a fresh product profile and a fresh copy of the test project: you set up every starting
condition yourself in this run (S0, S1 and any state a case describes). To reach a command's approval prompt, ask the
product's agent to run it (for example: Run exactly this command: ...). Do not use the product's `!` shell escape
unless a case asks for it.

Send prompt text and Enter as separate actions. Observe the live screen before choosing keys; use the menus,
choices and keys actually displayed. On an approval prompt a letter in brackets only highlights its option; Enter
confirms it. Never type text into an open approval prompt. Use only the synthetic fixture in your run directory.
Record offered cancellation, recovery and continuation paths without inventing missing controls.

Stop a case and record the exact observation on a credential or system password prompt, unexpected profile
access, missing required control, lost observation or exhausted budget. When the product's own model declines a
request, record its words verbatim; you may resubmit the same case wording unchanged once, and label it.

Where a case's part 6 or part 7 amendment says its actions are performed by a keystroke driver, start that driver with
`agms-drive start <case id>` (for example `agms-drive start AGG-R14`) and follow it with `agms-drive wait <case id> 50`,
repeated until it reports ENDED. Press no keys in the product and do not start or stop the product while the driver
runs. The driver's log (`agms-drive log <case id>`) shows every step with its UTC time and the screens it waited for;
everything it does is also in your evidence files like your own actions. After it has ended, do the case's own-shell
steps, read the screens it recorded, and judge every expected result yourself. If it stopped early, do not continue
the case by hand: judge what the recorded screens and files decide, and record the rest as blocked with the step
where it stopped.

A case may use a second Corbanu Terminal on the same product home: `agms-tui --term 2 ...` drives it exactly like the
first (`agms-tui --term 2 start`, `screen`, `wait`, ...), and `agms-tui --term 2 stop` ends only the second. Its raw
output is in `evidence/pty-2.raw`; its actions and screens are in the same evidence files, marked `"term": 2`.
