#!/usr/bin/env bash
# Wall-clock timings of the command-safety classifier for the input shapes
# in codex-rs/core/src/security/tainted_action_timing_probe.rs, at
# 1k/2k/4k/8k units each, on a release build.
#
# Usage: scripts/classifier_timing_probe.sh [OUT_DIR]
# Env:   PROBE_SIZES (default "1000 2000 4000 8000"), PROBE_REPS (5),
#        PROBE_TIMEOUT seconds per pattern and size (600),
#        PROBE_PATTERNS (default: all five).
#
# Writes OUT_DIR/raw.tsv (every timed run), OUT_DIR/table.md (median per
# size, doubling ratios, verdict) and one log per run. A run that times out
# or crashes is listed in the table as TIMEOUT / EXIT <code>.
set -uo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-$repo/classifier-timing}
sizes=${PROBE_SIZES:-1000 2000 4000 8000}
reps=${PROBE_REPS:-5}
limit=${PROBE_TIMEOUT:-600}
patterns=${PROBE_PATTERNS:-nested_substitution alias_chain arithmetic glob_alternatives nested_quotes}
filter=security::tainted_action::tainted_action_timing_probe::classifier_timing_probe

mkdir -p "$out"
cd "$repo/codex-rs" || exit 1

echo "== build: cargo test --release -p codex-core --lib --no-run"
cargo test --release -p codex-core --lib --no-run 2>&1 | tee "$out/build.log"
build_status=${PIPESTATUS[0]}
if [ "$build_status" -ne 0 ]; then
  echo "build failed (exit $build_status)" >&2
  exit "$build_status"
fi

printf 'pattern\tunits\tbytes\trun\tns\tverdict\n' >"$out/raw.tsv"
: >"$out/status.tsv"
for pattern in $patterns; do
  for n in $sizes; do
    log="$out/run-$pattern-$n.log"
    start=$(date +%s.%N)
    PROBE_PATTERN=$pattern PROBE_UNITS=$n PROBE_REPS=$reps \
      timeout "$limit" cargo test --release -q -p codex-core --lib -- \
      --ignored --exact --nocapture "$filter" >"$log" 2>&1
    status=$?
    end=$(date +%s.%N)
    grep '^probe' "$log" | cut -f2- >>"$out/raw.tsv"
    case $status in
      0) state=ok ;;
      124) state=TIMEOUT ;;
      *) state="EXIT $status" ;;
    esac
    printf '%s\t%s\t%s\t%s\n' "$pattern" "$n" "$state" \
      "$(awk -v a="$start" -v b="$end" 'BEGIN { printf "%.2f", b - a }')" >>"$out/status.tsv"
    echo "$pattern $n: $state"
  done
done

python3 - "$out" "$reps" <<'PY'
import collections
import statistics
import sys

out, reps = sys.argv[1], int(sys.argv[2])
runs = collections.defaultdict(list)
verdicts = {}
with open(f"{out}/raw.tsv") as f:
    next(f)
    for row in f:
        pattern, units, _bytes, run, ns, verdict = row.rstrip("\n").split("\t")
        if run != "warmup":
            runs[(pattern, int(units))].append(int(ns))
        verdicts[(pattern, int(units))] = verdict
status = {}
order = []
with open(f"{out}/status.tsv") as f:
    for row in f:
        pattern, units, state, _secs = row.rstrip("\n").split("\t")
        status[(pattern, int(units))] = state
        if pattern not in order:
            order.append(pattern)
sizes = sorted({units for _, units in status})

lines = [
    "| pattern | " + " | ".join(f"{s} median ms" for s in sizes)
    + " | doubling ratios | verdict |",
    "|---" * (len(sizes) + 3) + "|",
]
for pattern in order:
    cells, medians = [], []
    for s in sizes:
        state = status.get((pattern, s), "untested")
        times = runs.get((pattern, s), [])
        if state == "ok" and len(times) == reps:
            median = statistics.median(times) / 1e6
            medians.append(median)
            cells.append(f"{median:.3f}")
        else:
            medians.append(None)
            cells.append(state)
    ratios = [
        b / a
        for a, b in zip(medians, medians[1:])
        if a is not None and b is not None and a > 0
    ]
    if None in medians or not ratios:
        verdict = "incomplete (see cells)"
    elif max(ratios) >= 3.0:
        verdict = "super-linear"
    elif max(ratios) <= 2.5:
        verdict = "linear"
    else:
        verdict = "unclear (ratio between 2.5 and 3.0)"
    lines.append(
        f"| {pattern} | " + " | ".join(cells)
        + " | " + ", ".join(f"{r:.2f}" for r in ratios) + f" | {verdict} |"
    )
lines.append("")
lines.append("Verdicts on each line (classifier result):")
for pattern in order:
    seen = sorted({verdicts[(pattern, s)] for s in sizes if (pattern, s) in verdicts})
    lines.append(f"- {pattern}: {', '.join(seen) or 'none'}")
with open(f"{out}/table.md", "w") as f:
    f.write("\n".join(lines) + "\n")
print("\n".join(lines))
PY
