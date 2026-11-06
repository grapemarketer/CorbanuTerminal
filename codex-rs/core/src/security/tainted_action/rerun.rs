//! #1064: besides `eval` and `trap`, commands that run text as code, now or
//! later: zsh `print -P` (with `promptsubst` the prompt expansion runs the
//! substitutions in its text), builtins that keep a command or load code by
//! a name (`fc -e`, `bind -x`, `complete -C`, `zle -N`, `autoload`,
//! `enable -f`), terminal multiplexers that start or type a command (`tmux
//! new-session`, `tmux send-keys`), `parallel` running its arguments, and
//! options whose value is a command: `find -exec`, `split --filter`, git's
//! `rebase -x`, `submodule foreach`, `bisect run`, `filter-branch
//! --tree-filter`, `difftool -x`. An operand holding a value the classifier
//! cannot see is code it cannot read. #1405: `mapfile -C` runs a callback
//! on each line it reads.
//!
//! #1159: git options that make git run a program the classifier does not
//! read whatever their value: `--upload-pack`/`--receive-pack`, `--exec` (of
//! `push`, `archive`, `send-pack`, `fetch-pack`), `clone -u`, hooks copied by `clone`/`init --template`, and config set on
//! the new repository by `clone -c`/`--config`.

use super::git_helpers;
use super::indirect;
use super::quoted;
use super::shell::SimpleCommand;
use super::shell::UNSEEN_OUTPUT;
use super::shell::basename;
use super::work;

/// Commands that run, or keep as code, their operands. Without a command
/// before `:::`, `parallel` runs its arguments; which of its words are
/// option values is not known, so any of them counts.
const RUNS_OPERANDS: &[&str] = &[
    "fc", "bind", "complete", "compgen", "zle", "autoload", "zmodload", "enable", "tmux", "screen",
    "parallel",
];

/// Commands whose words after one of these words are a command it runs:
/// (command, word that must also be on the line or "", words starting the
/// command). For git, as its option parser reads them, a short option also
/// starts it in a group or with its value attached (`-ix`, `-x"$v"`), and a
/// long one by any prefix from four characters, its value attached or not
/// (`--exe="$v"`).
const COMMAND_OPTIONS: &[(&str, &str, &[&str])] = &[
    ("find", "", &["-exec", "-execdir", "-ok", "-okdir"]),
    ("git", "rebase", &["-x", "--exec"]),
    ("git", "submodule", &["foreach"]),
    ("git", "bisect", &["run"]),
    ("git", "difftool", &["-x", "--extcmd"]),
    (
        "git",
        "filter-branch",
        &[
            "--env-filter",
            "--tree-filter",
            "--index-filter",
            "--parent-filter",
            "--msg-filter",
            "--commit-filter",
            "--tag-name-filter",
        ],
    ),
];

/// #1449: how a runner runs an option's value: as a shell line, as a
/// program given a file it names, or as a program given data on its input
/// (a compressor, `tar --to-command`), which a shell runs as code.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Runs {
    Line,
    Program,
    Filter,
}

/// #1449: commands with options whose value is a command they run (`su -c
/// CMD`, `su -s SHELL`, `rsync -e CMD`, `rg --pre CMD`, `rg --hostname-bin
/// CMD` (#1573), `tar -I CMD`, `tar --to-command=CMD`, `sort
/// --compress-program=CMD`): their names, then
/// each option, the shortest prefix of it the program takes (a long one;
/// getopt takes any unique one) and how it runs the value. A short option
/// may be in a group (`su -lc`, `rsync -ave`), its value the rest of the
/// group or the next word; a long one's value is attached (`=`) or the
/// next word. `tar --checkpoint-action=exec=CMD` runs CMD; old-style `tar
/// xIf CMD a.tar` takes the values in the order of its letters.
const OPERAND_RUNNERS: &[(&[&str], RunnerOptions)] = &[
    (
        &["su", "runuser"],
        &[
            ("-c", 2, Runs::Line),
            ("--command", 3, Runs::Line),
            ("--session-command", 4, Runs::Line),
            ("-s", 2, Runs::Line),
            ("--shell", 4, Runs::Line),
        ],
    ),
    (
        &["flock"],
        &[("-c", 2, Runs::Line), ("--command", 5, Runs::Line)],
    ),
    (
        &["script"],
        &[("-c", 2, Runs::Line), ("--command", 3, Runs::Line)],
    ),
    (
        &["rsync"],
        &[("-e", 2, Runs::Program), ("--rsh", 5, Runs::Program)],
    ),
    (
        &["rg"],
        &[
            ("--pre", 5, Runs::Program),
            ("--hostname-bin", 14, Runs::Program),
        ],
    ),
    (
        &["sort", "gsort"],
        &[("--compress-program", 4, Runs::Filter)],
    ),
    (
        TARS,
        &[
            ("-I", 2, Runs::Filter),
            ("--use-compress-program", 5, Runs::Filter),
            ("--to-command", 6, Runs::Filter),
            ("-F", 2, Runs::Line),
            ("--info-script", 5, Runs::Line),
            ("--new-volume-script", 6, Runs::Line),
            ("--rsh-command", 4, Runs::Program),
            ("--checkpoint-action", 13, Runs::Line),
        ],
    ),
];

/// #1449: a runner's options: each spelling, its shortest prefix and how
/// it runs the value.
type RunnerOptions = &'static [(&'static str, usize, Runs)];

/// #1449: GNU tar's names.
const TARS: &[&str] = &["tar", "gtar"];
/// #1449: old-style tar letters that take the next value, in order.
const TAR_VALUE_LETTERS: &str = "bCfFgHIKLNTVX";

/// #1449: the commands of [`OPERAND_RUNNERS`] that `nested::runner_scripts`
/// reads only with [`runner_values`] (it reads the others' `-c` as before,
/// and these values besides).
pub(super) const VALUE_RUNNERS: &[&str] = &["rsync", "rg", "sort", "gsort", "tar", "gtar"];

/// #1449: an option value a runner runs.
pub(super) struct RunnerValue {
    /// The word holding it.
    pub(super) holder: usize,
    pub(super) value: String,
    pub(super) runs: Runs,
}

/// #1449: for the [`OPERAND_RUNNERS`] command at `at` in `words`, its
/// option values that are commands, for the options among the words
/// before `end`; and whether those words go past `limit`, unread (the
/// line asks). The command word's own options are all read: only a
/// wrapper before it (`flock F …`) is held to the limit ([`limit_for`]).
pub(super) fn runner_values(
    words: &[String],
    at: usize,
    end: usize,
    limit: usize,
) -> (Vec<RunnerValue>, bool) {
    let lower = words[at].to_lowercase();
    let name = basename(&lower);
    let Some((_, options)) = OPERAND_RUNNERS
        .iter()
        .find(|(names, _)| names.contains(&name))
    else {
        return (Vec::new(), false);
    };
    let end = end.min(words.len());
    let read_end = end.min(at.saturating_add(1).saturating_add(limit));
    let mut values = Vec::new();
    // Old-style tar: a first word of letters without a dash; its values
    // run to `old_end`.
    let mut old_end = at + 2;
    if TARS.contains(&name)
        && let Some(letters) = words
            .get(at + 1)
            .filter(|word| at + 1 < end && !word.starts_with('-'))
    {
        let mut next = at + 2;
        for letter in letters.chars() {
            work::step();
            if !TAR_VALUE_LETTERS.contains(letter) {
                continue;
            }
            let Some(value) = words.get(next) else {
                break;
            };
            let runs = match letter {
                'I' => Some(Runs::Filter),
                'F' => Some(Runs::Line),
                _ => None,
            };
            if let Some(runs) = runs {
                values.push(RunnerValue {
                    holder: next,
                    value: value.clone(),
                    runs,
                });
            }
            next += 1;
        }
        old_end = next;
    }
    for index in at + 1..read_end {
        work::step();
        let word = &words[index];
        // Options end at `--`, unless it is an option's value (`tar -cf
        // --`, old-style `tar xf --`).
        if word == "--"
            && !words[index - 1].starts_with('-')
            && !(TARS.contains(&name) && index < old_end)
        {
            return (values, false);
        }
        // More values than are judged: the line asks.
        if values.len() > super::outbound::MAX_JUDGED_POSITIONS {
            return (values, true);
        }
        // An option's value may be the word after the last one read.
        let next = || words.get(index + 1).map(|next| (index + 1, next.clone()));
        for (option, shortest, runs) in *options {
            work::step();
            let value = if option.starts_with("--") {
                let (long, attached) = match word.split_once('=') {
                    Some((long, value)) => (long, Some(value)),
                    None => (word.as_str(), None),
                };
                if !(long.len() >= *shortest && option.starts_with(long)) {
                    continue;
                }
                match attached {
                    Some(value) => Some((index, value.to_string())),
                    None => next(),
                }
            } else if let Some(cluster) = word.strip_prefix('-')
                && !cluster.starts_with('-')
                && let Some(letter) = option.chars().nth(1)
                && let Some((at_letter, _)) = cluster.char_indices().find(|(_, ch)| *ch == letter)
            {
                let rest = &cluster[at_letter + 1..];
                if rest.is_empty() {
                    next()
                } else {
                    Some((index, rest.to_string()))
                }
            } else {
                continue;
            };
            let Some((holder, value)) = value else {
                continue;
            };
            let value = if *option == "--checkpoint-action" {
                match value.strip_prefix("exec=") {
                    Some(command) => command.to_string(),
                    None => continue,
                }
            } else {
                value
            };
            values.push(RunnerValue {
                holder,
                value,
                runs: *runs,
            });
        }
    }
    (values, end > read_end)
}

/// #1449: how many words after a runner at `at` are read for its
/// options, for a line whose command word is at `command`: all for the
/// command itself when `whole` (the first one read in full: one per
/// simple command), else `MAX_JUDGED_POSITIONS`.
pub(super) fn limit_for(at: usize, command: usize, whole: bool) -> usize {
    if at == command && whole {
        usize::MAX
    } else {
        super::outbound::MAX_JUDGED_POSITIONS
    }
}

/// #1449: where the options of the runner at `at` may be, for a line
/// whose command word is at `command`: `flock`'s end at the command it
/// runs (`flock F grep -c "$p"`); the others' (`su bob -c CMD`) at the end.
pub(super) fn options_end(words: &[String], at: usize, command: usize) -> usize {
    if at < command && basename(&words[at].to_lowercase()) == "flock" {
        command + 1
    } else {
        words.len()
    }
}

/// #1449: whether a runner among `words` up to the command word at `index`
/// (`su -c`, `flock F -c`, `rsync -e`, `tar -I`; see [`OPERAND_RUNNERS`])
/// runs an option value holding a value the classifier cannot see (`flock
/// F -c "$(cat notes)"`), or has more words than are read
/// ([`options_end`]).
pub(super) fn runner_unseen(words: &[String], unseen: &[bool], index: usize) -> bool {
    (0..=index.min(words.len().saturating_sub(1))).any(|at| {
        work::step();
        let end = options_end(words, at, index);
        let (values, cut) = runner_values(words, at, end, limit_for(at, index, /*whole*/ true));
        cut || values
            .iter()
            .any(|value| unseen.get(value.holder).copied().unwrap_or(true))
    })
}

/// Whether a simple command runs an operand holding a value the classifier
/// cannot see. `index` and `name` are its command word's index and
/// lowercase basename; `unseen` marks the words holding such a value.
pub(super) fn runs_unseen(name: &str, words: &[String], unseen: &[bool], index: usize) -> bool {
    let after = |from: usize| unseen.iter().skip(from).any(|unseen| *unseen);
    if RUNS_OPERANDS.contains(&name) {
        return after(index + 1);
    }
    // Literal substitutions in its text (`print -P '$(cmd)'`) are read as
    // commands already.
    if name == "print" && prints_prompt(&words[index + 1..]) {
        return after(index + 1);
    }
    // `split --filter` runs its command on each piece of a file: its input
    // is code too (`--filter=sh`).
    if name == "split"
        && words[index + 1..]
            .iter()
            .any(|word| word.starts_with("--fi"))
    {
        return true;
    }
    // A dashed helper (`git-rebase`) is `git rebase`; `git.exe` is git.
    let helper = git_helpers::git_name(name);
    let subcommand = helper.as_ref().and_then(Option::as_deref);
    let name = if helper.is_some() { "git" } else { name };
    for (command, required, starts) in COMMAND_OPTIONS {
        work::step();
        if *command != name
            || !(required.is_empty()
                || subcommand == Some(*required)
                || words[index + 1..].iter().any(|word| word == required))
        {
            continue;
        }
        let git = name == "git";
        let start = words[index + 1..].iter().position(|word| {
            starts.iter().any(|start| {
                let long = word.split_once('=').map_or(word.as_str(), |(name, _)| name);
                word == start
                    || (start.starts_with("--")
                        && (long == *start || (git && long.len() >= 4 && start.starts_with(long))))
                    || (git
                        && start.len() == 2
                        && start.starts_with('-')
                        && word.starts_with('-')
                        && !word.starts_with("--")
                        && word[1..].contains(&start[1..]))
            })
        });
        if let Some(start) = start
            && after(index + 1 + start)
        {
            return true;
        }
    }
    false
}

/// Whether zsh `print`'s words (`args`, those after it) include `-P`. Read
/// loosely, so as to ask: any option word before `-` or `--` counts, past
/// option values and operands.
fn prints_prompt(args: &[String]) -> bool {
    for word in args {
        work::step();
        if word == "-" || word == "--" {
            return false;
        }
        if word.starts_with('-') && !word.starts_with("--") && word.contains('P') {
            return true;
        }
    }
    false
}

/// #1405: whether `mapfile`/`readarray` may run a callback (`-C eval`) on
/// each line it reads, so its input is code. A literal option word may
/// hold `C` (`-tC`, `-Ceval`), and any word a glob (`-?`, extglob `-!(t)`)
/// that may match a file `-C`. A value the classifier cannot see (`$o`,
/// `-$x`, `${o^}`, `$IFS`) may spell it, unless it is a length
/// (`"${#a[@]}"`, the append idiom); a known one may split, glob or change
/// case into it (`o=' -C'`, `IFS=,`, `declare -u o=-c`). Redirection
/// targets holding a value (`< "$f"`, `< <(ls)`) are not its words, but
/// one right after an option that takes a value is (`-d '<' -C`: the
/// lexer ignores quotes; [`quoted_mapfile_callback`] reads such lines with
/// their quotes). `words`/`unseen`: the command's words with known values
/// filled in, index for index with `simple.words`.
pub(super) fn mapfile_callback(simple: &SimpleCommand, words: &[String], unseen: &[bool]) -> bool {
    use std::collections::HashSet;
    let Some(index) = mapfile_word(words, |_| false) else {
        return false;
    };
    let set = |indexes: &[usize]| indexes.iter().copied().collect::<HashSet<usize>>();
    let (writes, misread) = (set(&simple.writes_to), set(&simple.operated));
    let hidden = set(&simple.hidden);
    // An option word whose last letter takes the next word as its value.
    let takes_value = |word: &str| {
        word.starts_with('-')
            && !word.starts_with("--")
            && word.ends_with(['d', 'n', 'O', 's', 'u', 'C', 'c'])
    };
    words.iter().enumerate().skip(index + 1).any(|(at, word)| {
        work::step();
        let raw = simple.words.get(at).map_or(word.as_str(), String::as_str);
        let built = super::may_be_built(raw) || unseen.get(at).is_some_and(|unseen| *unseen);
        let target =
            simple.stdin_from == Some(at) || simple.here_string == Some(at) || writes.contains(&at);
        let after_option = at
            .checked_sub(1)
            .and_then(|before| simple.words.get(before))
            .is_some_and(|before| takes_value(before));
        if target && built && !after_option {
            return false;
        }
        if misread.contains(&at)
            || hidden.contains(&at)
            || unseen.get(at).is_some_and(|unseen| *unseen)
            || super::may_be_built(word)
        {
            return !length(raw);
        }
        if built {
            return globbed(word) || (word.contains('-') && word.contains(['c', 'C']));
        }
        globbed(word) || (word.starts_with('-') && !word.starts_with("--") && word.contains('C'))
    })
}

/// #1405: whether shell `text`, read with its quotes (`quoted.rs`), holds a
/// `mapfile`/`readarray` that may run a callback. The word lexer ignores
/// quotes, so a quoted `<` or `>` (`-d 'x>' "$o"`) reads as a redirection
/// there and hides the word after it from [`mapfile_callback`]. Here its
/// options are read as bash reads them: a literal option word holding `C`
/// or a glob asks; so does a value in an option's place (`"$o"`), unless
/// it is a length, and an option's value that may split or glob into more
/// words (`-n $n`). A double-quoted value of an option (`-n "$n"`) is one
/// word. A reading that is unsure where a quote or substitution ends asks
/// too. `coproc` runs the command after it. Called only for a line the
/// word lexer found a `mapfile` in, whatever its spelling (`ma''pfile`,
/// `$m`). #1422: a command word holding a brace expression
/// (`{,mapfile}`) may be it too. Text that may not be shell (`strict`
/// false) is read loosely, as `nested.rs` reads it: a quote it leaves open
/// does not ask.
pub(super) fn quoted_mapfile_callback(text: &str, strict: bool) -> bool {
    let reading = quoted::commands(text);
    if reading.too_deep || reading.unsure || (strict && reading.unterminated) {
        return true;
    }
    reading.commands.iter().any(|command| {
        let texts: Vec<String> = command.words.iter().map(|word| word.text.clone()).collect();
        // The word lexer found a `mapfile` in this line: a command named by
        // a value (`$m`) or a brace expression (`{,mapfile}`) may be it.
        // One that may expand to an option too (`{mapfile,-C}`) asks.
        let braced = |at: usize| {
            command
                .words
                .get(at)
                .is_some_and(|word| brace_expression(&word.text))
        };
        let dynamic = |at: usize| command.words.get(at).is_some_and(|word| word.dynamic);
        mapfile_word(&texts, |at| dynamic(at) || braced(at)).is_some_and(|at| {
            (braced(at) && texts[at].contains('-')) || quoted_callback(&command.words[at + 1..])
        })
    })
}

/// #1422: the command strings `compgen -C cmd` runs as shell code, from its
/// words after `compgen` (`args`, from the quote-aware reading), read as
/// bash reads its options: up to the first operand or `--`, a letter that
/// takes a value taking the rest of its word or the next word (`-W '-C
/// x'` is a word list). `None` when an option's place holds a value or a
/// brace expression (`"$o"`, `{-C,cmd}`), which may be `-C`.
pub(super) fn compgen_commands(args: &[quoted::Word]) -> Option<Vec<String>> {
    let mut commands = Vec::new();
    let mut at = 0;
    while let Some(word) = args.get(at) {
        work::step();
        at += 1;
        if word.dynamic || word.text.contains(UNSEEN_OUTPUT) || brace_expression(&word.text) {
            return None;
        }
        let word = &word.text;
        if word == "--" || word == "-" || !word.starts_with('-') {
            break;
        }
        for (offset, letter) in word.char_indices().skip(1) {
            work::step();
            if !"oAGWFCXPSV".contains(letter) {
                continue;
            }
            let rest = &word[offset + letter.len_utf8()..];
            let value = if rest.is_empty() {
                at += 1;
                args.get(at - 1).map(|word| word.text.as_str())
            } else {
                Some(rest)
            };
            if letter == 'C' {
                commands.extend(value.map(str::to_string));
            }
            break;
        }
    }
    Some(commands)
}

/// #1405: whether `words` (a command's, values filled in) may run `mapfile`
/// or `readarray`. #1422: any word of that name counts, in any case: the
/// word lexer ignores quotes, so a quoted blank or separator in an
/// assignment before it (`x='a b' mapfile`) moves the command word.
pub(super) fn names_mapfile(words: &[String]) -> bool {
    words.iter().any(|word| {
        work::step();
        let name = basename(word);
        name.eq_ignore_ascii_case("mapfile") || name.eq_ignore_ascii_case("readarray")
    })
}

/// #1405: the index of a `mapfile`/`readarray` command word in `words`,
/// past assignments, keywords, wrappers and `coproc` (`time coproc
/// mapfile`), or of a word `also` accepts there.
fn mapfile_word(words: &[String], also: impl Fn(usize) -> bool) -> Option<usize> {
    let mut start = 0;
    while let Some((at, name)) = words.get(start..).and_then(indirect::command_word) {
        work::step();
        let at = start + at;
        if name == "coproc" {
            start = at + 1;
            continue;
        }
        return (matches!(name.as_str(), "mapfile" | "readarray") || also(at)).then_some(at);
    }
    None
}

/// #1405: see [`quoted_mapfile_callback`]; `args` are the words after the
/// command word.
fn quoted_callback(args: &[quoted::Word]) -> bool {
    let mut value_next = false;
    for word in args {
        work::step();
        let text = word.text.as_str();
        let dynamic = word.dynamic || text.contains(UNSEEN_OUTPUT);
        if std::mem::take(&mut value_next) {
            if (dynamic && word.splits && !length(text)) || (!dynamic && globbed(text)) {
                return true;
            }
            continue;
        }
        if dynamic {
            if !length(text) {
                return true;
            }
            continue;
        }
        if text == "--" {
            return false;
        }
        // #1422: a brace expression may expand to an option (`{-C,eval}`).
        if globbed(text) || brace_expression(text) {
            return true;
        }
        if let Some(cluster) = text.strip_prefix('-')
            && !cluster.is_empty()
        {
            if cluster.contains('C') {
                return true;
            }
            // The first letter taking a value takes the rest of the word,
            // or the next word.
            if let Some(at) = cluster.find(['d', 'n', 'O', 's', 'u', 'c']) {
                value_next = at + 1 == cluster.len();
            }
        }
    }
    false
}

/// #1422: whether `text` holds a brace expression (`{,mapfile}`, `{a..c}`),
/// not a parameter expansion (`${PY:-python3.11}`).
fn brace_expression(text: &str) -> bool {
    let mut open = false;
    let mut before = None;
    for ch in text.chars() {
        work::step();
        match ch {
            '{' => open = before != Some('$'),
            '}' => open = false,
            ',' if open => return true,
            '.' if open && before == Some('.') => return true,
            _ => {}
        }
        before = Some(ch);
    }
    false
}

/// #1405: whether `word` may glob (or extglob) into another word.
fn globbed(word: &str) -> bool {
    word.contains(['*', '?', '[', ']', '(', ')', '!', '@', '+'])
}

/// Whether `raw` is only the length of a variable or array (`${#a[@]}`,
/// `${#s}`, `$#`), as written or with the lexer's braces dropped.
fn length(raw: &str) -> bool {
    if raw == "$#" {
        return true;
    }
    let Some(rest) = raw.strip_prefix("${#").or_else(|| raw.strip_prefix("$#")) else {
        return false;
    };
    let rest = rest.strip_suffix('}').unwrap_or(rest);
    let name = rest
        .strip_suffix("[@]")
        .or_else(|| rest.strip_suffix("[*]"))
        .unwrap_or(rest);
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// #1159: whether git's words after `git` (`args`) name a program git runs
/// that the classifier does not read, or copy hooks or set config on a new
/// repository: `--upload-pack`, `--receive-pack`, `push`/`archive --exec`,
/// `clone -u`, `clone`/`init --template`, `clone -c`/`--config`, and (#1516)
/// `grep -O`/`--open-files-in-pager`, which runs a pager on the matching
/// files (`-O sh` runs them). git takes any unambiguous prefix of a long
/// option (`--upload`, `--templ`, `--op`).
/// `subcommand`: a dashed helper's (`git-clone`). Read once per command,
/// from its first git word to its end.
pub(super) fn git_runs_program(args: &[String], subcommand: Option<&str>) -> bool {
    // A later dashed helper (`git-clone`) names its subcommand too.
    let has = |word: &str| {
        subcommand == Some(word)
            || args.iter().any(|arg| {
                work::step();
                arg == word || git_helpers::git_name(arg).flatten().as_deref() == Some(word)
            })
    };
    let clone = has("clone");
    let creates = clone || has("init");
    let exec = has("push") || has("archive") || has("send-pack") || has("fetch-pack");
    let grep = has("grep");
    let long = |word: &str, option: &str, shortest: usize| {
        let name = word.split_once('=').map_or(word, |(name, _)| name);
        name.len() >= shortest && option.starts_with(name)
    };
    args.iter().any(|word| {
        work::step();
        if word.starts_with("--") {
            long(word, "--upload-pack", "--up".len())
                || long(word, "--receive-pack", "--rec".len())
                || (exec && long(word, "--exec", "--ex".len()))
                || (creates && long(word, "--template", "--te".len()))
                || (clone && long(word, "--config", "--con".len()))
                || (grep && long(word, "--open-files-in-pager", "--op".len()))
        } else {
            // A group of short options (`-qc k=v`, `-u./x`, `-nOsh`).
            word.strip_prefix('-').is_some_and(|cluster| {
                (clone && cluster.contains(['c', 'u'])) || (grep && cluster.contains('O'))
            })
        }
    })
}

/// #1064: the names a nameref declaration makes (`declare -n r=v`, `local
/// -n r`, `typeset -n`, ksh `nameref r=v`): each reads another variable's
/// value, which the substituted words do not follow. `command` is the
/// command word's index and lowercase name.
pub(super) fn namerefs<'a>(command: Option<&(usize, String)>, words: &'a [String]) -> Vec<&'a str> {
    let Some((index, name)) = command else {
        return Vec::new();
    };
    let operands = &words[index + 1..];
    let nameref = name == "nameref"
        || (matches!(name.as_str(), "declare" | "typeset" | "local")
            && operands.iter().any(|word| {
                word.starts_with('-') && !word.starts_with("--") && word.contains('n')
            }));
    if !nameref {
        return Vec::new();
    }
    operands
        .iter()
        .filter(|word| !word.starts_with(['-', '+']))
        .map(|word| word.split_once('=').map_or(word.as_str(), |(name, _)| name))
        .collect()
}
