Screens of the candidate under test (Corbanu Terminal 0.1.48, origin/main 4c9b0a0cf0), captured by the integrator on the
host Mac on 2026-10-15 between 07:49Z and 07:59Z in disposable profiles and test projects, GLM 5.3 Flash on zai, with
the milestone profile's config. Folder paths are shortened to /Users/tester/run. Never a test-case run, not a test result.

X (two Corbanu Terminals on the same product home and folder; "A" first, "B" second):
- X00: A's first start (folder-trust screen). X01, X02: A, /security, Down Down, Enter: the Aggressive review, top and
  bottom. X03: the saved screen (screen 07). X04: A after r. X05: B started. X06: B's /security.
- X07: A, after "Run exactly this command: echo approved > ks-pending.txt": the approval prompt.
- X08-X11: B: g (grants and kill switch), Down + Enter (review), Enter (kill switch on), Esc.
- X12: A after the kill switch was turned on in B. X13: A after typing y and Enter. X14: B after more Esc presses.

Y (one profile; started with -c features.secretless_agent_launch=false, i.e. protected launch off, then relaunched):
- Y00: start with protected launch off (the warning it shows). Y01: at Permissive, "Run exactly this command: echo
  remembered >> ../outside-notes.txt": the approval prompt. Y02: after option 2 ("don't ask again"). Y03: the same
  request again: no prompt.
- Y04, Y05: relaunched with protected launch on (Permissive): the same request.
- Y06, Y06b, Y07: with protected launch off, /security, Aggressive: the review (preflight blocked), and Enter: not saved.
- Y08: relaunched with protected launch on, Aggressive saved and r pressed: the same request.

Z (a fresh profile; started with -c features.secretless_agent_launch=false -c allow_login_shell=false, then relaunched
without them):
- Z01: at Permissive, the same request: the approval prompt; option 2 ("don't ask again") was then chosen.
- Z02: relaunched with protected launch on (Permissive): the same request.
- Z03: /security, Aggressive, review, Enter: the saved screen. Z04: after r, /security. Z05: the same request: the
  approval prompt. Z06: after Esc.
