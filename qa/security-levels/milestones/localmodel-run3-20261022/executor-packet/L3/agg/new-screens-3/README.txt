Screens of the candidate (Corbanu Terminal 0.1.48, origin/main 4b6dd55c85, bin/corbanu SHA-256 ad288be98d80d5eb...0675043),
captured by the coordinator on 2026-10-13 between 14:38Z and 14:53Z with a disposable profile and test project, GLM 5.3 Flash on
zai. Folder paths are shortened to /Users/tester/run. Never a test-case run.

- T01: /model at Permissive. T14: /model at Aggressive.
- T02, T03: choosing Aggressive (review) and the screen after confirming it.
- Before starting, the project had been set up from the person's own shell with `git config core.hooksPath .husky`
  and a `.husky/pre-commit` file. T04-T07 (Aggressive after restart): appending to .husky/pre-commit; approval prompt,
  Grant once review, after "Grant and run", and the same command again approved with y.
- T08, T09: appending to ~/.zshrc (outside the project) at Aggressive: prompt and result after y.
- T10: `ln -s .agents ag` at Aggressive (a .agents folder had been created from the person's own shell).
- T11: appending to .agents/notes.md at Aggressive: the approval prompt.
- T12: at Permissive, a command writing outside the project; option 2 ("don't ask again") was then chosen.
- T13: after switching to Aggressive and restarting, the same command again.
