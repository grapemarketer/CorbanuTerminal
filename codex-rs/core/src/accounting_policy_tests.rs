use super::*;
use anyhow::Result;
use codex_protocol::protocol::SessionSource;
use codex_state::SqliteConfig;
use codex_state::ThreadMetadataBuilder;
use codex_utils_absolute_path::AbsolutePathBuf;
use futures::poll;
use pretty_assertions::assert_eq;
use sqlx::Connection;
use std::panic::AssertUnwindSafe;
use std::panic::catch_unwind;
use std::time::Duration;

#[cfg(feature = "developer-accounting")]
#[tokio::test]
async fn accounting_admission_matrix_agrees_at_all_three_gates() -> Result<()> {
    use codex_login::CodexAuth;
    use codex_model_provider_info::ModelProviderInfo;
    use codex_model_provider_info::WireApi;
    let fixture = Fixture::new().await?;
    let api = CodexAuth::from_api_key("synthetic");
    let subscription =
        CodexAuth::from_external_chatgpt_tokens("header.e30.synthetic", "synthetic-account", None)?;
    let mut cells = 0;
    for id in [
        "anthropic",
        "openai",
        "claude-plan",
        "corbanu",
        "openrouter",
        "custom",
    ] {
        for wire in [WireApi::Responses, WireApi::Chat, WireApi::Anthropic] {
            for auth_kind in 0..5 {
                let auth = match auth_kind {
                    1 => Some(&api),
                    2 => Some(&subscription),
                    _ => None,
                };
                // `chat_completions_provider` is no longer an exclusion: a configured
                // routing preference names the provider that serves and bills the
                // request. It stays in the list as an ADMITTED shape so the cells
                // prove that, rather than silently dropping the case.
                for exclusion in [
                    None,
                    Some("aws"),
                    Some("chat_completions_provider"),
                    Some("query_params"),
                ] {
                    let mut provider = ModelProviderInfo::create_openai_provider(None);
                    provider.wire_api = wire;
                    if auth_kind == 3 {
                        provider.experimental_bearer_token = Some("synthetic-plan".into());
                    }
                    if auth_kind == 4 {
                        provider.auth = Some(serde_json::from_value(
                            serde_json::json!({"command":"never-executed-fixture"}),
                        )?);
                    }
                    if exclusion == Some("aws") {
                        provider.aws = Some(codex_model_provider_info::ModelProviderAwsAuthInfo {
                            profile: None,
                            region: None,
                        });
                    }
                    if exclusion == Some("chat_completions_provider") {
                        provider.chat_completions_provider = Some(serde_json::json!({}));
                    }
                    // The resolved URL carries a query string, and the pin now carries
                    // it too, so this shape collects. These cells prove the stored
                    // route equals the one the client builds for it.
                    if exclusion == Some("query_params") {
                        provider.query_params = Some(std::collections::HashMap::from([(
                            "api-version".to_string(),
                            "2025-04-01-preview".to_string(),
                        )]));
                    }
                    let selected = developer_accounting_mode(id, &provider);
                    let endpoint = provider
                        .to_api_provider(auth.map(CodexAuth::auth_mode))?
                        .base_url;
                    let bound = turn_mode(
                        &selected,
                        id,
                        &provider,
                        auth.map(CodexAuth::auth_mode),
                        &endpoint,
                    );
                    // Every provider shape now collects: signing, configured query
                    // parameters and a configured routing preference are all
                    // attributable. Only a request-level routing key the
                    // configuration did not ask for is refused, per request.
                    let admitted = true;
                    let reason = route_refusal(&provider);
                    assert_eq!(reason.is_none(), admitted, "{id}/{wire}/{exclusion:?}");
                    assert_eq!(!matches!(selected, AccountingMode::Disabled), admitted);
                    assert_eq!(collects(&bound, id, &provider, wire), admitted);
                    // A configured routing preference is attributable only when the
                    // body actually carries exactly it, so build the request the way
                    // this configuration would send it.
                    let mut request = chat_body();
                    request.provider = provider.chat_completions_provider.clone();
                    let eligible = match wire {
                        WireApi::Responses => {
                            responses::eligible(&provider, auth.map(CodexAuth::auth_mode))
                        }
                        WireApi::Chat => chat::eligible(&provider, auth, &request),
                        WireApi::Anthropic => route_refusal(&provider).is_none(),
                    };
                    assert_eq!(eligible, admitted, "{id}/{wire}/{exclusion:?}: {reason:?}");
                    // And a routing preference configuration did NOT ask for is never
                    // attributable, whatever the provider shape.
                    if wire == WireApi::Chat {
                        let mut foreign = chat_body();
                        foreign.provider = Some(serde_json::json!({"order": ["someone-else"]}));
                        assert!(
                            !chat::eligible(&provider, auth, &foreign),
                            "{id}/{exclusion:?}: an unconfigured routing preference must be refused"
                        );
                    }
                    if admitted {
                        let sample = Sampling::start(
                            fixture.db.clone(),
                            fixture.sampling.owner,
                            format!("matrix-{cells}"),
                            &bound,
                        )
                        .await?;
                        assert_eq!(sample.provider, id);
                        let path = match wire {
                            WireApi::Responses => "responses",
                            WireApi::Chat => "chat/completions",
                            WireApi::Anthropic => "messages",
                        };
                        // Compare against the client's own URL builder rather than the
                        // accounting helper, so this cannot pass by both sides using
                        // the same expression.
                        let requested = provider
                            .to_api_provider(auth.map(CodexAuth::auth_mode))?
                            .url_for_path(path);
                        assert_eq!(
                            super::canonical_route(&sample.endpoint),
                            super::canonical_route(&requested),
                            "the stored route must be the one the client requests"
                        );
                        assert!(
                            sample
                                .admit("fixture", "http://wrong.invalid")
                                .await
                                .is_err()
                        );
                    }
                    cells += 1;
                }
            }
        }
    }
    assert_eq!(cells, 360);
    assert!(serde_json::from_value::<WireApi>(serde_json::json!("unknown")).is_err());
    Ok(())
}

#[test]
fn accounting_chat_request_overrides_are_unattributable() -> Result<()> {
    use codex_model_provider_info::ModelProviderInfo;
    use codex_model_provider_info::WireApi;
    let mut provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
    provider.wire_api = WireApi::Chat;
    for field in ["provider", "provider_options", "plugins"] {
        let mut value =
            serde_json::json!({"model":"fixture","messages":[],"stream":true,"tools":[]});
        value[field] = if field == "plugins" {
            serde_json::json!([])
        } else {
            serde_json::json!({})
        };
        let http =
            codex_http_client::Request::new(http::Method::POST, ENDPOINT.into()).with_json(&value);
        assert_eq!(
            transport::request_refusal(
                &http, /*configured_routing*/ None, /*configured_routing_options*/ None,
                /*configured_plugins*/ None
            ),
            Some(field)
        );
        let mut request = chat_body();
        match field {
            "provider" => request.provider = Some(value[field].clone()),
            "provider_options" => request.provider_options = Some(value[field].clone()),
            "plugins" => request.plugins = Some(vec![]),
            _ => unreachable!(),
        }
        assert!(
            !chat::eligible(&provider, None, &request),
            "{field} changes serving attribution"
        );
    }
    Ok(())
}

/// The transport sees the body AFTER preparation, which for this client means
/// zstd-compressed bytes. Inspecting only plain JSON refused every real turn and
/// collected nothing, so cover both encodings and both answers here.
#[test]
fn accounting_prepared_bodies_are_inspected_not_refused() -> Result<()> {
    use codex_http_client::Request;
    use codex_http_client::RequestCompression;
    let ordinary = serde_json::json!({"model":"fixture","input":[],"stream":true});
    for compression in [RequestCompression::None, RequestCompression::Zstd] {
        let prepared = Request::new(http::Method::POST, ENDPOINT.into())
            .with_json(&ordinary)
            .with_compression(compression)
            .into_prepared()
            .map_err(anyhow::Error::msg)?;
        assert_eq!(
            transport::request_refusal(
                &prepared, /*configured_routing*/ None,
                /*configured_routing_options*/ None, /*configured_plugins*/ None
            ),
            None,
            "an ordinary prepared body must remain collectable under {compression:?}"
        );
        for field in ["provider", "provider_options", "plugins"] {
            let mut value = ordinary.clone();
            value[field] = if field == "plugins" {
                serde_json::json!([])
            } else {
                serde_json::json!({})
            };
            let prepared = Request::new(http::Method::POST, ENDPOINT.into())
                .with_json(&value)
                .with_compression(compression)
                .into_prepared()
                .map_err(anyhow::Error::msg)?;
            assert_eq!(
                transport::request_refusal(
                    &prepared, /*configured_routing*/ None,
                    /*configured_routing_options*/ None, /*configured_plugins*/ None
                ),
                Some(field),
                "{field} must stay unattributable under {compression:?}"
            );
        }
    }
    // Serialize a real request rather than hand-writing the JSON: the wire name is
    // `providerOptions`, and a hand-built snake_case body would pass a check that
    // the actual traffic walks straight past.
    for compression in [RequestCompression::None, RequestCompression::Zstd] {
        let mut request = chat_body();
        request.provider_options = Some(serde_json::json!({"gateway":{"only":["zai"]}}));
        let prepared = Request::new(http::Method::POST, ENDPOINT.into())
            .with_json(&request)
            .with_compression(compression)
            .into_prepared()
            .map_err(anyhow::Error::msg)?;
        assert_eq!(
            transport::request_refusal(
                &prepared, /*configured_routing*/ None,
                /*configured_routing_options*/ None, /*configured_plugins*/ None
            ),
            Some("providerOptions"),
            "a serialized gateway pin must be refused under {compression:?}"
        );
        let prepared = Request::new(http::Method::POST, ENDPOINT.into())
            .with_json(&chat_body())
            .with_compression(compression)
            .into_prepared()
            .map_err(anyhow::Error::msg)?;
        assert_eq!(
            transport::request_refusal(
                &prepared, /*configured_routing*/ None,
                /*configured_routing_options*/ None, /*configured_plugins*/ None
            ),
            None,
            "an ordinary serialized request must remain collectable under {compression:?}"
        );
    }
    // A configuration-emitted routing value is attributable; a different one is not.
    for (key, configured) in [
        ("provider", serde_json::json!({"order": ["anthropic"]})),
        (
            "providerOptions",
            serde_json::json!({"gateway": {"only": ["zai"]}}),
        ),
    ] {
        let mut body = ordinary.clone();
        body[key] = configured.clone();
        let prepared = Request::new(http::Method::POST, ENDPOINT.into())
            .with_json(&body)
            .with_compression(RequestCompression::Zstd)
            .into_prepared()
            .map_err(anyhow::Error::msg)?;
        let (routing, options) = if key == "provider" {
            (Some(&configured), None)
        } else {
            (None, Some(&configured))
        };
        assert_eq!(
            transport::request_refusal(
                &prepared, routing, options, /*configured_plugins*/ None
            ),
            None,
            "{key} exactly as configured must stay collectable"
        );
        let foreign = serde_json::json!({"order": ["someone-else"]});
        let (routing, options) = if key == "provider" {
            (Some(&foreign), None)
        } else {
            (None, Some(&foreign))
        };
        assert_eq!(
            transport::request_refusal(
                &prepared, routing, options, /*configured_plugins*/ None
            ),
            Some(key),
            "{key} that configuration did not ask for must be refused"
        );
    }
    let opaque = Request::new(http::Method::POST, ENDPOINT.into())
        .with_raw_body(vec![0x00, 0x01, 0x02, 0x03]);
    assert_eq!(
        transport::request_refusal(
            &opaque, /*configured_routing*/ None, /*configured_routing_options*/ None,
            /*configured_plugins*/ None
        ),
        Some("uninspectable request body"),
        "a body whose routing keys cannot be read must still be refused"
    );
    Ok(())
}

/// Authentication chooses which economics apply, and a route this client cannot
/// price is not the same thing as a plan. Pin all three outcomes at the real
/// default endpoint, so swapping the auth cannot silently keep per-token rates
/// and an unpriceable route cannot silently become subscription capacity.
#[test]
fn accounting_pricing_authority_follows_auth_mode_at_the_default_endpoint() {
    use codex_model_provider_info::ModelProviderInfo;
    use codex_model_provider_info::WireApi;
    use codex_protocol::auth::AuthMode;
    // Both built-in metered routes, because the Anthropic arm was dead code until
    // the predicate stopped rejecting a provider for declaring its own api-key
    // header, and nothing asserted the positive side of that arm.
    for (id, provider, endpoint) in [
        (
            "openai",
            ModelProviderInfo::create_openai_provider(/*base_url*/ None),
            "https://api.openai.com/v1",
        ),
        (
            "anthropic",
            ModelProviderInfo::create_anthropic_provider(),
            codex_model_provider_info::ANTHROPIC_BASE_URL,
        ),
    ] {
        let mode = crate::config::AccountingMode::Provider {
            scope: uuid::Uuid::new_v4(),
            provider_id: id.into(),
            wire_api: provider.wire_api,
            approved_endpoint: endpoint.into(),
            approved_query: None,
            pricing: PriceAuthority::Unavailable,
            basis_source: Default::default(),
            account: None,
        };
        let bound = super::turn_mode(&mode, id, &provider, Some(AuthMode::ApiKey), endpoint);
        let crate::config::AccountingMode::Provider {
            approved_query: None,
            pricing,
            ..
        } = bound
        else {
            panic!("{id} provider mode must survive rebinding");
        };
        assert_eq!(
            pricing,
            PriceAuthority::ApiKeyRates,
            "a metered API-key {id} route at its own default endpoint must be priced"
        );
        // The same credential pointed somewhere else states nothing at all: not
        // rates, and not a plan rate either.
        let elsewhere = super::turn_mode(
            &mode,
            id,
            &provider,
            Some(AuthMode::ApiKey),
            "https://relay.invalid/v1",
        );
        assert!(
            matches!(
                elsewhere,
                crate::config::AccountingMode::Provider {
                    pricing: PriceAuthority::Undeclared,
                    ..
                }
            ),
            "{id} off its own route is not declared"
        );
    }
    let provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
    assert_eq!(provider.wire_api, WireApi::Responses);
    let scope = uuid::Uuid::new_v4();
    let mode = crate::config::AccountingMode::Provider {
        scope,
        provider_id: "openai".into(),
        wire_api: WireApi::Responses,
        approved_endpoint: "https://api.openai.com/v1".into(),
        approved_query: None,
        pricing: PriceAuthority::Unavailable,
        basis_source: Default::default(),
        account: None,
    };
    // Each credential is checked at the route that credential actually resolves
    // to: a ChatGPT plan turn goes to the Codex route, an API key to the API one.
    for (auth, expected) in [
        (Some(AuthMode::ApiKey), PriceAuthority::ApiKeyRates),
        (Some(AuthMode::Chatgpt), PriceAuthority::PlanRate),
        (Some(AuthMode::ChatgptAuthTokens), PriceAuthority::PlanRate),
        // Request headers are another OpenAI account credential: no row.
        (Some(AuthMode::Headers), PriceAuthority::Undeclared),
        // No visible credential is not a subscription. Recording it as one would
        // book a turn this client cannot attribute as plan capacity; the table
        // declares nothing for it.
        (None, PriceAuthority::Undeclared),
        (Some(AuthMode::BedrockApiKey), PriceAuthority::Undeclared),
    ] {
        let endpoint = provider
            .to_api_provider(auth)
            .map(|api| api.base_url)
            .expect("openai route for this credential");
        let bound = super::turn_mode(&mode, "openai", &provider, auth, &endpoint);
        let crate::config::AccountingMode::Provider {
            approved_query: None,
            pricing,
            ..
        } = bound
        else {
            panic!("provider mode must survive rebinding for {auth:?}");
        };
        assert_eq!(
            pricing, expected,
            "authentication decides which economics apply ({auth:?})"
        );
    }
}

/// The client emits `providerOptions` for the Vercel gateway and `plugins` for
/// OpenRouter web search. Requiring them to be absent excluded those sessions
/// from collection on every turn, which is not "available on all providers".
#[test]
fn accounting_chat_collects_the_fields_the_client_itself_emits() {
    use codex_model_provider_info::ModelProviderInfo;
    use codex_model_provider_info::WireApi;
    let plain = {
        let mut provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
        provider.wire_api = WireApi::Chat;
        provider
    };
    let mut with_options = chat_body();
    with_options.provider_options = Some(serde_json::json!({"gateway": {"only": ["zai"]}}));
    let mut with_plugins = chat_body();
    with_plugins.plugins = Some(vec![serde_json::json!({"id": "web"})]);

    // An ordinary provider still refuses both: nothing explains the fields.
    assert!(!chat::eligible(&plain, None, &with_options));
    assert!(!chat::eligible(&plain, None, &with_plugins));

    let gateway = {
        let mut provider = ModelProviderInfo::create_openai_provider(Some(
            "https://ai-gateway.vercel.sh/v1".to_string(),
        ));
        provider.wire_api = WireApi::Chat;
        provider
    };
    assert!(gateway.is_vercel_gateway(), "fixture must be the gateway");
    assert!(
        chat::eligible(&gateway, None, &with_options),
        "the gateway's own vendor pin must not exclude the session"
    );

    let openrouter = {
        // `is_openrouter` keys off the provider NAME, so take the real builder
        // rather than an OpenAI provider pointed at OpenRouter's URL.
        let mut provider =
            codex_model_provider_info::built_in_model_providers(/*openai_base_url*/ None)
                .into_values()
                .find(ModelProviderInfo::is_openrouter)
                .expect("built-in OpenRouter provider");
        provider.wire_api = WireApi::Chat;
        provider
    };
    assert!(openrouter.is_openrouter(), "fixture must be openrouter");
    assert!(
        chat::eligible(&openrouter, None, &with_plugins),
        "an OpenRouter web-search session must not be excluded"
    );
}

/// "Available on all providers" should be checked against the product's real
/// catalogue, not six synthetic identities. Every built-in provider must select a
/// collecting mode at its own wire dialect; if a future provider is added with a
/// shape this code cannot attribute, this test is where that shows up.
#[cfg(feature = "developer-accounting")]
#[test]
fn accounting_every_built_in_provider_collects() {
    use codex_model_provider_info::built_in_model_providers;
    let mut checked = 0;
    let mut priced_providers: Vec<String> = Vec::new();
    for (id, provider) in built_in_model_providers(None) {
        let selected = developer_accounting_mode(&id, &provider);
        assert!(
            !matches!(selected, AccountingMode::Disabled),
            "{id} ({:?}) selects no accounting mode",
            provider.wire_api
        );
        // Bind at the provider's own resolved endpoint and under API-key auth, so
        // the pricing assertion below can actually fail: with no auth mode every
        // entry takes the plan side and proves nothing about rates.
        let endpoint = provider
            .to_api_provider(Some(codex_protocol::auth::AuthMode::ApiKey))
            .map(|api| api.base_url)
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
        let bound = turn_mode(
            &selected,
            &id,
            &provider,
            Some(codex_protocol::auth::AuthMode::ApiKey),
            &endpoint,
        );
        assert!(
            collects(&bound, &id, &provider, provider.wire_api),
            "{id} ({:?}) binds a mode that does not collect",
            provider.wire_api
        );
        // Economics follow the declared basis for the provider's own route and
        // the credential it is built to send (PF-60-S05): never the kind of
        // login. Per-token rates need a credential this client can attribute.
        if let AccountingMode::Provider { pricing, .. } = bound {
            let credential = codex_model_provider_info::BillingCredential::of(
                &provider,
                Some(codex_protocol::auth::AuthMode::ApiKey),
            );
            let declared = codex_model_provider_info::built_in_basis(&id, credential);
            let attributable = provider.aws.is_none()
                && provider.auth.is_none()
                && provider.experimental_bearer_token.is_none();
            use codex_model_provider_info::BillingBasis;
            let expected = match declared {
                Some(BillingBasis::Subscription) => PriceAuthority::PlanRate,
                Some(BillingBasis::Local) => PriceAuthority::Local,
                Some(BillingBasis::PayPerUse) if attributable => PriceAuthority::ApiKeyRates,
                Some(BillingBasis::PayPerUse) => PriceAuthority::Unavailable,
                None => PriceAuthority::Undeclared,
            };
            assert_eq!(pricing, expected, "{id} pricing authority at its own route");
            if expected == PriceAuthority::ApiKeyRates {
                priced_providers.push(id.clone());
            }
            // The basis is bound to the route: read through an endpoint no
            // table declares, the same provider is not declared - not rates,
            // and not a plan rate standing in for them. Bedrock's credentials
            // reach only Bedrock, whose route is regional: pay per use with no
            // attributable rate wherever it goes.
            let elsewhere = turn_mode(
                &selected,
                &id,
                &provider,
                Some(codex_protocol::auth::AuthMode::ApiKey),
                "https://relay.invalid/v1",
            );
            let expected_elsewhere = if id == codex_model_provider_info::AMAZON_BEDROCK_PROVIDER_ID
            {
                PriceAuthority::Unavailable
            } else {
                PriceAuthority::Undeclared
            };
            assert!(
                matches!(
                    elsewhere,
                    AccountingMode::Provider { pricing, .. } if pricing == expected_elsewhere
                ),
                "{id} must not carry its basis away from its own route"
            );
        }
        // And the per-turn dialect gate must admit it too, not just the selector.
        let admitted = match provider.wire_api {
            codex_model_provider_info::WireApi::Responses => responses::eligible(&provider, None),
            codex_model_provider_info::WireApi::Chat => {
                let mut request = chat_body();
                request.provider = provider.chat_completions_provider.clone();
                chat::eligible(&provider, None, &request)
            }
            codex_model_provider_info::WireApi::Anthropic => route_refusal(&provider).is_none(),
        };
        assert!(admitted, "{id} is selected but not admitted per turn");
        checked += 1;
    }
    // The catalogue is the product's real provider list; if it shrinks, this
    // test should be updated deliberately rather than silently covering less.
    // The catalogue has twenty-one entries today; assert the real number so a
    // silently shrinking catalogue is visible rather than tolerated.
    assert_eq!(checked, 21, "catalogue size changed");
    // Money used to exist on two providers. Name the ones that now carry rates,
    // so losing any of them is a visible change rather than a quiet one.
    priced_providers.sort();
    assert!(
        priced_providers.len() >= 13,
        "pricing authority shrank to {priced_providers:?}"
    );
    for required in ["anthropic", "openai", "openrouter", "vercel", "deepseek"] {
        assert!(
            priced_providers.iter().any(|id| id == required),
            "{required} lost pricing authority: {priced_providers:?}"
        );
    }
}

/// A compaction's turn identity must fit the store's bounded identity, or the
/// compaction of a long submission id would be unrecordable while its ordinary
/// turn recorded fine.
#[test]
fn accounting_compaction_label_stays_recordable() -> Result<()> {
    use codex_state::accounting::Attempt;
    for sub_id in [
        "short".to_string(),
        "x".repeat(120),
        "x".repeat(128),
        "x".repeat(400),
        format!("{}é", "y".repeat(126)),
    ] {
        let label = super::compaction_turn_label(&sub_id);
        assert!(label.starts_with("compact:"), "{label}");
        assert!(label.len() <= 128, "{} bytes", label.len());
        // Two submissions sharing a long prefix must not collapse into one turn.
        let sibling = super::compaction_turn_label(&format!("{sub_id}-sibling"));
        assert_ne!(
            label, sibling,
            "labels must stay distinct for distinct submissions"
        );
        let attempt = Attempt {
            attempt_id: uuid::Uuid::new_v4(),
            request_id: uuid::Uuid::new_v4(),
            thread_id: ThreadId::new(),
            turn: label.clone(),
            retry_of: None,
            provider: "fixture".into(),
            model: MODEL.into(),
            scope: uuid::Uuid::new_v4(),
            dialect: codex_state::accounting::Dialect::Inclusive,
            dispatched_at_ms: 1i64.try_into()?,
            account: None,
        };
        assert!(
            attempt.validate().is_ok(),
            "the store must accept the compaction label for {} bytes",
            sub_id.len()
        );
    }
    Ok(())
}

fn chat_body() -> codex_api::ChatCompletionsRequest {
    codex_api::ChatCompletionsRequest {
        model: "fixture".into(),
        messages: vec![],
        stream: true,
        tools: vec![],
        stream_options: None,
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

const MODEL: &str = "claude-opus-5";
const ENDPOINT: &str = "http://127.0.0.1:1/v1/messages";

/// A body that can re-route the serving provider must not be attributed, must not
/// kill the turn, and must not leave partial evidence. Nothing covered this
/// before, which is how an earlier version that rejected the turn's sampling and
/// then sent the request anyway passed every lane.
#[tokio::test]
async fn accounting_unattributable_request_is_served_without_evidence() -> Result<()> {
    use codex_http_client::HttpTransport;
    use codex_http_client::Request;
    use codex_http_client::RequestCompression;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    #[derive(Default, Clone)]
    struct Counting(Arc<AtomicUsize>);
    impl HttpTransport for Counting {
        async fn execute(
            &self,
            _req: Request,
        ) -> std::result::Result<codex_http_client::Response, codex_http_client::TransportError>
        {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(codex_http_client::TransportError::Build("stub".into()))
        }
        async fn stream(
            &self,
            _req: Request,
        ) -> std::result::Result<codex_http_client::StreamResponse, codex_http_client::TransportError>
        {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(codex_http_client::TransportError::Build("stub".into()))
        }
    }

    let fixture = Fixture::new().await?;
    let evidence = transport::ResponseEvidence::new(Arc::clone(&fixture.sampling));
    let inner = Counting::default();
    let sent = Arc::clone(&inner.0);
    let wrapper =
        transport::AccountingTransport::new(inner, Some(Arc::clone(&evidence)), MODEL.into());
    let mut body = serde_json::json!({"model": MODEL, "input": [], "stream": true});
    body["providerOptions"] = serde_json::json!({"gateway": {"only": ["zai"]}});
    let request = Request::new(http::Method::POST, ENDPOINT.into())
        .with_json(&body)
        .with_compression(RequestCompression::Zstd)
        .into_prepared()
        .map_err(anyhow::Error::msg)?;
    // The request reaches the real transport rather than being short-circuited.
    assert!(
        wrapper.stream(request).await.is_err(),
        "stub inner transport"
    );
    assert_eq!(
        sent.load(Ordering::SeqCst),
        1,
        "the turn's request was sent"
    );
    // The turn survives: rejecting the sampling here would abort it at the next check.
    fixture.sampling.check()?;
    // And nothing was recorded for a request we could not attribute.
    assert!(fixture.attempts().await?.is_empty());
    // A usage event on an excluded request records nothing instead of failing the
    // stream - on every dialect, because the three observers are three different
    // code shapes and a future edit could drop the guard from one of them.
    codex_api::ResponsesUsageObserver::observe(
        &*evidence,
        /*position*/ 0,
        Ok(codex_api::ResponsesUsagePatch::default()),
    )
    .await
    .map_err(|error| anyhow::anyhow!("excluded responses usage must not fail: {error}"))?;
    codex_api::ChatUsageObserver::observe(
        &*evidence,
        /*position*/ 0,
        Ok(codex_api::ChatUsagePatch::default()),
    )
    .await
    .map_err(|error| anyhow::anyhow!("excluded chat usage must not fail: {error}"))?;
    codex_api::AnthropicUsageObserver::observe(
        &*evidence,
        /*position*/ 0,
        Ok(codex_api::AnthropicUsagePatch::default()),
    )
    .await
    .map_err(|error| anyhow::anyhow!("excluded anthropic usage must not fail: {error}"))?;
    fixture.sampling.check()?;
    assert!(fixture.attempts().await?.is_empty());
    Ok(())
}

struct Fixture {
    home: tempfile::TempDir,
    db: Arc<StateRuntime>,
    sampling: Arc<Sampling>,
}

impl Fixture {
    async fn new() -> Result<Self> {
        let home = tempfile::tempdir()?;
        let db = StateRuntime::init(
            SqliteConfig::from_sqlite_home(AbsolutePathBuf::try_from(home.path())?),
            "anthropic".into(),
        )
        .await?;
        let owner = ThreadId::new();
        db.upsert_thread(
            &ThreadMetadataBuilder::new(
                owner,
                home.path().join("policy.jsonl"),
                chrono::Utc::now(),
                SessionSource::Cli,
            )
            .build("anthropic"),
        )
        .await?;
        let sampling = Sampling::start(db.clone(), owner, "policy-turn".into(), &mode()).await?;
        Ok(Self { home, db, sampling })
    }

    async fn connection(&self) -> Result<sqlx::SqliteConnection> {
        let pool = self
            .db
            .sqlite()
            .open_read_write_pool(&self.db.sqlite().state_db_path())
            .await?;
        let connection = pool.acquire().await?.detach();
        pool.close().await;
        Ok(connection)
    }

    async fn attempts(&self) -> Result<Vec<Attempt>> {
        let mut connection = self.connection().await?;
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT payload FROM draft_accounting_attempts ORDER BY rowid")
                .fetch_all(&mut connection)
                .await?;
        connection.close().await?;
        rows.into_iter()
            .map(|row| Ok(serde_json::from_str(&row)?))
            .collect()
    }

    async fn two_reopens(self, expected: Vec<Attempt>) -> Result<()> {
        // Release the sampling owner before shutting down its SQLite workers.
        let Self { home, db, sampling } = self;
        drop(sampling);
        db.close().await;
        drop(db);
        for _ in 0..2 {
            let db = StateRuntime::init(
                SqliteConfig::from_sqlite_home(AbsolutePathBuf::try_from(home.path())?),
                "anthropic".into(),
            )
            .await?;
            AccountingStore::open(&db, now()).await?;
            let pool = db
                .sqlite()
                .open_read_only_pool(&db.sqlite().state_db_path())
                .await?;
            let rows = sqlx::query_scalar::<_, String>(
                "SELECT payload FROM draft_accounting_attempts ORDER BY rowid",
            )
            .fetch_all(&pool)
            .await;
            pool.close().await;
            let actual: Vec<Attempt> = rows?
                .into_iter()
                .map(|row| serde_json::from_str(&row))
                .collect::<Result<_, _>>()?;
            assert_eq!(actual, expected);
            db.close().await;
            drop(db);
        }
        Ok(())
    }
}

fn mode() -> AccountingMode {
    AccountingMode::DirectAnthropic {
        scope: Uuid::new_v4(),
        approved_endpoint: "http://127.0.0.1:1/v1".into(),
    }
}

fn poison<T>(mutex: &Mutex<T>) {
    assert!(
        catch_unwind(AssertUnwindSafe(|| {
            let _guard = mutex.lock().unwrap();
            panic!("deliberate policy fixture poison");
        }))
        .is_err()
    );
}

/// The state DB's write lock held by another connection, as another Corbanu
/// process holds it; released by `release`.
async fn hold_write_lock(fixture: &Fixture) -> Result<sqlx::SqliteConnection> {
    let mut connection = fixture.connection().await?;
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut connection)
        .await?;
    Ok(connection)
}

async fn release(mut connection: sqlx::SqliteConnection) -> Result<()> {
    sqlx::query("ROLLBACK").execute(&mut connection).await?;
    connection.close().await?;
    Ok(())
}

/// Longest a record may hold a request up while the state DB is busy.
const BUSY_WAIT_BOUND: Duration = Duration::from_millis(1_500);

#[tokio::test]
async fn accounting_policy_busy_state_db_never_holds_a_request_up_and_loses_nothing() -> Result<()>
{
    let fixture = Fixture::new().await?;
    let lock = hold_write_lock(&fixture).await?;
    // OFF still fails; a valid start writes nothing and does not wait.
    assert!(
        Sampling::start(
            fixture.db.clone(),
            fixture.sampling.owner,
            "off".into(),
            &AccountingMode::Disabled,
        )
        .await
        .is_err()
    );
    let started = std::time::Instant::now();
    let fresh = Sampling::start(
        fixture.db.clone(),
        fixture.sampling.owner,
        "fresh".into(),
        &mode(),
    )
    .await?;
    assert!(started.elapsed() < BUSY_WAIT_BOUND);
    // Records are put off, not refused: the samplings stay open.
    let started = std::time::Instant::now();
    let first = fixture.sampling.admit(MODEL, ENDPOINT).await?;
    let usage = AnthropicUsagePatch {
        input_tokens: AnthropicTokenPresence::Number(7),
        ..Default::default()
    };
    fixture
        .sampling
        .observe(&first, Uuid::new_v4(), /*position*/ 1, usage)
        .await?;
    let second = fresh.admit(MODEL, ENDPOINT).await?;
    assert!(
        started.elapsed() < BUSY_WAIT_BOUND,
        "waited {:?}",
        started.elapsed()
    );
    fixture.sampling.check()?;
    fresh.check()?;
    assert!(fixture.attempts().await?.is_empty());
    release(lock).await?;
    assert!(fixture.db.flush_accounting(Duration::from_secs(30)).await);
    let observations: i64 = {
        let mut connection = fixture.connection().await?;
        let count = sqlx::query_scalar("SELECT count(*) FROM draft_accounting_observations")
            .fetch_one(&mut connection)
            .await?;
        connection.close().await?;
        count
    };
    assert_eq!(observations, 1);
    drop(fresh);
    fixture.two_reopens(vec![first, second]).await
}

/// A record the ledger writer refuses after its request stopped waiting is
/// still reported: by the turn that notices it, once.
#[tokio::test]
async fn accounting_policy_late_refusals_are_reported_once() -> Result<()> {
    let fixture = Fixture::new().await?;
    let owner = fixture.sampling.owner;
    let notices = SessionNotices::default();
    let ledger = (fixture.db.clone(), owner);
    let refuse = || async {
        // Recorded under this conversation, for another one: refused.
        let mut other = fixture.sampling.admit(MODEL, ENDPOINT).await?;
        other.thread_id = ThreadId::new();
        let refused = fixture
            .db
            .record_accounting(codex_state::accounting::LedgerWrite::Admit {
                owner,
                attempt: other,
                prices: Vec::new(),
            })
            .await;
        assert!(refused.is_err());
        anyhow::Ok(())
    };
    // One from before the session's first turn is not its own.
    refuse().await?;
    notices.count_refusals_from(&fixture.db, owner);
    assert!(!notices.refused_since(Some(&ledger)));
    refuse().await?;
    assert!(notices.refused_since(Some(&ledger)));
    assert!(!notices.refused_since(Some(&ledger)));
    assert!(!notices.refused_since(/*ledger*/ None));
    Ok(())
}

/// Cancelling a sampling while it waits for the ledger to open attaches
/// nothing.
#[tokio::test]
async fn accounting_policy_start_cancel_attaches_nothing() -> Result<()> {
    let fixture = Fixture::new().await?;
    let lock = hold_write_lock(&fixture).await?;
    let slot = Slot::default();
    let enabled = mode();
    let mut start = Box::pin(async {
        let sampling = Sampling::start(
            fixture.db.clone(),
            fixture.sampling.owner,
            "cancelled".into(),
            &enabled,
        )
        .await?;
        SamplingScope::attach(slot.clone(), Some(sampling))
    });
    let _ = poll!(&mut start);
    drop(start);
    assert!(read_slot(&slot)?.is_none());
    release(lock).await?;
    fixture.two_reopens(Vec::new()).await
}

#[tokio::test]
async fn accounting_policy_serial_retry_identity_and_time_sample_after_the_admission_lock()
-> Result<()> {
    let fixture = Fixture::new().await?;
    let admission = fixture.sampling.admission.acquire().await?;
    let mut first = Box::pin(fixture.sampling.admit(MODEL, ENDPOINT));
    let mut second = Box::pin(fixture.sampling.admit(MODEL, ENDPOINT));
    assert!(poll!(&mut first).is_pending());
    assert!(poll!(&mut second).is_pending());
    let queued_at = now();
    tokio::time::timeout(Duration::from_secs(1), async {
        while now() <= queued_at {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    let barrier_time = now();
    drop(admission);
    let (first, second) = tokio::try_join!(first, second)?;
    assert_eq!(first.retry_of, None);
    assert_eq!(second.retry_of, Some(first.attempt_id));
    assert_ne!(first.attempt_id, second.attempt_id);
    for attempt in [&first, &second] {
        assert_eq!(attempt.request_id, fixture.sampling.request);
        assert_eq!(attempt.thread_id, fixture.sampling.owner);
        assert_eq!(attempt.turn, "policy-turn");
        assert!(i64::from(attempt.dispatched_at_ms) >= barrier_time);
    }
    assert!(i64::from(second.dispatched_at_ms) >= i64::from(first.dispatched_at_ms));
    fixture.two_reopens(vec![first, second]).await
}

#[tokio::test]
async fn accounting_policy_cancel_in_operation_closes_the_sampling_and_keeps_the_record()
-> Result<()> {
    for operation in ["admit", "observe"] {
        let fixture = Fixture::new().await?;
        let attempt = fixture.sampling.admit(MODEL, ENDPOINT).await?;
        let lock = hold_write_lock(&fixture).await?;
        let sampling = &fixture.sampling;
        let mut pending = Box::pin(async {
            if operation == "admit" {
                sampling.admit(MODEL, ENDPOINT).await.map(|_| ())
            } else {
                sampling
                    .observe(
                        &attempt,
                        Uuid::new_v4(),
                        /*position*/ 1,
                        AnthropicUsagePatch {
                            input_tokens: AnthropicTokenPresence::Number(7),
                            ..Default::default()
                        },
                    )
                    .await
            }
        });
        // Handed over and waiting for the writer's first try.
        assert!(
            tokio::time::timeout(Duration::from_millis(300), &mut pending)
                .await
                .is_err()
        );
        drop(pending);
        assert!(fixture.sampling.check().is_err());
        assert!(fixture.sampling.admit(MODEL, ENDPOINT).await.is_err());
        release(lock).await?;
        assert!(fixture.db.flush_accounting(Duration::from_secs(30)).await);
        // The record handed over before the cancel is in the ledger.
        let mut connection = fixture.connection().await?;
        let (attempts, observations): (i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM draft_accounting_attempts),
                    (SELECT count(*) FROM draft_accounting_observations)",
        )
        .fetch_one(&mut connection)
        .await?;
        connection.close().await?;
        assert_eq!(
            (attempts, observations),
            if operation == "admit" { (2, 0) } else { (1, 1) }
        );
        let fresh = tokio::time::timeout(
            Duration::from_secs(10),
            Sampling::start(
                fixture.db.clone(),
                fixture.sampling.owner,
                "fresh".into(),
                &mode(),
            ),
        )
        .await??;
        let next = fresh.admit(MODEL, ENDPOINT).await?;
        assert_eq!(next.retry_of, None);
        assert_ne!(next.request_id, attempt.request_id);
        drop(fresh);
        let mut expected = fixture.attempts().await?;
        expected.retain(|row| row.attempt_id != next.attempt_id);
        expected.push(next);
        fixture.two_reopens(expected).await?;
    }
    Ok(())
}

#[tokio::test]
async fn accounting_policy_completion_guard_never_repairs_possible_committed_rows() -> Result<()> {
    let fixture = Fixture::new().await?;
    let attempt = fixture.sampling.admit(MODEL, ENDPOINT).await?;
    // Directly exercise the uncertain-completion guard with durable data.
    // This is not a claim of a process kill at an unobservable SQL instruction.
    drop(Completion {
        sampling: &fixture.sampling,
        complete: false,
    });
    assert!(fixture.sampling.check().is_err());
    assert!(fixture.sampling.admit(MODEL, ENDPOINT).await.is_err());
    fixture.two_reopens(vec![attempt]).await
}

#[tokio::test]
async fn accounting_policy_previous_poison_before_and_during_admission_is_sticky() -> Result<()> {
    for phase in ["before", "during"] {
        let fixture = Fixture::new().await?;
        let mut connection = fixture.connection().await?;
        if phase == "during" {
            sqlx::query("BEGIN IMMEDIATE")
                .execute(&mut connection)
                .await?;
        } else {
            poison(&fixture.sampling.previous);
        }
        let mut pending = Box::pin(fixture.sampling.admit(MODEL, ENDPOINT));
        if phase == "during" {
            assert!(poll!(&mut pending).is_pending());
            poison(&fixture.sampling.previous);
            sqlx::query("ROLLBACK").execute(&mut connection).await?;
        }
        assert!(pending.await.is_err());
        assert!(fixture.sampling.check().is_err());
        assert!(fixture.sampling.admit(MODEL, ENDPOINT).await.is_err());
        connection.close().await?;
        assert!(fixture.db.flush_accounting(Duration::from_secs(30)).await);
        let rows = fixture.attempts().await?;
        assert_eq!(rows.len(), usize::from(phase == "during"));
        if let Some(attempt) = rows.first() {
            assert_eq!(attempt.retry_of, None);
            assert_eq!(attempt.request_id, fixture.sampling.request);
        }
        fixture.two_reopens(rows).await?;
    }
    Ok(())
}

#[tokio::test]
async fn accounting_policy_slot_poison_closes_stale_and_incoming_and_serves_unrecorded()
-> Result<()> {
    for operation in ["read", "attach", "drop"] {
        let fixture = Fixture::new().await?;
        let incoming = Sampling::start(
            fixture.db.clone(),
            fixture.sampling.owner,
            "incoming".into(),
            &mode(),
        )
        .await?;
        let slot = Slot::default();
        let scope = SamplingScope::attach(slot.clone(), Some(fixture.sampling.clone()))?;
        poison(&slot);
        match operation {
            "read" => assert!(read_slot(&slot)?.is_none()),
            "attach" => {
                drop(SamplingScope::attach(slot.clone(), Some(incoming.clone()))?);
                assert!(incoming.check().is_err());
            }
            _ => {}
        }
        drop(scope);
        assert!(fixture.sampling.check().is_err());
        assert!(slot.is_poisoned());
        assert!(slot.lock().err().unwrap().into_inner().is_none());
        // A poisoned slot records nothing; its requests are served unrecorded.
        assert!(read_slot(&slot)?.is_none());
        drop(SamplingScope::attach(slot.clone(), /*sampling*/ None)?);
        assert!(fixture.attempts().await?.is_empty());
        fixture.db.close().await;
    }
    Ok(())
}

/// A path can bring query of its own - realtime call creation appends the pairs
/// that select its architecture - and those pairs are part of the route the
/// request is admitted against. A pin that dropped them refused every call it
/// was meant to record.
#[test]
fn a_pinned_route_carries_both_the_configured_query_and_the_path_s_own() {
    use super::canonical_route;
    use super::pinned_route;

    assert_eq!(
        pinned_route("https://example.com/v1", /*query*/ None, "responses"),
        "https://example.com/v1/responses"
    );
    assert_eq!(
        pinned_route("https://example.com/v1", Some("key=value"), "responses"),
        "https://example.com/v1/responses?key=value"
    );
    assert_eq!(
        pinned_route(
            "https://example.com/v1",
            /*query*/ None,
            "realtime/calls?intent=quicksilver&architecture=avas"
        ),
        "https://example.com/v1/realtime/calls?intent=quicksilver&architecture=avas"
    );
    // Both sets, and the comparison that matters does not depend on their order.
    assert_eq!(
        canonical_route(&pinned_route(
            "https://example.com/v1",
            Some("key=value"),
            "realtime/calls?intent=quicksilver"
        )),
        canonical_route("https://example.com/v1/realtime/calls?intent=quicksilver&key=value")
    );
}

/// A Claude Plan turn is paid for by the Claude subscription, and by nothing
/// else in the profile.
///
/// Found live: a session on `claude-opus-5-plan` recorded its tokens and no
/// money at all, and the reason was an OpenAI API key sitting in the same
/// profile for an unrelated provider. `provider_plan_login` excluded
/// `AuthMode::ApiKey`, so the Codex credential decided the economics of a route
/// it has nothing to do with, and the API-key arm then rejected the route for
/// carrying its own credential. Every Claude Plan turn on such a profile was
/// unpriceable, which is the same silent hole the catalogue work keeps closing.
///
/// The authority must be the same under every Codex auth mode, because none of
/// them is the credential paying for this route.
#[test]
fn claude_plan_states_the_plan_side_whatever_openai_credential_exists() {
    use codex_model_provider_info::CLAUDE_PLAN_PROVIDER_ID;
    use codex_model_provider_info::built_in_model_providers;
    use codex_protocol::auth::AuthMode;

    let providers = built_in_model_providers(/*openai_base_url*/ None);
    let provider = providers
        .get(CLAUDE_PLAN_PROVIDER_ID)
        .expect("built-in claude-plan provider")
        .clone();
    let endpoint = provider
        .to_api_provider(/*auth_mode*/ None)
        .expect("claude-plan route")
        .base_url;
    // Seeded literally rather than through `developer_accounting_mode`, which
    // lives behind the `developer-accounting` feature. What is under test -
    // `turn_mode` and `provider_plan_login` - is ordinary production code, so
    // gating this test on that feature would leave the live regression it
    // guards with no assertion in the default lane, which passes no crate
    // features at all.
    let selected = crate::config::AccountingMode::Provider {
        scope: uuid::Uuid::new_v4(),
        provider_id: CLAUDE_PLAN_PROVIDER_ID.into(),
        wire_api: provider.wire_api,
        approved_endpoint: endpoint.clone(),
        approved_query: super::canonical_query(&provider),
        pricing: PriceAuthority::Unavailable,
        basis_source: Default::default(),
        account: None,
    };

    for auth_mode in [
        None,
        Some(AuthMode::ApiKey),
        Some(AuthMode::Chatgpt),
        Some(AuthMode::Headers),
    ] {
        let bound = turn_mode(
            &selected,
            CLAUDE_PLAN_PROVIDER_ID,
            &provider,
            auth_mode,
            &endpoint,
        );
        assert!(
            matches!(
                bound,
                AccountingMode::Provider {
                    pricing: PriceAuthority::PlanRate,
                    ..
                }
            ),
            "claude-plan under {auth_mode:?} must state the plan side, got {bound:?}"
        );
    }

    // And it is still bound to its own route: pointed elsewhere it is not
    // declared, an API key in the profile notwithstanding.
    let elsewhere = turn_mode(
        &selected,
        CLAUDE_PLAN_PROVIDER_ID,
        &provider,
        Some(AuthMode::ApiKey),
        "https://relay.invalid/v1",
    );
    assert!(matches!(
        elsewhere,
        AccountingMode::Provider {
            pricing: PriceAuthority::Undeclared,
            ..
        }
    ));
}
