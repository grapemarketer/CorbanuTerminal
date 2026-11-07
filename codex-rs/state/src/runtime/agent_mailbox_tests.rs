use super::*;
use codex_protocol::AgentPath;
use codex_protocol::protocol::AgentMessageKind;
use pretty_assertions::assert_eq;

fn communication(message_id: &str) -> InterAgentCommunication {
    let mut communication = InterAgentCommunication::new(
        AgentPath::root(),
        AgentPath::try_from("/root/worker").expect("agent path"),
        Vec::new(),
        "inspect the repository".to_string(),
        /*trigger_turn*/ true,
    )
    .with_kind(AgentMessageKind::Assignment);
    communication.message_id = Some(message_id.to_string());
    communication.created_at_ms = Some(1_000);
    communication
}

#[tokio::test]
async fn mailbox_admission_is_idempotent_and_recoverable() {
    let runtime = StateRuntime::init_for_testing(
        test_support::unique_temp_dir(),
        "test-provider".to_string(),
    )
    .await
    .expect("state runtime");
    let recipient = ThreadId::new();
    let message = communication("message-1");

    assert_eq!(
        runtime
            .admit_agent_message(recipient, &message, /*now_ms*/ 1_000)
            .await
            .expect("first admission"),
        AgentMailboxAdmission::Inserted
    );
    assert_eq!(
        runtime
            .admit_agent_message(recipient, &message, /*now_ms*/ 1_001)
            .await
            .expect("duplicate admission"),
        AgentMailboxAdmission::Existing(AgentMailboxPhase::Admitted)
    );
    let mut timestamp_retry = message.clone();
    timestamp_retry.created_at_ms = Some(9_999);
    assert_eq!(
        runtime
            .admit_agent_message(recipient, &timestamp_retry, /*now_ms*/ 1_001)
            .await
            .expect("server timestamp must not alter logical identity"),
        AgentMailboxAdmission::Existing(AgentMailboxPhase::Admitted)
    );
    assert!(
        runtime
            .transition_agent_message(
                "message-1",
                AgentMailboxPhase::Admitted,
                AgentMailboxPhase::Ready,
                /*now_ms*/ 1_002,
            )
            .await
            .expect("ready transition")
    );
    assert_eq!(
        runtime
            .list_recoverable_agent_messages(recipient)
            .await
            .expect("recoverable messages"),
        vec![AgentMailboxMessage {
            recipient_thread_id: recipient,
            communication: message,
            phase: AgentMailboxPhase::Ready,
            attempt_id: None,
            created_at_ms: 1_000,
            updated_at_ms: 1_002,
        }]
    );
}

#[tokio::test]
async fn mailbox_records_a_new_attempt_without_changing_logical_identity() {
    let runtime = StateRuntime::init_for_testing(
        test_support::unique_temp_dir(),
        "test-provider".to_string(),
    )
    .await
    .expect("state runtime");
    let recipient = ThreadId::new();
    let message = communication("message-1");
    runtime
        .admit_agent_message(recipient, &message, /*now_ms*/ 1_000)
        .await
        .expect("admission");
    assert!(
        runtime
            .transition_agent_message(
                "message-1",
                AgentMailboxPhase::Admitted,
                AgentMailboxPhase::Ready,
                /*now_ms*/ 1_001,
            )
            .await
            .expect("ready")
    );
    assert!(
        runtime
            .begin_agent_message_submission(
                "message-1",
                AgentMailboxPhase::Ready,
                "attempt-1",
                /*now_ms*/ 1_002,
            )
            .await
            .expect("begin attempt")
    );

    let messages = runtime
        .list_recoverable_agent_messages(recipient)
        .await
        .expect("recoverable");
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].phase, AgentMailboxPhase::Submitting);
    assert_eq!(messages[0].attempt_id.as_deref(), Some("attempt-1"));
    assert_eq!(
        messages[0].communication.message_id.as_deref(),
        Some("message-1")
    );
}

#[tokio::test]
async fn completed_mailbox_message_is_terminal_but_remains_auditable() {
    let runtime = StateRuntime::init_for_testing(
        test_support::unique_temp_dir(),
        "test-provider".to_string(),
    )
    .await
    .expect("state runtime");
    let recipient = ThreadId::new();
    let message = communication("message-1");
    runtime
        .admit_agent_message(recipient, &message, /*now_ms*/ 1_000)
        .await
        .expect("admission");
    assert!(
        runtime
            .mark_agent_message_completed("message-1", /*now_ms*/ 1_001)
            .await
            .expect("completion")
    );
    assert_eq!(
        runtime
            .list_recoverable_agent_messages(recipient)
            .await
            .expect("recoverable"),
        Vec::new()
    );
    let stored = runtime
        .get_agent_message("message-1")
        .await
        .expect("lookup")
        .expect("stored message");
    assert_eq!(stored.phase, AgentMailboxPhase::Completed);
    assert_eq!(stored.communication, message);
}

#[tokio::test]
async fn mailbox_rejects_message_id_reuse_for_different_content() {
    let runtime = StateRuntime::init_for_testing(
        test_support::unique_temp_dir(),
        "test-provider".to_string(),
    )
    .await
    .expect("state runtime");
    let recipient = ThreadId::new();
    let first = communication("message-1");
    runtime
        .admit_agent_message(recipient, &first, /*now_ms*/ 1_000)
        .await
        .expect("first admission");
    let mut conflicting = first;
    conflicting.content = "different work".to_string();

    let error = runtime
        .admit_agent_message(recipient, &conflicting, /*now_ms*/ 1_001)
        .await
        .expect_err("message-id collision must fail");
    assert!(
        error
            .to_string()
            .contains("conflicts with a different message")
    );
}

async fn admit(
    runtime: &StateRuntime,
    recipient: ThreadId,
    message: &InterAgentCommunication,
) -> anyhow::Result<AgentMailboxAdmission> {
    runtime
        .admit_agent_message(recipient, message, /*now_ms*/ 1_000)
        .await
}

fn labelled(message: &InterAgentCommunication, label: Option<&str>) -> InterAgentCommunication {
    let mut message = message.clone();
    message.author_label = label.map(str::to_string);
    message
}

/// #1335: a message persisted before `author_label` existed deserializes without one; the same
/// message retried by a build that labels cross-tree mail is the same logical message.
#[tokio::test]
async fn legacy_unlabelled_message_matches_a_labelled_retry() {
    let runtime = StateRuntime::init_for_testing(
        test_support::unique_temp_dir(),
        "test-provider".to_string(),
    )
    .await
    .expect("state runtime");
    let recipient = ThreadId::new();
    let legacy = communication("message-1");
    let legacy_json = serde_json::to_string(&legacy).expect("serialize legacy message");
    assert!(
        !legacy_json.contains("author_label"),
        "an unlabelled row must look like a pre-#1263 row: {legacy_json}"
    );
    assert_eq!(
        admit(&runtime, recipient, &legacy)
            .await
            .expect("legacy admission"),
        AgentMailboxAdmission::Inserted
    );

    let mut retry = labelled(&legacy, Some("agent /root in another pane's agent tree"));
    retry.created_at_ms = Some(9_999);
    assert_eq!(
        admit(&runtime, recipient, &retry)
            .await
            .expect("a labelled retry of a legacy message is the same message"),
        AgentMailboxAdmission::Existing(AgentMailboxPhase::Admitted)
    );
    let stored = runtime
        .get_agent_message("message-1")
        .await
        .expect("lookup")
        .expect("stored message");
    assert_eq!(stored.communication, legacy);
}

/// The rule is one-directional: a labelled row retried without a label conflicts. The label is
/// the only field naming the sending thread, and the incoming copy is the one delivered, so an
/// unlabelled retry would reach the model without its `Sender:` line.
#[tokio::test]
async fn labelled_message_rejects_an_unlabelled_retry() {
    let runtime = StateRuntime::init_for_testing(
        test_support::unique_temp_dir(),
        "test-provider".to_string(),
    )
    .await
    .expect("state runtime");
    let recipient = ThreadId::new();
    let stored = labelled(&communication("message-1"), Some("pane"));
    admit(&runtime, recipient, &stored)
        .await
        .expect("labelled admission");

    let error = admit(&runtime, recipient, &labelled(&stored, /*label*/ None))
        .await
        .expect_err(
            "an unlabelled retry of a labelled message would be delivered without its sender",
        );
    assert!(
        error
            .to_string()
            .contains("conflicts with a different message"),
        "{error}"
    );
}

#[tokio::test]
async fn mailbox_still_rejects_a_different_label_or_field_on_message_id_reuse() {
    let runtime = StateRuntime::init_for_testing(
        test_support::unique_temp_dir(),
        "test-provider".to_string(),
    )
    .await
    .expect("state runtime");
    let recipient = ThreadId::new();
    let labelled_a = labelled(&communication("labelled"), Some("a"));
    admit(&runtime, recipient, &labelled_a)
        .await
        .expect("labelled admission");
    let unlabelled = communication("unlabelled");
    admit(&runtime, recipient, &unlabelled)
        .await
        .expect("unlabelled admission");

    let mut different_content = labelled(&unlabelled, Some("a"));
    different_content.content = "different work".to_string();
    let mut different_author = labelled(&unlabelled, Some("a"));
    different_author.author = AgentPath::try_from("/root/other").expect("agent path");
    let mut different_content_same_label = labelled_a.clone();
    different_content_same_label.content = "different work".to_string();
    for (case, conflicting) in [
        ("a different known label", labelled(&labelled_a, Some("b"))),
        (
            "a different body with the same label",
            different_content_same_label,
        ),
        ("a different body on a legacy row", different_content),
        ("a different author on a legacy row", different_author),
    ] {
        let error = admit(&runtime, recipient, &conflicting)
            .await
            .expect_err(case);
        assert!(
            error
                .to_string()
                .contains("conflicts with a different message"),
            "{case}: {error}"
        );
    }
}
