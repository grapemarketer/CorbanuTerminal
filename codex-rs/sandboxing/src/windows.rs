use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;

use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::protocol::WritableRoot;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_windows_sandbox::DenyReadTargets;

use crate::SandboxType;
use crate::compatibility_sandbox_policy_for_permission_profile;

/// Resolved filesystem overrides for the Windows sandbox backends.
///
/// The elevated Windows backend consumes extra deny-read paths plus explicit
/// read and write roots during setup/refresh. The unelevated restricted-token
/// backend only consumes extra deny-write carveouts on top of the legacy
/// `WorkspaceWrite` allow set. Read-root overrides are layered on top of the
/// baseline helper roots that the elevated setup path needs to launch the
/// sandboxed command; split policies that opt into platform defaults carry
/// that explicitly with the override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowsSandboxFilesystemOverrides {
    pub read_roots_override: Option<Vec<PathBuf>>,
    pub read_roots_include_platform_defaults: bool,
    pub write_roots_override: Option<Vec<PathBuf>>,
    /// Deny-read rules with their expanded paths. The elevated backend's
    /// persistent sync removes an entry only once its rule is gone (#304).
    pub additional_deny_read: DenyReadTargets,
    pub additional_deny_write_paths: Vec<AbsolutePathBuf>,
}

/// How to switch to the backend that can enforce read restrictions (#300).
macro_rules! switch_to_elevated {
    () => {
        "To run it, switch to the elevated Windows sandbox: run /setup-default-sandbox in the TUI, or set `sandbox = \"elevated\"` under `[windows]` in config.toml (for one run: `-c windows.sandbox=elevated`)."
    };
}

/// Refusal for a profile with deny-read entries under the unelevated
/// restricted-token backend, whose token cannot block those reads (#300).
pub const UNELEVATED_DENY_READ_REFUSAL: &str = concat!(
    "the command was not run because this Windows sandbox mode (unelevated) can't block reading the files this permission profile protects. ",
    switch_to_elevated!()
);

/// Refusal for a profile that limits which paths are readable at all.
pub const UNELEVATED_READ_ROOTS_REFUSAL: &str = concat!(
    "the command was not run because this Windows sandbox mode (unelevated) can't limit which files a command may read, which this permission profile requires. ",
    switch_to_elevated!()
);

/// D331: refusal for a profile with a writable folder (`descendant`) at or
/// inside a read-only one (`read_only`), which neither Windows sandbox
/// enforces directly.
pub fn writable_inside_read_only_refusal(read_only: &Path, descendant: &Path) -> String {
    format!(
        "the command was not run because the Windows sandbox doesn't support a writable folder in a read-only one, which this permission profile asks for: {} is writable but lies in {}, which is read-only (the protected .git, .codex and .agents folders count as read-only). Change the profile so that no writable folder lies in a read-only one.",
        descendant.display(),
        read_only.display()
    )
}

pub fn windows_sandbox_uses_elevated_backend(
    sandbox_level: WindowsSandboxLevel,
    proxy_enforced: bool,
) -> bool {
    // Windows firewall enforcement is tied to the logon-user sandbox identities, so
    // proxy-enforced sessions must use that backend even when the configured mode is
    // the default restricted-token sandbox.
    proxy_enforced || matches!(sandbox_level, WindowsSandboxLevel::Elevated)
}

pub fn permission_profile_supports_windows_restricted_token_sandbox(
    permission_profile: &PermissionProfile,
) -> bool {
    match permission_profile {
        PermissionProfile::Managed { file_system, .. } => {
            !file_system.to_sandbox_policy().has_full_disk_write_access()
        }
        PermissionProfile::Disabled | PermissionProfile::External { .. } => false,
    }
}

pub fn unsupported_windows_restricted_token_sandbox_reason(
    sandbox: SandboxType,
    permission_profile: &PermissionProfile,
    sandbox_policy_cwd: &AbsolutePathBuf,
    windows_sandbox_level: WindowsSandboxLevel,
) -> Option<String> {
    if windows_sandbox_level == WindowsSandboxLevel::Elevated {
        resolve_windows_elevated_filesystem_overrides(
            sandbox,
            permission_profile,
            sandbox_policy_cwd,
            windows_sandbox_level == WindowsSandboxLevel::Elevated,
        )
        .err()
    } else {
        resolve_windows_restricted_token_filesystem_overrides(
            sandbox,
            permission_profile,
            sandbox_policy_cwd,
            windows_sandbox_level,
        )
        .err()
    }
}

/// #300: refuses a launch on the unelevated restricted-token backend when its
/// profile restricts reads: any deny-read entry (an exact path or a glob,
/// whether or not it matches a file yet), or a limited set of readable roots.
/// That backend's token cannot block reads. Its spawn rejects exactly these
/// profiles (`has_full_disk_read_access`) with an unclear error, so this
/// refuses them first, with a message that says how to switch to the elevated
/// backend. `Ok` for every other sandbox type.
pub fn refuse_unenforceable_windows_read_restrictions(
    sandbox: SandboxType,
    permission_profile: &PermissionProfile,
) -> std::result::Result<(), String> {
    if sandbox != SandboxType::WindowsRestrictedToken {
        return Ok(());
    }
    let (file_system_sandbox_policy, _) = permission_profile.to_runtime_permissions();
    if file_system_sandbox_policy.has_denied_read_restrictions() {
        return Err(UNELEVATED_DENY_READ_REFUSAL.to_string());
    }
    if !file_system_sandbox_policy.has_full_disk_read_access() {
        return Err(UNELEVATED_READ_ROOTS_REFUSAL.to_string());
    }
    Ok(())
}

/// D331: the refusals of [`resolve_windows_restricted_token_filesystem_overrides`]
/// that concern what the unelevated backend can enforce from the profile
/// alone, for launches that pass it nothing else (the agent's commands): reads
/// (#300) and a writable folder in a read-only one. Its other refusals check
/// that `command/exec`'s extra deny-write paths can be derived from the
/// legacy projection, which such launches don't use.
pub fn refuse_unenforceable_windows_restricted_token_profile(
    sandbox: SandboxType,
    permission_profile: &PermissionProfile,
    sandbox_policy_cwd: &AbsolutePathBuf,
    windows_sandbox_level: WindowsSandboxLevel,
) -> std::result::Result<(), String> {
    if sandbox != SandboxType::WindowsRestrictedToken
        || windows_sandbox_level == WindowsSandboxLevel::Elevated
    {
        return Ok(());
    }
    refuse_unenforceable_windows_read_restrictions(sandbox, permission_profile)?;
    let (file_system_sandbox_policy, _) = permission_profile.to_runtime_permissions();
    refuse_writable_inside_read_only(file_system_sandbox_policy, sandbox_policy_cwd)
}

/// D331 for any profile, including one in the older workspace-write format
/// that needs no direct enforcement (#1810): its protected `.git`, `.codex`
/// and `.agents` folders can hold a writable folder too.
fn refuse_writable_inside_read_only(
    mut file_system_sandbox_policy: FileSystemSandboxPolicy,
    sandbox_policy_cwd: &AbsolutePathBuf,
) -> std::result::Result<(), String> {
    file_system_sandbox_policy.remove_skip_missing_path_entries();
    let split_writable_roots =
        file_system_sandbox_policy.get_writable_roots_with_cwd(sandbox_policy_cwd);
    match reopened_writable_descendant(
        &file_system_sandbox_policy,
        &split_writable_roots,
        sandbox_policy_cwd,
    ) {
        Some((read_only, descendant)) => {
            Err(writable_inside_read_only_refusal(&read_only, &descendant))
        }
        None => Ok(()),
    }
}

pub fn resolve_windows_restricted_token_filesystem_overrides(
    sandbox: SandboxType,
    permission_profile: &PermissionProfile,
    sandbox_policy_cwd: &AbsolutePathBuf,
    windows_sandbox_level: WindowsSandboxLevel,
) -> std::result::Result<Option<WindowsSandboxFilesystemOverrides>, String> {
    if sandbox != SandboxType::WindowsRestrictedToken
        || windows_sandbox_level == WindowsSandboxLevel::Elevated
    {
        return Ok(None);
    }
    refuse_network_working_folder(sandbox_policy_cwd.as_path())?;
    // D331: the backend refuses these too, with an unclear error, including
    // the profiles the early return below lets through.
    refuse_unenforceable_windows_read_restrictions(sandbox, permission_profile)?;

    let (mut file_system_sandbox_policy, network_sandbox_policy) =
        permission_profile.to_runtime_permissions();

    let needs_direct_runtime_enforcement = file_system_sandbox_policy
        .needs_direct_runtime_enforcement(network_sandbox_policy, sandbox_policy_cwd);

    if permission_profile_supports_windows_restricted_token_sandbox(permission_profile)
        && !needs_direct_runtime_enforcement
    {
        // #1810: D331 holds for these too.
        refuse_writable_inside_read_only(file_system_sandbox_policy, sandbox_policy_cwd)?;
        return Ok(None);
    }

    if !permission_profile_supports_windows_restricted_token_sandbox(permission_profile) {
        let permission_profile_name = permission_profile_display_name(permission_profile);
        return Err(format!(
            "windows sandbox backend cannot enforce file_system={:?}, network={network_sandbox_policy:?}, permission_profile={permission_profile_name}; refusing to run unsandboxed",
            file_system_sandbox_policy.kind,
        ));
    }

    // Windows protects existing metadata paths through the legacy writable root
    // projection. Do not turn skip-missing entries into newly-created
    // deny-write sentinels.
    file_system_sandbox_policy.remove_skip_missing_path_entries();

    // The read checks at the top already refuse what the next two do; they
    // stay as a second line of defence.
    // The restricted-token backend can still enforce split write restrictions,
    // but its WRITE_RESTRICTED token does not make capability SID deny-read ACEs
    // participate in read access checks. Read restrictions therefore require the
    // elevated backend, even when the filesystem root remains readable.
    if !windows_policy_has_root_read_access(&file_system_sandbox_policy, sandbox_policy_cwd) {
        return Err(UNELEVATED_READ_ROOTS_REFUSAL.to_string());
    }

    let additional_deny_read_paths = codex_windows_sandbox::resolve_windows_deny_read_paths(
        &file_system_sandbox_policy,
        sandbox_policy_cwd,
    )?;
    if !additional_deny_read_paths.is_empty() {
        return Err(UNELEVATED_DENY_READ_REFUSAL.to_string());
    }

    let split_writable_roots =
        file_system_sandbox_policy.get_writable_roots_with_cwd(sandbox_policy_cwd);
    if let Some((read_only, descendant)) = reopened_writable_descendant(
        &file_system_sandbox_policy,
        &split_writable_roots,
        sandbox_policy_cwd,
    ) {
        return Err(writable_inside_read_only_refusal(&read_only, &descendant));
    }

    let legacy_projection = compatibility_sandbox_policy_for_permission_profile(
        permission_profile,
        sandbox_policy_cwd.as_path(),
    );
    let legacy_writable_roots = legacy_projection.get_writable_roots_with_cwd(sandbox_policy_cwd);
    // D398 (#1766): both sets compare by real path. The split roots are
    // already partly resolved (a `subst` or mapped drive, an 8.3 name or the
    // letter case), the legacy roots are as spelled, so a working folder
    // spelled one of those ways differed by spelling only. Real paths are
    // equal only for the same folders, where both grants land.
    let legacy_real_roots: Vec<PathBuf> = legacy_writable_roots
        .iter()
        .map(|root| real_windows_override_path(root.root.as_path()))
        .collect::<std::result::Result<_, _>>()?;
    let legacy_root_paths: BTreeSet<&PathBuf> = legacy_real_roots.iter().collect();
    let split_real_roots: Vec<PathBuf> = split_writable_roots
        .iter()
        .map(|root| real_windows_override_path(root.root.as_path()))
        .collect::<std::result::Result<_, _>>()?;
    let split_root_paths: BTreeSet<&PathBuf> = split_real_roots.iter().collect();

    if legacy_root_paths != split_root_paths {
        return Err(
            "windows unelevated restricted-token sandbox cannot enforce split writable root sets directly; refusing to run unsandboxed"
                .to_string(),
        );
    }

    // The legacy read-only paths by real path. Two spellings of one root
    // (the workspace by its real path, the working folder through a `subst`
    // drive) each bring theirs; the backend denies all of them, whichever
    // root brought it.
    let mut legacy_read_only: BTreeMap<&PathBuf, BTreeSet<PathBuf>> = BTreeMap::new();
    for (legacy_root, real_root) in legacy_writable_roots.iter().zip(&legacy_real_roots) {
        let paths = legacy_read_only.entry(real_root).or_default();
        for read_only_subpath in &legacy_root.read_only_subpaths {
            paths.insert(real_windows_override_path(read_only_subpath.as_path())?);
        }
    }

    let mut additional_deny_write_paths = BTreeSet::new();
    for (split_root, real_root) in split_writable_roots.iter().zip(&split_real_roots) {
        let Some(already_read_only) = legacy_read_only.get(real_root) else {
            return Err(
                "windows unelevated restricted-token sandbox cannot enforce split writable root sets directly; refusing to run unsandboxed"
                    .to_string(),
            );
        };
        for read_only_subpath in &split_root.read_only_subpaths {
            // Any other read-only path is passed on as an extra deny-write
            // path, as spelled.
            if !already_read_only
                .contains(&real_windows_override_path(read_only_subpath.as_path())?)
            {
                additional_deny_write_paths.insert(normalize_windows_override_path(
                    read_only_subpath.as_path(),
                )?);
            }
        }
    }

    if additional_deny_write_paths.is_empty() {
        return Ok(None);
    }

    Ok(Some(WindowsSandboxFilesystemOverrides {
        read_roots_override: None,
        read_roots_include_platform_defaults: false,
        write_roots_override: None,
        additional_deny_read: DenyReadTargets::default(),
        additional_deny_write_paths: additional_deny_write_paths
            .into_iter()
            .map(|path| AbsolutePathBuf::from_absolute_path(path).map_err(|err| err.to_string()))
            .collect::<std::result::Result<_, _>>()?,
    }))
}

pub fn resolve_windows_elevated_filesystem_overrides(
    sandbox: SandboxType,
    permission_profile: &PermissionProfile,
    sandbox_policy_cwd: &AbsolutePathBuf,
    use_windows_elevated_backend: bool,
) -> std::result::Result<Option<WindowsSandboxFilesystemOverrides>, String> {
    if sandbox != SandboxType::WindowsRestrictedToken || !use_windows_elevated_backend {
        return Ok(None);
    }

    let (mut file_system_sandbox_policy, network_sandbox_policy) =
        permission_profile.to_runtime_permissions();

    if !permission_profile_supports_windows_restricted_token_sandbox(permission_profile) {
        let permission_profile_name = permission_profile_display_name(permission_profile);
        return Err(format!(
            "windows sandbox backend cannot enforce file_system={:?}, network={network_sandbox_policy:?}, permission_profile={permission_profile_name}; refusing to run unsandboxed",
            file_system_sandbox_policy.kind,
        ));
    }

    // Windows protects existing metadata paths through the legacy writable root
    // projection. Do not turn skip-missing entries into newly-created
    // deny-write sentinels.
    file_system_sandbox_policy.remove_skip_missing_path_entries();

    let additional_deny_read = codex_windows_sandbox::resolve_windows_deny_read_targets(
        &file_system_sandbox_policy,
        sandbox_policy_cwd,
    )?;

    let split_writable_roots =
        file_system_sandbox_policy.get_writable_roots_with_cwd(sandbox_policy_cwd);
    if let Some((read_only, descendant)) = reopened_writable_descendant(
        &file_system_sandbox_policy,
        &split_writable_roots,
        sandbox_policy_cwd,
    ) {
        return Err(writable_inside_read_only_refusal(&read_only, &descendant));
    }

    let needs_direct_runtime_enforcement = file_system_sandbox_policy
        .needs_direct_runtime_enforcement(network_sandbox_policy, sandbox_policy_cwd);
    let has_explicit_write_carveouts = split_writable_roots.iter().any(|writable_root| {
        writable_root.read_only_subpaths.iter().any(|path| {
            file_system_sandbox_policy
                .has_explicit_non_write_entry_for_path_with_cwd(path.as_path(), sandbox_policy_cwd)
        })
    });
    let normalize_path = |path: PathBuf| dunce::canonicalize(&path).unwrap_or(path);
    let legacy_projection = compatibility_sandbox_policy_for_permission_profile(
        permission_profile,
        sandbox_policy_cwd.as_path(),
    );
    let legacy_writable_roots = legacy_projection.get_writable_roots_with_cwd(sandbox_policy_cwd);
    let legacy_root_paths: BTreeSet<PathBuf> = legacy_writable_roots
        .iter()
        .map(|root| normalize_path(root.root.to_path_buf()))
        .collect();
    let split_readable_roots: Vec<PathBuf> = file_system_sandbox_policy
        .get_readable_roots_with_cwd(sandbox_policy_cwd)
        .into_iter()
        .map(AbsolutePathBuf::into_path_buf)
        .map(&normalize_path)
        .collect();
    let split_root_paths: Vec<PathBuf> = split_writable_roots
        .iter()
        .map(|root| normalize_path(root.root.to_path_buf()))
        .collect();
    let split_root_path_set: BTreeSet<PathBuf> = split_root_paths.iter().cloned().collect();

    // `has_full_disk_read_access()` is intentionally false when deny-read
    // entries exist. For Windows setup overrides, the important question is
    // whether the baseline still reads from the filesystem root and only needs
    // additional deny ACLs layered on top.
    let split_has_root_read_access =
        windows_policy_has_root_read_access(&file_system_sandbox_policy, sandbox_policy_cwd);
    let read_roots_override = if split_has_root_read_access {
        None
    } else {
        Some(split_readable_roots)
    };

    let write_roots_override = if split_root_path_set == legacy_root_paths {
        None
    } else {
        Some(split_root_paths)
    };

    let additional_deny_write_paths = if needs_direct_runtime_enforcement
        || has_explicit_write_carveouts
    {
        let mut deny_paths = BTreeSet::new();
        for writable_root in &split_writable_roots {
            let writable_root_path = normalize_path(writable_root.root.to_path_buf());
            let legacy_root = legacy_writable_roots.iter().find(|candidate| {
                normalize_path(candidate.root.to_path_buf()) == writable_root_path
            });
            for read_only_subpath in &writable_root.read_only_subpaths {
                let read_only_subpath_suffix = read_only_subpath
                    .as_path()
                    .strip_prefix(writable_root.root.as_path())
                    .ok();
                let already_denied_by_legacy = legacy_root.is_some_and(|legacy_root| {
                    legacy_root.read_only_subpaths.iter().any(|candidate| {
                        candidate
                            .as_path()
                            .strip_prefix(legacy_root.root.as_path())
                            .ok()
                            == read_only_subpath_suffix
                    })
                });
                let explicitly_configured = file_system_sandbox_policy
                    .has_explicit_non_write_entry_for_path_with_cwd(
                        read_only_subpath.as_path(),
                        sandbox_policy_cwd,
                    );
                if explicitly_configured || !already_denied_by_legacy {
                    deny_paths.insert(normalize_path(read_only_subpath.to_path_buf()));
                }
            }
        }
        deny_paths
            .into_iter()
            .map(|path| AbsolutePathBuf::from_absolute_path(path).map_err(|err| err.to_string()))
            .collect::<std::result::Result<_, _>>()?
    } else {
        Vec::new()
    };

    if read_roots_override.is_none()
        && write_roots_override.is_none()
        // A deny-read rule matching nothing now is still passed on, so the
        // persistent sync keeps its earlier entries (#304).
        && additional_deny_read.is_empty()
        && additional_deny_write_paths.is_empty()
    {
        return Ok(None);
    }

    Ok(Some(WindowsSandboxFilesystemOverrides {
        read_roots_include_platform_defaults: read_roots_override.is_some()
            && file_system_sandbox_policy.include_platform_defaults(),
        read_roots_override,
        write_roots_override,
        additional_deny_read,
        additional_deny_write_paths,
    }))
}

fn normalize_windows_override_path(path: &Path) -> std::result::Result<PathBuf, String> {
    AbsolutePathBuf::from_absolute_path(dunce::simplified(path))
        .map(AbsolutePathBuf::into_path_buf)
        .map_err(|err| err.to_string())
}

/// D398: `path` by its real location ([`canonicalize_allowing_missing`]),
/// or as [`normalize_windows_override_path`] has it when that can't be
/// resolved.
fn real_windows_override_path(path: &Path) -> std::result::Result<PathBuf, String> {
    match canonicalize_allowing_missing(path) {
        Some(real) => normalize_windows_override_path(&real),
        None => normalize_windows_override_path(path),
    }
}

/// D446: refuses a working folder on a network (a UNC path, or a mapped
/// drive, whose real path is one) for the unelevated restricted-token
/// backend. Its commands can't write there at all, so they would run as if
/// the folder were read-only (#1866 measured a mapped drive); refuse with a
/// clear message instead. `\\localhost` paths are refused earlier (#1863).
pub fn refuse_network_working_folder(cwd: &Path) -> std::result::Result<(), String> {
    use codex_windows_sandbox::local_drive::is_network_path;
    let network = is_network_path(cwd)
        || canonicalize_allowing_missing(cwd).is_some_and(|real| is_network_path(&real));
    if network {
        return Err(format!(
            "`{}` is a network folder (a UNC path or a mapped network drive). The Windows non-admin sandbox can't let commands write there, so it doesn't run them in it; use a folder on a local drive instead.",
            cwd.display()
        ));
    }
    Ok(())
}

fn windows_policy_has_root_read_access(
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
    cwd: &AbsolutePathBuf,
) -> bool {
    let Some(root) = cwd.as_path().ancestors().last() else {
        return false;
    };
    file_system_sandbox_policy.can_read_path_with_cwd(root, cwd.as_path())
}

fn permission_profile_display_name(permission_profile: &PermissionProfile) -> &'static str {
    match permission_profile {
        PermissionProfile::Managed { .. } => "Managed",
        PermissionProfile::Disabled => "Disabled",
        PermissionProfile::External { .. } => "External",
    }
}

/// A read-only path of one writable root and another writable root at or
/// inside it, if any (D331): compared as spelled, and also by real
/// (link-followed) path (#1801), so a junction, folder link or `subst` drive
/// on either folder or on the working folder doesn't hide the shape. As the
/// non-admin backend does (#1780), a root's read-only paths include the
/// profile's explicit read-only and deny paths inside it by real path only.
/// By real path only a root strictly inside counts: the same folder named
/// read-only and writable under two spellings stays read-only (#1780).
fn reopened_writable_descendant(
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
    writable_roots: &[WritableRoot],
    cwd: &AbsolutePathBuf,
) -> Option<(PathBuf, PathBuf)> {
    let spelled = writable_roots.iter().find_map(|writable_root| {
        writable_root
            .read_only_subpaths
            .iter()
            .find_map(|read_only_subpath| {
                writable_roots
                    .iter()
                    .find(|candidate| {
                        candidate.root.as_path() != writable_root.root.as_path()
                            && candidate
                                .root
                                .as_path()
                                .starts_with(read_only_subpath.as_path())
                    })
                    .map(|candidate| {
                        (
                            read_only_subpath.to_path_buf(),
                            candidate.root.to_path_buf(),
                        )
                    })
            })
    });
    if spelled.is_some() {
        return spelled;
    }
    let explicit_non_write: Vec<(AbsolutePathBuf, Option<PathBuf>)> = file_system_sandbox_policy
        .get_read_only_paths_with_cwd(cwd.as_path())
        .into_iter()
        .chain(file_system_sandbox_policy.get_unreadable_roots_with_cwd(cwd.as_path()))
        .map(|path| {
            let real = canonicalize_allowing_missing(path.as_path());
            (path, real)
        })
        .collect();
    let real_roots: Vec<Option<PathBuf>> = writable_roots
        .iter()
        .map(|writable_root| canonicalize_allowing_missing(writable_root.root.as_path()))
        .collect();
    for (writable_root, real_root) in writable_roots.iter().zip(&real_roots) {
        let Some(real_root) = real_root else {
            continue;
        };
        let read_only_paths = writable_root
            .read_only_subpaths
            .iter()
            .map(|path| (path, canonicalize_allowing_missing(path.as_path())))
            .chain(
                explicit_non_write
                    .iter()
                    .filter(|(_, real)| {
                        real.as_ref()
                            .is_some_and(|real| strictly_inside(real, real_root))
                    })
                    .map(|(path, real)| (path, real.clone())),
            );
        for (read_only, real_read_only) in read_only_paths {
            let Some(real_read_only) = real_read_only else {
                continue;
            };
            let reopened = writable_roots
                .iter()
                .zip(&real_roots)
                .find(|(candidate, real)| {
                    candidate.root.as_path() != writable_root.root.as_path()
                        && real
                            .as_ref()
                            .is_some_and(|real| strictly_inside(real, &real_read_only))
                });
            if let Some((candidate, _)) = reopened {
                return Some((read_only.to_path_buf(), candidate.root.to_path_buf()));
            }
        }
    }
    None
}

fn strictly_inside(path: &Path, root: &Path) -> bool {
    path != root && path.starts_with(root)
}

/// `path` with links followed: its nearest existing ancestor canonicalized,
/// plus the rest as spelled (as `codex-windows-sandbox` resolves it, #1780).
/// `None` when that fails other than for a missing folder.
fn canonicalize_allowing_missing(path: &Path) -> Option<PathBuf> {
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        match dunce::canonicalize(existing) {
            Ok(real) => {
                return Some(
                    missing
                        .iter()
                        .rev()
                        .fold(real, |path, part| path.join(part)),
                );
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                missing.push(existing.file_name()?.to_os_string());
                existing = existing.parent()?;
            }
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
