//! #748: under security level Moderate, a command the sandbox stopped because
//! of Moderate's protected-path rules says so where it failed, to the person
//! (a warning) and to the model (a note on the output), as #442 does under
//! Aggressive. Without it both saw only "Operation not permitted".
//!
//! Only a denial the rules add counts: the path is one they protect, and the
//! person's own permissions (the profile without the rules) allow the access.
//! A write outside the project, a network failure or the person's own denial
//! is not Moderate's and is not attributed to it. A path the secretless
//! launch contract also keeps at every level is attributed to that contract
//! (a person's approval may have lifted Moderate's rules for the command).
//! Otherwise the way forward is switching to Permissive, unless the person's
//! own full access runs commands outside a sandbox, which the contract
//! refuses at every level. Nothing is said about approvals lifting the rules
//! (#747), except that Corbanu's security state and keys stay denied even
//! then (#838). #1139 (D94): one denial the person's own sandbox makes is
//! Moderate's too: a write into a writable folder's `.git`, which only a run
//! outside the sandbox could make and Moderate never allows one (#747).
//! Explanation only: it never changes what runs or what is denied.

use std::cell::RefCell;
use std::path::Path;
use std::path::PathBuf;

use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxKind;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::ReadDenyMatcher;
use codex_utils_absolute_path::AbsolutePathBuf;

use super::MAX_REPORTED_LINES;
use super::ReadDenials;
use super::ReadPolicyInputs;
use crate::security::launch_contract::LaunchContract;

/// How a person could allow what Moderate's rules denied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WayForward {
    /// Switching to Permissive.
    Permissive,
    /// Nothing at any level: the person's own permissions run commands
    /// outside a sandbox, which the secretless launch contract refuses.
    NoLevel,
}

impl WayForward {
    fn text(self) -> &'static str {
        match self {
            Self::Permissive => {
                "To allow it, a person can switch to Permissive in /security (it applies after a \
                 restart)."
            }
            Self::NoLevel => {
                "Secretless agent launch runs agent commands only inside a sandbox at every \
                 security level, so changing the level would not allow it."
            }
        }
    }
}

/// The explanation for a read of `path` that Moderate's rules denied.
pub(crate) fn read_explanation(path: &str, way: WayForward) -> String {
    format!(
        "Security level Moderate blocked access to `{}`: agent commands can't read credential \
         files in your home folder (such as SSH keys, cloud and CLI tokens, and keychains) or \
         Corbanu's own files (its sign-in, vault, config and history). {}",
        super::shown_path(path),
        way.text()
    )
}

/// #838: the explanation for a read of Corbanu's security state or keys,
/// which Moderate keeps from agent commands even when a person approves one.
pub(crate) fn security_file_explanation(path: &str, way: WayForward) -> String {
    format!(
        "Security level Moderate blocked access to `{}`: agent commands can't read or change \
         Corbanu's security state and keys, even when you approve them. {}",
        super::shown_path(path),
        way.text()
    )
}

/// The explanation for a write to `path` that Moderate's rules denied.
pub(crate) fn write_explanation(path: &str, way: WayForward) -> String {
    format!(
        "Security level Moderate blocked writing `{}`: agent commands can't change files that \
         run code or set policy later (shell start-up files, git hooks and config, the \
         project's .codex and .agents) or Corbanu's own files in its home. {}",
        super::shown_path(path),
        way.text()
    )
}

/// The explanation for a read of `path` the secretless launch contract
/// denies at every level. It is not attributed to Moderate alone: a
/// person's approval may have lifted Moderate's rules for the command.
pub(crate) fn kept_read_explanation(path: &str) -> String {
    format!(
        "Secretless agent launch blocked access to `{}`: at every security level, Moderate \
         included, agent commands can't read Corbanu's own files (such as its sign-in, vault, \
         config and history) or common CLI tokens, even when you approve them, so changing the \
         level would not allow it.",
        super::shown_path(path)
    )
}

/// The explanation for a write to `path` the secretless launch contract
/// keeps read-only at every level (see [`kept_read_explanation`]).
pub(crate) fn kept_write_explanation(path: &str) -> String {
    format!(
        "Secretless agent launch blocked writing `{}`: at every security level, Moderate \
         included, agent commands can't change Corbanu's home or the credential files it \
         protects, even when you approve them, so changing the level would not allow it.",
        super::shown_path(path)
    )
}

/// What an explained denial was, for the inspector's recent denials (#839).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Denied {
    /// A read Moderate's rules denied.
    Read,
    /// A write Moderate's rules denied.
    Write,
    /// A read the secretless launch contract keeps at every level.
    KeptRead,
    /// A read of Corbanu's security state or keys, which Moderate keeps
    /// even after an approval (#838).
    SecurityFile,
    /// A write the secretless launch contract keeps at every level.
    KeptWrite,
    /// A write into the project's `.git` folder (#1139).
    GitWrite,
    /// The same, where secretless agent launch keeps every command in the
    /// sandbox at every level.
    KeptGitWrite,
}

impl Denied {
    /// What was refused and how, in the inspector's fixed text.
    pub(super) fn inspected(self) -> (&'static str, &'static str) {
        const MODERATE: &str = "denied by the sandbox under Moderate";
        const KEPT: &str = "denied by the sandbox; secretless agent launch keeps it at every level, even when you approve";
        match self {
            Self::Read => ("reading a protected file", MODERATE),
            Self::Write => (
                "writing a file that runs code later, or Corbanu's own files",
                MODERATE,
            ),
            Self::KeptRead => ("reading Corbanu's own files or CLI tokens", KEPT),
            Self::SecurityFile => (
                "reading Corbanu's security state or keys",
                "denied by the sandbox under Moderate, even when you approve",
            ),
            Self::KeptWrite => (
                "writing Corbanu's home or a protected credential file",
                KEPT,
            ),
            Self::GitWrite => (
                "writing the project's .git folder (git hooks and config)",
                "denied by the sandbox under Moderate, even when you approve",
            ),
            Self::KeptGitWrite => (
                "writing the project's .git folder (git hooks and config)",
                KEPT,
            ),
        }
    }
}

/// The explanation for a command that printed `denial_lines`, when
/// Moderate's rules denied a path it names (or reports) and the person's
/// own permissions would have allowed it, and what was denied. #1312: a
/// command that exited 0 (`failed` false) is explained only by a path it
/// names itself, not by its folder or the output alone.
pub(super) async fn denial_explanation(
    inputs: ReadPolicyInputs,
    command: &[String],
    cwd: &Path,
    denial_lines: Vec<String>,
    failed: bool,
) -> Option<(Denied, String)> {
    let contract = crate::security::launch_contract::active();
    let (command, cwd) = (command.to_vec(), cwd.to_path_buf());
    super::off_thread(move || {
        explain_denied(&inputs, contract, &command, &cwd, &denial_lines, failed)
    })
    .await
}

#[cfg(all(test, unix))]
fn explain(
    inputs: &ReadPolicyInputs,
    contract: Option<&LaunchContract>,
    command: &[String],
    cwd: &Path,
    denial_lines: &[String],
) -> Option<String> {
    explain_denied(
        inputs,
        contract,
        command,
        cwd,
        denial_lines,
        /*failed*/ true,
    )
    .map(|(_, text)| text)
}

fn explain_denied(
    inputs: &ReadPolicyInputs,
    contract: Option<&LaunchContract>,
    command: &[String],
    cwd: &Path,
    denial_lines: &[String],
    failed: bool,
) -> Option<(Denied, String)> {
    let own_profile = own_profile(inputs);
    let base = own_profile.file_system_sandbox_policy();
    let denials = inputs.denials();
    // A malformed pattern denies every path; explain nothing then.
    let own = ReadDenyMatcher::try_new(&base, cwd).ok()?;
    let protected = denials.apply(&inputs.profile)?.file_system_sandbox_policy();
    let protected = ReadDenyMatcher::try_new(&protected, cwd).ok().flatten()?;
    let (user_home, codex_home) = (inputs.user_home.as_deref(), inputs.codex_home.as_path());
    // The secretless launch contract refuses a launch outside a sandbox at
    // every level, so with the person's own full access no level change
    // lets the command run at all.
    let unsandboxed = base.kind != FileSystemSandboxKind::Restricted;
    let way = if contract.is_some() && unsandboxed {
        WayForward::NoLevel
    } else {
        WayForward::Permissive
    };
    // What the contract narrows: the command's sandbox without Moderate's
    // rules; with full access, full write, as Moderate's rules leave it.
    let launched = if unsandboxed {
        FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry::new(
            FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            FileSystemAccessMode::Write,
        )])
    } else {
        base.clone()
    };
    // The path the last accepted word or line names.
    let found: RefCell<Option<PathBuf>> = RefCell::new(None);

    let read = |path: &Path| {
        let blocked = protected.is_read_denied(path)
            && base.can_read_path_with_cwd(path, cwd)
            && !own.as_ref().is_some_and(|own| own.is_read_denied(path));
        if blocked {
            found.replace(Some(path.to_path_buf()));
        }
        blocked
    };
    // #1312: the steps that go by the output or the folder alone need a
    // failed command.
    let lines = if failed {
        &denial_lines[..denial_lines.len().min(MAX_REPORTED_LINES)]
    } else {
        &[]
    };
    let read_word = (failed && read(cwd))
        .then(|| cwd.display().to_string())
        .or_else(|| {
            super::denied_word(
                command,
                cwd,
                user_home,
                codex_home,
                read,
                Some(denial_lines),
                /*skip_patterns*/ !failed,
            )
        })
        // #621: a path built at run time that the denial line reports.
        .or_else(|| super::path_in_lines(command, cwd, user_home, codex_home, lines, &read));
    if let Some(word) = read_word {
        let path = found.take();
        let kept = path
            .as_ref()
            .zip(contract)
            .is_some_and(|(path, contract)| contract.keeps_unreadable(path, cwd));
        let security = path
            .as_ref()
            .is_some_and(|path| ReadDenials::is_security_file(codex_home, user_home, path));
        return Some(if kept {
            (Denied::KeptRead, kept_read_explanation(&word))
        } else if security {
            (Denied::SecurityFile, security_file_explanation(&word, way))
        } else {
            (Denied::Read, read_explanation(&word, way))
        });
    }

    let writable: Vec<&AbsolutePathBuf> = denials.home_writable_paths().collect();
    let write = |path: &Path| {
        // A write lands where a link finally points.
        let target = AbsolutePathBuf::from_absolute_path(path)
            .ok()
            .and_then(|path| ReadDenials::real_location(&path))
            .map_or_else(|| path.to_path_buf(), AbsolutePathBuf::into_path_buf);
        let blocked = super::lands_in(path, denials.read_only_paths(), &writable)
            && base.can_write_path_with_cwd(&target, cwd);
        if blocked {
            found.replace(Some(target));
        }
        blocked
    };
    if let Some(word) = super::written_word(
        command,
        cwd,
        user_home,
        codex_home,
        &write,
        Some(denial_lines),
    ) {
        let kept = found
            .take()
            .zip(contract)
            .is_some_and(|(path, contract)| contract.keeps_read_only(&launched, &path, cwd));
        return Some(if kept {
            (Denied::KeptWrite, kept_write_explanation(&word))
        } else {
            (Denied::Write, write_explanation(&word, way))
        });
    }

    // #1139 (D94): a write into a project's `.git` folder. The person's own
    // sandbox keeps it read-only inside a writable folder; under Moderate no
    // approval runs a command outside the sandbox (#747, D3), so Moderate is
    // why it stays read-only. Explanation only: nothing else changes.
    // A relative path is resolved against the command's folder, which is
    // not where it wrote if it changed folder (`cd`, `git -C`, ...): then
    // only an absolute path is attributed.
    let relative_ok = !changes_folder(command);
    let git_write = |path: &Path| git_folder_write(&base, path, cwd);
    let word = super::written_word(
        command,
        cwd,
        user_home,
        codex_home,
        &git_write,
        Some(denial_lines),
    )
    .filter(|word| relative_ok || Path::new(word).is_absolute())
    .or_else(|| git_path_in_lines(cwd, lines, relative_ok, &git_write))?;
    Some(if contract.is_some() {
        (Denied::KeptGitWrite, kept_git_write_explanation(&word))
    } else {
        (Denied::GitWrite, git_write_explanation(&word))
    })
}

/// Whether `path` lies in a `.git` folder that `base` keeps read-only while
/// it can write the folder holding it (a writable folder's protected `.git`).
fn git_folder_write(base: &FileSystemSandboxPolicy, path: &Path, cwd: &Path) -> bool {
    let mut git_folder = None;
    for ancestor in path.ancestors() {
        if ancestor.file_name() == Some(std::ffi::OsStr::new(".git")) {
            git_folder = Some(ancestor);
        }
    }
    let Some(parent) = git_folder.and_then(Path::parent) else {
        return false;
    };
    !base.can_write_path_with_cwd(path, cwd) && base.can_write_path_with_cwd(parent, cwd)
}

/// #1139: a path inside a `.git` folder that a denial line reports and
/// `accept` takes (`error: could not lock config file .git/config: Operation
/// not permitted`, `bash: .git/hooks/pre-commit: Operation not permitted`),
/// as the line spells it.
fn git_path_in_lines(
    cwd: &Path,
    lines: &[String],
    relative_ok: bool,
    accept: &dyn Fn(&Path) -> bool,
) -> Option<String> {
    lines
        .iter()
        .flat_map(|line| line.split_whitespace())
        .map(|token| {
            token
                .trim_matches(|c: char| matches!(c, '\'' | '"' | '`' | '‘' | '’' | ':' | ',' | ';'))
        })
        .filter(|token| {
            let path = Path::new(token);
            (relative_ok || path.is_absolute())
                && path.components().any(|c| c.as_os_str() == ".git")
        })
        .find(|token| accept(&cwd.join(token)))
        .map(str::to_string)
}

/// #1139 review: whether `command` may run in, or point git at, another
/// folder than its own (`cd`, `pushd`, `git -C`, `--git-dir`, `--work-tree`,
/// `GIT_DIR=`, `GIT_WORK_TREE=`), so a relative path it reports may not be
/// relative to its folder.
fn changes_folder(command: &[String]) -> bool {
    command.iter().any(|argument| {
        argument
            .split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '`'))
            .any(|word| {
                matches!(word, "cd" | "pushd" | "popd" | "-C")
                    || word.starts_with("--git-dir")
                    || word.starts_with("--work-tree")
                    || word.starts_with("GIT_DIR=")
                    || word.starts_with("GIT_WORK_TREE=")
            })
    })
}

/// #1139 (D94): the explanation for a write into the project's `.git`
/// folder, which Moderate keeps read-only.
pub(crate) fn git_write_explanation(path: &str) -> String {
    format!(
        "Security level Moderate blocked writing `{}`: under Moderate, agent commands always run \
         inside the sandbox, even when you approve them, and the sandbox keeps the project's .git \
         folder (git hooks and config) read-only. To make this change, a person can run the \
         command in their own terminal, or switch to Permissive in /security (it applies after a \
         restart), where an approved command can run outside the sandbox.",
        super::shown_path(path)
    )
}

/// #1139: as [`git_write_explanation`], where secretless agent launch keeps
/// every agent command inside the sandbox at every level.
pub(crate) fn kept_git_write_explanation(path: &str) -> String {
    format!(
        "Security level Moderate blocked writing `{}`: agent commands always run inside the \
         sandbox, even when you approve them, and the sandbox keeps the project's .git folder \
         (git hooks and config) read-only. Secretless agent launch keeps agent commands inside \
         the sandbox at every security level, so changing the level would not allow it; a person \
         can run the command in their own terminal.",
        super::shown_path(path)
    )
}

/// The person's own permissions for the command: the profile without
/// Moderate's rules, with the project roots as the sandbox resolves them.
pub(super) fn own_profile(inputs: &ReadPolicyInputs) -> PermissionProfile {
    inputs
        .profile
        .clone()
        .materialize_project_roots_with_workspace_roots(&inputs.roots())
}

#[cfg(test)]
#[path = "moderate_tests.rs"]
mod tests;
