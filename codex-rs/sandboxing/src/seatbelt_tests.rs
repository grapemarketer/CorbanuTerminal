use super::CreateSeatbeltCommandArgsParams;
use super::MACOS_PATH_TO_SEATBELT_EXECUTABLE;
use super::MACOS_SEATBELT_BASE_POLICY;
use super::ProxyPolicyInputs;
use super::UnixDomainSocketPolicy;
use super::build_seatbelt_unreadable_glob_policy;
use super::create_seatbelt_command_args;
use super::create_seatbelt_command_args_for_legacy_policy;
use super::dynamic_network_policy;
use super::normalize_path_for_sandbox;
use super::seatbelt_regex_for_unreadable_glob;
use super::unix_socket_dir_params;
use super::unix_socket_policy;
use codex_network_proxy::ConfigReloader;
use codex_network_proxy::ConfigReloaderFuture;
use codex_network_proxy::ConfigState;
use codex_network_proxy::ManagedNetworkSandboxContext;
use codex_network_proxy::NetworkMode;
use codex_network_proxy::NetworkProxy;
use codex_network_proxy::NetworkProxyConfig;
use codex_network_proxy::NetworkProxyConstraints;
use codex_network_proxy::NetworkProxyState;
use codex_network_proxy::build_config_state;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::permissions::PROTECTED_METADATA_PATH_NAMES;
use codex_protocol::protocol::SandboxPolicy;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use tempfile::TempDir;

fn assert_seatbelt_denied(stderr: &[u8], path: &Path) {
    let stderr = String::from_utf8_lossy(stderr);
    let expected = format!("bash: {}: Operation not permitted\n", path.display());
    assert!(
        stderr == expected
            || stderr.contains("sandbox-exec: sandbox_apply: Operation not permitted"),
        "unexpected stderr: {stderr}"
    );
}

fn absolute_path(path: &str) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path(Path::new(path)).expect("absolute path")
}

fn seatbelt_policy_arg(args: &[String]) -> &str {
    let policy_index = args
        .iter()
        .position(|arg| arg == "-p")
        .expect("seatbelt args should include -p");
    args.get(policy_index + 1)
        .expect("seatbelt args should include policy text")
}

fn seatbelt_protected_metadata_name_requirements(root: &Path) -> String {
    let mut root = root.to_string_lossy().to_string();
    while root.len() > 1 && root.ends_with('/') {
        root.pop();
    }
    let root = regex_lite::escape(&root);
    PROTECTED_METADATA_PATH_NAMES
        .iter()
        .map(|name| {
            let name = regex_lite::escape(name);
            if root == "/" {
                format!(r#"(require-not (regex #"^/{name}(/.*)?$"))"#)
            } else {
                format!(r#"(require-not (regex #"^{root}/{name}(/.*)?$"))"#)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

struct TestConfigReloader;

impl ConfigReloader for TestConfigReloader {
    fn source_label(&self) -> String {
        "seatbelt test config".to_string()
    }

    fn maybe_reload(&self) -> ConfigReloaderFuture<'_, Option<ConfigState>> {
        Box::pin(async { Ok(None) })
    }

    fn reload_now(&self) -> ConfigReloaderFuture<'_, ConfigState> {
        Box::pin(async { Err(anyhow::anyhow!("seatbelt test config cannot reload")) })
    }
}

#[test]
fn base_policy_allows_node_cpu_sysctls() {
    assert!(
        MACOS_SEATBELT_BASE_POLICY.contains("(sysctl-name \"machdep.cpu.brand_string\")"),
        "base policy must allow CPU brand lookup for os.cpus()"
    );
    assert!(
        MACOS_SEATBELT_BASE_POLICY.contains("(sysctl-name \"hw.model\")"),
        "base policy must allow hardware model lookup for os.cpus()"
    );
}

#[test]
fn base_policy_allows_kmp_registration_shm_read_create_and_unlink() {
    let expected = r##"(allow ipc-posix-shm-read-data
  ipc-posix-shm-write-create
  ipc-posix-shm-write-unlink
  (ipc-posix-name-regex #"^/__KMP_REGISTERED_LIB_[0-9]+$"))"##;

    assert!(
        MACOS_SEATBELT_BASE_POLICY.contains(expected),
        "base policy must allow only KMP registration shm read/create/unlink:\n{MACOS_SEATBELT_BASE_POLICY}"
    );
}

/// #444 (AGG-36): with network off, confstr(DARWIN_USER_TEMP_DIR) needs these
/// lookups; without them every /usr/bin/python3 run printed xcrun errors.
#[test]
fn base_policy_resolves_user_folders_without_network() {
    for name in [
        "(global-name \"com.apple.bsd.dirhelper\")",
        "(global-name \"com.apple.system.opendirectoryd.membership\")",
    ] {
        assert!(
            MACOS_SEATBELT_BASE_POLICY.contains(name),
            "base policy must allow {name}:\n{MACOS_SEATBELT_BASE_POLICY}"
        );
    }
}

/// #444 (AGG-36): under a network-off sandbox without writable temp folders
/// (as Aggressive), the per-user temp folder still resolves; the shims use it
/// to find their cache instead of failing over to an unwritable /tmp.
#[test]
fn user_temp_folder_resolves_in_a_network_off_sandbox() {
    let cwd = TempDir::new().expect("cwd");
    let policy = SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![],
        network_access: false,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: true,
    };
    let args = create_seatbelt_command_args_for_legacy_policy(
        vec![
            "/usr/bin/getconf".to_string(),
            "DARWIN_USER_TEMP_DIR".to_string(),
        ],
        &policy,
        cwd.path(),
        /*enforce_managed_network*/ false,
        /*network*/ None,
    )
    .expect("seatbelt args");
    let output = Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
        .args(&args)
        .current_dir(cwd.path())
        .output()
        .expect("execute seatbelt command");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.trim().starts_with('/'),
        "getconf DARWIN_USER_TEMP_DIR failed under the sandbox: status {:?}, stdout {stdout:?}, stderr {:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn create_seatbelt_args_routes_network_through_proxy_ports() {
    let policy = dynamic_network_policy(
        &SandboxPolicy::new_read_only_policy(),
        /*enforce_managed_network*/ false,
        &ProxyPolicyInputs {
            ports: vec![43128, 48081],
            has_proxy_config: true,
            allow_local_binding: false,
            ..ProxyPolicyInputs::default()
        },
    );

    assert!(
        policy.contains("(allow network-outbound (remote ip \"localhost:43128\"))"),
        "expected HTTP proxy port allow rule in policy:\n{policy}"
    );
    assert!(
        policy.contains("(allow network-outbound (remote ip \"localhost:48081\"))"),
        "expected SOCKS proxy port allow rule in policy:\n{policy}"
    );
    assert!(
        !policy.contains("\n(allow network-outbound)\n"),
        "policy should not include blanket outbound allowance when proxy ports are present:\n{policy}"
    );
    assert!(
        !policy.contains("(allow network-bind (local ip \"*:*\"))"),
        "policy should not allow local binding unless explicitly enabled:\n{policy}"
    );
    assert!(
        !policy.contains("(allow network-inbound (local ip \"localhost:*\"))"),
        "policy should not allow loopback inbound unless explicitly enabled:\n{policy}"
    );
    assert!(
        !policy.contains("(allow network-outbound (remote ip \"*:53\"))"),
        "policy should not allow raw DNS unless local binding is explicitly enabled:\n{policy}"
    );
}

#[test]
fn dynamic_network_policy_allows_tls_without_darwin_user_cache_write() {
    let policy = dynamic_network_policy(
        &SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            network_access: true,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        },
        /*enforce_managed_network*/ false,
        &ProxyPolicyInputs::default(),
    );

    assert!(
        policy.contains("(global-name \"com.apple.trustd.agent\")"),
        "policy should keep trustd agent access for TLS certificate verification:\n{policy}"
    );
    assert!(
        !policy.contains("DARWIN_USER_CACHE_DIR"),
        "network policy should not grant broad user cache writes:\n{policy}"
    );
}

#[test]
fn explicit_unreadable_paths_are_excluded_from_full_disk_read_and_write_access() {
    let unreadable = absolute_path("/tmp/codex-unreadable");
    let file_system_policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Write,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path { path: unreadable },
            access: FileSystemAccessMode::Deny,
            missing_path_behavior: None,
        },
    ]);

    let args = create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
        command: vec!["/bin/true".to_string()],
        file_system_sandbox_policy: &file_system_policy,
        network_sandbox_policy: NetworkSandboxPolicy::Restricted,
        sandbox_policy_cwd: Path::new("/"),
        enforce_managed_network: false,
        managed_network: None,
        environment_id: None,
        network: None,
        extra_allow_unix_sockets: &[],
    })
    .unwrap();

    let policy = seatbelt_policy_arg(&args);
    let unreadable_roots = file_system_policy.get_unreadable_roots_with_cwd(Path::new("/"));
    let unreadable_root = unreadable_roots.first().expect("expected unreadable root");
    assert!(
        policy.contains("(require-not (literal (param \"READABLE_ROOT_0_EXCLUDED_0\")))"),
        "expected exact read carveout in policy:\n{policy}"
    );
    assert!(
        policy.contains("(require-not (subpath (param \"READABLE_ROOT_0_EXCLUDED_0\")))"),
        "expected read carveout in policy:\n{policy}"
    );
    assert!(
        policy.contains("(require-not (literal (param \"WRITABLE_ROOT_0_EXCLUDED_0\")))"),
        "expected exact write carveout in policy:\n{policy}"
    );
    assert!(
        policy.contains("(require-not (subpath (param \"WRITABLE_ROOT_0_EXCLUDED_0\")))"),
        "expected write carveout in policy:\n{policy}"
    );
    assert!(
        policy.contains(&seatbelt_protected_metadata_name_requirements(Path::new(
            "/"
        ))),
        "expected metadata protection regex deny requirements in policy:\n{policy}"
    );
    assert!(
        args.iter().any(
            |arg| arg == &format!("-DREADABLE_ROOT_0_EXCLUDED_0={}", unreadable_root.display())
        ),
        "expected read carveout parameter in args: {args:#?}"
    );
    let writable_definitions: Vec<String> = args
        .iter()
        .filter(|arg| arg.starts_with("-DWRITABLE_ROOT_"))
        .cloned()
        .collect();
    assert_eq!(
        writable_definitions,
        vec![
            "-DWRITABLE_ROOT_0=/".to_string(),
            "-DWRITABLE_ROOT_0_EXCLUDED_0=/.codex".to_string(),
            format!("-DWRITABLE_ROOT_0_EXCLUDED_1={}", unreadable_root.display()),
        ],
        "unexpected write carveout parameters in args: {args:#?}"
    );
}

#[test]
fn prepared_managed_network_context_allows_only_its_proxy_ports() {
    let file_system_policy = FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(
        &SandboxPolicy::new_read_only_policy(),
        Path::new("/"),
    );
    let managed_network = ManagedNetworkSandboxContext {
        loopback_ports: vec![43123, 48081],
        allow_local_binding: false,
    };
    let args = create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
        command: vec!["/bin/true".to_string()],
        file_system_sandbox_policy: &file_system_policy,
        network_sandbox_policy: NetworkSandboxPolicy::Restricted,
        sandbox_policy_cwd: Path::new("/"),
        enforce_managed_network: true,
        managed_network: Some(&managed_network),
        environment_id: None,
        network: None,
        extra_allow_unix_sockets: &[],
    })
    .unwrap();

    let policy = seatbelt_policy_arg(&args);
    assert!(policy.contains("(allow network-outbound (remote ip \"localhost:43123\"))"));
    assert!(policy.contains("(allow network-outbound (remote ip \"localhost:48081\"))"));
    assert!(!policy.contains("(allow network-outbound (remote ip \"localhost:9999\"))"));
    assert!(!policy.contains("(allow network-bind (local ip \"*:*\"))"));
    assert!(!policy.contains("(allow network-outbound)\n"));
}

#[test]
fn explicit_unreadable_paths_are_excluded_from_readable_roots() {
    let root = absolute_path("/tmp/codex-readable");
    let unreadable = absolute_path("/tmp/codex-readable/private");
    let file_system_policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Path { path: root },
            access: FileSystemAccessMode::Read,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path { path: unreadable },
            access: FileSystemAccessMode::Deny,
            missing_path_behavior: None,
        },
    ]);

    let args = create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
        command: vec!["/bin/true".to_string()],
        file_system_sandbox_policy: &file_system_policy,
        network_sandbox_policy: NetworkSandboxPolicy::Restricted,
        sandbox_policy_cwd: Path::new("/"),
        enforce_managed_network: false,
        managed_network: None,
        environment_id: None,
        network: None,
        extra_allow_unix_sockets: &[],
    })
    .unwrap();

    let policy = seatbelt_policy_arg(&args);
    let readable_roots = file_system_policy.get_readable_roots_with_cwd(Path::new("/"));
    let readable_root = readable_roots.first().expect("expected readable root");
    let unreadable_roots = file_system_policy.get_unreadable_roots_with_cwd(Path::new("/"));
    let unreadable_root = unreadable_roots.first().expect("expected unreadable root");
    assert!(
        policy.contains("(require-not (literal (param \"READABLE_ROOT_0_EXCLUDED_0\")))"),
        "expected exact read carveout in policy:\n{policy}"
    );
    assert!(
        policy.contains("(require-not (subpath (param \"READABLE_ROOT_0_EXCLUDED_0\")))"),
        "expected read carveout in policy:\n{policy}"
    );
    assert!(
        args.iter()
            .any(|arg| arg == &format!("-DREADABLE_ROOT_0={}", readable_root.display())),
        "expected readable root parameter in args: {args:#?}"
    );
    assert!(
        args.iter().any(
            |arg| arg == &format!("-DREADABLE_ROOT_0_EXCLUDED_0={}", unreadable_root.display())
        ),
        "expected read carveout parameter in args: {args:#?}"
    );
}

#[test]
fn unreadable_globstar_slash_matches_zero_or_more_directories() {
    let regex = seatbelt_regex_for_unreadable_glob("/tmp/repo/**/*.env");
    assert_eq!(regex.as_deref(), Some(r"^/tmp/repo/(.*/)?[^/]*\.env$"));
    let regex = regex_lite::Regex::new(regex.as_deref().expect("glob should compile"))
        .expect("regex should compile");

    assert!(regex.is_match("/tmp/repo/.env"));
    assert!(regex.is_match("/tmp/repo/app/.env"));
    assert!(regex.is_match("/tmp/repo/app/config.env"));
    assert!(!regex.is_match("/tmp/repo/app/config.toml"));
}

#[test]
fn unreadable_globs_use_git_style_component_matching() {
    let regex = seatbelt_regex_for_unreadable_glob("/tmp/repo/*/file[0-9]?.txt");
    assert_eq!(
        regex.as_deref(),
        Some(r"^/tmp/repo/[^/]*/file[0-9][^/]\.txt$")
    );
    let regex = regex_lite::Regex::new(regex.as_deref().expect("glob should compile"))
        .expect("regex should compile");

    assert!(regex.is_match("/tmp/repo/app/file42.txt"));
    assert!(!regex.is_match("/tmp/repo/app/nested/file42.txt"));
    assert!(!regex.is_match("/tmp/repo/app/file4.txt"));
    assert!(!regex.is_match("/tmp/repo/app/fileab.txt"));
}

#[test]
fn unreadable_globs_treat_unclosed_character_classes_as_literals() {
    let regex = seatbelt_regex_for_unreadable_glob("/tmp/repo/[*.env");
    assert_eq!(regex.as_deref(), Some(r"^/tmp/repo/\[[^/]*\.env$"));
    let regex = regex_lite::Regex::new(regex.as_deref().expect("glob should compile"))
        .expect("regex should compile");

    assert!(regex.is_match("/tmp/repo/[local.env"));
    assert!(regex.is_match("/tmp/repo/[.env"));
    assert!(!regex.is_match("/tmp/repo/local.env"));
}

#[cfg(unix)]
#[test]
fn unreadable_glob_policy_includes_canonicalized_static_prefix() {
    use std::os::unix::fs::symlink;

    let temp_dir = TempDir::new().expect("temp dir");
    let real_root = temp_dir.path().join("real-root");
    let link_root = temp_dir.path().join("link-root");
    fs::create_dir(&real_root).expect("create real root");
    symlink(&real_root, &link_root).expect("create symlinked root");

    let pattern = format!("{}/**/*.env", link_root.display());
    let canonical_pattern = format!(
        "{}/**/*.env",
        real_root
            .canonicalize()
            .expect("canonicalize real root")
            .display()
    );
    let expected_regex = seatbelt_regex_for_unreadable_glob(&canonical_pattern)
        .expect("canonical glob should compile");
    let mut policy = FileSystemSandboxPolicy::default();
    policy.entries.push(FileSystemSandboxEntry {
        path: FileSystemPath::GlobPattern {
            pattern,
            scan: None,
        },
        access: FileSystemAccessMode::Deny,
        missing_path_behavior: None,
    });

    let seatbelt_policy = build_seatbelt_unreadable_glob_policy(&policy, temp_dir.path());

    assert!(
        seatbelt_policy.contains(&format!(r#"(deny file-read* (regex #"{expected_regex}"))"#)),
        "expected canonicalized glob regex in policy:\n{seatbelt_policy}"
    );
}

#[test]
fn seatbelt_args_without_extension_profile_keep_legacy_preferences_read_access() {
    let cwd = std::env::temp_dir();
    let args = create_seatbelt_command_args_for_legacy_policy(
        vec!["echo".to_string(), "ok".to_string()],
        &SandboxPolicy::new_read_only_policy(),
        cwd.as_path(),
        /*enforce_managed_network*/ false,
        /*network*/ None,
    )
    .unwrap();
    let policy = &args[1];
    assert!(policy.contains("(allow user-preference-read)"));
    assert!(!policy.contains("(allow user-preference-write)"));
}

#[test]
fn create_seatbelt_args_allows_local_binding_when_explicitly_enabled() {
    let policy = dynamic_network_policy(
        &SandboxPolicy::new_read_only_policy(),
        /*enforce_managed_network*/ false,
        &ProxyPolicyInputs {
            ports: vec![43128],
            has_proxy_config: true,
            allow_local_binding: true,
            ..ProxyPolicyInputs::default()
        },
    );

    assert!(
        policy.contains("(allow network-bind (local ip \"*:*\"))"),
        "policy should allow loopback local binding when explicitly enabled:\n{policy}"
    );
    assert!(
        policy.contains("(allow network-inbound (local ip \"localhost:*\"))"),
        "policy should allow loopback inbound when explicitly enabled:\n{policy}"
    );
    assert!(
        policy.contains("(allow network-outbound (remote ip \"localhost:*\"))"),
        "policy should allow loopback outbound when explicitly enabled:\n{policy}"
    );
    assert!(
        policy.contains("(allow network-outbound (remote ip \"*:53\"))"),
        "policy should allow DNS egress when local binding is explicitly enabled:\n{policy}"
    );
    assert!(
        !policy.contains("\n(allow network-outbound)\n"),
        "policy should keep proxy-routed behavior without blanket outbound allowance:\n{policy}"
    );
}

#[test]
fn dynamic_network_policy_preserves_restricted_policy_when_proxy_config_without_ports() {
    let policy = dynamic_network_policy(
        &SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            network_access: true,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        },
        /*enforce_managed_network*/ false,
        &ProxyPolicyInputs {
            ports: vec![],
            has_proxy_config: true,
            allow_local_binding: false,
            ..ProxyPolicyInputs::default()
        },
    );

    assert!(
        policy.contains("(socket-domain AF_SYSTEM)"),
        "policy should keep the restricted network profile when proxy config is present without ports:\n{policy}"
    );
    assert!(
        !policy.contains("\n(allow network-outbound)\n"),
        "policy should not include blanket outbound allowance when proxy config is present without ports:\n{policy}"
    );
    assert!(
        !policy.contains("(allow network-outbound (remote ip \"localhost:"),
        "policy should not include proxy port allowance when proxy config is present without ports:\n{policy}"
    );
    assert!(
        !policy.contains("(allow network-outbound (remote ip \"*:53\"))"),
        "policy should stay fail-closed for DNS when no proxy ports are available:\n{policy}"
    );
}

#[test]
fn dynamic_network_policy_blocks_dns_when_local_binding_has_no_proxy_ports() {
    let policy = dynamic_network_policy(
        &SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            network_access: true,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        },
        /*enforce_managed_network*/ false,
        &ProxyPolicyInputs {
            ports: vec![],
            has_proxy_config: true,
            allow_local_binding: true,
            ..ProxyPolicyInputs::default()
        },
    );

    assert!(
        policy.contains("(allow network-bind (local ip \"*:*\"))"),
        "policy should still allow explicitly configured local binding:\n{policy}"
    );
    assert!(
        !policy.contains("(allow network-outbound (remote ip \"*:53\"))"),
        "policy should not allow DNS egress when no proxy ports are available:\n{policy}"
    );
}

#[test]
fn dynamic_network_policy_preserves_restricted_policy_for_managed_network_without_proxy_config() {
    let policy = dynamic_network_policy(
        &SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            network_access: true,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        },
        /*enforce_managed_network*/ true,
        &ProxyPolicyInputs {
            ports: vec![],
            has_proxy_config: false,
            allow_local_binding: false,
            ..ProxyPolicyInputs::default()
        },
    );

    assert!(
        policy.contains("(socket-domain AF_SYSTEM)"),
        "policy should keep the restricted network profile when managed network is active without proxy endpoints:\n{policy}"
    );
    assert!(
        !policy.contains("\n(allow network-outbound)\n"),
        "policy should not include blanket outbound allowance when managed network is active without proxy endpoints:\n{policy}"
    );
    assert!(
        !policy.contains("(allow network-outbound (remote ip \"*:53\"))"),
        "policy should stay fail-closed for DNS when no proxy endpoints are available:\n{policy}"
    );
}

#[test]
fn create_seatbelt_args_allowlists_unix_socket_paths() {
    let policy = dynamic_network_policy(
        &SandboxPolicy::new_read_only_policy(),
        /*enforce_managed_network*/ false,
        &ProxyPolicyInputs {
            ports: vec![43128],
            has_proxy_config: true,
            allow_local_binding: false,
            unix_domain_socket_policy: UnixDomainSocketPolicy::Restricted {
                allowed: vec![absolute_path("/tmp/example.sock")],
            },
        },
    );

    assert!(
        policy.contains("(allow system-socket (socket-domain AF_UNIX))"),
        "policy should allow AF_UNIX socket creation for configured unix sockets:\n{policy}"
    );
    assert!(
        policy.contains(
            "(allow network-bind (local unix-socket (subpath (param \"UNIX_SOCKET_PATH_0\"))))"
        ),
        "policy should allow binding explicitly configured unix sockets:\n{policy}"
    );
    assert!(
        policy.contains(
            "(allow network-outbound (remote unix-socket (subpath (param \"UNIX_SOCKET_PATH_0\"))))"
        ),
        "policy should allow connecting to explicitly configured unix sockets:\n{policy}"
    );
    assert!(
        !policy.contains("(allow network* (subpath"),
        "policy should no longer use the generic subpath unix-socket rules:\n{policy}"
    );
}

#[test]
fn create_seatbelt_args_allowlists_explicit_unix_socket_paths_without_proxy() {
    let cwd = TempDir::new().expect("temp cwd");
    let file_system_policy = FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(
        &SandboxPolicy::new_read_only_policy(),
        cwd.path(),
    );
    let extra_allow_unix_sockets = vec![absolute_path("/tmp/codex-browser-use")];
    let args = create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
        command: vec!["/usr/bin/true".to_string()],
        file_system_sandbox_policy: &file_system_policy,
        network_sandbox_policy: NetworkSandboxPolicy::Restricted,
        sandbox_policy_cwd: cwd.path(),
        enforce_managed_network: false,
        managed_network: None,
        environment_id: None,
        network: None,
        extra_allow_unix_sockets: &extra_allow_unix_sockets,
    })
    .unwrap();
    let policy = seatbelt_policy_arg(&args);

    assert!(
        policy.contains("(allow system-socket (socket-domain AF_UNIX))"),
        "policy should allow AF_UNIX when explicit socket paths are requested:\n{policy}"
    );
    assert!(
        policy.contains(
            "(allow network-outbound (remote unix-socket (subpath (param \"UNIX_SOCKET_PATH_0\"))))"
        ),
        "policy should allow outbound AF_UNIX traffic for explicit socket paths:\n{policy}"
    );
    let expected_socket_root = normalize_path_for_sandbox(Path::new("/tmp/codex-browser-use"))
        .expect("socket root should normalize")
        .to_string_lossy()
        .into_owned();
    assert!(
        args.iter()
            .any(|arg| arg == &format!("-DUNIX_SOCKET_PATH_0={expected_socket_root}")),
        "seatbelt args should pass the configured socket root as a sandbox param: {args:?}"
    );
}

#[tokio::test]
async fn create_seatbelt_args_merges_proxy_and_explicit_unix_socket_paths() -> anyhow::Result<()> {
    let cwd = TempDir::new().expect("temp cwd");
    let file_system_policy = FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(
        &SandboxPolicy::new_read_only_policy(),
        cwd.path(),
    );
    let network_socket = "/tmp/codex-proxy-use";
    let explicit_socket = "/tmp/codex-browser-use";
    let mut network_config = NetworkProxyConfig {
        enabled: true,
        mode: NetworkMode::Full,
        ..Default::default()
    };
    network_config.set_allow_unix_sockets(vec![network_socket.to_string()]);
    let state = build_config_state(network_config, NetworkProxyConstraints::default())?;
    let network_proxy = NetworkProxy::builder()
        .state(Arc::new(NetworkProxyState::with_reloader(
            state,
            Arc::new(TestConfigReloader),
        )))
        .managed_by_codex(/*managed_by_codex*/ false)
        .build()
        .await?;
    let extra_allow_unix_sockets = vec![absolute_path(explicit_socket)];

    let args = create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
        command: vec!["/usr/bin/true".to_string()],
        file_system_sandbox_policy: &file_system_policy,
        network_sandbox_policy: NetworkSandboxPolicy::Restricted,
        sandbox_policy_cwd: cwd.path(),
        enforce_managed_network: false,
        managed_network: None,
        environment_id: None,
        network: Some(&network_proxy),
        extra_allow_unix_sockets: &extra_allow_unix_sockets,
    })
    .unwrap();

    let expected_explicit_socket = normalize_path_for_sandbox(Path::new(explicit_socket))
        .expect("explicit socket root should normalize");
    let expected_network_socket = normalize_path_for_sandbox(Path::new(network_socket))
        .expect("network socket root should normalize");
    let unix_socket_definitions = args
        .iter()
        .filter(|arg| arg.starts_with("-DUNIX_SOCKET_PATH_"))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        unix_socket_definitions,
        vec![
            format!(
                "-DUNIX_SOCKET_PATH_0={}",
                expected_explicit_socket.display()
            ),
            format!("-DUNIX_SOCKET_PATH_1={}", expected_network_socket.display()),
        ],
        "seatbelt args should include both explicit and network proxy socket roots: {args:?}"
    );
    Ok(())
}

#[test]
fn create_seatbelt_args_preserves_full_network_with_explicit_unix_socket_paths() {
    let cwd = TempDir::new().expect("temp cwd");
    let file_system_policy = FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(
        &SandboxPolicy::new_read_only_policy(),
        cwd.path(),
    );
    let extra_allow_unix_sockets = vec![absolute_path("/tmp/codex-browser-use")];
    let args = create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
        command: vec!["/usr/bin/true".to_string()],
        file_system_sandbox_policy: &file_system_policy,
        network_sandbox_policy: NetworkSandboxPolicy::Enabled,
        sandbox_policy_cwd: cwd.path(),
        enforce_managed_network: false,
        managed_network: None,
        environment_id: None,
        network: None,
        extra_allow_unix_sockets: &extra_allow_unix_sockets,
    })
    .unwrap();
    let policy = seatbelt_policy_arg(&args);

    assert!(
        policy.contains("(allow network-outbound)\n"),
        "policy should preserve full outbound network access:\n{policy}"
    );
    assert!(
        policy.contains("(allow network-inbound)\n"),
        "policy should preserve full inbound network access:\n{policy}"
    );
    assert!(
        policy.contains(
            "(allow network-outbound (remote unix-socket (subpath (param \"UNIX_SOCKET_PATH_0\"))))"
        ),
        "policy should still allow outbound AF_UNIX traffic for explicit socket paths:\n{policy}"
    );
}

#[test]
fn unix_socket_policy_non_empty_output_is_newline_terminated() {
    let allowlist_policy = unix_socket_policy(&ProxyPolicyInputs {
        unix_domain_socket_policy: UnixDomainSocketPolicy::Restricted {
            allowed: vec![absolute_path("/tmp/example.sock")],
        },
        ..ProxyPolicyInputs::default()
    });
    assert!(
        allowlist_policy.ends_with('\n'),
        "allowlist unix socket policy should end with a newline:\n{allowlist_policy}"
    );

    let allow_all_policy = unix_socket_policy(&ProxyPolicyInputs {
        unix_domain_socket_policy: UnixDomainSocketPolicy::AllowAll,
        ..ProxyPolicyInputs::default()
    });
    assert!(
        allow_all_policy.ends_with('\n'),
        "allow-all unix socket policy should end with a newline:\n{allow_all_policy}"
    );
}

#[test]
fn unix_socket_dir_params_use_stable_param_names() {
    let params = unix_socket_dir_params(&ProxyPolicyInputs {
        unix_domain_socket_policy: UnixDomainSocketPolicy::Restricted {
            allowed: vec![
                absolute_path("/tmp/b.sock"),
                absolute_path("/tmp/a.sock"),
                absolute_path("/tmp/a.sock"),
            ],
        },
        ..ProxyPolicyInputs::default()
    });

    assert_eq!(
        params,
        vec![
            (
                "UNIX_SOCKET_PATH_0".to_string(),
                PathBuf::from("/tmp/a.sock")
            ),
            (
                "UNIX_SOCKET_PATH_1".to_string(),
                PathBuf::from("/tmp/b.sock")
            ),
        ]
    );
}

#[test]
fn normalize_path_for_sandbox_rejects_relative_paths() {
    assert_eq!(normalize_path_for_sandbox(Path::new("relative.sock")), None);
}

#[test]
fn create_seatbelt_args_allows_all_unix_sockets_when_enabled() {
    let policy = dynamic_network_policy(
        &SandboxPolicy::new_read_only_policy(),
        /*enforce_managed_network*/ false,
        &ProxyPolicyInputs {
            ports: vec![43128],
            has_proxy_config: true,
            allow_local_binding: false,
            unix_domain_socket_policy: UnixDomainSocketPolicy::AllowAll,
        },
    );

    assert!(
        policy.contains("(allow system-socket (socket-domain AF_UNIX))"),
        "policy should allow AF_UNIX socket creation when unix sockets are enabled:\n{policy}"
    );
    assert!(
        policy.contains("(allow network-bind (local unix-socket))"),
        "policy should allow binding unix sockets when enabled:\n{policy}"
    );
    assert!(
        policy.contains("(allow network-outbound (remote unix-socket))"),
        "policy should allow connecting to unix sockets when enabled:\n{policy}"
    );
    assert!(
        !policy.contains("(allow network* (subpath"),
        "policy should no longer use the generic subpath unix-socket rules:\n{policy}"
    );
}

#[test]
fn create_seatbelt_args_full_network_with_proxy_is_still_proxy_only() {
    let policy = dynamic_network_policy(
        &SandboxPolicy::WorkspaceWrite {
            writable_roots: vec![],
            network_access: true,
            exclude_tmpdir_env_var: true,
            exclude_slash_tmp: true,
        },
        /*enforce_managed_network*/ false,
        &ProxyPolicyInputs {
            ports: vec![43128],
            has_proxy_config: true,
            allow_local_binding: false,
            ..ProxyPolicyInputs::default()
        },
    );

    assert!(
        policy.contains("(allow network-outbound (remote ip \"localhost:43128\"))"),
        "expected proxy endpoint allow rule in policy:\n{policy}"
    );
    assert!(
        !policy.contains("\n(allow network-outbound)\n"),
        "policy should not include blanket outbound allowance when proxy is configured:\n{policy}"
    );
    assert!(
        !policy.contains("\n(allow network-inbound)\n"),
        "policy should not include blanket inbound allowance when proxy is configured:\n{policy}"
    );
}

#[test]
fn create_seatbelt_args_with_read_only_git_and_codex_subpaths() {
    // Create a temporary workspace with two writable roots: one containing
    // top-level workspace metadata paths and one without them.
    let tmp = TempDir::new().expect("tempdir");
    let PopulatedTmp {
        vulnerable_root,
        vulnerable_root_canonical,
        dot_git_canonical,
        dot_agents_canonical: _,
        dot_codex_canonical,
        empty_root,
        empty_root_canonical,
    } = populate_tmpdir(tmp.path());
    let cwd = tmp.path().join("cwd");
    fs::create_dir_all(&cwd).expect("create cwd");

    // Build a policy that only includes the two test roots as writable and
    // does not automatically include defaults TMPDIR or /tmp.
    let policy = SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![vulnerable_root, empty_root]
            .into_iter()
            .map(|p| p.try_into().unwrap())
            .collect(),
        network_access: false,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: true,
    };

    // Create the Seatbelt command to wrap a shell command that tries to
    // write to .codex/config.toml in the vulnerable root.
    let shell_command: Vec<String> = [
        "bash",
        "-c",
        "echo 'sandbox_mode = \"danger-full-access\"' > \"$1\"",
        "bash",
        dot_codex_canonical
            .join("config.toml")
            .to_string_lossy()
            .as_ref(),
    ]
    .iter()
    .map(std::string::ToString::to_string)
    .collect();
    let args = create_seatbelt_command_args_for_legacy_policy(
        shell_command.clone(),
        &policy,
        &cwd,
        /*enforce_managed_network*/ false,
        /*network*/ None,
    )
    .unwrap();

    let policy_text = seatbelt_policy_arg(&args);
    assert!(
        policy_text.contains("(require-all (subpath (param \"WRITABLE_ROOT_0\"))"),
        "expected cwd writable root to carry protected carveouts:\n{policy_text}",
    );
    assert!(
        policy_text.contains("WRITABLE_ROOT_0_EXCLUDED_0"),
        "expected cwd metadata carveouts in policy:\n{policy_text}",
    );
    assert!(
        policy_text.contains("WRITABLE_ROOT_0_EXCLUDED_1")
            && policy_text.contains("WRITABLE_ROOT_0_EXCLUDED_2"),
        "expected symbolic cwd .git/.agents carveouts in policy:\n{policy_text}",
    );
    assert!(
        policy_text.contains("WRITABLE_ROOT_1_EXCLUDED_0")
            && policy_text.contains("WRITABLE_ROOT_1_EXCLUDED_1"),
        "expected explicit writable root .git/.codex carveouts in policy:\n{policy_text}",
    );
    assert!(
        policy_text.contains(&seatbelt_protected_metadata_name_requirements(
            &cwd.canonicalize().expect("canonicalize cwd")
        )),
        "expected cwd metadata protection regex requirements in policy:\n{policy_text}",
    );
    assert!(
        policy_text.contains(&seatbelt_protected_metadata_name_requirements(
            &vulnerable_root_canonical
        )),
        "expected populated root metadata protection regex requirements in policy:\n{policy_text}",
    );
    assert!(
        policy_text.contains(&seatbelt_protected_metadata_name_requirements(
            &empty_root_canonical
        )),
        "expected empty root metadata protection regex requirements in policy:\n{policy_text}",
    );

    let expected_definitions = [
        format!(
            "-DWRITABLE_ROOT_0={}",
            cwd.canonicalize()
                .expect("canonicalize cwd")
                .to_string_lossy()
        ),
        format!(
            "-DWRITABLE_ROOT_0_EXCLUDED_0={}",
            cwd.canonicalize()
                .expect("canonicalize cwd")
                .join(".codex")
                .display()
        ),
        format!(
            "-DWRITABLE_ROOT_0_EXCLUDED_1={}",
            cwd.canonicalize()
                .expect("canonicalize cwd")
                .join(".git")
                .display()
        ),
        format!(
            "-DWRITABLE_ROOT_0_EXCLUDED_2={}",
            cwd.canonicalize()
                .expect("canonicalize cwd")
                .join(".agents")
                .display()
        ),
        format!(
            "-DWRITABLE_ROOT_1={}",
            vulnerable_root_canonical.to_string_lossy()
        ),
        format!(
            "-DWRITABLE_ROOT_1_EXCLUDED_0={}",
            dot_git_canonical.to_string_lossy()
        ),
        format!(
            "-DWRITABLE_ROOT_1_EXCLUDED_1={}",
            dot_codex_canonical.to_string_lossy()
        ),
        format!(
            "-DWRITABLE_ROOT_2={}",
            empty_root_canonical.to_string_lossy()
        ),
    ];
    let writable_definitions: Vec<String> = args
        .iter()
        .filter(|arg| arg.starts_with("-DWRITABLE_ROOT_"))
        .cloned()
        .collect();
    assert_eq!(
        writable_definitions, expected_definitions,
        "unexpected writable-root parameter definitions in {args:#?}"
    );
    let command_index = args
        .iter()
        .position(|arg| arg == "--")
        .expect("seatbelt args should include command separator");
    assert_eq!(args[command_index + 1..], shell_command);

    // Verify that .codex/config.toml cannot be modified under the generated
    // Seatbelt policy.
    let config_toml = dot_codex_canonical.join("config.toml");
    let output = Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
        .args(&args)
        .current_dir(&cwd)
        .output()
        .expect("execute seatbelt command");
    assert_eq!(
        "sandbox_mode = \"read-only\"\n",
        String::from_utf8_lossy(&fs::read(&config_toml).expect("read config.toml")),
        "config.toml should contain its original contents because it should not have been modified"
    );
    assert!(
        !output.status.success(),
        "command to write {} should fail under seatbelt",
        config_toml.display()
    );
    assert_seatbelt_denied(&output.stderr, &config_toml);

    // Create a similar Seatbelt command that tries to write to a file in
    // the .git folder, which should also be blocked.
    let pre_commit_hook = dot_git_canonical.join("hooks").join("pre-commit");
    let shell_command_git: Vec<String> = [
        "bash",
        "-c",
        "echo 'pwned!' > \"$1\"",
        "bash",
        pre_commit_hook.to_string_lossy().as_ref(),
    ]
    .iter()
    .map(std::string::ToString::to_string)
    .collect();
    let write_hooks_file_args = create_seatbelt_command_args_for_legacy_policy(
        shell_command_git,
        &policy,
        &cwd,
        /*enforce_managed_network*/ false,
        /*network*/ None,
    )
    .unwrap();
    let output = Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
        .args(&write_hooks_file_args)
        .current_dir(&cwd)
        .output()
        .expect("execute seatbelt command");
    assert!(
        !fs::exists(&pre_commit_hook).expect("exists pre-commit hook"),
        "{} should not exist because it should not have been created",
        pre_commit_hook.display()
    );
    assert!(
        !output.status.success(),
        "command to write {} should fail under seatbelt",
        pre_commit_hook.display()
    );
    assert_seatbelt_denied(&output.stderr, &pre_commit_hook);

    // Verify that writing a file to the folder containing .git and .codex is allowed.
    let allowed_file = vulnerable_root_canonical.join("allowed.txt");
    let shell_command_allowed: Vec<String> = [
        "bash",
        "-c",
        "echo 'this is allowed' > \"$1\"",
        "bash",
        allowed_file.to_string_lossy().as_ref(),
    ]
    .iter()
    .map(std::string::ToString::to_string)
    .collect();
    let write_allowed_file_args = create_seatbelt_command_args_for_legacy_policy(
        shell_command_allowed,
        &policy,
        &cwd,
        /*enforce_managed_network*/ false,
        /*network*/ None,
    )
    .unwrap();
    let output = Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
        .args(&write_allowed_file_args)
        .current_dir(&cwd)
        .output()
        .expect("execute seatbelt command");
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success()
        && stderr.contains("sandbox-exec: sandbox_apply: Operation not permitted")
    {
        return;
    }
    assert!(
        output.status.success(),
        "command to write {} should succeed under seatbelt",
        allowed_file.display()
    );
    assert_eq!(
        "this is allowed\n",
        String::from_utf8_lossy(&fs::read(&allowed_file).expect("read allowed.txt")),
        "{} should contain the written text",
        allowed_file.display()
    );
}

#[test]
fn create_seatbelt_args_block_first_time_dot_codex_creation_with_metadata_name_regex() {
    let tmp = TempDir::new().expect("tempdir");
    let repo_root = tmp.path().join("repo");
    fs::create_dir_all(&repo_root).expect("create repo root");

    Command::new("git")
        .arg("init")
        .arg(".")
        .current_dir(&repo_root)
        .output()
        .expect("git init .");

    let dot_codex = repo_root.join(".codex");
    let config_toml = dot_codex.join("config.toml");
    let policy = SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![repo_root.as_path().try_into().expect("absolute repo root")],
        network_access: false,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: true,
    };

    let shell_command: Vec<String> = [
        "bash",
        "-c",
        "mkdir -p \"$1\" && echo 'sandbox_mode = \"danger-full-access\"' > \"$2\"",
        "bash",
        dot_codex.to_string_lossy().as_ref(),
        config_toml.to_string_lossy().as_ref(),
    ]
    .iter()
    .map(std::string::ToString::to_string)
    .collect();
    let args = create_seatbelt_command_args_for_legacy_policy(
        shell_command,
        &policy,
        repo_root.as_path(),
        /*enforce_managed_network*/ false,
        /*network*/ None,
    )
    .unwrap();

    let policy_text = seatbelt_policy_arg(&args);
    assert!(
        policy_text.contains(&seatbelt_protected_metadata_name_requirements(
            &repo_root.canonicalize().expect("canonicalize repo root")
        )),
        "expected metadata protection regex requirements in policy:\n{policy_text}"
    );
}

#[test]
fn create_seatbelt_args_with_read_only_git_pointer_file() {
    let tmp = TempDir::new().expect("tempdir");
    let worktree_root = tmp.path().join("worktree_root");
    fs::create_dir_all(&worktree_root).expect("create worktree_root");
    let gitdir = worktree_root.join("actual-gitdir");
    fs::create_dir_all(&gitdir).expect("create gitdir");
    let gitdir_config = gitdir.join("config");
    let gitdir_config_contents = "[core]\n";
    fs::write(&gitdir_config, gitdir_config_contents).expect("write gitdir config");

    let dot_git = worktree_root.join(".git");
    let dot_git_contents = format!("gitdir: {}\n", gitdir.to_string_lossy());
    fs::write(&dot_git, &dot_git_contents).expect("write .git pointer");

    let cwd = tmp.path().join("cwd");
    fs::create_dir_all(&cwd).expect("create cwd");

    let policy = SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![worktree_root.try_into().expect("worktree_root is absolute")],
        network_access: false,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: true,
    };

    let shell_command: Vec<String> = [
        "bash",
        "-c",
        "echo 'pwned!' > \"$1\"",
        "bash",
        dot_git.to_string_lossy().as_ref(),
    ]
    .iter()
    .map(std::string::ToString::to_string)
    .collect();
    let args = create_seatbelt_command_args_for_legacy_policy(
        shell_command,
        &policy,
        &cwd,
        /*enforce_managed_network*/ false,
        /*network*/ None,
    )
    .unwrap();

    let output = Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
        .args(&args)
        .current_dir(&cwd)
        .output()
        .expect("execute seatbelt command");

    assert_eq!(
        dot_git_contents,
        String::from_utf8_lossy(&fs::read(&dot_git).expect("read .git pointer")),
        ".git pointer file should not be modified under seatbelt"
    );
    assert!(
        !output.status.success(),
        "command to write {} should fail under seatbelt",
        dot_git.display()
    );
    assert_seatbelt_denied(&output.stderr, &dot_git);

    let shell_command_gitdir: Vec<String> = [
        "bash",
        "-c",
        "echo 'pwned!' > \"$1\"",
        "bash",
        gitdir_config.to_string_lossy().as_ref(),
    ]
    .iter()
    .map(std::string::ToString::to_string)
    .collect();
    let gitdir_args = create_seatbelt_command_args_for_legacy_policy(
        shell_command_gitdir,
        &policy,
        &cwd,
        /*enforce_managed_network*/ false,
        /*network*/ None,
    )
    .unwrap();
    let output = Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
        .args(&gitdir_args)
        .current_dir(&cwd)
        .output()
        .expect("execute seatbelt command");

    assert_eq!(
        gitdir_config_contents,
        String::from_utf8_lossy(&fs::read(&gitdir_config).expect("read gitdir config")),
        "gitdir config should contain its original contents because it should not have been modified"
    );
    assert!(
        !output.status.success(),
        "command to write {} should fail under seatbelt",
        gitdir_config.display()
    );
    assert_seatbelt_denied(&output.stderr, &gitdir_config);
}

#[test]
fn create_seatbelt_args_for_cwd_as_git_repo() {
    // Create a temporary workspace with two writable roots: one containing
    // top-level workspace metadata paths and one without them.
    let tmp = TempDir::new().expect("tempdir");
    let PopulatedTmp {
        vulnerable_root,
        vulnerable_root_canonical,
        dot_git_canonical,
        dot_agents_canonical,
        dot_codex_canonical,
        ..
    } = populate_tmpdir(tmp.path());

    // Build a policy that does not specify any writable_roots, but does
    // use the default ones (cwd and TMPDIR) and verifies the protected
    // metadata checks are done properly for cwd.
    let policy = SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![],
        network_access: false,
        exclude_tmpdir_env_var: false,
        exclude_slash_tmp: false,
    };

    let shell_command: Vec<String> = [
        "bash",
        "-c",
        "echo 'sandbox_mode = \"danger-full-access\"' > \"$1\"",
        "bash",
        dot_codex_canonical
            .join("config.toml")
            .to_string_lossy()
            .as_ref(),
    ]
    .iter()
    .map(std::string::ToString::to_string)
    .collect();
    let args = create_seatbelt_command_args_for_legacy_policy(
        shell_command.clone(),
        &policy,
        vulnerable_root.as_path(),
        /*enforce_managed_network*/ false,
        /*network*/ None,
    )
    .unwrap();

    let slash_tmp = PathBuf::from("/tmp")
        .canonicalize()
        .expect("canonicalize /tmp");
    let policy_text = seatbelt_policy_arg(&args);
    assert!(
        policy_text.contains(&seatbelt_protected_metadata_name_requirements(
            &vulnerable_root_canonical
        )),
        "expected cwd metadata protection regex requirements in policy:\n{policy_text}",
    );
    assert!(
        policy_text.contains(&seatbelt_protected_metadata_name_requirements(&slash_tmp)),
        "expected /tmp metadata protection regex requirements in policy:\n{policy_text}",
    );
    if let Some(tmpdir_env_var) = std::env::var("TMPDIR")
        .ok()
        .map(PathBuf::from)
        .and_then(|p| p.canonicalize().ok())
    {
        assert!(
            policy_text.contains(&seatbelt_protected_metadata_name_requirements(
                &tmpdir_env_var
            )),
            "expected TMPDIR metadata protection regex requirements in policy:\n{policy_text}",
        );
    }

    let expected_root = format!(
        "-DWRITABLE_ROOT_0={}",
        vulnerable_root_canonical.to_string_lossy()
    );
    assert!(
        args.contains(&expected_root),
        "missing {expected_root}: {args:#?}"
    );
    let expected_dot_git = format!(
        "-DWRITABLE_ROOT_0_EXCLUDED_0={}",
        dot_git_canonical.to_string_lossy()
    );
    assert!(
        args.contains(&expected_dot_git),
        "missing {expected_dot_git}: {args:#?}"
    );
    let expected_dot_codex = format!(
        "-DWRITABLE_ROOT_0_EXCLUDED_1={}",
        dot_codex_canonical.to_string_lossy()
    );
    assert!(
        args.contains(&expected_dot_codex),
        "missing {expected_dot_codex}: {args:#?}"
    );
    let unexpected_dot_agents = format!(
        "-DWRITABLE_ROOT_0_EXCLUDED_1={}",
        dot_agents_canonical.to_string_lossy()
    );
    assert!(
        !args.contains(&unexpected_dot_agents),
        "missing .agents should be handled by regex rather than materialized as a path param: {args:#?}"
    );
    let expected_slash_tmp = format!("-DWRITABLE_ROOT_1={}", slash_tmp.to_string_lossy());
    assert!(
        args.contains(&expected_slash_tmp),
        "missing {expected_slash_tmp}: {args:#?}"
    );
    let command_index = args
        .iter()
        .position(|arg| arg == "--")
        .expect("seatbelt args should include command separator");
    assert_eq!(args[command_index + 1..], shell_command);
}

struct PopulatedTmp {
    /// Path containing protected metadata subfolders.
    /// For the purposes of this test, we consider this a "vulnerable" root
    /// because a bad actor could write to .git/hooks/pre-commit so an
    /// unsuspecting user would run code as privileged the next time they
    /// ran `git commit` themselves, or modified .codex/config.toml to
    /// contain `sandbox_mode = "danger-full-access"` so the agent would
    /// have full privileges the next time it ran in that repo.
    vulnerable_root: PathBuf,
    vulnerable_root_canonical: PathBuf,
    dot_git_canonical: PathBuf,
    dot_agents_canonical: PathBuf,
    dot_codex_canonical: PathBuf,

    /// Path without protected metadata subfolders.
    empty_root: PathBuf,
    /// Canonicalized version of `empty_root`.
    empty_root_canonical: PathBuf,
}

fn populate_tmpdir(tmp: &Path) -> PopulatedTmp {
    let vulnerable_root = tmp.join("vulnerable_root");
    fs::create_dir_all(&vulnerable_root).expect("create vulnerable_root");

    // TODO(mbolin): Should also support the case where `.git` is a file
    // with a gitdir: ... line.
    Command::new("git")
        .arg("init")
        .arg(".")
        .current_dir(&vulnerable_root)
        .output()
        .expect("git init .");

    fs::create_dir_all(vulnerable_root.join(".codex")).expect("create .codex");
    fs::write(
        vulnerable_root.join(".codex").join("config.toml"),
        "sandbox_mode = \"read-only\"\n",
    )
    .expect("write .codex/config.toml");

    let empty_root = tmp.join("empty_root");
    fs::create_dir_all(&empty_root).expect("create empty_root");

    // Ensure we have canonical paths for -D parameter matching.
    let vulnerable_root_canonical = vulnerable_root
        .canonicalize()
        .expect("canonicalize vulnerable_root");
    let dot_git_canonical = vulnerable_root_canonical.join(".git");
    let dot_agents_canonical = vulnerable_root_canonical.join(".agents");
    let dot_codex_canonical = vulnerable_root_canonical.join(".codex");
    let empty_root_canonical = empty_root.canonicalize().expect("canonicalize empty_root");
    PopulatedTmp {
        vulnerable_root,
        vulnerable_root_canonical,
        dot_git_canonical,
        dot_agents_canonical,
        dot_codex_canonical,
        empty_root,
        empty_root_canonical,
    }
}

/// PF-33-S02: under the destination guard (`url_destination_policy`) the
/// macOS sandbox may reach only the proxy's loopback ports. Local binding
/// (every loopback port, raw DNS) and Unix-socket grants are dropped, so the
/// loopback exemption cannot become a route around the proxy.
#[tokio::test]
async fn pf_33_s02_guard_limits_seatbelt_egress_to_proxy_ports() -> anyhow::Result<()> {
    let cwd = TempDir::new().expect("temp cwd");
    let file_system_policy = FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(
        &SandboxPolicy::new_read_only_policy(),
        cwd.path(),
    );
    let mut policies = Vec::new();
    for guarded in [false, true] {
        let mut network_config = NetworkProxyConfig {
            enabled: true,
            mode: NetworkMode::Full,
            allow_local_binding: true,
            ..Default::default()
        };
        network_config.set_allow_unix_sockets(vec!["/tmp/pf-33-s02-docker.sock".to_string()]);
        network_config.set_url_destination_policy(guarded);
        let state = build_config_state(network_config, NetworkProxyConstraints::default())?;
        let network_proxy = NetworkProxy::builder()
            .state(Arc::new(NetworkProxyState::with_reloader(
                state,
                Arc::new(TestConfigReloader),
            )))
            .managed_by_codex(/*managed_by_codex*/ false)
            .build()
            .await?;
        let args = create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
            command: vec!["/usr/bin/true".to_string()],
            file_system_sandbox_policy: &file_system_policy,
            network_sandbox_policy: NetworkSandboxPolicy::Restricted,
            sandbox_policy_cwd: cwd.path(),
            enforce_managed_network: true,
            managed_network: None,
            environment_id: None,
            network: Some(&network_proxy),
            extra_allow_unix_sockets: &[],
        })
        .unwrap();
        policies.push(args.join("\n"));
    }
    let [open, guarded] = [&policies[0], &policies[1]];

    for rule in [
        "(allow network-outbound (remote ip \"localhost:*\"))",
        "(allow network-outbound (remote ip \"*:53\"))",
        "UNIX_SOCKET_PATH_0",
    ] {
        assert!(open.contains(rule), "flag off keeps {rule}:\n{open}");
        assert!(!guarded.contains(rule), "guard drops {rule}:\n{guarded}");
    }
    assert!(
        guarded.contains("(allow network-outbound (remote ip \"localhost:"),
        "guard keeps the proxy port rules:\n{guarded}"
    );
    Ok(())
}

/// #583: a writable root inside a read-only folder (the Corbanu home's
/// `worktrees`) cannot be renamed or removed, so no link can take its place
/// before the next command; writing inside it still works.
#[test]
fn a_writable_root_inside_a_read_only_folder_cannot_be_moved() {
    let dir = TempDir::new().expect("tempdir");
    let base = dir.path().canonicalize().expect("canonical");
    let home = base.join("home");
    fs::create_dir_all(home.join("worktrees")).expect("worktrees");
    let entry = |path: &Path, access| FileSystemSandboxEntry {
        path: FileSystemPath::Path {
            path: AbsolutePathBuf::from_absolute_path(path).expect("absolute"),
        },
        access,
        missing_path_behavior: None,
    };
    // A workspace covering the home, as `ReadDenials::apply` builds it.
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Read,
            missing_path_behavior: None,
        },
        entry(&base, FileSystemAccessMode::Write),
        entry(&home.join("auth.json"), FileSystemAccessMode::Deny),
        entry(&home, FileSystemAccessMode::Read),
        entry(&home.join("worktrees"), FileSystemAccessMode::Write),
    ]);
    let script = "mv home/worktrees wt; echo mv=$?; rmdir home/worktrees; echo rmdir=$?; \
                  echo ok > home/worktrees/ok && echo wrote";
    let args = create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
        command: vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
        file_system_sandbox_policy: &policy,
        network_sandbox_policy: NetworkSandboxPolicy::Restricted,
        sandbox_policy_cwd: &base,
        enforce_managed_network: false,
        managed_network: None,
        environment_id: None,
        network: None,
        extra_allow_unix_sockets: &[],
    })
    .expect("args");
    // Guarded explicitly, whatever its protected names do.
    let worktrees = format!("={}", home.join("worktrees").display());
    assert!(
        args.iter()
            .any(|arg| arg.starts_with("-DRENAME_GUARD_") && arg.ends_with(&worktrees)),
        "{args:?}"
    );
    let output = Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
        .args(&args)
        .current_dir(&base)
        .output()
        .expect("run seatbelt");
    let stdout = String::from_utf8_lossy(&output.stdout);
    if String::from_utf8_lossy(&output.stderr)
        .contains("sandbox-exec: sandbox_apply: Operation not permitted")
    {
        return;
    }
    for held in ["mv=1", "rmdir=1", "wrote"] {
        assert!(stdout.contains(held), "{held}: {stdout}");
    }
    assert!(home.join("worktrees").is_dir());
    assert!(!base.join("wt").exists());
}

/// PF-23-S02: once a policy denies reads, a protected path's folders cannot
/// be renamed (which would move it out from under its rule), and a
/// protected symlink cannot be removed; writing elsewhere in those folders
/// still works.
#[test]
fn renaming_a_folder_above_a_protected_path_is_denied() {
    let dir = TempDir::new().expect("tempdir");
    let base = dir.path().canonicalize().expect("canonical");
    let config = base.join("config");
    fs::create_dir_all(config.join("fish")).expect("fish");
    fs::write(config.join("fish/config.fish"), "ok\n").expect("config");
    fs::create_dir_all(base.join("dotfiles")).expect("dotfiles");
    fs::write(base.join("dotfiles/zshrc"), "ok\n").expect("zshrc");
    std::os::unix::fs::symlink(base.join("dotfiles/zshrc"), base.join(".zshrc")).expect("link");
    let entry = |path: &Path, access| FileSystemSandboxEntry {
        path: FileSystemPath::Path {
            path: AbsolutePathBuf::from_absolute_path(path).expect("absolute"),
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
        entry(&base.join("secret"), FileSystemAccessMode::Deny),
        entry(&config.join("fish"), FileSystemAccessMode::Read),
        entry(&base.join(".zshrc"), FileSystemAccessMode::Read),
        // Its parent `local` does not exist yet.
        entry(&base.join("local/bin"), FileSystemAccessMode::Read),
        // Its parent `empty` exists and is empty.
        entry(&base.join("empty/git"), FileSystemAccessMode::Read),
    ]);
    fs::create_dir_all(base.join("empty")).expect("empty");
    let script = "mv config moved; echo mv=$?; rm .zshrc; echo rm=$?; \
                  echo new > config/other.txt && echo wrote-other; \
                  mkdir -p prep/bin && echo x > prep/bin/tool; \
                  mv prep local; echo mv-missing=$?; mkdir local; echo mkdir-missing=$?; \
                  mkdir -p prep2/git && echo x > prep2/git/config; \
                  perl -e 'rename(\"prep2\", \"empty\") or exit 1'; echo mv-over-empty=$?";
    let args = create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
        command: vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
        file_system_sandbox_policy: &policy,
        network_sandbox_policy: NetworkSandboxPolicy::Restricted,
        sandbox_policy_cwd: &base,
        enforce_managed_network: false,
        managed_network: None,
        environment_id: None,
        network: None,
        extra_allow_unix_sockets: &[],
    })
    .expect("args");
    let output = Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
        .args(&args)
        .current_dir(&base)
        .output()
        .expect("run seatbelt");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("mv=1"), "{stdout}");
    assert!(stdout.contains("rm=1"), "{stdout}");
    assert!(stdout.contains("wrote-other"), "{stdout}");
    assert!(stdout.contains("mv-missing=1"), "{stdout}");
    assert!(stdout.contains("mkdir-missing=1"), "{stdout}");
    assert!(stdout.contains("mv-over-empty=1"), "{stdout}");
    assert!(!base.join("local").exists());
    assert!(!base.join("empty/git").exists());
    assert!(config.join("fish/config.fish").exists());
    assert!(base.join(".zshrc").is_symlink());
}

/// #493 (Linux) has no Seatbelt equivalent: under a full-write profile with a
/// denial, `/dev/null` stays writable and the denied file stays unreadable.
#[test]
fn full_write_profile_with_denial_keeps_dev_null_usable() {
    let dir = TempDir::new().expect("tempdir");
    let base = dir.path().canonicalize().expect("canonical");
    let secret = base.join("secret.txt");
    fs::write(&secret, "secret-493\n").expect("secret");
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
                path: AbsolutePathBuf::from_absolute_path(&secret).expect("absolute"),
            },
            access: FileSystemAccessMode::Deny,
            missing_path_behavior: None,
        },
    ]);
    let script = "echo hi > /dev/null; echo null=$?; cat secret.txt; echo cat=$?";
    let args = create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
        command: vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
        file_system_sandbox_policy: &policy,
        network_sandbox_policy: NetworkSandboxPolicy::Restricted,
        sandbox_policy_cwd: &base,
        enforce_managed_network: false,
        managed_network: None,
        environment_id: None,
        network: None,
        extra_allow_unix_sockets: &[],
    })
    .expect("args");
    let output = Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
        .args(&args)
        .current_dir(&base)
        .output()
        .expect("run seatbelt");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("null=0"), "{stdout}");
    assert!(stdout.contains("cat=1"), "{stdout}");
    assert!(!stdout.contains("secret-493"), "{stdout}");
}

// #488: denials are attached to every root that reaches them, in the
// spellings Seatbelt matches.

/// The spelling Seatbelt checks: the nearest existing ancestor resolved.
/// Written independently of the code under test.
fn seen_by_seatbelt(path: &Path) -> PathBuf {
    match path.canonicalize() {
        Ok(path) => path,
        Err(_) => match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => seen_by_seatbelt(parent).join(name),
            _ => path.to_path_buf(),
        },
    }
}

/// The Aggressive shape: read everywhere, write the workspace and deny the
/// given paths.
fn workspace_policy_denying(denied: &[PathBuf]) -> FileSystemSandboxPolicy {
    let mut entries = vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            access: FileSystemAccessMode::Read,
            missing_path_behavior: None,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::SlashTmp,
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
    ];
    entries.extend(denied.iter().map(|path| FileSystemSandboxEntry {
        path: FileSystemPath::Path {
            path: AbsolutePathBuf::from_absolute_path(path).expect("absolute"),
        },
        access: FileSystemAccessMode::Deny,
        missing_path_behavior: None,
    }));
    FileSystemSandboxPolicy::restricted(entries)
}

fn seatbelt_args(
    policy: &FileSystemSandboxPolicy,
    cwd: &Path,
    command: Vec<String>,
) -> Vec<String> {
    create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
        command,
        file_system_sandbox_policy: policy,
        network_sandbox_policy: NetworkSandboxPolicy::Restricted,
        sandbox_policy_cwd: cwd,
        enforce_managed_network: false,
        managed_network: None,
        environment_id: None,
        network: None,
        extra_allow_unix_sockets: &[],
    })
    .expect("seatbelt args")
}

/// The `prefix` clauses read back from generated arguments: each root with
/// its excluded paths.
fn generated_clauses(args: &[String], prefix: &str) -> Vec<(PathBuf, Vec<PathBuf>)> {
    let policy = seatbelt_policy_arg(args);
    let params: BTreeMap<&str, PathBuf> = args
        .iter()
        .filter_map(|arg| arg.strip_prefix("-D")?.split_once('='))
        .map(|(key, value)| (key, PathBuf::from(value)))
        .collect();
    (0..)
        .map_while(|index| {
            let key = format!("{prefix}_{index}");
            let root = params.get(key.as_str())?.clone();
            let excluded = (0..)
                .map_while(|excluded| {
                    let key = format!("{key}_EXCLUDED_{excluded}");
                    let path = params.get(key.as_str())?.clone();
                    assert!(
                        policy.contains(&format!("(require-not (subpath (param \"{key}\")))")),
                        "{key} is not applied:\n{policy}"
                    );
                    Some(path)
                })
                .collect();
            Some((root, excluded))
        })
        .collect()
}

fn clauses_allow(clauses: &[(PathBuf, Vec<PathBuf>)], path: &Path) -> bool {
    let path = seen_by_seatbelt(path);
    clauses.iter().any(|(root, excluded)| {
        path.starts_with(root) && !excluded.iter().any(|excluded| path.starts_with(excluded))
    })
}

struct LinkScenario {
    name: &'static str,
    cwd: PathBuf,
    denied: Vec<PathBuf>,
    /// Every spelling of the denied files, all of which must be denied.
    denied_spellings: Vec<PathBuf>,
    /// Files the policy allows reading and writing.
    allowed: Vec<PathBuf>,
}

/// The scenarios #488 asks for. Each `TempDir` must outlive its scenario.
fn link_scenarios(base: &Path, tmp: &Path) -> Vec<LinkScenario> {
    let real = base.join("real");
    let link = base.join("link");
    fs::create_dir_all(real.join("sub")).expect("real");
    std::os::unix::fs::symlink(&real, &link).expect("link");
    for dir in [&real, &tmp.to_path_buf()] {
        fs::write(dir.join(".env"), "API_KEY=x\n").expect(".env");
        fs::write(dir.join("other"), "ok\n").expect("other");
    }
    let physical_tmp = tmp.canonicalize().expect("canonical tmp");
    let both = |name: &str| vec![real.join(name), link.join(name)];
    let allowed = |dir: &Path| vec![dir.join("other"), dir.join("sub/new"), dir.join("new")];
    vec![
        LinkScenario {
            name: "plain workspace",
            cwd: real.clone(),
            denied: vec![real.join(".env")],
            denied_spellings: vec![real.join(".env")],
            allowed: allowed(&real),
        },
        LinkScenario {
            name: "/tmp workspace denied by its /private/tmp path",
            cwd: tmp.to_path_buf(),
            denied: vec![physical_tmp.join(".env")],
            denied_spellings: vec![tmp.join(".env"), physical_tmp.join(".env")],
            allowed: [allowed(tmp), allowed(&physical_tmp)].concat(),
        },
        LinkScenario {
            name: "/private/tmp workspace denied by its /tmp path",
            cwd: physical_tmp.clone(),
            denied: vec![tmp.join(".env")],
            denied_spellings: vec![tmp.join(".env"), physical_tmp.join(".env")],
            allowed: [allowed(tmp), allowed(&physical_tmp)].concat(),
        },
        LinkScenario {
            name: "nested link workspace",
            cwd: link.clone(),
            denied: vec![real.join(".env")],
            denied_spellings: both(".env"),
            allowed: [allowed(&real), allowed(&link)].concat(),
        },
        LinkScenario {
            name: "denial reached through a link",
            cwd: real.clone(),
            denied: vec![link.join(".env")],
            denied_spellings: both(".env"),
            allowed: [allowed(&real), allowed(&link)].concat(),
        },
        LinkScenario {
            name: "missing denied paths",
            cwd: link.clone(),
            denied: vec![real.join("later.key"), link.join("never.key")],
            denied_spellings: [both("later.key"), both("never.key")].concat(),
            allowed: [allowed(&real), allowed(&link)].concat(),
        },
    ]
}

#[test]
fn denials_hold_through_links_and_nothing_allowed_is_denied() {
    let base = TempDir::new().expect("tempdir");
    let base = base.path().canonicalize().expect("canonical");
    let tmp = tempfile::tempdir_in("/tmp").expect("tempdir in /tmp");
    assert!(tmp.path().starts_with("/tmp"), "{:?}", tmp.path());
    for scenario in link_scenarios(&base, tmp.path()) {
        let name = scenario.name;
        let policy = workspace_policy_denying(&scenario.denied);
        let args = seatbelt_args(&policy, &scenario.cwd, vec!["/bin/true".to_string()]);
        let read = generated_clauses(&args, "READABLE_ROOT");
        let write = generated_clauses(&args, "WRITABLE_ROOT");
        assert!(!read.is_empty() && !write.is_empty(), "{name}: {args:#?}");
        for path in &scenario.denied_spellings {
            assert!(!clauses_allow(&read, path), "{name}: {path:?} readable");
            assert!(!clauses_allow(&write, path), "{name}: {path:?} writable");
        }
        for path in &scenario.allowed {
            assert!(clauses_allow(&read, path), "{name}: {path:?} unreadable");
            assert!(clauses_allow(&write, path), "{name}: {path:?} unwritable");
        }
        assert!(clauses_allow(&read, Path::new("/usr/bin/true")), "{name}");
        // Only paths the policy already keeps from the sandbox are excluded:
        // denied paths, and for writes its read-only paths.
        let denied: Vec<PathBuf> = scenario
            .denied
            .iter()
            .map(|path| seen_by_seatbelt(path))
            .collect();
        let read_only: Vec<PathBuf> = policy
            .get_writable_roots_with_cwd(&scenario.cwd)
            .into_iter()
            .flat_map(|root| root.read_only_subpaths)
            .map(|path| seen_by_seatbelt(path.as_path()))
            .collect();
        for (_, excluded) in &read {
            for path in excluded {
                assert!(
                    denied.contains(&seen_by_seatbelt(path)),
                    "{name}: read exclusion {path:?} is not denied"
                );
            }
        }
        for (_, excluded) in &write {
            for path in excluded {
                let path = seen_by_seatbelt(path);
                assert!(
                    denied.contains(&path) || read_only.contains(&path),
                    "{name}: write exclusion {path:?} is not denied or read-only"
                );
            }
        }
        // The launch check's helper evaluates the same clauses.
        for path in scenario
            .denied_spellings
            .iter()
            .chain(&scenario.allowed)
            .chain([&PathBuf::from(".env"), &PathBuf::from("other")])
        {
            assert_eq!(
                super::seatbelt_allows_file_read(&policy, &scenario.cwd, path),
                clauses_allow(&read, &scenario.cwd.join(path)),
                "{name}: {path:?}"
            );
        }
    }
}

/// A more specific allow under a denied folder keeps its own clause, also
/// when the two are spelled through different sides of a link; a denied
/// link to a folder denies that folder's clause, as the denial requires.
#[test]
fn allows_inside_denied_folders_survive_across_links() {
    let base = TempDir::new().expect("tempdir");
    let base = base.path().canonicalize().expect("canonical");
    let real = base.join("real");
    let link = base.join("link");
    fs::create_dir_all(real.join("secret/public")).expect("dirs");
    fs::write(real.join("secret/key"), "x").expect("key");
    fs::write(real.join("secret/public/notes"), "ok").expect("notes");
    std::os::unix::fs::symlink(&real, &link).expect("link");
    let entry = |path: PathBuf, access| FileSystemSandboxEntry {
        path: FileSystemPath::Path {
            path: AbsolutePathBuf::from_absolute_path(path).expect("absolute"),
        },
        access,
        missing_path_behavior: None,
    };
    for (denied, public) in [
        (real.join("secret"), link.join("secret/public")),
        (link.join("secret"), real.join("secret/public")),
    ] {
        let policy = FileSystemSandboxPolicy::restricted(vec![
            entry(base.clone(), FileSystemAccessMode::Read),
            entry(denied.clone(), FileSystemAccessMode::Deny),
            entry(public.clone(), FileSystemAccessMode::Read),
        ]);
        let args = seatbelt_args(&policy, &base, vec!["/bin/true".to_string()]);
        let read = generated_clauses(&args, "READABLE_ROOT");
        for dir in [&real, &link] {
            assert!(
                clauses_allow(&read, &dir.join("secret/public/notes")),
                "{denied:?}: {dir:?}/secret/public/notes"
            );
            assert!(
                !clauses_allow(&read, &dir.join("secret/key")),
                "{denied:?}: {dir:?}/secret/key"
            );
        }
        assert!(clauses_allow(&read, &base.join("other")), "{denied:?}");
    }
    // A denied link to a readable folder denies the folder itself.
    let policy = FileSystemSandboxPolicy::restricted(vec![
        entry(real.clone(), FileSystemAccessMode::Read),
        entry(link, FileSystemAccessMode::Deny),
    ]);
    let args = seatbelt_args(&policy, &base, vec!["/bin/true".to_string()]);
    let read = generated_clauses(&args, "READABLE_ROOT");
    assert!(!clauses_allow(&read, &real.join("secret/public/notes")));
}

/// Without denials (Permissive's shape) reads stay unrestricted.
#[test]
fn policies_without_denials_keep_full_read_access() {
    let cwd = TempDir::new().expect("tempdir");
    let policy = FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(
        &SandboxPolicy::new_workspace_write_policy(),
        cwd.path(),
    );
    let args = seatbelt_args(&policy, cwd.path(), vec!["/bin/true".to_string()]);
    assert!(seatbelt_policy_arg(&args).contains("(allow file-read*)"));
    assert!(generated_clauses(&args, "READABLE_ROOT").is_empty());
    assert!(super::seatbelt_allows_file_read(
        &policy,
        cwd.path(),
        &cwd.path().join(".env")
    ));
}

/// #488 regression: agent commands in a workspace opened through a nested
/// link cannot read or create the denied files in any spelling.
#[test]
fn sandboxed_commands_cannot_read_denied_files_through_a_nested_link() {
    let base = TempDir::new().expect("tempdir");
    let base = base.path().canonicalize().expect("canonical");
    let tmp = tempfile::tempdir_in("/tmp").expect("tempdir in /tmp");
    let real = base.join("real");
    let link = base.join("link");
    // Missing denied paths are covered by the creation check below.
    for scenario in link_scenarios(&base, tmp.path())
        .into_iter()
        .filter(|scenario| scenario.name == "nested link workspace")
    {
        let name = scenario.name;
        let policy = workspace_policy_denying(&scenario.denied);
        let denied = scenario.denied[0].file_name().expect("name");
        let denied = denied.to_string_lossy();
        let script = format!(
            "cat {denied}; echo dot=$?; cat '{real}/{denied}'; echo real=$?; \
             cat '{link}/{denied}'; echo link=$?; cat other; echo other=$?; \
             echo y > fresh.txt; echo fresh=$?",
            real = real.display(),
            link = link.display(),
        );
        let args = seatbelt_args(
            &policy,
            &scenario.cwd,
            vec!["/bin/sh".to_string(), "-c".to_string(), script],
        );
        let output = Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
            .args(&args)
            .current_dir(&link)
            .output()
            .expect("run seatbelt");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(!stdout.contains("API_KEY"), "{name}: {stdout}");
        for line in ["dot=1", "real=1", "link=1", "other=0", "fresh=0"] {
            assert!(stdout.contains(line), "{name}: {line} not in {stdout}");
        }
        let _ = fs::remove_file(real.join("fresh.txt"));
    }
    // A denied path not created yet cannot be created through the link,
    // whichever spelling denies it.
    for denied in [link.join("never.key"), real.join("later.key")] {
        let policy = workspace_policy_denying(std::slice::from_ref(&denied));
        let name = denied.file_name().expect("name").to_string_lossy();
        let args = seatbelt_args(
            &policy,
            &link,
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                format!("echo x > {name}; echo created=$?"),
            ],
        );
        let output = Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
            .args(&args)
            .current_dir(&link)
            .output()
            .expect("run seatbelt");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("created=1"), "{denied:?}: {stdout}");
        assert!(!real.join(&*name).exists(), "{denied:?}");
    }
}

fn path_entry(path: &Path, access: FileSystemAccessMode) -> FileSystemSandboxEntry {
    FileSystemSandboxEntry {
        path: FileSystemPath::Path {
            path: AbsolutePathBuf::from_absolute_path(path).expect("absolute"),
        },
        access,
        missing_path_behavior: None,
    }
}

fn run_in_seatbelt(policy: &FileSystemSandboxPolicy, cwd: &Path, script: &str) -> String {
    let args = seatbelt_args(
        policy,
        cwd,
        vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
    );
    let output = Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
        .args(&args)
        .current_dir(cwd)
        .output()
        .expect("run seatbelt");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// #529: a writable root that is a link to the workspace's `.git` cannot
/// reopen it (Linux since #511), nor can a writable link into another root
/// reopen that root's read-only path. Each stays read-only through the link
/// and under its real path; the rest stays writable.
#[test]
fn writable_links_cannot_reopen_read_only_paths_of_other_roots() {
    let base = TempDir::new().expect("tempdir");
    let base = base.path().canonicalize().expect("canonical");
    let workspace = base.join("ws");
    let git = workspace.join(".git");
    fs::create_dir_all(&git).expect(".git");
    fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    let git_link = base.join("gitlink");
    std::os::unix::fs::symlink(&git, &git_link).expect("link to .git");
    let outer = base.join("outer");
    let target = outer.join("c");
    let read_only = target.join("ro");
    fs::create_dir_all(&read_only).expect("ro");
    let link = base.join("link");
    std::os::unix::fs::symlink(&target, &link).expect("link into outer");
    let denied = base.join("denied");
    fs::write(&denied, "x").expect("denied");
    let root_read = FileSystemSandboxEntry {
        path: FileSystemPath::Special {
            value: FileSystemSpecialPath::Root,
        },
        access: FileSystemAccessMode::Read,
        missing_path_behavior: None,
    };
    let policy = FileSystemSandboxPolicy::restricted(vec![
        root_read,
        path_entry(&workspace, FileSystemAccessMode::Write),
        path_entry(&git_link, FileSystemAccessMode::Write),
        path_entry(&outer, FileSystemAccessMode::Write),
        path_entry(&link, FileSystemAccessMode::Write),
        path_entry(&read_only, FileSystemAccessMode::Read),
        path_entry(&denied, FileSystemAccessMode::Deny),
    ]);
    let script = format!(
        "echo x >> '{git}/HEAD'; echo git=$?; echo x >> '{git_link}/HEAD'; echo gitlink=$?; \
         echo x > '{read_only}/a'; echo ro=$?; echo x > '{link}/ro/b'; echo linkro=$?; \
         echo x > '{workspace}/file'; echo ws=$?; echo x > '{link}/file'; echo link=$?",
        git = git.display(),
        git_link = git_link.display(),
        read_only = read_only.display(),
        link = link.display(),
        workspace = workspace.display(),
    );
    let stdout = run_in_seatbelt(&policy, &workspace, &script);
    assert_lines(
        &stdout,
        &["git=1", "gitlink=1", "ro=1", "linkro=1", "ws=0", "link=0"],
    );
    assert_eq!(
        fs::read_to_string(git.join("HEAD")).expect("HEAD"),
        "ref: refs/heads/main\n"
    );
    assert!(!read_only.join("a").exists() && !read_only.join("b").exists());
}

/// #529: a policy without denied paths (Permissive's shape) keeps today's
/// write clauses for an outer root holding the workspace: the change applies
/// only once the policy denies reads. #535 (Travis, 2026-10-12): a writable
/// link to the workspace's `.git` is no grant at either.
#[test]
fn writable_link_clauses_are_unchanged_without_denials() {
    let base = TempDir::new().expect("tempdir");
    let base = base.path().canonicalize().expect("canonical");
    let workspace = base.join("ws");
    fs::create_dir_all(workspace.join(".git")).expect(".git");
    let git_link = base.join("gitlink");
    std::os::unix::fs::symlink(workspace.join(".git"), &git_link).expect("link");
    let root_read = FileSystemSandboxEntry {
        path: FileSystemPath::Special {
            value: FileSystemSpecialPath::Root,
        },
        access: FileSystemAccessMode::Read,
        missing_path_behavior: None,
    };
    let with_link = |outer: bool, denied: bool| {
        let mut entries = vec![
            root_read.clone(),
            path_entry(&workspace, FileSystemAccessMode::Write),
            path_entry(&git_link, FileSystemAccessMode::Write),
        ];
        if outer {
            entries.push(path_entry(&base, FileSystemAccessMode::Write));
        }
        if denied {
            entries.push(path_entry(&base.join("denied"), FileSystemAccessMode::Deny));
        }
        let args = seatbelt_args(
            &FileSystemSandboxPolicy::restricted(entries),
            &workspace,
            vec!["/bin/true".to_string()],
        );
        generated_clauses(&args, "WRITABLE_ROOT")
    };
    let head = workspace.join(".git/HEAD");
    assert!(!clauses_allow(&with_link(false, false), &head));
    assert!(clauses_allow(&with_link(true, false), &head));
    let write = with_link(true, true);
    assert!(!clauses_allow(&write, &head));
    assert!(clauses_allow(&write, &workspace.join("file")));
}

fn assert_lines(stdout: &str, expected: &[&str]) {
    for line in expected {
        assert!(
            stdout.lines().any(|got| got == *line),
            "{line} not in {stdout}"
        );
    }
}

fn root_read_entry() -> FileSystemSandboxEntry {
    FileSystemSandboxEntry {
        path: FileSystemPath::Special {
            value: FileSystemSpecialPath::Root,
        },
        access: FileSystemAccessMode::Read,
        missing_path_behavior: None,
    }
}

/// #529 review: a writable folder holding the workspace (like `/tmp`)
/// cannot write, create or rename-around the workspace's protected
/// metadata, whether it exists or not.
#[test]
fn an_outer_writable_root_keeps_the_workspace_metadata_protected() {
    let base = TempDir::new().expect("tempdir");
    let base = base.path().canonicalize().expect("canonical");
    let outer = base.join("outer");
    let workspace = outer.join("ws");
    fs::create_dir_all(workspace.join(".git")).expect(".git");
    fs::write(workspace.join(".git/HEAD"), "ref\n").expect("HEAD");
    let policy = FileSystemSandboxPolicy::restricted(vec![
        root_read_entry(),
        path_entry(&outer, FileSystemAccessMode::Write),
        path_entry(&workspace, FileSystemAccessMode::Write),
        path_entry(&base.join("denied"), FileSystemAccessMode::Deny),
    ]);
    let script = format!(
        "echo x >> '{ws}/.git/HEAD'; echo git=$?; mkdir '{ws}/.agents'; echo agents=$?; \
         mkdir '{ws}/.codex'; echo codex=$?; mv '{ws}' '{outer}/moved'; echo moved=$?; \
         mv '{ws}/.git' '{outer}/git'; echo gitmoved=$?; echo x > '{ws}/file'; echo ws=$?; \
         echo x > '{outer}/file'; echo outer=$?",
        ws = workspace.display(),
        outer = outer.display(),
    );
    let stdout = run_in_seatbelt(&policy, &workspace, &script);
    assert_lines(
        &stdout,
        &[
            "git=1",
            "agents=1",
            "codex=1",
            "moved=1",
            "gitmoved=1",
            "ws=0",
            "outer=0",
        ],
    );
    assert!(workspace.join(".git/HEAD").exists() && !workspace.join(".agents").exists());
}

/// Whether `dir` is on a volume that ignores case (macOS's default).
fn ignores_case(dir: &Path) -> bool {
    let probe = dir.join("case-probe-606");
    fs::write(&probe, "x").expect("probe");
    let ignores = dir.join("CASE-PROBE-606").exists();
    fs::remove_file(&probe).expect("remove probe");
    ignores
}

/// #606: on a volume that ignores case, a `.GIT`, `.Codex` or `.AGENTS` made
/// where the name is missing is the folder git or Corbanu opens. Seatbelt
/// matches the protected-name rules without regard to case there, at every
/// level, so none can be created in a writable root.
#[test]
fn metadata_names_are_protected_in_any_case() {
    let base = TempDir::new().expect("tempdir");
    let base = base.path().canonicalize().expect("canonical");
    if !ignores_case(&base) {
        return;
    }
    for denies_reads in [true, false] {
        let workspace = base.join(format!("ws-{denies_reads}"));
        fs::create_dir_all(&workspace).expect("ws");
        let mut entries = vec![
            root_read_entry(),
            path_entry(&workspace, FileSystemAccessMode::Write),
        ];
        if denies_reads {
            entries.push(path_entry(&base.join("denied"), FileSystemAccessMode::Deny));
        }
        let policy = FileSystemSandboxPolicy::restricted(entries);
        let script = format!(
            "mkdir '{ws}/.GIT'; echo git=$?; mkdir '{ws}/.Codex'; echo codex=$?; \
             mkdir '{ws}/.AGENTS'; echo agents=$?; mkdir '{ws}/.gitx'; echo gitx=$?; \
             echo x > '{ws}/file'; echo ws=$?",
            ws = workspace.display(),
        );
        let stdout = run_in_seatbelt(&policy, &workspace, &script);
        assert_lines(&stdout, &["git=1", "codex=1", "agents=1", "gitx=0", "ws=0"]);
    }
}

/// #529 review: an explicit writable root stays writable when another root
/// spells its folder through a link (the workspace opened through a nested
/// link, its real `.git` granted).
#[test]
fn an_explicit_writable_root_stays_writable_under_another_spelling() {
    let base = TempDir::new().expect("tempdir");
    let base = base.path().canonicalize().expect("canonical");
    let real = base.join("real");
    fs::create_dir_all(real.join(".git")).expect(".git");
    let link = base.join("link");
    std::os::unix::fs::symlink(&real, &link).expect("link");
    let policy = FileSystemSandboxPolicy::restricted(vec![
        root_read_entry(),
        path_entry(&link, FileSystemAccessMode::Write),
        path_entry(&real.join(".git"), FileSystemAccessMode::Write),
        path_entry(&base.join("denied"), FileSystemAccessMode::Deny),
    ]);
    let script = format!(
        "echo x > '{real}/.git/HEAD'; echo git=$?; echo x > '{link}/.git/config'; echo linkgit=$?",
        real = real.display(),
        link = link.display(),
    );
    let stdout = run_in_seatbelt(&policy, &link, &script);
    assert_lines(&stdout, &["git=0", "linkgit=0"]);
}

/// #529 review: a configured writable root not created yet inside another
/// writable root (a `target` folder before the first build) can be created.
#[test]
fn a_missing_writable_root_inside_another_root_can_be_created() {
    let base = TempDir::new().expect("tempdir");
    let base = base.path().canonicalize().expect("canonical");
    let workspace = base.join("ws");
    fs::create_dir_all(&workspace).expect("ws");
    let target = workspace.join("target");
    let policy = FileSystemSandboxPolicy::restricted(vec![
        root_read_entry(),
        path_entry(&workspace, FileSystemAccessMode::Write),
        path_entry(&target, FileSystemAccessMode::Write),
        path_entry(&base.join("denied"), FileSystemAccessMode::Deny),
    ]);
    let script = format!(
        "mkdir '{target}'; echo target=$?; echo x > '{target}/out'; echo out=$?",
        target = target.display(),
    );
    let stdout = run_in_seatbelt(&policy, &workspace, &script);
    assert_lines(&stdout, &["target=0", "out=0"]);
}

/// #506: a directory the policy may not list cannot be listed, by any
/// spelling, under a read-only or a full-write base; its entries, their
/// metadata and its own metadata stay as the rest of the policy leaves them,
/// and a denied entry stays denied.
#[test]
fn unlistable_directory_cannot_be_listed_but_its_entries_stay_usable() {
    let dir = TempDir::new().expect("tempdir");
    let base = dir.path().canonicalize().expect("canonical");
    let home = base.join("home");
    fs::create_dir_all(home.join("tmp")).expect("tmp");
    fs::write(home.join("tmp/helper.txt"), "helper-506\n").expect("helper");
    fs::write(home.join("listing-canary-506"), "x\n").expect("canary");
    fs::write(home.join("secret.txt"), "secret-506\n").expect("secret");
    std::os::unix::fs::symlink(&home, base.join("link")).expect("link");
    let home_path = AbsolutePathBuf::from_absolute_path(&home).expect("absolute");
    for root_access in [FileSystemAccessMode::Read, FileSystemAccessMode::Write] {
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
        let script = "ls home; echo ls=$?; ls home/; echo ls-slash=$?; ls link/; echo ls-link=$?; \
                      ls home/tmp/..; echo ls-dotdot=$?; echo home/*; \
                      cat home/tmp/helper.txt; ls home/tmp; echo ls-tmp=$?; \
                      test -d home && echo is-dir; cd home/tmp && pwd -P; cd \"$OLDPWD\"; \
                      cat home/secret.txt; echo cat-secret=$?";
        let args = create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
            command: vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
            file_system_sandbox_policy: &policy,
            network_sandbox_policy: NetworkSandboxPolicy::Restricted,
            sandbox_policy_cwd: &base,
            enforce_managed_network: false,
            managed_network: None,
            environment_id: None,
            network: None,
            extra_allow_unix_sockets: &[],
        })
        .expect("args");
        let output = Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
            .args(&args)
            .current_dir(&base)
            .output()
            .expect("run seatbelt");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let context = format!("{root_access:?}: {stdout}");
        for denied in ["ls=1", "ls-slash=1", "ls-link=1", "ls-dotdot=1"] {
            assert!(stdout.contains(denied), "{denied} {context}");
        }
        assert!(!stdout.contains("listing-canary-506"), "{context}");
        assert!(stdout.contains("helper-506"), "{context}");
        assert!(stdout.contains("helper.txt"), "{context}");
        assert!(stdout.contains("ls-tmp=0"), "{context}");
        assert!(stdout.contains("is-dir"), "{context}");
        assert!(stdout.contains("cat-secret=1"), "{context}");
        assert!(!stdout.contains("secret-506"), "{context}");
    }
}

/// #442 (Travis, 2026-10-12), real `sandbox-exec`: `.env.example`,
/// `.env.sample` and `.env.template`, in any case, are not credential files:
/// an agent command reads, creates, overwrites and removes them, but cannot
/// make one a link or pipe (a person's later `.env` could be renamed from
/// it). Every other credential name stays denied, and a template name never
/// reaches the person's `.env` through a link or hard link.
#[test]
fn credential_templates_stay_usable() {
    let tmp = TempDir::new().expect("tempdir");
    let work = tmp.path().canonicalize().expect("canonical");
    fs::write(work.join(".env"), "API_KEY=dummy-442-person").expect("env");
    fs::write(work.join(".env.example"), "API_KEY=example-442").expect("example");
    fs::write(work.join(".ENV.SAMPLE"), "API_KEY=sample-442").expect("sample");
    fs::write(work.join("notes"), "plain-notes").expect("notes");
    let mut entries = vec![
        FileSystemSandboxEntry::new(
            FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            FileSystemAccessMode::Read,
        ),
        FileSystemSandboxEntry::new(
            FileSystemPath::Path {
                path: AbsolutePathBuf::from_absolute_path(&work).expect("absolute"),
            },
            FileSystemAccessMode::Write,
        ),
    ];
    entries.extend(
        codex_protocol::permissions::credential_file_globs(&work.to_string_lossy())
            .into_iter()
            .map(|pattern| {
                FileSystemSandboxEntry::new(
                    FileSystemPath::GlobPattern {
                        pattern,
                        scan: None,
                    },
                    FileSystemAccessMode::Deny,
                )
            }),
    );
    let policy = FileSystemSandboxPolicy::restricted(entries);
    let script = "cat .env.example; echo example=$?; cat .ENV.SAMPLE; echo sample=$?; \
         printf tpl > .env.template; echo template=$?; cat .env.template; echo; \
         printf x >> .env.example; echo append=$?; \
         mkdir -p app && cp .env.example app/.Env.Template; echo copy=$?; \
         rm .ENV.SAMPLE; echo removed=$?; \
         cat .env; echo read=$?; printf x > .env.local; echo create=$?; \
         printf x > .env.example.local; echo near=$?; \
         ln -s notes .env.sample; echo symlink=$?; mkfifo .env.sample; echo fifo=$?; \
         ln -s .env .env.sample; cat .env.sample; echo link=$?; \
         rm .env.template; ln .env .env.template; cat .env.template; echo readtpl=$?; \
         ln .env hard-copy; cat hard-copy; echo hardcopy=$?; \
         mv .env .env.example; echo mvenv=$?; \
         ln -s notes xlink; echo mklink=$?; mv xlink .env.sample; echo mvlink=$?; \
         mkfifo xfifo; echo mkfifo=$?; mv xfifo .env.sample; echo mvfifo=$?; \
         ln -s notes ylink; mv ylink .env.example; echo mvover=$?; \
         cp notes y2; mv y2 .env.template; echo mvreg=$?; cat .env.template; echo";
    let args = create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
        command: vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
        file_system_sandbox_policy: &policy,
        network_sandbox_policy: NetworkSandboxPolicy::Restricted,
        sandbox_policy_cwd: &work,
        enforce_managed_network: false,
        managed_network: None,
        environment_id: None,
        network: None,
        extra_allow_unix_sockets: &[],
    })
    .expect("args");
    let output = Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
        .args(&args)
        .current_dir(&work)
        .output()
        .expect("sandbox-exec");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("dummy-442"), "{stdout}");
    for line in [
        "API_KEY=example-442example=0",
        "API_KEY=sample-442sample=0",
        "template=0",
        "tpl\n",
        "append=0",
        "copy=0",
        "removed=0",
        "read=1",
        "create=1",
        "near=1",
        "symlink=1",
        "fifo=1",
        "link=1",
        "readtpl=1",
        "hardcopy=1",
        "mvenv=1",
        "mklink=0",
        "mvlink=1",
        "mkfifo=0",
        "mvfifo=1",
        "mvover=1",
        "mvreg=0",
        "plain-notes",
    ] {
        assert!(stdout.contains(line), "{line} not in {stdout}");
    }
    assert!(work.join("app/.Env.Template").is_file());
    assert_eq!(
        fs::read_to_string(work.join(".env")).expect("env"),
        "API_KEY=dummy-442-person"
    );
    // No link or pipe was planted under a template name.
    assert!(fs::symlink_metadata(work.join(".env.sample")).is_err());
    for denied in [".env.local", ".env.example.local"] {
        assert!(
            fs::symlink_metadata(work.join(denied)).is_err(),
            "{denied} was created"
        );
    }
}

/// #442 (Travis, 2026-10-12), real `sandbox-exec`: a person's own
/// `**/.env.*` deny glob (not the whole credential rule) keeps covering the
/// templates, at any level.
#[test]
fn own_env_star_glob_still_covers_templates() {
    let tmp = TempDir::new().expect("tempdir");
    let work = tmp.path().canonicalize().expect("canonical");
    fs::write(work.join(".env.example"), "API_KEY=own-442").expect("example");
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry::new(
            FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            FileSystemAccessMode::Read,
        ),
        FileSystemSandboxEntry::new(
            FileSystemPath::GlobPattern {
                scan: None,
                pattern: format!("{}/**/.env.*", work.display()),
            },
            FileSystemAccessMode::Deny,
        ),
    ]);
    let args = create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
        command: vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "cat .env.example; echo example=$?".to_string(),
        ],
        file_system_sandbox_policy: &policy,
        network_sandbox_policy: NetworkSandboxPolicy::Restricted,
        sandbox_policy_cwd: &work,
        enforce_managed_network: false,
        managed_network: None,
        environment_id: None,
        network: None,
        extra_allow_unix_sockets: &[],
    })
    .expect("args");
    let output = Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
        .args(&args)
        .current_dir(&work)
        .output()
        .expect("sandbox-exec");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("own-442"), "{stdout}");
    assert!(stdout.contains("example=1"), "{stdout}");
}

/// #442 (Travis, 2026-10-11), real `sandbox-exec`: under credential-name deny
/// globs an agent command can neither read such a file nor create, link,
/// rename to or overwrite one, in any letter case, so it cannot plant a link or
/// pipe that the person's later `.env` would be written through. A folder
/// with such a name (a `.env` virtualenv) and other files stay usable.
#[test]
fn credential_names_cannot_be_read_created_or_planted() {
    let tmp = TempDir::new().expect("tempdir");
    let work = tmp.path().canonicalize().expect("canonical");
    fs::write(work.join(".env"), "API_KEY=dummy-442-person").expect("env");
    fs::write(work.join("notes"), "plain-notes").expect("notes");
    let mut entries = vec![
        FileSystemSandboxEntry::new(
            FileSystemPath::Special {
                value: FileSystemSpecialPath::Root,
            },
            FileSystemAccessMode::Read,
        ),
        FileSystemSandboxEntry::new(
            FileSystemPath::Path {
                path: AbsolutePathBuf::from_absolute_path(&work).expect("absolute"),
            },
            FileSystemAccessMode::Write,
        ),
    ];
    entries.extend(
        codex_protocol::permissions::credential_file_globs(&work.to_string_lossy())
            .into_iter()
            .map(|pattern| {
                FileSystemSandboxEntry::new(
                    FileSystemPath::GlobPattern {
                        pattern,
                        scan: None,
                    },
                    FileSystemAccessMode::Deny,
                )
            }),
    );
    let policy = FileSystemSandboxPolicy::restricted(entries);
    let script = "cat .env; echo read=$?; cat .ENV; echo case=$?; \
         printf x > .env; echo overwrite=$?; printf x >> .env; echo append=$?; \
         printf x > .env.local; echo create=$?; ln notes .env.hard; echo hard=$?; \
         ln -s notes .envrc; echo sym=$?; mkfifo .env.pipe; echo fifo=$?; \
         cp notes n2 && mv n2 .env.moved; echo moved=$?; mv .env gone; echo away=$?; \
         printf x > .ENV.Local; echo upper=$?; ln -s notes .ENVRC; echo upperlink=$?; \
         mkdir .env.venv && printf ok > .env.venv/bin; echo venv=$?; \
         printf ok > other; cat other; echo other=$?";
    let args = create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
        command: vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
        file_system_sandbox_policy: &policy,
        network_sandbox_policy: NetworkSandboxPolicy::Restricted,
        sandbox_policy_cwd: &work,
        enforce_managed_network: false,
        managed_network: None,
        environment_id: None,
        network: None,
        extra_allow_unix_sockets: &[],
    })
    .expect("args");
    let output = Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
        .args(&args)
        .current_dir(&work)
        .output()
        .expect("sandbox-exec");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!stdout.contains("dummy-442"), "{stdout}");
    for line in [
        "read=1",
        "case=1",
        "overwrite=1",
        "append=1",
        "create=1",
        "hard=1",
        "sym=1",
        "fifo=1",
        "moved=1",
        "away=1",
        "upper=1",
        "upperlink=1",
        "venv=0",
        "okother=0",
    ] {
        assert!(stdout.contains(line), "{line} not in {stdout}");
    }
    assert_eq!(
        fs::read_to_string(work.join(".env")).expect("env"),
        "API_KEY=dummy-442-person"
    );
    for planted in [
        ".env.local",
        ".env.hard",
        ".envrc",
        ".env.pipe",
        ".env.moved",
        ".ENV.Local",
        ".ENVRC",
    ] {
        assert!(
            fs::symlink_metadata(work.join(planted)).is_err(),
            "{planted} was created"
        );
    }
}

/// Policies shaped like each security level, for the #535 tests: Permissive
/// (a workspace-write config with extra writable roots, no denials), Moderate
/// (the same with a denied path) and Aggressive (as Moderate, with the
/// temporary folders not writable).
fn level_policies(
    workspace: &Path,
    extra_writable: &[&Path],
    denied: &Path,
) -> Vec<(&'static str, FileSystemSandboxPolicy)> {
    let workspace_write = |exclude_tmp: bool| {
        FileSystemSandboxPolicy::from_legacy_sandbox_policy_for_cwd(
            &SandboxPolicy::WorkspaceWrite {
                writable_roots: extra_writable
                    .iter()
                    .map(|path| AbsolutePathBuf::from_absolute_path(path).expect("absolute"))
                    .collect(),
                network_access: false,
                exclude_tmpdir_env_var: exclude_tmp,
                exclude_slash_tmp: exclude_tmp,
            },
            workspace,
        )
    };
    let denial = path_entry(denied, FileSystemAccessMode::Deny);
    let permissive = workspace_write(false);
    let mut moderate = permissive.entries.clone();
    moderate.push(denial.clone());
    let mut aggressive = workspace_write(true).entries;
    aggressive.push(denial);
    vec![
        ("permissive", permissive),
        ("moderate", FileSystemSandboxPolicy::restricted(moderate)),
        (
            "aggressive",
            FileSystemSandboxPolicy::restricted(aggressive),
        ),
    ]
}

/// #535 (Travis, 2026-10-12: option B), real `sandbox-exec`: a writable root
/// that is a link into another root's read-only path (a workspace's
/// `.git/hooks`, or its `.git` itself) is not a grant at any level: the hooks
/// and `.git` stay read-only through it, while the workspace stays writable.
#[test]
fn writable_links_into_a_read_only_path_are_not_grants_at_any_level() {
    let base = TempDir::new().expect("tempdir");
    let base = base.path().canonicalize().expect("canonical");
    let workspace = base.join("ws");
    let hooks = workspace.join(".git").join("hooks");
    fs::create_dir_all(&hooks).expect("hooks");
    fs::write(hooks.join("pre-commit"), "#!/bin/sh\n").expect("hook");
    let hooks_link = base.join("hooks-link");
    std::os::unix::fs::symlink(&hooks, &hooks_link).expect("link to hooks");
    let git_link = base.join("git-link");
    std::os::unix::fs::symlink(workspace.join(".git"), &git_link).expect("link to .git");
    let denied = base.join("denied");
    fs::write(&denied, "x").expect("denied");
    // #535 review: on a filesystem that ignores case (macOS's default), links
    // spelled in another case, and a link root typed in another case.
    let ignores_case = base.join("WS").exists();
    let case_hooks = base.join("case-hooks");
    std::os::unix::fs::symlink(workspace.join(".GIT").join("hooks"), &case_hooks)
        .expect("case link to hooks");
    let case_git = base.join("case-git");
    std::os::unix::fs::symlink(workspace.join(".Git"), &case_git).expect("case link to .git");
    let typed = base.join("HOOKS-LINK");
    let mut links: Vec<&Path> = vec![&hooks_link, &git_link];
    if ignores_case {
        links.extend([case_hooks.as_path(), case_git.as_path(), typed.as_path()]);
    }
    for (level, policy) in level_policies(&workspace, &links, &denied) {
        let exclusions = policy.writable_links_into_read_only_paths(&workspace);
        assert_eq!(exclusions.len(), links.len(), "{level}: {exclusions:?}");
        let script = format!(
            "echo x >> '{hooks_link}/pre-commit'; echo hook=$?; echo x > '{hooks_link}/new'; \
             echo hooknew=$?; echo x > '{git_link}/HEAD'; echo gitlink=$?; \
             echo x > '{hooks}/direct'; echo direct=$?; echo x > '{workspace}/file'; echo ws=$?; \
             echo x > '{case_hooks}/case'; echo casehooks=$?; echo x > '{case_git}/HEAD'; \
             echo casegit=$?; echo x > '{typed}/typed'; echo typed=$?",
            hooks_link = hooks_link.display(),
            git_link = git_link.display(),
            hooks = hooks.display(),
            workspace = workspace.display(),
            case_hooks = case_hooks.display(),
            case_git = case_git.display(),
            typed = typed.display(),
        );
        let stdout = run_in_seatbelt(&policy, &workspace, &script);
        for line in [
            "hook=1",
            "hooknew=1",
            "gitlink=1",
            "direct=1",
            "ws=0",
            "casehooks=1",
            "casegit=1",
            "typed=1",
        ] {
            assert!(
                stdout.lines().any(|got| got == line),
                "{level}: {line} not in {stdout}"
            );
        }
        assert_eq!(
            fs::read_to_string(hooks.join("pre-commit")).expect("hook"),
            "#!/bin/sh\n",
            "{level}"
        );
        assert!(!hooks.join("new").exists() && !workspace.join(".git/HEAD").exists());
        assert!(!hooks.join("case").exists() && !hooks.join("typed").exists());
        fs::remove_file(workspace.join("file")).expect("ws file");
    }
}

/// #535: what stays a grant at every level: an explicit root spelled through
/// the read-only path (the person names `.git/hooks` itself), a link that
/// leads elsewhere, a workspace spelled through `/tmp` (`/private/tmp`), and
/// a git worktree with the main checkout as an extra root.
#[test]
fn ordinary_roots_stay_writable_at_every_level() {
    let base = tempfile::Builder::new()
        .prefix("roots-535-")
        .tempdir_in("/tmp")
        .expect("tempdir in /tmp");
    // Spelled through the `/tmp` alias on purpose.
    let base = base.path().to_path_buf();
    assert!(base.starts_with("/tmp"));
    let main = base.join("main");
    let gitdir = main.join(".git").join("worktrees").join("wt");
    fs::create_dir_all(&gitdir).expect("gitdir");
    fs::create_dir_all(main.join(".git/hooks")).expect("hooks");
    let worktree = base.join("wt");
    fs::create_dir_all(&worktree).expect("worktree");
    fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", gitdir.display()),
    )
    .expect(".git file");
    let elsewhere = base.join("elsewhere");
    fs::create_dir_all(&elsewhere).expect("elsewhere");
    // A link inside `/tmp` (itself writable) that leads elsewhere in it.
    let link = base.join("cache");
    std::os::unix::fs::symlink(&elsewhere, &link).expect("link elsewhere");
    let hooks = main.join(".git/hooks");
    let denied = base.join("denied");
    fs::write(&denied, "x").expect("denied");
    for (level, policy) in level_policies(&worktree, &[&main, &hooks, &link], &denied) {
        assert_eq!(
            policy.writable_links_into_read_only_paths(&worktree),
            Vec::new(),
            "{level}"
        );
        let script = format!(
            "echo x > file; echo wt=$?; echo x > '{main}/file'; echo main=$?; \
             echo x > '{hooks}/explicit'; echo hooks=$?; echo x > '{link}/file'; echo link=$?",
            main = main.display(),
            link = link.display(),
            hooks = hooks.display(),
        );
        let stdout = run_in_seatbelt(&policy, &worktree, &script);
        for line in ["wt=0", "main=0", "hooks=0", "link=0"] {
            assert!(
                stdout.lines().any(|got| got == line),
                "{level}: {line} not in {stdout}"
            );
        }
        assert!(elsewhere.join("file").exists(), "{level}");
        for written in [
            worktree.join("file"),
            main.join("file"),
            hooks.join("explicit"),
            elsewhere.join("file"),
        ] {
            fs::remove_file(written).expect("written");
        }
    }
}

/// A folder for test files outside every writable root and outside the
/// source tree: the test binary's folder.
fn outside_temp_root() -> PathBuf {
    std::env::current_exe()
        .expect("test binary")
        .parent()
        .expect("test binary folder")
        .to_path_buf()
}

/// Runs `script` in `cwd` the way a session does: through
/// [`crate::SandboxManager::transform`] (which checks the writable roots,
/// #536) and real `sandbox-exec`. `Err` carries a refusal.
fn run_transformed(
    policy: &FileSystemSandboxPolicy,
    cwd: &Path,
    script: &str,
) -> Result<String, String> {
    use codex_protocol::config_types::WindowsSandboxLevel;
    use codex_protocol::models::PermissionProfile;
    use codex_utils_path_uri::PathUri;

    let cwd_uri = PathUri::from_abs_path(&AbsolutePathBuf::from_absolute_path(cwd).expect("cwd"));
    let permissions =
        PermissionProfile::from_runtime_permissions(policy, NetworkSandboxPolicy::Restricted);
    let request = crate::SandboxManager::new()
        .transform(crate::SandboxTransformRequest {
            command: crate::SandboxCommand {
                program: "/bin/sh".into(),
                args: vec!["-c".to_string(), script.to_string()],
                cwd: cwd_uri.clone(),
                env: std::collections::HashMap::new(),
                managed_network: None,
                additional_permissions: None,
            },
            permissions: &permissions,
            sandbox: crate::SandboxType::MacosSeatbelt,
            enforce_managed_network: false,
            environment_id: None,
            network: None,
            sandbox_policy_cwd: &cwd_uri,
            codex_linux_sandbox_exe: None,
            use_legacy_landlock: false,
            windows_sandbox_level: WindowsSandboxLevel::Disabled,
            windows_sandbox_private_desktop: false,
        })
        .map_err(|err| err.to_string())?;
    let output = Command::new(&request.command[0])
        .args(&request.command[1..])
        .current_dir(cwd)
        .output()
        .expect("sandbox-exec");
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// #536 (Travis, 2026-10-12), real `sandbox-exec` through the sandbox
/// manager: at every level, Permissive included, a configured writable root
/// that does not exist yet and is created by a command as a link elsewhere
/// does not move write access there; the next command is refused and says
/// why. Removing the link lets commands run again.
#[test]
fn a_root_created_as_a_link_during_the_session_is_refused_at_every_level() {
    let base = TempDir::new().expect("tempdir");
    let base = base.path().canonicalize().expect("canonical");
    // Outside every writable root at every level (not in a temporary folder).
    let outside = tempfile::tempdir_in(outside_temp_root()).expect("outside");
    let outside = outside.path().canonicalize().expect("canonical");
    let denied = base.join("denied");
    fs::write(&denied, "x").expect("denied");
    for (level, policy) in level_policies_with(&base, "ws", &["target"], &denied) {
        let workspace = base.join(level).join("ws");
        let target = workspace.join("target");
        crate::writable_root_ledger::record_writable_roots(&policy, &workspace);
        let made = run_transformed(
            &policy,
            &workspace,
            &format!("ln -s '{}' target; echo link=$?", outside.display()),
        )
        .expect("the first command runs");
        assert!(made.contains("link=0"), "{level}: {made}");
        assert!(target.is_symlink());
        let refusal = run_transformed(
            &policy,
            &workspace,
            "echo pwned > target/planted; echo wrote=$?",
        )
        .expect_err("the next command is refused");
        assert!(
            refusal.contains("Refused to run the command")
                && refusal.contains(&target.display().to_string()),
            "{level}: {refusal}"
        );
        assert!(!outside.join("planted").exists(), "{level}");
        fs::remove_file(&target).expect("remove the link");
        let again = run_transformed(&policy, &workspace, "echo ok > file; echo ws=$?")
            .expect("runs again without the link");
        assert!(again.contains("ws=0"), "{level}: {again}");
    }
}

/// The level shapes of [`level_policies`] for a workspace `base/<level>/<name>`
/// (created) with extra writable roots below it (not created).
fn level_policies_with(
    base: &Path,
    name: &str,
    extra_below_workspace: &[&str],
    denied: &Path,
) -> Vec<(&'static str, FileSystemSandboxPolicy)> {
    ["permissive", "moderate", "aggressive"]
        .into_iter()
        .map(|level| {
            let workspace = base.join(level).join(name);
            fs::create_dir_all(&workspace).expect("workspace");
            let extra: Vec<PathBuf> = extra_below_workspace
                .iter()
                .map(|child| workspace.join(child))
                .collect();
            let extra: Vec<&Path> = extra.iter().map(PathBuf::as_path).collect();
            let policy = level_policies(&workspace, &extra, denied)
                .into_iter()
                .find(|(shape, _)| *shape == level)
                .map(|(_, policy)| policy)
                .expect("level");
            (level, policy)
        })
        .collect()
}

/// #536: through the sandbox manager, ordinary roots still run at every
/// level: a workspace spelled through `/tmp` (`/private/tmp`), a git
/// worktree with its main checkout, and a configured root created during the
/// session as a real folder.
#[test]
fn ordinary_roots_still_run_through_the_manager_at_every_level() {
    let base = tempfile::Builder::new()
        .prefix("roots-536-")
        .tempdir_in("/tmp")
        .expect("tempdir in /tmp");
    let base = base.path().to_path_buf();
    assert!(base.starts_with("/tmp"));
    let denied = base.join("denied");
    fs::write(&denied, "x").expect("denied");
    for level in ["permissive", "moderate", "aggressive"] {
        let main = base.join(level).join("main");
        let gitdir = main.join(".git").join("worktrees").join("wt");
        fs::create_dir_all(&gitdir).expect("gitdir");
        let worktree = base.join(level).join("wt");
        fs::create_dir_all(&worktree).expect("worktree");
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .expect(".git file");
        let build = worktree.join("build");
        let (_, policy) = level_policies(&worktree, &[&main, &build], &denied)
            .into_iter()
            .find(|(shape, _)| *shape == level)
            .expect("level");
        crate::writable_root_ledger::record_writable_roots(&policy, &worktree);
        let script = format!(
            "mkdir build && echo x > build/out; echo build=$?; echo x > file; echo wt=$?; \
             echo x > '{}/file'; echo main=$?",
            main.display()
        );
        let stdout = run_transformed(&policy, &worktree, &script).expect("runs");
        for line in ["build=0", "wt=0", "main=0"] {
            assert!(
                stdout.lines().any(|got| got == line),
                "{level}: {line} not in {stdout}"
            );
        }
        let again = run_transformed(&policy, &worktree, "echo x >> build/out; echo again=$?")
            .expect("a root created as a real folder keeps running");
        assert!(again.contains("again=0"), "{level}: {again}");
    }
}

/// #536: the Seatbelt arguments grant a writable root at the real location
/// recorded when the session first saw it, not wherever the root leads when
/// they are built: a root swapped for a link after the check grants nothing
/// through the link (real `sandbox-exec`).
#[test]
fn seatbelt_grants_writable_roots_at_their_pinned_location() {
    let base = TempDir::new().expect("tempdir");
    let base = base.path().canonicalize().expect("canonical");
    let workspace = base.join("ws");
    let target = workspace.join("target");
    fs::create_dir_all(&target).expect("target");
    let outside = tempfile::tempdir_in(outside_temp_root()).expect("outside");
    let outside = outside.path().canonicalize().expect("canonical");
    let policy = FileSystemSandboxPolicy::restricted(vec![
        root_read_entry(),
        path_entry(&workspace, FileSystemAccessMode::Read),
        path_entry(&target, FileSystemAccessMode::Write),
    ]);
    crate::writable_root_ledger::record_writable_roots(&policy, &workspace);
    // Swapped after the check, before the arguments are built.
    fs::rename(&target, workspace.join("moved")).expect("move away");
    std::os::unix::fs::symlink(&outside, &target).expect("link");
    let args = seatbelt_args(&policy, &workspace, vec!["/bin/true".to_string()]);
    let write = generated_clauses(&args, "WRITABLE_ROOT");
    assert_eq!(
        write
            .iter()
            .map(|(root, _)| root.clone())
            .collect::<Vec<_>>(),
        vec![target]
    );
    assert!(!clauses_allow(&write, &outside.join("file")));
    let stdout = run_in_seatbelt(&policy, &workspace, "echo x > target/file; echo wrote=$?");
    assert!(stdout.contains("wrote=1"), "{stdout}");
    assert!(!outside.join("file").exists());
}

/// #605: a `:minimal`-read profile with a workspace (a git worktree) below
/// `/tmp`. The platform defaults keep `/tmp` writable for temporary files.
/// `protected`: whether the profile also denies a path (Moderate's and
/// Aggressive's shape), when they no longer reach the workspace's
/// protected paths. Returns the script's output.
fn minimal_profile_in_tmp(protected: bool) -> (TempDir, String) {
    let base = tempfile::tempdir_in("/tmp").expect("tempdir in /tmp");
    // As a person spells it: through the `/tmp` link.
    let x = base.path().to_path_buf();
    let main_git = x.join("main/.git");
    let gitdir = main_git.join("worktrees/wt");
    fs::create_dir_all(&gitdir).expect("gitdir");
    fs::create_dir_all(main_git.join("hooks")).expect("hooks");
    fs::write(gitdir.join("HEAD"), "ref\n").expect("HEAD");
    let workspace = x.join("wt");
    fs::create_dir_all(&workspace).expect("wt");
    fs::write(
        workspace.join(".git"),
        format!("gitdir: {}\n", gitdir.display()),
    )
    .expect(".git file");
    let mut entries = vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Minimal,
            },
            access: FileSystemAccessMode::Read,
            missing_path_behavior: None,
        },
        path_entry(&workspace, FileSystemAccessMode::Write),
    ];
    if protected {
        entries.push(path_entry(&x.join("denied"), FileSystemAccessMode::Deny));
        // A read-only path outside every writable root (the repository's
        // hooks, as Moderate records them).
        entries.push(path_entry(
            &main_git.join("hooks"),
            FileSystemAccessMode::Read,
        ));
    }
    let script = format!(
        "echo x > '{ws}/.git'; echo git=$?; echo x >> '{gitdir}/HEAD'; echo gitdir=$?; \
         echo x > '{main}/hooks/pre-commit'; echo hooks=$?; mkdir '{ws}/.codex'; echo codex=$?; \
         mkdir '{ws}/.agents'; echo agents=$?; mv '{gitdir}' '{x}/moved'; echo moved=$?; \
         echo x > '{ws}/file'; echo ws=$?; echo x > '{x}/scratch'; echo scratch=$?; \
         echo x > /tmp/.codex-605-$$; echo slash_tmp=$?; rm -f /tmp/.codex-605-$$",
        ws = workspace.display(),
        gitdir = gitdir.display(),
        main = main_git.display(),
        x = x.display(),
    );
    let stdout = run_in_seatbelt(
        &FileSystemSandboxPolicy::restricted(entries),
        &workspace,
        &script,
    );
    (base, stdout)
}

/// #605: once the policy denies reads, `/tmp` stays writable for temporary
/// files but no longer reaches a workspace's `.git` (also the worktree's git
/// folder), `.codex`, `.agents` or another read-only path there, which stay
/// read-only as with `:root` reads.
#[test]
fn minimal_reads_keep_protected_paths_in_tmp_read_only() {
    let (base, stdout) = minimal_profile_in_tmp(/*protected*/ true);
    assert_lines(
        &stdout,
        &[
            "git=1",
            "gitdir=1",
            "hooks=1",
            "codex=1",
            "agents=1",
            "moved=1",
            "ws=0",
            "scratch=0",
            "slash_tmp=0",
        ],
    );
    let x = base.path();
    assert_eq!(
        fs::read_to_string(x.join("main/.git/worktrees/wt/HEAD")).expect("HEAD"),
        "ref\n"
    );
    assert!(!x.join("wt/.codex").exists() && !x.join("main/.git/hooks/pre-commit").exists());
}

/// #605: a policy without denied paths (Permissive's shape) keeps the
/// platform defaults' `/tmp` writes as they were.
#[test]
fn minimal_reads_keep_tmp_writes_unchanged_without_denials() {
    let (_base, stdout) = minimal_profile_in_tmp(/*protected*/ false);
    assert_lines(
        &stdout,
        &["git=0", "gitdir=0", "ws=0", "scratch=0", "slash_tmp=0"],
    );
}

/// #649: a `:minimal`-read profile with a workspace below `/tmp`, a denied
/// file in it and one outside it (both below `/tmp`), a `.env` there (a
/// denied glob) and a folder that may not be listed. `denied`: whether the
/// profile denies those paths (Moderate's and Aggressive's shape). Returns
/// the script's output.
fn minimal_reads_in_tmp(denied: bool) -> (TempDir, String) {
    let base = tempfile::tempdir_in("/tmp").expect("tempdir in /tmp");
    let x = base.path().to_path_buf();
    let workspace = x.join("ws");
    let unlistable = x.join("home");
    fs::create_dir_all(&workspace).expect("ws");
    fs::create_dir_all(&unlistable).expect("home");
    fs::write(workspace.join("secret.key"), "ws-secret-649\n").expect("ws secret");
    fs::write(workspace.join(".env"), "env-secret-649\n").expect(".env");
    fs::write(workspace.join("file"), "ws-file-649\n").expect("ws file");
    fs::write(x.join("secret.key"), "outside-secret-649\n").expect("secret");
    fs::write(x.join("scratch"), "scratch-649\n").expect("scratch");
    fs::write(unlistable.join("listing-canary-649"), "x\n").expect("canary");
    let mut entries = vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::Minimal,
            },
            access: FileSystemAccessMode::Read,
            missing_path_behavior: None,
        },
        path_entry(&workspace, FileSystemAccessMode::Write),
    ];
    if denied {
        entries.push(path_entry(
            &workspace.join("secret.key"),
            FileSystemAccessMode::Deny,
        ));
        entries.push(path_entry(
            &x.join("secret.key"),
            FileSystemAccessMode::Deny,
        ));
        entries.push(FileSystemSandboxEntry {
            path: FileSystemPath::GlobPattern {
                scan: None,
                pattern: format!("{}/**/.env", x.display()),
            },
            access: FileSystemAccessMode::Deny,
            missing_path_behavior: None,
        });
        entries.push(FileSystemSandboxEntry::deny_directory_listing(
            &AbsolutePathBuf::from_absolute_path(&unlistable).expect("absolute"),
        ));
    }
    let script = format!(
        "cat '{ws}/secret.key'; echo ws_secret=$?; cat '{x}/secret.key'; echo secret=$?; \
         cat '{ws}/.env'; echo env=$?; ls '{home}'; echo ls=$?; \
         cat '{ws}/file'; echo ws=$?; cat '{x}/scratch'; echo scratch=$?",
        ws = workspace.display(),
        x = x.display(),
        home = unlistable.display(),
    );
    let stdout = run_in_seatbelt(
        &FileSystemSandboxPolicy::restricted(entries),
        &workspace,
        &script,
    );
    (base, stdout)
}

/// #649: once the policy denies reads, `/tmp` stays readable for temporary
/// files but no longer reaches a denied path, a denied glob or a folder that
/// may not be listed there: they stay unreadable as with `:root` reads.
#[test]
fn minimal_reads_keep_denied_paths_in_tmp_unreadable() {
    let (_base, stdout) = minimal_reads_in_tmp(/*denied*/ true);
    assert_lines(
        &stdout,
        &[
            "ws_secret=1",
            "secret=1",
            "env=1",
            "ls=1",
            "ws-file-649",
            "ws=0",
            "scratch-649",
            "scratch=0",
        ],
    );
    for leaked in [
        "ws-secret-649",
        "outside-secret-649",
        "env-secret-649",
        "listing-canary-649",
    ] {
        assert!(!stdout.contains(leaked), "{leaked} leaked: {stdout}");
    }
}

/// #649: a policy without denied paths (Permissive's shape) keeps the
/// platform defaults' `/tmp` reads as they were.
#[test]
fn minimal_reads_keep_tmp_reads_unchanged_without_denials() {
    let (_base, stdout) = minimal_reads_in_tmp(/*denied*/ false);
    assert_lines(
        &stdout,
        &[
            "ws_secret=0",
            "secret=0",
            "env=0",
            "ls=0",
            "ws=0",
            "scratch=0",
            "listing-canary-649",
        ],
    );
}

/// #651: a database glob below a folder reached through a link whose real
/// location holds glob syntax (`link -> db[1]`) still denies the databases
/// there: the resolved folder is escaped, not read as a character class.
#[test]
fn glob_below_a_link_to_a_folder_with_glob_syntax_is_escaped() {
    let dir = TempDir::new().expect("tempdir");
    let base = dir.path().canonicalize().expect("canonical");
    let real = base.join("db[1]");
    fs::create_dir_all(&real).expect("real");
    fs::write(real.join("x.sqlite"), "secret-651\n").expect("db");
    fs::write(real.join("notes.txt"), "notes-651\n").expect("notes");
    let link = base.join("link");
    std::os::unix::fs::symlink(&real, &link).expect("link");
    let policy = FileSystemSandboxPolicy::restricted(vec![
        root_read_entry(),
        FileSystemSandboxEntry {
            path: FileSystemPath::GlobPattern {
                scan: None,
                pattern: format!("{}/*.sqlite*", link.display()),
            },
            access: FileSystemAccessMode::Deny,
            missing_path_behavior: None,
        },
    ]);
    let script = format!(
        "cat '{real}/x.sqlite'; echo db=$?; cat '{link}/x.sqlite'; echo link=$?; \
         cat '{real}/notes.txt'; echo notes=$?",
        real = real.display(),
        link = link.display(),
    );
    let stdout = run_in_seatbelt(&policy, &base, &script);
    assert_lines(&stdout, &["db=1", "link=1", "notes=0"]);
    assert!(!stdout.contains("secret-651"), "{stdout}");
}

/// A disposable keychain file with one item, made and deleted with `HOME`
/// and the preferences folder (`CFFIXED_USER_HOME`) pointed at its own
/// folder, so the search list and login keychain of the person running the
/// test are never touched; every query names the file (#758).
struct DisposableKeychain {
    folder: TempDir,
    path: PathBuf,
}

impl DisposableKeychain {
    const SERVICE: &str = "corbanu-probe-758";
    const SECRET: &str = "keychain-canary-758";

    fn security(folder: &Path, args: &[&str]) -> std::process::Output {
        Command::new("/usr/bin/security")
            .args(args)
            .env("HOME", folder)
            .env("CFFIXED_USER_HOME", folder)
            .output()
            .expect("run /usr/bin/security")
    }

    fn create() -> Self {
        let folder = TempDir::new().expect("tempdir");
        let path = folder.path().join("probe-758.keychain-db");
        let keychain = path.to_str().expect("UTF-8 path");
        for args in [
            vec!["create-keychain", "-p", "disposable-758", keychain],
            vec!["unlock-keychain", "-p", "disposable-758", keychain],
            vec![
                "add-generic-password",
                "-A",
                "-s",
                Self::SERVICE,
                "-a",
                "kc-758",
                "-w",
                Self::SECRET,
                keychain,
            ],
        ] {
            let output = Self::security(folder.path(), &args);
            assert!(
                output.status.success(),
                "security {}: {}",
                args[0],
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Self { folder, path }
    }

    fn find_argv(&self) -> Vec<String> {
        [
            "/usr/bin/security",
            "find-generic-password",
            "-s",
            Self::SERVICE,
            "-w",
            self.path.to_str().expect("UTF-8 path"),
        ]
        .map(String::from)
        .to_vec()
    }

    /// What `security find-generic-password` prints under `args`
    /// (`sandbox-exec` arguments ending with the command).
    fn find_in(&self, args: &[String]) -> String {
        let output = Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
            .args(args)
            .env("HOME", self.folder.path())
            .env("CFFIXED_USER_HOME", self.folder.path())
            .output()
            .expect("run seatbelt");
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }
}

impl Drop for DisposableKeychain {
    fn drop(&mut self) {
        let keychain = self.path.to_str().unwrap_or_default();
        let _ = Self::security(self.folder.path(), &["delete-keychain", keychain]);
    }
}

/// #758: a keychain-services entry denies the keychain services after
/// every other rule: in each branch whose network rules allow
/// `com.apple.SecurityServer` (full network, a managed network's proxy
/// ports, an allowed Unix socket) the item is found without the entry and
/// not with it; with network off it is never found. Real `sandbox-exec`,
/// disposable keychain.
#[test]
fn keychain_services_entry_closes_the_keychain_in_every_network_branch() {
    let keychain = DisposableKeychain::create();
    let outside = DisposableKeychain::security(
        keychain.folder.path(),
        &keychain.find_argv()[1..]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    assert_eq!(
        String::from_utf8_lossy(&outside.stdout).trim(),
        DisposableKeychain::SECRET,
        "the item is there outside the sandbox: {}",
        String::from_utf8_lossy(&outside.stderr)
    );
    let cwd = keychain.folder.path();
    let managed_network = ManagedNetworkSandboxContext {
        loopback_ports: vec![43123],
        allow_local_binding: false,
    };
    let socket = vec![absolute_path("/tmp/codex-kc-758.sock")];
    struct Branch<'a> {
        name: &'a str,
        network: NetworkSandboxPolicy,
        enforce_managed_network: bool,
        managed_network: Option<&'a ManagedNetworkSandboxContext>,
        unix_sockets: &'a [AbsolutePathBuf],
        opens_keychain: bool,
    }
    let branches = [
        Branch {
            name: "network off",
            network: NetworkSandboxPolicy::Restricted,
            enforce_managed_network: false,
            managed_network: None,
            unix_sockets: &[],
            opens_keychain: false,
        },
        Branch {
            name: "managed network proxy",
            network: NetworkSandboxPolicy::Restricted,
            enforce_managed_network: true,
            managed_network: Some(&managed_network),
            unix_sockets: &[],
            opens_keychain: true,
        },
        Branch {
            name: "allowed Unix socket",
            network: NetworkSandboxPolicy::Restricted,
            enforce_managed_network: false,
            managed_network: None,
            unix_sockets: &socket,
            opens_keychain: true,
        },
        Branch {
            name: "full network",
            network: NetworkSandboxPolicy::Enabled,
            enforce_managed_network: false,
            managed_network: None,
            unix_sockets: &[],
            opens_keychain: true,
        },
    ];
    for branch in branches {
        for deny in [false, true] {
            let mut policy = FileSystemSandboxPolicy::restricted(vec![root_read_entry()]);
            if deny {
                policy
                    .entries
                    .push(FileSystemSandboxEntry::deny_keychain_services());
            }
            let args = create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
                command: keychain.find_argv(),
                file_system_sandbox_policy: &policy,
                network_sandbox_policy: branch.network,
                sandbox_policy_cwd: cwd,
                enforce_managed_network: branch.enforce_managed_network,
                managed_network: branch.managed_network,
                environment_id: None,
                network: None,
                extra_allow_unix_sockets: branch.unix_sockets,
            })
            .expect("seatbelt args");
            let text = seatbelt_policy_arg(&args);
            assert_eq!(
                text.trim_end()
                    .ends_with(super::MACOS_KEYCHAIN_SERVICES_DENY_POLICY),
                deny,
                "{}: the deny section comes last:\n{text}",
                branch.name
            );
            let found = keychain.find_in(&args);
            assert_eq!(
                found.contains(DisposableKeychain::SECRET),
                branch.opens_keychain && !deny,
                "{} (entry: {deny}): {found}",
                branch.name
            );
        }
    }
}

/// #758: without the entry the keychain services are not denied, and the
/// network rules keep `com.apple.SecurityServer` (Permissive and Moderate
/// unchanged).
#[test]
fn keychain_services_are_denied_only_with_the_entry() {
    let policy = FileSystemSandboxPolicy::restricted(vec![root_read_entry()]);
    let args = create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
        command: vec!["/usr/bin/true".to_string()],
        file_system_sandbox_policy: &policy,
        network_sandbox_policy: NetworkSandboxPolicy::Enabled,
        sandbox_policy_cwd: Path::new("/"),
        enforce_managed_network: false,
        managed_network: None,
        environment_id: None,
        network: None,
        extra_allow_unix_sockets: &[],
    })
    .expect("seatbelt args");
    let text = seatbelt_policy_arg(&args);
    assert!(text.contains("(global-name \"com.apple.SecurityServer\")"));
    assert!(!text.contains("(deny mach-lookup"), "{text}");
}
