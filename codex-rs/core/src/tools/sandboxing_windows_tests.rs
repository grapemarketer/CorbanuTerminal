//! #300 on a real Windows host: an agent command launched through the tool
//! path (`SandboxAttempt::env_for`) under the unelevated restricted-token
//! sandbox must not read a file its profile denies. That backend cannot block
//! reads, so the launch is refused up front with a message that says how to
//! switch to the elevated sandbox. Run it in a normal (medium-integrity)
//! session too: the unelevated sandbox is what such a session uses by default.

// The probe's output is the evidence of the measured runs (`--nocapture`).
#![allow(clippy::print_stderr)]

use super::SandboxAttempt;
use crate::exec::ExecCapturePolicy;
use crate::exec::ExecExpiration;
use crate::sandboxing::ExecOptions;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::error::CodexErr;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_sandboxing::SandboxCommand;
use codex_sandboxing::SandboxManager;
use codex_sandboxing::SandboxType;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;

const SECRET: &str = "sec-win-300-secret";
const PUBLIC: &str = "sec-win-300-public";

struct EnvGuard {
    key: &'static str,
    original: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &Path) -> Self {
        let original = std::env::var_os(key);
        // SAFETY: CI runs this test in its own process (nextest) or filtered
        // with `--test-threads=1`. Other tests in this crate that set
        // CODEX_HOME are not serialized with it.
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

/// Prepares `cmd /C type secret.env & type public.txt` through the tool path
/// agent commands use, under the unelevated sandbox.
fn unelevated_tool_launch(
    permissions: &PermissionProfile,
    cwd: &AbsolutePathBuf,
) -> Result<crate::sandboxing::ExecRequest, CodexErr> {
    unelevated_tool_launch_script(permissions, cwd, "type secret.env & type public.txt")
}

/// Prepares `cmd /C <script>` through the tool path, under the unelevated
/// sandbox.
fn unelevated_tool_launch_script(
    permissions: &PermissionProfile,
    cwd: &AbsolutePathBuf,
    script: &str,
) -> Result<crate::sandboxing::ExecRequest, CodexErr> {
    let cwd_uri = PathUri::from_abs_path(cwd);
    let manager = SandboxManager::new();
    SandboxAttempt {
        sandbox: SandboxType::WindowsRestrictedToken,
        sandbox_requested: true,
        permissions,
        exec_server_permissions: permissions,
        enforce_managed_network: false,
        manager: &manager,
        sandbox_cwd: &cwd_uri,
        workspace_roots: std::slice::from_ref(&cwd_uri),
        codex_linux_sandbox_exe: None,
        use_legacy_landlock: false,
        windows_sandbox_level: WindowsSandboxLevel::RestrictedToken,
        windows_sandbox_private_desktop: false,
        network_denial_cancellation_token: None,
        network_proxy: None,
    }
    .env_for_with_contract(
        /*contract*/ None,
        SandboxCommand {
            program: "cmd.exe".into(),
            args: vec!["/D".to_string(), "/C".to_string(), script.to_string()],
            cwd: cwd_uri.clone(),
            env: HashMap::new(),
            managed_network: None,
            additional_permissions: None,
        },
        ExecOptions {
            expiration: ExecExpiration::from(60_000),
            capture_policy: ExecCapturePolicy::ShellTool,
        },
        /*network*/ None,
        /*environment_id*/ None,
    )
}

#[tokio::test]
#[serial_test::serial(codex_home)]
async fn sec_win_300_unelevated_tool_launch_refuses_deny_read_profiles() {
    let codex_home_dir = tempfile::tempdir().expect("codex home");
    let workspace_dir = tempfile::tempdir().expect("workspace");
    let _codex_home = EnvGuard::set("CODEX_HOME", codex_home_dir.path());
    let cwd = AbsolutePathBuf::from_absolute_path(
        dunce::canonicalize(workspace_dir.path()).expect("canonical workspace"),
    )
    .expect("absolute workspace");
    std::fs::write(cwd.join("secret.env"), SECRET).expect("secret file");
    std::fs::write(cwd.join("public.txt"), PUBLIC).expect("public file");

    let base = PermissionProfile::workspace_write()
        .materialize_project_roots_with_workspace_roots(std::slice::from_ref(&cwd));
    // Control: the unelevated sandbox runs commands here, and without a deny
    // entry it reads both files.
    let control = crate::sandboxing::execute_env(
        unelevated_tool_launch(&base, &cwd).expect("control launch"),
        /*stdout_stream*/ None,
    )
    .await
    .expect("unelevated sandbox run");
    let control = format!("{}{}", control.stdout.text, control.stderr.text);
    eprintln!("sec-win-300 unelevated, no deny entry: {control}");
    assert!(
        control.contains(SECRET) && control.contains(PUBLIC),
        "{control}"
    );

    let denies = [
        (
            "exact path",
            FileSystemPath::Path {
                path: cwd.join("secret.env"),
            },
        ),
        (
            "glob",
            FileSystemPath::GlobPattern {
                scan: None,
                pattern: "**/*.env".to_string(),
            },
        ),
        // No file matches it yet; the backend cannot enforce it either.
        (
            "glob matching nothing",
            FileSystemPath::GlobPattern {
                scan: None,
                pattern: "**/*.key".to_string(),
            },
        ),
    ];
    let mut outcomes = Vec::new();
    for (label, path) in denies {
        let (mut file_system, _) = base.to_runtime_permissions();
        file_system.entries.push(FileSystemSandboxEntry {
            path,
            access: FileSystemAccessMode::Deny,
            missing_path_behavior: None,
        });
        let denying = PermissionProfile::from_runtime_permissions(
            &file_system,
            NetworkSandboxPolicy::Restricted,
        );
        let outcome = match unelevated_tool_launch(&denying, &cwd) {
            Err(refused) => refused.to_string(),
            Ok(request) => {
                match crate::sandboxing::execute_env(request, /*stdout_stream*/ None).await {
                    Ok(output) => format!("ran: {}{}", output.stdout.text, output.stderr.text),
                    Err(err) => format!("failed at spawn: {err}"),
                }
            }
        };
        eprintln!("sec-win-300 unelevated, {label} deny on secret.env: {outcome}");
        outcomes.push((label, outcome));
    }
    let refusal = format!(
        "unsupported operation: {}",
        codex_sandboxing::UNELEVATED_DENY_READ_REFUSAL
    );
    for (label, outcome) in outcomes {
        assert!(!outcome.contains(SECRET), "{label}: read a denied file");
        assert_eq!(outcome, refusal, "{label}");
    }
}

/// D331: the agent's commands (the tool path) under the unelevated sandbox
/// now refuse a profile with a writable folder inside a read-only one, as
/// `command/exec` does, with a message naming both folders, and nothing runs.
/// (What the backend itself enforces for that shape is
/// `carve_out_link::legacy_writable_folder_inside_read_only_one_with_the_profile_alone`
/// in `codex-windows-sandbox`.)
#[tokio::test]
#[serial_test::serial(codex_home)]
async fn sec_win_d331_unelevated_tool_launch_refuses_writable_inside_read_only() {
    let codex_home_dir = tempfile::tempdir().expect("codex home");
    let workspace_dir = tempfile::tempdir().expect("workspace");
    let _codex_home = EnvGuard::set("CODEX_HOME", codex_home_dir.path());
    let cwd = AbsolutePathBuf::from_absolute_path(
        dunce::canonicalize(workspace_dir.path()).expect("canonical workspace"),
    )
    .expect("absolute workspace");
    let (docs, out) = (cwd.join("docs"), cwd.join("docs").join("out"));
    std::fs::create_dir_all(out.as_path()).expect("create docs\\out");
    let base = PermissionProfile::workspace_write()
        .materialize_project_roots_with_workspace_roots(std::slice::from_ref(&cwd));
    let (mut file_system, _) = base.to_runtime_permissions();
    for (path, access) in [
        (&docs, FileSystemAccessMode::Read),
        (&out, FileSystemAccessMode::Write),
    ] {
        file_system.entries.push(FileSystemSandboxEntry {
            path: FileSystemPath::Path { path: path.clone() },
            access,
            missing_path_behavior: None,
        });
    }
    let nested =
        PermissionProfile::from_runtime_permissions(&file_system, NetworkSandboxPolicy::Restricted);

    let outcome = match unelevated_tool_launch_script(&nested, &cwd, "echo ran> ran.txt") {
        Err(refused) => refused.to_string(),
        Ok(request) => {
            match crate::sandboxing::execute_env(request, /*stdout_stream*/ None).await {
                Ok(output) => format!("ran: {}{}", output.stdout.text, output.stderr.text),
                Err(err) => format!("failed at spawn: {err}"),
            }
        }
    };
    eprintln!("sec-win-d331 unelevated, docs read-only, docs\\out writable: {outcome}");

    assert_eq!(
        (outcome, cwd.join("ran.txt").as_path().exists()),
        (
            format!(
                "unsupported operation: {}",
                codex_sandboxing::writable_inside_read_only_refusal(docs.as_path(), out.as_path())
            ),
            false,
        )
    );
}

/// #1506 item 2 (D262): `.codex` and `.agents` folders an unelevated sandbox
/// command creates in a trusted project (absent when its session started)
/// aren't loaded, config or skills, until the user confirms them.
#[tokio::test]
#[serial_test::serial(codex_home)]
async fn sec_win_1506_planted_project_config_is_not_loaded() {
    use codex_config::ConfigLayerSource;
    use codex_config::ConfigLoadOptions;
    use codex_config::LoaderOverrides;
    use codex_config::NoopThreadConfigLoader;
    use codex_config::loader::load_config_layers_state;

    let temp = tempfile::tempdir().expect("fixture directory");
    let root = dunce::canonicalize(temp.path()).expect("canonical fixture root");
    let home = root.join("home");
    let project = root.join("project");
    for dir in [
        &home,
        &project,
        &root.join("account"),
        &root.join("managed"),
    ] {
        std::fs::create_dir_all(dir).expect("fixture directory");
    }
    std::fs::write(project.join(".fixture-root"), "").expect("project root marker");
    let project_key = toml::Value::String(project.display().to_string());
    std::fs::write(
        home.join("config.toml"),
        format!(
            "model = \"fixture-user\"\nproject_root_markers = [\".fixture-root\"]\n\
             [projects.{project_key}]\ntrust_level = \"trusted\"\n"
        ),
    )
    .expect("user config");
    let _codex_home = EnvGuard::set("CODEX_HOME", &home);
    let cwd = AbsolutePathBuf::from_absolute_path(&project).expect("absolute project");
    let base = PermissionProfile::workspace_write()
        .materialize_project_roots_with_workspace_roots(std::slice::from_ref(&cwd));

    let planted = crate::sandboxing::execute_env(
        unelevated_tool_launch_script(
            &base,
            &cwd,
            // A TOML literal string: cmd doesn't understand the `\"` the
            // argument quoting would give a double quote.
            "mkdir .codex && (echo model = 'planted')> .codex\\config.toml && mkdir .agents\\skills",
        )
        .expect("planting launch"),
        /*stdout_stream*/ None,
    )
    .await
    .expect("unelevated sandbox run");
    eprintln!(
        "sec-win-1506 planting command: exit {} {}{}",
        planted.exit_code, planted.stdout.text, planted.stderr.text
    );

    let load = || async {
        let stack = load_config_layers_state(
            codex_exec_server::LOCAL_FS.as_ref(),
            &home,
            Some(cwd.clone()),
            &[],
            ConfigLoadOptions {
                loader_overrides: LoaderOverrides {
                    managed_config_path: Some(root.join("managed/legacy.toml")),
                    system_config_path: Some(root.join("managed/config.toml")),
                    system_requirements_path: Some(root.join("managed/requirements.toml")),
                    user_home_dir: Some(root.join("account")),
                    macos_managed_config_requirements_base64: Some(String::new()),
                    ..Default::default()
                },
                ..Default::default()
            },
            &NoopThreadConfigLoader,
        )
        .await
        .expect("load config");
        let model = stack
            .effective_config()
            .get("model")
            .and_then(toml::Value::as_str)
            .map(str::to_string);
        let project_layer_disabled = stack
            .get_layers(
                codex_config::ConfigLayerStackOrdering::LowestPrecedenceFirst,
                /*include_disabled*/ true,
            )
            .into_iter()
            .find(|layer| matches!(layer.name, ConfigLayerSource::Project { .. }))
            .map(|layer| layer.disabled_reason.is_some());
        (model, project_layer_disabled)
    };
    let before_confirming = (
        planted.exit_code,
        project.join(".codex").join("config.toml").is_file(),
        load().await,
    );

    let agents_planted =
        || codex_config::planted_project_metadata().contains(&project.join(".agents"));
    let agents_before = agents_planted();
    codex_windows_sandbox::confirm_planted_project_metadata(&project.join(".codex"))
        .expect("confirm .codex");
    let after_confirming = load().await;
    // Confirming one folder leaves the other planted.
    let agents_after_codex = agents_planted();
    codex_windows_sandbox::confirm_planted_project_metadata(&project.join(".agents"))
        .expect("confirm .agents");
    let agents_after = agents_planted();

    assert_eq!(
        (
            before_confirming,
            after_confirming,
            (agents_before, agents_after_codex, agents_after),
        ),
        (
            (0, true, (Some("fixture-user".to_string()), Some(true))),
            (Some("planted".to_string()), Some(false)),
            (true, true, false),
        )
    );
}

/// How a test folder is reached through a link (#1801).
#[derive(Clone, Copy, Debug)]
enum LinkKind {
    Junction,
    Symlink,
    Subst,
}

/// A link to `target`: a junction or folder symbolic link at `link`, or a
/// `subst` drive (its root returned). `None` if this account may not make a
/// symbolic link (a normal session without developer mode) or a `subst`
/// drive (a normal session on the CI runner gets "Access denied"), unless
/// `CORBANU_TEST_REQUIRE_SYMLINKS` or `CORBANU_TEST_REQUIRE_SUBST` is set
/// (the elevated CI run).
#[allow(clippy::expect_used)] // test fixture setup
fn make_link(kind: LinkKind, link: &Path, target: &Path) -> Option<AbsolutePathBuf> {
    let absolute = |path: &Path| AbsolutePathBuf::from_absolute_path(path).expect("absolute");
    match kind {
        LinkKind::Junction => {
            let status = std::process::Command::new("cmd.exe")
                .args(["/d", "/c", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .expect("run mklink");
            assert!(status.status.success(), "mklink /J: {status:?}");
            Some(absolute(link))
        }
        LinkKind::Symlink => match std::os::windows::fs::symlink_dir(target, link) {
            Ok(()) => Some(absolute(link)),
            Err(err) => {
                assert!(
                    std::env::var_os("CORBANU_TEST_REQUIRE_SYMLINKS").is_none(),
                    "symlink_dir: {err}"
                );
                None
            }
        },
        LinkKind::Subst => {
            let drive = ('M'..='Z')
                .rev()
                .map(|letter| format!("{letter}:"))
                .find(|drive| !Path::new(&format!("{drive}\\")).exists())
                .expect("a free drive letter for subst");
            let made = std::process::Command::new("subst")
                .arg(&drive)
                .arg(target)
                .status()
                .is_ok_and(|status| status.success());
            assert!(
                made || std::env::var_os("CORBANU_TEST_REQUIRE_SUBST").is_none(),
                "subst {drive} failed"
            );
            made.then(|| absolute(Path::new(&format!("{drive}\\"))))
        }
    }
}

/// Removes a `subst` drive when dropped.
struct SubstGuard(Option<AbsolutePathBuf>);

impl Drop for SubstGuard {
    fn drop(&mut self) {
        if let Some(root) = &self.0 {
            let drive = root.as_path().to_string_lossy();
            let _ = std::process::Command::new("subst")
                .args([drive.trim_end_matches('\\'), "/d"])
                .status();
        }
    }
}

/// #1801: D331's refusal of a writable folder inside a read-only one holds
/// when the two (or the working folder) are spelled apart by a link, as the
/// agent's commands launch under the unelevated sandbox: the working folder
/// given as a junction, and a writable folder reached through a junction, a
/// folder symbolic link or a `subst` drive into the read-only folder. Nothing
/// runs and nothing is written in the read-only folder. Not refused: a
/// writable folder through a link elsewhere, and the read-only folder itself
/// named writable through a link (read-only wins there, #1780).
#[tokio::test]
#[serial_test::serial(codex_home)]
async fn sec_win_d331_1801_writable_inside_read_only_through_a_link() {
    let codex_home_dir = tempfile::tempdir().expect("codex home");
    let _codex_home = EnvGuard::set("CODEX_HOME", codex_home_dir.path());

    let refusal_start = codex_sandboxing::writable_inside_read_only_refusal(
        Path::new("<read-only>"),
        Path::new("<writable>"),
    );
    let refusal_start = refusal_start
        .split("<writable>")
        .next()
        .expect("message names the writable folder")
        .to_string();
    let mut outcomes = Vec::new();
    // (label, cwd through a junction, writable through a link of this kind
    // (the writable folder is `out` otherwise), link target)
    let cases: [(&str, bool, Option<LinkKind>, &str); 7] = [
        ("cwd through a junction", true, None, ""),
        (
            "out through a junction",
            false,
            Some(LinkKind::Junction),
            "docs",
        ),
        (
            "out through a symbolic link",
            false,
            Some(LinkKind::Symlink),
            "docs",
        ),
        ("out through subst", false, Some(LinkKind::Subst), "docs"),
        (
            "cwd and out through junctions",
            true,
            Some(LinkKind::Junction),
            "docs",
        ),
        (
            "control: a link elsewhere",
            false,
            Some(LinkKind::Junction),
            "other",
        ),
        (
            "control: docs itself",
            false,
            Some(LinkKind::Junction),
            "docs\\",
        ),
    ];
    for (label, cwd_via_junction, link, target) in cases {
        let root = tempfile::tempdir().expect("fixture root");
        let real_root = dunce::canonicalize(root.path()).expect("canonical root");
        let workspace = real_root.join("workspace");
        let docs = workspace.join("docs");
        std::fs::create_dir_all(docs.join("out")).expect("create docs\\out");
        std::fs::create_dir_all(workspace.join("other")).expect("create other");
        let real_docs = AbsolutePathBuf::from_absolute_path(&docs).expect("absolute docs");
        let cwd = if cwd_via_junction {
            make_link(LinkKind::Junction, &real_root.join("cwd-link"), &workspace)
                .expect("junction")
        } else {
            AbsolutePathBuf::from_absolute_path(&workspace).expect("absolute workspace")
        };
        // The writable folder, as the profile spells it, and where a write
        // through it lands.
        let mut subst = SubstGuard(None);
        let (writable, lands_in) = match link {
            None => (real_docs.join("out"), docs.join("out")),
            Some(kind) => {
                let real_target = workspace.join(target.trim_end_matches('\\'));
                let Some(link_root) = make_link(kind, &real_root.join("link"), &real_target) else {
                    eprintln!("sec-win-1801 {label}: skipped: may not make this link here");
                    continue;
                };
                if matches!(kind, LinkKind::Subst) {
                    subst.0 = Some(link_root.clone());
                }
                if target == "docs" {
                    (link_root.join("out"), docs.join("out"))
                } else {
                    (link_root, real_target)
                }
            }
        };
        let base = PermissionProfile::workspace_write()
            .materialize_project_roots_with_workspace_roots(std::slice::from_ref(&cwd));
        let (mut file_system, _) = base.to_runtime_permissions();
        for (path, access) in [
            (&real_docs, FileSystemAccessMode::Read),
            (&writable, FileSystemAccessMode::Write),
        ] {
            file_system.entries.push(FileSystemSandboxEntry {
                path: FileSystemPath::Path { path: path.clone() },
                access,
                missing_path_behavior: None,
            });
        }
        let profile = PermissionProfile::from_runtime_permissions(
            &file_system,
            NetworkSandboxPolicy::Restricted,
        );

        // Unquoted: `cmd /C` gets the script as one escaped argument, and the
        // fixture paths have no spaces.
        let script = format!("echo y> {}", writable.join("y.txt").as_path().display());
        let outcome = match unelevated_tool_launch_script(&profile, &cwd, &script) {
            Err(refused) => refused.to_string(),
            Ok(request) => {
                match crate::sandboxing::execute_env(request, /*stdout_stream*/ None).await {
                    Ok(output) => format!("ran: {}{}", output.stdout.text, output.stderr.text),
                    Err(err) => format!("failed at spawn: {err}"),
                }
            }
        };
        let written = lands_in.join("y.txt").exists();
        eprintln!("sec-win-1801 {label}: written={written} {outcome}");
        // The folders it names are spelled as the resolver has them (a
        // `subst` drive is already resolved there).
        let refused = outcome.starts_with(&format!("unsupported operation: {refusal_start}"));
        outcomes.push((label, refused, written));
        drop(subst);
    }

    let expected = [
        ("cwd through a junction", true, false),
        ("out through a junction", true, false),
        ("out through a symbolic link", true, false),
        ("out through subst", true, false),
        ("cwd and out through junctions", true, false),
        ("control: a link elsewhere", false, true),
        ("control: docs itself", false, false),
    ];
    let expected: Vec<_> = expected
        .into_iter()
        .filter(|(label, ..)| outcomes.iter().any(|(ran, ..)| ran == label))
        .collect();
    assert_eq!(outcomes, expected);
}
