# Parity runs between test virtual machines: method

A parity run checks whether a new test virtual machine (VM) gives the same case results as the current one, before
milestone runs move to it. The first one, the Tart parity run of the Moderate milestone, is recorded in
[moderate-tart-parity-20261020/](moderate-tart-parity-20261020/README.md); its predeclared comparison method,
[design/parity-method.md](moderate-tart-parity-20261020/design/parity-method.md), is frozen with that run and is not
changed here.

## Rule added by owner decision D97 (2026-10-20)

- **Same build on both machines.** A parity run uses the same candidate build on both VMs: the same signed package
  (the same `bin/corbanu` SHA-256 and the same commit in `INSTALL.json`), with the same case texts, amendments and
  executor packet. A baseline taken from an earlier record counts only if it ran that same build; otherwise the
  baseline cases are run again on the current VM with the build under test.
- Why: in the Tart parity run every UTM result being compared came from an older build than the Tart run's, so a
  difference could come from the machine or from the product, and a product change (#1139) looked like a machine
  difference until a tie-break re-ran the old build on Tart.

Everything else in the frozen method above (the unit of comparison, the classes, the tie-break and the predeclared
rules) is the template for the next parity run, written and frozen before its first group.

## Owner decision D161 (2026-10-22)

- Milestone runs use Tart (one fresh clone of `corbanu-qa-base` per group), as the D139 three-attempt runs
  ([moderate-vm-parity3-20261021/](moderate-vm-parity3-20261021/README.md)) recommended. UTM "macOS 3" is no longer
  the milestone VM. D162: its real Z.AI key file and `auth.json` were deleted on 2026-10-22
  ([localmodel-run3-20261022/](localmodel-run3-20261022/README.md#d162-the-real-zai-key-in-utm-macos-3)); the owner rotates the key.
