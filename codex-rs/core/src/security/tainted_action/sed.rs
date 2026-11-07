//! #885: GNU sed runs shell commands: its `e` command runs a command (or
//! the line), and the `e` flag of `s` runs the line it produced. After
//! untrusted content, a sed program that may do so, or that the classifier
//! cannot read in full, is code the host cannot read. A sed command that
//! saves its output to a file run in the same command line wrote what runs.

use super::indirect;
use super::outbound;
use super::quoted::Word;
use super::work;

/// Names GNU sed runs under.
pub(super) const NAMES: &[&str] = &["sed", "gsed"];

/// Where sed runs in these words: the command word (after wrappers), any
/// word after an exec marker (`find -exec sed`), and any word an exec-style
/// wrapper runs (`uv run sed`).
pub(super) fn positions(words: &[String]) -> Vec<usize> {
    let (starts, _) = command_starts(words);
    let mut found: Vec<usize> = starts
        .into_iter()
        .filter_map(|start| {
            let (index, name) = indirect::command_word(&words[start..])?;
            NAMES.contains(&name.as_str()).then_some(start + index)
        })
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

/// Where a command may start in these words (see `positions`), at most
/// `MAX_JUDGED_POSITIONS` of them, and whether there were more (fail
/// closed; the word lexer's line is refused for it too).
pub(super) fn command_starts(words: &[String]) -> (Vec<usize>, bool) {
    let wrapped = indirect::command_word(words)
        .is_some_and(|(_, name)| outbound::EXEC_WRAPPERS.contains(&name.as_str()));
    let mut starts = (0..words.len()).filter(|&index| {
        index == 0 || wrapped || outbound::EXEC_MARKERS.contains(&words[index - 1].as_str())
    });
    let capped: Vec<usize> = starts
        .by_ref()
        .take(outbound::MAX_JUDGED_POSITIONS)
        .collect();
    let more = starts.next().is_some();
    (capped, more)
}

/// What one sed invocation is given.
#[derive(Clone, Debug, Default)]
pub(super) struct Call {
    /// Program text: each `-e`/`--expression`, or else the first operand.
    pub(super) programs: Vec<Word>,
    /// Script files (`-f`, `--file`).
    pub(super) files: Vec<Word>,
    /// The files it reads, and edits with `-i`.
    pub(super) operands: Vec<Word>,
    pub(super) in_place: bool,
    /// An option it does not know, or a word that may split into options.
    pub(super) unknown: bool,
    /// Read as BSD sed reads it (which has no `e`).
    pub(super) bsd: bool,
    /// Its options ended (`--`, or an operand under `POSIXLY_CORRECT`):
    /// words added after them are operands.
    pub(super) options_ended: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Long {
    Expression,
    File,
    InPlace,
    Value,
    Flag,
}

/// GNU sed's long options; any unambiguous prefix works (`--e`, `--fi`).
const LONG: &[(&str, Long)] = &[
    ("expression", Long::Expression),
    ("file", Long::File),
    ("in-place", Long::InPlace),
    ("line-length", Long::Value),
    ("binary", Long::Flag),
    ("debug", Long::Flag),
    ("follow-symlinks", Long::Flag),
    ("help", Long::Flag),
    ("null-data", Long::Flag),
    ("posix", Long::Flag),
    ("quiet", Long::Flag),
    ("regexp-extended", Long::Flag),
    ("sandbox", Long::Flag),
    ("separate", Long::Flag),
    ("silent", Long::Flag),
    ("unbuffered", Long::Flag),
    ("version", Long::Flag),
    ("zero-terminated", Long::Flag),
];

/// The ways sed may read these arguments: as GNU sed does; as it does
/// with `POSIXLY_CORRECT` set (options end at the first operand), which
/// the environment may hold; and, when a bare `-i` is followed by a word,
/// as BSD (macOS) sed does, which takes that word as the backup suffix
/// (`sed -i '' 's/a/b/' f`).
pub(super) fn calls(args: &[Word]) -> Vec<Call> {
    let mut calls = vec![
        call(args, /*bsd*/ false, /*posix*/ false),
        call(args, /*bsd*/ false, /*posix*/ true),
    ];
    let bare_i = args.iter().any(|arg| {
        arg.text.starts_with('-') && !arg.text.starts_with("--") && arg.text.ends_with('i')
    });
    if bare_i {
        calls.push(call(args, /*bsd*/ true, /*posix*/ false));
    }
    calls
}

/// Read sed's arguments as GNU sed does: options anywhere before `--`
/// (`posix`: before the first operand), short options grouped, `-i` with
/// an attached suffix only (`bsd`: or the next word).
fn call(args: &[Word], bsd: bool, posix: bool) -> Call {
    let mut call = Call {
        bsd,
        ..Call::default()
    };
    let mut explicit = false;
    let mut operands = Vec::new();
    let mut options_done = false;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        work::step();
        index += 1;
        // An unquoted expansion may become any words, options included.
        call.unknown |= arg.splits;
        let text = arg.text.as_str();
        if options_done || !text.starts_with('-') || text == "-" {
            operands.push(arg.clone());
            options_done |= posix;
            continue;
        }
        if text == "--" {
            options_done = true;
            continue;
        }
        let value_of = |attached: &str, index: &mut usize| {
            if attached.is_empty() {
                *index += 1;
                args.get(*index - 1).cloned()
            } else {
                Some(Word {
                    text: attached.to_string(),
                    ..arg.clone()
                })
            }
        };
        if let Some(long) = text.strip_prefix("--") {
            let (name, attached) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            let exact = LONG.iter().find(|(option, _)| *option == name);
            let mut matching = LONG.iter().filter(|(option, _)| option.starts_with(name));
            let kind = match (exact, matching.next(), matching.next()) {
                (Some((_, kind)), _, _) | (None, Some((_, kind)), None) => *kind,
                _ => {
                    call.unknown = true;
                    continue;
                }
            };
            let mut value = || match attached {
                Some(value) => Some(Word {
                    text: value.to_string(),
                    ..arg.clone()
                }),
                None => {
                    index += 1;
                    args.get(index - 1).cloned()
                }
            };
            match kind {
                Long::Expression => {
                    explicit = true;
                    call.programs.extend(value());
                }
                Long::File => {
                    explicit = true;
                    call.files.extend(value());
                }
                Long::InPlace => call.in_place = true,
                Long::Value => {
                    let _ = value();
                }
                Long::Flag => {}
            }
            continue;
        }
        let cluster = &text[1..];
        for (at, letter) in cluster.char_indices() {
            work::step();
            let rest = &cluster[at + letter.len_utf8()..];
            match letter {
                'n' | 'r' | 'E' | 's' | 'u' | 'z' | 'b' => continue,
                'i' => {
                    call.in_place = true;
                    if bsd && rest.is_empty() {
                        index += 1;
                    }
                }
                'e' => {
                    explicit = true;
                    call.programs.extend(value_of(rest, &mut index));
                }
                'f' => {
                    explicit = true;
                    call.files.extend(value_of(rest, &mut index));
                }
                'l' => {
                    let _ = value_of(rest, &mut index);
                }
                _ => call.unknown = true,
            }
            break;
        }
    }
    call.options_ended = options_done;
    if !explicit && !operands.is_empty() {
        call.programs.push(operands.remove(0));
    }
    call.operands = operands;
    call
}

/// What a sed program does, as far as running and writing go.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Program {
    /// It may run a shell command, or it does not parse and holds an `e`.
    pub(super) runs: bool,
    /// Files it writes (`w`, `W`, `s///w`).
    pub(super) writes: Vec<String>,
    /// It only passes, drops or adds whole lines of literal text (`p`, `d`,
    /// `a`, `i`, `c`, ...): what it outputs is its input's own lines and
    /// `texts`.
    pub(super) keeps_lines: bool,
    /// Text it adds (`a`, `i`, `c`), as sed outputs it.
    pub(super) texts: Vec<String>,
}

/// Judge a sed program. A bracket expression may or may not hide the
/// delimiter (`s/[/]/x/`) depending on the sed; both readings are judged.
pub(super) fn program(text: &str) -> Program {
    let readings = [
        read(text, /*brackets*/ true),
        read(text, /*brackets*/ false),
    ];
    let mut program = Program {
        keeps_lines: true,
        ..Program::default()
    };
    for reading in readings {
        work::step();
        match reading {
            Some(reading) => {
                program.runs |= reading.runs;
                program.writes.extend(reading.writes);
                program.keeps_lines &= reading.keeps_lines;
                program.texts.extend(reading.texts);
            }
            None => {
                // Only `e` runs anything: a program without one cannot.
                program.runs |= text.contains('e');
                program.keeps_lines = false;
            }
        }
    }
    program
}

/// One reading of a program; `None` when it does not parse.
fn read(text: &str, brackets: bool) -> Option<Program> {
    let chars: Vec<char> = text.chars().collect();
    let mut parser = Parser {
        chars: &chars,
        index: 0,
        brackets,
    };
    let mut program = Program {
        keeps_lines: true,
        ..Program::default()
    };
    loop {
        work::step();
        parser.skip(" \t\n;");
        let Some(ch) = parser.peek() else {
            return Some(program);
        };
        if ch == '#' {
            parser.rest_of_line();
            continue;
        }
        if ch == '}' {
            parser.index += 1;
            parser.end_of_command()?;
            continue;
        }
        if parser.address()? {
            parser.skip(" \t");
            if parser.peek() == Some(',') {
                parser.index += 1;
                parser.skip(" \t");
                if !parser.address()? && !parser.step() {
                    return None;
                }
            }
        }
        parser.skip(" \t");
        while parser.peek() == Some('!') {
            work::step();
            parser.index += 1;
            parser.skip(" \t");
        }
        let command = parser.next()?;
        match command {
            '{' => continue,
            'a' | 'i' | 'c' => {
                program.texts.push(parser.text());
                continue;
            }
            'e' => {
                program.runs = true;
                parser.rest_of_line();
                continue;
            }
            'r' | 'R' | 'w' | 'W' => {
                parser.skip(" \t");
                let file = parser.rest_of_line();
                if matches!(command, 'w' | 'W') {
                    program.writes.push(file);
                }
                // Reading another file adds its lines.
                program.keeps_lines &= matches!(command, 'w' | 'W');
                continue;
            }
            ':' | 'b' | 't' | 'T' | 'v' => {
                parser.skip(" \t");
                let label = parser.label();
                if command == ':' && label.is_empty() {
                    return None;
                }
            }
            'l' | 'q' | 'Q' | 'L' => {
                parser.skip(" \t");
                parser.digits();
                program.keeps_lines &= command != 'l';
            }
            '=' | 'd' | 'p' | 'n' | 'F' => {}
            'D' | 'g' | 'G' | 'h' | 'H' | 'N' | 'P' | 'x' | 'z' => program.keeps_lines = false,
            's' => {
                program.keeps_lines = false;
                let delimiter = parser.next()?;
                if matches!(delimiter, '\n' | '\\') {
                    return None;
                }
                parser.part(delimiter, brackets)?;
                parser.part(delimiter, /*brackets*/ false)?;
                let mut wrote = false;
                while let Some(flag) = parser.peek() {
                    work::step();
                    match flag {
                        'g' | 'p' | 'i' | 'I' | 'm' | 'M' | '0'..='9' => parser.index += 1,
                        'e' => {
                            program.runs = true;
                            parser.index += 1;
                        }
                        'w' => {
                            parser.index += 1;
                            parser.skip(" \t");
                            program.writes.push(parser.rest_of_line());
                            wrote = true;
                            break;
                        }
                        _ => break,
                    }
                }
                if wrote {
                    continue;
                }
            }
            'y' => {
                program.keeps_lines = false;
                let delimiter = parser.next()?;
                parser.part(delimiter, /*brackets*/ false)?;
                parser.part(delimiter, /*brackets*/ false)?;
            }
            _ => return None,
        }
        parser.end_of_command()?;
    }
}

struct Parser<'a> {
    chars: &'a [char],
    index: usize,
    brackets: bool,
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.index).copied()
    }

    fn next(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.index += 1;
        Some(ch)
    }

    fn skip(&mut self, set: &str) {
        while self.peek().is_some_and(|ch| set.contains(ch)) {
            work::step();
            self.index += 1;
        }
    }

    fn digits(&mut self) -> bool {
        let start = self.index;
        while self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
            work::step();
            self.index += 1;
        }
        self.index > start
    }

    /// The text to the end of the line.
    fn rest_of_line(&mut self) -> String {
        let mut text = String::new();
        while let Some(ch) = self.next() {
            work::step();
            if ch == '\n' {
                break;
            }
            text.push(ch);
        }
        text
    }

    /// A label: to `;`, a newline, `}` or blank space (the earliest place
    /// any sed ends one, so nothing after it is missed).
    fn label(&mut self) -> String {
        let mut label = String::new();
        while let Some(ch) = self.peek() {
            work::step();
            if ch.is_whitespace() || ";}".contains(ch) {
                break;
            }
            label.push(ch);
            self.index += 1;
        }
        label
    }

    /// Text of `a`, `i` or `c` (`a text` or `a\` and a newline): to the
    /// first newline not escaped, with escapes removed.
    fn text(&mut self) -> String {
        self.skip(" \t");
        if self.peek() == Some('\\') {
            self.index += 1;
            if self.peek() == Some('\n') {
                self.index += 1;
            }
        }
        let mut text = String::new();
        let mut escaped = false;
        while let Some(ch) = self.next() {
            work::step();
            match ch {
                _ if escaped => {
                    escaped = false;
                    text.push(ch);
                }
                '\\' => escaped = true,
                '\n' => break,
                _ => text.push(ch),
            }
        }
        text
    }

    /// After a command: blank space, then the end, `;`, a newline, `}` or
    /// a comment.
    fn end_of_command(&mut self) -> Option<()> {
        self.skip(" \t");
        match self.peek() {
            None | Some(';' | '\n' | '}' | '#') => Some(()),
            Some(_) => None,
        }
    }

    /// An address: a line number (`first~step`), `$`, `/regex/` or
    /// `\cregexc` with `I`/`M` flags. `Some(false)` when there is none.
    fn address(&mut self) -> Option<bool> {
        match self.peek() {
            Some(ch) if ch.is_ascii_digit() => {
                self.digits();
                if self.peek() == Some('~') {
                    self.index += 1;
                    self.digits();
                }
                Some(true)
            }
            Some('$') => {
                self.index += 1;
                Some(true)
            }
            Some('/') => {
                self.index += 1;
                self.part('/', self.brackets)?;
                self.skip("IM");
                Some(true)
            }
            Some('\\') => {
                self.index += 1;
                let delimiter = self.next()?;
                self.part(delimiter, self.brackets)?;
                self.skip("IM");
                Some(true)
            }
            _ => Some(false),
        }
    }

    /// A second address's `+N` or `~N`.
    fn step(&mut self) -> bool {
        if matches!(self.peek(), Some('+' | '~')) {
            self.index += 1;
            return self.digits();
        }
        false
    }

    /// A regex or replacement up to an unescaped `delimiter`; with
    /// `brackets`, a bracket expression may hold the delimiter.
    fn part(&mut self, delimiter: char, brackets: bool) -> Option<()> {
        loop {
            work::step();
            let ch = self.next()?;
            if ch == '\\' {
                self.next()?;
            } else if ch == delimiter {
                return Some(());
            } else if ch == '[' && brackets {
                self.bracket()?;
            }
        }
    }

    /// The rest of a bracket expression after `[`.
    fn bracket(&mut self) -> Option<()> {
        if self.peek() == Some('^') {
            self.index += 1;
        }
        if self.peek() == Some(']') {
            self.index += 1;
        }
        loop {
            work::step();
            match self.next()? {
                ']' => return Some(()),
                '[' if matches!(self.peek(), Some(':' | '.' | '=')) => {
                    let close = self.next()?;
                    loop {
                        work::step();
                        if self.next()? == close && self.peek() == Some(']') {
                            self.index += 1;
                            break;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn word(text: &str) -> Word {
        Word {
            text: text.to_string(),
            ..Word::default()
        }
    }

    fn programs(args: &[&str]) -> (Vec<String>, Vec<String>, bool, bool) {
        let words: Vec<Word> = args.iter().map(|arg| word(arg)).collect();
        let call = call(&words, /*bsd*/ false, /*posix*/ false);
        (
            call.programs.into_iter().map(|word| word.text).collect(),
            call.files.into_iter().map(|word| word.text).collect(),
            call.in_place,
            call.unknown,
        )
    }

    #[test]
    fn options_are_read_as_gnu_sed_reads_them() {
        let none: Vec<String> = Vec::new();
        assert_eq!(
            programs(&["-n", "1,5p", "f"]),
            (vec!["1,5p".into()], none.clone(), false, false)
        );
        assert_eq!(
            programs(&["-i", "", "1e x", "f"]),
            (vec![String::new()], none.clone(), true, false)
        );
        assert_eq!(
            programs(&["-nes/x/y/e", "--expr=1e x", "--e", "e", "-ie", "p"]),
            (
                vec!["s/x/y/e".into(), "1e x".into(), "e".into()],
                none.clone(),
                true,
                false
            )
        );
        assert_eq!(
            programs(&["--fi=s.sed", "-nf", "t.sed", "-fu.sed", "f", "-e"]),
            (
                none,
                vec!["s.sed".into(), "t.sed".into(), "u.sed".into()],
                false,
                false
            )
        );
        assert_eq!(programs(&["--f=x", "p"]).3, true);
        assert_eq!(programs(&["-x", "p"]).3, true);
        assert_eq!(
            programs(&["-l", "5", "p", "--", "-e"]).0,
            vec!["p".to_string()]
        );
        let posix = calls(&[word("-n"), word("1e x"), word("-e"), word("p"), word("f")]);
        assert_eq!(
            posix
                .iter()
                .map(|call| call.programs[0].text.as_str())
                .collect::<Vec<_>>(),
            vec!["p", "1e x"]
        );
        let macos = calls(&[word("-i"), word(""), word("1a x"), word("f")]);
        assert_eq!(
            macos
                .iter()
                .map(|call| (call.programs[0].text.as_str(), call.operands.len()))
                .collect::<Vec<_>>(),
            vec![("", 2), ("", 2), ("1a x", 1)]
        );
    }

    #[test]
    fn programs_that_run_commands() {
        for (text, runs) in [
            ("1,120p", false),
            ("s/a/b/g", false),
            ("$a done", false),
            ("/x/{p;d}", false),
            ("y/abc/xyz/", false),
            ("s/e/E/g;/e/d", false),
            ("b end;:end", false),
            ("1e ./p.sh", true),
            ("e", true),
            ("s|^|./p.sh|e", true),
            ("s/x/y/ge", true),
            ("s/x/y/3e", true),
            ("/^/I s|^|./p.sh|e", true),
            ("\\%x%e ./p.sh", true),
            ("/x/I,+2!e", true),
            ("1~2{s/a/b/;e cmd\n}", true),
            (":x;e cmd", true),
            ("b x;e cmd", true),
            ("s/[/]/x/e", true),
            ("s/x/y/w e", false),
            ("$!N;e", true),
            // Does not parse: fails closed only with an `e`.
            ("p p", false),
            ("me", true),
            ("s/a/b/ze", true),
        ] {
            assert_eq!(program(text).runs, runs, "{text:?}");
        }
        assert_eq!(
            program("/x/w out.sh\ns/a/b/w r.sh"),
            Program {
                runs: false,
                writes: vec![
                    "out.sh".into(),
                    "r.sh".into(),
                    "out.sh".into(),
                    "r.sh".into()
                ],
                keeps_lines: false,
                texts: Vec::new(),
            }
        );
        let added = program("/x/d;1a text;$i\\\n  more\\\nlines");
        assert!(added.keeps_lines);
        // Both readings (with and without bracket expressions) agree.
        assert_eq!(added.texts, vec!["text;$i\n  more\nlines".to_string(); 2]);
        assert!(!program("s/a/b/").keeps_lines);
        assert!(!program("1r other").keeps_lines);
    }
}
