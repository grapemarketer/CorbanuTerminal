//! Wall-clock probe for input shapes #1804's linearity tests do not
//! cover: nested command substitutions, alias chains, arithmetic with many
//! terms, glob alternatives and nested quotes. Ignored (no assertions on
//! timings, never runs in CI). One pattern and size per run, so a timeout
//! or a crash stays one data point:
//!
//! ```text
//! PROBE_PATTERN=arithmetic PROBE_UNITS=4000 PROBE_REPS=5 \
//!   cargo test --release -p codex-core --lib -- --ignored --nocapture classifier_timing_probe
//! ```
//!
//! Prints one `probe` line per timed run: pattern, units, line bytes, run
//! number, nanoseconds and verdict.

use super::*;
use crate::sandboxing::SandboxPermissions;
use codex_utils_absolute_path::AbsolutePathBuf;
use std::path::PathBuf;
use std::time::Instant;

const HOME: &str = "/home/fixture/.corbanu";
const USER_HOME: &str = "/home/fixture";

/// The patterns, by `PROBE_PATTERN` name.
pub(super) const PATTERNS: &[&str] = &[
    "nested_substitution",
    "alias_chain",
    "arithmetic",
    "glob_alternatives",
    "nested_quotes",
];

/// A fixture path, on drive `C:` on Windows (#1240).
fn fixture(path: &str) -> PathBuf {
    if cfg!(windows) && !Path::new(path).is_absolute() {
        PathBuf::from(format!("C:{}", path.replace('/', "\\")))
    } else {
        PathBuf::from(path)
    }
}

/// The verdict on `bash -lc <line>` run in `/work`.
fn script(line: &str) -> Option<ProtectedActionKind> {
    let action = ApprovalAction::Shell {
        id: "call".into(),
        environment_id: "local".into(),
        command: vec!["bash".to_string(), "-lc".to_string(), line.to_string()],
        cwd: PathUri::from_abs_path(
            &AbsolutePathBuf::from_absolute_path_checked(fixture("/work")).expect("absolute"),
        ),
        sandbox_permissions: SandboxPermissions::UseDefault,
        additional_permissions: None,
        justification: None,
    };
    classify_with(&action, &fixture(HOME), Some(&fixture(USER_HOME)))
}

/// The line for `pattern` with `n` units.
pub(super) fn line(pattern: &str, n: usize) -> String {
    match pattern {
        // Unit: one nesting level. `cat $(cat $(… $(cat f) …))`.
        "nested_substitution" => {
            format!("cat {}f{}", "$(cat ".repeat(n), ")".repeat(n))
        }
        // Unit: one definition. `alias a0=a1; …; alias a<n-1>=./x; a0`.
        "alias_chain" => {
            let mut line = String::new();
            for i in 0..n {
                if i + 1 == n {
                    line.push_str(&format!("alias a{i}=./x; "));
                } else {
                    line.push_str(&format!("alias a{i}=a{}; ", i + 1));
                }
            }
            line.push_str("a0");
            line
        }
        // Unit: one term. `echo $((v0+v1+…))`.
        "arithmetic" => {
            let terms: Vec<String> = (0..n).map(|i| format!("v{i}")).collect();
            format!("echo $(({}))", terms.join("+"))
        }
        // Unit: one alternative. `cat ./@(f0|f1|…)` (extglob).
        "glob_alternatives" => {
            let alternatives: Vec<String> = (0..n).map(|i| format!("f{i}")).collect();
            format!("shopt -s extglob; cat ./@({})", alternatives.join("|"))
        }
        // Unit: one quote level. `echo "${a:-"${a:-"…x…"}"}"`.
        "nested_quotes" => {
            format!("echo {}x{}", "\"${a:-".repeat(n), "}\"".repeat(n))
        }
        other => panic!("unknown PROBE_PATTERN {other:?}; one of {PATTERNS:?}"),
    }
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .map(|value| value.parse().unwrap_or_else(|_| panic!("{name}={value:?}")))
        .unwrap_or(default)
}

#[test]
fn lines_have_the_units_asked_for() {
    assert_eq!(line("nested_substitution", 2), "cat $(cat $(cat f))");
    assert_eq!(line("alias_chain", 2), "alias a0=a1; alias a1=./x; a0");
    assert_eq!(line("arithmetic", 3), "echo $((v0+v1+v2))");
    assert_eq!(
        line("glob_alternatives", 2),
        "shopt -s extglob; cat ./@(f0|f1)"
    );
    assert_eq!(line("nested_quotes", 2), "echo \"${a:-\"${a:-x}\"}\"");
}

#[test]
#[ignore = "timing probe: run by hand, see the module docs"]
fn classifier_timing_probe() {
    let pattern = std::env::var("PROBE_PATTERN").expect("PROBE_PATTERN");
    let n = env_usize("PROBE_UNITS", 1000);
    let reps = env_usize("PROBE_REPS", 5);
    let line = line(&pattern, n);
    // Tracing silenced while timing, as the work counter does (#1716).
    tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
        let warm = Instant::now();
        let verdict = script(&line);
        println!(
            "probe\t{pattern}\t{n}\t{}\twarmup\t{}\t{verdict:?}",
            line.len(),
            warm.elapsed().as_nanos()
        );
        for rep in 1..=reps {
            let start = Instant::now();
            let verdict = std::hint::black_box(script(std::hint::black_box(&line)));
            println!(
                "probe\t{pattern}\t{n}\t{}\t{rep}\t{}\t{verdict:?}",
                line.len(),
                start.elapsed().as_nanos()
            );
        }
    });
}
