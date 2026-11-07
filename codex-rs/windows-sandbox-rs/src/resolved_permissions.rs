use anyhow::Result;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxKind;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_utils_absolute_path::AbsolutePathBuf;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

/// Windows-local view of the runtime permission profile.
///
/// Most Windows sandbox code needs resolved runtime permissions plus a few
/// Windows-specific path conventions, not the user/config-facing
/// `PermissionProfile` enum itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWindowsSandboxPermissions {
    file_system: FileSystemSandboxPolicy,
    network: NetworkSandboxPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WindowsWritableRoot {
    pub(crate) root: PathBuf,
    pub(crate) read_only_subpaths: Vec<PathBuf>,
}

/// Restricted-token family needed to enforce a Windows permission profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsSandboxTokenMode {
    ReadOnlyCapability,
    WritableRootsCapability,
}

/// Chooses the restricted-token family needed for a managed permission profile.
pub fn token_mode_for_permission_profile(
    permission_profile: &PermissionProfile,
    workspace_roots: &[AbsolutePathBuf],
    cwd: &Path,
    env_map: &HashMap<String, String>,
) -> Result<WindowsSandboxTokenMode> {
    let permissions =
        ResolvedWindowsSandboxPermissions::try_from_permission_profile_for_workspace_roots(
            permission_profile,
            workspace_roots,
        )?;
    if permissions.file_system.has_full_disk_write_access() {
        anyhow::bail!(
            "permission profile requests full-disk filesystem writes, which cannot be enforced by the Windows sandbox"
        );
    }
    if permissions.writable_roots_for_cwd(cwd, env_map).is_empty() {
        Ok(WindowsSandboxTokenMode::ReadOnlyCapability)
    } else {
        Ok(WindowsSandboxTokenMode::WritableRootsCapability)
    }
}

impl ResolvedWindowsSandboxPermissions {
    pub fn try_from_permission_profile(permission_profile: &PermissionProfile) -> Result<Self> {
        if !matches!(permission_profile, PermissionProfile::Managed { .. }) {
            anyhow::bail!(
                "only managed permission profiles can be enforced by the Windows sandbox"
            );
        }
        let (file_system, network) = permission_profile.to_runtime_permissions();
        if !matches!(file_system.kind, FileSystemSandboxKind::Restricted) {
            anyhow::bail!(
                "only restricted managed filesystem permissions can be enforced by the Windows sandbox"
            );
        }
        Ok(Self {
            file_system,
            network,
        })
    }

    /// #734: the file-system half of a run's permissions, `:workspace_roots`
    /// bound as in [`Self::try_from_permission_profile_for_workspace_roots`].
    pub(crate) fn from_file_system_policy(
        file_system: FileSystemSandboxPolicy,
        workspace_roots: &[AbsolutePathBuf],
    ) -> Self {
        Self {
            file_system: file_system
                .materialize_project_roots_with_workspace_roots(workspace_roots),
            network: NetworkSandboxPolicy::default(),
        }
    }

    /// Resolves a managed permission profile and binds symbolic `:workspace_roots`
    /// entries to the workspace roots supplied by the caller.
    pub fn try_from_permission_profile_for_workspace_roots(
        permission_profile: &PermissionProfile,
        workspace_roots: &[AbsolutePathBuf],
    ) -> Result<Self> {
        let mut permissions = Self::try_from_permission_profile(permission_profile)?;
        permissions.file_system = permissions
            .file_system
            .materialize_project_roots_with_workspace_roots(workspace_roots);
        Ok(permissions)
    }

    pub(crate) fn should_apply_network_block(&self) -> bool {
        !self.network.is_enabled()
    }

    pub(crate) fn network_policy(&self) -> NetworkSandboxPolicy {
        self.network
    }

    pub(crate) fn is_enforceable_by_windows_sandbox(&self) -> bool {
        matches!(self.file_system.kind, FileSystemSandboxKind::Restricted)
    }

    /// Whether the profile names `path` itself as not writable (an explicit
    /// read-only carve-out, as opposed to the default `.git`/`.codex`/`.agents`
    /// protections).
    pub(crate) fn is_explicit_read_only_path(&self, path: &Path, cwd: &Path) -> bool {
        // D293 (#1542 item 2): also when spelled against a writable root's
        // resolved path.
        self.file_system
            .has_explicit_non_write_entry_resolving_to(path, cwd)
    }

    pub(crate) fn has_full_disk_read_access(&self) -> bool {
        self.file_system.has_full_disk_read_access()
    }

    pub(crate) fn include_platform_defaults(&self) -> bool {
        self.file_system.include_platform_defaults()
    }

    pub(crate) fn readable_roots_for_cwd(&self, cwd: &Path) -> Vec<PathBuf> {
        self.file_system
            .get_readable_roots_with_cwd(cwd)
            .into_iter()
            .map(AbsolutePathBuf::into_path_buf)
            .collect()
    }

    pub(crate) fn uses_write_capabilities_for_cwd(
        &self,
        cwd: &Path,
        env_map: &HashMap<String, String>,
    ) -> bool {
        !self.writable_roots_for_cwd(cwd, env_map).is_empty()
    }

    pub(crate) fn writable_roots_for_cwd(
        &self,
        cwd: &Path,
        env_map: &HashMap<String, String>,
    ) -> Vec<WindowsWritableRoot> {
        let mut roots = self.writable_roots_other_than_temp_for_cwd(cwd);
        if self.has_writable_tmpdir_entry() {
            // #1708: explicit read-only (and deny) paths inside a temp root
            // are carved out of it like any other root's. The default
            // `.git`/`.codex`/`.agents` protections stay off there (D283).
            let explicit_non_write: Vec<PathBuf> = self
                .file_system
                .get_read_only_paths_with_cwd(cwd)
                .into_iter()
                .chain(self.file_system.get_unreadable_roots_with_cwd(cwd))
                .map(AbsolutePathBuf::into_path_buf)
                .collect();
            roots.extend(windows_temp_env_roots(env_map).into_iter().map(|root| {
                let read_only_subpaths = explicit_non_write
                    .iter()
                    .filter(|path| strictly_inside(path, &root))
                    .cloned()
                    .collect();
                WindowsWritableRoot {
                    root,
                    read_only_subpaths,
                }
            }));
        }
        roots
    }

    /// The writable roots that don't come from the temp folder setting
    /// (`%TEMP%`/`%TMP%`), even where one resolves to the same folder (D283).
    pub(crate) fn writable_roots_other_than_temp_for_cwd(
        &self,
        cwd: &Path,
    ) -> Vec<WindowsWritableRoot> {
        let mut file_system = self.file_system.clone();
        file_system
            .entries
            .retain(|FileSystemSandboxEntry { path, .. }| {
                !matches!(
                    path,
                    FileSystemPath::Special {
                        value: codex_protocol::permissions::FileSystemSpecialPath::Tmpdir
                            | codex_protocol::permissions::FileSystemSpecialPath::SlashTmp,
                    }
                )
            });

        file_system
            .get_writable_roots_with_cwd(cwd)
            .into_iter()
            .map(|root| WindowsWritableRoot {
                root: root.root.into_path_buf(),
                read_only_subpaths: root
                    .read_only_subpaths
                    .into_iter()
                    .map(AbsolutePathBuf::into_path_buf)
                    .collect(),
            })
            .collect::<Vec<_>>()
    }

    fn has_writable_tmpdir_entry(&self) -> bool {
        self.file_system
            .entries
            .iter()
            .any(|FileSystemSandboxEntry { path, access, .. }| {
                matches!(
                    path,
                    FileSystemPath::Special {
                        value: codex_protocol::permissions::FileSystemSpecialPath::Tmpdir,
                    }
                ) && access.can_write()
            })
    }
}

fn windows_temp_env_roots(env_map: &HashMap<String, String>) -> Vec<PathBuf> {
    ["TEMP", "TMP"]
        .into_iter()
        .filter_map(|key| {
            env_map
                .get(key)
                .map(|value| PathBuf::from(value.as_str()))
                .or_else(|| std::env::var_os(key).map(PathBuf::from))
        })
        .filter(|path| path.is_absolute())
        .collect()
}

/// Whether `path` lies below `root` (not at it), as spelled or with links
/// followed (`%TEMP%` is often an 8.3 short name; `path` may not exist yet).
fn strictly_inside(path: &Path, root: &Path) -> bool {
    let below = |path: &Path, root: &Path| path != root && path.starts_with(root);
    below(path, root)
        || canonicalize_allowing_missing(path)
            .zip(dunce::canonicalize(root).ok())
            .is_some_and(|(path, root)| below(&path, &root))
}

/// `path` with links followed: its nearest existing ancestor canonicalized,
/// plus the rest as spelled.
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
mod tests {
    use super::*;
    use codex_protocol::models::ManagedFileSystemPermissions;
    use codex_protocol::permissions::FileSystemAccessMode;
    use codex_protocol::permissions::FileSystemSandboxEntry;
    use codex_protocol::permissions::FileSystemSpecialPath;
    use codex_protocol::permissions::project_roots_glob_pattern;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    fn workspace_roots_for(root: &Path) -> Vec<AbsolutePathBuf> {
        vec![AbsolutePathBuf::from_absolute_path(root).expect("absolute workspace root")]
    }

    #[test]
    fn permission_profile_workspace_write_uses_windows_temp_env_vars() {
        let tmp = TempDir::new().expect("tempdir");
        let cwd = tmp.path().join("workspace");
        let temp_dir = tmp.path().join("temp");
        std::fs::create_dir_all(&cwd).expect("create cwd");
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");

        let mut env_map = HashMap::new();
        env_map.insert("TEMP".to_string(), temp_dir.to_string_lossy().to_string());
        env_map.insert("TMP".to_string(), temp_dir.to_string_lossy().to_string());

        let permissions = ResolvedWindowsSandboxPermissions::try_from_permission_profile(
            &PermissionProfile::workspace_write(),
        )
        .expect("managed permission profile");
        let roots = permissions
            .writable_roots_for_cwd(&cwd, &env_map)
            .into_iter()
            .map(|root| root.root)
            .collect::<std::collections::HashSet<_>>();

        let expected_roots = [
            temp_dir,
            dunce::canonicalize(&cwd).expect("canonicalize cwd"),
        ]
        .into_iter()
        .collect::<std::collections::HashSet<_>>();

        assert_eq!(expected_roots, roots);
    }

    /// #1708: a temp root carries the explicit read-only and deny paths
    /// inside it (missing ones too; spelled like the root, in another case,
    /// or matching only once both are resolved), but not the root itself, the
    /// default `.git`/`.codex`/`.agents` protections, nor paths outside it.
    #[test]
    fn temp_root_carries_explicit_read_only_paths_inside_it() {
        let tmp = TempDir::new().expect("tempdir");
        let base = dunce::canonicalize(tmp.path()).expect("canonical tempdir");
        let cwd = base.join("workspace");
        let temp_dir = base.join("temp");
        std::fs::create_dir_all(&cwd).expect("create cwd");
        std::fs::create_dir_all(temp_dir.join("keep")).expect("create keep");
        std::fs::create_dir_all(temp_dir.join("cased")).expect("create cased");
        std::fs::create_dir_all(temp_dir.join(".git")).expect("create .git");
        // `TMP` spells the temp root as configured; `TEMP` through `..`, which
        // only matches the configured paths once both are resolved.
        let temp_env = base.join("workspace").join("..").join("temp");
        let absolute = |path: PathBuf| AbsolutePathBuf::from_absolute_path(path).expect("abs");
        let entry = |path: PathBuf, access| FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: absolute(path),
            },
            access,
            missing_path_behavior: None,
        };
        let permission_profile = PermissionProfile::Managed {
            file_system: ManagedFileSystemPermissions::Restricted {
                entries: vec![
                    FileSystemSandboxEntry {
                        path: FileSystemPath::Special {
                            value: FileSystemSpecialPath::Root,
                        },
                        access: FileSystemAccessMode::Read,
                        missing_path_behavior: None,
                    },
                    entry(cwd.clone(), FileSystemAccessMode::Write),
                    FileSystemSandboxEntry {
                        path: FileSystemPath::Special {
                            value: FileSystemSpecialPath::Tmpdir,
                        },
                        access: FileSystemAccessMode::Write,
                        missing_path_behavior: None,
                    },
                    entry(temp_dir.clone(), FileSystemAccessMode::Read),
                    entry(temp_dir.join("keep"), FileSystemAccessMode::Read),
                    entry(base.join("TEMP").join("CASED"), FileSystemAccessMode::Read),
                    entry(temp_dir.join("missing"), FileSystemAccessMode::Read),
                    entry(temp_dir.join("secret"), FileSystemAccessMode::Deny),
                    entry(base.join("elsewhere"), FileSystemAccessMode::Read),
                ],
                glob_scan_max_depth: None,
            },
            network: NetworkSandboxPolicy::Restricted,
        };
        let permissions =
            ResolvedWindowsSandboxPermissions::try_from_permission_profile_for_workspace_roots(
                &permission_profile,
                workspace_roots_for(cwd.as_path()).as_slice(),
            )
            .expect("managed permission profile");
        let env_map = HashMap::from([
            ("TEMP".to_string(), temp_env.to_string_lossy().into_owned()),
            ("TMP".to_string(), temp_dir.to_string_lossy().into_owned()),
        ]);

        let temp_roots: Vec<Vec<PathBuf>> = permissions
            .writable_roots_for_cwd(&cwd, &env_map)
            .into_iter()
            .filter(|root| root.root == temp_env || root.root == temp_dir)
            .map(|root| {
                let mut subpaths = root.read_only_subpaths;
                subpaths.sort();
                subpaths
            })
            .collect();

        let mut expected = vec![
            base.join("TEMP").join("CASED"),
            temp_dir.join("keep"),
            temp_dir.join("missing"),
            temp_dir.join("secret"),
        ];
        expected.sort();
        assert_eq!(temp_roots, vec![expected.clone(), expected]);
    }

    #[test]
    fn permission_profile_workspace_root_uses_runtime_workspace_roots() {
        let tmp = TempDir::new().expect("tempdir");
        let workspace_root = tmp.path().join("workspace");
        let command_cwd = workspace_root.join("subdir");
        std::fs::create_dir_all(&command_cwd).expect("create command cwd");

        let permission_profile = PermissionProfile::Managed {
            file_system: ManagedFileSystemPermissions::Restricted {
                entries: vec![FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::project_roots(/*subpath*/ None),
                    },
                    access: FileSystemAccessMode::Write,
                    missing_path_behavior: None,
                }],
                glob_scan_max_depth: None,
            },
            network: NetworkSandboxPolicy::Restricted,
        };
        let workspace_roots = workspace_roots_for(workspace_root.as_path());
        let permissions =
            ResolvedWindowsSandboxPermissions::try_from_permission_profile_for_workspace_roots(
                &permission_profile,
                workspace_roots.as_slice(),
            )
            .expect("managed permission profile");

        let roots = permissions
            .writable_roots_for_cwd(&command_cwd, &HashMap::new())
            .into_iter()
            .map(|root| root.root)
            .collect::<Vec<_>>();

        assert_eq!(
            roots,
            vec![dunce::canonicalize(&workspace_root).expect("canonical workspace root")]
        );
    }

    #[test]
    fn permission_profile_workspace_roots_expand_all_runtime_workspace_roots() {
        let tmp = TempDir::new().expect("tempdir");
        let first = AbsolutePathBuf::from_absolute_path(tmp.path().join("first"))
            .expect("absolute first root");
        let second = AbsolutePathBuf::from_absolute_path(tmp.path().join("second"))
            .expect("absolute second root");
        let permission_profile = PermissionProfile::Managed {
            file_system: ManagedFileSystemPermissions::Restricted {
                entries: vec![
                    FileSystemSandboxEntry {
                        path: FileSystemPath::Special {
                            value: FileSystemSpecialPath::project_roots(/*subpath*/ None),
                        },
                        access: FileSystemAccessMode::Write,
                        missing_path_behavior: None,
                    },
                    FileSystemSandboxEntry {
                        path: FileSystemPath::Special {
                            value: FileSystemSpecialPath::project_roots(Some(".git".into())),
                        },
                        access: FileSystemAccessMode::Deny,
                        missing_path_behavior: None,
                    },
                    FileSystemSandboxEntry {
                        path: FileSystemPath::GlobPattern {
                            scan: None,
                            pattern: project_roots_glob_pattern(Path::new("**/*.env")),
                        },
                        access: FileSystemAccessMode::Deny,
                        missing_path_behavior: None,
                    },
                ],
                glob_scan_max_depth: None,
            },
            network: NetworkSandboxPolicy::Restricted,
        };

        let permissions =
            ResolvedWindowsSandboxPermissions::try_from_permission_profile_for_workspace_roots(
                &permission_profile,
                &[first.clone(), second.clone()],
            )
            .expect("managed permission profile");

        assert_eq!(
            permissions.file_system,
            FileSystemSandboxPolicy::restricted(vec![
                FileSystemSandboxEntry {
                    path: FileSystemPath::Path {
                        path: first.clone(),
                    },
                    access: FileSystemAccessMode::Write,
                    missing_path_behavior: None,
                },
                FileSystemSandboxEntry {
                    path: FileSystemPath::Path {
                        path: second.clone(),
                    },
                    access: FileSystemAccessMode::Write,
                    missing_path_behavior: None,
                },
                FileSystemSandboxEntry {
                    path: FileSystemPath::Path {
                        path: first.join(".git"),
                    },
                    access: FileSystemAccessMode::Deny,
                    missing_path_behavior: None,
                },
                FileSystemSandboxEntry {
                    path: FileSystemPath::Path {
                        path: second.join(".git"),
                    },
                    access: FileSystemAccessMode::Deny,
                    missing_path_behavior: None,
                },
                FileSystemSandboxEntry {
                    path: FileSystemPath::GlobPattern {
                        scan: None,
                        pattern: AbsolutePathBuf::resolve_path_against_base(
                            "**/*.env",
                            first.as_path(),
                        )
                        .to_string_lossy()
                        .into_owned(),
                    },
                    access: FileSystemAccessMode::Deny,
                    missing_path_behavior: None,
                },
                FileSystemSandboxEntry {
                    path: FileSystemPath::GlobPattern {
                        scan: None,
                        pattern: AbsolutePathBuf::resolve_path_against_base(
                            "**/*.env",
                            second.as_path(),
                        )
                        .to_string_lossy()
                        .into_owned(),
                    },
                    access: FileSystemAccessMode::Deny,
                    missing_path_behavior: None,
                },
            ])
        );
    }

    #[test]
    fn token_mode_for_profile_without_writable_roots_uses_readonly_capability() {
        let tmp = TempDir::new().expect("tempdir");
        let cwd = tmp.path().join("workspace");
        std::fs::create_dir_all(&cwd).expect("create cwd");
        let workspace_roots = workspace_roots_for(cwd.as_path());

        let token_mode = token_mode_for_permission_profile(
            &PermissionProfile::read_only(),
            workspace_roots.as_slice(),
            &cwd,
            &HashMap::new(),
        )
        .expect("token mode");

        assert_eq!(WindowsSandboxTokenMode::ReadOnlyCapability, token_mode);
    }

    #[test]
    fn token_mode_for_profile_with_writable_roots_uses_write_capabilities() {
        let tmp = TempDir::new().expect("tempdir");
        let cwd = tmp.path().join("workspace");
        std::fs::create_dir_all(&cwd).expect("create cwd");
        let workspace_roots = workspace_roots_for(cwd.as_path());

        let token_mode = token_mode_for_permission_profile(
            &PermissionProfile::workspace_write(),
            workspace_roots.as_slice(),
            &cwd,
            &HashMap::new(),
        )
        .expect("token mode");

        assert_eq!(WindowsSandboxTokenMode::WritableRootsCapability, token_mode);
    }

    #[test]
    fn permission_profile_rejects_disabled_profiles() {
        let err = ResolvedWindowsSandboxPermissions::try_from_permission_profile(
            &PermissionProfile::Disabled,
        )
        .expect_err("disabled profile should not resolve for sandbox enforcement");

        assert!(
            err.to_string()
                .contains("only managed permission profiles can be enforced")
        );
    }

    #[test]
    fn permission_profile_rejects_unrestricted_managed_filesystem() {
        let permission_profile = PermissionProfile::Managed {
            file_system: ManagedFileSystemPermissions::Unrestricted,
            network: NetworkSandboxPolicy::Restricted,
        };

        let err =
            ResolvedWindowsSandboxPermissions::try_from_permission_profile(&permission_profile)
                .expect_err("unrestricted profile should not resolve for sandbox enforcement");

        assert!(
            err.to_string()
                .contains("only restricted managed filesystem permissions can be enforced")
        );
    }

    #[test]
    fn token_mode_rejects_full_disk_write_entries() {
        let tmp = TempDir::new().expect("tempdir");
        let cwd = tmp.path().join("workspace");
        std::fs::create_dir_all(&cwd).expect("create cwd");
        let permission_profile = PermissionProfile::Managed {
            file_system: ManagedFileSystemPermissions::Restricted {
                entries: vec![FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::Root,
                    },
                    access: FileSystemAccessMode::Write,
                    missing_path_behavior: None,
                }],
                glob_scan_max_depth: None,
            },
            network: NetworkSandboxPolicy::Restricted,
        };
        let workspace_roots = workspace_roots_for(cwd.as_path());

        let err = token_mode_for_permission_profile(
            &permission_profile,
            workspace_roots.as_slice(),
            &cwd,
            &HashMap::new(),
        )
        .expect_err("full disk writes should not resolve to a token mode");

        assert!(
            err.to_string()
                .contains("full-disk filesystem writes, which cannot be enforced")
        );
    }
}
