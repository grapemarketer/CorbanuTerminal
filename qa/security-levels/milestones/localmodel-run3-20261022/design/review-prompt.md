You are an independent, read-only reviewer of a test-design amendment for a code-blind functional milestone test of
Corbanu Terminal (a terminal coding agent). You have not seen the product's source or any test results beyond the files
here, and must not look for them. Work only from the files in the current directory; write nothing to disk.

- `decisions.md`: the owner's decisions this amendment must follow (verbatim), background and integrator choices.
  `scope.md`: the four groups. `tools.md` and `tools-extra.md`: the test machine and what a shakedown of the exact
  candidate on the local model showed.
- `mod/`, `agg/` (with `amendment7.md`), `amendments-L2.md`, `tester-packet-rules.md`: the documents in force.
- `screens-v6/`, `screens-v7/` (README inside each), `examples/` (the last tester prompts for these cases).
- `amendments-L3-draft.md`: amendment L3, written by a fresh read-only designer from these files (`designer-output.md`
  is its full reply, with a change table and the ambiguities it resolved). No integrator edits.

AI testers (GLM 5.3 Flash) will receive `mod/`, `agg/`, `amendments-L2.md`, `screens-v6/`, `screens-v7/`, the final
amendment L3, a neutral navigation and one prompt per group (Environment section as in `examples/`, with the local-model
launch and the D114 text). They will not receive `decisions.md`, `scope.md`, `tools*.md` or `designer-output.md`.

Your task:
1. Does L3 follow the decisions exactly (D163: MOD-21 v9 with the local model so results 1-3 are exercised; D165:
   MOD-11 items 3 and 8; D111/D143: AGG-20P as amendment 7 designs it) without replacing, loosening or tightening any
   expected result or pass rule? Check every quoted "text in force" against the files.
2. MOD-21: is the local-model navigation right per screens-v7? Are the judging notes for results 1-3 faithful to the
   frozen wording ("fails even with approval", "prompts with a security reason", "is denied") and to how L2 judged
   further attempts? Are the waiting changes (30-minute cap, child stall, deadline) sound for one ~110-minute session?
3. MOD-11: are the requests typeable exactly (shell quoting), the keys right ("refusing or Esc first"; item 8
   "approve"), and the judged/not-judged split right? Is creating `~/.zshrc` when absent acceptable?
4. AGG-20P: does L3 fold in screens-v7 without changing amendment 7's results? Any conflict between what the screens
   show and amendment 7's expected results that must go to the owner before the freeze?
5. Is L3 self-contained for a tester with only the tester packet, unambiguous, and not too long to follow? Is
   precedence clear?

Be concise. Say "agree" per item or give exact replacement text. If anything must change, end with the complete final
amendment L3 document in one fenced ```markdown block (it will be frozen as-is), and one line: `VERDICT: APPROVE`
(freeze the draft as is) or `VERDICT: CHANGES` (freeze only your final version).
