//! PF-30-S03 post-taint authority checks.
//!
//! Once a session has taken in content without standing (tool, MCP, agent,
//! memory or unattributed text, PF-30-S02), a model-selected action that
//! reaches credentials, the vault or security policy needs fresh, exact human
//! approval. Neither a cached session approval, a permission hook, the
//! automatic reviewer nor `approval_policy = never` can stand in for it, and
//! the approval is bound to the taint generation and the effective security
//! policy it was given under.
//!
//! This is a deterministic net over the exact command or patch the host will
//! run, not a prompt classifier, and it never relaxes anything: outside
//! `source_envelopes` with Moderate/Aggressive it does nothing. Besides the
//! words of the command it follows variables, `cd`/`-C` folders, symlinks,
//! inline interpreter code, encoded payloads, script files and patched
//! scripts; code it cannot see through (a decode-and-run pipeline) counts as
//! protected.

use crate::tools::sandboxing::ApprovalAction;
use codex_security_policy::ActorChain;
use codex_security_policy::SecurityLevel;
use codex_utils_path_uri::PathUri;
use invocation::Code;
use paths::Homes;
use shell::SimpleCommand;
use shell::basename;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;

mod aliases;
mod arithmetic;
mod certain_cd;
mod code_variables;
mod git_config;
mod git_helpers;
mod grammar;
mod indirect;
mod invocation;
mod launched_git;
mod launched_input;
mod loader_variables;
mod mktemp;
mod named_shell;
mod nested;
mod outbound;
mod outputs;
mod paths;
mod quoted;
mod rerun;
mod sed;
mod shadow;
mod shell;
mod unknown_folder;
/// #1296: also expands the Windows home spellings for protected-read
/// explanations (`protected_reads`).
pub(crate) mod windows_paths;
mod windows_reads;
mod windows_shell;
mod work;

pub(crate) use paths::USER_PERSISTENCE;
pub(crate) use paths::WORKSPACE_AGENT_SETTINGS;
pub(crate) use paths::WORKSPACE_PERSISTENCE;

/// Protected surfaces a tainted session may not reach on model authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProtectedActionKind {
    /// The Corbanu vault (`corbanu vault ...`).
    Vault,
    /// Credential stores: login tokens, SSH/cloud keys, OS keychain.
    Credentials,
    /// Security policy: Corbanu config, rules, hooks, requirements, login state.
    SecurityPolicy,
    /// Code the host cannot read before it runs (a download piped into a
    /// shell, a script it cannot read in full, a variable it cannot see), or
    /// an action it cannot describe.
    UnseenCode,
    /// Local content (a file, another command's output, session data passed
    /// to an outside tool) sent to another machine or service (PF-23-S01).
    Disclosure,
    /// Moving money or tokens (PF-23-S01).
    ValueTransfer,
    /// A tool route the security level has not classified (PF-23-S01).
    UnclassifiedTool,
    /// Writing a file that runs code or sets policy later, outside the
    /// sandbox: shell start-up files, login items, git hooks (PF-23-S02).
    Persistence,
    /// Typing into a process whose sandbox was set before the protected-path
    /// rules applied, so it can still read credentials (PF-23-S02).
    UnconfinedProcess,
}

impl ProtectedActionKind {
    /// The more protected of the two (the one a prompt names).
    pub(crate) fn strongest(self, other: Self) -> Self {
        if rank(other) > rank(self) {
            other
        } else {
            self
        }
    }

    pub(crate) fn describe(self) -> &'static str {
        match self {
            Self::Vault => "vault access",
            Self::Credentials => "credential access",
            // #841: also a read of Corbanu's own files, not only a change.
            Self::SecurityPolicy => {
                "access to Corbanu's own files or security settings (its config, rules, hooks, skills, history or sign-in state)"
            }
            Self::UnseenCode => "running code the host cannot read first",
            Self::Disclosure => "sending local data to another machine or service",
            Self::ValueTransfer => "a value transfer",
            Self::UnclassifiedTool => "a tool the security level has not classified",
            Self::Persistence => {
                "changing a file that runs code later (shell start-up, login items, git hooks)"
            }
            Self::UnconfinedProcess => {
                "typing into a process started before the session's protected-path rules applied"
            }
        }
    }
}

/// The effective security policy a post-taint approval is bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PolicyBinding {
    /// No live policy runtime is bound to this session.
    Unbound,
    /// The policy runtime could not be read.
    Unavailable,
    Bound {
        epoch: u64,
        revocation_generation: u64,
        kill_switch_active: bool,
        level: SecurityLevel,
        /// The agent lineage the session acts for.
        actor_chain: ActorChain,
    },
}

/// Session state a post-taint decision depends on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PostTaintState {
    /// Count of recorded batches without standing.
    pub(crate) taint_generation: u64,
    pub(crate) policy: PolicyBinding,
    /// The effective level: the stricter of the configured level and the
    /// live policy's (Aggressive when the policy cannot be read).
    pub(crate) level: SecurityLevel,
}

impl PostTaintState {
    /// Whether the sandbox's protected-path rules apply: after untrusted
    /// content, and under a protected level (Moderate or Aggressive) from the
    /// start. Applying them before taint under Moderate closes the files-route
    /// read gap (issue #239): a workspace-write sandbox otherwise lets an
    /// agent read credential files from `$HOME` before any untrusted content
    /// arrives. Sessions without a protected level carry no state today; the
    /// taint arm stays as defense in depth.
    pub(crate) fn protected_paths_apply(&self) -> bool {
        self.taint_generation > 0 || self.level != SecurityLevel::Permissive
    }

    /// Under Moderate (configured and live), a human approval given at taint
    /// generation `approved_at` lifts the protected-path rules for its command
    /// while no new untrusted content has arrived since (issue #239).
    pub(crate) fn human_approval_lifts_rules(&self, approved_at: Option<u64>) -> bool {
        self.moderate_bound() && approved_at == Some(self.taint_generation)
    }

    /// Moderate, both configured and in the live policy: a human approval
    /// may lift the protected-path rules for one command.
    pub(crate) fn moderate_bound(&self) -> bool {
        self.level == SecurityLevel::Moderate
            && matches!(
                self.policy,
                PolicyBinding::Bound {
                    level: SecurityLevel::Moderate,
                    ..
                }
            )
    }
}

/// Host-held state for one protected action, captured before approval and
/// checked again right before it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PostTaintAction {
    pub(crate) kind: ProtectedActionKind,
    pub(crate) state: PostTaintState,
}

/// Appended to the prompt and the approvals-off refusal when
/// [`PostTaintAction::reads_stay_denied`] holds.
const AGGRESSIVE_READS_STAY_DENIED: &str = " Under security level Aggressive, credential files, \
     the vault and the Corbanu home stay unreadable to agent commands even if you approve, and \
     that is never granted, not even once; to allow it, switch to Permissive in /security (it \
     applies after a restart).";

impl PostTaintAction {
    /// Shown in the approval prompt so the human sees why it was asked again.
    pub(crate) fn approval_reason(&self) -> String {
        let mut reason = format!(
            "Security level: this is {} after untrusted content entered the session. \
             Approve only if you asked for it; a cached or automatic approval does not count.",
            self.kind.describe()
        );
        if self.reads_stay_denied() {
            reason.push_str(AGGRESSIVE_READS_STAY_DENIED);
        }
        reason
    }

    /// #442: under Aggressive, what this action protects is denied by the
    /// sandbox whatever the person answers, and no grant lifts it, so none
    /// is offered for it.
    pub(crate) fn reads_stay_denied(&self) -> bool {
        self.state.level == SecurityLevel::Aggressive
            && !matches!(self.state.policy, PolicyBinding::Unavailable)
            && matches!(
                self.kind,
                ProtectedActionKind::Credentials | ProtectedActionKind::Vault
            )
    }

    pub(crate) fn approvals_off_rejection(&self) -> String {
        let mut reason = format!(
            "Not run: Security level {}: this is {} after untrusted content entered the session, \
             so it needs your approval, and approvals are off (approval_policy = never). \
             Review the action and turn approval prompts on (/permissions, or approval_policy \
             in config.toml) before asking again. Other security protections still apply.",
            level_name(self.state.level),
            self.kind.describe()
        );
        // #941 review: approving would not make these reads work under Aggressive.
        if self.reads_stay_denied() {
            reason.push_str(AGGRESSIVE_READS_STAY_DENIED);
        }
        reason
    }

    /// Refusal before any prompt: the kill switch denies protected actions.
    pub(crate) fn refused_up_front(&self) -> Option<String> {
        matches!(
            self.state.policy,
            PolicyBinding::Bound {
                kill_switch_active: true,
                ..
            }
        )
        .then(|| {
            format!(
                "Not run: this is {} after untrusted content entered the session, and the \
                 security kill switch is on.",
                self.kind.describe()
            )
        })
    }

    /// Re-check after the human answered: the decision covers only the taint
    /// and policy that were in front of them. `now` is `None` when the checks
    /// no longer apply (the level or flag changed), which also refuses.
    pub(crate) fn recheck(&self, now: Option<&PostTaintState>) -> Result<(), String> {
        let describe = self.kind.describe();
        match now {
            Some(now) if now.taint_generation != self.state.taint_generation => Err(format!(
                "Not run: new untrusted content arrived while {describe} was waiting for \
                 approval. Ask again if it is still needed."
            )),
            Some(now) if *now == self.state => Ok(()),
            Some(_) | None => Err(format!(
                "Not run: the security policy changed while {describe} was waiting for \
                 approval. Ask again if it is still needed."
            )),
        }
    }
}

const CLI_NAMES: &[&str] = &["corbanu", "codex", "pfterminal", "corbanu-debug"];
/// Subcommands of the Corbanu CLI that change policy or login state.
const CLI_POLICY_WORDS: &[&str] = &["config", "features", "login", "logout"];
/// Corbanu CLI flags that widen a child run's policy.
const CLI_POLICY_FLAGS: &[&str] = &["--full-auto", "--yolo", "danger-full-access", "--add-dir"];
/// `-c key=value` overrides of these keys change security policy.
const POLICY_CONFIG_KEYS: &[&str] = &[
    "approval_policy",
    "approvals_reviewer",
    "sandbox_mode",
    "sandbox_workspace_write",
    "sandbox_permissions",
    "permissions",
    "security",
    "features",
    "shell_environment_policy",
    "projects",
];
/// macOS `security` verbs that read or change keychain items.
pub(crate) const KEYCHAIN_VERBS: &[&str] = &[
    "find-generic-password",
    "find-internet-password",
    "find-certificate",
    "add-generic-password",
    "add-internet-password",
    "delete-generic-password",
    "delete-internet-password",
    "set-generic-password-partition-list",
    "dump-keychain",
    "export",
    "unlock-keychain",
];
/// Credential commands: the command name plus words that make it a secret read.
const CREDENTIAL_COMMANDS: &[(&str, &[&str])] = &[
    ("security", KEYCHAIN_VERBS),
    ("gh", &["token"]),
    ("gcloud", &["print-access-token", "print-identity-token"]),
    ("aws", &["export-credentials", "get-session-token"]),
    ("op", &["read", "item"]),
    ("secret-tool", &["lookup", "search"]),
    ("gpg", &["--export-secret-keys", "--export-secret-subkeys"]),
    ("pass", &["show"]),
];
/// Commands that read a folder tree (with the flag that makes them do so,
/// if any): pointed at a folder holding a home, they take its credentials.
const RECURSIVE_READERS: &[(&str, &[&str])] = &[
    ("tar", &[]),
    ("zip", &[]),
    ("7z", &[]),
    ("7za", &[]),
    ("rsync", &[]),
    ("scp", &[]),
    ("ditto", &[]),
    ("cpio", &[]),
    ("pax", &[]),
    ("cp", &["-r", "-R", "-a", "--recursive", "--archive"]),
    ("grep", &["-r", "-R", "--recursive"]),
    ("rg", &["--hidden", "-u", "-.", "--no-ignore"]),
    ("find", &["-exec", "-execdir", "-ok"]),
];
/// Commands that write the paths they name (PF-23-S02): naming a persistence
/// file to one of them, or redirecting output to it, is a write.
pub(super) const WRITERS: &[&str] = &[
    "tee", "cp", "mv", "install", "ln", "rsync", "dd", "truncate", "touch", "ditto", "sed", "perl",
    "ed", "ex", "patch", "chmod", "unzip", "tar", "curl", "wget", "rm", "rmdir", "unlink", "mkdir",
    // #1159: `script FILE` writes its typescript to FILE.
    "script",
];
/// `git config` keys that make git run a program (PF-23-S02).
const GIT_RUN_KEYS: &[&str] = &[
    "core.hookspath",
    "core.fsmonitor",
    "core.sshcommand",
    "core.pager",
    "core.editor",
    "core.askpass",
    "credential.helper",
];
/// Flags whose value is the folder later relative words resolve against.
const FOLDER_FLAGS: &[&str] = &["-C", "--directory", "--cwd", "--cd", "--chdir"];
/// Nesting limit for scripts, payloads and literals classified inside an
/// action. Deeper nesting fails closed.
const MAX_DEPTH: usize = 4;
/// Shell builtins that change what the rest of a line does (its folder,
/// variables or code): named by a path they run a file instead.
const PATH_BUILTINS: &[&str] = &[
    "cd", "pushd", "popd", "chdir", "source", ".", "eval", "exec", "export", "local", "declare",
    "readonly", "typeset", "trap", "alias", "builtin", "command", "let", "read", "set", "shopt",
    "unset",
];
/// Bytes of command text one classification lexes; more fail closed.
const MAX_COMMAND_BYTES: usize = 1024 * 1024;
/// Script files one classification reads; more fail closed.
const MAX_SCRIPT_FILES: usize = 16;
/// Bytes of variable values one classification substitutes (#1065): a
/// value doubled on each step (`a=$a$a`) or used many times would otherwise
/// grow without bound. Past it the line asks, and values up to
/// `MAX_SMALL_VALUE` bytes are still substituted, up to the same number of
/// bytes again, so the line still names what it reaches; a larger value is
/// judged once on its own.
const MAX_SUBSTITUTED_BYTES: usize = 1024 * 1024;
const MAX_SMALL_VALUE: usize = 64;
/// #1436: loops one line reads again (`LoopPass`); past them a loop with a
/// folder change starts in a folder not known.
const MAX_REPLAYED_LOOPS: usize = 8;
/// #1436: folders a replayed loop's passes may start from (`LoopPass`).
const MAX_BRANCH_FOLDERS: usize = 8;
/// Bytes of command text, and command lines, one classification lexes in
/// all, nested scripts, decoded payloads and script files included (#1065);
/// more fail closed.
const MAX_CLASSIFIED_BYTES: usize = 4 * 1024 * 1024;
const MAX_CLASSIFIED_LINES: usize = 4096;
/// Folders `pushd` may stack, and bytes of a folder name the classifier
/// follows (#1065); past them the line asks and the folder stays. The
/// stack is shared, not copied, by the readings of quoted text.
const MAX_FOLDER_STACK: usize = 64;
const MAX_FOLDER_BYTES: usize = 1024;
/// `-C`-style folders one command chains (#1141); more fail closed.
const MAX_CHAINED_FOLDERS: usize = 8;
/// Magic numbers of executables, which are not read as scripts.
const BINARY_MAGIC: &[&[u8]] = &[
    b"\x7fELF",
    b"\xfe\xed\xfa\xce",
    b"\xfe\xed\xfa\xcf",
    b"\xce\xfa\xed\xfe",
    b"\xcf\xfa\xed\xfe",
    b"\xca\xfe\xba\xbe",
    b"MZ",
];

/// Classify the exact action the host is about to run. `codex_home` is the
/// session's Corbanu home; the user's home resolves `~` and `$HOME`.
pub(crate) fn classify(action: &ApprovalAction, codex_home: &Path) -> Option<ProtectedActionKind> {
    classify_with(action, codex_home, dirs::home_dir().as_deref())
}

/// [`classify`] for an action the host runs exactly as its argv says: a
/// shell command line runs under the shell it names (not, for example,
/// under the zsh exec bridge, which runs any `-c` script under zsh). D85:
/// then a line that provably never reaches zsh skips the zsh-only checks.
pub(crate) fn classify_as_named(
    action: &ApprovalAction,
    codex_home: &Path,
) -> Option<ProtectedActionKind> {
    classify_as_named_with(action, codex_home, dirs::home_dir().as_deref())
}

pub(super) fn classify_with(
    action: &ApprovalAction,
    codex_home: &Path,
    user_home: Option<&Path>,
) -> Option<ProtectedActionKind> {
    classified(action, codex_home, user_home, /*as_named*/ false).found
}

pub(super) fn classify_as_named_with(
    action: &ApprovalAction,
    codex_home: &Path,
    user_home: Option<&Path>,
) -> Option<ProtectedActionKind> {
    classified(action, codex_home, user_home, /*as_named*/ true).found
}

/// The classifier after judging the action, with its counters. `as_named`:
/// the host runs the action as its argv names (see [`classify_as_named`]).
/// #1240: on Windows the action is also judged under Windows path rules
/// (`windows_paths`, `windows_shell`), and the stronger finding is kept, so
/// the Windows view only adds findings. #1261: on Windows the first
/// judgment uses Windows path rules too and opens files only through
/// `windows_reads`.
fn classified(
    action: &ApprovalAction,
    codex_home: &Path,
    user_home: Option<&Path>,
    as_named: bool,
) -> Classifier {
    let mut classifier = judged(action, Homes::new(codex_home, user_home), as_named);
    if cfg!(windows) {
        let windows = judged(action, Homes::windows(codex_home, user_home), as_named);
        if let Some(kind) = windows.found {
            classifier.note(kind);
        }
    }
    classifier
}

/// #1240: [`classify_with`] of a command run in `cwd` as on Windows, on any
/// platform (an action cannot hold a Windows folder elsewhere). Filesystem
/// lookups under Windows rules happen only on Windows (#1261: the first
/// judgment opens files there through `windows_reads`).
#[cfg(test)]
pub(super) fn classify_as_windows(
    command: &[String],
    cwd: &str,
    codex_home: &Path,
    user_home: Option<&Path>,
) -> Option<ProtectedActionKind> {
    let mut classifier = Classifier::new(
        Homes::windows_first(codex_home, user_home),
        /*avoids_zsh*/ false,
    );
    classifier.judge_command(command, cwd);
    let mut windows = Classifier::new(
        Homes::windows(codex_home, user_home),
        /*avoids_zsh*/ false,
    );
    windows.judge_command(command, cwd);
    if let Some(kind) = windows.found {
        classifier.note(kind);
    }
    classifier.found
}

/// One judgment of the action, under the path rules of `homes`.
fn judged(action: &ApprovalAction, homes: Homes, as_named: bool) -> Classifier {
    let avoids_zsh = as_named
        && match action {
            ApprovalAction::Shell { command, .. } | ApprovalAction::ExecCommand { command, .. } => {
                named_shell::avoids_zsh(command) || named_shell::grammar_proves_data(command)
            }
            ApprovalAction::ApplyPatch { .. } => false,
        };
    judged_avoiding(action, homes, avoids_zsh)
}

/// [`judged`] with the zsh-only checks skipped at the top level when
/// `avoids_zsh` (#932's measuring harness also judges with it forced).
fn judged_avoiding(action: &ApprovalAction, homes: Homes, avoids_zsh: bool) -> Classifier {
    let mut classifier = Classifier::new(homes, avoids_zsh);
    match action {
        ApprovalAction::Shell { command, cwd, .. }
        | ApprovalAction::ExecCommand { command, cwd, .. } => {
            // #1261: see `reads_follow`.
            classifier.reads_follow = classifier.homes.windows_paths()
                && !classifier.homes.is_windows_view()
                && named_shell::windows_reads_follow(command);
            classifier.judge_command(command, &path_text(cwd));
        }
        ApprovalAction::ApplyPatch {
            files, patch, cwd, ..
        } => {
            let cwd = path_text(cwd);
            classifier.homes.allow_lookups_under(&cwd);
            for file in files {
                work::step();
                let file = path_text(file);
                classifier.path(&file);
                if classifier.homes.is_persistence(&file) {
                    classifier.note(ProtectedActionKind::Persistence);
                }
            }
            // Commands written into a file that is run later are judged now,
            // whether the file looks runnable by name or by where it leads.
            for (file, body) in indirect::patch_additions(patch) {
                work::step();
                let resolved = classifier.homes.resolve(&file, &cwd);
                let canonical = classifier.homes.canonical(&resolved);
                let runnable = [Some(resolved.to_lowercase()), canonical]
                    .into_iter()
                    .flatten()
                    .any(|path| indirect::is_runnable_file(&path, &body));
                if runnable {
                    classifier.script(&body, &cwd, /*depth*/ 1);
                }
            }
        }
    }
    // #1064: an interactive POSIX shell sources the file `ENV` names.
    if classifier.sets_env && classifier.interactive_shell {
        classifier.note(ProtectedActionKind::UnseenCode);
    }
    // A lookup budget ran out: something was not followed.
    if classifier.homes.exhausted() {
        classifier.note(ProtectedActionKind::UnseenCode);
    }
    // D191: an action that asks anyway asks as on main.
    if classifier.found.is_some()
        && let Some(kind) = classifier.held.take()
    {
        classifier.note(kind);
    }
    classifier
}

impl Classifier {
    fn new(homes: Homes, avoids_zsh: bool) -> Self {
        Self {
            avoids_zsh,
            homes,
            found: None,
            scripts_read: 0,
            rebuilt: 0,
            not_shell: 0,
            sed_scope: None,
            ran: Vec::new(),
            ran_unknown: false,
            argv0: String::new(),
            written: Vec::new(),
            nested_scripts: 0,
            unknown_folder: 0,
            unseen_folder: false,
            sudo_redefined: false,
            quoted_text: false,
            launched: launched_git::Launch::default(),
            substitution_budget: Budget {
                left: MAX_SUBSTITUTED_BYTES,
                small_left: MAX_SUBSTITUTED_BYTES,
            },
            classified_bytes: 0,
            classified_lines: 0,
            compared: shadow::Budget::new(),
            windows_decoded: HashSet::new(),
            windows_budget: windows_shell::RESOLVE_BUDGET,
            backslash: false,
            share: false,
            in_file: 0,
            reads_follow: false,
            temp: None,
            held: None,
            workspace: String::new(),
            cut_values: HashSet::new(),
            sets_env: false,
            interactive_shell: false,
            rebuilt_foreign: true,
            visited: Vec::new(),
            visited_all: true,
        }
    }

    /// Judge a command line run in `cwd`.
    fn judge_command(&mut self, command: &[String], cwd: &str) {
        self.argv0 = command.first().cloned().unwrap_or_default();
        if self.workspace.is_empty() {
            self.workspace = cwd.to_string();
        }
        self.homes.allow_lookups_under(cwd);
        self.path(cwd);
        self.command(command, cwd, /*depth*/ 0);
    }
}

fn path_text(path: &PathUri) -> String {
    path.to_abs_path()
        .map(|path| path.as_path().to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

fn rank(kind: ProtectedActionKind) -> u8 {
    match kind {
        ProtectedActionKind::UnclassifiedTool => 0,
        ProtectedActionKind::UnseenCode => 1,
        ProtectedActionKind::UnconfinedProcess => 2,
        ProtectedActionKind::Disclosure => 3,
        ProtectedActionKind::Persistence => 4,
        ProtectedActionKind::SecurityPolicy => 5,
        ProtectedActionKind::Credentials => 6,
        ProtectedActionKind::ValueTransfer => 7,
        ProtectedActionKind::Vault => 8,
    }
}

/// Commands that install something to run later outside the sandbox:
/// cron, launchd and systemd user units (PF-23-S02; git config is judged by
/// [`GitConfigWrites`]), given the words after `name` (`later` from `from`).
fn persistence_command(name: &str, later: &mut LaterWords, from: usize) -> bool {
    match name {
        "crontab" => !later.holds(from, &["-l"]),
        "launchctl" => later.holds(from, &["load", "bootstrap", "submit", "enable"]),
        "systemctl" => {
            later.holds(from, &["--user"]) && later.holds(from, &["enable", "link", "edit"])
        }
        _ => false,
    }
}

/// #1275: what the words after each word of a simple command hold, for the
/// checks made on each command word (credential, persistence and Corbanu
/// CLI commands). Each is worked out once, on its first use, so judging
/// every copy of a command word (`gh gh gh …`) stays linear.
struct LaterWords<'a> {
    /// The command's words, lowercase.
    words: &'a [String],
    /// For a set of words, whether each word or one after it is in it.
    holding: Vec<(&'static [&'static str], Vec<bool>)>,
    /// Whether each word or one after it widens a Corbanu CLI run's policy.
    policy_flags: Option<Vec<bool>>,
    /// The CLI subcommand of the words from each word on.
    subcommands: Option<Vec<Option<usize>>>,
}

impl<'a> LaterWords<'a> {
    fn new(words: &'a [String]) -> Self {
        Self {
            words,
            holding: Vec::new(),
            policy_flags: None,
            subcommands: None,
        }
    }

    /// Whether `words[from..]` holds one of `set`.
    fn holds(&mut self, from: usize, set: &'static [&'static str]) -> bool {
        let words = self.words;
        // Sets are constants: there are a few of them.
        let at = match self.holding.iter().position(|(known, _)| {
            work::step();
            *known == set
        }) {
            Some(at) => at,
            None => {
                let held = suffix_any(words.len(), |at| set.contains(&words[at].as_str()));
                self.holding.push((set, held));
                self.holding.len() - 1
            }
        };
        self.holding[at].1.get(from).copied().unwrap_or(false)
    }

    /// Whether a Corbanu CLI run given `words[from..]` has its policy
    /// widened by a flag or a `-c` override.
    fn policy_flag(&mut self, from: usize) -> bool {
        let words = self.words;
        let flags = self.policy_flags.get_or_insert_with(|| {
            let policy_key =
                |value: &str| POLICY_CONFIG_KEYS.iter().any(|key| value.starts_with(key));
            suffix_any(words.len(), |at| {
                let word = &words[at];
                CLI_POLICY_FLAGS.iter().any(|flag| word.contains(flag))
                    || word.starts_with("--dangerously")
                    || word.strip_prefix("--config=").is_some_and(policy_key)
                    || (word.len() > 2 && word.strip_prefix("-c").is_some_and(policy_key))
                    || (matches!(word.as_str(), "-c" | "--config")
                        && words.get(at + 1).is_some_and(|value| policy_key(value)))
            })
        });
        flags.get(from).copied().unwrap_or(false)
    }

    /// The CLI subcommand of `words[from..]`: the first word that is neither
    /// a flag nor a flag's value.
    fn subcommand(&mut self, from: usize) -> Option<&'a str> {
        let words = self.words;
        let subcommands = self.subcommands.get_or_insert_with(|| {
            let mut subcommands = vec![None; words.len() + 2];
            for (at, word) in words.iter().enumerate().rev() {
                work::step();
                subcommands[at] = if !word.starts_with('-') {
                    Some(at)
                } else if !word.contains('=') && CLI_VALUE_FLAGS.contains(&word.as_str()) {
                    subcommands[at + 2]
                } else {
                    subcommands[at + 1]
                };
            }
            subcommands
        });
        subcommands
            .get(from)
            .copied()
            .flatten()
            .map(|at| words[at].as_str())
    }
}

/// For each of `len` words and the end, whether `check` holds for it or a
/// word after it.
fn suffix_any(len: usize, check: impl Fn(usize) -> bool) -> Vec<bool> {
    let mut held = vec![false; len + 1];
    for at in (0..len).rev() {
        work::step();
        held[at] = held[at + 1] || check(at);
    }
    held
}

/// #1159: whether `git config` writes global config or a git key that runs
/// a program, for each git word of a simple command (`git`, `git.exe`, a
/// dashed helper such as `git-config` that gives its subcommand apart),
/// or opens it in an editor (#1255: `git config -e`). What follows each
/// word is worked out once, on the first git word, so judging every git
/// word of a line (`git config git config …`) stays linear.
struct GitConfigWrites<'a> {
    /// The command's words, lowercase.
    words: &'a [String],
    /// #1283: which of them the quote-aware reading cannot spell as a name,
    /// and which it cannot know (see `Classifier::word_marks`); empty when
    /// unknown.
    hidden: &'a [bool],
    unchecked: &'a [bool],
    seen: Option<FollowingWords>,
}

/// For each word of a command (and its end), what it and the words after it
/// hold.
struct FollowingWords {
    /// Where the first `config` is.
    next_config: Vec<Option<usize>>,
    /// Whether one is `--global`/`--system` or a key that runs a program.
    persisting: Vec<bool>,
    /// Whether, as the words after `config`, they only read.
    reads: Vec<bool>,
    /// #1255: whether, as the words after `config`, they open an editor.
    edits: Vec<bool>,
    /// #1255: whether a `config` among them is followed by words that open
    /// an editor (any of them: the first may be an option's value, as in
    /// `git -C config config -e`).
    config_edits: Vec<bool>,
    /// #1283: whether one of them is unchecked, or may open an editor.
    unchecked: Vec<bool>,
    editor_words: Vec<bool>,
}

impl<'a> GitConfigWrites<'a> {
    fn new(words: &'a [String], hidden: &'a [bool], unchecked: &'a [bool]) -> Self {
        Self {
            words,
            hidden,
            unchecked,
            seen: None,
        }
    }

    /// Whether `git <subcommand> words[from..]` (or `git words[from..]`)
    /// writes such config.
    fn persists(&mut self, from: usize, subcommand: Option<&str>) -> bool {
        let words = self.words;
        let unchecked = self.unchecked;
        let seen = self.seen.get_or_insert_with(|| {
            let mut next_config = vec![None; words.len() + 1];
            let mut persisting = vec![false; words.len() + 1];
            let edits = outputs::git_config_edits_each(words);
            let mut config_edits = vec![false; words.len() + 1];
            for (at, word) in words.iter().enumerate().rev() {
                work::step();
                next_config[at] = if word == "config" {
                    Some(at)
                } else {
                    next_config[at + 1]
                };
                config_edits[at] = config_edits[at + 1] || (word == "config" && edits[at + 1]);
                persisting[at] = persisting[at + 1]
                    || matches!(word.as_str(), "--global" | "--system")
                    || GIT_RUN_KEYS.contains(&word.as_str())
                    || word.starts_with("alias.");
            }
            FollowingWords {
                unchecked: suffix_any(words.len(), |at| {
                    unchecked.get(at).copied().unwrap_or(false)
                }),
                editor_words: suffix_any(words.len(), |at| outputs::may_open_editor(&words[at])),
                next_config,
                persisting,
                // #1141: a reading word given as a value (`git config -f f k
                // get`) does not make it a read.
                reads: outputs::git_config_reads_each(words),
                edits,
                config_edits,
            }
        });
        let flag = |flags: &[bool], at: usize| flags.get(at).copied().unwrap_or(false);
        let edits = flag(&seen.config_edits, from)
            || (subcommand == Some("config") && flag(&seen.edits, from));
        let start = if subcommand == Some("config") {
            from
        } else {
            match seen.next_config.get(from).copied().flatten() {
                Some(at) => at + 1,
                None => return edits,
            }
        };
        // #1283: git or its `config` spelled by text the quote-aware reading
        // cannot see (`{git,config}`, `$g config`): that reading, which keeps
        // quoted values whole, cannot check what this one makes of them.
        let hidden = |at: usize| self.hidden.get(at).copied().unwrap_or(false);
        if hidden(from - 1) || hidden(start - 1) {
            return true;
        }
        // A word it cannot know (`"$k"` with `k=core.pager`): judged here
        // only, where quoted text is split, so a read is not trusted and any
        // editor option counts.
        if flag(&seen.unchecked, from) {
            return edits || flag(&seen.editor_words, start) || flag(&seen.persisting, start);
        }
        edits || (start < seen.reads.len() && !seen.reads[start] && seen.persisting[start])
    }
}

/// The folders a command's words resolve against: the last `-C`-style
/// folder (resolved against the working folder), each folder of the chain
/// (each resolved against the one before) and the working folder, without
/// repeats.
fn folder_bases(folder: &str, chained: &[String], cwd: &str) -> Vec<String> {
    let mut bases = vec![folder.to_string()];
    for base in chained.iter().map(String::as_str).chain([cwd]) {
        work::step();
        if !bases.iter().any(|known| known == base) {
            bases.push(base.to_string());
        }
    }
    bases
}

/// Corbanu CLI flags that take the next word as their value.
const CLI_VALUE_FLAGS: &[&str] = &[
    "-c",
    "--config",
    "-p",
    "--profile",
    "-m",
    "--model",
    "--cd",
    "-s",
    "--sandbox",
    "-a",
    "--ask-for-approval",
    "-i",
    "--image",
];

/// Whether `word` sets `flag`: exactly for long flags, and inside a group of
/// short flags (`-rn` sets `-r`).
fn has_flag(word: &str, flag: &str) -> bool {
    if word == flag {
        return true;
    }
    match flag.strip_prefix('-') {
        Some(letter) if letter.len() == 1 && !letter.starts_with('-') => {
            word.starts_with('-') && !word.starts_with("--") && word[1..].contains(letter)
        }
        _ => false,
    }
}

/// #1064: programs and shell words that neither build text nor run it as
/// code (`printf` may decode `\x40`, but only to its output or a variable,
/// which another command would have to run).
const BUILDS_NO_CODE: &[&str] = &[
    // The rest of `${a[$((i+1))]}` after the lexer's `))`.
    "]",
    "}",
    "echo",
    "printf",
    ":",
    "true",
    "false",
    "test",
    "[",
    "[[",
    "]]",
    "local",
    "declare",
    "typeset",
    "export",
    "readonly",
    "unset",
    "shift",
    "return",
    "exit",
    "break",
    "continue",
    "cd",
    "pushd",
    "popd",
    "dirs",
    "read",
    "mapfile",
    "readarray",
    "wait",
    "for",
    "in",
    "case",
    "esac",
    "done",
    "fi",
    "ls",
    "cat",
    "head",
    "tail",
    "wc",
    "uniq",
    "cut",
    "tr",
    "date",
    "basename",
    "dirname",
    "mkdir",
    "touch",
    "rm",
    "cp",
    "mv",
    "seq",
    "pwd",
    "grep",
    "sleep",
    "stat",
    "realpath",
    "readlink",
];

/// #1064: whether a simple command builds no shell text it runs: the text
/// of an arithmetic expansion, no command, one of [`BUILDS_NO_CODE`] not
/// behind a wrapper, or a shell given its script on the line (judged as a
/// line of its own).
fn runs_no_built_text(simple: &SimpleCommand) -> bool {
    if simple.arithmetic {
        return true;
    }
    let Some((index, name)) = indirect::command_word(&simple.words) else {
        return simple.words.iter().all(|word| {
            work::step();
            is_assignment(word)
        });
    };
    // Before it only assignments and keywords (`do`, `then`, `!`), no
    // wrapper.
    let bare = simple.words[..index].iter().all(|word| {
        work::step();
        is_assignment(word)
            || matches!(
                word.as_str(),
                "!" | "if" | "then" | "elif" | "else" | "do" | "while" | "until"
            )
    });
    // The program by its name, not a file named by a path (`./echo`).
    let named = simple.words[index].to_lowercase() == name;
    let args = &simple.words[index + 1..];
    // `mapfile -C eval` runs its callback on each line it reads; `$o` or
    // `-$x` may spell `-C`.
    let callback = matches!(name.as_str(), "mapfile" | "readarray")
        && args.iter().any(|arg| {
            work::step();
            (arg.starts_with('-') && !arg.starts_with("--") && arg.contains('C'))
                || may_be_built(arg)
        });
    // Other options may load a file or run code first (`--rcfile notes`,
    // fish `-C./n`, `$o`): only a POSIX-style shell given known options.
    // Korn shells may read `ENV` when not interactive (ksh88, pdksh).
    let inline_script = shell::SHELLS.contains(&name.as_str())
        && !matches!(
            name.as_str(),
            "fish" | "csh" | "tcsh" | "ksh" | "mksh" | "lksh" | "oksh" | "posh"
        )
        && plain_shell_options(args)
        && matches!(
            invocation::parse(&name, args).map(|invocation| invocation.code),
            Some(Code::Inline(None))
        );
    bare && named && !callback && (BUILDS_NO_CODE.contains(&name.as_str()) || inline_script)
}

/// #1064: whether `args` are only short options of a POSIX-style shell
/// that load and run nothing (`-l`, `-c`, `-eu`, `-o pipefail`): no
/// `-i`/`-k`/`-s`, ksh's `-E`, long option or word built at run time.
fn plain_shell_options(args: &[String]) -> bool {
    const LETTERS: &str = "abcefhlmnpuvxBCHP";
    let mut names = 0usize;
    for arg in args {
        work::step();
        if names > 0 {
            names -= 1;
            if arg.is_empty()
                || !arg
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            {
                return false;
            }
            continue;
        }
        let Some(letters) = arg.strip_prefix(['-', '+']) else {
            return false;
        };
        if letters.is_empty() {
            return false;
        }
        for letter in letters.chars() {
            work::step();
            if matches!(letter, 'o' | 'O') {
                names += 1;
            } else if !LETTERS.contains(letter) {
                return false;
            }
        }
    }
    names == 0
}

/// #1064: whether `word` holds text the shell builds when it runs: a
/// variable, a substitution or unseen output.
fn may_be_built(word: &str) -> bool {
    word.contains(['$', '`']) || word.contains(shell::UNSEEN_OUTPUT)
}

/// #1064: whether `word` is an assignment as the shell reads one: a name,
/// an optional subscript, an optional `+`, then `=` (`./b=1` and zsh's
/// `=bash` are commands).
fn is_assignment(word: &str) -> bool {
    let Some((target, _)) = word.split_once('=') else {
        return false;
    };
    let target = target.strip_suffix('+').unwrap_or(target);
    let name = match target.split_once('[') {
        Some((name, subscript)) if subscript.ends_with(']') => name,
        Some(_) => return false,
        None => target,
    };
    name.starts_with(|ch: char| ch.is_ascii_alphabetic() || ch == '_')
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// #1064: folders a line may be in that a value the lexer cuts short is
/// judged against; past it such a value asks.
const MAX_VISITED_FOLDERS: usize = 64;
/// #1064: temp folders, which the agent may write.
const TEMP_FOLDERS: &[&str] = &[
    "/tmp",
    "/private/tmp",
    "/var/tmp",
    "/private/var/tmp",
    "/var/folders",
    "/private/var/folders",
    "/dev/shm",
];

/// Bytes of variable values left to substitute (#1065): any value, then
/// only small ones once that runs out.
struct Budget {
    left: usize,
    small_left: usize,
}

/// #1436: a loop with a folder change, being read: its first and last
/// command, the pass (1 to 4), where the pass started, and the state
/// after the first pass, which the line goes on from.
struct LoopPass {
    start: usize,
    end: usize,
    round: u8,
    place: Place,
    first: Option<(Folders, HashMap<String, String>)>,
    /// The variables its changes name (`cd "$D"`).
    names: Vec<String>,
    /// A word in it climbs (`../x`): a pass deeper than the one before may
    /// still reach the folders above it.
    climbs: bool,
    /// Whether `cd -` and the folder stack count in its `Place`.
    counts: (bool, bool),
    /// The folders the line may be in when the pass starts.
    folders: Vec<String>,
    /// Folders a pass that came back left the line in besides: one more
    /// pass is read from each, alone (`cd nope; cd ..`), and from those
    /// they leave in turn, up to `MAX_BRANCH_FOLDERS`.
    branches: Vec<String>,
    /// The folders passes started from or were queued from.
    seen: Vec<String>,
    /// Past `MAX_BRANCH_FOLDERS`: after the loop the folder is not known.
    overflow: bool,
}

/// #1436: the folders a reading may be in.
fn possible_folders(folders: &Folders) -> Vec<String> {
    std::iter::once(&folders.cwd)
        .chain(&folders.others)
        .cloned()
        .collect()
}

/// #1436: where a reading is: the folder it follows, where `cd -` and
/// `popd` go, the values of the variables the loop's changes name, and
/// whether it knows them. Not the other folders it may be in (a change
/// that may fail): a later pass reads them, but a pass that comes back
/// (`cd sub; …; cd ..`) has not moved.
#[derive(PartialEq, Eq)]
struct Place {
    cwd: String,
    previous: String,
    stack: std::rc::Rc<Vec<String>>,
    values: Vec<Option<String>>,
    known: (bool, bool, bool, bool, bool),
}

impl Place {
    /// `back`, `stack`: whether the loop goes back or uses the stack, so
    /// they count.
    fn of(
        folders: &Folders,
        variables: &HashMap<String, String>,
        names: &[String],
        (back, stack): (bool, bool),
    ) -> Self {
        Self {
            cwd: folders.cwd.clone(),
            previous: if back {
                folders.previous.clone()
            } else {
                String::new()
            },
            stack: if stack {
                folders.stack.clone()
            } else {
                std::rc::Rc::default()
            },
            values: names
                .iter()
                .map(|name| {
                    work::step();
                    variables.get(name).cloned()
                })
                .collect(),
            known: (
                folders.unfollowed,
                folders.lost,
                folders.unseen,
                folders.searched,
                back && folders.back_unsure,
            ),
        }
    }
}

/// Where the shell is while it runs a script: the folder, the previous one
/// (`cd -`) and the `pushd` stack.
#[derive(Clone)]
struct Folders {
    cwd: String,
    previous: String,
    stack: std::rc::Rc<Vec<String>>,
    /// A `cd`/`pushd` past the folder limits was not followed (#1065).
    unfollowed: bool,
    /// #1283: a `cd`/`pushd` took a word the quote-aware reading cannot
    /// know (`D=~; cd "$D"`), so that reading's folder may be wrong.
    unchecked: bool,
    /// #1309: other folders the line may be in: a change it followed may
    /// not have happened (`cd /nonexistent`, `false && cd x`, `(cd x)`,
    /// `cd x &`).
    others: Vec<String>,
    /// #1309: past `MAX_POSSIBLE_FOLDERS` the line may be in too many
    /// folders to follow, so a file it names relative to its folder may
    /// be anywhere.
    lost: bool,
    /// D194: folders set aside by a `cd` whose failure stops what follows.
    aside: certain_cd::Aside,
    /// #1410: a change went to a folder this reading cannot see (`cd
    /// "$X"`): a file named relative to the folder may be any file.
    unseen: bool,
    /// #1410: the same, by a change in a substitution: until the command
    /// it is part of.
    unseen_in_substitution: bool,
    /// #1436: a name that changes where `cd` goes was set (`CDPATH`,
    /// `OLDPWD`, `cdable_vars`): a later change may go anywhere.
    searched: bool,
    /// #1436: `previous` may not be where `cd -` goes: a change in a
    /// substitution or subshell, or one that may not have happened.
    back_unsure: bool,
}

struct Classifier {
    homes: Homes,
    found: Option<ProtectedActionKind>,
    scripts_read: usize,
    /// #1261: under Windows path rules, whether the first judgment lexed
    /// text holding a backslash. Its POSIX lexer drops every backslash
    /// (`bash 'C:\work\run.sh'` reads the word `C:workrun.sh`, `cd 'a\b'`
    /// the folder `ab`), so a file a word then names may not be the one that
    /// runs: from then on no file is opened.
    backslash: bool,
    /// #1461: under Windows path rules, whether a line read held `$\`, as
    /// a share does (`\\host\c$\work`, which the lexer reads as
    /// `hostc$work`, see `backslash`).
    share: bool,
    /// #1261: judging text read from a file. Under Windows path rules such
    /// text may be cmd or PowerShell code, whose folder changes and control
    /// flow the POSIX reading does not follow, so no file it names is
    /// opened, and it is also read as cmd and PowerShell read it.
    in_file: usize,
    /// #1261: under Windows path rules, whether the action is one whose
    /// script runs the first judgment follows exactly
    /// (`named_shell::windows_reads_follow`); only then does it open files.
    reads_follow: bool,
    /// #1064: the action's working folder, where the agent may write.
    workspace: String,
    /// #1064: variables whose value the word lexer may have cut short
    /// (`loader_variables::scan_split_values`).
    cut_values: HashSet<String>,
    /// #1064: the action sets `ENV`, and starts a shell that may be
    /// interactive (which sources it).
    sets_env: bool,
    interactive_shell: bool,
    /// #1064: the folders the line being judged has been in (up to
    /// `MAX_VISITED_FOLDERS`; `visited_all` is false past it).
    visited: Vec<String>,
    visited_all: bool,
    /// #1064: the `foreign` reading of the line whose joined literals are
    /// being judged (see `command`).
    rebuilt_foreign: bool,
    /// Inside text rebuilt from joined literals: it names things but is not
    /// itself run, so its variables are not judged as unseen code.
    rebuilt: usize,
    /// Inside a file that is not a shell script (Python, JavaScript, ...):
    /// shell brace expansion does not apply to it.
    not_shell: usize,
    /// #841: the sed programs of the command line being judged, when a real
    /// shell parse of it is available.
    sed_scope: Option<SedScope>,
    /// #885: script files the action runs (as resolved), and whether it runs
    /// one whose name the classifier cannot see.
    ran: Vec<String>,
    ran_unknown: bool,
    /// The action's own program (`argv[0]`): if it is missing, nothing runs.
    argv0: String,
    /// #885: files the action's command lines write (`None`: one the
    /// classifier cannot name).
    written: Vec<Option<String>>,
    /// #885: nested shell scripts judged so far.
    nested_scripts: usize,
    /// #1283: inside a nested script whose folder the quote-aware reading
    /// cannot know (run after `cd "$d"`, or by `env -C`).
    unknown_folder: usize,
    /// #1436: the command being judged runs in a folder the word reading
    /// cannot see, so a script named relative to it may be any file.
    unseen_folder: bool,
    /// #1268: inside a script a launcher runs with its input added after
    /// it (`xargs env -S'git config'`) or put into its text (`xargs -I% sh
    /// -c 'git config %'`, where any word may be input).
    launched: launched_git::Launch,
    /// #1065: bytes of variable values left to substitute.
    substitution_budget: Budget,
    /// D217: the text being read, or one it is read in, may make `sudo`
    /// other than sudo ([`unknown_folder::may_redefine_sudo`]), so its `-C`
    /// reads as a folder again. A text run in a child shell cannot change
    /// what `sudo` is in the line that runs it, and every way the line
    /// itself may load code (`.`, `source`, `eval`, `BASH_ENV`, ...) is in
    /// its own text.
    sudo_redefined: bool,
    /// D156: the line (or one that runs it) holds a quote or a backslash,
    /// which the word lexer ignores: it may end a command early there
    /// (`'a; b'`, `'a ( b'`, `a\;b`).
    quoted_text: bool,
    /// #1065: bytes of command text, and command lines, lexed so far.
    classified_bytes: usize,
    classified_lines: usize,
    /// D85: the action's own command line never reaches zsh
    /// ([`named_shell::avoids_zsh`]); nested lines are not covered.
    avoids_zsh: bool,
    /// #932: comparisons left with the grammar.
    compared: shadow::Budget,
    /// #1240: PowerShell encoded commands the Windows view judged, with
    /// the folder each was judged in: once each.
    windows_decoded: HashSet<(String, String)>,
    /// #1240: bytes of words and folders the Windows view may still
    /// resolve; past it the action fails closed.
    windows_budget: usize,
    /// D191: the action's own line's variables that hold a `mktemp` path.
    temp: Option<mktemp::Scope>,
    /// D191: prompts only a temp path raised, dropped only when the action
    /// asks for nothing else (else it asks as on main, for the strongest).
    held: Option<ProtectedActionKind>,
}

/// #841 (MOD-06): the word classifier splits quoted text into commands on
/// purpose (it over-approximates the shell), so a sed program such as
/// `'/f.write(...)/a\ ...'` yields "commands" like `/a` that name no file.
/// A word is sed's data, not a command the shell runs, when a real shell
/// parse shows the line is plain commands with literal words, the word occurs
/// in a sed program that only appends, inserts or changes text (text sed
/// never runs), and it occurs in no other word of the line.
struct SedScope {
    /// The line's text-only sed programs.
    programs: Vec<String>,
    /// Every other word of the line, command words included.
    others: Vec<String>,
}

impl SedScope {
    /// `None` (nothing is sed's data) unless the parse is plain, a sed
    /// command there has a text-only program, and the line's other words
    /// carry no backslash or brace.
    fn of(command: &[String]) -> Option<Self> {
        let commands = codex_shell_command::bash::parse_shell_lc_plain_commands(command)?;
        let piped = codex_shell_command::bash::extract_bash_command(command)
            .is_some_and(|(_, script)| script.contains('|'));
        let mut scope = Self {
            programs: Vec::new(),
            others: Vec::new(),
        };
        // A line that could change what `sed` means (an assignment such as
        // `PATH=…`, `export`, `alias`, `hash`, `enable`, sourcing) gives
        // nothing up.
        let redefines = commands.iter().any(|words| {
            words.first().is_some_and(|word| {
                word.contains('=')
                    || matches!(
                        word.as_str(),
                        "export"
                            | "alias"
                            | "hash"
                            | "enable"
                            | "set"
                            | "declare"
                            | "typeset"
                            | "readonly"
                            | "source"
                            | "."
                            | "builtin"
                            | "command"
                    )
            })
        });
        if redefines {
            return None;
        }
        for words in commands {
            work::step();
            // Only `sed` or `gsed` as written, not a path to another program.
            let sed = words
                .first()
                .is_some_and(|word| matches!(word.as_str(), "sed" | "gsed"));
            // Only plain flags: no attached or long-form program (`-es…`,
            // `-ee`, `--expression=`) and no script file (`-f`, `--file`);
            // `e` only ends a cluster.
            let plain_flags = words.iter().skip(1).all(|word| {
                let Some(cluster) = word.strip_prefix('-') else {
                    return true;
                };
                let flags = cluster.strip_suffix('e').unwrap_or(cluster);
                !cluster.is_empty() && flags.chars().all(|flag| "nEirsuz".contains(flag))
            });
            if sed && plain_flags {
                // The programs: each word after `-e`, or else the first word
                // that is not a flag; the rest are files.
                let with_e = words
                    .iter()
                    .any(|word| word.starts_with('-') && word.contains('e'));
                // #885: text sed saves (in place or through a pipe) may be
                // run later in the line, so only the address and command
                // letter of such a program are data.
                let saves = piped
                    || words
                        .iter()
                        .skip(1)
                        .any(|word| word.starts_with('-') && word.contains('i'));
                // macOS `sed -i ''`: the empty word after `-i` is its suffix.
                let mut program_next = false;
                let mut suffix_next = false;
                let mut positional_taken = false;
                for (index, word) in words.into_iter().enumerate() {
                    work::step();
                    let suffix = std::mem::take(&mut suffix_next) && word.is_empty();
                    let program = index > 0
                        && !suffix
                        && !word.starts_with('-')
                        && if with_e {
                            program_next
                        } else {
                            !std::mem::replace(&mut positional_taken, true)
                        };
                    program_next = word.starts_with('-') && word.ends_with('e');
                    suffix_next = word.starts_with('-') && word.ends_with('i');
                    if program && let Some(prefix) = sed_text_prefix(&word) {
                        let data = if saves {
                            word[..prefix].to_string()
                        } else {
                            word
                        };
                        scope.programs.push(data);
                    } else {
                        scope.others.push(word);
                    }
                }
            } else {
                scope.others.extend(words);
            }
        }
        // The word lexer drops backslashes and expands braces; the parse
        // keeps both, so such a word could hide a run of a data word.
        let usable = !scope.programs.is_empty()
            && !scope.others.iter().any(|word| word.contains(['\\', '{']));
        usable.then_some(scope)
    }

    fn is_data(&self, word: &str) -> bool {
        !word.is_empty()
            && self.programs.iter().any(|program| program.contains(word))
            && !self.others.iter().any(|other| other.contains(word))
    }
}

/// #841: whether a sed program is one `a`, `i` or `c` command (append,
/// insert or change text) after at most one address or range of line
/// numbers, `$` or `/regex/`: its text runs to the first newline not
/// escaped by a backslash, and only blank space may follow it. Sed never
/// runs such text; anything else (`s`, `e`, several commands) is not
/// text-only.
#[cfg(test)]
fn sed_text_only(program: &str) -> bool {
    sed_text_prefix(program).is_some()
}

/// For a text-only sed program (see `sed_text_only`), the length of its
/// address and command letter: the part that is not text sed outputs.
fn sed_text_prefix(program: &str) -> Option<usize> {
    fn address(rest: &str) -> Option<&str> {
        if let Some(regex) = rest.strip_prefix('/') {
            let mut escaped = false;
            for (index, ch) in regex.char_indices() {
                work::step();
                match ch {
                    _ if escaped => escaped = false,
                    '\\' => escaped = true,
                    '\n' => return None,
                    '/' => return Some(&regex[index + 1..]),
                    _ => {}
                }
            }
            return None;
        }
        if let Some(rest) = rest.strip_prefix('$') {
            return Some(rest);
        }
        Some(rest.trim_start_matches(|ch: char| ch.is_ascii_digit()))
    }
    let mut rest = address(program.trim_start_matches([' ', '\t']))?;
    if let Some(second) = rest.strip_prefix(',') {
        match address(second) {
            Some(after) if after.len() < second.len() => rest = after,
            _ => return None,
        }
    }
    let rest = rest.trim_start_matches([' ', '\t']);
    let text = rest.strip_prefix(['a', 'i', 'c'])?;
    let prefix = program.len() - text.len();
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        work::step();
        match ch {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '\n' => return text[index + 1..].trim().is_empty().then_some(prefix),
            _ => {}
        }
    }
    Some(prefix)
}

/// A file read as code.
enum FileText {
    Missing,
    /// Not a regular file, too large, past the file limit or where lookups
    /// are not allowed.
    Unreadable,
    /// An executable (by its magic number).
    Binary,
    Text(String),
}

/// Whether a script the command runs must be read in full (it is what runs)
/// or is read only if it is there (a module that may be installed).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Script {
    Required,
    IfPresent,
}

impl Classifier {
    #[cfg_attr(test, track_caller)]
    fn note(&mut self, kind: ProtectedActionKind) {
        #[cfg(test)]
        named_shell::measure_tests::noted(kind, std::panic::Location::caller());
        if self.found.is_none_or(|current| rank(kind) > rank(current)) {
            self.found = Some(kind);
        }
    }

    /// D191: a prompt that only a temp path raised is held back until the
    /// line is read (`temp`); any other is noted.
    #[cfg_attr(test, track_caller)]
    fn note_or_hold(&mut self, kind: ProtectedActionKind, temp: bool) {
        match &mut self.temp {
            Some(scope) if temp => scope.hold(kind),
            _ => self.note(kind),
        }
    }

    /// D191: a word naming a file at a temp path of this line.
    fn temp_place(&self, word: &str) -> bool {
        self.temp.is_some() && mktemp::temp_place(word)
    }

    #[cfg_attr(test, track_caller)]
    fn path(&mut self, path: &str) {
        if let Some(kind) = self.homes.classify_resolved(path) {
            self.note(kind);
        }
    }

    /// A word that may be a path. Inside scripts, payloads and patches only
    /// path-looking words are looked up on disk (the rest stay lexical), so a
    /// long body cannot spend the lookup budget.
    fn word_path(&mut self, resolved: &str, word: &str, depth: usize) {
        if depth == 0 || word.contains('/') || word.starts_with(['.', '~', '$']) {
            self.path(resolved);
        } else if let Some(kind) = self.homes.classify_path(resolved) {
            self.note(kind);
        }
    }

    /// Text that is itself a command line (a script file, a decoded payload,
    /// joined literals, patched lines).
    fn script(&mut self, text: &str, cwd: &str, depth: usize) {
        self.command(&["sh".into(), "-c".into(), text.into()], cwd, depth);
    }

    fn command(&mut self, command: &[String], cwd: &str, depth: usize) {
        if depth > MAX_DEPTH {
            // Too deeply nested to follow: fail closed.
            self.note(ProtectedActionKind::UnseenCode);
            return;
        }
        // A command line too long to classify fails closed, as does more
        // text in all than one classification lexes.
        let bytes = command.iter().map(String::len).sum::<usize>();
        self.classified_bytes = self.classified_bytes.saturating_add(bytes);
        self.classified_lines += 1;
        if bytes > MAX_COMMAND_BYTES
            || self.classified_bytes > MAX_CLASSIFIED_BYTES
            || self.classified_lines > MAX_CLASSIFIED_LINES
        {
            self.note(ProtectedActionKind::UnseenCode);
            return;
        }
        // #1261: text read from a file under Windows path rules may be cmd
        // or PowerShell code, which the POSIX lexer misreads: it is also read
        // as the Windows view reads a line (its decoded payloads too).
        if self.homes.windows_paths() && !self.homes.is_windows_view() && self.in_file > 0 {
            self.windows_text(command, cwd, depth);
        }
        // #1261: see `backslash`.
        if self.homes.windows_paths() && !self.backslash {
            self.backslash = command.iter().any(|part| {
                work::step();
                part.contains('\\')
            });
        }
        if self.homes.windows_paths() && !self.share {
            self.share = command.iter().any(|part| {
                work::step();
                part.contains("$\\")
            });
        }
        // #1240: the same text as cmd and PowerShell read it.
        if self.homes.is_windows_view() {
            self.windows_text(command, cwd, depth);
        }
        let outer_sudo_redefined = self.sudo_redefined;
        let outer_quoted_text = self.quoted_text;
        self.quoted_text |= command.iter().any(|part| part.contains(['\'', '"', '\\']));
        self.sudo_redefined |= command
            .iter()
            .any(|part| unknown_folder::may_redefine_sudo(part));
        // #841: this line's own sed programs (nested lines have theirs).
        let outer_scope = std::mem::replace(&mut self.sed_scope, SedScope::of(command));
        let lexed = shell::simple_commands(command);
        // D191: only the action's own line, judged under POSIX path rules.
        let outer_temp = std::mem::replace(
            &mut self.temp,
            (depth == 0 && !self.homes.windows_paths())
                .then(|| mktemp::Scope::of(command, &lexed.commands))
                .flatten(),
        );
        // #1064: whether the line may build shell text at run time (another
        // language's `chr(64)`, `printf '\x40' | bash`); see
        // `shell::prompt_expansion`. Text rebuilt from joined literals is
        // read from the line it was joined in.
        let foreign = if self.rebuilt > 0 {
            self.rebuilt_foreign
        } else {
            self.not_shell > 0 || !lexed.commands.iter().all(runs_no_built_text)
        };
        let pipe_sources: std::collections::HashSet<usize> = lexed
            .commands
            .iter()
            .filter_map(|command| command.pipe_from)
            .collect();
        // #1012: bash `${v@P}` expands the value as a prompt, running the
        // substitutions in it, like `eval`. Also in non-shell and rebuilt
        // text, which may reach a shell (`os.system`); the lexed words are
        // checked below.
        if (lexed.incomplete && self.not_shell == 0 && self.rebuilt == 0)
            || command
                .iter()
                .any(|arg| shell::prompt_expansion(arg, foreign))
            || lexed.too_deep
            || indirect::opaque_execution(&lexed.commands)
        {
            #[cfg(test)]
            named_shell::measure_tests::detail(|| {
                format!(
                    "lexed incomplete={} prompt={} too_deep={} opaque={}",
                    lexed.incomplete && self.not_shell == 0 && self.rebuilt == 0,
                    shell::prompt_expansion_why(command, foreign),
                    lexed.too_deep,
                    indirect::opaque_execution_why(&lexed.commands),
                )
            });
            self.note(ProtectedActionKind::UnseenCode);
        }
        let mut folders = Folders {
            cwd: cwd.to_string(),
            previous: cwd.to_string(),
            stack: std::rc::Rc::default(),
            unfollowed: false,
            unchecked: self.unknown_folder > 0,
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
        let mut variables: HashMap<String, String> = HashMap::new();
        for (name, value) in [
            ("PWD", Some(cwd)),
            ("HOME", self.homes.user_home()),
            ("CODEX_HOME", Some(self.homes.codex_home())),
            // #442: the same home under its other names.
            ("CORBANU_HOME", Some(self.homes.codex_home())),
            ("PFTERMINAL_HOME", Some(self.homes.codex_home())),
        ] {
            work::step();
            if let Some(value) = value {
                variables.insert(name.to_string(), value.to_string());
            }
        }
        // #1064: assignments the word lexer does not read as the shell does
        // (a quoted blank, `~` or `$`), judged once the line's folders are
        // known.
        let mut splits = Vec::new();
        for arg in command {
            work::step();
            let judgment =
                loader_variables::scan_split_values(arg, &mut self.cut_values, &mut splits);
            self.loader_judgment(&judgment);
        }
        // #1449: `x='a b' alias $1`, which the word lexer splits.
        let split_line = command
            .iter()
            .any(|arg| aliases::quoted_assignment_alias(arg));
        // #1064: every folder the line is in, up to a limit.
        let outer_visited = std::mem::replace(&mut self.visited, vec![cwd.to_string()]);
        let outer_visited_all = std::mem::replace(&mut self.visited_all, self.unknown_folder == 0);
        let mut lexed_seds = 0;
        let mut lexed_shells = 0;
        let mut arithmetic = arithmetic::Arithmetic::new(command);
        // Values too large to substitute, judged once each (by name and
        // length) while the lexing limits allow.
        let mut judged: HashSet<(String, usize)> = HashSet::new();
        // #1268: the word lexer drops braces, so a launcher's placeholders
        // (`{}`, `{.}`) are only seen in the text as written.
        let braces = command.iter().any(|arg| arg.contains('{'));
        let mut loops = launched_git::PositionalLoops::default();
        // #1405: a `mapfile` was found, by any spelling.
        let mut mapfile = false;
        let mut aliases = aliases::Aliases::of(&lexed.commands, split_line);
        // #1436: a loop's later passes start where the one before left
        // (`for i in 1 2; do echo x >> .zshrc; cd ~; done`): a loop with a
        // folder change whose pass moves the folder (`Place`) is read again
        // from there, up to three passes, each from the state the one
        // before ends in. If it still moves, other than deeper (`cd sub`,
        // with no `..` in the loop), it is read once more in a folder not
        // known. After it the reading goes on from the first pass, as
        // before, the folder unknown if it kept moving. Past
        // `MAX_REPLAYED_LOOPS` such a loop starts in a folder not known.
        let mut changing = unknown_folder::changing_loops(&lexed.commands).into_iter();
        let mut next_loop = changing.next();
        let mut replayed = 0;
        let mut pass: Option<LoopPass> = None;
        let mut cursor = 0;
        while let Some(simple) = lexed.commands.get(cursor) {
            work::step();
            let index = cursor;
            cursor += 1;
            if pass.is_none()
                && let Some(changing_loop) = next_loop.take_if(|found| found.start == index)
            {
                next_loop = changing.next();
                if replayed < MAX_REPLAYED_LOOPS {
                    replayed += 1;
                    pass = Some(LoopPass {
                        start: changing_loop.start,
                        end: changing_loop.end,
                        round: 1,
                        place: Place::of(
                            &folders,
                            &variables,
                            &changing_loop.names,
                            (changing_loop.back, changing_loop.stack),
                        ),
                        first: None,
                        names: changing_loop.names,
                        climbs: changing_loop.climbs,
                        counts: (changing_loop.back, changing_loop.stack),
                        folders: possible_folders(&folders),
                        branches: Vec::new(),
                        seen: Vec::new(),
                        overflow: false,
                    });
                } else {
                    folders.unseen = true;
                }
            }
            let (words, unseen, skipped) =
                substitute(simple, &variables, &mut self.substitution_budget);
            // #1436: a value may climb too (`P=../..; … > $P/.zshrc`).
            if let Some(current) = &mut pass
                && !current.climbs
            {
                current.climbs = words.iter().any(|word| word.contains(".."));
            }
            mapfile = mapfile || rerun::names_mapfile(&words);
            if let Some(scope) = &mut self.temp {
                scope.read(simple, &words);
            }
            if self.substitution_budget.left == 0 {
                self.note(ProtectedActionKind::UnseenCode);
            }
            for name in skipped {
                work::step();
                let Some(value) = variables.get(&name) else {
                    continue;
                };
                if !judged.insert((name, value.len())) {
                    continue;
                }
                if value.len() > MAX_COMMAND_BYTES
                    || self.classified_bytes.saturating_add(value.len()) > MAX_CLASSIFIED_BYTES
                {
                    self.note(ProtectedActionKind::UnseenCode);
                    continue;
                }
                self.script(value, &folders.cwd, depth + 1);
            }
            // Decoded, quote-removed and with known variables substituted:
            // `eval "echo \${v@"P"}"`, `$'\x40'`, `p=@P; eval "echo \${v$p}"`.
            if words
                .iter()
                .any(|word| shell::prompt_expansion(word, foreign))
            {
                self.note(ProtectedActionKind::UnseenCode);
            }
            // D191: a trap that only removes temp paths reads no arithmetic;
            // its prompt is held back below.
            if !self
                .temp
                .as_ref()
                .is_some_and(|scope| scope.removal_trap(&words))
            {
                arithmetic.note(simple, &words);
            }
            // #1436: once, not again in a loop's later passes.
            if pass.as_ref().is_none_or(|current| current.round == 1) {
                lexed_seds += sed::positions(&words).len();
                lexed_shells += nested::shell_commands(&words);
            }
            let feeds_pipe = pipe_sources.contains(&index);
            // #1410: a substitution's changes end with it.
            if !simple.substituted {
                folders.unseen_in_substitution = false;
            }
            // D194: where a certain `cd` stops being certain.
            let certain = certain_cd::before(
                &mut folders,
                if simple.substituted {
                    certain_cd::Joint::Other
                } else {
                    simple.after
                },
                &simple.words,
                simple.stdin_from.is_some()
                    || simple.here_string.is_some()
                    || !simple.writes_to.is_empty(),
            );
            // #1422: an alias the line defines runs text neither reading
            // reads as the shell does (`alias b=bash` then `b run.sh`).
            if aliases.read(&words) {
                self.note(ProtectedActionKind::UnseenCode);
            }
            self.simple_command(
                simple,
                &words,
                &unseen,
                feeds_pipe,
                braces,
                &mut folders,
                &mut variables,
                &mut loops,
                depth,
                certain,
            );
            // #1309: past its possible folders the line may be anywhere.
            if folders.lost || folders.unseen || self.unknown_folder > 0 {
                self.visited_all = false;
            }
            for folder in std::iter::once(&folders.cwd).chain(&folders.others) {
                work::step();
                if self.visited.contains(folder) {
                    continue;
                }
                if self.visited.len() >= MAX_VISITED_FOLDERS {
                    self.visited_all = false;
                } else {
                    self.visited.push(folder.clone());
                }
            }
            // #1436: the end of a loop being read again.
            if let Some(current) = &mut pass
                && current.end == index
            {
                let place = Place::of(&folders, &variables, &current.names, current.counts);
                let now = possible_folders(&folders);
                if current.round == 0 {
                    // A pass from another folder: folders it leaves that no
                    // pass has started from are read too, up to a limit.
                    for folder in now {
                        work::step();
                        if current.seen.contains(&folder) {
                            continue;
                        }
                        if current.seen.len() < MAX_BRANCH_FOLDERS {
                            current.branches.push(folder.clone());
                        } else {
                            current.overflow = true;
                        }
                        current.seen.push(folder);
                    }
                } else {
                    let moved = place != current.place;
                    // A pass that comes back may still leave other folders
                    // the line may be in (`cd nope; cd ..`).
                    let widened = now.iter().any(|folder| !current.folders.contains(folder));
                    let deeper = !current.climbs
                        && place.known == current.place.known
                        && place
                            .cwd
                            .strip_prefix(current.place.cwd.as_str())
                            .is_some_and(|rest| rest.starts_with('/'));
                    if current.round == 1 && (moved || widened) {
                        current.first = Some((folders.clone(), variables.clone()));
                    }
                    let next = match current.round {
                        1 | 2 if moved => Some(current.round + 1),
                        3 if moved && !deeper => Some(current.round + 1),
                        _ => None,
                    };
                    if let Some(round) = next {
                        folders.unseen |= round == 4;
                        current.round = round;
                        current.place = place;
                        current.folders = now;
                        cursor = current.start;
                    } else if current.round < 4 && widened {
                        // Read once more from each of them, alone (round
                        // 0 marks these passes).
                        current.seen = current.folders.clone();
                        for folder in now {
                            work::step();
                            if !current.seen.contains(&folder) {
                                current.branches.push(folder.clone());
                                current.seen.push(folder);
                            }
                        }
                        current.round = 0;
                    } else {
                        if let Some((first, values)) = current.first.take() {
                            folders = first;
                            variables = values;
                            folders.unseen |= current.round == 4;
                        }
                        pass = None;
                    }
                }
                if let Some(current) = &mut pass
                    && current.round == 0
                {
                    match (current.branches.pop(), &current.first) {
                        (Some(branch), Some((first, values))) => {
                            folders = first.clone();
                            variables = values.clone();
                            folders.cwd = branch;
                            folders.others.clear();
                            cursor = current.start;
                        }
                        _ => {
                            if let Some((first, values)) = current.first.take() {
                                folders = first;
                                variables = values;
                                // Past the limit the line may be anywhere.
                                folders.unseen |= current.overflow;
                            }
                            pass = None;
                        }
                    }
                }
            }
        }
        if !splits.is_empty() {
            let here: Vec<&str> = self.visited.iter().map(String::as_str).collect();
            let roots = std::cell::OnceCell::new();
            let windows = self.homes.windows_paths();
            let risky = !self.visited_all
                || splits.iter().any(|split| {
                    work::step();
                    loader_variables::judge_split(split, windows, &|entry| {
                        self.writable_folder(entry, &here, &roots)
                    })
                });
            if risky {
                self.note(ProtectedActionKind::UnseenCode);
            }
        }
        certain_cd::finish(&mut folders);
        // #1405: the word lexer ignores quotes (`-d 'x>' "$o"`): a line
        // with a `mapfile` is read with them too. #1422: text that may not
        // be shell is read loosely.
        let strict = self.not_shell == 0 && self.rebuilt == 0;
        if mapfile
            && command
                .iter()
                .any(|arg| rerun::quoted_mapfile_callback(arg, strict))
        {
            self.note(ProtectedActionKind::UnseenCode);
        }
        // #937: arithmetic on a variable filled from command output or
        // `read` runs the substitutions in its value, like `eval`.
        // #1028: zsh `e`/`+` glob qualifiers run code the classifier does
        // not read as commands; #1084: so may a value under glob
        // substitution. D85: not on the action's own line when it never
        // reaches zsh. #1142: a variable named at run time may be one git
        // runs (`let "$n=1"`, `${(P)n=x}`).
        let zsh_possible = !(depth == 0 && self.avoids_zsh);
        if (self.rebuilt == 0
            && self.not_shell == 0
            && (arithmetic.runs_unseen(command)
                || arithmetic.assigns_unseen_name(command)
                || (zsh_possible
                    && (arithmetic.runs_qualifier_code(command)
                        || arithmetic.uses_glob_substitution(command)))))
            || folders.unfollowed
        {
            #[cfg(test)]
            named_shell::measure_tests::detail(|| {
                let gated = self.rebuilt == 0 && self.not_shell == 0;
                let zsh = gated && zsh_possible;
                let runs = gated && arithmetic.runs_unseen(command);
                format!(
                    "arith ru={runs} why={} an={} qc={} gs={} uf={} unf={} qcw={:?} gsw={}",
                    if runs {
                        arithmetic.runs_unseen_why(command)
                    } else {
                        "-".to_string()
                    },
                    gated && arithmetic.assigns_unseen_name(command),
                    zsh && arithmetic.runs_qualifier_code(command),
                    zsh && arithmetic.uses_glob_substitution(command),
                    folders.unfollowed,
                    named_shell::measure_tests::unfollowed_reasons(),
                    if zsh {
                        arithmetic.qualifier_trigger(command)
                    } else {
                        None
                    },
                    if zsh {
                        arithmetic.glob_subst_why(command)
                    } else {
                        "-".to_string()
                    },
                )
            });
            self.note(ProtectedActionKind::UnseenCode);
        }
        // D191: the prompts only a temp path raised, kept now unless its
        // variable kept that path (see `release_held`).
        if let Some(scope) = std::mem::replace(&mut self.temp, outer_temp) {
            match scope.finish(arithmetic.assigns_unseen_name(command)) {
                (Some(kind), true) => self.note(kind),
                (Some(kind), false) => {
                    self.held = Some(self.held.map_or(kind, |held| held.strongest(kind)));
                }
                (None, _) => {}
            }
        }
        self.path(&folders.cwd);
        // Strings assembled from literals by inline code (`'~/.co' + 'dex'`).
        let outer_foreign = std::mem::replace(&mut self.rebuilt_foreign, foreign);
        for arg in command {
            work::step();
            for joined in indirect::joined_literals(arg) {
                work::step();
                let resolved = self.homes.resolve(&joined, &folders.cwd);
                self.word_path(&resolved, &joined, depth);
                self.rebuilt += 1;
                self.script(&joined, &folders.cwd, depth + 1);
                self.rebuilt -= 1;
            }
        }
        self.rebuilt_foreign = outer_foreign;
        // #885: nested shells' scripts and sed programs, read with quotes.
        self.quoted_pass(command, cwd, depth, (lexed_seds, lexed_shells));
        self.sed_scope = outer_scope;
        self.sudo_redefined = outer_sudo_redefined;
        self.quoted_text = outer_quoted_text;
        self.visited = outer_visited;
        self.visited_all = outer_visited_all;
    }

    #[allow(clippy::too_many_arguments)]
    fn simple_command(
        &mut self,
        simple: &SimpleCommand,
        words: &[String],
        unseen: &[bool],
        feeds_pipe: bool,
        braces: bool,
        folders: &mut Folders,
        variables: &mut HashMap<String, String>,
        loops: &mut launched_git::PositionalLoops,
        depth: usize,
        certain: bool,
    ) {
        // #1436: a script named relative to a folder this reading cannot
        // see may be any file (`cd "$X" && sh ./run.sh`), also in a script
        // run there or a decoded payload.
        let outer_unseen_folder = self.unseen_folder;
        self.unseen_folder = outer_unseen_folder
            || folders.unseen
            || folders.unseen_in_substitution
            || folders.lost
            || self.unknown_folder > 0;
        let lower: Vec<String> = words.iter().map(|word| word.to_lowercase()).collect();
        let (hidden, mut unchecked) = self.word_marks(simple, words, variables);
        loops.read(&simple.words);
        let looped = loops.marks(&simple.words);
        // #1283: a loop variable over the positional parameters is a value
        // neither reading knows, wherever its quoting takes effect.
        for (unchecked, looped) in unchecked.iter_mut().zip(&looped) {
            work::step();
            *unchecked |= *looped;
        }
        let command = indirect::command_word(words);
        // Assignments (`A=x`, `export A=x`) feed later `$A` words.
        let comment = words.first().is_some_and(|word| word.starts_with('#'));
        let assigns = !comment
            && command.as_ref().is_none_or(|(_, name)| {
                matches!(
                    name.as_str(),
                    "export" | "local" | "declare" | "readonly" | "typeset"
                )
            });
        if assigns {
            for word in words {
                work::step();
                if let Some((name, value)) = word.split_once('=')
                    && !name.is_empty()
                    && name
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                {
                    // Output the classifier cannot see keeps the variable
                    // unseen; an environment reference (`$JAVA_HOME/bin/java`)
                    // is kept as written.
                    // Once unseen, a variable stays unseen for the action
                    // (a later assignment may not run).
                    let temp = self
                        .temp
                        .as_ref()
                        .and_then(|scope| scope.value(name, value, !variables.contains_key(name)));
                    let value = if let Some(temp) = temp {
                        temp.to_string()
                    } else if value.contains(shell::UNSEEN_OUTPUT)
                        || variables
                            .get(name)
                            .is_some_and(|old| old.contains(shell::UNSEEN_OUTPUT))
                    {
                        shell::UNSEEN_OUTPUT.to_string()
                    } else {
                        value.to_string()
                    };
                    variables.insert(name.to_string(), value);
                }
            }
        }
        // #1064: a nameref (`declare -n r=v`) reads another variable's
        // value, which the substituted words do not follow: its value is
        // unseen.
        for name in rerun::namerefs(command.as_ref(), words) {
            work::step();
            variables.insert(name.to_string(), shell::UNSEEN_OUTPUT.to_string());
        }
        if self.rebuilt == 0 && indirect::misread_command(simple, words) {
            self.note(ProtectedActionKind::UnseenCode);
        }
        // #1436: as the quote-aware reading reads them, when set.
        folders.searched |= unknown_folder::sets_search(words, command.as_ref());
        if let Some((index, name)) = &command {
            let args = &words[index + 1..];
            // Code named by a variable or substitution the classifier cannot
            // see: the command word (`$X`, `bash -c "$X"`), an interpreter's
            // inline code (`python3 -c "$X"`) or `eval "$x"`.
            // Inline code, or an argument after it (`sh -c 'eval "$1"' _ "$X"`).
            let inline_unseen = indirect::invocation(words).is_some_and(|invocation| {
                matches!(invocation.code, Code::Inline(Some(code))
                    if words
                        .iter()
                        // Attached code (`-c$X`) ends its option word.
                        .position(|word| word.ends_with(code))
                        .is_some_and(|at| unseen[at..].iter().any(|unseen| *unseen)))
            });
            // #988: `trap` runs its action later, like `eval` now. The word
            // lexer splits a quoted action, so every operand counts.
            // #1064: and other commands that run an operand as code.
            let eval_unseen = (matches!(name.as_str(), "eval" | "trap")
                && unseen[index + 1..].iter().any(|unseen| *unseen))
                || rerun::runs_unseen(name, words, unseen, *index)
                || rerun::runner_unseen(words, unseen, *index)
                || rerun::mapfile_callback(simple, words, unseen);
            if self.rebuilt == 0 && (unseen[*index] || inline_unseen || eval_unseen) {
                // D191: a trap that only removes temp paths.
                let removes = name == "trap"
                    && !unseen[*index]
                    && !inline_unseen
                    && self
                        .temp
                        .as_ref()
                        .is_some_and(|scope| scope.removes_temp(args));
                #[cfg(test)]
                if !(removes && self.temp.is_some()) {
                    named_shell::measure_tests::detail(|| {
                        format!(
                            "cmd word_unseen={} inline={inline_unseen} evaltrap={} runs={} \
                             runner={} mapfile={} name={name:?} word={:?}",
                            unseen[*index],
                            matches!(name.as_str(), "eval" | "trap")
                                && unseen[index + 1..].iter().any(|unseen| *unseen),
                            rerun::runs_unseen(name, words, unseen, *index),
                            rerun::runner_unseen(words, unseen, *index),
                            rerun::mapfile_callback(simple, words, unseen),
                            words[*index],
                        )
                    });
                }
                self.note_or_hold(ProtectedActionKind::UnseenCode, removes);
            }
            folders.unchecked |= matches!(name.as_str(), "cd" | "pushd")
                && unchecked[index + 1..].iter().any(|word| *word);
            // `pushd -n x`, `popd +1`: the folder stack changes, the folder
            // may not (or goes to an entry of a stack not followed).
            // #1403: nor does a program named by a path (`./cd sub`): the
            // file runs, not the builtin (or a function of that name).
            // #1422: nor `CD sub` or a wrapper's `env cd sub`.
            if unknown_folder::stack_change(name, args)
                || unknown_folder::external_change(words, *index, name)
            {
                #[cfg(test)]
                named_shell::measure_tests::unfollowed_reason(|| {
                    if unknown_folder::stack_change(name, args) {
                        "stack_change"
                    } else {
                        "external_change"
                    }
                });
                folders.unfollowed = true;
            } else {
                // #1309: this reading knows the words of `cd "$d"`; it loses
                // its folder only past the possible folders.
                let unknown = folders.lost;
                let changes = matches!(name.as_str(), "cd" | "pushd");
                // #1436: `cd -` (`~-`) before any change goes to the
                // `OLDPWD` the action started with; after one that may not
                // have happened, or happened in a substitution or subshell,
                // it may go there too.
                let back = changes
                    && (folders.previous == folders.cwd
                        || folders.back_unsure
                        || !folders.others.is_empty())
                    && args.iter().any(|arg| arg == "-" || arg.starts_with("~-"));
                self.follow_change(name, args, folders, variables, unknown, certain);
                folders.back_unsure |= unknown_folder::CHANGERS.contains(&name.as_str())
                    && (simple.substituted || !folders.others.is_empty());
                // #1410: nor a target it cannot see (a variable never set
                // in the action, a command's output: `cd "$ZDOTDIR"`, `cd
                // "$(cat f)"`), which may be any folder; nor zsh `chdir`,
                // which it does not follow. In a substitution, only for the
                // commands of that substitution. #1436: nor one a search
                // name may send anywhere (`CDPATH="$X" cd sub`, `cd -`).
                if name == "chdir"
                    || (changes
                        && (back
                            || folders.searched
                            || args.iter().any(|word| self.unseen_value(word))))
                {
                    if simple.substituted {
                        folders.unseen_in_substitution = true;
                    } else {
                        folders.unseen = true;
                    }
                }
            }
        }
        let mut git_config = GitConfigWrites::new(&lower, &hidden, &unchecked);
        let mut local_git_config = git_config::Writes::new(words);
        let mut launched_git = launched_git::LaunchedGit::new(
            words,
            &lower,
            braces,
            self.launched,
            launched_git::Marks {
                hidden: &hidden,
                unchecked: &unchecked,
                looped: &looped,
                ..launched_git::Marks::default()
            },
        );
        // #1159 follow-up: zsh's `=word` runs the program `word` on the
        // path (`=$X`, `=$(…)`: one the classifier cannot see). The command
        // reading skips such a word as an assignment, so that program is
        // never judged (`=$X log --output=~/.zshrc`, `echo x | =tee -a
        // ~/.zshrc`, `nice =sort -o ~/.zshrc f`): before the command word,
        // or right after a precommand modifier (`noglob =tee …`), it asks.
        // Also before the command after precommand modifiers (`noglob
        // command =tee`), and right after an exec marker or an exec wrapper
        // (`find . -exec =curl`, `strace =curl`); not on a comment line.
        //
        // #1159 follow-up: the command after zsh's precommand modifiers
        // (`noglob git log --output=~/.zshrc`, `noglob tee -a ~/.zshrc`)
        // writes too. The modifier stays the command word, as before (a
        // function may have its name): this only adds what it writes.
        let after_modifiers = command
            .as_ref()
            .filter(|(_, name)| unknown_folder::MODIFIERS.contains(&name.as_str()))
            .map(|(index, _)| {
                words[index + 1..]
                    .iter()
                    .position(|word| {
                        work::step();
                        !unknown_folder::MODIFIERS.contains(&word.as_str())
                    })
                    .map_or(words.len(), |offset| index + 1 + offset)
            });
        let modified = after_modifiers.and_then(|after| {
            indirect::command_word(&words[after..]).map(|(at, name)| (after + at, name))
        });
        let skipped_to = match (after_modifiers, &modified, &command) {
            (Some(_), Some((at, _)), _) | (None, _, Some((at, _))) => *at,
            _ => lower.len(),
        };
        let runs_next = |word: &str| {
            matches!(word, "-exec" | "-execdir" | "-ok" | "-okdir")
                || outbound::EXEC_WRAPPERS.contains(&basename(word))
                || unknown_folder::MODIFIERS.contains(&word)
        };
        // An exec wrapper's command may be any later word (`strace -f =tee`).
        let exec_wrapped = lower
            .get(skipped_to)
            .is_some_and(|word| outbound::EXEC_WRAPPERS.contains(&basename(word)));
        if !comment
            && lower.iter().enumerate().any(|(at, word)| {
                work::step();
                word.len() > 1
                    && word.starts_with('=')
                    && (at < skipped_to
                        || (exec_wrapped && at > skipped_to)
                        || (at > 0 && runs_next(&lower[at - 1])))
            })
        {
            self.note(ProtectedActionKind::UnseenCode);
        }
        let mut later = LaterWords::new(&lower);
        let mut git_programs_read = false;
        // Not only at command position: wrappers (`nice -n 5`, `sudo -u x`,
        // `timeout 9`, `eval`, `npx`, nested `sh -c`) put the real command later.
        for (index, word) in lower.iter().enumerate() {
            work::step();
            let name = basename(word);
            let imported = index > 0 && matches!(lower[index - 1].as_str(), "from" | "import");
            if CLI_NAMES.contains(&name) && !imported {
                self.cli(&mut later, index + 1);
            }
            // #1159: git under another name (`git.exe`), or a dashed helper
            // (`/usr/lib/git-core/git-config`) that runs `git <subcommand>`.
            // #1276: or a finder's exec option with git attached
            // (`fd --exec=git`).
            let git_word = launched_git.attached(index).unwrap_or(word);
            let is_git = basename(git_word) == "git" || indirect::equals_git(git_word);
            // #1159: zsh `=git` (`=git.exe`, `=git-config`) the command
            // reading does not take for a git command word: a wrapper's
            // option loop skips it (`nice =git log --output=~/.zshrc`), it
            // follows a command (`sudo =git`, `noglob =git`), or it names a
            // dashed helper. The output, working-tree and other
            // command-word checks then never see git: it asks.
            if word.strip_prefix('=').is_some_and(|rest| {
                basename(rest) == "git" || git_helpers::git_name(rest).is_some()
            }) && command
                .as_ref()
                .is_none_or(|(at, name)| *at != index || name != "git")
            {
                self.note(ProtectedActionKind::UnseenCode);
            }
            let helper = git_helpers::git_name(git_word);
            let subcommand = helper.as_ref().and_then(Option::as_deref);
            if persistence_command(name, &mut later, index + 1)
                || ((is_git || helper.is_some()) && git_config.persists(index + 1, subcommand))
            {
                self.note(ProtectedActionKind::Persistence);
            }
            // Also inspect local command-bearing keys in Git invocations
            // found through wrappers or dashed helpers, using the shared name.
            let inspecting = command.as_ref().is_some_and(|(at, _)| {
                *at == index && git_config::inspection_only_prefix(&words[..index])
            });
            let launched_kind = launched_git.judge(index, helper.as_ref());
            if (name == "git" || helper.is_some())
                && !inspecting
                // Unknown launcher input is already classified by the shared check.
                && launched_kind != Some(ProtectedActionKind::UnseenCode)
                && local_git_config.writes_code_setting(index + 1, subcommand)
            {
                self.note(ProtectedActionKind::Persistence);
            }
            // #1268: git given words from a launcher's input (`xargs git`).
            if let Some(kind) = launched_kind {
                self.note(kind);
            }
            launched_git.step(index);
            // A helper named by text the classifier cannot see may be any
            // subcommand (`git-$x`, `git-conf*`), like `git $x`.
            if subcommand.is_some_and(|subcommand| {
                subcommand.contains(['$', '*', '?', '['])
                    || subcommand.contains(shell::UNSEEN_OUTPUT)
                    || simple
                        .words
                        .get(index)
                        .is_some_and(|raw| basename(raw).contains('$'))
            }) {
                self.note(ProtectedActionKind::UnseenCode);
            }
            // #1142: `git -c core.pager=…` runs the value (args as written).
            // A dashed helper takes no git options.
            let git_args = (
                &words[index + 1..],
                simple.words.get(index + 1..).unwrap_or_default(),
            );
            // #1159: programs git runs or copies, read once per command.
            let git_program = (is_git || helper.is_some())
                && !std::mem::replace(&mut git_programs_read, true)
                && rerun::git_runs_program(git_args.0, subcommand);
            // D156: git or a dashed helper spelled by text the quote-aware
            // reading cannot see (`$g`, braces, `$IFS`) is not checked
            // there, and here its words may be cut inside quotes (`$g
            // clone 'a; b' --upload-pack=./x u`): in a line with quoted text
            // it asks.
            let hidden_git = (is_git || helper.is_some())
                && self.quoted_text
                && hidden.get(index).copied().unwrap_or(false);
            if (is_git && code_variables::git_runs_code_after_git(git_args.0, git_args.1))
                || (helper == Some(None) && code_variables::git_runs_code(git_args.0, git_args.1))
                || git_program
                || hidden_git
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
        let stdin_fed =
            simple.pipe_from.is_some() || simple.stdin_from.is_some() || simple.reads_outer_output;
        if let Some((index, _)) = &command
            && let Some(kind) = outbound::classify(
                words,
                *index,
                stdin_fed,
                variables
                    .keys()
                    .any(|name| name.to_lowercase().ends_with("_proxy")),
            )
        {
            self.note(kind);
        }
        let recursive = command.as_ref().is_some_and(|(_, name)| {
            RECURSIVE_READERS.iter().any(|(reader, flags)| {
                *reader == name.as_str()
                    && (flags.is_empty()
                        || (name == "find" && feeds_pipe)
                        || lower
                            .iter()
                            .any(|word| flags.iter().any(|flag| has_flag(word, flag))))
            })
        });
        // #1142: setting a variable whose value git or a pager runs.
        if code_variables::sets_code_variable(simple, words, command.as_ref()) {
            #[cfg(test)]
            named_shell::measure_tests::detail(|| {
                code_variables::sets_code_variable_why(
                    simple,
                    words,
                    command.as_ref(),
                    self.not_shell > 0 || self.rebuilt > 0,
                )
            });
            self.note(ProtectedActionKind::UnseenCode);
        }
        // #1064: a variable that makes a later program load code, set to a
        // risky value (`PATH=./bin:$PATH`, `NODE_OPTIONS=--require=x`).
        let here: Vec<&str> = [folders.cwd.as_str(), folders.previous.as_str()]
            .into_iter()
            .chain(folders.others.iter().map(String::as_str))
            .chain(self.visited.iter().map(String::as_str))
            .collect();
        let mut cut = std::mem::take(&mut self.cut_values);
        let roots = std::cell::OnceCell::new();
        let judgment = loader_variables::judge(
            simple,
            words,
            command.as_ref(),
            &mut cut,
            self.homes.windows_paths(),
            &|entry| !self.visited_all || self.writable_folder(entry, &here, &roots),
        );
        self.cut_values = cut;
        self.loader_judgment(&judgment);
        if !self.interactive_shell {
            let fed = simple.pipe_from.is_some()
                || simple.stdin_from.is_some()
                || simple.here_string.is_some()
                || simple.reads_outer_output;
            self.interactive_shell = loader_variables::starts_interactive_shell(&lower, fed);
        }
        // #1141: an output option makes a writer of its command, and its
        // value is a file written (`git log --output=~/.zshrc`, `-o.zshrc`).
        let outputs = command.as_ref().and_then(|(index, name)| {
            // #1283: a `config` the quote-aware reading cannot spell.
            // A wrapper's words count too (`env -C "$d" git`), and so does a
            // folder changed through one (`cd "$d"`).
            let unchecked =
                hidden[*index] || folders.unchecked || unchecked.iter().any(|word| *word);
            outputs::output_files(name, &words[index + 1..], unchecked)
        });
        let modified_outputs = modified.as_ref().and_then(|(index, name)| {
            let unchecked =
                hidden[*index] || folders.unchecked || unchecked.iter().any(|word| *word);
            outputs::output_files(name, &words[index + 1..], unchecked)
        });
        if command
            .as_ref()
            .is_some_and(|(index, name)| outputs::joins_names(name, &words[index + 1..]))
        {
            self.note(ProtectedActionKind::Persistence);
        }
        let writer = command.as_ref().map(|(index, name)| {
            (
                *index,
                WRITERS.contains(&name.as_str())
                    || outputs.is_some()
                    || modified_outputs.is_some()
                    || modified
                        .as_ref()
                        .is_some_and(|(_, name)| WRITERS.contains(&name.as_str())),
            )
        });
        let path_writer = command
            .as_ref()
            .filter(|(_, name)| outputs::PATH_WRITERS.contains(&name.as_str()))
            .map(|(index, _)| *index);
        // #1159: `time -o FILE cmd` writes FILE.
        let wrapper_outputs = command
            .as_ref()
            .map(|(index, _)| outputs::wrapper_output_files(&words[..*index]))
            .unwrap_or_default();
        let cwd = folders.cwd.clone();
        // Words resolve against the folder of a `-C`/`--directory` flag
        // (`tar -C ~ ...`, `git -C ~/.codex ...`) once one has been given.
        let mut folder = cwd.clone();
        // #1141: each relative `-C` resolves against the one before it
        // (`git -C ~ -C . ...` runs in `~`). Words resolve against every
        // folder of the chain too: the word lexer drops empty words, so
        // `-C ~ -C '' log` reads as `-C ~ -C log`.
        let mut chained: Vec<String> = Vec::new();
        let mut previous: Option<&str> = None;
        let mut bases = vec![cwd.clone()];
        // #1309: the folders written words resolve against from each other
        // folder the line may be in, after the folder flags given so far.
        let mut flags: Vec<&str> = Vec::new();
        let mut other_bases = std::borrow::Cow::Borrowed(folders.others.as_slice());
        // D217: sudo's `-C N` names no folder.
        let closes = unknown_folder::sudo_close_from(words, self.sudo_unsure());
        for (position, word) in words.iter().enumerate() {
            work::step();
            let written = simple.writes_to.contains(&position)
                || writer.is_some_and(|(index, writes)| writes && position > index);
            // A folder flag's value is where the command runs, not what it reads.
            let mut folder_value = false;
            if let Some(flag) = previous
                && FOLDER_FLAGS.contains(&flag)
                && !(position > 0 && closes.binary_search(&(position - 1)).is_ok())
            {
                folder = self.homes.resolve(word, &cwd);
                self.chain_folder(&mut chained, word, &cwd);
                folder_value = true;
                flags.push(word);
            } else if let Some((flag, value)) = word.split_once('=')
                && FOLDER_FLAGS.contains(&flag)
                && closes.binary_search(&position).is_err()
            {
                folder = self.homes.resolve(value, &cwd);
                self.chain_folder(&mut chained, value, &cwd);
                folder_value = true;
                flags.push(value);
            }
            if folder_value {
                bases = folder_bases(&folder, &chained, &cwd);
                if flags.len() <= MAX_CHAINED_FOLDERS {
                    other_bases = self.other_bases(&folders.others, &flags, &bases).into();
                }
            }
            previous = Some(word.as_str());
            // #1159: a file written whose name holds output the classifier
            // cannot see (`tee "$(cat f)"`, `> $(cat f)`) may be any file.
            if (simple.writes_to.contains(&position)
                || (path_writer.is_some_and(|index| position > index)
                    && simple.stdin_from != Some(position)
                    && simple.here_string != Some(position)))
                && outputs::unseen_place(word)
            {
                self.note_or_hold(ProtectedActionKind::Persistence, self.temp_place(word));
            }
            // Attached, assigned and volume forms (`-o/path`, `f=@/path`,
            // `~/.aws:/c`) are paths too.
            let pieces = std::iter::once(word.as_str()).chain(
                word.split(['=', ':'])
                    .filter(|piece| piece.len() < word.len()),
            );
            for candidate in pieces {
                work::step();
                let candidate = candidate.trim_start_matches('@');
                if candidate.is_empty() || candidate.starts_with('-') && !candidate.contains('/') {
                    continue;
                }
                let candidate = match candidate.find('/') {
                    Some(slash) if candidate.starts_with('-') => &candidate[slash..],
                    _ => candidate,
                };
                // `-C` means something else to some commands (`grep -C 3`):
                // resolve against both folders.
                for base in &bases {
                    work::step();
                    let resolved = self.homes.resolve(candidate, base);
                    self.word_path(&resolved, candidate, depth);
                    if written
                        && (self.homes.is_persistence(&resolved)
                            || self.unknown_place(folders, candidate))
                    {
                        let temp = self.temp_place(candidate);
                        self.note_or_hold(ProtectedActionKind::Persistence, temp);
                    }
                    if recursive && !folder_value && self.homes.holds_a_home(&resolved) {
                        self.note(ProtectedActionKind::Credentials);
                    }
                }
                // #1309: a file written in each other folder the line may be in.
                for base in other_bases.iter().filter(|_| written) {
                    work::step();
                    let resolved = self.homes.resolve(candidate, base);
                    self.word_path(&resolved, candidate, depth);
                    if self.homes.is_persistence(&resolved) {
                        let temp = self.temp_place(candidate);
                        self.note_or_hold(ProtectedActionKind::Persistence, temp);
                    }
                }
            }
            if let Some(payload) = indirect::decoded_payload(word) {
                self.script(&payload, &cwd, depth + 1);
            }
        }
        // #1159: git writing the working tree in a folder that is or holds
        // a home. D195 (#1309): or in one the classifier cannot know,
        // unless git's own leading `-C` names one (`git -C /x pull`; not a
        // value elsewhere, `merge -m --cd=/x`).
        if let Some((index, name)) = &command
            && outputs::writes_working_tree(name, &words[index + 1..])
            && (bases
                .iter()
                .chain(other_bases.iter())
                .any(|base| self.homes.holds_a_home(&self.homes.resolve(".", base)))
                || (self.unknown_place(folders, ".")
                    && !(name.as_str() == "git"
                        && words[index + 1..]
                            .chunks(2)
                            .take_while(|pair| pair.len() == 2 && pair[0] == "-C")
                            .any(|pair| {
                                work::step();
                                !self.relative_place(&pair[1])
                            }))))
        {
            self.note(ProtectedActionKind::Persistence);
        }
        for file in outputs
            .iter()
            .flatten()
            .chain(&wrapper_outputs)
            .chain(modified_outputs.iter().flatten())
        {
            work::step();
            if outputs::unseen_place(file) {
                self.note_or_hold(ProtectedActionKind::Persistence, self.temp_place(file));
            }
            for base in bases.iter().chain(other_bases.iter()) {
                work::step();
                let resolved = self.homes.resolve(file, base);
                self.word_path(&resolved, file, depth);
                // #1159: files of any name written in a folder (`--work-tree
                // ~`, `--output-directory ~/`) may be the home's.
                if self.homes.is_persistence(&resolved)
                    || (file.ends_with('/') && self.homes.holds_a_home(&resolved))
                    || self.unknown_place(folders, file)
                {
                    self.note_or_hold(ProtectedActionKind::Persistence, self.temp_place(file));
                }
            }
        }
        self.scripts_of(simple, words, unseen, command.as_ref(), &cwd, depth);
        self.path_programs(words, command.as_ref(), &cwd, depth);
        self.unseen_folder = outer_unseen_folder;
    }

    /// A program named by a path (`./python3`, `./env`, `/usr/bin/env`) is
    /// the file that runs, whatever its name: it is read as any program run
    /// by its path, besides what its name would run. A shell builtin named
    /// by a path (`./cd`, `./source`) runs that file, not the builtin, so
    /// the rest of the line is not known: it asks.
    fn path_programs(
        &mut self,
        words: &[String],
        command: Option<&(usize, String)>,
        cwd: &str,
        depth: usize,
    ) {
        // #1261: Windows path rules follow their own reads.
        if self.homes.windows_paths() {
            return;
        }
        // `scripts_of` read a command that names no shell or interpreter.
        let read = command
            .filter(|_| indirect::invocation(words).is_none())
            .map(|(index, _)| *index);
        let elsewhere = unknown_folder::elsewhere_at(words, self.sudo_unsure());
        for at in indirect::programs(words) {
            work::step();
            let word = &words[at];
            if !word.contains('/') {
                continue;
            }
            // #1403: also when `scripts_of` read it (`./cd`, `./export`).
            if PATH_BUILTINS.contains(&basename(&word.to_lowercase())) {
                self.note(ProtectedActionKind::UnseenCode);
            } else if Some(at) != read && !self.runs_elsewhere(elsewhere, at, word) {
                self.named_program_file(word, cwd, depth);
            }
        }
    }

    /// A program named by a path whose name is a known tool's: judged as a
    /// script when it is text. An executable is not one the action ran as
    /// a script (`/bin/bash`). Missing, it asks as any program run by its
    /// path, unless it is the action's own program: then nothing runs.
    fn named_program_file(&mut self, word: &str, cwd: &str, depth: usize) {
        if word.contains('$') {
            return;
        }
        let file = self.homes.resolve(word, cwd);
        let data = self
            .sed_scope
            .as_ref()
            .is_some_and(|scope| scope.is_data(word));
        match self.read_file(&file, word, cwd) {
            FileText::Binary => {}
            FileText::Missing if data || (depth == 0 && self.argv0 == word) => {}
            FileText::Missing | FileText::Unreadable => {
                self.ran.push(file);
                self.note(ProtectedActionKind::UnseenCode);
            }
            FileText::Text(text) => {
                self.ran.push(file);
                self.script(&text, cwd, depth + 1);
            }
        }
    }

    /// Whether `file`, a word of the program at `at` or after it, names a
    /// file relative to a folder a wrapper before that program moved to
    /// (`env -C sub bash run.sh`, `sudo -D sub ./run.sh`): which file runs
    /// is not known, so it asks.
    /// `from`: `unknown_folder::elsewhere_at` of the command's words.
    /// #1436: in a folder the word reading cannot see, such a file may be
    /// another too: it asks, and the file here is still read, as before.
    fn runs_elsewhere(&mut self, from: Option<usize>, at: usize, file: &str) -> bool {
        let moved = from.is_some_and(|from| from < at);
        let relative = !self.homes.windows_paths()
            && (moved || self.unseen_folder)
            && (file.contains("..")
                || self.homes.resolve(file, "/a") != self.homes.resolve(file, "/b"));
        if relative {
            self.note(ProtectedActionKind::UnseenCode);
        }
        if relative && !moved {
            self.ran_unknown = true;
        }
        relative && moved
    }

    /// #1283: for each of a simple command's words, whether the quote-aware
    /// reading (`nested.rs`), which checks git on the words as the shell
    /// passes them on, cannot spell it as a name (`hidden`: `$g` for `git`,
    /// a brace expansion, `$IFS`, a substitution's output; not
    /// `"$HOME/bin/git"`), or cannot know its text (`unchecked`: also any
    /// variable it does not resolve, as in `k=core.pager; git config "$k"`).
    /// `variables`: as the words were substituted.
    fn word_marks(
        &self,
        simple: &SimpleCommand,
        words: &[String],
        variables: &HashMap<String, String>,
    ) -> (Vec<bool>, Vec<bool>) {
        let user_home = self.homes.user_home();
        let codex_home = self.homes.codex_home();
        // The homes the quote-aware reading resolves itself.
        let resolved = |name: &str, value: &str| match name {
            "HOME" => user_home == Some(value),
            "CODEX_HOME" | "CORBANU_HOME" | "PFTERMINAL_HOME" => codex_home == value,
            _ => false,
        };
        let mut hidden = Vec::with_capacity(words.len());
        let mut unchecked = Vec::with_capacity(words.len());
        for (word, raw) in words.iter().zip(&simple.words) {
            work::step();
            hidden.push(basename(&word.to_lowercase()) != basename(&raw.to_lowercase()));
            unchecked.push(
                word != raw
                    && raw.match_indices('$').any(|(at, _)| {
                        work::step();
                        let after = &raw[at + 1..];
                        let end = after
                            .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
                            .unwrap_or(after.len());
                        let name = &after[..end];
                        variables
                            .get(name)
                            .is_some_and(|value| !resolved(name, value))
                    }),
            );
        }
        for at in &simple.hidden {
            work::step();
            if let (Some(hidden), Some(unchecked)) = (hidden.get_mut(*at), unchecked.get_mut(*at)) {
                *hidden = true;
                *unchecked = true;
            }
        }
        (hidden, unchecked)
    }

    /// D217: whether `sudo` may not be sudo, or its options are not
    /// sudo's: see [`unknown_folder::sudo_close_from`].
    fn sudo_unsure(&self) -> bool {
        self.sudo_redefined || self.homes.windows_paths()
    }

    /// Follow a relative `-C` from the folder before it. Past
    /// `MAX_CHAINED_FOLDERS` folders, or a folder name past
    /// `MAX_FOLDER_BYTES`, the chain is not followed (#1102: `-C a -C a ...`
    /// would grow it without bound) and the line asks.
    fn chain_folder(&mut self, chained: &mut Vec<String>, value: &str, cwd: &str) {
        let next = self
            .homes
            .resolve(value, chained.last().map_or(cwd, String::as_str));
        if chained.len() >= MAX_CHAINED_FOLDERS || next.len() > MAX_FOLDER_BYTES {
            self.note(ProtectedActionKind::UnseenCode);
        } else {
            chained.push(next);
        }
    }

    /// Read what the command runs from files: its script, a local Python
    /// module, preloaded files, a redirected stdin, or the command itself when
    /// it is a path. A required path the classifier cannot resolve (it holds
    /// a variable) fails closed.
    fn scripts_of(
        &mut self,
        simple: &SimpleCommand,
        words: &[String],
        unseen: &[bool],
        command: Option<&(usize, String)>,
        cwd: &str,
        depth: usize,
    ) {
        let Some((index, name)) = command else {
            return;
        };
        let local = |word: &str| word.starts_with(['.', '/', '~']);
        let elsewhere = if self.homes.windows_paths() {
            None
        } else {
            unknown_folder::elsewhere_at(words, self.sudo_unsure())
        };
        let Some(invocation) = indirect::invocation(words) else {
            let word = &words[*index];
            // `env -C sub ./run.sh` runs `sub/run.sh`.
            if word.contains('/') && !self.runs_elsewhere(elsewhere, *index, word) {
                // A binary through an environment path (`$JAVA_HOME/bin/java`)
                // is not a script; an unseen one was already judged.
                if !word.contains('$') {
                    let file = self.homes.resolve(word, cwd);
                    // #841 (MOD-06): a "command" split out of a quoted sed
                    // program (an address such as `/a`) is sed's data, not a
                    // command the shell runs: read it if it is a file, but a
                    // missing one does not fail closed.
                    let need = if self
                        .sed_scope
                        .as_ref()
                        .is_some_and(|scope| scope.is_data(word))
                    {
                        Script::IfPresent
                    } else {
                        Script::Required
                    };
                    self.script_file(&file, word, cwd, depth, need);
                }
            }
            return;
        };
        let shell =
            shell::SHELLS.contains(&name.as_str()) || matches!(name.as_str(), "source" | ".");
        match invocation.code {
            Code::File(file) => {
                if !self.runs_elsewhere(elsewhere, *index, file) {
                    self.required_script(file, cwd, depth, shell);
                }
            }
            Code::Module(module) if module.contains('$') => {
                self.note(ProtectedActionKind::UnseenCode);
            }
            Code::Module(module) => {
                let module = module.replace('.', "/");
                for candidate in [format!("{module}.py"), format!("{module}/__main__.py")] {
                    work::step();
                    if self.runs_elsewhere(elsewhere, *index, &candidate) {
                        continue;
                    }
                    let file = self.homes.resolve(&candidate, cwd);
                    self.not_shell += 1;
                    self.script_file(&file, &candidate, cwd, depth, Script::IfPresent);
                    self.not_shell -= 1;
                }
            }
            Code::Stdin => {
                // #1436: the file the shell opens, in its folder.
                if let Some(at) = simple.stdin_from
                    && !self.runs_elsewhere(/*from*/ None, at, &words[at])
                {
                    self.required_script(&words[at], cwd, depth, shell);
                }
                if simple.here_string.is_some_and(|at| unseen[at]) {
                    self.note(ProtectedActionKind::UnseenCode);
                }
            }
            Code::Inline(_) | Code::Tool => {}
        }
        for preload in invocation.preloads {
            work::step();
            if self.runs_elsewhere(elsewhere, *index, preload) {
                continue;
            }
            // #1405: a shell's start-up file is a path it sources; an empty
            // one (`--rcfile /dev/null`) runs nothing.
            if shell {
                if preload != "/dev/null" {
                    self.required_script(preload, cwd, depth, shell);
                }
            } else if local(preload) {
                self.required_script(preload, cwd, depth, /*shell*/ false);
            } else {
                let file = self.homes.resolve(preload, cwd);
                self.not_shell += 1;
                self.script_file(&file, preload, cwd, depth, Script::IfPresent);
                self.not_shell -= 1;
            }
        }
    }

    /// A script the command certainly runs: read in full or fail closed.
    /// (#932 measuring: in test builds a finding noted here is sited at
    /// the caller, which tells the kinds of script apart.)
    #[cfg_attr(test, track_caller)]
    fn required_script(&mut self, file: &str, cwd: &str, depth: usize, shell: bool) {
        if file.contains('$') || file.contains(shell::UNSEEN_OUTPUT) {
            self.ran_unknown = true;
            self.note(ProtectedActionKind::UnseenCode);
            return;
        }
        let resolved = self.homes.resolve(file, cwd);
        if !shell {
            self.not_shell += 1;
        }
        self.script_file(&resolved, file, cwd, depth, Script::Required);
        if !shell {
            self.not_shell -= 1;
        }
    }

    /// #1283: in a folder the classifier cannot know, a file named relative
    /// to it (`.zshenv`, `../x`, `$d/x`) may be any file.
    /// #1309: so too past `MAX_POSSIBLE_FOLDERS`.
    /// #1410: so too after a change to a folder it cannot see.
    fn unknown_place(&self, folders: &Folders, word: &str) -> bool {
        (self.unknown_folder > 0
            || folders.lost
            || folders.unseen
            || folders.unseen_in_substitution)
            && self.relative_place(word)
    }

    /// #1410: whether a word (with the variables this reading knows, the
    /// homes among them, substituted) holds a value it cannot see: a
    /// variable it does not know (`$ZDOTDIR`, `$1`) or a command's output.
    /// As written: a path's cleanup would drop `$X` from `$X/..`. Under
    /// Windows path rules (#1461) too, but not a `$` that may be a share's
    /// (#1261: `\\host\c$\work`): one ending a segment or, once the line
    /// held `$\` (see `share`), one after a letter or digit.
    fn unseen_value(&self, word: &str) -> bool {
        let bytes = word.as_bytes();
        outputs::unseen_place(word)
            || word.match_indices('$').any(|(at, _)| {
                !(self.homes.windows_paths()
                    && (matches!(bytes.get(at + 1), None | Some(b'/' | b'\\'))
                        || (self.share && at > 0 && bytes[at - 1].is_ascii_alphanumeric())))
            })
    }

    /// Whether `word` names a file relative to the folder it is used in.
    fn relative_place(&self, word: &str) -> bool {
        word.contains("..")
            // #1261: under Windows path rules `C:x` is relative to drive
            // `C:`'s own folder, which the probes below do not change.
            || (self.homes.windows_paths() && windows_reads::drive_relative(word))
            || self.homes.resolve(word, "/a") != self.homes.resolve(word, "/b")
    }

    /// #1064: note what [`loader_variables`] found.
    fn loader_judgment(&mut self, judgment: &loader_variables::Judgment) {
        self.sets_env |= judgment.sets_env;
        if judgment.risky {
            self.note(ProtectedActionKind::UnseenCode);
        }
    }

    /// #1064: whether `entry` (a search-path entry, absolute or under `~`)
    /// is a folder the agent may write: in the action's working folder, in
    /// a folder of `here` (where the line is; the first resolves `entry`),
    /// or in a temp folder. Also through canonical forms. `roots`: their
    /// spellings, worked out on first use.
    fn writable_folder(
        &self,
        entry: &str,
        here: &[&str],
        roots: &std::cell::OnceCell<Vec<String>>,
    ) -> bool {
        let cwd = here.first().copied().unwrap_or(&self.workspace);
        let resolved = self.homes.resolve(entry, cwd);
        if resolved.starts_with('~') {
            // The home is unknown.
            return true;
        }
        let key = |path: &str| path.replace('\\', "/").to_lowercase();
        let spellings = |path: &str| {
            let mut spellings = vec![key(path)];
            spellings.extend(self.homes.canonical(path).map(|canonical| key(&canonical)));
            spellings
        };
        let roots = roots.get_or_init(|| {
            let temp = std::env::temp_dir().to_string_lossy().into_owned();
            let mut roots = Vec::new();
            for root in [self.workspace.as_str()]
                .into_iter()
                .chain(here.iter().copied())
                .chain(TEMP_FOLDERS.iter().copied())
                .chain([temp.as_str()])
            {
                work::step();
                if !root.is_empty() {
                    roots.extend(
                        spellings(&self.homes.resolve(root, cwd))
                            .into_iter()
                            .map(|root| root.trim_end_matches('/').to_string()),
                    );
                }
            }
            roots
        });
        spellings(&resolved).iter().any(|path| {
            roots.iter().any(|root| {
                work::step();
                path.strip_prefix(root.as_str())
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
            })
        })
    }

    /// Follow `cd`, `pushd` and `popd` (also behind `builtin`/`command`).
    /// Returns whether the followed folder changed.
    fn change_folder(
        &self,
        name: &str,
        args: &[String],
        folders: &mut Folders,
        variables: &mut HashMap<String, String>,
    ) -> bool {
        let target = args
            .iter()
            .find(|word| !word.starts_with('-') || *word == "-");
        let next = match (name, target) {
            // #1261: under Windows path rules through `resolve`, which keeps
            // the home from being read (WSL's is not Windows').
            ("cd", None) if self.homes.windows_paths() => {
                Some(self.homes.resolve("~", &folders.cwd))
            }
            ("cd", None) => self.homes.user_home().map(str::to_string),
            ("cd", Some(target)) if target == "-" => Some(folders.previous.clone()),
            ("cd" | "pushd", Some(target)) => Some(self.homes.resolve(target, &folders.cwd)),
            ("popd", _) => std::rc::Rc::make_mut(&mut folders.stack).pop(),
            _ => None,
        };
        if let Some(next) = next {
            if next.len() > MAX_FOLDER_BYTES
                || (name == "pushd" && folders.stack.len() >= MAX_FOLDER_STACK)
            {
                #[cfg(test)]
                named_shell::measure_tests::unfollowed_reason(|| "folder_limits");
                folders.unfollowed = true;
                return false;
            }
            if name == "pushd" {
                std::rc::Rc::make_mut(&mut folders.stack).push(folders.cwd.clone());
            }
            folders.previous = std::mem::replace(&mut folders.cwd, next);
            variables.insert("PWD".to_string(), folders.cwd.clone());
            variables.insert("OLDPWD".to_string(), folders.previous.clone());
            return true;
        }
        false
    }

    /// The checks on a Corbanu CLI word, given the words after it (`later`
    /// from `from`).
    fn cli(&mut self, later: &mut LaterWords, from: usize) {
        if later.holds(from, &["vault"]) {
            self.note(ProtectedActionKind::Vault);
        }
        if later
            .subcommand(from)
            .is_some_and(|sub| CLI_POLICY_WORDS.contains(&sub))
        {
            self.note(ProtectedActionKind::SecurityPolicy);
        }
        if later.policy_flag(from) {
            self.note(ProtectedActionKind::SecurityPolicy);
        }
    }

    /// A script run from a file is judged by its text (a patched script run
    /// later, `bash x.sh`, `./run`). Executables are recognised by their
    /// magic number and skipped. A required script the classifier cannot read
    /// in full (missing, written by the same command, not a regular file, too
    /// large, past the file limit, or where lookups are not allowed) fails
    /// closed; an optional one is skipped when it is not there. `file` is
    /// `word` resolved in `cwd`.
    #[cfg_attr(test, track_caller)]
    fn script_file(&mut self, file: &str, word: &str, cwd: &str, depth: usize, need: Script) {
        // #1240: the first judgment reads script files; the Windows view's
        // paths are not ones to open (and where it may not look a missing
        // optional script would read as unreadable).
        if self.homes.is_windows_view() {
            return;
        }
        self.ran.push(file.to_string());
        // #932 measuring: the line that asked for this script (in test
        // builds this function and `required_script` track their caller).
        #[cfg(test)]
        let caller = std::panic::Location::caller().line();
        let unreadable = |classifier: &mut Self, missing: bool| {
            if need == Script::Required || !missing {
                #[cfg(test)]
                named_shell::measure_tests::detail(|| {
                    let why = if !classifier.homes.lookups_allowed(file) {
                        "no_lookup"
                    } else {
                        match std::fs::metadata(file) {
                            Err(_) => "missing",
                            Ok(metadata) if !metadata.is_file() => "not_file",
                            Ok(_) if classifier.scripts_read >= MAX_SCRIPT_FILES => "too_many",
                            Ok(_) => "read_fail",
                        }
                    };
                    let need = if need == Script::Required {
                        "req"
                    } else {
                        "opt"
                    };
                    format!(
                        "script why={why} need={need} caller={caller} depth={depth} in_file={} \
                         word={word:?} file={file:?} cwd={cwd:?}",
                        classifier.in_file
                    )
                });
                classifier.note(ProtectedActionKind::UnseenCode);
            }
        };
        match self.read_file(file, word, cwd) {
            FileText::Missing => unreadable(self, /*missing*/ true),
            FileText::Unreadable => unreadable(self, /*missing*/ false),
            FileText::Binary => {}
            FileText::Text(text) => {
                // #1261: see `in_file`.
                self.in_file += 1;
                self.script(&text, cwd, depth + 1);
                self.in_file -= 1;
            }
        }
    }

    /// Read a file the action runs or uses as code, within the lookup and
    /// file limits. `file` is `word` resolved in `cwd`.
    fn read_file(&mut self, file: &str, word: &str, cwd: &str) -> FileText {
        // #1261: under Windows path rules only through `windows_reads`.
        if self.homes.windows_paths() {
            return self.read_windows_file(word, cwd);
        }
        if !self.homes.lookups_allowed(file) {
            return FileText::Unreadable;
        }
        // Check before opening: opening a FIFO or device would block.
        let Ok(metadata) = std::fs::metadata(file) else {
            return FileText::Missing;
        };
        if !metadata.is_file() {
            return FileText::Unreadable;
        }
        self.scripts_read += 1;
        if self.scripts_read > MAX_SCRIPT_FILES {
            return FileText::Unreadable;
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Never block if the file was swapped for a FIFO after the check.
            options.custom_flags(libc::O_NONBLOCK);
        }
        let Ok(handle) = options.open(file) else {
            return FileText::Unreadable;
        };
        Self::read_text(handle)
    }

    /// #1261: [`Self::read_file`] under Windows path rules: only a plain path
    /// on a local fixed drive, never through a link or share.
    fn read_windows_file(&mut self, word: &str, cwd: &str) -> FileText {
        // A file the first judgment may not name as the shell does is not
        // opened; as on main (where no Windows file could be opened) a
        // required one then asks and an optional one is not looked for.
        if !self.reads_follow || self.backslash || self.in_file > 0 {
            return FileText::Missing;
        }
        let Some((drive, names)) = self.homes.windows_read_path(word, cwd) else {
            return FileText::Unreadable;
        };
        let homes = &self.homes;
        let handle = match windows_reads::locate(drive, &names, &|| homes.take_lookup()) {
            windows_reads::Located::File(handle) => handle,
            windows_reads::Located::Missing => return FileText::Missing,
            windows_reads::Located::Refused => return FileText::Unreadable,
        };
        self.scripts_read += 1;
        if self.scripts_read > MAX_SCRIPT_FILES {
            return FileText::Unreadable;
        }
        match Self::read_text(handle) {
            // A Cygwin or Interix link, or a Windows shortcut (its header
            // `L 00 00 00 01 14 02 00` without the NUL bytes), which Git Bash
            // follows.
            FileText::Text(text)
                if text.starts_with("!<symlink>")
                    || text.starts_with("IntxLNK")
                    || text.starts_with("L\u{1}\u{14}\u{2}") =>
            {
                FileText::Unreadable
            }
            read => read,
        }
    }

    /// The text of an opened file, within the file limits.
    fn read_text(handle: std::fs::File) -> FileText {
        use std::io::Read;
        if !handle.metadata().is_ok_and(|metadata| metadata.is_file()) {
            return FileText::Unreadable;
        }
        let mut bytes = Vec::new();
        let mut reader = handle.take(indirect::SCRIPT_READ_LIMIT + 1);
        if reader.read_to_end(&mut bytes).is_err() {
            return FileText::Unreadable;
        }
        // Executables first: their size does not matter.
        if BINARY_MAGIC.iter().any(|magic| bytes.starts_with(magic)) {
            return FileText::Binary;
        }
        if bytes.len() as u64 > indirect::SCRIPT_READ_LIMIT {
            return FileText::Unreadable;
        }
        // The shell skips NUL bytes; so does the scan.
        bytes.retain(|byte| *byte != 0);
        FileText::Text(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// Shell parameters that stand for the command's own arguments or status,
/// which are already in front of the classifier.
fn is_special_parameter(name: &str) -> bool {
    matches!(name, "@" | "*" | "#" | "?" | "$" | "!" | "-" | "0")
        || (name.len() == 1 && name.chars().all(|ch| ch.is_ascii_digit()))
}

/// Words with known `$NAME` variables replaced, left to right as the shell
/// reads them (`$a$b` is `a` then `b`), and for each word whether it holds a
/// value the classifier cannot see: a variable never assigned in this action
/// (other than special parameters) or one set from an unseen substitution.
/// `budget` is the bytes of values left to substitute (#1065); a value
/// that does not fit is unseen output, runs the budget out, and is
/// returned by name.
fn substitute(
    command: &SimpleCommand,
    variables: &HashMap<String, String>,
    budget: &mut Budget,
) -> (Vec<String>, Vec<bool>, HashSet<String>) {
    let mut skipped = HashSet::new();
    let (words, unseen): (Vec<String>, Vec<bool>) = command
        .words
        .iter()
        .map(|word| {
            let mut out = String::new();
            let mut unseen = word.contains(shell::UNSEEN_OUTPUT);
            let mut rest = word.as_str();
            while let Some(at) = rest.find('$') {
                work::step();
                out.push_str(&rest[..at]);
                let after = &rest[at + 1..];
                let len = match after.chars().next() {
                    Some(ch) if "@*#?$!-".contains(ch) || ch.is_ascii_digit() => ch.len_utf8(),
                    _ => after
                        .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
                        .unwrap_or(after.len()),
                };
                let name = &after[..len];
                match variables.get(name) {
                    Some(value) if len > 0 && value.len() < budget.left => {
                        budget.left -= value.len();
                        unseen |= value.contains(shell::UNSEEN_OUTPUT);
                        out.push_str(value);
                    }
                    Some(value)
                        if len > 0
                            && value.len() <= MAX_SMALL_VALUE
                            && value.len() <= budget.small_left =>
                    {
                        budget.left = 0;
                        budget.small_left -= value.len();
                        unseen |= value.contains(shell::UNSEEN_OUTPUT);
                        out.push_str(value);
                    }
                    Some(_) if len > 0 => {
                        budget.left = 0;
                        unseen = true;
                        if !skipped.contains(name) {
                            skipped.insert(name.to_string());
                        }
                        out.push_str(shell::UNSEEN_OUTPUT);
                    }
                    _ => {
                        unseen |= len > 0 && !is_special_parameter(name);
                        out.push('$');
                        out.push_str(name);
                    }
                }
                rest = &after[len..];
            }
            out.push_str(rest);
            (out, unseen)
        })
        .unzip();
    (words, unseen, skipped)
}

/// The level as other product messages name it ("security level Moderate").
fn level_name(level: SecurityLevel) -> &'static str {
    match level {
        SecurityLevel::Permissive => "Permissive",
        SecurityLevel::Moderate => "Moderate",
        SecurityLevel::Aggressive => "Aggressive",
    }
}

#[cfg(test)]
#[path = "tainted_action_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tainted_action_refusal_tests.rs"]
mod tainted_action_refusal_tests;
