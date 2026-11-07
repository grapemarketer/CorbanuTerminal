#![allow(
    clippy::disallowed_methods,
    reason = "#[traced_test] installs tracing-test's in-memory global subscriber"
)]
use super::*;
use crate::accounting::Sampling;
use crate::accounting::SamplingScope;
use crate::accounting::transport::AccountingTransport;
use crate::accounting::transport::ResponseEvidence;
use crate::config::AccountingMode;
use codex_api::AnthropicTokenPresence;
use codex_api::AnthropicUsageObserver;
use codex_api::AnthropicUsagePatch;
use codex_protocol::protocol::SessionSource;
use codex_state::SqliteConfig;
use codex_state::StateRuntime;
use codex_state::ThreadMetadataBuilder;
use codex_state::accounting::Attempt;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use sqlx::Connection;
use sqlx::SqliteConnection;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use uuid::Uuid;

/// Waits until the ledger writer has written what it was handed. A slow write
/// is put off and written later (#865), so read the ledger after this.
async fn settle(db: &StateRuntime) {
    assert!(
        db.flush_accounting(std::time::Duration::from_secs(30))
            .await,
        "records still queued"
    );
}

const ENDPOINT: &str = "http://127.0.0.1:1/v1";

#[tokio::test]
async fn accounting_build_activation_uses_loaded_provider() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let config = crate::config::Config::load_config_with_layer_stack(
        codex_exec_server::LOCAL_FS.as_ref(),
        codex_config::config_toml::ConfigToml {
            model_provider: Some("anthropic".into()),
            ..Default::default()
        },
        crate::config::ConfigOverrides {
            cwd: Some(home.path().to_path_buf()),
            ..Default::default()
        },
        AbsolutePathBuf::from_absolute_path(home.path())?,
        codex_config::ConfigLayerStack::default(),
    )
    .await?;
    #[cfg(not(feature = "developer-accounting"))]
    assert_eq!(config.accounting, AccountingMode::Disabled);
    #[cfg(feature = "developer-accounting")]
    assert!(matches!(
        config.accounting,
        AccountingMode::Provider { approved_endpoint, .. }
            if approved_endpoint == codex_model_provider_info::ANTHROPIC_BASE_URL
    ));
    Ok(())
}

#[cfg(feature = "developer-accounting")]
#[tokio::test]
async fn accounting_developer_loader_override_survives_refresh() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    for mode in [
        AccountingMode::Disabled,
        AccountingMode::DirectOpenAiResponses {
            scope: Uuid::new_v4(),
            approved_endpoint: ENDPOINT.into(),
        },
    ] {
        let mut config = crate::config::Config::load_config_with_layer_stack(
            codex_exec_server::LOCAL_FS.as_ref(),
            codex_config::config_toml::ConfigToml::default(),
            crate::config::ConfigOverrides {
                cwd: Some(home.path().to_path_buf()),
                ..Default::default()
            },
            AbsolutePathBuf::from_absolute_path(home.path())?,
            codex_config::ConfigLayerStack::default(),
        )
        .await?;
        config.accounting = mode.clone();
        assert_eq!(config.accounting, mode);
        let mut refreshed = config.rebuild_preserving_session_layers(&config).await?;
        assert_eq!(refreshed.accounting, mode);
        refreshed.model_provider_id = "anthropic".into();
        let switched = refreshed
            .rebuild_preserving_session_layers(&refreshed)
            .await?;
        assert_eq!(switched.model_provider_id, "anthropic");
        assert_eq!(switched.accounting, mode);
    }
    Ok(())
}

#[cfg(feature = "developer-accounting")]
#[test]
fn accounting_developer_activation_accepts_provider_id_independently_of_dialect() {
    for id in [
        "anthropic",
        "openai",
        "claude-plan",
        "openrouter",
        "corbanu",
        "custom",
    ] {
        for wire in [
            codex_model_provider_info::WireApi::Anthropic,
            codex_model_provider_info::WireApi::Responses,
            codex_model_provider_info::WireApi::Chat,
        ] {
            let mut provider = ModelProviderInfo::create_anthropic_provider();
            provider.wire_api = wire;
            assert!(matches!(
                crate::accounting::developer_accounting_mode(id, &provider),
                AccountingMode::Provider { provider_id, wire_api, .. } if provider_id == id && wire_api == wire
            ));
        }
    }
}

#[cfg(feature = "developer-accounting")]
#[tokio::test]
async fn accounting_developer_activation_samples_http_and_installs_inspectable_day()
-> anyhow::Result<()> {
    use codex_state::accounting::AccountingStore;
    use codex_state::accounting::InspectionDay;
    use futures::StreamExt;
    use serde_json::json;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;

    struct FixtureAuth;
    impl codex_api::AuthProvider for FixtureAuth {
        fn add_auth_headers(&self, _: &mut http::HeaderMap) {}
    }

    let server = MockServer::start().await;
    let endpoint = format!("{}/v1", server.uri());
    let events = [
        json!({"type":"message_start","message":{"id":"activation-fixture","model":"claude-opus-5","usage":{"input_tokens":7,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"fixture complete"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}),
        json!({"type":"message_stop"}),
    ];
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .expect(1)
        .mount(&server)
        .await;
    let home = tempfile::tempdir()?;
    // Built-in Anthropic intentionally ignores TOML endpoint overrides.
    // Bind the local embedding fixture through the production developer selector.
    let mode = crate::accounting::developer_accounting_mode(
        "anthropic",
        &ModelProviderInfo {
            base_url: Some(endpoint.clone()),
            ..ModelProviderInfo::create_anthropic_provider()
        },
    );
    let db = StateRuntime::init(
        SqliteConfig::from_sqlite_home(AbsolutePathBuf::try_from(home.path())?),
        "anthropic".into(),
    )
    .await?;
    let owner = ThreadId::new();
    db.upsert_thread(
        &ThreadMetadataBuilder::new(
            owner,
            home.path().join("activation.jsonl"),
            chrono::Utc::now(),
            SessionSource::Cli,
        )
        .build("anthropic"),
    )
    .await?;
    let now = chrono::Utc::now().timestamp_millis();
    assert_eq!(
        AccountingStore::inspect_day(&db, owner, now / 86_400_000, now).await?,
        InspectionDay::Absent
    );
    let sampling = Sampling::start(db.clone(), owner, "activation-turn".into(), &mode).await?;
    let evidence = ResponseEvidence::new(sampling.clone());
    let transport = AccountingTransport::new(
        codex_http_client::ReqwestTransport::from_http_client(
            codex_http_client::HttpClientBuilder::new().build_direct()?,
        ),
        Some(evidence.clone()),
        "claude-opus-5".into(),
    );
    let provider = codex_api::Provider {
        name: "anthropic".into(),
        base_url: endpoint,
        query_params: None,
        headers: Default::default(),
        retry: codex_api::RetryConfig {
            max_attempts: 1,
            base_delay: std::time::Duration::ZERO,
            retry_429: false,
            retry_5xx: false,
            retry_transport: false,
        },
        stream_idle_timeout: std::time::Duration::from_secs(5),
    };
    let client =
        codex_api::AnthropicMessagesClient::new(transport, provider, Arc::new(FixtureAuth))
            .with_usage_observer(Some(evidence.clone()));
    let mut stream = client
        .stream_request(
            codex_api::AnthropicMessagesRequest {
                model: "claude-opus-5".into(),
                system: vec![],
                messages: vec![],
                tools: vec![],
                tool_choice: None,
                stream: true,
                max_tokens: 10,
                thinking: None,
                output_config: None,
                provider_options: None,
            },
            Default::default(),
        )
        .await?;
    while let Some(event) = stream.next().await {
        event?;
    }
    settle(&db).await;
    let now = chrono::Utc::now().timestamp_millis();
    let day = AccountingStore::inspect_day(&db, owner, now / 86_400_000, now).await?;
    let InspectionDay::Ready(view) = day else {
        anyhow::bail!("expected populated day, got {day:?}");
    };
    assert_eq!(view.requests.len(), 1);
    let quote = &view.requests.values().next().unwrap()[0];
    assert_eq!(quote.observations.len(), 2);
    assert_eq!(quote.attempt.model, "claude-opus-5");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    drop(stream);
    drop(client);
    drop(evidence);
    drop(sampling);
    db.close().await;
    Ok(())
}

struct DeniedAuth;

impl codex_api::AuthProvider for DeniedAuth {
    fn add_auth_headers(&self, _: &mut http::HeaderMap) {}
    fn apply_auth(&self, _: Request) -> codex_api::AuthProviderFuture<'_> {
        Box::pin(async {
            Err(codex_api::AuthError::Build(
                "synthetic preflight denial".into(),
            ))
        })
    }
}

#[tokio::test]
async fn accounting_awaited_auth_denial_creates_no_attempt_or_transport_send() -> anyhow::Result<()>
{
    let fixture = Fixture::new().await?;
    let sends = Arc::new(AtomicUsize::new(0));
    let evidence = ResponseEvidence::new(fixture.sampling.clone());
    let transport = StageOneGuardedTransport::new(
        AccountingTransport::new(
            Probe {
                sends: sends.clone(),
                fail_first: false,
            },
            Some(evidence.clone()),
            "claude-opus-5".into(),
        ),
        /*binding*/ None,
    );
    let provider = codex_api::Provider {
        name: "anthropic".into(),
        base_url: ENDPOINT.into(),
        query_params: None,
        headers: Default::default(),
        retry: codex_api::RetryConfig {
            max_attempts: 2,
            base_delay: std::time::Duration::ZERO,
            retry_429: true,
            retry_5xx: true,
            retry_transport: true,
        },
        stream_idle_timeout: std::time::Duration::from_secs(1),
    };
    let client = codex_api::AnthropicMessagesClient::new(transport, provider, Arc::new(DeniedAuth))
        .with_usage_observer(Some(evidence));
    let request = codex_api::AnthropicMessagesRequest {
        model: "claude-opus-5".into(),
        system: vec![],
        messages: vec![],
        tools: vec![],
        tool_choice: None,
        stream: true,
        max_tokens: 10,
        thinking: None,
        output_config: None,
        provider_options: None,
    };
    assert!(
        client
            .stream_request(request, Default::default())
            .await
            .is_err()
    );
    assert_eq!(fixture.attempts().await?, vec![]);
    assert_eq!(sends.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn accounting_role_reload_preserves_internal_binding_without_changing_off_route_behavior()
-> anyhow::Result<()> {
    for on in [false, true] {
        let mut config = crate::config::test_config().await;
        config.model_provider_id = "anthropic".into();
        config.model = Some("claude-opus-5".into());
        config.model_provider = ModelProviderInfo {
            base_url: Some(ENDPOINT.into()),
            env_key: None,
            experimental_bearer_token: Some("synthetic".into()),
            ..ModelProviderInfo::create_anthropic_provider()
        };
        assert_eq!(config.accounting, AccountingMode::Disabled);
        if on {
            config.accounting = AccountingMode::DirectAnthropic {
                scope: Uuid::new_v4(),
                approved_endpoint: ENDPOINT.into(),
            };
        }
        let path = config.codex_home.join("accounting-role.toml");
        std::fs::create_dir_all(config.codex_home.as_path())?;
        std::fs::write(&path, "model_reasoning_effort = \"low\"")?;
        config.agent_roles.insert(
            "accounting-role".into(),
            crate::config::AgentRoleConfig {
                config_file: Some(path.to_path_buf()),
                description: Some("fixture".into()),
                nickname_candidates: None,
            },
        );
        let mode = config.accounting.clone();
        let provider = config.model_provider.clone();
        crate::agent::role::apply_role_to_config(&mut config, Some("accounting-role"))
            .await
            .map_err(anyhow::Error::msg)?;
        assert_eq!(config.accounting, mode);
        if on {
            assert_eq!(config.model_provider, provider);
        } else {
            assert_eq!(
                config.model_provider,
                ModelProviderInfo::create_anthropic_provider()
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn accounting_role_overlay_keeps_explicit_provider_fields_and_validates_conflicts()
-> anyhow::Result<()> {
    let mut config = crate::config::test_config().await;
    config.model_provider_id = "anthropic".into();
    config.model = Some("claude-opus-5".into());
    config.model_provider = ModelProviderInfo {
        base_url: Some(ENDPOINT.into()),
        env_key: None,
        experimental_bearer_token: Some("synthetic-parent".into()),
        http_headers: Some([("x-parent".into(), "preserved".into())].into()),
        ..ModelProviderInfo::create_anthropic_provider()
    };
    config.accounting = AccountingMode::DirectAnthropic {
        scope: Uuid::new_v4(),
        approved_endpoint: ENDPOINT.into(),
    };
    let role = config.codex_home.join("accounting-overlay.toml");
    std::fs::create_dir_all(config.codex_home.as_path())?;
    std::fs::write(
        &role,
        r#"
model_provider = "anthropic"
[model_providers.anthropic]
name = "explicit role"
base_url = "http://127.0.0.1:2/v1"
experimental_bearer_token = "synthetic-child"
wire_api = "anthropic"
request_max_retries = 0
stream_max_retries = 0
stream_idle_timeout_ms = 10000
stream_actionable_timeout_ms = 5000
stream_long_failure_retry_threshold_ms = 2000
stream_long_failure_max_retries = 0
websocket_connect_timeout_ms = 3000
[model_providers.anthropic.http_headers]
x-child = "explicit"
[model_providers.anthropic.runtime_policy]
request_body_max_bytes = 30000000
retry_request_body_max_bytes = 15000000
web_search_max_uses = 2
"#,
    )?;
    config.agent_roles.insert(
        "overlay".into(),
        crate::config::AgentRoleConfig {
            config_file: Some(role.to_path_buf()),
            description: None,
            nickname_candidates: None,
        },
    );
    let mode = config.accounting.clone();
    let mut expected = ModelProviderInfo {
        name: "explicit role".into(),
        base_url: Some("http://127.0.0.1:2/v1".into()),
        experimental_bearer_token: Some("synthetic-child".into()),
        request_max_retries: Some(0),
        stream_max_retries: Some(0),
        stream_idle_timeout_ms: Some(10000),
        stream_actionable_timeout_ms: Some(5000),
        stream_long_failure_retry_threshold_ms: Some(2000),
        stream_long_failure_max_retries: Some(0),
        websocket_connect_timeout_ms: Some(3000),
        http_headers: Some(
            [
                ("x-parent".into(), "preserved".into()),
                ("x-child".into(), "explicit".into()),
            ]
            .into(),
        ),
        ..config.model_provider.clone()
    };
    expected.runtime_policy.web_search_max_uses = Some(2);
    crate::agent::role::apply_role_to_config(&mut config, Some("overlay"))
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(config.model_provider, expected);
    assert_eq!(config.model_providers["anthropic"], expected);
    assert_eq!(config.accounting, mode);
    // A wire bypass or conflicting command credential is rejected, not silently
    // dropped or used to authorize a different route/credential source.
    for overlay in [
        "[model_providers.anthropic]\nname = \"wire override\"\nwire_api = \"responses\"",
        "[model_providers.anthropic]\nname = \"auth override\"\n[model_providers.anthropic.auth]\ncommand = \"never-executed-fixture\"",
    ] {
        std::fs::write(&role, overlay)?;
        assert!(
            crate::agent::role::apply_role_to_config(&mut config, Some("overlay"))
                .await
                .is_err()
        );
        assert_eq!(config.model_provider, expected);
        assert_eq!(config.accounting, mode);
    }
    Ok(())
}

struct Fixture {
    _home: tempfile::TempDir,
    owner: ThreadId,
    db: Arc<StateRuntime>,
    sampling: Arc<Sampling>,
}

impl Fixture {
    async fn new() -> anyhow::Result<Self> {
        Self::with_endpoint(ENDPOINT).await
    }

    async fn with_endpoint(endpoint: &str) -> anyhow::Result<Self> {
        let home = tempfile::tempdir()?;
        let db = StateRuntime::init(
            SqliteConfig::from_sqlite_home(AbsolutePathBuf::try_from(home.path().to_path_buf())?),
            "anthropic".into(),
        )
        .await?;
        let owner = ThreadId::new();
        let metadata = ThreadMetadataBuilder::new(
            owner,
            home.path().join("fixture.jsonl"),
            chrono::Utc::now(),
            SessionSource::Cli,
        );
        db.upsert_thread(&metadata.build("anthropic")).await?;
        let sampling = Sampling::start(
            db.clone(),
            owner,
            "turn".into(),
            &AccountingMode::DirectAnthropic {
                scope: Uuid::new_v4(),
                approved_endpoint: endpoint.into(),
            },
        )
        .await?;
        Ok(Self {
            _home: home,
            owner,
            db,
            sampling,
        })
    }

    async fn connection(&self) -> anyhow::Result<SqliteConnection> {
        let pool = self
            .db
            .sqlite()
            .open_read_write_pool(&self.db.sqlite().state_db_path())
            .await?;
        let mut connection = pool.acquire().await?.detach();
        pool.close().await;
        sqlx::query("PRAGMA foreign_keys=ON")
            .execute(&mut connection)
            .await?;
        Ok(connection)
    }

    async fn attempts(&self) -> anyhow::Result<Vec<Attempt>> {
        // A read snapshot must not initialize writable connection pragmas while
        // the separate fault-fixture connection intentionally holds its writer.
        let pool = self
            .db
            .sqlite()
            .open_read_only_pool(&self.db.sqlite().state_db_path())
            .await?;
        let mut connection = pool.acquire().await?.detach();
        pool.close().await;
        let values: Vec<String> =
            sqlx::query_scalar("SELECT payload FROM draft_accounting_attempts ORDER BY rowid")
                .fetch_all(&mut connection)
                .await?;
        connection.close().await?;
        values
            .into_iter()
            .map(|value| serde_json::from_str(&value).map_err(Into::into))
            .collect()
    }
}

struct Probe {
    sends: Arc<AtomicUsize>,
    fail_first: bool,
}

impl HttpTransport for Probe {
    async fn execute(&self, _: Request) -> Result<Response, TransportError> {
        unreachable!("streaming fixture")
    }
    async fn stream(&self, _: Request) -> Result<StreamResponse, TransportError> {
        let number = self.sends.fetch_add(1, Ordering::SeqCst);
        if self.fail_first && number == 0 {
            return Err(TransportError::Timeout);
        }
        Ok(StreamResponse {
            status: http::StatusCode::OK,
            headers: Default::default(),
            bytes: futures::stream::empty().boxed(),
        })
    }
}

fn request() -> Request {
    Request {
        method: http::Method::POST,
        url: format!("{ENDPOINT}/messages"),
        headers: Default::default(),
        body: None,
        compression: Default::default(),
        timeout: None,
    }
}

fn usage(value: i64) -> AnthropicUsagePatch {
    AnthropicUsagePatch {
        input_tokens: AnthropicTokenPresence::Number(value),
        ..Default::default()
    }
}

#[tokio::test]
async fn accounting_actual_transport_retries_and_old_response_keep_distinct_owned_identity()
-> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let sends = Arc::new(AtomicUsize::new(0));
    let first = ResponseEvidence::new(fixture.sampling.clone());
    let transport = AccountingTransport::new(
        Probe {
            sends: sends.clone(),
            fail_first: true,
        },
        Some(first.clone()),
        "claude-opus-5".into(),
    );
    assert!(transport.stream(request()).await.is_err());
    transport.stream(request()).await?;
    first
        .observe(/*position*/ 1, Ok(usage(/*value*/ 7)))
        .await?;
    let second = ResponseEvidence::new(fixture.sampling.clone());
    AccountingTransport::new(
        Probe {
            sends: sends.clone(),
            fail_first: false,
        },
        Some(second.clone()),
        "claude-opus-5".into(),
    )
    .stream(request())
    .await?;
    second
        .observe(/*position*/ 1, Ok(usage(/*value*/ 11)))
        .await?;
    first
        .observe(/*position*/ 2, Ok(usage(/*value*/ 8)))
        .await?;
    settle(&fixture.db).await;
    let attempts = fixture.attempts().await?;
    assert_eq!(attempts.len(), 3);
    assert_eq!(
        attempts
            .iter()
            .map(|attempt| (
                attempt.request_id,
                attempt.thread_id,
                attempt.turn.clone(),
                attempt.retry_of
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                attempts[0].request_id,
                attempts[0].thread_id,
                "turn".into(),
                None
            ),
            (
                attempts[0].request_id,
                attempts[0].thread_id,
                "turn".into(),
                Some(attempts[0].attempt_id)
            ),
            (
                attempts[0].request_id,
                attempts[0].thread_id,
                "turn".into(),
                Some(attempts[1].attempt_id)
            )
        ]
    );
    // Sorted: the writer merges records queued for the same attempt, so a
    // record it put off changes the order rows are written in.
    let mut rows: Vec<(String, i64, String)> = sqlx::query_as("SELECT attempt_id, revision, json_extract(payload, '$.patch') FROM draft_accounting_observations")
        .fetch_all(&mut fixture.connection().await?).await?;
    rows.sort();
    let mut expected: Vec<(String, i64, String)> = vec![
        (
            attempts[1].attempt_id.to_string(),
            1,
            r#"{"input":7}"#.into(),
        ),
        (
            attempts[2].attempt_id.to_string(),
            1,
            r#"{"input":11}"#.into(),
        ),
        (
            attempts[1].attempt_id.to_string(),
            2,
            r#"{"input":8}"#.into(),
        ),
    ];
    expected.sort();
    assert_eq!(rows, expected);
    assert_eq!(sends.load(Ordering::SeqCst), 3);
    Ok(())
}

/// The saved records of `fixture`'s ledger writer not yet in the ledger.
fn pending_records(fixture: &Fixture) -> usize {
    std::fs::read_dir(
        fixture
            .db
            .sqlite()
            .home()
            .join(codex_state::accounting::PENDING_DIR),
    )
    .map(|listing| {
        listing
            .flatten()
            .filter(|item| item.path().extension().is_some_and(|ext| ext == "json"))
            .count()
    })
    .unwrap_or(0)
}

/// The admission is durable before the request is sent: in the ledger, or,
/// while another process holds the state DB's write lock, saved for the
/// ledger writer. A busy state DB holds the request up for at most about
/// `QUICK_BUSY_TIMEOUT`.
#[tokio::test]
async fn accounting_admission_is_durable_before_send_and_a_busy_state_db_does_not_hold_it_up()
-> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    // Uncontended: in the ledger before the send.
    let sends = Arc::new(AtomicUsize::new(0));
    AccountingTransport::new(
        Probe {
            sends: sends.clone(),
            fail_first: false,
        },
        Some(ResponseEvidence::new(fixture.sampling.clone())),
        "claude-opus-5".into(),
    )
    .stream(request())
    .await?;
    assert_eq!(
        (
            fixture.attempts().await?.len(),
            sends.load(Ordering::SeqCst)
        ),
        (1, 1)
    );

    let mut conn = fixture.connection().await?;
    let transaction = conn.begin_with("BEGIN IMMEDIATE").await?;
    let transport = AccountingTransport::new(
        Probe {
            sends: sends.clone(),
            fail_first: false,
        },
        Some(ResponseEvidence::new(fixture.sampling.clone())),
        "claude-opus-5".into(),
    );
    let started = std::time::Instant::now();
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        transport.stream(request()),
    )
    .await??;
    assert!(
        started.elapsed() < codex_state::QUICK_BUSY_TIMEOUT + std::time::Duration::from_millis(750),
        "held up {:?}",
        started.elapsed()
    );
    assert_eq!(sends.load(Ordering::SeqCst), 2);
    // Saved, not yet in the ledger.
    assert_eq!(pending_records(&fixture), 1);
    transaction.rollback().await?;
    assert!(
        fixture
            .db
            .flush_accounting(std::time::Duration::from_secs(30))
            .await
    );
    assert_eq!(fixture.attempts().await?.len(), 2);
    assert_eq!(pending_records(&fixture), 0);
    fixture.sampling.check()?;
    Ok(())
}

#[tokio::test]
async fn accounting_guard_denial_precedes_admission_and_never_sends() -> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let sends = Arc::new(AtomicUsize::new(0));
    let binding = Arc::new(StageOneMemoryBinding {
        owner: Weak::new(),
        termination: futures::future::pending().boxed().shared(),
        owner_id: ThreadId::new(),
        provider: ModelProviderInfo::create_anthropic_provider(),
        floor: SecurityLevel::Permissive,
        runtime_nonce: [0; 16],
        session_id: "fixture".into(),
        denial: Mutex::new(None),
        unlabelled_request: std::sync::atomic::AtomicBool::new(false),
    });
    let transport = StageOneGuardedTransport::new(
        AccountingTransport::new(
            Probe {
                sends: sends.clone(),
                fail_first: false,
            },
            Some(ResponseEvidence::new(fixture.sampling.clone())),
            "claude-opus-5".into(),
        ),
        Some(binding),
    );
    assert!(transport.stream(request()).await.is_err());
    assert_eq!(
        (
            fixture.attempts().await?.len(),
            sends.load(Ordering::SeqCst)
        ),
        (0, 0)
    );
    assert!(fixture.sampling.check().is_ok());
    Ok(())
}

#[tokio::test]
async fn accounting_admission_failure_and_route_mismatch_serve_the_request_unrecorded()
-> anyhow::Result<()> {
    for fault in ["admission", "route", "missing_owner"] {
        let fixture = Fixture::new().await?;
        if fault == "admission" {
            sqlx::query("CREATE TRIGGER reject_admission BEFORE INSERT ON draft_accounting_attempts BEGIN SELECT RAISE(ABORT, 'fixture-admission'); END")
                .execute(&mut fixture.connection().await?).await?;
        }
        if fault == "missing_owner" {
            fixture.db.delete_thread(fixture.owner).await?;
        }
        let sends = Arc::new(AtomicUsize::new(0));
        let transport = AccountingTransport::new(
            Probe {
                sends: sends.clone(),
                fail_first: false,
            },
            Some(ResponseEvidence::new(fixture.sampling.clone())),
            "claude-opus-5".into(),
        );
        let mut outbound = request();
        if fault == "route" {
            outbound.url = "http://127.0.0.1:2/v1/messages".into();
        }
        // Accounting never stops a request: both are sent, neither recorded,
        // and the sampling stays closed for the rest of the turn.
        transport.stream(outbound).await?;
        transport.stream(request()).await?;
        assert_eq!(
            (
                fixture.attempts().await?.len(),
                sends.load(Ordering::SeqCst)
            ),
            (0, 2)
        );
        assert!(fixture.sampling.is_closed());
    }
    Ok(())
}

/// #654: under Aggressive the credential broker serves model requests, and
/// each reaches the transport rewritten to the broker's plain-HTTP URL. Every
/// such request is recorded against the approved HTTPS route, as it is when
/// the request goes out directly.
#[tokio::test]
async fn issue_654_brokered_requests_are_all_recorded() -> anyhow::Result<()> {
    const PROVIDER: &str = "https://api.z.ai/api/anthropic/v1";
    let fixture = Fixture::with_endpoint(PROVIDER).await?;
    let sends = Arc::new(AtomicUsize::new(0));
    let requests = 3;
    for _ in 0..requests {
        let brokered = crate::model_broker_auth::rewritten_for_broker_in_tests(Request {
            url: format!("{PROVIDER}/messages"),
            ..request()
        });
        assert!(brokered.url.starts_with("http://api.z.ai:443/"));
        AccountingTransport::new(
            Probe {
                sends: sends.clone(),
                fail_first: false,
            },
            Some(ResponseEvidence::new(fixture.sampling.clone())),
            "glm-5.3-flash".into(),
        )
        .stream(brokered)
        .await?;
    }
    settle(&fixture.db).await;
    assert_eq!(
        (
            sends.load(Ordering::SeqCst),
            fixture.attempts().await?.len()
        ),
        (requests, requests),
        "every request the broker served is recorded"
    );
    assert!(!fixture.sampling.is_closed());

    // A brokered request to another route is still not attributed to this one.
    let other = Fixture::with_endpoint(PROVIDER).await?;
    let transport = AccountingTransport::new(
        Probe {
            sends: sends.clone(),
            fail_first: false,
        },
        Some(ResponseEvidence::new(other.sampling.clone())),
        "glm-5.3-flash".into(),
    );
    transport
        .stream(crate::model_broker_auth::rewritten_for_broker_in_tests(
            Request {
                url: "https://api.z.ai:8443/api/anthropic/v1/messages".into(),
                ..request()
            },
        ))
        .await?;
    settle(&other.db).await;
    assert_eq!(other.attempts().await?.len(), 0);
    assert!(other.sampling.is_closed());
    Ok(())
}

#[tokio::test]
async fn accounting_scope_drop_clears_cancelled_sampling_and_observation_failure_closes_it()
-> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let slot = Default::default();
    let scope = SamplingScope::attach(Arc::clone(&slot), Some(fixture.sampling.clone()))?;
    assert!(slot.lock().unwrap().is_some());
    drop(scope);
    assert!(slot.lock().unwrap().is_none());
    let evidence = ResponseEvidence::new(fixture.sampling.clone());
    let sends = Arc::new(AtomicUsize::new(0));
    AccountingTransport::new(
        Probe {
            sends: sends.clone(),
            fail_first: false,
        },
        Some(evidence.clone()),
        "claude-opus-5".into(),
    )
    .stream(request())
    .await?;
    evidence
        .observe(/*position*/ 1, Ok(usage(/*value*/ 7)))
        .await?;
    settle(&fixture.db).await;
    sqlx::query("CREATE TRIGGER reject_observation BEFORE INSERT ON draft_accounting_observations BEGIN SELECT RAISE(ABORT, 'fixture-observation'); END")
        .execute(&mut fixture.connection().await?).await?;
    // The stream carries on; only recording stops.
    evidence
        .observe(/*position*/ 2, Ok(usage(/*value*/ 9)))
        .await?;
    assert!(fixture.sampling.is_closed());
    settle(&fixture.db).await;
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT json_extract(payload, '$.patch') FROM draft_accounting_observations",
    )
    .fetch_all(&mut fixture.connection().await?)
    .await?;
    assert_eq!(rows, vec![r#"{"input":7}"#]);
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    Ok(())
}

/// Another Corbanu process sharing the state DB can hold its write lock for
/// seconds. That used to hold every admission and observation up for up to
/// 15 s and then send the request unrecorded, losing its record. Now the
/// records are put off: the request and its stream go on at once, and the
/// records are written once the lock is free.
#[tokio::test]
#[tracing_test::traced_test]
async fn accounting_puts_records_off_while_another_process_holds_the_state_db() -> anyhow::Result<()>
{
    const HELD: std::time::Duration = std::time::Duration::from_millis(3_000);
    let fixture = Fixture::new().await?;
    let sends = Arc::new(AtomicUsize::new(0));
    let evidence = ResponseEvidence::new(fixture.sampling.clone());
    let transport = AccountingTransport::new(
        Probe {
            sends: sends.clone(),
            fail_first: false,
        },
        Some(evidence.clone()),
        "claude-opus-5".into(),
    );

    let mut conn = fixture.connection().await?;
    let held = conn.begin_with("BEGIN IMMEDIATE").await?;
    let started = std::time::Instant::now();
    transport.stream(request()).await?;
    evidence
        .observe(/*position*/ 1, Ok(usage(/*value*/ 7)))
        .await?;
    assert!(started.elapsed() < HELD, "held up {:?}", started.elapsed());
    assert_eq!(sends.load(Ordering::SeqCst), 1);
    tokio::time::sleep(HELD.saturating_sub(started.elapsed())).await;
    held.commit().await?;
    assert!(
        fixture
            .db
            .flush_accounting(std::time::Duration::from_secs(30))
            .await
    );
    assert_eq!(fixture.attempts().await?.len(), 1);
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT json_extract(payload, '$.patch') FROM draft_accounting_observations",
    )
    .fetch_all(&mut conn)
    .await?;
    assert_eq!(rows, vec![r#"{"input":7}"#]);
    assert!(fixture.sampling.check().is_ok());
    assert!(logs_contain("records are written once it is free"));
    Ok(())
}

/// #287: with the clock behind the ledger checkpoint (58 s, 6 days), opening a
/// sampling failed and every request in the turn stopped before it was sent.
/// Now the sampling opens and the request is sent and recorded at the time the
/// clock read.
#[tokio::test]
async fn accounting_clock_behind_the_checkpoint_still_records_and_sends() -> anyhow::Result<()> {
    for behind in [58_000_i64, 6 * 86_400_000] {
        let fixture = Fixture::new().await?;
        let ahead = chrono::Utc::now().timestamp_millis() + behind;
        sqlx::query("UPDATE draft_accounting_retention_checkpoint SET completed_as_of_ms = ?")
            .bind(ahead)
            .execute(&mut fixture.connection().await?)
            .await?;
        let sampling = Sampling::start(
            fixture.db.clone(),
            fixture.owner,
            "skewed".into(),
            &AccountingMode::DirectAnthropic {
                scope: Uuid::new_v4(),
                approved_endpoint: ENDPOINT.into(),
            },
        )
        .await?;
        let sends = Arc::new(AtomicUsize::new(0));
        let evidence = ResponseEvidence::new(sampling.clone());
        AccountingTransport::new(
            Probe {
                sends: sends.clone(),
                fail_first: false,
            },
            Some(evidence.clone()),
            "claude-opus-5".into(),
        )
        .stream(request())
        .await?;
        evidence
            .observe(/*position*/ 1, Ok(usage(/*value*/ 7)))
            .await?;
        settle(&fixture.db).await;
        let attempts = fixture.attempts().await?;
        assert_eq!((attempts.len(), sends.load(Ordering::SeqCst)), (1, 1));
        assert!(i64::from(attempts[0].dispatched_at_ms) < ahead);
        assert!(!sampling.is_closed());
    }
    Ok(())
}

/// A clock at least 90 days behind the checkpoint (a clock that once ran far
/// ahead, then was corrected) cannot be recorded at all:
/// every attempt would already be past detail retention. It is a gap, never a
/// stopped request.
#[tokio::test]
async fn accounting_clock_far_behind_the_checkpoint_sends_unrecorded() -> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let ahead = chrono::Utc::now().timestamp_millis() + 100 * 86_400_000;
    sqlx::query("UPDATE draft_accounting_retention_checkpoint SET completed_as_of_ms = ?")
        .bind(ahead)
        .execute(&mut fixture.connection().await?)
        .await?;
    let sends = Arc::new(AtomicUsize::new(0));
    AccountingTransport::new(
        Probe {
            sends: sends.clone(),
            fail_first: false,
        },
        Some(ResponseEvidence::new(fixture.sampling.clone())),
        "claude-opus-5".into(),
    )
    .stream(request())
    .await?;
    assert_eq!(
        (
            fixture.attempts().await?.len(),
            sends.load(Ordering::SeqCst)
        ),
        (0, 1)
    );
    assert!(fixture.sampling.is_closed());
    Ok(())
}

/// The request is sent; the log records which step failed and why.
#[tokio::test]
#[tracing_test::traced_test]
async fn accounting_failure_logs_its_step_and_cause() -> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    sqlx::query("CREATE TRIGGER reject_admission BEFORE INSERT ON draft_accounting_attempts BEGIN SELECT RAISE(ABORT, 'fixture-admission'); END")
        .execute(&mut fixture.connection().await?).await?;
    let transport = AccountingTransport::new(
        Probe {
            sends: Arc::new(AtomicUsize::new(0)),
            fail_first: false,
        },
        Some(ResponseEvidence::new(fixture.sampling.clone())),
        "claude-opus-5".into(),
    );
    transport.stream(request()).await?;
    assert!(logs_contain("admit attempt"));
    assert!(logs_contain("fixture-admission"));
    assert!(logs_contain("request sent unrecorded"));
    // Not contention, so it was not retried.
    assert!(!logs_contain("retrying"));
    Ok(())
}

/// #403: the TUI's `/cost` names a reopen warm-up by this exact prefix (it
/// can't import the constant), so a change here must change it there too.
#[test]
fn reopen_prewarm_label_is_stable() {
    use crate::accounting::REOPEN_PREWARM_LABEL;
    use crate::accounting::prewarm_turn_label;
    use crate::accounting::reopen_prewarm_turn_label;
    assert_eq!(REOPEN_PREWARM_LABEL, "prewarm:reopen:");
    let label = reopen_prewarm_turn_label();
    assert!(label.starts_with("prewarm:reopen:"), "{label}");
    assert!(prewarm_turn_label().starts_with("prewarm:"));
    assert!(!prewarm_turn_label().starts_with(REOPEN_PREWARM_LABEL));
}

/// #824: `/cost` names a realtime WebSocket session by this label prefix
/// (the TUI's `REALTIME_SESSION_LABEL`). It stays a `realtime:` label, and a
/// WebRTC call's label is not one.
#[test]
fn realtime_session_label_is_stable() {
    use crate::accounting::REALTIME_SESSION_LABEL;
    use crate::accounting::realtime_session_turn_label;
    use crate::accounting::realtime_turn_label;
    assert_eq!(REALTIME_SESSION_LABEL, "realtime:ws:");
    assert_eq!(realtime_session_turn_label("sub"), "realtime:ws:sub");
    assert!(realtime_session_turn_label("sub").starts_with("realtime:"));
    assert!(!realtime_turn_label("sub").starts_with(REALTIME_SESSION_LABEL));
}
