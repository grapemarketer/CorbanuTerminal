# What the first run found that made some frozen steps unreachable (coordinator, 2026-10-22)

The frozen design (`design-cases.md`) was run once, one fresh machine per group. Some steps could not be carried out
as written, or tested a different situation than the case intended, for the reasons below. These are observations of
the test machine and of what the screen showed; they are given only so the affected cases can be re-run in a form that
reaches the intended situation.

1. **Keys cannot be typed into `corbanu account add`.** Started in a terminal with no piped input, `account add`
   exits at once with code 1 and prints: "Error: pipe the secret in instead of typing it, for example
   `cat key.txt | corbanu account add <provider> <name>`". CLIK therefore never adds an account. Affected: NA-01
   (steps 2, 4, 6, 7), NA-04 (step 1 and everything after), ST-02 (every add). New test-machine option:
   `agms-tui start --home H --stdin TEXT ...` pipes TEXT (letters, digits, `.`, `_`, `-`) into the program's standard
   input, exactly like `printf TEXT | corbanu ...`, then behaves as `start` (the terminal stays open after exit).
2. **A conversation's recorded account changes with each turn.** The conversation (session) files record the provider
   and account for every turn. In PR-01, `<id>` was created on `work`; step 1 (`resume --account personal <id>`, then a
   turn) recorded that turn on `personal`; the following `resume <id>` with no flag then ran on `personal`. GH-02 and
   NA-05 assumed `<id>` was still recorded on `work`, so their "recorded account is missing" situation was never set
   up (both resumed on `personal` without any message).
3. **`/providers` showed exactly 8 rows.** In `zai3` (accounts `work` and `personal`) the list showed the six other
   providers, `Z.AI · default` and `Z.AI · personal`, and the footer; `work` never appeared, and no scroll hint was
   shown. Testers moved down only as far as row 8, so whether further rows exist below is unknown. NA-06, NA-07 and
   NA-10 stopped at "no `work` row".
4. **The "Add a … account" name field.** Text typed and Enter pressed within the same second left the name in the field
   and added an empty line; a second Enter then advanced to the next step (seen in the OpenAI add). In the Z.AI add,
   the tester then typed the key, which appeared in clear in the name field, and the next Enter gave "invalid account
   name: use 1-32 lowercase letters, digits or '-' …"; nothing was saved.
5. **Fresh-home onboarding is taller than the terminal.** The provider list (logo plus about 14 providers) does not fit
   in 45 rows; Up/Down scrolls it. After the Z.AI key was saved ("✓ Z.AI API key configured"), Enter on the highlighted
   row only cycled between the provider list and the Z.AI actions ("Use configured credentials", "Add another
   account", "Replace with a new API key"); the end of the list was never reached. The list's own text says the
   Corbanu API setup "runs after Done", so a final row such as "Done" may exist below.
6. **G6 stopped early.** Its tester stopped (model output limit) after NA-11 and ESC-01 and wrote no report; G6 is
   replayed unchanged, which needs no amendment.
7. **REG-01d lost its window.** The first `C-c` ended the test machine's terminal wrapper with the program, so the exit
   line and the echo check were never shown. The test machine now keeps the terminal open after a `C-c` (it shows the
   exit line, and a later `C-c` no longer closes the echo stage). REG-01d is replayed unchanged, like G6; it is not
   part of your groups.
