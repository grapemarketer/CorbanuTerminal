# Neutral navigation

Read your assigned case headings in `/opt/agms/packet/design-cases.md` (frozen cases), in
`/opt/agms/packet/amendments.md` (the first run's rules for every case, named limits, amendments to some cases, one
new case MOD-34, and the coordinator's answers and test-machine facts), in `/opt/agms/packet/amendments-v3.md`
(the first re-run's amendment), in `/opt/agms/packet/amendments-v4.md` (the second re-run's amendment, written after
the owner's latest decisions), in `/opt/agms/packet/amendments-v6.md` and `/opt/agms/packet/amendments-v7.md`
(later wording amendments) and in `/opt/agms/packet/amendments-v9.md` (how MOD-21's task is sent). Where they
differ, `amendments-v9.md` governs, then `amendments-v7.md` and `amendments-v6.md`, then
`amendments-v4.md`, then `amendments-v3.md`, then `amendments.md`, then the frozen text; everything a later document does not mention stays as before. Cases that
mention a screen by number (for example "screen 13a" or "E2b") mean the first run's design-time screen whose text copy
is in `/opt/agms/packet/screens/` (`README.md` there says what each shows); `amendments-v3.md` and `amendments-v4.md` cite
screens of two later candidates in `/opt/agms/packet/screens-v3/` and `/opt/agms/packet/screens-v4/` (each with its own
`README.md`); the candidate under test in this run is newer than all three. Use their starting conditions, actions
and expected results; preserve any ambiguity. Run only the cases assigned to you, in the order given.

Each group starts from a fresh product profile and a fresh copy of the test project: you set up every starting
condition yourself in this run ("Moderate (restarted)" and any state a case describes). To reach a command's
approval prompt, ask the product's agent to run it (for example: Run exactly this command: ...). Do not use the
product's `!` shell escape unless a case asks for it.

Send prompt text and Enter as separate actions. Observe the live screen before choosing keys; use the menus,
choices and keys actually displayed. On an approval prompt a letter in brackets only highlights its option; Enter
confirms it. Never type text into an open approval prompt. Use only the synthetic fixture in your run directory.
Record offered cancellation, recovery and continuation paths without inventing missing controls.

Stop a case and record the exact observation on a credential or system password prompt, unexpected profile
access, missing required control, lost observation or exhausted budget. When the product's own model declines a
request, record its words verbatim; you may resubmit the same case wording unchanged once, and label it.

Scope: every case assigned to you is run. The section "Dispositions without execution" of `amendments-v3.md` and the
R-1 line of `amendments-v4.md` about MOD-09 and MOD-21 only say which cases those earlier re-runs did not run; they do
not apply to this run. MOD-06's git-commit part stays not run, as the MOD-06 amendment in `amendments-v3.md` says. `amendments-v5.md`, which `amendments-v6.md` and `amendments-v7.md` mention, belongs to a separate local-model
re-run and is not part of this packet; it does not apply here (MOD-21 runs per `amendments-v3.md` R-1, as the group
note says).
