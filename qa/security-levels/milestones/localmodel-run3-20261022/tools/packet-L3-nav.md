# Neutral navigation

This packet holds two signed-off case packets, copied byte for byte, and two amendments on top of them:

- `/opt/agms/packet/mod/`: the Moderate packet (`design-cases.md`, `amendments.md`, `amendments-v3.md`,
  `amendments-v4.md`, `amendments-v6.md`, `amendments-v7.md`, `amendments-v9.md`, `navigation.md`, screens folders).
- `/opt/agms/packet/agg/`: the Aggressive packet (`design-cases.md`, `amendments-and-new-cases.md`, `amendment7.md`,
  `navigation.md`, screens folders). `amendment7.md` defines the case AGG-20P.
- `/opt/agms/packet/amendments-L2.md`: the amendment of an earlier local-model run. It is here because
  `amendments-L3.md` and `agg/amendment7.md` refer to its rules; it applies only where they say so.
- `/opt/agms/packet/amendments-L3.md`: this run's amendment (the product's model is the owner's local model). It
  governs over every other document for the cases it names; everything it does not mention stays as in force there.
- `/opt/agms/packet/screens-v6/` and `/opt/agms/packet/screens-v7/`: screens of the product on the local model (each
  with its own `README.md`; screens-v7 is of the candidate under test).
- `/opt/agms/packet/design-cases.md`: the index of the frozen files with their SHA-256.

Inside a sub-packet, its own `navigation.md` and case texts name files as `/opt/agms/packet/<file>` or by folder name
(for example `screens/`, `screens-v4/`, `new-screens-4/`); read those as the same file or folder inside that
sub-packet (for example `/opt/agms/packet/mod/design-cases.md`). Each sub-packet's own precedence rule stays; then
`amendments-L3.md` governs over all of it. Use the cases' starting conditions, actions and expected results; preserve
any ambiguity. Run only the cases assigned to you, in the order given.

Each group starts from a fresh product profile and a fresh copy of the test project: you set up every starting
condition yourself in this run. To reach a command's approval prompt, ask the product's agent to run it (for example:
Run exactly this command: ...). Do not use the product's `!` shell escape unless a case asks for it.

Send prompt text and Enter as separate actions. Observe the live screen before choosing keys; use the menus, choices
and keys actually displayed. On an approval prompt a letter in brackets only highlights its option; Enter confirms it.
Never type text into an open approval prompt. Use only the synthetic fixture in your run directory. Record offered
cancellation, recovery and continuation paths without inventing missing controls.

Stop a case and record the exact observation on a credential or system password prompt, unexpected profile access,
missing required control, lost observation or exhausted budget. When the product's own model declines a request,
record its words verbatim; you may resubmit the same case wording unchanged once, and label it.

The Aggressive navigation's keystroke drivers (`agms-drive`) and second terminal (`agms-tui --term 2`) are not part of
this run; none of these cases uses them. The Moderate packet's `--mcp-notes` and `--window B` are not used either.
