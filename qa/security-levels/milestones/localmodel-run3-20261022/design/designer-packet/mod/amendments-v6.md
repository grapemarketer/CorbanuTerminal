# Amendments to the Moderate design (amendments-v6, MOD-22 inspector step)

Applied on top of `design-cases.md` + `amendments-v2.md` (cited in later amendments as `amendments.md`) +
`amendments-v3.md` + `amendments-v4.md` + `amendments-v5.md`, for future runs of MOD-22 only. Precedence: this
amendment over amendments-v5, over amendments-v4, over amendments-v3, over amendments-v2, over the frozen cases. For
MOD-22 this amendment governs where it differs. Everything it does not mention stays as in force: the frozen text,
amendments-v2 (the 'baseline' screen in the starting conditions, action 3 and the 'restart now' named limit) and
amendments-v3 (action 5 and the expected results after the relaunch). Amendments v4 and v5 do not touch MOD-22.
No expected result of MOD-22 is replaced or added. Amendment v7 (D97: MOD-12, MOD-23, MOD-29) is separate. The two
do not overlap and neither depends on the other.

Decision label (listed in the integrator's record, not part of the tester's packet):
- **D96** (owner, 2026-10-20): "MOD-22 case text: add `/security`, `i` (inspector) after the relaunch step."

## Rules

- G-1, G-2, G-3, the named limits, R-1, R-2, R-3 and the rules of amendments-v4 and amendments-v5 are unchanged.
  Each still applies only to the cases it names. This amendment changes no rule.

## Case amendments

### MOD-22 (amended; D96)

- Action 4 (replace) "`/security`." with: `/security`. Record the picker screen (screen label
  'after-relaunch-picker'). Then press `i` to open the inspector. As for the baseline, read the approval, sandbox and
  network rows, scrolling the inspector if needed. Record the screen or screens that show them (screen label
  'after-relaunch'; if more than one screen is needed, 'after-relaunch-2', 'after-relaunch-3', ...). Copy the
  approval, sandbox and network rows of both 'baseline' and 'after-relaunch' word for word into the record, side by
  side. Change nothing: in the picker press no ↑/↓, and press no Enter in the picker or the inspector. Then press Esc
  to close the inspector and Esc to close the picker; if the conversation is already shown after the first Esc, do
  not press Esc again. Check that the conversation is shown before action 5.
- Note (D96): This step comes right after the relaunch (action 3) and before action 5's request. It is not a
  request, so amendments-v3's "After the relaunch, send only this request" still holds. Screen
  'after-relaunch-picker' is the "/security picker in action 4" that amendments-v3's added expected result (B)
  refers to.
- Note (D96): The expected result "The person's own approval and sandbox settings match the baseline" (amendments-v3
  wording, unchanged) is judged by comparing screen 'after-relaunch' (with any 'after-relaunch-2', ...) with screen
  'baseline' (amendments-v2), row by row:
  - the approval row with the approval row;
  - the sandbox row with the sandbox row.
  A row matches when it shows the same setting values in both screens. Differences only in layout, line wrapping or
  scroll position do not count. Record any other difference in a row's text word for word.
- Note (D96): The network row is recorded in both screens and any difference is recorded. It is not part of this
  expected result and is not judged by it, unless the inspector shows network access inside the sandbox row; then it
  is compared as part of that row. Other inspector rows (saved and active level, untrusted content, MCP servers,
  recent denials) are not part of this comparison.
- Note (D96): If either screen does not show the approval or the sandbox row, record which row is missing. That row
  cannot be compared; nothing else is added.
