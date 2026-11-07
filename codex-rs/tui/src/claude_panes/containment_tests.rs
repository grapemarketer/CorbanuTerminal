use std::collections::BTreeMap;

use pretty_assertions::assert_eq;

use super::*;

#[test]
fn launch_env_keeps_only_the_allowlist_without_proxies_then_adds_the_pane() {
    let inherited = [
        ("PATH", "/usr/bin"),
        ("HOME", "/Users/someone"),
        ("LANG", "en_US.UTF-8"),
        ("ANTHROPIC_API_KEY", "sk-ant-not-real"),
        ("ZAI_API_KEY", "zai-not-real"),
        ("GITHUB_TOKEN", "ghp-not-real"),
        ("CLAUDE_CODE_OAUTH_TOKEN", "oauth-not-real"),
        ("ANTHROPIC_BASE_URL", "https://example.invalid"),
        ("HTTPS_PROXY", "http://127.0.0.1:9999"),
        ("http_proxy", "http://127.0.0.1:9998"),
        ("NO_PROXY", "localhost"),
        ("WS_PROXY", "http://127.0.0.1:9997"),
        ("DYLD_INSERT_LIBRARIES", "/tmp/x.dylib"),
        ("CLAUDE_CONFIG_DIR", "/Users/someone/.claude"),
    ]
    .map(|(name, value)| (name.to_string(), value.to_string()));
    let pane = BTreeMap::from([
        (
            "HTTP_PROXY".to_string(),
            "http://127.0.0.1:4242".to_string(),
        ),
        ("CLAUDE_CONFIG_DIR".to_string(), "/state/config".to_string()),
        (
            "ANTHROPIC_AUTH_TOKEN".to_string(),
            "bridge-token".to_string(),
        ),
    ]);

    let mut env = launch_env(inherited, &pane).into_iter().collect::<Vec<_>>();
    env.sort();
    let expected = [
        ("ANTHROPIC_AUTH_TOKEN", "bridge-token"),
        ("CLAUDE_CONFIG_DIR", "/state/config"),
        ("HOME", "/Users/someone"),
        ("HTTP_PROXY", "http://127.0.0.1:4242"),
        ("LANG", "en_US.UTF-8"),
        ("PATH", "/usr/bin"),
    ]
    .map(|(name, value)| (name.to_string(), value.to_string()));
    assert_eq!(env, expected.to_vec());
}

#[test]
fn state_dir_is_outside_codex_home_and_keyed_by_home_and_pane() {
    let codex_home = Path::new("/tmp/corbanu-home-a");
    let first = state_dir(codex_home, "claude-1", /*state_root*/ None).unwrap();
    assert!(first.is_absolute());
    assert!(!first.starts_with(codex_home));
    assert!(first.ends_with("claude-1"));
    assert_ne!(
        first,
        state_dir(codex_home, "claude-2", /*state_root*/ None).unwrap()
    );
    assert_ne!(
        first,
        state_dir(
            Path::new("/tmp/corbanu-home-b"),
            "claude-1",
            /*state_root*/ None
        )
        .unwrap()
    );
}

#[test]
fn base_profile_writes_only_the_pane_folder_and_its_state_with_network_off() {
    let cwd = tempfile::tempdir().unwrap();
    let state_root = tempfile::tempdir().unwrap();
    let state = state_root.path().join("pane-a");
    std::fs::create_dir_all(state_root.path().join("pane-b/config")).unwrap();
    let containment = ClaudeContainment {
        state_dir: state.clone(),
        panes_dir: cwd.path().join("panes"),
        linux_sandbox_exe: None,
    };
    let profile = base_profile(&containment).unwrap();
    let (file_system, network) = profile.to_runtime_permissions();
    let cwd = cwd.path();
    assert!(!network.is_enabled());
    assert!(file_system.can_write_path_with_cwd(&cwd.join("file"), cwd));
    assert!(file_system.can_write_path_with_cwd(&state.join("config/x"), cwd));
    assert!(file_system.can_read_path_with_cwd(&state.join("config/x"), cwd));
    assert!(!file_system.can_read_path_with_cwd(&state_root.path().join("pane-b/config/x"), cwd));
    // The folder holding the panes' state stays readable: Claude Code
    // examines every component of its own paths.
    assert!(file_system.can_read_path_with_cwd(state_root.path(), cwd));
    // Corbanu's settings for the turn are read-only; other panes' records
    // are unreadable.
    let settings = containment.settings_path();
    assert!(file_system.can_read_path_with_cwd(&settings, cwd));
    assert!(!file_system.can_write_path_with_cwd(&settings, cwd));
    assert!(
        !file_system
            .can_read_path_with_cwd(&containment.panes_dir.join("other/turn-0001.jsonl"), cwd)
    );
    assert!(file_system.can_read_path_with_cwd(Path::new("/usr/bin/env"), cwd));
    for outside in ["/tmp/corbanu-contained-probe", "/usr/local/corbanu-probe"] {
        assert!(
            !file_system.can_write_path_with_cwd(Path::new(outside), cwd),
            "{outside}"
        );
    }
    assert!(!file_system.can_write_path_with_cwd(&cwd.join(".git/config"), cwd));
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn contained_launch_is_refused_without_the_secretless_contract() {
    if crate::legacy_core::external_agent_contract_armed() {
        return;
    }
    let cwd = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let containment = ClaudeContainment {
        state_dir: state.path().to_path_buf(),
        panes_dir: cwd.path().join("panes"),
        linux_sandbox_exe: None,
    };
    let err = contain(
        &containment,
        "claude",
        &["-p".to_string()],
        &BTreeMap::new(),
        cwd.path(),
        /*bridge_port*/ 4242,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("secretless_agent_launch"),
        "{err:#}"
    );
}

/// #758: a contained pane's sandbox (network only to its bridge port, which
/// selects Seatbelt's network rules) keeps the keychain services out of
/// reach under Aggressive and only there: a disposable keychain item (its
/// own `HOME`, never the login keychain or search list) is found under
/// Permissive and not under Aggressive. Real `sandbox-exec`.
#[cfg(target_os = "macos")]
#[test]
fn issue_758_contained_pane_reaches_no_keychain_under_aggressive() {
    use crate::security::level::ChosenLevel;
    use crate::security::level::test_levels;

    const SERVICE: &str = "corbanu-probe-758";
    const SECRET: &str = "keychain-canary-758";
    struct Keychain(tempfile::TempDir, String);
    impl Keychain {
        fn security(&self, args: &[&str]) -> std::process::Output {
            std::process::Command::new("/usr/bin/security")
                .args(args)
                .env("HOME", self.0.path())
                .env("CFFIXED_USER_HOME", self.0.path())
                .output()
                .expect("run /usr/bin/security")
        }
    }
    impl Drop for Keychain {
        fn drop(&mut self) {
            let _ = self.security(&["delete-keychain", &self.1]);
        }
    }
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("probe-758.keychain-db");
    let keychain = Keychain(folder, path.to_string_lossy().into_owned());
    let file = keychain.1.clone();
    for args in [
        vec!["create-keychain", "-p", "disposable-758", &file],
        vec!["unlock-keychain", "-p", "disposable-758", &file],
        vec![
            "add-generic-password",
            "-A",
            "-s",
            SERVICE,
            "-a",
            "kc-758",
            "-w",
            SECRET,
            &file,
        ],
    ] {
        let output = keychain.security(&args);
        assert!(output.status.success(), "security {}", args[0]);
    }
    let find = ["find-generic-password", "-s", SERVICE, "-w", &file];
    let outside = keychain.security(&find);
    assert_eq!(String::from_utf8_lossy(&outside.stdout).trim(), SECRET);

    let cwd = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    // A folder of its own: the profile denies each sibling of the state
    // folder (other panes' state).
    let state_dir = state.path().join("pane-758");
    std::fs::create_dir_all(&state_dir).unwrap();
    let containment = ClaudeContainment {
        state_dir,
        panes_dir: cwd.path().join("panes"),
        linux_sandbox_exe: None,
    };
    let base = base_profile(&containment).unwrap();
    for (level, found) in [
        (ChosenLevel::Permissive, true),
        (ChosenLevel::Aggressive, false),
    ] {
        let _levels = test_levels::set(level, level);
        let env = HashMap::from([
            ("HOME".to_string(), keychain.0.path().display().to_string()),
            (
                "CFFIXED_USER_HOME".to_string(),
                keychain.0.path().display().to_string(),
            ),
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        ]);
        let command = sandboxed(
            &containment,
            "/usr/bin/security",
            &find.map(String::from),
            env,
            cwd.path(),
            /*bridge_port*/ 4242,
            &level_rules(base.clone()),
        )
        .unwrap();
        let output = std::process::Command::new(&command.argv[0])
            .args(&command.argv[1..])
            .env_clear()
            .envs(&command.env)
            .current_dir(cwd.path())
            .output()
            .unwrap();
        let shown = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(shown.contains(SECRET), found, "{level:?}: {shown}");
    }
}

/// #743: the pane's sandbox keeps the session's `TMPDIR`, where the Linux
/// sandbox keeps its shared placeholder registry; Claude Code's own
/// temporary folder is `CLAUDE_CODE_TMPDIR` in the pane's state.
#[test]
fn issue_743_the_pane_keeps_the_session_tmpdir() {
    let containment = ClaudeContainment {
        state_dir: PathBuf::from("/state/pane-a"),
        panes_dir: PathBuf::from("/home/someone/.corbanu/panes"),
        linux_sandbox_exe: None,
    };
    let inherited = [
        ("PATH", "/usr/bin"),
        ("HOME", "/home/someone"),
        ("TMPDIR", "/session/tmp"),
    ]
    .map(|(name, value)| (name.to_string(), value.to_string()));
    let env = launch_env(inherited, &containment.claude_env());
    assert_eq!(env.get("TMPDIR").map(String::as_str), Some("/session/tmp"));
    let pane_tmp = containment.state_dir.join("tmp");
    assert_eq!(
        env.get("CLAUDE_CODE_TMPDIR").map(String::as_str),
        Some(pane_tmp.to_string_lossy().as_ref())
    );
    assert!(!containment.claude_env().contains_key("TMPDIR"));
}

/// #743 follow-up (Travis, 2026-10-14, option b): on Linux, Claude Code
/// starts inside the sandbox with the pane's own `TMPDIR`, while the sandbox
/// (and its shared placeholder registry) keeps the session's. macOS is
/// unchanged.
#[test]
fn issue_743_on_linux_claude_starts_with_the_pane_tmpdir() {
    let tmp = Path::new("/state/pane-a/tmp");
    let args = ["-p".to_string(), "--verbose".to_string()];
    assert_eq!(
        pane_command(
            SandboxType::LinuxSeccomp,
            tmp,
            "/usr/local/bin/claude",
            &args
        ),
        (
            "/usr/bin/env".to_string(),
            vec![
                "TMPDIR=/state/pane-a/tmp".to_string(),
                "/usr/local/bin/claude".to_string(),
                "-p".to_string(),
                "--verbose".to_string(),
            ]
        )
    );
    assert_eq!(
        pane_command(
            SandboxType::MacosSeatbelt,
            tmp,
            "/usr/local/bin/claude",
            &args
        ),
        ("/usr/local/bin/claude".to_string(), args.to_vec())
    );
    // The sandbox's own environment keeps the session's `TMPDIR`.
    let containment = ClaudeContainment {
        state_dir: PathBuf::from("/state/pane-a"),
        panes_dir: PathBuf::from("/home/someone/.corbanu/panes"),
        linux_sandbox_exe: None,
    };
    assert_eq!(containment.tmp_dir(), tmp);
    let env = launch_env(
        [("TMPDIR".to_string(), "/session/tmp".to_string())],
        &containment.claude_env(),
    );
    assert_eq!(env.get("TMPDIR").map(String::as_str), Some("/session/tmp"));
}

/// #743 follow-up, real Linux sandbox: a contained pane's commands can make
/// temporary files (`mktemp`, a here-document) in the pane's own `TMPDIR`,
/// while the session's `TMPDIR` (`/tmp` here) stays read-only to the pane.
/// Skipped where `codex-linux-sandbox` is not built or cannot run.
#[cfg(target_os = "linux")]
#[test]
#[allow(clippy::print_stderr, reason = "says why the test was skipped")]
fn issue_743_linux_pane_commands_can_make_temporary_files() {
    let Ok(exe) = codex_utils_cargo_bin::cargo_bin("codex-linux-sandbox") else {
        eprintln!("skipped: codex-linux-sandbox is not built");
        return;
    };
    let cwd = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    // Keyed like `state_dir`: its siblings (other panes) are denied.
    let state_dir = state.path().canonicalize().unwrap().join("home-key/pane-a");
    let containment = ClaudeContainment {
        state_dir,
        panes_dir: cwd.path().join("panes"),
        linux_sandbox_exe: Some(exe),
    };
    std::fs::create_dir_all(containment.tmp_dir()).unwrap();
    let env = HashMap::from([
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        (
            "HOME".to_string(),
            cwd.path().to_string_lossy().into_owned(),
        ),
        ("TMPDIR".to_string(), "/tmp".to_string()),
        // The bridge, as a pane's plan sets it.
        (
            "HTTP_PROXY".to_string(),
            "http://127.0.0.1:4242".to_string(),
        ),
    ]);
    // `wrapped`: through `pane_command`, as a pane's Claude Code starts.
    let run = |script: &str, wrapped: bool| {
        let sh_args = ["-c".to_string(), script.to_string()];
        let (program, args) = if wrapped {
            pane_command(
                SandboxType::LinuxSeccomp,
                &containment.tmp_dir(),
                "/bin/sh",
                &sh_args,
            )
        } else {
            ("/bin/sh".to_string(), sh_args.to_vec())
        };
        let command = sandboxed(
            &containment,
            &program,
            &args,
            env.clone(),
            cwd.path(),
            /*bridge_port*/ 4242,
            &base_profile(&containment).unwrap(),
        )
        .unwrap();
        let mut process = std::process::Command::new(&command.argv[0]);
        process
            .args(&command.argv[1..])
            .env_clear()
            .envs(&command.env)
            .current_dir(cwd.path());
        if let Some(arg0) = &command.arg0 {
            std::os::unix::process::CommandExt::arg0(&mut process, arg0);
        }
        process.output().unwrap()
    };
    // Unwrapped, so only a sandbox that cannot run here skips the test.
    let probe = run("true", /*wrapped*/ false);
    if !probe.status.success() {
        eprintln!(
            "skipped: the Linux sandbox cannot run here: {}",
            String::from_utf8_lossy(&probe.stderr)
        );
        return;
    }
    let output = run(
        "f=$(mktemp) && echo made > \"$f\" && cat \"$f\" && echo \"$f\" && \
         cat <<'DOC'\nheredoc-ok\nDOC\n\
         if touch /tmp/corbanu-743-$$ 2>/dev/null; then echo session-tmp-writable; fi",
        /*wrapped*/ true,
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.first().copied(), Some("made"), "{stdout}\n{stderr}");
    assert!(
        lines
            .get(1)
            .is_some_and(|path| Path::new(path).starts_with(containment.tmp_dir())),
        "{stdout}"
    );
    assert!(lines.contains(&"heredoc-ok"), "{stdout}\n{stderr}");
    assert!(!lines.contains(&"session-tmp-writable"), "{stdout}");
}

/// #891 (Travis, 2026-10-16, D12 option a): on Linux a contained pane's
/// profile denies user namespaces under Moderate and Aggressive, active or
/// saved, as agent commands' profiles do (#826). Permissive keeps the
/// profile, and macOS and Windows keep it at every level (the keychain rule
/// of #758 applies under Aggressive, see #807).
#[test]
fn issue_891_contained_panes_deny_user_namespaces_under_protected_levels_on_linux() {
    use crate::security::level::ChosenLevel;
    use crate::security::level::test_levels;
    use ChosenLevel::Aggressive;
    use ChosenLevel::Moderate;
    use ChosenLevel::Permissive;

    let cwd = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let containment = ClaudeContainment {
        state_dir: state.path().join("pane-891"),
        panes_dir: cwd.path().join("panes"),
        linux_sandbox_exe: None,
    };
    let base = base_profile(&containment).unwrap();
    assert!(!base.file_system_sandbox_policy().denies_user_namespaces());
    for (active, stored, protected) in [
        (Permissive, Permissive, None),
        (Moderate, Moderate, Some("Moderate")),
        (Aggressive, Aggressive, Some("Aggressive")),
        // Saved but not yet active, and the stricter of the two.
        (Permissive, Moderate, Some("Moderate")),
        (Moderate, Aggressive, Some("Aggressive")),
    ] {
        let _levels = test_levels::set(active, stored);
        let expected = protected.filter(|_| cfg!(target_os = "linux"));
        let profile = level_rules(base.clone());
        let mut without = profile.file_system_sandbox_policy();
        without
            .entries
            .retain(|entry| !entry.denies_user_namespaces());
        let mut keychain_only = base.file_system_sandbox_policy();
        // #807: the keychain rule follows the stricter level too.
        if protected == Some("Aggressive") {
            keychain_only
                .entries
                .push(FileSystemSandboxEntry::deny_keychain_services());
        }
        assert_eq!(
            (
                user_namespaces_denied_at(crate::security::level::protected_level_for_panes()),
                profile
                    .file_system_sandbox_policy()
                    .denies_user_namespaces(),
                without == keychain_only,
            ),
            (expected, expected.is_some(), true),
            "{active:?}/{stored:?}"
        );
    }
}

/// #807: a contained pane's level rules follow the strictest of the level
/// active at launch, the saved level and Core's live policy on the home, as
/// Core's rules for agent commands do: Aggressive saved or live alone keeps
/// the keychain services out of reach (#758), and on Linux a protected
/// level saved or live alone denies user namespaces (#891).
#[test]
fn issue_807_contained_panes_follow_the_stricter_saved_or_live_level() {
    use crate::security::level::ChosenLevel;
    use crate::security::level::protected_level_for_panes;
    use crate::security::level::test_levels;
    use ChosenLevel::Aggressive;
    use ChosenLevel::Moderate;
    use ChosenLevel::Permissive;

    let cwd = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let containment = ClaudeContainment {
        state_dir: state.path().join("pane-807"),
        panes_dir: cwd.path().join("panes"),
        linux_sandbox_exe: None,
    };
    let base = base_profile(&containment).unwrap();
    assert!(!base.file_system_sandbox_policy().denies_keychain_services());
    for (active, stored, live, strictest) in [
        (Permissive, Permissive, Permissive, None),
        (Aggressive, Permissive, Permissive, Some(Aggressive)),
        // Saved but not yet active (the launch-time level alone missed it).
        (Moderate, Aggressive, Permissive, Some(Aggressive)),
        (Permissive, Aggressive, Permissive, Some(Aggressive)),
        // Raised in Core's live policy alone.
        (Permissive, Permissive, Aggressive, Some(Aggressive)),
        (Moderate, Moderate, Aggressive, Some(Aggressive)),
        (Permissive, Permissive, Moderate, Some(Moderate)),
        // A lower live level never lowers the active or saved one.
        (Aggressive, Aggressive, Permissive, Some(Aggressive)),
        (Moderate, Permissive, Permissive, Some(Moderate)),
    ] {
        let _levels = test_levels::set_with_live(active, stored, live);
        let policy = level_rules(base.clone()).file_system_sandbox_policy();
        assert_eq!(
            (
                protected_level_for_panes(),
                policy.denies_keychain_services(),
                policy.denies_user_namespaces(),
            ),
            (
                strictest,
                strictest == Some(Aggressive),
                cfg!(target_os = "linux") && strictest.is_some(),
            ),
            "{active:?}/{stored:?}/{live:?}"
        );
    }
}

/// #891, real Linux sandbox: under Moderate and Aggressive a contained
/// pane's commands cannot create a user namespace (`unshare` and
/// bubblewrap, which Claude Code's own Bash sandbox uses, are refused) and
/// the sandbox marks their environment; Permissive is unchanged. Skipped
/// where `codex-linux-sandbox` is not built or cannot run.
#[cfg(target_os = "linux")]
#[test]
#[allow(clippy::print_stderr, reason = "says why the test was skipped")]
fn issue_891_linux_pane_commands_cannot_create_user_namespaces() {
    use crate::security::level::ChosenLevel;
    use crate::security::level::test_levels;

    let Ok(exe) = codex_utils_cargo_bin::cargo_bin("codex-linux-sandbox") else {
        eprintln!("skipped: codex-linux-sandbox is not built");
        return;
    };
    let cwd = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let state_dir = state
        .path()
        .canonicalize()
        .unwrap()
        .join("home-key/pane-891");
    let containment = ClaudeContainment {
        state_dir,
        panes_dir: cwd.path().join("panes"),
        linux_sandbox_exe: Some(exe),
    };
    std::fs::create_dir_all(containment.tmp_dir()).unwrap();
    let env = HashMap::from([
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        (
            "HOME".to_string(),
            cwd.path().to_string_lossy().into_owned(),
        ),
        ("TMPDIR".to_string(), "/tmp".to_string()),
        // The bridge, as a pane's plan sets it (the sandbox routes the
        // pane's network to it).
        (
            "HTTP_PROXY".to_string(),
            "http://127.0.0.1:4242".to_string(),
        ),
    ]);
    let run = |script: &str| {
        let command = sandboxed(
            &containment,
            "/bin/sh",
            &["-c".to_string(), script.to_string()],
            env.clone(),
            cwd.path(),
            /*bridge_port*/ 4242,
            &level_rules(base_profile(&containment).unwrap()),
        )
        .unwrap();
        let mut process = std::process::Command::new(&command.argv[0]);
        process
            .args(&command.argv[1..])
            .env_clear()
            .envs(&command.env)
            .current_dir(cwd.path());
        if let Some(arg0) = &command.arg0 {
            std::os::unix::process::CommandExt::arg0(&mut process, arg0);
        }
        let output = process.output().unwrap();
        (
            output.status,
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    {
        let _levels = test_levels::set(ChosenLevel::Permissive, ChosenLevel::Permissive);
        let (status, stdout, stderr) = run("echo ready");
        if !status.success() || !stdout.contains("ready") {
            eprintln!("skipped: the Linux sandbox cannot run here: {status}: {stderr}");
            return;
        }
    }
    let script = "echo marker=${CODEX_SANDBOX_USER_NAMESPACES_DENIED:-unset}; \
         unshare -Ur true && echo NAMESPACE-MADE; \
         if command -v bwrap >/dev/null; then bwrap --ro-bind / / true && echo BWRAP-RAN; fi; \
         true";
    for level in [ChosenLevel::Moderate, ChosenLevel::Aggressive] {
        let _levels = test_levels::set(level, level);
        let (_, stdout, stderr) = run(script);
        eprintln!("{level:?}: stdout={stdout:?} stderr={stderr:?}");
        assert!(stdout.contains("marker=1"), "{level:?}: {stdout}");
        assert!(!stdout.contains("NAMESPACE-MADE"), "{level:?}: {stdout}");
        assert!(!stdout.contains("BWRAP-RAN"), "{level:?}: {stdout}");
        assert!(
            stderr.contains("unshare failed: Operation not permitted"),
            "{level:?}: {stderr}"
        );
        // What the pane then tells the person.
        assert!(
            codex_sandboxing::user_namespaces::reports_user_namespace_denial(&stderr),
            "{level:?}: {stderr}"
        );
    }
    let _levels = test_levels::set(ChosenLevel::Permissive, ChosenLevel::Permissive);
    let (_, stdout, stderr) = run(script);
    eprintln!("Permissive: stdout={stdout:?} stderr={stderr:?}");
    assert!(stdout.contains("marker=unset"), "{stdout}");
}
