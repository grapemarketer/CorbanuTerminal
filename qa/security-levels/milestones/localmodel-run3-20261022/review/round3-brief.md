You are an independent, read-only evidence checker for a code-blind QA record of Corbanu Terminal. Do not read product
source code. Work only in `qa/security-levels/milestones/localmodel-run3-20261022/` (and the earlier records it links
to, for the rules in force). Write nothing.

The record: README.md, dispositions.md, results.json; the frozen amendment `executor-packet/L3/amendments-L3.md`
(governs), the packet it sits on (`executor-packet/L3/mod/`, `agg/` with `amendment7.md`, `amendments-L2.md`); per group
`runs/<G>/` (tester report `collected/evidence/result.md` or `collected/work-result.md`, screens
`collected/evidence/screens.jsonl`, `product-excerpt.txt`, `evidence-check.txt`, `task-delivery.txt`, `session-files.txt`,
mediator logs) and `common/<G>/`; `d162/`; `design/` (freeze, designer and reviewer outputs).

Check:
1. Each disposition against the raw evidence and the rules in force (L3, v9, the frozen MOD cases with G-1,
   amendment 7 and L2's AGG-20 notes): MOD-21 passed; MOD-11 L02 blocked and L02R passed (are the replay tester's
   deviations — no `~/.zshrc` created, varied wording, item 8 as `printf '{}' >` — compatible with "passed"?);
   AGG-20P-B passed (result 4 way a in C, B1, B2, B3; results 3, 5, 6, 7); AGG-20P-A failed on result 2's per-command
   wording rule (is "failed" the correct literal reading of L2's per-command note when the child's command differs from
   the parent's, or should it be another outcome?). Quote the evidence you rely on.
2. Every factual claim in README.md and dispositions.md (times, counts, quotes, hashes, issue numbers, the freeze
   order, D162 steps) against the files. List each wrong or unsupported claim with the exact correction.
3. Whether any secret or real credential appears in the record.

Be concise. End with exactly one line: `VERDICT: APPROVE` (record correct as is) or `VERDICT: CHANGES` (list the required
changes above it).
