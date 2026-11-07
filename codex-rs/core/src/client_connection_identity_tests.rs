use super::super::ConnectionIdentity;
use super::super::ModelClient;
use super::test_model_client_with_provider;
use super::test_model_info;
use super::test_responses_metadata_for_client;
use super::test_session_telemetry;
use crate::Prompt;
use crate::test_support::TestCodexResponsesRequestKind;
use codex_api::AuthProvider;
use codex_api::ResponseEvent;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::auth::AgentIdentityAuthPolicy;
use codex_model_provider::BearerAuthProvider;
use codex_model_provider_info::WireApi;
use codex_model_provider_info::create_oss_provider_with_base_url;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::protocol::SessionSource;
use codex_rollout_trace::InferenceTraceContext;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::start_websocket_server;
use core_test_support::skip_if_no_network;
use futures::StreamExt;
use http::HeaderMap;
use http::HeaderValue;
use pretty_assertions::assert_eq;

struct HeaderAuth(HeaderMap);

impl AuthProvider for HeaderAuth {
    fn add_auth_headers(&self, headers: &mut HeaderMap) {
        headers.extend(self.0.clone());
    }
}

#[test]
fn api_only_identity_follows_resolved_credentials_without_exposing_them() {
    let old = BearerAuthProvider::new("synthetic-old-token".to_string());
    let same = BearerAuthProvider::new("synthetic-old-token".to_string());
    let new = BearerAuthProvider::new("synthetic-new-token".to_string());
    let anonymous = BearerAuthProvider::default();
    let identity = ConnectionIdentity::of(/*auth*/ None, &HeaderMap::new(), &old);

    assert_eq!(
        identity,
        ConnectionIdentity::of(/*auth*/ None, &HeaderMap::new(), &same)
    );
    assert_ne!(
        identity,
        ConnectionIdentity::of(/*auth*/ None, &HeaderMap::new(), &new)
    );
    assert_ne!(
        identity,
        ConnectionIdentity::of(/*auth*/ None, &HeaderMap::new(), &anonymous)
    );
    assert_eq!(format!("{identity:?}"), "ConnectionIdentity(..)");
}

#[test]
fn api_only_identity_canonicalizes_header_names_but_preserves_value_bytes_and_order() {
    let mut headers = HeaderMap::new();
    headers.append("x-api-key", HeaderValue::from_static("synthetic-first"));
    headers.append("x-api-key", HeaderValue::from_static("synthetic-second"));
    headers.insert("x-account", HeaderValue::from_static("test-account"));
    let identity = ConnectionIdentity::of(
        /*auth*/ None,
        &HeaderMap::new(),
        &HeaderAuth(headers.clone()),
    );

    let mut reordered = HeaderMap::new();
    reordered.insert("x-account", HeaderValue::from_static("test-account"));
    reordered.append("x-api-key", HeaderValue::from_static("synthetic-first"));
    reordered.append("x-api-key", HeaderValue::from_static("synthetic-second"));
    assert_eq!(
        identity,
        ConnectionIdentity::of(
            /*auth*/ None,
            &HeaderMap::new(),
            &HeaderAuth(reordered)
        )
    );

    headers.remove("x-api-key");
    headers.append("x-api-key", HeaderValue::from_static("synthetic-second"));
    headers.append("x-api-key", HeaderValue::from_static("synthetic-first"));
    assert_ne!(
        identity,
        ConnectionIdentity::of(
            /*auth*/ None,
            &HeaderMap::new(),
            &HeaderAuth(headers.clone())
        )
    );
    let reversed = ConnectionIdentity::of(
        /*auth*/ None,
        &HeaderMap::new(),
        &HeaderAuth(headers.clone()),
    );
    headers.append(
        "x-api-key",
        HeaderValue::from_bytes(b"synthetic-\xff").expect("header"),
    );
    assert_ne!(
        reversed,
        ConnectionIdentity::of(/*auth*/ None, &HeaderMap::new(), &HeaderAuth(headers))
    );
}

#[test]
fn known_chatgpt_identity_keeps_the_connection_across_bearer_refreshes() {
    let auth = CodexAuth::create_dummy_chatgpt_auth_for_testing();
    let old = BearerAuthProvider::new("synthetic-old-token".to_string());
    let refreshed = BearerAuthProvider::new("synthetic-refreshed-token".to_string());

    assert_eq!(
        ConnectionIdentity::of(Some(&auth), &HeaderMap::new(), &old),
        ConnectionIdentity::of(Some(&auth), &HeaderMap::new(), &refreshed)
    );
}

#[test]
fn configured_header_identity_follows_values_and_removal_without_exposing_them() {
    let anonymous = BearerAuthProvider::default();
    for name in ["authorization", "x-api-key", "x-tenant"] {
        let mut headers = HeaderMap::new();
        headers.insert(name, HeaderValue::from_static("synthetic-old-value"));
        let identity = ConnectionIdentity::of(/*auth*/ None, &headers, &anonymous);
        assert_eq!(
            identity,
            ConnectionIdentity::of(/*auth*/ None, &headers.clone(), &anonymous)
        );
        assert_eq!(format!("{identity:?}"), "ConnectionIdentity(..)");

        headers.insert(name, HeaderValue::from_static("synthetic-new-value"));
        assert_ne!(
            identity,
            ConnectionIdentity::of(/*auth*/ None, &headers, &anonymous)
        );
        headers.remove(name);
        assert_ne!(
            identity,
            ConnectionIdentity::of(/*auth*/ None, &headers, &anonymous)
        );
    }
}

#[test]
fn configured_header_identity_uses_resolved_auth_precedence() {
    let old = BearerAuthProvider::new("synthetic-old-token".to_string());
    let new = BearerAuthProvider::new("synthetic-new-token".to_string());
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_static("synthetic-overridden-one"),
    );
    headers.insert("x-tenant", HeaderValue::from_static("synthetic-tenant-one"));
    let identity = ConnectionIdentity::of(/*auth*/ None, &headers, &old);

    headers.insert(
        "authorization",
        HeaderValue::from_static("synthetic-overridden-two"),
    );
    assert_eq!(
        identity,
        ConnectionIdentity::of(/*auth*/ None, &headers, &old)
    );
    assert_ne!(
        identity,
        ConnectionIdentity::of(/*auth*/ None, &headers, &new)
    );

    headers.insert("x-tenant", HeaderValue::from_static("synthetic-tenant-two"));
    assert_ne!(
        identity,
        ConnectionIdentity::of(/*auth*/ None, &headers, &old)
    );
}

#[test]
fn configured_header_identity_preserves_duplicate_order_and_raw_bytes() {
    let anonymous = BearerAuthProvider::default();
    let mut headers = HeaderMap::new();
    headers.append("x-api-key", HeaderValue::from_static("synthetic-first"));
    headers.append("x-api-key", HeaderValue::from_static("synthetic-second"));
    headers.insert("x-tenant", HeaderValue::from_static("synthetic-tenant"));
    let identity = ConnectionIdentity::of(/*auth*/ None, &headers, &anonymous);
    let mut reordered = HeaderMap::new();
    reordered.insert("x-tenant", HeaderValue::from_static("synthetic-tenant"));
    reordered.append("x-api-key", HeaderValue::from_static("synthetic-first"));
    reordered.append("x-api-key", HeaderValue::from_static("synthetic-second"));
    assert_eq!(
        identity,
        ConnectionIdentity::of(/*auth*/ None, &reordered, &anonymous)
    );

    headers.remove("x-api-key");
    headers.append("x-api-key", HeaderValue::from_static("synthetic-second"));
    headers.append("x-api-key", HeaderValue::from_static("synthetic-first"));
    assert_ne!(
        identity,
        ConnectionIdentity::of(/*auth*/ None, &headers, &anonymous)
    );
    let reversed = ConnectionIdentity::of(/*auth*/ None, &headers, &anonymous);
    headers.append(
        "x-api-key",
        HeaderValue::from_bytes(b"synthetic-\xff").expect("header"),
    );
    assert_ne!(
        reversed,
        ConnectionIdentity::of(/*auth*/ None, &headers, &anonymous)
    );
}

/// Exercise the real preconnect and stream paths with resolved experimental
/// bearer credentials and no CodexAuth. Keep the cached socket while supplying
/// the next resolved client setup, as a credential rotation does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_only_rotation_reconnects_a_prewarmed_socket_and_keeps_same_key_reuse() {
    skip_if_no_network!();
    let server = start_websocket_server(vec![
        vec![
            vec![ev_response_created("old-1"), ev_completed("old-1")],
            vec![ev_response_created("old-2"), ev_completed("old-2")],
            // A broken implementation can finish instead of timing out;
            // the handshake and connection assertions below then fail.
            vec![ev_response_created("stale-3"), ev_completed("stale-3")],
        ],
        vec![vec![ev_response_created("new-3"), ev_completed("new-3")]],
    ])
    .await;
    let thread_id = ThreadId::new();
    let client_with_token = |token: &str| {
        let mut provider =
            create_oss_provider_with_base_url(&format!("{}/v1", server.uri()), WireApi::Responses);
        provider.supports_websockets = true;
        provider.experimental_bearer_token = Some(token.to_string());
        test_model_client_with_provider(thread_id, SessionSource::Cli, provider)
    };
    let mut session = client_with_token("synthetic-old-token").new_session();
    let telemetry = test_session_telemetry();
    let model = test_model_info();
    let metadata = test_responses_metadata_for_client(
        &session.client,
        /*turn_id*/ None,
        "identity-test".to_string(),
        /*parent_thread_id*/ None,
        TestCodexResponsesRequestKind::WebsocketConnection,
    );
    assert!(
        session
            .client
            .current_client_setup()
            .await
            .expect("setup")
            .auth
            .is_none()
    );
    session
        .preconnect_websocket(&telemetry, &metadata)
        .await
        .expect("preconnect");

    let mut completions = Vec::new();
    for (turn, token) in [
        ("one", "synthetic-old-token"),
        ("two", "synthetic-old-token"),
        ("three", "synthetic-new-token"),
    ] {
        session.client = client_with_token(token);
        let metadata = test_responses_metadata_for_client(
            &session.client,
            Some(turn),
            "identity-test".to_string(),
            /*parent_thread_id*/ None,
            TestCodexResponsesRequestKind::Turn,
        );
        let mut stream = session
            .stream(
                &Prompt::default(),
                &model,
                &telemetry,
                /*effort*/ None,
                ReasoningSummary::None,
                /*service_tier*/ None,
                &metadata,
                &InferenceTraceContext::disabled(),
            )
            .await
            .expect("stream");
        while let Some(event) = stream.next().await {
            if let ResponseEvent::Completed { response_id, .. } = event.expect("event") {
                completions.push(response_id);
                break;
            }
        }
    }
    assert_eq!(completions, vec!["old-1", "old-2", "new-3"]);
    assert_eq!(
        server
            .handshakes()
            .iter()
            .map(|handshake| handshake.header("authorization"))
            .collect::<Vec<_>>(),
        vec![
            Some("Bearer synthetic-old-token".to_string()),
            Some("Bearer synthetic-new-token".to_string()),
        ]
    );
    assert_eq!(
        server
            .connections()
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        vec![2, 1]
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_header_rotation_reconnects_prewarmed_socket_and_keeps_same_value_reuse() {
    skip_if_no_network!();
    for header_name in ["authorization", "x-api-key"] {
        let server = start_websocket_server(vec![
            vec![
                vec![ev_response_created("old-1"), ev_completed("old-1")],
                vec![ev_response_created("old-2"), ev_completed("old-2")],
                // A stale connection completes so the assertions fail without a timeout.
                vec![ev_response_created("stale-3"), ev_completed("stale-3")],
            ],
            vec![vec![ev_response_created("new-3"), ev_completed("new-3")]],
        ])
        .await;
        let thread_id = ThreadId::new();
        let client_with_header = |value: &str| {
            let mut provider = create_oss_provider_with_base_url(
                &format!("{}/v1", server.uri()),
                WireApi::Responses,
            );
            provider.supports_websockets = true;
            provider.http_headers = Some([(header_name.to_string(), value.to_string())].into());
            test_model_client_with_provider(thread_id, SessionSource::Cli, provider)
        };
        let mut session = client_with_header("synthetic-old-value").new_session();
        let telemetry = test_session_telemetry();
        let model = test_model_info();
        let metadata = test_responses_metadata_for_client(
            &session.client,
            /*turn_id*/ None,
            "configured-identity-test".to_string(),
            /*parent_thread_id*/ None,
            TestCodexResponsesRequestKind::WebsocketConnection,
        );
        let setup = session.client.current_client_setup().await.expect("setup");
        assert!(setup.auth.is_none());
        assert!(setup.api_auth.to_auth_headers().is_empty());
        assert_eq!(
            setup.api_provider.headers.get(header_name),
            Some(&HeaderValue::from_static("synthetic-old-value"))
        );
        session
            .preconnect_websocket(&telemetry, &metadata)
            .await
            .expect("preconnect");

        let mut completions = Vec::new();
        for (turn, value) in [
            ("one", "synthetic-old-value"),
            ("two", "synthetic-old-value"),
            ("three", "synthetic-new-value"),
        ] {
            session.client = client_with_header(value);
            let metadata = test_responses_metadata_for_client(
                &session.client,
                Some(turn),
                "configured-identity-test".to_string(),
                /*parent_thread_id*/ None,
                TestCodexResponsesRequestKind::Turn,
            );
            let mut stream = session
                .stream(
                    &Prompt::default(),
                    &model,
                    &telemetry,
                    /*effort*/ None,
                    ReasoningSummary::None,
                    /*service_tier*/ None,
                    &metadata,
                    &InferenceTraceContext::disabled(),
                )
                .await
                .expect("stream");
            while let Some(event) = stream.next().await {
                if let ResponseEvent::Completed { response_id, .. } = event.expect("event") {
                    completions.push(response_id);
                    break;
                }
            }
        }
        assert_eq!(completions, vec!["old-1", "old-2", "new-3"]);
        assert_eq!(
            server
                .handshakes()
                .iter()
                .map(|handshake| handshake.header(header_name))
                .collect::<Vec<_>>(),
            vec![
                Some("synthetic-old-value".to_string()),
                Some("synthetic-new-value".to_string()),
            ]
        );
        assert_eq!(
            server
                .connections()
                .iter()
                .map(Vec::len)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
        server.shutdown().await;
    }
}

/// #1343 case 2: under `broker_model_auth` Core holds only the placeholder key
/// and the brokered auth attaches no header (the broker adds the credential),
/// so Core cannot tell two brokered credentials apart. Brokered identities
/// therefore never match, so no cached socket is reused across them even if
/// websockets were on (they are off under the broker:
/// `tests/issue_1343_broker_websocket.rs`).
#[test]
fn issue_1343_brokered_identities_never_match() {
    struct BrokeredAuth;
    impl AuthProvider for BrokeredAuth {
        fn add_auth_headers(&self, _headers: &mut HeaderMap) {}
    }
    let placeholder = CodexAuth::from_api_key(codex_model_provider::BROKERED_KEY_PLACEHOLDER);
    let removed_account =
        ConnectionIdentity::of(Some(&placeholder), &HeaderMap::new(), &BrokeredAuth);
    let other_account =
        ConnectionIdentity::of(Some(&placeholder), &HeaderMap::new(), &BrokeredAuth);

    assert_ne!(removed_account, other_account);
    assert_ne!(
        removed_account,
        ConnectionIdentity::of(Some(&placeholder), &HeaderMap::new(), &BrokeredAuth)
    );
}

/// #1504: the other half of the broker check. In a brokered process
/// (`model_key_broker_required()`) identities never match even without the
/// placeholder key, whatever the sign-in; outside one the same inputs do.
#[test]
fn issue_1504_identities_in_a_brokered_process_never_match() {
    let chatgpt = CodexAuth::create_dummy_chatgpt_auth_for_testing();
    let api_key = CodexAuth::from_api_key("synthetic-api-key");
    let bearer = BearerAuthProvider::new("synthetic-token".to_string());
    let mut headers = HeaderMap::new();
    headers.insert("x-tenant", HeaderValue::from_static("synthetic-tenant"));
    for auth in [None, Some(&chatgpt), Some(&api_key)] {
        let identity = |broker_required| {
            ConnectionIdentity::with_broker_required(broker_required, auth, &headers, &bearer)
        };
        assert_eq!(identity(false), identity(false));
        assert_ne!(identity(true), identity(true));
        assert_ne!(identity(true), identity(false));
    }
}

/// #1504: a `CodexAuth::Headers` sign-in resolved through the real provider
/// path (`HeaderAuthProvider`). Rotating the external headers drops the
/// prewarmed socket and reconnects with the new ones; the same headers keep it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issue_1504_external_header_sign_in_rotation_reconnects_the_prewarmed_socket() {
    skip_if_no_network!();
    let server = start_websocket_server(vec![
        vec![
            vec![ev_response_created("old-1"), ev_completed("old-1")],
            vec![ev_response_created("old-2"), ev_completed("old-2")],
            // A stale connection completes so the assertions fail without a timeout.
            vec![ev_response_created("stale-3"), ev_completed("stale-3")],
        ],
        vec![vec![ev_response_created("new-3"), ev_completed("new-3")]],
    ])
    .await;
    let thread_id = ThreadId::new();
    let client_signed_in_as = |value: &'static str| {
        let mut provider =
            create_oss_provider_with_base_url(&format!("{}/v1", server.uri()), WireApi::Responses);
        provider.supports_websockets = true;
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static(value));
        let auth = CodexAuth::Headers(codex_login::AuthHeaders::new(headers));
        ModelClient::new(
            Some(AuthManager::from_auth_for_testing(auth)),
            AgentIdentityAuthPolicy::JwtOnly,
            thread_id,
            provider,
            SessionSource::Cli,
            "test_originator".to_string(),
            /*model_verbosity*/ None,
            /*enable_request_compression*/ false,
            /*include_timing_metrics*/ false,
            /*beta_features_header*/ None,
            /*concurrent_reasoning_summaries_enabled*/ false,
            /*attestation_provider*/ None,
            HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
        )
    };
    let mut session = client_signed_in_as("Bearer synthetic-old-user").new_session();
    let telemetry = test_session_telemetry();
    let model = test_model_info();
    let setup = session.client.current_client_setup().await.expect("setup");
    assert!(matches!(setup.auth, Some(CodexAuth::Headers(_))));
    assert_eq!(
        setup.api_auth.to_auth_headers().get("authorization"),
        Some(&HeaderValue::from_static("Bearer synthetic-old-user"))
    );
    let metadata = test_responses_metadata_for_client(
        &session.client,
        /*turn_id*/ None,
        "external-header-identity-test".to_string(),
        /*parent_thread_id*/ None,
        TestCodexResponsesRequestKind::WebsocketConnection,
    );
    session
        .preconnect_websocket(&telemetry, &metadata)
        .await
        .expect("preconnect");

    let mut completions = Vec::new();
    for (turn, value) in [
        ("one", "Bearer synthetic-old-user"),
        ("two", "Bearer synthetic-old-user"),
        ("three", "Bearer synthetic-new-user"),
    ] {
        session.client = client_signed_in_as(value);
        let metadata = test_responses_metadata_for_client(
            &session.client,
            Some(turn),
            "external-header-identity-test".to_string(),
            /*parent_thread_id*/ None,
            TestCodexResponsesRequestKind::Turn,
        );
        let mut stream = session
            .stream(
                &Prompt::default(),
                &model,
                &telemetry,
                /*effort*/ None,
                ReasoningSummary::None,
                /*service_tier*/ None,
                &metadata,
                &InferenceTraceContext::disabled(),
            )
            .await
            .expect("stream");
        while let Some(event) = stream.next().await {
            if let ResponseEvent::Completed { response_id, .. } = event.expect("event") {
                completions.push(response_id);
                break;
            }
        }
    }
    assert_eq!(completions, vec!["old-1", "old-2", "new-3"]);
    assert_eq!(
        server
            .handshakes()
            .iter()
            .map(|handshake| handshake.header("authorization"))
            .collect::<Vec<_>>(),
        vec![
            Some("Bearer synthetic-old-user".to_string()),
            Some("Bearer synthetic-new-user".to_string()),
        ]
    );
    assert_eq!(
        server
            .connections()
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        vec![2, 1]
    );
    server.shutdown().await;
}

/// #1343: externally supplied auth headers (`CodexAuth::Headers`) carry no
/// account, user or API key, so the headers themselves are the sign-in.
#[test]
fn external_header_sign_in_identity_follows_its_headers() {
    let sign_in = |value: &'static str| {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static(value));
        (
            CodexAuth::Headers(codex_login::AuthHeaders::new(headers.clone())),
            HeaderAuth(headers),
        )
    };
    let (old_auth, old_api_auth) = sign_in("synthetic-old-user");
    let (same_auth, same_api_auth) = sign_in("synthetic-old-user");
    let (new_auth, new_api_auth) = sign_in("synthetic-new-user");
    let identity = ConnectionIdentity::of(Some(&old_auth), &HeaderMap::new(), &old_api_auth);

    assert_eq!(
        identity,
        ConnectionIdentity::of(Some(&same_auth), &HeaderMap::new(), &same_api_auth)
    );
    assert_ne!(
        identity,
        ConnectionIdentity::of(Some(&new_auth), &HeaderMap::new(), &new_api_auth)
    );
}

/// #1343: a sign-in with a semantic identity still follows the configured
/// provider headers, while a refreshed bearer token keeps the connection.
#[test]
fn known_identity_follows_configured_headers_across_bearer_refreshes() {
    let auth = CodexAuth::create_dummy_chatgpt_auth_for_testing();
    let old = BearerAuthProvider::new("synthetic-old-token".to_string());
    let refreshed = BearerAuthProvider::new("synthetic-refreshed-token".to_string());
    let mut headers = HeaderMap::new();
    headers.insert("x-tenant", HeaderValue::from_static("synthetic-tenant-one"));
    let identity = ConnectionIdentity::of(Some(&auth), &headers, &old);

    assert_eq!(
        identity,
        ConnectionIdentity::of(Some(&auth), &headers, &refreshed)
    );
    headers.insert("x-tenant", HeaderValue::from_static("synthetic-tenant-two"));
    assert_ne!(
        identity,
        ConnectionIdentity::of(Some(&auth), &headers, &refreshed)
    );
    headers.remove("x-tenant");
    assert_ne!(
        identity,
        ConnectionIdentity::of(Some(&auth), &headers, &refreshed)
    );
}

/// #1343 case 1: a sign-in supplied through configured provider headers
/// (`http_headers`), not `client_setup.auth`/`api_auth`. The rotated header is
/// part of the connection identity, so the cached socket opened with the old
/// header is dropped and the turn reconnects with the new one (#1398 pinned
/// the gap with `stale-3`, one handshake and connections `[3]`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issue_1343_header_sign_in_rotation_reconnects_the_prewarmed_socket() {
    skip_if_no_network!();
    let server = start_websocket_server(vec![
        vec![
            vec![ev_response_created("old-1"), ev_completed("old-1")],
            vec![ev_response_created("old-2"), ev_completed("old-2")],
            vec![ev_response_created("stale-3"), ev_completed("stale-3")],
        ],
        vec![vec![ev_response_created("new-3"), ev_completed("new-3")]],
    ])
    .await;
    let thread_id = ThreadId::new();
    let client_with_header = |key: &str| {
        let mut provider =
            create_oss_provider_with_base_url(&format!("{}/v1", server.uri()), WireApi::Responses);
        provider.supports_websockets = true;
        provider.http_headers = Some(std::collections::HashMap::from([(
            "x-api-key".to_string(),
            key.to_string(),
        )]));
        test_model_client_with_provider(thread_id, SessionSource::Cli, provider)
    };
    let mut session = client_with_header("synthetic-old-key").new_session();
    let telemetry = test_session_telemetry();
    let model = test_model_info();
    let setup = session.client.current_client_setup().await.expect("setup");
    assert!(setup.auth.is_none());
    assert!(setup.api_auth.to_auth_headers().is_empty());
    let metadata = test_responses_metadata_for_client(
        &session.client,
        /*turn_id*/ None,
        "identity-test".to_string(),
        /*parent_thread_id*/ None,
        TestCodexResponsesRequestKind::WebsocketConnection,
    );
    session
        .preconnect_websocket(&telemetry, &metadata)
        .await
        .expect("preconnect");

    let mut completions = Vec::new();
    for (turn, key) in [
        ("one", "synthetic-old-key"),
        ("two", "synthetic-old-key"),
        ("three", "synthetic-new-key"),
    ] {
        session.client = client_with_header(key);
        let metadata = test_responses_metadata_for_client(
            &session.client,
            Some(turn),
            "identity-test".to_string(),
            /*parent_thread_id*/ None,
            TestCodexResponsesRequestKind::Turn,
        );
        let mut stream = session
            .stream(
                &Prompt::default(),
                &model,
                &telemetry,
                /*effort*/ None,
                ReasoningSummary::None,
                /*service_tier*/ None,
                &metadata,
                &InferenceTraceContext::disabled(),
            )
            .await
            .expect("stream");
        while let Some(event) = stream.next().await {
            if let ResponseEvent::Completed { response_id, .. } = event.expect("event") {
                completions.push(response_id);
                break;
            }
        }
    }
    assert_eq!(completions, vec!["old-1", "old-2", "new-3"]);
    assert_eq!(
        server
            .handshakes()
            .iter()
            .map(|handshake| handshake.header("x-api-key"))
            .collect::<Vec<_>>(),
        vec![
            Some("synthetic-old-key".to_string()),
            Some("synthetic-new-key".to_string()),
        ]
    );
    assert_eq!(
        server
            .connections()
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        vec![2, 1]
    );
    server.shutdown().await;
}
