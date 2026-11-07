//! #1142: settings that make git (or a pager or editor it starts) run a
//! command. git runs the value of `GIT_PAGER`, `PAGER`, `GIT_EDITOR`,
//! `GIT_SSH_COMMAND`, `GIT_EXTERNAL_DIFF`... through a shell, reads more such
//! settings from `GIT_CONFIG_*` and runs programs from `GIT_EXEC_PATH`; a
//! `-c key=value` or `--config-env` override sets any config key, many of
//! which name a command (`core.pager`, `alias.x=!cmd`, `diff.x.textconv`).
//! Setting one is code the classifier does not read.
//!
//! This over-approximates on purpose: any word that names such a variable
//! other than by reading it (`GIT_PAGER=x`, `export PAGER`, `printf -v
//! PAGER`, `read EDITOR`, `declare -n r=PAGER`, `${PAGER:=x}`) counts, in
//! any command, since a variable set in a persistent shell is used by a
//! later command; so does a builtin that names a variable the classifier
//! cannot see (`printf -v "$name"`).

use super::shell;
use super::shell::SimpleCommand;
use super::work;

/// Variables whose value git, a pager or an editor runs as a command or
/// that point git at programs or config to run.
const CODE_VARIABLES: &[&str] = &[
    "GIT_PAGER",
    "PAGER",
    "MANPAGER",
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "EDITOR",
    "VISUAL",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_EXTERNAL_DIFF",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_PROXY_COMMAND",
    "GIT_EXEC_PATH",
    "GIT_TEMPLATE_DIR",
    "GIT_ALLOW_PROTOCOL",
    "GIT_CONFIG",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    // #1159: a file git writes (the index) and the folder it writes the
    // working tree to (`GIT_WORK_TREE=~ git checkout -- .zshrc`).
    "GIT_INDEX_FILE",
    "GIT_WORK_TREE",
    // Where git finds the user's config (`$HOME/.gitconfig`).
    "HOME",
    "XDG_CONFIG_HOME",
    "LESS",
    "LESSOPEN",
    "LESSCLOSE",
    "LESSEDIT",
    "MANOPT",
    "BROWSER",
    // #1064/#1159: the shell runs a prompt variable's value (`PS4` under
    // `set -x`, `PROMPT_COMMAND`, zsh prompts under `promptsubst`), sources
    // the file `BASH_ENV` names or reads start-up files from `ZDOTDIR`.
    // (Names also set for other reasons, `PATH`, `ENV`, library paths and
    // interpreter options, are an owner decision.)
    "PS0",
    "PS1",
    "PS2",
    "PS3",
    "PS4",
    "PROMPT_COMMAND",
    "PROMPT2",
    "PROMPT3",
    "PROMPT4",
    "RPROMPT",
    "RPROMPT2",
    "RPS1",
    "RPS2",
    "SPROMPT",
    "PROMPT_EOL_MARK",
    "BASH_ENV",
    "ZDOTDIR",
    // Libraries the loader runs in every program it starts.
    "LD_PRELOAD",
    "LD_AUDIT",
    "DYLD_INSERT_LIBRARIES",
];
/// `GIT_CONFIG_KEY_<n>`/`GIT_CONFIG_VALUE_<n>` set config keys,
/// `GIT_TRACE*` names a file git writes, `GIT_TEST_*` are test hooks (some
/// run commands: `GIT_TEST_FSMONITOR`), `LESSKEY*` a file that sets less's
/// variables.
const CODE_VARIABLE_PREFIXES: &[&str] = &[
    "GIT_CONFIG_KEY_",
    "GIT_CONFIG_VALUE_",
    "GIT_TRACE",
    "GIT_TEST_",
    "LESSKEY",
];
/// #1159: code variables whose names are common words (`grep PROMPT`,
/// `GetEnvironmentVariable('PROMPT')`, `docker run -e PROMPT=x`), counted
/// only where they are set: assigned before a command or by `env`
/// (`PROMPT=x zsh -i`, `FPATH=x:$FPATH`), by PowerShell (`$env:PROMPT = 'x'`)
/// or an expansion (`${PROMPT:=x}`), or named to a builtin that sets them
/// (`export PROMPT`, `read PROMPT`, `set prompt=x`). zsh's `FPATH` is where
/// `autoload` finds the functions it runs.
const SET_ONLY_VARIABLES: &[&str] = &["PROMPT", "FPATH"];

fn is_set_only(name: &str) -> bool {
    SET_ONLY_VARIABLES.contains(&name)
}
/// Builtins whose operands name the variables they set.
const NAMING_BUILTINS: &[&str] = &[
    "declare",
    "typeset",
    "local",
    "export",
    "readonly",
    "integer",
    "float",
    "private",
    "read",
    "vared",
    "mapfile",
    "readarray",
    "getopts",
];
/// git `-c` keys that never name a command. Any other key may
/// (`core.pager`, `pager.log`, `alias.x`, `diff.x.textconv`, `include.path`).
const SAFE_GIT_KEYS: &[&str] = &[
    "user.name",
    "user.email",
    "core.quotepath",
    "core.autocrlf",
    "core.safecrlf",
    "core.abbrev",
    "init.defaultbranch",
    "pull.rebase",
    "pull.ff",
    "merge.ff",
    "rebase.autostash",
    "rebase.autosquash",
    "fetch.prune",
    "push.default",
    "push.autosetupremote",
    "diff.renames",
    "diff.noprefix",
    "diff.mnemonicprefix",
    "diff.algorithm",
    "status.showuntrackedfiles",
];
const SAFE_GIT_KEY_PREFIXES: &[&str] = &["color.", "advice."];
/// git options before the subcommand that take the next word as their value.
pub(super) const GIT_VALUE_OPTIONS: &[&str] = &[
    "-C",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--super-prefix",
    "--list-cmds",
    "--attr-source",
    "--shallow-file",
];

fn is_code_variable(name: &str) -> bool {
    CODE_VARIABLES.contains(&name)
        || CODE_VARIABLE_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

fn is_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// Whether `word` names a code variable other than by reading it.
fn names_code_variable(word: &str) -> bool {
    // `${ENV=x}`, `${ENV:=x}` (the word lexer drops the braces).
    if word.match_indices('$').any(|(at, _)| {
        work::step();
        let rest = &word[at + 1..];
        let rest = rest.strip_prefix('{').unwrap_or(rest);
        let end = rest
            .find(|ch: char| !is_name_char(ch))
            .unwrap_or(rest.len());
        is_set_only(&rest[..end]) && rest[end..].trim_start_matches(':').starts_with('=')
    }) {
        return true;
    }
    // PowerShell's environment, in any case (`$env:pager = 'x'`).
    let powershell = ["$env:", "env:"].iter().find_map(|prefix| {
        word.get(..prefix.len())
            .filter(|head| head.eq_ignore_ascii_case(prefix))
            .map(|_| &word[prefix.len()..])
    });
    if let Some(rest) = powershell {
        let name_len = rest
            .find(|ch: char| !is_name_char(ch))
            .unwrap_or(rest.len());
        if is_code_variable(&rest[..name_len].to_ascii_uppercase()) {
            return true;
        }
    }
    // An attached option value (`printf -vPAGER`, `read -raEDITOR`).
    // Each suffix's leading name (`env -SPAGER=x`, `printf -vHOME[0]`);
    // where each run of name characters ends is found once.
    if let Some(cluster) = word.strip_prefix('-')
        && !cluster.starts_with('-')
    {
        let mut run_end = cluster.len();
        for (at, ch) in cluster.char_indices().rev() {
            work::step();
            if !is_name_char(ch) {
                run_end = at;
                continue;
            }
            let before = &cluster[..at];
            let read = before.ends_with('$') || before.ends_with("${");
            if !read && is_code_variable(&cluster[at..run_end]) {
                return true;
            }
        }
    }
    // The name as a token anywhere, unless it is read (`$PAGER`,
    // `${PAGER}`): `export PAGER`, `r=PAGER`, `${PAGER:=x}`.
    let mut start = None;
    for (at, ch) in word
        .char_indices()
        .chain(std::iter::once((word.len(), ' ')))
    {
        work::step();
        if is_name_char(ch) {
            start.get_or_insert(at);
            continue;
        }
        let Some(from) = start.take() else {
            continue;
        };
        if !is_code_variable(&word[from..at]) {
            continue;
        }
        let before = &word[..from];
        let read = before.ends_with('$') || (before.ends_with("${") && ch == '}');
        if !read {
            return true;
        }
    }
    false
}

/// Whether a simple command's words (as written and, in `words`, with
/// known variables substituted) set a code variable, or name a variable the
/// classifier cannot see in a builtin that sets it. `command` is the command
/// word's index and lowercase name.
pub(super) fn sets_code_variable(
    simple: &SimpleCommand,
    words: &[String],
    command: Option<&(usize, String)>,
) -> bool {
    let raw = &simple.words;
    if raw
        .iter()
        .chain(words)
        .any(|word| names_code_variable(word))
    {
        return true;
    }
    // A name the classifier cannot see: an expansion (judged as written
    // too, since a known value may be cut short: `a=(…)`, `export o='a b'`)
    // or, where the shell globs the word, a pattern (`env PAG?R=*` names a
    // file `PAGER=x`).
    let expands = |word: &str| word.contains('$') || word.contains(shell::UNSEEN_OUTPUT);
    let globs = |word: &str| word.contains(['*', '?', '[']);
    let name_of = |word: &str| -> String {
        word.split_once('=')
            .map_or(word, |(name, _)| name)
            .to_string()
    };
    // An assignment before the command (`"$n=x" git`, `env "$n=x" git`)
    // whose name the classifier cannot see. Leading assignments (`a[i]=1
    // git`) are not globbed; a wrapper's operands (`env`) are.
    let before = command.map_or(words.len(), |(index, _)| *index);
    let mut leading = true;
    for at in 0..before {
        work::step();
        leading &= words[at]
            .split_once('=')
            .is_some_and(|(name, _)| assignment_form(name));
        for word in [words.get(at), raw.get(at)].into_iter().flatten() {
            work::step();
            if word.starts_with('-') || !word.contains('=') {
                continue;
            }
            let name = name_of(word);
            if expands(&name)
                || (!leading && globs(word))
                || is_set_only(name.split(['+', '[']).next().unwrap_or(&name))
            {
                return true;
            }
        }
    }
    // PowerShell sets a set-only variable (`$env:PROMPT = 'x'`, `$env:Fpath+=x`).
    for (at, word) in raw.iter().enumerate() {
        work::step();
        let lower = word.to_ascii_lowercase();
        let Some(rest) = ["$env:", "${env:"]
            .iter()
            .find_map(|prefix| lower.strip_prefix(prefix))
        else {
            continue;
        };
        let end = rest
            .find(|ch: char| !is_name_char(ch))
            .unwrap_or(rest.len());
        let assigned = |text: &str| {
            let text = text.trim_start_matches('}').trim_start();
            text.starts_with('=') || text.starts_with("+=") || text.starts_with("-=")
        };
        if is_set_only(&rest[..end].to_ascii_uppercase())
            && (assigned(&rest[end..]) || raw.get(at + 1).is_some_and(|next| assigned(next)))
        {
            return true;
        }
    }
    let Some((index, name)) = command else {
        return false;
    };
    let name = name.as_str();
    let start = index + 1;
    // Windows names are in any case: `set pager=x` (cmd), `setx pager x`.
    if matches!(name, "set" | "setx") {
        return words[start..].iter().any(|word| names_in_any_case(word));
    }
    // A nameref's target (`declare -n r="$x"`) is a name too.
    let nameref = matches!(name, "declare" | "typeset" | "local")
        && words[start..]
            .iter()
            .any(|word| word.starts_with('-') && !word.starts_with("--") && word.contains('n'));
    let redirect = |at: usize| {
        simple.stdin_from == Some(at)
            || simple.here_string == Some(at)
            || simple.writes_to.contains(&at)
    };
    // A name the classifier cannot see, or one in any case when it came
    // from a variable: a value given the uppercase attribute (`declare -u
    // n=pager; printf -v "$n"`) names `PAGER`.
    // Declaration builtins do not glob their assignments (`declare
    // a[0]=1`); other operands are globbed (`read PAGE[R]` with a file
    // `PAGER`).
    let declares = matches!(
        name,
        "declare" | "typeset" | "local" | "export" | "readonly" | "integer" | "float" | "private"
    );
    let judged = |at: usize, name: &str| {
        let raw_name = raw
            .get(at)
            .map(|word| if nameref { word.clone() } else { name_of(word) });
        let globbed = globs(name)
            && !(declares
                && words
                    .get(at)
                    .is_some_and(|word| word.contains('=') && assignment_form(name)));
        !redirect(at)
            && (expands(name)
                || globbed
                || raw_name.is_some_and(|raw_name| expands(&raw_name))
                || (raw.get(at) != words.get(at) && names_in_any_case(name)))
    };
    // Options whose value is not a name (`read -p PROMPT`), and the one
    // whose value is (`read -a NAME`).
    let (value_flags, name_flag) = match name {
        "printf" | "print" => ("", Some('v')),
        "read" => ("dinNptu", Some('a')),
        "mapfile" | "readarray" => ("dnOsuCc", None),
        "vared" => ("prMmit", None),
        _ if NAMING_BUILTINS.contains(&name) => ("", None),
        _ => return false,
    };
    let printf = name_flag == Some('v');
    let mut skip = false;
    let mut name_next = false;
    // `getopts OPTSTRING NAME`: the first operand is not a name.
    let mut optstring = name == "getopts";
    for (at, word) in words.iter().enumerate().skip(start) {
        work::step();
        if std::mem::take(&mut skip) {
            continue;
        }
        if std::mem::take(&mut name_next) {
            if judged(at, word) || is_set_only(word) {
                return true;
            }
            continue;
        }
        if let Some(cluster) = word.strip_prefix('-')
            && !cluster.is_empty()
        {
            let flag = cluster
                .char_indices()
                .find(|(_, flag)| value_flags.contains(*flag) || Some(*flag) == name_flag);
            if let Some((flag_at, flag)) = flag {
                let attached = &cluster[flag_at + flag.len_utf8()..];
                if Some(flag) == name_flag && !attached.is_empty() {
                    if judged(at, attached) {
                        return true;
                    }
                } else if Some(flag) == name_flag {
                    name_next = true;
                } else {
                    skip = attached.is_empty();
                }
            }
            continue;
        }
        if printf || std::mem::take(&mut optstring) {
            continue;
        }
        let name_part = if nameref {
            word.as_str()
        } else {
            word.split_once('=').map_or(word.as_str(), |(name, _)| name)
        };
        if judged(at, name_part) || name_part.split('=').any(is_set_only) {
            return true;
        }
    }
    false
}

/// Whether `name` is a variable name, or one with a subscript
/// (`a[i]`, `a[$i]`), as an assignment names it.
fn assignment_form(name: &str) -> bool {
    let name = name.strip_suffix('+').unwrap_or(name);
    let base = match name.split_once('[') {
        Some((base, subscript)) if subscript.ends_with(']') => base,
        Some(_) => return false,
        None => name,
    };
    let mut chars = base.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(is_name_char)
}

/// Whether a name operand, or a piece of it between `=` (a nameref's
/// target), is a code variable in any case.
fn names_in_any_case(operand: &str) -> bool {
    operand.split('=').any(|piece| {
        let upper = piece.to_ascii_uppercase();
        is_code_variable(&upper) || is_set_only(&upper)
    })
}

/// Whether a lowercase Git key is known never to name a command.
/// Shared by command-line overrides and persistent local configuration.
pub(super) fn safe_git_key(key: &str) -> bool {
    SAFE_GIT_KEYS.contains(&key)
        || SAFE_GIT_KEY_PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix))
}

/// Whether git's options before its subcommand (`args`, as written) set a
/// config key that may name a command (`-c core.pager=x`, `--config-env`)
/// or point git at other programs (`--exec-path=dir`), or may do so through
/// a word the classifier cannot see (`git $o x` with `o='-c alias.x=!sh'`).
///
/// Where the subcommand starts is certain only when it is the first word:
/// the lexer splits quoted text at blanks and commas (`--namespace 'a b'`,
/// `-c 'user.name=a b'`), so after any option a later word may still be
/// one of git's. Those lines are read to the end: a setting with a value
/// (`-c k=v`; not `commit -c HEAD`) or a word holding an unseen value
/// counts. Stops at the next `git` that an option follows (#1102: its own
/// scan reads on from there).
///
/// D153: a git name the lexer may have split out of quoted text (`git -c
/// 'user.name=a git.exe b' -c core.pager=sh log`) is followed by more of
/// that text, not an option, and a scan from it would take that text for
/// the subcommand. So the scan reads on past a git name that a word other
/// than an option follows. The scan from such a name ends at its first
/// word, and the others end at the next git name an option follows, so
/// each word is still read by at most two scans.
///
/// This is the scan for a word that names git another way (`git.exe`): it
/// also stops at such names. The scan for a `git` word is
/// [`git_runs_code_after_git`].
pub(super) fn git_runs_code(args: &[String], raw: &[String]) -> bool {
    git_runs_code_until(args, raw, /*other_git_names_stop*/ true)
}

/// [`git_runs_code`] for a `git` word: like the scan before #1159, it reads
/// on past `git.exe` and other names for git (D153), and stops only at a
/// `git`.
pub(super) fn git_runs_code_after_git(args: &[String], raw: &[String]) -> bool {
    git_runs_code_until(args, raw, /*other_git_names_stop*/ false)
}

fn git_runs_code_until(args: &[String], raw: &[String], other_git_names_stop: bool) -> bool {
    let unsafe_key = |setting: &str| {
        let key = setting
            .split_once('=')
            .map_or(setting, |(key, _)| key)
            .to_ascii_lowercase();
        !safe_git_key(&key)
    };
    let unseen_text = |word: &str| word.contains('$') || word.contains(shell::UNSEEN_OUTPUT);
    // An expansion, as written too (a known value may be cut short: `a=(…)`,
    // `export o='a b'`), or a glob (`-[c]` may name a file `-c`).
    let unseen = |at: usize| {
        args.get(at)
            .is_some_and(|word| unseen_text(word) || word.contains(['*', '?', '[']))
            || raw.get(at).is_some_and(|word| unseen_text(word))
    };
    // #1159: `git.exe` too (a dashed helper takes no git options, so it
    // does not end them). #1515: and zsh's `=git`, whose own scan reads on
    // from there, so a line of many stays linear.
    let is_git = |word: &str| {
        shell::basename(word).eq_ignore_ascii_case("git")
            || super::indirect::equals_git(word)
            || (other_git_names_stop && super::git_helpers::git_name(word) == Some(None))
    };
    // Past the first word that is not an option: only settings with a
    // value, and unseen words, count.
    let mut past_subcommand = false;
    let mut index = 0;
    while let Some(word) = args.get(index) {
        work::step();
        // #1159: an option word ending in a git name (`-ccore.pager=./git`,
        // `--exec-path=./git.exe`) is judged before the scan stops there.
        let option = word.starts_with('-');
        // D153: a git name ends the scan only when an option follows it.
        let ends_scan = |at: usize| {
            args.get(at).is_some_and(|word| is_git(word))
                && args.get(at + 1).is_none_or(|after| after.starts_with('-'))
        };
        if !option && ends_scan(index) {
            return false;
        }
        let next = args.get(index + 1).map(String::as_str);
        let setting = |value: &str, at: usize| {
            unsafe_key(value) && (!past_subcommand || value.contains('=') || unseen(at))
        };
        if word == "-c" || word == "--config-env" {
            match next {
                None => return !past_subcommand,
                // #1159: the setting first (`-c core.pager=./git`).
                Some(value) if setting(value, index + 1) => return true,
                Some(_) if ends_scan(index + 1) => return false,
                Some(_) => {}
            }
            index += 2;
            continue;
        }
        if let Some(value) = word.strip_prefix("--config-env=") {
            if unsafe_key(value) {
                return true;
            }
        } else if word.starts_with("--exec-path=")
            || unseen(index)
            || (word.starts_with("-c") && !word.starts_with("--") && setting(&word[2..], index))
        {
            return true;
        } else if !word.starts_with('-') && !past_subcommand {
            // The subcommand: its own options follow (`commit -c HEAD`).
            if index == 0 {
                return false;
            }
            past_subcommand = true;
        }
        if option && ends_scan(index) {
            return false;
        }
        if !past_subcommand
            && GIT_VALUE_OPTIONS.contains(&word.as_str())
            && let Some(value) = next
        {
            // A value that is a `git` is checked on its own (#1102); one
            // that looks like an option is read as one (the lexer drops
            // empty words: `-C '' -c k=v`); an unseen one may be options.
            if ends_scan(index + 1) {
                return false;
            }
            if unseen(index + 1) {
                return true;
            }
            if !value.starts_with('-') {
                index += 1;
            }
        }
        index += 1;
    }
    false
}

/// #932 measuring: which reading of `sets_code_variable` holds, as a short
/// tag (the variable, the form that set it, the class of its value and the
/// command), for the local measuring harness. Never holds a line's text
/// beyond a command name and a value from a fixed list.
#[cfg(test)]
pub(super) fn sets_code_variable_why(
    simple: &SimpleCommand,
    words: &[String],
    command: Option<&(usize, String)>,
    foreign: bool,
) -> String {
    let (index, cmd) = command.map_or((words.len(), "-"), |(index, name)| (*index, name.as_str()));
    let cmd = short_name(cmd);
    let raw = &simple.words;
    let found = raw
        .iter()
        .enumerate()
        .map(|(at, word)| (at, word, "raw"))
        .chain(
            words
                .iter()
                .enumerate()
                .map(|(at, word)| (at, word, "subst")),
        )
        .find(|(_, word, _)| names_code_variable(word));
    let Some((at, word, view)) = found else {
        let why = unseen_name_why(simple, words, index, &cmd);
        return format!("codevar sub=unseen {why} cmd={cmd} foreign={foreign}");
    };
    let pos = match at.cmp(&index) {
        std::cmp::Ordering::Less => "leading",
        std::cmp::Ordering::Equal => "command",
        std::cmp::Ordering::Greater => "operand",
    };
    let (sub, name, value) = names_code_variable_how(word);
    format!(
        "codevar sub={sub} name={name} value={value} pos={pos} view={view} cmd={cmd} foreign={foreign}"
    )
}

/// #932 measuring: which reading of a name `sets_code_variable` cannot
/// see (or of a set-only variable) holds: the branch and the shape of the
/// name (`$1`, `$x`, `$(…)`), approximately.
#[cfg(test)]
fn unseen_name_why(simple: &SimpleCommand, words: &[String], index: usize, cmd: &str) -> String {
    let raw = &simple.words;
    let shape = |name: &str| -> &'static str {
        if name.contains(shell::UNSEEN_OUTPUT) {
            "output"
        } else if let Some(rest) = name.split_once('$').map(|(_, rest)| rest) {
            let rest = rest.trim_start_matches('{');
            if rest.starts_with(|ch: char| ch.is_ascii_digit()) {
                "positional"
            } else if rest.starts_with(['@', '*', '#', '?', '!', '$', '-']) {
                "special"
            } else if rest.starts_with('(') {
                "substitution"
            } else {
                "variable"
            }
        } else if name.contains(['*', '?', '[']) {
            "pattern"
        } else {
            "literal"
        }
    };
    let name_of = |word: &str| {
        word.split_once('=')
            .map_or(word, |(name, _)| name)
            .to_string()
    };
    let mut leading = true;
    for at in 0..index.min(words.len()) {
        work::step();
        leading &= words[at]
            .split_once('=')
            .is_some_and(|(name, _)| assignment_form(name));
        for word in [words.get(at), raw.get(at)].into_iter().flatten() {
            work::step();
            if word.starts_with('-') || !word.contains('=') {
                continue;
            }
            let name = name_of(word);
            let base = name.split(['+', '[']).next().unwrap_or(&name).to_string();
            if is_set_only(&base) {
                return format!("why=set_only:{base} leading={leading}");
            }
            if name.contains('$') || name.contains(shell::UNSEEN_OUTPUT) {
                return format!(
                    "why=before_command shape={} leading={leading}",
                    shape(&name)
                );
            }
            if !leading && word.contains(['*', '?', '[']) {
                return "why=before_command shape=pattern leading=false".to_string();
            }
        }
    }
    if raw.iter().any(|word| {
        let lower = word.to_ascii_lowercase();
        lower.starts_with("$env:") || lower.starts_with("${env:")
    }) {
        return "why=powershell".to_string();
    }
    if matches!(cmd, "set" | "setx") {
        return "why=windows_set".to_string();
    }
    let operand = words.iter().skip(index + 1).find_map(|word| {
        let name = name_of(word);
        let base = name.split(['+', '[']).next().unwrap_or(&name).to_string();
        if is_set_only(&base) {
            Some(format!("set_only:{base}"))
        } else if name.contains('$')
            || name.contains(shell::UNSEEN_OUTPUT)
            || name.contains(['*', '?', '['])
        {
            Some(format!("shape={}", shape(&name)))
        } else {
            None
        }
    });
    format!(
        "why=builtin_operand {}",
        operand.unwrap_or_else(|| "any_case_or_other".to_string())
    )
}

/// The first 24 characters of a command's basename.
#[cfg(test)]
fn short_name(name: &str) -> String {
    shell::basename(name).chars().take(24).collect()
}

/// #932 measuring: how `names_code_variable` matched `word`: the branch,
/// the variable and, for an assignment, the class of its value.
#[cfg(test)]
fn names_code_variable_how(word: &str) -> (&'static str, String, String) {
    /// Values a pager or editor variable is commonly set to that run one
    /// known program with no shell text of their own.
    const PLAIN: &[&str] = &["", "cat", "less", "more", "true", ":", "head", "tail", "-"];
    let token_at = |word: &str| -> Option<(usize, usize)> {
        let mut start = None;
        for (at, ch) in word
            .char_indices()
            .chain(std::iter::once((word.len(), ' ')))
        {
            work::step();
            if is_name_char(ch) {
                start.get_or_insert(at);
                continue;
            }
            if let Some(from) = start.take()
                && is_code_variable(&word[from..at])
            {
                let before = &word[..from];
                if !(before.ends_with('$') || (before.ends_with("${") && ch == '}')) {
                    return Some((from, at));
                }
            }
        }
        None
    };
    if word.match_indices('$').any(|(at, _)| {
        let rest = &word[at + 1..];
        let rest = rest.strip_prefix('{').unwrap_or(rest);
        let end = rest
            .find(|ch: char| !is_name_char(ch))
            .unwrap_or(rest.len());
        is_set_only(&rest[..end]) && rest[end..].trim_start_matches(':').starts_with('=')
    }) {
        return ("default_expansion", "set_only".into(), "-".into());
    }
    let lower = word.to_ascii_lowercase();
    if lower.starts_with("$env:") || lower.starts_with("env:") {
        return ("powershell", "-".into(), "-".into());
    }
    if word.starts_with('-') && !word.starts_with("--") && token_at(word).is_none() {
        return ("attached_option", "-".into(), "-".into());
    }
    let Some((from, to)) = token_at(word) else {
        return ("other", "-".into(), "-".into());
    };
    let name = word[from..to].to_string();
    let Some(value) = word[to..].strip_prefix('=') else {
        let form = if from == 0 && to == word.len() {
            "bare_name"
        } else {
            "mention"
        };
        return (form, name, "-".into());
    };
    let form = if from == 0 {
        "assignment"
    } else {
        "embedded_assignment"
    };
    let unquoted: String = value
        .chars()
        .filter(|ch| !matches!(ch, '\'' | '"'))
        .collect();
    let value = if PLAIN.contains(&unquoted.trim()) {
        format!("plain:{}", unquoted.trim())
    } else if unquoted.contains('$') || unquoted.contains(shell::UNSEEN_OUTPUT) {
        "expansion".into()
    } else if unquoted.split_whitespace().next().is_some_and(|program| {
        ["less", "cat", "more", "delta", "bat"].contains(&shell::basename(program))
    }) {
        "plain_with_options".into()
    } else {
        "other_literal".into()
    };
    (form, name, value)
}

#[cfg(test)]
mod tests {
    use super::SimpleCommand;
    use super::git_runs_code;
    use super::sets_code_variable;

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_string()).collect()
    }

    /// `raw` as written, `words` with variables substituted.
    fn sets_from(raw: &[&str], words: &[&str]) -> bool {
        let simple = SimpleCommand {
            words: strings(raw),
            ..SimpleCommand::default()
        };
        let words = strings(words);
        let command = super::super::indirect::command_word(&words);
        sets_code_variable(&simple, &words, command.as_ref())
    }

    fn sets(words: &[&str]) -> bool {
        sets_from(words, words)
    }

    #[test]
    fn code_variable_settings_are_found() {
        for words in [
            &["GIT_PAGER=touch x", "git", "log"][..],
            &["env", "PAGER=x", "git", "log"],
            &["export", "GIT_EDITOR=x"],
            &["export", "PAGER"],
            &["printf", "-v", "PAGER", "%s", "x"],
            &["printf", "-vGIT_PAGER", "%s", "x"],
            &["read", "-r", "EDITOR"],
            &["read", "-raEDITOR"],
            &["declare", "-n", "r=PAGER"],
            &[":", "${PAGER:=x}"],
            &["GIT_CONFIG_KEY_0=core.pager"],
            &["PAGER+=x"],
            &["PAGER[0]=x"],
            &["$env:git_pager='x'"],
            &["set", "pager=x"],
            &["for", "PAGER", "in", "x"],
            &["printf", "-v", "$name", "%s", "x"],
            &["declare", "$name=x"],
            &["declare", "-n", "r=$name"],
            &["read", "$name"],
            &["GIT_TRACE=/tmp/t", "git", "status"],
            &["HOME=/tmp/h", "git", "log"],
            &["LESSKEYIN=/tmp/k", "less", "f"],
            &["GIT_TEST_FSMONITOR=x", "git", "status"],
            &["env", "$n=x", "git", "diff"],
            &["\u{1}unseen-output\u{1}=x", "git", "diff"],
        ] {
            assert!(sets(words), "{words:?}");
        }
        for words in [
            &["git", "log", "--oneline"][..],
            &["echo", "$PAGER"],
            &["echo", "${GIT_PAGER}"],
            &["printf", "%s", "$x"],
            &["declare", "x=$y"],
            &["export", "PAGERS=x"],
            &["declare", "-u", "name=value"],
            &["cd", "$HOME/src"],
            &["MY_EDITOR=x"],
            &["X=$(date)", "git", "log"],
            &["sudo", "-u", "$user", "git", "log"],
            &["git", "commit", "-m", "fix editor crash"],
        ] {
            assert!(!sets(words), "{words:?}");
        }
    }

    #[test]
    fn uppercased_names_from_variables_are_found() {
        assert!(sets_from(
            &["printf", "-v", "$n", "%s", "x"],
            &["printf", "-v", "pager", "%s", "x"]
        ));
        assert!(sets_from(
            &["declare", "-n", "r=$t"],
            &["declare", "-n", "r=git_pager"]
        ));
        assert!(sets_from(&["read", "-a", "$n"], &["read", "-a", "editor"]));
        // A literal lowercase name is another variable.
        assert!(!sets(&["local", "home=x", "editor"]));
        assert!(!sets(&["printf", "-v", "pager", "%s", "x"]));
    }

    #[test]
    fn options_that_are_not_names_are_skipped() {
        for words in [
            &["read", "-p", "$prompt", "x"][..],
            &["read", "-rt$timeout", "x"],
            &["mapfile", "-u", "$fd", "lines"],
            &["getopts", "$optstring", "opt"],
            &["printf", "-v", "x", "%s", "$y"],
        ] {
            assert!(!sets(words), "{words:?}");
        }
        for words in [
            &["read", "-p", "x", "$name"][..],
            &["read", "-ra$name"],
            &["getopts", "ab", "$name"],
            &["setx", "pager", "x"],
        ] {
            assert!(sets(words), "{words:?}");
        }
    }

    #[test]
    fn git_overrides_that_may_run_code_are_found() {
        for args in [
            &["-c", "core.pager=touch x", "log"][..],
            &["-c", "alias.x=!sh", "x"],
            &["-c", "Core.Pager=x", "log"],
            &["-C", "dir", "-c", "pager.log=x", "log"],
            &["--config-env=core.editor=E", "commit"],
            &["--config-env", "diff.x.textconv=T", "diff"],
            &["--exec-path=/tmp/x", "log"],
            &["-c"],
            &["-C", "-c", "core.fsmonitor=x", "status"],
            &["--namespace", "-c", "alias.x=!sh", "x"],
            // Review round 3: a split value, a missing value option, unseen
            // words before the subcommand.
            &["--namespace", "a", "b", "-c", "alias.x=!sh", "x"],
            &["-c", "user.name=a", "b", "$o", "x"],
            &["--shallow-file", "x", "-c", "alias.y=!sh", "y"],
            &["$o", "x"],
            &["-p", "$o", "x"],
            &["-C", "$dir", "status"],
            &["-C", "d", "log", "\u{1}unseen-output\u{1}"],
            // Review round 4: globs.
            &["-[c]", "alias.x=!sh", "x"],
            &["[-]c", "alias.x=!sh", "x"],
            &["--exec-pat[h]=d", "x"],
        ] {
            assert!(git_runs_code(&strings(args), &[]), "{args:?}");
        }
        for args in [
            &["log", "--oneline"][..],
            &["-C", "dir", "status"],
            &["-c", "user.name=x", "-c", "color.ui=always", "commit"],
            &["commit", "-c", "HEAD"],
            &["--exec-path"],
            // A `git` value or word is checked on its own.
            &["-C", "git", "-c", "core.pager=x", "log"],
            &["-C", "GIT", "-c", "core.pager=x", "log"],
            &["commit", "-m", "$msg"],
            &["log", "-c", "alias.x=!sh"],
            &["-C", "d", "commit", "-c", "HEAD"],
            &["-C", "d", "log", "-c"],
        ] {
            assert!(!git_runs_code(&strings(args), &[]), "{args:?}");
        }
    }
}
