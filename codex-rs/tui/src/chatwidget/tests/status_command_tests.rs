use super::*;
use assert_matches::assert_matches;
use codex_utils_path_uri::PathUri;

#[tokio::test]
async fn status_command_renders_immediately_and_refreshes_rate_limits_for_chatgpt_auth() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);

    chat.dispatch_command(SlashCommand::Status);

    let rendered = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => {
            lines_to_single_string(&cell.display_lines(/*width*/ 80))
        }
        other => panic!("expected status output before refresh request, got {other:?}"),
    };
    assert!(
        !rendered.contains("refreshing limits"),
        "expected /status to avoid transient refresh text in terminal history, got: {rendered}"
    );
    let request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StatusCommand { request_id },
        }) => request_id,
        other => panic!("expected rate-limit refresh request, got {other:?}"),
    };
    pretty_assertions::assert_eq!(request_id, 0);
}

#[tokio::test]
async fn status_command_refresh_updates_cached_limits_for_future_status_outputs() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);

    chat.dispatch_command(SlashCommand::Status);

    match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(_)) => {}
        other => panic!("expected status output before refresh request, got {other:?}"),
    }
    let first_request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StatusCommand { request_id },
        }) => request_id,
        other => panic!("expected rate-limit refresh request, got {other:?}"),
    };

    chat.finish_status_rate_limit_refresh(first_request_id, vec![snapshot(/*percent*/ 92.0)]);
    drain_insert_history(&mut rx);

    chat.dispatch_command(SlashCommand::Status);
    let refreshed = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => {
            lines_to_single_string(&cell.display_lines(/*width*/ 80))
        }
        other => panic!("expected refreshed status output, got {other:?}"),
    };
    assert!(
        refreshed.contains("8% left"),
        "expected a future /status output to use refreshed cached limits, got: {refreshed}"
    );
}

#[tokio::test]
async fn status_command_renders_immediately_without_rate_limit_refresh() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    chat.dispatch_command(SlashCommand::Status);

    assert_matches!(rx.try_recv(), Ok(AppEvent::InsertHistoryCell(_)));
    assert!(
        !std::iter::from_fn(|| rx.try_recv().ok())
            .any(|event| matches!(event, AppEvent::RefreshRateLimits { .. })),
        "non-ChatGPT sessions should not request a rate-limit refresh for /status"
    );
}

#[tokio::test]
async fn status_command_uses_catalog_default_reasoning_when_config_empty() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.4")).await;
    chat.config.model_reasoning_effort = None;

    chat.dispatch_command(SlashCommand::Status);

    let rendered = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => {
            lines_to_single_string(&cell.display_lines(/*width*/ 80))
        }
        other => panic!("expected status output, got {other:?}"),
    };
    assert!(
        rendered.contains("gpt-5.4 (reasoning medium, summaries auto)"),
        "expected /status to render the catalog default reasoning effort, got: {rendered}"
    );
}

#[tokio::test]
async fn status_command_renders_native_and_foreign_instruction_sources() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let (foreign_source, foreign_display) = if cfg!(windows) {
        (
            PathUri::parse("file:///remote/AGENTS.md").expect("POSIX instruction source"),
            "/remote/AGENTS.md",
        )
    } else {
        (
            PathUri::parse("file:///C:/remote/AGENTS.md").expect("Windows instruction source"),
            r"C:\remote\AGENTS.md",
        )
    };
    chat.instruction_source_paths = vec![
        PathUri::from_abs_path(&chat.config.cwd.join("AGENTS.md")),
        foreign_source,
    ];

    chat.dispatch_command(SlashCommand::Status);

    let rendered = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => {
            lines_to_single_string(&cell.display_lines(/*width*/ 80))
        }
        other => panic!("expected status output, got {other:?}"),
    };
    assert!(
        rendered.contains(&format!("AGENTS.md, {foreign_display}")),
        "expected /status to show native-relative and environment-native foreign paths, got: {rendered}"
    );
    assert!(
        !rendered.contains("Agents.md  <none>"),
        "expected /status to avoid stale <none> when app-server provided instruction sources, got: {rendered}"
    );
}

#[tokio::test]
async fn status_command_overlapping_refreshes_update_matching_cells_only() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);

    chat.dispatch_command(SlashCommand::Status);
    match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(_)) => {}
        other => panic!("expected first status output, got {other:?}"),
    }
    let first_request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StatusCommand { request_id },
        }) => request_id,
        other => panic!("expected first refresh request, got {other:?}"),
    };

    chat.dispatch_command(SlashCommand::Status);
    let second_rendered = match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => {
            lines_to_single_string(&cell.display_lines(/*width*/ 80))
        }
        other => panic!("expected second status output, got {other:?}"),
    };
    let second_request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StatusCommand { request_id },
        }) => request_id,
        other => panic!("expected second refresh request, got {other:?}"),
    };

    assert_ne!(first_request_id, second_request_id);
    assert!(
        !second_rendered.contains("refreshing limits"),
        "expected /status to avoid transient refresh text in terminal history, got: {second_rendered}"
    );

    chat.finish_status_rate_limit_refresh(first_request_id, Vec::new());
    pretty_assertions::assert_eq!(chat.refreshing_status_outputs.len(), 1);

    chat.finish_status_rate_limit_refresh(second_request_id, vec![snapshot(/*percent*/ 92.0)]);
    assert!(chat.refreshing_status_outputs.is_empty());
}

#[tokio::test]
async fn account_update_rejects_stale_status_rate_limit_snapshots() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    set_chatgpt_auth(&mut chat);
    chat.dispatch_command(SlashCommand::Status);
    assert_matches!(rx.try_recv(), Ok(AppEvent::InsertHistoryCell(_)));
    let request_id = match rx.try_recv() {
        Ok(AppEvent::RefreshRateLimits {
            origin: RateLimitRefreshOrigin::StatusCommand { request_id },
        }) => request_id,
        other => panic!("expected status refresh request, got {other:?}"),
    };

    chat.update_account_state(
        /*status_account_display*/ None, /*plan_type*/ None,
        /*has_chatgpt_account*/ true, /*has_codex_backend_auth*/ true,
    );
    chat.finish_status_rate_limit_refresh(request_id, vec![snapshot(/*percent*/ 92.0)]);

    assert!(chat.rate_limit_snapshots_by_limit_id.is_empty());
}

/// PF-84: a Z.AI session whose threads (the coordinator on `default`, a
/// spawned agent on `work`, a worker on another provider) reported usage.
fn zai_session_with_account_usage(chat: &mut ChatWidget, named_accounts: bool) {
    chat.config.model_provider_id = "zai".to_string();
    chat.config.model_provider = codex_model_provider_info::ModelProviderInfo {
        name: "Z.AI".to_string(),
        env_key: Some("PF84_STATUS_ACCOUNT_USAGE_UNSET_KEY".to_string()),
        ..Default::default()
    };
    if named_accounts {
        chat.config
            .features
            .enable(codex_features::Feature::NamedAccounts)
            .expect("enable named accounts");
    }
    let report = |provider_id: &str, account: &str, input: i64, output: i64| {
        codex_app_server_protocol::ThreadAccountTokenUsage {
            provider_id: provider_id.to_string(),
            account: account.to_string(),
            usage: codex_app_server_protocol::TokenUsageBreakdown {
                total_tokens: input + output,
                input_tokens: input,
                cached_input_tokens: 0,
                cache_write_input_tokens: 0,
                output_tokens: output,
                reasoning_output_tokens: 0,
            },
        }
    };
    chat.record_thread_account_usage(
        ThreadId::new(),
        vec![report(
            "zai", "default", /*input*/ 1_000, /*output*/ 100,
        )],
    );
    chat.record_thread_account_usage(
        ThreadId::new(),
        vec![report(
            "zai", "work", /*input*/ 400, /*output*/ 40,
        )],
    );
    chat.record_thread_account_usage(
        ThreadId::new(),
        vec![report("kimi", "alt", /*input*/ 7, /*output*/ 7)],
    );
}

fn render_status(
    chat: &mut ChatWidget,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) -> String {
    chat.dispatch_command(SlashCommand::Status);
    match rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => {
            lines_to_single_string(&cell.display_lines(/*width*/ 100))
        }
        other => panic!("expected status output, got {other:?}"),
    }
}

/// PF-84: with named accounts off, `/status` is byte-for-byte what it was,
/// whatever usage the threads reported.
#[tokio::test]
async fn status_account_usage_flag_off_leaves_status_unchanged() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    zai_session_with_account_usage(&mut chat, /*named_accounts*/ false);
    let with_reports = render_status(&mut chat, &mut rx);
    chat.session_account_usage.clear();
    let without_reports = render_status(&mut chat, &mut rx);

    pretty_assertions::assert_eq!(with_reports, without_reports);
    assert!(!with_reports.contains("Account usage"), "{with_reports}");
}

/// PF-84: a spawned agent on another account counts toward that account; the
/// session's account is marked; another provider's accounts are left out.
#[tokio::test]
async fn status_account_usage_lists_the_active_providers_accounts() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    zai_session_with_account_usage(&mut chat, /*named_accounts*/ true);
    let rendered = render_status(&mut chat, &mut rx);

    assert!(rendered.contains("Account usage:"), "{rendered}");
    assert!(
        rendered.contains("default (active)  1,000     100"),
        "{rendered}"
    );
    assert!(
        rendered.contains("work                400      40"),
        "{rendered}"
    );
    assert!(!rendered.contains("alt"), "{rendered}");
    assert!(rendered.contains("· account default"), "{rendered}");

    // A resumed thread or a fork runs on the account the app server
    // reported; the provider line names that account too (#555).
    chat.thread_provider_account = Some("zai:work".to_string());
    let rendered = render_status(&mut chat, &mut rx);
    assert!(rendered.contains("work (active)"), "{rendered}");
    assert!(rendered.contains("· account work"), "{rendered}");
}

/// #1193: `/status` decides whether to name the account from the answer the
/// background check cached, never from the vault: each step below renders
/// the opposite of what reading the vault would give. Until the answer is
/// known the account is shown rather than hidden.
#[tokio::test]
async fn status_reads_the_cached_named_account_answer_not_the_vault() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    zai_session_with_account_usage(&mut chat, /*named_accounts*/ true);
    // Only `default` has usage, so the session's own rows say nothing.
    chat.session_account_usage.clear();
    let vault = codex_vault::Vault::new(chat.config.codex_home.to_path_buf());
    let add = |provider_id: &str, name: &str| {
        vault
            .create_provider_account(
                provider_id,
                &codex_vault::ProviderAccountName::parse(name).expect("account name"),
                codex_vault::ProviderAccountKind::ApiKey,
                "test-key-not-a-secret",
            )
            .expect("add named account");
    };
    let shows_account = |rendered: &str| {
        let named = rendered.contains("Z.AI · account default");
        assert_eq!(named, rendered.contains("Account usage:"), "{rendered}");
        named
    };

    // Each `/status` also starts a background check; answers set here are
    // numbered above those, so the checks don't change what is rendered.
    const MANUAL: u64 = 1 << 40;

    // Not known yet; the vault holds no Z.AI account.
    add("kimi", "alt");
    assert!(shows_account(&render_status(&mut chat, &mut rx)));

    // Known: no Z.AI account; the vault now holds one.
    add("zai", "work");
    chat.model_catalog.set_named_account_providers(
        /*generation*/ MANUAL + 1,
        Some(["kimi".to_string()].into()),
    );
    assert!(!shows_account(&render_status(&mut chat, &mut rx)));

    // Known: a Z.AI account. An older check answering late changes nothing.
    chat.model_catalog.set_named_account_providers(
        /*generation*/ MANUAL + 3,
        Some(["zai".to_string()].into()),
    );
    chat.model_catalog
        .set_named_account_providers(/*generation*/ MANUAL + 2, Some(Default::default()));
    assert!(shows_account(&render_status(&mut chat, &mut rx)));

    // The vault could not be read: unknown again, so shown.
    chat.model_catalog
        .set_named_account_providers(/*generation*/ MANUAL + 4, /*providers*/ None);
    assert!(shows_account(&render_status(&mut chat, &mut rx)));
}

/// #1193: each `/status` refreshes the cached answer in the background, so
/// an account added by another process shows on the next one.
#[tokio::test]
async fn status_refreshes_the_cached_named_account_answer_in_the_background() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    zai_session_with_account_usage(&mut chat, /*named_accounts*/ true);
    chat.session_account_usage.clear();
    chat.model_catalog
        .set_named_account_providers(/*generation*/ 1, Some(Default::default()));
    assert!(!render_status(&mut chat, &mut rx).contains("· account default"));

    codex_vault::Vault::new(chat.config.codex_home.to_path_buf())
        .create_provider_account(
            "zai",
            &codex_vault::ProviderAccountName::parse("work").expect("account name"),
            codex_vault::ProviderAccountKind::ApiKey,
            "test-key-not-a-secret",
        )
        .expect("add named account");
    let _ = render_status(&mut chat, &mut rx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while chat.model_catalog.provider_has_named_account("zai") != Some(true) {
        assert!(
            std::time::Instant::now() < deadline,
            "no background refresh"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(render_status(&mut chat, &mut rx).contains("· account default"));
}

/// #1251: an OpenAI session on the named ChatGPT account `work`, whose
/// vault sign-in is on the `plus` plan.
fn named_sign_in_session(chat: &mut ChatWidget) -> codex_login::NamedChatgptAccount {
    app_test_support::write_named_chatgpt_auth(
        chat.config.codex_home.as_path(),
        "openai",
        "work",
        app_test_support::ChatGptAuthFixture::new("work-access-not-a-secret")
            .account_id("acct-work")
            .plan_type("plus"),
    )
    .expect("named sign-in");
    chat.config.model_provider_id = "openai".to_string();
    chat.config.model_provider =
        codex_model_provider_info::ModelProviderInfo::create_openai_provider(
            /*base_url*/ None,
        );
    chat.config.model_provider.account = Some(codex_model_provider_info::NamedProviderAccount {
        provider_id: "openai".to_string(),
        name: "work".to_string(),
    });
    chat.config
        .features
        .enable(codex_features::Feature::NamedAccounts)
        .expect("enable named accounts");
    codex_login::NamedChatgptAccount {
        provider_id: "openai".to_string(),
        name: "work".to_string(),
    }
}

/// #1251: `/status` describes a named sign-in account from the summary the
/// background check cached, never from the vault: each step below renders
/// something the vault (a `plus` sign-in) would not give. Until the summary
/// is known the account is named alone.
#[tokio::test]
async fn status_reads_the_cached_named_sign_in_summary_not_the_vault() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let account = named_sign_in_session(&mut chat);
    // Each `/status` also starts a background check; answers set here are
    // numbered above those, so the checks don't change what is rendered.
    const MANUAL: u64 = 1 << 40;

    // Not known yet: the account alone.
    let rendered = render_status(&mut chat, &mut rx);
    assert!(rendered.contains("account work"), "{rendered}");
    assert!(!rendered.contains("ChatGPT sign-in"), "{rendered}");
    assert!(!rendered.contains("no sign-in"), "{rendered}");

    // Known: signed out (D171), although the vault holds a sign-in.
    chat.model_catalog.set_named_sign_in(
        /*generation*/ MANUAL + 1,
        account.clone(),
        crate::provider_named_accounts::NamedSignInState::NoSignIn,
    );
    let rendered = render_status(&mut chat, &mut rx);
    assert!(
        rendered.contains("account work has no sign-in"),
        "{rendered}"
    );
    assert!(rendered.contains("account unavailable"), "{rendered}");

    // The vault could not be read.
    chat.model_catalog.set_named_sign_in(
        /*generation*/ MANUAL + 2,
        account.clone(),
        crate::provider_named_accounts::NamedSignInState::Unreadable,
    );
    let rendered = render_status(&mut chat, &mut rx);
    assert!(
        rendered.contains("account work is not signed in"),
        "{rendered}"
    );
    assert!(!rendered.contains("account unavailable"), "{rendered}");

    // Known: a `pro` sign-in. An older check answering late changes nothing.
    chat.model_catalog.set_named_sign_in(
        /*generation*/ MANUAL + 4,
        account.clone(),
        crate::provider_named_accounts::NamedSignInState::SignedIn(
            codex_login::NamedSignInSummary {
                kind: "ChatGPT sign-in",
                email_hint: None,
                plan: Some("pro".to_string()),
            },
        ),
    );
    chat.model_catalog.set_named_sign_in(
        /*generation*/ MANUAL + 3,
        account,
        crate::provider_named_accounts::NamedSignInState::NoSignIn,
    );
    let rendered = render_status(&mut chat, &mut rx);
    assert!(
        rendered.contains("ChatGPT sign-in · pro (account work)"),
        "{rendered}"
    );
    assert!(!rendered.to_lowercase().contains("plus"), "{rendered}");

    // Another account's answer does not describe this one.
    let mut other = chat.config.model_provider.clone();
    other.account = Some(codex_model_provider_info::NamedProviderAccount {
        provider_id: "openai".to_string(),
        name: "home".to_string(),
    });
    chat.config.model_provider = other;
    let rendered = render_status(&mut chat, &mut rx);
    assert!(rendered.contains("account home"), "{rendered}");
    assert!(!rendered.contains("ChatGPT sign-in"), "{rendered}");
}

/// #1251: each `/status` refreshes the cached summary off the UI thread, so
/// a sign-in made by another process shows on the next one.
#[tokio::test]
async fn status_named_sign_in_summary_is_filled_in_the_background() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let account = named_sign_in_session(&mut chat);
    let _ = render_status(&mut chat, &mut rx);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while chat.model_catalog.named_sign_in(&account).is_none() {
        assert!(std::time::Instant::now() < deadline, "no background check");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let rendered = render_status(&mut chat, &mut rx);
    assert!(rendered.contains("ChatGPT sign-in"), "{rendered}");
    assert!(rendered.contains("(account work)"), "{rendered}");
}
