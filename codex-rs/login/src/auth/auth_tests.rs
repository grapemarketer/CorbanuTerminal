use super::*;
use crate::auth::storage::FileAuthStorage;
use crate::auth::storage::get_auth_file;
use crate::token_data::IdTokenInfo;
use codex_protocol::account::PlanType as AccountPlanType;
use codex_protocol::auth::AuthMode;
use codex_protocol::auth::KnownPlan as InternalKnownPlan;
use codex_protocol::auth::PlanType as InternalPlanType;
use codex_protocol::protocol::SessionSource;

use base64::Engine;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_protocol::config_types::ModelProviderAuthInfo;
use pretty_assertions::assert_eq;
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tempfile::TempDir;
use tempfile::tempdir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_partial_json;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

const WORKSPACE_ID_ALLOWED: &str = "123e4567-e89b-42d3-a456-426614174000";
const WORKSPACE_ID_SECOND_ALLOWED: &str = "123e4567-e89b-42d3-a456-426614174001";
const WORKSPACE_ID_DISALLOWED: &str = "123e4567-e89b-42d3-a456-426614174002";

#[tokio::test]
async fn refresh_without_id_token() {
    let codex_home = tempdir().unwrap();
    let fake_jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let storage = create_auth_storage(
        codex_home.path().to_path_buf(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    );
    let updated = super::persist_tokens(
        &storage,
        /*refreshed_from*/ "unused-for-the-default-login",
        /*id_token*/ None,
        Some("new-access-token".to_string()),
        Some("new-refresh-token".to_string()),
    )
    .expect("update_tokens should succeed");

    let tokens = updated.tokens.expect("tokens should exist");
    assert_eq!(tokens.id_token.raw_jwt, fake_jwt);
    assert_eq!(tokens.access_token, "new-access-token");
    assert_eq!(tokens.refresh_token, "new-refresh-token");
}

#[test]
fn login_with_api_key_overwrites_existing_auth_json() {
    let dir = tempdir().unwrap();
    let auth_path = dir.path().join("auth.json");
    let stale_auth = json!({
        "OPENAI_API_KEY": "sk-old",
        "tokens": {
            "id_token": "stale.header.payload",
            "access_token": "stale-access",
            "refresh_token": "stale-refresh",
            "account_id": "stale-acc"
        }
    });
    std::fs::write(
        &auth_path,
        serde_json::to_string_pretty(&stale_auth).unwrap(),
    )
    .unwrap();

    super::login_with_api_key(
        dir.path(),
        "sk-new",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("login_with_api_key should succeed");

    let storage = FileAuthStorage::new(dir.path().to_path_buf());
    let auth = storage
        .try_read_auth_json(&auth_path)
        .expect("auth.json should parse");
    assert_eq!(auth.openai_api_key.as_deref(), Some("sk-new"));
    assert!(auth.tokens.is_none(), "tokens should be cleared");
}

#[tokio::test]
async fn login_with_access_token_writes_agent_identity_jwt() {
    let dir = tempdir().unwrap();
    let auth_path = dir.path().join("auth.json");
    let record = agent_identity_record(WORKSPACE_ID_ALLOWED);
    let agent_identity =
        signed_agent_identity_jwt(&record, json!(record.plan_type)).expect("signed agent identity");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/agent-identities/jwks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(test_jwks_body()))
        .expect(1)
        .mount(&server)
        .await;
    let authapi_base_url = server.uri();
    let chatgpt_base_url = format!("{authapi_base_url}/backend-api");

    super::login_with_access_token(
        dir.path(),
        &agent_identity,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        Some(&chatgpt_base_url),
        AuthKeyringBackendKind::default(),
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect("login_with_access_token should succeed");

    let storage = FileAuthStorage::new(dir.path().to_path_buf());
    let auth = storage
        .try_read_auth_json(&auth_path)
        .expect("auth.json should parse");
    assert_eq!(auth.auth_mode, Some(AuthMode::AgentIdentity));
    assert_eq!(
        auth.agent_identity,
        Some(AgentIdentityStorage::Jwt(agent_identity))
    );
    assert!(auth.tokens.is_none(), "tokens should be cleared");
    assert!(auth.openai_api_key.is_none(), "API key should be cleared");
    server.verify().await;
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn stored_agent_identity_jwt_keeps_auth_json_unchanged() -> anyhow::Result<()> {
    let _access_token_guard = remove_access_token_env_var();
    let codex_home = tempdir()?;
    let record = agent_identity_record(WORKSPACE_ID_ALLOWED);
    let agent_identity =
        signed_agent_identity_jwt(&record, json!(record.plan_type)).expect("signed agent identity");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/agent-identities/jwks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(test_jwks_body()))
        .expect(1)
        .mount(&server)
        .await;
    mock_agent_task_registration(&server, "", &record.agent_runtime_id, "task-id").await;
    let authapi_base_url = server.uri();
    let chatgpt_base_url = format!("{authapi_base_url}/backend-api");
    save_auth(
        codex_home.path(),
        &AuthDotJson {
            auth_mode: Some(AuthMode::AgentIdentity),
            openai_api_key: None,
            tokens: None,
            last_refresh: None,
            agent_identity: Some(AgentIdentityStorage::Jwt(agent_identity.clone())),
            personal_access_token: None,
            bedrock_api_key: None,
        },
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::Direct,
    )?;

    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        Some(&chatgpt_base_url),
        AuthKeyringBackendKind::Direct,
        Some(&authapi_base_url),
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await?
    .expect("auth should load");

    let CodexAuth::AgentIdentity(agent_identity_auth) = auth else {
        panic!("stored JWT should load as agent identity auth");
    };
    assert_eq!(agent_identity_auth.run_task_id(), "task-id");
    let storage = FileAuthStorage::new(codex_home.path().to_path_buf());
    let auth = storage
        .try_read_auth_json(&get_auth_file(codex_home.path()))
        .expect("auth.json should parse");
    assert_eq!(
        auth.agent_identity,
        Some(AgentIdentityStorage::Jwt(agent_identity))
    );
    server.verify().await;
    Ok(())
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn login_with_access_token_writes_only_personal_access_token() {
    let dir = tempdir().unwrap();
    let auth_path = dir.path().join("auth.json");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/user-auth-credential/whoami"))
        .and(header("authorization", "Bearer at-login-test"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(personal_access_token_whoami(WORKSPACE_ID_ALLOWED)),
        )
        .expect(1)
        .mount(&server)
        .await;
    let _authapi_guard = EnvVarGuard::set("CODEX_AUTHAPI_BASE_URL", &server.uri());
    let allowed_workspaces = [WORKSPACE_ID_ALLOWED.to_string()];
    super::login_with_access_token(
        dir.path(),
        "at-login-test",
        AuthCredentialsStoreMode::File,
        Some(&allowed_workspaces),
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect("personal access token login should succeed");

    let storage = FileAuthStorage::new(dir.path().to_path_buf());
    let auth = storage
        .try_read_auth_json(&auth_path)
        .expect("auth.json should parse");
    assert_eq!(
        auth,
        AuthDotJson {
            auth_mode: None,
            openai_api_key: None,
            tokens: None,
            last_refresh: None,
            agent_identity: None,
            personal_access_token: Some("at-login-test".to_string()),
            bedrock_api_key: None,
        }
    );
    assert_eq!(auth.resolved_mode(), AuthMode::PersonalAccessToken);
    let persisted: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(auth_path).unwrap()).unwrap();
    assert!(persisted.get("auth_mode").is_none());
    server.verify().await;
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn login_with_access_token_rejects_personal_access_token_workspace_mismatch() {
    let dir = tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/user-auth-credential/whoami"))
        .and(header("authorization", "Bearer at-workspace-mismatch"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(personal_access_token_whoami(WORKSPACE_ID_DISALLOWED)),
        )
        .expect(1)
        .mount(&server)
        .await;
    let _authapi_guard = EnvVarGuard::set("CODEX_AUTHAPI_BASE_URL", &server.uri());
    let allowed_workspaces = [WORKSPACE_ID_ALLOWED.to_string()];

    let err = super::login_with_access_token(
        dir.path(),
        "at-workspace-mismatch",
        AuthCredentialsStoreMode::File,
        Some(&allowed_workspaces),
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect_err("personal access token workspace mismatch should fail");

    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(
        !get_auth_file(dir.path()).exists(),
        "workspace mismatch should not write auth.json"
    );
    server.verify().await;
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn login_with_access_token_rejects_invalid_personal_access_token() {
    let dir = tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/user-auth-credential/whoami"))
        .respond_with(ResponseTemplate::new(403))
        .expect(1)
        .mount(&server)
        .await;
    let _authapi_guard = EnvVarGuard::set("CODEX_AUTHAPI_BASE_URL", &server.uri());

    let err = super::login_with_access_token(
        dir.path(),
        "at-invalid-login",
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect_err("invalid personal access token should fail");

    assert_eq!(err.kind(), std::io::ErrorKind::Other);
    assert!(
        !get_auth_file(dir.path()).exists(),
        "invalid personal access token should not write auth.json"
    );
    server.verify().await;
}

#[tokio::test]
async fn login_with_access_token_rejects_invalid_jwt() {
    let dir = tempdir().unwrap();

    let err = super::login_with_access_token(
        dir.path(),
        "not-a-jwt",
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect_err("invalid access token should fail");

    assert_eq!(err.kind(), std::io::ErrorKind::Other);
    assert!(
        !get_auth_file(dir.path()).exists(),
        "invalid access token should not write auth.json"
    );
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn chatgpt_auth_registers_agent_identity_when_enabled() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some("account-123".to_string()),
        },
        codex_home.path(),
    )?;
    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        /*agent_identity_authapi_base_url*/ None,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await?
    .expect("auth should load");

    assert!(
        auth.agent_identity_auth(
            AgentIdentityAuthPolicy::JwtOnly,
            /*agent_identity_authapi_base_url*/ None,
            /*forced_chatgpt_workspace_id*/ None,
            &crate::test_support::transport_default_auth_route_config(),
            SessionSource::Cli,
        )
        .await?
        .is_none()
    );

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/agent/register"))
        .and(header("authorization", "Bearer test-access-token"))
        .and(body_partial_json(json!({
            "abom": {
                "agent_harness_id": "codex-cli",
            },
            "capabilities": ["responsesapi"],
            "ttl": null,
        })))
        .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
            "agent_runtime_id": "agent-runtime-123",
        })))
        .expect(/*r*/ 1)
        .mount(&server)
        .await;
    mock_agent_task_registration(&server, "", "agent-runtime-123", "task-123").await;

    let agent_auth = auth
        .agent_identity_auth(
            AgentIdentityAuthPolicy::ChatGptAuth,
            Some(&server.uri()),
            /*forced_chatgpt_workspace_id*/ None,
            &crate::test_support::transport_default_auth_route_config(),
            SessionSource::Cli,
        )
        .await?
        .expect("agent identity should register");
    let reused = auth
        .agent_identity_auth(
            AgentIdentityAuthPolicy::ChatGptAuth,
            Some(&server.uri()),
            /*forced_chatgpt_workspace_id*/ None,
            &crate::test_support::transport_default_auth_route_config(),
            SessionSource::Cli,
        )
        .await?
        .expect("agent identity should be reused");

    assert_eq!(
        agent_auth.record().agent_runtime_id,
        reused.record().agent_runtime_id
    );
    assert_eq!(agent_auth.run_task_id(), "task-123");
    assert_eq!(reused.run_task_id(), "task-123");
    assert_eq!(agent_auth.record().agent_runtime_id, "agent-runtime-123");
    assert_eq!(agent_auth.record().account_id, "account-123");
    assert_eq!(agent_auth.record().chatgpt_user_id, "user-12345");
    assert_eq!(agent_auth.record().task_id.as_deref(), Some("task-123"));
    assert_eq!(reused.record().task_id.as_deref(), Some("task-123"));
    let persisted = auth
        .stored_managed_chatgpt_agent_identity_record("account-123")
        .expect("identity should persist");
    assert_eq!(persisted.agent_runtime_id, "agent-runtime-123");
    assert_eq!(persisted.task_id.as_deref(), Some("task-123"));

    let reloaded = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        /*agent_identity_authapi_base_url*/ None,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await?
    .expect("auth should reload");
    let reloaded_agent_auth = reloaded
        .agent_identity_auth(
            AgentIdentityAuthPolicy::ChatGptAuth,
            Some(&server.uri()),
            /*forced_chatgpt_workspace_id*/ None,
            &crate::test_support::transport_default_auth_route_config(),
            SessionSource::Cli,
        )
        .await?
        .expect("agent identity should reload from storage");
    assert_eq!(
        reloaded_agent_auth.record().agent_runtime_id,
        "agent-runtime-123"
    );
    assert_eq!(reloaded_agent_auth.run_task_id(), "task-123");
    Ok(())
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn chatgpt_auth_retries_transient_agent_identity_registration() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some("account-123".to_string()),
        },
        codex_home.path(),
    )?;
    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        /*agent_identity_authapi_base_url*/ None,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await?
    .expect("auth should load");

    let server = MockServer::start().await;
    let registration_count = Arc::new(AtomicUsize::new(0));
    let response_count = Arc::clone(&registration_count);
    Mock::given(method("POST"))
        .and(path("/v1/agent/register"))
        .respond_with(move |_request: &wiremock::Request| {
            if response_count.fetch_add(1, Ordering::SeqCst) < 2 {
                ResponseTemplate::new(/*status*/ 503)
            } else {
                ResponseTemplate::new(/*status*/ 200).set_body_json(json!({
                    "agent_runtime_id": "agent-runtime-123",
                }))
            }
        })
        .expect(/*requests*/ 3)
        .mount(&server)
        .await;
    mock_agent_task_registration(&server, "", "agent-runtime-123", "task-123").await;

    let agent_auth = auth
        .agent_identity_auth(
            AgentIdentityAuthPolicy::ChatGptAuth,
            Some(&server.uri()),
            /*forced_chatgpt_workspace_id*/ None,
            &crate::test_support::transport_default_auth_route_config(),
            SessionSource::Cli,
        )
        .await?
        .expect("agent identity should register after retries");

    assert_eq!(registration_count.load(Ordering::SeqCst), 3);
    assert_eq!(agent_auth.record().agent_runtime_id, "agent-runtime-123");
    assert_eq!(agent_auth.record().task_id.as_deref(), Some("task-123"));
    assert_eq!(
        auth.stored_managed_chatgpt_agent_identity_record("account-123")
            .and_then(|record| record.task_id),
        Some("task-123".to_string())
    );
    Ok(())
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn chatgpt_auth_registration_retry_exhaustion_is_fallback_eligible() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some("account-123".to_string()),
        },
        codex_home.path(),
    )?;
    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        /*agent_identity_authapi_base_url*/ None,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await?
    .expect("auth should load");

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/agent/register"))
        .respond_with(ResponseTemplate::new(/*status*/ 503))
        .expect(/*requests*/ 3)
        .mount(&server)
        .await;

    let err = auth
        .agent_identity_auth(
            AgentIdentityAuthPolicy::ChatGptAuth,
            Some(&server.uri()),
            /*forced_chatgpt_workspace_id*/ None,
            &crate::test_support::transport_default_auth_route_config(),
            SessionSource::Cli,
        )
        .await
        .expect_err("retry exhaustion should return an error");

    assert!(AgentIdentityAuthError::bootstrap_unavailable(&err).is_some());
    assert!(
        auth.stored_managed_chatgpt_agent_identity_record("account-123")
            .is_none()
    );
    Ok(())
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn chatgpt_auth_task_registration_retry_exhaustion_is_fallback_eligible() -> anyhow::Result<()>
{
    let codex_home = tempdir()?;
    write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some("account-123".to_string()),
        },
        codex_home.path(),
    )?;
    let mut record = agent_identity_record("account-123");
    record.chatgpt_user_id = "user-12345".to_string();
    record.email = Some("user@example.com".to_string());
    let storage = FileAuthStorage::new(codex_home.path().to_path_buf());
    let auth_path = get_auth_file(codex_home.path());
    let mut auth_json = storage.try_read_auth_json(&auth_path)?;
    auth_json.agent_identity = Some(AgentIdentityStorage::Record(record.clone()));
    storage.save(&auth_json)?;
    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        /*agent_identity_authapi_base_url*/ None,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await?
    .expect("auth should load");

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/v1/agent/{}/task/register",
            record.agent_runtime_id
        )))
        .respond_with(ResponseTemplate::new(/*status*/ 503))
        .expect(/*requests*/ 3)
        .mount(&server)
        .await;

    let err = auth
        .agent_identity_auth(
            AgentIdentityAuthPolicy::ChatGptAuth,
            Some(&server.uri()),
            /*forced_chatgpt_workspace_id*/ None,
            &crate::test_support::transport_default_auth_route_config(),
            SessionSource::Cli,
        )
        .await
        .expect_err("task retry exhaustion should return an error");

    assert!(AgentIdentityAuthError::bootstrap_unavailable(&err).is_some());
    record.task_id = None;
    assert_eq!(
        auth.stored_managed_chatgpt_agent_identity_record("account-123"),
        Some(record)
    );
    Ok(())
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn chatgpt_auth_non_retryable_registration_error_is_hard_failure() -> anyhow::Result<()> {
    let codex_home = tempdir()?;
    write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some("account-123".to_string()),
        },
        codex_home.path(),
    )?;
    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        /*agent_identity_authapi_base_url*/ None,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await?
    .expect("auth should load");

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/agent/register"))
        .respond_with(ResponseTemplate::new(/*status*/ 403))
        .expect(/*requests*/ 1)
        .mount(&server)
        .await;

    let err = auth
        .agent_identity_auth(
            AgentIdentityAuthPolicy::ChatGptAuth,
            Some(&server.uri()),
            /*forced_chatgpt_workspace_id*/ None,
            &crate::test_support::transport_default_auth_route_config(),
            SessionSource::Cli,
        )
        .await
        .expect_err("hard registration failure should return an error");

    assert!(AgentIdentityAuthError::bootstrap_unavailable(&err).is_none());
    assert!(
        auth.stored_managed_chatgpt_agent_identity_record("account-123")
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn agent_identity_jwt_task_registration_retry_exhaustion_is_strict() -> anyhow::Result<()> {
    let record = agent_identity_record(WORKSPACE_ID_ALLOWED);
    let agent_identity =
        signed_agent_identity_jwt(&record, json!(record.plan_type)).expect("signed agent identity");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/agent-identities/jwks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(test_jwks_body()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!(
            "/v1/agent/{}/task/register",
            record.agent_runtime_id
        )))
        .respond_with(ResponseTemplate::new(/*status*/ 503))
        .expect(/*requests*/ 3)
        .mount(&server)
        .await;
    let authapi_base_url = server.uri();
    let chatgpt_base_url = format!("{authapi_base_url}/backend-api");

    let err = CodexAuth::from_agent_identity_jwt_with_authapi_base_url(
        &agent_identity,
        Some(&chatgpt_base_url),
        &authapi_base_url,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect_err("agent identity jwt task retry exhaustion should fail");

    assert!(AgentIdentityAuthError::bootstrap_unavailable(&err).is_none());
    Ok(())
}

#[tokio::test]
async fn login_with_access_token_rejects_unsigned_jwt() {
    let dir = tempdir().unwrap();
    let record = agent_identity_record(WORKSPACE_ID_ALLOWED);
    let agent_identity = fake_agent_identity_jwt(&record).expect("fake agent identity");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/agent-identities/jwks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(test_jwks_body()))
        .expect(1)
        .mount(&server)
        .await;
    let authapi_base_url = server.uri();
    let chatgpt_base_url = format!("{authapi_base_url}/backend-api");

    super::login_with_access_token(
        dir.path(),
        &agent_identity,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        Some(&chatgpt_base_url),
        AuthKeyringBackendKind::default(),
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect_err("unsigned access token should fail");

    assert!(
        !get_auth_file(dir.path()).exists(),
        "unsigned access token should not write auth.json"
    );
    server.verify().await;
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn missing_auth_json_returns_none() {
    let dir = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let auth = CodexAuth::from_auth_storage(
        dir.path(),
        AuthCredentialsStoreMode::File,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect("call should succeed");
    assert_eq!(auth, None);
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn pro_account_with_no_api_key_uses_chatgpt_auth() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let fake_jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        /*agent_identity_authapi_base_url*/ None,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(None, auth.api_key());
    assert_eq!(AuthMode::Chatgpt, auth.auth_mode());
    assert_eq!(auth.get_chatgpt_user_id().as_deref(), Some("user-12345"));

    let auth_dot_json = auth
        .get_current_auth_json()
        .expect("AuthDotJson should exist");
    let last_refresh = auth_dot_json
        .last_refresh
        .expect("last_refresh should be recorded");

    assert_eq!(
        AuthDotJson {
            auth_mode: None,
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: IdTokenInfo {
                    email: Some("user@example.com".to_string()),
                    chatgpt_plan_type: Some(InternalPlanType::Known(InternalKnownPlan::Pro)),
                    chatgpt_user_id: Some("user-12345".to_string()),
                    chatgpt_account_id: None,
                    chatgpt_account_is_fedramp: false,
                    raw_jwt: fake_jwt,
                },
                access_token: "test-access-token".to_string(),
                refresh_token: "test-refresh-token".to_string(),
                account_id: None,
            }),
            last_refresh: Some(last_refresh),
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
        },
        auth_dot_json
    );
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn product_homes_fall_back_to_legacy_codex_auth() {
    for product_home_name in [".corbanu", ".pfterminal"] {
        let root = tempdir().unwrap();
        let product_home = root.path().join(product_home_name);
        let legacy_codex_home = root.path().join(".codex");
        std::fs::create_dir_all(&product_home).expect("create product home");
        std::fs::create_dir_all(&legacy_codex_home).expect("create legacy codex home");
        let _access_token_guard = remove_access_token_env_var();
        write_auth_file(
            AuthFileParams {
                openai_api_key: Some("sk-legacy-key".to_string()),
                chatgpt_plan_type: Some("pro".to_string()),
                chatgpt_account_id: None,
            },
            &legacy_codex_home,
        )
        .expect("failed to write legacy auth file");
        let legacy_auth_file = get_auth_file(&legacy_codex_home);
        let mut legacy_auth_json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&legacy_auth_file).expect("read auth"))
                .expect("parse auth");
        legacy_auth_json["auth_mode"] = serde_json::Value::String("chatgpt".to_string());
        std::fs::write(
            &legacy_auth_file,
            serde_json::to_string_pretty(&legacy_auth_json).expect("serialize auth"),
        )
        .expect("write auth");

        let auth = super::load_auth(
            &product_home,
            /*enable_codex_api_key_env*/ false,
            AuthCredentialsStoreMode::File,
            /*forced_chatgpt_workspace_id*/ None,
            /*chatgpt_base_url*/ None,
            AuthKeyringBackendKind::Direct,
            /*agent_identity_authapi_base_url*/ None,
            &crate::test_support::transport_default_auth_route_config(),
        )
        .await
        .expect("load auth")
        .expect("auth should fall back to legacy codex home");

        assert_eq!(AuthMode::Chatgpt, auth.auth_mode());
        assert_eq!(auth.get_chatgpt_user_id().as_deref(), Some("user-12345"));
    }
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn loads_api_key_from_auth_json() {
    let dir = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let auth_file = dir.path().join("auth.json");
    std::fs::write(
        auth_file,
        r#"{"OPENAI_API_KEY":"sk-test-key","tokens":null,"last_refresh":null}"#,
    )
    .unwrap();

    let auth = super::load_auth(
        dir.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        /*agent_identity_authapi_base_url*/ None,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(auth.auth_mode(), AuthMode::ApiKey);
    assert_eq!(auth.api_key(), Some("sk-test-key"));

    assert!(auth.get_token_data().is_err());
}

#[test]
fn logout_removes_auth_file() -> Result<(), std::io::Error> {
    let dir = tempdir()?;
    let auth_dot_json = AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some("sk-test-key".to_string()),
        tokens: None,
        last_refresh: None,
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
    };
    super::save_auth(
        dir.path(),
        &auth_dot_json,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    let auth_file = get_auth_file(dir.path());
    assert!(auth_file.exists());
    assert!(logout(
        dir.path(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?);
    assert!(!auth_file.exists());
    Ok(())
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn unauthorized_recovery_reports_mode_and_step_names() {
    let dir = tempdir().unwrap();
    let manager = AuthManager::shared(
        dir.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        crate::test_support::transport_default_auth_route_config(),
    )
    .await;
    let managed = UnauthorizedRecovery {
        manager: Arc::clone(&manager),
        step: UnauthorizedRecoveryStep::Reload,
        expected_account_id: None,
        mode: UnauthorizedRecoveryMode::Managed,
    };
    assert_eq!(managed.mode_name(), "managed");
    assert_eq!(managed.step_name(), "reload");

    let external = UnauthorizedRecovery {
        manager,
        step: UnauthorizedRecoveryStep::ExternalRefresh,
        expected_account_id: None,
        mode: UnauthorizedRecoveryMode::External,
    };
    assert_eq!(external.mode_name(), "external");
    assert_eq!(external.step_name(), "external_refresh");
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn refresh_failure_is_scoped_to_the_matching_auth_snapshot() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some(WORKSPACE_ID_ALLOWED.to_string()),
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        /*agent_identity_authapi_base_url*/ None,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect("load auth")
    .expect("auth available");
    let mut updated_auth_dot_json = auth
        .get_current_auth_json()
        .expect("AuthDotJson should exist");
    let updated_tokens = updated_auth_dot_json
        .tokens
        .as_mut()
        .expect("tokens should exist");
    updated_tokens.access_token = "new-access-token".to_string();
    updated_tokens.refresh_token = "new-refresh-token".to_string();
    let updated_auth = CodexAuth::from_auth_dot_json(
        codex_home.path(),
        updated_auth_dot_json,
        AuthCredentialsStoreMode::File,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        /*agent_identity_authapi_base_url*/ None,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect("updated auth should parse");

    let manager = AuthManager::from_auth_for_testing(auth.clone());
    let error = RefreshTokenFailedError::new(
        RefreshTokenFailedReason::Exhausted,
        "refresh token already used",
    );
    manager.record_permanent_refresh_failure_if_unchanged(&auth, &error);

    assert_eq!(manager.refresh_failure_for_auth(&auth), Some(error));
    assert_eq!(manager.refresh_failure_for_auth(&updated_auth), None);
}

#[tokio::test]
async fn external_bearer_only_auth_manager_uses_cached_provider_token() {
    let script = ProviderAuthScript::new(&["provider-token", "next-token"]).unwrap();
    let manager = AuthManager::external_bearer_only(script.auth_config());

    let first = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));
    let second = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));

    assert_eq!(first.as_deref(), Some("provider-token"));
    assert_eq!(second.as_deref(), Some("provider-token"));
    assert_eq!(manager.auth_mode(), Some(AuthMode::ApiKey));
    assert_eq!(manager.get_api_auth_mode(), Some(AuthMode::ApiKey));
}

/// PF-84: a named account reaches the provider command only as
/// `CORBANU_PROVIDER_ACCOUNT`; the default account never inherits one.
#[cfg(unix)]
#[tokio::test]
async fn external_bearer_command_receives_the_named_account() {
    let tempdir = tempfile::tempdir().unwrap();
    let config: ModelProviderAuthInfo = serde_json::from_value(json!({
        "command": "/bin/sh",
        "args": ["-c", "printf 'account-%s' \"${CORBANU_PROVIDER_ACCOUNT:-none}\""],
        "timeout_ms": 10_000,
        "refresh_interval_ms": 60000,
        "cwd": tempdir.path(),
    }))
    .expect("provider auth config should deserialize");
    let token = |account: Option<&str>| {
        let manager = AuthManager::external_bearer_only_for_account(
            config.clone(),
            ExternalBearerCachePolicy::FreshPerRequest,
            account.map(|name| crate::auth::ExternalBearerAccount {
                provider_id: "custom".to_string(),
                name: name.to_string(),
                registry_home: None,
                server: crate::auth::ExternalBearerAccountServer::Command,
            }),
        );
        async move {
            manager
                .auth()
                .await
                .and_then(|auth| auth.api_key().map(str::to_string))
        }
    };
    assert_eq!(token(Some("work")).await.as_deref(), Some("account-work"));
    assert_eq!(token(None).await.as_deref(), Some("account-none"));
}

/// PF-84: an account missing from the registry never runs the command.
#[cfg(unix)]
#[tokio::test]
async fn external_bearer_command_never_runs_for_an_unenrolled_account() {
    let tempdir = tempfile::tempdir().unwrap();
    let canary = tempdir.path().join("ran");
    let config: ModelProviderAuthInfo = serde_json::from_value(json!({
        "command": "/bin/sh",
        "args": ["-c", format!("touch {} && printf token", canary.display())],
        "timeout_ms": 10_000,
        "refresh_interval_ms": 60000,
        "cwd": tempdir.path(),
    }))
    .expect("provider auth config should deserialize");
    let empty_home = tempfile::tempdir().unwrap();
    let manager = AuthManager::external_bearer_only_for_account(
        config,
        ExternalBearerCachePolicy::FreshPerRequest,
        Some(crate::auth::ExternalBearerAccount {
            provider_id: "custom".to_string(),
            name: "work".to_string(),
            registry_home: Some(empty_home.path().to_path_buf()),
            server: crate::auth::ExternalBearerAccountServer::Command,
        }),
    );
    assert!(manager.auth().await.is_none());
    assert!(!canary.exists(), "the command must not run");
}

/// PF-84 (#419): an unenrolled Claude Plan account never spawns the helper.
#[cfg(unix)]
#[tokio::test]
async fn claude_plan_helper_never_runs_for_an_unenrolled_account() {
    let tempdir = tempfile::tempdir().unwrap();
    let canary = tempdir.path().join("ran");
    let config: ModelProviderAuthInfo = serde_json::from_value(json!({
        "command": "/bin/sh",
        "args": ["-c", format!("touch {} && printf token", canary.display())],
        "timeout_ms": 10_000,
        "refresh_interval_ms": 0,
        "cwd": tempdir.path(),
    }))
    .expect("provider auth config should deserialize");
    let empty_home = tempfile::tempdir().unwrap();
    let account = crate::auth::ExternalBearerAccount {
        provider_id: "claude-plan".to_string(),
        name: "ghost".to_string(),
        registry_home: Some(empty_home.path().to_path_buf()),
        server: crate::auth::ExternalBearerAccountServer::ClaudePlanHelper,
    };
    let manager = AuthManager::external_bearer_only_for_account(
        config.clone(),
        ExternalBearerCachePolicy::FreshPerRequest,
        Some(account.clone()),
    );
    assert!(manager.auth().await.is_none());
    let error = account
        .ensure_enrolled()
        .expect_err("an unenrolled account must be refused");
    assert!(error.to_string().contains("--kind claude-token"), "{error}");
    assert!(!canary.exists(), "the helper must not run");
}

/// PF-84 (#414): Corbanu's Claude helper gets the account in argv, so a
/// helper that predates named accounts (and ignores
/// `CORBANU_PROVIDER_ACCOUNT`) fails instead of serving the default account.
#[cfg(unix)]
#[tokio::test]
async fn claude_plan_helper_that_predates_named_accounts_is_refused() {
    use std::os::unix::fs::PermissionsExt;

    let tempdir = tempfile::tempdir().unwrap();
    let helper = |name: &str, body: &str| -> ModelProviderAuthInfo {
        let path = tempdir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        serde_json::from_value(json!({
            "command": path,
            "args": [crate::auth::CLAUDE_PLAN_TOKEN_HELPER_SUBCOMMAND],
            "timeout_ms": 10_000,
            "refresh_interval_ms": 0,
            "cwd": tempdir.path(),
        }))
        .expect("provider auth config should deserialize")
    };
    // Like a pre-PF-84 `corbanu internal-claude-oauth-token`: no options, and
    // always the default account's token.
    let old_helper = helper(
        "old-corbanu",
        "if [ $# -gt 1 ]; then echo \"error: unexpected argument '$2' found\" >&2; exit 2; fi; printf default-account-token",
    );
    let current_helper = helper(
        "corbanu",
        "[ \"$*\" = \"internal-claude-oauth-token --account $CORBANU_PROVIDER_ACCOUNT --enable named_accounts\" ] && printf \"account-%s\" \"$CORBANU_PROVIDER_ACCOUNT\"",
    );
    let token = |config: &ModelProviderAuthInfo, account: Option<&str>| {
        let manager = AuthManager::external_bearer_only_for_account(
            config.clone(),
            ExternalBearerCachePolicy::FreshPerRequest,
            account.map(|name| crate::auth::ExternalBearerAccount {
                provider_id: "claude-plan".to_string(),
                name: name.to_string(),
                registry_home: None,
                server: crate::auth::ExternalBearerAccountServer::ClaudePlanHelper,
            }),
        );
        async move {
            manager
                .auth()
                .await
                .and_then(|auth| auth.api_key().map(str::to_string))
        }
    };

    assert_eq!(token(&old_helper, Some("work")).await, None);
    let error = crate::validate_provider_auth_command_for_account(&old_helper, Some("work"))
        .await
        .expect_err("an outdated helper must be refused");
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    assert!(
        error.to_string().contains("predates named accounts"),
        "{error}"
    );
    assert_eq!(
        token(&current_helper, Some("work")).await.as_deref(),
        Some("account-work")
    );
    // The default account keeps today's argv.
    assert_eq!(
        token(&old_helper, None).await.as_deref(),
        Some("default-account-token")
    );
}

#[tokio::test]
async fn fresh_per_request_external_bearer_never_reuses_the_previous_token() {
    let script = ProviderAuthScript::new(&["first-source-token", "second-source-token"]).unwrap();
    let manager = AuthManager::external_bearer_only_with_cache_policy(
        script.auth_config(),
        ExternalBearerCachePolicy::FreshPerRequest,
    );

    let first = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));
    let second = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));

    assert_eq!(first.as_deref(), Some("first-source-token"));
    assert_eq!(second.as_deref(), Some("second-source-token"));
}

#[tokio::test]
async fn revision_tracked_external_bearer_reuses_until_selection_changes() {
    let script = ProviderAuthScript::new(&["first-source-token", "second-source-token"]).unwrap();
    let revision_dir = tempdir().unwrap();
    let revision_path = revision_dir.path().join("selection-revision");
    std::fs::write(&revision_path, "revision-1").unwrap();
    let manager = AuthManager::external_bearer_only_with_cache_policy(
        script.auth_config(),
        ExternalBearerCachePolicy::InvalidateOnChange(revision_path.clone()),
    );

    let first = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));
    let cached = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));
    std::fs::write(&revision_path, "revision-2").unwrap();
    let replacement = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));
    let replacement_cached = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));

    assert_eq!(first.as_deref(), Some("first-source-token"));
    assert_eq!(cached.as_deref(), Some("first-source-token"));
    assert_eq!(replacement.as_deref(), Some("second-source-token"));
    assert_eq!(replacement_cached.as_deref(), Some("second-source-token"));
}

#[tokio::test]
async fn revision_tracked_external_bearer_falls_back_to_uncached_when_marker_is_missing() {
    let script = ProviderAuthScript::new(&["first-source-token", "second-source-token"]).unwrap();
    let revision_dir = tempdir().unwrap();
    let manager = AuthManager::external_bearer_only_with_cache_policy(
        script.auth_config(),
        ExternalBearerCachePolicy::InvalidateOnChange(
            revision_dir.path().join("missing-selection-revision"),
        ),
    );

    let first = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));
    let second = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));

    assert_eq!(first.as_deref(), Some("first-source-token"));
    assert_eq!(second.as_deref(), Some("second-source-token"));
}

#[tokio::test]
async fn fresh_per_request_external_bearer_does_not_retain_a_refreshed_token() {
    let script = ProviderAuthScript::new(&[
        "first-source-token",
        "refreshed-source-token",
        "replacement-source-token",
    ])
    .unwrap();
    let manager = AuthManager::external_bearer_only_with_cache_policy(
        script.auth_config(),
        ExternalBearerCachePolicy::FreshPerRequest,
    );
    let mut recovery = manager.unauthorized_recovery();

    let first = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));
    recovery
        .next()
        .await
        .expect("external refresh should succeed");
    let replacement = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));

    assert_eq!(first.as_deref(), Some("first-source-token"));
    assert_eq!(replacement.as_deref(), Some("replacement-source-token"));
}

#[tokio::test]
async fn external_bearer_only_auth_manager_disables_auto_refresh_when_interval_is_zero() {
    let script = ProviderAuthScript::new(&["provider-token", "next-token"]).unwrap();
    let mut auth_config = script.auth_config();
    auth_config.refresh_interval_ms = 0;
    let manager = AuthManager::external_bearer_only(auth_config);

    let first = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));
    let second = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));

    assert_eq!(first.as_deref(), Some("provider-token"));
    assert_eq!(second.as_deref(), Some("provider-token"));
}

#[tokio::test]
async fn external_bearer_only_auth_manager_returns_none_when_command_fails() {
    let script = ProviderAuthScript::new_failing().unwrap();
    let manager = AuthManager::external_bearer_only(script.auth_config());

    assert_eq!(manager.auth().await, None);
}

#[tokio::test]
async fn provider_auth_command_validation_accepts_a_usable_token_without_returning_it() {
    let script = ProviderAuthScript::new(&["provider-token"]).unwrap();

    crate::auth::validate_provider_auth_command(&script.auth_config())
        .await
        .expect("usable provider auth command should pass preflight");
}

#[tokio::test]
async fn provider_auth_command_validation_preserves_actionable_failures() {
    let script = ProviderAuthScript::new_failing().unwrap();

    let error = crate::auth::validate_provider_auth_command(&script.auth_config())
        .await
        .expect_err("failing provider auth command should fail preflight");

    assert!(error.to_string().contains("exited with status"));
}

// Keep the same writable-file condition as an inherited descriptor without
// depending on a concurrent fork landing in a timing window.
#[cfg(unix)]
#[tokio::test]
async fn provider_auth_fixture_accepts_token_while_script_is_open_for_writing() {
    let script = ProviderAuthScript::new(&["provider-token"]).unwrap();
    let writer = std::fs::OpenOptions::new()
        .write(true)
        .open(script.tempdir.path().join("print-token.sh"))
        .unwrap();

    crate::auth::validate_provider_auth_command(&script.auth_config())
        .await
        .expect("the interpreter should read the fixture while its write descriptor is open");

    drop(writer);
}

#[cfg(unix)]
#[tokio::test]
async fn provider_auth_fixture_preserves_failure_while_script_is_open_for_writing() {
    let script = ProviderAuthScript::new_failing().unwrap();
    let writer = std::fs::OpenOptions::new()
        .write(true)
        .open(script.tempdir.path().join("fail.sh"))
        .unwrap();

    let error = crate::auth::validate_provider_auth_command(&script.auth_config())
        .await
        .expect_err("the fixture should still exit unsuccessfully");

    assert!(error.to_string().contains("exited with status"));
    drop(writer);
}

/// Writes `name` in a temp dir: prints `stderr` to stderr and fails.
#[cfg(unix)]
fn failing_helper(
    name: &str,
    stderr: &str,
    args: &[&str],
) -> (tempfile::TempDir, ModelProviderAuthInfo) {
    use std::os::unix::fs::PermissionsExt;
    let tempdir = tempfile::tempdir().expect("tempdir");
    let path = tempdir.path().join(name);
    std::fs::write(&path, format!("#!/bin/sh\necho '{stderr}' >&2\nexit 1\n"))
        .expect("write helper");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let config = serde_json::from_value(json!({
        "command": path,
        "args": args,
        "timeout_ms": 10_000,
        "cwd": tempdir.path(),
    }))
    .expect("provider auth config should deserialize");
    (tempdir, config)
}

/// The error of a helper written just before: retried while Linux still
/// reports it busy (another test's fork can hold the write descriptor).
#[cfg(unix)]
async fn failing_helper_message(config: &ModelProviderAuthInfo) -> String {
    for _ in 0..40 {
        let message = crate::auth::validate_provider_auth_command(config)
            .await
            .expect_err("failing helper")
            .to_string();
        if !message.contains("Text file busy") {
            return message;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("helper stayed busy");
}

/// #548: another program's stderr may echo anything; it stays out of the
/// error, which is shown and logged.
#[cfg(unix)]
#[tokio::test]
async fn provider_auth_command_failure_withholds_helper_stderr() {
    const CANARY: &str = "fake-helper-stderr-canary-548-81d0e4";
    let (_dir, config) = failing_helper("helper.sh", CANARY, &[]);

    let message = failing_helper_message(&config).await;

    assert!(!message.contains(CANARY), "{message}");
    assert!(
        message.contains("exited with status") && message.contains("output is withheld"),
        "{message}"
    );
}

/// Corbanu's own helpers' messages stay actionable, credential-shaped
/// values redacted.
#[cfg(unix)]
#[tokio::test]
async fn corbanu_internal_helper_failure_keeps_its_message_redacted() {
    const TOKEN: &str = "fake-internal-token-548-5be0c3";
    // Any program name (a debug build, `/proc/self/exe`): the subcommand decides.
    let (_dir, config) = failing_helper(
        "corbanu-debug",
        &format!("Claude Plan is not signed in; run corbanu login (access_token={TOKEN})"),
        &["internal-claude-oauth-token"],
    );

    let message = failing_helper_message(&config).await;

    assert!(message.contains("run corbanu login"), "{message}");
    assert!(!message.contains(TOKEN), "{message}");
}

/// #616: only Corbanu's own helper exiting 75 marks an unreadable home, the
/// failure `corbanu exec` waits out.
#[cfg(unix)]
#[tokio::test]
async fn internal_helper_exit_75_marks_an_unreadable_home() {
    use std::os::unix::fs::PermissionsExt;
    let tempdir = tempfile::tempdir().expect("tempdir");
    let path = tempdir.path().join("corbanu");
    std::fs::write(
        &path,
        "#!/bin/sh\necho 'Corbanu could not read its home (Operation not permitted)' >&2\nexit 75\n",
    )
    .expect("write helper");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let config = |args: &[&str]| -> ModelProviderAuthInfo {
        serde_json::from_value(json!({
            "command": path,
            "args": args,
            "timeout_ms": 10_000,
            "cwd": tempdir.path(),
        }))
        .expect("provider auth config should deserialize")
    };
    let mut marked = Vec::new();
    for args in [&["internal-claude-oauth-token"][..], &[]] {
        let mut error = None;
        for _ in 0..50 {
            let failure = crate::auth::validate_provider_auth_command(&config(args))
                .await
                .expect_err("failing helper");
            // Linux may still report the just-written helper busy.
            if !failure.to_string().contains("Text file busy") {
                error = Some(failure);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let error = error.expect("helper stayed busy");
        marked.push(codex_protocol::home_unreadable::is_home_unreadable(&error));
    }
    assert_eq!(marked, vec![true, false]);
}

#[tokio::test]
async fn unauthorized_recovery_uses_external_refresh_for_bearer_manager() {
    let script = ProviderAuthScript::new(&["provider-token", "refreshed-provider-token"]).unwrap();
    let mut auth_config = script.auth_config();
    auth_config.refresh_interval_ms = 0;
    let manager = AuthManager::external_bearer_only(auth_config);
    let mut recovery = manager.unauthorized_recovery();
    let initial_token = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));

    assert!(recovery.has_next());
    assert_eq!(recovery.mode_name(), "external");
    assert_eq!(recovery.step_name(), "external_refresh");

    let result = recovery
        .next()
        .await
        .expect("external refresh should succeed");

    assert_eq!(result.auth_state_changed(), Some(true));
    let refreshed_token = manager
        .auth()
        .await
        .and_then(|auth| auth.api_key().map(str::to_string));
    assert_eq!(initial_token.as_deref(), Some("provider-token"));
    assert_eq!(refreshed_token.as_deref(), Some("refreshed-provider-token"));
}

#[derive(Clone)]
struct StaticExternalAuth(CodexAuth);

impl ExternalAuth for StaticExternalAuth {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Ok(self.0.clone()) })
    }

    fn refresh(&self, _context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(async { Ok(self.0.clone()) })
    }
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn external_auth_provider_can_install_headers() {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        http::header::AUTHORIZATION,
        http::HeaderValue::from_static("Bearer external"),
    );
    headers.insert("x-external-auth", http::HeaderValue::from_static("enabled"));
    let auth = CodexAuth::Headers(AuthHeaders::new(headers));
    let codex_home = tempdir().expect("tempdir");
    let manager = AuthManager::new(
        codex_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::Ephemeral,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        crate::test_support::transport_default_auth_route_config(),
    )
    .await;

    manager
        .set_external_auth(Arc::new(StaticExternalAuth(auth.clone())))
        .await
        .expect("external auth should install");

    assert_eq!(manager.auth_cached(), Some(auth));
    assert!(
        manager
            .auth_cached()
            .is_some_and(|auth| auth.uses_codex_backend())
    );
    assert!(
        !manager
            .auth_cached()
            .is_some_and(|auth| auth.is_chatgpt_auth())
    );
}

struct ProviderAuthScript {
    tempdir: TempDir,
    command: String,
    args: Vec<String>,
    /// #1201: held while the helper may run; see `TEST_CHILD_PROCESSES`.
    /// One per test: a second read lock on the same thread can deadlock
    /// while the writer waits.
    _helper_runs: std::sync::RwLockReadGuard<'static, ()>,
}

fn helper_runs() -> std::sync::RwLockReadGuard<'static, ()> {
    crate::TEST_CHILD_PROCESSES
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl ProviderAuthScript {
    fn new(tokens: &[&str]) -> std::io::Result<Self> {
        let tempdir = tempfile::tempdir()?;
        let token_file = tempdir.path().join("tokens.txt");
        // `cmd.exe`'s `set /p` treats LF-only input as one line, so use CRLF on Windows.
        let token_line_ending = if cfg!(windows) { "\r\n" } else { "\n" };
        let mut token_file_contents = String::new();
        for token in tokens {
            token_file_contents.push_str(token);
            token_file_contents.push_str(token_line_ending);
        }
        std::fs::write(&token_file, token_file_contents)?;

        #[cfg(unix)]
        let (command, args) = {
            let script_path = tempdir.path().join("print-token.sh");
            std::fs::write(
                &script_path,
                r#"#!/bin/sh
first_line=$(sed -n '1p' tokens.txt)
printf '%s\n' "$first_line"
tail -n +2 tokens.txt > tokens.next
mv tokens.next tokens.txt
"#,
            )?;
            // Execute the stable interpreter, not a just-written script whose write
            // descriptor another test's fork may still hold (ETXTBSY on Linux).
            ("/bin/sh".to_string(), vec!["./print-token.sh".to_string()])
        };

        #[cfg(windows)]
        let (command, args) = {
            let script_path = tempdir.path().join("print-token.cmd");
            std::fs::write(
                &script_path,
                r#"@echo off
setlocal EnableExtensions DisableDelayedExpansion
set "first_line="
<tokens.txt set /p "first_line="
if not defined first_line exit /b 1
setlocal EnableDelayedExpansion
echo(!first_line!
endlocal
rem Drop the first line without more.com, which fails in Bazel's batched
rem Windows test runs and would leave an empty token file behind.
type nul > tokens.next
for /f "usebackq skip=1 delims=" %%L in ("tokens.txt") do >>tokens.next echo(%%L
move /y tokens.next tokens.txt >nul
"#,
            )?;
            (
                "cmd.exe".to_string(),
                vec![
                    "/d".to_string(),
                    "/s".to_string(),
                    "/c".to_string(),
                    ".\\print-token.cmd".to_string(),
                ],
            )
        };

        Ok(Self {
            tempdir,
            command,
            args,
            _helper_runs: helper_runs(),
        })
    }

    fn new_failing() -> std::io::Result<Self> {
        let tempdir = tempfile::tempdir()?;

        #[cfg(unix)]
        let (command, args) = {
            let script_path = tempdir.path().join("fail.sh");
            std::fs::write(
                &script_path,
                r#"#!/bin/sh
exit 1
"#,
            )?;
            ("/bin/sh".to_string(), vec!["./fail.sh".to_string()])
        };

        #[cfg(windows)]
        let (command, args) = (
            "cmd.exe".to_string(),
            vec![
                "/d".to_string(),
                "/s".to_string(),
                "/c".to_string(),
                "exit /b 1".to_string(),
            ],
        );

        Ok(Self {
            tempdir,
            command,
            args,
            _helper_runs: helper_runs(),
        })
    }

    fn auth_config(&self) -> ModelProviderAuthInfo {
        serde_json::from_value(json!({
            "command": self.command,
            "args": self.args,
            // Process startup can be slow on loaded Windows CI workers, so leave enough slack to
            // avoid turning these auth-cache assertions into a process-launch timing test.
            // #1201: 10 s was not enough. On the native Windows lane, `cmd.exe` helper runs
            // that take ~20 ms took 1.5-4.4 s, and occasionally over 10 s, while Bazel built
            // other targets (such as `core-unit-tests-bin`) next to this binary. No test here
            // exercises the timeout; the product default stays 5 s.
            "timeout_ms": 60_000,
            "refresh_interval_ms": 60000,
            "cwd": self.tempdir.path(),
        }))
        .expect("provider auth config should deserialize")
    }
}

struct AuthFileParams {
    openai_api_key: Option<String>,
    chatgpt_plan_type: Option<String>,
    chatgpt_account_id: Option<String>,
}

fn write_auth_file(params: AuthFileParams, codex_home: &Path) -> std::io::Result<String> {
    let fake_jwt = fake_jwt_for_auth_file_params(&params)?;
    let auth_file = get_auth_file(codex_home);
    let auth_json_data = json!({
        "OPENAI_API_KEY": params.openai_api_key,
        "tokens": {
            "id_token": fake_jwt,
            "access_token": "test-access-token",
            "refresh_token": "test-refresh-token"
        },
        "last_refresh": Utc::now(),
    });
    let auth_json = serde_json::to_string_pretty(&auth_json_data)?;
    std::fs::write(auth_file, auth_json)?;
    Ok(fake_jwt)
}

fn fake_jwt_for_auth_file_params(params: &AuthFileParams) -> std::io::Result<String> {
    #[derive(Serialize)]
    struct Header {
        alg: &'static str,
        typ: &'static str,
    }

    let header = Header {
        alg: "none",
        typ: "JWT",
    };
    let mut auth_payload = serde_json::json!({
        "chatgpt_user_id": "user-12345",
        "user_id": "user-12345",
    });

    if let Some(chatgpt_plan_type) = params.chatgpt_plan_type.as_ref() {
        auth_payload["chatgpt_plan_type"] = serde_json::Value::String(chatgpt_plan_type.clone());
    }

    if let Some(chatgpt_account_id) = params.chatgpt_account_id.as_ref() {
        auth_payload["chatgpt_account_id"] = serde_json::Value::String(chatgpt_account_id.clone());
    }

    let payload = serde_json::json!({
        "email": "user@example.com",
        "email_verified": true,
        "https://api.openai.com/auth": auth_payload,
    });
    let b64 = |b: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
    let header_b64 = b64(&serde_json::to_vec(&header)?);
    let payload_b64 = b64(&serde_json::to_vec(&payload)?);
    let signature_b64 = b64(b"sig");
    Ok(format!("{header_b64}.{payload_b64}.{signature_b64}"))
}

async fn build_config(
    codex_home: &Path,
    forced_login_method: Option<ForcedLoginMethod>,
    forced_chatgpt_workspace_id: Option<Vec<String>>,
) -> AuthConfig {
    AuthConfig {
        codex_home: codex_home.to_path_buf(),
        auth_credentials_store_mode: AuthCredentialsStoreMode::File,
        keyring_backend_kind: AuthKeyringBackendKind::Direct,
        forced_login_method,
        forced_chatgpt_workspace_id,
        chatgpt_base_url: None,
        auth_route_config: crate::test_support::transport_default_auth_route_config(),
    }
}

/// Use sparingly.
/// TODO (gpeal): replace this with an injectable env var provider.
#[cfg(test)]
struct EnvVarGuard {
    key: &'static str,
    original: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let original = env::var_os(key);
        unsafe {
            env::set_var(key, value);
        }
        Self { key, original }
    }

    fn remove(key: &'static str) -> Self {
        let original = env::var_os(key);
        unsafe {
            env::remove_var(key);
        }
        Self { key, original }
    }
}

#[cfg(test)]
impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.original {
                Some(value) => env::set_var(self.key, value),
                None => env::remove_var(self.key),
            }
        }
    }
}

fn remove_access_token_env_var() -> EnvVarGuard {
    EnvVarGuard::remove(CODEX_ACCESS_TOKEN_ENV_VAR)
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn load_auth_reads_access_token_from_env() {
    let codex_home = tempdir().unwrap();
    let mut expected_record = agent_identity_record(WORKSPACE_ID_ALLOWED);
    let agent_identity =
        signed_agent_identity_jwt(&expected_record, json!(expected_record.plan_type))
            .expect("signed agent identity");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/agent-identities/jwks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(test_jwks_body()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/agent/agent-runtime-id/task/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "task_id": "task-123",
        })))
        .expect(1)
        .mount(&server)
        .await;
    expected_record.task_id = Some("task-123".to_string());
    let _access_token_guard = EnvVarGuard::set(CODEX_ACCESS_TOKEN_ENV_VAR, &agent_identity);

    let authapi_base_url = server.uri();
    let chatgpt_base_url = format!("{authapi_base_url}/backend-api");
    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        Some(&chatgpt_base_url),
        AuthKeyringBackendKind::Direct,
        Some(&authapi_base_url),
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect("env auth should load")
    .expect("env auth should be present");

    let CodexAuth::AgentIdentity(agent_identity) = auth else {
        panic!("env auth should load as agent identity");
    };
    assert_eq!(agent_identity.record(), &expected_record);
    assert_eq!(agent_identity.run_task_id(), "task-123");
    assert!(
        !get_auth_file(codex_home.path()).exists(),
        "env auth should not write auth.json"
    );
    server.verify().await;
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn load_auth_reads_personal_access_token_from_env() {
    let codex_home = tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/user-auth-credential/whoami"))
        .and(header("authorization", "Bearer at-env-test"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(personal_access_token_whoami(WORKSPACE_ID_ALLOWED)),
        )
        .expect(2)
        .mount(&server)
        .await;
    let _authapi_guard = EnvVarGuard::set("CODEX_AUTHAPI_BASE_URL", &server.uri());
    let _access_token_guard = EnvVarGuard::set(CODEX_ACCESS_TOKEN_ENV_VAR, "at-env-test");

    for auth_credentials_store_mode in [
        AuthCredentialsStoreMode::File,
        AuthCredentialsStoreMode::Ephemeral,
    ] {
        let auth = super::load_auth(
            codex_home.path(),
            /*enable_codex_api_key_env*/ false,
            auth_credentials_store_mode,
            /*forced_chatgpt_workspace_id*/ None,
            /*chatgpt_base_url*/ None,
            AuthKeyringBackendKind::default(),
            /*agent_identity_authapi_base_url*/ None,
            &crate::test_support::transport_default_auth_route_config(),
        )
        .await
        .expect("env auth should load")
        .expect("env auth should be present");

        assert_eq!(auth.api_auth_mode(), AuthMode::PersonalAccessToken);
        assert_eq!(
            auth.get_token()
                .expect("personal access token should be exposed"),
            "at-env-test"
        );
        assert_eq!(auth.get_account_id().as_deref(), Some(WORKSPACE_ID_ALLOWED));
        assert_eq!(auth.get_chatgpt_user_id().as_deref(), Some("user-123"));
        assert_eq!(
            auth.get_account_email().as_deref(),
            Some("user@example.com")
        );
        assert_eq!(auth.account_plan_type(), Some(AccountPlanType::Business));
        assert!(auth.is_fedramp_account());
    }
    assert!(
        !get_auth_file(codex_home.path()).exists(),
        "env auth should not write auth.json"
    );
    server.verify().await;
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn auth_manager_rejects_env_personal_access_token_workspace_mismatch() {
    let codex_home = tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/user-auth-credential/whoami"))
        .and(header("authorization", "Bearer at-env-workspace-mismatch"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(personal_access_token_whoami(WORKSPACE_ID_DISALLOWED)),
        )
        .expect(1)
        .mount(&server)
        .await;
    let _authapi_guard = EnvVarGuard::set("CODEX_AUTHAPI_BASE_URL", &server.uri());
    let _access_token_guard =
        EnvVarGuard::set(CODEX_ACCESS_TOKEN_ENV_VAR, "at-env-workspace-mismatch");

    let manager = AuthManager::new(
        codex_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        Some(vec![WORKSPACE_ID_ALLOWED.to_string()]),
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        crate::test_support::transport_default_auth_route_config(),
    )
    .await;

    assert_eq!(manager.auth().await, None);
    server.verify().await;
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn auth_manager_rejects_stored_personal_access_token_workspace_mismatch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/user-auth-credential/whoami"))
        .and(header(
            "authorization",
            "Bearer at-stored-workspace-mismatch",
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(personal_access_token_whoami(WORKSPACE_ID_DISALLOWED)),
        )
        .expect(4)
        .mount(&server)
        .await;
    let _authapi_guard = EnvVarGuard::set("CODEX_AUTHAPI_BASE_URL", &server.uri());
    let _access_token_guard = remove_access_token_env_var();

    for auth_credentials_store_mode in [
        AuthCredentialsStoreMode::File,
        AuthCredentialsStoreMode::Ephemeral,
    ] {
        let codex_home = tempdir().unwrap();
        super::login_with_access_token(
            codex_home.path(),
            "at-stored-workspace-mismatch",
            auth_credentials_store_mode,
            /*forced_chatgpt_workspace_id*/ None,
            /*chatgpt_base_url*/ None,
            AuthKeyringBackendKind::default(),
            &crate::test_support::transport_default_auth_route_config(),
        )
        .await
        .expect("personal access token login should succeed");

        let manager = AuthManager::new(
            codex_home.path().to_path_buf(),
            /*enable_codex_api_key_env*/ false,
            auth_credentials_store_mode,
            Some(vec![WORKSPACE_ID_ALLOWED.to_string()]),
            /*chatgpt_base_url*/ None,
            AuthKeyringBackendKind::default(),
            crate::test_support::transport_default_auth_route_config(),
        )
        .await;

        assert_eq!(manager.auth().await, None);
    }
    server.verify().await;
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn personal_access_token_does_not_offer_unauthorized_recovery() {
    let codex_home = tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/user-auth-credential/whoami"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(personal_access_token_whoami(WORKSPACE_ID_ALLOWED)),
        )
        .expect(1)
        .mount(&server)
        .await;
    let _authapi_guard = EnvVarGuard::set("CODEX_AUTHAPI_BASE_URL", &server.uri());
    let _access_token_guard =
        EnvVarGuard::set(CODEX_ACCESS_TOKEN_ENV_VAR, "at-no-unauthorized-recovery");
    let manager = Arc::new(
        AuthManager::new(
            codex_home.path().to_path_buf(),
            /*enable_codex_api_key_env*/ false,
            AuthCredentialsStoreMode::File,
            /*forced_chatgpt_workspace_id*/ None,
            /*chatgpt_base_url*/ None,
            AuthKeyringBackendKind::default(),
            crate::test_support::transport_default_auth_route_config(),
        )
        .await,
    );

    let recovery = manager.unauthorized_recovery();

    assert!(!recovery.has_next());
    assert_eq!(recovery.unavailable_reason(), "not_refreshable_auth");
    manager
        .refresh_token_from_authority()
        .await
        .expect("personal access tokens do not use OAuth refresh");
    server.verify().await;
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn load_auth_keeps_codex_api_key_env_precedence() {
    let codex_home = tempdir().unwrap();
    let record = agent_identity_record(WORKSPACE_ID_ALLOWED);
    let agent_identity = fake_agent_identity_jwt(&record).expect("fake agent identity");
    let _access_token_guard = EnvVarGuard::set(CODEX_ACCESS_TOKEN_ENV_VAR, &agent_identity);
    let _ambient_api_key_guard = EnvVarGuard::set(AMBIENT_API_KEY_ENV_VAR, "ambient-env");
    let _api_key_guard = EnvVarGuard::set(CODEX_API_KEY_ENV_VAR, "sk-env");

    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ true,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        /*agent_identity_authapi_base_url*/ None,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect("env auth should load")
    .expect("env auth should be present");

    assert_eq!(auth.api_key(), Some("sk-env"));
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn load_auth_reads_ambient_api_key_from_env() {
    let codex_home = tempdir().unwrap();
    let record = agent_identity_record(WORKSPACE_ID_ALLOWED);
    let agent_identity = fake_agent_identity_jwt(&record).expect("fake agent identity");
    let _access_token_guard = EnvVarGuard::set(CODEX_ACCESS_TOKEN_ENV_VAR, &agent_identity);
    let _codex_api_key_guard = EnvVarGuard::remove(CODEX_API_KEY_ENV_VAR);
    let _ambient_api_key_guard = EnvVarGuard::set(AMBIENT_API_KEY_ENV_VAR, "ambient-env");

    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ true,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        /*agent_identity_authapi_base_url*/ None,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect("env auth should load")
    .expect("env auth should be present");

    assert_eq!(auth.api_key(), Some("ambient-env"));
}

async fn manager_with_openai_api_key_env(
    codex_home: &Path,
    enable_codex_api_key_env: bool,
) -> AuthManager {
    AuthManager::new_with_openai_api_key_env(
        codex_home.to_path_buf(),
        enable_codex_api_key_env,
        OpenAiApiKeyEnv::Fallback,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        crate::test_support::transport_default_auth_route_config(),
    )
    .await
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn openai_api_key_env_is_used_when_nothing_is_stored() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _codex_api_key_guard = EnvVarGuard::remove(CODEX_API_KEY_ENV_VAR);
    let _ambient_api_key_guard = EnvVarGuard::remove(AMBIENT_API_KEY_ENV_VAR);
    let _openai_api_key_guard = EnvVarGuard::set(OPENAI_API_KEY_ENV_VAR, "sk-openai-env");

    // exec (CODEX_API_KEY enabled) and the TUI/app-server (disabled) agree.
    for enable_codex_api_key_env in [true, false] {
        let manager =
            manager_with_openai_api_key_env(codex_home.path(), enable_codex_api_key_env).await;
        let auth = manager.auth_cached().expect("OPENAI_API_KEY should load");

        assert_eq!(auth.api_key(), Some("sk-openai-env"));
        assert_eq!(auth.api_key_env_var(), Some(OPENAI_API_KEY_ENV_VAR));
        assert_eq!(
            manager.openai_auth_metadata(),
            OpenAiAuthMetadata::EnvironmentApiKey
        );
    }
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn stored_openai_login_takes_precedence_over_openai_api_key_env() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _codex_api_key_guard = EnvVarGuard::remove(CODEX_API_KEY_ENV_VAR);
    let _ambient_api_key_guard = EnvVarGuard::remove(AMBIENT_API_KEY_ENV_VAR);
    let _openai_api_key_guard = EnvVarGuard::set(OPENAI_API_KEY_ENV_VAR, "sk-openai-env");
    login_with_api_key(
        codex_home.path(),
        "sk-stored",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::Direct,
    )
    .expect("seed api key");

    let manager =
        manager_with_openai_api_key_env(codex_home.path(), /*enable_codex_api_key_env*/ true).await;
    let auth = manager.auth_cached().expect("stored auth should load");

    assert_eq!(auth.api_key(), Some("sk-stored"));
    assert_eq!(auth.api_key_env_var(), None);
    assert_eq!(manager.openai_auth_metadata(), OpenAiAuthMetadata::ApiKey);
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn codex_api_key_env_takes_precedence_over_openai_api_key_env() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _ambient_api_key_guard = EnvVarGuard::remove(AMBIENT_API_KEY_ENV_VAR);
    let _codex_api_key_guard = EnvVarGuard::set(CODEX_API_KEY_ENV_VAR, "sk-codex-env");
    let _openai_api_key_guard = EnvVarGuard::set(OPENAI_API_KEY_ENV_VAR, "sk-openai-env");

    let manager =
        manager_with_openai_api_key_env(codex_home.path(), /*enable_codex_api_key_env*/ true).await;
    let auth = manager.auth_cached().expect("CODEX_API_KEY should load");

    assert_eq!(auth.api_key(), Some("sk-codex-env"));
    assert_eq!(auth.api_key_env_var(), Some(CODEX_API_KEY_ENV_VAR));
}

struct TestAuthManagerConfig {
    codex_home: PathBuf,
    forced_login_method: Option<ForcedLoginMethod>,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    forced_chatgpt_workspace_id: Option<Vec<String>>,
    named_session: bool,
    /// `None`: the same as `forced_login_method`.
    configured_forced_login_method: Option<Option<ForcedLoginMethod>>,
}

impl AuthManagerConfig for TestAuthManagerConfig {
    fn codex_home(&self) -> PathBuf {
        self.codex_home.clone()
    }

    fn cli_auth_credentials_store_mode(&self) -> AuthCredentialsStoreMode {
        self.auth_credentials_store_mode
    }

    fn auth_keyring_backend_kind(&self) -> AuthKeyringBackendKind {
        AuthKeyringBackendKind::Direct
    }

    fn forced_chatgpt_workspace_id(&self) -> Option<Vec<String>> {
        self.forced_chatgpt_workspace_id.clone()
    }

    fn chatgpt_base_url(&self) -> String {
        "https://chatgpt.com/backend-api/".to_string()
    }

    fn auth_route_config(&self) -> AuthRouteConfig {
        crate::test_support::transport_default_auth_route_config()
    }

    fn forced_login_method(&self) -> Option<ForcedLoginMethod> {
        self.forced_login_method
    }

    fn runs_on_named_sign_in_account(&self) -> bool {
        self.named_session
    }

    fn configured_forced_login_method(&self) -> Option<ForcedLoginMethod> {
        self.configured_forced_login_method
            .unwrap_or(self.forced_login_method)
    }
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn shared_from_config_reads_openai_api_key_env_unless_chatgpt_login_is_required() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _codex_api_key_guard = EnvVarGuard::remove(CODEX_API_KEY_ENV_VAR);
    let _ambient_api_key_guard = EnvVarGuard::remove(AMBIENT_API_KEY_ENV_VAR);
    let _openai_api_key_guard = EnvVarGuard::set(OPENAI_API_KEY_ENV_VAR, "sk-openai-env");
    let config = |forced_login_method, auth_credentials_store_mode| TestAuthManagerConfig {
        codex_home: codex_home.path().to_path_buf(),
        forced_login_method,
        auth_credentials_store_mode,
        forced_chatgpt_workspace_id: None,
        named_session: false,
        configured_forced_login_method: None,
    };

    for mode in [
        AuthCredentialsStoreMode::File,
        AuthCredentialsStoreMode::Ephemeral,
    ] {
        let manager = AuthManager::shared_from_config(
            &config(None, mode),
            /*enable_codex_api_key_env*/ false,
        )
        .await;
        assert!(manager.openai_api_key_env_enabled());
        assert_eq!(
            manager
                .auth_cached()
                .and_then(|auth| auth.api_key_env_var()),
            Some(OPENAI_API_KEY_ENV_VAR)
        );
    }

    let forced = AuthManager::shared_from_config(
        &config(
            Some(ForcedLoginMethod::Chatgpt),
            AuthCredentialsStoreMode::File,
        ),
        /*enable_codex_api_key_env*/ true,
    )
    .await;
    assert!(!forced.openai_api_key_env_enabled());
    assert!(forced.auth_cached().is_none());

    // Plain constructors (cloud tasks, cloud config) don't read it.
    let plain = AuthManager::new(
        codex_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        crate::test_support::transport_default_auth_route_config(),
    )
    .await;
    assert!(plain.auth_cached().is_none());
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn required_chatgpt_login_ignores_openai_api_key_env() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _codex_api_key_guard = EnvVarGuard::remove(CODEX_API_KEY_ENV_VAR);
    let _ambient_api_key_guard = EnvVarGuard::remove(AMBIENT_API_KEY_ENV_VAR);
    let _openai_api_key_guard = EnvVarGuard::set(OPENAI_API_KEY_ENV_VAR, "sk-openai-env");
    let config = build_config(
        codex_home.path(),
        Some(ForcedLoginMethod::Chatgpt),
        /*forced_chatgpt_workspace_id*/ None,
    )
    .await;

    super::enforce_login_restrictions(&config)
        .await
        .expect("an ignored OPENAI_API_KEY is not a login-method violation");
    assert_eq!(
        OpenAiApiKeyEnv::for_forced_login_method(Some(ForcedLoginMethod::Chatgpt)),
        OpenAiApiKeyEnv::Ignore
    );
    assert_eq!(
        OpenAiApiKeyEnv::for_forced_login_method(Some(ForcedLoginMethod::Api)),
        OpenAiApiKeyEnv::Fallback
    );
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn enforce_login_restrictions_logs_out_for_method_mismatch() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    login_with_api_key(
        codex_home.path(),
        "sk-test",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("seed api key");

    let config = build_config(
        codex_home.path(),
        Some(ForcedLoginMethod::Chatgpt),
        /*forced_chatgpt_workspace_id*/ None,
    )
    .await;

    super::enforce_login_restrictions(&config)
        .await
        .expect("method mismatch should clear auth and continue");
    assert!(
        !codex_home.path().join("auth.json").exists(),
        "auth.json should be removed on mismatch"
    );
}

/// PF-84-S06 (#570): in a process whose session runs on a named sign-in
/// account (so startup left the default login alone), the default login's
/// manager treats a login the restrictions rule out as absent, also after a
/// reload, and never deletes it. Other processes load it as before.
#[tokio::test]
#[serial(codex_auth_env)]
async fn shared_from_config_hides_a_restricted_default_login_only_in_named_sessions() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _codex_api_key_guard = EnvVarGuard::remove(CODEX_API_KEY_ENV_VAR);
    let _ambient_api_key_guard = EnvVarGuard::remove(AMBIENT_API_KEY_ENV_VAR);
    let _openai_api_key_guard = EnvVarGuard::remove(OPENAI_API_KEY_ENV_VAR);
    let auth_file = codex_home.path().join("auth.json");
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some(WORKSPACE_ID_DISALLOWED.to_string()),
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");
    let before = std::fs::read(&auth_file).unwrap();
    let config = |forced_login_method, workspace: &str, named_session| TestAuthManagerConfig {
        codex_home: codex_home.path().to_path_buf(),
        forced_login_method,
        auth_credentials_store_mode: AuthCredentialsStoreMode::File,
        forced_chatgpt_workspace_id: Some(vec![workspace.to_string()]),
        named_session,
        configured_forced_login_method: None,
    };

    for (forced_login_method, workspace) in [
        (Some(ForcedLoginMethod::Api), WORKSPACE_ID_DISALLOWED),
        (None, WORKSPACE_ID_ALLOWED),
    ] {
        let named = AuthManager::shared_from_config(
            &config(forced_login_method, workspace, /*named_session*/ true),
            /*enable_codex_api_key_env*/ false,
        )
        .await;
        assert!(named.auth_cached().is_none());
        assert!(named.hides_restricted_default_login());
        assert!(
            named
                .hidden_default_login_reason()
                .is_some_and(|reason| reason.contains("left unchanged"))
        );
        named.reload().await;
        assert!(named.auth().await.is_none());
        assert!(named.hidden_default_login_reason().is_some());

        let default_session = AuthManager::shared_from_config(
            &config(forced_login_method, workspace, /*named_session*/ false),
            /*enable_codex_api_key_env*/ false,
        )
        .await;
        assert!(default_session.auth_cached().is_some());
        assert_eq!(default_session.hidden_default_login_reason(), None);
    }

    // An allowed login is used as before.
    let allowed = AuthManager::shared_from_config(
        &config(
            Some(ForcedLoginMethod::Chatgpt),
            WORKSPACE_ID_DISALLOWED,
            /*named_session*/ true,
        ),
        /*enable_codex_api_key_env*/ false,
    )
    .await;
    assert!(allowed.auth_cached().is_some());
    assert_eq!(std::fs::read(&auth_file).unwrap(), before);
}

/// #571: a third-party session provider implies an API-key-only login for
/// onboarding (`forced_login_method` reads `api`), but a named OpenAI ChatGPT
/// account is refused only by a configured `forced_login_method = "api"`.
#[tokio::test]
#[serial(codex_auth_env)]
async fn named_chatgpt_account_follows_only_the_configured_login_method() {
    let codex_home = tempdir().unwrap();
    let scratch = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("plus".to_string()),
            chatgpt_account_id: Some(WORKSPACE_ID_ALLOWED.to_string()),
        },
        scratch.path(),
    )
    .expect("failed to write auth file");
    let sign_in: AuthDotJson =
        serde_json::from_slice(&std::fs::read(scratch.path().join("auth.json")).unwrap()).unwrap();
    let work = NamedChatgptAccount {
        provider_id: "openai".to_string(),
        name: "work".to_string(),
    };
    crate::auth::save_named_account_auth(
        codex_home.path(),
        &work,
        &sign_in,
        /*must_be_new*/ true,
    )
    .expect("named sign-in");
    let config = |configured| TestAuthManagerConfig {
        codex_home: codex_home.path().to_path_buf(),
        forced_login_method: Some(ForcedLoginMethod::Api),
        auth_credentials_store_mode: AuthCredentialsStoreMode::File,
        forced_chatgpt_workspace_id: None,
        named_session: false,
        configured_forced_login_method: Some(configured),
    };

    let implied = AuthManager::shared_from_config(
        &config(/*configured*/ None),
        /*enable_codex_api_key_env*/ false,
    )
    .await;
    assert!(
        implied
            .for_named_account(work.clone())
            .auth_cached()
            .is_some_and(|auth| auth.is_chatgpt_auth())
    );
    let configured = AuthManager::shared_from_config(
        &config(Some(ForcedLoginMethod::Api)),
        /*enable_codex_api_key_env*/ false,
    )
    .await;
    let refused = configured.for_named_account(work);
    assert!(refused.auth().await.is_none());
    assert!(
        refused
            .named_sign_in_unavailable()
            .is_some_and(|reason| reason.contains("requires an API key")),
        "{:?}",
        refused.named_sign_in_unavailable()
    );
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn enforce_login_restrictions_logs_out_for_workspace_mismatch() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some(WORKSPACE_ID_DISALLOWED.to_string()),
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let config = build_config(
        codex_home.path(),
        /*forced_login_method*/ None,
        Some(vec![WORKSPACE_ID_ALLOWED.to_string()]),
    )
    .await;

    let err = super::enforce_login_restrictions(&config)
        .await
        .expect_err("expected workspace mismatch to error");
    assert!(
        err.to_string()
            .contains(&format!("workspace(s) {WORKSPACE_ID_ALLOWED}"))
    );
    assert!(
        !codex_home.path().join("auth.json").exists(),
        "auth.json should be removed on mismatch"
    );
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn enforce_login_restrictions_logs_out_for_personal_access_token_workspace_mismatch() {
    let codex_home = tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/user-auth-credential/whoami"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(personal_access_token_whoami(WORKSPACE_ID_DISALLOWED)),
        )
        .expect(2)
        .mount(&server)
        .await;
    let _access_token_guard = remove_access_token_env_var();
    let _authapi_guard = EnvVarGuard::set("CODEX_AUTHAPI_BASE_URL", &server.uri());
    super::login_with_access_token(
        codex_home.path(),
        "at-workspace-mismatch",
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect("personal access token login should succeed");

    let config = AuthConfig {
        codex_home: codex_home.path().to_path_buf(),
        auth_credentials_store_mode: AuthCredentialsStoreMode::File,
        keyring_backend_kind: AuthKeyringBackendKind::default(),
        forced_login_method: None,
        forced_chatgpt_workspace_id: Some(vec![WORKSPACE_ID_ALLOWED.to_string()]),
        chatgpt_base_url: None,
        auth_route_config: crate::test_support::transport_default_auth_route_config(),
    };

    let err = super::enforce_login_restrictions(&config)
        .await
        .expect_err("expected workspace mismatch to error");
    assert!(err.to_string().contains(&format!(
        "current credentials belong to {WORKSPACE_ID_DISALLOWED}"
    )));
    assert!(
        !codex_home.path().join("auth.json").exists(),
        "auth.json should be removed on mismatch"
    );
    server.verify().await;
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn enforce_login_restrictions_allows_matching_workspace() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some(WORKSPACE_ID_ALLOWED.to_string()),
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let config = build_config(
        codex_home.path(),
        /*forced_login_method*/ None,
        Some(vec![WORKSPACE_ID_ALLOWED.to_string()]),
    )
    .await;

    super::enforce_login_restrictions(&config)
        .await
        .expect("matching workspace should succeed");
    assert!(
        codex_home.path().join("auth.json").exists(),
        "auth.json should remain when restrictions pass"
    );
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn enforce_login_restrictions_allows_any_matching_workspace_in_list() {
    let codex_home = tempdir().unwrap();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: Some(WORKSPACE_ID_ALLOWED.to_string()),
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let config = build_config(
        codex_home.path(),
        /*forced_login_method*/ None,
        Some(vec![
            WORKSPACE_ID_SECOND_ALLOWED.to_string(),
            WORKSPACE_ID_ALLOWED.to_string(),
        ]),
    )
    .await;

    super::enforce_login_restrictions(&config)
        .await
        .expect("any matching workspace in the allowed list should succeed");
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn enforce_login_restrictions_logs_out_for_agent_identity_workspace_mismatch() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let record = agent_identity_record(WORKSPACE_ID_DISALLOWED);
    let agent_identity =
        signed_agent_identity_jwt(&record, json!(record.plan_type)).expect("signed agent identity");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/agent-identities/jwks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(test_jwks_body()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/agent/agent-runtime-id/task/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "task_id": "task-123",
        })))
        .expect(1)
        .mount(&server)
        .await;
    let authapi_base_url = server.uri();
    let chatgpt_base_url = format!("{authapi_base_url}/backend-api");
    save_auth(
        codex_home.path(),
        &AuthDotJson {
            auth_mode: Some(AuthMode::AgentIdentity),
            openai_api_key: None,
            tokens: None,
            last_refresh: None,
            agent_identity: Some(AgentIdentityStorage::Jwt(agent_identity)),
            personal_access_token: None,
            bedrock_api_key: None,
        },
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("seed agent identity auth");

    let config = AuthConfig {
        codex_home: codex_home.path().to_path_buf(),
        auth_credentials_store_mode: AuthCredentialsStoreMode::File,
        keyring_backend_kind: AuthKeyringBackendKind::Direct,
        forced_login_method: None,
        forced_chatgpt_workspace_id: Some(vec![WORKSPACE_ID_ALLOWED.to_string()]),
        chatgpt_base_url: Some(chatgpt_base_url),
        auth_route_config: crate::test_support::transport_default_auth_route_config(),
    };

    let err = super::enforce_login_restrictions_with_agent_identity_authapi_base_url(
        &config,
        Some(&authapi_base_url),
    )
    .await
    .expect_err("expected workspace mismatch to error");
    let message = err.to_string();
    assert!(
        message.contains(&format!(
            "current credentials belong to {WORKSPACE_ID_DISALLOWED}"
        )),
        "{message}"
    );
    assert!(
        !codex_home.path().join("auth.json").exists(),
        "auth.json should be removed on mismatch"
    );
    server.verify().await;
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn enforce_login_restrictions_allows_api_key_if_login_method_not_set_but_forced_chatgpt_workspace_id_is_set()
 {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    login_with_api_key(
        codex_home.path(),
        "sk-test",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("seed api key");

    let config = build_config(
        codex_home.path(),
        /*forced_login_method*/ None,
        Some(vec![WORKSPACE_ID_ALLOWED.to_string()]),
    )
    .await;

    super::enforce_login_restrictions(&config)
        .await
        .expect("matching workspace should succeed");
    assert!(
        codex_home.path().join("auth.json").exists(),
        "auth.json should remain when restrictions pass"
    );
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn enforce_login_restrictions_blocks_env_api_key_when_chatgpt_required() {
    let _guard = EnvVarGuard::set(CODEX_API_KEY_ENV_VAR, "sk-env");
    let _access_token_guard = remove_access_token_env_var();
    let codex_home = tempdir().unwrap();

    let config = build_config(
        codex_home.path(),
        Some(ForcedLoginMethod::Chatgpt),
        /*forced_chatgpt_workspace_id*/ None,
    )
    .await;

    let err = super::enforce_login_restrictions(&config)
        .await
        .expect_err("environment API key should not satisfy forced ChatGPT login");
    assert!(
        err.to_string()
            .contains("ChatGPT login is required, but an API key is currently being used.")
    );
}

fn agent_identity_record(account_id: &str) -> AgentIdentityAuthRecord {
    let key_material =
        codex_agent_identity::generate_agent_key_material().expect("generate agent key material");
    AgentIdentityAuthRecord {
        agent_runtime_id: "agent-runtime-id".to_string(),
        agent_private_key: key_material.private_key_pkcs8_base64,
        account_id: account_id.to_string(),
        chatgpt_user_id: "user-id".to_string(),
        email: Some("user@example.com".to_string()),
        plan_type: AccountPlanType::Pro,
        chatgpt_account_is_fedramp: false,
        task_id: None,
    }
}

async fn mock_agent_task_registration(
    server: &MockServer,
    path_prefix: &str,
    agent_runtime_id: &str,
    task_id: &str,
) {
    Mock::given(method("POST"))
        .and(path(format!(
            "{path_prefix}/v1/agent/{agent_runtime_id}/task/register"
        )))
        .respond_with(ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
            "task_id": task_id,
        })))
        .expect(/*r*/ 1)
        .mount(server)
        .await;
}

fn fake_agent_identity_jwt(record: &AgentIdentityAuthRecord) -> std::io::Result<String> {
    fake_agent_identity_jwt_with_plan_type(record, serde_json::to_value(record.plan_type)?)
}

fn fake_agent_identity_jwt_with_plan_type(
    record: &AgentIdentityAuthRecord,
    plan_type: serde_json::Value,
) -> std::io::Result<String> {
    let encode = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let header_b64 = encode(br#"{"alg":"EdDSA","typ":"JWT"}"#);
    let payload = json!({
        "iss": "https://chatgpt.com/codex-backend/agent-identity",
        "aud": "codex-app-server",
        "iat": 1_700_000_000usize,
        "exp": 4_000_000_000usize,
        "agent_runtime_id": record.agent_runtime_id,
        "agent_private_key": record.agent_private_key,
        "account_id": record.account_id,
        "chatgpt_user_id": record.chatgpt_user_id,
        "email": record.email,
        "plan_type": plan_type,
        "chatgpt_account_is_fedramp": record.chatgpt_account_is_fedramp,
    });
    let payload_b64 = encode(&serde_json::to_vec(&payload)?);
    let signature_b64 = encode(b"sig");
    Ok(format!("{header_b64}.{payload_b64}.{signature_b64}"))
}

fn signed_agent_identity_jwt(
    record: &AgentIdentityAuthRecord,
    plan_type: serde_json::Value,
) -> jsonwebtoken::errors::Result<String> {
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some("test-key".to_string());
    jsonwebtoken::encode(
        &header,
        &json!({
            "iss": "https://chatgpt.com/codex-backend/agent-identity",
            "aud": "codex-app-server",
            "iat": 1_700_000_000usize,
            "exp": 4_000_000_000usize,
            "agent_runtime_id": record.agent_runtime_id,
            "agent_private_key": record.agent_private_key,
            "account_id": record.account_id,
            "chatgpt_user_id": record.chatgpt_user_id,
            "email": record.email,
            "plan_type": plan_type,
            "chatgpt_account_is_fedramp": record.chatgpt_account_is_fedramp,
        }),
        &jsonwebtoken::EncodingKey::from_rsa_pem(TEST_AGENT_IDENTITY_RSA_PRIVATE_KEY_PEM)?,
    )
}

fn test_jwks_body() -> serde_json::Value {
    json!({
        "keys": [{
            "kty": "RSA",
            "kid": "test-key",
            "use": "sig",
            "alg": "RS256",
            "n": "1qQF2MqTrGAMDm7wXbjJP5sWqGA83tAGUs2ksy7iJXLJdhCg4AtwGm4SFl4f6kxhCSzlN1QdXuZjvRT2wZZiGUi9xUE28rf4WLrTxSnwqLuTy5knMP08yC0t_0YU_FGPZMcWb14hG05IvZr8UbmRaVagxSR8H4rSIymRoVwwmFSrqz068XrWGSYNIfLEASyo5GdAaqmk1JALINHgYGQJVxMxtwcvDxoVKmC7eltUNymMNBZhsv4E8sx9YNLpBoEibznfEpDU_DGzrM5eZCsQzaqbhBOlGd427ifud_Nnd9cPqzgCUc23-0FXSPfpbgksCXAwAmD0OFjQWrgqVdKL6Q",
            "e": "AQAB",
        }]
    })
}

fn personal_access_token_whoami(account_id: &str) -> serde_json::Value {
    json!({
        "email": "user@example.com",
        "chatgpt_user_id": "user-123",
        "chatgpt_account_id": account_id,
        "chatgpt_plan_type": "business",
        "chatgpt_account_is_fedramp": true,
    })
}

const TEST_AGENT_IDENTITY_RSA_PRIVATE_KEY_PEM: &[u8] = br#"-----BEGIN PRIVATE KEY-----
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQDWpAXYypOsYAwO
bvBduMk/mxaoYDze0AZSzaSzLuIlcsl2EKDgC3AabhIWXh/qTGEJLOU3VB1e5mO9
FPbBlmIZSL3FQTbyt/hYutPFKfCou5PLmScw/TzILS3/RhT8UY9kxxZvXiEbTki9
mvxRuZFpVqDFJHwfitIjKZGhXDCYVKurPTrxetYZJg0h8sQBLKjkZ0BqqaTUkAsg
0eBgZAlXEzG3By8PGhUqYLt6W1Q3KYw0FmGy/gTyzH1g0ukGgSJvOd8SkNT8MbOs
zl5kKxDNqpuEE6UZ3jbuJ+5382d31w+rOAJRzbf7QVdI9+luCSwJcDACYPQ4WNBa
uCpV0ovpAgMBAAECggEAVu84LwZdqYN9XpswX8VoPYrjMm9IODapWQBRpQFoNyK2
1ksF3bjEPvA2Azk8U/l7k+vLKw22l6lY3EyRZPcz5GnB8xLm3ogE3mtNOp4yCyVu
RxhQ91aaN7mU17/a4BdorLi2LYVCg3zBmYociD1Q2AluNGsCmwPu+K7tfR2J0Sg8
NjqiTbDG1XDpR/icwgC9t6vh8lZpCHDhF4tbQfLLVLeA/OdcuzXDyMCXbmdVIdBQ
rm4aIFmr2e1/2ctTbCg85S6AGFTH+pSLjrwTzyvf+F6NW5uNjLQAQLFj+EznBDxj
Xdx90cySrjsKK6PVWQF4RiTvkSW8eWL7R6B2FZbGwQKBgQDuVQRj72hWloR7mbEL
aUEEv3pIXTMXWEsoMBNczos/1L1RnAN1AI44TurznasPZAWvQj+kVbLDR+TAeZrL
iA8HIWswQUI18hFmgKzSkwIXGtubcKVrgsKeS4lMDKCM/Ef6WAYdeq6ronoY5lCN
YrJFmGp81W5zcV7lyiycgbSiGwKBgQDmjWYf6pZjrK7Z+OJ3X1AZfi2vss15SCvL
3fPgzIDbViztpGyQhc3DQZIsBNIu0xZp/veGce9TEeTds2ro9NfdJFeou8+fC7Pq
sOsM3amGFFi+ZW/9BWyjZEM88bgWWAjqLHbpfHDxjAf5CSxddqxgHlbP0Ytyb1Vg
gmPDn9YKSwKBgQDbTi3hC35WFuDHn0/zcSHcDZmnFuOZeqyFyV83yfMGhGrEuqvP
sPgtRikajJ3IZsB4WZyYSidZXEFY/0z6NjOl2xF38MTNQPbT/FmK1q1Yt2UWrlv5
BvSwlk87RG9D7C0LZo4R+D7cPoDdgqjiwMvMEIkEX5zn641oI1ZTmWKuuwKBgQCD
KF+3unnRvHRAVoFnTZbA2fJdqMeRvogD04GhGlYX8V9f1hFY6nXTJaNlXVzA/J8c
r8ra9kgjJuPfZ+ljG58OFFW2DRohLcQtuHYPfK6rMzoFHqnl9EcIcMp7ijuionR3
29HOJFgQYgxLFXfit9d6WugiE+BTupiEbckZif13HwKBgE/lAlkVHP6YahOO2Ljc
J1bwkqKZTB5dHolX9A58e/xXnfZ5P8f3Z83+Izap3FwqQulk7b1WO1MQcHuVg2NN
5da0D4h2rYOXnbYIg0BVu4spQbaM6ewsp66b8+MzLOBvj8SzWdt1Oyw0q/MRyQAR
8U4M2TSWCKUY/A6sT4W8+mT9
-----END PRIVATE KEY-----"#;

#[tokio::test]
#[serial(codex_auth_env)]
async fn agent_identity_plan_type_maps_raw_enterprise_alias() {
    assert_agent_identity_plan_alias(json!("hc"), AccountPlanType::Enterprise).await;
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn agent_identity_plan_type_maps_raw_education_alias() {
    assert_agent_identity_plan_alias(json!("education"), AccountPlanType::Edu).await;
}

async fn assert_agent_identity_plan_alias(
    plan_type: serde_json::Value,
    expected_plan_type: AccountPlanType,
) {
    let record = agent_identity_record("account-id");
    let jwt = signed_agent_identity_jwt(&record, plan_type).expect("agent identity jwt");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/backend-api/wham/agent-identities/jwks"))
        .respond_with(ResponseTemplate::new(200).set_body_json(test_jwks_body()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/agent/agent-runtime-id/task/register"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "task_id": "task-123",
        })))
        .expect(1)
        .mount(&server)
        .await;
    let authapi_base_url = server.uri();
    let chatgpt_base_url = format!("{authapi_base_url}/backend-api");
    let auth = CodexAuth::from_agent_identity_jwt_with_authapi_base_url(
        &jwt,
        Some(&chatgpt_base_url),
        &authapi_base_url,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect("agent identity auth");

    pretty_assertions::assert_eq!(auth.account_plan_type(), Some(expected_plan_type));
    server.verify().await;
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn plan_type_maps_known_plan() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("pro".to_string()),
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        /*agent_identity_authapi_base_url*/ None,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect("load auth")
    .expect("auth available");

    pretty_assertions::assert_eq!(auth.account_plan_type(), Some(AccountPlanType::Pro));
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn plan_type_maps_self_serve_business_usage_based_plan() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("self_serve_business_usage_based".to_string()),
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        /*agent_identity_authapi_base_url*/ None,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect("load auth")
    .expect("auth available");

    pretty_assertions::assert_eq!(
        auth.account_plan_type(),
        Some(AccountPlanType::SelfServeBusinessUsageBased)
    );
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn plan_type_maps_enterprise_cbp_usage_based_plan() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("enterprise_cbp_usage_based".to_string()),
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        /*agent_identity_authapi_base_url*/ None,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect("load auth")
    .expect("auth available");

    pretty_assertions::assert_eq!(
        auth.account_plan_type(),
        Some(AccountPlanType::EnterpriseCbpUsageBased)
    );
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn plan_type_maps_unknown_to_unknown() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: Some("mystery-tier".to_string()),
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        /*agent_identity_authapi_base_url*/ None,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect("load auth")
    .expect("auth available");

    pretty_assertions::assert_eq!(auth.account_plan_type(), Some(AccountPlanType::Unknown));
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn missing_plan_type_maps_to_unknown() {
    let codex_home = tempdir().unwrap();
    let _access_token_guard = remove_access_token_env_var();
    let _jwt = write_auth_file(
        AuthFileParams {
            openai_api_key: None,
            chatgpt_plan_type: None,
            chatgpt_account_id: None,
        },
        codex_home.path(),
    )
    .expect("failed to write auth file");

    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::Direct,
        /*agent_identity_authapi_base_url*/ None,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect("load auth")
    .expect("auth available");

    pretty_assertions::assert_eq!(auth.account_plan_type(), Some(AccountPlanType::Unknown));
}

#[tokio::test]
#[serial(codex_auth_env)]
async fn provider_api_key_login_is_provider_scoped_and_not_primary_auth() {
    let _access_token_guard = remove_access_token_env_var();
    let _codex_api_key_guard = EnvVarGuard::remove(CODEX_API_KEY_ENV_VAR);
    let _ambient_api_key_guard = EnvVarGuard::remove(AMBIENT_API_KEY_ENV_VAR);
    let _zai_api_key_guard = EnvVarGuard::remove("ZAI_API_KEY");
    let codex_home = tempdir().unwrap();

    super::login_with_provider_api_key(
        codex_home.path(),
        "ZAI_API_KEY",
        "zai-test-key",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("provider API key should save");

    let stored = super::provider_api_key_from_auth_storage(
        codex_home.path(),
        "ZAI_API_KEY",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("provider API key should load");
    assert_eq!(stored.as_deref(), Some("zai-test-key"));

    let auth = super::load_auth(
        codex_home.path(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        /*agent_identity_authapi_base_url*/ None,
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect("primary auth load should succeed");
    assert!(auth.is_none());

    let auth_manager = AuthManager::new(
        codex_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        crate::test_support::transport_default_auth_route_config(),
    )
    .await;
    assert!(auth_manager.auth_cached().is_none());
    assert_eq!(
        auth_manager
            .provider_api_key("ZAI_API_KEY")
            .expect("provider API key should resolve")
            .as_deref(),
        Some("zai-test-key")
    );

    super::login_with_provider_api_key(
        codex_home.path(),
        "ZAI_API_KEY",
        "zai-updated-key",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("provider API key update should save");
    assert_eq!(
        auth_manager
            .provider_api_key("ZAI_API_KEY")
            .expect("updated provider API key should resolve")
            .as_deref(),
        Some("zai-updated-key")
    );

    assert!(
        !super::logout(
            codex_home.path(),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("logout should preserve provider auth")
    );
    assert_eq!(
        super::provider_api_key_from_auth_storage(
            codex_home.path(),
            "ZAI_API_KEY",
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("provider auth storage should still be readable"),
        Some("zai-updated-key".to_string())
    );

    assert!(
        super::logout_all_credentials(
            codex_home.path(),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("destructive logout should remove provider auth")
    );
    assert_eq!(
        super::provider_api_key_from_auth_storage(
            codex_home.path(),
            "ZAI_API_KEY",
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("provider auth storage should still be readable"),
        None
    );
    assert_eq!(
        auth_manager
            .provider_api_key("ZAI_API_KEY")
            .expect("provider API key cache should observe credential removal"),
        None
    );
}

#[test]
fn provider_api_key_metadata_reports_legacy_missing_and_suppressed_without_secret() {
    let codex_home = tempdir().expect("tempdir");
    super::legacy_save_provider_key(codex_home.path(), "CUSTOM_API_KEY", "secret-canary")
        .expect("seed legacy provider key");

    assert_eq!(
        super::provider_api_key_metadata_from_auth_storage(codex_home.path(), "CUSTOM_API_KEY")
            .expect("inspect legacy provider key"),
        ProviderApiKeyStorageMetadata::Stored {
            source: ProviderApiKeyStorageSource::LegacyPlaintext,
        }
    );
    assert_eq!(
        super::provider_api_key_metadata_from_auth_storage(codex_home.path(), "MISSING_API_KEY")
            .expect("inspect missing provider key"),
        ProviderApiKeyStorageMetadata::Missing
    );

    super::suppress_provider_api_key(codex_home.path(), "CUSTOM_API_KEY")
        .expect("suppress provider key");
    let metadata =
        super::provider_api_key_metadata_from_auth_storage(codex_home.path(), "CUSTOM_API_KEY")
            .expect("inspect suppressed provider key");
    assert_eq!(metadata, ProviderApiKeyStorageMetadata::Suppressed);
    assert!(!format!("{metadata:?}").contains("secret-canary"));
}

#[test]
fn provider_api_key_metadata_snapshot_matches_single_key_precedence_without_secret() {
    let codex_home = tempdir().expect("tempdir");
    let canary = "bulk-metadata-secret-canary";
    super::legacy_save_provider_key(codex_home.path(), "LEGACY_API_KEY", canary)
        .expect("seed legacy provider key");
    super::legacy_save_provider_key(codex_home.path(), "SUPPRESSED_API_KEY", "suppressed-secret")
        .expect("seed provider key to suppress");
    super::suppress_provider_api_key(codex_home.path(), "SUPPRESSED_API_KEY")
        .expect("suppress provider key");

    let provider_key_ids = ["LEGACY_API_KEY", "SUPPRESSED_API_KEY", "MISSING_API_KEY"];
    let snapshot = super::provider_api_key_metadata_snapshot_from_auth_storage(
        codex_home.path(),
        provider_key_ids,
    )
    .expect("inspect provider-key snapshot");
    for provider_key_id in provider_key_ids {
        assert_eq!(
            snapshot.get(provider_key_id),
            Some(
                super::provider_api_key_metadata_from_auth_storage(
                    codex_home.path(),
                    provider_key_id,
                )
                .expect("inspect single provider key"),
            ),
        );
    }
    assert!(!format!("{snapshot:?}").contains(canary));
    assert!(!format!("{snapshot:?}").contains("suppressed-secret"));
}

#[test]
fn provider_api_key_metadata_snapshot_is_io_free_when_empty_and_redacts_parse_errors() {
    let codex_home = tempdir().expect("tempdir");
    let canary = "malformed-secret-canary";
    std::fs::write(
        codex_home.path().join("provider_auth.json"),
        format!("{{ malformed {canary}"),
    )
    .expect("write malformed provider auth");

    let empty = super::provider_api_key_metadata_snapshot_from_auth_storage(
        codex_home.path(),
        std::iter::empty::<&str>(),
    )
    .expect("empty snapshot must not touch storage");
    assert_eq!(empty.get("ANY_API_KEY"), None);

    let snapshot_error = super::provider_api_key_metadata_snapshot_from_auth_storage(
        codex_home.path(),
        ["ANY_API_KEY"],
    )
    .expect_err("nonempty snapshot must report malformed legacy metadata");
    let single_error =
        super::provider_api_key_metadata_from_auth_storage(codex_home.path(), "ANY_API_KEY")
            .expect_err("single lookup must report malformed legacy metadata");
    assert_eq!(snapshot_error.kind(), single_error.kind());
    assert!(!format!("{snapshot_error:?}").contains(canary));
}

#[test]
fn provider_api_key_metadata_snapshot_matches_encrypted_vault_without_secret() {
    let codex_home = tempdir().expect("tempdir");
    let canary = "encrypted-bulk-secret-canary";
    super::login_with_provider_api_key(
        codex_home.path(),
        "ENCRYPTED_API_KEY",
        canary,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("store encrypted provider key");

    let snapshot = super::provider_api_key_metadata_snapshot_from_auth_storage(
        codex_home.path(),
        ["ENCRYPTED_API_KEY"],
    )
    .expect("inspect encrypted provider-key snapshot");
    let single =
        super::provider_api_key_metadata_from_auth_storage(codex_home.path(), "ENCRYPTED_API_KEY")
            .expect("inspect single encrypted provider key");
    assert_eq!(snapshot.get("ENCRYPTED_API_KEY"), Some(single));
    assert_eq!(
        single,
        ProviderApiKeyStorageMetadata::Stored {
            source: ProviderApiKeyStorageSource::EncryptedVault,
        }
    );
    assert!(!format!("{snapshot:?}").contains(canary));
}

#[test]
fn openai_auth_metadata_distinguishes_account_api_key_and_unsupported_auth() {
    let account =
        AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
    let api_key = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("secret-canary"));
    let bedrock = AuthManager::from_auth_for_testing(CodexAuth::BedrockApiKey(BedrockApiKeyAuth {
        api_key: "bedrock-secret".to_string(),
        region: "us-east-1".to_string(),
    }));

    assert_eq!(
        [
            account.openai_auth_metadata(),
            api_key.openai_auth_metadata(),
            bedrock.openai_auth_metadata(),
        ],
        [
            OpenAiAuthMetadata::Account,
            OpenAiAuthMetadata::ApiKey,
            OpenAiAuthMetadata::Unsupported,
        ]
    );
}

#[test]
fn deleting_one_provider_api_key_preserves_other_legacy_credentials() {
    let codex_home = tempdir().expect("tempdir");
    for (provider, key) in [
        ("PFTERMINAL_PLAN_API_KEY", "pft-plan-secret"),
        (AMBIENT_API_KEY_ENV_VAR, "ambient-secret"),
    ] {
        super::legacy_save_provider_key(codex_home.path(), provider, key)
            .expect("seed legacy provider key");
    }

    assert!(
        super::delete_provider_api_key(codex_home.path(), "PFTERMINAL_PLAN_API_KEY")
            .expect("delete plan key")
    );
    assert_eq!(
        super::legacy_provider_key(codex_home.path(), "PFTERMINAL_PLAN_API_KEY")
            .expect("deleted plan key"),
        None
    );
    assert_eq!(
        super::legacy_provider_key(codex_home.path(), AMBIENT_API_KEY_ENV_VAR)
            .expect("unrelated provider key")
            .as_deref(),
        Some("ambient-secret")
    );
}

#[test]
fn provider_vault_label_maps_back_to_cache_key_without_accepting_arbitrary_labels() {
    assert_eq!(
        super::provider_api_key_id_from_vault_label("provider/deepseek_api_key"),
        Some("DEEPSEEK_API_KEY".to_string())
    );
    assert_eq!(
        super::provider_api_key_id_from_vault_label(" provider/openrouter_api_key "),
        Some("OPENROUTER_API_KEY".to_string())
    );
    assert_eq!(
        super::provider_api_key_id_from_vault_label("provider/../../auth.json"),
        None
    );
    assert_eq!(
        super::provider_api_key_id_from_vault_label("qa/disposable"),
        None
    );
}

#[test]
fn deleting_provider_key_suppresses_an_unreadable_vault_until_explicit_relogin() {
    let codex_home = tempdir().expect("tempdir");
    super::legacy_save_provider_key(
        codex_home.path(),
        "PFTERMINAL_PLAN_API_KEY",
        "legacy-plan-key",
    )
    .expect("seed legacy provider key");

    let secrets_dir = codex_home.path().join("secrets");
    std::fs::create_dir_all(&secrets_dir).expect("create secrets dir");
    std::fs::write(secrets_dir.join("local.age"), b"not-an-age-file")
        .expect("write unreadable vault fixture");

    assert!(
        super::delete_provider_api_key(codex_home.path(), "PFTERMINAL_PLAN_API_KEY")
            .expect("an unavailable vault must not block effective deletion")
    );
    let provider_auth: serde_json::Value = serde_json::from_slice(
        &std::fs::read(codex_home.path().join("provider_auth.json")).expect("deletion marker file"),
    )
    .expect("deletion marker JSON");
    assert_eq!(
        provider_auth["deleted_api_keys"],
        serde_json::json!(["PFTERMINAL_PLAN_API_KEY"]),
        "vault failure must leave a durable suppression marker"
    );
    assert_eq!(
        super::provider_api_key_from_auth_storage(
            codex_home.path(),
            "PFTERMINAL_PLAN_API_KEY",
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("deleted key lookup"),
        None
    );

    super::login_with_provider_api_key(
        codex_home.path(),
        "PFTERMINAL_PLAN_API_KEY",
        "replacement-plan-key",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("explicit login should clear the deletion marker");
    assert_eq!(
        super::provider_api_key_from_auth_storage(
            codex_home.path(),
            "PFTERMINAL_PLAN_API_KEY",
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("replacement key lookup")
        .as_deref(),
        Some("replacement-plan-key")
    );
}

#[test]
fn suppressing_provider_key_is_immediate_and_explicit_relogin_clears_it() {
    let codex_home = tempdir().expect("codex home");
    super::login_with_provider_api_key(
        codex_home.path(),
        "PFTERMINAL_PLAN_API_KEY",
        "plan-key",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("store provider key");

    assert!(
        super::suppress_provider_api_key(codex_home.path(), "PFTERMINAL_PLAN_API_KEY")
            .expect("suppress provider key")
    );
    assert_eq!(
        super::provider_api_key_from_auth_storage(
            codex_home.path(),
            "PFTERMINAL_PLAN_API_KEY",
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("read suppressed provider key"),
        None
    );

    super::login_with_provider_api_key(
        codex_home.path(),
        "PFTERMINAL_PLAN_API_KEY",
        "replacement-plan-key",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("replace provider key");
    assert_eq!(
        super::provider_api_key_from_auth_storage(
            codex_home.path(),
            "PFTERMINAL_PLAN_API_KEY",
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("read replacement provider key")
        .as_deref(),
        Some("replacement-plan-key")
    );
}

#[test]
fn legacy_provider_auth_replacements_are_atomic_and_leave_no_temporary_file() {
    let codex_home = tempdir().expect("tempdir");
    super::legacy_save_provider_key(codex_home.path(), "FIRST_API_KEY", "first-secret")
        .expect("save first key");
    super::legacy_save_provider_key(codex_home.path(), "SECOND_API_KEY", "second-secret")
        .expect("replace provider auth atomically");

    assert_eq!(
        super::legacy_provider_key(codex_home.path(), "FIRST_API_KEY")
            .expect("read first key")
            .as_deref(),
        Some("first-secret")
    );
    assert_eq!(
        super::legacy_provider_key(codex_home.path(), "SECOND_API_KEY")
            .expect("read second key")
            .as_deref(),
        Some("second-secret")
    );
    let entries = std::fs::read_dir(codex_home.path())
        .expect("read codex home")
        .map(|entry| entry.expect("directory entry").file_name())
        .collect::<Vec<_>>();
    assert_eq!(
        entries,
        vec![std::ffi::OsString::from("provider_auth.json")]
    );
}
