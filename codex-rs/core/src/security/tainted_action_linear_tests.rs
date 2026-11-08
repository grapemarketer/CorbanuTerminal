//! #1774: the superlinear costs found by #1719's linearity probes, each
//! held to linear work: argv environment assignments (`env A1=1 … ./x`),
//! long runs of here-documents (`<<E <<E … ./x`) and long runs of leading
//! writes (`>a >a … ./x`). The here-documents' cost was in the bash
//! grammar, outside the work counter, so their tests also count its
//! parses. Each long line still gets main's verdict. In its own file:
//! `tainted_action_tests.rs` is over the repository's blob size limit.

use super::*;
use crate::sandboxing::SandboxPermissions;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use std::path::PathBuf;

/// The work counter with tracing silenced while it counts, as in
/// `tainted_action_tests.rs` (#1716).
mod work {
    pub(super) fn assert_linear(label: &str, run: impl Fn(usize)) {
        quiet(|| super::super::work::assert_linear(label, run));
    }

    pub(super) fn parses(run: impl FnOnce()) -> u64 {
        quiet(|| super::super::work::parses(run))
    }

    fn quiet<T>(run: impl FnOnce() -> T) -> T {
        tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), run)
    }
}

const HOME: &str = "/home/fixture/.corbanu";
const USER_HOME: &str = "/home/fixture";

/// A fixture path, on drive `C:` on Windows (#1240).
fn fixture(path: &str) -> PathBuf {
    if cfg!(windows) && !Path::new(path).is_absolute() {
        PathBuf::from(format!("C:{}", path.replace('/', "\\")))
    } else {
        PathBuf::from(path)
    }
}

/// The verdict on `command` run in `/work`.
fn kind(command: &[String]) -> Option<ProtectedActionKind> {
    let action = ApprovalAction::Shell {
        id: "call".into(),
        environment_id: "local".into(),
        command: command.to_vec(),
        cwd: PathUri::from_abs_path(
            &AbsolutePathBuf::from_absolute_path_checked(fixture("/work")).expect("absolute"),
        ),
        sandbox_permissions: SandboxPermissions::UseDefault,
        additional_permissions: None,
        justification: None,
    };
    classify_with(&action, &fixture(HOME), Some(&fixture(USER_HOME)))
}

fn script(line: &str) -> Option<ProtectedActionKind> {
    kind(&["bash".to_string(), "-lc".to_string(), line.to_string()])
}

/// `env`, then `n` assignments, then `rest`, as argv words.
fn env_argv(n: usize, rest: &[&str]) -> Vec<String> {
    std::iter::once("env".to_string())
        .chain((0..n).map(|i| format!("A{i}=1")))
        .chain(rest.iter().map(|word| (*word).to_string()))
        .collect()
}

/// Item 1: each argv word was checked against every word before it for a
/// shell name, by the word lexer (`shell::simple_commands`) and by the
/// quote-aware reading (`nested.rs`); now each word is checked once.
#[test]
fn issue_1774_argv_assignments_are_linear() {
    for (label, rest) in [
        ("env assignments", &["./x"][..]),
        ("env assignments then a shell", &["bash", "-c", "./x"][..]),
        (
            "env assignments then a substitution",
            &["A=$(true)", "./x"][..],
        ),
    ] {
        work::assert_linear(label, |scale| {
            let _ = kind(&env_argv(400 * scale, rest));
        });
    }
}

/// Item 2: `SedScope::of` parsed every line with the bash grammar, whose
/// here-document scanner takes time growing with the square of the
/// here-documents left open; the work counter cannot see inside it.
#[test]
fn issue_1774_here_document_runs_are_linear() {
    for (label, line) in [
        ("here-documents", "{}./x"),
        ("here-documents after sed", "sed 1d f {}"),
    ] {
        work::assert_linear(label, |scale| {
            let _ = script(&line.replace("{}", &"<<E ".repeat(400 * scale)));
        });
    }
}

/// Item 2: a line that names no `sed` is not parsed for its sed scope (no
/// command there can be named `sed`), and a line holding more than
/// `MAX_SED_SCOPE_HEREDOCS` `<<` is not parsed either.
#[test]
fn issue_1774_sed_scope_parses_only_lines_that_can_have_one() {
    let unparsed = [
        format!("{}./x", "<<E ".repeat(/*n*/ 1000)),
        "cat f | grep x > out".to_string(),
        format!("sed 1d f {}", "<<E ".repeat(MAX_SED_SCOPE_HEREDOCS + 1)),
    ];
    for line in &unparsed {
        assert_eq!(
            work::parses(|| {
                let _ = script(line);
            }),
            0,
            "{line:.40}"
        );
    }
    // Controls: lines that can name `sed` are still parsed for it.
    let parsed = [
        "sed -e 'a ./run.sh' f && ./run.sh".to_string(),
        format!("sed 1d f {}", "<<E ".repeat(MAX_SED_SCOPE_HEREDOCS)),
        "gsed -i 1d f".to_string(),
    ];
    for line in &parsed {
        assert!(
            work::parses(|| {
                let _ = script(line);
            }) > 0,
            "{line:.40}"
        );
    }
}

/// Item 3: each word was looked up in `writes_to`, which holds one index
/// per output redirection; `Vec::contains` is outside the work counter.
#[test]
fn issue_1774_leading_write_runs_are_linear() {
    for (label, line) in [
        ("leading writes", "{}./x"),
        ("leading writes then a write", "{}echo x >> ~/.zshenv"),
    ] {
        work::assert_linear(label, |scale| {
            let _ = script(&line.replace("{}", &">a ".repeat(400 * scale)));
        });
    }
}

/// The long lines keep main's verdicts: what follows a long run is still
/// read, and a write before one is still judged.
#[cfg(unix)]
#[test]
fn issue_1774_long_runs_keep_their_verdicts() {
    use ProtectedActionKind::*;
    let run = |unit: &str| unit.repeat(/*n*/ 400);
    let cases: Vec<(Vec<String>, Option<ProtectedActionKind>)> = vec![
        (
            env_argv(/*n*/ 400, &["bash", "-c", "cat ~/.ssh/id_rsa"]),
            Some(Credentials),
        ),
        (env_argv(/*n*/ 400, &["./x"]), Some(UnseenCode)),
        (
            [
                vec!["env".to_string(), "A=$(true)".to_string()],
                env_argv(/*n*/ 400, &["sh", "-c", "cat ~/.ssh/id_rsa"])[1..].to_vec(),
            ]
            .concat(),
            Some(Credentials),
        ),
        (
            vec![
                "bash".into(),
                "-lc".into(),
                format!("{}echo x >> ~/.zshenv", run(">a ")),
            ],
            Some(Persistence),
        ),
        (
            vec![
                "bash".into(),
                "-lc".into(),
                format!(">~/.zshenv {}echo x", run(">a ")),
            ],
            Some(Persistence),
        ),
        (
            vec!["bash".into(), "-lc".into(), format!("{}./x", run(">a "))],
            Some(UnseenCode),
        ),
        (
            vec![
                "bash".into(),
                "-lc".into(),
                format!("{}cat ~/.ssh/id_rsa", run("<<E ")),
            ],
            Some(Credentials),
        ),
        (
            vec!["bash".into(), "-lc".into(), format!("{}./x", run("<<E "))],
            Some(UnseenCode),
        ),
        (
            vec![
                "bash".into(),
                "-lc".into(),
                format!("sed 1d f {}", run("<<E ")),
            ],
            None,
        ),
    ];
    for (command, expected) in cases {
        let shown = command.join(" ");
        assert_eq!(kind(&command), expected, "{:.60}", shown);
    }
}
