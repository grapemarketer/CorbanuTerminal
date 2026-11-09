//! #1876: git `ext::` remotes the line does not show. #1841 asks when a git
//! command's own words name an `ext::` remote; these forms reach git without
//! the remote on the line: input a launcher appends (`xargs git fetch`), and a
//! remote held in a variable the line does not resolve (`git fetch "$r"`).
//! Stored `.git/config` / `.gitmodules` remotes are not on the line at all:
//! the classifier asks when the line itself allows `ext`, and otherwise
//! leaves them to git's default (`protocol.ext.allow=never`). In its own file:
//! `tainted_action_tests.rs` is close to the repository's blob size limit.

use super::*;
use crate::sandboxing::SandboxPermissions;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use std::path::PathBuf;

/// The work counter with tracing silenced while it counts, as in
/// `tainted_action_linear_tests.rs`.
mod work {
    pub(super) fn assert_linear(label: &str, run: impl Fn(usize)) {
        tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
            super::super::work::assert_linear(label, run)
        });
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

/// The verdict on `bash -lc <line>` in `/work`.
fn script(line: &str) -> Option<ProtectedActionKind> {
    let action = ApprovalAction::Shell {
        id: "call".into(),
        environment_id: "local".into(),
        command: ["bash", "-lc", line].map(str::to_string).to_vec(),
        cwd: PathUri::from_abs_path(
            &AbsolutePathBuf::from_absolute_path_checked(fixture("/work")).expect("absolute"),
        ),
        sandbox_permissions: SandboxPermissions::UseDefault,
        additional_permissions: None,
        justification: None,
    };
    classify_with(&action, &fixture(HOME), Some(&fixture(USER_HOME)))
}

/// Lines whose verdict is not `expected`.
fn wrong(lines: &[&str], expected: Option<ProtectedActionKind>) -> Vec<String> {
    lines
        .iter()
        .filter(|line| script(line) != expected)
        .map(|line| format!("{line} -> {:?}", script(line)))
        .collect()
}

/// Route 1: input words a launcher adds to a git subcommand that contacts a
/// remote may be the remote (`ext::…`) or a transport option
/// (`--upload-pack=…`, which runs whatever `protocol.ext.allow` says). Each
/// of these was quiet on main.
#[test]
fn issue_1876_launcher_input_to_a_transport_asks() {
    let lines = [
        "echo 'ext::sh -c id' | xargs git fetch",
        "printf 'ext::sh -c touch%% m\\n' | xargs -d '\\n' git fetch",
        "printf 'ext::sh -c id' | xargs git ls-remote",
        "cat remotes.txt | xargs -n1 git fetch",
        "xargs git fetch < remotes.txt",
        "printf -- '--upload-pack=sh' | xargs git fetch origin",
        "printf -- '--upload-pack=touch m' | xargs git ls-remote origin",
        "echo r | xargs git pull",
        "echo r | xargs git clone",
        "echo r | xargs git push",
        "echo o | xargs git remote add o",
        "echo r | xargs git submodule add",
        "echo r | xargs git archive --format=tar HEAD",
        "echo r | xargs git-fetch",
        // A subcommand git does not know may be an alias (`alias.f=fetch`).
        "xargs git f < remotes.txt",
        "echo r | xargs git subtree pull --prefix=lib",
        "parallel 'cd x && git fetch' ::: r",
    ];
    assert_eq!(
        wrong(&lines, Some(ProtectedActionKind::UnseenCode)),
        Vec::<String>::new()
    );
}

/// Route 3: the remote, or a word before it (which may be an option such as
/// `--upload-pack=…`), is a value the line does not resolve. Each of these
/// was quiet on main (unquoted, unspaced assignments already asked).
#[test]
fn issue_1876_unresolved_remote_asks() {
    let lines = [
        "r='ext::sh -c id'; git fetch \"$r\"",
        "r=\"ext::sh -c id\"; git ls-remote \"$r\"",
        "git fetch \"$R\"",
        "git pull \"$R\" main",
        "while read r; do git fetch \"$r\"; done < remotes.txt",
        "r=$(cat remotes.txt); git fetch \"$r\"",
        "git fetch `cat remotes.txt`",
        "git fetch $opts origin",
        "git fetch --depth 1 \"$r\"",
        "git clone \"$url\" dir",
        "git remote add o \"$url\"",
        "git remote set-url o \"$url\"",
        "git submodule add \"$url\"",
        "git archive --remote \"$r\" HEAD",
        "git -C sub fetch \"$r\"",
        "git-fetch \"$r\"",
        // `-u` and `-t` are flags here, not options taking the next word.
        "git push -u \"$r\"",
        "git fetch -t \"$r\"",
        "git f \"$r\"",
        // A group's last letter, or an abbreviation, takes the next word.
        "git clone -qb main \"$r\" dst",
        "git clone --bra main \"$r\" dst",
        "git clone --rem \"$r\" dst",
        // A prefix of another subcommand's valued option (`--pre`, of
        // archive's `--prefix`) takes no value here: the word is an operand.
        "git push --pre \"$r\"",
        // Codex review round 3: a valued option of ls-remote, and archive's
        // `--remote` after its tree-ish.
        "git ls-remote --sort refname \"$r\"",
        "git archive HEAD --remote \"$r\"",
        "git archive --format=tar HEAD --remote=\"$r\"",
        "git push --repo \"$r\"",
        "git clone $(true)--upload-pack=./x r d",
    ];
    assert_eq!(
        wrong(&lines, Some(ProtectedActionKind::UnseenCode)),
        Vec::<String>::new()
    );
}

/// Git's `ext::` helper run by itself runs its command, even with
/// `protocol.ext.allow=never`. Quiet on main.
#[test]
fn issue_1876_remote_ext_helper_asks() {
    let lines = [
        "printf 'connect git-upload-pack\\n' | git remote-ext x 'sh -c touch% m'",
        "git remote-ext origin \"$cmd\" < req",
        "git -c protocol.ext.allow=never remote-ext x 'sh -c id' < req",
        "/usr/lib/git-core/git-remote-ext x 'sh -c id' < req",
        "echo 'sh -c id' | xargs git remote-ext x",
    ];
    assert_eq!(
        wrong(&lines, Some(ProtectedActionKind::UnseenCode)),
        Vec::<String>::new()
    );
}

/// What stays as on main: visible remotes and values, unseen words after
/// the remote that cannot start with `-` (`HEAD:"$b"`) or follow `--`,
/// option values, subcommands that contact no remote, and launcher input to
/// other subcommands.
#[test]
fn issue_1876_visible_remotes_and_other_words_stay_quiet() {
    let lines = [
        "git fetch origin",
        "git pull origin main",
        "git push origin HEAD:\"$branch\"",
        "git push origin refs/heads/\"$branch\"",
        "git push origin origin/\"$branch\"",
        "git push origin -- \"$branch\"",
        // Quoted expansions do not split, visible starts do not glob into
        // options, and redirections are not git's words.
        "git push . HEAD:\"$b\"",
        "git fetch --depth=\"$n\" .",
        "git fetch origin 'refs/heads/*:refs/remotes/origin/*'",
        "git fetch origin refs/heads/*:refs/remotes/origin/*",
        "git fetch origin >\"$log\" 2>&1",
        "git fetch origin 2>\"$err\"",
        "git fetch --multiple origin upstream",
        "sudo -u git git fetch origin",
        "git clone https://x.example/r -- \"$dir\"",
        "git clone -b \"$b\" https://x.example/r d",
        "git fetch --depth=\"$DEPTH\" origin",
        "git clone --branch=\"$b\" https://x.example/r",
        "r=origin; git fetch \"$r\"",
        "git fetch --depth \"$n\" origin",
        "git push -o \"$opt\" origin",
        "git pull -s \"$s\" origin main",
        "git clone -b \"$b\" https://x.example/r",
        "git archive -o \"$out\" HEAD",
        "git remote -v",
        "git remote rename a \"$b\"",
        "git submodule update --init \"$path\"",
        "git log \"$range\"",
        "git checkout \"$branch\"",
        "echo x | xargs git log --oneline",
        "xargs git config --get user.name < f",
        // The word lexer may leave a quote on the subcommand (`log'`).
        "parallel 'cd x && git log' ::: a",
        // `find -execdir` without `{}` adds no words: it picks the
        // repository, whose stored remotes are route 2 (#1268 keeps it quiet).
        "find . -type d -name .git -execdir git pull \\;",
    ];
    assert_eq!(wrong(&lines, /*expected*/ None), Vec::<String>::new());
}

/// Review round 1 on #1897. Each of these asks; each was quiet on the
/// head reviewed (895ae6fa).
#[test]
fn issue_1876_review_1_lines_ask() {
    let lines = [
        // Item 1: git reads options after operands, so an unseen word
        // after the remote may be `--upload-pack=…`, `--receive-pack=…`,
        // `--config=…` or `--template=…` (archive's: `--exec=…`).
        "git fetch origin \"$v\"",
        "git pull origin \"$v\"",
        "git push origin HEAD:refs/heads/z \"$v\"",
        "git clone ../src d1 \"$v\"",
        "git archive --remote=. \"$x\" HEAD",
        "git push origin \"$branch\"",
        "git fetch origin $(cat f)",
        "git clone https://x.example/r \"$dir\"",
        "git archive --format=tar \"$rev\" \"$path\"",
        "git fetch -- \"$r\"",
        // Item 2: a wrapper's value named `git` is not the git that runs.
        "sudo -u git git fetch \"$r\"",
        "env -u git git fetch \"$r\"",
        "git log git; git fetch \"$r\"",
        // A remote named `git`: the scan the next git word starts does not
        // end this command's options (Codex review).
        "git fetch git log \"$v\"",
        "git push git HEAD:x \"$v\"",
        // Item 3: a case-insensitive filesystem finds the helper.
        "git REMOTE-EXT x ./e.sh",
        "git Remote-Ext x ./e.sh",
        "git -C sub REMOTE-EXT x ./e.sh",
        // Item 4: valued options missing from the tables.
        "git push --recurse-submodules check \"$r\"",
        "git clone --revision v1 \"$r\" d",
        "git ls-remote --exec x \"$r\"",
        // Item 5: ls-remote's hidden `--exec` runs its program.
        "git ls-remote --exec=./e.sh .",
        "git ls-remote --exec ./e.sh .",
        "git ls-remote --ex=./e.sh .",
        // An attached value still asks where it may name the remote or the
        // option is not named in full.
        "git archive --remote=\"$r\" HEAD",
        "git push --repo=\"$r\"",
        "git clone --bra=\"$b\" \"$r\"",
        "git fetch --$o=1 origin",
    ];
    assert_eq!(
        wrong(&lines, Some(ProtectedActionKind::UnseenCode)),
        Vec::<String>::new()
    );
}

/// Review round 2 on #1897. Each of these asks; each was quiet on the
/// head reviewed (e81e45f9).
#[test]
fn issue_1876_review_2_lines_ask() {
    let lines = [
        // Item 1: upper- and mixed-case git words (see the linear test).
        "git fetch GIT \"$v\"",
        "Git fetch origin \"$v\"",
        // Item 2: an unquoted expansion splits, so a visible start protects
        // only its first piece (`b='x --receive-pack=./y'`).
        "git push . HEAD:$b",
        "git push origin refs/heads/$b",
        "git fetch --depth=$n .",
        "git fetch --depth $n .",
        "git fetch -q origin $B",
        // Item 3: every operand may be a remote: `fetch --multiple`'s and
        // an alias's, after `--` too.
        "git fetch --multiple origin -- \"$r\"",
        "git fetch -m origin e\"$r\"",
        "git fetch --mult origin e\"$r\"",
        "git f o -- \"$url\"",
        "git f o e\"$url\"",
        // Item 4: a `--` that is an option's value does not end options.
        "git fetch git log --refmap -- \"$v\"",
        "git-fetch git log --refmap -- \"$v\"",
        "git remote add -f -t git log -- \"$url\"",
        // Item 5: an unquoted glob may match a file named `--upload-pack=…`.
        "git fetch origin *",
        "git push . *",
        "git fetch origin ?",
        "git clone ../src [d]*",
    ];
    assert_eq!(
        wrong(&lines, Some(ProtectedActionKind::UnseenCode)),
        Vec::<String>::new()
    );
}

/// Route 2: a remote stored in `.git/config` or `.gitmodules` is not on the
/// line, so these stay as on main: quiet, since git refuses `ext` unless
/// config allows it. A line that itself allows `ext` asks (on main too).
#[test]
fn issue_1876_stored_remotes_ask_only_when_the_line_allows_ext() {
    assert_eq!(
        wrong(
            &[
                "git fetch origin",
                "git submodule update --init",
                "git pull"
            ],
            /*expected*/ None
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        wrong(
            &[
                "git -c protocol.ext.allow=always fetch origin",
                "git -c protocol.allow=always submodule update --init",
                "GIT_ALLOW_PROTOCOL=ext git fetch origin",
            ],
            Some(ProtectedActionKind::UnseenCode)
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        script("git config --global protocol.ext.allow always"),
        Some(ProtectedActionKind::Persistence)
    );
}

/// The remote scan reads a command's words once: long lines of git words,
/// of options before the remote, or of git commands stay linear.
#[test]
fn issue_1876_remote_scan_is_linear() {
    for (label, word, line) in [
        (
            "refspecs after the remote",
            "\"$r\" ",
            "git fetch origin {}",
        ),
        (
            "options before the remote",
            "--quiet ",
            "git fetch {}\"$r\"",
        ),
        (
            "valued options before the remote",
            "--depth 1 ",
            "git fetch {}origin",
        ),
        (
            "abbreviated options before the remote",
            "--bra x ",
            "git clone {}\"$r\"",
        ),
        (
            "option groups before the remote",
            "-qvb x ",
            "git clone {}\"$r\"",
        ),
        (
            "git words in one command",
            "git fetch --depth git ",
            "{}\"$r\"",
        ),
        (
            "unseen words after the remote",
            "\"$v\" -- ",
            "git push origin HEAD:x {}",
        ),
        // Git words the outer loop finds through its own reading: upper
        // case (`GIT`) and a finder's attached command (`--exec=git`).
        ("upper-case git words", "-C GIT ", "git {}fetch \"$r\""),
        ("attached execs", "fd --exec=git ", "git fetch {}\"$r\""),
        // Review round 2 item 1: each `GIT`/`Git` starts a scan, which ends
        // at the next.
        (
            "upper-case git words as remotes",
            "GIT fetch ",
            "echo GIT; git fetch {}origin",
        ),
        ("mixed-case git words", "Git fetch ", "git fetch {}\"$v\""),
        (
            "unquoted words after the remote",
            "HEAD:$b ",
            "git push origin {}",
        ),
        ("globs after the remote", "refs/* ", "git push origin {}"),
        (
            "global options before the subcommand",
            "-C d ",
            "git {}fetch origin",
        ),
        ("launcher input", "-n1 ", "echo r | xargs {}git fetch"),
        ("git commands", "git fetch \"$r\"; ", "{}true"),
    ] {
        work::assert_linear(label, |scale| {
            let _ = script(&line.replace("{}", &word.repeat(200 * scale)));
        });
    }
}
