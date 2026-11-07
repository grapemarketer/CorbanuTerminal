//! #1685, #1704: the shells' tables of code run by name. zsh's `functions`
//! and alias tables hold code run when the name is (`functions[f]='…'; f`),
//! its `commands` and bash's `BASH_CMDS` the programs names run; bash's
//! `BASH_ALIASES` holds aliases. An assignment to one of their entries is
//! read as a line of its own where its value is code (`nested.rs`). Any
//! other way to write one fails closed: a command writing variables named
//! by its operands that names a table ([`writes_table`]: `zparseopts -a
//! functions`, `zstyle -a ctx style functions`, `read -A aliases`), and a
//! write in an expansion or a reference ([`sets_table_otherwise`]:
//! `${(AA)=aliases::=…}`, `typeset -n r=BASH_ALIASES` too).
//! A name the classifier cannot see (`zparseopts -a $n`, `function[s]`)
//! fails closed where such a command takes the name it writes.

use super::indirect;
use super::shell;
use super::unknown_folder;
use super::work;

/// The tables, as they are spelled (parameter names are case-sensitive).
const TABLES: &[&str] = &[
    "functions",
    "dis_functions",
    "aliases",
    "dis_aliases",
    "galiases",
    "dis_galiases",
    "saliases",
    "dis_saliases",
    "commands",
    "BASH_ALIASES",
    "BASH_CMDS",
];

/// Builtins (bash, zsh and its modules, zsh's shipped functions) that write
/// variables named by their operands, wherever the name is (`-afunctions`,
/// `-x:=functions`, `f=functions`).
const NAME_WRITERS: &[&str] = &[
    "read",
    "mapfile",
    "readarray",
    "getopts",
    "vared",
    "let",
    "wait",
    "compgen",
    "compadd",
    "coproc",
    "zparseopts",
    "sysread",
    "getln",
    "zformat",
    "zregexparse",
    "pcre_match",
    "regexp-replace",
    "zstat",
    "strftime",
    "zselect",
    "ztcp",
    "zsocket",
    "zpty",
    "zsystem",
    "zcurses",
    "ztie",
    "zgetattr",
    "zlistattr",
];

/// Commands that declare variables: a word names one before its `=`
/// (`local x=functions` sets `x`), or anywhere with a reference option
/// (`typeset -n r=functions`).
const DECLARERS: &[&str] = &[
    "typeset", "declare", "local", "export", "readonly", "integer", "float", "private",
];

/// Commands that write a variable named by an operand only with an option
/// holding one of these letters: `set -A`/`+A`, `print -v`/`printf -v`,
/// zsh's `stat -A`/`-H`, and `zstyle`'s fetching forms (`-a`, `-s`, `-b`,
/// `-g`, ...; [`writes_table`] counts any option).
const OPTION_WRITERS: &[(&str, &[char])] = &[
    ("set", &['A']),
    ("print", &['v']),
    ("printf", &['v']),
    ("stat", &['A', 'H']),
];

/// Commands run with the words after them: `eval` and `trap` text (the word
/// lexer splits it), zsh's `repeat N`, `coproc`, precommand modifiers, and
/// (by [`indirect::command_word`]) `builtin`, `command`, `exec` and the like.
fn skipped_words(name: &str, words: &[String]) -> Option<usize> {
    match name {
        "eval" | "trap" => Some(1 + usize::from(words.get(1).is_some_and(|word| word == "--"))),
        "repeat" => Some(2),
        name if unknown_folder::MODIFIERS.contains(&name) => Some(1),
        _ => None,
    }
}

/// Whether one simple command's `words` write, by name, a table of code
/// run by name: a writer of variables named by its operands
/// ([`NAME_WRITERS`], [`DECLARERS`], [`OPTION_WRITERS`], `zstyle`) with a
/// word naming a table, or one the classifier cannot see where it takes a
/// name ([`hides_name`]; `raw` are the words as written, aligned with
/// `words`), past assignments, wrappers, `eval`/`trap` text and zsh's
/// modifiers. Over-matches only (a word naming a table that is not the
/// name written still asks). Hidden words where a writer reads its
/// options, or before a name ([`Reading`]), count only in `strict` text:
/// read as shell, not loosely (literals joined, as from `awk -F'\t'
/// '{print $2}'`, or text that may not be shell), as for #937's
/// arithmetic.
pub(super) fn writes_table(words: &[String], raw: &[String], strict: bool) -> bool {
    let mut from = 0;
    while let Some((at, name)) = indirect::command_word(&words[from..]) {
        work::step();
        let at = from + at;
        let args = &words[at + 1..];
        if writer_names_table(&name, args)
            || hides_name(&name, args, raw.get(at + 1..).unwrap_or_default(), strict)
        {
            return true;
        }
        // `coproc read -A functions`: `coproc` both names an array and runs
        // the command after it.
        match skipped_words(&name, &words[at..]) {
            Some(skip) => from = (at + skip).min(words.len()),
            None if name == "coproc" => from = at + 1,
            None => return false,
        }
    }
    false
}

/// Where a writer not judged by `code_variables` takes the names it writes:
/// its option letters whose value is a name, those taking another value,
/// and which operands (counted from 0, past its options) are names
/// ([`reading`] says where it reads options). `zparseopts` (all its words
/// are options, specs or names) and `zstyle` are judged in [`hides_name`].
const NAME_POSITIONS: &[(&str, &str, &str, Operands)] = &[
    ("set", "A", "o", Operands::None),
    ("print", "v", "ufCxX", Operands::None),
    ("printf", "v", "", Operands::None),
    ("stat", "AH", "fF", Operands::None),
    ("zstat", "AH", "fF", Operands::None),
    ("wait", "p", "", Operands::None),
    ("compgen", "V", "AGWXPSFCo", Operands::None),
    ("compadd", "AOD", "PSpsiIWdJVXxrRMEFo", Operands::None),
    ("zformat", "faF", "", Operands::None),
    ("pcre_match", "va", "n", Operands::None),
    ("zselect", "aA", "t", Operands::None),
    ("strftime", "s", "", Operands::None),
    ("zsystem", "f", "ti", Operands::None),
    ("getopts", "", "", Operands::At(&[1])),
    ("getln", "", "", Operands::From(0)),
    ("ztie", "", "df", Operands::From(0)),
    ("sysread", "c", "iost", Operands::From(0)),
    ("zcurses", "", "", Operands::From(2)),
    ("zregexparse", "", "", Operands::At(&[0, 1])),
    ("regexp-replace", "", "", Operands::At(&[0])),
    ("zpty", "", "", Operands::At(&[1])),
    ("zlistattr", "", "", Operands::At(&[1])),
    ("zgetattr", "", "", Operands::At(&[2])),
];

#[derive(Clone, Copy)]
enum Operands {
    None,
    From(usize),
    At(&'static [usize]),
}

/// How a writer reads its options, and which words the classifier cannot
/// see fail closed there (#1704, round 8 of #1698's review). The classifier
/// does not know whether such a word was quoted, so it may split into
/// several words (`set $(cat f)`, or a parameter in bash or under zsh's
/// `shwordsplit`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reading {
    /// Options come before the first operand. Any such word among them may
    /// be an option taking a name (`o=-A; set $o commands ls ./x`) or
    /// several words; one among the operands before the last name may move
    /// a later word into a name's place (`zlistattr $(cat f)`).
    Leading,
    /// Options come among the operands too (`zselect 0 -A NAME`, `zsystem
    /// flock -f NAME file`). Only a hidden name fails closed: these write
    /// descriptor numbers.
    Anywhere,
    /// Options come before the first operand, and only a hidden name fails
    /// closed: `wait -p` writes a job's number (`wait $pid` stays quiet).
    Names,
}

fn reading(writer: &str) -> Reading {
    match writer {
        "zselect" | "zsystem" => Reading::Anywhere,
        "wait" => Reading::Names,
        _ => Reading::Leading,
    }
}

/// Whether the command `name` with `args` (and `raw`, as written) takes a
/// name it writes from a word the classifier cannot see: an expansion, a
/// command's output, or a word the shell may glob or brace-expand into a
/// table's name (`touch functions; zparseopts -a function[s]`); or has
/// such a word where it reads options, or before a name it may move
/// (see [`Reading`]). Its other words (`set -- "$@"`, `zstyle ':x' y
/// "$v"`, `wait $pid`) are not names.
fn hides_name(name: &str, args: &[String], raw: &[String], strict: bool) -> bool {
    let hidden = |at: usize| {
        [args.get(at), raw.get(at)]
            .into_iter()
            .flatten()
            .any(|word| {
                work::step();
                word.contains(['$', '`'])
                    || word.contains(GLOB)
                    || word.contains(shell::UNSEEN_OUTPUT)
            })
    };
    // Where a name is not taken, a word that may become any other or
    // several ([`may_become`]).
    let unseen = |at: usize| {
        strict
            && [args.get(at), raw.get(at)]
                .into_iter()
                .flatten()
                .any(|word| {
                    work::step();
                    may_become(word)
                })
    };
    match name {
        "zparseopts" => return (0..args.len()).any(hidden),
        // `zstyle -s|-b|-a ctx style NAME`, `zstyle -g NAME`; a hidden
        // first word may be such an option (`zstyle $o :x y functions`),
        // and a hidden context or style may be several words, the name
        // among them (`zstyle -a $(cat f)`).
        "zstyle" => {
            if unseen(0) {
                return true;
            }
            let Some(option) = args.first().filter(|arg| arg.starts_with('-')) else {
                return false;
            };
            return (option.contains(['s', 'b', 'a']) && (hidden(3) || (1..3).any(unseen)))
                || (option.contains('g') && hidden(1));
        }
        _ => {}
    }
    let Some(&(_, names, values, operands)) =
        NAME_POSITIONS.iter().find(|(writer, ..)| *writer == name)
    else {
        return false;
    };
    let reading = reading(name);
    let leading = reading == Reading::Leading;
    let named = |operand: usize| match operands {
        Operands::None => false,
        Operands::From(first) => operand >= first,
        Operands::At(list) => list.contains(&operand),
    };
    // A split operand before the last name moves a later word into it.
    let before_name = |operand: usize| match operands {
        Operands::None => false,
        Operands::From(first) => operand < first,
        Operands::At(list) => list.iter().any(|name| operand < *name),
    };
    // Past the options: `--` ends them, and so does the first operand
    // where options lead.
    let mut operand = None;
    let mut ended = false;
    let mut at = 0;
    while at < args.len() {
        work::step();
        let arg = &args[at];
        let options = !ended && (operand.is_none() || reading == Reading::Anywhere);
        if options && arg == "--" {
            ended = true;
            operand = Some(operand.unwrap_or(0));
        } else if options && arg.len() > 1 && arg.starts_with(['-', '+']) {
            // Where options lead, a hidden option (`-$o`) may be any.
            if leading && unseen(at) {
                return true;
            }
            if let Some((flag_at, flag)) = arg[1..]
                .char_indices()
                .find(|(_, flag)| names.contains(*flag) || values.contains(*flag))
            {
                let attached = 1 + flag_at + flag.len_utf8() < arg.len();
                let value = if attached { at } else { at + 1 };
                // A hidden name, or where options lead a hidden value,
                // which may be several words (`set -o $(cat f)`).
                if (names.contains(flag) && hidden(value)) || (leading && unseen(value)) {
                    return true;
                }
                at = value;
            }
        } else if options && leading && unseen(at) {
            // Where options lead, a hidden word before them ends may be
            // one, or several (`set $o commands ls ./x`, `set $(cat f)`).
            return true;
        } else {
            let index = operand.unwrap_or(0);
            if (named(index) && hidden(at)) || (leading && before_name(index) && unseen(at)) {
                return true;
            }
            operand = Some(index + 1);
        }
        at += 1;
    }
    false
}

/// Characters by which the shell may glob a word (zsh's extended forms
/// too) or that a brace expansion left.
const GLOB: [char; 9] = ['*', '?', '[', '{', '^', '~', '#', '(', '<'];

/// Whether a word may become any word, or several: an expansion or a
/// command's output, whose value the classifier cannot see and which may
/// split (it does not know whether the word was quoted), or a pattern
/// that may match a file named like an option or a table (`?A`, `*`,
/// `function[s]`). A pattern matching only paths (`~/x`, `*/x`; not past
/// zsh's `~` exclusion or in a group: `*~a/b`) or names with another start
/// (`:completion:*`) cannot.
fn may_become(word: &str) -> bool {
    if word.contains(['$', '`']) || word.contains(shell::UNSEEN_OUTPUT) {
        return true;
    }
    let Some(glob) = word.find(GLOB) else {
        return false;
    };
    let paths = word.contains('/')
        && !word
            .get(1..)
            .is_some_and(|rest| rest.contains(['~', '(', '|']));
    let prefix = &word[..glob];
    !paths
        && ["-", "+"].iter().chain(TABLES).any(|start| {
            work::step();
            start.starts_with(prefix)
        })
}

/// Whether the command `name` with `args` writes a variable named by a
/// word that names a table.
fn writer_names_table(name: &str, args: &[String]) -> bool {
    let option = |letters: &[char]| {
        args.iter().any(|arg| {
            work::step();
            arg.starts_with(['-', '+']) && arg.contains(letters)
        })
    };
    let writes = NAME_WRITERS.contains(&name)
        || (name == "zstyle" && args.first().is_some_and(|arg| arg.starts_with('-')))
        || OPTION_WRITERS
            .iter()
            .any(|(writer, letters)| *writer == name && option(letters));
    if writes {
        return args.iter().any(|arg| names_table(arg));
    }
    if DECLARERS.contains(&name) {
        let reference = option(&['n']);
        return args.iter().any(|arg| {
            let named = match arg.split_once('=') {
                Some((name, _)) if !reference && !arg.starts_with(['-', '+']) => name,
                _ => arg.as_str(),
            };
            names_table(named)
        });
    }
    false
}

/// Whether `word` names a table: not inside another name (`myfunctions`,
/// `functions_x`), except as an option's attached value (`-afunctions`).
fn names_table(word: &str) -> bool {
    let attached = word.starts_with(['-', '+']);
    TABLES.iter().any(|table| {
        word.match_indices(table).any(|(at, _)| {
            work::step();
            let before = word[..at].chars().next_back();
            let after = word[at + table.len()..].chars().next();
            !after.is_some_and(identifier) && (attached || !before.is_some_and(identifier))
        })
    })
}

fn identifier(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// Whether a word may write a table other than by a command naming it
/// ([`writes_table`]) or an assignment to an entry `nested.rs` reads as
/// code (`aliases[x]=…`, `exempt`): a write in an expansion
/// (`${(AA)=aliases::=…}`, `${^~commands[ls]::=…}`, `${BASH_CMDS[ls]:=…}`),
/// an entry assignment to a table of programs (`commands[ls]=./x`), or a
/// value naming a table, which may name it later (`x=functions; eval
/// "$x[f]=…"`; not an option's value, `--only=functions`, which no writer
/// but `zparseopts` reads as a name, and [`writes_table`] reads it). For
/// zsh's `functions`, any subscript (`printf -v 'functions[f]'`) too.
pub(super) fn sets_table_otherwise(text: &str, exempt: bool) -> bool {
    TABLES.iter().any(|table| {
        text.match_indices(table).any(|(at, _)| {
            work::step();
            let rest = &text[at + table.len()..];
            let before = text[..at].chars().next_back();
            let option = text.starts_with('-') && before == Some('v');
            if !option
                && !matches!(
                    before,
                    None | Some('{' | '$' | '!' | '#' | '=' | ')' | '~' | '^' | '+')
                )
            {
                return false;
            }
            let writes = |rest: &str| {
                rest.starts_with('=')
                    || rest.starts_with("+=")
                    || rest.starts_with(":=")
                    || rest.starts_with("::=")
            };
            if at == 0 && exempt {
                return false;
            }
            writes(rest)
                || (rest.starts_with('[')
                    && (*table == "functions"
                        || rest.match_indices(']').any(|(end, _)| {
                            work::step();
                            writes(&rest[end + 1..])
                        })))
                || (before == Some('=') && rest.is_empty() && !text.starts_with('-'))
        })
    })
}
