//! Indirect routes to a protected surface the plain word scan misses:
//! string literals assembled by inline interpreter code, encoded payloads,
//! scripts run from a file, opaque decode-and-run pipelines and patches that
//! write commands into files that are run later.

use super::invocation;
use super::invocation::Code;
use super::invocation::Invocation;
use super::shell::SimpleCommand;
use super::shell::basename;
use super::work;
use base64::Engine;

/// Bytes of one script file the classifier reads.
pub(super) const SCRIPT_READ_LIMIT: u64 = 256 * 1024;

/// Text built from adjacent string literals: `'~/.co' + 'dex'`,
/// `join('~', '.codex')` and `' '.join(['corb' + 'anu', 'vault'])` all name
/// a protected surface. Escapes such as `\x2e` are decoded. A run of
/// literals joined only by concatenation or argument punctuation yields two
/// texts: arguments joined as a path (`a/b`) and as words (`a b`).
pub(super) fn joined_literals(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    // Each run: (as a path, as words, plain, literal count).
    let mut runs: Vec<(String, String, String, usize)> = Vec::new();
    let mut run: (String, String, String, usize) = Default::default();
    // Literals nested in a literal (`"... '~' + '/.co' ..."`).
    let mut nested: Vec<String> = Vec::new();
    let mut index = 0;
    let mut gap = String::new();
    while index < chars.len() {
        work::step();
        let quote = chars[index];
        if !matches!(quote, '\'' | '"' | '`') {
            gap.push(quote);
            index += 1;
            continue;
        }
        let mut literal = String::new();
        let mut escaped = false;
        index += 1;
        while index < chars.len() && chars[index] != quote {
            work::step();
            if chars[index] == '\\' && index + 1 < chars.len() {
                let (decoded, used) = decode_escape(&chars[index + 1..]);
                // #841: only an escape that changes the text hides
                // anything (`\x2e`, `\056`, `\n`); `\[`, `\\` or a
                // backslash before a newline (a sed `a\` append) do not.
                escaped |= decoded != Some(chars[index + 1]);
                literal.extend(decoded);
                index += 1 + used;
            } else {
                literal.push(chars[index]);
                index += 1;
            }
        }
        index += 1;
        // #841: an empty literal (`sed -i ''`) adds nothing to a run.
        if literal.is_empty() {
            continue;
        }
        if literal.contains(['\'', '"', '`']) {
            nested.extend(joined_literals(&literal));
        }
        // A lone literal matters too once escapes are decoded (`'\x2ecodex'`)
        // or when a shell would expand its braces (`shell=True`).
        if escaped || (literal.contains('{') && literal.contains(',')) {
            nested.push(literal.clone());
        }
        let joins = gap
            .chars()
            .all(|ch| ch.is_whitespace() || "+.,/()[]".contains(ch));
        if !joins {
            runs.push(std::mem::take(&mut run));
        }
        let (path, words, plain, count) = &mut run;
        if *count > 0 {
            if gap.contains(',') || gap.contains('/') {
                path.push('/');
            }
            if gap.contains(',') {
                words.push(' ');
            }
        }
        gap.clear();
        path.push_str(&literal);
        words.push_str(&literal);
        plain.push_str(&literal);
        *count += 1;
    }
    runs.push(run);
    runs.into_iter()
        .filter(|(_, _, _, count)| *count > 1)
        .flat_map(|(path, words, plain, _)| [path, words, plain])
        .chain(nested)
        .collect()
}

/// One backslash escape (after the backslash): `\xHH`, `\uHHHH`, octal and
/// the usual single letters. Returns the character and the length consumed.
fn decode_escape(chars: &[char]) -> (Option<char>, usize) {
    let hex = |len: usize| {
        let text: String = chars
            .iter()
            .skip(1)
            .take(len)
            .take_while(|ch| ch.is_ascii_hexdigit())
            .collect();
        let decoded = u32::from_str_radix(&text, 16).ok().and_then(char::from_u32);
        (decoded, 1 + text.len())
    };
    match chars.first() {
        Some('x') => hex(2),
        Some('u') => hex(4),
        Some('0'..='7') => {
            let text: String = chars
                .iter()
                .take(3)
                .take_while(|ch| ('0'..='7').contains(*ch))
                .collect();
            let decoded = u32::from_str_radix(&text, 8).ok().and_then(char::from_u32);
            (decoded, text.len())
        }
        Some('n') => (Some('\n'), 1),
        Some('t') => (Some('\t'), 1),
        Some(other) => (Some(*other), 1),
        None => (None, 0),
    }
}

/// Text hidden in a base64 or hex word, when it decodes to readable text.
pub(super) fn decoded_payload(word: &str) -> Option<String> {
    let word = word.trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '=');
    if word.len() < 12 {
        return None;
    }
    let bytes = if word.len() % 2 == 0 && word.chars().all(|ch| ch.is_ascii_hexdigit()) {
        hex_bytes(word)?
    } else if word
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/' | '-' | '_' | '='))
    {
        let engine = base64::engine::general_purpose::GeneralPurpose::new(
            &base64::alphabet::STANDARD,
            base64::engine::GeneralPurposeConfig::new()
                .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
        );
        let standard = word.replace('-', "+").replace('_', "/");
        engine.decode(standard).ok()?
    } else {
        return None;
    };
    let text = String::from_utf8(bytes).ok()?;
    let readable = text
        .chars()
        .all(|ch| !ch.is_control() || ch.is_whitespace());
    (readable && text.chars().any(char::is_alphabetic)).then_some(text)
}

fn hex_bytes(word: &str) -> Option<Vec<u8>> {
    (0..word.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&word[at..at + 2], 16).ok())
        .collect()
}

/// Wrappers: options that take a value, and operands before the command.
pub(super) const WRAPPERS: &[(&str, &[&str], usize)] = &[
    (
        "sudo",
        &[
            "-u",
            "-g",
            "-h",
            "-p",
            "-C",
            "-D",
            "-r",
            "-t",
            "-U",
            "-T",
            "--user",
            "--group",
            "--host",
            "--prompt",
            "--close-from",
            "--chdir",
            "--role",
            "--type",
            "--other-user",
            "--command-timeout",
        ],
        0,
    ),
    ("doas", &["-u", "-C"], 0),
    ("env", &["-u", "-C", "--unset", "--chdir"], 0),
    ("nice", &["-n", "--adjustment"], 0),
    ("nohup", &[], 0),
    ("time", &["-f", "-o", "--format", "--output"], 0),
    ("timeout", &["-s", "--signal", "-k", "--kill-after"], 1),
    ("exec", &["-a"], 0),
    ("command", &[], 0),
    ("builtin", &[], 0),
    (
        "xargs",
        &[
            "-I",
            "-L",
            "-n",
            "-P",
            "-s",
            "-d",
            "-E",
            "-a",
            "--max-args",
            "--max-procs",
            "--delimiter",
            "--arg-file",
            "--replace",
        ],
        0,
    ),
    ("stdbuf", &["-i", "-o", "-e"], 0),
    ("setsid", &[], 0),
    ("caffeinate", &["-t", "-w"], 0),
    ("arch", &[], 0),
    ("unbuffer", &[], 0),
    ("ionice", &["-c", "-n", "-p", "-P", "-u"], 0),
    ("chrt", &[], 1),
    // `taskset -c 0 cmd`: `-c` (`--cpu-list`) takes no value; it reads the
    // mask operand as a CPU list.
    ("taskset", &[], 1),
    (
        "flock",
        &["-w", "-E", "--timeout", "--conflict-exit-code"],
        1,
    ),
    ("runuser", &["-u", "-g", "-G"], 0),
    ("sg", &[], 1),
    ("nsenter", &["-t", "--target"], 0),
    ("unshare", &[], 0),
    ("firejail", &[], 0),
    ("systemd-run", &["-p", "--property", "-u", "--unit"], 0),
    ("busybox", &[], 0),
    ("toybox", &[], 0),
    ("watch", &["-n", "--interval"], 0),
];

/// #1482: letters known to take no value, per wrapper. A cluster is read
/// through these only: a letter not listed here may take a value (`sudo
/// -R`, `xargs -i`, `nsenter -n`), which would hold the rest of the word,
/// so a cluster with one is read as one word, as before.
const FLAG_LETTERS: &[(&str, &str)] = &[
    ("sudo", "ABbEeHiKklnPSsVv"),
    ("doas", "Lns"),
    ("env", "0iv"),
    ("time", "alpqv"),
    ("timeout", "v"),
    ("exec", "cl"),
    ("xargs", "0oprtx"),
    ("caffeinate", "dimsu"),
    ("ionice", "t"),
    ("flock", "Fnosux"),
    ("runuser", "flmPp"),
    ("nsenter", "aFZ"),
    ("systemd-run", "dGPqrSt"),
    ("watch", "bcegptx"),
];

/// #1482: how many words a wrapper's option word uses: one, or two when
/// it takes a value that is not attached. A short cluster takes the next
/// word only when its last letter takes a value and every letter before it
/// is a known flag of `wrapper` (`sudo -nC 3`, `sudo -nD sub`, `env -iu
/// X`); any other cluster is one word. A long option takes the next word
/// only when it is named in full (`sudo --chdir sub`); an abbreviation is
/// read as before, since some long options take an optional value that is
/// never a separate word.
pub(super) fn option_words(wrapper: &str, option: &str, value_options: &[&str]) -> usize {
    if value_options.contains(&option) {
        return 2;
    }
    if option.starts_with("--") {
        return 1;
    }
    let Some(letters) = option.strip_prefix('-') else {
        return 1;
    };
    let flags = FLAG_LETTERS
        .iter()
        .find(|(name, _)| *name == wrapper)
        .map_or("", |(_, flags)| *flags);
    for (at, letter) in letters.char_indices() {
        work::step();
        let takes_value = value_options.iter().any(|value_option| {
            work::step();
            value_option
                .strip_prefix('-')
                .is_some_and(|name| name.len() == letter.len_utf8() && name.starts_with(letter))
        });
        if takes_value {
            return 1 + usize::from(letters[at + letter.len_utf8()..].is_empty());
        }
        if !flags.contains(letter) {
            return 1;
        }
    }
    1
}

/// Lowercase basename of the command a simple command runs, skipping
/// assignments, shell keywords and wrappers (with their options and
/// arguments), with the index of that word.
pub(super) fn command_word(words: &[String]) -> Option<(usize, String)> {
    command_chain(words, &mut |_| {})
}

/// The positions of the programs a simple command runs: its wrappers
/// (`env`, `sudo`, `timeout`, ...) and then its command.
pub(super) fn programs(words: &[String]) -> Vec<usize> {
    let mut found = Vec::new();
    if let Some((index, _)) = command_chain(words, &mut |at| found.push(at)) {
        found.push(index);
    }
    found
}

/// `command_word`, telling `wrapper` the position of each wrapper skipped.
fn command_chain(words: &[String], wrapper: &mut dyn FnMut(usize)) -> Option<(usize, String)> {
    const KEYWORDS: &[&str] = &["!", "if", "then", "elif", "else", "do", "while", "until"];
    let mut index = 0;
    while let Some(word) = words.get(index) {
        work::step();
        // #1159: zsh `=git` runs git (`$(which git)`); any other `=word`
        // is read as before.
        let equals_git = equals_git(word);
        let name = if equals_git {
            "git".to_string()
        } else {
            basename(word).to_lowercase()
        };
        let assignment = word.contains('=') && !word.starts_with('-') && !equals_git;
        if assignment || KEYWORDS.contains(&name.as_str()) {
            index += 1;
            continue;
        }
        // A comment (`#!/bin/sh`, `# note`) is not a command. Its words are
        // still scanned, which can only over-match.
        if word.starts_with('#') {
            return None;
        }
        let Some((_, value_options, operands)) =
            WRAPPERS.iter().find(|(wrapper, _, _)| *wrapper == name)
        else {
            return Some((index, name));
        };
        wrapper(index);
        index += 1;
        while let Some(option) = words.get(index) {
            work::step();
            if option.starts_with('-') {
                index += option_words(&name, option, value_options);
            } else if option.contains('=') {
                index += 1;
            } else {
                break;
            }
        }
        index += *operands;
    }
    None
}

/// #1159: whether `word` is zsh's `=git` (EQUALS expansion: the `git` on the
/// path).
pub(super) fn equals_git(word: &str) -> bool {
    word.strip_prefix('=')
        .is_some_and(|rest| basename(rest).eq_ignore_ascii_case("git"))
}

/// #989: whether the command word holds a parameter expansion the lexer
/// misreads (`${c:-eval}`, `${!c}`, `${c}al`): its name is not the one the
/// words spell, so it is code the classifier cannot name. Such a word
/// counts as unseen wherever it could be a command or a wrapper; one that
/// assigns a variable (`A=${x:-y} cmd`) stays an assignment.
pub(super) fn misread_command(command: &SimpleCommand, words: &[String]) -> bool {
    if command.operated.is_empty() {
        return false;
    }
    let mut masked = words.to_vec();
    for at in &command.operated {
        work::step();
        if let Some(word) = masked.get_mut(*at)
            && command
                .words
                .get(*at)
                .is_none_or(|raw| super::arithmetic::assignment(raw).is_none())
        {
            *word = super::shell::UNSEEN_OUTPUT.to_string();
        }
    }
    command_word(&masked).is_some_and(|(at, _)| command.operated.contains(&at))
}

/// #1482 item 2: whether the word read as the command, in the words from
/// `from` on, is part of a redirection: its file (`>log ./run.sh`, `sudo
/// -R >log -C sub ./run.sh`), a descriptor (`2>log sh x`, `2>&1 ./x`,
/// `>&2 ./x`), a `{fd}` name (`{fd}>log ./x`) or a here-document's
/// delimiter (`<<EOF ./x`), while the words without those run a command
/// this reading would judge by more than its name: a program named by a
/// path, a shell or interpreter, `eval`, `source`, `.` or `trap` (`>log
/// eval ./x`), or a word starting with `-` (`&>log` is lexed as `&` and
/// `>log`, so `sudo &>log -u root ./x` leaves `-u root ./x`). The shell
/// removes the redirection's words, so that command is never judged.
/// `2>/dev/null git status`, `exec >out` and foreign text read as shell
/// (`<?php echo 1;`) are read as before. `from`: the operands of `eval`.
pub(super) fn redirect_hides_command(
    command: &SimpleCommand,
    words: &[String],
    from: usize,
) -> bool {
    let held = |at: usize| {
        command.writes_to.contains(&at)
            || command.redirect_words.contains(&at)
            || command.stdin_from == Some(at)
            || command.here_string == Some(at)
    };
    let Some(words) = words.get(from..) else {
        return false;
    };
    if !command_word(words).is_some_and(|(at, _)| held(from + at)) {
        return false;
    }
    // One pass over the words, once per simple command.
    let runs: Vec<String> = words
        .iter()
        .enumerate()
        .filter(|(at, _)| {
            work::step();
            !held(from + at)
        })
        .map(|(_, word)| word.clone())
        .collect();
    command_word(&runs).is_some_and(|(at, name)| {
        runs[at].contains('/')
            || runs[at].starts_with('-')
            || matches!(name.as_str(), "eval" | "source" | "." | "trap")
    }) || invocation(&runs).is_some()
}

/// #1482 item 4: whether a sudo among the programs of these words runs an
/// askpass helper or an editor named by the environment (`sudo -A`, `sudo
/// -e`, `--askpass`, `--edit`, `sudoedit`): code the words do not name.
/// sudo's own options are read up to its command, with their values (`sudo
/// -p A ./x` asks for nothing; `sudo -R sub -A ./x` runs a helper).
/// `sudo -i` (the target user's home) is a folder option of
/// `unknown_folder`; `sudo -s` runs `$SHELL` and is read as before (#1309).
pub(super) fn sudo_runs_helper(words: &[String]) -> bool {
    let options = WRAPPERS
        .iter()
        .find(|(wrapper, _, _)| *wrapper == "sudo")
        .map_or(&[][..], |(_, options, _)| *options);
    let values: String = options
        .iter()
        .filter_map(|option| option.strip_prefix('-').filter(|name| name.len() == 1))
        .chain(["R"])
        .collect();
    // Whether the option runs a helper, and whether its value is the next
    // word.
    let read = |option: &str| -> (bool, bool) {
        work::step();
        if let Some(long) = option.strip_prefix("--") {
            let (name, value) = long
                .split_once('=')
                .map_or((long, false), |(name, _)| (name, true));
            let helper =
                !name.is_empty() && ("askpass".starts_with(name) || "edit".starts_with(name));
            let takes = !value
                && !name.is_empty()
                && options.iter().any(|known| {
                    work::step();
                    known
                        .strip_prefix("--")
                        .is_some_and(|known| known.starts_with(name))
                });
            return (helper, takes);
        }
        let letters = option.strip_prefix('-').unwrap_or_default();
        for (at, letter) in letters.char_indices() {
            work::step();
            if matches!(letter, 'A' | 'e') {
                return (true, false);
            }
            // The rest of the word, or the next word, is this letter's value.
            if values.contains(letter) {
                return (false, letters[at + letter.len_utf8()..].is_empty());
            }
        }
        (false, false)
    };
    programs(words).into_iter().any(|at| {
        work::step();
        let name = basename(&words[at]).to_lowercase();
        if name == "sudoedit" {
            return true;
        }
        if name != "sudo" {
            return false;
        }
        let mut next = at + 1;
        while let Some(option) = words.get(next) {
            work::step();
            if option == "--" || !option.starts_with('-') || option == "-" {
                break;
            }
            let (helper, takes) = read(option);
            if helper {
                return true;
            }
            next += 1 + usize::from(takes);
        }
        false
    })
}

/// The invocation a simple command makes, if it runs a shell or interpreter.
pub(super) fn invocation(words: &[String]) -> Option<Invocation<'_>> {
    let (index, name) = command_word(words)?;
    invocation::parse(&name, &words[index + 1..])
}

/// Whether the command runs code it reads from its input: `eval`, a shell or
/// interpreter reading stdin, inline code that is missing from its words
/// (`xargs sh -c`) or given by a substitution, `parallel` without a command,
/// or a substitution used as the command itself (`$(curl ...)`).
fn executes_input(command: &SimpleCommand) -> bool {
    let Some((index, name)) = command_word(&command.words) else {
        return command.words.is_empty() && !command.fed_by.is_empty();
    };
    if name.starts_with('$')
        || name.contains(super::shell::UNSEEN_OUTPUT)
        || matches!(name.as_str(), "eval" | "trap")
    {
        return true;
    }
    if name == "parallel" {
        return command.words.len() == index + 1;
    }
    // `xargs sh -c %` and `parallel sh -c {}` put their input into the code.
    let fed_args = command.words[..index]
        .iter()
        .any(|word| matches!(basename(word), "xargs" | "parallel"));
    match invocation::parse(&name, &command.words[index + 1..]).map(|invocation| invocation.code) {
        Some(Code::Stdin | Code::Inline(None)) => true,
        Some(Code::Inline(Some(_))) if fed_args => true,
        Some(Code::Inline(Some(_))) => !command.fed_by.is_empty(),
        // `source <(curl ...)`, `bash <(curl ...)`: the script is a substitution.
        Some(Code::File(file)) if file.contains(super::shell::UNSEEN_OUTPUT) => {
            !command.fed_by.is_empty()
        }
        Some(Code::File(_) | Code::Module(_) | Code::Tool) | None => false,
    }
}

/// Whether a stage only prints text the classifier already sees.
fn literal_stage(command: &SimpleCommand) -> bool {
    super::shell::substitution_output(&command.words).is_some() && command.fed_by.is_empty()
}

/// Opaque execution: code fed to an executor through a pipe, a substitution
/// or `>(...)`, by any stage that is not a plain `echo`/`printf` of visible
/// text (a decoder, a download, `cat` of a file, a transform). The host
/// cannot describe what will run, so it counts as protected.
pub(super) fn opaque_execution(commands: &[SimpleCommand]) -> bool {
    commands.iter().any(|command| {
        if !executes_input(command) {
            return false;
        }
        if command.reads_outer_output {
            return true;
        }
        // Every command whose output reaches this one, transitively.
        let mut feeders: Vec<usize> = command.fed_by.clone();
        feeders.extend(command.pipe_from);
        let mut seen = std::collections::HashSet::new();
        while let Some(index) = feeders.pop() {
            work::step();
            if !seen.insert(index) {
                continue;
            }
            let Some(stage) = commands.get(index) else {
                continue;
            };
            if !literal_stage(stage) {
                return true;
            }
            feeders.extend(stage.fed_by.iter().copied());
            feeders.extend(stage.pipe_from);
        }
        false
    })
}

/// The added lines of each file in an `apply_patch` body.
pub(super) fn patch_additions(patch: &str) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = Vec::new();
    for line in patch.lines() {
        work::step();
        let header = ["*** Add File: ", "*** Update File: ", "*** Move to: "]
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix));
        if let Some(path) = header {
            files.push((path.trim().to_string(), String::new()));
            continue;
        }
        if let (Some(added), Some((_, body))) = (line.strip_prefix('+'), files.last_mut()) {
            body.push_str(added);
            body.push('\n');
        }
    }
    files
}

/// Whether a file (lowercase path) with these added lines is run later:
/// scripts, task files, hooks, shell start-up files, or anything starting
/// with a shebang.
pub(super) fn is_runnable_file(lower: &str, body: &str) -> bool {
    const SCRIPT_EXTENSIONS: &[&str] = &[
        "sh", "bash", "zsh", "fish", "ksh", "command", "py", "js", "mjs", "cjs", "ts", "rb", "pl",
        "php", "lua", "ps1", "bat", "cmd",
    ];
    const RUNNABLE_NAMES: &[&str] = &[
        "makefile",
        "justfile",
        "package.json",
        ".envrc",
        ".bashrc",
        ".zshrc",
        ".zshenv",
        ".profile",
        ".bash_profile",
        ".bash_login",
        ".zprofile",
    ];
    let name = basename(lower);
    let extension = name.rsplit_once('.').map(|(_, extension)| extension);
    body.starts_with("#!")
        || is_runnable_path(lower)
        || RUNNABLE_NAMES.contains(&name)
        || extension.is_some_and(|extension| SCRIPT_EXTENSIONS.contains(&extension))
}

/// Paths whose files run later whatever their name: git hooks and config
/// (`core.fsmonitor`, `core.hooksPath`), husky hooks, and extension-less
/// files in a `bin` folder.
fn is_runnable_path(lower: &str) -> bool {
    let name = basename(lower);
    lower.contains(".git/hooks/")
        || lower.ends_with(".git/config")
        || lower.ends_with(".gitconfig")
        || lower.ends_with(".config/git/config")
        || lower.contains(".husky/")
        || ((lower.starts_with("bin/") || lower.contains("/bin/")) && !name.contains('.'))
}

/// #932 measuring: why `opaque_execution` holds, as a short tag (the
/// executor's and the first opaque feeder's command names), for the local
/// measuring harness.
#[cfg(test)]
pub(super) fn opaque_execution_why(commands: &[SimpleCommand]) -> String {
    let name_of = |command: &SimpleCommand| -> String {
        command_word(&command.words).map_or_else(
            || "none".to_string(),
            |(_, name)| {
                if name.starts_with('$') {
                    "$var".to_string()
                } else if name.contains(super::shell::UNSEEN_OUTPUT) {
                    "$(output)".to_string()
                } else {
                    basename(&name).chars().take(24).collect()
                }
            },
        )
    };
    for command in commands.iter().filter(|command| executes_input(command)) {
        work::step();
        // The executor's own word, and the first after it: how its code is
        // given (`-`, `-c`, a file, none).
        let executor = command_word(&command.words).map_or_else(String::new, |(index, name)| {
            let next = command.words.get(index + 1).map_or("none", |word| {
                if word.starts_with('-') {
                    word.as_str()
                } else {
                    "operand"
                }
            });
            let name = if name.starts_with('$') {
                name
            } else {
                String::new()
            };
            format!(" word={name:?} next={next:?}")
        });
        if command.reads_outer_output {
            return format!("exec={} outer{executor}", name_of(command));
        }
        let mut feeders: Vec<usize> = command.fed_by.clone();
        feeders.extend(command.pipe_from);
        let mut seen = std::collections::HashSet::new();
        while let Some(index) = feeders.pop() {
            work::step();
            if !seen.insert(index) {
                continue;
            }
            let Some(stage) = commands.get(index) else {
                continue;
            };
            if !literal_stage(stage) {
                let link = if command.pipe_from == Some(index) {
                    "pipe"
                } else {
                    "subst"
                };
                return format!(
                    "exec={} {link} feeder={}{executor}",
                    name_of(command),
                    name_of(stage)
                );
            }
            feeders.extend(stage.fed_by.iter().copied());
            feeders.extend(stage.pipe_from);
        }
    }
    "-".to_string()
}
