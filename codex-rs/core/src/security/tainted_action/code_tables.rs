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
//! A name the classifier cannot see (`zparseopts -a $n`, `functio[n]s`)
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
/// name written still asks).
pub(super) fn writes_table(words: &[String], raw: &[String]) -> bool {
    let mut from = 0;
    while let Some((at, name)) = indirect::command_word(&words[from..]) {
        work::step();
        let at = from + at;
        let args = &words[at + 1..];
        if writer_names_table(&name, args)
            || hides_name(&name, args, raw.get(at + 1..).unwrap_or_default())
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
/// and which operands (counted from 0, past its options) are names.
/// `zparseopts` (all its words are options, specs or names) and `zstyle`
/// are judged in [`hides_name`].
const NAME_POSITIONS: &[(&str, &str, &str, Operands)] = &[
    ("set", "A", "o", Operands::None),
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

/// Whether the command `name` with `args` (and `raw`, as written) takes a
/// name it writes from a word the classifier cannot see: an expansion, a
/// command's output, or a word the shell may glob or brace-expand into a
/// table's name (`touch functions; zparseopts -a functio[n]s`). Its other
/// words (`set -- "$@"`, `zstyle ':x' y "$v"`) are not names.
fn hides_name(name: &str, args: &[String], raw: &[String]) -> bool {
    let hidden = |at: usize| {
        [args.get(at), raw.get(at)]
            .into_iter()
            .flatten()
            .any(|word| {
                work::step();
                word.contains(['$', '`', '*', '?', '[', '{', '^', '~', '#', '(', '<'])
                    || word.contains(shell::UNSEEN_OUTPUT)
            })
    };
    match name {
        "zparseopts" => return (0..args.len()).any(hidden),
        // `zstyle -s|-b|-a ctx style NAME`, `zstyle -g NAME`.
        "zstyle" => {
            let Some(option) = args.first().filter(|arg| arg.starts_with('-')) else {
                return false;
            };
            return (option.contains(['s', 'b', 'a']) && hidden(3))
                || (option.contains('g') && hidden(1));
        }
        _ => {}
    }
    let Some(&(_, names, values, operands)) =
        NAME_POSITIONS.iter().find(|(writer, ..)| *writer == name)
    else {
        return false;
    };
    let named = |operand: usize| match operands {
        Operands::None => false,
        Operands::From(first) => operand >= first,
        Operands::At(list) => list.contains(&operand),
    };
    // Past the options (`--` or the first operand ends them).
    let mut operand = None;
    let mut at = 0;
    while at < args.len() {
        work::step();
        let arg = &args[at];
        match operand {
            None if arg == "--" => operand = Some(0),
            None if arg.len() > 1 && arg.starts_with(['-', '+']) => {
                if let Some((flag_at, flag)) = arg[1..]
                    .char_indices()
                    .find(|(_, flag)| names.contains(*flag) || values.contains(*flag))
                {
                    let attached = 1 + flag_at + flag.len_utf8() < arg.len();
                    let value = if attached { at } else { at + 1 };
                    if names.contains(flag) && hidden(value) {
                        return true;
                    }
                    at = value;
                }
            }
            _ => {
                let index = operand.unwrap_or(0);
                if named(index) && hidden(at) {
                    return true;
                }
                operand = Some(index + 1);
            }
        }
        at += 1;
    }
    false
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
