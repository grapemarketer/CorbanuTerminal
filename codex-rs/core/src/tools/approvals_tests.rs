use super::*;
use codex_protocol::approvals::NetworkPolicyAmendment;
use pretty_assertions::assert_eq;

#[test]
fn approval_resolution_rejects_denied_network_policy_amendment() {
    let resolution = ApprovalResolution {
        decision: ReviewDecision::NetworkPolicyAmendment {
            network_policy_amendment: NetworkPolicyAmendment {
                host: "denied.example.com".to_string(),
                action: NetworkPolicyRuleAction::Deny,
            },
        },
        source: ApprovalResolutionSource::User,
    };
    assert!(matches!(
        resolution.into_tool_result(),
        Err(ToolError::Declined(rejection)) if rejection == "rejected by user"
    ));
}

#[test]
fn guardian_cwd_preserves_drive_shaped_local_posix_path() {
    let native_cwd = AbsolutePathBuf::try_from(std::path::PathBuf::from("/C:/workspace"))
        .expect("drive-shaped POSIX path should be absolute");
    let cwd = PathUri::from_abs_path(&native_cwd);

    assert_eq!(
        guardian_cwd(codex_exec_server::LOCAL_ENVIRONMENT_ID, cwd)
            .expect("local cwd should retain the host path convention"),
        native_cwd
    );
}

#[test]
fn guardian_cwd_rejects_foreign_remote_path() {
    let cwd = PathUri::parse("file:///C:/workspace").expect("valid Windows path URI");

    assert!(guardian_cwd(codex_exec_server::REMOTE_ENVIRONMENT_ID, cwd).is_err());
}

/// #437: what the inspector lists for each refused approval.
#[test]
fn sec_437_refused_approvals_are_described_for_the_inspector() {
    let resolution = |decision, source| ApprovalResolution { decision, source };
    for (decision, source, expected) in [
        (
            ReviewDecision::Abort,
            ApprovalResolutionSource::User,
            Some("declined by you"),
        ),
        (
            ReviewDecision::denied("no"),
            ApprovalResolutionSource::Hook,
            Some("declined by a permission hook"),
        ),
        (
            ReviewDecision::denied("no"),
            ApprovalResolutionSource::Guardian,
            Some("declined by the automatic reviewer"),
        ),
        (
            ReviewDecision::TimedOut,
            ApprovalResolutionSource::Guardian,
            Some("not answered in time"),
        ),
        (
            ReviewDecision::Approved,
            ApprovalResolutionSource::User,
            None,
        ),
    ] {
        assert_eq!(decline_outcome(&resolution(decision, source)), expected);
    }
    assert_eq!(approval_kind("exec_command"), "command approval");
    assert_eq!(approval_kind("apply_patch"), "file edit approval");
    assert_eq!(approval_kind("mcp__x__y"), "tool approval");
}

/// Issue #513: an answer from the approval cache ("don't ask again" given
/// earlier) is not a person asked about this request now; a fresh request
/// skips the cache and asks.
#[tokio::test]
async fn issue_513_a_remembered_approval_is_not_a_person_asked_now() {
    use crate::tools::sandboxing::asking_person;
    use crate::tools::sandboxing::with_cached_approval;
    let (session, _turn) = crate::session::tests::make_session_and_context().await;
    let services = &session.services;
    let ask = |decision: ReviewDecision| move || async move { decision };
    assert_eq!(
        asking_person(|| with_cached_approval(
            services,
            "shell",
            vec!["issue-513"],
            /*fresh_human_authority*/ false,
            ask(ReviewDecision::ApprovedForSession),
        ))
        .await,
        (ReviewDecision::ApprovedForSession, true)
    );
    assert_eq!(
        asking_person(|| with_cached_approval(
            services,
            "shell",
            vec!["issue-513"],
            /*fresh_human_authority*/ false,
            ask(ReviewDecision::denied("the cache answers first")),
        ))
        .await,
        (ReviewDecision::ApprovedForSession, false)
    );
    assert_eq!(
        asking_person(|| with_cached_approval(
            services,
            "shell",
            vec!["issue-513"],
            /*fresh_human_authority*/ true,
            ask(ReviewDecision::Approved),
        ))
        .await,
        (ReviewDecision::Approved, true)
    );
    // Nothing marked: not asked (fail closed).
    assert_eq!(
        asking_person(|| async { ReviewDecision::Approved }).await,
        (ReviewDecision::Approved, false)
    );
}
