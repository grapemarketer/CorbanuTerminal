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
        command: ["bash", "-lc", line].map(str::to_string).to_vec(),
        cwd: PathUri::from_abs_path(
            &AbsolutePathBuf::from_absolute_path_checked(work).expect("absolute"),
        ),
        sandbox_permissions: SandboxPermissions::UseDefault,
        additional_permissions: None,
        justification: None,
    };
    classify_with(&action, Path::new(HOME), Some(work))
}

/// The words and redirection positions the lexer reads, per command.
fn lexed(line: &str) -> Vec<(Vec<String>, Vec<usize>, Vec<usize>, Vec<usize>, Vec<usize>)> {
    shell::simple_commands(&["bash", "-lc", line].map(str::to_string))
        .commands
        .into_iter()
        .map(|command| {
            (
                command.words,
                command.stdin_from,
                command.here_string,
                command.writes_to,
                command.redirect_words,
            )
        })
        .collect()
}

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_string()).collect()
}

#[cfg(unix)]
const SUB_EVIL: &[(&str, &str)] = &[("run.sh", "true\n"), ("sub/run.sh", EVIL)];
#[cfg(unix)]
const X_EVIL: &[(&str, &str)] = &[("x", EVIL)];
#[cfg(unix)]
const X_OK: &[(&str, &str)] = &[("x", "true\n")];

/// Every line #1772 lists, by item, with the files it is judged among.
#[cfg(unix)]
const ASKS: &[(&str, &[(&str, &str)], &str)] = &[
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
            none.clone()
        )]
    );
    assert_eq!(lexed("sleep 1 & ls").len(), 2);
}

/// The quoted-file reading scans each text once: unclosed quotes and
/// double quotes holding an expansion after many redirections.
#[test]
fn issue_1772_quoted_files_are_linear() {
    for (label, unit) in [
        ("unclosed single", ">' "),
        ("unclosed double", ">\" "),
        ("expanding double", ">\"$a\" "),
        ("escaped space", ">a\\ b "),
    ] {
        tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
            work::assert_linear(label, |scale| {
                lexed(&unit.repeat(scale));
            });
        });
    }
}
