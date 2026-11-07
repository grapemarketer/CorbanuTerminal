use super::*;
use crate::app_event::ConnectorsSnapshot;
use crate::chatwidget::connectors::ConnectorsCacheState;
use crate::spawn_orchestration::SpawnRole;
use codex_app_server_protocol::HookErrorInfo;
use codex_app_server_protocol::HooksListEntry;
use codex_app_server_protocol::HooksListResponse;
use codex_app_server_protocol::MarketplaceLoadErrorInfo;
use codex_app_server_protocol::MarketplaceRemoveResponse;
use codex_app_server_protocol::PluginAvailability;
use codex_app_server_protocol::PluginShareContext;
use codex_app_server_protocol::PluginShareDiscoverability;
use codex_app_server_protocol::PluginSource;
use codex_connectors::AppInfo;
use codex_features::Stage;
use codex_model_provider_info::AMBIENT_DEFAULT_MODEL;
use codex_model_provider_info::AMBIENT_PROVIDER_ID;
use codex_model_provider_info::ANTHROPIC_DEFAULT_MODEL;
use codex_model_provider_info::BASETEN_DEFAULT_MODEL;
use codex_model_provider_info::CLAUDE_FABLE_5_1_MODEL;
use codex_model_provider_info::CLAUDE_FABLE_5_1_PLAN_MODEL;
use codex_model_provider_info::CLAUDE_FABLE_5_MODEL;
use codex_model_provider_info::CLAUDE_FABLE_5_PLAN_MODEL;
use codex_model_provider_info::CLAUDE_PLAN_MODEL;
use codex_model_provider_info::CORBANU_API_DEEPSEEK_V4_1_FLASH_MODEL;
use codex_model_provider_info::CORBANU_API_DEEPSEEK_V4_PRO_MODEL;
use codex_model_provider_info::CORBANU_API_GLM_5_3_FLASH_MODEL;
use codex_model_provider_info::CORBANU_API_GLM_5_3_MODEL;
use codex_model_provider_info::CORBANU_API_GPT_5_6_LUNA_MODEL;
use codex_model_provider_info::CORBANU_API_GPT_5_6_SOL_MODEL;
use codex_model_provider_info::CORBANU_API_KIMI_K3_MODEL;
use codex_model_provider_info::DEEPSEEK_DEFAULT_MODEL;
use codex_model_provider_info::DEEPSEEK_PROVIDER_ID;
use codex_model_provider_info::KIMI_CODE_K3_MODEL;
use codex_model_provider_info::KIMI_CODE_PROVIDER_ID;
use codex_model_provider_info::OPENROUTER_PROVIDER_ID;
use codex_model_provider_info::PFTERMINAL_PLAN_PROVIDER_ID;
use codex_model_provider_info::VERCEL_DEEPSEEK_V4_PRO_MODEL;
use codex_model_provider_info::VERCEL_DEFAULT_MODEL;
use codex_model_provider_info::VERCEL_GLM_5_2_FAST_MODEL;
use codex_model_provider_info::VERCEL_GLM_5_3_FLASH_MODEL;
use codex_model_provider_info::VERCEL_GLM_5_3_MODEL;
use codex_model_provider_info::VERCEL_KIMI_K3_MODEL;
use codex_model_provider_info::VERCEL_PROVIDER_ID;
use codex_model_provider_info::ZAI_DEFAULT_MODEL;
use codex_protocol::openai_models::ReasoningEffort;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn telegram_setup_disconnected_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.6-sol")).await;
    chat.open_telegram_menu();
    chat.refresh_telegram_menu(Ok(crate::chatwidget::telegram_setup::TelegramStatus {
        configured: false,
        token_stored: false,
        running: false,
        pid: None,
        bot_username: None,
        allowed_chat_ids: Vec::new(),
        default_model: None,
        default_cwd: None,
        approval_policy: None,
        sandbox_mode: None,
    }));

    assert_chatwidget_snapshot!(
        "telegram_setup_disconnected",
        render_bottom_popup(&chat, /*width*/ 94)
    );
}

#[tokio::test]
async fn telegram_setup_connected_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.6-sol")).await;
    chat.open_telegram_menu();
    chat.refresh_telegram_menu(Ok(crate::chatwidget::telegram_setup::TelegramStatus {
        configured: true,
        token_stored: true,
        running: true,
        pid: Some(4242),
        bot_username: Some("pumps_bot".to_string()),
        allowed_chat_ids: vec![123456],
        default_model: Some("gpt-5.6-sol".to_string()),
        default_cwd: Some(PathBuf::from("/work/project")),
        approval_policy: Some("on-request".to_string()),
        sandbox_mode: Some("workspace-write".to_string()),
    }));

    assert_chatwidget_snapshot!(
        "telegram_setup_connected",
        render_bottom_popup(&chat, /*width*/ 94)
    );
}

#[tokio::test]
async fn telegram_setup_resumes_after_token_validation_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.6-sol")).await;
    chat.open_telegram_menu();
    chat.refresh_telegram_menu(Ok(crate::chatwidget::telegram_setup::TelegramStatus {
        configured: false,
        token_stored: true,
        running: false,
        pid: None,
        bot_username: Some("pumps_bot".to_string()),
        allowed_chat_ids: Vec::new(),
        default_model: None,
        default_cwd: None,
        approval_policy: None,
        sandbox_mode: None,
    }));

    assert_chatwidget_snapshot!(
        "telegram_setup_waiting_for_chat",
        render_bottom_popup(&chat, /*width*/ 94)
    );
}

#[tokio::test]
async fn telegram_setup_polls_until_a_chat_is_discovered_and_ignores_stale_results() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.6-sol")).await;
    let identity = crate::chatwidget::telegram_setup::TelegramBotIdentity {
        id: 99,
        username: "pumps_bot".to_string(),
        display_name: "Pumps".to_string(),
    };
    let stale_generation = chat.begin_telegram_discovery(Some(identity.clone()));
    let generation = chat.begin_telegram_discovery(Some(identity.clone()));

    assert_chatwidget_snapshot!(
        "telegram_setup_auto_discovery",
        render_bottom_popup(&chat, /*width*/ 94)
    );
    assert!(!chat.apply_telegram_discovery(
        stale_generation,
        crate::chatwidget::telegram_setup::TelegramDiscovery {
            identity: identity.clone(),
            candidates: vec![crate::chatwidget::telegram_setup::TelegramChatCandidate {
                chat_id: 7,
                actor_user_id: 7,
                display_name: "Stale chat".to_string(),
                chat_kind: "private".to_string(),
            }],
        },
    ));
    assert!(chat.apply_telegram_discovery(
        generation,
        crate::chatwidget::telegram_setup::TelegramDiscovery {
            identity: identity.clone(),
            candidates: Vec::new(),
        },
    ));
    assert!(!chat.apply_telegram_discovery(
        generation,
        crate::chatwidget::telegram_setup::TelegramDiscovery {
            identity,
            candidates: vec![crate::chatwidget::telegram_setup::TelegramChatCandidate {
                chat_id: 42,
                actor_user_id: 42,
                display_name: "Alice".to_string(),
                chat_kind: "private".to_string(),
            }],
        },
    ));
    let popup = render_bottom_popup(&chat, /*width*/ 94);
    assert!(popup.contains("Alice"));
    assert!(!popup.contains("Stale chat"));

    let cancelled_generation = chat.begin_telegram_discovery(/*identity*/ None);
    chat.bottom_pane
        .dismiss_active_view_if_id(crate::chatwidget::telegram_setup::TELEGRAM_DISCOVERY_VIEW_ID);
    assert!(!chat.apply_telegram_discovery(
        cancelled_generation,
        crate::chatwidget::telegram_setup::TelegramDiscovery {
            identity: crate::chatwidget::telegram_setup::TelegramBotIdentity {
                id: 99,
                username: "pumps_bot".to_string(),
                display_name: "Pumps".to_string(),
            },
            candidates: Vec::new(),
        },
    ));
}

#[tokio::test]
async fn telegram_setup_surfaces_polling_failure_with_a_retry_action() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.6-sol")).await;
    let generation = chat.begin_telegram_discovery(/*identity*/ None);

    chat.telegram_discovery_failed(
        generation,
        "Another process is already polling this bot.".to_string(),
    );

    let popup = render_bottom_popup(&chat, /*width*/ 94);
    assert!(popup.contains("Chat discovery stopped"));
    assert!(popup.contains("Retry chat discovery"));
    assert!(popup.contains("Return to Telegram settings"));
}

#[tokio::test]
async fn wallet_disconnect_wraps_copy_and_dismisses_confirmation_before_replacement() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.config.model_provider_id = PFTERMINAL_PLAN_PROVIDER_ID.to_string();

    chat.confirm_wallet_plan_disconnect();
    assert_eq!(
        chat.bottom_pane.active_view_id(),
        Some(crate::chatwidget::wallet_menu::WALLET_DISCONNECT_PLAN_VIEW_ID)
    );
    for width in [67, 94] {
        let confirmation = render_bottom_popup(&chat, width);
        let normalized = confirmation
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            normalized.contains("remain unchanged."),
            "expected complete disconnect warning at width {width}, got:\n{confirmation}"
        );
    }

    chat.on_wallet_plan_disconnected(Ok(true));

    assert_ne!(
        chat.bottom_pane.active_view_id(),
        Some(crate::chatwidget::wallet_menu::WALLET_DISCONNECT_PLAN_VIEW_ID)
    );
    let replacement = render_bottom_popup(&chat, /*width*/ 94);
    assert!(!replacement.contains("Remove only the metered-inference credential"));
    assert!(
        chat.bottom_pane
            .dismiss_view_by_id(crate::chatwidget::wallet_menu::WALLET_MENU_VIEW_ID)
    );
}

#[tokio::test]
async fn wallet_removal_wraps_copy_and_dismisses_confirmation_before_replacement() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.config.model_provider_id = PFTERMINAL_PLAN_PROVIDER_ID.to_string();

    chat.confirm_wallet_removal("11111111111111111111111111111111".to_string());
    assert_eq!(
        chat.bottom_pane.active_view_id(),
        Some(crate::chatwidget::wallet_menu::WALLET_REMOVE_VIEW_ID)
    );
    for width in [67, 94] {
        let confirmation = render_bottom_popup(&chat, width);
        let normalized = confirmation
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            normalized.contains("on-chain funds and dollar balance remain unchanged."),
            "expected complete removal warning at width {width}, got:\n{confirmation}"
        );
    }

    chat.on_wallet_removed(Ok(()));

    assert_ne!(
        chat.bottom_pane.active_view_id(),
        Some(crate::chatwidget::wallet_menu::WALLET_REMOVE_VIEW_ID)
    );
    let replacement = render_bottom_popup(&chat, /*width*/ 94);
    assert!(!replacement.contains("I have saved the recovery material"));
    assert!(
        chat.bottom_pane
            .dismiss_view_by_id(crate::chatwidget::wallet_menu::WALLET_MENU_VIEW_ID)
    );
}

#[tokio::test]
async fn reopening_wallet_replaces_the_existing_surface_instead_of_stacking() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    chat.open_wallet_menu();
    chat.open_wallet_menu();
    assert_eq!(
        chat.bottom_pane.active_view_id(),
        Some(crate::chatwidget::wallet_menu::WALLET_MENU_VIEW_ID)
    );

    chat.bottom_pane
        .handle_key_event(KeyEvent::from(KeyCode::Esc));

    assert_eq!(chat.bottom_pane.active_view_id(), None);
}

#[tokio::test]
async fn experimental_mode_plan_is_ignored_on_startup() {
    let codex_home = tempdir().expect("tempdir");
    let cfg = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .cli_overrides(vec![
            (
                "features.collaboration_modes".to_string(),
                TomlValue::Boolean(true),
            ),
            (
                "tui.experimental_mode".to_string(),
                TomlValue::String("plan".to_string()),
            ),
        ])
        .build()
        .await
        .expect("config");
    let resolved_model = get_model_offline_for_tests(cfg.model.as_deref());
    let session_telemetry = test_session_telemetry(&cfg, resolved_model.as_str());
    let init = ChatWidgetInit {
        config: cfg.clone(),
        frame_requester: FrameRequester::test_dummy(),
        app_event_tx: AppEventSender::new(unbounded_channel::<AppEvent>().0),
        workspace_command_runner: None,
        initial_user_message: None,
        enhanced_keys_supported: false,
        has_chatgpt_account: false,
        has_codex_backend_auth: false,
        model_catalog: test_model_catalog(&cfg),
        feedback: codex_feedback::CodexFeedback::new(),
        is_first_run: true,
        status_account_display: None,
        runtime_model_provider_base_url: None,
        initial_plan_type: None,
        model: Some(resolved_model.clone()),
        startup_tooltip_override: None,
        status_line_invalid_items_warned: Arc::new(AtomicBool::new(false)),
        terminal_title_invalid_items_warned: Arc::new(AtomicBool::new(false)),
        session_telemetry,
    };

    let chat = ChatWidget::new_with_app_event(init);
    assert_eq!(chat.active_collaboration_mode_kind(), ModeKind::Default);
    assert_eq!(chat.current_model(), resolved_model);
}

#[tokio::test]
async fn plugins_popup_loading_state_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    chat.add_plugins_output();

    let popup = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        popup.contains("Loading available plugins..."),
        "expected /plugins to open in a loading state before the marketplace arrives, got:\n{popup}"
    );
    assert_chatwidget_snapshot!("plugins_popup_loading_state", popup);
}

#[tokio::test]
async fn marketplace_upgrade_loading_popup_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    chat.open_marketplace_upgrade_loading_popup(Some("debug"));

    let popup = render_bottom_popup(&chat, /*width*/ 100);
    let upgrade_lines = popup
        .lines()
        .map(str::trim)
        .filter(|line| line.contains("Upgrading"))
        .collect::<Vec<_>>()
        .join(" | ");
    insta::assert_snapshot!(
        upgrade_lines,
        @"Upgrading debug marketplace... | ›    Upgrading debug marketplace...  This updates when marketplace upgrade completes."
    );
}

#[tokio::test]
async fn marketplace_upgrade_failure_includes_backend_messages_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);
    let cwd = chat.config.cwd.clone();

    chat.on_marketplace_upgrade_loaded(
        cwd.to_path_buf(),
        Ok(MarketplaceUpgradeResponse {
            selected_marketplaces: vec!["debug".to_string(), "tools".to_string()],
            upgraded_roots: Vec::new(),
            errors: vec![
                MarketplaceUpgradeErrorInfo {
                    marketplace_name: "debug".to_string(),
                    message: "git ls-remote marketplace source failed with status 128: authentication failed".to_string(),
                },
                MarketplaceUpgradeErrorInfo {
                    marketplace_name: "tools".to_string(),
                    message: "failed to validate upgraded marketplace root: marketplace root does not contain a supported manifest".to_string(),
                },
            ],
        }),
    );

    let rendered = drain_insert_history(&mut rx)
        .iter()
        .map(|lines| lines_to_single_string(lines))
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(
        rendered.trim(),
        @"■ Failed to upgrade 2 marketplaces: debug: git ls-remote marketplace source failed with status 128: authentication failed; tools: failed to validate upgraded marketplace root: marketplace root does not contain a supported manifest"
    );
}

#[tokio::test]
async fn hooks_popup_shows_list_diagnostics() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let cwd = chat.config.cwd.clone();

    chat.on_hooks_loaded(
        cwd.to_path_buf(),
        Ok(HooksListResponse {
            data: vec![HooksListEntry {
                cwd: cwd.to_path_buf(),
                hooks: Vec::new(),
                warnings: vec!["skipped invalid matcher for PreToolUse".to_string()],
                errors: vec![HookErrorInfo {
                    path: test_path_buf("/tmp/hooks.json"),
                    message: "failed to parse hooks config".to_string(),
                }],
            }],
        }),
    );

    let popup = normalize_snapshot_paths(render_bottom_popup(&chat, /*width*/ 112));
    assert_chatwidget_snapshot!("hooks_popup_shows_list_diagnostics", popup);
}

#[tokio::test]
async fn plugins_popup_snapshot_shows_all_marketplaces_and_sorts_installed_then_name() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let response = plugins_test_response(vec![
        plugins_test_curated_marketplace(vec![
            plugins_test_summary(
                "plugin-bravo",
                "bravo",
                Some("Bravo Search"),
                Some("Search docs and tickets."),
                /*installed*/ false,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
            plugins_test_summary(
                "plugin-alpha",
                "alpha",
                Some("Alpha Sync"),
                Some("Already installed but disabled."),
                /*installed*/ true,
                /*enabled*/ false,
                PluginInstallPolicy::Available,
            ),
            plugins_test_summary(
                "plugin-starter",
                "starter",
                Some("Starter"),
                Some("Included by default."),
                /*installed*/ false,
                /*enabled*/ false,
                PluginInstallPolicy::InstalledByDefault,
            ),
        ]),
        plugins_test_repo_marketplace(vec![plugins_test_summary(
            "plugin-hidden",
            "hidden",
            Some("Hidden Repo Plugin"),
            Some("Should not be shown in /plugins."),
            /*installed*/ false,
            /*enabled*/ true,
            PluginInstallPolicy::Available,
        )]),
    ]);
    render_loaded_plugins_popup(&mut chat, response);
    let popup = render_bottom_popup(&chat, /*width*/ 320);
    assert_chatwidget_snapshot!("plugins_popup_curated_marketplace", popup);
    assert!(
        popup.contains("Hidden Repo Plugin"),
        "expected /plugins to include non-curated marketplaces, got:\n{popup}"
    );
    assert!(
        plugins_test_popup_row_position(&popup, "Alpha Sync")
            < plugins_test_popup_row_position(&popup, "Bravo Search")
            && plugins_test_popup_row_position(&popup, "Bravo Search")
                < plugins_test_popup_row_position(&popup, "Hidden Repo Plugin")
            && plugins_test_popup_row_position(&popup, "Hidden Repo Plugin")
                < plugins_test_popup_row_position(&popup, "Starter"),
        "expected /plugins rows to sort installed plugins first, then alphabetically, got:\n{popup}"
    );
}

#[tokio::test]
async fn plugins_popup_keeps_loaded_marketplace_state_with_load_errors() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let mut response = plugins_test_response(vec![plugins_test_curated_marketplace(Vec::new())]);
    response
        .marketplace_load_errors
        .push(MarketplaceLoadErrorInfo {
            marketplace_path: plugins_test_absolute_path("marketplaces/broken"),
            message: "failed to load marketplace manifest".to_string(),
        });

    let popup = render_loaded_plugins_popup(&mut chat, response);
    assert!(
        popup.contains("No marketplace plugins available")
            && !popup.contains("Marketplace unavailable"),
        "expected /plugins to keep the loaded marketplace state, got:\n{popup}"
    );
}

#[tokio::test]
async fn plugins_popup_truncates_long_descriptions_in_list_rows() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let response = plugins_test_response(vec![plugins_test_curated_marketplace(vec![
        plugins_test_summary(
            "plugin-alpha",
            "alpha",
            Some("Alpha"),
            Some("Short description."),
            /*installed*/ false,
            /*enabled*/ true,
            PluginInstallPolicy::Available,
        ),
        plugins_test_summary(
            "plugin-verbose",
            "verbose",
            Some("Verbose Plugin"),
            Some("This description keeps going and going until the row would normally wrap."),
            /*installed*/ false,
            /*enabled*/ true,
            PluginInstallPolicy::Available,
        ),
    ])]);

    let cwd = chat.config.cwd.to_path_buf();
    chat.on_plugins_loaded(cwd, Ok(response));
    chat.add_plugins_output();

    let popup = render_bottom_popup(&chat, /*width*/ 70);
    let verbose_row = popup
        .lines()
        .find(|line| line.contains("Verbose Plugin"))
        .expect("expected verbose plugin row in popup");
    insta::assert_snapshot!(
        verbose_row,
        @"  [-] Verbose Plugin  Available · OpenAI Curated · This description…"
    );
    assert!(
        !popup
            .contains("This description keeps going and going until the row would normally wrap."),
        "expected the long plugin description to truncate instead of wrapping, got:\n{popup}"
    );
}

#[tokio::test]
async fn plugins_popup_add_marketplace_tab_opens_prompt_and_submits_source() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let cwd = chat.config.cwd.to_path_buf();
    render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![plugins_test_curated_marketplace(Vec::new())]),
    );

    while rx.try_recv().is_ok() {}
    let popup = select_plugins_tab_containing(
        &mut chat,
        /*width*/ 100,
        "Add a marketplace from a Git repo or local root.",
    );
    assert!(
        popup.contains("Add a marketplace from a Git repo or local root."),
        "expected Add marketplace tab, got:\n{popup}"
    );

    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    match rx.try_recv() {
        Ok(AppEvent::OpenMarketplaceAddPrompt) => {}
        other => panic!("expected OpenMarketplaceAddPrompt event, got {other:?}"),
    }

    chat.open_marketplace_add_prompt();
    let prompt = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        prompt.contains("owner/repo, git URL, or local marketplace path"),
        "expected marketplace source prompt, got:\n{prompt}"
    );

    chat.handle_paste("owner/repo".to_string());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    match rx.try_recv() {
        Ok(AppEvent::OpenMarketplaceAddLoading { source }) => {
            assert_eq!(source, "owner/repo");
        }
        other => panic!("expected OpenMarketplaceAddLoading event, got {other:?}"),
    }
    match rx.try_recv() {
        Ok(AppEvent::FetchMarketplaceAdd {
            cwd: event_cwd,
            source,
        }) => {
            assert_eq!(event_cwd, cwd);
            assert_eq!(source, "owner/repo");
        }
        other => panic!("expected FetchMarketplaceAdd event, got {other:?}"),
    }
}

#[tokio::test]
async fn plugins_popup_upgrades_user_configured_git_marketplace_from_marketplace_tab() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let cwd = chat.config.cwd.to_path_buf();
    let temp = tempdir().expect("tempdir");
    let config_toml_path = temp.path().join("config.toml").abs();
    chat.config.config_layer_stack = ConfigLayerStack::default().with_user_config(
        &config_toml_path,
        toml::from_str::<TomlValue>(
            "[marketplaces.repo]\nsource_type = \"git\"\nsource = \"https://github.com/owner/repo.git\"\n",
        )
        .expect("marketplace config"),
    )
    .expect("marketplace user config should be valid");

    render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![
            plugins_test_curated_marketplace(Vec::new()),
            plugins_test_repo_marketplace(vec![plugins_test_summary(
                "plugin-debug",
                "debug",
                Some("Debug Plugin"),
                Some("Debug marketplace plugin."),
                /*installed*/ false,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            )]),
        ]),
    );

    while rx.try_recv().is_ok() {}
    let popup = select_plugins_tab_containing(&mut chat, /*width*/ 100, "Repo Marketplace.");
    assert!(
        popup.contains("Repo Marketplace.")
            && popup.contains("ctrl + u upgrade")
            && popup.contains("ctrl + r remove")
            && popup.contains("Debug Plugin"),
        "expected upgradeable user-configured marketplace tab, got:\n{popup}"
    );

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    chat.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));

    match rx.try_recv() {
        Ok(AppEvent::OpenMarketplaceUpgradeLoading { marketplace_name }) => {
            assert_eq!(marketplace_name, Some("repo".to_string()));
        }
        other => panic!("expected OpenMarketplaceUpgradeLoading event, got {other:?}"),
    }
    match rx.try_recv() {
        Ok(AppEvent::FetchMarketplaceUpgrade {
            cwd: event_cwd,
            marketplace_name,
        }) => {
            assert_eq!(event_cwd, cwd);
            assert_eq!(marketplace_name, Some("repo".to_string()));
        }
        other => panic!("expected FetchMarketplaceUpgrade event, got {other:?}"),
    }
    let no_more_events = rx.try_recv();
    assert!(
        no_more_events.is_err(),
        "expected no duplicate marketplace upgrade events, got {no_more_events:?}"
    );
}

#[tokio::test]
async fn marketplace_add_success_refreshes_to_new_marketplace_tab() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let cwd = chat.config.cwd.to_path_buf();
    let marketplace_root = plugins_test_absolute_path("marketplaces/debug");
    let marketplace_path =
        plugins_test_absolute_path("marketplaces/debug/.agents/plugins/marketplace.json");
    let temp = tempdir().expect("tempdir");
    let config_toml_path = temp.path().join("config.toml").abs();
    chat.config.config_layer_stack = ConfigLayerStack::default().with_user_config(
        &config_toml_path,
        toml::from_str::<TomlValue>(
            "[marketplaces.debug]\nsource_type = \"git\"\nsource = \"https://github.com/owner/debug.git\"\n",
        )
        .expect("marketplace config"),
    )
    .expect("marketplace user config should be valid");
    render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![plugins_test_curated_marketplace(Vec::new())]),
    );
    chat.open_marketplace_add_loading_popup("owner/repo");
    let loading_popup = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        !loading_popup.contains("owner/repo"),
        "expected marketplace loading popup to avoid echoing the source, got:\n{loading_popup}"
    );
    chat.on_marketplace_add_loaded(
        cwd.clone(),
        "owner/repo".to_string(),
        Ok(MarketplaceAddResponse {
            marketplace_name: "debug".to_string(),
            installed_root: marketplace_root,
            already_added: false,
        }),
    );
    chat.on_plugins_loaded(
        cwd,
        Ok(plugins_test_response(vec![
            plugins_test_curated_marketplace(Vec::new()),
            PluginMarketplaceEntry {
                name: "debug".to_string(),
                path: Some(marketplace_path),
                interface: Some(MarketplaceInterface {
                    display_name: Some("Debug Marketplace".to_string()),
                }),
                plugins: vec![plugins_test_summary(
                    "plugin-debug",
                    "debug",
                    Some("Debug Plugin"),
                    Some("Debug marketplace plugin."),
                    /*installed*/ false,
                    /*enabled*/ true,
                    PluginInstallPolicy::Available,
                )],
            },
        ])),
    );

    let popup = render_bottom_popup(&chat, /*width*/ 100);
    assert_chatwidget_snapshot!("plugins_popup_newly_installed_marketplace", popup);
    assert!(
        popup.contains("Debug Marketplace installed successfully.")
            && popup.contains("ctrl + u upgrade")
            && popup.contains("ctrl + r remove")
            && popup.contains("Debug Plugin"),
        "expected marketplace add refresh to switch to the new marketplace tab, got:\n{popup}"
    );

    chat.handle_key_event(KeyEvent::from(KeyCode::Esc));
    chat.add_plugins_output();
    let reopened_popup = (0..8)
        .find_map(|_| {
            let popup = render_bottom_popup(&chat, /*width*/ 100);
            if popup.contains("[Debug Marketplace]") {
                Some(popup)
            } else {
                chat.handle_key_event(KeyEvent::from(KeyCode::Right));
                None
            }
        })
        .unwrap_or_else(|| {
            let popup = render_bottom_popup(&chat, /*width*/ 100);
            panic!("expected Debug Marketplace tab after reopening, got:\n{popup}");
        });
    assert!(
        reopened_popup.contains("Installed 0 of 1 Debug Marketplace plugins.")
            && !reopened_popup.contains("installed successfully"),
        "expected reopening the marketplace tab later to use the normal header, got:\n{reopened_popup}"
    );
}

#[tokio::test]
async fn plugins_popup_removes_user_configured_marketplace_flow() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);
    let cwd = chat.config.cwd.to_path_buf();
    let temp = tempdir().expect("tempdir");
    let config_toml_path = temp.path().join("config.toml").abs();
    chat.config.config_layer_stack = ConfigLayerStack::default().with_user_config(
        &config_toml_path,
        toml::from_str::<TomlValue>(
            "[marketplaces.repo]\nsource_type = \"git\"\nsource = \"https://github.com/owner/repo.git\"\n",
        )
        .expect("marketplace config"),
    )
    .expect("marketplace user config should be valid");

    render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![
            plugins_test_curated_marketplace(Vec::new()),
            plugins_test_repo_marketplace(vec![plugins_test_summary(
                "plugin-debug",
                "debug",
                Some("Debug Plugin"),
                Some("Debug marketplace plugin."),
                /*installed*/ false,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            )]),
        ]),
    );
    while rx.try_recv().is_ok() {}

    let repo_tab =
        select_plugins_tab_containing(&mut chat, /*width*/ 100, "Repo Marketplace.");
    assert!(
        repo_tab.contains("Repo Marketplace.")
            && repo_tab.contains("ctrl + u upgrade")
            && repo_tab.contains("ctrl + r remove")
            && repo_tab.contains("Debug Plugin"),
        "expected removable user-configured marketplace tab, got:\n{repo_tab}"
    );

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
    let confirmation = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        confirmation.contains("Remove Repo Marketplace marketplace?")
            && confirmation.contains("Remove marketplace")
            && confirmation.contains("Back to plugins"),
        "expected marketplace removal confirmation, got:\n{confirmation}"
    );
    assert_chatwidget_snapshot!(
        "plugins_popup_marketplace_remove_confirmation",
        confirmation
    );

    chat.handle_key_event(KeyEvent::from(KeyCode::Up));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let marketplace_display_name = match rx.try_recv() {
        Ok(AppEvent::OpenMarketplaceRemoveLoading {
            marketplace_display_name,
        }) => marketplace_display_name,
        other => panic!("expected OpenMarketplaceRemoveLoading event, got {other:?}"),
    };
    assert_eq!(marketplace_display_name, "Repo Marketplace");
    match rx.try_recv() {
        Ok(AppEvent::FetchMarketplaceRemove {
            cwd: event_cwd,
            marketplace_name,
            marketplace_display_name,
        }) => {
            assert_eq!(event_cwd, cwd);
            assert_eq!(marketplace_name, "repo");
            assert_eq!(marketplace_display_name, "Repo Marketplace");
        }
        other => panic!("expected FetchMarketplaceRemove event, got {other:?}"),
    }

    chat.open_marketplace_remove_loading_popup(&marketplace_display_name);
    let loading = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        loading.contains("Removing Repo Marketplace...")
            && loading.contains("Removing marketplace..."),
        "expected marketplace removal loading state, got:\n{loading}"
    );

    chat.on_marketplace_remove_loaded(
        cwd.clone(),
        "repo".to_string(),
        marketplace_display_name,
        Ok(MarketplaceRemoveResponse {
            marketplace_name: "repo".to_string(),
            installed_root: Some(plugins_test_absolute_path("marketplaces/repo")),
        }),
    );
    chat.on_plugins_loaded(
        cwd,
        Ok(plugins_test_response(vec![
            plugins_test_curated_marketplace(Vec::new()),
        ])),
    );

    let refreshed = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        refreshed.contains("Browse plugins from available marketplaces.")
            && !refreshed.contains("Repo Marketplace")
            && !refreshed.contains("Debug Plugin")
            && !refreshed.contains("ctrl + r remove"),
        "expected refreshed plugin list without removed marketplace, got:\n{refreshed}"
    );
}

#[tokio::test]
async fn plugin_detail_popup_snapshot_labels_personal_marketplace_as_local() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let marketplace_name = "personal-marketplace";
    let personal_marketplace_path = AbsolutePathBuf::try_from(
        dirs::home_dir()
            .expect("home directory")
            .join(".agents/plugins/marketplace.json"),
    )
    .expect("absolute personal marketplace path");
    let summary = plugins_test_summary(
        "plugin-figma",
        "figma",
        Some("Figma"),
        Some("Design handoff."),
        /*installed*/ false,
        /*enabled*/ true,
        PluginInstallPolicy::Available,
    );
    let response = plugins_test_response(vec![PluginMarketplaceEntry {
        name: marketplace_name.to_string(),
        path: Some(personal_marketplace_path.clone()),
        interface: None,
        plugins: vec![summary.clone()],
    }]);
    let cwd = chat.config.cwd.clone();
    chat.on_plugins_loaded(cwd.to_path_buf(), Ok(response));
    chat.add_plugins_output();
    let mut plugin = plugins_test_detail(
        summary,
        Some("Turn Figma files into implementation context."),
        &["design-review", "extract-copy"],
        &[
            (codex_app_server_protocol::HookEventName::PreToolUse, 1),
            (codex_app_server_protocol::HookEventName::Stop, 2),
        ],
        &["Figma", "Slack"],
        &["figma-mcp", "docs-mcp"],
    );
    plugin.marketplace_name = marketplace_name.to_string();
    plugin.marketplace_path = Some(personal_marketplace_path);
    chat.on_plugin_detail_loaded(cwd.to_path_buf(), Ok(PluginReadResponse { plugin }));

    let popup = render_bottom_popup(&chat, /*width*/ 100);
    assert_chatwidget_snapshot!(
        "plugin_detail_popup_installable",
        strip_osc8_for_snapshot(&popup)
    );
}

#[tokio::test]
async fn plugin_detail_popup_snapshot_shows_npm_source() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let mut summary = plugins_test_summary(
        "plugin-figma",
        "figma",
        Some("Figma"),
        Some("Design handoff."),
        /*installed*/ false,
        /*enabled*/ true,
        PluginInstallPolicy::Available,
    );
    summary.source = PluginSource::Npm {
        package: "@acme/figma-plugin".to_string(),
        version: Some("^1.2.0".to_string()),
        registry: Some("https://npm.example.com".to_string()),
    };
    let response = plugins_test_response(vec![plugins_test_curated_marketplace(vec![
        summary.clone(),
    ])]);
    let cwd = chat.config.cwd.clone();
    chat.on_plugins_loaded(cwd.to_path_buf(), Ok(response));
    chat.add_plugins_output();
    let plugin = plugins_test_detail(
        summary,
        Some("Turn Figma files into implementation context."),
        &["design-review", "extract-copy"],
        &[
            (codex_app_server_protocol::HookEventName::PreToolUse, 1),
            (codex_app_server_protocol::HookEventName::Stop, 2),
        ],
        &["Figma", "Slack"],
        &["figma-mcp", "docs-mcp"],
    );
    chat.on_plugin_detail_loaded(cwd.to_path_buf(), Ok(PluginReadResponse { plugin }));

    let popup = render_bottom_popup(&chat, /*width*/ 100);
    assert_chatwidget_snapshot!(
        "plugin_detail_popup_npm_source",
        strip_osc8_for_snapshot(&popup)
    );
}

#[tokio::test]
async fn plugin_detail_popup_distinguishes_admin_installed_from_enabled() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);
    let summary = plugins_test_summary(
        "plugin-figma",
        "figma",
        Some("Figma"),
        Some("Design handoff."),
        /*installed*/ true,
        /*enabled*/ true,
        PluginInstallPolicy::InstalledByDefault,
    );
    let response = plugins_test_response(vec![plugins_test_curated_marketplace(vec![
        summary.clone(),
    ])]);
    let cwd = chat.config.cwd.clone();
    chat.on_plugins_loaded(cwd.to_path_buf(), Ok(response));
    chat.add_plugins_output();
    let mut plugin = plugins_test_detail(
        summary,
        Some("Turn Figma files into implementation context."),
        &["design-review", "extract-copy"],
        &[
            (codex_app_server_protocol::HookEventName::PreToolUse, 1),
            (codex_app_server_protocol::HookEventName::Stop, 2),
        ],
        &["Figma", "Slack"],
        &["figma-mcp", "docs-mcp"],
    );
    chat.on_plugin_detail_loaded(
        cwd.to_path_buf(),
        Ok(PluginReadResponse {
            plugin: plugin.clone(),
        }),
    );

    let popup = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        popup.contains("Installed by admin")
            && !popup.contains("Uninstall plugin")
            && !popup.contains("Data shared with this app is subject to the app's"),
        "expected admin-installed plugin details to block uninstall and hide the disclosure line, got:\n{popup}"
    );
    assert_chatwidget_snapshot!(
        "plugin_detail_popup_installed",
        strip_osc8_for_snapshot(&popup)
    );

    plugin.summary.installed = false;
    plugin.summary.enabled = false;
    chat.on_plugin_detail_loaded(cwd.to_path_buf(), Ok(PluginReadResponse { plugin }));
    let popup = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        popup.contains("Enabled by Admin")
            && popup.contains("Install plugin")
            && !popup.contains("Installed by admin"),
        "expected an unmaterialized default plugin to show its admin assignment and remain installable, got:\n{popup}"
    );
    insta::assert_snapshot!(
        popup
            .lines()
            .find(|line| line.contains("Figma ·"))
            .expect("expected plugin detail header")
            .trim(),
        @"Figma · Enabled by Admin · ChatGPT Marketplace"
    );
}

#[tokio::test]
async fn plugins_popup_remote_row_opens_remote_detail() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let popup = render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![PluginMarketplaceEntry {
            name: "workspace-directory".to_string(),
            path: None,
            interface: Some(MarketplaceInterface {
                display_name: Some("Workspace".to_string()),
            }),
            plugins: vec![plugins_test_remote_summary(
                "plugins~Plugin_calendar",
                "calendar",
                Some("Calendar"),
                Some("Workspace schedules."),
                /*installed*/ false,
            )],
        }]),
    );
    let remote_row = popup
        .lines()
        .find(|line| line.contains("Calendar"))
        .expect("expected remote plugin row");
    assert!(
        remote_row.contains("Available")
            && remote_row.contains("Press Enter to install or view plugin details."),
        "expected remote plugin row to be viewable, got:\n{remote_row}"
    );

    while rx.try_recv().is_ok() {}
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    match rx.try_recv() {
        Ok(AppEvent::OpenPluginDetailLoading {
            plugin_display_name,
        }) => {
            assert_eq!(plugin_display_name, "Calendar");
        }
        other => panic!("expected OpenPluginDetailLoading event, got {other:?}"),
    }
    match rx.try_recv() {
        Ok(AppEvent::FetchPluginDetail { cwd: _, params }) => {
            assert_eq!(params.marketplace_path, None);
            assert_eq!(
                params.remote_marketplace_name,
                Some("workspace-directory".to_string())
            );
            assert_eq!(params.plugin_name, "plugins~Plugin_calendar");
        }
        other => panic!("expected FetchPluginDetail event, got {other:?}"),
    }
}

#[tokio::test]
async fn plugin_detail_unmaterialized_default_uses_remote_install_path() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let summary = PluginSummary {
        install_policy: PluginInstallPolicy::InstalledByDefault,
        ..plugins_test_remote_summary(
            "plugins~Plugin_linear",
            "linear",
            Some("Linear"),
            Some("Issue tracking."),
            /*installed*/ false,
        )
    };
    let cwd = chat.config.cwd.clone();
    chat.on_plugins_loaded(
        cwd.to_path_buf(),
        Ok(plugins_test_response(vec![PluginMarketplaceEntry {
            name: "workspace-shared-with-me-private".to_string(),
            path: None,
            interface: Some(MarketplaceInterface {
                display_name: Some("Shared with me".to_string()),
            }),
            plugins: vec![summary.clone()],
        }])),
    );
    chat.add_plugins_output();
    chat.on_plugin_detail_loaded(
        cwd.to_path_buf(),
        Ok(PluginReadResponse {
            plugin: PluginDetail {
                marketplace_name: "workspace-shared-with-me-private".to_string(),
                marketplace_path: None,
                summary,
                share_url: None,
                description: Some("Install shared Linear plugin.".to_string()),
                skills: Vec::new(),
                hooks: Vec::new(),
                apps: Vec::new(),
                app_templates: Vec::new(),
                mcp_servers: Vec::new(),
                scheduled_tasks: None,
            },
        }),
    );
    let popup = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        popup.contains("Install plugin") && popup.contains("Install this plugin now."),
        "expected remote detail to offer install, got:\n{popup}"
    );

    while rx.try_recv().is_ok() {}
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    match rx.try_recv() {
        Ok(AppEvent::OpenPluginInstallLoading {
            plugin_display_name,
        }) => {
            assert_eq!(plugin_display_name, "Linear");
        }
        other => panic!("expected OpenPluginInstallLoading event, got {other:?}"),
    }
    match rx.try_recv() {
        Ok(AppEvent::FetchPluginInstall {
            cwd: _,
            location: crate::app_event::PluginLocation::Remote { marketplace_name },
            plugin_name,
            plugin_display_name,
        }) => {
            assert_eq!(marketplace_name, "workspace-shared-with-me-private");
            assert_eq!(plugin_name, "plugins~Plugin_linear");
            assert_eq!(plugin_display_name, "Linear");
        }
        other => panic!("expected remote FetchPluginInstall event, got {other:?}"),
    }
}

#[tokio::test]
async fn plugin_detail_remote_uninstall_uses_remote_plugin_id() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let summary = plugins_test_remote_summary(
        "plugins~Plugin_linear",
        "linear",
        Some("Linear"),
        Some("Issue tracking."),
        /*installed*/ true,
    );
    let cwd = chat.config.cwd.clone();
    chat.on_plugins_loaded(
        cwd.to_path_buf(),
        Ok(plugins_test_response(vec![
            plugins_test_remote_marketplace(
                "workspace-shared-with-me-private",
                "Shared with me",
                vec![summary.clone()],
            ),
        ])),
    );
    chat.add_plugins_output();
    chat.on_plugin_detail_loaded(
        cwd.to_path_buf(),
        Ok(PluginReadResponse {
            plugin: plugins_test_remote_detail(
                "workspace-shared-with-me-private",
                summary,
                Some("Installed shared Linear plugin."),
            ),
        }),
    );

    while rx.try_recv().is_ok() {}
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    match rx.try_recv() {
        Ok(AppEvent::OpenPluginUninstallLoading {
            plugin_display_name,
        }) => {
            assert_eq!(plugin_display_name, "Linear");
        }
        other => panic!("expected OpenPluginUninstallLoading event, got {other:?}"),
    }
    match rx.try_recv() {
        Ok(AppEvent::FetchPluginUninstall {
            plugin_id,
            plugin_display_name,
            ..
        }) => {
            assert_eq!(plugin_id, "plugins~Plugin_linear");
            assert_eq!(plugin_display_name, "Linear");
        }
        other => panic!("expected remote FetchPluginUninstall event, got {other:?}"),
    }
}

#[tokio::test]
async fn plugin_detail_remote_without_remote_id_disables_uninstall_action() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let summary = PluginSummary {
        source: PluginSource::Remote,
        ..plugins_test_summary(
            "linear@workspace-shared-with-me-private",
            "linear",
            Some("Linear"),
            Some("Issue tracking."),
            /*installed*/ true,
            /*enabled*/ true,
            PluginInstallPolicy::Available,
        )
    };
    let cwd = chat.config.cwd.clone();
    chat.on_plugins_loaded(
        cwd.to_path_buf(),
        Ok(plugins_test_response(vec![
            plugins_test_remote_marketplace(
                "workspace-shared-with-me-private",
                "Shared with me",
                vec![summary.clone()],
            ),
        ])),
    );
    chat.add_plugins_output();
    chat.on_plugin_detail_loaded(
        cwd.to_path_buf(),
        Ok(PluginReadResponse {
            plugin: plugins_test_remote_detail(
                "workspace-shared-with-me-private",
                summary,
                Some("Installed shared Linear plugin."),
            ),
        }),
    );

    let popup = render_bottom_popup(&chat, /*width*/ 120);
    assert!(
        popup.contains("This remote plugin did not provide an uninstall identity.")
            && !popup.contains("Remove this plugin now."),
        "expected missing remote ID to disable uninstall, got:\n{popup}"
    );

    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    assert_eq!(
        render_bottom_popup(&chat, /*width*/ 120),
        popup,
        "expected navigation to skip the disabled uninstall row"
    );
}

#[tokio::test]
async fn plugin_detail_popup_shows_local_share_context_as_read_only_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let summary = PluginSummary {
        share_context: Some(PluginShareContext {
            remote_plugin_id: "plugins~Plugin_docs".to_string(),
            remote_version: Some("7".to_string()),
            discoverability: Some(PluginShareDiscoverability::Private),
            share_url: Some("https://chatgpt.com/codex/plugins/share/docs".to_string()),
            creator_account_user_id: None,
            creator_name: Some("Test User".to_string()),
            share_principals: None,
            can_publish_to_workspace: None,
        }),
        ..plugins_test_summary(
            "plugin-docs",
            "docs",
            Some("Docs"),
            Some("Workspace docs."),
            /*installed*/ false,
            /*enabled*/ true,
            PluginInstallPolicy::Available,
        )
    };
    let response = plugins_test_response(vec![plugins_test_curated_marketplace(vec![
        summary.clone(),
    ])]);
    let cwd = chat.config.cwd.clone();
    chat.on_plugins_loaded(cwd.to_path_buf(), Ok(response));
    chat.add_plugins_output();
    let mut detail = plugins_test_detail(summary, Some("Workspace docs."), &[], &[], &[], &[]);
    detail.marketplace_path = Some(plugins_test_personal_marketplace_path());
    chat.on_plugin_detail_loaded(cwd.to_path_buf(), Ok(PluginReadResponse { plugin: detail }));

    let popup = render_bottom_popup(&chat, /*width*/ 120);
    assert_chatwidget_snapshot!(
        "plugin_detail_popup_local_share_read_only",
        strip_osc8_for_snapshot(&popup)
    );
}

#[tokio::test]
async fn plugin_detail_popup_shows_admin_disabled_status_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let summary = PluginSummary {
        availability: PluginAvailability::DisabledByAdmin,
        ..plugins_test_summary(
            "plugin-admin-blocked",
            "admin-blocked",
            Some("Admin Blocked"),
            Some("Blocked by policy."),
            /*installed*/ false,
            /*enabled*/ true,
            PluginInstallPolicy::Available,
        )
    };
    let response = plugins_test_response(vec![plugins_test_curated_marketplace(vec![
        summary.clone(),
    ])]);
    let cwd = chat.config.cwd.clone();
    chat.on_plugins_loaded(cwd.to_path_buf(), Ok(response));
    chat.add_plugins_output();
    chat.on_plugin_detail_loaded(
        cwd.to_path_buf(),
        Ok(PluginReadResponse {
            plugin: plugins_test_detail(summary, Some("Blocked by policy."), &[], &[], &[], &[]),
        }),
    );

    let popup = render_bottom_popup(&chat, /*width*/ 120);
    let status_row = popup
        .lines()
        .find(|line| line.contains("Disabled by admin"))
        .expect("expected admin-disabled status row");
    insta::assert_snapshot!(
        status_row,
        @"  Admin Blocked · Disabled by admin · ChatGPT Marketplace"
    );
    assert!(
        popup.contains("This plugin is disabled by your workspace admin.")
            && !popup.contains("Install this plugin now."),
        "expected admin-disabled detail to block install, got:\n{popup}"
    );

    while rx.try_recv().is_ok() {}
    assert!(
        rx.try_recv().is_err(),
        "expected no action after rendering disabled install state"
    );
}

#[tokio::test]
async fn plugins_popup_admin_disabled_installed_plugin_has_no_toggle_hint() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let summary = PluginSummary {
        availability: PluginAvailability::DisabledByAdmin,
        ..plugins_test_summary(
            "plugin-admin-blocked",
            "admin-blocked",
            Some("Admin Blocked"),
            Some("Blocked by policy."),
            /*installed*/ true,
            /*enabled*/ true,
            PluginInstallPolicy::Available,
        )
    };
    render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![plugins_test_curated_marketplace(vec![summary])]),
    );

    let popup = render_bottom_popup(&chat, /*width*/ 120);
    assert_chatwidget_snapshot!("plugins_popup_admin_disabled_installed", popup);
    assert!(
        popup.contains("[!] Admin Blocked")
            && popup.contains("Disabled")
            && popup.contains("Press Enter to view plugin details.")
            && !popup.contains("Disabled by admin")
            && !popup.contains("Space to disable"),
        "expected admin-disabled installed row to omit toggle hint, got:\n{popup}"
    );

    while rx.try_recv().is_ok() {}
    let before = render_bottom_popup(&chat, /*width*/ 120);
    chat.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    let after = render_bottom_popup(&chat, /*width*/ 120);
    assert!(
        rx.try_recv().is_err(),
        "space should not toggle admin-disabled installed plugins"
    );
    assert_eq!(after, before);
}

#[tokio::test]
async fn plugins_popup_admin_disabled_available_plugin_has_view_only_hint() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let summary = PluginSummary {
        availability: PluginAvailability::DisabledByAdmin,
        ..plugins_test_summary(
            "plugin-admin-blocked",
            "admin-blocked",
            Some("Admin Blocked"),
            Some("Blocked by policy."),
            /*installed*/ false,
            /*enabled*/ true,
            PluginInstallPolicy::Available,
        )
    };
    render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![plugins_test_curated_marketplace(vec![summary])]),
    );

    let popup = render_bottom_popup(&chat, /*width*/ 100);
    let admin_blocked_row = popup
        .lines()
        .find(|line| line.contains("Admin Blocked"))
        .expect("expected admin-disabled plugin row");
    assert!(
        admin_blocked_row.contains("Press Enter to view plugin details.")
            && !admin_blocked_row.contains("install or view"),
        "expected admin-disabled available plugin to stay view-only, got:\n{admin_blocked_row}"
    );
}

#[tokio::test]
async fn plugins_popup_remote_section_fallback_states_when_remote_plugin_disabled_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);
    chat.set_feature_enabled(Feature::RemotePlugin, /*enabled*/ false);

    let select_tab_containing = |chat: &mut ChatWidget, visible_text: &str| -> String {
        for _ in 0..8 {
            let popup = render_bottom_popup(chat, /*width*/ 100);
            if popup.contains(visible_text) {
                return popup;
            }
            chat.handle_key_event(KeyEvent::from(KeyCode::Right));
        }

        let popup = render_bottom_popup(chat, /*width*/ 100);
        panic!("expected plugins tab containing {visible_text:?}, got:\n{popup}");
    };
    let remote_section_state = |popup: &str| -> String {
        let header = popup
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .nth(1)
            .expect("expected remote section header");
        let item = popup
            .lines()
            .find_map(|line| line.trim_start().strip_prefix('›'))
            .expect("expected selected remote section item")
            .trim();
        format!("{header}\n{item}")
    };

    chat.add_plugins_output();
    let cwd = chat.config.cwd.clone();
    chat.on_plugins_loaded(
        cwd.to_path_buf(),
        Ok(plugins_test_response(vec![
            plugins_test_curated_marketplace(Vec::new()),
        ])),
    );
    let curated_loading_popup =
        select_tab_containing(&mut chat, "Loading OpenAI Curated plugins...");
    let workspace_loading_popup = select_tab_containing(&mut chat, "Loading Workspace plugins.");
    let shared_loading_popup = select_tab_containing(&mut chat, "Loading Shared with me plugins.");
    let _ = select_tab_containing(&mut chat, "Loading Workspace plugins.");

    chat.on_plugin_remote_sections_loaded(cwd.to_path_buf(), Vec::new(), Vec::new());
    let loaded_popup = render_bottom_popup(&chat, /*width*/ 100);
    assert_chatwidget_snapshot!("plugins_popup_empty_shared_section_hidden", loaded_popup);
    assert!(
        !loaded_popup.contains("Shared with me"),
        "expected empty shared section to stay hidden, got:\n{loaded_popup}"
    );

    chat.on_plugin_remote_sections_loaded(
        cwd.to_path_buf(),
        Vec::new(),
        vec![crate::app_event::PluginRemoteSectionError {
            section_id: "workspace".to_string(),
            label: "Workspace".to_string(),
            message: "Sign in to ChatGPT to load workspace plugins.".to_string(),
        }],
    );
    let workspace_error_popup = select_tab_containing(&mut chat, "Workspace unavailable.");

    let (mut remote_chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    remote_chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);
    remote_chat.add_plugins_output();
    let remote_cwd = remote_chat.config.cwd.clone();
    remote_chat.on_plugins_loaded(
        remote_cwd.to_path_buf(),
        Ok(plugins_test_response(vec![
            plugins_test_curated_marketplace(Vec::new()),
        ])),
    );
    let remote_curated_empty_popup =
        select_tab_containing(&mut remote_chat, "No OpenAI Curated plugins available");

    insta::assert_snapshot!(
        [
            remote_section_state(&curated_loading_popup),
            remote_section_state(&workspace_loading_popup),
            remote_section_state(&shared_loading_popup),
            remote_section_state(&workspace_error_popup),
            remote_section_state(&remote_curated_empty_popup),
        ]
        .join("\n\n"),
        @r###"
        OpenAI Curated marketplace.
        Loading OpenAI Curated plugins...  This updates when OpenAI Curated plugins finish loading.

        Loading Workspace plugins.
        Loading Workspace plugins...  This updates when workspace plugins finish loading.

        Loading Shared with me plugins.
        Loading Shared with me plugins...  This updates when shared plugins finish loading.

        Workspace unavailable.
        Workspace unavailable  Sign in to ChatGPT to load workspace plugins.

        OpenAI Curated marketplace.
        No OpenAI Curated plugins available  No OpenAI Curated plugins available.
        "###
    );
}

#[tokio::test]
async fn plugins_popup_remote_detail_tracks_physical_and_policy_install_state() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let remote_plugin_id = "plugins~Plugin_docs";
    let remote_marketplace_name = "workspace-shared-with-me-private";
    let mut local_summary = PluginSummary {
        share_context: Some(PluginShareContext {
            remote_plugin_id: remote_plugin_id.to_string(),
            remote_version: None,
            discoverability: None,
            share_url: None,
            creator_account_user_id: None,
            creator_name: None,
            share_principals: None,
            can_publish_to_workspace: None,
        }),
        ..plugins_test_summary(
            "plugin-docs",
            "docs",
            Some("Docs"),
            Some("Local editable docs plugin."),
            /*installed*/ false,
            /*enabled*/ true,
            PluginInstallPolicy::InstalledByDefault,
        )
    };
    let popup = render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![
            plugins_test_curated_marketplace(vec![local_summary.clone()]),
            PluginMarketplaceEntry {
                name: remote_marketplace_name.to_string(),
                path: None,
                interface: Some(MarketplaceInterface {
                    display_name: Some("Shared with me".to_string()),
                }),
                plugins: vec![plugins_test_remote_summary(
                    remote_plugin_id,
                    "docs",
                    Some("Docs"),
                    Some("Shared docs plugin."),
                    /*installed*/ true,
                )],
            },
        ]),
    );
    let all_plugins_row = popup
        .lines()
        .find(|line| line.contains("Docs"))
        .expect("expected all-plugins row");
    assert!(
        popup.contains("Installed 1 of 1 available plugins.")
            && all_plugins_row.contains("Installed")
            && !all_plugins_row.contains("Available"),
        "expected installed remote duplicate to win over local mapped share, got:\n{popup}"
    );

    chat.handle_key_event(KeyEvent::from(KeyCode::Right));
    let installed_popup = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        installed_popup.contains("Showing 1 installed plugins.")
            && installed_popup.contains("Docs"),
        "expected installed remote duplicate in the Installed tab, got:\n{installed_popup}"
    );

    while rx.try_recv().is_ok() {}
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    match rx.try_recv() {
        Ok(AppEvent::OpenPluginDetailLoading {
            plugin_display_name,
        }) => {
            assert_eq!(plugin_display_name, "Docs");
        }
        other => panic!("expected OpenPluginDetailLoading event, got {other:?}"),
    }
    match rx.try_recv() {
        Ok(AppEvent::FetchPluginDetail { params, .. }) => {
            assert_eq!(params.marketplace_path, None);
            assert_eq!(
                params.remote_marketplace_name,
                Some(remote_marketplace_name.to_string())
            );
            assert_eq!(params.plugin_name, remote_plugin_id);
        }
        other => panic!("expected FetchPluginDetail event, got {other:?}"),
    }

    local_summary.install_policy = PluginInstallPolicy::Available;
    let mut remote_summary = plugins_test_remote_summary(
        remote_plugin_id,
        "docs",
        Some("Docs"),
        Some("Shared docs plugin."),
        /*installed*/ false,
    );
    remote_summary.install_policy = PluginInstallPolicy::InstalledByDefault;
    let popup = render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![
            plugins_test_curated_marketplace(vec![local_summary]),
            PluginMarketplaceEntry {
                name: remote_marketplace_name.to_string(),
                path: None,
                interface: Some(MarketplaceInterface {
                    display_name: Some("Shared with me".to_string()),
                }),
                plugins: vec![remote_summary],
            },
        ]),
    );
    assert!(
        popup.contains("Installed 0 of 1 available plugins.") && popup.contains("Admin assigned"),
        "expected the unmaterialized admin-assigned remote duplicate to win without counting as installed, got:\n{popup}"
    );

    chat.handle_key_event(KeyEvent::from(KeyCode::Right));
    let installed_popup = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        installed_popup.contains("Showing 0 installed plugins.")
            && installed_popup.contains("No installed plugins")
            && !installed_popup.contains("Docs"),
        "expected the unmaterialized admin-assigned plugin to stay out of the Installed tab, got:\n{installed_popup}"
    );
    chat.handle_key_event(KeyEvent::from(KeyCode::Left));

    while rx.try_recv().is_ok() {}
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::OpenPluginDetailLoading { .. })
    ));
    match rx.try_recv() {
        Ok(AppEvent::FetchPluginDetail { params, .. }) => {
            assert_eq!(params.marketplace_path, None);
            assert_eq!(
                params.remote_marketplace_name,
                Some(remote_marketplace_name.to_string())
            );
            assert_eq!(params.plugin_name, remote_plugin_id);
        }
        other => panic!("expected FetchPluginDetail event, got {other:?}"),
    }
}

#[tokio::test]
async fn plugin_detail_error_popup_skips_disabled_row_numbering() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let response = plugins_test_response(vec![plugins_test_curated_marketplace(vec![
        plugins_test_summary(
            "plugin-figma",
            "figma",
            Some("Figma"),
            Some("Design handoff."),
            /*installed*/ false,
            /*enabled*/ true,
            PluginInstallPolicy::Available,
        ),
    ])]);
    let cwd = chat.config.cwd.clone();
    chat.on_plugins_loaded(cwd.to_path_buf(), Ok(response));
    chat.add_plugins_output();
    chat.on_plugin_detail_loaded(
        cwd.to_path_buf(),
        Err("Failed to load plugin details.".to_string()),
    );

    let popup = render_bottom_popup(&chat, /*width*/ 100);
    assert_chatwidget_snapshot!("plugin_detail_error_popup", popup);
}

#[tokio::test]
async fn plugins_popup_refresh_preserves_selected_row_position() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let initial = plugins_test_response(vec![plugins_test_curated_marketplace(vec![
        plugins_test_summary(
            "plugin-notion",
            "notion",
            Some("Notion"),
            Some("Workspace docs."),
            /*installed*/ false,
            /*enabled*/ true,
            PluginInstallPolicy::Available,
        ),
        plugins_test_summary(
            "plugin-slack",
            "slack",
            Some("Slack"),
            Some("Team chat."),
            /*installed*/ false,
            /*enabled*/ true,
            PluginInstallPolicy::Available,
        ),
    ])]);
    render_loaded_plugins_popup(&mut chat, initial);
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));

    let before = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        before.contains("› [-] Slack"),
        "expected Slack to be selected before refresh, got:\n{before}"
    );

    let refreshed = plugins_test_response(vec![plugins_test_curated_marketplace(vec![
        plugins_test_summary(
            "plugin-airtable",
            "airtable",
            Some("Airtable"),
            Some("Structured records."),
            /*installed*/ false,
            /*enabled*/ true,
            PluginInstallPolicy::Available,
        ),
        plugins_test_summary(
            "plugin-notion",
            "notion",
            Some("Notion"),
            Some("Workspace docs."),
            /*installed*/ false,
            /*enabled*/ true,
            PluginInstallPolicy::Available,
        ),
        plugins_test_summary(
            "plugin-slack",
            "slack",
            Some("Slack"),
            Some("Team chat."),
            /*installed*/ false,
            /*enabled*/ true,
            PluginInstallPolicy::Available,
        ),
    ])]);
    let cwd = chat.config.cwd.clone();
    chat.on_plugins_loaded(cwd.to_path_buf(), Ok(refreshed));

    let after = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        after.contains("› [-] Notion"),
        "expected refresh to preserve the selected row position, got:\n{after}"
    );
    assert!(
        after.contains("Airtable"),
        "expected refreshed popup to include the updated plugin list, got:\n{after}"
    );
    assert!(
        after.contains("Slack"),
        "expected refreshed popup to include the updated plugin list, got:\n{after}"
    );
}

#[tokio::test]
async fn plugins_popup_refreshes_installed_counts_after_install() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let initial = plugins_test_response(vec![plugins_test_curated_marketplace(vec![
        plugins_test_summary(
            "plugin-calendar",
            "calendar",
            Some("Calendar"),
            Some("Schedule management."),
            /*installed*/ false,
            /*enabled*/ true,
            PluginInstallPolicy::Available,
        ),
        plugins_test_summary(
            "plugin-drive",
            "drive",
            Some("Drive"),
            Some("Document access."),
            /*installed*/ true,
            /*enabled*/ true,
            PluginInstallPolicy::Available,
        ),
    ])]);
    let before = render_loaded_plugins_popup(&mut chat, initial);
    assert!(
        before.contains("Installed 1 of 2 available plugins."),
        "expected initial installed count before refresh, got:\n{before}"
    );
    assert!(
        before.contains("Available"),
        "expected pre-install popup copy before refresh, got:\n{before}"
    );

    let refreshed = plugins_test_response(vec![plugins_test_curated_marketplace(vec![
        plugins_test_summary(
            "plugin-calendar",
            "calendar",
            Some("Calendar"),
            Some("Schedule management."),
            /*installed*/ true,
            /*enabled*/ true,
            PluginInstallPolicy::Available,
        ),
        plugins_test_summary(
            "plugin-drive",
            "drive",
            Some("Drive"),
            Some("Document access."),
            /*installed*/ true,
            /*enabled*/ true,
            PluginInstallPolicy::Available,
        ),
    ])]);
    let cwd = chat.config.cwd.clone();
    chat.on_plugins_loaded(cwd.to_path_buf(), Ok(refreshed));

    let after = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        after.contains("Installed 2 of 2 available plugins."),
        "expected /plugins to refresh installed counts after install, got:\n{after}"
    );
    assert!(
        after.contains("Installed   Space to disable; Enter view details."),
        "expected refreshed selected row copy to reflect the installed plugin state, got:\n{after}"
    );
}

#[tokio::test]
async fn plugins_popup_space_toggles_installed_plugin_from_list() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let cwd = chat.config.cwd.to_path_buf();
    render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![plugins_test_curated_marketplace(vec![
            plugins_test_summary(
                "plugin-calendar",
                "calendar",
                Some("Calendar"),
                Some("Schedule management."),
                /*installed*/ true,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
            plugins_test_summary(
                "plugin-drive",
                "drive",
                Some("Drive"),
                Some("Document access."),
                /*installed*/ true,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
        ])]),
    );

    while rx.try_recv().is_ok() {}
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));

    match rx.try_recv() {
        Ok(AppEvent::SetPluginEnabled {
            cwd: event_cwd,
            plugin_id,
            enabled,
        }) => {
            assert_eq!(event_cwd, cwd);
            assert_eq!(plugin_id, "plugin-drive");
            assert!(!enabled);
        }
        other => panic!("expected SetPluginEnabled event, got {other:?}"),
    }

    chat.on_plugin_enabled_set(
        cwd,
        "plugin-drive".to_string(),
        /*enabled*/ false,
        Ok(()),
    );

    let popup = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        popup.contains("› [ ] Drive"),
        "expected selected plugin row to stay selected after refresh, got:\n{popup}"
    );
}

#[tokio::test]
async fn plugins_popup_space_on_uninstalled_row_does_not_start_search() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![plugins_test_curated_marketplace(vec![
            plugins_test_summary(
                "plugin-calendar",
                "calendar",
                Some("Calendar"),
                Some("Schedule management."),
                /*installed*/ false,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
            plugins_test_summary(
                "plugin-drive",
                "drive",
                Some("Drive"),
                Some("Document access."),
                /*installed*/ false,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
        ])]),
    );

    while rx.try_recv().is_ok() {}
    let before = render_bottom_popup(&chat, /*width*/ 100);
    chat.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    let after = render_bottom_popup(&chat, /*width*/ 100);

    assert!(
        rx.try_recv().is_err(),
        "did not expect Space on an uninstalled plugin to emit an event"
    );
    assert_eq!(after, before);
}

#[tokio::test]
async fn plugins_popup_space_with_active_search_does_not_toggle_installed_plugin() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![plugins_test_curated_marketplace(vec![
            plugins_test_summary(
                "plugin-calendar",
                "calendar",
                Some("Calendar"),
                Some("Schedule management."),
                /*installed*/ true,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
            plugins_test_summary(
                "plugin-drive",
                "drive",
                Some("Drive"),
                Some("Document access."),
                /*installed*/ true,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
        ])]),
    );

    while rx.try_recv().is_ok() {}
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    type_plugins_search_query(&mut chat, "dr");
    chat.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));

    assert!(
        rx.try_recv().is_err(),
        "did not expect Space with an active plugin search to emit a toggle event"
    );
}

#[tokio::test]
async fn plugins_popup_search_filters_visible_rows_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![plugins_test_curated_marketplace(vec![
            plugins_test_summary(
                "plugin-calendar",
                "calendar",
                Some("Calendar"),
                Some("Schedule management."),
                /*installed*/ false,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
            plugins_test_summary(
                "plugin-slack",
                "slack",
                Some("Slack"),
                Some("Team chat."),
                /*installed*/ false,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
            plugins_test_summary(
                "plugin-drive",
                "drive",
                Some("Drive"),
                Some("Document access."),
                /*installed*/ false,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
        ])]),
    );

    type_plugins_search_query(&mut chat, "sla");

    let popup = render_bottom_popup(&chat, /*width*/ 100);
    assert_chatwidget_snapshot!("plugins_popup_search_filtered", popup);
    assert!(
        !popup.contains("Calendar") && !popup.contains("Drive"),
        "expected search to leave only matching rows visible, got:\n{popup}"
    );
}

#[tokio::test]
async fn plugins_popup_search_matches_plugin_descriptions() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![plugins_test_curated_marketplace(vec![
            plugins_test_summary(
                "plugin-calendar",
                "calendar",
                Some("Calendar"),
                Some("Schedule management."),
                /*installed*/ false,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
            plugins_test_summary(
                "plugin-drive",
                "drive",
                Some("Drive"),
                Some("Document access."),
                /*installed*/ false,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
        ])]),
    );

    type_plugins_search_query(&mut chat, "document");

    let popup = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        popup.contains("Drive") && !popup.contains("Calendar"),
        "expected plugin search to match descriptions, got:\n{popup}"
    );
}

#[tokio::test]
async fn plugins_popup_installed_tab_filters_rows_and_clears_search() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![plugins_test_curated_marketplace(vec![
            plugins_test_summary(
                "plugin-calendar",
                "calendar",
                Some("Calendar"),
                Some("Schedule management."),
                /*installed*/ true,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
            plugins_test_summary(
                "plugin-slack",
                "slack",
                Some("Slack"),
                Some("Team chat."),
                /*installed*/ false,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
        ])]),
    );

    type_plugins_search_query(&mut chat, "sla");
    chat.handle_key_event(KeyEvent::from(KeyCode::Right));

    let popup = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        popup.contains("Installed plugins.") && popup.contains("Showing 1 installed plugins."),
        "expected Installed tab header, got:\n{popup}"
    );
    assert!(
        popup.contains("Calendar") && !popup.contains("Slack"),
        "expected Installed tab to show only installed plugins, got:\n{popup}"
    );
    assert!(
        !popup.contains("sla"),
        "expected tab switch to clear search query, got:\n{popup}"
    );
}

#[tokio::test]
async fn plugins_popup_openai_curated_tab_omits_marketplace_in_rows() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![
            plugins_test_curated_marketplace(vec![plugins_test_summary(
                "plugin-calendar",
                "calendar",
                Some("Calendar"),
                Some("Schedule management."),
                /*installed*/ false,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            )]),
            plugins_test_repo_marketplace(vec![plugins_test_summary(
                "plugin-repo",
                "repo",
                Some("Repo Plugin"),
                Some("Repo-only plugin."),
                /*installed*/ false,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            )]),
        ]),
    );

    chat.handle_key_event(KeyEvent::from(KeyCode::Right));
    chat.handle_key_event(KeyEvent::from(KeyCode::Right));

    let popup = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        popup.contains("OpenAI Curated marketplace."),
        "expected OpenAI Curated tab header, got:\n{popup}"
    );
    assert!(
        popup.contains("Calendar") && !popup.contains("Repo Plugin"),
        "expected OpenAI Curated tab to show only official marketplace plugins, got:\n{popup}"
    );
    assert!(
        !popup.contains("ChatGPT Marketplace ·"),
        "expected marketplace-specific rows to omit marketplace labels, got:\n{popup}"
    );
}

#[tokio::test]
async fn plugins_popup_refresh_preserves_duplicate_marketplace_tab_by_path() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    let response = plugins_test_response(vec![
        PluginMarketplaceEntry {
            name: "duplicate".to_string(),
            path: Some(plugins_test_absolute_path(
                "marketplaces/home/marketplace.json",
            )),
            interface: Some(MarketplaceInterface {
                display_name: Some("Duplicate Marketplace".to_string()),
            }),
            plugins: vec![plugins_test_summary(
                "plugin-home",
                "home",
                Some("Home Plugin"),
                Some("Home marketplace plugin."),
                /*installed*/ false,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            )],
        },
        PluginMarketplaceEntry {
            name: "duplicate".to_string(),
            path: Some(plugins_test_absolute_path(
                "marketplaces/repo/marketplace.json",
            )),
            interface: Some(MarketplaceInterface {
                display_name: Some("Duplicate Marketplace".to_string()),
            }),
            plugins: vec![plugins_test_summary(
                "plugin-repo",
                "repo",
                Some("Repo Plugin"),
                Some("Repo marketplace plugin."),
                /*installed*/ false,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            )],
        },
    ]);
    let cwd = chat.config.cwd.to_path_buf();
    chat.on_plugins_loaded(cwd.clone(), Ok(response.clone()));
    chat.add_plugins_output();

    for _ in 0..10 {
        let popup = render_bottom_popup(&chat, /*width*/ 100);
        if popup.contains("Duplicate Marketplace (2/2).")
            && popup.contains("Repo Plugin")
            && !popup.contains("Home Plugin")
        {
            break;
        }
        chat.handle_key_event(KeyEvent::from(KeyCode::Right));
    }

    chat.on_plugins_loaded(cwd, Ok(response));

    let popup = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        popup.contains("Duplicate Marketplace (2/2)."),
        "expected refresh to preserve the second duplicate marketplace tab, got:\n{popup}"
    );
    assert!(
        popup.contains("Repo Plugin") && !popup.contains("Home Plugin"),
        "expected second duplicate marketplace rows after refresh, got:\n{popup}"
    );
}

#[tokio::test]
async fn plugins_popup_search_no_matches_and_backspace_restores_results() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::Plugins, /*enabled*/ true);

    render_loaded_plugins_popup(
        &mut chat,
        plugins_test_response(vec![plugins_test_curated_marketplace(vec![
            plugins_test_summary(
                "plugin-calendar",
                "calendar",
                Some("Calendar"),
                Some("Schedule management."),
                /*installed*/ false,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
            plugins_test_summary(
                "plugin-slack",
                "slack",
                Some("Slack"),
                Some("Team chat."),
                /*installed*/ false,
                /*enabled*/ true,
                PluginInstallPolicy::Available,
            ),
        ])]),
    );

    type_plugins_search_query(&mut chat, "zzz");

    let no_matches = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        no_matches.contains("zzz"),
        "expected popup to show the typed search query, got:\n{no_matches}"
    );
    assert!(
        no_matches.contains("no matches"),
        "expected popup to render the no-matches UX, got:\n{no_matches}"
    );

    for _ in 0..3 {
        chat.handle_key_event(KeyEvent::from(KeyCode::Backspace));
    }

    let restored = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        restored.contains("Calendar") && restored.contains("Slack"),
        "expected clearing the query to restore the plugin rows, got:\n{restored}"
    );
    assert!(
        !restored.contains("no matches"),
        "did not expect the no-matches state after clearing the query, got:\n{restored}"
    );
}

#[tokio::test]
async fn apps_popup_stays_loading_until_final_snapshot_updates() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.config
        .features
        .enable(Feature::Apps)
        .expect("test config should allow feature update");
    chat.bottom_pane.set_connectors_enabled(/*enabled*/ true);
    let notion_id = "unit_test_apps_popup_refresh_connector_1";
    let linear_id = "unit_test_apps_popup_refresh_connector_2";

    chat.on_connectors_loaded(
        Ok(ConnectorsSnapshot {
            connectors: vec![AppInfo {
                id: notion_id.to_string(),
                name: "Notion".to_string(),
                description: Some("Workspace docs".to_string()),
                logo_url: None,
                logo_url_dark: None,
                icon_assets: None,
                icon_dark_assets: None,
                distribution_channel: None,
                branding: None,
                app_metadata: None,
                labels: None,
                install_url: Some("https://example.test/notion".to_string()),
                is_accessible: true,
                is_enabled: true,
                plugin_display_names: Vec::new(),
            }],
        }),
        /*is_final*/ false,
    );
    chat.add_connectors_output();
    assert!(
        chat.connectors.prefetch_in_flight,
        "expected /apps to trigger a forced connectors refresh"
    );

    let before = render_bottom_popup(&chat, /*width*/ 80);
    assert!(
        before.contains("Loading installed and available apps..."),
        "expected /apps to stay in the loading state until the full list arrives, got:\n{before}"
    );
    assert_chatwidget_snapshot!("apps_popup_loading_state", before);

    chat.on_connectors_loaded(
        Ok(ConnectorsSnapshot {
            connectors: vec![
                AppInfo {
                    id: notion_id.to_string(),
                    name: "Notion".to_string(),
                    description: Some("Workspace docs".to_string()),
                    logo_url: None,
                    logo_url_dark: None,
                    icon_assets: None,
                    icon_dark_assets: None,
                    distribution_channel: None,
                    branding: None,
                    app_metadata: None,
                    labels: None,
                    install_url: Some("https://example.test/notion".to_string()),
                    is_accessible: true,
                    is_enabled: true,
                    plugin_display_names: Vec::new(),
                },
                AppInfo {
                    id: linear_id.to_string(),
                    name: "Linear".to_string(),
                    description: Some("Project tracking".to_string()),
                    logo_url: None,
                    logo_url_dark: None,
                    icon_assets: None,
                    icon_dark_assets: None,
                    distribution_channel: None,
                    branding: None,
                    app_metadata: None,
                    labels: None,
                    install_url: Some("https://example.test/linear".to_string()),
                    is_accessible: true,
                    is_enabled: true,
                    plugin_display_names: Vec::new(),
                },
            ],
        }),
        /*is_final*/ true,
    );

    let after = render_bottom_popup(&chat, /*width*/ 80);
    assert!(
        after.contains("Installed 2 of 2 available apps."),
        "expected refreshed apps popup snapshot, got:\n{after}"
    );
    assert!(
        after.contains("Linear"),
        "expected refreshed popup to include new connector, got:\n{after}"
    );
}

#[tokio::test]
async fn apps_notification_update_excludes_inaccessible_apps_from_mentions() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.config
        .features
        .enable(Feature::Apps)
        .expect("test config should allow feature update");
    chat.bottom_pane.set_connectors_enabled(/*enabled*/ true);
    chat.bottom_pane
        .set_composer_text("$".to_string(), Vec::new(), Vec::new());

    chat.on_connectors_loaded(
        Ok(ConnectorsSnapshot {
            connectors: vec![
                AppInfo {
                    id: "google_drive".to_string(),
                    name: "Google Drive".to_string(),
                    description: Some("Connected files".to_string()),
                    logo_url: None,
                    logo_url_dark: None,
                    icon_assets: None,
                    icon_dark_assets: None,
                    distribution_channel: None,
                    branding: None,
                    app_metadata: None,
                    labels: None,
                    install_url: Some("https://example.test/google-drive".to_string()),
                    is_accessible: true,
                    is_enabled: true,
                    plugin_display_names: Vec::new(),
                },
                AppInfo {
                    id: "arabica_uae".to_string(),
                    name: "% Arabica UAE".to_string(),
                    description: Some("Directory-only app".to_string()),
                    logo_url: None,
                    logo_url_dark: None,
                    icon_assets: None,
                    icon_dark_assets: None,
                    distribution_channel: None,
                    branding: None,
                    app_metadata: None,
                    labels: None,
                    install_url: Some("https://example.test/arabica".to_string()),
                    is_accessible: false,
                    is_enabled: true,
                    plugin_display_names: Vec::new(),
                },
            ],
        }),
        /*is_final*/ false,
    );

    assert_matches!(
        &chat.connectors.partial_snapshot,
        Some(snapshot)
            if snapshot
                .connectors
                .iter()
                .find(|connector| connector.id == "arabica_uae")
                .is_some_and(|connector| !connector.is_accessible)
    );

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert!(
        popup.contains("Google Drive"),
        "expected accessible apps to appear in the mention popup, got:\n{popup}"
    );
    assert!(
        !popup.contains("% Arabica UAE"),
        "did not expect an inaccessible directory app in the mention popup, got:\n{popup}"
    );
}

#[tokio::test]
async fn apps_refresh_failure_keeps_existing_full_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.config
        .features
        .enable(Feature::Apps)
        .expect("test config should allow feature update");
    chat.bottom_pane.set_connectors_enabled(/*enabled*/ true);
    let notion_id = "unit_test_apps_refresh_failure_connector_1";
    let linear_id = "unit_test_apps_refresh_failure_connector_2";

    let full_connectors = vec![
        AppInfo {
            id: notion_id.to_string(),
            name: "Notion".to_string(),
            description: Some("Workspace docs".to_string()),
            logo_url: None,
            logo_url_dark: None,
            icon_assets: None,
            icon_dark_assets: None,
            distribution_channel: None,
            branding: None,
            app_metadata: None,
            labels: None,
            install_url: Some("https://example.test/notion".to_string()),
            is_accessible: true,
            is_enabled: true,
            plugin_display_names: Vec::new(),
        },
        AppInfo {
            id: linear_id.to_string(),
            name: "Linear".to_string(),
            description: Some("Project tracking".to_string()),
            logo_url: None,
            logo_url_dark: None,
            icon_assets: None,
            icon_dark_assets: None,
            distribution_channel: None,
            branding: None,
            app_metadata: None,
            labels: None,
            install_url: Some("https://example.test/linear".to_string()),
            is_accessible: false,
            is_enabled: true,
            plugin_display_names: Vec::new(),
        },
    ];
    chat.on_connectors_loaded(
        Ok(ConnectorsSnapshot {
            connectors: full_connectors.clone(),
        }),
        /*is_final*/ true,
    );

    chat.on_connectors_loaded(
        Ok(ConnectorsSnapshot {
            connectors: vec![AppInfo {
                id: notion_id.to_string(),
                name: "Notion".to_string(),
                description: Some("Workspace docs".to_string()),
                logo_url: None,
                logo_url_dark: None,
                icon_assets: None,
                icon_dark_assets: None,
                distribution_channel: None,
                branding: None,
                app_metadata: None,
                labels: None,
                install_url: Some("https://example.test/notion".to_string()),
                is_accessible: true,
                is_enabled: true,
                plugin_display_names: Vec::new(),
            }],
        }),
        /*is_final*/ false,
    );
    chat.on_connectors_loaded(
        Err("failed to load apps".to_string()),
        /*is_final*/ true,
    );

    assert_matches!(
        &chat.connectors.cache,
        ConnectorsCacheState::Ready(snapshot) if snapshot.connectors == full_connectors
    );

    chat.add_connectors_output();
    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert!(
        popup.contains("Installed 1 of 2 available apps."),
        "expected previous full snapshot to be preserved, got:\n{popup}"
    );
}

#[tokio::test]
async fn apps_popup_preserves_selected_app_across_refresh() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.config
        .features
        .enable(Feature::Apps)
        .expect("test config should allow feature update");
    chat.bottom_pane.set_connectors_enabled(/*enabled*/ true);

    chat.on_connectors_loaded(
        Ok(ConnectorsSnapshot {
            connectors: vec![
                AppInfo {
                    id: "notion".to_string(),
                    name: "Notion".to_string(),
                    description: Some("Workspace docs".to_string()),
                    logo_url: None,
                    logo_url_dark: None,
                    icon_assets: None,
                    icon_dark_assets: None,
                    distribution_channel: None,
                    branding: None,
                    app_metadata: None,
                    labels: None,
                    install_url: Some("https://example.test/notion".to_string()),
                    is_accessible: true,
                    is_enabled: true,
                    plugin_display_names: Vec::new(),
                },
                AppInfo {
                    id: "slack".to_string(),
                    name: "Slack".to_string(),
                    description: Some("Team chat".to_string()),
                    logo_url: None,
                    logo_url_dark: None,
                    icon_assets: None,
                    icon_dark_assets: None,
                    distribution_channel: None,
                    branding: None,
                    app_metadata: None,
                    labels: None,
                    install_url: Some("https://example.test/slack".to_string()),
                    is_accessible: true,
                    is_enabled: true,
                    plugin_display_names: Vec::new(),
                },
            ],
        }),
        /*is_final*/ true,
    );
    chat.add_connectors_output();
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));

    let before = render_bottom_popup(&chat, /*width*/ 80);
    assert!(
        before.contains("› Slack"),
        "expected Slack to be selected before refresh, got:\n{before}"
    );

    chat.on_connectors_loaded(
        Ok(ConnectorsSnapshot {
            connectors: vec![
                AppInfo {
                    id: "airtable".to_string(),
                    name: "Airtable".to_string(),
                    description: Some("Spreadsheets".to_string()),
                    logo_url: None,
                    logo_url_dark: None,
                    icon_assets: None,
                    icon_dark_assets: None,
                    distribution_channel: None,
                    branding: None,
                    app_metadata: None,
                    labels: None,
                    install_url: Some("https://example.test/airtable".to_string()),
                    is_accessible: true,
                    is_enabled: true,
                    plugin_display_names: Vec::new(),
                },
                AppInfo {
                    id: "notion".to_string(),
                    name: "Notion".to_string(),
                    description: Some("Workspace docs".to_string()),
                    logo_url: None,
                    logo_url_dark: None,
                    icon_assets: None,
                    icon_dark_assets: None,
                    distribution_channel: None,
                    branding: None,
                    app_metadata: None,
                    labels: None,
                    install_url: Some("https://example.test/notion".to_string()),
                    is_accessible: true,
                    is_enabled: true,
                    plugin_display_names: Vec::new(),
                },
                AppInfo {
                    id: "slack".to_string(),
                    name: "Slack".to_string(),
                    description: Some("Team chat".to_string()),
                    logo_url: None,
                    logo_url_dark: None,
                    icon_assets: None,
                    icon_dark_assets: None,
                    distribution_channel: None,
                    branding: None,
                    app_metadata: None,
                    labels: None,
                    install_url: Some("https://example.test/slack".to_string()),
                    is_accessible: true,
                    is_enabled: true,
                    plugin_display_names: Vec::new(),
                },
            ],
        }),
        /*is_final*/ true,
    );

    let after = render_bottom_popup(&chat, /*width*/ 80);
    assert!(
        after.contains("› Slack"),
        "expected Slack to stay selected after refresh, got:\n{after}"
    );
    assert!(
        !after.contains("› Notion"),
        "did not expect selection to reset to Notion after refresh, got:\n{after}"
    );
}

#[tokio::test]
async fn apps_refresh_failure_with_cached_snapshot_triggers_pending_force_refetch() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.config
        .features
        .enable(Feature::Apps)
        .expect("test config should allow feature update");
    chat.bottom_pane.set_connectors_enabled(/*enabled*/ true);
    chat.connectors.prefetch_in_flight = true;
    chat.connectors.force_refetch_pending = true;

    let full_connectors = vec![AppInfo {
        id: "unit_test_apps_refresh_failure_pending_connector".to_string(),
        name: "Notion".to_string(),
        description: Some("Workspace docs".to_string()),
        logo_url: None,
        logo_url_dark: None,
        icon_assets: None,
        icon_dark_assets: None,
        distribution_channel: None,
        branding: None,
        app_metadata: None,
        labels: None,
        install_url: Some("https://example.test/notion".to_string()),
        is_accessible: true,
        is_enabled: true,
        plugin_display_names: Vec::new(),
    }];
    chat.connectors.cache = ConnectorsCacheState::Ready(ConnectorsSnapshot {
        connectors: full_connectors.clone(),
    });

    chat.on_connectors_loaded(
        Err("failed to load apps".to_string()),
        /*is_final*/ true,
    );

    assert!(chat.connectors.prefetch_in_flight);
    assert!(!chat.connectors.force_refetch_pending);
    assert_matches!(
        &chat.connectors.cache,
        ConnectorsCacheState::Ready(snapshot) if snapshot.connectors == full_connectors
    );
}

#[tokio::test]
async fn apps_popup_keeps_existing_full_snapshot_while_partial_refresh_loads() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.config
        .features
        .enable(Feature::Apps)
        .expect("test config should allow feature update");
    chat.bottom_pane.set_connectors_enabled(/*enabled*/ true);

    let full_connectors = vec![
        AppInfo {
            id: "unit_test_connector_1".to_string(),
            name: "Notion".to_string(),
            description: Some("Workspace docs".to_string()),
            logo_url: None,
            logo_url_dark: None,
            icon_assets: None,
            icon_dark_assets: None,
            distribution_channel: None,
            branding: None,
            app_metadata: None,
            labels: None,
            install_url: Some("https://example.test/notion".to_string()),
            is_accessible: true,
            is_enabled: true,
            plugin_display_names: Vec::new(),
        },
        AppInfo {
            id: "unit_test_connector_2".to_string(),
            name: "Linear".to_string(),
            description: Some("Project tracking".to_string()),
            logo_url: None,
            logo_url_dark: None,
            icon_assets: None,
            icon_dark_assets: None,
            distribution_channel: None,
            branding: None,
            app_metadata: None,
            labels: None,
            install_url: Some("https://example.test/linear".to_string()),
            is_accessible: false,
            is_enabled: true,
            plugin_display_names: Vec::new(),
        },
    ];
    chat.on_connectors_loaded(
        Ok(ConnectorsSnapshot {
            connectors: full_connectors.clone(),
        }),
        /*is_final*/ true,
    );
    chat.add_connectors_output();

    chat.on_connectors_loaded(
        Ok(ConnectorsSnapshot {
            connectors: vec![
                AppInfo {
                    id: "unit_test_connector_1".to_string(),
                    name: "Notion".to_string(),
                    description: Some("Workspace docs".to_string()),
                    logo_url: None,
                    logo_url_dark: None,
                    icon_assets: None,
                    icon_dark_assets: None,
                    distribution_channel: None,
                    branding: None,
                    app_metadata: None,
                    labels: None,
                    install_url: Some("https://example.test/notion".to_string()),
                    is_accessible: true,
                    is_enabled: true,
                    plugin_display_names: Vec::new(),
                },
                AppInfo {
                    id: "connector_openai_hidden".to_string(),
                    name: "Hidden OpenAI".to_string(),
                    description: Some("Should be filtered".to_string()),
                    logo_url: None,
                    logo_url_dark: None,
                    icon_assets: None,
                    icon_dark_assets: None,
                    distribution_channel: None,
                    branding: None,
                    app_metadata: None,
                    labels: None,
                    install_url: Some("https://example.test/hidden-openai".to_string()),
                    is_accessible: true,
                    is_enabled: true,
                    plugin_display_names: Vec::new(),
                },
            ],
        }),
        /*is_final*/ false,
    );

    assert_matches!(
        &chat.connectors.cache,
        ConnectorsCacheState::Ready(snapshot) if snapshot.connectors == full_connectors
    );

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert!(
        popup.contains("Installed 1 of 2 available apps."),
        "expected popup to keep the last full snapshot while partial refresh loads, got:\n{popup}"
    );
    assert!(
        !popup.contains("Hidden OpenAI"),
        "expected popup to ignore partial refresh rows until the full list arrives, got:\n{popup}"
    );
}

#[tokio::test]
async fn apps_refresh_failure_without_full_snapshot_falls_back_to_installed_apps() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.config
        .features
        .enable(Feature::Apps)
        .expect("test config should allow feature update");
    chat.bottom_pane.set_connectors_enabled(/*enabled*/ true);

    chat.on_connectors_loaded(
        Ok(ConnectorsSnapshot {
            connectors: vec![AppInfo {
                id: "unit_test_apps_refresh_failure_fallback_connector".to_string(),
                name: "Notion".to_string(),
                description: Some("Workspace docs".to_string()),
                logo_url: None,
                logo_url_dark: None,
                icon_assets: None,
                icon_dark_assets: None,
                distribution_channel: None,
                branding: None,
                app_metadata: None,
                labels: None,
                install_url: Some("https://example.test/notion".to_string()),
                is_accessible: true,
                is_enabled: true,
                plugin_display_names: Vec::new(),
            }],
        }),
        /*is_final*/ false,
    );

    chat.add_connectors_output();
    let loading_popup = render_bottom_popup(&chat, /*width*/ 80);
    assert!(
        loading_popup.contains("Loading installed and available apps..."),
        "expected /apps to keep showing loading before the final result, got:\n{loading_popup}"
    );

    chat.on_connectors_loaded(
        Err("failed to load apps".to_string()),
        /*is_final*/ true,
    );

    assert_matches!(
        &chat.connectors.cache,
        ConnectorsCacheState::Ready(snapshot) if snapshot.connectors.len() == 1
    );

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert!(
        popup.contains("Installed 1 of 1 available apps."),
        "expected /apps to fall back to the installed apps snapshot, got:\n{popup}"
    );
    assert!(
        popup.contains("Installed. Press Enter to open the app page"),
        "expected the fallback popup to behave like the installed apps view, got:\n{popup}"
    );
}

#[tokio::test]
async fn apps_popup_shows_disabled_status_for_installed_but_disabled_apps() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.config
        .features
        .enable(Feature::Apps)
        .expect("test config should allow feature update");
    chat.bottom_pane.set_connectors_enabled(/*enabled*/ true);

    chat.on_connectors_loaded(
        Ok(ConnectorsSnapshot {
            connectors: vec![AppInfo {
                id: "connector_1".to_string(),
                name: "Notion".to_string(),
                description: Some("Workspace docs".to_string()),
                logo_url: None,
                logo_url_dark: None,
                icon_assets: None,
                icon_dark_assets: None,
                distribution_channel: None,
                branding: None,
                app_metadata: None,
                labels: None,
                install_url: Some("https://example.test/notion".to_string()),
                is_accessible: true,
                is_enabled: false,
                plugin_display_names: Vec::new(),
            }],
        }),
        /*is_final*/ true,
    );

    chat.add_connectors_output();
    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert!(
        popup.contains("Installed · Disabled. Press Enter to open the app page"),
        "expected selected app description to include disabled status, got:\n{popup}"
    );
    assert!(
        popup.contains("enable/disable this app."),
        "expected selected app description to mention enable/disable action, got:\n{popup}"
    );
}

#[tokio::test]
async fn apps_refresh_preserves_toggled_enabled_state() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.config
        .features
        .enable(Feature::Apps)
        .expect("test config should allow feature update");
    chat.bottom_pane.set_connectors_enabled(/*enabled*/ true);

    chat.on_connectors_loaded(
        Ok(ConnectorsSnapshot {
            connectors: vec![AppInfo {
                id: "connector_1".to_string(),
                name: "Notion".to_string(),
                description: Some("Workspace docs".to_string()),
                logo_url: None,
                logo_url_dark: None,
                icon_assets: None,
                icon_dark_assets: None,
                distribution_channel: None,
                branding: None,
                app_metadata: None,
                labels: None,
                install_url: Some("https://example.test/notion".to_string()),
                is_accessible: true,
                is_enabled: true,
                plugin_display_names: Vec::new(),
            }],
        }),
        /*is_final*/ true,
    );
    chat.update_connector_enabled("connector_1", /*enabled*/ false);

    chat.on_connectors_loaded(
        Ok(ConnectorsSnapshot {
            connectors: vec![AppInfo {
                id: "connector_1".to_string(),
                name: "Notion".to_string(),
                description: Some("Workspace docs".to_string()),
                logo_url: None,
                logo_url_dark: None,
                icon_assets: None,
                icon_dark_assets: None,
                distribution_channel: None,
                branding: None,
                app_metadata: None,
                labels: None,
                install_url: Some("https://example.test/notion".to_string()),
                is_accessible: true,
                is_enabled: true,
                plugin_display_names: Vec::new(),
            }],
        }),
        /*is_final*/ true,
    );

    assert_matches!(
        &chat.connectors.cache,
        ConnectorsCacheState::Ready(snapshot)
            if snapshot
                .connectors
                .iter()
                .find(|connector| connector.id == "connector_1")
                .is_some_and(|connector| !connector.is_enabled)
    );

    chat.add_connectors_output();
    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert!(
        popup.contains("Installed · Disabled. Press Enter to open the app page"),
        "expected disabled status to persist after reload, got:\n{popup}"
    );
}

#[tokio::test]
async fn apps_popup_for_not_installed_app_uses_install_only_selected_description() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.config
        .features
        .enable(Feature::Apps)
        .expect("test config should allow feature update");
    chat.bottom_pane.set_connectors_enabled(/*enabled*/ true);

    chat.on_connectors_loaded(
        Ok(ConnectorsSnapshot {
            connectors: vec![AppInfo {
                id: "connector_2".to_string(),
                name: "Linear".to_string(),
                description: Some("Project tracking".to_string()),
                logo_url: None,
                logo_url_dark: None,
                icon_assets: None,
                icon_dark_assets: None,
                distribution_channel: None,
                branding: None,
                app_metadata: None,
                labels: None,
                install_url: Some("https://example.test/linear".to_string()),
                is_accessible: false,
                is_enabled: true,
                plugin_display_names: Vec::new(),
            }],
        }),
        /*is_final*/ true,
    );

    chat.add_connectors_output();
    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert!(
        popup.contains("Can be installed. Press Enter to open the app page to install"),
        "expected selected app description to be install-only for not-installed apps, got:\n{popup}"
    );
    assert!(
        !popup.contains("enable/disable this app."),
        "did not expect enable/disable text for not-installed apps, got:\n{popup}"
    );
}

#[tokio::test]
async fn experimental_features_popup_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    let features = vec![
        ExperimentalFeatureItem {
            feature: Feature::JsRepl,
            name: "JavaScript REPL".to_string(),
            description: "Enable a persistent Node-backed JavaScript REPL for interactive website debugging and other inline JavaScript execution capabilities.".to_string(),
            enabled: false,
        },
        ExperimentalFeatureItem {
            feature: Feature::ShellTool,
            name: "Shell tool".to_string(),
            description: "Allow the model to run shell commands.".to_string(),
            enabled: true,
        },
    ];
    let view = ExperimentalFeaturesView::new(
        features,
        chat.app_event_tx.clone(),
        crate::keymap::RuntimeKeymap::defaults().list,
    );
    chat.bottom_pane.show_view(Box::new(view));

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("experimental_features_popup", popup);
}

#[tokio::test]
async fn experimental_features_toggle_saves_on_exit() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    let expected_feature = Feature::JsRepl;
    let view = ExperimentalFeaturesView::new(
        vec![ExperimentalFeatureItem {
            feature: expected_feature,
            name: "JavaScript REPL".to_string(),
            description: "Enable a persistent Node-backed JavaScript REPL for interactive website debugging and other inline JavaScript execution capabilities.".to_string(),
            enabled: false,
        }],
        chat.app_event_tx.clone(),
        crate::keymap::RuntimeKeymap::defaults().list,
    );
    chat.bottom_pane.show_view(Box::new(view));

    chat.handle_key_event(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));

    assert!(
        rx.try_recv().is_err(),
        "expected no updates until saving the popup"
    );

    chat.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    let mut updates = None;
    while let Ok(event) = rx.try_recv() {
        if let AppEvent::UpdateFeatureFlags {
            updates: event_updates,
        } = event
        {
            updates = Some(event_updates);
            break;
        }
    }

    let updates = updates.expect("expected UpdateFeatureFlags event");
    assert_eq!(updates, vec![(expected_feature, true)]);
}

#[tokio::test]
async fn experimental_popup_omits_stable_guardian_approval() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let guardian_stage = FEATURES
        .iter()
        .find(|spec| spec.id == Feature::GuardianApproval)
        .map(|spec| spec.stage)
        .expect("expected guardian approval feature metadata");

    assert_eq!(guardian_stage, Stage::Stable);

    chat.open_experimental_popup();

    let popup = render_bottom_popup(&chat, /*width*/ 120);
    assert!(
        !popup.contains("Auto-review"),
        "expected stable auto-review feature to be omitted from experimental popup, got:\n{popup}"
    );
}

#[tokio::test]
async fn multi_agent_enable_prompt_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    chat.open_multi_agent_enable_prompt();

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("multi_agent_enable_prompt", popup);
}

#[tokio::test]
async fn multi_agent_enable_prompt_updates_feature_and_emits_notice() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    chat.open_multi_agent_enable_prompt();
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::UpdateFeatureFlags { updates }) if updates == vec![(Feature::Collab, true)]
    );
    let cell = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected InsertHistoryCell event, got {other:?}"),
    };
    let rendered = lines_to_single_string(&cell.display_lines(/*width*/ 120));
    assert!(rendered.contains("Subagents will be enabled in the next session."));
}

#[tokio::test]
async fn memories_enable_prompt_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::MemoryTool, /*enabled*/ false);

    chat.open_memories_popup();

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("memories_enable_prompt", popup);
}

#[tokio::test]
async fn memories_enable_prompt_updates_feature_without_notice() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::MemoryTool, /*enabled*/ false);

    chat.open_memories_popup();
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::UpdateFeatureFlags { updates }) if updates == vec![(Feature::MemoryTool, true)]
    );
    assert!(
        rx.try_recv().is_err(),
        "memory enable prompt should not emit the success notice before persistence succeeds"
    );
}

#[tokio::test]
async fn memories_settings_popup_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::MemoryTool, /*enabled*/ true);
    chat.config.memories.use_memories = true;
    chat.config.memories.generate_memories = false;

    chat.open_memories_popup();

    let popup = strip_osc8_for_snapshot(&render_bottom_popup(&chat, /*width*/ 80));
    assert_chatwidget_snapshot!("memories_settings_popup", popup);
}

#[tokio::test]
async fn memories_reset_confirmation_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::MemoryTool, /*enabled*/ true);
    chat.config.memories.use_memories = true;
    chat.config.memories.generate_memories = false;

    chat.open_memories_popup();
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("memories_reset_confirmation", popup);
}

#[tokio::test]
async fn memories_settings_toggle_saves_on_enter() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::MemoryTool, /*enabled*/ true);
    chat.config.memories.use_memories = true;
    chat.config.memories.generate_memories = false;

    chat.open_memories_popup();
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Char(' ')));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::UpdateMemorySettings {
            use_memories: true,
            generate_memories: true,
        })
    );
}

#[tokio::test]
async fn memories_reset_confirmation_sends_event_on_confirm() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_feature_enabled(Feature::MemoryTool, /*enabled*/ true);
    chat.config.memories.use_memories = true;
    chat.config.memories.generate_memories = false;

    chat.open_memories_popup();
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_matches!(rx.try_recv(), Ok(AppEvent::ResetMemories));
}

#[tokio::test]
async fn model_selection_popup_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some(AMBIENT_DEFAULT_MODEL)).await;
    chat.thread_id = Some(ThreadId::new());
    chat.open_model_popup();

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("model_selection_popup", popup);
}

#[tokio::test]
async fn model_selection_popup_openai_provider_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some(AMBIENT_DEFAULT_MODEL)).await;
    chat.thread_id = Some(ThreadId::new());
    let mut presets = chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load");
    for preset in &mut presets {
        preset.is_default = preset.model == "gpt-5.6-sol";
    }
    chat.open_all_models_popup(presets);
    chat.handle_key_event(KeyEvent::from(KeyCode::Left));

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("model_selection_popup_openai_provider", popup);
}

#[tokio::test]
async fn model_selection_popup_openrouter_provider_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("x-ai/grok-4.7")).await;
    chat.config.model_provider_id = "openrouter".to_string();
    chat.set_model("x-ai/grok-4.7");
    chat.thread_id = Some(ThreadId::new());
    let presets = chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load");
    chat.open_all_models_popup(presets);

    let popup = render_bottom_popup_with_height(&chat, /*width*/ 130, /*height*/ 44);
    for model in [
        "x-ai/grok-4.7",
        "deepseek/deepseek-v4.1-flash",
        "z-ai/glm-5.3-flash",
        "nvidia/nemotron-3-ultra-550b-a55b:free",
    ] {
        assert!(popup.contains(model), "missing {model}: {popup}");
    }
    for retired in [
        "grok-4.5",
        "grok-4.6",
        "deepseek-v4-pro",
        "deepseek-v4-flash",
        "owl-alpha",
    ] {
        assert!(!popup.contains(retired), "retired {retired}: {popup}");
    }
    assert_chatwidget_snapshot!("model_selection_popup_openrouter_provider", popup);
}

#[tokio::test]
async fn model_selection_popup_zai_flash_provider_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("glm-5.3-flash")).await;
    chat.thread_id = Some(ThreadId::new());
    let presets = chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load");
    chat.open_all_models_popup(presets);

    let popup = render_bottom_popup_with_height(&chat, /*width*/ 100, /*height*/ 32);
    assert_chatwidget_snapshot!("model_selection_popup_zai_flash_provider", popup);
}

#[tokio::test]
async fn spawn_model_selection_popup_deepseek_provider_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some(AMBIENT_DEFAULT_MODEL)).await;
    chat.thread_id = Some(ThreadId::new());
    let presets = chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load");
    chat.open_all_models_popup_for_purpose(
        presets,
        spawn_model_purpose(
            SpawnRole::Orc,
            /*parent_node_id*/ None,
            DEEPSEEK_DEFAULT_MODEL,
        ),
    );

    let popup = render_bottom_popup_with_height(&chat, /*width*/ 110, /*height*/ 24);
    assert_chatwidget_snapshot!("spawn_model_selection_popup_deepseek_provider", popup);
    assert!(popup.contains("Corbanu Terminal Orc pane - DeepSeek API key"));
    assert!(popup.contains("[DeepSeek]"));
    assert!(popup.contains("DeepSeek V4.1 Flash (Direct) (current)"));

    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let (preset, purpose) = loop {
        match rx.try_recv().expect("DeepSeek reasoning event") {
            AppEvent::OpenReasoningPopup { model, purpose } => break (model, purpose),
            AppEvent::SettingsSelectionClosed => continue,
            event => panic!("unexpected event: {event:?}"),
        }
    };
    chat.open_reasoning_popup_for_purpose(preset, purpose);
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::CreateSpawnAgent {
            model,
            provider: Some(provider),
            ..
        }) if model == DEEPSEEK_DEFAULT_MODEL && provider == DEEPSEEK_PROVIDER_ID
    );
}

#[tokio::test]
async fn model_selection_popup_kimi_code_provider_snapshot() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some(KIMI_CODE_K3_MODEL)).await;
    chat.thread_id = Some(ThreadId::new());
    chat.config.model_provider_id = KIMI_CODE_PROVIDER_ID.to_string();
    let presets = chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load");
    chat.open_all_models_popup(presets);

    let popup = render_bottom_popup_with_height(&chat, /*width*/ 100, /*height*/ 24);
    assert_chatwidget_snapshot!("model_selection_popup_kimi_code_provider", popup);
    assert!(popup.contains("[Kimi Code]"));
    assert!(popup.contains("Kimi Code K3 (current)"));

    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let (model, purpose) = loop {
        match rx.try_recv().expect("Kimi reasoning event") {
            AppEvent::OpenReasoningPopup { model, purpose } => break (model, purpose),
            AppEvent::SettingsSelectionClosed => continue,
            event => panic!("unexpected event: {event:?}"),
        }
    };
    chat.open_reasoning_popup_for_purpose(model, purpose);
    let reasoning_popup = render_bottom_popup(&chat, /*width*/ 100);
    assert_chatwidget_snapshot!("kimi_code_reasoning_popup", reasoning_popup);
    for label in ["Low", "High", "More reasoning…"] {
        assert!(
            reasoning_popup.contains(label),
            "expected Kimi reasoning option {label:?} in picker:\n{reasoning_popup}"
        );
    }
}

fn spawn_model_purpose(
    role: SpawnRole,
    parent_node_id: Option<&str>,
    default_model: &str,
) -> ModelSelectionPurpose {
    ModelSelectionPurpose::SpawnAgent {
        role,
        parent_node_id: parent_node_id.map(str::to_string),
        default_model: default_model.to_string(),
    }
}

#[tokio::test]
async fn spawn_model_selection_popup_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some(AMBIENT_DEFAULT_MODEL)).await;
    let presets = chat.model_catalog.try_list_models().expect("model catalog");
    chat.open_all_models_popup_for_purpose(
        presets,
        spawn_model_purpose(
            SpawnRole::Nazgul,
            /*parent_node_id*/ None,
            "gpt-5.6-sol",
        ),
    );

    let popup = render_bottom_popup_with_height(&chat, /*width*/ 100, /*height*/ 30);
    assert_chatwidget_snapshot!("spawn_model_selection_popup", popup);
    assert!(popup.contains("Corbanu Terminal Nazgul pane - OpenAI Codex plan"));
    assert!(popup.contains("GPT-5.6-Sol (current)"));

    chat.handle_key_event(KeyEvent::from(KeyCode::Right));
    let switched = render_bottom_popup_with_height(&chat, /*width*/ 100, /*height*/ 30);
    assert!(switched.contains("Corbanu Terminal Nazgul pane - Ambient coding plan"));
    assert!(switched.lines().any(|line| line.contains('›')));
}

#[tokio::test]
async fn codex_pane_model_picker_uses_provider_tabs_and_opens_name_prompt() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some(AMBIENT_DEFAULT_MODEL)).await;
    let original_model = chat.current_model().to_string();
    let presets = chat.model_catalog.try_list_models().expect("model catalog");
    chat.open_all_models_popup_for_purpose(
        presets,
        ModelSelectionPurpose::CodexPane {
            default_model: "gpt-5.6-sol".to_string(),
        },
    );

    let popup = render_bottom_popup_with_height(&chat, /*width*/ 100, /*height*/ 30);
    assert!(popup.contains("New Corbanu Terminal pane - OpenAI Codex plan"));
    assert!(popup.contains("GPT-5.6-Sol (current)"));

    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let (preset, purpose) = match rx.try_recv() {
        Ok(AppEvent::OpenReasoningPopup { model, purpose }) => (model, purpose),
        other => panic!("expected pane reasoning popup event, got {other:?}"),
    };
    assert_matches!(purpose, ModelSelectionPurpose::CodexPane { .. });
    chat.open_reasoning_popup_for_purpose(preset, purpose);
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::OpenCodexPaneNamePrompt {
            model,
            provider: Some(provider),
            effort: Some(_),
        }) if model == "gpt-5.6-sol" && provider == "openai"
    );
    assert_eq!(chat.current_model(), original_model);
}

#[tokio::test]
async fn spawn_effort_popup_snapshot_and_create_path_leave_session_unchanged() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some(AMBIENT_DEFAULT_MODEL)).await;
    chat.set_feature_enabled(Feature::CollaborationModes, /*enabled*/ true);
    let plan_mask = collaboration_modes::mask_for_kind(chat.model_catalog.as_ref(), ModeKind::Plan)
        .expect("plan collaboration mode");
    chat.set_collaboration_mask(plan_mask);
    chat.set_reasoning_effort(Some(ReasoningEffort::Medium));
    while rx.try_recv().is_ok() {}
    let original_model = chat.current_model().to_string();
    let original_effort = chat.current_reasoning_effort();
    let parent = "thread:11111111-1111-4111-8111-111111111111";
    let presets = chat.model_catalog.try_list_models().expect("model catalog");
    chat.open_all_models_popup_for_purpose(
        presets,
        spawn_model_purpose(SpawnRole::Orc, Some(parent), "gpt-5.6-sol"),
    );

    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let (model, purpose) = loop {
        match rx.try_recv().expect("reasoning event") {
            AppEvent::OpenReasoningPopup { model, purpose } => break (model, purpose),
            AppEvent::SettingsSelectionClosed => continue,
            event => panic!("unexpected event: {event:?}"),
        }
    };
    chat.open_reasoning_popup_for_purpose(model, purpose);
    let popup = render_bottom_popup(&chat, /*width*/ 88);
    assert_chatwidget_snapshot!("spawn_effort_selection_popup", popup);
    assert!(
        popup
            .lines()
            .any(|line| line.contains('›') && line.contains("Extra high"))
    );

    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let created = loop {
        match rx.try_recv().expect("create event") {
            AppEvent::CreateSpawnAgent {
                role,
                parent_node_id,
                model,
                provider,
                effort,
                ..
            } => break (role, parent_node_id, model, provider, effort),
            AppEvent::SettingsSelectionClosed => continue,
            event => panic!("spawn selection mutated session: {event:?}"),
        }
    };
    assert_eq!(created.0, SpawnRole::Orc);
    assert_eq!(created.1.as_deref(), Some(parent));
    assert_eq!(created.2, "gpt-5.6-sol");
    assert_eq!(created.3.as_deref(), Some("openai"));
    assert_eq!(created.4, Some(ReasoningEffort::XHigh));
    assert_eq!(chat.current_model(), original_model);
    assert_eq!(chat.current_reasoning_effort(), original_effort);
    assert!(chat.no_modal_or_popup_active());
    assert!(rx.try_recv().is_err());
}

/// #1266: Standard is the catalogue's internal Medium effort, even though its
/// provider description says `reasoning_effort=high`. The role's Deep default
/// must only preselect the popup; moving to Standard must forward Medium.
#[tokio::test]
async fn spawn_glm_reasoning_choice_is_forwarded_without_changing_session() {
    for selected_effort in [ReasoningEffort::Medium, ReasoningEffort::XHigh] {
        let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.6-sol")).await;
        let original = (
            chat.current_model().to_string(),
            chat.current_reasoning_effort(),
        );
        let preset = chat
            .model_catalog
            .try_list_models()
            .expect("model catalog")
            .into_iter()
            .find(|preset| preset.model == ZAI_DEFAULT_MODEL)
            .expect("Z.AI GLM 5.2 preset");
        chat.open_reasoning_popup_for_purpose(
            preset,
            spawn_model_purpose(SpawnRole::Troll, Some("thread:parent"), ZAI_DEFAULT_MODEL),
        );
        let popup = render_bottom_popup(&chat, /*width*/ 100);
        assert!(popup.contains("Standard (default)"));
        assert!(
            popup
                .lines()
                .any(|line| line.contains('›') && line.contains("Deep"))
        );
        if selected_effort == ReasoningEffort::Medium {
            chat.handle_key_event(KeyEvent::from(KeyCode::Up));
        }
        chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

        let created = loop {
            match rx.try_recv().expect("create event") {
                AppEvent::CreateSpawnAgent {
                    role,
                    parent_node_id,
                    model,
                    provider,
                    effort,
                    ..
                } => break (role, parent_node_id, model, provider, effort),
                AppEvent::SettingsSelectionClosed => continue,
                event => panic!("spawn selection mutated session: {event:?}"),
            }
        };
        assert_eq!(
            created,
            (
                SpawnRole::Troll,
                Some("thread:parent".to_string()),
                ZAI_DEFAULT_MODEL.to_string(),
                Some("zai".to_string()),
                Some(selected_effort.clone()),
            )
        );
        assert_eq!(
            ChatWidget::status_line_reasoning_effort_label_for_model(
                ZAI_DEFAULT_MODEL,
                Some(&selected_effort),
            ),
            if selected_effort == ReasoningEffort::Medium {
                "standard"
            } else {
                "deep"
            }
        );
        assert_eq!(
            (
                chat.current_model().to_string(),
                chat.current_reasoning_effort(),
            ),
            original
        );
        assert!(chat.no_modal_or_popup_active());
        assert!(rx.try_recv().is_err());
    }
}

#[tokio::test]
async fn spawn_single_effort_model_creates_directly_for_troll() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some(AMBIENT_DEFAULT_MODEL)).await;
    let preset = ModelPreset {
        id: "single-effort".to_string(),
        model: "gpt-5.6-sol".to_string(),
        provider_id: None,
        orchestration: None,
        display_name: "Single Effort".to_string(),
        description: "test".to_string(),
        default_reasoning_effort: ReasoningEffort::High,
        supported_reasoning_efforts: vec![ReasoningEffortPreset {
            effort: ReasoningEffort::High,
            description: "high".to_string(),
        }],
        supports_personality: false,
        additional_speed_tiers: Vec::new(),
        service_tiers: Vec::new(),
        default_service_tier: None,
        is_default: true,
        upgrade: None,
        show_in_picker: true,
        multi_agent_version: None,
        availability_nux: None,
        supported_in_api: true,
        input_modalities: default_input_modalities(),
    };
    chat.open_all_models_popup_for_purpose(
        vec![preset],
        spawn_model_purpose(SpawnRole::Troll, Some("thread:parent"), "gpt-5.6-sol"),
    );
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let (model, purpose) = match rx.try_recv().expect("reasoning event") {
        AppEvent::OpenReasoningPopup { model, purpose } => (model, purpose),
        event => panic!("unexpected event: {event:?}"),
    };
    chat.open_reasoning_popup_for_purpose(model, purpose);

    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::CreateSpawnAgent {
            role: SpawnRole::Troll,
            parent_node_id: Some(parent),
            model,
            effort: Some(ReasoningEffort::High),
            ..
        }) if parent == "thread:parent" && model == "gpt-5.6-sol"
    );
    assert!(chat.no_modal_or_popup_active());
}

#[tokio::test]
async fn spawn_effort_escape_returns_to_tabs_without_creating_agent() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some(AMBIENT_DEFAULT_MODEL)).await;
    let presets = chat.model_catalog.try_list_models().expect("model catalog");
    chat.open_all_models_popup_for_purpose(
        presets,
        spawn_model_purpose(
            SpawnRole::Nazgul,
            /*parent_node_id*/ None,
            "gpt-5.6-sol",
        ),
    );
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let (model, purpose) = loop {
        match rx.try_recv().expect("reasoning event") {
            AppEvent::OpenReasoningPopup { model, purpose } => break (model, purpose),
            AppEvent::SettingsSelectionClosed => continue,
            event => panic!("unexpected event: {event:?}"),
        }
    };
    chat.open_reasoning_popup_for_purpose(model, purpose);

    chat.handle_key_event(KeyEvent::from(KeyCode::Esc));
    assert!(render_bottom_popup(&chat, /*width*/ 100).contains("Select Model and Effort"));
    chat.handle_key_event(KeyEvent::from(KeyCode::Esc));
    assert!(chat.no_modal_or_popup_active());
    while let Ok(event) = rx.try_recv() {
        assert!(!matches!(event, AppEvent::CreateSpawnAgent { .. }));
    }
}

#[tokio::test]
async fn spawn_picker_only_shows_models_with_constructible_native_providers() {
    let (chat, _rx, _op_rx) = make_chatwidget_manual(Some(AMBIENT_DEFAULT_MODEL)).await;
    let presets = chat.model_catalog.try_list_models().expect("model catalog");
    let mut shown_provider_groups = std::collections::BTreeSet::new();

    for preset in presets
        .iter()
        .filter(|preset| ChatWidget::show_in_pfterminal_model_picker(preset))
    {
        let provider = ChatWidget::model_provider_for_selection(&preset.model)
            .unwrap_or_else(|| panic!("shown spawn model has no provider: {}", preset.model));
        assert!(
            provider == chat.config.model_provider_id
                || chat.config.model_providers.contains_key(&provider),
            "shown spawn model {} resolves to unavailable provider {provider}",
            preset.model
        );
        shown_provider_groups.insert(provider);
    }

    assert!(shown_provider_groups.contains("openai"));
    assert!(shown_provider_groups.contains("claude-plan"));
    assert!(shown_provider_groups.contains("openrouter"));
    assert!(shown_provider_groups.len() >= 5);
}

#[tokio::test]
async fn astra_model_and_reasoning_picker_snapshots() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.6-sol")).await;
    chat.thread_id = Some(ThreadId::new());
    let presets = chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load");
    let astra = presets
        .iter()
        .find(|preset| preset.model == "gpt-6-astra")
        .expect("bundled Astra preset")
        .clone();
    chat.open_all_models_popup(presets);
    let popup = render_bottom_popup_with_height(&chat, /*width*/ 100, /*height*/ 30);
    assert_chatwidget_snapshot!("astra_openai_model_picker", popup);
    chat.open_reasoning_popup(astra);
    let popup = render_bottom_popup_with_height(&chat, /*width*/ 100, /*height*/ 20);
    assert_chatwidget_snapshot!("astra_reasoning_picker", popup);
}

#[tokio::test]
async fn gpt_5_6_model_selection_popup_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.6-sol")).await;
    chat.thread_id = Some(ThreadId::new());
    let mut presets = chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load");
    presets.retain(|preset| preset.model == "gpt-5.5" || preset.model.starts_with("gpt-5.6-"));
    for preset in &mut presets {
        if preset.model.starts_with("gpt-5.6-") {
            preset.show_in_picker = true;
        }
        preset.is_default = preset.model == "gpt-5.6-sol";
    }
    chat.open_all_models_popup(presets);

    let popup = render_bottom_popup_with_height(&chat, /*width*/ 96, /*height*/ 30);
    assert_chatwidget_snapshot!("gpt_5_6_model_selection_popup", popup);
}

#[tokio::test]
async fn personality_selection_popup_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
    chat.thread_id = Some(ThreadId::new());
    chat.open_personality_popup();

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("personality_selection_popup", popup);
}

#[tokio::test]
async fn skills_menu_default_mentions_shortcut_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.open_skills_menu();

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("skills_menu_default_mentions_shortcut", popup);
}

#[tokio::test]
async fn model_picker_hides_show_in_picker_false_models_from_cache() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some(AMBIENT_DEFAULT_MODEL)).await;
    chat.thread_id = Some(ThreadId::new());
    let preset = |slug: &str, show_in_picker: bool| ModelPreset {
        id: slug.to_string(),
        model: slug.to_string(),
        provider_id: None,
        orchestration: None,
        display_name: slug.to_string(),
        description: format!("{slug} description"),
        default_reasoning_effort: ReasoningEffortConfig::Medium,
        supported_reasoning_efforts: vec![ReasoningEffortPreset {
            effort: ReasoningEffortConfig::Medium,
            description: "medium".to_string(),
        }],
        supports_personality: false,
        additional_speed_tiers: Vec::new(),
        service_tiers: Vec::new(),
        default_service_tier: None,
        is_default: false,
        upgrade: None,
        show_in_picker,
        multi_agent_version: None,
        availability_nux: None,
        supported_in_api: true,
        input_modalities: default_input_modalities(),
    };

    let hidden_model = "hidden-test-model";
    chat.open_model_popup_with_presets(vec![
        preset(AMBIENT_DEFAULT_MODEL, true),
        preset(hidden_model, false),
    ]);
    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("model_picker_filters_hidden_models", popup);
    assert!(
        popup.contains(AMBIENT_DEFAULT_MODEL),
        "expected visible model to appear in picker:\n{popup}"
    );
    assert!(
        !popup.contains(hidden_model),
        "expected hidden model to be excluded from picker:\n{popup}"
    );
}

fn move_model_picker_selection_to(chat: &mut ChatWidget, model: &str) {
    let exact_model_field = format!("Model: {model}.");
    for _ in 0..16 {
        for _ in 0..20 {
            let popup =
                render_bottom_popup_with_height(chat, /*width*/ 140, /*height*/ 40);
            if popup
                .lines()
                .any(|line| line.contains('›') && line.contains(&exact_model_field))
            {
                return;
            }
            chat.handle_key_event(KeyEvent::from(KeyCode::Down));
        }
        chat.handle_key_event(KeyEvent::from(KeyCode::Right));
    }

    let popup = render_bottom_popup_with_height(chat, /*width*/ 140, /*height*/ 40);
    panic!("could not select model {model:?} in picker:\n{popup}");
}

#[tokio::test]
async fn model_picker_same_slug_marks_only_exact_provider_current() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some(CLAUDE_FABLE_5_1_PLAN_MODEL)).await;
    let base = chat
        .model_catalog
        .try_list_models()
        .unwrap()
        .into_iter()
        .find(|preset| preset.model == CLAUDE_FABLE_5_1_PLAN_MODEL)
        .unwrap();
    let mut first = base.clone();
    first.provider_id = Some("custom-a".to_string());
    let mut second = base;
    second.provider_id = Some("custom-b".to_string());
    chat.config.model_provider_id = "custom-b".to_string();
    chat.open_all_models_popup(vec![first, second]);
    let popup = render_bottom_popup_with_height(&chat, /*width*/ 120, /*height*/ 20);
    assert_eq!(popup.matches("(current)").count(), 1, "{popup}");
    insta::assert_snapshot!("model_picker_exact_provider_current", popup);
}

#[tokio::test]
async fn model_picker_claude_recovery_refresh_restores_full_subscription_catalog() {
    use crate::provider_status_host::ProviderAccountMetadata;
    use crate::provider_status_host::ProviderStatusHost;
    use codex_provider_auth::ClaudeCredentialMetadata;
    use codex_provider_auth::ClaudeCredentialSource;
    use codex_provider_auth::ProviderRuntimeAuthorizations;
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some(CLAUDE_FABLE_5_1_PLAN_MODEL)).await;
    let home = tempfile::tempdir().unwrap();
    let mut config = crate::legacy_core::config::ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await
        .unwrap();
    config.cli_auth_credentials_store_mode = codex_login::AuthCredentialsStoreMode::File;
    config.model_provider_id = "claude-plan".to_string();
    chat.config.model_provider_id = "claude-plan".to_string();
    let host = ProviderStatusHost::from_config(&config, ProviderAccountMetadata::default());
    chat.model_catalog.set_provider_policy(
        super::super::provider_model_policy::ProviderModelPolicy::new(
            host.clone(),
            ProviderRuntimeAuthorizations::default(),
        ),
    );
    assert!(
        !chat
            .model_catalog
            .provider_is_selectable("claude-plan", CLAUDE_PLAN_MODEL)
    );
    host.update_account_metadata(ProviderAccountMetadata {
        claude: ClaudeCredentialMetadata::Configured {
            source: ClaudeCredentialSource::Managed,
        },
        ..Default::default()
    });
    chat.model_catalog
        .update_provider_statuses(host.resolve().entries());
    chat.model_catalog.sync_runtime_models(
        config.model_providers.keys().map(String::as_str),
        Some(CLAUDE_FABLE_5_1_PLAN_MODEL),
        "claude-plan",
    );
    let presets = chat.model_catalog.try_list_models().unwrap();
    chat.open_all_models_popup(presets);
    let popup = render_bottom_popup_with_height(&chat, /*width*/ 140, /*height*/ 30);
    for model in [
        CLAUDE_PLAN_MODEL,
        CLAUDE_FABLE_5_PLAN_MODEL,
        CLAUDE_FABLE_5_1_PLAN_MODEL,
    ] {
        assert!(popup.contains(model), "{popup}");
    }
    assert_eq!(popup.matches("(current)").count(), 1, "{popup}");
    assert!(!popup.contains("[Other]"), "{popup}");
    assert!(
        !chat
            .model_catalog
            .provider_is_selectable("anthropic", ANTHROPIC_DEFAULT_MODEL)
    );
    insta::assert_snapshot!("model_picker_recovered_claude_subscription", popup);

    // A separately configured API key enables Anthropic without moving the
    // current subscription session or borrowing the subscription credential.
    codex_login::login_with_provider_api_key(
        home.path(),
        "ANTHROPIC_API_KEY",
        "synthetic-picker-anthropic-key",
        config.cli_auth_credentials_store_mode,
        config.auth_keyring_backend_kind(),
    )
    .unwrap();
    chat.model_catalog
        .update_provider_statuses(host.resolve().entries());
    assert!(
        chat.model_catalog
            .provider_is_selectable("anthropic", ANTHROPIC_DEFAULT_MODEL)
    );
    chat.handle_key_event(KeyEvent::from(KeyCode::Esc));
    chat.open_all_models_popup(chat.model_catalog.try_list_models().unwrap());
    chat.handle_key_event(KeyEvent::from(KeyCode::Right));
    let api_popup = render_bottom_popup_with_height(&chat, /*width*/ 140, /*height*/ 30);
    assert!(api_popup.contains("[Anthropic]"), "{api_popup}");
    for model in [
        ANTHROPIC_DEFAULT_MODEL,
        CLAUDE_FABLE_5_MODEL,
        CLAUDE_FABLE_5_1_MODEL,
    ] {
        assert!(
            api_popup.contains(&format!("Model: {model}.")),
            "{api_popup}"
        );
    }
    assert_eq!(api_popup.matches("(current)").count(), 0);
    assert_eq!(chat.config.model_provider_id, "claude-plan");
}

#[tokio::test]
async fn model_picker_hides_fake_openai_models_and_shows_curated_provider_models() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some(AMBIENT_DEFAULT_MODEL)).await;
    chat.thread_id = Some(ThreadId::new());

    assert_eq!(chat.config.model_provider_id, AMBIENT_PROVIDER_ID);
    assert_eq!(chat.model_display_name(), "Ambient GLM 5.2");

    let presets = chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load");
    chat.open_all_models_popup(presets);
    let popup = render_bottom_popup_with_height(&chat, /*width*/ 140, /*height*/ 40);

    assert!(
        popup.contains("Ambient GLM 5.2"),
        "expected Ambient GLM display name in /model picker:\n{popup}"
    );
    assert!(
        popup.contains(AMBIENT_DEFAULT_MODEL),
        "expected Ambient GLM slug in /model picker description:\n{popup}"
    );
    assert!(
        popup.contains("[Ambient]") && popup.contains("OpenAI") && popup.contains("OpenRouter"),
        "expected provider tabs in /model picker:\n{popup}"
    );
    assert!(
        popup.contains("Kimi Code"),
        "expected Kimi Code provider tab in /model picker:\n{popup}"
    );
    assert!(
        popup.contains("Ambient: GLM 5.2 - $0.76/M input, $0.14/M cached input, $2.42/M output."),
        "expected Ambient model description in /model picker:\n{popup}"
    );
    assert_chatwidget_snapshot!("ambient_glm_only_model_picker", popup);
    assert!(
        !popup.contains(&format!("Model: {ZAI_DEFAULT_MODEL}."))
            && !popup.contains(&format!("Model: {CLAUDE_PLAN_MODEL}.")),
        "expected the Ambient tab to contain only Ambient models:\n{popup}"
    );

    let (mut claude_plan_chat, _claude_plan_rx, _claude_plan_op_rx) =
        make_chatwidget_manual(Some(CLAUDE_PLAN_MODEL)).await;
    claude_plan_chat.thread_id = Some(ThreadId::new());
    claude_plan_chat.config.model_provider_id = "claude-plan".to_string();
    let presets = claude_plan_chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load");
    claude_plan_chat.open_all_models_popup(presets);
    let claude_plan_popup =
        render_bottom_popup_with_height(&claude_plan_chat, /*width*/ 140, /*height*/ 36);
    assert!(
        claude_plan_popup.contains("[Claude Plan]")
            && claude_plan_popup.contains(CLAUDE_PLAN_MODEL)
            && claude_plan_popup.contains(CLAUDE_FABLE_5_1_PLAN_MODEL)
            && claude_plan_popup.contains(CLAUDE_FABLE_5_PLAN_MODEL),
        "expected Claude Code models in the Claude Plan tab:\n{claude_plan_popup}"
    );
    insta::assert_snapshot!("claude_plan_model_picker_fable_versions", claude_plan_popup);
    assert!(
        !claude_plan_popup.contains("claude-opus-4-8-plan"),
        "deprecated Claude Opus 4.8 Plan must not appear in the picker:\n{claude_plan_popup}"
    );
    assert!(
        claude_plan_popup.contains("through Claude Code subscription auth"),
        "expected Claude Plan rows to explain subscription auth:\n{claude_plan_popup}"
    );
    let (mut anthropic_chat, _anthropic_rx, _anthropic_op_rx) =
        make_chatwidget_manual(Some(ANTHROPIC_DEFAULT_MODEL)).await;
    anthropic_chat.thread_id = Some(ThreadId::new());
    anthropic_chat.config.model_provider_id = "anthropic".to_string();
    let presets = anthropic_chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load");
    anthropic_chat.open_all_models_popup(presets);
    let anthropic_popup =
        render_bottom_popup_with_height(&anthropic_chat, /*width*/ 140, /*height*/ 36);

    assert!(
        anthropic_popup.contains("[Anthropic]"),
        "expected Anthropic current model to open the Anthropic tab:\n{anthropic_popup}"
    );
    assert!(
        anthropic_popup.contains(ANTHROPIC_DEFAULT_MODEL),
        "expected Anthropic API-key model in /model picker:\n{anthropic_popup}"
    );
    assert!(
        anthropic_popup.contains(CLAUDE_FABLE_5_1_MODEL)
            && anthropic_popup.contains(CLAUDE_FABLE_5_MODEL),
        "expected Claude Fable API-key model in /model picker:\n{anthropic_popup}"
    );
    assert!(
        !anthropic_popup.contains("claude-opus-4-8"),
        "deprecated Claude Opus 4.8 must not appear in the picker:\n{anthropic_popup}"
    );
    assert!(
        !anthropic_popup.contains("openrouter/owl-alpha"),
        "expected the Anthropic tab to exclude OpenRouter models:\n{anthropic_popup}"
    );
    let (mut baseten_chat, _baseten_rx, _baseten_op_rx) =
        make_chatwidget_manual(Some(BASETEN_DEFAULT_MODEL)).await;
    baseten_chat.thread_id = Some(ThreadId::new());
    baseten_chat.config.model_provider_id = "baseten".to_string();
    let presets = baseten_chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load");
    baseten_chat.open_all_models_popup(presets);
    let baseten_popup =
        render_bottom_popup_with_height(&baseten_chat, /*width*/ 140, /*height*/ 28);

    assert!(
        baseten_popup.contains(BASETEN_DEFAULT_MODEL),
        "expected Baseten GLM 5.2 in /model picker:\n{baseten_popup}"
    );
    assert!(
        baseten_popup
            .contains("Baseten: GLM 5.2 - $1.40/M input, $0.14/M cached input, $4.40/M output."),
        "expected Baseten GLM price description in /model picker:\n{baseten_popup}"
    );
    let (mut kimi_chat, _kimi_rx, _kimi_op_rx) =
        make_chatwidget_manual(Some(KIMI_CODE_K3_MODEL)).await;
    kimi_chat.thread_id = Some(ThreadId::new());
    kimi_chat.config.model_provider_id = KIMI_CODE_PROVIDER_ID.to_string();
    let presets = kimi_chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load");
    kimi_chat.open_all_models_popup(presets);
    let kimi_popup =
        render_bottom_popup_with_height(&kimi_chat, /*width*/ 140, /*height*/ 24);
    assert_eq!(
        ChatWidget::model_provider_for_selection(KIMI_CODE_K3_MODEL).as_deref(),
        Some(KIMI_CODE_PROVIDER_ID)
    );
    assert!(
        kimi_popup.contains("[Kimi Code]")
            && kimi_popup.contains("Kimi Code K3")
            && kimi_popup.contains("up to 1M on Allegretto and above"),
        "expected Kimi Code K3 in its own provider tab:\n{kimi_popup}"
    );
    let (mut vercel_chat, _vercel_rx, _vercel_op_rx) =
        make_chatwidget_manual(Some(VERCEL_DEFAULT_MODEL)).await;
    vercel_chat.thread_id = Some(ThreadId::new());
    vercel_chat.config.model_provider_id = VERCEL_PROVIDER_ID.to_string();
    let presets = vercel_chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load");
    vercel_chat.open_all_models_popup(presets);
    let vercel_popup =
        render_bottom_popup_with_height(&vercel_chat, /*width*/ 140, /*height*/ 28);

    assert!(
        vercel_popup.contains(VERCEL_DEFAULT_MODEL),
        "expected Vercel GLM 5.2 in /model picker:\n{vercel_popup}"
    );
    assert!(
        vercel_popup
            .contains("Vercel: GLM 5.2 - $1.40/M input, $0.26/M cached input, $4.40/M output."),
        "expected Vercel GLM price description in /model picker:\n{vercel_popup}"
    );
    for (model, display_name) in [
        (VERCEL_GLM_5_3_FLASH_MODEL, "Vercel GLM 5.3 Flash"),
        (VERCEL_GLM_5_3_MODEL, "Vercel GLM 5.3"),
        (VERCEL_KIMI_K3_MODEL, "Vercel Kimi K3"),
        (VERCEL_DEEPSEEK_V4_PRO_MODEL, "Vercel DeepSeek V4 Pro"),
    ] {
        assert!(
            vercel_popup.contains(model) && vercel_popup.contains(display_name),
            "expected {display_name} ({model}) in the Vercel tab:\n{vercel_popup}"
        );
        assert_eq!(
            ChatWidget::model_provider_for_selection(model).as_deref(),
            Some(VERCEL_PROVIDER_ID),
            "expected {model} to select the Vercel provider"
        );
    }

    let (mut vercel_fast_chat, _vercel_fast_rx, _vercel_fast_op_rx) =
        make_chatwidget_manual(Some(VERCEL_GLM_5_2_FAST_MODEL)).await;
    vercel_fast_chat.thread_id = Some(ThreadId::new());
    vercel_fast_chat.config.model_provider_id = "vercel-anthropic-fast".to_string();
    let presets = vercel_fast_chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load");
    vercel_fast_chat.open_all_models_popup(presets);
    let vercel_fast_popup =
        render_bottom_popup_with_height(&vercel_fast_chat, /*width*/ 140, /*height*/ 28);

    assert!(
        vercel_fast_popup.contains(VERCEL_GLM_5_2_FAST_MODEL),
        "expected Vercel GLM 5.2 Fast in /model picker:\n{vercel_fast_popup}"
    );
    assert!(
        vercel_fast_popup.contains(
            "Vercel: GLM 5.2 Fast - $2.10/M input, $0.21/M cached input, $6.60/M output."
        ),
        "expected Vercel GLM Fast price description in /model picker:\n{vercel_fast_popup}"
    );
    let (mut minimax_chat, _minimax_rx, _minimax_op_rx) =
        make_chatwidget_manual(Some("minimax/minimax-m3")).await;
    minimax_chat.thread_id = Some(ThreadId::new());
    minimax_chat.config.model_provider_id = OPENROUTER_PROVIDER_ID.to_string();
    let presets = minimax_chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load");
    minimax_chat.open_all_models_popup(presets);
    let minimax_popup =
        render_bottom_popup_with_height(&minimax_chat, /*width*/ 140, /*height*/ 28);

    assert!(
        minimax_popup.contains("minimax/minimax-m3"),
        "expected MiniMax M3 in /model picker:\n{minimax_popup}"
    );
    assert!(
        minimax_popup.contains("OpenRouter: MiniMax M3 - $0.60/M input, $2.40/M output."),
        "expected MiniMax M3 price description in /model picker:\n{minimax_popup}"
    );
    assert!(
        minimax_popup.contains("x-ai/grok-4.7"),
        "expected OpenRouter models to share the OpenRouter tab:\n{minimax_popup}"
    );
    // Owl Alpha is retired from the picker; it stays selectable by exact name.
    assert!(
        !minimax_popup.contains("openrouter/owl-alpha"),
        "a hidden OpenRouter row is not offered:\n{minimax_popup}"
    );
    move_model_picker_selection_to(&mut minimax_chat, "moonshotai/kimi-k3");
    let kimi_openrouter_popup =
        render_bottom_popup_with_height(&minimax_chat, /*width*/ 140, /*height*/ 28);
    assert!(
        kimi_openrouter_popup.contains("moonshotai/kimi-k3")
            && kimi_openrouter_popup.contains("OpenRouter: Kimi K3")
            && kimi_openrouter_popup.contains("$3.00/M input, $0.30/M cached input, $15.00/M"),
        "expected Kimi K3 in the OpenRouter tab:\n{kimi_openrouter_popup}"
    );

    let (mut openai_chat, _openai_rx, _openai_op_rx) =
        make_chatwidget_manual(Some("gpt-5.6-sol")).await;
    openai_chat.thread_id = Some(ThreadId::new());
    openai_chat.config.model_provider_id = "openai".to_string();
    let presets = openai_chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load");
    openai_chat.open_all_models_popup(presets);
    let openai_popup =
        render_bottom_popup_with_height(&openai_chat, /*width*/ 140, /*height*/ 28);
    assert!(
        openai_popup.contains("[OpenAI]") && openai_popup.contains("gpt-6-sol"),
        "expected GPT-6 Sol in the OpenAI tab:\n{openai_popup}"
    );
    assert!(
        !openai_popup.contains("Model: gpt-5.5."),
        "expected retired GPT-5.5 to be hidden from /model picker:\n{openai_popup}"
    );
    assert!(
        !openai_popup.contains("gpt-5.4"),
        "expected older OpenAI models to be hidden from /model picker:\n{openai_popup}"
    );
    assert!(
        !openai_popup.contains("codex-auto-review"),
        "expected hidden OpenAI/Codex models to be hidden from /model picker:\n{openai_popup}"
    );

    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    while let Ok(event) = rx.try_recv() {
        if let AppEvent::OpenReasoningPopup { model, purpose } = event {
            chat.open_reasoning_popup_for_purpose(model, purpose);
            break;
        }
    }
    let reasoning_popup = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        reasoning_popup.contains("Select Reasoning Mode"),
        "expected selecting Ambient model to open reasoning picker:\n{reasoning_popup}"
    );
    for label in ["Standard (default)", "Deep"] {
        assert!(
            reasoning_popup.contains(label),
            "expected Ambient reasoning option {label:?} in picker:\n{reasoning_popup}"
        );
    }
    for label in ["None", "Low", "Medium", "High", "Extra high"] {
        assert!(
            !reasoning_popup.contains(label),
            "expected old Ambient reasoning option {label:?} to be hidden:\n{reasoning_popup}"
        );
    }
}

#[tokio::test]
async fn corbanu_api_model_picker_preserves_ambient_and_shows_the_seven_public_routes() {
    let (mut chat, _rx, _op_rx) =
        make_chatwidget_manual(Some(CORBANU_API_GLM_5_3_FLASH_MODEL)).await;
    chat.thread_id = Some(ThreadId::new());
    chat.config.model_provider_id = PFTERMINAL_PLAN_PROVIDER_ID.to_string();

    let mut presets = chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load");
    presets.extend(ChatWidget::corbanu_api_presets(&presets));
    chat.open_all_models_popup(presets);
    let popup = render_bottom_popup_with_height(&chat, /*width*/ 140, /*height*/ 40);

    assert_chatwidget_snapshot!("corbanu_api_model_picker", popup);
    assert!(popup.contains("[Corbanu API]"), "{popup}");
    assert_eq!(popup.matches("DeepSeek V4.1 Flash").count(), 1);
    for model in [
        CORBANU_API_GLM_5_3_FLASH_MODEL,
        AMBIENT_DEFAULT_MODEL,
        CORBANU_API_GLM_5_3_MODEL,
        CORBANU_API_GPT_5_6_LUNA_MODEL,
        CORBANU_API_GPT_5_6_SOL_MODEL,
        CORBANU_API_KIMI_K3_MODEL,
        CORBANU_API_DEEPSEEK_V4_PRO_MODEL,
        CORBANU_API_DEEPSEEK_V4_1_FLASH_MODEL,
    ] {
        assert!(
            popup.contains(model),
            "expected {model} in Corbanu API tab:\n{popup}"
        );
    }
    assert!(popup.contains("Recommended"), "{popup}");
    assert!(popup.contains("Uses balance faster"), "{popup}");
    assert!(popup.contains("Ambient GLM 5.2"), "{popup}");
    assert!(!popup.contains("Claude Fable"), "{popup}");
    assert!(!popup.contains("corbanu/claude-fable-5"), "{popup}");
    for legacy_row in ["Ambient Kimi K2.7 Code", "DeepSeek V4 Pro (Direct)"] {
        assert!(
            !popup.contains(legacy_row),
            "legacy row {legacy_row:?} leaked into Corbanu API tab:\n{popup}"
        );
    }
}

#[tokio::test]
async fn model_picker_dismisses_after_selecting_openrouter_model_without_effort_choices() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some(AMBIENT_DEFAULT_MODEL)).await;
    chat.thread_id = Some(ThreadId::new());

    let presets = chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load");
    chat.open_all_models_popup(presets);

    chat.handle_key_event(KeyEvent::from(KeyCode::Right));
    move_model_picker_selection_to(&mut chat, "minimax/minimax-m3");
    let before = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        before.contains("minimax/minimax-m3"),
        "expected MiniMax OpenRouter row before selection:\n{before}"
    );

    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let mut saw_open_reasoning_popup = false;
    while let Ok(event) = rx.try_recv() {
        if let AppEvent::OpenReasoningPopup { model, purpose } = event {
            assert_eq!(model.model, "minimax/minimax-m3");
            saw_open_reasoning_popup = true;
            chat.open_reasoning_popup_for_purpose(model, purpose);
        }
    }

    assert!(
        saw_open_reasoning_popup,
        "expected selecting Gemini to dispatch through the model apply path"
    );
    assert!(
        chat.no_modal_or_popup_active(),
        "expected model picker to dismiss after selecting a no-effort OpenRouter model"
    );
    let after = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        !after.contains("Select Model and Effort"),
        "model picker should no longer be visible after selection:\n{after}"
    );
}

#[tokio::test]
async fn model_picker_selects_kimi_k3_with_required_max_reasoning() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some(AMBIENT_DEFAULT_MODEL)).await;
    chat.thread_id = Some(ThreadId::new());

    let presets = chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load");
    chat.open_all_models_popup(presets);
    move_model_picker_selection_to(&mut chat, "moonshotai/kimi-k3");
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let mut saw_open_reasoning_popup = false;
    while let Ok(event) = rx.try_recv() {
        if let AppEvent::OpenReasoningPopup { model, purpose } = event {
            assert_eq!(model.model, "moonshotai/kimi-k3");
            saw_open_reasoning_popup = true;
            chat.open_reasoning_popup_for_purpose(model, purpose);
            break;
        }
    }
    assert!(
        saw_open_reasoning_popup,
        "expected Kimi K3 selection to use the model apply path"
    );

    // Max is intentionally behind the explicit advanced-effort step.
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let advanced_preset = std::iter::from_fn(|| rx.try_recv().ok()).find_map(|event| match event {
        AppEvent::OpenAdvancedReasoningPopup { model, .. } => Some(model),
        _ => None,
    });
    chat.open_advanced_reasoning_popup(advanced_preset.expect("Kimi advanced reasoning popup"));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::UpdateModelSelection { model, provider }
            if model == "moonshotai/kimi-k3" && provider.as_deref() == Some(OPENROUTER_PROVIDER_ID)
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::UpdateReasoningEffort(Some(ReasoningEffortConfig::Max))
    )));
}

#[tokio::test]
async fn model_picker_opens_openrouter_reasoning_options_for_gemini() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some(AMBIENT_DEFAULT_MODEL)).await;
    chat.thread_id = Some(ThreadId::new());

    let presets = chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load");
    chat.open_all_models_popup(presets);

    chat.handle_key_event(KeyEvent::from(KeyCode::Right));
    move_model_picker_selection_to(&mut chat, "google/gemini-3.5-flash");
    let before = render_bottom_popup_with_height(&chat, /*width*/ 140, /*height*/ 32);
    assert!(
        before.contains("google/gemini-3.5-flash"),
        "expected Gemini OpenRouter row before selection:\n{before}"
    );

    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let mut saw_open_reasoning_popup = false;
    while let Ok(event) = rx.try_recv() {
        if let AppEvent::OpenReasoningPopup { model, purpose } = event {
            assert_eq!(model.model, "google/gemini-3.5-flash");
            saw_open_reasoning_popup = true;
            chat.open_reasoning_popup_for_purpose(model, purpose);
        }
    }

    assert!(
        saw_open_reasoning_popup,
        "expected selecting Gemini to open the OpenRouter reasoning picker"
    );
    let reasoning_popup = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        reasoning_popup.contains("Select Reasoning Level"),
        "expected OpenRouter Gemini to use the generic reasoning picker:\n{reasoning_popup}"
    );
    for label in ["Minimal", "Low", "Medium", "High"] {
        assert!(
            reasoning_popup.contains(label),
            "expected Gemini reasoning option {label:?} in picker:\n{reasoning_popup}"
        );
    }
    assert!(
        !reasoning_popup.contains("(default)"),
        "expected Gemini reasoning picker not to invent a Corbanu Terminal default:\n{reasoning_popup}"
    );
}

#[tokio::test]
async fn model_picker_opens_vercel_glm_reasoning_modes() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some(AMBIENT_DEFAULT_MODEL)).await;
    chat.thread_id = Some(ThreadId::new());

    let preset = chat
        .model_catalog
        .try_list_models()
        .expect("model catalog should load")
        .into_iter()
        .find(|preset| preset.model == VERCEL_DEFAULT_MODEL)
        .expect("Vercel GLM should be in the model catalog");
    assert_eq!(preset.supported_reasoning_efforts.len(), 2);

    chat.open_reasoning_popup(preset);

    let reasoning_popup = render_bottom_popup(&chat, /*width*/ 100);
    assert!(
        reasoning_popup.contains("Select Reasoning Mode"),
        "expected Vercel GLM to use the GLM reasoning mode picker:\n{reasoning_popup}"
    );
    for label in ["Standard (default)", "Deep"] {
        assert!(
            reasoning_popup.contains(label),
            "expected Vercel reasoning option {label:?} in picker:\n{reasoning_popup}"
        );
    }
}

#[tokio::test]
async fn server_overloaded_error_does_not_switch_models() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(Some("gpt-5.2")).await;
    chat.set_model("gpt-5.2");
    while rx.try_recv().is_ok() {}
    while op_rx.try_recv().is_ok() {}

    handle_error(
        &mut chat,
        "server overloaded",
        Some(CodexErrorInfo::ServerOverloaded),
    );

    while let Ok(event) = rx.try_recv() {
        if let AppEvent::UpdateModel(model) = event {
            assert_eq!(
                model, "gpt-5.2",
                "did not expect model switch on server-overloaded error"
            );
        }
    }

    while let Ok(event) = op_rx.try_recv() {
        if let Op::OverrideTurnContext { model, .. } = event {
            assert!(
                model.is_none(),
                "did not expect OverrideTurnContext model update on server-overloaded error"
            );
        }
    }
}

#[tokio::test]
async fn model_reasoning_selection_popup_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;

    set_chatgpt_auth(&mut chat);
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::High));

    let mut preset = get_available_model(&chat, "gpt-5.4");
    preset.supported_reasoning_efforts.insert(
        2,
        ReasoningEffortPreset {
            effort: ReasoningEffortConfig::Max,
            description: "Maximum available reasoning".to_string(),
        },
    );
    preset
        .supported_reasoning_efforts
        .push(ReasoningEffortPreset {
            effort: ReasoningEffortConfig::Ultra,
            description: "Ultra reasoning".to_string(),
        });
    chat.open_reasoning_popup(preset);

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("model_reasoning_selection_popup", popup);
}

#[tokio::test]
async fn model_advanced_reasoning_selection_popup_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::Ultra));

    let mut preset = get_available_model(&chat, "gpt-5.4");
    preset.supported_reasoning_efforts.extend([
        ReasoningEffortPreset {
            effort: ReasoningEffortConfig::Ultra,
            description: "Ultra reasoning".to_string(),
        },
        ReasoningEffortPreset {
            effort: ReasoningEffortConfig::Max,
            description: "Maximum available reasoning".to_string(),
        },
    ]);
    chat.open_advanced_reasoning_popup(preset);

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("model_advanced_reasoning_selection_popup", popup);
}

#[tokio::test]
async fn model_reasoning_selection_popup_applies_custom_effort() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
    let custom_effort = ReasoningEffortConfig::Custom("future".to_string());
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::XHigh));

    let mut preset = get_available_model(&chat, "gpt-5.4");
    preset
        .supported_reasoning_efforts
        .push(ReasoningEffortPreset {
            effort: custom_effort.clone(),
            description: "Maximum available reasoning".to_string(),
        });
    chat.open_reasoning_popup(preset);
    while rx.try_recv().is_ok() {}

    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let selected_effort_events = std::iter::from_fn(|| rx.try_recv().ok())
        .filter_map(|event| match event {
            AppEvent::UpdateReasoningEffort(effort) => Some((None, effort)),
            AppEvent::PersistModelSelection { model, effort, .. } => Some((Some(model), effort)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        selected_effort_events,
        vec![
            (None, Some(custom_effort.clone())),
            (Some("gpt-5.4".to_string()), Some(custom_effort)),
        ]
    );
}

async fn select_ultra_with_multi_agent_thread_limit(max_threads: usize) -> (bool, Vec<String>) {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
    chat.config
        .multi_agent_v2
        .max_concurrent_threads_per_session = max_threads;
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::High));

    let mut preset = get_available_model(&chat, "gpt-5.4");
    preset.default_reasoning_effort = ReasoningEffortConfig::High;
    preset.supported_reasoning_efforts = vec![
        ReasoningEffortPreset {
            effort: ReasoningEffortConfig::High,
            description: "High reasoning".to_string(),
        },
        ReasoningEffortPreset {
            effort: ReasoningEffortConfig::Ultra,
            description: "Ultra reasoning".to_string(),
        },
    ];
    chat.open_reasoning_popup(preset);
    while rx.try_recv().is_ok() {}

    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let advanced_preset = std::iter::from_fn(|| rx.try_recv().ok()).find_map(|event| match event {
        AppEvent::OpenAdvancedReasoningPopup { model, .. } => Some(model),
        _ => None,
    });
    chat.open_advanced_reasoning_popup(advanced_preset.expect("advanced reasoning popup"));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let mut selected_ultra = false;
    let mut warnings = Vec::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            AppEvent::ApplyAdvancedReasoning {
                effort: ReasoningEffortConfig::Ultra,
                ..
            } => {
                selected_ultra = true;
            }
            AppEvent::InsertHistoryCell(cell) => {
                warnings.push(lines_to_single_string(&cell.display_lines(/*width*/ 80)));
            }
            _ => {}
        }
    }

    (selected_ultra, warnings)
}

#[tokio::test]
async fn ultra_reasoning_selection_warns_for_high_multi_agent_concurrency() {
    let (selected_ultra, warnings) =
        select_ultra_with_multi_agent_thread_limit(/*max_threads*/ 8).await;

    assert!(selected_ultra);
    assert_eq!(warnings.len(), 1);
    assert_chatwidget_snapshot!(
        "ultra_reasoning_selection_high_multi_agent_concurrency_warning",
        &warnings[0]
    );
}

#[tokio::test]
async fn ultra_reasoning_selection_skips_warning_below_threshold() {
    let below_threshold = select_ultra_with_multi_agent_thread_limit(/*max_threads*/ 7).await;

    assert_eq!(below_threshold, (true, Vec::new()));
}

/// #468: Max or Ultra chosen under "More reasoning…" for a spawned agent
/// spawns it with that effort and never changes or saves the session model.
#[tokio::test]
async fn spawn_advanced_reasoning_spawns_instead_of_saving_session_model() {
    for advanced in [ReasoningEffortConfig::Max, ReasoningEffortConfig::Ultra] {
        let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
        chat.config
            .multi_agent_v2
            .max_concurrent_threads_per_session = 8;
        let mut preset = get_available_model(&chat, "gpt-5.4");
        preset.default_reasoning_effort = ReasoningEffortConfig::Low;
        preset.supported_reasoning_efforts = vec![
            ReasoningEffortPreset {
                effort: ReasoningEffortConfig::Low,
                description: "Low reasoning".to_string(),
            },
            ReasoningEffortPreset {
                effort: advanced.clone(),
                description: "Advanced reasoning".to_string(),
            },
        ];
        let purpose = spawn_model_purpose(SpawnRole::Troll, Some("node-1"), "gpt-5.4");
        chat.open_reasoning_popup_for_purpose(preset, purpose.clone());
        while rx.try_recv().is_ok() {}

        chat.handle_key_event(KeyEvent::from(KeyCode::Down));
        chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
        let (model, forwarded) = std::iter::from_fn(|| rx.try_recv().ok())
            .find_map(|event| match event {
                AppEvent::OpenAdvancedReasoningPopup { model, purpose } => Some((model, purpose)),
                _ => None,
            })
            .expect("More reasoning… opens the advanced popup");
        assert_eq!(forwarded, purpose);

        chat.open_advanced_reasoning_popup_for_purpose(model, forwarded);
        chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
        let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
        assert!(
            events.iter().any(|event| matches!(
                event,
                AppEvent::CreateSpawnAgent {
                    role: SpawnRole::Troll,
                    parent_node_id: Some(parent),
                    model,
                    effort: Some(effort),
                    ..
                } if parent == "node-1" && model == "gpt-5.4" && *effort == advanced
            )),
            "{advanced:?} must spawn the Troll: {events:?}"
        );
        assert!(
            events.iter().all(|event| !matches!(
                event,
                AppEvent::PersistModelSelection { .. }
                    | AppEvent::UpdateModelSelection { .. }
                    | AppEvent::UpdateReasoningEffort(_)
                    | AppEvent::ApplyAdvancedReasoning { .. }
            )),
            "{advanced:?} for a spawn must not touch the session model: {events:?}"
        );
        // Ultra still warns about multi-agent concurrency for the new pane.
        assert_eq!(
            events
                .iter()
                .any(|event| matches!(event, AppEvent::InsertHistoryCell(_))),
            advanced == ReasoningEffortConfig::Ultra,
            "{events:?}"
        );

        // A new Corbanu Terminal pane gets the effort the same way.
        let pane = ModelSelectionPurpose::CodexPane {
            default_model: "gpt-5.4".to_string(),
        };
        let mut preset = get_available_model(&chat, "gpt-5.4");
        preset.supported_reasoning_efforts = vec![ReasoningEffortPreset {
            effort: advanced.clone(),
            description: "Advanced reasoning".to_string(),
        }];
        chat.open_advanced_reasoning_popup_for_purpose(preset, pane);
        chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
        let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
        assert!(
            events.iter().any(|event| matches!(
                event,
                AppEvent::OpenCodexPaneNamePrompt { model, effort: Some(effort), .. }
                    if model == "gpt-5.4" && *effort == advanced
            )),
            "{advanced:?} must open the pane prompt: {events:?}"
        );
        assert!(
            events.iter().all(|event| !matches!(
                event,
                AppEvent::PersistModelSelection { .. }
                    | AppEvent::UpdateModelSelection { .. }
                    | AppEvent::UpdateReasoningEffort(_)
                    | AppEvent::ApplyAdvancedReasoning { .. }
            )),
            "{advanced:?} for a pane must not touch the session model: {events:?}"
        );
    }
}

#[tokio::test]
async fn max_reasoning_selection_persists_model_selection() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::High));

    let mut preset = get_available_model(&chat, "gpt-5.4");
    preset.supported_reasoning_efforts = vec![ReasoningEffortPreset {
        effort: ReasoningEffortConfig::Max,
        description: "Maximum reasoning".to_string(),
    }];
    chat.open_advanced_reasoning_popup(preset);
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::UpdateReasoningEffort(Some(ReasoningEffortConfig::Max))
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::PersistModelSelection {
            model,
            provider: _,
            effort: Some(ReasoningEffortConfig::Max),
        } if model == "gpt-5.4"
    )));
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, AppEvent::ApplyAdvancedReasoning { .. }))
    );
}

#[tokio::test]
async fn model_reasoning_selection_popup_extra_high_warning_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.2")).await;

    set_chatgpt_auth(&mut chat);
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::XHigh));

    let preset = get_available_model(&chat, "gpt-5.2");
    chat.open_reasoning_popup(preset);

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("model_reasoning_selection_popup_extra_high_warning", popup);
}

async fn assert_reasoning_shortcuts_update_effort(
    key_events: [KeyEvent; 2],
    expected_effort: ReasoningEffortConfig,
) {
    for key_event in key_events {
        let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
        chat.thread_id = Some(ThreadId::new());
        chat.set_reasoning_effort(Some(ReasoningEffortConfig::Medium));

        chat.handle_key_event(key_event);

        let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, AppEvent::UpdateModel(_))),
            "did not expect model update event for {key_event:?}; events: {events:?}"
        );
        assert!(
            events.iter().any(|event| matches!(
                event,
                AppEvent::UpdateReasoningEffort(Some(effort)) if effort == &expected_effort
            )),
            "expected reasoning update event for {key_event:?}; events: {events:?}"
        );
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, AppEvent::PersistModelSelection { .. })),
            "expected no model persistence event for {key_event:?}; events: {events:?}"
        );
    }
}

#[tokio::test]
async fn reasoning_up_shortcuts_raise_reasoning_effort() {
    assert_reasoning_shortcuts_update_effort(
        [
            KeyEvent::new(KeyCode::Char('.'), KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT),
        ],
        ReasoningEffortConfig::High,
    )
    .await;
}

#[tokio::test]
async fn reasoning_down_shortcuts_lower_reasoning_effort() {
    assert_reasoning_shortcuts_update_effort(
        [
            KeyEvent::new(KeyCode::Char(','), KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT),
        ],
        ReasoningEffortConfig::Low,
    )
    .await;
}

#[tokio::test]
async fn reasoning_shortcut_clears_armed_quit_shortcut() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
    chat.thread_id = Some(ThreadId::new());
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::Medium));
    chat.arm_quit_shortcut(key_hint::ctrl(KeyCode::Char('c')));

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::ALT));

    assert!(!chat.bottom_pane.quit_shortcut_hint_visible());
    assert!(chat.quit_shortcut_expires_at.is_none());
    assert!(chat.quit_shortcut_key.is_none());
    let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, AppEvent::Exit(_))),
        "did not expect reasoning shortcut to quit; events: {events:?}"
    );
}

#[tokio::test]
async fn reasoning_shortcut_is_ignored_with_model_popup_open() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
    chat.thread_id = Some(ThreadId::new());
    chat.set_reasoning_effort(Some(ReasoningEffortConfig::Medium));
    chat.open_model_popup();

    chat.handle_key_event(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::ALT));

    let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, AppEvent::UpdateReasoningEffort(_))),
        "did not expect reasoning update while popup is active; events: {events:?}"
    );
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, AppEvent::PersistModelSelection { .. })),
        "did not expect model persistence while popup is active; events: {events:?}"
    );
}

#[tokio::test]
async fn reasoning_up_shortcut_does_not_silently_enter_advanced_effort() {
    for (model, model_path) in [
        ("gpt-5.4", "All models → gpt-5.4"),
        ("codex-auto-test", "codex-auto-test"),
    ] {
        let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
        chat.thread_id = Some(ThreadId::new());
        let mut preset = get_available_model(&chat, "gpt-5.4");
        preset.id = model.to_string();
        preset.model = model.to_string();
        preset.display_name = model.to_string();
        preset.supported_reasoning_efforts.extend([
            ReasoningEffortPreset {
                effort: ReasoningEffortConfig::Max,
                description: "Maximum reasoning".to_string(),
            },
            ReasoningEffortPreset {
                effort: ReasoningEffortConfig::Ultra,
                description: "Ultra reasoning".to_string(),
            },
        ]);
        chat.model_catalog = std::sync::Arc::new(ModelCatalog::new(vec![preset]));
        chat.set_model(model);

        for effort in [ReasoningEffortConfig::XHigh, ReasoningEffortConfig::Max] {
            chat.set_reasoning_effort(Some(effort));
            chat.handle_key_event(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::ALT));

            let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
            assert!(events.iter().all(|event| !matches!(
                event,
                AppEvent::UpdateReasoningEffort(_) | AppEvent::ApplyAdvancedReasoning { .. }
            )));
            let messages = events
                .into_iter()
                .filter_map(|event| match event {
                    AppEvent::InsertHistoryCell(cell) => {
                        Some(lines_to_single_string(&cell.display_lines(/*width*/ 140)))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                messages,
                vec![format!(
                    "• Max and Ultra are available under /model → {model_path} → More reasoning…\n"
                )]
            );
        }
    }
}

#[tokio::test]
async fn reasoning_down_shortcut_can_leave_advanced_effort() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
    chat.thread_id = Some(ThreadId::new());
    let mut preset = get_available_model(&chat, "gpt-5.4");
    preset.supported_reasoning_efforts.extend([
        ReasoningEffortPreset {
            effort: ReasoningEffortConfig::Ultra,
            description: "Ultra reasoning".to_string(),
        },
        ReasoningEffortPreset {
            effort: ReasoningEffortConfig::Max,
            description: "Maximum reasoning".to_string(),
        },
    ]);
    chat.model_catalog = std::sync::Arc::new(ModelCatalog::new(vec![preset]));

    for (current, expected) in [
        (ReasoningEffortConfig::Ultra, ReasoningEffortConfig::Max),
        (ReasoningEffortConfig::Max, ReasoningEffortConfig::XHigh),
    ] {
        chat.set_reasoning_effort(Some(current));
        chat.handle_key_event(KeyEvent::new(KeyCode::Char(','), KeyModifiers::ALT));

        let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
        assert!(events.iter().any(|event| matches!(
            event,
            AppEvent::UpdateReasoningEffort(Some(effort)) if effort == &expected
        )));
        assert!(
            events
                .iter()
                .all(|event| !matches!(event, AppEvent::PersistModelSelection { .. }))
        );
    }
}

#[tokio::test]
async fn reasoning_popup_shows_extra_high_with_space() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;

    set_chatgpt_auth(&mut chat);

    let preset = get_available_model(&chat, "gpt-5.4");
    chat.open_reasoning_popup(preset);

    let popup = render_bottom_popup(&chat, /*width*/ 120);
    assert!(
        popup.contains("Extra high"),
        "expected popup to include 'Extra high'; popup: {popup}"
    );
    assert!(
        !popup.contains("Extrahigh"),
        "expected popup not to include 'Extrahigh'; popup: {popup}"
    );
}

#[tokio::test]
async fn single_reasoning_option_skips_selection() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    let single_effort = vec![ReasoningEffortPreset {
        effort: ReasoningEffortConfig::High,
        description: "Greater reasoning depth for complex or ambiguous problems".to_string(),
    }];
    let preset = ModelPreset {
        id: "model-with-single-reasoning".to_string(),
        model: "model-with-single-reasoning".to_string(),
        provider_id: None,
        orchestration: None,
        display_name: "model-with-single-reasoning".to_string(),
        description: "".to_string(),
        default_reasoning_effort: ReasoningEffortConfig::High,
        supported_reasoning_efforts: single_effort,
        supports_personality: false,
        additional_speed_tiers: Vec::new(),
        service_tiers: Vec::new(),
        default_service_tier: None,
        is_default: false,
        upgrade: None,
        show_in_picker: true,
        multi_agent_version: None,
        availability_nux: None,
        supported_in_api: true,
        input_modalities: default_input_modalities(),
    };
    chat.open_reasoning_popup(preset);

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert!(
        !popup.contains("Select Reasoning Level"),
        "expected reasoning selection popup to be skipped"
    );

    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }

    assert!(
        events
            .iter()
            .any(|ev| matches!(ev, AppEvent::UpdateReasoningEffort(Some(effort)) if *effort == ReasoningEffortConfig::High)),
        "expected reasoning effort to be applied automatically; events: {events:?}"
    );
}

#[tokio::test]
async fn advanced_only_reasoning_option_requires_explicit_selection() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let mut preset = get_available_model(&chat, "gpt-5.4");
    preset.default_reasoning_effort = ReasoningEffortConfig::Ultra;
    preset.supported_reasoning_efforts = vec![ReasoningEffortPreset {
        effort: ReasoningEffortConfig::Ultra,
        description: "Ultra reasoning".to_string(),
    }];
    chat.open_reasoning_popup(preset);

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("advanced_only_reasoning_selection_popup", popup);
    let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(events.iter().all(|event| !matches!(
        event,
        AppEvent::UpdateReasoningEffort(_)
            | AppEvent::ApplyAdvancedReasoning { .. }
            | AppEvent::PersistModelSelection { .. }
    )));
}

#[tokio::test]
async fn curated_model_advertising_advanced_effort_opens_reasoning_picker() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let mut preset = get_available_model(&chat, "gpt-5.6-terra");
    preset.default_reasoning_effort = ReasoningEffortConfig::Medium;
    preset
        .supported_reasoning_efforts
        .push(ReasoningEffortPreset {
            effort: ReasoningEffortConfig::Ultra,
            description: "Ultra reasoning".to_string(),
        });
    chat.open_model_popup_with_presets(vec![preset]);

    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));

    let events = std::iter::from_fn(|| rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(events.iter().all(|event| !matches!(
        event,
        AppEvent::UpdateReasoningEffort(_)
            | AppEvent::ApplyAdvancedReasoning { .. }
            | AppEvent::PersistModelSelection { .. }
    )));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AppEvent::OpenReasoningPopup { .. }))
    );
}

#[tokio::test]
async fn feedback_selection_popup_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    // Open the feedback category selection popup via slash command.
    chat.dispatch_command(SlashCommand::Feedback);

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("feedback_selection_popup", popup);
}

#[tokio::test]
async fn feedback_upload_consent_popup_snapshot() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    chat.show_selection_view(crate::bottom_pane::feedback_upload_consent_params(
        chat.app_event_tx.clone(),
        crate::app_event::FeedbackCategory::Bug,
        chat.current_rollout_path.clone(),
        Some("auto-review-rollout-thread-1.jsonl".to_string()),
        /*include_windows_sandbox_log*/ true,
        &codex_feedback::FeedbackDiagnostics::new(vec![codex_feedback::FeedbackDiagnostic {
            headline: "Proxy environment variables are set and may affect connectivity."
                .to_string(),
            details: vec!["HTTPS_PROXY = hello".to_string()],
        }]),
    ));

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("feedback_upload_consent_popup", popup);
}

#[tokio::test]
async fn feedback_good_result_consent_popup_includes_connectivity_diagnostics_filename() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    chat.show_selection_view(crate::bottom_pane::feedback_upload_consent_params(
        chat.app_event_tx.clone(),
        crate::app_event::FeedbackCategory::GoodResult,
        chat.current_rollout_path.clone(),
        Some("auto-review-rollout-thread-1.jsonl".to_string()),
        /*include_windows_sandbox_log*/ false,
        &codex_feedback::FeedbackDiagnostics::new(vec![codex_feedback::FeedbackDiagnostic {
            headline: "Proxy environment variables are set and may affect connectivity."
                .to_string(),
            details: vec!["HTTPS_PROXY = hello".to_string()],
        }]),
    ));

    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("feedback_good_result_consent_popup", popup);
}

#[tokio::test]
async fn reasoning_popup_escape_returns_to_model_popup() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
    chat.thread_id = Some(ThreadId::new());
    chat.open_model_popup();

    let preset = get_available_model(&chat, "gpt-5.4");
    chat.open_reasoning_popup(preset);

    let before_escape = render_bottom_popup(&chat, /*width*/ 80);
    assert!(before_escape.contains("Select Reasoning Level"));

    chat.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    let after_escape = render_bottom_popup(&chat, /*width*/ 80);
    assert!(after_escape.contains("Select Model"));
    assert!(!after_escape.contains("Select Reasoning Level"));
}

#[tokio::test]
async fn model_picker_runtime_refresh_keeps_claude_out_of_openai_tab() {
    use codex_model_provider_info::CLAUDE_PLAN_PROVIDER_ID;
    let (mut chat, _, _) = make_chatwidget_manual(Some(CLAUDE_FABLE_5_PLAN_MODEL)).await;
    chat.config.model_provider_id = CLAUDE_PLAN_PROVIDER_ID.to_string();
    chat.thread_id = Some(ThreadId::new());
    chat.model_catalog.sync_runtime_models(
        ["openai", CLAUDE_PLAN_PROVIDER_ID],
        Some(CLAUDE_FABLE_5_PLAN_MODEL),
        CLAUDE_PLAN_PROVIDER_ID,
    );
    let presets = chat.model_catalog.try_list_models().unwrap();
    chat.open_all_models_popup(presets);
    let claude = render_bottom_popup_with_height(&chat, /*width*/ 140, /*height*/ 36);
    assert!(claude.contains("[Claude Plan]"), "{claude}");
    assert!(claude.contains(&format!("Model: {CLAUDE_FABLE_5_PLAN_MODEL}.")));
    for _ in 0..16 {
        let popup = render_bottom_popup_with_height(&chat, /*width*/ 140, /*height*/ 36);
        if popup.contains("[OpenAI]") {
            assert!(
                !popup.contains(&format!("Model: {CLAUDE_FABLE_5_PLAN_MODEL}.")),
                "{popup}"
            );
            insta::assert_snapshot!("openai_tab_after_claude_runtime_refresh", popup);
            return;
        }
        chat.handle_key_event(KeyEvent::from(KeyCode::Right));
    }
    panic!("OpenAI tab unavailable");
}
