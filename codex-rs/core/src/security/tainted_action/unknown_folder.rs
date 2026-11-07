//! #1309: more ways the folder a nested script runs in becomes one the
//! quote-aware reading (`nested.rs`) cannot know, beyond `cd "$d"` and a
//! literal `-C` (#1283). Such a script's relative writes and output files
//! ask (`Classifier::unknown_place`).

use super::FOLDER_FLAGS;
use super::indirect;
use super::shell::SimpleCommand;
use super::shell::basename;
use super::work;

/// Long options that run a command in another folder (`sudo --chdir`,
/// `nsenter --wd`, `docker exec --workdir`, `systemd-run
/// --working-directory`, `fd --base-directory`). getopt takes any
/// unambiguous abbreviation (`env --chd`), so a prefix of one counts too.
const LONG_FOLDER_FLAGS: &[&str] = &[
    "--chdir",
    "--cwd",
    "--wdns",
    "--cd",
    "--directory",
    "--wd",
    "--workdir",
    "--work-dir",
    "--working-dir",
    "--working-directory",
    "--base-directory",
    // #1309: apptainer/singularity `--pwd`, buildah `--workingdir`.
    "--pwd",
    "--workingdir",
];

/// Short folder options of wrappers, also in a cluster (`env -iC dir`) or
/// with the folder attached (`sudo -D~`).
const SHORT_FOLDER_FLAGS: &[(&str, char)] = &[
    ("env", 'C'),
    ("sudo", 'D'),
    ("nsenter", 'w'),
    ("nsenter", 'W'),
    ("unshare", 'w'),
    ("docker", 'w'),
    ("podman", 'w'),
    ("nerdctl", 'w'),
    ("proot", 'w'),
    ("start-stop-daemon", 'd'),
    // #1309: `schroot -d`.
    ("schroot", 'd'),
    // #1309: login shells start in the target user's home: `sudo -i`,
    // `su -l`, `runuser -l`.
    ("sudo", 'i'),
    ("su", 'l'),
    ("runuser", 'l'),
    // #1309 follow-up: `run0 -D DIR` (also a keep option: `-D .`).
    ("run0", 'D'),
];

/// #1309: wrappers whose login spelling starts the command in the target
/// user's home (`su -`, `sudo --login`, `machinectl shell`), with the word
/// that does it: `-` alone, or for a long option a prefix of it at least
/// this long (getopt's abbreviations; sudo's `--l` may be `--list`).
const LOGIN_WORDS: &[(&str, &str, usize)] = &[
    ("su", "-", 1),
    ("su", "--login", 3),
    ("runuser", "-", 1),
    ("runuser", "--login", 3),
    ("sudo", "--login", 4),
    ("machinectl", "shell", 5),
];

/// A program of `ELSEWHERE_UNLESS`: its name, keep options, options that
/// take a value, and options known to take none.
type KeepOptions = (
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
    &'static [&'static str],
);

/// #1309: programs that start what they run in another folder unless an
/// option keeps the caller's: `pkexec` (the target user's home, unless
/// `--keep-cwd`), `systemd-run` (a service starts in `/`, or a user
/// service in home, unless `--scope`, `-d` or `--same-dir`), `run0`
/// (unless `--chdir`/`-D`); with the options of each that take a value,
/// and those known to take none.
const ELSEWHERE_UNLESS: &[KeepOptions] = &[
    (
        "pkexec",
        &["--keep-cwd"],
        &["--user"],
        &["--disable-internal-agent", "--help", "--version"],
    ),
    (
        "systemd-run",
        &["--scope", "-d", "--same-dir"],
        &[
            "-u",
            "--unit",
            "-p",
            "--property",
            "--description",
            "--slice",
            "-E",
            "--setenv",
            "-H",
            "--host",
            "-M",
            "--machine",
            "--uid",
            "--gid",
            "--nice",
        ],
        &[
            "--user",
            "--system",
            "-r",
            "--remain-after-exit",
            "--send-sighup",
            "-G",
            "--collect",
            "--wait",
            "-P",
            "--pipe",
            "-t",
            "--pty",
            "-q",
            "--quiet",
            "--no-block",
            "-S",
            "--shell",
            "--no-ask-password",
            "--slice-inherit",
            "--on-clock-change",
            "--on-timezone-change",
            "-h",
            "--help",
            "--version",
        ],
    ),
    (
        "run0",
        &["--chdir", "-D"],
        &["-u", "--user", "-g", "--group", "--unit", "--nice"],
        &[
            "--no-ask-password",
            "--slice-inherit",
            "--pty",
            "--pipe",
            "-h",
            "--help",
            "--version",
        ],
    ),
];

/// Options read for a keep option; past this many the program counts as
/// elsewhere.
const MAX_KEEP_OPTIONS: usize = 64;

/// Whether the options of an `ELSEWHERE_UNLESS` program (`options`, the
/// words after it) hold one of `keeps` before its command (`pkexec sh -c
/// '…' --keep-cwd` gives `sh` the option). Only its `valued` options take
/// the next word as their value; any other is read as a flag, so a value
/// it takes ends the options and the program counts as elsewhere.
/// #1309 follow-up: an option neither `valued` nor among its `flags` may
/// take the next word as its value, so a keep option there does not count
/// (`systemd-run --on-active -d`).
fn kept(options: &[String], keeps: &[&str], valued: &[&str], flags: &[&str]) -> bool {
    let mut value_next = false;
    let mut maybe_value = false;
    for option in options.iter().take(MAX_KEEP_OPTIONS) {
        work::step();
        if std::mem::take(&mut value_next) {
            continue;
        }
        let after_unknown = std::mem::take(&mut maybe_value);
        if !option.starts_with('-') || option == "--" {
            return false;
        }
        let flag = option
            .split_once('=')
            .map_or(option.as_str(), |(flag, _)| flag);
        if keeps.contains(&flag) && !after_unknown {
            return true;
        }
        value_next = !option.contains('=') && valued.contains(&flag);
        maybe_value = !option.contains('=')
            && !valued.contains(&flag)
            && !flags.contains(&flag)
            && !keeps.contains(&flag);
    }
    false
}

/// Commands that run what they are given in another folder whatever their
/// options.
const ELSEWHERE: &[&str] = &["chroot"];

/// Options that run a command in the folder of each file found.
const IN_FOUND_FOLDER: &[&str] = &["-execdir", "-okdir"];

/// Words that start a loop: a later iteration runs its first commands in
/// the folder the earlier one left.
const LOOPS: &[&str] = &["for", "while", "until", "select", "repeat", "foreach"];

/// Words that define code run later than it is read: functions, traps and
/// aliases.
const DEFINERS: &[&str] = &["function", "trap", "alias"];

/// Commands that run text in the current shell, so a `cd` in it moves the
/// line (`eval 'cd ~'`, `alias go='cd ~'`).
const CARRIERS: &[&str] = &["eval", "alias", "trap", "bind", "fc"];

/// Folder changes.
pub(super) const CHANGERS: &[&str] = &["cd", "pushd", "popd", "chdir"];

/// Names that change where `cd` goes (`CDPATH`, zsh `cdpath`, bash
/// `cdable_vars`, `OLDPWD` for `cd -`, zsh `AUTO_CD`), lowercased with
/// underscores removed (zsh option spelling).
const SEARCHES: &[&str] = &["cdpath", "cdablevars", "oldpwd", "autocd"];

/// sudo's short options that take no value and run nothing else (not
/// `-A`, `-e`, `-s`: an askpass helper, an editor, `$SHELL`) nor move the
/// command (`-i`: to the target user's home).
const SUDO_LETTERS: &str = "bBEHkKlnNPSVv";

/// sudo's long options that take no value (`--preserve-env` may have one
/// attached).
const SUDO_LONG: &[&str] = &[
    "background",
    "bell",
    "non-interactive",
    "preserve-env",
    "preserve-groups",
    "remove-timestamp",
    "reset-timestamp",
    "set-home",
    "stdin",
];

/// sudo by the names it is installed under, besides a bare `sudo`.
const SUDO_PATHS: &[&str] = &[
    "/usr/bin/sudo",
    "/bin/sudo",
    "/usr/sbin/sudo",
    "/run/wrappers/bin/sudo",
];

/// Names that may make `sudo` run something other than sudo: a function,
/// alias or trap (also zsh `functions[sudo]=`, PowerShell `Set-Alias`),
/// the command hash (bash `hash -p`, `BASH_CMDS`, zsh `commands[sudo]=`),
/// the search path, loadable builtins, start-up files and code loaded or
/// run from elsewhere.
const SUDO_REDEFINERS: &[&str] = &[
    "trap",
    "hash",
    "unhash",
    "bash_cmds",
    "commands",
    "path",
    "fpath",
    "enable",
    "disable",
    "autoload",
    "zmodload",
    "emulate",
    "source",
    "eval",
    "bash_env",
    "env",
    "zdotdir",
    "rcfile",
    "init",
    "sal",
    "nal",
];

/// D217: the only characters a text may hold for `sudo` to be read as
/// sudo: none that the shell expands or removes (quotes, backslashes, `$`,
/// `~`, patterns, braces, `=word` aside), groups, redirections or comments,
/// so the words judged are the words that run and the names
/// [`SUDO_REDEFINERS`] lists cannot be spelled another way.
fn plain_for_sudo(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(
            ch,
            ' ' | '\t' | '\n' | '-' | '_' | '.' | '/' | '=' | ';' | '&' | '|'
        )
}

/// D217: whether `text`, a text the action holds or reads, may make `sudo`
/// other than sudo (see [`SUDO_REDEFINERS`]), imports a function
/// (`BASH_FUNC_sudo%%`) or sources a file (`. ./f`), or is not plain
/// ([`plain_for_sudo`]).
pub(super) fn may_redefine_sudo(text: &str) -> bool {
    !text.chars().all(|ch| {
        work::step();
        plain_for_sudo(ch)
    }) || defines(text)
        || tokens(text).any(|token| {
            work::step();
            let token = token.to_ascii_lowercase();
            token.contains("function")
                || token.contains("alias")
                || token.contains("bash_func")
                || SUDO_REDEFINERS.contains(&token.as_str())
        })
        || text
            .split(|ch: char| ch.is_whitespace() || ";&|(){}`".contains(ch))
            .any(|word| {
                work::step();
                word == "."
            })
}

/// D217 (#1403): the positions of `words` that are sudo's own `-C N`
/// (`-C N`, `-CN`, `-nC N`, N a number): it closes file descriptors from N
/// and names no folder. Only sudo as the command's first word (no
/// assignment before it), by a bare `sudo` or one of [`SUDO_PATHS`],
/// counts, and only options before it that take no value
/// ([`SUDO_LETTERS`], [`SUDO_LONG`]): any other word ends them, so any
/// other `-C` still reads as a folder. `redefined`: the action may make `sudo` other than sudo
/// ([`may_redefine_sudo`]), or Windows path rules apply: none counts.
/// Sorted.
pub(super) fn sudo_close_from(words: &[String], redefined: bool) -> Vec<usize> {
    let mut closes = Vec::new();
    if redefined {
        return closes;
    }
    let Some(word) = words.first() else {
        return closes;
    };
    if !(word == "sudo" || SUDO_PATHS.contains(&word.as_str())) {
        return closes;
    }
    let number = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    let mut index = 1;
    while let Some(word) = words.get(index) {
        work::step();
        if let Some(long) = word.strip_prefix("--") {
            if !SUDO_LONG.contains(&long) && !long.starts_with("preserve-env=") {
                break;
            }
            index += 1;
            continue;
        }
        let Some(letters) = word.strip_prefix('-').filter(|letters| !letters.is_empty()) else {
            break;
        };
        let Some(close) = letters.find(|letter: char| !SUDO_LETTERS.contains(letter)) else {
            index += 1;
            continue;
        };
        if !letters[close..].starts_with('C') {
            break;
        }
        // The value is the rest of the word, or the next word.
        let attached = &letters[close + 1..];
        let value = if attached.is_empty() {
            words.get(index + 1).map_or("", String::as_str)
        } else {
            attached
        };
        if !number(value) {
            break;
        }
        closes.push(index);
        index += 1 + usize::from(attached.is_empty());
    }
    closes
}

/// Where a command with these words starts to run the scripts it is given
/// in a folder other than the line's: a wrapper's folder option in any
/// spelling, `find -execdir`, `chroot` or a systemd `WorkingDirectory=`.
/// `Some`: the position of the first of `texts` that makes it so; it holds
/// for the texts before a position after it, and only those. `None`: none
/// does.
/// `redefined`: see [`sudo_close_from`].
pub(super) fn elsewhere_at(texts: &[String], redefined: bool) -> Option<usize> {
    let mut tmux = Tmux::default();
    let mut letters: Vec<char> = Vec::new();
    let mut logins: Vec<(&str, usize)> = Vec::new();
    let closes = sudo_close_from(texts, redefined);
    for (at, text) in texts.iter().enumerate() {
        work::step();
        let flag = text.split_once('=').map_or(text.as_str(), |(flag, _)| flag);
        // D217: sudo's `-C N` names no folder.
        let close = closes.binary_search(&at).is_ok();
        if (!close && FOLDER_FLAGS.contains(&flag))
            // `env -C"$d"`: the folder attached.
            || (!close && text.starts_with("-C"))
            || IN_FOUND_FOLDER.contains(&text.as_str())
            || ELSEWHERE.contains(&basename(text))
            || (flag.len() > 2
                && flag.starts_with("--")
                && LONG_FOLDER_FLAGS.iter().any(|long| {
                    work::step();
                    long.starts_with(flag)
                }))
            || text.as_bytes().windows(16).any(|window| {
                work::step();
                window.eq_ignore_ascii_case(b"workingdirectory")
            })
        {
            return Some(at);
        }
        if text.len() > 1
            && text.starts_with('-')
            && !text.starts_with("--")
            && text[1..].chars().any(|letter| {
                work::step();
                letters.contains(&letter)
            })
        {
            return Some(at);
        }
        if logins.iter().any(|(word, shortest)| {
            work::step();
            login_word(text, word, *shortest)
        }) {
            return Some(at);
        }
        if let Some((_, keeps, valued, flags)) = ELSEWHERE_UNLESS
            .iter()
            .find(|(program, _, _, _)| basename(text) == *program)
            && !kept(&texts[at + 1..], keeps, valued, flags)
        {
            return Some(at);
        }
        // #1309 follow-up: tmux's `new-session`/`new-window`/`split-window`
        // /`respawn-*` `-c DIR` (`display-popup -d DIR`) runs its command
        // there.
        if tmux.option(text) {
            return Some(at);
        }
        tmux.after(text);
        for (wrapper, letter) in SHORT_FOLDER_FLAGS {
            work::step();
            if basename(text) == *wrapper && !letters.contains(letter) {
                letters.push(*letter);
            }
        }
        for (wrapper, word, shortest) in LOGIN_WORDS {
            work::step();
            if basename(text) == *wrapper && !logins.contains(&(*word, *shortest)) {
                logins.push((word, *shortest));
            }
        }
    }
    None
}

/// #1309 follow-up: tmux commands that run a command in the folder an
/// option names: the command (and its alias), that option's letter, and
/// the letters of its options that take a value.
const TMUX_FOLDERS: &[(&str, &str, char, &str)] = &[
    ("new-session", "new", 'c', "ceFfnstxy"),
    ("new-window", "neww", 'c', "ceFnt"),
    ("split-window", "splitw", 'c', "ceFlpt"),
    ("new-pane", "newp", 'c', "ceFlpt"),
    ("respawn-pane", "respawnp", 'c', "cet"),
    ("respawn-window", "respawnw", 'c', "cet"),
    ("run-shell", "run", 'c', "cdt"),
    ("display-popup", "popup", 'd', "bcdehsStTwxy"),
];

/// Shortest prefix of a tmux command name read as that command (tmux
/// takes any unambiguous one: `new-w`, `split-w`); a shorter or ambiguous
/// one only over-matches.
const TMUX_PREFIX: usize = 3;

/// The `TMUX_FOLDERS` entry `word` names: its alias, or a prefix of its
/// name; not `display`, `display-message`'s alias.
fn tmux_folder_command(
    word: &str,
) -> Option<&'static (&'static str, &'static str, char, &'static str)> {
    TMUX_FOLDERS.iter().find(|(name, alias, _, _)| {
        work::step();
        word == *alias || (word.len() >= TMUX_PREFIX && word != "display" && name.starts_with(word))
    })
}

/// tmux's options that take a value, before its command.
const TMUX_VALUED: &str = "cfLST";

/// #1309 follow-up: the shell commands tmux at `at` in `texts` starts: the
/// words after each of `TMUX_FOLDERS`' options, to a `;`, joined into one
/// line, and the value of tmux's own `-c`; each with the word it starts
/// at. Past `outbound::MAX_JUDGED_POSITIONS` words, the rest is not read.
pub(super) fn tmux_commands(texts: &[String], at: usize) -> Vec<(usize, String)> {
    let end = texts
        .len()
        .min(at + 1 + super::outbound::MAX_JUDGED_POSITIONS);
    let mut commands = Vec::new();
    let mut index = at + 1;
    // tmux's own options; `-c` runs a shell command.
    while index < end && texts[index].starts_with('-') && texts[index].len() > 1 {
        work::step();
        let word = &texts[index];
        index += 1;
        if word == "--" {
            break;
        }
        for (offset, letter) in word.char_indices().skip(1) {
            work::step();
            if !TMUX_VALUED.contains(letter) {
                continue;
            }
            let rest = &word[offset + 1..];
            let value = if rest.is_empty() {
                index += 1;
                (index - 1, texts.get(index - 1).cloned())
            } else {
                (index - 1, Some(rest.to_string()))
            };
            if letter == 'c'
                && let (holder, Some(value)) = value
            {
                commands.push((holder, value));
            }
            break;
        }
    }
    while index < end {
        work::step();
        let word = &texts[index];
        index += 1;
        let Some((_, _, _, valued)) = tmux_folder_command(word) else {
            continue;
        };
        while index < end && texts[index].starts_with('-') && texts[index].len() > 1 {
            work::step();
            let option = &texts[index];
            index += 1;
            if option == "--" {
                break;
            }
            if let Some((offset, letter)) = option.char_indices().skip(1).find(|(_, letter)| {
                work::step();
                valued.contains(*letter)
            }) {
                index += usize::from(offset + letter.len_utf8() == option.len());
            }
        }
        index = index.min(end);
        let start = index;
        while index < end && texts[index] != ";" {
            work::step();
            index += 1;
        }
        if start < index {
            commands.push((start, texts[start..index].join(" ")));
        }
    }
    commands
}

/// Where `elsewhere_at` is in a tmux line: after `tmux`, and in the
/// options of one of `TMUX_FOLDERS`.
#[derive(Default)]
struct Tmux {
    seen: bool,
    /// The folder letter and valued letters of the command whose options
    /// are being read.
    options: Option<(char, &'static str)>,
    /// The next word is an option's value.
    value_next: bool,
}

impl Tmux {
    /// Whether `text`, read in a tmux command's options, names its folder.
    fn option(&mut self, text: &str) -> bool {
        let Some((folder, valued)) = self.options else {
            return false;
        };
        if std::mem::take(&mut self.value_next) {
            return false;
        }
        let Some(group) = text
            .strip_prefix('-')
            .filter(|group| !group.is_empty() && !group.starts_with('-'))
        else {
            self.options = None;
            return false;
        };
        for (index, letter) in group.char_indices() {
            work::step();
            if letter == folder {
                return true;
            }
            if valued.contains(letter) {
                self.value_next = index + letter.len_utf8() == group.len();
                break;
            }
        }
        false
    }

    /// Notes `text` as a word read: `tmux`, or one of `TMUX_FOLDERS` after it.
    fn after(&mut self, text: &str) {
        if self.options.is_some() {
            return;
        }
        if basename(text).eq_ignore_ascii_case("tmux") {
            self.seen = true;
        } else if self.seen
            && let Some((_, _, folder, valued)) = tmux_folder_command(text)
        {
            self.options = Some((*folder, valued));
        }
    }
}

/// Whether `text` is `word` (a login spelling), or for a long option an
/// abbreviation of it at least `shortest` long (`su --l`, `sudo --log`).
fn login_word(text: &str, word: &str, shortest: usize) -> bool {
    let flag = text.split_once('=').map_or(text, |(flag, _)| flag);
    text == word || (word.starts_with("--") && flag.len() >= shortest && word.starts_with(flag))
}

/// Whether `text` defines code that runs later than it is read: a
/// function (`f() { ...; }`, `function f`), a trap or an alias.
pub(super) fn defines(text: &str) -> bool {
    // `f()`, `f ( )`.
    let mut open = false;
    for ch in text.chars() {
        work::step();
        match ch {
            '(' => open = true,
            ')' if open => return true,
            ch if ch.is_whitespace() => {}
            _ => open = false,
        }
    }
    tokens(text).any(|token| DEFINERS.contains(&token))
}

/// Whether a command whose words are `texts` may start a loop: the
/// reading keeps reserved words before it (`then for ...`, `do while
/// ...`), so any word counts.
pub(super) fn starts_loop(texts: &[String]) -> bool {
    texts.iter().any(|text| {
        work::step();
        LOOPS.contains(&text.as_str())
    })
}

/// #1436: a loop that holds a folder change.
pub(super) struct ChangingLoop {
    /// Its first and last command.
    pub(super) start: usize,
    pub(super) end: usize,
    /// The variables its changes name (`cd "$D"`, `cd ${D}/x`).
    pub(super) names: Vec<String>,
    /// A word in it climbs (`..`).
    pub(super) climbs: bool,
    /// A change in it goes back (`cd -`, `~-`) or uses the folder stack
    /// (`pushd`, `popd`).
    pub(super) back: bool,
    pub(super) stack: bool,
}

/// #1436: the outermost loops of `commands` that hold a folder change
/// (`cd`, `eval 'cd ~'`, `source f`). A loop starts at a command whose
/// first word, past keywords (`do for`), is a loop keyword, and ends at a
/// `done`; one left open ends with the text.
pub(super) fn changing_loops(commands: &[SimpleCommand]) -> Vec<ChangingLoop> {
    let mut found = Vec::new();
    let mut open = 0usize;
    let mut start = 0;
    let mut changes = false;
    let mut names = Vec::new();
    let mut climbs = false;
    let mut back = false;
    let mut stack = false;
    for (at, command) in commands.iter().enumerate() {
        work::step();
        let words = &command.words;
        let first = words.iter().find(|word| {
            work::step();
            !matches!(
                word.as_str(),
                "do" | "then" | "else" | "elif" | "if" | "!" | "{" | "time"
            )
        });
        if first.is_some_and(|word| LOOPS.contains(&word.as_str())) {
            if open == 0 {
                start = at;
                changes = false;
                names.clear();
                climbs = false;
                back = false;
                stack = false;
            }
            open += 1;
        }
        if open > 0 {
            climbs |= words.iter().any(|word| word.contains(".."));
            if let Some((index, name)) = indirect::command_word(words)
                && (CHANGERS.contains(&name.as_str()) || carries_change(&name, &words[index + 1..]))
            {
                changes = true;
                stack |= matches!(name.as_str(), "pushd" | "popd");
                for word in &words[index + 1..] {
                    work::step();
                    back |= word == "-" || word.starts_with("~-");
                    names.extend(variable_names(word));
                }
            }
        }
        if open > 0 && words.first().is_some_and(|word| word == "done") {
            open -= 1;
            if open == 0 && changes {
                found.push(ChangingLoop {
                    start,
                    end: at,
                    names: std::mem::take(&mut names),
                    climbs,
                    back,
                    stack,
                });
            }
        }
    }
    if open > 0 && changes {
        found.push(ChangingLoop {
            start,
            end: commands.len() - 1,
            names,
            climbs,
            back,
            stack,
        });
    }
    found
}

/// The names of the variables `word` expands (`$D`, `${D}`, `${D:-x}`).
fn variable_names(word: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = word;
    while let Some(at) = rest.find('$') {
        work::step();
        rest = &rest[at + 1..];
        let inner = rest.strip_prefix('{').unwrap_or(rest);
        let end = inner
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .unwrap_or(inner.len());
        if end > 0 {
            names.push(inner[..end].to_string());
        }
    }
    names
}

/// #1436: whether a simple command (`words`, its command word `command`)
/// sets a name that changes where `cd` goes ([`searches`]): an assignment
/// on its own, declared, or before a folder change; a name it reads into
/// (`read CDPATH`); or an option (`shopt -s cdable_vars`, zsh `setopt
/// auto_cd`).
pub(super) fn sets_search(words: &[String], command: Option<&(usize, String)>) -> bool {
    let name = command.map(|(_, name)| name.as_str());
    let named = matches!(
        name,
        Some("shopt" | "setopt" | "set" | "read" | "mapfile" | "readarray" | "printf")
    );
    let assigns = matches!(
        name,
        None | Some(
            "export" | "declare" | "typeset" | "local" | "readonly" | "cd" | "pushd" | "popd"
        )
    );
    words.iter().any(|word| {
        work::step();
        if named {
            return searches(word);
        }
        assigns
            && word.split_once('=').is_some_and(|(assigned, _)| {
                let assigned = assigned.trim_end_matches('+');
                searches(assigned.split('[').next().unwrap_or(assigned))
            })
    })
}

/// Whether `text` names something that changes where `cd` goes.
pub(super) fn searches(text: &str) -> bool {
    if text == "HOME" || text.starts_with("HOME=") || text.starts_with("HOME+=") {
        return true;
    }
    let mut normal = String::with_capacity(text.len());
    for ch in text.chars() {
        work::step();
        if ch != '_' {
            normal.push(ch.to_ascii_lowercase());
        }
    }
    SEARCHES.iter().any(|name| {
        work::step();
        normal.contains(name)
    })
}

/// Whether `name` (a command word) runs text that may change the folder of
/// the line: a sourced file, or `eval`/`alias`/`trap` text with a `cd`, or
/// with any character the shell may quote, expand or match when it reads
/// the text again (`eval 'c\d ~'`, `alias go='c""d ~'`, `eval 'c?'`,
/// `eval '{c,}d'`).
pub(super) fn carries_change(name: &str, args: &[String]) -> bool {
    // #1309: zsh `zmodload`/`disable` and bash `enable` load or switch
    // builtins (a `cd` of a module, `enable -f x.so cd`, `enable -n cd`,
    // `disable cd`); `fc`, unless it only lists (`-l` without `-s`/`-e`),
    // and zsh `r` run commands from the history.
    matches!(
        name,
        "source" | "." | "zmodload" | "enable" | "disable" | "r"
    ) || (name == "fc" && !fc_lists(args))
        || (CARRIERS.contains(&name)
            && args.iter().any(|arg| {
                work::step();
                !arg.chars().all(|ch| {
                    work::step();
                    ch.is_ascii_alphanumeric()
                        || matches!(ch, '_' | '-' | '.' | '/' | '=' | ':' | '@' | '%' | ' ')
                }) || tokens(arg)
                    .any(|token| CHANGERS.contains(&token) || CARRIED_LOADERS.contains(&token))
                    || arg.split([' ', '=']).any(|piece| piece == ".")
            }))
}

/// #1309: whether `fc` given `args` only lists the history: an `l` in an
/// option group and no `s` or `e` (`fc -l -s` and `fc -l -e -` run).
fn fc_lists(args: &[String]) -> bool {
    let options = || {
        args.iter()
            .filter(|arg| arg.starts_with('-') && !arg.starts_with("--"))
    };
    options().any(|arg| {
        work::step();
        arg.contains('l')
    }) && !options().any(|arg| {
        work::step();
        arg.contains(['s', 'e'])
    })
}

/// Commands that, run from carrier text, load more text into the current
/// shell (`eval . ./f`, `eval source f`, `alias go=eval`).
const CARRIED_LOADERS: &[&str] = &["source", "eval", "alias", "autoload"];

/// Whether a folder change goes where the walk does not follow: zsh
/// `chdir`, a place on the folder stack (`cd -2`, `pushd +1`, `popd +1`, a
/// bare `pushd`, `cd ~1`), `cd ~-`/`~+` and other tilde forms (a named
/// folder, `cd ~h`), `pushd -n`, zsh's two-word `cd old new`, operands
/// after `--`, or `cd -`: a change the walk followed may not have
/// happened (`false && cd x`, `(cd x)`), so it may go to the inherited
/// `OLDPWD`.
pub(super) fn odd_change(name: &str, args: &[String]) -> bool {
    if name == "chdir" {
        return true;
    }
    let mut operands = 0;
    for arg in args {
        work::step();
        if arg == "--" || arg.starts_with('+') {
            return true;
        }
        // `pushd -` passes `-` on to `cd`.
        if arg == "-" && matches!(name, "cd" | "pushd") {
            return true;
        }
        if arg.starts_with('-') && arg != "-" {
            // `pushd -n`, `popd -n`: the stack changes, the folder does not.
            if name != "cd" || arg[1..].starts_with(|ch: char| ch.is_ascii_digit()) {
                return true;
            }
            continue;
        }
        if arg.starts_with('~') && arg != "~" && !arg.starts_with("~/") {
            return true;
        }
        operands += 1;
    }
    match name {
        "popd" => !args.is_empty(),
        "pushd" => operands == 0,
        _ => operands > 1,
    }
}

/// #1309: whether a command word is spelled as a pattern the shell may
/// match against file names (`c?`, `[c]d`, `e*l`) or expand (`{c,}d`), so
/// it may be any command. `[` and `[[` are commands.
pub(super) fn pattern_word(word: &str) -> bool {
    if matches!(word, "[" | "[[") {
        return false;
    }
    word.contains(['?', '*'])
        || word.find('[').is_some_and(|at| word[at..].contains(']'))
        // `{c,}d`, `{c..c}d`.
        || word
            .find('{')
            .is_some_and(|at| word[at..].contains(',') || word[at..].contains(".."))
        // extglob `@(c)d`, `+(c)d`, `!(x)`, zsh `(c|x)d`; not `f()`.
        || (word.contains('(') && !word.ends_with("()"))
        // #1309 follow-up: zsh extendedglob `cdd#`, `cd##`, `^x`, `cd~x`.
        || word.contains('^')
        || word.bytes().skip(1).any(|byte| byte == b'#' || byte == b'~')
}

/// #1309: zsh's precommand modifiers, which run the command after them:
/// `noglob cd ~` changes the folder.
pub(super) const MODIFIERS: &[&str] = &["noglob", "nocorrect", "-"];

/// #1309: whether the command after zsh's precommand modifiers (`words`,
/// the words after the first) may change the folder: a folder change, a
/// pattern, or text or builtins it loads (`noglob builtin cd ~`, `noglob
/// eval 'cd ~'`, `noglob source f`, `noglob enable -n cd`).
pub(super) fn modified_change(words: &[String]) -> bool {
    let start = words
        .iter()
        .position(|word| {
            work::step();
            !MODIFIERS.contains(&word.as_str())
        })
        .unwrap_or(words.len());
    let words = &words[start..];
    let Some((index, name)) = indirect::command_word(words) else {
        return false;
    };
    CHANGERS.contains(&name.as_str())
        || pattern_word(&words[index])
        || carries_change(&name, &words[index + 1..])
}

/// #1403: whether a folder change is named by a path (`./cd`,
/// `"$PWD/pushd"`): that file runs, not the builtin, so the folder it
/// leaves is not known. `word` is the command word, `name` its lowercase
/// basename.
pub(super) fn path_change(word: &str, name: &str) -> bool {
    CHANGERS.contains(&name) && word.contains('/')
}

/// #1422: whether the folder change at `words[index]` (`name`, its
/// lowercase basename) runs a program, not the builtin, so the folder it
/// leaves is not known: named by a path (`./cd`, #1403), spelled in
/// another case (`CD`: builtins are matched exactly), or behind a wrapper
/// that runs a program (`env cd`, `sudo cd`, `nice cd`, `exec cd`). Only
/// `command`, `builtin` and the `time` keyword run the builtin.
pub(super) fn external_change(words: &[String], index: usize, name: &str) -> bool {
    let Some(word) = words.get(index) else {
        return false;
    };
    if !CHANGERS.contains(&name) {
        return false;
    }
    path_change(word, name)
        || basename(word) != name
        || super::indirect::programs(words).into_iter().any(|at| {
            work::step();
            at != index
                && !matches!(
                    words.get(at).map(|word| basename(word)),
                    Some("command" | "builtin" | "time")
                )
        })
}

/// Whether `pushd`/`popd` changes the folder stack rather than (only) the
/// folder: `-n` keeps the folder, `+N`/`-N` pick an entry of a stack that
/// is not followed, and `pushd -` goes back (zsh).
pub(super) fn stack_change(name: &str, args: &[String]) -> bool {
    matches!(name, "pushd" | "popd")
        && args.iter().any(|arg| {
            work::step();
            arg == "-"
                || arg.starts_with('+')
                || (arg.starts_with('-')
                    && !arg.starts_with("--")
                    && (arg.contains('n') || arg[1..].starts_with(|ch: char| ch.is_ascii_digit())))
        })
}

/// The names in `text`: runs of letters, digits and underscores.
fn tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .filter(|token| !token.is_empty())
}
