Screens of the second targeted re-run's candidate (origin/main df3d8c5d1c, packaged macOS arm64 0.1.48), captured by
the integrator on the host Mac on 2026-10-14 (23:18Z-23:30Z) in one disposable profile (no native keyring), GLM 5.3
Flash on zai, 150x45 terminal, before the driver design; paths shortened (~/run = the capture folder). They show the
candidate's menus, prompts and wording; they are design-time screens, not test results. Blank lines are kept as captured.

V00        first start: folder-trust screen
V01        main screen after trusting
V02-V03    /security picker (Permissive active; Aggressive highlighted after Down Down)
V04-V05    Aggressive review (top; bottom after scrolling down)
V06        saved screen ("screen 07") after Enter
V07        after pressing r: the product restarted itself
V08-V09    /panes picker; "+ Corbanu Terminal Pane" highlighted after one Down
V10-V13    new pane: model list (Enter on 1), reasoning level (Enter on 1), name field prefilled "Corbanu 1",
           cleared with 9 x BSpace and typed B
V14        after Enter: pane B created and current
V15        /panes from pane B (highlight starts on the first row, Main); V15b: after one Up from there (it wraps)
V16        after Enter on Main: back in the main pane
V17        an approval prompt in the main pane at Aggressive, 6 s after sending "Run exactly this command: printf ok >
           cap-v.txt" and Enter; V17b the same prompt 3 s later
V18        after Esc on that prompt
V19        /security after the restart
V20        g: grants and kill switch (highlight on "Revoke all active authority")
V21        Down, Enter: kill-switch review
V22        Enter: kill switch on
V23-V25    Esc from V22 (back to the list), Esc again (levels picker), Esc again (closed)
V26        a message sent while the kill switch is on
V27a       /security, g, Down: grants view with the switch on, second row highlighted
V27b       the same view reopened later with /security, g (highlight back on the first row)
V28-V29    from V27b, Down, Enter: kill-switch-off review (default "Back"); Down, Enter: the switch off
V30        closed again
V31        "Spawn a child agent and have it run: echo hi > cap-child.txt", Enter: the product asks about its tool call
V32        after Enter (Allow once): spawned, then a second tool question
V33        after Enter: the child's model's answer and a third tool question
V34        after Enter, and Enter on one more tool question: the parent's summary (in this capture the child's model did
           not run the command; no command approval prompt appeared)
Between V18 and V19 nothing else was done. Between V27a and V27b (not captured): Enter on V27a opened the off review,
Enter there chose "Back" ("Cancelled. Nothing changed."), Esc presses closed it; then /security, g, Enter, Down, Enter
revoked all active authority by mistake (there were no grants), and Esc closed it.
