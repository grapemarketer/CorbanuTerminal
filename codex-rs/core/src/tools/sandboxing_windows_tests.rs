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
