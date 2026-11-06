//! #1141: commands that write the file an output option names
//! (`git log --output=FILE`, `git archive -o FILE`, `sort -o FILE`). Such a
//! command is a writer like `tee`, and the option's value is a file it
//! writes.

use super::git_helpers;
use super::work;

/// Commands with an output-file option: the long option (git and sort
/// accept any unambiguous prefix of it, `--out=FILE`) and its short letter.
/// git's `--output-directory` (`format-patch`) starts with `--output`, so a
/// prefix of it covers both; `-o` means output to `archive` and
/// `format-patch` and is taken as output for every git subcommand.
const OUTPUT_OPTIONS: &[(&str, &str, char)] = &[
    ("git", "--output-directory", 'o'),
    ("sort", "--output", 'o'),
    // #1159: more programs' output files.
    ("iconv", "--output", 'o'),
    ("pandoc", "--output", 'o'),
];
/// #1159: programs whose output-file options do not group with other
/// letters: each option word takes the next word (`-o FILE`, `-out FILE`),
/// a long one its value attached too (`--output=FILE`), and a two-letter
/// one, or #1449 `-MF`, an attached value (`-oFILE`, `-MFdeps.d`). #1449:
/// the compilers' `-MF` dependency file, and rustc's `--out-dir` (a folder
/// it writes files in) and `--emit` (`link=FILE,obj=FILE`).
const SINGLE_OUTPUT_OPTIONS: &[(&str, &[&str])] = &[
    ("cc", &["-o", "--output", "-MF"]),
    ("gcc", &["-o", "--output", "-MF"]),
    ("g++", &["-o", "--output", "-MF"]),
    ("c++", &["-o", "--output", "-MF"]),
    ("clang", &["-o", "--output", "-MF"]),
    ("clang++", &["-o", "--output", "-MF"]),
    ("tcc", &["-o", "-MF"]),
    ("ld", &["-o", "--output"]),
    ("ld.lld", &["-o", "--output"]),
    ("ld64.lld", &["-o", "--output"]),
    ("lld", &["-o", "--output"]),
    ("as", &["-o"]),
    ("rustc", &["-o", "--out-dir", "--emit"]),
    ("openssl", &["-out", "-keyout"]),
    ("csplit", &["-f", "--prefix"]),
];

/// git options that name a file `fast-export`/`fast-import` writes.
const GIT_MARK_OPTIONS: &[&str] = &["--export-marks", "--export-pack-edges"];
/// A short letter no option word holds.
const NO_SHORT: char = '\0';

/// Shortest long-option prefix taken as the output option (`--o`).
const MIN_PREFIX: usize = 3;

/// For a command that has an output-file option and uses it, the files it
/// names (empty when the value is missing; a folder it writes files of any
/// name in ends with `/`); `None` otherwise. `name` is the
/// command's lowercase basename, `args` the words after it as written.
/// #1159: a dashed git helper (`git-log`) is `git log`, `git.exe` is git.
/// #1283: `unchecked`: git or its `config` is spelled by text the
/// quote-aware reading cannot see, so a `git config` read is not trusted.
pub(super) fn output_files(name: &str, args: &[String], unchecked: bool) -> Option<Vec<String>> {
    let helper = git_helpers::git_name(name);
    let subcommand = helper.as_ref().and_then(Option::as_deref);
    let name = if helper.is_some() { "git" } else { name };
    if let Some(files) = other_output_files(name, args) {
        return Some(files);
    }
    let (_, long, short) = OUTPUT_OPTIONS
        .iter()
        .find(|(command, _, _)| *command == name)?;
    let has = |word: &str| subcommand == Some(word) || args.iter().any(|arg| arg == word);
    // `git config --file FILE key value` writes FILE; `git bundle create
    // FILE` writes its operand (a writer's operands are written).
    let config_file = name == "git"
        && if subcommand == Some("config") {
            unchecked || !git_config_reads(args)
        } else {
            args.iter()
                .position(|arg| arg == "config")
                .is_some_and(|at| unchecked || !git_config_reads(&args[at + 1..]))
        };
    let mut found = name == "git" && has("bundle") && has("create");
    let mut files = Vec::new();
    if config_file {
        found |= option_values(args, "--file", 'f', &mut files);
    }
    // `git credential-store --file FILE store` appends to FILE.
    if name == "git" && has("credential-store") {
        found |= option_values(args, "--file", NO_SHORT, &mut files);
    }
    found |= option_values(args, long, *short, &mut files);
    if name == "git" {
        // `fast-export`/`fast-import` write their marks and pack edges.
        for long in GIT_MARK_OPTIONS {
            work::step();
            found |= option_values(args, long, NO_SHORT, &mut files);
        }
        // #1159: `git --work-tree=DIR checkout` writes its files in DIR
        // (taken as a folder written, whatever the subcommand).
        let mut trees = Vec::new();
        found |= option_values(args, "--work-tree", NO_SHORT, &mut trees);
        files.extend(trees.iter().map(|tree| format!("{tree}/")));
        // `git clone URL DIR` and `git worktree add DIR` write in DIR.
        if let Some(at) = position_or_helper(args, subcommand, "clone") {
            found = true;
            files.extend(clone_folder(&args[at..]));
        }
        if let Some(at) = position_or_helper(args, subcommand, "worktree")
            && let Some(add) = args[at..].iter().position(|arg| arg == "add")
            && let Some(folder) = operands(&args[at + add + 1..], "bB", &["--reason"]).first()
        {
            found = true;
            files.push(format!("{folder}/"));
        }
        // `git read-tree --index-output=FILE` writes FILE.
        if has("read-tree") {
            found |= option_values(args, "--index-output", NO_SHORT, &mut files);
        }
        // `git checkout-index --prefix=~/.z` writes `~/.z` + each path
        // (`~/.zshrc`): the folder the prefix is in is written.
        if has("checkout-index") {
            let mut prefixes = Vec::new();
            found |= option_values(args, "--prefix", NO_SHORT, &mut prefixes);
            files.extend(prefixes.iter().map(|prefix| prefix_folder(prefix)));
        }
    }
    found.then_some(files)
}

/// #1159: the files [`SINGLE_OUTPUT_OPTIONS`] name, `uniq IN OUT`'s output
/// and `split IN PREFIX`'s prefix, as for [`output_files`]. A prefix (`split`,
/// `csplit -f`) writes the folder it is in, and makes names (`split -a1
/// notes .git/hooks/pre-commi` writes `pre-commit`).
fn other_output_files(name: &str, args: &[String]) -> Option<Vec<String>> {
    match name {
        // A second operand is the output. Which words are option values
        // is not known, so every operand after the first counts.
        "uniq" => {
            let operands = operands(args, "", &[]);
            return (operands.len() > 1).then(|| operands[1..].to_vec());
        }
        // `split [IN [PREFIX]]`: the last of two or more operands.
        "split" => {
            let operands = operands(args, "abClnt", SPLIT_VALUE_OPTIONS);
            return (operands.len() > 1).then(|| {
                let prefix = &operands[operands.len() - 1];
                vec![prefix.clone(), prefix_folder(prefix)]
            });
        }
        _ => {}
    }
    let (_, options) = SINGLE_OUTPUT_OPTIONS
        .iter()
        .find(|(command, _)| *command == name)?;
    let mut found = false;
    let mut files = Vec::new();
    for (index, word) in args.iter().enumerate() {
        work::step();
        // #1449: a compiler's options for its preprocessor or linker
        // (`-Wp,-MD,FILE`, `-Wl,-o,FILE`).
        if options.contains(&"-MF")
            && let Some(passed) = word
                .strip_prefix("-Wp,")
                .or_else(|| word.strip_prefix("-Wl,"))
        {
            let parts: Vec<&str> = passed.split(',').collect();
            for (at, part) in parts.iter().enumerate() {
                work::step();
                let file = if matches!(
                    *part,
                    "-MD" | "-MMD" | "-MF" | "-o" | "--output" | "-Map" | "--Map"
                ) {
                    parts.get(at + 1).copied()
                } else {
                    part.strip_prefix("--output=")
                        .or_else(|| part.strip_prefix("-Map="))
                        .or_else(|| part.strip_prefix("--Map="))
                        .or_else(|| part.strip_prefix("--out-implib="))
                        .or_else(|| part.strip_prefix("--dependency-file="))
                        .or_else(|| part.strip_prefix("-MF").filter(|file| !file.is_empty()))
                        .or_else(|| part.strip_prefix("-o").filter(|file| !file.is_empty()))
                };
                if let Some(file) = file {
                    found = true;
                    files.push(file.to_string());
                }
            }
            continue;
        }
        let (option, value) = if options.contains(&word.as_str()) {
            (word.as_str(), args.get(index + 1).cloned())
        } else if let Some((option, value)) = word.split_once('=')
            && option.starts_with("--")
            && options.contains(&option)
        {
            (option, Some(value.to_string()))
        } else if let Some(option) = options.iter().find(|option| {
            (option.len() == 2 || **option == "-MF")
                && word.len() > option.len()
                && word.starts_with(**option)
        }) {
            (*option, Some(word[option.len()..].to_string()))
        } else {
            continue;
        };
        found = true;
        match (option, value) {
            ("--out-dir", Some(folder)) => files.push(format!("{folder}/")),
            // `--emit=link=FILE,obj`: the files after `=`.
            ("--emit", Some(kinds)) => files.extend(kinds.split(',').filter_map(|kind| {
                work::step();
                kind.split_once('=').map(|(_, file)| file.to_string())
            })),
            (_, value) => files.extend(value),
        }
    }
    if name == "csplit" {
        let folders: Vec<String> = files.iter().map(|file| prefix_folder(file)).collect();
        files.extend(folders);
    }
    found.then_some(files)
}

/// `split`'s long options that take the next word as their value.
const SPLIT_VALUE_OPTIONS: &[&str] = &[
    "--suffix-length",
    "--bytes",
    "--line-bytes",
    "--lines",
    "--number",
    "--separator",
    "--additional-suffix",
    "--filter",
];

/// The operands in `args` (`-` names standard input), past the values of
/// options: a group of short options whose first letter in `short` takes
/// the rest of the group or, ending it, the next word; a long option in
/// `long` (or a prefix of one) without `=` takes the next word.
fn operands(args: &[String], short: &str, long: &[&str]) -> Vec<String> {
    let mut operands = Vec::new();
    let mut value_next = false;
    let mut options = true;
    for arg in args {
        work::step();
        if std::mem::take(&mut value_next) {
            continue;
        }
        if options && arg == "--" {
            options = false;
            continue;
        }
        if !options || arg == "-" || !arg.starts_with('-') {
            operands.push(arg.clone());
        } else if arg.starts_with("--") {
            value_next = !arg.contains('=')
                && arg.len() > 2
                && long.iter().any(|option| option.starts_with(arg.as_str()));
        } else if let Some(at) = arg.find(|ch: char| short.contains(ch)) {
            value_next = at + 1 == arg.len();
        }
    }
    operands
}

/// #1159: the files GNU `time -o FILE` writes, for the wrapper words before
/// a command (`words`, as written). Each `time` reads up to the next.
pub(super) fn wrapper_output_files(words: &[String]) -> Vec<String> {
    let mut files = Vec::new();
    let starts: Vec<usize> = words
        .iter()
        .enumerate()
        .filter(|(_, word)| super::shell::basename(word) == "time")
        .map(|(at, _)| at)
        .collect();
    for (nth, start) in starts.iter().enumerate() {
        work::step();
        let end = starts.get(nth + 1).copied().unwrap_or(words.len());
        option_values(&words[start + 1..end], "--output", 'o', &mut files);
    }
    files
}

/// #1159: writers whose operands are all paths (not a program, a mode or a
/// URL): one named by output the classifier cannot see may be any file.
pub(super) const PATH_WRITERS: &[&str] = &[
    "tee", "cp", "mv", "install", "ln", "rsync", "dd", "truncate", "touch", "ditto", "mkdir",
];

/// #1159: whether a word holds output the classifier cannot see, other
/// than an arithmetic expansion's number: as a file written, it may be any
/// file (`tee "$(cat f)"`).
pub(super) fn unseen_place(word: &str) -> bool {
    word.contains(super::shell::UNSEEN_OUTPUT)
        && word
            .replace(super::shell::ARITHMETIC_OUTPUT, "0")
            .contains(super::shell::UNSEEN_OUTPUT)
}

/// #1573 (1): whether zsh reads a written word as the full path of a
/// program on `PATH` (`=mytool`, the `EQUALS` option, on by default; also
/// for redirection targets: `echo x >> =mytool` appends to the installed
/// `mytool`). A bare `=` stays a file name.
pub(super) fn zsh_program_path(word: &str) -> bool {
    word.strip_prefix('=').is_some_and(|name| !name.is_empty())
}

/// #1159: whether git (`name`, with the words after it, `args`) runs a
/// subcommand that writes files of the working tree (`git checkout`, `git
/// pull`): in a folder that is or holds a home, it may write the home's own
/// files (`cd ~ && git checkout -- .zshrc`).
pub(super) fn writes_working_tree(name: &str, args: &[String]) -> bool {
    let helper = git_helpers::git_name(name);
    let subcommand = helper.as_ref().and_then(Option::as_deref);
    (name == "git" || helper.is_some())
        && WORKING_TREE_WRITERS.iter().any(|word| {
            work::step();
            subcommand == Some(*word) || args.iter().any(|arg| arg == word)
        })
}

/// #1159: git subcommands that write files of the working tree.
const WORKING_TREE_WRITERS: &[&str] = &[
    "checkout",
    "switch",
    "restore",
    "reset",
    "stash",
    "merge",
    "pull",
    "rebase",
    "cherry-pick",
    "revert",
    "am",
    "apply",
    "rm",
    "mv",
    "sparse-checkout",
    "bisect",
    "submodule",
    "read-tree",
];
/// `git clone`'s options that take the next word as their value.
const CLONE_VALUE_OPTIONS: &[&str] = &[
    "--origin",
    "--branch",
    "--upload-pack",
    "--reference",
    "--reference-if-able",
    "--separate-git-dir",
    "--depth",
    "--shallow-since",
    "--shallow-exclude",
    "--jobs",
    "--template",
    "--config",
    "--filter",
    "--server-option",
    "--bundle-uri",
    "--ref-format",
    "--revision",
];

/// The index of `word` in `args`, or 0 when it is the dashed helper's
/// subcommand (`git-clone`).
fn position_or_helper(args: &[String], subcommand: Option<&str>, word: &str) -> Option<usize> {
    if subcommand == Some(word) {
        return Some(0);
    }
    args.iter().position(|arg| arg == word).map(|at| at + 1)
}

/// The folder `git clone` (its words after `clone`) writes in, ending with
/// `/`: its second operand, else one named after the repository
/// (`https://x/r.git` clones into `r/`, `r.git/` with `--bare`).
fn clone_folder(args: &[String]) -> Option<String> {
    let operands = operands(args, "objuc", CLONE_VALUE_OPTIONS);
    if let Some(folder) = operands.get(1) {
        return Some(format!("{folder}/"));
    }
    let url = operands.first()?.trim_end_matches('/');
    let url = url.strip_suffix("/.git").unwrap_or(url);
    let name = url.rsplit(['/', ':']).next().unwrap_or(url);
    let name = name.strip_suffix(".git").unwrap_or(name);
    let bare = args.iter().any(|arg| arg == "--bare" || arg == "--mirror");
    Some(if bare {
        format!("{name}.git/")
    } else {
        format!("{name}/")
    })
}

/// #1159: `git checkout-index --prefix=P` writes P followed by each path it
/// checks out. A prefix that does not end with `/` makes new names
/// (`--prefix=.` and a tracked `git/hooks/x` write `.git/hooks/x`): what it
/// writes is not known. `name` and `args` as for [`output_files`].
pub(super) fn joins_names(name: &str, args: &[String]) -> bool {
    let helper = git_helpers::git_name(name);
    let subcommand = helper.as_ref().and_then(Option::as_deref);
    if !(name == "git" || helper.is_some())
        || !(subcommand == Some("checkout-index") || args.iter().any(|arg| arg == "checkout-index"))
    {
        return false;
    }
    let mut prefixes = Vec::new();
    option_values(args, "--prefix", NO_SHORT, &mut prefixes);
    prefixes
        .iter()
        .any(|prefix| !prefix.is_empty() && !prefix.ends_with('/'))
}

/// The folder a path prefix writes in, ending with `/`: itself when it ends
/// with `/`, else the folder that holds it (`./` when it names none).
fn prefix_folder(prefix: &str) -> String {
    match work::rfind(prefix, "/") {
        Some(at) => prefix[..=at].to_string(),
        None => "./".to_string(),
    }
}

/// git config options that take the next word as their value (`-zf FILE`
/// too).
pub(super) const GIT_CONFIG_VALUES: &[&str] = &[
    "-f",
    "--file",
    "--blob",
    "--type",
    "--default",
    "--comment",
    "--value",
    "--url",
];
/// git config options that take no value (others may take the next word).
pub(super) const GIT_CONFIG_FLAGS: &[&str] = &[
    "--global",
    "--system",
    "--local",
    "--worktree",
    "--show-origin",
    "--show-scope",
    "--null",
    "--name-only",
    "--includes",
    "--no-includes",
    "--bool",
    "--int",
    "--bool-or-int",
    "--path",
    "--expiry-date",
    "--fixed-value",
    "--all",
    "--no-type",
];
/// git config options that make it read, not write.
pub(super) const GIT_CONFIG_READS: &[&str] = &[
    "--get",
    "--get-all",
    "--get-regexp",
    "--get-urlmatch",
    "--get-color",
    "--get-colorbool",
    "--list",
    "-l",
];

/// Whether `git config` with these words (those after `config`) only reads:
/// its first word is the `get` or `list` subcommand, or a reading option
/// comes before the first word that is not an option. git stops reading
/// options there, so every later word is a key, value or pattern (`k v
/// --get`, `--comment --get`, `-- --get` write). Read conservatively: a
/// word that may be a value is one. Stops at the next `git` (#1102: each
/// word is scanned once per line).
pub(super) fn git_config_reads(words: &[String]) -> bool {
    if reads_by_subcommand(words) {
        return true;
    }
    let mut next = ConfigWord::Option;
    for word in words {
        work::step();
        match config_word(word, next) {
            Ok(reads) => return reads,
            Err(state) => next = state,
        }
    }
    false
}

/// #1159: [`git_config_reads`] of `words[at..]` for every `at` (and the
/// empty end), worked out in one pass from the end: what a word does depends
/// only on it and on what the word before it made it.
pub(super) fn git_config_reads_each(words: &[String]) -> Vec<bool> {
    const STATES: [ConfigWord; 3] = [
        ConfigWord::Option,
        ConfigWord::Value,
        ConfigWord::MaybeValue,
    ];
    // From each word on, read in each state.
    let mut after = [false; 3];
    let mut each = vec![false; words.len() + 1];
    for (at, word) in words.iter().enumerate().rev() {
        work::step();
        let mut here = [false; 3];
        for (slot, state) in here.iter_mut().zip(STATES) {
            work::step();
            *slot = match config_word(word, state) {
                Ok(reads) => reads,
                Err(next) => after[next as usize],
            };
        }
        after = here;
        each[at] = reads_by_subcommand(&words[at..]) || here[ConfigWord::Option as usize];
    }
    each
}

fn reads_by_subcommand(words: &[String]) -> bool {
    words
        .first()
        .is_some_and(|word| matches!(word.as_str(), "get" | "list"))
}

/// #1255: for every `at` (and the empty end), whether `git config` with
/// `words[at..]` (the words after `config`) opens the config file in an
/// editor: `-e`, `--edit` or a prefix of it from `--ed`, a short group
/// holding `e` (`-ze`), the `edit` subcommand, or any of them after `--`
/// (`git config -- -e` edits too). What the editor writes is config the
/// classifier never sees. Read loosely, so as to ask: any option before
/// the first other word counts, past the values of the options that take
/// one. An option that is neither a known flag nor a known value option
/// may be either (git takes any unambiguous prefix: `--fil F -e`, `--typ
/// bool -e`). #1269: in a short group, the letters after `f` or `t` are
/// their value (`-fpager.log`, `-zfe`), not options. Worked out in one pass
/// from the end.
pub(super) fn git_config_edits_each(words: &[String]) -> Vec<bool> {
    // Whether the words from the next word on, and from the one after it,
    // open the editor when the first of them is read as an option.
    let mut next = false;
    let mut after_next = false;
    let mut each = vec![false; words.len() + 1];
    for (at, word) in words.iter().enumerate().rev() {
        work::step();
        let here = if !word.starts_with('-') {
            false
        } else if opens_editor(word) {
            true
        } else if GIT_CONFIG_VALUES.contains(&word.as_str())
            || (!word.starts_with("--") && word.ends_with('f'))
        {
            // #1269: an `f` or `t` before the last letter takes the rest
            // of the group (`-ff` is the file `f`), so the next word may
            // be an option too.
            let attached = !word.starts_with("--") && word[1..word.len() - 1].contains(['f', 't']);
            after_next || (attached && next)
        } else if word.contains('=') || GIT_CONFIG_FLAGS.contains(&word.as_str()) {
            next
        } else {
            next || after_next
        };
        after_next = next;
        next = here;
        each[at] = here || word == "edit";
    }
    each
}

/// Whether `word`, read as a `git config` option, opens the editor.
fn opens_editor(word: &str) -> bool {
    if word.starts_with("--") {
        word.len() >= "--ed".len() && "--edit".starts_with(word)
    } else {
        word.split(['f', 't']).next().unwrap_or(word).contains('e')
    }
}

/// #1283: whether `word` (lowercase) may open the editor as a `git config`
/// option or subcommand wherever it is.
pub(super) fn may_open_editor(word: &str) -> bool {
    (word.starts_with('-') && opens_editor(word)) || word == "edit"
}

/// What the next `git config` word may be.
#[derive(Clone, Copy)]
enum ConfigWord {
    Option = 0,
    /// The value of the option before it.
    Value = 1,
    /// The value of an unknown option before it, when it looks like an
    /// option (`--comment -l`).
    MaybeValue = 2,
}

/// One `git config` word read as `state` says: `Ok(reads)` when it decides,
/// else what the next word may be.
fn config_word(word: &str, state: ConfigWord) -> Result<bool, ConfigWord> {
    let option = word.starts_with('-');
    match state {
        ConfigWord::Value => return Err(ConfigWord::Option),
        ConfigWord::MaybeValue if option => return Err(ConfigWord::Option),
        ConfigWord::Option | ConfigWord::MaybeValue => {}
    }
    // A word that is not an option may be the key (after an unknown option
    // too), which ends the options. #1159: a git helper (`git-config`)
    // starts another git line too.
    if word == "--"
        || !option
        || super::shell::basename(word) == "git"
        || git_helpers::git_name(word).is_some()
    {
        return Ok(false);
    }
    if GIT_CONFIG_READS.contains(&word) {
        return Ok(true);
    }
    if word.contains('=') || GIT_CONFIG_FLAGS.contains(&word) {
        return Err(ConfigWord::Option);
    }
    if GIT_CONFIG_VALUES.contains(&word) || (!word.starts_with("--") && word.ends_with('f')) {
        Err(ConfigWord::Value)
    } else {
        Err(ConfigWord::MaybeValue)
    }
}

/// Push the values of `long` (or a prefix of it) and `short` in `args` to
/// `files`; whether the option is used.
fn option_values(args: &[String], long: &str, short: char, files: &mut Vec<String>) -> bool {
    let mut found = false;
    for (index, word) in args.iter().enumerate() {
        work::step();
        let next = || args.get(index + 1).cloned();
        if let Some(option) = word.strip_prefix("--") {
            let (option, value) = match option.split_once('=') {
                Some((option, value)) => (option, Some(value.to_string())),
                None => (option, None),
            };
            let option = format!("--{option}");
            if option.len() >= MIN_PREFIX && long.starts_with(option.as_str()) {
                found = true;
                files.extend(value.or_else(next));
            }
        } else if let Some(cluster) = word.strip_prefix('-')
            && let Some(at) = cluster.find(short)
        {
            // A group of short options (`-vo FILE`, `-oFILE`): the rest of
            // the group after the letter is its value, else the next word.
            found = true;
            let attached = &cluster[at + short.len_utf8()..];
            if attached.is_empty() {
                files.extend(next());
            } else {
                files.push(attached.to_string());
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::git_config_edits_each;
    use super::git_config_reads;
    use super::git_config_reads_each;
    use super::output_files;
    use pretty_assertions::assert_eq;

    /// #1159: the one-pass reading agrees with reading each start alone.
    #[test]
    fn config_reads_each_start() {
        for line in [
            "get k",
            "-f f k get",
            "--comment --get -f f k z",
            "-f f k -- --get",
            "--global --get user.name",
            "-zf f --list",
            "--type bool --get core.bare",
            "--comment -l k v",
            "-f config -f git -f config --get",
            "--get-colorbool x git --list config -l",
            "-/git --get",
            "--file --get k --unknown -x --get-all",
        ] {
            let words: Vec<String> = line.split(' ').map(str::to_string).collect();
            let each = git_config_reads_each(&words);
            let alone: Vec<bool> = (0..=words.len())
                .map(|at| git_config_reads(&words[at..]))
                .collect();
            assert_eq!(each, alone, "{line}");
        }
    }

    #[test]
    fn config_null_flag_keeps_read_options_distinct_from_values() {
        // Keep the shared parser at main's unknown-option handling for `-z`.
        // Only the local write detector recognizes it as a no-value flag.
        for (line, expected) in [
            ("-z --get pager.log", false),
            ("--null --get pager.log", true),
            ("-z --list", false),
            ("-z --comment note --get pager.log", false),
            ("-z --comment --get pager.log x", true),
            ("-z --comm --get pager.log x", true),
            ("-z --file --get pager.log x", true),
            ("-z -- --get pager.log", true),
            ("-z pager.log --get", false),
        ] {
            let words: Vec<String> = line.split(' ').map(str::to_string).collect();
            assert_eq!(git_config_reads(&words), expected, "{line}");
            assert_eq!(git_config_reads_each(&words)[0], expected, "{line}");
        }
    }

    fn files(name: &str, args: &[&str]) -> Option<Vec<String>> {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        output_files(name, &args, /*unchecked*/ false)
    }

    /// #1255: which `git config` words open the editor, from the start and
    /// from every later word, against git's reading of them (`-e x` edits,
    /// `x -e` sets `x` to `-e`, `-f -e` names a file `-e`).
    #[test]
    fn config_edits_each_start() {
        for (line, from_start) in [
            ("-e", true),
            ("--edit", true),
            ("--ed", true),
            ("--edi", true),
            ("edit", true),
            ("edit --local", true),
            ("-- -e", true),
            ("-- --local -e", true),
            ("--global -e", true),
            ("-ze", true),
            ("-z -e", true),
            ("--file=y -e", true),
            ("-fy -e", true),
            ("--type=bool -e", true),
            ("-e x", true),
            ("--fil y -e", true),
            ("--typ bool -e", true),
            ("--ty bool -e", true),
            ("-t bool -e", true),
            ("--unknown -e", true),
            ("user.name -e", false),
            ("--local user.name -e", false),
            ("-- edit", false),
            ("-f -e", false),
            ("--file -e k v", false),
            ("-zf -e k v", false),
            ("--comment -e", false),
            ("--e", false),
            ("--expiry-date", false),
            ("--get user.name", false),
            ("--list", false),
            ("", false),
        ] {
            let words: Vec<String> = line.split_whitespace().map(str::to_string).collect();
            let each = git_config_edits_each(&words);
            assert_eq!(each[0], from_start, "{line}");
            let alone: Vec<bool> = (0..=words.len())
                .map(|at| git_config_edits_each(&words[at..])[0])
                .collect();
            assert_eq!(each, alone, "{line}");
        }
    }

    #[test]
    fn output_options_name_written_files() {
        let some = |files: &[&str]| Some(files.iter().map(|file| (*file).to_string()).collect());
        for (name, args, expected) in [
            (
                "git",
                &["log", "--output=~/.zshrc"][..],
                some(&["~/.zshrc"]),
            ),
            ("git", &["log", "--output", "f"], some(&["f"])),
            ("git", &["diff", "--outp=f"], some(&["f"])),
            ("git", &["archive", "-o", "f", "HEAD"], some(&["f"])),
            ("git", &["archive", "-of"], some(&["f"])),
            ("git", &["archive", "-vof"], some(&["f"])),
            (
                "git",
                &["format-patch", "--output-directory", "d"],
                some(&["d"]),
            ),
            ("git", &["log", "--output"], some(&[])),
            (
                "git",
                &["config", "--file", "~/.zshrc", "a.b", "x"],
                some(&["~/.zshrc"]),
            ),
            (
                "git",
                &["config", "-f~/.zshrc", "a.b", "x"],
                some(&["~/.zshrc"]),
            ),
            ("git", &["bundle", "create", "~/.zshrc", "HEAD"], some(&[])),
            (
                "git",
                &["fast-export", "--export-marks=~/.zshrc", "HEAD"],
                some(&["~/.zshrc"]),
            ),
            (
                "git",
                &["credential-store", "--file", "~/.zshrc", "store"],
                some(&["~/.zshrc"]),
            ),
            ("sort", &["-o", "f", "in"], some(&["f"])),
            ("sort", &["-ro~/.zshrc"], some(&["~/.zshrc"])),
            ("sort", &["--output=f"], some(&["f"])),
            ("git", &["log", "--oneline", "-n", "5"], None),
            ("git", &["log", "--output-indicator-new=+"], None),
            ("sort", &["--other=f"], None),
            ("git", &["commit", "--file=msg"], None),
            (
                "git",
                &["config", "-f", "~/.gitconfig", "--get", "a.b"],
                None,
            ),
            ("cat", &["-o", "f"], None),
        ] {
            assert_eq!(files(name, args), expected, "{name} {args:?}");
        }
    }
}
