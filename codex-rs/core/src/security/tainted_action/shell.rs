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
    /// Indexes of the words its stdin is redirected from (`< file`), in
    /// ascending order: every one, not only the last (#1772 item 3: `<a <b
    /// ./x`).
    pub(super) stdin_from: Vec<usize>,
    /// Indexes of its here-string words (`<<< word`), in ascending order.
    pub(super) here_string: Vec<usize>,
    /// Indexes of the words its output is redirected to (`> file`, `>> file`).
    pub(super) writes_to: Vec<usize>,
    /// #1482 item 2: indexes of the words a redirection holds that are not
    /// its file: a descriptor number (`2` of `2>log` and `2>&1`, `1` of
    /// `2>&1`, `2` of `>&2`), a `{fd}` name (`{fd}>log`, read as `fd`) and
    /// a here-document's delimiter (`EOF` of `<<EOF`). The shell runs none
    /// of them.
    pub(super) redirect_words: Vec<usize>,
    /// #1607: files zsh writes where bash reads the line otherwise, by
    /// word index: the word after `>!` (`>! f`, which bash passes on as an
    /// argument; `false`) or the rest of the `!` word (`>!f`, bash's file
    /// `!f`; `true`: drop the `!`). Judged only as files written.
    pub(super) zsh_writes: Vec<(usize, bool)>,
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
    /// #1772 review 3: a redirection sends its stdout elsewhere (`>log`,
    /// `>&2`, `1>&2`, `&>log`; not `2>log` or `2>&1`), so a substitution
    /// of it prints nothing the words show.
    pub(super) stdout_redirected: bool,
    /// #1772 item 1: when `redirections_last` moved the words a
    /// redirection holds after the others, the position each word had as
    /// written (`written_at[now]`); empty when nothing moved.
    pub(super) written_at: Vec<usize>,
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
    /// #1772 review 1: some text was read where main reads it otherwise: a
    /// quoted or escaped redirection file holding a blank as one word,
    /// `&>` as one redirection, a redirection's words moved last, a second
    /// `<` or `<<<`, or a substitution with its stdout redirected left
    /// unspliced (see `simple_commands_read`).
    pub(super) read_newly: bool,
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

/// Simple commands of the argv and of any script inside it (`bash -lc "..."`),
/// read as main reads them. #1772 review 2: the helper lexers (`one_word`,
/// `holds_git_name`, arithmetic's) keep main's reading; only the
/// classifier's own lexing reads newly (`simple_commands_read`).
pub(super) fn simple_commands(command: &[String]) -> Lexed {
    simple_commands_read(command, /*main_reading*/ true)
}

/// [`simple_commands`], with `main_reading`: as main reads redirections.
/// #1772 review 1: the lexer cannot tell whether the `>` or `<` before a
/// quote was inside quotes, a comment, a `case` pattern or a here-document
/// (`echo '>"'; ./x; echo '"'`), and `&>` splits under `sh` and `dash`
/// (`true &>f ./x` runs `./x`). So a quoted or escaped redirection file is
/// one word and `&>` one redirection only when `main_reading` is off; the
/// classifier then also judges the action with it on and keeps the
/// stricter verdict (`Lexed::read_newly`). #1772 review 2: with
/// `main_reading` on, nothing is read otherwise than on main: no word moves
/// (`SimpleCommand::redirections_last`), only the last `<` and `<<<` are
/// kept, and a substitution with its stdout redirected is spliced.
pub(super) fn simple_commands_read(command: &[String], main_reading: bool) -> Lexed {
    let mut lexer = Lexer {
        joint: Some(Joint::Start),
        main_reading,
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
    // #1772 review 2: after `finish`, which reorders the last command.
    let (commands, read_newly) = lexer.finish();
    Lexed {
        commands,
        incomplete,
        too_deep,
        read_newly,
    }
}

/// #1482 item 5: whether `text`, read as a line, is one word. An argv
/// element is one word whatever it holds; one that the lexer reads as
/// several words or commands (`A=1 ./y`, `./{x,y}`, ``true`./x` ``) is not
/// the word the classifier judges. Quotes and braces the lexer drops in one
/// word (`-I{}`) are read as before. Empty text runs nothing.
pub(super) fn one_word(text: &str) -> bool {
    if text.is_empty() {
        return true;
    }
    let lexed = simple_commands(&[text.to_string()]);
    !lexed.incomplete
        && !lexed.too_deep
        && matches!(
            lexed.commands.as_slice(),
            [only] if only.fed_by.is_empty() && only.words.len() == 1
        )
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
    /// #1607: the outer word follows a zsh `>!` (`>!$(cat f)`, `>! $(cat f)`).
    outer_clobber: bool,
    outer_zsh_writes_next: bool,
    outer_redirect_word_next: bool,
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
    /// #1607: a `!` follows the output redirection just read (`>!`, `>>!`,
    /// `&>!`, `>&!`, `2>!`), which zsh reads as `>|`: the file is the word
    /// after the `!`, where bash writes a file `!` (or `!f`).
    clobber: bool,
    /// #1607: zsh writes the next word (after `>! `).
    zsh_writes_next: bool,
    /// #1482 item 2: the next word is part of a redirection, not its file
    /// (`SimpleCommand::redirect_words`).
    redirect_word_next: bool,
    /// A substitution opened past `MAX_SUBSTITUTION_DEPTH`.
    too_deep: bool,
    /// Open substitutions that are not past `MAX_SUBSTITUTION_DEPTH`.
    real_open: usize,
    /// D194: the operators since the last word (`None`: a word).
    joint: Option<Joint>,
    /// #1772 review 1: redirections are read as on main
    /// (`simple_commands_read`).
    main_reading: bool,
    /// #1772 review 1: see `Lexed::read_newly`.
    read_newly: bool,
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
            if std::mem::take(&mut self.redirect_word_next) {
                self.current.redirect_words.extend(index);
            }
            let redirect = std::mem::take(&mut self.redirect);
            let clobber = std::mem::take(&mut self.clobber) && redirect == Redirect::Stdout;
            match redirect {
                // #1772 review 2: main keeps only the last one.
                Redirect::Stdin if self.main_reading => {
                    self.current.stdin_from = index.into_iter().collect()
                }
                Redirect::HereString if self.main_reading => {
                    self.current.here_string = index.into_iter().collect();
                }
                Redirect::Stdin => {
                    self.read_newly |= !self.current.stdin_from.is_empty();
                    self.current.stdin_from.extend(index);
                }
                Redirect::HereString => {
                    self.read_newly |= !self.current.here_string.is_empty();
                    self.current.here_string.extend(index);
                }
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
            // #1607: the words stay bash's; zsh's file after the `!` is
            // the rest of this word (`>!f`) or the next word (`>! f`).
            let at = self.current.words.len();
            if std::mem::take(&mut self.zsh_writes_next) {
                self.current.zsh_writes.push((at, false));
            }
            match word.strip_prefix('!').filter(|_| clobber) {
                Some("") => self.zsh_writes_next = true,
                Some(_) => self.current.zsh_writes.push((at, true)),
                None => {}
            }
            self.current.words.push(word);
        }
    }

    /// #1772 review 3: at an output redirection operator, whether it
    /// redirects stdout: no descriptor number before it, or `1`. A `{fd}`
    /// name counts as stdout too (it can only ask more).
    fn redirects_stdout(&self) -> bool {
        let word = &self.word;
        word.is_empty() || word == "1" || !word.bytes().all(|byte| byte.is_ascii_digit())
    }

    /// #1482 item 2: at a redirection operator, whether the word being
    /// built is its descriptor (`2>log`, `2>&1`, `0<f`) or `{fd}` name
    /// (`{fd}>log`, its braces already dropped): flushed as part of the
    /// redirection.
    fn redirect_prefix(&mut self, previous: Option<char>) {
        let word = &self.word;
        if !word.is_empty()
            && (word.bytes().all(|byte| byte.is_ascii_digit())
                || (previous == Some('}')
                    && word
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')))
        {
            self.redirect_word_next = true;
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
        self.zsh_writes_next = false;
        self.redirect_word_next = false;
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
        let mut finished = std::mem::replace(&mut self.current, next);
        // #1772 review 2: main's reading moves no word (`>/dev/null done`
        // keeps bash's reading: `done` is no reserved word there).
        if !self.main_reading {
            self.read_newly |= finished.redirections_last();
        }
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
        let outer_clobber = std::mem::take(&mut self.clobber);
        let outer_zsh_writes_next = std::mem::take(&mut self.zsh_writes_next);
        let outer_redirect_word_next = std::mem::take(&mut self.redirect_word_next);
        self.open.push(Open {
            outer,
            outer_word,
            outer_looked_up,
            outer_hidden,
            outer_operated,
            outer_redirect,
            outer_clobber,
            outer_zsh_writes_next,
            outer_redirect_word_next,
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
                self.clobber = open.outer_clobber;
                self.zsh_writes_next = open.outer_zsh_writes_next;
                self.redirect_word_next = open.outer_redirect_word_next;
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
            // #1772 review 3: `$(>&2 echo x)`, `$(echo x >log)` print
            // nothing here.
            [only] if !self.main_reading && self.done[*only].stdout_redirected => {
                self.read_newly = true;
                None
            }
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
            self.clobber = open.outer_clobber;
            self.zsh_writes_next = open.outer_zsh_writes_next;
            self.redirect_word_next = open.outer_redirect_word_next;
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

    /// The commands, and whether any text was read newly
    /// (`Lexed::read_newly`).
    fn finish(mut self) -> (Vec<SimpleCommand>, bool) {
        while !self.open.is_empty() {
            work::step();
            self.close_substitution(/*arithmetic_close*/ false);
        }
        self.finish_current();
        (self.done, self.read_newly)
    }

    fn arg(&mut self, arg: &str) {
        let chars: Vec<char> = arg.chars().collect();
        let mut index = 0;
        // #989: arithmetic text (`$((...))`, `((...))`) up to here holds no
        // command words, so its parameter operators do not count. Not over
        // a line break: a `((` in a comment or here-document is not
        // arithmetic, and the lines after it are code.
        let mut arithmetic_end = 0;
        // #1772 item 2: a quote of this kind (`[single, double]`) has no
        // closing one later in the text.
        let mut unclosed = [false; 2];
        // The closing quote of double quotes read as before: none up to it
        // opens a quoted redirection file.
        let mut read_as_before: Option<usize> = None;
        let arithmetic_line = |start: usize, end: Option<usize>| {
            end.filter(|end| !chars[start..*end].contains(&'\n'))
        };
        while index < chars.len() {
            work::step();
            let ch = chars[index];
            let next = chars.get(index + 1).copied();
            let previous = index.checked_sub(1).map(|at| chars[at]);
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
                // #1772 item 2: an escaped space in a redirection's file
                // (`>a\ b ./x`) is part of the file name.
                '\\' if self.redirect != Redirect::None
                    && !self.main_reading
                    && next.is_some_and(char::is_whitespace) =>
                {
                    self.read_newly = true;
                    self.current.escaped = true;
                    self.word.extend(next);
                    index += 2;
                    continue;
                }
                // #1772 item 2: a quoted redirection file (`>'a b' ./x`,
                // `2>"a b" ./x`) is one word, spaces and operators included.
                // A double-quoted one holding an expansion or escape, and an
                // escaped quote (`>\'a`), are read as before.
                '\'' | '"'
                    if self.redirect != Redirect::None
                        && !self.main_reading
                        && previous != Some('\\')
                        && !unclosed[usize::from(ch == '"')]
                        && read_as_before.is_none_or(|end| index > end) =>
                {
                    let end = match quoted_target(&chars, index, &mut unclosed) {
                        Ok(end) => end,
                        Err(close) => {
                            // Read as before up to the closing quote: the
                            // quotes are dropped, and no quote inside opens
                            // (`>"$d's" ./x`).
                            read_as_before = close;
                            index += 1;
                            continue;
                        }
                    };
                    self.read_newly = true;
                    for inner in &chars[index + 1..end] {
                        work::step();
                        if *inner == HIDDEN {
                            self.hidden = true;
                        } else {
                            self.word.push(literal_char(*inner));
                        }
                    }
                    index = end + 1;
                    continue;
                }
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
                    self.redirect_prefix(previous);
                    self.flush();
                    self.redirect = Redirect::HereString;
                    index += 3;
                    continue;
                }
                '<' if next == Some('<') => {
                    // A here-document: its text follows in the script.
                    self.redirect_prefix(previous);
                    self.flush();
                    self.redirect_word_next = true;
                    self.joint = Some(Joint::Other);
                    index += 2;
                    continue;
                }
                '<' => {
                    self.redirect_prefix(previous);
                    self.flush();
                    self.redirect = Redirect::Stdin;
                }
                // `>&2`, `2>&1`, `>&-`: a descriptor, not a file; `>&file`
                // writes the file.
                '>' if next == Some('&') => {
                    self.current.stdout_redirected |= self.redirects_stdout();
                    self.redirect_prefix(previous);
                    self.flush();
                    let descriptor = chars
                        .get(index + 2)
                        .is_some_and(|after| after.is_ascii_digit() || *after == '-');
                    if descriptor {
                        self.redirect_word_next = true;
                    } else {
                        self.redirect = Redirect::Stdout;
                        self.clobber = chars.get(index + 2) == Some(&'!');
                    }
                    index += 2;
                    continue;
                }
                '>' => {
                    self.current.stdout_redirected |= self.redirects_stdout();
                    self.redirect_prefix(previous);
                    self.flush();
                    self.redirect = Redirect::Stdout;
                    self.clobber = next == Some('!');
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
                // #1772 item 4: `&>` and `&>>` redirect stdout and stderr
                // in bash and zsh; the `>` is read next. Not a command
                // separator. `sh` and `dash` read `&` and `>` (review 2).
                '&' if next == Some('>') && !self.main_reading => {
                    self.read_newly = true;
                    self.flush();
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

/// #1772 item 2: the index of the quote closing the one at `start`, when
/// the text between them is literal: single quotes, or double quotes
/// holding no `$`, backtick or backslash. Otherwise it is read as before:
/// `Err` with the closing quote of double quotes holding one, which opens
/// nothing. A quote with no closing one sets `unclosed` (`[single,
/// double]`), so no later quote of that kind is looked for: each text is
/// scanned once.
fn quoted_target(
    chars: &[char],
    start: usize,
    unclosed: &mut [bool; 2],
) -> Result<usize, Option<usize>> {
    let quote = chars[start];
    let mut special = false;
    for (at, ch) in chars.iter().enumerate().skip(start + 1) {
        work::step();
        if *ch == quote {
            return if quote == '\'' || !special {
                Ok(at)
            } else {
                Err(Some(at))
            };
        }
        special |= matches!(ch, '$' | '`' | '\\');
    }
    unclosed[usize::from(quote == '"')] = true;
    Err(None)
}

impl SimpleCommand {
    /// Whether a redirection holds the word at `at`: its file, descriptor,
    /// `{fd}` name or here-document delimiter. The lexer adds the indexes in
    /// ascending order.
    pub(super) fn held(&self, at: usize) -> bool {
        work::step();
        self.writes_to.binary_search(&at).is_ok()
            || self.redirect_words.binary_search(&at).is_ok()
            || self.stdin_from.binary_search(&at).is_ok()
            || self.here_string.binary_search(&at).is_ok()
    }

    /// #1772 item 1: the shell removes a redirection's words wherever they
    /// stand, so `>log cd sub` runs `cd sub` as `cd sub >log` does. The
    /// words a redirection holds move after the others, in order, and
    /// every index moves with its word: each reading then sees the command
    /// it runs, and its redirections where it reads them on any line.
    /// Commands whose redirections already come last are unchanged.
    /// Returns whether any word moved.
    fn redirections_last(&mut self) -> bool {
        let held: Vec<bool> = (0..self.words.len()).map(|at| self.held(at)).collect();
        let Some(first_held) = held.iter().position(|held| *held) else {
            return false;
        };
        if held[first_held..].iter().all(|held| *held) {
            return false;
        }
        let order: Vec<usize> = (0..held.len())
            .filter(|at| !held[*at])
            .chain((0..held.len()).filter(|at| held[*at]))
            .collect();
        let mut moved = vec![0; held.len()];
        for (to, from) in order.iter().enumerate() {
            work::step();
            moved[*from] = to;
        }
        let mut words = std::mem::take(&mut self.words)
            .into_iter()
            .map(Some)
            .collect::<Vec<_>>();
        self.words = order
            .iter()
            .filter_map(|from| words.get_mut(*from).and_then(Option::take))
            .collect();
        for indexes in [
            &mut self.stdin_from,
            &mut self.here_string,
            &mut self.writes_to,
            &mut self.redirect_words,
            &mut self.looked_up,
            &mut self.hidden,
            &mut self.operated,
        ] {
            work::step();
            for at in indexes.iter_mut() {
                work::step();
                *at = moved.get(*at).copied().unwrap_or(*at);
            }
            indexes.sort_unstable();
        }
        for (at, _) in &mut self.zsh_writes {
            work::step();
            *at = moved.get(*at).copied().unwrap_or(*at);
        }
        self.zsh_writes.sort_unstable();
        self.written_at = order;
        true
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
