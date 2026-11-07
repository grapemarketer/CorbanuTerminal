//! Issue863 call-path tests using the existing capturing ThreadManager; no inference.
use super::*;
use pretty_assertions::assert_eq;

async fn custom_source_context() -> (crate::session::session::Session, TurnContext) {
    let (session, turn) = make_session_and_context().await;
    let mut turn = turn
        .with_model("gpt-5.6-sol".to_string(), &session.services.models_manager)
        .await;
    let mut config = (*turn.config).clone();
    let mut provider = config.model_provider.clone();
    provider.name = "My OpenAI".to_string();
    provider.base_url = Some("https://api.openai.com/v1".to_string());
    provider.wire_api = codex_model_provider_info::WireApi::Responses;
    provider.requires_openai_auth = false;
    provider.env_key = Some("SYNTHETIC_ISSUE863_API_KEY".to_string());
    config.model_provider_id = "myopenai".to_string();
    config.model_provider = provider.clone();
    config
        .model_providers
        .insert("myopenai".to_string(), provider.clone());
    config
        .features
        .enable(Feature::MultiAgentV2)
        .expect("feature");
    turn.provider = create_model_provider(provider, turn.auth_manager.clone());
    set_turn_config(&mut turn, config);
    (session, turn)
}

#[tokio::test]
async fn issue863_custom_native_spawn_send_followup_keep_source_and_ciphertext() {
    let (mut session, turn) = custom_source_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let spawned = SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({"message":"opaque-spawn", "task_name":"issue863_child"})),
        ))
        .await
        .expect("native same-source spawn must be admitted");
    let (text, _) = expect_text_output(spawned);
    let result: serde_json::Value = serde_json::from_str(&text).expect("spawn JSON");
    assert_eq!(result["model_provider"], json!("myopenai"));
    assert_eq!(result["task_name"], json!("/root/issue863_child"));
    let child = session
        .services
        .agent_control
        .resolve_agent_reference(session.thread_id, &turn.session_source, "issue863_child")
        .await
        .expect("child reference");
    let child_config = manager
        .get_thread(child)
        .await
        .expect("child")
        .config_snapshot()
        .await;
    assert_eq!(child_config.model_provider_id, "myopenai");

    SendMessageHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "send_message",
            function_payload(json!({"target":"issue863_child", "message":"opaque-send"})),
        ))
        .await
        .expect("same-source send");
    FollowupTaskHandlerV2
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "followup_task",
            function_payload(json!({"target":"issue863_child", "message":"opaque-followup"})),
        ))
        .await
        .expect("same-source followup");
    let messages: Vec<_> = manager
        .captured_ops()
        .iter()
        .filter_map(|(id, op)| {
            if *id != child {
                return None;
            }
            match op {
                Op::InterAgentCommunication { communication } => Some(json!({
                    "content": communication.content,
                    "encrypted_content": communication.encrypted_content,
                    "trigger_turn": communication.trigger_turn,
                    "recipient": communication.recipient.as_str(),
                })),
                _ => None,
            }
        })
        .collect();
    assert_eq!(
        messages,
        vec![
            json!({"content":"", "encrypted_content":"opaque-spawn", "trigger_turn":true, "recipient":"/root/issue863_child"}),
            json!({"content":"", "encrypted_content":"opaque-send", "trigger_turn":false, "recipient":"/root/issue863_child"}),
            json!({"content":"", "encrypted_content":"opaque-followup", "trigger_turn":true, "recipient":"/root/issue863_child"}),
        ]
    );
}

#[tokio::test]
async fn issue863_custom_native_send_followup_reject_builtin_before_mailbox_mutation() {
    let (mut session, turn) = custom_source_context().await;
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let builtin_path = AgentPath::try_from("/root/builtin").expect("path");
    let mut config = (*turn.config).clone();
    config.model_provider_id = OPENAI_PROVIDER_ID.to_string();
    config.model_provider = config.model_providers[OPENAI_PROVIDER_ID].clone();
    let builtin = session
        .services
        .agent_control
        .spawn_agent_with_metadata(
            config,
            Vec::new(),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: root.thread_id,
                depth: 1,
                agent_path: Some(builtin_path.clone()),
                agent_nickname: Some("Synthetic".to_string()),
                agent_role: Some("orc".to_string()),
                agent_class: None,
            })),
            crate::agent::control::SpawnAgentOptions::default(),
        )
        .await
        .expect("register built-in child without inference");
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    for tool in ["send_message", "followup_task"] {
        let call = invocation(
            session.clone(),
            turn.clone(),
            tool,
            function_payload(json!({"target":builtin_path, "message":"opaque-cross-source"})),
        );
        let result = if tool == "send_message" {
            SendMessageHandlerV2.handle(call).await
        } else {
            FollowupTaskHandlerV2.handle(call).await
        };
        let error = result
            .err()
            .expect("cross-source native payload must be rejected");
        assert_eq!(
            error,
            FunctionCallError::RespondToModel(format!(
                "target provider `openai` cannot consume this OpenAI-encrypted collaboration payload. Call the function named `{tool}_plaintext` directly with exactly the same `target` and the plaintext `message`; do not call `{tool}` again and do not add an `adapter` field. No message was admitted and no target turn was started."
            ))
        );
    }
    let captured: Vec<_> = manager
        .captured_ops()
        .iter()
        .filter_map(|(id, op)| {
            if *id == builtin.thread_id
                && let Op::InterAgentCommunication { communication } = op
            {
                return Some(communication.clone());
            }
            None
        })
        .collect();
    assert_eq!(captured, Vec::new());
}

/// Spawns natively from the custom OpenAI-endpoint parent with `arguments`
/// (with the `fork-context-role` role installed when `role`, and a `work`
/// account of `myopenai` when `named_accounts`) and returns the result and
/// how many children the spawn left behind.
async fn custom_source_native_spawn(
    named_accounts: bool,
    role: bool,
    arguments: serde_json::Value,
) -> (
    Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError>,
    usize,
) {
    custom_source_native_spawn_with(named_accounts, role, arguments, |_| {}).await
}

/// [`custom_source_native_spawn`], with `configure` applied to the parent's
/// turn before the spawn.
async fn custom_source_native_spawn_with(
    named_accounts: bool,
    role: bool,
    arguments: serde_json::Value,
    configure: impl FnOnce(&mut TurnContext),
) -> (
    Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError>,
    usize,
) {
    let (mut session, mut turn) = custom_source_context().await;
    if role {
        install_role_with_model_override(&mut turn).await;
    }
    if named_accounts {
        let mut config = (*turn.config).clone();
        config
            .features
            .enable(Feature::NamedAccounts)
            .expect("feature");
        codex_vault::Vault::new(config.codex_home.to_path_buf())
            .write_provider_account(
                "myopenai",
                &codex_vault::ProviderAccountName::parse("work").expect("name"),
                codex_vault::ProviderAccountKind::ApiKey,
                "canary-1016-work",
            )
            .expect("write account");
        set_turn_config(&mut turn, config);
    }
    configure(&mut turn);
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root");
    // The root's own control plane carries its security policy binding.
    session.services.agent_control = root.thread.session.services.agent_control.clone();
    session.thread_id = root.thread_id;
    let result = SpawnAgentHandlerV2::default()
        .handle(invocation(
            Arc::new(session),
            Arc::new(turn),
            "spawn_agent",
            function_payload(arguments),
        ))
        .await;
    let children = manager
        .list_thread_ids()
        .await
        .into_iter()
        .filter(|id| *id != root.thread_id)
        .count();
    (result, children)
}

/// #1016: a role that moves the child of a custom OpenAI-endpoint parent to
/// the built-in `openai` provider refuses the native spawn before the child
/// starts and points the model to the plaintext adapter.
#[tokio::test]
async fn issue1016_custom_native_spawn_moved_to_builtin_by_role_is_refused() {
    let (result, children) = custom_source_native_spawn(
        /*named_accounts*/ false,
        /*role*/ true,
        json!({"message": "opaque-spawn", "task_name": "issue1016_child", "fork_turns": "none", "agent_type": "fork-context-role"}),
    )
    .await;
    assert_eq!(
        (result.err(), children),
        (
            Some(FunctionCallError::RespondToModel(
                "target provider `openai` cannot consume this OpenAI-encrypted collaboration payload. Call the function named `spawn_agent_plaintext` directly with exactly the same `target` and the plaintext `message`; do not call `spawn_agent` again and do not add an `adapter` field. No message was admitted and no target turn was started.".to_string()
            )),
            0
        )
    );
}

/// #1016: a native spawn onto another account of the parent's own provider
/// is refused, as the encrypted payload is bound to the account; the
/// parent's own account is still admitted.
#[tokio::test]
async fn issue1016_custom_native_spawn_on_another_account_is_refused() {
    let (result, children) = custom_source_native_spawn(
        /*named_accounts*/ true,
        /*role*/ false,
        json!({"message": "opaque-spawn", "task_name": "issue1016_child", "fork_turns": "none", "account": "work"}),
    )
    .await;
    assert_eq!(
        (result.err(), children),
        (
            Some(FunctionCallError::RespondToModel(
                "target account `work` of `myopenai` (this agent runs on account `default`) cannot consume this OpenAI-encrypted collaboration payload. Call the function named `spawn_agent_plaintext` directly with exactly the same `target` and the plaintext `message`; do not call `spawn_agent` again and do not add an `adapter` field. No message was admitted and no target turn was started.".to_string()
            )),
            0
        )
    );
    for account in [json!({}), json!({"account": "default"})] {
        let mut arguments = json!({"message": "opaque-spawn", "task_name": "issue1016_child", "fork_turns": "none"});
        for (key, value) in account.as_object().expect("object") {
            arguments[key] = value.clone();
        }
        let (result, children) = custom_source_native_spawn(
            /*named_accounts*/ true, /*role*/ false, arguments,
        )
        .await;
        assert_eq!((result.is_ok(), children), (true, 1), "{account}");
    }
}

/// #1144: under Aggressive, a native spawn onto another account is refused
/// for the encrypted payload before any approval is asked, so the user is
/// never asked for a spawn that would be refused anyway.
#[tokio::test]
async fn issue1144_aggressive_native_spawn_on_another_account_is_refused_before_asking() {
    for approval in [AskForApproval::Never, AskForApproval::OnRequest] {
        let spawn = custom_source_native_spawn_with(
            /*named_accounts*/ true,
            /*role*/ false,
            json!({"message": "opaque-spawn", "task_name": "issue1144_child", "fork_turns": "none", "account": "work"}),
            |turn| {
                let mut config = (*turn.config).clone();
                config.security_level = codex_security_policy::SecurityLevel::Aggressive;
                set_turn_config(turn, config);
                turn.approval_policy
                    .set(approval)
                    .expect("approval policy should be set");
            },
        );
        let (result, children) = tokio::time::timeout(std::time::Duration::from_secs(20), spawn)
            .await
            .unwrap_or_else(|_| panic!("{approval:?}: the spawn waited for an answer"));
        assert_eq!(
            (result.err(), children),
            (
                Some(FunctionCallError::RespondToModel(
                    "target account `work` of `myopenai` (this agent runs on account `default`) cannot consume this OpenAI-encrypted collaboration payload. Call the function named `spawn_agent_plaintext` directly with exactly the same `target` and the plaintext `message`; do not call `spawn_agent` again and do not add an `adapter` field. No message was admitted and no target turn was started.".to_string()
                )),
                0
            ),
            "{approval:?}"
        );
    }
}

/// #1127 follow-up: native `send_message` and `followup_task` to a child on
/// another account of the parent's own provider are refused end to end,
/// before anything reaches the child's mailbox, and the model is pointed to
/// the plaintext tools.
#[tokio::test]
async fn issue1127_custom_native_send_followup_to_another_account_are_refused() {
    let (mut session, mut turn) = custom_source_context().await;
    let mut config = (*turn.config).clone();
    config
        .features
        .enable(Feature::NamedAccounts)
        .expect("feature");
    codex_vault::Vault::new(config.codex_home.to_path_buf())
        .write_provider_account(
            "myopenai",
            &codex_vault::ProviderAccountName::parse("work").expect("name"),
            codex_vault::ProviderAccountKind::ApiKey,
            "canary-1127-work",
        )
        .expect("write account");
    set_turn_config(&mut turn, config);
    let manager = thread_manager();
    let root = manager
        .start_thread((*turn.config).clone())
        .await
        .expect("root");
    session.services.agent_control = manager.agent_control();
    session.thread_id = root.thread_id;
    let work_path = AgentPath::try_from("/root/work_child").expect("path");
    let mut child_config = (*turn.config).clone();
    crate::config::stamp_provider_account("myopenai", &mut child_config.model_provider, "work")
        .expect("stamp");
    let work_child = session
        .services
        .agent_control
        .spawn_agent_with_metadata(
            child_config,
            Vec::new(),
            Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: root.thread_id,
                depth: 1,
                agent_path: Some(work_path.clone()),
                agent_nickname: Some("Synthetic".to_string()),
                agent_role: Some("orc".to_string()),
                agent_class: None,
            })),
            crate::agent::control::SpawnAgentOptions::default(),
        )
        .await
        .expect("register a child on account `work` without inference");
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    for tool in ["send_message", "followup_task"] {
        let call = invocation(
            session.clone(),
            turn.clone(),
            tool,
            function_payload(json!({"target":work_path, "message":"opaque-cross-account"})),
        );
        let result = if tool == "send_message" {
            SendMessageHandlerV2.handle(call).await
        } else {
            FollowupTaskHandlerV2.handle(call).await
        };
        assert_eq!(
            result.err(),
            Some(FunctionCallError::RespondToModel(format!(
                "target account `work` of `myopenai` (this agent runs on account `default`) cannot consume this OpenAI-encrypted collaboration payload. Call the function named `{tool}_plaintext` directly with exactly the same `target` and the plaintext `message`; do not call `{tool}` again and do not add an `adapter` field. No message was admitted and no target turn was started."
            )))
        );
    }
    let captured: Vec<_> = manager
        .captured_ops()
        .iter()
        .filter_map(|(id, op)| {
            if *id == work_child.thread_id
                && let Op::InterAgentCommunication { communication } = op
            {
                return Some(communication.clone());
            }
            None
        })
        .collect();
    assert_eq!(captured, Vec::new());
}

#[tokio::test]
async fn issue1016_custom_native_spawn_default_model_keeps_source_and_ciphertext() {
    let (mut session, mut turn) = custom_source_context().await;
    let mut config = (*turn.config).clone();
    // Use a different model so the route exercises model selection, not inheritance.
    config.agent_default_subagent_model = Some("gpt-5.6-terra".to_string());
    set_turn_config(&mut turn, config);
    let manager = install_live_root(&mut session, &turn).await;
    let threads_before: std::collections::HashSet<_> =
        manager.list_thread_ids().await.into_iter().collect();
    let operations_before = manager.captured_ops();
    let session = Arc::new(session);
    let turn = Arc::new(turn);

    let spawned = SpawnAgentHandlerV2::default()
        .handle(invocation(
            session.clone(),
            turn.clone(),
            "spawn_agent",
            function_payload(json!({
                "message": "opaque-default-model",
                "task_name": "issue1016_default_model",
                "fork_turns": "none",
            })),
        ))
        .await
        .expect("a default model must preserve the custom source provider");
    let (text, _) = expect_text_output(spawned);
    let result: serde_json::Value = serde_json::from_str(&text).expect("spawn JSON");
    assert_eq!(
        json!({
            "model_provider": result["model_provider"],
            "model": result["model"],
            "task_name": result["task_name"],
        }),
        json!({
            "model_provider": "myopenai",
            "model": "gpt-5.6-terra",
            "task_name": "/root/issue1016_default_model",
        })
    );
    let child = session
        .services
        .agent_control
        .resolve_agent_reference(
            session.thread_id,
            &turn.session_source,
            "issue1016_default_model",
        )
        .await
        .expect("child reference");
    let child_config = manager
        .get_thread(child)
        .await
        .expect("child")
        .config_snapshot()
        .await;
    assert_eq!(
        (child_config.model_provider_id, child_config.model),
        ("myopenai".to_string(), "gpt-5.6-terra".to_string())
    );
    let threads_after: std::collections::HashSet<_> =
        manager.list_thread_ids().await.into_iter().collect();
    let mut expected_threads = threads_before;
    assert!(
        expected_threads.insert(child),
        "spawn must create a new child"
    );
    assert_eq!(threads_after, expected_threads);
    let operations_after = manager.captured_ops();
    let new_operations = operations_after
        .strip_prefix(operations_before.as_slice())
        .expect("spawn must preserve all prior captured operations");
    let [(actual_child, Op::InterAgentCommunication { communication })] = new_operations else {
        panic!("spawn must add exactly one captured communication operation");
    };
    assert_eq!(*actual_child, child);
    assert_eq!(
        json!({
            "content": communication.content,
            "encrypted_content": communication.encrypted_content,
            "trigger_turn": communication.trigger_turn,
            "recipient": communication.recipient.as_str(),
        }),
        json!({
            "content": "",
            "encrypted_content": "opaque-default-model",
            "trigger_turn": true,
            "recipient": "/root/issue1016_default_model",
        })
    );
}
