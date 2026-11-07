use super::AGENT_FINAL_MESSAGE_PREFIX;
use super::HANDOFF_STREAM_TRUNCATION_MARKER;
use super::RealtimeHandoffState;
use super::RealtimeSessionKind;
use super::RealtimeStreamedItem;
use super::realtime_api_key;
use super::realtime_delegation_from_handoff;
use super::realtime_request_headers;
use super::realtime_text_from_handoff_request;
use super::wrap_realtime_delegation_input;
use crate::context::RealtimeDelegationSource;
use async_channel::bounded;
use codex_api::RealtimeEventParser;
use codex_protocol::models::MessagePhase;
use codex_protocol::protocol::CodexResponseHandoffMode;
use codex_protocol::protocol::RealtimeHandoffRequested;
use codex_protocol::protocol::RealtimeTranscriptEntry;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

#[path = "realtime_webrtc_accounting_tests.rs"]
mod webrtc_accounting;

fn pf_30_realtime_start(
    model_client: crate::client::ModelClient,
    base_url: String,
    sdp: Option<String>,
) -> super::RealtimeStart {
    let mut api_provider = model_client
        .provider_info()
        .to_api_provider(/*auth_mode*/ None)
        .unwrap();
    api_provider.base_url = base_url;
    super::RealtimeStart {
        accounting: None,
        websocket_credential: super::RealtimeRecordKey::None,
        api_provider,
        realtime_sideband_base_url: None,
        extra_headers: None,
        client_managed_handoffs: false,
        flush_transcript_tail_on_session_end: true,
        codex_responses_as_items: false,
        codex_response_item_prefix: None,
        codex_response_handoff_mode: CodexResponseHandoffMode::default(),
        codex_response_handoff_channel_prefixes: None,
        realtime_call_api_provider: None,
        session_config: codex_api::RealtimeSessionConfig {
            instructions: "unadmitted-initial-instructions".into(),
            initial_items: Vec::new(),
            model: None,
            session_id: None,
            event_parser: RealtimeEventParser::V1,
            session_mode: codex_api::RealtimeSessionMode::Conversational,
            output_modality: codex_protocol::protocol::RealtimeOutputModality::Audio,
            voice: codex_protocol::protocol::RealtimeVoice::Alloy,
        },
        model_client,
        sdp,
    }
}

#[tokio::test]
async fn pf_30_s01_realtime_transports_deny_protected_start_before_network() {
    use codex_security_policy::SecurityLevel;
    let (session, _) = crate::session::tests::make_session_and_context().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    for level in [SecurityLevel::Moderate, SecurityLevel::Aggressive] {
        for sdp in [None, Some("unadmitted-sdp".into())] {
            let model_client = session
                .services
                .model_client()
                .as_ref()
                .clone()
                .with_ingress_level(level);
            let manager = super::RealtimeConversationManager::new();
            let result = manager
                .start(pf_30_realtime_start(model_client, base_url.clone(), sdp))
                .await;
            let error = result.err().expect("protected realtime must fail closed");
            assert!(error.to_string().contains("source admission"), "{error}");
            assert!(manager.running_state().await.is_none());
        }
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn pf_30_s01_realtime_live_strengthening_stops_next_frame() {
    use crate::security::EffectivePolicyInitialization;
    use crate::security::EffectivePolicyView;
    use crate::security::PersistedHumanSecurityState;
    use crate::security::TrustedSecurityController;
    use codex_protocol::protocol::ConversationTextParams;
    use codex_protocol::protocol::ConversationTextRole;
    use codex_security_policy::PolicyPrincipal;
    use codex_security_policy::PrincipalKind;
    use codex_security_policy::RevocationState;
    use codex_security_policy::SecurityLevel;
    use codex_security_policy::SecuritySettings;
    use futures::StreamExt;
    use std::time::Duration;

    let (session, _) = crate::session::tests::make_session_and_context().await;
    let view = EffectivePolicyView::default();
    let controller = TrustedSecurityController::initialize(
        &view,
        PersistedHumanSecurityState::new(
            SecuritySettings::new(SecurityLevel::Permissive),
            PolicyPrincipal::new(PrincipalKind::Human, "fixture-human").unwrap(),
            RevocationState::new(),
        )
        .unwrap(),
        session.thread_id,
        codex_protocol::SessionId::from(session.thread_id),
        EffectivePolicyInitialization::Root,
    )
    .unwrap();
    let model_client = session
        .services
        .model_client()
        .as_ref()
        .clone()
        .with_ingress_policy(SecurityLevel::Permissive, view);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (frames_tx, frames_rx) = async_channel::unbounded();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut websocket = tokio_tungstenite::accept_async(socket).await.unwrap();
        while let Some(Ok(message)) = websocket.next().await {
            if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
                frames_tx.send(text.to_string()).await.unwrap();
            }
        }
    });
    let manager = super::RealtimeConversationManager::new();
    let output = manager
        .start(pf_30_realtime_start(
            model_client,
            base_url,
            /*sdp*/ None,
        ))
        .await
        .unwrap();
    let initial = tokio::time::timeout(Duration::from_secs(5), frames_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(initial.contains("unadmitted-initial-instructions"));
    manager
        .text_in(ConversationTextParams {
            text: "permissive-text".into(),
            role: ConversationTextRole::User,
        })
        .await
        .unwrap();
    let text = tokio::time::timeout(Duration::from_secs(5), frames_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(text.contains("permissive-text"));
    controller
        .apply_confirmed_change(
            controller
                .confirm_level_change(SecurityLevel::Moderate, RevocationState::new())
                .unwrap(),
        )
        .unwrap();
    manager
        .text_in(ConversationTextParams {
            text: "protected-canary".into(),
            role: ConversationTextRole::User,
        })
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(5), output.events_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(event, codex_api::RealtimeEvent::Error(error) if error.contains("source admission"))
    );
    // Await task completion before checking the recording, so absence is not a
    // race with the sender. No transcript tail may re-enter Core after denial.
    manager.shutdown().await.unwrap();
    while let Ok(frame) = frames_rx.try_recv() {
        assert!(!frame.contains("protected-canary"));
    }
    assert!(output.transcript_tail_rx.try_recv().is_err());
    server.abort();
}

#[test]
fn prefers_handoff_input_transcript_over_active_transcript() {
    let handoff = RealtimeHandoffRequested {
        handoff_id: "handoff_1".to_string(),
        item_id: "item_1".to_string(),
        input_transcript: "ignored".to_string(),
        active_transcript: vec![
            RealtimeTranscriptEntry {
                role: "user".to_string(),
                text: "hello".to_string(),
            },
            RealtimeTranscriptEntry {
                role: "assistant".to_string(),
                text: "hi there".to_string(),
            },
        ],
    };
    assert_eq!(
        realtime_text_from_handoff_request(&handoff),
        Some("ignored".to_string())
    );
}

#[test]
fn extracts_text_from_handoff_request_active_transcript_if_input_missing() {
    let handoff = RealtimeHandoffRequested {
        handoff_id: "handoff_1".to_string(),
        item_id: "item_1".to_string(),
        input_transcript: String::new(),
        active_transcript: vec![RealtimeTranscriptEntry {
            role: "user".to_string(),
            text: "hello".to_string(),
        }],
    };
    assert_eq!(
        realtime_text_from_handoff_request(&handoff),
        Some("user: hello".to_string())
    );
}

#[test]
fn wraps_handoff_with_transcript_delta() {
    let handoff = RealtimeHandoffRequested {
        handoff_id: "handoff_1".to_string(),
        item_id: "item_1".to_string(),
        input_transcript: "delegate this".to_string(),
        active_transcript: vec![
            RealtimeTranscriptEntry {
                role: "user".to_string(),
                text: "hello".to_string(),
            },
            RealtimeTranscriptEntry {
                role: "assistant".to_string(),
                text: "hi there".to_string(),
            },
        ],
    };
    assert_eq!(
        realtime_delegation_from_handoff(&handoff),
        Some(
            "<realtime_delegation>\n  <input>delegate this</input>\n  <transcript_delta>user: hello\nassistant: hi there</transcript_delta>\n</realtime_delegation>"
                .to_string()
        )
    );
}

#[test]
fn extracts_text_from_handoff_request_input_transcript_if_messages_missing() {
    let handoff = RealtimeHandoffRequested {
        handoff_id: "handoff_1".to_string(),
        item_id: "item_1".to_string(),
        input_transcript: "ignored".to_string(),
        active_transcript: vec![],
    };
    assert_eq!(
        realtime_text_from_handoff_request(&handoff),
        Some("ignored".to_string())
    );
}

#[test]
fn ignores_empty_handoff_request_input_transcript() {
    let handoff = RealtimeHandoffRequested {
        handoff_id: "handoff_1".to_string(),
        item_id: "item_1".to_string(),
        input_transcript: String::new(),
        active_transcript: vec![],
    };
    assert_eq!(realtime_text_from_handoff_request(&handoff), None);
}

#[test]
fn wraps_realtime_delegation_input() {
    assert_eq!(
        wrap_realtime_delegation_input(
            "hello",
            /*transcript_delta*/ None,
            RealtimeDelegationSource::Handoff,
        ),
        "<realtime_delegation>\n  <input>hello</input>\n</realtime_delegation>"
    );
}

#[test]
fn wraps_realtime_delegation_input_with_xml_escaping() {
    assert_eq!(
        wrap_realtime_delegation_input(
            "use a < b && c > d",
            Some("saw <that>"),
            RealtimeDelegationSource::Handoff,
        ),
        "<realtime_delegation>\n  <input>use a &lt; b &amp;&amp; c &gt; d</input>\n  <transcript_delta>saw &lt;that&gt;</transcript_delta>\n</realtime_delegation>"
    );
}

#[test]
fn wraps_realtime_delegation_input_with_xml_escaping_without_transcript() {
    assert_eq!(
        wrap_realtime_delegation_input(
            "use a < b && c > d",
            /*transcript_delta*/ None,
            RealtimeDelegationSource::Handoff,
        ),
        "<realtime_delegation>\n  <input>use a &lt; b &amp;&amp; c &gt; d</input>\n</realtime_delegation>"
    );
}

#[tokio::test]
async fn clears_active_handoff_explicitly() {
    let (tx, _rx) = bounded(1);
    let state = RealtimeHandoffState {
        output_tx: tx,
        last_output: Arc::new(Mutex::new(None)),
        stream: Arc::new(Mutex::new(Default::default())),
        client_managed_handoffs: false,
        codex_responses_as_items: false,
        codex_response_item_prefix: None,
        codex_response_handoff_mode: CodexResponseHandoffMode::Thinking,
        codex_response_handoff_channel_prefixes: Arc::new(BTreeMap::new()),
        session_kind: RealtimeSessionKind::V1,
        event_parser: RealtimeEventParser::V1,
    };

    state.stream.lock().await.active_handoff = Some("handoff_1".to_string());
    assert_eq!(
        state.stream.lock().await.active_handoff.clone(),
        Some("handoff_1".to_string())
    );

    state.stream.lock().await.active_handoff = None;
    assert_eq!(state.stream.lock().await.active_handoff.clone(), None);
}

#[test]
fn streamed_handoff_preserves_a_bounded_final_tail() {
    let mut item = RealtimeStreamedItem {
        handoff_id: "handoff_1".to_string(),
        phase: Some(MessagePhase::FinalAnswer),
        bem_channel_parser: None,
        prefix_final_message: true,
        sent_bytes: 0,
        buffered_text: String::new(),
        tail_text: String::new(),
        truncated: false,
        last_flush_at: Instant::now(),
        flush_scheduled: false,
    };
    item.push_text(&format!("HEAD{}TAIL", "x".repeat(/*n*/ 5_000)));

    let first = item
        .drain_stream_chunk()
        .expect("oversized output should retain a streamable head");
    let final_chunk = item
        .drain_final_chunk()
        .expect("oversized output should retain a final tail");
    let output = format!("{first}{final_chunk}");

    assert!(output.len() <= 4_000);
    assert!(output.starts_with(&format!("{AGENT_FINAL_MESSAGE_PREFIX}HEAD")));
    assert!(output.contains(HANDOFF_STREAM_TRUNCATION_MARKER));
    assert!(output.ends_with("TAIL"));
}

#[test]
fn streamed_v3_handoff_omits_the_final_message_prefix() {
    let mut item = RealtimeStreamedItem {
        handoff_id: "handoff_1".to_string(),
        phase: Some(MessagePhase::FinalAnswer),
        bem_channel_parser: None,
        prefix_final_message: false,
        sent_bytes: 0,
        buffered_text: String::new(),
        tail_text: String::new(),
        truncated: false,
        last_flush_at: Instant::now(),
        flush_scheduled: false,
    };
    item.push_text("done");

    assert_eq!(item.drain_final_chunk(), Some("done".to_string()));
}

#[test]
fn uses_quicksilver_alpha_header_for_realtime_v1() {
    let headers = realtime_request_headers(
        Some("session_1"),
        Some("sk-test"),
        RealtimeEventParser::V1,
        "codex_work_desktop",
    )
    .expect("headers")
    .expect("headers");

    assert_eq!(
        headers
            .get("openai-alpha")
            .and_then(|value| value.to_str().ok()),
        Some("quicksilver=v1")
    );
}

#[test]
fn omits_quicksilver_alpha_header_for_realtime_v2() {
    let headers = realtime_request_headers(
        Some("session_1"),
        Some("sk-test"),
        RealtimeEventParser::RealtimeV2,
        "codex_work_desktop",
    )
    .expect("headers")
    .expect("headers");

    assert!(headers.get("openai-alpha").is_none());
}

#[test]
fn uses_frameless_alpha_header_for_realtime_v3() {
    let headers = realtime_request_headers(
        Some("session_1"),
        Some("sk-test"),
        RealtimeEventParser::FramelessBidi,
        "codex_work_desktop",
    )
    .expect("headers")
    .expect("headers");

    assert_eq!(
        headers
            .get("openai-alpha")
            .and_then(|value| value.to_str().ok()),
        Some("quicksilver=v2")
    );
}

#[test]
fn realtime_headers_include_only_non_default_originator() {
    let default_originator = codex_login::default_client::originator();
    for (originator, expected_header) in [
        ("codex_work_desktop", Some("codex_work_desktop")),
        (default_originator.value.as_str(), None),
    ] {
        let headers = realtime_request_headers(
            Some("session_1"),
            Some("sk-test"),
            RealtimeEventParser::RealtimeV2,
            originator,
        )
        .expect("headers")
        .expect("headers");

        assert_eq!(
            headers
                .get("originator")
                .and_then(|value| value.to_str().ok()),
            expected_header
        );
    }
}

/// A realtime call is a model request the operator paid for, and it recorded
/// nothing. The realtime protocol this client parses carries no usage anywhere,
/// so the call is recorded with its tokens unknown - which is what the provider
/// said - rather than left invisible.
#[tokio::test]
async fn pf_60_s03_realtime_call_is_recorded() -> anyhow::Result<()> {
    use codex_state::SqliteConfig;
    use codex_state::StateRuntime;
    use codex_state::ThreadMetadataBuilder;
    use codex_utils_absolute_path::AbsolutePathBuf;

    let server = wiremock::MockServer::start().await;
    let endpoint = format!("{}/v1", server.uri());
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/v1/realtime/calls"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .insert_header("Location", "/v1/live/rtc_fixture")
                .set_body_string("v=answer\r\n"),
        )
        .mount(&server)
        .await;

    let home = tempfile::tempdir()?;
    let test_cwd = tempfile::tempdir()?;
    let mut config = crate::session::tests::build_test_config(home.path(), test_cwd.path()).await;
    config.model_provider = codex_model_provider_info::ModelProviderInfo {
        request_max_retries: Some(0),
        stream_max_retries: Some(0),
        supports_websockets: false,
        ..codex_model_provider_info::ModelProviderInfo::create_openai_provider(Some(
            endpoint.clone(),
        ))
    };
    config.model_provider_id = "openai".into();
    config.accounting = crate::config::AccountingMode::Provider {
        scope: uuid::Uuid::new_v4(),
        provider_id: "openai".into(),
        wire_api: codex_model_provider_info::WireApi::Responses,
        approved_endpoint: endpoint.clone(),
        approved_query: None,
        pricing: crate::config::PriceAuthority::Unavailable,
        basis_source: Default::default(),
        account: None,
    };
    config
        .features
        .enable(codex_features::Feature::Sqlite)
        .expect("sqlite for the ledger");

    let (mut session, _context) =
        crate::session::tests::make_session_and_context_for_config(config).await;
    let db = StateRuntime::init(
        SqliteConfig::from_sqlite_home(AbsolutePathBuf::try_from(home.path().to_path_buf())?),
        "openai".into(),
    )
    .await?;
    session.services.state_db = Some(db.clone());
    // Accounting attributes attempts to a persisted thread; ephemeral sessions
    // run uncollected, so the fixture needs a live thread like a real session.
    crate::session::tests::open_thread_persistence(&mut session).await;
    let owner = Arc::new(session);
    db.upsert_thread(
        &ThreadMetadataBuilder::new(
            owner.thread_id,
            home.path().join("realtime-fixture.jsonl"),
            chrono::Utc::now(),
            codex_protocol::protocol::SessionSource::Cli,
        )
        .build("openai"),
    )
    .await?;

    let accounting = super::realtime_accounting(
        &owner,
        &endpoint,
        /*auth_mode*/ None,
        crate::accounting::realtime_turn_label("fixture-sub"),
    )
    .await
    .expect("realtime accounting attaches");
    owner
        .services
        .model_client()
        .create_realtime_call_with_headers(
            "v=offer\r\n".to_string(),
            codex_api::RealtimeSessionConfig {
                instructions: String::new(),
                initial_items: Vec::new(),
                model: Some("gpt-realtime-1.5".into()),
                session_id: None,
                event_parser: RealtimeEventParser::V1,
                session_mode: codex_api::RealtimeSessionMode::Conversational,
                output_modality: codex_protocol::protocol::RealtimeOutputModality::Audio,
                voice: codex_protocol::protocol::RealtimeVoice::Alloy,
            },
            Default::default(),
            /*api_provider_override*/ None,
            &accounting.0.responses_accounting,
        )
        .await?;

    let pool = db
        .sqlite()
        .open_read_only_pool(&db.sqlite().state_db_path())
        .await?;
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT payload FROM draft_accounting_attempts ORDER BY rowid")
            .fetch_all(&pool)
            .await?;
    let turns: Vec<String> = rows
        .iter()
        .filter_map(|row| {
            serde_json::from_str::<serde_json::Value>(row).ok()?["turn"]
                .as_str()
                .map(str::to_string)
        })
        .collect();
    assert_eq!(
        turns
            .iter()
            .filter(|turn| turn.starts_with("realtime:"))
            .count(),
        1,
        "turns recorded: {turns:?}"
    );
    Ok(())
}

#[test]
fn realtime_sends_the_openai_api_key_env_fallback_only_to_openai() {
    let env_fallback =
        codex_login::CodexAuth::from_api_key_env("sk-env", codex_login::OPENAI_API_KEY_ENV_VAR);
    let oss = codex_model_provider_info::create_oss_provider_with_base_url(
        "http://localhost:11434/v1",
        codex_model_provider_info::WireApi::Responses,
    );
    let openai = codex_model_provider_info::ModelProviderInfo::create_openai_provider(
        /*base_url*/ None,
    );

    assert!(realtime_api_key(Some(&env_fallback), &oss).is_err());
    assert_eq!(
        realtime_api_key(Some(&env_fallback), &openai)
            .expect("openai key")
            .key,
        "sk-env"
    );
    let stored = codex_login::CodexAuth::from_api_key("sk-stored");
    assert_eq!(
        realtime_api_key(Some(&stored), &oss)
            .expect("stored key")
            .key,
        "sk-stored"
    );
}

fn named_openai_account() -> codex_model_provider_info::NamedProviderAccount {
    codex_model_provider_info::NamedProviderAccount {
        provider_id: "openai".into(),
        name: "work".into(),
    }
}

/// #889 item 1: the realtime key says whether it is the ambient
/// `OPENAI_API_KEY`; only that key under a selected named account is left
/// unrecorded, since the record would be grouped under that account.
#[test]
fn pf_889_ambient_realtime_key_under_a_named_account_is_not_attributed() {
    let openai = codex_model_provider_info::ModelProviderInfo::create_openai_provider(
        /*base_url*/ None,
    );
    let named = codex_model_provider_info::ModelProviderInfo {
        account: Some(named_openai_account()),
        ..openai.clone()
    };
    let env_fallback =
        codex_login::CodexAuth::from_api_key_env("sk-env", codex_login::OPENAI_API_KEY_ENV_VAR);
    let stored = codex_login::CodexAuth::from_api_key("sk-stored");
    let ambient = realtime_api_key(Some(&env_fallback), &named).expect("ambient key");
    assert!(ambient.ambient);
    let own = realtime_api_key(Some(&stored), &named).expect("stored key");
    assert!(!own.ambient);
    // A named ChatGPT sign-in has no API key: the key is the environment's.
    let chatgpt = codex_login::CodexAuth::create_dummy_chatgpt_auth_for_testing();
    let from_env =
        super::realtime_api_key_with_env(Some(&chatgpt), &named, || Some("sk-ambient".to_string()))
            .expect("environment key");
    assert_eq!(from_env.key, "sk-ambient");
    assert!(from_env.ambient);
    assert!(matches!(
        super::RealtimeRecordKey::for_websocket(&from_env, &named),
        super::RealtimeRecordKey::AmbientUnderNamedAccount
    ));

    assert!(matches!(
        super::RealtimeRecordKey::for_websocket(&ambient, &named),
        super::RealtimeRecordKey::AmbientUnderNamedAccount
    ));
    assert!(matches!(
        super::RealtimeRecordKey::for_websocket(&ambient, &openai),
        super::RealtimeRecordKey::Sent(_)
    ));
    assert!(matches!(
        super::RealtimeRecordKey::for_websocket(&own, &named),
        super::RealtimeRecordKey::Sent(_)
    ));
}

/// D171 (#1286, #1311 item 5): a realtime WebSocket session on a named
/// OpenAI account whose sign-in was removed is refused with the account's
/// recovery, as its other requests are. Before, it fell back to the ambient
/// `OPENAI_API_KEY` (or failed asking for an API key when none was set).
/// While the account is signed in, its own key is used as before.
#[tokio::test]
async fn pf_889_realtime_on_a_removed_named_account_is_refused_not_sent_the_ambient_key()
-> anyhow::Result<()> {
    let endpoint = realtime_stand_in(StandInEnd::ClientCloses).await?;
    let home = tempfile::tempdir()?;
    let test_cwd = tempfile::tempdir()?;
    let work = codex_login::NamedChatgptAccount {
        provider_id: "openai".into(),
        name: "work".into(),
    };
    codex_login::login_named_account_with_api_key(home.path(), &work, "sk-work-canary")?;
    let (owner, _db) = realtime_ledger_session_with(
        home.path(),
        test_cwd.path(),
        &endpoint,
        crate::config::AccountingMode::Disabled,
        |config| config.model_provider.account = Some(named_openai_account()),
    )
    .await?;

    let signed_in = super::prepare_realtime_start(&owner, realtime_websocket_params()).await?;
    assert!(matches!(
        signed_in.websocket_credential,
        super::RealtimeRecordKey::Sent(_)
    ));

    let name = codex_vault::ProviderAccountName::parse("work")?;
    assert!(
        codex_vault::Vault::new(home.path().to_path_buf())
            .remove_provider_account("openai", &name)?
    );
    let refused = match super::prepare_realtime_start(&owner, realtime_websocket_params()).await {
        Ok(_) => panic!("a removed named account must not start a realtime session"),
        Err(error) => error.to_string(),
    };
    assert!(
        refused.contains("account `work` has no sign-in (it may have been removed or signed out)")
            && refused.contains("corbanu login --account work"),
        "{refused}"
    );
    assert!(!refused.contains("sk-"), "{refused}");
    Ok(())
}

/// How the stand-in realtime server ends a session it accepted.
#[derive(Clone, Copy)]
enum StandInEnd {
    /// Keeps the session open until the client closes it.
    ClientCloses,
    /// Drops the connection after the client's first message.
    ServerDrops,
    /// Sends these server events after the client's first message, then
    /// keeps the session open until the client closes it.
    Replies(&'static [&'static str]),
}

/// A local stand-in for the realtime WebSocket endpoint: accepts sessions at
/// `http://127.0.0.1:<port>/v1` and answers nothing. No paid session is
/// opened.
async fn realtime_stand_in(end: StandInEnd) -> anyhow::Result<String> {
    use futures::StreamExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/v1", listener.local_addr()?);
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(socket).await else {
                    return;
                };
                let _ = ws.next().await;
                match end {
                    StandInEnd::ClientCloses => while let Some(Ok(_)) = ws.next().await {},
                    StandInEnd::ServerDrops => drop(ws),
                    StandInEnd::Replies(events) => {
                        use futures::SinkExt;
                        for event in events {
                            let message = tokio_tungstenite::tungstenite::Message::Text(
                                (*event).to_string().into(),
                            );
                            if ws.send(message).await.is_err() {
                                return;
                            }
                        }
                        while let Some(Ok(_)) = ws.next().await {}
                    }
                }
            });
        }
    });
    Ok(endpoint)
}

/// A persisted session whose OpenAI provider and realtime endpoint are the
/// stand-in at `endpoint`, recording under `accounting`.
async fn realtime_ledger_session(
    home: &std::path::Path,
    cwd: &std::path::Path,
    endpoint: &str,
    accounting: crate::config::AccountingMode,
) -> anyhow::Result<(
    Arc<crate::session::session::Session>,
    Arc<codex_state::StateRuntime>,
)> {
    realtime_ledger_session_with(home, cwd, endpoint, accounting, |_| {}).await
}

async fn realtime_ledger_session_with(
    home: &std::path::Path,
    cwd: &std::path::Path,
    endpoint: &str,
    accounting: crate::config::AccountingMode,
    configure: impl FnOnce(&mut crate::config::Config),
) -> anyhow::Result<(
    Arc<crate::session::session::Session>,
    Arc<codex_state::StateRuntime>,
)> {
    let (owner, db, _events) =
        realtime_ledger_session_with_events(home, cwd, endpoint, accounting, configure).await?;
    Ok((owner, db))
}

/// [`realtime_ledger_session_with`], also returning the session's events.
async fn realtime_ledger_session_with_events(
    home: &std::path::Path,
    cwd: &std::path::Path,
    endpoint: &str,
    accounting: crate::config::AccountingMode,
    configure: impl FnOnce(&mut crate::config::Config),
) -> anyhow::Result<(
    Arc<crate::session::session::Session>,
    Arc<codex_state::StateRuntime>,
    async_channel::Receiver<codex_protocol::protocol::Event>,
)> {
    use codex_state::SqliteConfig;
    use codex_state::StateRuntime;
    use codex_state::ThreadMetadataBuilder;
    use codex_utils_absolute_path::AbsolutePathBuf;

    let mut config = crate::session::tests::build_test_config(home, cwd).await;
    config.model_provider = codex_model_provider_info::ModelProviderInfo {
        request_max_retries: Some(0),
        stream_max_retries: Some(0),
        supports_websockets: false,
        ..codex_model_provider_info::ModelProviderInfo::create_openai_provider(Some(
            endpoint.to_string(),
        ))
    };
    config.model_provider_id = "openai".into();
    config.accounting = accounting;
    config
        .features
        .enable(codex_features::Feature::Sqlite)
        .expect("sqlite for the ledger");
    configure(&mut config);
    let (mut session, _context) =
        crate::session::tests::make_session_and_context_for_config(config).await;
    let events = crate::session::tests::capture_events(&mut session);
    let db = StateRuntime::init(
        SqliteConfig::from_sqlite_home(AbsolutePathBuf::try_from(home.to_path_buf())?),
        "openai".into(),
    )
    .await?;
    session.services.state_db = Some(db.clone());
    crate::session::tests::open_thread_persistence(&mut session).await;
    let owner = Arc::new(session);
    db.upsert_thread(
        &ThreadMetadataBuilder::new(
            owner.thread_id,
            home.join("realtime-fixture.jsonl"),
            chrono::Utc::now(),
            codex_protocol::protocol::SessionSource::Cli,
        )
        .build("openai"),
    )
    .await?;
    Ok((owner, db, events))
}

fn realtime_websocket_params() -> codex_protocol::protocol::ConversationStartParams {
    codex_protocol::protocol::ConversationStartParams {
        client_managed_handoffs: false,
        flush_transcript_tail_on_session_end: false,
        codex_responses_as_items: false,
        codex_response_item_prefix: None,
        codex_response_handoff_mode: CodexResponseHandoffMode::default(),
        codex_response_handoff_channel_prefixes: None,
        model: Some("gpt-realtime-1.5".into()),
        output_modality: codex_protocol::protocol::RealtimeOutputModality::Text,
        include_startup_context: false,
        initial_items: Vec::new(),
        prompt: Some(Some("fixture prompt".into())),
        realtime_session_id: None,
        transport: Some(codex_protocol::protocol::ConversationStartTransport::Websocket),
        version: Some(codex_protocol::protocol::RealtimeConversationVersion::V2),
        voice: None,
    }
}

/// The ledger's attempts and observations, as stored.
async fn realtime_ledger(
    db: &codex_state::StateRuntime,
) -> anyhow::Result<(Vec<serde_json::Value>, Vec<serde_json::Value>)> {
    let pool = db
        .sqlite()
        .open_read_only_pool(&db.sqlite().state_db_path())
        .await?;
    let read = |sql: &'static str| {
        let pool = pool.clone();
        async move {
            let rows: Vec<String> = sqlx::query_scalar(sql).fetch_all(&pool).await?;
            rows.iter()
                .map(|row| Ok(serde_json::from_str(row)?))
                .collect::<anyhow::Result<Vec<serde_json::Value>>>()
        }
    };
    let attempts = read("SELECT payload FROM draft_accounting_attempts ORDER BY rowid").await?;
    let observations =
        read("SELECT payload FROM draft_accounting_observations ORDER BY rowid").await?;
    pool.close().await;
    Ok((attempts, observations))
}

/// Whether the ledger's tables exist: a home that never recorded anything
/// has none.
async fn realtime_ledger_exists(db: &codex_state::StateRuntime) -> anyhow::Result<bool> {
    let pool = db
        .sqlite()
        .open_read_only_pool(&db.sqlite().state_db_path())
        .await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'draft_accounting_attempts')",
    )
    .fetch_one(&pool)
    .await?;
    pool.close().await;
    Ok(exists)
}

/// Wait until the ledger holds `count` observations, or give up after ten
/// seconds.
async fn realtime_ledger_settled(
    db: &codex_state::StateRuntime,
    count: usize,
) -> anyhow::Result<(Vec<serde_json::Value>, Vec<serde_json::Value>)> {
    realtime_ledger_settled_within(db, count, std::time::Duration::from_secs(10)).await
}

/// Wait until the ledger holds `count` observations, or give up after
/// `wait`. A check that nothing more is recorded waits the whole time.
async fn realtime_ledger_settled_within(
    db: &codex_state::StateRuntime,
    count: usize,
    wait: std::time::Duration,
) -> anyhow::Result<(Vec<serde_json::Value>, Vec<serde_json::Value>)> {
    let deadline = Instant::now() + wait;
    loop {
        if realtime_ledger_exists(db).await? {
            let ledger = realtime_ledger(db).await?;
            if ledger.1.len() >= count || Instant::now() > deadline {
                return Ok(ledger);
            }
        } else if Instant::now() > deadline {
            return Ok((Vec::new(), Vec::new()));
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// The one recorded realtime session: its route, model and provider, its
/// start (admission) and end times, and no token count at all.
fn assert_one_unknown_session(
    attempts: &[serde_json::Value],
    observations: &[serde_json::Value],
    started_after_ms: i64,
) {
    let realtime: Vec<&serde_json::Value> = attempts
        .iter()
        .filter(|attempt| {
            attempt["turn"]
                .as_str()
                .is_some_and(|turn| turn.starts_with("realtime:ws:"))
        })
        .collect();
    assert_eq!(realtime.len(), 1, "attempts: {attempts:#?}");
    assert_eq!(attempts.len(), 1, "attempts: {attempts:#?}");
    let session = realtime[0];
    assert_eq!(session["model"], "gpt-realtime-1.5");
    assert_eq!(session["provider"], "openai");
    assert_eq!(session["retry_of"], serde_json::Value::Null);
    let started = session["dispatched_at_ms"].as_i64().expect("start time");
    assert!(started >= started_after_ms, "{session:#?}");
    assert_eq!(observations.len(), 1, "observations: {observations:#?}");
    let patch = &observations[0]["patch"];
    let ended = patch["session_ended_at_ms"].as_i64().expect("end time");
    assert!(ended >= started, "{patch:#?}");
    // The end states nothing else: every token count stays unknown.
    assert_eq!(
        patch.as_object().map(serde_json::Map::len),
        Some(1),
        "{patch:#?}"
    );
}

/// #824 (Travis, 2026-10-15, option a): a realtime session over WebSocket is
/// recorded as one request - route, model, provider, start and end times -
/// with its tokens unknown, not zero. Before #824 it was not recorded at all.
#[tokio::test]
async fn pf_824_websocket_realtime_session_is_one_request_with_tokens_unknown() -> anyhow::Result<()>
{
    let endpoint = realtime_stand_in(StandInEnd::ClientCloses).await?;
    let home = tempfile::tempdir()?;
    let test_cwd = tempfile::tempdir()?;
    let (owner, db) = realtime_ledger_session(
        home.path(),
        test_cwd.path(),
        &endpoint,
        crate::config::AccountingMode::DirectOpenAiResponses {
            scope: uuid::Uuid::new_v4(),
            approved_endpoint: endpoint.clone(),
        },
    )
    .await?;
    let before = chrono::Utc::now().timestamp_millis();
    super::handle_start(&owner, "fixture-sub".into(), realtime_websocket_params()).await?;
    assert!(
        owner.conversation.running_state().await.is_some(),
        "the session started"
    );
    super::handle_close(&owner, "fixture-sub".into()).await;

    let (attempts, observations) = realtime_ledger_settled(&db, /*count*/ 1).await?;
    assert_one_unknown_session(&attempts, &observations, before);
    Ok(())
}

/// #889 D223 (Travis, 2026-10-28): a WebSocket session records the token
/// counts OpenAI reports at the end of each response - input, cached input,
/// output and total, as running sums - and its end after them. Its cost stays
/// unknown: no rate is bound, even for a configured model the catalogue
/// prices, so its tokens never read as a text model's dollar figure.
#[tokio::test]
async fn pf_889_d223_websocket_session_records_reported_tokens_with_no_price() -> anyhow::Result<()>
{
    const EVENTS: &[&str] = &[
        r#"{"type":"response.done","response":{"id":"resp_1","usage":{"total_tokens":253,"input_tokens":132,"output_tokens":121,"input_token_details":{"text_tokens":119,"audio_tokens":13,"cached_tokens":64},"output_token_details":{"text_tokens":30,"audio_tokens":91}}}}"#,
        // A response that reports no usage adds nothing.
        r#"{"type":"response.done","response":{"id":"resp_2"}}"#,
        r#"{"type":"response.done","response":{"id":"resp_3","usage":{"total_tokens":230,"input_tokens":200,"output_tokens":30,"input_token_details":{"cached_tokens":150}}}}"#,
    ];
    let endpoint = realtime_stand_in(StandInEnd::Replies(EVENTS)).await?;
    let home = tempfile::tempdir()?;
    let test_cwd = tempfile::tempdir()?;
    // API-key rates from the catalogue: a priced text model would bind them.
    let (owner, db) = realtime_ledger_session(
        home.path(),
        test_cwd.path(),
        &endpoint,
        crate::config::AccountingMode::DirectOpenAiResponses {
            scope: uuid::Uuid::new_v4(),
            approved_endpoint: endpoint.clone(),
        },
    )
    .await?;
    let mut params = realtime_websocket_params();
    params.model = Some("gpt-5.6-sol".into());
    super::handle_start(&owner, "fixture-sub".into(), params).await?;
    let (_, observations) = realtime_ledger_settled(&db, /*count*/ 2).await?;
    assert_eq!(observations.len(), 2, "{observations:#?}");
    super::handle_close(&owner, "fixture-sub".into()).await;

    let (attempts, observations) = realtime_ledger_settled(&db, /*count*/ 3).await?;
    assert_eq!(attempts.len(), 1, "one request: {attempts:#?}");
    assert_eq!(observations.len(), 3, "{observations:#?}");
    let revisions: Vec<i64> = observations
        .iter()
        .map(|observation| observation["revision"].as_i64().expect("revision"))
        .collect();
    assert_eq!(revisions, vec![1, 2, 3]);
    let patch = |index: usize| observations[index]["patch"].clone();
    assert_eq!(
        patch(0),
        serde_json::json!({"input": 132, "read": 64, "output": 121, "total": 253})
    );
    assert_eq!(
        patch(1),
        serde_json::json!({"input": 332, "read": 214, "output": 151, "total": 483})
    );
    let ended = patch(2)["session_ended_at_ms"].as_i64().expect("end time");
    assert_eq!(patch(2).as_object().map(serde_json::Map::len), Some(1));
    assert!(ended >= attempts[0]["dispatched_at_ms"].as_i64().expect("start"));

    let now = chrono::Utc::now().timestamp_millis();
    let codex_state::accounting::InspectionDay::Ready(view) =
        codex_state::accounting::AccountingStore::inspect_day(
            &db,
            owner.thread_id,
            now / 86_400_000,
            now,
        )
        .await?
    else {
        panic!("expected the day");
    };
    let quotes: Vec<&codex_state::accounting::ObservationQuote> =
        view.requests.values().flatten().collect();
    assert_eq!(quotes.len(), 1);
    let quote = quotes[0];
    assert_eq!(quote.attempt.model, "gpt-5.6-sol");
    assert_eq!(
        (
            quote.usage.input,
            quote.usage.read,
            quote.usage.output,
            quote.usage.total
        ),
        (Some(332), Some(214), Some(151), Some(483))
    );
    assert_eq!(quote.usage.session_ended_at_ms, Some(ended));
    let snapshot = quote.snapshot.as_ref().expect("a basis-only record");
    assert_eq!(snapshot.basis, codex_state::accounting::Basis::Billed);
    assert_eq!(snapshot.rates, codex_state::accounting::Rates::default());
    assert!(
        !quote
            .buckets
            .iter()
            .any(|bucket| matches!(bucket, codex_state::accounting::BucketQuote::Priced(_))),
        "{:?}",
        quote.buckets
    );
    assert_eq!(
        view.totals.known_usd,
        codex_state::accounting::Decimal::default()
    );
    assert_eq!(view.totals.per_use_unknown(), 1, "cost unknown, not zero");
    Ok(())
}

/// #824: a session the provider drops (an abnormal close) is recorded the
/// same way, with the time it ended.
#[tokio::test]
async fn pf_824_websocket_realtime_session_dropped_by_the_server_is_recorded() -> anyhow::Result<()>
{
    let endpoint = realtime_stand_in(StandInEnd::ServerDrops).await?;
    let home = tempfile::tempdir()?;
    let test_cwd = tempfile::tempdir()?;
    // The provider-bound mode a developer-accounting build uses.
    let (owner, db) = realtime_ledger_session(
        home.path(),
        test_cwd.path(),
        &endpoint,
        crate::config::AccountingMode::Provider {
            scope: uuid::Uuid::new_v4(),
            provider_id: "openai".into(),
            wire_api: codex_model_provider_info::WireApi::Responses,
            approved_endpoint: endpoint.clone(),
            approved_query: None,
            pricing: crate::config::PriceAuthority::Unavailable,
            basis_source: Default::default(),
            account: None,
        },
    )
    .await?;
    let before = chrono::Utc::now().timestamp_millis();
    super::handle_start(&owner, "fixture-sub".into(), realtime_websocket_params()).await?;

    let (attempts, observations) = realtime_ledger_settled(&db, /*count*/ 1).await?;
    assert_one_unknown_session(&attempts, &observations, before);
    super::handle_close(&owner, "fixture-sub".into()).await;
    Ok(())
}

/// #824: with collection off (any build without developer accounting), the
/// session runs exactly as before and nothing is recorded.
#[tokio::test]
async fn pf_824_websocket_realtime_session_is_not_recorded_when_collection_is_off()
-> anyhow::Result<()> {
    let endpoint = realtime_stand_in(StandInEnd::ClientCloses).await?;
    let home = tempfile::tempdir()?;
    let test_cwd = tempfile::tempdir()?;
    let (owner, db) = realtime_ledger_session(
        home.path(),
        test_cwd.path(),
        &endpoint,
        crate::config::AccountingMode::Disabled,
    )
    .await?;
    super::handle_start(&owner, "fixture-sub".into(), realtime_websocket_params()).await?;
    assert!(
        owner.conversation.running_state().await.is_some(),
        "the session started"
    );
    super::handle_close(&owner, "fixture-sub".into()).await;
    // Nothing was recorded, so this home never got a ledger at all.
    assert!(!realtime_ledger_exists(&db).await?);
    Ok(())
}

/// The provider-bound mode a developer-accounting build uses.
fn provider_bound(endpoint: &str) -> crate::config::AccountingMode {
    crate::config::AccountingMode::Provider {
        scope: uuid::Uuid::new_v4(),
        provider_id: "openai".into(),
        wire_api: codex_model_provider_info::WireApi::Responses,
        approved_endpoint: endpoint.to_string(),
        approved_query: None,
        pricing: crate::config::PriceAuthority::Unavailable,
        basis_source: Default::default(),
        account: None,
    }
}

/// #824 (review): a session on `experimental_realtime_ws_base_url` - a
/// `ws://` base that is not the provider's own base URL - is recorded against
/// that base in HTTP form, not left unrecorded on a scheme mismatch.
#[tokio::test]
async fn pf_824_websocket_session_on_a_ws_base_url_is_recorded() -> anyhow::Result<()> {
    let endpoint = realtime_stand_in(StandInEnd::ClientCloses).await?;
    let ws_base = endpoint.replacen("http://", "ws://", 1);
    let home = tempfile::tempdir()?;
    let test_cwd = tempfile::tempdir()?;
    let (owner, db) = realtime_ledger_session_with(
        home.path(),
        test_cwd.path(),
        "http://127.0.0.1:1/v1",
        provider_bound("http://127.0.0.1:1/v1"),
        |config| config.experimental_realtime_ws_base_url = Some(ws_base),
    )
    .await?;
    let before = chrono::Utc::now().timestamp_millis();
    super::handle_start(&owner, "fixture-sub".into(), realtime_websocket_params()).await?;
    assert!(
        owner.conversation.running_state().await.is_some(),
        "the session started"
    );
    super::handle_close(&owner, "fixture-sub".into()).await;

    let (attempts, observations) = realtime_ledger_settled(&db, /*count*/ 1).await?;
    assert_one_unknown_session(&attempts, &observations, before);
    Ok(())
}

/// #824 (review): a session replaced by a new start records its end before
/// the new one is admitted, and one the session's shutdown stops records its
/// end too.
#[tokio::test]
async fn pf_824_replaced_and_shut_down_sessions_record_their_ends() -> anyhow::Result<()> {
    let endpoint = realtime_stand_in(StandInEnd::ClientCloses).await?;
    let home = tempfile::tempdir()?;
    let test_cwd = tempfile::tempdir()?;
    let (owner, db) = realtime_ledger_session(
        home.path(),
        test_cwd.path(),
        &endpoint,
        provider_bound(&endpoint),
    )
    .await?;
    super::handle_start(&owner, "first".into(), realtime_websocket_params()).await?;
    super::handle_start(&owner, "second".into(), realtime_websocket_params()).await?;
    assert!(
        owner.conversation.running_state().await.is_some(),
        "the second session started"
    );
    let (attempts, observations) = realtime_ledger_settled(&db, /*count*/ 1).await?;
    assert_eq!(attempts.len(), 2, "{attempts:#?}");
    assert_eq!(observations.len(), 1, "{observations:#?}");
    let started = |turn: &str| {
        attempts
            .iter()
            .find(|attempt| attempt["turn"] == turn)
            .and_then(|attempt| attempt["dispatched_at_ms"].as_i64())
            .expect("an admitted session")
    };
    let first_end = observations[0]["patch"]["session_ended_at_ms"]
        .as_i64()
        .expect("the first session's end");
    assert!(started("realtime:ws:first") <= first_end);
    assert!(first_end <= started("realtime:ws:second"));

    owner.conversation.shutdown().await?;
    let (attempts, observations) = realtime_ledger_settled(&db, /*count*/ 2).await?;
    assert_eq!(attempts.len(), 2, "{attempts:#?}");
    assert_eq!(observations.len(), 2, "{observations:#?}");
    for observation in &observations {
        assert_eq!(
            observation["patch"].as_object().map(serde_json::Map::len),
            Some(1),
            "{observation:#?}"
        );
    }
    Ok(())
}

/// #824 (review): a WebSocket session always sends an API key, so it is
/// recorded as API-key work at its base URL in HTTP form, whatever the
/// session's sign-in; a WebRTC call keeps its own endpoint and sign-in.
#[test]
fn pf_824_websocket_session_is_recorded_as_api_key_work() {
    let api_provider = |base_url: &str| {
        let mut provider = codex_model_provider_info::ModelProviderInfo::create_openai_provider(
            /*base_url*/ None,
        )
        .to_api_provider(Some(codex_protocol::auth::AuthMode::ApiKey))
        .expect("api provider");
        provider.base_url = base_url.to_string();
        provider
    };
    let config = codex_api::RealtimeSessionConfig {
        instructions: String::new(),
        initial_items: Vec::new(),
        model: Some("gpt-realtime-1.5".into()),
        session_id: None,
        event_parser: RealtimeEventParser::RealtimeV2,
        session_mode: codex_api::RealtimeSessionMode::Conversational,
        output_modality: codex_protocol::protocol::RealtimeOutputModality::Text,
        voice: codex_protocol::protocol::RealtimeVoice::Alloy,
    };
    assert_eq!(
        super::realtime_accounting_route(
            &codex_protocol::protocol::ConversationStartTransport::Websocket,
            &api_provider("wss://proxy.example/v1"),
            /*realtime_call_api_provider*/ None,
            &config,
            "sub",
        ),
        (
            "https://proxy.example/v1".to_string(),
            Some(codex_protocol::auth::AuthMode::ApiKey),
            "realtime:ws:sub".to_string(),
        )
    );
    assert_eq!(
        super::realtime_accounting_route(
            &codex_protocol::protocol::ConversationStartTransport::Webrtc { sdp: String::new() },
            &api_provider("https://api.openai.com/v1"),
            Some(&api_provider("https://calls.example/v1")),
            &config,
            "sub",
        ),
        (
            "https://calls.example/v1".to_string(),
            /*auth_mode*/ None,
            "realtime:sub".to_string(),
        )
    );
}

/// #824 (review): under a ChatGPT sign-in, a WebSocket session recorded with
/// the API-key mode it actually uses gets API-key economics at OpenAI's API
/// route; recorded with the session's own sign-in it would not.
#[tokio::test]
async fn pf_824_websocket_session_under_chatgpt_sign_in_is_api_key_work() -> anyhow::Result<()> {
    const OPENAI_API: &str = "https://api.openai.com/v1";
    let home = tempfile::tempdir()?;
    let test_cwd = tempfile::tempdir()?;
    let (owner, _db) = realtime_ledger_session(
        home.path(),
        test_cwd.path(),
        OPENAI_API,
        provider_bound(OPENAI_API),
    )
    .await?;
    owner
        .services
        .active_auth
        .replace_for_testing(crate::test_support::auth_manager_from_auth(
            codex_login::CodexAuth::create_dummy_chatgpt_auth_for_testing(),
        ));
    let pricing = |auth_mode| {
        let owner = Arc::clone(&owner);
        async move {
            let (_session, scopes) = super::realtime_accounting(
                &owner,
                OPENAI_API,
                auth_mode,
                crate::accounting::realtime_session_turn_label("sub"),
            )
            .await
            .expect("realtime accounting attaches");
            match scopes.responses.as_ref().map(|deferred| deferred.mode()) {
                Some(crate::config::AccountingMode::Provider { pricing, .. }) => Some(*pricing),
                _ => None,
            }
        }
    };
    assert_eq!(
        pricing(Some(codex_protocol::auth::AuthMode::ApiKey)).await,
        Some(crate::config::PriceAuthority::ApiKeyRates)
    );
    assert_ne!(
        pricing(/*auth_mode*/ None).await,
        Some(crate::config::PriceAuthority::ApiKeyRates)
    );
    Ok(())
}

/// #824 (review round 2): end to end, a WebSocket session started under a
/// ChatGPT sign-in is admitted with the API key it sends. The API-key-only
/// mode admits nothing else, so a session resolved with the sign-in would
/// go unrecorded.
#[tokio::test]
async fn pf_824_websocket_session_under_chatgpt_sign_in_is_recorded_with_its_key()
-> anyhow::Result<()> {
    let endpoint = realtime_stand_in(StandInEnd::ClientCloses).await?;
    let home = tempfile::tempdir()?;
    let test_cwd = tempfile::tempdir()?;
    let (owner, db) = realtime_ledger_session(
        home.path(),
        test_cwd.path(),
        &endpoint,
        crate::config::AccountingMode::DirectOpenAiResponses {
            scope: uuid::Uuid::new_v4(),
            approved_endpoint: endpoint.clone(),
        },
    )
    .await?;
    // The session signs in with ChatGPT; the realtime key comes from the
    // model client's API-key sign-in, as the environment key would.
    owner
        .services
        .active_auth
        .replace_for_testing(crate::test_support::auth_manager_from_auth(
            codex_login::CodexAuth::create_dummy_chatgpt_auth_for_testing(),
        ));
    let before = chrono::Utc::now().timestamp_millis();
    super::handle_start(&owner, "fixture-sub".into(), realtime_websocket_params()).await?;
    super::handle_close(&owner, "fixture-sub".into()).await;

    let (attempts, observations) = realtime_ledger_settled(&db, /*count*/ 1).await?;
    assert_one_unknown_session(&attempts, &observations, before);
    Ok(())
}

/// #889 item 1 (Travis, 2026-10-16, D15): a WebSocket session that sends
/// the ambient `OPENAI_API_KEY` while a named OpenAI account is selected is
/// left unrecorded rather than grouped under that account in `/cost`; the
/// session itself runs as before. The same start with the key it sends is
/// recorded.
#[tokio::test]
async fn pf_889_ambient_key_session_under_a_named_account_is_left_unrecorded() -> anyhow::Result<()>
{
    for ambient_under_named_account in [false, true] {
        let endpoint = realtime_stand_in(StandInEnd::ClientCloses).await?;
        let home = tempfile::tempdir()?;
        let test_cwd = tempfile::tempdir()?;
        let (owner, db) = realtime_ledger_session(
            home.path(),
            test_cwd.path(),
            &endpoint,
            provider_bound(&endpoint),
        )
        .await?;
        let mut prepared =
            super::prepare_realtime_start(&owner, realtime_websocket_params()).await?;
        assert!(matches!(
            prepared.websocket_credential,
            super::RealtimeRecordKey::Sent(_)
        ));
        if ambient_under_named_account {
            // What `RealtimeRecordKey::for_websocket` decides for that key
            // (see the test above); the environment key itself can't be set
            // safely in a test.
            prepared.websocket_credential = super::RealtimeRecordKey::AmbientUnderNamedAccount;
        }
        let before = chrono::Utc::now().timestamp_millis();
        super::handle_start_inner(&owner, "fixture-sub", prepared).await?;
        assert!(
            owner.conversation.running_state().await.is_some(),
            "the session started"
        );
        super::handle_close(&owner, "fixture-sub".into()).await;

        if ambient_under_named_account {
            // A session is admitted before its start returns, so nothing
            // later can add one; the home never got a ledger at all.
            assert!(!realtime_ledger_exists(&db).await?);
        } else {
            let (attempts, observations) = realtime_ledger_settled(&db, /*count*/ 1).await?;
            assert_one_unknown_session(&attempts, &observations, before);
        }
    }
    Ok(())
}
