//! #1064/#1159 (option (c), D157 precedent): variables that make a later
//! program load code from a folder or file, set to a value the classifier
//! cannot vouch for. Unlike the variables in `code_variables`, these are
//! also set for ordinary reasons (`PATH=$HOME/bin:$PATH`, `ENV=production`,
//! `NODE_OPTIONS=--max-old-space-size=4096`), so only risky values count:
//!
//! - A search path (`PATH`, `LD_LIBRARY_PATH`, `PYTHONPATH`, ...) given an
//!   entry that is empty (the current folder), relative, unknown (an
//!   unseen value, a variable other than the one being set, a glob), or a
//!   folder the agent may write: in the action's folders or a temp folder.
//! - Interpreter options that load code: `NODE_OPTIONS` `--require`/`-r`/
//!   `--import`/`--loader`, Java's `-javaagent`/`-agentpath` and the like,
//!   `RUBYOPT` `-r`/`-I`; any `PYTHONSTARTUP` or `PERL5OPT`.
//! - `ENV` (which an interactive POSIX shell sources), on a line that also
//!   starts a shell that may be interactive (judged by the caller).
//!
//! A setting whose value is not visible (`read PATH`, `printf -v PATH`,
//! `declare -n r=PATH`, `${PATH:=x}`, `for PATH in`, PowerShell's
//! `$env:PATH = ...`, cmd's `set PATH=...`) counts. Everything here only
//! adds findings.

use super::shell;
use super::shell::SimpleCommand;
use super::work;
use std::collections::HashSet;

/// What a variable's value does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    /// `:`-separated folders programs or libraries are loaded from.
    SearchPath,
    /// Interpreter options; the flags that load code are listed.
    Options(&'static [&'static str]),
    /// Any value loads code (`PYTHONSTARTUP`, `PERL5OPT`).
    AnyValue,
    /// `ENV`: sourced by an interactive POSIX shell.
    ShellEnv,
}

/// Node options that load a module (`-r` also as `-rx`).
const NODE_LOADING: &[&str] = &[
    "-r",
    "--require",
    "--import",
    "--loader",
    "--experimental-loader",
    // Options that read more options or code from a file.
    "--env-file",
    "--env-file-if-exists",
    "--experimental-config-file",
    "--snapshot-blob",
    "--openssl-config",
];
/// Java options that load agents, classes or native libraries, run a
/// command, or read more options from a file (`@file`).
const JAVA_LOADING: &[&str] = &[
    "-javaagent",
    "-agentpath",
    "-agentlib",
    "-Xbootclasspath",
    "-XX:OnError",
    "-XX:OnOutOfMemoryError",
    "-XX:VMOptionsFile",
    "-XX:Flags",
    "-Djava.system.class.loader",
    "-Djava.security.manager",
    "-Djava.library.path",
    "-Djava.class.path",
    "-Djava.ext.dirs",
    "-Djava.endorsed.dirs",
    "-cp",
    "-classpath",
    "--class-path",
    "--module-path",
    "--upgrade-module-path",
    "--patch-module",
    "@",
];
/// Ruby options that require a library or add a load folder, also within
/// a cluster (`-wrx`, but not `-Ebinary`, an option's value).
const RUBY_LOADING: &[&str] = &["-r", "-I"];

fn kind_of(name: &str) -> Option<Kind> {
    VARIABLES
        .iter()
        .find(|(variable, _)| *variable == name)
        .map(|(_, kind)| *kind)
}

/// Every variable here, for builtins that name one.
const VARIABLES: &[(&str, Kind)] = &[
    ("PATH", Kind::SearchPath),
    ("LD_LIBRARY_PATH", Kind::SearchPath),
    ("DYLD_LIBRARY_PATH", Kind::SearchPath),
    ("DYLD_FALLBACK_LIBRARY_PATH", Kind::SearchPath),
    ("DYLD_FRAMEWORK_PATH", Kind::SearchPath),
    ("DYLD_FALLBACK_FRAMEWORK_PATH", Kind::SearchPath),
    ("PYTHONPATH", Kind::SearchPath),
    ("PYTHONHOME", Kind::SearchPath),
    ("PYTHONUSERBASE", Kind::SearchPath),
    ("NODE_PATH", Kind::SearchPath),
    ("PERL5LIB", Kind::SearchPath),
    ("PERLLIB", Kind::SearchPath),
    ("RUBYLIB", Kind::SearchPath),
    ("CLASSPATH", Kind::SearchPath),
    ("GEM_PATH", Kind::SearchPath),
    ("GEM_HOME", Kind::SearchPath),
    ("BASH_LOADABLES_PATH", Kind::SearchPath),
    ("GCONV_PATH", Kind::SearchPath),
    ("NODE_OPTIONS", Kind::Options(NODE_LOADING)),
    ("JAVA_TOOL_OPTIONS", Kind::Options(JAVA_LOADING)),
    ("_JAVA_OPTIONS", Kind::Options(JAVA_LOADING)),
    ("JDK_JAVA_OPTIONS", Kind::Options(JAVA_LOADING)),
    ("RUBYOPT", Kind::Options(RUBY_LOADING)),
    ("PYTHONSTARTUP", Kind::AnyValue),
    ("PERL5OPT", Kind::AnyValue),
    ("ENV", Kind::ShellEnv),
];

/// Declaration builtins, whose `NAME=value` operands set variables.
const DECLARES: &[&str] = &[
    "declare", "typeset", "local", "export", "readonly", "integer", "float", "private",
];
/// Builtins that set a variable they name to a value read at run time.
const READERS: &[&str] = &[
    "read",
    "printf",
    "print",
    "mapfile",
    "readarray",
    "vared",
    "getopts",
];

/// What one simple command does to these variables.
#[derive(Default)]
pub(super) struct Judgment {
    /// A variable was set to a risky value, or to one the classifier
    /// cannot see.
    pub(super) risky: bool,
    /// `ENV` was set (any value).
    pub(super) sets_env: bool,
}

/// Judge the settings in `simple` (`words`: as substituted; `command`: the
/// command word's index and lowercase name). `cut`: variables whose known
/// value may not be the shell's (see [`scan_split_values`]), so a value
/// spelled with one is unknown; a variable assigned from one is added. `risky_folder` judges an absolute entry of a search
/// path. Linear in the words.
pub(super) fn judge(
    simple: &SimpleCommand,
    words: &[String],
    command: Option<&(usize, String)>,
    cut: &mut HashSet<String>,
    windows: bool,
    risky_folder: &dyn Fn(&str) -> bool,
) -> Judgment {
    let mut judgment = Judgment::default();
    let raw = &simple.words;
    // `${PATH:=x}` (braces dropped: `$PATH:=x`) sets it to a value the
    // classifier does not follow.
    for word in raw.iter().chain(words) {
        work::step();
        for (at, _) in word.match_indices('$') {
            work::step();
            let rest = &word[at + 1..];
            if let Some((kind, end)) = leading_name(rest)
                && rest[end..].trim_start_matches(':').starts_with('=')
            {
                judgment.unseen(kind);
            }
        }
    }
    // PowerShell (`$env:Path = ...`, `${env:PATH} += ...`), in any case:
    // its values are not read.
    for (at, word) in raw.iter().enumerate() {
        work::step();
        let upper = word.to_ascii_uppercase();
        let Some(rest) = ["$ENV:", "${ENV:"]
            .iter()
            .find_map(|prefix| upper.strip_prefix(prefix))
        else {
            continue;
        };
        let assigned = |text: &str| {
            let text = text.trim_start_matches('}').trim_start();
            ["=", "+=", "-="].iter().any(|op| text.starts_with(op))
        };
        if let Some((kind, end)) = leading_name(rest)
            && (assigned(&rest[end..]) || raw.get(at + 1).is_some_and(|next| assigned(next)))
        {
            judgment.unseen(kind);
        }
    }
    let before = command.map_or(words.len(), |(index, _)| *index);
    let name = command.map(|(_, name)| name.as_str());
    let operands = command.map_or(&words[..0], |(index, _)| &words[index + 1..]);
    // cmd (`set PATH=...`, `set "PATH=..."`, `setx PATH ...`), in any case.
    if matches!(name, Some("set" | "setx")) {
        for word in operands {
            work::step();
            if let Some((kind, _)) =
                leading_name(&word.trim_start_matches('"').to_ascii_uppercase())
            {
                judgment.unseen(kind);
            }
        }
    }
    // Assignments before the command (`PATH=x cmd`, `env PATH=x cmd`, `env
    // -SPATH=x cmd`) and a declaration builtin's operands (`export PATH=x`).
    let declares = name.is_some_and(|name| DECLARES.contains(&name));
    let nameref = declares
        && operands
            .iter()
            .any(|word| word.starts_with('-') && !word.starts_with("--") && word.contains('n'));
    for (at, word) in words.iter().enumerate() {
        work::step();
        if !(at < before || (declares && at > before)) {
            continue;
        }
        let word = word
            .strip_prefix("--split-string=")
            .or_else(|| word.strip_prefix("-S"))
            .unwrap_or(word);
        let Some((target, value)) = word.split_once('=') else {
            continue;
        };
        // A nameref's target (`declare -n r=PATH`) is set through it, and a
        // nameref by one's name (`declare -n PATH=v`) reads another value.
        if nameref {
            for name in [target, value] {
                work::step();
                if let Some(kind) = kind_of(name) {
                    judgment.unseen(kind);
                }
            }
            continue;
        }
        let appended = target.ends_with('+');
        let base = target
            .trim_end_matches('+')
            .split('[')
            .next()
            .unwrap_or(target);
        // As written: a misread expansion (`${PATH#*:}`) or a variable whose
        // known value may be cut short is not the value substituted, and a
        // variable given one may be cut short too (`q=$o`).
        let written = raw.get(at).map_or("", String::as_str);
        let from_cut = references_cut(written, base, cut);
        if from_cut {
            cut.insert(base.to_string());
        }
        let Some(kind) = kind_of(base) else {
            continue;
        };
        let kept = self_default(written, base);
        let misread = simple.operated.contains(&at) || simple.hidden.contains(&at);
        let misread = misread && kept.is_none() && !colons_only(written);
        let value = kept.unwrap_or_else(|| value.to_string());
        if kind == Kind::ShellEnv {
            judgment.sets_env = true;
        } else if from_cut
            || misread
            || risky_value(kind, base, &value, appended, windows, risky_folder)
        {
            judgment.risky = true;
        }
    }
    // `read PATH`, `printf -v PATH`, `mapfile -t PATH`, `for PATH in`,
    // `select PATH in`: set to a value read at run time (also attached:
    // `-vPATH`).
    let looped = matches!(name, Some("for" | "select"));
    if name.is_some_and(|name| READERS.contains(&name)) || looped {
        for word in operands {
            work::step();
            // `read PATH[0]`, `printf -v 'PATH[0]'` set it too.
            let operand = word.trim_start_matches('-');
            let operand = operand.split('[').next().unwrap_or(operand);
            for (found, kind) in VARIABLES {
                work::step();
                if operand == *found || (word.starts_with('-') && operand.ends_with(found)) {
                    judgment.unseen(*kind);
                }
            }
            if looped {
                break;
            }
        }
    }
    judgment
}

impl Judgment {
    /// A setting whose value the classifier cannot see.
    fn unseen(&mut self, kind: Kind) {
        match kind {
            Kind::ShellEnv => self.sets_env = true,
            _ => self.risky = true,
        }
    }
}

/// The variable whose name starts `text`, and where the name ends.
fn leading_name(text: &str) -> Option<(Kind, usize)> {
    let end = text
        .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .unwrap_or(text.len());
    kind_of(&text[..end]).map(|kind| (kind, end))
}

/// Whether `written` spells a variable in `cut` other than `name` itself
/// (whose own cut values are judged as written).
fn references_cut(written: &str, name: &str, cut: &HashSet<String>) -> bool {
    !cut.is_empty()
        && written.match_indices('$').any(|(at, _)| {
            work::step();
            let rest = &written[at + 1..];
            let end = rest
                .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
                .unwrap_or(rest.len());
            &rest[..end] != name && cut.contains(&rest[..end])
        })
}

/// The everyday misread expansion that keeps the old value, as the value
/// it gives: `NAME=prefix${NAME:+:$NAME}` is `prefix:$NAME`, and
/// `NAME=${NAME:+$NAME:}suffix` is `$NAME:suffix`, with no other expansion
/// (braces dropped: `NAME=/opt/lib$NAME:+:$NAME`).
fn self_default(written: &str, name: &str) -> Option<String> {
    let (_, value) = written.split_once('=')?;
    let reference = format!("${name}");
    let after = format!("{reference}:+:{reference}");
    let before = format!("{reference}:+{reference}:");
    let plain = |text: &&str| !text.is_empty() && !text.contains(['$', '`']);
    value
        .strip_suffix(&after)
        .filter(plain)
        .map(|prefix| format!("{prefix}:{reference}"))
        .or_else(|| {
            value
                .strip_prefix(&before)
                .filter(plain)
                .map(|suffix| format!("{reference}:{suffix}"))
        })
}

/// Whether the misread expansions `written` (as lexed: braces dropped) may
/// hold are only zsh's `$name:` read as a modifier where none follows
/// (`$p:$PATH`, `$PATH:/usr/sbin`): every `$name` ends the word or is
/// followed by `$` or by `:` and then `/`, `~`, `:`, `$` or the end. A
/// braced operator (`${p#/}`, `${p/x/y}`, `${p:1}`) reads as `$p` followed
/// by something else, so it does not pass.
fn colons_only(written: &str) -> bool {
    written.match_indices('$').all(|(at, _)| {
        work::step();
        let rest = &written[at + 1..];
        let end = rest
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .unwrap_or(rest.len());
        let after = &rest[end..];
        end > 0
            && (after.is_empty()
                || after.starts_with('$')
                || after.strip_prefix(':').is_some_and(|after| {
                    after.is_empty() || after.starts_with(['/', '~', ':', '$'])
                }))
    })
}

/// Text whose value the classifier does not know.
fn unknown(text: &str) -> bool {
    text.contains(['$', '`', '*', '?', '[']) || text.contains(shell::UNSEEN_OUTPUT)
}

/// Whether `value` of the variable `name` is risky (`appended`: set by
/// `+=`; `windows`: under Windows path rules, where `C:\x` is one entry).
fn risky_value(
    kind: Kind,
    name: &str,
    value: &str,
    appended: bool,
    windows: bool,
    risky_folder: &dyn Fn(&str) -> bool,
) -> bool {
    // The variable's own value is kept (`PATH=$PATH:x`).
    let reference = format!("${name}");
    match kind {
        Kind::ShellEnv => false,
        Kind::AnyValue => true,
        Kind::Options(loading) => value.split_whitespace().any(|token| {
            work::step();
            token != reference
                && (unknown(token)
                    || loading.iter().any(|flag| {
                        work::step();
                        loads(flag, token)
                    }))
        }),
        Kind::SearchPath => {
            let entry_risky = |entry: &str| {
                // `$HOME/bin` as written (a quoted value) is `~/bin`.
                let home = entry
                    .strip_prefix("$HOME")
                    .filter(|rest| rest.is_empty() || rest.starts_with('/'))
                    .map(|rest| format!("~{rest}"));
                let entry = home.as_deref().unwrap_or(entry);
                let absolute = entry.starts_with(['/', '~'])
                    || (windows && (entry.starts_with('\\') || drive(entry)));
                entry != reference
                    && (entry.is_empty()
                        || unknown(entry)
                        || !absolute
                        // Linux: `/proc/self/cwd` is the working folder.
                        || entry.starts_with("/proc/")
                        || risky_folder(entry))
            };
            entries(value, windows)
                .into_iter()
                .enumerate()
                .any(|(at, entry)| {
                    work::step();
                    // `NAME+=:x` adds to the last entry; an empty start adds
                    // an empty entry only where the variable was unset
                    // (`PATH` always is set).
                    if at == 0 && appended && entry.is_empty() {
                        return name != "PATH";
                    }
                    entry_risky(entry)
                })
        }
    }
}

/// Whether `entry` starts with a drive (`C:\`, `C:/`).
fn drive(entry: &str) -> bool {
    let bytes = entry.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

/// A search path's `:`-separated entries; under Windows path rules a
/// one-letter piece before `\` or `/` is a drive (`C:\tools`). Linear.
fn entries(value: &str, windows: bool) -> Vec<&str> {
    let mut entries = Vec::new();
    let mut start = 0;
    let mut from = 0;
    loop {
        work::step();
        let end = value[from..].find(':').map_or(value.len(), |at| from + at);
        if windows && end < value.len() && drive(&value[start..]) && end == start + 1 {
            from = end + 1;
            continue;
        }
        entries.push(&value[start..end]);
        if end == value.len() {
            return entries;
        }
        start = end + 1;
        from = start;
    }
}

/// Whether `token` is the loading option `flag`: as a word, with an
/// attached value (`--require=x`, `-javaagent:x`, `-Xbootclasspath/a:x`),
/// within a short-option cluster for one-letter flags (`-rx`, `-wIlib`), or
/// any argument file (`@file`).
fn loads(flag: &str, token: &str) -> bool {
    if flag == "@" {
        return token.starts_with('@');
    }
    if flag.len() == 2 && !token.starts_with("--") && token.starts_with('-') {
        // A cluster ends at an option that takes the rest as its value.
        for letter in token[1..].chars() {
            work::step();
            if flag[1..].starts_with(letter) {
                return true;
            }
            if "EKWCx0Fil".contains(letter) {
                return false;
            }
        }
        return false;
    }
    token == flag
        || token
            .strip_prefix(flag)
            .is_some_and(|rest| rest.starts_with(['=', ':', '/']) || flag.starts_with("-XX"))
}

/// Bytes that end an unquoted shell word.
fn ends_word(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t' | b'\n' | b'\r' | b';' | b'&' | b'|' | b'(' | b')' | b'<' | b'>' | b'`'
    )
}

/// How a byte of a word was written.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Written {
    Plain,
    /// After a backslash.
    Escaped,
    /// Inside double quotes.
    Double,
    /// Inside single quotes or `$'...'`.
    Single,
}

/// One step of a quote-aware reading of `bytes` at `at`, which `quote`
/// (`'`, `"`, or `$` for `$'...'`) tracks: the next position, and the byte
/// the word keeps there with how it was written.
fn read(bytes: &[u8], at: usize, quote: &mut Option<u8>) -> (usize, Option<(u8, Written)>) {
    let byte = bytes[at];
    match (*quote, byte) {
        (None | Some(b'"') | Some(b'$'), b'\\') if at + 1 < bytes.len() => {
            (at + 2, Some((bytes[at + 1], Written::Escaped)))
        }
        (None, b'\'' | b'"') => {
            *quote = Some(byte);
            (at + 1, None)
        }
        (None, b'$') if bytes.get(at + 1) == Some(&b'\'') => {
            *quote = Some(b'$');
            (at + 2, None)
        }
        (Some(b'\'' | b'$'), b'\'') | (Some(b'"'), b'"') => {
            *quote = None;
            (at + 1, None)
        }
        (open, _) => {
            let written = match open {
                None => Written::Plain,
                Some(b'"') => Written::Double,
                Some(_) => Written::Single,
            };
            (at + 1, Some((byte, written)))
        }
    }
}

/// Whether the word lexer, which removes quotes and backslashes and drops
/// braces, keeps `byte` (written as `written`) as the shell does: plain
/// name and path characters anywhere, `$` (an expansion both expand) and
/// `~` unquoted or `$` in double quotes, anything else unquoted except
/// braces (literal in an assignment). A quoted blank or operator splits the
/// lexer's word; a quoted `~` or `$`, or a backslash, is literal.
fn lexed_alike(byte: u8, written: Written) -> bool {
    byte.is_ascii_alphanumeric()
        || byte >= 0x80
        || b"/._-:@%+=".contains(&byte)
        || (written == Written::Plain && !matches!(byte, b'{' | b'}'))
        || (written == Written::Double && byte == b'$')
}

/// #1064: find assignments in `text` whose value the word lexer does not
/// read as the shell does ([`lexed_alike`]): a quoted blank or operator
/// splits it (`PATH="/usr/bin :." git`, `export o='--a --require ./x'`),
/// a quoted `~` or `$` is literal (`PATH='~/bin'` is relative). Each such
/// name is added to `cut`, and a value of one of these variables is judged
/// here as written (an expansion in it other than the variable's own is
/// unknown; a search path with a literal `~`, `$` or backslash is risky).
/// Search-path and option values are returned in `splits`, to be judged
/// against the folders the line may be in. An
/// assignment counts at the start of a word, just inside a quote that
/// starts one (`env "PATH=…"`), or unquoted after `=` or `-S`
/// (`--split-string=PATH=…`, `env -SPATH=…`; a quoted `-S'PATH=…'` is
/// split at blanks by `env` as the lexer splits it, and judged there).
/// One pass over `text`.
pub(super) fn scan_split_values(
    text: &str,
    cut: &mut HashSet<String>,
    splits: &mut Vec<Split>,
) -> Judgment {
    let mut judgment = Judgment::default();
    let bytes = text.as_bytes();
    let mut quote = None;
    let mut word_start = 0;
    let mut at = 0;
    while at < bytes.len() {
        work::step();
        if quote.is_none() && ends_word(bytes[at]) {
            at += 1;
            word_start = at;
            continue;
        }
        let opened_word = quote.is_some()
            && (at == word_start + 1 || (at == word_start + 2 && bytes[word_start] == b'$'));
        let candidate = (bytes[at].is_ascii_alphabetic() || bytes[at] == b'_')
            && (opened_word
                || (quote.is_none()
                    && (at == word_start
                        || bytes[at - 1] == b'='
                        || (at >= 2 && &bytes[at - 2..at] == b"-S"))));
        if !candidate {
            at = read(bytes, at, &mut quote).0;
            continue;
        }
        let mut end = at;
        while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
            work::step();
            end += 1;
        }
        let name = &text[at..end];
        let appended = bytes.get(end) == Some(&b'+');
        let equals = end + usize::from(appended);
        if bytes.get(equals) != Some(&b'=') {
            at = end;
            continue;
        }
        // The value, to an unquoted end of the word.
        let mut value = Vec::new();
        let mut differs = false;
        let mut literal = false;
        at = equals + 1;
        while at < bytes.len() && !(quote.is_none() && ends_word(bytes[at])) {
            work::step();
            let (next, kept) = read(bytes, at, &mut quote);
            if let Some((byte, written)) = kept {
                let alike = lexed_alike(byte, written);
                differs |= !alike;
                literal |=
                    !alike && matches!(byte, b'~' | b'$' | b'\\') || written == Written::Escaped;
                value.push(byte);
            }
            at = next;
        }
        if differs || literal {
            cut.insert(name.to_string());
            match kind_of(name) {
                Some(Kind::ShellEnv) => judgment.sets_env = true,
                Some(Kind::SearchPath) if literal => judgment.risky = true,
                Some(kind) => splits.push(Split {
                    kind,
                    value: kept_value(&plain_braces(&String::from_utf8_lossy(&value)), name),
                    name: name.to_string(),
                    appended,
                }),
                None => {}
            }
        }
    }
    judgment
}

/// [`self_default`] as written with braces: `prefix${NAME:+:$NAME}` is
/// `prefix:$NAME`, `${NAME:+$NAME:}suffix` is `$NAME:suffix`.
fn kept_value(value: &str, name: &str) -> String {
    let reference = format!("${name}");
    let lexed = value
        .strip_suffix(&format!("${{{name}:+:{reference}}}"))
        .map(|prefix| format!("{prefix}{reference}:+:{reference}"))
        .or_else(|| {
            value
                .strip_prefix(&format!("${{{name}:+{reference}:}}"))
                .map(|suffix| format!("{reference}:+{reference}:{suffix}"))
        });
    lexed
        .and_then(|lexed| self_default(&format!("{name}={lexed}"), name))
        .unwrap_or_else(|| value.to_string())
}

/// A value [`scan_split_values`] read as written, judged by [`judge_split`]
/// once the folders the line may be in are known.
pub(super) struct Split {
    kind: Kind,
    name: String,
    value: String,
    appended: bool,
}

/// Whether a value read as written is risky (see [`risky_value`]).
pub(super) fn judge_split(
    split: &Split,
    windows: bool,
    risky_folder: &dyn Fn(&str) -> bool,
) -> bool {
    risky_value(
        split.kind,
        &split.name,
        &split.value,
        split.appended,
        windows,
        risky_folder,
    )
}

/// `${NAME}` as `$NAME`, as the word lexer spells it. Linear.
fn plain_braces(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("${") {
        work::step();
        out.push_str(&rest[..at + 1]);
        let after = &rest[at + 2..];
        let end = after
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .unwrap_or(after.len());
        if end > 0 && after[end..].starts_with('}') {
            out.push_str(&after[..end]);
            rest = &after[end + 1..];
        } else {
            out.push('{');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Options of a shell word read before it counts as interactive.
const MAX_SHELL_OPTIONS: usize = 64;

/// POSIX shells that source `ENV` when interactive (zsh does when it
/// emulates sh or ksh).
const ENV_SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "dash", "ksh", "ash", "mksh", "yash", "rbash", "posh", "lksh", "oksh",
    "hush", "osh", "ksh93", "pdksh",
];

/// Whether `lower` (one simple command's lowercase words) starts a shell
/// that may be interactive: given `-i`, or reading its input (`-s`, or no
/// `-c` and no script) when that input is not a pipe or redirection
/// (`fed`), since it may be a terminal. Each scan stops at the first word
/// that is not an option, so the words are read about once.
pub(super) fn starts_interactive_shell(lower: &[String], fed: bool) -> bool {
    lower.iter().enumerate().any(|(at, word)| {
        work::step();
        if !ENV_SHELLS.contains(&shell::basename(word)) {
            return false;
        }
        let mut skip = false;
        for (offset, option) in lower[at + 1..].iter().enumerate() {
            work::step();
            // Past this many options it may be interactive (a later `-i`);
            // the cap keeps the scans from one shell word to the next
            // linear.
            if offset >= MAX_SHELL_OPTIONS {
                return true;
            }
            if std::mem::take(&mut skip) {
                continue;
            }
            if option == "--" {
                return !fed && lower.len() == at + offset + 2;
            }
            if !(option.starts_with('-') || option.starts_with('+')) {
                // A script operand.
                return false;
            }
            if option.starts_with("--") {
                skip = matches!(option.as_str(), "--rcfile" | "--init-file");
                continue;
            }
            let cluster = &option[1..];
            if cluster.contains('i') {
                return true;
            }
            if cluster.contains('c') {
                return false;
            }
            if cluster.contains('s') {
                return !fed;
            }
            skip = cluster.ends_with(['o', 'O']);
        }
        !fed
    })
}
