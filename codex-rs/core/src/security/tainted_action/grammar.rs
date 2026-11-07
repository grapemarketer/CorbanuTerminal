//! #932: the bash grammar's reading of shell text (tree-sitter-bash, as
//! `codex-shell-command` already uses it), beside the hand-written reader in
//! `quoted.rs`. It finds the simple commands a line runs, those inside
//! substitutions included, and tells which of their words are data: text
//! no shell expands. It refuses, rather than guesses at, text it knows it
//! may not read the way the shells do: a parse error or missing token, text
//! too long or too slow to parse, characters some locales read differently,
//! nesting too deep to walk, and the desyncs listed in the design note.
//!
//! Best-effort: bash, zsh and dash differ in more ways than these refusals
//! cover, so an Ok reading is evidence, not proof. It may decide something
//! only together with an independent check, for narrow forms (see the
//! design note's "What the reading promises").
//!
//! See docs/development/shell-grammar-classifier.md. The differential
//! harness (`grammar_harness_tests.rs`) compares this reading with
//! `quoted.rs` and with real shells; `shadow.rs` compares it on every
//! command line the classifier reads and logs a difference. Nothing is
//! classified by it yet.

use super::shell::UNSEEN_OUTPUT;
use super::shell::ansi_c;
use super::work;
use codex_shell_command::bash::ParseStop;
use codex_shell_command::bash::SyntaxNode as Node;
use codex_shell_command::bash::try_parse_shell_bounded;

/// Longer text is refused. Some valid text (`x | x | ...`, nested arrays)
/// takes tree-sitter time quadratic in its length, which neither of its
/// work counters bounds; this keeps the worst case small.
pub(super) const MAX_BYTES: usize = 4096;

/// More here-documents than this, or a longer delimiter, is refused: the
/// grammar's scanner keeps its pending here-documents in a state of at most
/// 1024 bytes and silently forgets them all past that.
const MAX_HERE_DOCUMENTS: usize = 8;
const MAX_DELIMITER_BYTES: usize = 64;

/// Syntax nodes nested deeper than this are refused.
const MAX_DEPTH: usize = 512;

/// tree-sitter checks its progress about once per 100 parse operations.
/// Text of `bytes` may use this many checks; more is refused. With the stop
/// at the first syntax error (error recovery can be quadratic), this bounds
/// the parse linearly in the text.
pub(super) fn parse_budget(bytes: usize) -> usize {
    bytes / 2 + 64
}

/// Why text was not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Refusal {
    TooLarge,
    /// A control character other than newline or tab, or ASCII punctuation
    /// right after a non-ASCII character: in a multibyte locale such as GBK
    /// or Big5 a shell may read that byte as part of the character.
    Characters,
    /// The parse ran past [`parse_budget`].
    ParseBudget,
    /// The grammar found an error or a missing token.
    Syntax,
    TooDeep,
    /// Too many here-documents, or a delimiter too long, for the grammar's
    /// scanner to keep track of.
    HereDocuments,
    /// A construct the grammar reads differently from the shells' defaults:
    /// an extended glob pattern (`@(a|b)`) is a syntax error in bash without
    /// `shopt -s extglob`.
    Unsupported,
    /// The grammar's words may not be the shells': a reserved word as a
    /// command's name, a word joined to a redirection's (`>f"x"`), blanks
    /// or operators inside a word, a `#` comment inside a word.
    Words,
    /// A backslash-newline, which the shells remove before reading words.
    LineContinuation,
    /// A command that may change how the shells read the lines after it:
    /// `alias`, `shopt`, `setopt`, ... named anywhere in a command, or a
    /// command name spelled through an expansion.
    Reparse,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Word {
    /// The text the command receives when `data`. Otherwise an
    /// approximation: expansions as written, a substitution's output as
    /// `UNSEEN_OUTPUT`.
    pub(super) text: String,
    /// Holds a parameter expansion or a substitution.
    pub(super) dynamic: bool,
    /// Every part of it is data in bash, zsh and dash alike: single-quoted
    /// text, double-quoted text without `$`, a backquote or a backslash, or
    /// unquoted characters none of them expands (letters, digits and
    /// `_ . / , : + @ % -`, and `=` after the first character).
    pub(super) data: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Command {
    /// The command's name and arguments, without leading assignments and
    /// redirections. A declaration (`export`, `local`, ...) or `unset`
    /// starts with its keyword; a test (`[[ ... ]]`) is only its keyword.
    pub(super) words: Vec<Word>,
    /// It runs inside `$(...)`, backquotes or `<(...)`/`>(...)`.
    pub(super) substituted: bool,
}

#[derive(Debug, Default)]
pub(super) struct Reading {
    /// In the order they start in the text.
    pub(super) commands: Vec<Command>,
    /// Text zsh may read differently from bash: zsh-only reserved words and
    /// wrappers (`repeat`, `noglob`, `builtin`), expansion flags, subscripts
    /// or modifiers after an expansion, an argument `}`, or a command name
    /// that is not data.
    pub(super) zsh_unsure: bool,
    /// Text dash (an `sh` on Debian and Ubuntu) reads differently from
    /// bash: `[[`, `((` (two subshells to dash), process substitution,
    /// here-strings, `$'...'`, arrays, `&>`, `|&`, `function`, `select`.
    pub(super) bash_only: bool,
    /// Progress checks the parse used (see [`parse_budget`]).
    pub(super) parse_checks: usize,
}

/// Reserved words and precommand modifiers zsh has and bash does not.
const ZSH_WORDS: &[&str] = &[
    "repeat",
    "foreach",
    "end",
    "always",
    "nocorrect",
    "noglob",
    "-",
];

/// Commands after which a shell may read the rest of the text another way
/// (aliases, parser options, builtins switched): refused wherever their
/// name is a word, so `builtin alias` and `command shopt` count too.
const REPARSING: &[&str] = &[
    "alias", "unalias", "setopt", "unsetopt", "emulate", "shopt", "enable", "disable", "zmodload",
    "autoload",
];

/// Commands that set a variable they name.
const DECLARING: &[&str] = &[
    "export",
    "declare",
    "typeset",
    "local",
    "readonly",
    "read",
    "printf",
    "mapfile",
    "readarray",
    "unset",
    "let",
    "getopts",
    "print",
];

/// Variables that change how this or a later shell reads text, or which
/// command a name runs (`PS4` runs under `set -x`). Named anywhere in the
/// text, the reading is refused (`declare -n p=POSIXLY_CORRECT`, `:
/// ${POSIXLY_CORRECT:=1}`).
const OPTION_VARIABLES: &[&str] = &[
    "SHELLOPTS",
    "BASHOPTS",
    "POSIXLY_CORRECT",
    "BASH_ALIASES",
    "BASH_CMDS",
    "BASH_ENV",
    "PS4",
];

/// Names zsh reads differently through a wrapper (`builtin setopt`) or
/// options `set` changes.
const ZSH_REPARSING: &[&str] = &["set", "builtin", "command"];

/// Reserved words the grammar takes as a command's name where the shells
/// do not (`done`, `fi`), or whose commands it does not see (`coproc cmd`
/// runs `cmd`; the grammar reads a command named `coproc`).
const RESERVED: &[&str] = &[
    "!", "{", "}", "[[", "]]", "if", "then", "else", "elif", "fi", "do", "done", "while", "until",
    "for", "in", "case", "esac", "select", "function", "coproc",
];

/// Read `text`, or say why not.
pub(super) fn read(text: &str) -> Result<Reading, Refusal> {
    if text.len() > MAX_BYTES {
        return Err(Refusal::TooLarge);
    }
    if !characters_agree(text) {
        return Err(Refusal::Characters);
    }
    // bash joins the lines around a backslash-newline before splitting
    // words; the grammar ends a word there (`1i\<newline>x` is one word).
    if text.contains("\\\n") {
        return Err(Refusal::LineContinuation);
    }
    if !here_documents_fit(text) {
        return Err(Refusal::HereDocuments);
    }
    if OPTION_VARIABLES.iter().any(|name| {
        work::step();
        text.contains(name)
    }) {
        return Err(Refusal::Reparse);
    }
    let (tree, parse_checks) = try_parse_shell_bounded(text, parse_budget(text.len()));
    let tree = tree.map_err(|stop| match stop {
        ParseStop::SyntaxError => Refusal::Syntax,
        ParseStop::Budget => Refusal::ParseBudget,
    })?;
    let root = tree.root_node();
    if root.has_error() {
        return Err(Refusal::Syntax);
    }
    let mut walk = Walk {
        text,
        zsh_unsure: false,
        bash_only: false,
        refusal: None,
        leaves: Vec::new(),
    };
    let mut commands = Vec::new();
    let mut stack = vec![(root, Place::default())];
    let mut children = Vec::new();
    while let Some((node, place)) = stack.pop() {
        work::step();
        let Place {
            depth,
            substituted,
            parent,
            expanding,
            in_test,
            literal_quotes,
        } = place;
        if depth > MAX_DEPTH {
            return Err(Refusal::TooDeep);
        }
        walk.node(node, parent);
        if matches!(
            node.kind(),
            "command" | "declaration_command" | "unset_command" | "test_command"
        ) {
            commands.push(walk.command(node, substituted));
        }
        if let Some(refusal) = walk.refusal {
            return Err(refusal);
        }
        if node.child_count() == 0 {
            walk.leaf(node, expanding, in_test, literal_quotes);
        }
        let inner =
            substituted || matches!(node.kind(), "command_substitution" | "process_substitution");
        let fresh = matches!(node.kind(), "command_substitution" | "process_substitution");
        let expanding = !fresh
            && (expanding
                || matches!(
                    node.kind(),
                    "string" | "heredoc_body" | "arithmetic_expansion" | "expansion" | "subscript"
                )
                // A `(( ... ))` statement (a compound statement to the
                // grammar).
                || (node.kind() == "compound_statement" && walk.source(node).starts_with("((")));
        let in_test = !fresh && (in_test || node.kind() == "test_command");
        let literal_quotes = !fresh
            && (literal_quotes
                || matches!(
                    node.kind(),
                    "string" | "heredoc_body" | "arithmetic_expansion"
                )
                || (node.kind() == "compound_statement" && walk.source(node).starts_with("((")));
        let mut cursor = node.walk();
        children.clear();
        children.extend(node.children(&mut cursor));
        for child in children.iter().rev() {
            work::step();
            // A C-style `for`'s header is arithmetic; its body is not.
            let header = node.kind() == "c_style_for_statement"
                && !matches!(child.kind(), "do_group" | "compound_statement");
            stack.push((
                *child,
                Place {
                    depth: depth + 1,
                    substituted: inner,
                    parent: node.kind(),
                    expanding: expanding || header,
                    in_test,
                    literal_quotes: literal_quotes || header,
                },
            ));
        }
    }
    // Every character the grammar did not take into a token must be a
    // blank: one it skipped (`\ ` before a word) may be part of a word to
    // the shells.
    walk.leaves.sort_unstable();
    let mut covered = 0;
    for (start, end) in walk.leaves {
        work::step();
        if start > covered
            && !text[covered..start]
                .chars()
                .all(|ch| matches!(ch, ' ' | '\t' | '\n'))
        {
            return Err(Refusal::Words);
        }
        covered = covered.max(end);
    }
    if !text[covered..]
        .chars()
        .all(|ch| matches!(ch, ' ' | '\t' | '\n'))
    {
        return Err(Refusal::Words);
    }
    Ok(Reading {
        commands,
        zsh_unsure: walk.zsh_unsure,
        bash_only: walk.bash_only,
        parse_checks,
    })
}

fn characters_agree(text: &str) -> bool {
    let mut previous_non_ascii = false;
    for ch in text.chars() {
        work::step();
        if (ch.is_control() && !matches!(ch, '\n' | '\t'))
            || (previous_non_ascii && ch.is_ascii_punctuation())
        {
            return false;
        }
        previous_non_ascii = !ch.is_ascii();
    }
    true
}

/// At most [`MAX_HERE_DOCUMENTS`] `<<` (here-strings counted too), each
/// followed by a word of at most [`MAX_DELIMITER_BYTES`]. A quoted
/// delimiter is measured only to its first blank; `delimiter` refuses
/// any with a blank.
fn here_documents_fit(text: &str) -> bool {
    let mut count = 0;
    let mut rest = text;
    while let Some(at) = rest.find("<<") {
        work::step();
        count += 1;
        rest = rest[at + 2..].trim_start_matches(['<', '-', ' ', '\t']);
        let delimiter = rest
            .find(|ch: char| ch.is_whitespace() || ";&|<>()".contains(ch))
            .unwrap_or(rest.len());
        if count > MAX_HERE_DOCUMENTS || delimiter > MAX_DELIMITER_BYTES {
            return false;
        }
    }
    true
}

fn is_redirect(node: Node) -> bool {
    matches!(
        node.kind(),
        "file_redirect" | "herestring_redirect" | "heredoc_redirect"
    )
}

/// Where a node is.
#[derive(Clone, Copy, Default)]
struct Place {
    depth: usize,
    /// In a substitution.
    substituted: bool,
    /// The parent node's kind.
    parent: &'static str,
    /// Where the shells take quotes as ordinary characters and still expand
    /// (double quotes, a here-document body, arithmetic, `${...}`).
    expanding: bool,
    /// In a test (`[[ ... ]]`, `[ ... ]`, `(( ... ))`), where a pattern or
    /// expression may hold blanks.
    in_test: bool,
    /// Where zsh or dash take quotes as ordinary characters even for where
    /// the text ends (double quotes, a here-document body, arithmetic):
    /// `"${x:-'}"` ends at the quoted `}` to them.
    literal_quotes: bool,
}

struct Walk<'a> {
    text: &'a str,
    zsh_unsure: bool,
    bash_only: bool,
    /// The first reason found not to trust the reading.
    refusal: Option<Refusal>,
    /// The byte ranges of the tokens.
    leaves: Vec<(usize, usize)>,
}

impl<'a> Walk<'a> {
    fn refuse(&mut self, refusal: Refusal) {
        self.refusal.get_or_insert(refusal);
    }

    fn source(&self, node: Node) -> &'a str {
        &self.text[node.byte_range()]
    }

    /// A token: its range, and that it hides no expansion unless its text
    /// is taken literally (quoted, a comment, a here-document's).
    fn leaf(&mut self, node: Node, expanding: bool, in_test: bool, literal_quotes: bool) {
        self.leaves.push((node.start_byte(), node.end_byte()));
        if literal_quotes && matches!(node.kind(), "raw_string" | "ansi_c_string") {
            self.refuse(Refusal::Words);
        }
        // One token for text the shells split: blanks or operators outside
        // quotes, a body, a comment or `${...}` (`echo == x; rm y` makes
        // `x; rm y` one pattern token to the grammar). In a test a pattern
        // may hold blanks and `|` only inside parentheses outside quotes
        // (bash's rule for `=~`); the grammar also counts quoted ones.
        if node.is_named()
            && !expanding
            && !matches!(
                node.kind(),
                "raw_string"
                    | "ansi_c_string"
                    | "string_content"
                    | "comment"
                    | "heredoc_body"
                    | "heredoc_content"
                    | "heredoc_start"
                    | "heredoc_end"
            )
            && if in_test {
                splits_in_test(self.source(node))
            } else {
                splits(self.source(node), " \t\n;&|<>()")
            }
        {
            self.refuse(Refusal::Words);
        }
        // Quotes inside double quotes, a body, arithmetic or `${...}` are
        // ordinary characters: `"${x:-'$(cmd)'}"` runs `cmd`.
        let quoted = matches!(node.kind(), "raw_string" | "ansi_c_string");
        if node.is_named()
            && !(quoted && !expanding)
            && !matches!(
                node.kind(),
                "comment" | "heredoc_body" | "heredoc_content" | "heredoc_start" | "heredoc_end"
            )
            && hides_expansion(self.source(node))
        {
            self.refuse(Refusal::Words);
        }
    }

    /// A here-document must end where the shells end it: the grammar's
    /// scanner ends it at a line that only starts with the delimiter, keeps
    /// quotes inside the delimiter, skips any blanks before it after `<<-`
    /// and does not look for backquotes in the body.
    fn here_document(&mut self, node: Node) {
        let mut cursor = node.walk();
        let mut strip = false;
        let mut start = None;
        let mut body = None;
        let mut end = None;
        for child in node.children(&mut cursor) {
            work::step();
            match child.kind() {
                "<<-" => strip = true,
                "heredoc_start" => start = Some(child),
                "heredoc_body" => body = Some(child),
                "heredoc_end" => end = Some(child),
                _ => {}
            }
        }
        let (Some(start), Some(end)) = (start, end) else {
            self.refuse(Refusal::HereDocuments);
            return;
        };
        let Some((delimiter, quoted)) = delimiter(self.source(start)) else {
            self.refuse(Refusal::HereDocuments);
            return;
        };
        let text = self.text;
        // The body starts on the line after the operator's (one
        // here-document per line) and ends at the first line that is the
        // delimiter (after leading tabs for `<<-`).
        let end_line = work::rfind(&text[..end.start_byte()], "\n").map_or(0, |at| at + 1);
        let body_start = body.map_or(end_line, |body| body.start_byte());
        let (Some(between), Some(body_text)) = (
            text.get(start.end_byte()..body_start),
            text.get(body_start..end_line),
        ) else {
            // The end is not on a line after the body.
            self.refuse(Refusal::HereDocuments);
            return;
        };
        // `<<-` strips the tabs at the start of the body's first line too.
        let between = if strip {
            between.trim_end_matches('\t')
        } else {
            between
        };
        let lines: Vec<&str> = body_text.split('\n').collect();
        let is_end = |line: &str| {
            let line = if strip {
                line.trim_start_matches('\t')
            } else {
                line
            };
            line == delimiter
        };
        if self.source(end) != delimiter
            || !(between.ends_with('\n') && between.matches('\n').count() == 1)
            || !text.get(end_line..end.end_byte()).is_some_and(is_end)
            || !text[end.end_byte()..].is_empty() && !text[end.end_byte()..].starts_with('\n')
            || lines.iter().any(|line| {
                work::step();
                is_end(line)
            })
        {
            self.refuse(Refusal::HereDocuments);
        }
        // Quoted: no expansions. Unquoted: no backquote or backslash, which
        // the grammar does not follow in a body.
        let Some(body) = body else { return };
        // The body's text is a token even where the grammar made no node
        // of it (around its expansions).
        self.leaves.push((body.start_byte(), body.end_byte()));
        // Its literal text: the gaps between its expansions, and its
        // content nodes.
        let mut literal_text = Vec::new();
        let mut at = body.start_byte();
        let mut cursor = body.walk();
        for part in body.named_children(&mut cursor) {
            work::step();
            literal_text.push(text.get(at..part.start_byte()).unwrap_or_default());
            if part.kind() == "heredoc_content" {
                literal_text.push(self.source(part));
            } else if quoted {
                // Quoted: nothing in it expands.
                self.refuse(Refusal::HereDocuments);
            }
            at = at.max(part.end_byte());
        }
        literal_text.push(text.get(at..body.end_byte()).unwrap_or_default());
        // Unquoted: no backquote or backslash, which the grammar does not
        // follow in a body, and no expansion it left in the text.
        if !quoted
            && literal_text.iter().any(|literal| {
                work::step();
                literal.contains(['`', '\\']) || hides_expansion(literal)
            })
        {
            self.refuse(Refusal::HereDocuments);
        }
    }

    /// Checks that hold for every node.
    fn node(&mut self, node: Node, parent: &str) {
        let kind = node.kind();
        let source = self.source(node);
        // The shells end a backquote substitution at the first backquote,
        // in quotes or a comment too; the grammar reads quotes and
        // comments inside it.
        if kind == "command_substitution"
            && let Some(inner) = source
                .strip_prefix('`')
                .and_then(|rest| rest.strip_suffix('`'))
            && inner.contains(['\'', '"', '#', '`', '\\'])
        {
            self.refuse(Refusal::Words);
        }
        // zsh and dash end arithmetic at `))` inside quotes too.
        let arithmetic = match kind {
            "arithmetic_expansion" => Some(source),
            "compound_statement" if source.starts_with("((") => Some(source),
            // The header: up to the loop's body.
            "c_style_for_statement" => {
                let mut cursor = node.walk();
                let body = node
                    .children(&mut cursor)
                    .find(|child| matches!(child.kind(), "do_group" | "compound_statement"));
                Some(
                    &source
                        [..body.map_or(source.len(), |body| body.start_byte() - node.start_byte())],
                )
            }
            _ => None,
        };
        // An assignment in arithmetic to a name built by an expansion
        // (`$(( ${x}CT = 1 ))`) may set any variable.
        if arithmetic
            .is_some_and(|text| text.contains(['\'', '"']) || (text.contains('$') && assigns(text)))
        {
            self.refuse(Refusal::Words);
        }
        self.bash_only |= matches!(
            kind,
            "process_substitution"
                | "herestring_redirect"
                | "ansi_c_string"
                | "array"
                | "c_style_for_statement"
                | "&>"
                | "&>>"
                | "|&"
                | "function"
                | "select"
        ) || ((kind == "test_command" || kind == "compound_statement")
            && (source.starts_with("[[") || source.starts_with("((")));
        // zsh's special arrays: `aliases[ls]=...`, `options[x]=on`.
        self.zsh_unsure |= kind == "variable_assignment"
            && [
                "aliases",
                "galiases",
                "saliases",
                "options",
                "functions",
                "builtins",
            ]
            .iter()
            .any(|name| source.starts_with(name));
        // The grammar lets a redirection take every word after it
        // (`rm >/dev/null -rf x` runs `rm -rf x`), and words after a
        // here-document's delimiter: refused.
        if matches!(kind, "file_redirect" | "herestring_redirect") {
            let mut cursor = node.walk();
            let mut targets = 0;
            let mut closes = false;
            for child in node.children(&mut cursor) {
                work::step();
                if !child.is_named() {
                    // `>&-` closes the descriptor and takes no word.
                    closes |= matches!(self.source(child), ">&-" | "<&-");
                } else if child.kind() != "file_descriptor" {
                    targets += 1;
                }
            }
            if targets > 1 || (closes && targets > 0) {
                self.refuse(Refusal::Words);
            }
        }
        if kind == "heredoc_redirect" {
            let mut cursor = node.walk();
            if cursor.goto_first_child() {
                loop {
                    work::step();
                    if cursor.field_name() == Some("argument") {
                        self.refuse(Refusal::Words);
                    }
                    if !cursor.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
        // `{fd}>f` names a variable to hold the descriptor; the grammar
        // takes `{fd}` for an argument.
        if is_redirect(node) && self.text[..node.start_byte()].ends_with('}') {
            self.refuse(Refusal::Words);
        }
        if kind == "heredoc_redirect" {
            self.here_document(node);
        }
        if node.is_error() || node.is_missing() {
            self.refuse(Refusal::Syntax);
        }
        if kind == "extglob_pattern" {
            self.refuse(Refusal::Unsupported);
        }
        // The grammar takes `f(x)` as a command with a subshell, and any
        // command as a function's body; to bash both are syntax errors.
        if kind == "subshell" && parent == "command" {
            self.refuse(Refusal::Syntax);
        }
        if kind == "variable_assignment"
            && node
                .child_by_field_name("name")
                .is_some_and(|name| option_variable(self.source(name)))
        {
            self.refuse(Refusal::Reparse);
        }
        // `{echo,hi}` is a brace expansion to the shells, a block to the
        // grammar.
        if kind == "compound_statement"
            && self
                .source(node)
                .strip_prefix('{')
                .is_some_and(|rest| !rest.starts_with(char::is_whitespace))
        {
            self.refuse(Refusal::Words);
        }
        // `[ a > b ]` redirects to `b`; the grammar reads a comparison.
        if kind == "test_command" {
            let source = self.source(node);
            if source.starts_with('[') && !source.starts_with("[[") && source.contains(['<', '>']) {
                self.refuse(Refusal::Words);
            }
        }
        // `--x=1 cmd` runs `--x=1`; the grammar takes it for an assignment.
        if kind == "variable_assignment"
            && node.child_by_field_name("name").is_none_or(|name| {
                let name = self.source(name);
                let name = name.split('[').next().unwrap_or_default();
                !name.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '_')
                    || !name
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            })
        {
            self.refuse(Refusal::Words);
        }
        // `[v]` is a word (a glob) to the shells, a test to the grammar.
        if kind == "test_command"
            && node.child(0).is_some_and(|open| {
                !self.text[open.end_byte()..].starts_with(|ch: char| ch.is_whitespace())
            })
        {
            self.refuse(Refusal::Words);
        }
        if kind == "function_definition"
            && node.child_by_field_name("body").is_none_or(|body| {
                !matches!(
                    body.kind(),
                    "compound_statement"
                        | "subshell"
                        | "if_statement"
                        | "while_statement"
                        | "for_statement"
                        | "c_style_for_statement"
                        | "case_statement"
                )
            })
        {
            self.refuse(Refusal::Syntax);
        }
        // `;;` outside a `case` is a syntax error to the shells.
        if matches!(kind, ";;" | ";&" | ";;&") && parent != "case_item" {
            self.refuse(Refusal::Syntax);
        }
        // A redirection's word on another line (`<<<` then a newline) is a
        // syntax error to the shells.
        if is_redirect(node) {
            let mut cursor = node.walk();
            let mut operator_end = None;
            for child in node.children(&mut cursor) {
                work::step();
                if let Some(end) = operator_end {
                    if self
                        .text
                        .get(end..child.start_byte())
                        .is_none_or(|gap| gap.contains('\n'))
                    {
                        self.refuse(Refusal::Syntax);
                    }
                    break;
                }
                if !child.is_named() {
                    operator_end = Some(child.end_byte());
                }
            }
        }
        // `#` starts a comment only at the start of a word.
        if kind == "comment"
            && self.text[..node.start_byte()]
                .chars()
                .next_back()
                .is_some_and(|before| !before.is_whitespace() && !";&|()<>".contains(before))
        {
            self.refuse(Refusal::Words);
        }
        // zsh expansion flags (`${(e)x}`, `${=x}`, `${~x}`, `${^x}`).
        self.zsh_unsure |= kind == "expansion"
            && ["${(", "${=", "${~", "${^"]
                .iter()
                .any(|flag| source.starts_with(flag));
    }

    fn command(&mut self, node: Node, substituted: bool) -> Command {
        let kind = node.kind();
        let mut words: Vec<Word> = Vec::new();
        let mut cursor = node.walk();
        // Where the last part ended, and whether it was a word kept.
        let mut last_end: Option<(usize, bool)> = None;
        for child in node.children(&mut cursor) {
            work::step();
            let redirect = is_redirect(child);
            // Nothing between two parts: one word to the shells.
            let joined = !redirect && last_end.is_some_and(|(end, _)| end == child.start_byte());
            if joined && last_end.is_some_and(|(_, word)| !word) {
                // `>f"x"`: the shells write to `fx`.
                self.refuse(Refusal::Words);
            }
            let kept = !redirect && !(child.kind() == "variable_assignment" && kind == "command");
            last_end = Some((child.end_byte(), kept));
            let word = match child.kind() {
                _ if redirect => continue,
                "variable_assignment" if kind == "command" => continue,
                "command_name" => {
                    if RESERVED.contains(&self.source(child)) {
                        self.refuse(Refusal::Words);
                    }
                    match child.named_child(0) {
                        Some(name) => self.word(name),
                        None => continue,
                    }
                }
                _ if kind == "test_command" => {
                    words.push(literal(self.source(child)));
                    break;
                }
                // A declaration's keyword, or an operator argument (`==`).
                _ if !child.is_named() => self.unquoted(self.source(child)),
                _ => self.word(child),
            };
            match words.last_mut() {
                Some(last) if joined => {
                    last.text.push_str(&word.text);
                    last.dynamic |= word.dynamic;
                    last.data &= word.data;
                }
                _ => words.push(word),
            }
        }
        if kind == "command"
            && let Some(name) = words.first()
        {
            // A name spelled through an expansion may be any command,
            // one that changes how the lines after it parse included.
            if name.dynamic {
                self.refuse(Refusal::Reparse);
            }
            // A name zsh reads as a reserved word or a wrapper, or a
            // command whose options change how zsh reads later lines.
            self.zsh_unsure |= !name.data
                || ZSH_WORDS.contains(&name.text.as_str())
                || ZSH_REPARSING.contains(&name.text.as_str());
        }
        if words
            .iter()
            .any(|word| REPARSING.contains(&word.text.as_str()))
        {
            self.refuse(Refusal::Reparse);
        }
        // `set -k` makes later `name=value` arguments assignments, `set -o
        // posix` turns on aliases: only harmless options are read.
        // The first word after wrappers and their options (`command -p`,
        // `builtin --`, `time -p`).
        let mut after_wrapper = false;
        let wrapped = words.iter().position(|word| {
            let wrapper = matches!(
                word.text.as_str(),
                "builtin" | "command" | "noglob" | "nocorrect" | "-" | "time"
            );
            let option = after_wrapper && word.text.starts_with('-');
            after_wrapper |= wrapper;
            !wrapper && !option
        });
        // A nameref (`declare -n p=...`) makes a name stand for another.
        if let Some(at) = wrapped
            && matches!(words[at].text.as_str(), "declare" | "typeset" | "local")
            && words[at + 1..]
                .iter()
                .any(|word| word.text.starts_with('-') && word.text.contains('n'))
        {
            self.refuse(Refusal::Reparse);
        }
        // A variable named through an expansion (`export "$v=1"`, `read
        // "$v"`, `declare -n`) may be one that changes how lines are read.
        if let Some(at) = wrapped
            && DECLARING.contains(&words[at].text.as_str())
            && (!matches!(words[at].text.as_str(), "printf" | "print")
                || words[at + 1..]
                    .iter()
                    .any(|word| word.text.starts_with("-v")))
            && words[at + 1..].iter().any(|word| {
                // The name: before `=`, or the whole word.
                let name = word.text.split('=').next().unwrap_or_default();
                word.dynamic && (!word.text.contains('=') || name.contains(['$', '\u{1}', '`']))
            })
        {
            self.refuse(Refusal::Reparse);
        }
        // A name spelled through an expansion behind a wrapper
        // (`command $c -s expand_aliases`).
        if kind == "command" && wrapped.is_some_and(|at| at > 0 && words[at].dynamic) {
            self.refuse(Refusal::Reparse);
        }
        if kind == "command"
            && let Some(at) = wrapped
            && words[at].text == "set"
            && !harmless_set(&words[at + 1..])
        {
            self.refuse(Refusal::Reparse);
        }
        // Variables that set the options of shells started from this line.
        if words.iter().any(|word| option_variable(&word.text)) {
            self.refuse(Refusal::Reparse);
        }
        self.zsh_unsure |= words
            .iter()
            .skip(1)
            .any(|word| word.text == "}" && !word.dynamic);
        Command { words, substituted }
    }

    /// A word, from its syntax node.
    fn word(&mut self, node: Node) -> Word {
        let source = self.source(node);
        match node.kind() {
            "word" => self.unquoted(source),
            "number" => Word {
                text: source.to_string(),
                dynamic: false,
                data: source.bytes().all(|byte| byte.is_ascii_digit()),
            },
            "raw_string" => {
                match source
                    .strip_prefix('\'')
                    .and_then(|rest| rest.strip_suffix('\''))
                {
                    Some(inner) => literal(inner),
                    None => unknown(source),
                }
            }
            "string" => self.double_quoted(node),
            "ansi_c_string" => {
                // dash before 0.5.13 reads `$'a'` as `$a`: not data.
                let chars: Vec<char> = source.chars().skip(2).collect();
                let mut decoded = String::new();
                ansi_c(&chars, /*index*/ 0, &mut decoded);
                Word {
                    text: decoded,
                    dynamic: false,
                    data: false,
                }
            }
            "concatenation" => {
                let mut joined = literal("");
                let mut cursor = node.walk();
                let mut after_expansion = false;
                for part in node.children(&mut cursor) {
                    work::step();
                    let part_source = self.source(part);
                    // zsh subscripts and modifiers: `$x[1]`, `$x:h`.
                    self.zsh_unsure |= after_expansion && part_source.starts_with(['[', ':']);
                    after_expansion = part.kind() == "simple_expansion";
                    let part = if part.is_named() {
                        self.word(part)
                    } else {
                        self.unquoted(part_source)
                    };
                    joined.text.push_str(&part.text);
                    joined.dynamic |= part.dynamic;
                    joined.data &= part.data;
                }
                // Braces across parts may still expand (`{a,"b"}`).
                joined.dynamic |= source.contains('{') && source.contains('}');
                joined
            }
            "command_substitution" | "process_substitution" => Word {
                text: UNSEEN_OUTPUT.to_string(),
                dynamic: true,
                data: false,
            },
            "simple_expansion" | "expansion" | "arithmetic_expansion" | "translated_string" => {
                expanded()
            }
            // An assignment's name stays readable (`export l=$(m)`).
            "variable_assignment" => match node.child_by_field_name("name") {
                Some(name) if source.contains(['$', '`', '(', '{']) => Word {
                    text: format!("{}={EXPANDED}", self.source(name)),
                    dynamic: true,
                    data: false,
                },
                _ => unknown(source),
            },
            // Brace expressions, arrays, regular expressions.
            _ => unknown(source),
        }
    }

    /// An unquoted word: backslashes removed. Data only without a
    /// backslash and with characters no shell expands. Unescaped blanks or
    /// operators in it mean the grammar joined what the shells split; a
    /// parenthesis is a syntax error to bash and a glob qualifier to zsh.
    fn unquoted(&mut self, source: &str) -> Word {
        let mut text = String::with_capacity(source.len());
        let mut chars = source.chars();
        while let Some(ch) = chars.next() {
            work::step();
            match ch {
                '\\' => text.extend(chars.next()),
                ch if ch.is_whitespace() || ";&|<>()".contains(ch) => {
                    self.refuse(Refusal::Words);
                    text.push(ch);
                }
                ch => text.push(ch),
            }
        }
        let data = !source.is_empty()
            && !source.starts_with('=')
            && source.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(
                        byte,
                        b'_' | b'.' | b'/' | b',' | b':' | b'+' | b'@' | b'%' | b'-' | b'='
                    )
            });
        Word {
            text,
            // A `$` the grammar left in a word (`$a$b` may come as `$a`,
            // `$`, `b`) may still expand, and braces may expand into
            // several words (`{alias,ls=x}`).
            dynamic: source.contains('$') || (source.contains('{') && source.contains('}')),
            data,
        }
    }

    /// A `"..."` string: data when it holds no `$`, backquote or backslash.
    fn double_quoted(&mut self, node: Node) -> Word {
        let source = self.source(node);
        let Some(inner) = source
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
        else {
            return unknown(source);
        };
        if !inner.contains(['$', '`', '\\']) {
            return literal(inner);
        }
        let mut approximate = String::new();
        let mut cursor = node.walk();
        for part in node.named_children(&mut cursor) {
            work::step();
            match part.kind() {
                "string_content" => approximate.push_str(self.source(part)),
                "command_substitution" => approximate.push_str(UNSEEN_OUTPUT),
                _ => approximate.push_str(EXPANDED),
            }
        }
        Word {
            text: approximate,
            dynamic: inner.contains(['$', '`']),
            data: false,
        }
    }
}

fn literal(text: &str) -> Word {
    Word {
        text: text.to_string(),
        dynamic: false,
        data: true,
    }
}

/// Text that is not data: kept as written, or [`EXPANDED`] when it may
/// hold an expansion or a substitution (copying nested ones whole at every
/// level would take work quadratic in their depth).
fn unknown(source: &str) -> Word {
    if source.contains(['$', '`', '(', '{']) {
        return expanded();
    }
    Word {
        text: source.to_string(),
        dynamic: false,
        data: false,
    }
}

/// Whether unquoted text holds one of `separators` not escaped with a
/// backslash.
fn splits(text: &str, separators: &str) -> bool {
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        work::step();
        if ch == '\\' {
            chars.next();
        } else if separators.contains(ch) {
            return true;
        }
    }
    false
}

/// Whether a test's token holds, outside quotes and backslash escapes, a
/// `;&<>` or `]]`, or a blank or `|` outside parentheses.
fn splits_in_test(text: &str) -> bool {
    let mut chars = text.chars().peekable();
    let mut quote = None;
    let mut depth = 0usize;
    while let Some(ch) = chars.next() {
        work::step();
        match (quote, ch) {
            (Some(open), ch) if ch == open => quote = None,
            (Some('"'), '\\') => {
                chars.next();
            }
            (Some(_), _) => {}
            (None, '\\') => {
                chars.next();
            }
            (None, '\'' | '"') => quote = Some(ch),
            (None, '(') => depth += 1,
            (None, ')') => depth = depth.saturating_sub(1),
            (None, ';' | '&' | '<' | '>') => return true,
            (None, ']') if chars.peek() == Some(&']') => return true,
            (None, ch) if depth == 0 && (ch.is_whitespace() || ch == '|') => return true,
            _ => {}
        }
    }
    false
}

/// `set` arguments that change nothing about how the lines after them are
/// read: `-e`, `-u`, `-v` (and `+`), `-o` with one of these names,
/// and `--` with any positional parameters.
fn harmless_set(args: &[Word]) -> bool {
    let mut option_name = false;
    for arg in args {
        work::step();
        let text = arg.text.as_str();
        if !arg.data {
            return false;
        }
        if std::mem::take(&mut option_name) {
            if !matches!(text, "errexit" | "nounset" | "verbose" | "pipefail") {
                return false;
            }
            continue;
        }
        if text == "--" {
            return true;
        }
        let Some(flags) = text.strip_prefix(['-', '+']) else {
            return false;
        };
        if flags.is_empty() || !flags.chars().all(|flag| "euvo".contains(flag)) {
            return false;
        }
        option_name = flags.ends_with('o');
    }
    !option_name
}

/// Whether `word` names (or assigns) a variable that changes how this or a
/// later shell reads text or which command a name runs: `SHELLOPTS`,
/// `BASHOPTS`, `POSIXLY_CORRECT` (posix mode turns on aliases),
/// `BASH_ALIASES`, `BASH_CMDS`.
fn option_variable(word: &str) -> bool {
    // Anywhere in the word as the command receives it (quotes removed):
    // `declare -n q=POSIX"LY_CORRECT"` names it too.
    OPTION_VARIABLES.iter().any(|name| word.contains(name))
}

/// Whether arithmetic text holds an assignment operator (`=`, `+=`, `<<=`, ...,
/// `++`, `--`), not a comparison (`==`, `!=`, `<=`, `>=`).
fn assigns(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.iter().enumerate().any(|(at, byte)| {
        work::step();
        let before = at.checked_sub(1).map(|before| bytes[before]);
        let after = bytes.get(at + 1).copied();
        // `<<=` and `>>=` assign; `<=` and `>=` compare.
        let shift = at >= 2 && matches!(&bytes[at - 2..at], b"<<" | b">>");
        (*byte == b'='
            && after != Some(b'=')
            && (shift || !matches!(before, Some(b'=' | b'!' | b'<' | b'>'))))
            || (*byte == b'+' && after == Some(b'+'))
            || (*byte == b'-' && after == Some(b'-'))
    })
}

/// The text of a word's expansion, whose value is not known.
const EXPANDED: &str = "\u{1}expansion\u{1}";

fn expanded() -> Word {
    Word {
        text: EXPANDED.to_string(),
        dynamic: true,
        data: false,
    }
}

/// Whether text the grammar took as a token holds an expansion or a
/// substitution it did not parse (`${x#$(cmd)}` is one token to it):
/// a backquote, or `$` before a name, a special parameter, a brace, a
/// bracket, a parenthesis or a quote.
fn hides_expansion(text: &str) -> bool {
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        work::step();
        if ch == '`'
            || (ch == '$'
                && chars
                    .peek()
                    .is_some_and(|next| next.is_alphanumeric() || "_({['\"@*#?$!-".contains(*next)))
        {
            return true;
        }
    }
    false
}

/// A here-document's delimiter word as written: plain characters, or the
/// same quoted whole (`'EOF'`, `"EOF"`), with whether it was quoted.
fn delimiter(word: &str) -> Option<(&str, bool)> {
    let plain = |text: &str| {
        !text.is_empty()
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
    };
    if plain(word) {
        return Some((word, false));
    }
    ["'", "\""].into_iter().find_map(|quote| {
        word.strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
            .filter(|inner| plain(inner))
            .map(|inner| (inner, true))
    })
}

#[cfg(test)]
#[path = "grammar_harness_tests.rs"]
mod harness_tests;

#[cfg(test)]
#[path = "grammar_tests.rs"]
mod tests;
