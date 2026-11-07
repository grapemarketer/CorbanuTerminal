//! #1159: local Git writes outside the shared command-free key allowlist.
//! Reads are resolved within the write scan; uncertain options and split
//! values must not turn a possible persistent code setting into a silent action.

use super::basename;
use super::code_variables;
use super::git_helpers;
use super::outputs;
use super::work;

enum ConfigArgs<'a> {
    Found(&'a [String]),
    NotConfig,
    Uncertain,
}

/// Find `config`, sharing Git's global value-option boundaries. The word
/// lexer splits quoted values, so after an option a plain word may still be
/// its value. Only plain Git followed by an option lets the caller restart
/// with the same suffix; a helper-looking value remains uncertain.
fn config_args(args: &[String]) -> ConfigArgs<'_> {
    let mut at = 0;
    let mut after_option = false;
    while let Some(word) = args.get(at) {
        work::step();
        let takes_value = code_variables::GIT_VALUE_OPTIONS.contains(&word.as_str())
            || matches!(word.as_str(), "-c" | "--config-env");
        let boundary_at = at + usize::from(takes_value);
        if let Some(boundary) = args.get(boundary_at) {
            let git_name = git_helpers::git_name(boundary);
            let plain_git =
                basename(boundary).eq_ignore_ascii_case("git") || git_name == Some(None);
            if plain_git
                && args
                    .get(boundary_at + 1)
                    .is_none_or(|after| after.starts_with('-'))
            {
                return ConfigArgs::NotConfig;
            }
            if !plain_git && git_name.is_some() {
                return ConfigArgs::Uncertain;
            }
        }
        if word == "config" {
            return ConfigArgs::Found(&args[at + 1..]);
        }
        if takes_value {
            after_option = true;
            at += 2;
        } else if word.starts_with('-') || after_option {
            after_option = true;
            at += 1;
        } else {
            return ConfigArgs::NotConfig;
        }
    }
    ConfigArgs::NotConfig
}

/// Local config writes, sharing work across all Git words in a command.
/// A Git-looking word can be an option's value, so scans from different
/// Git words may overlap even when each scan respects option boundaries.
pub(super) struct Writes<'a> {
    words: &'a [String],
    seen: Option<FollowingWrites>,
}

struct FollowingWrites {
    /// Whether a later word is `config`, including this word.
    has_config: Vec<bool>,
    /// Whether any possible key, excluding the final value, is unsafe.
    unsafe_keys: Vec<bool>,
    /// A suffix's result for each combination of the three parser flags.
    writes: Vec<[Option<bool>; 8]>,
}

impl<'a> Writes<'a> {
    pub(super) fn new(words: &'a [String]) -> Self {
        Self { words, seen: None }
    }

    /// Whether a possible local write sets a key outside the same allowlist
    /// used for `git -c`. Values do not make an unsafe key safe: even `false`
    /// or `rebase` asks after untrusted content. Section renames can install
    /// previously harmless keys in a program-running family and also ask.
    pub(super) fn writes_code_setting(&mut self, from: usize, subcommand: Option<&str>) -> bool {
        let words = self.words;
        let start = match subcommand {
            Some("config") => Some(from),
            Some(_) => return false,
            None => match config_args(&words[from..]) {
                ConfigArgs::Found(args) => Some(words.len() - args.len()),
                ConfigArgs::NotConfig => return false,
                ConfigArgs::Uncertain => None,
            },
        };
        let seen = self.seen.get_or_insert_with(|| {
            let mut has_config = vec![false; words.len() + 1];
            let mut unsafe_keys = vec![false; words.len() + 1];
            for (at, word) in words.iter().enumerate().rev() {
                work::step();
                has_config[at] = word == "config" || has_config[at + 1];
                // Plain alphanumeric fragments cannot be keys: Git requires
                // a section/name separator. They can be split option values
                // or part of the value after a safe key. Dynamic and dotted
                // words still need the shared safe-list check.
                unsafe_keys[at] = unsafe_keys[at + 1]
                    || (at + 1 < words.len()
                        && !word.bytes().all(|byte| byte.is_ascii_alphanumeric())
                        && !code_variables::safe_git_key(&word.to_ascii_lowercase()));
            }
            FollowingWrites {
                has_config,
                unsafe_keys,
                writes: vec![[None; 8]; words.len()],
            }
        });
        let Some(mut at) = start else {
            // A helper-looking value before `status` or `log` is not
            // evidence of a config write. The suffix is shared by every
            // Git start, so this additional check stays linear.
            return seen.has_config[from];
        };
        let mut subcommand_position = true;
        let mut options = true;
        let mut after_value_option = false;
        let mut visited = Vec::new();
        let writes = loop {
            work::step();
            let Some(word) = words.get(at) else {
                break false;
            };
            let state = usize::from(options)
                | (usize::from(subcommand_position) << 1)
                | (usize::from(after_value_option) << 2);
            if let Some(writes) = seen.writes[at][state] {
                break writes;
            }
            visited.push((at, state));
            let first = std::mem::take(&mut subcommand_position);
            if options && word == "--" {
                options = false;
                at += 1;
                continue;
            }
            if options && word.starts_with('-') {
                let option = word.split_once('=').map_or(word.as_str(), |(name, _)| name);
                // Only flags before the first value-taking short option can select a list.
                let bundled_list = word
                    .strip_prefix('-')
                    .filter(|short| !short.starts_with('-'))
                    .and_then(|short| short.split(['f', 't']).next())
                    .is_some_and(|flags| {
                        flags.contains('l') && flags.chars().all(|flag| matches!(flag, 'l' | 'z'))
                    });
                if outputs::GIT_CONFIG_READS.contains(&word.as_str())
                    || bundled_list
                    || matches!(
                        word.as_str(),
                        "--unset" | "--unset-all" | "--remove-section" | "--help" | "-h"
                    )
                {
                    if !after_value_option {
                        break false;
                    }
                    // A read-looking word can be part of a split option value.
                    // Keep checking for a later unsafe key/value pair.
                    at += 1;
                    continue;
                }
                if word == "--rename-section" {
                    break words.get(at + 2).is_some();
                }
                if outputs::GIT_CONFIG_VALUES.contains(&option) || option == "-t" {
                    after_value_option = true;
                    at += if word.contains('=') { 1 } else { 2 };
                } else if outputs::GIT_CONFIG_FLAGS.contains(&word.as_str())
                    || matches!(
                        word.as_str(),
                        "-z" | "--add" | "--replace-all" | "--append" | "--bool-or-str"
                    )
                {
                    at += 1;
                } else if let Some(short) = word.strip_prefix('-')
                    && !short.starts_with('-')
                    && let Some((offset, flag)) =
                        short.char_indices().find(|(_, ch)| matches!(ch, 'f' | 't'))
                    && short[..offset].chars().all(|ch| ch == 'z')
                {
                    // A short option group (`-zf FILE`) or attached value.
                    after_value_option = true;
                    at += 1 + usize::from(offset + flag.len_utf8() == short.len());
                } else {
                    // An abbreviation can rename a section or take the next
                    // word as its value. The read check did not prove a read;
                    // ask instead of guessing where a write's key starts.
                    break true;
                }
                continue;
            }
            // At the key position, a Git-looking word is still a key. The
            // suffix memo keeps overlapping invocations linear.
            // After any option, Git uses legacy key/value syntax. Only the
            // first argument can select a subcommand such as `unset`.
            if options && first {
                match word.as_str() {
                    "get" | "list" | "unset" | "remove-section" => break false,
                    "rename-section" => break words.get(at + 2).is_some(),
                    "set" => {
                        at += 1;
                        continue;
                    }
                    _ => {}
                }
            }
            let safe = |key: &str| code_variables::safe_git_key(&key.to_ascii_lowercase());
            // A value split at blanks or commas can leave a safe-looking
            // fragment where the key would be. Every following possible key
            // except the final value must be safe before this write stays quiet.
            break words.get(at + 1).is_some()
                && if after_value_option {
                    seen.unsafe_keys[at]
                } else {
                    !safe(word)
                };
        };
        // Each word/state is scanned at most once across all invocations.
        for (at, state) in visited {
            work::step();
            seen.writes[at][state] = Some(writes);
        }
        writes
    }
}

/// The shared resolver deliberately skips wrapper options conservatively.
/// For this additional detector only, exclude wrappers that inspect names or
/// print their own help instead of running the resolved Git command. The
/// prefix ends at that resolved command, so other words here are assignments,
/// shell keywords or wrappers already recognized by `command_word`.
/// Consume their value options too: `env -u command git ...` does not invoke
/// the `command` builtin. Existing classifier checks remain unchanged.
pub(super) fn inspection_only_prefix(prefix: &[String]) -> bool {
    let mut at = 0;
    while let Some(word) = prefix.get(at) {
        work::step();
        let name = basename(word).to_lowercase();
        at += 1;
        if word.contains('=')
            || matches!(
                name.as_str(),
                "!" | "if" | "then" | "elif" | "else" | "do" | "while" | "until"
            )
        {
            continue;
        }
        // Operand boundaries are the same as the unchanged shared resolver.
        let (value_options, mut operands) = super::indirect::WRAPPERS
            .iter()
            .find(|(wrapper, _, _)| *wrapper == name)
            .map_or((&[][..], 0), |(_, options, operands)| (*options, *operands));
        let mut options = true;
        while let Some(option) = prefix.get(at) {
            work::step();
            if option.starts_with('-') {
                if options
                    && ((name == "command"
                        && !option.starts_with("--")
                        && option.chars().skip(1).any(|ch| matches!(ch, 'v' | 'V')))
                        || (name == "env" && matches!(option.as_str(), "--help" | "--version")))
                {
                    return true;
                }
                if option == "--" {
                    options = false;
                }
                if name == "taskset" && matches!(option.as_str(), "-c" | "--cpu-list") {
                    operands = 0;
                }
                at += 1 + usize::from(value_options.contains(&option.as_str()));
            } else if option.contains('=') {
                at += 1;
            } else {
                break;
            }
        }
        at += operands;
    }
    false
}
