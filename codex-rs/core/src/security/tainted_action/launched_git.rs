//! #1268: git run by a launcher that adds words from its input: `xargs`,
//! `parallel` (also its `:::` arguments), `find -exec`/`-execdir`/`-ok`/
//! `-okdir`, `fd -x`/`-X` (also in a group: `-HX`). Those words are unseen,
//! and may be git's own
//! options (`printf -- '-c core.pager=sh log' | xargs git`) or what `git
//! config` writes (`printf 'filter.a.clean /tmp/x' | xargs git config`).
//! So is a positional parameter (`xargs sh -c 'git config "$@"' _`).
//!
//! Words added at the end of a line whose subcommand git has already read
//! are that subcommand's, so `xargs git log --oneline` stays quiet. Fails
//! closed otherwise: git with no subcommand yet, or an option first (the
//! word lexer may split quoted text, so where the subcommand starts is
//! certain only when it is the first word), or a first word input may fill
//! (`xargs -I{} git {} log`), asks as code the classifier cannot see; `git
//! config` given input words asks as a write unless its visible words
//! already make it a read (`xargs git config --get user.name`).

use super::ProtectedActionKind;
use super::outputs;
use super::shell::basename;
use super::work;

/// Launchers that add words from their input to the line they run (#1276:
/// `xjobs` too).
pub(super) const FEEDERS: &[&str] = &["xargs", "gxargs", "xjobs"];
/// Launchers like GNU `parallel` (#1276: `env_parallel`, `rush`): they
/// also put input where placeholders are and run the line with a shell.
pub(super) const PARALLELS: &[&str] = &["parallel", "env_parallel", "rush"];
/// Finders whose exec options start a launched command (#1276: `bfs`).
pub(super) const FINDERS: &[&str] = &["find", "gfind", "fd", "fdfind", "bfs"];

/// GNU `parallel` options that take the next word as their value.
pub(super) const PARALLEL_VALUED: &[&str] = &[
    "-j",
    "--jobs",
    "-P",
    "--max-procs",
    "-N",
    "--max-replace-args",
    "-n",
    "--max-args",
    "-a",
    "--arg-file",
    "-d",
    "--delimiter",
    "--colsep",
    "-C",
    "-S",
    "--sshlogin",
    "--timeout",
    "--halt",
    "--joblog",
    "--results",
    "--retries",
    "--delay",
    "--tmpdir",
    "--workdir",
    "--wd",
    "--memfree",
    "--load",
    "--block",
];
/// GNU `parallel` options that take no value.
pub(super) const PARALLEL_FLAGS: &[&str] = &[
    "-k",
    "--keep-order",
    "-q",
    "--quote",
    "-0",
    "--null",
    "-v",
    "--verbose",
    "--dry-run",
    "--dryrun",
    "--bar",
    "--progress",
    "--eta",
    "--lb",
    "--line-buffer",
    "--group",
    "-u",
    "--ungroup",
    "--tag",
    "--pipe",
    "--pipepart",
    "-r",
    "--no-run-if-empty",
    "-m",
    "-X",
    "--xargs",
    "--will-cite",
    "--citation",
];

/// Replacement strings (`xargs -I R`) remembered; past this many, any word
/// may be one.
const MAX_REPLACEMENTS: usize = 8;

/// The launchers met so far in one simple command's words, and what the
/// git checks need of the words, worked out once.
pub(super) struct LaunchedGit<'a> {
    /// The command's words, as written and lowercase.
    words: &'a [String],
    lower: &'a [String],
    /// The line as written holds a brace, so a word may have held a
    /// placeholder (`{}`, `{.}`) the word lexer dropped.
    braces: bool,
    /// A launcher that adds words from its input runs what follows.
    fed: bool,
    /// A `find` or `fd`, whose exec options start such a command.
    finder: bool,
    /// An `fd`, whose `-x` may hold its command (`-xgit`).
    fd: bool,
    /// The launcher puts input where placeholders (`{}`) are: `find -exec`,
    /// `fd -x`, `parallel`.
    placeholders: bool,
    /// The next word may be a replacement string (`xargs -I R`).
    replacement_next: bool,
    /// In `parallel`'s options; the next word is an option's value.
    parallel_options: bool,
    parallel_value_next: bool,
    replacements: Vec<String>,
    /// More replacement strings than remembered.
    any_replacement: bool,
    /// For each word (and the end): whether, as the words after `config`,
    /// they read, and whether one of them is a positional parameter.
    seen: Option<(Vec<bool>, Vec<bool>, Vec<bool>)>,
    /// #1283/#1276: what else is known of the words.
    marks: Marks<'a>,
    /// #1283: a launcher met so far was hidden.
    hidden_launcher: bool,
}

/// What else is known of a command's words (each empty when unknown).
#[derive(Clone, Copy, Default)]
pub(super) struct Marks<'a> {
    /// #1283: the quote-aware reading cannot spell it (`$g` for `git`,
    /// `{git,config}`), so it cannot check what the word lexer makes of the
    /// words.
    pub(super) hidden: &'a [bool],
    /// #1283: the quote-aware reading cannot know its text (`"$k"` with
    /// `k=core.pager`).
    pub(super) unchecked: &'a [bool],
    /// #1276: it holds a loop variable that iterates the positional
    /// parameters (`for k; do git config "$k"`).
    pub(super) looped: &'a [bool],
    /// #1276 item 3: it holds an expansion of a value the quote-aware
    /// reading does not know outside double quotes, so, like `$@`, it may
    /// be several words (`x=$(cat f); git config $x`).
    pub(super) splits: &'a [bool],
    /// #1276 item 3: it holds an expansion of a value the quote-aware
    /// reading does not know (`git config "$k"`, which may be `-e`).
    pub(super) dynamic: &'a [bool],
}

/// #1276: loop variables that iterate the positional parameters (`for k;`,
/// `select k do`, no `in`), met so far in a script. Past
/// `MAX_LOOP_NAMES`, any variable may be one.
#[derive(Default)]
pub(super) struct PositionalLoops {
    names: Vec<String>,
    any: bool,
}

const MAX_LOOP_NAMES: usize = 8;

impl PositionalLoops {
    /// Takes in a simple command's words as written.
    pub(super) fn read(&mut self, words: &[String]) {
        let keyword = words
            .first()
            .is_some_and(|word| matches!(word.to_lowercase().as_str(), "for" | "select"));
        let Some(name) = words.get(1).filter(|_| keyword) else {
            return;
        };
        let iterates_positional = words.get(2).is_none_or(|word| word.to_lowercase() == "do");
        if !iterates_positional || !is_name(name) || self.names.contains(name) {
            return;
        }
        if self.names.len() == MAX_LOOP_NAMES {
            self.any = true;
        } else {
            self.names.push(name.clone());
        }
    }

    /// For each of a simple command's words as written, whether it holds
    /// one of the loop variables (`$k`, `${k}`).
    pub(super) fn marks(&self, words: &[String]) -> Vec<bool> {
        if self.names.is_empty() && !self.any {
            return Vec::new();
        }
        words
            .iter()
            .map(|word| {
                word.match_indices('$').any(|(at, _)| {
                    work::step();
                    let after = &word[at + 1..];
                    let after = after.strip_prefix('{').unwrap_or(after);
                    let end = after
                        .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
                        .unwrap_or(after.len());
                    let name = &after[..end];
                    !name.is_empty() && (self.any || self.names.iter().any(|known| known == name))
                })
            })
            .collect()
    }
}

fn is_name(word: &str) -> bool {
    !word.is_empty()
        && !word.starts_with(|ch: char| ch.is_ascii_digit())
        && word
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

impl<'a> LaunchedGit<'a> {
    /// `launched`: the words are in a script a launcher adds its input to
    /// (`xargs env -S'git config'`), or puts it in, so that any word may be
    /// input (`xargs -I% sh -c 'cd x && git config %'`).
    pub(super) fn new(
        words: &'a [String],
        lower: &'a [String],
        braces: bool,
        launched: Launch,
        marks: Marks<'a>,
    ) -> Self {
        Self {
            words,
            lower,
            braces,
            fed: launched.fed || launched.replaced,
            finder: false,
            fd: false,
            placeholders: false,
            replacement_next: false,
            parallel_options: false,
            parallel_value_next: false,
            replacements: Vec::new(),
            any_replacement: launched.replaced,
            seen: None,
            marks,
            hidden_launcher: false,
        }
    }

    fn hidden(&self, index: usize) -> bool {
        self.marks.hidden.get(index).copied().unwrap_or(false)
    }

    /// Whether a launcher's input words may be put inside the words that
    /// follow (rather than only after them).
    fn replaces(&self) -> bool {
        self.fed
            && (self.any_replacement
                || self.replacement_next
                || !self.replacements.is_empty()
                || (self.braces && self.placeholders))
    }

    /// Takes in `words[index]` after it is judged.
    pub(super) fn step(&mut self, index: usize) {
        let word = self.words[index].as_str();
        let name = basename(&self.lower[index]);
        if std::mem::take(&mut self.replacement_next) {
            self.replace_with(word);
        }
        if self.fed {
            self.replacement_option(word);
        }
        if self.parallel_options {
            self.parallel_option(word);
        }
        // `fd` takes `-x`/`-X` at the end of a group too (`-HX`), and its
        // command attached (#1276: `--exec=git`, `-xgit`).
        let finder_exec = self.finder
            && (matches!(
                word,
                "-exec" | "-execdir" | "-ok" | "-okdir" | "--exec" | "--exec-batch"
            ) || (word.starts_with('-')
                && !word.starts_with("--")
                && word.ends_with(['x', 'X']))
                || self.attached(index).is_some());
        let parallel = PARALLELS.contains(&name);
        self.placeholders |= finder_exec || parallel;
        if FEEDERS.contains(&name) || parallel || finder_exec {
            self.fed = true;
            self.hidden_launcher |= self.hidden(index);
        }
        self.finder |= FINDERS.contains(&name);
        self.fd |= matches!(name, "fd" | "fdfind");
        self.parallel_options |= parallel;
    }

    /// #1276: the command a finder's exec option holds attached
    /// (`--exec=git`, `--exec-batch=git`, `fd -xgit`, `fd -Hxgit`), lowercase.
    pub(super) fn attached(&self, index: usize) -> Option<&'a str> {
        if !self.finder {
            return None;
        }
        let word: &'a str = self.lower[index].as_str();
        if let Some(long) = word.strip_prefix("--") {
            return ["exec=", "exec-batch="]
                .iter()
                .find_map(|option| long.strip_prefix(option));
        }
        let group = word.strip_prefix('-').filter(|_| self.fd)?;
        let at = group.find(['x', 'X'])?;
        // `-x=git` too.
        let command = &group[at + 1..];
        Some(command.strip_prefix('=').unwrap_or(command)).filter(|command| !command.is_empty())
    }

    /// One of `parallel`'s own options, before its command. It has many
    /// that set replacement strings (`--er X`, `--rpl 'X …'`, `-I`) and
    /// takes any unambiguous prefix of a long one, so any option not known
    /// to set none may: any word may be input.
    fn parallel_option(&mut self, word: &str) {
        if std::mem::take(&mut self.parallel_value_next) {
            return;
        }
        if word.starts_with(":::") || !word.starts_with('-') || word == "-" {
            // `:::` arguments or the command: the options ended.
            self.parallel_options = false;
            return;
        }
        let option = word.split_once('=').map_or(word, |(option, _)| option);
        let counted = ["-j", "-P", "-N", "-n"].iter().any(|short| {
            option.strip_prefix(short).is_some_and(|count| {
                !count.is_empty()
                    && count
                        .chars()
                        .all(|ch| ch.is_ascii_digit() || "+-%".contains(ch))
            })
        });
        if PARALLEL_VALUED.contains(&option) {
            self.parallel_value_next = !word.contains('=');
        } else if !(counted || PARALLEL_FLAGS.contains(&option)) {
            self.any_replacement = true;
        }
    }

    /// A launcher's replacement-string option: `xargs -I R`, `-IR`, `-i`,
    /// `-iR`, `--replace[=R]` (or a prefix of it), BSD `-J R`, `parallel -I
    /// R`, in a group of short options too (`-0I%`). Read loosely: a
    /// string taken for one that is not only makes more words input.
    fn replacement_option(&mut self, word: &str) {
        if let Some(long) = word.strip_prefix("--") {
            let (option, value) = long.split_once('=').unwrap_or((long, ""));
            if !option.is_empty() && "replace".starts_with(option) && option.starts_with('r') {
                self.replace_option(value);
            }
        } else if let Some(group) = word.strip_prefix('-')
            && let Some(at) = group.find(['I', 'J', 'i'])
        {
            self.replace_option(&group[at + 1..]);
        }
    }

    /// A replacement option's value. Without one, the string may be the
    /// default (`xargs -i`, bare `--replace`: `{}`) or the next word (`-I
    /// R`): both.
    fn replace_option(&mut self, value: &str) {
        if value.is_empty() {
            self.replace_with("{}");
            self.replacement_next = true;
        } else {
            self.replace_with(value);
        }
    }

    fn replace_with(&mut self, replacement: &str) {
        if replacement.is_empty() {
            return;
        }
        if self.replacements.len() == MAX_REPLACEMENTS {
            self.any_replacement = true;
        } else {
            self.replacements.push(replacement.to_string());
        }
    }

    /// Whether input may fill `word` (a placeholder, which the word lexer
    /// drops, a replacement string, `parallel`'s `:::` arguments).
    fn input_fills(&self, word: &str) -> bool {
        self.braces
            || self.any_replacement
            || word.starts_with(":::")
            || self
                .replacements
                .iter()
                .any(|replacement| word.contains(replacement.as_str()))
    }

    /// The kind `words[index]` asks as when it may run git (`helper`: what
    /// [`super::git_helpers::git_name`] made of it).
    pub(super) fn judge(
        &mut self,
        index: usize,
        helper: Option<&Option<String>>,
    ) -> Option<ProtectedActionKind> {
        // #1276: a finder's exec option with its command attached runs it.
        let attached = self.attached(index);
        if attached.is_some() {
            self.fed = true;
            self.placeholders = true;
        }
        let name = basename(attached.unwrap_or(&self.lower[index]));
        if helper.is_none() && name != "git" {
            // A dashed helper whose name lost a placeholder (`git-{}`).
            let dropped = matches!(name, "git-" | "git-.exe");
            return (dropped && self.fed && self.braces).then_some(ProtectedActionKind::UnseenCode);
        }
        let config = |this: &mut Self, from: usize| {
            let fed = this.fed;
            // #1283: the launcher, git or `config` spelled by text the
            // quote-aware reading cannot see, which then cannot check what
            // this reading makes of quoted values.
            let unchecked = this.hidden_launcher || this.hidden(index) || this.hidden(from - 1);
            // #1276 item 3: or the first word after `config` is a value
            // this reading does not know, which may be an option (`-e`).
            let dynamic = this.marks.dynamic.get(from).copied().unwrap_or(false);
            let (reads, positional, unknown) = this.seen();
            // Or a word it cannot know (`"$k"` with `k=core.pager`).
            let unchecked = unchecked || unknown[index + 1];
            ((fed || positional[from] || dynamic) && (unchecked || !reads[from]))
                .then_some(ProtectedActionKind::Persistence)
        };
        if let Some(Some(subcommand)) = helper {
            // A dashed helper: its name gives the subcommand.
            if self.fed && self.input_fills(&self.words[index]) {
                return Some(ProtectedActionKind::UnseenCode);
            }
            return if subcommand == "config" {
                config(self, index + 1)
            } else {
                None
            };
        }
        match self.words.get(index + 1) {
            None => self.fed.then_some(ProtectedActionKind::UnseenCode),
            Some(first) if self.fed && (first.starts_with('-') || self.input_fills(first)) => {
                Some(ProtectedActionKind::UnseenCode)
            }
            Some(_) if self.lower[index + 1] == "config" => config(self, index + 2),
            Some(_) => None,
        }
    }

    /// For each word (and the end), whether as the words after `config`
    /// they read, and whether one of them is a positional parameter or
    /// unchecked (see [`Marks`]).
    fn seen(&mut self) -> &(Vec<bool>, Vec<bool>, Vec<bool>) {
        let words = self.lower;
        let marks = self.marks;
        self.seen.get_or_insert_with(|| {
            let reads = outputs::git_config_reads_each(words);
            let mark = |marks: &[bool], at: usize| marks.get(at).copied().unwrap_or(false);
            let mut positional = vec![false; words.len() + 1];
            let mut unchecked = vec![false; words.len() + 1];
            for (at, word) in words.iter().enumerate().rev() {
                work::step();
                positional[at] = positional[at + 1]
                    || holds_positional(word)
                    || mark(marks.looped, at)
                    || mark(marks.splits, at);
                unchecked[at] = unchecked[at + 1] || mark(marks.unchecked, at);
            }
            (reads, positional, unchecked)
        })
    }
}

/// Whether `word` holds a positional parameter (`$1`, `"$@"`, `${*}`),
/// which the classifier never sees (the substitution step keeps them as
/// written).
fn holds_positional(word: &str) -> bool {
    word.match_indices('$').any(|(at, _)| {
        let after = &word[at + 1..];
        // zsh and fish name them `$argv`.
        let (braced, after) = match after.strip_prefix('{') {
            Some(inside) => (true, inside),
            None => (false, after),
        };
        // #1276: `$_` is the last word of the command before, which may
        // be one of them (`: "$@"; git config $_`).
        let underscore = after.strip_prefix('_').is_some_and(|rest| {
            !rest.starts_with(|ch: char| ch.is_ascii_alphanumeric() || ch == '_')
        });
        underscore
            || after.starts_with("argv")
            || after.chars().next().is_some_and(|ch| {
                ch == '@' || ch == '*' || ch.is_ascii_digit() || (braced && ch == '!')
            })
    })
}

/// #1268: for each of a command's words (as the quoted reader gives them),
/// whether a launcher before it adds input words to the line it runs, and
/// whether it puts them inside that line's words (`xargs -I%`, `find -exec`
/// with a `{}`). `braces`: the line as written holds a brace.
/// And the same after the last word. `inherited`: what a launcher does
/// with its input in the script these words are in.
pub(super) fn launches(texts: &[String], braces: bool, inherited: Launch) -> Vec<Launch> {
    let lower: Vec<String> = texts.iter().map(|text| text.to_lowercase()).collect();
    let mut launched = LaunchedGit::new(texts, &lower, braces, inherited, Marks::default());
    let mut each = Vec::with_capacity(texts.len() + 1);
    for index in 0..=texts.len() {
        work::step();
        each.push(Launch {
            fed: launched.fed,
            replaced: launched.replaces(),
        });
        if index < texts.len() {
            launched.step(index);
        }
    }
    each
}

/// What a launcher does with its input at one word, or in a script it runs.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(super) struct Launch {
    /// It adds input words (after the line it runs).
    pub(super) fed: bool,
    /// It may put them inside its words.
    pub(super) replaced: bool,
}
