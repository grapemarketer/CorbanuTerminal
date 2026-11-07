//! #1343 case 2: the credential broker (`broker_model_auth`) attaches the
//! provider credential itself, so the websocket connection identity cannot
//! see it and brokered identities never match
//! (`client_connection_identity_tests.rs`). Websockets are also off once a
//! broker is installed: a handshake cannot carry a signed broker frame
//! (PF-27-S05).
//!
//! Its own test binary, because installing the broker is process-wide.
#![allow(clippy::expect_used)]

use std::sync::Arc;

use codex_api::AuthProvider;
use codex_api::SharedAuthProvider;
use codex_core::ModelClient;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_login::auth::AgentIdentityAuthPolicy;
use codex_model_provider::BrokeredAuthRequest;
use codex_model_provider::ModelKeyBroker;
use codex_model_provider_info::WireApi;
use codex_model_provider_info::create_oss_provider_with_base_url;
use codex_protocol::ThreadId;
use codex_protocol::protocol::SessionSource;
use http::HeaderMap;

/// Auth the broker signs for: no header leaves Core.
struct BrokeredAuth;

impl AuthProvider for BrokeredAuth {
    fn add_auth_headers(&self, _headers: &mut HeaderMap) {}
}

struct TestBroker;

impl ModelKeyBroker for TestBroker {
    fn auth(
        &self,
        _request: BrokeredAuthRequest,
    ) -> codex_protocol::error::Result<SharedAuthProvider> {
        Ok(Arc::new(BrokeredAuth))
    }
}

fn websocket_client(token: &str) -> ModelClient {
    let mut provider =
        create_oss_provider_with_base_url("http://127.0.0.1:9/v1", WireApi::Responses);
    provider.supports_websockets = true;
    provider.experimental_bearer_token = Some(token.to_string());
    ModelClient::new(
        /*auth_manager*/ None,
        AgentIdentityAuthPolicy::JwtOnly,
        ThreadId::new(),
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
}

#[test]
fn issue_1343_brokered_sign_in_never_uses_a_websocket() {
    assert!(websocket_client("synthetic-old-token").responses_websocket_enabled());

    assert!(codex_model_provider::install_model_key_broker(Arc::new(
        TestBroker
    )));
    assert!(codex_model_provider::model_key_broker_required());

    // Neither the old nor the rotated credential gets a websocket, so no
    // socket opened as one brokered sign-in can serve another.
    for token in ["synthetic-old-token", "synthetic-new-token"] {
        assert!(!websocket_client(token).responses_websocket_enabled());
    }
}
