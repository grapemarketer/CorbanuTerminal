use super::*;
use crate::accounting::transport::AccountingTransport;
use crate::accounting::transport::ResponseEvidence;
use codex_api::ChatTokenPresence;
use codex_api::ChatUsageObserver;
use codex_api::ChatUsagePatch;
use codex_http_client::HttpTransport;
use codex_http_client::Request;
use codex_http_client::Response;
use codex_http_client::StreamResponse;
use codex_http_client::TransportError;
use codex_protocol::protocol::SessionSource;
use codex_state::SqliteConfig;
use codex_state::StateRuntime;
use codex_state::ThreadMetadataBuilder;
use codex_state::accounting::Attempt;
use codex_state::accounting::Observation;
use codex_utils_absolute_path::AbsolutePathBuf;
use futures::FutureExt;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use std::sync::atomic::AtomicUsize;

const ENDPOINT: &str = "http://127.0.0.1:1/v1";
fn mode() -> AccountingMode {
    AccountingMode::DirectOpenAiChat {
        scope: Uuid::new_v4(),
        approved_endpoint: ENDPOINT.into(),
    }
}
fn provider() -> ModelProviderInfo {
    ModelProviderInfo {
        wire_api: WireApi::Chat,
        ..ModelProviderInfo::create_openai_provider(Some(ENDPOINT.into()))
    }
}
fn auth() -> CodexAuth {
    CodexAuth::from_api_key("synthetic-accounting-test")
}

struct Fixture {
    _home: tempfile::TempDir,
    deferred: Arc<DeferredChatSampling>,
    db: Arc<StateRuntime>,
}
impl Fixture {
    async fn new() -> anyhow::Result<Self> {
        let home = tempfile::tempdir()?;
        let db = StateRuntime::init(
            SqliteConfig::from_sqlite_home(AbsolutePathBuf::try_from(home.path())?),
            "openai".into(),
        )
        .await?;
        let (mut session, _) = crate::session::tests::make_session_and_context().await;
        session.services.state_db = Some(db.clone());
        db.upsert_thread(
            &ThreadMetadataBuilder::new(
                session.thread_id,
                home.path().join("fixture.jsonl"),
                chrono::Utc::now(),
                SessionSource::Cli,
            )
            .build("openai"),
        )
        .await?;
        Ok(Self {
            _home: home,
            deferred: DeferredChatSampling::new(Arc::new(session), "turn".into(), mode()),
            db,
        })
    }
    async fn resolve(&self) -> anyhow::Result<Arc<Sampling>> {
        Ok(self
            .deferred
            .resolve(
                &provider(),
                Some(&auth()),
                &format!("{ENDPOINT}/chat/completions"),
                &body(),
            )
            .await?
            .unwrap())
    }
    async fn rows<T: serde::de::DeserializeOwned>(
        &self,
        query: &'static str,
    ) -> anyhow::Result<Vec<T>> {
        // A slow write is put off and written later (#865); wait for the writer.
        assert!(
            self.db
                .flush_accounting(std::time::Duration::from_secs(30))
                .await
        );
        let pool = self
            .db
            .sqlite()
            .open_read_only_pool(&self.db.sqlite().state_db_path())
            .await?;
        let values = sqlx::query_scalar::<_, String>(query)
            .fetch_all(&pool)
            .await;
        pool.close().await;
        values?
            .into_iter()
            .map(|value| Ok(serde_json::from_str(&value)?))
            .collect()
    }
}
const ATTEMPTS: &str = "SELECT payload FROM draft_accounting_attempts ORDER BY rowid";
const OBSERVATIONS: &str = "SELECT payload FROM draft_accounting_observations ORDER BY rowid";

#[tokio::test]
async fn accounting_chat_bootstrap_is_lazy_once_and_mode_local() -> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    assert!(!fixture.deferred.sampling.initialized());
    assert!(fixture.rows::<Attempt>(ATTEMPTS).await.is_err());
    let request = fixture.deferred.request;
    let first = fixture.resolve().await?;
    let second = fixture.resolve().await?;
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(first.request, request);
    assert_eq!(fixture.rows::<Attempt>(ATTEMPTS).await?, vec![]);
    Ok(())
}

#[tokio::test]
async fn accounting_chat_direct_auth_and_gateway_eligibility() -> anyhow::Result<()> {
    assert!(legacy_eligible(&provider(), Some(&auth()), &body()));
    assert!(!legacy_eligible(&provider(), /*auth*/ None, &body()));
    for kind in 0..4 {
        let mut provider = provider();
        match kind {
            0 => provider.experimental_bearer_token = Some("synthetic-override".into()),
            1 => {
                provider.http_headers = Some([("Authorization".into(), "synthetic".into())].into())
            }
            2 => {
                provider.env_http_headers =
                    Some([("authorization".into(), "UNREAD_FIXTURE".into())].into())
            }
            3 => provider.wire_api = WireApi::Responses,
            _ => unreachable!(),
        }
        assert!(!legacy_eligible(&provider, Some(&auth()), &body()));
    }
    for index in 0..11 {
        let mut route = provider();
        let mut request = body();
        match index {
            0 => route = ModelProviderInfo::create_pfterminal_plan_provider(),
            1 => route.name = "compatible".into(),
            2 => route.requires_openai_auth = false,
            3 => route.chat_completions_provider = Some(serde_json::json!({})),
            4 => request.provider = Some(serde_json::json!({})),
            5 => request.provider_options = Some(serde_json::json!({})),
            6 => request.plugins = Some(vec![]),
            7 => route.env_key = Some("ANTHROPIC_API_KEY".into()),
            8 => {
                route.auth = Some(serde_json::from_value(
                    serde_json::json!({"command":"never-execute-fixture"}),
                )?)
            }
            9 => route.http_headers = Some([("API-KEY".into(), "unread".into())].into()),
            10 => {
                route.env_http_headers = Some([("Api-Key".into(), "UNREAD_FIXTURE".into())].into())
            }
            _ => unreachable!(),
        }
        route.base_url = Some(ENDPOINT.into());
        assert!(!legacy_eligible(&route, Some(&auth()), &request));
        let fixture = Fixture::new().await?;
        assert!(
            fixture
                .deferred
                .resolve(
                    &route,
                    Some(&auth()),
                    &format!("{ENDPOINT}/chat/completions"),
                    &request
                )
                .await?
                .is_none()
        );
        assert!(!fixture.deferred.sampling.initialized());
    }
    let fixture = Fixture::new().await?;
    fixture.resolve().await?;
    // Eligibility lost mid-turn closes collection; the request is still served.
    assert!(
        fixture
            .deferred
            .resolve(
                &provider(),
                /*auth*/ None,
                &format!("{ENDPOINT}/chat/completions"),
                &body()
            )
            .await?
            .is_none()
    );
    assert!(fixture.deferred.check().is_err());
    Ok(())
}

struct Probe(Arc<AtomicUsize>);
impl HttpTransport for Probe {
    async fn execute(&self, _: Request) -> Result<Response, TransportError> {
        unreachable!()
    }
    async fn stream(&self, _: Request) -> Result<StreamResponse, TransportError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(StreamResponse {
            status: http::StatusCode::OK,
            headers: Default::default(),
            bytes: futures::stream::empty().boxed(),
        })
    }
}
struct Denied;
impl codex_api::AuthProvider for Denied {
    fn add_auth_headers(&self, _: &mut http::HeaderMap) {}
    fn apply_auth(&self, _: Request) -> codex_api::AuthProviderFuture<'_> {
        Box::pin(async {
            tokio::task::yield_now().await;
            Err(codex_api::AuthError::Build("synthetic denial".into()))
        })
    }
}
fn request() -> Request {
    Request {
        method: http::Method::POST,
        url: format!("{ENDPOINT}/chat/completions"),
        headers: Default::default(),
        body: None,
        compression: Default::default(),
        timeout: None,
    }
}

// Uses the debug-only stage-one binding fixture.
#[cfg(debug_assertions)]
#[tokio::test]
async fn accounting_chat_auth_and_guard_before_admission() -> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let sends = Arc::new(AtomicUsize::new(0));
    let evidence = ResponseEvidence::new(fixture.resolve().await?);
    let transport = crate::memory_stage_one::StageOneGuardedTransport::new(
        AccountingTransport::new(
            Probe(sends.clone()),
            Some(evidence.clone()),
            "gpt-5.6-sol".into(),
        ),
        /*binding*/ None,
    );
    let api_provider = provider().to_api_provider(Some(codex_protocol::auth::AuthMode::ApiKey))?;
    let client = codex_api::ChatCompletionsClient::new(transport, api_provider, Arc::new(Denied))
        .with_usage_observer(Some(evidence.clone()));
    assert!(
        client
            .stream_request(body(), Default::default())
            .await
            .is_err()
    );
    assert_eq!(
        (
            sends.load(Ordering::SeqCst),
            fixture.rows::<Attempt>(ATTEMPTS).await?.len()
        ),
        (0, 0)
    );
    let (mut owner, _) = crate::session::tests::make_session_and_context().await;
    owner.services.agent_control = owner
        .services
        .agent_control
        .clone()
        .with_effective_security_policy(
            codex_security_policy::SecurityLevel::Permissive,
            owner.thread_id,
            /*inherits_from_spawn_parent*/ false,
        )?;
    let owner = Arc::new(owner);
    let memory = crate::memory_stage_one::StageOneMemoryClient::new(
        Arc::downgrade(&owner),
        futures::future::pending().boxed().shared(),
        owner.thread_id,
        &owner.provider().await,
    )
    .await?;
    let guard = crate::memory_stage_one::StageOneGuardedTransport::new(
        AccountingTransport::new(Probe(sends.clone()), Some(evidence), "gpt-5.6-sol".into()),
        Some(memory.binding_for_fixture(owner.thread_id)?),
    );
    // The actual guard checks the live owner before invoking admitted stream().
    let controller = owner
        .services
        .agent_control
        .trusted_security_controller()
        .unwrap();
    let change = controller.confirm_level_change(
        codex_security_policy::SecurityLevel::Moderate,
        codex_security_policy::RevocationState::new(),
    )?;
    controller.apply_confirmed_change(change)?;
    assert!(guard.stream(request()).await.is_err());
    assert_eq!(
        (
            sends.load(Ordering::SeqCst),
            fixture.rows::<Attempt>(ATTEMPTS).await?.len()
        ),
        (0, 0)
    );
    Ok(())
}

// Uses the debug-only stage-one binding fixture.
#[cfg(debug_assertions)]
#[tokio::test]
async fn accounting_chat_borrowed_binding_denies_existing_and_future_clones() -> anyhow::Result<()>
{
    let (mut owner, _) = crate::session::tests::make_session_and_context().await;
    owner.services.agent_control = owner
        .services
        .agent_control
        .clone()
        .with_effective_security_policy(
            codex_security_policy::SecurityLevel::Permissive,
            owner.thread_id,
            /*inherits_from_spawn_parent*/ false,
        )?;
    let owner = Arc::new(owner);
    let memory = crate::memory_stage_one::StageOneMemoryClient::new(
        Arc::downgrade(&owner),
        futures::future::pending().boxed().shared(),
        owner.thread_id,
        &owner.provider().await,
    )
    .await?;
    let client = owner.services.model_client();
    let existing = client.as_ref().clone();
    let borrowed = client.as_ref();
    borrowed.with_stage_one_memory_binding(memory.binding_for_fixture(owner.thread_id)?)?;
    let future = borrowed.clone();
    let controller = owner
        .services
        .agent_control
        .trusted_security_controller()
        .unwrap();
    let change = controller.confirm_level_change(
        codex_security_policy::SecurityLevel::Moderate,
        codex_security_policy::RevocationState::new(),
    )?;
    controller.apply_confirmed_change(change)?;
    let sends = Arc::new(AtomicUsize::new(0));
    for clone in [&existing, &future] {
        let transport = crate::memory_stage_one::StageOneGuardedTransport::new(
            Probe(sends.clone()),
            clone.stage_one_memory_binding.get().cloned(),
        );
        assert!(
            transport.stream(request()).await.is_err(),
            "binding installed through a borrowed client must deny dispatch on every clone"
        );
    }
    assert_eq!(sends.load(Ordering::SeqCst), 0);
    assert!(
        borrowed
            .with_stage_one_memory_binding(memory.binding_for_fixture(owner.thread_id)?)
            .is_err()
    );
    Ok(())
}

// Uses the debug-only stage-one binding fixture.
#[cfg(debug_assertions)]
#[tokio::test]
async fn accounting_chat_frame_guard_denies_provider_change_before_client_publication()
-> anyhow::Result<()> {
    use crate::memory_stage_one::StageOneMemoryBinding;
    use crate::memory_stage_one::StageOneMemoryClient;
    use crate::memory_stage_one::StageOneMemoryDenial;
    use crate::session::session::Session;
    use crate::session::session::SessionSettingsUpdate;
    use codex_security_policy::SecurityLevel;
    use std::sync::Mutex;
    use std::sync::OnceLock;
    use std::sync::Weak;

    type Observation = (bool, Result<(), StageOneMemoryDenial>);
    #[derive(Default)]
    struct FrameAtConfigChange {
        owner: OnceLock<Weak<Session>>,
        binding: OnceLock<Arc<StageOneMemoryBinding>>,
        observations: Mutex<Vec<Observation>>,
    }
    impl codex_extension_api::ConfigContributor<crate::config::Config> for FrameAtConfigChange {
        fn on_config_changed(
            &self,
            _: &codex_extension_api::ExtensionData,
            _: &codex_extension_api::ExtensionData,
            previous: &crate::config::Config,
            next: &crate::config::Config,
        ) {
            assert_ne!(previous.model_provider, next.model_provider);
            let owner = self.owner.get().unwrap().upgrade().unwrap();
            // Each lock is polled once, outside tokio's cooperative budget: once
            // the task has spent its budget, a free tokio lock reports pending,
            // which a loaded Windows runner hit (#1855). A held lock still does.
            let configured = tokio::task::unconstrained(owner.provider())
                .now_or_never()
                .expect("configuration callback must run outside the state lock");
            // This real callback runs after configuration publication, before the old
            // implementation replaces its ModelClient. Keep state locked during the
            // frame check, as a concurrent session operation can do in that window.
            let _locked = tokio::task::unconstrained(owner.lock_state_for_accounting_fixture())
                .now_or_never()
                .expect("configuration callback must run outside the state lock");
            self.observations.lock().unwrap().push((
                configured == next.model_provider,
                self.binding.get().unwrap().check_stream(),
            ));
        }
    }

    let mut observed = Vec::new();
    for path in ["settings", "turn"] {
        let (mut owner, _) = crate::session::tests::make_session_and_context().await;
        owner.services.agent_control = owner
            .services
            .agent_control
            .clone()
            .with_effective_security_policy(
                SecurityLevel::Permissive,
                owner.thread_id,
                /*inherits_from_spawn_parent*/ false,
            )?;
        let probe = Arc::new(FrameAtConfigChange::default());
        let mut extensions = codex_extension_api::ExtensionRegistryBuilder::new();
        extensions.config_contributor(probe.clone());
        owner.services.extensions = Arc::new(extensions.build());
        let owner = Arc::new(owner);
        probe.owner.set(Arc::downgrade(&owner)).unwrap();
        let memory = StageOneMemoryClient::new(
            Arc::downgrade(&owner),
            futures::future::pending().boxed().shared(),
            owner.thread_id,
            &owner.provider().await,
        )
        .await?;
        probe
            .binding
            .set(memory.binding_for_fixture(owner.thread_id)?)
            .unwrap();
        probe.binding.get().unwrap().check_stream()?;
        let updates = SessionSettingsUpdate {
            model_provider: Some("anthropic".into()),
            ..Default::default()
        };
        match path {
            "settings" => owner.update_settings(updates).await?,
            "turn" => {
                owner
                    .new_turn_with_sub_id("provider-change".into(), updates)
                    .await?;
            }
            _ => unreachable!(),
        }
        observed.push((path, probe.observations.lock().unwrap().clone()));
    }
    assert_eq!(
        observed,
        vec![
            (
                "settings",
                vec![(true, Err(StageOneMemoryDenial::ProviderChanged))]
            ),
            (
                "turn",
                vec![(true, Err(StageOneMemoryDenial::ProviderChanged))]
            ),
        ],
        "each frame must reject the prior provider as soon as changed configuration is observable"
    );
    Ok(())
}

#[tokio::test]
async fn accounting_chat_bootstrap_cancel_scope_and_latch() -> anyhow::Result<()> {
    let (mut session, _) = crate::session::tests::make_session_and_context().await;
    session.services.state_db = None;
    let deferred = DeferredChatSampling::new(Arc::new(session), "missing-state".into(), mode());
    // No state database: the request goes unrecorded rather than failing.
    assert!(
        deferred
            .resolve(
                &provider(),
                Some(&auth()),
                &format!("{ENDPOINT}/chat/completions"),
                &body()
            )
            .await?
            .is_none()
    );
    assert!(deferred.check().is_err());
    // Opening a sampling writes nothing, so another connection holding the
    // state DB's write lock does not hold it up.
    let fixture = Fixture::new().await?;
    let mut lock = crate::accounting::tests_support::hold_write_lock(&fixture.db).await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), fixture.resolve()).await??;
    fixture.deferred.check()?;
    assert!(fixture.deferred.sampling.initialized());
    sqlx::query("ROLLBACK").execute(&mut *lock).await?;
    fixture.db.close().await;
    let fixture = Fixture::new().await?;
    let slot = Slot::default();
    let scope = Scope::attach(slot.clone(), Some(fixture.deferred.clone()))?;
    assert!(read(&slot)?.is_some());
    let evidence = ResponseEvidence::new(fixture.resolve().await?);
    // Invalid usage closes collection without failing the stream.
    assert!(
        evidence
            .observe(/*position*/ 1, Err(codex_api::InvalidChatUsage))
            .await
            .is_ok()
    );
    assert!(fixture.deferred.check().is_err());
    drop(scope);
    assert!(read(&slot)?.is_none());
    fixture.db.close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_chat_response_local_attempt_identity() -> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let sampling = fixture.resolve().await?;
    let sends = Arc::new(AtomicUsize::new(0));
    let old = ResponseEvidence::new(sampling.clone());
    let new = ResponseEvidence::new(sampling);
    for evidence in [&old, &new] {
        AccountingTransport::new(
            Probe(sends.clone()),
            Some(evidence.clone()),
            "gpt-5.6-sol".into(),
        )
        .stream(request())
        .await?;
    }
    for (evidence, count) in [(&new, 7), (&old, 3)] {
        evidence
            .observe(
                /*position*/ 1,
                Ok(ChatUsagePatch {
                    input_tokens: ChatTokenPresence::Number(count),
                    ..Default::default()
                }),
            )
            .await?;
    }
    let attempts = fixture.rows::<Attempt>(ATTEMPTS).await?;
    let observations = fixture.rows::<Observation>(OBSERVATIONS).await?;
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[1].retry_of, Some(attempts[0].attempt_id));
    assert_eq!(attempts[0].request_id, fixture.deferred.request);
    assert_eq!(attempts[1].request_id, fixture.deferred.request);
    assert_ne!(observations[0].source, observations[1].source);
    assert_eq!(sends.load(Ordering::SeqCst), 2);
    let pool = fixture
        .db
        .sqlite()
        .open_read_only_pool(&fixture.db.sqlite().state_db_path())
        .await?;
    let bound: Vec<String> =
        sqlx::query_scalar("SELECT attempt_id FROM draft_accounting_observations ORDER BY rowid")
            .fetch_all(&pool)
            .await?;
    pool.close().await;
    assert_eq!(
        bound,
        vec![
            attempts[1].attempt_id.to_string(),
            attempts[0].attempt_id.to_string()
        ]
    );
    assert_eq!(
        observations[0].patch.input,
        codex_state::accounting::Presence::Number(7.try_into()?)
    );
    assert_eq!(
        observations[1].patch.input,
        codex_state::accounting::Presence::Number(3.try_into()?)
    );
    Ok(())
}

#[tokio::test]
async fn accounting_chat_role_inheritance_reserved_id_parity() -> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let mut config = crate::config::test_config().await;
    config.model_provider_id = "openai".into();
    config.model = Some("gpt-5.6-sol".into());
    config.model_provider = provider();
    config.accounting = fixture.deferred.mode.clone();
    let role = config.codex_home.join("responses-role.toml");
    std::fs::create_dir_all(config.codex_home.as_path())?;
    std::fs::write(
        &role,
        "[model_providers.openai]\nname = \"fixture role\"\nrequest_max_retries = 0\nstream_idle_timeout_ms = 9876\nsupports_websockets = true\n",
    )?;
    config.agent_roles.insert(
        "fixture".into(),
        crate::config::AgentRoleConfig {
            config_file: Some(role.to_path_buf()),
            description: None,
            nickname_candidates: None,
        },
    );
    std::fs::write(
        &role,
        "developer_instructions = \"synthetic instruction role\"\n",
    )?;
    crate::agent::role::apply_role_to_config(&mut config, Some("fixture"))
        .await
        .map_err(anyhow::Error::msg)?;
    assert_eq!(config.model_provider, provider());
    assert_eq!(config.accounting, fixture.deferred.mode);
    std::fs::write(&role, "[model_providers.openai]\nname = \"fixture role\"\n")?;
    let parse_error =
        toml::from_str::<codex_config::config_toml::ConfigToml>(&std::fs::read_to_string(&role)?)
            .unwrap_err()
            .to_string();
    assert!(parse_error.contains("reserved built-in provider IDs: `openai`"));
    assert!(
        crate::agent::role::apply_role_to_config(&mut config, Some("fixture"))
            .await
            .is_err()
    );
    assert_eq!(config.model_provider, provider());
    assert_eq!(config.model_provider_id, "openai");
    assert_eq!(config.accounting, fixture.deferred.mode);
    assert!(legacy_eligible(
        &config.model_provider,
        Some(&auth()),
        &body()
    ));
    let endpoint = format!("{ENDPOINT}/chat/completions");
    let sampling = fixture
        .deferred
        .resolve(&config.model_provider, Some(&auth()), &endpoint, &body())
        .await?
        .expect("built-in API-key Responses binding");
    assert_eq!(sampling.endpoint, endpoint);
    assert_eq!(sampling.provider, "openai");
    assert_eq!(
        sampling.dialect,
        codex_state::accounting::Dialect::Inclusive
    );
    assert_eq!(sampling.request, fixture.deferred.request);
    assert_eq!(sampling.owner, fixture.deferred.session.thread_id);
    let AccountingMode::DirectOpenAiChat { scope, .. } = config.accounting else {
        unreachable!("fixture Responses mode")
    };
    assert_eq!(sampling.scope, scope);
    Ok(())
}

fn body() -> codex_api::ChatCompletionsRequest {
    codex_api::ChatCompletionsRequest {
        model: "gpt-5.6-sol".into(),
        messages: vec![],
        stream: true,
        stream_options: None,
        tools: vec![],
        tool_choice: None,
        parallel_tool_calls: None,
        prompt_cache_key: None,
        response_format: None,
        emit_usage: None,
        enable_thinking: None,
        reasoning_effort: None,
        reasoning: None,
        provider: None,
        plugins: None,
        provider_options: None,
    }
}

struct Mutate(&'static str);
impl codex_api::AuthProvider for Mutate {
    fn add_auth_headers(&self, _: &mut http::HeaderMap) {}
    fn apply_auth(&self, mut request: Request) -> codex_api::AuthProviderFuture<'_> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            request.url = self.0.into();
            Ok(request)
        })
    }
}

#[tokio::test]
async fn accounting_chat_exact_final_endpoint_binding() -> anyhow::Result<()> {
    for endpoint in [
        "https://127.0.0.1:1/v1/chat/completions",
        "http://localhost:1/v1/chat/completions",
        "http://127.0.0.1:2/v1/chat/completions",
        "http://127.0.0.1:1/v1/responses",
        "http://127.0.0.1:1/v1/chat/completions?q=1",
        "http://user@127.0.0.1:1/v1/chat/completions",
        "http://127.0.0.1:1/v1/chat/completions#fragment",
    ] {
        let fixture = Fixture::new().await?;
        let sends = Arc::new(AtomicUsize::new(0));
        let transport = AccountingTransport::new(
            Probe(sends.clone()),
            Some(ResponseEvidence::new(fixture.resolve().await?)),
            "gpt-5.6-sol".into(),
        );
        let api = codex_api::ChatCompletionsClient::new(
            transport,
            provider().to_api_provider(Some(codex_protocol::auth::AuthMode::ApiKey))?,
            Arc::new(Mutate(endpoint)),
        );
        // A final route other than the approved one is sent, but not recorded.
        assert!(api.stream_request(body(), Default::default()).await.is_ok());
        assert_eq!(
            (
                sends.load(Ordering::SeqCst),
                fixture.rows::<Attempt>(ATTEMPTS).await?.len()
            ),
            (1, 0)
        );
        assert!(fixture.deferred.check().is_err());
    }
    Ok(())
}

#[test]
fn accounting_chat_cache_writes_are_read_only_where_their_meaning_is_known() -> anyhow::Result<()> {
    use codex_state::accounting::Presence;
    let usage = |write| ChatUsagePatch {
        input_tokens: ChatTokenPresence::Number(8499),
        cached_tokens: ChatTokenPresence::Number(1152),
        cache_write_tokens: write,
        output_tokens: ChatTokenPresence::Number(29),
        ..Default::default()
    };
    // OpenRouter omits the count for models with no cache-write charge.
    assert_eq!(
        patch(
            usage(ChatTokenPresence::Missing),
            "openrouter",
            /*route*/ None
        )?
        .write,
        Presence::Number(0.try_into()?)
    );
    // A reported count is kept there, and an explicit null stays null.
    assert_eq!(
        patch(
            usage(ChatTokenPresence::Number(300)),
            "openrouter",
            /*route*/ None
        )?
        .write,
        Presence::Number(300.try_into()?)
    );
    assert_eq!(
        patch(
            usage(ChatTokenPresence::Null),
            "openrouter",
            /*route*/ None
        )?
        .write,
        Presence::Null
    );
    // OpenAI reports writes as a subset of the prompt (#361): kept as reported,
    // and an absent count stays unknown.
    for (write, expected) in [
        (
            ChatTokenPresence::Number(300),
            Presence::Number(300.try_into()?),
        ),
        (ChatTokenPresence::Null, Presence::Null),
        (ChatTokenPresence::Missing, Presence::Missing),
    ] {
        assert_eq!(
            patch(usage(write), "openai", /*route*/ None)?.write,
            expected
        );
    }
    // Every other route leaves the count unknown, reported or not: DeepSeek
    // bills written tokens as input, and a disjoint report would fail replay.
    for provider in [
        "deepseek",
        "baseten",
        "zai",
        "custom-openrouter",
        "openai-custom",
    ] {
        for write in [
            ChatTokenPresence::Missing,
            ChatTokenPresence::Null,
            ChatTokenPresence::Number(300),
        ] {
            assert_eq!(
                patch(usage(write), provider, /*route*/ None)?.write,
                Presence::Missing
            );
        }
    }
    Ok(())
}

#[test]
fn accounting_chat_billed_charge_is_recorded_only_from_openrouter() -> anyhow::Result<()> {
    let usage = |billed: Option<&str>| ChatUsagePatch {
        input_tokens: ChatTokenPresence::Number(10),
        billed_usd: billed.map(str::to_string),
        ..Default::default()
    };
    let stated = codex_state::accounting::Decimal::try_from("0.0123312".to_string())?;
    assert_eq!(
        patch(usage(Some("0.0123312")), "openrouter", /*route*/ None)?.billed_usd,
        Some(stated)
    );
    assert_eq!(
        patch(usage(None), "openrouter", /*route*/ None)?.billed_usd,
        None
    );
    // Beyond the exact type's 18 decimal places: dropped, never rounded, and
    // the token counts still record.
    let fine = patch(
        usage(Some("0.0000000000000000001")),
        "openrouter",
        /*route*/ None,
    )?;
    assert_eq!(fine.billed_usd, None);
    assert_eq!(
        fine.input,
        codex_state::accounting::Presence::Number(10.try_into()?)
    );
    assert_eq!(
        patch(
            usage(Some("0.000018")),
            "pfterminal-plan",
            /*route*/ None
        )?
        .billed_usd,
        Some(codex_state::accounting::Decimal::try_from(
            "0.000018".to_string()
        )?)
    );
    for provider in ["deepseek", "zai", "custom-openrouter"] {
        assert_eq!(
            patch(usage(Some("0.5")), provider, /*route*/ None)?.billed_usd,
            None
        );
    }
    Ok(())
}

/// #368: Moonshot's Kimi Open Platform documents `cache_write_tokens` as the
/// written part of the prompt, apart from the cached part, so the count is
/// kept on that route whatever the provider is named, and nowhere else.
#[test]
fn accounting_chat_cache_writes_are_read_on_the_moonshot_route() -> anyhow::Result<()> {
    use crate::config::PricedRoute;
    use codex_state::accounting::Presence;
    let usage = |write| ChatUsagePatch {
        input_tokens: ChatTokenPresence::Number(8499),
        cached_tokens: ChatTokenPresence::Number(1152),
        cache_write_tokens: write,
        output_tokens: ChatTokenPresence::Number(29),
        ..Default::default()
    };
    for provider in ["kimi", "my-moonshot"] {
        for (write, expected) in [
            (
                ChatTokenPresence::Number(7168),
                Presence::Number(7168.try_into()?),
            ),
            (ChatTokenPresence::Null, Presence::Null),
            // Moonshot leaves out a count that is zero (#368).
            (ChatTokenPresence::Missing, Presence::Number(0.try_into()?)),
        ] {
            assert_eq!(
                patch(
                    usage(write),
                    provider,
                    Some(PricedRoute::MoonshotOpenPlatform)
                )?
                .write,
                expected
            );
        }
        // The same provider somewhere else keeps the count unknown.
        assert_eq!(
            patch(
                usage(ChatTokenPresence::Number(7168)),
                provider,
                /*route*/ None
            )?
            .write,
            Presence::Missing
        );
    }
    Ok(())
}

/// #368: on the Moonshot route a cache count the API left out of a stated
/// prompt is zero, a read missing from the prompt details is taken from
/// `usage.cached_tokens`, and a null stays unknown. Nowhere else does a
/// missing count become zero or `usage.cached_tokens` count.
#[test]
fn accounting_chat_moonshot_counts_left_out_are_zero() -> anyhow::Result<()> {
    use crate::config::PricedRoute;
    use codex_state::accounting::Presence;
    let usage = |input, cached, usage_cached, write| ChatUsagePatch {
        input_tokens: input,
        cached_tokens: cached,
        usage_cached_tokens: usage_cached,
        cache_write_tokens: write,
        output_tokens: ChatTokenPresence::Number(11),
        ..Default::default()
    };
    let n = ChatTokenPresence::Number;
    let missing = ChatTokenPresence::Missing;
    let null = ChatTokenPresence::Null;
    let moonshot = Some(PricedRoute::MoonshotOpenPlatform);
    let zero = Presence::Number(0.try_into()?);
    let cases = [
        // (input, details read, usage read, write) -> (read, write)
        ((n(100), missing, missing, missing), (zero, zero)),
        (
            (n(100), missing, n(40), missing),
            (Presence::Number(40.try_into()?), zero),
        ),
        (
            (n(100), n(30), n(40), n(5)),
            (
                Presence::Number(30.try_into()?),
                Presence::Number(5.try_into()?),
            ),
        ),
        (
            (n(100), null, n(40), null),
            (Presence::Null, Presence::Null),
        ),
        ((n(100), missing, null, missing), (Presence::Null, zero)),
        (
            (missing, missing, missing, missing),
            (Presence::Missing, Presence::Missing),
        ),
    ];
    // Whatever the provider is named, the route decides.
    for provider in [
        "kimi",
        codex_model_provider_info::OPENAI_PROVIDER_ID,
        codex_model_provider_info::OPENROUTER_PROVIDER_ID,
    ] {
        for ((input, cached, usage_cached, write), (read, written)) in cases {
            let patch = patch(
                usage(input, cached, usage_cached, write),
                provider,
                moonshot,
            )?;
            assert_eq!((patch.read, patch.write), (read, written), "{provider}");
        }
    }
    // Off the route `usage.cached_tokens` never counts and a missing read stays
    // unknown; only OpenRouter's own write rule makes a missing write zero.
    for (provider, written) in [
        ("kimi", Presence::Missing),
        (
            codex_model_provider_info::OPENAI_PROVIDER_ID,
            Presence::Missing,
        ),
        (codex_model_provider_info::OPENROUTER_PROVIDER_ID, zero),
    ] {
        let patch = patch(
            usage(n(100), missing, n(40), missing),
            provider,
            /*route*/ None,
        )?;
        assert_eq!(
            (patch.read, patch.write),
            (Presence::Missing, written),
            "{provider}"
        );
    }
    Ok(())
}

/// #786 (Travis, 2026-10-14, option a): a reasoning count above the output
/// count it is part of is recorded as not reported, and the rest of the
/// report - the charge included - is kept, so replay accepts the record.
#[test]
fn accounting_chat_reasoning_above_output_is_not_reported() -> anyhow::Result<()> {
    use codex_state::accounting::Presence;
    let usage = |reasoning, output| ChatUsagePatch {
        input_tokens: ChatTokenPresence::Number(9098),
        output_tokens: output,
        reasoning_tokens: reasoning,
        total_tokens: ChatTokenPresence::Number(9134),
        billed_usd: Some("0.00274188".to_string()),
        ..Default::default()
    };
    // The real MiniMax M3 report: 37 reasoning tokens of 36 output.
    let report = patch(
        usage(ChatTokenPresence::Number(37), ChatTokenPresence::Number(36)),
        "openrouter",
        /*route*/ None,
    )?;
    assert_eq!(report.reasoning, Presence::Missing);
    assert_eq!(report.output, Presence::Number(36.try_into()?));
    assert_eq!(report.input, Presence::Number(9098.try_into()?));
    assert_eq!(report.total, Presence::Number(9134.try_into()?));
    assert_eq!(
        report.billed_usd,
        Some(codex_state::accounting::Decimal::try_from(
            "0.00274188".to_string()
        )?)
    );
    // A count within the output, equal included, is kept as reported; so is
    // a reasoning count with no output count to compare it with.
    for (reasoning, output, expected) in [
        (36, Some(36), Presence::Number(36.try_into()?)),
        (5, Some(36), Presence::Number(5.try_into()?)),
        (37, None, Presence::Number(37.try_into()?)),
    ] {
        let output = output.map_or(ChatTokenPresence::Missing, ChatTokenPresence::Number);
        assert_eq!(
            patch(
                usage(ChatTokenPresence::Number(reasoning), output),
                "openrouter",
                /*route*/ None
            )?
            .reasoning,
            expected
        );
    }
    Ok(())
}
