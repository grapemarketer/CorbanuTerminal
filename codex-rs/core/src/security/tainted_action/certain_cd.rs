//! D194 (#1309): a `cd` whose failure stops what follows it. After `cd x
//! && ...` the commands joined to it by `&&` (and the pipelines among them)
//! run only where the change happened; after `cd x || exit` (`exit N`), so
//! does the rest of the shell. The folders the line was in before such a
//! change are set aside, and become possible again where that stops: after
//! the `&&` list; for `|| exit`, wherever the shell running the commands may
//! differ (a group, subshell, pipeline, background job or compound command).
//!
//! Only in the action's own shell script (`bash -lc '...'`), and only when
//! it is plain: ASCII words, no quotes, backslashes, expansions (`$`,
//! patterns, `~`), groups, comments or here-documents, so both readings
//! split it as a shell does and every value is visible; when every command
//! is one of a fixed list (`COMMANDS`, option B after review 5: `cd`,
//! `exit`, `set -e`, a few harmless builtins and build programs) named by
//! its first word; and when nothing in the line may make `cd` or `exit`
//! other than the builtins, change where `cd` goes, or read a value from
//! data. `|| return` stays
//! uncertain: bash's `return` outside a function fails and the line goes
//! on.
//!
//! Where a `cd` goes is followed as before; what the line does not show
//! may still send it elsewhere or hide its failure (an exported `CDPATH`,
//! symlinks, a `cd` function from the user's startup files). The folders
//! before it no longer cover the case where that place is one of them.
use super::Folders;
use super::nested;
use super::shell::basename;
use super::unknown_folder;
use super::work;

/// What joins a command to the one before it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Joint {
    /// Anything else: a group, substitution, `&`, `;;`, several operators.
    #[default]
    Other,
    /// The start of the script.
    Start,
    /// `;` or a line break.
    Seq,
    And,
    Or,
    Pipe,
}

impl Joint {
    /// The joint after `current` (`None`: a word) and then `next`.
    pub(super) fn then(current: Option<Joint>, next: Joint) -> Joint {
        match (current, next) {
            (None, next) => next,
            (Some(Joint::Start | Joint::Seq), Joint::Seq) => Joint::Seq,
            _ => Joint::Other,
        }
    }
}

/// The operator `ch` (`;`, `&`, `|` or a line break) starts, given the
/// characters around it; `None` for the second character of `&&`/`||`.
pub(super) fn operator(previous: Option<char>, ch: char, next: Option<char>) -> Option<Joint> {
    match (previous, ch, next) {
        (Some('&'), '&', _) | (Some('|'), '|', _) => None,
        (_, '&', Some('&')) => Some(Joint::And),
        (_, '|', Some('|')) => Some(Joint::Or),
        (_, '|', Some('&')) => Some(Joint::Other),
        (_, '|', _) => Some(Joint::Pipe),
        (_, ';', Some(';')) | (Some(';'), ';', _) => Some(Joint::Other),
        (_, ';' | '\n', _) => Some(Joint::Seq),
        _ => Some(Joint::Other),
    }
}

/// Which certain changes a reading may hold.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Allowed {
    /// `cd x && ...`.
    and: bool,
    /// `cd x || exit`.
    exit: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Last {
    #[default]
    Other,
    /// A certain `cd`.
    Change,
    /// The `exit` after one.
    Exit,
}

/// Folders set aside. Each list holds at most `MAX_ASIDE`; past it, where
/// they come back, the folder is unknown.
#[derive(Clone, Debug, Default)]
pub(super) struct Aside {
    allowed: Allowed,
    /// Where the line is if a change in the current `&&` list failed.
    chain: Vec<String>,
    chain_lost: bool,
    /// Where it is if a change before `|| exit` failed.
    held: Vec<String>,
    held_lost: bool,
    last: Last,
}

const MAX_ASIDE: usize = 8;

/// Words that start or end a part of a compound command. #1409: `scan`
/// lets none of them start a command, but the word reading splits `>|`
/// and `>>|` as a pipe, so the file after one may reach `before` as a
/// command's first word (`cd x || exit; make >| fi; ...`): keep them.
const COMPOUND: &[&str] = &[
    "if", "then", "else", "elif", "fi", "do", "done", "case", "esac", "end", "{", "}",
];

/// Names that may make `cd` or `exit` other than the builtin, or run code
/// that may (besides any name holding `function` or `alias`).
const REDEFINERS: &[&str] = &[
    "trap",
    "enable",
    "disable",
    "source",
    "eval",
    "autoload",
    "zmodload",
    "emulate",
    "fpath",
    // Code run from a callback or the history (`mapfile -C .`, `fc -s`).
    "mapfile",
    "readarray",
    "fc",
    "history",
    "unhash",
    // Values read from data (`read v < f; let v` may set `CDPATH`).
    "read",
    "select",
    "vared",
    "sysread",
    // zsh options by letter (`setopt -T` is `CDABLE_VARS`).
    "setopt",
    "unsetopt",
    "shopt",
];

/// The options `set -o` may name.
const SET_OPTIONS: &[&str] = &["errexit", "nounset", "pipefail", "xtrace"];

/// D194 option B: the commands a plain script may hold for its changes to
/// be certain, each as the first word of every command. The builtins here
/// cannot make `cd` or `exit` other than the builtins or change where `cd`
/// goes (`set` only with `SET_LETTERS`/`SET_OPTIONS`); the rest are
/// programs, which run in their own process. `python`/`python3` only as
/// `-m pip`.
const COMMANDS: &[&str] = &[
    "cd", "exit", "set", "true", "echo", "[", "ls", "cat", "mkdir", "tee", "make", "cargo", "npm",
    "pnpm", "yarn", "git", "go", "python", "python3",
];

/// The letters `set` may take (zsh `set -T` is `CDABLE_VARS`).
const SET_LETTERS: &str = "eux";

impl Aside {
    /// For the action's own line (`argv`): a shell's plain script.
    pub(super) fn for_line(argv: &[String]) -> Aside {
        Aside {
            allowed: allowed(argv),
            ..Aside::default()
        }
    }

    /// For text run elsewhere (quoted text): no certain changes; what was
    /// set aside comes back at its first command.
    pub(super) fn inner(&self) -> Aside {
        Aside {
            allowed: Allowed::default(),
            ..self.clone()
        }
    }
}

fn allowed(argv: &[String]) -> Allowed {
    let [shell, flags @ .., script] = argv else {
        return Allowed::default();
    };
    let shell_ok = matches!(basename(shell), "bash" | "sh" | "zsh" | "dash");
    let flags_ok = flags.iter().any(|flag| flag.contains('c'))
        && flags.iter().all(|flag| {
            flag.len() > 1
                && flag.starts_with('-')
                && flag[1..].chars().all(|letter| "lceu".contains(letter))
        });
    // Characters both readings split as a shell does, with nothing it
    // expands: ASCII words; no quotes, backslashes, `$`, groups, comments,
    // patterns or `~`; no here-documents and no `&>` (a background job in
    // dash).
    let plain = script.chars().all(|ch| {
        matches!(ch, ' ' | '\t' | '\n')
            || (ch.is_ascii_graphic() && !"'\"\\`#(){}$,*?~^".contains(ch))
    }) && !script.contains("<<")
        && !script.contains("&>");
    if !(shell_ok && flags_ok && plain) {
        return Allowed::default();
    }
    let Some(scan) = scan(script) else {
        return Allowed::default();
    };
    let mut redefines = false;
    let mut moves = false;
    for token in script.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_')) {
        work::step();
        // `HOME[0]=x`, `declare -n r=HOME`; zsh `PWD=x`.
        moves |= matches!(token, "HOME" | "PWD");
        let token = token.to_ascii_lowercase();
        // zsh `functions[cd]=...`, `aliases[cd]=...` too.
        redefines |= token.contains("function")
            || token.contains("alias")
            || REDEFINERS.contains(&token.as_str());
    }
    // Where `cd` goes may change too (`CDPATH`, `cdable_vars`).
    let searches = moves
        || scan.words.iter().any(|word| {
            work::step();
            unknown_folder::searches(word)
        });
    let and = !redefines && !searches;
    Allowed {
        and,
        exit: and && !scan.exit_redirected && !unknown_folder::starts_loop(&scan.words),
    }
}

/// What `scan` finds in a plain script.
struct Scan {
    words: Vec<String>,
    /// An `exit` has a redirection.
    exit_redirected: bool,
}

/// Read a plain script's words (`None`: a command whose first word is not
/// one of `COMMANDS`, `set` with a redirection or with options other than
/// `SET_LETTERS` and `-o` naming one of `SET_OPTIONS`, `python` other
/// than `-m pip`, a `[` or `]` that may be a pattern, any `<` (zsh `<1-9>`
/// is a pattern) or any `!` (zsh `>!`)). Commands end at `;`, `&`, `|` and
/// line breaks (not in `>&`, `>|`); words end at spaces and redirection
/// characters.
fn scan(script: &str) -> Option<Scan> {
    if script.contains(['<', '!']) {
        return None;
    }
    let chars: Vec<char> = script.chars().collect();
    let mut segments: Vec<&[char]> = Vec::new();
    let mut from = 0;
    for (at, ch) in chars.iter().enumerate() {
        work::step();
        let redirect = at > 0 && chars[at - 1] == '>';
        if matches!(ch, ';' | '\n') || (matches!(ch, '&' | '|') && !redirect) {
            segments.push(&chars[from..at]);
            from = at + 1;
        }
    }
    segments.push(&chars[from..]);
    let mut found = Scan {
        words: Vec::new(),
        exit_redirected: false,
    };
    for segment in segments {
        work::step();
        let text: String = segment.iter().collect();
        let redirected = text.contains('>');
        let words: Vec<String> = text
            .split(|ch: char| ch.is_ascii_whitespace() || matches!(ch, '>' | '&' | '|'))
            .filter(|word| !word.is_empty())
            .map(str::to_string)
            .collect();
        let Some(name) = words.first() else {
            continue;
        };
        // The name is the first word: not a redirection, its descriptor
        // or an assignment.
        if text.trim_start().starts_with('>') || !COMMANDS.contains(&name.as_str()) {
            return None;
        }
        for word in &words {
            work::step();
            if word.contains(['[', ']']) && !matches!(word.as_str(), "[" | "]") {
                return None;
            }
        }
        match name.as_str() {
            "set" if redirected || !set_options(&words[1..]) => return None,
            "python" | "python3" if words.get(1..3) != Some(&["-m".into(), "pip".into()]) => {
                return None;
            }
            "exit" => found.exit_redirected |= redirected,
            _ => {}
        }
        found.words.extend(words);
    }
    Some(found)
}

/// `set`'s words: options of `SET_LETTERS`, a final `o` taking one of
/// `SET_OPTIONS`.
fn set_options(words: &[String]) -> bool {
    let mut option_name = false;
    words.iter().all(|word| {
        work::step();
        if std::mem::take(&mut option_name) {
            return SET_OPTIONS.contains(&word.as_str());
        }
        let letters = word.get(1..).unwrap_or_default();
        let letters = letters.strip_suffix('o').map_or(letters, |rest| {
            option_name = true;
            rest
        });
        word.starts_with(['-', '+'])
            && word.len() > 1
            && letters.chars().all(|letter| SET_LETTERS.contains(letter))
    }) && !option_name
}

/// Before each command a reading follows, in order (`joint`: `Other` for
/// one in a substitution; `words` as written; `redirected`: it has a
/// redirection its words do not hold): bring back what was set aside where
/// that stops. Returns whether the command is a certain `cd`.
pub(super) fn before(
    folders: &mut Folders,
    joint: Joint,
    words: &[String],
    redirected: bool,
) -> bool {
    let aside = &mut folders.aside;
    let last = std::mem::take(&mut aside.last);
    if last == Last::Change
        && joint == Joint::Or
        && aside.allowed.exit
        && !redirected
        && is_exit(words)
    {
        // `cd x || exit`: where a change of its `&&` list failed, the
        // shell exits.
        let chain = std::mem::take(&mut aside.chain);
        aside.held_lost |= std::mem::take(&mut aside.chain_lost);
        keep(&mut aside.held, &mut aside.held_lost, chain);
        aside.last = Last::Exit;
        return false;
    }
    // A pipe after the change runs it in a pipeline.
    let ends_chain = joint != Joint::And && (joint != Joint::Pipe || last == Last::Change);
    let ends_held = matches!(joint, Joint::Other | Joint::Start)
        || (last == Last::Exit && !matches!(joint, Joint::Seq | Joint::And))
        || words
            .first()
            .is_some_and(|word| COMPOUND.contains(&word.as_str()));
    if ends_chain {
        bring_back(folders, /*held*/ false);
    }
    if ends_held {
        bring_back(folders, /*held*/ true);
    }
    folders.aside.allowed.and
        && matches!(joint, Joint::Start | Joint::Seq | Joint::And)
        && plain_cd(words)
}

/// A certain change: `possible`, the folders the line may have been in
/// before it, are where it is if the change failed.
pub(super) fn set_aside(folders: &mut Folders, possible: Vec<String>) {
    let aside = &mut folders.aside;
    keep(&mut aside.chain, &mut aside.chain_lost, possible);
    aside.last = Last::Change;
}

/// After a reading's last command: everything comes back.
pub(super) fn finish(folders: &mut Folders) {
    folders.aside.last = Last::Other;
    bring_back(folders, /*held*/ false);
    bring_back(folders, /*held*/ true);
}

fn keep(list: &mut Vec<String>, lost: &mut bool, folders: Vec<String>) {
    for folder in folders {
        work::step();
        if !list.contains(&folder) {
            list.push(folder);
        }
        if list.len() > MAX_ASIDE {
            *lost = true;
            list.clear();
        }
    }
}

fn bring_back(folders: &mut Folders, held: bool) {
    let aside = &mut folders.aside;
    let (list, lost) = if held {
        (
            std::mem::take(&mut aside.held),
            std::mem::take(&mut aside.held_lost),
        )
    } else {
        (
            std::mem::take(&mut aside.chain),
            std::mem::take(&mut aside.chain_lost),
        )
    };
    if lost {
        folders.unchecked = true;
        folders.lost = true;
        folders.others.clear();
        return;
    }
    if folders.lost {
        return;
    }
    for folder in list {
        work::step();
        if folder != folders.cwd && !folders.others.contains(&folder) {
            folders.others.push(folder);
        }
    }
    nested::too_many(folders);
}

/// `exit` or `exit N`.
fn is_exit(words: &[String]) -> bool {
    match words {
        [exit] => exit == "exit",
        [exit, status] => {
            exit == "exit"
                && (1..=3).contains(&status.len())
                && status.bytes().all(|byte| byte.is_ascii_digit())
        }
        _ => false,
    }
}

/// `cd [-P|-L] target`, a target that names one folder as written.
fn plain_cd(words: &[String]) -> bool {
    let target = match words {
        [cd, target] if cd == "cd" => target,
        [cd, option, target] if cd == "cd" && matches!(option.as_str(), "-P" | "-L") => target,
        _ => return false,
    };
    !target.is_empty()
        && !target.starts_with(['-', '+'])
        && target.chars().all(|ch| {
            work::step();
            ch.is_alphanumeric() || "._-/+@%:".contains(ch)
        })
}
