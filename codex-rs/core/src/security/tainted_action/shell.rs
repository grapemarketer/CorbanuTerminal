//! Shell-like word splitting for the post-taint classifier.
//!
//! This over-approximates the shell on purpose: quotes are removed rather
//! than honoured, so the text inside `sh -c '...'` or `python -c "..."` is
//! split into commands too. Command boundaries, pipes and substitutions are
//! kept so wrappers, pipelines and code fed to an interpreter can be followed.

use super::certain_cd;
use super::certain_cd::Joint;
use super::work;

/// Shells whose `-c` argument is its own command line.
pub(super) const SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "dash", "ksh", "fish", "csh", "tcsh", "ash", "mksh", "yash",
    // #1276: more spellings of POSIX-style shells.
    "rbash", "posh", "lksh", "oksh", "hush", "osh",
];
/// Words one brace expression may expand to before it is left as written.
const MAX_BRACE_EXPANSION: usize = 64;
/// Placeholder for the output of a substitution the classifier cannot see.
/// Holds a control character, so no variable or word can spell it.
pub(super) const UNSEEN_OUTPUT: &str = "\u{1}unseen-output\u{1}";
/// Placeholder for an arithmetic expansion's number (`$((i + 1))`). It holds
/// [`UNSEEN_OUTPUT`], so every check for unseen output treats it the same;
/// only the arithmetic check (#937) tells it apart from command output. Its
/// own control character brackets it, so no neighbouring text can complete
/// or absorb it.
pub(super) const ARITHMETIC_OUTPUT: &str = "\u{2}\u{1}unseen-output\u{1}\u{2}";
/// #1283: marks the words a brace expansion or `$IFS` made, which the
/// quote-aware reading (`quoted.rs`) keeps as one word as written. The
/// lexer drops it; text holding it only marks more words.
const HIDDEN: char = '\u{5}';

/// A character of the command text as a word holds it: the placeholders'
/// control characters are replaced, so text can never spell a placeholder.
pub(super) fn literal_char(ch: char) -> char {
    if matches!(ch, '\u{1}' | '\u{2}') {
        char::REPLACEMENT_CHARACTER
    } else {
        ch
    }
}

#[derive(Debug, Default)]
pub(super) struct SimpleCommand {
    /// Words with quotes and backslashes removed, original case.
    pub(super) words: Vec<String>,
    /// The command whose output is this command's stdin (`a | b`).
    pub(super) pipe_from: Option<usize>,
    /// It runs inside `$(...)`, `<(...)` or backticks.
    pub(super) substituted: bool,
    /// Commands inside this command's substitutions; their output is part of
    /// its words.
    pub(super) fed_by: Vec<usize>,
    /// It runs inside `>(...)`: its stdin is the outer command's output.
    pub(super) reads_outer_output: bool,
    /// Index of the word its stdin is redirected from (`< file`).
    pub(super) stdin_from: Option<usize>,
    /// Index of its here-string word (`<<< word`).
    pub(super) here_string: Option<usize>,
    /// Indexes of the words its output is redirected to (`> file`, `>> file`).
    pub(super) writes_to: Vec<usize>,
    /// Indexes of the words holding what a lookup printed (`$(readlink x)`
    /// stands for `x`, but prints whatever the link holds).
    pub(super) looked_up: Vec<usize>,
    /// Its text held a backslash, which the words no longer show
    /// (`printf 'a\x24'` decodes it).
    pub(super) escaped: bool,
    /// #1283: indexes of the words the quote-aware reading cannot spell:
    /// made by a brace expansion or `$IFS`, or holding a substitution's
    /// spliced output (`{git,config}`, `git${IFS}config`, `$(echo git)`).
    pub(super) hidden: Vec<usize>,
    /// #1064: it is the text of an arithmetic expansion (`$((i + 1))`),
    /// which runs no command.
    pub(super) arithmetic: bool,
    /// #989: indexes of the words holding a parameter expansion the lexer
    /// misreads (`${c:-eval}`, `${!c}`, `${c}al`, `"$c"al`, zsh `$c:t`): it
    /// drops braces and quotes, so the substituted word is not its value.
    pub(super) operated: Vec<usize>,
    /// D194: what joins it to the command before it.
    pub(super) after: Joint,
}

/// Lexed simple commands, in the order they finish (a substitution comes
/// before the command using it), and whether some text was left unexpanded.
pub(super) struct Lexed {
    pub(super) commands: Vec<SimpleCommand>,
    /// A brace expression had too many alternatives to expand.
    pub(super) incomplete: bool,
    /// Substitutions nested past `MAX_SUBSTITUTION_DEPTH`; the deeper ones
    /// were read without linking their output (#1020).
    pub(super) too_deep: bool,
}

/// How deeply substitutions (`$(`, `<(`, `>(`, backticks) are read inside
/// each other. A deeper one's commands are still read and judged by name,
/// and the command it interrupts keeps its words around an unseen value,
/// but its output is not spliced in or linked, and the line fails closed. The reader stays linear in the text times this
/// depth squared (#1020).
pub(super) const MAX_SUBSTITUTION_DEPTH: usize = 32;

pub(super) fn basename(word: &str) -> &str {
    word.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(word)
}

/// Simple commands of the argv and of any script inside it (`bash -lc "..."`).
pub(super) fn simple_commands(command: &[String]) -> Lexed {
    let mut lexer = Lexer {
        joint: Some(Joint::Start),
        ..Lexer::default()
    };
    let mut incomplete = false;
    let mut previous: Option<&str> = None;
    for arg in command {
        work::step();
        // The script after a shell's `-c`/`-lc` is its own command line.
        let in_shell = lexer
            .current
            .words
            .iter()
            .any(|word| SHELLS.contains(&basename(&word.to_lowercase())));
        if in_shell
            && previous.is_some_and(|flag| {
                flag.starts_with('-') && !flag.starts_with("--") && flag.contains('c')
            })
        {
            lexer.start(Start::Plain);
            lexer.joint = Some(Joint::Start);
        }
        previous = Some(arg.as_str());
        let (expanded, complete) = expand_braces(&replace_ifs(arg));
        incomplete |= !complete;
        lexer.arg(&expanded);
        lexer.flush();
    }
    let too_deep = lexer.too_deep;
    Lexed {
        commands: lexer.finish(),
        incomplete,
        too_deep,
    }
}

/// Whether text may use bash's prompt transformation (`${v@P}`,
/// `${a[0]@P}`, `${!r@P}`), which runs the command substitutions in the
/// value (#1012): `@P` not followed by a name character, in the text as
/// written (backslash-newlines joined) or with its quotes and backslashes
/// removed (`${v@'P'}`). The lexed words (braces dropped, `$'...'` decoded,
/// known variables substituted) are checked the same way by the caller.
/// Read without quotes, so nested shells' scripts count. Linear.
///
/// #1064: also a transformation whose operator is not one of the letters
/// that only quote or describe the value (`${v@$p}`, `${!r@${p^}}`): it may
/// be `P` when it runs.
///
/// #1064: `foreign`: the line may run code of another language, which can
/// build shell text at run time (`chr(64)`); there an uncertain subscript
/// counts on any line holding an `@`, as before.
pub(super) fn prompt_expansion(text: &str, foreign: bool) -> bool {
    if !text.contains('@') {
        return false;
    }
    let joined = text.replace("\\\n", "");
    let bare: String = joined
        .chars()
        .filter(|ch| !matches!(ch, '\'' | '"' | '\\'))
        .collect();
    // #1064: a subscript whose end is uncertain counts only where some
    // subscript's end is followed by an operator (in either reading), or
    // where other code may build one.
    let after_subscript = foreign
        || [joined.as_str(), bare.as_str()]
            .iter()
            .any(|text| operator_after_subscript(text));
    [joined.as_str(), bare.as_str()].iter().any(|text| {
        text.match_indices("@P").any(|(at, _)| {
            !text[at + 2..].starts_with(|ch: char| ch.is_ascii_alphanumeric() || ch == '_')
        }) || computed_transformation(text, after_subscript)
    })
}

/// #932 measuring: which reading of `prompt_expansion` holds for some
/// argument (`@P`, or a computed transformation), for the local measuring
/// harness.
#[cfg(test)]
pub(super) fn prompt_expansion_why(command: &[String], foreign: bool) -> String {
    let Some(text) = command.iter().find(|arg| prompt_expansion(arg, foreign)) else {
        return "-".to_string();
    };
    let joined = text.replace("\\\n", "");
    let bare: String = joined
        .chars()
        .filter(|ch| !matches!(ch, '\'' | '"' | '\\'))
        .collect();
    let at_p = [joined.as_str(), bare.as_str()].iter().any(|text| {
        text.match_indices("@P").any(|(at, _)| {
            !text[at + 2..].starts_with(|ch: char| ch.is_ascii_alphanumeric() || ch == '_')
        })
    });
    if at_p {
        "at_p".to_string()
    } else {
        format!("computed foreign={foreign}")
    }
}

/// #1064: whether text holds `]@` followed by anything but one of the
/// letters that only quote or describe a value (`${a[$(f)]@$p}`, but not
/// `${a[@]}`, `"$@"` or `${a[i]@Q}`). The `]` that ends a subscript is
/// written right before a transformation's `@` (backslash-newlines joined,
/// or with quotes and backslashes removed). Linear.
fn operator_after_subscript(text: &str) -> bool {
    text.match_indices("]@").any(|(at, _)| {
        work::step();
        !text[at + 2..].starts_with(|ch: char| "QEAKakUuL".contains(ch))
    })
}

/// Whether text holds `${name@op}` (`name` with an optional `!` or `#`
/// before it and a subscript after it) whose `op` is not one of `QEAKakUuL`
/// (`${!prefix@}` lists names). A subscript is read at most 64 bytes; one
/// not closed by then counts. Linear.
///
/// #1064: such a subscript counts only when `after_subscript` (see
/// [`operator_after_subscript`]), so `"${a[${#a[@]}-1]}"` does not.
fn computed_transformation(text: &str, after_subscript: bool) -> bool {
    text.match_indices("${").any(|(at, _)| {
        work::step();
        let mut rest = &text[at + "${".len()..];
        let bang = rest.starts_with('!');
        if rest.len() > 1 && rest.starts_with(['!', '#']) && !rest[1..].starts_with('}') {
            rest = &rest[1..];
        }
        let name = rest
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .unwrap_or(rest.len());
        let name = match name {
            0 if rest.starts_with(['@', '*', '#', '?', '$', '!', '-']) => 1,
            name => name,
        };
        rest = &rest[name..];
        // A subscript ends at its matching `]` (`${a[b[0]]@$p}`); one
        // holding a quote, backslash or substitution, which may hide a `]`
        // (`${a["]"]@$p}`, `${a[$(echo ])]@$p}`, `${a[${x:-]}]@$p}`), or not
        // closed within 64 bytes, counts.
        if rest.starts_with('[') {
            let mut depth = 0usize;
            let mut close = None;
            for (at, byte) in rest.as_bytes().iter().take(64).enumerate() {
                work::step();
                match byte {
                    b'[' => depth += 1,
                    b']' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(at);
                            break;
                        }
                    }
                    b'\'' | b'"' | b'\\' | b'`' | b'(' | b'{' => return after_subscript,
                    _ => {}
                }
            }
            let Some(close) = close else {
                return after_subscript;
            };
            rest = &rest[close + 1..];
        }
        rest.strip_prefix('@').is_some_and(|op| {
            !(op.starts_with(|ch: char| "QEAKakUuL".contains(ch)) || (bang && op.starts_with('}')))
        })
    })
}

/// Whether text may use zsh glob substitution (#1084), under which zsh
/// reads a value as a pattern, glob qualifiers included, so the value can
/// run code (`*(e:code:)`): `$~x`, `${~x}`, `$^~x`, `${(f)~x}` (a flag
/// group counts when any `~` follows it), the `GLOB_SUBST`
/// option in any spelling (zsh ignores case and underscores: `setopt glob_subst`,
/// `zsh -o GLOBSUBST`, `options[globsubst]=on`), a write to zsh's
/// `options` array, or `ARGV0` (a zsh started as `csh` or `sh` turns the
/// option on). Read as written (backslash-newlines joined) and with quotes
/// and backslashes removed, so nested shells' scripts count. Linear.
pub(super) fn glob_substitution(text: &str) -> bool {
    let joined = text.replace("\\\n", "");
    let bare: String = joined
        .chars()
        .filter(|ch| !matches!(ch, '\'' | '"' | '\\'))
        .collect();
    let option: String = bare
        .chars()
        .filter(|ch| !matches!(ch, '_' | '-'))
        .collect::<String>()
        .to_ascii_lowercase();
    option.contains("globsubst")
        || option.contains("argv0")
        // `ARGV${:-0}`, `${:-ARGV}0`, `ARGV{0,}`: a name the reader cannot
        // complete (not `argv[1]`, a subscript).
        || option.match_indices("argv").any(|(at, _)| {
            option[at + "argv".len()..].starts_with(['$', '{', '}', '*', '?', '(', '`'])
        })
        || [joined.as_str(), bare.as_str()]
            .iter()
            .any(|text| substitutes_globs(text) || writes_options(text) || assigns_named(text))
        || reparses(&joined)
}

/// `${(e)y}`: a flag group with `e` reparses a value, whose expansions may
/// spell `${~x}` with no `~` on the line (`y='${'${(#):-126}'x}'`). Each
/// group is read at most 64 bytes; one not closed by then counts. Linear.
fn reparses(text: &str) -> bool {
    text.match_indices("${(").any(|(at, _)| {
        work::step();
        let flags = &text.as_bytes()[at + "${(".len()..];
        let group = &flags[..flags.len().min(64)];
        group
            .iter()
            .position(|&byte| byte == b')')
            .is_none_or(|end| group[..end].contains(&b'e'))
    })
}

/// `${(P)n::=on}`: a `${(` flag group with any later `P` and `=` may
/// assign the variable a value names (`options` included). Linear.
fn assigns_named(text: &str) -> bool {
    let (Some(flag), Some(assign)) = (work::rfind(text, "P"), work::rfind(text, "=")) else {
        return false;
    };
    text.find("${(").is_some_and(|at| flag > at && assign > at)
}

/// `$~x`, `${~x}`, `$^~x`, or a `${(flags)...}` with any `~` after it on
/// the line: the `~` flag may sit anywhere in the group, after any
/// delimiter (`${(s:,:~pj:$x:)a}`, `${(@)=~x}`), so a later home folder
/// (`${(f)x} ~/dir`) counts too. Linear.
fn substitutes_globs(text: &str) -> bool {
    let Some(last) = work::rfind(text, "~") else {
        return false;
    };
    text.match_indices('$').any(|(at, _)| {
        let rest = &text[at + 1..];
        let braced = rest.starts_with('{');
        let rest = if braced { &rest[1..] } else { rest };
        let run = rest
            .find(|ch: char| !"^=~#+".contains(ch))
            .unwrap_or(rest.len());
        rest[..run].contains('~') || (braced && rest[run..].starts_with('(') && last > at)
    })
}

/// A write to zsh's `options` array: `options[k]=on`, `options=(...)`,
/// `options+=(...)`, `${options[k]::=on}`, `${(AA)=options::=...}`,
/// `set -Aoptions ...`.
fn writes_options(text: &str) -> bool {
    text.match_indices("options").any(|(at, _)| {
        // An array name attached to its option (`set -Aoptions`,
        // `-xAoptions`), its letters read back at most 64 bytes.
        let bytes = text.as_bytes();
        let mut start = at;
        while start > 0 && at - start < 64 && bytes[start - 1].is_ascii_alphabetic() {
            work::step();
            start -= 1;
        }
        let attached = start > 0
            && start < at
            && bytes[at - 1] == b'A'
            && matches!(bytes[start - 1], b'-' | b'+');
        // Not part of a longer name (`--options=x`, `self.options[k]`).
        attached
            || (!text[..at].ends_with(|ch: char| ch.is_ascii_alphanumeric() || "_-./".contains(ch))
                && ["[", "=", "+=", ":=", "::="]
                    .iter()
                    .any(|next| text[at + "options".len()..].starts_with(next)))
    })
}

/// `$IFS` and `${IFS}` separate words (#1283: marked on both sides).
fn replace_ifs(arg: &str) -> String {
    let split = format!("{HIDDEN} {HIDDEN}");
    let arg = arg.replace("${IFS}", &split);
    super::paths::replace_variable(&arg, "IFS", &split)
}

/// Bounded brace expansion inside each whitespace-separated word:
/// `~/.{x,a}ws` names `~/.xws` and `~/.aws`. Braces inside quotes are left as
/// written, as the shell does (JSON bodies, code). Returns the text with each
/// word's alternatives separated by spaces, and `false` when a word had too
/// many alternatives (left as written).
fn expand_braces(text: &str) -> (String, bool) {
    if !text.contains('{') {
        return (text.to_string(), true);
    }
    // Mask quoted braces and commas so only unquoted ones expand. The quote
    // state follows comments and `$'...'`; a word without any quote character
    // expands whatever the state says, so a stray apostrophe elsewhere (a
    // here-document line) cannot hide it.
    const MASKS: [(char, char); 3] = [('{', '\u{2}'), ('}', '\u{3}'), (',', '\u{4}')];
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Quote {
        None,
        Single,
        Double,
        AnsiC,
        Comment,
    }
    let chars: Vec<char> = text.chars().collect();
    let mut masked = String::with_capacity(text.len());
    let mut quote = Quote::None;
    let mut escaped = false;
    for (index, &ch) in chars.iter().enumerate() {
        work::step();
        let previous = index.checked_sub(1).map(|at| chars[at]);
        if escaped {
            escaped = false;
        } else {
            quote = match (quote, ch) {
                (Quote::Comment, '\n') => Quote::None,
                (Quote::None, '#') if previous.is_none_or(char::is_whitespace) => Quote::Comment,
                (Quote::None, '\'') if previous == Some('$') => Quote::AnsiC,
                (Quote::None, '\'') => Quote::Single,
                (Quote::None, '"') => Quote::Double,
                (Quote::Single | Quote::AnsiC, '\'') | (Quote::Double, '"') => Quote::None,
                (Quote::None | Quote::Double | Quote::AnsiC, '\\') => {
                    escaped = true;
                    quote
                }
                (state, _) => state,
            };
        }
        let inside = matches!(quote, Quote::Single | Quote::Double | Quote::AnsiC);
        let mask = MASKS
            .iter()
            .find(|(from, _)| inside && *from == ch)
            .map_or(ch, |(_, to)| *to);
        masked.push(mask);
    }
    let unmask = |word: &str| {
        MASKS.iter().fold(word.to_string(), |word, (from, to)| {
            word.replace(*to, &from.to_string())
        })
    };
    let mut complete = true;
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let mut flush = |word: &mut String, out: &mut String| {
        if word.is_empty() {
            return;
        }
        let mut words = vec![std::mem::take(word)];
        let mut expanded = false;
        while let Some(position) = words
            .iter()
            .position(|word| brace_alternatives(word).is_some())
        {
            work::step();
            let alternatives = brace_alternatives(&words[position]).unwrap_or_default();
            words.splice(position..=position, alternatives);
            expanded = true;
            if words.len() > MAX_BRACE_EXPANSION {
                complete = false;
                break;
            }
        }
        let separator = if expanded {
            // #1283: each word it made is marked.
            out.push(HIDDEN);
            format!(" {HIDDEN}")
        } else {
            " ".to_string()
        };
        out.push_str(&unmask(&words.join(&separator)));
    };
    let mut has_quote = false;
    for (original, ch) in chars.iter().zip(masked.chars()) {
        work::step();
        if ch.is_whitespace() {
            if !has_quote {
                word = unmask(&word);
            }
            flush(&mut word, &mut out);
            has_quote = false;
            out.push(ch);
        } else {
            has_quote |= matches!(original, '\'' | '"');
            word.push(ch);
        }
    }
    if !has_quote {
        word = unmask(&word);
    }
    flush(&mut word, &mut out);
    (out, complete)
}

/// The alternatives of the first innermost `{a,b}` group, if any.
fn brace_alternatives(word: &str) -> Option<Vec<String>> {
    let close = word.find('}')?;
    let open = work::rfind(&word[..close], "{")?;
    let inner = &word[open + 1..close];
    if !inner.contains(',') {
        return None;
    }
    let (prefix, suffix) = (&word[..open], &word[close + 1..]);
    Some(
        inner
            .split(',')
            .map(|alternative| format!("{prefix}{alternative}{suffix}"))
            .collect(),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Start {
    Plain,
    Piped,
}

/// An open substitution: the command it interrupts and where its own
/// commands start.
struct Open {
    outer: SimpleCommand,
    outer_word: String,
    /// The outer word already holds a lookup's output.
    outer_looked_up: bool,
    /// The outer word is already hidden from the quote-aware reading.
    outer_hidden: bool,
    /// The outer word already holds a parameter operator.
    outer_operated: bool,
    /// #1159: the redirection the outer word is the target of (`> $(cat
    /// f)`), not the substitution's first word.
    outer_redirect: Redirect,
    first: usize,
    backtick: bool,
    /// `>(...)`: the substitution reads the outer command's output.
    output: bool,
    /// `<(...)`: the outer command gets a file name, not the text.
    process: bool,
    /// Opened by `$((`: arithmetic when it also closes with `))`.
    arithmetic: bool,
    /// Past `MAX_SUBSTITUTION_DEPTH`: its commands are read, but not
    /// linked to the command it interrupts or spliced into its words.
    skip: bool,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Redirect {
    #[default]
    None,
    Stdin,
    HereString,
    Stdout,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    /// `( ... )` or `{ ... }`.
    Bracket,
    /// `if`/`while`/`until`/`for`/`case`/`select` ... `fi`/`done`/`esac`.
    Keyword,
}

#[derive(Default)]
struct Lexer {
    /// Finished commands, in order.
    done: Vec<SimpleCommand>,
    current: SimpleCommand,
    word: String,
    /// The word being built holds a lookup's output.
    looked_up: bool,
    /// #1283: the word being built is hidden from the quote-aware reading.
    hidden: bool,
    /// #989: the word being built holds a parameter operator.
    operated: bool,
    open: Vec<Open>,
    /// Open groups and the pipe that feeds every command inside them.
    groups: Vec<(Group, Option<usize>)>,
    redirect: Redirect,
    /// A substitution opened past `MAX_SUBSTITUTION_DEPTH`.
    too_deep: bool,
    /// Open substitutions that are not past `MAX_SUBSTITUTION_DEPTH`.
    real_open: usize,
    /// D194: the operators since the last word (`None`: a word).
    joint: Option<Joint>,
}

impl Lexer {
    /// D194: the operator at `chars[index]`.
    fn operator(&mut self, chars: &[char], index: usize) {
        let previous = index.checked_sub(1).map(|at| chars[at]);
        if let Some(op) =
            certain_cd::operator(previous, chars[index], chars.get(index + 1).copied())
        {
            self.joint = Some(Joint::then(self.joint, op));
        }
    }

    fn flush(&mut self) {
        if !self.word.is_empty() {
            let index = Some(self.current.words.len());
            match std::mem::take(&mut self.redirect) {
                Redirect::Stdin => self.current.stdin_from = index,
                Redirect::HereString => self.current.here_string = index,
                Redirect::Stdout => self.current.writes_to.extend(index),
                Redirect::None => {}
            }
            if std::mem::take(&mut self.looked_up) {
                self.current.looked_up.push(self.current.words.len());
            }
            if std::mem::take(&mut self.hidden) {
                self.current.hidden.push(self.current.words.len());
            }
            if std::mem::take(&mut self.operated) {
                self.current.operated.push(self.current.words.len());
            }
            let word = std::mem::take(&mut self.word);
            let joint = self.joint.take();
            if self.current.words.is_empty() {
                self.current.after = joint.unwrap_or_default();
            }
            self.current.words.push(word);
        }
    }

    /// The pipe feeding the innermost group (stored with it when it opened).
    fn group_pipe(&self) -> Option<usize> {
        self.groups.last().and_then(|(_, pipe)| *pipe)
    }

    fn open_group(&mut self, group: Group, pipe: Option<usize>) {
        let inherited = pipe.or(self.group_pipe());
        self.groups.push((group, inherited));
    }

    fn close_group(&mut self, group: Group) {
        if self.groups.last().is_some_and(|(open, _)| *open == group) {
            self.groups.pop();
        }
    }

    fn finish_current(&mut self) -> Option<usize> {
        self.flush();
        if self.current.words.is_empty() && self.current.fed_by.is_empty() {
            // Nothing to finish: keep its pipe and context (`a | (sh)`,
            // `a |& sh`, `a |` + newline + `sh`).
            return None;
        }
        let next = SimpleCommand {
            substituted: self.current.substituted,
            reads_outer_output: self.current.reads_outer_output,
            ..SimpleCommand::default()
        };
        let finished = std::mem::replace(&mut self.current, next);
        // A compound command fed by a pipe feeds every command inside it.
        let first = finished.words.first().map(|word| word.to_lowercase());
        match first.as_deref() {
            Some("if" | "while" | "until" | "for" | "case" | "select") => {
                self.open_group(Group::Keyword, finished.pipe_from.or(self.group_pipe()));
            }
            Some("fi" | "done" | "esac") => self.close_group(Group::Keyword),
            _ => {}
        }
        self.done.push(finished);
        Some(self.done.len() - 1)
    }

    /// End the current command; a piped one reads the previous one's output,
    /// and one inside a piped group reads the group's input.
    fn start(&mut self, start: Start) {
        let finished = self.finish_current();
        if start == Start::Piped {
            self.current.pipe_from = finished.or(self.done.len().checked_sub(1));
        } else if self.current.pipe_from.is_none() {
            self.current.pipe_from = self.group_pipe();
        }
    }

    /// `(` or `{`: a group; when it directly follows a pipe, the pipe feeds
    /// every command in it.
    fn open_bracket(&mut self) {
        self.flush();
        let piped = if self.current.words.is_empty() {
            self.current.pipe_from
        } else {
            None
        };
        self.start(Start::Plain);
        self.joint = Some(Joint::Other);
        self.open_group(Group::Bracket, piped);
        if self.current.pipe_from.is_none() {
            self.current.pipe_from = self.group_pipe();
        }
    }

    fn close_bracket(&mut self) {
        self.start(Start::Plain);
        self.joint = Some(Joint::Other);
        self.close_group(Group::Bracket);
        self.current.pipe_from = self.group_pipe();
    }

    fn open_substitution(&mut self, backtick: bool, output: bool, process: bool, arithmetic: bool) {
        let skip = self.real_open >= MAX_SUBSTITUTION_DEPTH;
        if skip {
            self.too_deep = true;
        } else {
            self.real_open += 1;
        }
        let outer = std::mem::replace(
            &mut self.current,
            SimpleCommand {
                substituted: true,
                reads_outer_output: output,
                ..SimpleCommand::default()
            },
        );
        let outer_word = std::mem::take(&mut self.word);
        let outer_looked_up = std::mem::take(&mut self.looked_up);
        let outer_hidden = std::mem::take(&mut self.hidden);
        let outer_operated = std::mem::take(&mut self.operated);
        let outer_redirect = std::mem::take(&mut self.redirect);
        self.open.push(Open {
            outer,
            outer_word,
            outer_looked_up,
            outer_hidden,
            outer_operated,
            outer_redirect,
            first: self.done.len(),
            backtick,
            output,
            process,
            arithmetic,
            skip,
        });
        self.joint = Some(Joint::Other);
    }

    /// Close the innermost substitution and continue the command it
    /// interrupted. `echo`/`printf` output is spliced into the word; any
    /// other output is a placeholder the classifier cannot see through.
    /// `arithmetic_close`: this `)` is followed by another (`$((...))`).
    fn close_substitution(&mut self, arithmetic_close: bool) {
        self.finish_current();
        let Some(open) = self.open.pop() else {
            return;
        };
        // Back in the command it interrupted.
        self.joint = None;
        if open.skip {
            // The command it interrupted continues; what it printed is
            // unseen.
            self.current = open.outer;
            self.word = open.outer_word;
            self.looked_up = open.outer_looked_up;
            self.hidden = open.outer_hidden;
            self.operated = open.outer_operated;
            // `> >(cmd)` adds no outer word: its redirection is done.
            if !open.output {
                self.redirect = open.outer_redirect;
                self.word.push_str(UNSEEN_OUTPUT);
            }
            return;
        }
        self.real_open -= 1;
        let inner: Vec<usize> = (open.first..self.done.len()).collect();
        // The commands at this level (not inside a nested substitution),
        // found in one pass over what the inner commands are fed by.
        let nested: std::collections::HashSet<usize> = inner
            .iter()
            .flat_map(|other| self.done[*other].fed_by.iter().copied())
            .collect();
        let top: Vec<usize> = inner
            .iter()
            .copied()
            .filter(|index| !nested.contains(index))
            .collect();
        let spliced = match top.as_slice() {
            _ if open.output || open.process => None,
            [only] => substitution_output(&self.done[*only].words),
            // `$(cd X && pwd)` prints X.
            [cd, pwd]
                if self.done[*pwd].words == ["pwd"]
                    && self.done[*cd]
                        .words
                        .first()
                        .is_some_and(|word| word == "cd") =>
            {
                self.done[*cd].words.get(1).cloned()
            }
            _ => None,
        };
        self.current = open.outer;
        if !open.output {
            self.current.fed_by.extend(inner);
        }
        self.word = open.outer_word;
        self.looked_up = open.outer_looked_up;
        self.hidden = open.outer_hidden || spliced.is_some();
        self.operated = open.outer_operated;
        if !open.output {
            self.redirect = open.outer_redirect;
            // Not literal text: the output of a lookup or of `cd X && pwd`.
            self.looked_up |= spliced.is_some()
                && !matches!(top.as_slice(), [only] if fact_output(&self.done[*only]));
            let unseen = if open.arithmetic && arithmetic_close {
                for index in open.first..self.done.len() {
                    work::step();
                    self.done[index].arithmetic = true;
                }
                ARITHMETIC_OUTPUT
            } else {
                UNSEEN_OUTPUT
            };
            self.word.push_str(spliced.as_deref().unwrap_or(unseen));
        }
    }

    fn finish(mut self) -> Vec<SimpleCommand> {
        while !self.open.is_empty() {
            work::step();
            self.close_substitution(/*arithmetic_close*/ false);
        }
        self.finish_current();
        self.done
    }

    fn arg(&mut self, arg: &str) {
        let chars: Vec<char> = arg.chars().collect();
        let mut index = 0;
        // #989: arithmetic text (`$((...))`, `((...))`) up to here holds no
        // command words, so its parameter operators do not count. Not over
        // a line break: a `((` in a comment or here-document is not
        // arithmetic, and the lines after it are code.
        let mut arithmetic_end = 0;
        let arithmetic_line = |start: usize, end: Option<usize>| {
            end.filter(|end| !chars[start..*end].contains(&'\n'))
        };
        while index < chars.len() {
            work::step();
            let ch = chars[index];
            let next = chars.get(index + 1).copied();
            match ch {
                '$' if index >= arithmetic_end && misread_parameter(&chars[index + 1..]) => {
                    self.operated = true;
                    self.word.push(literal_char(ch));
                }
                '$' if next == Some('\'') => {
                    index = ansi_c(&chars, index + 2, &mut self.word);
                    continue;
                }
                '$' | '<' | '>' if next == Some('(') => {
                    let arithmetic = if ch == '$' && chars.get(index + 2) == Some(&'(') {
                        arithmetic_expansion(&chars, index + 3)
                    } else {
                        None
                    };
                    arithmetic_end =
                        arithmetic_line(index + 3, arithmetic).unwrap_or(arithmetic_end);
                    self.open_substitution(
                        /*backtick*/ false,
                        /*output*/ ch == '>',
                        /*process*/ ch == '<',
                        /*arithmetic*/ arithmetic.is_some(),
                    );
                    index += 2;
                    continue;
                }
                // A line continuation joins the two lines.
                '\\' if next == Some('\n') => index += 1,
                '`' => {
                    if self.open.last().is_some_and(|open| open.backtick) {
                        self.close_substitution(/*arithmetic_close*/ false);
                    } else {
                        self.open_substitution(
                            /*backtick*/ true, /*output*/ false, /*process*/ false,
                            /*arithmetic*/ false,
                        );
                    }
                }
                ')' if self.open.last().is_some_and(|open| !open.backtick) => {
                    self.close_substitution(/*arithmetic_close*/ next == Some(')'));
                }
                '{' if self.word.is_empty() && next.is_none_or(char::is_whitespace) => {
                    self.open_bracket();
                }
                '}' if self.word.is_empty() => self.close_bracket(),
                '\\' => self.current.escaped = true,
                HIDDEN => self.hidden = true,
                '\'' | '"' | '{' | '}' => {}
                '<' if next == Some('<') && chars.get(index + 2) == Some(&'<') => {
                    self.flush();
                    self.redirect = Redirect::HereString;
                    index += 3;
                    continue;
                }
                '<' if next == Some('<') => {
                    // A here-document: its text follows in the script.
                    self.flush();
                    self.joint = Some(Joint::Other);
                    index += 2;
                    continue;
                }
                '<' => {
                    self.flush();
                    self.redirect = Redirect::Stdin;
                }
                // `>&2`, `2>&1`, `>&-`: a descriptor, not a file; `>&file`
                // writes the file.
                '>' if next == Some('&') => {
                    self.flush();
                    let descriptor = chars
                        .get(index + 2)
                        .is_some_and(|after| after.is_ascii_digit() || *after == '-');
                    if !descriptor {
                        self.redirect = Redirect::Stdout;
                    }
                    index += 2;
                    continue;
                }
                '>' => {
                    self.flush();
                    self.redirect = Redirect::Stdout;
                }
                '(' => {
                    if next == Some('(')
                        && self.word.is_empty()
                        && let Some(end) =
                            arithmetic_line(index + 2, arithmetic_expansion(&chars, index + 2))
                    {
                        arithmetic_end = end;
                    }
                    self.open_bracket();
                }
                ')' => self.close_bracket(),
                '|' => {
                    let pipe = next != Some('|') && (index == 0 || chars[index - 1] != '|');
                    self.start(if pipe { Start::Piped } else { Start::Plain });
                    self.operator(&chars, index);
                }
                ';' | '&' | '\n' => {
                    self.start(Start::Plain);
                    self.operator(&chars, index);
                }
                ch if ch.is_whitespace() || matches!(ch, '<' | '>' | ',') => self.flush(),
                ch => self.word.push(literal_char(ch)),
            }
            index += 1;
        }
    }
}

/// #989: whether the parameter expansion after a `$` reads otherwise once
/// the lexer drops braces, quotes and backslashes: a braced form that is not
/// a plain name (`${c:-eval}`, `${!c}`, `${c#x}`, `${c[0]}`), a name joined
/// to the text after it (`${c}al`, `"$c"al` read as `$cal`), or a zsh
/// subscript, modifier or flag (`$c[1]`, `$c:t`, `$=c`). Reads only the name and the
/// quotes after it, so a line of them stays linear.
fn misread_parameter(after: &[char]) -> bool {
    let is_name = |ch: &char| ch.is_ascii_alphanumeric() || *ch == '_';
    let braced = after.first() == Some(&'{');
    let after = if braced { &after[1..] } else { after };
    let len = match after.first() {
        // zsh `$=c`, `$~c`, `$^c` change how the value splits or globs.
        Some(ch) if !braced && "=~^".contains(*ch) => return true,
        Some(ch) if braced && "@*#?$!-".contains(*ch) => 1,
        // `${10}` lexes as `$1` then `0`.
        Some(ch) if braced && ch.is_ascii_digit() => match after.get(1) {
            Some('}') => 1,
            _ => return true,
        },
        Some(ch) if ch.is_ascii_digit() => 0,
        _ => after.iter().take_while(|ch| is_name(ch)).count(),
    };
    let rest = if braced {
        if len == 0 || after.get(len) != Some(&'}') {
            return true;
        }
        &after[len + 1..]
    } else if len == 0 {
        return false;
    } else {
        &after[len..]
    };
    let quotes = rest
        .iter()
        .take_while(|ch| matches!(ch, '\'' | '"' | '\\'))
        .count();
    ((braced || quotes > 0) && rest.get(quotes).is_some_and(is_name))
        // `$c$'al'`: the decoded text joins the name too.
        || (rest.get(quotes) == Some(&'$') && rest.get(quotes + 1) == Some(&'\''))
        || (!braced && matches!(rest.first(), Some('[' | ':')))
}

/// Whether the `$((` before `start` is certainly an arithmetic expansion:
/// it closes with `))` and holds nothing that could make bash or zsh read it
/// as a command substitution instead (`$(((1));cat f)`, `$((echo '))';cat f) )`).
/// When unsure, `None`: its value then counts as command output. Otherwise
/// the index of its first closing `)`.
fn arithmetic_expansion(chars: &[char], start: usize) -> Option<usize> {
    let mut braces = 0_usize;
    for (at, ch) in chars.iter().enumerate().skip(start) {
        work::step();
        match ch {
            '\'' | '"' | '`' | '\\' | '(' => return None,
            '{' => braces += 1,
            '}' => braces = braces.saturating_sub(1),
            ')' => return (braces == 0 && chars.get(at + 1) == Some(&')')).then_some(at),
            _ => {}
        }
    }
    None
}

/// Decode a bash `$'...'` string starting at `index` (after the quote)
/// into `word`; returns the index after the closing quote.
pub(super) fn ansi_c(chars: &[char], mut index: usize, word: &mut String) -> usize {
    let digits = |chars: &[char], start: usize, radix: u32, max: usize| {
        let text: String = chars[start..]
            .iter()
            .take(max)
            .take_while(|ch| ch.is_digit(radix))
            .collect();
        let value = u32::from_str_radix(&text, radix).ok();
        (value.and_then(char::from_u32), text.len())
    };
    while index < chars.len() {
        work::step();
        match chars[index] {
            '\'' => return index + 1,
            '\\' if index + 1 < chars.len() => {
                let escape = chars[index + 1];
                index += 2;
                let simple = match escape {
                    'n' => Some('\n'),
                    't' => Some('\t'),
                    'r' => Some('\r'),
                    'a' => Some('\u{7}'),
                    'b' => Some('\u{8}'),
                    'e' | 'E' => Some('\u{1b}'),
                    'f' => Some('\u{c}'),
                    'v' => Some('\u{b}'),
                    'x' | 'u' | 'U' | '0'..='7' => None,
                    other => Some(other),
                };
                if let Some(ch) = simple {
                    word.push(literal_char(ch));
                    continue;
                }
                let (decoded, used) = match escape {
                    'x' => digits(chars, index, 16, 2),
                    'u' => digits(chars, index, 16, 4),
                    'U' => digits(chars, index, 16, 8),
                    _ => {
                        // Octal: the escape character is the first digit.
                        index -= 1;
                        digits(chars, index, 8, 3)
                    }
                };
                index += used;
                word.extend(decoded.map(literal_char));
            }
            ch => {
                word.push(literal_char(ch));
                index += 1;
            }
        }
    }
    index
}

/// What a substitution prints when the classifier can tell: `echo`/`printf`
/// text, the name a lookup command was given (`which corbanu` stands for
/// `corbanu`), or a neutral value for commands that print facts, not code.
pub(super) fn substitution_output(words: &[String]) -> Option<String> {
    if let Some(printed) = literal_output(words) {
        return Some(printed);
    }
    let (first, rest) = words.split_first()?;
    let operand = rest.iter().rev().find(|word| !word.starts_with('-'));
    // A fact's arguments must be literal: `date +"$v"` and
    // `git rev-parse --no-revs "$v"` print them back.
    let literal = rest
        .iter()
        .all(|word| !word.contains('$') && !word.contains('`') && !word.contains(UNSEEN_OUTPUT));
    match basename(&first.to_lowercase()) {
        "which" | "dirname" | "basename" | "realpath" | "readlink" => operand.cloned(),
        "command" if rest.first().is_some_and(|word| word == "-v") => operand.cloned(),
        "pwd" => Some("$PWD".to_string()),
        "date" | "uname" | "whoami" | "hostname" | "id" | "nproc" | "getconf" if literal => {
            Some("value".to_string())
        }
        // The repository root is at or above the working folder.
        "git" if literal && rest.first().is_some_and(|word| word == "rev-parse") => Some(
            if rest.iter().any(|word| word == "--show-toplevel") {
                "$PWD"
            } else {
                "value"
            }
            .to_string(),
        ),
        _ => None,
    }
}

/// Whether the substitution's spliced output is literal text or a neutral
/// fact (`echo x`, `date`, `pwd`), not something a lookup found.
fn fact_output(command: &SimpleCommand) -> bool {
    let words = &command.words;
    // `printf`/`echo -e`/zsh `echo` decode escapes (`\x24`) and `printf`
    // joins operands (`%s%s`), so what they print is not the text shown.
    let decodes = command.escaped
        || words.first().is_some_and(|first| {
            basename(&first.to_lowercase()) == "printf"
                && words[1..].iter().any(|word| word.contains('%'))
        });
    (literal_output(words).is_some() && !decodes)
        || words.first().is_some_and(|first| {
            matches!(
                basename(&first.to_lowercase()),
                "date" | "uname" | "whoami" | "hostname" | "id" | "nproc" | "getconf"
            )
        })
}

/// What `echo`/`printf` with these words prints, if that is all they are.
pub(super) fn literal_output(words: &[String]) -> Option<String> {
    let (first, rest) = words.split_first()?;
    if !matches!(basename(&first.to_lowercase()), "echo" | "printf") {
        return None;
    }
    let printed: Vec<&str> = rest
        .iter()
        .map(String::as_str)
        .skip_while(|word| word.starts_with('-'))
        .collect();
    Some(printed.join(" "))
}
