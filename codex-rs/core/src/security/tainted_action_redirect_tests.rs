//! #1772: redirection follow-ups to #1719. Each `ASKS` line was quiet on
//! main (judged with the classifier test harness, `bash -lc <line>` in a
//! workspace whose `./x` and `sub/run.sh` are harmful); each `QUIET` line
//! stays quiet. In its own file: `tainted_action_tests.rs` is at the
//! repository's blob size limit.

use super::*;
use pretty_assertions::assert_eq;

#[cfg(unix)]
const HOME: &str = "/home/fixture/.corbanu";
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
fn in_workspace_argv(files: &[(&str, &str)], argv: &[&str]) -> Option<ProtectedActionKind> {
    use crate::sandboxing::SandboxPermissions;
    use codex_utils_absolute_path::AbsolutePathBuf;
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
    let action = ApprovalAction::Shell {
        id: "call".into(),
        environment_id: "local".into(),
        command: argv.iter().map(|arg| (*arg).to_string()).collect(),
        cwd: PathUri::from_abs_path(
            &AbsolutePathBuf::from_absolute_path_checked(work).expect("absolute"),
        ),
        sandbox_permissions: SandboxPermissions::UseDefault,
        additional_permissions: None,
        justification: None,
    };
    classify_with(&action, Path::new(HOME), Some(work))
}

/// One command as the lexer reads it: words, stdin files, here-strings,
/// files written, other words a redirection holds.
type Read = (Vec<String>, Vec<usize>, Vec<usize>, Vec<usize>, Vec<usize>);

/// The words and redirection positions the lexer reads, per command.
// `iter().copied()`: `stdin_from` and `here_string` are `Option`s on main,
// where the reverted run builds these tests too.
#[allow(clippy::iter_cloned_collect)]
fn lexed(line: &str) -> Vec<Read> {
    shell::simple_commands(&[line.to_string()])
        .commands
        .into_iter()
        .map(|command| {
            (
                command.words,
                // `iter()`: also on main, where each was an `Option`.
                command.stdin_from.iter().copied().collect(),
                command.here_string.iter().copied().collect(),
                command.writes_to,
                command.redirect_words,
            )
        })
        .collect()
}

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_string()).collect()
}

/// Files of a temporary workspace: name and text.
#[cfg(unix)]
type Files = &'static [(&'static str, &'static str)];

#[cfg(unix)]
const SUB_EVIL: &[(&str, &str)] = &[("run.sh", "true\n"), ("sub/run.sh", EVIL)];
#[cfg(unix)]
const X_EVIL: &[(&str, &str)] = &[("x", EVIL)];
#[cfg(unix)]
const X_OK: &[(&str, &str)] = &[("x", "true\n")];

/// Every line #1772 lists, by item, with the files it is judged among.
#[cfg(unix)]
const ASKS: &[(&str, Files, &str)] = &[
    // Item 1: a command behind a leading redirection, judged by name only.
    ("1", SUB_EVIL, "2>/dev/null cd sub && ./run.sh"),
    ("1", SUB_EVIL, ">log cd sub; ./run.sh"),
    ("1", SUB_EVIL, ">log pushd sub && ./run.sh"),
    ("1", X_EVIL, ">log $(cat x)"),
    ("1", X_EVIL, ">log `cat x`"),
    ("1", X_EVIL, ">log PATH=.:$PATH x"),
    ("1", X_EVIL, ">log export PATH=.:$PATH && x"),
    ("1", X_EVIL, "2>/dev/null find . -exec ./x \\;"),
    ("1", X_EVIL, ">log git -c core.pager=./x log"),
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
    ("4", X_EVIL, "&>log ./x"),
    ("4", X_EVIL, "sudo &>log -u root ./x"),
    ("4", X_EVIL, "&>log sh x"),
    ("4", SUB_EVIL, "&>log cd sub && ./run.sh"),
    ("4", X_EVIL, "&>>log ./x"),
];

/// Ordinary lines with the same redirections.
#[cfg(unix)]
const QUIET: &[&str] = &[
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
const REVIEW_ASKS: &[(&str, &str)] = &[
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
    for argv in [
        ["sh", "-c", "true &>f ./x"],
        ["dash", "-c", "true &>f ./x"],
    ] {
        if in_workspace_argv(X_EVIL, &argv).is_none() {
            quiet.push(format!("item 2: {argv:?}"));
        }
    }
    assert_eq!(quiet, Vec::<String>::new(), "review lines quiet");
}

/// The lexer's reading: a redirection's words follow the command's, every
/// `<` and `<<<` is kept, a quoted or escaped file is one word, and `&>`
/// is one redirection, not a command separator.
#[test]
fn issue_1772_lexer_reads_redirections_as_the_shell_does() {
    let none: Vec<usize> = Vec::new();
    // Item 1: the words a leading redirection holds move last.
    assert_eq!(
        lexed(">log cd sub"),
        vec![(
            strings(&["cd", "sub", "log"]),
            none.clone(),
            none.clone(),
            vec![2],
            none.clone()
        )]
    );
    assert_eq!(
        lexed("2>/dev/null git status"),
        vec![(
            strings(&["git", "status", "2", "/dev/null"]),
            none.clone(),
            none.clone(),
            vec![3],
            vec![2]
        )]
    );
    // Already last: unchanged.
    assert_eq!(
        lexed("cd sub >log"),
        vec![(
            strings(&["cd", "sub", "log"]),
            none.clone(),
            none.clone(),
            vec![2],
            none.clone()
        )]
    );
    // Item 2.
    for line in [">'a b' ./x", ">\"a b\" ./x", ">a\\ b ./x"] {
        assert_eq!(
            lexed(line),
            vec![(
                strings(&["./x", "a b"]),
                none.clone(),
                none.clone(),
                vec![1],
                none.clone()
            )],
            "{line}"
        );
    }
    assert_eq!(
        lexed("2>'a;b' ./x"),
        vec![(
            strings(&["./x", "2", "a;b"]),
            none.clone(),
            none.clone(),
            vec![2],
            vec![1]
        )]
    );
    // A double-quoted file holding an expansion, and a quote inside it,
    // are read as before (the quotes dropped).
    assert_eq!(
        lexed(">\"$d's\" ./x 'q'"),
        vec![(
            strings(&["./x", "q", "$ds"]),
            none.clone(),
            none.clone(),
            vec![2],
            none.clone()
        )]
    );
    // Item 3.
    assert_eq!(
        lexed("<a <b ./x"),
        vec![(
            strings(&["./x", "a", "b"]),
            vec![1, 2],
            none.clone(),
            none.clone(),
            none.clone()
        )]
    );
    assert_eq!(
        lexed("sudo <<<a <<<b -A true"),
        vec![(
            strings(&["sudo", "-A", "true", "a", "b"]),
            none.clone(),
            vec![3, 4],
            none.clone(),
            none.clone()
        )]
    );
    // Item 4.
    assert_eq!(
        lexed("sudo &>log -u root ./x"),
        vec![(
            strings(&["sudo", "-u", "root", "./x", "log"]),
            none.clone(),
            none.clone(),
            vec![4],
            none
        )]
    );
    assert_eq!(lexed("sleep 1 & ls").len(), 2);
}

/// The quoted-file reading scans each text once: an unclosed quote before
/// many redirections, quoted files, double quotes holding an expansion and
/// escaped spaces. Each input is 100 units at 1x, so the 1x run is the same
/// reading as the larger one.
#[test]
fn issue_1772_quoted_files_are_linear() {
    for (label, prefix, unit) in [
        ("unclosed single", ">'", " >a\\ b"),
        ("unclosed double", ">\"", " >a\\ b"),
        ("quoted single", "", ">'a b' "),
        ("quoted double", "", ">\"a b\" "),
        ("expanding double", "", ">\"$a\" "),
        ("escaped space", "", ">a\\ b "),
    ] {
        tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
            work::assert_linear(label, |scale| {
                lexed(&format!("{prefix}{}", unit.repeat(100 * scale)));
            });
        });
    }
}
