//! Bubblewrap-based filesystem sandboxing for Linux.
//!
//! This module mirrors the semantics used by the macOS Seatbelt sandbox:
//! - the filesystem is read-only by default,
//! - explicit writable roots are layered on top, and
//! - sensitive subpaths such as `.git`, `.agents`, and `.codex` remain
//!   read-only even when their parent root is writable.
//!
//! The overall Linux sandbox is composed of:
//! - seccomp + `PR_SET_NO_NEW_PRIVS` applied in-process, and
//! - bubblewrap used to construct the filesystem view before exec.
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::fs::File;
use std::fs::Metadata;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use crate::linux_run_main::RegistryLock;
use crate::linux_run_main::synthetic_mount_registry_root;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;
use codex_protocol::permissions::FileSystemDenyGlob;
use codex_protocol::permissions::PinnedWritableRoot;
use codex_protocol::permissions::credential_glob_exempts_templates;
use codex_protocol::permissions::credential_rule_roots;
use codex_protocol::permissions::is_credential_template_name;
use codex_protocol::permissions::is_protected_metadata_name;
use codex_protocol::protocol::FileSystemAccessMode;
use codex_protocol::protocol::FileSystemPath;
use codex_protocol::protocol::FileSystemSandboxPolicy;
use codex_protocol::protocol::FileSystemSpecialPath;
use codex_protocol::protocol::WritableRoot;
use codex_utils_absolute_path::AbsolutePathBuf;
use globset::GlobBuilder;
use globset::GlobSet;
use globset::GlobSetBuilder;

/// Linux "platform defaults" that keep common system binaries and dynamic
/// libraries readable when a split filesystem policy requests `:minimal`.
///
/// These are intentionally system-level paths only (plus Nix store roots) so
/// `include_platform_defaults` does not silently widen access to user data.
const LINUX_PLATFORM_DEFAULT_READ_ROOTS: &[&str] = &[
    "/bin",
    "/sbin",
    "/usr",
    "/etc",
    "/lib",
    "/lib64",
    "/nix/store",
    "/run/current-system/sw",
];

const MAX_UNREADABLE_GLOB_MATCHES: usize = 8192;

/// #536: the writable roots the caller pinned (`--pinned-writable-roots`).
/// Every writable root is then bound from the folder this helper opened and
/// checked at its pinned location (`--bind-fd`), never by path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WritableRootPins {
    pub(crate) roots: &'static [PinnedWritableRoot],
    /// Whether the bubblewrap that runs supports `--bind-fd` (0.8+).
    pub(crate) bind_fd: bool,
}

/// Options that control how bubblewrap is invoked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BwrapOptions {
    /// Whether to mount a fresh `/proc` inside the sandbox.
    ///
    /// This is the secure default, but some restrictive container environments
    /// deny `--proc /proc`.
    pub mount_proc: bool,
    /// How networking should be configured inside the bubblewrap sandbox.
    pub network_mode: BwrapNetworkMode,
    /// Optional maximum depth for expanding unreadable glob patterns with ripgrep.
    ///
    /// Keep this uncapped by default so existing nested deny-read matches are
    /// masked before the sandboxed command starts.
    pub glob_scan_max_depth: Option<usize>,
    /// #536: the caller's pinned writable roots, for a session's command.
    pub pins: Option<WritableRootPins>,
}

impl Default for BwrapOptions {
    fn default() -> Self {
        Self {
            mount_proc: true,
            network_mode: BwrapNetworkMode::FullAccess,
            glob_scan_max_depth: None,
            pins: None,
        }
    }
}

/// Network policy modes for bubblewrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum BwrapNetworkMode {
    /// Keep access to the host network namespace.
    #[default]
    FullAccess,
    /// Remove access to the host network namespace.
    Isolated,
    /// Intended proxy-only mode.
    ///
    /// Bubblewrap enforces this by unsharing the network namespace. The
    /// proxy-routing bridge is established by the helper process after startup.
    ProxyOnly,
}

impl BwrapNetworkMode {
    fn should_unshare_network(self) -> bool {
        !matches!(self, Self::FullAccess)
    }
}

#[derive(Debug)]
pub(crate) struct BwrapArgs {
    pub args: Vec<String>,
    pub preserved_files: Vec<File>,
    pub synthetic_mount_targets: Vec<SyntheticMountTarget>,
    pub protected_create_targets: Vec<ProtectedCreateTarget>,
    /// #673: held from before the paths were looked at until the targets
    /// are registered.
    pub registry_lock: Option<RegistryLock>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    dev: u64,
    ino: u64,
}

impl FileIdentity {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SyntheticMountTargetKind {
    EmptyFile,
    EmptyDirectory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SyntheticMountTarget {
    path: PathBuf,
    kind: SyntheticMountTargetKind,
    // If an empty metadata path was already present, remember its inode so
    // cleanup does not delete a real pre-existing file or directory.
    pre_existing_path: Option<FileIdentity>,
    /// #607: where it is, with the folder above it resolved when the run
    /// registered it (before the command ran). It is removed there, without
    /// following a link, so a folder the command swaps for a link cannot
    /// redirect the removal.
    real_path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProtectedCreateTarget {
    path: PathBuf,
}

impl ProtectedCreateTarget {
    pub(crate) fn missing(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl SyntheticMountTarget {
    pub(crate) fn missing(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: SyntheticMountTargetKind::EmptyFile,
            pre_existing_path: None,
            real_path: None,
        }
    }

    pub(crate) fn missing_empty_directory(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: SyntheticMountTargetKind::EmptyDirectory,
            pre_existing_path: None,
            real_path: None,
        }
    }

    pub(crate) fn existing_empty_file(path: &Path, metadata: &Metadata) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: SyntheticMountTargetKind::EmptyFile,
            pre_existing_path: Some(FileIdentity::from_metadata(metadata)),
            real_path: None,
        }
    }

    pub(crate) fn existing_empty_directory(path: &Path, metadata: &Metadata) -> Self {
        Self {
            path: path.to_path_buf(),
            kind: SyntheticMountTargetKind::EmptyDirectory,
            pre_existing_path: Some(FileIdentity::from_metadata(metadata)),
            real_path: None,
        }
    }

    /// #607: the same target, at `real_path` (see [`Self::removal_path`]).
    pub(crate) fn with_real_path(mut self, real_path: Option<PathBuf>) -> Self {
        self.real_path = real_path;
        self
    }

    /// #607: where it is removed: its location when it was registered, or
    /// its path when that could not be resolved.
    pub(crate) fn removal_path(&self) -> &Path {
        self.real_path.as_deref().unwrap_or(&self.path)
    }

    pub(crate) fn real_path(&self) -> Option<&Path> {
        self.real_path.as_deref()
    }

    pub(crate) fn preserves_pre_existing_path(&self) -> bool {
        self.pre_existing_path.is_some()
    }

    /// #562: whether the file or folder found when the arguments were built
    /// is still the one at the path. Another run's cleanup may have removed
    /// it before this run registered it; the mount target made then is this
    /// run's own placeholder. #607: once registered, a change is the
    /// person's (or a mount target bubblewrap made again after the path
    /// vanished in between, which is not told apart): left in place.
    pub(crate) fn pre_existing_path_is_unchanged(&self) -> bool {
        let Some(identity) = self.pre_existing_path else {
            return true;
        };
        fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| FileIdentity::from_metadata(&metadata) == identity)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn kind(&self) -> SyntheticMountTargetKind {
        self.kind
    }

    /// Whether what is at the path after bubblewrap ran is still this
    /// placeholder: an empty file, or a folder (`empty_file`, `dir`), and not
    /// the pre-existing path found when the arguments were built (`identity`
    /// is its `(dev, ino)`).
    pub(crate) fn should_remove(&self, empty_file: bool, dir: bool, identity: (u64, u64)) -> bool {
        match self.kind {
            SyntheticMountTargetKind::EmptyFile => {
                if !empty_file {
                    return false;
                }
            }
            SyntheticMountTargetKind::EmptyDirectory => {
                if !dir {
                    return false;
                }
            }
        }

        match self.pre_existing_path {
            Some(pre_existing_path) => {
                pre_existing_path
                    != FileIdentity {
                        dev: identity.0,
                        ino: identity.1,
                    }
            }
            None => true,
        }
    }
}

/// Wrap a command with bubblewrap so the filesystem is read-only by default,
/// with explicit writable roots and read-only subpaths layered afterward.
///
/// When the policy grants full disk write access and full network access, this
/// returns `command` unchanged so we avoid unnecessary sandboxing overhead.
/// If network isolation is requested, we still wrap with bubblewrap so network
/// namespace restrictions apply while preserving full filesystem access, except
/// that `/dev` is a fresh minimal device tree (#498).
pub(crate) fn create_bwrap_command_args(
    command: Vec<String>,
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
    sandbox_policy_cwd: &Path,
    command_cwd: &Path,
    options: BwrapOptions,
) -> Result<BwrapArgs> {
    let unreadable_globs =
        file_system_sandbox_policy.get_unreadable_glob_scans_with_cwd(sandbox_policy_cwd);
    // Full disk write normally skips bwrap, but unreadable glob patterns still
    // need concrete bwrap masks for the matches expanded below, an
    // unlistable directory (#506) its own mount, and a path that may not be
    // moved (#598) its pins.
    if file_system_sandbox_policy.has_full_disk_write_access()
        && unreadable_globs.is_empty()
        && file_system_sandbox_policy
            .get_unlistable_directories()
            .is_empty()
        && file_system_sandbox_policy.get_unmovable_paths().is_empty()
    {
        return if options.network_mode == BwrapNetworkMode::FullAccess {
            Ok(BwrapArgs {
                args: command,
                preserved_files: Vec::new(),
                synthetic_mount_targets: Vec::new(),
                protected_create_targets: Vec::new(),
                registry_lock: None,
            })
        } else {
            Ok(create_bwrap_flags_full_filesystem(command, options))
        };
    }

    create_bwrap_flags(
        command,
        file_system_sandbox_policy,
        sandbox_policy_cwd,
        command_cwd,
        options,
    )
}

fn create_bwrap_flags_full_filesystem(command: Vec<String>, options: BwrapOptions) -> BwrapArgs {
    let mut args = vec![
        "--new-session".to_string(),
        "--die-with-parent".to_string(),
        "--bind".to_string(),
        "/".to_string(),
        "/".to_string(),
        // #498: bubblewrap binds `nodev`, so the host's `/dev/null` cannot be
        // opened under `--bind / /`. Mount a fresh minimal `/dev` (#493).
        "--dev".to_string(),
        "/dev".to_string(),
        // Always enter a fresh user namespace so root inside a container does
        // not need ambient CAP_SYS_ADMIN to create the remaining namespaces.
        "--unshare-user".to_string(),
        "--unshare-pid".to_string(),
    ];
    if options.network_mode.should_unshare_network() {
        args.push("--unshare-net".to_string());
    }
    if options.mount_proc {
        args.push("--proc".to_string());
        args.push("/proc".to_string());
    }
    args.push("--".to_string());
    args.extend(command);
    BwrapArgs {
        args,
        preserved_files: Vec::new(),
        synthetic_mount_targets: Vec::new(),
        protected_create_targets: Vec::new(),
        registry_lock: None,
    }
}

/// Build the bubblewrap flags (everything after `argv[0]`).
fn create_bwrap_flags(
    command: Vec<String>,
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
    sandbox_policy_cwd: &Path,
    command_cwd: &Path,
    options: BwrapOptions,
) -> Result<BwrapArgs> {
    let BwrapArgs {
        args: filesystem_args,
        preserved_files,
        synthetic_mount_targets,
        protected_create_targets,
        registry_lock,
    } = create_filesystem_args_with_pins(
        file_system_sandbox_policy,
        sandbox_policy_cwd,
        options
            .glob_scan_max_depth
            .or(file_system_sandbox_policy.glob_scan_max_depth),
        options.pins,
    )?;
    let normalized_command_cwd = normalize_command_cwd_for_bwrap(command_cwd);
    let mut args = Vec::new();
    args.push("--new-session".to_string());
    args.push("--die-with-parent".to_string());
    args.extend(filesystem_args);
    // Request a user namespace explicitly rather than relying on bubblewrap's
    // auto-enable behavior, which is skipped when the caller runs as uid 0.
    args.push("--unshare-user".to_string());
    args.push("--unshare-pid".to_string());
    if options.network_mode.should_unshare_network() {
        args.push("--unshare-net".to_string());
    }
    // Mount a fresh /proc unless the caller explicitly disables it.
    if options.mount_proc {
        args.push("--proc".to_string());
        args.push("/proc".to_string());
    }
    if normalized_command_cwd.as_path() != command_cwd {
        // Bubblewrap otherwise inherits the helper's logical cwd, which can be
        // a symlink alias that disappears once the sandbox only mounts
        // canonical roots. Enter the canonical command cwd explicitly so
        // relative paths stay aligned with the mounted filesystem view.
        args.push("--chdir".to_string());
        args.push(path_to_string(normalized_command_cwd.as_path()));
    }
    args.push("--".to_string());
    args.extend(command);
    Ok(BwrapArgs {
        args,
        preserved_files,
        synthetic_mount_targets,
        protected_create_targets,
        registry_lock,
    })
}

/// Build the bubblewrap filesystem mounts for a given filesystem policy.
///
/// The mount order is important:
/// 1. Full-read policies, and restricted policies that explicitly read `/`,
///    use `--ro-bind / /`; other restricted-read policies start from
///    `--tmpfs /` and layer scoped `--ro-bind` mounts.
/// 2. `--dev /dev` mounts a minimal writable `/dev` with standard device nodes
///    (including `/dev/urandom`) even under a read-only root.
/// 3. Unreadable ancestors of writable roots are masked before their child
///    mounts are rebound so nested writable carveouts can be reopened safely.
/// 4. `--bind <root> <root>` re-enables writes for allowed roots, including
///    writable subpaths under `/dev` (for example, `/dev/shm`). A writable
///    root above `/dev` (the `/` of a full-write profile) is followed by a
///    fresh `--dev /dev`: its bind would otherwise cover the minimal `/dev`
///    with the host's, mounted `nodev`, so even `/dev/null` fails (#493).
///    For a session's command, `--bind-fd <checked folder> <root>` (#536).
/// 5. `--ro-bind <subpath> <subpath>` re-applies read-only protections under
///    those writable roots so protected subpaths win.
/// 6. Nested unreadable carveouts under a writable root are masked after that
///    root is bound, and unrelated unreadable roots are masked afterward.
#[cfg(test)]
fn create_filesystem_args(
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
    cwd: &Path,
    glob_scan_max_depth: Option<usize>,
) -> Result<BwrapArgs> {
    create_filesystem_args_with_pins(
        file_system_sandbox_policy,
        cwd,
        glob_scan_max_depth,
        /*pins*/ None,
    )
}

/// [`create_filesystem_args`] for a session's command, whose writable roots
/// the caller pinned (#536).
fn create_filesystem_args_with_pins(
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
    cwd: &Path,
    glob_scan_max_depth: Option<usize>,
    pins: Option<WritableRootPins>,
) -> Result<BwrapArgs> {
    let unreadable_globs = file_system_sandbox_policy.get_unreadable_glob_scans_with_cwd(cwd);
    // #536: for a session's command, no writable root held by another may
    // lead outside every root holding it, including the roots computed here.
    let checked = match pins {
        Some(_) => crate::pinned::check_held_links(file_system_sandbox_policy, cwd)?,
        None => Vec::new(),
    };
    // Bubblewrap requires bind mount targets to exist. Skip missing writable
    // roots so mixed-platform configs can keep harmless paths for other
    // environments without breaking Linux command startup.
    let mut writable_roots = file_system_sandbox_policy
        .get_writable_roots_with_cwd(cwd)
        .into_iter()
        .filter(|writable_root| writable_root.root.as_path().exists())
        .collect::<Vec<_>>();
    if writable_roots.is_empty()
        && file_system_sandbox_policy.has_full_disk_write_access()
        && (!unreadable_globs.is_empty()
            || !file_system_sandbox_policy
                .get_unlistable_directories()
                .is_empty()
            || !file_system_sandbox_policy.get_unmovable_paths().is_empty())
    {
        writable_roots.push(WritableRoot {
            root: AbsolutePathBuf::from_absolute_path("/")?,
            read_only_subpaths: Vec::new(),
            protected_metadata_names: Vec::new(),
        });
    }
    let missing_auto_metadata_read_only_project_root_subpaths: HashSet<PathBuf> =
        file_system_sandbox_policy
            .entries
            .iter()
            .filter(|entry| entry.access == FileSystemAccessMode::Read)
            .filter_map(|entry| {
                let subpath = match &entry.path {
                    FileSystemPath::Special {
                        value:
                            FileSystemSpecialPath::ProjectRoots {
                                subpath: Some(subpath),
                            },
                    } => subpath,
                    _ => return None,
                };
                // Automatic repo-metadata read masks are skipped here so the
                // metadata handling below can apply the root-scoped
                // protection consistently for `.git`, `.agents`, and `.codex`.
                // User-authored `read` rules for other subpaths and `none`
                // rules should keep their normal bwrap behavior, which can mask
                // the first missing component to prevent creation under writable
                // roots.
                let project_subpath = Path::new(subpath);
                if project_subpath != Path::new(".git")
                    && project_subpath != Path::new(".agents")
                    && project_subpath != Path::new(".codex")
                {
                    return None;
                }
                let resolved = AbsolutePathBuf::resolve_path_against_base(subpath, cwd);
                (!resolved.as_path().exists()).then(|| resolved.into_path_buf())
            })
            .collect();
    let mut unreadable_roots = file_system_sandbox_policy
        .get_unreadable_roots_with_cwd(cwd)
        .into_iter()
        .map(AbsolutePathBuf::into_path_buf)
        .collect::<Vec<_>>();
    // Bubblewrap can only mask concrete paths. Expand unreadable glob patterns
    // to the existing matches we can see before constructing the mount overlay;
    // core tool helpers still evaluate the original patterns directly at read time.
    unreadable_roots.extend(
        expand_unreadable_globs(&unreadable_globs, cwd, glob_scan_max_depth, ripgrep_files)?
            .into_iter()
            .map(AbsolutePathBuf::into_path_buf),
    );
    unreadable_roots.sort();
    unreadable_roots.dedup();
    // #673: from here on, whether a path exists and is another run's
    // placeholder decides how it is mounted; no placeholder is removed until
    // this run has registered those it found.
    let registry_lock = RegistryLock::acquire();

    // Read-only roots reached through a link and mounted at the link's path
    // (restricted reads), with their real targets (#501).
    let mut logical_link_roots: Vec<(PathBuf, PathBuf)> = Vec::new();
    let args = if file_system_sandbox_policy.has_full_disk_read_access() {
        // Read-only root, then mount a minimal device tree.
        // In bubblewrap (`bubblewrap.c`, `SETUP_MOUNT_DEV`), `--dev /dev`
        // creates the standard minimal nodes: null, zero, full, random,
        // urandom, and tty. `/dev` must be mounted before writable roots so
        // explicit `/dev/*` writable binds remain visible.
        vec![
            "--ro-bind".to_string(),
            "/".to_string(),
            "/".to_string(),
            "--dev".to_string(),
            "/dev".to_string(),
        ]
    } else {
        // Start from an empty filesystem and add only the approved readable
        // roots plus a minimal `/dev`.
        let mut args = vec![
            "--tmpfs".to_string(),
            "/".to_string(),
            "--dev".to_string(),
            "/dev".to_string(),
        ];

        let mut readable_roots: BTreeSet<PathBuf> = file_system_sandbox_policy
            .get_readable_roots_with_cwd(cwd)
            .into_iter()
            .map(PathBuf::from)
            .collect();
        if file_system_sandbox_policy.include_platform_defaults() {
            readable_roots.extend(
                LINUX_PLATFORM_DEFAULT_READ_ROOTS
                    .iter()
                    .map(|path| PathBuf::from(*path))
                    .filter(|path| path.exists()),
            );
        }

        // A restricted policy can still explicitly request `/`, which is
        // the broad read baseline. Explicit unreadable carveouts are
        // re-applied later.
        if readable_roots.iter().any(|root| root == Path::new("/")) {
            args = vec![
                "--ro-bind".to_string(),
                "/".to_string(),
                "/".to_string(),
                "--dev".to_string(),
                "/dev".to_string(),
            ];
        } else {
            for root in readable_roots {
                if !root.exists() {
                    continue;
                }
                // Writable roots are rebound by real target below; mirror that
                // for their restricted-read bootstrap mount. Plain read-only
                // roots must stay logical because callers may execute those
                // paths inside bwrap, such as Bazel runfiles helper binaries.
                let mount_root = if writable_roots
                    .iter()
                    .any(|writable_root| root.starts_with(writable_root.root.as_path()))
                {
                    canonical_target_if_symlinked_path(&root).unwrap_or(root)
                } else {
                    if let Some(target) = canonical_target_if_symlinked_path(&root) {
                        logical_link_roots.push((root.clone(), target));
                    }
                    root
                };
                args.push("--ro-bind".to_string());
                args.push(path_to_string(&mount_root));
                args.push(path_to_string(&mount_root));
            }
        }

        args
    };
    let mut bwrap_args = BwrapArgs {
        args,
        preserved_files: Vec::new(),
        synthetic_mount_targets: Vec::new(),
        protected_create_targets: Vec::new(),
        registry_lock,
    };
    let mut allowed_write_paths = Vec::with_capacity(writable_roots.len());
    for writable_root in &writable_roots {
        let root = writable_root.root.as_path();
        allowed_write_paths.push(root.to_path_buf());
        if let Some(target) = canonical_target_if_symlinked_path(root) {
            allowed_write_paths.push(target);
        }
    }
    let mut unreadable_paths: HashSet<PathBuf> = unreadable_roots.iter().cloned().collect();
    // #502: a denial spelled through a link the command cannot change is
    // masked where the link leads. Bubblewrap cannot create a mount point
    // through an absolute link (every command would fail), and inside the
    // sandbox the link leads to the same masked file. Links a command could
    // change still fail closed below.
    // #501: under restricted reads, a read-only root reached through a link
    // is mounted at the link's path, so a denial below its target is masked
    // there too.
    let unreadable_roots = {
        let mut resolved = Vec::with_capacity(unreadable_roots.len());
        for path in unreadable_roots {
            let path = if first_writable_symlink_component_in_path(&path, &allowed_write_paths)
                .is_none()
                && let Some(real) = real_path_through_links(&path, &allowed_write_paths)
            {
                real
            } else {
                path
            };
            for (root, target) in &logical_link_roots {
                if target.starts_with(&path) {
                    // The denial holds the whole linked root.
                    resolved.push(root.clone());
                } else if let Ok(relative) = path.strip_prefix(target) {
                    resolved.push(root.join(relative));
                }
            }
            resolved.push(path);
        }
        resolved.sort();
        resolved.dedup();
        // A denial inside a denied folder that is masked whole, with no
        // writable root between them, is already hidden; bubblewrap could
        // not create its mount point in the folder's read-only mask.
        let hidden: Vec<PathBuf> = resolved
            .iter()
            .filter(|path| {
                resolved.iter().any(|folder| {
                    *path != folder
                        && path.starts_with(folder)
                        && folder.is_dir()
                        && !allowed_write_paths
                            .iter()
                            .any(|root| root.starts_with(folder) && path.starts_with(root))
                })
            })
            .cloned()
            .collect();
        resolved.retain(|path| !hidden.contains(path));
        unreadable_paths.extend(resolved.iter().cloned());
        unreadable_paths.extend(hidden);
        resolved
    };
    // #503: order by the bound (canonical) path, so a symlink to `/` is bound
    // first like `/` itself instead of covering shallower roots' masks. The
    // target is resolved once, so the order and the binds agree.
    let mut sorted_writable_roots: Vec<(WritableRoot, Option<PathBuf>)> = writable_roots
        .into_iter()
        .map(|writable_root| {
            let symlink_target = canonical_target_if_symlinked_path(writable_root.root.as_path());
            (writable_root, symlink_target)
        })
        .collect();
    sorted_writable_roots.sort_by_key(|(writable_root, symlink_target)| {
        path_depth(
            symlink_target
                .as_deref()
                .unwrap_or(writable_root.root.as_path()),
        )
    });
    // #506: each unlistable directory is mounted in the pass that exposes it:
    // the last writable-root pass (in mount order) above it, or the read
    // baseline.
    let mount_roots: Vec<PathBuf> = sorted_writable_roots
        .iter()
        .map(|(writable_root, symlink_target)| {
            symlink_target
                .clone()
                .unwrap_or_else(|| writable_root.root.to_path_buf())
        })
        .collect();
    let unlistable_directories = unlistable_directories_by_pass(
        file_system_sandbox_policy,
        cwd,
        &mount_roots,
        &unreadable_roots,
    );
    let mut unlisted = Vec::new();
    for unlistable in &unlistable_directories {
        if unlistable.pass.is_none() {
            append_unlistable_directory_args(
                &mut bwrap_args.args,
                unlistable,
                /*writable*/ false,
            )?;
            unlisted.push(unlistable.target.clone());
        }
    }
    // Mask only the unreadable ancestors that sit outside every writable root.
    // Unreadable paths nested under a broader writable root are applied after
    // that broader root is bound, then reopened by any deeper writable child.
    let mut unreadable_ancestors_of_writable_roots: Vec<PathBuf> = unreadable_roots
        .iter()
        .filter(|path| {
            let unreadable_root = path.as_path();
            !allowed_write_paths
                .iter()
                .any(|root| unreadable_root.starts_with(root))
                && allowed_write_paths
                    .iter()
                    .any(|root| root.starts_with(unreadable_root))
        })
        .cloned()
        .collect();
    unreadable_ancestors_of_writable_roots.sort_by_key(|path| path_depth(path));

    for unreadable_root in &unreadable_ancestors_of_writable_roots {
        append_unreadable_root_args(&mut bwrap_args, unreadable_root, &allowed_write_paths)?;
    }

    // #587: once the policy denies reads (as Seatbelt's rename guard), the
    // folders above each protected path inside a writable root are pinned.
    let pin_protected_folders = !unreadable_roots.is_empty();
    // #594/#598: and, whatever else the policy holds, each path it says may
    // not be moved and the folders above it.
    let unmovable = unmovable_pin_targets(file_system_sandbox_policy);
    // `:root = write` (full access with denials), not a project at `/`.
    let full_write_root = file_system_sandbox_policy.entries.iter().any(|entry| {
        entry.access == FileSystemAccessMode::Write
            && matches!(
                entry.path,
                FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root
                }
            )
    });
    // #503: read-only paths applied by earlier passes, by bound path. A later
    // pass whose bind covers them (two roots bound at the same directory, or a
    // link whose target lies inside an earlier root) applies them again.
    let mut earlier_read_only_subpaths: Vec<PathBuf> = Vec::new();
    // #503: the same for denials, in the form an earlier pass masked them
    // (a denial spelled through a link is masked at the link's target).
    let mut earlier_unreadable_roots: Vec<PathBuf> = Vec::new();
    for (writable_root, symlink_target) in &sorted_writable_roots {
        let root = writable_root.root.as_path();
        let mount_root = symlink_target.as_deref().unwrap_or(root);
        // #473: placeholders are deduplicated within this pass only. Its
        // `--bind` puts the live host folder back over every mount an earlier
        // (broader) pass made below it.
        let earlier_synthetic_mount_targets =
            std::mem::take(&mut bwrap_args.synthetic_mount_targets);
        // If a denied ancestor was already masked, recreate any missing mount
        // target parents before binding the narrower writable descendant.
        if let Some(masking_root) = unreadable_roots
            .iter()
            .map(PathBuf::as_path)
            .filter(|unreadable_root| mount_root.starts_with(unreadable_root))
            .max_by_key(|unreadable_root| path_depth(unreadable_root))
        {
            append_mount_target_parent_dir_args(&mut bwrap_args.args, mount_root, masking_root);
        }

        // #536: for a session's command, every writable root is bound from
        // the folder opened and checked at its pinned location.
        match pins {
            Some(pins) => {
                if !pins.bind_fd {
                    return Err(CodexErr::UnsupportedOperation(
                        "Refused to run the command: binding a checked writable folder needs \
                         bubblewrap 0.8 or newer (`--bind-fd`), and none was found."
                            .to_string(),
                    ));
                }
                let real =
                    crate::pinned::expected_location(pins.roots, &checked, &writable_root.root);
                let file = crate::pinned::open_at(&[root, mount_root], &real)?;
                bwrap_args.args.push("--bind-fd".to_string());
                bwrap_args.args.push(file.as_raw_fd().to_string());
                bwrap_args.args.push(path_to_string(mount_root));
                bwrap_args.preserved_files.push(file);
            }
            None => {
                bwrap_args.args.push("--bind".to_string());
                bwrap_args.args.push(path_to_string(mount_root));
                bwrap_args.args.push(path_to_string(mount_root));
            }
        }
        // #493: restore the minimal `/dev` (step 2) before the deny masks of
        // this pass and before deeper `/dev/*` writable roots (sorted later).
        let dev = Path::new("/dev");
        if dev.starts_with(mount_root) && mount_root != dev {
            bwrap_args.args.push("--dev".to_string());
            bwrap_args.args.push("/dev".to_string());
        }

        let mut read_only_subpaths: Vec<PathBuf> = writable_root
            .read_only_subpaths
            .iter()
            .map(|path| path.as_path().to_path_buf())
            .filter(|path| !unreadable_paths.contains(path))
            .filter(|path| !missing_auto_metadata_read_only_project_root_subpaths.contains(path))
            // The filesystem root of a full-write profile with denials (PF-23
            // protected paths under full access) is not a project root, and
            // an unprivileged bwrap cannot create a mount point there:
            // `mkdir /.git` or `/.codex` would fail every command.
            .filter(|path| {
                !(full_write_root && mount_root.parent().is_none() && path.parent() == Some(root))
                    || path.exists()
            })
            .collect();
        let protected_metadata_names = if mount_root.parent().is_none() && full_write_root {
            Vec::new()
        } else {
            writable_root.protected_metadata_names.clone()
        };
        append_metadata_path_masks_for_writable_root(
            &mut read_only_subpaths,
            root,
            &protected_metadata_names,
        );
        if let Some(target) = symlink_target {
            read_only_subpaths = remap_paths_for_symlink_target(read_only_subpaths, root, target);
        }
        append_protected_create_targets_for_writable_root(
            &mut bwrap_args,
            &protected_metadata_names,
            root,
            symlink_target.as_deref(),
            &read_only_subpaths,
        );
        // A link's own bound path stays read-only when an earlier root made
        // it read-only (for example a link to a workspace's `.git`).
        for path in &earlier_read_only_subpaths {
            if path.starts_with(mount_root)
                && (symlink_target.is_some() || path.as_path() != mount_root)
                && !read_only_subpaths.contains(path)
            {
                read_only_subpaths.push(path.clone());
            }
        }
        for path in &read_only_subpaths {
            if !earlier_read_only_subpaths.contains(path) {
                earlier_read_only_subpaths.push(path.clone());
            }
        }
        read_only_subpaths.sort_by_key(|path| path_depth(path));
        // A root reached through a link is bound at its target, so a denied
        // path spelled under the target is nested too (#488); the rootless
        // pass below skips it because the target is an allowed write path.
        let mut nested_unreadable_roots: Vec<PathBuf> = unreadable_roots
            .iter()
            .filter(|path| {
                path.starts_with(root)
                    || symlink_target
                        .as_ref()
                        .is_some_and(|target| path.starts_with(target))
            })
            .cloned()
            .collect();
        if let Some(target) = symlink_target {
            nested_unreadable_roots =
                remap_paths_for_symlink_target(nested_unreadable_roots, root, target);
        }
        nested_unreadable_roots.extend(
            earlier_unreadable_roots
                .iter()
                .filter(|path| path.starts_with(mount_root))
                .cloned(),
        );
        nested_unreadable_roots.sort();
        nested_unreadable_roots.dedup();
        for path in &nested_unreadable_roots {
            if !earlier_unreadable_roots.contains(path) {
                earlier_unreadable_roots.push(path.clone());
            }
        }
        nested_unreadable_roots.sort_by_key(|path| path_depth(path));
        // #473: denied paths missing below a read-only folder, by the folder
        // that gets a snapshot (see `append_read_only_snapshot_dir_args`).
        let mut snapshot_dirs: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
        let mut snapshot_covered: HashSet<PathBuf> = HashSet::new();
        for unreadable_root in &nested_unreadable_roots {
            if let Some(missing) = missing_component_below_read_only_path(
                unreadable_root,
                &read_only_subpaths,
                &allowed_write_paths,
            ) && let Some(dir) = missing.parent()
            {
                snapshot_dirs
                    .entry(dir.to_path_buf())
                    .or_default()
                    .push(missing.clone());
                snapshot_covered.insert(unreadable_root.clone());
            }
        }
        // #506: this pass's unlistable directories go among its read-only
        // mounts so that none of those binds the real folder back over them:
        // after every read-only folder at or above them and before the ones
        // below (deny masks come later still); after a snapshot folder above
        // them. A snapshot of the folder itself is made unlistable instead.
        // In writable passes an unlistable folder's target is its real
        // location, where the pass's own mounts are.
        let pass_unlistable: Vec<&Unlistable> = unlistable_directories
            .iter()
            .filter(|unlistable| unlistable.pass.as_deref() == Some(mount_root))
            .collect();
        let under_snapshot = |directory: &Path| {
            snapshot_dirs
                .keys()
                .any(|dir| directory.starts_with(dir) && directory != dir)
        };
        let mut pending_unlistable: Vec<(&Unlistable, bool)> = pass_unlistable
            .iter()
            .filter(|unlistable| {
                !snapshot_dirs.contains_key(&unlistable.target)
                    && !under_snapshot(&unlistable.target)
            })
            .map(|unlistable| {
                let read_only = read_only_subpaths
                    .iter()
                    .any(|read_only| unlistable.target.starts_with(read_only));
                (*unlistable, read_only)
            })
            .collect();
        if pin_protected_folders || !unmovable.is_empty() {
            let protected: Vec<&Path> = read_only_subpaths
                .iter()
                .chain(&nested_unreadable_roots)
                .map(PathBuf::as_path)
                .chain(
                    pass_unlistable
                        .iter()
                        .map(|unlistable| unlistable.target.as_path()),
                )
                .collect();
            append_protected_folder_pins(
                &mut bwrap_args,
                mount_root,
                &protected,
                pin_protected_folders,
                &unmovable,
                &allowed_write_paths,
            );
        }
        for subpath in &read_only_subpaths {
            let depth = path_depth(subpath);
            for (unlistable, read_only) in pending_unlistable
                .extract_if(.., |(unlistable, _)| depth > path_depth(&unlistable.target))
            {
                append_unlistable_directory_args(&mut bwrap_args.args, unlistable, !read_only)?;
                unlisted.push(unlistable.target.clone());
            }
            if missing_component_below_read_only_path(
                subpath,
                &read_only_subpaths,
                &allowed_write_paths,
            )
            .is_some()
            {
                // #473: the read-only folder above already stops the command
                // creating it, and bubblewrap cannot create a mount target
                // in a read-only mount.
                continue;
            }
            append_read_only_subpath_args(&mut bwrap_args, subpath, &allowed_write_paths)?;
        }
        for (unlistable, read_only) in pending_unlistable {
            append_unlistable_directory_args(&mut bwrap_args.args, unlistable, !read_only)?;
            unlisted.push(unlistable.target.clone());
        }
        let after_snapshots: Vec<&Unlistable> = pass_unlistable
            .iter()
            .filter(|unlistable| under_snapshot(&unlistable.target))
            .copied()
            .collect();
        let mut snapshot_dirs: Vec<(PathBuf, Vec<PathBuf>)> = snapshot_dirs.into_iter().collect();
        snapshot_dirs.sort_by_key(|(dir, _)| path_depth(dir));
        for (dir, hidden) in &snapshot_dirs {
            let unlistable = pass_unlistable
                .iter()
                .any(|unlistable| unlistable.target == *dir);
            append_read_only_snapshot_dir_args(
                &mut bwrap_args,
                dir,
                hidden,
                if unlistable { "111" } else { "555" },
            )?;
            if unlistable {
                unlisted.push(dir.clone());
            }
        }
        // Inside a read-only snapshot: read-only too.
        for unlistable in after_snapshots {
            append_unlistable_directory_args(
                &mut bwrap_args.args,
                unlistable,
                /*writable*/ false,
            )?;
            unlisted.push(unlistable.target.clone());
        }
        // Protect the registry only where a writable bind exposes it. Apply
        // this before deny masks so a denied parent stays hidden, rather than
        // trying to recreate the registry beneath an already read-only tmpfs.
        let registry_root = synthetic_mount_registry_root();
        let registry_mount = if registry_root.starts_with(mount_root) {
            Some(registry_root.as_path())
        } else if mount_root.starts_with(&registry_root) {
            Some(mount_root)
        } else {
            None
        };
        if let Some(registry_mount) = registry_mount {
            fs::create_dir_all(&registry_root)?;
            bwrap_args.args.push("--ro-bind".to_string());
            bwrap_args.args.push(path_to_string(registry_mount));
            bwrap_args.args.push(path_to_string(registry_mount));
        }
        // #348: the same for the placeholder record kept in the Corbanu
        // home, so the command cannot forge a record a later run acts on.
        if let Some(record_mount) = crate::placeholder_journal::record_dir_mount(mount_root) {
            bwrap_args.args.push("--ro-bind".to_string());
            bwrap_args.args.push(path_to_string(&record_mount));
            bwrap_args.args.push(path_to_string(&record_mount));
        }
        for unreadable_root in nested_unreadable_roots {
            if snapshot_covered.contains(&unreadable_root) {
                // Hidden by the snapshot above, also if it is created later.
                continue;
            }
            append_unreadable_root_args(&mut bwrap_args, &unreadable_root, &allowed_write_paths)?;
        }
        let pass_synthetic_mount_targets = std::mem::replace(
            &mut bwrap_args.synthetic_mount_targets,
            earlier_synthetic_mount_targets,
        );
        bwrap_args
            .synthetic_mount_targets
            .extend(pass_synthetic_mount_targets);
    }

    let mut rootless_unreadable_roots: Vec<PathBuf> = unreadable_roots
        .iter()
        .filter(|path| {
            let unreadable_root = path.as_path();
            !allowed_write_paths
                .iter()
                .any(|root| unreadable_root.starts_with(root) || root.starts_with(unreadable_root))
        })
        .cloned()
        .collect();
    rootless_unreadable_roots.sort_by_key(|path| path_depth(path));
    for unreadable_root in rootless_unreadable_roots {
        append_unreadable_root_args(&mut bwrap_args, &unreadable_root, &allowed_write_paths)?;
    }
    // #506: last, once every mount target below them exists, so the command
    // cannot change their mode (or add entries) either.
    for directory in unlisted {
        bwrap_args.args.push("--remount-ro".to_string());
        bwrap_args.args.push(path_to_string(&directory));
    }

    Ok(bwrap_args)
}

/// #587: binds each folder between `mount_root` and a protected path onto
/// itself, before the pass's masks. A mount point cannot be renamed or
/// removed (`EBUSY`), so a command cannot move a protected path's folder
/// aside (`mv ~/.corbanu x`, then plant a new `~/.corbanu`) and so leave the
/// rules behind: the new folder would have none, and the moved copy's
/// denied stores would be readable under its new name. Seatbelt denies the
/// same renames (`build_seatbelt_rename_guard_policy`). The bind is the live
/// folder, so what is in it is unchanged; a folder gone by then (another
/// run's placeholder) is skipped (`--bind-try`). Folders at or below a
/// protected path (masked or read-only already), writable roots (bound
/// already), links, folders at or below `/dev` (a bind would put the host's
/// back over the minimal one, #493) and folders in a parent the user
/// neither owns nor can write (they cannot be renamed, such as `/home`) are
/// left out.
///
/// `above_protected`: whether the folders above `protected` are pinned
/// (#587, once the policy denies reads); they are always left out. #594/#598:
/// the folders above each [`UnmovableTarget`] are pinned in any case, and
/// the target itself when it is a folder.
fn append_protected_folder_pins(
    bwrap_args: &mut BwrapArgs,
    mount_root: &Path,
    protected: &[&Path],
    above_protected: bool,
    unmovable: &[UnmovableTarget],
    allowed_write_paths: &[PathBuf],
) {
    let mut folders = BTreeSet::new();
    let sources = protected
        .iter()
        .filter(|_| above_protected)
        .map(|path| (*path, /*itself*/ false))
        .chain(
            unmovable
                .iter()
                .map(|target| (target.path.as_path(), target.pin_itself)),
        );
    for (path, itself) in sources {
        for folder in path.ancestors().skip(usize::from(!itself)) {
            if folder == mount_root || !folder.starts_with(mount_root) {
                break;
            }
            folders.insert(folder);
        }
    }
    let mut pins: Vec<&Path> = folders
        .into_iter()
        .filter(|folder| {
            !protected.iter().any(|path| folder.starts_with(path))
                && !folder.starts_with("/dev")
                && !allowed_write_paths.iter().any(|root| root == folder)
                && fs::symlink_metadata(folder).is_ok_and(|metadata| metadata.is_dir())
                && folder.parent().is_some_and(user_may_change)
        })
        .collect();
    pins.sort_by_key(|folder| path_depth(folder));
    for folder in pins {
        bwrap_args.args.push("--bind-try".to_string());
        bwrap_args.args.push(path_to_string(folder));
        bwrap_args.args.push(path_to_string(folder));
    }
}

/// #594/#598: a location the sandbox keeps in place for a path the policy
/// says may not be moved ([`FileSystemSandboxPolicy::get_unmovable_paths`]).
#[derive(Debug, PartialEq, Eq)]
struct UnmovableTarget {
    /// Real location: of the path, or of a link on its way (the link's own
    /// folder resolved).
    path: PathBuf,
    /// Whether the location itself is pinned (a folder), or only the folders
    /// above it (a link: a mount cannot sit on a link, so a command that can
    /// write the folder holding it can still replace it; Seatbelt keeps it).
    pin_itself: bool,
}

/// #594/#598: the real location of each unmovable path, and of each link on
/// its way (a Corbanu home reached through `~/.corbanu -> ~/dotfiles/x` keeps
/// `~/dotfiles/x`, `~/dotfiles` and the folders above `~/.corbanu`).
fn unmovable_pin_targets(policy: &FileSystemSandboxPolicy) -> Vec<UnmovableTarget> {
    let mut targets: Vec<UnmovableTarget> = Vec::new();
    let mut push = |path: PathBuf, pin_itself: bool| {
        if !targets
            .iter()
            .any(|target| target.path == path && target.pin_itself == pin_itself)
        {
            targets.push(UnmovableTarget { path, pin_itself });
        }
    };
    for path in policy.get_unmovable_paths() {
        let path = path.as_path();
        if let Ok(real) = fs::canonicalize(path) {
            push(real, /*pin_itself*/ true);
        }
        let mut spelled = PathBuf::new();
        for component in path.components() {
            spelled.push(component);
            if fs::symlink_metadata(&spelled)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
                && let (Some(parent), Some(name)) = (spelled.parent(), spelled.file_name())
                && let Ok(parent) = fs::canonicalize(parent)
            {
                push(parent.join(name), /*pin_itself*/ false);
            }
        }
    }
    targets
}

/// Whether the user could rename an entry of `folder`: it can write it, or
/// owns it (and so could make it writable first).
fn user_may_change(folder: &Path) -> bool {
    // SAFETY: `geteuid` has no preconditions.
    let euid = unsafe { libc::geteuid() };
    if fs::metadata(folder).is_ok_and(|metadata| metadata.uid() == euid) {
        return true;
    }
    let Ok(folder) = std::ffi::CString::new(folder.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    // SAFETY: `folder` is a valid NUL-terminated string for the call.
    unsafe { libc::access(folder.as_ptr(), libc::W_OK) == 0 }
}

/// #506: one unlistable directory, as the sandbox shows it.
struct Unlistable {
    /// The mount root of the last writable-root pass (in mount order) that
    /// contains it; `None`: the read baseline exposes it.
    pass: Option<PathBuf>,
    /// Its real location on the host, where its entries are bound from.
    source: PathBuf,
    /// Where the command sees it: the real location, or below a read-only
    /// root bound at its own (logical) spelling.
    target: PathBuf,
}

/// #506: the policy's unlistable directories that exist and the command can
/// see. A directory hidden by a deny mask is left out.
fn unlistable_directories_by_pass(
    file_system_sandbox_policy: &FileSystemSandboxPolicy,
    cwd: &Path,
    mount_roots: &[PathBuf],
    unreadable_roots: &[PathBuf],
) -> Vec<Unlistable> {
    // Restricted-read baselines bind readable roots at their own spelling.
    let readable_roots: Vec<(PathBuf, PathBuf)> = file_system_sandbox_policy
        .get_readable_roots_with_cwd(cwd)
        .into_iter()
        .map(|root| {
            let real = root
                .as_path()
                .canonicalize()
                .unwrap_or_else(|_| root.to_path_buf());
            (root.to_path_buf(), real)
        })
        .collect();
    let mut directories: Vec<Unlistable> = Vec::new();
    for directory in file_system_sandbox_policy.get_unlistable_directories() {
        let Ok(source) = directory.as_path().canonicalize() else {
            continue;
        };
        if !source.is_dir() || directories.iter().any(|known| known.source == source) {
            continue;
        }
        // Passes run in this order; a later one binds over an earlier one.
        let pass = mount_roots
            .iter()
            .rfind(|root| source.starts_with(root))
            .cloned();
        // Hidden by a deny mask no writable root below it reopens.
        let hidden = unreadable_roots.iter().any(|unreadable| {
            source.starts_with(unreadable)
                && !pass
                    .as_ref()
                    .is_some_and(|root| root.starts_with(unreadable))
        });
        if hidden {
            continue;
        }
        let target = if pass.is_some() || file_system_sandbox_policy.has_full_disk_read_access() {
            Some(source.clone())
        } else {
            readable_roots
                .iter()
                .filter(|(_, real)| source.starts_with(real))
                .max_by_key(|(_, real)| path_depth(real))
                .and_then(|(logical, real)| {
                    source.strip_prefix(real).ok().map(|rest| {
                        if rest.as_os_str().is_empty() {
                            logical.clone()
                        } else {
                            logical.join(rest)
                        }
                    })
                })
        };
        if let Some(target) = target {
            directories.push(Unlistable {
                pass,
                source,
                target,
            });
        }
    }
    directories
}

/// #506: replace the directory, where the command sees it, with an empty
/// folder only its owner can pass through (mode 0111: no listing), and put
/// each entry back as it was: links as links, everything else bound from the
/// host. The folder is made read-only at the end (see
/// [`create_filesystem_args`]). A directory that cannot be read fails the
/// command rather than staying listable.
fn append_unlistable_directory_args(
    args: &mut Vec<String>,
    unlistable: &Unlistable,
    writable: bool,
) -> Result<()> {
    let Unlistable { source, target, .. } = unlistable;
    let directory = source.as_path();
    let entries = fs::read_dir(directory)
        .and_then(Iterator::collect::<io::Result<Vec<_>>>)
        .map_err(|err| {
            CodexErr::Fatal(format!(
                "cannot keep {} from being listed: {err}",
                directory.display()
            ))
        })?;
    let mut children: Vec<(std::ffi::OsString, Option<PathBuf>)> = entries
        .into_iter()
        .map(|entry| {
            let link = entry
                .file_type()
                .is_ok_and(|kind| kind.is_symlink())
                .then(|| fs::read_link(entry.path()).ok())
                .flatten();
            (entry.file_name(), link)
        })
        .collect();
    children.sort();
    args.extend([
        "--perms".to_string(),
        "111".to_string(),
        "--tmpfs".to_string(),
        path_to_string(target),
    ]);
    for (name, link) in children {
        let at = path_to_string(&target.join(&name));
        match link {
            Some(link) => args.extend(["--symlink".to_string(), path_to_string(&link), at]),
            None => args.extend([
                if writable {
                    "--bind-try"
                } else {
                    "--ro-bind-try"
                }
                .to_string(),
                path_to_string(&source.join(&name)),
                at,
            ]),
        }
    }
    Ok(())
}

fn append_protected_create_targets_for_writable_root(
    bwrap_args: &mut BwrapArgs,
    protected_metadata_names: &[String],
    root: &Path,
    symlink_target: Option<&Path>,
    read_only_subpaths: &[PathBuf],
) {
    for name in protected_metadata_names {
        let mut path = root.join(name);
        if let Some(target) = symlink_target
            && let Ok(relative_path) = path.strip_prefix(root)
        {
            path = target.join(relative_path);
        }
        if read_only_subpaths.iter().any(|subpath| subpath == &path) || path.exists() {
            continue;
        }
        bwrap_args
            .protected_create_targets
            .push(ProtectedCreateTarget::missing(&path));
    }
}

fn append_metadata_path_masks_for_writable_root(
    read_only_subpaths: &mut Vec<PathBuf>,
    root: &Path,
    protected_metadata_names: &[String],
) {
    for name in protected_metadata_names {
        let path = root.join(name);
        if !read_only_subpaths.iter().any(|subpath| subpath == &path) {
            read_only_subpaths.push(path);
        }
    }
}

type GlobFileScanner = fn(&Path, &[String], Option<usize>) -> Result<Vec<AbsolutePathBuf>>;
/// Search root, credential rule, depth, and whether it is a database-entry
/// scan (#732).
type ScanGroup = (AbsolutePathBuf, bool, Option<usize>, bool);

fn expand_unreadable_globs(
    patterns: &[FileSystemDenyGlob],
    cwd: &Path,
    max_depth: Option<usize>,
    scan_files: GlobFileScanner,
) -> Result<Vec<AbsolutePathBuf>> {
    if patterns.is_empty() || max_depth == Some(0) {
        return Ok(Vec::new());
    }

    // Group each pattern by the static path prefix before its first glob
    // metacharacter. That keeps scans narrow, avoids searching from `/`, and
    // lets one `rg --files` call handle all patterns under the same root.
    // The whole credential rule's globs are scanned apart: their matches
    // leave out the template names (#442, Travis 2026-10-12), which other
    // globs still cover.
    let plain_patterns: Vec<String> = patterns.iter().map(|glob| glob.pattern.clone()).collect();
    let rule_roots = credential_rule_roots(&plain_patterns);
    // #732: depth is part of the group identity. A generic glob sharing this
    // root (even the identical pattern text) retains its own recursive scan.
    // #732 (D226): a database-entry scan lists only its folder's own entries.
    let mut patterns_by_search_root: BTreeMap<ScanGroup, Vec<String>> = BTreeMap::new();
    for pattern in patterns {
        let database_directory = pattern
            .database_entries_directory()
            .map_err(|message| CodexErr::Fatal(message.to_string()))?;
        let scan = match database_directory {
            Some(directory) => Some((directory.clone(), "*.sqlite*".to_string(), Some(1), true)),
            None => split_pattern_for_ripgrep(&pattern.pattern, cwd)
                .map(|(root, glob)| (root, glob, max_depth, false)),
        };
        if let Some((search_root, glob, depth, database)) = scan
            && search_root.as_path().is_dir()
        {
            let credential = credential_glob_exempts_templates(&pattern.pattern, &rule_roots);
            patterns_by_search_root
                .entry((search_root, credential, depth, database))
                .or_default()
                .push(glob);
        }
    }

    // Record both the logical match and any canonical symlink target. The bwrap
    // overlay needs the resolved target to prevent a readable symlink path from
    // bypassing an unreadable glob match.
    let mut expanded_paths = BTreeSet::new();
    for ((search_root, credential, depth, database), globs) in patterns_by_search_root {
        // #732 (D226): ripgrep still opens each subfolder at `--max-depth 1`
        // (ripgrep 14.1), so an unreadable one failed the scan; listing the
        // folder itself never opens a subfolder.
        let scan: GlobFileScanner = if database { glob_files } else { scan_files };
        for path in scan(search_root.as_path(), &globs, depth)? {
            if credential
                && path
                    .as_path()
                    .file_name()
                    .is_some_and(is_credential_template_name)
            {
                continue;
            }
            if let Some(target) = canonical_target_if_symlinked_path(path.as_path()) {
                expanded_paths.insert(AbsolutePathBuf::from_absolute_path_checked(target)?);
            }
            expanded_paths.insert(path);
            if expanded_paths.len() > MAX_UNREADABLE_GLOB_MATCHES {
                return Err(CodexErr::Fatal(format!(
                    "unreadable glob expansion for {} matched more than {MAX_UNREADABLE_GLOB_MATCHES} paths",
                    search_root.display()
                )));
            }
        }
    }

    Ok(expanded_paths.into_iter().collect())
}

fn split_pattern_for_ripgrep(pattern: &str, cwd: &Path) -> Option<(AbsolutePathBuf, String)> {
    // Resolve relative patterns once, then split at the first glob
    // metacharacter. The prefix becomes the search root and the suffix stays as
    // the ripgrep glob. Root-level glob scans are intentionally skipped because
    // they are too broad for startup-time sandbox construction.
    let absolute_pattern = AbsolutePathBuf::resolve_path_against_base(pattern, cwd);
    let pattern = absolute_pattern.to_string_lossy();
    let first_glob_index = pattern
        .char_indices()
        .find_map(|(index, ch)| matches!(ch, '*' | '?' | '[' | ']').then_some(index))?;
    let static_prefix = &pattern[..first_glob_index];
    if static_prefix.is_empty() || static_prefix == "/" {
        return None;
    }
    let search_root_end = if static_prefix.ends_with('/') {
        static_prefix.len() - 1
    } else {
        static_prefix.rfind('/').unwrap_or(0)
    };
    let search_root = if search_root_end == 0 {
        PathBuf::from("/")
    } else {
        PathBuf::from(&pattern[..search_root_end])
    };
    let search_root = AbsolutePathBuf::from_absolute_path_checked(search_root).ok()?;
    let glob = escape_unclosed_glob_classes(&pattern[search_root_end + 1..]);
    (!glob.is_empty()).then_some((search_root, glob))
}

fn escape_unclosed_glob_classes(glob: &str) -> String {
    // The filesystem policy accepts an unclosed `[` as a literal. Ripgrep treats
    // that as invalid glob syntax, so escape only the unclosed class opener.
    let mut escaped = String::with_capacity(glob.len());
    let mut chars = glob.chars();

    while let Some(ch) = chars.next() {
        if ch != '[' {
            escaped.push(ch);
            continue;
        }

        let mut class = String::new();
        let mut closed = false;
        for class_ch in chars.by_ref() {
            if class_ch == ']' {
                closed = true;
                break;
            }
            class.push(class_ch);
        }

        if closed {
            escaped.push('[');
            escaped.push_str(&class);
            escaped.push(']');
        } else {
            escaped.push_str(r"\[");
            escaped.push_str(&class);
        }
    }

    escaped
}

fn ripgrep_files(
    search_root: &Path,
    globs: &[String],
    max_depth: Option<usize>,
) -> Result<Vec<AbsolutePathBuf>> {
    // Use `rg --files` rather than shell expansion so dotfiles and ignored files
    // are still considered. A status 1 with no stderr is ripgrep's "no matches"
    // case, not a sandbox construction error.
    let mut command = Command::new("rg");
    command
        .arg("--files")
        .arg("--hidden")
        .arg("--no-ignore")
        .arg("--null");
    if let Some(max_depth) = max_depth {
        command.arg("--max-depth").arg(max_depth.to_string());
    }
    for glob in globs {
        command.arg("--glob").arg(glob);
    }
    command.arg("--").arg(search_root);

    /*
     * Prefer ripgrep for unreadable glob expansion because it is fast and
     * already implements the file-walking semantics we want here: include
     * dotfiles, ignore ignore files, and do not recurse through symlinked
     * directories. If `rg` is not installed in the runtime environment, fall
     * back to the internal globset walker so sandbox construction still masks
     * matching paths. Other ripgrep failures stay fatal so deny-read does not
     * silently weaken.
     */
    let output = match command.output() {
        Ok(output) => output,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return glob_files(search_root, globs, max_depth);
        }
        Err(err) => return Err(err.into()),
    };
    if !output.status.success() {
        if output.status.code() == Some(1) && output.stderr.is_empty() {
            return Ok(Vec::new());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CodexErr::Fatal(format!(
            "ripgrep unreadable glob scan failed for {}: {stderr}",
            search_root.display()
        )));
    }

    let paths = output
        .stdout
        .split(|byte| *byte == b'\0')
        .filter(|path| !path.is_empty())
        .map(|path| {
            let path = PathBuf::from(OsString::from_vec(path.to_vec()));
            if path.is_absolute() {
                path
            } else {
                search_root.join(path)
            }
        })
        .map(AbsolutePathBuf::from_absolute_path_checked)
        .collect::<io::Result<Vec<_>>>()?;
    Ok(paths)
}

fn glob_files(
    search_root: &Path,
    globs: &[String],
    max_depth: Option<usize>,
) -> Result<Vec<AbsolutePathBuf>> {
    let mut builder = GlobSetBuilder::new();
    for glob in globs {
        let glob = GlobBuilder::new(glob)
            .literal_separator(true)
            .allow_unclosed_class(true)
            .build()
            .map_err(|err| {
                CodexErr::Fatal(format!(
                    "unreadable glob pattern is invalid for {}: {err}",
                    search_root.display()
                ))
            })?;
        builder.add(glob);
    }
    let glob_set = builder.build().map_err(|err| {
        CodexErr::Fatal(format!(
            "unreadable glob matcher failed for {}: {err}",
            search_root.display()
        ))
    })?;

    let mut paths = Vec::new();
    collect_glob_files(search_root, search_root, &glob_set, max_depth, &mut paths)?;
    Ok(paths)
}

fn collect_glob_files(
    search_root: &Path,
    dir: &Path,
    glob_set: &GlobSet,
    remaining_depth: Option<usize>,
    paths: &mut Vec<AbsolutePathBuf>,
) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        let relative = path.strip_prefix(search_root).unwrap_or(path.as_path());

        if (file_type.is_file() || file_type.is_symlink()) && glob_set.is_match(relative) {
            paths.push(AbsolutePathBuf::from_absolute_path_checked(&path)?);
        }

        if !file_type.is_dir() {
            continue;
        }
        let remaining_depth = match remaining_depth {
            Some(0 | 1) => continue,
            Some(depth) => Some(depth - 1),
            None => None,
        };
        collect_glob_files(search_root, &path, glob_set, remaining_depth, paths)?;
    }
    Ok(())
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn path_depth(path: &Path) -> usize {
    path.components().count()
}

fn canonical_target_if_symlinked_path(path: &Path) -> Option<PathBuf> {
    // Return the fully resolved target only when some path component is a
    // symlink. Callers use this to bind/mask the real filesystem location while
    // leaving ordinary paths in their logical form.
    let mut current = PathBuf::new();
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::RootDir => {
                current.push(Path::new("/"));
                continue;
            }
            Component::CurDir => continue,
            Component::ParentDir => {
                current.pop();
                continue;
            }
            Component::Normal(part) => current.push(part),
            Component::Prefix(_) => continue,
        }

        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(_) => return None,
        };
        if metadata.file_type().is_symlink() {
            let target = fs::canonicalize(path).ok()?;
            if target.as_path() == path {
                return None;
            }
            return Some(target);
        }
    }
    None
}

/// `path` with links resolved up to its nearest existing component (the
/// rest appended as written), when that differs from `path`. `None` too
/// when a link on the way lies inside a writable root (by its real
/// location): the command could change it, so the path keeps today's
/// handling.
fn real_path_through_links(path: &Path, allowed_write_paths: &[PathBuf]) -> Option<PathBuf> {
    use std::path::Component;
    let mut real = PathBuf::from("/");
    let mut components = path.components();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            if component == Component::ParentDir {
                real.pop();
            }
            continue;
        };
        let candidate = real.join(name);
        let Ok(metadata) = fs::symlink_metadata(&candidate) else {
            real = candidate;
            real.extend(components);
            break;
        };
        if !metadata.file_type().is_symlink() {
            real = candidate;
            continue;
        }
        if allowed_write_paths
            .iter()
            .any(|root| candidate.starts_with(root) && candidate != *root)
        {
            return None;
        }
        // A dangling link keeps today's handling too.
        real = fs::canonicalize(&candidate).ok()?;
    }
    (real != path).then_some(real)
}

fn remap_paths_for_symlink_target(paths: Vec<PathBuf>, root: &Path, target: &Path) -> Vec<PathBuf> {
    paths
        .into_iter()
        .map(|path| {
            if let Ok(relative) = path.strip_prefix(root) {
                target.join(relative)
            } else {
                path
            }
        })
        .collect()
}

fn normalize_command_cwd_for_bwrap(command_cwd: &Path) -> PathBuf {
    command_cwd
        .canonicalize()
        .unwrap_or_else(|_| command_cwd.to_path_buf())
}

fn append_mount_target_parent_dir_args(args: &mut Vec<String>, mount_target: &Path, anchor: &Path) {
    let mount_target_dir = if mount_target.is_dir() {
        mount_target
    } else if let Some(parent) = mount_target.parent() {
        parent
    } else {
        return;
    };
    let mut mount_target_dirs: Vec<PathBuf> = mount_target_dir
        .ancestors()
        .take_while(|path| *path != anchor)
        .map(Path::to_path_buf)
        .collect();
    mount_target_dirs.reverse();
    for mount_target_dir in mount_target_dirs {
        args.push("--dir".to_string());
        args.push(path_to_string(&mount_target_dir));
    }
}

fn append_read_only_subpath_args(
    bwrap_args: &mut BwrapArgs,
    subpath: &Path,
    allowed_write_paths: &[PathBuf],
) -> Result<()> {
    if let Some(symlink) = first_writable_symlink_component_in_path(subpath, allowed_write_paths) {
        /*
         * A read-only carveout under a writable symlink cannot be made reliable
         * with bwrap path arguments. Binding the symlink's current target would
         * only protect a startup-time snapshot; the sandboxed process could
         * replace the writable symlink before it reads through the logical path.
         */
        return Err(CodexErr::Fatal(format!(
            "cannot enforce sandbox read-only path {} because it crosses writable symlink {}",
            subpath.display(),
            symlink.display()
        )));
    }

    if let Some(metadata) = transient_empty_metadata_path(subpath)
        && is_within_allowed_write_paths(subpath, allowed_write_paths)
    {
        // Another concurrent bwrap setup can leave an empty mount target at
        // a missing path (#562: any name, not only metadata). Treat it like
        // the missing case instead of binding that transient host path as
        // the stable source, so its owner's cleanup cannot remove it from
        // under this run.
        match metadata {
            EmptyProtectedMetadataPath::File(metadata) => {
                append_existing_empty_file_bind_data_args(bwrap_args, subpath, &metadata)?;
            }
            EmptyProtectedMetadataPath::Directory(metadata) => {
                append_existing_empty_directory_args(bwrap_args, subpath, &metadata);
            }
        }
        return Ok(());
    }

    if !subpath.exists() {
        if let Some(first_missing_component) = find_first_non_existent_component(subpath)
            && is_within_allowed_write_paths(&first_missing_component, allowed_write_paths)
            && !hold_file_above_missing_path(
                bwrap_args,
                &first_missing_component,
                allowed_write_paths,
            )?
        {
            append_missing_read_only_subpath_args(bwrap_args, &first_missing_component)?;
        }
        return Ok(());
    }

    if is_within_allowed_write_paths(subpath, allowed_write_paths) {
        bwrap_args.args.push("--ro-bind".to_string());
        bwrap_args.args.push(path_to_string(subpath));
        bwrap_args.args.push(path_to_string(subpath));
    }
    Ok(())
}

fn append_empty_file_bind_data_args(bwrap_args: &mut BwrapArgs, path: &Path) -> Result<()> {
    // bubblewrap reads a `--ro-bind-data` descriptor to its end and then
    // closes it, so every mask needs its own. A shared one left the second
    // mask reading a closed descriptor ("Can't write data to file ...: Bad
    // file descriptor") unless bubblewrap happened to reuse its number (#636).
    let null = File::open("/dev/null")?;
    bwrap_args.args.push("--ro-bind-data".to_string());
    bwrap_args.args.push(null.as_raw_fd().to_string());
    bwrap_args.args.push(path_to_string(path));
    bwrap_args.preserved_files.push(null);
    Ok(())
}

fn append_empty_directory_args(bwrap_args: &mut BwrapArgs, path: &Path) {
    bwrap_args.args.push("--perms".to_string());
    bwrap_args.args.push("555".to_string());
    bwrap_args.args.push("--tmpfs".to_string());
    bwrap_args.args.push(path_to_string(path));
    bwrap_args.args.push("--remount-ro".to_string());
    bwrap_args.args.push(path_to_string(path));
}

fn append_missing_read_only_subpath_args(bwrap_args: &mut BwrapArgs, path: &Path) -> Result<()> {
    if has_synthetic_mount_target(bwrap_args, path) {
        return Ok(());
    }
    if path.file_name().is_some_and(is_protected_metadata_name) {
        append_empty_directory_args(bwrap_args, path);
        bwrap_args
            .synthetic_mount_targets
            .push(SyntheticMountTarget::missing_empty_directory(path));
        return Ok(());
    }

    append_missing_empty_file_bind_data_args(bwrap_args, path)
}

fn append_missing_empty_file_bind_data_args(bwrap_args: &mut BwrapArgs, path: &Path) -> Result<()> {
    if has_synthetic_mount_target(bwrap_args, path) {
        return Ok(());
    }
    append_empty_file_bind_data_args(bwrap_args, path)?;
    bwrap_args
        .synthetic_mount_targets
        .push(SyntheticMountTarget::missing(path));
    Ok(())
}

/// #473: several denied paths can share their first missing component
/// (`~/.config` for `~/.config/gh` and `~/.config/gcloud`). One empty
/// placeholder there already stops the whole tree being created, and
/// bubblewrap fails to mount a second one over the first. Only this
/// writable-root pass's targets are listed (see `create_filesystem_args`).
fn has_synthetic_mount_target(bwrap_args: &BwrapArgs, path: &Path) -> bool {
    bwrap_args
        .synthetic_mount_targets
        .iter()
        .any(|target| target.path() == path)
}

/// #473: the first missing component of `path` when the folder it would be
/// created in sits at or below an existing read-only path. That folder is
/// bound read-only, so the command cannot create it, and neither can
/// bubblewrap create a mount target there. `None` when `path` exists, is not
/// below a writable root, or crosses a writable symlink (masking it then
/// fails closed as before).
fn missing_component_below_read_only_path(
    path: &Path,
    read_only_subpaths: &[PathBuf],
    allowed_write_paths: &[PathBuf],
) -> Option<PathBuf> {
    if first_writable_symlink_component_in_path(path, allowed_write_paths).is_some() {
        return None;
    }
    let missing = find_first_non_existent_component(path)?;
    if !is_within_allowed_write_paths(&missing, allowed_write_paths) {
        return None;
    }
    let parent = missing.parent()?;
    let parent_is_dir = fs::symlink_metadata(parent).is_ok_and(|metadata| metadata.is_dir());
    (parent_is_dir
        && read_only_subpaths
            .iter()
            .any(|read_only| parent.starts_with(read_only)))
    .then_some(missing)
}

/// #473: hides denied paths that are missing in `dir`, a read-only folder,
/// for the whole run. A placeholder cannot be created there, and a live
/// read-only bind would still show a credential file the host writes while
/// the command runs. So `dir` becomes a read-only tmpfs that holds what it
/// has at launch (folders and files bound read-only, links recreated),
/// without `hidden`: nothing created after launch appears inside. Entries
/// that vanish before bubblewrap runs are skipped; names that are not UTF-8
/// are left out.
fn append_read_only_snapshot_dir_args(
    bwrap_args: &mut BwrapArgs,
    dir: &Path,
    hidden: &[PathBuf],
    perms: &str,
) -> Result<()> {
    let mut entries = fs::read_dir(dir)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    bwrap_args.args.push("--perms".to_string());
    bwrap_args.args.push(perms.to_string());
    bwrap_args.args.push("--tmpfs".to_string());
    bwrap_args.args.push(path_to_string(dir));
    for entry in entries {
        let path = entry.path();
        if hidden.contains(&path) || path.to_str().is_none() {
            continue;
        }
        if entry
            .file_type()
            .is_ok_and(|file_type| file_type.is_symlink())
        {
            let Ok(target) = fs::read_link(&path) else {
                continue;
            };
            let Some(target) = target.to_str() else {
                continue;
            };
            bwrap_args.args.push("--symlink".to_string());
            bwrap_args.args.push(target.to_string());
        } else {
            bwrap_args.args.push("--ro-bind-try".to_string());
            bwrap_args.args.push(path_to_string(&path));
        }
        bwrap_args.args.push(path_to_string(&path));
    }
    bwrap_args.args.push("--remount-ro".to_string());
    bwrap_args.args.push(path_to_string(dir));
    Ok(())
}

fn append_existing_empty_file_bind_data_args(
    bwrap_args: &mut BwrapArgs,
    path: &Path,
    metadata: &Metadata,
) -> Result<()> {
    append_empty_file_bind_data_args(bwrap_args, path)?;
    bwrap_args
        .synthetic_mount_targets
        .push(SyntheticMountTarget::existing_empty_file(path, metadata));
    Ok(())
}

fn append_existing_empty_directory_args(
    bwrap_args: &mut BwrapArgs,
    path: &Path,
    metadata: &Metadata,
) {
    append_empty_directory_args(bwrap_args, path);
    bwrap_args
        .synthetic_mount_targets
        .push(SyntheticMountTarget::existing_empty_directory(
            path, metadata,
        ));
}

fn append_unreadable_root_args(
    bwrap_args: &mut BwrapArgs,
    unreadable_root: &Path,
    allowed_write_paths: &[PathBuf],
) -> Result<()> {
    if let Some(symlink) =
        first_writable_symlink_component_in_path(unreadable_root, allowed_write_paths)
    {
        /*
         * Deny-read masks must fail closed when the protected path crosses a
         * symlink that remains writable to the sandboxed process. Resolving and
         * masking the symlink's current target is a TOCTTOU snapshot: bwrap would
         * protect the old target while the logical path could later point
         * somewhere else.
         */
        return Err(CodexErr::Fatal(format!(
            "cannot enforce sandbox deny-read path {} because it crosses writable symlink {}",
            unreadable_root.display(),
            symlink.display()
        )));
    }

    if !unreadable_root.exists() {
        if let Some(first_missing_component) = find_first_non_existent_component(unreadable_root)
            && is_within_allowed_write_paths(&first_missing_component, allowed_write_paths)
            && !hold_file_above_missing_path(
                bwrap_args,
                &first_missing_component,
                allowed_write_paths,
            )?
        {
            append_missing_empty_file_bind_data_args(bwrap_args, &first_missing_component)?;
        }
        return Ok(());
    }

    // #562: another live run's placeholder (made in that run's writable
    // root, which need not be one of this run's): mask it as before, and
    // register it so that run's cleanup cannot remove it from under this one.
    if let Some(metadata) = live_placeholder_file(unreadable_root) {
        if has_synthetic_mount_target(bwrap_args, unreadable_root) {
            return Ok(());
        }
        bwrap_args.args.push("--perms".to_string());
        bwrap_args.args.push("000".to_string());
        return append_existing_empty_file_bind_data_args(bwrap_args, unreadable_root, &metadata);
    }
    // #607: the same for another live run's placeholder folder (a protected
    // metadata name it holds read-only): masked as a denied folder and
    // registered, so its maker's cleanup (or a sweep after that run was
    // killed) cannot remove it, and the mask with it, from under this run.
    if let Some(metadata) = live_placeholder_directory(unreadable_root) {
        append_existing_unreadable_path_args(bwrap_args, unreadable_root, allowed_write_paths)?;
        if !has_synthetic_mount_target(bwrap_args, unreadable_root) {
            bwrap_args.synthetic_mount_targets.push(
                SyntheticMountTarget::existing_empty_directory(unreadable_root, &metadata),
            );
        }
        return Ok(());
    }

    append_existing_unreadable_path_args(bwrap_args, unreadable_root, allowed_write_paths)
}

/// #562: the folder `missing` would go in is a regular file, so `missing`
/// needs no placeholder of its own (bubblewrap would fail the run: `Can't
/// mkdir parents … Not a directory`). The file stays in place instead, so the
/// command cannot remove it and create the path: another live run's
/// placeholder (for a path below it, such as `~/Library` for
/// `~/Library/Keychains`) is held as one (masked and registered, so that
/// run's cleanup cannot remove it from under this one); any other file in a
/// writable root is bound read-only where it is (a mount point cannot be
/// removed or renamed). #745: a folder there that is another run's empty
/// placeholder is held too ([`hold_placeholder_folder_above_missing_path`]).
/// Returns whether `missing` needs nothing more.
fn hold_file_above_missing_path(
    bwrap_args: &mut BwrapArgs,
    missing: &Path,
    allowed_write_paths: &[PathBuf],
) -> Result<bool> {
    let Some(parent) = missing.parent() else {
        return Ok(false);
    };
    let Ok(metadata) = fs::symlink_metadata(parent) else {
        return Ok(false);
    };
    if metadata.file_type().is_dir() {
        return Ok(hold_placeholder_folder_above_missing_path(
            bwrap_args,
            parent,
            &metadata,
            allowed_write_paths,
        ));
    }
    if !metadata.file_type().is_file() {
        return Ok(false);
    }
    if has_synthetic_mount_target(bwrap_args, parent) || has_read_only_bind(bwrap_args, parent) {
        return Ok(true);
    }
    if let Some(metadata) = live_placeholder_file(parent) {
        append_existing_empty_file_bind_data_args(bwrap_args, parent, &metadata)?;
    } else if is_within_allowed_write_paths(parent, allowed_write_paths) {
        bwrap_args.args.push("--ro-bind".to_string());
        bwrap_args.args.push(path_to_string(parent));
        bwrap_args.args.push(path_to_string(parent));
    }
    Ok(true)
}

/// #745: the folder `missing` would go in (`folder`) is another run's empty
/// placeholder folder (a protected `.git` it made, which this run's policy
/// took for a repository's). A placeholder made in it would change it, so
/// that its maker could not remove it, and it was left behind for good. It
/// is held read-only instead, as when it is a protected path itself: the
/// command cannot create `missing` (or anything) in it, as it could not
/// create the folder without the other run. Whether `missing` needs nothing
/// more.
fn hold_placeholder_folder_above_missing_path(
    bwrap_args: &mut BwrapArgs,
    folder: &Path,
    metadata: &Metadata,
    allowed_write_paths: &[PathBuf],
) -> bool {
    // Held already in this pass: its read-only mount covers `missing`.
    if has_synthetic_mount_target(bwrap_args, folder) {
        return true;
    }
    if !is_within_allowed_write_paths(folder, allowed_write_paths)
        || allowed_write_paths.iter().any(|root| root == folder)
        || !directory_is_empty(folder)
        || !(crate::linux_run_main::is_live_placeholder(folder)
            || crate::placeholder_journal::recorded_by_another_run(folder))
    {
        return false;
    }
    append_existing_empty_directory_args(bwrap_args, folder, metadata);
    true
}

fn has_read_only_bind(bwrap_args: &BwrapArgs, path: &Path) -> bool {
    let path = path_to_string(path);
    bwrap_args
        .args
        .windows(3)
        .any(|window| window[0] == "--ro-bind" && window[1] == path && window[2] == path)
}

/// #562: `path` when it is an empty file another live run registered as its
/// placeholder.
fn live_placeholder_file(path: &Path) -> Option<Metadata> {
    let metadata = empty_file_metadata(path)?;
    crate::linux_run_main::is_live_placeholder(path).then_some(metadata)
}

/// #607: `path` when it is an empty folder another live run registered as
/// its placeholder.
fn live_placeholder_directory(path: &Path) -> Option<Metadata> {
    let metadata = fs::symlink_metadata(path).ok()?;
    (metadata.file_type().is_dir() && directory_is_empty(path))
        .then_some(metadata)
        .filter(|_| crate::linux_run_main::is_live_placeholder(path))
}

fn empty_file_metadata(path: &Path) -> Option<Metadata> {
    let metadata = fs::symlink_metadata(path).ok()?;
    (metadata.file_type().is_file() && metadata.len() == 0).then_some(metadata)
}

fn append_existing_unreadable_path_args(
    bwrap_args: &mut BwrapArgs,
    unreadable_root: &Path,
    allowed_write_paths: &[PathBuf],
) -> Result<()> {
    if unreadable_root.is_dir() {
        let mut writable_descendants: Vec<&Path> = allowed_write_paths
            .iter()
            .map(PathBuf::as_path)
            .filter(|path| *path != unreadable_root && path.starts_with(unreadable_root))
            .collect();
        bwrap_args.args.push("--perms".to_string());
        // Execute-only perms let the process traverse into explicitly
        // re-opened writable descendants while still hiding the denied
        // directory contents. Plain denied directories with no writable child
        // mounts stay at `000`.
        bwrap_args.args.push(if writable_descendants.is_empty() {
            "000".to_string()
        } else {
            "111".to_string()
        });
        bwrap_args.args.push("--tmpfs".to_string());
        bwrap_args.args.push(path_to_string(unreadable_root));
        // Recreate any writable descendants inside the tmpfs before remounting
        // the denied parent read-only. Otherwise bubblewrap cannot mkdir the
        // nested mount targets after the parent has been frozen.
        writable_descendants.sort_by_key(|path| path_depth(path));
        for writable_descendant in writable_descendants {
            append_mount_target_parent_dir_args(
                &mut bwrap_args.args,
                writable_descendant,
                unreadable_root,
            );
        }
        bwrap_args.args.push("--remount-ro".to_string());
        bwrap_args.args.push(path_to_string(unreadable_root));
        return Ok(());
    }

    bwrap_args.args.push("--perms".to_string());
    bwrap_args.args.push("000".to_string());
    append_empty_file_bind_data_args(bwrap_args, unreadable_root)
}

/// Returns true when `path` is under any allowed writable root.
fn is_within_allowed_write_paths(path: &Path, allowed_write_paths: &[PathBuf]) -> bool {
    allowed_write_paths
        .iter()
        .any(|root| path.starts_with(root))
}

enum EmptyProtectedMetadataPath {
    File(Metadata),
    Directory(Metadata),
}

fn transient_empty_metadata_path(path: &Path) -> Option<EmptyProtectedMetadataPath> {
    if !path.file_name().is_some_and(is_protected_metadata_name) {
        // #562: another live run's placeholder, whatever its name.
        return live_placeholder_file(path).map(EmptyProtectedMetadataPath::File);
    }

    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_file() && metadata.len() == 0 {
        return Some(EmptyProtectedMetadataPath::File(metadata));
    }

    if metadata.file_type().is_dir() && directory_is_empty(path) {
        return Some(EmptyProtectedMetadataPath::Directory(metadata));
    }

    None
}

fn directory_is_empty(path: &Path) -> bool {
    let Ok(mut entries) = fs::read_dir(path) else {
        return false;
    };
    entries.next().is_none()
}

fn first_writable_symlink_component_in_path(
    target_path: &Path,
    allowed_write_paths: &[PathBuf],
) -> Option<PathBuf> {
    /*
     * Walk the logical path and report the first symlink component that lives
     * under a writable root. These symlinks are mutable from inside the sandbox,
     * so any mount or mask based on their resolved target would be racing a path
     * the sandboxed process can change.
     */
    let mut current = PathBuf::new();

    for component in target_path.components() {
        use std::path::Component;
        match component {
            Component::RootDir => {
                current.push(Path::new("/"));
                continue;
            }
            Component::CurDir => continue,
            Component::ParentDir => {
                current.pop();
                continue;
            }
            Component::Normal(part) => current.push(part),
            Component::Prefix(_) => continue,
        }

        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(_) => break,
        };

        if metadata.file_type().is_symlink()
            && is_within_allowed_write_paths(&current, allowed_write_paths)
        {
            return Some(current);
        }
    }

    None
}

/// Find the first missing path component while walking `target_path`.
///
/// Mounting `/dev/null` on the first missing component prevents the sandboxed
/// process from creating the protected path hierarchy.
fn find_first_non_existent_component(target_path: &Path) -> Option<PathBuf> {
    let mut current = PathBuf::new();

    for component in target_path.components() {
        use std::path::Component;
        match component {
            Component::RootDir => {
                current.push(Path::new("/"));
                continue;
            }
            Component::CurDir => continue,
            Component::ParentDir => {
                current.pop();
                continue;
            }
            Component::Normal(part) => current.push(part),
            Component::Prefix(_) => continue,
        }

        if !current.exists() {
            return Some(current);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    use codex_protocol::protocol::FileSystemAccessMode;
    use codex_protocol::protocol::FileSystemPath;
    use codex_protocol::protocol::FileSystemSandboxEntry;
    use codex_protocol::protocol::FileSystemSandboxPolicy;
    use codex_protocol::protocol::FileSystemSpecialPath;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    const NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH: Option<usize> = None;

    #[test]
    fn default_unreadable_glob_scan_has_no_depth_cap() {
        assert_eq!(BwrapOptions::default().glob_scan_max_depth, None);
    }

    fn unreadable_glob_entry(pattern: String) -> FileSystemSandboxEntry {
        FileSystemSandboxEntry {
            path: FileSystemPath::GlobPattern {
                pattern,
                scan: None,
            },
            access: FileSystemAccessMode::Deny,
            missing_path_behavior: None,
        }
    }

    fn default_policy_with_unreadable_glob(pattern: String) -> FileSystemSandboxPolicy {
        let mut policy = FileSystemSandboxPolicy::default();
        policy.entries.push(unreadable_glob_entry(pattern));
        policy
    }

    #[test]
    fn full_disk_write_full_network_returns_unwrapped_command() {
        let command = vec!["/bin/true".to_string()];
        let args = create_bwrap_command_args(
            command.clone(),
            &FileSystemSandboxPolicy::unrestricted(),
            Path::new("/"),
            Path::new("/"),
            BwrapOptions {
                mount_proc: true,
                network_mode: BwrapNetworkMode::FullAccess,
                ..Default::default()
            },
        )
        .expect("create bwrap args");

        assert_eq!(args.args, command);
    }

    #[test]
    fn full_disk_write_proxy_only_keeps_full_filesystem_but_unshares_network() {
        let command = vec!["/bin/true".to_string()];
        let args = create_bwrap_command_args(
            command,
            &FileSystemSandboxPolicy::unrestricted(),
            Path::new("/"),
            Path::new("/"),
            BwrapOptions {
                mount_proc: true,
                network_mode: BwrapNetworkMode::ProxyOnly,
                ..Default::default()
            },
        )
        .expect("create bwrap args");

        assert_eq!(
            args.args,
            vec![
                "--new-session".to_string(),
                "--die-with-parent".to_string(),
                "--bind".to_string(),
                "/".to_string(),
                "/".to_string(),
                "--dev".to_string(),
                "/dev".to_string(),
                "--unshare-user".to_string(),
                "--unshare-pid".to_string(),
                "--unshare-net".to_string(),
                "--proc".to_string(),
                "/proc".to_string(),
                "--".to_string(),
                "/bin/true".to_string(),
            ]
        );
    }

    #[test]
    fn full_disk_write_with_unreadable_glob_still_wraps_and_masks_match() {
        if !ripgrep_available() {
            return;
        }

        let temp_dir = TempDir::new().expect("temp dir");
        let root_env = temp_dir.path().join(".env");
        std::fs::write(&root_env, "secret").expect("write env");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Write,
                missing_path_behavior: None,
            },
            unreadable_glob_entry(format!("{}/**/*.env", temp_dir.path().display())),
        ]);
        let command = vec!["/bin/true".to_string()];

        let args = create_bwrap_command_args(
            command.clone(),
            &policy,
            temp_dir.path(),
            temp_dir.path(),
            BwrapOptions::default(),
        )
        .expect("create bwrap args");

        assert_ne!(
            args.args, command,
            "full-write policy with unreadable globs must still use bwrap"
        );
        assert_file_masked(&args.args, &root_env);
    }

    #[cfg(unix)]
    #[test]
    fn restricted_policy_chdirs_to_canonical_command_cwd() {
        let temp_dir = TempDir::new().expect("temp dir");
        let real_root = temp_dir.path().join("real");
        let real_subdir = real_root.join("subdir");
        let link_root = temp_dir.path().join("link");
        std::fs::create_dir_all(&real_subdir).expect("create real subdir");
        std::os::unix::fs::symlink(&real_root, &link_root).expect("create symlinked root");

        let sandbox_policy_cwd = AbsolutePathBuf::from_absolute_path(&link_root)
            .expect("absolute symlinked root")
            .to_path_buf();
        let command_cwd = link_root.join("subdir");
        let canonical_command_cwd = real_subdir
            .canonicalize()
            .expect("canonicalize command cwd");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Minimal,
                },
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::project_roots(/*subpath*/ None),
                },
                access: FileSystemAccessMode::Write,
                missing_path_behavior: None,
            },
        ]);

        let args = create_bwrap_command_args(
            vec!["/bin/true".to_string()],
            &policy,
            sandbox_policy_cwd.as_path(),
            &command_cwd,
            BwrapOptions::default(),
        )
        .expect("create bwrap args");
        let canonical_sandbox_cwd = path_to_string(
            &real_root
                .canonicalize()
                .expect("canonicalize sandbox policy cwd"),
        );
        let canonical_command_cwd = path_to_string(&canonical_command_cwd);
        let link_sandbox_cwd = path_to_string(&link_root);
        let link_command_cwd = path_to_string(&command_cwd);

        assert!(
            args.args
                .windows(2)
                .any(|window| { window == ["--chdir", canonical_command_cwd.as_str()] })
        );
        assert!(args.args.windows(3).any(|window| {
            window
                == [
                    "--ro-bind",
                    canonical_sandbox_cwd.as_str(),
                    canonical_sandbox_cwd.as_str(),
                ]
        }));
        assert!(
            !args
                .args
                .windows(2)
                .any(|window| { window == ["--chdir", link_command_cwd.as_str()] })
        );
        assert!(!args.args.windows(3).any(|window| {
            window
                == [
                    "--ro-bind",
                    link_sandbox_cwd.as_str(),
                    link_sandbox_cwd.as_str(),
                ]
        }));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_writable_roots_bind_real_target_and_remap_carveouts() {
        let temp_dir = TempDir::new().expect("temp dir");
        let real_root = temp_dir.path().join("real");
        let link_root = temp_dir.path().join("link");
        let blocked = real_root.join("blocked");
        std::fs::create_dir_all(&blocked).expect("create blocked dir");
        std::os::unix::fs::symlink(&real_root, &link_root).expect("create symlinked root");

        let link_root =
            AbsolutePathBuf::from_absolute_path(&link_root).expect("absolute symlinked root");
        let link_blocked = link_root.join("blocked");
        let real_root_str = path_to_string(&real_root);
        let real_blocked_str = path_to_string(&blocked);
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Path { path: link_root },
                access: FileSystemAccessMode::Write,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path { path: link_blocked },
                access: FileSystemAccessMode::Deny,
                missing_path_behavior: None,
            },
        ]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");

        assert!(args.args.windows(3).any(|window| {
            window == ["--bind", real_root_str.as_str(), real_root_str.as_str()]
        }));
        assert!(args.args.windows(6).any(|window| {
            window
                == [
                    "--perms",
                    "000",
                    "--tmpfs",
                    real_blocked_str.as_str(),
                    "--remount-ro",
                    real_blocked_str.as_str(),
                ]
        }));
    }

    /// #488: a denial spelled by the real path is masked after the
    /// symlinked root is bound, not skipped as already inside a writable
    /// root.
    #[cfg(unix)]
    #[test]
    fn symlinked_writable_roots_mask_denials_spelled_by_the_target() {
        let temp_dir = TempDir::new().expect("temp dir");
        let real_root = temp_dir.path().join("real");
        let link_root = temp_dir.path().join("link");
        let blocked = real_root.join("blocked");
        std::fs::create_dir_all(&blocked).expect("create blocked dir");
        std::os::unix::fs::symlink(&real_root, &link_root).expect("create symlinked root");

        let real_root_str = path_to_string(&real_root);
        let real_blocked_str = path_to_string(&blocked);
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: AbsolutePathBuf::from_absolute_path(&link_root).expect("absolute"),
                },
                access: FileSystemAccessMode::Write,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: AbsolutePathBuf::from_absolute_path(&blocked).expect("absolute"),
                },
                access: FileSystemAccessMode::Deny,
                missing_path_behavior: None,
            },
        ]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");

        let bind = args
            .args
            .windows(3)
            .position(|window| window == ["--bind", real_root_str.as_str(), real_root_str.as_str()])
            .expect("bind");
        let mask = args
            .args
            .windows(4)
            .position(|window| window == ["--perms", "000", "--tmpfs", real_blocked_str.as_str()])
            .expect("mask");
        assert!(bind < mask, "{:?}", args.args);
        assert_eq!(
            args.args
                .iter()
                .filter(|arg| arg.as_str() == real_blocked_str)
                .count(),
            2,
            "masked once: {:?}",
            args.args
        );
    }

    /// #502: a denial spelled through an absolute link to the workspace is
    /// masked at the link's target after the workspace bind; bubblewrap is
    /// never asked to create a mount point through the link.
    #[cfg(unix)]
    #[test]
    fn denials_spelled_through_a_read_only_link_mask_the_target() {
        let temp_dir = TempDir::new().expect("temp dir");
        let real_root = temp_dir.path().join("real");
        let link_root = temp_dir.path().join("link");
        std::fs::create_dir_all(&real_root).expect("create real root");
        std::fs::write(real_root.join(".env"), "x").expect("create .env");
        std::os::unix::fs::symlink(&real_root, &link_root).expect("create link");
        let entry = |path: &Path, access| FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::from_absolute_path(path).expect("absolute"),
            },
            access,
            missing_path_behavior: None,
        };
        // Existing, and not created yet.
        for name in [".env", "later.key"] {
            let policy = FileSystemSandboxPolicy::restricted(vec![
                entry(&real_root, FileSystemAccessMode::Write),
                entry(&link_root.join(name), FileSystemAccessMode::Deny),
            ]);
            let args =
                create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                    .expect("filesystem args");
            let real_root_str = path_to_string(&real_root);
            let masked = path_to_string(&real_root.join(name));
            let bind = args
                .args
                .windows(3)
                .position(|window| {
                    window == ["--bind", real_root_str.as_str(), real_root_str.as_str()]
                })
                .expect("bind");
            let mask = args
                .args
                .iter()
                .position(|arg| arg == &masked)
                .expect("masked at the target");
            assert!(bind < mask, "{name}: {:?}", args.args);
            let through_link = path_to_string(&link_root.join(name));
            assert!(
                !args.args.contains(&through_link),
                "{name}: {:?}",
                args.args
            );
        }
    }

    /// #501: under restricted reads, a read-only root reached through a link
    /// is mounted at the link's path; a denial below its target, spelled
    /// either way, is masked at that path too.
    #[cfg(unix)]
    #[test]
    fn restricted_reads_mask_denials_below_a_linked_read_only_root() {
        let temp_dir = TempDir::new().expect("temp dir");
        let real_root = temp_dir.path().join("real");
        let link_root = temp_dir.path().join("link");
        std::fs::create_dir_all(&real_root).expect("create real root");
        std::fs::write(real_root.join("secret"), "x").expect("create secret");
        std::os::unix::fs::symlink(&real_root, &link_root).expect("create link");
        let entry = |path: &Path, access| FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::from_absolute_path(path).expect("absolute"),
            },
            access,
            missing_path_behavior: None,
        };
        let link_root_str = path_to_string(&link_root);
        let link_secret = path_to_string(&link_root.join("secret"));
        for denied in [real_root.join("secret"), link_root.join("secret")] {
            let policy = FileSystemSandboxPolicy::restricted(vec![
                entry(&link_root, FileSystemAccessMode::Read),
                entry(&denied, FileSystemAccessMode::Deny),
            ]);
            let args =
                create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                    .expect("filesystem args");
            let bind = args
                .args
                .windows(3)
                .position(|window| {
                    window == ["--ro-bind", link_root_str.as_str(), link_root_str.as_str()]
                })
                .expect("linked root mounted at its path");
            let mask = args
                .args
                .iter()
                .rposition(|arg| arg == &link_secret)
                .expect("masked through the mounted link");
            assert!(bind < mask, "{denied:?}: {:?}", args.args);
        }
    }

    /// #501 review: a denial holding a linked read-only root's target masks
    /// the whole root at its link path; a missing denial below it runs.
    #[cfg(unix)]
    #[test]
    fn restricted_reads_mask_a_linked_root_inside_a_denied_folder() {
        let temp_dir = TempDir::new().expect("temp dir");
        let parent = temp_dir.path().join("parent");
        let real_root = parent.join("real");
        let link_root = temp_dir.path().join("link");
        std::fs::create_dir_all(&real_root).expect("create real root");
        std::fs::write(real_root.join("secret"), "x").expect("create secret");
        std::os::unix::fs::symlink(&real_root, &link_root).expect("create link");
        let entry = |path: &Path, access| FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::from_absolute_path(path).expect("absolute"),
            },
            access,
            missing_path_behavior: None,
        };
        let link_root_str = path_to_string(&link_root);
        for denied in [parent, real_root.clone()] {
            let policy = FileSystemSandboxPolicy::restricted(vec![
                entry(&link_root, FileSystemAccessMode::Read),
                entry(&denied, FileSystemAccessMode::Deny),
            ]);
            let args =
                create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                    .expect("filesystem args");
            let bind = args
                .args
                .windows(3)
                .position(|window| {
                    window == ["--ro-bind", link_root_str.as_str(), link_root_str.as_str()]
                })
                .expect("linked root mounted at its path");
            let mask = args
                .args
                .windows(2)
                .rposition(|window| window == ["--tmpfs", link_root_str.as_str()])
                .expect("linked root masked whole");
            assert!(bind < mask, "{denied:?}: {:?}", args.args);
        }
        let policy = FileSystemSandboxPolicy::restricted(vec![
            entry(&link_root, FileSystemAccessMode::Read),
            entry(&real_root.join("later.key"), FileSystemAccessMode::Deny),
        ]);
        create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
            .expect("a missing denial below a linked root runs");
    }

    /// A denial that is itself a read-only link is masked at its target; a
    /// denial inside a folder masked whole is not masked again (bubblewrap
    /// could not create it in the read-only mask); a link inside a writable
    /// root reached through a read-only link keeps today's handling.
    #[cfg(unix)]
    #[test]
    fn denials_through_links_mask_once_and_keep_writable_links() {
        let temp_dir = TempDir::new().expect("temp dir");
        let base = temp_dir.path();
        let secrets = base.join("secrets");
        std::fs::create_dir_all(&secrets).expect("secrets");
        std::fs::write(secrets.join("key"), "x").expect("key");
        std::os::unix::fs::symlink(secrets.join("key"), base.join("key-link")).expect("link");
        std::os::unix::fs::symlink(&secrets, base.join("secrets-link")).expect("link");
        let entry = |path: &Path, access| FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::from_absolute_path(path).expect("absolute"),
            },
            access,
            missing_path_behavior: None,
        };
        let key = path_to_string(&secrets.join("key"));
        let secrets_str = path_to_string(&secrets);
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,
            },
            entry(&base.join("key-link"), FileSystemAccessMode::Deny),
        ]);
        let args = create_filesystem_args(&policy, base, NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
            .expect("filesystem args");
        assert!(args.args.contains(&key), "{:?}", args.args);
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,
            },
            entry(&base.join("secrets-link"), FileSystemAccessMode::Deny),
            entry(&secrets.join("key"), FileSystemAccessMode::Deny),
        ]);
        let args = create_filesystem_args(&policy, base, NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
            .expect("filesystem args");
        assert!(
            args.args
                .windows(2)
                .any(|window| window == ["--tmpfs", secrets_str.as_str()]),
            "{:?}",
            args.args
        );
        assert!(!args.args.contains(&key), "{:?}", args.args);

        // `/w/link -> /w/real` is read-only; `/w/real/sub` is a link inside
        // the writable `real`: not resolved, so it still fails closed or
        // masks as before rather than snapshotting the writable link.
        let real = base.join("w").join("real");
        std::fs::create_dir_all(&real).expect("real");
        std::fs::create_dir_all(base.join("t")).expect("t");
        std::os::unix::fs::symlink(base.join("t"), real.join("sub")).expect("sub");
        std::os::unix::fs::symlink(&real, base.join("w").join("link")).expect("link");
        assert_eq!(
            real_path_through_links(
                &base.join("w").join("link").join("sub").join(".env"),
                std::slice::from_ref(&real)
            ),
            None
        );
        assert_eq!(
            real_path_through_links(&base.join("w").join("link").join(".env"), &[]),
            Some(real.join(".env"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn writable_roots_under_symlinked_ancestors_bind_real_target() {
        let temp_dir = TempDir::new().expect("temp dir");
        let logical_home = temp_dir.path().join("home");
        let real_codex = temp_dir.path().join("real-codex");
        let logical_codex = logical_home.join(".codex");
        let real_memories = real_codex.join("memories");
        let logical_memories = logical_codex.join("memories");
        std::fs::create_dir_all(&logical_home).expect("create logical home");
        std::fs::create_dir_all(&real_memories).expect("create memories dir");
        std::os::unix::fs::symlink(&real_codex, &logical_codex)
            .expect("create symlinked codex home");

        let logical_memories_root =
            AbsolutePathBuf::from_absolute_path(&logical_memories).expect("absolute memories");
        let real_memories_str = path_to_string(&real_memories);
        let logical_memories_str = path_to_string(&logical_memories);
        let policy = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: logical_memories_root,
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        }]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");

        assert!(args.args.windows(3).any(|window| {
            window
                == [
                    "--bind",
                    real_memories_str.as_str(),
                    real_memories_str.as_str(),
                ]
        }));
        assert!(!args.args.windows(3).any(|window| {
            window
                == [
                    "--bind",
                    logical_memories_str.as_str(),
                    logical_memories_str.as_str(),
                ]
        }));
    }

    #[cfg(unix)]
    #[test]
    fn protected_symlinked_directory_subpaths_fail_closed() {
        let temp_dir = TempDir::new().expect("temp dir");
        let root = temp_dir.path().join("root");
        let agents_target = root.join("agents-target");
        let agents_link = root.join(".agents");
        std::fs::create_dir_all(&agents_target).expect("create agents target");
        std::os::unix::fs::symlink(&agents_target, &agents_link).expect("create symlinked .agents");

        let root = AbsolutePathBuf::from_absolute_path(&root).expect("absolute root");
        let agents_link_str = path_to_string(&agents_link);
        let policy = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
            path: FileSystemPath::Path { path: root },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        }]);

        let err =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect_err("protected symlinked subpath should fail closed");
        let message = err.to_string();

        assert!(
            message.contains("cannot enforce sandbox read-only path"),
            "{message}"
        );
        assert!(message.contains(&agents_link_str), "{message}");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_writable_roots_nested_symlink_escape_paths_fail_closed() {
        let temp_dir = TempDir::new().expect("temp dir");
        let real_root = temp_dir.path().join("real");
        let link_root = temp_dir.path().join("link");
        let outside = temp_dir.path().join("outside-private");
        let linked_private = real_root.join("linked-private");
        std::fs::create_dir_all(&real_root).expect("create real root");
        std::fs::create_dir_all(&outside).expect("create outside dir");
        std::os::unix::fs::symlink(&real_root, &link_root).expect("create symlinked root");
        std::os::unix::fs::symlink(&outside, &linked_private)
            .expect("create nested escape symlink");

        let link_root =
            AbsolutePathBuf::from_absolute_path(&link_root).expect("absolute symlinked root");
        let link_private = link_root.join("linked-private");
        let real_linked_private_str = path_to_string(&linked_private);
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Path { path: link_root },
                access: FileSystemAccessMode::Write,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path { path: link_private },
                access: FileSystemAccessMode::Deny,
                missing_path_behavior: None,
            },
        ]);

        let err =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect_err("deny-read path crossing writable symlink should fail closed");
        let message = err.to_string();

        assert!(
            message.contains("cannot enforce sandbox deny-read path"),
            "{message}"
        );
        assert!(message.contains(&real_linked_private_str), "{message}");
    }

    #[test]
    fn registry_protection_precedes_denied_parent_mask() {
        let registry_root = synthetic_mount_registry_root();
        let temp_root = registry_root.parent().expect("registry parent");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Write,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: AbsolutePathBuf::try_from(temp_root).expect("absolute temp root"),
                },
                access: FileSystemAccessMode::Deny,
                missing_path_behavior: None,
            },
        ]);

        let args =
            create_filesystem_args(&policy, Path::new("/"), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");
        let registry_path = path_to_string(&registry_root);
        let temp_path = path_to_string(temp_root);
        let registry_bind = args
            .args
            .windows(3)
            .position(|args| args == ["--ro-bind", registry_path.as_str(), registry_path.as_str()]);
        let temp_mask = args
            .args
            .windows(2)
            .position(|args| args == ["--tmpfs", temp_path.as_str()]);
        assert!(
            registry_bind.expect("registry read-only bind") < temp_mask.expect("temp deny mask"),
            "registry protection must not reopen denied temp paths"
        );
    }

    #[test]
    fn missing_read_only_subpath_uses_empty_file_bind_data() {
        let temp_dir = TempDir::new().expect("temp dir");
        let workspace = temp_dir.path().join("workspace");
        let blocked = workspace.join("blocked");
        std::fs::create_dir_all(&workspace).expect("create workspace");

        let workspace_root =
            AbsolutePathBuf::from_absolute_path(&workspace).expect("absolute workspace");
        let blocked_root = AbsolutePathBuf::from_absolute_path(&blocked).expect("absolute blocked");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: workspace_root,
                },
                access: FileSystemAccessMode::Write,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path { path: blocked_root },
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,
            },
        ]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");

        assert_empty_file_bound_without_perms(&args.args, &blocked);
        assert_empty_directory_mounted_read_only(&args.args, &workspace.join(".git"));
        assert_empty_directory_mounted_read_only(&args.args, &workspace.join(".agents"));
        assert_empty_directory_mounted_read_only(&args.args, &workspace.join(".codex"));
        assert_eq!(args.preserved_files.len(), 1);
        assert_eq!(
            synthetic_mount_target_paths(&args),
            vec![
                blocked.clone(),
                workspace.join(".git"),
                workspace.join(".agents"),
                workspace.join(".codex"),
            ]
        );
        assert!(
            !blocked.exists(),
            "missing path mask should not materialize host-side metadata paths at arg construction time",
        );
    }

    #[test]
    fn transient_empty_preserved_file_uses_empty_file_bind_data() {
        let temp_dir = TempDir::new().expect("temp dir");
        let workspace = temp_dir.path().join("workspace");
        let dot_git = workspace.join(".git");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        File::create(&dot_git).expect("create empty .git file");

        let workspace_root =
            AbsolutePathBuf::from_absolute_path(&workspace).expect("absolute workspace");
        let policy = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: workspace_root,
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        }]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");
        let dot_git_str = path_to_string(&dot_git);

        assert_empty_file_bound_without_perms(&args.args, &dot_git);
        assert_empty_directory_mounted_read_only(&args.args, &workspace.join(".agents"));
        assert_empty_directory_mounted_read_only(&args.args, &workspace.join(".codex"));
        assert_eq!(
            synthetic_mount_target_paths(&args),
            vec![
                dot_git.clone(),
                workspace.join(".agents"),
                workspace.join(".codex"),
            ]
        );
        assert!(
            !args
                .args
                .windows(3)
                .any(|window| window == ["--ro-bind", dot_git_str.as_str(), dot_git_str.as_str()]),
            "transient empty preserved file should not be treated as a stable bind source",
        );
        let metadata = std::fs::symlink_metadata(&dot_git).expect("stat .git");
        assert!(
            !args.synthetic_mount_targets[0].should_remove(
                metadata.file_type().is_file() && metadata.len() == 0,
                metadata.file_type().is_dir(),
                (metadata.dev(), metadata.ino()),
            ),
            "pre-existing empty preserved files must not be cleaned up as synthetic targets",
        );
    }

    #[test]
    fn missing_child_git_under_parent_repo_is_mounted_read_only() {
        let temp_dir = TempDir::new().expect("temp dir");
        let repo = temp_dir.path().join("repo");
        let workspace = repo.join("workspace");
        let dot_git = workspace.join(".git");
        std::fs::create_dir_all(repo.join(".git")).expect("create parent .git");
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").expect("write HEAD");
        std::fs::create_dir_all(&workspace).expect("create workspace");

        let workspace_root =
            AbsolutePathBuf::from_absolute_path(&workspace).expect("absolute workspace");
        let policy = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: workspace_root,
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        }]);

        let args = create_filesystem_args(&policy, &workspace, NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
            .expect("filesystem args");
        assert_empty_directory_mounted_read_only(&args.args, &dot_git);
        assert_empty_directory_mounted_read_only(&args.args, &workspace.join(".agents"));
        assert_empty_directory_mounted_read_only(&args.args, &workspace.join(".codex"));
        assert_eq!(
            synthetic_mount_target_paths(&args),
            vec![workspace.join(".codex"), dot_git, workspace.join(".agents")],
        );
        assert!(
            protected_create_target_paths(&args).is_empty(),
            "missing child .git should be mount protected before command execution",
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_missing_child_git_under_parent_repo_mounts_effective_path_read_only() {
        let temp_dir = TempDir::new().expect("temp dir");
        let repo = temp_dir.path().join("repo");
        let workspace = repo.join("workspace");
        let link_repo = temp_dir.path().join("link-repo");
        let link_workspace = link_repo.join("workspace");
        let dot_git = workspace.join(".git");
        std::fs::create_dir_all(repo.join(".git")).expect("create parent .git");
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").expect("write HEAD");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        std::os::unix::fs::symlink(&repo, &link_repo).expect("create symlinked repo");

        let link_workspace_root = AbsolutePathBuf::from_absolute_path(&link_workspace)
            .expect("absolute symlinked workspace");
        let policy = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: link_workspace_root,
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        }]);

        let args =
            create_filesystem_args(&policy, &link_workspace, NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");
        assert_empty_directory_mounted_read_only(&args.args, &dot_git);
        assert_empty_directory_mounted_read_only(&args.args, &workspace.join(".agents"));
        assert_empty_directory_mounted_read_only(&args.args, &workspace.join(".codex"));
        assert_eq!(
            synthetic_mount_target_paths(&args),
            vec![workspace.join(".codex"), dot_git, workspace.join(".agents")],
        );
        assert!(
            protected_create_target_paths(&args).is_empty(),
            "symlinked missing child .git should be mount protected before command execution",
        );
    }

    #[test]
    fn ignores_missing_writable_roots() {
        let temp_dir = TempDir::new().expect("temp dir");
        let existing_root = temp_dir.path().join("existing");
        let missing_root = temp_dir.path().join("missing");
        std::fs::create_dir(&existing_root).expect("create existing root");

        let policy = FileSystemSandboxPolicy::workspace_write(
            &[
                AbsolutePathBuf::try_from(existing_root.as_path()).expect("absolute existing root"),
                AbsolutePathBuf::try_from(missing_root.as_path()).expect("absolute missing root"),
            ],
            /*exclude_tmpdir_env_var*/ true,
            /*exclude_slash_tmp*/ true,
        );

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");
        let existing_root = path_to_string(&existing_root);
        let missing_root = path_to_string(&missing_root);

        assert!(
            args.args.windows(3).any(|window| {
                window == ["--bind", existing_root.as_str(), existing_root.as_str()]
            }),
            "existing writable root should be rebound writable",
        );
        assert!(
            !args.args.iter().any(|arg| arg == &missing_root),
            "missing writable root should be skipped",
        );
        assert_eq!(
            args.args.iter().filter(|arg| *arg == "--dev").count(),
            1,
            "a writable root below `/dev`'s parent keeps the single minimal `/dev`",
        );
    }

    /// #493: the writable `/` of a full-write profile with denials is
    /// followed by a fresh minimal `/dev`, before the deny masks and before a
    /// deeper writable `/dev/shm`.
    #[test]
    fn full_write_root_bind_is_followed_by_minimal_dev() {
        let temp_dir = TempDir::new().expect("temp dir");
        let denied = temp_dir.path().join("secret");
        std::fs::create_dir(&denied).expect("create denied dir");
        let entry = |path: &Path, access| FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::try_from(path).expect("absolute path"),
            },
            access,
            missing_path_behavior: None,
        };
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Write,
                missing_path_behavior: None,
            },
            entry(&denied, FileSystemAccessMode::Deny),
            entry(Path::new("/dev/full"), FileSystemAccessMode::Deny),
            entry(Path::new("/dev/shm"), FileSystemAccessMode::Write),
        ]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");
        let position = |window: &[&str]| {
            args.args
                .windows(window.len())
                .position(|candidate| candidate == window)
        };
        let root_bind = position(&["--bind", "/", "/", "--dev", "/dev"])
            .expect("writable `/` should be followed by `--dev /dev`");
        let denied = path_to_string(&denied);
        let deny_mask = args
            .args
            .iter()
            .rposition(|arg| arg == &denied)
            .expect("denied path should be masked");
        assert!(root_bind < deny_mask, "{:?}", args.args);
        // A denied `/dev` node is masked on top of the fresh `/dev`.
        let dev_mask = args
            .args
            .iter()
            .rposition(|arg| arg == "/dev/full")
            .expect("denied /dev/full should be masked");
        assert!(root_bind < dev_mask, "{:?}", args.args);
        if Path::new("/dev/shm").exists() {
            let shm_bind = position(&["--bind", "/dev/shm", "/dev/shm"])
                .expect("writable `/dev/shm` should be rebound");
            assert!(root_bind < shm_bind, "{:?}", args.args);
        }
    }

    /// #503: a writable root that is a symlink to `/` is ordered by its
    /// target, so it is bound before shallower roots instead of covering
    /// them (and their masks) afterwards, and is followed by a fresh `/dev`.
    #[cfg(unix)]
    #[test]
    fn writable_symlink_to_filesystem_root_is_bound_first() {
        let temp_dir = TempDir::new().expect("temp dir");
        let workspace = temp_dir.path().join("ws");
        let denied_ancestor = temp_dir.path().join("a");
        let link = denied_ancestor.join("b").join("c").join("root-link");
        let denied = workspace.join("secret");
        std::fs::create_dir_all(link.parent().expect("link parent")).expect("create link parent");
        std::fs::create_dir(&workspace).expect("create workspace");
        std::fs::write(&denied, "secret").expect("create denied file");
        std::os::unix::fs::symlink("/", &link).expect("create symlink to /");
        let entry = |path: &Path, access| FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::try_from(path).expect("absolute path"),
            },
            access,
            missing_path_behavior: None,
        };
        let policy = FileSystemSandboxPolicy::restricted(vec![
            entry(&workspace, FileSystemAccessMode::Write),
            entry(&link, FileSystemAccessMode::Write),
            entry(&denied, FileSystemAccessMode::Deny),
            entry(&denied_ancestor, FileSystemAccessMode::Deny),
        ]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");
        let position = |window: &[&str]| {
            args.args
                .windows(window.len())
                .position(|candidate| candidate == window)
        };
        let root_bind = position(&["--bind", "/", "/", "--dev", "/dev"])
            .expect("the symlink to `/` should bind `/` followed by `--dev /dev`");
        let workspace = path_to_string(&workspace);
        let workspace_bind = position(&["--bind", workspace.as_str(), workspace.as_str()])
            .expect("workspace should be bound");
        assert!(root_bind < workspace_bind, "{:?}", args.args);
        let denied = path_to_string(&denied);
        let deny_mask = args
            .args
            .iter()
            .rposition(|arg| arg == &denied)
            .expect("denied file should be masked");
        assert!(workspace_bind < deny_mask, "{:?}", args.args);
        // The link is bound at its target, never at its own spelling, and its
        // denied ancestor is masked after the `/` bind.
        let link = path_to_string(&link);
        assert_eq!(
            position(&["--bind", link.as_str(), link.as_str()]),
            None,
            "{:?}",
            args.args
        );
        let denied_ancestor = path_to_string(&denied_ancestor);
        let ancestor_mask = args
            .args
            .iter()
            .rposition(|arg| arg == &denied_ancestor)
            .expect("denied ancestor should be masked");
        assert!(root_bind < ancestor_mask, "{:?}", args.args);
    }

    /// #503: a link whose target lies inside another writable root is bound
    /// after it (deeper real path) and re-applies that root's read-only paths
    /// below the target, which its own bind would otherwise cover.
    #[cfg(unix)]
    #[test]
    fn symlinked_root_inside_another_root_reapplies_its_read_only_paths() {
        let temp_dir = TempDir::new().expect("temp dir");
        let outer = temp_dir.path().join("outer");
        let target = outer.join("c").join("d");
        let read_only = target.join("ro");
        let link = temp_dir.path().join("link");
        std::fs::create_dir_all(&read_only).expect("create read-only dir");
        std::os::unix::fs::symlink(&target, &link).expect("create link");
        let entry = |path: &Path, access| FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::try_from(path).expect("absolute path"),
            },
            access,
            missing_path_behavior: None,
        };
        let policy = FileSystemSandboxPolicy::restricted(vec![
            entry(&outer, FileSystemAccessMode::Write),
            entry(&link, FileSystemAccessMode::Write),
            entry(&read_only, FileSystemAccessMode::Read),
        ]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");
        let target = path_to_string(&target);
        let target_bind = args
            .args
            .windows(3)
            .position(|window| window == ["--bind", target.as_str(), target.as_str()])
            .expect("link should be bound at its target");
        let read_only = path_to_string(&read_only);
        let read_only_bind = args
            .args
            .windows(3)
            .rposition(|window| window == ["--ro-bind", read_only.as_str(), read_only.as_str()])
            .expect("read-only path should be bound read-only");
        assert!(target_bind < read_only_bind, "{:?}", args.args);
    }

    /// #536: with pinned roots, each writable root is bound with `--bind-fd`
    /// from the folder opened at its pinned location (never `--bind` by
    /// path), each `--ro-bind-data` mask reads its own `/dev/null`
    /// descriptor, a root swapped for a link is refused, and so is a
    /// bubblewrap without `--bind-fd`.
    #[test]
    fn pinned_writable_roots_are_bound_from_checked_folders() {
        let temp_dir = TempDir::new().expect("temp dir");
        let base = temp_dir.path().canonicalize().expect("canonical");
        let workspace = base.join("ws");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let secret = workspace.join("secret.txt");
        std::fs::write(&secret, "x").expect("secret");
        let entry = |path: &Path, access| FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::try_from(path).expect("absolute path"),
            },
            access,
            missing_path_behavior: None,
        };
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,
            },
            entry(&workspace, FileSystemAccessMode::Write),
            entry(&secret, FileSystemAccessMode::Deny),
        ]);
        let roots: &'static [PinnedWritableRoot] = Box::leak(
            vec![PinnedWritableRoot {
                root: AbsolutePathBuf::try_from(workspace.as_path()).expect("absolute"),
                real: workspace.clone(),
            }]
            .into_boxed_slice(),
        );
        let pins = |bind_fd| Some(WritableRootPins { roots, bind_fd });
        let args = create_filesystem_args_with_pins(
            &policy,
            &workspace,
            NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH,
            pins(true),
        )
        .expect("filesystem args");
        let ws = path_to_string(&workspace);
        assert!(
            !args
                .args
                .windows(3)
                .any(|window| window == ["--bind", ws.as_str(), ws.as_str()]),
            "{:?}",
            args.args
        );
        let bind = args
            .args
            .windows(3)
            .find(|window| window[0] == "--bind-fd" && window[2] == ws)
            .expect("bound with --bind-fd");
        let fd_target =
            |fd: &str| std::fs::read_link(format!("/proc/self/fd/{fd}")).expect("preserved fd");
        assert_eq!(fd_target(&bind[1]), workspace);
        // The masks read `/dev/null`, each through its own descriptor.
        let masks = args
            .args
            .windows(3)
            .filter(|window| window[0] == "--ro-bind-data")
            .map(|window| window[1].clone())
            .collect::<Vec<_>>();
        assert!(!masks.is_empty(), "{:?}", args.args);
        for fd in &masks {
            assert_eq!(fd_target(fd), PathBuf::from("/dev/null"));
        }
        assert_eq!(
            masks.iter().collect::<std::collections::HashSet<_>>().len(),
            masks.len(),
            "{masks:?}"
        );

        assert!(
            create_filesystem_args_with_pins(
                &policy,
                &workspace,
                NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH,
                pins(false),
            )
            .is_err()
        );
        std::fs::rename(&workspace, base.join("moved")).expect("move away");
        std::fs::create_dir(base.join("elsewhere")).expect("elsewhere");
        std::os::unix::fs::symlink(base.join("elsewhere"), &workspace).expect("link");
        let err = create_filesystem_args_with_pins(
            &policy,
            &workspace,
            NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH,
            pins(true),
        )
        .expect_err("a swapped root is refused");
        assert!(
            err.to_string().contains("Refused to run the command"),
            "{err}"
        );
    }

    /// #503: a writable link to a workspace's `.git` cannot make it writable;
    /// since #535 the link is no writable root at all.
    #[cfg(unix)]
    #[test]
    fn writable_link_to_protected_git_keeps_it_read_only() {
        let temp_dir = TempDir::new().expect("temp dir");
        let workspace = temp_dir.path().join("p").join("q").join("w");
        let git = workspace.join(".git");
        let link = temp_dir.path().join("l");
        std::fs::create_dir_all(&git).expect("create .git");
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("write HEAD");
        std::os::unix::fs::symlink(&git, &link).expect("create link");
        let entry = |path: &Path| FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::try_from(path).expect("absolute path"),
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        };
        let policy = FileSystemSandboxPolicy::restricted(vec![entry(&workspace), entry(&link)]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");
        let git = path_to_string(&git);
        // #535 (Travis, 2026-10-12): the link is no grant, so `.git` is never
        // bound writable, and stays read-only.
        assert!(
            !args
                .args
                .windows(3)
                .any(|window| window == ["--bind", git.as_str(), git.as_str()]),
            "{:?}",
            args.args
        );
        assert!(
            args.args
                .windows(3)
                .any(|window| window == ["--ro-bind", git.as_str(), git.as_str()]),
            "{:?}",
            args.args
        );
    }

    /// #503: a denial spelled through a link is masked at the link's target.
    /// A writable root bound later inside that target (deeper, or the same
    /// directory) masks it again after its own bind.
    #[cfg(unix)]
    #[test]
    fn denial_spelled_through_a_link_survives_later_binds_of_its_target() {
        let entry = |path: &Path, access| FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::try_from(path).expect("absolute path"),
            },
            access,
            missing_path_behavior: None,
        };
        let masked_after_last_bind = |args: &BwrapArgs, bound: &Path, denied: &Path| {
            let bound = path_to_string(bound);
            let denied = path_to_string(denied);
            let last_bind = args
                .args
                .windows(3)
                .rposition(|window| window == ["--bind", bound.as_str(), bound.as_str()])
                .expect("writable root should be bound");
            let deny_mask = args
                .args
                .iter()
                .rposition(|arg| arg == &denied)
                .expect("denial should be masked at the link target");
            assert!(last_bind < deny_mask, "{:?}", args.args);
        };

        // The link is spelled deeper than its target, which holds a deeper
        // writable root.
        let temp_dir = TempDir::new().expect("temp dir");
        let target = temp_dir.path().join("x");
        let nested = target.join("sub");
        let link = temp_dir.path().join("a").join("b").join("c").join("l");
        std::fs::create_dir_all(&nested).expect("create nested root");
        std::fs::write(nested.join("secret"), "secret").expect("create secret");
        std::fs::create_dir_all(link.parent().expect("link parent")).expect("link parent");
        std::os::unix::fs::symlink(&target, &link).expect("create link");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            entry(&link, FileSystemAccessMode::Write),
            entry(&nested, FileSystemAccessMode::Write),
            entry(&link.join("sub").join("secret"), FileSystemAccessMode::Deny),
        ]);
        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");
        masked_after_last_bind(&args, &nested, &nested.join("secret"));

        // The link and a writable root bind the same directory.
        let temp_dir = TempDir::new().expect("temp dir");
        let target = temp_dir.path().join("x");
        let link = temp_dir.path().join("a").join("b").join("l");
        std::fs::create_dir_all(&target).expect("create target");
        std::fs::write(target.join("secret"), "secret").expect("create secret");
        std::fs::create_dir_all(link.parent().expect("link parent")).expect("link parent");
        std::os::unix::fs::symlink(&target, &link).expect("create link");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            entry(&link, FileSystemAccessMode::Write),
            entry(&target, FileSystemAccessMode::Write),
            entry(&link.join("secret"), FileSystemAccessMode::Deny),
        ]);
        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");
        masked_after_last_bind(&args, &target, &target.join("secret"));
    }

    /// #503: with `:root = write`, an extra writable link to `/` binds `/`
    /// again; read-only paths and denials outside every workspace are applied
    /// after the last `/` bind.
    #[cfg(unix)]
    #[test]
    fn full_write_root_and_link_to_root_keep_protections_outside_workspaces() {
        let temp_dir = TempDir::new().expect("temp dir");
        let read_only = temp_dir.path().join("ro");
        let denied = temp_dir.path().join("secret");
        let link = temp_dir.path().join("root-link");
        std::fs::create_dir(&read_only).expect("create read-only dir");
        std::fs::write(&denied, "secret").expect("create denied file");
        std::os::unix::fs::symlink("/", &link).expect("create symlink to /");
        let entry = |path: &Path, access| FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::try_from(path).expect("absolute path"),
            },
            access,
            missing_path_behavior: None,
        };
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Write,
                missing_path_behavior: None,
            },
            entry(&link, FileSystemAccessMode::Write),
            entry(&read_only, FileSystemAccessMode::Read),
            entry(&denied, FileSystemAccessMode::Deny),
        ]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");
        let last_root_bind = args
            .args
            .windows(3)
            .rposition(|window| window == ["--bind", "/", "/"])
            .expect("`/` should be bound writable");
        let read_only = path_to_string(&read_only);
        let read_only_bind = args
            .args
            .windows(3)
            .rposition(|window| window == ["--ro-bind", read_only.as_str(), read_only.as_str()])
            .expect("read-only path should be bound read-only");
        assert!(last_root_bind < read_only_bind, "{:?}", args.args);
        let denied = path_to_string(&denied);
        let deny_mask = args
            .args
            .iter()
            .rposition(|arg| arg == &denied)
            .expect("denied file should be masked");
        assert!(last_root_bind < deny_mask, "{:?}", args.args);
        // Neither pass asks for mount points at the filesystem root.
        assert!(
            !args
                .args
                .iter()
                .any(|arg| arg == "/.git" || arg == "/.codex"),
            "{:?}",
            args.args
        );
    }

    #[test]
    fn missing_project_root_metadata_carveouts_use_metadata_path_masks() {
        let temp_dir = TempDir::new().expect("temp dir");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,
            },
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
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::project_roots(Some(".agents".into())),
                },
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::project_roots(Some(".codex".into())),
                },
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,
            },
        ]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");
        let dot_git = path_to_string(&temp_dir.path().join(".git"));
        let dot_agents = path_to_string(&temp_dir.path().join(".agents"));
        let dot_codex = path_to_string(&temp_dir.path().join(".codex"));

        assert_empty_directory_mounted_read_only(&args.args, Path::new(&dot_git));
        assert_empty_directory_mounted_read_only(&args.args, Path::new(&dot_agents));
        assert_empty_directory_mounted_read_only(&args.args, Path::new(&dot_codex));
        assert!(args.preserved_files.is_empty());
        let synthetic_targets = synthetic_mount_target_paths(&args);
        assert!(synthetic_targets.contains(&PathBuf::from(&dot_git)));
        assert!(synthetic_targets.contains(&PathBuf::from(&dot_agents)));
        assert!(synthetic_targets.contains(&PathBuf::from(&dot_codex)));
        assert_eq!(
            protected_create_target_paths(&args),
            Vec::<PathBuf>::new(),
            "missing protected metadata paths should fail at creation time through read-only mounts",
        );
    }

    #[test]
    fn missing_user_project_root_subpath_rules_are_still_enforced() {
        let temp_dir = TempDir::new().expect("temp dir");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::project_roots(/*subpath*/ None),
                },
                access: FileSystemAccessMode::Write,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::project_roots(Some(".vscode".into())),
                },
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::project_roots(Some(".secrets".into())),
                },
                access: FileSystemAccessMode::Deny,
                missing_path_behavior: None,
            },
        ]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");
        let dot_vscode = path_to_string(&temp_dir.path().join(".vscode"));
        let dot_secrets = path_to_string(&temp_dir.path().join(".secrets"));

        assert_empty_file_bound_without_perms(&args.args, Path::new(&dot_vscode));
        assert_empty_file_bound_without_perms(&args.args, Path::new(&dot_secrets));
    }

    /// PF-23: full access with denials (`:root = write`) masks no repository
    /// metadata at `/`, which an unprivileged bwrap could not create, also
    /// when the command runs in `/`.
    #[test]
    fn full_write_root_with_denials_creates_nothing_at_the_filesystem_root() {
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Write,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: AbsolutePathBuf::try_from(Path::new("/tmp/pf23-nonexistent-secret"))
                        .expect("absolute"),
                },
                access: FileSystemAccessMode::Deny,
                missing_path_behavior: None,
            },
        ]);
        for cwd in ["/", "/tmp"] {
            let args =
                create_filesystem_args(&policy, Path::new(cwd), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                    .expect("bwrap fs args");
            let at_root: Vec<PathBuf> = synthetic_mount_target_paths(&args)
                .into_iter()
                .filter(|path| {
                    [".git", ".agents", ".codex"]
                        .iter()
                        .any(|name| path == &Path::new("/").join(name))
                })
                .collect();
            assert_eq!(at_root, Vec::<PathBuf>::new(), "cwd {cwd}");
            assert!(
                !args
                    .args
                    .iter()
                    .any(|arg| arg == "/.git" || arg == "/.codex"),
                "cwd {cwd}: {:?}",
                args.args
            );
        }
    }

    #[test]
    fn mounts_dev_before_writable_dev_binds() {
        let sandbox_policy = FileSystemSandboxPolicy::workspace_write(
            &[AbsolutePathBuf::try_from(Path::new("/dev")).expect("/dev path")],
            /*exclude_tmpdir_env_var*/ true,
            /*exclude_slash_tmp*/ true,
        );

        let args = create_filesystem_args(
            &sandbox_policy,
            Path::new("/"),
            NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH,
        )
        .expect("bwrap fs args");
        assert!(args.preserved_files.is_empty());
        assert_eq!(
            synthetic_mount_target_paths(&args),
            vec![
                PathBuf::from("/.git"),
                PathBuf::from("/.agents"),
                PathBuf::from("/.codex"),
                PathBuf::from("/dev/.git"),
                PathBuf::from("/dev/.agents"),
                PathBuf::from("/dev/.codex"),
            ]
        );
        assert_eq!(
            args.args,
            vec![
                // Start from a read-only view of the full filesystem.
                "--ro-bind".to_string(),
                "/".to_string(),
                "/".to_string(),
                // Recreate a writable /dev inside the sandbox.
                "--dev".to_string(),
                "/dev".to_string(),
                // Make the writable root itself writable again.
                "--bind".to_string(),
                "/".to_string(),
                "/".to_string(),
                // #493: the `/` bind covers the minimal /dev with the host's,
                // mounted `nodev`; mount a fresh one over it.
                "--dev".to_string(),
                "/dev".to_string(),
                // Mask the default metadata path names under the writable root.
                // Because the root is `/` in this test, these carveout paths
                // appear directly below `/`.
                "--perms".to_string(),
                "555".to_string(),
                "--tmpfs".to_string(),
                "/.git".to_string(),
                "--remount-ro".to_string(),
                "/.git".to_string(),
                "--perms".to_string(),
                "555".to_string(),
                "--tmpfs".to_string(),
                "/.agents".to_string(),
                "--remount-ro".to_string(),
                "/.agents".to_string(),
                "--perms".to_string(),
                "555".to_string(),
                "--tmpfs".to_string(),
                "/.codex".to_string(),
                "--remount-ro".to_string(),
                "/.codex".to_string(),
                "--ro-bind".to_string(),
                path_to_string(&synthetic_mount_registry_root()),
                path_to_string(&synthetic_mount_registry_root()),
                // The explicit writable /dev root binds the host /dev over
                // the fresh one (still `nodev`, as before #493).
                "--bind".to_string(),
                "/dev".to_string(),
                "/dev".to_string(),
                // Then mask the metadata names that would otherwise be
                // creatable below the writable /dev bind.
                "--perms".to_string(),
                "555".to_string(),
                "--tmpfs".to_string(),
                "/dev/.git".to_string(),
                "--remount-ro".to_string(),
                "/dev/.git".to_string(),
                "--perms".to_string(),
                "555".to_string(),
                "--tmpfs".to_string(),
                "/dev/.agents".to_string(),
                "--remount-ro".to_string(),
                "/dev/.agents".to_string(),
                "--perms".to_string(),
                "555".to_string(),
                "--tmpfs".to_string(),
                "/dev/.codex".to_string(),
                "--remount-ro".to_string(),
                "/dev/.codex".to_string(),
            ]
        );
    }

    #[test]
    fn restricted_read_only_uses_scoped_read_roots_instead_of_erroring() {
        let temp_dir = TempDir::new().expect("temp dir");
        let readable_root = temp_dir.path().join("readable");
        std::fs::create_dir(&readable_root).expect("create readable root");

        let policy = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
            path: FileSystemPath::Path {
                path: AbsolutePathBuf::try_from(readable_root.as_path())
                    .expect("absolute readable root"),
            },
            access: FileSystemAccessMode::Read,
            missing_path_behavior: None,
        }]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");

        assert_eq!(args.args[0..4], ["--tmpfs", "/", "--dev", "/dev"]);

        let readable_root_str = path_to_string(&readable_root);
        assert!(args.args.windows(3).any(|window| {
            window
                == [
                    "--ro-bind",
                    readable_root_str.as_str(),
                    readable_root_str.as_str(),
                ]
        }));
    }

    #[test]
    fn restricted_read_only_with_platform_defaults_includes_usr_when_present() {
        let temp_dir = TempDir::new().expect("temp dir");
        let policy = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Minimal,
            },
            access: FileSystemAccessMode::Read,
            missing_path_behavior: None,
        }]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");

        assert!(
            args.args
                .starts_with(&["--tmpfs".to_string(), "/".to_string()])
        );

        if Path::new("/usr").exists() {
            assert!(
                args.args
                    .windows(3)
                    .any(|window| window == ["--ro-bind", "/usr", "/usr"])
            );
        }
    }

    #[test]
    fn split_policy_reapplies_unreadable_carveouts_after_writable_binds() {
        let temp_dir = TempDir::new().expect("temp dir");
        let writable_root = temp_dir.path().join("workspace");
        let blocked = writable_root.join("blocked");
        std::fs::create_dir_all(&blocked).expect("create blocked dir");
        let writable_root =
            AbsolutePathBuf::from_absolute_path(&writable_root).expect("absolute writable root");
        let blocked = AbsolutePathBuf::from_absolute_path(&blocked).expect("absolute blocked dir");
        let writable_root_str = path_to_string(writable_root.as_path());
        let blocked_str = path_to_string(blocked.as_path());
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: writable_root,
                },
                access: FileSystemAccessMode::Write,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path { path: blocked },
                access: FileSystemAccessMode::Deny,
                missing_path_behavior: None,
            },
        ]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");

        assert!(args.args.windows(3).any(|window| {
            window
                == [
                    "--bind",
                    writable_root_str.as_str(),
                    writable_root_str.as_str(),
                ]
        }));
        let blocked_mask_index = args
            .args
            .windows(6)
            .position(|window| {
                window
                    == [
                        "--perms",
                        "000",
                        "--tmpfs",
                        blocked_str.as_str(),
                        "--remount-ro",
                        blocked_str.as_str(),
                    ]
            })
            .expect("blocked directory should be remounted unreadable");

        let writable_root_bind_index = args
            .args
            .windows(3)
            .position(|window| {
                window
                    == [
                        "--bind",
                        writable_root_str.as_str(),
                        writable_root_str.as_str(),
                    ]
            })
            .expect("writable root should be rebound writable");

        assert!(
            writable_root_bind_index < blocked_mask_index,
            "expected unreadable carveout to be re-applied after writable bind: {:#?}",
            args.args
        );
    }

    #[test]
    fn split_policy_reenables_nested_writable_subpaths_after_read_only_parent() {
        let temp_dir = TempDir::new().expect("temp dir");
        let writable_root = temp_dir.path().join("workspace");
        let docs = writable_root.join("docs");
        let docs_public = docs.join("public");
        std::fs::create_dir_all(&docs_public).expect("create docs/public");
        let writable_root =
            AbsolutePathBuf::from_absolute_path(&writable_root).expect("absolute writable root");
        let docs = AbsolutePathBuf::from_absolute_path(&docs).expect("absolute docs");
        let docs_public =
            AbsolutePathBuf::from_absolute_path(&docs_public).expect("absolute docs/public");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: writable_root,
                },
                access: FileSystemAccessMode::Write,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path { path: docs.clone() },
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: docs_public.clone(),
                },
                access: FileSystemAccessMode::Write,
                missing_path_behavior: None,
            },
        ]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");
        let docs_str = path_to_string(docs.as_path());
        let docs_public_str = path_to_string(docs_public.as_path());
        let docs_ro_index = args
            .args
            .windows(3)
            .position(|window| window == ["--ro-bind", docs_str.as_str(), docs_str.as_str()])
            .expect("docs should be remounted read-only");
        let docs_public_rw_index = args
            .args
            .windows(3)
            .position(|window| {
                window == ["--bind", docs_public_str.as_str(), docs_public_str.as_str()]
            })
            .expect("docs/public should be rebound writable");

        assert!(
            docs_ro_index < docs_public_rw_index,
            "expected read-only parent remount before nested writable bind: {:#?}",
            args.args
        );
    }

    #[test]
    fn split_policy_reenables_writable_subpaths_after_unreadable_parent() {
        let temp_dir = TempDir::new().expect("temp dir");
        let blocked = temp_dir.path().join("blocked");
        let allowed = blocked.join("allowed");
        std::fs::create_dir_all(&allowed).expect("create blocked/allowed");
        let blocked = AbsolutePathBuf::from_absolute_path(&blocked).expect("absolute blocked");
        let allowed = AbsolutePathBuf::from_absolute_path(&allowed).expect("absolute allowed");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: blocked.clone(),
                },
                access: FileSystemAccessMode::Deny,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: allowed.clone(),
                },
                access: FileSystemAccessMode::Write,
                missing_path_behavior: None,
            },
        ]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");
        let blocked_str = path_to_string(blocked.as_path());
        let allowed_str = path_to_string(allowed.as_path());
        let blocked_none_index = args
            .args
            .windows(4)
            .position(|window| window == ["--perms", "111", "--tmpfs", blocked_str.as_str()])
            .expect("blocked should be masked first");
        let allowed_dir_index = args
            .args
            .windows(2)
            .position(|window| window == ["--dir", allowed_str.as_str()])
            .expect("allowed mount target should be recreated");
        let blocked_remount_ro_index = args
            .args
            .windows(2)
            .position(|window| window == ["--remount-ro", blocked_str.as_str()])
            .expect("blocked directory should be remounted read-only");
        let allowed_bind_index = args
            .args
            .windows(3)
            .position(|window| window == ["--bind", allowed_str.as_str(), allowed_str.as_str()])
            .expect("allowed path should be rebound writable");

        assert!(
            blocked_none_index < allowed_dir_index
                && allowed_dir_index < blocked_remount_ro_index
                && blocked_remount_ro_index < allowed_bind_index,
            "expected writable child target recreation before remounting and rebinding under unreadable parent: {:#?}",
            args.args
        );
    }

    #[test]
    fn split_policy_reenables_writable_files_after_unreadable_parent() {
        let temp_dir = TempDir::new().expect("temp dir");
        let blocked = temp_dir.path().join("blocked");
        let allowed_dir = blocked.join("allowed");
        let allowed_file = allowed_dir.join("note.txt");
        std::fs::create_dir_all(&allowed_dir).expect("create blocked/allowed");
        std::fs::write(&allowed_file, "ok").expect("create note");
        let blocked = AbsolutePathBuf::from_absolute_path(&blocked).expect("absolute blocked");
        let allowed_dir =
            AbsolutePathBuf::from_absolute_path(&allowed_dir).expect("absolute allowed dir");
        let allowed_file =
            AbsolutePathBuf::from_absolute_path(&allowed_file).expect("absolute allowed file");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: blocked.clone(),
                },
                access: FileSystemAccessMode::Deny,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: allowed_file.clone(),
                },
                access: FileSystemAccessMode::Write,
                missing_path_behavior: None,
            },
        ]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");
        let blocked_str = path_to_string(blocked.as_path());
        let allowed_dir_str = path_to_string(allowed_dir.as_path());
        let allowed_file_str = path_to_string(allowed_file.as_path());

        assert!(
            args.args
                .windows(2)
                .any(|window| window == ["--dir", allowed_dir_str.as_str()]),
            "expected ancestor directory to be recreated: {:#?}",
            args.args
        );
        assert!(
            !args
                .args
                .windows(2)
                .any(|window| window == ["--dir", allowed_file_str.as_str()]),
            "writable file target should not be converted into a directory: {:#?}",
            args.args
        );
        let blocked_none_index = args
            .args
            .windows(4)
            .position(|window| window == ["--perms", "111", "--tmpfs", blocked_str.as_str()])
            .expect("blocked should be masked first");
        let allowed_bind_index = args
            .args
            .windows(3)
            .position(|window| {
                window
                    == [
                        "--bind",
                        allowed_file_str.as_str(),
                        allowed_file_str.as_str(),
                    ]
            })
            .expect("allowed file should be rebound writable");

        assert!(
            blocked_none_index < allowed_bind_index,
            "expected unreadable parent mask before rebinding writable file child: {:#?}",
            args.args
        );
    }

    #[test]
    fn split_policy_reenables_nested_writable_roots_after_unreadable_parent() {
        let temp_dir = TempDir::new().expect("temp dir");
        let writable_root = temp_dir.path().join("workspace");
        let blocked = writable_root.join("blocked");
        let allowed = blocked.join("allowed");
        std::fs::create_dir_all(&allowed).expect("create blocked/allowed dir");
        let writable_root =
            AbsolutePathBuf::from_absolute_path(&writable_root).expect("absolute writable root");
        let blocked = AbsolutePathBuf::from_absolute_path(&blocked).expect("absolute blocked dir");
        let allowed = AbsolutePathBuf::from_absolute_path(&allowed).expect("absolute allowed dir");
        let blocked_str = path_to_string(blocked.as_path());
        let allowed_str = path_to_string(allowed.as_path());
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: writable_root,
                },
                access: FileSystemAccessMode::Write,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path { path: blocked },
                access: FileSystemAccessMode::Deny,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path { path: allowed },
                access: FileSystemAccessMode::Write,
                missing_path_behavior: None,
            },
        ]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");
        let blocked_none_index = args
            .args
            .windows(4)
            .position(|window| window == ["--perms", "111", "--tmpfs", blocked_str.as_str()])
            .expect("blocked should be masked first");
        let allowed_dir_index = args
            .args
            .windows(2)
            .position(|window| window == ["--dir", allowed_str.as_str()])
            .expect("allowed mount target should be recreated");
        let allowed_bind_index = args
            .args
            .windows(3)
            .position(|window| window == ["--bind", allowed_str.as_str(), allowed_str.as_str()])
            .expect("allowed path should be rebound writable");

        assert!(
            blocked_none_index < allowed_dir_index && allowed_dir_index < allowed_bind_index,
            "expected unreadable parent mask before recreating and rebinding writable child: {:#?}",
            args.args
        );
    }

    #[test]
    fn split_policy_masks_root_read_directory_carveouts() {
        let temp_dir = TempDir::new().expect("temp dir");
        let blocked = temp_dir.path().join("blocked");
        std::fs::create_dir_all(&blocked).expect("create blocked dir");
        let blocked = AbsolutePathBuf::from_absolute_path(&blocked).expect("absolute blocked dir");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: blocked.clone(),
                },
                access: FileSystemAccessMode::Deny,
                missing_path_behavior: None,
            },
        ]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");
        let blocked_str = path_to_string(blocked.as_path());

        assert!(
            args.args
                .windows(3)
                .any(|window| window == ["--ro-bind", "/", "/"])
        );
        assert!(
            args.args
                .windows(4)
                .any(|window| { window == ["--perms", "000", "--tmpfs", blocked_str.as_str()] })
        );
        assert!(
            args.args
                .windows(2)
                .any(|window| window == ["--remount-ro", blocked_str.as_str()])
        );
    }

    #[test]
    fn split_policy_masks_root_read_file_carveouts() {
        let temp_dir = TempDir::new().expect("temp dir");
        let blocked_file = temp_dir.path().join("blocked.txt");
        std::fs::write(&blocked_file, "secret").expect("create blocked file");
        let blocked_file =
            AbsolutePathBuf::from_absolute_path(&blocked_file).expect("absolute blocked file");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,
            },
            FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: blocked_file.clone(),
                },
                access: FileSystemAccessMode::Deny,
                missing_path_behavior: None,
            },
        ]);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");
        let blocked_file_str = path_to_string(blocked_file.as_path());

        assert_eq!(args.preserved_files.len(), 1);
        assert!(args.synthetic_mount_targets.is_empty());
        assert!(args.args.windows(5).any(|window| {
            window[0] == "--perms"
                && window[1] == "000"
                && window[2] == "--ro-bind-data"
                && window[4] == blocked_file_str
        }));
    }

    /// #636: bubblewrap closes a `--ro-bind-data` descriptor once it has read
    /// it, so two masked files must not share one.
    #[test]
    fn every_masked_file_gets_its_own_null_descriptor() {
        let temp_dir = TempDir::new().expect("temp dir");
        let mut entries = vec![FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Read,
            missing_path_behavior: None,
        }];
        let mut blocked = Vec::new();
        for name in ["first.txt", "second.txt", "third.txt"] {
            let path = temp_dir.path().join(name);
            std::fs::write(&path, "secret").expect("create blocked file");
            entries.push(FileSystemSandboxEntry {
                path: FileSystemPath::Path {
                    path: AbsolutePathBuf::from_absolute_path(&path).expect("absolute"),
                },
                access: FileSystemAccessMode::Deny,
                missing_path_behavior: None,
            });
            blocked.push(path_to_string(&path));
        }
        let policy = FileSystemSandboxPolicy::restricted(entries);

        let args =
            create_filesystem_args(&policy, temp_dir.path(), NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args");

        let masks = args
            .args
            .windows(3)
            .filter(|window| window[0] == "--ro-bind-data")
            .map(|window| (window[2].clone(), window[1].clone()))
            .collect::<Vec<_>>();
        let mut masked = masks
            .iter()
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        masked.sort();
        assert_eq!(masked, blocked);
        let fds = masks
            .iter()
            .map(|(_, fd)| fd.clone())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(fds.len(), blocked.len(), "{masks:?}");
        for fd in fds {
            assert_eq!(
                std::fs::read_link(format!("/proc/self/fd/{fd}")).expect("preserved fd"),
                PathBuf::from("/dev/null")
            );
        }
        assert_eq!(args.preserved_files.len(), blocked.len());
    }

    #[test]
    fn unreadable_globs_expand_existing_matches_with_configured_depth() {
        if !ripgrep_available() {
            return;
        }

        let temp_dir = TempDir::new().expect("temp dir");
        let root_env = temp_dir.path().join(".env");
        let nested_env = temp_dir.path().join("app").join(".env");
        let too_deep_env = temp_dir.path().join("app").join("deep").join(".env");
        std::fs::create_dir_all(too_deep_env.parent().expect("parent")).expect("create parent");
        std::fs::write(temp_dir.path().join(".gitignore"), ".env\n").expect("write gitignore");
        std::fs::write(&root_env, "secret").expect("write root env");
        std::fs::write(&nested_env, "secret").expect("write nested env");
        std::fs::write(&too_deep_env, "secret").expect("write deep env");
        let policy =
            default_policy_with_unreadable_glob(format!("{}/**/*.env", temp_dir.path().display()));

        let args =
            create_filesystem_args(&policy, temp_dir.path(), Some(2)).expect("filesystem args");

        assert_file_masked(&args.args, &root_env);
        assert_file_masked(&args.args, &nested_env);
        assert!(
            !args
                .args
                .iter()
                .any(|arg| arg == &path_to_string(&too_deep_env)),
            "max depth should keep deeper matches out of bwrap args: {:#?}",
            args.args
        );
    }

    /// #442 (Travis, 2026-10-12): the credential globs hide `.env` and
    /// `.env.local` but not the templates `.env.example`, `.env.sample` and
    /// `.env.template`; a person's own glob that covers a template still
    /// hides it.
    #[test]
    fn credential_globs_leave_templates_readable() {
        let temp_dir = TempDir::new().expect("temp dir");
        let root = temp_dir.path();
        for name in [
            ".env",
            ".env.local",
            ".env.example",
            ".env.sample",
            ".env.template",
        ] {
            std::fs::write(root.join(name), "x").expect("write");
        }
        let expand = |globs: Vec<String>| {
            let globs: Vec<FileSystemDenyGlob> = globs
                .into_iter()
                .map(|pattern| FileSystemDenyGlob {
                    pattern,
                    scan: None,
                })
                .collect();
            expand_unreadable_globs(&globs, root, /*max_depth*/ None, ripgrep_files)
                .expect("expand")
                .into_iter()
                .map(|path| path.as_path().file_name().expect("name").to_os_string())
                .collect::<Vec<_>>()
        };
        let credential =
            codex_protocol::permissions::credential_file_globs(&root.to_string_lossy());
        assert_eq!(expand(credential.clone()), vec![".env", ".env.local"]);
        let mut own = credential;
        own.push(format!("{}/**/*.sample", root.display()));
        assert_eq!(expand(own), vec![".env", ".env.local", ".env.sample"]);
        // A person's own `**/.env.*` alone is not the credential rule.
        assert_eq!(
            expand(vec![format!("{}/**/.env.*", root.display())]),
            vec![".env.example", ".env.local", ".env.sample", ".env.template"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_globs_add_canonical_targets_for_symlink_matches() {
        if !ripgrep_available() {
            return;
        }

        let temp_dir = TempDir::new().expect("temp dir");
        let real_root = temp_dir.path().join("real");
        let link_root = temp_dir.path().join("link");
        let real_secret = real_root.join("secret.env");
        std::fs::create_dir_all(&real_root).expect("create real root");
        std::fs::write(&real_secret, "secret").expect("write real secret");
        std::os::unix::fs::symlink(&real_root, &link_root).expect("create symlink");
        let policy =
            default_policy_with_unreadable_glob(format!("{}/**/*.env", link_root.display()));

        let args =
            create_filesystem_args(&policy, temp_dir.path(), Some(2)).expect("filesystem args");

        assert_file_masked(&args.args, &real_secret);
    }

    #[test]
    fn root_prefix_unreadable_globs_are_too_broad_for_linux_expansion() {
        assert_eq!(
            split_pattern_for_ripgrep("/**/*.env", Path::new("/tmp")),
            None
        );
    }

    #[test]
    fn unclosed_character_classes_are_escaped_for_ripgrep() {
        let (search_root, glob) =
            split_pattern_for_ripgrep("/tmp/[*.env", Path::new("/")).expect("split pattern");

        assert_eq!(search_root.as_path(), Path::new("/tmp"));
        assert_eq!(glob, r"\[*.env");
    }

    fn ripgrep_available() -> bool {
        Command::new("rg")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn path_entry(
        path: &Path,
        access: FileSystemAccessMode,
        skip_missing: bool,
    ) -> FileSystemSandboxEntry {
        let path = FileSystemPath::Path {
            path: AbsolutePathBuf::from_absolute_path(path).expect("absolute path"),
        };
        if skip_missing {
            FileSystemSandboxEntry::skip_missing_path(path, access)
        } else {
            FileSystemSandboxEntry::new(path, access)
        }
    }

    /// #587: with denials, each folder between the writable root and a
    /// protected path is bound onto itself before the pass's masks (a mount
    /// point cannot be renamed); the protected folder itself and folders
    /// below it are not. Without denials (Permissive) nothing is pinned.
    #[test]
    fn folders_above_protected_paths_are_pinned_before_their_masks() {
        let temp_dir = TempDir::new().expect("temp dir");
        let base = temp_dir.path().canonicalize().expect("canonical");
        let home = base.join("outer/home");
        std::fs::create_dir_all(home.join("skills/inner")).expect("home");
        std::fs::write(home.join("auth.json"), "x").expect("store");
        let writable = FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        };
        let read_only = path_entry(
            &home.join("skills"),
            FileSystemAccessMode::Read,
            /*skip_missing*/ true,
        );
        let policy = FileSystemSandboxPolicy::restricted(vec![
            writable.clone(),
            path_entry(
                &home.join("auth.json"),
                FileSystemAccessMode::Deny,
                /*skip_missing*/ true,
            ),
            read_only.clone(),
        ]);
        // A folder the user owns but cannot write right now could be made
        // writable first: its entries are pinned too.
        let outer = base.join("outer");
        let args = {
            let _read_only = ModeGuard::set(&outer, /*mode*/ 0o555);
            create_filesystem_args(&policy, &base, NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("bwrap fs args")
                .args
        };
        let binds = pinned_folders(&args);
        let path = |path: &Path| path_to_string(path);
        for folder in [&base, &base.join("outer"), &home] {
            assert!(
                binds.contains(&path(folder).as_str()),
                "{folder:?}: {args:?}"
            );
        }
        // A folder whose parent the user neither owns nor can write (the
        // temporary folder's top-level folder, in `/`) cannot be renamed.
        let top = base
            .ancestors()
            .find(|folder| folder.parent() == Some(Path::new("/")))
            .expect("top-level folder");
        // SAFETY: `geteuid` has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            assert!(!binds.contains(&path(top).as_str()), "{top:?}: {args:?}");
        }
        for folder in [home.join("skills"), home.join("skills/inner")] {
            assert!(
                !binds.contains(&path(&folder).as_str()),
                "{folder:?}: {args:?}"
            );
        }
        let position = |flag: &str, target: &Path| {
            args.windows(2)
                .position(|window| window[0] == flag && window[1] == path(target))
                .expect("mount")
        };
        assert!(position("--bind-try", &home) < position("--ro-bind", &home.join("skills")));

        let workspace = FileSystemSandboxPolicy::restricted(vec![writable, read_only]);
        let args = create_filesystem_args(&workspace, &base, NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
            .expect("bwrap fs args")
            .args;
        assert!(!args.iter().any(|arg| arg == "--bind-try"), "{args:?}");
    }

    /// Restores a folder's mode when dropped, also when the test fails.
    struct ModeGuard<'a>(&'a Path);

    impl<'a> ModeGuard<'a> {
        fn set(path: &'a Path, mode: u32) -> Self {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
            Self(path)
        }
    }

    impl Drop for ModeGuard<'_> {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o755));
        }
    }

    /// The folders a run binds onto themselves (`--bind-try d d`).
    fn pinned_folders(args: &[String]) -> Vec<&str> {
        args.windows(3)
            .filter(|window| window[0] == "--bind-try" && window[1] == window[2])
            .map(|window| window[1].as_str())
            .collect()
    }

    /// #594/#598: a path the policy says may not be moved (the Corbanu home
    /// of a command a person approved: no denials, only its read-only
    /// runtime entries) is pinned at its real location, before the read-only
    /// mounts inside it, and so is every folder above it and above a link
    /// on its way. The link itself cannot be pinned. Without the entry
    /// (Permissive) nothing is pinned.
    #[test]
    fn unmovable_paths_pin_the_real_folder_and_the_folders_above_a_link() {
        let temp_dir = TempDir::new().expect("temp dir");
        let base = temp_dir.path().canonicalize().expect("canonical");
        let home = base.join("dot/home");
        std::fs::create_dir_all(home.join("skills")).expect("home");
        std::fs::create_dir_all(base.join("links")).expect("links");
        let link = base.join("links/home");
        std::os::unix::fs::symlink(&home, &link).expect("link");
        let writable = FileSystemSandboxEntry::new(
            FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            FileSystemAccessMode::Write,
        );
        let read_only = path_entry(
            &home.join("skills"),
            FileSystemAccessMode::Read,
            /*skip_missing*/ true,
        );
        let absolute = |path: &Path| AbsolutePathBuf::from_absolute_path(path).expect("absolute");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            writable.clone(),
            read_only.clone(),
            FileSystemSandboxEntry::deny_moving(&absolute(&link)),
        ]);
        assert_eq!(
            unmovable_pin_targets(&policy),
            vec![
                UnmovableTarget {
                    path: home.clone(),
                    pin_itself: true,
                },
                UnmovableTarget {
                    path: link.clone(),
                    pin_itself: false,
                },
            ]
        );
        let args = create_filesystem_args(&policy, &base, NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
            .expect("bwrap fs args")
            .args;
        let binds = pinned_folders(&args);
        let path = |path: &Path| path_to_string(path);
        for folder in [&base, &base.join("dot"), &home, &base.join("links")] {
            assert!(
                binds.contains(&path(folder).as_str()),
                "{folder:?}: {args:?}"
            );
        }
        for folder in [&link, &home.join("skills")] {
            assert!(
                !binds.contains(&path(folder).as_str()),
                "{folder:?}: {args:?}"
            );
        }
        let position = |flag: &str, target: &Path| {
            args.windows(2)
                .position(|window| window[0] == flag && window[1] == path(target))
                .expect("mount")
        };
        assert!(position("--bind-try", &home) < position("--ro-bind", &home.join("skills")));

        let permissive = FileSystemSandboxPolicy::restricted(vec![writable, read_only]);
        let args = create_filesystem_args(&permissive, &base, NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
            .expect("bwrap fs args")
            .args;
        assert!(!args.iter().any(|arg| arg == "--bind-try"), "{args:?}");
    }

    /// #473: `~/.claude` read-only (it runs hooks) with no
    /// `.credentials.json` yet. A placeholder cannot be made in the
    /// read-only folder, so the folder becomes a snapshot of what it holds.
    #[test]
    fn missing_denied_path_below_read_only_folder_gets_a_snapshot() {
        let temp_dir = TempDir::new().expect("temp dir");
        let home = temp_dir.path().join("home");
        let claude = home.join(".claude");
        std::fs::create_dir_all(claude.join("projects")).expect("create .claude");
        std::fs::write(claude.join("settings.json"), "{}").expect("settings");
        std::os::unix::fs::symlink("settings.json", claude.join("link")).expect("link");
        let credentials = claude.join(".credentials.json");
        let secret = claude.join("secret.json");
        std::fs::write(&secret, "secret").expect("secret");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            path_entry(
                &home,
                FileSystemAccessMode::Write,
                /*skip_missing*/ false,
            ),
            path_entry(
                &claude,
                FileSystemAccessMode::Read,
                /*skip_missing*/ true,
            ),
            path_entry(
                &credentials,
                FileSystemAccessMode::Deny,
                /*skip_missing*/ true,
            ),
            path_entry(
                &secret,
                FileSystemAccessMode::Deny,
                /*skip_missing*/ true,
            ),
        ]);

        let args = create_filesystem_args(&policy, &home, NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
            .expect("filesystem args")
            .args;

        let claude_arg = path_to_string(&claude);
        let position = |window: &[&str]| {
            args.windows(window.len())
                .position(|candidate| candidate == window)
                .unwrap_or_else(|| panic!("expected {window:?} in {args:#?}"))
        };
        let read_only_bind = position(&["--ro-bind", &claude_arg, &claude_arg]);
        let snapshot = position(&["--perms", "555", "--tmpfs", &claude_arg]);
        let remount = position(&["--remount-ro", &claude_arg]);
        assert!(read_only_bind < snapshot && snapshot < remount, "{args:#?}");
        for child in ["projects", "secret.json", "settings.json"] {
            let child = path_to_string(&claude.join(child));
            let bind = position(&["--ro-bind-try", &child, &child]);
            assert!(snapshot < bind && bind < remount, "{args:#?}");
        }
        // An existing denied entry stays masked over its snapshot bind.
        let secret_arg = path_to_string(&secret);
        let secret_mask = args
            .windows(5)
            .position(|window| {
                window[0] == "--perms"
                    && window[1] == "000"
                    && window[2] == "--ro-bind-data"
                    && window[4] == secret_arg
            })
            .unwrap_or_else(|| panic!("expected a mask for {secret_arg}: {args:#?}"));
        assert!(remount < secret_mask, "{args:#?}");
        position(&[
            "--symlink",
            "settings.json",
            &path_to_string(&claude.join("link")),
        ]);
        assert!(
            !args.contains(&path_to_string(&credentials)),
            "nothing is mounted at the missing file: {args:#?}"
        );
        assert!(!credentials.exists(), "no placeholder on the host");
    }

    /// #473: `~/.config/gh` and `~/.config/gcloud` with no `~/.config`
    /// share one placeholder; bubblewrap cannot mount a second over it.
    #[test]
    fn missing_denied_paths_sharing_a_missing_folder_mask_it_once() {
        let temp_dir = TempDir::new().expect("temp dir");
        let home = temp_dir.path().join("home");
        std::fs::create_dir_all(&home).expect("create home");
        let config = home.join(".config");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            path_entry(
                &home,
                FileSystemAccessMode::Write,
                /*skip_missing*/ false,
            ),
            path_entry(
                &config.join("gh"),
                FileSystemAccessMode::Deny,
                /*skip_missing*/ true,
            ),
            path_entry(
                &config.join("gcloud"),
                FileSystemAccessMode::Deny,
                /*skip_missing*/ true,
            ),
        ]);

        let args = create_filesystem_args(&policy, &home, NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
            .expect("filesystem args");

        let config_arg = path_to_string(&config);
        assert_eq!(
            args.args
                .windows(3)
                .filter(|window| window[0] == "--ro-bind-data" && window[2] == config_arg)
                .count(),
            1,
            "{:#?}",
            args.args
        );
        assert_eq!(
            synthetic_mount_target_paths(&args)
                .into_iter()
                .filter(|path| path == &config)
                .count(),
            1
        );
    }

    /// #473 review: a broader writable root's pass leaves an empty
    /// placeholder on the host; the nested root's `--bind` shows that live
    /// file again, so its own pass must mask the path again.
    #[test]
    fn missing_denied_path_is_masked_again_after_a_nested_writable_bind() {
        let temp_dir = TempDir::new().expect("temp dir");
        let home = temp_dir.path().join("home");
        let work = home.join("work");
        std::fs::create_dir_all(&work).expect("create work");
        let secret = work.join("secret");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            path_entry(
                &home,
                FileSystemAccessMode::Write,
                /*skip_missing*/ false,
            ),
            path_entry(
                &work,
                FileSystemAccessMode::Write,
                /*skip_missing*/ false,
            ),
            path_entry(
                &secret,
                FileSystemAccessMode::Deny,
                /*skip_missing*/ true,
            ),
        ]);

        let args = create_filesystem_args(&policy, &home, NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
            .expect("filesystem args")
            .args;

        let work_arg = path_to_string(&work);
        let secret_arg = path_to_string(&secret);
        let inner_bind = args
            .windows(3)
            .position(|window| window == ["--bind", work_arg.as_str(), work_arg.as_str()])
            .unwrap_or_else(|| panic!("expected a bind of {work_arg}: {args:#?}"));
        let masks: Vec<usize> = args
            .windows(3)
            .enumerate()
            .filter(|(_, window)| window[0] == "--ro-bind-data" && window[2] == secret_arg)
            .map(|(index, _)| index)
            .collect();
        assert_eq!(masks.len(), 2, "{args:#?}");
        assert!(masks[0] < inner_bind && inner_bind < masks[1], "{args:#?}");
    }

    /// #473: a missing read-only path below a read-only folder needs no
    /// placeholder; the folder already stops it being created.
    #[test]
    fn missing_read_only_path_below_read_only_folder_is_left_to_the_folder() {
        let temp_dir = TempDir::new().expect("temp dir");
        let home = temp_dir.path().join("home");
        let claude = home.join(".claude");
        std::fs::create_dir_all(&claude).expect("create .claude");
        let hooks = claude.join("missing/hooks");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            path_entry(
                &home,
                FileSystemAccessMode::Write,
                /*skip_missing*/ false,
            ),
            path_entry(
                &claude,
                FileSystemAccessMode::Read,
                /*skip_missing*/ true,
            ),
            path_entry(
                &hooks,
                FileSystemAccessMode::Read,
                /*skip_missing*/ true,
            ),
        ]);

        let args = create_filesystem_args(&policy, &home, NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
            .expect("filesystem args");

        let missing = path_to_string(&claude.join("missing"));
        assert!(
            !args.args.iter().any(|arg| arg.starts_with(&missing)),
            "{:#?}",
            args.args
        );
        assert!(!synthetic_mount_target_paths(&args).contains(&claude.join("missing")));
    }

    /// Assert that `path` is masked due to a bwrap arg sequence like:
    ///
    /// `bwrap ... --perms 000 --ro-bind-data FD PATH`
    fn assert_file_masked(args: &[String], path: &Path) {
        let path = path_to_string(path);
        assert!(
            args.windows(5).any(|window| {
                window[0] == "--perms"
                    && window[1] == "000"
                    && window[2] == "--ro-bind-data"
                    && window[4] == path
            }),
            "expected file mask for {path}: {args:#?}"
        );
    }

    /// Assert that `path` is backed by an fd-supplied empty file without
    /// changing the next mount operation's permissions.
    fn assert_empty_file_bound_without_perms(args: &[String], path: &Path) {
        let path = path_to_string(path);
        assert!(
            args.windows(3)
                .any(|window| { window[0] == "--ro-bind-data" && window[2] == path }),
            "expected empty file bind for {path}: {args:#?}"
        );
        assert!(
            !args.windows(5).any(|window| {
                window[0] == "--perms"
                    && window[1] == "000"
                    && window[2] == "--ro-bind-data"
                    && window[4] == path
            }),
            "missing path bind should not set explicit file perms for {path}: {args:#?}"
        );
    }

    fn assert_empty_directory_mounted_read_only(args: &[String], path: &Path) {
        let path = path_to_string(path);
        assert!(
            args.windows(4)
                .any(|window| window == ["--perms", "555", "--tmpfs", path.as_str()]),
            "expected empty directory mount for {path}: {args:#?}"
        );
        assert!(
            args.windows(2)
                .any(|window| window == ["--remount-ro", path.as_str()]),
            "expected read-only remount for {path}: {args:#?}"
        );
    }

    fn synthetic_mount_target_paths(args: &BwrapArgs) -> Vec<PathBuf> {
        args.synthetic_mount_targets
            .iter()
            .map(|target| target.path().to_path_buf())
            .collect()
    }

    fn protected_create_target_paths(args: &BwrapArgs) -> Vec<PathBuf> {
        args.protected_create_targets
            .iter()
            .map(|target| target.path().to_path_buf())
            .collect()
    }

    /// #506: an unlistable directory becomes an empty mode-0111 folder with
    /// its entries put back, in the pass that exposes it (the read baseline,
    /// or the writable root above it), before that pass's deny masks, and is
    /// made read-only last.
    #[test]
    fn issue_506_unlistable_directory_is_rebuilt_without_listing() {
        let temp_dir = TempDir::new().expect("temp dir");
        let base = temp_dir.path().canonicalize().expect("canonical");
        let home = base.join("home");
        std::fs::create_dir_all(home.join("tmp")).expect("tmp");
        std::fs::write(home.join("secret.txt"), "secret").expect("secret");
        std::os::unix::fs::symlink("tmp", home.join("link")).expect("link");
        let home_path = AbsolutePathBuf::from_absolute_path(&home).expect("absolute");
        let home_str = path_to_string(&home);
        let tmp_str = path_to_string(&home.join("tmp"));
        let secret_str = path_to_string(&home.join("secret.txt"));
        for (root_access, bind) in [
            (FileSystemAccessMode::Read, "--ro-bind-try"),
            (FileSystemAccessMode::Write, "--bind-try"),
        ] {
            let policy = FileSystemSandboxPolicy::restricted(vec![
                FileSystemSandboxEntry::new(
                    FileSystemPath::Special {
                        value: FileSystemSpecialPath::Root,
                    },
                    root_access,
                ),
                FileSystemSandboxEntry::new(
                    FileSystemPath::Path {
                        path: home_path.join("secret.txt"),
                    },
                    FileSystemAccessMode::Deny,
                ),
                FileSystemSandboxEntry::deny_directory_listing(&home_path),
            ]);
            let args = create_filesystem_args(&policy, &base, NO_UNREADABLE_GLOB_SCAN_MAX_DEPTH)
                .expect("filesystem args")
                .args;
            let position = |window: &[&str]| {
                args.windows(window.len())
                    .position(|candidate| candidate.iter().zip(window).all(|(a, b)| a == b))
                    .unwrap_or_else(|| panic!("{root_access:?}: {window:?} missing in {args:?}"))
            };
            let tmpfs = position(&["--perms", "111", "--tmpfs", &home_str]);
            let tmp = position(&[bind, &tmp_str, &tmp_str]);
            let link = position(&["--symlink", "tmp", &path_to_string(&home.join("link"))]);
            let secret = position(&[bind, &secret_str, &secret_str]);
            let secret_mask = position(&["--perms", "000", "--ro-bind-data"]);
            let remount = position(&["--remount-ro", &home_str]);
            assert!(tmpfs < tmp && tmpfs < link && tmpfs < secret, "{args:?}");
            assert!(secret < secret_mask, "{args:?}");
            assert_eq!(remount, args.len() - 2, "{args:?}");
            if root_access == FileSystemAccessMode::Write {
                assert!(position(&["--bind", "/", "/"]) < tmpfs, "{args:?}");
            }
        }
    }

    /// #506: full write with only an unlistable directory still needs bwrap.
    #[test]
    fn issue_506_full_write_with_an_unlistable_directory_is_wrapped() {
        let temp_dir = TempDir::new().expect("temp dir");
        let home = AbsolutePathBuf::from_absolute_path(temp_dir.path()).expect("absolute");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry::new(
                FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                FileSystemAccessMode::Write,
            ),
            FileSystemSandboxEntry::deny_directory_listing(&home),
        ]);
        let command = vec!["/bin/true".to_string()];
        let args = create_bwrap_command_args(
            command.clone(),
            &policy,
            Path::new("/"),
            Path::new("/"),
            BwrapOptions::default(),
        )
        .expect("bwrap args");
        assert_ne!(args.args, command);
        assert!(args.args.iter().any(|arg| arg == "--remount-ro"));
    }

    /// #506 review: a home named `.codex` (or `.agents`) directly under a
    /// writable root is also a read-only subpath of that root. Its read-only
    /// bind must come before the unlistable folder (not over it), the entries
    /// go back read-only, and a missing denied entry's read-only snapshot of
    /// the home is made unlistable itself.
    #[test]
    fn issue_506_unlistable_home_under_a_writable_root_stays_unlistable() {
        let temp_dir = TempDir::new().expect("temp dir");
        let base = temp_dir.path().canonicalize().expect("canonical");
        let workspace = base.join("ws");
        let home = workspace.join(".codex");
        std::fs::create_dir_all(home.join("tmp")).expect("tmp");
        let home_path = AbsolutePathBuf::from_absolute_path(&home).expect("absolute");
        let home_str = path_to_string(&home);
        let tmp_str = path_to_string(&home.join("tmp"));
        let entries = |missing_denied: bool| {
            let mut entries = vec![
                FileSystemSandboxEntry::new(
                    FileSystemPath::Special {
                        value: FileSystemSpecialPath::Root,
                    },
                    FileSystemAccessMode::Read,
                ),
                FileSystemSandboxEntry::new(
                    FileSystemPath::Path {
                        path: AbsolutePathBuf::from_absolute_path(&workspace).expect("ws"),
                    },
                    FileSystemAccessMode::Write,
                ),
                FileSystemSandboxEntry::new(
                    FileSystemPath::Path {
                        path: home_path.clone(),
                    },
                    FileSystemAccessMode::Read,
                ),
                FileSystemSandboxEntry::deny_directory_listing(&home_path),
            ];
            if missing_denied {
                entries.push(FileSystemSandboxEntry::skip_missing_path(
                    FileSystemPath::Path {
                        path: home_path.join("wallet"),
                    },
                    FileSystemAccessMode::Deny,
                ));
            }
            FileSystemSandboxPolicy::restricted(entries)
        };
        let find = |args: &[String], window: &[&str]| {
            args.windows(window.len())
                .position(|candidate| candidate.iter().zip(window).all(|(a, b)| a == b))
        };

        let args = create_filesystem_args(
            &entries(false),
            &workspace,
            /*glob_scan_max_depth*/ None,
        )
        .expect("filesystem args")
        .args;
        let read_only = find(&args, &["--ro-bind", &home_str, &home_str]).expect("ro bind");
        let tmpfs = find(&args, &["--perms", "111", "--tmpfs", &home_str]).expect("tmpfs");
        assert!(read_only < tmpfs, "{args:?}");
        assert!(
            find(&args, &["--ro-bind-try", &tmp_str, &tmp_str]).is_some(),
            "{args:?}"
        );
        assert!(
            find(&args, &["--bind-try", &tmp_str, &tmp_str]).is_none(),
            "{args:?}"
        );
        assert_eq!(
            find(&args, &["--remount-ro", &home_str]),
            Some(args.len() - 2),
            "{args:?}"
        );

        let args = create_filesystem_args(
            &entries(true),
            &workspace,
            /*glob_scan_max_depth*/ None,
        )
        .expect("filesystem args")
        .args;
        assert!(
            find(&args, &["--perms", "555", "--tmpfs", &home_str]).is_none(),
            "{args:?}"
        );
        let snapshot = find(&args, &["--perms", "111", "--tmpfs", &home_str]).expect("snapshot");
        assert!(read_only_after(&args, snapshot, &home_str), "{args:?}");
        assert!(
            !args
                .iter()
                .any(|arg| arg == &path_to_string(&home.join("wallet"))),
            "{args:?}"
        );
    }

    /// Nothing binds the real folder back over `dir` after `from`.
    fn read_only_after(args: &[String], from: usize, dir: &str) -> bool {
        !args[from..]
            .windows(3)
            .any(|window| window[0].contains("bind") && window[2] == dir)
    }

    /// #506 review round 2: in a restricted-read profile whose readable root
    /// is a symlink, the unlistable folder is mounted where the command sees
    /// it (below the root's own spelling), its entries bound from the real
    /// location, and nothing is mounted at the real location.
    #[test]
    fn issue_506_unlistable_home_under_a_symlinked_readable_root() {
        let temp_dir = TempDir::new().expect("temp dir");
        let base = temp_dir.path().canonicalize().expect("canonical");
        let real = base.join("real");
        std::fs::create_dir_all(real.join("home/tmp")).expect("tmp");
        std::fs::write(real.join("home/auth.json"), "secret").expect("auth");
        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).expect("link");
        let policy = FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry::new(
                FileSystemPath::Path {
                    path: AbsolutePathBuf::from_absolute_path(&link).expect("absolute"),
                },
                FileSystemAccessMode::Read,
            ),
            FileSystemSandboxEntry::deny_directory_listing(
                &AbsolutePathBuf::from_absolute_path(link.join("home")).expect("absolute"),
            ),
        ]);
        let args = create_filesystem_args(&policy, &base, /*glob_scan_max_depth*/ None)
            .expect("filesystem args")
            .args;
        let logical = path_to_string(&link.join("home"));
        let real_home = path_to_string(&real.join("home"));
        assert!(
            args.windows(4)
                .any(|window| window == ["--perms", "111", "--tmpfs", logical.as_str()]),
            "{args:?}"
        );
        assert!(
            args.windows(3).any(|window| window
                == [
                    "--ro-bind-try",
                    path_to_string(&real.join("home/auth.json")).as_str(),
                    path_to_string(&link.join("home/auth.json")).as_str(),
                ]),
            "{args:?}"
        );
        // Nothing is mounted at the real spelling, which the profile does not
        // expose: it appears only as a bind source.
        for (index, arg) in args.iter().enumerate() {
            if arg.starts_with(&real_home) {
                assert_eq!(args[index - 1].as_str(), "--ro-bind-try", "{args:?}");
            }
        }
    }
}

#[cfg(test)]
#[path = "database_glob_tests.rs"]
mod database_glob_tests;
