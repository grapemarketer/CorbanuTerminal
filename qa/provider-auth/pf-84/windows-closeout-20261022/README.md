# Windows coordinator checks, 2026-10-22

**Result: 24 assertions passed** on the native Windows candidate; every disposable fixture was removed. See [account-checks.txt](account-checks.txt). These are supporting CLI checks, not completion of the full independent gate.

Travis confirmed that the Windows machine was up and requested any needed checks before the program pauses. The required Opus 5.5 High worker was unavailable due to a confirmed provider cooldown. These direct coordinator checks supply supporting evidence; they do not replace the independent PF-84 Windows gate or grant release sign-off.

- Machine: DESKTOP-UJS3HC4, Windows 11 Pro 10.0.26200, reached over SSH.
- Binary: Corbanu 0.1.48; SHA-256 `6919a1b6bc3da7c5b964aa3bff68f186cb0b1e3cf7c0106faf9bf5588ad89fff`.
- Candidate: main commit `36ca28ce2ef00d9aae0e448c8f1ba264b5103608`, after #1357; named accounts enabled explicitly in the disposable config. This is not draft #1080's default-on candidate.
- Build: Rust 1.99.1 MSVC, `cargo build --locked --profile dev-small -j8 -p codex-cli --bin corbanu`, new detached worktree and target under `C:\CorbanuQA\closeout-20261022`. The older checkout and its untracked `before.txt` were left intact.
- Isolation: each child receives a cleared environment, temporary product/user homes, native keyring disabled in a debug-assertions build, generated synthetic credentials, and a loopback-only OpenAI API address. The real default login is never loaded. A synthetic default login is hashed before and after the account operations.
- Passed coverage: add/list/rename/remove, repeat-list write stability, invalid/reserved names, named API-key login, rejection of rename collisions, removed-account startup refusal despite a default credential, unchanged default login, and fixture cleanup.

Full Windows TUI and device-code qualification, held-lock canary coverage, removal during an active request, real provider authentication and owner sign-off remain pending. #1080 stays draft; #1337's account-key input defect also blocks default-on.

Reproduction scripts are in `tools/`. They use no real credentials. Raw build output and a result summary are kept under the machine's closeout folder and copied to `.codex-work/windows-closeout-20261022/` on the coordinator's Mac before pausing.

Setup correction: Windows PowerShell 5 treats ordinary native stderr (such as rustup progress) as an error with `ErrorActionPreference=Stop`. The initial toolchain command stopped for that harness reason. The build script now allows native progress output and checks each native command's exit code explicitly; this is not a product failure.

Fixture corrections before the final passing run: built-in `openai` cannot be replaced in `[model_providers]`, so the loopback transport uses the custom `qa` provider; generic API-key CRUD uses that provider, while OpenAI uses its supported `login --account --with-api-key` flow. The removed OpenAI sign-in correctly reports “not signed in”, rather than generic API accounts' “not configured”; the assertion was corrected to match that distinct refusal. The final script exercises both explicit OpenAI-account and session-provider removal refusals. Earlier fixture/assertion failures are not counted as product defects.

Cleanup retains the verified binary at `C:\CorbanuQA\closeout-20261022\bin\corbanu.exe`, scripts, logs, and the Rust 1.99.1 toolchain. The newly created source worktree and build target are removed after the binary's copied hash is checked. No scheduled tasks, new users, or firewall changes were needed.

Script hardening, 2026-10-26 (review findings on #1365): `build.ps1` now refuses to build if the source worktree has tracked changes or untracked non-ignored files, so `BUILD_OK` can only refer to the pinned commit (ignored files and paths marked assume-unchanged/skip-worktree are outside this check; the cargo build is not expected to read them). `cleanup.ps1` now checks the built binary's hash before copying, copies to a staged name, verifies it, and only then replaces the retained binary. These changes were written while the Windows machine was off and have not been run on Windows; the recorded 2026-10-22 run used the earlier scripts with a newly created worktree.
