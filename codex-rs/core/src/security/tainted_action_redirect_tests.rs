//! #1772: redirection follow-ups to #1719. Each `ASKS` line was quiet on
//! main (judged with the classifier test harness, `bash -lc <line>` in a
//! workspace whose `./x` and `sub/run.sh` are harmful); each `QUIET` line
//! stays quiet. `ASKS_ON_MAIN`, `REVIEW_ASKS` and `REVIEW_2_ASKS` ask on
//! main and keep asking. `tainted_action_reading_tests.rs` checks both
//! claims against main's reading. In its own file:
//! `tainted_action_tests.rs` is at the repository's blob size limit.

use super::*;
use pretty_assertions::assert_eq;

#[cfg(unix)]
pub(super) const HOME: &str = "/home/fixture/.corbanu";
#[cfg(unix)]
const EVIL: &str = "echo x >> ~/.zshenv\n";

/// The classifier's verdict on `bash -lc <line>` in a temporary workspace
/// holding `files` and an empty `sub/`.
#[cfg(unix)]
fn in_workspace(files: &[(&str, &str)], line: &str) -> Option<ProtectedActionKind> {
    in_workspace_argv(files, &["bash", "-lc", line])
}

/// [`in_workspace`] of the action whose argv is `argv`.
#[cfg(unix)]
pub(super) fn in_workspace_argv(
    files: &[(&str, &str)],
    argv: &[&str],
) -> Option<ProtectedActionKind> {
    let root = workspace(files);
    let action = action_in(root.path(), argv);
    classify_with(&action, Path::new(HOME), Some(root.path()))
}

/// A temporary workspace holding `files` and an empty `sub/`.
#[cfg(unix)]
pub(super) fn workspace(files: &[(&str, &str)]) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("tempdir");
    let work = root.path();
    std::fs::create_dir_all(work.join("sub")).expect("sub");
    for (name, text) in files {
        let path = work.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("folder");
        }
        std::fs::write(path, text).expect("file");
    }
    root
}

/// The shell action whose argv is `argv`, run in `work`.
#[cfg(unix)]
pub(super) fn action_in(work: &Path, argv: &[&str]) -> ApprovalAction {
    use crate::sandboxing::SandboxPermissions;
    use codex_utils_absolute_path::AbsolutePathBuf;
    ApprovalAction::Shell {
        id: "call".into(),
        environment_id: "local".into(),
        command: argv.iter().map(|arg| (*arg).to_string()).collect(),
        cwd: PathUri::from_abs_path(
            &AbsolutePathBuf::from_absolute_path_checked(work).expect("absolute"),
        ),
        sandbox_permissions: SandboxPermissions::UseDefault,
        additional_permissions: None,
        justification: None,
    }
}

/// Files of a temporary workspace: name and text.
#[cfg(unix)]
pub(super) type Files = &'static [(&'static str, &'static str)];

#[cfg(unix)]
pub(super) const SUB_EVIL: &[(&str, &str)] = &[("run.sh", "true\n"), ("sub/run.sh", EVIL)];
#[cfg(unix)]
pub(super) const X_EVIL: &[(&str, &str)] = &[("x", EVIL)];
#[cfg(unix)]
pub(super) const X_OK: &[(&str, &str)] = &[("x", "true\n")];

/// Every line #1772 lists, by item, with the files it is judged among.
#[cfg(unix)]
pub(super) const ASKS: &[(&str, Files, &str)] = &[
    // Item 1: a command behind a leading redirection, judged by name only.
    ("1", SUB_EVIL, "2>/dev/null cd sub && ./run.sh"),
    ("1", SUB_EVIL, ">log cd sub; ./run.sh"),
    ("1", SUB_EVIL, ">log pushd sub && ./run.sh"),
    ("1", X_EVIL, ">log $(cat x)"),
    ("1", X_EVIL, ">log `cat x`"),
    ("1", X_EVIL, ">log PATH=.:$PATH x"),
    ("1", X_EVIL, ">log export PATH=.:$PATH && x"),
    ("1", X_EVIL, "2>/dev/null find . -exec ./x \\;"),
    // Item 2: a redirect file name with a quoted or escaped space.
    ("2", X_EVIL, ">'a b' ./x"),
    ("2", X_EVIL, ">\"a b\" ./x"),
    ("2", X_EVIL, ">a\\ b ./x"),
    ("2", X_EVIL, "2>'a b' ./x"),
    // Item 3: repeated stdin or here-string redirections.
    ("3", X_EVIL, "<a <b ./x"),
    ("3", X_OK, "sudo <a <b -A true"),
    ("3", X_OK, "<a <b sudo -A true"),
    ("3", X_OK, "sudo 3<a <b -A true"),
    ("3", X_OK, "sudo <<<a <<<b -A true"),
    // Item 4: `&>` read as `&` and `>`.
    ("4", SUB_EVIL, "&>log cd sub && ./run.sh"),
];

/// PR #1794 review 2: lines #1772 lists that already ask on main in this
/// harness (`./x` is harmful; `&>` is read as `&` and `>log ./x`, which
/// #1719 asks for). They keep asking.
#[cfg(unix)]
pub(super) const ASKS_ON_MAIN: &[(&str, Files, &str)] = &[
    ("1", X_EVIL, ">log git -c core.pager=./x log"),
    ("4", X_EVIL, "&>log ./x"),
    ("4", X_EVIL, "sudo &>log -u root ./x"),
    ("4", X_EVIL, "&>log sh x"),
    ("4", X_EVIL, "&>>log ./x"),
];

/// Ordinary lines with the same redirections.
#[cfg(unix)]
pub(super) const QUIET: &[&str] = &[
    "2>/dev/null git status",
    ">log ls",
    "ls >'a b'",
    "echo hi >'a b'",
    "echo hi >a\\ b",
    "cat <a <b",
    "<a <b cat",
    "cat <<<a <<<b",
    "ls &>log",
    "ls &>>log",
    "sleep 1 & ls",
];

#[cfg(unix)]
#[test]
fn issue_1772_redirection_cases_ask() {
    let quiet: Vec<String> = ASKS
        .iter()
        .chain(ASKS_ON_MAIN)
        .filter(|(_, files, line)| in_workspace(files, line).is_none())
        .map(|(item, _, line)| format!("item {item}: {line}"))
        .collect();
    assert_eq!(quiet, Vec::<String>::new(), "#1772 lines still quiet");
    let asking: Vec<(&str, Option<ProtectedActionKind>)> = QUIET
        .iter()
        .map(|line| (*line, in_workspace(X_OK, line)))
        .filter(|(_, verdict)| verdict.is_some())
        .collect();
    assert_eq!(asking, Vec::new(), "ordinary redirections now ask");
}

/// #1719's asks stay: a path, shell or `eval` hidden behind a redirection
/// as written asks also when the file it runs is harmless.
#[cfg(unix)]
#[test]
fn issue_1772_keeps_1719_hidden_command_asks() {
    let quiet: Vec<&str> = [
        ">log ./x",
        "2>&1 ./x",
        "<<EOF ./x",
        ">log eval ./x",
        "eval '>log ./x'",
        ">'a b' ./x",
    ]
    .into_iter()
    .filter(|line| in_workspace(X_OK, line).is_none())
    .collect();
    assert_eq!(quiet, Vec::<&str>::new(), "#1719 lines now quiet");
}

/// PR #1794 review 1: lines that ask on main and must keep asking. Item 1:
/// the `>` or `<` before a quote may be quoted text, a comment, a `case`
/// pattern or a here-document, so the quoted-file reading can swallow the
/// commands after it. Item 2: `sh` and `dash` read `&>` as `&` and `>`.
/// Item 3: a substitution whose stdout is redirected prints nothing.
#[cfg(unix)]
pub(super) const REVIEW_ASKS: &[(&str, &str)] = &[
    ("1", "echo '>\"'; ./x; echo '\"'"),
    ("1", "echo \">'\" && ./x && echo \"'\""),
    ("1", "echo '>'; echo \"'\"; ./x; echo \"'\""),
    ("1", "printf '%s\\n' 'a>\"b'; ./x; echo '\"'"),
    ("1", "echo '<\"'; ./x; echo '\"'"),
    ("1", "echo '2>\"'; ./x; echo '\"'"),
    ("1", "case a in \">'\") ;; esac; ./x; echo \"'\""),
    ("1", "ls # >\"\n./x\n# \""),
    ("1", "cat <<'EOF'\na >'\nEOF\n./x\necho \"'\""),
    ("1", "grep '>\"' f | xargs ./x '\"'"),
    ("1", "find . -name '>\"' -exec ./x {} + -name '\"'"),
    ("2", "sh -c 'true &>f ./x'"),
    ("2", "dash -c 'true &>f ./x'"),
    ("2", "bash -c 'true &>f ./x'"),
    ("2", "true &>a ./x"),
    ("3", "$(>&2 echo true) ./x"),
    ("3", "`>&2 echo true` ./x"),
    ("3", "$(>log echo true) ./x"),
    ("3", "$(>&2 printf true) ./x"),
    ("3", "$(echo true >log) ./x"),
];

#[cfg(unix)]
#[test]
fn issue_1772_review_lines_ask() {
    let mut quiet: Vec<String> = REVIEW_ASKS
        .iter()
        .filter(|(_, line)| in_workspace(X_EVIL, line).is_none())
        .map(|(item, line)| format!("item {item}: {line}"))
        .collect();
    for argv in [["sh", "-c", "true &>f ./x"], ["dash", "-c", "true &>f ./x"]] {
        if in_workspace_argv(X_EVIL, &argv).is_none() {
            quiet.push(format!("item 2: {argv:?}"));
        }
    }
    assert_eq!(quiet, Vec::<String>::new(), "review lines quiet");
}

/// PR #1794 review 2: lines that ask on main and were quiet with the
/// reorder and the quoted-file reading in main's judgment too. Item 1: a
/// reserved word after a redirection is no reserved word, so the loop
/// stays open and `sh` reads the piped text. Item 2 (argv) and item 3
/// (`git -c`): the helper lexers read as main. Item 4: a leading
/// non-stdout redirection in a substitution that prints nothing. Item 5:
/// quoted `<` and `>` in a search pattern (main's verdict, kept). Item 6:
/// a leading redirection asks as on main.
#[cfg(unix)]
pub(super) const REVIEW_2_ASKS: &[(&str, Files, &[&str])] = &[
    (
        "1",
        X_EVIL,
        &[
            "bash",
            "-lc",
            "cat x | while read l; do true; >/dev/null done; sh; done",
        ],
    ),
    (
        "1",
        X_EVIL,
        &[
            "bash",
            "-lc",
            "cat x | while read l; do true; 2>/dev/null done; sh; done",
        ],
    ),
    (
        "1",
        X_EVIL,
        &[
            "bash",
            "-lc",
            "cat x | if true; then true; >/dev/null fi; sh; fi",
        ],
    ),
    (
        "1",
        X_EVIL,
        &[
            "bash",
            "-lc",
            "cat x | case a in a) true; >/dev/null esac; sh;; esac",
        ],
    ),
    ("2", X_OK, &[">'a b'", "ls"]),
    ("2", X_OK, &["env", ">'a b'", "ls"]),
    ("2", X_OK, &[">a\\ b", "ls"]),
    ("2", X_OK, &[">\"a b\"", "ls"]),
    (
        "3",
        X_OK,
        &["bash", "-lc", "git -c \"user.name=x>'a g\\it'\" status"],
    ),
    ("3", X_OK, &["git", "-c", "user.name=x>'a g\\it'", "status"]),
    ("4", X_EVIL, &["bash", "-lc", "$(2>log echo) ./x"]),
    ("4", X_EVIL, &["bash", "-lc", "$(2>&1 echo) ./x"]),
    ("4", X_EVIL, &["bash", "-lc", "$(<f echo) ./x"]),
    ("4", X_EVIL, &["bash", "-lc", "$(<<<a echo) ./x"]),
    ("4", X_EVIL, &["bash", "-lc", "$(2>log printf '') ./x"]),
    ("4", X_EVIL, &["bash", "-lc", "`2>log echo` ./x"]),
    (
        "5",
        X_OK,
        &[
            "bash",
            "-lc",
            "grep -n \"<<<<<<<\\|>>>>>>>\\|=======\" -A3 f",
        ],
    ),
    ("6", X_OK, &["bash", "-lc", ">./out.txt echo hi"]),
];

#[cfg(unix)]
#[test]
fn issue_1772_review_2_lines_ask() {
    let quiet: Vec<String> = REVIEW_2_ASKS
        .iter()
        .filter(|(_, files, argv)| in_workspace_argv(files, argv).is_none())
        .map(|(item, _, argv)| format!("item {item}: {argv:?}"))
        .collect();
    assert_eq!(quiet, Vec::<String>::new(), "review 2 lines quiet");
}

/// PR #1794 review 2: the reorder through the whole classifier, both
/// readings, with redirections and other words interleaved (`>a x >b y
/// ...`). Each input is 100 units at 1x.
#[cfg(unix)]
#[test]
fn issue_1772_interleaved_redirections_are_linear() {
    let root = workspace(X_OK);
    for (label, unit) in [
        ("stdout", " >a x"),
        ("stderr", " 2>a x"),
        ("stdin", " <a x"),
        ("here-string", " <<<a x"),
        ("both", " &>a x"),
        ("quoted file", " >'a b' x"),
        ("escaped file", " >a\\ b x"),
        ("descriptor", " 2>&1 x"),
    ] {
        tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
            work::assert_linear(label, |scale| {
                let line = format!("echo{}", unit.repeat(100 * scale));
                let action = action_in(root.path(), &["bash", "-lc", &line]);
                classify_with(&action, Path::new(HOME), Some(root.path()));
            });
        });
    }
}
