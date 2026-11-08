//! #885: what the word lexer cannot see because it removes quotes. A nested
//! shell's script (`bash -c "sh -c './x.sh'"`) is judged like a command
//! line of its own, and a sed program is judged whole: one that may run a
//! shell command, or that saves what it outputs to a file the same line
//! runs, is code the host cannot read.
//!
//! Quoted text is judged wherever it is (as the word lexer does), so a
//! misreading of which text is data cannot hide anything; and a shell or
//! sed command the word lexer finds that this reading does not fails
//! closed.

use super::Classifier;
use super::FileText;
use super::Folders;
use super::GitConfigWrites;
use super::MAX_FOLDER_BYTES;
use super::ProtectedActionKind;
use super::WRITERS;
use super::braces;
use super::certain_cd;
use super::code_tables;
use super::code_variables;
use super::git_config;
use super::git_helpers;
use super::indirect;
use super::invocation;
use super::invocation::Code;
use super::launched_git;
use super::launched_input;
use super::outbound;
use super::outputs;
use super::quoted;
use super::quoted::Command;
use super::quoted::Word;
use super::rerun;
use super::sed;
use super::shadow;
use super::shell::SHELLS;
use super::shell::UNSEEN_OUTPUT;
use super::shell::basename;
use super::unknown_folder;
use super::work;
use std::collections::HashMap;
use std::collections::HashSet;

/// Levels of quoted text read for nested commands. Deeper text is still
/// split by the word lexer, and a command only it sees fails closed.
const MAX_QUOTED_LEVEL: usize = 4;

/// Nested scripts judged for one action; more fail closed.
const MAX_NESTED_SCRIPTS: usize = 256;

/// #1309: folders a line may be in after changes that may not have
/// happened, each judged for the scripts it runs; more and the folder is
/// unknown.
const MAX_POSSIBLE_FOLDERS: usize = 4;

/// Scripts, sed calls and written files one reading collects (#1065);
/// more fail closed.
const MAX_COLLECTED: usize = 4096;

/// Pipeline stages followed from one command for the files they write
/// (#1065): following every stage from every stage is quadratic. Past it
/// the pipeline writes a file the classifier cannot name.
const MAX_PIPELINE_STAGES: usize = 64;

/// Targets retained for one collected `sed`. Each target costs a vector slot
/// plus its known name, and every collected `Sed` clones the list. Keeping
/// both bounded makes those copies constant-size.
const MAX_SAVED_TARGETS: usize = 64;
const MAX_SAVED_TARGET_BYTES: usize = 4 * 1024;

/// Pipeline stages that save their input to the files they are given.
const PIPE_WRITERS: &[&str] = &["tee", "dd", "sponge", "cp", "install"];

/// Pipeline stages that only print a transform of their input. Any other
/// stage (`awk`, `sh -c`, `python3`, `while read`) may write it to a file.
/// `sort` and `uniq` may also save it (`sort -o f`, `uniq - f`).
const PURE_FILTERS: &[&str] = &[
    "cat", "column", "cut", "fmt", "fold", "grep", "egrep", "fgrep", "head", "jq", "nl", "rg",
    "sed", "gsed", "sort", "tac", "tail", "tr", "uniq", "wc",
];

/// What the pass found in one command line.
#[derive(Default)]
struct Found {
    /// #1704: the text read is the code `eval` or `trap` runs, read as
    /// shell like a line's own words.
    evaluated: bool,
    /// Scripts that nested shells run, with the folder they run in, and
    /// what a launcher does with its input there (#1268).
    /// #1283: and whether that folder is unknown.
    scripts: Vec<(String, String, launched_git::Launch, bool)>,
    /// The line as written holds a brace: a launcher's placeholder (`{}`)
    /// may be in it.
    braces: bool,
    seds: Vec<Sed>,
    /// sed commands found (each may be read more than one way).
    sed_commands: usize,
    /// Shells given a `-c` script.
    shell_commands: usize,
    /// Text it could not read in full: substitutions nested too deeply, a
    /// quote left open, or too many places a command may start.
    unreadable: bool,
    /// Files the line writes (`None`: one it cannot name).
    written: Vec<Option<String>>,
    /// A command other than sed writes a file (a copy, move, download or
    /// redirection), which may carry what sed output.
    other_writes: bool,
    /// Redirections and pipes of compound commands (`{ ...; } > f`,
    /// `done | tee f`, `exec 3> f`): any command's output may go there.
    shared_targets: Vec<Option<String>>,
    /// An interpreter runs code that may write files the classifier does
    /// not see (`python3 -c "open('x','w')..."`).
    runs_code: bool,
    /// #1283: what git does with the words as the shell passes them on.
    git: Vec<ProtectedActionKind>,
    /// #1309: the text defines code run later than it is read (a
    /// function, trap or alias), or a loop has started: a script collected
    /// before a later folder change may run after it.
    deferred: bool,
    /// #1309: scripts collected before the last folder change.
    before_change: usize,
    /// #1309: output files named relative to the folder found so far, and
    /// how many were found before the last folder change.
    relative_outputs: usize,
    outputs_before_change: usize,
}

/// One reading of a sed invocation and where it runs.
struct Sed {
    call: sed::Call,
    cwd: String,
    /// Where its output is saved besides in place: its redirections and
    /// the files later pipeline stages write.
    saved_to: Vec<Option<String>>,
    /// It runs inside a substitution: its output is part of another
    /// command's words.
    substituted: bool,
    /// It gets more arguments at run time (`xargs sed`, `parallel sed`)...
    fed: bool,
    /// ...that may go into its words (`xargs -I{}`, `{}`).
    replaced: bool,
    /// #1309: `cwd` is another folder the line may be in, where a change
    /// it followed did not happen: without its program file there, sed
    /// fails.
    other_folder: bool,
    /// #1436: the reading cannot know the folder it runs in, so a program
    /// file named relative to it may be another.
    unknown: bool,
}

/// What quoted text is to the command that gets it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Text that may be code (or data, which is judged the same way).
    Code,
    /// A nested shell's script, judged as a line by `script`.
    Script,
}

fn name(word: &str) -> String {
    basename(&word.to_lowercase()).to_string()
}

/// A word's text when the classifier knows it.
fn known(word: &Word) -> Option<String> {
    (!word.dynamic).then(|| word.text.clone())
}

/// A file a command names, when the classifier can name it: not an
/// expansion and not a pattern (`*.sh`, `r.{a,b}`).
fn target(text: Option<String>) -> Option<String> {
    text.filter(|text| !text.contains(['*', '?', '[', '{']))
}

/// How many shells the word lexer's command runs with a `-c` script.
pub(super) fn shell_commands(words: &[String]) -> usize {
    let (starts, _) = sed::command_starts(words);
    let mut resolved = HashSet::new();
    starts
        .into_iter()
        .filter_map(|start| {
            let (index, shell) = indirect::command_word(&words[start..])?;
            let at = start + index;
            let inline = SHELLS.contains(&shell.as_str())
                && matches!(
                    invocation::parse(&shell, &words[at + 1..]).map(|invocation| invocation.code),
                    Some(Code::Inline(_))
                );
            (inline && resolved.insert(at)).then_some(())
        })
        .count()
}

impl Classifier {
    /// `lexed`: how many sed commands and shells with a `-c` script the
    /// word lexer found in the line.
    pub(super) fn quoted_pass(
        &mut self,
        command: &[String],
        cwd: &str,
        depth: usize,
        lexed: (usize, usize),
    ) {
        // The words as given, and a shell's `-c` script as a line of its
        // own, as the word lexer reads them.
        let mut argv = Command::default();
        let mut scripts = Vec::new();
        let mut previous: Option<&str> = None;
        for arg in command {
            work::step();
            let in_shell = argv
                .words
                .iter()
                .any(|word| SHELLS.contains(&name(&word.text).as_str()));
            let script = in_shell
                && previous.is_some_and(|flag| {
                    flag.starts_with('-') && !flag.starts_with("--") && flag.contains('c')
                });
            if script {
                scripts.push(arg.as_str());
            } else {
                argv.words.push(Word {
                    text: arg.clone(),
                    quoted: true,
                    ..Word::default()
                });
            }
            previous = Some(arg);
        }
        let mut folders = Folders {
            cwd: cwd.to_string(),
            previous: cwd.to_string(),
            stack: std::rc::Rc::default(),
            unfollowed: false,
            unchecked: false,
            others: Vec::new(),
            lost: false,
            aside: if depth == 0 {
                certain_cd::Aside::for_line(command)
            } else {
                certain_cd::Aside::default()
            },
            unseen: false,
            unseen_in_substitution: false,
            searched: false,
            back_unsure: false,
        };
        let mut found = Found {
            braces: command.iter().any(|arg| arg.contains('{')),
            deferred: command.iter().any(|arg| {
                work::step();
                unknown_folder::defines(arg)
            }),
            ..Found::default()
        };
        // The words around the scripts start no launcher's input: under an
        // inherited launch they are the synthetic `sh -c` of `script`, whose
        // script is walked below with that launch (#1273 review 7).
        let inherited = std::mem::take(&mut self.launched);
        self.walk(&[argv], &mut folders, /*level*/ 0, &mut found);
        self.launched = inherited;
        // Shell text a shell would refuse may split differently from this
        // reading (text of other languages is read loosely).
        let strict = self.not_shell == 0 && self.rebuilt == 0;
        for script in scripts {
            work::step();
            let reading = quoted::commands(script);
            // #932 phase 2: compared and logged; decides nothing. Once per
            // action: not again in the Windows view (#1240).
            if !self.homes.is_windows_view() {
                shadow::shadow(script, &reading, &mut self.compared);
            }
            found.unreadable |=
                reading.too_deep || reading.unsure || (strict && reading.unterminated);
            self.walk(
                &reading.commands,
                &mut folders,
                /*level*/ 0,
                &mut found,
            );
        }
        if found.unreadable || folders.unfollowed {
            self.note(ProtectedActionKind::UnseenCode);
        }
        for kind in std::mem::take(&mut found.git) {
            work::step();
            self.note(kind);
        }
        self.written.extend(found.written.iter().cloned());
        // #1309: so may an output file named relative to the folder
        // (`f() { git log --output=.zshenv; }; cd ~; f`, `trap '…' EXIT`).
        if found.deferred && found.outputs_before_change > 0 {
            self.note(ProtectedActionKind::Persistence);
        }
        // #1309: a script collected before a later folder change, in text
        // that may run it later (`f() { sh -c '...'; }; cd ~; f`), may run
        // in that folder.
        if found.deferred {
            for entry in found.scripts.iter_mut().take(found.before_change) {
                work::step();
                entry.3 = true;
            }
        }
        let mut judged = HashSet::new();
        for (script, cwd, launched, unknown) in std::mem::take(&mut found.scripts) {
            work::step();
            if !judged.insert((script.clone(), cwd.clone(), launched, unknown)) {
                continue;
            }
            self.nested_scripts += 1;
            if self.nested_scripts > MAX_NESTED_SCRIPTS {
                self.note(ProtectedActionKind::UnseenCode);
                break;
            }
            let outer = self.launched;
            self.launched = launched_git::Launch {
                fed: outer.fed || launched.fed,
                replaced: outer.replaced || launched.replaced,
            };
            self.unknown_folder += usize::from(unknown);
            self.script(&script, &cwd, depth + 1);
            self.unknown_folder -= usize::from(unknown);
            self.launched = outer;
        }
        // Shared targets are the same for every sed in this reading. If
        // present, they make `elsewhere` true, so judging a sed cannot reach
        // `self.script` and add a new executed file. If absent, this is false
        // regardless of later scripts. Judge them once, without copying their
        // potentially large names for every sed (#1132).
        let shared_may_run = found
            .shared_targets
            .iter()
            .any(|target| self.may_run(target.as_deref()));
        for sed in std::mem::take(&mut found.seds) {
            work::step();
            self.judge_sed(sed, &found, depth, shared_may_run);
        }
        // A command the word lexer finds that this reading does not
        // (spelled through a variable, braces or `$IFS`) fails closed.
        let (lexed_seds, lexed_shells) = lexed;
        if lexed_seds > found.sed_commands || lexed_shells > found.shell_commands {
            self.note(ProtectedActionKind::UnseenCode);
        }
    }

    /// Collect nested scripts, sed invocations and written files from
    /// `commands` and the quoted text in them, following `cd`.
    fn walk(&self, commands: &[Command], folders: &mut Folders, level: usize, found: &mut Found) {
        // #1276: `for k;` loops met so far.
        let mut loops = launched_git::PositionalLoops::default();
        for command in commands {
            work::step();
            if found.scripts.len()
                + found.seds.len()
                + found.shared_targets.len()
                + found.written.len()
                > MAX_COLLECTED
            {
                found.unreadable = true;
                return;
            }
            let texts: Vec<String> = command.words.iter().map(|word| word.text.clone()).collect();
            // D194: where a certain `cd` stops being certain.
            let certain = certain_cd::before(
                folders,
                if command.substituted {
                    certain_cd::Joint::Other
                } else {
                    command.after
                },
                &texts,
                !command.writes_to.is_empty()
                    || command.writes_descriptor
                    || !command.input.is_empty()
                    || !command.other_text.is_empty(),
            );
            // #1573 (1): zsh writes a redirection to `=mytool` (also
            // `="mytool"`) into the program `mytool` on `PATH`, which runs
            // later. Only at this reading's own level: quoted words read
            // again as code (`level` > 0) would turn the `>=` of `awk
            // 'NR>=10'` into one; nested shells' scripts get a level 0 of
            // their own. Not in other languages' files (`if n>=2:`).
            if level == 0
                && (self.not_shell == 0 || self.rebuilt > 0)
                && command.writes_to.iter().any(|word| {
                    work::step();
                    outputs::zsh_program_path(&word.text)
                })
            {
                note_git(found, ProtectedActionKind::Persistence);
            }
            // #1309: a later iteration of a loop starts where the one
            // before it left.
            found.deferred |= unknown_folder::starts_loop(&texts);
            let command_at = indirect::command_word(&texts);
            // #1620: zsh expands `=mytool` in any word, so a writer's operand
            // names the installed program too (`tee -a =mytool`, `cp x
            // =mytool`). And the text `eval`, `trap` and `alias` run is a
            // command line read at this level: its redirection targets and
            // writers' operands count (`eval "echo x >> =mytool"`). The same
            // gate as above.
            if level == 0
                && (self.not_shell == 0 || self.rebuilt > 0)
                && (writes_zsh_program(&command.words, command_at.as_ref())
                    || code_writes_zsh_program(&texts, command_at.as_ref()))
            {
                note_git(found, ProtectedActionKind::Persistence);
            }
            // #1309: a command word spelled as a pattern (`c? ~`, `[c]d ~`,
            // `{c,}d ~`) may be `cd`, `eval` or `source`; a `cd` behind
            // zsh's precommand modifiers (`noglob cd ~`) is not followed.
            if !command.substituted
                && let Some((index, name)) = &command_at
                && (unknown_folder::pattern_word(&texts[*index])
                    || (unknown_folder::MODIFIERS.contains(&name.as_str())
                        && unknown_folder::modified_change(&texts[index + 1..])))
            {
                folders.unchecked = true;
                found.before_change = found.scripts.len();
                found.outputs_before_change = found.relative_outputs;
            }
            // A substitution's `cd` does not move this line.
            if !command.substituted
                && let Some((index, name)) = &command_at
                && unknown_folder::external_change(&texts, *index, name)
            {
                // #1403: `./cd sub` runs that file: the folder is unknown.
                // #1422: so do `CD sub` and `env cd sub`.
                folders.unchecked = true;
            } else if !command.substituted
                && let Some((index, name)) = &command_at
            {
                let unknown = folders.unchecked;
                self.follow_change(
                    name,
                    &texts[index + 1..],
                    folders,
                    &mut HashMap::new(),
                    unknown,
                    certain,
                );
                // #1283: `cd "$d"` goes where this reading cannot follow.
                // #1436: as written, homes expanded: path cleanup would
                // drop `$X` from `$X/..` (and a command's output).
                folders.unchecked |= matches!(name.as_str(), "cd" | "pushd")
                    && texts[index + 1..].iter().any(|text| {
                        work::step();
                        let text = self.homes.expand_homes(text);
                        text.contains(['$', '`']) || outputs::unseen_place(&text)
                    });
            }
            // #1309: nor where `CDPATH`, `cdable_vars`, `OLDPWD`, a
            // sourced file, `eval 'cd ~'`, zsh `chdir`, `cd old new` or the
            // folder stack send it, nor any change in code that may run
            // later (a function, loop, trap or alias).
            let searched = texts.iter().any(|text| {
                work::step();
                unknown_folder::searches(text)
            });
            let (changes, carried, odd) =
                command_at
                    .as_ref()
                    .map_or((false, false, false), |(index, name)| {
                        let args = &texts[index + 1..];
                        let changes = unknown_folder::CHANGERS.contains(&name.as_str());
                        (
                            changes,
                            !command.substituted && unknown_folder::carries_change(name, args),
                            changes
                                && !command.substituted
                                && unknown_folder::odd_change(name, args),
                        )
                    });
            folders.unchecked |= searched || carried || odd || (changes && found.deferred);
            if searched || carried || changes {
                found.before_change = found.scripts.len();
                found.outputs_before_change = found.relative_outputs;
            }
            let first = texts.first().map(|word| name(word));
            let compound = first
                .as_deref()
                .is_none_or(|first| matches!(first, "done" | "fi" | "esac" | "exec"));
            let mut targets: Vec<Option<String>> = command
                .writes_to
                .iter()
                .map(|word| target(known(word)))
                .collect();
            if command.writes_descriptor {
                targets.push(None);
            }
            if compound {
                found.shared_targets.extend(targets.iter().cloned());
                found
                    .shared_targets
                    .extend(pipeline_targets(commands, command));
            }
            found.written.extend(targets);
            // D153: a git setting whose quoted text names git.
            found.unreadable |= git_helpers::setting_holds_git_name(&command.words);
            loops.read(&texts);
            self.judge_git(&texts, &command.words, &loops, found);
            // #1515: one this reading cut at a bracket the shell reads as
            // text (`git -C { config core.pager sh`) may go on past it.
            if command.cut && cut_matters(&command.words) {
                note_git(found, ProtectedActionKind::UnseenCode);
            }
            // #1268: where a launcher's input reaches the words, and the
            // scripts of shells it puts input into, wherever the command
            // word is read to be (`xargs --replace sh -c '... {}'`).
            let launches = launched_git::launches(&texts, found.braces, self.launched);
            // #1276: a launcher's input as the program it runs (`xargs -I%
            // % config`), as an interpreter's code (`xargs -I% python3 -c
            // '...%...'`), or as whole command lines (`parallel < f`). In
            // quoted text read as code (`level` > 0), only where input is
            // put in place.
            // #1276 follow-up: also in the text `eval` or `trap` runs as a
            // shell line, which is walked as quoted data when it holds no
            // quotes of its own (`eval 'xargs env < f'`).
            if launched_input::input_runs(&texts, level > 0)
                || (level == 0
                    && command_at.as_ref().is_some_and(|(index, name)| {
                        launched_input::evaluated_input_runs(name, &texts[index + 1..])
                    }))
            {
                note_git(found, ProtectedActionKind::UnseenCode);
            }
            // #1283: scripts run in a folder this reading does not follow
            // (after `cd "$d"`, or a wrapper's: `env -C "$d" sh -c`).
            // #1309: in any of a wrapper's spellings (`env -C"$d"`, `sudo
            // -D`, `nsenter --wd=`, `find -execdir`).
            let elsewhere_from = unknown_folder::elsewhere_at(&texts, self.sudo_unsure());
            let elsewhere = elsewhere_from.is_some();
            let unknown = folders.unchecked || elsewhere;
            for (word, script) in replaced_shell_scripts(&texts, &launches, &mut found.unreadable) {
                work::step();
                push_script(found, folders, script, replaced(launches[word]), unknown);
            }
            // #1309: an option the command-word reading does not know may
            // hide the shell such a command runs (`env -iC ~ sh -c`,
            // `docker exec -w ~ c sh -c`): its scripts, wherever they are.
            let shell_command = command_at
                .as_ref()
                .is_some_and(|(_, name)| SHELLS.contains(&name.as_str()));
            if elsewhere && !shell_command {
                let anywhere = vec![
                    launched_git::Launch {
                        fed: false,
                        replaced: true,
                    };
                    texts.len() + 1
                ];
                for (word, script) in
                    replaced_shell_scripts(&texts, &anywhere, &mut found.unreadable)
                {
                    work::step();
                    push_script(
                        found,
                        folders,
                        script,
                        replaced(launches[word]),
                        /*unknown*/ true,
                    );
                }
            }
            // #1309: so may the git it runs, and its output files (`docker
            // exec -w ~ c git log --output=f`, `env -iC ~ git config -f f`):
            // read from the first git, when it is after the folder option
            // (git's own `-C` is followed by `judge_outputs`).
            // #1606: the first git after it, so an option value spelled
            // `git` before it (`env -u git -iC ~ git log`) hides none; but
            // not where the command is git and the option its own.
            if let Some(from) = elsewhere_from
                && !own_git_folder(&texts, from)
                && let Some(at) = (from + 1..texts.len()).find(|at| {
                    work::step();
                    let program = name(&texts[*at]);
                    program == "git"
                        || git_helpers::git_name(&program).is_some()
                        || indirect::equals_git(&texts[*at])
                })
                && outputs::output_files(
                    &if indirect::equals_git(&texts[at]) {
                        "git".to_string()
                    } else {
                        name(&texts[at])
                    },
                    &texts[at + 1..],
                    /*unchecked*/ false,
                )
                .is_some_and(|files| {
                    files
                        .iter()
                        .any(|file| self.unknown_folder_file(folders, /*unknown*/ true, file))
                })
            {
                note_git(found, ProtectedActionKind::Persistence);
            }
            let (starts, overflow) = sed::command_starts(&texts);
            found.unreadable |= overflow;
            let mut roles = vec![Role::Code; texts.len()];
            let mut input_role = Role::Code;
            let mut resolved = HashSet::new();
            // Words before it were checked as wrappers (starts ascend).
            let mut wrappers_checked = 0;
            let mut joined_left = MAX_JOINED_BYTES;
            // #1685: a value set in zsh's `functions`, `aliases`,
            // `galiases` or `saliases` or bash's `BASH_ALIASES` is code run
            // by name later (`functions[f]='tee -a ~/.zshrc'; f`): a line of
            // its own too, within the same budget.
            let mut tables = 0;
            // An array form (`aliases+=(x '…')`) is not read, nor is a
            // command writing a table by name (`zparseopts -a functions`,
            // `set -A aliases …`), nor any other write to one: they fail
            // closed (#1704).
            // #1704: words that may be any or several where a writer reads
            // its options count where text is read as shell: at this
            // reading's own level or for `eval`/`trap` text (not `awk
            // '{print $2}'` read again as code), as for #937's arithmetic.
            let seen: Option<Vec<code_tables::Seen>> = ((level == 0 || found.evaluated)
                && self.not_shell == 0
                && self.rebuilt == 0)
                .then(|| {
                    command
                        .words
                        .iter()
                        .map(|word| {
                            work::step();
                            code_tables::Seen::of(word)
                        })
                        .collect()
                });
            // Main's reading stays a floor: the reading with quotes reads
            // some words otherwise (`eval $'getln *.png'`).
            let stat_builtin = self.stat_builtin;
            found.unreadable |=
                code_tables::writes_table(&texts, &texts, /*seen*/ None, stat_builtin)
                    || seen.is_some_and(|seen| {
                        code_tables::writes_table(&texts, &texts, Some(&seen), stat_builtin)
                    });
            'tables: for (at, text) in texts.iter().enumerate() {
                work::step();
                let (values, array) = code_table_values(text);
                found.unreadable |=
                    array || code_tables::sets_table_otherwise(text, !values.is_empty());
                for value in values {
                    work::step();
                    tables += 1;
                    let Some(rest) = joined_left
                        .checked_sub(value.len() + 1)
                        .filter(|_| tables <= outbound::MAX_JUDGED_POSITIONS)
                    else {
                        found.unreadable = true;
                        break 'tables;
                    };
                    joined_left = rest;
                    push_script(
                        found,
                        folders,
                        value.to_string(),
                        replaced(launches[at]),
                        unknown,
                    );
                }
            }
            let mut runners = 0;
            // #1449: one runner's options are read to the end of the
            // command; any other's up to `MAX_JUDGED_POSITIONS` words.
            let mut read_whole = true;
            let mut watched = false;
            let mut sed_command = false;
            for start in starts {
                work::step();
                let command_at = indirect::command_word(&texts[start..]);
                // #1268: with no command word, the line may be in an
                // option (`env -S'git config k v'`, `--split-string=...`).
                let last = match &command_at {
                    Some((index, _)) => start + index,
                    None if texts[start].starts_with('#') => continue,
                    None => texts.len() - 1,
                };
                // Wrappers that run a string as a shell line (`watch`,
                // `su -c`, `flock f -c`, `env -S`). Only the outermost
                // `watch` counts: an inner one is part of its line.
                for wrapper in start.max(wrappers_checked)..=last {
                    work::step();
                    wrappers_checked = wrapper + 1;
                    let is_watch = name(&texts[wrapper]) == "watch";
                    if is_watch && std::mem::replace(&mut watched, true) {
                        continue;
                    }
                    let runs = runner_scripts(
                        &texts,
                        wrapper,
                        last,
                        &mut read_whole,
                        &mut joined_left,
                        &mut found.unreadable,
                    );
                    for (word, script, whole) in runs {
                        work::step();
                        // #1449: `rerun::OPERAND_RUNNERS` values stay read
                        // as code too.
                        if whole {
                            roles[word] = Role::Script;
                        }
                        // Input words a launcher adds go to the wrapper's
                        // line (`xargs env -S'git config'`).
                        push_script(found, folders, script, launches[wrapper], unknown);
                        runners += 1;
                    }
                    // Too many shell strings in one command: fail closed.
                    if runners > outbound::MAX_JUDGED_POSITIONS {
                        found.unreadable = true;
                        break;
                    }
                }
                let Some((index, resolved_name)) = command_at else {
                    continue;
                };
                let at = start + index;
                if !resolved.insert(at) {
                    continue;
                }
                if sed::NAMES.contains(&resolved_name.as_str()) {
                    sed_command = true;
                } else {
                    // #1141: an output option's file (`git log --output=f`).
                    let outputs = outputs::output_files(
                        &resolved_name,
                        &texts[at + 1..],
                        /*unchecked*/ false,
                    );
                    if let Some(files) = &outputs {
                        // A wrapper's folder counts too (`env -C ~ git`).
                        // #1309: in each folder the line may be in.
                        for cwd in std::iter::once(&folders.cwd).chain(&folders.others) {
                            work::step();
                            self.judge_outputs(&texts, files, cwd, found);
                        }
                        // #1309: the word reading's rule (see
                        // `unknown_folder_file`), with this reading's
                        // folder; a wrapper before the command moved it
                        // (`sudo -D ~ git config -f .zshenv …`).
                        let moved = elsewhere_from.is_some_and(|from| from < at);
                        if files.iter().any(|file| {
                            self.unknown_folder_file(folders, moved || folders.unchecked, file)
                        }) {
                            note_git(found, ProtectedActionKind::Persistence);
                        }
                        if files.iter().any(|file| self.relative_place(file)) {
                            found.relative_outputs += 1;
                        }
                    }
                    if WRITERS.contains(&resolved_name.as_str())
                        || outputs.is_some()
                        || !command.writes_to.is_empty()
                    {
                        found.other_writes = true;
                        found.written.extend(
                            texts[at + 1..]
                                .iter()
                                .filter(|word| !word.starts_with('-'))
                                .map(|word| target(Some(file_operand(word).to_string()))),
                        );
                        found
                            .written
                            .extend(outputs.into_iter().flatten().map(|file| target(Some(file))));
                    }
                }
                // #1268: GNU parallel joins its command words, adds its
                // input and runs the line with a shell (`parallel 'cd x &&
                // git config'`).
                if launched_git::PARALLELS.contains(&resolved_name.as_str()) {
                    let end = texts[at + 1..]
                        .iter()
                        .position(|word| word.starts_with(":::"))
                        .map_or(texts.len(), |offset| at + 1 + offset);
                    if end > at + 1 {
                        let line = joined(
                            String::new(),
                            &texts[at + 1..end],
                            &mut joined_left,
                            &mut found.unreadable,
                        );
                        let launch = launched_git::Launch {
                            fed: true,
                            replaced: launches[end].replaced,
                        };
                        push_script(found, folders, line, launch, unknown);
                    }
                }
                if resolved_name == "eval" {
                    // #1283: `eval` joins its words and parses them again, so
                    // quoting split across them takes effect (`eval git config
                    // --comment \'x --get \' core.pager sh`): the line it runs
                    // is judged as a script. Words without quotes or
                    // backslashes parse back the same (and their quoted
                    // text is walked below), so a chain of `eval`s is read
                    // once.
                    let requotes = texts[at + 1..].iter().any(|text| {
                        work::step();
                        text.contains(['\'', '"', '\\'])
                    });
                    if requotes {
                        let line = joined(
                            String::new(),
                            &texts[at + 1..],
                            &mut joined_left,
                            &mut found.unreadable,
                        );
                        push_script(found, folders, line, replaced(launches[at]), unknown);
                    }
                    continue;
                }
                // #1685: an alias's value is a line of its own, run later
                // (`alias x='tee -a ~/.zshrc'`): every check of a line counts
                // on it. (The word lexer reads `eval`'s and `trap`'s text.)
                // Within the command's budget for joined lines.
                if resolved_name == "alias" {
                    for (nth, value) in texts[at + 1..]
                        .iter()
                        .filter_map(|arg| arg.split_once('=').map(|(_, value)| value))
                        .enumerate()
                    {
                        work::step();
                        let Some(rest) = joined_left
                            .checked_sub(value.len() + 1)
                            .filter(|_| nth < outbound::MAX_JUDGED_POSITIONS)
                        else {
                            found.unreadable = true;
                            break;
                        };
                        joined_left = rest;
                        push_script(
                            found,
                            folders,
                            value.to_string(),
                            replaced(launches[at]),
                            unknown,
                        );
                    }
                }
                // #1685: trap's action, when it holds quotes or backslashes
                // the word lexer drops, is a line of its own read with them
                // (as `eval`'s, #1283). Not on a line where D191 found every
                // trap only removes temp paths, whose variables a line of its
                // own would not know.
                if resolved_name == "trap" && !self.temp_traps {
                    let args = &texts[at + 1..];
                    let args = args.strip_prefix(&["--".to_string()]).unwrap_or(args);
                    if let Some(action) = args.first()
                        && action.contains(['\'', '"', '\\'])
                    {
                        match joined_left.checked_sub(action.len() + 1) {
                            Some(rest) => {
                                joined_left = rest;
                                push_script(
                                    found,
                                    folders,
                                    action.clone(),
                                    replaced(launches[at]),
                                    unknown,
                                );
                            }
                            None => found.unreadable = true,
                        }
                    }
                }
                // #1422: `compgen -C cmd` runs `cmd` as shell code.
                if resolved_name == "compgen" {
                    match rerun::compgen_commands(command.words.get(at + 1..).unwrap_or_default()) {
                        Some(codes) => {
                            for code in codes {
                                work::step();
                                push_script(found, folders, code, replaced(launches[at]), unknown);
                            }
                        }
                        None => found.unreadable = true,
                    }
                }
                let shell = SHELLS.contains(&resolved_name.as_str());
                let code = invocation::parse(&resolved_name, &texts[at + 1..])
                    .map(|invocation| invocation.code);
                found.runs_code |= code.is_some() && !shell;
                match code {
                    Some(Code::Inline(inline)) if shell => {
                        found.shell_commands += 1;
                        let Some(inline) = inline else {
                            continue;
                        };
                        let Some(candidates) = script_candidates(&texts, at, inline) else {
                            continue;
                        };
                        for candidate in candidates {
                            work::step();
                            roles[candidate] = Role::Script;
                            push_script(
                                found,
                                folders,
                                texts[candidate].clone(),
                                replaced(launches[at]),
                                unknown,
                            );
                        }
                    }
                    // A here-string or here-document the shell reads.
                    Some(Code::Stdin) if shell => {
                        input_role = Role::Script;
                        for input in &command.input {
                            work::step();
                            push_script(
                                found,
                                folders,
                                input.text.clone(),
                                replaced(launches[at]),
                                unknown,
                            );
                        }
                    }
                    _ => {}
                }
            }
            if sed_command {
                collect_seds(commands, command, &texts, folders, found);
            }
            if level >= MAX_QUOTED_LEVEL {
                // #1283: git in text this reading does not reach is judged
                // only by the word lexer, which splits quoted values.
                let deeper = command
                    .words
                    .iter()
                    .filter(|word| word.quoted)
                    .chain(&command.input)
                    .chain(&command.writes_to)
                    .chain(&command.other_text)
                    .any(|word| {
                        work::step();
                        spells_git(&word.text)
                    });
                if deeper {
                    note_git(found, ProtectedActionKind::UnseenCode);
                }
                continue;
            }
            // #1704: `eval` and `trap` run their text as shell, also behind
            // zsh's modifiers and `repeat` (`noglob eval '…'`).
            let mut evaluates = false;
            let mut program = command_at.clone();
            while let Some((at, name)) = program {
                work::step();
                if matches!(name.as_str(), "eval" | "trap") {
                    evaluates = true;
                    break;
                }
                // zsh's `repeat N` runs the command after its count.
                let skip = match name.as_str() {
                    "repeat" => 2,
                    name if unknown_folder::MODIFIERS.contains(&name) => 1,
                    _ => break,
                };
                let from = (at + skip).min(texts.len());
                program =
                    indirect::command_word(&texts[from..]).map(|(next, name)| (from + next, name));
            }
            // Also redirection files, comments and other text the shell
            // does not pass on as words.
            let quoted = command
                .words
                .iter()
                .zip(roles)
                .filter(|(word, _)| word.quoted)
                .chain(command.input.iter().map(|input| (input, input_role)))
                .chain(
                    command
                        .writes_to
                        .iter()
                        .filter(|word| word.quoted)
                        .chain(&command.other_text)
                        .map(|word| (word, Role::Code)),
                );
            for (word, role) in quoted {
                work::step();
                // Quoted text runs in a shell of its own, if at all: its
                // `cd` does not move this line.
                let mut inner = Folders {
                    cwd: folders.cwd.clone(),
                    previous: folders.previous.clone(),
                    stack: folders.stack.clone(),
                    unfollowed: false,
                    unchecked: folders.unchecked,
                    others: folders.others.clone(),
                    lost: folders.lost,
                    aside: folders.aside.inner(),
                    unseen: folders.unseen,
                    unseen_in_substitution: folders.unseen_in_substitution,
                    searched: folders.searched,
                    back_unsure: folders.back_unsure,
                };
                let reading = quoted::commands(&word.text);
                found.unreadable |= reading.too_deep || reading.unsure;
                if role == Role::Code {
                    found.deferred |= unknown_folder::defines(&word.text);
                    // Commands in data the word lexer does not read as
                    // command lines do not count: a decoy (`echo 'sed p'`)
                    // must not make up for a command this reading missed.
                    let counts = (found.sed_commands, found.shell_commands);
                    // Only its words; not its input, files or comments.
                    let words = command.words.as_ptr_range();
                    let own = words.contains(&std::ptr::from_ref(word));
                    let evaluated = std::mem::replace(&mut found.evaluated, evaluates && own);
                    self.walk(&reading.commands, &mut inner, level + 1, found);
                    found.evaluated = evaluated;
                    found.unreadable |= inner.unfollowed;
                    if !word.lines {
                        (found.sed_commands, found.shell_commands) = counts;
                    }
                    continue;
                }
                // A nested shell's script is judged when `script` reads
                // it: here its commands are only counted (unless the word
                // lexer keeps it as one word, `bash -c $'...'`).
                let mut counted = Found::default();
                self.walk(&reading.commands, &mut inner, level + 1, &mut counted);
                if !word.whole {
                    found.sed_commands += counted.sed_commands;
                    found.shell_commands += counted.shell_commands;
                }
                found.unreadable |= counted.unreadable || inner.unfollowed;
            }
        }
        certain_cd::finish(folders);
    }

    /// #1283: the git checks of `simple_command` on the words as the shell
    /// passes them on. The word lexer splits quoted text, so a value's text
    /// can read as an option there (`git config --comment 'x --get'
    /// core.pager sh` as a read); here each value stays one word. Git
    /// spelled by text this reading cannot see is checked there instead
    /// (`hidden_words`).
    /// D156: git's options are read here too. The lexer splits a value at
    /// `;`, `|` or `&` (`git -c 'user.name=a; b' -c core.pager=sh log`), so
    /// its scan from `git` ends before the settings after that value; here
    /// it reads to the command's end. This only adds findings.
    /// `loops`: loop variables over the positional parameters so far.
    fn judge_git(
        &self,
        texts: &[String],
        words: &[Word],
        loops: &launched_git::PositionalLoops,
        found: &mut Found,
    ) {
        self.judge_git_words(texts, words, loops, found);
        // #1636: and on the words as the shell makes them. This reading cuts
        // the command where the shell does, so an option a brace group or an
        // empty expansion makes is seen even where the word lexer cuts the
        // command first (`git clone r d {-q,--upload-pack=./x}|cat`,
        // `-b }x`, `$((1))`, a comment) or does not expand the group
        // (`--{u..u}pload-pack`, `${N}{--upload-pack=./x,}`).
        // As bash 5 and zsh expand braces, and as bash 3.2 does.
        let mut previous: Option<Vec<String>> = None;
        for old_bash in [false, true] {
            work::step();
            let Some(made) = shell_made(words, old_bash) else {
                continue;
            };
            let made_texts: Vec<String> = made.words.iter().map(|word| word.text.clone()).collect();
            if previous.as_ref() == Some(&made_texts) {
                continue;
            }
            self.judge_git_words(&made_texts, &made.words, loops, found);
            // Past a bound, the words not made may hold git's name or an
            // option.
            let spelled = texts.iter().chain(&made_texts).any(|text| spells_git(text));
            if made.unsure(spelled) {
                note_git(found, ProtectedActionKind::UnseenCode);
            }
            previous = Some(made_texts);
        }
    }

    /// [`Self::judge_git`] on one reading of the command's words.
    fn judge_git_words(
        &self,
        texts: &[String],
        words: &[Word],
        loops: &launched_git::PositionalLoops,
        found: &mut Found,
    ) {
        if !texts.iter().any(|text| text.to_lowercase().contains("git")) {
            return;
        }
        let lower: Vec<String> = texts.iter().map(|text| text.to_lowercase()).collect();
        let looped = loops.marks(texts);
        // #1276 item 3: values this reading does not know.
        let splits: Vec<bool> = words
            .iter()
            .map(|word| word.dynamic && word.splits)
            .collect();
        let dynamic: Vec<bool> = words.iter().map(|word| word.dynamic).collect();
        let mut config = GitConfigWrites::new(&lower, &[], &[]);
        let mut launched = launched_git::LaunchedGit::new(
            texts,
            &lower,
            found.braces,
            self.launched,
            launched_git::Marks {
                looped: &looped,
                splits: &splits,
                dynamic: &dynamic,
                ..launched_git::Marks::default()
            },
        );
        let mut git_programs_read = false;
        let mut local_config = git_config::Writes::new(texts);
        let command_at = indirect::command_word(texts);
        // D156/D258: the other lexer may cut this command at quoted text
        // before a brace-generated option. In a line of plain words
        // (`brace_plain`) quote history is command-local and only unquoted
        // brace groups count: a quote in a later command or literal `{a,b}`
        // cannot cause this. In any other line, D156's reading: the line's
        // quotes and any `,` or `..` between braces.
        let mut braced_from = vec![false; texts.len() + 1];
        for at in (0..texts.len()).rev() {
            work::step();
            let expands = if self.brace_unsure {
                // As D156 reads it: a `,` or `..` between braces.
                let text = &texts[at];
                text.find('{').is_some_and(|open| {
                    work::rfind(text, "}").is_some_and(|close| {
                        open < close && {
                            let inside = &text[open..close];
                            inside.contains(',') || inside.contains("..")
                        }
                    })
                })
            } else {
                words[at].quoted_before && words[at].brace_expansion
            };
            braced_from[at] = braced_from[at + 1] || expands;
        }
        for (index, word) in lower.iter().enumerate() {
            work::step();
            // #1276: or a finder's exec option with git attached.
            let git_word = launched.attached(index).unwrap_or(word);
            let helper = git_helpers::git_name(git_word);
            let subcommand = helper.as_ref().and_then(Option::as_deref);
            let mut kinds = Vec::new();
            if (basename(git_word) == "git" || helper.is_some())
                && config.persists(index + 1, subcommand)
            {
                kinds.push(ProtectedActionKind::Persistence);
            }
            // D156: as `simple_command` reads them (#1142, #1159), on these
            // words; as written, so `raw` is the same words.
            let is_git = basename(git_word) == "git" || indirect::equals_git(git_word);
            let args = &texts[index + 1..];
            let git_program = (is_git || helper.is_some())
                && !std::mem::replace(&mut git_programs_read, true)
                && rerun::git_runs_program(args, subcommand);
            if (is_git && code_variables::git_runs_code_after_git(args, args))
                || (helper == Some(None) && code_variables::git_runs_code(args, args))
                || git_program
                || ((is_git || helper.is_some()) && self.quoted_text && braced_from[index + 1])
            {
                kinds.push(ProtectedActionKind::UnseenCode);
            }
            // D156: and local config writes (#1161), with the same
            // exemption for wrappers that only inspect the command.
            let inspecting = command_at.as_ref().is_some_and(|(at, _)| {
                *at == index && git_config::inspection_only_prefix(&texts[..index])
            });
            let launched_kind = launched.judge(index, helper.as_ref());
            if (basename(git_word) == "git" || helper.is_some())
                && !inspecting
                && launched_kind != Some(ProtectedActionKind::UnseenCode)
                && local_config.writes_code_setting(index + 1, subcommand)
            {
                kinds.push(ProtectedActionKind::Persistence);
            }
            kinds.extend(launched_kind);
            launched.step(index);
            for kind in kinds {
                work::step();
                note_git(found, kind);
            }
        }
    }

    /// #1283: the files an output option names (`git config -f ~/.zshenv
    /// --comment x,--get k v`), with quoted values kept whole, resolved
    /// against the working folder and every `-C`-style folder in `words`
    /// (the command's, wrappers' included).
    fn judge_outputs(&self, words: &[String], files: &[String], cwd: &str, found: &mut Found) {
        const MAX_BASES: usize = 8;
        let mut bases = vec![cwd.to_string()];
        let mut previous: Option<&str> = None;
        // D217: sudo's `-C N` names no folder.
        let closes = unknown_folder::sudo_close_from(words, self.sudo_unsure());
        for (at, word) in words.iter().enumerate() {
            work::step();
            let value = match previous {
                Some(flag)
                    if super::FOLDER_FLAGS.contains(&flag)
                        && !(at > 0 && closes.binary_search(&(at - 1)).is_ok()) =>
                {
                    Some(word.as_str())
                }
                _ => word
                    .split_once('=')
                    .filter(|(flag, _)| {
                        super::FOLDER_FLAGS.contains(flag) && closes.binary_search(&at).is_err()
                    })
                    .map(|(_, value)| value),
            };
            previous = Some(word);
            let Some(value) = value else {
                continue;
            };
            if bases.len() + 2 > MAX_BASES || value.len() > super::MAX_FOLDER_BYTES {
                // Too many folders, or one too long, to follow: fail closed.
                note_git(found, ProtectedActionKind::Persistence);
                return;
            }
            // Against the folder before it and the working folder.
            let last = bases.last().cloned().unwrap_or_default();
            bases.push(self.homes.resolve(value, &last));
            bases.push(self.homes.resolve(value, cwd));
        }
        let writes_home = files.iter().any(|file| {
            bases.iter().any(|base| {
                work::step();
                let resolved = self.homes.resolve(file, base);
                self.homes.is_persistence(&resolved)
                    || (file.ends_with('/') && self.homes.holds_a_home(&resolved))
            })
        });
        if writes_home {
            note_git(found, ProtectedActionKind::Persistence);
        }
    }

    /// A sed that may run a shell command, whose program or options the
    /// classifier cannot read, or that saves what it outputs to a file the
    /// line runs is code the host cannot read; an in-place edit that only
    /// keeps, drops or adds lines has the text it adds judged instead.
    fn judge_sed(&mut self, sed: Sed, line: &Found, depth: usize, shared_may_run: bool) {
        // #1240: judged once, by the first judgment (it reads program
        // files; the Windows view does not open files).
        if self.homes.is_windows_view() {
            return;
        }
        let call = &sed.call;
        let dynamic = call
            .programs
            .iter()
            .chain(&call.files)
            .any(|word| word.dynamic);
        // Arguments added at run time may be the program or options
        // (GNU sed reads options anywhere before `--`).
        let fed = sed.fed
            && !call.bsd
            && (sed.replaced
                || !call.options_ended
                || (call.programs.is_empty() && call.files.is_empty()));
        if call.unknown || fed || (dynamic && self.rebuilt == 0) {
            self.note(ProtectedActionKind::UnseenCode);
            return;
        }
        let mut chunks: Vec<String> = call.programs.iter().map(|word| word.text.clone()).collect();
        for file in &call.files {
            work::step();
            // A script file the action writes, or that code the classifier
            // cannot follow may write, may differ when sed runs.
            let rewritten = line.runs_code
                || !self.ran.is_empty()
                || self.written.iter().any(|written| {
                    written
                        .as_deref()
                        .is_none_or(|written| name(written) == name(&file.text))
                });
            // #1436: the file here is still read.
            if sed.unknown && self.relative_place(&file.text) {
                self.note(ProtectedActionKind::UnseenCode);
            }
            let path = self.homes.resolve(&file.text, &sed.cwd);
            match (rewritten, self.read_file(&path, &file.text, &sed.cwd)) {
                (false, FileText::Text(text)) => chunks.push(text),
                (false, FileText::Missing) if sed.other_folder => return,
                _ => {
                    self.note(ProtectedActionKind::UnseenCode);
                    return;
                }
            }
        }
        // GNU sed joins the pieces with newlines; each is also read alone.
        let mut readings = vec![sed::program(&chunks.join("\n"))];
        if chunks.len() > 1 {
            readings.extend(chunks.iter().map(|chunk| sed::program(chunk)));
        }
        if readings.iter().any(|reading| reading.runs) {
            self.note(ProtectedActionKind::UnseenCode);
            return;
        }
        let mut targets = sed.saved_to.clone();
        targets.extend(readings.iter().flat_map(|reading| {
            reading
                .writes
                .iter()
                .cloned()
                .map(|file| target(Some(file)))
        }));
        // Output captured into another command's words.
        if sed.substituted && line.other_writes {
            targets.push(None);
        }
        let elsewhere = !targets.is_empty() || !line.shared_targets.is_empty();
        if !elsewhere && !call.in_place {
            return;
        }
        // Another command may copy or move what sed saved.
        if line.other_writes {
            targets.push(None);
        }
        if call.in_place {
            targets.extend(call.operands.iter().map(|word| target(known(word))));
        }
        if !shared_may_run && !targets.iter().any(|target| self.may_run(target.as_deref())) {
            return;
        }
        if elsewhere || !readings.iter().all(|reading| reading.keeps_lines) {
            self.note(ProtectedActionKind::UnseenCode);
            return;
        }
        // The edited file holds its own lines and this text: running the
        // file reads the former, the text is judged here.
        let texts: Vec<String> = readings
            .iter()
            .flat_map(|reading| reading.texts.iter().cloned())
            .collect();
        for text in texts {
            work::step();
            self.script(&text, &sed.cwd, depth + 1);
        }
    }

    /// #1309: follow a folder change and keep every folder the line may be
    /// in after it: a `cd` may fail, or run behind a condition, in a
    /// subshell or in a background job. Those are the folders before it and
    /// its target from each of them; past `MAX_POSSIBLE_FOLDERS`, or one
    /// too long, the folder is unknown. `unknown`: the reading already
    /// cannot know its folder. `certain` (D194): what follows runs only
    /// where the change happened, so the folders before it are set aside.
    pub(super) fn follow_change(
        &self,
        name: &str,
        args: &[String],
        folders: &mut Folders,
        variables: &mut HashMap<String, String>,
        unknown: bool,
        certain: bool,
    ) {
        let before = folders.cwd.clone();
        if !self.change_folder(name, args, folders, variables) {
            return;
        }
        if unknown {
            folders.others.clear();
            return;
        }
        // A relative target is reached from each folder; any other
        // (`cd`, `popd`, an absolute one) is the same from all.
        let relative = matches!(name, "cd" | "pushd")
            .then(|| {
                args.iter()
                    .find(|word| !word.starts_with('-') || *word == "-")
            })
            .flatten()
            .filter(|target| *target != "-");
        let mut possible = vec![before];
        possible.append(&mut folders.others);
        let mut reached = Vec::new();
        for from in &possible {
            work::step();
            reached.push(match relative {
                Some(target) => self.homes.resolve(target, from),
                None => folders.cwd.clone(),
            });
        }
        if certain {
            // D194: what follows runs only where the change happened.
            certain_cd::set_aside(folders, std::mem::replace(&mut possible, reached));
        } else {
            possible.extend(reached);
        }
        for folder in possible {
            work::step();
            if folder != folders.cwd && !folders.others.contains(&folder) {
                folders.others.push(folder);
            }
        }
        too_many(folders);
    }

    /// #1309: the folders a command's relative words resolve against from
    /// each of `others` after its folder flags' values (`flags`; see
    /// `simple_command`), less those in `bases`.
    pub(super) fn other_bases(
        &self,
        others: &[String],
        flags: &[&str],
        bases: &[String],
    ) -> Vec<String> {
        let mut found: Vec<String> = Vec::new();
        for other in others {
            work::step();
            let folder = flags
                .last()
                .map_or_else(|| other.clone(), |flag| self.homes.resolve(flag, other));
            let mut chained: Vec<String> = Vec::new();
            for flag in flags {
                work::step();
                let next = self
                    .homes
                    .resolve(flag, chained.last().unwrap_or(other).as_str());
                chained.push(next);
            }
            for base in super::folder_bases(&folder, &chained, other) {
                work::step();
                if !bases.contains(&base) && !found.contains(&base) {
                    found.push(base);
                }
            }
        }
        found
    }

    /// Whether the action runs a script that may be `target` (by name;
    /// `None` is a file the classifier cannot name).
    fn may_run(&self, target: Option<&str>) -> bool {
        if self.ran_unknown {
            return true;
        }
        match target {
            None => !self.ran.is_empty(),
            Some(target) => {
                let target = name(target);
                self.ran.iter().any(|ran| name(ran) == target)
            }
        }
    }
}

/// #1309: past `MAX_POSSIBLE_FOLDERS`, or with one too long, the folder
/// is unknown.
pub(super) fn too_many(folders: &mut Folders) {
    if folders.others.len() >= MAX_POSSIBLE_FOLDERS
        || folders
            .others
            .iter()
            .any(|folder| folder.len() > MAX_FOLDER_BYTES)
    {
        folders.unchecked = true;
        folders.lost = true;
        folders.others.clear();
    }
}

/// #1309: a script, judged in every folder the line may be in (once when
/// its folder is unknown anyway).
fn push_script(
    found: &mut Found,
    folders: &Folders,
    script: String,
    launch: launched_git::Launch,
    unknown: bool,
) {
    if !unknown {
        for other in &folders.others {
            work::step();
            found
                .scripts
                .push((script.clone(), other.clone(), launch, false));
        }
    }
    found
        .scripts
        .push((script, folders.cwd.clone(), launch, unknown));
}

fn collect_seds(
    commands: &[Command],
    command: &Command,
    texts: &[String],
    folders: &Folders,
    found: &mut Found,
) {
    // Where its output goes: its redirections, then the later stages of
    // its pipeline.
    let mut saved_to: Vec<Option<String>> = command
        .writes_to
        .iter()
        .map(|word| target(known(word)))
        .collect();
    if command.writes_descriptor {
        saved_to.push(None);
    }
    saved_to.extend(pipeline_targets(commands, command));
    let saved_to = byte_bounded_targets(saved_to);
    for at in sed::positions(texts) {
        work::step();
        found.sed_commands += 1;
        let fed = texts[..at]
            .iter()
            .any(|word| matches!(name(word).as_str(), "xargs" | "parallel"));
        let replaced = fed
            && (texts[..at].iter().any(|word| {
                ["-I", "-i", "-J", "--replace"]
                    .iter()
                    .any(|flag| word.starts_with(flag))
            }) || texts[at + 1..].iter().any(|word| word.contains("{}")));
        for call in sed::calls(&command.words[at + 1..]) {
            work::step();
            // #1309: in each folder the line may be in.
            for (other_folder, cwd) in std::iter::once((false, &folders.cwd))
                .chain(folders.others.iter().map(|other| (true, other)))
            {
                work::step();
                found.seds.push(Sed {
                    call: call.clone(),
                    cwd: cwd.clone(),
                    saved_to: saved_to.clone(),
                    substituted: command.substituted,
                    fed,
                    replaced,
                    other_folder,
                    unknown: folders.unchecked || folders.unseen || folders.lost,
                });
            }
        }
    }
}

/// Files the later stages of `command`'s pipeline may write what it
/// outputs to (`None`: ones the classifier cannot name).
fn pipeline_targets(commands: &[Command], command: &Command) -> Vec<Option<String>> {
    let mut targets = Vec::new();
    if command.piped && command.pipe_to.is_none() {
        targets.push(None);
    }
    let mut stage = command.pipe_to;
    let mut steps = 0;
    while let Some(index) = stage
        && steps < commands.len()
    {
        work::step();
        if steps == MAX_PIPELINE_STAGES {
            targets.push(None);
            break;
        }
        steps += 1;
        let Some(consumer) = commands.get(index) else {
            targets.push(None);
            break;
        };
        targets.extend(consumer.writes_to.iter().map(|word| target(known(word))));
        if consumer.writes_descriptor {
            targets.push(None);
        }
        let texts: Vec<String> = consumer
            .words
            .iter()
            .map(|word| word.text.clone())
            .collect();
        match indirect::command_word(&texts) {
            Some((at, consumer_name)) if PIPE_WRITERS.contains(&consumer_name.as_str()) => {
                targets.extend(
                    consumer.words[at + 1..]
                        .iter()
                        .filter(|word| !word.text.starts_with('-'))
                        .map(|word| {
                            target(known(word).map(|text| file_operand(&text).to_string()))
                        }),
                );
            }
            Some((at, consumer_name)) if PURE_FILTERS.contains(&consumer_name.as_str()) => {
                if filter_saves(&consumer_name, &texts[at + 1..]) {
                    targets.push(None);
                }
            }
            // A stage that may write what it reads anywhere.
            _ => targets.push(None),
        }
        stage = consumer.pipe_to;
    }
    targets
}

/// Keep output targets only while their count and aggregate allocation stay
/// bounded. An unnamed target is conservative for every target this drops, so
/// it also avoids retaining an almost-full list after either budget is spent.
fn byte_bounded_targets(targets: Vec<Option<String>>) -> Vec<Option<String>> {
    let mut bounded = Vec::new();
    let mut bytes: usize = 0;
    for target in targets {
        work::step();
        let target_bytes = target
            .as_ref()
            .map_or(0, String::len)
            .checked_add(std::mem::size_of::<Option<String>>());
        let Some(target_bytes) = target_bytes else {
            return vec![None];
        };
        let Some(next) = bytes.checked_add(target_bytes) else {
            return vec![None];
        };
        if bounded.len() == MAX_SAVED_TARGETS || next > MAX_SAVED_TARGET_BYTES {
            return vec![None];
        }
        bytes = next;
        bounded.push(target);
    }
    bounded
}

/// Whether a pure filter's arguments may save its output to a file:
/// `sort -o f` (also in a group of short options, or `--output`, or a
/// `--compress-program` it runs, or any `--c` abbreviation but `--check`), or `uniq`'s second operand. Option values
/// may be read as operands here (fail closed).
fn filter_saves(filter: &str, args: &[String]) -> bool {
    match filter {
        "sort" => args.iter().take_while(|arg| *arg != "--").any(|arg| {
            arg.starts_with("--o")
                || (arg.starts_with("--c") && !arg.starts_with("--check"))
                || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains('o'))
        }),
        "uniq" => {
            let mut operands = 0;
            let mut options_ended = false;
            for arg in args {
                work::step();
                if !options_ended && arg == "--" {
                    options_ended = true;
                } else if options_ended || arg == "-" || !arg.starts_with('-') {
                    operands += 1;
                }
            }
            operands > 1
        }
        _ => false,
    }
}

/// The word from `from` on that ends with `inline` (an attached `-c$X`
/// ends its option word).
fn word_holding(texts: &[String], from: usize, inline: &str) -> Option<usize> {
    (from..texts.len()).find(|&index| {
        let word = texts[index].as_str();
        word.len() >= inline.len() && std::ptr::eq(&word[word.len() - inline.len()..], inline)
    })
}

/// The file in an operand such as `of=r.sh`.
fn file_operand(word: &str) -> &str {
    word.rsplit_once('=').map_or(word, |(_, file)| file)
}

/// Bytes of words joined into lines (`env -S'git config' k v`), for each
/// command.
const MAX_JOINED_BYTES: usize = 64 * 1024;

/// `line` with `words` after it, each after a space, as a shell would run
/// them: at most `MAX_JUDGED_POSITIONS` words, adding at most the bytes
/// `left` of one command's `MAX_JOINED_BYTES`. `cut`: some were left out,
/// so the line is not seen in full.
fn joined(mut line: String, words: &[String], left: &mut usize, cut: &mut bool) -> String {
    *cut |= words.len() > outbound::MAX_JUDGED_POSITIONS;
    for word in words.iter().take(outbound::MAX_JUDGED_POSITIONS) {
        work::step();
        let Some(rest) = left.checked_sub(word.len() + 1) else {
            *cut = true;
            break;
        };
        *left = rest;
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    line
}

/// The words that may be the script of the shell at `at`, given the
/// inline code `invocation::parse` found in its words. After `-c`, bash
/// reads further options (`-o pipefail`) and runs the first operand; other
/// shells run the word after `-c`. Every word that may be the script is
/// judged.
fn script_candidates(texts: &[String], at: usize, inline: &str) -> Option<Vec<usize>> {
    let first = word_holding(texts, at + 1, inline)?;
    Some(if texts[first].starts_with(['-', '+']) {
        (first..texts.len())
            .filter(|&index| !texts[index].starts_with(['-', '+']))
            .collect()
    } else {
        vec![first]
    })
}

/// #1268: the `-c` scripts of shells after a launcher that puts its input
/// into their words, with each shell's word, wherever the command word is
/// read to be. A script that cannot be found sets `cut`, as do more than
/// `MAX_JUDGED_POSITIONS` such shells.
fn replaced_shell_scripts(
    texts: &[String],
    launches: &[launched_git::Launch],
    cut: &mut bool,
) -> Vec<(usize, String)> {
    let mut scripts = Vec::new();
    let mut shells = 0;
    for (at, text) in texts.iter().enumerate() {
        work::step();
        let shell = name(text);
        if !launches[at].replaced || !SHELLS.contains(&shell.as_str()) {
            continue;
        }
        shells += 1;
        if shells > outbound::MAX_JUDGED_POSITIONS {
            *cut = true;
            break;
        }
        if shell == "fish" {
            scripts.extend(
                fish_codes(texts, at, cut)
                    .into_iter()
                    .map(|code| (at, code)),
            );
        }
        match invocation::parse(&shell, &texts[at + 1..]).map(|invocation| invocation.code) {
            Some(Code::Inline(Some(inline))) => match script_candidates(texts, at, inline) {
                Some(candidates) if !candidates.is_empty() => scripts.extend(
                    candidates
                        .into_iter()
                        .map(|candidate| (at, texts[candidate].clone())),
                ),
                _ => *cut = true,
            },
            Some(Code::Inline(None)) => *cut = true,
            _ => {}
        }
    }
    scripts
}

/// Code fish runs from its options after `at`, read as fish's getopt
/// reads them (`+hPilNnvc:C:p:d:f:D:o:`): every `-c`/`--command` and
/// `-C`/`--init-command` value, attached or the next word, in a group or
/// after other options' values (`-c true -C 'CODE'`), long ones by any
/// unambiguous prefix. An option it does not know sets `cut`.
fn fish_codes(texts: &[String], at: usize, cut: &mut bool) -> Vec<String> {
    /// Long options, whether each takes a value, and whether that is code.
    const LONG: &[(&str, bool, bool)] = &[
        ("command", true, true),
        ("init-command", true, true),
        ("features", true, false),
        ("debug", true, false),
        ("debug-output", true, false),
        ("debug-stack-frames", true, false),
        ("profile", true, false),
        ("profile-startup", true, false),
        ("interactive", false, false),
        ("login", false, false),
        ("no-execute", false, false),
        ("no-config", false, false),
        ("private", false, false),
        ("print-rusage-self", false, false),
        ("print-debug-categories", false, false),
        ("help", false, false),
        ("version", false, false),
    ];
    let mut codes = Vec::new();
    let mut index = at + 1;
    while let Some(word) = texts.get(index) {
        work::step();
        index += 1;
        if word == "--" || word == "-" || !word.starts_with('-') {
            break;
        }
        // An option's value: attached, or else the next word.
        let value = |attached: Option<&str>, index: &mut usize| match attached {
            Some(attached) => Some(attached.to_string()),
            None => {
                let next = texts.get(*index).cloned();
                *index += 1;
                next
            }
        };
        if let Some(long) = word.strip_prefix("--") {
            let (option, attached) = match long.split_once('=') {
                Some((option, attached)) => (option, Some(attached)),
                None => (long, None),
            };
            let exact = LONG.iter().find(|(name, _, _)| *name == option);
            let mut prefixed = LONG.iter().filter(|(name, _, _)| name.starts_with(option));
            let found = exact.or_else(|| match (prefixed.next(), prefixed.next()) {
                (Some(only), None) => Some(only),
                _ => None,
            });
            match found {
                Some((_, true, is_code)) => {
                    let taken = value(attached, &mut index);
                    if *is_code {
                        match taken {
                            Some(code) => codes.push(code),
                            None => *cut = true,
                        }
                    }
                }
                Some((_, false, _)) if attached.is_none() => {}
                _ => *cut = true,
            }
            continue;
        }
        for (at_letter, letter) in word.char_indices().skip(1) {
            work::step();
            let rest = &word[at_letter + letter.len_utf8()..];
            let attached = (!rest.is_empty()).then_some(rest);
            if "cC".contains(letter) {
                match value(attached, &mut index) {
                    Some(code) => codes.push(code),
                    None => *cut = true,
                }
                break;
            }
            if "pdfDo".contains(letter) {
                value(attached, &mut index);
                break;
            }
            if !"hPilNnv".contains(letter) {
                *cut = true;
                break;
            }
        }
    }
    codes
}

/// #1515: whether a cut command's later words may count: git under any
/// name, a shell, interpreter or `source` (its script), a program the
/// classifier judges by its later words (sed, the Corbanu CLI, credential
/// tools, schedulers), or a word it cannot see. Also as a finder's exec
/// option holds it (`--exec=git`, `-xgit`, #1276), or as one part of a word
/// `eval` splits (`eval 'git -C' { …`).
fn cut_matters(words: &[Word]) -> bool {
    let judged = |text: &str| {
        let name = basename(text);
        name == "git"
            || indirect::equals_git(name)
            || git_helpers::git_name(name).is_some()
            || invocation::parse(name, &[]).is_some()
            || sed::NAMES.contains(&name)
            || super::CLI_NAMES.contains(&name)
            || matches!(name, "crontab" | "launchctl" | "systemctl")
            || super::CREDENTIAL_COMMANDS
                .iter()
                .any(|(command, _)| *command == name)
    };
    words.iter().any(|word| {
        work::step();
        let lower = word.text.to_lowercase();
        let attached = lower.strip_prefix('-').and_then(|option| {
            option.split_once('=').map(|(_, value)| value).or_else(|| {
                option
                    .find('x')
                    .map(|at| option[at + 1..].trim_start_matches('='))
            })
        });
        word.dynamic
            || judged(&lower)
            || attached.is_some_and(judged)
            || lower.split_whitespace().any(|part| {
                work::step();
                judged(part)
            })
    })
}

/// #1636: a command's words as the shell may make them ([`shell_made`]).
struct ShellMade {
    words: Vec<Word>,
    /// The words expanded only in part ([`Truncated`]).
    truncated: Vec<Truncated>,
}

/// #1636: a word expanded only in part.
struct Truncated {
    /// It may make an option ([`may_make_option`]).
    option: bool,
    /// It may spell git ([`may_spell_git`]).
    git: bool,
    /// A git word comes before it.
    after_git: bool,
}

impl ShellMade {
    /// Whether a word expanded only in part may hide an option of a git
    /// command (`spelled`: the line spells git), or git's name itself.
    fn unsure(&self, spelled: bool) -> bool {
        self.truncated.iter().any(|word| {
            work::step();
            (word.option && (spelled || word.git)) || (!word.after_git && word.git)
        })
    }
}

/// #1636: whether some word `text` expands to may start with `-`: it starts
/// with `-`, `=`, an expansion or a substitution's output, or with a group
/// and holds any of those (an empty alternative may come before it:
/// `{,}--upload-pack=./x`), or a range whose ends are not letters or digits
/// (`{+../}`).
fn may_make_option(text: &str) -> bool {
    if text.starts_with(['-', '=', '$']) || text.starts_with(UNSEEN_OUTPUT) {
        return true;
    }
    if !text.starts_with('{') {
        return false;
    }
    // A range end that is no letter or digit (`{+../}`).
    let odd_end = |ch: &u8| !ch.is_ascii_alphanumeric() && !b"{},.".contains(ch);
    let bytes = text.as_bytes();
    bytes.iter().any(|byte| {
        work::step();
        matches!(byte, b'-' | b'$' | b'=' | 1 | 2)
    }) || bytes.windows(2).any(|pair| {
        work::step();
        matches!(pair, [b'{' | b'.', ch] if odd_end(ch))
    })
}

/// #1636: whether a word expanded only in part may spell git: `g`, `i` and
/// `t` in that order, a letter range (`{f..h}`) or an expansion.
fn may_spell_git(text: &str) -> bool {
    let mut wanted = "git".chars().peekable();
    for ch in text.chars() {
        work::step();
        if wanted.peek() == Some(&ch.to_ascii_lowercase()) {
            wanted.next();
        }
    }
    wanted.peek().is_none()
        || text.contains('$')
        || text.contains(UNSEEN_OUTPUT)
        || text.as_bytes().windows(5).any(|window| {
            work::step();
            matches!(window, [b'{', from, b'.', b'.', to]
                if from.is_ascii_alphabetic() && to.is_ascii_alphabetic())
        })
}

/// #1636: a command's words as the shell may make them, where that differs
/// from this reading: brace groups expanded ([`braces::expand`], as bash 3.2
/// does with `old_bash`; in a word with quoted text only when it has an
/// unquoted group; empty words of an unquoted one dropped, as the shell
/// drops them), and an unknown expansion (or a substitution's output) at
/// the start of a word that then reads as an option ([`option_readings`]).
/// `None`: the same words.
fn shell_made(words: &[Word], old_bash: bool) -> Option<ShellMade> {
    /// Words one word may expand to, as the word lexer allows.
    const MAX_WORDS: usize = 64;
    /// Words of one command, made.
    const MAX_MADE: usize = 1024;
    let mut made = ShellMade {
        words: Vec::with_capacity(words.len()),
        truncated: Vec::new(),
    };
    let mut changed = false;
    // A git word is among the words made so far.
    let mut after_git = false;
    for word in words {
        work::step();
        let expands = word.text.contains('{') && (!word.quoted || word.brace_expansion);
        let alternatives = if expands {
            let room = MAX_WORDS.min(MAX_MADE.saturating_sub(made.words.len()).max(1));
            let expansion = braces::expand(&word.text, room, old_bash);
            if expansion.truncated {
                made.truncated.push(Truncated {
                    option: may_make_option(&word.text),
                    git: may_spell_git(&word.text),
                    after_git,
                });
            }
            expansion.words
        } else {
            vec![word.text.clone()]
        };
        changed |= alternatives.len() != 1 || alternatives[0] != word.text;
        for alternative in alternatives {
            work::step();
            if expands && !word.quoted && alternative.is_empty() {
                continue;
            }
            // zsh-only forms (`$=N`) may read as text here.
            let readings = if word.dynamic || alternative.starts_with('$') {
                option_readings(&alternative)
            } else {
                Vec::new()
            };
            changed |= !readings.is_empty();
            let texts = if readings.is_empty() {
                vec![alternative]
            } else {
                readings
            };
            for text in texts {
                work::step();
                let program = name(&text);
                after_git |= program == "git"
                    || git_helpers::git_name(&program).is_some()
                    || indirect::equals_git(&text);
                made.words.push(Word {
                    text,
                    brace_expansion: false,
                    ..word.clone()
                });
            }
        }
    }
    changed.then_some(made)
}

/// #1636: the options `text` may make when the parameter expansions and
/// substitution outputs it starts with are empty, or (`${N:-…}`, `${N-…}`,
/// `${N:+…}`, `${N:=…}`) their words: `${N}--upload-pack=./x`,
/// `${N:---upload-pack=./x}`. zsh's `$=N`, `$^N`, `$~N`, subscripts and
/// `:x` modifiers too. Expansions that are never empty (`$$`, `$#`, `$?`,
/// `$-`) end the search.
fn option_readings(text: &str) -> Vec<String> {
    /// Readings tried; each is shorter than the one it comes from.
    const MAX_READINGS: usize = 16;
    let mut found = Vec::new();
    let mut pending = vec![text.to_string()];
    let mut tried = 0;
    while let Some(reading) = pending.pop() {
        work::step();
        tried += 1;
        if tried > MAX_READINGS {
            break;
        }
        match leading_expansion(&reading) {
            Some((length, value)) => {
                let rest = &reading[length..];
                if let Some(value) = value {
                    pending.push(format!("{value}{rest}"));
                }
                pending.push(rest.to_string());
            }
            None => {
                if reading.starts_with('-') && reading.len() < text.len() {
                    found.push(reading);
                }
            }
        }
    }
    found
}

/// #1636: the length of the parameter expansion or substitution output
/// `text` starts with, if any, and the word it may be instead of the value
/// (`${N:-word}`).
fn leading_expansion(text: &str) -> Option<(usize, Option<String>)> {
    if text.starts_with(UNSEEN_OUTPUT) {
        return Some((UNSEEN_OUTPUT.len(), None));
    }
    let rest = text.strip_prefix('$')?;
    if let Some(inner) = rest.strip_prefix('{') {
        let mut depth = 1usize;
        let end = inner.char_indices().find_map(|(at, ch)| {
            work::step();
            match ch {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            (depth == 0).then_some(at)
        })?;
        let body = &inner[..end];
        // zsh flags (`${(f)N}`), then the name and a subscript.
        let body = match body.strip_prefix('(') {
            Some(flags) => flags.split_once(')').map_or("", |(_, rest)| rest),
            None => body,
        };
        let body = body.trim_start_matches(['#', '!', '=', '^', '~']);
        // zsh: a nested expansion's value (`${${N}:-word}`, `${$N:-word}`).
        let body = after_nested(body);
        let name = body
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || "@*".contains(ch)))
            .unwrap_or(body.len());
        let after = skip_subscript(&body[name..]);
        let value = [":-", ":=", ":+", "-", "=", "+"]
            .iter()
            .find_map(|op| after.strip_prefix(op))
            .map(str::to_string);
        return Some((2 + end + 1, value));
    }
    let flags = rest.len() - rest.trim_start_matches(['=', '^', '~']).len();
    let after_flags = &rest[flags..];
    let name = if after_flags.starts_with(|ch: char| ch.is_ascii_digit() || "@*!".contains(ch)) {
        1
    } else {
        after_flags
            .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
            .unwrap_or(after_flags.len())
    };
    if name == 0 {
        return None;
    }
    let mut after = skip_subscript(&after_flags[name..]);
    // zsh's `:t`, `:h`, … modifiers.
    while let Some(modified) = after
        .strip_prefix(':')
        .filter(|modified| modified.starts_with(|ch: char| ch.is_ascii_alphabetic()))
    {
        work::step();
        after = &modified[1..];
    }
    Some((text.len() - after.len(), None))
}

/// #1636: `body` after the nested expansion it starts with, if any.
fn after_nested(body: &str) -> &str {
    if let Some(inner) = body.strip_prefix("${") {
        let mut depth = 1usize;
        return inner
            .char_indices()
            .find_map(|(at, ch)| {
                work::step();
                match ch {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
                (depth == 0).then(|| &inner[at + 1..])
            })
            .unwrap_or(body);
    }
    match body.strip_prefix('$') {
        Some(rest) => rest.trim_start_matches(|ch: char| ch.is_ascii_alphanumeric() || ch == '_'),
        None => body,
    }
}

/// `text` after a `[…]` subscript it starts with.
fn skip_subscript(text: &str) -> &str {
    text.strip_prefix('[')
        .and_then(|inner| inner.split_once(']'))
        .map_or(text, |(_, rest)| rest)
}

/// #1606: whether the folder option at `from` is git's own: the command
/// is git, with nothing before it, and only its global options (and their
/// values) come between them (`git -c k=v -C ~ log`). Not a git after a
/// wrapper or a subcommand (`=git submodule foreach env -iC ~ git log`),
/// and not one a wrapper's option value spells (`sudo --user git -D ~`).
fn own_git_folder(texts: &[String], from: usize) -> bool {
    const VALUED: &[&str] = &[
        "-C",
        "-c",
        "--git-dir",
        "--work-tree",
        "--namespace",
        "--exec-path",
        "--config-env",
        "--super-prefix",
    ];
    // As written: `=git` is not read so here (as on main), so a later git
    // is still judged (`=git -C ~/r submodule foreach git log …`).
    let Some(first) = texts.first() else {
        return false;
    };
    let program = name(first);
    if !(program == "git" || git_helpers::git_name(first) == Some(None))
        || !texts.get(from).is_some_and(|word| word.starts_with("-C"))
    {
        return false;
    }
    let mut value_next = false;
    texts.get(1..from).unwrap_or_default().iter().all(|word| {
        work::step();
        let option = std::mem::take(&mut value_next) || word.starts_with('-');
        value_next = VALUED.contains(&word.as_str());
        option
    })
}

/// #1620: whether a writer's operand is a word zsh reads as the installed
/// program it names (`tee -a =mytool`, `cp x ="mytool"`, `curl -o
/// =mytool`, `cp x ""=mytool`, `cp x {,}=mytool`: any `=` in a word with
/// a brace group may start one of its words), after zsh's precommand
/// modifiers (`noglob tee =mytool`). An option's quoted value (`curl
/// --proto '=https'`) is not expanded.
fn writes_zsh_program(words: &[Word], command_at: Option<&(usize, String)>) -> bool {
    let Some((mut index, mut program)) = command_at.cloned() else {
        return false;
    };
    while unknown_folder::MODIFIERS.contains(&program.as_str()) {
        work::step();
        index += 1;
        let Some(word) = words.get(index) else {
            return false;
        };
        program = name(&word.text);
    }
    WRITERS.contains(&program.as_str())
        && words
            .get(index + 1..)
            .unwrap_or_default()
            .iter()
            .any(|word| {
                work::step();
                (word.equals_first && outputs::zsh_program_path(&word.text))
                    || (word.brace_expansion && word.text.contains('='))
            })
}

/// #1620: whether the command line `eval` (its words joined), `trap` or
/// `alias` (each value) runs writes such a program: a redirection target or
/// a writer's operand (`eval "echo x >> =mytool"`, `alias x='tee =mytool'`).
/// Read with its quotes, so `eval "awk 'NR>=10' f"` writes nothing.
fn code_writes_zsh_program(texts: &[String], command_at: Option<&(usize, String)>) -> bool {
    let Some((index, name)) = command_at else {
        return false;
    };
    let args = &texts[index + 1..];
    let lines: Vec<&str> = match name.as_str() {
        "eval" => {
            let args = args.strip_prefix(&["--".to_string()]).unwrap_or(args);
            return writes_in(&args.join(" "));
        }
        "trap" => args.iter().map(String::as_str).collect(),
        "alias" => args
            .iter()
            .map(|arg| arg.split_once('=').map_or(arg.as_str(), |(_, value)| value))
            .collect(),
        _ => return false,
    };
    lines.into_iter().any(|line| {
        work::step();
        writes_in(line)
    })
}

/// #1685: the values an assignment to an entry of a table of code run by
/// name may set (`functions[f]=…`, `aliases[x]+=…`, `BASH_ALIASES[x]=…`):
/// the text after each `]=` or `]+=`, since a subscript may hold one
/// (`functions[f]+='… #]=x'` sets the text after the first). And whether
/// it is an array form of such a table (`aliases+=(…)`, `functions=(…)`)
/// or an empty value.
fn code_table_values(text: &str) -> (Vec<&str>, bool) {
    const TABLES: &[&str] = &[
        "functions",
        "aliases",
        "galiases",
        "saliases",
        "BASH_ALIASES",
    ];
    let name_end = text.find(['[', '+', '=']).unwrap_or(text.len());
    if !TABLES.contains(&&text[..name_end]) {
        return (Vec::new(), false);
    }
    let rest = &text[name_end..];
    // The reading ends the word before an array's `(` (`aliases=`).
    if matches!(rest, "=" | "+=") || rest.starts_with("=(") || rest.starts_with("+=(") {
        return (Vec::new(), true);
    }
    if !rest.starts_with('[') {
        return (Vec::new(), false);
    }
    let values = rest
        .match_indices(']')
        .filter_map(|(at, _)| {
            work::step();
            let after = &rest[at + 1..];
            after.strip_prefix('=').or_else(|| after.strip_prefix("+="))
        })
        .collect();
    (values, false)
}

/// #1620: see [`code_writes_zsh_program`].
fn writes_in(line: &str) -> bool {
    quoted::commands(line).commands.iter().any(|command| {
        work::step();
        let texts: Vec<String> = command.words.iter().map(|word| word.text.clone()).collect();
        command.writes_to.iter().any(|word| {
            work::step();
            outputs::zsh_program_path(&word.text)
        }) || writes_zsh_program(&command.words, indirect::command_word(&texts).as_ref())
    })
}

/// #1283: note `kind` once (a few kinds at most, so checking is cheap).
fn note_git(found: &mut Found, kind: ProtectedActionKind) {
    if !found.git.contains(&kind) {
        found.git.push(kind);
    }
}

/// #1283: whether text may spell `git` once the word lexer reads it: it
/// removes quotes, backslashes, line continuations and braces, and decodes
/// `$'...'`.
fn spells_git(text: &str) -> bool {
    let bare: String = text
        .replace("\\\n", "")
        .chars()
        .filter(|ch| !matches!(ch, '\'' | '"' | '\\' | '{' | '}'))
        .collect();
    bare.to_lowercase().contains("git") || text.contains("$'")
}

/// What a launcher puts into a shell's script (`xargs -I% sh -c 'git %'`):
/// its added words go to the shell's own arguments, not the script.
fn replaced(launch: launched_git::Launch) -> launched_git::Launch {
    launched_git::Launch {
        fed: false,
        replaced: launch.replaced,
    }
}

/// Scripts a wrapper at `at` runs as a shell line, with the word that
/// holds each: `watch CMD...`, `su -c CMD`, `runuser -c`, `flock F -c`,
/// `script -c`, `env -S STRING`, also in a group of short options
/// (`su -lc`, `script -qc`). Options are looked for among the next
/// `MAX_JUDGED_POSITIONS` words. `left`: bytes the command may still join
/// ([`joined`]); `cut`: a script was cut short.
fn runner_scripts(
    texts: &[String],
    at: usize,
    last: usize,
    whole: &mut bool,
    left: &mut usize,
    cut: &mut bool,
) -> Vec<(usize, String, bool)> {
    let wrapper = name(&texts[at]);
    let args = at + 1..texts.len().min(at + 1 + outbound::MAX_JUDGED_POSITIONS);
    match wrapper.as_str() {
        "watch" => {
            // Its options, then the command, joined into one shell line.
            let mut index = at + 1;
            while let Some(word) = texts.get(index)
                && word.starts_with('-')
            {
                work::step();
                let valued = matches!(word.as_str(), "-n" | "--interval" | "-q" | "--equexit");
                index += 1 + usize::from(valued);
            }
            if index >= texts.len() {
                return Vec::new();
            }
            vec![(index, texts[index..].join(" "), true)]
        }
        // #1449: an option's command, given what it runs on: a file it
        // names, or data on its input (a shell runs either as code).
        name if rerun::VALUE_RUNNERS.contains(&name) => {
            let limit = rerun::limit_for(at, last, at == last && std::mem::take(whole));
            runner_option_scripts(texts, at, texts.len(), limit, cut)
        }
        "su" | "runuser" | "flock" | "script" | "env" => {
            let (letter, long) = if wrapper == "env" {
                ('S', "--split-string")
            } else {
                ('c', "--command")
            };
            let mut scripts = Vec::new();
            for index in args {
                work::step();
                let word = &texts[index];
                let next = || texts.get(index + 1).map(|next| (index + 1, next.clone()));
                if word == long {
                    scripts.extend(next());
                } else if let Some(attached) = word.strip_prefix(&format!("{long}=")) {
                    scripts.push((index, attached.to_string()));
                } else if let Some(cluster) = word.strip_prefix('-')
                    && !cluster.starts_with('-')
                    && let Some((at_letter, _)) =
                        cluster.char_indices().find(|(_, ch)| *ch == letter)
                {
                    // The letter's value is the rest of the group, or the
                    // next word.
                    let rest = &cluster[at_letter + 1..];
                    if rest.is_empty() {
                        scripts.extend(next());
                    } else {
                        scripts.push((index, rest.to_string()));
                    }
                }
            }
            // #1268: env runs the split string with its later words after
            // it (`env -S'git config' core.pager sh`), judged besides the
            // string alone. Past the words read here, the line is not seen
            // in full: fail closed.
            if wrapper == "env" {
                let mut with_later = Vec::new();
                for (index, script) in &scripts {
                    work::step();
                    let later = &texts[*index + 1..];
                    if !later.is_empty() {
                        with_later.push((*index, joined(script.clone(), later, left, cut)));
                    }
                }
                scripts.extend(with_later);
            }
            let mut scripts: Vec<(usize, String, bool)> = scripts
                .into_iter()
                .map(|(index, script)| (index, script, true))
                .collect();
            // #1449: and the other spellings `rerun::OPERAND_RUNNERS` reads
            // (`su -s SHELL`, `--session-command`, `--comm=`), up to the
            // command it runs when it wraps one.
            if wrapper != "env" {
                let end = rerun::options_end(texts, at, last);
                let limit = rerun::limit_for(at, last, at == last && std::mem::take(whole));
                for found in runner_option_scripts(texts, at, end, limit, cut) {
                    work::step();
                    if !scripts
                        .iter()
                        .any(|(index, script, _)| *index == found.0 && *script == found.1)
                    {
                        scripts.push(found);
                    }
                }
            }
            scripts
        }
        // #1309 follow-up: the shell commands tmux starts (`tmux new -c ~
        // 'echo x >> .zshenv'`, `tmux -c 'cmd'`), each one shell line; the
        // words stay read as code too.
        "tmux" => unknown_folder::tmux_commands(texts, at)
            .into_iter()
            .map(|(index, script)| (index, script, /*script role*/ false))
            .collect(),
        _ => Vec::new(),
    }
}

/// #1449: the option values `rerun::runner_values` finds for the runner
/// at `at` (options before `end`, `limit` words read), as shell lines: a
/// program is given a file and a filter data, neither of which the
/// classifier sees. Each with its word, which stays read as code too (its
/// commands count as main counts them; `tar "-x;…;Igzip"`). More words
/// than are read: `cut`.
fn runner_option_scripts(
    texts: &[String],
    at: usize,
    end: usize,
    limit: usize,
    cut: &mut bool,
) -> Vec<(usize, String, bool)> {
    let (values, unread) = rerun::runner_values(texts, at, end, limit);
    *cut |= unread;
    values
        .into_iter()
        .map(|found| {
            let script = match found.runs {
                rerun::Runs::Line => found.value,
                rerun::Runs::Program => format!("{} {UNSEEN_OUTPUT}", found.value),
                rerun::Runs::Filter => format!("{} < {UNSEEN_OUTPUT}", found.value),
            };
            (found.holder, script, /*script role*/ false)
        })
        .collect()
}

#[cfg(test)]
mod pipeline_tests {
    use super::*;

    /// #1065: each command follows a bounded number of pipeline stages, so
    /// a long pipeline (`tee f|tee f|...`) is linear. Counted, not timed.
    #[test]
    fn issue_1065_pipeline_targets_are_bounded() {
        let reading = quoted::commands(&"tee f|".repeat(1000));
        assert_eq!(reading.commands.len(), 1000);
        let longest = reading
            .commands
            .iter()
            .map(|command| pipeline_targets(&reading.commands, command).len())
            .max();
        assert_eq!(longest, Some(MAX_PIPELINE_STAGES + 1));
    }

    #[test]
    fn issue_1132_saved_target_limits_keep_one_unknown_target() {
        assert_eq!(
            byte_bounded_targets(vec![Some("a".to_string()); MAX_SAVED_TARGETS + 1]),
            vec![None]
        );
        assert_eq!(
            byte_bounded_targets(vec![Some("a".repeat(MAX_SAVED_TARGET_BYTES))]),
            vec![None]
        );
    }

    /// #1132: allocation accounting includes both String bytes and the
    /// `Option<String>` vector slots. Keep mixed long and short names below
    /// and at the byte limit, but collapse an over-limit list to unknown.
    #[test]
    fn issue_1132_mixed_target_byte_boundaries_are_fail_closed() {
        let slots = 3 * std::mem::size_of::<Option<String>>();
        assert!(MAX_SAVED_TARGET_BYTES > slots + 2);
        let targets = |long_bytes| {
            vec![
                Some("l".repeat(long_bytes)),
                Some("s".to_string()),
                Some(String::new()),
            ]
        };

        let below = targets(MAX_SAVED_TARGET_BYTES - slots - 2);
        assert_eq!(byte_bounded_targets(below.clone()), below);

        let at_limit = targets(MAX_SAVED_TARGET_BYTES - slots - 1);
        assert_eq!(byte_bounded_targets(at_limit.clone()), at_limit);

        let above = targets(MAX_SAVED_TARGET_BYTES - slots);
        assert_eq!(byte_bounded_targets(above), vec![None]);
    }
}
