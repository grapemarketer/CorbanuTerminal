//! #1048: D48's explanation-only note belongs to an attempt, not a stale
//! approval round. The runtime below returns synthetic output: it never
//! executes the displayed command, starts a child or creates a namespace.

use super::*;
use crate::session::tests::build_test_config;
use crate::session::tests::make_session_and_context_for_config;
use crate::tools::sandboxing::Approvable;
use crate::tools::sandboxing::Sandboxable;
use codex_security_policy::PolicyPrincipal;
use codex_security_policy::PrincipalKind;
use codex_security_policy::RevocationState;
use codex_security_policy::SecurityLevel;
use codex_security_policy::SecuritySettings;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

const REFUSED: &str = "unshare: unshare failed: Operation not permitted";

struct Probe {
    session: Arc<crate::session::session::Session>,
    controller: crate::security::TrustedSecurityController,
    transition_requested: bool,
    transition_pending: AtomicBool,
    asked: Vec<(String, u64)>,
    explanations: Vec<Option<String>>,
}

impl Approvable<()> for Probe {
    type ApprovalKey = String;

    fn additional_permissions<'a>(
        &self,
        _req: &'a (),
    ) -> Option<&'a codex_protocol::models::AdditionalPermissionProfile> {
        None
    }

    fn approval_keys(&self, _req: &()) -> Vec<Self::ApprovalKey> {
        vec!["issue-1048-probe".to_string()]
    }

    fn sandbox_permissions(&self, _req: &()) -> crate::sandboxing::SandboxPermissions {
        crate::sandboxing::SandboxPermissions::RequireEscalated
    }

    fn exec_approval_requirement(&self, _req: &()) -> Option<ExecApprovalRequirement> {
        Some(ExecApprovalRequirement::NeedsApproval {
            reason: None,
            proposed_execpolicy_amendment: None,
        })
    }

    fn start_approval_async<'a>(
        &'a mut self,
        _req: &'a (),
        ctx: ApprovalCtx<'a>,
    ) -> futures::future::BoxFuture<'a, ReviewDecision> {
        assert!(ctx.fresh_human_authority, "D48 asks the person afresh");
        crate::tools::sandboxing::note_person_asked();
        let state = self
            .session
            .services
            .model_client()
            .post_taint_state()
            .unwrap();
        self.asked
            .push((ctx.retry_reason.unwrap(), state.taint_generation));
        Box::pin(async move {
            // Arm only the first approved round. Earlier preference reads,
            // including the pre-approval Git check, cannot commit a transition.
            if std::mem::take(&mut self.transition_requested) {
                self.transition_pending.store(true, Ordering::SeqCst);
            }
            ReviewDecision::Approved
        })
    }

    fn approval_action(&self, _req: &(), ctx: &ApprovalCtx<'_>) -> std::io::Result<ApprovalAction> {
        Ok(ApprovalAction::Shell {
            id: ctx.call_id.to_string(),
            environment_id: codex_exec_server::LOCAL_ENVIRONMENT_ID.to_string(),
            // An absolute system-tool name exercises D48's actual resolver;
            // the Probe runtime never launches it.
            command: vec!["/usr/bin/unshare".to_string(), "-Ur".to_string()],
            #[allow(deprecated)]
            cwd: PathUri::from_abs_path(&ctx.turn.cwd),
            sandbox_permissions: crate::sandboxing::SandboxPermissions::RequireEscalated,
            additional_permissions: None,
            justification: Some("synthetic namespace denial".to_string()),
        })
    }
}

impl Sandboxable for Probe {
    fn sandbox_preference(&self) -> codex_sandboxing::SandboxablePreference {
        // This callback is also used before approval. Only the approved future
        // arms it, so the transition occurs after D48's allowance and before
        // the final D50 check, without a timer or production-only test hook.
        if self.transition_pending.swap(false, Ordering::SeqCst) {
            assert_eq!(
                self.asked.len(),
                1,
                "only the first approval arms the transition"
            );
            assert_eq!(
                self.asked[0].1, 1,
                "the first warning precedes the transition"
            );
            assert_eq!(
                self.session
                    .services
                    .model_client()
                    .post_taint_state()
                    .unwrap()
                    .taint_generation,
                1,
                "commit only after approval of the original generation",
            );
            let before = self
                .session
                .services
                .authority_marker()
                .expect("the fixture has an initialized policy");
            // Reapplying Moderate with empty revocations is a no-op. A real
            // synthetic grant revocation moves D50's marker without changing
            // the level whose D48 warning the second round must show.
            let mut revocations = RevocationState::new();
            let event = codex_security_policy::RevocationEvent::new(
                PolicyPrincipal::new(PrincipalKind::Human, "human:issue-1048").unwrap(),
                codex_security_policy::RevocationTarget::Grant {
                    grant_id: codex_security_policy::BoundedText::new("issue-1048-grant").unwrap(),
                },
                codex_security_policy::RevocationReason::HumanRequest,
                /*created_at_unix_seconds*/ 1,
            )
            .unwrap();
            assert!(
                revocations
                    .apply(&event)
                    .expect("apply synthetic revocation")
            );
            let change = self
                .controller
                .confirm_level_change(SecurityLevel::Moderate, revocations)
                .expect("confirm the synthetic transition");
            self.controller
                .apply_confirmed_change(change)
                .expect("commit transition");
            assert_eq!(
                self.session.services.authority_marker(),
                Some((before.0 + 1, before.1 + 1)),
                "the revocation must advance both parts of D50's marker",
            );
            assert!(self.session.services.transition_since(Some(before)));
            self.session
                .services
                .model_client()
                .note_unrecorded_input_for_taint();
        }
        codex_sandboxing::SandboxablePreference::Auto
    }
}

impl ToolRuntime<(), ()> for Probe {
    fn workspace_roots<'a>(&self, _req: &'a ()) -> &'a [PathUri] {
        &[]
    }

    async fn run(
        &mut self,
        _req: &(),
        _attempt: &SandboxAttempt<'_>,
        ctx: &ToolCtx,
    ) -> Result<(), ToolError> {
        self.explanations
            .push(crate::security::user_namespaces::denial_explanation(
                &ctx.session,
                &ctx.turn,
                &ctx.call_id,
                Some(1),
                REFUSED,
            ));
        Ok(())
    }
}

struct Fixture {
    probe: Probe,
    ctx: ToolCtx,
    // Owners outlive the session, config and turn that use their paths.
    _home: tempfile::TempDir,
    _cwd: tempfile::TempDir,
}

impl Fixture {
    async fn new(transition_requested: bool) -> Self {
        let home = tempfile::tempdir().expect("temporary Corbanu home");
        let cwd = tempfile::tempdir().expect("temporary working folder");
        let config = build_test_config(home.path(), cwd.path()).await;
        let (session, turn) = make_session_and_context_for_config(config).await;
        let session = Arc::new(session);
        let turn = Arc::new(turn);
        let controller = crate::security::TrustedSecurityController::initialize(
            &session.services.agent_control.effective_security_policy(),
            crate::security::PersistedHumanSecurityState::new(
                SecuritySettings::new(SecurityLevel::Moderate),
                PolicyPrincipal::new(PrincipalKind::Human, "human:issue-1048").unwrap(),
                RevocationState::new(),
            )
            .unwrap(),
            session.thread_id(),
            codex_protocol::SessionId::from(session.thread_id()),
            crate::security::EffectivePolicyInitialization::Root,
        )
        .expect("initialize the fixture policy");
        let client = (*session.services.model_client())
            .clone()
            .with_ingress_policy(
                SecurityLevel::Moderate,
                session.services.agent_control.effective_security_policy(),
            )
            .with_source_envelopes(/*enabled*/ true);
        session.services.replace_model_client(client);
        session
            .services
            .model_client()
            .note_unrecorded_input_for_taint();
        let ctx = ToolCtx {
            session: Arc::clone(&session),
            turn,
            call_id: codex_protocol::ThreadId::new().to_string(),
            tool_name: codex_tools::ToolName::plain("issue_1048_probe"),
        };
        Self {
            probe: Probe {
                session,
                controller,
                transition_requested,
                transition_pending: AtomicBool::new(false),
                asked: Vec::new(),
                explanations: Vec::new(),
            },
            ctx,
            _home: home,
            _cwd: cwd,
        }
    }

    fn assert_warning(&self, generations: &[u64]) {
        assert_eq!(
            self.probe
                .asked
                .iter()
                .map(|(_, at)| *at)
                .collect::<Vec<_>>(),
            generations,
            "each prompt uses the current taint generation",
        );
        let warning = codex_sandboxing::user_namespaces::moderate_one_run_warning();
        for (reason, _) in &self.probe.asked {
            assert!(reason.contains(&warning), "D48 warning is shown: {reason}");
        }
    }

    fn assert_no_leftover_note(&self) {
        let explanation = crate::security::user_namespaces::denial_explanation(
            &self.ctx.session,
            &self.ctx.turn,
            &self.ctx.call_id,
            Some(1),
            REFUSED,
        );
        assert!(
            explanation.is_some_and(|text| text.starts_with("Security level Moderate")),
            "a stale or already-consumed allowance must not hide a later denial",
        );
    }
}

#[tokio::test]
async fn issue_1048_stale_round_leaves_no_allowed_run_note() {
    let mut fixture = Fixture::new(/*transition_requested*/ true).await;
    let marker = fixture.ctx.session.services.authority_marker();
    let result = ToolOrchestrator::new()
        .run_round(
            &mut fixture.probe,
            &(),
            &fixture.ctx,
            &fixture.ctx.turn,
            AskForApproval::OnRequest,
            /*asked_again*/ false,
            marker,
        )
        .await
        .expect("stale round is not an execution error");
    assert!(matches!(result, Round::Stale { person_asked: true }));
    assert_eq!(
        fixture.probe.explanations,
        Vec::<Option<String>>::new(),
        "nothing ran"
    );
    fixture.assert_warning(&[1]);
    fixture.assert_no_leftover_note();
}

#[tokio::test]
async fn issue_1048_redecided_round_warns_again_and_records_only_the_run() {
    let mut fixture = Fixture::new(/*transition_requested*/ true).await;
    ToolOrchestrator::new()
        .run(
            &mut fixture.probe,
            &(),
            &fixture.ctx,
            &fixture.ctx.turn,
            AskForApproval::OnRequest,
        )
        .await
        .expect("freshly approved second round runs");
    fixture.assert_warning(&[1, 2]);
    assert!(
        fixture.probe.asked[1]
            .0
            .contains(crate::tools::approvals::ASKED_AGAIN_NOTE)
    );
    assert_eq!(
        fixture.probe.explanations,
        vec![None],
        "one authorized run's host denial"
    );
    fixture.assert_no_leftover_note();
}

#[tokio::test]
async fn issue_1048_unchanged_round_consumes_its_one_allowed_run_note() {
    let mut fixture = Fixture::new(/*transition_requested*/ false).await;
    ToolOrchestrator::new()
        .run(
            &mut fixture.probe,
            &(),
            &fixture.ctx,
            &fixture.ctx.turn,
            AskForApproval::OnRequest,
        )
        .await
        .expect("the unchanged round runs");
    fixture.assert_warning(&[1]);
    assert_eq!(
        fixture.probe.explanations,
        vec![None],
        "the host denial is not blamed on policy"
    );
    fixture.assert_no_leftover_note();
}
