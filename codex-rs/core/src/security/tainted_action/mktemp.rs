//! D191 (owner-approved relaxation): the output of a plain `mktemp` is a
//! known path in the temp folder, so `tmp=$(mktemp); … > "$tmp"` and `trap
//! "rm -f $tmp" EXIT` after untrusted content no longer ask.
//!
//! Bounded on purpose. Only the action's own `sh`/`bash`/`zsh -c` script
//! qualifies, and only plain lowercase variables it sets first (after at
//! most `set -eu`), from `mktemp` with no template, `-t NAME`, or a
//! template directly in `/tmp` or `$TMPDIR` (`-d`, `-q`, `--suffix`, `-p` a
//! temp folder; not `-p ./x`, `-u`). Such a variable holds [`TEMP_FILE`] or
//! [`TEMP_DIR`], which every check still reads as unseen output. Only two
//! prompts stop: for the file itself written (`> "$tmp"`, `cp f "$tmp"`; not
//! `"$tmp.x"` or a folder's member, which a failed `mktemp` or an extracted
//! link may put elsewhere), and for a trap that only removes the path.
//! They are held back until the whole line has been read, and the line
//! keeps them when it may give the variable another value: it names the
//! variable other than as `$tmp` (`read tmp`, `tmp=x`, `${tmp:=x}`, `export
//! tmp`), uses a builtin that sets or evaluates variables (`read`, `printf
//! -v`, `declare`, `eval`, `source`), names a command at run time (`$c`),
//! defines a function, or assigns a variable it names at run time (`(( $n
//! = 1 ))`); and unless the bash grammar reads every command as named by
//! plain text (not `{r..r}ead`, `rea?`, `$(printf %s read)`) with
//! `printf`/`print`/`set` options spelled out (every word of zsh `print`).
//! An action that asks for anything else asks as on main, for the strongest
//! prompt.
//!
//! What is promised is what a literal temp path (`/tmp/x`) already gets:
//! the classifier does not follow a link that the line, or a program it
//! runs, puts at that path before writing it. As best-effort hardening
//! only, the line also keeps its prompts when it hands the file's path to a
//! command other than a few plain ones (`mv ./link "$tmp"`, `ln`, `./x
//! "$tmp"`), passes words from input (`xargs`, `parallel`), holds a
//! here-document, or spells the temp folder (`"$TMPDIR"/tmp.*`).

use super::ProtectedActionKind;
use super::grammar;
use super::indirect;
use super::shell::SimpleCommand;
use super::shell::UNSEEN_OUTPUT;
use super::work;
use std::collections::HashMap;

/// What a qualifying variable set from `mktemp` holds: unseen output (every
/// other check treats it as such) marked as a fresh temp file. Text never
/// holds `\u{1}`, and the placeholders only hold it around `unseen-output`,
/// so nothing but [`Scope::value`] makes this.
pub(super) const TEMP_FILE: &str = "\u{1}unseen-output\u{1}\u{1}mktemp\u{1}";
/// The same for `mktemp -d`.
pub(super) const TEMP_DIR: &str = "\u{1}unseen-output\u{1}\u{1}mktemp-d\u{1}";

/// Builtins that set variables they are given or evaluate text, and words
/// that change how the shell reads a line: a line using any of them keeps
/// its prompts.
const SETS_VARIABLES: &[&str] = &[
    "read",
    "mapfile",
    "readarray",
    "getopts",
    "declare",
    "typeset",
    "local",
    "export",
    "readonly",
    "integer",
    "float",
    "private",
    "unset",
    "let",
    "eval",
    "source",
    "alias",
    "unalias",
    "vared",
    "sysread",
    "zparseopts",
    "wait",
    "coproc",
    "select",
    "enable",
    "disable",
    "setopt",
    "unsetopt",
    "emulate",
    "shopt",
    "zmodload",
    "autoload",
    "bind",
    "complete",
    "compgen",
    "compctl",
    "zle",
    "fc",
    "getln",
    "zstyle",
    "zformat",
    "zregexparse",
    "pcre_match",
    "zstat",
    "strftime",
    "zselect",
    "ztcp",
    "zsocket",
    "zpty",
    "zsystem",
    "zcurses",
    "hash",
    "nameref",
    "sched",
    "zgetattr",
    "zlistattr",
    "zsetattr",
    "zdelattr",
    "ztie",
    "syserror",
    "sysopen",
];

/// Words before a command that leave it in this shell, and keywords.
const PREFIXES: &[&str] = &[
    "!",
    "if",
    "then",
    "elif",
    "else",
    "do",
    "while",
    "until",
    "time",
    "noglob",
    "nocorrect",
    "-",
    "builtin",
    "command",
    "exec",
    "{",
    "(",
];

/// Lowercase names zsh sets or reads itself (bash's are uppercase or `_`).
const SHELL_NAMES: &[&str] = &[
    "argv",
    "match",
    "mbegin",
    "mend",
    "reply",
    "path",
    "fpath",
    "cdpath",
    "manpath",
    "mailpath",
    "module_path",
    "psvar",
    "fignore",
    "watch",
    "histchars",
    "prompt",
    "pipestatus",
    "status",
    "signals",
    "funcstack",
    "functrace",
    "funcfiletrace",
    "funcsourcetrace",
    "zsh_eval_context",
    "zsh_scheduled_events",
    "options",
    "parameters",
    "commands",
    "aliases",
    "galiases",
    "saliases",
    "functions",
    "functions_source",
    "builtins",
    "reswords",
    "history",
    "historywords",
    "jobdirs",
    "jobstates",
    "jobtexts",
    "dirstack",
    "termcap",
    "terminfo",
    "userdirs",
    "usergroups",
    "nameddirs",
    "widgets",
    "modules",
    "keymaps",
    "patchars",
    "sysparams",
    "errnos",
    "killring",
    "region_highlight",
    "zle_highlight",
    "mapfile",
    "epochtime",
];

/// Signals and shell events a trap may name.
const SIGNALS: &[&str] = &[
    "EXIT", "ERR", "DEBUG", "RETURN", "ZERR", "HUP", "INT", "QUIT", "ILL", "TRAP", "ABRT", "IOT",
    "BUS", "FPE", "KILL", "USR1", "SEGV", "USR2", "PIPE", "ALRM", "TERM", "STKFLT", "CHLD", "CLD",
    "CONT", "STOP", "TSTP", "TTIN", "TTOU", "URG", "XCPU", "XFSZ", "VTALRM", "PROF", "WINCH", "IO",
    "POLL", "PWR", "SYS", "INFO", "EMT",
];

/// The variables of the line being read that hold a temp path, and the
/// prompts held back for them.
#[derive(Debug)]
pub(super) struct Scope {
    /// The variables, and whether each holds a folder.
    names: HashMap<String, bool>,
    held: Option<ProtectedActionKind>,
    /// The line may give one of `names` another value, or put a link in a
    /// temp file's place.
    spoiled: bool,
}

impl Scope {
    /// The scope of the action's own command line (`command`, lexed as
    /// `lexed`), if its script sets a qualifying variable first and no word
    /// of the line may set it again.
    pub(super) fn of(command: &[String], lexed: &[SimpleCommand]) -> Option<Self> {
        let (shell, script) = codex_shell_command::bash::extract_bash_command(command)?;
        let system_shell = !shell.contains('/')
            || [
                "/bin/",
                "/usr/bin/",
                "/usr/local/bin/",
                "/opt/homebrew/bin/",
            ]
            .iter()
            .any(|dir| {
                shell
                    .strip_prefix(dir)
                    .is_some_and(|rest| !rest.contains('/'))
            });
        // Text the word lexer may read differently from the shell.
        let plain = script
            .bytes()
            .all(|byte| byte == b'\n' || byte == b'\t' || (0x20..0x7f).contains(&byte))
            && !script.contains("\\\n")
            && !script.contains("$'")
            && !script.contains("$\"")
            // bash 5.3 and ksh function substitutions run in this shell.
            && !script.contains("${ ")
            && !script.contains("${\t")
            && !script.contains("${\n")
            && !script.contains("${|")
            // Word splitting, where commands are found, and functions
            // (`cp() { …; }`) change what a later word does.
            && !script.contains("IFS")
            && !script.contains("PATH")
            && !script.contains("function")
            && !defines_function(script);
        if !system_shell || !plain {
            return None;
        }
        let (definitions, rest) = leading_definitions(script);
        let names: HashMap<String, bool> = definitions
            .into_iter()
            .filter(|(name, _)| plain_variable(name))
            .map(|(name, folder)| (name.to_string(), folder))
            .collect();
        if names.is_empty() {
            return None;
        }
        let scope = Self {
            names,
            held: None,
            spoiled: false,
        };
        // The text after the definitions, as written: it names the variables
        // only as `$tmp` or `${tmp}` (`$(( tmp = 0 ))` lexes as a number) and
        // spells every trap in full. Best effort (see above): it does not
        // spell the temp folder (`mv ./link "$TMPDIR"/tmp.*`) or hold a
        // here-document, whose lines the lexer reads as commands.
        let traps = scope.raw_traps(rest)?;
        // The grammar is unsure how zsh reads `set`; the leading ones were
        // read above.
        if !commands_named_as_written(after_sets(script)) {
            return None;
        }
        if !scope.raw_names_ok(rest)
            || ["TMPDIR", "/tmp", "/var/", "/private/", "<<"]
                .iter()
                .any(|text| rest.contains(text))
        {
            return None;
        }
        let mut definitions = 0;
        let mut lexed_traps = 0;
        for simple in lexed {
            work::step();
            if simple.words.len() == 1 && scope.definition(&simple.words[0]) {
                definitions += 1;
                continue;
            }
            if !own_mktemp(simple) && !scope.allows(&simple.words, /*substituted*/ false) {
                return None;
            }
            lexed_traps += simple.words.iter().filter(|word| *word == "trap").count();
        }
        (definitions == scope.names.len() && lexed_traps == traps).then_some(scope)
    }

    /// What `name=value` stores: [`TEMP_FILE`] or [`TEMP_DIR`] for a
    /// qualifying variable's first value (its `mktemp` output), else
    /// nothing.
    pub(super) fn value(&self, name: &str, value: &str, first: bool) -> Option<&'static str> {
        let folder = self
            .names
            .get(name)
            .filter(|_| first && value == UNSEEN_OUTPUT)?;
        Some(if *folder { TEMP_DIR } else { TEMP_FILE })
    }

    /// Read a command's words as substituted (`$a$b` may spell a name):
    /// the line keeps its prompts when they may set a variable, or when the
    /// command may put a link in a temp file's place or pass its path on.
    pub(super) fn read(&mut self, simple: &SimpleCommand, words: &[String]) {
        if words.len() == 1 && self.definition(&words[0]) {
            return;
        }
        if (!own_mktemp(simple) && !self.allows(words, /*substituted*/ true))
            || !file_operands_ok(simple, words)
        {
            self.spoiled = true;
        }
    }

    /// Whether a trap's operands (as substituted) only remove temp paths.
    pub(super) fn removes_temp(&self, args: &[String]) -> bool {
        self.removes(args, /*substituted*/ true)
    }

    /// Whether a command (as substituted) is such a trap.
    pub(super) fn removal_trap(&self, words: &[String]) -> bool {
        words.first().is_some_and(|word| word == "trap") && self.removes_temp(&words[1..])
    }

    /// Hold back a prompt that only a temp path raised.
    pub(super) fn hold(&mut self, kind: ProtectedActionKind) {
        if self.held.is_none_or(|held| held.strongest(kind) != held) {
            self.held = Some(kind);
        }
    }

    /// The prompts held back, and whether the line keeps them now: when it
    /// may give a variable another value. `assigns_unseen_name`: arithmetic
    /// may set a variable named at run time.
    pub(super) fn finish(self, assigns_unseen_name: bool) -> (Option<ProtectedActionKind>, bool) {
        (self.held, self.spoiled || assigns_unseen_name)
    }

    /// `name=` and the unseen output of its `mktemp`.
    fn definition(&self, word: &str) -> bool {
        word.split_once('=')
            .is_some_and(|(name, value)| self.names.contains_key(name) && value == UNSEEN_OUTPUT)
    }

    /// Whether a command's words leave every qualifying variable alone.
    fn allows(&self, words: &[String], substituted: bool) -> bool {
        let mut at_command = true;
        let mut after_wrapper = false;
        let mut prints = false;
        for (index, word) in words.iter().enumerate() {
            work::step();
            // zsh `set -A name`; best effort: commands that turn input into
            // words (a printed path into `mv`'s operand).
            if SETS_VARIABLES.contains(&word.as_str())
                || matches!(word.as_str(), "xargs" | "parallel")
                || word
                    .strip_prefix(['-', '+'])
                    .is_some_and(|flags| flags.starts_with('A'))
                || !self.names_ok(word)
            {
                return false;
            }
            if word == "trap" && !self.removes(&words[index + 1..], substituted) {
                return false;
            }
            // `printf -v`, `print -v`, also as a pattern (`-?`).
            if prints
                && word
                    .strip_prefix('-')
                    .is_some_and(|flags| flags.contains(['v', '?', '*', '[', '{']))
            {
                return false;
            }
            prints |= matches!(word.as_str(), "printf" | "print");
            if at_command {
                let assignment = word
                    .split_once('=')
                    .is_some_and(|(name, _)| !name.is_empty() && name.bytes().all(is_name_byte));
                if assignment
                    || PREFIXES.contains(&word.as_str())
                    || (after_wrapper && word.starts_with('-'))
                {
                    after_wrapper |=
                        matches!(word.as_str(), "builtin" | "command" | "exec" | "time");
                    continue;
                }
                // `. file` and a command named at run time (`$c x`).
                if word == "."
                    || (!substituted && (word.contains(['$', '`']) || word.contains(UNSEEN_OUTPUT)))
                {
                    return false;
                }
                at_command = false;
            }
        }
        true
    }

    /// Whether every name in `word` that a qualifying variable has is only
    /// read (`$tmp`, `${tmp}`) or part of a path or option (`/tmp`, `.tmp`,
    /// `-tmp`), not set (`tmp=x`, `read tmp`, `${tmp:=x}`, `${!tmp}`).
    fn names_ok(&self, word: &str) -> bool {
        let text = word
            .replace(TEMP_FILE, " ")
            .replace(TEMP_DIR, " ")
            .replace(UNSEEN_OUTPUT, " ");
        let bytes = text.as_bytes();
        let mut start = 0;
        while start < bytes.len() {
            work::step();
            if !is_name_byte(bytes[start]) {
                start += 1;
                continue;
            }
            let end = bytes[start..]
                .iter()
                .position(|byte| !is_name_byte(*byte))
                .map_or(bytes.len(), |len| start + len);
            if self.names.contains_key(&text[start..end]) {
                let before = start.checked_sub(1).map(|at| bytes[at]);
                let after = bytes.get(end).copied();
                let read =
                    before == Some(b'$') && !matches!(after, Some(b'=' | b':' | b'[' | b'('));
                if !read && !matches!(before, Some(b'/' | b'.' | b'-')) {
                    return false;
                }
            }
            start = end;
        }
        true
    }

    /// Whether a trap's words (after `trap`) only remove qualifying
    /// variables' paths: `[--] rm [-f…] [--] "$tmp"… SIGNAL…`.
    fn removes(&self, words: &[String], substituted: bool) -> bool {
        let mut words = words.iter().map(String::as_str).peekable();
        words.next_if_eq(&"--");
        if words.next() != Some("rm") {
            return false;
        }
        while words.next_if(|word| rm_flags(word)).is_some() {
            work::step();
        }
        words.next_if_eq(&"--");
        let mut removed = 0;
        while words
            .next_if(|word| {
                if substituted {
                    *word == TEMP_FILE || *word == TEMP_DIR
                } else {
                    word.strip_prefix('$')
                        .is_some_and(|name| self.names.contains_key(name))
                }
            })
            .is_some()
        {
            work::step();
            removed += 1;
        }
        let mut signals = 0;
        for word in words {
            work::step();
            if !signal(word) {
                return false;
            }
            signals += 1;
        }
        removed > 0 && signals > 0
    }

    /// The traps of `text` (after the definitions), if every `trap` in it is
    /// spelled `trap [--] 'rm [-f…] [--] "$tmp"…' SIGNAL…` (or with double
    /// quotes and no inner ones): the quoted action and the signals stay
    /// apart, which the word lexer's reading does not show.
    fn raw_traps(&self, text: &str) -> Option<usize> {
        let mut traps = 0;
        for (at, _) in text.match_indices("trap") {
            work::step();
            // Not a command (`./bootstrap.sh`); a quoted or escaped `trap`
            // the lexer reads as one makes the counts differ.
            let starts = text[..at]
                .bytes()
                .next_back()
                .is_none_or(|byte| b" \t\n;&|".contains(&byte));
            if !starts {
                continue;
            }
            let rest = text[at + 4..].trim_start_matches([' ', '\t']);
            let rest = rest
                .strip_prefix("--")
                .map_or(rest, |rest| rest.trim_start_matches([' ', '\t']));
            let quote = rest
                .bytes()
                .next()
                .filter(|byte| matches!(byte, b'\'' | b'"'))?;
            let body = &rest[1..];
            let end = body.find(quote as char)?;
            if text[at + 4..].bytes().next() != Some(b' ') {
                return None;
            }
            let mut words = body[..end]
                .split([' ', '\t'])
                .filter(|word| !word.is_empty());
            if words.next() != Some("rm") {
                return None;
            }
            let mut removed = 0;
            for word in words {
                work::step();
                let inner = if quote == b'\'' {
                    word.strip_prefix('"')
                        .and_then(|word| word.strip_suffix('"'))
                        .unwrap_or(word)
                } else {
                    word
                };
                let name = inner
                    .strip_prefix("${")
                    .and_then(|name| name.strip_suffix('}'))
                    .or_else(|| inner.strip_prefix('$'));
                if name.is_some_and(|name| self.names.contains_key(name)) {
                    removed += 1;
                } else if removed > 0 || !(rm_flags(word) || word == "--") {
                    return None;
                }
            }
            let mut signals = 0;
            let mut after = body[end + 1..].split([' ', '\t']);
            for word in after.by_ref() {
                work::step();
                if word.is_empty() {
                    continue;
                }
                let (word, more) = match word.find([';', '\n', '&', '|']) {
                    Some(stop) => (&word[..stop], true),
                    None => (word, false),
                };
                if !word.is_empty() {
                    if !signal(word) {
                        return None;
                    }
                    signals += 1;
                }
                if more {
                    break;
                }
            }
            if removed == 0 || signals == 0 {
                return None;
            }
            traps += 1;
        }
        Some(traps)
    }

    /// Like [`Self::names_ok`] for the text as written, where `${tmp}`
    /// keeps its braces.
    fn raw_names_ok(&self, text: &str) -> bool {
        let bytes = text.as_bytes();
        let mut start = 0;
        while start < bytes.len() {
            work::step();
            if !is_name_byte(bytes[start]) {
                start += 1;
                continue;
            }
            let end = bytes[start..]
                .iter()
                .position(|byte| !is_name_byte(*byte))
                .map_or(bytes.len(), |len| start + len);
            if self.names.contains_key(&text[start..end]) {
                let before = start.checked_sub(1).map(|at| bytes[at]);
                let after = bytes.get(end).copied();
                let braced = before == Some(b'{')
                    && start.checked_sub(2).map(|at| bytes[at]) == Some(b'$')
                    && after == Some(b'}');
                let read =
                    before == Some(b'$') && !matches!(after, Some(b'=' | b':' | b'[' | b'('));
                if !(read || braced || matches!(before, Some(b'/' | b'.' | b'-'))) {
                    return false;
                }
            }
            start = end;
        }
        true
    }
}

/// Whether the bash grammar reads `text` as zsh would and finds every
/// command named by plain text that is not a builtin setting variables,
/// with `printf`, `print` and `set` options (every word of `print`)
/// spelled out too. The word lexer does not expand `{r..r}ead`,
/// `rea?` or `$(printf %s read)`, which the shell may turn into `read`; nor
/// `printf -? "$n"` into `printf -v`. Used only to refuse.
fn commands_named_as_written(text: &str) -> bool {
    let Ok(reading) = grammar::read(text) else {
        return false;
    };
    if reading.zsh_unsure {
        return false;
    }
    reading.commands.iter().all(|command| {
        work::step();
        let Some((name, args)) = command.words.split_first() else {
            return true;
        };
        let plain = |word: &grammar::Word| {
            word.data
                && !word.text.is_empty()
                && word
                    .text
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._/+-".contains(&byte))
        };
        if !plain(name) || name.text == "." || SETS_VARIABLES.contains(&name.text.as_str()) {
            return false;
        }
        if !matches!(name.text.as_str(), "printf" | "print" | "set") {
            return true;
        }
        // Options come first: as written, up to `--` or the first other
        // word, and none sets a variable (`-v`, zsh `set -A`). zsh `print`
        // reads options again after an option's value, which may itself be
        // `--` (`print -f %s -v n`, `print -f -- -v n`): all its words.
        let print = name.text == "print";
        for word in args {
            work::step();
            if !word.data {
                return false;
            }
            if !print && (word.text == "--" || !word.text.starts_with(['-', '+'])) {
                break;
            }
            if !word.text.starts_with(['-', '+']) {
                continue;
            }
            if word.text.contains(['v', 'A']) {
                return false;
            }
        }
        true
    })
}

/// A `mktemp` run in a substitution (its template may hold a variable's
/// name), which cannot set the line's variables.
fn own_mktemp(simple: &SimpleCommand) -> bool {
    simple.substituted && simple.words.first().is_some_and(|word| word == "mktemp")
}

/// A signal or shell event a trap names (`EXIT`, `SIGTERM`, `0`, `15`).
fn signal(word: &str) -> bool {
    SIGNALS.contains(&word.strip_prefix("SIG").unwrap_or(word))
        || (!word.is_empty() && word.len() <= 2 && word.bytes().all(|byte| byte.is_ascii_digit()))
}

/// A name no shell sets or reads on its own, and no environment variable
/// (those are uppercase by convention, and assigning one exports it):
/// lowercase letters, digits and `_`, starting with a letter.
fn plain_variable(name: &str) -> bool {
    name.bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        && !SHELL_NAMES.contains(&name)
        && !name.ends_with("_functions")
        && !name.ends_with("_proxy")
}

/// Whether a written word (or a piece of one) is a qualifying variable's
/// temp file itself.
pub(super) fn temp_place(word: &str) -> bool {
    word == TEMP_FILE
}

/// Best effort (see the module comment): whether a command takes a temp
/// file's path, other than as a file it redirects, only whole and only as
/// an operand of a command that does not itself put a link in its place:
/// `rm`, `cat`, `cp` without options, `mv` as the file moved, `tee`,
/// `chmod`, `touch`, `sort`, `uniq`, `sed`, a test and a removing trap
/// (judged on its own). Such a command may still print the path.
fn file_operands_ok(simple: &SimpleCommand, words: &[String]) -> bool {
    // A here-string passes the path itself (`cat <<< "$tmp" | xargs …`).
    let writes_to: std::collections::HashSet<usize> = simple.writes_to.iter().copied().collect();
    let redirected = |at: usize| writes_to.contains(&at) || simple.stdin_from == Some(at);
    let mut used = false;
    for (at, word) in words.iter().enumerate() {
        work::step();
        if word.contains(TEMP_FILE) && simple.here_string == Some(at) {
            return false;
        }
        if word.contains(TEMP_FILE) && !redirected(at) {
            if word != TEMP_FILE {
                return false;
            }
            used = true;
        }
    }
    if !used {
        return true;
    }
    // The command itself as written, not behind a wrapper that may add
    // words (`xargs cp …`) or a program named by a path (`./cp`).
    let Some((index, name)) = indirect::command_word(words) else {
        return false;
    };
    if index != 0 || words[0] != name {
        return false;
    }
    let operands: Vec<&str> = words[index + 1..]
        .iter()
        .enumerate()
        .filter(|(at, _)| !redirected(index + 1 + at))
        .map(|(_, word)| word.as_str())
        .collect();
    match name.as_str() {
        "rm" | "cat" | "tee" | "chmod" | "touch" | "sort" | "uniq" | "sed" | "test" | "["
        | "[[" | "trap" => true,
        "cp" => !operands.iter().any(|word| word.starts_with('-')),
        // The file moved, not where it goes (`mv ./link "$tmp"`).
        "mv" => {
            let files: Vec<&str> = operands
                .iter()
                .copied()
                .filter(|word| *word != "-f")
                .collect();
            matches!(files.as_slice(), [from, to]
                if *from == TEMP_FILE && *to != TEMP_FILE && !to.starts_with('-'))
        }
        _ => false,
    }
}

/// Whether text may define a function: `(` and `)` with only blanks
/// between them (`f() {`, `f ( ) {`).
fn defines_function(text: &str) -> bool {
    text.match_indices('(').any(|(at, _)| {
        work::step();
        text[at + 1..]
            .trim_start_matches([' ', '\t'])
            .starts_with(')')
    })
}

fn rm_flags(word: &str) -> bool {
    word.strip_prefix('-')
        .is_some_and(|flags| !flags.is_empty() && flags.bytes().all(|byte| b"rRfv".contains(&byte)))
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The variables a script sets first, each from a plain `mktemp`, one
/// after another (`a=$(mktemp); b="$(mktemp -d)" && …`, after `set -eu` or
/// `set -o pipefail`), whether each holds a folder, and the text after them.
fn leading_definitions(script: &str) -> (Vec<(&str, bool)>, &str) {
    let mut names = Vec::new();
    let mut rest = after_sets(script);
    while let Some((name, folder, after)) = definition(rest) {
        work::step();
        let trimmed = after.trim_start_matches([' ', '\t']);
        if trimmed.is_empty() || trimmed.starts_with("&&") || trimmed.starts_with("||") {
            names.push((name, folder));
            return (names, after);
        }
        match trimmed.strip_prefix([';', '\n']) {
            // `;;`, `;&` and `;|` end a case item.
            Some(next) if !next.starts_with([';', '&', '|']) => {
                names.push((name, folder));
                rest = next.trim_start_matches([' ', '\t', '\n']);
            }
            _ => return (Vec::new(), script),
        }
    }
    (names, rest)
}

/// The text after the leading `set`s that [`leading_set`] reads.
fn after_sets(script: &str) -> &str {
    let mut rest = script.trim_start_matches([' ', '\t', '\n']);
    while let Some(next) = leading_set(rest) {
        work::step();
        rest = next;
    }
    rest
}

/// The text after a leading `set` that only turns on `-e`, `-u`, `-x`,
/// `-v` or `-o pipefail`/`errexit`/`nounset`/`xtrace`, and its `;` or line
/// break.
fn leading_set(text: &str) -> Option<&str> {
    let end = text.find([';', '\n'])?;
    let mut words = text[..end]
        .split([' ', '\t'])
        .filter(|word| !word.is_empty());
    if words.next() != Some("set") {
        return None;
    }
    let mut option_next = false;
    let mut any = false;
    for word in words {
        work::step();
        let ok = if std::mem::take(&mut option_next) {
            matches!(word, "pipefail" | "errexit" | "nounset" | "xtrace")
        } else if word == "-o" {
            option_next = true;
            true
        } else {
            // `-euo pipefail`: a cluster may end in `o`.
            let flags = word.strip_prefix('-');
            option_next = flags.is_some_and(|flags| flags.len() > 1 && flags.ends_with('o'));
            flags.is_some_and(|flags| {
                let flags = flags.strip_suffix('o').unwrap_or(flags);
                !flags.is_empty() && flags.bytes().all(|byte| b"euxv".contains(&byte))
            })
        };
        if !ok {
            return None;
        }
        any = true;
    }
    if !any || option_next {
        return None;
    }
    let next = &text[end + 1..];
    (!next.starts_with([';', '&', '|'])).then(|| next.trim_start_matches([' ', '\t', '\n']))
}

/// `NAME=$(mktemp …)` or `NAME="$(mktemp …)"` at the start of `text`,
/// whether it makes a folder, and the text after it.
fn definition(text: &str) -> Option<(&str, bool, &str)> {
    let name_len = text.bytes().position(|byte| !is_name_byte(byte))?;
    let name = &text[..name_len];
    if name.is_empty() || name.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    let value = text[name_len..].strip_prefix('=')?;
    let (quoted, value) = match value.strip_prefix('"') {
        Some(value) => (true, value),
        None => (false, value),
    };
    let body = value.strip_prefix("$(")?;
    let close = body.find(')')?;
    // `${TMPDIR:-/tmp}` holds no `)`; any other `(` ends it.
    if body[..close].contains('(') {
        return None;
    }
    let folder = plain_mktemp(&body[..close])?;
    let after = &body[close + 1..];
    let after = if quoted {
        after.strip_prefix('"')?
    } else {
        after
    };
    // The value ends at the word's end.
    if after
        .bytes()
        .next()
        .is_some_and(|byte| !matches!(byte, b' ' | b'\t' | b'\n' | b';' | b'&' | b'|'))
    {
        return None;
    }
    Some((name, folder, after))
}

/// `mktemp` with options `-d`, `-q`, `--suffix=NAME`, `-p` a temp folder,
/// and at most one operand after them: `-t NAME` (BSD's prefix, GNU's
/// template, both in `$TMPDIR`; `-t` ends the options, as BSD reads its
/// next word as the prefix), with `-p` a plain template, else a template
/// directly in `/tmp` or `$TMPDIR`; a template ends in the `X`s `mktemp`
/// replaces. Whether it makes a folder.
fn plain_mktemp(text: &str) -> Option<bool> {
    const ROOTS: &[&str] = &["/tmp", "$TMPDIR", "${TMPDIR}", "${TMPDIR:-/tmp}"];
    let mut words = text.split([' ', '\t']).filter(|word| !word.is_empty());
    if words.next() != Some("mktemp") {
        return None;
    }
    let mut folder = false;
    let mut in_tmpdir = false;
    let mut in_root = false;
    let mut root_next = false;
    let mut operand: Option<&str> = None;
    for word in words {
        work::step();
        if operand.is_some() {
            return None;
        }
        if std::mem::take(&mut root_next) {
            if !ROOTS.contains(&unquoted(word)?) {
                return None;
            }
            in_root = true;
            continue;
        }
        if in_tmpdir {
            operand = Some(unquoted(word)?);
            continue;
        }
        match word {
            "-t" => in_tmpdir = true,
            "-p" => root_next = true,
            "--directory" => folder = true,
            "--quiet" => {}
            _ if word.strip_prefix("--suffix=").is_some_and(|suffix| {
                !suffix.is_empty()
                    && suffix
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            }) => {}
            _ => match word.strip_prefix('-') {
                Some(flags) if !flags.is_empty() => {
                    if !flags.bytes().all(|byte| b"dq".contains(&byte)) {
                        return None;
                    }
                    folder |= flags.contains('d');
                }
                Some(_) => return None,
                None => operand = Some(unquoted(word)?),
            },
        }
    }
    if root_next || (in_tmpdir && operand.is_none()) {
        return None;
    }
    let Some(operand) = operand else {
        return Some(folder);
    };
    let in_temp = if in_tmpdir {
        plain_name(operand) && !in_root
    } else if in_root {
        plain_name(operand) && operand.ends_with("XXX")
    } else {
        ["/tmp/", "$TMPDIR/", "${TMPDIR}/", "${TMPDIR:-/tmp}/"]
            .iter()
            .any(|root| {
                operand
                    .strip_prefix(root)
                    .is_some_and(|name| plain_name(name) && name.ends_with("XXX"))
            })
    };
    in_temp.then_some(folder)
}

/// A word without its surrounding quotes, if it holds no other quote and
/// no `$` but a leading `$TMPDIR` (single-quoted: none).
fn unquoted(word: &str) -> Option<&str> {
    let (inner, expands) = if let Some(inner) = word
        .strip_prefix('\'')
        .and_then(|word| word.strip_suffix('\''))
    {
        (inner, false)
    } else if let Some(inner) = word
        .strip_prefix('"')
        .and_then(|word| word.strip_suffix('"'))
    {
        (inner, true)
    } else {
        (word, true)
    };
    if inner.contains(['\'', '"']) {
        return None;
    }
    let tmpdir = ["$TMPDIR", "${TMPDIR}", "${TMPDIR:-/tmp}"]
        .iter()
        .find_map(|root| inner.strip_prefix(root))
        .filter(|rest| rest.is_empty() || rest.starts_with('/'));
    match tmpdir {
        Some(rest) if expands && !rest.contains(['$', '{', '}']) => Some(inner),
        _ if !inner.contains(['$', '{', '}']) => Some(inner),
        _ => None,
    }
}

/// A file name of letters, digits, `.`, `_` and `-`, not starting with `.`.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}
