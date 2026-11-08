//! #1772, PR #1794 review 2: the two readings. The classifier's own
//! lexing reads redirections newly (`shell::simple_commands_read` with
//! `main_reading` off); its second judgment and the helper lexers
//! (`shell::simple_commands`) read exactly as main. These tests use the
//! fix's own functions, so they are declared by `tainted_action.rs`, not
//! with the behaviour tests of `tainted_action_redirect_tests.rs` (which
//! also build on main for the fix-reverted run).

use super::*;
use pretty_assertions::assert_eq;

/// One command as the lexer reads it: words, stdin files, here-strings,
/// files written, other words a redirection holds.
type Read = (Vec<String>, Vec<usize>, Vec<usize>, Vec<usize>, Vec<usize>);

/// The words and redirection positions the classifier's own lexing reads,
/// per command.
fn lexed(line: &str) -> Vec<Read> {
    lexed_read(line, /*main_reading*/ false)
}

/// [`lexed`], read as main when `main_reading`.
fn lexed_read(line: &str, main_reading: bool) -> Vec<Read> {
    commands(shell::simple_commands_read(
        &[line.to_string()],
        main_reading,
    ))
}

fn commands(lexed: shell::Lexed) -> Vec<Read> {
    lexed
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

/// Lines the new reading reads otherwise than main: words moved last, a
/// second `<` or `<<<`, a quoted or escaped file, `&>`, a substitution with
/// its stdout redirected, a reserved word after a redirection.
const READ_NEWLY: &[&str] = &[
    ">log cd sub",
    "2>/dev/null git status",
    "<a <b ./x",
    "sudo <<<a <<<b -A true",
    ">'a b' ./x",
    ">a\\ b ./x",
    "true &>a ./x",
    "$(>&2 echo true) ./x",
    "$(echo true >log) ./x",
    "cat x | while read l; do true; >/dev/null done; sh; done",
];

/// PR #1794 review 2: main's reading moves no word, keeps only the last
/// `<` and `<<<`, and reports nothing read newly; the helper lexers read
/// the same; the classifier's own reading reports each line, so the
/// action is judged both ways.
#[test]
fn issue_1772_main_reading_is_mains() {
    for line in READ_NEWLY {
        let main = shell::simple_commands_read(&[line.to_string()], /*main_reading*/ true);
        assert!(!main.read_newly, "{line}: main's reading read newly");
        assert!(
            main.commands
                .iter()
                .all(|command| command.written_at.is_empty()),
            "{line}: main's reading moved words"
        );
        assert_eq!(
            commands(main),
            commands(shell::simple_commands(&[line.to_string()])),
            "{line}: the helper lexers read otherwise than main's reading"
        );
        assert!(
            shell::simple_commands_read(&[line.to_string()], /*main_reading*/ false).read_newly,
            "{line}: the new reading is not reported"
        );
    }
    let none: Vec<usize> = Vec::new();
    assert_eq!(
        lexed_read("<a <b ./x", /*main_reading*/ true),
        vec![(
            strings(&["a", "b", "./x"]),
            vec![1],
            none.clone(),
            none.clone(),
            none.clone()
        )]
    );
    assert_eq!(
        lexed_read("sudo <<<a <<<b -A true", /*main_reading*/ true),
        vec![(
            strings(&["sudo", "a", "b", "-A", "true"]),
            none.clone(),
            vec![2],
            none.clone(),
            none
        )]
    );
    // Lines read the same both ways are judged once.
    for line in ["ls >out", "cat <a", "git status 2>/dev/null"] {
        assert!(
            !shell::simple_commands_read(&[line.to_string()], /*main_reading*/ false).read_newly,
            "{line}"
        );
    }
}

/// Main's verdict on the action whose argv is `argv`, in a workspace of
/// `files`: one judgment with main's reading.
#[cfg(unix)]
fn mains_verdict(files: &[(&str, &str)], argv: &[&str]) -> Option<ProtectedActionKind> {
    use super::tainted_action_redirect_tests::HOME;
    use super::tainted_action_redirect_tests::action_in;
    use super::tainted_action_redirect_tests::workspace;
    let root = workspace(files);
    let action = action_in(root.path(), argv);
    let homes = Homes::new(Path::new(HOME), Some(root.path()));
    judged(
        &action, homes, /*as_named*/ false, /*main_reading*/ true,
    )
    .found
}

/// The verdict of the classifier's own (new) reading alone.
fn new_verdict(files: &[(&str, &str)], argv: &[&str]) -> Option<ProtectedActionKind> {
    use super::tainted_action_redirect_tests::HOME;
    use super::tainted_action_redirect_tests::action_in;
    use super::tainted_action_redirect_tests::workspace;
    let root = workspace(files);
    let action = action_in(root.path(), argv);
    let homes = Homes::new(Path::new(HOME), Some(root.path()));
    judged(
        &action, homes, /*as_named*/ false, /*main_reading*/ false,
    )
    .found
}

/// Review lines that main's reading leaves quiet: they ask only through
/// the new reading. Review 1 item 3's trailing stdout redirection inside a
/// substitution is unspliced only when `!main_reading` (route 1), so
/// main's reading still splices `true` in as the command word.
const REVIEW_QUIET_ON_MAIN: &[&str] = &["review 1 item 3: $(echo true >log) ./x"];

/// PR #1794 review 2: the case lists' claims, on main's reading: each
/// `ASKS` line is quiet there (it asks only through the new reading), and
/// each `ASKS_ON_MAIN` and review line asks there, except
/// [`REVIEW_QUIET_ON_MAIN`], which asks on the new reading instead.
#[cfg(unix)]
#[test]
fn issue_1772_case_lists_hold_on_mains_reading() {
    use super::tainted_action_redirect_tests::ASKS;
    use super::tainted_action_redirect_tests::ASKS_ON_MAIN;
    use super::tainted_action_redirect_tests::REVIEW_2_ASKS;
    use super::tainted_action_redirect_tests::REVIEW_ASKS;
    use super::tainted_action_redirect_tests::X_EVIL;
    let asking: Vec<String> = ASKS
        .iter()
        .filter(|(_, files, line)| mains_verdict(files, &["bash", "-lc", line]).is_some())
        .map(|(item, _, line)| format!("item {item}: {line}"))
        .collect();
    assert_eq!(
        asking,
        Vec::<String>::new(),
        "ASKS lines asking on main's reading"
    );
    let mut quiet: Vec<String> = ASKS_ON_MAIN
        .iter()
        .filter(|(_, files, line)| mains_verdict(files, &["bash", "-lc", line]).is_none())
        .map(|(item, _, line)| format!("item {item}: {line}"))
        .collect();
    quiet.extend(
        REVIEW_ASKS
            .iter()
            .filter(|(_, line)| mains_verdict(X_EVIL, &["bash", "-lc", line]).is_none())
            .map(|(item, line)| format!("review 1 item {item}: {line}")),
    );
    quiet.extend(
        REVIEW_2_ASKS
            .iter()
            .filter(|(_, files, argv)| mains_verdict(files, argv).is_none())
            .map(|(item, _, argv)| format!("review 2 item {item}: {argv:?}")),
    );
    assert_eq!(
        quiet, REVIEW_QUIET_ON_MAIN,
        "review lines quiet on main's reading"
    );
    let line = "$(echo true >log) ./x";
    assert!(
        REVIEW_ASKS.contains(&("3", line)) && new_verdict(X_EVIL, &["bash", "-lc", line]).is_some(),
        "review 1 item 3 asks on the new reading"
    );
}
