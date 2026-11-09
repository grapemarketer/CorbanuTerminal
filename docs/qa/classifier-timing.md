# Classifier timing: input shapes #1804 does not cover

Wall-clock timings of the command-safety classifier
(`codex-rs/core/src/security/tainted_action`) for five input shapes that
#1804's linearity tests do not cover, at 1k, 2k, 4k and 8k units each, on a
release build. Nothing in the classifier changes; the probe is an ignored
test (`tainted_action_timing_probe_tests.rs`, never runs in CI) and a runner
script (`scripts/classifier_timing_probe.sh`).

The raw logs below were recorded when the file and module were still named
`tainted_action_timing_probe` (see each `environment.txt`). They were renamed
to `*_tests` afterwards, with no other change, so that `work.rs`'s
`every_classifier_module_is_checked` treats the probe as a test module.

## Patterns

Each line is classified as `bash -lc <line>` in `/work` through
`classify_with`, the same entry point the classifier tests use. A unit is:

| pattern | unit | line at 2 units |
|---|---|---|
| `nested_substitution` | one nesting level | `cat $(cat $(cat f))` |
| `alias_chain` | one alias definition | `alias a0=a1; alias a1=./x; a0` |
| `arithmetic` | one term | `echo $((v0+v1+v2))` (3 units) |
| `glob_alternatives` | one extglob alternative | `shopt -s extglob; cat ./@(f0\|f1)` |
| `nested_quotes` | one quote level | `echo "${a:-"${a:-x}"}"` |

`lines_have_the_units_asked_for` (a normal, non-ignored test) pins these
generators.

## How to reproduce

```sh
git clone https://github.com/CorbanuCore/CorbanuTerminal.git
cd CorbanuTerminal
git checkout <this PR's merge commit or later main>
export CARGO_PROFILE_RELEASE_LTO=false CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
  CARGO_INCREMENTAL=0 RUST_MIN_STACK=8388608
scripts/classifier_timing_probe.sh /tmp/classifier-timing
cat /tmp/classifier-timing/table.md
```

The script builds once (`cargo test --release -p codex-core --lib --no-run`),
then runs one process per pattern and size:

```sh
PROBE_PATTERN=arithmetic PROBE_UNITS=4000 PROBE_REPS=5 \
  cargo test --release -q -p codex-core --lib -- \
  --ignored --exact --nocapture \
  security::tainted_action::tainted_action_timing_probe_tests::classifier_timing_probe
```

Each process does one warm-up classification and `PROBE_REPS` (5) timed ones
(`std::time::Instant`, tracing silenced, `black_box` on input and result).
The table uses the median of the 5 timed runs; the doubling ratio is the
median at 2n over the median at n. Each process runs under a 600 s limit
(`PROBE_TIMEOUT`; GNU `timeout`, `gtimeout` or perl `alarm` on macOS); a run
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

Measured on `main` at `16dcfa301ef611951da5e871aba44fbba3e2d72d` with the
probe commit `254f97af7b9e2462a92acac50c6d2a9ff800e420` merged in. The two
later commits on this branch only add a lint attribute
(`#[expect(clippy::print_stdout)]`), literal-argument comments and rustfmt
line breaks; they do not change what the probe runs or prints.

Raw data for each run (`environment.txt`, `raw.tsv` with every timed run and
its verdict, `status.tsv` with the exit state and wall seconds per process,
`table.md` as the script wrote it) is in `classifier-timing/`.

### ubuntu-24.04 (AMD EPYC 9V74, 4 vCPU, 16 GB; `classifier-timing/ubuntu-24.04/`)

| pattern | 1000 ms | 2000 ms | 4000 ms | 8000 ms | doubling ratios | verdict |
|---|---|---|---|---|---|---|
| nested_substitution | 26.252 | 52.659 | 107.037 | 218.171 | 2.01, 2.03, 2.04 | linear |
| alias_chain | 45.517 | 74.504 | 132.289 | 233.176 | 1.64, 1.78, 1.76 | linear |
| arithmetic | 1.148 | 2.252 | 4.529 | 9.267 | 1.96, 2.01, 2.05 | linear |
| glob_alternatives | 15.189 | 30.945 | 57.321 | 111.186 | 2.04, 1.85, 1.94 | linear |
| nested_quotes | 1.706 | 3.230 | 6.283 | 12.919 | 1.89, 1.95, 2.06 | linear |

Medians of 5 timed runs. All 20 processes exited 0; no timeouts (the
longest process, `alias_chain` at 8000, took 2.09 s including start-up).

### macos-15 (Apple M1 virtual, 3 vCPU, 7 GB; `classifier-timing/macos-15/`)

| pattern | 1000 ms | 2000 ms | 4000 ms | 8000 ms | doubling ratios | script verdict |
|---|---|---|---|---|---|---|
| nested_substitution | 67.516 | 88.819 | 153.283 | 384.408 | 1.32, 1.73, 2.51 | unclear (2.5–3.0) |
| alias_chain | 115.657 | 196.152 | 262.866 | 456.335 | 1.70, 1.34, 1.74 | linear |
| arithmetic | 33.952 | 38.378 | 47.256 | 52.940 | 1.13, 1.23, 1.12 | linear |
| glob_alternatives | 57.656 | 76.088 | 119.072 | 251.884 | 1.32, 1.56, 2.12 | linear |
| nested_quotes | 30.543 | 40.622 | 37.735 | 53.194 | 1.33, 0.93, 1.41 | linear |

All 20 processes exited 0; no timeouts (longest: `alias_chain` at 8000,
4.00 s). On this runner even the cheapest lines take about 30 ms per
classification, so `arithmetic` and `nested_quotes` (1–13 ms on ubuntu) show that floor
plus noise, not their growth; their verdict comes from the ubuntu run.

`nested_substitution` 4000 → 8000 came out at 2.51, just past the script's
2.5 "linear" line. The five timed runs spread widely on this runner (4000:
141.4–175.5 ms; 8000: 335.4–389.6 ms; fastest-to-fastest 2.37), ubuntu gives
2.01, 2.03, 2.04 for the same lines, and an earlier macos-15 run of the same
probe on `main` `f048967c21` gave 1.79, 1.53, 2.01 (run 42535773156).

A macos-15 rerun of this one pattern with 15 timed runs per size and a 16000
step (run 42545500274, same `main` `16dcfa301e`, probe at `68db54cd43`;
`classifier-timing/macos-15-nested-rerun/`) came out linear:

| pattern | 1000 ms | 2000 ms | 4000 ms | 8000 ms | 16000 ms | doubling ratios | script verdict |
|---|---|---|---|---|---|---|---|
| nested_substitution | 93.156 | 133.176 | 217.681 | 456.091 | 831.492 | 1.43, 1.63, 2.10, 1.82 | linear |

Medians of 15 timed runs. All 5 processes exited 0, no timeouts (longest:
16000, 16.37 s). The spread per size is wide again (8000: 321.6–600.1 ms),
so the 2.51 above is runner noise, not growth.

## Verdicts

| pattern | verdict | classifier result on the lines |
|---|---|---|
| nested_substitution | linear (macos-15 1k–8k run had one ratio of 2.51; its 15-run rerun to 16000 is linear) | `Some(UnseenCode)` at every size |
| alias_chain | linear | `Some(UnseenCode)` at every size |
| arithmetic | linear | `None` at every size |
| glob_alternatives | linear | `None` at 1000 and 2000, `Some(UnseenCode)` at 4000 and 8000 |
| nested_quotes | linear | `Some(UnseenCode)` at every size |

**No super-linear case was found, so no issue was opened.** No ratio
reached 3.0 on either runner. No size was skipped and no run timed out.

## What these sizes do and do not exercise

- **The grammar reader never runs at the card's sizes.** It refuses any line
  over `MAX_BYTES` = 4096 bytes (`tainted_action/grammar.rs:31`, checked in
  `read` at line 188), and the smallest 1000-unit line is 4899 bytes
  (`arithmetic`). So 1k–8k measures the lexer path only. To cover the
  grammar path, the same script ran at 64/128/256/512 units
  (`PROBE_SIZES="64 128 256 512"`, `classifier-timing/ubuntu-24.04-below-cap/`,
  AMD EPYC 7763):

  | pattern | 64 ms | 128 ms | 256 ms | 512 ms | doubling ratios | line bytes at 64 / 512 |
  |---|---|---|---|---|---|---|
  | nested_substitution | 2.793 | 5.196 | 10.133 | 19.244 | 1.86, 1.95, 1.90 | 453 / 3589 |
  | alias_chain | 8.366 | 16.561 | 32.602 | 43.612 | 1.98, 1.97, 1.34 | 943 / 8487 |
  | arithmetic | 0.422 | 0.609 | 1.020 | 1.552 | 1.44, 1.68, 1.52 | 255 / 2459 |
  | glob_alternatives | 2.190 | 4.071 | 9.736 | 21.110 | 1.86, 2.39, 2.17 | 272 / 2476 |
  | nested_quotes | 0.683 | 1.154 | 1.953 | 1.373 | 1.69, 1.69, 0.70 | 518 / 4102 |

  All linear. `nested_quotes` at 512 units is 4102 bytes, just over the cap,
  and gets faster (1.95 → 1.37 ms) because the grammar no longer runs.
- **`glob_alternatives` changes verdict between 2038 and 2039
  alternatives** (`PROBE_PATTERNS=glob_alternatives PROBE_SIZES="2038 2039"`,
  `classifier-timing/ubuntu-24.04-glob-boundary/`): 2038 (11144 bytes) gives
  `None`, 2039 (11150 bytes) gives `Some(UnseenCode)`, both in 41.1 ms. This
  is consistent with the per-line file-lookup budget `FS_LOOKUP_BUDGET` =
  2048 (`tainted_action/paths.rs:95`; past it the classifier asks rather than
  guesses), but the probe does not count lookups, so that cause is not
  proven. The time does not jump at the boundary, and the result fails
  closed (asks), so it is not a slowdown.
- Times are wall clock on shared hosted runners, one process per pattern and
  size, so runner-to-runner differences (CPU model, neighbours) move the
  absolute numbers; compare ratios within one run.

## Where the numbers came from

Every run used GitHub-hosted runners through the `classifier-timing`
workflow in `darkmatter-sudo/corbanu-macos-repro`
([workflow at `54e503d3`](https://github.com/darkmatter-sudo/corbanu-macos-repro/blob/54e503d3287314d52be394ef00ec0c7a1b854c89/.github/workflows/classifier-timing.yml)):
clone this repository, check out the `main` commit, merge the probe commit,
then run the self-test and `scripts/classifier_timing_probe.sh` with the
release overrides above. Runs: 42538602895 (1k–8k, ubuntu-24.04 and
macos-15), 42538684868 (64–512), 42538688053 (2038/2039), 42545500274
(macos-15 `nested_substitution` rerun). The full logs of
each run, including the build output and every per-process log, are in
that repository under `logs/`.
