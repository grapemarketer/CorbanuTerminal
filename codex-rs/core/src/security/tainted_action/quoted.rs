//! #885: a quote-aware reading of shell text, beside the word lexer in
//! `shell.rs` (which removes quotes on purpose). It keeps each quoted word
//! whole, so the script a nested shell runs (`bash -c "sh -c './x'"`) and
//! the program a sed command gets are judged as the shell passes them on.
//! Substitutions are read as commands of their own; here-documents and
//! here-strings are kept as the text fed to their command.

use super::certain_cd;
use super::certain_cd::Joint;
use super::shell::UNSEEN_OUTPUT;
use super::shell::ansi_c;
use super::work;
use std::collections::HashMap;

#[derive(Clone, Debug, Default)]
pub(super) struct Word {
    /// The text the command receives. A substitution's output is
    /// `UNSEEN_OUTPUT`; a parameter expansion is kept as written.
    pub(super) text: String,
    /// Some of it was quoted or escaped, so the word lexer may split it.
    pub(super) quoted: bool,
    /// It holds an expansion whose value the classifier does not know.
    pub(super) dynamic: bool,
    /// An expansion outside double quotes: it may split into several words.
    pub(super) splits: bool,
    /// The word lexer keeps it as one word (a `$'...'` string, a
    /// here-string), so commands in it do not count against the lexer's
    /// even when it is a shell's script.
    pub(super) whole: bool,
    /// A here-document body, which the word lexer reads as command lines:
    /// commands in it count against the lexer's.
    pub(super) lines: bool,
}

#[derive(Debug, Default)]
pub(super) struct Command {
    pub(super) words: Vec<Word>,
    /// Words its output is redirected to (`> f`, `>> f`, `&> f`).
    pub(super) writes_to: Vec<Word>,
    /// Its output feeds a pipe: the next command of the pipeline, when it
    /// is in the same list of commands.
    pub(super) piped: bool,
    pub(super) pipe_to: Option<usize>,
    /// Its output goes to a descriptor other than 1 or 2 (`>&3`), which
    /// may be any file.
    pub(super) writes_descriptor: bool,
    /// Here-strings and here-document bodies fed to its input.
    pub(super) input: Vec<Word>,
    /// Text the shell does not pass on as its words: comments, the files
    /// it reads from, here-document delimiters. It is judged like quoted
    /// text, so a misreading of where a quote or substitution ends cannot
    /// drop text from judgement.
    pub(super) other_text: Vec<Word>,
    /// It runs inside a substitution, in a shell of its own.
    pub(super) substituted: bool,
    /// D194: what joins it to the command before it.
    pub(super) after: Joint,
    /// #1515: this reading ended it at a `{`, `}` or `(` the shell may
    /// read as text: an argument (`git -C { config core.pager sh`; bash
    /// reads `}` so too), a glob (zsh `x(N)`, extglob `@(a)`) or zsh's
    /// `=(…)`. The words after it may be its own.
    pub(super) cut: bool,
}

impl Command {
    fn is_empty(&self) -> bool {
        self.words.is_empty()
            && self.writes_to.is_empty()
            && self.input.is_empty()
            && self.other_text.is_empty()
            && !self.piped
    }
}

/// Substitutions nested deeper than this are not read (fail closed).
const MAX_NESTING: usize = 32;

/// The simple commands of some shell text.
pub(super) struct Reading {
    /// Substitutions included. A command inside a group (`( ... )`,
    /// `{ ...; }`) is not linked to the group's pipe or redirections; the
    /// group's own (empty) command holds them.
    pub(super) commands: Vec<Command>,
    /// Substitutions nested too deeply to read: the rest was not read.
    pub(super) too_deep: bool,
    /// A quote, substitution or `${...}` was not closed: a shell would
    /// refuse the line, and this reading may not match how it splits.
    pub(super) unterminated: bool,
    /// A `case` this reading does not follow (not as a keyword, or not
    /// `case WORD in`) is in a substitution: where it ends is unknown.
    pub(super) unsure: bool,
}

/// Read `text`, substitutions included.
pub(super) fn commands(text: &str) -> Reading {
    read(
        text, /*nesting*/ 0, /*body*/ false, /*arithmetic*/ false,
    )
}

/// Read `text` (`body`: the body of an unquoted here-document, where only
/// substitutions run) inside `nesting` substitutions (`arithmetic`: in text
/// that may hold arithmetic or a subscript).
fn read(text: &str, nesting: usize, body: bool, arithmetic: bool) -> Reading {
    let chars: Vec<char> = text.chars().collect();
    let mut reader = Reader {
        chars: &chars,
        out: Vec::new(),
        nesting,
        too_deep: false,
        unterminated: false,
        unsure: false,
        arithmetic: arithmetic || text.contains("((") || text.contains('['),
        in_arithmetic: None,
        closes: None,
    };
    if body {
        reader.substitutions_only();
    } else {
        let mut index = 0;
        while index < chars.len() {
            work::step();
            index = reader.list(index, /*nested*/ false);
        }
    }
    // Drop empty commands, keeping pipe links.
    let mut kept = Vec::with_capacity(reader.out.len());
    let mut next = 0;
    for command in &reader.out {
        work::step();
        kept.push((!command.is_empty()).then(|| {
            next += 1;
            next - 1
        }));
    }
    let mut out: Vec<Command> = Vec::with_capacity(next);
    for mut command in reader.out {
        work::step();
        if command.is_empty() {
            continue;
        }
        command.pipe_to = command
            .pipe_to
            .and_then(|to| kept.get(to).copied().flatten());
        out.push(command);
    }
    Reading {
        commands: out,
        too_deep: reader.too_deep,
        unterminated: reader.unterminated,
        unsure: reader.unsure,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Redirect {
    None,
    Stdin,
    Stdout,
    /// Output to a descriptor other than 1 or 2 (`>&3`).
    Descriptor,
    HereString,
    /// `<<` or `<<-` (strip leading tabs).
    HereDocument(bool),
}

struct Reader<'a> {
    chars: &'a [char],
    out: Vec<Command>,
    /// Substitutions open around the text being read.
    nesting: usize,
    /// They nested past `MAX_NESTING`; reading stopped.
    too_deep: bool,
    /// Something was left open at the end.
    unterminated: bool,
    /// See `Reading::unsure`.
    unsure: bool,
    /// The text may hold arithmetic (`$((`, `((`, `$[`) or a subscript
    /// (`a[...]`), where single quotes do not stop a substitution.
    arithmetic: bool,
    /// #1515: the `((`/`$((` arithmetic being read, as the index of its
    /// last `)` and its substitution depth: a `(` before that index at that
    /// depth is arithmetic, not a cut. By index, so text this reading skips
    /// (a comment, a here-document) cannot leave it open.
    in_arithmetic: Option<(usize, usize)>,
    /// [`clean_closes`] of the text, once a `((` needs it.
    closes: Option<HashMap<usize, usize>>,
}

/// One list of commands being read.
struct List {
    command: usize,
    word: Word,
    has_word: bool,
    redirect: Redirect,
    /// Here-documents whose bodies start after the next newline.
    pending: Vec<HereDocument>,
    /// `case` commands open in this list at the current bracket depth:
    /// their patterns end with `)`.
    cases: usize,
    /// The next word is in a command's first position (after reserved
    /// words such as `do`; not after a word or redirection).
    keyword: bool,
    /// Where a `case` keyword is in the command's words: `in` must follow
    /// its subject.
    case_at: Option<usize>,
    /// The list is a substitution's, which a `)` closes.
    nested: bool,
    /// D194: the operators since the last word (`None`: a word).
    joint: Option<Joint>,
}

/// Reserved words after which `case` is still a command's first word.
const BEFORE_CASE: &[&str] = &[
    "!", "do", "then", "else", "elif", "if", "while", "until", "time",
];

struct HereDocument {
    delimiter: String,
    /// `<<-`: leading tabs are stripped.
    strip: bool,
    /// The delimiter was quoted, so the body is not expanded.
    quoted: bool,
    command: usize,
}

impl Reader<'_> {
    fn new_command(&mut self) -> usize {
        self.out.push(Command {
            substituted: self.nesting > 0,
            ..Command::default()
        });
        self.out.len() - 1
    }

    fn finish_word(&mut self, list: &mut List) {
        if !std::mem::take(&mut list.has_word) {
            return;
        }
        let word = std::mem::take(&mut list.word);
        let command = &mut self.out[list.command];
        let redirect = std::mem::replace(&mut list.redirect, Redirect::None);
        if redirect == Redirect::None && !word.quoted {
            let keyword = std::mem::replace(&mut list.keyword, false);
            match word.text.as_str() {
                "case" if keyword => {
                    list.cases += 1;
                    list.case_at = Some(command.words.len());
                }
                "esac" if keyword => list.cases = list.cases.saturating_sub(1),
                // `case`/`esac` this reading does not take as a keyword (a
                // shell may: `repeat 1 case`, `coproc case`) could end a
                // substitution elsewhere than the shell does.
                "case" | "esac" if list.nested => self.unsure = true,
                text => list.keyword = keyword && BEFORE_CASE.contains(&text),
            }
        } else if redirect == Redirect::None {
            list.keyword = false;
        }
        match redirect {
            Redirect::None => {
                let joint = list.joint.take();
                if command.words.is_empty() {
                    command.after = joint.unwrap_or_default();
                }
                command.words.push(word);
            }
            Redirect::Stdin | Redirect::Descriptor => command.other_text.push(word),
            Redirect::Stdout => command.writes_to.push(word),
            Redirect::HereString => command.input.push(Word {
                whole: true,
                ..word
            }),
            Redirect::HereDocument(strip) => {
                command.other_text.push(word.clone());
                list.pending.push(HereDocument {
                    quoted: word.quoted,
                    delimiter: word.text,
                    strip,
                    command: list.command,
                });
            }
        }
    }

    /// #1515: a `{` or `}` word where the shell reads a word (`git -C {
    /// …`), not a reserved word: after a command's words, other than
    /// `function NAME {`.
    fn note_argument_brace(&mut self, list: &List) {
        let words = &self.out[list.command].words;
        let function = words.len() == 2 && !words[0].quoted && words[0].text == "function";
        if !list.keyword && !words.is_empty() && !function {
            self.out[list.command].cut = true;
        }
    }

    /// #1515: whether a `(` opens a group, a function's `()`, an array
    /// value, a `case` pattern or a test's pattern, rather than being text
    /// a shell may read as a glob (`x(N)`, `@(a)`, `=(…)`) or as an
    /// argument.
    fn opens_group(&self, list: &List, next: Option<char>) -> bool {
        let words = &self.out[list.command].words;
        let first = words
            .first()
            .filter(|word| !word.quoted)
            .map(|word| word.text.as_str());
        let last = words
            .last()
            .filter(|word| !word.quoted)
            .map(|word| word.text.as_str());
        let named = words.len() + usize::from(list.has_word);
        if !list.has_word {
            return list.keyword
                || (words.len() == 1 && first == Some("for"))
                || (list.case_at.is_some() && last == Some("in"))
                || first == Some("[[")
                || (next == Some(')')
                    && (named == 1 || (named == 2 && first == Some("function"))));
        }
        let text = list.word.text.as_str();
        (next == Some(')')
            && !list.word.quoted
            && (named == 1 || (named == 2 && first == Some("function"))))
            || first == Some("[[")
            || (!list.word.quoted && array_name(text) && assigning(words))
    }

    /// #1515: whether `index` is inside the arithmetic being read, at its
    /// depth.
    fn arithmetic_at(&self, index: usize) -> bool {
        self.in_arithmetic
            .is_some_and(|(end, depth)| index < end && depth == self.nesting)
    }

    /// #1515: the `((` or `$((` whose second `(` is at `open` is
    /// arithmetic, read at `depth`, when that bracket closes right before
    /// the last `)` with no quote or backslash between.
    fn note_arithmetic(&mut self, open: usize, depth: usize) {
        let chars = self.chars;
        let closes = self.closes.get_or_insert_with(|| clean_closes(chars));
        if let Some(&close) = closes.get(&open)
            && chars.get(close + 1) == Some(&')')
        {
            self.in_arithmetic = Some((close + 1, depth));
        }
    }

    fn finish_command(&mut self, list: &mut List) {
        self.finish_word(list);
        list.keyword = true;
        // `case WORD in`; in a substitution, another form (zsh's
        // `case x { ... }`) is not read.
        if let Some(at) = list.case_at.take()
            && list.nested
            && !self.out[list.command]
                .words
                .get(at + 2)
                .is_some_and(|word| !word.quoted && word.text == "in")
        {
            self.unsure = true;
        }
        if !self.out[list.command].is_empty() {
            list.command = self.new_command();
        }
    }

    /// Read commands from `index` to the end, or (`nested`) to the `)` that
    /// closes a substitution; returns the index after it.
    fn list(&mut self, index: usize, nested: bool) -> usize {
        if !self.enter(nested) {
            return self.chars.len();
        }
        let end = self.read_list(index, nested);
        self.nesting -= usize::from(nested);
        end
    }

    /// Open one more substitution (when `nested`); `false` past the limit.
    fn enter(&mut self, nested: bool) -> bool {
        if self.too_deep || (nested && self.nesting >= MAX_NESTING) {
            self.too_deep = true;
            return false;
        }
        self.nesting += usize::from(nested);
        true
    }

    fn read_list(&mut self, mut index: usize, nested: bool) -> usize {
        let chars = self.chars;
        let mut list = List {
            command: self.new_command(),
            word: Word::default(),
            has_word: false,
            redirect: Redirect::None,
            pending: Vec::new(),
            cases: 0,
            keyword: true,
            case_at: None,
            nested,
            joint: Some(if nested || self.nesting > 0 {
                Joint::Other
            } else {
                Joint::Start
            }),
        };
        // The `case` counts of the brackets around the text being read.
        let mut opened: Vec<usize> = Vec::new();
        while index < chars.len() {
            work::step();
            let ch = chars[index];
            let next = chars.get(index + 1).copied();
            let previous = index.checked_sub(1).map(|at| chars[at]);
            match ch {
                // A `case` pattern's `)` closes neither a bracket nor the
                // substitution (`esac)` closes the case first).
                ')' => {
                    self.finish_word(&mut list);
                    if list.cases == 0 {
                        match opened.pop() {
                            Some(cases) => list.cases = cases,
                            None if nested => return index + 1,
                            None => {}
                        }
                    }
                    self.finish_command(&mut list);
                    list.joint = Some(Joint::Other);
                }
                '(' => {
                    // #1515: a `((` that `))` closes is arithmetic up to it.
                    let opens = self.opens_group(&list, next);
                    if opens && next == Some('(') && !self.arithmetic_at(index) {
                        self.note_arithmetic(index + 1, self.nesting);
                    }
                    if !opens && !self.arithmetic_at(index) {
                        self.out[list.command].cut = true;
                    }
                    self.finish_command(&mut list);
                    opened.push(std::mem::take(&mut list.cases));
                    list.joint = Some(Joint::Other);
                }
                ' ' | '\t' => self.finish_word(&mut list),
                '\n' => {
                    self.finish_command(&mut list);
                    list.joint = Some(Joint::then(list.joint, Joint::Seq));
                    index = self.here_documents(index + 1, &mut list.pending);
                    continue;
                }
                '&' if next == Some('>') => {
                    self.finish_word(&mut list);
                    list.redirect = Redirect::Stdout;
                    list.keyword = false;
                    index += if chars.get(index + 2) == Some(&'>') {
                        3
                    } else {
                        2
                    };
                    continue;
                }
                ';' | '&' => {
                    self.finish_command(&mut list);
                    if let Some(op) = certain_cd::operator(previous, ch, next) {
                        list.joint = Some(Joint::then(list.joint, op));
                    }
                }
                '|' => {
                    self.finish_word(&mut list);
                    let from = list.command;
                    let pipe = next != Some('|') && previous != Some('|');
                    self.finish_command(&mut list);
                    if let Some(op) = certain_cd::operator(previous, ch, next) {
                        list.joint = Some(Joint::then(list.joint, op));
                    }
                    if pipe {
                        // After a group (`( ... ) | tee f`) the piped
                        // command is the group's empty one.
                        if from == list.command {
                            list.command = self.new_command();
                        }
                        self.out[from].piped = true;
                        self.out[from].pipe_to = Some(list.command);
                    }
                }
                '{' if !list.has_word && next.is_none_or(char::is_whitespace) => {
                    self.note_argument_brace(&list);
                    self.finish_command(&mut list);
                    list.joint = Some(Joint::Other);
                }
                '}' if !list.has_word
                    && next.is_none_or(|next| next.is_whitespace() || ";|&)".contains(next)) =>
                {
                    self.note_argument_brace(&list);
                    self.finish_command(&mut list);
                    list.joint = Some(Joint::Other);
                }
                // A comment is judged too (the word lexer reads it as words).
                '#' if !list.has_word => {
                    let mut comment = Word {
                        quoted: true,
                        ..Word::default()
                    };
                    index += 1;
                    while index < chars.len() && chars[index] != '\n' {
                        work::step();
                        comment.text.push(chars[index]);
                        index += 1;
                    }
                    self.out[list.command].other_text.push(comment);
                    continue;
                }
                '\'' => {
                    list.has_word = true;
                    list.word.quoted = true;
                    index += 1;
                    let from = index;
                    while index < chars.len() && chars[index] != '\'' {
                        work::step();
                        list.word.text.push(chars[index]);
                        index += 1;
                    }
                    self.quoted_span(from, index);
                    self.unterminated |= index >= chars.len();
                }
                '"' => {
                    list.has_word = true;
                    list.word.quoted = true;
                    index = self.double_quoted(index + 1, &mut list.word);
                    continue;
                }
                '\\' => {
                    match next {
                        // A line continuation.
                        Some('\n') => {}
                        Some(escaped) => {
                            list.has_word = true;
                            list.word.quoted = true;
                            list.word.text.push(escaped);
                        }
                        None => {}
                    }
                    index += 2;
                    continue;
                }
                '$' if next == Some('\'') => {
                    list.has_word = true;
                    list.word.quoted = true;
                    list.word.whole = true;
                    let from = index + 2;
                    index = ansi_c(chars, from, &mut list.word.text);
                    self.quoted_span(from, index);
                    continue;
                }
                '$' | '`' => {
                    list.has_word = true;
                    index = self.expansion(index, &mut list.word, /*in_double*/ false);
                    continue;
                }
                '<' | '>' if next == Some('(') => {
                    // Process substitution: a file name the classifier
                    // cannot see, and commands of its own.
                    list.has_word = true;
                    list.word.text.push_str(UNSEEN_OUTPUT);
                    list.word.dynamic = true;
                    index = self.list(index + 2, /*nested*/ true);
                    continue;
                }
                '<' | '>' => {
                    // A descriptor number (`2>`) is not a word.
                    if list.has_word
                        && !list.word.quoted
                        && list.word.text.chars().all(|ch| ch.is_ascii_digit())
                    {
                        list.has_word = false;
                        list.word = Word::default();
                    }
                    self.finish_word(&mut list);
                    list.keyword = false;
                    let (redirect, used) = redirection(&chars[index..]);
                    if redirect == Redirect::Descriptor {
                        self.out[list.command].writes_descriptor = true;
                        list.redirect = Redirect::None;
                    } else {
                        list.redirect = redirect;
                    }
                    index += used;
                    continue;
                }
                ch => {
                    list.has_word = true;
                    list.word.text.push(ch);
                }
            }
            index += 1;
        }
        self.finish_word(&mut list);
        self.unterminated |= nested;
        index
    }

    /// Text after `"` up to the closing quote; returns the index after it.
    fn double_quoted(&mut self, mut index: usize, word: &mut Word) -> usize {
        let chars = self.chars;
        while index < chars.len() {
            work::step();
            match chars[index] {
                '"' => return index + 1,
                '\\' => match chars.get(index + 1) {
                    Some('\n') => index += 2,
                    Some(escaped @ ('$' | '`' | '"' | '\\')) => {
                        word.text.push(*escaped);
                        index += 2;
                    }
                    _ => {
                        word.text.push('\\');
                        index += 1;
                    }
                },
                '$' | '`' => index = self.expansion(index, word, /*in_double*/ true),
                ch => {
                    word.text.push(ch);
                    index += 1;
                }
            }
        }
        self.unterminated = true;
        index
    }

    /// A quoted span `from..to`: bash, dash and zsh run a substitution in
    /// it in arithmetic (`$(( '$(cmd)' ))`, `$[ $'$(cmd)' ]`, zsh `(( ))`)
    /// and subscripts (`a['$(cmd)']=1`). Where such text may be, a span
    /// that holds one is not read here, so unsure.
    fn quoted_span(&mut self, from: usize, to: usize) {
        let span = &self.chars[from.min(to)..to.min(self.chars.len())];
        if self.arithmetic
            && span.iter().enumerate().any(|(at, ch)| {
                *ch == '`' || (*ch == '$' && matches!(span.get(at + 1), Some('(' | '{')))
            })
        {
            self.unsure = true;
        }
    }

    /// `$name`, `${...}`, `$(...)`, `$((...))` or a backtick substitution
    /// at `index`; returns the index after it.
    fn expansion(&mut self, index: usize, word: &mut Word, in_double: bool) -> usize {
        let chars = self.chars;
        let next = chars.get(index + 1).copied();
        let dynamic = |word: &mut Word| {
            word.dynamic = true;
            word.splits |= !in_double;
        };
        if chars[index] == '`' {
            let mut inner = String::new();
            let mut at = index + 1;
            while at < chars.len() && chars[at] != '`' {
                work::step();
                if chars[at] == '\\'
                    && let Some(escaped @ ('`' | '\\' | '$')) = chars.get(at + 1)
                {
                    inner.push(*escaped);
                    at += 2;
                    continue;
                }
                inner.push(chars[at]);
                at += 1;
            }
            self.unterminated |= at >= chars.len();
            self.nested_text(&inner, /*body*/ false);
            word.text.push_str(UNSEEN_OUTPUT);
            dynamic(word);
            return at + 1;
        }
        match next {
            Some('(') => {
                word.text.push_str(UNSEEN_OUTPUT);
                dynamic(word);
                if chars.get(index + 2) == Some(&'(') && !self.arithmetic_at(index) {
                    self.note_arithmetic(index + 2, self.nesting + 1);
                }
                self.list(index + 2, /*nested*/ true)
            }
            Some('{') => {
                // `${x:-$(...)}`: substitutions inside run too.
                if !self.enter(/*nested*/ true) {
                    return chars.len();
                }
                // Quotes inside it are honoured, as bash does.
                let mut scratch = Word::default();
                let mut at = index + 2;
                let mut open = 1usize;
                while at < chars.len() {
                    work::step();
                    match chars[at] {
                        '$' if !in_double && chars.get(at + 1) == Some(&'\'') => {
                            at = ansi_c(chars, at + 2, &mut scratch.text);
                            continue;
                        }
                        '$' | '`' => {
                            at = self.expansion(at, &mut scratch, in_double);
                            continue;
                        }
                        '"' => {
                            at = self.double_quoted(at + 1, &mut scratch);
                            continue;
                        }
                        // Even inside double quotes (`"${x:-'}'}"`), where
                        // a substitution in it still runs
                        // (`"${x:-'$(cmd)'}"`), as in arithmetic
                        // (`$(( ${x:-'$(cmd)'} ))`, zsh `(( ))` and
                        // `for ((...))`) and zsh subscripts: not read here,
                        // so unsure wherever the `${...}` is.
                        '\'' => {
                            at += 1;
                            while at < chars.len() && chars[at] != '\'' {
                                work::step();
                                if chars[at] == '`'
                                    || (chars[at] == '$'
                                        && matches!(chars.get(at + 1), Some('(' | '{')))
                                {
                                    self.unsure = true;
                                }
                                at += 1;
                            }
                        }
                        '\\' => at += 1,
                        '{' => open += 1,
                        '}' => {
                            open -= 1;
                            if open == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    at += 1;
                }
                self.unterminated |= at >= chars.len();
                self.nesting -= 1;
                let end = (at + 1).min(chars.len());
                word.text.extend(&chars[index.min(end)..end]);
                dynamic(word);
                end
            }
            Some(ch) if ch.is_ascii_alphabetic() || ch == '_' => {
                let mut at = index + 1;
                while chars
                    .get(at)
                    .is_some_and(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                {
                    work::step();
                    at += 1;
                }
                word.text.extend(&chars[index..at]);
                dynamic(word);
                at
            }
            Some(ch) if ch.is_ascii_digit() || "@*#?$!-".contains(ch) => {
                word.text.extend(&chars[index..index + 2]);
                dynamic(word);
                index + 2
            }
            // `$"..."` is a double-quoted string.
            Some('"') if !in_double => index + 1,
            _ => {
                word.text.push('$');
                index + 1
            }
        }
    }

    /// Commands of text inside a substitution (a backtick's text, or an
    /// unquoted here-document `body`, whose substitutions run).
    fn nested_text(&mut self, text: &str, body: bool) {
        if !self.enter(/*nested*/ true) {
            return;
        }
        let reading = read(text, self.nesting, body, self.arithmetic);
        self.nesting -= 1;
        self.too_deep |= reading.too_deep;
        self.unterminated |= reading.unterminated;
        self.unsure |= reading.unsure;
        let offset = self.out.len();
        self.out
            .extend(reading.commands.into_iter().map(|command| Command {
                substituted: true,
                pipe_to: command.pipe_to.map(|to| to + offset),
                ..command
            }));
    }

    /// The substitutions in a here-document body.
    fn substitutions_only(&mut self) {
        let chars = self.chars;
        let mut scratch = Word::default();
        let mut index = 0;
        while index < chars.len() {
            work::step();
            match chars[index] {
                '\\' => index += 2,
                '$' | '`' => index = self.expansion(index, &mut scratch, /*in_double*/ true),
                _ => index += 1,
            }
        }
    }

    /// Bodies of pending here-documents, from `index` (a line start);
    /// returns the index after the last delimiter line.
    fn here_documents(&mut self, mut index: usize, pending: &mut Vec<HereDocument>) -> usize {
        let chars = self.chars;
        for HereDocument {
            delimiter,
            strip,
            quoted,
            command,
        } in std::mem::take(pending)
        {
            work::step();
            let mut body = String::new();
            while index < chars.len() {
                work::step();
                let end = chars[index..]
                    .iter()
                    .position(|ch| *ch == '\n')
                    .map_or(chars.len(), |at| index + at);
                let line: String = chars[index..end].iter().collect();
                index = (end + 1).min(chars.len());
                let compared = if strip {
                    line.trim_start_matches('\t')
                } else {
                    line.as_str()
                };
                if compared == delimiter {
                    break;
                }
                body.push_str(&line);
                body.push('\n');
            }
            if !quoted {
                self.nested_text(&body, /*body*/ true);
            }
            self.out[command].input.push(Word {
                text: body,
                quoted: true,
                lines: true,
                ..Word::default()
            });
        }
        index
    }
}

/// #1515: each `(` of `chars` and the `)` that closes it, when nothing
/// between them can make a shell count brackets other than this one pass
/// does (one pass). Every review found another such corner of zsh's
/// arithmetic (`${x#((}`, a comment, a here-document, `##(`, unpaired
/// `[`/`]`, brackets in a nested `$(…)`), so the whole class is out: a
/// quote, backslash, backtick, newline, `#`, `[`, `]`, `{`, `}` (so `${`)
/// or `$(` between them, or a `${…}` around them, disqualifies the pair,
/// and the text is read as commands.
fn clean_closes(chars: &[char]) -> HashMap<usize, usize> {
    // Each open `(`: where, and whether a `${…}` holds it.
    let mut open: Vec<(usize, bool)> = Vec::new();
    let mut last_quote = None;
    let mut closes = HashMap::new();
    // The `${…}` (and braces inside) open.
    let mut braces = 0usize;
    for (at, ch) in chars.iter().enumerate() {
        work::step();
        let before = at.checked_sub(1).map(|before| chars[before]);
        match ch {
            '{' if braces > 0 || before == Some('$') => {
                braces += 1;
                last_quote = Some(at);
            }
            '}' if braces > 0 => {
                braces -= 1;
                last_quote = Some(at);
            }
            '\'' | '"' | '`' | '\\' | '\n' | '#' | '[' | ']' | '{' | '}' => {
                last_quote = Some(at);
            }
            '$' if chars.get(at + 1) == Some(&'(') => last_quote = Some(at),
            '(' => open.push((at, braces > 0)),
            ')' => {
                if let Some((from, braced)) = open.pop()
                    && !braced
                    && last_quote.is_none_or(|quote| quote < from)
                {
                    closes.insert(from, at);
                }
            }
            _ => {}
        }
    }
    closes
}

/// #1515: an array assignment's start (`a=`, `a+=`, `a[1]=`), which `(`
/// follows.
fn array_name(text: &str) -> bool {
    text.strip_suffix('=').is_some_and(variable_name)
}

/// #1515: whether a word after `words` is an assignment: they are all
/// assignments (`a=1 b=(x)`), or a builtin that assigns comes first
/// (`declare -a b=(x)`). Elsewhere `b=(x)` is a zsh glob.
fn assigning(words: &[Word]) -> bool {
    words.first().is_some_and(|first| {
        !first.quoted
            && matches!(
                first.text.as_str(),
                "declare" | "typeset" | "local" | "export" | "readonly" | "integer" | "float"
            )
    }) || words.iter().all(|word| {
        work::step();
        word.text
            .split_once('=')
            .is_some_and(|(name, _)| variable_name(name))
    })
}

/// A variable's name in an assignment (`a`, `a+`, `a[1]`).
fn variable_name(name: &str) -> bool {
    let name = name.strip_suffix('+').unwrap_or(name);
    let name = name
        .split_once('[')
        .filter(|(_, subscript)| subscript.ends_with(']'))
        .map_or(name, |(name, _)| name);
    name.chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && name.chars().all(|ch| {
            work::step();
            ch.is_ascii_alphanumeric() || ch == '_'
        })
}

/// The redirection at the start of `chars` and its length.
fn redirection(chars: &[char]) -> (Redirect, usize) {
    match chars {
        ['<', '<', '<', ..] => (Redirect::HereString, 3),
        ['<', '<', '-', ..] => (Redirect::HereDocument(true), 3),
        ['<', '<', ..] => (Redirect::HereDocument(false), 2),
        // `<>` opens the file for writing too.
        ['<', '>', ..] => (Redirect::Stdout, 2),
        ['<', '&', ..] => (Redirect::Stdin, 2),
        ['<', ..] => (Redirect::Stdin, 1),
        // `>&2`, `2>&1`, `>&-`: standard output or error, or closed.
        ['>', '&', '1' | '2', after, ..] if after.is_ascii_digit() => (Redirect::Descriptor, 3),
        ['>', '&', '1' | '2' | '-', ..] => (Redirect::None, 3),
        // `>&3`: whatever file descriptor 3 is.
        ['>', '&', after, ..] if after.is_ascii_digit() => (Redirect::Descriptor, 3),
        ['>', '>' | '|' | '&', ..] => (Redirect::Stdout, 2),
        _ => (Redirect::Stdout, 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn all(text: &str) -> Vec<Command> {
        let reading = commands(text);
        assert!(!reading.too_deep && !reading.unterminated, "{text:?}");
        reading.commands
    }

    fn texts(text: &str) -> Vec<Vec<String>> {
        all(text)
            .into_iter()
            .map(|command| command.words.into_iter().map(|word| word.text).collect())
            .collect()
    }

    #[test]
    fn quoted_words_stay_whole() {
        assert_eq!(
            texts("bash -c \"sh -c './missing.sh'\"; sed -n '1,5p' f | sort"),
            vec![
                vec!["bash", "-c", "sh -c './missing.sh'"],
                vec!["sed", "-n", "1,5p", "f"],
                vec!["sort"],
            ]
        );
        assert_eq!(
            texts("s\\ed $'\\x31e x' \"a\\\"b\" # c\necho `sed e` $(gsed e) x"),
            vec![
                vec!["sed", "1e x", "a\"b"],
                vec!["echo", UNSEEN_OUTPUT, UNSEEN_OUTPUT, "x"],
                vec!["sed", "e"],
                vec!["gsed", "e"],
            ]
        );
        let read = all("sed p f > r.sh 2>&1 | tee x; cat <<-'E' <<< w\n\tsed e\n\tE\nls");
        assert_eq!(read[0].writes_to[0].text, "r.sh");
        assert!(read[0].piped);
        assert_eq!(read[2].input[0].text, "w");
        assert_eq!(read[2].input[1].text, "\tsed e\n");
        assert_eq!(read[3].words[0].text, "ls");
        let substituted: Vec<bool> = all("cd a; echo $(cd b) `cd c` <(cd d)")
            .iter()
            .map(|command| command.substituted)
            .collect();
        assert_eq!(substituted, vec![false, false, true, true, true]);
        let dynamic = &all("sed \"$P\" '$p' ${Q}")[0].words;
        assert_eq!(
            dynamic
                .iter()
                .map(|word| (word.dynamic, word.splits))
                .collect::<Vec<_>>(),
            vec![(false, false), (true, false), (false, false), (true, true)]
        );
    }

    #[test]
    fn substitutions_redirections_and_nesting() {
        // Commands in `${...}` and in an unquoted here-document run; a
        // quoted one's body is only text.
        assert_eq!(
            texts("echo ${x:-$(sh -c a)}; cat <<E\n$(sh -c b)\nE\ncat <<'Q'\n$(sh -c c)\nQ"),
            vec![
                vec!["echo", "${x:-$(sh -c a)}"],
                vec!["sh", "-c", "a"],
                vec!["cat"],
                vec!["cat"],
                vec!["sh", "-c", "b"],
            ]
        );
        let read = all("sed p f | sort | tee r.sh; sed p f 1<>w.sh >&3");
        assert_eq!(read[0].pipe_to, Some(1));
        assert_eq!(read[1].pipe_to, Some(2));
        assert_eq!(read[2].pipe_to, None);
        assert_eq!(read[3].writes_to[0].text, "w.sh");
        assert!(read[3].writes_descriptor);
        assert!(!all("ls 2>&1 >&2")[0].writes_descriptor);
        // Too deep to read: refused, never a stack overflow.
        let deep = format!("{}x{}", "$(".repeat(100_000), ")".repeat(100_000));
        assert!(commands(&deep).too_deep);
        assert!(!commands(&"`".repeat(10_000)).too_deep);
        let backticks = format!("{}x", "\\`echo ".repeat(200));
        let _ = commands(&backticks);
        let braces = format!("{}x{}", "${a:-".repeat(100_000), "}".repeat(100_000));
        assert!(commands(&braces).too_deep);
        // Quotes in `${...}` and a `case` in a substitution keep the reading
        // in step with bash; what is left open is reported.
        assert_eq!(
            texts("echo ${x:-'}'}; bash -c x; echo \"$(case a in a) echo '\"' ;; esac)\"; sh -c y"),
            vec![
                vec!["echo", "${x:-'}'}"],
                vec!["bash", "-c", "x"],
                vec!["echo", UNSEEN_OUTPUT],
                vec!["case", "a", "in", "a"],
                vec!["echo", "\""],
                vec!["esac"],
                vec!["sh", "-c", "y"],
            ]
        );
        for open in ["echo 'a", "echo \"a", "echo $(a", "echo `a", "echo ${a"] {
            assert!(commands(open).unterminated, "{open:?}");
        }
        let group = all("(sed p f) | tee r.sh");
        assert_eq!(group[1].pipe_to, Some(2));
        assert!(group[1].words.is_empty());
    }

    #[test]
    fn case_patterns_comments_and_redirections() {
        // A `case` in brackets in a substitution, after a reserved word,
        // and a `case` that is not a first word (`>f case`).
        assert_eq!(
            texts(
                "echo \"$( (case x in a) :;; esac) )\" #c\n\
                 echo \"$(for i in a; do case $i in a) :;; esac; done)\"; \
                 echo \"$(>f case x in a)\"; sh -c y"
            ),
            vec![
                vec!["echo", UNSEEN_OUTPUT],
                vec!["case", "x", "in", "a"],
                vec![":"],
                vec!["esac"],
                vec!["echo", UNSEEN_OUTPUT],
                vec!["for", "i", "in", "a"],
                vec!["do", "case", "$i", "in", "a"],
                vec![":"],
                vec!["esac"],
                vec!["done"],
                vec!["echo", UNSEEN_OUTPUT],
                vec!["case", "x", "in", "a"],
                vec!["sh", "-c", "y"],
            ]
        );
        // Comments and the files a command reads from are kept to be
        // judged; `$'...'` and here-strings are kept whole, a here-document
        // body is read as lines.
        let read = all("cat < \"$x b\" <<E # sh -c y\nz\nE\necho $'a' <<< b");
        let other: Vec<&str> = read[0]
            .other_text
            .iter()
            .map(|word| word.text.as_str())
            .collect();
        assert_eq!(other, vec!["$x b", "E", " sh -c y"]);
        assert!(read[0].input[0].lines);
        assert!(read[1].words[1].whole && read[1].input[0].whole);
        // A `case` this reading cannot follow in a substitution.
        for unsure in [
            "echo \"$(repeat 1 case x in a) :;; esac)\"",
            "echo \"$(coproc case x in a) :;; esac)\"",
            "echo \"$(case x { a) :;; })\"",
            "echo \"$(echo case)\"",
            "echo \"${x:-'$(sed 1e\\ id f)'}\"",
            "echo \"${x:-'`sed 1e\\ id f`'}\"",
            "echo $(( ${x:-'$(sed 1e\\ id f)'} )) $[ ${x:-'${y}'} ]",
            "echo ${a[${y:-'$(sed 1e\\ id f)'}]}",
            // Single quotes do not stop a substitution in arithmetic or
            // a subscript.
            "echo $(( '$(sed 1e\\ id f)' )) \"$(( '`id`' ))\"",
            "echo $[ $'${y}' ]",
            "(( '$(sed 1e\\ id f)' )); for (( ; '$(id)' ; )); do :; done",
            "a['$(sed 1e\\ id f)']=1",
            "[[ 1 -eq 'a[$(id)]' ]]",
            // A line break before `in` is valid, but not followed here.
            "echo \"$(case x\nin a) :;; esac)\"",
        ] {
            assert!(commands(unsure).unsure, "{unsure:?}");
        }
        for sure in [
            "echo \"$(if case x in a) true;; esac; then :; fi)\"",
            "echo case; case x { a) :;; }",
            "echo \"${x:-'$HOME'}\" ${x:-'a'}",
            // No arithmetic or subscript in the line: the quotes quote.
            "grep -n '$(x)' f; echo $'`y`' \"$(echo '${z}')\"",
        ] {
            assert!(!commands(sure).unsure, "{sure:?}");
        }
        // Reserved words before a `case` are tracked as they come: a line
        // of them takes linear work (#1102).
        work::assert_linear("reserved words", |scale| {
            let words = "! ".repeat(50_000 * scale);
            let keywords = format!("echo \"$({words}case x in a) :;; esac)\"");
            assert!(!commands(&keywords).unsure);
        });
    }
}
