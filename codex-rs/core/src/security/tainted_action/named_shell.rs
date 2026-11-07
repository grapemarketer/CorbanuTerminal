//! D85: whether a command line provably never reaches zsh, so the zsh-only
//! checks (glob qualifiers #1028, glob substitution #1084) need not ask for
//! it (`git commit -m "fix(tests)"` under bash). #932 phase 3
//! ([`grammar_proves_data`]) also lifts them under any system shell, zsh
//! and any `sh` included, for lines whose every word the grammar proves is
//! data.
//!
//! Fail closed: only a line the host runs under the bash, dash or sh its
//! argv names by system path (an sh that is not zsh), whose script is plain
//! commands with literal words (a real bash parse: no substitutions,
//! parameter expansions, redirections, groups, assignments or control flow),
//! each a program that never hands its words to a shell. Where the parse
//! and bash may split words differently ([`splits_differently`]) the line
//! keeps the checks; braces and glob characters, which bash still expands,
//! must stay out of words whose meaning matters. Anything else keeps the
//! checks, a shell named without a system path included.

use super::grammar;
use super::quoted;
use super::shadow;
use super::shadow::Comparison;
use codex_shell_command::bash::PlainWord;

/// Longer scripts are not parsed (they keep the checks).
const MAX_SCRIPT_BYTES: usize = 64 * 1024;
/// Shells, by system path, that are never zsh.
const NOT_ZSH: &[&str] = &["/bin/bash", "/usr/bin/bash", "/bin/dash", "/usr/bin/dash"];
/// `sh`, which counts only when it is one of those.
const SYSTEM_SH: &[&str] = &["/bin/sh", "/usr/bin/sh"];
/// Programs that only print, read or name files and never run their words,
/// whatever their words expand to (not `printf`: `-v` assigns a variable).
const DATA_ONLY: &[&str] = &[
    "echo", "cd", "pwd", "true", "false", "ls", "cat", "head", "tail", "wc", "grep", "mkdir",
    "touch",
];
/// Git subcommands that run nothing named on the line (hooks, pagers and
/// filters come from configuration, which the classifier protects), without
/// `--output` (a file the classifier does not judge as written, #1141) and
/// without words bash may expand into it: unquoted braces or glob
/// characters (`{--output=x,}`, a glob matching a file so named).
const GIT_DATA_ONLY: &[&str] = &[
    "status", "diff", "log", "show", "add", "commit", "restore", "switch", "checkout", "branch",
    "rm", "mv", "stash",
];

/// Whether `command`, run under the shell it names, never reaches zsh.
pub(super) fn avoids_zsh(command: &[String]) -> bool {
    let [shell, flag, script] = command else {
        return false;
    };
    if !matches!(flag.as_str(), "-c" | "-lc")
        || script.len() > MAX_SCRIPT_BYTES
        || splits_differently(script)
        || !not_zsh(shell)
    {
        return false;
    }
    codex_shell_command::bash::parse_shell_script_into_plain_words(script).is_some_and(|commands| {
        !commands.is_empty() && commands.iter().all(|words| data_only(words))
    })
}

/// #932 phase 3: the zsh-only checks need not ask for a line, under a
/// system bash, zsh, dash or sh, when the bash grammar proves every word of it is
/// data no shell expands (`grammar.rs`) and the line is D85's plain
/// commands of programs that never run their words. No unquoted glob
/// character, parenthesis or expansion is left for a zsh glob qualifier or
/// glob substitution to act on.
///
/// Fail closed: the grammar must read the line (no refusal), find nothing
/// zsh may read differently, and find the same commands as the quote-aware
/// reader; the line must hold no substitution, no `''` (zsh's `RC_QUOTES`
/// reads `'a''b'` as `a'b`) and no `cd` (zsh runs `chpwd` hooks).
pub(super) fn grammar_proves_data(command: &[String]) -> bool {
    /// Shells by system path (as D85, a shell named otherwise may be
    /// anything).
    const SYSTEM_SHELLS: &[&str] = &[
        "/bin/bash",
        "/usr/bin/bash",
        "/bin/zsh",
        "/usr/bin/zsh",
        "/bin/sh",
        "/usr/bin/sh",
        "/bin/dash",
        "/usr/bin/dash",
    ];
    let [shell, flag, script] = command else {
        return false;
    };
    if !matches!(flag.as_str(), "-c" | "-lc")
        || !SYSTEM_SHELLS.contains(&shell.as_str())
        || script.len() > grammar::MAX_BYTES
        || splits_differently(script)
        || script.contains("''")
    {
        return false;
    }
    let Ok(reading) = grammar::read(script) else {
        return false;
    };
    let mut budget = usize::MAX;
    let (comparison, _) = shadow::compare(script, &quoted::commands(script), &mut budget);
    !reading.zsh_unsure
        && !reading.bash_only
        && comparison == Comparison::Agree
        && !reading.commands.is_empty()
        && reading.commands.iter().all(|command| {
            !command.substituted
                && command.words.first().is_some_and(|name| name.text != "cd")
                && command.words.iter().all(|word| word.data)
        })
        && codex_shell_command::bash::parse_shell_script_into_plain_words(script).is_some_and(
            |commands| {
                commands.len() == reading.commands.len()
                    && commands.iter().all(|words| data_only(words))
            },
        )
}

/// #1261: how deep `-c` scripts of POSIX shells are followed for
/// [`windows_reads_follow`].
const MAX_FOLLOWED_DEPTH: usize = 3;
/// Programs besides [`DATA_ONLY`] that run no script and change no folder
/// or mount, in a script [`windows_reads_follow`] follows.
const FOLLOWED_TOOLS: &[&str] = &["sed", "sort", "uniq", "cut", "tr", "rg"];
/// Interpreters whose first word, a file, is a script they run.
const SCRIPT_INTERPRETERS: &[&str] = &["python", "python3", "node", "ruby", "perl"];

/// #1261: whether, on Windows, the first judgment may open the script and
/// sed program files `command` (the action's argv) runs. Its word lexer
/// splits inside quoted words and follows only POSIX `cd`, so a file named
/// in text that cmd, PowerShell or an interpreter's inline code runs may
/// not be the one that runs (`cmd /c "cd sub & bash run.sh"`). So only: a
/// POSIX shell (`bash`, `sh`, `dash`, `zsh`, by name) given `-c` (not a
/// login shell: an MSYS2 or Cygwin profile may `cd` to the home first) and
/// a script the bash grammar reads as plain commands of literal words
/// with no shell syntax in them, each a script run by file (`bash x.sh`,
/// `sh -x x.sh`, `./x`, `source x`, `python3 x.py`), a listed program
/// that never runs its words or changes a folder (no `cd`, `pushd`,
/// `popd`, `mount`), or such a shell's `-c` script that is one too.
pub(super) fn windows_reads_follow(command: &[String]) -> bool {
    follows(command, /*depth*/ 0)
}

fn follows(command: &[String], depth: usize) -> bool {
    let [shell, flag, script, ..] = command else {
        return false;
    };
    if depth > MAX_FOLLOWED_DEPTH
        // A shell named by a path may be any program (`./bash`, `/bin/bash`
        // is `\bin\bash` on the working folder's drive under Windows).
        || shell.contains(['/', '\\'])
        || !posix_shell(shell)
        // Not `-lc`: an MSYS2 or Cygwin login shell's profile may `cd` to
        // the home before the script runs.
        || flag != "-c"
        || script.len() > MAX_SCRIPT_BYTES
        || splits_differently(script)
    {
        return false;
    }
    codex_shell_command::bash::parse_shell_script_into_plain_words(script).is_some_and(|commands| {
        !commands.is_empty() && commands.iter().all(|words| followed(words, depth))
    })
}

/// A shell named by `word` (any folder, `.exe` or not) that reads POSIX
/// shell text.
fn posix_shell(word: &str) -> bool {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let name = name.strip_suffix(".exe").unwrap_or(name);
    // Not zsh: it reads `~/.zshenv` (and `/etc/zshenv`, writable under
    // MSYS2) before any `-c` script.
    matches!(name, "bash" | "sh" | "dash")
}

/// One plain command [`windows_reads_follow`] accepts: its words hold no
/// text a lexer that ignores quotes would split or expand, and it runs no
/// script in a way the first judgment may follow wrongly: no wrapper before
/// it (`env -C dir`, `sudo -D dir`, `timeout`, ...), and a shell or
/// interpreter only with a script file (no inline code, preloads or modules),
/// or a POSIX shell's `-c` script that is one too.
fn followed(words: &[PlainWord], depth: usize) -> bool {
    let texts: Vec<String> = words.iter().map(|word| word.text.clone()).collect();
    let Some(name) = texts.first() else {
        return false;
    };
    if posix_shell(name) {
        // A shell named by a path may be any program (see `follows`).
        if name.contains(['/', '\\']) {
            return false;
        }
        let short = |letter: char| {
            texts[1..].iter().any(|word| {
                word.starts_with('-') && !word.starts_with("--") && word.contains(letter)
            })
        };
        // A login shell may start in the home (see `follows`).
        if short('l') || texts.iter().any(|word| word == "--login") {
            return false;
        }
        if short('c') {
            return follows(&texts, depth + 1);
        }
    }
    let plain = texts.iter().all(|word| {
        !word.is_empty()
            && !word.chars().any(|ch| {
                ch.is_whitespace()
                    || matches!(
                        ch,
                        ';' | '&'
                            | '|'
                            | '('
                            | ')'
                            | '<'
                            | '>'
                            | '`'
                            | '$'
                            | '\\'
                            | '\''
                            | '"'
                            | '{'
                            | '}'
                            | '*'
                            | '?'
                            | '['
                            | ']'
                            | '#'
                            | '~'
                            | '='
                    )
            })
    });
    if !plain {
        return false;
    }
    let Some((index, command)) = super::indirect::command_word(&texts) else {
        return false;
    };
    if index > 0 {
        return false;
    }
    // A program run by its path (`./x`) is judged by its bare name, so
    // `./cd` or `./python3 x.py` would read as the builtin or interpreter:
    // only a path whose name is neither is followed.
    if name.contains('/') {
        return !matches!(command.as_str(), "cd" | "pushd" | "popd")
            && super::invocation::parse(&command, &texts[1..]).is_none();
    }
    match super::invocation::parse(&command, &texts[1..]) {
        // A command that runs nothing and changes no folder or mount (not `cd`: a `cd` that
        // `&&`, `||`, `&` or a pipeline skips, or `pushd -n`, would leave
        // the shell elsewhere than the classifier follows).
        None => command != "cd" && (data_only(words) || FOLLOWED_TOOLS.contains(&command.as_str())),
        Some(invocation) => {
            matches!(invocation.code, super::invocation::Code::File(_))
                && invocation.preloads.is_empty()
                && (posix_shell(&command)
                    || matches!(command.as_str(), "source" | ".")
                    || SCRIPT_INTERPRETERS.contains(&command.as_str()))
        }
    }
}

/// Text the parse may split into words other than bash's: a backslash
/// (`diff\<newline>tool` is one bash word, two parsed words), a control
/// character other than newline or tab, or a non-ASCII character before
/// `|`, a brace, a bracket or `~` (in a GBK, Big5 or Shift-JIS locale its
/// last byte may start a character that takes that one in).
fn splits_differently(script: &str) -> bool {
    let mut previous_non_ascii = false;
    script.chars().any(|ch| {
        let differs = ch == '\\'
            || (ch.is_control() && !matches!(ch, '\n' | '\t'))
            || (previous_non_ascii && matches!(ch, '|' | '{' | '}' | '[' | ']' | '~'));
        previous_non_ascii = !ch.is_ascii();
        differs
    })
}

fn not_zsh(shell: &str) -> bool {
    NOT_ZSH.contains(&shell) || (SYSTEM_SH.contains(&shell) && system_sh_not_zsh(shell))
}

/// macOS runs `/bin/sh` as the shell `/private/var/select/sh` names.
#[cfg(target_os = "macos")]
fn system_sh_not_zsh(_shell: &str) -> bool {
    std::fs::read_link("/private/var/select/sh")
        .is_ok_and(|target| matches!(target.to_str(), Some("/bin/bash" | "/bin/dash")))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn system_sh_not_zsh(shell: &str) -> bool {
    std::fs::canonicalize(shell).is_ok_and(|target| {
        target
            .to_str()
            .is_some_and(|target| NOT_ZSH.contains(&target))
    })
}

#[cfg(not(unix))]
fn system_sh_not_zsh(_shell: &str) -> bool {
    false
}

fn data_only(words: &[PlainWord]) -> bool {
    let Some((name, rest)) = words.split_first() else {
        return false;
    };
    match name.text.as_str() {
        "git" => {
            rest.first()
                .is_some_and(|subcommand| GIT_DATA_ONLY.contains(&subcommand.text.as_str()))
                && !rest.iter().any(|word| {
                    word.text.starts_with("--output")
                        || (word.unquoted && word.text.contains(['{', '*', '?', '[']))
                })
        }
        name => DATA_ONLY.contains(&name),
    }
}

#[cfg(test)]
#[path = "measure_tests.rs"]
pub(super) mod measure_tests;
