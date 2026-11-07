//! #1159: git by another name. A dashed helper (`git-config`, the
//! `/usr/lib/git-core/git-log` in git's exec path, `git-lfs`) runs as `git
//! config`, `git log`, `git lfs`; `git.exe` (and `git-config.exe`) is git on
//! Windows. The git checks judge them as the git line they stand for.

use super::quoted::Word;
use super::shell;
use super::work;

/// What `word` names, read as git: `Some(None)` for git itself under another
/// name (`git.exe`, `C:\Git\cmd\git`), `Some(Some(subcommand))` for a dashed
/// helper, its subcommand in lower case. `git` and `/usr/bin/git` are already
/// seen as git: `None`, like any other word.
pub(super) fn git_name(word: &str) -> Option<Option<String>> {
    let base = word
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(word)
        .to_lowercase();
    let base = base.strip_suffix(".exe").unwrap_or(&base);
    if base == "git" {
        return (!shell::basename(word).eq_ignore_ascii_case("git")).then_some(None);
    }
    base.strip_prefix("git-")
        .filter(|subcommand| !subcommand.is_empty())
        .map(|subcommand| Some(subcommand.to_string()))
}

/// #1661: whether the git at `at` runs a git of its own in another folder:
/// `submodule foreach` or `bisect run` (`git -C ~/r submodule foreach
/// git log --output=...`, `git bisect run git log --output=...`). After
/// git's global options, the words `submodule` + `foreach` or `bisect` +
/// `run`; a dashed helper (`git-submodule`) already names its subcommand.
pub(super) fn runs_wrapped_git(words: &[String], at: usize) -> bool {
    let Some(word) = words.get(at) else {
        return false;
    };
    // A dashed helper (`git-submodule`, `git-bisect`) already names the
    // subcommand it runs.
    if let Some(Some(subcommand)) = git_name(word) {
        return matches!(
            (subcommand.as_str(), words.get(at + 1).map(String::as_str)),
            ("submodule", Some("foreach")) | ("bisect", Some("run"))
        );
    }
    if !shell::basename(word).eq_ignore_ascii_case("git") && git_name(word).is_none() {
        return false;
    }
    // Git's global options, with the value of one that takes it.
    const VALUED: &[&str] = &[
        "-C",
        "-c",
        "--git-dir",
        "--work-tree",
        "--namespace",
        "--exec-path",
        "--config-env",
        "--super-prefix",
    ];
    let mut index = at + 1;
    while index < words.len() {
        work::step();
        let word = &words[index];
        if word == "--" {
            index += 1;
            break;
        }
        if word.starts_with('-') && word != "-" {
            index += 1;
            if VALUED.contains(&word.as_str()) && !word.contains('=') {
                index += 1;
            }
            continue;
        }
        break;
    }
    matches!(
        (
            words.get(index).map(String::as_str),
            words.get(index + 1).map(String::as_str)
        ),
        (Some("submodule"), Some("foreach")) | (Some("bisect"), Some("run"))
    )
}

/// D153: whether a git command (`git`, `git.exe`, after any wrapper) gives a
/// `-c` or `--config-env` setting as text holding a git name
/// (`git -c 'user.name=a git.exe b' -c core.pager=sh log`). The word lexer
/// splits that text, so the git checks would take the name for another git
/// command and start reading there, past the settings that follow: fail
/// closed. `words` are read with quotes, each kept whole.
pub(super) fn setting_holds_git_name(words: &[Word]) -> bool {
    let mut git = false;
    let mut value_next = false;
    for word in words {
        work::step();
        let text = word.text.as_str();
        let value = std::mem::take(&mut value_next)
            || (git
                && (text.starts_with("--config-env=")
                    || (text.starts_with("-c") && !text.starts_with("--"))));
        if value && holds_git_name(text) {
            return true;
        }
        git |= shell::basename(text).eq_ignore_ascii_case("git") || git_name(text) == Some(None);
        value_next = git && matches!(text, "-c" | "--config-env");
    }
    false
}

/// Whether `text` holds a word that names git or a dashed helper: as the
/// word lexer reads it (braces, `$'...'`, `$IFS`), or split at blanks and
/// shell punctuation with backslashes kept (`C:\Git\cmd\git.exe`).
fn holds_git_name(text: &str) -> bool {
    let names_git = |word: &str| {
        work::step();
        shell::basename(word).eq_ignore_ascii_case("git") || git_name(word).is_some()
    };
    let lexed = shell::simple_commands(&[text.to_string()]);
    lexed.incomplete
        || lexed.too_deep
        || lexed
            .commands
            .iter()
            .flat_map(|command| &command.words)
            .any(|word| names_git(word))
        || text
            .split(|ch: char| {
                ch.is_whitespace()
                    || matches!(
                        ch,
                        ',' | ';' | '&' | '|' | '(' | ')' | '<' | '>' | '`' | '\'' | '"'
                    )
            })
            .any(names_git)
}

#[cfg(test)]
mod tests {
    use super::git_name;
    use pretty_assertions::assert_eq;

    #[test]
    fn git_names() {
        let helper = |name: &str| Some(Some(name.to_string()));
        for (word, want) in [
            ("git-config", helper("config")),
            ("/usr/lib/git-core/git-config", helper("config")),
            ("/opt/x/GIT-Log", helper("log")),
            ("git-lfs", helper("lfs")),
            ("git-config.exe", helper("config")),
            (
                "C:\\Git\\mingw64\\libexec\\git-core\\git-log.exe",
                helper("log"),
            ),
            ("git.exe", Some(None)),
            ("C:/Program Files/Git/cmd/git.exe", Some(None)),
            ("C:\\Git\\cmd\\GIT.EXE", Some(None)),
            ("C:\\Git\\cmd\\git", Some(None)),
            ("git", None),
            ("/usr/bin/git", None),
            ("GIT", None),
            ("git-", None),
            ("legit-x", None),
            (".git-blame-ignore-revs", None),
            ("digit.exe", None),
        ] {
            assert_eq!(git_name(word), want, "{word}");
        }
    }
}
