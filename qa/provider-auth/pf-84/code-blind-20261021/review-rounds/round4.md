**Round 4 review: approved.** All six round-3 findings are fixed correctly, and I found no new material inaccuracy.

**Round-3 findings, checked against the files**
1. **Docs ambiguity (resolved).** The headline, the NA-05 row and decision 2 now say "ambiguous". The NA-05 row quotes line 91 of `docs/provider-accounts.md` word for word. That sentence is in the "Fail closed" section, as the separate paragraph after the `corbanu fork` paragraph, so "and then" is a fair paraphrase.
   - #1287 (`gh issue view 1287`: open, no comments) says the docs are ambiguous, quotes both passages and line 91, and keeps the "launch `--account`" gap.
   - Its option (a) now says to qualify the "Inside a session, `/resume` …" bullet.
   - It also says the probe homes differ from NA-05 in the `[provider_accounts]` entry, and it carries the package caveat.
2. **Arm-B package caveat (resolved).** I compared `armB/package-files-probe-armB.txt` with `harness/package-files.txt`.
   - `corbanu-acp`, `corbanu-walletd` and `codex-path/rg` are absent from arm B.
   - `codex-code-mode-host`, `codex-package.json` and `setup-telegram.sh` have different digests.
   - `corbanu` (`bbae7d37…`), the other telegram resources and `zsh` are identical.
   - The probe README lists exactly this and now says "likely cause".
   - The host and guest scripts differ only in the arm-B names, the guest-script filename and `--account personal`.
3. **`candidate-identity.md` (resolved).** It now has lines for the arm-B clone `032245Z` (v4 digests), the no-relay discard `031535Z`, and the staging failure (marked "no digests"). The safety paths now include `armB/clone-*` and `armB/discarded/*`.
   - The arm-B clone's guard file shows 0 `SECURITYAGENT-SEEN` lines from 272 checks; the no-relay discard's shows 0 from 375.
   - All three erase files show 0 home entries, 0 synthetic credential files, and the leaf key and placeholder key file absent.
4. **Empty guard file (resolved).** Both READMEs disclose it.
5. **Transcript provenance (resolved).** The probe README now covers arm B. `armB/probe-transcript.txt` starts with the host `gate … PASS` line, and line 22 of the arm-B script has the same bare redirect.
6. **Approval label (resolved).** It now reads "Sign-in approval (per the coordinator)".

**Remaining findings (both non-blocking)**
1. **Low – README NA-05 row.** The row still ends "— so the launch `--account` is the cause". That is stronger than the probe README ("the likely cause", plus the package caveat). The row also has no caveat.
   - It should say: "— so the launch `--account` is the likely cause (arm B's companion package differed; `corbanu` identical; see the probe README)".
   - The evidence is otherwise consistent: the relay shows `zpersonal` at 03:27:15, the conversation records `['work','personal']`, and the `corbanu` binary is identical. So this is a wording-precision fix, not a wrong conclusion.
2. **Nit – #1287 caveat.** It says the arm-B package had "different companion binaries". Some were in fact absent, and two of the differing files are not binaries. It links the probe README, so this is acceptable. "Different or missing companion files" would be exact.

**Other checks**
- Totals are unchanged and correct: 28 cases (23 passed, 2 in part, 1 not complete, 2 failed), with tester verdicts of 22 passed and 6 failed.
- "Three attempts plus two discarded clones" matches the clone files.
- `REVIEW.md` lists rounds 1–3 accurately; a line for this round still needs adding.
- No secrets are in the arm-B files.
- Readiness is stated correctly: D136 blocks, and NA-05 needs an owner decision.

VERDICT: APPROVE
