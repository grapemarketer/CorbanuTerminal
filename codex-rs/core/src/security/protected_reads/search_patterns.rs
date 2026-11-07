//! #1323: the words of a command that are search patterns (`rg ".env"
//! logs/`), not paths the command opens. A search that exits 0 can print old
//! denial lines naming its pattern; such a word explains nothing.
//!
//! Text-based and conservative: only `grep`, `egrep`, `fgrep`, `rg` and
//! `git grep` are recognised, a search ends at a separator or a new line,
//! and where the text is unclear (a process substitution, a script whose
//! lines cannot be told apart) nothing is marked. An option this does not
//! know is taken as a flag, so its value, not a later operand, is taken for
//! the pattern (a value that is a path, as for an unknown abbreviation, is
//! then missed). Explanation only.

use std::collections::HashSet;

/// Words that end a simple command.
const SEPARATORS: [&str; 9] = ["|", "||", "&&", ";", "&", "|&", "(", ")", "!"];

/// Words after which the next word is a program.
const PROGRAM_BEFORE: [&str; 14] = [
    "if", "elif", "while", "until", "then", "do", "else", "{", "sudo", "env", "time", "nohup",
    "command", "exec",
];
/// Prefixes whose next word is always the program, never an assignment.
const NO_ASSIGNMENTS_AFTER: [&str; 3] = ["nohup", "command", "exec"];

#[derive(Clone, Copy, PartialEq)]
enum Search {
    /// `grep`, `egrep`, `fgrep`, `git grep`.
    Grep,
    /// `rg`.
    Ripgrep,
}

/// What an option takes as its value.
#[derive(Clone, Copy, PartialEq)]
enum Value {
    /// Nothing: a flag (also an option this does not know).
    Nothing,
    /// The rest of the word, or the next word.
    Any,
    /// A number: the next word only when it is one (BSD `grep -C` takes an
    /// optional count).
    Number,
}

impl Search {
    /// What a short option takes.
    fn short_value(self, option: char) -> Value {
        match (self, option) {
            (_, 'A' | 'B' | 'C' | 'm') | (Self::Ripgrep, 'j' | 'M') => Value::Number,
            (_, 'e' | 'f')
            | (Self::Grep, 'D' | 'd')
            | (Self::Ripgrep, 'E' | 'g' | 'r' | 't' | 'T') => Value::Any,
            _ => Value::Nothing,
        }
    }

    /// What a long option, without `=`, takes from the next word.
    fn long_value(self, option: &str) -> Value {
        // BSD grep's `--context` takes an optional count: never the next
        // word.
        const NUMBER: [&str; 6] = [
            "--after-context",
            "--before-context",
            "--max-count",
            "--max-depth",
            "--threads",
            "--max-columns",
        ];
        const GREP: [&str; 9] = [
            "--file",
            "--include",
            "--exclude",
            "--exclude-dir",
            "--exclude-from",
            "--label",
            "--devices",
            "--directories",
            "--binary-files",
        ];
        const RIPGREP: [&str; 16] = [
            "--file",
            "--hostname-bin",
            "--pre",
            "--pre-glob",
            "--ignore-file",
            "--color",
            "--glob",
            "--iglob",
            "--type",
            "--type-not",
            "--type-add",
            "--replace",
            "--encoding",
            "--max-filesize",
            "--sort",
            "--sortr",
        ];
        let any = match self {
            Self::Grep => {
                GREP.contains(&option)
                    // GNU grep accepts unambiguous long-option prefixes.
                    // `--exclude-f` distinguishes this path-valued option
                    // from `--exclude` and `--exclude-dir`.
                    || (option.starts_with("--exclude-f") && "--exclude-from".starts_with(option))
            }
            Self::Ripgrep => RIPGREP.contains(&option),
        };
        if NUMBER.contains(&option) || (self == Self::Ripgrep && option == "--context") {
            Value::Number
        } else if any {
            Value::Any
        } else {
            Value::Nothing
        }
    }

    /// Modes in which no word is a pattern (`rg --files` lists paths).
    fn no_pattern(self, option: &str) -> bool {
        self == Self::Ripgrep && matches!(option, "--files" | "--type-list")
    }
}

/// Whether the word after an option taking `value` is that value.
fn takes_next(value: Value, next: Option<&String>) -> bool {
    match value {
        Value::Nothing => false,
        Value::Any => next.is_some(),
        Value::Number => {
            next.is_some_and(|word| !word.is_empty() && word.chars().all(|c| c.is_ascii_digit()))
        }
    }
}

/// The indices into `words` (`command` split into words by
/// [`super::command_words`]) of the search patterns: a pattern word
/// (`rg PATTERN path`), the value of `-e` (`grep -e PATTERN path`), or
/// `--regexp=PATTERN`.
pub(super) fn search_pattern_indices(
    command: &[String],
    words: &[String],
    windows: bool,
) -> HashSet<usize> {
    let mut patterns = HashSet::new();
    let Some(starts) = line_starts(command, words, windows) else {
        return patterns;
    };
    let Some((literal_ends, assignments)) = literal_word_ends(command, words, windows) else {
        return patterns;
    };
    let words = Words {
        words,
        starts: &starts,
        literal_ends: &literal_ends,
        assignments: &assignments,
    };
    let mut index = 0;
    while index < words.words.len() {
        // #1480: the cheap name check first, so the walk back over prefixes
        // runs only at search programs (linear in a long `A=1 B=1 …` chain).
        let start = search_program(words.words, index)
            .filter(|_| program_position(&words, index, Assignments::Parsed));
        match start {
            // A search as the words read once unquoted. #1480: where the
            // shell runs another program (`X=1/grep .env`, `'X=1' grep .env`)
            // its patterns are not marked, but the scan still resumes after
            // it, so a later word of that command (`X=1/grep rg .env`) can't
            // start a search either.
            Some((search, first)) => {
                let shell_runs_it = !words.assignment(index)
                    && program_position(&words, index, Assignments::AsWritten);
                let mut not_patterns = HashSet::new();
                let marked = if shell_runs_it {
                    &mut patterns
                } else {
                    &mut not_patterns
                };
                index = mark_patterns(&words, search, first, marked);
            }
            None => index += 1,
        }
    }
    patterns
}

/// A command's words, and which of them start a new line of a script.
struct Words<'a> {
    words: &'a [String],
    starts: &'a [bool],
    literal_ends: &'a [bool],
    /// #1480: which words the shell takes for an assignment: one whose name
    /// and `=` are neither quoted nor escaped (`'X=1'` and `X\=1` are not).
    assignments: &'a [bool],
}

impl Words<'_> {
    /// Whether the command `words[index]` is in ends with it (`x;`, the
    /// last word of a line).
    fn ends(&self, index: usize) -> bool {
        (!self.literal_ends[index] && self.words[index].ends_with([';', '|', '&']))
            || self.starts.get(index + 1).copied().unwrap_or(true)
    }

    fn separator(&self, index: usize) -> bool {
        !self.literal_ends[index] && SEPARATORS.contains(&self.words[index].as_str())
    }

    fn assignment(&self, index: usize) -> bool {
        self.assignments[index]
    }
}

/// Whether each word's last character was quoted or escaped, and (#1480)
/// whether it is an assignment as written, before unquoting. Shlex removes
/// these distinctions: `grep 'a|' rg .env` must not start another search at
/// `rg`, and `'X=1' grep` runs a program named `X=1`. Keep its tokenization
/// authoritative; unclear or mismatching words bail out.
fn literal_word_ends(
    command: &[String],
    words: &[String],
    windows: bool,
) -> Option<(Vec<bool>, Vec<bool>)> {
    let posix = !windows
        || command
            .first()
            .is_some_and(|program| super::posix_shell(program));
    if !posix {
        return Some((
            vec![false; words.len()],
            words.iter().map(|word| assignment_word(word)).collect(),
        ));
    }
    let mut joined = Vec::with_capacity(words.len());
    let mut literal_ends = Vec::with_capacity(words.len());
    let mut assignments = Vec::with_capacity(words.len());
    for argument in command {
        let mut raw_words = Vec::new();
        let (mut start, mut quote, mut escaped, mut comment) = (None, None, false, false);
        let mut literal_end = false;
        for (offset, c) in argument.char_indices() {
            if comment {
                comment = c != '\n';
            } else if escaped {
                if c != '\n' {
                    literal_end = true;
                }
                escaped = false;
            } else if let Some(open) = quote {
                if c == open {
                    quote = None;
                } else if c == '\\' && open == '"' {
                    escaped = true;
                } else {
                    literal_end = true;
                }
            } else if matches!(c, ' ' | '\t' | '\n') {
                if let Some(begin) = start.take() {
                    raw_words.push((&argument[begin..offset], literal_end));
                }
            } else if c == '#' && start.is_none() {
                comment = true;
            } else {
                start.get_or_insert(offset);
                match c {
                    '\'' | '"' => quote = Some(c),
                    '\\' => escaped = true,
                    _ => literal_end = false,
                }
            }
        }
        if quote.is_some() || escaped {
            return None;
        }
        if let Some(begin) = start {
            raw_words.push((&argument[begin..], literal_end));
        }
        for (raw, literal_end) in raw_words {
            let parsed = super::argument_words(raw, /*posix*/ true);
            if parsed.len() > 1 {
                return None;
            }
            // A standalone backslash-newline produces no word.
            literal_ends.extend(std::iter::repeat_n(literal_end, parsed.len()));
            // Quotes or a backslash in the name or before the `=` leave no
            // valid name in the raw text.
            assignments.extend(std::iter::repeat_n(assignment_word(raw), parsed.len()));
            joined.extend(parsed);
        }
    }
    (joined == words).then_some((literal_ends, assignments))
}

/// For each of `words`, whether it starts a new line of the script it is
/// in: a newline outside quotes ends a command, as `;` does, but the words
/// of a POSIX script ([`super::command_words`]) do not show it. `None` when
/// the lines' words are not the script's (a quote or comment this does not
/// follow), or when a script that no POSIX shell runs has several lines.
fn line_starts(command: &[String], words: &[String], windows: bool) -> Option<Vec<bool>> {
    let posix = !windows
        || command
            .first()
            .is_some_and(|program| super::posix_shell(program));
    let mut starts = Vec::with_capacity(words.len());
    let mut joined: Vec<String> = Vec::with_capacity(words.len());
    for argument in command {
        if !argument.contains('\n') {
            let argument_words = super::argument_words(argument, posix);
            starts.extend(std::iter::repeat_n(false, argument_words.len()));
            joined.extend(argument_words);
            continue;
        }
        if !posix {
            return None;
        }
        let whole = super::argument_words(argument, posix);
        let before = joined.len();
        for line in unquoted_lines(argument) {
            let line_words = super::argument_words(line, posix);
            starts.extend((0..line_words.len()).map(|index| index == 0));
            joined.extend(line_words);
        }
        if joined[before..] != whole[..] {
            return None;
        }
    }
    (joined == words).then_some(starts)
}

/// `script` split at its newlines outside quotes and comments (a newline
/// after `\` continues the line).
fn unquoted_lines(script: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let (mut start, mut quote, mut escaped, mut comment) = (0, None, false, false);
    let mut previous = ' ';
    for (offset, c) in script.char_indices() {
        let (was_escaped, was_quoted) = (escaped, quote.is_some());
        if escaped {
            escaped = false;
        } else if comment {
            comment = c != '\n';
        } else if let Some(open) = quote {
            if c == open {
                quote = None;
            } else if c == '\\' && open == '"' {
                escaped = true;
            }
        } else {
            match c {
                '\\' => escaped = true,
                '\'' | '"' => quote = Some(c),
                '#' if previous.is_whitespace() => comment = true,
                _ => {}
            }
        }
        if c == '\n' && !was_quoted && !was_escaped {
            lines.push(&script[start..offset]);
            start = offset + 1;
        }
        previous = c;
    }
    lines.push(&script[start..]);
    lines
}

/// Whether `words[index]` is where a program is named.
/// `assignments` says which words count as assignments: those of the
/// unquoted words, or (#1480) those the shell takes for one as written.
fn program_position(lines: &Words<'_>, mut index: usize, assignments: Assignments) -> bool {
    let words = lines.words;
    let assignment = |index: usize| match assignments {
        Assignments::Parsed => assignment_word(&words[index]),
        Assignments::AsWritten => lines.assignment(index),
    };
    // Prefixes must themselves start a command: `LC_ALL=C grep` and
    // `sudo grep`, but not `cat LC_ALL=C grep` or `cat do grep`.
    while index > 0
        && !lines.starts[index]
        && !lines.ends(index - 1)
        && (assignment(index - 1)
            || (PROGRAM_BEFORE.contains(&words[index - 1].as_str())
                // `nohup X=1 grep` runs a program named `X=1`. The word as
                // the shell passes it (also quoted: `nohup "X=1/grep"`).
                && !(assignment_word(&words[index])
                    && NO_ASSIGNMENTS_AFTER.contains(&words[index - 1].as_str()))))
    {
        index -= 1;
    }
    if lines.starts[index] {
        return true;
    }
    let Some(previous) = index.checked_sub(1).map(|index| words[index].as_str()) else {
        return true;
    };
    if lines.separator(index - 1) || lines.ends(index - 1) {
        return true;
    }
    // `bash -c 'grep …'`, `sh -lc …`: the script's first word.
    let is_shell_option =
        previous.starts_with('-') && !previous.starts_with("--") && previous.contains('c');
    is_shell_option
        && index
            .checked_sub(2)
            .is_some_and(|index| super::posix_shell(&words[index]))
}

/// A shell assignment has a variable name before its first `=`. Paths,
/// options, array subscripts and other expressions do not count as names.
fn assignment_word(word: &str) -> bool {
    let Some((name, _value)) = word.split_once('=') else {
        return false;
    };
    let mut name = name.bytes();
    name.next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && name.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Which words [`program_position`] takes for assignments.
#[derive(Clone, Copy)]
enum Assignments {
    /// Those of the unquoted words.
    Parsed,
    /// #1480: those the shell takes for one, as written (not `'X=1'`).
    AsWritten,
}

/// The search `words[index]` starts, and the index of its first argument.
/// #1480: the caller marks nothing where it is an assignment word
/// (`X=1/grep .env` runs `.env`). After `nohup`, `command` or `exec` such a
/// word, quoted or not, is the program, a path ending in `/grep`, but
/// [`program_position`] does not take it, so its pattern stays unmarked.
fn search_program(words: &[String], index: usize) -> Option<(Search, usize)> {
    let word = words[index].trim_start_matches('(');
    let name = word.rsplit('/').next().unwrap_or(word);
    match name {
        "grep" | "egrep" | "fgrep" => Some((Search::Grep, index + 1)),
        "rg" => Some((Search::Ripgrep, index + 1)),
        "git" if words.get(index + 1).is_some_and(|word| word == "grep") => {
            Some((Search::Grep, index + 2))
        }
        _ => None,
    }
}

/// Marks the patterns of the search whose arguments start at `first`, and
/// returns the index of the word after its last argument.
fn mark_patterns(
    lines: &Words<'_>,
    search: Search,
    first: usize,
    patterns: &mut HashSet<usize>,
) -> usize {
    let words = lines.words;
    let mut operands = Vec::new();
    let mut given = Vec::new();
    // `-e` or `-f` gives the patterns, or a mode takes none, or a process
    // substitution makes the operands unclear: no operand is a pattern then.
    let mut no_operand_pattern = false;
    let mut options_end = false;
    let mut index = first;
    // The value of the option at `index` when the next word is it.
    let next =
        |index: usize, value: Value| !lines.ends(index) && takes_next(value, words.get(index + 1));
    while index < words.len() {
        let word = words[index].as_str();
        if lines.separator(index) {
            break;
        }
        if word.contains("<(") || word.contains(">(") {
            no_operand_pattern = true;
        } else if !options_end && word == "--" {
            options_end = true;
        } else if !options_end && word.starts_with("--") {
            let (option, attached) = match word.split_once('=') {
                Some((option, _)) => (option, true),
                None => (word, false),
            };
            // GNU getopt takes an unambiguous abbreviation (`--reg`).
            let option = if search == Search::Grep
                && option.len() >= "--reg".len()
                && "--regexp".starts_with(option)
            {
                "--regexp"
            } else {
                option
            };
            match option {
                "--regexp" => {
                    no_operand_pattern = true;
                    if attached {
                        given.push(index);
                    } else if next(index, Value::Any) {
                        index += 1;
                        given.push(index);
                    }
                }
                "--file" => no_operand_pattern = true,
                _ => no_operand_pattern |= search.no_pattern(option),
            }
            if !attached && option != "--regexp" && next(index, search.long_value(option)) {
                index += 1;
            }
        } else if !options_end && word.starts_with('-') && word.len() > 1 {
            // A cluster of short options (`-inA3`, `-ie`, `-e.env`).
            let options: Vec<char> = word.chars().skip(1).collect();
            for (position, &option) in options.iter().enumerate() {
                if option == 'e' || option == 'f' {
                    no_operand_pattern = true;
                }
                let value = search.short_value(option);
                if value != Value::Nothing {
                    // The rest of the word, or the next word.
                    if position + 1 == options.len() && next(index, value) {
                        index += 1;
                        if option == 'e' {
                            given.push(index);
                        }
                    }
                    break;
                }
            }
        } else if let Some(bare) = redirection(word) {
            // `2>/dev/null`; `> out`, `2> err`, `&>> log` take the next word.
            if bare && next(index, Value::Any) {
                index += 1;
            }
        } else {
            operands.push(index);
        }
        // The command ends with this word, or with the value it took.
        let ended = lines.ends(index);
        index += 1;
        if ended {
            break;
        }
    }
    // A pattern that runs a command (`grep "$(cat ~/.ssh/id_rsa)" notes`)
    // names the paths that command opens: kept.
    let plain = |&index: &usize| {
        words
            .get(index)
            .is_some_and(|word| !word.contains("$(") && !word.contains('`'))
    };
    patterns.extend(given.into_iter().filter(plain));
    if !no_operand_pattern
        && let Some(&pattern) = operands.first()
        && plain(&pattern)
    {
        patterns.insert(pattern);
    }
    index
}

/// Whether `word` redirects (`>out`, `2>/dev/null`, `<in`, `&>log`), and
/// if so whether its target is the next word (`>`, `2>>`, `&>`).
fn redirection(word: &str) -> Option<bool> {
    let rest = word
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .trim_start_matches('&');
    rest.starts_with(['<', '>'])
        .then(|| rest.chars().all(|c| matches!(c, '<' | '>')))
}

#[cfg(test)]
#[path = "search_patterns_tests.rs"]
mod tests;
