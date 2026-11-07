use std::path::Path;
use std::sync::Arc;

use codex_config::ConfigLayerEntry;
use codex_config::ConfigLayerSource;
use codex_config::ConfigLayerStack;
use codex_config::ConfigRequirements;
use codex_config::ConfigRequirementsToml;
use codex_execpolicy::Decision;
use codex_execpolicy::PolicyParser;
use codex_protocol::approvals::ExecPolicyAmendment;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use toml::Value as TomlValue;

use super::*;
use crate::exec_policy::ApprovedUnder;
use crate::exec_policy::ExecApprovalRequest;
use crate::exec_policy::ExecPolicyManager;
use crate::sandboxing::SandboxPermissions;
use crate::tools::sandboxing::ExecApprovalRequirement;

fn words(text: &str) -> Vec<String> {
    text.split(' ').map(str::to_string).collect()
}

fn marked_rule(prefix: &str) -> String {
    let pattern = words(prefix)
        .iter()
        .map(|word| format!("{word:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "prefix_rule(pattern=[{pattern}], decision=\"allow\", justification={APPROVED_UNDER_AGGRESSIVE:?})"
    )
}

/// A Corbanu home (`home/config.toml`, its rules in `home/rules`) and a
/// project's `.codex` folder, as the session loads them.
fn stack(home: &Path, project: &Path) -> ConfigLayerStack {
    let user = ConfigLayerEntry::new(
        ConfigLayerSource::User {
            file: AbsolutePathBuf::from_absolute_path(home.join("config.toml")).expect("absolute"),
            profile: None,
        },
        TomlValue::Table(Default::default()),
    );
    let project = ConfigLayerEntry::new(
        ConfigLayerSource::Project {
            dot_codex_folder: AbsolutePathBuf::from_absolute_path(project).expect("absolute"),
        },
        TomlValue::Table(Default::default()),
    );
    ConfigLayerStack::new(
        vec![user, project],
        ConfigRequirements::default(),
        ConfigRequirementsToml::default(),
    )
    .expect("ConfigLayerStack")
}

fn write_rules(folder: &Path, rules: &[String]) {
    std::fs::create_dir_all(folder.join("rules")).expect("rules folder");
    std::fs::write(
        folder.join("rules").join("default.rules"),
        rules.join("\n") + "\n",
    )
    .expect("write rules");
}

async fn requirement(
    manager: &ExecPolicyManager,
    command: &str,
    live_aggressive: bool,
) -> ExecApprovalRequirement {
    let command = words(command);
    manager
        .create_exec_approval_requirement(
            ExecApprovalRequest {
                command: &command,
                approval_policy: AskForApproval::UnlessTrusted,
                permission_profile: PermissionProfile::read_only(),
                windows_sandbox_level: WindowsSandboxLevel::Disabled,
                sandbox_permissions: SandboxPermissions::UseDefault,
                prefix_rule: None,
            },
            live_aggressive,
        )
        .await
}

fn runs_unasked(requirement: &ExecApprovalRequirement) -> bool {
    matches!(requirement, ExecApprovalRequirement::Skip { .. })
}

fn reason(requirement: &ExecApprovalRequirement) -> String {
    match requirement {
        ExecApprovalRequirement::NeedsApproval { reason, .. } => reason.clone().unwrap_or_default(),
        other => panic!("not asked: {other:?}"),
    }
}

#[test]
fn only_marked_allow_rules_of_the_home_survive_suspension() {
    let source = [
        r#"prefix_rule(pattern=["touch"], decision="allow")"#.to_string(),
        r#"prefix_rule(pattern=["git", "push"], decision="prompt")"#.to_string(),
        r#"prefix_rule(pattern=["rm"], decision="forbidden")"#.to_string(),
        marked_rule("echo hi"),
        // Marked, but not recorded by the Corbanu home (a project's rules).
        marked_rule("curl"),
    ]
    .join("\n");
    let mut parser = PolicyParser::new();
    parser.parse("test.rules", &source).expect("parses");
    let policy = parser.build();
    let suspended = suspend_unapproved(&policy, &[words("echo hi")]);
    let decision = |command: &str| {
        suspended
            .check(&words(command), &|_| Decision::Prompt)
            .decision
    };
    assert_eq!(decision("touch x"), Decision::Prompt);
    assert_eq!(decision("curl x"), Decision::Prompt);
    assert_eq!(decision("echo hi there"), Decision::Allow);
    assert_eq!(decision("rm x"), Decision::Forbidden);
    assert_eq!(
        suspended
            .check(&words("git push"), &|_| Decision::Allow)
            .decision,
        Decision::Prompt
    );
}

#[test]
fn approved_in_reads_only_marked_allow_rules() {
    let source = [
        r#"prefix_rule(pattern=["touch"], decision="allow")"#.to_string(),
        marked_rule("echo hi"),
        format!(
            "prefix_rule(pattern=[\"rm\"], decision=\"forbidden\", justification={APPROVED_UNDER_AGGRESSIVE:?})"
        ),
    ]
    .join("\n");
    assert_eq!(
        approved_in(Path::new("default.rules"), &source),
        vec![words("echo hi")]
    );
    assert_eq!(
        approved_in(Path::new("default.rules"), "not a rule("),
        Vec::<Vec<String>>::new()
    );
}

/// #626 (AGG-33 replay): a "don't ask again" approval saved under a less
/// strict level runs its commands unasked there, but under Aggressive the
/// person is asked and told why; the rule is kept.
#[tokio::test]
async fn issue_626_rule_saved_outside_aggressive_is_suspended_there_only() {
    let home = TempDir::new().expect("home");
    let project = TempDir::new().expect("project");
    let rule = r#"prefix_rule(pattern=["touch", "x"], decision="allow")"#.to_string();
    write_rules(home.path(), std::slice::from_ref(&rule));
    let manager = ExecPolicyManager::load(
        &stack(home.path(), project.path()),
        /*strict_rules*/ false,
    )
    .await
    .expect("load");

    assert!(runs_unasked(
        &requirement(&manager, "touch x", /*live_aggressive*/ false).await
    ));
    let asked = requirement(&manager, "touch x", /*live_aggressive*/ true).await;
    let reason = reason(&asked);
    assert!(
        reason.contains("A remembered approval (don't ask again for commands that start with `touch x`) was not applied because security level Aggressive is on"),
        "{reason}"
    );
    assert!(reason.contains("apply again when you return"), "{reason}");
    // Kept on disk.
    assert_eq!(
        std::fs::read_to_string(home.path().join("rules/default.rules")).expect("rules"),
        rule + "\n"
    );
    // A command no rule approves is asked about without the note.
    let other = requirement(&manager, "touch y", /*live_aggressive*/ true).await;
    assert!(!reason_of(&other).contains("remembered"));
}

fn reason_of(requirement: &ExecApprovalRequirement) -> String {
    match requirement {
        ExecApprovalRequirement::NeedsApproval { reason, .. } => reason.clone().unwrap_or_default(),
        _ => String::new(),
    }
}

/// #626: a rule a person saved while Aggressive was active applies under
/// Aggressive, now and in later sessions, and under Permissive too.
#[tokio::test]
async fn issue_626_rule_saved_under_aggressive_applies_there() {
    let home = TempDir::new().expect("home");
    let project = TempDir::new().expect("project");
    let manager = ExecPolicyManager::load(
        &stack(home.path(), project.path()),
        /*strict_rules*/ false,
    )
    .await
    .expect("load");
    assert!(!runs_unasked(
        &requirement(&manager, "touch x", /*live_aggressive*/ true).await
    ));
    manager
        .append_amendment_and_update(
            home.path(),
            &ExecPolicyAmendment::from(words("touch x")),
            ApprovedUnder::Aggressive,
        )
        .await
        .expect("saved");
    assert!(runs_unasked(
        &requirement(&manager, "touch x", /*live_aggressive*/ true).await
    ));
    assert_eq!(
        std::fs::read_to_string(home.path().join("rules/default.rules")).expect("rules"),
        marked_rule("touch x") + "\n"
    );

    let later = ExecPolicyManager::load(
        &stack(home.path(), project.path()),
        /*strict_rules*/ false,
    )
    .await
    .expect("load");
    assert!(runs_unasked(
        &requirement(&later, "touch x", /*live_aggressive*/ true).await
    ));
    assert!(runs_unasked(
        &requirement(&later, "touch x", /*live_aggressive*/ false).await
    ));
}

/// #626: re-approving under Aggressive a prefix already remembered under a
/// less strict level makes it apply under Aggressive too.
#[tokio::test]
async fn issue_626_reapproving_under_aggressive_marks_an_existing_rule() {
    let home = TempDir::new().expect("home");
    let project = TempDir::new().expect("project");
    write_rules(
        home.path(),
        &[r#"prefix_rule(pattern=["touch", "x"], decision="allow")"#.to_string()],
    );
    let manager = ExecPolicyManager::load(
        &stack(home.path(), project.path()),
        /*strict_rules*/ false,
    )
    .await
    .expect("load");
    manager
        .append_amendment_and_update(
            home.path(),
            &ExecPolicyAmendment::from(words("touch x")),
            ApprovedUnder::Aggressive,
        )
        .await
        .expect("saved");
    assert!(runs_unasked(
        &requirement(&manager, "touch x", /*live_aggressive*/ true).await
    ));
}

/// #626: only the Corbanu home says a rule was approved under Aggressive: a
/// project's rules file carrying the mark is suspended like any other.
#[tokio::test]
async fn issue_626_project_rules_cannot_claim_aggressive_approval() {
    let home = TempDir::new().expect("home");
    let project = TempDir::new().expect("project");
    write_rules(project.path(), &[marked_rule("touch x")]);
    let manager = ExecPolicyManager::load(
        &stack(home.path(), project.path()),
        /*strict_rules*/ false,
    )
    .await
    .expect("load");
    assert!(runs_unasked(
        &requirement(&manager, "touch x", /*live_aggressive*/ false).await
    ));
    assert!(!runs_unasked(
        &requirement(&manager, "touch x", /*live_aggressive*/ true).await
    ));
}

/// #626: the prompt names only the suspended approvals, not one that a
/// person saved under Aggressive and that applied to another part.
#[tokio::test]
async fn issue_626_note_names_only_suspended_rules() {
    let home = TempDir::new().expect("home");
    let project = TempDir::new().expect("project");
    write_rules(
        home.path(),
        &[
            r#"prefix_rule(pattern=["touch", "x"], decision="allow")"#.to_string(),
            marked_rule("mkdir y"),
        ],
    );
    let manager = ExecPolicyManager::load(
        &stack(home.path(), project.path()),
        /*strict_rules*/ false,
    )
    .await
    .expect("load");
    let command = vec![
        "bash".to_string(),
        "-lc".to_string(),
        "touch x && mkdir y".to_string(),
    ];
    let asked = manager
        .create_exec_approval_requirement(
            ExecApprovalRequest {
                command: &command,
                approval_policy: AskForApproval::UnlessTrusted,
                permission_profile: PermissionProfile::read_only(),
                windows_sandbox_level: WindowsSandboxLevel::Disabled,
                sandbox_permissions: SandboxPermissions::UseDefault,
                prefix_rule: None,
            },
            /*live_aggressive*/ true,
        )
        .await;
    let reason = reason(&asked);
    assert!(
        reason.contains("start with `touch x`) was not applied"),
        "{reason}"
    );
    assert!(!reason.contains("mkdir"), "{reason}");
}

/// #626 (review r1): where prompts are rejected (approval policy never),
/// the rejection says a remembered approval was suspended. On Windows the
/// default (elevated) Windows sandbox is set up, as Aggressive requires
/// there (#691): without it a read-only command is refused for that first.
#[tokio::test]
async fn issue_626_rejection_names_the_suspended_rule() {
    let home = TempDir::new().expect("home");
    let project = TempDir::new().expect("project");
    write_rules(
        home.path(),
        &[r#"prefix_rule(pattern=["touch", "x"], decision="allow")"#.to_string()],
    );
    let manager = ExecPolicyManager::load(
        &stack(home.path(), project.path()),
        /*strict_rules*/ false,
    )
    .await
    .expect("load");
    let command = words("touch x");
    let rejected = manager
        .create_exec_approval_requirement(
            ExecApprovalRequest {
                command: &command,
                approval_policy: AskForApproval::Never,
                permission_profile: PermissionProfile::read_only(),
                windows_sandbox_level: if cfg!(windows) {
                    WindowsSandboxLevel::Elevated
                } else {
                    WindowsSandboxLevel::Disabled
                },
                sandbox_permissions: SandboxPermissions::UseDefault,
                prefix_rule: None,
            },
            /*live_aggressive*/ true,
        )
        .await;
    match rejected {
        ExecApprovalRequirement::Forbidden { reason } => {
            assert!(
                reason.contains("was not applied because security level Aggressive is on"),
                "{reason}"
            );
        }
        // Never asking, the command may run sandboxed instead: nothing is
        // rejected then, and there is nothing to say.
        ExecApprovalRequirement::Skip { .. } => {}
        other => panic!("asked although approvals are off: {other:?}"),
    }
}

#[test]
fn manager_without_rules_suspends_nothing_outside_aggressive() {
    let mut parser = PolicyParser::new();
    parser
        .parse(
            "t.rules",
            r#"prefix_rule(pattern=["touch"], decision="allow")"#,
        )
        .expect("parses");
    let manager = ExecPolicyManager::new(Arc::new(parser.build()));
    let command = words("touch x");
    assert_eq!(
        manager
            .current_for(/*live_aggressive*/ false)
            .check(&command, &|_| Decision::Prompt)
            .decision,
        Decision::Allow
    );
    assert_eq!(
        manager
            .current_for(/*live_aggressive*/ true)
            .check(&command, &|_| Decision::Prompt)
            .decision,
        Decision::Prompt
    );
}

fn network_rule(host: &str, decision: &str, justification: Option<&str>) -> String {
    match justification {
        Some(justification) => format!(
            "network_rule(host={host:?}, protocol=\"https\", decision={decision:?}, justification={justification:?})"
        ),
        None => format!("network_rule(host={host:?}, protocol=\"https\", decision={decision:?})"),
    }
}

fn allowed_hosts(policy: &codex_execpolicy::Policy) -> Vec<String> {
    let (mut allowed, _) = policy.compiled_network_domains();
    allowed.sort();
    allowed
}

/// D52 (#1001): under Aggressive, network allow rules saved outside it are
/// suspended; deny rules, and allow rules the Corbanu home marks as saved
/// under Aggressive, apply. Outside Aggressive every rule applies as before.
#[tokio::test]
async fn issue_1001_network_allow_rules_saved_outside_aggressive_are_suspended_there() {
    let home = TempDir::new().expect("home");
    let project = TempDir::new().expect("project");
    write_rules(
        home.path(),
        &[
            network_rule("permissive.example", "allow", Some("Allowed by you")),
            network_rule(
                "aggressive.example",
                "allow",
                Some(APPROVED_UNDER_AGGRESSIVE),
            ),
            network_rule("denied.example", "deny", /*justification*/ None),
        ],
    );
    // A project's rules file cannot claim the mark.
    write_rules(
        project.path(),
        &[network_rule(
            "project.example",
            "allow",
            Some(APPROVED_UNDER_AGGRESSIVE),
        )],
    );
    let manager = ExecPolicyManager::load(
        &stack(home.path(), project.path()),
        /*strict_rules*/ false,
    )
    .await
    .expect("load");

    assert_eq!(
        allowed_hosts(&manager.current_for(/*live_aggressive*/ false)),
        vec![
            "aggressive.example".to_string(),
            "permissive.example".to_string(),
            "project.example".to_string(),
        ]
    );
    let aggressive = manager.current_for(/*live_aggressive*/ true);
    assert_eq!(
        allowed_hosts(&aggressive),
        vec!["aggressive.example".to_string()]
    );
    assert_eq!(
        aggressive.compiled_network_domains().1,
        vec!["denied.example".to_string()],
        "deny rules apply"
    );
}

/// D52: a host allowed while Aggressive is in force is marked in the Corbanu
/// home's rules file and applies under Aggressive, now and after a restart;
/// one allowed under a less strict level applies only outside it.
#[tokio::test]
async fn issue_1001_network_allow_rule_saved_under_aggressive_applies_there() {
    let home = TempDir::new().expect("home");
    let project = TempDir::new().expect("project");
    let manager = ExecPolicyManager::load(
        &stack(home.path(), project.path()),
        /*strict_rules*/ false,
    )
    .await
    .expect("load");
    for (host, approved_under) in [
        ("aggressive.example", ApprovedUnder::Aggressive),
        ("permissive.example", ApprovedUnder::LessStrict),
    ] {
        manager
            .append_network_rule_and_update(
                home.path(),
                host,
                codex_execpolicy::NetworkRuleProtocol::Https,
                Decision::Allow,
                Some("Allowed by you".to_string()),
                approved_under,
            )
            .await
            .expect("saved");
    }
    let rules = std::fs::read_to_string(home.path().join("rules/default.rules")).expect("rules");
    assert_eq!(
        rules,
        [
            network_rule(
                "aggressive.example",
                "allow",
                Some(APPROVED_UNDER_AGGRESSIVE)
            ),
            network_rule("permissive.example", "allow", Some("Allowed by you")),
        ]
        .join("\n")
            + "\n"
    );
    let check = |manager: &ExecPolicyManager| {
        (
            allowed_hosts(&manager.current_for(/*live_aggressive*/ true)),
            allowed_hosts(&manager.current_for(/*live_aggressive*/ false)),
        )
    };
    let expected = (
        vec!["aggressive.example".to_string()],
        vec![
            "aggressive.example".to_string(),
            "permissive.example".to_string(),
        ],
    );
    assert_eq!(check(&manager), expected);
    let later = ExecPolicyManager::load(
        &stack(home.path(), project.path()),
        /*strict_rules*/ false,
    )
    .await
    .expect("load");
    assert_eq!(check(&later), expected, "after a restart");
}
