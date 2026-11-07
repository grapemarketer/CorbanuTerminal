//! #1276 items 1 and 5: input a launcher puts into the line it runs may be
//! the program itself (`printf git | xargs -I % % config core.pager sh`,
//! `find . -exec {} \;`, `xargs env < cmds`), code an interpreter or shell
//! runs (`xargs -I% python3 -c 'os.system("git config %")'`, `awk
//! 'BEGIN{system("%")}'`, `xargs sh -c`, `parallel 'eval {}'`), or, with no
//! command given, whole command lines (`parallel < cmds`, `xjobs < cmds`).
//! The classifier sees none of that input, so each asks as code it cannot
//! see, whatever feeds the launcher.
//!
//! Read loosely, so as to ask: an option a launcher's reading does not know
//! may take the next word as its value, so the command may start at that
//! word or at the one after it; past `MAX_LAUNCHED` launchers or launched
//! commands, or `MAX_DEPTH` shell scripts deep, any line asks. Input that
//! is only added after the command's words, and whole lines from input,
//! count only for a launcher at a program position (not `rg -n parallel`).

use super::indirect;
use super::invocation;
use super::invocation::Code;
use super::launched_git::FEEDERS;
use super::launched_git::FINDERS;
use super::launched_git::PARALLEL_FLAGS;
use super::launched_git::PARALLEL_VALUED;
use super::outbound;
use super::quoted;
use super::sed;
use super::shell::SHELLS;
use super::shell::basename;
use super::work;

/// Launcher words read, and launched commands judged, in one simple
/// command; more ask.
const MAX_LAUNCHED: usize = 16;
/// Shell scripts followed into (`xargs -I% sh -c 'sh -c "% x"'`); deeper
/// asks.
const MAX_DEPTH: usize = 4;
/// Replacement strings remembered for one launcher; past this many, any
/// word may hold one.
const MAX_REPLACEMENTS: usize = 8;

/// The word that stands for input a launcher adds after the command's own
/// words (`xargs env`, `fd -x sh -c`). No command line holds it.
const ADDED: &str = "\u{1}added-input\u{1}";

/// awk and its variants: the program is code (`awk 'BEGIN{system("x")}'`).
const AWKS: &[&str] = &["awk", "gawk", "mawk", "nawk", "busybox-awk"];

/// Commands that run their arguments as code in the shell they are in.
const EVALUATORS: &[&str] = &["eval", "source", ".", "trap"];

/// Programs that run the program named in their words but that the
/// command-word reading does not skip (`xargs -I% strace %`).
const RUNNERS: &[&str] = &["strace", "ltrace", "chroot"];

/// How a shell's script refers to its positional parameters, which a
/// launcher's input fills when it is in the shell's later arguments
/// (`find . -exec sh -c '"$1" x' _ {} \;`).
const POSITIONAL: &[&str] = &[
    "$@", "$*", "$0", "$1", "$2", "$3", "$4", "$5", "$6", "$7", "$8", "$9", "${", "$argv",
];

/// Where a launcher's input may go in the line it runs.
#[derive(Clone, Default)]
struct Fills {
    replacements: Vec<String>,
    /// More replacement strings than remembered.
    any: bool,
    /// The placeholders a finder or `parallel` replaces.
    placeholder: Placeholder,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Placeholder {
    #[default]
    None,
    /// `find`, `bfs`: `{}`.
    Find,
    /// `fd`, `parallel`, `rush`: `{}`, `{.}`, `{/}`, `{//}`, `{/.}`, `{#}`,
    /// `{%}`, `{1}`, `{=perl=}`, `{+.}`, rush's `{:}`, `{^x}`, `{@re}`,
    /// `{?}`; not awk's `{print}` or `{++n}`.
    Braced,
}

impl Placeholder {
    fn in_text(self, text: &str) -> bool {
        match self {
            Self::None => false,
            Self::Find => text.contains("{}"),
            Self::Braced => text.match_indices('{').any(|(at, _)| {
                work::step();
                let after = &text[at + 1..];
                after.starts_with(|ch: char| ch.is_ascii_digit() || "}./#%=:^@?,".contains(ch))
                    || after.starts_with("+.")
                    || after.starts_with("+/")
                    || after
                        .strip_prefix('-')
                        .is_some_and(|rest| rest.starts_with(|ch: char| ch.is_ascii_digit()))
            }),
        }
    }
}

impl Fills {
    fn new(placeholder: Placeholder) -> Self {
        Self {
            placeholder,
            ..Self::default()
        }
    }

    /// How `code`, a shell's script, may refer to its positional
    /// parameters: `$1`, `"$@"`, and the variables of loops over them
    /// (`for f; do`, `for f do`, `select f;`).
    fn positional(code: &str) -> Self {
        let mut replacements: Vec<String> =
            POSITIONAL.iter().map(|name| (*name).to_string()).collect();
        let words: Vec<&str> = code
            .split(|ch: char| ch.is_whitespace() || ch == ';')
            .filter(|word| !word.is_empty())
            .collect();
        for pair in words.windows(3) {
            work::step();
            if matches!(pair[0], "for" | "select") && pair[2] != "in" {
                replacements.push(format!("${}", pair[1]));
            }
        }
        // `for f;` at the end of the words too.
        if let [.., keyword, name] = words.as_slice()
            && matches!(*keyword, "for" | "select")
        {
            replacements.push(format!("${name}"));
        }
        let any = replacements.len() > POSITIONAL.len() + MAX_REPLACEMENTS;
        Self {
            replacements,
            any,
            ..Self::default()
        }
    }

    fn push(&mut self, replacement: &str) {
        if replacement.is_empty() {
            return;
        }
        if self.replacements.len() == MAX_REPLACEMENTS {
            self.any = true;
        } else {
            self.replacements.push(replacement.to_string());
        }
    }

    /// Whether input may be put in `text`, added after the words too.
    fn fill(&self, text: &str) -> bool {
        text.contains(ADDED) || self.placed(text)
    }

    /// Whether input may be put in `text` where it is.
    fn placed(&self, text: &str) -> bool {
        self.any
            || self.placeholder.in_text(text)
            || self
                .replacements
                .iter()
                .any(|replacement| text.contains(replacement.as_str()))
    }
}

/// One command a launcher runs.
enum Launched {
    /// The launcher runs its input as whole command lines.
    Lines,
    /// `parallel`/`rush`: the words are joined into one shell line.
    /// `piped`: the input goes to its standard input (`--pipe`).
    Line {
        words: Vec<String>,
        fills: Fills,
        piped: bool,
    },
    Words(Vec<String>, Fills),
}

/// Whether input a launcher in `texts` (one simple command's words, as the
/// quote-aware reading gives them) may run as a program or as code.
/// `data`: the words are quoted text read as code (`rg 'parallel' -S`),
/// where no launcher counts as at a program position.
pub(super) fn input_runs(texts: &[String], data: bool) -> bool {
    let lower: Vec<String> = texts.iter().map(|text| text.to_lowercase()).collect();
    let programs = if data {
        vec![false; texts.len()]
    } else {
        let Some(programs) = program_positions(texts) else {
            return true;
        };
        programs
    };
    let mut launched = Vec::new();
    let mut launchers = 0;
    for (at, word) in lower.iter().enumerate() {
        work::step();
        let name = basename(word);
        let program = programs[at];
        let args = &texts[at + 1..];
        match name {
            _ if FEEDERS.contains(&name) => xargs(args, name == "xjobs", program, &mut launched),
            "parallel" | "env_parallel" => parallel(args, program, &mut launched),
            "rush" => rush(args, &mut launched),
            _ if FINDERS.contains(&name) => {
                finder(
                    args,
                    matches!(name, "fd" | "fdfind"),
                    program,
                    &mut launched,
                );
            }
            _ => continue,
        }
        launchers += 1;
        if launchers > MAX_LAUNCHED || launched.len() > MAX_LAUNCHED {
            return true;
        }
    }
    launched.iter().any(|launched| {
        work::step();
        match launched {
            Launched::Lines => true,
            Launched::Line {
                words,
                fills,
                piped,
            } => {
                let line = words.join(" ");
                (*piped && stdin_code(&line)) || line_runs(&line, fills, /*depth*/ 0)
            }
            Launched::Words(words, fills) => words_run(words, fills, /*depth*/ 0),
        }
    })
}

/// #1276 follow-up: whether a launcher's input may run as a program or as
/// code in the text `eval` (its words joined, past a `--`) or `trap` (each
/// operand) runs as a shell line, read with its program positions: such
/// text is walked as quoted data when it holds no quotes of its own (`eval
/// 'xargs env < f'`, `trap 'parallel < cmds' EXIT`).
pub(super) fn evaluated_input_runs(name: &str, args: &[String]) -> bool {
    evaluated(name, args, /*depth*/ 0)
}

/// `evaluated_input_runs`, `depth` evaluations deep; deeper than
/// `MAX_DEPTH` asks.
fn evaluated(name: &str, args: &[String], depth: usize) -> bool {
    let lines = match name {
        "eval" => vec![
            args.strip_prefix(&["--".to_string()])
                .unwrap_or(args)
                .join(" "),
        ],
        // A signal name read as a command only over-matches.
        "trap" => args.to_vec(),
        _ => return false,
    };
    if depth > MAX_DEPTH {
        return true;
    }
    lines.iter().any(|line| {
        work::step();
        quoted::commands(line).commands.iter().any(|command| {
            work::step();
            let texts: Vec<String> = command.words.iter().map(|word| word.text.clone()).collect();
            // `eval`s at its command word (behind `command` or `builtin`,
            // with a `--`) run the rest: skipped in one pass (`eval eval --
            // xargs env`).
            let mut start = 0;
            let mut evals = false;
            while let Some((index, word)) = indirect::command_word(&texts[start..])
                && word == "eval"
            {
                work::step();
                start += index + 1;
                if texts.get(start).is_some_and(|text| text == "--") {
                    start += 1;
                }
                evals = true;
            }
            let rest = &texts[start..];
            // Text those `eval`s parse again (`trap "eval 'xargs env < f'"
            // EXIT`), and a `trap` in the text.
            let parsed_again = evals
                && rest.iter().any(|text| {
                    work::step();
                    text.contains(|ch: char| ch.is_whitespace() || "'\"\\;|&<>`$(".contains(ch))
                });
            input_runs(rest, /*data*/ false)
                || (parsed_again && evaluated("eval", rest, depth + 1))
                || indirect::command_word(rest).is_some_and(|(index, name)| {
                    name == "trap" && evaluated("trap", &rest[index + 1..], depth + 1)
                })
        })
    })
}

/// For each word, whether it is where a program may start: a command word,
/// a wrapper's, or one an exec wrapper or `-exec` runs. `None` past the
/// places read.
fn program_positions(texts: &[String]) -> Option<Vec<bool>> {
    let (starts, more) = sed::command_starts(texts);
    if more {
        return None;
    }
    let mut programs = vec![false; texts.len()];
    for start in starts {
        work::step();
        for at in indirect::programs(&texts[start..]) {
            work::step();
            programs[start + at] = true;
        }
    }
    Some(programs)
}

/// The words of a command a launcher runs, from `at`, with the word for
/// input it adds after them (`added`).
fn command_words(words: &[String], added: bool) -> Vec<String> {
    let mut command = words.to_vec();
    if added {
        command.push(ADDED.to_string());
    }
    command
}

/// `xargs`/`xjobs` and its options: the command starts at the first word
/// that is not an option or an option's value (or at the word after an
/// option it does not know). Without a replacement string, input is added
/// after the command's words. xjobs without a command, or with `-s
/// script`, runs its input lines as commands. `program`: the launcher is
/// at a program position.
fn xargs(words: &[String], xjobs: bool, program: bool, launched: &mut Vec<Launched>) {
    let mut fills = Fills::default();
    let mut start = 0;
    // #1276 follow-up: after a value an option this reading does not know
    // may take, xargs' options are read on (`xargs --frob x -I% %`).
    for reading in 0.. {
        work::step();
        if reading == MAX_LAUNCHED {
            launched.push(Launched::Lines);
            return;
        }
        let Some((at, unsure)) = xargs_options(words, start, xjobs, program, &mut fills, launched)
        else {
            return;
        };
        if at >= words.len() {
            if xjobs && program {
                launched.push(Launched::Lines);
            }
            return;
        }
        // Without a replacement string, input words go after the command's.
        let added = program && fills.replacements.is_empty();
        if unsure && at + 1 < words.len() {
            launched.push(Launched::Words(
                command_words(&words[at + 1..], added),
                fills.clone(),
            ));
        }
        launched.push(Launched::Words(
            command_words(&words[at..], added),
            fills.clone(),
        ));
        if !(unsure && words.get(at + 1).is_some_and(|word| word.starts_with('-'))) {
            return;
        }
        start = at + 1;
    }
}

/// Reads `xargs`/`xjobs` options in `words` from `start`, adding the
/// replacement strings they set to `fills`: where the command starts, and
/// whether the option before it is one this reading does not know. `None`
/// once xjobs' `-s` gave its script (pushed to `launched`).
fn xargs_options(
    words: &[String],
    start: usize,
    xjobs: bool,
    program: bool,
    fills: &mut Fills,
    launched: &mut Vec<Launched>,
) -> Option<(usize, bool)> {
    const XARGS_VALUED: &[&str] = &[
        "arg-file",
        "delimiter",
        "max-args",
        "max-procs",
        "max-chars",
        "process-slot-var",
    ];
    const XARGS_ATTACHED: &[&str] = &["eof", "replace", "max-lines"];
    const XARGS_FLAGS: &[&str] = &[
        "null",
        "interactive",
        "no-run-if-empty",
        "verbose",
        "exit",
        "open-tty",
        "show-limits",
        "help",
        "version",
    ];
    // The word before the command is an option this reading does not know.
    let mut unsure = false;
    let mut at = start;
    while let Some(word) = words.get(at) {
        work::step();
        if word == "--" {
            at += 1;
            unsure = false;
            break;
        }
        if !word.starts_with('-') || word == "-" {
            break;
        }
        unsure = false;
        if let Some(long) = word.strip_prefix("--").filter(|_| !xjobs) {
            let (option, value) = long
                .split_once('=')
                .map_or((long, None), |(option, value)| (option, Some(value)));
            let pick = |names: &[&'static str]| names.iter().any(|name| name.starts_with(option));
            if option.is_empty() {
                unsure = value.is_none();
            } else if "replace".starts_with(option) && option.starts_with('r') {
                fills.push(value.unwrap_or("{}"));
            } else if pick(XARGS_VALUED) {
                at += usize::from(value.is_none());
            } else if !(pick(XARGS_ATTACHED) || pick(XARGS_FLAGS)) {
                unsure = value.is_none();
            }
            at += 1;
            continue;
        }
        let Some(group) = word.strip_prefix('-').filter(|group| !group.is_empty()) else {
            break;
        };
        at += 1;
        for (index, letter) in group.char_indices() {
            work::step();
            let rest = &group[index + letter.len_utf8()..];
            let valued = if xjobs { "jlqv" } else { "adEIJLnPRsS" };
            if xjobs && letter == 's' {
                if program {
                    launched.push(Launched::Lines);
                }
                return None;
            }
            if valued.contains(letter) {
                let value = if rest.is_empty() {
                    at += 1;
                    words.get(at - 1).map_or("", String::as_str)
                } else {
                    rest
                };
                if !xjobs && matches!(letter, 'I' | 'J') {
                    fills.push(value);
                }
                break;
            }
            if !xjobs && matches!(letter, 'e' | 'i' | 'l') {
                if letter == 'i' {
                    fills.push(if rest.is_empty() { "{}" } else { rest });
                }
                break;
            }
            let flags = if xjobs { "01dnptV" } else { "0oprtx" };
            if !flags.contains(letter) {
                unsure = rest.is_empty();
                break;
            }
        }
    }
    Some((at, unsure))
}

/// GNU `parallel`'s replacement-string options, by full name (`-I`,
/// `--extensionreplace`/`--er`, `--bnr`, `--dnr`, `--bner`, `--rpl 'R
/// perl'`); a long one also by an abbreviation of four letters or more
/// (`--extensionrep=@`).
fn sets_replacement(option: &str) -> bool {
    const LONG: &[&str] = &[
        "--replace",
        "--extensionreplace",
        "--basenamereplace",
        "--dirnamereplace",
        "--basenameextensionreplace",
        "--seqreplace",
        "--slotreplace",
        "--rpl",
    ];
    matches!(
        option,
        "-I" | "--er" | "--bnr" | "--dnr" | "--bner" | "--rpl" | "--parens"
    ) || (option.len() >= 6
        && LONG.iter().any(|long| {
            work::step();
            long.starts_with(option)
        }))
}

/// GNU `parallel` options that take a value, besides `PARALLEL_VALUED`
/// (kept apart: that list also tells `launched_git` an option sets no
/// replacement string).
const PARALLEL_MORE_VALUED: &[&str] = &[
    "--nice",
    "--env",
    "--header",
    "--tagstring",
    "-s",
    "--max-chars",
    "-E",
    "--eof",
    "-L",
    "--max-lines",
    "--basefile",
    "--bf",
    "--arg-sep",
    "--arg-file-sep",
    "--sshloginfile",
    "--slf",
    "--transferfile",
    "--tf",
    "--return",
    "--trc",
    "--shebang-wrap",
    "--sqlmaster",
    "--sqlworker",
    "--sqlandworker",
    "--limit",
    "--termseq",
    "--ctag",
];

/// GNU `parallel` options that print something and run nothing.
const PARALLEL_INFO: &[&str] = &[
    "--version",
    "--help",
    "-h",
    "--number-of-cores",
    "--number-of-cpus",
    "--number-of-sockets",
    "--number-of-threads",
    "--max-line-length-allowed",
    "--minversion",
    "--citation",
    "--bibtex",
    "--record-env",
];

/// How far `parallel`'s options were read from a start.
struct ParallelOptions {
    /// Where the command (or the `:::` arguments) starts.
    at: usize,
    /// The last option is one this reading does not know, which may take
    /// the word at `at` as its value.
    unsure: bool,
    file_input: bool,
    piped: bool,
    info: bool,
    /// Replacement strings `-I` may set bundled after other letters
    /// (`-kI@`): input may be put where they are, but they do not keep
    /// input from being added after the words as well.
    bundled: Vec<String>,
}

/// Reads `parallel`'s options in `words` from `start`, adding replacement
/// strings they set to `fills`.
fn parallel_options(words: &[String], start: usize, fills: &mut Fills) -> ParallelOptions {
    let mut read = ParallelOptions {
        at: start,
        unsure: false,
        file_input: false,
        piped: false,
        info: false,
        bundled: Vec::new(),
    };
    while let Some(word) = words.get(read.at) {
        work::step();
        if word.starts_with(":::") || !word.starts_with('-') || word == "-" {
            break;
        }
        read.at += 1;
        read.unsure = false;
        if word == "--" {
            break;
        }
        let (option, value) = word
            .split_once('=')
            .map_or((word.as_str(), None), |(option, value)| {
                (option, Some(value))
            });
        read.file_input |= option.starts_with("-a") || option.starts_with("--a");
        read.piped |= matches!(option, "--pipe" | "--pipepart");
        read.info |= PARALLEL_INFO.contains(&option);
        if option == "--parens" {
            // Other placeholder brackets.
            fills.any = true;
        }
        // #1276 follow-up: `-I` bundled after other letters (`-kI@`) may
        // set one too; the option is still read as one this reading does
        // not know (below).
        if let Some(group) = option
            .strip_prefix('-')
            .filter(|group| !group.starts_with('-'))
            && let Some(at) = group.find('I').filter(|at| *at > 0)
            && at + 1 < group.len()
        {
            read.bundled.push(group[at + 1..].to_string());
        }
        // `-I%` attached too.
        let attached_replacement = option.strip_prefix("-I").filter(|rest| !rest.is_empty());
        if attached_replacement.is_some() || sets_replacement(option) {
            let value = match (attached_replacement, value) {
                (Some(replacement), _) => replacement,
                (None, Some(value)) => value,
                (None, None) => {
                    read.at += 1;
                    words.get(read.at - 1).map_or("", String::as_str)
                }
            };
            fills.push(value.split_whitespace().next().unwrap_or(""));
            continue;
        }
        let counted = ["-j", "-P", "-N", "-n"].iter().any(|short| {
            option.strip_prefix(short).is_some_and(|count| {
                !count.is_empty()
                    && count
                        .chars()
                        .all(|ch| ch.is_ascii_digit() || "+-%".contains(ch))
            })
        });
        if PARALLEL_VALUED.contains(&option) || PARALLEL_MORE_VALUED.contains(&option) {
            read.at += usize::from(value.is_none());
        } else if !(counted || PARALLEL_FLAGS.contains(&option) || read.info) {
            // An option this reading does not know: if it takes a value,
            // that is the next word, which may set a replacement string.
            read.unsure = value.is_none();
        }
    }
    // A value option at the end has no value.
    read.at = read.at.min(words.len());
    read
}

/// Readings of `parallel`'s options kept when unknown options may take a
/// value; past this many the last reading stands.
const MAX_PARALLEL_READINGS: usize = 4;

/// GNU `parallel` (`env_parallel`): the command words before the first
/// `:::`-style argument are joined and run as a shell line, with input put
/// where `{...}` placeholders and replacement strings are, or added at the
/// end when the line holds none. Without a command it runs its input lines
/// as commands, unless they are only its own literal `:::` arguments (judged
/// as lines elsewhere) or it only prints (`--version`). An option this
/// reading does not know may take the next word as its value (which may
/// set a replacement string): then the options are read on from the word
/// after it too.
fn parallel(words: &[String], program: bool, launched: &mut Vec<Launched>) {
    let mut fills = Fills::new(Placeholder::Braced);
    let mut bundled = Vec::new();
    let mut start = 0;
    for reading in 0..MAX_PARALLEL_READINGS {
        work::step();
        let read = parallel_options(words, start, &mut fills);
        let at = read.at;
        let end = words[at..]
            .iter()
            .position(|word| word.starts_with(":::"))
            .map_or(words.len(), |offset| at + offset);
        if at == end {
            let literal_only = end < words.len()
                && !read.file_input
                && words[end..].iter().all(|word| {
                    work::step();
                    !word.starts_with("::::")
                        && !word.contains(['$', '`'])
                        && !word.contains(super::shell::UNSEEN_OUTPUT)
                });
            if program && !literal_only && !read.info {
                launched.push(Launched::Lines);
            }
            return;
        }
        bundled.extend(read.bundled);
        launched.push(parallel_line(
            &words[at..end],
            fills.clone(),
            &bundled,
            read.piped,
            program,
        ));
        if !read.unsure || at + 1 >= end || reading + 1 == MAX_PARALLEL_READINGS {
            return;
        }
        // The unknown option took `words[at]` as its value.
        fills.push(words[at].split_whitespace().next().unwrap_or(""));
        start = at + 1;
    }
}

/// A `parallel` line: input goes where its placeholders are, else (unless
/// piped, and for a launcher at a program position) after its words.
/// `bundled`: more places, which do not count for that.
fn parallel_line(
    words: &[String],
    mut fills: Fills,
    bundled: &[String],
    piped: bool,
    program: bool,
) -> Launched {
    let placed = words.iter().any(|word| {
        work::step();
        fills.placed(word)
    });
    let mut words = words.to_vec();
    if !placed && !piped && program {
        words.push(ADDED.to_string());
    }
    for replacement in bundled {
        work::step();
        fills.push(replacement);
    }
    Launched::Line {
        words,
        fills,
        piped,
    }
}

/// `rush [flags] command`: its flags may come anywhere; the other words
/// are the command line it runs with a shell, with input where its
/// placeholders are (and `-v name=value` names more: `{name}`). Without a
/// command it prints its input.
fn rush(words: &[String], launched: &mut Vec<Launched>) {
    rush_reading(words, /*escape_symbols*/ false, launched);
    // #1276 follow-up: `-Q`/`--escape-symbols` take a value too; read
    // both ways, so a word read as the line before stays in it.
    if words.iter().any(|word| {
        work::step();
        word.starts_with("--escape-symbols")
            || (word.starts_with('-') && !word.starts_with("--") && word.contains('Q'))
    }) {
        rush_reading(words, /*escape_symbols*/ true, launched);
    }
}

/// One reading of `rush`'s words; `escape_symbols`: `-Q` and
/// `--escape-symbols` take a value.
fn rush_reading(words: &[String], escape_symbols: bool, launched: &mut Vec<Launched>) {
    /// rush's long flags that take a value.
    const VALUED: &[&str] = &[
        "jobs",
        "retries",
        "retry-interval",
        "timeout",
        "succeed-cmd-file",
        "record-delimiter",
        "nrecords",
        "field-delimiter",
        "records-join-sep",
        "infile",
        "out-file",
        "assign",
        "trim",
        "delay",
        "load",
        "memfree",
        "cleanup-time",
        "no-stop-exes",
        "no-kill-exes",
    ];
    let mut fills = Fills::new(Placeholder::Braced);
    let mut piped = false;
    let mut line = Vec::new();
    let mut at = 0;
    while let Some(word) = words.get(at) {
        work::step();
        at += 1;
        if word == "--" {
            line.extend(words[at..].iter().cloned());
            break;
        }
        let assigned = |fills: &mut Fills, value: &str| {
            if let Some((name, _)) = value.split_once('=') {
                fills.push(&format!("{{{name}}}"));
            }
        };
        if let Some(long) = word.strip_prefix("--") {
            let (option, value) = long
                .split_once('=')
                .map_or((long, None), |(option, value)| (option, Some(value)));
            piped |= option == "pipe";
            if VALUED.contains(&option) || (escape_symbols && option == "escape-symbols") {
                let value = value.or_else(|| {
                    at += 1;
                    words.get(at - 1).map(String::as_str)
                });
                if option == "assign" {
                    assigned(&mut fills, value.unwrap_or(""));
                }
            }
            continue;
        }
        let Some(group) = word.strip_prefix('-').filter(|group| !group.is_empty()) else {
            line.push(word.clone());
            continue;
        };
        for (index, letter) in group.char_indices() {
            work::step();
            if !("jrtCDndJiovT".contains(letter) || (escape_symbols && letter == 'Q')) {
                continue;
            }
            let rest = &group[index + letter.len_utf8()..];
            let value = if rest.is_empty() {
                at += 1;
                words.get(at - 1).map_or("", String::as_str)
            } else {
                rest
            };
            if letter == 'v' {
                assigned(&mut fills, value);
            }
            break;
        }
    }
    if !line.is_empty() {
        launched.push(Launched::Line {
            words: line,
            fills,
            piped,
        });
    }
}

/// `find`/`bfs` `-exec`/`-execdir`/`-ok`/`-okdir`, and `fd` `-x`/`-X`/
/// `--exec`/`--exec-batch` (in a group too: `-HX`; or attached: `--exec=cmd`,
/// `-xcmd`): the command each runs, up to its `;` or `+`. fd adds the path
/// after a command that holds no placeholder.
fn finder(words: &[String], fd: bool, program: bool, launched: &mut Vec<Launched>) {
    let placeholder = if fd {
        Placeholder::Braced
    } else {
        Placeholder::Find
    };
    let mut at = 0;
    while let Some(word) = words.get(at) {
        work::step();
        at += 1;
        let (starts, attached) = if fd {
            fd_exec(word)
        } else {
            (
                matches!(word.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir"),
                None,
            )
        };
        if !starts {
            continue;
        }
        // `;`, or `+` right after a `{}` (find; fd takes `;` only).
        let end = (at..words.len())
            .find(|&end| {
                work::step();
                words[end] == ";"
                    || (!fd && words[end] == "+" && end > at && words[end - 1].ends_with("{}"))
            })
            .unwrap_or(words.len());
        let mut command: Vec<String> = attached.into_iter().map(str::to_string).collect();
        command.extend(words[at..end].iter().cloned());
        let fills = Fills::new(placeholder);
        let placed = command.iter().any(|word| {
            work::step();
            fills.placed(word)
        });
        if fd && program && !placed {
            command.push(ADDED.to_string());
        }
        launched.push(Launched::Words(command, fills));
        if launched.len() > MAX_LAUNCHED {
            return;
        }
        at = end;
    }
}

/// Whether an `fd` word starts an exec option, and the command it holds
/// attached. In a group of short options, the letters after one that takes
/// a value are that value (`-tx` is the type `x`).
fn fd_exec(word: &str) -> (bool, Option<&str>) {
    if let Some(long) = word.strip_prefix("--") {
        if let Some(command) = ["exec=", "exec-batch="]
            .iter()
            .find_map(|option| long.strip_prefix(option))
        {
            return (true, Some(command).filter(|command| !command.is_empty()));
        }
        return (matches!(long, "exec" | "exec-batch"), None);
    }
    let Some(group) = word.strip_prefix('-') else {
        return (false, None);
    };
    for (at, letter) in group.char_indices() {
        work::step();
        if matches!(letter, 'x' | 'X') {
            let command = &group[at + 1..];
            let command = command.strip_prefix('=').unwrap_or(command);
            return (true, Some(command).filter(|command| !command.is_empty()));
        }
        // fd's short options that take a value.
        if "tedEjScoD".contains(letter) {
            break;
        }
    }
    (false, None)
}

/// For each of a launched command's words, whether a program may start
/// there: its command word and wrappers', or every word after an exec
/// wrapper, or the word after a finder's exec option. Not after `--`,
/// which ends options (`xargs -I{} git checkout -- {}`), unless the `--`
/// is where the command word was read to be: after a wrapper's operands
/// (#1276 follow-up: `xargs -I% flock l -- %`, where a wrapper that reads
/// options after operands skips the `--`). `None` past
/// `outbound::MAX_JUDGED_POSITIONS` starts.
fn launched_programs(words: &[String]) -> Option<Vec<bool>> {
    let wrapped = indirect::command_word(words)
        .is_some_and(|(_, name)| outbound::EXEC_WRAPPERS.contains(&name.as_str()));
    let mut programs = vec![false; words.len()];
    let mut starts = 0;
    for start in 0..words.len() {
        work::step();
        let exec = start > 0
            && matches!(
                words[start - 1].as_str(),
                "-exec" | "-execdir" | "-ok" | "-okdir"
            );
        let ended = start > 0 && programs[start - 1] && words[start - 1] == "--";
        if !(start == 0 || wrapped || exec || ended) {
            continue;
        }
        starts += 1;
        if starts > outbound::MAX_JUDGED_POSITIONS {
            return None;
        }
        for at in indirect::programs(&words[start..]) {
            work::step();
            programs[start + at] = true;
        }
    }
    Some(programs)
}

/// `env -S'% x'` (`-iS'…'`, `--split-string=…`): env splits the string
/// into the words it runs. The words with every such string split, once,
/// and whether there was one; env's options end at its command. With them,
/// whether a split `S` may be in another option's value instead (`env
/// -uSSH_AUTH_SOCK`: a letter before it in its group takes a value).
fn env_split(words: &[String], options: usize) -> Option<(Vec<String>, bool)> {
    /// env's short options (GNU, BSD) that take a value.
    const VALUED: &[char] = &['u', 'C', 'a', 'P', 'L', 'U'];
    let mut env = false;
    let mut doubtful = false;
    let mut split = false;
    let mut split_next = false;
    let mut expanded = Vec::with_capacity(words.len());
    for word in &words[..options] {
        work::step();
        let value = if std::mem::take(&mut split_next) {
            Some(word.as_str())
        } else if !env {
            None
        } else if let Some(long) = word.strip_prefix("--") {
            (long.len() > 1 && "split-string".starts_with(long.split('=').next().unwrap_or("")))
                .then(|| match long.split_once('=') {
                    Some((_, value)) => Some(value),
                    None => {
                        split_next = true;
                        None
                    }
                })
                .flatten()
        } else if let Some(group) = word.strip_prefix('-')
            && let Some(at) = group.find('S')
        {
            doubtful |= group[..at].contains(VALUED);
            let value = &group[at + 1..];
            if value.is_empty() {
                split_next = true;
                None
            } else {
                Some(value)
            }
        } else {
            None
        };
        split |= split_next || value.is_some();
        match value {
            Some(value) => expanded.extend(value.split_whitespace().map(str::to_string)),
            None if split_next => {}
            None => expanded.push(word.clone()),
        }
        env |= basename(&word.to_lowercase()) == "env";
    }
    expanded.extend(words[options..].iter().cloned());
    split.then_some((expanded, doubtful))
}

/// Whether input reaches the program `words` run (or a wrapper's, or one
/// an exec wrapper or `-exec` runs: `xargs -I{} env {} config`), code an
/// evaluator, interpreter or awk runs, or a shell's script, or the
/// positional parameters a shell's script runs.
fn words_run(words: &[String], fills: &Fills, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return true;
    }
    let Some(programs) = launched_programs(words) else {
        return true;
    };
    if words
        .iter()
        .zip(&programs)
        .any(|(word, program)| *program && fills.placed(word))
    {
        return true;
    }
    let command = indirect::command_word(words);
    let options = command.as_ref().map_or(words.len(), |(at, _)| at + 1);
    // Judged once as the words env runs, and (#1276 follow-up) as written
    // when the `S` may be in another option's value (`env
    // -uSSH_AUTH_SOCK`).
    if let Some((expanded, doubtful)) = env_split(words, options) {
        if words_run(&expanded, fills, depth + 1) {
            return true;
        }
        if !doubtful {
            return false;
        }
    }
    // #1276 follow-up: a `--` read as the command word after a wrapper's
    // operands (`xargs -I% flock l -- python3 -c '…%…'`). GNU tools stop
    // reading options at their first operand and run a program named
    // `--`; one that reads options after operands skips it and runs the
    // words after it. Judged as those words too: this only adds prompts.
    if let Some((index, name)) = &command
        && name == "--"
        && *index > 0
        && words_run(&words[index + 1..], fills, depth + 1)
    {
        return true;
    }
    // Input added after words that only wrap is the program (`xargs env`).
    if command.as_ref().is_none_or(|(at, _)| words[*at] == ADDED)
        && words.iter().any(|word| word == ADDED)
    {
        return true;
    }
    let Some((index, name)) = command else {
        return false;
    };
    // A command named by a substitution or variable.
    if name.contains(['$', '`']) || name.contains(super::shell::UNSEEN_OUTPUT) {
        return words.iter().any(|word| fills.fill(word));
    }
    let args = &words[index + 1..];
    if EVALUATORS.contains(&name.as_str()) {
        return args.iter().any(|arg| fills.fill(arg));
    }
    if RUNNERS.contains(&name.as_str()) {
        return runner_program(&name, args).is_some_and(|arg| fills.fill(arg));
    }
    if AWKS.contains(&name.as_str()) {
        let programs = awk_programs(args);
        return if programs.is_empty() {
            // No program yet: input added after the words is the program.
            args.last().is_some_and(|arg| arg == ADDED)
        } else {
            programs.iter().any(|program| fills.fill(program))
        };
    }
    let shell = SHELLS.contains(&name.as_str());
    match invocation::parse(&name, args).map(|invocation| invocation.code) {
        // `xargs sh -c`: the added input is the code.
        Some(Code::Inline(None)) => args.last().is_some_and(|arg| arg == ADDED),
        Some(Code::Inline(Some(code))) => {
            if fills.fill(code) && (!shell || line_runs(code, fills, depth + 1)) {
                return true;
            }
            // `sh -c '"$1" x' _ {}`: input in the shell's arguments.
            let after = args
                .iter()
                .position(|arg| arg.ends_with(code))
                .map_or(args.len(), |at| at + 1);
            shell
                && args[after..].iter().any(|arg| fills.fill(arg))
                && line_runs(code, &Fills::positional(code), depth + 1)
        }
        _ => false,
    }
}

/// The program a runner runs: its first operand (chroot's second, after
/// the folder), past the values of chroot's `--userspec`/`--groups`.
fn runner_program<'a>(name: &str, args: &'a [String]) -> Option<&'a String> {
    let mut operands = args.iter().scan(false, |value_next, arg| {
        work::step();
        let value = std::mem::take(value_next);
        *value_next = name == "chroot" && matches!(arg.as_str(), "--userspec" | "--groups");
        Some((value, arg))
    });
    operands
        .by_ref()
        .filter(|(value, arg)| !value && !arg.starts_with('-'))
        .map(|(_, arg)| arg)
        .nth(usize::from(name == "chroot"))
}

/// Whether input reaches a program or code in the commands of a shell line
/// that holds it.
fn line_runs(line: &str, fills: &Fills, depth: usize) -> bool {
    if !fills.fill(line) {
        return false;
    }
    if depth > MAX_DEPTH {
        return true;
    }
    let reading = quoted::commands(line);
    if reading.too_deep || reading.unsure {
        return true;
    }
    reading.commands.iter().any(|command| {
        work::step();
        let texts: Vec<String> = command.words.iter().map(|word| word.text.clone()).collect();
        // A shell or interpreter reading code from its input (`echo {} |
        // sh`, `bash <<< {}`), or a command named by a variable or
        // substitution (`x={}; $x`, `$(echo {}) x`), in a line that holds
        // the input.
        indirect::command_word(&texts).is_some_and(|(_, name)| {
            name.contains(['$', '`']) || name.contains(super::shell::UNSEEN_OUTPUT)
        }) || stdin_reader(&texts)
            || words_run(&texts, fills, depth)
    })
}

/// Whether a shell line runs a shell or interpreter that reads code from
/// its standard input (`parallel --pipe bash`).
fn stdin_code(line: &str) -> bool {
    let reading = quoted::commands(line);
    reading.too_deep
        || reading.unsure
        || reading.commands.iter().any(|command| {
            work::step();
            let texts: Vec<String> = command.words.iter().map(|word| word.text.clone()).collect();
            stdin_reader(&texts)
        })
}

fn stdin_reader(texts: &[String]) -> bool {
    indirect::invocation(texts).is_some_and(|invocation| matches!(invocation.code, Code::Stdin))
}

/// awk's program texts: `-e`/`--source` values, or else its first operand
/// unless a program file is given (`-f`, `--file`, gawk `-E`).
fn awk_programs(args: &[String]) -> Vec<&str> {
    /// gawk's long options that take a value.
    const VALUED: &[&str] = &[
        "source",
        "file",
        "exec",
        "include",
        "load",
        "assign",
        "field-separator",
    ];
    let mut programs = Vec::new();
    let mut file = false;
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        work::step();
        at += 1;
        if arg == "--" {
            break;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (option, value) = long
                .split_once('=')
                .map_or((long, None), |(option, value)| (option, Some(value)));
            let Some(name) = VALUED
                .iter()
                .find(|name| option.len() > 1 && name.starts_with(option))
            else {
                continue;
            };
            let value = value.or_else(|| {
                at += 1;
                args.get(at - 1).map(String::as_str)
            });
            match *name {
                "source" => programs.extend(value),
                "file" | "exec" => file = true,
                _ => {}
            }
            continue;
        }
        let Some(group) = arg.strip_prefix('-').filter(|group| !group.is_empty()) else {
            at -= 1;
            break;
        };
        for (index, letter) in group.char_indices() {
            work::step();
            let rest = &group[index + letter.len_utf8()..];
            if !"FvfEeilW".contains(letter) {
                continue;
            }
            let value = if rest.is_empty() {
                at += 1;
                args.get(at - 1).map_or("", String::as_str)
            } else {
                rest
            };
            match letter {
                'e' => programs.push(value),
                'f' | 'E' => file = true,
                _ => {}
            }
            break;
        }
    }
    if programs.is_empty()
        && !file
        && let Some(program) = args.get(at).filter(|program| *program != ADDED)
    {
        programs.push(program);
    }
    programs
}
