use std::collections::BTreeMap;
use std::collections::HashMap;
#[cfg(unix)]
use std::path::PathBuf;

use codex_protocol::config_types::ShellEnvironmentPolicyFilter;
use pretty_assertions::assert_eq;

use codex_protocol::config_types::SandboxMode;
use codex_protocol::permissions::credential_file_globs;

use super::*;
use crate::legacy_core::config::ConfigBuilder;
use codex_config::LoaderOverrides;

async fn load(home: &Path, cwd: &Path, user_config: &str) -> Config {
    load_with(home, cwd, user_config, base_overrides(home, home)).await
}

async fn load_with(
    home: &Path,
    cwd: &Path,
    user_config: &str,
    mut cli: Vec<(String, toml::Value)>,
) -> Config {
    std::fs::write(home.join("config.toml"), user_config).unwrap();
    cli.extend(env_overrides(&ShellEnvironmentPolicyToml::default()));
    let mut harness = ConfigOverrides {
        cwd: Some(cwd.to_path_buf()),
        ..Default::default()
    };
    apply_launch_overrides(&mut harness);
    ConfigBuilder::default()
        .codex_home(home.to_path_buf())
        .cli_overrides(cli)
        .harness_overrides(harness)
        .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
        .build()
        .await
        .unwrap()
}

fn names(overrides: &[(String, toml::Value)]) -> Vec<String> {
    overrides.iter().map(|(key, _)| key.clone()).collect()
}

#[test]
fn env_overrides_extend_legacy_excludes_and_blank_secret_sets() {
    let user = ShellEnvironmentPolicyToml {
        exclude: Some(vec!["AWS_*".to_string(), "*VAULT*".to_string()]),
        r#set: Some(HashMap::from([
            ("PATH_EXTRA".to_string(), "/opt/bin".to_string()),
            ("DEPLOY_TOKEN".to_string(), "value".to_string()),
        ])),
        ..Default::default()
    };
    let exclude = [
        "AWS_*",
        "*VAULT*",
        "*PASSWORD*",
        "*PASSPHRASE*",
        "*CREDENTIAL*",
    ]
    .map(|pattern| toml::Value::String(pattern.to_string()));
    assert_eq!(
        env_overrides(&user),
        vec![
            (
                "shell_environment_policy.exclude".to_string(),
                toml::Value::Array(exclude.to_vec())
            ),
            (
                "shell_environment_policy.set.DEPLOY_TOKEN".to_string(),
                toml::Value::String(String::new())
            ),
        ]
    );
}

#[test]
fn env_overrides_keep_canonical_filters() {
    let user = ShellEnvironmentPolicyToml {
        filters: Some(BTreeMap::from([(
            "AWS_*".to_string(),
            ShellEnvironmentPolicyFilter::Exclude,
        )])),
        ..Default::default()
    };
    assert_eq!(
        names(&env_overrides(&user)),
        SECRET_ENV_PATTERNS
            .map(|pattern| format!("shell_environment_policy.filters.{pattern}"))
            .to_vec()
    );
}

#[test]
fn launch_flags_cannot_weaken_aggressive() {
    let mut overrides = ConfigOverrides {
        approval_policy: Some(AskForApproval::Never),
        sandbox_mode: Some(SandboxMode::DangerFullAccess),
        additional_writable_roots: vec!["/elsewhere".into()],
        default_permissions: Some(":danger-full-access".to_string()),
        ..Default::default()
    };
    assert_eq!(
        apply_launch_overrides(&mut overrides),
        vec!["--ask-for-approval", "--sandbox", "--add-dir"]
    );
    assert_eq!(
        (
            overrides.approval_policy,
            overrides.approvals_reviewer,
            overrides.sandbox_mode,
            overrides.default_permissions.as_deref(),
            overrides.additional_writable_roots,
        ),
        (
            Some(AskForApproval::UnlessTrusted),
            Some(ApprovalsReviewer::User),
            None,
            Some(PROFILE_ID),
            Vec::new(),
        )
    );
}

/// A permissive user config plus the overlay loads as every mapping row;
/// without the overlay the same config fails verification.
#[tokio::test]
async fn overlay_on_permissive_user_config_verifies_every_row() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        r#"approval_policy = "never"
approvals_reviewer = "auto_review"
sandbox_mode = "danger-full-access"
web_search = "live"

[sandbox_workspace_write]
writable_roots = ["/elsewhere"]
network_access = true

[shell_environment_policy]
set = { MY_API_KEY = "abc" }
"#,
    )
    .unwrap();
    let build = |cli: Vec<(String, toml::Value)>, harness: ConfigOverrides| {
        ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
            .cli_overrides(cli)
            .harness_overrides(ConfigOverrides {
                cwd: Some(cwd.path().to_path_buf()),
                ..harness
            })
            .build()
    };
    let plain = build(Vec::new(), ConfigOverrides::default()).await.unwrap();
    let rows = verify(
        &plain,
        /*rules_present*/ false,
        plain.codex_home.as_path(),
    )
    .iter()
    .filter_map(|failure| failure.split(':').next().map(str::to_string))
    .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        rows,
        ["Approvals", "Child agents", "Network", "Sandbox", "Vault"]
            .map(str::to_string)
            .into()
    );

    let user_env = ShellEnvironmentPolicyToml {
        r#set: Some(HashMap::from([(
            "MY_API_KEY".to_string(),
            "abc".to_string(),
        )])),
        ..Default::default()
    };
    let mut cli = base_overrides(home.path(), home.path());
    cli.extend(env_overrides(&user_env));
    let mut harness = ConfigOverrides::default();
    apply_launch_overrides(&mut harness);
    let config = build(cli, harness).await.unwrap();
    assert_eq!(
        verify(
            &config,
            /*rules_present*/ true,
            config.codex_home.as_path()
        ),
        Vec::<String>::new()
    );
    assert_eq!(
        verify(
            &config,
            /*rules_present*/ false,
            config.codex_home.as_path()
        ),
        vec!["Vault: the exec-policy rule file is missing".to_string()]
    );
}

/// A workspace that contains the Corbanu home still cannot rewrite the level.
#[tokio::test]
async fn codex_home_inside_the_workspace_stays_read_only() {
    let cwd = tempfile::tempdir().unwrap();
    let home = cwd.path().join(".corbanu");
    std::fs::create_dir_all(&home).unwrap();
    let config = load(&home, cwd.path(), "").await;
    assert_eq!(
        verify(
            &config,
            /*rules_present*/ true,
            config.codex_home.as_path()
        ),
        Vec::<String>::new()
    );
}

#[cfg(unix)]
fn isolation_problems(config: &Config, path: &Path) -> Vec<String> {
    crate::security::preflight::verify_isolation(config, &[path.to_path_buf()])
}

/// #467: a workspace opened through `/tmp` on macOS and denied by its
/// `/private/tmp` path (as launch does from `current_dir()`), or the other
/// way round, is isolated; a path no rule denies is still reported.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn isolation_is_verified_through_tmp_on_macos() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let logical = root.path().to_path_buf();
    let physical = logical.canonicalize().unwrap();
    assert!(physical.starts_with("/private/tmp"), "{physical:?}");
    std::fs::write(physical.join(".env"), "API_KEY=x").unwrap();
    let home = tempfile::tempdir().unwrap();
    for (denied_in, opened_in) in [(&physical, &logical), (&logical, &physical)] {
        let mut cli = base_overrides(home.path(), home.path());
        deny_reads(&mut cli, &[denied_in.join(".env")]);
        let config = load_with(home.path(), opened_in, "", cli).await;
        for spelling in [&logical, &physical] {
            assert_eq!(
                isolation_problems(&config, &spelling.join(".env")),
                Vec::<String>::new(),
                "{} denied, opened in {}",
                denied_in.display(),
                opened_in.display()
            );
            assert_eq!(
                isolation_problems(&config, &spelling.join("other.key")).len(),
                1,
                "other.key is not denied"
            );
        }
    }
}

/// A real `…/real` and a nested `…/link -> real`, with a `.env` and an
/// `other` file in it.
#[cfg(unix)]
fn nested_link_workspace(root: &Path) -> (PathBuf, PathBuf) {
    let real = root.join("real");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(real.join(".env"), "API_KEY=x").unwrap();
    std::fs::write(real.join("other"), "ok").unwrap();
    let link = root.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    (real, link)
}

/// Off macOS the check matches a denial by its spelling with nested links
/// kept, so a file spelled through a nested link that the rule names
/// without it is reported rather than claimed.
#[cfg(all(unix, not(target_os = "macos")))]
#[tokio::test]
async fn isolation_through_a_nested_link_is_still_reported() {
    let root = tempfile::tempdir().unwrap();
    let (real, link) = nested_link_workspace(root.path());
    let home = tempfile::tempdir().unwrap();
    let mut cli = base_overrides(home.path(), home.path());
    deny_reads(&mut cli, &[real.join(".env")]);
    let config = load_with(home.path(), &link, "", cli).await;
    assert_eq!(isolation_problems(&config, &link.join(".env")).len(), 1);
    assert_eq!(isolation_problems(&config, &real.join(".env")).len(), 0);
}

/// #488: in a workspace opened through a nested link, the Aggressive check's
/// verdict on each spelling of a denied file is exactly what the generated
/// Seatbelt clauses enforce, and agent commands cannot read it.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn isolation_check_matches_seatbelt_through_a_nested_link() {
    use codex_protocol::permissions::NetworkSandboxPolicy;
    use codex_sandboxing::seatbelt::CreateSeatbeltCommandArgsParams;
    use codex_sandboxing::seatbelt::MACOS_PATH_TO_SEATBELT_EXECUTABLE;
    use codex_sandboxing::seatbelt::create_seatbelt_command_args;

    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let (real, link) = nested_link_workspace(&root);
    let home = tempfile::tempdir().unwrap();
    for (denied, opened_in) in [(&real, &link), (&link, &real), (&real, &real)] {
        let mut cli = base_overrides(home.path(), home.path());
        deny_reads(&mut cli, &[denied.join(".env")]);
        let config = load_with(home.path(), opened_in, "", cli).await;
        let policy = config.permissions.file_system_sandbox_policy();
        let cwd = config.cwd.as_path();
        let args = |command: &str| {
            create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
                command: vec!["/bin/sh".to_string(), "-c".to_string(), command.to_string()],
                file_system_sandbox_policy: &policy,
                network_sandbox_policy: NetworkSandboxPolicy::Restricted,
                sandbox_policy_cwd: cwd,
                enforce_managed_network: false,
                managed_network: None,
                environment_id: None,
                network: None,
                extra_allow_unix_sockets: &[],
            })
            .unwrap()
        };
        // The read clauses, read back from the generated arguments.
        let generated = args("true");
        let params: BTreeMap<&str, PathBuf> = generated
            .iter()
            .filter_map(|arg| arg.strip_prefix("-D")?.split_once('='))
            .map(|(key, value)| (key, PathBuf::from(value)))
            .collect();
        let clauses: Vec<(PathBuf, Vec<PathBuf>)> = (0..)
            .map_while(|index| {
                let key = format!("READABLE_ROOT_{index}");
                let root = params.get(key.as_str())?.clone();
                let excluded = (0..)
                    .map_while(|n| params.get(format!("{key}_EXCLUDED_{n}").as_str()).cloned())
                    .collect();
                Some((root, excluded))
            })
            .collect();
        assert!(!clauses.is_empty(), "{generated:#?}");
        let label = format!(
            "{} denied, opened in {}",
            denied.display(),
            opened_in.display()
        );
        for dir in [&real, &link] {
            for name in [".env", "other"] {
                let path = dir.join(name);
                let seen = path.canonicalize().unwrap();
                let readable = clauses.iter().any(|(root, excluded)| {
                    seen.starts_with(root) && !excluded.iter().any(|e| seen.starts_with(e))
                });
                assert_eq!(readable, name == "other", "{label}: {path:?}");
                assert_eq!(
                    isolation_problems(&config, &path).len(),
                    usize::from(readable),
                    "{label}: {path:?}"
                );
            }
        }
        // And the sandbox enforces them.
        let script = format!(
            "cat .env; echo dot=$?; cat '{}/.env'; echo real=$?; cat '{}/.env'; echo link=$?; \
             cat other; echo other=$?",
            real.display(),
            link.display()
        );
        let output = std::process::Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
            .args(args(&script))
            .current_dir(opened_in)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(!stdout.contains("API_KEY"), "{label}: {stdout}");
        for line in ["dot=1", "real=1", "link=1", "other=0"] {
            assert!(stdout.contains(line), "{label}: {line} not in {stdout}");
        }
    }
}

/// A workspace that is the account home cannot delete the Aggressive-homes
/// registry's entries, and a profile that would let it fails verification.
#[tokio::test]
async fn registry_inside_the_workspace_stays_read_only() {
    let account = tempfile::tempdir().unwrap();
    let cwd = account.path();
    let registry = cwd.join("Library/Application Support/Corbanu/aggressive-homes");
    std::fs::create_dir_all(&registry).unwrap();
    let home = tempfile::tempdir().unwrap();
    let entry = registry.join("entry");
    let cli = base_overrides_with_registry(home.path(), home.path(), Some(&registry));

    let config = load_with(home.path(), cwd, "", cli.clone()).await;
    let file_system = config.permissions.file_system_sandbox_policy();
    assert!(!file_system.can_write_path_with_cwd(&entry, cwd));
    assert!(!file_system.can_write_path_with_cwd(&registry, cwd));
    assert!(file_system.can_read_path_with_cwd(&entry, cwd));
    assert!(file_system.can_write_path_with_cwd(&cwd.join("other"), cwd));
    let writable_roots = file_system.get_writable_roots_with_cwd(cwd);
    assert!(
        writable_roots.iter().any(
            |root| root
                .read_only_subpaths
                .iter()
                .any(|path| path.as_path() == registry
                    || path.as_path()
                        == codex_utils_absolute_path::canonicalize_preserving_symlinks(&registry)
                            .unwrap())
        ),
        "{writable_roots:?}"
    );
    let origin = config.codex_home.to_path_buf();
    assert_eq!(
        verify_with_registry(
            &config,
            /*rules_present*/ true,
            &origin,
            Some(&registry)
        ),
        Vec::<String>::new()
    );

    // Without the entry, verification names the registry.
    let registry_key = registry.to_str().unwrap().to_string();
    let mut without = cli;
    for (key, value) in &mut without {
        if key == &format!("permissions.{PROFILE_ID}") {
            value
                .get_mut("filesystem")
                .and_then(toml::Value::as_table_mut)
                .unwrap()
                .remove(&registry_key)
                .unwrap();
        }
    }
    let config = load_with(home.path(), cwd, "", without).await;
    assert_eq!(
        verify_with_registry(
            &config,
            /*rules_present*/ true,
            &origin,
            Some(&registry)
        ),
        vec![format!(
            "Sandbox: the Aggressive-homes registry {} is writable",
            registry.display()
        )]
    );
}

/// A registry the workspace contains must exist (Linux would put a
/// placeholder there), and a path the profile cannot express is reported.
#[tokio::test]
async fn missing_or_unexpressible_registry_fails_verification() {
    let account = tempfile::tempdir().unwrap();
    let cwd = account.path();
    let home = tempfile::tempdir().unwrap();
    let missing = cwd.join("Library/Application Support/Corbanu/aggressive-homes");
    let config = load_with(
        home.path(),
        cwd,
        "",
        base_overrides_with_registry(home.path(), home.path(), Some(&missing)),
    )
    .await;
    let origin = config.codex_home.to_path_buf();
    assert_eq!(
        verify_with_registry(
            &config,
            /*rules_present*/ true,
            &origin,
            Some(&missing)
        ),
        vec![format!(
            "Sandbox: the Aggressive-homes registry {} is missing",
            missing.display()
        )]
    );

    let globbed = cwd.join("glob[1]/aggressive-homes");
    std::fs::create_dir_all(&globbed).unwrap();
    let cli = base_overrides_with_registry(home.path(), home.path(), Some(&globbed));
    assert!(
        !cli.iter()
            .any(|(_, value)| value.to_string().contains("glob[1]")),
        "a glob path must not become a profile key"
    );
    let config = load_with(home.path(), cwd, "", cli).await;
    assert_eq!(
        verify_with_registry(
            &config,
            /*rules_present*/ true,
            &origin,
            Some(&globbed)
        ),
        vec![format!(
            "Sandbox: the Aggressive-homes registry path {} cannot be written into the profile (not UTF-8, or it contains glob characters)",
            globbed.display()
        )]
    );
}

/// A nested `corbanu exec` held to Aggressive protects the registry too.
#[test]
fn nested_exec_overrides_keep_the_registry_read_only() {
    let home = tempfile::tempdir().unwrap();
    let registry = home.path().join("registry");
    let cli = base_overrides_with_registry(home.path(), home.path(), Some(&registry));
    let profile = cli
        .iter()
        .find(|(key, _)| key == &format!("permissions.{PROFILE_ID}"))
        .map(|(_, value)| value.clone())
        .unwrap();
    assert_eq!(
        profile
            .get("filesystem")
            .and_then(|filesystem| filesystem.get(registry.to_str().unwrap())),
        Some(&toml::Value::String("read".to_string()))
    );
}

/// User or project layers cannot widen the profile through a table merge.
#[tokio::test]
async fn another_layer_defining_the_profile_fails_verification() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let config = load(
        home.path(),
        cwd.path(),
        "[permissions.corbanu-aggressive.network]\nenabled = true\n",
    )
    .await;
    let failures = verify(
        &config,
        /*rules_present*/ true,
        config.codex_home.as_path(),
    );
    assert!(
        failures
            .iter()
            .any(|failure| failure.contains("also defines permissions.corbanu-aggressive")),
        "{failures:?}"
    );
}

/// A custom role that would hand children different values is refused.
#[tokio::test]
async fn role_that_changes_child_values_fails_verification() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents")).unwrap();
    std::fs::write(
        home.path().join("agents/loose.toml"),
        "web_search = \"live\"\n[features]\nshell_snapshot = true\n",
    )
    .unwrap();
    std::fs::write(
        home.path().join("agents/plain.toml"),
        "developer_instructions = \"Be brief\"\n",
    )
    .unwrap();
    let config = load(
        home.path(),
        cwd.path(),
        "[agents.loose]\ndescription = \"x\"\nconfig_file = \"./agents/loose.toml\"\n[agents.plain]\ndescription = \"y\"\nconfig_file = \"./agents/plain.toml\"\n",
    )
    .await;
    assert_eq!(
        verify(
            &config,
            /*rules_present*/ true,
            config.codex_home.as_path()
        ),
        vec!["Child agents: role `loose` sets web_search, features.shell_snapshot".to_string()]
    );
}

/// A role cannot hand children lenient rule parsing.
#[tokio::test]
async fn role_that_relaxes_strict_rules_fails_verification() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents")).unwrap();
    std::fs::write(
        home.path().join("agents/lenient.toml"),
        "strict_rules = false\n",
    )
    .unwrap();
    let config = load(
        home.path(),
        cwd.path(),
        "[agents.lenient]\ndescription = \"x\"\nconfig_file = \"./agents/lenient.toml\"\n",
    )
    .await;
    assert_eq!(
        verify(
            &config,
            /*rules_present*/ true,
            config.codex_home.as_path()
        ),
        vec!["Child agents: role `lenient` sets strict_rules".to_string()]
    );
}

/// The launch check loads the whole exec policy the way a session does.
#[tokio::test]
async fn exec_policy_must_load_and_forbid_vault_commands() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    super::super::level::sync_rules(home.path(), super::super::level::ChosenLevel::Aggressive)
        .unwrap();
    let rules = home.path().join("rules");
    let config = load(home.path(), cwd.path(), "").await;
    assert_eq!(verify_exec_policy(&config).await, Vec::<String>::new());

    // A broken file next to the vault rule: a session would drop every rule.
    let broken = rules.join("mine.rules");
    std::fs::write(&broken, "prefix_rule(pattern = [\"git\"], decision = \n").unwrap();
    let failures = verify_exec_policy(&config).await;
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].starts_with(&format!(
            "Vault: an exec-policy rules file does not parse, so sessions would drop every rule, including the vault rule: {}:",
            broken.display()
        )),
        "{failures:?}"
    );
    std::fs::remove_file(&broken).unwrap();

    // `host_executable` would make the rule skip `corbanu` at other paths.
    // The path must be absolute on this platform (Starlark-escaped).
    let elsewhere = if cfg!(windows) {
        r"C:\\opt\\elsewhere\\corbanu.exe"
    } else {
        "/opt/elsewhere/corbanu"
    };
    std::fs::write(
        rules.join("paths.rules"),
        format!("host_executable(name = \"corbanu\", paths = [\"{elsewhere}\"])\n"),
    )
    .unwrap();
    assert_eq!(
        verify_exec_policy(&config).await,
        vec![
            "Vault: host_executable rules limit `corbanu` to listed paths, so the vault rule would not apply at other paths"
                .to_string()
        ]
    );
    std::fs::remove_file(rules.join("paths.rules")).unwrap();

    std::fs::remove_file(super::super::level::rules_path(home.path())).unwrap();
    assert_eq!(
        verify_exec_policy(&config).await,
        vec![
            "Vault: the loaded exec policy does not forbid `corbanu vault`, `codex vault`, `pfterminal vault`, `corbanu-debug vault`, `pfterminal-debug vault`"
                .to_string()
        ]
    );
}

/// #441 (milestone AGG-19): with nested agents set to refuse, an agent's
/// `corbanu exec` or `corbanu review` is forbidden by the session's exec
/// policy, so it is refused before any approval prompt. With "pass" it is
/// not forbidden: it runs with Aggressive enforced once a person approves.
#[tokio::test]
async fn issue_441_nested_agent_launches_are_forbidden_before_approval_when_set_to_refuse() {
    use super::super::level::ChosenLevel;
    use super::super::level::NestedAgents;
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let decision = |policy: &codex_execpolicy::Policy, command: &[&str]| {
        let command = command.iter().map(ToString::to_string).collect::<Vec<_>>();
        policy.check(&command, &|_| Decision::Prompt).decision
    };
    let nested = [
        vec!["corbanu", "exec", "say hi"],
        vec!["corbanu", "review"],
        vec!["corbanu", "e", "say hi"],
        vec!["codex", "exec", "say hi"],
        vec!["pfterminal", "review"],
    ];

    super::super::level::save(home.path(), ChosenLevel::Aggressive, NestedAgents::Refuse).unwrap();
    let config = load(home.path(), cwd.path(), "").await;
    let policy = load_exec_policy(&config.config_layer_stack).await.unwrap();
    for command in &nested {
        assert_eq!(
            decision(&policy, command),
            Decision::Forbidden,
            "{command:?}"
        );
    }
    // Other `corbanu` commands are untouched.
    assert_eq!(
        decision(&policy, &["corbanu", "--version"]),
        Decision::Prompt
    );
    assert_eq!(verify_exec_policy(&config).await, Vec::<String>::new());

    super::super::level::save(home.path(), ChosenLevel::Aggressive, NestedAgents::Pass).unwrap();
    let config = load(home.path(), cwd.path(), "").await;
    let policy = load_exec_policy(&config.config_layer_stack).await.unwrap();
    for command in &nested {
        assert_eq!(decision(&policy, command), Decision::Prompt, "{command:?}");
    }
    assert_eq!(
        decision(&policy, &["corbanu", "vault", "list"]),
        Decision::Forbidden
    );
}

/// New threads must fail rather than drop the vault rule when a `.rules` file
/// breaks after launch; a user `strict_rules = false` cannot turn that off.
#[tokio::test]
async fn strict_rules_is_forced_and_verified() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let config = load(home.path(), cwd.path(), "strict_rules = false\n").await;
    assert!(config.strict_rules);
    assert_eq!(
        verify(
            &config,
            /*rules_present*/ true,
            config.codex_home.as_path()
        ),
        Vec::<String>::new()
    );

    let cli = base_overrides(home.path(), home.path())
        .into_iter()
        .filter(|(key, _)| key != "strict_rules")
        .collect::<Vec<_>>();
    let mut harness = ConfigOverrides {
        cwd: Some(cwd.path().to_path_buf()),
        ..Default::default()
    };
    apply_launch_overrides(&mut harness);
    let lenient = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .cli_overrides(cli)
        .harness_overrides(harness)
        .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
        .build()
        .await
        .unwrap();
    assert!(
        verify(
            &lenient,
            /*rules_present*/ true,
            lenient.codex_home.as_path()
        )
        .contains(
            &"Vault: a rules file that breaks later would let new threads drop the vault rule"
                .to_string()
        )
    );
}

/// A broken `.rules` file in the project's `.codex/rules` counts only when the
/// project is trusted, exactly as a session loads it.
#[tokio::test]
async fn project_rules_count_only_when_the_project_is_trusted() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    super::super::level::sync_rules(home.path(), super::super::level::ChosenLevel::Aggressive)
        .unwrap();
    let project_rules = cwd.path().join(".codex").join("rules");
    std::fs::create_dir_all(&project_rules).unwrap();
    std::fs::write(
        project_rules.join("broken.rules"),
        "prefix_rule(pattern = [\"git\"], decision = \n",
    )
    .unwrap();

    let untrusted = load(home.path(), cwd.path(), "").await;
    assert_eq!(verify_exec_policy(&untrusted).await, Vec::<String>::new());

    let trusted = load(
        home.path(),
        cwd.path(),
        &format!(
            "[projects.{:?}]\ntrust_level = \"trusted\"\n",
            cwd.path().display().to_string()
        ),
    )
    .await;
    let failures = verify_exec_policy(&trusted).await;
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(
        failures[0].contains("does not parse") && failures[0].contains("broken.rules"),
        "{failures:?}"
    );
}

/// A nested `corbanu exec` with nested agents set to pass, on a home other
/// than the origin: every row holds, and its commands can read neither vault
/// store, write outside the folder or reach the network.
#[tokio::test]
async fn nested_child_on_another_home_protects_both_homes() {
    let origin = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    let mut cli = base_overrides(home.path(), origin.path());
    cli.extend(env_overrides(&ShellEnvironmentPolicyToml::default()));
    let mut harness = ConfigOverrides {
        cwd: Some(cwd.clone()),
        sandbox_mode: Some(SandboxMode::DangerFullAccess),
        bypass_hook_trust: Some(true),
        ..Default::default()
    };
    assert_eq!(
        apply_launch_overrides(&mut harness),
        vec!["--sandbox", "--dangerously-bypass-hook-trust"]
    );
    let config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .cli_overrides(cli)
        .harness_overrides(harness)
        .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
        .build()
        .await
        .unwrap();
    assert_eq!(
        verify(&config, /*rules_present*/ true, origin.path()),
        Vec::<String>::new()
    );
    let file_system = config.permissions.file_system_sandbox_policy();
    for vault in [origin.path(), home.path()] {
        assert!(!file_system.can_read_path_with_cwd(&vault.join("secrets"), &cwd));
        assert!(!file_system.can_write_path_with_cwd(&vault.join("security_level.toml"), &cwd));
    }
    assert!(!file_system.can_write_path_with_cwd(&root.path().join("outside.txt"), &cwd));
    assert!(!config.permissions.network_sandbox_policy().is_enabled());
    assert!(!config.bypass_hook_trust);
    // A different marker fails verification.
    assert!(
        verify(&config, /*rules_present*/ true, home.path())
            .iter()
            .any(|failure| failure.starts_with("Child agents:"))
    );
}

/// The profile entry that denies credential names below the workspace.
fn without_credential_names(cli: &mut [(String, toml::Value)]) {
    for (key, value) in cli.iter_mut() {
        if key == &format!("permissions.{PROFILE_ID}") {
            let filesystem = value
                .get_mut("filesystem")
                .and_then(toml::Value::as_table_mut)
                .unwrap();
            filesystem.remove(":workspace_roots").unwrap();
            filesystem.retain(|key, _| !key.starts_with("/**/"));
        }
    }
}

/// #442 (Travis, 2026-10-11): the launch check fails when credential files
/// created later would not be denied by name.
#[tokio::test]
async fn issue_442_launch_check_requires_credential_names() {
    let cwd = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let config = load(home.path(), cwd.path(), "").await;
    let origin = config.codex_home.to_path_buf();
    assert_eq!(
        verify(&config, /*rules_present*/ true, &origin),
        Vec::<String>::new()
    );
    let globs = config
        .permissions
        .file_system_sandbox_policy()
        .get_unreadable_globs_with_cwd(config.cwd.as_path());
    for glob in credential_file_globs(&config.cwd.to_string_lossy()) {
        assert!(globs.contains(&glob), "{glob} not in {globs:?}");
    }

    let mut cli = base_overrides(home.path(), home.path());
    without_credential_names(&mut cli);
    let config = load_with(home.path(), cwd.path(), "", cli).await;
    assert_eq!(
        verify(&config, /*rules_present*/ true, &origin),
        vec![
            "Sandbox: credential files created later (.env, .env.* (except .env.example, .env.sample and .env.template) and .envrc) would be readable to agent commands"
                .to_string()
        ]
    );
}

/// Runs `script` in `cwd` under the Seatbelt arguments for `config`.
#[cfg(target_os = "macos")]
fn seatbelt_run(config: &Config, script: &str) -> String {
    use codex_protocol::permissions::NetworkSandboxPolicy;
    use codex_sandboxing::seatbelt::CreateSeatbeltCommandArgsParams;
    use codex_sandboxing::seatbelt::MACOS_PATH_TO_SEATBELT_EXECUTABLE;
    use codex_sandboxing::seatbelt::create_seatbelt_command_args;

    let policy = config.permissions.file_system_sandbox_policy();
    let args = create_seatbelt_command_args(CreateSeatbeltCommandArgsParams {
        command: vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
        file_system_sandbox_policy: &policy,
        network_sandbox_policy: NetworkSandboxPolicy::Restricted,
        sandbox_policy_cwd: config.cwd.as_path(),
        enforce_managed_network: false,
        managed_network: None,
        environment_id: None,
        network: None,
        extra_allow_unix_sockets: &[],
    })
    .unwrap();
    let output = std::process::Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
        .args(args)
        .current_dir(config.cwd.as_path())
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// #442 (Travis, 2026-10-11), real `sandbox-exec`: under the Aggressive
/// profile an agent command cannot create a credential file, and ones the
/// person creates after launch cannot be read (or overwritten) by the next
/// agent command, in the workspace or elsewhere; the `.env` the preflight
/// found keeps its explicit denial, and other files stay readable. Under
/// Permissive nothing changes.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn issue_442_credential_files_created_later_stay_unreadable_under_seatbelt() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let cwd = root.join("work");
    let elsewhere = root.join("other-project");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(cwd.join(".env"), "API_KEY=dummy-found-at-start").unwrap();
    std::fs::write(cwd.join("notes.txt"), "plain-notes").unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut cli = base_overrides(home.path(), home.path());
    deny_reads(&mut cli, &[cwd.join(".env")]);
    let config = load_with(home.path(), &cwd, "", cli).await;
    assert_eq!(
        verify(
            &config,
            /*rules_present*/ true,
            &config.codex_home.to_path_buf()
        ),
        Vec::<String>::new()
    );

    // An agent command cannot create credential files mid-session (so it
    // cannot plant a link the person's file would be written through).
    let made = seatbelt_run(
        &config,
        "mkdir -p app; printf API_KEY=dummy-made-by-agent > .env.local; echo local=$?; \
         printf API_KEY=dummy-made-by-agent > app/.env; echo app=$?; \
         ln -s notes.txt .envrc; echo link=$?",
    );
    for line in ["local=1", "app=1", "link=1"] {
        assert!(made.contains(line), "{line} not in {made}");
    }
    // The person creates them, here and in another project.
    std::fs::write(cwd.join(".env.local"), "API_KEY=dummy-made-by-person").unwrap();
    std::fs::write(cwd.join("app/.env"), "API_KEY=dummy-made-by-person").unwrap();
    std::fs::write(cwd.join(".envrc"), "export API_KEY=dummy-made-by-person").unwrap();
    std::fs::write(cwd.join(".env.production"), "API_KEY=dummy-made-by-person").unwrap();
    std::fs::write(elsewhere.join(".env"), "API_KEY=dummy-elsewhere").unwrap();
    // The next agent command can read none of them, in any spelling.
    let script = format!(
        "for f in .env .env.local app/.env .envrc .env.production '{}/.env' ./app/../.env.local; do \
         cat \"$f\"; echo \"$f=$?\"; done; cp .env.local copy; echo copy=$?; \
         ln .env.local hard; echo hard=$?; ln -s .env.local soft; cat soft; echo soft=$?; \
         mv .env.local moved; echo moved=$?; printf x > .env.production; echo over=$?; \
         cat notes.txt; echo notes=$?",
        elsewhere.display()
    );
    let read = seatbelt_run(&config, &script);
    assert!(!read.contains("dummy-"), "{read}");
    for line in [
        ".env=1",
        ".env.local=1",
        "app/.env=1",
        ".envrc=1",
        ".env.production=1",
        "other-project/.env=1",
        "copy=1",
        "soft=1",
        "moved=1",
        "over=1",
        "plain-notes",
        "notes=0",
    ] {
        assert!(read.contains(line), "{line} not in {read}");
    }
    assert!(!read.contains("hard=0"), "{read}");

    // Permissive is unchanged: no Aggressive overrides, nothing denied by name.
    let permissive = {
        std::fs::write(home.path().join("config.toml"), "").unwrap();
        ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .harness_overrides(ConfigOverrides {
                cwd: Some(cwd.clone()),
                ..Default::default()
            })
            .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
            .build()
            .await
            .unwrap()
    };
    assert!(
        permissive
            .permissions
            .file_system_sandbox_policy()
            .get_unreadable_globs_with_cwd(&cwd)
            .is_empty()
    );
    let read = seatbelt_run(&permissive, "cat .env.production; echo read=$?");
    assert!(read.contains("dummy-made-by-person"), "{read}");
}

/// #442: the launch check names a credential file made before the session
/// that the sandbox cannot protect (a link, extra hard links), and accepts
/// plain files and folders with such names.
#[cfg(unix)]
#[tokio::test]
async fn issue_442_launch_check_names_unsafe_credential_files() {
    let cwd = tempfile::tempdir().unwrap();
    let work = cwd.path().canonicalize().unwrap();
    std::fs::create_dir(work.join("app")).unwrap();
    std::fs::write(work.join("notes.txt"), "").unwrap();
    std::fs::write(work.join(".env.development"), "").unwrap();
    std::fs::create_dir(work.join(".env")).unwrap();
    std::fs::write(work.join("app/.env.local"), "").unwrap();
    let home = tempfile::tempdir().unwrap();
    let config = load(home.path(), &work, "").await;
    let origin = config.codex_home.to_path_buf();
    assert_eq!(
        verify(&config, /*rules_present*/ true, &origin),
        Vec::<String>::new()
    );

    std::os::unix::fs::symlink("../notes.txt", work.join("app/.envrc")).unwrap();
    std::fs::hard_link(work.join("notes.txt"), work.join(".env.hard")).unwrap();
    let mut failures = verify(&config, /*rules_present*/ true, &origin);
    failures.sort();
    assert_eq!(failures.len(), 2, "{failures:?}");
    assert!(
        failures[0].contains(".env.hard has other hard links"),
        "{failures:?}"
    );
    // #442 (Travis, 2026-10-12): the refusal names the link, its target and
    // exactly how to fix it.
    assert!(
        failures[1].contains(&format!(
            "{} is a link to {}",
            work.join("app/.envrc").display(),
            work.join("notes.txt").display()
        )),
        "{failures:?}"
    );
    assert!(
        failures[1].contains("replace the link with a regular file"),
        "{failures:?}"
    );
    assert!(
        failures[1].contains("or move it out of the workspace"),
        "{failures:?}"
    );
    assert!(
        failures[0].contains("delete the other names"),
        "{failures:?}"
    );
    // The exposed target is named, with what to do about it.
    assert!(failures[1].contains("delete that file too"), "{failures:?}");
}

/// #442 (Travis, 2026-10-12): `.env.example`, `.env.sample` and
/// `.env.template` are not credential files: the launch check accepts them
/// as links, and the profile leaves them readable on every system.
#[cfg(unix)]
#[tokio::test]
async fn issue_442_credential_templates_are_exempt() {
    let cwd = tempfile::tempdir().unwrap();
    let work = cwd.path().canonicalize().unwrap();
    std::fs::write(work.join("notes.txt"), "").unwrap();
    std::os::unix::fs::symlink("notes.txt", work.join(".env.example")).unwrap();
    std::os::unix::fs::symlink("notes.txt", work.join(".ENV.Template")).unwrap();
    let home = tempfile::tempdir().unwrap();
    let config = load(home.path(), &work, "").await;
    let origin = config.codex_home.to_path_buf();
    assert_eq!(
        verify(&config, /*rules_present*/ true, &origin),
        Vec::<String>::new()
    );
    let matcher = codex_protocol::permissions::ReadDenyMatcher::new(
        &config.permissions.file_system_sandbox_policy(),
        &work,
    )
    .expect("deny rules");
    for template in [".env.example", "app/.env.sample", ".ENV.Template"] {
        assert!(!matcher.is_read_denied(&work.join(template)), "{template}");
    }
    for credential in [".env", "app/.env.local", ".envrc"] {
        assert!(
            matcher.is_read_denied(&work.join(credential)),
            "{credential}"
        );
    }
    // The real Aggressive profile under Seatbelt: a checkout or scaffold can
    // write a template, and the person's `.env` stays unreadable.
    #[cfg(target_os = "macos")]
    {
        std::fs::write(work.join(".env"), "API_KEY=dummy-442-person").unwrap();
        std::fs::write(work.join("notes.txt"), "TEMPLATE_KEY=").unwrap();
        let out = seatbelt_run(
            &config,
            "cat .env.example; echo example=$?; printf x > .env.sample; echo sample=$?; \
             cat .env; echo env=$?; printf x > .env.local; echo local=$?",
        );
        assert!(!out.contains("dummy-442"), "{out}");
        for line in ["TEMPLATE_KEY=example=0", "sample=0", "env=1", "local=1"] {
            assert!(out.contains(line), "{line} not in {out}");
        }
    }
}

/// #594 (D225): on Linux, Aggressive refuses a Corbanu home reached through a
/// link in the folder it lets commands write (at launch and for every later
/// folder, `launch::verify_reloaded`), and accepts the real folder given
/// directly or a folder that does not contain the link.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn issue_594_home_reached_through_a_writable_link_fails_verification() {
    use crate::security::home_link;
    let base = tempfile::tempdir().unwrap();
    let base = base.path().canonicalize().unwrap();
    let user = base.join("user");
    let real = base.join("dotfiles/corbanu");
    let project = base.join("project");
    for dir in [&user, &real, &project] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let link = user.join(".corbanu");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let refusal = home_link::aggressive_refusal(
        &real,
        &[home_link::ExposedLink {
            link: link.clone(),
            folder: user.clone(),
        }],
    );
    let failures = |config: &Config| verify(config, /*rules_present*/ true, &real);

    home_link::GIVEN_FOR_TEST.with(|given| *given.borrow_mut() = Some(link.clone()));
    let in_user = load(&real, &user, "").await;
    assert_eq!(failures(&in_user), vec![refusal]);
    let in_project = load(&real, &project, "").await;
    assert_eq!(failures(&in_project), Vec::<String>::new());

    home_link::GIVEN_FOR_TEST.with(|given| *given.borrow_mut() = Some(real.clone()));
    assert_eq!(failures(&in_user), Vec::<String>::new());
}
