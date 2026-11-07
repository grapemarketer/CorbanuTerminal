use super::*;
use crate::accounting::transport::AccountingTransport;
use crate::accounting::transport::ResponseEvidence;
use codex_api::ResponsesTokenPresence;
use codex_api::ResponsesUsageObserver;
use codex_api::ResponsesUsagePatch;
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
    AccountingMode::DirectOpenAiResponsesHttp {
        scope: Uuid::new_v4(),
        approved_endpoint: ENDPOINT.into(),
    }
}
fn provider() -> ModelProviderInfo {
    ModelProviderInfo::create_openai_provider(Some(ENDPOINT.into()))
}
fn auth() -> CodexAuth {
    CodexAuth::from_api_key("synthetic-accounting-test")
}

struct Fixture {
    _home: tempfile::TempDir,
    deferred: Arc<DeferredResponsesSampling>,
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
            deferred: DeferredResponsesSampling::new(Arc::new(session), "turn".into(), mode()),
            db,
        })
    }
    async fn resolve(&self) -> anyhow::Result<Arc<Sampling>> {
        Ok(self
            .deferred
            .resolve(&provider(), Some(&auth()), &format!("{ENDPOINT}/responses"))
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
        let values: Vec<String> = sqlx::query_scalar(query).fetch_all(&pool).await?;
        pool.close().await;
        values
            .into_iter()
            .map(|value| Ok(serde_json::from_str(&value)?))
            .collect()
    }
}
const ATTEMPTS: &str = "SELECT payload FROM draft_accounting_attempts ORDER BY rowid";
const OBSERVATIONS: &str = "SELECT payload FROM draft_accounting_observations ORDER BY rowid";

#[tokio::test]
async fn accounting_responses_bootstrap_is_lazy_and_once() -> anyhow::Result<()> {
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
async fn accounting_responses_auth_route_eligibility() -> anyhow::Result<()> {
    assert!(legacy_eligible(&provider(), Some(AuthMode::ApiKey)));
    assert!(!legacy_eligible(&provider(), /*auth_mode*/ None));
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
            3 => provider.wire_api = WireApi::Chat,
            _ => unreachable!(),
        }
        assert!(!legacy_eligible(&provider, Some(AuthMode::ApiKey)));
    }
    let fixture = Fixture::new().await?;
    fixture.resolve().await?;
    assert!(
        fixture
            .deferred
            .resolve(
                &provider(),
                /*auth*/ None,
                &format!("{ENDPOINT}/responses")
            )
            .await?
            .is_none()
    );
    assert!(fixture.deferred.check().is_err());
    Ok(())
}

#[test]
fn issue_889_auth_modes_preserve_responses_eligibility() {
    let provider = provider();
    for auth_mode in [
        None,
        Some(AuthMode::ApiKey),
        Some(AuthMode::Chatgpt),
        Some(AuthMode::ChatgptAuthTokens),
        Some(AuthMode::Headers),
        Some(AuthMode::AgentIdentity),
        Some(AuthMode::PersonalAccessToken),
        Some(AuthMode::BedrockApiKey),
    ] {
        assert_eq!(
            legacy_eligible(&provider, auth_mode),
            auth_mode == Some(AuthMode::ApiKey),
            "{auth_mode:?}"
        );
        assert!(eligible(&provider, auth_mode), "{auth_mode:?}");
    }
}

#[tokio::test]
async fn issue_889_resolve_path_excludes_non_key_modes_before_and_after_admission()
-> anyhow::Result<()> {
    for auth_mode in [
        None,
        Some(AuthMode::Chatgpt),
        Some(AuthMode::ChatgptAuthTokens),
        Some(AuthMode::Headers),
        Some(AuthMode::AgentIdentity),
        Some(AuthMode::PersonalAccessToken),
        Some(AuthMode::BedrockApiKey),
    ] {
        let fixture = Fixture::new().await?;
        let provider = provider();
        let endpoint = format!("{ENDPOINT}/responses");
        assert!(
            fixture
                .deferred
                .resolve_path(&provider, auth_mode, &endpoint, "responses")
                .await?
                .is_none(),
            "{auth_mode:?}"
        );
        assert!(!fixture.deferred.sampling.initialized());
        assert!(fixture.deferred.check().is_ok());
        assert!(
            fixture
                .deferred
                .resolve_path(&provider, Some(AuthMode::ApiKey), &endpoint, "responses")
                .await?
                .is_some()
        );
        assert!(
            fixture
                .deferred
                .resolve_path(&provider, auth_mode, &endpoint, "responses")
                .await?
                .is_none(),
            "{auth_mode:?}"
        );
        assert!(fixture.deferred.check().is_err());
    }
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
        url: format!("{ENDPOINT}/responses"),
        headers: Default::default(),
        body: None,
        compression: Default::default(),
        timeout: None,
    }
}

#[tokio::test]
async fn accounting_responses_auth_and_guard_precede_admission() -> anyhow::Result<()> {
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
    let client = codex_api::ResponsesClient::new(transport, api_provider, Arc::new(Denied))
        .with_usage_observer(Some(evidence));
    assert!(
        client
            .stream(
                serde_json::json!({"model":"gpt-5.6-sol"}),
                Default::default(),
                codex_api::Compression::None,
                /*turn_state*/ None
            )
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
    // Existing guard construction rejects a dead owner before any client exists.
    assert!(
        crate::memory_stage_one::StageOneMemoryClient::new(
            std::sync::Weak::new(),
            futures::future::pending().boxed().shared(),
            fixture.deferred.session.thread_id,
            &provider(),
        )
        .await
        .is_err()
    );
    assert_eq!(sends.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn accounting_responses_bootstrap_failure_and_cancellation() -> anyhow::Result<()> {
    let (mut session, _) = crate::session::tests::make_session_and_context().await;
    session.services.state_db = None;
    let deferred =
        DeferredResponsesSampling::new(Arc::new(session), "missing-state".into(), mode());
    assert!(
        deferred
            .resolve(&provider(), Some(&auth()), &format!("{ENDPOINT}/responses"))
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
    Ok(())
}

#[tokio::test]
async fn accounting_responses_scope_cleanup_and_failure_latch() -> anyhow::Result<()> {
    let fixture = Fixture::new().await?;
    let slot = Slot::default();
    let scope = Scope::attach(slot.clone(), Some(fixture.deferred.clone()))?;
    assert!(read(&slot)?.is_some());
    let evidence = ResponseEvidence::new(fixture.resolve().await?);
    assert!(
        evidence
            .observe(/*position*/ 1, Err(codex_api::InvalidResponsesUsage))
            .await
            .is_ok()
    );
    assert!(fixture.deferred.check().is_err());
    drop(scope);
    assert!(read(&slot)?.is_none());
    Ok(())
}

#[tokio::test]
async fn accounting_responses_response_local_identity() -> anyhow::Result<()> {
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
                Ok(ResponsesUsagePatch {
                    input_tokens: ResponsesTokenPresence::Number(count),
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
    Ok(())
}

#[tokio::test]
async fn accounting_responses_role_overlay_preserves_binding() -> anyhow::Result<()> {
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
        Some(AuthMode::ApiKey)
    ));
    let endpoint = format!("{ENDPOINT}/responses");
    let sampling = fixture
        .deferred
        .resolve(&config.model_provider, Some(&auth()), &endpoint)
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
    let AccountingMode::DirectOpenAiResponsesHttp { scope, .. } = config.accounting else {
        unreachable!("fixture Responses mode")
    };
    assert_eq!(sampling.scope, scope);
    Ok(())
}

#[test]
fn accounting_responses_billed_charge_is_recorded_only_from_vercel() -> anyhow::Result<()> {
    let usage = |billed: Option<&str>| codex_api::ResponsesUsagePatch {
        input_tokens: codex_api::ResponsesTokenPresence::Number(10),
        billed_usd: billed.map(str::to_string),
        ..Default::default()
    };
    assert_eq!(
        patch(usage(Some("0.00116615")), "vercel")?.billed_usd,
        Some(codex_state::accounting::Decimal::try_from(
            "0.00116615".to_string()
        )?)
    );
    assert_eq!(patch(usage(None), "vercel")?.billed_usd, None);
    for provider in ["openai", "openrouter", "zai", "custom-vercel"] {
        assert_eq!(patch(usage(Some("0.5")), provider)?.billed_usd, None);
    }
    Ok(())
}

// An invalid reasoning count must not discard valid usage or the Vercel charge.
#[test]
fn issue814_responses_excess_reasoning_retains_valid_usage_and_vercel_charge() -> anyhow::Result<()>
{
    use codex_state::accounting::Decimal;
    use codex_state::accounting::Patch;
    use codex_state::accounting::Presence;
    let usage = ResponsesUsagePatch {
        input_tokens: ResponsesTokenPresence::Number(100),
        cached_tokens: ResponsesTokenPresence::Number(20),
        output_tokens: ResponsesTokenPresence::Number(10),
        reasoning_tokens: ResponsesTokenPresence::Number(11),
        total_tokens: ResponsesTokenPresence::Number(110),
        billed_usd: Some("0.012345".to_string()),
        ..Default::default()
    };
    assert_eq!(
        patch(usage, codex_model_provider_info::VERCEL_PROVIDER_ID)?,
        Patch {
            input: Presence::Number(100.try_into()?),
            read: Presence::Number(20.try_into()?),
            write: Presence::Missing,
            output: Presence::Number(10.try_into()?),
            reasoning: Presence::Missing,
            total: Presence::Number(110.try_into()?),
            billed_usd: Some(Decimal::try_from("0.012345".to_string())?),
            refused_status: None,
            session_ended_at_ms: None,
        }
    );
    Ok(())
}

#[test]
fn issue814_responses_valid_absent_and_null_reasoning_are_preserved() -> anyhow::Result<()> {
    use codex_state::accounting::Patch;
    use codex_state::accounting::Presence;
    for (reported, expected) in [
        (
            ResponsesTokenPresence::Number(9),
            Presence::Number(9.try_into()?),
        ),
        (
            ResponsesTokenPresence::Number(10),
            Presence::Number(10.try_into()?),
        ),
        (ResponsesTokenPresence::Missing, Presence::Missing),
        (ResponsesTokenPresence::Null, Presence::Null),
    ] {
        let usage = ResponsesUsagePatch {
            output_tokens: ResponsesTokenPresence::Number(10),
            reasoning_tokens: reported,
            ..Default::default()
        };
        assert_eq!(
            patch(usage, codex_model_provider_info::VERCEL_PROVIDER_ID)?,
            Patch {
                input: Presence::Missing,
                read: Presence::Missing,
                write: Presence::Missing,
                output: Presence::Number(10.try_into()?),
                reasoning: expected,
                total: Presence::Missing,
                billed_usd: None,
                refused_status: None,
                session_ended_at_ms: None,
            }
        );
    }
    Ok(())
}

#[test]
fn issue814_responses_does_not_guess_output_from_separate_reasoning_chunk() -> anyhow::Result<()> {
    use codex_state::accounting::Patch;
    use codex_state::accounting::Presence;
    let usage = ResponsesUsagePatch {
        reasoning_tokens: ResponsesTokenPresence::Number(11),
        ..Default::default()
    };
    assert_eq!(
        patch(usage, codex_model_provider_info::VERCEL_PROVIDER_ID)?,
        Patch {
            input: Presence::Missing,
            read: Presence::Missing,
            write: Presence::Missing,
            output: Presence::Missing,
            reasoning: Presence::Number(11.try_into()?),
            total: Presence::Missing,
            billed_usd: None,
            refused_status: None,
            session_ended_at_ms: None,
        }
    );
    Ok(())
}

#[test]
fn issue814_responses_keeps_existing_billed_charge_source_boundary() -> anyhow::Result<()> {
    let usage = ResponsesUsagePatch {
        output_tokens: ResponsesTokenPresence::Number(10),
        reasoning_tokens: ResponsesTokenPresence::Number(11),
        billed_usd: Some("0.012345".to_string()),
        ..Default::default()
    };
    let value = patch(usage, codex_model_provider_info::OPENAI_PROVIDER_ID)?;
    assert_eq!(
        (value.reasoning, value.billed_usd),
        (codex_state::accounting::Presence::Missing, None)
    );
    Ok(())
}

#[test]
fn issue814_chat_normalization_remains_unchanged() -> anyhow::Result<()> {
    use codex_api::ChatTokenPresence;
    use codex_api::ChatUsagePatch;
    use codex_state::accounting::Presence;
    for (reported, expected) in [
        (ChatTokenPresence::Number(11), Presence::Missing),
        (
            ChatTokenPresence::Number(10),
            Presence::Number(10.try_into()?),
        ),
        (ChatTokenPresence::Missing, Presence::Missing),
        (ChatTokenPresence::Null, Presence::Null),
    ] {
        let value = crate::accounting::chat::patch(
            ChatUsagePatch {
                output_tokens: ChatTokenPresence::Number(10),
                reasoning_tokens: reported,
                ..Default::default()
            },
            codex_model_provider_info::OPENAI_PROVIDER_ID,
            /*route*/ None,
        )?;
        assert_eq!(
            (value.output, value.reasoning),
            (Presence::Number(10.try_into()?), expected)
        );
    }
    Ok(())
}

#[tokio::test]
async fn issue814_replay_rejection_is_atomic_and_normalized_report_retains_charge()
-> anyhow::Result<()> {
    use codex_state::accounting::AccountingStore;
    use codex_state::accounting::Decimal;
    use codex_state::accounting::Dialect;
    use codex_state::accounting::Presence;
    // Reject the raw report atomically, then persist its normalized usage and charge.
    let fixture = Fixture::new().await?;
    let owner = fixture.deferred.session.thread_id;
    let store = AccountingStore::open(&fixture.db, /*as_of*/ 0).await?;
    let attempt = Attempt {
        attempt_id: Uuid::new_v4(),
        request_id: Uuid::new_v4(),
        thread_id: owner,
        turn: "issue814-synthetic".into(),
        retry_of: None,
        provider: "vercel".into(),
        model: "synthetic".into(),
        scope: Uuid::new_v4(),
        dialect: Dialect::Inclusive,
        dispatched_at_ms: 0.try_into()?,
        account: None,
    };
    store.admit(owner, &attempt, &[], /*as_of*/ 0).await?;
    let normalized = patch(
        ResponsesUsagePatch {
            input_tokens: ResponsesTokenPresence::Number(100),
            cached_tokens: ResponsesTokenPresence::Number(20),
            cache_write_tokens: ResponsesTokenPresence::Number(0),
            output_tokens: ResponsesTokenPresence::Number(10),
            reasoning_tokens: ResponsesTokenPresence::Number(11),
            total_tokens: ResponsesTokenPresence::Number(110),
            billed_usd: Some("0.012345".to_string()),
        },
        codex_model_provider_info::VERCEL_PROVIDER_ID,
    )?;
    let mut raw = normalized.clone();
    raw.reasoning = Presence::Number(11.try_into()?);
    let mut observation = Observation {
        revision: 1.try_into()?,
        source: Uuid::new_v4(),
        sequence: 1.try_into()?,
        patch: raw,
    };
    let error = store
        .observe(owner, &attempt, &[observation.clone()], /*as_of*/ 0)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("reasoning exceeds output"));
    assert_eq!(fixture.rows::<Observation>(OBSERVATIONS).await?, vec![]);
    observation.patch = normalized;
    let quote = store
        .observe(owner, &attempt, &[observation.clone()], /*as_of*/ 0)
        .await?;
    assert_eq!(
        quote.usage,
        codex_state::accounting::Usage {
            input: Some(100),
            noncached: Some(80),
            read: Some(20),
            write: Some(0),
            output: Some(10),
            reasoning: None,
            total: Some(110),
            billed_usd: Some(Decimal::try_from("0.012345".to_string())?),
            refused_status: None,
            session_ended_at_ms: None,
        }
    );
    assert_eq!(
        fixture.rows::<Observation>(OBSERVATIONS).await?,
        vec![observation]
    );
    Ok(())
}
