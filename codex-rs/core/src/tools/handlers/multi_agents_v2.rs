//! Implements the MultiAgentV2 collaboration tool surface.

use crate::agent::AgentStatus;
use crate::agent::agent_resolver::resolve_agent_target;
use crate::context::ContextualUserFragment;
use crate::context::InterAgentMessage;
use crate::context::InterAgentMessageType;
use crate::function_tool::FunctionCallError;
use crate::session::turn_context::TurnContext;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::multi_agents_common::*;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_model_provider_info::OPENAI_PROVIDER_ID;
use codex_protocol::AgentPath;
use codex_protocol::items::CollabAgentTool;
use codex_protocol::items::CollabAgentToolCallItem;
use codex_protocol::items::CollabAgentToolCallStatus;
use codex_protocol::items::SubAgentActivityItem;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::SubAgentActivityKind;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;

pub(crate) use followup_task::Handler as FollowupTaskHandler;
pub(crate) use followup_task::PlaintextHandler as PlaintextFollowupTaskHandler;
pub(crate) use interrupt_agent::Handler as InterruptAgentHandler;
pub(crate) use list_agents::Handler as ListAgentsHandler;
pub(crate) use send_message::Handler as SendMessageHandler;
pub(crate) use send_message::PlaintextHandler as PlaintextSendMessageHandler;
pub(crate) use spawn::Handler as SpawnAgentHandler;
pub(crate) use spawn::PlaintextHandler as PlaintextSpawnAgentHandler;
pub(crate) use wait::Handler as WaitAgentHandler;

mod followup_task;
mod interrupt_agent;
mod list_agents;
pub(crate) mod message_tool;
mod send_message;
mod spawn;
pub(crate) mod wait;

#[cfg(test)]
#[path = "multi_agents_v2_source_identity_tests.rs"]
mod source_identity_tests;

pub(crate) const PLAINTEXT_SPAWN_AGENT_TOOL: &str = "spawn_agent_plaintext";
pub(crate) const PLAINTEXT_SEND_MESSAGE_TOOL: &str = "send_message_plaintext";
pub(crate) const PLAINTEXT_FOLLOWUP_TASK_TOOL: &str = "followup_task_plaintext";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CollaborationMessageEncoding {
    ProviderNative,
    PlaintextAdapter,
}

pub(super) fn plaintext_adapter_spec(
    spec: ToolSpec,
    adapter_name: &str,
    native_name: &str,
) -> ToolSpec {
    match spec {
        ToolSpec::Function(mut tool) => {
            tool.name = adapter_name.to_string();
            tool.description = format!(
                "Plaintext adapter for `{native_name}`. Use this for explicit provider/model selection (the native OpenAI spawn schema has no runtime override fields), or to deliver an ordinary task brief to another provider. It supports OpenAI recipients too. Prefer the native encrypted tool when inheriting an OpenAI runtime. Supply a fresh plaintext task brief, never native encrypted content, credentials or other secrets. It performs the same authorized Core graph/mailbox operation. {}",
                tool.description
            );
            ToolSpec::Function(tool)
        }
        other => other,
    }
}

/// A collaborator's configured provider id and account (`None` is the
/// default account), as encrypted collaboration content is bound to both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ConfiguredRuntime<'a> {
    pub(super) provider_id: &'a str,
    pub(super) account: Option<RuntimeAccount<'a>>,
}

/// A named account as `<provider>:<name>`: an account carried onto another
/// connection keeps the provider it belongs to, so two accounts are the same
/// only when both match (#1127 review).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct RuntimeAccount<'a> {
    pub(super) provider_id: &'a str,
    pub(super) name: &'a str,
}

impl<'a> ConfiguredRuntime<'a> {
    /// `provider_id` on its own account `account` (its name; `None` is the
    /// default account).
    #[cfg(test)]
    pub(super) fn new(provider_id: &'a str, account: Option<&'a str>) -> Self {
        Self {
            provider_id,
            account: account.map(|name| RuntimeAccount { provider_id, name }),
        }
    }

    /// `provider_id` on its default account.
    #[cfg(test)]
    pub(super) fn on_default(provider_id: &'a str) -> Self {
        Self::new(provider_id, /*account*/ None)
    }

    /// `provider_id` running on `provider`'s account, with the provider that
    /// account belongs to.
    pub(super) fn from_provider(
        provider_id: &'a str,
        provider: &'a codex_model_provider_info::ModelProviderInfo,
    ) -> Self {
        Self {
            provider_id,
            account: provider.account.as_ref().map(|account| RuntimeAccount {
                provider_id: account.provider_id.as_str(),
                name: account.name.as_str(),
            }),
        }
    }

    /// From a thread snapshot's `<provider>:<name>` account.
    pub(super) fn from_snapshot(provider_id: &'a str, provider_account: Option<&'a str>) -> Self {
        let account = provider_account
            .and_then(|selection| selection.split_once(':'))
            .filter(|(_, name)| *name != "default")
            .map(|(provider_id, name)| RuntimeAccount { provider_id, name });
        Self {
            provider_id,
            account,
        }
    }

    /// The account as shown: its name, qualified when it belongs to another
    /// provider than the one it runs on.
    fn shown_account(&self) -> String {
        match self.account {
            None => "default".to_string(),
            Some(account) if account.provider_id == self.provider_id => account.name.to_string(),
            Some(account) => format!("{}:{}", account.provider_id, account.name),
        }
    }
}

/// Refuses native (encrypted) collaboration content for a target that cannot
/// decrypt it. `configured_source` is the sender's configured provider and
/// account; `source_codec_id` is the codec its messages use (`openai` for an
/// OpenAI-compatible endpoint with the reserved schema, see
/// `collaboration_wire::message_source_provider_id`). An OpenAI-encrypted
/// payload is admitted only for a target on the same configured provider and
/// the same account (#1016).
pub(super) fn ensure_message_encoding_matches_target(
    configured_source: ConfiguredRuntime<'_>,
    source_codec_id: &str,
    source: &crate::tools::context::ToolCallSource,
    target: ConfiguredRuntime<'_>,
    encoding: CollaborationMessageEncoding,
    native_tool_name: &str,
    plaintext_tool_name: &str,
) -> Result<(), FunctionCallError> {
    if encoding == CollaborationMessageEncoding::PlaintextAdapter {
        // Encoding belongs to the declared tool surface, not the recipient's ability
        // to also consume encrypted assignments. OpenAI accepts plaintext agent
        // messages, and its reserved native spawn schema cannot select a model.
        return Ok(());
    }

    // Wire compatibility selects the codec; it does not make a custom provider
    // the same configured source as the built-in OpenAI provider, nor another
    // account of the same provider the same source.
    let source_is_encrypted_openai_call = source_codec_id == OPENAI_PROVIDER_ID
        && matches!(source, crate::tools::context::ToolCallSource::Direct);
    if !source_is_encrypted_openai_call || target == configured_source {
        return Ok(());
    }
    let target_provider_id = target.provider_id;
    let recipient = if target_provider_id == configured_source.provider_id {
        format!(
            "target account `{}` of `{target_provider_id}` (this agent runs on account `{}`)",
            target.shown_account(),
            configured_source.shown_account(),
        )
    } else {
        format!("target provider `{target_provider_id}`")
    };
    Err(FunctionCallError::RespondToModel(format!(
        "{recipient} cannot consume this OpenAI-encrypted collaboration payload. Call the function named `{plaintext_tool_name}` directly with exactly the same `target` and the plaintext `message`; do not call `{native_tool_name}` again and do not add an `adapter` field. No message was admitted and no target turn was started."
    )))
}

pub(crate) async fn emit_sub_agent_activity(
    session: &crate::session::session::Session,
    turn: &crate::session::turn_context::TurnContext,
    item: SubAgentActivityItem,
) {
    let item = TurnItem::SubAgentActivity(item);
    session.emit_turn_item_started(turn, &item).await;
    session.emit_turn_item_completed(turn, item).await;
}

fn communication_from_tool_message(
    author: AgentPath,
    recipient: AgentPath,
    message: String,
    source: &crate::tools::context::ToolCallSource,
    provider_id: &str,
    trigger_turn: bool,
) -> InterAgentCommunication {
    if provider_id == OPENAI_PROVIDER_ID
        && matches!(source, crate::tools::context::ToolCallSource::Direct)
    {
        return InterAgentCommunication::new_encrypted(
            author,
            recipient,
            Vec::new(),
            message,
            trigger_turn,
        );
    }
    let message_type = if trigger_turn {
        InterAgentMessageType::NewTask
    } else {
        InterAgentMessageType::Message
    };
    let content =
        InterAgentMessage::new(message_type, recipient.clone(), author.clone(), message).render();
    InterAgentCommunication::new(author, recipient, Vec::new(), content, trigger_turn)
}

pub(super) fn ensure_manager_tool_allowed(
    _turn: &TurnContext,
    _tool_name: &str,
) -> Result<(), FunctionCallError> {
    // Role names are behavioral profiles and UI labels, not authorization tokens. Delegation
    // permissions belong to the crew policy at the Corbanu Terminal control-plane boundary.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_plaintext_new_task(source: crate::tools::context::ToolCallSource, provider_id: &str) {
        let communication = communication_from_tool_message(
            AgentPath::root(),
            AgentPath::try_from("/root/worker").expect("agent path"),
            "do the delegated work".to_string(),
            &source,
            provider_id,
            /*trigger_turn*/ true,
        );

        assert_eq!(communication.encrypted_content, None);
        assert_eq!(
            communication.content,
            "Message Type: NEW_TASK\nTask name: /root/worker\nSender: /root\nPayload:\ndo the delegated work"
        );
        assert!(matches!(
            communication.to_model_input_item(),
            codex_protocol::models::ResponseItem::AgentMessage { content, .. }
                if matches!(content.as_slice(), [codex_protocol::models::AgentMessageInputContent::InputText { text }] if text.contains("do the delegated work"))
        ));
    }

    #[test]
    fn openai_collaboration_tool_payload_preserves_native_encryption() {
        let communication = communication_from_tool_message(
            AgentPath::root(),
            AgentPath::try_from("/root/worker").expect("agent path"),
            "opaque-ciphertext".to_string(),
            &crate::tools::context::ToolCallSource::Direct,
            OPENAI_PROVIDER_ID,
            /*trigger_turn*/ true,
        );

        assert!(communication.content.is_empty());
        assert_eq!(
            communication.encrypted_content.as_deref(),
            Some("opaque-ciphertext")
        );
    }

    #[test]
    fn non_openai_collaboration_tool_payload_is_plaintext() {
        let communication = communication_from_tool_message(
            AgentPath::try_from("/root/worker").expect("agent path"),
            AgentPath::root(),
            "ordinary provider report".to_string(),
            &crate::tools::context::ToolCallSource::Direct,
            "zai",
            /*trigger_turn*/ true,
        );

        assert_eq!(
            communication.content,
            "Message Type: NEW_TASK\nTask name: /root\nSender: /root/worker\nPayload:\nordinary provider report"
        );
        assert_eq!(communication.encrypted_content, None);
        assert!(matches!(
            communication.to_model_input_item(),
            codex_protocol::models::ResponseItem::AgentMessage { content, .. }
                if matches!(content.as_slice(), [codex_protocol::models::AgentMessageInputContent::InputText { text }] if text.contains("ordinary provider report"))
        ));
    }

    #[test]
    fn non_openai_spawn_task_is_provider_neutral_plaintext() {
        assert_plaintext_new_task(crate::tools::context::ToolCallSource::Direct, "deepseek");
        assert_plaintext_new_task(
            crate::tools::context::ToolCallSource::CodeMode {
                cell_id: "cell-1".to_string(),
                runtime_tool_call_id: "tool-1".to_string(),
            },
            "deepseek",
        );
    }

    #[test]
    fn explicitly_plaintext_openai_spawn_task_remains_plaintext() {
        assert_plaintext_new_task(
            crate::tools::context::ToolCallSource::DirectPlaintextMessage,
            OPENAI_PROVIDER_ID,
        );
    }

    #[test]
    fn code_mode_openai_message_is_plaintext() {
        assert_plaintext_new_task(
            crate::tools::context::ToolCallSource::CodeMode {
                cell_id: "cell-1".to_string(),
                runtime_tool_call_id: "tool-1".to_string(),
            },
            OPENAI_PROVIDER_ID,
        );
    }

    #[test]
    fn explicit_plaintext_assignments_support_same_and_cross_provider_recipients() {
        for target in [OPENAI_PROVIDER_ID, "kimi-code", "custom-provider"] {
            assert_eq!(
                ensure_message_encoding_matches_target(
                    ConfiguredRuntime::on_default(OPENAI_PROVIDER_ID),
                    OPENAI_PROVIDER_ID,
                    &crate::tools::context::ToolCallSource::Direct,
                    ConfiguredRuntime::on_default(target),
                    CollaborationMessageEncoding::PlaintextAdapter,
                    "spawn_agent",
                    PLAINTEXT_SPAWN_AGENT_TOOL,
                ),
                Ok(())
            );
        }
    }

    #[test]
    fn encrypted_openai_message_requires_plaintext_adapter_for_external_target() {
        let err = ensure_message_encoding_matches_target(
            ConfiguredRuntime::on_default(OPENAI_PROVIDER_ID),
            OPENAI_PROVIDER_ID,
            &crate::tools::context::ToolCallSource::Direct,
            ConfiguredRuntime::on_default("openrouter"),
            CollaborationMessageEncoding::ProviderNative,
            "followup_task",
            PLAINTEXT_FOLLOWUP_TASK_TOOL,
        )
        .expect_err("encrypted payload must fail before cross-provider admission");
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "target provider `openrouter` cannot consume this OpenAI-encrypted collaboration payload. Call the function named `followup_task_plaintext` directly with exactly the same `target` and the plaintext `message`; do not call `followup_task` again and do not add an `adapter` field. No message was admitted and no target turn was started.".to_string()
            )
        );
    }

    #[test]
    fn encrypted_openai_spawn_task_remains_encrypted() {
        let communication = communication_from_tool_message(
            AgentPath::root(),
            AgentPath::try_from("/root/worker").expect("agent path"),
            "opaque-ciphertext".to_string(),
            &crate::tools::context::ToolCallSource::Direct,
            OPENAI_PROVIDER_ID,
            /*trigger_turn*/ true,
        );

        assert!(communication.content.is_empty());
        assert_eq!(
            communication.encrypted_content.as_deref(),
            Some("opaque-ciphertext")
        );
    }

    #[tokio::test]
    async fn issue820_custom_reserved_messages_preserve_codec_and_fail_closed_targets() {
        let (_session, mut turn) = crate::session::tests::make_session_and_context().await;
        let mut config = (*turn.config).clone();
        // The exact #820 provider entry, with a synthetic unset key.
        let provider_info: codex_model_provider_info::ModelProviderInfo = toml::from_str(
            r#"
name = "Custom official endpoint"
base_url = "https://api.openai.com/v1"
env_key = "ISSUE820_SYNTHETIC_UNSET_KEY"
wire_api = "responses"
billing = "pay_per_use"
"#,
        )
        .expect("synthetic #820 provider config");
        config.model_provider_id = "myopenai".to_string();
        config.model_provider = provider_info;
        turn.provider = codex_model_provider::create_model_provider(
            config.model_provider.clone(),
            turn.auth_manager.clone(),
        );
        turn.config = std::sync::Arc::new(config);
        let codec_provider = crate::tools::collaboration_wire::message_source_provider_id(&turn);
        assert_eq!(turn.config.model_provider_id, "myopenai");
        assert!(!turn.provider.info().is_openai());
        assert_eq!(codec_provider, OPENAI_PROVIDER_ID);
        let source = crate::tools::context::ToolCallSource::Direct;
        let communication = communication_from_tool_message(
            AgentPath::root(),
            AgentPath::try_from("/root/worker").expect("agent path"),
            "synthetic-opaque-payload".to_string(),
            &source,
            codec_provider,
            /*trigger_turn*/ true,
        );
        assert!(communication.content.is_empty());
        assert_eq!(
            communication.encrypted_content.as_deref(),
            Some("synthetic-opaque-payload")
        );
        let codex_protocol::models::ResponseItem::AgentMessage { content, .. } =
            communication.to_model_input_item()
        else {
            panic!("encrypted collaboration must serialize as AgentMessage");
        };
        pretty_assertions::assert_eq!(
            content,
            vec![
                codex_protocol::models::AgentMessageInputContent::InputText {
                    text:
                        "Message Type: NEW_TASK\nTask name: /root/worker\nSender: /root\nPayload:\n"
                            .to_string(),
                },
                codex_protocol::models::AgentMessageInputContent::EncryptedContent {
                    encrypted_content: "synthetic-opaque-payload".to_string(),
                },
            ]
        );
        for target in [OPENAI_PROVIDER_ID, "openrouter"] {
            assert_eq!(
                ensure_message_encoding_matches_target(
                    ConfiguredRuntime::on_default(&turn.config.model_provider_id),
                    codec_provider,
                    &source,
                    ConfiguredRuntime::on_default(target),
                    CollaborationMessageEncoding::ProviderNative,
                    "followup_task",
                    PLAINTEXT_FOLLOWUP_TASK_TOOL,
                ),
                Err(FunctionCallError::RespondToModel(format!(
                    "target provider `{target}` cannot consume this OpenAI-encrypted collaboration payload. Call the function named `followup_task_plaintext` directly with exactly the same `target` and the plaintext `message`; do not call `followup_task` again and do not add an `adapter` field. No message was admitted and no target turn was started."
                )))
            );
            assert_eq!(
                ensure_message_encoding_matches_target(
                    ConfiguredRuntime::on_default(&turn.config.model_provider_id),
                    codec_provider,
                    &source,
                    ConfiguredRuntime::on_default(target),
                    CollaborationMessageEncoding::PlaintextAdapter,
                    "followup_task",
                    PLAINTEXT_FOLLOWUP_TASK_TOOL,
                ),
                Ok(())
            );
        }
        assert_plaintext_new_task(
            crate::tools::context::ToolCallSource::DirectPlaintextMessage,
            codec_provider,
        );
    }
}
