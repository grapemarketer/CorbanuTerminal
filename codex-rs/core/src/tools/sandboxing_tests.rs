use super::*;
use crate::sandboxing::SandboxPermissions;
use crate::tools::hook_names::HookToolName;
use codex_network_proxy::ManagedNetworkSandboxContext;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::protocol::GranularApprovalConfig;
use codex_sandboxing::SandboxCommand;
use codex_sandboxing::SandboxManager;
use codex_sandboxing::SandboxType;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::HashMap;

#[test]
fn bash_permission_request_payload_omits_missing_description() {
    assert_eq!(
        PermissionRequestPayload::bash("echo hi".to_string(), /*description*/ None),
        PermissionRequestPayload {
            tool_name: HookToolName::bash(),
            tool_input: json!({ "command": "echo hi" }),
        }
    );
}

#[test]
fn bash_permission_request_payload_includes_description_when_present() {
    assert_eq!(
        PermissionRequestPayload::bash(
            "echo hi".to_string(),
            Some("network-access example.com".to_string()),
        ),
        PermissionRequestPayload {
            tool_name: HookToolName::bash(),
            tool_input: json!({
                "command": "echo hi",
                "description": "network-access example.com",
            }),
        }
    );
}

#[test]
fn external_sandbox_skips_exec_approval_on_request() {
    assert_eq!(
        default_exec_approval_requirement(
            AskForApproval::OnRequest,
            &FileSystemSandboxPolicy::external_sandbox(),
        ),
        ExecApprovalRequirement::Skip {
            bypass_sandbox: false,
            proposed_execpolicy_amendment: None,
        }
    );
}

#[test]
fn restricted_sandbox_requires_exec_approval_on_request() {
    assert_eq!(
        default_exec_approval_requirement(
            AskForApproval::OnRequest,
            &FileSystemSandboxPolicy::default()
        ),
        ExecApprovalRequirement::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        }
    );
}

#[test]
fn default_exec_approval_requirement_rejects_sandbox_prompt_when_granular_disables_it() {
    let policy = AskForApproval::Granular(GranularApprovalConfig {
        sandbox_approval: false,
        rules: true,
        skill_approval: true,
        request_permissions: true,
        mcp_elicitations: true,
    });

    let requirement =
        default_exec_approval_requirement(policy, &FileSystemSandboxPolicy::default());

    assert_eq!(
        requirement,
        ExecApprovalRequirement::Forbidden {
            reason: "approval policy disallowed sandbox approval prompt".to_string(),
        }
    );
}

#[test]
fn default_exec_approval_requirement_keeps_prompt_when_granular_allows_sandbox_approval() {
    let policy = AskForApproval::Granular(GranularApprovalConfig {
        sandbox_approval: true,
        rules: false,
        skill_approval: true,
        request_permissions: true,
        mcp_elicitations: false,
    });

    let requirement =
        default_exec_approval_requirement(policy, &FileSystemSandboxPolicy::default());

    assert_eq!(
        requirement,
        ExecApprovalRequirement::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        }
    );
}

#[test]
fn additional_permissions_allow_bypass_sandbox_first_attempt_when_execpolicy_skips() {
    assert_eq!(
        sandbox_override_for_first_attempt(
            SandboxPermissions::WithAdditionalPermissions,
            &ExecApprovalRequirement::Skip {
                bypass_sandbox: true,
                proposed_execpolicy_amendment: None,
            },
            &FileSystemSandboxPolicy::default(),
        ),
        SandboxOverride::BypassSandboxFirstAttempt
    );
}

#[test]
fn guardian_bypasses_sandbox_for_explicit_escalation_on_first_attempt() {
    assert_eq!(
        sandbox_override_for_first_attempt(
            SandboxPermissions::RequireEscalated,
            &ExecApprovalRequirement::Skip {
                bypass_sandbox: false,
                proposed_execpolicy_amendment: None,
            },
            &FileSystemSandboxPolicy::default(),
        ),
        SandboxOverride::BypassSandboxFirstAttempt
    );
}

/// #807: on macOS the keychain-services rule of Aggressive (#758) keeps an
/// escalated or allow-listed run in the sandbox by itself, although it
/// denies no file read; elsewhere no sandbox takes it.
#[test]
fn issue_807_keychain_rule_alone_keeps_the_run_sandboxed_on_macos() {
    let full_write = FileSystemSandboxEntry::new(
        FileSystemPath::Special {
            value: codex_protocol::permissions::FileSystemSpecialPath::Root,
        },
        FileSystemAccessMode::Write,
    );
    let without = FileSystemSandboxPolicy::restricted(vec![full_write.clone()]);
    let with = FileSystemSandboxPolicy::restricted(vec![
        full_write,
        FileSystemSandboxEntry::deny_keychain_services(),
    ]);
    assert!(!with.has_denied_read_restrictions());
    assert!(unsandboxed_execution_allowed(&without));
    let macos = cfg!(target_os = "macos");
    assert_eq!(unsandboxed_execution_allowed(&with), !macos);
    let skip = ExecApprovalRequirement::Skip {
        bypass_sandbox: true,
        proposed_execpolicy_amendment: None,
    };
    assert_eq!(
        (
            sandbox_override_for_first_attempt(SandboxPermissions::RequireEscalated, &skip, &with),
            sandbox_permissions_preserving_denied_reads(
                SandboxPermissions::RequireEscalated,
                &with
            ),
        ),
        if macos {
            (SandboxOverride::NoOverride, SandboxPermissions::UseDefault)
        } else {
            (
                SandboxOverride::BypassSandboxFirstAttempt,
                SandboxPermissions::RequireEscalated,
            )
        }
    );
}

#[test]
fn deny_read_blocks_explicit_escalation_and_policy_bypass() {
    let file_system_policy = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry {
        path: FileSystemPath::GlobPattern {
            scan: None,
            pattern: "**/*.env".to_string(),
        },
        access: FileSystemAccessMode::Deny,
        missing_path_behavior: None,
    }]);

    assert_eq!(
        sandbox_override_for_first_attempt(
            SandboxPermissions::RequireEscalated,
            &ExecApprovalRequirement::Skip {
                bypass_sandbox: false,
                proposed_execpolicy_amendment: None,
            },
            &file_system_policy,
        ),
        SandboxOverride::NoOverride,
        "explicit escalation would drop deny-read filesystem policy, so keep the first attempt sandboxed",
    );
    assert!(!unsandboxed_execution_allowed(&file_system_policy));
    assert_eq!(
        sandbox_permissions_preserving_denied_reads(
            SandboxPermissions::RequireEscalated,
            &file_system_policy,
        ),
        SandboxPermissions::UseDefault,
    );
    assert_eq!(
        sandbox_permissions_preserving_denied_reads(
            SandboxPermissions::WithAdditionalPermissions,
            &file_system_policy,
        ),
        SandboxPermissions::WithAdditionalPermissions,
    );
    assert_eq!(
        sandbox_permissions_preserving_denied_reads(
            SandboxPermissions::RequireEscalated,
            &FileSystemSandboxPolicy::default(),
        ),
        SandboxPermissions::RequireEscalated,
    );
    assert_eq!(
        sandbox_override_for_first_attempt(
            SandboxPermissions::WithAdditionalPermissions,
            &ExecApprovalRequirement::Skip {
                bypass_sandbox: true,
                proposed_execpolicy_amendment: None,
            },
            &file_system_policy,
        ),
        SandboxOverride::NoOverride,
        "exec-policy allow rules would drop deny-read filesystem policy, so keep the first attempt sandboxed",
    );
}

#[test]
fn exec_server_env_keeps_command_native_and_carries_sandbox_context() {
    let cwd: AbsolutePathBuf = std::env::current_dir()
        .expect("current dir")
        .try_into()
        .expect("absolute cwd");
    let cwd_uri = PathUri::from_abs_path(&cwd);
    let exec_server_permissions = codex_protocol::models::PermissionProfile::workspace_write();
    let permissions = exec_server_permissions
        .clone()
        .materialize_project_roots_with_workspace_roots(std::slice::from_ref(&cwd));
    let manager = SandboxManager::new();
    let mut attempt = SandboxAttempt {
        sandbox: SandboxType::None,
        sandbox_requested: true,
        permissions: &permissions,
        exec_server_permissions: &exec_server_permissions,
        enforce_managed_network: true,
        manager: &manager,
        sandbox_cwd: &cwd_uri,
        workspace_roots: std::slice::from_ref(&cwd_uri),
        codex_linux_sandbox_exe: None,
        use_legacy_landlock: false,
        windows_sandbox_level: codex_protocol::config_types::WindowsSandboxLevel::Disabled,
        windows_sandbox_private_desktop: false,
        network_denial_cancellation_token: None,
        network_proxy: None,
    };
    let managed_network = ManagedNetworkSandboxContext {
        loopback_ports: vec![43123],
        allow_local_binding: false,
    };
    let command = || SandboxCommand {
        program: "/bin/bash".into(),
        args: vec!["-lc".to_string(), "pwd".to_string()],
        cwd: cwd_uri.clone(),
        env: HashMap::new(),
        managed_network: Some(managed_network.clone()),
        additional_permissions: None,
    };
    let options = || crate::sandboxing::ExecOptions {
        expiration: crate::exec::ExecExpiration::DefaultTimeout,
        capture_policy: crate::exec::ExecCapturePolicy::ShellTool,
    };
    let request = attempt
        .env_for_exec_server(command(), options())
        .expect("prepare remote exec request");

    assert_eq!(
        request.command,
        vec![
            "/bin/bash".to_string(),
            "-lc".to_string(),
            "pwd".to_string()
        ]
    );
    assert_eq!(request.arg0, None);
    assert_eq!(request.sandbox, SandboxType::None);
    assert_eq!(
        request.exec_server_sandbox,
        Some(codex_exec_server::FileSystemSandboxContext {
            permissions: exec_server_permissions.clone().into(),
            cwd: Some(cwd_uri.clone()),
            workspace_roots: vec![cwd_uri.clone()],
            windows_sandbox_level: codex_protocol::config_types::WindowsSandboxLevel::Disabled,
            windows_sandbox_private_desktop: false,
            windows_sandbox_proxy_settings_mode: None,
            use_legacy_landlock: false,
        })
    );
    assert!(request.exec_server_enforce_managed_network);
    assert_eq!(
        request.exec_server_managed_network,
        Some(managed_network.clone())
    );

    attempt.sandbox_requested = false;
    let request = attempt
        .env_for_exec_server(command(), options())
        .expect("prepare unsandboxed remote exec request");

    assert_eq!(request.exec_server_sandbox, None);
    assert!(!request.exec_server_enforce_managed_network);
    assert_eq!(request.exec_server_managed_network, Some(managed_network));
}

/// PF-27-S02: drives `env_for` / `env_for_exec_server` with an explicit armed
/// contract (the process-wide one is never armed in unit tests).
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn pf_27_s02_env_for_applies_the_launch_contract() {
    use crate::security::launch_contract::LaunchContract;
    use std::ffi::OsString;

    const RAW: &str = "ghp_pf27s02EnvForCanary000000000000000000000";
    let codex_home_dir = tempfile::tempdir().expect("codex home");
    let workspace_dir = tempfile::tempdir().expect("workspace");
    let codex_home =
        AbsolutePathBuf::from_absolute_path(codex_home_dir.path()).expect("absolute home");
    let cwd = AbsolutePathBuf::from_absolute_path(workspace_dir.path()).expect("absolute cwd");
    let contract = LaunchContract::capture(
        &codex_home,
        [(OsString::from("GITHUB_TOKEN"), OsString::from(RAW))],
        /*hardened*/ true,
    );
    let cwd_uri = PathUri::from_abs_path(&cwd);
    let exec_server_permissions = codex_protocol::models::PermissionProfile::workspace_write();
    let permissions = exec_server_permissions
        .clone()
        .materialize_project_roots_with_workspace_roots(std::slice::from_ref(&cwd));
    let manager = SandboxManager::new();
    let sandbox = if cfg!(target_os = "macos") {
        SandboxType::MacosSeatbelt
    } else {
        SandboxType::LinuxSeccomp
    };
    let linux_sandbox_exe = std::path::PathBuf::from("/bin/true");
    let mut attempt = SandboxAttempt {
        sandbox,
        sandbox_requested: true,
        permissions: &permissions,
        exec_server_permissions: &exec_server_permissions,
        enforce_managed_network: false,
        manager: &manager,
        sandbox_cwd: &cwd_uri,
        workspace_roots: std::slice::from_ref(&cwd_uri),
        codex_linux_sandbox_exe: Some(&linux_sandbox_exe),
        use_legacy_landlock: false,
        windows_sandbox_level: codex_protocol::config_types::WindowsSandboxLevel::Disabled,
        windows_sandbox_private_desktop: false,
        network_denial_cancellation_token: None,
        network_proxy: None,
    };
    let command = |args: &[&str], env: &[(&str, &str)]| SandboxCommand {
        program: "/bin/sh".into(),
        args: args.iter().map(ToString::to_string).collect(),
        cwd: cwd_uri.clone(),
        env: env
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect(),
        managed_network: None,
        additional_permissions: None,
    };
    let options = || crate::sandboxing::ExecOptions {
        expiration: crate::exec::ExecExpiration::DefaultTimeout,
        capture_policy: crate::exec::ExecCapturePolicy::ShellTool,
    };
    let refusal = |result: Result<crate::sandboxing::ExecRequest, CodexErr>| match result {
        Err(err) => err.to_string(),
        Ok(_) => "accepted".to_string(),
    };

    // Accepted: the protected profile denies the vault store; raw values are scrubbed.
    let request = attempt
        .env_for_with_contract(
            Some(&contract),
            command(&["-c", "true"], &[("LEAK", RAW), ("PATH", "/usr/bin")]),
            options(),
            /*network*/ None,
            /*environment_id*/ None,
        )
        .expect("protected launch");
    assert!(request.env.values().all(|value| value != RAW));
    assert!(
        !request
            .permission_profile
            .file_system_sandbox_policy()
            .can_read_path_with_cwd(codex_home.join("secrets").as_path(), cwd.as_path())
    );

    // Refused: login shell, raw argv, unsandboxed attempt, remote environment.
    assert!(
        refusal(attempt.env_for_with_contract(
            Some(&contract),
            command(&["-lc", "true"], &[]),
            options(),
            /*network*/ None,
            /*environment_id*/ None,
        ))
        .contains("Protected launch refused: login shells")
    );
    assert!(
        refusal(attempt.env_for_with_contract(
            Some(&contract),
            command(&["-c", &format!("echo {RAW}")], &[]),
            options(),
            /*network*/ None,
            /*environment_id*/ None,
        ))
        .contains("command line contains a managed secret")
    );
    assert!(
        refusal(attempt.env_for_exec_server_with_contract(
            Some(&contract),
            command(&["-c", "true"], &[]),
            options(),
        ))
        .contains("remote environment")
    );
    attempt.sandbox = SandboxType::None;
    attempt.sandbox_requested = false;
    assert!(
        refusal(attempt.env_for_with_contract(
            Some(&contract),
            command(&["-c", "true"], &[]),
            options(),
            /*network*/ None,
            /*environment_id*/ None,
        ))
        .contains("outside the OS sandbox")
    );
    // Without a contract the same unsandboxed launch is unchanged.
    assert_eq!(
        refusal(attempt.env_for_with_contract(
            /*contract*/ None,
            command(&["-c", "true"], &[]),
            options(),
            /*network*/ None,
            /*environment_id*/ None,
        )),
        "accepted"
    );
}

/// PF-27-S06: on Windows the contract accepts only launches that run under
/// the elevated sandbox; the unelevated restricted-token one is refused.
#[cfg(windows)]
#[test]
fn pf_27_s06_env_for_requires_the_elevated_windows_sandbox() {
    use crate::security::launch_contract::LaunchContract;
    use codex_protocol::config_types::WindowsSandboxLevel;

    let codex_home_dir = tempfile::tempdir().expect("codex home");
    let workspace_dir = tempfile::tempdir().expect("workspace");
    let codex_home =
        AbsolutePathBuf::from_absolute_path(codex_home_dir.path()).expect("absolute home");
    let cwd = AbsolutePathBuf::from_absolute_path(workspace_dir.path()).expect("absolute cwd");
    let contract = LaunchContract::capture(&codex_home, std::iter::empty(), /*hardened*/ true);
    let cwd_uri = PathUri::from_abs_path(&cwd);
    let exec_server_permissions = codex_protocol::models::PermissionProfile::workspace_write();
    let permissions = exec_server_permissions
        .clone()
        .materialize_project_roots_with_workspace_roots(std::slice::from_ref(&cwd));
    let manager = SandboxManager::new();
    let mut attempt = SandboxAttempt {
        sandbox: SandboxType::WindowsRestrictedToken,
        sandbox_requested: true,
        permissions: &permissions,
        exec_server_permissions: &exec_server_permissions,
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
    };
    let launch = |attempt: &SandboxAttempt<'_>| {
        attempt
            .env_for_with_contract(
                Some(&contract),
                SandboxCommand {
                    program: "cmd.exe".into(),
                    args: vec!["/D".to_string(), "/C".to_string(), "echo ok".to_string()],
                    cwd: cwd_uri.clone(),
                    env: std::collections::HashMap::new(),
                    managed_network: None,
                    additional_permissions: None,
                },
                crate::sandboxing::ExecOptions {
                    expiration: crate::exec::ExecExpiration::DefaultTimeout,
                    capture_policy: crate::exec::ExecCapturePolicy::ShellTool,
                },
                /*network*/ None,
                /*environment_id*/ None,
            )
            .map(|_| ())
            .map_err(|err| err.to_string())
    };
    let refused = launch(&attempt).expect_err("unelevated launch refused");
    assert!(refused.contains("unelevated Windows sandbox"), "{refused}");
    attempt.windows_sandbox_level = WindowsSandboxLevel::Elevated;
    match launch(&attempt) {
        Ok(()) => {}
        // Before the elevated sandbox's first setup its users group does not
        // exist, and the launch is refused rather than left unprotected.
        Err(refused)
            if codex_windows_sandbox::resolve_sid("CodexSandboxUsers").is_err()
                && refused.contains("not set up yet") => {}
        Err(refused) => panic!("elevated launch refused: {refused}"),
    }
}

/// #300: the unelevated Windows sandbox cannot block reads, so the tool path
/// (`env_for`) refuses a profile with deny-read entries, as
/// `process_exec_tool_call` does. The same profile without the deny entry,
/// and the elevated backend, are unaffected.
#[test]
fn sec_win_300_unelevated_tool_path_refuses_deny_read_profiles() {
    use codex_protocol::config_types::WindowsSandboxLevel;
    use codex_protocol::models::PermissionProfile;
    use codex_protocol::permissions::NetworkSandboxPolicy;

    let workspace_dir = tempfile::tempdir().expect("workspace");
    let cwd = AbsolutePathBuf::from_absolute_path(
        dunce::canonicalize(workspace_dir.path()).expect("canonical workspace"),
    )
    .expect("absolute cwd");
    let secret = cwd.join("secret.env");
    std::fs::write(&secret, "sec-win-300").expect("secret file");
    let cwd_uri = PathUri::from_abs_path(&cwd);
    let base = PermissionProfile::workspace_write()
        .materialize_project_roots_with_workspace_roots(std::slice::from_ref(&cwd));
    let (mut file_system, _) = base.to_runtime_permissions();
    file_system.entries.push(FileSystemSandboxEntry {
        path: FileSystemPath::Path {
            path: secret.clone(),
        },
        access: FileSystemAccessMode::Deny,
        missing_path_behavior: None,
    });
    let denying =
        PermissionProfile::from_runtime_permissions(&file_system, NetworkSandboxPolicy::Restricted);
    let manager = SandboxManager::new();
    let launch = |permissions: &PermissionProfile, level: WindowsSandboxLevel| {
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
            windows_sandbox_level: level,
            windows_sandbox_private_desktop: false,
            network_denial_cancellation_token: None,
            network_proxy: None,
        }
        .env_for_with_contract(
            /*contract*/ None,
            SandboxCommand {
                program: "cmd.exe".into(),
                args: vec!["/D".to_string(), "/C".to_string(), "echo ok".to_string()],
                cwd: cwd_uri.clone(),
                env: HashMap::new(),
                managed_network: None,
                additional_permissions: None,
            },
            crate::sandboxing::ExecOptions {
                expiration: crate::exec::ExecExpiration::DefaultTimeout,
                capture_policy: crate::exec::ExecCapturePolicy::ShellTool,
            },
            /*network*/ None,
            /*environment_id*/ None,
        )
    };

    let refused = launch(&denying, WindowsSandboxLevel::RestrictedToken)
        .expect_err("the unelevated tool path must refuse a deny-read profile");
    assert_eq!(
        refused.to_string(),
        format!(
            "unsupported operation: {}",
            codex_sandboxing::UNELEVATED_DENY_READ_REFUSAL
        )
    );
    launch(&base, WindowsSandboxLevel::RestrictedToken)
        .expect("a profile without deny-read entries still runs unelevated");
    let elevated = launch(&denying, WindowsSandboxLevel::Elevated)
        .expect("the elevated backend enforces deny-read entries");
    assert!(
        elevated
            .windows_sandbox_filesystem_overrides
            .is_some_and(|overrides| overrides.additional_deny_read.contains_path(&secret)),
        "the elevated launch carries the deny entry"
    );
}
