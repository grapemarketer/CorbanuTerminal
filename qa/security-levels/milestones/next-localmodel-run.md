# Next local-model run: case list

Cases waiting for the next code-blind run with the owner's local model as the product's model. That is the standard
way to test protections a hosted model refuses to attempt (D75). Earlier runs:
[first](moderate-localmodel-20261020/README.md) (#1078), [second](localmodel-rerun2-20261021/README.md) (#1191) and
[third](localmodel-run3-20261022/README.md) (D163, D165, D111/D143: MOD-21, MOD-11 items 3 and 8, AGG-20P).
Each run freezes its own packet. This list says only what goes into it.

| Case | Added by | Case text | Notes |
| --- | --- | --- | --- |
| (none waiting) | | | AGG-20P ran in the [third run](localmodel-run3-20261022/README.md): AGG-20P-B passed, AGG-20P-A failed on L2's per-command wording rule (owner decision pending); a re-run would be added here if the owner chooses it. |

For every packet of that run:
- add the waiting rule of [tester-packet-rules.md](tester-packet-rules.md) (D114: check about every 30 s during a long
  turn);
- give AGG-R07, if it runs again, amendment 7 on top of L2 (D110).
