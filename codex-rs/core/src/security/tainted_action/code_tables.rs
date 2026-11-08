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
use super::quoted;
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
/// name written still asks). `seen`, from the quote-aware reading of
/// text read as shell and aligned with `words`, says which words may be
/// any or several; with it, such words where a writer reads its options
/// or before a name ask too ([`Reading`]).
/// D410: a `stat` doing so is [`Writes::StatIfLoaded`].
pub(super) fn writes_table(words: &[String], raw: &[String], seen: Option<&[Seen]>) -> Writes {
    let mut writes = Writes::No;
    each_command(words, |at, name| {
        let args = &words[at + 1..];
        let seen = seen.map(|seen| seen.get(at + 1..).unwrap_or_default());
        if writer_names_table(name, args)
            || hides_name(name, args, raw.get(at + 1..).unwrap_or_default(), seen)
        {
            writes = if name == "stat" {
                Writes::StatIfLoaded
            } else {
                Writes::Table
            };
        }
        writes != Writes::No
    });
    writes
}

/// What [`writes_table`] found.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Writes {
    No,
    Table,
    /// Only zsh's `stat` builtin would (D410, #1704): zsh starts without
    /// it, so this asks only where the action may load it
    /// ([`may_load_stat`]); elsewhere `stat` is the external program,
    /// which writes no variable (`stat -f %m $f` on macOS).
    StatIfLoaded,
}

/// Calls `judge` with the position and name of each command `words` run,
/// past assignments, wrappers, `eval`/`trap` text, `repeat N`, zsh's
/// modifiers and `coproc`, until it returns true; returns whether it did.
fn each_command(words: &[String], mut judge: impl FnMut(usize, &str) -> bool) -> bool {
    let mut from = 0;
    while let Some((at, name)) = indirect::command_word(&words[from..]) {
        work::step();
        let at = from + at;
        if judge(at, &name) {
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

/// D410 (#1704): whether a line (`command`, as written, and `commands`, its
/// simple commands as lexed) may load a `stat` builtin. zsh starts without
/// one (start-up files may load it; D410 accepts that). A word, as written
/// with quotes and backslashes aside (`zmod''load`) or as lexed
/// (`$'zmo\144load'`, lines joined, comma braces expanded), names a load
/// ([`names_stat_load`]); a word as written holds a brace range, which the
/// lexer does not expand (`zmo{d..d}load`); or a command, also one `eval`,
/// `trap`, `repeat` or a modifier runs, after a redirection or a zsh
/// `[[ … ]]` test (`>f $a$b`, `if [[ 1 ]] $a$b`), is bash's `enable`
/// (`enable -f stat stat` loads bash's own), or is named by an expansion
/// or a pattern (`eval $a$b $c$d`, `$(cat c)`, `zm*`).
pub(super) fn may_load_stat(command: &[String], commands: &[shell::SimpleCommand]) -> bool {
    command.iter().any(|part| {
        work::step();
        names_stat_load(part) || brace_range(part)
    }) || commands.iter().any(|simple| {
        work::step();
        simple.words.iter().any(|word| {
            work::step();
            names_stat_load(word)
        }) || each_command(&run_words(simple), |_, name| {
            name == "enable"
                || (!matches!(name, "[" | "[[")
                    && name.contains(['$', '`', '*', '?', '[', '{', '(']))
                || name.contains(shell::UNSEEN_OUTPUT)
        })
    })
}

/// D410: whether `text`, quotes and backslashes aside, names `zmodload`
/// (any use, `zmodload -F zsh/stat b:zstat` too) or the module.
pub(super) fn names_stat_load(text: &str) -> bool {
    let plain: String = text
        .chars()
        .filter(|ch| {
            work::step();
            !matches!(ch, '\'' | '"' | '\\')
        })
        .collect();
    plain.contains("modload") || plain.contains("zsh/stat")
}

/// The words of `simple` the shell may run as commands: without those a
/// redirection holds (`>f $a$b` runs `$a$b`) or a zsh `[[ … ]]` test spans
/// (`if [[ 1 ]] $a$b`, a short `if`).
fn run_words(simple: &shell::SimpleCommand) -> Vec<String> {
    let mut test = false;
    let mut words = Vec::with_capacity(simple.words.len());
    for (at, word) in simple.words.iter().enumerate() {
        work::step();
        let held = simple.writes_to.binary_search(&at).is_ok()
            || simple.redirect_words.binary_search(&at).is_ok()
            || simple.stdin_from == Some(at)
            || simple.here_string == Some(at);
        match word.as_str() {
            _ if held => {}
            "[[" if !test => test = true,
            "]]" if test => test = false,
            _ if test => {}
            _ => words.push(word.clone()),
        }
    }
    words
}

/// Whether `text` holds a brace range (`{a..z}`, `{1..3}`), read loosely:
/// a `..` between a `{` and the next `}`.
fn brace_range(text: &str) -> bool {
    let mut open = false;
    let mut dot = false;
    let mut range = false;
    for ch in text.chars() {
        work::step();
        match ch {
            '{' => (open, dot, range) = (true, false, false),
            '}' if open && range => return true,
            '}' => open = false,
            '.' if open => (range, dot) = (range || dot, true),
            _ => dot = false,
        }
    }
    false
}

/// What the quote-aware reading shows of a word ([`writes_table`]).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Seen {
    /// Its text as written.
    Known,
    /// One word of a value the classifier cannot see (`"$o"`).
    One,
    /// Any number of words: an unquoted expansion, which may split (`$o`,
    /// `$(cat f)`), one of several words (`"$@"`), a brace expansion, or
    /// an unquoted pattern that may match a file named like an option or
    /// a table (`?A`, `"$o"*`).
    Many,
    /// Any number of words, none an option or a table's name: a pattern
    /// that cannot match one (`*.png`, `:completion:*`).
    Pattern,
}

impl Seen {
    pub(super) fn of(word: &quoted::Word) -> Self {
        // In double quotes too: `"$@"`, `"${a[@]}"`, zsh's flags
        // (`"${(@f)x}"`, `"${=x}"`), and zsh's `$=x`, `$~x` and `$^x`,
        // which the reading keeps as text; not a length (`"${#a[@]}"`).
        let mut text = String::with_capacity(word.text.len());
        let mut rest = word.text.as_str();
        while let Some(start) = rest.find("${#") {
            work::step();
            text.push_str(&rest[..start]);
            rest = rest[start..]
                .find('}')
                .map_or("", |end| &rest[start + end + 1..]);
        }
        text.push_str(rest);
        let several = [
            "$@", "${@", "[@]", "${(", "${=", "${~", "${^", "$=", "$~", "$^",
        ]
        .iter()
        .any(|form| {
            work::step();
            text.contains(form)
        });
        if word.splits
            || word.brace_expansion
            || several
            || (word.globs && (word.dynamic || may_match(&word.text)))
        {
            Seen::Many
        } else if word.dynamic {
            Seen::One
        } else if word.globs {
            Seen::Pattern
        } else {
            Seen::Known
        }
    }
}

/// Where a writer not judged by `code_variables` takes the names it writes:
/// its option letters whose value is a name, those taking another value,
/// and which operands (counted from 0, past its options) are names
/// ([`reading`] says where it reads options). `zparseopts` (all its words
/// are options, specs or names) and `zstyle` are judged in [`hides_name`].
const NAME_POSITIONS: &[(&str, &str, &str, Operands)] = &[
    ("set", "A", "o", Operands::None),
    ("print", "v", "fuCxX", Operands::None),
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

/// Writers `code_variables` judges by their names, read here only with
/// quotes (where redirections are not words: `read -r x < "$f"`), for a
/// value that may be several words (`read -d $(cat f)`) or a word that may
/// be an option where options go. zsh's `read -p` and `-k` take no value
/// there; counting them as taking one only adds prompts.
const QUOTED_NAME_POSITIONS: &[(&str, &str, &str, Operands)] = &[
    ("read", "aA", "dinNptuk", Operands::From(0)),
    ("mapfile", "", "dnOsuCc", Operands::From(0)),
    ("readarray", "", "dnOsuCc", Operands::From(0)),
    ("vared", "", "prMmift", Operands::From(0)),
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
fn hides_name(name: &str, args: &[String], raw: &[String], seen: Option<&[Seen]>) -> bool {
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
    let seen_at = |at: usize| {
        seen.and_then(|seen| seen.get(at).copied())
            .unwrap_or(Seen::Known)
    };
    let many = |at: usize| seen_at(at) == Seen::Many;
    // Whether a word after `at` is one the classifier cannot see, or names
    // a table. Worked out once, from the end.
    let later = std::cell::OnceCell::new();
    let names_later = |at: usize| {
        let later = later.get_or_init(|| {
            let mut later = vec![false; args.len() + 1];
            for at in (0..args.len()).rev() {
                work::step();
                later[at] = later[at + 1]
                    || matches!(seen_at(at), Seen::One | Seen::Many)
                    || names_table(&args[at]);
            }
            later
        });
        later.get(at + 1).copied().unwrap_or(false)
    };
    // A word where an option goes: several words may be an option taking a
    // name (`set $(cat f)`). So may one with a word after it where options
    // take names, or both (`set "$o" ls ./x` with `-Acommands`); where
    // none does, it may be a flag moving a later word, one the classifier
    // cannot see or a table's name, into a name's place.
    let option = |at: usize, takes_names: bool| match seen_at(at) {
        Seen::Many => true,
        Seen::One if takes_names => at + 1 < args.len(),
        Seen::One => names_later(at),
        Seen::Pattern | Seen::Known => false,
    };
    match name {
        "zparseopts" => return (0..args.len()).any(hidden),
        // `zstyle -s|-b|-a ctx style NAME`, `zstyle -g NAME`; its first
        // word may be such an option (`zstyle $o :x y functions`), and a
        // context or style that is several words may hold the name
        // (`zstyle -a $(cat f)`).
        "zstyle" => {
            // `zstyle` takes no option's value attached (`-afoo`): one
            // word there is one option, its name a later word.
            if option(0, /*takes_names*/ false) {
                return true;
            }
            let Some(option) = args.first().filter(|arg| arg.starts_with('-')) else {
                return false;
            };
            return (option.contains(['s', 'b', 'a']) && (hidden(3) || (1..3).any(many)))
                || (option.contains('g') && hidden(1));
        }
        _ => {}
    }
    let quoted = seen.is_some().then_some(QUOTED_NAME_POSITIONS);
    let Some(&(_, names, values, operands)) = NAME_POSITIONS
        .iter()
        .chain(quoted.into_iter().flatten())
        .find(|(writer, ..)| *writer == name)
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
    // Several words before the last name move a later word into it.
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
            // Where options lead, one the classifier cannot see (`-$o`) may
            // be any.
            if leading && option(at, !names.is_empty()) {
                return true;
            }
            if let Some((flag_at, flag)) = arg[1..]
                .char_indices()
                .find(|(_, flag)| names.contains(*flag) || values.contains(*flag))
            {
                let attached = 1 + flag_at + flag.len_utf8() < arg.len();
                let value = if attached { at } else { at + 1 };
                // A hidden name, or where options lead a value that is
                // several words (`set -o $(cat f)`).
                if (names.contains(flag) && hidden(value)) || (leading && many(value)) {
                    return true;
                }
                at = value;
            }
        } else if options && leading && option(at, !names.is_empty()) {
            return true;
        } else if options && leading && seen_at(at) == Seen::Pattern {
            // A pattern may match nothing (`*.none(N)`, `nullglob`), so
            // options may still follow it, or several words: either moves
            // a later word into an option's or a name's place.
            if names_later(at) {
                return true;
            }
        } else {
            let index = operand.unwrap_or(0);
            // Several words before a name move a later word into it: one
            // the classifier cannot see, or a table's (`zgetattr *.png
            // "$n"`).
            let moves = many(at) || (seen_at(at) == Seen::Pattern && names_later(at));
            if (named(index) && hidden(at)) || (leading && before_name(index) && moves) {
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

/// Whether a pattern may match a file named like an option or a table
/// (`?A`, `-e?`, `*`, `function[s]`). One matching only paths (`~/x`,
/// `*/x`), names with another start (`:completion:*`), or names holding a
/// `.` and ending other than in `]` (`*.png`), which no table, name or
/// option taking one is, cannot; not past zsh's `^`, `~` or a group, nor a
/// `.` in brackets or before `#` (`*~a/b`, `^*.png`, `*[.]x`, `x.#`).
fn may_match(pattern: &str) -> bool {
    let Some(glob) = pattern.find(GLOB) else {
        return false;
    };
    // zsh's `#` repeats what is before it, which may be nothing.
    let mut prefix = &pattern[..glob];
    if pattern[glob..].starts_with('#') {
        prefix = prefix
            .char_indices()
            .next_back()
            .map_or(prefix, |(last, _)| &prefix[..last]);
    }
    if !prefix.starts_with(['-', '+'])
        && !TABLES.iter().any(|table| {
            work::step();
            table.starts_with(prefix)
        })
    {
        return false;
    }
    let mut chars = pattern.chars().peekable();
    let mut first = true;
    let mut bracket = false;
    let mut dot = false;
    while let Some(ch) = chars.next() {
        work::step();
        match ch {
            '^' | '(' | '|' => return true,
            '~' if !first => return true,
            '[' => bracket = true,
            ']' => bracket = false,
            '/' if !bracket => return false,
            '.' if !bracket && chars.peek() != Some(&'#') => dot = true,
            _ => {}
        }
        first = false;
    }
    !(dot && !bracket && !pattern.ends_with(GLOB) && !pattern.ends_with(']'))
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
