use super::*;
use crate::app::test_support::make_test_app;
use crate::test_support::PathBufExt;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tempfile::tempdir;

fn name(raw: &str) -> ProviderAccountName {
    ProviderAccountName::parse(raw).expect("valid name")
}

#[tokio::test]
async fn issue1290_dismissal_during_save_or_summary_keeps_the_committed_result() {
    use crate::chatwidget::named_account_login::NamedAccountLoginState;
    use base64::Engine;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;
    use tokio::sync::mpsc;
    use tokio::sync::oneshot;

    for save_before_wait in [false, true] {
        for drop_view in [false, true] {
            let home = tempdir().expect("isolated account home");
            let account = codex_login::NamedChatgptAccount {
                provider_id: "openai".to_string(),
                name: "work".to_string(),
            };
            let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                serde_json::json!({"https://api.openai.com/auth": {
                    "chatgpt_user_id": "issue1290-user"
                }})
                .to_string(),
            );
            let auth = codex_login::AuthDotJson {
                auth_mode: Some(codex_protocol::auth::AuthMode::Chatgpt),
                openai_api_key: None,
                tokens: Some(codex_login::TokenData {
                    id_token: codex_login::token_data::parse_chatgpt_jwt_claims(&format!(
                        "e30.{claims}.c2ln"
                    ))
                    .expect("fixture claims"),
                    access_token: "issue1290-access".to_string(),
                    refresh_token: "issue1290-refresh".to_string(),
                    ..Default::default()
                }),
                last_refresh: None,
                agent_identity: None,
                personal_access_token: None,
                bedrock_api_key: None,
            };
            codex_login::save_named_account_auth(
                home.path(),
                &codex_login::NamedChatgptAccount {
                    name: "other".to_string(),
                    ..account.clone()
                },
                &auth,
                /*must_be_new*/ true,
            )
            .expect("existing account for the duplicate-user warning");
            let (mut chat, sender, mut events, _ops) =
                crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
            while events.try_recv().is_ok() {}
            let state = NamedAccountLoginState::default();
            let task_state = state.clone();
            let path = home.path().to_path_buf();
            let task_account = account.clone();
            let (reached, waiting) = oneshot::channel();
            let (release, released) = oneshot::channel();
            let (done, completed) = oneshot::channel();
            let task = tokio::spawn(async move {
                task_state
                    .begin_persistence()
                    .expect("save won cancellation gate");
                let result = tokio::task::spawn_blocking(move || {
                    let save = || {
                        codex_login::save_named_account_auth(
                            &path,
                            &task_account,
                            &auth,
                            /*must_be_new*/ true,
                        )
                        .expect("named account saved")
                    };
                    if save_before_wait {
                        save();
                    }
                    let _ = reached.send(());
                    released.blocking_recv().expect("release save or summary");
                    if !save_before_wait {
                        save();
                    }
                    signed_in_message(
                        &path,
                        "openai",
                        &name("work"),
                        &task_account,
                        codex_login::AuthCredentialsStoreMode::File,
                        codex_login::AuthKeyringBackendKind::default(),
                    )
                })
                .await
                .map_err(|error| error.to_string());
                task_state.finish(&sender, result);
                let _ = done.send(());
            });
            let (_codes, code_rx) = mpsc::unbounded_channel();
            chat.open_named_account_login(
                "openai account `work`".to_string(),
                code_rx,
                task,
                state,
            );
            tokio::time::timeout(std::time::Duration::from_secs(30), waiting)
                .await
                .expect("blocking save or summary reached")
                .expect("checkpoint");
            if drop_view {
                drop(chat);
            } else {
                chat.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
                assert!(chat.no_modal_or_popup_active());
            }
            assert!(events.try_recv().is_err(), "no false Cancelled result");
            release.send(()).expect("save still owned");
            tokio::time::timeout(std::time::Duration::from_secs(30), completed)
                .await
                .expect("finalization continues after dismissal")
                .expect("finished");
            let expected = signed_in_message(
                home.path(),
                "openai",
                &name("work"),
                &account,
                codex_login::AuthCredentialsStoreMode::File,
                codex_login::AuthKeyringBackendKind::default(),
            );
            assert!(
                expected.contains("same ChatGPT user as account(s) `other`"),
                "{expected}"
            );
            match events.try_recv().expect("actual committed result") {
                AppEvent::ProviderAccount(ProviderAccountEvent::Finished { result }) => {
                    assert_eq!(result, Ok(expected));
                }
                other => panic!("unexpected event: {other:?}"),
            }
            assert!(
                codex_login::named_account_summary(home.path(), &account)
                    .unwrap()
                    .is_some()
            );
            assert!(
                !home.path().join("auth.json").exists(),
                "default login unchanged"
            );
            assert!(events.try_recv().is_err());
        }
    }
}

async fn app_with_named_accounts(extra_config: &str) -> (App, TempDir) {
    let mut app = make_test_app().await;
    let home = tempdir().expect("home");
    std::fs::write(
        home.path().join("config.toml"),
        format!("[features]\nnamed_accounts = true\n{extra_config}"),
    )
    .expect("config");
    app.config.codex_home = home.path().to_path_buf().abs();
    app.refresh_in_memory_config_from_disk()
        .await
        .expect("load config");
    (app, home)
}

fn session_account(app: &App, provider_id: &str) -> Option<String> {
    app.config.model_providers[provider_id]
        .account
        .as_ref()
        .map(|account| account.name.clone())
}

#[tokio::test]
async fn a_session_account_that_cannot_load_changes_nothing() {
    let (mut app, _home) = app_with_named_accounts("").await;
    // A local server has no credential, so no named accounts (OpenAI sign-in
    // can hold them since S06).
    let error = app
        .select_session_provider_account("ollama", Some(&name("work")))
        .await
        .expect_err("refused");
    assert!(error.contains("does not support named accounts"), "{error}");
    assert_eq!(app.harness_overrides.provider_account, None);
    assert_eq!(session_account(&app, "ollama"), None);
}

#[tokio::test]
async fn one_session_selection_covers_one_provider() {
    let (mut app, _home) = app_with_named_accounts("").await;
    assert_eq!(
        app.select_session_provider_account("zai", Some(&name("work")))
            .await,
        Ok(None)
    );
    assert_eq!(session_account(&app, "zai").as_deref(), Some("work"));

    // Another provider's selection is never silently dropped.
    let error = app
        .select_session_provider_account("openrouter", Some(&name("alt")))
        .await
        .expect_err("refused");
    assert!(
        error.contains("already selects zai account `work`"),
        "{error}"
    );
    assert_eq!(
        app.harness_overrides.provider_account.as_deref(),
        Some("zai:work")
    );

    // Back to the default account, then another provider is free.
    assert_eq!(
        app.select_session_provider_account("zai", /*name*/ None)
            .await,
        Ok(Some("zai:work".to_string()))
    );
    assert_eq!(session_account(&app, "zai"), None);
    assert!(
        app.select_session_provider_account("openrouter", Some(&name("alt")))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn rename_carries_the_default_along() {
    let (mut app, home) = app_with_named_accounts("[provider_accounts]\nzai = \"work\"\n").await;
    codex_vault::Vault::new(home.path().to_path_buf())
        .write_provider_account(
            "zai",
            &name("work"),
            codex_vault::ProviderAccountKind::ApiKey,
            "pf84-s04-rename-canary",
        )
        .expect("write");
    let message = app
        .rename_provider_account("zai", &name("work"), &name("home"))
        .await
        .expect("renamed");
    assert_eq!(
        message,
        "Renamed zai account `work` to `home`. It stays the default for new sessions."
    );
    let config = std::fs::read_to_string(home.path().join("config.toml")).expect("config");
    assert!(config.contains("zai = \"home\""), "{config}");
    assert_eq!(session_account(&app, "zai").as_deref(), Some("home"));
}

#[tokio::test]
async fn a_default_set_by_a_higher_layer_is_reported() {
    let (mut app, _home) = app_with_named_accounts("").await;
    app.cli_kv_overrides.push((
        "provider_accounts.zai".to_string(),
        toml::Value::String("work".to_string()),
    ));
    let error = app
        .persist_default_provider_account("zai", /*name*/ None)
        .await
        .expect_err("outranked");
    assert!(error.contains("another config layer"), "{error}");
}

/// #572: with a named Bedrock account (an AWS profile) in the vault, a
/// session on another provider is offered Bedrock's models in `/model` (and
/// the provider policy lets them be picked), so an account chosen with *Start
/// a new session on this account* is reachable. Without one, with the feature off, or
/// once the last one is removed, Bedrock stays out of the list as before,
/// except the model the session is on. Only the newest check is applied.
#[tokio::test]
async fn named_bedrock_account_offers_bedrock_models() {
    use super::super::provider_management_status::holds_named_bedrock_account;
    use codex_model_provider_info::AMAZON_BEDROCK_PROVIDER_ID;
    let bedrock_models = |app: &App| {
        app.model_catalog
            .try_list_models()
            .unwrap()
            .into_iter()
            .filter(|preset| preset.provider_id.as_deref() == Some(AMAZON_BEDROCK_PROVIDER_ID))
            .collect::<Vec<_>>()
    };
    let (mut app, home) = app_with_named_accounts("").await;
    app.model_catalog.set_provider_policy(
        crate::chatwidget::provider_model_policy::ProviderModelPolicy::new(
            crate::provider_status_host::ProviderStatusHost::from_config(
                &app.config,
                crate::provider_status_host::ProviderAccountMetadata::default(),
            ),
            codex_provider_auth::ProviderRuntimeAuthorizations::default(),
        ),
    );
    let check = |app: &mut App| {
        let offered = holds_named_bedrock_account(&app.config);
        let generation = app.model_catalog.begin_provider_models_check();
        app.named_bedrock_account_checked(generation, offered);
    };
    assert_eq!(holds_named_bedrock_account(&app.config), Some(false));
    check(&mut app);
    assert!(bedrock_models(&app).is_empty());

    let vault = codex_vault::Vault::new(home.path().to_path_buf());
    vault
        .write_provider_account(
            AMAZON_BEDROCK_PROVIDER_ID,
            &name("work"),
            codex_vault::ProviderAccountKind::AwsProfile,
            "corp-dev",
        )
        .expect("write account");
    let mut feature_off = app.config.clone();
    feature_off
        .features
        .disable(codex_features::Feature::NamedAccounts)
        .expect("named accounts can be turned off");
    assert_eq!(holds_named_bedrock_account(&feature_off), Some(false));
    assert_eq!(holds_named_bedrock_account(&app.config), Some(true));

    // A stale check's answer is dropped.
    let stale = app.model_catalog.begin_provider_models_check();
    check(&mut app);
    app.named_bedrock_account_checked(stale, Some(false));
    let offered = bedrock_models(&app);
    assert!(
        offered.iter().any(
            |preset| preset.model == codex_model_provider_info::AMAZON_BEDROCK_GPT_5_4_MODEL_ID
        ),
        "{offered:?}"
    );
    assert!(
        offered
            .iter()
            .all(|preset| app.model_catalog.preset_is_selectable(preset)),
        "{offered:?}"
    );

    // The session switches to one of them; removing the last account then
    // withdraws the others but keeps the model the session is on.
    // (As the model picker does: provider in the config, model in the chat
    // widget; `config.model` keeps the model the session started with.)
    app.config.model_provider_id = AMAZON_BEDROCK_PROVIDER_ID.to_string();
    app.chat_widget
        .set_model(codex_model_provider_info::AMAZON_BEDROCK_GPT_5_4_MODEL_ID);
    vault
        .remove_provider_account(AMAZON_BEDROCK_PROVIDER_ID, &name("work"))
        .expect("remove account");
    check(&mut app);
    let kept = bedrock_models(&app);
    assert_eq!(
        kept.iter()
            .map(|preset| preset.model.as_str())
            .collect::<Vec<_>>(),
        vec![codex_model_provider_info::AMAZON_BEDROCK_GPT_5_4_MODEL_ID]
    );
    // It keeps its Bedrock metadata.
    assert!(!kept[0].supported_reasoning_efforts.is_empty(), "{kept:?}");
}

/// #573: with a third-party provider configured, `/providers` still offers a
/// device-code ChatGPT sign-in for a named OpenAI account; only a configured
/// `forced_login_method` narrows the choice.
#[tokio::test]
async fn openai_add_methods_follow_only_the_configured_login_method() {
    use crate::provider_named_accounts::AddAccountMethod;
    let both = vec![
        AddAccountMethod::ChatgptDeviceCode,
        AddAccountMethod::OpenaiApiKey,
    ];
    for (extra_config, expected) in [
        ("", both.clone()),
        (
            "model_provider = \"zai\"\nmodel = \"glm-5.3-flash\"\n",
            both.clone(),
        ),
        (
            "forced_login_method = \"api\"\n",
            vec![AddAccountMethod::OpenaiApiKey],
        ),
        (
            "forced_login_method = \"chatgpt\"\n",
            vec![AddAccountMethod::ChatgptDeviceCode],
        ),
    ] {
        let (app, _home) = app_with_named_accounts_at_top(extra_config).await;
        assert_eq!(
            app.provider_account_methods("openai"),
            expected,
            "{extra_config:?}"
        );
    }
}

/// Like [`app_with_named_accounts`], with `top` before the `[features]` table.
async fn app_with_named_accounts_at_top(top: &str) -> (App, TempDir) {
    let mut app = make_test_app().await;
    let home = tempdir().expect("home");
    std::fs::write(
        home.path().join("config.toml"),
        format!("{top}[features]\nnamed_accounts = true\n"),
    )
    .expect("config");
    app.config.codex_home = home.path().to_path_buf().abs();
    app.refresh_in_memory_config_from_disk()
        .await
        .expect("load config");
    (app, home)
}

/// PF-84: only a model switch that changes the sign-in account refreshes the
/// model list (and says so); one between models of the same account does not.
#[test]
fn only_a_sign_in_account_change_is_a_switch() {
    use crate::app::event_dispatch::DEFAULT_LOGIN_RESTRICTION_CAVEAT;
    use crate::app::event_dispatch::account_switch_message_for;
    use crate::app::event_dispatch::sign_in_account_switch;
    use codex_model_provider_info::ModelProviderInfo;
    use codex_model_provider_info::NamedProviderAccount;

    let openai = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
    let mut work = openai.clone();
    work.account = Some(NamedProviderAccount {
        provider_id: "openai".to_string(),
        name: "work".to_string(),
    });
    let zai = codex_model_provider_info::built_in_model_providers(/*openai_base_url*/ None)["zai"]
        .clone();
    let work_account = codex_login::NamedChatgptAccount {
        provider_id: "openai".to_string(),
        name: "work".to_string(),
    };

    assert_eq!(
        sign_in_account_switch(&openai, &work),
        Some(Some(work_account.clone()))
    );
    assert_eq!(sign_in_account_switch(&work, &zai), Some(None));
    assert_eq!(sign_in_account_switch(&work, &work), None);
    assert_eq!(sign_in_account_switch(&openai, &zai), None);
    assert_eq!(
        account_switch_message_for(
            Some(&work_account),
            /*default_login_left_in_place*/ false,
            /*login_restrictions_configured*/ false,
        ),
        "Connectors, usage and the model list now use openai account `work`; model list refreshed."
    );
    // #1205: the default login is the default OpenAI login, not the new
    // provider's default account.
    assert_eq!(
        account_switch_message_for(
            /*account*/ None, /*default_login_left_in_place*/ false,
            /*login_restrictions_configured*/ false,
        ),
        "Connectors, usage and the model list now use the default OpenAI login; model list refreshed."
    );
    // #1205: startup enforcement skipped, but no restriction configured: no
    // forced-login caveat.
    assert_eq!(
        account_switch_message_for(
            /*account*/ None, /*default_login_left_in_place*/ true,
            /*login_restrictions_configured*/ false,
        ),
        "Connectors, usage and the model list now use the default OpenAI login; model list refreshed."
    );
    // #609: a process that left a ruled-out default login in place hides it.
    assert_eq!(
        account_switch_message_for(
            /*account*/ None, /*default_login_left_in_place*/ true,
            /*login_restrictions_configured*/ true,
        ),
        format!(
            "Connectors, usage and the model list now use the default OpenAI login; model list refreshed. {DEFAULT_LOGIN_RESTRICTION_CAVEAT}"
        )
    );
    // A restriction configured but enforced at startup: nothing is hidden.
    assert_eq!(
        account_switch_message_for(
            /*account*/ None, /*default_login_left_in_place*/ false,
            /*login_restrictions_configured*/ true,
        ),
        "Connectors, usage and the model list now use the default OpenAI login; model list refreshed."
    );
    assert_eq!(
        account_switch_message_for(
            Some(&work_account),
            /*default_login_left_in_place*/ true,
            /*login_restrictions_configured*/ true,
        ),
        account_switch_message_for(
            Some(&work_account),
            /*default_login_left_in_place*/ false,
            /*login_restrictions_configured*/ false,
        )
    );
}

/// #1205: only a configured `forced_login_method` or
/// `forced_chatgpt_workspace_id` counts as a login restriction; the method an
/// API-key provider such as Z.AI implies does not.
#[tokio::test]
async fn account_switch_caveat_needs_a_configured_login_restriction() {
    use crate::app::event_dispatch::account_switch_message;
    use crate::app::event_dispatch::login_restrictions_configured;
    use codex_protocol::config_types::ForcedLoginMethod;

    let work_account = codex_login::NamedChatgptAccount {
        provider_id: "openai".to_string(),
        name: "work".to_string(),
    };
    // The last column is the effective `forced_login_method`. Z.AI implies
    // `Api` without configuring it, so that case tells
    // `configured_forced_login_method` apart from `forced_login_method`.
    for (extra_config, expected, forced_login_method) in [
        ("", false, None),
        (
            "model_provider = \"zai\"\nmodel = \"glm-5.3-flash\"\n",
            false,
            Some(ForcedLoginMethod::Api),
        ),
        (
            "forced_login_method = \"api\"\n",
            true,
            Some(ForcedLoginMethod::Api),
        ),
        (
            "forced_login_method = \"chatgpt\"\n",
            true,
            Some(ForcedLoginMethod::Chatgpt),
        ),
        ("forced_chatgpt_workspace_id = \"ws-1\"\n", true, None),
    ] {
        let (app, _home) = app_with_named_accounts_at_top(extra_config).await;
        assert_eq!(
            app.config.forced_login_method, forced_login_method,
            "{extra_config:?}"
        );
        assert_eq!(
            login_restrictions_configured(&app.config),
            expected,
            "{extra_config:?}"
        );
        // A switch back to a named account reads the same either way.
        assert_eq!(
            account_switch_message(Some(&work_account), &app.config),
            "Connectors, usage and the model list now use openai account `work`; model list refreshed.",
            "{extra_config:?}"
        );
    }
}

/// PF-84: only `Confirmed { Applied }` counts as applied before the reply;
/// an uncertain outcome or a plain acceptance does not refresh the list.
#[test]
fn only_an_applied_confirmation_counts() {
    use codex_app_server_protocol::ThreadSettingsUpdateOutcome;
    use codex_app_server_protocol::ThreadSettingsUpdateResponse;

    assert!(crate::app_server_session::applied_before_reply(
        &ThreadSettingsUpdateResponse::Confirmed {
            outcome: ThreadSettingsUpdateOutcome::Applied,
        }
    ));
    assert!(!crate::app_server_session::applied_before_reply(
        &ThreadSettingsUpdateResponse::Confirmed {
            outcome: ThreadSettingsUpdateOutcome::Uncertain,
        }
    ));
    assert!(!crate::app_server_session::applied_before_reply(
        &ThreadSettingsUpdateResponse::Accepted {}
    ));
}
