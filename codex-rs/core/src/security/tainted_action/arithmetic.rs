//! #937: shell arithmetic evaluates a variable's value as an expression, and
//! an array subscript in that value runs its command substitution
//! (`v=$(cat f); echo $((v))` runs `a[$(cmd)0]` held in `f`). Like `eval`,
//! arithmetic that uses a variable filled from command output or `read` in
//! the same script runs code the classifier cannot see (owner decision D33,
//! option B). Variables set from literals (loop counters) are not affected.
//!
//! This over-approximates on purpose: quotes are not honoured, names are
//! tracked for the whole line regardless of order, and any value that
//! mentions a filled name passes it on.

use super::indirect;
use super::shell;
use super::work;
use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::Range;

/// `[[` operators that evaluate both operands as arithmetic (bash, zsh and
/// ksh; ksh's `test`/`[` too), and `-v`, whose operand is a name that may
/// carry a subscript (bash and zsh `test` too).
const TEST_OPERATORS: &[&str] = &["-eq", "-ne", "-lt", "-le", "-gt", "-ge", "-v"];
/// Builtins whose operands are numbers evaluated as arithmetic (zsh and ksh
/// evaluate them; bash does not, which only over-matches).
const NUMERIC_BUILTINS: &[&str] = &[
    "let", "exit", "return", "shift", "break", "continue", "repeat", "ulimit", "limit", "unlimit",
];
/// Builtins whose operands name variables, and whose names may hold a
/// subscript (`declare "$x"=1` with `x='a[$(cmd)0]'`).
const NAMING_BUILTINS: &[&str] = &[
    "declare",
    "typeset",
    "local",
    "export",
    "readonly",
    "integer",
    "float",
    "private",
    "unset",
    "read",
    "vared",
    "mapfile",
    "readarray",
    "getopts",
];
/// Builtins that fill their named variables from their input.
const FILLING_BUILTINS: &[&str] = &[
    "read",
    "vared",
    "mapfile",
    "readarray",
    "getopts",
    "sysread",
    "zparseopts",
];
/// Variables the shell fills from input or a match when no name is given:
/// `read`/`select` fill `REPLY`, `mapfile` fills `MAPFILE`, `=~` fills
/// `BASH_REMATCH` (bash) and `match`/`MATCH` (zsh).
const READ_DEFAULTS: &[&str] = &["REPLY", "MAPFILE"];
const MATCH_DEFAULTS: &[&str] = &["BASH_REMATCH", "match", "MATCH"];
/// Shells whose `test`/`[` evaluate `-eq` operands as arithmetic.
const KSH: &[&str] = &["ksh", "ksh93", "mksh", "pdksh", "oksh", "lksh"];
/// Prefix of the nodes that stand for a group of values (`for a b in ...`),
/// so the graph stays linear. A variable of that name only over-taints.
const GROUP: &str = "__corbanu_arithmetic_group_";
/// Compound lists nested deeper than this are not followed (fails closed).
const MAX_LIST_DEPTH: usize = 4;

/// Bytes read for where a quoted qualifier group ends (#1028).
const MAX_QUOTED_GROUP: usize = 4096;

/// Words walked back from an array assignment to where its command starts.
const MAX_ASSIGNMENT_WALK: usize = 64;
/// Longest `$NAME` prefix compared with the variables names are built from.
const MAX_BUILDER_PREFIX: usize = 64;
/// Commands that do not write variables named by their operands (or whose
/// writes are handled on their own: `read`, `printf -v`, `declare`, `for`).
const OPERAND_SAFE: &[&str] = &[
    "((",
    "[[",
    "[",
    "test",
    "let",
    "echo",
    "printf",
    "print",
    ":",
    "true",
    "false",
    "cd",
    "pushd",
    "popd",
    "shift",
    "exit",
    "return",
    "break",
    "continue",
    "unset",
    "set",
    "eval",
    "for",
    "select",
    "foreach",
    "while",
    "until",
    "if",
    "elif",
    "then",
    "else",
    "do",
    "case",
    "local",
    "declare",
    "typeset",
    "export",
    "readonly",
    "integer",
    "float",
    "alias",
    "source",
    ".",
    "noglob",
    "nocorrect",
    "time",
    "exec",
    "command",
    "builtin",
    "trap",
    "kill",
    "sleep",
    "type",
    "which",
    "whence",
    "read",
    "mapfile",
    "readarray",
    "vared",
    "getopts",
    "sysread",
    "zparseopts",
];
/// Builtins (bash, zsh and its modules) that write variables named by
/// their operands.
const VARIABLE_WRITERS: &[&str] = &[
    "getln",
    "zstyle",
    "zformat",
    "zregexparse",
    "pcre_match",
    "zstat",
    "stat",
    "strftime",
    "zselect",
    "ztcp",
    "zsocket",
    "zpty",
    "zsystem",
    "zcurses",
    "compgen",
    "wait",
    "coproc",
    "zparseopts",
    "sysread",
];
/// Stands for the unseen output of a function substitution (`${ cmd; }`).
const FUNCTION_SUBSTITUTION: &str = "__corbanu_function_substitution";
/// Longest `${NAME[...]` subscript `defaults` looks through.
const MAX_SUBSCRIPT: usize = 256;
/// Variables shells fill from input or matches (`sysread`, zsh `(#b)`,
/// `getopts`): once the line holds command output or input, they may hold
/// it too.
const FILLED_DEFAULTS: &[&str] = &[
    "REPLY",
    "reply",
    "MAPFILE",
    "BASH_REMATCH",
    "match",
    "MATCH",
    "mbegin",
    "mend",
    "MBEGIN",
    "MEND",
    "OPTARG",
];
/// Builtins that declare variables (and may give them the integer attribute).
const DECLARING_BUILTINS: &[&str] = &[
    "declare", "typeset", "local", "export", "readonly", "integer", "float", "private",
];
/// `read` options whose value is a number (evaluated as arithmetic in ksh).
const READ_NUMERIC_OPTIONS: &[char] = &['t', 'n', 'N', 'k', 'u'];
/// Positional parameters, which `set --` or a function call can fill.
/// Special parameters that evaluate a value assigned to them as arithmetic
/// (in bash, zsh or ksh).
const INTEGER_SPECIALS: &[&str] = &[
    "EGID",
    "EUID",
    "FUNCNEST",
    "GID",
    "UID",
    "BAUD",
    "COLUMNS",
    "DIRSTACKSIZE",
    "ERRNO",
    "HISTCMD",
    "HISTSIZE",
    "KEYTIMEOUT",
    "LINENO",
    "LINES",
    "LISTMAX",
    "MAILCHECK",
    "OPTIND",
    "RANDOM",
    "SAVEHIST",
    "SECONDS",
    "SHLVL",
    "TMOUT",
    "TRY_BLOCK_ERROR",
    "TRY_BLOCK_INTERRUPT",
    "ZLE_RPROMPT_INDENT",
];
const POSITIONAL: &[&str] = &[
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "8",
    "9",
    "@",
    "*",
    // zsh `argv[1]`, bash `BASH_ARGV[0]` (extdebug).
    "argv",
    "BASH_ARGV",
];

/// Parameters that hold numbers when the line does not assign them (bash
/// and zsh specials, zsh `status`).
const NUMERIC_SPECIALS: &[&str] = &[
    "EGID",
    "EUID",
    "GID",
    "UID",
    "PPID",
    "BASHPID",
    "BASH_SUBSHELL",
    "EPOCHSECONDS",
    "SRANDOM",
    "RANDOM",
    "SECONDS",
    "LINENO",
    "OPTIND",
    "SHLVL",
    "HISTCMD",
    "COLUMNS",
    "LINES",
    "status",
    // What `shell::substitution_output` splices for a fact (`$(date +%s)`,
    // `$(nproc)`): output the classifier trusts.
    "value",
];

/// What one command line does with variables, gathered command by command.
#[derive(Default)]
pub(super) struct Arithmetic {
    /// Functions the line defines (their arguments become `$1`...).
    functions: HashSet<String>,
    /// The line mentions ksh, whose `test`/`[` evaluates `-eq` operands.
    ksh: bool,
    /// Aliases the line defines, by the command they stand for.
    aliases: HashMap<String, String>,
    /// Variables filled from input (`read x`, `mapfile x`).
    filled: HashSet<String>,
    /// `NAME=value` (as lexed), `for NAME in value...`, `printf -v NAME`.
    assigned: Vec<(String, String)>,
    /// Variables given the integer attribute: assigning them evaluates.
    integers: HashSet<String>,
    /// Words evaluated as arithmetic (`let` operands, numeric operands).
    evaluated: Vec<String>,
    /// Words naming variables (`read "$x"`, `declare "$x"=1`).
    named: Vec<String>,
    /// Group nodes created so far.
    groups: usize,
    /// A command whose name comes from a variable is a builtin that
    /// evaluates (`c='let v'; $c`), whose operands the lexer cannot see.
    opaque: bool,
    /// The line sources a file, which shares its variables (`. ./lib.sh`).
    sources: bool,
    /// The line is too nested to follow.
    unfollowed: bool,
    /// Variables whose value a parameter expansion transforms or joins to
    /// other name text (`${x}r`, `${x#z}`, `va$x`): the result may name any
    /// variable, so a value built this way may stand for a tainted one.
    builders: HashSet<String>,
    /// A building expansion whose variable could not be read (`${(L)x}`).
    unknown_builder: bool,
    /// Variables appended to (`x+=ar`) or printed into (`printf -v`),
    /// which may now name any variable.
    appended: HashSet<String>,
    /// Group nodes holding `$_` (alone they do not make the line tainted:
    /// most lines leave command output in `$_`).
    underscore: HashSet<String>,
    /// Some word on the line holds command output.
    output_seen: bool,
    /// The line holds `$'...'` text, which the lexer decodes into one word.
    ansi_c: bool,
    /// The line defines a zsh anonymous function (`() { ... } args`,
    /// `function { ... } args`) or sets `functions[...]`, which take the
    /// positional parameters from words the lexer reads as a command.
    anonymous_function: bool,
    /// Variables declared as namerefs (`declare -n r`).
    namerefs: HashSet<String>,
    /// Name operands of commands that may write them.
    operands: HashSet<String>,
    /// The line uses a ksh/bash 5.3 function substitution (`${ cmd; }`,
    /// `${| cmd; }`), whose output the lexer cannot see.
    function_substitution: bool,
    /// The line runs `eval` or defines an alias: text the shell reads
    /// again, which quotes and escapes can hide from the reader.
    evaluates_text: bool,
    /// Operands the shell evaluates as arithmetic (numeric operands of
    /// builtins, `-eq` operands, numeric `printf` operands), for the
    /// fallback check.
    strict_words: Vec<String>,
    /// Lexed words that may hold a zsh glob-qualifier list (`/*(.[w])`),
    /// whose subscripts and `e`/`+` qualifiers the shell evaluates (#937,
    /// D57), decoded (`zsh -c $'print /*\\x28.[w])'`).
    qualifier_words: Vec<String>,
    /// The line may use zsh glob substitution (#1084): a lexed word spells
    /// it, or a command may turn on options the reader cannot name.
    glob_subst: bool,
}

impl Arithmetic {
    /// Start a command line: `text` is its argv, scanned for function
    /// definitions.
    pub(super) fn new(text: &[String]) -> Self {
        let text: Vec<String> = text.iter().map(|arg| joined(arg)).collect();
        let mut arithmetic = Self {
            functions: text.iter().flat_map(|arg| functions(arg)).collect(),
            ksh: text.iter().any(|arg| {
                arg.split(|ch: char| !(is_name_char(ch) || ch == '/'))
                    .any(|word| KSH.contains(&shell::basename(word)))
            }),
            integers: INTEGER_SPECIALS
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
            ..Self::default()
        };
        for arg in &text {
            work::step();
            let (builders, unknown) = building(arg);
            arithmetic.builders.extend(builders);
            arithmetic.unknown_builder |= unknown;
            arithmetic.function_substitution |= arg
                .match_indices("${")
                .any(|(at, _)| arg[at + 2..].starts_with([' ', '\t', '\n', '|']));
            arithmetic.ansi_c |= arg.contains("$'");
            arithmetic.anonymous_function |= anonymous_function(arg);
            // zsh/mapfile: `$mapfile[file]` reads the file.
            if arg.contains("$mapfile") || arg.contains("${mapfile") {
                arithmetic.filled.insert("mapfile".to_string());
            }
        }
        // Lists in parentheses, which the lexer splits at `(`:
        // `a=(x $(cat f))`, zsh `for x (0 $(cat f))`.
        for arg in &text {
            work::step();
            let (lists, deep) = lists(arg);
            arithmetic.unfollowed |= deep;
            for list in lists {
                work::step();
                let body = &arg[list.body];
                // Raw text: any substitution in it counts as command output.
                let value = if body.contains("$(") || body.contains("<(") || body.contains('`') {
                    format!("{body}{}", shell::UNSEEN_OUTPUT)
                } else {
                    body.to_string()
                };
                let node = arithmetic.group(std::iter::once(value));
                for name in list.names {
                    work::step();
                    arithmetic.assigned.push((name, node.clone()));
                }
            }
        }
        arithmetic
    }

    /// A node holding every value, which the names then receive.
    fn group(&mut self, values: impl IntoIterator<Item = String>) -> String {
        let node = format!("{GROUP}{}", self.groups);
        self.groups += 1;
        for value in values {
            work::step();
            self.assigned.push((node.clone(), value));
        }
        node
    }

    /// Note one simple command: `raw` are its words as lexed and `words`
    /// the same words after variable substitution, which name the command
    /// (`c=read; $c x` reads into `x`). Values are judged as lexed, so a
    /// copy of a counter (`j=$i`) is not mistaken for command output.
    pub(super) fn note(&mut self, simple: &shell::SimpleCommand, words: &[String]) {
        // What a lookup printed (`$(readlink link)`) is output, not the
        // operand it stands for.
        let raw: Vec<String> = simple
            .words
            .iter()
            .enumerate()
            .map(|(index, word)| {
                if simple.looked_up.contains(&index) {
                    format!("{word}{}", shell::UNSEEN_OUTPUT)
                } else {
                    word.clone()
                }
            })
            .collect();
        self.output_seen |= raw.iter().any(|word| command_output(word));
        self.glob_subst |= words
            .iter()
            .chain(&raw)
            .any(|word| shell::glob_substitution(word))
            || renames_shell(&raw)
            // A name built by expansion (`n=options; read -A $n`).
            || simple.words.iter().zip(words).any(|(raw, word)| {
                work::step();
                raw != word && word.to_ascii_lowercase().contains("options")
            });
        self.qualifier_words.extend(
            words
                .iter()
                .filter(|word| !all_qualifier_lists(word).is_empty())
                .cloned(),
        );
        // Redirect targets (`read x < "$f"`) are files, not operands.
        let mut redirect = vec![false; raw.len()];
        for index in simple
            .stdin_from
            .iter()
            .chain(&simple.here_string)
            .chain(&simple.writes_to)
        {
            work::step();
            if let Some(slot) = redirect.get_mut(*index) {
                *slot = true;
            }
        }
        let mut start = 0;
        let mut eval_scanned = false;
        // `eval let v`, zsh `noglob let v`: the rest is a command too. Each
        // level notes only what precedes its command, so this is linear.
        // #988: so is a `trap` action, which the shell reads again later.
        while start < raw.len().min(words.len()) {
            work::step();
            let raw = &raw[start..];
            let command = indirect::command_word(&words[start..]);
            match command {
                Some((index, name))
                    if matches!(name.as_str(), "eval" | "trap" | "noglob" | "nocorrect")
                        && !self.functions.contains(&name)
                        && !self.aliases.contains_key(&name) =>
                {
                    // Escapes (`eval \(\(v\)\)`) or `$'...'` decoding hide
                    // the evaluated text from the lexer.
                    let evaluates = matches!(name.as_str(), "eval" | "trap");
                    if evaluates && (simple.escaped || self.ansi_c) {
                        self.opaque = true;
                    }
                    if evaluates {
                        self.evaluates_text = true;
                        // `eval 'print ${'$t'x}'`: an expansion in the text
                        // may spell `${~x}` (#1084). Scanned at the first
                        // `eval` only: later levels see a suffix (linear).
                        if !eval_scanned {
                            eval_scanned = true;
                            self.glob_subst |= words[start + index + 1..].iter().any(|word| {
                                work::step();
                                word.contains(['$', '`']) || word.contains(shell::UNSEEN_OUTPUT)
                            });
                        }
                    }
                    self.note_one(&raw[..index], &redirect[start..], /*command*/ None);
                    start += index + 1;
                    // `trap -- 'let v' EXIT`: the action follows its options.
                    while name == "trap"
                        && let Some(option) = words.get(start)
                        && option.len() > 1
                        && option.starts_with('-')
                    {
                        work::step();
                        start += 1;
                        if option == "--" {
                            break;
                        }
                    }
                }
                command => {
                    self.note_one(raw, &redirect[start..], command.as_ref());
                    break;
                }
            }
        }
    }

    fn note_one(&mut self, words: &[String], redirect: &[bool], command: Option<&(usize, String)>) {
        let end = command.map_or(words.len(), |(index, _)| *index);
        // Assignments before the command (`IFS= read`, `X=1 cmd`), and all
        // words when there is no command.
        for word in &words[..end] {
            work::step();
            if let Some((name, value)) = assignment(word) {
                self.assigned.push((name.to_string(), value.to_string()));
                if appends(word) {
                    self.appended.insert(name.to_string());
                }
            }
        }
        // `: ${a:=$(cat f)}`, `${a=...}`, zsh `${a::=...}` assign `a`; the
        // lexer splits the value at spaces, so it may be any later word.
        let defaulted: Vec<String> = words.iter().flat_map(|word| defaults(word)).collect();
        if !defaulted.is_empty() {
            let node = self.group(words.iter().cloned());
            for name in defaulted {
                work::step();
                self.assigned.push((name, node.clone()));
            }
        }
        let Some((index, name)) = command else {
            return;
        };
        let args = &words[index + 1..];
        let operand = |at: usize| !redirect.get(index + 1 + at).copied().unwrap_or(false);
        // `$_` is the previous command's last word.
        let last = self.group(words[*index..].last().cloned());
        self.underscore.insert(last.clone());
        self.assigned.push(("_".to_string(), last));
        // Builtins and functions may write any variable they are given
        // (zsh `zstyle -s`, `zformat -f`, `getln`): once the line is tainted,
        // a name operand of a command not known to leave it alone may hold
        // anything.
        // `(( n + m ))` lexes as the command `n`: a word made only of
        // arithmetic operators marks arithmetic rather than a command
        // (known writers always count).
        let arithmetic_like = args.iter().any(|word| {
            !word.is_empty()
                && word != "--"
                && word.bytes().all(|byte| b"+-*/%<>=!&|^~?:,".contains(&byte))
        });
        if VARIABLE_WRITERS.contains(&name.as_str()) {
            // Like `read`: they fill names from outside data.
            self.filled.extend(
                args.iter()
                    .enumerate()
                    .filter(|(at, word)| operand(*at) && is_identifier(word))
                    .map(|(_, word)| word.clone()),
            );
        }
        if !OPERAND_SAFE.contains(&name.as_str()) && !arithmetic_like {
            self.operands.extend(
                args.iter()
                    .enumerate()
                    .filter(|(at, word)| operand(*at) && is_identifier(word))
                    .map(|(_, word)| word.clone()),
            );
        }
        // `alias L='let v'`: the definition is judged as the command it
        // stands for, and later `L` commands as that command.
        if name == "alias" && self.ansi_c {
            // `alias L=$'let \x76'`: the decoded body is one word.
            self.opaque = true;
        }
        if name == "alias" {
            self.evaluates_text = true;
            for (at, word) in args.iter().enumerate() {
                work::step();
                let Some((alias, value)) = word.split_once('=') else {
                    continue;
                };
                let mut aliased = vec![value.to_string()];
                aliased.extend(
                    args[at + 1..]
                        .iter()
                        .take_while(|word| !word.contains('='))
                        .cloned(),
                );
                if let Some((first, target)) = indirect::command_word(&aliased)
                    && target != "alias"
                {
                    self.note_one(&aliased, &[], Some(&(first, target.clone())));
                    // Command names are compared lowercased.
                    self.aliases.insert(alias.to_lowercase(), target);
                }
            }
        }
        let name = self
            .aliases
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.clone());
        let name = name.as_str();
        if words[*index].contains('$') && evaluates(name) {
            self.opaque = true;
        }
        if matches!(name, "source" | ".") {
            self.sources = true;
        }
        if sets_unnamed_options(name, args) {
            self.glob_subst = true;
        }
        // `export ARG${:-V0}=sh`: a declared name the reader cannot
        // complete may be `ARGV0` or `options`.
        if DECLARING_BUILTINS.contains(&name)
            && args.iter().any(|word| {
                work::step();
                let declared = word.split('=').next().unwrap_or(word);
                !word.starts_with(['-', '+'])
                    && (declared.contains(['$', '`']) || declared.contains(shell::UNSEEN_OUTPUT))
            })
        {
            self.glob_subst = true;
        }
        if matches!(
            name,
            "read" | "select" | "mapfile" | "readarray" | "vared" | "sysread"
        ) {
            self.filled
                .extend(READ_DEFAULTS.iter().map(|name| (*name).to_string()));
        }
        if name == "getopts" {
            // `OPTARG` comes from the arguments.
            self.assigned.push(("OPTARG".to_string(), "$@".to_string()));
        }
        if matches!(name, "[[" | "test" | "[") && args.iter().any(|word| word == "=~") {
            let node = self.group(args.iter().cloned());
            for name in MATCH_DEFAULTS {
                work::step();
                self.assigned.push(((*name).to_string(), node.clone()));
            }
        }
        if NUMERIC_BUILTINS.contains(&name) {
            self.evaluated.extend(args.iter().cloned());
            // Not options (`ulimit -n 1024`); `let -x` and `exit -x` negate `x`.
            let options = matches!(name, "ulimit" | "limit" | "unlimit");
            self.strict_words.extend(
                args.iter()
                    .filter(|word| {
                        !(options
                            && word.len() > 1
                            && word.starts_with('-')
                            && word[1..].bytes().all(|byte| byte.is_ascii_alphabetic()))
                    })
                    .cloned(),
            );
        }
        let operator = |word: &String| {
            word == "-v" || (TEST_OPERATORS.contains(&word.as_str()) && (name == "[[" || self.ksh))
        };
        if matches!(name, "test" | "[" | "[[") && args.iter().any(operator) {
            self.evaluated.extend(args.iter().cloned());
            for (at, word) in args.iter().enumerate() {
                work::step();
                if word == "-v" {
                    // `-v a[sub]` evaluates the subscript.
                    if let Some(next) = args.get(at + 1)
                        && let Some(open) = next.find('[')
                    {
                        self.strict_words.push(next[open..].to_string());
                    }
                } else if operator(word) {
                    self.strict_words.extend(
                        at.checked_sub(1)
                            .and_then(|before| args.get(before))
                            .into_iter()
                            .chain(args.get(at + 1))
                            .cloned(),
                    );
                }
            }
        }
        if matches!(name, "printf" | "print") {
            self.printf(name, args);
        }
        if name == "read" {
            self.read_options(args);
        }
        if NAMING_BUILTINS.contains(&name) {
            self.named.extend(
                args.iter()
                    .enumerate()
                    .filter(|(at, word)| operand(*at) && !word.starts_with(['-', '+']))
                    .map(|(_, word)| word)
                    .map(|word| word.split_once('=').map_or(word.as_str(), |(name, _)| name))
                    .map(str::to_string),
            );
        }
        if FILLING_BUILTINS.contains(&name) {
            // zsh/ksh `read 'v?prompt'`, zparseopts `a:=arr`, and an
            // array name attached to its option (`read -raarr`, `-Aarr`).
            for (_, word) in args.iter().enumerate().filter(|(at, _)| operand(*at)) {
                work::step();
                if let Some(cluster) = word.strip_prefix('-')
                    && let Some(at) = cluster.find(['a', 'A'])
                    && is_identifier(&cluster[at + 1..])
                {
                    self.filled.insert(cluster[at + 1..].to_string());
                }
                let prompted = word.split_once('?').map_or(word.as_str(), |(name, _)| name);
                let spec = word.rsplit_once('=').map(|(_, name)| name);
                for candidate in std::iter::once(prompted).chain(spec) {
                    work::step();
                    if is_identifier(candidate) {
                        self.filled.insert(candidate.to_string());
                    }
                }
            }
        }
        if DECLARING_BUILTINS.contains(&name) {
            let integer = matches!(name, "integer" | "float")
                || args.iter().any(|word| {
                    word.len() > 1
                        && word.starts_with(['-', '+'])
                        && word[1..].contains(['i', 'F', 'E'])
                });
            let flagged = |flags: &[char]| {
                args.iter().any(|word| {
                    word.len() > 1 && word.starts_with(['-', '+']) && word[1..].contains(flags)
                })
            };
            let nameref = flagged(&['n']);
            // Case conversion (`-l`, `-u`), justification (`-L`, `-R`,
            // `-Z`) and zsh `-T` (joined with a separator) rebuild values.
            let rebuilt = flagged(&['l', 'u', 'L', 'R', 'Z', 'T']);
            for word in args {
                work::step();
                let declared = assignment(word)
                    .map(|(name, _)| name)
                    .or_else(|| is_identifier(word).then_some(word.as_str()));
                if let Some(declared) = declared {
                    if rebuilt {
                        self.appended.insert(declared.to_string());
                    }
                    if nameref {
                        self.namerefs.insert(declared.to_string());
                    }
                }
                let Some((name, value)) = assignment(word) else {
                    if integer && is_identifier(word) {
                        self.integers.insert(word.clone());
                    }
                    continue;
                };
                self.assigned.push((name.to_string(), value.to_string()));
                if appends(word) {
                    self.appended.insert(name.to_string());
                }
                // bash `declare -n r=$v`: expanding `$r` evaluates the
                // subscript in the name it refers to, like `${!v}`.
                if nameref {
                    self.named.push(value.to_string());
                }
                if integer {
                    self.integers.insert(name.to_string());
                    self.evaluated.push(value.to_string());
                    self.strict_words.push(value.to_string());
                }
            }
        }
        if matches!(name, "for" | "select" | "foreach") {
            // `for a b in ...` (zsh) fills each name from the values.
            let mut variables: Vec<String> = args
                .iter()
                .take_while(|word| word.as_str() != "in" && is_identifier(word))
                .cloned()
                .collect();
            let values = &args[variables.len()..];
            // The `in` keyword is not a value.
            let values = values
                .split_first()
                .filter(|(first, _)| first.as_str() == "in")
                .map_or(values, |(_, rest)| rest);
            // `for x; do`, `for x do`: the values are `"$@"`.
            let node = if let Some(at) = variables.iter().position(|word| word == "do") {
                variables.truncate(at);
                self.group(std::iter::once("$@".to_string()))
            } else if values.is_empty() {
                self.group(std::iter::once("$@".to_string()))
            } else {
                self.group(values.iter().cloned())
            };
            for variable in variables {
                work::step();
                self.assigned.push((variable, node.clone()));
            }
        }
        if name == "set" {
            // zsh/ksh `set -A a values...`: every array named gets one
            // group of all the words, so repeated `-A` stays linear.
            let mut node = None;
            for pair in args.windows(2) {
                work::step();
                // `-A`, `+A`, or in a cluster (`-sA`).
                let array = pair[0].len() > 1
                    && pair[0].starts_with(['-', '+'])
                    && pair[0][1..].contains('A');
                if array && is_identifier(&pair[1]) {
                    let node = node
                        .get_or_insert_with(|| self.group(args.iter().cloned()))
                        .clone();
                    self.assigned.push((pair[1].clone(), node));
                }
            }
        }
        if name == "set" || self.functions.contains(name) {
            for value in args {
                work::step();
                for positional in POSITIONAL {
                    work::step();
                    self.assigned
                        .push(((*positional).to_string(), value.clone()));
                }
            }
        }
    }

    /// `printf`/`print -f` with a numeric conversion evaluates its operands
    /// (zsh, ksh); `-v NAME` names a variable.
    fn printf(&mut self, name: &str, args: &[String]) {
        if args.iter().any(|word| numeric_format(word)) {
            self.evaluated.extend(args.iter().cloned());
        }
        // The variables hold what it prints (any operand, for linearity).
        let mut node: Option<String> = None;
        // `-v NAME`, or attached or in a cluster: `-vNAME`, zsh `-rv NAME`,
        // `-rvNAME`.
        let mut targets: Vec<String> = Vec::new();
        // Where the format is: printf's first operand, or after print's `-f`.
        let mut format: Option<usize> = None;
        let mut at = 0;
        while at < args.len() {
            work::step();
            let word = &args[at];
            let cluster = word
                .strip_prefix('-')
                .filter(|cluster| !cluster.is_empty() && *cluster != "-");
            let Some(cluster) = cluster else {
                if name == "printf" && format.is_none() && word != "--" {
                    format = Some(at);
                }
                if word == "--" && name == "printf" {
                    format = format.or(Some(at + 1));
                }
                at += 1;
                continue;
            };
            // zsh `print` reads options after `-f FORMAT`; `printf '%s' -v x`
            // prints `-v`, which only over-matches.
            if let Some(v) = cluster.find('v') {
                let attached = &cluster[v + 1..];
                if attached.is_empty() {
                    targets.extend(args.get(at + 1).cloned());
                    at += 1;
                } else {
                    targets.push(attached.to_string());
                }
            } else if name == "print" && format.is_none() && cluster.ends_with('f') {
                format = Some(at + 1);
                at += 1;
            }
            at += 1;
        }
        targets.retain(|target| !target.is_empty());
        // The lexer splits a quoted format at blanks (`'%s %d'` reads `%s`,
        // `%d`), so where the format ends is unknown: every later word may be
        // an operand, read without its conversions and escapes.
        let numeric = args.iter().any(|word| numeric_format(word));
        if let Some(format) = format
            && let Some(text) = args.get(format)
        {
            // A format from a variable may convert anything numerically.
            let variable = text.contains('$') || text.contains('`');
            if numeric || variable {
                if variable {
                    self.strict_words.push(text.clone());
                }
                self.strict_words
                    .extend(args[format + 1..].iter().map(|word| format_text(word)));
            }
        } else if numeric {
            let first = args.iter().position(|word| word.contains('%')).unwrap_or(0);
            self.strict_words
                .extend(args[first + 1..].iter().map(|word| format_text(word)));
        }
        for target in &targets {
            work::step();
            self.named.push(target.clone());
            if let Some((name, _)) = assignment(&format!("{target}=")) {
                let node = node
                    .get_or_insert_with(|| self.group(args.iter().cloned()))
                    .clone();
                self.assigned.push((name.to_string(), node));
                // The format joins operands (`%s%s va r` prints `var`).
                self.appended.insert(name.to_string());
            }
        }
    }

    /// `read -t N`, `-n N`, `-k N`, `-u N`: numbers (attached or not).
    fn read_options(&mut self, args: &[String]) {
        let mut take_next = false;
        for word in args {
            work::step();
            if take_next {
                self.evaluated.push(word.clone());
                self.strict_words.push(word.clone());
                take_next = false;
                continue;
            }
            let Some(flags) = word.strip_prefix('-') else {
                continue;
            };
            if let Some(at) = flags.find(READ_NUMERIC_OPTIONS) {
                let value = &flags[at + 1..];
                if value.is_empty() {
                    take_next = true;
                } else {
                    self.evaluated.push(value.to_string());
                    self.strict_words.push(value.to_string());
                }
            }
        }
    }

    /// Whether the line may run code from a zsh glob qualifier (#1028): a
    /// parenthesised group attached to a word that zsh would parse as
    /// qualifiers reaching an `e` one (`*(e:code:)`, `*(eXcodeX)`), a `+`
    /// one (`*(+func)`), a sort by either (`*(oe:code:)`) or an expansion
    /// (`*($x)`, whose value zsh parses as qualifiers). Read without
    /// quotes, and again without backslashes (`x=/\\(e:c:\\)` used by
    /// `$~x`), so nested shells' scripts, `trap` and `functions[...]` text,
    /// and values later expanded count; lexed (decoded) words count too.
    /// With or without command output on the line. `text` is the line's
    /// argv.
    pub(super) fn runs_qualifier_code(&self, text: &[String]) -> bool {
        let runs = |text: &str| {
            all_qualifier_lists(text).into_iter().any(|range| {
                // A group left open may hold either. The group may also end
                // later than read without quotes, at the first `)` outside
                // quotes (`e:'a)':`, `e:a\\):`): both are read.
                if range.is_empty() && range.start == text.len() {
                    return true;
                }
                let list = &text[range.clone()];
                qualifier_runs_code(list)
                    || (list.contains(['\'', '"', '\\'])
                        && quoted_group_end(text, range.start)
                            .is_none_or(|end| qualifier_runs_code(&text[range.start..end])))
            })
        };
        text.iter()
            .map(|arg| joined(arg))
            .chain(self.qualifier_words.iter().cloned())
            .any(|text| runs(&text) || (text.contains('\\') && runs(&text.replace('\\', ""))))
    }

    /// Whether the line may use zsh glob substitution (#1084), under which
    /// a value from anywhere (command output, input, the environment, a
    /// file) is read as a pattern whose qualifiers may run code: a spelling
    /// in the argv or a lexed word ([`shell::glob_substitution`]), a write
    /// to zsh's `options` array, or a command that may turn on options the
    /// reader cannot name. With or without command output on the line.
    /// `text` is the line's argv.
    pub(super) fn uses_glob_substitution(&self, text: &[String]) -> bool {
        let options = |name: &str| name == "options";
        self.glob_subst
            || text
                .iter()
                .any(|arg| shell::glob_substitution(&joined(arg)))
            || self.assigned.iter().any(|(name, _)| options(name))
            || self
                .filled
                .iter()
                .chain(&self.appended)
                .chain(&self.namerefs)
                .any(|name| options(name))
            || self
                .named
                .iter()
                .any(|word| options(word.split(['[', '=']).next().unwrap_or(word)))
    }

    /// Whether the line evaluates, as arithmetic, a variable filled from
    /// command output or input, or command output itself. `text` is the
    /// line's argv.
    pub(super) fn runs_unseen(&self, text: &[String]) -> bool {
        if self.unfollowed {
            return true;
        }
        let (tainted, any) = self.tainted();
        // Operands the lexer cannot see, a sourced file, or text the shell
        // reads again (`eval "(""(v))"`, aliases) may use them.
        if (self.opaque || self.sources || self.evaluates_text) && any {
            return true;
        }
        let builds = |value: &str| any && self.builds(value);
        let tainted_value = |value: &str| {
            command_output(value)
                || builds(value)
                || names(&visible(value)).any(|name| tainted.contains(name))
        };
        if self.integers.iter().any(|name| tainted.contains(name))
            || self.evaluated.iter().any(|word| tainted_value(word))
            || self
                .named
                .iter()
                .chain(
                    self.assigned
                        .iter()
                        .filter(|(name, _)| self.namerefs.contains(name))
                        .map(|(_, value)| value),
                )
                .any(|word| {
                    command_output(word)
                        || builds(word)
                        || references(word).any(|name| tainted.contains(name))
                        || word.find('[').is_some_and(|at| tainted_value(&word[at..]))
                })
        {
            return true;
        }
        text.iter().any(|arg| {
            let arg = joined(arg);
            regions(&arg).into_iter().any(|range| {
                let region = &arg[range];
                // Quote removal joins names (`va""r`).
                let bare = unquoted(region);
                names(&bare).any(|name| tainted.contains(name))
                    || unseen_substitution(region)
                    || (any && {
                        let (builders, unknown) = building(region);
                        unknown || !builders.is_empty()
                    })
            })
        }) || (any && self.strict_unseen(text, &tainted))
    }

    /// The #937 fallback (review round 6): once the line holds command
    /// output or input, arithmetic may read only variables whose every value
    /// on the line is a number, an arithmetic result or a copy of such a
    /// variable. Anything else (the environment, positional parameters,
    /// variables written by commands the reader does not follow) may hold
    /// the output.
    fn strict_unseen(&self, text: &[String], tainted: &HashSet<String>) -> bool {
        // Quote removal can form a region (`(""(v))` reads `((v))`).
        // The argv texts, then those that quote removal changes.
        let mut texts: Vec<String> = text.iter().map(|arg| joined(arg)).collect();
        let originals = texts.len();
        for at in 0..originals {
            work::step();
            let bare = unquoted(&texts[at]);
            if bare != texts[at] {
                texts.push(bare);
            }
        }
        let found: Vec<(usize, Range<usize>)> = texts
            .iter()
            .enumerate()
            .flat_map(|(at, text)| regions(text).into_iter().map(move |range| (at, range)))
            .collect();
        // Where each name is first assigned and first read, in line order: a
        // variable read before the line assigns it (`echo $((i)); i=0`), or
        // whose first assignment reads it (`i=$((i+1))`, `((i++))`), holds a
        // value from outside the line.
        let mut results: HashSet<String> = HashSet::new();
        let mut first_assigned: HashMap<&str, (usize, usize)> = HashMap::new();
        let mut first_read: HashMap<&str, (usize, usize)> = HashMap::new();
        let mut self_reads: HashSet<&str> = HashSet::new();
        for (at, text) in texts[..originals].iter().enumerate() {
            work::step();
            let bytes = text.as_bytes();
            // How many regions cover each byte, and where each one ends.
            let mut depth = vec![0_i32; bytes.len() + 1];
            let mut ends: HashMap<usize, usize> = HashMap::new();
            let mut plain: HashSet<usize> = HashSet::new();
            for (_, range) in found.iter().filter(|(text_at, _)| *text_at == at) {
                work::step();
                depth[range.start] += 1;
                depth[range.end] -= 1;
                ends.entry(range.start).or_insert(range.end);
                let region = &text[range.clone()];
                if pure_arithmetic(region) {
                    for (offset, name) in arithmetic_assignments(region) {
                        work::step();
                        plain.insert(range.start + offset);
                        results.insert(name.to_string());
                    }
                }
            }
            let mut inside = Vec::with_capacity(bytes.len());
            let mut open = 0;
            for delta in &depth[..bytes.len()] {
                work::step();
                open += delta;
                inside.push(open > 0);
            }
            for name in names(text) {
                work::step();
                let start = name.as_ptr() as usize - text.as_ptr() as usize;
                let end = start + name.len();
                let before = &text[..start];
                let after = &text[end..];
                let reference = before.ends_with('$')
                    || before.ends_with("${")
                    || before.ends_with("${#")
                    || before.ends_with("${!")
                    || before.ends_with("$#");
                let assignment = !reference
                    && ((after.starts_with('=') && !after.starts_with("=="))
                        || before.trim_end().ends_with("for")
                        || before.trim_end().ends_with("select"));
                let position = (at, start);
                if plain.contains(&start) || (assignment && !inside[start]) {
                    if !first_assigned.contains_key(name) {
                        first_assigned.insert(name, position);
                        // The value it is given, up to a blank, or a whole
                        // region (`i=$(( i + 1 ))`), looking a bounded way.
                        let value = after.strip_prefix('=').unwrap_or("");
                        let value_at = end + (after.len() - value.len());
                        let window = match ends.get(&value_at) {
                            Some(region_end) => &text[value_at..*region_end],
                            None => match value.bytes().take(MAX_SUBSCRIPT).position(|byte| {
                                byte.is_ascii_whitespace() || b";&|".contains(&byte)
                            }) {
                                Some(stop) => &value[..stop],
                                None if value.len() <= MAX_SUBSCRIPT => value,
                                None => name,
                            },
                        };
                        if names(window).any(|read| read == name) {
                            self_reads.insert(name);
                        }
                    }
                } else if reference || inside[start] {
                    first_read.entry(name).or_insert(position);
                }
            }
        }
        let read_first: HashSet<String> = first_read
            .iter()
            .filter(|(name, read)| {
                self_reads.contains(*name)
                    || first_assigned
                        .get(*name)
                        .is_none_or(|assigned| *read < assigned)
            })
            .map(|(name, _)| (*name).to_string())
            .chain(self_reads.iter().map(|name| (*name).to_string()))
            .collect();
        let safe = self.numeric_names(tainted, &results, &read_first);
        // Substitutions are judged on their own (`unseen_substitution`): a
        // readable one (`$(date +%s)`) prints a fact, not a name.
        let unsafe_in =
            |text: &str| names(&without_substitutions(text)).any(|name| !safe.contains(name));
        let unsafe_word = |word: &str| command_output(word) || unsafe_in(&unquoted(&visible(word)));
        self.strict_words.iter().any(|word| unsafe_word(word))
            || self
                .assigned
                .iter()
                .filter(|(name, _)| self.integers.contains(name))
                .any(|(_, value)| unsafe_word(value))
            || self
                .named
                .iter()
                .chain(
                    self.assigned
                        .iter()
                        .filter(|(name, _)| self.namerefs.contains(name))
                        .map(|(_, value)| value),
                )
                .any(|word| {
                    let word = visible(word);
                    references(&word).any(|name| !safe.contains(name))
                        || word
                            .find('[')
                            .is_some_and(|open| unsafe_in(&unquoted(&word[open..])))
                })
            // D57: a glob-qualifier list that reads a name or command output,
            // wherever in the list (`/*(.[w])`, `/*(om[w])`, `/*(e:code:)`).
            || texts[..originals]
                .iter()
                .chain(&self.qualifier_words)
                .any(|text| {
                    qualifier_lists(text).into_iter().any(|range| {
                        // A group left open asks.
                        let open = range.is_empty() && range.start == text.len();
                        let list = &text[range];
                        open
                            || unsafe_in(&unquoted(list))
                            || unseen_substitution(list)
                            || !only_facts(list)
                    })
                })
            || found.iter().any(|(at, range)| {
                let text = &texts[*at];
                let region = &text[range.clone()];
                if region.starts_with("[[") {
                    condition_unsafe(region, &unsafe_in)
                } else if region.starts_with('[') {
                    subscript_context(text, range) && unsafe_in(&unquoted(region))
                } else if region.starts_with("${!") {
                    !index_listing(region) && unsafe_in(&unquoted(region))
                } else if region.starts_with("${(") {
                    flags_unsafe(region, &unsafe_in)
                } else {
                    unsafe_in(&unquoted(region))
                }
            })
    }

    /// Variables whose every value on the line is a number, an arithmetic
    /// result, a length, or a copy of such a variable (a greatest fixed
    /// point, computed by walking from the variables that fail, so linear),
    /// plus special parameters that always hold numbers when the line does
    /// not assign them. Variables in `read_first` (read before the line
    /// assigns them, or reading themselves in their first assignment) are
    /// not numeric.
    fn numeric_names(
        &self,
        tainted: &HashSet<String>,
        results: &HashSet<String>,
        read_first: &HashSet<String>,
    ) -> HashSet<String> {
        let assigned: HashSet<&str> = self
            .assigned
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        let excluded = |name: &str| {
            tainted.contains(name)
                || self.filled.contains(name)
                || self.operands.contains(name)
                || self.appended.contains(name)
                || self.namerefs.contains(name)
        };
        let candidate = |name: &str| {
            !excluded(name)
                && (((assigned.contains(name) || results.contains(name))
                    && !read_first.contains(name))
                    || (NUMERIC_SPECIALS.contains(&name) && !assigned.contains(name)))
        };
        let mut failed: HashSet<String> = HashSet::new();
        let mut receivers: HashMap<&str, Vec<&str>> = HashMap::new();
        for (name, value) in &self.assigned {
            work::step();
            match numeric_references(value) {
                Some(sources) => {
                    for source in sources {
                        work::step();
                        if candidate(source) {
                            receivers.entry(source).or_default().push(name);
                        } else {
                            failed.insert(name.clone());
                        }
                    }
                }
                None => {
                    failed.insert(name.clone());
                }
            }
        }
        let mut queue: Vec<String> = failed.iter().cloned().collect();
        while let Some(source) = queue.pop() {
            work::step();
            for receiver in receivers.get(source.as_str()).into_iter().flatten() {
                work::step();
                if failed.insert((*receiver).to_string()) {
                    queue.push((*receiver).to_string());
                }
            }
        }
        assigned
            .iter()
            .copied()
            .chain(results.iter().map(String::as_str))
            .chain(NUMERIC_SPECIALS.iter().copied())
            .filter(|name| candidate(name) && !failed.contains(*name))
            .map(str::to_string)
            .collect()
    }

    /// Variables whose value is unseen: set from command output, filled
    /// from input, or assigned a value that mentions such a variable.
    /// Also whether the line is tainted by more than `$_`, which turns on
    /// the checks for names built from fragments.
    fn tainted(&self) -> (HashSet<String>, bool) {
        let mut tainted: HashSet<String> = self.filled.clone();
        if self.function_substitution {
            tainted.insert(FUNCTION_SUBSTITUTION.to_string());
        }
        // Who receives each name's value: a graph walk keeps this linear.
        let mut receivers: HashMap<String, Vec<&str>> = HashMap::new();
        for (name, value) in &self.assigned {
            work::step();
            // A lexed value still holding `$(` or a backtick was quoted or
            // decoded (`$'\x24\x28'`): arithmetic would run it.
            if command_output(value) || value.contains("$(") || value.contains('`') {
                tainted.insert(name.clone());
            }
            for source in names(&visible(value)) {
                work::step();
                receivers.entry(source.to_string()).or_default().push(name);
            }
        }
        // Once anything is tainted, a value built from name fragments may
        // name it.
        // The #937 fallback widens this to any line holding command output,
        // even only as an argument (held by `$_`).
        let any = self.output_seen || tainted.iter().any(|name| !self.underscore.contains(name));
        // Command output anywhere (even only as an argument, held by `$_`)
        // may reach the variables shells fill on their own (`match`).
        if self.output_seen || !tainted.is_empty() {
            tainted.extend(FILLED_DEFAULTS.iter().map(|name| (*name).to_string()));
        }
        if any && self.anonymous_function {
            tainted.extend(POSITIONAL.iter().map(|name| (*name).to_string()));
        }
        if any {
            tainted.extend(self.appended.iter().cloned());
            tainted.extend(self.operands.iter().cloned());
            // Allow-list: a value is clean only if it is made of numbers,
            // operators, bare names, arithmetic results and whole `$NAME`s
            // of variables the line assigns (whose cleanliness the graph
            // walk below decides).
            let known: HashSet<&str> = self
                .assigned
                .iter()
                .map(|(name, _)| name.as_str())
                .chain(self.filled.iter().map(String::as_str))
                .collect();
            for (name, value) in &self.assigned {
                work::step();
                if self.builds(value) || !clean_form(value, &known) {
                    tainted.insert(name.clone());
                }
            }
        }
        let mut queue: Vec<String> = tainted.iter().cloned().collect();
        while let Some(source) = queue.pop() {
            work::step();
            for receiver in receivers.get(&source).into_iter().flatten() {
                work::step();
                if tainted.insert((*receiver).to_string()) {
                    queue.push((*receiver).to_string());
                }
            }
        }
        (tainted, any)
    }

    /// #932 measuring: which reading of `runs_unseen` holds, with the region.
    #[cfg(test)]
    pub(super) fn runs_unseen_why(&self, text: &[String]) -> String {
        if self.unfollowed {
            return "unfollowed".into();
        }
        let (tainted, any) = self.tainted();
        if (self.opaque || self.sources || self.evaluates_text) && any {
            return format!(
                "opaque_sources_evaltext:o{}s{}e{}",
                u8::from(self.opaque),
                u8::from(self.sources),
                u8::from(self.evaluates_text)
            );
        }
        let builds = |value: &str| any && self.builds(value);
        let tainted_value = |value: &str| {
            command_output(value)
                || builds(value)
                || names(&visible(value)).any(|name| tainted.contains(name))
        };
        if self.integers.iter().any(|name| tainted.contains(name)) {
            return "integers".into();
        }
        if let Some(w) = self.evaluated.iter().find(|word| tainted_value(word)) {
            return format!("evaluated {w:?}");
        }
        if self.named.iter().any(|word| {
            command_output(word)
                || builds(word)
                || references(word).any(|name| tainted.contains(name))
                || word.find('[').is_some_and(|at| tainted_value(&word[at..]))
        }) {
            return "named".into();
        }
        for arg in text {
            work::step();
            let arg = joined(arg);
            for range in regions(&arg) {
                work::step();
                let region = &arg[range];
                let bare = unquoted(region);
                if names(&bare).any(|name| tainted.contains(name)) {
                    return format!("region_tainted_name {region:?}");
                }
                if unseen_substitution(region) {
                    return format!("region_substitution {region:?}");
                }
                if any {
                    let (builders, unknown) = building(region);
                    if unknown || !builders.is_empty() {
                        return format!("region_building {region:?}");
                    }
                }
            }
        }
        if any && self.strict_unseen(text, &tainted) {
            return "strict_unseen".into();
        }
        "namerefs_or_other".into()
    }

    /// #932 measuring: the argument text (or lexed word) that reads as a
    /// qualifier that runs code.
    #[cfg(test)]
    pub(super) fn qualifier_trigger(&self, text: &[String]) -> Option<String> {
        let one = |t: &[String]| self.runs_qualifier_code_in(t);
        for arg in text {
            work::step();
            if one(std::slice::from_ref(arg)) {
                let j = joined(arg);
                for t in [j.clone(), j.replace('\\', "")] {
                    work::step();
                    for range in all_qualifier_lists(&t) {
                        work::step();
                        let hit = (range.is_empty() && range.start == t.len()) || {
                            let list = &t[range.clone()];
                            qualifier_runs_code(list)
                                || (list.contains(['\'', '"', '\\'])
                                    && quoted_group_end(&t, range.start).is_none_or(|end| {
                                        qualifier_runs_code(&t[range.start..end])
                                    }))
                        };
                        if hit {
                            let from = t[..range.start]
                                .char_indices()
                                .rev()
                                .nth(24)
                                .map_or(0, |(i, _)| i);
                            let to = t[range.end..]
                                .char_indices()
                                .nth(4)
                                .map_or(t.len(), |(i, _)| range.end + i);
                            return Some(format!(
                                "argv:{}|{}|{}",
                                &t[from..range.start],
                                &t[range.clone()],
                                &t[range.end..to]
                            ));
                        }
                    }
                }
                return Some("argv:?".to_string());
            }
        }
        for word in &self.qualifier_words {
            work::step();
            if one(std::slice::from_ref(word)) {
                return Some(format!("lexed:{word}"));
            }
        }
        None
    }

    /// #932 measuring: `runs_qualifier_code` over `text` alone.
    #[cfg(test)]
    fn runs_qualifier_code_in(&self, text: &[String]) -> bool {
        let runs = |text: &str| {
            all_qualifier_lists(text).into_iter().any(|range| {
                if range.is_empty() && range.start == text.len() {
                    return true;
                }
                let list = &text[range.clone()];
                qualifier_runs_code(list)
                    || (list.contains(['\'', '"', '\\'])
                        && quoted_group_end(text, range.start)
                            .is_none_or(|end| qualifier_runs_code(&text[range.start..end])))
            })
        };
        text.iter()
            .map(|arg| joined(arg))
            .any(|text| runs(&text) || (text.contains('\\') && runs(&text.replace('\\', ""))))
    }

    /// #932 measuring: why `uses_glob_substitution` holds.
    #[cfg(test)]
    pub(super) fn glob_subst_why(&self, text: &[String]) -> String {
        let options = |name: &str| name == "options";
        if self.glob_subst {
            return "lexed_glob_subst".into();
        }
        if let Some(arg) = text
            .iter()
            .find(|arg| shell::glob_substitution(&joined(arg)))
        {
            return format!("argv:{arg}");
        }
        if self.assigned.iter().any(|(name, _)| options(name))
            || self
                .filled
                .iter()
                .chain(&self.appended)
                .chain(&self.namerefs)
                .any(|name| options(name))
            || self
                .named
                .iter()
                .any(|word| options(word.split(['[', '=']).next().unwrap_or(word)))
        {
            return "options_array".into();
        }
        "-".into()
    }
}

impl Arithmetic {
    /// #1142: whether arithmetic may set a variable the classifier cannot
    /// name (`PAGER`, `HOME` built from fragments): an assignment whose name
    /// is built at run time (`(( $n = 1 ))`, `let "$n=1"`, `a[$n=1]`, `x=$n=1`
    /// for an integer `x`), or a variable whose value the line builds at run
    /// time and arithmetic evaluates (`m="$n=1"; (( m ))` assigns the
    /// variable `$n` names). `text` is the line's argv. Values from earlier
    /// lines are not seen.
    pub(super) fn assigns_unseen_name(&self, text: &[String]) -> bool {
        let text: Vec<String> = text.iter().map(|arg| unquoted(&joined(arg))).collect();
        // zsh `${(P)n=x}`/`${(P)n::=x}` and bash `${!n:=x}` assign the
        // variable `n` names (the lexer splits these words, so the text is
        // read).
        if text.iter().any(|arg| assigns_named_variable(arg)) {
            return true;
        }
        let evaluated: Vec<String> = self
            .evaluated
            .iter()
            .chain(
                self.assigned
                    .iter()
                    .filter(|(name, _)| self.integers.contains(name))
                    .map(|(_, value)| value),
            )
            .map(|word| unquoted(word))
            .collect();
        let regions: Vec<&str> = text
            .iter()
            .flat_map(|arg| regions(arg).into_iter().map(move |range| &arg[range]))
            .chain(evaluated.iter().map(String::as_str))
            .collect();
        if regions.iter().any(|region| built_assignment(region)) {
            return true;
        }
        let built = self.built_values();
        !built.is_empty()
            && regions
                .iter()
                .any(|region| names(&visible(region)).any(|name| built.contains(name)))
    }

    /// Variables whose value the line builds at run time from expansions
    /// arithmetic does not read as numbers (`n=${x/*/HO}ME=1`), and those
    /// whose value uses one (`m="$n=1"`, `k=$m`).
    fn built_values(&self) -> HashSet<String> {
        let known: HashSet<&str> = self
            .assigned
            .iter()
            .map(|(name, _)| name.as_str())
            .chain(self.filled.iter().map(String::as_str))
            .collect();
        let mut built: HashSet<String> = HashSet::new();
        let mut receivers: HashMap<String, Vec<&str>> = HashMap::new();
        for (name, value) in &self.assigned {
            work::step();
            let expands = value.contains('$') || value.contains('`') || command_output(value);
            // An `=` the value joins to an expansion (`m="$n=1"`) may form
            // an assignment. A group's words may be the defaults that fill
            // it (`${PORT:=8080}` arrives as `$PORT:=8080`), which do not.
            let joins_equals = || {
                if name.starts_with(GROUP) {
                    without_defaults(value).contains('=')
                } else {
                    value.contains('=')
                }
            };
            if expands && (!clean_form(value, &known) || joins_equals()) {
                built.insert(name.clone());
            }
            for source in names(&visible(value)) {
                work::step();
                receivers
                    .entry(source.to_string())
                    .or_default()
                    .push(name.as_str());
            }
        }
        let mut queue: Vec<String> = built.iter().cloned().collect();
        while let Some(source) = queue.pop() {
            work::step();
            for receiver in receivers.get(&source).into_iter().flatten() {
                work::step();
                if built.insert((*receiver).to_string()) {
                    queue.push((*receiver).to_string());
                }
            }
        }
        built
    }

    /// Whether lexed text (where `${x}r` reads `$xr`) expands a variable
    /// that the line builds names from, or joins an expansion to name text.
    fn builds(&self, lexed: &str) -> bool {
        let (joined, unknown) = building(lexed);
        unknown
            || !joined.is_empty()
            || lexed.match_indices('$').any(|(at, _)| {
                let rest = &lexed[at + 1..];
                let rest = rest.strip_prefix('{').unwrap_or(rest);
                let len = rest
                    .find(|ch: char| !is_name_char(ch))
                    .unwrap_or(rest.len());
                // Long names are not split up (that would be quadratic).
                len > 0
                    && (self.unknown_builder
                        || (len > MAX_BUILDER_PREFIX && !self.builders.is_empty())
                        || (1..=len.min(MAX_BUILDER_PREFIX))
                            .any(|end| self.builders.contains(&rest[..end])))
            })
    }
}

/// Whether a region is arithmetic through and through (`$((...))`,
/// `((...))`, `$[...]`, the offset of `${x:offset}`), where every name is a
/// variable.
fn pure_arithmetic(region: &str) -> bool {
    region.starts_with("$((")
        || region.starts_with("((")
        || region.starts_with("$[")
        || region.starts_with(':')
}

/// Text with its command substitutions (`$(...)`, `` `...` ``) blanked.
/// Arithmetic (`$((`) is kept. Linear: each byte is visited once.
fn without_substitutions(text: &str) -> Cow<'_, str> {
    if !text.contains('`') && !text.contains("$(") {
        return Cow::Borrowed(text);
    }
    let bytes = text.as_bytes();
    let mut kept = String::with_capacity(text.len());
    let mut at = 0;
    let mut from = 0;
    while at < bytes.len() {
        work::step();
        let substitution = bytes[at] == b'`'
            || (bytes[at] == b'$'
                && bytes.get(at + 1) == Some(&b'(')
                && bytes.get(at + 2) != Some(&b'('));
        if !substitution {
            at += 1;
            continue;
        }
        kept.push_str(&text[from..at]);
        kept.push(' ');
        if bytes[at] == b'`' {
            at = text[at + 1..]
                .find('`')
                .map_or(bytes.len(), |end| at + 1 + end + 1);
        } else {
            let mut depth = 0_usize;
            let mut end = bytes.len();
            for (offset, &byte) in bytes[at + 1..].iter().enumerate() {
                work::step();
                match byte {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = at + 1 + offset + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            at = end;
        }
        from = at;
    }
    kept.push_str(&text[from.min(text.len())..]);
    Cow::Owned(kept)
}

/// Whether every command substitution in text prints a neutral fact
/// (`$(date +%s)`, `$(git rev-parse HEAD)`), not text such as a name
/// (`$(echo w)`, `` `basename w` ``). Linear, like `without_substitutions`.
fn only_facts(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        work::step();
        let (body, end) = if bytes[at] == b'`' {
            let end = text[at + 1..]
                .find('`')
                .map_or(bytes.len(), |end| at + 1 + end);
            (&text[at + 1..end], end + 1)
        } else if bytes[at] == b'$'
            && bytes.get(at + 1) == Some(&b'(')
            && bytes.get(at + 2) != Some(&b'(')
        {
            let mut depth = 0_usize;
            let mut end = bytes.len();
            for (offset, &byte) in bytes[at + 1..].iter().enumerate() {
                work::step();
                match byte {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = at + 1 + offset;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            (&text[(at + 2).min(end)..end], end + 1)
        } else {
            at += 1;
            continue;
        };
        let commands = shell::simple_commands(&[body.to_string()]).commands;
        let fact = matches!(commands.as_slice(), [command]
        if matches!(
            shell::substitution_output(&command.words).as_deref(),
            Some("value" | "$PWD")
        ));
        if !fact {
            return false;
        }
        at = end;
    }
    true
}

/// Longest zsh flag group read (`${(P)n=x}`); a longer one counts.
const MAX_FLAGS: usize = 64;

/// Whether text assigns the variable another variable names: zsh
/// `${(P)n=x}`, `${(P)n::=x}`, bash `${!n=x}`, `${!n:=x}`. Over-matches
/// (`${(P)n:-x=y}`). Linear: each expansion looks at most `MAX_FLAGS`
/// bytes ahead for its flags, and its `=` is any later one.
fn assigns_named_variable(text: &str) -> bool {
    let Some(last_equals) = work::rfind(text, "=") else {
        return false;
    };
    text.match_indices("${").any(|(at, _)| {
        work::step();
        let rest = &text[at + 2..];
        if let Some(flags) = rest.strip_prefix('(') {
            // Bytes, so a window end inside a character cannot panic.
            let window = &flags.as_bytes()[..flags.len().min(MAX_FLAGS)];
            // Flags are ASCII letters and delimiters; an unclosed or long
            // group counts.
            return window
                .iter()
                .position(|&byte| byte == b')')
                .is_none_or(|close| {
                    window[..close].contains(&b'P') && last_equals > at + 3 + close
                });
        }
        rest.strip_prefix('!').is_some_and(|name| {
            let len = name
                .find(|ch: char| !is_name_char(ch))
                .unwrap_or(name.len());
            let after = &name[len..];
            len > 0 && (after.starts_with('=') || after.starts_with(":="))
        })
    })
}

/// Lexed text without the default assignments that fill a group
/// (`$a:=1`, `$a=1`, `$a::=1` lose their `$a:=`).
fn without_defaults(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        work::step();
        out.push_str(&rest[..at]);
        let body = &rest[at + 1..];
        let len = body
            .find(|ch: char| !is_name_char(ch))
            .unwrap_or(body.len());
        let tail = &body[len..];
        let marker = ["::=", ":=", "="]
            .into_iter()
            .find(|marker| len > 0 && tail.starts_with(marker));
        match marker {
            Some(marker) => rest = &tail[marker.len()..],
            None => {
                out.push('$');
                rest = body;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Whether arithmetic text assigns (`=`, `+=`, `<<=`...) to a name built
/// at run time (`$n = 1`, `${n}=1`, `$(cat f)=1`); a subscript of a plain
/// name (`a[$i]=1`) is not. Linear: each assignment looks back over its own
/// operand, and over at most `MAX_SUBSCRIPT` bytes of a subscript.
fn built_assignment(text: &str) -> bool {
    let bytes = text.as_bytes();
    for (at, _) in text.match_indices('=') {
        work::step();
        let before = |back: usize| at.checked_sub(back).map(|index| bytes[index]);
        if bytes.get(at + 1) == Some(&b'=') {
            continue;
        }
        let mut end = at;
        match before(1) {
            Some(b'=' | b'!') => continue,
            Some(op @ (b'<' | b'>')) => {
                if before(2) != Some(op) {
                    continue;
                }
                end -= 2;
            }
            Some(b'+' | b'-' | b'*' | b'/' | b'%' | b'&' | b'|' | b'^') => end -= 1,
            _ => {}
        }
        while end > 0 && bytes[end - 1].is_ascii_whitespace() {
            work::step();
            end -= 1;
        }
        // `name[subscript]`: the name is before the subscript.
        if end > 0 && bytes[end - 1] == b']' {
            let window = end.saturating_sub(MAX_SUBSCRIPT);
            let mut depth = 0_usize;
            let open = (window..end).rev().find(|&index| {
                work::step();
                match bytes[index] {
                    b']' => depth += 1,
                    b'[' => depth -= 1,
                    _ => {}
                }
                depth == 0
            });
            match open {
                Some(open) => end = open,
                None => return true,
            }
        }
        let mut start = end;
        while start > 0
            && !bytes[start - 1].is_ascii_whitespace()
            && !b"=+-*/%&|^<>!~?:,;()[".contains(&bytes[start - 1])
        {
            work::step();
            start -= 1;
        }
        let operand = &text[start..end];
        if operand.contains('$') || operand.contains('`') || command_output(operand) {
            return true;
        }
    }
    false
}

/// Plain assignments in arithmetic text (`i=0`, `i = 2`, not `i == 2`,
/// `i += 2` or `i = i + 1`): each name and its offset in the text.
fn arithmetic_assignments(text: &str) -> impl Iterator<Item = (usize, &str)> {
    names(text).filter_map(move |name| {
        let start = name.as_ptr() as usize - text.as_ptr() as usize;
        let rest = text[start + name.len()..].trim_start();
        let value = rest
            .strip_prefix('=')
            .filter(|value| !value.starts_with('='))?;
        // Up to the end of this expression, looking a bounded distance
        // (longer values are not counted, which is stricter).
        let end = match value
            .bytes()
            .take(MAX_SUBSCRIPT)
            .position(|byte| matches!(byte, b',' | b';' | b')'))
        {
            Some(end) => end,
            None if value.len() <= MAX_SUBSCRIPT => value.len(),
            None => return None,
        };
        (!names(&value[..end]).any(|read| read == name)).then_some((start, name))
    })
}

/// The variables a lexed value reads if it is only numbers, operators,
/// arithmetic results, lengths, special numbers (`$?`) and variables
/// (`$x`, `${x}`, bare `x`, which arithmetic reads too); `None` for
/// anything else (command output, strings, subscripts, `$@`, `$1`).
fn numeric_references(value: &str) -> Option<Vec<&str>> {
    if command_output(value) || value.contains("$(") || value.contains('`') {
        return None;
    }
    let bytes = value.as_bytes();
    let name_end = |from: usize| {
        let mut end = from;
        while bytes
            .get(end)
            .is_some_and(|&byte| is_name_char(byte as char))
        {
            work::step();
            end += 1;
        }
        end
    };
    let mut sources = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        work::step();
        if value[at..].starts_with(shell::ARITHMETIC_OUTPUT) {
            at += shell::ARITHMETIC_OUTPUT.len();
            continue;
        }
        let byte = bytes[at];
        match byte {
            b' ' | b'\t' | b'\n' | b'+' | b'-' | b'*' | b'/' | b'%' | b'(' | b')' | b'<' | b'>'
            | b'=' | b'!' | b'&' | b'|' | b'^' | b'~' | b',' | b'.' => at += 1,
            // A number (`10`, `0x1f`, `16#ff`, `1.5`).
            b'0'..=b'9' => {
                while bytes
                    .get(at)
                    .is_some_and(|&byte| is_name_char(byte as char) || matches!(byte, b'#' | b'.'))
                {
                    work::step();
                    at += 1;
                }
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                let end = name_end(at);
                sources.push(&value[at..end]);
                at = end;
            }
            b'$' => {
                let rest = &value[at + 1..];
                if let Some(indexed) = rest.strip_prefix('!').filter(|indexed| {
                    let end = indexed
                        .find(|ch: char| !is_name_char(ch))
                        .unwrap_or(indexed.len());
                    end > 0
                        && (indexed[end..].starts_with("[@]") || indexed[end..].starts_with("[*]"))
                }) {
                    // `${!NAME[@]}` as lexed (`$!NAME[@]`): its keys.
                    let end = indexed.find('[').unwrap_or(indexed.len());
                    if !is_identifier(&indexed[..end]) {
                        return None;
                    }
                    sources.push(&indexed[..end]);
                    at += 2 + end + 3;
                } else if rest.starts_with(['?', '$', '!']) {
                    at += 2;
                } else if rest.starts_with('#') {
                    // `$#`, `$#NAME`, `$#NAME[@]` (the lexer drops braces):
                    // a count.
                    at = name_end(at + 2);
                    for listed in ["[@]", "[*]"] {
                        work::step();
                        if value[at..].starts_with(listed) {
                            at += listed.len();
                        }
                    }
                } else if let Some(body) = rest.strip_prefix('{') {
                    let close = body.find('}')?;
                    let inner = &body[..close];
                    let listed = inner
                        .strip_suffix("[@]")
                        .or_else(|| inner.strip_suffix("[*]"));
                    if let Some(length) = inner.strip_prefix('#') {
                        // `${#NAME}`, `${#NAME[@]}`: a length.
                        let length = listed
                            .and_then(|inner| inner.strip_prefix('#'))
                            .unwrap_or(length);
                        if !is_identifier(length) && !matches!(length, "@" | "*") {
                            return None;
                        }
                    } else if let Some(indexed) = inner.strip_prefix('!') {
                        // `${!NAME[@]}`: its keys, set where it is filled.
                        let indexed = indexed
                            .strip_suffix("[@]")
                            .or_else(|| indexed.strip_suffix("[*]"))?;
                        if !is_identifier(indexed) {
                            return None;
                        }
                        sources.push(indexed);
                    } else if is_identifier(inner) {
                        sources.push(inner);
                    } else {
                        return None;
                    }
                    at += 2 + close + 1;
                } else {
                    let end = name_end(at + 1);
                    let name = &value[at + 1..end];
                    // `$1`, `$a[1]`, zsh `$x:l` are not plain copies.
                    if !is_identifier(name)
                        || bytes
                            .get(end)
                            .is_some_and(|&byte| matches!(byte, b'[' | b':'))
                    {
                        return None;
                    }
                    sources.push(name);
                    at = end;
                }
            }
            _ => return None,
        }
    }
    Some(sources)
}

/// Whether a `[[ ... ]]` condition compares a value that is not a known
/// number arithmetically (`-eq` operands, the subscript after `-v`).
fn condition_unsafe(region: &str, unsafe_in: &impl Fn(&str) -> bool) -> bool {
    let words: Vec<&str> = region.split_whitespace().collect();
    words.iter().enumerate().any(|(at, word)| {
        work::step();
        if *word == "-v" {
            words
                .get(at + 1)
                .and_then(|next| next.find('[').map(|open| &next[open..]))
                .is_some_and(|subscript| unsafe_in(&unquoted(subscript)))
        } else if TEST_OPERATORS.contains(word) {
            at.checked_sub(1)
                .and_then(|before| words.get(before))
                .into_iter()
                .chain(words.get(at + 1))
                .any(|operand| unsafe_in(&unquoted(operand)))
        } else {
            false
        }
    })
}

/// Whether a `[` region outside other arithmetic is a subscript the shell
/// evaluates (`$a[i]`, `${a[i]}`, `a[i]=v`, `[k]=v` in a list) rather than
/// a pattern (`grep 'x[ab]'`).
fn subscript_context(text: &str, range: &Range<usize>) -> bool {
    let before = &text[..range.start];
    let stem = before.trim_end_matches(is_name_char);
    if stem.len() == before.len() {
        return true;
    }
    // `${#a[i]}`, zsh `${=a[i]}`, `$^a[i]`.
    let mut sigils = stem;
    for _ in 0..2 {
        work::step();
        sigils = sigils
            .strip_suffix(['#', '=', '^', '~', '!', '+'])
            .unwrap_or(sigils);
    }
    let after = &text[range.end..];
    sigils.ends_with('$')
        || sigils.ends_with("${")
        || after.starts_with('=')
        || after.starts_with("+=")
}

/// `${!NAME[@]}`, `${!NAME[*]}`, `${!PREFIX@}`, `${!PREFIX*}`: keys or
/// names listed, not a name evaluated.
fn index_listing(region: &str) -> bool {
    let Some(body) = region
        .strip_prefix("${!")
        .and_then(|rest| rest.strip_suffix('}'))
    else {
        return false;
    };
    let name = body
        .strip_suffix("[@]")
        .or_else(|| body.strip_suffix("[*]"))
        .or_else(|| body.strip_suffix('@'))
        .or_else(|| body.strip_suffix('*'));
    name.is_some_and(is_identifier)
}

/// Whether zsh flags (`${(l:n:)x}`, `${(r:$n:)x}`, `${(I:n:)x}`) evaluate a
/// value that is not a known number, or `(P)`/`(e)` reads one again.
fn flags_unsafe(region: &str, unsafe_in: &impl Fn(&str) -> bool) -> bool {
    let body = &region[3..];
    let Some(close) = body.find(')') else {
        return unsafe_in(&unquoted(region));
    };
    let flags = &body[..close];
    // `(P)` takes a name from the value; `(e)` expands it again.
    if flags.contains(['P', 'e']) {
        return unsafe_in(&unquoted(region));
    }
    if references(flags).any(unsafe_in) {
        return true;
    }
    // Keep checking flags inside malformed arguments: a safe outer suffix
    // can contain an unsafe inner argument. Remove quotes once, then map each
    // original argument's boundaries into that shared text instead of copying
    // every overlapping suffix. Delimiter searches and callbacks are unchanged.
    let unquoted_flags = unquoted(flags);
    let quotes: Vec<usize> = flags
        .match_indices(['"', '\'', '\\'])
        .map(|(at, _)| at)
        .collect();
    let unquoted_at = |at| at - quotes.partition_point(|&quote| quote < at);
    flags.char_indices().any(|(at, flag)| {
        work::step();
        if !matches!(flag, 'l' | 'r' | 'I') {
            return false;
        }
        let rest = &flags[at + flag.len_utf8()..];
        let Some(delimiter) = rest.chars().next().filter(|ch| !ch.is_ascii_alphanumeric()) else {
            return false;
        };
        let start = at + flag.len_utf8() + delimiter.len_utf8();
        let end = flags[start..]
            .find(delimiter)
            .map_or(flags.len(), |end| start + end);
        unsafe_in(&unquoted_flags[unquoted_at(start)..unquoted_at(end)])
    })
}

/// A word without `printf` conversions (`%5.2f`, `%%`) and backslash
/// escapes (`\n`), which are format text rather than names. The lexer drops
/// backslashes (`%d\n` reads `%dn`), so name text right after a conversion
/// goes too (arithmetic cannot start with `%`). A `%` after a name or `)`
/// is the remainder operator (`m%n`) and stays.
fn format_text(word: &str) -> String {
    let mut kept = String::with_capacity(word.len());
    let mut chars = word.chars().peekable();
    let mut previous = ' ';
    while let Some(ch) = chars.next() {
        work::step();
        match ch {
            '\\' => {
                chars.next();
                kept.push(' ');
            }
            '%' if !(is_name_char(previous) || previous == ')') => {
                while chars
                    .next_if(|ch| "-+ #0123456789.'*hlLqjzt".contains(*ch))
                    .is_some()
                {
                    work::step();
                }
                if chars
                    .next_if(|ch| "diouxXeEfFgGaAcsbqQ%".contains(*ch))
                    .is_some()
                {
                    while chars.next_if(|ch| is_name_char(*ch)).is_some() {
                        work::step();
                    }
                }
                kept.push(' ');
            }
            ch => kept.push(ch),
        }
        previous = kept.chars().next_back().unwrap_or(' ');
    }
    kept
}

/// Whether text defines a zsh anonymous function or sets `functions[...]`.
fn anonymous_function(text: &str) -> bool {
    text.contains("functions[")
        || text.match_indices("()").any(|(at, _)| {
            let before = text[..at].trim_end_matches([' ', '\t']);
            let after = text[at + 2..].trim_start_matches([' ', '\t', '\n']);
            after.starts_with('{')
                && (before.is_empty() || before.ends_with([';', '&', '|', '\n', '(', '{']))
        })
        || text.match_indices("function").any(|(at, _)| {
            let before = text[..at].trim_end_matches([' ', '\t']);
            let after = text[at + "function".len()..].trim_start_matches([' ', '\t', '\n']);
            after.starts_with('{')
                && (before.is_empty() || before.ends_with([';', '&', '|', '\n', '(', '{']))
        })
}

/// `NAME+=value` or `NAME[sub]+=value`: the assignment appends.
fn appends(word: &str) -> bool {
    word.split_once('=')
        .is_some_and(|(left, _)| left.ends_with('+'))
}

/// Text after quote removal, which can join names (`va""r`, `va\r`).
fn unquoted(text: &str) -> String {
    text.chars()
        .filter(|ch| !matches!(ch, '"' | '\'' | '\\'))
        .collect()
}

/// Whether a lexed value is made only of what arithmetic reads without
/// building a new name: digits, blanks, operators, bare names, arithmetic
/// results (`$((...))`), `$#NAME`, special numbers (`$?`), and `$NAME`
/// for a variable in `known` (the lexer drops braces and quotes, so
/// `${x}r` and `$x"r"` read `$xr`, which the line does not assign).
/// `$@`, `$*`, `$a[*]`, `$a[@]` and other characters are not clean.
fn clean_form(value: &str, known: &HashSet<&str>) -> bool {
    let bytes = value.as_bytes();
    let name_at = |at: usize| {
        bytes
            .get(at)
            .is_some_and(|&byte| is_name_char(byte as char))
    };
    let mut at = 0;
    while at < bytes.len() {
        work::step();
        if value[at..].starts_with(shell::ARITHMETIC_OUTPUT) {
            let end = at + shell::ARITHMETIC_OUTPUT.len();
            if (at > 0 && name_at(at - 1)) || name_at(end) {
                return false;
            }
            at = end;
            continue;
        }
        let byte = bytes[at];
        match byte {
            b' '
            | b'\t'
            | b'\n'
            | b'0'..=b'9'
            | b'+'
            | b'-'
            | b'*'
            | b'/'
            | b'%'
            | b'('
            | b')'
            | b'<'
            | b'>'
            | b'='
            | b'!'
            | b'&'
            | b'|'
            | b'^'
            | b'~'
            | b'?'
            | b':'
            | b','
            | b'.'
            | b'['
            | b']'
            | b'#' => at += 1,
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                while name_at(at) {
                    work::step();
                    at += 1;
                }
            }
            b'$' => {
                let next = bytes.get(at + 1).copied();
                match next {
                    Some(b'?' | b'$' | b'!' | b'#') => {
                        at += 2;
                        // `$#NAME` (`${#NAME}`, `${#a[@]}`): a length.
                        while name_at(at) {
                            work::step();
                            at += 1;
                        }
                        for subscript in ["[@]", "[*]"] {
                            work::step();
                            if value[at..].starts_with(subscript) {
                                at += subscript.len();
                            }
                        }
                    }
                    Some(ch) if ch.is_ascii_alphanumeric() || ch == b'_' => {
                        let start = at + 1;
                        let mut end = start;
                        if ch.is_ascii_digit() {
                            end += 1;
                        } else {
                            while name_at(end) {
                                work::step();
                                end += 1;
                            }
                        }
                        // `$a[*]`/`$a[@]` join elements with IFS; zsh `$x:l`
                        // modifies the value.
                        let joins = value[end..].starts_with("[*]")
                            || value[end..].starts_with("[@]")
                            || (bytes.get(end) == Some(&b':')
                                && bytes.get(end + 1).is_some_and(u8::is_ascii_alphabetic))
                            // zsh `$a[1]r`: an element joined to name text.
                            || (bytes.get(end) == Some(&b'[') && {
                                let window = &bytes[end..bytes.len().min(end + MAX_SUBSCRIPT)];
                                window.iter().position(|&byte| byte == b']').is_none_or(|close| {
                                    bytes.get(end + close + 1).is_some_and(|&byte| {
                                        is_name_char(byte as char) || byte == b'$'
                                    })
                                })
                            });
                        if !known.contains(&value[start..end])
                            || joins
                            || bytes.get(end) == Some(&b'$')
                        {
                            return false;
                        }
                        at = end;
                    }
                    _ => return false,
                }
            }
            _ => return false,
        }
    }
    true
}

/// Parameter expansions in `text` whose result can form a new name: one
/// that transforms its value (`${x#z}`, `${x/a/b}`, zsh `${(L)x}`), or
/// that touches name text or another expansion (`${x}r`, `va$x`, `$x$y`).
/// Returns their variables, and whether one could not be read.
/// `$NAME`, `${NAME}`, `${#NAME}`, `${NAME[...]}` and `${NAME:-0}` alone
/// do not build names.
fn building(text: &str) -> (Vec<String>, bool) {
    let bytes = text.as_bytes();
    let mut builders = Vec::new();
    let mut unknown = false;
    let mut at = 0;
    while at < bytes.len() {
        work::step();
        if bytes[at] != b'$' {
            at += 1;
            continue;
        }
        let Some(&next) = bytes.get(at + 1) else {
            break;
        };
        let (end, name, transforms) = match next {
            b'(' | b'[' => {
                at += 1;
                continue;
            }
            b'{' => {
                let mut depth = 0;
                let Some(close) = text[at + 1..].char_indices().find_map(|(offset, ch)| {
                    match ch {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(at + 1 + offset);
                            }
                        }
                        _ => {}
                    }
                    None
                }) else {
                    return (builders, true);
                };
                let content = &text[at + 2..close];
                let (name, transforms) = braced(content);
                (close + 1, name, transforms)
            }
            ch if ch.is_ascii_alphabetic() || ch == b'_' => {
                let len = text[at + 1..]
                    .find(|ch: char| !is_name_char(ch))
                    .unwrap_or(text.len() - at - 1);
                let mut end = at + 1 + len;
                // zsh `$x:l`, `$x:t` (modifiers) transform the value.
                let mut transforms = bytes.get(end) == Some(&b':')
                    && bytes.get(end + 1).is_some_and(u8::is_ascii_alphabetic);
                // zsh `$a[1]r`, `$x[1,2]`: the subscript is part of it.
                if bytes.get(end) == Some(&b'[') {
                    let window = &bytes[end..bytes.len().min(end + MAX_SUBSCRIPT)];
                    match window.iter().position(|&byte| byte == b']') {
                        Some(close) => end += close + 1,
                        None => transforms = true,
                    }
                }
                (end, Some(&text[at + 1..at + 1 + len]), transforms)
            }
            // zsh `$^x`, `$=x`, `$~x` change how the value expands.
            b'^' | b'=' | b'~'
                if bytes
                    .get(at + 2)
                    .is_some_and(|&byte| is_name_char(byte as char) || byte == b'{') =>
            {
                return (builders, true);
            }
            ch if ch.is_ascii_digit() || matches!(ch, b'@' | b'*') => {
                (at + 2, Some(&text[at + 1..at + 2]), false)
            }
            b'#' | b'?' | b'$' | b'!' | b'-' => {
                at += 2;
                continue;
            }
            _ => {
                at += 1;
                continue;
            }
        };
        // A name run before `$` that is not a number (`0x$h` is). Quotes
        // join what they separate (`va"$x"`, `$x"r"`), so they are skipped.
        let quote = |byte: &u8| matches!(byte, b'"' | b'\'' | b'\\');
        let before: Vec<u8> = text[..at]
            .bytes()
            .rev()
            .filter(|byte| !quote(byte))
            .take_while(|&byte| is_name_char(byte as char))
            .collect();
        let touches_before = before.last().is_some_and(|byte| !byte.is_ascii_digit());
        let touches_after = bytes[end.min(bytes.len())..]
            .iter()
            .find(|byte| !quote(byte))
            .is_some_and(|&byte| is_name_char(byte as char) || byte == b'$');
        if transforms || touches_before || touches_after {
            match name {
                Some(name) => builders.push(name.to_string()),
                None => unknown = true,
            }
        }
        at = end;
    }
    (builders, unknown)
}

/// The variable a `${...}` body expands, and whether it transforms the
/// value into something that may be a new name.
fn braced(content: &str) -> (Option<&str>, bool) {
    let body = content.strip_prefix(['#', '!']).unwrap_or(content);
    let len = body
        .find(|ch: char| !is_name_char(ch))
        .unwrap_or(body.len());
    if len == 0 {
        // Positional or special (`${1}`, `${@}`), or zsh flags `${(L)x}`.
        let plain = !body.is_empty()
            && body
                .chars()
                .all(|ch| ch.is_ascii_digit() || matches!(ch, '@' | '*' | '#' | '?'));
        return (plain.then_some(body), !plain);
    }
    let name = &body[..len];
    let rest = &body[len..];
    // `${!x}` expands the variable `x` names.
    let plain = !content.starts_with('!')
        && (rest.is_empty()
            || (rest.starts_with('[') && rest.ends_with(']'))
            || (!content.starts_with(['#', '!'])
                && [":-", "-", ":=", "="].iter().any(|operator| {
                    rest.strip_prefix(operator).is_some_and(|default| {
                        default
                            .chars()
                            .all(|ch| ch.is_ascii_digit() || matches!(ch, ' ' | '+' | '-'))
                    })
                })));
    (Some(name), !plain)
}

/// Whether a command may turn on zsh options the reader cannot name
/// (#1084): `setopt`/`unsetopt` with an expanded name, one built by a
/// brace range or glob (`glob{s..s}ubst`, `glob?subst`) or by pattern
/// (`-m`), `set`, `emulate` or a zsh with such an option or `-o` value, or
/// `emulate` (also `zsh --emulate`) in a mode other than zsh: `csh`, `ksh`
/// and `sh` turn on `GLOB_SUBST`. Also `zmv`, `zcp` and `zln` with an
/// expanded pattern, which they glob as `${~pat}`.
fn sets_unnamed_options(name: &str, args: &[String]) -> bool {
    let expanded = |word: &str| word.contains(['$', '`']) || word.contains(shell::UNSEEN_OUTPUT);
    // The lexer drops braces but keeps a range's `..`.
    let unnamed = |word: &str| {
        expanded(word) || word.contains(['{', '}', '*', '?', '[', '<', '(']) || word.contains("..")
    };
    let name = shell::basename(name);
    // keeper's `keep` globs all its arguments as `$~*`.
    if name == "keep" {
        return args.iter().any(|word| expanded(word));
    }
    if matches!(name, "zmv" | "zcp" | "zln") {
        let mut words = args.iter();
        while let Some(word) = words.next() {
            work::step();
            if expanded(word) {
                return true;
            }
            if word == "--" {
                return words.next().is_some_and(|pattern| expanded(pattern));
            }
            let Some(cluster) = word.strip_prefix('-') else {
                // The pattern.
                return false;
            };
            // `-p prog`, `-np prog`: a value-taking letter at the end of
            // its cluster takes the next word (`-pprog` holds its own).
            if let Some(at) = cluster.find(['o', 'p', 'P'])
                && at + 1 == cluster.len()
            {
                words.next();
            }
        }
        return false;
    }
    if matches!(name, "setopt" | "unsetopt") {
        return args
            .iter()
            .any(|word| unnamed(word) || (word.starts_with(['-', '+']) && word.contains('m')));
    }
    if !matches!(name, "set" | "emulate" | "zsh" | "-zsh") {
        return false;
    }
    let emulate = name == "emulate";
    let mut mode_seen = false;
    let mut script_next = false;
    let mut words = args.iter();
    while let Some(word) = words.next() {
        work::step();
        if word == "--" {
            break;
        }
        if !(word.len() > 1 && word.starts_with(['-', '+'])) {
            // `emulate zsh -c 'code'`, `zsh -c 'code'`: the rest is code,
            // read as commands.
            if (emulate && mode_seen) || script_next {
                break;
            }
            // An expanded word where an option or mode may stand
            // (`set $(cat f)`, `emulate $m`).
            if expanded(word) {
                return true;
            }
            if emulate && !mode_seen {
                if word != "zsh" {
                    return true;
                }
                mode_seen = true;
                continue;
            }
            break;
        }
        if unnamed(word) {
            return true;
        }
        if let Some(long) = word.strip_prefix("--") {
            let mode = match long.strip_prefix("emulate") {
                Some("") => words.next().map(String::as_str),
                Some(value) => value.strip_prefix('='),
                None => continue,
            };
            if mode.is_none_or(|mode| mode != "zsh") {
                return true;
            }
            continue;
        }
        script_next |= name != "set" && !emulate && word.starts_with('-') && word.contains('c');
        // `-o NAME` and clusters holding `o` (`-xo NAME`) take the next word.
        if word[1..].contains('o') && words.next().is_some_and(|value| unnamed(value)) {
            return true;
        }
    }
    false
}

/// Whether lexed words may start a zsh under another name (`exec -a csh
/// zsh`, `exec -acsh zsh`, GNU `env -a csh zsh`), which it reads as an
/// emulation mode (#1084): `exec` or `env`, a later option word with `a`
/// and a later word that names a zsh or is expanded. One pass.
fn renames_shell(words: &[String]) -> bool {
    let (mut launcher, mut renamed, mut shell) = (false, false, false);
    for word in words {
        work::step();
        if launcher {
            renamed |= word.starts_with('-') && !word.starts_with("--") && word.contains('a');
            shell |= shell::basename(word).contains("zsh")
                || word.contains(['$', '`'])
                || word.contains(shell::UNSEEN_OUTPUT);
        }
        launcher |= matches!(shell::basename(word), "exec" | "env");
    }
    renamed && shell
}

/// Text as the shell parses it: a backslash-newline joins the lines.
fn joined(text: &str) -> String {
    text.replace("\\\n", "")
}

/// Whether a builtin evaluates its operands, or names variables in them.
fn evaluates(name: &str) -> bool {
    NUMERIC_BUILTINS.contains(&name)
        || NAMING_BUILTINS.contains(&name)
        || matches!(
            name,
            "test" | "[" | "[[" | "printf" | "print" | "for" | "set"
        )
}

/// Names a word assigns through `${NAME:=...}`, `${NAME=...}` or zsh
/// `${NAME::=...}` (a bare `$NAME=` only over-matches).
fn defaults(word: &str) -> Vec<String> {
    // The lexer drops braces, so `${a:=x}` arrives as `$a:=x`.
    word.match_indices('$')
        .filter_map(|(at, _)| {
            let body = &word[at + 1..];
            let body = body.strip_prefix('{').unwrap_or(body);
            let len = body
                .find(|ch: char| !is_name_char(ch))
                .unwrap_or(body.len());
            let mut rest = &body[len..];
            // `${b[0]:=...}`: skip a subscript (a bounded look, so this
            // stays linear; an unclosed one counts as assigning).
            if rest.starts_with('[') {
                let window = &rest.as_bytes()[..rest.len().min(MAX_SUBSCRIPT)];
                // `]` is ASCII, so the byte after it is a char boundary.
                rest = match window.iter().position(|&byte| byte == b']') {
                    Some(close) => &rest[close + 1..],
                    None => "=",
                };
            }
            (len > 0
                && is_identifier(&body[..len])
                && (rest.starts_with('=') || rest.starts_with(":=") || rest.starts_with("::=")))
            .then(|| body[..len].to_string())
        })
        .collect()
}

/// A list in parentheses that fills variables: `NAME=(...)`, `NAME+=(...)`
/// or zsh `for NAME... (...)`.
struct List {
    names: Vec<String>,
    /// The text inside the parentheses.
    body: Range<usize>,
}

/// The lists in the text, or none and `true` when they nest too deeply to
/// follow. Quotes are not honoured. Linear in the text.
fn lists(text: &str) -> (Vec<List>, bool) {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    // Open parentheses: the names a list fills, if it is one.
    let mut stack: Vec<Option<(Vec<String>, usize)>> = Vec::new();
    let mut depth = 0;
    for (at, &byte) in bytes.iter().enumerate() {
        work::step();
        match byte {
            b'(' => {
                let names = list_names(&text[..at]);
                if names.is_some() {
                    depth += 1;
                    if depth > MAX_LIST_DEPTH {
                        return (Vec::new(), true);
                    }
                }
                stack.push(names.map(|names| (names, at + 1)));
            }
            b')' => {
                if let Some(Some((names, start))) = stack.pop() {
                    depth -= 1;
                    found.push(List {
                        names,
                        body: start..at,
                    });
                }
            }
            _ => {}
        }
    }
    // Unclosed lists run to the end of the text.
    for (names, start) in stack.into_iter().flatten() {
        work::step();
        found.push(List {
            names,
            body: start..bytes.len(),
        });
    }
    (found, false)
}

/// Byte offset where the trailing run of name characters in `text` starts.
fn name_tail_start(text: &str) -> usize {
    text.char_indices()
        .rev()
        .find(|&(_, ch)| !is_name_char(ch))
        .map_or(0, |(at, ch)| at + ch.len_utf8())
}

/// The names a `(` after this text fills: `NAME=` or `NAME+=` right before
/// it, or zsh `for NAME...` (`foreach`).
fn list_names(before: &str) -> Option<Vec<String>> {
    if let Some(left) = before.strip_suffix('=') {
        let left = left.strip_suffix('+').unwrap_or(left);
        let name = &left[name_tail_start(left)..];
        return is_identifier(name).then(|| vec![name.to_string()]);
    }
    let mut names = Vec::new();
    let mut rest = before.trim_end_matches([' ', '\t']);
    loop {
        work::step();
        let start = name_tail_start(rest);
        let word = &rest[start..];
        if matches!(word, "for" | "foreach") {
            let before_keyword = rest[..start].trim_end_matches([' ', '\t']);
            let keyword_starts = before_keyword.is_empty()
                || before_keyword.ends_with([';', '&', '|', '\n', '{', '(', '!']);
            return (keyword_starts && !names.is_empty()).then_some(names);
        }
        if !is_identifier(word) || start == 0 {
            return None;
        }
        names.push(word.to_string());
        let trimmed = rest[..start].trim_end_matches([' ', '\t']);
        if trimmed.len() == start {
            // Not separated by blanks.
            return None;
        }
        rest = trimmed;
    }
}

/// Whether lexed text holds the output of a command substitution the
/// classifier cannot see (an arithmetic expansion's number does not count).
fn command_output(text: &str) -> bool {
    text.replace(shell::ARITHMETIC_OUTPUT, "")
        .contains(shell::UNSEEN_OUTPUT)
}

/// Lexed text without its placeholders, so their letters are not names.
fn visible(text: &str) -> String {
    text.replace(shell::ARITHMETIC_OUTPUT, " ")
        .replace(shell::UNSEEN_OUTPUT, " ")
}

/// Whether arithmetic text runs a command substitution whose output the
/// classifier cannot see (`$(( $(cat f) + 1 ))`, `a[`cat f`]`). Nested
/// arithmetic is read as grouping, not as a substitution.
fn unseen_substitution(region: &str) -> bool {
    if !region.contains("$(") && !region.contains('`') {
        return false;
    }
    let text = region.replace("$((", "((").replace("$[", "[");
    shell::simple_commands(&[text])
        .commands
        .iter()
        .any(|command| command.words.iter().any(|word| command_output(word)))
}

/// `NAME=value`, `NAME+=value` or `NAME[sub]=value`: the name and value.
pub(super) fn assignment(word: &str) -> Option<(&str, &str)> {
    let (left, value) = word.split_once('=')?;
    let left = left.strip_suffix('+').unwrap_or(left);
    let name = left.split_once('[').map_or(left, |(name, _)| name);
    is_identifier(name).then_some((name, value))
}

fn is_identifier(word: &str) -> bool {
    let mut chars = word.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn is_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

/// The variable names text mentions as arithmetic would read it: bare
/// identifiers and `$NAME`/`${NAME}` references, including positional `$1`
/// and `$@`. Not a number's digits (`0x1f`, `16#ff`) or a length (`${#x}`).
fn names(text: &str) -> impl Iterator<Item = &str> {
    let bytes = text.as_bytes();
    let mut index = 0;
    std::iter::from_fn(move || {
        while index < bytes.len() {
            work::step();
            let start = index;
            let ch = bytes[index] as char;
            let previous = start.checked_sub(1).map(|at| bytes[at] as char);
            let after_dollar = previous == Some('$')
                || (previous == Some('{') && start >= 2 && bytes[start - 2] == b'$');
            if after_dollar && (ch.is_ascii_digit() || ch == '@' || ch == '*') {
                index += 1;
                return Some(&text[start..index]);
            }
            if ch.is_ascii_alphabetic() || ch == '_' {
                while index < bytes.len() && is_name_char(bytes[index] as char) {
                    work::step();
                    index += 1;
                }
                let number_part = previous.is_some_and(|ch| ch.is_ascii_digit() || ch == '#');
                if !number_part {
                    return Some(&text[start..index]);
                }
                continue;
            }
            index += 1;
        }
        None
    })
}

/// The `$NAME`/`${NAME` references in a word.
fn references(word: &str) -> impl Iterator<Item = &str> {
    word.match_indices('$').filter_map(|(at, _)| {
        let rest = &word[at + 1..];
        let rest = rest.strip_prefix('{').unwrap_or(rest);
        let len = rest
            .find(|ch: char| !is_name_char(ch))
            .unwrap_or(rest.len());
        (len > 0).then(|| &rest[..len])
    })
}

/// Whether a `printf` format holds a numeric conversion (`%d`, `%5.2f`) or
/// a width or precision taken from an operand (`%*s`).
fn numeric_format(word: &str) -> bool {
    word.split('%').skip(1).any(|conversion| {
        let spec =
            conversion.trim_start_matches(|ch: char| "-+ #0123456789.".contains(ch) || ch == '*');
        let flags = &conversion[..conversion.len() - spec.len()];
        flags.contains('*')
            || spec
                .chars()
                .next()
                .is_some_and(|ch| "diouxXeEfFgGaA".contains(ch))
    })
}

/// Functions defined in the text: `name()`, `name ()` or `function name`.
fn functions(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let bytes = text.as_bytes();
    for (at, _) in text.match_indices('(') {
        work::step();
        let after = text[at + 1..].trim_start_matches([' ', '\t']);
        if !after.starts_with(')') {
            continue;
        }
        let before = text[..at].trim_end_matches([' ', '\t']);
        let start = before
            .char_indices()
            .rev()
            .find(|(_, ch)| !(is_name_char(*ch) || matches!(ch, '-' | '.' | ':')))
            .map_or(0, |(at, ch)| at + ch.len_utf8());
        if start < before.len() && bytes[start] != b'$' {
            found.push(before[start..].to_string());
        }
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    for pair in words.windows(2) {
        work::step();
        if pair[0] == "function" {
            let name = pair[1].trim_end_matches(['(', ')', '{']);
            if !name.is_empty() {
                found.push(name.to_string());
            }
        }
    }
    found
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Frame {
    /// `$((`, `((`: closes at `))`.
    Arithmetic,
    /// `$[`, a subscript `x[`, or any `[` inside arithmetic.
    Bracket,
    /// `(` inside arithmetic (or `$(` there).
    Paren,
    /// `${...}`; arithmetic when it holds an offset or indirection.
    Brace { arithmetic: bool },
}

impl Frame {
    fn arithmetic(self) -> bool {
        !matches!(self, Frame::Brace { arithmetic: false })
    }
}

/// Open frames, counting the arithmetic ones so each step is constant time.
#[derive(Default)]
struct Frames {
    frames: Vec<Frame>,
    arithmetic: usize,
}

impl Frames {
    fn push(&mut self, frame: Frame) {
        self.arithmetic += usize::from(frame.arithmetic());
        self.frames.push(frame);
    }

    fn pop(&mut self) -> Option<Frame> {
        let frame = self.frames.pop()?;
        self.arithmetic -= usize::from(frame.arithmetic());
        Some(frame)
    }

    fn last(&self) -> Option<&Frame> {
        self.frames.last()
    }

    fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Whether some open frame is arithmetic.
    fn inside(&self) -> bool {
        self.arithmetic > 0
    }
}

/// Byte ranges of the text that the shell evaluates as arithmetic:
/// `$(( ))`, `(( ))`, `$[ ]`, subscripts, `${x:offset:length}`, `${!x}`,
/// zsh `${(flags)x}`, and `[[ ]]` with an arithmetic operator. Quotes are
/// not honoured; an unclosed region runs to the end of the text. Linear in
/// the text.
fn regions(text: &str) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut stack = Frames::default();
    // How many lists (`a=(...)`) each byte is in: `[k]=v` there is a
    // subscript.
    let mut in_list = vec![0_i32; bytes.len() + 1];
    for list in lists(text).0 {
        work::step();
        in_list[list.body.start] += 1;
        in_list[list.body.end] -= 1;
    }
    let mut lists_open = 0;
    // Where the outermost open arithmetic frame starts.
    let mut open: Option<usize> = None;
    let mut index = 0;
    let at = |offset: usize| bytes.get(offset).copied().map(char::from);
    // The colon of the innermost `${x:offset}`, where arithmetic starts.
    let mut offset: Option<usize> = None;
    // The next `]` at or after each byte, for subscripts in `${a[i]:v}`.
    let mut next_close = vec![bytes.len(); bytes.len() + 1];
    for at in (0..bytes.len()).rev() {
        work::step();
        next_close[at] = if bytes[at] == b']' {
            at
        } else {
            next_close[at + 1]
        };
    }
    let mut counted = 0;
    while index < bytes.len() {
        work::step();
        while counted <= index {
            work::step();
            lists_open += in_list[counted];
            counted += 1;
        }
        if offset == Some(index) && stack.last() == Some(&Frame::Brace { arithmetic: false }) {
            stack.pop();
            if open.is_none() {
                open = Some(index);
            }
            stack.push(Frame::Brace { arithmetic: true });
        }
        let ch = bytes[index] as char;
        let next = at(index + 1);
        let inside = stack.inside();
        let push = |frame: Frame, stack: &mut Frames, open: &mut Option<usize>| {
            if frame.arithmetic() && open.is_none() {
                *open = Some(index);
            }
            stack.push(frame);
        };
        let mut step = 1;
        match ch {
            '$' if next == Some('(') && at(index + 2) == Some('(') => {
                push(Frame::Arithmetic, &mut stack, &mut open);
                step = 3;
            }
            '(' if next == Some('(') => {
                push(Frame::Arithmetic, &mut stack, &mut open);
                step = 2;
            }
            '$' if next == Some('[') => {
                push(Frame::Bracket, &mut stack, &mut open);
                step = 2;
            }
            '$' if next == Some('{') => {
                // `${!x}` and zsh `${(flags)x}` evaluate names and numbers;
                // `${x:offset:length}` from its colon.
                let arithmetic = matches!(at(index + 2), Some('!' | '('));
                offset = brace_offset(text, index + 2, &next_close);
                push(Frame::Brace { arithmetic }, &mut stack, &mut open);
                step = 2;
            }
            '[' if inside
                || index.checked_sub(1).is_some_and(|before| {
                    let before = bytes[before] as char;
                    is_name_char(before)
                        // `a=([k]=v)`, zsh glob qualifiers `*([n])`.
                        || before == '('
                        || (lists_open > 0 && before.is_whitespace())
                }) =>
            {
                push(Frame::Bracket, &mut stack, &mut open);
            }
            '(' if inside => push(Frame::Paren, &mut stack, &mut open),
            '{' if !stack.is_empty() => {
                push(Frame::Brace { arithmetic: false }, &mut stack, &mut open)
            }
            ')' => match stack.last() {
                Some(Frame::Paren) => {
                    stack.pop();
                }
                Some(Frame::Arithmetic) if next == Some(')') => {
                    stack.pop();
                    step = 2;
                }
                _ => {}
            },
            ']' if stack.last() == Some(&Frame::Bracket) => {
                stack.pop();
            }
            '}' if matches!(stack.last(), Some(Frame::Brace { .. })) => {
                stack.pop();
            }
            _ => {}
        }
        index += step;
        if let Some(start) = open
            && !stack.inside()
        {
            ranges.push(start..index);
            open = None;
        }
    }
    if let Some(start) = open {
        ranges.push(start..bytes.len());
    }
    ranges.extend(conditions(text));
    ranges
}

/// Where text may hold glob-qualifier lists: the bodies of parenthesised
/// groups attached to a word (`/*(.[w])`, `file(N)`, `x*(e:code:)`,
/// `(#q.)`), outermost first, read without quotes so nested shells' scripts
/// and strings later run as code count. zsh evaluates the qualifiers'
/// subscripts as arithmetic wherever they sit, and `e`/`+` qualifiers as
/// code (#937, D57); bash extended globs (`@(a|b)`) are read the same way.
/// Groups that open a command or expansion do not count: `$(`, `${(`, `<(`
/// at a word's start, `a=(` in an assignment, `((`, `f()`, and groups inside
/// arithmetic regions, whose every name the fallback checks itself. A group
/// left open yields the empty range at the end of the text, which asks.
/// Linear: parentheses are matched in one pass, and each backslash or `$`
/// run is counted at most twice.
fn qualifier_lists(text: &str) -> Vec<Range<usize>> {
    lists_attached(text, /*skip_arithmetic*/ true)
}

/// `qualifier_lists` including groups inside arithmetic regions, for
/// checks that do not read those regions themselves (#1028): a region read
/// without quotes may span a real qualifier (`echo '$((' ; print *(e:x:)`).
fn all_qualifier_lists(text: &str) -> Vec<Range<usize>> {
    lists_attached(text, /*skip_arithmetic*/ false)
}

fn lists_attached(text: &str, skip_arithmetic: bool) -> Vec<Range<usize>> {
    if !text.contains('(') {
        return Vec::new();
    }
    let bytes = text.as_bytes();
    let mut depth = vec![0_i32; bytes.len() + 1];
    let skipped = if skip_arithmetic {
        regions(text)
    } else {
        Vec::new()
    };
    for range in skipped {
        work::step();
        let region = &text[range.clone()];
        if ["$((", "((", "$[", ":"]
            .iter()
            .any(|start| region.starts_with(start))
        {
            depth[range.start] += 1;
            depth[range.end] -= 1;
        }
    }
    // Where each `(` closes, read without quotes.
    let mut close = vec![bytes.len(); bytes.len()];
    let mut open = Vec::new();
    for (at, byte) in bytes.iter().enumerate() {
        work::step();
        match byte {
            b'(' => open.push(at),
            b')' => {
                if let Some(start) = open.pop() {
                    close[start] = at;
                }
            }
            _ => {}
        }
    }
    // Whether the byte at `at` follows an odd run of backslashes.
    let escaped = |at: usize| {
        bytes[..at]
            .iter()
            .rev()
            .take_while(|byte| **byte == b'\\')
            .count()
            % 2
            == 1
    };
    let separator = |byte: u8| byte.is_ascii_whitespace() || b";&|(".contains(&byte);
    let attached = |at: usize| {
        if bytes.get(at + 1) == Some(&b'#') {
            return true;
        }
        let Some(before) = at.checked_sub(1) else {
            return false;
        };
        if escaped(before) {
            return true;
        }
        let opens = match bytes[before] {
            byte if separator(byte) => true,
            b'[' | b'`' => true,
            // `$(`; `$$(` is the process ID, then a group.
            b'$' => {
                bytes[..before]
                    .iter()
                    .rev()
                    .take_while(|byte| **byte == b'$')
                    .count()
                    % 2
                    == 0
            }
            b'{' => before > 0 && bytes[before - 1] == b'$' && !escaped(before - 1),
            // `<(cmd)`, `>(cmd)`, zsh `=(cmd)`; `<1-5>(.)` is a glob.
            b'<' | b'>' | b'=' if before == 0 || separator(bytes[before - 1]) => true,
            b'=' => assignment_position(text, before),
            _ => false,
        };
        !opens
    };
    let mut lists = Vec::new();
    let mut masked = 0;
    let mut after = 0;
    for at in 0..bytes.len() {
        work::step();
        masked += depth[at];
        if at < after
            || bytes[at] != b'('
            || masked > 0
            || escaped(at)
            || bytes.get(at + 1) == Some(&b')')
            || !attached(at)
        {
            continue;
        }
        if close[at] == bytes.len() {
            lists.push(bytes.len()..bytes.len());
            break;
        }
        lists.push(at + 1..close[at]);
        after = close[at];
    }
    lists
}

/// Bare qualifier text with arithmetic expansions (`$((...))`) and
/// neutral facts (`$(date +%s)`) read as `0`. `None` when another
/// substitution's output (chosen by whatever it reads) would be read.
/// Linear: each substitution is skipped once.
fn neutral_expansions(list: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(list.len());
    let mut at = 0;
    while at < list.len() {
        work::step();
        let arithmetic = list[at..].starts_with(b"$((");
        let substitution =
            list[at] == b'`' || (list[at] == b'$' && list.get(at + 1) == Some(&b'('));
        if !substitution {
            out.push(list[at]);
            at += 1;
            continue;
        }
        // Where the inner group of `$((` closes, to tell arithmetic
        // (`$((1))`) from a subshell in a substitution (`$((cmd) )`).
        let mut inner_close = None;
        let end = if list[at] == b'`' {
            at + 1 + list[at + 1..].iter().position(|byte| *byte == b'`')? + 1
        } else {
            let mut depth = 0_usize;
            let mut end = None;
            for (offset, byte) in list[at + 1..].iter().enumerate() {
                work::step();
                match byte {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 1 && inner_close.is_none() {
                            inner_close = Some(at + 1 + offset);
                        }
                        if depth == 0 {
                            end = Some(at + 1 + offset + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            end?
        };
        let arithmetic = arithmetic && inner_close == Some(end - 2);
        let span = &list[at..end];
        let neutral = if arithmetic {
            // Its own expansions are read like any other.
            !span[3..].contains(&b'$') && !span.contains(&b'`')
        } else if span.starts_with(b"$((") {
            // A subshell in a substitution (`$((cmd) )`).
            false
        } else {
            only_facts(&String::from_utf8_lossy(span))
        };
        if !neutral {
            return None;
        }
        out.push(b'0');
        at = end;
    }
    Some(out)
}

/// Where the group whose `(` is just before `start` ends when quotes and
/// backslashes are read: at the first `)` outside them and outside nested
/// groups. `None` when it does not end within `MAX_QUOTED_GROUP` bytes,
/// which the caller treats as code (bounded, so many groups stay linear).
fn quoted_group_end(text: &str, start: usize) -> Option<usize> {
    let bytes = &text.as_bytes()[..text.len().min(start + MAX_QUOTED_GROUP)];
    let mut depth = 0_usize;
    let mut at = start;
    while at < bytes.len() {
        work::step();
        match bytes[at] {
            b'\\' => at += 1,
            b'\'' => {
                at = bytes[at + 1..]
                    .iter()
                    .position(|byte| *byte == b'\'')
                    .map_or(bytes.len(), |end| at + 1 + end);
            }
            b'"' => {
                at += 1;
                while at < bytes.len() && bytes[at] != b'"' {
                    work::step();
                    at += if bytes[at] == b'\\' { 2 } else { 1 };
                }
            }
            b'(' => depth += 1,
            b')' if depth == 0 => return Some(at),
            b')' => depth -= 1,
            _ => {}
        }
        at += 1;
    }
    None
}

/// Whether zsh, parsing a group's text as glob qualifiers, may reach one
/// that runs code before a character it rejects: `e` with any delimiter
/// and non-empty code that closes,
/// `+name`, `oe`/`o+` sorts, or an expansion (`$x`, a substitution that is
/// not a neutral fact) whose value it would parse as qualifiers. Quotes
/// and backslashes are removed first, as zsh does (`'e':x:`). Text zsh
/// rejects (`print('hello')`: `h` is no qualifier) or reads as modifiers
/// after `:` runs nothing. Linear.
fn qualifier_runs_code(list: &str) -> bool {
    let list: Vec<u8> = {
        let mut bare = Vec::with_capacity(list.len());
        let mut bytes = list.bytes();
        while let Some(byte) = bytes.next() {
            work::step();
            match byte {
                b'\'' | b'"' => {}
                b'\\' => bare.extend(bytes.next()),
                byte => bare.push(byte),
            }
        }
        bare
    };
    // zsh expands the word before reading qualifiers, so an expansion
    // anywhere (`P:$x:`, `u$x`, `[$x]`) may become code: only arithmetic
    // (a number) and neutral facts are read on, as `0`. Brace expansion
    // (`{e,u}:code:`) may also form one.
    let Some(list) = neutral_expansions(&list) else {
        return true;
    };
    if list.contains(&b'$') || list.contains(&b'`') {
        return true;
    }
    if let Some(open) = list.iter().position(|byte| *byte == b'{')
        && (list[open..].contains(&b',') || list[open..].windows(2).any(|pair| pair == b".."))
    {
        return true;
    }
    let list = list.as_slice();
    // Past a delimited argument starting at `at` (the delimiter); `None`
    // when it never closes.
    // The delimiter is a whole character, as zsh reads it (`Pé-xé`).
    let delimited = |at: usize| -> Option<usize> {
        let open = *list.get(at)?;
        let width = match open {
            0xf0.. => 4,
            0xe0.. => 3,
            0xc0.. => 2,
            _ => 1,
        };
        let close: &[u8] = match open {
            b'(' => b")",
            b'[' => b"]",
            b'{' => b"}",
            b'<' => b">",
            _ => list.get(at..at + width)?,
        };
        list[at + width..]
            .windows(close.len())
            .position(|window| window == close)
            .map(|end| at + width + end + close.len())
    };
    let digits = |mut at: usize| {
        let start = at;
        while list.get(at).is_some_and(u8::is_ascii_digit) {
            work::step();
            at += 1;
        }
        (at > start).then_some(at)
    };
    let sign = |at: usize| at + usize::from(matches!(list.get(at), Some(b'+' | b'-')));
    let mut at = 0;
    if list.starts_with(b"#q") {
        at = 2;
    }
    while at < list.len() {
        work::step();
        let byte = list[at];
        at = match byte {
            // Code between delimiters: zsh rejects it left open, and empty
            // code runs nothing.
            b'e' => match delimited(at + 1) {
                Some(next) if next > at + 3 => return true,
                Some(next) => next,
                None => return false,
            },
            b'+' => {
                return list.get(at + 1).is_some_and(|next| {
                    next.is_ascii_alphanumeric() || !next.is_ascii() || b"_{$`".contains(next)
                });
            }
            b'o' | b'O' => match list.get(at + 1) {
                Some(b'e') => match delimited(at + 2) {
                    Some(next) if next > at + 4 => return true,
                    Some(next) => next,
                    None => return false,
                },
                Some(b'+') => {
                    return list.get(at + 2).is_some_and(|next| {
                        next.is_ascii_alphanumeric() || !next.is_ascii() || b"_{$`".contains(next)
                    });
                }
                Some(b'n' | b'L' | b'l' | b'a' | b'm' | b'c' | b'd' | b'N') => at + 2,
                _ => return false,
            },
            b'a' | b'm' | b'c' => {
                let unit = at
                    + 1
                    + usize::from(matches!(
                        list.get(at + 1),
                        Some(b'M' | b'w' | b'd' | b'h' | b'm' | b's')
                    ));
                match digits(sign(unit)) {
                    Some(next) => next,
                    None => return false,
                }
            }
            b'L' => {
                let unit = at
                    + 1
                    + usize::from(matches!(
                        list.get(at + 1),
                        Some(b'k' | b'K' | b'm' | b'M' | b'g' | b'G' | b't' | b'T' | b'p' | b'P')
                    ));
                match digits(sign(unit)) {
                    Some(next) => next,
                    None => return false,
                }
            }
            b'l' => match digits(sign(at + 1)) {
                Some(next) => next,
                None => return false,
            },
            b'd' | b'Y' => match digits(at + 1) {
                Some(next) => next,
                None => return false,
            },
            b'u' | b'g' => match digits(at + 1) {
                Some(next) => next,
                None => match delimited(at + 1) {
                    Some(next) => next,
                    None => return false,
                },
            },
            b'P' => match delimited(at + 1) {
                Some(next) => next,
                None => return false,
            },
            // A mode, as zsh reads it: octal digits (`f644`), one of
            // `=+-?` and digits or permission letters (`f+rw`, `f?644`), or
            // a spec delimited by any other character (`fu+xu`, `feu+xe`).
            b'f' => match list.get(at + 1) {
                Some(b'0'..=b'7') => {
                    let mut next = at + 1;
                    while list
                        .get(next)
                        .is_some_and(|byte| (b'0'..=b'7').contains(byte))
                    {
                        work::step();
                        next += 1;
                    }
                    next
                }
                Some(b'=' | b'+' | b'-' | b'?') => match digits(at + 2) {
                    Some(next) => next,
                    None => {
                        let mut next = at + 2;
                        while list.get(next).is_some_and(|byte| b"rwxXst".contains(byte)) {
                            work::step();
                            next += 1;
                        }
                        next
                    }
                },
                _ => match delimited(at + 1) {
                    Some(next) => next,
                    None => return false,
                },
            },
            // A subscript, which may nest (`[a[1]+1]`).
            b'[' => {
                let mut depth = 0_usize;
                let mut end = None;
                for (offset, byte) in list[at..].iter().enumerate() {
                    work::step();
                    match byte {
                        b'[' => depth += 1,
                        b']' => {
                            depth -= 1;
                            if depth == 0 {
                                end = Some(at + offset + 1);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                match end {
                    Some(end) => end,
                    None => return false,
                }
            }
            b'%' => at + 1 + usize::from(matches!(list.get(at + 1), Some(b'b' | b'c'))),
            // Modifiers follow; none of them runs code.
            b':' => return false,
            b'/' | b'.' | b'@' | b'=' | b'p' | b'*' | b'r' | b'w' | b'x' | b'A' | b'I' | b'E'
            | b'R' | b'W' | b'X' | b's' | b'S' | b't' | b'F' | b'N' | b'D' | b'n' | b'M' | b'T'
            | b'U' | b'G' | b'^' | b'-' | b',' | b'q' => at + 1,
            _ => return false,
        };
    }
    false
}

/// Whether the `=` at `at` ends a word that assigns an array (`a=(`,
/// `a+=(`, `a[k]=(`): the word starts the command, or follows other
/// assignments or the options of a declaring builtin.
fn assignment_position(text: &str, at: usize) -> bool {
    let before = &text[..at];
    let start = work::rfind_by(before, |ch: char| {
        ch.is_ascii_whitespace() || ";&|(".contains(ch)
    })
    .map_or(0, |space| space + 1);
    let left = before[start..]
        .strip_suffix('+')
        .unwrap_or(&before[start..]);
    let name = left.split_once('[').map_or(left, |(name, _)| name);
    if !is_identifier(name) {
        return false;
    }
    let mut rest = before[..start].trim_end();
    // A bounded walk: past that the group counts as a qualifier list.
    for _ in 0..MAX_ASSIGNMENT_WALK {
        work::step();
        if rest.is_empty() || rest.ends_with([';', '&', '|', '(', '{', '!']) {
            return true;
        }
        let word_start = work::rfind_by(rest, |ch: char| {
            ch.is_ascii_whitespace() || ";&|({".contains(ch)
        })
        .map_or(0, |space| space + 1);
        let word = &rest[word_start..];
        // An earlier array assignment (`a=(1 2) b=(3)`): read on from its
        // `a=`.
        if word.ends_with(')') {
            let from = rest.len().saturating_sub(MAX_SUBSCRIPT);
            let Some(group) = rest.as_bytes()[from..]
                .iter()
                .rposition(|byte| *byte == b'(')
            else {
                return false;
            };
            rest = &rest[..from + group];
            continue;
        }
        if matches!(
            word,
            "then"
                | "do"
                | "else"
                | "elif"
                | "typeset"
                | "declare"
                | "local"
                | "export"
                | "readonly"
                | "integer"
                | "float"
        ) {
            return true;
        }
        if !(word.starts_with('-') || assignment(word).is_some()) {
            return false;
        }
        rest = rest[..word_start].trim_end();
    }
    false
}

/// Where a `${` body (after the `${`) takes an offset: the colon after a
/// name (`NAME:`, `NAME[sub]:`) or a special parameter (`@:`, `1:`) when not
/// followed by `-`, `=`, `?` or `+`.
/// `start` is where the body begins; `next_close` maps each byte to the next
/// `]` (a nested `]` ends a subscript early, which only misses an offset
/// already inside a subscript region). Returns the colon's byte index.
fn brace_offset(text: &str, start: usize, next_close: &[usize]) -> Option<usize> {
    let body = &text[start..];
    let mut len = body
        .find(|ch: char| !is_name_char(ch))
        .unwrap_or(body.len());
    if len == 0 && body.starts_with(['@', '*', '#', '?', '$', '!', '-']) {
        len = 1;
    }
    if len > 0 && body[len..].starts_with('[') && next_close[start + len] < text.len() {
        len = next_close[start + len] - start + 1;
    }
    let rest = &body[len..];
    (len > 0 && rest.starts_with(':') && !rest[1..].starts_with(['-', '=', '?', '+']))
        .then_some(start + len)
}

/// The end of the `[[` condition whose body starts at `start`: after the
/// first `]]` outside quotes, substitutions and parameter expansions, or
/// the end of the text. Linear in what it scans.
fn condition_end(text: &str, start: usize) -> usize {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Context {
        /// `'...'`.
        Single,
        /// `$'...'`, where a backslash escapes.
        AnsiC,
        /// `"..."`.
        Double,
        /// `$(...)` or `(...)`.
        Paren,
        /// `${...}`.
        Brace,
        /// `` `...` ``.
        Backtick,
    }
    let bytes = text.as_bytes();
    let mut stack: Vec<Context> = Vec::new();
    let mut index = start;
    while index < bytes.len() {
        work::step();
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();
        let mut step = 1;
        match (stack.last().copied(), byte) {
            (Some(Context::Single), b'\'') => {
                stack.pop();
            }
            (Some(Context::Single), _) => {}
            (Some(Context::AnsiC), b'\\') => step = 2,
            (Some(Context::AnsiC), b'\'') => {
                stack.pop();
            }
            (Some(Context::AnsiC), _) => {}
            (_, b'\\') => step = 2,
            (Some(Context::Double), b'"') | (Some(Context::Backtick), b'`') => {
                stack.pop();
            }
            (_, b'$') if next == Some(b'(') => {
                stack.push(Context::Paren);
                step = 2;
            }
            (_, b'$') if next == Some(b'{') => {
                stack.push(Context::Brace);
                step = 2;
            }
            (_, b'`') => stack.push(Context::Backtick),
            (Some(Context::Double), _) => {}
            (_, b'$') if next == Some(b'\'') => {
                stack.push(Context::AnsiC);
                step = 2;
            }
            (_, b'\'') => stack.push(Context::Single),
            (_, b'"') => stack.push(Context::Double),
            (_, b'(') => stack.push(Context::Paren),
            (Some(Context::Paren), b')') | (Some(Context::Brace), b'}') => {
                stack.pop();
            }
            (None, b']') if next == Some(b']') => return index + 2,
            _ => {}
        }
        index += step;
    }
    bytes.len()
}

/// `[[ ... ]]` spans holding an arithmetic operator. A quoted `]]` does
/// not close one.
fn conditions(text: &str) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(open) = text[from..].find("[[").map(|at| from + at) {
        work::step();
        let close = condition_end(text, open + 2);
        let span = &text[open..close];
        if span
            .split(|ch: char| ch.is_whitespace() || matches!(ch, '(' | ')' | '!'))
            .any(|word| TEST_OPERATORS.contains(&word))
        {
            found.push(open..close);
        }
        from = close.max(open + 2);
    }
    found
}

#[cfg(test)]
#[path = "arithmetic_tests.rs"]
mod tests;
