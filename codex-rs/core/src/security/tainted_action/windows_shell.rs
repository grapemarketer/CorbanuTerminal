//! #1240: command text as cmd and PowerShell read it, for the classifier's
//! Windows view. The POSIX lexer removes backslashes (they escape in a POSIX
//! shell), so a Windows path in a cmd or PowerShell line (`type
//! C:\Users\me\.corbanu\auth.json`) reaches it as one meaningless word. This
//! pass reads the same text again with `\` kept: words split at blanks and
//! at cmd and PowerShell operators, double quotes (and, in one of two
//! readings, single quotes) grouping, `^` and `` ` `` escapes dropped. Every
//! word may be a path; it is resolved under Windows rules against the
//! working folder and every folder a `cd`/`Set-Location` in the text names.
//! A word after a writing command (`copy`, `Set-Content`, `Out-File`, ...),
//! a redirection (`>`, `>>`, `2>`) or a writing parameter (`-OutFile`) is a
//! write. Where unsure the pass over-approximates; it only adds findings.

use super::CLI_NAMES;
use super::CREDENTIAL_COMMANDS;
use super::Classifier;
use super::FOLDER_FLAGS;
use super::GitConfigWrites;
use super::LaterWords;
use super::MAX_FOLDER_BYTES;
use super::ProtectedActionKind;
use super::RECURSIVE_READERS;
use super::WRITERS;
use super::code_variables;
use super::git_helpers;
use super::has_flag;
use super::indirect;
use super::paths::glob_any;
use super::persistence_command;
use super::rerun;
use super::windows_paths;
use super::work;
use base64::Engine;

/// Folders a line may be in at once (and folder flags' values in one
/// command); twice as many it may have been in. Past them the line fails
/// closed.
const MAX_FOLDERS: usize = 16;
/// cmd and PowerShell commands (and aliases) that write, move or remove the
/// paths after them, besides the POSIX [`WRITERS`].
const WINDOWS_WRITERS: &[&str] = &[
    "copy",
    "xcopy",
    "robocopy",
    "move",
    "ren",
    "rename",
    "mklink",
    "del",
    "erase",
    "rd",
    "md",
    "replace",
    "attrib",
    "icacls",
    "takeown",
    "expand",
    "certutil",
    "bitsadmin",
    "set-content",
    "sc",
    "add-content",
    "ac",
    "out-file",
    "tee-object",
    "copy-item",
    "cpi",
    "move-item",
    "mi",
    "rename-item",
    "rni",
    "remove-item",
    "ri",
    "new-item",
    "ni",
    "clear-content",
    "clc",
    "set-item",
    "si",
    "export-csv",
    "export-clixml",
    "expand-archive",
    "compress-archive",
    "invoke-webrequest",
    "iwr",
    "invoke-restmethod",
    "irm",
    "start-bitstransfer",
    "set-acl",
];
/// .NET methods that write (`[IO.File]::WriteAllText(...)`), by prefix.
const WRITING_METHODS: &[&str] = &[
    "write", "append", "copy", "move", "create", "replace", "delete", "open", "set", "encrypt",
    "decrypt",
];
/// PowerShell parameters whose value is a file written. PowerShell takes
/// any unambiguous prefix of a parameter name.
const WRITING_PARAMETERS: &[&str] = &[
    "-outfile",
    "-destination",
    "-destinationpath",
    "-filepath",
    "-redirectstandardoutput",
    "-redirectstandarderror",
];
/// Commands that copy or archive a whole folder tree.
const WINDOWS_TREE_COPIERS: &[&str] = &["xcopy", "robocopy", "compress-archive"];
/// #1261 (D168): the names after `CurrentVersion` of the registry keys whose
/// values Windows runs at sign-in (the policy key is `Policies\Explorer\Run`).
const RUN_KEYS: &[&str] = &[
    "run",
    "runonce",
    "runonceex",
    "runservices",
    "runservicesonce",
];
/// Commands that only read a Run key they name.
const RUN_KEY_READERS: &[&str] = &[
    "get-itemproperty",
    "gp",
    "get-itempropertyvalue",
    "gpv",
    "get-item",
    "gi",
    "get-childitem",
    "gci",
    "dir",
    "ls",
    "test-path",
    "echo",
    "write-output",
    "write-host",
    "findstr",
    "select-string",
    "rg",
    "grep",
    "git",
];
/// Words that run the command after them, skipped (with their options) to
/// find the command itself (`cmd /c reg add ...`).
const LAUNCHERS: &[&str] = &["cmd", "powershell", "pwsh", "start", "call", "&"];
/// .NET registry methods that write (`[Microsoft.Win32.Registry]::SetValue`,
/// `.CreateSubKey(...)`).
const REGISTRY_METHODS: &[&str] = &["setvalue", "createsubkey", "deletevalue", "deletesubkey"];
/// `reg` operations that only read.
const REG_READS: &[&str] = &["query", "export", "compare", "/?", "-?"];
/// PowerShell commands (and aliases) that write registry values or keys,
/// besides [`WINDOWS_WRITERS`] and the POSIX [`WRITERS`].
const REGISTRY_WRITERS: &[&str] = &[
    "new-itemproperty",
    "set-itemproperty",
    "sp",
    "remove-itemproperty",
    "rp",
    "rename-itemproperty",
    "rnp",
    "copy-itemproperty",
    "cpp",
    "move-itemproperty",
    "mp",
    "clear-itemproperty",
    "clp",
    "clear-item",
    "cli",
];
/// PowerShell commands that register or change a scheduled task or job.
const TASK_SCHEDULERS: &[&str] = &[
    "register-scheduledtask",
    "set-scheduledtask",
    "enable-scheduledtask",
    "register-scheduledjob",
    "set-scheduledjob",
];
/// Commands that do so with a recursive flag.
const WINDOWS_RECURSIVE: &[&str] = &[
    "copy-item",
    "cpi",
    "copy",
    "select-string",
    "sls",
    "findstr",
];
/// Commands that change the folder later commands run in.
const CHANGE_FOLDER: &[&str] = &[
    "cd",
    "chdir",
    "set-location",
    "sl",
    "pushd",
    "push-location",
];

/// One command of cmd or PowerShell text: its words (quotes and escapes
/// removed) and which of them a redirection writes.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Command {
    pub(super) words: Vec<String>,
    pub(super) redirected: Vec<bool>,
    /// Whether a command separator (`&`, `|`, `;`, a line end) or the end
    /// of the text ended it, rather than a brace.
    pub(super) separated: bool,
}

/// How a reading takes quotes: grouping a word, or separating words.
#[derive(Clone, Copy)]
pub(super) struct Quotes {
    /// `'` groups a word (PowerShell); else it separates words (cmd keeps
    /// it as text; reading it as a separator keeps `echo don't > f` from
    /// hiding the redirection).
    pub(super) single: bool,
    /// `"` groups a word; else it separates words, so a command line
    /// quoted whole (`cmd /c "echo x > f"`) is read as one.
    pub(super) double: bool,
}

/// The readings each text gets.
const READINGS: [Quotes; 3] = [
    Quotes {
        single: true,
        double: true,
    },
    Quotes {
        single: false,
        double: true,
    },
    Quotes {
        single: false,
        double: false,
    },
];

/// The commands of `text`, read with `quotes`.
pub(super) fn commands(text: &str, quotes: Quotes) -> Vec<Command> {
    let chars: Vec<char> = text.chars().collect();
    let mut done = Vec::new();
    let mut current = Command::default();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut redirect = false;
    let flush = |word: &mut String, current: &mut Command, redirect: &mut bool| {
        if !word.is_empty() {
            current.words.push(std::mem::take(word));
            current.redirected.push(std::mem::take(redirect));
        }
    };
    let end =
        |done: &mut Vec<Command>, current: &mut Command, redirect: &mut bool, separated: bool| {
            *redirect = false;
            current.separated = separated;
            let command = std::mem::take(current);
            if !command.words.is_empty() {
                done.push(command);
            }
        };
    let mut at = 0;
    while at < chars.len() {
        work::step();
        let ch = chars[at];
        if let Some(open) = quote {
            if ch == open {
                quote = None;
            } else if ch != '`' {
                word.push(ch);
            }
            at += 1;
            continue;
        }
        match ch {
            '"' if quotes.double => quote = Some('"'),
            '\'' if quotes.single => quote = Some('\''),
            '`' | '^' => {}
            // A POSIX escaped `;` (`find ... -exec cmd {} \;`), not a path.
            '\\' if chars.get(at + 1) == Some(&';') => {
                word.push(';');
                at += 1;
            }
            '>' => {
                // A stream number (`2>`, `*>`) belongs to the operator.
                if word.chars().all(|ch| ch.is_ascii_digit() || ch == '*') {
                    word.clear();
                }
                flush(&mut word, &mut current, &mut redirect);
                if chars.get(at + 1) == Some(&'>') {
                    at += 1;
                }
                // `2>&1`: a stream, not a file.
                if chars.get(at + 1) == Some(&'&')
                    && chars.get(at + 2).is_some_and(char::is_ascii_digit)
                {
                    at += 2;
                } else {
                    redirect = true;
                }
            }
            '|' | '&' | ';' | '\n' | '\r' | '{' | '}' => {
                flush(&mut word, &mut current, &mut redirect);
                let separated = !matches!(ch, '{' | '}');
                end(&mut done, &mut current, &mut redirect, separated);
            }
            ch if ch.is_whitespace() || matches!(ch, ',' | '(' | ')' | '<' | '\'' | '"') => {
                flush(&mut word, &mut current, &mut redirect);
            }
            ch => word.push(ch),
        }
        at += 1;
    }
    flush(&mut word, &mut current, &mut redirect);
    end(
        &mut done,
        &mut current,
        &mut redirect,
        /*separated*/ true,
    );
    done
}

/// #1261 (D168): whether a cmd or PowerShell command installs something
/// Windows runs later: a scheduled task (`schtasks /create`,
/// `Register-ScheduledTask`, the task scheduler's COM object), a value of a
/// registry Run key (any command naming one, unless it is a reader with no
/// writer among its words), or a registry import the classifier cannot read
/// (`reg import`, `regedit`, `regini`), anywhere in the command. Words are
/// read by their blank-separated parts too (`cmd /c "reg add ..."`).
/// `lower` is the command's words, lowercase; `piped`: the text holds a
/// `|`, so a reader of a key may feed a writer.
fn windows_persistence(lower: &[String], piped: bool) -> bool {
    let tokens: Vec<&str> = lower
        .iter()
        .flat_map(|word| {
            work::step();
            word.split_whitespace()
        })
        .collect();
    // The command run: the first part past launchers, their options (and
    // option values) and a variable assignment (`$r = ...`).
    let mut command: Option<usize> = None;
    let mut skip_value = false;
    for (index, token) in tokens.iter().enumerate() {
        work::step();
        if skip_value {
            skip_value = false;
            continue;
        }
        if token.starts_with(['/', '-']) {
            skip_value = takes_a_value(token);
            continue;
        }
        if LAUNCHERS.contains(&command_name(token)) || token.starts_with('$') || *token == "=" {
            continue;
        }
        command = Some(index);
        break;
    }
    let name_at = |index: usize| {
        tokens.get(index).map_or("", |token| {
            command_name(token.trim_start_matches(['(', '$', '@', '{']))
        })
    };
    let command_name_run = command.map_or("", name_at);
    let mut schtasks = false;
    let mut changes_a_task = false;
    let mut writes = false;
    let mut names_a_run_key = false;
    for (index, token) in tokens.iter().enumerate() {
        work::step();
        let name = name_at(index);
        let previous = index.checked_sub(1).map_or("", name_at);
        let following = tokens.get(index + 1).copied().unwrap_or_default();
        changes_a_task |=
            schtasks && matches!(*token, "/create" | "-create" | "/change" | "-change");
        schtasks |= name == "schtasks";
        let scheduler = TASK_SCHEDULERS.contains(&name);
        // `Schedule.Service` or `Schedule.Service.1`, also attached
        // (`-ComObject:Schedule.Service`); not a file of that name.
        let progid = token
            .trim_matches(['\'', '"'])
            .rsplit(':')
            .next()
            .unwrap_or_default();
        let com_scheduler = matches!(progid, "schedule.service" | "schedule.service.1");
        let import = previous == "reg" && matches!(*token, "import" | "restore" | "load");
        let editor = matches!(name, "regedit" | "regini")
            && !following.is_empty()
            && !matches!(
                tokens.get(index.wrapping_sub(1)).copied(),
                Some("/im" | "-im" | "-name" | "-processname")
            );
        if scheduler || com_scheduler || import || editor {
            return true;
        }
        writes |= REGISTRY_WRITERS.contains(&name)
            || WINDOWS_WRITERS.contains(&name)
            || WRITERS.contains(&name)
            || (previous == "reg" && !REG_READS.contains(token))
            || REGISTRY_METHODS.iter().any(|method| token.contains(method))
            || token.split("::").skip(1).any(|method| {
                WRITING_METHODS
                    .iter()
                    .any(|prefix| method.starts_with(prefix))
            });
        names_a_run_key = names_a_run_key || run_key(token);
    }
    let reads = (RUN_KEY_READERS.contains(&command_name_run) || command_name_run == "reg")
        && !writes
        && !piped;
    changes_a_task || (names_a_run_key && !reads)
}

/// Whether `word` (lowercase) names a registry Run key: `...\CurrentVersion\`
/// and one of [`RUN_KEYS`], or `...\Policies\Explorer\Run`, with `\` or
/// `/`, `.` and `..` resolved. Once a segment names a hive (`HKCU`,
/// `HKEY_...`, `Registry::`) a wildcard name counts as any name it matches
/// (`CurrentVer*\R*`).
fn run_key(word: &str) -> bool {
    let mut hive = false;
    let mut segments: Vec<&str> = Vec::new();
    for segment in word.split(['/', '\\']) {
        work::step();
        let segment = segment.trim_matches(['\'', '"']);
        match segment {
            "" | "." => continue,
            ".." => {
                segments.pop();
                continue;
            }
            _ => {}
        }
        hive |= ["hkcu", "hklm", "hku", "hkey_", "registry::"]
            .iter()
            .any(|name| segment.starts_with(name));
        let same = |segment: &str, name: &str| segment == name || (hive && glob_any(segment, name));
        let one = segments.last().copied().unwrap_or_default();
        let two = segments
            .len()
            .checked_sub(2)
            .and_then(|at| segments.get(at))
            .copied()
            .unwrap_or_default();
        let after_current_version =
            same(one, "currentversion") && RUN_KEYS.iter().any(|key| same(segment, key));
        let policy = same(two, "policies") && same(one, "explorer") && same(segment, "run");
        if after_current_version || policy {
            return true;
        }
        segments.push(segment);
    }
    false
}

/// A command word's name: lowercase, without its folder or a Windows
/// program extension (`C:\Git\bin\git.exe` is `git`).
fn command_name(word: &str) -> &str {
    let base = word
        .trim_start_matches('@')
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(word);
    [".exe", ".com", ".cmd", ".bat"]
        .iter()
        .find_map(|extension| base.strip_suffix(extension))
        .unwrap_or(base)
}

/// The paths a word may name: itself, an attached value (`-Path:C:\x`,
/// `/OUT:x`, `f=@x`) and a `file:` URL's path.
fn candidates(word: &str) -> Vec<&str> {
    let mut found = vec![word];
    if (word.starts_with('-') || word.starts_with('/'))
        && let Some((_, value)) = word.split_once(':')
    {
        found.push(value);
    }
    found.extend(word.split('=').skip(1));
    if word
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
    {
        found.push(word[5..].trim_start_matches('/'));
    }
    // A PowerShell provider path (`FileSystem::C:\x`,
    // `Microsoft.PowerShell.Core\FileSystem::~\x`).
    if let Some(at) = word.to_ascii_lowercase().find("filesystem::") {
        found.push(&word[at + "filesystem::".len()..]);
    }
    found
        .into_iter()
        .map(|candidate| candidate.trim_start_matches('@'))
        .filter(|candidate| {
            !candidate.is_empty()
                && !(candidate.starts_with('-') && !candidate.contains(['/', '\\']))
        })
        .collect()
}

/// Words a join may take as operands (`Join-Path a b c`).
const MAX_JOINED: usize = 3;
/// Words after `New-Item` its `-Path` and `-Name` are looked for in.
const MAX_PARAMETER_WORDS: usize = 16;

/// Paths a command builds from several words, each with the position of
/// its first word: `Join-Path a b`, `[IO.Path]::Combine(a, b)`, `a + b`, and
/// `New-Item -Path a -Name b`.
fn joined_paths(words: &[String], lower: &[String], names: &[&str]) -> Vec<(usize, String)> {
    let operand = |word: &String| !word.starts_with('-');
    let value_of = |flag: &str, shortest: usize, from: usize| {
        (from..words.len().saturating_sub(1))
            .take(MAX_PARAMETER_WORDS)
            .find(|at| {
                lower[*at].len() >= shortest
                    && lower[*at].starts_with('-')
                    && flag.starts_with(&lower[*at])
            })
            .map(|at| at + 1)
    };
    let mut found = Vec::new();
    for at in 0..words.len() {
        work::step();
        if names[at] == "join-path" || lower[at].ends_with("::combine") {
            let operands: Vec<usize> = (at + 1..words.len())
                .take(2 * MAX_JOINED)
                .filter(|index| operand(&words[*index]))
                .take(MAX_JOINED)
                .collect();
            if let [first, ..] = operands[..]
                && operands.len() > 1
            {
                let parts: Vec<&str> = operands
                    .iter()
                    .map(|index| words[*index].as_str())
                    .collect();
                found.push((first, parts.join("\\")));
            }
        }
        if words[at] == "+" && at > 0 && at + 1 < words.len() {
            found.push((at - 1, format!("{}{}", words[at - 1], words[at + 1])));
        }
        if matches!(names[at], "new-item" | "ni")
            && let Some(name) = value_of("-name", 3, at + 1)
        {
            let path = value_of("-path", 2, at + 1).or_else(|| {
                (at + 1..words.len())
                    .take(MAX_PARAMETER_WORDS)
                    .find(|index| operand(&words[*index]) && *index != name)
            });
            if let Some(path) = path {
                found.push((path, format!("{}\\{}", words[path], words[name])));
            }
        }
    }
    found
}

/// Whether `word` (lowercase) is a PowerShell parameter naming a file
/// written.
fn writing_parameter(word: &str) -> bool {
    word.len() >= 4
        && word.starts_with('-')
        && WRITING_PARAMETERS
            .iter()
            .any(|parameter| parameter.starts_with(word))
}

/// Whether `word` (lowercase) asks for a recursive copy or search.
fn recursive_flag(word: &str) -> bool {
    matches!(
        word,
        "-r" | "--recursive" | "-a" | "--archive" | "/s" | "/e" | "/mir"
    ) || (word.len() >= 3 && "-recurse".starts_with(word))
}

/// The text of PowerShell's `-EncodedCommand` (base64 of UTF-16LE).
fn decoded_command(word: &str) -> Option<String> {
    // .NET's base64 ignores white space; invalid UTF-16 becomes U+FFFD.
    let word: String = word.chars().filter(|ch| !ch.is_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(word)
        .ok()?;
    if bytes.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    Some(String::from_utf16_lossy(&units))
}

/// Whether `word` (lowercase) may be one of PowerShell's encoded
/// parameters (`-EncodedCommand`, `-EncodedArguments` and the prefixes and
/// short names it accepts, also with `/`): any `-e…` parameter but
/// `-ExecutionPolicy`. Its value is decoded and judged, or fails closed.
fn encoded_parameter(word: &str) -> bool {
    let word = word
        .strip_prefix('/')
        .map_or(word.to_string(), |rest| format!("-{rest}"));
    word.starts_with("-e") && !(word.len() >= 3 && "-executionpolicy".starts_with(word.as_str()))
}

/// Whether `word` (lowercase) is a folder-changing command with its
/// folder attached (`cd..`, `cd\Users`, `chdir\x`): the folder.
fn attached_folder(word: &str) -> Option<&str> {
    ["chdir", "cd"].iter().find_map(|name| {
        word.strip_prefix(name)
            .filter(|rest| rest.starts_with(['.', '\\', '/']) && *rest != "/d")
    })
}

/// Bytes of words and folders one action's Windows view may resolve (each
/// word against each folder); past it the action fails closed. Many times
/// any line's length, it bounds the view's work whatever the folders.
pub(super) const RESOLVE_BUDGET: usize = 16 * super::MAX_CLASSIFIED_BYTES;

/// Words after a folder-changing command looked at for its folder.
const FOLDER_WINDOW: usize = 8;

/// Where a line may be: each folder it may be in now (a `cd` may have
/// failed or not run, so the folders before it stay possible), and every
/// folder it has been in (`popd` may return to any of them), normalized,
/// the working folder first.
pub(super) struct Folders {
    possible: Vec<String>,
    history: Vec<String>,
}

/// The folders a command names, each with the index of the word naming it
/// and whether the line moves there (`cd x`, `cd..`, `pushd x`,
/// `Set-Location -Path:x`, a bare `D:`, `start /D x` and `pwsh
/// -WorkingDirectory x` for what follows) or only the command runs there
/// (`git -C x`). A `cd` without a folder is PowerShell's move to the home.
fn folder_targets(
    words: &[String],
    lower: &[String],
    names: &[&str],
    separated: bool,
) -> Vec<(String, Option<usize>, bool)> {
    let mut found = Vec::new();
    let window = |from: usize| from..words.len().min(from + FOLDER_WINDOW);
    // cmd's `cd` takes the rest of its command, spaces included; read in
    // full for the command's first folder change (so once per command).
    let mut rest_read = false;
    for at in 0..words.len() {
        work::step();
        if let Some(target) = attached_folder(&lower[at]) {
            found.push((target.to_string(), Some(at), true));
        } else if CHANGE_FOLDER.contains(&names[at])
            || matches!(lower[at].as_str(), "cd/d" | "chdir/d")
        {
            // The first operand: `-Path:x` gives its value, other switches
            // (`/d`, `-Path`, `-PassThru`, `-StackName s`) are skipped; a
            // `-x\y` word is a folder (cmd has no switches with `\`).
            let mut operand = None;
            let mut skip = false;
            for next in window(at + 1) {
                work::step();
                let word = &words[next];
                if std::mem::take(&mut skip) {
                    continue;
                }
                let name = lower[next].split(':').next().unwrap_or_default();
                if takes_a_non_folder_value(name) {
                    // `-StackName s`, `-ErrorAction Stop`: not the folder.
                    skip = !lower[next].contains(':');
                    continue;
                } else if word.starts_with('-') && word.contains(':') {
                    operand = Some((word.split_once(':').map_or("", |(_, value)| value), next));
                } else if word.starts_with('-') && word.contains(['\\', '/']) {
                    operand = Some((word.as_str(), next));
                } else if word.starts_with('-') || lower[next] == "/d" {
                    continue;
                } else {
                    operand = Some((word.as_str(), next));
                }
                break;
            }
            match operand {
                Some((target, next)) => {
                    found.push((target.to_string(), Some(next), true));
                    let end = if std::mem::replace(&mut rest_read, true) {
                        words.len().min(next + FOLDER_WINDOW)
                    } else {
                        words.len()
                    };
                    // Marked `None`: only a spelling, skipped where it
                    // could not be a folder (longer than any cmd folder).
                    if end - next > 1 {
                        found.push((words[next..end].join(" "), None, true));
                    }
                }
                None => found.push(("~".to_string(), None, true)),
            }
        } else if names[at] == "start" {
            if let Some(next) = window(at + 1).find(|next| lower[*next] == "/d")
                && next + 1 < words.len()
            {
                found.push((words[next + 1].clone(), Some(next + 1), true));
            }
        } else if (lower[at] == "-wd"
            || (lower[at].len() >= 3 && "-workingdirectory".starts_with(lower[at].as_str())))
            && at + 1 < words.len()
        {
            found.push((words[at + 1].clone(), Some(at + 1), true));
        } else if FOLDER_FLAGS.contains(&words[at].as_str()) && at + 1 < words.len() {
            found.push((words[at + 1].clone(), Some(at + 1), false));
        } else if let Some((_, value)) = words[at]
            .split_once('=')
            .filter(|(flag, _)| FOLDER_FLAGS.contains(flag))
        {
            found.push((value.to_string(), Some(at), false));
        } else if words.len() == 1
            && separated
            && drive_relative(&words[at]).is_some_and(|(_, rest)| rest.is_empty())
        {
            // A bare drive (`D:`) moves to that drive's current folder.
            found.push((words[at].clone(), Some(at), true));
        }
    }
    found
}

/// Which words are PowerShell's own parameters (or their values): after
/// each `powershell`/`pwsh`, up to its code parameter (`-Command`) or the
/// first word that is neither a parameter nor a parameter's value (the
/// command itself: `powershell Get-Content a -Encoding utf8`).
fn powershell_parameters(names: &[&str], lower: &[String]) -> Vec<bool> {
    let mut parameters = vec![false; lower.len()];
    let mut in_parameters = false;
    for (index, name) in names.iter().enumerate() {
        work::step();
        let word = lower[index].as_str();
        if matches!(*name, "powershell" | "pwsh") {
            in_parameters = true;
        } else if in_parameters
            && (starts_the_code(word)
                || (!word.starts_with(['-', '/'])
                    && !(index > 0 && takes_a_value(&lower[index - 1]))))
        {
            in_parameters = false;
        } else {
            parameters[index] = in_parameters;
        }
    }
    parameters
}

/// Whether `word` (lowercase, before any `:`) is a parameter of
/// `Set-Location`/`Push-Location` whose value is not the folder:
/// `-StackName` and the common parameters, by the prefixes PowerShell
/// accepts.
fn takes_a_non_folder_value(word: &str) -> bool {
    matches!(
        word,
        "-s" | "-ea" | "-ev" | "-ov" | "-wa" | "-wv" | "-infa" | "-iv" | "-ob" | "-pv" | "-pi"
    ) || (word.len() >= 3 && "-stackname".starts_with(word))
        || (word.len() >= 4
            && [
                "-stackname",
                "-erroraction",
                "-errorvariable",
                "-outvariable",
                "-warningaction",
                "-warningvariable",
                "-informationaction",
                "-informationvariable",
                "-outbuffer",
                "-pipelinevariable",
                "-progressaction",
            ]
            .iter()
            .any(|name| name.starts_with(word)))
}

/// Whether `word` (lowercase) is one of PowerShell's own parameters that
/// takes a value (`-ExecutionPolicy Bypass`, `-WindowStyle Hidden`, the
/// encoded ones), by the prefixes it accepts, also with `/`.
fn takes_a_value(word: &str) -> bool {
    let word = word
        .strip_prefix('/')
        .map_or(word.to_string(), |rest| format!("-{rest}"));
    encoded_parameter(&word)
        || matches!(
            word.as_str(),
            "-ep" | "-w" | "-if" | "-of" | "-o" | "-v" | "-wd"
        )
        || (word.len() >= 3
            && [
                "-executionpolicy",
                "-windowstyle",
                "-inputformat",
                "-outputformat",
                "-configurationname",
                "-version",
                "-psconsolefile",
                "-workingdirectory",
                "-settingsfile",
                "-custompipename",
                "-configurationfile",
            ]
            .iter()
            .any(|name| name.starts_with(word.as_str())))
}

/// Whether `word` (lowercase) is PowerShell's parameter that ends its own
/// parameters and starts the code (`-Command`, `-c`, `-File`, `-f`,
/// `-CommandWithArgs`, also with `/`).
fn starts_the_code(word: &str) -> bool {
    let word = word
        .strip_prefix('/')
        .map_or(word.to_string(), |rest| format!("-{rest}"));
    word.len() >= 2
        && (["-command", "-file", "-commandwithargs"]
            .iter()
            .any(|name| name.starts_with(word.as_str()))
            || word == "-cwa")
}

/// Words of one command that name programs judged by name (the Corbanu
/// CLI, credential tools, schedulers, git); past it the command fails
/// closed, so judging each with the words after it stays linear.
const MAX_NAMED_PROGRAMS: usize = 8;

/// Whether `expanded` (a word with the known home variables expanded)
/// leaves an unknown variable by `..` (`%APPDATA%\..\..\.bashrc`): where it
/// lands cannot be known.
fn climbs_out_of_a_variable(expanded: &str) -> bool {
    let lower = expanded.to_lowercase();
    let start = [lower.find('%'), lower.find("$env:"), lower.find("${env:")]
        .into_iter()
        .flatten()
        .min();
    start.is_some_and(|start| {
        lower[start..].split(['/', '\\']).any(|segment| {
            segment.trim_end_matches(['.', ' ']).is_empty() && segment.contains("..")
        })
    })
}

/// The drive and the rest of a drive-relative path (`C:x`, `C:`), which is
/// relative to that drive's own current folder.
fn drive_relative(word: &str) -> Option<(char, &str)> {
    let mut chars = word.chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    let rest = &word[1..];
    let rest = rest.strip_prefix(':')?;
    (!rest.starts_with(['/', '\\', ':'])).then_some((letter, rest))
}

/// Whether `word` is relative to the current folder of a drive other than
/// one of `bases`' (normalized folders): that folder cannot be known.
fn on_another_drive(word: &str, bases: &[String]) -> bool {
    drive_relative(word).is_some_and(|(letter, _)| {
        bases.iter().any(|base| {
            !base
                .get(1..2)
                .is_some_and(|drive| drive.eq_ignore_ascii_case(&letter.to_string()))
        })
    })
}

impl Classifier {
    /// #1240: judge `command` (an argv, or a script as `sh -c text`) as cmd
    /// and PowerShell text, on its own and as one command line (Windows
    /// passes an argv to a program as one line; `cmd /c` and `powershell
    /// -Command` run the words after them as one). Each reading starts in
    /// the working folder.
    pub(super) fn windows_text(&mut self, command: &[String], cwd: &str, depth: usize) {
        let line = command
            .iter()
            .map(|arg| {
                work::step();
                if (arg.is_empty() || arg.contains(char::is_whitespace)) && !arg.contains('"') {
                    format!("\"{arg}\"")
                } else {
                    arg.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        if windows_paths::hides_a_home(&line) {
            self.note(ProtectedActionKind::UnseenCode);
        }
        let cwd_path = windows_paths::normalize(cwd);
        for text in std::iter::once(&line).chain(command) {
            work::step();
            let text = windows_paths::cmd_spellings(text);
            for quotes in READINGS {
                work::step();
                let mut folders = Folders {
                    possible: vec![cwd_path.clone()],
                    history: vec![cwd_path.clone()],
                };
                // #1261 (D168): a reader of a Run key may pipe it into a
                // writer (`Get-Item ...\Run | New-ItemProperty ...`).
                let piped = text.contains('|');
                for found in commands(&text, quotes) {
                    work::step();
                    self.windows_command(&found, &mut folders, depth, piped);
                }
            }
        }
    }

    /// One cmd or PowerShell command. Its words resolve against every
    /// folder the line has been in (a `cd` may not have run, `popd` may
    /// return) and its folder flags' values.
    /// `piped`: the text holds a `|`.
    fn windows_command(
        &mut self,
        command: &Command,
        folders: &mut Folders,
        depth: usize,
        piped: bool,
    ) {
        if self.windows_budget == 0 {
            self.note(ProtectedActionKind::UnseenCode);
            return;
        }
        let words = &command.words;
        let lower: Vec<String> = words.iter().map(|word| word.to_lowercase()).collect();
        let names: Vec<&str> = lower.iter().map(|word| command_name(word)).collect();
        // Folder values are where a command runs, not what it reads.
        let mut folder_values = vec![false; words.len()];
        let before = folders.possible.clone();
        let mut flagged: Vec<String> = Vec::new();
        if names
            .iter()
            .any(|name| matches!(*name, "popd" | "pop-location"))
        {
            // Back to any folder the line has been in.
            let history = folders.history.clone();
            self.add_folders(&mut folders.possible, history, MAX_FOLDERS);
        }
        for (target, at, moves) in folder_targets(words, &lower, &names, command.separated) {
            work::step();
            if let Some(at) = at {
                folder_values[at] = true;
            }
            // cmd's whole-rest spelling of a `cd` (marked by no index): one
            // that resolves past any folder's length names none (a word
            // `ci/cd` in a long message is no move); it does not fail closed.
            let spelling_only = at.is_none() && target != "~";
            // Each spelling of the folder (`-Path:x`, `a=b` kept whole too),
            // from each folder the line may be in.
            let mut reached = Vec::new();
            for target in candidates(&target) {
                work::step();
                if on_another_drive(target, &folders.possible) {
                    self.note(ProtectedActionKind::UnseenCode);
                }
                let cost = folders.possible.iter().map(String::len).sum::<usize>()
                    + target.len() * folders.possible.len();
                if !self.spend(cost) {
                    return;
                }
                // #1261 (D168): a start-up place's variable is also read as
                // its usual value (see `windows_path`).
                let places = windows_paths::names_a_place(target);
                if places && !self.spend(cost) {
                    return;
                }
                for folder in &folders.possible {
                    work::step();
                    let mut resolved = vec![self.homes.resolve(target, folder)];
                    if places {
                        resolved.push(self.homes.resolve_places(target, folder));
                    }
                    for resolved in resolved {
                        work::step();
                        if resolved.len() <= MAX_FOLDER_BYTES {
                            reached.push(resolved);
                        } else if !spelling_only {
                            self.note(ProtectedActionKind::UnseenCode);
                        }
                    }
                }
            }
            if !moves {
                self.add_folders(&mut flagged, reached, MAX_FOLDERS);
                continue;
            }
            // The folders before stay possible: the move may fail, may not
            // run (`if`, a block, `||`), or may not move (`cd D:\x`).
            self.add_folders(&mut folders.history, reached.clone(), 2 * MAX_FOLDERS);
            self.add_folders(&mut folders.possible, reached, MAX_FOLDERS);
        }
        let mut bases: Vec<String> = Vec::new();
        let cost = [&before, &folders.possible, &flagged]
            .iter()
            .flat_map(|folders| folders.iter())
            .map(String::len)
            .sum::<usize>();
        if !self.spend(cost) {
            return;
        }
        for base in [before, folders.possible.clone(), flagged] {
            work::step();
            self.add_folders(&mut bases, base, 3 * MAX_FOLDERS);
        }
        let bases = &bases;
        let current = bases.first().cloned().unwrap_or_default();
        let writer = names.iter().position(|name| {
            WINDOWS_WRITERS.contains(name)
                || WRITERS.contains(name)
                || name.rsplit_once("::").is_some_and(|(_, method)| {
                    WRITING_METHODS
                        .iter()
                        .any(|prefix| method.starts_with(prefix))
                })
        });
        // Flags looked for once per command, not once per command word.
        let recursive_flags = lower.iter().any(|word| recursive_flag(word));
        let readers_flagged: Vec<bool> = RECURSIVE_READERS
            .iter()
            .map(|(_, flags)| {
                work::step();
                flags.is_empty()
                    || lower
                        .iter()
                        .any(|word| flags.iter().any(|flag| has_flag(word, flag)))
            })
            .collect();
        let recursive = names.iter().any(|name| {
            WINDOWS_TREE_COPIERS.contains(name)
                || (recursive_flags && WINDOWS_RECURSIVE.contains(name))
                || RECURSIVE_READERS
                    .iter()
                    .zip(&readers_flagged)
                    .any(|((reader, _), flagged)| *flagged && reader == name)
        });
        // PowerShell's own parameters: after each `powershell`/`pwsh`,
        // before its code.
        let parameters = powershell_parameters(&names, &lower);
        // Words after a `powershell`/`pwsh` word.
        let mut powershell_seen = vec![false; words.len()];
        let mut seen = false;
        for (index, name) in names.iter().enumerate() {
            work::step();
            powershell_seen[index] = seen;
            seen |= matches!(*name, "powershell" | "pwsh");
        }
        let written = |index: usize| {
            command.redirected[index]
                || writer.is_some_and(|at| index > at)
                || (index > 0 && writing_parameter(&lower[index - 1]))
        };
        // #1261 (D168): scheduled tasks, Run keys and registry imports.
        if windows_persistence(&lower, piped) {
            self.note(ProtectedActionKind::Persistence);
        }
        let mut git_config = GitConfigWrites::new(&lower, &[], &[]);
        let mut later = LaterWords::new(&lower);
        let mut named_programs = 0;
        let mut git_programs_read = false;
        for (index, word) in words.iter().enumerate() {
            work::step();
            let name = names[index];
            // The checks the POSIX pass makes by command name, for names
            // only this reading sees (`C:\Tools\corbanu.exe vault list`).
            // #1515: `=git` too, where the git option scans stop.
            let equals_git = indirect::equals_git(&lower[index]);
            // #1528: a dashed helper by its Windows path (`git-grep.exe`)
            // names its subcommand. #1635: read from its command name, so
            // `@git-grep.exe` and `git-grep.cmd` name `grep` too.
            let helper = git_helpers::git_name(name).flatten();
            let named = CLI_NAMES.contains(&name)
                || name == "git"
                || equals_git
                || helper.is_some()
                || matches!(name, "crontab" | "launchctl" | "systemctl")
                || CREDENTIAL_COMMANDS
                    .iter()
                    .any(|(credential_command, _)| *credential_command == name);
            if named {
                named_programs += 1;
            }
            if named_programs > MAX_NAMED_PROGRAMS {
                self.note(ProtectedActionKind::UnseenCode);
            } else if named {
                let imported = index > 0 && matches!(lower[index - 1].as_str(), "from" | "import");
                if CLI_NAMES.contains(&name) && !imported {
                    self.cli(&mut later, index + 1);
                }
                if persistence_command(name, &mut later, index + 1)
                    || (name == "git" && git_config.persists(index + 1, /*subcommand*/ None))
                {
                    self.note(ProtectedActionKind::Persistence);
                }
                // #1516: and programs git runs (`grep -O`, `--upload-pack`),
                // read once per command, as the POSIX pass reads them. A
                // dashed helper takes no git options (#1528).
                let git = name == "git" || equals_git;
                if (git
                    && code_variables::git_runs_code_after_git(
                        &words[index + 1..],
                        &words[index + 1..],
                    ))
                    || ((git || helper.is_some())
                        && !std::mem::replace(&mut git_programs_read, true)
                        && rerun::git_runs_program(&words[index + 1..], helper.as_deref()))
                {
                    self.note(ProtectedActionKind::UnseenCode);
                }
                for (credential_command, verbs) in CREDENTIAL_COMMANDS {
                    work::step();
                    if name == *credential_command && later.holds(index + 1, verbs) {
                        self.note(ProtectedActionKind::Credentials);
                    }
                }
            }
            // An encoded parameter outside the parameters read (one of
            // PowerShell's this reading does not know may come before it)
            // is judged too when it decodes.
            let encoded_outside = powershell_seen[index]
                && index > 0
                && !parameters[index - 1]
                && encoded_parameter(&lower[index - 1]);
            if encoded_outside && decoded_command(word).is_none() {
                // `-Encoding utf8`: a parameter of the command, not encoded.
            } else if index > 0
                && (parameters[index - 1] || encoded_outside)
                && encoded_parameter(&lower[index - 1])
            {
                // Run from each folder the line may be in, once each.
                let decoded = decoded_command(word);
                for base in bases {
                    work::step();
                    if !self.windows_decoded.insert((word.clone(), base.clone())) {
                        continue;
                    }
                    match &decoded {
                        Some(text) => self.script(text, base, depth + 1),
                        None => self.note(ProtectedActionKind::UnseenCode),
                    }
                }
            }
            for candidate in candidates(word) {
                work::step();
                self.windows_path(
                    candidate,
                    bases,
                    &current,
                    written(index),
                    recursive && !folder_values[index],
                );
            }
        }
        for (index, path) in joined_paths(words, &lower, &names) {
            work::step();
            self.windows_path(&path, bases, &current, written(index), recursive);
        }
    }

    /// Spend `cost` bytes of the Windows view's work budget; past it the
    /// action fails closed and the view stops.
    fn spend(&mut self, cost: usize) -> bool {
        if self.windows_budget <= cost {
            self.windows_budget = 0;
            self.note(ProtectedActionKind::UnseenCode);
            return false;
        }
        self.windows_budget -= cost;
        true
    }

    /// Add `more` to `folders`, without repeats. Past `limit` folders the
    /// line fails closed.
    /// The bytes compared are charged to the budget; an overflow stops the
    /// view (the action has already failed closed).
    fn add_folders(&mut self, folders: &mut Vec<String>, more: Vec<String>, limit: usize) {
        let cost = folders.iter().chain(&more).map(String::len).sum::<usize>();
        if !self.spend(cost) {
            return;
        }
        let mut known: std::collections::HashSet<String> = folders.iter().cloned().collect();
        for folder in more {
            work::step();
            if !known.insert(folder.clone()) {
                continue;
            }
            if folders.len() >= limit {
                self.note(ProtectedActionKind::UnseenCode);
                self.windows_budget = 0;
                return;
            }
            folders.push(folder);
        }
    }

    /// One word that may be a path, resolved against each folder. A path
    /// whose place cannot be known fails closed: one on another drive's
    /// current folder (`D:x` from `C:\work`), or one climbing out of an
    /// unknown variable (`%APPDATA%\..\..\.bashrc`).
    fn windows_path(
        &mut self,
        candidate: &str,
        bases: &[String],
        cwd: &str,
        written: bool,
        recursive: bool,
    ) {
        let path_like =
            candidate.contains(['/', '\\']) || candidate.starts_with(['.', '~', '%', '$']);
        let expanded = windows_paths::expand(
            candidate,
            &windows_paths::Values {
                codex_home: self.homes.codex_home(),
                user_home: self.homes.user_home(),
                cwd,
                places: false,
            },
        );
        if climbs_out_of_a_variable(&expanded) {
            self.note(ProtectedActionKind::UnseenCode);
        }
        // Written, or spelled as a path (`D:.netrc`, `D:x\y`); not any
        // `x:y` word (`e:true:`, `a::b`).
        let spelled_as_path = drive_relative(&expanded).is_some_and(|(_, rest)| {
            written || rest.starts_with('.') || rest.contains(['/', '\\'])
        });
        if spelled_as_path && on_another_drive(&expanded, bases) {
            self.note(ProtectedActionKind::UnseenCode);
        }
        // #1261 (D168): a start-up place's variable is also read as its
        // usual value, besides as unknown.
        let places = windows_paths::names_a_place(candidate);
        for base in bases {
            work::step();
            if !self.spend(candidate.len() + base.len()) {
                return;
            }
            let resolved = self.homes.resolve(candidate, base);
            self.judge_windows_path(&resolved, path_like, written, recursive);
            if places {
                if !self.spend(candidate.len() + base.len()) {
                    return;
                }
                let resolved = self.homes.resolve_places(candidate, base);
                self.judge_windows_path(&resolved, path_like, written, recursive);
            }
        }
    }

    /// The findings for a path a word resolved to.
    fn judge_windows_path(
        &mut self,
        resolved: &str,
        path_like: bool,
        written: bool,
        recursive: bool,
    ) {
        if path_like {
            self.path(resolved);
        } else if let Some(kind) = self.homes.classify_path(resolved) {
            self.note(kind);
        }
        if written && self.homes.is_persistence(resolved) {
            self.note(ProtectedActionKind::Persistence);
        }
        if recursive && self.homes.holds_a_home(resolved) {
            self.note(ProtectedActionKind::Credentials);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn words(text: &str, single_quotes: bool) -> Vec<(Vec<&'static str>, Vec<bool>)> {
        commands(
            text,
            Quotes {
                single: single_quotes,
                double: true,
            },
        )
        .into_iter()
        .map(|command| {
            (
                command
                    .words
                    .into_iter()
                    .map(|word| &*Box::leak(word.into_boxed_str()))
                    .collect(),
                command.redirected,
            )
        })
        .collect()
    }

    #[test]
    fn issue_1240_windows_commands_split_as_cmd_and_powershell_do() {
        assert_eq!(
            words(
                r"type C:\Users\me\.corbanu\auth.json",
                /*single_quotes*/ true
            ),
            vec![(
                vec!["type", r"C:\Users\me\.corbanu\auth.json"],
                vec![false, false]
            )]
        );
        assert_eq!(
            words(
                r#"echo x >> "C:\Users\John Smith\.bashrc" 2>&1 & dir"#,
                /*single_quotes*/ true
            ),
            vec![
                (
                    vec!["echo", "x", r"C:\Users\John Smith\.bashrc"],
                    vec![false, false, true]
                ),
                (vec!["dir"], vec![false]),
            ]
        );
        assert_eq!(
            words(
                r"Set-Content -Path 'C:\a b\.zshrc' -Value x; [IO.File]::ReadAllText('C:\k')",
                /*single_quotes*/ true
            ),
            vec![
                (
                    vec!["Set-Content", "-Path", r"C:\a b\.zshrc", "-Value", "x"],
                    vec![false; 5]
                ),
                (vec!["[IO.File]::ReadAllText", r"C:\k"], vec![false, false]),
            ]
        );
        // cmd keeps `'` as text: read as a separator it cannot hide `>`.
        assert_eq!(
            words(r"echo don't > C:\x\.bashrc", /*single_quotes*/ false),
            vec![(
                vec!["echo", "don", "t", r"C:\x\.bashrc"],
                vec![false, false, false, true]
            )]
        );
        assert_eq!(
            words(
                r"ty^pe C:\x\.cor`banu\auth.json",
                /*single_quotes*/ true
            ),
            vec![(vec!["type", r"C:\x\.corbanu\auth.json"], vec![false, false])]
        );
        assert_eq!(command_name(r"c:\program files\git\bin\git.exe"), "git");
        assert_eq!(command_name("corbanu.cmd"), "corbanu");
        assert_eq!(candidates(r"-Path:C:\x"), vec![r"-Path:C:\x", r"C:\x"]);
        assert_eq!(candidates("f=@C:/k"), vec!["f=@C:/k", "C:/k"]);
        assert_eq!(
            candidates("file:///C:/Users/x/.ssh/id_rsa"),
            vec!["file:///C:/Users/x/.ssh/id_rsa", "C:/Users/x/.ssh/id_rsa"]
        );
        assert!(candidates("-Recurse").is_empty());
        assert!(writing_parameter("-outf"));
        assert!(!writing_parameter("-o"));
        assert!(recursive_flag("-rec"));
        let parameters = |line: &str| {
            let lower: Vec<String> = line.split(' ').map(str::to_string).collect();
            let names: Vec<&str> = lower.iter().map(|word| command_name(word)).collect();
            powershell_parameters(&names, &lower)
        };
        assert_eq!(
            parameters("powershell get-content a -encoding utf8"),
            vec![false; 5]
        );
        assert_eq!(
            parameters("pwsh -nop -ep bypass -enc x"),
            vec![false, true, true, true, true, true]
        );
        assert_eq!(
            parameters("pwsh -c pwsh -e x"),
            vec![false, false, false, true, true]
        );
        assert!(encoded_parameter("-enc"));
        assert!(encoded_parameter("/ec"));
        assert!(encoded_parameter("-encodedarguments"));
        assert!(!encoded_parameter("-executionpolicy"));
        assert!(!encoded_parameter("-ex"));
        assert_eq!(attached_folder(r"cd\users"), Some(r"\users"));
        assert_eq!(attached_folder("cd.."), Some(".."));
        assert_eq!(attached_folder("cdrom"), None);
        assert!(climbs_out_of_a_variable(r"%APPDATA%\..\..\.bashrc"));
        assert!(climbs_out_of_a_variable(r"$env:TEMP\..\.bashrc"));
        assert!(!climbs_out_of_a_variable(r"%TEMP%\out.txt"));
        assert!(!climbs_out_of_a_variable(r"..\%TEMP%"));
        assert_eq!(drive_relative("D:x"), Some(('D', "x")));
        assert_eq!(drive_relative("D:"), Some(('D', "")));
        assert_eq!(drive_relative(r"D:\x"), None);
        assert_eq!(drive_relative("a::b"), None);
        assert_eq!(drive_relative("ab:x"), None);
        let encoded = base64::engine::general_purpose::STANDARD.encode(
            "type x"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<u8>>(),
        );
        assert_eq!(decoded_command(&encoded).as_deref(), Some("type x"));
    }
}
