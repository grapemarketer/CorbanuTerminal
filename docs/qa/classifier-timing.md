# Classifier timing: input shapes #1804 does not cover

Wall-clock timings of the command-safety classifier
(`codex-rs/core/src/security/tainted_action`) for five input shapes that
#1804's linearity tests do not cover, at 1k, 2k, 4k and 8k units each, on a
release build. Nothing in the classifier changes; the probe is an ignored
test (`tainted_action_timing_probe_tests.rs`, never runs in CI) and a runner
script (`scripts/classifier_timing_probe.sh`).

## Patterns

Each line is classified as `bash -lc <line>` in `/work` through
`classify_with`, the same entry point the classifier tests use. A unit is:

| pattern | unit | line at 2 units |
|---|---|---|
| `nested_substitution` | one nesting level | `cat $(cat $(cat f))` |
| `alias_chain` | one alias definition | `alias a0=a1; alias a1=./x; a0` |
| `arithmetic` | one term | `echo $((v0+v1+v2))` (3 units) |
| `glob_alternatives` | one extglob alternative | `shopt -s extglob`, newline, `cat ./@(f0\|f1)` |
| `nested_quotes` | one quote level | `echo "${a:-"${a:-x}"}"` |

`lines_have_the_units_asked_for` (a normal, non-ignored test) pins these
generators.

- `glob_alternatives` puts `shopt -s extglob` and the pattern on separate
  lines. Bash parses a whole line before it runs `shopt`, so
  `shopt -s extglob; cat ./@(f0|f1)` on one line is a syntax error
  (`bash -c 'shopt -s extglob; cat ./@(f0|f1)'` fails, the two-line form
  works). The first version of this probe used `;`; the `glob_alternatives`
  numbers below are from the two-line form.
- `alias_chain` is a chain only from the classifier's point of view:
  non-interactive bash does not expand aliases, and a definition does not
  apply to the line it is on.

## How to reproduce

```sh
git clone https://github.com/CorbanuCore/CorbanuTerminal.git
cd CorbanuTerminal
git checkout <this PR's merge commit or later main>
export CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
  CARGO_INCREMENTAL=0 RUST_MIN_STACK=8388608
scripts/classifier_timing_probe.sh classifier-timing-out
cat classifier-timing-out/table.md
```

`OUT_DIR` may be relative (to the directory you run the script from); the
default is `codex-rs/target/classifier-timing`, which git ignores.

The script builds once
(`cargo test --release -p codex-core --lib --no-run --message-format=json`),
takes the `codex_core` lib test executable from cargo's artifact messages,
and runs that executable directly in `codex-rs/core`, one process per
pattern and size:

```sh
cd codex-rs/core
PROBE_PATTERN=arithmetic PROBE_UNITS=4000 PROBE_REPS=5 \
  ../target/release/deps/codex_core-<hash> --ignored --exact --nocapture \
  security::tainted_action::tainted_action_timing_probe_tests::classifier_timing_probe
```

Each process does one warm-up classification and `PROBE_REPS` (5) timed ones
(`std::time::Instant`, tracing silenced, `black_box` on input and result).
The table uses the median of the 5 timed runs; the doubling ratio is the
median at 2n over the median at n. Each process runs under a 600 s limit
(`PROBE_TIMEOUT`; GNU `timeout`, `gtimeout` or perl `alarm` on macOS). The
limit is on the test executable itself, not on a `cargo` parent, so a
process past the limit is ended rather than left running; a run
past the limit is listed as `TIMEOUT`, a crash as `EXIT <code>`, and any
`EXIT` makes the script exit 1. Other knobs: `PROBE_SIZES`, `PROBE_PATTERNS`.

Verdict rule (in the script): **linear** if every doubling ratio is at most
2.5, **super-linear** if any is 3.0 or more, otherwise **unclear**. 2.0 is
exactly linear; a quadratic step gives about 4. Ratios on the arithmetic and
nested-quotes lines are the noisiest because those times are a few
milliseconds.

## Build

The classifier is a library inside `codex-core`, so the build is the
`codex-core` test binary in release mode:
`cargo test --release -p codex-core --lib --no-run`, with the release
overrides `corbanu-terminal-release.yml` uses for the shipped binary
(`CARGO_PROFILE_RELEASE_LTO=false`, `CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16`).
With `Cargo.toml`'s own release profile (thin LTO, 4 codegen units) the
ubuntu-24.04 hosted runner was shut down while linking this test binary
(exit 143), so the overrides are not optional on a hosted runner. Toolchain:
`codex-rs/rust-toolchain.toml` (1.99.1).

## Results

Measured on `main` at `3ae732a62f80eccc51bce019ea4069f4d9dab6e8` with this
branch at `63644feac8b426af343348eacd0acaf5644900cc` merged in (review 1
fixes, extglob on its own line, runner script that times the test
executable directly). These replace the first round's numbers (`main`
`16dcfa301e`, one-line extglob), which are in the PR history and in the
logs linked at the end. Every 1k–8k row in this round is linear, and no
ratio in either round reached 3.0.

Raw data for each run (`environment.txt`, `raw.tsv` with every timed run and
its verdict, `status.tsv` with the exit state and wall seconds per process,
`table.md` as the script wrote it) is in `classifier-timing/`.

### ubuntu-24.04 (AMD EPYC 7763, 4 vCPU, 16 GB; `classifier-timing/ubuntu-24.04/`)

| pattern | 1000 ms | 2000 ms | 4000 ms | 8000 ms | doubling ratios | verdict |
|---|---|---|---|---|---|---|
| nested_substitution | 33.746 | 67.954 | 136.238 | 278.834 | 2.01, 2.00, 2.05 | linear |
| alias_chain | 65.328 | 102.213 | 176.309 | 306.195 | 1.56, 1.72, 1.74 | linear |
| arithmetic | 1.429 | 2.758 | 5.455 | 11.080 | 1.93, 1.98, 2.03 | linear |
| glob_alternatives | 20.284 | 40.390 | 76.613 | 147.403 | 1.99, 1.90, 1.92 | linear |
| nested_quotes | 2.383 | 4.580 | 8.877 | 18.159 | 1.92, 1.94, 2.05 | linear |

Medians of 5 timed runs. All 20 processes exited 0; no timeouts (the
longest process, `alias_chain` at 8000, took 1.87 s including start-up).
The same run's self-test step ran every `security::` test in release mode
on the merged tree: 843 passed, 0 failed, 5 ignored.

### macos-15 (Apple M1 virtual, 3 vCPU, 7 GB; `classifier-timing/macos-15/`)

| pattern | 1000 ms | 2000 ms | 4000 ms | 8000 ms | doubling ratios | verdict |
|---|---|---|---|---|---|---|
| nested_substitution | 92.252 | 138.068 | 203.693 | 424.946 | 1.50, 1.48, 2.09 | linear |
| alias_chain | 131.083 | 197.656 | 279.915 | 492.406 | 1.51, 1.42, 1.76 | linear |
| arithmetic | 40.063 | 43.810 | 43.822 | 61.638 | 1.09, 1.00, 1.41 | linear |
| glob_alternatives | 58.826 | 93.187 | 158.734 | 263.658 | 1.58, 1.70, 1.66 | linear |
| nested_quotes | 37.162 | 48.086 | 63.539 | 61.125 | 1.29, 1.32, 0.96 | linear |

Same run, same merged tree as the ubuntu table. Medians of 5 timed runs.
All 20 processes exited 0; no timeouts (longest: `alias_chain` at 8000,
3.04 s including start-up). The self-test step ran every `security::` test
in release mode: 843 passed, 0 failed, 5 ignored.

On this runner even the cheapest lines take 35–50 ms per classification, so
`arithmetic` and `nested_quotes` (1–18 ms on ubuntu) show that floor plus
noise, not their growth; their verdict comes from the ubuntu run. The timed
runs spread more than on ubuntu (for example `nested_substitution` 4000:
183.0–207.8 ms, 8000: 372.7–430.4 ms). In round 1 the
`nested_substitution` 4000 → 8000 ratio on this runner was 2.51, just past
the 2.5 line; a 15-run rerun then gave 1.43–2.10, and this round gives
2.09 with ubuntu at 2.05, so that 2.51 was runner noise.

## Verdicts

| pattern | verdict | classifier result on the lines |
|---|---|---|
| nested_substitution | linear | `Some(UnseenCode)` at every size |
| alias_chain | linear | `Some(UnseenCode)` at every size |
| arithmetic | linear | `None` at every size |
| glob_alternatives | linear at 1k–8k; below the byte cap, ratios of 2.3–2.5 from 128 to 512 (see below) | `None` at 1000 and 2000, `Some(UnseenCode)` at 4000 and 8000 |
| nested_quotes | linear | `Some(UnseenCode)` at every size |

**No case reached the probe's 3.0 super-linear threshold on either runner,
so no issue was opened.** No size was skipped and no run timed out.

## What these sizes do and do not exercise

- **The grammar reader never runs at the card's sizes.** It refuses any line
  over `MAX_BYTES` = 4096 bytes (`tainted_action/grammar.rs`, checked in
  `read`), and the smallest 1000-unit line is 4899 bytes (`arithmetic`). So
  1k–8k measures the lexer path only. To cover the grammar path, the same
  script ran at 64/128/256/512 units (`PROBE_SIZES="64 128 256 512"`,
  `classifier-timing/ubuntu-24.04-below-cap/`, AMD EPYC 9V74):

  | pattern | 64 ms | 128 ms | 256 ms | 512 ms | doubling ratios | line bytes at 64 / 128 / 256 / 512 |
  |---|---|---|---|---|---|---|
  | nested_substitution | 2.186 | 4.087 | 7.959 | 15.093 | 1.87, 1.95, 1.90 | 453 / 901 / 1797 / 3589 |
  | alias_chain | 5.671 | 11.154 | 21.440 | 29.507 | 1.97, 1.92, 1.38 | 943 / 1959 / **4135** / **8487** |
  | arithmetic | 0.314 | 0.483 | 0.796 | 1.204 | 1.54, 1.65, 1.51 | 255 / 539 / 1179 / 2459 |
  | glob_alternatives | 1.644 | 3.041 | 7.736 | 16.180 | 1.85, 2.54, 2.09 | 271 / 555 / 1195 / 2475 |
  | nested_quotes | 0.521 | 0.870 | 1.469 | 0.963 | 1.67, 1.69, 0.66 | 518 / 1030 / 2054 / **4102** |

  Bold sizes are over `MAX_BYTES`, so the grammar does not run on them.
  For `alias_chain` that is already 256 and 512 units (4135 and 8487
  bytes): only 64 → 128 reaches the grammar for that pattern, and the
  256 → 512 step (1.38) times the lexer path. `nested_quotes` crosses the
  cap at 512 units (4102 bytes) and gets faster (1.47 → 0.96 ms) because the
  grammar no longer runs.
- **`glob_alternatives` below the cap.** In that run its 128 → 256 ratio was
  2.54, just past the script's 2.5 "linear" line. A rerun of this one
  pattern with 15 timed runs per size and a 1024 step
  (`PROBE_PATTERNS=glob_alternatives PROBE_SIZES="64 128 256 512 1024"
  PROBE_REPS=15`, `classifier-timing/ubuntu-24.04-glob-rerun/`, AMD EPYC
  9V74):

  | pattern | 64 ms | 128 ms | 256 ms | 512 ms | 1024 ms | doubling ratios | script verdict |
  |---|---|---|---|---|---|---|---|
  | glob_alternatives | 1.769 | 3.007 | 7.221 | 16.538 | 15.361 | 1.70, 2.40, 2.29, 0.93 | linear |

  The spread is narrow here (256: 7.18–7.36 ms; 512: 15.97–17.16 ms over
  15 runs), so unlike the macOS `nested_substitution` case this is not
  runner noise: on the grammar path the time per alternative grows a little
  with the line (about 2.3–2.5 per doubling from 128 to 512 units, below
  the 3.0 line and well below the ~4 of a quadratic step). At 1024 units
  (5059 bytes, over the cap) the grammar stops and the time drops back to
  15.4 ms. Line length on this path is bounded by `MAX_BYTES`, so the step
  cannot keep growing. The probe does not profile which part of the grammar
  path costs more per alternative.
- **Depth limits.** Quoted text stops at `MAX_NESTING` = 32
  (`tainted_action/quoted.rs`) and command substitutions at
  `MAX_SUBSTITUTION_DEPTH` = 32 (`tainted_action/shell.rs`), so at 1k–8k
  levels `nested_substitution` and `nested_quotes` mostly time the
  "too deep" path; that is why they return `Some(UnseenCode)` at every size.
- **`glob_alternatives` changes verdict between 2038 and 2039
  alternatives** (`PROBE_PATTERNS=glob_alternatives
  PROBE_SIZES="2036 2037 2038 2039 2040"`,
  `classifier-timing/ubuntu-24.04-glob-boundary/`): 2036–2038 (11131–11143
  bytes) give `None`, 2039 and 2040 (11149 and 11155 bytes) give
  `Some(UnseenCode)`, all in 40.3–40.9 ms. The boundary is the same as with
  the one-line form in round 1. This is consistent with the per-line
  file-lookup budget `FS_LOOKUP_BUDGET` = 2048 (`tainted_action/paths.rs`;
  past it the classifier asks rather than guesses), but the probe does not
  count lookups, so that cause is not proven. The time does not jump at the
  boundary, and the result fails closed (asks), so it is not a slowdown.
- Times are wall clock on shared hosted runners, one process per pattern and
  size, so runner-to-runner differences (CPU model, neighbours) move the
  absolute numbers; compare ratios within one run.

## Where the numbers came from

Every run used GitHub-hosted runners through the `classifier-timing`
workflow in `darkmatter-sudo/corbanu-macos-repro`
([workflow at `df9bea0f`](https://github.com/darkmatter-sudo/corbanu-macos-repro/blob/df9bea0fbdcdfc2b811237e947783eb75cc7537e/.github/workflows/classifier-timing.yml)):
clone this repository, check out the `main` commit, merge the probe commit,
then run the self-test and `scripts/classifier_timing_probe.sh` with the
release overrides above. Round 2 runs (this document): 42574734745 (1k–8k,
ubuntu-24.04 and macos-15, self-test `security::`), 42574743667 (64–512),
42574752638 (2036–2040), 42577649476 (`glob_alternatives` rerun). Round 1:
42538602895, 42538684868, 42538688053, 42545500274. The full logs of each
run, including the build output and every per-process log, are in that
repository under `logs/`.
