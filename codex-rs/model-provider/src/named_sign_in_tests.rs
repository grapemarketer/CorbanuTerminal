//! PF-84-S06 slice B: a provider on a named OpenAI account uses only that
//! account's sign-in, and a provider on the default account never borrows a
//! named one (a session swapped to a named manager switching providers).
//! Runs under the isolated test runner (`CORBANU_TEST_NO_NATIVE_KEYRING=1`).

use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::NamedChatgptAccount;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::NamedProviderAccount;
use pretty_assertions::assert_eq;

use crate::provider::create_model_provider;

fn openai(account: Option<&str>) -> ModelProviderInfo {
    let mut info = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
    info.account = account.map(|name| NamedProviderAccount {
        provider_id: "openai".to_string(),
        name: name.to_string(),
    });
    info
}

#[tokio::test]
async fn named_and_default_openai_providers_never_mix_sign_ins() {
    let home = std::env::temp_dir().join(format!("pf84-s06-mix-{}", std::process::id()));
    std::fs::create_dir_all(&home).expect("home");
    let work = NamedChatgptAccount {
        provider_id: "openai".to_string(),
        name: "work".to_string(),
    };
    codex_login::login_named_account_with_api_key(&home, &work, "sk-work-canary")
        .expect("named sign-in");
    let base = AuthManager::from_auth_for_testing_with_home(
        CodexAuth::from_api_key("sk-default-canary"),
        home.clone(),
    );
    let key = |auth: Option<CodexAuth>| auth.and_then(|auth| auth.api_key().map(str::to_string));

    let named = create_model_provider(openai(Some("work")), Some(base.clone()));
    let swapped = base.for_named_account(work);
    let default_on_swapped = create_model_provider(openai(/*account*/ None), Some(swapped));
    let missing = create_model_provider(openai(Some("missing")), Some(base));
    assert_eq!(
        (
            key(named.auth().await),
            key(default_on_swapped.auth().await),
            key(missing.auth().await),
        ),
        (
            Some("sk-work-canary".to_string()),
            Some("sk-default-canary".to_string()),
            None,
        )
    );
    let _ = std::fs::remove_dir_all(&home);
}

/// #571: a provider on a named sign-in account that has no usable sign-in
/// refuses the request with the reason instead of sending it without
/// credentials.
#[tokio::test]
async fn named_openai_account_without_a_sign_in_is_refused_not_sent_unauthenticated() {
    let home = std::env::temp_dir().join(format!("pf84-s06-unsigned-{}", std::process::id()));
    std::fs::create_dir_all(&home).expect("home");
    let base = AuthManager::from_auth_for_testing_with_home(
        CodexAuth::from_api_key("sk-default-canary"),
        home.clone(),
    );
    let provider = create_model_provider(openai(Some("missing")), Some(base));
    let Err(error) = provider.api_auth().await else {
        panic!("a named account without a sign-in must not authorize a request");
    };
    let message = error.to_string();
    assert!(
        message.contains("account `missing` has no sign-in")
            && message.contains("corbanu login --account missing"),
        "{message}"
    );
    assert!(!message.contains("sk-default-canary"), "{message}");
    let _ = std::fs::remove_dir_all(&home);
}

/// D171 (#1286): a provider on a named sign-in account whose account another
/// process removed while the session ran refuses the next request with the
/// recovery, instead of running on the sign-in it held in memory or on the
/// default login.
#[tokio::test]
async fn named_openai_account_removed_mid_session_is_refused() {
    let home = std::env::temp_dir().join(format!("pf84-d171-removed-{}", std::process::id()));
    std::fs::create_dir_all(&home).expect("home");
    let work = NamedChatgptAccount {
        provider_id: "openai".to_string(),
        name: "work".to_string(),
    };
    codex_login::login_named_account_with_api_key(&home, &work, "sk-work-canary")
        .expect("named sign-in");
    let base = AuthManager::from_auth_for_testing_with_home(
        CodexAuth::from_api_key("sk-default-canary"),
        home.clone(),
    );
    let provider = create_model_provider(openai(Some("work")), Some(base));
    assert!(provider.api_auth().await.is_ok());

    // What `corbanu account remove openai work` runs, from outside this
    // provider's manager (an API-key sign-in has nothing to revoke).
    let route_config = codex_login::AuthRouteConfig::from_http_client_factory(
        codex_http_client::HttpClientFactory::new(
            codex_http_client::OutboundProxyPolicy::ReqwestDefault,
        ),
    );
    assert!(
        codex_login::logout_named_account_with_revoke(&home, &work, &route_config)
            .await
            .expect("remove")
    );
    let Err(error) = provider.api_auth().await else {
        panic!("a removed account must not authorize a request");
    };
    let message = error.to_string();
    assert!(
        message.contains("account `work` has no sign-in (it may have been removed or signed out)")
            && message.contains("corbanu login --account work"),
        "{message}"
    );
    assert!(provider.auth().await.is_none());
    assert!(
        !message.contains("sk-default-canary") && !message.contains("sk-work-canary"),
        "{message}"
    );
    // The first-party path model requests take refuses it too, instead of
    // sending the request without credentials.
    let scope = crate::auth::ProviderAuthScope {
        agent_identity_policy: codex_login::AgentIdentityAuthPolicy::ChatGptAuth,
        session_source: codex_protocol::protocol::SessionSource::Cli,
        agent_identity_session_fallback: Default::default(),
    };
    let Err(error) = provider.api_auth_for_scope(scope).await else {
        panic!("a removed account must not authorize a model request");
    };
    assert!(
        error
            .to_string()
            .contains("account `work` has no sign-in (it may have been removed or signed out)"),
        "{error}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

struct NamedSessionConfig {
    home: std::path::PathBuf,
}

impl codex_login::AuthManagerConfig for NamedSessionConfig {
    fn codex_home(&self) -> std::path::PathBuf {
        self.home.clone()
    }

    fn cli_auth_credentials_store_mode(&self) -> codex_login::AuthCredentialsStoreMode {
        codex_login::AuthCredentialsStoreMode::File
    }

    fn auth_keyring_backend_kind(&self) -> codex_login::AuthKeyringBackendKind {
        codex_login::AuthKeyringBackendKind::default()
    }

    fn forced_chatgpt_workspace_id(&self) -> Option<Vec<String>> {
        None
    }

    fn chatgpt_base_url(&self) -> String {
        "https://chatgpt.com/backend-api/".to_string()
    }

    fn auth_route_config(&self) -> codex_login::AuthRouteConfig {
        codex_login::AuthRouteConfig::from_http_client_factory(
            codex_http_client::HttpClientFactory::new(
                codex_http_client::OutboundProxyPolicy::ReqwestDefault,
            ),
        )
    }

    fn forced_login_method(&self) -> Option<codex_protocol::config_types::ForcedLoginMethod> {
        Some(codex_protocol::config_types::ForcedLoginMethod::Chatgpt)
    }

    fn runs_on_named_sign_in_account(&self) -> bool {
        true
    }
}

/// #570: a process whose session runs on a named account hides a default
/// login the restrictions rule out; a provider on the default login then
/// refuses the request with the reason instead of sending it without
/// credentials, and the login stays in place.
#[tokio::test]
async fn hidden_default_login_is_refused_not_sent_unauthenticated() {
    let home = std::env::temp_dir().join(format!("pf84-s06-hidden-{}", std::process::id()));
    std::fs::create_dir_all(&home).expect("home");
    codex_login::login_with_api_key(
        &home,
        "sk-default-canary",
        codex_login::AuthCredentialsStoreMode::File,
        codex_login::AuthKeyringBackendKind::default(),
    )
    .expect("default login");
    let base = AuthManager::shared_from_config(
        &NamedSessionConfig { home: home.clone() },
        /*enable_codex_api_key_env*/ false,
    )
    .await;
    let provider = create_model_provider(openai(/*account*/ None), Some(base));
    let Err(error) = provider.api_auth().await else {
        panic!("a hidden default login must not authorize a request");
    };
    let message = error.to_string();
    assert!(
        message.contains("ChatGPT login is required") && message.contains("left unchanged"),
        "{message}"
    );
    assert!(!message.contains("sk-default-canary"), "{message}");
    assert!(home.join("auth.json").exists());
    let _ = std::fs::remove_dir_all(&home);
}
