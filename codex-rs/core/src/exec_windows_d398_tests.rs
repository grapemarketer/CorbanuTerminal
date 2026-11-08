//! D398 (#1766) on a real Windows host: `command/exec` (`build_exec_request`)
//! under the non-admin sandbox, with the working folder spelled through a
//! `subst` drive, a mapped drive, an 8.3 short name or in lower case. Before
//! D398 each of those was refused ("cannot enforce split writable root sets
//! directly"). Now each runs as from the real path: the working folder is
//! writable, and the read-only `docs` and the protected `.git` stay
//! read-only. Junctions and folder symbolic links ran before and still do.
//!
//! It must not let anything else through: a working folder that is a
//! junction, folder symbolic link or `subst` drive leading out of the
//! workspace is still refused and nothing is written there.
//!
//! The table it prints is the evidence (`--nocapture`); run it in an elevated
//! and a normal (medium-integrity) session.

#![allow(clippy::print_stderr, clippy::expect_used)]

use super::ExecCapturePolicy;
use super::ExecExpiration;
use super::ExecParams;
use super::build_exec_request;
use crate::sandboxing::SandboxPermissions;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

const SPLIT_ROOTS_REFUSAL: &str = "cannot enforce split writable root sets directly";

struct EnvGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &Path) -> Self {
        let original = std::env::var_os(key);
        // SAFETY: serialized with the other CODEX_HOME tests; CI runs it
        // filtered with `--test-threads=1`.
        unsafe { std::env::set_var(key, value) };
        Self { key, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: as in `set`.
        unsafe {
            match &self.original {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// A drive letter made for the test (`subst` or `net use`), removed on drop.
struct DriveGuard {
    drive: String,
    mapped: bool,
}

impl Drop for DriveGuard {
    fn drop(&mut self) {
        let _ = if self.mapped {
            Command::new("net")
                .args(["use", &self.drive, "/delete", "/y"])
                .output()
        } else {
            Command::new("subst").args([&self.drive, "/d"]).output()
        };
    }
}

fn required(var: &str) -> bool {
    std::env::var_os(var).is_some()
}

fn free_drive() -> String {
    ('M'..='Z')
        .rev()
        .map(|letter| format!("{letter}:"))
        .find(|drive| !Path::new(&format!("{drive}\\")).exists())
        .expect("a free drive letter")
}

fn junction(link: &Path, target: &Path) -> PathBuf {
    let output = Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .expect("run mklink");
    assert!(output.status.success(), "mklink /J: {output:?}");
    link.to_path_buf()
}

fn symlink(link: &Path, target: &Path) -> Result<PathBuf, String> {
    match std::os::windows::fs::symlink_dir(target, link) {
        Ok(()) => Ok(link.to_path_buf()),
        Err(err) => {
            assert!(
                !required("CORBANU_TEST_REQUIRE_SYMLINKS"),
                "symlink_dir: {err}"
            );
            Err(format!("symlink_dir: {err}"))
        }
    }
}

/// `subst X: target`: the drive's root, or why it couldn't be made (a normal
/// session on the CI runner gets "Access denied").
fn subst(target: &Path) -> Result<(PathBuf, DriveGuard), String> {
    let drive = free_drive();
    let output = Command::new("subst")
        .arg(&drive)
        .arg(target)
        .output()
        .expect("run subst");
    let made = output.status.success() && Path::new(&format!("{drive}\\")).is_dir();
    assert!(
        made || !required("CORBANU_TEST_REQUIRE_SUBST"),
        "subst {drive}: {output:?}"
    );
    if !made {
        return Err(format!(
            "subst: {}",
            String::from_utf8_lossy(&output.stdout)
        ));
    }
    Ok((
        PathBuf::from(format!("{drive}\\")),
        DriveGuard {
            drive,
            mapped: false,
        },
    ))
}

/// `net use Y: \\localhost\<drive>$\<folder>` (the administrative share), or
/// `CODEX_TEST_D398_SHARE` (`\\host\share`) over `CODEX_TEST_D398_SHARE_DIR`
/// when the workflow made one: the mapped drive's root.
fn mapped_drive(target: &Path) -> Result<(PathBuf, DriveGuard), String> {
    let target_text = target.display().to_string();
    let share_path = match (
        std::env::var("CODEX_TEST_D398_SHARE"),
        std::env::var("CODEX_TEST_D398_SHARE_DIR"),
    ) {
        (Ok(share), Ok(dir)) => {
            let real_dir = dunce::canonicalize(&dir).map_err(|err| format!("{dir}: {err}"))?;
            let rest = target
                .strip_prefix(&real_dir)
                .map_err(|_| format!("{target_text} is not under {dir}"))?;
            format!("{share}\\{}", rest.display())
        }
        _ => format!(
            "\\\\localhost\\{}$\\{}",
            &target_text[..1],
            &target_text[3..]
        ),
    };
    let drive = free_drive();
    let output = Command::new("net")
        .args(["use", &drive, &share_path])
        .output()
        .expect("run net use");
    let made = output.status.success() && Path::new(&format!("{drive}\\")).is_dir();
    assert!(
        made || !required("CORBANU_TEST_REQUIRE_MAPPED_DRIVE"),
        "net use {drive} {share_path}: {output:?}"
    );
    if !made {
        return Err(format!(
            "net use {share_path}: {}{}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok((
        PathBuf::from(format!("{drive}\\")),
        DriveGuard {
            drive,
            mapped: true,
        },
    ))
}

/// The 8.3 short spelling of `path`, if its volume makes them.
fn short_name(path: &Path) -> Result<PathBuf, String> {
    let output = Command::new("cmd.exe")
        .args(["/d", "/c"])
        .arg(format!("for %I in (\"{}\") do @echo %~sI", path.display()))
        .output()
        .expect("run cmd for %~sI");
    let short = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let made = output.status.success() && short != path && short.is_dir();
    assert!(
        made || !required("CORBANU_TEST_REQUIRE_8DOT3"),
        "no 8.3 name for {}: {output:?}",
        path.display()
    );
    if made {
        Ok(short)
    } else {
        Err(format!("no 8.3 name for {}", path.display()))
    }
}

/// A workspace with `docs` and `.git`, and an `outside` folder beside it.
fn make_workspace(root: &Path) -> (PathBuf, PathBuf) {
    let workspace = root.join("Workspace Folder");
    let outside = root.join("Outside Folder");
    for dir in [
        workspace.join("docs"),
        workspace.join(".git"),
        outside.clone(),
    ] {
        std::fs::create_dir_all(&dir).expect("create fixture folders");
    }
    (workspace, outside)
}

/// The three profile shapes #1766 measured that need direct enforcement,
/// with `:project_roots` the `workspace` folder as spelled.
/// Each with whether `docs` is read-only.
fn profiles(workspace: &Path, real_docs: &Path) -> Vec<(&'static str, PermissionProfile, bool)> {
    let special =
        |value, access| FileSystemSandboxEntry::new(FileSystemPath::Special { value }, access);
    let root_read = special(FileSystemSpecialPath::Root, FileSystemAccessMode::Read);
    let project_write = special(
        FileSystemSpecialPath::project_roots(/*subpath*/ None),
        FileSystemAccessMode::Write,
    );
    let workspace = AbsolutePathBuf::from_absolute_path(workspace).expect("absolute workspace");
    let profile = |entries: Vec<FileSystemSandboxEntry>| {
        PermissionProfile::from_runtime_permissions(
            &FileSystemSandboxPolicy::restricted(entries),
            NetworkSandboxPolicy::Restricted,
        )
        .materialize_project_roots_with_workspace_roots(std::slice::from_ref(&workspace))
    };
    vec![
        (
            "`:project_roots` write",
            profile(vec![root_read.clone(), project_write.clone()]),
            false,
        ),
        (
            "+ read-only `:project_roots/docs`",
            profile(vec![
                root_read.clone(),
                project_write.clone(),
                special(
                    FileSystemSpecialPath::project_roots(Some("docs".to_string())),
                    FileSystemAccessMode::Read,
                ),
            ]),
            true,
        ),
        (
            "+ read-only docs by real path",
            profile(vec![
                root_read,
                project_write,
                FileSystemSandboxEntry::new(
                    FileSystemPath::Path {
                        path: AbsolutePathBuf::from_absolute_path(real_docs).expect("absolute"),
                    },
                    FileSystemAccessMode::Read,
                ),
            ]),
            true,
        ),
    ]
}

/// What happened when `command/exec` ran a command writing `ok.txt` in the
/// working folder, `docs\x.txt` and `.git\g.txt`, checked in `real` (the
/// folder the working folder leads to).
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// Refused with #1766's message; nothing written.
    SplitRootsRefused {
        written: bool,
    },
    Ran {
        ok: bool,
        docs: bool,
        git: bool,
    },
    Other(String),
}

async fn run_command_exec(
    profile: &PermissionProfile,
    cwd: &Path,
    real: &Path,
    has_docs: bool,
) -> Outcome {
    let cwd = AbsolutePathBuf::from_absolute_path(cwd).expect("absolute cwd");
    let written = |name: &str| real.join(name).exists();
    for name in ["ok.txt", "docs\\x.txt", ".git\\g.txt"] {
        let _ = std::fs::remove_file(real.join(name));
    }
    let request = build_exec_request(
        ExecParams {
            command: vec![
                "cmd.exe".to_string(),
                "/D".to_string(),
                "/C".to_string(),
                "echo ok> ok.txt & echo x> docs\\x.txt & echo g> .git\\g.txt".to_string(),
            ],
            cwd: cwd.clone(),
            expiration: ExecExpiration::from(60_000),
            capture_policy: ExecCapturePolicy::ShellTool,
            env: HashMap::new(),
            network: None,
            network_environment_id: None,
            sandbox_permissions: SandboxPermissions::UseDefault,
            windows_sandbox_level: WindowsSandboxLevel::RestrictedToken,
            windows_sandbox_private_desktop: false,
            justification: None,
            arg0: None,
        },
        profile,
        &cwd,
        std::slice::from_ref(&cwd),
        &None,
        /*use_legacy_landlock*/ false,
    );
    match request {
        Err(err) if err.to_string().contains(SPLIT_ROOTS_REFUSAL) => Outcome::SplitRootsRefused {
            written: written("ok.txt"),
        },
        Err(err) => Outcome::Other(format!("refused: {err}")),
        Ok(request) => {
            match crate::sandboxing::execute_env(request, /*stdout_stream*/ None).await {
                Ok(_) => Outcome::Ran {
                    ok: written("ok.txt"),
                    docs: has_docs && written("docs\\x.txt"),
                    git: has_docs && written(".git\\g.txt"),
                },
                Err(err) => Outcome::Other(format!("failed: {err}")),
            }
        }
    }
}

/// D398: each spelling of the working folder runs as the real path does, and
/// `docs` (read-only) and `.git` stay unwritten; a working folder leading out
/// of the workspace is still refused.
#[tokio::test]
#[serial_test::serial(codex_home)]
async fn sec_win_d398_command_exec_non_canonical_cwd() {
    let codex_home_dir = tempfile::tempdir().expect("codex home");
    let _codex_home = EnvGuard::set("CODEX_HOME", codex_home_dir.path());
    let temp = tempfile::tempdir().expect("fixture root");
    let base = dunce::canonicalize(temp.path()).expect("canonical fixture root");
    let parent = base.join("Long Parent Folder");
    let (workspace, outside) = make_workspace(&parent);

    // 8.3 names: CI's temp drive has them off, so that form gets its own
    // workspace on the system drive (the workflow turns them on there).
    let system_root = PathBuf::from(format!(
        "{}\\d398-{}",
        std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string()),
        std::process::id()
    ));
    let system_parent = system_root.join("Long Parent Folder");
    let (system_workspace, _) = make_workspace(&system_parent);
    let system_workspace = dunce::canonicalize(&system_workspace).expect("canonical");

    let mut guards = Vec::new();
    // (form, cwd as spelled, the real folder it leads to)
    let mut forms: Vec<(&str, Result<PathBuf, String>, PathBuf)> = vec![(
        "canonical (control)",
        Ok(workspace.clone()),
        workspace.clone(),
    )];
    let subst_form = subst(&parent).map(|(root, guard)| {
        guards.push(guard);
        root.join("Workspace Folder")
    });
    forms.push(("subst drive", subst_form, workspace.clone()));
    let mapped_form = mapped_drive(&parent).map(|(root, guard)| {
        guards.push(guard);
        root.join("Workspace Folder")
    });
    forms.push(("mapped drive", mapped_form, workspace.clone()));
    forms.push((
        "8.3 short name",
        short_name(&system_workspace),
        system_workspace.clone(),
    ));
    forms.push((
        "whole path in lower case",
        Ok(PathBuf::from(
            workspace.display().to_string().to_lowercase(),
        )),
        workspace.clone(),
    ));
    forms.push((
        "junction (control)",
        Ok(junction(&base.join("cwd-junction"), &workspace)),
        workspace.clone(),
    ));
    forms.push((
        "folder symlink (control)",
        symlink(&base.join("cwd-symlink"), &workspace),
        workspace.clone(),
    ));

    eprintln!("\n| cwd form | profile | command/exec (non-admin) |");
    eprintln!("|---|---|---|");
    let mut results = Vec::new();
    for (form, cwd, real) in &forms {
        let cwd = match cwd {
            Ok(cwd) => cwd,
            Err(why) => {
                eprintln!("| {form} | | not made here: {why} |");
                continue;
            }
        };
        for (profile_name, profile, docs_read_only) in profiles(cwd, &real.join("docs")) {
            let outcome = run_command_exec(&profile, cwd, real, /*has_docs*/ true).await;
            eprintln!(
                "| {form} `{}` | {profile_name} | {outcome:?} |",
                cwd.display()
            );
            let expected = Outcome::Ran {
                ok: true,
                docs: !docs_read_only,
                git: false,
            };
            results.push((format!("{form} / {profile_name}"), outcome, expected));
        }
    }

    // Negative: the working folder leads out of the workspace (a junction, a
    // folder symbolic link, a `subst` drive at another folder); the profile's
    // workspace is the real one.
    let mut leading_out: Vec<(&str, Result<PathBuf, String>)> = vec![
        (
            "junction to a folder outside the workspace",
            Ok(junction(&base.join("out-junction"), &outside)),
        ),
        (
            "folder symlink to a folder outside the workspace",
            symlink(&base.join("out-symlink"), &outside),
        ),
    ];
    let other_subst = subst(&outside).map(|(root, guard)| {
        guards.push(guard);
        root
    });
    leading_out.push(("subst drive at a different folder", other_subst));
    let mut negatives = Vec::new();
    for (form, cwd) in &leading_out {
        let cwd = match cwd {
            Ok(cwd) => cwd,
            Err(why) => {
                eprintln!("| {form} | | not made here: {why} |");
                continue;
            }
        };
        for (profile_name, profile, _) in profiles(&workspace, &workspace.join("docs")) {
            let outcome = run_command_exec(&profile, cwd, &outside, /*has_docs*/ false).await;
            eprintln!(
                "| {form} `{}` | {profile_name} | {outcome:?} |",
                cwd.display()
            );
            negatives.push((format!("{form} / {profile_name}"), outcome));
        }
    }
    drop(guards);
    let _ = std::fs::remove_dir_all(&system_root);

    let (outcomes, expected): (Vec<_>, Vec<_>) = results
        .into_iter()
        .map(|(label, outcome, expected)| ((label.clone(), outcome), (label, expected)))
        .unzip();
    assert_eq!(
        outcomes, expected,
        "every spelling of the workspace runs as the real path; read-only docs and .git stay unwritten"
    );
    assert_eq!(
        negatives,
        negatives
            .iter()
            .map(|(label, _)| (label.clone(), Outcome::SplitRootsRefused { written: false }))
            .collect::<Vec<_>>(),
        "a working folder leading out of the workspace is still refused"
    );
}
