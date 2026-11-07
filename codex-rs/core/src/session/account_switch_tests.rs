//! PF-84: a mid-session switch between a named ChatGPT/OpenAI sign-in
//! account and the default login (or another named account) moves every
//! account-bound service, not only model requests. Runs under the isolated
//! test runner (`CORBANU_TEST_NO_NATIVE_KEYRING=1`).

use std::sync::Arc;

use codex_login::NamedChatgptAccount;
use codex_login::login_named_account_with_api_key;
use codex_model_provider_info::NamedProviderAccount;
use codex_protocol::auth::AuthMode;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::user_input::UserInput;
use pretty_assertions::assert_eq;

use super::Session;
use super::SessionSettingsUpdate;
use super::TurnInput;
use super::tests::TestFolders;
use super::tests::build_test_config;
use super::tests::make_session_and_context_for_config;
use super::tests::open_thread_persistence;
use crate::rollout::recorder::RolloutRecorder;

/// The default login of these sessions (see `make_session_and_context_for_config`).
const DEFAULT_KEY: &str = "Test API Key";
const WORK_KEY: &str = "sk-fake-work-switch-0001";
const PLAY_KEY: &str = "sk-fake-play-switch-0002";

fn account(name: &str) -> NamedChatgptAccount {
    NamedChatgptAccount {
        provider_id: "openai".to_string(),
        name: name.to_string(),
    }
}

/// A session on the default OpenAI login whose catalog also has
/// `openai-work` and `openai-play`: OpenAI on named accounts `work` and
/// `play`, each signed in with its own API key in the home's vault.
async fn session_with_accounts() -> (Arc<Session>, TestFolders) {
    let (session, home) = unshared_session_with_accounts().await;
    (Arc::new(session), home)
}

async fn unshared_session_with_accounts() -> (Session, TestFolders) {
    unshared_session_with_accounts_and(|_| {}).await
}

async fn unshared_session_with_accounts_and(
    configure: impl FnOnce(&mut crate::config::Config),
) -> (Session, TestFolders) {
    let home = tempfile::tempdir().expect("home");
    let test_cwd = tempfile::tempdir().expect("create test cwd");
    let mut config = build_test_config(home.path(), test_cwd.path()).await;
    configure(&mut config);
    for (name, key) in [("work", WORK_KEY), ("play", PLAY_KEY)] {
        login_named_account_with_api_key(home.path(), &account(name), key).expect("sign-in");
        let mut provider = config.model_provider.clone();
        provider.account = Some(NamedProviderAccount {
            provider_id: "openai".to_string(),
            name: name.to_string(),
        });
        config
            .model_providers
            .insert(format!("openai-{name}"), provider);
    }
    let (session, _turn_context) = make_session_and_context_for_config(config).await;
    let folders = TestFolders {
        home,
        cwd: test_cwd,
    };
    (session, folders)
}

async fn switch_to(session: &Session, provider_id: &str) {
    session
        .update_settings(SessionSettingsUpdate {
            model_provider: Some(provider_id.to_string()),
            ..Default::default()
        })
        .await
        .expect("switch provider");
}

fn active_key(session: &Session) -> Option<String> {
    session
        .services
        .auth_manager()
        .auth_cached()
        .and_then(|auth| auth.get_token().ok())
}

/// Connectors (the MCP runtime and its Apps auth) reconnect with the account
/// the session switched to: named, back to the default, and another named one.
#[tokio::test]
async fn connectors_follow_a_mid_session_account_switch() {
    let (session, _home) = session_with_accounts().await;
    session.refresh_mcp_if_dirty().await;
    assert_eq!(active_key(&session).as_deref(), Some(DEFAULT_KEY));

    for (provider_id, key, named) in [
        ("openai-work", WORK_KEY, Some(account("work"))),
        ("openai", DEFAULT_KEY, None),
        ("openai-play", PLAY_KEY, Some(account("play"))),
        ("openai-work", WORK_KEY, Some(account("work"))),
    ] {
        switch_to(&session, provider_id).await;
        let auth_manager = session.services.auth_manager();
        assert_eq!(
            (
                active_key(&session).as_deref(),
                auth_manager.named_account()
            ),
            (Some(key), named.as_ref()),
            "{provider_id}"
        );
        assert!(
            session.mcp_refresh.is_pending(),
            "{provider_id}: connectors were not marked for reconnection"
        );
        session.refresh_mcp_if_dirty().await;
        assert!(
            session
                .services
                .mcp_runtime
                .current_auth_matches(auth_manager.auth_cached().as_ref()),
            "{provider_id}: connectors still use the previous account"
        );
    }
}

/// The sign-in the ledger prices a request under is that of the request's
/// own provider: after a switch to the named API-key account its requests
/// are priced as API-key requests, while a request still going to the
/// default provider (a turn that started before the switch) keeps the
/// default ChatGPT login's mode.
#[tokio::test]
async fn accounting_follows_a_mid_session_account_switch() {
    let (session, _home) = session_with_accounts().await;
    session.services.active_auth.replace_for_testing(
        codex_login::AuthManager::from_auth_for_testing_with_home(
            codex_login::CodexAuth::create_dummy_chatgpt_auth_for_testing(),
            session.codex_home().await.to_path_buf(),
        ),
    );
    let default_provider = session.provider().await;
    let mode = |provider| crate::accounting_extensions::accounting_auth_mode(&session, provider);
    assert_eq!(mode(&default_provider).await, Some(AuthMode::Chatgpt));

    switch_to(&session, "openai-work").await;
    let work_provider = session.provider().await;
    assert_eq!(mode(&work_provider).await, Some(AuthMode::ApiKey));
    assert_eq!(mode(&default_provider).await, Some(AuthMode::Chatgpt));
    switch_to(&session, "openai").await;
    assert_eq!(
        mode(&session.provider().await).await,
        Some(AuthMode::Chatgpt)
    );
}

/// The MCP prewarm worker keeps running across switches, including back
/// from an account whose manager is gone by then, and reconnects the
/// connectors with each new account.
#[tokio::test]
async fn the_connector_worker_survives_switches_and_reconnects() {
    let (session, _home) = session_with_accounts().await;
    let (_requests, requests_rx) = async_channel::bounded(1);
    session.start_mcp_prewarm_worker(
        requests_rx,
        session.services.auth_manager().auth_change_receiver(),
    );
    for (provider_id, key) in [
        ("openai-work", WORK_KEY),
        ("openai", DEFAULT_KEY),
        ("openai-play", PLAY_KEY),
        ("openai", DEFAULT_KEY),
    ] {
        switch_to(&session, provider_id).await;
        assert_eq!(active_key(&session).as_deref(), Some(key));
        let auth = session.services.auth_manager().auth_cached();
        let reconnected = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !session
                .services
                .mcp_runtime
                .current_auth_matches(auth.as_ref())
            {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(
            reconnected.is_ok(),
            "{provider_id}: the worker did not reconnect"
        );
        let worker_running = session
            .mcp_prewarm_task
            .lock()
            .expect("worker slot")
            .as_ref()
            .is_some_and(|worker| !worker.is_finished());
        assert!(worker_running, "{provider_id}: the worker stopped");
    }
    session.stop_mcp_prewarm_worker().await;
}

/// A turn whose settings change the provider (`Op::UserInput` with thread
/// settings) moves the account too, as `update_settings` does.
#[tokio::test]
async fn a_turn_that_changes_the_provider_moves_the_account() {
    let (session, _home) = session_with_accounts().await;
    session.refresh_mcp_if_dirty().await;
    session
        .new_turn_with_sub_id(
            "turn-1".to_string(),
            SessionSettingsUpdate {
                model_provider: Some("openai-work".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("turn");
    assert_eq!(
        (
            active_key(&session).as_deref(),
            session.services.auth_manager().named_account().cloned()
        ),
        (Some(WORK_KEY), Some(account("work")))
    );
    assert!(session.mcp_refresh.is_pending());
}

/// A switch that keeps the same sign-in (another model of the same
/// provider) leaves the connectors alone.
#[tokio::test]
async fn a_switch_that_keeps_the_account_does_not_reconnect() {
    let (session, _home) = session_with_accounts().await;
    switch_to(&session, "openai-work").await;
    session.refresh_mcp_if_dirty().await;
    let before = session.services.auth_manager();

    let collaboration_mode = session.collaboration_mode().await.with_updates(
        Some("gpt-5.4".to_string()),
        /*effort*/ None,
        /*developer_instructions*/ None,
    );
    session
        .update_settings(SessionSettingsUpdate {
            collaboration_mode: Some(collaboration_mode),
            ..Default::default()
        })
        .await
        .expect("update settings");

    assert!(Arc::ptr_eq(&before, &session.services.auth_manager()));
    assert!(!session.mcp_refresh.is_pending());
}

/// The process's model list (what `model/list` and the picker offer) is
/// fetched and filtered with the account the session switched to.
#[tokio::test]
async fn the_model_list_follows_a_mid_session_account_switch() {
    let (mut session, folders) = unshared_session_with_accounts().await;
    let home = &folders.home;
    let default_auth = codex_login::AuthManager::from_auth_for_testing_with_home(
        codex_login::CodexAuth::create_dummy_chatgpt_auth_for_testing(),
        home.path().to_path_buf(),
    );
    session
        .services
        .active_auth
        .replace_for_testing(Arc::clone(&default_auth));
    let mut config = build_test_config(home.path(), folders.cwd.path()).await;
    let only_chatgpt =
        |slug: &str, supported_in_api: bool| codex_protocol::openai_models::ModelInfo {
            supported_in_api,
            ..codex_models_manager::model_info::model_info_from_slug(slug)
        };
    config.model_catalog = Some(codex_protocol::openai_models::ModelsResponse {
        models: vec![
            only_chatgpt("pf84-api-model", true),
            only_chatgpt("pf84-chatgpt-only-model", false),
        ],
    });
    session.services.models_manager =
        crate::thread_manager::build_models_manager(&config, default_auth);
    let session = Arc::new(session);
    let listed = || {
        let mut slugs: Vec<String> = session
            .services
            .models_manager
            .try_list_models()
            .expect("models")
            .into_iter()
            .map(|preset| preset.model)
            .collect();
        slugs.sort();
        slugs
    };
    let both = vec![
        "pf84-api-model".to_string(),
        "pf84-chatgpt-only-model".to_string(),
    ];
    assert_eq!(listed(), both);

    switch_to(&session, "openai-work").await;
    assert_eq!(listed(), vec!["pf84-api-model".to_string()]);
    switch_to(&session, "openai").await;
    assert_eq!(listed(), both);
}

/// Moved from `session::tests` (that file is over the blob size limit).
#[tokio::test]
async fn mcp_refresh_updates_plugin_auth_mode_before_checking_pending_state() {
    let codex_home = tempfile::tempdir().expect("create auth test directory");
    let (session, _turn_context) = super::tests::make_session_and_context().await;
    session.services.active_auth.replace_for_testing(
        codex_login::AuthManager::from_auth_for_testing_with_home(
            codex_login::CodexAuth::from_api_key("old-api-key"),
            codex_home.path().to_path_buf(),
        ),
    );
    let session = Arc::new(session);
    let auth_mode = session.services.auth_manager().get_api_auth_mode();

    assert_ne!(session.services.plugins_manager.auth_mode(), auth_mode);
    session.mcp_refresh.claim();

    session.refresh_mcp_if_dirty().await;

    assert_eq!(session.services.plugins_manager.auth_mode(), auth_mode);
    assert!(
        session
            .services
            .mcp_runtime
            .current_binding()
            .await
            .is_some()
    );

    codex_login::login_with_api_key(
        codex_home.path(),
        "new-api-key",
        codex_login::AuthCredentialsStoreMode::File,
        codex_login::AuthKeyringBackendKind::default(),
    )
    .expect("store replacement API key");
    session.services.auth_manager().reload().await;
    assert_eq!(
        session
            .services
            .auth_manager()
            .auth_cached()
            .and_then(|auth| auth.get_token().ok()),
        Some("new-api-key".to_string())
    );
    assert_eq!(session.services.plugins_manager.auth_mode(), auth_mode);
    session.mcp_refresh.claim();

    session.refresh_mcp_if_dirty().await;

    assert!(
        session
            .services
            .mcp_runtime
            .current_auth_matches(session.services.auth_manager().auth_cached().as_ref())
    );
}

/// #1331 (D171): a prompt refused because the session's named account was
/// removed meanwhile (here as `corbanu account remove openai work` does it,
/// from outside the session) is not kept as a turn on that account. Neither
/// the prompt nor a turn context naming the account is written to the
/// conversation, so a later resume does not count it as that account's most
/// recent turn; the turn's completion carries the refusal.
#[tokio::test]
async fn a_prompt_refused_for_a_removed_account_is_not_recorded_as_its_turn() {
    const REFUSED_PROMPT: &str = "refused-1331-prompt";
    let (mut session, folders) = unshared_session_with_accounts_and(|config| {
        // Turn contexts record the account only while named accounts are on.
        config
            .features
            .enable(codex_features::Feature::NamedAccounts)
            .expect("enable named accounts");
    })
    .await;
    let rollout_path = open_thread_persistence(&mut session).await;
    session.ensure_rollout_materialized().await;
    let session = Arc::new(session);
    let turn = session
        .new_turn_with_sub_id(
            "turn-refused".to_string(),
            SessionSettingsUpdate {
                model_provider: Some("openai-work".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("turn");
    assert_eq!(active_key(&session).as_deref(), Some(WORK_KEY));

    // An API-key sign-in has nothing to revoke.
    let route_config = codex_login::AuthRouteConfig::from_http_client_factory(
        codex_http_client::HttpClientFactory::new(
            codex_http_client::OutboundProxyPolicy::ReqwestDefault,
        ),
    );
    assert!(
        codex_login::logout_named_account_with_revoke(
            folders.home.path(),
            &account("work"),
            &route_config
        )
        .await
        .expect("remove")
    );

    session
        .spawn_task(
            Arc::clone(&turn),
            vec![TurnInput::UserInput {
                content: vec![UserInput::Text {
                    text: REFUSED_PROMPT.to_string(),
                    text_elements: Vec::new(),
                }],
                client_id: None,
            }],
            crate::tasks::RegularTask::new(),
        )
        .await;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let (completed, history) = loop {
        session.flush_rollout().await.expect("flush rollout");
        let InitialHistory::Resumed(resumed) = RolloutRecorder::get_rollout_history(&rollout_path)
            .await
            .expect("read rollout history")
        else {
            panic!("expected resumed rollout history");
        };
        let completed = resumed.history.iter().find_map(|item| match item {
            RolloutItem::EventMsg(EventMsg::TurnComplete(event))
                if event.turn_id == turn.sub_id =>
            {
                Some(event.clone())
            }
            _ => None,
        });
        if let Some(completed) = completed {
            break (completed, resumed.history);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the refused turn did not complete"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };

    let refusal = completed
        .error
        .map(|error| error.message)
        .unwrap_or_default();
    assert!(
        refusal.contains("account `work` has no sign-in (it may have been removed or signed out)")
            && refusal.contains("corbanu login --account work"),
        "{refusal}"
    );
    // Resume restores the account of the newest turn context (PF-84).
    let turn_contexts = history
        .iter()
        .filter_map(|item| match item {
            RolloutItem::TurnContext(context) => {
                Some((context.turn_id.clone(), context.provider_account.clone()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(turn_contexts, Vec::new());
    let kept_prompt = history
        .iter()
        .filter_map(|item| serde_json::to_string(item).ok())
        .filter(|line| line.contains(REFUSED_PROMPT))
        .collect::<Vec<_>>();
    assert_eq!(kept_prompt, Vec::<String>::new());
    let in_memory = session
        .clone_history()
        .await
        .raw_items()
        .iter()
        .filter_map(|item| serde_json::to_string(item).ok())
        .filter(|line| line.contains(REFUSED_PROMPT))
        .collect::<Vec<_>>();
    assert_eq!(in_memory, Vec::<String>::new());
}
