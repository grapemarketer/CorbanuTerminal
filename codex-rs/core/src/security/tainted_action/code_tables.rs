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

use super::indirect;
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
/// word naming a table, past assignments, wrappers, `eval`/`trap` text and
/// zsh's modifiers. Over-matches only (a word naming a table that is not
/// the name written still asks).
pub(super) fn writes_table(words: &[String]) -> bool {
    let mut from = 0;
    while let Some((at, name)) = indirect::command_word(&words[from..]) {
        work::step();
        let at = from + at;
        let args = &words[at + 1..];
        if writer_names_table(&name, args) {
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
/// (`${(AA)=aliases::=…}`, `${commands[ls]::=…}`, `${BASH_CMDS[ls]:=…}`),
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
            if !option && !matches!(before, None | Some('{' | '$' | '!' | '#' | '=' | ')')) {
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
