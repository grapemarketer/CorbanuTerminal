You are a fresh, independent test designer for a code-blind functional milestone test of Corbanu Terminal (a terminal
coding agent). You have not seen the product's source code and must not look for it. Work only from the files in the
current directory. You are read-only: write nothing to disk; put everything in your reply.

Files:
- `decisions.md`: the owner's decisions for this run (verbatim), background and the integrator's choices.
- `scope.md`: the four groups and what this amendment must do for each.
- `tools.md`: the test machine (with this run's two harness changes) and what a shakedown of the exact candidate on
  the local model showed.
- `mod/`: the Moderate packet in force (frozen `design-cases.md`; `amendments.md` = amendments-v2; `amendments-v3.md`,
  `-v4.md`, `-v6.md`, `-v7.md`, `-v9.md`; `navigation.md`; `screens/`).
- `agg/`: the Aggressive packet (`design-cases.md`, `amendments-and-new-cases.md`, `navigation.md`) and
  `amendment7.md` (owner-approved; defines AGG-20P).
- `amendments-L2.md`: the second local-model run's amendment (rules L-1..L-5 and its cases); a model for this one.
- `tester-packet-rules.md`: the owner-approved waiting rule (D114) every new packet must carry.
- `screens-v6/` (README inside): the local model on an earlier candidate. `screens-v7/` (README inside): this candidate
  on the local model, captured for this run (MOD-21's Spawn-status flow, AGG-20P's custom roles).
- `examples/`: the tester prompts of the last MOD-21, MOD-11 and AGG-20 groups (what a tester receives besides the
  packet).

Task: write **amendment L3** (third local-model run), applied on top of the documents in force, for exactly these
groups: L01 MOD-21; L02 MOD-11 items 3 and 8 only; L03 AGG-20P-A; L04 AGG-20P-B.

1. **Rules for this run** (in the style of L2's "Rules for this re-run"): the local-model launch and what to record;
   which of L2's L-1..L-5 apply to which case as written and where L3 differs (MOD-21 has a child pane on the local
   model and v9's own keys and waiting rules; MOD-11 uses the frozen "refusing or Esc first"; AGG-20P uses amendment
   7's keys); declines (G-1 for the MOD cases, amendment 7's rule for AGG-20P), stalls, long turns and the group time
   limit with the local model's real latency (`tools.md`); recording and quoting typed requests; the D114 text. Keep
   every rule a tester needs inside L3 or by an exact reference.
2. **MOD-21 (D163):** amendment v9 with the local model as the product's model for the main pane and for the Troll
   child, replacing amendments-v3 R-1 (GLM 5.2) for this run; fix v9's navigation where screens-v7 shows the local
   model differs (model list, no reasoning screen). The three commands are expected to be attempted now: say how
   further attempts after a denial are judged (as L2's L-4 does), keeping v9's keys (option 1 on every prompt).
   Expected results unchanged.
3. **MOD-11 items 3 and 8 (D165):** exact requests, keys and which expected results are judged (and which are not run
   or judged), as `scope.md` says. Item 8's path must be written out so the tester can type it. Expected results
   unchanged.
4. **AGG-20P-A and AGG-20P-B (D111/D143):** as amendment 7, under this run's rules. Fold in what screens-v7 shows
   (navigation, the exit on a refused start, the Spawned cell without a role, no `/spawn` row for custom roles) where
   amendment 7's text would mislead a tester. Do not change amendment 7's expected results or pass rules. If anything
   the screens show conflicts with an expected result of amendment 7, say so explicitly in part (c) (it goes to the
   owner before the freeze).
5. Give each group's verdict line format and a one-line note of how each outcome is recorded (follow-ups to the
   Moderate and Aggressive records). Keep every other case, rule and named limit unchanged; say so in one line.

Reply with:
(a) the complete amendment L3 document in one fenced ```markdown block: a short header (what it applies on top of,
per-case table of documents in force, precedence, decision labels), "## Rules for this run", "## Case amendments"
with "### MOD-21 (amended; …)", "### MOD-11, items 3 and 8 only (amended; …)", "### AGG-20P-A and AGG-20P-B (…)",
then "## How each outcome is recorded" with each group's passed/failed/blocked rule (referring to the existing
expected results and pass rules, not replacing them);
(b) a table: case, text in force that L3 changes (quoted), new text, reason;
(c) any ambiguity, conflict with an expected result, or gap, and how you resolved it, or why you could not.
End with one line: `DESIGN: COMPLETE` or `DESIGN: BLOCKED <reason>`.
