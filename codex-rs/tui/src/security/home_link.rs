//! #594 (D225, Travis 2026-10-28): on Linux a Corbanu home reached through a
//! link that sits in a folder agent commands can write is not protected by
//! the sandbox (a mount cannot sit on a link). A command could replace the
//! link with a folder of its own, and the next start would use that folder
//! and lose the stored level. Moderate warns at start-up; Aggressive refuses
//! to start, to be chosen, or to keep running in a new folder. Recovery:
//! point `CORBANU_HOME` at the real folder. macOS keeps the link itself
//! read-only, so it is left out there.

use std::collections::VecDeque;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use codex_protocol::permissions::FileSystemSandboxPolicy;

/// Links followed more than this many times are reported as one problem.
const MAX_LINKS: usize = 40;

/// A link on the way to the Corbanu home that agent commands could replace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExposedLink {
    /// The link, in its real folder.
    pub(crate) link: PathBuf,
    /// The real folder holding it.
    pub(crate) folder: PathBuf,
}

/// Whether the check applies on this platform.
pub(crate) fn applies() -> bool {
    cfg!(target_os = "linux")
}

/// Every link followed while resolving `path`, with the real folder holding
/// it, in order. Each link's own target is resolved too, so a link inside a
/// link's target is listed as well.
pub(crate) fn traversed_links(path: &Path) -> Vec<ExposedLink> {
    let mut links = Vec::new();
    let mut pending: VecDeque<PathBuf> = components(path);
    let mut current = PathBuf::from("/");
    while let Some(part) = pending.pop_front() {
        let next = current.join(&part);
        if part == Path::new("/") {
            current = next;
            continue;
        }
        if part == Path::new("..") {
            current.pop();
            continue;
        }
        match std::fs::symlink_metadata(&next) {
            Ok(meta) if meta.file_type().is_symlink() => {
                links.push(ExposedLink {
                    link: next.clone(),
                    folder: current.clone(),
                });
                let Ok(target) = std::fs::read_link(&next) else {
                    break;
                };
                if links.len() > MAX_LINKS {
                    break;
                }
                for part in components(&target).into_iter().rev() {
                    pending.push_front(part);
                }
            }
            _ => current = next,
        }
    }
    links
}

fn components(path: &Path) -> VecDeque<PathBuf> {
    path.components()
        .filter_map(|component| match component {
            Component::RootDir => Some(PathBuf::from("/")),
            Component::ParentDir => Some(PathBuf::from("..")),
            Component::Normal(name) => Some(PathBuf::from(name)),
            Component::CurDir | Component::Prefix(_) => None,
        })
        .collect()
}

/// The links on the way to `given` that agent commands under `policy` could
/// replace: the profile lets them write the link's folder, and the user (as
/// whom they run) controls it or a folder above it that the profile also
/// lets them write (renaming that one rebuilds the path). A folder named
/// through a link in the policy counts by its real path.
pub(crate) fn exposed_links(
    given: &Path,
    policy: &FileSystemSandboxPolicy,
    cwd: &Path,
) -> Vec<ExposedLink> {
    exposed_links_with(given, |folder| {
        policy.can_write_path_with_cwd(folder, cwd)
            || policy.can_write_effective_path_with_cwd(folder, cwd)
    })
}

/// [`exposed_links`] for Aggressive: `policy`, and also the current folder
/// by its real path (Aggressive's only writable root; `aggressive::verify`
/// refuses any other).
pub(crate) fn exposed_links_under_aggressive(
    given: &Path,
    policy: Option<&FileSystemSandboxPolicy>,
    cwd: &Path,
) -> Vec<ExposedLink> {
    let real_cwd = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    exposed_links_with(given, |folder| {
        folder.starts_with(&real_cwd)
            || policy.is_some_and(|policy| {
                policy.can_write_path_with_cwd(folder, cwd)
                    || policy.can_write_effective_path_with_cwd(folder, cwd)
            })
    })
}

fn exposed_links_with(given: &Path, writable: impl Fn(&Path) -> bool) -> Vec<ExposedLink> {
    traversed_links(given)
        .into_iter()
        .filter(|link| {
            writable(&link.folder)
                && link
                    .folder
                    .ancestors()
                    .any(|folder| writable(folder) && user_controls(folder))
        })
        .collect()
}

/// Whether the user can change `folder`'s entries: it is writable to them,
/// or theirs (they could make it writable).
#[cfg(unix)]
fn user_controls(folder: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let Ok(path) = std::ffi::CString::new(folder.as_os_str().as_bytes()) else {
        return true;
    };
    // SAFETY: `path` is a valid NUL-terminated string for the call.
    let writable = unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 };
    // SAFETY: `geteuid` has no preconditions.
    let euid = unsafe { libc::geteuid() };
    writable || std::fs::metadata(folder).map_or(true, |meta| meta.uid() == euid)
}

#[cfg(not(unix))]
fn user_controls(_folder: &Path) -> bool {
    true
}

/// The given home, its real folder and the exposed links, when the given
/// home names `codex_home` (the home this process uses).
pub(crate) fn check(
    codex_home: &Path,
    exposed: impl Fn(&Path) -> Vec<ExposedLink>,
) -> Option<(PathBuf, Vec<ExposedLink>)> {
    check_given(&given_home()?, codex_home, exposed)
}

#[cfg(test)]
thread_local! {
    /// Tests name the given home here instead of the process environment.
    pub(crate) static GIVEN_FOR_TEST: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

fn given_home() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(given) = GIVEN_FOR_TEST.with(|given| given.borrow().clone()) {
        return Some(given);
    }
    codex_utils_home_dir::given_codex_home().map(|(_, given)| given)
}

fn check_given(
    given: &Path,
    codex_home: &Path,
    exposed: impl Fn(&Path) -> Vec<ExposedLink>,
) -> Option<(PathBuf, Vec<ExposedLink>)> {
    let real = std::fs::canonicalize(given).ok()?;
    if std::fs::canonicalize(codex_home).ok()? != real {
        return None;
    }
    let links = exposed(given);
    (!links.is_empty()).then_some((real, links))
}

fn named(links: &[ExposedLink]) -> String {
    links
        .iter()
        .map(|link| link.link.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The start-up warning under Moderate.
pub(crate) fn moderate_warning(real: &Path, links: &[ExposedLink]) -> String {
    format!(
        "Security level Moderate: your Corbanu home is reached through a link ({}) in a folder agent commands can write, so a command could replace it and the next start would use another folder without your security level. Start Corbanu Terminal with CORBANU_HOME={} to close this.",
        named(links),
        real.display()
    )
}

/// The line Aggressive's verification adds (launch, a new folder, or the
/// `/security` review).
pub(crate) fn aggressive_refusal(real: &Path, links: &[ExposedLink]) -> String {
    format!(
        "Corbanu home: it is reached through a link ({}) in a folder agent commands can write, so a command could replace it and the next start would lose Aggressive. Start Corbanu Terminal with CORBANU_HOME={} (the real folder), or start it from a folder that does not contain the link.",
        named(links),
        real.display()
    )
}

/// Moderate's start-up warning for this process, if one applies.
pub(crate) fn moderate_startup_warning(
    codex_home: &Path,
    policy: &FileSystemSandboxPolicy,
    cwd: &Path,
) -> Option<String> {
    if !applies() {
        return None;
    }
    check(codex_home, |given| exposed_links(given, policy, cwd))
        .map(|(real, links)| moderate_warning(&real, &links))
}

/// Why Aggressive cannot run (or be chosen) from `cwd`, if it cannot.
pub(crate) fn aggressive_blocker(codex_home: &Path, cwd: &Path) -> Option<String> {
    if !applies() {
        return None;
    }
    check(codex_home, |given| {
        exposed_links_under_aggressive(given, /*policy*/ None, cwd)
    })
    .map(|(real, links)| aggressive_refusal(&real, &links))
}

#[cfg(test)]
#[path = "home_link_tests.rs"]
mod tests;
