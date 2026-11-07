//! #1422: aliases an action defines (`alias b=bash`) and runs (`b run.sh`;
//! bash with `expand_aliases`, zsh and sh expand them). Neither reading
//! expands them: the word lexer ignores the quotes of a definition (`alias
//! e='true;eval'`) and of a use, and the quote-aware reading's checks go by
//! command name. So a word naming an alias the line defines asks, wherever
//! it is (`eval 'b run.sh'`), even read before the definition it runs
//! after (`f() { b; }; alias b=bash; f`, a trap). A definition that does
//! not name its alias asks (`BASH_ALIASES[$k]=bash`, `aliases=(b bash)`,
//! `declare -n r=BASH_ALIASES`, zsh suffix aliases). An alias defined but
//! not used stays quiet (`alias gs='git status'`).

use super::indirect;
use super::shell::UNSEEN_OUTPUT;
use super::shell::basename;
use super::work;
use std::collections::HashSet;

/// The parameters that hold aliases: bash's, and zsh's regular, global and
/// suffix ones.
const TABLES: &[&str] = &["BASH_ALIASES", "aliases", "galiases", "saliases"];

#[derive(Default)]
pub(super) struct Aliases {
    /// Lowercase names the line defines.
    names: HashSet<String>,
    /// A definition the readings cannot name.
    opaque: bool,
    /// #1449: see [`Self::of`].
    split: bool,
}

impl Aliases {
    /// The aliases `commands` (a line's simple commands, as written)
    /// define, so a use read before its definition counts. `split`: the
    /// line runs `alias` after a quoted assignment
    /// ([`quoted_assignment_alias`]), which the word lexer may split, so
    /// any `alias` word may be the command (`x='a b' alias $1` is `x=a`,
    /// `b`, `alias`, `$1`).
    pub(super) fn of(commands: &[super::shell::SimpleCommand], split: bool) -> Self {
        let mut aliases = Self {
            split,
            ..Self::default()
        };
        for command in commands {
            work::step();
            aliases.define(&command.words, /*as_written*/ true);
        }
        aliases
    }

    /// Read `words`, a simple command's with values filled in (`a=alias;
    /// $a b=bash`, `k=b; BASH_ALIASES[$k]=bash`): whether a word names an
    /// alias the line defines, or a definition cannot be named.
    pub(super) fn read(&mut self, words: &[String]) -> bool {
        self.define(words, /*as_written*/ false);
        self.opaque
            || (!self.names.is_empty()
                && words.iter().any(|word| {
                    work::step();
                    self.names.contains(&basename(word).to_lowercase())
                }))
    }

    /// The definitions in one command's `words`: after an `alias` word (or,
    /// as written, a command word holding a value, which may be `alias`:
    /// `$a`, `al$x`), each `name=value` word. Where the definer is the
    /// command (past `eval`, zsh `noglob`/`nocorrect`/`-`, #1449 `builtin`
    /// and `command`, and their options, `eval -- alias`; #1449: anywhere,
    /// as written, on a `split` line), a word before the first definition
    /// that
    /// may hold one asks: a value or a glob (`alias $1`, `alias "$@"`,
    /// `alias b?bash`), as does `-s` (zsh suffix aliases); so does a name
    /// holding one (`alias b[=]bash`). Words after a definition may be its
    /// quoted value (`alias k='kubectl --context $CTX'`). Any alias table
    /// word counts too, and zsh `set -A aliases`.
    fn define(&mut self, words: &[String], as_written: bool) {
        if self.opaque {
            return;
        }
        let command = indirect::command_word(words).map(|(at, _)| at);
        let valued = command.filter(|at| as_written && unknown(&words[*at]));
        // Where a command-position `alias` is: past the words that run
        // their operands as a command.
        let position = command.map(|command| {
            command
                + words[command..]
                    .iter()
                    .take_while(|word| {
                        work::step();
                        matches!(
                            word.as_str(),
                            "eval" | "noglob" | "nocorrect" | "-" | "builtin" | "command"
                        ) || word.starts_with('-')
                    })
                    .count()
        });
        let sets_array = command.is_some_and(|at| {
            words[at] == "set"
                && words[at + 1..].iter().any(|word| {
                    work::step();
                    word.starts_with(['-', '+']) && word.contains('A')
                })
        });
        let mut defining = false;
        let mut commanding = false;
        let mut named = false;
        for (at, word) in words.iter().enumerate() {
            work::step();
            self.table(word);
            self.opaque |= sets_array && TABLES.contains(&word.as_str());
            if basename(word).eq_ignore_ascii_case("alias") || valued == Some(at) {
                defining = true;
                named = false;
                // A command named by a value defines only `name=value`
                // words (`$PYTHON "$f"` is not `alias "$f"`).
                commanding =
                    valued != Some(at) && (position == Some(at) || (as_written && self.split));
                continue;
            }
            if !defining {
                continue;
            }
            if let Some((name, _)) = word.split_once('=')
                && !name.is_empty()
                && !word.starts_with('-')
            {
                named = true;
                if unknown(name) || (commanding && globbed(name)) {
                    self.opaque = true;
                } else {
                    self.names.insert(name.to_lowercase());
                }
            } else if commanding && !named {
                // `alias -s sh=bash`: suffix aliases run for file names.
                self.opaque |= if word.starts_with('-') {
                    !word.starts_with("--") && word.contains('s')
                } else {
                    unknown(word) || globbed(word)
                };
            }
        }
    }

    /// An alias table in `word`: a subscript anywhere (`BASH_ALIASES[b]`;
    /// `read`, `printf -v` and `${…:=}` write one too) defines its key; a
    /// key not known, a suffix alias, an assignment to the whole table
    /// (`aliases=(…)`) or a reference to one (`r=BASH_ALIASES`) cannot be
    /// named.
    fn table(&mut self, word: &str) {
        for table in TABLES {
            work::step();
            for (at, _) in word.match_indices(table) {
                work::step();
                let rest = &word[at + table.len()..];
                let before = word[..at].chars().next_back();
                // A parameter's name starts the word or follows `${`, `$`,
                // `${!`, `${#`, `=`, zsh flags `${(AA)` or an option's
                // letter (`printf -vBASH_ALIASES[b]`), not `jq
                // '.aliases[0]'`.
                let option = word.starts_with('-') && before == Some('v');
                if !option && !matches!(before, None | Some('{' | '$' | '!' | '#' | '=' | ')')) {
                    continue;
                }
                if before == Some(')') {
                    self.opaque = true;
                    return;
                }
                if let Some(subscript) = rest.strip_prefix('[') {
                    // Up to its `]`; one inside another is not read.
                    let key = match subscript.find([']', '[']) {
                        Some(end) if subscript[end..].starts_with(']') => &subscript[..end],
                        _ => {
                            self.opaque = true;
                            return;
                        }
                    };
                    if *table == "saliases" || key.is_empty() || unknown(key) {
                        self.opaque = true;
                    } else {
                        self.names.insert(key.to_lowercase());
                    }
                } else if (at == 0 && (rest.starts_with('=') || rest.starts_with("+=")))
                    || (before == Some('=') && rest.is_empty())
                {
                    self.opaque = true;
                }
            }
        }
    }
}

/// #1449: whether `text` runs `alias` as the command after an assignment
/// whose value is quoted (`x='a b' alias $1`, `x="a;b" builtin alias
/// $1`): past further assignments, `builtin`, `command`, `eval`, zsh
/// `noglob`/`nocorrect`/`-` and options. Shell words are read with their
/// quotes, up to a blank or an operator outside them. One pass over
/// `text`.
pub(super) fn quoted_assignment_alias(text: &str) -> bool {
    let mut after_quoted = false;
    let mut word = String::new();
    let mut quoted = false;
    let mut quote = None;
    let mut escaped = false;
    for ch in text.chars().chain(std::iter::once('\n')) {
        work::step();
        if escaped {
            escaped = false;
            word.push(ch);
            continue;
        }
        match (quote, ch) {
            (None | Some('"'), '\\') => {
                // The word lexer splits `x=a\ b` as it splits `x='a b'`.
                escaped = true;
                quoted |= quote.is_none();
                continue;
            }
            (None, '\'' | '"') => {
                quote = Some(ch);
                quoted = true;
                continue;
            }
            (Some(open), _) if ch == open => {
                quote = None;
                continue;
            }
            (Some(_), _) => {
                word.push(ch);
                continue;
            }
            (None, _) if !ch.is_whitespace() && !";|&()<>".contains(ch) => {
                word.push(ch);
                continue;
            }
            _ => {}
        }
        // A word ends.
        if !word.is_empty() || quoted {
            let assignment = word.split_once('=').is_some_and(|(name, _)| {
                let name = name.strip_suffix('+').unwrap_or(name);
                !name.is_empty()
                    && name
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            });
            if assignment {
                after_quoted |= quoted;
            } else if after_quoted && word == "alias" {
                return true;
            } else if !(after_quoted
                && (matches!(
                    word.as_str(),
                    "builtin" | "command" | "eval" | "noglob" | "nocorrect" | "-"
                ) || word.starts_with('-')))
            {
                after_quoted = false;
            }
        }
        word.clear();
        quoted = false;
        // An operator or a line break ends the command.
        if ";|&()<>\n".contains(ch) {
            after_quoted = false;
        }
    }
    false
}

/// Whether `text` may glob into other words.
fn globbed(text: &str) -> bool {
    text.contains(['*', '?', '['])
}

/// Whether `text` holds a value the readings do not know.
fn unknown(text: &str) -> bool {
    text.contains(['$', '`']) || text.contains(UNSEEN_OUTPUT)
}
