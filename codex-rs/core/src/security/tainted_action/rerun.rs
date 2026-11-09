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
        let target = simple.stdin_from.binary_search(&at).is_ok()
            || simple.here_string.binary_search(&at).is_ok()
            || writes.contains(&at);
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
/// #1528: a subcommand git does not know may be an alias from existing
/// config (`git g -Osh x` with `alias.g=grep`) or a typo `help.autocorrect`
/// runs (`git gerp -Osh x`), so it may be `grep`, `clone`, `init` or `push`:
/// its `-O`, `-u`, `-c`, `--open-files-in-pager`, `--exec`, `--template` and
/// `--config` ask too (the long ones as in #1597).
pub(super) fn git_runs_program(args: &[String], subcommand: Option<&str>) -> bool {
    // A later dashed helper (`git-clone`) names its subcommand too.
    let has = |word: &str| {
        subcommand == Some(word)
            || args.iter().any(|arg| {
                work::step();
                arg == word || git_helpers::git_name(arg).flatten().as_deref() == Some(word)
            })
    };
    // A dashed helper's words start with its own options, not a subcommand;
    // a later `git` in them starts one (`git-bisect run git g -Osh x`).
    // Its `-c` asks only after it: before it, `-c k=v` is git's own.
    let unknown_at = unknown_git_subcommand(args, /*scanning*/ subcommand.is_none());
    let unknown = unknown_at.is_some();
    let clone = has("clone");
    let creates = clone || has("init");
    // #1876: ls-remote takes a hidden `--exec` (`git ls-remote --exec=./x .`).
    let exec =
        has("push") || has("archive") || has("send-pack") || has("fetch-pack") || has("ls-remote");
    let grep = has("grep");
    let long = |word: &str, option: &str, shortest: usize| {
        let name = word.split_once('=').map_or(word, |(name, _)| name);
        name.len() >= shortest && option.starts_with(name)
    };
    // Git's `ext::` smart transport runs an arbitrary command as the remote
    // helper (`git fetch ext::sh -c id` runs `sh -c id`), code the host cannot
    // read. Git refuses `ext` by default, but config the classifier cannot see
    // may allow it, so a remote naming it asks like git's other
    // program-running forms.
    if args.iter().any(|word| {
        work::step();
        ext_remote_scheme(word)
    }) {
        return true;
    }
    // #1876: git's `ext::` helper run by itself (`git remote-ext x 'sh -c
    // id'`) runs its second operand when its input asks it to connect,
    // whatever `protocol.ext.allow` says. In any case: a case-insensitive
    // filesystem (macOS) finds the helper for `git REMOTE-EXT` too.
    if has("remote-ext")
        || args.iter().any(|arg| {
            work::step();
            arg.eq_ignore_ascii_case("remote-ext")
        })
    {
        return true;
    }
    args.iter().enumerate().any(|(at, word)| {
        work::step();
        if word.starts_with("--") {
            long(word, "--upload-pack", "--up".len())
                || long(word, "--receive-pack", "--rec".len())
                || ((exec || unknown) && long(word, "--exec", "--ex".len()))
                || ((creates || unknown) && long(word, "--template", "--te".len()))
                || ((clone || unknown) && long(word, "--config", "--con".len()))
                || ((grep || unknown) && long(word, "--open-files-in-pager", "--op".len()))
        } else {
            // A group of short options (`-qc k=v`, `-u./x`, `-nOsh`).
            word.strip_prefix('-').is_some_and(|cluster| {
                (clone && cluster.contains(['c', 'u']))
                    || (grep && cluster.contains('O'))
                    || (unknown && cluster.contains(['O', 'u']))
                    || (unknown_at.is_some_and(|unknown| at > unknown) && cluster.contains('c'))
            })
        }
    })
}

/// #1876: git subcommands that contact a remote. Their remote (`ext::sh -c
/// id`) or a transport option (`--upload-pack=sh`) may run a program, so
/// where the line does not show those words, the command asks.
pub(super) const GIT_TRANSPORTS: &[&str] = &[
    "fetch",
    "pull",
    "clone",
    "ls-remote",
    "push",
    "archive",
    "fetch-pack",
    "send-pack",
    "remote",
    "submodule",
];

/// #1876: the options of each transport subcommand that take the next word
/// as their value (`--depth 1`), long and short, as `git <subcommand> -h`
/// lists them (git 2.43 and 2.56; `--[no-]x[=<v>]` takes no next word), plus
/// hidden and newer ones (`ls-remote --exec`, `clone --revision`). A
/// subcommand git does not know (an alias) may be any of them.
fn git_transport_valued(sub: &str) -> (&'static [&'static str], &'static str) {
    match sub {
        "fetch" => (
            &[
                "--upload-pack",
                "--jobs",
                "--depth",
                "--shallow-since",
                "--shallow-exclude",
                "--deepen",
                "--refmap",
                "--server-option",
                "--negotiation-tip",
                "--filter",
                "--recurse-submodules-default",
                "--submodule-prefix",
            ],
            "oj",
        ),
        "pull" => (
            &[
                "--cleanup",
                "--strategy",
                "--strategy-option",
                "--upload-pack",
                "--depth",
                "--shallow-since",
                "--shallow-exclude",
                "--deepen",
                "--refmap",
                "--server-option",
                "--negotiation-tip",
                "--filter",
            ],
            "sXo",
        ),
        "clone" => (
            &[
                "--jobs",
                "--template",
                "--reference",
                "--reference-if-able",
                "--origin",
                "--branch",
                "--upload-pack",
                "--depth",
                "--shallow-since",
                "--shallow-exclude",
                "--separate-git-dir",
                "--config",
                "--server-option",
                "--filter",
                "--bundle-uri",
                "--ref-format",
                "--revision",
            ],
            "jobuc",
        ),
        "ls-remote" => (
            &["--upload-pack", "--exec", "--sort", "--server-option"],
            "o",
        ),
        "push" => (
            &[
                "--repo",
                "--receive-pack",
                "--exec",
                "--push-option",
                "--recurse-submodules",
            ],
            "o",
        ),
        "archive" => (
            &[
                "--format",
                "--prefix",
                "--add-file",
                "--add-virtual-file",
                "--output",
                "--mtime",
                "--remote",
                "--exec",
            ],
            "o",
        ),
        "send-pack" => (
            &["--receive-pack", "--exec", "--remote", "--push-option"],
            "",
        ),
        "fetch-pack" => (&[], ""),
        "remote" => (&["--track", "--master"], "tm"),
        "submodule" => (&["--branch", "--name", "--reference", "--depth"], "b"),
        _ => (GIT_TRANSPORT_ANY_VALUED, "obucjsXtm"),
    }
}

/// The valued long options of all the transport subcommands.
const GIT_TRANSPORT_ANY_VALUED: &[&str] = &[
    "--upload-pack",
    "--receive-pack",
    "--exec",
    "--jobs",
    "--depth",
    "--shallow-since",
    "--shallow-exclude",
    "--deepen",
    "--refmap",
    "--server-option",
    "--push-option",
    "--negotiation-tip",
    "--filter",
    "--recurse-submodules-default",
    "--submodule-prefix",
    "--cleanup",
    "--strategy",
    "--strategy-option",
    "--template",
    "--reference",
    "--reference-if-able",
    "--origin",
    "--branch",
    "--separate-git-dir",
    "--config",
    "--bundle-uri",
    "--ref-format",
    "--revision",
    "--recurse-submodules",
    "--sort",
    "--repo",
    "--format",
    "--prefix",
    "--add-file",
    "--add-virtual-file",
    "--output",
    "--mtime",
    "--remote",
    "--track",
    "--master",
    "--name",
];

/// For an option word of transport subcommand `sub`: whether git takes the
/// next word as its value, and whether that value may still be the remote,
/// so that one the line does not show asks: `--remote` and push's `--repo`
/// name it, and an alias's options may be anything. A group of short
/// options (`-qb main`) takes it when its first valued letter is the last
/// letter (`-bmain` holds its value). A long option may be abbreviated
/// (`--bra main`): a prefix of a valued option takes it, but may abbreviate
/// another option, so its value is doubted.
fn git_transport_option(sub: &str, word: &str) -> (bool, bool) {
    let (long_valued, short_valued) = git_transport_valued(sub);
    let alias = !GIT_COMMANDS.contains(&sub);
    if let Some(long) = word.strip_prefix("--") {
        if long.is_empty() || long.contains('=') {
            return (false, false);
        }
        if long_valued.contains(&word) {
            return (true, alias || matches!(word, "--remote" | "--repo"));
        }
        let prefix = long_valued.iter().any(|option| {
            work::step();
            option.starts_with(word)
        });
        return (prefix, prefix);
    }
    let letters = &word[1..];
    let takes = letters
        .char_indices()
        .find(|(_, letter)| {
            work::step();
            short_valued.contains(*letter)
        })
        .is_some_and(|(at, letter)| at + letter.len_utf8() == letters.len());
    (takes, takes && alias)
}

/// #1876: whether git subcommand `sub` may contact a remote: a transport,
/// or one git does not know, which may be an alias of one (#1528).
pub(super) fn git_may_contact_remote(sub: &str) -> bool {
    GIT_TRANSPORTS.contains(&sub) || !GIT_COMMANDS.contains(&sub)
}

/// #1876: whether a transport subcommand's remote, or another word git may
/// read as one of its options, is text the line does not show: a variable it
/// does not resolve (`git fetch "$r"` with `r` read from input, set to a
/// quoted value with spaces, or from the environment), a substitution's
/// output or a backtick. Git would read that value as the remote, and an
/// `ext::` remote runs a program (#1841 judges the visible ones); git reads
/// options after operands too, so a value after the remote may be
/// `--upload-pack=…` (`git fetch origin "$v"`). Such a word stays quiet when a
/// visible start means it is no option (`HEAD:"$b"`, `refs/heads/"$b"`), or
/// after `--`. The remote is the first operand; `remote add|set-url <name>`
/// and `submodule add` name it after their verb, their other verbs take none,
/// and `archive`'s is only `--remote`'s value. `subcommand`: a dashed
/// helper's (`git-fetch`); git's own is found in `args`, and one git does not
/// know may be an alias of a transport (#1528). The words read end at the
/// next git word (`sudo -u git git fetch "$r"`: the user name is not the git
/// that runs), so each word is read by one scan.
pub(super) fn git_remote_unseen(args: &[String], subcommand: Option<&str>) -> bool {
    let end = args
        .iter()
        .position(|word| {
            work::step();
            basename(word) == "git"
                || indirect::equals_git(word)
                || git_helpers::git_name(word).is_some()
        })
        .unwrap_or(args.len());
    let args = &args[..end];
    // A dashed helper (`git-fetch`) names its subcommand; its words start
    // right after its name. Otherwise the subcommand is git's first word
    // after its global options and their values (`-C x fetch`).
    let (sub, start) = match subcommand {
        Some(sub) => (sub.to_ascii_lowercase(), 0),
        None => {
            let mut value_next = false;
            let Some(at) = args.iter().position(|word| {
                work::step();
                if std::mem::take(&mut value_next) {
                    return false;
                }
                if word.starts_with('-') {
                    value_next = super::code_variables::GIT_VALUE_OPTIONS.contains(&word.as_str())
                        || matches!(word.as_str(), "-c" | "--config-env");
                    return false;
                }
                true
            }) else {
                return false;
            };
            (args[at].to_ascii_lowercase(), at + 1)
        }
    };
    // #1528: a subcommand git does not know may be an alias of `fetch`.
    if !git_may_contact_remote(&sub) {
        return false;
    }
    let unseen = |word: &str| word.contains(['$', '`']) || word.contains(UNSEEN_OUTPUT);
    if unseen(&sub) {
        return false; // a subcommand the line does not show already asks (simple_command)
    }
    // Whether a word the line does not show may start with `-`: nothing
    // visible comes before its first unseen text (`"$v"`, `$(…)x`).
    let may_be_option = |word: &str| {
        let word = word.trim_start_matches(['"', '\'']);
        word.starts_with(['$', '`', '-']) || word.starts_with(UNSEEN_OUTPUT)
    };
    let rest = args.get(start..).unwrap_or_default();
    let before = match sub.as_str() {
        "remote" | "submodule" => {
            let verb = rest.iter().find(|word| {
                work::step();
                !word.starts_with('-')
            });
            match (sub.as_str(), verb.map(String::as_str)) {
                (_, None) => return false,
                // A verb the line does not show may be `add`.
                (_, Some(verb)) if unseen(verb) => return true,
                ("remote", Some("add" | "set-url")) => 2,
                ("submodule", Some("add")) => 1,
                _ => return false,
            }
        }
        // `git archive <tree-ish> [<path>…]`: its remote is `--remote`'s
        // value, wherever that is (`git archive HEAD --remote "$r"`).
        "archive" => usize::MAX,
        _ => 0,
    };
    let mut operands = 0;
    let mut value_next = false;
    let mut doubted = false;
    let mut after_dashes = false;
    for word in rest {
        work::step();
        // An option's value (`--depth "$n"`) is not the remote, unless it
        // is doubted (`git archive --remote "$r"`, an abbreviation).
        // Program-running options (`--upload-pack "$x"`) ask whatever their
        // value (`git_runs_program`).
        if std::mem::take(&mut value_next) {
            if std::mem::take(&mut doubted) && unseen(word) {
                return true;
            }
            continue;
        }
        if after_dashes {
            // Operands only, none an option: one up to the remote may
            // still be the remote (`git fetch -- "$r"`).
            if unseen(word) && before != usize::MAX && operands <= before {
                return true;
            }
            operands += 1;
            continue;
        }
        if word == "--" {
            after_dashes = true;
            continue;
        }
        if word.starts_with('-') {
            if unseen(word) && git_attached_value_doubted(&sub, word, unseen) {
                return true; // it may be `--remote=$r` or `--$x`
            }
            if !word.contains('=') {
                (value_next, doubted) = git_transport_option(&sub, word);
            }
            continue;
        }
        // An operand up to the remote may be the remote (archive's are not:
        // they are a tree-ish and paths); any may be an option.
        let remote_or_before = before != usize::MAX && operands <= before;
        if unseen(word) && (remote_or_before || may_be_option(word)) {
            return true;
        }
        operands += 1;
    }
    false
}

/// For an option word the line does not fully show (`--depth="$n"`,
/// `--remote=$r`, `-j"$n"`): whether it asks. A valued long option named
/// in full, whose value cannot name the remote (`--depth`, `--branch`),
/// keeps a value the line does not show quiet; `--remote`, `--repo`,
/// abbreviations, short options, an alias's options and an option named by
/// text the line does not show may be anything. Program-running options
/// (`--upload-pack=$x`) ask in `git_runs_program`.
fn git_attached_value_doubted(sub: &str, word: &str, unseen: impl Fn(&str) -> bool) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return true;
    };
    if unseen(name) || !name.starts_with("--") || !GIT_COMMANDS.contains(&sub) {
        return true;
    }
    let (long_valued, _) = git_transport_valued(sub);
    !long_valued.contains(&name) || matches!(name, "--remote" | "--repo")
}

/// Whether `word` names a git remote using the `ext::` smart transport, whose
/// operand git runs as a command: the word itself, or a long option's value
/// after its first `=` (`--remote=ext::…`, `--repo=ext::…`), which git passes
/// to the transport too. The scheme is matched case-insensitively at the
/// start of the remote, so text holding `ext::` later (`git@host:ext::x`,
/// `crate::ext::Foo`) is left alone.
fn ext_remote_scheme(word: &str) -> bool {
    let is_ext = |remote: &str| {
        remote
            .split_once("::")
            .is_some_and(|(scheme, rest)| scheme.eq_ignore_ascii_case("ext") && !rest.is_empty())
    };
    is_ext(word)
        || (word.starts_with("--") && word.split_once('=').is_some_and(|(_, value)| is_ext(value)))
}

/// #1528: git's built-in commands (`git --list-cmds=builtins`; list from
/// #1597). An alias cannot hide one of them, and autocorrect runs only what
/// matches none. Optional porcelain (`gui`, `send-email`, `subtree`…) is
/// left out: where it is not installed, an alias of that name runs.
/// #1635: so are built-ins newer than git 2.34 (`hook` 2.36,
/// `fsmonitor--daemon` 2.36, `diagnose` 2.38, `replay` 2.44, `refs` 2.46,
/// `backfill` 2.49, `diff-pairs` 2.50), and `whatchanged` and
/// `pack-redundant`, which builds with breaking changes leave out: on a git
/// without them, an alias of that name runs (`alias.refs=clone`).
const GIT_COMMANDS: &[&str] = &[
    "add",
    "am",
    "annotate",
    "apply",
    "archive",
    "bisect",
    "blame",
    "branch",
    "bugreport",
    "bundle",
    "cat-file",
    "check-attr",
    "check-ignore",
    "check-mailmap",
    "check-ref-format",
    "checkout",
    "checkout--worker",
    "checkout-index",
    "cherry",
    "cherry-pick",
    "clean",
    "clone",
    "column",
    "commit",
    "commit-graph",
    "commit-tree",
    "config",
    "count-objects",
    "credential",
    "credential-cache",
    "credential-cache--daemon",
    "credential-store",
    "describe",
    "diff",
    "diff-files",
    "diff-index",
    "diff-tree",
    "difftool",
    "fast-export",
    "fast-import",
    "fetch",
    "fetch-pack",
    "fmt-merge-msg",
    "for-each-ref",
    "for-each-repo",
    "format-patch",
    "fsck",
    "fsck-objects",
    "gc",
    "get-tar-commit-id",
    "grep",
    "hash-object",
    "help",
    "index-pack",
    "init",
    "init-db",
    "interpret-trailers",
    "log",
    "ls-files",
    "ls-remote",
    "ls-tree",
    "mailinfo",
    "mailsplit",
    "maintenance",
    "merge",
    "merge-base",
    "merge-file",
    "merge-index",
    "merge-ours",
    "merge-recursive",
    "merge-recursive-ours",
    "merge-recursive-theirs",
    "merge-subtree",
    "merge-tree",
    "mktag",
    "mktree",
    "multi-pack-index",
    "mv",
    "name-rev",
    "notes",
    "pack-objects",
    "pack-refs",
    "patch-id",
    "pickaxe",
    "prune",
    "prune-packed",
    "pull",
    "push",
    "range-diff",
    "read-tree",
    "rebase",
    "receive-pack",
    "reflog",
    "remote",
    "remote-ext",
    "remote-fd",
    "repack",
    "replace",
    "rerere",
    "reset",
    "restore",
    "rev-list",
    "rev-parse",
    "revert",
    "rm",
    "send-pack",
    "shortlog",
    "show",
    "show-branch",
    "show-index",
    "show-ref",
    "sparse-checkout",
    "stage",
    "stash",
    "status",
    "stripspace",
    "submodule",
    "submodule--helper",
    "switch",
    "symbolic-ref",
    "tag",
    "unpack-file",
    "unpack-objects",
    "update-index",
    "update-ref",
    "update-server-info",
    "upload-archive",
    "upload-archive--writer",
    "upload-pack",
    "var",
    "verify-commit",
    "verify-pack",
    "verify-tag",
    "version",
    "worktree",
    "write-tree",
];

/// #1528: where a subcommand in git's words after `git` (`args`) is not
/// one of git's own commands. Global options and their values are skipped
/// (`-C x`, `-c k=v`); a later git word (`git.exe`, `=git`) starts another.
fn unknown_git_subcommand(args: &[String], mut scanning: bool) -> Option<usize> {
    let mut value_next = false;
    for (at, word) in args.iter().enumerate() {
        work::step();
        if basename(word).eq_ignore_ascii_case("git")
            || indirect::equals_git(word)
            || git_helpers::git_name(word) == Some(None)
        {
            scanning = true;
            value_next = false;
            continue;
        }
        if !scanning {
            continue;
        }
        if std::mem::replace(&mut value_next, false) {
            continue;
        }
        if word.starts_with('-') {
            value_next = super::code_variables::GIT_VALUE_OPTIONS.contains(&word.as_str())
                || matches!(word.as_str(), "-c" | "--config-env");
            continue;
        }
        if !GIT_COMMANDS.contains(&word.as_str()) {
            return Some(at);
        }
        scanning = false;
    }
    None
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
