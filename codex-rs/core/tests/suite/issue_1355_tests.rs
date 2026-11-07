//! #1355 item 1: queued agent mail explains a named-account refusal without
//! starting a refused turn or losing the mail. Run through `just test` so the
//! synthetic named sign-in uses the isolated test vault, never a native keyring.

use codex_core::StartThreadOptions;
use codex_features::Feature;
use codex_login::NamedChatgptAccount;
use codex_login::login_named_account_with_api_key;
use codex_model_provider_info::NamedProviderAccount;
use codex_protocol::AgentPath;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn held_child_mail_warns_the_parent_and_survives_account_recovery() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));
    const KEY: &str = "synthetic-1355-work-key";
    const MAIL: &str = "held-1355-worker-message";
    let account = NamedChatgptAccount {
        provider_id: "openai".to_string(),
        name: "work".to_string(),
    };
    let server = start_mock_server().await;
    let captured = mount_sse_sequence(
        &server,
        ["before-removal", "after-recovery"]
            .into_iter()
            .map(|id| {
                sse(vec![
                    ev_response_created(id),
                    ev_assistant_message(id, "noted"),
                    ev_completed(id),
                ])
            })
            .collect(),
    )
    .await;
    let sign_in = account.clone();
    let test = test_codex()
        .with_model("test-gpt-5.1-codex")
        .with_auth_in_home()
        .with_config(move |config| {
            login_named_account_with_api_key(&config.codex_home, &sign_in, KEY)
                .expect("synthetic named sign-in");
            config
                .features
                .enable(Feature::NamedAccounts)
                .expect("enable named accounts");
        })
        .build_with_auto_env(&server)
        .await?;
    let child_path = AgentPath::try_from("/root/worker").map_err(anyhow::Error::msg)?;
    let mut child_config = test.config.clone();
    child_config.model_provider.account = Some(NamedProviderAccount {
        provider_id: account.provider_id.clone(),
        name: account.name.clone(),
    });
    let child = test
        .thread_manager
        .start_thread(StartThreadOptions {
            session_source: Some(SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                parent_thread_id: test.session_configured.thread_id,
                depth: 1,
                agent_path: Some(child_path.clone()),
                agent_nickname: None,
                agent_role: None,
                agent_class: None,
            })),
            ..StartThreadOptions::new(child_config)
        })
        .await?;

    // Prove the configured account can run before removing it. The fixture's
    // separate default sign-in remains available throughout the test.
    child
        .thread
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "Before removal".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;
    wait_for_event(&child.thread, |event| {
        assert!(!matches!(event, EventMsg::Error(_)), "{event:?}");
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(
        captured.single_request().header("authorization"),
        Some(format!("Bearer {KEY}"))
    );
    assert!(
        codex_vault::Vault::new(test.codex_home_path().to_path_buf())
            .remove_provider_account("openai", &codex_vault::ProviderAccountName::parse("work")?,)?
    );

    child
        .thread
        .submit(Op::InterAgentCommunication {
            communication: InterAgentCommunication::new(
                AgentPath::root(),
                child_path,
                Vec::new(),
                MAIL.to_string(),
                /*trigger_turn*/ true,
            ),
        })
        .await?;
    // Consume only the parent's stream: the person does not have to open the
    // child's conversation to see why their requested work is still waiting.
    let warning = wait_for_event(&test.codex, |event| {
        assert!(
            !matches!(
                event,
                EventMsg::TurnStarted(_) | EventMsg::TurnComplete(_) | EventMsg::Error(_)
            ),
            "the warning must not create a parent turn: {event:?}"
        );
        matches!(event, EventMsg::Warning(warning) if warning.message.starts_with("Agent /root/worker: Agent messages remain queued:"))
    })
    .await;
    let EventMsg::Warning(warning) = warning else {
        unreachable!("matched warning")
    };
    assert!(warning.message.contains("account `work` has no sign-in"));
    assert!(warning.message.contains("corbanu login --account work"));
    assert!(
        warning
            .message
            .contains("send another message or start a turn to retry")
    );
    assert_eq!(captured.requests().len(), 1, "no fallback model request");

    // Re-enrol the same account and explicitly start a turn. Merely emitting
    // the warning must not have consumed the original queued mail.
    login_named_account_with_api_key(test.codex_home_path(), &account, KEY)?;
    child
        .thread
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "Continue after signing in".to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;
    wait_for_event(&child.thread, |event| {
        assert!(!matches!(event, EventMsg::Error(_)), "{event:?}");
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let requests = captured.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].header("authorization"),
        Some(format!("Bearer {KEY}"))
    );
    assert!(requests[1].body_contains_text(MAIL));
    assert!(requests[1].body_contains_text("Continue after signing in"));
    Ok(())
}
