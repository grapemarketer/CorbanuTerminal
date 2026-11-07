//! #442: under Aggressive, reads of protected files (credential files such as
//! `.env`, `~/.ssh` or cloud and CLI tokens, and the Corbanu home) are never
//! granted, not even once. This module finds the protected path a command
//! names, so that:
//! - a command that failed because the sandbox denied such a read is
//!   explained where it failed (the level, never grantable, how to change
//!   it), instead of a bare "Operation not permitted";
//! - the approval prompt for such a command offers no grant and says why;
//! - #620: the same for a start-up file outside the current folder that a
//!   command writes (`>> ~/.zshrc`): Aggressive's sandbox never writes
//!   there, so no grant could let the command write it;
//! - #584: the same for a command that writes only paths no grant lifts
//!   under Aggressive (#520: the project's `.codex` and `.agents`; the
//!   Corbanu home);
//! - #737, #740: the same for a command that writes only outside the
//!   current folder (a config file such as `~/.gitconfig` that `git config
//!   --global` writes, or any other file) or in a read-only part of it (the
//!   project's `.git`), or that, as far as its text shows, writes nothing a
//!   grant lifts at all;
//! - #736: the same for a macOS keychain command (`security
//!   find-generic-password`): the keychain services are out of reach of
//!   Aggressive's sandbox (#758: denied after its network rules too), so it
//!   runs but cannot read or change items;
//! - #622: such a denial is listed under the inspector's recent denials.
//! - #748: under Moderate, a denial its rules add is explained too (the
//!   [`moderate`] module).
//!
//! Explanation only: it never changes what runs or what is denied (it only
//! withholds a grant offer that could not apply). The path
//! lookups run off the async thread, bounded by a timeout.

use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::ReadDenyMatcher;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::WarningEvent;
use codex_security_policy::SecurityLevel;
use codex_utils_absolute_path::AbsolutePathBuf;

use super::launch_contract::KeptFile;
use super::protected_surface::ReadDenials;
use super::tainted_action::PolicyBinding;
use super::tainted_action::USER_PERSISTENCE;
use crate::session::session::Session;
use crate::session::turn_context::TurnContext;
use codex_protocol::permissions::EffectiveWrites;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use writes::keychain_command;
use writes::write_targets;

#[path = "protected_reads_writes.rs"]
mod writes;

/// #826 follow-up: whether `command` runs a program that always needs a
/// user namespace (text-based; see `writes::user_namespace_command`).
pub(crate) fn runs_user_namespace_program(command: &[String]) -> bool {
    writes::user_namespace_command(command)
}

/// Output that says the operating system refused access (on Linux a write
/// outside the writable folders reports a read-only file system; #620).
const DENIAL_MARKERS: [&str; 3] = [
    "operation not permitted",
    "permission denied",
    "read-only file system",
];

/// #1296: whether commands run on Windows, where PowerShell and cmd spell
/// paths (`C:\Users\me`, `$env:USERPROFILE`, `%USERPROFILE%`) and report
/// denials (`Access is denied.`) their own way.
const WINDOWS: bool = cfg!(windows);

/// Whether `line` reports a denial (see [`DENIAL_MARKERS`]); `windows`
/// (#1296) adds Windows' texts: cmd's (Win32's) `Access is denied.` and
/// PowerShell's (.NET's) `Access to the path '…' is denied.`.
fn is_denial_line(line: &str, windows: bool) -> bool {
    let line = line.to_ascii_lowercase();
    DENIAL_MARKERS.iter().any(|marker| line.contains(marker))
        || (windows
            && (line.contains("access is denied")
                || (line.contains("access to the path") && line.contains("is denied"))))
}

/// #1296: cmd's bare `Access is denied.` names no path, so the denial is
/// attributed to the protected path the command names.
fn names_no_path(line: &str, windows: bool) -> bool {
    windows
        && line
            .trim()
            .trim_end_matches('.')
            .eq_ignore_ascii_case("access is denied")
}

/// The words of `command`, shell scripts split into words. #1296: with
/// `windows`, a command no POSIX shell runs (PowerShell, cmd) is split as
/// they split it: `\` is a path separator there, not an escape.
fn command_words(command: &[String], windows: bool) -> Vec<String> {
    let posix = !windows || command.first().is_some_and(|program| posix_shell(program));
    command
        .iter()
        .flat_map(|argument| argument_words(argument, posix))
        .collect()
}

/// The words of one argument of a command (see [`command_words`]).
fn argument_words(argument: &str, posix: bool) -> Vec<String> {
    if posix {
        shlex::split(argument)
            .unwrap_or_else(|| argument.split_whitespace().map(str::to_string).collect())
    } else {
        windows_words(argument)
    }
}

/// #1296: whether `program` is a POSIX shell (Git for Windows' `bash.exe`).
fn posix_shell(program: &str) -> bool {
    let name = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    matches!(name, "bash" | "sh" | "zsh" | "dash" | "ksh")
}

/// #1296: `text` split into words as PowerShell and cmd split it, as far as
/// paths go: at whitespace outside quotes; `'…'` and `"…"` quote (the quotes
/// are dropped); `\` is kept.
fn windows_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote = None;
    for ch in text.chars() {
        match quote {
            Some(open) if ch == open => quote = None,
            Some(_) => word.push(ch),
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                started = true;
            }
            None if ch.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            None => {
                word.push(ch);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// How long the path lookups may take before no explanation is given.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(2);

/// #902: the bound in a test process that asks for it (see
/// [`unhurried_lookups_for_tests`]).
const TEST_LOOKUP_TIMEOUT: Duration = Duration::from_secs(20);

static UNHURRIED_LOOKUPS: AtomicBool = AtomicBool::new(false);

/// #902: test harnesses call this so the path lookups get
/// [`TEST_LOOKUP_TIMEOUT`] instead of [`LOOKUP_TIMEOUT`]. Each lookup does
/// many filesystem calls; on a loaded machine (many test processes, a slow
/// disk) they took over 2 seconds, so the explanation and the withheld grant
/// offer the tests check went missing. Process-wide and in memory only; what
/// a lookup finds is unchanged. Production code must not call it.
pub(crate) fn unhurried_lookups_for_tests(enabled: bool) {
    UNHURRIED_LOOKUPS.store(enabled, Ordering::Relaxed);
}

pub(crate) fn lookup_timeout() -> Duration {
    // #997: this crate's own unit tests get the test bound too; under load
    // they took over 2 seconds as the integration suites did (#902).
    // `cfg!(test)` is never set in a build that ships.
    lookup_timeout_in(/*unit_test*/ cfg!(test))
}

/// The bound in a unit-test build (`unit_test`) or any other process.
fn lookup_timeout_in(unit_test: bool) -> Duration {
    if unit_test || UNHURRIED_LOOKUPS.load(Ordering::Relaxed) {
        TEST_LOOKUP_TIMEOUT
    } else {
        LOOKUP_TIMEOUT
    }
}

/// The explanation shown when `path` could not be accessed.
pub(crate) fn explanation(path: &str) -> String {
    let path = shown_path(path);
    format!(
        "Security level Aggressive blocked access to `{path}`: agent commands can never read \
         protected files (credential files such as .env, .env.* and .envrc, also ones created \
         during the session, but not .env.example, .env.sample or .env.template; SSH, cloud \
         and CLI tokens; and Corbanu's sign-in, vault, config and history), and this cannot be \
         granted, not even \
         once. To allow it, a person can switch to Permissive in /security (it applies after a \
         restart)."
    )
}

/// #506: the explanation shown when listing the folder `path` was denied.
pub(crate) fn listing_explanation(path: &str) -> String {
    format!(
        "Security level Aggressive blocked listing `{}`: agent commands can never list the \
         Corbanu home or read its protected files (Corbanu's sign-in, vault, config and \
         history), and this cannot be granted, not even once. To allow it, a person can switch \
         to Permissive in /security (it applies after a restart).",
        shown_path(path)
    )
}

/// #620: the explanation for a start-up file outside the current folder
/// (`~/.zshrc`): Aggressive's sandbox writes only inside the current folder,
/// and a grant lifts the start-up file protections only there.
pub(crate) fn startup_explanation(path: &str) -> String {
    format!(
        "Security level Aggressive blocks writing `{}`: it is, or leads to, a start-up file \
         outside the current folder, and agent commands can write only inside the current \
         folder, even when you approve them; this cannot be granted, not even once (Grant once \
         lifts the start-up file and git write protections only inside the current folder). To \
         allow it, a person can switch to Permissive in /security (it applies after a restart).",
        shown_path(path)
    )
}

/// #584: the explanation for a write to a path no grant lifts under
/// Aggressive (#520: the project's `.codex` and `.agents`; the Corbanu home
/// outside `worktrees`).
pub(crate) fn never_writable_explanation(path: &str) -> String {
    format!(
        "Security level Aggressive blocks writing `{}`: agent commands can never write the \
         project's .codex and .agents folders (agent settings) or the Corbanu home (outside its \
         worktrees folder), even when you approve them, and this cannot be granted, not even \
         once. To allow it, a person can switch to Permissive in /security (it applies after a \
         restart).",
        shown_path(path)
    )
}

/// #740: the explanation for a write outside the current folder (#737: a
/// config file such as `~/.npmrc`): Aggressive's sandbox writes only inside
/// it, and a grant lifts protections only there.
pub(crate) fn outside_folder_explanation(path: &str) -> String {
    format!(
        "Security level Aggressive blocks writing `{}`: it is outside the current folder, and \
         agent commands can write only inside the current folder, even when you approve them; \
         this cannot be granted, not even once (Grant once lifts the start-up file and git write \
         protections only inside the current folder). To allow it, a person can switch to \
         Permissive in /security (it applies after a restart).",
        shown_path(path)
    )
}

/// #740: the explanation for a write to a read-only part of the current
/// folder that no grant lifts (the project's `.git`).
pub(crate) fn read_only_in_folder_explanation(path: &str) -> String {
    format!(
        "Security level Aggressive blocks writing `{}`: agent commands cannot write it, even \
         when you approve them (it is read-only in this session, as the project's .git folder, \
         its hooks and config included, always is); this cannot be granted, not even once \
         (Grant once lifts only git hooks and config kept outside a .git folder and, when the \
         current folder holds your home folder, start-up files). To allow it, a person can \
         switch to Permissive in /security (it applies after a restart).",
        shown_path(path)
    )
}

/// #740: why no grant is offered for a command that, as far as its text
/// shows, writes nothing a grant lifts.
pub(crate) const NOTHING_TO_LIFT: &str = "Grant once is not offered for this command: under \
     security level Aggressive it only lifts the write protection of git hooks and config kept \
     outside a .git folder and, when the current folder holds your home folder, of start-up \
     files, and nothing this command writes is protected that way.";

/// #736: the approval prompt's explanation for a macOS keychain command.
pub(crate) const KEYCHAIN_PROMPT: &str = "Security level Aggressive keeps the macOS keychain out \
     of reach of agent commands: if you approve, `security` runs, but it cannot reach the \
     keychain service, so it cannot read, add or change keychain items (it reports them as not \
     found, or an error), and this cannot be granted, not even once. To allow it, a person can \
     switch to Permissive in /security (it applies after a restart).";

/// #736: the explanation where a macOS keychain command failed.
pub(crate) const KEYCHAIN_DENIAL: &str = "Security level Aggressive kept the macOS keychain out \
     of reach of this command: agent commands cannot reach the keychain service, so `security` \
     could not read, add or change keychain items (an error such as \"could not be found\" comes \
     from that, not from what your keychain holds), and this cannot be granted, not even once. \
     To allow it, a person can switch to Permissive in /security (it applies after a restart).";

/// `path` as an explanation shows it: it is the command's own spelling, so
/// escaped and bounded.
pub(crate) fn shown_path(path: &str) -> String {
    let mut path = super::grant_offer::escape_controls(path);
    if path.chars().count() > MAX_SHOWN_PATH_CHARS {
        path = path.chars().take(MAX_SHOWN_PATH_CHARS).collect::<String>() + "…";
    }
    path
}

/// Longest path shown in an explanation.
const MAX_SHOWN_PATH_CHARS: usize = 200;

/// What the session's read policy is built from, captured on the async
/// thread; building it reads folders, so that happens off it.
struct ReadPolicyInputs {
    codex_home: PathBuf,
    /// The database folder (#611).
    sqlite_home: PathBuf,
    turn_cwd: AbsolutePathBuf,
    workspace_roots: Vec<AbsolutePathBuf>,
    profile: PermissionProfile,
    user_home: Option<PathBuf>,
    /// #736, #758: agent commands cannot reach the macOS keychain services:
    /// on macOS, Aggressive's sandbox denies them after every other rule,
    /// also with a managed network proxy or an allowed Unix socket.
    keychain_closed: bool,
}

impl ReadPolicyInputs {
    /// Only under a live (or configured) Aggressive level, not when the
    /// policy is unreadable and Aggressive is only the fallback.
    fn capture(session: &Session, turn: &TurnContext) -> Option<Self> {
        Self::capture_at(session, turn, SecurityLevel::Aggressive)
    }

    /// Only while `level` is the effective level the protected-path rules
    /// apply under, not when the policy is unreadable.
    fn capture_at(session: &Session, turn: &TurnContext, level: SecurityLevel) -> Option<Self> {
        session
            .services
            .model_client()
            .post_taint_state()
            .filter(|state| {
                state.level == level && !matches!(state.policy, PolicyBinding::Unavailable)
            })?;
        // Local paths only: a remote command's paths are not this host's.
        if turn
            .environments
            .primary()
            .is_some_and(|environment| environment.environment.is_remote())
        {
            return None;
        }
        #[allow(deprecated)]
        let turn_cwd = turn.cwd.clone();
        let workspace_roots = turn
            .environments
            .primary()
            .map(|environment| {
                environment
                    .workspace_roots()
                    .iter()
                    .filter_map(|root| root.to_abs_path().ok())
                    .collect()
            })
            .unwrap_or_default();
        let profile = turn.config.permissions.permission_profile().clone();
        Some(Self {
            codex_home: turn.config.codex_home.to_path_buf(),
            sqlite_home: turn.config.sqlite.home().to_path_buf(),
            turn_cwd,
            workspace_roots,
            // #758: the denial is a rule of Corbanu's sandbox, which an
            // external sandbox does not take.
            keychain_closed: cfg!(target_os = "macos")
                && !matches!(profile, PermissionProfile::External { .. }),
            profile,
            user_home: dirs::home_dir(),
        })
    }

    /// The deny-read matcher of the session's read policy; `None` for a
    /// profile that cannot take the rules (an external sandbox).
    fn matcher(&self, cwd: &Path) -> Option<ReadDenyMatcher> {
        let mut denials = self.denials();
        // Captured only under Aggressive (see `capture`).
        denials.deny_home_listing();
        let policy = denials.apply(&self.profile)?.file_system_sandbox_policy();
        // A malformed pattern denies every path; explain nothing then.
        ReadDenyMatcher::try_new(&policy, cwd).ok().flatten()
    }

    /// The session's protected paths, as the read policy is built from them.
    fn denials(&self) -> ReadDenials {
        ReadDenials::for_turn(
            &self.codex_home,
            &self.sqlite_home,
            &self.turn_cwd,
            &self.workspace_roots,
            &self.profile,
        )
    }

    /// #620: whether `path` is one of the user's start-up files (`~/.zshrc`,
    /// login items; [`USER_PERSISTENCE`]), as spelled, through any link on
    /// the way or where it finally points, that the sandbox the command runs
    /// in cannot write where a write lands (the link's final target):
    /// outside its writable folders, so no grant could let it be written.
    fn unwritable_startup_file(&self, path: &Path) -> bool {
        let (Some(user_home), Ok(path)) = (
            self.user_home.as_deref(),
            AbsolutePathBuf::from_absolute_path(path),
        ) else {
            return false;
        };
        let mut spellings = link_chain(&path);
        spellings.extend(ReadDenials::real_location(&path));
        let mut homes = vec![user_home.to_path_buf()];
        homes.extend(std::fs::canonicalize(user_home).ok());
        let is_startup = |path: &AbsolutePathBuf| {
            homes.iter().any(|home| {
                USER_PERSISTENCE
                    .iter()
                    .any(|entry| path.as_path().starts_with(home.join(entry)))
            })
        };
        if !spellings.iter().any(is_startup) {
            return false;
        }
        let policy = self.base_policy();
        let effective = policy.effective_writes_with_cwd(self.turn_cwd.as_path());
        !self.base_writable(&policy, &effective, &path)
    }

    /// The project roots as the orchestrator runs the profile: the
    /// workspace roots (or the current folder), also by their real
    /// locations, as the sandbox resolves them.
    fn roots(&self) -> Vec<AbsolutePathBuf> {
        let mut roots = if self.workspace_roots.is_empty() {
            vec![self.turn_cwd.clone()]
        } else {
            self.workspace_roots.clone()
        };
        let real_roots: Vec<AbsolutePathBuf> = roots
            .iter()
            .filter_map(|root| std::fs::canonicalize(root.as_path()).ok())
            .filter_map(|root| AbsolutePathBuf::from_absolute_path(root).ok())
            .collect();
        roots.extend(real_roots);
        roots
    }

    /// The session's file policy without the protected-path rules (what a
    /// grant would leave). Write permissions granted for the turn or
    /// requested by the command are left out: Aggressive turns permission
    /// requests off and refuses additional writes (#245).
    fn base_policy(&self) -> FileSystemSandboxPolicy {
        self.profile
            .clone()
            .materialize_project_roots_with_workspace_roots(&self.roots())
            .file_system_sandbox_policy()
    }

    /// Whether `policy` writes where a write to `path` lands (a link's final
    /// target), also through a root spelled by an alias of its location
    /// (`/tmp` for `/private/tmp`), as the sandboxes match it (#619).
    /// `effective` is `policy`'s write checks in the turn's folder, built
    /// once for many paths (#907).
    fn base_writable(
        &self,
        policy: &FileSystemSandboxPolicy,
        effective: &EffectiveWrites<'_>,
        path: &AbsolutePathBuf,
    ) -> bool {
        let target = ReadDenials::real_location(path).unwrap_or_else(|| path.clone());
        policy.can_write_path_with_cwd(target.as_path(), self.turn_cwd.as_path())
            || effective.can_write_effective_path(target.as_path())
    }

    /// #740: whether a write to `path` lands inside one of `policy`'s
    /// writable folders (where it is not writable: a read-only part of the
    /// current folder), by any spelling of the folder.
    fn in_writable_folder(&self, policy: &FileSystemSandboxPolicy, path: &AbsolutePathBuf) -> bool {
        let target = ReadDenials::real_location(path).unwrap_or_else(|| path.clone());
        policy
            .get_writable_roots_with_cwd(self.turn_cwd.as_path())
            .into_iter()
            .flat_map(|root| {
                let real = std::fs::canonicalize(root.root.as_path()).ok();
                std::iter::once(root.root.to_path_buf()).chain(real)
            })
            .any(|root| target.as_path().starts_with(&root) || path.as_path().starts_with(&root))
    }

    /// #737, #740: for a command whose writes its text fully shows, when
    /// none of them could be lifted by a grant, why: its first write that
    /// no grant makes possible (outside the current folder, or in a
    /// read-only part of it such as the project's `.git`), or else that it
    /// writes nothing a grant lifts. `None` when a write is liftable (a hook
    /// in `.husky`), or may be (a program or path the text does not show:
    /// `npx husky install | tee ../log` keeps the offer).
    fn unliftable_note(&self, command: &[String], cwd: &Path) -> Option<ProtectedNote> {
        let mut writes = writes::writes(command);
        if !writes.understood {
            // #1182: `cd DIR && echo x >> ../notes.txt`. Only ever adds a
            // note: without it this command is not explained (and offered).
            writes = writes::writes_following_cd(command);
        }
        let denials = self.denials();
        let policy = self.base_policy();
        let effective = policy.effective_writes_with_cwd(self.turn_cwd.as_path());
        let mut understood = writes.understood;
        let mut first = None;
        for word in writes.targets {
            let Some(path) = resolve(&word, cwd, self.user_home.as_deref(), &self.codex_home)
                .and_then(|path| AbsolutePathBuf::from_absolute_path(path).ok())
            else {
                understood = false;
                continue;
            };
            // `/dev/null`, `/dev/stderr`: no file.
            if path.as_path().starts_with("/dev") {
                continue;
            }
            if self.base_writable(&policy, &effective, &path) {
                if lands_in(path.as_path(), denials.liftable_read_only_paths(), &[]) {
                    return None;
                }
                continue;
            }
            if first.is_none() {
                first = Some(if self.in_writable_folder(&policy, &path) {
                    ProtectedNote::ReadOnlyInFolder(word)
                } else {
                    ProtectedNote::OutsideFolder(word)
                });
            }
        }
        understood.then(|| first.unwrap_or(ProtectedNote::NothingToLift))
    }

    /// #740: whether a write to `path` is one no grant makes possible: the
    /// sandbox does not write there even without the protected-path rules.
    fn unliftable_write(&self, path: &Path) -> Option<bool> {
        let path = AbsolutePathBuf::from_absolute_path(path).ok()?;
        let policy = self.base_policy();
        let effective = policy.effective_writes_with_cwd(self.turn_cwd.as_path());
        if path.as_path().starts_with("/dev") || self.base_writable(&policy, &effective, &path) {
            return None;
        }
        Some(self.in_writable_folder(&policy, &path))
    }
}

/// #584: whether a write to `path` lands, as spelled, through a link on the
/// way or where it finally points, in one of `kept` (or one's real
/// location), and not in a folder of `writable` inside that kept path
/// (`worktrees` in the Corbanu home, #583), as the sandbox's most specific
/// entry wins: a kept path inside the writable folder (a worktree's own
/// `.codex`) still counts. A writable folder is also matched through the
/// kept path's spelling (the home reached through a link, whose `worktrees`
/// is known by its real location only).
fn lands_in<'a>(
    path: &Path,
    kept: impl Iterator<Item = &'a AbsolutePathBuf>,
    writable: &[&AbsolutePathBuf],
) -> bool {
    let Ok(path) = AbsolutePathBuf::from_absolute_path(path) else {
        return false;
    };
    let mut spellings = link_chain(&path);
    spellings.extend(ReadDenials::real_location(&path));
    let with_real = |paths: Vec<AbsolutePathBuf>| {
        let real: Vec<AbsolutePathBuf> = paths
            .iter()
            .filter_map(ReadDenials::real_location)
            .collect();
        paths.into_iter().chain(real).collect::<Vec<_>>()
    };
    let writable = with_real(writable.iter().map(|folder| (*folder).clone()).collect());
    let kept = with_real(kept.cloned().collect());
    kept.iter().any(|kept| {
        let real_kept = ReadDenials::real_location(kept).unwrap_or_else(|| kept.clone());
        // The writable folders inside this kept path, also spelled the way
        // it is.
        let inside: Vec<PathBuf> = writable
            .iter()
            .flat_map(|folder| {
                let as_spelled = folder
                    .as_path()
                    .strip_prefix(real_kept.as_path())
                    .ok()
                    .map(|rest| kept.as_path().join(rest));
                folder
                    .as_path()
                    .starts_with(kept.as_path())
                    .then(|| folder.to_path_buf())
                    .into_iter()
                    .chain(as_spelled)
            })
            .collect();
        spellings.iter().any(|spelling| {
            spelling.as_path().starts_with(kept.as_path())
                && !inside
                    .iter()
                    .any(|folder| spelling.as_path().starts_with(folder))
        })
    })
}

/// #584: the first word `command` writes to (see [`write_targets`]) that
/// lands in a path no grant lifts under Aggressive, when it writes no
/// read-only path a grant could lift (a git hook beside it: then Grant once
/// still applies to that, and is offered).
fn only_never_writable_word(
    command: &[String],
    cwd: &Path,
    user_home: Option<&Path>,
    codex_home: &Path,
    denials: &ReadDenials,
) -> Option<String> {
    let writable: Vec<&AbsolutePathBuf> = denials.home_writable_paths().collect();
    let mut found = None;
    for word in write_targets(command) {
        let Some(path) = resolve(&word, cwd, user_home, codex_home) else {
            continue;
        };
        if lands_in(&path, denials.never_lifted_paths(), &writable) {
            found.get_or_insert(word);
        } else if lands_in(&path, denials.liftable_read_only_paths(), &[]) {
            return None;
        }
    }
    found
}

/// Links followed from a file: `path` and each name it is a link to, as far
/// as [`MAX_LINK_HOPS`] (a link in a folder above a name is not followed).
fn link_chain(path: &AbsolutePathBuf) -> Vec<AbsolutePathBuf> {
    let mut chain = vec![path.clone()];
    while chain.len() <= MAX_LINK_HOPS {
        let last = chain[chain.len() - 1].clone();
        let Some(parent) = last.as_path().parent() else {
            break;
        };
        let Ok(target) = std::fs::read_link(last.as_path()) else {
            break;
        };
        chain.push(AbsolutePathBuf::resolve_path_against_base(target, parent));
    }
    chain
}

/// Links [`link_chain`] follows.
const MAX_LINK_HOPS: usize = 8;

async fn off_thread<T: Send + 'static>(
    work: impl FnOnce() -> Option<T> + Send + 'static,
) -> Option<T> {
    tokio::time::timeout(lookup_timeout(), tokio::task::spawn_blocking(work))
        .await
        .ok()?
        .ok()?
}

/// The explanation for a command that just finished with a known exit,
/// when an output line reports a denial of a protected path the command
/// names (or the command's folder is itself protected), or of a protected
/// path the line itself reports (#621: a path the command built at run
/// time, such as an interpreter's `os.listdir(...)`).
///
/// #1312: also when the exit is 0: a later command on the same line
/// (`echo x >> ../notes.txt; echo "exit=$?"`) can hide the denied one's
/// failure. Then only a path the command itself names, and a denial line
/// names too, is explained: output alone (a search printing old denial
/// lines) proves nothing was denied. The keychain note, which has no denial
/// line, needs a failed exit.
///
/// #622: a denial explained here is also listed under the inspector's
/// recent denials.
pub(crate) async fn denial_explanation(
    session: &Session,
    turn: &TurnContext,
    command: &[String],
    cwd: &Path,
    exit_code: Option<i32>,
    output: &str,
) -> Option<String> {
    let failed = exit_code? != 0;
    // #736: a keychain command fails without a file denial: the keychain
    // service is out of reach of the sandbox.
    if failed
        && keychain_command(command)
        && ReadPolicyInputs::capture(session, turn).is_some_and(|inputs| inputs.keychain_closed)
    {
        super::inspection::record_sandbox_denial(
            session.thread_id(),
            ProtectedNote::Keychain.inspected(),
        );
        return Some(KEYCHAIN_DENIAL.to_string());
    }
    let denial_lines: Vec<String> = output
        .lines()
        .filter(|line| is_denial_line(line, WINDOWS))
        .map(str::to_string)
        .collect();
    if denial_lines.is_empty() {
        return None;
    }
    // #748: Moderate explains the denials its own rules add.
    if let Some(inputs) = ReadPolicyInputs::capture_at(session, turn, SecurityLevel::Moderate) {
        // #839: listed under the inspector's recent denials, as under
        // Aggressive (#622).
        let (denied, explanation) =
            moderate::denial_explanation(inputs, command, cwd, denial_lines, failed).await?;
        let (what, outcome) = denied.inspected();
        super::inspection::record_refusal(session.thread_id(), what, outcome);
        return Some(explanation);
    }
    let inputs = ReadPolicyInputs::capture(session, turn)?;
    let (command, cwd) = (command.to_vec(), cwd.to_path_buf());
    let found = off_thread(move || {
        let matcher = inputs.matcher(&cwd)?;
        // #1312: the steps that go by the output or the folder alone, not
        // by a path the command names, need a failed command.
        if failed && matcher.is_read_denied(&cwd) {
            return Some(ProtectedNote::Read(cwd.display().to_string()));
        }
        let word = |denied: &dyn Fn(&Path) -> bool| {
            denied_word(
                &command,
                &cwd,
                inputs.user_home.as_deref(),
                &inputs.codex_home,
                denied,
                Some(&denial_lines),
                /*skip_patterns*/ !failed,
            )
        };
        let in_lines = |lines: &[String], accept: &dyn Fn(&Path) -> bool| {
            path_in_lines(
                &command,
                &cwd,
                inputs.user_home.as_deref(),
                &inputs.codex_home,
                lines,
                accept,
            )
        };
        word(&|path| matcher.is_read_denied(path))
            .map(ProtectedNote::Read)
            .or_else(|| word(&|path| matcher.is_listing_denied(path)).map(ProtectedNote::Listing))
            // #506: `ls` run in the Corbanu home itself (the denial names the
            // folder as `.` or by its name).
            .or_else(|| {
                let name = cwd.file_name()?.to_str()?.to_string();
                (failed
                    && matcher.is_listing_denied(&cwd)
                    && denial_lines.iter().any(|line| {
                        line.contains(&name) || line.contains(" .:") || line.contains("'.'")
                    }))
                .then(|| ProtectedNote::Listing(cwd.display().to_string()))
            })
            // #554: a folder above the home listed recursively (`find ~`,
            // `ls -aR ~`): the denial line names the home below it.
            .or_else(|| {
                if !failed {
                    return None;
                }
                unlistable_path_in_lines(
                    &command,
                    &cwd,
                    inputs.user_home.as_deref(),
                    &inputs.codex_home,
                    &denial_lines,
                    &|path| matcher.is_listing_denied(path) && !matcher.is_read_denied(path),
                )
                .map(ProtectedNote::Listing)
            })
            // #620: a start-up file outside the current folder.
            .or_else(|| {
                written_word(
                    &command,
                    &cwd,
                    inputs.user_home.as_deref(),
                    &inputs.codex_home,
                    &|path| inputs.unwritable_startup_file(path),
                    Some(&denial_lines),
                )
                .map(ProtectedNote::Startup)
            })
            // #584: a path no grant lifts.
            .or_else(|| {
                let denials = inputs.denials();
                let writable: Vec<&AbsolutePathBuf> = denials.home_writable_paths().collect();
                written_word(
                    &command,
                    &cwd,
                    inputs.user_home.as_deref(),
                    &inputs.codex_home,
                    &|path| lands_in(path, denials.never_lifted_paths(), &writable),
                    Some(&denial_lines),
                )
                .map(ProtectedNote::NeverWritable)
            })
            // #737, #740: a write no grant makes possible (outside the
            // current folder, the project's `.git`).
            .or_else(|| {
                if cfg!(windows) {
                    return None;
                }
                let (user_home, codex_home) = (inputs.user_home.as_deref(), &inputs.codex_home);
                // The denial line names the path as spelled or in full, not
                // only its file name (a short name matches too much).
                let named = |word: &str, path: &Path| {
                    let full = path.display().to_string();
                    denial_lines
                        .iter()
                        .any(|line| line.contains(word) || line.contains(&full))
                };
                let word = written_word(
                    &command,
                    &cwd,
                    user_home,
                    codex_home,
                    &|path| inputs.unliftable_write(path).is_some(),
                    Some(&denial_lines),
                )?;
                let path = resolve(&word, &cwd, user_home, codex_home)?;
                if !named(&word, &path) {
                    return None;
                }
                Some(if inputs.unliftable_write(&path)? {
                    ProtectedNote::ReadOnlyInFolder(word)
                } else {
                    ProtectedNote::OutsideFolder(word)
                })
            })
            // #621: a protected path the denial line reports that no word
            // of the command names (built at run time). Bounded: only the
            // first lines are looked at.
            .or_else(|| {
                if !failed {
                    return None;
                }
                let lines = &denial_lines[..denial_lines.len().min(MAX_REPORTED_LINES)];
                in_lines(lines, &|path| matcher.is_read_denied(path)).map(ProtectedNote::Read)
            })
    })
    .await?;
    super::inspection::record_sandbox_denial(session.thread_id(), found.inspected());
    Some(found.explanation())
}

/// Denial lines looked at for a path the command does not name (#621).
const MAX_REPORTED_LINES: usize = 32;

/// Folders a relative path in a denial line is resolved against, besides
/// the command's own.
const MAX_BASES: usize = 64;

/// #554: the path a denial line reports, as it spells it, that `unlistable`
/// accepts. A relative path is taken from the command's folder or from a
/// folder the command names (`cd ~ && find .`).
pub(super) fn unlistable_path_in_lines(
    command: &[String],
    cwd: &Path,
    user_home: Option<&Path>,
    codex_home: &Path,
    denial_lines: &[String],
    unlistable: &dyn Fn(&Path) -> bool,
) -> Option<String> {
    // Only the Corbanu home is unlistable: skip the other lines (`find ~`
    // can report many) without looking anything up.
    let name = codex_home.file_name()?.to_str()?;
    let lines: Vec<String> = denial_lines
        .iter()
        .filter(|line| line.contains(name))
        .cloned()
        .collect();
    path_in_lines(command, cwd, user_home, codex_home, &lines, unlistable)
}

/// The path one of `denial_lines` reports, as it spells it, that `accept`
/// accepts; relative paths as in [`unlistable_path_in_lines`].
fn path_in_lines(
    command: &[String],
    cwd: &Path,
    user_home: Option<&Path>,
    codex_home: &Path,
    denial_lines: &[String],
    accept: &dyn Fn(&Path) -> bool,
) -> Option<String> {
    if denial_lines.is_empty() {
        return None;
    }
    let mut bases = vec![cwd.to_path_buf()];
    bases.extend(
        command_words(command, WINDOWS)
            .into_iter()
            .map(|word| {
                word.trim_matches(|c: char| matches!(c, ';' | '|' | '&' | '(' | ')'))
                    .to_string()
            })
            .filter(|word| !word.is_empty() && !word.starts_with('-'))
            .filter_map(|word| resolve(&word, cwd, user_home, codex_home))
            .take(MAX_BASES),
    );
    denial_lines
        .iter()
        .flat_map(|line| paths_in_denial_line(line))
        .find(|spelled| {
            bases
                .iter()
                .any(|base| accept(&base.join(spelled.as_str())))
        })
}

/// The paths a denial line may name: quoted (`find: ‘/x’: Permission
/// denied`, `ls: cannot open directory '/x': …`), between the program and
/// the reason (`ls: /x: Operation not permitted`), or before the reason
/// when the program's name was split off (#1194).
fn paths_in_denial_line(line: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for (open, close) in [('‘', '’'), ('\'', '\''), ('"', '"'), ('`', '\'')] {
        let mut rest = line;
        while let Some(start) = rest.find(open) {
            let after = &rest[start + open.len_utf8()..];
            let Some(end) = after.find(close) else {
                break;
            };
            paths.push(after[..end].to_string());
            rest = &after[end + close.len_utf8()..];
        }
    }
    let fields: Vec<&str> = line.split(": ").collect();
    if fields.len() >= 3 {
        paths.push(fields[1..fields.len() - 1].join(": "));
    } else if fields.len() == 2 {
        // #1194: BSD `ls` and `find` write `ls: ` and `/x: Operation not
        // permitted` separately, and unified exec merges stdout and stderr
        // as they arrive, so output flushed in between can leave the line
        // without the program's name.
        paths.push(fields[0].to_string());
    }
    paths.retain(|path| !path.trim().is_empty());
    paths
}

/// #442: the protected path `command` names, under Aggressive: a grant for
/// it would never apply, so none is offered.
#[cfg(test)]
pub(crate) async fn protected_path_in_command(
    session: &Session,
    turn: &TurnContext,
    command: &[String],
    cwd: &Path,
) -> Option<String> {
    match protected_note_in_command(session, turn, command, cwd).await? {
        ProtectedNote::Read(path) => Some(path),
        _ => None,
    }
}

/// What a command names that no grant opens under Aggressive.
pub(crate) enum ProtectedNote {
    /// A protected path it reads (#442).
    Read(String),
    /// A folder it lists that may not be listed (#506).
    Listing(String),
    /// A start-up file outside the current folder, which Aggressive's
    /// sandbox never writes (#620).
    Startup(String),
    /// A path no grant lifts that it writes (#584: the project's `.codex`
    /// and `.agents`, #520; the Corbanu home).
    NeverWritable(String),
    /// A file outside the current folder that it writes (#740; #737: a
    /// config file such as `~/.npmrc`).
    OutsideFolder(String),
    /// A read-only part of the current folder that it writes, which no
    /// grant lifts (#740: the project's `.git`).
    ReadOnlyInFolder(String),
    /// Nothing a grant lifts, as far as its text shows (#740).
    NothingToLift,
    /// A macOS keychain command, which cannot reach the keychain service
    /// (#736).
    Keychain,
}

impl ProtectedNote {
    /// The explanation for the approval prompt, or where the command failed.
    pub(crate) fn explanation(&self) -> String {
        match self {
            Self::Read(path) => explanation(path),
            Self::Listing(path) => listing_explanation(path),
            Self::Startup(path) => startup_explanation(path),
            Self::NeverWritable(path) => never_writable_explanation(path),
            Self::OutsideFolder(path) => outside_folder_explanation(path),
            Self::ReadOnlyInFolder(path) => read_only_in_folder_explanation(path),
            Self::NothingToLift => NOTHING_TO_LIFT.to_string(),
            Self::Keychain => KEYCHAIN_PROMPT.to_string(),
        }
    }

    /// #622: what the inspector's recent denials say was refused (fixed
    /// text: the inspector never shows paths).
    fn inspected(&self) -> &'static str {
        match self {
            Self::Read(_) => "reading a protected file",
            Self::Listing(_) => "listing the Corbanu home",
            Self::Startup(_) => "writing a start-up file outside the current folder",
            Self::NeverWritable(_) => {
                "writing the project's .codex or .agents, or the Corbanu home"
            }
            Self::OutsideFolder(_) => "writing outside the current folder",
            Self::ReadOnlyInFolder(_) => "writing a read-only part of the current folder",
            // Never explained where a command failed.
            Self::NothingToLift => "a write no grant lifts",
            Self::Keychain => "using the macOS keychain",
        }
    }
}

/// The protected path `command` names (#442), or else the folder it lists
/// that may not be listed (#506), or else a start-up file outside the
/// current folder it names (#620), or else, when it writes no path a grant
/// lifts, a path no grant lifts that it writes (#584: the project's `.codex`
/// and `.agents`, the Corbanu home; #740: outside the current folder, the
/// project's `.git`), or else, when its text shows every write, that it
/// writes nothing a grant lifts (#740): no grant could apply to any of
/// them. On macOS a keychain command comes first (#736).
pub(crate) async fn protected_note_in_command(
    session: &Session,
    turn: &TurnContext,
    command: &[String],
    cwd: &Path,
) -> Option<ProtectedNote> {
    let inputs = ReadPolicyInputs::capture(session, turn)?;
    if inputs.keychain_closed && keychain_command(command) {
        return Some(ProtectedNote::Keychain);
    }
    let (command, cwd) = (command.to_vec(), cwd.to_path_buf());
    off_thread(move || {
        let matcher = inputs.matcher(&cwd)?;
        if matcher.is_read_denied(&cwd) {
            return Some(ProtectedNote::Read(cwd.display().to_string()));
        }
        let word = |denied: &dyn Fn(&Path) -> bool| {
            denied_word(
                &command,
                &cwd,
                inputs.user_home.as_deref(),
                &inputs.codex_home,
                denied,
                /*denial_lines*/ None,
                /*skip_patterns*/ false,
            )
        };
        word(&|path| matcher.is_read_denied(path))
            .map(ProtectedNote::Read)
            // #554: Windows does not enforce the listing denial (its sandbox
            // ignores the entry), so it says nothing about listings there.
            .or_else(|| {
                (!cfg!(windows))
                    .then(|| word(&|path| matcher.is_listing_denied(path)))
                    .flatten()
                    .map(ProtectedNote::Listing)
            })
            .or_else(|| {
                written_word(
                    &command,
                    &cwd,
                    inputs.user_home.as_deref(),
                    &inputs.codex_home,
                    &|path| inputs.unwritable_startup_file(path),
                    /*denial_lines*/ None,
                )
                .map(ProtectedNote::Startup)
            })
            // Windows drops the read-only entry of a missing path, so a
            // missing `.codex` may well be written there (#554 likewise).
            .or_else(|| {
                (!cfg!(windows))
                    .then(|| {
                        only_never_writable_word(
                            &command,
                            &cwd,
                            inputs.user_home.as_deref(),
                            &inputs.codex_home,
                            &inputs.denials(),
                        )
                    })
                    .flatten()
                    .map(ProtectedNote::NeverWritable)
            })
            // #737, #740 (Windows enforces writes differently, #554).
            .or_else(|| {
                (!cfg!(windows))
                    .then(|| inputs.unliftable_note(&command, &cwd))
                    .flatten()
            })
    })
    .await
}

/// #841, #1077: under Moderate with the secretless launch contract, what a
/// person is told before approving a protected action that reads a file the
/// contract keeps unreadable at every level (a CLI token file, the keychains,
/// the Google Cloud CLI's folder on Windows, or Corbanu's own files such as
/// its sign-in, vault, config or history): the file, what it is, and that
/// approving runs the command but cannot unlock it.
pub(crate) fn moderate_kept_read_note(path: &str, kept: KeptFile) -> String {
    let what = match kept {
        KeptFile::CliToken => "is a CLI token file",
        KeptFile::GoogleCloudCliFolder => {
            "is the Google Cloud CLI's folder (credentials and config)"
        }
        KeptFile::InGoogleCloudCliFolder => {
            "is in the Google Cloud CLI's folder (credentials and config)"
        }
        KeptFile::Keychain => "is part of your keychains",
        KeptFile::CorbanuOwn => {
            "is one of Corbanu's own files (such as its sign-in, vault, config or history)"
        }
    };
    format!(
        "`{}` {what}: secretless agent launch keeps it unreadable to agent commands at every \
         security level, even if you approve, so approving cannot unlock it. If you approve, \
         the command runs, but that read fails.",
        shown_path(path)
    )
}

/// #841: under Moderate with the secretless launch contract, the note for
/// `command` when it names a path the contract keeps unreadable at every
/// level ([`moderate_kept_read_note`]).
pub(crate) async fn moderate_kept_read_note_for_command(
    session: &Session,
    turn: &TurnContext,
    command: &[String],
    cwd: &Path,
) -> Option<String> {
    let contract = super::launch_contract::active()?;
    let inputs = ReadPolicyInputs::capture_at(session, turn, SecurityLevel::Moderate)?;
    let (command, cwd) = (command.to_vec(), cwd.to_path_buf());
    off_thread(move || kept_read_note(&inputs, contract, &command, &cwd)).await
}

/// See [`moderate_kept_read_note_for_command`]: the first word naming a kept
/// path, as the command spelled it.
fn kept_read_note(
    inputs: &ReadPolicyInputs,
    contract: &super::launch_contract::LaunchContract,
    command: &[String],
    cwd: &Path,
) -> Option<String> {
    let found = std::cell::Cell::new(None);
    let word = denied_word(
        command,
        cwd,
        inputs.user_home.as_deref(),
        &inputs.codex_home,
        |path| {
            let kept = contract.kept_file(path, cwd);
            if kept.is_some() {
                found.set(kept);
            }
            kept.is_some()
        },
        /*denial_lines*/ None,
        /*skip_patterns*/ false,
    )?;
    Some(moderate_kept_read_note(&word, found.get()?))
}

/// Issue #245: under Aggressive, the explanation for a request whose own
/// additional permissions would open a protected path. Checked before the
/// request is validated, so it is refused with this explanation even where
/// permission requests are off (as Aggressive keeps them).
pub(crate) async fn additional_permission_refusal(
    session: &Session,
    turn: &TurnContext,
    additional: Option<&codex_protocol::models::AdditionalPermissionProfile>,
    cwd: &Path,
) -> Option<String> {
    let additional = additional?.clone();
    let inputs = ReadPolicyInputs::capture(session, turn)?;
    // An external sandbox takes no protected-path rules (and ignores
    // additional permissions), as in the orchestrator's check.
    if matches!(
        inputs.profile,
        codex_protocol::models::PermissionProfile::External { .. }
    ) {
        return None;
    }
    let cwd = cwd.to_path_buf();
    off_thread(move || {
        let denials = ReadDenials::for_turn(
            &inputs.codex_home,
            &inputs.sqlite_home,
            &inputs.turn_cwd,
            &inputs.workspace_roots,
            &inputs.profile,
        );
        let mut cwds = vec![cwd.as_path(), inputs.turn_cwd.as_path()];
        cwds.extend(inputs.workspace_roots.iter().map(AbsolutePathBuf::as_path));
        // Issue #513: nor a path no grant lifts for writing (#565: the
        // runtime entries of the Corbanu home and the project's `.codex` and
        // `.agents`). Other write protections wait for the run's grant to
        // be known.
        let protected = denials
            .apply(&inputs.profile)
            .map(|protected| protected.file_system_sandbox_policy());
        let check = super::protected_additional::Check {
            denials: &denials,
            protected: protected.as_ref(),
            reads: true,
            writes: super::protected_additional::ReadOnlyScope::NeverLifted,
        };
        super::protected_additional::opened(check, &additional, &cwds)
            .map(|opened| opened.refusal(SecurityLevel::Aggressive))
    })
    .await
}

/// #622: lists a command refused for additional permissions that would
/// open a protected path ([`additional_permission_refusal`]) under the
/// inspector's recent denials.
pub(crate) fn record_permission_refusal(session: &Session) {
    super::inspection::record_refusal(
        session.thread_id(),
        "additional permissions naming a protected path",
        "refused before it ran; never granted under Aggressive",
    );
}

/// #1087: the explanation for an in-process file tool's read of `path` (the
/// image tool's) that the protected-path rules refused where the tool's own
/// permissions allow it, or that the secretless launch contract keeps
/// unreadable where the person's own permissions allow it: under Moderate
/// or Aggressive, it names the level as a refused command does (#442, #748;
/// a path the contract keeps is attributed to it under Moderate, as there),
/// and is listed under the inspector's recent denials (#622, #839). `own` is
/// the tool's sandbox context before the rules (the contract's denials
/// included), `protected` after them. Explanation only.
pub(crate) async fn file_tool_denial_explanation(
    session: &Session,
    turn: &TurnContext,
    own: &codex_file_system::FileSystemSandboxContext,
    protected: &codex_file_system::FileSystemSandboxContext,
    path: &Path,
) -> Option<String> {
    let (level, inputs) = ReadPolicyInputs::capture_at(session, turn, SecurityLevel::Moderate)
        .map(|inputs| (SecurityLevel::Moderate, inputs))
        .or_else(|| {
            ReadPolicyInputs::capture(session, turn)
                .map(|inputs| (SecurityLevel::Aggressive, inputs))
        })?;
    let policy = |context: &codex_file_system::FileSystemSandboxContext| {
        PermissionProfile::try_from(context.permissions.clone())
            .ok()
            .map(|profile| profile.file_system_sandbox_policy())
    };
    let (own_policy, protected_policy) = (policy(own)?, policy(protected)?);
    let cwd = own
        .cwd
        .as_ref()
        .and_then(|cwd| cwd.to_abs_path().ok())
        .unwrap_or_else(|| inputs.turn_cwd.clone());
    let contract = super::launch_contract::active();
    let path = path.to_path_buf();
    let found = off_thread(move || {
        explain_file_tool_denial(
            level,
            &inputs,
            contract,
            &own_policy,
            &protected_policy,
            cwd.as_path(),
            &path,
        )
    })
    .await;
    let (what, outcome, explanation) = found?;
    match outcome {
        Some(outcome) => super::inspection::record_refusal(session.thread_id(), what, outcome),
        None => super::inspection::record_sandbox_denial(session.thread_id(), what),
    }
    Some(explanation)
}

/// See [`file_tool_denial_explanation`]: what was denied (for the
/// inspector, with `None` for Aggressive's sandbox-denial outcome) and the
/// explanation.
fn explain_file_tool_denial(
    level: SecurityLevel,
    inputs: &ReadPolicyInputs,
    contract: Option<&super::launch_contract::LaunchContract>,
    own_policy: &FileSystemSandboxPolicy,
    protected_policy: &FileSystemSandboxPolicy,
    cwd: &Path,
    path: &Path,
) -> Option<(&'static str, Option<&'static str>, String)> {
    let readable = |policy: &FileSystemSandboxPolicy| {
        super::protected_surface::readable_under(policy, path, cwd)
    };
    // The person's own permissions, without the contract or the rules.
    let person = moderate::own_profile(inputs).file_system_sandbox_policy();
    let kept =
        contract.is_some_and(|contract| contract.keeps_unreadable(path, cwd)) && readable(&person);
    let refused = !readable(protected_policy) && readable(own_policy);
    if !kept && !refused {
        return None;
    }
    let shown = path.display().to_string();
    Some(if level == SecurityLevel::Aggressive {
        let note = ProtectedNote::Read(shown);
        (note.inspected(), None, note.explanation())
    } else if kept {
        let (what, outcome) = moderate::Denied::KeptRead.inspected();
        (what, Some(outcome), moderate::kept_read_explanation(&shown))
    } else if ReadDenials::is_security_file(&inputs.codex_home, inputs.user_home.as_deref(), path) {
        let (what, outcome) = moderate::Denied::SecurityFile.inspected();
        (
            what,
            Some(outcome),
            moderate::security_file_explanation(&shown, moderate::WayForward::Permissive),
        )
    } else {
        let (what, outcome) = moderate::Denied::Read.inspected();
        (
            what,
            Some(outcome),
            moderate::read_explanation(&shown, moderate::WayForward::Permissive),
        )
    })
}

/// Shows `explanation` to the person as a warning beside the failed command.
pub(crate) async fn notify(session: &Session, turn: &TurnContext, explanation: String) {
    session
        .send_event(
            turn,
            EventMsg::Warning(WarningEvent {
                message: explanation,
            }),
        )
        .await;
}

/// The first word of `command` (shell scripts split into words) that names a
/// path `denied` reports as unreadable, as the command spelled it. With
/// `denial_lines`, only a word whose file name one of those lines mentions.
/// #1323: with `skip_patterns` (the command exited 0), not a search pattern
/// (`rg ".env" logs/` can print an old denial line naming `.env`).
pub(super) fn denied_word(
    command: &[String],
    cwd: &Path,
    user_home: Option<&Path>,
    codex_home: &Path,
    denied: impl Fn(&Path) -> bool,
    denial_lines: Option<&[String]>,
    skip_patterns: bool,
) -> Option<String> {
    let words = command_words(command, WINDOWS);
    let patterns = if skip_patterns {
        search_patterns::search_pattern_indices(command, &words, WINDOWS)
    } else {
        Default::default()
    };
    words
        .into_iter()
        .enumerate()
        .filter(|(index, _)| !patterns.contains(index))
        .flat_map(|(_, whole)| {
            // `--file=.env`, `<.env`, `.env;`: the path part of the word.
            let word = whole.trim_matches(|c: char| matches!(c, ';' | '|' | '&' | '(' | ')'));
            let word = word.trim_start_matches(['<', '>']);
            let mut parts = vec![word.to_string()];
            if let Some((_, value)) = word.split_once('=') {
                parts.push(value.to_string());
            }
            // #621: paths inside code an interpreter runs (`python3 -c`,
            // `node -e`, `perl -e`, `ruby -e`).
            parts.extend(embedded_paths(&whole));
            parts
        })
        .filter(|word| !word.is_empty() && !word.starts_with('-'))
        .find(|word| {
            let Some(path) = resolve(word, cwd, user_home, codex_home) else {
                return false;
            };
            // As spelled, or as resolved (`$CODEX_HOME`, `dir/..`; #506).
            let resolved = AbsolutePathBuf::from_absolute_path(&path)
                .ok()
                .and_then(|path| {
                    path.as_path()
                        .file_name()
                        .map(std::ffi::OsStr::to_os_string)
                });
            let names = [
                Path::new(word)
                    .file_name()
                    .map(std::ffi::OsStr::to_os_string),
                resolved,
            ];
            denial_lines.is_none_or(|lines| {
                names
                    .iter()
                    .flatten()
                    .filter_map(|name| name.to_str())
                    .any(|name| {
                        lines
                            .iter()
                            .any(|line| line.contains(name) || names_no_path(line, WINDOWS))
                    })
            }) && denied(path.as_path())
        })
}

/// #620: the first word of `command` that it writes to (see
/// [`write_targets`]) and `accept` accepts, as the command spelled it. With
/// `denial_lines`, only a word whose file name one of those lines mentions.
fn written_word(
    command: &[String],
    cwd: &Path,
    user_home: Option<&Path>,
    codex_home: &Path,
    accept: &dyn Fn(&Path) -> bool,
    denial_lines: Option<&[String]>,
) -> Option<String> {
    write_targets(command).into_iter().find(|word| {
        let Some(path) = resolve(word, cwd, user_home, codex_home) else {
            return false;
        };
        denial_lines.is_none_or(|lines| {
            Path::new(word)
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|name| lines.iter().any(|line| line.contains(name)))
        }) && accept(&path)
    })
}

/// Characters that end a path inside code.
const CODE_DELIMITERS: [char; 16] = [
    '\'', '"', '`', '(', ')', '[', ']', '{', '}', ',', ';', '+', ' ', '\t', '\n', '\r',
];

/// Paths looked at inside one word that is code.
const MAX_EMBEDDED_PATHS: usize = 64;

/// #621: the path literals in `word` when it may be code rather than a path
/// (any word with quotes, brackets, separators or spaces, such as
/// `import os; print(os.listdir('/x'))`): the quoted strings, and the tokens
/// that start like a path (`/`, `~`, `$`). Text in a message counts too: a
/// protected path it names withholds the grant (reads are never granted
/// anyway). Bounded; a path built at run time is not found here, and is
/// explained where the sandbox denies it instead.
fn embedded_paths(word: &str) -> Vec<String> {
    if !word.contains(CODE_DELIMITERS) {
        return Vec::new();
    }
    let mut found: Vec<String> = Vec::new();
    let quoted = ['\'', '"', '`'].into_iter().flat_map(|quote| {
        word.split(quote)
            .skip(1)
            .step_by(2)
            .map(str::to_string)
            .collect::<Vec<_>>()
    });
    let tokens = word
        .split(CODE_DELIMITERS)
        .filter(|token| token.starts_with(['/', '~', '$']))
        .map(str::to_string);
    for path in quoted.chain(tokens) {
        if found.len() == MAX_EMBEDDED_PATHS {
            break;
        }
        let path = path.trim().to_string();
        if !path.is_empty() && path != word && !found.contains(&path) {
            found.push(path);
        }
    }
    found
}

fn resolve(word: &str, cwd: &Path, user_home: Option<&Path>, codex_home: &Path) -> Option<PathBuf> {
    expand(word, cwd, user_home, codex_home, WINDOWS).map(|expanded| cwd.join(expanded))
}

/// `word` with the home variables and `~` expanded; `None` when another
/// variable is left. #1296: `windows` adds PowerShell's and cmd's spellings
/// (`$env:USERPROFILE`, `${env:CODEX_HOME}`, `%USERPROFILE%`, in any letter
/// case) and `~\`.
fn expand(
    word: &str,
    cwd: &Path,
    user_home: Option<&Path>,
    codex_home: &Path,
    windows: bool,
) -> Option<String> {
    let codex_home = codex_home.to_string_lossy();
    let mut expanded = word.to_string();
    if windows {
        let user_home = user_home.map(Path::to_string_lossy);
        expanded = super::tainted_action::windows_paths::expand(
            &expanded,
            &super::tainted_action::windows_paths::Values {
                codex_home: &codex_home,
                user_home: user_home.as_deref(),
                cwd: &cwd.to_string_lossy(),
                places: false,
            },
        );
    }
    for name in ["CODEX_HOME", "CORBANU_HOME", "PFTERMINAL_HOME"] {
        for spelling in [format!("${{{name}}}"), format!("${name}")] {
            expanded = expanded.replace(&spelling, &codex_home);
        }
    }
    if let Some(home) = user_home.map(Path::to_string_lossy) {
        for spelling in ["${HOME}", "$HOME"] {
            expanded = expanded.replace(spelling, &home);
        }
        if expanded == "~" {
            expanded = home.into_owned();
        } else if let Some(rest) = expanded.strip_prefix("~/") {
            expanded = format!("{home}/{rest}");
        } else if let Some(rest) = expanded.strip_prefix("~\\").filter(|_| windows) {
            expanded = format!("{home}\\{rest}");
        }
    }
    if expanded.contains('$') || expanded.starts_with('~') {
        return None;
    }
    Some(expanded)
}

mod moderate;

mod search_patterns;

#[cfg(test)]
#[path = "protected_reads_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "protected_reads_writes_tests.rs"]
mod writes_tests;
