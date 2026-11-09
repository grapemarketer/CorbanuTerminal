use super::*;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

#[test]
fn d447_appends_the_entry_and_restores_it_after_the_snapshot() {
    let mut command_env = env(&[("PATH", "/bin")]);
    let mut overrides = HashMap::new();
    apply(&mut command_env, &mut overrides);
    let guard = env(&[
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "protocol.ext.allow"),
        ("GIT_CONFIG_VALUE_0", "never"),
    ]);
    assert_eq!(overrides, guard);
    let mut expected = guard;
    expected.insert("PATH".to_string(), "/bin".to_string());
    assert_eq!(command_env, expected);
}

#[test]
fn d447_keeps_the_users_entries_and_goes_last() {
    let mut command_env = env(&[
        ("GIT_CONFIG_COUNT", "2"),
        ("GIT_CONFIG_KEY_0", "protocol.ext.allow"),
        ("GIT_CONFIG_VALUE_0", "always"),
        ("GIT_CONFIG_KEY_1", "core.abbrev"),
        ("GIT_CONFIG_VALUE_1", "12"),
    ]);
    let mut overrides = env(&[("FOO", "bar")]);
    apply(&mut command_env, &mut overrides);
    assert_eq!(
        command_env,
        env(&[
            ("GIT_CONFIG_COUNT", "3"),
            ("GIT_CONFIG_KEY_0", "protocol.ext.allow"),
            ("GIT_CONFIG_VALUE_0", "always"),
            ("GIT_CONFIG_KEY_1", "core.abbrev"),
            ("GIT_CONFIG_VALUE_1", "12"),
            ("GIT_CONFIG_KEY_2", "protocol.ext.allow"),
            ("GIT_CONFIG_VALUE_2", "never"),
        ])
    );
    assert_eq!(
        overrides,
        env(&[
            ("FOO", "bar"),
            ("GIT_CONFIG_COUNT", "3"),
            ("GIT_CONFIG_KEY_2", "protocol.ext.allow"),
            ("GIT_CONFIG_VALUE_2", "never"),
        ])
    );
}

#[test]
fn d447_leaves_an_unreadable_count_and_drops_ext_from_allowed_protocols() {
    let mut command_env = env(&[
        ("GIT_CONFIG_COUNT", "x"),
        ("GIT_ALLOW_PROTOCOL", "https:ext:ssh"),
    ]);
    let mut overrides = HashMap::new();
    apply(&mut command_env, &mut overrides);
    assert_eq!(
        command_env,
        env(&[
            ("GIT_CONFIG_COUNT", "x"),
            ("GIT_ALLOW_PROTOCOL", "https:ssh"),
        ])
    );
    assert_eq!(overrides, env(&[("GIT_ALLOW_PROTOCOL", "https:ssh")]));
}

#[test]
fn d447_reads_the_count_as_git_does() {
    for (count, read) in [
        ("", Some(0)),
        ("2", Some(2)),
        (" 2", Some(2)),
        ("+2", Some(2)),
        ("\t+2", Some(2)),
        (" ", None),
        ("+", None),
        ("++2", None),
        ("2 ", None),
        ("-1", None),
        ("x", None),
    ] {
        assert_eq!(git_count(count), read, "{count:?}");
    }
    let mut command_env = env(&[("GIT_CONFIG_COUNT", "")]);
    apply(&mut command_env, &mut HashMap::new());
    assert_eq!(
        command_env.get("GIT_CONFIG_KEY_0").map(String::as_str),
        Some("protocol.ext.allow")
    );
    let mut command_env = env(&[("GIT_CONFIG_COUNT", &usize::MAX.to_string())]);
    apply(&mut command_env, &mut HashMap::new());
    assert_eq!(command_env.len(), 1);
}

/// git with none of the `GIT_*` variables a runner (or a git hook running
/// the tests) might set, only those in `env`.
fn git(dir: &Path, env: &HashMap<String, String>, args: &[&str]) -> std::process::Output {
    let mut command = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    command
        .current_dir(dir)
        .envs(env)
        .args(args)
        .output()
        .expect("run git")
}

/// A repository whose own config allows `ext` and stores an `ext::` remote.
fn repo_allowing_ext(dir: &Path, remote: &str) {
    let none = HashMap::new();
    assert!(git(dir, &none, &["init", "-q"]).status.success());
    for args in [
        &["config", "protocol.ext.allow", "always"][..],
        &["remote", "add", "origin", remote],
    ] {
        assert!(git(dir, &none, args).status.success());
    }
}

/// Real git: the entry outranks the repository's own config and keeps the
/// user's other entries.
#[test]
fn d447_git_reads_never_over_the_repository_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    repo_allowing_ext(dir.path(), "ext::git %s");
    let mut command_env = env(&[
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "core.abbrev"),
        ("GIT_CONFIG_VALUE_0", "12"),
    ]);
    let read = |command_env: &HashMap<String, String>, key: &str| {
        let output = git(dir.path(), command_env, &["config", "--get", key]);
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };
    assert_eq!(read(&command_env, "protocol.ext.allow"), "always");
    apply(&mut command_env, &mut HashMap::new());
    assert_eq!(read(&command_env, "protocol.ext.allow"), "never");
    assert_eq!(read(&command_env, "core.abbrev"), "12");
}

/// Real git: a stored `ext::` remote runs its command without the entry and
/// is refused with it.
#[cfg(unix)]
#[test]
fn d447_stored_ext_remote_does_not_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("ran");
    repo_allowing_ext(
        dir.path(),
        &format!("ext::sh -c touch% {}", marker.display()),
    );
    let fetch = |command_env: &HashMap<String, String>| {
        git(dir.path(), command_env, &["fetch", "-q", "origin"])
    };

    let mut command_env = HashMap::new();
    apply(&mut command_env, &mut HashMap::new());
    let refused = fetch(&command_env);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("transport 'ext' not allowed"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(!marker.exists(), "the ext:: command ran with the guard");

    // Without it the same fetch runs the command (the repository allows it).
    let _ = fetch(&HashMap::new());
    assert!(
        marker.exists(),
        "the ext:: command never ran without the guard"
    );
}

/// Real git: `ext` allowed by `GIT_ALLOW_PROTOCOL` runs a stored `ext::`
/// remote; with the guard (which drops it from the list) it doesn't.
#[cfg(unix)]
#[test]
fn d447_ext_allowed_by_the_environment_list_does_not_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("ran");
    let none = HashMap::new();
    assert!(git(dir.path(), &none, &["init", "-q"]).status.success());
    let remote = format!("ext::sh -c touch% {}", marker.display());
    assert!(
        git(dir.path(), &none, &["remote", "add", "origin", &remote])
            .status
            .success()
    );
    let allowed = env(&[("GIT_ALLOW_PROTOCOL", "ext:https")]);

    let mut guarded = allowed.clone();
    apply(&mut guarded, &mut HashMap::new());
    let refused = git(dir.path(), &guarded, &["fetch", "-q", "origin"]);
    assert!(!refused.status.success());
    assert!(!marker.exists(), "the ext:: command ran with the guard");

    let _ = git(dir.path(), &allowed, &["fetch", "-q", "origin"]);
    assert!(
        marker.exists(),
        "the ext:: command never ran without the guard"
    );
}

/// Real git: a `.gitmodules` `ext::` URL, with `ext` allowed by the user's
/// global config, runs its command on `git submodule update --init` without
/// the guard; with it, the submodule's clone (a child git) refuses it.
#[cfg(unix)]
#[test]
fn d447_gitmodules_ext_url_does_not_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("ran");
    let global = dir.path().join("global.gitconfig");
    std::fs::write(&global, "[protocol \"ext\"]\n\tallow = always\n").expect("global config");
    let base = env(&[("GIT_CONFIG_GLOBAL", &global.display().to_string())]);
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).expect("repo dir");
    assert!(git(&repo, &base, &["init", "-q"]).status.success());
    std::fs::write(
        repo.join(".gitmodules"),
        format!(
            "[submodule \"sub\"]\n\tpath = sub\n\turl = ext::sh -c touch% {}\n",
            marker.display()
        ),
    )
    .expect(".gitmodules");
    // Any commit id will do: the clone runs the command first.
    let gitlink = "160000,4b825dc642cb6eb9a060e54bf8d69288fbee4904,sub";
    assert!(
        git(
            &repo,
            &base,
            &["update-index", "--add", "--cacheinfo", gitlink]
        )
        .status
        .success()
    );

    let mut guarded = base.clone();
    apply(&mut guarded, &mut HashMap::new());
    let refused = git(&repo, &guarded, &["submodule", "update", "--init"]);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("transport 'ext' not allowed"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(!marker.exists(), "the ext:: command ran with the guard");

    let _ = git(&repo, &base, &["submodule", "update", "--init"]);
    assert!(
        marker.exists(),
        "the ext:: command never ran without the guard"
    );
}

/// Through the shell tools (`shell_command` and the unified `exec_command`):
/// a command run after untrusted content reads `protocol.ext.allow=never`
/// though the repository allows `ext`; before untrusted content it reads the
/// repository's value.
#[tokio::test]
async fn d447_shell_command_after_untrusted_content_gets_the_entry() {
    use crate::session::step_context::StepContext;
    use crate::tools::context::ToolInvocation;
    use crate::tools::context::ToolPayload;
    use crate::turn_diff_tracker::TurnDiffTracker;
    use codex_protocol::models::FunctionCallOutputPayload;
    use codex_protocol::models::PermissionProfile;
    use codex_protocol::models::ResponseInputItem;
    use codex_protocol::models::ResponseItem;
    use codex_protocol::protocol::AskForApproval;
    use codex_security_policy::SecurityLevel;
    use codex_tools::ToolExecutor;

    let dir = tempfile::tempdir().expect("tempdir");
    repo_allowing_ext(dir.path(), "ext::git %s");
    let workdir = dir.path().to_string_lossy().to_string();
    let command = "git config --get protocol.ext.allow";

    for tool in ["shell_command", "exec_command"] {
        for tainted in [false, true] {
            let (session, mut turn) = crate::session::tests::make_session_and_context().await;
            turn.approval_policy
                .set(AskForApproval::Never)
                .expect("approval policy");
            turn.permission_profile = PermissionProfile::Disabled;
            // Moderate keeps commands in the sandbox, which needs its helper
            // on Linux.
            let mut config = (*turn.config).clone();
            config.codex_linux_sandbox_exe = core_test_support::codex_linux_sandbox_exe_or_skip!();
            turn.config = Arc::new(config);
            let client = (*session.services.model_client())
                .clone()
                .with_ingress_level(SecurityLevel::Moderate)
                .with_source_envelopes(/*enabled*/ true);
            if tainted {
                client.note_recorded_for_taint(&[ResponseItem::FunctionCallOutput {
                    id: None,
                    call_id: "call-read".into(),
                    output: FunctionCallOutputPayload::from_text("README".into()),
                    internal_chat_message_metadata_passthrough: None,
                }]);
            }
            session.services.replace_model_client(client);
            assert_eq!(applies(&session), tainted);

            let session = Arc::new(session);
            let turn = Arc::new(turn);
            let invocation = ToolInvocation {
                session: Arc::clone(&session),
                step_context: StepContext::for_test(Arc::clone(&turn)),
                turn: Arc::clone(&turn),
                cancellation_token: tokio_util::sync::CancellationToken::new(),
                tracker: Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
                call_id: "call-git".to_string(),
                tool_name: codex_tools::ToolName::plain(tool),
                source: crate::tools::context::ToolCallSource::Direct,
                payload: ToolPayload::Function {
                    arguments: if tool == "shell_command" {
                        serde_json::json!({
                            "command": command,
                            "login": false,
                            "workdir": workdir,
                            "timeout_ms": 30_000,
                        })
                    } else {
                        serde_json::json!({
                            "cmd": command,
                            "login": false,
                            "workdir": workdir,
                            "yield_time_ms": 30_000,
                        })
                    }
                    .to_string(),
                },
            };
            let response = if tool == "shell_command" {
                crate::tools::handlers::ShellCommandHandler::from(
                    codex_tools::ShellCommandBackendConfig::Classic,
                )
                .handle(invocation)
                .await
            } else {
                crate::tools::handlers::ExecCommandHandler::default()
                    .handle(invocation)
                    .await
            }
            .unwrap_or_else(|err| panic!("{tool} tainted={tainted}: {err:?}"));
            let text = match response.to_response_item(
                "call-git",
                &ToolPayload::Function {
                    arguments: "{}".to_string(),
                },
            ) {
                ResponseInputItem::FunctionCallOutput { output, .. }
                | ResponseInputItem::CustomToolCallOutput { output, .. } => {
                    output.body.to_text().unwrap_or_default()
                }
                other => panic!("expected a function call output, got {other:?}"),
            };
            let expected = if tainted { "never" } else { "always" };
            assert!(
                text.lines().any(|line| line.trim() == expected),
                "{tool} tainted={tainted}: {text}"
            );
        }
    }
}
