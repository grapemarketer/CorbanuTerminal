//! Source-informed issue863 regression. Provider data is synthetic; no inference.
use super::*;
use crate::tools::collaboration_wire::message_source_provider_id;
use pretty_assertions::assert_eq;

/// A turn on `myopenai`, a custom provider at the official OpenAI endpoint.
async fn custom_openai_endpoint_turn() -> crate::session::turn_context::TurnContext {
    let (_session, mut turn) = crate::session::tests::make_session_and_context().await;
    let mut config = (*turn.config).clone();
    config.model_provider_id = "myopenai".to_string();
    config.model_provider.name = "My OpenAI".to_string();
    config.model_provider.base_url = Some("https://api.openai.com/v1".to_string());
    config.model_provider.wire_api = codex_model_provider_info::WireApi::Responses;
    config.model_provider.requires_openai_auth = false;
    config.model_provider.env_key = Some("SYNTHETIC_ISSUE863_API_KEY".to_string());
    turn.provider = codex_model_provider::create_model_provider(
        config.model_provider.clone(),
        turn.auth_manager.clone(),
    );
    turn.config = std::sync::Arc::new(config);
    turn
}

#[tokio::test]
async fn issue863_native_custom_same_source_is_admitted() {
    let turn = custom_openai_endpoint_turn().await;
    assert_eq!(turn.config.model_provider_id, "myopenai");
    assert_eq!(message_source_provider_id(&turn), OPENAI_PROVIDER_ID);
    assert_eq!(
        ensure_message_encoding_matches_target(
            ConfiguredRuntime::on_default(&turn.config.model_provider_id),
            message_source_provider_id(&turn),
            &crate::tools::context::ToolCallSource::Direct,
            ConfiguredRuntime::on_default("myopenai"),
            CollaborationMessageEncoding::ProviderNative,
            "spawn_agent",
            PLAINTEXT_SPAWN_AGENT_TOOL,
        ),
        Ok(())
    );
}

#[tokio::test]
async fn issue863_native_custom_to_builtin_source_is_rejected_before_admission() {
    let turn = custom_openai_endpoint_turn().await;
    assert_eq!(
        ensure_message_encoding_matches_target(
            ConfiguredRuntime::on_default(&turn.config.model_provider_id),
            message_source_provider_id(&turn),
            &crate::tools::context::ToolCallSource::Direct,
            ConfiguredRuntime::on_default(OPENAI_PROVIDER_ID),
            CollaborationMessageEncoding::ProviderNative,
            "followup_task",
            PLAINTEXT_FOLLOWUP_TASK_TOOL,
        ),
        Err(FunctionCallError::RespondToModel(
            "target provider `openai` cannot consume this OpenAI-encrypted collaboration payload. Call the function named `followup_task_plaintext` directly with exactly the same `target` and the plaintext `message`; do not call `followup_task` again and do not add an `adapter` field. No message was admitted and no target turn was started.".to_string()
        ))
    );
}

#[test]
fn issue863_existing_builtin_and_plaintext_controls() {
    let source = crate::tools::context::ToolCallSource::Direct;
    for (target, encoding) in [
        (
            OPENAI_PROVIDER_ID,
            CollaborationMessageEncoding::ProviderNative,
        ),
        ("myopenai", CollaborationMessageEncoding::PlaintextAdapter),
    ] {
        assert_eq!(
            ensure_message_encoding_matches_target(
                ConfiguredRuntime::on_default(OPENAI_PROVIDER_ID),
                OPENAI_PROVIDER_ID,
                &source,
                ConfiguredRuntime::on_default(target),
                encoding,
                "spawn_agent",
                PLAINTEXT_SPAWN_AGENT_TOOL,
            ),
            Ok(())
        );
    }
}

#[tokio::test]
async fn issue863_reserved_schema_endpoint_matrix_stays_conservative() {
    use crate::tools::collaboration_wire::uses_openai_reserved_collaboration_schema;
    use codex_model_provider_info::WireApi;

    let (_session, mut turn) = crate::session::tests::make_session_and_context().await;
    let cases = [
        ("https://api.openai.com/v1", true),
        ("https://api.openai.com/v1/", true),
        ("https://api.openai.com:443/v1", true),
        // #863 item 3: the fully qualified host and OpenAI's documented regional domains.
        ("https://api.openai.com./v1", true),
        ("https://API.OPENAI.COM./v1/", true),
        ("https://eu.api.openai.com/v1", true),
        ("https://EU.API.OPENAI.COM/v1", true),
        ("https://us.api.openai.com/v1", true),
        ("https://jp.api.openai.com:443/v1", true),
        ("https://eu.api.openai.com./v1", true),
        ("https://ae.api.openai.com/v1", true),
        ("https://au.api.openai.com/v1", true),
        ("https://ca.api.openai.com/v1", true),
        ("https://gb.api.openai.com/v1", true),
        ("https://in.api.openai.com/v1", true),
        ("https://kr.api.openai.com/v1", true),
        ("https://sg.api.openai.com/v1", true),
        ("http://api.openai.com/v1", false),
        ("http://eu.api.openai.com/v1", false),
        ("https://api.openai.com../v1", false),
        ("https://xx.api.openai.com/v1", false),
        ("https://.api.openai.com/v1", false),
        ("https://\u{0430}pi.openai.com/v1", false),
        ("https://xn--eu-api.openai.com/v1", false),
        ("https://162.159.140.245/v1", false),
        ("https://[2606:4700::6812:7261]/v1", false),
        ("https://eu.eu.api.openai.com/v1", false),
        ("https://euapi.openai.com/v1", false),
        ("https://eu.api.openai.com.example.test/v1", false),
        ("https://eu.api.openai.com:444/v1", false),
        ("https://eu.api.openai.com/v2", false),
        ("https://eu.api.openai.com/proxy/v1", false),
        ("https://synthetic@eu.api.openai.com/v1", false),
        ("https://gateway.example/v1", false),
        ("https://api.openai.com:444/v1", false),
        ("https://api.openai.com/v2", false),
        ("https://api.openai.com/v1?tenant=synthetic", false),
        ("https://api.openai.com/v1#synthetic", false),
        ("https://synthetic@api.openai.com/v1", false),
        ("https://user:synthetic@api.openai.com/v1", false),
    ];
    for wire in [WireApi::Responses, WireApi::Chat, WireApi::Anthropic] {
        for (url, responses_supported) in cases {
            let mut config = (*turn.config).clone();
            config.model_provider_id = "myopenai".to_string();
            config.model_provider.name = "My OpenAI".to_string();
            config.model_provider.base_url = Some(url.to_string());
            config.model_provider.wire_api = wire;
            config.model_provider.requires_openai_auth = false;
            turn.provider = codex_model_provider::create_model_provider(
                config.model_provider.clone(),
                turn.auth_manager.clone(),
            );
            turn.config = std::sync::Arc::new(config);
            let expected = wire == WireApi::Responses && responses_supported;
            assert_eq!(
                uses_openai_reserved_collaboration_schema(&turn),
                expected,
                "{wire}: {url}"
            );
            assert_eq!(turn.config.model_provider_id, "myopenai");
            assert_eq!(
                message_source_provider_id(&turn),
                if expected {
                    OPENAI_PROVIDER_ID
                } else {
                    "myopenai"
                }
            );
        }
    }
}

#[test]
fn issue863_real_source_codec_target_and_plaintext_matrix() {
    let source = crate::tools::context::ToolCallSource::Direct;
    for (real_source, codec, target, encoding, admitted) in [
        (
            "myopenai",
            OPENAI_PROVIDER_ID,
            "myopenai",
            CollaborationMessageEncoding::ProviderNative,
            true,
        ),
        (
            "myopenai",
            OPENAI_PROVIDER_ID,
            OPENAI_PROVIDER_ID,
            CollaborationMessageEncoding::ProviderNative,
            false,
        ),
        (
            "myopenai",
            OPENAI_PROVIDER_ID,
            "other-custom",
            CollaborationMessageEncoding::ProviderNative,
            false,
        ),
        (
            OPENAI_PROVIDER_ID,
            OPENAI_PROVIDER_ID,
            OPENAI_PROVIDER_ID,
            CollaborationMessageEncoding::ProviderNative,
            true,
        ),
        (
            OPENAI_PROVIDER_ID,
            OPENAI_PROVIDER_ID,
            "myopenai",
            CollaborationMessageEncoding::ProviderNative,
            false,
        ),
        (
            "myopenai",
            OPENAI_PROVIDER_ID,
            OPENAI_PROVIDER_ID,
            CollaborationMessageEncoding::PlaintextAdapter,
            true,
        ),
        (
            "myopenai",
            OPENAI_PROVIDER_ID,
            "other-custom",
            CollaborationMessageEncoding::PlaintextAdapter,
            true,
        ),
        (
            "local-provider",
            "local-provider",
            "other-custom",
            CollaborationMessageEncoding::ProviderNative,
            true,
        ),
    ] {
        let expected = if admitted {
            Ok(())
        } else {
            Err(FunctionCallError::RespondToModel(format!(
                "target provider `{target}` cannot consume this OpenAI-encrypted collaboration payload. Call the function named `followup_task_plaintext` directly with exactly the same `target` and the plaintext `message`; do not call `followup_task` again and do not add an `adapter` field. No message was admitted and no target turn was started."
            )))
        };
        assert_eq!(
            ensure_message_encoding_matches_target(
                ConfiguredRuntime::on_default(real_source),
                codec,
                &source,
                ConfiguredRuntime::on_default(target),
                encoding,
                "followup_task",
                PLAINTEXT_FOLLOWUP_TASK_TOOL,
            ),
            expected
        );
    }
    for source in [
        crate::tools::context::ToolCallSource::DirectPlaintextMessage,
        crate::tools::context::ToolCallSource::CodeMode {
            cell_id: "synthetic-cell".to_string(),
            runtime_tool_call_id: "synthetic-tool".to_string(),
        },
    ] {
        assert_eq!(
            ensure_message_encoding_matches_target(
                ConfiguredRuntime::on_default("myopenai"),
                OPENAI_PROVIDER_ID,
                &source,
                ConfiguredRuntime::on_default("other-custom"),
                CollaborationMessageEncoding::ProviderNative,
                "followup_task",
                PLAINTEXT_FOLLOWUP_TASK_TOOL,
            ),
            Ok(())
        );
    }
}

/// #1016: an OpenAI-encrypted payload is bound to the account as well as the
/// provider: another account of the same provider is refused, the same
/// account is admitted, and the plaintext adapter is unaffected.
#[test]
fn issue1016_encrypted_payload_compares_provider_and_account() {
    let source = crate::tools::context::ToolCallSource::Direct;
    let refusal = |target: &str, source: &str| {
        Err(FunctionCallError::RespondToModel(format!(
            "target account `{target}` of `myopenai` (this agent runs on account `{source}`) cannot consume this OpenAI-encrypted collaboration payload. Call the function named `followup_task_plaintext` directly with exactly the same `target` and the plaintext `message`; do not call `followup_task` again and do not add an `adapter` field. No message was admitted and no target turn was started."
        )))
    };
    for (source_account, target_account, expected) in [
        (None, None, Ok(())),
        (Some("work"), Some("work"), Ok(())),
        (None, Some("work"), refusal("work", "default")),
        (Some("work"), None, refusal("default", "work")),
        (Some("work"), Some("home"), refusal("home", "work")),
    ] {
        let check = |encoding| {
            ensure_message_encoding_matches_target(
                ConfiguredRuntime::new("myopenai", source_account),
                OPENAI_PROVIDER_ID,
                &source,
                ConfiguredRuntime::new("myopenai", target_account),
                encoding,
                "followup_task",
                PLAINTEXT_FOLLOWUP_TASK_TOOL,
            )
        };
        assert_eq!(
            check(CollaborationMessageEncoding::ProviderNative),
            expected,
            "{source_account:?} -> {target_account:?}"
        );
        assert_eq!(
            check(CollaborationMessageEncoding::PlaintextAdapter),
            Ok(())
        );
    }
    // A codec other than OpenAI's carries no encrypted payload.
    assert_eq!(
        ensure_message_encoding_matches_target(
            ConfiguredRuntime::new("local-provider", /*account*/ None),
            "local-provider",
            &source,
            ConfiguredRuntime::new("local-provider", Some("work")),
            CollaborationMessageEncoding::ProviderNative,
            "followup_task",
            PLAINTEXT_FOLLOWUP_TASK_TOOL,
        ),
        Ok(())
    );
}

#[test]
fn issue1016_snapshot_account_is_read_as_provider_and_name() {
    for (snapshot, account) in [
        (None, None),
        (Some("myopenai:default"), None),
        (Some("myopenai:work"), Some("work")),
        (Some("malformed"), None),
    ] {
        assert_eq!(
            ConfiguredRuntime::from_snapshot("myopenai", snapshot),
            ConfiguredRuntime::new("myopenai", account),
            "{snapshot:?}"
        );
    }
    // An account of another provider carried onto `myopenai` keeps its
    // provider.
    assert_ne!(
        ConfiguredRuntime::from_snapshot("myopenai", Some("openai:work")),
        ConfiguredRuntime::new("myopenai", Some("work")),
    );
}

/// #1127 review: accounts compare as `<provider>:<name>`. An account `work`
/// of another provider carried onto `myopenai` (`with_account_from`) is not
/// `myopenai`'s own `work`, and the refusal names it qualified.
#[test]
fn issue1127_same_name_account_of_another_provider_is_another_account() {
    let mut carried = codex_model_provider_info::ModelProviderInfo::create_openai_provider(
        /*base_url*/ None,
    );
    carried.account = Some(codex_model_provider_info::NamedProviderAccount {
        provider_id: OPENAI_PROVIDER_ID.to_string(),
        name: "work".to_string(),
    });
    let source = crate::tools::context::ToolCallSource::Direct;
    let check = |target| {
        ensure_message_encoding_matches_target(
            ConfiguredRuntime::from_provider("myopenai", &carried),
            OPENAI_PROVIDER_ID,
            &source,
            target,
            CollaborationMessageEncoding::ProviderNative,
            "followup_task",
            PLAINTEXT_FOLLOWUP_TASK_TOOL,
        )
    };
    assert_eq!(
        check(ConfiguredRuntime::from_snapshot(
            "myopenai",
            Some("myopenai:work")
        )),
        Err(FunctionCallError::RespondToModel(format!(
            "target account `work` of `myopenai` (this agent runs on account `openai:work`) cannot consume this OpenAI-encrypted collaboration payload. Call the function named `{PLAINTEXT_FOLLOWUP_TASK_TOOL}` directly with exactly the same `target` and the plaintext `message`; do not call `followup_task` again and do not add an `adapter` field. No message was admitted and no target turn was started."
        )))
    );
    assert_eq!(
        check(ConfiguredRuntime::from_snapshot(
            "myopenai",
            Some("openai:work")
        )),
        Ok(())
    );
}
