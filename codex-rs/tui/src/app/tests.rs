//! App-level orchestration tests for the TUI.

async fn accounting_fixture(
    app: &mut App,
    path: &std::path::Path,
) -> anyhow::Result<(ThreadId, i64)> {
    accounting_fixture_at(app, path, chrono::Utc::now().timestamp_millis()).await
}

/// `accounting_fixture`, with the ledger written at `now` rather than the clock.
async fn accounting_fixture_at(
    app: &mut App,
    path: &std::path::Path,
    now: i64,
) -> anyhow::Result<(ThreadId, i64)> {
    use codex_state::accounting::*;
    let owner = ThreadId::new();
    let db = codex_state::StateRuntime::init(
        codex_state::SqliteConfig::from_sqlite_home(path.abs()),
        "synthetic".into(),
    )
    .await?;
    let metadata = codex_state::ThreadMetadataBuilder::new(
        owner,
        path.join("synthetic.jsonl"),
        chrono::Utc::now(),
        codex_protocol::protocol::SessionSource::Cli,
    );
    db.upsert_thread(&metadata.build("synthetic")).await?;
    let a: Attempt = serde_json::from_value(serde_json::json!({
        "attempt_id":uuid::Uuid::from_u128(1),"request_id":uuid::Uuid::from_u128(2),
        "thread_id":owner,"turn":"synthetic","retry_of":null,"provider":"fixture-only",
        "model":"fixture-model","scope":uuid::Uuid::nil(),"dialect":"Inclusive","dispatched_at_ms":now
    }))?;
    let s: Snapshot = serde_json::from_value(serde_json::json!({
        "id":uuid::Uuid::from_u128(3),"provider":"fixture-only","model":"fixture-model",
        "scope":uuid::Uuid::nil(),"currency":"USD","unit":"PerMillionTokens",
        "rates":{"noncached":"1","read":"1","write":"2","output":"4"},
        "source_reference":uuid::Uuid::from_u128(4),"source_kind":"ProviderPublished",
        "observed_at_ms":0,"approved_at_ms":0,"effective_from_ms":0,"effective_end_ms":null
    }))?;
    let o: Observation = serde_json::from_value(serde_json::json!({
        "revision":1,"source":uuid::Uuid::from_u128(5),"sequence":1,
        "patch":{"input":100,"read":20,"output":40}
    }))?;
    let store = AccountingStore::open(&db, now).await?;
    store.admit(owner, &a, &[s], now).await?;
    store.observe(owner, &a, &[o], now).await?;
    app.chat_widget
        .handle_thread_session_quiet(test_thread_session(owner, path.to_path_buf()));
    app.active_thread_id = Some(owner);
    app.state_db = Some(db);
    Ok((owner, now / 86_400_000))
}

async fn accounting_load(
    app: &mut App,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    day: i64,
) -> AppEvent {
    while rx.try_recv().is_ok() {}
    app.handle_accounting_inspector_event(AppEvent::OpenAccountingInspector { day });
    let load = rx.recv().await.unwrap();
    assert!(matches!(load, AppEvent::LoadAccountingInspector { .. }));
    app.handle_accounting_inspector_event(load);
    tokio::time::timeout(Duration::from_secs(20), rx.recv())
        .await
        .unwrap()
        .unwrap()
}

fn accounting_scroll(app: &mut App) -> String {
    app.chat_widget.on_terminal_resize(/*width*/ 40);
    let mut output = String::new();
    for _ in 0..120 {
        output.push_str(&render_bottom_popup(&app.chat_widget, /*width*/ 40));
        app.chat_widget
            .handle_key_event(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Down,
                crossterm::event::KeyModifiers::NONE,
            ));
    }
    output
}

#[tokio::test]
async fn accounting_inspect_app_real_store_to_view() -> anyhow::Result<()> {
    use codex_state::accounting::InspectionDay;
    let path = tempdir()?;
    let (mut app, mut rx, mut ops) = make_test_app_with_channels().await;
    let (_, day) = accounting_fixture(&mut app, path.path()).await?;
    while ops.try_recv().is_ok() {}
    let event = accounting_load(&mut app, &mut rx, day).await;
    let generation = match &event {
        AppEvent::AccountingInspectorLoaded {
            result: Ok(InspectionDay::Ready(v)),
            generation,
            ..
        } => {
            assert_eq!(
                serde_json::to_value(v.totals.known_usd)?,
                serde_json::json!("0.00018")
            );
            assert_eq!(v.totals.unknown_estimates, 1);
            assert_eq!(v.requests.len(), 1);
            *generation
        }
        other => panic!("{other:?}"),
    };
    app.handle_accounting_inspector_event(event);
    let summary = accounting_scroll(&mut app);
    assert!(
        summary.contains("$0.000180") && summary.contains("+ unknown") && summary.contains("costs")
    );
    for page in [1, 2] {
        app.handle_accounting_inspector_event(AppEvent::NavigateAccountingInspector {
            generation,
            page,
        });
    }
    let detail = accounting_scroll(&mut app);
    for required in [
        "Cache write: unknown",
        "fixture-only",
        "fixture-model",
        "0.00018",
        "Price ID:",
    ] {
        assert!(detail.contains(required), "{required}");
    }
    assert!(ops.try_recv().is_err());
    assert!(rx.try_recv().is_err());
    app.state_db.as_ref().unwrap().close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_range_app_dispatch_preserves_query() -> anyhow::Result<()> {
    use codex_state::accounting::InspectionDay;
    use codex_state::accounting::InspectionGrouping;
    use codex_state::accounting::InspectionRange;
    let path = tempdir()?;
    let (mut app, mut rx, _ops) = make_test_app_with_channels().await;
    let started = chrono::Utc::now().timestamp_millis();
    let (_, day) = accounting_fixture(&mut app, path.path()).await?;
    while rx.try_recv().is_ok() {}
    app.handle_accounting_inspector_event(AppEvent::OpenAccountingInspector { day });
    let mut load = rx.recv().await.unwrap();
    let requested = InspectionRange {
        start_ms: day * 86_400_000,
        end_ms: (day + 1) * 86_400_000,
        grouping: InspectionGrouping::Hour,
    };
    let AppEvent::LoadAccountingInspector { range, .. } = &mut load else {
        panic!()
    };
    *range = Some(requested);
    app.handle_accounting_inspector_event(load);
    let event = tokio::time::timeout(Duration::from_secs(20), rx.recv())
        .await?
        .unwrap();
    assert!(matches!(&event, AppEvent::AccountingInspectorLoaded {
        result: Ok(InspectionDay::Range { requested: actual, buckets, .. }), ..
    } if *actual == requested && buckets.len() == 24));
    app.handle_accounting_inspector_event(event);
    let text = accounting_scroll(&mut app);
    assert!(text.contains("timezone: UTC"));
    // Today's hours: the current one is in progress and later ones have not
    // started, so the total so far is shown (#289 R2). Unless the hour turned
    // while the test ran, which leaves the request's hour partial.
    let hour = |ms: i64| ms / 3_600_000;
    if hour(started) == hour(chrono::Utc::now().timestamp_millis()) {
        assert!(text.contains("In progress — totals so far"), "{text}");
        assert!(!text.contains("Range total unavailable"), "{text}");
    }
    app.state_db.as_ref().unwrap().close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_app_remote_does_not_read_local() -> anyhow::Result<()> {
    let path = tempdir()?;
    let (mut app, mut rx, _ops) = make_test_app_with_channels().await;
    let (_, day) = accounting_fixture(&mut app, path.path()).await?;
    app.app_server_target = crate::AppServerTarget::Remote {
        endpoint: crate::RemoteAppServerEndpoint::UnixSocket {
            socket_path: path.path().join("missing.sock").abs(),
        },
    };
    let event = accounting_load(&mut app, &mut rx, day).await;
    assert!(
        matches!(&event, AppEvent::AccountingInspectorLoaded { result: Err(message), .. } if message.contains("remote") && message.contains("Next step: "))
    );
    app.handle_accounting_inspector_event(event);
    let screen = accounting_scroll(&mut app);
    assert!(!screen.contains("0.000180"));
    assert!(!screen.contains("fixture-only"));
    app.state_db.as_ref().unwrap().close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_app_thread_switch_stale_reply() -> anyhow::Result<()> {
    let path = tempdir()?;
    let (mut app, mut rx, _ops) = make_test_app_with_channels().await;
    let (_, day) = accounting_fixture(&mut app, path.path()).await?;
    let held_result = accounting_load(&mut app, &mut rx, day).await;
    let other = ThreadId::new();
    app.chat_widget
        .handle_thread_session_quiet(test_thread_session(other, path.path().to_path_buf()));
    app.active_thread_id = Some(other);
    app.handle_accounting_inspector_event(held_result);
    assert!(
        !render_bottom_popup(&app.chat_widget, /*width*/ 80).contains("Cost — this conversation")
    );
    let next = accounting_load(&mut app, &mut rx, day).await;
    assert!(matches!(next, AppEvent::AccountingInspectorLoaded {
        thread: Some(id), result: Ok(codex_state::accounting::InspectionDay::MissingThread), ..
    } if id == other));
    app.state_db.as_ref().unwrap().close().await;
    Ok(())
}

/// #1518: a ledger written while the clock read ahead (here, three hours) is
/// intact once the clock is set right, yet newer than it. `/cost` names the
/// clock rather than calling the evidence corrupt.
#[tokio::test]
async fn accounting_inspect_app_clock_behind_ledger_names_the_clock() -> anyhow::Result<()> {
    let path = tempdir()?;
    let (mut app, mut rx, _ops) = make_test_app_with_channels().await;
    let ahead = chrono::Utc::now().timestamp_millis() + 3 * 3_600_000;
    accounting_fixture_at(&mut app, path.path(), ahead).await?;
    let day = chrono::Utc::now().timestamp_millis() / 86_400_000;
    let loaded = accounting_load(&mut app, &mut rx, day).await;
    let AppEvent::AccountingInspectorLoaded {
        result: Err(message),
        ..
    } = &loaded
    else {
        panic!("expected a refused read, got {loaded:?}");
    };
    assert_eq!(
        message,
        &super::event_dispatch::clock_behind_ledger_message(3 * 3_600_000)
    );
    app.handle_accounting_inspector_event(loaded);
    let rendered = render_bottom_popup(&app.chat_widget, /*width*/ 80);
    assert!(
        rendered.contains("your system clock is behind the newest recorded usage"),
        "{rendered}"
    );
    assert!(!rendered.contains("corrupt"), "{rendered}");
    app.state_db.as_ref().unwrap().close().await;
    Ok(())
}

/// #1522: a view whose UTC day starts after the clock (opened while the clock
/// read ahead, refreshed after it was set back) names the clock, not damage.
#[tokio::test]
async fn accounting_inspect_app_day_after_clock_names_the_clock() -> anyhow::Result<()> {
    let path = tempdir()?;
    let (mut app, mut rx, _ops) = make_test_app_with_channels().await;
    accounting_fixture(&mut app, path.path()).await?;
    // Two days ahead, so the test holds even if it runs across UTC midnight.
    let ahead = chrono::Utc::now().timestamp_millis() / 86_400_000 + 2;
    let loaded = accounting_load(&mut app, &mut rx, ahead).await;
    let AppEvent::AccountingInspectorLoaded {
        result: Err(message),
        ..
    } = &loaded
    else {
        panic!("expected a refused read, got {loaded:?}");
    };
    assert!(
        message.starts_with(
            "Unavailable — your system clock reads earlier than the day this view shows ("
        ),
        "{message}"
    );
    app.handle_accounting_inspector_event(loaded);
    let rendered = render_bottom_popup(&app.chat_widget, /*width*/ 80);
    assert!(
        rendered.contains("your system clock reads earlier"),
        "{rendered}"
    );
    assert!(!rendered.contains("corrupt"), "{rendered}");
    app.state_db.as_ref().unwrap().close().await;
    Ok(())
}

#[test]
fn accounting_clock_before_day_message_states_the_lag() {
    let rendered = [30_000, 3 * 3_600_000 - 5_000, 6 * 86_400_000]
        .map(super::event_dispatch::clock_before_day_message)
        .join("\n")
        // The command name depends on the build's features.
        .replace(crate::chatwidget::cost_command(), "<cost command>");
    insta::assert_snapshot!("accounting_clock_before_day_messages", rendered);
}

#[test]
fn accounting_clock_behind_message_states_the_lag() {
    let rendered = [
        30_000,
        89_000,
        58 * 60_000,
        3 * 3_600_000 - 5_000,
        47 * 3_600_000,
        6 * 86_400_000,
    ]
    .map(super::event_dispatch::clock_behind_ledger_message)
    .join("\n");
    insta::assert_snapshot!("accounting_clock_behind_ledger_messages", rendered);
}

#[tokio::test]
async fn accounting_inspect_app_error_retry_and_timeout() -> anyhow::Result<()> {
    let path = tempdir()?;
    let (mut app, mut rx, _ops) = make_test_app_with_channels().await;
    let (_, day) = accounting_fixture(&mut app, path.path()).await?;
    let db = app.state_db.take().unwrap();
    let missing = accounting_load(&mut app, &mut rx, day).await;
    assert!(
        matches!(&missing, AppEvent::AccountingInspectorLoaded { result: Err(s), .. } if s.contains("database is not open"))
    );
    app.handle_accounting_inspector_event(missing);
    app.state_db = Some(db.clone());
    let event = accounting_load(&mut app, &mut rx, day).await;
    let AppEvent::AccountingInspectorLoaded { generation, .. } = &event else {
        panic!()
    };
    let generation = *generation;
    app.handle_accounting_inspector_event(event);
    tokio::time::pause();
    let timeout =
        super::event_dispatch::accounting_inspector_read_result(std::future::pending()).await;
    tokio::time::resume();
    assert!(timeout.unwrap_err().contains("timed out"));
    app.handle_accounting_inspector_event(AppEvent::RefreshAccountingInspector { generation });
    let AppEvent::LoadAccountingInspector {
        generation: next, ..
    } = rx.recv().await.unwrap()
    else {
        panic!()
    };
    assert_ne!(generation, next);
    db.close().await;
    let failed = accounting_load(&mut app, &mut rx, day).await;
    assert!(
        matches!(&failed, AppEvent::AccountingInspectorLoaded { result: Err(s), .. } if s.contains("could not be read") && !s.contains("SELECT"))
    );
    app.handle_accounting_inspector_event(failed);
    app.chat_widget
        .handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        ));
    app.handle_accounting_inspector_event(rx.recv().await.unwrap());
    assert!(
        !render_bottom_popup(&app.chat_widget, /*width*/ 80).contains("Cost — this conversation")
    );
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_app_restart_and_profile_isolation() -> anyhow::Result<()> {
    let path = tempdir()?;
    let fresh = tempdir()?;
    let (mut app, mut rx, _ops) = make_test_app_with_channels().await;
    let (owner, day) = accounting_fixture(&mut app, path.path()).await?;
    let before = accounting_load(&mut app, &mut rx, day).await;
    app.handle_accounting_inspector_event(before);
    app.state_db.take().unwrap().close().await;
    app.chat_widget.clear_pending_token_activity_refreshes();
    app.state_db = Some(
        codex_state::StateRuntime::init(
            codex_state::SqliteConfig::from_sqlite_home(path.path().abs()),
            "synthetic".into(),
        )
        .await?,
    );
    let reopened = accounting_load(&mut app, &mut rx, day).await;
    assert!(
        matches!(reopened, AppEvent::AccountingInspectorLoaded { result: Ok(codex_state::accounting::InspectionDay::Ready(v)), .. } if v.owner == owner && v.totals.attempts == 1)
    );
    app.state_db.take().unwrap().close().await;
    app.chat_widget.update_account_state(
        /*status_account_display*/ None, /*plan_type*/ None,
        /*has_chatgpt_account*/ false, /*has_codex_backend_auth*/ false,
    );
    app.state_db = Some(
        codex_state::StateRuntime::init(
            codex_state::SqliteConfig::from_sqlite_home(fresh.path().abs()),
            "synthetic".into(),
        )
        .await?,
    );
    let empty = accounting_load(&mut app, &mut rx, day).await;
    assert!(matches!(
        &empty,
        AppEvent::AccountingInspectorLoaded {
            result: Ok(codex_state::accounting::InspectionDay::Absent),
            ..
        }
    ));
    app.handle_accounting_inspector_event(empty);
    assert!(!accounting_scroll(&mut app).contains("0.000180"));
    app.state_db.take().unwrap().close().await;
    Ok(())
}

#[tokio::test]
async fn accounting_inspect_app_off_and_old_usage_routes() -> anyhow::Result<()> {
    let (mut app, mut rx, mut ops) = make_test_app_with_channels().await;
    let event = accounting_load(&mut app, &mut rx, /*day*/ 0).await;
    assert!(
        matches!(&event, AppEvent::AccountingInspectorLoaded { result: Err(s), .. } if s.contains("no current native thread"))
    );
    app.handle_accounting_inspector_event(event);
    assert!(ops.try_recv().is_err());
    assert!(rx.try_recv().is_err());
    app.chat_widget.clear_pending_token_activity_refreshes();
    set_chatgpt_auth(&mut app.chat_widget);
    app.chat_widget.handle_paste("/usage".into());
    app.chat_widget
        .handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
    app.chat_widget
        .handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
    assert!(matches!(rx.recv().await, Some(AppEvent::OpenTokenActivity)));
    Ok(())
}

#[path = "tests/advanced_reasoning_tests.rs"]
mod advanced_reasoning_tests;
mod dispatch_integration;
mod issue_613;
mod issue_646;
mod issue_660;
mod model_catalog;
mod plugin_catalog;
mod rate_limits;
mod safety_buffering;
#[path = "tests/session_lifecycle_requests.rs"]
mod session_lifecycle_requests;
mod session_summary;
mod startup;
#[path = "tests/turn_submission.rs"]
mod turn_submission;

use super::*;
use crate::app_backtrack::BacktrackSelection;
use crate::app_backtrack::BacktrackState;
use crate::app_backtrack::user_count;
use crate::app_event::HistoryBatchEntryResponse;
use codex_utils_absolute_path::test_support::PathExt;

use crate::chatwidget::ChatWidgetInit;
use crate::chatwidget::create_initial_user_message;
use crate::chatwidget::tests::helpers::render_bottom_popup;
use crate::chatwidget::tests::helpers::set_active_cell;
use crate::chatwidget::tests::make_chatwidget_manual_with_sender;
use crate::chatwidget::tests::set_chatgpt_auth;
use crate::chatwidget::tests::set_fast_mode_test_catalog;
use crate::claude_panes::CODEX_MAIN_PANE_ID;
use crate::file_search::FileSearchManager;
use crate::goal_files;
use crate::history_cell::AgentMarkdownCell;
use crate::history_cell::AgentMessageCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::PlainHistoryCell;
use crate::history_cell::UserHistoryCell;
use crate::history_cell::new_session_info;
use crate::multi_agents::AgentPickerThreadEntry;
use crate::multi_agents::SubAgentActivityDisplay;
use crate::spawn_orchestration::thread_node_id;
use crate::status::StatusAccountDisplay;
use assert_matches::assert_matches;

use crate::app_command::AppCommand as Op;
use crate::app_event::ConsolidationScrollbackReflow;
use crate::diff_model::FileChange;
use crate::legacy_core::config::ConfigBuilder;
use crate::legacy_core::config::ConfigOverrides;
use crate::legacy_core::config::PermissionProfileSnapshot;
use crate::legacy_core::config::TerminalResizeReflowMaxRows;
use codex_app_server_client::AppServerPath;
use codex_app_server_protocol::AdditionalContextKind;
use codex_app_server_protocol::AdditionalFileSystemPermissions;
use codex_app_server_protocol::AdditionalNetworkPermissions;
use codex_app_server_protocol::AdditionalPermissionProfile;
use codex_app_server_protocol::AgentMessageDeltaNotification;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::CodexErrorInfo as AppServerCodexErrorInfo;
use codex_app_server_protocol::CommandExecutionRequestApprovalParams;
use codex_app_server_protocol::FileChangeRequestApprovalParams;
use codex_app_server_protocol::FileUpdateChange;
use codex_app_server_protocol::ItemCompletedNotification;
use codex_app_server_protocol::ItemStartedNotification;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::McpServerElicitationRequest;
use codex_app_server_protocol::McpServerElicitationRequestParams;
use codex_app_server_protocol::McpServerStartupState;
use codex_app_server_protocol::McpServerStatusUpdatedNotification;
use codex_app_server_protocol::NetworkApprovalContext as AppServerNetworkApprovalContext;
use codex_app_server_protocol::NetworkApprovalProtocol as AppServerNetworkApprovalProtocol;
use codex_app_server_protocol::NetworkPolicyAmendment as AppServerNetworkPolicyAmendment;
use codex_app_server_protocol::NetworkPolicyRuleAction as AppServerNetworkPolicyRuleAction;
use codex_app_server_protocol::NonSteerableTurnKind as AppServerNonSteerableTurnKind;
use codex_app_server_protocol::PatchChangeKind;
use codex_app_server_protocol::PermissionsRequestApprovalParams;
use codex_app_server_protocol::RequestId as AppServerRequestId;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadClosedNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadSettings;
use codex_app_server_protocol::ThreadSettingsUpdatedNotification;
use codex_app_server_protocol::ThreadStartedNotification;
use codex_app_server_protocol::ThreadTokenUsage;
use codex_app_server_protocol::ThreadTokenUsageUpdatedNotification;
use codex_app_server_protocol::TokenUsageBreakdown;
use codex_app_server_protocol::ToolRequestUserInputParams;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnError as AppServerTurnError;
use codex_app_server_protocol::TurnStartedNotification;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput;
use codex_app_server_protocol::UserInput as AppServerUserInput;
use codex_app_server_protocol::WarningNotification;
use codex_model_provider_info::ANTHROPIC_PROVIDER_ID;
use codex_model_provider_info::CLAUDE_FABLE_5_PLAN_MODEL;
use codex_model_provider_info::CLAUDE_PLAN_PROVIDER_ID;
use codex_model_provider_info::KIMI_CODE_PROVIDER_ID;
use codex_model_provider_info::OPENAI_PROVIDER_ID;
use codex_model_provider_info::OPENROUTER_API_KEY_ENV_VAR;
use codex_model_provider_info::OPENROUTER_PROVIDER_ID;
use codex_model_provider_info::VERCEL_ANTHROPIC_FAST_PROVIDER_ID;
use codex_model_provider_info::VERCEL_API_KEY_ENV_VAR;
use codex_model_provider_info::VERCEL_GLM_5_2_FAST_MODEL;
use codex_model_provider_info::VERCEL_PROVIDER_ID;
use codex_model_provider_info::ZAI_API_KEY_ENV_VAR;
use codex_models_manager::test_support::construct_model_info_offline_for_tests;
use codex_models_manager::test_support::get_model_offline_for_tests;
use codex_otel::SessionTelemetry;
use codex_protocol::ThreadId;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::CollaborationModeMask;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Personality;
use codex_protocol::config_types::SandboxMode;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::config_types::Settings;
use codex_protocol::models::ActivePermissionProfile;
use codex_protocol::models::FileSystemPermissions;
use codex_protocol::models::NetworkPermissions;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::MAX_THREAD_GOAL_OBJECTIVE_CHARS;
use codex_protocol::protocol::MultiAgentVersion;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::SessionMeta;
use codex_protocol::protocol::SessionSource as RolloutSessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::protocol::TurnStartedEvent;
use codex_protocol::protocol::UserMessageEvent;
use codex_protocol::request_permissions::RequestPermissionProfile;
use codex_protocol::user_input::TextElement;
use codex_utils_absolute_path::AbsolutePathBuf;
use crossterm::event::KeyModifiers;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::prelude::Line;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tempfile::tempdir;
use tokio::time;

macro_rules! assert_app_snapshot {
    ($name:expr, $value:expr $(,)?) => {
        insta::with_settings!({snapshot_path => "../snapshots"}, {
            let normalized = crate::status::snapshot_helpers::normalize_snapshot_version(&$value);
            assert_snapshot!($name, normalized);
        });
    };
}

#[tokio::test]
async fn modal_tui_input_handoff_accounts_for_keys_and_returns_queue_to_chat() {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    let keys = [
        KeyCode::Char('f'),
        KeyCode::Down,
        KeyCode::Esc,
        KeyCode::Char('x'),
    ];
    let source =
        tokio_stream::iter(keys.map(|code| TuiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))));
    let mut drained = spawn_tui_event_drainer(Box::pin(source));
    {
        let mut modal = modal_tui_events(&mut drained.rx, &drained.watchdog);
        for expected in &keys[..3] {
            let event = time::timeout(Duration::from_secs(2), modal.next())
                .await
                .expect("modal receives input")
                .expect("key");
            assert!(matches!(event, TuiEvent::Key(key) if key.code == *expected));
        }
    }
    let event = drained
        .rx
        .recv()
        .await
        .expect("chat retains ownership after cancel");
    drained.watchdog.note_handled();
    assert!(matches!(event, TuiEvent::Key(key) if key.code == KeyCode::Char('x')));
    assert_eq!(drained.watchdog.pending_events.load(Ordering::Relaxed), 0);
    // Reopening after EOF does not create another terminal reader or hang.
    assert!(
        modal_tui_events(&mut drained.rx, &drained.watchdog)
            .next()
            .await
            .is_none()
    );
}

#[tokio::test]
async fn tui_event_drainer_keeps_polling_during_in_process_event_flood() {
    let terminal_events = (0..512).map(|_| {
        TuiEvent::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('x'),
            KeyModifiers::NONE,
        ))
    });
    let mut drained = spawn_tui_event_drainer(Box::pin(tokio_stream::iter(terminal_events)));
    let (event_tx, _event_rx) =
        tokio::sync::mpsc::unbounded_channel::<codex_app_server_client::InProcessServerEvent>();

    let flood_task = tokio::spawn(async move {
        for agent_index in 0..4 {
            for chunk_index in 0..2048 {
                event_tx
                    .send(
                        codex_app_server_client::InProcessServerEvent::ServerNotification(
                            Box::new(ServerNotification::AgentMessageDelta(
                                AgentMessageDeltaNotification {
                                    thread_id: format!("child-thread-{agent_index}"),
                                    turn_id: "turn".to_string(),
                                    item_id: format!("agent-item-{agent_index}"),
                                    delta: "streaming-output\n".to_string(),
                                },
                            )),
                        ),
                    )
                    .expect("simulated app-server event receiver should stay alive");
                if chunk_index % 256 == 0 {
                    tokio::task::yield_now().await;
                }
            }
        }
    });

    time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if drained.watchdog.pending_events.load(Ordering::Relaxed) == 512 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("terminal input drainer should not wait for app-server events to be consumed");

    for _ in 0..512 {
        drained
            .rx
            .recv()
            .await
            .expect("drained terminal event should be available");
        drained.watchdog.note_handled();
    }
    assert_eq!(drained.watchdog.pending_events.load(Ordering::Relaxed), 0);
    flood_task
        .await
        .expect("simulated app-server event flood should finish");
}

fn test_absolute_path(path: &str) -> AbsolutePathBuf {
    AbsolutePathBuf::try_from(PathBuf::from(path)).expect("absolute test path")
}

#[tokio::test]
async fn chat_widget_frame_reuses_active_cell_height_across_frame_passes() {
    #[derive(Debug)]
    struct CountingHistoryCell {
        desired_height_calls: Arc<AtomicUsize>,
    }

    impl HistoryCell for CountingHistoryCell {
        fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
            vec![Line::from("active cell")]
        }

        fn raw_lines(&self) -> Vec<Line<'static>> {
            vec![Line::from("active cell")]
        }

        fn desired_height(&self, _width: u16) -> u16 {
            self.desired_height_calls.fetch_add(1, Ordering::Relaxed);
            1
        }
    }

    let mut app = make_test_app().await;
    let desired_height_calls = Arc::new(AtomicUsize::new(0));
    set_active_cell(
        &mut app.chat_widget,
        Box::new(CountingHistoryCell {
            desired_height_calls: Arc::clone(&desired_height_calls),
        }),
    );
    let width = 80;
    app.with_chat_widget_frame(width, |desired_height, chat_widget| {
        let area = Rect::new(/*x*/ 0, /*y*/ 0, width, desired_height);
        let mut buffer = Buffer::empty(area);

        chat_widget.render(area, &mut buffer);
        assert!(chat_widget.cursor_pos(area).is_some());
        let _ = chat_widget.cursor_style(area);
    });

    assert_eq!(desired_height_calls.load(Ordering::Relaxed), 1);
}

async fn next_thread_settings_updated(
    app_server: &mut AppServerSession,
    thread_id: ThreadId,
) -> ThreadSettingsUpdatedNotification {
    for _ in 0..20 {
        let event = time::timeout(
            std::time::Duration::from_secs(/*secs*/ 2),
            app_server.next_event(),
        )
        .await
        .expect("app-server should emit an event")
        .expect("app-server event stream should remain open");
        if let codex_app_server_client::AppServerEvent::ServerNotification(notification) = event
            && let ServerNotification::ThreadSettingsUpdated(notification) = *notification
            && notification.thread_id == thread_id.to_string()
        {
            return notification;
        }
    }
    panic!("expected ThreadSettingsUpdated for thread {thread_id}");
}

#[tokio::test]
async fn handle_mcp_inventory_result_respects_origin_thread() {
    let mut app = make_test_app().await;
    app.transcript_cells
        .push(Arc::new(history_cell::new_mcp_inventory_loading(
            /*animations_enabled*/ false,
        )));

    app.handle_mcp_inventory_result(
        Ok(vec![McpServerStatus {
            name: "docs".to_string(),
            server_info: None,
            tools: HashMap::new(),
            resources: Vec::new(),
            resource_templates: Vec::new(),
            auth_status: codex_app_server_protocol::McpAuthStatus::Unsupported,
        }]),
        McpServerStatusDetail::ToolsAndAuthOnly,
        /*thread_id*/ None,
    );

    assert_eq!(app.transcript_cells.len(), 0);

    app.active_thread_id = Some(ThreadId::new());
    app.transcript_cells
        .push(Arc::new(history_cell::new_mcp_inventory_loading(
            /*animations_enabled*/ false,
        )));

    app.handle_mcp_inventory_result(
        Ok(Vec::new()),
        McpServerStatusDetail::ToolsAndAuthOnly,
        Some(ThreadId::new()),
    );

    assert_eq!(app.transcript_cells.len(), 1);
}

#[test]
fn bypass_hook_trust_startup_warning_snapshot() {
    let rendered = lines_to_single_string(
        &history_cell::new_warning_event(
            "`--dangerously-bypass-hook-trust` is enabled. Enabled hooks may run without review for this invocation."
                .to_string(),
        )
        .display_lines(/*width*/ 80),
    );

    assert_app_snapshot!("bypass_hook_trust_startup_warning", rendered);
}

#[test]
fn legacy_windows_sandbox_startup_warning_snapshot() {
    let rendered = lines_to_single_string(
        &history_cell::new_warning_event(
            codex_sandboxing::LEGACY_WINDOWS_SANDBOX_WARNING.to_string(),
        )
        .display_lines(/*width*/ 80),
    );

    assert_app_snapshot!("legacy_windows_sandbox_startup_warning", rendered);
}
#[tokio::test]
async fn enqueue_primary_thread_session_replays_buffered_approval_after_attach() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    app.pending_permission_confirmation = Some(test_support::pending_confirmation(thread_id));
    let approval_request =
        exec_approval_request(thread_id, "turn-1", "call-1", /*approval_id*/ None);

    assert_eq!(
        app.pending_app_server_requests
            .note_server_request(&approval_request),
        None
    );
    app.enqueue_primary_thread_request(approval_request).await?;
    app.enqueue_primary_thread_session(
        test_thread_session(thread_id, test_path_buf("/tmp/project")),
        Vec::new(),
    )
    .await?;

    let rx = app
        .active_thread_rx
        .as_mut()
        .expect("primary thread receiver should be active");
    let event = time::timeout(Duration::from_millis(50), rx.recv())
        .await
        .expect("timed out waiting for buffered approval event")
        .expect("channel closed unexpectedly");

    assert!(matches!(
        &event,
        ThreadBufferedEvent::Request(request)
            if matches!(
                request.as_ref(),
                ServerRequest::CommandExecutionRequestApproval { params, .. }
                    if params.turn_id == "turn-1"
            )
    ));

    app.handle_thread_event_now(event);
    // Enter confirms the highlighted "Yes, proceed".
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    while let Ok(app_event) = app_event_rx.try_recv() {
        if let AppEvent::SubmitThreadOp {
            thread_id: op_thread_id,
            ..
        } = app_event
        {
            assert_eq!(op_thread_id, thread_id);
            assert!(app.pending_permission_confirmation.is_some());
            return Ok(());
        }
    }

    panic!("expected approval action to submit a thread-scoped op");
}

#[tokio::test]
async fn resolved_buffered_approval_does_not_become_actionable_after_drain() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    let approval_request =
        exec_approval_request(thread_id, "turn-1", "call-1", /*approval_id*/ None);

    app.enqueue_primary_thread_session(
        test_thread_session(thread_id, test_path_buf("/tmp/project")),
        Vec::new(),
    )
    .await?;
    while app_event_rx.try_recv().is_ok() {}

    assert_eq!(
        app.pending_app_server_requests
            .note_server_request(&approval_request),
        None
    );
    app.enqueue_thread_request(thread_id, approval_request)
        .await?;

    let resolved = app
        .pending_app_server_requests
        .resolve_notification(&AppServerRequestId::Integer(1))
        .expect("matching app-server request should resolve");
    app.chat_widget.dismiss_app_server_request(&resolved);
    while app_event_rx.try_recv().is_ok() {}

    let rx = app
        .active_thread_rx
        .as_mut()
        .expect("primary thread receiver should be active");
    let event = time::timeout(Duration::from_millis(50), rx.recv())
        .await
        .expect("timed out waiting for buffered approval event")
        .expect("channel closed unexpectedly");

    assert!(matches!(
        &event,
        ThreadBufferedEvent::Request(request)
            if matches!(
                request.as_ref(),
                ServerRequest::CommandExecutionRequestApproval { params, .. }
                    if params.turn_id == "turn-1"
            )
    ));

    app.handle_thread_event_now(event);
    // Enter confirms the highlighted "Yes, proceed".
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    while let Ok(app_event) = app_event_rx.try_recv() {
        assert!(
            !matches!(app_event, AppEvent::SubmitThreadOp { .. }),
            "resolved buffered approval should not become actionable"
        );
    }

    Ok(())
}

#[tokio::test]
async fn enqueue_primary_thread_session_replays_turns_before_initial_prompt_submit() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    let initial_prompt = "follow-up after replay".to_string();
    let config = app.config.clone();
    let model = get_model_offline_for_tests(config.model.as_deref());
    app.chat_widget = ChatWidget::new_with_app_event(ChatWidgetInit {
        config,
        frame_requester: crate::tui::FrameRequester::test_dummy(),
        app_event_tx: app.app_event_tx.clone(),
        workspace_command_runner: None,
        initial_user_message: create_initial_user_message(
            Some(initial_prompt.clone()),
            Vec::new(),
            Vec::new(),
        ),
        enhanced_keys_supported: false,
        has_chatgpt_account: false,
        has_codex_backend_auth: false,
        model_catalog: app.model_catalog.clone(),
        feedback: codex_feedback::CodexFeedback::new(),
        is_first_run: false,
        status_account_display: None,
        runtime_model_provider_base_url: None,
        initial_plan_type: None,
        model: Some(model),
        startup_tooltip_override: None,
        status_line_invalid_items_warned: app.status_line_invalid_items_warned.clone(),
        terminal_title_invalid_items_warned: app.terminal_title_invalid_items_warned.clone(),
        session_telemetry: app.session_telemetry.clone(),
    });

    app.enqueue_primary_thread_session(
        test_thread_session(thread_id, test_path_buf("/tmp/project")),
        vec![test_turn(
            "turn-1",
            TurnStatus::Completed,
            vec![ThreadItem::UserMessage {
                id: "user-1".to_string(),
                client_id: None,
                content: vec![AppServerUserInput::Text {
                    text: "earlier prompt".to_string(),
                    text_elements: Vec::new(),
                }],
            }],
        )],
    )
    .await?;

    let mut saw_replayed_answer = false;
    let mut saw_optimistic_prompt = false;
    let mut submitted_items = None;
    while let Ok(event) = app_event_rx.try_recv() {
        match event {
            AppEvent::InsertHistoryCell(cell) => {
                let transcript = lines_to_single_string(&cell.transcript_lines(/*width*/ 80));
                saw_replayed_answer |= transcript.contains("earlier prompt");
                saw_optimistic_prompt |= transcript.contains(&initial_prompt);
            }
            AppEvent::SubmitThreadOp {
                thread_id: op_thread_id,
                op: Op::UserTurn { items, .. },
            } => {
                assert_eq!(op_thread_id, thread_id);
                submitted_items = Some(items);
            }
            AppEvent::CodexOp(Op::UserTurn { items, .. }) => {
                assert!(
                    saw_optimistic_prompt,
                    "expected optimistic prompt before turn submission"
                );
                submitted_items = Some(items);
            }
            _ => {}
        }
    }
    assert!(
        saw_replayed_answer,
        "expected replayed history before initial prompt submit"
    );
    assert_eq!(
        submitted_items,
        Some(vec![UserInput::Text {
            text: initial_prompt,
            text_elements: Vec::new(),
        }])
    );

    Ok(())
}

#[tokio::test]
async fn reset_thread_event_state_aborts_listener_tasks() {
    struct NotifyOnDrop(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }

    let mut app = make_test_app().await;
    let thread_id = ThreadId::new();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        let _notify_on_drop = NotifyOnDrop(Some(dropped_tx));
        let _ = started_tx.send(());
        std::future::pending::<()>().await;
    });
    app.thread_event_listener_tasks.insert(thread_id, handle);
    started_rx
        .await
        .expect("listener task should report it started");

    app.reset_thread_event_state();

    assert_eq!(app.thread_event_listener_tasks.is_empty(), true);
    time::timeout(Duration::from_millis(50), dropped_rx)
        .await
        .expect("timed out waiting for listener task abort")
        .expect("listener task drop notification should succeed");
}

#[tokio::test]
async fn history_lookup_response_is_routed_to_requesting_thread() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();

    app.lookup_message_history_entry(thread_id, /*offset*/ 0, /*log_id*/ 1)
        .await?;

    let app_event = tokio::time::timeout(Duration::from_secs(1), app_event_rx.recv())
        .await
        .expect("history lookup should emit an app event")
        .expect("app event channel should stay open");

    let AppEvent::ThreadHistoryEntryResponse {
        thread_id: routed_thread_id,
        event,
    } = app_event
    else {
        panic!("expected thread-routed history response");
    };
    assert_eq!(routed_thread_id, thread_id);
    assert_eq!(
        event,
        HistoryLookupResponse::Entry {
            offset: 0,
            log_id: 1,
            entry: None,
        }
    );

    let cursor = codex_message_history::HistoryBatchCursor::new(/*end_offset*/ 10);
    app.lookup_message_history_batch(thread_id, cursor, /*log_id*/ 1)
        .await?;
    let app_event = tokio::time::timeout(Duration::from_secs(1), app_event_rx.recv())
        .await
        .expect("history batch lookup should emit an app event")
        .expect("app event channel should stay open");
    let AppEvent::ThreadHistoryEntryResponse {
        thread_id: routed_thread_id,
        event,
    } = app_event
    else {
        panic!("expected thread-routed history batch response");
    };
    assert_eq!(routed_thread_id, thread_id);
    assert_eq!(
        event,
        HistoryLookupResponse::BatchError { cursor, log_id: 1 }
    );

    Ok(())
}

#[tokio::test]
async fn enqueue_thread_event_does_not_block_when_channel_full() -> Result<()> {
    let mut app = make_test_app().await;
    let thread_id = ThreadId::new();
    app.thread_event_channels
        .insert(thread_id, ThreadEventChannel::new(/*capacity*/ 1));
    app.set_thread_active(thread_id, /*active*/ true).await;

    let event = thread_closed_notification(thread_id);

    app.enqueue_thread_notification(thread_id, event.clone())
        .await?;
    time::timeout(
        Duration::from_millis(50),
        app.enqueue_thread_notification(thread_id, event),
    )
    .await
    .expect("enqueue_thread_notification blocked on a full channel")?;

    let mut rx = app
        .thread_event_channels
        .get_mut(&thread_id)
        .expect("missing thread channel")
        .receiver
        .take()
        .expect("missing receiver");

    time::timeout(Duration::from_millis(50), rx.recv())
        .await
        .expect("timed out waiting for first event")
        .expect("channel closed unexpectedly");
    time::timeout(Duration::from_millis(50), rx.recv())
        .await
        .expect("timed out waiting for second event")
        .expect("channel closed unexpectedly");

    Ok(())
}

#[tokio::test]
async fn active_history_batch_is_delivered_without_replay_buffering() -> Result<()> {
    let mut app = make_test_app().await;
    let thread_id = ThreadId::new();
    app.thread_event_channels
        .insert(thread_id, ThreadEventChannel::new(/*capacity*/ 4));
    app.set_thread_active(thread_id, /*active*/ true).await;

    let cursor = codex_message_history::HistoryBatchCursor::new(/*end_offset*/ 1);
    let event = HistoryLookupResponse::Batch {
        cursor,
        log_id: 1,
        entries: vec![HistoryBatchEntryResponse {
            offset: 1,
            entry: Some("history entry".to_string()),
        }],
        next_older_cursor: Some(codex_message_history::HistoryBatchCursor::new(
            /*end_offset*/ 0,
        )),
    };
    app.enqueue_thread_history_entry_response(thread_id, event.clone())
        .await?;

    let channel = app
        .thread_event_channels
        .get_mut(&thread_id)
        .expect("missing thread channel");
    assert!(channel.store.lock().await.buffer.is_empty());
    let mut receiver = channel.receiver.take().expect("missing receiver");
    let delivered = time::timeout(Duration::from_millis(50), receiver.recv())
        .await
        .expect("timed out waiting for history batch")
        .expect("missing history batch");
    let ThreadBufferedEvent::HistoryEntryResponse(delivered) = delivered else {
        panic!("expected history batch response");
    };
    assert_eq!(delivered, event);

    Ok(())
}

#[tokio::test]
async fn replay_thread_snapshot_restores_draft_and_queued_input() {
    let mut app = make_test_app().await;
    let thread_id = ThreadId::new();
    let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
    app.thread_event_channels.insert(
        thread_id,
        ThreadEventChannel::new_with_session(
            THREAD_EVENT_CHANNEL_CAPACITY,
            session.clone(),
            Vec::new(),
        ),
    );
    app.activate_thread_channel(thread_id).await;
    app.chat_widget.handle_thread_session(session.clone());

    app.chat_widget
        .apply_external_edit("draft prompt".to_string());
    app.chat_widget.submit_user_message_with_mode(
        "queued follow-up".to_string(),
        CollaborationModeMask {
            name: "Default".to_string(),
            mode: None,
            model: None,
            reasoning_effort: None,
            developer_instructions: None,
        },
    );
    let expected_input_state = app
        .chat_widget
        .capture_thread_input_state()
        .expect("expected thread input state");

    app.store_active_thread_receiver().await;

    let snapshot = {
        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("thread channel should exist");
        let store = channel.store.lock().await;
        assert_eq!(store.input_state, Some(expected_input_state));
        store.snapshot()
    };

    let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
        make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat_widget;

    app.replay_thread_snapshot(snapshot, /*resume_restored_queue*/ true);

    assert_eq!(app.chat_widget.composer_text_with_pending(), "draft prompt");
    assert!(app.chat_widget.queued_user_message_texts().is_empty());
    while let Ok(op) = new_op_rx.try_recv() {
        assert!(
            !matches!(op, Op::UserTurn { .. }),
            "draft-only replay should not auto-submit queued input"
        );
    }
}

#[tokio::test]
async fn replay_thread_snapshot_restores_the_matching_safety_buffer_prompt() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
    app.thread_event_channels.insert(
        thread_id,
        ThreadEventChannel::new_with_session(
            THREAD_EVENT_CHANNEL_CAPACITY,
            session.clone(),
            Vec::new(),
        ),
    );
    app.activate_thread_channel(thread_id).await;
    app.chat_widget.handle_thread_session(session);
    let default_mode = CollaborationModeMask {
        name: "Default".to_string(),
        mode: None,
        model: None,
        reasoning_effort: None,
        developer_instructions: None,
    };
    app.chat_widget
        .submit_user_message_with_mode("buffered prompt A".to_string(), default_mode.clone());
    let expected_input_state = app
        .chat_widget
        .capture_thread_input_state()
        .expect("expected thread input state");

    app.store_active_thread_receiver().await;
    let snapshot = {
        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("thread channel should exist");
        let store = channel.store.lock().await;
        assert_eq!(store.input_state, Some(expected_input_state.clone()));
        store.snapshot()
    };

    let (mut chat_widget, _app_event_tx, _rx, _op_rx) = make_chatwidget_manual_with_sender().await;
    chat_widget.handle_thread_session(test_thread_session(
        ThreadId::new(),
        test_path_buf("/tmp/other-project"),
    ));
    chat_widget.submit_user_message_with_mode("buffered prompt B".to_string(), default_mode);
    app.chat_widget = chat_widget;
    app.replay_thread_snapshot(snapshot, /*resume_restored_queue*/ false);

    assert_eq!(
        app.chat_widget.capture_thread_input_state(),
        Some(expected_input_state)
    );
}

#[tokio::test]
async fn active_turn_id_for_thread_uses_snapshot_turns() {
    let mut app = make_test_app().await;
    let thread_id = ThreadId::new();
    let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
    app.thread_event_channels.insert(
        thread_id,
        ThreadEventChannel::new_with_session(
            THREAD_EVENT_CHANNEL_CAPACITY,
            session,
            vec![test_turn("turn-1", TurnStatus::InProgress, Vec::new())],
        ),
    );

    assert_eq!(
        app.active_turn_id_for_thread(thread_id).await,
        Some("turn-1".to_string())
    );
}

#[tokio::test]
async fn active_turn_submission_clears_stale_visible_idle_turn_id() {
    let mut app = make_test_app().await;
    let thread_id = ThreadId::new();
    let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
    app.primary_thread_id = Some(thread_id);
    app.active_thread_id = Some(thread_id);
    app.thread_event_channels.insert(
        thread_id,
        ThreadEventChannel::new_with_session(
            THREAD_EVENT_CHANNEL_CAPACITY,
            session.clone(),
            vec![test_turn("turn-stale", TurnStatus::InProgress, Vec::new())],
        ),
    );
    app.chat_widget.handle_thread_session(session);
    assert!(
        !app.chat_widget.visible_task_running(),
        "repro requires the visible Main pane to be idle"
    );

    assert_eq!(app.active_turn_id_for_submission(thread_id).await, None);
    assert_eq!(app.active_turn_id_for_thread(thread_id).await, None);
}

#[tokio::test]
async fn active_turn_submission_keeps_inactive_thread_turn_id() {
    let mut app = make_test_app().await;
    let main_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    let child_session = test_thread_session(child_thread_id, test_path_buf("/tmp/project"));
    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.thread_event_channels.insert(
        child_thread_id,
        ThreadEventChannel::new_with_session(
            THREAD_EVENT_CHANNEL_CAPACITY,
            child_session,
            vec![test_turn("turn-child", TurnStatus::InProgress, Vec::new())],
        ),
    );

    assert_eq!(
        app.active_turn_id_for_submission(child_thread_id).await,
        Some("turn-child".to_string())
    );
    assert_eq!(
        app.active_turn_id_for_thread(child_thread_id).await,
        Some("turn-child".to_string())
    );
}

#[tokio::test]
async fn replayed_turn_complete_submits_restored_queued_follow_up() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
    app.chat_widget.handle_thread_session(session.clone());
    app.chat_widget.handle_server_notification(
        turn_started_notification(thread_id, "turn-1"),
        /*replay_kind*/ None,
    );
    app.chat_widget.handle_server_notification(
        agent_message_delta_notification(thread_id, "turn-1", "agent-1", "streaming"),
        /*replay_kind*/ None,
    );
    app.chat_widget
        .apply_external_edit("queued follow-up".to_string());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    let input_state = app
        .chat_widget
        .capture_thread_input_state()
        .expect("expected queued follow-up state");

    let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
        make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat_widget;
    app.chat_widget.handle_thread_session(session.clone());
    while new_op_rx.try_recv().is_ok() {}
    app.replay_thread_snapshot(
        ThreadEventSnapshot {
            session: None,
            turns: Vec::new(),
            events: vec![ThreadBufferedEvent::Notification(Box::new(
                turn_completed_notification(thread_id, "turn-1", TurnStatus::Completed),
            ))],
            input_state: Some(input_state),
        },
        /*resume_restored_queue*/ true,
    );

    match next_user_turn_op(&mut new_op_rx) {
        Op::UserTurn { items, .. } => assert_eq!(
            items,
            vec![UserInput::Text {
                text: "queued follow-up".to_string(),
                text_elements: Vec::new(),
            }]
        ),
        other => panic!("expected queued follow-up submission, got {other:?}"),
    }
}

#[tokio::test]
async fn replay_only_thread_keeps_restored_queue_visible() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
    app.chat_widget.handle_thread_session(session.clone());
    app.chat_widget.handle_server_notification(
        turn_started_notification(thread_id, "turn-1"),
        /*replay_kind*/ None,
    );
    app.chat_widget.handle_server_notification(
        agent_message_delta_notification(thread_id, "turn-1", "agent-1", "streaming"),
        /*replay_kind*/ None,
    );
    app.chat_widget
        .apply_external_edit("queued follow-up".to_string());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    let input_state = app
        .chat_widget
        .capture_thread_input_state()
        .expect("expected queued follow-up state");

    let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
        make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat_widget;
    app.chat_widget.handle_thread_session(session.clone());
    while new_op_rx.try_recv().is_ok() {}

    app.replay_thread_snapshot(
        ThreadEventSnapshot {
            session: None,
            turns: Vec::new(),
            events: vec![ThreadBufferedEvent::Notification(Box::new(
                turn_completed_notification(thread_id, "turn-1", TurnStatus::Completed),
            ))],
            input_state: Some(input_state),
        },
        /*resume_restored_queue*/ false,
    );

    assert_eq!(
        app.chat_widget.queued_user_message_texts(),
        vec!["queued follow-up".to_string()]
    );
    assert!(
        new_op_rx.try_recv().is_err(),
        "replay-only threads should not auto-submit restored queue"
    );
}

#[tokio::test]
async fn replay_thread_snapshot_keeps_queue_when_running_state_only_comes_from_snapshot() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
    app.chat_widget.handle_thread_session(session.clone());
    app.chat_widget.handle_server_notification(
        turn_started_notification(thread_id, "turn-1"),
        /*replay_kind*/ None,
    );
    app.chat_widget.handle_server_notification(
        agent_message_delta_notification(thread_id, "turn-1", "agent-1", "streaming"),
        /*replay_kind*/ None,
    );
    app.chat_widget
        .apply_external_edit("queued follow-up".to_string());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    let input_state = app
        .chat_widget
        .capture_thread_input_state()
        .expect("expected queued follow-up state");

    let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
        make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat_widget;
    app.chat_widget.handle_thread_session(session.clone());
    while new_op_rx.try_recv().is_ok() {}

    app.replay_thread_snapshot(
        ThreadEventSnapshot {
            session: None,
            turns: Vec::new(),
            events: vec![],
            input_state: Some(input_state),
        },
        /*resume_restored_queue*/ true,
    );

    assert_eq!(
        app.chat_widget.queued_user_message_texts(),
        vec!["queued follow-up".to_string()]
    );
    assert!(
        new_op_rx.try_recv().is_err(),
        "restored queue should stay queued when replay did not prove the turn finished"
    );
}

#[tokio::test]
async fn replay_thread_snapshot_in_progress_turn_restores_running_queue_state() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
    app.chat_widget.handle_thread_session(session.clone());
    app.chat_widget.handle_server_notification(
        turn_started_notification(thread_id, "turn-1"),
        /*replay_kind*/ None,
    );
    app.chat_widget.handle_server_notification(
        agent_message_delta_notification(thread_id, "turn-1", "agent-1", "streaming"),
        /*replay_kind*/ None,
    );
    app.chat_widget
        .apply_external_edit("queued follow-up".to_string());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    let input_state = app
        .chat_widget
        .capture_thread_input_state()
        .expect("expected queued follow-up state");

    let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
        make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat_widget;
    app.chat_widget.handle_thread_session(session.clone());
    while new_op_rx.try_recv().is_ok() {}

    app.replay_thread_snapshot(
        ThreadEventSnapshot {
            session: None,
            turns: vec![test_turn("turn-1", TurnStatus::InProgress, Vec::new())],
            events: Vec::new(),
            input_state: Some(input_state),
        },
        /*resume_restored_queue*/ true,
    );

    assert_eq!(
        app.chat_widget.queued_user_message_texts(),
        vec!["queued follow-up".to_string()]
    );
    assert!(
        new_op_rx.try_recv().is_err(),
        "restored queue should stay queued while replayed turn is still running"
    );
}

#[tokio::test]
async fn replay_thread_snapshot_in_progress_turn_restores_running_state_without_input_state() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
    let (chat_widget, _app_event_tx, _rx, _new_op_rx) = make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat_widget;
    app.chat_widget.handle_thread_session(session);

    app.replay_thread_snapshot(
        ThreadEventSnapshot {
            session: None,
            turns: vec![test_turn("turn-1", TurnStatus::InProgress, Vec::new())],
            events: Vec::new(),
            input_state: None,
        },
        /*resume_restored_queue*/ false,
    );

    assert!(app.chat_widget.is_task_running_for_test());
}

#[tokio::test]
async fn replay_thread_snapshot_does_not_submit_queue_before_replay_catches_up() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
    app.chat_widget.handle_thread_session(session.clone());
    app.chat_widget.handle_server_notification(
        turn_started_notification(thread_id, "turn-1"),
        /*replay_kind*/ None,
    );
    app.chat_widget.handle_server_notification(
        agent_message_delta_notification(thread_id, "turn-1", "agent-1", "streaming"),
        /*replay_kind*/ None,
    );
    app.chat_widget
        .apply_external_edit("queued follow-up".to_string());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    let input_state = app
        .chat_widget
        .capture_thread_input_state()
        .expect("expected queued follow-up state");

    let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
        make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat_widget;
    app.chat_widget.handle_thread_session(session.clone());
    while new_op_rx.try_recv().is_ok() {}

    app.replay_thread_snapshot(
        ThreadEventSnapshot {
            session: None,
            turns: Vec::new(),
            events: vec![
                ThreadBufferedEvent::Notification(Box::new(turn_completed_notification(
                    thread_id,
                    "turn-0",
                    TurnStatus::Completed,
                ))),
                ThreadBufferedEvent::Notification(Box::new(turn_started_notification(
                    thread_id, "turn-1",
                ))),
            ],
            input_state: Some(input_state),
        },
        /*resume_restored_queue*/ true,
    );

    assert!(
        new_op_rx.try_recv().is_err(),
        "queued follow-up should stay queued until the latest turn completes"
    );
    assert_eq!(
        app.chat_widget.queued_user_message_texts(),
        vec!["queued follow-up".to_string()]
    );

    app.chat_widget.handle_server_notification(
        turn_completed_notification(thread_id, "turn-1", TurnStatus::Completed),
        /*replay_kind*/ None,
    );

    match next_user_turn_op(&mut new_op_rx) {
        Op::UserTurn { items, .. } => assert_eq!(
            items,
            vec![UserInput::Text {
                text: "queued follow-up".to_string(),
                text_elements: Vec::new(),
            }]
        ),
        other => panic!("expected queued follow-up submission, got {other:?}"),
    }
}

#[tokio::test]
async fn replay_thread_snapshot_restores_pending_pastes_for_submit() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
    app.thread_event_channels.insert(
        thread_id,
        ThreadEventChannel::new_with_session(
            THREAD_EVENT_CHANNEL_CAPACITY,
            session.clone(),
            Vec::new(),
        ),
    );
    app.activate_thread_channel(thread_id).await;
    app.chat_widget.handle_thread_session(session);

    let large = "x".repeat(1005);
    app.chat_widget.handle_paste(large.clone());
    let expected_input_state = app
        .chat_widget
        .capture_thread_input_state()
        .expect("expected thread input state");

    app.store_active_thread_receiver().await;

    let snapshot = {
        let channel = app
            .thread_event_channels
            .get(&thread_id)
            .expect("thread channel should exist");
        let store = channel.store.lock().await;
        assert_eq!(store.input_state, Some(expected_input_state));
        store.snapshot()
    };

    let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
        make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat_widget;
    app.replay_thread_snapshot(snapshot, /*resume_restored_queue*/ true);

    assert_eq!(app.chat_widget.composer_text_with_pending(), large);

    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    match next_user_turn_op(&mut new_op_rx) {
        Op::UserTurn { items, .. } => assert_eq!(
            items,
            vec![UserInput::Text {
                text: large,
                text_elements: Vec::new(),
            }]
        ),
        other => panic!("expected restored paste submission, got {other:?}"),
    }
}

#[tokio::test]
async fn replay_thread_snapshot_restores_collaboration_mode_for_draft_submit() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
    app.chat_widget.handle_thread_session(session.clone());
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::High));
    app.chat_widget
        .set_collaboration_mask(CollaborationModeMask {
            name: "Plan".to_string(),
            mode: Some(ModeKind::Plan),
            model: Some("gpt-restored".to_string()),
            reasoning_effort: Some(Some(ReasoningEffortConfig::High)),
            developer_instructions: None,
        });
    app.chat_widget
        .apply_external_edit("draft prompt".to_string());
    let input_state = app
        .chat_widget
        .capture_thread_input_state()
        .expect("expected draft input state");

    let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
        make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat_widget;
    app.chat_widget.handle_thread_session(session.clone());
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::Low));
    app.chat_widget
        .set_collaboration_mask(CollaborationModeMask {
            name: "Default".to_string(),
            mode: Some(ModeKind::Default),
            model: Some("gpt-replacement".to_string()),
            reasoning_effort: Some(Some(ReasoningEffortConfig::Low)),
            developer_instructions: None,
        });
    while new_op_rx.try_recv().is_ok() {}

    app.replay_thread_snapshot(
        ThreadEventSnapshot {
            session: None,
            turns: Vec::new(),
            events: vec![],
            input_state: Some(input_state),
        },
        /*resume_restored_queue*/ true,
    );
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    match next_user_turn_op(&mut new_op_rx) {
        Op::UserTurn {
            items,
            model,
            effort,
            collaboration_mode,
            ..
        } => {
            assert_eq!(
                items,
                vec![UserInput::Text {
                    text: "draft prompt".to_string(),
                    text_elements: Vec::new(),
                }]
            );
            assert_eq!(model, "gpt-restored".to_string());
            assert_eq!(effort, Some(ReasoningEffortConfig::High));
            assert_eq!(
                collaboration_mode,
                Some(CollaborationMode {
                    mode: ModeKind::Plan,
                    settings: Settings {
                        model: "gpt-restored".to_string(),
                        reasoning_effort: Some(ReasoningEffortConfig::High),
                        developer_instructions: None,
                    },
                })
            );
        }
        other => panic!("expected restored draft submission, got {other:?}"),
    }
}

#[tokio::test]
async fn replay_thread_snapshot_restores_collaboration_mode_without_input() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
    app.chat_widget.handle_thread_session(session.clone());
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::High));
    app.chat_widget
        .set_collaboration_mask(CollaborationModeMask {
            name: "Plan".to_string(),
            mode: Some(ModeKind::Plan),
            model: Some("gpt-restored".to_string()),
            reasoning_effort: Some(Some(ReasoningEffortConfig::High)),
            developer_instructions: None,
        });
    let input_state = app
        .chat_widget
        .capture_thread_input_state()
        .expect("expected collaboration-only input state");

    let (chat_widget, _app_event_tx, _rx, _new_op_rx) = make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat_widget;
    app.chat_widget.handle_thread_session(session.clone());
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::Low));
    app.chat_widget
        .set_collaboration_mask(CollaborationModeMask {
            name: "Default".to_string(),
            mode: Some(ModeKind::Default),
            model: Some("gpt-replacement".to_string()),
            reasoning_effort: Some(Some(ReasoningEffortConfig::Low)),
            developer_instructions: None,
        });

    app.replay_thread_snapshot(
        ThreadEventSnapshot {
            session: None,
            turns: Vec::new(),
            events: vec![],
            input_state: Some(input_state),
        },
        /*resume_restored_queue*/ true,
    );

    assert_eq!(
        app.chat_widget.active_collaboration_mode_kind(),
        ModeKind::Plan
    );
    assert_eq!(app.chat_widget.current_model(), "gpt-restored");
    assert_eq!(
        app.chat_widget.current_reasoning_effort(),
        Some(ReasoningEffortConfig::High)
    );
}

#[tokio::test]
async fn replayed_interrupted_turn_restores_queued_input_to_composer() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    let session = test_thread_session(thread_id, test_path_buf("/tmp/project"));
    app.chat_widget.handle_thread_session(session.clone());
    app.chat_widget.handle_server_notification(
        turn_started_notification(thread_id, "turn-1"),
        /*replay_kind*/ None,
    );
    app.chat_widget.handle_server_notification(
        agent_message_delta_notification(thread_id, "turn-1", "agent-1", "streaming"),
        /*replay_kind*/ None,
    );
    app.chat_widget
        .apply_external_edit("queued follow-up".to_string());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let input_state = app
        .chat_widget
        .capture_thread_input_state()
        .expect("expected queued follow-up state");

    let (chat_widget, _app_event_tx, _rx, mut new_op_rx) =
        make_chatwidget_manual_with_sender().await;
    app.chat_widget = chat_widget;
    app.chat_widget.handle_thread_session(session.clone());
    while new_op_rx.try_recv().is_ok() {}

    app.replay_thread_snapshot(
        ThreadEventSnapshot {
            session: None,
            turns: Vec::new(),
            events: vec![ThreadBufferedEvent::Notification(Box::new(
                turn_completed_notification(thread_id, "turn-1", TurnStatus::Interrupted),
            ))],
            input_state: Some(input_state),
        },
        /*resume_restored_queue*/ true,
    );

    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "queued follow-up"
    );
    assert!(app.chat_widget.queued_user_message_texts().is_empty());
    assert!(
        new_op_rx.try_recv().is_err(),
        "replayed interrupted turns should restore queued input for editing, not submit it"
    );
}

#[tokio::test]
async fn token_usage_update_refreshes_status_line_with_runtime_context_window() {
    let mut app = make_test_app().await;
    app.chat_widget.setup_status_line(
        vec![crate::bottom_pane::StatusLineItem::ContextWindowSize],
        /*use_theme_colors*/ true,
    );

    assert_eq!(app.chat_widget.status_line_text(), None);

    app.handle_thread_event_now(ThreadBufferedEvent::Notification(Box::new(
        token_usage_notification(ThreadId::new(), "turn-1", Some(950_000)),
    )));

    assert_eq!(
        app.chat_widget.status_line_text(),
        Some("950K window".into())
    );
}

#[tokio::test]
async fn collab_receiver_notification_caches_thread_without_app_server_read() {
    let mut app = make_test_app().await;
    let receiver_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000123").expect("valid thread id");

    app.handle_thread_event_now(ThreadBufferedEvent::Notification(Box::new(
        ServerNotification::ItemStarted(ItemStartedNotification {
            thread_id: ThreadId::new().to_string(),
            turn_id: "turn-1".to_string(),
            started_at_ms: 0,
            item: ThreadItem::CollabAgentToolCall {
                id: "wait-1".to_string(),
                tool: codex_app_server_protocol::CollabAgentTool::Wait,
                status: codex_app_server_protocol::CollabAgentToolCallStatus::InProgress,
                sender_thread_id: ThreadId::new().to_string(),
                receiver_thread_ids: vec![receiver_thread_id.to_string()],
                prompt: None,
                model: None,
                reasoning_effort: None,
                provider_account: None,
                agents_states: HashMap::new(),
            },
        }),
    )));

    assert_eq!(
        app.agent_navigation.get(&receiver_thread_id),
        Some(&AgentPickerThreadEntry {
            agent_nickname: None,
            agent_role: None,
            agent_path: None,
            model: None,
            last_task_message: None,
            last_result_message: None,
            is_running: false,
            is_closed: false,
        })
    );
}

#[tokio::test]
async fn collab_receiver_notification_does_not_cache_not_found_thread() {
    let mut app = make_test_app().await;
    let receiver_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000124").expect("valid thread id");

    app.handle_thread_event_now(ThreadBufferedEvent::Notification(Box::new(
        ServerNotification::ItemCompleted(codex_app_server_protocol::ItemCompletedNotification {
            thread_id: ThreadId::new().to_string(),
            turn_id: "turn-1".to_string(),
            completed_at_ms: 0,
            item: ThreadItem::CollabAgentToolCall {
                id: "send-1".to_string(),
                tool: codex_app_server_protocol::CollabAgentTool::SendInput,
                status: codex_app_server_protocol::CollabAgentToolCallStatus::Failed,
                sender_thread_id: ThreadId::new().to_string(),
                receiver_thread_ids: vec![receiver_thread_id.to_string()],
                prompt: Some("hello".to_string()),
                model: None,
                reasoning_effort: None,
                provider_account: None,
                agents_states: HashMap::from([(
                    receiver_thread_id.to_string(),
                    codex_app_server_protocol::CollabAgentState {
                        status: codex_app_server_protocol::CollabAgentStatus::NotFound,
                        message: None,
                    },
                )]),
            },
        }),
    )));

    assert_eq!(app.agent_navigation.get(&receiver_thread_id), None);
}

#[tokio::test]
async fn collab_receiver_notification_caches_result_preview() {
    let mut app = make_test_app().await;
    let receiver_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000125").expect("valid thread id");

    app.upsert_agent_picker_thread(
        receiver_thread_id,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.handle_thread_event_now(ThreadBufferedEvent::Notification(Box::new(
        ServerNotification::ItemCompleted(codex_app_server_protocol::ItemCompletedNotification {
            thread_id: ThreadId::new().to_string(),
            turn_id: "turn-1".to_string(),
            completed_at_ms: 0,
            item: ThreadItem::CollabAgentToolCall {
                id: "wait-1".to_string(),
                tool: codex_app_server_protocol::CollabAgentTool::Wait,
                status: codex_app_server_protocol::CollabAgentToolCallStatus::Completed,
                sender_thread_id: ThreadId::new().to_string(),
                receiver_thread_ids: vec![receiver_thread_id.to_string()],
                prompt: None,
                model: None,
                reasoning_effort: None,
                provider_account: None,
                agents_states: HashMap::from([(
                    receiver_thread_id.to_string(),
                    codex_app_server_protocol::CollabAgentState {
                        status: codex_app_server_protocol::CollabAgentStatus::Completed,
                        message: Some("created animated proof card".to_string()),
                    },
                )]),
            },
        }),
    )));

    assert_eq!(
        app.agent_navigation
            .get(&receiver_thread_id)
            .and_then(|entry| entry.last_result_message.as_deref()),
        Some("created animated proof card")
    );
}

#[tokio::test]
async fn native_followup_dispatch_transitions_only_the_matching_assignment() {
    let mut app = make_test_app().await;
    write_test_whip(
        &app,
        "native-assignment",
        "# assignment: native-assignment\nComplete the worker task.",
    );
    let manager_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000611").expect("manager id");
    let first_worker_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000612").expect("worker id");
    let second_worker_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000613").expect("worker id");
    let unrelated_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000614").expect("unrelated id");
    for (thread_id, name) in [
        (manager_thread_id, "Manager"),
        (first_worker_thread_id, "First Worker"),
        (second_worker_thread_id, "Second Worker"),
        (unrelated_thread_id, "Unrelated"),
    ] {
        app.upsert_agent_picker_thread(
            thread_id,
            Some(name.to_string()),
            /*agent_role*/ None,
            /*is_closed*/ false,
        );
    }
    let manager_node = crate::spawn_orchestration::thread_node_id(manager_thread_id);
    let first_worker_node = crate::spawn_orchestration::thread_node_id(first_worker_thread_id);
    let second_worker_node = crate::spawn_orchestration::thread_node_id(second_worker_thread_id);
    app.handle_orchestrate_command(format!(
        "attach {first_worker_node} native-assignment --mode review --holder {manager_node} --for 8h"
    ));
    app.handle_orchestrate_command(format!(
        "attach {second_worker_node} native-assignment --mode review --holder {manager_node} --for 8h"
    ));

    let completed_send = |receiver_thread_id: ThreadId, call_id: &str| {
        ServerNotification::ItemCompleted(ItemCompletedNotification {
            thread_id: manager_thread_id.to_string(),
            turn_id: "turn-1".to_string(),
            completed_at_ms: 0,
            item: ThreadItem::SubAgentActivity {
                id: call_id.to_string(),
                kind: codex_app_server_protocol::SubAgentActivityKind::Interacted,
                agent_thread_id: receiver_thread_id.to_string(),
                agent_path: format!("/root/worker-{receiver_thread_id}"),
                agent_nickname: None,
                agent_role: None,
                task_preview: None,
            },
        })
    };

    app.handle_thread_event_now(ThreadBufferedEvent::Notification(Box::new(completed_send(
        unrelated_thread_id,
        "send-unrelated",
    ))));
    assert!(matches!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|whip| &whip.kind),
        Some(crate::orchestrate::WhipKind::Assignment {
            phase: crate::orchestrate::AssignmentPhase::Drafting,
            ..
        })
    ));
    assert!(matches!(
        app.orchestrate_whips
            .get("assignment-2")
            .map(|whip| &whip.kind),
        Some(crate::orchestrate::WhipKind::Assignment {
            phase: crate::orchestrate::AssignmentPhase::Drafting,
            ..
        })
    ));

    // Deliberately dispatch to the other active assignment than the legacy holder-only lookup
    // returns. Native delivery identifies both endpoints and must not collapse multiple
    // assignments managed by the same durable thread onto an arbitrary HashMap entry.
    let holder_only_target = app
        .assignment_dispatch_target_for_holder(&manager_node)
        .expect("one active assignment")
        .1;
    let (matching_thread_id, matching_assignment_id, other_assignment_id) =
        if holder_only_target == first_worker_node {
            (second_worker_thread_id, "assignment-2", "assignment-1")
        } else {
            (first_worker_thread_id, "assignment-1", "assignment-2")
        };

    app.handle_thread_event_now(ThreadBufferedEvent::Notification(Box::new(completed_send(
        matching_thread_id,
        "send-worker",
    ))));
    let assignment = app
        .orchestrate_whips
        .get(matching_assignment_id)
        .expect("assignment should remain registered");
    assert!(matches!(
        assignment.kind,
        crate::orchestrate::WhipKind::Assignment {
            phase: crate::orchestrate::AssignmentPhase::Executing,
            execution_started_utc: Some(_),
            ..
        }
    ));
    assert_eq!(
        assignment.last_dispatch_result.as_deref(),
        Some("delivered")
    );
    let other_assignment = app
        .orchestrate_whips
        .get(other_assignment_id)
        .expect("other assignment should remain registered");
    assert!(matches!(
        other_assignment.kind,
        crate::orchestrate::WhipKind::Assignment {
            phase: crate::orchestrate::AssignmentPhase::Drafting,
            ..
        }
    ));
    assert_eq!(other_assignment.last_dispatch_result, None);
}

#[tokio::test]
async fn native_direct_parent_assignment_uses_core_result_delivery_without_duplicate_mandate() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(
        &app,
        "native-parent-assignment",
        "# assignment: native-parent-assignment\nComplete the worker task.",
    );
    let manager_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000621").expect("manager id");
    let worker_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000622").expect("worker id");
    for (thread_id, name) in [(manager_thread_id, "Manager"), (worker_thread_id, "Worker")] {
        app.upsert_agent_picker_thread(
            thread_id,
            Some(name.to_string()),
            /*agent_role*/ None,
            /*is_closed*/ false,
        );
    }
    let manager_node = crate::spawn_orchestration::thread_node_id(manager_thread_id);
    let worker_node = crate::spawn_orchestration::thread_node_id(worker_thread_id);
    app.spawn_parent_by_node
        .insert(worker_node.clone(), manager_node.clone());
    app.handle_orchestrate_command(format!(
        "attach {worker_node} native-parent-assignment --mode review --holder {manager_node} --for 8h"
    ));
    while app_event_rx.try_recv().is_ok() {}
    app.note_whip_holder_dispatched(&manager_node, &worker_node);

    app.note_whip_target_idle_with_fire_control(
        &worker_node,
        Some("NATIVE_PARENT_RESULT"),
        /*allow_fire*/ true,
        /*turn_succeeded*/ true,
    );

    assert!(
        drain_spawn_agent_tasks_for(&mut app_event_rx, manager_thread_id).is_empty(),
        "Core already delivers a direct native child's result to its parent Manager"
    );
    assert_eq!(
        app.orchestrate_whips
            .get("assignment-1")
            .and_then(|whip| whip.last_target_output.as_deref()),
        Some("NATIVE_PARENT_RESULT")
    );

    app.note_whip_target_idle_with_fire_control(
        &manager_node,
        Some("WHIP_DONE"),
        /*allow_fire*/ false,
        /*turn_succeeded*/ true,
    );
    assert!(matches!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|whip| &whip.kind),
        Some(crate::orchestrate::WhipKind::Assignment {
            phase: crate::orchestrate::AssignmentPhase::Done,
            ..
        })
    ));
}

#[tokio::test]
async fn spawn_status_shows_orc_task_preview_from_troll_activity() {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000126").expect("valid thread id");
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000127").expect("valid thread id");
    let orc_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000128").expect("valid thread id");

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.upsert_agent_picker_thread(
        troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        orc_thread_id,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_thread
        .insert(troll_thread_id, main_thread_id);
    app.spawn_parent_by_thread
        .insert(orc_thread_id, troll_thread_id);

    app.agent_navigation
        .record_sub_agent_activity(SubAgentActivityDisplay {
            thread_id: orc_thread_id,
            agent_path: "/root/troll_burzum/orc_snaga".to_string(),
            agent_nickname: None,
            agent_role: None,
            task_preview: Some("build the animated website shell".to_string()),
            is_running_hint: true,
        });

    let status_items = app.spawn_tree_items(/*show_task_actions*/ true);
    assert!(status_items.iter().any(|item| {
        item.name.contains("Snaga [orc]")
            && item
                .description
                .as_deref()
                .is_some_and(|description| description.contains("build the animated website shell"))
    }));
}

#[tokio::test]
async fn pane_spawn_tree_hides_task_actions() {
    let (mut app, mut rx, _op_rx) = make_test_app_with_channels().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000228").expect("valid thread id");
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000229").expect("valid thread id");
    let orc_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000230").expect("valid thread id");

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.upsert_agent_picker_thread(
        troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        orc_thread_id,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_thread
        .insert(troll_thread_id, main_thread_id);
    app.spawn_parent_by_thread
        .insert(orc_thread_id, troll_thread_id);

    let pane_items = app.spawn_tree_items(/*show_task_actions*/ false);
    assert!(
        pane_items
            .iter()
            .any(|item| item.name.contains("Burzum [troll]"))
    );
    assert!(
        pane_items
            .iter()
            .any(|item| item.name.contains("Snaga [orc]"))
    );
    assert!(
        pane_items
            .iter()
            .all(|item| !item.name.contains("Send task to")),
        "/panes should only switch panes, not show dispatch actions"
    );

    let status_items = app.spawn_tree_items(/*show_task_actions*/ true);
    assert!(
        status_items
            .iter()
            .any(|item| item.name.contains("Send task to Burzum [troll]"))
    );
    assert!(
        status_items
            .iter()
            .any(|item| item.name.contains("Send task to Snaga [orc]"))
    );

    let troll_task_item = status_items
        .iter()
        .find(|item| item.name.contains("Send task to Burzum [troll]"))
        .expect("Burzum task action row");
    assert_eq!(troll_task_item.actions.len(), 1);
    (troll_task_item.actions[0])(&app.app_event_tx);
    match rx
        .try_recv()
        .expect("Burzum task action should emit an event")
    {
        AppEvent::OpenSpawnAgentTaskPrompt { thread_id } => assert_eq!(thread_id, troll_thread_id),
        event => panic!("expected Burzum task prompt event, got {event:?}"),
    }

    let orc_task_item = status_items
        .iter()
        .find(|item| item.name.contains("Send task to Snaga [orc]"))
        .expect("Snaga task action row");
    assert_eq!(orc_task_item.actions.len(), 1);
    (orc_task_item.actions[0])(&app.app_event_tx);
    match rx
        .try_recv()
        .expect("Snaga task action should emit an event")
    {
        AppEvent::OpenSpawnAgentTaskPrompt { thread_id } => assert_eq!(thread_id, orc_thread_id),
        event => panic!("expected Snaga task prompt event, got {event:?}"),
    }
}

/// D95 (#1146): only the person's Spawn status prompt sends a task as the
/// person's own input; the route agents' tasks take (assignment fires,
/// model dispatch) still sends them as inter-agent messages.
#[tokio::test]
async fn spawn_status_prompt_alone_sends_person_tasks() {
    let (mut app, mut rx, _op_rx) = make_test_app_with_channels().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000001146").expect("valid thread id");
    let orc_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000001147").expect("valid thread id");
    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.upsert_agent_picker_thread(
        orc_thread_id,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_thread
        .insert(orc_thread_id, main_thread_id);

    (crate::spawn_orchestration::spawn_agent_task_prompt_submit(
        app.app_event_tx.clone(),
        orc_thread_id,
    ))("typed by the person".to_string());
    match rx.try_recv().expect("the prompt sends an event") {
        AppEvent::SubmitSpawnAgentTaskFromPerson { thread_id, task } => {
            assert_eq!(thread_id, orc_thread_id);
            assert_eq!(task, "typed by the person");
        }
        event => panic!("expected the person's task, got {event:?}"),
    }

    app.send_native_task_event(orc_thread_id, "sent by an agent".to_string());
    match rx.try_recv().expect("an agent's task sends an event") {
        AppEvent::SubmitSpawnAgentTask {
            thread_id, task, ..
        } => {
            assert_eq!(thread_id, orc_thread_id);
            assert_eq!(task, "sent by an agent");
        }
        event => panic!("expected an agent's task, got {event:?}"),
    }
}

/// D106 (#1176): an agent's task for a pane the person opened takes the
/// mailbox route, as for an agent-opened pane; it is never sent as the
/// person's input. The person's own Spawn status task still is (D95).
#[tokio::test]
async fn agent_task_for_a_person_opened_pane_takes_the_mailbox_route() {
    let (mut app, mut rx, _op_rx) = make_test_app_with_channels().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000001176").expect("valid thread id");
    let pane_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000001177").expect("valid thread id");
    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.upsert_agent_picker_thread(
        pane_thread_id,
        Some("Worker".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    let entry = app
        .agent_navigation
        .get(&pane_thread_id)
        .expect("pane entry")
        .clone();
    assert!(app.is_operator_owned_codex_user_pane(pane_thread_id, &entry));

    app.send_native_task_event(pane_thread_id, "sent by an agent".to_string());
    match rx.try_recv().expect("an agent's task sends an event") {
        AppEvent::SubmitSpawnAgentTask {
            thread_id, task, ..
        } => {
            assert_eq!(thread_id, pane_thread_id);
            assert_eq!(task, "sent by an agent");
        }
        event => panic!("expected an agent's task for the mailbox, got {event:?}"),
    }
    assert!(rx.try_recv().is_err(), "one event only");

    (crate::spawn_orchestration::spawn_agent_task_prompt_submit(
        app.app_event_tx.clone(),
        pane_thread_id,
    ))("typed by the person".to_string());
    match rx.try_recv().expect("the prompt sends an event") {
        AppEvent::SubmitSpawnAgentTaskFromPerson { thread_id, task } => {
            assert_eq!(thread_id, pane_thread_id);
            assert_eq!(task, "typed by the person");
        }
        event => panic!("expected the person's task, got {event:?}"),
    }
}

#[tokio::test]
async fn pane_spawn_tree_disables_restored_unloaded_native_rows() {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000231").expect("valid thread id");
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000232").expect("valid thread id");
    let orc_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000233").expect("valid thread id");

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.upsert_agent_picker_thread(
        troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ true,
    );
    app.upsert_agent_picker_thread(
        orc_thread_id,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ true,
    );
    app.spawn_parent_by_thread
        .insert(troll_thread_id, main_thread_id);
    app.spawn_parent_by_thread
        .insert(orc_thread_id, troll_thread_id);

    let pane_items = app.spawn_tree_items(/*show_task_actions*/ false);
    let troll_item = pane_items
        .iter()
        .find(|item| item.name.contains("Burzum [troll]"))
        .expect("restored Troll row");
    assert!(troll_item.is_disabled);
    assert!(troll_item.actions.is_empty());
    assert!(
        troll_item
            .disabled_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("no replay transcript or live session"))
    );
    assert!(
        troll_item
            .description
            .as_deref()
            .is_some_and(|description| description.contains("saved-only"))
    );
    let orc_item = pane_items
        .iter()
        .find(|item| item.name.contains("Snaga [orc]"))
        .expect("restored Orc row");
    assert!(orc_item.is_disabled);
    assert!(orc_item.actions.is_empty());

    let status_items = app.spawn_tree_items(/*show_task_actions*/ true);
    let troll_task_item = status_items
        .iter()
        .find(|item| item.name.contains("Send task to Burzum [troll]"))
        .expect("restored Troll task row");
    assert!(!troll_task_item.is_disabled);
    assert!(!troll_task_item.actions.is_empty());
}

#[tokio::test]
async fn pane_picker_keeps_failed_operator_restore_visible_without_automatic_retry() {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000241").expect("valid thread id");
    let failed_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000242").expect("valid thread id");
    let pending_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000243").expect("valid thread id");

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.upsert_agent_picker_thread(
        failed_thread_id,
        Some("Unavailable Pane".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ true,
    );
    app.upsert_agent_picker_thread(
        pending_thread_id,
        Some("Pending Pane".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );

    assert_eq!(
        app.restorable_operator_owned_codex_user_pane_ids(),
        vec![pending_thread_id],
        "a failed pane must not be retried automatically whenever /panes opens"
    );

    let items = app.pane_picker_items();
    let failed_item = items
        .iter()
        .find(|item| item.name.contains("Unavailable Pane"))
        .expect("failed pane remains inspectable");
    assert!(failed_item.is_disabled);
    assert!(failed_item.actions.is_empty());
    assert!(!failed_item.dismiss_on_select);
    assert!(
        failed_item
            .disabled_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("no replay transcript or live session"))
    );
}

#[tokio::test]
async fn terminal_operator_pane_lifecycle_removes_layout_membership_and_falls_back_to_main() {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000251").expect("valid thread id");
    let operator_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000252").expect("valid thread id");
    let managed_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000253").expect("valid thread id");

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(operator_thread_id);
    app.upsert_agent_picker_thread(
        operator_thread_id,
        Some("Archive Me".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        managed_thread_id,
        Some("Keep Managed".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );

    assert!(app.forget_terminal_operator_pane(operator_thread_id));
    assert_eq!(app.active_thread_id, Some(main_thread_id));
    assert!(app.agent_navigation.get(&operator_thread_id).is_none());
    assert!(app.agent_navigation.get(&managed_thread_id).is_some());
    assert!(!app.forget_terminal_operator_pane(managed_thread_id));
    assert!(
        app.pane_picker_items()
            .iter()
            .all(|item| !item.name.contains("Archive Me"))
    );
}

#[tokio::test]
async fn terminal_lifecycle_scope_blocks_external_managed_and_parent_owned_panes() {
    let mut app = make_test_app().await;
    let main_thread_id = ThreadId::new();
    let operator_thread_id = ThreadId::new();
    let crew_thread_id = ThreadId::new();
    let parent_owned_thread_id = ThreadId::new();
    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(operator_thread_id);
    app.upsert_agent_picker_thread(
        operator_thread_id,
        Some("Operator".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );

    assert!(
        app.terminal_thread_lifecycle_block_reason("archive", main_thread_id)
            .is_none()
    );
    assert!(
        app.terminal_thread_lifecycle_block_reason("delete", operator_thread_id)
            .is_none()
    );

    app.ensure_custom_spawn_root(crate::claude_panes::CODEX_MAIN_PANE_ID)
        .expect("bind custom root");
    app.record_custom_spawn_member(
        &crate::spawn_orchestration::thread_node_id(crew_thread_id),
        crate::claude_panes::CODEX_MAIN_PANE_ID,
        crate::spawn_orchestration::SpawnRole::Orc,
        "Snaga".to_string(),
        crate::dispatch_queue::SavedNativeSpawnRuntime {
            model: "deepseek-v4-flash".to_string(),
            provider: "deepseek".to_string(),
            reasoning_effort: Some(codex_protocol::openai_models::ReasoningEffort::High),
        },
    )
    .expect("record managed crew member");
    let crew_reason = app
        .terminal_thread_lifecycle_block_reason("delete", crew_thread_id)
        .expect("managed member must be protected");
    assert!(crew_reason.contains("managed /spawn member"));
    assert!(crew_reason.contains("native graph and pane layout"));

    app.agent_navigation
        .mark_parent_owned(parent_owned_thread_id);
    assert!(
        app.terminal_thread_lifecycle_block_reason("archive", parent_owned_thread_id)
            .is_some_and(|reason| reason.contains("parent-controlled task worker"))
    );

    let claude_pane_id = app
        .claude_panes
        .create_pane_without_vault_for_test(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
        )
        .expect("create Claude pane");
    app.claude_panes
        .set_active_user_pane(&claude_pane_id)
        .expect("activate Claude pane");
    let claude_reason = app
        .terminal_thread_lifecycle_block_reason("archive", main_thread_id)
        .expect("external pane must not fall back to Main");
    assert!(claude_reason.contains("cannot target Claude pane"));
    assert!(claude_reason.contains("Select Main"));
}

#[tokio::test]
async fn delete_current_thread_routes_active_claude_pane_to_external_cleanup() {
    let mut app = make_test_app().await;
    app.primary_thread_id = Some(ThreadId::new());
    let pane_id = app
        .claude_panes
        .create_pane_without_vault_for_test(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
        )
        .expect("create Claude pane");
    let artifact_dir = app.config.codex_home.join("panes").join(&pane_id);
    let cancel_token = tokio_util::sync::CancellationToken::new();
    let pane = app
        .claude_panes
        .panes
        .iter_mut()
        .find(|pane| pane.id == pane_id)
        .expect("pane");
    pane.status = crate::claude_panes::ClaudePaneStatus::Running;
    pane.cancel_token = Some(cancel_token.clone());

    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(
        app.chat_widget.config_ref(),
    ))
    .await
    .expect("embedded app server");
    let control = Box::pin(app.delete_current_thread(&mut app_server)).await;

    assert!(matches!(
        control,
        AppRunControl::Exit(ExitReason::UserRequested)
    ));
    assert!(cancel_token.is_cancelled());
    assert!(app.claude_panes.panes().is_empty());
    assert_eq!(app.claude_panes.active_user_pane_id(), CODEX_MAIN_PANE_ID);
    assert!(!artifact_dir.exists());
}

#[tokio::test]
async fn active_claude_pane_routes_before_native_thread_resolution() {
    let (mut app, mut event_rx, _op_rx) = make_test_app_with_channels().await;
    app.active_thread_id = None;
    app.claude_panes
        .create_pane_without_vault_for_test(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
        )
        .expect("create Claude pane");
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(
        app.chat_widget.config_ref(),
    ))
    .await
    .expect("embedded app server");

    app.submit_active_thread_op(&mut app_server, claude_pane_text_op("/panes"))
        .await
        .expect("route external pane command");

    assert!(
        std::iter::from_fn(|| event_rx.try_recv().ok())
            .any(|event| matches!(event, AppEvent::OpenPanePicker)),
        "the selected external pane must consume input before native-thread lookup"
    );
}

#[test]
fn whole_crew_removal_deletes_mixed_members_and_preserves_bound_main() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_stack_size(8 * 1024 * 1024)
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let mut app = Box::new(make_test_app().await);
        let mut app_server = start_config_write_test_app_server(&app).await?;
        let main = app_server.start_thread(&app.config).await?;
        let main_thread_id = main.session.thread_id;
        app.primary_thread_id = Some(main_thread_id);
        app.active_thread_id = Some(main_thread_id);
        app.primary_session_configured = Some(main.session.clone());
        app.set_spawn_nazgul_pane_binding(CODEX_MAIN_PANE_ID.to_string());

        let provider = app.config.model_provider_id.clone();
        let model = app.chat_widget.current_model().to_string();
        let effort = app.chat_widget.current_reasoning_effort();
        let spec = codex_protocol::crew::CrewSpec {
            schema_version: codex_protocol::crew::CURRENT_CREW_SCHEMA_VERSION,
            crew_id: "mixed-removal-fixture".to_string(),
            preset_id: None,
            members: vec![
                codex_protocol::crew::CrewMemberSpec {
                    logical_member_id: "nazgul".to_string(),
                    display_name: "Main".to_string(),
                    role_profile: "nazgul".to_string(),
                    parent_member_id: None,
                    runtime_request: codex_protocol::crew::RuntimeRequest::exact(
                        provider.clone(),
                        model.clone(),
                        effort.clone(),
                    ),
                },
                codex_protocol::crew::CrewMemberSpec {
                    logical_member_id: "troll".to_string(),
                    display_name: "Burzum".to_string(),
                    role_profile: "troll".to_string(),
                    parent_member_id: Some("nazgul".to_string()),
                    runtime_request: codex_protocol::crew::RuntimeRequest::exact(
                        provider.clone(),
                        model.clone(),
                        effort.clone(),
                    ),
                },
                codex_protocol::crew::CrewMemberSpec {
                    logical_member_id: "orc".to_string(),
                    display_name: "Snaga".to_string(),
                    role_profile: "orc".to_string(),
                    parent_member_id: Some("troll".to_string()),
                    runtime_request: codex_protocol::crew::RuntimeRequest::exact(
                        provider.clone(),
                        model.clone(),
                        effort.clone(),
                    ),
                },
            ],
            policy: codex_protocol::crew::CrewPolicy {
                delegation_mode: codex_protocol::crew::DelegationMode::Proactive,
                allow_ephemeral_descendants: true,
                provider_allowlist: vec![provider.clone()],
                maximum_spend_usd: None,
            },
        };
        let mut crew = crate::crew_state::CrewInstanceState::begin(spec).expect("valid crew");
        crew.record_member("nazgul", CODEX_MAIN_PANE_ID)
            .expect("map bound Main");

        let spawn_config = app.native_spawn_agent_config()?;
        let troll = app_server
            .spawn_agent_thread_with_class(
                &spawn_config,
                main_thread_id,
                "troll".to_string(),
                Some("Burzum".to_string()),
                codex_protocol::crew::AgentClass::CrewMember {
                    crew_id: crew.spec.crew_id.clone(),
                    logical_member_id: "troll".to_string(),
                    human_addressable: true,
                },
                model,
                Some(provider),
                effort,
                /*base_instructions*/ None,
            )
            .await?;
        let troll_thread_id = troll.session.thread_id;
        let troll_node_id = crate::spawn_orchestration::thread_node_id(troll_thread_id);
        app.register_spawn_agent_pane(
            troll_thread_id,
            main_thread_id,
            crate::spawn_orchestration::pane_node_id(CODEX_MAIN_PANE_ID),
            Some("Burzum".to_string()),
            "troll",
            troll,
            /*persist_layout*/ true,
        )
        .await;
        crew.record_member("troll", &troll_node_id)
            .expect("map Troll");

        let orc_pane_id = app
            .claude_panes
            .create_pane_with_role(
                crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
                app.config.cwd.to_path_buf(),
                app.config.codex_home.as_ref(),
                Some(crate::spawn_orchestration::SpawnRole::Orc),
                Some("Snaga".to_string()),
            )
            .expect("create managed Claude Orc");
        let orc_node_id = crate::spawn_orchestration::pane_node_id(&orc_pane_id);
        app.spawn_parent_by_node
            .insert(orc_node_id.clone(), troll_node_id.clone());
        crew.record_member("orc", &orc_node_id).expect("map Orc");
        crew.mark_ready().expect("ready crew");
        app.spawn_crew = Some(crew);
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let orc_pane = app
            .claude_panes
            .panes
            .iter_mut()
            .find(|pane| pane.id == orc_pane_id)
            .expect("Orc pane");
        orc_pane.status = crate::claude_panes::ClaudePaneStatus::Running;
        orc_pane.cancel_token = Some(cancel_token.clone());
        app.claude_panes
            .set_active_user_pane(&orc_pane_id)
            .expect("select Orc");
        let orc_artifact_dir = app.config.codex_home.join("panes").join(&orc_pane_id);

        let mut tui = crate::tui::test_support::make_test_tui()?;
        let result = Box::pin(app.remove_spawn_crew(&mut tui, &mut app_server)).await?;

        assert!(result.contains("preserved the bound user-owned root"));
        assert!(cancel_token.is_cancelled());
        assert!(!orc_artifact_dir.exists());
        assert!(app.claude_panes.panes().is_empty());
        assert!(app.spawn_crew.is_none());
        assert!(app.spawn_parent_by_node.is_empty());
        assert_eq!(app.claude_panes.active_user_pane_id(), CODEX_MAIN_PANE_ID);
        assert_eq!(app.active_thread_id, Some(main_thread_id));
        let empty_rows = app.spawn_tree_items(/*show_task_actions*/ false);
        assert_eq!(empty_rows.len(), 1);
        assert_eq!(empty_rows[0].name, "No managed crew");
        assert!(empty_rows[0].is_disabled);
        assert!(
            empty_rows.iter().all(|row| !row.name.contains("Nazgul")),
            "removing a crew must not reclassify the surviving Main pane as its Nazgul"
        );
        app_server
            .thread_read(main_thread_id, /*include_turns*/ false)
            .await
            .expect("bound Main must survive");
        assert!(
            app_server
                .thread_read(troll_thread_id, /*include_turns*/ false)
                .await
                .is_err(),
            "native managed member must be permanently deleted"
        );
        Ok(())
    })
}

#[tokio::test]
async fn empty_spawn_tree_does_not_synthesize_main_as_nazgul() {
    let mut app = make_test_app().await;
    let main_thread_id = ThreadId::new();
    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);

    let rows = app.spawn_tree_items(/*show_task_actions*/ false);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "No managed crew");
    assert!(rows[0].is_disabled);
    assert!(rows.iter().all(|row| !row.name.contains("Nazgul")));
    assert!(rows.iter().all(|row| !row.name.contains("Main")));
}

#[tokio::test]
async fn pane_spawn_tree_hides_superseded_saved_native_duplicates() {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000241").expect("valid thread id");
    let old_troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000242").expect("valid thread id");
    let old_orc_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000243").expect("valid thread id");
    let live_troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000244").expect("valid thread id");
    let live_orc_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000245").expect("valid thread id");

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.spawn_nazgul_pane_id = Some(crate::spawn_orchestration::thread_node_id(main_thread_id));
    app.upsert_agent_picker_thread(
        old_troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ true,
    );
    app.upsert_agent_picker_thread(
        old_orc_thread_id,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ true,
    );
    app.upsert_agent_picker_thread(
        live_troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        live_orc_thread_id,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_node.insert(
        crate::spawn_orchestration::thread_node_id(old_troll_thread_id),
        crate::spawn_orchestration::thread_node_id(main_thread_id),
    );
    app.spawn_parent_by_node.insert(
        crate::spawn_orchestration::thread_node_id(old_orc_thread_id),
        crate::spawn_orchestration::thread_node_id(old_troll_thread_id),
    );
    app.spawn_parent_by_node.insert(
        crate::spawn_orchestration::thread_node_id(live_troll_thread_id),
        crate::spawn_orchestration::thread_node_id(main_thread_id),
    );
    app.spawn_parent_by_node.insert(
        crate::spawn_orchestration::thread_node_id(live_orc_thread_id),
        crate::spawn_orchestration::thread_node_id(live_troll_thread_id),
    );

    let items = app.spawn_tree_items(/*show_task_actions*/ false);
    let burzum_rows = items
        .iter()
        .filter(|item| item.name.contains("Burzum [troll]"))
        .collect::<Vec<_>>();
    let snaga_rows = items
        .iter()
        .filter(|item| item.name.contains("Snaga [orc]"))
        .collect::<Vec<_>>();

    assert_eq!(burzum_rows.len(), 1);
    assert_eq!(snaga_rows.len(), 1);
    assert!(
        !burzum_rows[0]
            .description
            .as_deref()
            .is_some_and(|description| description.contains(&old_troll_thread_id.to_string()))
    );
    assert!(
        !snaga_rows[0]
            .description
            .as_deref()
            .is_some_and(|description| description.contains(&old_orc_thread_id.to_string()))
    );
}

#[tokio::test]
async fn duplicate_live_native_replacements_are_pruned() {
    let mut app = make_test_app().await;
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000246").expect("valid thread id");
    let old_snaga_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000247").expect("valid thread id");
    let old_ghash_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000248").expect("valid thread id");
    let new_snaga_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000249").expect("valid thread id");
    let new_ghash_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000250").expect("valid thread id");
    let troll_node_id = crate::spawn_orchestration::thread_node_id(troll_thread_id);

    app.upsert_agent_picker_thread(
        troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    for (thread_id, nickname) in [
        (old_snaga_thread_id, "Snaga"),
        (old_ghash_thread_id, "Ghash"),
        (new_snaga_thread_id, "Snaga"),
        (new_ghash_thread_id, "Ghash"),
    ] {
        app.upsert_agent_picker_thread(
            thread_id,
            Some(nickname.to_string()),
            Some("orc".to_string()),
            /*is_closed*/ false,
        );
        app.spawn_parent_by_node.insert(
            crate::spawn_orchestration::thread_node_id(thread_id),
            troll_node_id.clone(),
        );
    }

    app.prune_duplicate_live_native_spawn_threads();

    assert!(
        app.agent_navigation.get(&old_snaga_thread_id).is_none(),
        "saved Snaga should be replaced by a live endpoint; navigation={:?}; endpoints={:?}",
        app.agent_navigation.ordered_threads(),
        app.spawn_native_endpoint_by_node
    );
    assert!(
        app.agent_navigation.get(&old_ghash_thread_id).is_none(),
        "saved Ghash should be replaced by a live endpoint; navigation={:?}; endpoints={:?}",
        app.agent_navigation.ordered_threads(),
        app.spawn_native_endpoint_by_node
    );
    assert!(app.agent_navigation.get(&new_snaga_thread_id).is_some());
    assert!(app.agent_navigation.get(&new_ghash_thread_id).is_some());
    let child_thread_ids = app
        .spawn_parent_by_node
        .iter()
        .filter_map(|(child_node_id, parent_node_id)| {
            (parent_node_id == &troll_node_id)
                .then(|| crate::spawn_orchestration::node_id_thread(child_node_id))
                .flatten()
        })
        .collect::<Vec<_>>();
    assert_eq!(child_thread_ids.len(), 2);
    assert!(child_thread_ids.contains(&old_snaga_thread_id));
    assert!(child_thread_ids.contains(&old_ghash_thread_id));
    assert_eq!(
        app.spawn_native_endpoint_by_node
            [&crate::spawn_orchestration::thread_node_id(old_snaga_thread_id)],
        new_snaga_thread_id
    );
    assert_eq!(
        app.spawn_native_endpoint_by_node
            [&crate::spawn_orchestration::thread_node_id(old_ghash_thread_id)],
        new_ghash_thread_id
    );
    assert!(
        !app.spawn_parent_by_node
            .contains_key(&crate::spawn_orchestration::thread_node_id(
                new_snaga_thread_id
            ))
    );
    assert!(
        !app.spawn_parent_by_node
            .contains_key(&crate::spawn_orchestration::thread_node_id(
                new_ghash_thread_id
            ))
    );
}

#[tokio::test]
async fn restore_materializes_saved_native_orcs_without_rollouts() -> Result<()> {
    let mut app = make_test_app().await;
    let codex_home = tempdir()?;
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    std::fs::write(
        app.config.codex_home.join("provider_auth.json"),
        format!(
            r#"{{"api_keys":{{"{ZAI_API_KEY_ENV_VAR}":"test-key","{VERCEL_API_KEY_ENV_VAR}":"test-key"}}}}"#
        ),
    )?;
    app.chat_widget.update_account_state(
        /*status_account_display*/ None, /*plan_type*/ None,
        /*has_chatgpt_account*/ true, /*has_codex_backend_auth*/ true,
    );
    let mut app_server = start_config_write_test_app_server(&app).await?;
    let main = app_server.start_thread(&app.config).await?;
    let main_thread_id = main.session.thread_id;
    let main_rollout_path = main
        .session
        .rollout_path
        .as_ref()
        .expect("Main rollout path");
    assert!(main_rollout_path.is_file(), "Main rollout must be durable");
    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.primary_session_configured = Some(main.session.clone());

    let spawn_config = app.native_spawn_agent_config()?;
    let nazgul = app_server
        .spawn_agent_thread(
            &spawn_config,
            main_thread_id,
            "nazgul".to_string(),
            Some("Angmar".to_string()),
            App::STANDARD_NAZGUL_MODEL.to_string(),
            Some(CLAUDE_PLAN_PROVIDER_ID.to_string()),
            /*reasoning_effort*/ None,
            /*base_instructions*/ None,
        )
        .await?;
    let nazgul_thread_id = nazgul.session.thread_id;
    let nazgul_rollout_path = nazgul
        .session
        .rollout_path
        .as_ref()
        .expect("nazgul rollout path");
    assert!(nazgul_rollout_path.exists());
    app.register_spawn_agent_pane(
        nazgul_thread_id,
        main_thread_id,
        crate::spawn_orchestration::pane_node_id(crate::claude_panes::CODEX_MAIN_PANE_ID),
        Some("Angmar".to_string()),
        "nazgul",
        nazgul,
        /*persist_layout*/ true,
    )
    .await;
    app.set_spawn_nazgul_pane_binding(crate::spawn_orchestration::thread_node_id(nazgul_thread_id));

    let troll = app_server
        .spawn_agent_thread(
            &spawn_config,
            nazgul_thread_id,
            "troll".to_string(),
            Some("Burzum".to_string()),
            App::STANDARD_TROLL_MODEL.to_string(),
            Some(OPENAI_PROVIDER_ID.to_string()),
            Some(ReasoningEffortConfig::XHigh),
            /*base_instructions*/ None,
        )
        .await?;
    let troll_thread_id = troll.session.thread_id;
    let troll_rollout_path = troll
        .session
        .rollout_path
        .as_ref()
        .expect("troll rollout path");
    assert!(troll_rollout_path.exists());
    app.register_spawn_agent_pane(
        troll_thread_id,
        nazgul_thread_id,
        crate::spawn_orchestration::thread_node_id(nazgul_thread_id),
        Some("Burzum".to_string()),
        "troll",
        troll,
        /*persist_layout*/ true,
    )
    .await;
    app.thread_event_channels
        .get_mut(&troll_thread_id)
        .expect("registered Troll channel")
        .mark_replay_only();
    assert!(app.thread_event_channels.contains_key(&troll_thread_id));
    assert!(!app.native_agent_thread_has_loaded_session(troll_thread_id));

    let old_snaga_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000251").expect("valid thread id");
    let old_ghash_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000252").expect("valid thread id");
    let troll_node_id = crate::spawn_orchestration::thread_node_id(troll_thread_id);
    let old_snaga_node_id = crate::spawn_orchestration::thread_node_id(old_snaga_thread_id);
    let old_ghash_node_id = crate::spawn_orchestration::thread_node_id(old_ghash_thread_id);
    app.spawn_parent_by_node
        .insert(old_snaga_node_id.clone(), troll_node_id.clone());
    app.spawn_parent_by_node
        .insert(old_ghash_node_id.clone(), troll_node_id.clone());
    app.upsert_agent_picker_thread(
        old_snaga_thread_id,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ true,
    );
    app.upsert_agent_picker_thread(
        old_ghash_thread_id,
        Some("Ghash".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ true,
    );
    // Durable restoration is CrewSpec-backed. Register the saved logical members so the
    // restoration boundary does not mistake these deliberate no-rollout fixtures for unrelated
    // legacy task agents when Main itself is already durable.
    app.ensure_custom_spawn_root(&crate::spawn_orchestration::thread_node_id(
        nazgul_thread_id,
    ))
    .expect("record durable Nazgul root");
    app.record_custom_spawn_member(
        &troll_node_id,
        &crate::spawn_orchestration::thread_node_id(nazgul_thread_id),
        crate::spawn_orchestration::SpawnRole::Troll,
        "Burzum".to_string(),
        crate::dispatch_queue::SavedNativeSpawnRuntime {
            model: App::STANDARD_TROLL_MODEL.to_string(),
            provider: OPENAI_PROVIDER_ID.to_string(),
            reasoning_effort: Some(ReasoningEffortConfig::XHigh),
        },
    )
    .expect("record durable Troll");
    for (node_id, nickname) in [(&old_snaga_node_id, "Snaga"), (&old_ghash_node_id, "Ghash")] {
        app.record_custom_spawn_member(
            node_id,
            &troll_node_id,
            crate::spawn_orchestration::SpawnRole::Orc,
            nickname.to_string(),
            crate::dispatch_queue::SavedNativeSpawnRuntime {
                model: App::STANDARD_ORC_MODEL.to_string(),
                provider: OPENAI_PROVIDER_ID.to_string(),
                reasoning_effort: Some(ReasoningEffortConfig::XHigh),
            },
        )
        .expect("record durable Orc");
    }

    app.restore_native_spawn_panes_from_saved_state(&mut app_server)
        .await;

    assert!(
        app.agent_navigation.get(&old_snaga_thread_id).is_none(),
        "saved Snaga should be replaced by a live endpoint; navigation={:?}; endpoints={:?}",
        app.agent_navigation.ordered_threads(),
        app.spawn_native_endpoint_by_node
    );
    assert!(
        app.agent_navigation.get(&old_ghash_thread_id).is_none(),
        "saved Ghash should be replaced by a live endpoint; navigation={:?}; endpoints={:?}",
        app.agent_navigation.ordered_threads(),
        app.spawn_native_endpoint_by_node
    );
    assert!(app.native_agent_thread_has_loaded_session(troll_thread_id));
    assert_eq!(
        app.agent_navigation
            .get(&troll_thread_id)
            .and_then(|entry| entry.model.as_deref()),
        Some(App::STANDARD_TROLL_MODEL)
    );
    // Saved pane ids are durable logical identities. Materialization keeps those parent edges and
    // routes each logical id to the new live thread through spawn_native_endpoint_by_node.
    assert_eq!(
        app.spawn_parent_by_node.get(&old_snaga_node_id),
        Some(&troll_node_id)
    );
    assert_eq!(
        app.spawn_parent_by_node.get(&old_ghash_node_id),
        Some(&troll_node_id)
    );
    let restored_orc_thread_ids = [&old_snaga_node_id, &old_ghash_node_id]
        .into_iter()
        .map(|node_id| {
            *app.spawn_native_endpoint_by_node
                .get(node_id)
                .expect("saved Orc has a live endpoint")
        })
        .collect::<Vec<_>>();
    assert!(!restored_orc_thread_ids.contains(&old_snaga_thread_id));
    assert!(!restored_orc_thread_ids.contains(&old_ghash_thread_id));
    let restored_orcs = restored_orc_thread_ids
        .into_iter()
        .map(|thread_id| {
            (
                thread_id,
                app.agent_navigation
                    .get(&thread_id)
                    .expect("materialized Orc is visible"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(restored_orcs.len(), 2);
    let restored_names = restored_orcs
        .iter()
        .filter_map(|(_, entry)| entry.agent_nickname.as_deref())
        .collect::<Vec<_>>();
    assert!(restored_names.contains(&"Snaga"), "{restored_names:?}");
    assert!(restored_names.contains(&"Ghash"), "{restored_names:?}");
    for (thread_id, entry) in restored_orcs {
        assert!(!entry.is_closed);
        assert!(app.native_agent_thread_has_loaded_session(thread_id));
    }
    let items = app.spawn_tree_items(/*show_task_actions*/ false);
    assert!(
        items
            .iter()
            .filter(|item| item.name.contains("[orc]"))
            .all(|item| !item.is_disabled)
    );
    assert!(items.iter().all(|item| {
        !item
            .description
            .as_deref()
            .is_some_and(|description| description.contains("saved-only"))
    }));

    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn app_server_spawn_treats_hierarchy_role_as_metadata_under_general_worker() -> Result<()> {
    let app = make_test_app().await;
    let mut app_server = start_config_write_test_app_server(&app).await?;
    let main = app_server.start_thread(&app.config).await?;
    let spawn_config = app.native_spawn_agent_config()?;
    let worker = app_server
        .spawn_agent_thread(
            &spawn_config,
            main.session.thread_id,
            "worker".to_string(),
            Some("GeneralWorker".to_string()),
            App::STANDARD_TROLL_MODEL.to_string(),
            Some(OPENAI_PROVIDER_ID.to_string()),
            Some(ReasoningEffortConfig::High),
            /*base_instructions*/ None,
        )
        .await?;

    let orc = app_server
        .spawn_agent_thread(
            &spawn_config,
            worker.session.thread_id,
            "orc".to_string(),
            Some("ForgedOrc".to_string()),
            App::STANDARD_ORC_MODEL.to_string(),
            Some(OPENAI_PROVIDER_ID.to_string()),
            Some(ReasoningEffortConfig::High),
            /*base_instructions*/ None,
        )
        .await?;
    assert_eq!(orc.session.model, App::STANDARD_ORC_MODEL);
    assert_eq!(orc.session.model_provider_id, OPENAI_PROVIDER_ID);
    Ok(())
}

#[tokio::test]
async fn nazgul_can_be_bound_to_a_codex_agent_pane() {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000301").expect("valid thread id");
    // A second Codex agent pane (no Troll/Orc role) that should be bindable as the Nazgul root.
    let codex_pane_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000302").expect("valid thread id");

    app.primary_thread_id = Some(main_thread_id);
    app.upsert_agent_picker_thread(
        codex_pane_thread_id,
        Some("Mordor".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    app.agent_navigation
        .set_running(codex_pane_thread_id, /*is_running*/ true);

    // The Nazgul picker should offer the Codex agent pane as a bindable target.
    let picker_items = app.nazgul_codex_pane_picker_items();
    assert!(
        picker_items.iter().any(|item| item.name.contains("Mordor")),
        "Codex agent pane should appear in the Nazgul binding picker"
    );

    // Bind the Codex agent pane as the Nazgul root.
    let node_id = crate::spawn_orchestration::thread_node_id(codex_pane_thread_id);
    app.spawn_native_runtime_by_node.insert(
        node_id.clone(),
        crate::dispatch_queue::SavedNativeSpawnRuntime {
            model: app.chat_widget.current_model().to_string(),
            provider: app.config.model_provider_id.clone(),
            reasoning_effort: app.config.model_reasoning_effort.clone(),
        },
    );
    app.bind_spawn_nazgul_pane(node_id.clone());
    assert_eq!(app.spawn_nazgul_pane_id.as_deref(), Some(node_id.as_str()));

    // The spawn status tree should render the bound Codex pane by name, not as a raw node id.
    let items = app.spawn_tree_items(/*show_task_actions*/ false);
    let nazgul_item = items
        .iter()
        .find(|item| item.name.starts_with("Nazgul:"))
        .expect("status tree has a Nazgul row");
    assert!(
        nazgul_item.name.contains("Mordor"),
        "bound Codex pane should render by name, got: {}",
        nazgul_item.name
    );
    assert!(
        !nazgul_item.name.contains("thread:"),
        "bound Codex pane should not leak the raw thread node id: {}",
        nazgul_item.name
    );
    assert_eq!(
        nazgul_item.description.as_deref(),
        Some(
            "Root role binding; running; 00000000-0000-0000-0000-000000000302; same user pane listed above."
        ),
        "bound Nazgul must project the same live running state as other managed native roles"
    );

    app.spawn_status_by_thread.insert(
        codex_pane_thread_id,
        codex_app_server_protocol::CollabAgentState {
            status: codex_app_server_protocol::CollabAgentStatus::Completed,
            message: Some("finished root work".to_string()),
        },
    );
    let completed_items = app.spawn_tree_items(/*show_task_actions*/ false);
    let completed_nazgul_item = completed_items
        .iter()
        .find(|item| item.name.starts_with("Nazgul:"))
        .expect("completed status tree has a Nazgul row");
    assert_eq!(
        completed_nazgul_item.description.as_deref(),
        Some(
            "Root role binding; completed; 00000000-0000-0000-0000-000000000302; same user pane listed above."
        ),
        "bound Nazgul must project terminal state through the shared managed-role boundary"
    );

    // When the bound Codex pane is the active thread (and the user pane is Codex Main), the
    // Nazgul row should be marked current.
    app.active_thread_id = Some(codex_pane_thread_id);
    let items = app.spawn_tree_items(/*show_task_actions*/ false);
    let nazgul_item = items
        .iter()
        .find(|item| item.name.starts_with("Nazgul:"))
        .expect("status tree has a Nazgul row");
    assert!(
        nazgul_item.is_current,
        "Nazgul row should be current when the bound Codex pane is active"
    );
}

#[tokio::test]
async fn unbound_codex_main_never_receives_nazgul_context() {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000301").expect("valid thread id");
    let stale_child_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000302").expect("valid thread id");

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.spawn_nazgul_pane_id = None;
    app.spawn_parent_by_thread
        .insert(stale_child_thread_id, main_thread_id);
    app.spawn_status_by_thread.insert(
        stale_child_thread_id,
        codex_app_server_protocol::CollabAgentState {
            status: codex_app_server_protocol::CollabAgentStatus::Completed,
            message: Some("stale child result".to_string()),
        },
    );

    assert!(
        app.spawn_additional_context_for_thread(main_thread_id)
            .is_none(),
        "an unbound primary thread must keep the default prompt"
    );
    assert!(
        app.spawn_context_for_user_pane(CODEX_MAIN_PANE_ID)
            .is_none(),
        "an unbound user pane must not be assigned the Nazgul role"
    );
}

#[tokio::test]
async fn codex_main_bound_nazgul_turn_receives_domain_neutral_hierarchy_context() {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000303").expect("valid thread id");
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000304").expect("valid thread id");

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.bind_spawn_nazgul_pane(CODEX_MAIN_PANE_ID.to_string());
    let troll_runtime = crate::dispatch_queue::SavedNativeSpawnRuntime {
        model: app.chat_widget.current_model().to_string(),
        provider: app.config.model_provider_id.clone(),
        reasoning_effort: app.config.model_reasoning_effort.clone(),
    };
    app.record_custom_spawn_member(
        &crate::spawn_orchestration::thread_node_id(troll_thread_id),
        CODEX_MAIN_PANE_ID,
        crate::spawn_orchestration::SpawnRole::Troll,
        "Burzum".to_string(),
        troll_runtime,
    )
    .expect("register Troll in the bound CrewSpec");
    app.upsert_agent_picker_thread(
        troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_node.insert(
        crate::spawn_orchestration::thread_node_id(troll_thread_id),
        crate::spawn_orchestration::pane_node_id(CODEX_MAIN_PANE_ID),
    );

    let context_map = app
        .spawn_additional_context_for_thread(main_thread_id)
        .expect("bound Codex Main turn should receive Nazgul application context");
    let entry = context_map
        .get("pfterminal_spawn_context")
        .expect("spawn context entry");

    assert_eq!(entry.kind, AdditionalContextKind::Application);
    assert!(entry.value.contains("Nazgul/root pane"));
    assert!(entry.value.contains("Burzum [troll]"));
    assert!(entry.value.contains("send_message"));
    assert!(entry.value.contains("followup_task"));
    assert!(!entry.value.contains("<pfterminal_send_task"));
}

#[tokio::test]
async fn pane_picker_separates_user_panes_from_managed_spawn_crew() {
    let mut app = make_test_app().await;
    let manager_thread = ThreadId::new();
    let worker_thread = ThreadId::new();
    let parent_controlled_thread = ThreadId::new();
    app.upsert_agent_picker_thread(
        manager_thread,
        Some("Codex manager".to_string()),
        Some("default".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        worker_thread,
        Some("Codex worker".to_string()),
        Some("default".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_thread
        .insert(worker_thread, manager_thread);
    let manager_node = crate::spawn_orchestration::thread_node_id(manager_thread);
    app.spawn_native_runtime_by_node.insert(
        manager_node.clone(),
        crate::dispatch_queue::SavedNativeSpawnRuntime {
            model: app.chat_widget.current_model().to_string(),
            provider: app.config.model_provider_id.clone(),
            reasoning_effort: app.config.model_reasoning_effort.clone(),
        },
    );
    app.bind_spawn_nazgul_pane(manager_node);
    app.upsert_agent_picker_thread(
        parent_controlled_thread,
        Some("Task-only subagent".to_string()),
        Some("default".to_string()),
        /*is_closed*/ false,
    );
    app.agent_navigation
        .mark_parent_owned(parent_controlled_thread);
    let items = app.pane_picker_items();
    let names = items
        .iter()
        .map(|item| item.name.as_str())
        .collect::<Vec<_>>();
    assert!(names.contains(&"Corbanu Terminal - Codex manager"));
    assert!(names.contains(&"Corbanu Terminal - Codex worker"));
    assert!(
        !names.contains(&"Corbanu Terminal - Task-only subagent"),
        "managed workers stay out of user panes"
    );

    let user_index = names
        .iter()
        .position(|name| *name == "User Panes")
        .expect("user panes section");
    let create_index = names
        .iter()
        .position(|name| *name == "Create User Pane")
        .expect("user pane creation section");
    let crew_index = names
        .iter()
        .position(|name| *name == "Managed Crew (/spawn)")
        .expect("managed crew section");

    assert!(user_index < create_index);
    assert!(create_index < crew_index);
    assert!(
        items
            .iter()
            .find(|item| item.name.starts_with("Nazgul:"))
            .and_then(|item| item.description.as_deref())
            .is_some_and(|description| description.contains("same user pane listed above"))
    );
}

#[tokio::test]
async fn pane_picker_marks_exactly_the_active_native_thread_current() {
    let mut app = make_test_app().await;
    let main_thread = ThreadId::new();
    let user_thread = ThreadId::new();
    app.primary_thread_id = Some(main_thread);
    app.active_thread_id = Some(user_thread);
    app.upsert_agent_picker_thread(
        main_thread,
        Some("Main".to_string()),
        Some("default".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        user_thread,
        Some("Matrix Twin".to_string()),
        Some("default".to_string()),
        /*is_closed*/ false,
    );
    app.primary_session_configured = Some(ThreadSessionState {
        model: "deepseek-v4-flash".to_string(),
        ..test_thread_session(main_thread, test_path_buf("/tmp/main"))
    });
    app.agent_navigation
        .set_model(user_thread, Some("gpt-5.6-sol".to_string()));
    app.chat_widget.set_model("gpt-5.6-sol");

    let items = app.pane_picker_items();
    let main = items
        .iter()
        .find(|item| item.name == "Corbanu Terminal - Main")
        .expect("Main pane");
    let user = items
        .iter()
        .find(|item| item.name == "Corbanu Terminal - Matrix Twin")
        .expect("operator pane");

    assert!(!main.is_current, "inactive Main marked current");
    assert!(user.is_current, "active pane not current");
    assert_eq!(
        items.iter().filter(|item| item.is_current).count(),
        1,
        "expected one current pane"
    );
    assert_snapshot!(
        format!(
            "Main: {}\nMatrix Twin: {}",
            main.description.as_deref().expect("Main description"),
            user.description.as_deref().expect("operator pane description")
        ),
        @r###"
    Main: deepseek-v4-flash; idle
    Matrix Twin: gpt-5.6-sol; idle
    "###
    );
}

/// Drains native task events for `thread_id`, whether the target is a managed agent or an
/// operator-owned user pane (both take agents' tasks from the mailbox, D106). Returns the task
/// texts.
fn drain_spawn_agent_tasks_for(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    thread_id: ThreadId,
) -> Vec<String> {
    let mut matched = Vec::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            AppEvent::SubmitSpawnAgentTask {
                thread_id: t, task, ..
            } if t == thread_id => matched.push(task),
            _ => {}
        }
    }
    matched
}

/// Drains app events looking for a `SubmitSpawnAgentTask` for `thread_id`. Returns its task text.
fn drain_spawn_agent_task_for(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    thread_id: ThreadId,
) -> Option<String> {
    drain_spawn_agent_tasks_for(rx, thread_id).pop()
}

fn register_native_dispatch_pair(app: &mut App) -> (ThreadId, ThreadId) {
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000451").expect("valid thread id");
    let orc_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000452").expect("valid thread id");
    app.upsert_agent_picker_thread(
        troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        orc_thread_id,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_thread
        .insert(orc_thread_id, troll_thread_id);
    (troll_thread_id, orc_thread_id)
}

fn issue_1208_layout(edges: &[(&str, &str)]) -> crate::claude_panes::PaneLayoutState {
    crate::claude_panes::PaneLayoutState {
        spawn_parent_by_node: edges
            .iter()
            .map(|(child, parent)| (child.to_string(), parent.to_string()))
            .collect(),
        ..Default::default()
    }
}

const ISSUE_1208_MAIN: &str = "thread:00000000-0000-0000-0000-000000001208";
const ISSUE_1208_CHILD: &str = "thread:00000000-0000-0000-0000-000000001209";
const ISSUE_1208_GRANDCHILD: &str = "thread:00000000-0000-0000-0000-000000001210";

#[test]
fn model_spawned_sub_agents_alone_are_not_a_legacy_spawn_hierarchy() {
    // #1208: the spawn-agent tool records only thread -> thread edges.
    let native_only = issue_1208_layout(&[
        (ISSUE_1208_CHILD, ISSUE_1208_MAIN),
        (ISSUE_1208_GRANDCHILD, ISSUE_1208_CHILD),
    ]);
    assert_eq!(
        crate::spawn_orchestration::pane_layout_has_legacy_spawn_hierarchy(&native_only),
        false
    );

    // A /spawn Troll without a Nazgul binding hangs off the Main pane node.
    let legacy_pane_root = issue_1208_layout(&[
        (ISSUE_1208_CHILD, "pane:codex-main"),
        (ISSUE_1208_GRANDCHILD, ISSUE_1208_CHILD),
    ]);
    assert_eq!(
        crate::spawn_orchestration::pane_layout_has_legacy_spawn_hierarchy(&legacy_pane_root),
        true
    );

    // A /spawn hierarchy bound to a native Nazgul has only thread edges plus the binding.
    let legacy_bound_nazgul = crate::claude_panes::PaneLayoutState {
        spawn_nazgul_pane_id: Some(ISSUE_1208_MAIN.to_string()),
        ..issue_1208_layout(&[(ISSUE_1208_CHILD, ISSUE_1208_MAIN)])
    };
    assert_eq!(
        crate::spawn_orchestration::pane_layout_has_legacy_spawn_hierarchy(&legacy_bound_nazgul),
        true
    );

    // A /spawn Claude member is a pane node.
    let legacy_claude_member = issue_1208_layout(&[("pane:claude-1", ISSUE_1208_MAIN)]);
    assert_eq!(
        crate::spawn_orchestration::pane_layout_has_legacy_spawn_hierarchy(&legacy_claude_member),
        true
    );
}

#[tokio::test]
async fn resume_with_only_model_spawned_sub_agents_shows_no_legacy_spawn_notice() -> Result<()> {
    let native_only = issue_1208_layout(&[(ISSUE_1208_CHILD, ISSUE_1208_MAIN)]);
    let legacy = issue_1208_layout(&[(ISSUE_1208_CHILD, "pane:codex-main")]);
    for (layout, expect_notice) in [(native_only, false), (legacy, true)] {
        let (mut app, mut rx, _op_rx) = make_test_app_with_channels().await;
        // Set by hand as App construction does it in app.rs
        // (`restored_spawn_legacy_read_only`, the value given to
        // `spawn_legacy_read_only` when the App is built from the restored
        // layout). If that site stops calling
        // `pane_layout_has_legacy_spawn_hierarchy`, change this line too.
        app.spawn_legacy_read_only =
            crate::spawn_orchestration::pane_layout_has_legacy_spawn_hierarchy(&layout);
        let mut app_server = start_config_write_test_app_server(&app).await?;

        app.restore_native_spawn_panes_from_saved_state(&mut app_server)
            .await;

        let mut rendered = String::new();
        while let Ok(event) = rx.try_recv() {
            if let AppEvent::InsertHistoryCell(cell) = event {
                rendered.push_str(&lines_to_single_string(&cell.display_lines(/*width*/ 200)));
            }
        }
        assert_eq!(
            (
                app.spawn_legacy_read_only,
                rendered.contains("Restored a legacy /spawn hierarchy in read-only mode")
            ),
            (expect_notice, expect_notice),
            "layout edges {:?}; rendered: {rendered}",
            layout.spawn_parent_by_node
        );
    }
    Ok(())
}

#[tokio::test]
async fn restored_legacy_spawn_hierarchy_is_inspectable_but_rejects_mutation() {
    let (mut app, mut rx, _op_rx) = make_test_app_with_channels().await;
    let (source, target) = register_native_dispatch_pair(&mut app);
    app.spawn_legacy_read_only = true;

    app.dispatch_spawn_task_blocks(
        &thread_node_id(source),
        vec![crate::spawn_orchestration::SpawnTaskDispatch {
            target: target.to_string(),
            task: "must not enter the new control plane".to_string(),
            seq: Some(1),
        }],
    );

    while let Ok(event) = rx.try_recv() {
        assert!(
            !matches!(event, AppEvent::SubmitSpawnAgentTask { .. }),
            "legacy read-only sessions must never dispatch work"
        );
    }
}

#[tokio::test]
async fn restored_crew_validation_rejects_runtime_drift() {
    let mut app = make_test_app().await;
    let thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000499").expect("thread id");
    let node_id = thread_node_id(thread_id);
    app.upsert_agent_picker_thread(
        thread_id,
        Some("Manager".to_string()),
        Some("manager".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_node.insert(
        node_id.clone(),
        crate::spawn_orchestration::pane_node_id(CODEX_MAIN_PANE_ID),
    );
    app.spawn_native_runtime_by_node.insert(
        node_id.clone(),
        crate::dispatch_queue::SavedNativeSpawnRuntime {
            provider: "openai".to_string(),
            model: "gpt-5.6-sol".to_string(),
            // `None` in CrewSpec means use the provider/model default. Recovery records the
            // resolved effort and must not mistake that resolution for runtime drift.
            reasoning_effort: Some(ReasoningEffortConfig::High),
        },
    );
    let spec = codex_protocol::crew::CrewSpec {
        schema_version: codex_protocol::crew::CURRENT_CREW_SCHEMA_VERSION,
        crew_id: "validation-fixture".to_string(),
        preset_id: None,
        members: vec![codex_protocol::crew::CrewMemberSpec {
            logical_member_id: "manager".to_string(),
            display_name: "Manager".to_string(),
            role_profile: "manager".to_string(),
            parent_member_id: None,
            runtime_request: codex_protocol::crew::RuntimeRequest::exact(
                "openai",
                "gpt-5.6-sol",
                /*reasoning_effort*/ None,
            ),
        }],
        policy: codex_protocol::crew::CrewPolicy {
            delegation_mode: codex_protocol::crew::DelegationMode::ExplicitOnly,
            allow_ephemeral_descendants: true,
            provider_allowlist: vec!["openai".to_string()],
            maximum_spend_usd: None,
        },
    };
    let mut state = crate::crew_state::CrewInstanceState::begin(spec).expect("valid crew");
    state.record_member("manager", &node_id).expect("mapping");
    state.mark_ready().expect("ready");
    app.spawn_crew = Some(state);

    app.validate_restored_crew_state()
        .expect("exact restored mapping");
    app.spawn_native_runtime_by_node
        .get_mut(&node_id)
        .expect("runtime")
        .model = "different-model".to_string();
    let error = app
        .validate_restored_crew_state()
        .expect_err("runtime drift must stop recovery");
    assert!(error.to_string().contains("runtime changed"));
}

#[tokio::test]
async fn restored_spawn_resume_uses_saved_runtime_instead_of_parent_runtime() {
    let mut app = make_test_app().await;
    app.config.model = Some(codex_model_provider_info::CLAUDE_PLAN_MODEL.to_string());
    app.config.model_reasoning_effort = Some(ReasoningEffortConfig::High);
    let saved_runtime = crate::dispatch_queue::SavedNativeSpawnRuntime {
        provider: CLAUDE_PLAN_PROVIDER_ID.to_string(),
        model: codex_model_provider_info::CLAUDE_FABLE_5_PLAN_MODEL.to_string(),
        reasoning_effort: Some(ReasoningEffortConfig::High),
    };

    let (resume_config, model_override, permission_settings) =
        app.native_spawn_resume_request(Some(&saved_runtime));

    assert_eq!(
        resume_config.model.as_deref(),
        Some(codex_model_provider_info::CLAUDE_FABLE_5_PLAN_MODEL)
    );
    assert_eq!(
        resume_config.model_reasoning_effort,
        Some(ReasoningEffortConfig::High)
    );
    assert_eq!(
        model_override,
        Some(crate::app_server_session::ResumeModelOverride {
            model: Some(codex_model_provider_info::CLAUDE_FABLE_5_PLAN_MODEL.to_string()),
            model_provider: Some(CLAUDE_PLAN_PROVIDER_ID.to_string()),
        })
    );
    assert_eq!(
        permission_settings,
        crate::app_server_session::ResumePermissionSettings::RestoreFromThread
    );

    app.harness_overrides.sandbox_mode = Some(SandboxMode::WorkspaceWrite);
    app.harness_overrides.approval_policy =
        Some(codex_protocol::protocol::AskForApproval::OnRequest);
    let (_, _, permission_settings) = app.native_spawn_resume_request(Some(&saved_runtime));
    assert_eq!(
        permission_settings,
        crate::app_server_session::ResumePermissionSettings::OverrideFromCurrentConfig
    );
}

#[tokio::test]
async fn startup_resume_applies_persisted_runtime_before_bootstrap() {
    let mut app = make_test_app().await;
    let provider = app
        .config
        .model_providers
        .get(CLAUDE_PLAN_PROVIDER_ID)
        .expect("Claude Plan provider")
        .clone();
    app.config.model = Some("gpt-5.6-sol".to_string());
    app.config.model_provider_id = OPENAI_PROVIDER_ID.to_string();
    app.config.model_reasoning_effort = Some(ReasoningEffortConfig::XHigh);

    let model_override = apply_persisted_resume_runtime(
        &mut app.config,
        Some(codex_model_provider_info::CLAUDE_FABLE_5_PLAN_MODEL),
        CLAUDE_PLAN_PROVIDER_ID,
        Some(ReasoningEffortConfig::High),
    )
    .expect("persisted runtime");

    assert_eq!(
        app.config.model.as_deref(),
        Some(codex_model_provider_info::CLAUDE_FABLE_5_PLAN_MODEL)
    );
    assert_eq!(app.config.model_provider_id, CLAUDE_PLAN_PROVIDER_ID);
    assert_eq!(app.config.model_provider, provider);
    assert_eq!(
        app.config.model_reasoning_effort,
        Some(ReasoningEffortConfig::High)
    );
    assert_eq!(
        model_override,
        crate::app_server_session::ResumeModelOverride {
            model: Some(codex_model_provider_info::CLAUDE_FABLE_5_PLAN_MODEL.to_string()),
            model_provider: Some(CLAUDE_PLAN_PROVIDER_ID.to_string()),
        }
    );
}

#[tokio::test]
async fn manually_assembled_multimodel_spawn_crew_is_crewspec_backed() {
    let mut app = make_test_app().await;
    app.ensure_custom_spawn_root(crate::claude_panes::CODEX_MAIN_PANE_ID)
        .expect("bind custom root");
    app.record_custom_spawn_member(
        "thread:troll",
        crate::claude_panes::CODEX_MAIN_PANE_ID,
        crate::spawn_orchestration::SpawnRole::Troll,
        "Burzum".to_string(),
        crate::dispatch_queue::SavedNativeSpawnRuntime {
            model: "claude-opus-5-plan".to_string(),
            provider: "claude-plan".to_string(),
            reasoning_effort: None,
        },
    )
    .expect("add Opus Troll");
    app.record_custom_spawn_member(
        "thread:orc-kimi",
        "thread:troll",
        crate::spawn_orchestration::SpawnRole::Orc,
        "Snaga".to_string(),
        crate::dispatch_queue::SavedNativeSpawnRuntime {
            model: "k3".to_string(),
            provider: "kimi-code".to_string(),
            reasoning_effort: None,
        },
    )
    .expect("add Kimi Orc");

    let crew = app.spawn_crew.as_ref().expect("custom crew");
    assert!(matches!(
        crew.status,
        crate::crew_state::CrewCreationStatus::Ready
    ));
    assert_eq!(crew.spec.preset_id, None);
    assert_eq!(crew.spec.members.len(), 3);
    assert_eq!(
        crew.member_node_by_id
            .values()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["codex-main", "thread:orc-kimi", "thread:troll"]
    );
    crew.spec.validate().expect("custom crew stays valid");
    assert!(!app.spawn_legacy_read_only);
}

#[tokio::test]
async fn restored_crewspec_roles_drive_spawn_and_pane_projection_when_liveness_omits_roles() {
    let mut app = make_test_app().await;
    let main_thread_id = ThreadId::new();
    let nazgul_thread_id = ThreadId::new();
    let troll_thread_id = ThreadId::new();
    let orc_thread_id = ThreadId::new();
    let nazgul_node = crate::spawn_orchestration::thread_node_id(nazgul_thread_id);
    let troll_node = crate::spawn_orchestration::thread_node_id(troll_thread_id);
    let orc_node = crate::spawn_orchestration::thread_node_id(orc_thread_id);
    let runtime = crate::dispatch_queue::SavedNativeSpawnRuntime {
        model: "deepseek-v4-flash".to_string(),
        provider: "deepseek".to_string(),
        reasoning_effort: Some(ReasoningEffortConfig::High),
    };

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.spawn_nazgul_pane_id = Some(nazgul_node.clone());
    app.spawn_native_runtime_by_node
        .insert(nazgul_node.clone(), runtime.clone());
    app.ensure_custom_spawn_root(&nazgul_node)
        .expect("restore CrewSpec Nazgul");
    app.record_custom_spawn_member(
        &troll_node,
        &nazgul_node,
        crate::spawn_orchestration::SpawnRole::Troll,
        "Burzum".to_string(),
        runtime.clone(),
    )
    .expect("restore CrewSpec Troll");
    app.record_custom_spawn_member(
        &orc_node,
        &troll_node,
        crate::spawn_orchestration::SpawnRole::Orc,
        "Snaga".to_string(),
        runtime,
    )
    .expect("restore CrewSpec Orc");
    app.spawn_parent_by_node.insert(
        nazgul_node.clone(),
        crate::spawn_orchestration::pane_node_id(crate::claude_panes::CODEX_MAIN_PANE_ID),
    );
    app.spawn_parent_by_node
        .insert(troll_node.clone(), nazgul_node.clone());
    app.spawn_parent_by_node
        .insert(orc_node.clone(), troll_node.clone());
    for (node, thread_id, nickname) in [
        (&nazgul_node, nazgul_thread_id, "Angmar"),
        (&troll_node, troll_thread_id, "Burzum"),
        (&orc_node, orc_thread_id, "Snaga"),
    ] {
        app.spawn_native_endpoint_by_node
            .insert(node.clone(), thread_id);
        // `thread/list` and other liveness projections may omit role metadata. CrewSpec must
        // remain sufficient to render and operate every persistent member.
        app.upsert_agent_picker_thread(
            thread_id,
            Some(nickname.to_string()),
            /*agent_role*/ None,
            /*is_closed*/ false,
        );
    }

    let rows = app.spawn_tree_items(/*show_task_actions*/ false);

    let nazgul_row = rows
        .iter()
        .find(|row| row.name.contains("Nazgul: Angmar"))
        .expect("restored Nazgul row");
    let nazgul_search = nazgul_row
        .search_value
        .as_deref()
        .expect("Nazgul row must be searchable");
    assert!(nazgul_search.contains("Angmar"));
    assert!(nazgul_search.contains(nazgul_thread_id.to_string().as_str()));
    assert!(rows.iter().any(|row| row.name.contains("Burzum [troll]")));
    assert!(rows.iter().any(|row| row.name.contains("Snaga [orc]")));
    assert!(app.spawn_context_for_thread(troll_thread_id).is_some());
    assert!(app.spawn_context_for_thread(orc_thread_id).is_some());
}

#[tokio::test]
async fn custom_spawn_core_classes_match_persisted_crew_identity() {
    let mut app = make_test_app().await;
    let root_class = app
        .prepare_custom_spawn_root(
            "Angmar".to_string(),
            crate::dispatch_queue::SavedNativeSpawnRuntime {
                model: "deepseek-v4-flash".to_string(),
                provider: "deepseek".to_string(),
                reasoning_effort: Some(ReasoningEffortConfig::High),
            },
        )
        .expect("prepare custom root identity");
    let codex_protocol::crew::AgentClass::CrewMember {
        crew_id,
        logical_member_id,
        human_addressable,
    } = root_class
    else {
        panic!("custom root must be a persistent crew member");
    };
    assert_eq!(logical_member_id, "nazgul");
    assert!(human_addressable);
    assert_eq!(
        app.spawn_crew.as_ref().expect("prepared crew").spec.crew_id,
        crew_id
    );

    app.ensure_custom_spawn_root("thread:angmar")
        .expect("materialize prepared root identity");
    let troll_class = app
        .custom_spawn_member_agent_class(crate::spawn_orchestration::SpawnRole::Troll)
        .expect("allocate Troll identity from the same crew");
    let codex_protocol::crew::AgentClass::CrewMember {
        crew_id: troll_crew_id,
        logical_member_id: troll_member_id,
        human_addressable: troll_human_addressable,
    } = troll_class
    else {
        panic!("custom Troll must be a persistent crew member");
    };
    assert_eq!(troll_crew_id, crew_id);
    assert_eq!(troll_member_id, "troll-1");
    assert!(troll_human_addressable);

    app.record_custom_spawn_member(
        "thread:burzum",
        "thread:angmar",
        crate::spawn_orchestration::SpawnRole::Troll,
        "Burzum".to_string(),
        crate::dispatch_queue::SavedNativeSpawnRuntime {
            model: "claude-opus-5-plan".to_string(),
            provider: "claude-plan".to_string(),
            reasoning_effort: Some(ReasoningEffortConfig::High),
        },
    )
    .expect("persist Troll using the reserved Core identity");
    let crew = app.spawn_crew.as_ref().expect("ready custom crew");
    assert_eq!(
        crew.member_node_by_id
            .get(&troll_member_id)
            .map(String::as_str),
        Some("thread:burzum")
    );
    assert_eq!(crew.spec.crew_id, crew_id);
}

/// #435 (AGG-20, G12/G12b): `/spawn` → Troll before any Nazgul ended with
/// "Custom crew root was not created." and no reason. The Troll now starts the
/// crew rooted at its parent (the main pane), and a spawn that fails drops it.
#[tokio::test]
async fn sec_435_first_troll_starts_the_crew_at_its_parent() {
    use crate::spawn_orchestration::SpawnRole;
    let mut app = make_test_app().await;
    // The milestone failure.
    let err = app
        .custom_spawn_member_agent_class(SpawnRole::Troll)
        .expect_err("no crew yet");
    assert!(err.to_string().contains("spawn a Nazgul first"), "{err}");

    // The parent `/spawn` gives a Troll: the root node, `pane:codex-main`.
    let root = app.spawn_root_node_id();
    assert_eq!(root, format!("pane:{CODEX_MAIN_PANE_ID}"));
    let (class, started) = app
        .custom_spawn_member_agent_class_under(SpawnRole::Troll, &root)
        .expect("a first Troll starts the crew");
    assert!(started);
    let codex_protocol::crew::AgentClass::CrewMember {
        logical_member_id, ..
    } = class
    else {
        panic!("a Troll is a crew member");
    };
    assert_eq!(logical_member_id, "troll-1");
    let crew = app.spawn_crew.as_ref().expect("crew started");
    assert_eq!(
        crew.member_node_by_id.get("nazgul").map(String::as_str),
        Some(root.as_str())
    );

    // The spawn failed: the crew it started is dropped, so a Nazgul can
    // still be created afterwards.
    app.abort_started_custom_spawn_root();
    assert!(app.spawn_crew.is_none());
    app.preflight_new_custom_spawn_root()
        .expect("a Nazgul can still be created");

    // With a crew already present nothing new is started.
    app.ensure_custom_spawn_root(CODEX_MAIN_PANE_ID)
        .expect("bind custom root");
    let (_, started) = app
        .custom_spawn_member_agent_class_under(SpawnRole::Troll, CODEX_MAIN_PANE_ID)
        .expect("second Troll");
    assert!(!started);
}

#[tokio::test]
async fn native_task_agent_role_does_not_make_it_a_persistent_spawn_crew_member() {
    let mut app = make_test_app().await;
    let crew_orc = ThreadId::new();
    let task_agent = ThreadId::new();
    app.ensure_custom_spawn_root(crate::claude_panes::CODEX_MAIN_PANE_ID)
        .expect("bind custom root");
    app.upsert_agent_picker_thread(
        crew_orc,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        task_agent,
        Some("Ephemeral reviewer".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.record_custom_spawn_member(
        &crate::spawn_orchestration::thread_node_id(crew_orc),
        crate::claude_panes::CODEX_MAIN_PANE_ID,
        crate::spawn_orchestration::SpawnRole::Orc,
        "Snaga".to_string(),
        crate::dispatch_queue::SavedNativeSpawnRuntime {
            model: "k3".to_string(),
            provider: "kimi-code".to_string(),
            reasoning_effort: None,
        },
    )
    .expect("record durable crew member");

    assert!(app.is_managed_spawn_crew_thread(crew_orc));
    assert!(!app.is_managed_spawn_crew_thread(task_agent));
    let crew_rows = app.spawn_tree_items(/*show_task_actions*/ false);
    assert!(crew_rows.iter().any(|item| item.name.contains("Snaga")));
    assert!(
        crew_rows
            .iter()
            .all(|item| !item.name.contains("Ephemeral reviewer"))
    );
}

#[tokio::test]
async fn crewspec_restore_prunes_unrelated_native_tree_but_keeps_crew_descendants() {
    let mut app = make_test_app().await;
    let crew_orc = ThreadId::new();
    let crew_descendant = ThreadId::new();
    let unrelated_main = ThreadId::new();
    let unrelated_descendant = ThreadId::new();
    let crew_orc_node = thread_node_id(crew_orc);
    let crew_descendant_node = thread_node_id(crew_descendant);
    let unrelated_main_node = thread_node_id(unrelated_main);
    let unrelated_descendant_node = thread_node_id(unrelated_descendant);

    app.ensure_custom_spawn_root(CODEX_MAIN_PANE_ID)
        .expect("bind custom root");
    app.record_custom_spawn_member(
        &crew_orc_node,
        CODEX_MAIN_PANE_ID,
        crate::spawn_orchestration::SpawnRole::Orc,
        "Snaga".to_string(),
        crate::dispatch_queue::SavedNativeSpawnRuntime {
            model: "k3".to_string(),
            provider: "kimi-code".to_string(),
            reasoning_effort: None,
        },
    )
    .expect("record crew Orc");

    app.spawn_parent_by_node
        .insert(crew_orc_node.clone(), CODEX_MAIN_PANE_ID.to_string());
    app.spawn_parent_by_node
        .insert(crew_descendant_node.clone(), crew_orc_node.clone());
    app.spawn_parent_by_node.insert(
        unrelated_main_node.clone(),
        crate::spawn_orchestration::pane_node_id(CODEX_MAIN_PANE_ID),
    );
    app.spawn_parent_by_node.insert(
        unrelated_descendant_node.clone(),
        unrelated_main_node.clone(),
    );

    for (node_id, thread_id) in [
        (&crew_orc_node, crew_orc),
        (&crew_descendant_node, crew_descendant),
        (&unrelated_main_node, unrelated_main),
        (&unrelated_descendant_node, unrelated_descendant),
    ] {
        app.spawn_native_endpoint_by_node
            .insert(node_id.clone(), thread_id);
        app.spawn_native_runtime_by_node.insert(
            node_id.clone(),
            crate::dispatch_queue::SavedNativeSpawnRuntime {
                model: "fixture".to_string(),
                provider: "fixture".to_string(),
                reasoning_effort: None,
            },
        );
    }

    app.prune_noncrew_native_spawn_recovery_nodes();

    assert_eq!(
        app.spawn_parent_by_node.get(&crew_orc_node),
        Some(&CODEX_MAIN_PANE_ID.to_string())
    );
    assert_eq!(
        app.spawn_parent_by_node.get(&crew_descendant_node),
        Some(&crew_orc_node)
    );
    assert!(!app.spawn_parent_by_node.contains_key(&unrelated_main_node));
    assert!(
        !app.spawn_parent_by_node
            .contains_key(&unrelated_descendant_node)
    );
    assert!(
        app.spawn_native_endpoint_by_node
            .contains_key(&crew_descendant_node)
    );
    assert!(
        !app.spawn_native_endpoint_by_node
            .contains_key(&unrelated_main_node)
    );
    assert!(
        !app.spawn_native_runtime_by_node
            .contains_key(&unrelated_descendant_node)
    );
}

impl App {
    /// Test adapter for older unit fixtures. Production has no direct-dispatch bypass: this helper
    /// enters through the same stable model-origin path used by completed agent turns.
    fn dispatch_spawn_task_blocks(
        &mut self,
        source_pane_id: &str,
        dispatches: Vec<crate::spawn_orchestration::SpawnTaskDispatch>,
    ) {
        self.dispatch_spawn_task_blocks_from_model_turn(
            source_pane_id,
            source_pane_id,
            &ThreadId::new().to_string(),
            dispatches,
        );
    }
}

#[tokio::test]
async fn native_turn_completion_is_projection_only_and_never_schedules_tui_delivery() {
    let (mut app, mut rx, _op_rx) = make_test_app_with_channels().await;
    let (parent_thread_id, child_thread_id) = register_native_dispatch_pair(&mut app);

    app.handle_thread_event_now(ThreadBufferedEvent::Notification(Box::new(
        turn_completed_with_agent_message(
            child_thread_id,
            "turn-native-terminal",
            TurnStatus::Completed,
            "native terminal result",
        ),
    )));

    while let Ok(event) = rx.try_recv() {
        assert!(
            !matches!(
                event,
                AppEvent::SubmitSpawnAgentTask { .. }
                    | AppEvent::SendSpawnAgentMailboxMessage { .. }
            ),
            "native completion is already delivered by core and must not create a second TUI transport"
        );
    }
    let projected_reports = app
        .spawn_parent_reports_by_node
        .get(&thread_node_id(parent_thread_id))
        .expect("native completion should be projected into durable parent context");
    assert!(
        projected_reports
            .iter()
            .any(|report| report.contains("result=native terminal result")),
        "projection must retain the result without scheduling a second delivery"
    );
    assert_eq!(
        app.agent_navigation
            .get(&child_thread_id)
            .and_then(|entry| entry.last_result_message.as_deref()),
        Some("native terminal result"),
        "the TUI may project the native terminal state without transporting it"
    );
}

#[tokio::test]
async fn external_child_report_enters_native_parent_through_one_canonical_mailbox_message() {
    let (mut app, mut rx, _op_rx) = make_test_app_with_channels().await;
    let (parent_thread_id, _child_thread_id) = register_native_dispatch_pair(&mut app);
    let report = "child_report; seq=41; as_of=2026-07-25T18:00:00Z; child=External [orc]; status=done; result=verified";

    app.record_spawn_parent_report(thread_node_id(parent_thread_id), report.to_string());
    app.record_spawn_parent_report(thread_node_id(parent_thread_id), report.to_string());

    let mut mailbox_messages = Vec::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            AppEvent::SendSpawnAgentMailboxMessage { params } => mailbox_messages.push(params),
            AppEvent::SubmitSpawnAgentTask { .. } => {
                panic!("external reports must not use the obsolete synthetic-assignment path")
            }
            _ => {}
        }
    }
    assert_eq!(
        mailbox_messages.len(),
        1,
        "the edge adapter must deduplicate the report before entering the native mailbox"
    );
    let params = &mailbox_messages[0];
    assert_eq!(params.target_thread_id, parent_thread_id.to_string());
    assert_eq!(
        params.kind,
        codex_protocol::protocol::AgentMessageKind::TerminalResult
    );
    assert!(params.trigger_turn);
    assert_eq!(params.content, report);
    assert!(
        params
            .message_id
            .as_deref()
            .is_some_and(|message_id| message_id.starts_with("external-report:"))
    );
    assert_eq!(params.assignment_id, params.message_id);
}

#[tokio::test]
async fn spawn_roster_keeps_context_pressure_out_of_model_context() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    let (troll_thread_id, orc_thread_id) = register_native_dispatch_pair(&mut app);

    app.update_spawn_status_for_thread_notification(&token_usage_notification_with_total(
        orc_thread_id,
        "turn-context",
        /*total_tokens*/ 90_000,
        Some(100_000),
    ));

    let context = app
        .spawn_context_for_thread(troll_thread_id)
        .expect("Troll should receive spawn context");
    assert!(!context.contains("context_left="), "got context: {context}");
}

#[tokio::test]
async fn low_context_telemetry_does_not_manufacture_a_lifecycle_warning() {
    let (mut app, mut rx, _op_rx) = make_test_app_with_channels().await;
    let (troll_thread_id, orc_thread_id) = register_native_dispatch_pair(&mut app);
    app.spawn_operator_input_seen = true;
    let parent_node_id = crate::spawn_orchestration::thread_node_id(troll_thread_id);

    app.update_spawn_status_for_thread_notification(&token_usage_notification_with_total(
        orc_thread_id,
        "turn-low-1",
        /*total_tokens*/ 90_000,
        Some(100_000),
    ));
    let reports = app.spawn_parent_reports_by_node.get(&parent_node_id);
    assert!(reports.is_none_or(|reports| {
        reports
            .iter()
            .all(|report| !report.contains("low context") && !report.contains("journal/handoff"))
    }));
    assert!(drain_spawn_agent_tasks_for(&mut rx, troll_thread_id).is_empty());

    let context = app
        .spawn_context_for_thread(troll_thread_id)
        .expect("Troll should receive spawn context");
    assert!(!context.contains("context_left="));
}

#[tokio::test]
async fn spawn_roster_lines_carry_dispatch_and_report_seq() {
    let (mut app, mut rx, _op_rx) = make_test_app_with_channels().await;
    let (troll_thread_id, orc_thread_id) = register_native_dispatch_pair(&mut app);
    let source_node_id = crate::spawn_orchestration::thread_node_id(troll_thread_id);

    app.dispatch_spawn_task_blocks(
        &source_node_id,
        vec![crate::spawn_orchestration::SpawnTaskDispatch {
            target: orc_thread_id.to_string(),
            task: "sequence this work".to_string(),
            seq: Some(120),
        }],
    );
    assert!(matches!(
        rx.try_recv(),
        Ok(AppEvent::SubmitSpawnAgentTask { thread_id, .. }) if thread_id == orc_thread_id
    ));

    app.update_spawn_status_for_thread_notification(&turn_completed_with_agent_message(
        orc_thread_id,
        "turn-report-seq",
        TurnStatus::Completed,
        "sequence proof done",
    ));

    let context = app
        .spawn_context_for_thread(troll_thread_id)
        .expect("Troll should receive spawn context");
    assert!(context.contains("last_dispatch_seq=120"), "got: {context}");
    assert!(context.contains("last_report_seq=121"), "got: {context}");
    assert!(context.contains("as_of="), "got: {context}");

    let reports = app
        .spawn_parent_reports_by_node
        .get(&source_node_id)
        .expect("parent reports");
    assert!(reports.iter().any(|report| {
        report.contains("child_report; seq=121; as_of=") && report.contains("sequence proof done")
    }));
}

#[tokio::test]
async fn oversized_native_self_dispatch_is_rejected_without_delivery() {
    let (mut app, mut rx, _op_rx) = make_test_app_with_channels().await;
    let (source, _target) = register_native_dispatch_pair(&mut app);
    let source_node = thread_node_id(source);
    app.dispatch_spawn_task_blocks(
        &source_node,
        vec![crate::spawn_orchestration::SpawnTaskDispatch {
            target: source.to_string(),
            task: "x".repeat(crate::dispatch_queue::MAX_DISPATCH_TASK_BYTES + 10_000),
            seq: Some(503),
        }],
    );

    while let Ok(event) = rx.try_recv() {
        assert!(
            !matches!(
                event,
                AppEvent::SubmitSpawnAgentTask { .. } | AppEvent::SubmitSpawnClaudePaneTask { .. }
            ),
            "a rejected self-dispatch must not schedule delivery"
        );
    }
}

#[tokio::test]
async fn replacement_transaction_migrates_runtime_waiting_and_relationships() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    let (parent, old_thread) = register_native_dispatch_pair(&mut app);
    let new_thread =
        ThreadId::from_string("00000000-0000-0000-0000-000000000453").expect("thread id");
    app.upsert_agent_picker_thread(
        new_thread,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    let old_node = thread_node_id(old_thread);
    app.spawn_parent_by_node
        .insert(old_node.clone(), thread_node_id(parent));
    app.spawn_native_runtime_by_node.insert(
        old_node.clone(),
        crate::dispatch_queue::SavedNativeSpawnRuntime {
            model: "saved-custom-model".to_string(),
            provider: "saved-provider".to_string(),
            reasoning_effort: Some(codex_protocol::openai_models::ReasoningEffort::High),
        },
    );
    app.spawn_waiting_for_agents_by_thread
        .insert(old_thread, ("turn-old".to_string(), "wait-old".to_string()));
    app.replace_saved_native_spawn_thread(old_thread, new_thread);

    assert_eq!(
        app.spawn_native_runtime_by_node[&old_node].model,
        "saved-custom-model"
    );
    assert!(
        app.spawn_waiting_for_agents_by_thread
            .contains_key(&new_thread)
    );
    assert_eq!(app.spawn_parent_by_node[&old_node], thread_node_id(parent));
    assert_eq!(app.spawn_native_endpoint_by_node[&old_node], new_thread);
    assert_eq!(app.logical_native_node_for_thread(new_thread), old_node);
}

#[tokio::test]
async fn turn_start_failure_is_buffered_in_only_the_affected_pane() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    let affected = ThreadId::from_string("00000000-0000-0000-0000-000000000461")
        .expect("valid affected thread id");
    let unaffected = ThreadId::from_string("00000000-0000-0000-0000-000000000462")
        .expect("valid unaffected thread id");
    app.ensure_thread_channel(affected);
    app.ensure_thread_channel(unaffected);

    app.surface_turn_start_failure(
        affected,
        "turn/start failed in TUI: injected transport fault".to_string(),
        /*will_retry*/ false,
    )
    .await;

    {
        let affected_store = app
            .thread_event_channels
            .get(&affected)
            .expect("affected channel")
            .store
            .lock()
            .await;
        assert!(affected_store.buffer.iter().any(|event| matches!(
            event,
            ThreadBufferedEvent::Notification(notification)
                if matches!(
                    notification.as_ref(),
                    ServerNotification::Error(notification)
                        if !notification.will_retry
                            && notification.error.message.contains("pane remains available")
                            && notification.error.additional_details.as_deref()
                                == Some("turn/start failed in TUI: injected transport fault")
                )
        )));
    }

    let unaffected_store = app
        .thread_event_channels
        .get(&unaffected)
        .expect("unaffected channel")
        .store
        .lock()
        .await;
    assert!(unaffected_store.buffer.is_empty());
}

#[tokio::test]
async fn recovered_turn_start_failure_remains_visible_in_active_pane_history() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::from_string("00000000-0000-0000-0000-000000000463")
        .expect("valid active thread id");
    app.active_thread_id = Some(thread_id);

    app.surface_turn_start_failure(
        thread_id,
        "injected turn/start fault for qualification".to_string(),
        /*will_retry*/ true,
    )
    .await;

    let mut rendered = String::new();
    while let Ok(event) = app_event_rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            rendered.push_str(&lines_to_single_string(&cell.display_lines(/*width*/ 100)));
        }
    }
    assert!(rendered.contains("recovered after bounded retry"));
    assert!(rendered.contains("injected turn/start fault for qualification"));
}

#[tokio::test]
async fn failed_dispatch_records_durable_ack_without_fake_native_transport() {
    let (mut app, mut rx, _op_rx) = make_test_app_with_channels().await;
    let (troll_thread_id, _orc_thread_id) = register_native_dispatch_pair(&mut app);
    app.spawn_operator_input_seen = true;
    let source_node_id = crate::spawn_orchestration::thread_node_id(troll_thread_id);

    app.dispatch_spawn_task_blocks(
        &source_node_id,
        vec![crate::spawn_orchestration::SpawnTaskDispatch {
            target: "Missing Orc".to_string(),
            task: "handle the missing target".to_string(),
            seq: Some(78),
        }],
    );

    let sender_reports = app
        .spawn_parent_reports_by_node
        .get(&source_node_id)
        .expect("sender reports");
    assert!(sender_reports.iter().any(|report| {
        report.contains("dispatch_ack; #78") && report.contains("status=failed")
    }));
    let failure_ack_turns = drain_spawn_agent_tasks_for(&mut rx, troll_thread_id);
    assert!(
        failure_ack_turns.is_empty(),
        "native agents receive tool results from core; TUI must not synthesize a second turn"
    );
    assert!(
        !app.claude_pane_transcript_cells
            .contains_key(&source_node_id),
        "native sender errors must not be written as fake Claude-pane transcript cells"
    );
}

#[tokio::test]
async fn troll_spawn_task_submission_keeps_mailbox_payload_free_of_synthetic_context() {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000132").expect("valid thread id");
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000133").expect("valid thread id");
    let snaga_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000134").expect("valid thread id");
    let ghash_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000135").expect("valid thread id");

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.upsert_agent_picker_thread(
        troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        snaga_thread_id,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        ghash_thread_id,
        Some("Ghash".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_thread
        .insert(troll_thread_id, main_thread_id);
    app.spawn_parent_by_thread
        .insert(snaga_thread_id, troll_thread_id);
    app.spawn_parent_by_thread
        .insert(ghash_thread_id, troll_thread_id);

    app.agent_navigation
        .record_sub_agent_activity(SubAgentActivityDisplay {
            thread_id: snaga_thread_id,
            agent_path: "/root/troll_burzum/orc_snaga".to_string(),
            agent_nickname: None,
            agent_role: None,
            task_preview: None,
            is_running_hint: false,
        });
    app.agent_navigation
        .record_sub_agent_activity(SubAgentActivityDisplay {
            thread_id: ghash_thread_id,
            agent_path: "/root/troll_burzum/orc_ghash".to_string(),
            agent_nickname: None,
            agent_role: None,
            task_preview: None,
            is_running_hint: false,
        });

    let task =
        app.spawn_agent_task_for_submission(troll_thread_id, "Build the site and review it.");

    assert_eq!(task, "Build the site and review it.");
    let context = app
        .spawn_context_for_thread(troll_thread_id)
        .expect("Troll should receive live Core application context");
    assert!(context.contains("Snaga [orc]"));
    assert!(context.contains("/root/troll_burzum/orc_snaga"));
    assert!(context.contains("Ghash [orc]"));
    assert!(context.contains("followup_task"));
    assert!(context.contains("send_message"));
    assert!(!context.contains("<pfterminal_send_task"));
}

#[tokio::test]
async fn troll_live_context_reports_when_no_orcs_exist() {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000136").expect("valid thread id");
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000137").expect("valid thread id");

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.upsert_agent_picker_thread(
        troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_thread
        .insert(troll_thread_id, main_thread_id);

    let task =
        app.spawn_agent_task_for_submission(troll_thread_id, "Build the site and review it.");

    assert_eq!(task, "Build the site and review it.");
    let context = app
        .spawn_context_for_thread(troll_thread_id)
        .expect("Troll should receive live Core application context");
    assert!(context.contains("Orcs assigned to you:"));
    assert!(context.contains("none assigned yet"));
}

#[tokio::test]
async fn orc_spawn_task_submission_is_the_literal_user_assignment() {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000138").expect("valid thread id");
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000139").expect("valid thread id");
    let orc_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000140").expect("valid thread id");

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.upsert_agent_picker_thread(
        orc_thread_id,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_thread
        .insert(orc_thread_id, troll_thread_id);

    let task = app.spawn_agent_task_for_submission(
        orc_thread_id,
        "Run the acceptance command and report its literal output.",
    );

    assert_eq!(
        task,
        "Run the acceptance command and report its literal output."
    );
}

#[tokio::test]
async fn spawn_status_preserves_orc_activity_when_name_arrives_later() {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000129").expect("valid thread id");
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000130").expect("valid thread id");
    let orc_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000131").expect("valid thread id");

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.upsert_agent_picker_thread(
        troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_thread
        .insert(troll_thread_id, main_thread_id);
    app.spawn_parent_by_thread
        .insert(orc_thread_id, troll_thread_id);

    app.agent_navigation
        .record_sub_agent_activity(SubAgentActivityDisplay {
            thread_id: orc_thread_id,
            agent_path: "/root/troll_burzum/orc_snaga".to_string(),
            agent_nickname: None,
            agent_role: None,
            task_preview: Some("build the animated website shell".to_string()),
            is_running_hint: true,
        });
    app.upsert_agent_picker_thread(
        orc_thread_id,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );

    let entry = app
        .agent_navigation
        .get(&orc_thread_id)
        .expect("orc entry should be merged");
    assert_eq!(entry.agent_nickname.as_deref(), Some("Snaga"));
    assert_eq!(entry.agent_role.as_deref(), Some("orc"));
    assert_eq!(
        entry.agent_path.as_deref(),
        Some("/root/troll_burzum/orc_snaga")
    );
    assert_eq!(
        entry.last_task_message.as_deref(),
        Some("build the animated website shell")
    );

    let status_items = app.spawn_tree_items(/*show_task_actions*/ true);
    assert!(status_items.iter().any(|item| {
        item.name.contains("Snaga [orc]")
            && item.description.as_deref().is_some_and(|description| {
                description.contains("current task: build the animated website shell")
            })
    }));
}

#[tokio::test]
async fn native_spawn_turn_completion_updates_status_and_result_preview() {
    let mut app = make_test_app().await;
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000161").expect("valid thread id");
    let orc_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000162").expect("valid thread id");

    app.upsert_agent_picker_thread(
        troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        orc_thread_id,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_thread
        .insert(orc_thread_id, troll_thread_id);
    app.agent_navigation
        .set_last_task_message(orc_thread_id, Some("build components".to_string()));

    app.handle_thread_event_now(ThreadBufferedEvent::Notification(Box::new(
        turn_started_notification(orc_thread_id, "turn-1"),
    )));
    assert_eq!(
        app.spawn_status_by_thread
            .get(&orc_thread_id)
            .map(|state| &state.status),
        Some(&codex_app_server_protocol::CollabAgentStatus::Running)
    );

    app.handle_thread_event_now(ThreadBufferedEvent::Notification(Box::new(
        turn_completed_with_agent_message(
            orc_thread_id,
            "turn-1",
            TurnStatus::Completed,
            "Created the missing components and npm run build passed cleanly.",
        ),
    )));

    let status = app
        .spawn_status_by_thread
        .get(&orc_thread_id)
        .expect("spawn status should be cached");
    assert_eq!(
        status.status,
        codex_app_server_protocol::CollabAgentStatus::Completed
    );
    assert_eq!(
        status.message.as_deref(),
        Some("Created the missing components and npm run build passed cleanly.")
    );
    let entry = app
        .agent_navigation
        .get(&orc_thread_id)
        .expect("orc should stay in picker");
    assert!(!entry.is_running);
    assert_eq!(
        entry.last_result_message.as_deref(),
        Some("Created the missing components and npm run build passed cleanly.")
    );
    let status_items = app.spawn_tree_items(/*show_task_actions*/ true);
    assert!(status_items.iter().any(|item| {
        item.name.contains("Snaga [orc]")
            && item.description.as_deref().is_some_and(|description| {
                description.contains("completed")
                    && description.contains("latest result: Created the missing components")
                    && !description.starts_with("running")
            })
    }));
}

#[tokio::test]
async fn stale_receiver_running_status_does_not_hide_completed_orc_report() {
    let mut app = make_test_app().await;
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000163").expect("valid thread id");
    let orc_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000164").expect("valid thread id");

    app.upsert_agent_picker_thread(
        troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        orc_thread_id,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_thread
        .insert(orc_thread_id, troll_thread_id);
    app.spawn_parent_by_node.insert(
        crate::spawn_orchestration::thread_node_id(orc_thread_id),
        crate::spawn_orchestration::thread_node_id(troll_thread_id),
    );

    app.handle_thread_event_now(ThreadBufferedEvent::Notification(Box::new(
        turn_started_notification(orc_thread_id, "turn-1"),
    )));
    app.handle_thread_event_now(ThreadBufferedEvent::Notification(Box::new(
        turn_completed_with_agent_message(
            orc_thread_id,
            "turn-1",
            TurnStatus::Completed,
            "Wrote /tmp/pfterminal-orc-proof.txt and verified it.",
        ),
    )));

    app.handle_thread_event_now(ThreadBufferedEvent::Notification(Box::new(
        ServerNotification::ItemCompleted(codex_app_server_protocol::ItemCompletedNotification {
            thread_id: troll_thread_id.to_string(),
            turn_id: "turn-wait".to_string(),
            completed_at_ms: 0,
            item: ThreadItem::CollabAgentToolCall {
                id: "wait-1".to_string(),
                tool: codex_app_server_protocol::CollabAgentTool::Wait,
                status: codex_app_server_protocol::CollabAgentToolCallStatus::Completed,
                sender_thread_id: troll_thread_id.to_string(),
                receiver_thread_ids: vec![orc_thread_id.to_string()],
                prompt: None,
                model: None,
                reasoning_effort: None,
                provider_account: None,
                agents_states: HashMap::from([(
                    orc_thread_id.to_string(),
                    codex_app_server_protocol::CollabAgentState {
                        status: codex_app_server_protocol::CollabAgentStatus::Running,
                        message: None,
                    },
                )]),
            },
        }),
    )));

    let context = app
        .spawn_context_for_thread(troll_thread_id)
        .expect("Troll should receive live lifecycle context");
    assert!(
        context.contains("Snaga [orc]; status=completed"),
        "Troll roster must show the completed Orc, got: {context}"
    );
    assert!(
        !context.contains("Snaga [orc]; status=running"),
        "stale receiver status must not regress the completed Orc to running, got: {context}"
    );
    assert!(!context.contains("has_new_report="));
    assert!(!context.contains("Recent child reports delivered to this pane:"));
    assert!(!context.contains("result=Wrote /tmp/pfterminal-orc-proof.txt and verified it."));
}

#[tokio::test]
async fn native_orc_completion_is_projected_but_not_reinjected_into_parent_context() {
    let mut app = make_test_app().await;
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000231").expect("valid thread id");
    let orc_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000232").expect("valid thread id");

    app.upsert_agent_picker_thread(
        troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        orc_thread_id,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_thread
        .insert(orc_thread_id, troll_thread_id);
    app.agent_navigation
        .set_last_task_message(orc_thread_id, Some("audit latency hot paths".to_string()));

    app.enqueue_thread_notification(
        orc_thread_id,
        turn_completed_with_agent_message(
            orc_thread_id,
            "turn-1",
            TurnStatus::Completed,
            "Found two latency issues and no blockers.",
        ),
    )
    .await
    .expect("completion should enqueue");

    let context = app
        .spawn_context_for_thread(troll_thread_id)
        .expect("Troll should receive live lifecycle context");
    assert!(context.contains("Snaga [orc]; status=completed"));
    assert!(!context.contains("has_new_report="));
    assert!(!context.contains("Recent child reports delivered to this pane:"));
    assert!(!context.contains("result=Found two latency issues and no blockers."));
    let reports = app
        .spawn_parent_reports_by_node
        .get(&crate::spawn_orchestration::thread_node_id(troll_thread_id))
        .expect("TUI should retain a display projection of the result");
    assert!(
        reports
            .iter()
            .any(|report| report.contains("result=Found two latency issues and no blockers."))
    );
}

#[tokio::test]
async fn text_only_acknowledge_spawn_turn_reports_done() {
    let mut app = make_test_app().await;
    let (troll_thread_id, orc_thread_id) = register_native_dispatch_pair(&mut app);

    app.update_spawn_status_for_thread_notification(&turn_completed_with_agent_message(
        orc_thread_id,
        "turn-text-only-ack",
        TurnStatus::Completed,
        "Acknowledged. Standing by.",
    ));

    assert_eq!(
        app.spawn_status_by_thread
            .get(&orc_thread_id)
            .map(|state| &state.status),
        Some(&codex_app_server_protocol::CollabAgentStatus::Completed)
    );
    let parent_node_id = crate::spawn_orchestration::thread_node_id(troll_thread_id);
    let reports = app
        .spawn_parent_reports_by_node
        .get(&parent_node_id)
        .expect("parent report");
    assert!(reports.iter().any(|report| {
        report.contains("child_report;")
            && report.contains("status=completed")
            && report.contains("result=Acknowledged. Standing by.")
    }));
}

#[tokio::test]
async fn native_orc_completion_is_reported_to_claude_troll_context() {
    let mut app = make_test_app().await;
    let troll_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Troll),
            Some("Burzum".to_string()),
        )
        .expect("create Claude Troll pane");
    let orc_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000233").expect("valid thread id");

    app.upsert_agent_picker_thread(
        orc_thread_id,
        Some("Ghash".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_node.insert(
        crate::spawn_orchestration::thread_node_id(orc_thread_id),
        crate::spawn_orchestration::pane_node_id(&troll_pane_id),
    );

    app.enqueue_thread_notification(
        orc_thread_id,
        turn_completed_with_agent_message(
            orc_thread_id,
            "turn-1",
            TurnStatus::Completed,
            "Delivered the code-quality audit with three concrete findings.",
        ),
    )
    .await
    .expect("completion should enqueue");

    let context = app
        .spawn_context_for_user_pane(&troll_pane_id)
        .expect("Troll pane should receive spawn context");
    assert!(context.contains("Recent child reports delivered to this pane:"));
    assert!(context.contains("Ghash [orc]; status=completed; has_new_report=true"));
    assert!(
        context.contains("result=Delivered the code-quality audit with three concrete findings.")
    );
}

#[tokio::test]
async fn direct_six_orc_turn_reports_are_visible_to_claude_troll_context() {
    let mut app = make_test_app().await;
    let troll_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Troll),
            Some("Burzum".to_string()),
        )
        .expect("create Claude Troll pane");
    let native_orc_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000235").expect("valid thread id");
    app.upsert_agent_picker_thread(
        native_orc_thread_id,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_node.insert(
        crate::spawn_orchestration::thread_node_id(native_orc_thread_id),
        crate::spawn_orchestration::pane_node_id(&troll_pane_id),
    );

    let claude_orc_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Ghash".to_string()),
        )
        .expect("create Claude Orc pane");
    app.spawn_parent_by_node.insert(
        crate::spawn_orchestration::pane_node_id(&claude_orc_pane_id),
        crate::spawn_orchestration::pane_node_id(&troll_pane_id),
    );

    let turn_reports = [
        ("native", "turn-1", "ORC_A_TURN_1_DONE: alpha finding"),
        ("claude", "turn-2", "ORC_B_TURN_2_DONE: beta finding"),
        ("native", "turn-3", "ORC_A_TURN_3_DONE: gamma finding"),
        ("claude", "turn-4", "ORC_B_TURN_4_DONE: delta finding"),
        ("native", "turn-5", "ORC_A_TURN_5_DONE: epsilon finding"),
        ("claude", "turn-6", "ORC_B_TURN_6_DONE: zeta finding"),
    ];

    for (index, (harness, turn_id, report)) in turn_reports.iter().enumerate() {
        if *harness == "native" {
            app.enqueue_thread_notification(
                native_orc_thread_id,
                turn_completed_with_agent_message(
                    native_orc_thread_id,
                    turn_id,
                    TurnStatus::Completed,
                    report,
                ),
            )
            .await
            .expect("native Orc completion should enqueue");
        } else {
            app.on_claude_pane_turn_finished(
                claude_orc_pane_id.clone(),
                Ok(crate::claude_panes::ClaudePaneTurnOutput {
                    text: (*report).to_string(),
                    status: crate::claude_panes::ClaudePaneTurnStatus::Success,
                    session_id: Some("claude-session".to_string()),
                    usage_summary: None,
                    turn_usage_summary: None,
                    usage_status: crate::claude_panes::ClaudePaneUsageStatus::Missing,
                    artifact_path: app
                        .config
                        .cwd
                        .join(format!("{turn_id}.jsonl"))
                        .to_path_buf(),
                    audit_path: app
                        .config
                        .cwd
                        .join(format!("{turn_id}.audit.json"))
                        .to_path_buf(),
                    duration_ms: 1,
                    terminal_reason: None,
                    error_summary: None,
                    tool_names: Vec::new(),
                    tool_events: Vec::new(),
                    reasoning_events: Vec::new(),
                    command_mode: crate::claude_panes::ClaudeCommandMode::NewSession,
                    direct_accounting: None,
                }),
            );
        }

        let context = app
            .spawn_context_for_user_pane(&troll_pane_id)
            .expect("Troll pane should receive spawn context");
        assert!(context.contains("Recent child reports delivered to this pane:"));
        for (_, _, expected_report) in &turn_reports[..=index] {
            assert!(context.contains(expected_report));
        }
    }

    let context = app
        .spawn_context_for_user_pane(&troll_pane_id)
        .expect("Troll pane should receive spawn context");
    assert!(context.contains("Snaga [orc]; status=completed; has_new_report=true"));
    assert!(context.contains("Claude Code Ghash [orc] - Opus 5.5 Claude Plan; status=success"));
}

#[tokio::test]
async fn enqueued_native_spawn_turn_completion_updates_status_before_replay() {
    let mut app = make_test_app().await;
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000165").expect("valid thread id");
    let orc_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000166").expect("valid thread id");

    app.upsert_agent_picker_thread(
        troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        orc_thread_id,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_thread
        .insert(orc_thread_id, troll_thread_id);
    app.agent_navigation
        .set_last_task_message(orc_thread_id, Some("write proof file".to_string()));

    app.enqueue_thread_notification(
        orc_thread_id,
        turn_completed_with_agent_message(
            orc_thread_id,
            "turn-1",
            TurnStatus::Completed,
            "Wrote /tmp/pfterminal-spawn-status-proof.txt and verified it.",
        ),
    )
    .await
    .expect("completion should enqueue");

    let status = app
        .spawn_status_by_thread
        .get(&orc_thread_id)
        .expect("spawn status should be cached before replay");
    assert_eq!(
        status.status,
        codex_app_server_protocol::CollabAgentStatus::Completed
    );
    assert_eq!(
        status.message.as_deref(),
        Some("Wrote /tmp/pfterminal-spawn-status-proof.txt and verified it.")
    );

    let status_items = app.spawn_tree_items(/*show_task_actions*/ true);
    assert!(status_items.iter().any(|item| {
        item.name.contains("Snaga [orc]")
            && item.description.as_deref().is_some_and(|description| {
                description.contains("completed")
                    && description
                        .contains("latest result: Wrote /tmp/pfterminal-spawn-status-proof")
                    && !description.starts_with("idle")
            })
    }));
}

#[tokio::test]
async fn native_spawn_turn_interrupt_updates_status_without_closing_app() {
    let mut app = make_test_app().await;
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000163").expect("valid thread id");
    let orc_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000164").expect("valid thread id");

    app.upsert_agent_picker_thread(
        troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        orc_thread_id,
        Some("Ghash".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_thread
        .insert(orc_thread_id, troll_thread_id);

    app.handle_thread_event_now(ThreadBufferedEvent::Notification(Box::new(
        turn_started_notification(orc_thread_id, "turn-1"),
    )));
    app.handle_thread_event_now(ThreadBufferedEvent::Notification(Box::new(
        turn_completed_notification(orc_thread_id, "turn-1", TurnStatus::Interrupted),
    )));

    let status = app
        .spawn_status_by_thread
        .get(&orc_thread_id)
        .expect("spawn status should be cached");
    assert_eq!(
        status.status,
        codex_app_server_protocol::CollabAgentStatus::Interrupted
    );
    let entry = app
        .agent_navigation
        .get(&orc_thread_id)
        .expect("orc should stay in picker");
    assert!(!entry.is_running);

    let status_items = app.spawn_tree_items(/*show_task_actions*/ true);
    assert!(status_items.iter().any(|item| {
        item.name.contains("Ghash [orc]")
            && item.description.as_deref().is_some_and(|description| {
                description.starts_with("interrupted") && !description.starts_with("running")
            })
    }));
}

#[tokio::test]
async fn bound_claude_nazgul_context_explains_empty_spawn_hierarchy() {
    let mut app = make_test_app().await;
    app.spawn_nazgul_pane_id = Some("claude-test-pane".to_string());

    let context = app
        .spawn_context_for_user_pane("claude-test-pane")
        .expect("bound Claude pane should receive spawn context");

    assert!(context.contains("You are the Corbanu Terminal Nazgul/root pane"));
    assert!(context.contains("Troll and Orc are Corbanu Terminal orchestration roles"));
    assert!(context.contains("Trolls: none spawned yet."));
    assert!(context.contains("Orcs: none spawned yet."));
    assert!(context.contains("suggest using /spawn"));
    assert!(app.spawn_context_for_user_pane("other-pane").is_none());
}

#[tokio::test]
async fn bound_claude_nazgul_context_lists_named_trolls_and_orcs() {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000132").expect("valid thread id");
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000133").expect("valid thread id");
    let orc_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000134").expect("valid thread id");

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.spawn_nazgul_pane_id = Some("claude-nazgul".to_string());
    app.upsert_agent_picker_thread(
        troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        orc_thread_id,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_thread
        .insert(troll_thread_id, main_thread_id);
    app.spawn_parent_by_thread
        .insert(orc_thread_id, troll_thread_id);
    app.agent_navigation.set_last_task_message(
        orc_thread_id,
        Some("review the large diff before reporting".to_string()),
    );
    app.agent_navigation.set_last_result_message(
        orc_thread_id,
        Some("found a missing hierarchy context bridge".to_string()),
    );

    let context = app
        .spawn_context_for_user_pane("claude-nazgul")
        .expect("bound Claude pane should receive spawn context");

    assert!(context.contains("Burzum [troll]; status=idle"));
    assert!(context.contains("Snaga [orc]; status=idle"));
    assert!(context.contains("current_task=review the large diff before reporting"));
    assert!(context.contains("latest_result=found a missing hierarchy context bridge"));
}

#[tokio::test]
async fn bound_claude_nazgul_context_auto_nests_orphans_under_single_claude_troll() {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000135").expect("valid thread id");
    let orc_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000136").expect("valid thread id");

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.spawn_nazgul_pane_id = Some("claude-nazgul".to_string());
    let troll_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Troll),
            Some("Burzum".to_string()),
        )
        .expect("create Claude Troll pane");
    app.spawn_parent_by_node.insert(
        crate::spawn_orchestration::pane_node_id(&troll_pane_id),
        crate::spawn_orchestration::pane_node_id("claude-nazgul"),
    );
    app.upsert_agent_picker_thread(
        orc_thread_id,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_thread
        .insert(orc_thread_id, main_thread_id);

    let context = app
        .spawn_context_for_user_pane("claude-nazgul")
        .expect("bound Claude pane should receive spawn context");

    assert!(context.contains("Claude Code Burzum [troll] - Opus 5.5 Claude Plan; role=Troll"));
    assert!(context.contains("  - Snaga [orc]; status=idle"));
    assert!(!context.contains("Unassigned Orcs"));
}

#[tokio::test]
async fn claude_orc_completion_is_reported_to_parent_troll_context() {
    let mut app = make_test_app().await;
    let troll_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Troll),
            Some("Burzum".to_string()),
        )
        .expect("create Claude Troll pane");
    let orc_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Snaga".to_string()),
        )
        .expect("create Claude Orc pane");
    app.spawn_parent_by_node.insert(
        crate::spawn_orchestration::pane_node_id(&orc_pane_id),
        crate::spawn_orchestration::pane_node_id(&troll_pane_id),
    );

    let artifact_path = app.config.cwd.join("turn-0001.jsonl").to_path_buf();
    let audit_path = app.config.cwd.join("turn-0001.audit.json").to_path_buf();
    app.on_claude_pane_turn_finished(
        orc_pane_id,
        Ok(crate::claude_panes::ClaudePaneTurnOutput {
            text: "Implemented the mock website and npm run build passed.".to_string(),
            status: crate::claude_panes::ClaudePaneTurnStatus::Success,
            session_id: Some("claude-session".to_string()),
            usage_summary: None,
            turn_usage_summary: None,
            usage_status: crate::claude_panes::ClaudePaneUsageStatus::Missing,
            artifact_path,
            audit_path,
            duration_ms: 1,
            terminal_reason: None,
            error_summary: None,
            tool_names: Vec::new(),
            tool_events: Vec::new(),
            reasoning_events: Vec::new(),
            command_mode: crate::claude_panes::ClaudeCommandMode::NewSession,
            direct_accounting: None,
        }),
    );

    let context = app
        .spawn_context_for_user_pane(&troll_pane_id)
        .expect("Troll pane should receive spawn context");
    assert!(context.contains("Recent child reports delivered to this pane:"));
    assert!(context.contains("Claude Code Snaga [orc] - Opus 5.5 Claude Plan"));
    assert!(context.contains("status=idle"));
    assert!(context.contains("result=Implemented the mock website and npm run build passed."));
}

async fn make_child_report_auto_claude_pane_app() -> (
    App,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    String,
    String,
) {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    app.spawn_operator_input_seen = true;
    let troll_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Troll),
            Some("Burzum".to_string()),
        )
        .expect("create Claude Troll pane");
    let orc_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Snaga".to_string()),
        )
        .expect("create Claude Orc pane");
    app.spawn_parent_by_node.insert(
        crate::spawn_orchestration::pane_node_id(&orc_pane_id),
        crate::spawn_orchestration::pane_node_id(&troll_pane_id),
    );
    while app_event_rx.try_recv().is_ok() {}
    (app, app_event_rx, troll_pane_id, orc_pane_id)
}

fn drain_claude_pane_task_events(
    app_event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) -> Vec<(String, String)> {
    let mut submitted_tasks = Vec::new();
    while let Ok(event) = app_event_rx.try_recv() {
        if let AppEvent::SubmitSpawnClaudePaneTask { pane_id, task, .. } = event {
            submitted_tasks.push((pane_id, task));
        }
    }
    submitted_tasks
}

fn write_test_whip(app: &App, name: &str, body: &str) {
    let dir = app.config.codex_home.join("whips");
    std::fs::create_dir_all(&dir).expect("create whips dir");
    std::fs::write(dir.join(format!("{name}.md")), body).expect("write whip");
}

#[tokio::test]
async fn orchestrate_auto_whip_fires_once_for_idle_sweep() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(&app, "keep-going", "# whip: keep-going\nContinue the work.");
    let pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Krimp".to_string()),
        )
        .expect("create pane");

    app.handle_orchestrate_command(format!(
        "attach {pane_id} keep-going --mode auto --holder none --max 3"
    ));
    app.sweep_orchestrate_whips();
    app.sweep_orchestrate_whips();

    let submitted_tasks = drain_claude_pane_task_events(&mut app_event_rx);
    assert_eq!(submitted_tasks.len(), 1);
    assert_eq!(submitted_tasks[0].0, pane_id);
    assert!(submitted_tasks[0].1.contains("Whip #whip-1 fire 1/3"));
    assert!(submitted_tasks[0].1.contains("Continue the work."));
}

#[tokio::test]
async fn native_legacy_whip_target_receives_orchestration_lifecycle() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(&app, "keep-going", "# whip: keep-going\nContinue the work.");
    let worker_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000467").expect("worker id");
    app.upsert_agent_picker_thread(
        worker_thread_id,
        Some("Worker".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    let worker_node = crate::spawn_orchestration::thread_node_id(worker_thread_id);

    app.handle_orchestrate_command(format!(
        "attach {worker_node} keep-going --mode auto --holder none --max 2"
    ));

    assert!(
        app.is_spawn_orchestration_thread(worker_thread_id),
        "an armed native legacy-whip target must receive completion lifecycle events"
    );
    app.orchestrate_whips
        .get_mut("whip-1")
        .expect("legacy whip")
        .state = crate::orchestrate::WhipState::Exhausted;
    assert!(
        app.is_spawn_orchestration_thread(worker_thread_id),
        "an exhausted whip target must remain classified until its terminal turn arrives"
    );
}

#[tokio::test]
async fn orchestrate_stop_marker_pauses_before_fire() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(
        &app,
        "stop-aware",
        "# whip: stop-aware\nContinue until done.",
    );
    let pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Krimp".to_string()),
        )
        .expect("create pane");
    let target_node_id = crate::spawn_orchestration::pane_node_id(&pane_id);

    app.handle_orchestrate_command(format!(
        "attach {pane_id} stop-aware --mode auto --holder none --max 3"
    ));
    app.note_whip_target_idle_with_fire_control(
        &target_node_id,
        Some("done\nWHIP_DONE"),
        /*allow_fire*/ true,
        /*turn_succeeded*/ true,
    );

    assert!(drain_claude_pane_task_events(&mut app_event_rx).is_empty());
    assert_eq!(
        app.orchestrate_whips.get("whip-1").map(|whip| whip.state),
        Some(crate::orchestrate::WhipState::Paused)
    );
}

#[tokio::test]
async fn orchestrate_empty_output_loop_pauses_whip() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(
        &app,
        "loop-aware",
        "# whip: loop-aware\nContinue unless spinning.",
    );
    let pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Krimp".to_string()),
        )
        .expect("create pane");
    let target_node_id = crate::spawn_orchestration::pane_node_id(&pane_id);

    app.handle_orchestrate_command(format!(
        "attach {pane_id} loop-aware --mode auto --holder none --max 3 --cooldown 1s"
    ));
    app.note_whip_target_idle_with_fire_control(
        &target_node_id,
        Some(""),
        /*allow_fire*/ true,
        /*turn_succeeded*/ true,
    );
    app.note_whip_target_idle_with_fire_control(
        &target_node_id,
        Some("   "),
        /*allow_fire*/ true,
        /*turn_succeeded*/ true,
    );

    let submitted_tasks = drain_claude_pane_task_events(&mut app_event_rx);
    assert_eq!(submitted_tasks.len(), 1);
    assert_eq!(
        app.orchestrate_whips.get("whip-1").map(|whip| whip.state),
        Some(crate::orchestrate::WhipState::Paused)
    );
}

#[tokio::test]
async fn orchestrate_failed_turn_loop_pauses_and_success_resets_streak() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(
        &app,
        "failure-aware",
        "# whip: failure-aware\nContinue the work.",
    );
    let pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Krimp".to_string()),
        )
        .expect("create pane");
    let target_node_id = crate::spawn_orchestration::pane_node_id(&pane_id);

    app.handle_orchestrate_command(format!(
        "attach {pane_id} failure-aware --mode auto --holder none --max 5 --cooldown 1s"
    ));
    app.note_whip_target_idle_with_fire_control(
        &target_node_id,
        Some("provider error"),
        /*allow_fire*/ true,
        /*turn_succeeded*/ false,
    );
    app.note_whip_target_idle_with_fire_control(
        &target_node_id,
        Some("recovered"),
        /*allow_fire*/ true,
        /*turn_succeeded*/ true,
    );
    app.note_whip_target_idle_with_fire_control(
        &target_node_id,
        Some("provider error"),
        /*allow_fire*/ true,
        /*turn_succeeded*/ false,
    );
    assert_eq!(
        app.orchestrate_whips
            .get("whip-1")
            .map(|whip| whip.consecutive_failed_turns),
        Some(1)
    );

    app.note_whip_target_idle_with_fire_control(
        &target_node_id,
        Some("provider error"),
        /*allow_fire*/ true,
        /*turn_succeeded*/ false,
    );
    let whip = app.orchestrate_whips.get("whip-1").expect("whip");
    assert_eq!(whip.state, crate::orchestrate::WhipState::Paused);
    assert!(whip.fires <= 2);
    assert!(drain_claude_pane_task_events(&mut app_event_rx).len() <= 2);
}

#[tokio::test]
async fn assignment_overnight_loop_survives_cycles_backoff_and_manager_markers() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(
        &app,
        "overnight",
        "# assignment: overnight\nShip the flagship.",
    );
    let manager_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Troll),
            Some("Manager".to_string()),
        )
        .expect("create manager");
    let worker_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Worker".to_string()),
        )
        .expect("create worker");
    let manager_node = crate::spawn_orchestration::pane_node_id(&manager_pane_id);
    let worker_node = crate::spawn_orchestration::pane_node_id(&worker_pane_id);
    let started = chrono::DateTime::parse_from_rfc3339("2026-07-10T20:00:00Z")
        .expect("timestamp")
        .with_timezone(&chrono::Utc);
    app.orchestrate_now_override = Some(started);

    app.handle_orchestrate_command(format!(
        "attach {worker_pane_id} overnight --mode review --holder {manager_pane_id} --for 8h --cooldown 900s"
    ));
    assert!(matches!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|whip| &whip.kind),
        Some(crate::orchestrate::WhipKind::Assignment {
            phase: crate::orchestrate::AssignmentPhase::Drafting,
            ..
        })
    ));
    let birth_tasks = drain_claude_pane_task_events(&mut app_event_rx);
    assert_eq!(birth_tasks.len(), 1);
    assert!(birth_tasks[0].1.contains("The spec below is locked."));
    assert!(
        birth_tasks[0]
            .1
            .contains("Send the first concrete Worker task now")
    );
    assert!(birth_tasks[0].1.contains(&worker_node));
    assert!(birth_tasks[0].1.contains("wait for the Worker result"));
    assert!(birth_tasks[0].1.contains("not a shell command or tool"));
    assert!(!birth_tasks[0].1.contains("First iterate with the user"));
    app.note_whip_target_idle_with_fire_control(
        &manager_node,
        Some("I will emit WHIP_DONE when complete and ASSIGNMENT_BLOCKED: <reason> if needed."),
        /*allow_fire*/ false,
        /*turn_succeeded*/ true,
    );
    assert!(matches!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|whip| &whip.kind),
        Some(crate::orchestrate::WhipKind::Assignment {
            phase: crate::orchestrate::AssignmentPhase::Drafting,
            ..
        })
    ));
    app.sweep_orchestrate_whips();
    assert!(drain_claude_pane_task_events(&mut app_event_rx).is_empty());

    app.note_whip_holder_dispatched(&manager_node, &worker_node);
    assert!(matches!(
        app.orchestrate_whips.get("assignment-1").map(|whip| &whip.kind),
        Some(crate::orchestrate::WhipKind::Assignment {
            phase: crate::orchestrate::AssignmentPhase::Executing,
            execution_started_utc: Some(value),
            ..
        }) if *value == started
    ));
    assert_eq!(
        app.orchestrate_whips
            .get("assignment-1")
            .and_then(|whip| whip.expires_at),
        Some(started + chrono::Duration::hours(8))
    );
    app.note_assignment_user_turn(&manager_node);
    app.note_whip_target_idle_with_fire_control(
        &worker_node,
        Some("WORKER_AUDIT_RESULT: baseline checkout is invalid"),
        /*allow_fire*/ false,
        /*turn_succeeded*/ true,
    );
    assert!(
        drain_claude_pane_task_events(&mut app_event_rx).is_empty(),
        "a Worker completion must not preempt recent user activity"
    );
    if let Some(whip) = app.orchestrate_whips.get_mut("assignment-1") {
        whip.last_fire_utc = Some(started);
        if let crate::orchestrate::WhipKind::Assignment {
            last_user_turn_utc, ..
        } = &mut whip.kind
        {
            *last_user_turn_utc = None;
        }
    }
    app.orchestrate_now_override = Some(started + chrono::Duration::milliseconds(500));
    app.sweep_orchestrate_whips();
    assert!(
        drain_claude_pane_task_events(&mut app_event_rx).is_empty(),
        "completion handoffs must be rate limited"
    );
    app.orchestrate_now_override = Some(started + chrono::Duration::seconds(3));
    app.sweep_orchestrate_whips();
    assert_eq!(
        drain_claude_pane_task_events(&mut app_event_rx).len(),
        1,
        "the pending completion must retry after the rate-limit window"
    );
    let assignment = app
        .orchestrate_whips
        .get("assignment-1")
        .expect("assignment");
    let mandate = crate::orchestrate::assignment_mandate_task(
        assignment,
        "Worker",
        Some("Run the audit."),
        /*path*/ None,
        started,
        crate::orchestrate::AssignmentDispatchProtocol::HostAdapter,
    );
    assert!(mandate.contains("Worker's latest completed output:"));
    assert!(mandate.contains("WORKER_AUDIT_RESULT: baseline checkout is invalid"));
    for cycle in 1..=24 {
        app.orchestrate_now_override =
            Some(started + chrono::Duration::seconds(3) + chrono::Duration::seconds(901 * cycle));
        app.sweep_orchestrate_whips();
    }
    assert_eq!(drain_claude_pane_task_events(&mut app_event_rx).len(), 24);
    let assignment = app
        .orchestrate_whips
        .get("assignment-1")
        .expect("assignment");
    assert_eq!(assignment.state, crate::orchestrate::WhipState::Armed);
    assert_eq!(assignment.fires, 25); // one completion retry plus 24 watchdog cycles

    app.note_whip_target_idle_with_fire_control(
        &manager_node,
        Some("temporary provider failure"),
        /*allow_fire*/ false,
        /*turn_succeeded*/ false,
    );
    app.note_whip_target_idle_with_fire_control(
        &manager_node,
        Some("temporary provider failure"),
        /*allow_fire*/ false,
        /*turn_succeeded*/ false,
    );
    assert_eq!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(crate::orchestrate::assignment_effective_cadence_s),
        Some(3600)
    );
    let backed_off_now = started + chrono::Duration::hours(7);
    app.orchestrate_now_override = Some(backed_off_now);
    if let Some(whip) = app.orchestrate_whips.get_mut("assignment-1") {
        whip.last_fire_utc = Some(backed_off_now - chrono::Duration::seconds(3601));
        if let crate::orchestrate::WhipKind::Assignment {
            last_user_turn_utc, ..
        } = &mut whip.kind
        {
            *last_user_turn_utc = Some(backed_off_now - chrono::Duration::minutes(20));
        }
    }
    app.sweep_orchestrate_whips();
    assert_eq!(drain_claude_pane_task_events(&mut app_event_rx).len(), 1);
    app.note_whip_target_idle_with_fire_control(
        &manager_node,
        Some("recovered"),
        /*allow_fire*/ false,
        /*turn_succeeded*/ true,
    );
    assert_eq!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(crate::orchestrate::assignment_effective_cadence_s),
        Some(900)
    );

    app.note_whip_target_idle_with_fire_control(
        &worker_node,
        Some("ASSIGNMENT_BLOCKED: worker said it\nWHIP_DONE"),
        /*allow_fire*/ false,
        /*turn_succeeded*/ true,
    );
    assert!(matches!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|whip| &whip.kind),
        Some(crate::orchestrate::WhipKind::Assignment {
            phase: crate::orchestrate::AssignmentPhase::Executing,
            ..
        })
    ));
    app.note_whip_target_idle_with_fire_control(
        &manager_node,
        Some("I may emit WHIP_DONE later and mention ASSIGNMENT_BLOCKED: in prose."),
        /*allow_fire*/ false,
        /*turn_succeeded*/ true,
    );
    assert!(matches!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|whip| &whip.kind),
        Some(crate::orchestrate::WhipKind::Assignment {
            phase: crate::orchestrate::AssignmentPhase::Executing,
            ..
        })
    ));
    app.note_whip_target_idle_with_fire_control(
        &manager_node,
        Some("ASSIGNMENT_BLOCKED:\nprogress continues"),
        /*allow_fire*/ false,
        /*turn_succeeded*/ true,
    );
    assert!(matches!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|whip| &whip.kind),
        Some(crate::orchestrate::WhipKind::Assignment {
            phase: crate::orchestrate::AssignmentPhase::Executing,
            ..
        })
    ));
    app.note_whip_target_idle_with_fire_control(
        &manager_node,
        Some("progress\nASSIGNMENT_BLOCKED: waiting for production credentials"),
        /*allow_fire*/ false,
        /*turn_succeeded*/ true,
    );
    assert!(matches!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|whip| &whip.kind),
        Some(crate::orchestrate::WhipKind::Assignment {
            phase: crate::orchestrate::AssignmentPhase::Blocked { .. },
            ..
        })
    ));
    app.note_assignment_user_turn(&manager_node);
    assert!(matches!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|whip| &whip.kind),
        Some(crate::orchestrate::WhipKind::Assignment {
            phase: crate::orchestrate::AssignmentPhase::Executing,
            ..
        })
    ));
    app.note_whip_target_idle_with_fire_control(
        &manager_node,
        Some("WHIP_DONE"),
        /*allow_fire*/ false,
        /*turn_succeeded*/ true,
    );
    assert!(matches!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|whip| &whip.kind),
        Some(crate::orchestrate::WhipKind::Assignment {
            phase: crate::orchestrate::AssignmentPhase::Done,
            ..
        })
    ));
}

#[tokio::test]
async fn assignment_manager_empty_completion_retries_current_turn_once_then_pauses() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(&app, "empty-manager", "Draft and run the recovery audit.");
    let manager_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000463").expect("manager id");
    let worker_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000464").expect("worker id");
    app.upsert_agent_picker_thread(
        manager_thread_id,
        Some("Manager".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        worker_thread_id,
        Some("Worker".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    let manager_node = crate::spawn_orchestration::thread_node_id(manager_thread_id);
    let worker_node = crate::spawn_orchestration::thread_node_id(worker_thread_id);
    app.handle_orchestrate_command(format!(
        "attach {worker_node} empty-manager --mode review --holder {manager_node} --for 1h"
    ));
    while app_event_rx.try_recv().is_ok() {}
    assert!(
        app.is_spawn_orchestration_thread(manager_thread_id),
        "an active native assignment Manager must receive terminal lifecycle events"
    );
    assert!(
        app.is_spawn_orchestration_thread(worker_thread_id),
        "an active native assignment Worker must receive terminal lifecycle events"
    );

    // This is the incident shape: the picker still has an older visible answer, but the current
    // provider turn completes without an AgentMessage item.
    app.agent_navigation.set_last_result_message(
        manager_thread_id,
        Some("Before I dispatch, give me the assignment spec.".to_string()),
    );
    app.update_spawn_status_for_thread_notification(&turn_completed_notification(
        manager_thread_id,
        "manager-empty-1",
        TurnStatus::Completed,
    ));
    assert_eq!(
        app.agent_navigation
            .get(&manager_thread_id)
            .and_then(|entry| entry.last_result_message.as_deref()),
        Some("Turn completed without visible output."),
        "the picker should describe the terminal state without hiding the empty provider result"
    );

    let retry = drain_spawn_agent_task_for(&mut app_event_rx, manager_thread_id)
        .expect("first empty completion should retry the Manager");
    assert!(retry.contains("previous turn completed successfully"));
    assert!(retry.contains("latest user message already present"));
    assert!(retry.contains("Do not ask the user to repeat"));
    assert_eq!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|whip| (whip.state, whip.empty_output_fires)),
        Some((crate::orchestrate::WhipState::Armed, 1))
    );

    // The app observes the same completion once when received and again during buffered replay.
    // Replaying that terminal notification must not consume the one allowed recovery retry.
    app.update_spawn_status_for_thread_notification(&turn_completed_notification(
        manager_thread_id,
        "manager-empty-1",
        TurnStatus::Completed,
    ));
    assert!(drain_spawn_agent_task_for(&mut app_event_rx, manager_thread_id).is_none());
    assert_eq!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|whip| (whip.state, whip.empty_output_fires)),
        Some((crate::orchestrate::WhipState::Armed, 1))
    );

    app.update_spawn_status_for_thread_notification(&turn_completed_notification(
        manager_thread_id,
        "manager-empty-2",
        TurnStatus::Completed,
    ));
    assert!(drain_spawn_agent_task_for(&mut app_event_rx, manager_thread_id).is_none());
    assert_eq!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|whip| whip.state),
        Some(crate::orchestrate::WhipState::Paused)
    );
}

#[tokio::test]
async fn assignment_manager_visible_paraphrase_resets_empty_completion_guard() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(&app, "empty-reset", "Draft and run the recovery audit.");
    let manager_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000465").expect("manager id");
    let worker_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000466").expect("worker id");
    app.upsert_agent_picker_thread(
        manager_thread_id,
        Some("Manager".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        worker_thread_id,
        Some("Worker".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    let manager_node = crate::spawn_orchestration::thread_node_id(manager_thread_id);
    let worker_node = crate::spawn_orchestration::thread_node_id(worker_thread_id);
    app.handle_orchestrate_command(format!(
        "attach {worker_node} empty-reset --mode review --holder {manager_node} --for 1h"
    ));
    while app_event_rx.try_recv().is_ok() {}

    app.note_whip_target_idle_with_fire_control(
        &manager_node,
        /*last_output*/ None,
        /*allow_fire*/ false,
        /*turn_succeeded*/ true,
    );
    let _ = drain_spawn_agent_task_for(&mut app_event_rx, manager_thread_id)
        .expect("first empty completion should retry");
    app.note_whip_target_idle_with_fire_control(
        &manager_node,
        Some("I have enough context; drafting the acceptance criteria now."),
        /*allow_fire*/ false,
        /*turn_succeeded*/ true,
    );
    assert_eq!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|whip| whip.empty_output_fires),
        Some(0)
    );
    app.note_whip_target_idle_with_fire_control(
        &manager_node,
        Some("   "),
        /*allow_fire*/ false,
        /*turn_succeeded*/ true,
    );
    assert!(drain_spawn_agent_task_for(&mut app_event_rx, manager_thread_id).is_some());
    assert_eq!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|whip| whip.state),
        Some(crate::orchestrate::WhipState::Armed)
    );
}

#[tokio::test]
async fn codex_pane_description_uses_cached_model_instead_of_unknown() {
    let mut app = make_test_app().await;
    let thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000467").expect("thread id");
    app.upsert_agent_picker_thread(
        thread_id,
        Some("Manager".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    app.agent_navigation
        .set_model(thread_id, Some("claude-fable-5-plan".to_string()));
    let entry = app.agent_navigation.get(&thread_id).expect("picker entry");

    let description = app.codex_pane_description(thread_id, entry);

    assert!(
        description.starts_with("claude-fable-5-plan; idle"),
        "{description}"
    );
    assert!(!description.contains("model unknown"), "{description}");
}

#[tokio::test]
async fn assignment_bad_target_retries_durable_worker_once_then_pauses() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(&app, "durable-retry", "Run the recovery audit.");
    let manager_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000471").expect("manager id");
    let worker_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000472").expect("worker id");
    app.upsert_agent_picker_thread(
        manager_thread_id,
        Some("Manager".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        worker_thread_id,
        Some("Worker".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    let manager_node = crate::spawn_orchestration::thread_node_id(manager_thread_id);
    let worker_node = crate::spawn_orchestration::thread_node_id(worker_thread_id);
    app.handle_orchestrate_command(format!(
        "attach {worker_node} durable-retry --mode review --holder {manager_node} --for 1h"
    ));
    while app_event_rx.try_recv().is_ok() {}

    app.dispatch_spawn_task_blocks(
        &manager_node,
        vec![crate::spawn_orchestration::SpawnTaskDispatch {
            target: "Worker nickname that does not resolve".to_string(),
            task: "Audit read-only state.".to_string(),
            seq: Some(1),
        }],
    );

    let retried_task = drain_spawn_agent_task_for(&mut app_event_rx, worker_thread_id)
        .expect("one retry should target the durable Worker ID");
    let assignment = app
        .orchestrate_whips
        .get("assignment-1")
        .expect("assignment");
    assert_eq!(assignment.state, crate::orchestrate::WhipState::Armed);
    assert!(
        assignment
            .last_dispatch_result
            .as_deref()
            .is_some_and(|result| result.contains("retrying durable Worker ID"))
    );

    app.record_spawn_dispatch_failed_for_task(
        &worker_node,
        &retried_task,
        "Worker closed before turn start",
    );

    let assignment = app
        .orchestrate_whips
        .get("assignment-1")
        .expect("assignment");
    assert_eq!(assignment.state, crate::orchestrate::WhipState::Paused);
    assert!(
        assignment
            .last_dispatch_result
            .as_deref()
            .is_some_and(|result| result.contains("retry failed"))
    );
    assert!(drain_spawn_agent_task_for(&mut app_event_rx, worker_thread_id).is_none());
}

#[tokio::test]
async fn assignment_creation_fails_visibly_when_layout_cannot_be_persisted() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(
        &app,
        "persistence-check",
        "Verify durable assignment state.",
    );
    let manager_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000481").expect("manager id");
    let worker_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000482").expect("worker id");
    app.upsert_agent_picker_thread(
        manager_thread_id,
        Some("Manager".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        worker_thread_id,
        Some("Worker".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    app.primary_thread_id = Some(ThreadId::new());

    let panes_path = app.config.codex_home.join("panes");
    if panes_path.exists() {
        std::fs::remove_dir_all(&panes_path).expect("remove test panes directory");
    }
    std::fs::write(&panes_path, "path collision").expect("create persistence failure fixture");

    app.handle_orchestrate_command(format!(
        "attach {} persistence-check --mode review --holder {} --for 1h",
        crate::spawn_orchestration::thread_node_id(worker_thread_id),
        crate::spawn_orchestration::thread_node_id(manager_thread_id),
    ));

    assert!(app.orchestrate_whips.is_empty());
    let mut rendered = String::new();
    while let Ok(event) = app_event_rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            rendered.push_str(&lines_to_single_string(&cell.display_lines(/*width*/ 120)));
        }
    }
    assert!(rendered.contains("Pane layout was not saved"), "{rendered}");
    assert!(
        rendered.contains("Assignment was not created because its state could not be saved"),
        "{rendered}"
    );
}

#[tokio::test]
async fn orchestrate_fast_path_is_worker_then_manager_with_eight_hour_draft_defaults() {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000491").expect("main id");
    let manager_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000492").expect("manager id");
    let worker_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000493").expect("worker id");
    app.primary_thread_id = Some(main_thread_id);
    app.upsert_agent_picker_thread(
        manager_thread_id,
        Some("Manager".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        worker_thread_id,
        Some("Worker".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );

    app.open_orchestrate_fast_target_picker();
    assert_app_snapshot!(
        "orchestrate_fast_worker_picker",
        render_bottom_popup(&app.chat_widget, /*width*/ 100)
    );

    app.open_orchestrate_fast_manager_picker(crate::spawn_orchestration::thread_node_id(
        worker_thread_id,
    ));
    assert_app_snapshot!(
        "orchestrate_fast_manager_picker",
        render_bottom_popup(&app.chat_widget, /*width*/ 100)
    );
}

#[tokio::test]
async fn assignment_unreachable_watchdog_pauses_after_four_cadences() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(&app, "watchdog", "Keep managing.");
    let manager_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            /*spawn_role*/ None,
            Some("Manager".to_string()),
        )
        .expect("create manager");
    let worker_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            /*spawn_role*/ None,
            Some("Worker".to_string()),
        )
        .expect("create worker");
    let manager_node = crate::spawn_orchestration::pane_node_id(&manager_pane_id);
    let worker_node = crate::spawn_orchestration::pane_node_id(&worker_pane_id);
    let started = chrono::DateTime::parse_from_rfc3339("2026-07-10T20:00:00Z")
        .expect("timestamp")
        .with_timezone(&chrono::Utc);
    app.orchestrate_now_override = Some(started);
    app.handle_orchestrate_command(format!(
        "attach {worker_pane_id} watchdog --mode review --holder {manager_pane_id} --for 8h --cooldown 900s"
    ));
    app.note_whip_holder_dispatched(&manager_node, &worker_node);
    app.orchestrate_whips
        .get_mut("assignment-1")
        .expect("assignment")
        .target = "pane:missing-worker".to_string();

    app.sweep_orchestrate_whips();
    app.orchestrate_now_override = Some(started + chrono::Duration::seconds(3599));
    app.sweep_orchestrate_whips();
    assert_eq!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|whip| whip.state),
        Some(crate::orchestrate::WhipState::Armed)
    );
    app.orchestrate_now_override = Some(started + chrono::Duration::seconds(3600));
    app.sweep_orchestrate_whips();
    assert_eq!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|whip| whip.state),
        Some(crate::orchestrate::WhipState::Paused)
    );
}

#[tokio::test]
async fn assignment_for_one_hour_keeps_the_whole_hour() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(&app, "watchdog", "Keep managing.");
    let manager_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            /*spawn_role*/ None,
            Some("Manager".to_string()),
        )
        .expect("create manager");
    let worker_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            /*spawn_role*/ None,
            Some("Worker".to_string()),
        )
        .expect("create worker");
    // No clock override: the real clock moves between reads, as in the product.
    assert_eq!(app.orchestrate_now_override, None);
    app.handle_orchestrate_command(format!(
        "attach {worker_pane_id} watchdog --mode review --holder {manager_pane_id} --for 1h"
    ));

    let execution_duration_s =
        app.orchestrate_whips
            .get("assignment-1")
            .and_then(|whip| match &whip.kind {
                crate::orchestrate::WhipKind::Assignment {
                    execution_duration_s,
                    ..
                } => *execution_duration_s,
                crate::orchestrate::WhipKind::LegacyNudge => None,
            });
    assert_eq!(execution_duration_s, Some(3600));
}

#[tokio::test]
async fn assignment_restart_lifecycle_waits_one_cadence_then_continues() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(&app, "restart-loop", "Continue the recovery work.");
    let manager_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000501").expect("manager id");
    let worker_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000502").expect("worker id");
    app.upsert_agent_picker_thread(
        manager_thread_id,
        Some("Manager".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        worker_thread_id,
        Some("Worker".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    let manager_node = crate::spawn_orchestration::thread_node_id(manager_thread_id);
    let worker_node = crate::spawn_orchestration::thread_node_id(worker_thread_id);
    let started = chrono::DateTime::parse_from_rfc3339("2026-07-10T20:00:00Z")
        .expect("timestamp")
        .with_timezone(&chrono::Utc);
    app.orchestrate_now_override = Some(started);
    app.handle_orchestrate_command(format!(
        "attach {worker_node} restart-loop --mode review --holder {manager_node} --for 8h --cooldown 60s"
    ));
    app.note_whip_holder_dispatched(&manager_node, &worker_node);
    while app_event_rx.try_recv().is_ok() {}

    let restarted = started + chrono::Duration::seconds(10);
    app.orchestrate_now_override = Some(restarted);
    let assignment = app
        .orchestrate_whips
        .get_mut("assignment-1")
        .expect("assignment");
    assignment.last_fire_utc = Some(restarted);
    assignment.last_idle_generation_fired = Some(0);
    assignment.last_target_output = Some("saved Worker output".to_string());
    app.audit_restored_assignments();

    assert_eq!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|assignment| assignment.state),
        Some(crate::orchestrate::WhipState::Armed)
    );
    let mut restart_notice = String::new();
    while let Ok(event) = app_event_rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            restart_notice.push_str(&lines_to_single_string(&cell.display_lines(/*width*/ 120)));
        }
    }
    assert!(
        restart_notice.contains("restored; the next Manager mandate waits one cadence"),
        "{restart_notice}"
    );

    app.orchestrate_now_override = Some(restarted + chrono::Duration::seconds(59));
    app.sweep_orchestrate_whips();
    assert!(drain_spawn_agent_task_for(&mut app_event_rx, manager_thread_id).is_none());
    app.orchestrate_now_override = Some(restarted + chrono::Duration::seconds(61));
    app.sweep_orchestrate_whips();
    assert!(drain_spawn_agent_task_for(&mut app_event_rx, manager_thread_id).is_some());
}

#[tokio::test]
async fn assignment_rejects_codex_main_as_manager() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    app.primary_thread_id = Some(ThreadId::new());
    write_test_whip(&app, "main-manager", "Keep managing.");
    let worker_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            /*spawn_role*/ None,
            Some("Worker".to_string()),
        )
        .expect("create worker");

    app.handle_orchestrate_command(format!(
        "attach {worker_pane_id} main-manager --mode review --holder codex-main --for 1h"
    ));

    assert!(app.orchestrate_whips.is_empty());
}

#[tokio::test]
async fn orchestrate_review_holder_ignored_twice_pauses_whip() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(&app, "review-loop", "# whip: review-loop\nReview target.");
    let holder_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Troll),
            Some("Burzum".to_string()),
        )
        .expect("create holder pane");
    let target_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Krimp".to_string()),
        )
        .expect("create target pane");
    let holder_node_id = crate::spawn_orchestration::pane_node_id(&holder_pane_id);

    app.handle_orchestrate_command(format!(
        "attach {target_pane_id} review-loop --mode review --holder {holder_pane_id} --max 5"
    ));
    app.orchestrate_whips
        .get_mut("assignment-1")
        .expect("legacy review whip")
        .kind = crate::orchestrate::WhipKind::LegacyNudge;
    let _ = drain_claude_pane_task_events(&mut app_event_rx);
    app.handle_orchestrate_command("fire assignment-1".to_string());
    app.note_whip_target_idle_with_fire_control(
        &holder_node_id,
        Some("no dispatch"),
        /*allow_fire*/ true,
        /*turn_succeeded*/ true,
    );
    app.handle_orchestrate_command("fire assignment-1".to_string());
    app.note_whip_target_idle_with_fire_control(
        &holder_node_id,
        Some("still no dispatch"),
        /*allow_fire*/ true,
        /*turn_succeeded*/ true,
    );

    let submitted_tasks = drain_claude_pane_task_events(&mut app_event_rx);
    assert_eq!(
        submitted_tasks
            .iter()
            .filter(|(pane_id, _)| pane_id == &holder_pane_id)
            .count(),
        2
    );
    assert_eq!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|whip| whip.state),
        Some(crate::orchestrate::WhipState::Paused)
    );
}

#[tokio::test]
async fn orchestrate_detach_removes_whip_and_idle_generation() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(&app, "keep-going", "# whip: keep-going\nContinue the work.");
    let pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Krimp".to_string()),
        )
        .expect("create pane");

    app.handle_orchestrate_command(format!(
        "attach {pane_id} keep-going --mode auto --holder none --max 3"
    ));
    app.handle_orchestrate_command("detach whip-1".to_string());
    app.sweep_orchestrate_whips();

    assert!(drain_claude_pane_task_events(&mut app_event_rx).is_empty());
    assert!(app.orchestrate_whips.is_empty());
    assert!(app.orchestrate_idle_generation_by_target.is_empty());
}

#[tokio::test]
async fn assignment_worker_completion_wakes_manager_without_waiting_for_watchdog() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(&app, "completion-handoff", "Keep managing the Worker.");
    let manager_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Troll),
            Some("Manager".to_string()),
        )
        .expect("create manager");
    let worker_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Worker".to_string()),
        )
        .expect("create worker");
    let manager_node = crate::spawn_orchestration::pane_node_id(&manager_pane_id);
    let worker_node = crate::spawn_orchestration::pane_node_id(&worker_pane_id);
    let started = chrono::DateTime::parse_from_rfc3339("2026-07-10T20:00:00Z")
        .expect("timestamp")
        .with_timezone(&chrono::Utc);
    app.orchestrate_now_override = Some(started);
    app.handle_orchestrate_command(format!(
        "attach {worker_pane_id} completion-handoff --mode review --holder {manager_pane_id} --for 8h --cooldown 900s"
    ));
    app.note_whip_holder_dispatched(&manager_node, &worker_node);
    while app_event_rx.try_recv().is_ok() {}
    app.orchestrate_whips
        .get_mut("assignment-1")
        .expect("assignment")
        .last_fire_utc = Some(started);
    app.orchestrate_now_override = Some(started + chrono::Duration::seconds(3));

    app.note_whip_target_idle_with_fire_control(
        &worker_node,
        Some("first completed result"),
        /*allow_fire*/ true,
        /*turn_succeeded*/ true,
    );

    let immediate = drain_claude_pane_task_events(&mut app_event_rx);
    assert_eq!(immediate.len(), 1);
    assert_eq!(immediate[0].0, manager_pane_id);
    assert_eq!(
        app.orchestrate_whips
            .get("assignment-1")
            .and_then(|whip| whip.last_idle_generation_fired),
        Some(1)
    );

    app.claude_panes
        .panes
        .iter_mut()
        .find(|pane| pane.id == manager_pane_id)
        .expect("manager pane")
        .status = crate::claude_panes::ClaudePaneStatus::Running;
    app.note_whip_target_idle_with_fire_control(
        &worker_node,
        Some("second completed result"),
        /*allow_fire*/ true,
        /*turn_succeeded*/ true,
    );
    assert!(drain_claude_pane_task_events(&mut app_event_rx).is_empty());

    app.claude_panes
        .panes
        .iter_mut()
        .find(|pane| pane.id == manager_pane_id)
        .expect("manager pane")
        .status = crate::claude_panes::ClaudePaneStatus::Idle;
    app.orchestrate_now_override = Some(started + chrono::Duration::seconds(6));
    app.note_whip_target_idle_with_fire_control(
        &manager_node,
        Some("manager finished its prior audit"),
        /*allow_fire*/ false,
        /*turn_succeeded*/ true,
    );

    let pending = drain_claude_pane_task_events(&mut app_event_rx);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].0, manager_pane_id);
    assert_eq!(
        app.orchestrate_whips
            .get("assignment-1")
            .and_then(|whip| whip.last_idle_generation_fired),
        Some(2)
    );

    app.orchestrate_now_override = Some(started + chrono::Duration::seconds(9));
    app.note_whip_target_idle_with_fire_control(
        &worker_node,
        /*last_output*/ None,
        /*allow_fire*/ true,
        /*turn_succeeded*/ false,
    );

    let interrupted = drain_claude_pane_task_events(&mut app_event_rx);
    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0].0, manager_pane_id);
    let assignment = app
        .orchestrate_whips
        .get("assignment-1")
        .expect("assignment");
    assert_eq!(assignment.last_idle_generation_fired, Some(3));
    assert_eq!(
        assignment.last_target_output.as_deref(),
        Some("Worker turn ended unsuccessfully without visible output.")
    );
}

#[tokio::test]
async fn orchestrate_restored_whip_waits_for_fresh_idle_edge() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(
        &app,
        "resume-safe",
        "# whip: resume-safe\nContinue the work.",
    );
    let pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Krimp".to_string()),
        )
        .expect("create pane");
    let target_node_id = crate::spawn_orchestration::pane_node_id(&pane_id);

    app.handle_orchestrate_command(format!(
        "attach {pane_id} resume-safe --mode auto --holder none --max 3 --cooldown 1s"
    ));
    app.orchestrate_whips
        .get_mut("whip-1")
        .expect("whip")
        .last_idle_generation_fired = Some(0);

    app.sweep_orchestrate_whips();
    assert!(drain_claude_pane_task_events(&mut app_event_rx).is_empty());

    app.note_whip_target_idle_with_fire_control(
        &target_node_id,
        Some("completed"),
        /*allow_fire*/ true,
        /*turn_succeeded*/ true,
    );
    assert_eq!(drain_claude_pane_task_events(&mut app_event_rx).len(), 1);
}

#[tokio::test]
async fn orchestrate_rejects_pathy_whip_names() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;

    app.handle_orchestrate_command(
        "attach x ../../etc/hosts --mode auto --holder none".to_string(),
    );

    assert!(app.orchestrate_whips.is_empty());
    assert!(drain_claude_pane_task_events(&mut app_event_rx).is_empty());
}

#[tokio::test]
async fn orchestrate_fire_suppression_still_counts_ignored_review() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(&app, "review-loop", "# whip: review-loop\nReview target.");
    let holder_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Troll),
            Some("Burzum".to_string()),
        )
        .expect("create holder pane");
    let target_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Krimp".to_string()),
        )
        .expect("create target pane");
    let holder_node_id = crate::spawn_orchestration::pane_node_id(&holder_pane_id);

    app.handle_orchestrate_command(format!(
        "attach {target_pane_id} review-loop --mode review --holder {holder_pane_id} --max 5"
    ));
    app.orchestrate_whips
        .get_mut("assignment-1")
        .expect("legacy review whip")
        .kind = crate::orchestrate::WhipKind::LegacyNudge;
    let _ = drain_claude_pane_task_events(&mut app_event_rx);
    app.handle_orchestrate_command("fire assignment-1".to_string());
    app.note_whip_target_idle_with_fire_control(
        &holder_node_id,
        Some("no dispatch"),
        /*allow_fire*/ false,
        /*turn_succeeded*/ true,
    );

    let submitted_tasks = drain_claude_pane_task_events(&mut app_event_rx);
    assert_eq!(
        submitted_tasks
            .iter()
            .filter(|(pane_id, _)| pane_id == &holder_pane_id)
            .count(),
        1
    );
    let whip = app.orchestrate_whips.get("assignment-1").expect("whip");
    assert_eq!(whip.ignored_review_fires, 1);
    assert_eq!(whip.pending_review_fire, None);
    assert_eq!(whip.state, crate::orchestrate::WhipState::Armed);
}

#[tokio::test]
async fn orchestrate_agent_block_cannot_replace_user_held_whip() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(&app, "first", "# whip: first\nFirst mandate.");
    write_test_whip(&app, "second", "# whip: second\nSecond mandate.");
    let holder_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Troll),
            Some("Burzum".to_string()),
        )
        .expect("create holder pane");
    let agent_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Troll),
            Some("Gorbag".to_string()),
        )
        .expect("create agent pane");
    let target_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Krimp".to_string()),
        )
        .expect("create target pane");

    app.handle_orchestrate_command(format!(
        "attach {target_pane_id} first --mode review --holder {holder_pane_id} --max 5"
    ));
    let agent_node_id = crate::spawn_orchestration::pane_node_id(&agent_pane_id);
    app.dispatch_orchestrate_blocks_from_text(
        &agent_node_id,
        &format!(
            "```pfterminal-orchestrate\naction: attach\ntarget: {target_pane_id}\nwhip: second\nmode: auto\n```"
        ),
    );

    let active_whips: Vec<_> = app
        .orchestrate_whips
        .values()
        .filter(|whip| whip.state != crate::orchestrate::WhipState::Detached)
        .collect();
    assert_eq!(active_whips.len(), 1);
    assert_eq!(active_whips[0].instructions, "first");
    assert_eq!(
        active_whips[0].holder.as_deref(),
        Some(crate::spawn_orchestration::pane_node_id(&holder_pane_id).as_str())
    );
}

#[tokio::test]
async fn orchestrate_agent_block_cannot_replace_user_holderless_whip() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(&app, "first", "# whip: first\nFirst mandate.");
    write_test_whip(&app, "second", "# whip: second\nSecond mandate.");
    let agent_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Troll),
            Some("Gorbag".to_string()),
        )
        .expect("create agent pane");
    let target_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Krimp".to_string()),
        )
        .expect("create target pane");

    app.handle_orchestrate_command(format!(
        "attach {target_pane_id} first --mode auto --holder none --max 5"
    ));
    let agent_node_id = crate::spawn_orchestration::pane_node_id(&agent_pane_id);
    app.dispatch_orchestrate_blocks_from_text(
        &agent_node_id,
        &format!(
            "```pfterminal-orchestrate\naction: attach\ntarget: {target_pane_id}\nwhip: second\nmode: auto\n```"
        ),
    );

    let active_whips: Vec<_> = app
        .orchestrate_whips
        .values()
        .filter(|whip| whip.state != crate::orchestrate::WhipState::Detached)
        .collect();
    assert_eq!(active_whips.len(), 1);
    assert_eq!(active_whips[0].instructions, "first");
    assert_eq!(active_whips[0].holder, None);
}

#[tokio::test]
async fn orchestrate_agent_unlimited_attach_is_rejected() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(&app, "forever", "# whip: forever\nDo not run forever.");
    let agent_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Troll),
            Some("Burzum".to_string()),
        )
        .expect("create agent pane");
    let target_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Krimp".to_string()),
        )
        .expect("create target pane");
    let agent_node_id = crate::spawn_orchestration::pane_node_id(&agent_pane_id);

    app.dispatch_orchestrate_blocks_from_text(
        &agent_node_id,
        &format!(
            "```pfterminal-orchestrate\naction: attach\ntarget: {target_pane_id}\nwhip: forever\nfor: unlimited\n```"
        ),
    );

    assert!(app.orchestrate_whips.is_empty());
}

#[tokio::test]
async fn orchestrate_agent_can_pause_own_whip() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(&app, "review-loop", "# whip: review-loop\nReview target.");
    let agent_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Troll),
            Some("Burzum".to_string()),
        )
        .expect("create agent pane");
    let target_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Krimp".to_string()),
        )
        .expect("create target pane");
    let agent_node_id = crate::spawn_orchestration::pane_node_id(&agent_pane_id);

    app.dispatch_orchestrate_blocks_from_text(
        &agent_node_id,
        &format!(
            "```pfterminal-orchestrate\naction: attach\ntarget: {target_pane_id}\nwhip: review-loop\nmode: review\n```"
        ),
    );
    app.dispatch_orchestrate_blocks_from_text(
        &agent_node_id,
        "```pfterminal-orchestrate\naction: pause\nid: assignment-1\n```",
    );

    let whip = app.orchestrate_whips.get("assignment-1").expect("whip");
    assert_eq!(whip.holder.as_deref(), Some(agent_node_id.as_str()));
    assert_eq!(whip.state, crate::orchestrate::WhipState::Paused);
}

#[tokio::test]
async fn claude_adapter_emits_direct_records_without_prose_batching() {
    let (mut app, mut rx, troll_pane_id, orc_pane_id) =
        make_child_report_auto_claude_pane_app().await;
    app.dispatch_spawn_task_blocks(
        &troll_pane_id,
        vec![
            crate::spawn_orchestration::SpawnTaskDispatch {
                target: orc_pane_id.clone(),
                task: "first exact task".to_string(),
                seq: Some(601),
            },
            crate::spawn_orchestration::SpawnTaskDispatch {
                target: orc_pane_id,
                task: "second exact task".to_string(),
                seq: Some(602),
            },
        ],
    );
    let tasks = drain_claude_pane_task_events(&mut rx);
    assert_eq!(tasks.len(), 2);
    assert!(tasks[0].1.contains("first exact task"));
    assert!(!tasks[0].1.contains("second exact task"));
    assert!(tasks[1].1.contains("second exact task"));
    assert!(!tasks[1].1.contains("Multiple spawn dispatches"));
}

/// #507: turning the kill switch on interrupts every running Claude pane
/// turn (its cancel token ends the Claude process), and the pane says why.
#[tokio::test]
async fn issue_507_kill_switch_interrupts_running_claude_panes() {
    let (mut app, _app_event_rx, troll_pane_id, orc_pane_id) =
        make_child_report_auto_claude_pane_app().await;
    let codex_home = app.config.codex_home.to_path_buf();
    let running = app
        .claude_panes
        .prepare_turn(&troll_pane_id, "long task".to_string(), &codex_home)
        .expect("prepare running Troll pane");
    assert!(app.claude_panes.claude_pane_is_running(&troll_pane_id));
    assert!(!app.claude_panes.claude_pane_is_running(&orc_pane_id));

    app.stop_claude_panes_for_kill_switch();

    assert!(running.cancel_token.is_cancelled());
    let said = |pane_id: &str| {
        app.claude_pane_transcript_cells
            .get(pane_id)
            .into_iter()
            .flatten()
            .any(|cell| {
                lines_to_single_string(&cell.display_lines(/*width*/ 400))
                    .contains("security kill switch was turned on")
            })
    };
    assert!(said(&troll_pane_id));
    assert!(!said(&orc_pane_id), "an idle pane has nothing to stop");
}

/// #507 (Travis, 2026-10-12): a kill switch another Corbanu Terminal process
/// on this home turns on stops this app's running Claude panes too and says
/// where it came from; the same change reported again (by `/security` here
/// or the watch) is handled once; turning it off there is announced and
/// starts nothing here.
#[tokio::test]
async fn issue_507_kill_switch_from_another_process_stops_claude_panes_once() {
    let (mut app, mut app_event_rx, troll_pane_id, orc_pane_id) =
        make_child_report_auto_claude_pane_app().await;
    let codex_home = app.config.codex_home.to_path_buf();
    let rendered = |rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>| {
        let mut text = String::new();
        while let Ok(event) = rx.try_recv() {
            if let AppEvent::InsertHistoryCell(cell) = event {
                text.push_str(&lines_to_single_string(&cell.display_lines(/*width*/ 400)));
            }
        }
        text
    };
    let _ = rendered(&mut app_event_rx);
    let running = app
        .claude_panes
        .prepare_turn(&troll_pane_id, "long task".to_string(), &codex_home)
        .expect("prepare running Troll pane");
    assert!(!app.kill_switch_known_on);

    app.on_kill_switch_changed(/*on*/ true, /*elsewhere*/ true);
    assert!(running.cancel_token.is_cancelled());
    assert!(app.kill_switch_known_on);
    let text = rendered(&mut app_event_rx);
    assert!(
        text.contains("turned on in another Corbanu Terminal on this home"),
        "{text}"
    );

    // Reported again: nothing more is stopped or said.
    let later = app
        .claude_panes
        .prepare_turn(&orc_pane_id, "later".to_string(), &codex_home)
        .expect("prepare a later turn");
    app.on_kill_switch_changed(/*on*/ true, /*elsewhere*/ false);
    app.on_kill_switch_changed(/*on*/ true, /*elsewhere*/ true);
    assert!(!later.cancel_token.is_cancelled());
    assert!(!rendered(&mut app_event_rx).contains("another Corbanu Terminal"));

    // Turned off elsewhere: nothing starts here by itself (Travis,
    // 2026-10-12); a task held for a Claude pane stays held.
    app.kill_switch_held_claude_tasks
        .push((troll_pane_id.clone(), "held task".to_string()));
    app.on_kill_switch_changed(/*on*/ false, /*elsewhere*/ true);
    assert!(!app.kill_switch_known_on);
    let text = rendered(&mut app_event_rx);
    assert!(
        text.contains("turned off in another Corbanu Terminal on this home"),
        "{text}"
    );
    assert!(text.contains("Nothing restarts by itself here"), "{text}");
    assert_eq!(
        app.kill_switch_held_claude_tasks,
        vec![(troll_pane_id.clone(), "held task".to_string())]
    );
    // It goes with that pane's next turn (#602's delivery, tested there).
}

/// #842 (Travis, 2026-10-15): Moderate confirmed in another Corbanu Terminal
/// on this home (Core already raised this process's sessions) is announced
/// here, saying what applies now and what waits for the restart.
#[tokio::test]
async fn issue_842_moderate_from_another_terminal_is_announced() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    while app_event_rx.try_recv().is_ok() {}
    app.on_level_raised_elsewhere(crate::legacy_core::security_revocation::SecurityLevel::Moderate);
    let mut text = String::new();
    while let Ok(event) = app_event_rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            text.push_str(&lines_to_single_string(&cell.display_lines(/*width*/ 400)));
        }
    }
    for expected in [
        "Moderate was confirmed in another Corbanu Terminal on this home",
        "Core's Moderate rules apply here now",
        "shown as Permissive until you restart",
    ] {
        assert!(text.contains(expected), "{expected}: {text}");
    }
}

/// #919 (Travis, 2026-10-16, D20): Aggressive confirmed in another Corbanu
/// Terminal on this home (Core already raised this process's sessions and
/// required approval for every command) is announced here, saying what
/// applies now and what waits for the restart; the protections claimed
/// follow `source_envelopes`, and the level shown is this session's own.
#[tokio::test]
async fn issue_919_aggressive_from_another_terminal_is_announced() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    while app_event_rx.try_recv().is_ok() {}
    app.on_level_raised_elsewhere(
        crate::legacy_core::security_revocation::SecurityLevel::Aggressive,
    );
    let mut text = String::new();
    while let Ok(event) = app_event_rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            text.push_str(&lines_to_single_string(&cell.display_lines(/*width*/ 1000)));
        }
    }
    for expected in [
        "Aggressive was confirmed in another Corbanu Terminal on this home",
        "Core's Aggressive protections apply here now: agent commands cannot run `corbanu vault …`",
        "From each pane's next turn you approve every command that is not known read-only",
        "Grants and \"for session\" approvals here ended",
        "The rest of Aggressive starts when you restart Corbanu Terminal: its sandbox",
        "this session is shown as Permissive",
    ] {
        assert!(text.contains(expected), "{expected}: {text}");
    }
    // A lower level is never announced as a raise.
    app.on_level_raised_elsewhere(
        crate::legacy_core::security_revocation::SecurityLevel::Permissive,
    );
    assert!(app_event_rx.try_recv().is_err());
}

/// #919: the notice claims the protected-path denials only with the
/// untrusted-content labels on, and names the level this session is shown
/// as until the restart.
#[test]
fn issue_919_aggressive_notice_follows_labels_and_the_shown_level() {
    use crate::claude_panes::app_integration::aggressive_confirmed_elsewhere;
    use crate::security::level::ChosenLevel;
    let labelled =
        aggressive_confirmed_elsewhere(/*untrusted_content*/ true, ChosenLevel::Moderate);
    assert!(labelled.contains("protected paths"), "{labelled}");
    assert!(labelled.contains("shown as Moderate"), "{labelled}");
    // Moderate's credential broker already holds the model keys.
    assert!(!labelled.contains("credential broker"), "{labelled}");
    let vault_only =
        aggressive_confirmed_elsewhere(/*untrusted_content*/ false, ChosenLevel::Permissive);
    assert!(!vault_only.contains("protected paths"), "{vault_only}");
    assert!(
        vault_only.contains("model keys held by the credential broker, and the child-agent rules"),
        "{vault_only}"
    );
    assert!(
        vault_only.contains("in every Corbanu pane and child agent of this Corbanu Terminal"),
        "{vault_only}"
    );
}

fn interrupted_claude_turn(app: &App) -> crate::claude_panes::ClaudePaneTurnOutput {
    crate::claude_panes::ClaudePaneTurnOutput {
        text: String::new(),
        status: crate::claude_panes::ClaudePaneTurnStatus::Interrupted,
        session_id: None,
        usage_summary: None,
        turn_usage_summary: None,
        usage_status: crate::claude_panes::ClaudePaneUsageStatus::Missing,
        artifact_path: app.config.cwd.join("turn-0001.jsonl").to_path_buf(),
        audit_path: app.config.cwd.join("turn-0001.audit.json").to_path_buf(),
        duration_ms: 1,
        terminal_reason: Some("interrupted".to_string()),
        error_summary: Some("Claude pane turn interrupted by user.".to_string()),
        tool_names: Vec::new(),
        tool_events: Vec::new(),
        reasoning_events: Vec::new(),
        command_mode: crate::claude_panes::ClaudeCommandMode::NewSession,
        direct_accounting: None,
    }
}

/// #554: a Claude turn the kill switch stopped ends with the line Codex
/// panes show, not "interrupted by user"; a person's own interrupt keeps it.
#[tokio::test]
async fn issue_554_kill_switch_stop_does_not_say_interrupted_by_user() {
    let (mut app, _app_event_rx, troll_pane_id, _orc_pane_id) =
        make_child_report_auto_claude_pane_app().await;
    let codex_home = app.config.codex_home.to_path_buf();
    let transcript = |app: &App, pane_id: &str| {
        app.claude_pane_transcript_cells
            .get(pane_id)
            .into_iter()
            .flatten()
            .map(|cell| lines_to_single_string(&cell.display_lines(/*width*/ 400)))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let running = app
        .claude_panes
        .prepare_turn(&troll_pane_id, "long task".to_string(), &codex_home)
        .expect("prepare running Troll pane");
    app.stop_claude_panes_for_kill_switch();
    drop(running);
    let output = interrupted_claude_turn(&app);
    app.on_claude_pane_turn_finished(troll_pane_id.clone(), Ok(output));
    let said = transcript(&app, &troll_pane_id);
    assert!(
        said.contains("security kill switch was turned on"),
        "{said}"
    );
    assert!(
        said.contains(crate::chatwidget::KILL_SWITCH_TURN_STOPPED),
        "{said}"
    );
    assert!(!said.contains("interrupted by user"), "{said}");

    // The next turn, interrupted by the person, says so as before.
    let running = app
        .claude_panes
        .prepare_turn(&troll_pane_id, "another task".to_string(), &codex_home)
        .expect("prepare the next turn");
    app.claude_panes
        .interrupt_turn(&troll_pane_id)
        .expect("interrupt");
    drop(running);
    let output = interrupted_claude_turn(&app);
    app.on_claude_pane_turn_finished(troll_pane_id.clone(), Ok(output));
    let said = transcript(&app, &troll_pane_id);
    assert!(said.contains("interrupted by user"), "{said}");
}

/// #507: the person confirms `choice` in `/security`.
fn issue_507_switch(app: &App, choice: crate::legacy_core::security_revocation::HumanRevocation) {
    use crate::legacy_core::security_level_change::LevelBasis;
    use crate::legacy_core::security_level_change::SecurityLevel;
    use crate::legacy_core::security_revocation::commit_human_revocation;
    let codex_home = app.config.codex_home.to_path_buf();
    let basis = LevelBasis::read(&codex_home, SecurityLevel::Permissive, /*thread*/ None);
    commit_human_revocation(
        &codex_home,
        SecurityLevel::Permissive,
        /*thread*/ None,
        &basis,
        choice,
        /*now_unix_seconds*/ 1_000,
    )
    .expect("commit");
}

fn issue_507_transcript(app: &App, pane_id: &str) -> String {
    app.claude_pane_transcript_cells
        .get(pane_id)
        .into_iter()
        .flatten()
        .map(|cell| lines_to_single_string(&cell.display_lines(/*width*/ 400)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// #507 (owner decision 2026-10-12): tasks other agents send to Claude panes
/// while the kill switch is on wait (one per pane, the newest; the older one
/// is recorded as superseded). Turning the switch off starts nothing, and a
/// pane's turn ending does not send its waiting task either.
#[tokio::test]
async fn issue_507_switch_off_starts_no_claude_pane_turn() {
    use crate::legacy_core::security_revocation::HumanRevocation;
    let (mut app, _app_event_rx, troll_pane_id, orc_pane_id) =
        make_child_report_auto_claude_pane_app().await;
    let codex_home = app.config.codex_home.to_path_buf();
    let held = |app: &App| app.kill_switch_held_claude_tasks.clone();
    issue_507_switch(&app, HumanRevocation::KillSwitchOn);

    for tick in 1..=3 {
        app.submit_claude_pane_task(troll_pane_id.clone(), format!("assignment fire {tick}"));
    }
    app.submit_claude_pane_task(orc_pane_id.clone(), "orc task".to_string());
    let waiting = vec![
        (troll_pane_id.clone(), "assignment fire 3".to_string()),
        (orc_pane_id.clone(), "orc task".to_string()),
    ];
    assert_eq!(held(&app), waiting);
    assert!(app.claude_panes.prepared_prompts.is_empty());

    issue_507_switch(&app, HumanRevocation::KillSwitchOff);
    app.on_kill_switch_turned_off();
    assert!(!app.claude_panes.claude_pane_is_running(&troll_pane_id));
    assert!(!app.claude_panes.claude_pane_is_running(&orc_pane_id));
    assert!(
        app.claude_panes.prepared_prompts.is_empty(),
        "turning the switch off starts no turn"
    );
    assert_eq!(held(&app), waiting, "the tasks keep waiting");

    // A turn that ends in the pane (one its operator started) does not send
    // the waiting task either.
    drop(
        app.claude_panes
            .prepare_turn(&troll_pane_id, "busy".to_string(), &codex_home)
            .expect("a running turn"),
    );
    app.claude_panes
        .interrupt_turn(&troll_pane_id)
        .expect("stop the busy turn");
    app.on_claude_pane_turn_finished(troll_pane_id.clone(), Err("interrupted".to_string()));
    assert!(!app.claude_panes.claude_pane_is_running(&troll_pane_id));
    assert_eq!(
        app.claude_panes.prepared_prompts.len(),
        1,
        "only the busy turn"
    );
    assert_eq!(held(&app), waiting);
}

/// #507: after the switch is off, a Claude pane's waiting task goes with its
/// next turn: the one a person types (the waiting task first), or one new work
/// starts. Either way it is sent once, and the pane says so.
#[tokio::test]
async fn issue_507_waiting_claude_task_goes_with_the_next_turn() {
    use crate::legacy_core::security_revocation::HumanRevocation;
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let codex_home = app.config.codex_home.to_path_buf();
    let typed_pane = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            &codex_home,
            /*spawn_role*/ None,
            /*spawn_nickname*/ None,
        )
        .expect("create user pane");
    let worker_pane = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            &codex_home,
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Snaga".to_string()),
        )
        .expect("create worker pane");
    assert_eq!(
        app.claude_panes.active_claude_pane_id(),
        Some(typed_pane.as_str())
    );
    issue_507_switch(&app, HumanRevocation::KillSwitchOn);
    app.submit_claude_pane_task(typed_pane.clone(), "pf507 waiting task A".to_string());
    app.submit_claude_pane_task(worker_pane.clone(), "pf507 waiting task B".to_string());
    issue_507_switch(&app, HumanRevocation::KillSwitchOff);
    app.on_kill_switch_turned_off();
    assert!(app.claude_panes.prepared_prompts.is_empty());

    // A person types in the active pane.
    assert!(app.try_submit_active_claude_pane_op(&claude_pane_text_op("pf507 typed message")));
    let prompts = app.claude_panes.prepared_prompts.clone();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].0, typed_pane);
    let prompt = &prompts[0].1;
    let task = prompt
        .find("pf507 waiting task A")
        .expect("waiting task sent");
    let typed = prompt.find("pf507 typed message").expect("typed text sent");
    assert!(task < typed, "the waiting task arrived first: {prompt}");
    assert!(!prompt.contains("pf507 waiting task B"));

    // New work for the other pane: its waiting task goes with it.
    app.submit_claude_pane_task(worker_pane.clone(), "pf507 new task".to_string());
    let prompts = app.claude_panes.prepared_prompts.clone();
    assert_eq!(prompts.len(), 2);
    assert_eq!(prompts[1].0, worker_pane);
    assert!(prompts[1].1.contains("pf507 waiting task B"));
    assert!(prompts[1].1.contains("pf507 new task"));
    assert!(
        issue_507_transcript(&app, &worker_pane).contains("Sent with this turn"),
        "{}",
        issue_507_transcript(&app, &worker_pane)
    );
    assert!(app.kill_switch_held_claude_tasks.is_empty(), "sent once");
}

/// #507: a waiting task stays waiting when the next turn cannot start (the
/// pane is busy), and goes with the first turn that does.
#[tokio::test]
async fn issue_507_waiting_claude_task_survives_a_refused_turn() {
    use crate::legacy_core::security_revocation::HumanRevocation;
    let (mut app, _app_event_rx, _troll_pane_id, orc_pane_id) =
        make_child_report_auto_claude_pane_app().await;
    let codex_home = app.config.codex_home.to_path_buf();
    issue_507_switch(&app, HumanRevocation::KillSwitchOn);
    app.submit_claude_pane_task(orc_pane_id.clone(), "pf507 superseded".to_string());
    app.submit_claude_pane_task(orc_pane_id.clone(), "pf507 waiting".to_string());
    assert_eq!(
        app.kill_switch_held_claude_tasks,
        vec![(orc_pane_id.clone(), "pf507 waiting".to_string())],
        "the newest waits; the older one was reported superseded"
    );
    issue_507_switch(&app, HumanRevocation::KillSwitchOff);
    let busy = app
        .claude_panes
        .prepare_turn(&orc_pane_id, "busy".to_string(), &codex_home)
        .expect("a running turn");
    app.submit_claude_pane_task(orc_pane_id.clone(), "pf507 refused".to_string());
    assert_eq!(
        app.claude_panes.prepared_prompts.len(),
        1,
        "only the busy turn"
    );
    assert_eq!(
        app.kill_switch_held_claude_tasks,
        vec![(orc_pane_id.clone(), "pf507 waiting".to_string())]
    );
    drop(busy);
    app.claude_panes
        .interrupt_turn(&orc_pane_id)
        .expect("stop the busy turn");
    app.on_claude_pane_turn_finished(orc_pane_id.clone(), Err("interrupted".to_string()));
    app.submit_claude_pane_task(orc_pane_id.clone(), "pf507 next".to_string());
    assert!(app.kill_switch_held_claude_tasks.is_empty());
    let prompt = &app.claude_panes.prepared_prompts[1].1;
    assert!(prompt.contains("pf507 waiting"), "{prompt}");
    assert!(!prompt.contains("pf507 superseded"), "{prompt}");
}

/// #507: assignments and whips (timers) do not fire while the kill switch
/// is on; they are paused, and turning it off does not resume them. A person
/// resumes them.
#[tokio::test]
async fn issue_507_whips_pause_and_stay_paused_after_switch_off() {
    use crate::legacy_core::security_revocation::HumanRevocation;
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(&app, "keep-going", "# whip: keep-going\nContinue the work.");
    let pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Krimp".to_string()),
        )
        .expect("create pane");
    app.handle_orchestrate_command(format!(
        "attach {pane_id} keep-going --mode auto --holder none --max 3"
    ));
    while app_event_rx.try_recv().is_ok() {}

    issue_507_switch(&app, HumanRevocation::KillSwitchOn);
    app.sweep_orchestrate_whips();
    app.note_whip_target_idle_with_fire_control(
        &crate::spawn_orchestration::pane_node_id(&pane_id),
        Some("done"),
        /*allow_fire*/ true,
        /*turn_succeeded*/ true,
    );
    assert!(drain_claude_pane_task_events(&mut app_event_rx).is_empty());
    assert_eq!(
        app.orchestrate_whips.get("whip-1").map(|whip| whip.state),
        Some(crate::orchestrate::WhipState::Paused)
    );

    issue_507_switch(&app, HumanRevocation::KillSwitchOff);
    app.sweep_orchestrate_whips();
    assert!(
        drain_claude_pane_task_events(&mut app_event_rx).is_empty(),
        "switch-off resumes no whip"
    );

    app.handle_orchestrate_command("resume whip-1".to_string());
    app.sweep_orchestrate_whips();
    let fired = drain_claude_pane_task_events(&mut app_event_rx);
    assert_eq!(fired.len(), 1, "a person resumed it");
    assert!(fired[0].1.contains("Continue the work."));
}

/// #507 review: the switch events themselves (as `/security` sends them):
/// turning it on pauses armed whips at once, with no sweep in between, so the
/// first sweep after it is turned off fires nothing; turning it off starts
/// no turn, task, op or agent message, and the Claude pane's task keeps
/// waiting.
#[tokio::test]
async fn issue_507_switch_events_pause_whips_at_once_and_start_nothing() -> Result<()> {
    use crate::legacy_core::security_revocation::HumanRevocation;
    let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    let config = app.chat_widget.config_ref().clone();
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&config)).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    write_test_whip(&app, "keep-going", "# whip: keep-going\nContinue the work.");
    let pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Krimp".to_string()),
        )
        .expect("create pane");
    app.handle_orchestrate_command(format!(
        "attach {pane_id} keep-going --mode auto --holder none --max 3"
    ));
    while app_event_rx.try_recv().is_ok() {}

    issue_507_switch(&app, HumanRevocation::KillSwitchOn);
    app.submit_claude_pane_task(pane_id.clone(), "pf507 waiting".to_string());
    let control =
        Box::pin(app.handle_event(&mut tui, &mut app_server, AppEvent::KillSwitchTurnedOn)).await?;
    assert!(matches!(control, AppRunControl::Continue));
    assert_eq!(
        app.orchestrate_whips.get("whip-1").map(|whip| whip.state),
        Some(crate::orchestrate::WhipState::Paused),
        "paused when the switch went on, not at the next sweep"
    );

    issue_507_switch(&app, HumanRevocation::KillSwitchOff);
    Box::pin(app.handle_event(&mut tui, &mut app_server, AppEvent::KillSwitchTurnedOff)).await?;
    app.sweep_orchestrate_whips();
    while let Ok(event) = app_event_rx.try_recv() {
        assert!(
            !matches!(
                event,
                AppEvent::CodexOp(_)
                    | AppEvent::SubmitSpawnAgentTask { .. }
                    | AppEvent::SubmitSpawnClaudePaneTask { .. }
                    | AppEvent::SendSpawnAgentMailboxMessage { .. }
            ),
            "switch-off sent work"
        );
    }
    assert!(op_rx.try_recv().is_err(), "switch-off submitted an op");
    assert!(app.claude_panes.prepared_prompts.is_empty());
    assert_eq!(
        app.kill_switch_held_claude_tasks,
        vec![(pane_id, "pf507 waiting".to_string())]
    );
    Ok(())
}

/// #507 review: a Claude turn the kill switch stopped may only finish after
/// the switch is off; its report reaches the parent but starts no turn there.
#[tokio::test]
async fn issue_507_report_of_a_stopped_claude_turn_starts_no_parent_turn() {
    use crate::legacy_core::security_revocation::HumanRevocation;
    let (mut app, mut app_event_rx, troll_pane_id, orc_pane_id) =
        make_child_report_auto_claude_pane_app().await;
    let codex_home = app.config.codex_home.to_path_buf();
    let running = app
        .claude_panes
        .prepare_turn(&orc_pane_id, "long task".to_string(), &codex_home)
        .expect("a running Orc turn");
    issue_507_switch(&app, HumanRevocation::KillSwitchOn);
    app.stop_claude_panes_for_kill_switch();
    issue_507_switch(&app, HumanRevocation::KillSwitchOff);
    drop(running);
    let output = interrupted_claude_turn(&app);
    app.on_claude_pane_turn_finished(orc_pane_id.clone(), Ok(output));

    assert!(
        drain_claude_pane_task_events(&mut app_event_rx).is_empty(),
        "no auto-processing turn for the parent"
    );
    assert!(!app.claude_panes.claude_pane_is_running(&troll_pane_id));
    let troll_node = crate::spawn_orchestration::pane_node_id(&troll_pane_id);
    assert_eq!(
        app.spawn_parent_reports_by_node
            .get(&troll_node)
            .map(std::collections::VecDeque::len),
        Some(1),
        "the parent still has the report"
    );
}

/// #507 review: a removed Claude pane's waiting task is dropped (its sender
/// is told), and the switch-off notice does not name it.
#[tokio::test]
async fn issue_507_removed_pane_drops_its_waiting_task() {
    use crate::legacy_core::security_revocation::HumanRevocation;
    let (mut app, _app_event_rx, _troll_pane_id, orc_pane_id) =
        make_child_report_auto_claude_pane_app().await;
    issue_507_switch(&app, HumanRevocation::KillSwitchOn);
    app.submit_claude_pane_task(orc_pane_id.clone(), "pf507 waiting".to_string());
    app.claude_panes
        .remove_managed_crew_pane(&orc_pane_id, app.config.codex_home.as_ref())
        .expect("remove the pane");
    app.drop_held_claude_task_of_removed_pane(&orc_pane_id);
    assert!(app.kill_switch_held_claude_tasks.is_empty());
}

/// #507 review: while the switch is on, a child report starts no turn in a
/// Claude parent either (it would only wait, and push out the parent's real
/// waiting task); the report is kept for the parent's next turn.
#[tokio::test]
async fn issue_507_child_report_while_on_starts_no_parent_turn() {
    use crate::legacy_core::security_revocation::HumanRevocation;
    let (mut app, mut app_event_rx, troll_pane_id, orc_pane_id) =
        make_child_report_auto_claude_pane_app().await;
    issue_507_switch(&app, HumanRevocation::KillSwitchOn);
    app.submit_claude_pane_task(troll_pane_id.clone(), "pf507 real task".to_string());
    app.record_spawn_child_report_for_claude_pane(&orc_pane_id, "done", Some("result text"));
    assert!(drain_claude_pane_task_events(&mut app_event_rx).is_empty());
    assert_eq!(
        app.kill_switch_held_claude_tasks,
        vec![(troll_pane_id.clone(), "pf507 real task".to_string())],
        "the real task still waits"
    );
    let troll_node = crate::spawn_orchestration::pane_node_id(&troll_pane_id);
    assert_eq!(
        app.spawn_parent_reports_by_node
            .get(&troll_node)
            .map(std::collections::VecDeque::len),
        Some(1)
    );
}

/// #507 review: the report of a stopped Claude turn reaches a native parent
/// as mail that does not ask for a turn; an ordinary report still does.
#[tokio::test]
async fn issue_507_stopped_claude_report_to_a_native_parent_asks_for_no_turn() {
    use crate::legacy_core::security_revocation::HumanRevocation;
    let (mut app, mut app_event_rx, _troll_pane_id, orc_pane_id) =
        make_child_report_auto_claude_pane_app().await;
    let parent_thread =
        ThreadId::from_string("00000000-0000-0000-0000-000000000508").expect("thread id");
    app.spawn_parent_by_node.insert(
        crate::spawn_orchestration::pane_node_id(&orc_pane_id),
        crate::spawn_orchestration::thread_node_id(parent_thread),
    );
    let codex_home = app.config.codex_home.to_path_buf();
    let mail = |rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>| {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|event| match event {
                AppEvent::SendSpawnAgentMailboxMessage { params } => Some(params.trigger_turn),
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    let running = app
        .claude_panes
        .prepare_turn(&orc_pane_id, "long task".to_string(), &codex_home)
        .expect("a running Orc turn");
    issue_507_switch(&app, HumanRevocation::KillSwitchOn);
    app.stop_claude_panes_for_kill_switch();
    issue_507_switch(&app, HumanRevocation::KillSwitchOff);
    drop(running);
    let output = interrupted_claude_turn(&app);
    app.on_claude_pane_turn_finished(orc_pane_id.clone(), Ok(output));
    assert_eq!(mail(&mut app_event_rx), vec![false]);

    app.record_spawn_child_report_for_claude_pane(&orc_pane_id, "done", Some("result text"));
    assert_eq!(
        mail(&mut app_event_rx),
        vec![true],
        "control: a normal report"
    );
}

/// #507 review: a task for a Claude pane that does not exist is refused, not
/// held, while the switch is on.
#[tokio::test]
async fn issue_507_no_waiting_task_for_a_missing_claude_pane() {
    use crate::legacy_core::security_revocation::HumanRevocation;
    let (mut app, _app_event_rx, _troll_pane_id, _orc_pane_id) =
        make_child_report_auto_claude_pane_app().await;
    issue_507_switch(&app, HumanRevocation::KillSwitchOn);
    app.submit_claude_pane_task("claude-missing".to_string(), "pf507 task".to_string());
    assert!(app.kill_switch_held_claude_tasks.is_empty());
}

#[tokio::test]
async fn child_report_auto_starts_turn_on_idle_claude_pane_parent() {
    let (mut app, mut app_event_rx, troll_pane_id, orc_pane_id) =
        make_child_report_auto_claude_pane_app().await;

    assert!(!app.claude_panes.claude_pane_is_running(&troll_pane_id));
    app.record_spawn_child_report_for_claude_pane(&orc_pane_id, "done", Some("result text"));

    let submitted_tasks = drain_claude_pane_task_events(&mut app_event_rx);
    assert_eq!(submitted_tasks.len(), 1);
    let (pane_id, task) = &submitted_tasks[0];
    assert_eq!(pane_id, &troll_pane_id);
    assert!(task.contains("A child pane has reported back"));
    assert!(task.contains("result text"));
}

#[tokio::test]
async fn child_report_auto_does_not_start_turn_on_running_claude_pane_parent() {
    let (mut app, mut app_event_rx, troll_pane_id, orc_pane_id) =
        make_child_report_auto_claude_pane_app().await;
    let _prepared = app
        .claude_panes
        .prepare_turn(
            &troll_pane_id,
            "already running".to_string(),
            app.config.codex_home.as_ref(),
        )
        .expect("prepare running Troll pane");

    assert!(app.claude_panes.claude_pane_is_running(&troll_pane_id));
    app.record_spawn_child_report_for_claude_pane(&orc_pane_id, "done", Some("result text"));

    let submitted_tasks = drain_claude_pane_task_events(&mut app_event_rx);
    assert!(
        submitted_tasks
            .iter()
            .all(|(pane_id, _)| pane_id != &troll_pane_id)
    );
}

#[tokio::test]
async fn child_report_auto_duplicate_child_report_does_not_trigger_duplicate_turns() {
    let (mut app, mut app_event_rx, troll_pane_id, orc_pane_id) =
        make_child_report_auto_claude_pane_app().await;

    app.record_spawn_child_report_for_claude_pane(&orc_pane_id, "done", Some("result text"));
    app.record_spawn_child_report_for_claude_pane(&orc_pane_id, "done", Some("result text"));

    let submitted_tasks = drain_claude_pane_task_events(&mut app_event_rx);
    assert_eq!(
        submitted_tasks
            .iter()
            .filter(|(pane_id, _)| pane_id == &troll_pane_id)
            .count(),
        1
    );
}

#[tokio::test]
async fn external_pane_auto_dispatch_chain_pauses_and_resumes_on_fresh_input() {
    let (mut app, mut app_event_rx, troll_pane_id, orc_pane_id) =
        make_child_report_auto_claude_pane_app().await;
    let troll_node_id = crate::spawn_orchestration::pane_node_id(&troll_pane_id);

    for cycle in 0..codex_protocol::crew::CREW_AUTO_DISPATCH_CHAIN_LIMIT {
        app.record_spawn_child_report_for_claude_pane(
            &orc_pane_id,
            "done",
            Some(&format!("cycle {cycle} result")),
        );
        let auto_tasks = drain_claude_pane_task_events(&mut app_event_rx);
        assert_eq!(
            auto_tasks
                .iter()
                .filter(|(pane_id, _)| pane_id == &troll_pane_id)
                .count(),
            1
        );
        app.note_spawn_turn_started_for_auto_loop(&troll_node_id);
        app.dispatch_spawn_task_blocks(
            &troll_pane_id,
            vec![crate::spawn_orchestration::SpawnTaskDispatch {
                target: orc_pane_id.clone(),
                task: format!("cycle {cycle} follow-up"),
                seq: Some(700 + u64::from(cycle)),
            }],
        );
        let dispatched_tasks = drain_claude_pane_task_events(&mut app_event_rx);
        assert_eq!(
            dispatched_tasks
                .iter()
                .filter(|(pane_id, _)| pane_id == &orc_pane_id)
                .count(),
            1
        );
        app.note_spawn_turn_completed_for_auto_loop(&troll_node_id);
    }

    app.record_spawn_child_report_for_claude_pane(
        &orc_pane_id,
        "done",
        Some("the report after the configured chain limit"),
    );
    assert!(
        drain_claude_pane_task_events(&mut app_event_rx)
            .iter()
            .all(|(pane_id, _)| pane_id != &troll_pane_id),
        "the next report must remain visible without starting another paid parent turn"
    );
    assert_eq!(
        app.spawn_auto_loop_state_by_node[&troll_node_id].chain,
        codex_protocol::crew::CREW_AUTO_DISPATCH_CHAIN_LIMIT
    );

    // A real user task is represented by a non-pending turn start and resumes the same pane.
    app.note_spawn_turn_started_for_auto_loop(&troll_node_id);
    app.note_spawn_turn_completed_for_auto_loop(&troll_node_id);
    app.record_spawn_child_report_for_claude_pane(
        &orc_pane_id,
        "done",
        Some("fresh result after operator input"),
    );
    assert_eq!(
        drain_claude_pane_task_events(&mut app_event_rx)
            .iter()
            .filter(|(pane_id, _)| pane_id == &troll_pane_id)
            .count(),
        1
    );
}

#[tokio::test]
async fn external_pane_auto_acknowledgement_terminates_dispatch_chain() {
    let (mut app, mut app_event_rx, troll_pane_id, orc_pane_id) =
        make_child_report_auto_claude_pane_app().await;
    let troll_node_id = crate::spawn_orchestration::pane_node_id(&troll_pane_id);

    app.record_spawn_child_report_for_claude_pane(&orc_pane_id, "done", Some("first result"));
    drain_claude_pane_task_events(&mut app_event_rx);
    app.note_spawn_turn_started_for_auto_loop(&troll_node_id);
    app.dispatch_spawn_task_blocks(
        &troll_pane_id,
        vec![crate::spawn_orchestration::SpawnTaskDispatch {
            target: orc_pane_id.clone(),
            task: "one follow-up".to_string(),
            seq: Some(801),
        }],
    );
    drain_claude_pane_task_events(&mut app_event_rx);
    app.note_spawn_turn_completed_for_auto_loop(&troll_node_id);
    assert_eq!(app.spawn_auto_loop_state_by_node[&troll_node_id].chain, 1);

    app.record_spawn_child_report_for_claude_pane(
        &orc_pane_id,
        "done",
        Some("acknowledge this result"),
    );
    drain_claude_pane_task_events(&mut app_event_rx);
    app.note_spawn_turn_started_for_auto_loop(&troll_node_id);
    app.note_spawn_turn_completed_for_auto_loop(&troll_node_id);
    assert_eq!(app.spawn_auto_loop_state_by_node[&troll_node_id].chain, 0);
}

#[tokio::test]
async fn native_nazgul_sees_live_troll_and_orc_tree_even_if_spawned_before_them() {
    // The bug: a spawned Nazgul's base_instructions are frozen at spawn time, so if it was created
    // before its Troll/Orcs it would forever answer "none spawned yet". The fix renders the live
    // hierarchy per turn via spawn_context_for_thread. This test proves the live view sees a Troll
    // and two Orcs that exist now, regardless of when the Nazgul was spawned.
    let mut app = make_test_app().await;
    let nazgul_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000601").expect("valid thread id");
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000602").expect("valid thread id");
    let orc_a =
        ThreadId::from_string("00000000-0000-0000-0000-000000000603").expect("valid thread id");
    let orc_b =
        ThreadId::from_string("00000000-0000-0000-0000-000000000604").expect("valid thread id");

    app.upsert_agent_picker_thread(
        nazgul_thread_id,
        Some("Euclid".to_string()),
        Some("nazgul".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        orc_a,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        orc_b,
        Some("Ghash".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_thread
        .insert(troll_thread_id, nazgul_thread_id);
    app.spawn_parent_by_thread.insert(orc_a, troll_thread_id);
    app.spawn_parent_by_thread.insert(orc_b, troll_thread_id);

    let context = app
        .spawn_context_for_thread(nazgul_thread_id)
        .expect("Nazgul thread should receive live spawn context");
    assert!(context.contains("You are the Corbanu Terminal Nazgul/root pane"));
    assert!(
        context.contains("Euclid [nazgul]"),
        "context uses the Nazgul picker label, got: {context}"
    );
    assert!(
        context.contains("built-in nazgul.toml agent config"),
        "native Nazgul role prompt should come from role config, got: {context}"
    );
    assert!(
        !context.contains("You are not an individual contributor or coder"),
        "native Nazgul context should not duplicate the persistent role prompt, got: {context}"
    );
    assert!(
        context.contains("Burzum [troll]"),
        "Nazgul must see the live Troll, got: {context}"
    );
    assert!(
        context.contains("Snaga [orc]"),
        "Nazgul must see the live Orc Snaga, got: {context}"
    );
    assert!(
        context.contains("Ghash [orc]"),
        "Nazgul must see the live Orc Ghash, got: {context}"
    );
    assert!(
        !context.contains("none spawned yet"),
        "Nazgul must not claim the tree is empty when it is not"
    );
}

#[tokio::test]
async fn native_agent_text_is_never_used_as_dispatch_transport() {
    let (mut app, mut rx, _op_rx) = make_test_app_with_channels().await;
    let nazgul_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000648").expect("valid thread id");
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000649").expect("valid thread id");
    let snaga_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000650").expect("valid thread id");

    app.upsert_agent_picker_thread(
        nazgul_thread_id,
        Some("Angmar".to_string()),
        Some("nazgul".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        snaga_thread_id,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_thread
        .insert(troll_thread_id, nazgul_thread_id);
    app.spawn_parent_by_thread
        .insert(snaga_thread_id, troll_thread_id);

    let partial_message = r#"Dispatch started.
<pfterminal_send_task target="Snaga">
Reply with exactly OK."#;

    app.update_spawn_status_for_thread_notification(&item_completed_notification(
        troll_thread_id,
        "turn-partial-item-dispatch",
        "agent-message-partial-item-dispatch",
        partial_message,
    ));
    assert!(
        drain_spawn_agent_task_for(&mut rx, snaga_thread_id).is_none(),
        "partial item blocks must not dispatch"
    );

    app.update_spawn_status_for_thread_notification(&turn_completed_with_agent_message(
        troll_thread_id,
        "turn-partial-item-dispatch",
        TurnStatus::Interrupted,
        partial_message,
    ));
    assert!(
        drain_spawn_agent_task_for(&mut rx, snaga_thread_id).is_none(),
        "interrupted turn catch-all must not dispatch partial blocks"
    );

    let complete_legacy_block = r#"<pfterminal_send_task target="Snaga">
Reply with exactly OK.
</pfterminal_send_task>"#;
    app.update_spawn_status_for_thread_notification(&turn_completed_with_agent_message(
        troll_thread_id,
        "turn-complete-legacy-block",
        TurnStatus::Completed,
        complete_legacy_block,
    ));
    assert!(
        drain_spawn_agent_task_for(&mut rx, snaga_thread_id).is_none(),
        "native agents must dispatch with collaboration tools, never assistant-text tags"
    );
}

#[tokio::test]
async fn interrupted_turn_never_dispatches_even_with_complete_blocks() {
    let (mut app, mut rx, _op_rx) = make_test_app_with_channels().await;
    let nazgul_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000651").expect("valid thread id");
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000652").expect("valid thread id");

    app.upsert_agent_picker_thread(
        nazgul_thread_id,
        Some("Angmar".to_string()),
        Some("nazgul".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_thread
        .insert(troll_thread_id, nazgul_thread_id);

    let message = r#"Dispatching.
<pfterminal_send_task target="Burzum">
Task: um to remove the artifact and stop generating unvalidated busywork.
</pfterminal_send_task>
More text that never finished"#;

    for status in [TurnStatus::Interrupted, TurnStatus::Failed] {
        app.update_spawn_status_for_thread_notification(&turn_completed_with_agent_message(
            nazgul_thread_id,
            "turn-interrupted-dispatch",
            status.clone(),
            message,
        ));
        while let Ok(event) = rx.try_recv() {
            if let AppEvent::SubmitSpawnAgentTask { thread_id, .. } = event
                && thread_id == troll_thread_id
            {
                panic!("{status:?} turn must never dispatch spawn task blocks");
            }
        }
    }
}

#[tokio::test]
async fn manual_troll_spawn_uses_bound_native_nazgul_as_core_parent() {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000305").expect("valid main id");
    let nazgul_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000306").expect("valid Nazgul id");
    let nazgul_node_id = crate::spawn_orchestration::thread_node_id(nazgul_thread_id);

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.upsert_agent_picker_thread(
        nazgul_thread_id,
        Some("Angmar".to_string()),
        Some("nazgul".to_string()),
        /*is_closed*/ false,
    );
    app.set_spawn_nazgul_pane_binding(nazgul_node_id.clone());

    assert_eq!(
        app.backend_parent_thread_for_spawn(
            crate::spawn_orchestration::SpawnRole::Troll,
            Some(&nazgul_node_id),
        ),
        Some(nazgul_thread_id)
    );
    assert_eq!(
        app.logical_parent_node_for_spawn(
            crate::spawn_orchestration::SpawnRole::Troll,
            Some(&nazgul_node_id),
        ),
        nazgul_node_id
    );
}

#[tokio::test]
async fn active_native_nazgul_turn_receives_live_spawn_context_with_orcs() {
    // Direct user input in the active Nazgul pane uses the normal active-thread submit path, not
    // SubmitSpawnAgentTask. It still must receive live hierarchy context; otherwise the Nazgul sees
    // only spawn-time instructions and says no Orcs exist even while /spawn status lists them.
    let mut app = make_test_app().await;
    let nazgul_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000611").expect("valid thread id");
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000612").expect("valid thread id");
    let orc_a =
        ThreadId::from_string("00000000-0000-0000-0000-000000000613").expect("valid thread id");
    let orc_b =
        ThreadId::from_string("00000000-0000-0000-0000-000000000614").expect("valid thread id");

    app.active_thread_id = Some(nazgul_thread_id);
    app.upsert_agent_picker_thread(
        nazgul_thread_id,
        Some("Euclid".to_string()),
        Some("nazgul".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        orc_a,
        Some("Snaga".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        orc_b,
        Some("Ghash".to_string()),
        Some("orc".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_thread
        .insert(troll_thread_id, nazgul_thread_id);
    app.spawn_parent_by_thread.insert(orc_a, troll_thread_id);
    app.spawn_parent_by_thread.insert(orc_b, troll_thread_id);

    let context_map = app
        .spawn_additional_context_for_thread(nazgul_thread_id)
        .expect("active Nazgul turn should receive live spawn context");
    let context = &context_map
        .get("pfterminal_spawn_context")
        .expect("spawn context entry")
        .value;

    assert!(context.contains("Euclid [nazgul]"), "got: {context}");
    assert!(context.contains("Burzum [troll]"), "got: {context}");
    assert!(context.contains("Snaga [orc]"), "got: {context}");
    assert!(context.contains("Ghash [orc]"), "got: {context}");
    assert!(!context.contains("none spawned yet"), "got: {context}");
    assert!(context.contains("send_message"), "got: {context}");
    assert!(context.contains("followup_task"), "got: {context}");
    assert!(!context.contains("<pfterminal_send_task"), "got: {context}");
}

#[tokio::test]
async fn claude_orc_completion_uses_core_edge_message_not_native_prompt_reinjection() {
    let mut app = make_test_app().await;
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000234").expect("valid thread id");
    app.upsert_agent_picker_thread(
        troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    let orc_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Snaga".to_string()),
        )
        .expect("create Claude Orc pane");
    app.spawn_parent_by_node.insert(
        crate::spawn_orchestration::pane_node_id(&orc_pane_id),
        crate::spawn_orchestration::thread_node_id(troll_thread_id),
    );

    app.on_claude_pane_turn_finished(
        orc_pane_id,
        Ok(crate::claude_panes::ClaudePaneTurnOutput {
            text: "Finished the latency benchmark table and saved the output.".to_string(),
            status: crate::claude_panes::ClaudePaneTurnStatus::Success,
            session_id: Some("claude-session".to_string()),
            usage_summary: None,
            turn_usage_summary: None,
            usage_status: crate::claude_panes::ClaudePaneUsageStatus::Missing,
            artifact_path: app.config.cwd.join("turn-0001.jsonl").to_path_buf(),
            audit_path: app.config.cwd.join("turn-0001.audit.json").to_path_buf(),
            duration_ms: 1,
            terminal_reason: None,
            error_summary: None,
            tool_names: Vec::new(),
            tool_events: Vec::new(),
            reasoning_events: Vec::new(),
            command_mode: crate::claude_panes::ClaudeCommandMode::NewSession,
            direct_accounting: None,
        }),
    );

    let context = app
        .spawn_context_for_thread(troll_thread_id)
        .expect("Troll should receive live lifecycle context");
    assert!(!context.contains("Recent child reports delivered to this pane:"));
    assert!(context.contains("Claude Code Snaga [orc] - Opus 5.5 Claude Plan"));
    assert!(context.contains("status=idle"));
    assert!(!context.contains("result=Finished the latency benchmark table and saved the output."));
}

#[tokio::test]
async fn unbound_main_does_not_persist_nazgul_role_metadata() {
    let mut app = make_test_app().await;
    let codex_home = tempdir().expect("codex home");
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    app.config.sqlite = codex_state::SqliteConfig::new_for_testing(codex_home.path().abs());
    let state_db = codex_state::StateRuntime::init(
        app.config.sqlite.clone(),
        app.config.model_provider_id.clone(),
    )
    .await
    .expect("state db");
    app.state_db = Some(state_db.clone());

    let root_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000237").expect("valid thread id");
    app.primary_thread_id = Some(root_thread_id);
    app.active_thread_id = Some(root_thread_id);
    app.spawn_nazgul_pane_id = None;

    app.persist_bound_nazgul_root_thread_metadata().await;

    assert!(
        state_db
            .get_thread(root_thread_id)
            .await
            .expect("read metadata")
            .is_none(),
        "an unbound primary thread must not be persisted as Nazgul"
    );
}

#[tokio::test]
async fn bound_nazgul_root_persists_role_metadata_to_state_db() {
    let mut app = make_test_app().await;
    let codex_home = tempdir().expect("codex home");
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    app.config.sqlite = codex_state::SqliteConfig::new_for_testing(codex_home.path().abs());
    let state_db = codex_state::StateRuntime::init(
        app.config.sqlite.clone(),
        app.config.model_provider_id.clone(),
    )
    .await
    .expect("state db");
    app.state_db = Some(state_db.clone());

    let root_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000236").expect("valid thread id");
    app.primary_thread_id = Some(root_thread_id);
    app.active_thread_id = Some(root_thread_id);
    let root_node_id = crate::spawn_orchestration::thread_node_id(root_thread_id);
    app.spawn_native_runtime_by_node.insert(
        root_node_id.clone(),
        crate::dispatch_queue::SavedNativeSpawnRuntime {
            model: codex_model_provider_info::CLAUDE_FABLE_5_PLAN_MODEL.to_string(),
            provider: CLAUDE_PLAN_PROVIDER_ID.to_string(),
            reasoning_effort: Some(ReasoningEffortConfig::High),
        },
    );
    app.set_spawn_nazgul_pane_binding(root_node_id);

    app.persist_bound_nazgul_root_thread_metadata().await;

    let metadata = state_db
        .get_thread(root_thread_id)
        .await
        .expect("read metadata")
        .expect("root row should be persisted");
    assert_eq!(metadata.agent_role.as_deref(), Some("nazgul"));
    assert_eq!(metadata.agent_nickname.as_deref(), Some("Main"));
    assert_eq!(
        metadata.model.as_deref(),
        Some(codex_model_provider_info::CLAUDE_FABLE_5_PLAN_MODEL)
    );
    assert_eq!(metadata.model_provider, CLAUDE_PLAN_PROVIDER_ID);
    assert_eq!(metadata.reasoning_effort, Some(ReasoningEffortConfig::High));
}

#[tokio::test]
async fn native_spawn_registration_persists_started_session_model_provider_pair() {
    let mut app = make_test_app().await;
    let codex_home = tempdir().expect("codex home");
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    app.config.sqlite = codex_state::SqliteConfig::new_for_testing(codex_home.path().abs());
    app.config.model_provider_id = "claude-plan".to_string();
    let state_db = codex_state::StateRuntime::init(
        app.config.sqlite.clone(),
        app.config.model_provider_id.clone(),
    )
    .await
    .expect("state db");
    app.state_db = Some(state_db.clone());

    let parent_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000238").expect("valid thread id");
    let troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000239").expect("valid thread id");
    app.upsert_agent_picker_thread(
        parent_thread_id,
        Some("Main".to_string()),
        Some("nazgul".to_string()),
        /*is_closed*/ false,
    );

    let started = crate::app_server_session::AppServerStartedThread {
        session: ThreadSessionState {
            model: "gpt-5.5".to_string(),
            model_provider_id: "openai".to_string(),
            reasoning_effort: Some(ReasoningEffortConfig::XHigh),
            ..test_thread_session(troll_thread_id, test_path_buf("/tmp/project"))
        },
        turns: Vec::new(),
        blocks_direct_input: false,
    };
    app.register_spawn_agent_pane(
        troll_thread_id,
        parent_thread_id,
        crate::spawn_orchestration::thread_node_id(parent_thread_id),
        Some("Burzum".to_string()),
        "troll",
        started,
        /*persist_layout*/ true,
    )
    .await;

    let metadata = state_db
        .get_thread(troll_thread_id)
        .await
        .expect("read metadata")
        .expect("spawn row should be persisted");
    assert_eq!(metadata.agent_role.as_deref(), Some("troll"));
    assert_eq!(metadata.agent_nickname.as_deref(), Some("Burzum"));
    assert_eq!(metadata.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(metadata.model_provider, "openai");
    assert_eq!(
        metadata.reasoning_effort,
        Some(ReasoningEffortConfig::XHigh)
    );
}

async fn codex_user_pane_remains_interactive_after_liveness_refresh_impl() -> Result<()> {
    let (app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let mut app = Box::new(app);
    let mut app_server = start_config_write_test_app_server(&app).await?;
    let main = app_server.start_thread(&app.config).await?;
    let main_thread_id = main.session.thread_id;
    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.primary_session_configured = Some(main.session.clone());
    app.thread_event_channels.insert(
        main_thread_id,
        ThreadEventChannel::new_with_session(/*capacity*/ 4, main.session, main.turns),
    );

    let pane_config = app.native_spawn_agent_config()?;
    let model = app.chat_widget.current_model().to_string();
    let provider = app.config.model_provider_id.clone();
    let started = app_server
        .start_user_pane_thread(
            &pane_config,
            model,
            Some(provider),
            app.chat_widget.current_reasoning_effort(),
        )
        .await?;
    assert!(
        !started.blocks_direct_input,
        "operator-created panes must be server-authored user threads"
    );
    let pane_rollout_path = started
        .session
        .rollout_path
        .as_ref()
        .expect("user-pane rollout path");
    assert!(
        pane_rollout_path.is_file(),
        "user-pane rollout must be durable"
    );
    let pane_thread_id = started.session.thread_id;
    app.register_codex_user_pane(
        &mut app_server,
        pane_thread_id,
        Some("Interactive".to_string()),
        started,
    )
    .await;

    let persisted_pane = app_server
        .thread_read(pane_thread_id, /*include_turns*/ false)
        .await?;
    assert_eq!(
        persisted_pane.name.as_deref(),
        Some("Interactive"),
        "user-pane name must persist"
    );

    let mut tui = crate::tui::test_support::make_test_tui()?;
    // Selection carries the full replay/reattachment state machine. Pin that nested future on the
    // heap so this test also guards against accidentally embedding it in its caller's stack frame.
    Box::pin(app.select_agent_thread(&mut tui, &mut app_server, pane_thread_id)).await?;
    while app_event_rx.try_recv().is_ok() {}
    assert!(!app.agent_navigation.is_parent_owned(pane_thread_id));
    assert_eq!(
        app.agent_navigation
            .get(&pane_thread_id)
            .and_then(|entry| entry.agent_nickname.as_deref()),
        Some("Interactive"),
        "liveness refresh must not erase the operator-assigned pane name"
    );

    app.chat_widget
        .restore_user_message_to_composer("operator pane input".into());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(
        std::iter::from_fn(|| app_event_rx.try_recv().ok())
            .any(|event| matches!(event, AppEvent::CodexOp(Op::UserTurn { .. }))),
        "operator-created pane input must reach the normal user-turn path"
    );
    app_server.shutdown().await?;
    Ok(())
}

#[test]
fn codex_user_pane_remains_interactive_after_liveness_refresh() -> Result<()> {
    // The Rust test harness uses a much smaller stack than the terminal process. Run this
    // full-App integration fixture on an explicitly sized test thread; the production behavior
    // remains exercised with the ordinary Tokio runtime and no environment-variable override.
    std::thread::Builder::new()
        .name("codex-user-pane-liveness".to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(codex_user_pane_remains_interactive_after_liveness_refresh_impl())
        })
        .expect("spawn user-pane liveness test thread")
        .join()
        .expect("user-pane liveness test thread panicked")
}

#[tokio::test]
async fn human_addressable_spawn_pane_remains_interactive_after_liveness_refresh() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let mut app_server = start_config_write_test_app_server(&app).await?;
    let main = app_server.start_thread(&app.config).await?;
    let main_thread_id = main.session.thread_id;
    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.primary_session_configured = Some(main.session.clone());
    app.thread_event_channels.insert(
        main_thread_id,
        ThreadEventChannel::new_with_session(/*capacity*/ 4, main.session, main.turns),
    );

    let spawn_config = app.native_spawn_agent_config()?;
    let model = app.chat_widget.current_model().to_string();
    let provider = app.config.model_provider_id.clone();
    let started = app_server
        .spawn_agent_thread(
            &spawn_config,
            main_thread_id,
            "nazgul".to_string(),
            Some("Angmar".to_string()),
            model,
            Some(provider),
            app.chat_widget.current_reasoning_effort(),
            /*base_instructions*/ None,
        )
        .await?;
    assert!(
        !started.blocks_direct_input,
        "human-addressable /spawn panes must accept operator input"
    );
    let pane_thread_id = started.session.thread_id;
    app.register_spawn_agent_pane(
        pane_thread_id,
        main_thread_id,
        crate::spawn_orchestration::thread_node_id(main_thread_id),
        Some("Angmar".to_string()),
        "nazgul",
        started,
        /*persist_layout*/ true,
    )
    .await;

    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.select_agent_thread(&mut tui, &mut app_server, pane_thread_id)
        .await?;
    while app_event_rx.try_recv().is_ok() {}
    assert!(!app.agent_navigation.is_parent_owned(pane_thread_id));

    app.chat_widget
        .restore_user_message_to_composer("manage the Troll and Orc review crew".into());
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(
        std::iter::from_fn(|| app_event_rx.try_recv().ok())
            .any(|event| matches!(event, AppEvent::CodexOp(Op::UserTurn { .. }))),
        "operator input in a human-addressable /spawn pane must reach the normal user-turn path"
    );
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn native_spawn_config_inherits_active_thread_permissions_not_stale_app_defaults() {
    let mut app = make_test_app().await;

    // Reproduce the real boundary: the app-level config still contains persisted defaults while
    // the active thread reflects CLI/runtime permission overrides.
    app.config
        .permissions
        .set_permission_profile(PermissionProfile::workspace_write())
        .expect("workspace profile should be accepted");
    app.config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("on-request should be accepted");
    app.chat_widget
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::legacy(
            PermissionProfile::Disabled,
        ))
        .expect("danger-full-access profile should be accepted");
    app.chat_widget.set_approval_policy(AskForApproval::Never);

    let spawn_config = app
        .native_spawn_agent_config()
        .expect("native spawn config");

    assert_eq!(
        spawn_config.permissions.approval_policy.value(),
        AskForApproval::Never.to_core()
    );
    assert_eq!(
        spawn_config.permissions.effective_permission_profile(),
        PermissionProfile::Disabled
    );
    assert_eq!(spawn_config.permissions.active_permission_profile(), None);
}

#[tokio::test]
async fn app_server_spawn_preserves_active_thread_yolo_permissions() -> Result<()> {
    let mut app = make_test_app().await;
    let mut app_server = start_config_write_test_app_server(&app).await?;
    let main = app_server.start_thread(&app.config).await?;

    app.chat_widget
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::legacy(
            PermissionProfile::Disabled,
        ))?;
    app.chat_widget.set_approval_policy(AskForApproval::Never);
    let spawn_config = app.native_spawn_agent_config()?;
    assert_eq!(
        spawn_config.permissions.approval_policy.value(),
        AskForApproval::Never.to_core()
    );
    assert_eq!(
        spawn_config.permissions.effective_permission_profile(),
        PermissionProfile::Disabled
    );
    assert_eq!(spawn_config.permissions.active_permission_profile(), None);
    let started = app_server
        .spawn_agent_thread(
            &spawn_config,
            main.session.thread_id,
            "nazgul".to_string(),
            Some("Angmar".to_string()),
            app.chat_widget.current_model().to_string(),
            Some(app.config.model_provider_id.clone()),
            app.chat_widget.current_reasoning_effort(),
            /*base_instructions*/ None,
        )
        .await?;

    assert_eq!(started.session.approval_policy, AskForApproval::Never);
    assert_eq!(
        started.session.permission_profile,
        PermissionProfile::Disabled
    );
    assert_eq!(started.session.active_permission_profile, None);
    app_server.shutdown().await?;
    Ok(())
}

#[test]
#[ignore = "live integration test: spawns real threads and needs Claude Plan/OpenAI/OpenRouter provider auth present in the environment"]
fn spawn_app_path_creates_default_heterogeneous_crew() -> Result<()> {
    const WORKER_THREADS: usize = 1;
    const TEST_STACK_SIZE_BYTES: usize = 8 * 1024 * 1024;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .thread_stack_size(TEST_STACK_SIZE_BYTES)
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let mut app = make_test_app().await;
        if let Some(env_key) = app.config.model_provider.env_key.as_deref() {
            std::fs::write(
                app.config.codex_home.join("provider_auth.json"),
                format!(r#"{{"api_keys":{{"{env_key}":"test-key"}}}}"#),
            )?;
        }
        let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(
            app.chat_widget.config_ref(),
        ))
        .await?;
        let main = app_server
            .start_thread(app.chat_widget.config_ref())
            .await?;
        let main_thread_id = main.session.thread_id;
        app.primary_thread_id = Some(main_thread_id);
        app.active_thread_id = Some(main_thread_id);
        app.primary_session_configured = Some(main.session.clone());

        let native_spawn_config = app.native_spawn_agent_config()?;
        assert!(native_spawn_config.features.enabled(Feature::MultiAgentV2));
        assert!(
            native_spawn_config
                .features
                .enabled(Feature::MultiAgentMode)
        );

        let (nazgul_thread_id, troll_thread_id) =
            app.create_spawn_standard_crew(&mut app_server).await?;
        // The Nazgul is parented to Codex Main and auto-bound as root.
        assert_eq!(
            app.spawn_parent_by_thread.get(&nazgul_thread_id),
            Some(&main_thread_id)
        );
        assert_eq!(
            app.spawn_nazgul_pane_id.as_deref(),
            Some(crate::spawn_orchestration::thread_node_id(nazgul_thread_id).as_str())
        );
        // The Troll is parented to the Nazgul.
        assert_eq!(
            app.spawn_parent_by_thread.get(&troll_thread_id),
            Some(&nazgul_thread_id)
        );

        let orcs = app
            .agent_navigation
            .ordered_threads()
            .into_iter()
            .filter(|(thread_id, entry)| {
                app.spawn_parent_by_thread.get(thread_id) == Some(&troll_thread_id)
                    && entry.agent_role.as_deref() == Some("orc")
            })
            .collect::<Vec<_>>();
        assert_eq!(orcs.len(), 3);
        assert_eq!(orcs[0].1.agent_nickname.as_deref(), Some("Snaga"));
        assert_eq!(orcs[1].1.agent_nickname.as_deref(), Some("Ghash"));
        assert_eq!(orcs[2].1.agent_nickname.as_deref(), Some("Krimp"));
        assert_eq!(
            orcs.iter()
                .map(|(_, entry)| entry.model.as_deref())
                .collect::<Vec<_>>(),
            vec![
                Some(App::STANDARD_ORC_MODEL),
                Some(App::STANDARD_ORC_2_MODEL),
                Some(App::STANDARD_ORC_3_MODEL),
            ]
        );
        let status_items = app.spawn_tree_items(/*show_task_actions*/ true);
        assert!(
            status_items
                .iter()
                .all(|item| !item.name.contains("Demo task")),
            "spawn status must not expose a built-in demo action"
        );

        Ok(())
    })
}

#[tokio::test]
async fn standard_crew_quick_start_uses_the_expected_role_picker_label() {
    // Smoke test: the /spawn quick-start entry is labeled for the standard crew (Nazgul + Troll +
    // 3 Orcs), without restoring the old demo-task behavior.
    let mut app = make_test_app().await;
    app.open_spawn_role_picker();
    let popup = render_bottom_popup(&app.chat_widget, /*width*/ 120);
    assert_app_snapshot!("spawn_role_picker_with_corbanu_api_crew", popup);
    assert!(
        popup.contains("Create standard crew: Nazgul + Troll + 3 Orcs")
            && popup
                .contains("Create Corbanu API crew: Kimi K3 Nazgul + Luna Troll + 3 Flash Orcs"),
        "both built-in crew choices must remain visible:\n{popup}"
    );
    // The picker is rendered into the chat widget; assert the role-picker path doesn't error and
    // the standard crew constants resolve to the intended models/providers.
    assert_eq!(App::STANDARD_NAZGUL_MODEL, CLAUDE_FABLE_5_PLAN_MODEL);
    assert_eq!(App::STANDARD_TROLL_MODEL, "gpt-5.6-sol");
    assert_eq!(App::STANDARD_ORC_MODEL, "gpt-5.6-luna");
    assert_eq!(App::STANDARD_ORC_2_MODEL, "gpt-5.6-terra");
    assert_eq!(App::STANDARD_ORC_3_MODEL, "x-ai/grok-4.7");
    let orc_runtimes = App::standard_orc_runtimes();
    assert_eq!(
        orc_runtimes
            .iter()
            .map(|(model, provider, effort)| (
                model.as_str(),
                provider.as_str(),
                effort
                    .as_ref()
                    .map(codex_protocol::openai_models::ReasoningEffort::as_str)
            ))
            .collect::<Vec<_>>(),
        vec![
            ("gpt-5.6-luna", OPENAI_PROVIDER_ID, Some("xhigh")),
            ("gpt-5.6-terra", OPENAI_PROVIDER_ID, Some("xhigh")),
            ("x-ai/grok-4.7", OPENROUTER_PROVIDER_ID, None),
        ]
    );
    // Provider resolution for each crew model.
    assert_eq!(
        crate::chatwidget::ChatWidget::model_provider_for_selection(App::STANDARD_NAZGUL_MODEL)
            .as_deref(),
        Some(CLAUDE_PLAN_PROVIDER_ID)
    );
    assert_eq!(
        crate::chatwidget::ChatWidget::model_provider_for_selection(App::STANDARD_TROLL_MODEL)
            .as_deref(),
        Some(OPENAI_PROVIDER_ID)
    );
    assert_eq!(
        crate::chatwidget::ChatWidget::model_provider_for_selection(App::STANDARD_ORC_MODEL)
            .as_deref(),
        Some(OPENAI_PROVIDER_ID)
    );
    assert_eq!(
        crate::chatwidget::ChatWidget::model_provider_for_selection(App::STANDARD_ORC_2_MODEL)
            .as_deref(),
        Some(OPENAI_PROVIDER_ID)
    );
    assert_eq!(
        crate::chatwidget::ChatWidget::model_provider_for_selection(App::STANDARD_ORC_3_MODEL)
            .as_deref(),
        Some(OPENROUTER_PROVIDER_ID)
    );
}

#[tokio::test]
async fn native_spawn_defaults_follow_active_claude_profile() -> Result<()> {
    let mut app = make_test_app().await;
    let pane_id = app
        .claude_panes
        .create_pane_without_vault_for_test(
            crate::claude_panes::ClaudeProviderProfileKind::VercelGlm52Fast,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
        )
        .expect("create Vercel Claude pane");

    app.claude_panes
        .set_active_user_pane(&pane_id)
        .expect("activate Vercel Claude pane");

    let model = app.native_spawn_default_model();
    assert_eq!(model, VERCEL_GLM_5_2_FAST_MODEL);
    assert_eq!(
        ChatWidget::model_provider_for_selection(&model).as_deref(),
        Some(VERCEL_ANTHROPIC_FAST_PROVIDER_ID)
    );
    Ok(())
}

#[tokio::test]
async fn idle_claude_pane_selection_clears_previous_live_status_panel() -> Result<()> {
    let mut app = make_test_app().await;
    let burzum_pane_id = app
        .claude_panes
        .create_pane_without_vault_for_test(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
        )
        .expect("create running Troll pane");
    let nazgul_pane_id = app
        .claude_panes
        .create_pane_without_vault_for_test(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
        )
        .expect("create idle Nazgul pane");

    app.claude_panes
        .set_active_user_pane(&burzum_pane_id)
        .expect("activate running pane");
    let _prepared = app
        .claude_panes
        .prepare_turn(
            &burzum_pane_id,
            "long running task".to_string(),
            app.config.codex_home.as_ref(),
        )
        .expect("prepare running pane turn");
    app.sync_external_pane_turn_display(&burzum_pane_id);

    assert!(app.chat_widget.is_task_running_for_test());
    assert!(app.chat_widget.status_indicator_visible_for_test());
    assert!(app.claude_panes.claude_pane_is_running(&burzum_pane_id));

    app.claude_panes
        .set_active_user_pane(&nazgul_pane_id)
        .expect("activate idle pane");
    app.sync_external_pane_turn_display(&nazgul_pane_id);

    assert!(!app.chat_widget.is_task_running_for_test());
    assert!(!app.chat_widget.status_indicator_visible_for_test());
    assert!(app.claude_panes.claude_pane_is_running(&burzum_pane_id));
    Ok(())
}

#[tokio::test]
async fn native_spawn_auth_guard_blocks_unauthenticated_openai() {
    let app = make_test_app().await;
    install_runtime_provider_policy(
        &app,
        crate::provider_status_host::ProviderAccountMetadata::default(),
    );

    let error = app
        .native_spawn_provider_auth_error(Some(OPENAI_PROVIDER_ID))
        .expect("OpenAI without auth should be rejected");

    assert!(error.contains("inactive or unavailable"));
}

#[tokio::test]
async fn native_spawn_auth_guard_accepts_openai_api_key_auth() {
    let mut app = make_test_app().await;
    app.chat_widget.update_account_state(
        Some(StatusAccountDisplay::ApiKey { env_var: None }),
        /*plan_type*/ None,
        /*has_chatgpt_account*/ false,
        /*has_codex_backend_auth*/ false,
    );
    install_runtime_provider_policy(
        &app,
        crate::provider_status_host::ProviderAccountMetadata {
            openai: codex_login::OpenAiAuthMetadata::ApiKey,
            ..Default::default()
        },
    );

    assert_eq!(
        app.native_spawn_provider_auth_error(Some(OPENAI_PROVIDER_ID)),
        None
    );
}

#[tokio::test]
async fn spawn_provider_allowlist_is_operator_policy_not_model_choice() {
    let mut app = make_test_app().await;
    app.config.agent_provider_allowlist = Some(vec![
        CLAUDE_PLAN_PROVIDER_ID.to_string(),
        OPENAI_PROVIDER_ID.to_string(),
    ]);

    // Authorized providers pass regardless of which path asked for them.
    for provider in [CLAUDE_PLAN_PROVIDER_ID, OPENAI_PROVIDER_ID] {
        app.ensure_native_spawn_provider_authorized(Some(provider))
            .unwrap_or_else(|err| panic!("{provider} should be authorized: {err}"));
    }

    // Every unauthorized provider is refused, not just the one that caused the
    // original incident. This is the class: a model selects a runtime, it never
    // authorizes one.
    for provider in [
        ANTHROPIC_PROVIDER_ID,
        OPENROUTER_PROVIDER_ID,
        KIMI_CODE_PROVIDER_ID,
        VERCEL_PROVIDER_ID,
    ] {
        let error = app
            .ensure_native_spawn_provider_authorized(Some(provider))
            .expect_err("unauthorized provider must be refused")
            .to_string();
        assert!(
            error.contains("agents.provider_allowlist"),
            "{provider} rejection should name the operator setting, got: {error}"
        );
    }
}

#[tokio::test]
async fn spawn_provider_allowlist_unset_stays_unrestricted() {
    let app = make_test_app().await;
    assert!(app.config.agent_provider_allowlist.is_none());

    for provider in [
        ANTHROPIC_PROVIDER_ID,
        OPENROUTER_PROVIDER_ID,
        KIMI_CODE_PROVIDER_ID,
    ] {
        app.ensure_native_spawn_provider_authorized(Some(provider))
            .unwrap_or_else(|err| panic!("{provider} should be unrestricted: {err}"));
    }

    // The declared ceiling for a new custom crew comes from configured providers,
    // never from whichever runtime a model happened to request first.
    let authorized = app.authorized_spawn_providers();
    assert!(
        authorized.len() > 1,
        "expected configured providers, got {authorized:?}"
    );
}

#[tokio::test]
async fn native_spawn_auth_guard_accepts_provider_key_storage() -> Result<()> {
    let app = make_test_app().await;
    install_runtime_provider_policy(
        &app,
        crate::provider_status_host::ProviderAccountMetadata::default(),
    );

    let missing = app
        .native_spawn_provider_auth_error(Some(VERCEL_PROVIDER_ID))
        .expect("Vercel without key should be rejected");
    assert!(missing.contains("inactive or unavailable"));

    std::fs::write(
        app.config.codex_home.join("provider_auth.json"),
        format!(r#"{{"api_keys":{{"{VERCEL_API_KEY_ENV_VAR}":"test-key"}}}}"#),
    )?;
    install_runtime_provider_policy(
        &app,
        crate::provider_status_host::ProviderAccountMetadata::default(),
    );

    assert!(
        app.native_spawn_provider_auth_error(Some(VERCEL_PROVIDER_ID))
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn native_spawn_auth_guard_rejects_unavailable_external_bearer_command() {
    let mut app = make_test_app().await;
    let mut provider = app
        .config
        .model_providers
        .get(CLAUDE_PLAN_PROVIDER_ID)
        .expect("Claude Plan provider should be configured")
        .clone();
    provider
        .auth
        .as_mut()
        .expect("Claude Plan provider should use external bearer auth")
        .command = app
        .config
        .codex_home
        .join("missing-provider-auth-command")
        .display()
        .to_string();
    app.config
        .model_providers
        .insert(CLAUDE_PLAN_PROVIDER_ID.to_string(), provider);

    let error = app
        .ensure_native_spawn_provider_ready(Some(CLAUDE_PLAN_PROVIDER_ID))
        .await
        .expect_err("unavailable external bearer auth should be rejected before native spawn");
    let message = error.to_string();

    assert!(message.contains("Claude Plan"));
    assert!(message.contains("provider authentication is unavailable"));
    assert!(message.contains("failed to start"));
}

#[tokio::test]
async fn native_spawn_auth_guard_uses_selected_provider_after_onboarding() -> Result<()> {
    let mut app = make_test_app().await;
    let provider = app
        .config
        .model_providers
        .get(OPENROUTER_PROVIDER_ID)
        .expect("OpenRouter provider should be configured")
        .clone();
    app.config.model_provider_id = OPENROUTER_PROVIDER_ID.to_string();
    app.config.model_provider = provider;
    std::fs::write(
        app.config.codex_home.join("provider_auth.json"),
        format!(r#"{{"api_keys":{{"{OPENROUTER_API_KEY_ENV_VAR}":"test-key"}}}}"#),
    )?;
    install_runtime_provider_policy(
        &app,
        crate::provider_status_host::ProviderAccountMetadata::default(),
    );

    assert!(
        app.native_spawn_provider_auth_error(/*provider_id*/ None)
            .is_none()
    );
    Ok(())
}

#[test]
fn guarded_model_selection_rejects_inactive_provider() -> Result<()> {
    std::thread::Builder::new()
        .name("guarded-model-selection".to_string())
        .stack_size(32 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async {
                    let mut app = Box::pin(make_test_app()).await;
                    std::fs::write(
                        app.config.codex_home.join("provider_auth.json"),
                        format!(r#"{{"api_keys":{{"{VERCEL_API_KEY_ENV_VAR}":"test-key"}}}}"#),
                    )?;
                    let host = crate::provider_status_host::ProviderStatusHost::from_config(
                        &app.config,
                        crate::provider_status_host::ProviderAccountMetadata::default(),
                    );
                    host.persist_policy(
                        VERCEL_PROVIDER_ID,
                        codex_provider_auth::ProviderActivationPolicy::Inactive,
                    )?;
                    app.model_catalog.set_provider_policy(
                        crate::chatwidget::provider_model_policy::ProviderModelPolicy::new(
                            host,
                            codex_provider_auth::ProviderRuntimeAuthorizations::default(),
                        ),
                    );
                    let original_provider = app.config.model_provider_id.clone();
                    let original_model = app.chat_widget.current_model().to_string();
                    let mut app_server =
                        Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
                    let mut tui = crate::tui::test_support::make_test_tui()?;

                    let control = Box::pin(app.handle_event(
                        &mut tui,
                        &mut app_server,
                        AppEvent::UpdateModelSelection {
                            model: VERCEL_GLM_5_2_FAST_MODEL.to_string(),
                            provider: Some(VERCEL_PROVIDER_ID.to_string()),
                        },
                    ))
                    .await?;

                    assert!(matches!(control, AppRunControl::Continue));
                    assert_eq!(app.config.model_provider_id, original_provider);
                    assert_eq!(app.chat_widget.current_model(), original_model);
                    app_server.shutdown().await?;
                    Ok(())
                })
        })?
        .join()
        .map_err(|_| color_eyre::eyre::eyre!("guarded model selection thread panicked"))?
}

fn install_runtime_provider_policy(
    app: &App,
    account: crate::provider_status_host::ProviderAccountMetadata,
) {
    let host = crate::provider_status_host::ProviderStatusHost::from_config(&app.config, account);
    app.model_catalog.set_provider_policy(
        crate::chatwidget::provider_model_policy::ProviderModelPolicy::new(
            host,
            codex_provider_auth::ProviderRuntimeAuthorizations::default(),
        ),
    );
}

#[tokio::test]
async fn stored_corbanu_key_can_refresh_the_current_runtime_policy_in_process() {
    let app = make_test_app().await;
    install_runtime_provider_policy(
        &app,
        crate::provider_status_host::ProviderAccountMetadata::default(),
    );
    let policy = app
        .model_catalog
        .provider_policy()
        .expect("test app should install a provider policy");
    let host = policy.host();
    codex_login::login_with_provider_api_key(
        &app.config.codex_home,
        codex_model_provider_info::PFTERMINAL_PLAN_API_KEY_ENV_VAR,
        "corbanu-refresh-test-key",
        app.config.cli_auth_credentials_store_mode,
        app.config.auth_keyring_backend_kind(),
    )
    .expect("store Corbanu API key");
    assert!(host.activate(codex_model_provider_info::CORBANU_PLAN_PROVIDER_ID));
    app.model_catalog.refresh_provider_policy();

    assert!(app.model_catalog.provider_is_selectable(
        codex_model_provider_info::PFTERMINAL_PLAN_PROVIDER_ID,
        "corbanu/glm-5.3-flash",
    ));

    assert!(
        codex_login::delete_provider_api_key(
            &app.config.codex_home,
            codex_model_provider_info::PFTERMINAL_PLAN_API_KEY_ENV_VAR,
        )
        .expect("delete Corbanu API key")
    );
    app.model_catalog.refresh_provider_policy();
    assert!(!app.model_catalog.provider_is_selectable(
        codex_model_provider_info::PFTERMINAL_PLAN_PROVIDER_ID,
        "corbanu/glm-5.3-flash",
    ));
}

#[tokio::test]
async fn open_agent_picker_keeps_missing_threads_for_replay() -> Result<()> {
    let mut app = Box::pin(make_test_app()).await;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(
        app.chat_widget.config_ref(),
    ))
    .await
    .expect("embedded app server");
    let thread_id = ThreadId::new();
    app.thread_event_channels
        .insert(thread_id, ThreadEventChannel::new(/*capacity*/ 1));

    Box::pin(app.open_agent_picker(&mut app_server)).await;

    assert_eq!(app.thread_event_channels.contains_key(&thread_id), true);
    assert_eq!(
        app.agent_navigation.get(&thread_id),
        Some(&AgentPickerThreadEntry {
            agent_nickname: None,
            agent_role: None,
            agent_path: None,
            model: None,
            last_task_message: None,
            last_result_message: None,
            is_running: false,
            is_closed: true,
        })
    );
    assert_eq!(app.agent_navigation.ordered_thread_ids(), vec![thread_id]);
    Ok(())
}

#[tokio::test]
async fn open_agent_picker_preserves_cached_metadata_for_replay_threads() -> Result<()> {
    let mut app = Box::pin(make_test_app()).await;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(
        app.chat_widget.config_ref(),
    ))
    .await
    .expect("embedded app server");
    let thread_id = ThreadId::new();
    app.thread_event_channels
        .insert(thread_id, ThreadEventChannel::new(/*capacity*/ 1));
    app.agent_navigation.upsert(
        thread_id,
        Some("Robie".to_string()),
        Some("explorer".to_string()),
        /*is_closed*/ true,
    );

    Box::pin(app.open_agent_picker(&mut app_server)).await;

    assert_eq!(app.thread_event_channels.contains_key(&thread_id), true);
    assert_eq!(
        app.agent_navigation.get(&thread_id),
        Some(&AgentPickerThreadEntry {
            agent_nickname: Some("Robie".to_string()),
            agent_role: Some("explorer".to_string()),
            agent_path: None,
            model: None,
            last_task_message: None,
            last_result_message: None,
            is_running: false,
            is_closed: true,
        })
    );
    Ok(())
}

#[tokio::test]
async fn open_agent_picker_preserves_running_hints_until_observed_completion() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(
        app.chat_widget.config_ref(),
    ))
    .await
    .expect("embedded app server");
    let thread_id = ThreadId::new();
    app.thread_event_channels
        .insert(thread_id, ThreadEventChannel::new(/*capacity*/ 4));
    app.agent_navigation
        .record_sub_agent_activity(SubAgentActivityDisplay {
            thread_id,
            agent_path: "/root/child".to_string(),
            agent_nickname: None,
            agent_role: None,
            task_preview: None,
            is_running_hint: true,
        });

    Box::pin(app.open_agent_picker(&mut app_server)).await;

    let mut expected_entry = AgentPickerThreadEntry {
        agent_nickname: None,
        agent_role: None,
        agent_path: Some("/root/child".to_string()),
        model: None,
        last_task_message: None,
        last_result_message: None,
        is_running: true,
        is_closed: false,
    };
    assert_eq!(app.agent_navigation.get(&thread_id), Some(&expected_entry));
    let status = loop {
        let event = app_event_rx.try_recv().expect("agent status history cell");
        if let AppEvent::InsertHistoryCell(cell) = event {
            let rendered = lines_to_single_string(&cell.display_lines(/*width*/ 80));
            if rendered.contains("/agent") {
                break rendered;
            }
        }
    };
    assert_snapshot!(status, @r###"
    /agent
    Sub-agents running

      • `/root/child`
        No recent activity yet.
    "###);

    app.enqueue_thread_notification(
        thread_id,
        turn_completed_notification(thread_id, "turn-older", TurnStatus::Completed),
    )
    .await?;

    Box::pin(app.open_agent_picker(&mut app_server)).await;

    assert_eq!(app.agent_navigation.get(&thread_id), Some(&expected_entry));
    app.enqueue_thread_notification(thread_id, turn_started_notification(thread_id, "turn-1"))
        .await?;
    app.enqueue_thread_notification(
        thread_id,
        turn_completed_notification(thread_id, "turn-1", TurnStatus::Completed),
    )
    .await?;

    Box::pin(app.open_agent_picker(&mut app_server)).await;

    expected_entry.is_running = false;
    assert_eq!(app.agent_navigation.get(&thread_id), Some(&expected_entry));
    app.agent_navigation
        .record_sub_agent_activity(SubAgentActivityDisplay {
            thread_id,
            agent_path: "/root/child".to_string(),
            agent_nickname: None,
            agent_role: None,
            task_preview: None,
            is_running_hint: true,
        });

    Box::pin(app.open_agent_picker(&mut app_server)).await;

    expected_entry.is_running = false;
    assert_eq!(app.agent_navigation.get(&thread_id), Some(&expected_entry));
    app.enqueue_thread_notification(thread_id, turn_started_notification(thread_id, "turn-2"))
        .await?;

    Box::pin(app.open_agent_picker(&mut app_server)).await;

    expected_entry.is_running = true;
    assert_eq!(app.agent_navigation.get(&thread_id), Some(&expected_entry));
    Ok(())
}

#[tokio::test]
async fn open_agent_picker_clears_running_hint_from_completed_snapshot() -> Result<()> {
    let mut app = Box::pin(make_test_app()).await;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(
        app.chat_widget.config_ref(),
    ))
    .await
    .expect("embedded app server");
    let thread_id = ThreadId::new();
    app.thread_event_channels.insert(
        thread_id,
        ThreadEventChannel::new_with_session(
            THREAD_EVENT_CHANNEL_CAPACITY,
            test_thread_session(thread_id, test_path_buf("/tmp/project")),
            vec![test_turn("turn-1", TurnStatus::Completed, Vec::new())],
        ),
    );
    app.agent_navigation
        .record_sub_agent_activity(SubAgentActivityDisplay {
            thread_id,
            agent_path: "/root/child".to_string(),
            agent_nickname: None,
            agent_role: None,
            task_preview: None,
            is_running_hint: true,
        });
    assert!(!app.agent_navigation.is_parent_owned(thread_id));

    Box::pin(app.open_agent_picker(&mut app_server)).await;

    assert_eq!(
        app.agent_navigation.get(&thread_id),
        Some(&AgentPickerThreadEntry {
            agent_nickname: None,
            agent_role: None,
            agent_path: Some("/root/child".to_string()),
            model: None,
            last_task_message: None,
            last_result_message: None,
            is_running: false,
            is_closed: false,
        })
    );
    Ok(())
}

#[tokio::test]
async fn open_agent_picker_selects_path_backed_agent() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = Box::pin(make_test_app_with_channels()).await;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(
        app.chat_widget.config_ref(),
    ))
    .await
    .expect("embedded app server");
    let thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000123").expect("valid thread id");
    app.thread_event_channels
        .insert(thread_id, ThreadEventChannel::new(/*capacity*/ 1));
    app.agent_navigation
        .record_sub_agent_activity(SubAgentActivityDisplay {
            thread_id,
            agent_path: "/root/worker".to_string(),
            agent_nickname: None,
            agent_role: None,
            task_preview: None,
            is_running_hint: true,
        });

    Box::pin(app.open_agent_picker(&mut app_server)).await;

    assert_app_snapshot!(
        "path_backed_agent_picker",
        render_bottom_popup(&app.chat_widget, /*width*/ 80)
    );
    while app_event_rx.try_recv().is_ok() {}
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_matches!(
        app_event_rx.try_recv(),
        Ok(AppEvent::SelectAgentThread(selected_thread_id)) if selected_thread_id == thread_id
    );
    Ok(())
}

#[tokio::test]
async fn open_agent_picker_refreshes_replay_only_path_backed_liveness() -> Result<()> {
    let mut app = Box::pin(make_test_app()).await;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(
        app.chat_widget.config_ref(),
    ))
    .await
    .expect("embedded app server");
    let thread_id = ThreadId::new();
    let mut channel = ThreadEventChannel::new(/*capacity*/ 4);
    channel.mark_replay_only();
    {
        let mut store = channel.store.lock().await;
        store.push_notification(turn_started_notification(thread_id, "turn-1"));
    }
    app.thread_event_channels.insert(thread_id, channel);
    app.agent_navigation
        .record_sub_agent_activity(SubAgentActivityDisplay {
            thread_id,
            agent_path: "/root/child".to_string(),
            agent_nickname: None,
            agent_role: None,
            task_preview: None,
            is_running_hint: true,
        });

    Box::pin(app.open_agent_picker(&mut app_server)).await;

    assert_eq!(
        app.agent_navigation.get(&thread_id),
        Some(&AgentPickerThreadEntry {
            agent_nickname: None,
            agent_role: None,
            agent_path: Some("/root/child".to_string()),
            model: None,
            last_task_message: None,
            last_result_message: None,
            is_running: false,
            is_closed: true,
        })
    );
    Ok(())
}

#[tokio::test]
async fn open_agent_picker_prunes_terminal_metadata_only_threads() -> Result<()> {
    let mut app = Box::pin(make_test_app()).await;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(
        app.chat_widget.config_ref(),
    ))
    .await
    .expect("embedded app server");
    let thread_id = ThreadId::new();
    app.agent_navigation.upsert(
        thread_id,
        Some("Ghost".to_string()),
        Some("worker".to_string()),
        /*is_closed*/ false,
    );

    Box::pin(app.open_agent_picker(&mut app_server)).await;

    assert_eq!(app.agent_navigation.get(&thread_id), None);
    assert!(app.agent_navigation.is_empty());
    Ok(())
}

#[tokio::test]
async fn open_agent_picker_marks_terminal_read_errors_closed() -> Result<()> {
    let mut app = Box::pin(make_test_app()).await;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(
        app.chat_widget.config_ref(),
    ))
    .await
    .expect("embedded app server");
    let thread_id = ThreadId::new();
    app.thread_event_channels
        .insert(thread_id, ThreadEventChannel::new(/*capacity*/ 1));
    app.agent_navigation.upsert(
        thread_id,
        Some("Robie".to_string()),
        Some("explorer".to_string()),
        /*is_closed*/ false,
    );

    Box::pin(app.open_agent_picker(&mut app_server)).await;

    assert_eq!(
        app.agent_navigation.get(&thread_id),
        Some(&AgentPickerThreadEntry {
            agent_nickname: Some("Robie".to_string()),
            agent_role: Some("explorer".to_string()),
            agent_path: None,
            model: None,
            last_task_message: None,
            last_result_message: None,
            is_running: false,
            is_closed: true,
        })
    );
    Ok(())
}

#[test]
fn open_agent_picker_marks_loaded_threads_open() -> Result<()> {
    const WORKER_THREADS: usize = 1;
    const TEST_STACK_SIZE_BYTES: usize = 8 * 1024 * 1024;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .thread_stack_size(TEST_STACK_SIZE_BYTES)
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let mut app = Box::pin(make_test_app()).await;
        let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(
            app.chat_widget.config_ref(),
        ))
        .await
        .expect("embedded app server");
        let started = app_server
            .start_thread(app.chat_widget.config_ref())
            .await?;
        let thread_id = started.session.thread_id;
        app.thread_event_channels
            .insert(thread_id, ThreadEventChannel::new(/*capacity*/ 1));

        Box::pin(app.open_agent_picker(&mut app_server)).await;

        assert_eq!(
            app.agent_navigation.get(&thread_id),
            Some(&AgentPickerThreadEntry {
                agent_nickname: None,
                agent_role: None,
                agent_path: None,
                model: None,
                last_task_message: None,
                last_result_message: None,
                is_running: false,
                is_closed: false,
            })
        );
        Ok(())
    })
}

#[test]
fn selected_and_resumed_threads_use_server_capability_for_v1_and_v2_children() -> Result<()> {
    const WORKER_THREADS: usize = 1;
    const TEST_STACK_SIZE_BYTES: usize = 8 * 1024 * 1024;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .thread_stack_size(TEST_STACK_SIZE_BYTES)
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref()).await?;
        let root = app_server
            .start_thread(app.chat_widget.config_ref())
            .await?;
        let root_thread_id = root.session.thread_id;
        app.enqueue_primary_thread_session(root.session, root.turns)
            .await?;

        let rollout_dir = app
            .config
            .codex_home
            .join("sessions")
            .join("2026")
            .join("01")
            .join("01");
        std::fs::create_dir_all(&rollout_dir)?;
        let mut child_thread_ids = Vec::new();
        for (index, multi_agent_version) in [MultiAgentVersion::V1, MultiAgentVersion::V2]
            .into_iter()
            .enumerate()
        {
            let child_thread_id = ThreadId::new();
            let timestamp = format!("2026-01-01T00:00:0{index}Z");
            let session_meta = SessionMeta {
                session_id: child_thread_id.into(),
                id: child_thread_id,
                parent_thread_id: Some(root_thread_id),
                timestamp: timestamp.clone(),
                cwd: app.config.cwd.to_path_buf(),
                originator: "codex-tui-test".to_string(),
                cli_version: "0.0.0".to_string(),
                source: RolloutSessionSource::SubAgent(SubAgentSource::ThreadSpawn {
                    parent_thread_id: root_thread_id,
                    depth: 1,
                    agent_path: None,
                    agent_nickname: Some(format!("child-{index}")),
                    agent_role: Some("worker".to_string()),
                    agent_class: None,
                }),
                model_provider: Some(app.config.model_provider_id.clone()),
                multi_agent_version: Some(multi_agent_version),
                ..SessionMeta::default()
            };
            let rollout_path = rollout_dir.join(format!(
                "rollout-2026-01-01T00-00-0{index}-{child_thread_id}.jsonl"
            ));
            let session_meta_line = serde_json::json!({
                "timestamp": timestamp,
                "type": "session_meta",
                "payload": serde_json::to_value(session_meta)?,
            });
            std::fs::write(rollout_path, format!("{session_meta_line}\n"))?;

            assert!(
                app.attach_live_thread_for_selection(&mut app_server, child_thread_id)
                    .await?
            );
            assert_eq!(
                app.agent_navigation.is_parent_owned(child_thread_id),
                multi_agent_version == MultiAgentVersion::V2
            );
            child_thread_ids.push(child_thread_id);
        }

        app.agent_navigation
            .record_sub_agent_activity(SubAgentActivityDisplay {
                thread_id: child_thread_ids[0],
                agent_path: "/root/child-0".to_string(),
                agent_nickname: None,
                agent_role: None,
                task_preview: None,
                is_running_hint: true,
            });
        app.thread_event_channels.remove(&child_thread_ids[1]);
        let backfill = app.backfill_loaded_subagent_threads(&mut app_server).await;
        assert!(backfill.completed);
        assert_eq!(
            backfill.refreshed_thread_ids,
            [child_thread_ids[1]].into_iter().collect()
        );
        assert_eq!(
            app.agent_navigation.get(&child_thread_ids[0]),
            Some(&AgentPickerThreadEntry {
                agent_nickname: Some("child-0".to_string()),
                agent_role: Some("worker".to_string()),
                agent_path: Some("/root/child-0".to_string()),
                model: None,
                last_task_message: None,
                last_result_message: None,
                is_running: true,
                is_closed: false,
            })
        );
        assert!(!app.agent_navigation.is_parent_owned(child_thread_ids[0]));
        assert!(app.agent_navigation.is_parent_owned(child_thread_ids[1]));

        let mut tui = crate::tui::test_support::make_test_tui()?;
        app.select_agent_thread(&mut tui, &mut app_server, child_thread_ids[0])
            .await?;
        while app_event_rx.try_recv().is_ok() {}
        app.chat_widget
            .restore_user_message_to_composer("v1 remains writable".into());
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            std::iter::from_fn(|| app_event_rx.try_recv().ok())
                .any(|event| matches!(event, AppEvent::CodexOp(Op::UserTurn { .. })))
        );

        app.select_agent_thread(&mut tui, &mut app_server, child_thread_ids[1])
            .await?;
        while app_event_rx.try_recv().is_ok() {}
        app.chat_widget
            .restore_user_message_to_composer("v2 stays view-only".into());
        let draft = app.chat_widget.composer_text_with_pending();
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(app.chat_widget.composer_text_with_pending(), draft);
        assert!(
            !std::iter::from_fn(|| app_event_rx.try_recv().ok())
                .any(|event| matches!(event, AppEvent::CodexOp(Op::UserTurn { .. })))
        );

        let resumed = app_server
            .resume_thread(
                app.config.clone(),
                child_thread_ids[1],
                app.resume_model_settings(),
                app.resume_permission_settings(),
            )
            .await?;
        assert!(resumed.blocks_direct_input);
        app.replace_chat_widget_with_app_server_thread(
            &mut tui,
            resumed,
            crate::app::session_lifecycle::ThreadAttachPresentation::SessionLineage,
            /*initial_user_message*/ None,
        )
        .await?;
        while app_event_rx.try_recv().is_ok() {}
        app.chat_widget
            .restore_user_message_to_composer("direct resume stays view-only".into());
        let draft = app.chat_widget.composer_text_with_pending();
        app.chat_widget
            .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(app.chat_widget.composer_text_with_pending(), draft);
        assert!(
            !std::iter::from_fn(|| app_event_rx.try_recv().ok())
                .any(|event| matches!(event, AppEvent::CodexOp(Op::UserTurn { .. })))
        );
        Ok(())
    })
}

#[test]
fn attach_live_thread_for_selection_accepts_empty_live_threads() -> Result<()> {
    const WORKER_THREADS: usize = 1;
    const TEST_STACK_SIZE_BYTES: usize = 8 * 1024 * 1024;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .thread_stack_size(TEST_STACK_SIZE_BYTES)
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let config = {
            let app = make_test_app().await;
            app.chat_widget.config_ref().clone()
        };
        let mut app_server = crate::start_embedded_app_server_for_picker(&config)
            .await
            .expect("embedded app server");
        let started = app_server.start_thread(&config).await?;
        let thread_id = started.session.thread_id;
        let mut app = make_test_app().await;
        app.agent_navigation.upsert(
            thread_id,
            Some("Scout".to_string()),
            Some("worker".to_string()),
            /*is_closed*/ false,
        );

        let live_attached = app
            .attach_live_thread_for_selection(&mut app_server, thread_id)
            .await?;

        assert!(
            live_attached,
            "fresh durable panes must retain their live listener"
        );
        assert!(app.thread_event_channels.contains_key(&thread_id));
        Ok(())
    })
}

#[test]
fn attach_live_thread_for_selection_rejects_unmaterialized_fallback_threads() -> Result<()> {
    const WORKER_THREADS: usize = 1;
    const TEST_STACK_SIZE_BYTES: usize = 8 * 1024 * 1024;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .thread_stack_size(TEST_STACK_SIZE_BYTES)
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let mut app = make_test_app().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref()).await?;
        let mut ephemeral_config = app.chat_widget.config_ref().clone();
        ephemeral_config.ephemeral = true;
        let started = app_server.start_thread(&ephemeral_config).await?;
        let thread_id = started.session.thread_id;
        app.agent_navigation.upsert(
            thread_id,
            Some("Scout".to_string()),
            Some("worker".to_string()),
            /*is_closed*/ false,
        );

        let err = app
            .attach_live_thread_for_selection(&mut app_server, thread_id)
            .await
            .expect_err("ephemeral fallback should not attach as a blank live thread");

        assert_eq!(
            err.to_string(),
            format!("Agent thread {thread_id} is not yet available for replay or live attach.")
        );
        assert!(!app.thread_event_channels.contains_key(&thread_id));
        Ok(())
    })
}

#[tokio::test]
async fn should_attach_live_thread_for_selection_skips_closed_metadata_only_threads() {
    let mut app = make_test_app().await;
    let thread_id = ThreadId::new();
    app.agent_navigation.upsert(
        thread_id,
        Some("Ghost".to_string()),
        Some("worker".to_string()),
        /*is_closed*/ true,
    );

    assert!(!app.should_attach_live_thread_for_selection(thread_id));

    app.agent_navigation.upsert(
        thread_id,
        Some("Ghost".to_string()),
        Some("worker".to_string()),
        /*is_closed*/ false,
    );
    assert!(app.should_attach_live_thread_for_selection(thread_id));

    app.thread_event_channels
        .insert(thread_id, ThreadEventChannel::new(/*capacity*/ 1));
    assert!(!app.should_attach_live_thread_for_selection(thread_id));
}

#[tokio::test]
async fn refresh_agent_picker_thread_liveness_prunes_closed_metadata_only_threads() -> Result<()> {
    let mut app = Box::pin(make_test_app()).await;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(
        app.chat_widget.config_ref(),
    ))
    .await
    .expect("embedded app server");
    let thread_id = ThreadId::new();
    app.agent_navigation.upsert(
        thread_id,
        Some("Ghost".to_string()),
        Some("worker".to_string()),
        /*is_closed*/ false,
    );

    let is_available =
        Box::pin(app.refresh_agent_picker_thread_liveness(&mut app_server, thread_id)).await;

    assert!(!is_available);
    assert_eq!(app.agent_navigation.get(&thread_id), None);
    assert!(!app.thread_event_channels.contains_key(&thread_id));
    Ok(())
}

#[tokio::test]
async fn handle_start_side_seeds_navigation_before_thread_started() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let config = app.chat_widget.config_ref().clone();
    let parent_thread_id = ThreadId::from_string(
        &app_test_support::create_fake_rollout(
            config.codex_home.as_path(),
            "2025-01-05T12-00-00",
            "2025-01-05T12:00:00Z",
            "Saved user message",
            Some(config.model_provider_id.as_str()),
            /*git_info*/ None,
        )
        .expect("create source rollout"),
    )?;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&config)).await?;
    let started = app_server
        .resume_thread(
            config,
            parent_thread_id,
            crate::app_server_session::ResumeModelSettings::RestoreFromThread,
            crate::app_server_session::ResumePermissionSettings::RestoreFromThread,
        )
        .await?;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    while app_event_rx.try_recv().is_ok() {}
    let mut tui = crate::tui::test_support::make_test_tui()?;

    let control = Box::pin(app.handle_start_side(
        &mut tui,
        &mut app_server,
        parent_thread_id,
        /*user_message*/ None,
    ))
    .await?;

    let side_thread_id = app
        .active_thread_id
        .expect("side conversation should become active");
    assert!(matches!(control, AppRunControl::Continue));
    assert_ne!(side_thread_id, parent_thread_id);
    assert!(app.side_threads.contains_key(&side_thread_id));
    assert!(app.thread_event_channels.contains_key(&side_thread_id));
    assert!(
        !app.agent_navigation
            .get(&side_thread_id)
            .expect("side start should seed navigation before thread/started")
            .is_closed
    );

    let mut saw_thread_started = false;
    for _ in 0..20 {
        let event = time::timeout(
            std::time::Duration::from_secs(/*secs*/ 2),
            app_server.next_event(),
        )
        .await
        .expect("app-server should emit an event")
        .expect("app-server event stream should remain open");
        if let codex_app_server_client::AppServerEvent::ServerNotification(notification) = event
            && let ServerNotification::ThreadStarted(notification) = notification.as_ref()
            && notification.thread.id == side_thread_id.to_string()
        {
            saw_thread_started = true;
            break;
        }
    }

    assert!(saw_thread_started);
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn select_uncached_agent_thread_still_refreshes_liveness() -> Result<()> {
    let mut app = Box::pin(make_test_app()).await;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(
        app.chat_widget.config_ref(),
    ))
    .await?;
    let thread_id = ThreadId::new();
    app.agent_navigation.upsert(
        thread_id,
        Some("Ghost".to_string()),
        Some("worker".to_string()),
        /*is_closed*/ false,
    );
    let mut tui = crate::tui::test_support::make_test_tui()?;

    Box::pin(app.select_agent_thread(&mut tui, &mut app_server, thread_id)).await?;

    assert_eq!(app.active_thread_id, None);
    assert_eq!(app.agent_navigation.get(&thread_id), None);
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn open_agent_picker_prompts_to_enable_multi_agent_when_disabled() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = Box::pin(make_test_app_with_channels()).await;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(
        app.chat_widget.config_ref(),
    ))
    .await
    .expect("embedded app server");
    let _ = app.config.features.disable(Feature::Collab);

    Box::pin(app.open_agent_picker(&mut app_server)).await;
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_matches!(
        app_event_rx.try_recv(),
        Ok(AppEvent::UpdateFeatureFlags { updates }) if updates == vec![(Feature::Collab, true)]
    );
    let cell = match app_event_rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected InsertHistoryCell event, got {other:?}"),
    };
    let rendered = cell
        .display_lines(/*width*/ 120)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Subagents will be enabled in the next session."));
    Ok(())
}

#[tokio::test]
async fn update_memory_settings_persists_and_updates_widget_config() -> Result<()> {
    let (mut app, _app_event_rx, _op_rx) = Box::pin(make_test_app_with_channels()).await;
    let codex_home = tempdir()?;
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;

    Box::pin(app.update_memory_settings_with_app_server(
        &mut app_server,
        /*use_memories*/ false,
        /*generate_memories*/ false,
    ))
    .await;

    assert!(!app.config.memories.use_memories);
    assert!(!app.config.memories.generate_memories);
    assert!(!app.chat_widget.config_ref().memories.use_memories);
    assert!(!app.chat_widget.config_ref().memories.generate_memories);

    let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
    let config_value = toml::from_str::<TomlValue>(&config)?;
    let memories = config_value
        .as_table()
        .and_then(|table| table.get("memories"))
        .and_then(TomlValue::as_table)
        .expect("memories table should exist");
    assert_eq!(
        memories.get("use_memories"),
        Some(&TomlValue::Boolean(false))
    );
    assert_eq!(
        memories.get("generate_memories"),
        Some(&TomlValue::Boolean(false))
    );
    assert!(
        !memories.contains_key("disable_on_external_context")
            && !memories.contains_key("no_memories_if_mcp_or_web_search"),
        "the TUI menu should not write the external-context memory setting"
    );
    app_server.shutdown().await?;
    Ok(())
}

#[test]
fn update_memory_settings_updates_current_thread_memory_mode() -> Result<()> {
    const WORKER_THREADS: usize = 1;
    const TEST_STACK_SIZE_BYTES: usize = 8 * 1024 * 1024;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .thread_stack_size(TEST_STACK_SIZE_BYTES)
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let (mut app, _app_event_rx, _op_rx) = Box::pin(make_test_app_with_channels()).await;
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        app.config.sqlite = codex_state::SqliteConfig::new_for_testing(codex_home.path().abs());
        // Seed the previous setting so this test exercises the thread-mode update path.
        app.config.memories.generate_memories = true;

        let mut app_server =
            Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
        let started = app_server.start_thread(&app.config).await?;
        let thread_id = started.session.thread_id;
        app.active_thread_id = Some(thread_id);

        Box::pin(app.update_memory_settings_with_app_server(
            &mut app_server,
            /*use_memories*/ true,
            /*generate_memories*/ false,
        ))
        .await;

        let state_db = codex_state::StateRuntime::init(
            codex_state::SqliteConfig::new_for_testing(codex_home.path().abs()),
            app.config.model_provider_id.clone(),
        )
        .await
        .expect("state db should initialize");
        let memory_mode = state_db
            .get_thread_memory_mode(thread_id)
            .await
            .expect("thread memory mode should be readable");
        assert_eq!(memory_mode.as_deref(), Some("disabled"));

        app_server.shutdown().await?;
        Ok(())
    })
}

#[tokio::test]
async fn reset_memories_clears_local_memory_directories() -> Result<()> {
    Box::pin(async {
        let (mut app, _app_event_rx, _op_rx) = Box::pin(make_test_app_with_channels()).await;
        let codex_home = tempdir()?;
        app.config.codex_home = codex_home.path().to_path_buf().abs();
        app.config.sqlite = codex_state::SqliteConfig::new_for_testing(codex_home.path().abs());

        let memory_root = codex_home.path().join("memories");
        let extensions_root = memory_root.join("extensions");
        std::fs::create_dir_all(memory_root.join("rollout_summaries"))?;
        std::fs::create_dir_all(&extensions_root)?;
        std::fs::write(memory_root.join("MEMORY.md"), "stale memory\n")?;
        std::fs::write(
            memory_root.join("rollout_summaries").join("stale.md"),
            "stale summary\n",
        )?;
        std::fs::write(extensions_root.join("stale.txt"), "stale extension\n")?;

        let mut app_server =
            Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;

        Box::pin(app.reset_memories_with_app_server(&mut app_server)).await;

        assert_eq!(std::fs::read_dir(&memory_root)?.count(), 0);

        app_server.shutdown().await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn apply_permission_profile_selection_preserves_loader_overrides() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let mut app_server = start_config_write_test_app_server(&app).await?;
    let codex_home = tempdir()?;
    let selected_config = codex_home.path().join("work.config.toml");
    std::fs::write(
        &selected_config,
        r#"
default_permissions = "locked-down"

[permissions.locked-down.filesystem]
":minimal" = "read"
"#,
    )?;
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    app.loader_overrides.user_config_path = Some(selected_config.abs());
    app.harness_overrides.sandbox_mode = Some(SandboxMode::WorkspaceWrite);
    app.harness_overrides.permission_profile = Some(PermissionProfile::workspace_write());

    assert!(
        app.apply_permission_profile_selection(
            &mut app_server,
            PermissionProfileSelection {
                profile_id: "locked-down".to_string(),
                approval_policy: None,
                approvals_reviewer: None,
                display_label: "locked-down".to_string(),
            }
        )
        .await
    );

    assert_eq!(
        app.config
            .permissions
            .active_permission_profile()
            .as_ref()
            .map(|profile| profile.id.as_str()),
        Some("locked-down")
    );
    assert_eq!(
        app.chat_widget
            .config_ref()
            .permissions
            .active_permission_profile()
            .as_ref()
            .map(|profile| profile.id.as_str()),
        Some("locked-down")
    );
    assert_eq!(
        app.runtime_permission_profile_override,
        Some(RuntimePermissionProfileOverride::from_config(&app.config))
    );
    let cell = match app_event_rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected InsertHistoryCell event, got {other:?}"),
    };
    let rendered = cell
        .display_lines(/*width*/ 120)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Permissions selected for the new session: locked-down"));
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn update_feature_flags_enabling_guardian_selects_auto_review() -> Result<()> {
    let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    let codex_home = tempdir()?;
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    let auto_review = auto_review_mode();
    let mut app_server = start_config_write_test_app_server(&app).await?;

    app.update_feature_flags(&mut app_server, vec![(Feature::GuardianApproval, true)])
        .await;

    assert!(app.config.features.enabled(Feature::GuardianApproval));
    assert!(
        app.chat_widget
            .config_ref()
            .features
            .enabled(Feature::GuardianApproval)
    );
    assert_eq!(
        app.config.approvals_reviewer,
        auto_review.approvals_reviewer
    );
    assert_eq!(
        AskForApproval::from(app.config.permissions.approval_policy.value()),
        auto_review.approval_policy
    );
    assert_eq!(
        AskForApproval::from(
            app.chat_widget
                .config_ref()
                .permissions
                .approval_policy
                .value(),
        ),
        auto_review.approval_policy
    );
    assert_eq!(
        app.chat_widget
            .config_ref()
            .permissions
            .permission_profile(),
        &auto_review.permission_profile()
    );
    assert_eq!(
        app.config.permissions.active_permission_profile(),
        Some(auto_review.active_permission_profile.clone())
    );
    assert_eq!(
        app.chat_widget
            .config_ref()
            .permissions
            .active_permission_profile(),
        Some(auto_review.active_permission_profile.clone())
    );
    assert_eq!(
        app.chat_widget.config_ref().approvals_reviewer,
        auto_review.approvals_reviewer
    );
    assert_eq!(app.runtime_approval_policy_override, None);
    assert_eq!(
        app.runtime_permission_profile_override,
        Some(RuntimePermissionProfileOverride::from_config(&app.config))
    );
    assert_eq!(
        op_rx.try_recv(),
        Ok(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: Some(auto_review.approval_policy),
            approvals_reviewer: Some(auto_review.approvals_reviewer),
            permission_profile: Some(auto_review.permission_profile()),
            active_permission_profile: Some(auto_review.active_permission_profile.clone()),
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
    );
    let cell = match app_event_rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected InsertHistoryCell event, got {other:?}"),
    };
    let rendered = cell
        .display_lines(/*width*/ 120)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Permissions updated to Approve for me"));

    let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
    assert!(config.contains("guardian_approval = true"));
    assert!(config.contains("approvals_reviewer = \"auto_review\""));
    assert!(config.contains("approval_policy = \"on-request\""));
    assert!(config.contains("sandbox_mode = \"workspace-write\""));
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn update_feature_flags_disabling_guardian_clears_review_policy_and_restores_default()
-> Result<()> {
    let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    let codex_home = tempdir()?;
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    let config_toml_path = codex_home.path().join("config.toml").abs();
    let config_toml = "approvals_reviewer = \"guardian_subagent\"\napproval_policy = \"on-request\"\nsandbox_mode = \"workspace-write\"\n\n[features]\nguardian_approval = true\n";
    std::fs::write(config_toml_path.as_path(), config_toml)?;
    let user_config = toml::from_str::<TomlValue>(config_toml)?;
    app.config.config_layer_stack = app
        .config
        .config_layer_stack
        .with_user_config(&config_toml_path, user_config)?;
    app.config
        .features
        .set_enabled(Feature::GuardianApproval, /*enabled*/ true)?;
    app.chat_widget
        .set_feature_enabled(Feature::GuardianApproval, /*enabled*/ true);
    app.config.approvals_reviewer = ApprovalsReviewer::AutoReview;
    app.chat_widget
        .set_approvals_reviewer(ApprovalsReviewer::AutoReview);
    app.config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())?;
    app.config
        .permissions
        .set_permission_profile(PermissionProfile::workspace_write())?;
    app.chat_widget
        .set_approval_policy(AskForApproval::OnRequest);
    app.chat_widget
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::legacy(
            PermissionProfile::workspace_write(),
        ))?;
    let mut app_server = start_config_write_test_app_server(&app).await?;

    app.update_feature_flags(&mut app_server, vec![(Feature::GuardianApproval, false)])
        .await;

    assert!(!app.config.features.enabled(Feature::GuardianApproval));
    assert!(
        !app.chat_widget
            .config_ref()
            .features
            .enabled(Feature::GuardianApproval)
    );
    assert_eq!(app.config.approvals_reviewer, ApprovalsReviewer::User);
    assert_eq!(
        AskForApproval::from(app.config.permissions.approval_policy.value()),
        AskForApproval::OnRequest
    );
    assert_eq!(
        app.chat_widget.config_ref().approvals_reviewer,
        ApprovalsReviewer::User
    );
    assert_eq!(app.runtime_approval_policy_override, None);
    assert_eq!(
        op_rx.try_recv(),
        Ok(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            approvals_reviewer: Some(ApprovalsReviewer::User),
            permission_profile: None,
            active_permission_profile: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
    );
    let cell = match app_event_rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected InsertHistoryCell event, got {other:?}"),
    };
    let rendered = cell
        .display_lines(/*width*/ 120)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("Permissions updated to Ask for approval"));

    let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
    assert!(!config.contains("guardian_approval = true"));
    assert!(!config.contains("approvals_reviewer ="));
    assert!(config.contains("approval_policy = \"on-request\""));
    assert!(config.contains("sandbox_mode = \"workspace-write\""));
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn update_feature_flags_enabling_guardian_overrides_explicit_manual_review_policy()
-> Result<()> {
    let (mut app, _app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    let codex_home = tempdir()?;
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    let auto_review = auto_review_mode();
    let config_toml_path = codex_home.path().join("config.toml").abs();
    let config_toml = "approvals_reviewer = \"user\"\n";
    std::fs::write(config_toml_path.as_path(), config_toml)?;
    let user_config = toml::from_str::<TomlValue>(config_toml)?;
    app.config.config_layer_stack = app
        .config
        .config_layer_stack
        .with_user_config(&config_toml_path, user_config)?;
    app.config.approvals_reviewer = ApprovalsReviewer::User;
    app.chat_widget
        .set_approvals_reviewer(ApprovalsReviewer::User);
    let mut app_server = start_config_write_test_app_server(&app).await?;

    app.update_feature_flags(&mut app_server, vec![(Feature::GuardianApproval, true)])
        .await;

    assert!(app.config.features.enabled(Feature::GuardianApproval));
    assert_eq!(
        app.config.approvals_reviewer,
        auto_review.approvals_reviewer
    );
    assert_eq!(
        app.chat_widget.config_ref().approvals_reviewer,
        auto_review.approvals_reviewer
    );
    assert_eq!(
        AskForApproval::from(app.config.permissions.approval_policy.value()),
        auto_review.approval_policy
    );
    assert_eq!(
        app.chat_widget
            .config_ref()
            .permissions
            .permission_profile(),
        &auto_review.permission_profile()
    );
    assert_eq!(
        op_rx.try_recv(),
        Ok(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: Some(auto_review.approval_policy),
            approvals_reviewer: Some(auto_review.approvals_reviewer),
            permission_profile: Some(auto_review.permission_profile()),
            active_permission_profile: Some(auto_review.active_permission_profile.clone()),
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
    );

    let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
    assert!(config.contains("approvals_reviewer = \"auto_review\""));
    assert!(config.contains("guardian_approval = true"));
    assert!(config.contains("approval_policy = \"on-request\""));
    assert!(config.contains("sandbox_mode = \"workspace-write\""));
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn update_feature_flags_disabling_guardian_clears_manual_review_policy_without_history()
-> Result<()> {
    let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    let codex_home = tempdir()?;
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    let config_toml_path = codex_home.path().join("config.toml").abs();
    let config_toml = "approvals_reviewer = \"user\"\napproval_policy = \"on-request\"\nsandbox_mode = \"workspace-write\"\n\n[features]\nguardian_approval = true\n";
    std::fs::write(config_toml_path.as_path(), config_toml)?;
    let user_config = toml::from_str::<TomlValue>(config_toml)?;
    app.config.config_layer_stack = app
        .config
        .config_layer_stack
        .with_user_config(&config_toml_path, user_config)?;
    app.config
        .features
        .set_enabled(Feature::GuardianApproval, /*enabled*/ true)?;
    app.chat_widget
        .set_feature_enabled(Feature::GuardianApproval, /*enabled*/ true);
    app.config.approvals_reviewer = ApprovalsReviewer::User;
    app.chat_widget
        .set_approvals_reviewer(ApprovalsReviewer::User);
    let mut app_server = start_config_write_test_app_server(&app).await?;

    app.update_feature_flags(&mut app_server, vec![(Feature::GuardianApproval, false)])
        .await;

    assert!(!app.config.features.enabled(Feature::GuardianApproval));
    assert_eq!(app.config.approvals_reviewer, ApprovalsReviewer::User);
    assert_eq!(
        app.chat_widget.config_ref().approvals_reviewer,
        ApprovalsReviewer::User
    );
    assert_eq!(
        op_rx.try_recv(),
        Ok(Op::OverrideTurnContext {
            cwd: None,
            approval_policy: None,
            approvals_reviewer: Some(ApprovalsReviewer::User),
            permission_profile: None,
            active_permission_profile: None,
            windows_sandbox_level: None,
            model: None,
            effort: None,
            summary: None,
            service_tier: None,
            collaboration_mode: None,
            personality: None,
        })
    );
    assert!(
        app_event_rx.try_recv().is_err(),
        "manual review should not emit a permissions history update when the effective state stays default"
    );

    let config = std::fs::read_to_string(codex_home.path().join("config.toml"))?;
    assert!(!config.contains("guardian_approval = true"));
    assert!(!config.contains("approvals_reviewer ="));
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn open_agent_picker_allows_existing_agent_threads_when_feature_is_disabled() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = Box::pin(make_test_app_with_channels()).await;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(
        app.chat_widget.config_ref(),
    ))
    .await
    .expect("embedded app server");
    let thread_id = ThreadId::new();
    app.thread_event_channels
        .insert(thread_id, ThreadEventChannel::new(/*capacity*/ 1));

    Box::pin(app.open_agent_picker(&mut app_server)).await;
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert_matches!(
        app_event_rx.try_recv(),
        Ok(AppEvent::SelectAgentThread(selected_thread_id)) if selected_thread_id == thread_id
    );
    Ok(())
}

#[tokio::test]
async fn refresh_pending_thread_approvals_only_lists_inactive_threads() {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000001").expect("valid thread");
    let agent_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000002").expect("valid thread");

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.thread_event_channels
        .insert(main_thread_id, ThreadEventChannel::new(/*capacity*/ 1));

    let agent_channel = ThreadEventChannel::new(/*capacity*/ 1);
    {
        let mut store = agent_channel.store.lock().await;
        store.push_request(exec_approval_request(
            agent_thread_id,
            "turn-1",
            "call-1",
            /*approval_id*/ None,
        ));
    }
    app.thread_event_channels
        .insert(agent_thread_id, agent_channel);
    app.agent_navigation.upsert(
        agent_thread_id,
        Some("Robie".to_string()),
        Some("explorer".to_string()),
        /*is_closed*/ false,
    );

    app.refresh_pending_thread_approvals().await;
    assert_eq!(
        app.chat_widget.pending_thread_approvals(),
        &["Robie [explorer]".to_string()]
    );

    app.active_thread_id = Some(agent_thread_id);
    app.refresh_pending_thread_approvals().await;
    assert!(app.chat_widget.pending_thread_approvals().is_empty());
}

#[tokio::test]
async fn inactive_thread_approval_bubbles_into_active_view() -> Result<()> {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000011").expect("valid thread");
    let agent_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000022").expect("valid thread");

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.thread_event_channels
        .insert(main_thread_id, ThreadEventChannel::new(/*capacity*/ 1));
    app.thread_event_channels.insert(
        agent_thread_id,
        ThreadEventChannel::new_with_session(
            /*capacity*/ 1,
            ThreadSessionState {
                approval_policy: AskForApproval::OnRequest,
                permission_profile: PermissionProfile::workspace_write(),
                rollout_path: Some(test_path_buf("/tmp/agent-rollout.jsonl")),
                ..test_thread_session(agent_thread_id, test_path_buf("/tmp/agent"))
            },
            Vec::new(),
        ),
    );
    app.agent_navigation.upsert(
        agent_thread_id,
        Some("Robie".to_string()),
        Some("explorer".to_string()),
        /*is_closed*/ false,
    );

    app.enqueue_thread_request(
        agent_thread_id,
        exec_approval_request(
            agent_thread_id,
            "turn-approval",
            "call-approval",
            /*approval_id*/ None,
        ),
    )
    .await?;

    assert_eq!(app.chat_widget.has_active_view(), true);
    assert_eq!(
        app.chat_widget.pending_thread_approvals(),
        &["Robie [explorer]".to_string()]
    );

    Ok(())
}

#[tokio::test]
async fn side_defers_parent_approval_overlay_until_parent_replay() -> Result<()> {
    let mut app = make_test_app().await;
    let parent_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000011").expect("valid thread");
    let side_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000022").expect("valid thread");

    app.primary_thread_id = Some(parent_thread_id);
    app.active_thread_id = Some(side_thread_id);
    app.side_threads
        .insert(side_thread_id, SideThreadState::new(parent_thread_id));
    app.thread_event_channels.insert(
        parent_thread_id,
        ThreadEventChannel::new_with_session(
            /*capacity*/ 4,
            test_thread_session(parent_thread_id, test_path_buf("/tmp/main")),
            Vec::new(),
        ),
    );

    app.enqueue_thread_request(
        parent_thread_id,
        exec_approval_request(
            parent_thread_id,
            "turn-approval",
            "call-approval",
            /*approval_id*/ None,
        ),
    )
    .await?;

    assert_eq!(app.chat_widget.has_active_view(), false);
    assert!(app.chat_widget.pending_thread_approvals().is_empty());
    assert_eq!(
        app.side_threads
            .get(&side_thread_id)
            .and_then(|state| state.parent_status),
        Some(SideParentStatus::NeedsApproval)
    );

    let snapshot = {
        let channel = app
            .thread_event_channels
            .get(&parent_thread_id)
            .expect("parent thread channel");
        let store = channel.store.lock().await;
        store.snapshot()
    };
    app.side_threads.remove(&side_thread_id);
    app.active_thread_id = Some(parent_thread_id);
    app.replay_thread_snapshot(snapshot, /*resume_restored_queue*/ false);

    assert_eq!(app.chat_widget.has_active_view(), true);

    Ok(())
}

#[tokio::test]
async fn replay_snapshot_with_pending_request_suppresses_replay_notices() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000011").expect("valid thread");
    let stale_warning = "stale startup warning that should not cover the approval";

    app.replay_thread_snapshot(
        ThreadEventSnapshot {
            session: Some(test_thread_session(thread_id, test_path_buf("/tmp/main"))),
            turns: Vec::new(),
            events: vec![
                ThreadBufferedEvent::Notification(Box::new(ServerNotification::Warning(
                    WarningNotification {
                        thread_id: Some(thread_id.to_string()),
                        message: stale_warning.to_string(),
                    },
                ))),
                ThreadBufferedEvent::Request(Box::new(exec_approval_request(
                    thread_id,
                    "turn-approval",
                    "call-approval",
                    /*approval_id*/ None,
                ))),
            ],
            input_state: None,
        },
        /*resume_restored_queue*/ false,
    );

    assert_eq!(app.chat_widget.has_active_view(), true);

    let mut replayed_history = String::new();
    while let Ok(event) = app_event_rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            replayed_history.push_str(&lines_to_single_string(
                &cell.transcript_lines(/*width*/ 80),
            ));
        }
    }

    assert!(
        replayed_history.is_empty(),
        "expected pending approval replay to suppress session notices, got {replayed_history:?}"
    );
}

#[tokio::test]
async fn side_defers_subagent_approval_overlay_until_side_exits() -> Result<()> {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000011").expect("valid thread");
    let side_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000022").expect("valid thread");
    let agent_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000033").expect("valid thread");
    let quiet_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000044").expect("valid thread");

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(side_thread_id);
    app.side_threads
        .insert(side_thread_id, SideThreadState::new(main_thread_id));
    app.thread_event_channels.insert(
        agent_thread_id,
        ThreadEventChannel::new_with_session(
            /*capacity*/ 4,
            ThreadSessionState {
                approval_policy: AskForApproval::OnRequest,
                permission_profile: PermissionProfile::workspace_write(),
                rollout_path: Some(test_path_buf("/tmp/agent-rollout.jsonl")),
                ..test_thread_session(agent_thread_id, test_path_buf("/tmp/agent"))
            },
            Vec::new(),
        ),
    );
    app.agent_navigation.upsert(
        agent_thread_id,
        Some("Robie".to_string()),
        Some("explorer".to_string()),
        /*is_closed*/ false,
    );

    let pending_approval = exec_approval_request(
        agent_thread_id,
        "turn-approval",
        "call-approval",
        /*approval_id*/ None,
    );
    app.enqueue_thread_request(agent_thread_id, pending_approval.clone())
        .await?;
    app.enqueue_thread_request(
        quiet_thread_id,
        ServerRequest::DynamicToolCall {
            request_id: AppServerRequestId::Integer(99),
            params: codex_app_server_protocol::DynamicToolCallParams {
                thread_id: quiet_thread_id.to_string(),
                turn_id: "turn-quiet".to_string(),
                call_id: "call-quiet".to_string(),
                namespace: None,
                tool: "ignored-tool".to_string(),
                arguments: serde_json::json!({}),
            },
        },
    )
    .await?;

    assert_eq!(app.chat_widget.has_active_view(), false);
    assert_eq!(
        app.chat_widget.pending_thread_approvals(),
        &["Robie [explorer]".to_string()]
    );

    app.side_threads.remove(&side_thread_id);
    app.active_thread_id = Some(main_thread_id);
    assert_eq!(
        app.pending_inactive_thread_requests().await,
        vec![(agent_thread_id, pending_approval)]
    );
    app.surface_pending_inactive_thread_interactive_requests()
        .await?;

    assert_eq!(app.chat_widget.has_active_view(), true);

    Ok(())
}

#[tokio::test]
async fn inactive_thread_exec_approval_preserves_context() {
    let app = make_test_app().await;
    let thread_id = ThreadId::new();
    let mut request = exec_approval_request(
        thread_id,
        "turn-approval",
        "call-approval",
        /*approval_id*/ None,
    );
    let ServerRequest::CommandExecutionRequestApproval { params, .. } = &mut request else {
        panic!("expected exec approval request");
    };
    params.network_approval_context = Some(AppServerNetworkApprovalContext {
        host: "example.com".to_string(),
        protocol: AppServerNetworkApprovalProtocol::Socks5Tcp,
    });
    params.additional_permissions = Some(AdditionalPermissionProfile {
        network: Some(AdditionalNetworkPermissions {
            enabled: Some(true),
        }),
        file_system: Some(AdditionalFileSystemPermissions {
            read: Some(vec![test_absolute_path("/tmp/read-only").into()]),
            write: Some(vec![test_absolute_path("/tmp/write").into()]),
            glob_scan_max_depth: None,
            entries: None,
        }),
    });
    params.proposed_network_policy_amendments = Some(vec![AppServerNetworkPolicyAmendment {
        host: "example.com".to_string(),
        action: AppServerNetworkPolicyRuleAction::Allow,
    }]);

    let Some(ThreadInteractiveRequest::Approval(ApprovalRequest::Exec(approval))) = app
        .interactive_request_for_thread_request(thread_id, &request)
        .await
        .expect("valid localized paths")
    else {
        panic!("expected exec approval request");
    };

    assert_eq!(
        approval.network_approval_context,
        Some(AppServerNetworkApprovalContext {
            host: "example.com".to_string(),
            protocol: AppServerNetworkApprovalProtocol::Socks5Tcp,
        })
    );
    assert_eq!(
        approval.additional_permissions,
        Some(AdditionalPermissionProfile {
            network: Some(AdditionalNetworkPermissions {
                enabled: Some(true),
            }),
            file_system: Some(AdditionalFileSystemPermissions {
                read: Some(vec![test_absolute_path("/tmp/read-only").into()]),
                write: Some(vec![test_absolute_path("/tmp/write").into()]),
                glob_scan_max_depth: None,
                entries: None,
            }),
        })
    );
    assert_eq!(
        approval.available_decisions,
        vec![
            codex_app_server_protocol::CommandExecutionApprovalDecision::Accept,
            codex_app_server_protocol::CommandExecutionApprovalDecision::AcceptForSession,
            codex_app_server_protocol::CommandExecutionApprovalDecision::ApplyNetworkPolicyAmendment {
                network_policy_amendment: AppServerNetworkPolicyAmendment {
                    host: "example.com".to_string(),
                    action: AppServerNetworkPolicyRuleAction::Allow,
                },
            },
            codex_app_server_protocol::CommandExecutionApprovalDecision::Cancel,
        ]
    );
}

#[tokio::test]
async fn inactive_thread_exec_approval_splits_shell_wrapped_command() {
    let app = make_test_app().await;
    let thread_id = ThreadId::new();
    let script = r#"python3 -c 'print("Hello, world!")'"#;
    let mut request = exec_approval_request(
        thread_id,
        "turn-approval",
        "call-approval",
        /*approval_id*/ None,
    );
    let ServerRequest::CommandExecutionRequestApproval { params, .. } = &mut request else {
        panic!("expected exec approval request");
    };
    params.command =
        Some(shlex::try_join(["/bin/zsh", "-lc", script]).expect("round-trippable shell wrapper"));

    let Some(ThreadInteractiveRequest::Approval(ApprovalRequest::Exec(approval))) = app
        .interactive_request_for_thread_request(thread_id, &request)
        .await
        .expect("valid localized paths")
    else {
        panic!("expected exec approval request");
    };

    assert_eq!(
        approval.command,
        vec![
            "/bin/zsh".to_string(),
            "-lc".to_string(),
            script.to_string(),
        ]
    );
}

#[tokio::test]
async fn inactive_thread_file_change_approval_recovers_buffered_changes() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    app.enqueue_thread_notification(
        thread_id,
        ServerNotification::ItemStarted(ItemStartedNotification {
            thread_id: thread_id.to_string(),
            turn_id: "turn-approval".to_string(),
            started_at_ms: 0,
            item: ThreadItem::FileChange {
                id: "patch-approval".to_string(),
                changes: vec![FileUpdateChange {
                    path: "README.md".to_string(),
                    kind: PatchChangeKind::Add,
                    diff: "hello\n".to_string(),
                }],
                status: codex_app_server_protocol::PatchApplyStatus::InProgress,
            },
        }),
    )
    .await
    .expect("enqueue file change item");

    let request = ServerRequest::FileChangeRequestApproval {
        request_id: AppServerRequestId::Integer(9),
        params: FileChangeRequestApprovalParams {
            thread_id: thread_id.to_string(),
            turn_id: "turn-approval".to_string(),
            item_id: "patch-approval".to_string(),
            started_at_ms: 0,
            reason: Some("command failed; retry without sandbox?".to_string()),
            grant_root: None,
        },
    };

    let request = app
        .interactive_request_for_thread_request(thread_id, &request)
        .await
        .expect("valid localized paths")
        .expect("expected file change approval request");

    let ThreadInteractiveRequest::Approval(ApprovalRequest::ApplyPatch(approval)) = &request else {
        panic!("expected apply-patch approval request");
    };
    assert_eq!(
        &approval.changes,
        &HashMap::from([(
            PathBuf::from("README.md"),
            FileChange::Add {
                content: "hello\n".to_string(),
            },
        )])
    );
    assert_eq!(
        &approval.reason,
        &Some("command failed; retry without sandbox?".to_string())
    );

    app.push_thread_interactive_request(request);
    let cell = match app_event_rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected patch preview history cell, saw {other:?}"),
    };
    let rendered = lines_to_single_string(&cell.display_lines(/*width*/ 80));
    assert!(rendered.contains("• Added README.md (+1 -0)"));
    assert!(rendered.contains("1 +hello"));
}

#[tokio::test]
async fn inactive_thread_permissions_approval_preserves_file_system_permissions() {
    let app = make_test_app().await;
    let thread_id = ThreadId::new();
    let request = ServerRequest::PermissionsRequestApproval {
        request_id: AppServerRequestId::Integer(7),
        params: PermissionsRequestApprovalParams {
            thread_id: thread_id.to_string(),
            turn_id: "turn-approval".to_string(),
            item_id: "call-approval".to_string(),
            environment_id: Some("remote".to_string()),
            started_at_ms: 0,
            cwd: test_absolute_path("/tmp"),
            reason: Some("Need access to .git".to_string()),
            permissions: codex_app_server_protocol::RequestPermissionProfile {
                network: Some(AdditionalNetworkPermissions {
                    enabled: Some(true),
                }),
                file_system: Some(AdditionalFileSystemPermissions {
                    read: Some(vec![test_absolute_path("/tmp/read-only").into()]),
                    write: Some(vec![test_absolute_path("/tmp/write").into()]),
                    glob_scan_max_depth: None,
                    entries: None,
                }),
            },
        },
    };

    let Some(ThreadInteractiveRequest::Approval(ApprovalRequest::Permissions(approval))) = app
        .interactive_request_for_thread_request(thread_id, &request)
        .await
        .expect("valid localized paths")
    else {
        panic!("expected permissions approval request");
    };

    assert_eq!(approval.environment_id.as_deref(), Some("remote"));
    assert_eq!(
        approval.permissions,
        RequestPermissionProfile {
            network: Some(NetworkPermissions {
                enabled: Some(true),
            }),
            file_system: Some(FileSystemPermissions::from_read_write_roots(
                Some(vec![test_absolute_path("/tmp/read-only")]),
                Some(vec![test_absolute_path("/tmp/write")]),
            )),
        }
    );
}

#[tokio::test]
async fn inactive_thread_url_elicitation_routes_to_app_link() {
    let app = make_test_app().await;
    let thread_id = ThreadId::new();
    let request = ServerRequest::McpServerElicitationRequest {
        request_id: AppServerRequestId::Integer(9),
        params: McpServerElicitationRequestParams {
            thread_id: thread_id.to_string(),
            turn_id: Some("turn-auth".to_string()),
            server_name: "payments".to_string(),
            request: McpServerElicitationRequest::Url {
                meta: None,
                message: "Review the payment details to continue.".to_string(),
                url: "https://payments.example/checkout/123".to_string(),
                elicitation_id: "payment-123".to_string(),
            },
        },
    };

    let Some(ThreadInteractiveRequest::AppLink(params)) = app
        .interactive_request_for_thread_request(thread_id, &request)
        .await
        .expect("valid localized paths")
    else {
        panic!("expected app link request");
    };

    assert_eq!(params.title, "Action required");
    assert_eq!(params.description, Some("Server: payments".to_string()));
    assert_eq!(params.url, "https://payments.example/checkout/123");
    assert_eq!(
        params.elicitation_target,
        Some(crate::bottom_pane::AppLinkElicitationTarget {
            thread_id,
            server_name: "payments".to_string(),
            request_id: AppServerRequestId::Integer(9),
        })
    );
}

#[tokio::test]
async fn inactive_thread_invalid_url_elicitation_is_declined() {
    let (app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    let request = ServerRequest::McpServerElicitationRequest {
        request_id: AppServerRequestId::Integer(10),
        params: McpServerElicitationRequestParams {
            thread_id: thread_id.to_string(),
            turn_id: Some("turn-auth".to_string()),
            server_name: "payments".to_string(),
            request: McpServerElicitationRequest::Url {
                meta: None,
                message: "Review the payment details to continue.".to_string(),
                url: "http://payments.example/checkout/123".to_string(),
                elicitation_id: "payment-123".to_string(),
            },
        },
    };

    assert!(
        app.interactive_request_for_thread_request(thread_id, &request)
            .await
            .expect("valid localized paths")
            .is_none()
    );
    assert_matches!(
        app_event_rx.try_recv(),
        Ok(AppEvent::SubmitThreadOp {
            thread_id: op_thread_id,
            op: Op::ResolveElicitation {
                server_name,
                request_id: AppServerRequestId::Integer(10),
                decision: codex_app_server_protocol::McpServerElicitationAction::Decline,
                content: None,
                meta: None,
            },
        }) if op_thread_id == thread_id && server_name == "payments"
    );
}

#[tokio::test]
async fn inactive_thread_approval_badge_clears_after_turn_completion_notification() -> Result<()> {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000101").expect("valid thread");
    let agent_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000202").expect("valid thread");

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.thread_event_channels
        .insert(main_thread_id, ThreadEventChannel::new(/*capacity*/ 1));
    app.thread_event_channels.insert(
        agent_thread_id,
        ThreadEventChannel::new_with_session(
            /*capacity*/ 4,
            ThreadSessionState {
                approval_policy: AskForApproval::OnRequest,
                permission_profile: PermissionProfile::workspace_write(),
                rollout_path: Some(test_path_buf("/tmp/agent-rollout.jsonl")),
                ..test_thread_session(agent_thread_id, test_path_buf("/tmp/agent"))
            },
            Vec::new(),
        ),
    );
    app.agent_navigation.upsert(
        agent_thread_id,
        Some("Robie".to_string()),
        Some("explorer".to_string()),
        /*is_closed*/ false,
    );

    app.enqueue_thread_request(
        agent_thread_id,
        exec_approval_request(
            agent_thread_id,
            "turn-approval",
            "call-approval",
            /*approval_id*/ None,
        ),
    )
    .await?;
    assert_eq!(
        app.chat_widget.pending_thread_approvals(),
        &["Robie [explorer]".to_string()]
    );

    app.enqueue_thread_notification(
        agent_thread_id,
        turn_completed_notification(agent_thread_id, "turn-approval", TurnStatus::Completed),
    )
    .await?;

    assert!(
        app.chat_widget.pending_thread_approvals().is_empty(),
        "turn completion should clear inactive-thread approval badge immediately"
    );

    Ok(())
}

/// #867 (AGG-R14 result 2): when the security kill switch stops a sub-agent the model spawned,
/// the thread on screen (its parent, waiting for it) names it, and `/agent` lists it as stopped
/// until its next turn starts. The parent's own stop and a sub-agent's other warnings add nothing.
#[tokio::test]
async fn issue_867_kill_switch_names_a_stopped_sub_agent() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000101").expect("valid thread");
    let agent_thread_id =
        ThreadId::from_string("01a14100-0000-0000-0000-000000000202").expect("valid thread");
    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.thread_event_channels
        .insert(main_thread_id, ThreadEventChannel::new(/*capacity*/ 4));
    app.thread_event_channels
        .insert(agent_thread_id, ThreadEventChannel::new(/*capacity*/ 4));
    app.upsert_agent_picker_thread(
        main_thread_id,
        /*agent_nickname*/ None,
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        agent_thread_id,
        Some("Robie".to_string()),
        Some("explorer".to_string()),
        /*is_closed*/ false,
    );
    let warning = |thread_id: ThreadId, message: &str| {
        ServerNotification::Warning(codex_app_server_protocol::WarningNotification {
            thread_id: Some(thread_id.to_string()),
            message: message.to_string(),
        })
    };
    let stopped = crate::legacy_core::security_revocation::KILL_SWITCH_STOPPED;
    let mut drain = || {
        let mut rendered = Vec::new();
        while let Ok(event) = app_event_rx.try_recv() {
            if let AppEvent::InsertHistoryCell(cell) = event {
                rendered.push(lines_to_single_string(&cell.display_lines(/*width*/ 100)));
            }
        }
        rendered
    };
    let agent_list = |app: &mut App| {
        app.chat_widget
            .show_selection_view(app.agent_picker_selection_view_params(/*selected*/ None));
        render_bottom_popup(&app.chat_widget, /*width*/ 100)
    };

    app.enqueue_thread_notification(agent_thread_id, warning(agent_thread_id, "other"))
        .await?;
    assert!(!app.agent_navigation.is_kill_switch_stopped(agent_thread_id));
    let _ = drain();

    app.enqueue_thread_notification(agent_thread_id, warning(agent_thread_id, stopped))
        .await?;
    assert!(app.agent_navigation.is_kill_switch_stopped(agent_thread_id));
    let notice = drain().join("\n");
    assert_snapshot!(
        "issue_867_kill_switch_names_a_stopped_sub_agent",
        format!("{notice}\n\n{}", agent_list(&mut app))
    );

    // The parent's own stop names nobody (its own notice says it).
    app.enqueue_thread_notification(main_thread_id, warning(main_thread_id, stopped))
        .await?;
    assert!(!app.agent_navigation.is_kill_switch_stopped(main_thread_id));
    assert!(
        drain().iter().all(|cell| !cell.contains("also stopped")),
        "the parent itself is not named"
    );

    // The sub-agent's next turn clears it.
    app.enqueue_thread_notification(
        agent_thread_id,
        turn_started_notification(agent_thread_id, "turn-next"),
    )
    .await?;
    assert!(!app.agent_navigation.is_kill_switch_stopped(agent_thread_id));
    assert!(
        !agent_list(&mut app).contains("kill switch"),
        "cleared by the next turn"
    );
    Ok(())
}

#[tokio::test]
async fn inactive_thread_started_notification_initializes_replay_session() -> Result<()> {
    let mut app = make_test_app().await;
    let temp_dir = tempdir()?;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000101").expect("valid thread");
    let agent_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000202").expect("valid thread");
    let primary_cwd = test_path_buf("/tmp/main").abs();
    let shared_root = test_path_buf("/tmp/shared").abs();
    let primary_session = ThreadSessionState {
        approval_policy: AskForApproval::OnRequest,
        permission_profile: PermissionProfile::workspace_write(),
        runtime_workspace_roots: vec![primary_cwd.clone(), shared_root.clone()],
        ..test_thread_session(main_thread_id, primary_cwd.to_path_buf())
    };

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.primary_session_configured = Some(primary_session.clone());
    app.thread_event_channels.insert(
        main_thread_id,
        ThreadEventChannel::new_with_session(
            /*capacity*/ 4,
            primary_session.clone(),
            Vec::new(),
        ),
    );

    let rollout_path = temp_dir.path().join("agent-rollout.jsonl");
    let rollout = serde_json::json!({
        "timestamp": "t0",
        "type": "turn_context",
        "payload": {
            "cwd": test_path_buf("/tmp/agent"),
            "model": "gpt-agent",
        },
    });
    std::fs::write(
        &rollout_path,
        format!("{}\n", serde_json::to_string(&rollout)?),
    )?;
    app.enqueue_thread_notification(
        agent_thread_id,
        ServerNotification::ThreadStarted(ThreadStartedNotification {
            thread: Thread {
                id: agent_thread_id.to_string(),
                extra: None,
                session_id: agent_thread_id.to_string(),
                forked_from_id: None,
                parent_thread_id: None,
                preview: "agent thread".to_string(),
                ephemeral: false,
                section: None,
                section_entered_at: None,
                history_mode: Default::default(),
                model_provider: "agent-provider".to_string(),
                created_at: 1,
                updated_at: 2,
                recency_at: Some(2),
                status: codex_app_server_protocol::ThreadStatus::Idle,
                path: Some(rollout_path.clone()),
                cwd: test_path_buf("/tmp/agent").abs(),
                cli_version: "0.0.0".to_string(),
                source: codex_app_server_protocol::SessionSource::Unknown,
                can_accept_direct_input: None,
                thread_source: None,
                agent_nickname: Some("Robie".to_string()),
                agent_role: Some("explorer".to_string()),
                git_info: None,
                name: Some("agent thread".to_string()),
                turns: Vec::new(),
            },
        }),
    )
    .await?;

    let store = app
        .thread_event_channels
        .get(&agent_thread_id)
        .expect("agent thread channel")
        .store
        .lock()
        .await;
    let session = store.session.clone().expect("inferred session");
    drop(store);

    assert_eq!(session.thread_id, agent_thread_id);
    assert_eq!(session.thread_name, Some("agent thread".to_string()));
    assert_eq!(session.model, "gpt-agent");
    assert_eq!(session.model_provider_id, "agent-provider");
    assert_eq!(session.approval_policy, primary_session.approval_policy);
    assert_eq!(session.cwd.as_path(), test_path_buf("/tmp/agent").as_path());
    assert_eq!(
        session.runtime_workspace_roots,
        vec![test_path_buf("/tmp/agent").abs(), shared_root]
    );
    assert_eq!(session.rollout_path, Some(rollout_path));
    assert_eq!(
        app.agent_navigation.get(&agent_thread_id),
        Some(&AgentPickerThreadEntry {
            agent_nickname: Some("Robie".to_string()),
            agent_role: Some("explorer".to_string()),
            agent_path: None,
            model: Some("gpt-agent".to_string()),
            last_task_message: None,
            last_result_message: None,
            is_running: false,
            is_closed: false,
        })
    );

    Ok(())
}

#[tokio::test]
async fn inactive_thread_started_notification_preserves_primary_model_when_path_missing()
-> Result<()> {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000301").expect("valid thread");
    let agent_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000302").expect("valid thread");
    let primary_cwd = test_path_buf("/tmp/main").abs();
    let primary_session = ThreadSessionState {
        approval_policy: AskForApproval::OnRequest,
        permission_profile: PermissionProfile::workspace_write(),
        runtime_workspace_roots: vec![primary_cwd.clone()],
        ..test_thread_session(main_thread_id, primary_cwd.to_path_buf())
    };

    app.primary_thread_id = Some(main_thread_id);
    app.active_thread_id = Some(main_thread_id);
    app.primary_session_configured = Some(primary_session.clone());
    app.thread_event_channels.insert(
        main_thread_id,
        ThreadEventChannel::new_with_session(
            /*capacity*/ 4,
            primary_session.clone(),
            Vec::new(),
        ),
    );

    app.enqueue_thread_notification(
        agent_thread_id,
        ServerNotification::ThreadStarted(ThreadStartedNotification {
            thread: Thread {
                id: agent_thread_id.to_string(),
                extra: None,
                session_id: agent_thread_id.to_string(),
                forked_from_id: None,
                parent_thread_id: None,
                preview: "agent thread".to_string(),
                ephemeral: false,
                section: None,
                section_entered_at: None,
                history_mode: Default::default(),
                model_provider: "agent-provider".to_string(),
                created_at: 1,
                updated_at: 2,
                recency_at: Some(2),
                status: codex_app_server_protocol::ThreadStatus::Idle,
                path: None,
                cwd: test_path_buf("/tmp/agent").abs(),
                cli_version: "0.0.0".to_string(),
                source: codex_app_server_protocol::SessionSource::Unknown,
                can_accept_direct_input: None,
                thread_source: None,
                agent_nickname: Some("Robie".to_string()),
                agent_role: Some("explorer".to_string()),
                git_info: None,
                name: Some("agent thread".to_string()),
                turns: Vec::new(),
            },
        }),
    )
    .await?;

    let store = app
        .thread_event_channels
        .get(&agent_thread_id)
        .expect("agent thread channel")
        .store
        .lock()
        .await;
    let session = store.session.clone().expect("inferred session");

    assert_eq!(session.model, primary_session.model);

    Ok(())
}

/// `thread/read` is metadata/replay hydration and does not return a fresh
/// server-authored `PermissionProfile`, so it must not reuse the cached primary
/// session profile after swapping in the read thread's cwd.
#[tokio::test]
async fn thread_read_session_state_does_not_reuse_primary_permission_profile() {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000401").expect("valid thread");
    let read_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000402").expect("valid thread");
    let primary_cwd = test_path_buf("/tmp/main").abs();
    let primary_session = ThreadSessionState {
        approval_policy: AskForApproval::OnRequest,
        permission_profile: PermissionProfile::workspace_write(),
        runtime_workspace_roots: vec![primary_cwd.clone()],
        ..test_thread_session(main_thread_id, primary_cwd.to_path_buf())
    };
    app.primary_session_configured = Some(primary_session);

    let thread = Thread {
        id: read_thread_id.to_string(),
        extra: None,
        session_id: read_thread_id.to_string(),
        forked_from_id: None,
        parent_thread_id: None,
        preview: "read thread".to_string(),
        ephemeral: false,
        section: None,
        section_entered_at: None,
        history_mode: Default::default(),
        model_provider: "read-provider".to_string(),
        created_at: 1,
        updated_at: 2,
        recency_at: Some(2),
        status: codex_app_server_protocol::ThreadStatus::Idle,
        path: None,
        cwd: test_path_buf("/tmp/read").abs(),
        cli_version: "0.0.0".to_string(),
        source: codex_app_server_protocol::SessionSource::Unknown,
        can_accept_direct_input: None,
        thread_source: None,
        agent_nickname: None,
        agent_role: None,
        git_info: None,
        name: Some("read thread".to_string()),
        turns: Vec::new(),
    };

    let session = app
        .session_state_for_thread_read(read_thread_id, &thread)
        .await;

    assert_eq!(session.thread_id, read_thread_id);
    assert_eq!(session.cwd.as_path(), test_path_buf("/tmp/read").as_path());
    assert_eq!(
        session.runtime_workspace_roots,
        vec![test_path_buf("/tmp/read").abs()]
    );
    let expected_permission_profile = app
        .chat_widget
        .config_ref()
        .permissions
        .permission_profile()
        .clone();
    assert_eq!(
        session.permission_profile, expected_permission_profile,
        "thread/read does not return fresh server permissions; the fallback profile must use the \
         active widget permissions rather than reusing the cached primary session profile"
    );
}

#[test]
fn agent_picker_item_name_snapshot() {
    let thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000123").expect("valid thread id");
    let snapshot = [
        format!(
            "{} | {}",
            format_agent_picker_item_name(
                Some("Robie"),
                Some("explorer"),
                /*is_primary*/ true
            ),
            thread_id
        ),
        format!(
            "{} | {}",
            format_agent_picker_item_name(
                Some("Robie"),
                Some("explorer"),
                /*is_primary*/ false
            ),
            thread_id
        ),
        format!(
            "{} | {}",
            format_agent_picker_item_name(
                Some("Robie"),
                /*agent_role*/ None,
                /*is_primary*/ false
            ),
            thread_id
        ),
        format!(
            "{} | {}",
            format_agent_picker_item_name(
                /*agent_nickname*/ None,
                Some("explorer"),
                /*is_primary*/ false
            ),
            thread_id
        ),
        format!(
            "{} | {}",
            format_agent_picker_item_name(
                /*agent_nickname*/ None, /*agent_role*/ None, /*is_primary*/ false
            ),
            thread_id
        ),
    ]
    .join("\n");
    assert_app_snapshot!("agent_picker_item_name", snapshot);
}

#[tokio::test]
async fn side_fork_config_is_ephemeral_and_appends_developer_guardrails() {
    let app = make_test_app().await;
    let original_approval_policy = app.config.permissions.approval_policy.value();
    let original_sandbox_policy = app.config.legacy_sandbox_policy();

    let fork_config = app.side_fork_config();

    assert!(fork_config.ephemeral);
    assert_eq!(
        fork_config.permissions.approval_policy.value(),
        original_approval_policy
    );
    assert_eq!(fork_config.legacy_sandbox_policy(), original_sandbox_policy);
    let developer_instructions = fork_config
        .developer_instructions
        .as_deref()
        .expect("side developer instructions");
    assert!(
        developer_instructions.contains("You are in a side conversation, not the main thread.")
    );
    assert!(
        developer_instructions
            .contains("inherited fork history is provided only as reference context")
    );
    assert!(
        developer_instructions.contains(
            "Only instructions submitted after the side-conversation boundary are active"
        )
    );
    assert!(developer_instructions.contains("Do not continue, execute, or complete any task"));
    assert!(
        developer_instructions
            .contains("External tools may be available according to this thread's current")
    );
    assert!(
        developer_instructions
            .contains("Any MCP or external tool calls or outputs visible in the inherited")
    );
    assert!(developer_instructions.contains("non-mutating inspection"));
    assert!(developer_instructions.contains("Do not modify files"));
    assert!(developer_instructions.contains("Do not request escalated permissions"));
    assert!(app.transcript_cells.is_empty());
}

#[tokio::test]
async fn side_fork_config_inherits_parent_thread_runtime_settings() {
    let mut app = make_test_app().await;
    app.config.model = Some("persisted-default-model".to_string());
    app.config.model_reasoning_effort = Some(ReasoningEffortConfig::Low);

    let parent_service_tier = ServiceTier::Fast.request_value();
    let parent_permission_profile = PermissionProfile::workspace_write();
    app.chat_widget.set_model("parent-thread-model");
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::High));
    app.chat_widget
        .set_service_tier(Some(parent_service_tier.to_string()));
    app.chat_widget
        .set_approval_policy(AskForApproval::OnRequest);
    app.chat_widget
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::legacy(
            parent_permission_profile.clone(),
        ))
        .expect("test permission profile should be accepted");
    app.chat_widget
        .set_approvals_reviewer(ApprovalsReviewer::AutoReview);

    let fork_config = app.side_fork_config();

    assert_eq!(
        (
            fork_config.model.as_deref(),
            fork_config.model_reasoning_effort,
            fork_config.service_tier.as_deref(),
            fork_config.permissions.approval_policy.value(),
            fork_config.permissions.permission_profile(),
            fork_config.approvals_reviewer,
        ),
        (
            Some("parent-thread-model"),
            Some(ReasoningEffortConfig::High),
            Some(parent_service_tier),
            AskForApproval::OnRequest.to_core(),
            &parent_permission_profile,
            ApprovalsReviewer::AutoReview,
        )
    );
}

#[tokio::test]
async fn side_start_block_message_allows_replacing_open_side_conversation() {
    let mut app = make_test_app().await;
    assert_eq!(
        app.side_start_block_message(),
        Some("'/side' is unavailable until the main thread is ready.")
    );

    app.primary_thread_id = Some(ThreadId::new());
    assert_eq!(app.side_start_block_message(), None);

    let parent_thread_id = ThreadId::new();
    let side_thread_id = ThreadId::new();
    app.side_threads
        .insert(side_thread_id, SideThreadState::new(parent_thread_id));

    app.active_thread_id = Some(parent_thread_id);
    assert_eq!(app.side_start_block_message(), None);

    app.active_thread_id = Some(side_thread_id);
    assert_eq!(
        app.side_start_block_message(),
        Some(
            "A side conversation is already open. Press ctrl + c to return before starting another."
        )
    );

    app.side_threads.remove(&side_thread_id);
    assert_eq!(app.side_start_block_message(), None);
}

#[tokio::test]
async fn side_parent_status_tracks_parent_turn_lifecycle() -> Result<()> {
    let mut app = make_test_app().await;
    let parent_thread_id = ThreadId::new();
    let side_thread_id = ThreadId::new();
    app.primary_thread_id = Some(parent_thread_id);
    app.active_thread_id = Some(side_thread_id);
    app.side_threads
        .insert(side_thread_id, SideThreadState::new(parent_thread_id));

    app.enqueue_thread_notification(
        parent_thread_id,
        turn_completed_notification(parent_thread_id, "turn-1", TurnStatus::Completed),
    )
    .await?;
    assert_eq!(
        app.side_threads
            .get(&side_thread_id)
            .and_then(|state| state.parent_status),
        Some(SideParentStatus::Finished)
    );

    app.enqueue_thread_notification(
        parent_thread_id,
        turn_started_notification(parent_thread_id, "turn-2"),
    )
    .await?;
    assert_eq!(
        app.side_threads
            .get(&side_thread_id)
            .and_then(|state| state.parent_status),
        None
    );

    app.enqueue_thread_notification(
        parent_thread_id,
        turn_completed_notification(parent_thread_id, "turn-2", TurnStatus::Failed),
    )
    .await?;
    assert_eq!(
        app.side_threads
            .get(&side_thread_id)
            .and_then(|state| state.parent_status),
        Some(SideParentStatus::Failed)
    );

    Ok(())
}

#[tokio::test]
async fn side_parent_status_prioritizes_input_over_approval() -> Result<()> {
    let mut app = make_test_app().await;
    let parent_thread_id = ThreadId::new();
    let side_thread_id = ThreadId::new();
    app.primary_thread_id = Some(parent_thread_id);
    app.active_thread_id = Some(side_thread_id);
    app.side_threads
        .insert(side_thread_id, SideThreadState::new(parent_thread_id));

    app.enqueue_thread_request(
        parent_thread_id,
        exec_approval_request(
            parent_thread_id,
            "turn-approval",
            "call-approval",
            /*approval_id*/ None,
        ),
    )
    .await?;
    assert_eq!(
        app.side_threads
            .get(&side_thread_id)
            .and_then(|state| state.parent_status),
        Some(SideParentStatus::NeedsApproval)
    );

    app.enqueue_thread_request(
        parent_thread_id,
        request_user_input_request(parent_thread_id, "turn-input", "call-input"),
    )
    .await?;
    assert_eq!(
        app.side_threads
            .get(&side_thread_id)
            .and_then(|state| state.parent_status),
        Some(SideParentStatus::NeedsInput)
    );

    app.enqueue_thread_notification(
        parent_thread_id,
        ServerNotification::ServerRequestResolved(
            codex_app_server_protocol::ServerRequestResolvedNotification {
                thread_id: parent_thread_id.to_string(),
                request_id: AppServerRequestId::Integer(2),
            },
        ),
    )
    .await?;
    assert_eq!(
        app.side_threads
            .get(&side_thread_id)
            .and_then(|state| state.parent_status),
        Some(SideParentStatus::NeedsApproval)
    );

    app.enqueue_thread_notification(
        parent_thread_id,
        ServerNotification::ServerRequestResolved(
            codex_app_server_protocol::ServerRequestResolvedNotification {
                thread_id: parent_thread_id.to_string(),
                request_id: AppServerRequestId::Integer(1),
            },
        ),
    )
    .await?;
    assert_eq!(
        app.side_threads
            .get(&side_thread_id)
            .and_then(|state| state.parent_status),
        None
    );

    Ok(())
}

#[tokio::test]
async fn side_thread_snapshot_hides_forked_parent_transcript() {
    let parent_thread_id = ThreadId::new();
    let side_thread_id = ThreadId::new();
    let mut store = ThreadEventStore::new(/*capacity*/ 4);
    let session = ThreadSessionState {
        forked_from_id: Some(parent_thread_id),
        fork_parent_title: None,
        ..test_thread_session(side_thread_id, test_path_buf("/tmp/side"))
    };
    let parent_turn = test_turn(
        "parent-turn",
        TurnStatus::Completed,
        vec![ThreadItem::UserMessage {
            id: "parent-user".to_string(),
            client_id: None,
            content: vec![AppServerUserInput::Text {
                text: "parent prompt should stay hidden".to_string(),
                text_elements: Vec::new(),
            }],
        }],
    );

    App::install_side_thread_snapshot(&mut store, session, vec![parent_turn]);

    let stored_session = store.session.as_ref().expect("side session");
    assert_eq!(stored_session.thread_id, side_thread_id);
    assert_eq!(stored_session.forked_from_id, None);
    assert_eq!(store.turns, Vec::<Turn>::new());
    assert_eq!(store.active_turn_id(), None);
}

#[tokio::test]
async fn side_thread_snapshot_does_not_refresh_from_fork_history() {
    let mut app = make_test_app().await;
    let parent_thread_id = ThreadId::new();
    let side_thread_id = ThreadId::new();
    app.side_threads
        .insert(side_thread_id, SideThreadState::new(parent_thread_id));

    let snapshot = ThreadEventSnapshot {
        session: Some(ThreadSessionState {
            rollout_path: None,
            ..test_thread_session(side_thread_id, test_path_buf("/tmp/side"))
        }),
        turns: Vec::new(),
        events: Vec::new(),
        input_state: None,
    };

    assert!(!app.should_refresh_snapshot_session(
        side_thread_id,
        /*is_replay_only*/ false,
        &snapshot
    ));
}

#[tokio::test]
async fn side_thread_snapshot_skips_session_header_preamble() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    while app_event_rx.try_recv().is_ok() {}

    let parent_thread_id = ThreadId::new();
    let side_thread_id = ThreadId::new();
    app.primary_thread_id = Some(parent_thread_id);
    app.side_threads
        .insert(side_thread_id, SideThreadState::new(parent_thread_id));

    let snapshot = ThreadEventSnapshot {
        session: Some(ThreadSessionState {
            forked_from_id: Some(parent_thread_id),
            fork_parent_title: None,
            ..test_thread_session(side_thread_id, test_path_buf("/tmp/side"))
        }),
        turns: Vec::new(),
        events: Vec::new(),
        input_state: None,
    };

    app.replay_thread_snapshot(snapshot, /*resume_restored_queue*/ false);

    let mut rendered_cells = Vec::new();
    while let Ok(event) = app_event_rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            rendered_cells.push(lines_to_single_string(&cell.display_lines(/*width*/ 120)));
        }
    }
    assert_eq!(app.chat_widget.thread_id(), Some(side_thread_id));
    assert_eq!(rendered_cells, Vec::<String>::new());
    assert_eq!(
        app.chat_widget.active_cell_transcript_lines(/*width*/ 120),
        None
    );
}

#[tokio::test]
async fn primary_thread_ignores_child_mcp_startup_notifications() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    while app_event_rx.try_recv().is_ok() {}
    let sentry_config = toml::from_str::<toml::Value>("command = 'true'")
        .expect("test MCP config should parse")
        .try_into()
        .expect("test MCP config should deserialize");
    app.config
        .mcp_servers
        .set(std::collections::HashMap::from([(
            "sentry".to_string(),
            sentry_config,
        )]))
        .expect("test MCP servers should accept any configuration");
    let app_server = crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
        .await
        .expect("embedded app server");
    let parent_thread_id = ThreadId::new();
    let child_thread_id = ThreadId::new();
    app.primary_thread_id = Some(parent_thread_id);
    app.active_thread_id = Some(parent_thread_id);

    app.handle_app_server_event(
        &app_server,
        codex_app_server_client::AppServerEvent::ServerNotification(Box::new(
            ServerNotification::McpServerStatusUpdated(McpServerStatusUpdatedNotification {
                thread_id: Some(child_thread_id.to_string()),
                name: "sentry".to_string(),
                status: McpServerStartupState::Failed,
                error: Some("sentry is not logged in".to_string()),
                failure_reason: None,
            }),
        )),
    )
    .await;

    assert!(app_event_rx.try_recv().is_err());
    let mut child_snapshot = app
        .thread_event_channels
        .get(&child_thread_id)
        .expect("child thread channel should be created")
        .store
        .lock()
        .await
        .snapshot();
    assert!(
        matches!(
            child_snapshot.events.as_slice(),
            [ThreadBufferedEvent::Notification(notification)]
                if matches!(
                    notification.as_ref(),
                    ServerNotification::McpServerStatusUpdated(_)
                )
        ),
        "child MCP startup notification should be buffered for the child thread"
    );

    app.apply_refreshed_snapshot_thread(
        child_thread_id,
        AppServerStartedThread {
            session: test_thread_session(child_thread_id, test_path_buf("/tmp/child")),
            turns: Vec::new(),
            blocks_direct_input: false,
        },
        &mut child_snapshot,
    )
    .await;
    app.replay_thread_snapshot(child_snapshot, /*resume_restored_queue*/ false);

    let mut rendered_cells = Vec::new();
    while let Ok(event) = app_event_rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            rendered_cells.push(lines_to_single_string(&cell.display_lines(/*width*/ 120)));
        }
    }
    let rendered = rendered_cells.join("\n");
    assert_eq!(app.chat_widget.thread_id(), Some(child_thread_id));
    assert_eq!(rendered.matches("sentry is not logged in").count(), 1);
    assert_eq!(
        rendered
            .matches("MCP startup incomplete (failed: sentry)")
            .count(),
        1
    );
}

#[tokio::test]
async fn app_server_disconnect_is_visible_without_requesting_tui_exit() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    while app_event_rx.try_recv().is_ok() {}
    let app_server = crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
        .await
        .expect("embedded app server");

    app.handle_app_server_event(
        &app_server,
        codex_app_server_client::AppServerEvent::Disconnected {
            message: "injected transport loss".to_string(),
        },
    )
    .await;

    let mut rendered = Vec::new();
    while let Ok(event) = app_event_rx.try_recv() {
        match event {
            AppEvent::InsertHistoryCell(cell) => {
                rendered.push(lines_to_single_string(&cell.display_lines(/*width*/ 120)));
            }
            AppEvent::Exit(_) | AppEvent::FatalExitRequest { .. } => {
                panic!("a routine app-server disconnect must not request TUI exit");
            }
            _ => {}
        }
    }
    assert!(
        rendered.join("\n").contains("reconnect automatically"),
        "degraded connection state should be visible"
    );
}

#[tokio::test]
async fn app_scoped_mcp_startup_notifications_do_not_render_in_active_thread() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    while app_event_rx.try_recv().is_ok() {}
    let app_server = crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
        .await
        .expect("embedded app server");
    let thread_id = ThreadId::new();
    app.primary_thread_id = Some(thread_id);
    app.active_thread_id = Some(thread_id);

    app.handle_app_server_event(
        &app_server,
        codex_app_server_client::AppServerEvent::ServerNotification(Box::new(
            ServerNotification::McpServerStatusUpdated(McpServerStatusUpdatedNotification {
                thread_id: None,
                name: "sentry".to_string(),
                status: McpServerStartupState::Failed,
                error: Some("sentry is not logged in".to_string()),
                failure_reason: None,
            }),
        )),
    )
    .await;

    assert!(app_event_rx.try_recv().is_err());
    assert_eq!(
        app.chat_widget.active_cell_transcript_lines(/*width*/ 120),
        None
    );
}

#[tokio::test]
async fn active_side_thread_renders_live_mcp_startup_notifications() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    while app_event_rx.try_recv().is_ok() {}
    let sentry_config = toml::from_str::<toml::Value>("command = 'true'")
        .expect("test MCP config should parse")
        .try_into()
        .expect("test MCP config should deserialize");
    app.config
        .mcp_servers
        .set(std::collections::HashMap::from([(
            "sentry".to_string(),
            sentry_config,
        )]))
        .expect("test MCP servers should accept any configuration");
    let app_server = crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
        .await
        .expect("embedded app server");
    let parent_thread_id = ThreadId::new();
    let side_thread_id = ThreadId::new();
    app.primary_thread_id = Some(parent_thread_id);
    app.side_threads
        .insert(side_thread_id, SideThreadState::new(parent_thread_id));
    app.ensure_thread_channel(side_thread_id);
    app.activate_thread_channel(side_thread_id).await;
    app.replay_thread_snapshot(
        ThreadEventSnapshot {
            session: Some(test_thread_session(
                side_thread_id,
                test_path_buf("/tmp/side"),
            )),
            turns: Vec::new(),
            events: Vec::new(),
            input_state: None,
        },
        /*resume_restored_queue*/ false,
    );
    app.sync_side_thread_ui();

    for status in [
        McpServerStartupState::Starting,
        McpServerStartupState::Failed,
    ] {
        app.handle_app_server_event(
            &app_server,
            codex_app_server_client::AppServerEvent::ServerNotification(Box::new(
                ServerNotification::McpServerStatusUpdated(McpServerStatusUpdatedNotification {
                    thread_id: Some(side_thread_id.to_string()),
                    name: "sentry".to_string(),
                    status,
                    error: matches!(status, McpServerStartupState::Failed)
                        .then(|| "sentry is not logged in".to_string()),
                    failure_reason: None,
                }),
            )),
        )
        .await;
    }

    let mut active_thread_events = Vec::new();
    let active_thread_rx = app
        .active_thread_rx
        .as_mut()
        .expect("side thread receiver should be active");
    while let Ok(event) = active_thread_rx.try_recv() {
        active_thread_events.push(event);
    }
    for event in active_thread_events {
        app.handle_thread_event_now(event);
    }

    let mut rendered_cells = Vec::new();
    while let Ok(event) = app_event_rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            rendered_cells.push(lines_to_single_string(&cell.display_lines(/*width*/ 120)));
        }
    }
    let rendered = rendered_cells.join("\n");
    assert!(app.chat_widget.side_conversation_active());
    assert_eq!(rendered.matches("sentry is not logged in").count(), 1);
    assert_eq!(
        rendered
            .matches("MCP startup incomplete (failed: sentry)")
            .count(),
        1
    );
}

#[tokio::test]
async fn side_restore_user_message_puts_inline_question_back_in_composer() {
    let mut app = make_test_app().await;
    let user_message = crate::chatwidget::UserMessage::from("side question");

    app.restore_side_user_message(Some(user_message));

    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "side question"
    );
}

#[tokio::test]
async fn side_discard_selection_keeps_current_side_thread() {
    let mut app = make_test_app().await;
    let parent_thread_id = ThreadId::new();
    let side_thread_id = ThreadId::new();
    app.active_thread_id = Some(side_thread_id);
    app.side_threads
        .insert(side_thread_id, SideThreadState::new(parent_thread_id));

    assert_eq!(
        app.side_thread_to_discard_after_switch(side_thread_id),
        None
    );
    assert_eq!(
        app.side_thread_to_discard_after_switch(parent_thread_id),
        Some(side_thread_id)
    );

    app.active_thread_id = Some(parent_thread_id);
    assert_eq!(
        app.side_thread_to_discard_after_switch(ThreadId::new()),
        Some(side_thread_id)
    );
    assert_eq!(
        app.side_thread_to_discard_after_switch(side_thread_id),
        None
    );
}

#[tokio::test]
async fn discard_side_thread_removes_agent_navigation_entry() -> Result<()> {
    Box::pin(async {
        let mut app = make_test_app().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref()).await?;
        let mut side_config = app.chat_widget.config_ref().clone();
        side_config.ephemeral = true;
        let started = app_server.start_thread(&side_config).await?;
        let side_thread_id = started.session.thread_id;
        app.side_threads
            .insert(side_thread_id, SideThreadState::new(ThreadId::new()));
        app.agent_navigation.upsert(
            side_thread_id,
            Some("Side".to_string()),
            Some("side".to_string()),
            /*is_closed*/ false,
        );

        assert!(
            app.discard_side_thread(&mut app_server, side_thread_id)
                .await
        );

        assert_eq!(app.agent_navigation.get(&side_thread_id), None);
        assert!(!app.side_threads.contains_key(&side_thread_id));
        Ok(())
    })
    .await
}

#[tokio::test]
async fn discard_side_thread_keeps_local_state_when_server_close_fails() -> Result<()> {
    Box::pin(async {
        let mut app = make_test_app().await;
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref()).await?;
        let parent_thread_id = ThreadId::new();
        let side_thread_id = ThreadId::new();
        app.active_thread_id = Some(side_thread_id);
        app.side_threads
            .insert(side_thread_id, SideThreadState::new(parent_thread_id));
        app.agent_navigation.upsert(
            side_thread_id,
            Some("Side".to_string()),
            Some("side".to_string()),
            /*is_closed*/ false,
        );

        assert!(
            !app.discard_side_thread(&mut app_server, side_thread_id)
                .await
        );

        assert_eq!(app.active_thread_id, Some(side_thread_id));
        assert_eq!(
            app.side_threads
                .get(&side_thread_id)
                .map(|state| state.parent_thread_id),
            Some(parent_thread_id)
        );
        assert!(app.agent_navigation.get(&side_thread_id).is_some());
        Ok(())
    })
    .await
}

#[tokio::test]
async fn background_side_cleanup_removes_local_state_and_ignores_late_events() -> Result<()> {
    let mut app = make_test_app().await;
    let mut app_server =
        crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref()).await?;
    let parent_thread_id = ThreadId::new();
    let side_thread_id = ThreadId::new();
    app.active_thread_id = Some(parent_thread_id);
    app.side_threads
        .insert(side_thread_id, SideThreadState::new(parent_thread_id));
    app.thread_event_channels
        .insert(side_thread_id, ThreadEventChannel::new(/*capacity*/ 4));
    app.agent_navigation.upsert(
        side_thread_id,
        Some("Side".to_string()),
        Some("side".to_string()),
        /*is_closed*/ false,
    );
    app.discard_side_thread_in_background(&mut app_server, side_thread_id)
        .await;

    assert_eq!(app.active_thread_id, Some(parent_thread_id));
    assert!(!app.side_threads.contains_key(&side_thread_id));
    assert!(!app.thread_event_channels.contains_key(&side_thread_id));
    assert_eq!(app.agent_navigation.get(&side_thread_id), None);
    assert!(app.abandoned_side_threads.contains(&side_thread_id));

    app.enqueue_thread_notification(
        side_thread_id,
        agent_message_delta_notification(side_thread_id, "turn-1", "item-1", "late"),
    )
    .await?;
    assert!(!app.thread_event_channels.contains_key(&side_thread_id));

    app.handle_app_server_event(
        &app_server,
        codex_app_server_client::AppServerEvent::ServerRequest(Box::new(exec_approval_request(
            side_thread_id,
            "turn-1",
            "item-1",
            Some("approval-1"),
        ))),
    )
    .await;
    let resolution = app
        .pending_app_server_requests
        .take_resolution(Op::ExecApproval {
            id: "approval-1".to_string(),
            turn_id: None,
            decision: codex_app_server_protocol::CommandExecutionApprovalDecision::Accept,
        })
        .expect("approval resolution should serialize");
    assert_eq!(resolution, None);
    Ok(())
}

#[tokio::test]
async fn discard_closed_side_thread_removes_local_state_without_server_rpc() {
    let mut app = make_test_app().await;
    let parent_thread_id = ThreadId::new();
    let side_thread_id = ThreadId::new();
    app.active_thread_id = Some(side_thread_id);
    app.side_threads
        .insert(side_thread_id, SideThreadState::new(parent_thread_id));
    app.thread_event_channels
        .insert(side_thread_id, ThreadEventChannel::new(/*capacity*/ 4));
    app.agent_navigation.upsert(
        side_thread_id,
        Some("Side".to_string()),
        Some("side".to_string()),
        /*is_closed*/ false,
    );

    app.discard_closed_side_thread(side_thread_id).await;

    assert_eq!(app.active_thread_id, None);
    assert!(!app.side_threads.contains_key(&side_thread_id));
    assert!(!app.thread_event_channels.contains_key(&side_thread_id));
    assert_eq!(app.agent_navigation.get(&side_thread_id), None);
}

#[tokio::test]
async fn active_non_primary_shutdown_target_returns_none_for_non_shutdown_event() -> Result<()> {
    let mut app = make_test_app().await;
    app.active_thread_id = Some(ThreadId::new());
    app.primary_thread_id = Some(ThreadId::new());

    assert_eq!(
        app.active_non_primary_shutdown_target(&ServerNotification::SkillsChanged(
            codex_app_server_protocol::SkillsChangedNotification {},
        )),
        None
    );
    Ok(())
}

#[tokio::test]
async fn active_non_primary_shutdown_target_returns_none_for_primary_thread_shutdown() -> Result<()>
{
    let mut app = make_test_app().await;
    let thread_id = ThreadId::new();
    app.active_thread_id = Some(thread_id);
    app.primary_thread_id = Some(thread_id);

    assert_eq!(
        app.active_non_primary_shutdown_target(&thread_closed_notification(thread_id)),
        None
    );
    Ok(())
}

#[tokio::test]
async fn active_non_primary_shutdown_target_returns_ids_for_non_primary_shutdown() -> Result<()> {
    let mut app = make_test_app().await;
    let active_thread_id = ThreadId::new();
    let primary_thread_id = ThreadId::new();
    app.active_thread_id = Some(active_thread_id);
    app.primary_thread_id = Some(primary_thread_id);

    assert_eq!(
        app.active_non_primary_shutdown_target(&thread_closed_notification(active_thread_id)),
        Some((active_thread_id, primary_thread_id))
    );
    Ok(())
}

#[tokio::test]
async fn active_non_primary_shutdown_target_returns_none_when_shutdown_exit_is_pending()
-> Result<()> {
    let mut app = make_test_app().await;
    let active_thread_id = ThreadId::new();
    let primary_thread_id = ThreadId::new();
    app.active_thread_id = Some(active_thread_id);
    app.primary_thread_id = Some(primary_thread_id);
    app.pending_shutdown_exit_thread_id = Some(active_thread_id);

    assert_eq!(
        app.active_non_primary_shutdown_target(&thread_closed_notification(active_thread_id)),
        None
    );
    Ok(())
}

#[tokio::test]
async fn active_non_primary_shutdown_target_still_switches_for_other_pending_exit_thread()
-> Result<()> {
    let mut app = make_test_app().await;
    let active_thread_id = ThreadId::new();
    let primary_thread_id = ThreadId::new();
    app.active_thread_id = Some(active_thread_id);
    app.primary_thread_id = Some(primary_thread_id);
    app.pending_shutdown_exit_thread_id = Some(ThreadId::new());

    assert_eq!(
        app.active_non_primary_shutdown_target(&thread_closed_notification(active_thread_id)),
        Some((active_thread_id, primary_thread_id))
    );
    Ok(())
}

async fn render_clear_ui_header_after_long_transcript_for_snapshot() -> String {
    let mut app = make_test_app().await;
    app.config.cwd = test_path_buf("/tmp/project").abs();
    app.chat_widget.set_model("gpt-test");
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::High));
    let story_part_one = "In the cliffside town of Bracken Ferry, the lighthouse had been dark for \
            nineteen years, and the children were told it was because the sea no longer wanted a \
            guide. Mara, who repaired clocks for a living, found that hard to believe. Every dawn she \
            heard the gulls circling the empty tower, and every dusk she watched ships hesitate at the \
            mouth of the bay as if listening for a signal that never came. When an old brass key fell \
            out of a cracked parcel in her workshop, tagged only with the words 'for the lamp room,' \
            she decided to climb the hill and see what the town had forgotten.";
    let story_part_two = "Inside the lighthouse she found gears wrapped in oilcloth, logbooks filled \
            with weather notes, and a lens shrouded beneath salt-stiff canvas. The mechanism was not \
            broken, only unfinished. Someone had removed the governor spring and hidden it in a false \
            drawer, along with a letter from the last keeper admitting he had darkened the light on \
            purpose after smugglers threatened his family. Mara spent the night rebuilding the clockwork \
            from spare watch parts, her fingers blackened with soot and grease, while a storm gathered \
            over the water and the harbor bells began to ring.";
    let story_part_three = "At midnight the first squall hit, and the fishing boats returned early, \
            blind in sheets of rain. Mara wound the mechanism, set the teeth by hand, and watched the \
            great lens begin to turn in slow, certain arcs. The beam swept across the bay, caught the \
            whitecaps, and reached the boats just as they were drifting toward the rocks below the \
            eastern cliffs. In the morning the town square was crowded with wet sailors, angry elders, \
            and wide-eyed children, but when the oldest captain placed the keeper's log on the fountain \
            and thanked Mara for relighting the coast, nobody argued. By sunset, Bracken Ferry had a \
            lighthouse again, and Mara had more clocks to mend than ever because everyone wanted \
            something in town to keep better time.";

    let user_cell = |text: &str| -> Arc<dyn HistoryCell> {
        Arc::new(UserHistoryCell {
            message: text.to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
        }) as Arc<dyn HistoryCell>
    };
    let agent_cell = |text: &str| -> Arc<dyn HistoryCell> {
        Arc::new(AgentMessageCell::new(
            vec![Line::from(text.to_string())],
            /*is_first_line*/ true,
        )) as Arc<dyn HistoryCell>
    };
    let make_header = |is_first| -> Arc<dyn HistoryCell> {
        let session = ThreadSessionState {
            thread_id: ThreadId::new(),
            forked_from_id: None,
            fork_parent_title: None,
            thread_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: AskForApproval::Never,
            approvals_reviewer: ApprovalsReviewer::User,
            permission_profile: PermissionProfile::read_only(),
            active_permission_profile: None,
            cwd: test_path_buf("/tmp/project").abs(),
            runtime_workspace_roots: Vec::new(),
            instruction_source_paths: Vec::new(),
            reasoning_effort: Some(ReasoningEffortConfig::High),
            collaboration_mode: None,
            personality: None,
            message_history: None,
            network_proxy: None,
            rollout_path: Some(PathBuf::new()),
            provider_account: None,
        };
        Arc::new(new_session_info(
            app.chat_widget.config_ref(),
            app.chat_widget.current_model(),
            &session,
            is_first,
            /*tooltip_override*/ None,
            /*auth_plan*/ None,
            /*show_fast_status*/ false,
        )) as Arc<dyn HistoryCell>
    };

    app.transcript_cells = vec![
        make_header(true),
        Arc::new(crate::history_cell::new_info_event(
            "startup tip that used to replay".to_string(),
            /*hint*/ None,
        )) as Arc<dyn HistoryCell>,
        user_cell("Tell me a long story about a town with a dark lighthouse."),
        agent_cell(story_part_one),
        user_cell("Continue the story and reveal why the light went out."),
        agent_cell(story_part_two),
        user_cell("Finish the story with a storm and a resolution."),
        agent_cell(story_part_three),
    ];
    app.has_emitted_history_lines = true;

    let rendered = app
        .clear_ui_header_lines_with_version(/*width*/ 80, "<VERSION>")
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        !rendered.contains("startup tip that used to replay"),
        "clear header should not replay startup notices"
    );
    assert!(
        !rendered.contains("Bracken Ferry"),
        "clear header should not replay prior conversation turns"
    );
    rendered
}

#[tokio::test]
#[cfg_attr(
    target_os = "windows",
    ignore = "snapshot path rendering differs on Windows"
)]
async fn clear_ui_after_long_transcript_snapshots_fresh_header_only() {
    let rendered = render_clear_ui_header_after_long_transcript_for_snapshot().await;
    assert_app_snapshot!("clear_ui_after_long_transcript_fresh_header_only", rendered);
}

#[tokio::test]
#[cfg_attr(
    target_os = "windows",
    ignore = "snapshot path rendering differs on Windows"
)]
async fn ctrl_l_clear_ui_after_long_transcript_reuses_clear_header_snapshot() {
    let rendered = render_clear_ui_header_after_long_transcript_for_snapshot().await;
    assert_app_snapshot!("clear_ui_after_long_transcript_fresh_header_only", rendered);
}

#[tokio::test]
#[cfg_attr(
    target_os = "windows",
    ignore = "snapshot path rendering differs on Windows"
)]
async fn clear_ui_header_shows_fast_status_for_fast_capable_models() {
    let mut app = make_test_app().await;
    app.config.cwd = test_path_buf("/tmp/project").abs();
    app.chat_widget.set_model("gpt-5.4");
    set_fast_mode_test_catalog(&mut app.chat_widget);
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::XHigh));
    app.chat_widget.set_service_tier(Some(
        codex_protocol::config_types::ServiceTier::Fast
            .request_value()
            .to_string(),
    ));
    set_chatgpt_auth(&mut app.chat_widget);
    set_fast_mode_test_catalog(&mut app.chat_widget);

    let rendered = app
        .clear_ui_header_lines_with_version(/*width*/ 80, "<VERSION>")
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert_app_snapshot!("clear_ui_header_fast_status_fast_capable_models", rendered);
}

#[tokio::test]
#[cfg_attr(
    target_os = "windows",
    ignore = "snapshot path rendering differs on Windows"
)]
async fn model_change_refreshes_committed_session_header_snapshot() -> Result<()> {
    let mut app = make_test_app().await;
    app.config.cwd = test_path_buf("/tmp/project").abs();
    app.chat_widget.set_model("gpt-before");
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::Medium));

    let mut session = test_thread_session(ThreadId::new(), test_path_buf("/tmp/project"));
    session.model = "gpt-before".to_string();
    session.reasoning_effort = Some(ReasoningEffortConfig::Medium);
    app.transcript_cells = vec![Arc::new(new_session_info(
        app.chat_widget.config_ref(),
        app.chat_widget.current_model(),
        &session,
        /*is_first_event*/ true,
        /*tooltip_override*/ None,
        /*auth_plan*/ None,
        /*show_fast_status*/ false,
    )) as Arc<dyn HistoryCell>];
    app.has_emitted_history_lines = true;

    app.chat_widget.set_model("gpt-after");
    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::High));
    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.refresh_session_header(&mut tui)?;

    let rendered = app.transcript_cells[0]
        .display_lines(/*width*/ 80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!rendered.contains("gpt-before"));
    assert_app_snapshot!("model_change_refreshes_committed_session_header", rendered);
    Ok(())
}

#[tokio::test]
async fn clear_header_remains_source_backed_for_model_refresh() -> Result<()> {
    let mut app = make_test_app().await;
    app.config.cwd = test_path_buf("/tmp/project").abs();
    let mut tui = crate::tui::test_support::make_test_tui()?;

    app.queue_clear_ui_header(&mut tui);
    assert_eq!(app.transcript_cells.len(), 1);
    assert!(
        app.transcript_cells[0]
            .as_any()
            .is::<history_cell::SessionHeaderHistoryCell>()
    );

    app.chat_widget.set_model("gpt-after-clear");
    app.refresh_session_header(&mut tui)?;

    let rendered = app.transcript_cells[0]
        .display_lines(/*width*/ 80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("gpt-after-clear"));
    Ok(())
}

pub(super) async fn make_test_app() -> App {
    let (chat_widget, app_event_tx, _rx, _op_rx) = make_chatwidget_manual_with_sender().await;
    let config = chat_widget.config_ref().clone();
    let file_search = FileSearchManager::new(config.cwd.to_path_buf(), app_event_tx.clone());
    let model = get_model_offline_for_tests(config.model.as_deref());
    let session_telemetry = test_session_telemetry(&config, model.as_str());

    App {
        model_catalog: chat_widget.model_catalog(),
        session_telemetry,
        app_event_tx,
        chat_widget,
        workspace_command_runner: None,
        launch_cwd: config.cwd.to_path_buf(),
        config,
        active_deferred_provider_setup: None,
        shared_provider_setup_session: None,
        shared_provider_status_host: None,
        shared_provider_account_auth_host: None,
        provider_management_host: None,
        provider_management_generation: 0,
        pending_wallet_create_deferred: None,
        state_db: None,
        cli_kv_overrides: Vec::new(),
        harness_overrides: ConfigOverrides::default(),
        loader_overrides: LoaderOverrides::without_managed_config_for_tests(),
        cloud_config_bundle: CloudConfigBundleLoader::default(),
        runtime_approval_policy_override: None,
        runtime_permission_profile_override: None,
        pending_permission_confirmation: None,
        file_search,
        transcript_cells: Vec::new(),
        claude_pane_transcript_cells: HashMap::new(),
        overlay: None,
        deferred_history_lines: Vec::new(),
        has_emitted_history_lines: false,
        transcript_reflow: TranscriptReflowState::default(),
        initial_history_replay_buffer: None,
        enhanced_keys_supported: false,
        keymap: crate::keymap::RuntimeKeymap::defaults(),
        commit_anim_running: Arc::new(AtomicBool::new(false)),
        status_line_invalid_items_warned: Arc::new(AtomicBool::new(false)),
        terminal_title_invalid_items_warned: Arc::new(AtomicBool::new(false)),
        skill_load_warnings: SkillLoadWarningState::default(),
        skills_refresh_generation: Arc::new(AtomicU64::new(0)),
        backtrack: BacktrackState::default(),
        backtrack_render_pending: false,
        feedback: codex_feedback::CodexFeedback::new(),
        feedback_audience: FeedbackAudience::External,
        environment_manager: Arc::new(EnvironmentManager::default_for_tests()),
        app_server_target: crate::AppServerTarget::Embedded,
        pending_update_action: None,
        pending_shutdown_exit_thread_id: None,
        windows_sandbox: WindowsSandboxState::default(),
        thread_event_channels: HashMap::new(),
        thread_event_listener_tasks: HashMap::new(),
        agent_navigation: AgentNavigationState::default(),
        spawn_parent_by_thread: HashMap::new(),
        spawn_parent_by_node: HashMap::new(),
        spawn_native_runtime_by_node: HashMap::new(),
        spawn_native_endpoint_by_node: HashMap::new(),
        spawn_crew: None,
        spawn_legacy_read_only: false,
        spawn_status_by_thread: HashMap::new(),
        spawn_waiting_for_agents_by_thread: HashMap::new(),
        spawn_parent_reports_by_node: HashMap::new(),
        spawn_dispatch_acks_by_target_task: HashMap::new(),
        spawn_next_dispatch_seq: 1,
        spawn_processed_dispatch_seq_ids: HashSet::new(),
        spawn_processed_dispatch_origins: HashSet::new(),
        spawn_processed_terminal_turns: HashSet::new(),
        spawn_auto_loop_state_by_node: HashMap::new(),
        spawn_operator_input_seen: false,
        kill_switch_held_claude_tasks: Vec::new(),
        kill_switch_held_native_tasks: Vec::new(),
        kill_switch_known_on: false,
        spawn_quarantine_notified_by_node: HashSet::new(),
        spawn_context_left_by_thread: HashMap::new(),
        spawn_last_report_seq_by_node: HashMap::new(),
        spawn_last_dispatch_seq_by_node: HashMap::new(),
        spawn_last_event_at_by_node: HashMap::new(),
        spawn_nazgul_pane_id: None,
        spawn_nazgul_rebind_required: false,
        orchestrate_whips: Box::new(HashMap::new()),
        orchestrate_next_whip_seq: 0,
        orchestrate_now_override: None,
        orchestrate_idle_generation_by_target: Box::new(HashMap::new()),
        side_threads: HashMap::new(),
        claude_panes: Default::default(),
        abandoned_side_threads: HashSet::new(),
        active_thread_id: None,
        active_thread_rx: None,
        primary_thread_id: None,
        last_subagent_backfill_attempt: None,
        primary_session_configured: None,
        pending_primary_events: VecDeque::new(),
        pending_app_server_requests: PendingAppServerRequests::default(),
        pending_startup_thread_start: false,
        rate_limit_hard_stop_generation: 0,
        pending_plugin_enabled_writes: HashMap::new(),
        pending_hook_enabled_writes: HashMap::new(),
    }
}

pub(super) async fn make_test_app_with_channels() -> (
    App,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<Op>,
) {
    let (chat_widget, app_event_tx, rx, op_rx) = make_chatwidget_manual_with_sender().await;
    let config = chat_widget.config_ref().clone();
    let file_search = FileSearchManager::new(config.cwd.to_path_buf(), app_event_tx.clone());
    let model = get_model_offline_for_tests(config.model.as_deref());
    let session_telemetry = test_session_telemetry(&config, model.as_str());

    (
        App {
            model_catalog: chat_widget.model_catalog(),
            session_telemetry,
            app_event_tx,
            chat_widget,
            workspace_command_runner: None,
            launch_cwd: config.cwd.to_path_buf(),
            config,
            active_deferred_provider_setup: None,
            shared_provider_setup_session: None,
            shared_provider_status_host: None,
            shared_provider_account_auth_host: None,
            provider_management_host: None,
            provider_management_generation: 0,
            pending_wallet_create_deferred: None,
            state_db: None,
            cli_kv_overrides: Vec::new(),
            harness_overrides: ConfigOverrides::default(),
            loader_overrides: LoaderOverrides::without_managed_config_for_tests(),
            cloud_config_bundle: CloudConfigBundleLoader::default(),
            runtime_approval_policy_override: None,
            runtime_permission_profile_override: None,
            pending_permission_confirmation: None,
            file_search,
            transcript_cells: Vec::new(),
            claude_pane_transcript_cells: HashMap::new(),
            overlay: None,
            deferred_history_lines: Vec::new(),
            has_emitted_history_lines: false,
            transcript_reflow: TranscriptReflowState::default(),
            initial_history_replay_buffer: None,
            enhanced_keys_supported: false,
            keymap: crate::keymap::RuntimeKeymap::defaults(),
            commit_anim_running: Arc::new(AtomicBool::new(false)),
            status_line_invalid_items_warned: Arc::new(AtomicBool::new(false)),
            terminal_title_invalid_items_warned: Arc::new(AtomicBool::new(false)),
            skill_load_warnings: SkillLoadWarningState::default(),
            skills_refresh_generation: Arc::new(AtomicU64::new(0)),
            backtrack: BacktrackState::default(),
            backtrack_render_pending: false,
            feedback: codex_feedback::CodexFeedback::new(),
            feedback_audience: FeedbackAudience::External,
            environment_manager: Arc::new(EnvironmentManager::default_for_tests()),
            app_server_target: crate::AppServerTarget::Embedded,
            pending_update_action: None,
            pending_shutdown_exit_thread_id: None,
            windows_sandbox: WindowsSandboxState::default(),
            thread_event_channels: HashMap::new(),
            thread_event_listener_tasks: HashMap::new(),
            agent_navigation: AgentNavigationState::default(),
            spawn_parent_by_thread: HashMap::new(),
            spawn_parent_by_node: HashMap::new(),
            spawn_native_runtime_by_node: HashMap::new(),
            spawn_native_endpoint_by_node: HashMap::new(),
            spawn_crew: None,
            spawn_legacy_read_only: false,
            spawn_status_by_thread: HashMap::new(),
            spawn_waiting_for_agents_by_thread: HashMap::new(),
            spawn_parent_reports_by_node: HashMap::new(),
            spawn_dispatch_acks_by_target_task: HashMap::new(),
            spawn_next_dispatch_seq: 1,
            spawn_processed_dispatch_seq_ids: HashSet::new(),
            spawn_processed_dispatch_origins: HashSet::new(),
            spawn_processed_terminal_turns: HashSet::new(),
            spawn_auto_loop_state_by_node: HashMap::new(),
            spawn_operator_input_seen: false,
            kill_switch_held_claude_tasks: Vec::new(),
            kill_switch_held_native_tasks: Vec::new(),
            kill_switch_known_on: false,
            spawn_quarantine_notified_by_node: HashSet::new(),
            spawn_context_left_by_thread: HashMap::new(),
            spawn_last_report_seq_by_node: HashMap::new(),
            spawn_last_dispatch_seq_by_node: HashMap::new(),
            spawn_last_event_at_by_node: HashMap::new(),
            spawn_nazgul_pane_id: None,
            spawn_nazgul_rebind_required: false,
            orchestrate_whips: Box::new(HashMap::new()),
            orchestrate_next_whip_seq: 0,
            orchestrate_now_override: None,
            orchestrate_idle_generation_by_target: Box::new(HashMap::new()),
            side_threads: HashMap::new(),
            claude_panes: Default::default(),
            abandoned_side_threads: HashSet::new(),
            active_thread_id: None,
            active_thread_rx: None,
            primary_thread_id: None,
            last_subagent_backfill_attempt: None,
            primary_session_configured: None,
            pending_primary_events: VecDeque::new(),
            pending_app_server_requests: PendingAppServerRequests::default(),
            pending_startup_thread_start: false,
            rate_limit_hard_stop_generation: 0,
            pending_plugin_enabled_writes: HashMap::new(),
            pending_hook_enabled_writes: HashMap::new(),
        },
        rx,
        op_rx,
    )
}

#[tokio::test]
async fn set_thread_goal_draft_materializes_long_objective_and_confirms_before_paste() -> Result<()>
{
    let mut app = make_test_app().await;
    let mut app_server =
        crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref()).await?;
    let started = app_server
        .start_thread(app.chat_widget.config_ref())
        .await?;
    let thread_id = started.session.thread_id;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    let objective = "x".repeat(MAX_THREAD_GOAL_OBJECTIVE_CHARS + 1);

    app.set_thread_goal_draft(
        &mut app_server,
        thread_id,
        crate::goal_files::GoalDraft {
            objective: objective.clone(),
            ..Default::default()
        },
        crate::app_event::ThreadGoalSetMode::ConfirmIfExists,
    )
    .await;

    let response = app_server.thread_goal_get(thread_id).await?;
    let goal = response.goal.expect("goal should be set");
    let saved_objective = goal.objective.clone();
    let codex_home = app_server
        .codex_home_path(&app.chat_widget.config_ref().codex_home)
        .expect("codex home");
    assert!(goal_files::objective_file_path(&goal.objective, Some(&codex_home)).is_some());
    assert_eq!(
        goal_files::objective_text_for_edit(&mut app_server, Some(&codex_home), &goal.objective)
            .await
            .expect("managed goal file should be readable"),
        objective
    );
    let is_managed = |home: &AppServerPath, path: &str| {
        let reference = goal_files::objective_file_reference(&AppServerPath::from_app_server(path))
            .expect("goal objective reference");
        goal_files::objective_file_path(&reference, Some(home)).is_some()
    };
    let suffix = "attachments/00000000-0000-4000-8000-000000000000/goal-objective.md";
    for path in [
        format!("/tmp/{suffix}"),
        format!("{codex_home}/../other/{suffix}"),
        format!("{codex_home}/other/{suffix}"),
    ] {
        assert!(!is_managed(&codex_home, &path));
    }
    assert!(!is_managed(
        &AppServerPath::from_app_server("/tmp/codex\\home"),
        &format!("/tmp/codex/home/{suffix}")
    ));
    let unix_path = AppServerPath::from_app_server("/tmp/codex\\").join("a");
    assert_eq!(unix_path.as_str(), "/tmp/codex\\/a");
    let attachments_dir = app.chat_widget.config_ref().codex_home.join("attachments");
    let attachment_count = std::fs::read_dir(&attachments_dir)?.count();
    let placeholder = "[Pasted Content 5 chars]";
    let paste_draft = crate::goal_files::GoalDraft {
        objective: format!("Use {placeholder}"),
        text_elements: vec![TextElement::new(
            (4..4 + placeholder.len()).into(),
            Some(placeholder.to_string()),
        )],
        pending_pastes: vec![(placeholder.to_string(), "hello".to_string())],
        ..Default::default()
    };

    app.set_thread_goal_draft(
        &mut app_server,
        thread_id,
        paste_draft.clone(),
        crate::app_event::ThreadGoalSetMode::ConfirmIfExists,
    )
    .await;

    assert_eq!(
        std::fs::read_dir(&attachments_dir)?.count(),
        attachment_count
    );
    assert_eq!(
        app_server
            .thread_goal_get(thread_id)
            .await?
            .goal
            .expect("goal should still be set")
            .objective,
        saved_objective
    );

    app.set_thread_goal_draft(
        &mut app_server,
        thread_id,
        paste_draft,
        crate::app_event::ThreadGoalSetMode::ReplaceExisting,
    )
    .await;
    let goal = app_server
        .thread_goal_get(thread_id)
        .await?
        .goal
        .expect("replacement goal should be set");
    let paste_path = goal
        .objective
        .strip_prefix("Use pasted text file: ")
        .and_then(|text| text.strip_suffix(". Read this file before continuing."))
        .expect("paste file reference");
    assert_eq!(std::fs::read_to_string(paste_path)?, "hello");
    let attachment_count = std::fs::read_dir(&attachments_dir)?.count();

    let stale_paste = (placeholder.to_string(), "hello".to_string());
    app.set_thread_goal_draft(
        &mut app_server,
        thread_id,
        crate::goal_files::GoalDraft {
            objective: "small goal".to_string(),
            pending_pastes: vec![stale_paste],
            ..Default::default()
        },
        crate::app_event::ThreadGoalSetMode::ReplaceExisting,
    )
    .await;
    assert_eq!(
        std::fs::read_dir(&attachments_dir)?.count(),
        attachment_count
    );

    let whitespace_placeholder = "[Pasted Content 3 chars]";
    app.set_thread_goal_draft(
        &mut app_server,
        thread_id,
        crate::goal_files::GoalDraft {
            objective: whitespace_placeholder.to_string(),
            text_elements: vec![TextElement::new(
                (0..whitespace_placeholder.len()).into(),
                Some(whitespace_placeholder.to_string()),
            )],
            pending_pastes: vec![(whitespace_placeholder.to_string(), " \n\t".to_string())],
            ..Default::default()
        },
        crate::app_event::ThreadGoalSetMode::ReplaceExisting,
    )
    .await;
    assert_eq!(
        std::fs::read_dir(&attachments_dir)?.count(),
        attachment_count
    );
    assert_eq!(
        app_server
            .thread_goal_get(thread_id)
            .await?
            .goal
            .expect("small goal should remain set")
            .objective,
        "small goal"
    );

    let image_dir = tempfile::tempdir()?;
    let image_path = image_dir.path().join("local-image.png");
    std::fs::write(&image_path, b"png bytes")?;
    let image_placeholder = "[Image #3]";
    app.set_thread_goal_draft(
        &mut app_server,
        thread_id,
        crate::goal_files::GoalDraft {
            objective: format!("Describe {image_placeholder}"),
            text_elements: vec![TextElement::new(
                (9..9 + image_placeholder.len()).into(),
                Some(image_placeholder.to_string()),
            )],
            local_images: vec![crate::bottom_pane::LocalImageAttachment {
                placeholder: image_placeholder.to_string(),
                path: image_path,
            }],
            remote_image_urls: vec![
                "https://example.com/first.png".to_string(),
                "https://example.com/second.png".to_string(),
            ],
            ..Default::default()
        },
        crate::app_event::ThreadGoalSetMode::ReplaceExisting,
    )
    .await;
    let objective = app_server
        .thread_goal_get(thread_id)
        .await?
        .goal
        .expect("image goal should be set")
        .objective;
    let copied_image = objective
        .strip_prefix("Describe image file: ")
        .and_then(|text| text.split_once("\n\n"))
        .map(|(path, _)| path)
        .expect("copied image path");
    assert_eq!(std::fs::read(copied_image)?, b"png bytes");
    assert!(objective.contains(
        "Referenced image URLs:\n- [Image #1]: https://example.com/first.png\n- [Image #2]: https://example.com/second.png"
    ));
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn replace_goal_confirmation_snapshot() {
    let mut app = make_test_app().await;
    app.show_replace_thread_goal_confirmation(
        ThreadId::new(),
        goal_files::GoalDraft {
            objective: "New goal".to_string(),
            ..Default::default()
        },
    );
    assert_app_snapshot!(
        "replace_goal_confirmation",
        render_bottom_popup(&app.chat_widget, /*width*/ 80)
    );
}

pub(super) fn test_thread_session(thread_id: ThreadId, cwd: PathBuf) -> ThreadSessionState {
    ThreadSessionState {
        thread_id,
        forked_from_id: None,
        fork_parent_title: None,
        thread_name: None,
        model: "gpt-test".to_string(),
        model_provider_id: "test-provider".to_string(),
        service_tier: None,
        approval_policy: AskForApproval::Never,
        approvals_reviewer: ApprovalsReviewer::User,
        permission_profile: PermissionProfile::read_only(),
        active_permission_profile: None,
        cwd: cwd.abs(),
        runtime_workspace_roots: Vec::new(),
        instruction_source_paths: Vec::new(),
        reasoning_effort: None,
        collaboration_mode: None,
        personality: None,
        message_history: None,
        network_proxy: None,
        rollout_path: Some(PathBuf::new()),
        provider_account: None,
    }
}

fn plain_line_cell(text: impl Into<String>) -> Arc<dyn HistoryCell> {
    Arc::new(PlainHistoryCell::new(vec![Line::from(text.into())])) as Arc<dyn HistoryCell>
}

fn rendered_line_text(line: &crate::terminal_hyperlinks::HyperlinkLine) -> String {
    line.line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

#[tokio::test]
async fn capped_resize_reflow_renders_recent_suffix_only() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(5);
    app.transcript_cells = (0..20)
        .map(|i| plain_line_cell(format!("cell {i}")))
        .collect();

    let rendered = app.render_transcript_lines_for_reflow(/*width*/ 80);

    assert_eq!(rendered.lines.len(), 5);
    assert_eq!(
        rendered
            .lines
            .iter()
            .map(rendered_line_text)
            .collect::<Vec<_>>(),
        vec![
            "cell 17".to_string(),
            String::new(),
            "cell 18".to_string(),
            String::new(),
            "cell 19".to_string(),
        ]
    );
}

#[tokio::test]
async fn uncapped_resize_reflow_renders_all_cells_when_row_cap_absent() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Disabled;
    app.transcript_cells = (0..20)
        .map(|i| plain_line_cell(format!("cell {i}")))
        .collect();

    let rendered = app.render_transcript_lines_for_reflow(/*width*/ 80);

    assert_eq!(rendered.lines.len(), 39);
    assert_eq!(rendered_line_text(&rendered.lines[0]), "cell 0");
    assert_eq!(rendered_line_text(&rendered.lines[38]), "cell 19");
}

#[tokio::test]
async fn resize_reflow_wraps_transcript_early_when_pet_is_enabled() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Disabled;
    app.transcript_cells = vec![Arc::new(AgentMarkdownCell::new(
        "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda".to_string(),
        Path::new("/tmp"),
    ))];

    let without_pet = app.render_transcript_lines_for_reflow(/*width*/ 40);
    app.chat_widget
        .set_pet_image_support_for_tests(crate::pets::PetImageSupport::Supported(
            crate::pets::ImageProtocol::Kitty,
        ));
    app.chat_widget
        .install_test_ambient_pet_for_tests(/*animations_enabled*/ false);
    let width = app.chat_widget.history_wrap_width(/*width*/ 40);
    assert!(width < 40);
    let with_pet = app.render_transcript_lines_for_reflow(width);

    assert!(
        with_pet.lines.len() > without_pet.lines.len(),
        "expected pet-enabled transcript reflow to wrap earlier"
    );
}

#[tokio::test]
async fn uncapped_resize_reflow_renders_all_cells_under_row_limit() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(100);
    app.transcript_cells = (0..3)
        .map(|i| plain_line_cell(format!("cell {i}")))
        .collect();

    let rendered = app.render_transcript_lines_for_reflow(/*width*/ 80);

    assert_eq!(
        rendered
            .lines
            .iter()
            .map(rendered_line_text)
            .collect::<Vec<_>>(),
        vec![
            "cell 0".to_string(),
            String::new(),
            "cell 1".to_string(),
            String::new(),
            "cell 2".to_string(),
        ]
    );
}

#[tokio::test]
async fn initial_replay_buffer_keeps_recent_rows_when_row_cap_present() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(3);

    app.begin_initial_history_replay_buffer();
    for index in 0..5 {
        App::buffer_initial_history_replay_display_lines(
            app.initial_history_replay_buffer
                .as_mut()
                .expect("initial replay buffer active"),
            vec![Line::from(format!("line {index}")).into()],
            /*max_rows*/ 3,
        );
    }

    let buffer = app
        .initial_history_replay_buffer
        .as_ref()
        .expect("initial replay buffer should remain active");
    assert_eq!(
        buffer
            .retained_lines
            .iter()
            .map(rendered_line_text)
            .collect::<Vec<_>>(),
        vec![
            "line 2".to_string(),
            "line 3".to_string(),
            "line 4".to_string(),
        ]
    );
}

#[tokio::test]
async fn required_stream_reflow_during_capped_initial_replay_uses_transcript_tail() -> Result<()> {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(20);
    app.transcript_cells = vec![
        plain_line_cell("latest user question"),
        Arc::new(AgentMarkdownCell::new(
            "Final answer:\n\n| Pattern | Outcome |\n| --- | --- |\n| Table tail | Preserved |"
                .to_string(),
            Path::new("/tmp"),
        )),
    ];

    app.begin_initial_history_replay_buffer();
    App::buffer_initial_history_replay_display_lines(
        app.initial_history_replay_buffer
            .as_mut()
            .expect("initial replay buffer active"),
        vec![Line::from("latest user question").into()],
        /*max_rows*/ 20,
    );

    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.finish_required_stream_reflow(&mut tui)?;

    let buffer = app
        .initial_history_replay_buffer
        .as_ref()
        .expect("initial replay buffer should remain active");
    assert_eq!(
        (
            buffer.retained_lines.len(),
            buffer.render_from_transcript_tail
        ),
        (0, true),
    );

    let rendered = app.render_transcript_lines_for_reflow(/*width*/ 80);
    assert_snapshot!(
        "required_stream_reflow_during_capped_initial_replay",
        rendered
            .lines
            .iter()
            .map(rendered_line_text)
            .collect::<Vec<_>>()
            .join("\n")
    );

    app.finish_initial_history_replay_buffer(&mut tui);
    assert!(app.initial_history_replay_buffer.is_none());
    assert!(app.transcript_reflow.has_pending_reflow());
    Ok(())
}

#[tokio::test]
async fn directive_only_completion_removes_streamed_directive() -> Result<()> {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(20);
    app.begin_initial_history_replay_buffer();
    app.transcript_cells = vec![
        plain_line_cell("before directive"),
        Arc::new(AgentMessageCell::new(
            vec![Line::from(r#"::git-stage{cwd="/tmp"}"#)],
            /*is_first_line*/ true,
        )),
    ];

    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.handle_consolidate_agent_message(
        &mut tui,
        String::new(),
        PathBuf::from("/tmp"),
        /*inline_visualization_context*/ None,
        ConsolidationScrollbackReflow::Required,
        /*deferred_history_cell*/ None,
    )?;

    let rendered = app.render_transcript_lines_for_reflow(/*width*/ 80);
    assert_snapshot!(
        "directive_only_completion_removes_streamed_directive",
        rendered
            .lines
            .iter()
            .map(rendered_line_text)
            .collect::<Vec<_>>()
            .join("\n")
    );
    Ok(())
}

#[tokio::test]
async fn required_stream_reflow_during_capped_initial_replay_survives_transcript_overlay()
-> Result<()> {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(7);
    app.transcript_cells = vec![
        plain_line_cell("latest user question"),
        Arc::new(AgentMessageCell::new(
            vec![Line::from("stale streamed table tail")],
            /*is_first_line*/ true,
        )),
    ];

    app.begin_initial_history_replay_buffer();
    App::buffer_initial_history_replay_display_lines(
        app.initial_history_replay_buffer
            .as_mut()
            .expect("initial replay buffer active"),
        vec![Line::from("stale streamed table tail").into()],
        /*max_rows*/ 7,
    );

    let mut tui = crate::tui::test_support::make_test_tui()?;
    app.handle_consolidate_agent_message(
        &mut tui,
        "Final answer:\n\n| Pattern | Outcome |\n| --- | --- |\n| Table tail | Preserved |"
            .to_string(),
        PathBuf::from("/tmp"),
        /*inline_visualization_context*/ None,
        ConsolidationScrollbackReflow::Required,
        /*deferred_history_cell*/ None,
    )?;
    app.open_transcript_overlay(&mut tui);
    assert!(tui.is_alt_screen_active());

    app.finish_initial_history_replay_buffer(&mut tui);
    assert!(app.initial_history_replay_buffer.is_none());
    assert!(app.transcript_reflow.has_pending_reflow());

    app.maybe_run_resize_reflow(&mut tui)?;
    assert!(app.transcript_reflow.has_pending_reflow());

    app.close_transcript_overlay(&mut tui);
    assert!(!tui.is_alt_screen_active());
    assert!(app.transcript_reflow.has_pending_reflow());

    let rendered = app.render_transcript_lines_for_reflow(/*width*/ 80);
    assert_eq!(rendered.lines.len(), 7);
    assert_snapshot!(
        "required_stream_reflow_during_capped_initial_replay_survives_transcript_overlay",
        rendered
            .lines
            .iter()
            .map(rendered_line_text)
            .collect::<Vec<_>>()
            .join("\n")
    );
    Ok(())
}

#[tokio::test]
async fn thread_switch_replay_buffer_uses_transcript_tail_mode_when_row_cap_present() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Limit(3);

    app.begin_thread_switch_history_replay_buffer();

    let buffer = app
        .initial_history_replay_buffer
        .as_ref()
        .expect("thread switch replay buffer should be active");
    assert!(buffer.render_from_transcript_tail);
    assert!(buffer.retained_lines.is_empty());
}

#[tokio::test]
async fn thread_switch_replay_buffer_is_disabled_without_row_cap() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    app.config.terminal_resize_reflow.max_rows = TerminalResizeReflowMaxRows::Disabled;

    app.begin_thread_switch_history_replay_buffer();

    assert!(app.initial_history_replay_buffer.is_none());
}

#[tokio::test]
async fn height_shrink_schedules_resize_reflow() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    let frame_requester = crate::tui::FrameRequester::test_dummy();

    assert!(!app.handle_draw_size_change(
        ratatui::layout::Size::new(/*width*/ 118, /*height*/ 35),
        ratatui::layout::Size::new(/*width*/ 118, /*height*/ 35),
        &frame_requester,
    ));

    assert!(app.handle_draw_size_change(
        ratatui::layout::Size::new(/*width*/ 118, /*height*/ 24),
        ratatui::layout::Size::new(/*width*/ 118, /*height*/ 35),
        &frame_requester,
    ));
    assert!(app.transcript_reflow.has_pending_reflow());
}

fn test_turn(turn_id: &str, status: TurnStatus, items: Vec<ThreadItem>) -> Turn {
    Turn {
        id: turn_id.to_string(),
        items_view: codex_app_server_protocol::TurnItemsView::Full,
        items,
        status,
        error: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
    }
}

fn turn_started_notification(thread_id: ThreadId, turn_id: &str) -> ServerNotification {
    ServerNotification::TurnStarted(TurnStartedNotification {
        thread_id: thread_id.to_string(),
        turn: Turn {
            started_at: Some(0),
            ..test_turn(turn_id, TurnStatus::InProgress, Vec::new())
        },
    })
}

fn turn_completed_notification(
    thread_id: ThreadId,
    turn_id: &str,
    status: TurnStatus,
) -> ServerNotification {
    ServerNotification::TurnCompleted(TurnCompletedNotification {
        thread_id: thread_id.to_string(),
        turn: Turn {
            completed_at: Some(0),
            duration_ms: Some(1),
            ..test_turn(turn_id, status, Vec::new())
        },
    })
}

fn turn_completed_with_agent_message(
    thread_id: ThreadId,
    turn_id: &str,
    status: TurnStatus,
    message: &str,
) -> ServerNotification {
    ServerNotification::TurnCompleted(TurnCompletedNotification {
        thread_id: thread_id.to_string(),
        turn: Turn {
            completed_at: Some(0),
            duration_ms: Some(1),
            ..test_turn(
                turn_id,
                status,
                vec![ThreadItem::AgentMessage {
                    id: "agent-message-1".to_string(),
                    text: message.to_string(),
                    phase: None,
                    memory_citation: None,
                }],
            )
        },
    })
}

fn thread_closed_notification(thread_id: ThreadId) -> ServerNotification {
    ServerNotification::ThreadClosed(ThreadClosedNotification {
        thread_id: thread_id.to_string(),
    })
}

fn token_usage_notification(
    thread_id: ThreadId,
    turn_id: &str,
    model_context_window: Option<i64>,
) -> ServerNotification {
    token_usage_notification_with_total(
        thread_id,
        turn_id,
        /*total_tokens*/ 10,
        model_context_window,
    )
}

fn token_usage_notification_with_total(
    thread_id: ThreadId,
    turn_id: &str,
    total_tokens: i64,
    model_context_window: Option<i64>,
) -> ServerNotification {
    ServerNotification::ThreadTokenUsageUpdated(ThreadTokenUsageUpdatedNotification {
        thread_id: thread_id.to_string(),
        turn_id: turn_id.to_string(),
        token_usage: ThreadTokenUsage {
            total: TokenUsageBreakdown {
                total_tokens,
                input_tokens: 4,
                cached_input_tokens: 1,
                cache_write_input_tokens: 0,
                output_tokens: 5,
                reasoning_output_tokens: 0,
            },
            last: TokenUsageBreakdown {
                total_tokens,
                input_tokens: 4,
                cached_input_tokens: 1,
                cache_write_input_tokens: 0,
                output_tokens: 5,
                reasoning_output_tokens: 0,
            },
            model_context_window,
        },
        account_usage: None,
    })
}

fn agent_message_delta_notification(
    thread_id: ThreadId,
    turn_id: &str,
    item_id: &str,
    delta: &str,
) -> ServerNotification {
    ServerNotification::AgentMessageDelta(AgentMessageDeltaNotification {
        thread_id: thread_id.to_string(),
        turn_id: turn_id.to_string(),
        item_id: item_id.to_string(),
        delta: delta.to_string(),
    })
}

fn item_completed_notification(
    thread_id: ThreadId,
    turn_id: &str,
    item_id: &str,
    message: &str,
) -> ServerNotification {
    ServerNotification::ItemCompleted(ItemCompletedNotification {
        thread_id: thread_id.to_string(),
        turn_id: turn_id.to_string(),
        item: ThreadItem::AgentMessage {
            id: item_id.to_string(),
            text: message.to_string(),
            phase: None,
            memory_citation: None,
        },
        completed_at_ms: 0,
    })
}

fn exec_approval_request(
    thread_id: ThreadId,
    turn_id: &str,
    item_id: &str,
    approval_id: Option<&str>,
) -> ServerRequest {
    ServerRequest::CommandExecutionRequestApproval {
        request_id: AppServerRequestId::Integer(1),
        params: CommandExecutionRequestApprovalParams {
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
            item_id: item_id.to_string(),
            started_at_ms: 0,
            approval_id: approval_id.map(str::to_string),
            environment_id: None,
            reason: Some("needs approval".to_string()),
            network_approval_context: None,
            command: Some("echo hello".to_string()),
            cwd: Some(test_path_buf("/tmp/project").abs().into()),
            command_actions: None,
            additional_permissions: None,
            proposed_execpolicy_amendment: None,
            proposed_network_policy_amendments: None,
            available_decisions: None,
        },
    }
}

fn request_user_input_request(thread_id: ThreadId, turn_id: &str, item_id: &str) -> ServerRequest {
    ServerRequest::ToolRequestUserInput {
        request_id: AppServerRequestId::Integer(2),
        params: ToolRequestUserInputParams {
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
            item_id: item_id.to_string(),
            questions: Vec::new(),
            auto_resolution_ms: None,
        },
    }
}

#[tokio::test]
async fn feedback_submission_without_thread_emits_error_history_cell() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;

    app.handle_feedback_submitted(
        /*origin_thread_id*/ None,
        FeedbackCategory::Bug,
        /*include_logs*/ true,
        Err("boom".to_string()),
    )
    .await;

    let cell = match app_event_rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected feedback error history cell, saw {other:?}"),
    };
    assert_eq!(
        lines_to_single_string(&cell.display_lines(/*width*/ 120)),
        "■ Failed to upload feedback: boom"
    );
}

#[tokio::test]
async fn feedback_submission_for_inactive_thread_replays_into_origin_thread() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let origin_thread_id = ThreadId::new();
    let active_thread_id = ThreadId::new();
    let origin_session = test_thread_session(origin_thread_id, test_path_buf("/tmp/origin"));
    let active_session = test_thread_session(active_thread_id, test_path_buf("/tmp/active"));
    app.thread_event_channels.insert(
        origin_thread_id,
        ThreadEventChannel::new_with_session(
            THREAD_EVENT_CHANNEL_CAPACITY,
            origin_session.clone(),
            Vec::new(),
        ),
    );
    app.thread_event_channels.insert(
        active_thread_id,
        ThreadEventChannel::new_with_session(
            THREAD_EVENT_CHANNEL_CAPACITY,
            active_session.clone(),
            Vec::new(),
        ),
    );
    app.activate_thread_channel(active_thread_id).await;
    app.chat_widget.handle_thread_session(active_session);
    while app_event_rx.try_recv().is_ok() {}

    app.handle_feedback_submitted(
        Some(origin_thread_id),
        FeedbackCategory::Bug,
        /*include_logs*/ true,
        Ok("uploaded-thread".to_string()),
    )
    .await;

    assert_matches!(
        app_event_rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    );

    let snapshot = {
        let channel = app
            .thread_event_channels
            .get(&origin_thread_id)
            .expect("origin thread channel should exist");
        let store = channel.store.lock().await;
        assert!(matches!(
            store.buffer.back(),
            Some(ThreadBufferedEvent::FeedbackSubmission(_))
        ));
        store.snapshot()
    };

    app.replay_thread_snapshot(snapshot, /*resume_restored_queue*/ false);

    let mut rendered_cells = Vec::new();
    while let Ok(event) = app_event_rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            rendered_cells.push(lines_to_single_string(&cell.display_lines(/*width*/ 120)));
        }
    }
    assert!(rendered_cells.iter().any(|cell| {
        cell.contains("• Feedback uploaded. Please open an issue using the following URL:")
            && cell.contains("uploaded-thread")
    }));
}

fn next_user_turn_op(op_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Op>) -> Op {
    let mut seen = Vec::new();
    while let Ok(op) = op_rx.try_recv() {
        if matches!(op, Op::UserTurn { .. }) {
            return op;
        }
        seen.push(format!("{op:?}"));
    }
    panic!("expected UserTurn op, saw: {seen:?}");
}

fn lines_to_single_string(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn test_session_telemetry(config: &Config, model: &str) -> SessionTelemetry {
    let model_info =
        construct_model_info_offline_for_tests(model, &config.to_models_manager_config());
    SessionTelemetry::new(
        ThreadId::new(),
        model,
        model_info.slug.as_str(),
        /*account_id*/ None,
        /*account_email*/ None,
        /*auth_mode*/ None,
        "test_originator".to_string(),
        /*log_user_prompts*/ false,
        "test".to_string(),
        crate::test_support::session_source_cli(),
    )
}

#[test]
fn active_turn_not_steerable_turn_error_extracts_structured_server_error() {
    let turn_error = AppServerTurnError {
        message: "cannot steer a review turn".to_string(),
        codex_error_info: Some(AppServerCodexErrorInfo::ActiveTurnNotSteerable {
            turn_kind: AppServerNonSteerableTurnKind::Review,
        }),
        additional_details: None,
    };
    let error = TypedRequestError::Server {
        method: "turn/steer".to_string(),
        source: JSONRPCErrorError {
            code: -32602,
            message: turn_error.message.clone(),
            data: Some(serde_json::to_value(&turn_error).expect("turn error should serialize")),
        },
    };

    assert_eq!(
        active_turn_not_steerable_turn_error(&error),
        Some(turn_error)
    );
}

#[test]
fn turn_start_capacity_classifier_uses_structured_error_data() {
    let error = TypedRequestError::Server {
        method: "turn/start".to_string(),
        source: JSONRPCErrorError {
            code: -32603,
            message: "wording may change".to_string(),
            data: Some(serde_json::json!({
                "turn_start_error": "execution_capacity",
                "max_threads": 7,
            })),
        },
    };
    let report = color_eyre::eyre::eyre!(error).wrap_err("turn/start failed in TUI");

    assert_eq!(
        crate::app_server_session::turn_start_execution_capacity(&report),
        Some(7)
    );
}

#[test]
fn turn_start_capacity_classifier_rejects_message_only_match() {
    let error = TypedRequestError::Server {
        method: "turn/start".to_string(),
        source: JSONRPCErrorError {
            code: -32603,
            message: "execution_capacity max_threads=7".to_string(),
            data: None,
        },
    };
    let report = color_eyre::eyre::eyre!(error).wrap_err("turn/start failed in TUI");

    assert_eq!(
        crate::app_server_session::turn_start_execution_capacity(&report),
        None
    );
}

#[test]
fn session_start_failure_surfaces_archived_guidance_without_rollout_path() {
    let thread_id =
        ThreadId::from_string("019e72f4-e09a-70f2-b2c2-a153a57b8cc0").expect("thread id");
    let target_session = SessionTarget {
        path: Some(std::path::PathBuf::from(
            "/Users/me/.codex/archived_sessions/rollout.jsonl",
        )),
        thread_id,
    };
    let expected = format!(
        "session {thread_id} is archived. Run `corbanu unarchive {thread_id}` to unarchive it first."
    );

    for (action, server_binary) in [("resume", "codex"), ("fork", "pfterminal")] {
        let server_message = expected.replace(
            "Run `corbanu unarchive ",
            &format!("Run `{server_binary} unarchive "),
        );
        let err = color_eyre::eyre::eyre!(
            "thread/{action} failed during TUI bootstrap: thread/{action} failed: {server_message} (code -32600)"
        );

        assert_fatal_exit(
            session_start_failure(action, &target_session, err),
            &expected,
        );
    }
}

fn assert_fatal_exit(result: Result<AppExitInfo>, expected: &str) {
    match result.expect("a refusal ends the launch with a plain message") {
        AppExitInfo {
            exit_reason: ExitReason::Fatal(message),
            thread_id: None,
            resume_hint: None,
            ..
        } => assert_eq!(message, expected),
        other => panic!("expected a fatal exit, got {other:?}"),
    }
}

#[test]
fn session_start_failure_turns_a_server_refusal_into_a_plain_message() {
    let thread_id =
        ThreadId::from_string("019e72f4-e09a-70f2-b2c2-a153a57b8cc0").expect("thread id");
    let target_session = SessionTarget {
        path: Some(std::path::PathBuf::from(
            "/Users/me/.corbanu/sessions/2026/10/13/rollout-01a13453.jsonl",
        )),
        thread_id,
    };
    let refusal = "failed to load configuration: the thread's recorded account zai:work: named accounts need the `named_accounts` feature (`--enable named_accounts`); `--account zai:default` (on a resume or fork) uses the default credentials instead";

    for action in ["resume", "fork"] {
        for suffix in ["", r#", data: {"reason":"cloudRequirements"}"#] {
            let odd = "a message mentioning (code 7) itself";
            let err = color_eyre::eyre::eyre!(
                "thread/{action} failed during TUI bootstrap: thread/{action} failed: {odd} (code -32600){suffix}"
            );
            assert_fatal_exit(
                session_start_failure(action, &target_session, err),
                &format!("Could not {action} session {thread_id}: {odd}"),
            );
            let err = color_eyre::eyre::eyre!(
                "thread/{action} failed during TUI bootstrap: thread/{action} failed: {refusal} (code -32600){suffix}"
            );
            assert_fatal_exit(
                session_start_failure(action, &target_session, err),
                &format!("Could not {action} session {thread_id}: {refusal}"),
            );
        }
    }
}

#[test]
fn session_start_failure_keeps_a_report_for_transport_errors() {
    let thread_id =
        ThreadId::from_string("019e72f4-e09a-70f2-b2c2-a153a57b8cc0").expect("thread id");
    let target_session = SessionTarget {
        path: None,
        thread_id,
    };
    let err = color_eyre::eyre::eyre!(
        "thread/resume failed during TUI bootstrap: thread/resume transport error: broken pipe"
    );

    let report = session_start_failure("resume", &target_session, err)
        .expect_err("a transport failure is not a refusal");
    assert_eq!(
        report.to_string(),
        format!(
            "Failed to resume session from thread {thread_id}: thread/resume failed during TUI bootstrap: thread/resume transport error: broken pipe"
        )
    );
}

#[test]
fn active_turn_steer_race_detects_missing_active_turn() {
    let error = TypedRequestError::Server {
        method: "turn/steer".to_string(),
        source: JSONRPCErrorError {
            code: -32602,
            message: "no active turn to steer".to_string(),
            data: None,
        },
    };

    assert_eq!(
        active_turn_steer_race(&error),
        Some(ActiveTurnSteerRace::Missing)
    );
    assert_eq!(active_turn_not_steerable_turn_error(&error), None);
}

#[test]
fn active_turn_steer_race_extracts_actual_turn_id_from_mismatch() {
    let error = TypedRequestError::Server {
        method: "turn/steer".to_string(),
        source: JSONRPCErrorError {
            code: -32602,
            message: "expected active turn id `turn-expected` but found `turn-actual`".to_string(),
            data: None,
        },
    };

    assert_eq!(
        active_turn_steer_race(&error),
        Some(ActiveTurnSteerRace::ExpectedTurnMismatch {
            actual_turn_id: "turn-actual".to_string(),
        })
    );
}

#[test]
fn active_turn_interrupt_race_extracts_actual_turn_id_from_mismatch() {
    let error = TypedRequestError::Server {
        method: "turn/interrupt".to_string(),
        source: JSONRPCErrorError {
            code: -32602,
            message: "expected active turn id turn-expected but found turn-actual".to_string(),
            data: None,
        },
    };

    assert_eq!(
        active_turn_interrupt_race(&error),
        Some("turn-actual".to_string())
    );
}

#[tokio::test]
async fn interrupt_failure_message_is_pane_local() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    app.primary_thread_id = Some(thread_id);

    app.note_thread_interrupt_failure(
        thread_id,
        TypedRequestError::Server {
            method: "turn/interrupt".to_string(),
            source: JSONRPCErrorError {
                code: -32602,
                message: "no active turn to interrupt".to_string(),
                data: None,
            },
        },
    );

    let cell = match app_event_rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected pane-local interrupt error, got {other:?}"),
    };
    let rendered = lines_to_single_string(&cell.display_lines(/*width*/ 120));
    assert!(rendered.contains("Failed to interrupt Main [default]"));
    assert!(rendered.contains("The pane remains open."));
}

#[tokio::test]
async fn unexpected_turn_steer_failure_is_pane_local_and_nonfatal() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    app.active_thread_id = Some(thread_id);
    let error = TypedRequestError::Server {
        method: "turn/steer".to_string(),
        source: JSONRPCErrorError {
            code: -32600,
            message: "direct app-server input is not allowed for multi-agent v2 sub-agents"
                .to_string(),
            data: None,
        },
    };

    app.note_thread_steer_failure(thread_id, error);

    let mut rendered = String::new();
    while let Ok(event) = app_event_rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            rendered.push_str(&lines_to_single_string(&cell.display_lines(/*width*/ 120)));
        }
    }
    assert!(rendered.contains("Failed to steer"));
    assert!(rendered.contains("direct app-server input is not allowed"));
    assert!(rendered.contains("pane remains open"));
}

#[tokio::test]
async fn fresh_session_config_uses_current_service_tier() {
    let mut app = make_test_app().await;
    app.chat_widget.set_service_tier(Some(
        codex_protocol::config_types::ServiceTier::Fast
            .request_value()
            .to_string(),
    ));

    let config = app.fresh_session_config();

    assert_eq!(
        config.service_tier,
        Some(
            codex_protocol::config_types::ServiceTier::Fast
                .request_value()
                .to_string()
        )
    );
}

#[tokio::test]
async fn backtrack_selection_preserves_selected_prompt_and_requests_branch() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;

    let user_cell = |text: &str,
                     text_elements: Vec<TextElement>,
                     local_image_paths: Vec<PathBuf>,
                     remote_image_urls: Vec<String>|
     -> Arc<dyn HistoryCell> {
        Arc::new(UserHistoryCell {
            message: text.to_string(),
            text_elements,
            local_image_paths,
            remote_image_urls,
        }) as Arc<dyn HistoryCell>
    };
    let agent_cell = |text: &str| -> Arc<dyn HistoryCell> {
        Arc::new(AgentMessageCell::new(
            vec![Line::from(text.to_string())],
            /*is_first_line*/ true,
        )) as Arc<dyn HistoryCell>
    };

    let make_header = |is_first| {
        let session = ThreadSessionState {
            thread_id: ThreadId::new(),
            forked_from_id: None,
            fork_parent_title: None,
            thread_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: AskForApproval::Never,
            approvals_reviewer: ApprovalsReviewer::User,
            permission_profile: PermissionProfile::read_only(),
            active_permission_profile: None,
            cwd: test_path_buf("/home/user/project").abs(),
            runtime_workspace_roots: Vec::new(),
            instruction_source_paths: Vec::new(),
            reasoning_effort: None,
            collaboration_mode: None,
            personality: None,
            message_history: None,
            network_proxy: None,
            rollout_path: Some(PathBuf::new()),
            provider_account: None,
        };
        Arc::new(new_session_info(
            app.chat_widget.config_ref(),
            app.chat_widget.current_model(),
            &session,
            is_first,
            /*tooltip_override*/ None,
            /*auth_plan*/ None,
            /*show_fast_status*/ false,
        )) as Arc<dyn HistoryCell>
    };

    let placeholder = "[Image #1]";
    let edited_text = format!("follow-up (edited) {placeholder}");
    let edited_range = edited_text.len().saturating_sub(placeholder.len())..edited_text.len();
    let edited_text_elements = vec![TextElement::new(
        edited_range.into(),
        /*placeholder*/ None,
    )];
    let edited_local_image_paths = vec![PathBuf::from("/tmp/fake-image.png")];

    // Simulate a transcript with duplicated history (e.g., from prior backtracks)
    // and an edited turn appended after a session header boundary.
    app.transcript_cells = vec![
        make_header(true),
        user_cell("first question", Vec::new(), Vec::new(), Vec::new()),
        agent_cell("answer first"),
        user_cell("follow-up", Vec::new(), Vec::new(), Vec::new()),
        agent_cell("answer follow-up"),
        make_header(false),
        user_cell("first question", Vec::new(), Vec::new(), Vec::new()),
        agent_cell("answer first"),
        user_cell(
            &edited_text,
            edited_text_elements.clone(),
            edited_local_image_paths.clone(),
            vec!["https://example.com/backtrack.png".to_string()],
        ),
        agent_cell("answer edited"),
    ];

    assert_eq!(user_count(&app.transcript_cells), 2);
    let transcript_before: Vec<String> = app
        .transcript_cells
        .iter()
        .map(|cell| lines_to_single_string(&cell.display_lines(/*width*/ 80)))
        .collect();

    let base_id = ThreadId::new();
    app.chat_widget
        .handle_thread_session(crate::session_state::ThreadSessionState {
            thread_id: base_id,
            forked_from_id: None,
            fork_parent_title: None,
            thread_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: AskForApproval::Never,
            approvals_reviewer: ApprovalsReviewer::User,
            permission_profile: PermissionProfile::read_only(),
            active_permission_profile: None,
            cwd: test_path_buf("/home/user/project").abs(),
            runtime_workspace_roots: Vec::new(),
            instruction_source_paths: Vec::new(),
            reasoning_effort: None,
            collaboration_mode: None,
            personality: None,
            message_history: None,
            network_proxy: None,
            rollout_path: Some(PathBuf::new()),
            provider_account: None,
        });

    app.backtrack.base_id = Some(base_id);
    app.backtrack.primed = true;
    app.backtrack.nth_user_message = user_count(&app.transcript_cells).saturating_sub(1);

    let selection = app
        .confirm_backtrack_from_main()
        .expect("backtrack selection");
    let expected = BacktrackSelection {
        thread_id: base_id,
        nth_user_message: 1,
        prompt: crate::chatwidget::UserMessage {
            text: edited_text,
            local_images: vec![crate::bottom_pane::LocalImageAttachment {
                placeholder: placeholder.to_string(),
                path: edited_local_image_paths[0].clone(),
            }],
            remote_image_urls: vec!["https://example.com/backtrack.png".to_string()],
            text_elements: edited_text_elements,
            mention_bindings: Vec::new(),
        },
    };
    assert_eq!(selection, expected);

    app.apply_backtrack_selection(selection);
    let event = std::iter::from_fn(|| app_event_rx.try_recv().ok())
        .find(|event| matches!(event, AppEvent::ForkSessionForPromptEdit { .. }))
        .expect("prompt edit fork should be requested");
    assert_matches!(
        event,
        AppEvent::ForkSessionForPromptEdit {
            thread_id,
            nth_user_message,
            prompt,
        } if thread_id == expected.thread_id
            && nth_user_message == expected.nth_user_message
            && prompt == expected.prompt
    );

    let transcript_after: Vec<String> = app
        .transcript_cells
        .iter()
        .map(|cell| lines_to_single_string(&cell.display_lines(/*width*/ 80)))
        .collect();
    assert_eq!(transcript_after, transcript_before);
}

#[tokio::test]
async fn backtrack_branch_failure_restores_selected_prompt_snapshot() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;

    app.restore_backtrack_prompt_after_branch_error(
        crate::chatwidget::UserMessage::from("edit this prompt"),
        "branch unavailable",
    );

    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "edit this prompt"
    );
    let cell = match app_event_rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected InsertHistoryCell event, got {other:?}"),
    };
    let rendered = lines_to_single_string(&cell.display_lines(/*width*/ 80));
    assert_app_snapshot!(
        "backtrack_branch_failure_restores_selected_prompt",
        rendered
    );
}

#[tokio::test]
async fn remote_resume_current_cwd_rejection_snapshot() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    std::fs::write(
        app.config.codex_home.join("config.toml"),
        "[tui]\nresume_cwd = \"current\"\n",
    )?;
    app.app_server_target = crate::AppServerTarget::Remote {
        endpoint: crate::RemoteAppServerEndpoint::WebSocket {
            websocket_url: "ws://127.0.0.1:4500".to_string(),
            auth_token: None,
        },
    };
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;

    let control = app
        .resume_target_session(
            &mut tui,
            &mut app_server,
            crate::resume_picker::SessionTarget {
                path: None,
                thread_id: ThreadId::new(),
            },
        )
        .await?;

    assert!(matches!(control, AppRunControl::Continue));
    let cell = match app_event_rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected InsertHistoryCell event, got {other:?}"),
    };
    let rendered = lines_to_single_string(&cell.display_lines(/*width*/ 80));
    assert_app_snapshot!("remote_resume_current_cwd_rejected", rendered);
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn remote_exec_resume_current_cwd_is_rejected() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    std::fs::write(
        app.config.codex_home.join("config.toml"),
        "[tui]\nresume_cwd = \"current\"\n",
    )?;
    app.environment_manager = Arc::new(
        EnvironmentManager::create_for_tests(
            Some("ws://127.0.0.1:8765".to_string()),
            Some(codex_exec_server::ExecServerRuntimePaths::new(
                std::env::current_exe()?,
                /*codex_linux_sandbox_exe*/ None,
            )?),
        )
        .await,
    );
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;

    let control = app
        .resume_target_session(
            &mut tui,
            &mut app_server,
            crate::resume_picker::SessionTarget {
                path: None,
                thread_id: ThreadId::new(),
            },
        )
        .await?;

    assert!(matches!(control, AppRunControl::Continue));
    let cell = match app_event_rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected InsertHistoryCell event, got {other:?}"),
    };
    let rendered = lines_to_single_string(&cell.display_lines(/*width*/ 80));
    assert_eq!(
        rendered,
        "■ `tui.resume_cwd = \"current\"` requires `--cd` when using a remote workspace"
    );
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn in_app_resume_session_cwd_without_metadata_is_non_fatal() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    std::fs::write(
        app.config.codex_home.join("config.toml"),
        "[tui]\nresume_cwd = \"session\"\n",
    )?;
    app.state_db = None;
    let active_thread_id = app.chat_widget.thread_id();
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;

    let control = app
        .resume_target_session(
            &mut tui,
            &mut app_server,
            crate::resume_picker::SessionTarget {
                path: None,
                thread_id: ThreadId::new(),
            },
        )
        .await?;

    assert!(matches!(control, AppRunControl::Continue));
    assert_eq!(app.chat_widget.thread_id(), active_thread_id);
    let cell = match app_event_rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected InsertHistoryCell event, got {other:?}"),
    };
    let rendered = lines_to_single_string(&cell.display_lines(/*width*/ 100));
    assert_app_snapshot!("in_app_resume_session_cwd_without_metadata", rendered);
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn remote_resume_keeps_server_only_cwd_out_of_local_config() -> Result<()> {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let local_cwd = app.config.cwd.to_path_buf();
    let local_workspace_roots = app
        .rebuild_config_for_cwd(local_cwd.clone())
        .await?
        .workspace_roots;
    let remote_cwd = if cfg!(windows) {
        PathBuf::from("/srv/remote/project")
    } else {
        PathBuf::from(r"C:\remote\project")
    };
    let filename_timestamp = "2025-01-05T12-00-00";
    let thread_id = app_test_support::create_fake_rollout(
        app.config.codex_home.as_path(),
        filename_timestamp,
        "2025-01-05T12:00:00Z",
        "Saved user message",
        Some(&app.config.model_provider_id),
        /*git_info*/ None,
    )
    .expect("materialized rollout should be created");
    let rollout_path = app_test_support::rollout_path(
        app.config.codex_home.as_path(),
        filename_timestamp,
        &thread_id,
    );
    app.app_server_target = crate::AppServerTarget::Remote {
        endpoint: crate::RemoteAppServerEndpoint::WebSocket {
            websocket_url: "ws://127.0.0.1:4500".to_string(),
            auth_token: None,
        },
    };
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config))
        .await?
        .with_remote_cwd_override(Some(remote_cwd.clone()));
    let mut tui = crate::tui::test_support::make_test_tui()?;

    let control = app
        .resume_target_session(
            &mut tui,
            &mut app_server,
            crate::resume_picker::SessionTarget {
                path: Some(rollout_path),
                thread_id: ThreadId::from_string(&thread_id)?,
            },
        )
        .await?;

    assert!(matches!(control, AppRunControl::Continue));
    assert_eq!(app_server.remote_cwd_override(), Some(remote_cwd.as_path()));
    assert!(!crate::session_resume::cwds_differ(
        app.config.cwd.as_path(),
        &local_cwd,
    ));
    assert_eq!(app.config.workspace_roots, local_workspace_roots);
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn in_app_resume_uses_configured_or_explicit_cwd() -> Result<()> {
    for (configured_mode, has_explicit_cwd, has_remote_exec, expected_directory) in [
        ("current", false, false, "launch"),
        ("session", false, false, "session"),
        ("session", true, false, "explicit"),
        ("session", false, true, "session"),
        ("session", true, true, "explicit"),
    ] {
        let temp_dir = tempdir()?;
        let codex_home = temp_dir.path().join("codex-home");
        let launch_cwd = temp_dir.path().join("launch");
        let active_cwd = temp_dir.path().join("active");
        let session_cwd = temp_dir.path().join("session");
        let explicit_cwd = temp_dir.path().join("explicit");
        std::fs::create_dir_all(&codex_home)?;
        std::fs::create_dir_all(&launch_cwd)?;
        std::fs::create_dir_all(&active_cwd)?;
        std::fs::create_dir_all(&session_cwd)?;
        std::fs::create_dir_all(&explicit_cwd)?;
        std::fs::write(
            codex_home.join("config.toml"),
            format!("[tui]\nresume_cwd = \"{configured_mode}\"\n"),
        )?;
        let config = ConfigBuilder::default()
            .codex_home(codex_home.clone())
            .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
            .harness_overrides(ConfigOverrides {
                cwd: Some(active_cwd.clone()),
                ..Default::default()
            })
            .build()
            .await?;
        let filename_timestamp = "2025-01-05T12-00-00";
        let thread_id = app_test_support::create_fake_rollout(
            &codex_home,
            filename_timestamp,
            "2025-01-05T12:00:00Z",
            "Saved user message",
            Some(&config.model_provider_id),
            /*git_info*/ None,
        )
        .expect("materialized rollout should be created");
        let rollout_path =
            app_test_support::rollout_path(&codex_home, filename_timestamp, &thread_id);
        let mut rollout_lines = std::fs::read_to_string(&rollout_path)?
            .lines()
            .map(serde_json::from_str::<serde_json::Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rollout_lines[0]["payload"]["cwd"] = serde_json::to_value(&session_cwd)?;
        std::fs::write(
            &rollout_path,
            format!(
                "{}\n",
                rollout_lines
                    .into_iter()
                    .map(|line| line.to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )?;
        let thread_id = ThreadId::from_string(&thread_id)?;
        let state_db =
            crate::init_state_db_for_app_server_target(&config, &crate::AppServerTarget::Embedded)
                .await?;
        let environment_manager = if has_remote_exec {
            Arc::new(
                EnvironmentManager::create_for_tests(
                    Some("ws://127.0.0.1:8765".to_string()),
                    Some(codex_exec_server::ExecServerRuntimePaths::new(
                        std::env::current_exe()?,
                        /*codex_linux_sandbox_exe*/ None,
                    )?),
                )
                .await,
            )
        } else {
            Arc::new(EnvironmentManager::default_for_tests())
        };
        let mut app_server = crate::start_app_server_for_picker(
            &config,
            &crate::AppServerTarget::Embedded,
            state_db.clone(),
            Arc::clone(&environment_manager),
        )
        .await?;
        let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
        app.config = config;
        app.launch_cwd = launch_cwd;
        app.state_db = state_db;
        app.environment_manager = environment_manager;
        app.harness_overrides.cwd = has_explicit_cwd.then_some(explicit_cwd);
        app.chat_widget
            .handle_thread_session_quiet(test_thread_session(ThreadId::new(), active_cwd));
        let mut tui = crate::tui::test_support::make_test_tui()?;

        let control = app
            .resume_target_session(
                &mut tui,
                &mut app_server,
                crate::resume_picker::SessionTarget {
                    path: Some(rollout_path),
                    thread_id,
                },
            )
            .await?;

        assert!(matches!(control, AppRunControl::Continue));
        let expected_cwd = temp_dir.path().join(expected_directory);
        assert!(!crate::session_resume::cwds_differ(
            app.config.cwd.as_path(),
            &expected_cwd,
        ));
        assert!(!crate::session_resume::cwds_differ(
            app.chat_widget.config_ref().cwd.as_path(),
            &expected_cwd,
        ));
        assert_eq!(app.chat_widget.thread_id(), Some(thread_id));

        let control = Box::pin(app.handle_event(
            &mut tui,
            &mut app_server,
            AppEvent::ForkCurrentSession { name: None },
        ))
        .await?;

        assert!(matches!(control, AppRunControl::Continue));
        assert!(!crate::session_resume::cwds_differ(
            app.chat_widget.config_ref().cwd.as_path(),
            &expected_cwd,
        ));
        assert_ne!(app.chat_widget.thread_id(), Some(thread_id));
        app_server.shutdown().await?;
    }

    Ok(())
}

#[tokio::test]
async fn remembered_current_cwd_stays_at_launch_across_in_app_resumes() -> Result<()> {
    let temp_dir = tempdir()?;
    let codex_home = temp_dir.path().join("codex-home");
    let launch_cwd = temp_dir.path().join("launch");
    let active_cwd = temp_dir.path().join("active");
    let first_session_cwd = temp_dir.path().join("first-session");
    let second_session_cwd = temp_dir.path().join("second-session");
    std::fs::create_dir_all(&codex_home)?;
    std::fs::create_dir_all(&launch_cwd)?;
    std::fs::create_dir_all(&active_cwd)?;
    std::fs::create_dir_all(&first_session_cwd)?;
    std::fs::create_dir_all(&second_session_cwd)?;
    let config = ConfigBuilder::default()
        .codex_home(codex_home.clone())
        .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
        .harness_overrides(ConfigOverrides {
            cwd: Some(active_cwd.clone()),
            ..Default::default()
        })
        .build()
        .await?;
    let immediately_selected_cwd = crate::cwd_prompt::CwdSelection::CurrentAndRemember
        .selected_cwd(&active_cwd, &first_session_cwd, &launch_cwd);
    assert!(!crate::session_resume::cwds_differ(
        immediately_selected_cwd,
        &launch_cwd,
    ));
    crate::legacy_core::config::edit::ConfigEditsBuilder::for_config(&config)
        .set_resume_cwd(codex_config::types::ResumeCwdMode::Current)
        .apply()
        .await
        .map_err(std::io::Error::other)?;

    let mut targets = Vec::new();
    for (filename_timestamp, metadata_timestamp, session_cwd) in [
        (
            "2025-01-05T12-00-00",
            "2025-01-05T12:00:00Z",
            first_session_cwd,
        ),
        (
            "2025-01-05T12-01-00",
            "2025-01-05T12:01:00Z",
            second_session_cwd,
        ),
    ] {
        let thread_id = app_test_support::create_fake_rollout(
            &codex_home,
            filename_timestamp,
            metadata_timestamp,
            "Saved user message",
            Some(&config.model_provider_id),
            /*git_info*/ None,
        )
        .expect("materialized rollout should be created");
        let rollout_path =
            app_test_support::rollout_path(&codex_home, filename_timestamp, &thread_id);
        let mut rollout_lines = std::fs::read_to_string(&rollout_path)?
            .lines()
            .map(serde_json::from_str::<serde_json::Value>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rollout_lines[0]["payload"]["cwd"] = serde_json::to_value(&session_cwd)?;
        std::fs::write(
            &rollout_path,
            format!(
                "{}\n",
                rollout_lines
                    .into_iter()
                    .map(|line| line.to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        )?;
        targets.push(crate::resume_picker::SessionTarget {
            path: Some(rollout_path),
            thread_id: ThreadId::from_string(&thread_id)?,
        });
    }
    let state_db =
        crate::init_state_db_for_app_server_target(&config, &crate::AppServerTarget::Embedded)
            .await?;
    let mut app_server = crate::start_app_server_for_picker(
        &config,
        &crate::AppServerTarget::Embedded,
        state_db.clone(),
        Arc::new(EnvironmentManager::default_for_tests()),
    )
    .await?;
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;
    app.config = config;
    app.launch_cwd = launch_cwd.clone();
    app.state_db = state_db;
    app.chat_widget
        .handle_thread_session_quiet(test_thread_session(ThreadId::new(), active_cwd));
    let mut tui = crate::tui::test_support::make_test_tui()?;

    for target_session in targets {
        let control = app
            .resume_target_session(&mut tui, &mut app_server, target_session)
            .await?;

        assert!(matches!(control, AppRunControl::Continue));
        assert!(!crate::session_resume::cwds_differ(
            app.config.cwd.as_path(),
            &launch_cwd,
        ));
        assert!(!crate::session_resume::cwds_differ(
            app.chat_widget.config_ref().cwd.as_path(),
            &launch_cwd,
        ));
    }
    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn prompt_edit_forks_before_selected_prompt_and_preserves_source() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let config = app.chat_widget.config_ref().clone();
    let filename_ts = "2025-01-05T12-00-00";
    let source_thread_id = app_test_support::create_fake_rollout(
        config.codex_home.as_path(),
        filename_ts,
        "2025-01-05T12:00:00Z",
        "unused preview",
        Some("test-provider"),
        /*git_info*/ None,
    )
    .expect("materialized rollout should be created");
    let source_path =
        app_test_support::rollout_path(config.codex_home.as_path(), filename_ts, &source_thread_id);
    let session_meta = std::fs::read_to_string(&source_path)?
        .lines()
        .next()
        .expect("fake rollout should have session metadata")
        .to_string();
    std::fs::write(&source_path, format!("{session_meta}\n"))?;
    for (turn_id, message, images, local_images) in [
        ("turn-1", "retained prompt", None, Vec::new()),
        (
            "turn-2",
            "selected prompt [Image #1]",
            Some(vec!["https://example.com/backtrack.png".to_string()]),
            vec![PathBuf::from("/tmp/fake-image.png")],
        ),
    ] {
        for item in [
            RolloutItem::EventMsg(EventMsg::TurnStarted(TurnStartedEvent {
                turn_id: turn_id.to_string(),
                trace_id: None,
                started_at: None,
                model_context_window: None,
                collaboration_mode_kind: ModeKind::default(),
            })),
            RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
                message: message.to_string(),
                images,
                local_images,
                ..Default::default()
            })),
            RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
                turn_id: turn_id.to_string(),
                last_agent_message: None,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
                time_to_first_token_ms: None,
            })),
        ] {
            codex_rollout::append_rollout_item_to_path(&source_path, &item).await?;
        }
    }

    let source_thread_id = ThreadId::from_string(&source_thread_id)?;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&config)).await?;
    let started = app_server
        .resume_thread(
            config.clone(),
            source_thread_id,
            crate::app_server_session::ResumeModelSettings::OverrideFromCurrentConfig,
            crate::app_server_session::ResumePermissionSettings::OverrideFromCurrentConfig,
        )
        .await?;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    while app_event_rx.try_recv().is_ok() {}
    let source_before = std::fs::read_to_string(&source_path)?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let prompt = crate::chatwidget::UserMessage {
        text: "selected prompt [Image #1]".to_string(),
        local_images: vec![crate::bottom_pane::LocalImageAttachment {
            placeholder: "[Image #1]".to_string(),
            path: PathBuf::from("/tmp/fake-image.png"),
        }],
        remote_image_urls: vec!["https://example.com/backtrack.png".to_string()],
        text_elements: Vec::new(),
        mention_bindings: Vec::new(),
    };

    let control = Box::pin(app.handle_event(
        &mut tui,
        &mut app_server,
        AppEvent::ForkSessionForPromptEdit {
            thread_id: source_thread_id,
            nth_user_message: 1,
            prompt: prompt.clone(),
        },
    ))
    .await?;

    assert!(matches!(control, AppRunControl::Continue));
    let forked_thread_id = app
        .chat_widget
        .thread_id()
        .expect("prompt edit should switch to a forked thread");
    assert_ne!(forked_thread_id, source_thread_id);
    assert_eq!(app.chat_widget.composer_text_with_pending(), prompt.text);
    assert_eq!(
        app.chat_widget.remote_image_urls(),
        prompt.remote_image_urls
    );
    assert_eq!(std::fs::read_to_string(&source_path)?, source_before);
    assert_eq!(
        app_server
            .thread_read(source_thread_id, /*include_turns*/ true)
            .await?
            .turns
            .iter()
            .map(|turn| turn.id.as_str())
            .collect::<Vec<_>>(),
        vec!["turn-1", "turn-2"]
    );
    assert_eq!(
        app_server
            .thread_read(forked_thread_id, /*include_turns*/ true)
            .await?
            .turns
            .iter()
            .map(|turn| turn.id.as_str())
            .collect::<Vec<_>>(),
        vec!["turn-1"]
    );

    let history = std::iter::from_fn(|| app_event_rx.try_recv().ok())
        .filter_map(|event| match event {
            AppEvent::InsertHistoryCell(cell) => {
                Some(lines_to_single_string(&cell.display_lines(/*width*/ 120)))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let retained_index = history
        .iter()
        .position(|line| line.contains("retained prompt"))
        .expect("forked history should replay the retained prompt");
    let notice_index = history
        .iter()
        .position(|line| line == "• You’re continuing from this point in a new conversation")
        .expect("prompt edit should emit the branch notice");
    assert!(retained_index < notice_index);
    assert!(
        !history
            .iter()
            .any(|line| line.contains("Thread forked from"))
    );
    app_server.shutdown().await?;

    Ok(())
}

#[tokio::test]
async fn prompt_edit_before_first_prompt_starts_fresh_thread() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let config = app.chat_widget.config_ref().clone();
    let source_thread_id = app_test_support::create_fake_rollout(
        config.codex_home.as_path(),
        "2025-01-05T12-00-00",
        "2025-01-05T12:00:00Z",
        "first prompt",
        Some("test-provider"),
        /*git_info*/ None,
    )
    .expect("materialized rollout should be created");
    let source_thread_id = ThreadId::from_string(&source_thread_id)?;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&config)).await?;
    let started = app_server
        .resume_thread(
            config.clone(),
            source_thread_id,
            crate::app_server_session::ResumeModelSettings::OverrideFromCurrentConfig,
            crate::app_server_session::ResumePermissionSettings::OverrideFromCurrentConfig,
        )
        .await?;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    while app_event_rx.try_recv().is_ok() {}
    let mut tui = crate::tui::test_support::make_test_tui()?;

    let control = Box::pin(app.handle_event(
        &mut tui,
        &mut app_server,
        AppEvent::ForkSessionForPromptEdit {
            thread_id: source_thread_id,
            nth_user_message: 0,
            prompt: crate::chatwidget::UserMessage::from("first prompt"),
        },
    ))
    .await?;

    assert!(matches!(control, AppRunControl::Continue));
    let fresh_thread_id = app
        .chat_widget
        .thread_id()
        .expect("first prompt edit should start a fresh thread");
    assert_ne!(fresh_thread_id, source_thread_id);
    assert_eq!(app.chat_widget.composer_text_with_pending(), "first prompt");
    let history = std::iter::from_fn(|| app_event_rx.try_recv().ok())
        .filter_map(|event| match event {
            AppEvent::InsertHistoryCell(cell) => {
                Some(lines_to_single_string(&cell.display_lines(/*width*/ 120)))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        history.iter().any(|line| {
            line == "• You’re continuing from this point in a new conversation"
        })
    );
    assert!(
        !history
            .iter()
            .any(|line| line.contains("Thread forked from"))
    );
    app_server.shutdown().await?;

    Ok(())
}

#[tokio::test]
async fn replay_thread_snapshot_replays_turn_history_in_order() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    app.replay_thread_snapshot(
        ThreadEventSnapshot {
            session: Some(test_thread_session(
                thread_id,
                test_path_buf("/home/user/project"),
            )),
            turns: vec![
                Turn {
                    id: "turn-1".to_string(),
                    items_view: codex_app_server_protocol::TurnItemsView::Full,
                    items: vec![ThreadItem::UserMessage {
                        id: "user-1".to_string(),
                        client_id: None,
                        content: vec![AppServerUserInput::Text {
                            text: "first prompt".to_string(),
                            text_elements: Vec::new(),
                        }],
                    }],
                    status: TurnStatus::Completed,
                    error: None,
                    started_at: None,
                    completed_at: None,
                    duration_ms: None,
                },
                Turn {
                    id: "turn-2".to_string(),
                    items_view: codex_app_server_protocol::TurnItemsView::Full,
                    items: vec![
                        ThreadItem::UserMessage {
                            id: "user-2".to_string(),
                            client_id: None,
                            content: vec![AppServerUserInput::Text {
                                text: "third prompt".to_string(),
                                text_elements: Vec::new(),
                            }],
                        },
                        ThreadItem::AgentMessage {
                            id: "assistant-2".to_string(),
                            text: "done".to_string(),
                            phase: None,
                            memory_citation: None,
                        },
                    ],
                    status: TurnStatus::Completed,
                    error: None,
                    started_at: None,
                    completed_at: None,
                    duration_ms: None,
                },
            ],
            events: Vec::new(),
            input_state: None,
        },
        /*resume_restored_queue*/ false,
    );

    while let Ok(event) = app_event_rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            let cell: Arc<dyn HistoryCell> = cell.into();
            app.transcript_cells.push(cell);
        }
    }

    let user_messages: Vec<String> = app
        .transcript_cells
        .iter()
        .filter_map(|cell| {
            cell.as_any()
                .downcast_ref::<UserHistoryCell>()
                .map(|cell| cell.message.clone())
        })
        .collect();
    assert_eq!(
        user_messages,
        vec!["first prompt".to_string(), "third prompt".to_string()]
    );
}

#[tokio::test]
async fn replace_chat_widget_reseeds_collab_agent_metadata_for_replay() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let receiver_thread_id =
        ThreadId::from_string("019cff70-2599-75e2-af72-b958ce5dc1cc").expect("valid thread");
    app.agent_navigation.upsert(
        receiver_thread_id,
        Some("Robie".to_string()),
        Some("explorer".to_string()),
        /*is_closed*/ false,
    );

    let replacement = ChatWidget::new_with_app_event(ChatWidgetInit {
        config: app.config.clone(),
        frame_requester: crate::tui::FrameRequester::test_dummy(),
        app_event_tx: app.app_event_tx.clone(),
        workspace_command_runner: None,
        initial_user_message: None,
        enhanced_keys_supported: app.enhanced_keys_supported,
        has_chatgpt_account: app.chat_widget.has_chatgpt_account(),
        has_codex_backend_auth: app.chat_widget.has_codex_backend_auth(),
        model_catalog: app.model_catalog.clone(),
        feedback: app.feedback.clone(),
        is_first_run: false,
        status_account_display: app.chat_widget.status_account_display().cloned(),
        runtime_model_provider_base_url: app
            .chat_widget
            .runtime_model_provider_base_url()
            .map(str::to_string),
        initial_plan_type: app.chat_widget.current_plan_type(),
        model: Some(app.chat_widget.current_model().to_string()),
        startup_tooltip_override: None,
        status_line_invalid_items_warned: app.status_line_invalid_items_warned.clone(),
        terminal_title_invalid_items_warned: app.terminal_title_invalid_items_warned.clone(),
        session_telemetry: app.session_telemetry.clone(),
    });
    app.replace_chat_widget(replacement);

    app.replay_thread_snapshot(
        ThreadEventSnapshot {
            session: None,
            turns: Vec::new(),
            events: vec![ThreadBufferedEvent::Notification(Box::new(
                ServerNotification::ItemStarted(
                    codex_app_server_protocol::ItemStartedNotification {
                        thread_id: "thread-1".to_string(),
                        turn_id: "turn-1".to_string(),
                        started_at_ms: 0,
                        item: ThreadItem::CollabAgentToolCall {
                            id: "wait-1".to_string(),
                            tool: codex_app_server_protocol::CollabAgentTool::Wait,
                            status:
                                codex_app_server_protocol::CollabAgentToolCallStatus::InProgress,
                            sender_thread_id: ThreadId::new().to_string(),
                            receiver_thread_ids: vec![receiver_thread_id.to_string()],
                            prompt: None,
                            model: None,
                            reasoning_effort: None,
                            provider_account: None,
                            agents_states: HashMap::new(),
                        },
                    },
                ),
            ))],
            input_state: None,
        },
        /*resume_restored_queue*/ false,
    );

    let mut saw_named_wait = false;
    while let Ok(event) = app_event_rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            let transcript = lines_to_single_string(&cell.transcript_lines(/*width*/ 80));
            saw_named_wait |= transcript.contains("Robie [explorer]");
        }
    }

    assert!(
        saw_named_wait,
        "expected replayed wait item to keep agent name"
    );
}

#[tokio::test]
async fn refreshed_snapshot_session_persists_resumed_turns() {
    let mut app = make_test_app().await;
    let thread_id = ThreadId::new();
    let initial_session = test_thread_session(thread_id, test_path_buf("/tmp/original"));
    app.thread_event_channels.insert(
        thread_id,
        ThreadEventChannel::new_with_session(
            /*capacity*/ 4,
            initial_session.clone(),
            Vec::new(),
        ),
    );

    let resumed_turns = vec![test_turn(
        "turn-1",
        TurnStatus::Completed,
        vec![ThreadItem::UserMessage {
            id: "user-1".to_string(),
            client_id: None,
            content: vec![AppServerUserInput::Text {
                text: "restored prompt".to_string(),
                text_elements: Vec::new(),
            }],
        }],
    )];
    let resumed_session = ThreadSessionState {
        cwd: test_path_buf("/tmp/refreshed").abs(),
        runtime_workspace_roots: Vec::new(),
        instruction_source_paths: Vec::new(),
        ..initial_session.clone()
    };
    let mut snapshot = ThreadEventSnapshot {
        session: Some(initial_session),
        turns: Vec::new(),
        events: Vec::new(),
        input_state: None,
    };

    app.apply_refreshed_snapshot_thread(
        thread_id,
        AppServerStartedThread {
            session: resumed_session.clone(),
            turns: resumed_turns.clone(),
            blocks_direct_input: true,
        },
        &mut snapshot,
    )
    .await;

    assert!(app.agent_navigation.is_parent_owned(thread_id));
    assert_eq!(snapshot.session, Some(resumed_session.clone()));
    assert_eq!(snapshot.turns, resumed_turns);

    let store = app
        .thread_event_channels
        .get(&thread_id)
        .expect("thread channel")
        .store
        .lock()
        .await;
    let store_snapshot = store.snapshot();
    assert_eq!(store_snapshot.session, Some(resumed_session));
    assert_eq!(store_snapshot.turns, snapshot.turns);
}

#[tokio::test]
async fn late_usage_result_can_follow_finalized_plan() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    app.chat_widget
        .add_token_activity_output(crate::chatwidget::TokenActivityView::Daily);
    let request_id = match app_event_rx.try_recv() {
        Ok(AppEvent::RefreshTokenActivity { request_id }) => request_id,
        other => panic!("expected token activity refresh request, got {other:?}"),
    };

    app.chat_widget.note_stream_consolidation_queued();
    app.transcript_cells
        .push(Arc::new(history_cell::new_proposed_plan_stream(
            vec![Line::from("finalized plan")],
            /*is_stream_continuation*/ false,
        )));
    app.chat_widget.note_stream_consolidation_completed();

    assert!(
        app.chat_widget.finish_token_activity_refresh(
            request_id,
            Err("token activity unavailable".to_string()),
        )
    );
    assert!(!app.pending_usage_output_insertion_blocked());
    assert!(
        app.chat_widget
            .take_completed_token_activity_output()
            .is_some()
    );
}

#[tokio::test]
async fn new_session_requests_shutdown_for_previous_conversation() {
    Box::pin(async {
        let (mut app, mut app_event_rx, mut op_rx) = Box::pin(make_test_app_with_channels()).await;

        let thread_id = ThreadId::new();
        let event = crate::session_state::ThreadSessionState {
            thread_id,
            forked_from_id: None,
            fork_parent_title: None,
            thread_name: None,
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: AskForApproval::Never,
            approvals_reviewer: ApprovalsReviewer::User,
            permission_profile: PermissionProfile::read_only(),
            active_permission_profile: None,
            cwd: test_path_buf("/home/user/project").abs(),
            runtime_workspace_roots: Vec::new(),
            instruction_source_paths: Vec::new(),
            reasoning_effort: None,
            collaboration_mode: None,
            personality: None,
            message_history: None,
            network_proxy: None,
            rollout_path: Some(PathBuf::new()),
            provider_account: None,
        };

        app.chat_widget.handle_thread_session(event);

        while app_event_rx.try_recv().is_ok() {}
        while op_rx.try_recv().is_ok() {}

        let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(
            app.chat_widget.config_ref(),
        ))
        .await
        .expect("embedded app server");
        Box::pin(app.shutdown_current_thread(&mut app_server)).await;

        assert!(
            op_rx.try_recv().is_err(),
            "shutdown should not submit Op::Shutdown"
        );
    })
    .await;
}

#[tokio::test]
async fn shutdown_first_exit_returns_immediate_exit_when_shutdown_submit_fails() {
    let mut app = make_test_app().await;
    let thread_id = ThreadId::new();
    app.active_thread_id = Some(thread_id);

    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(
        app.chat_widget.config_ref(),
    ))
    .await
    .expect("embedded app server");
    let control = Box::pin(app.handle_exit_mode(&mut app_server, ExitMode::ShutdownFirst)).await;

    assert_eq!(app.pending_shutdown_exit_thread_id, None);
    assert!(matches!(
        control,
        AppRunControl::Exit(ExitReason::UserRequested)
    ));
}

#[tokio::test]
async fn shutdown_first_exit_uses_app_server_shutdown_without_submitting_op() {
    let (mut app, _app_event_rx, mut op_rx) = Box::pin(make_test_app_with_channels()).await;
    let thread_id = ThreadId::new();
    app.active_thread_id = Some(thread_id);

    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(
        app.chat_widget.config_ref(),
    ))
    .await
    .expect("embedded app server");
    let control = Box::pin(app.handle_exit_mode(&mut app_server, ExitMode::ShutdownFirst)).await;

    assert_eq!(app.pending_shutdown_exit_thread_id, None);
    assert!(matches!(
        control,
        AppRunControl::Exit(ExitReason::UserRequested)
    ));
    assert!(
        op_rx.try_recv().is_err(),
        "shutdown should not submit Op::Shutdown"
    );
}

#[tokio::test]
async fn interrupt_without_active_turn_is_treated_as_handled() {
    Box::pin(async {
        let mut app = make_test_app().await;
        let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(
            app.chat_widget.config_ref(),
        ))
        .await
        .expect("embedded app server");
        let started = app_server
            .start_thread(app.chat_widget.config_ref())
            .await
            .expect("thread/start should succeed");
        let thread_id = started.session.thread_id;
        app.pending_permission_confirmation = Some(test_support::pending_confirmation(thread_id));
        app.enqueue_primary_thread_session(started.session, started.turns)
            .await
            .expect("primary thread should be registered");
        app.backtrack.primed = true;
        let op = AppCommand::interrupt();

        let handled = Box::pin(app.try_submit_active_thread_op_via_app_server(
            &mut app_server,
            thread_id,
            &op,
        ))
        .await
        .expect("interrupt submission should not fail");

        assert_eq!(handled, true);
        assert!(!app.backtrack.primed);
        assert!(app.pending_permission_confirmation.is_some());
    })
    .await;
}

#[tokio::test]
async fn override_turn_context_sends_thread_settings_update() {
    Box::pin(async {
        let mut app = make_test_app().await;
        // The update below selects gpt-5.4. Exact model/provider validation rejects
        // that model on the default Ambient provider, so run the thread on OpenAI.
        let openai_provider = app
            .chat_widget
            .config_ref()
            .model_providers
            .get(OPENAI_PROVIDER_ID)
            .cloned()
            .expect("OpenAI provider");
        app.chat_widget
            .set_model_provider(OPENAI_PROVIDER_ID.to_string(), openai_provider);
        app.chat_widget.set_model("gpt-5.5");
        let mut app_server =
            crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
                .await
                .expect("embedded app server");
        let started = app_server
            .start_thread(app.chat_widget.config_ref())
            .await
            .expect("thread/start should succeed");
        let thread_id = started.session.thread_id;
        let initial_model = started.session.model.clone();
        let initial_effort = started.session.reasoning_effort.clone();
        app.enqueue_primary_thread_session(started.session, started.turns)
            .await
            .expect("primary thread should be registered");
        let service_tier = ServiceTier::Fast.request_value().to_string();
        let collaboration_mode = CollaborationMode {
            mode: ModeKind::Plan,
            settings: Settings {
                model: "gpt-5.4".to_string(),
                reasoning_effort: Some(ReasoningEffortConfig::High),
                developer_instructions: None,
            },
        };
        let op = AppCommand::override_turn_context(
            /*cwd*/ None,
            Some(AskForApproval::OnRequest),
            Some(ApprovalsReviewer::AutoReview),
            /*permission_profile*/ None,
            Some(ActivePermissionProfile::new(
                codex_protocol::models::BUILT_IN_PERMISSION_PROFILE_WORKSPACE,
            )),
            /*windows_sandbox_level*/ None,
            Some("gpt-5.4".to_string()),
            Some(Some(ReasoningEffortConfig::High)),
            /*summary*/ None,
            Some(Some(service_tier.clone())),
            Some(collaboration_mode.clone()),
            Some(Personality::Pragmatic),
        );

        let handled = app
            .try_submit_active_thread_op_via_app_server(&mut app_server, thread_id, &op)
            .await
            .expect("settings update submission should not fail");

        assert_eq!(handled, true);
        assert_eq!(
            app.primary_session_configured
                .as_ref()
                .expect("primary session")
                .model,
            initial_model,
            "thread/settings/update response is only an ack; cached state changes on notification"
        );

        let notification = next_thread_settings_updated(&mut app_server, thread_id).await;
        assert_eq!(notification.thread_settings.model, "gpt-5.4");
        assert_eq!(
            notification.thread_settings.effort,
            Some(ReasoningEffortConfig::High)
        );
        assert_eq!(
            notification.thread_settings.service_tier,
            Some(service_tier.clone())
        );
        assert_eq!(
            notification.thread_settings.approval_policy,
            AskForApproval::OnRequest
        );
        assert_eq!(
            notification.thread_settings.approvals_reviewer.to_core(),
            ApprovalsReviewer::AutoReview
        );
        let notified_mode = &notification.thread_settings.collaboration_mode;
        assert_eq!(notified_mode.mode, collaboration_mode.mode);
        assert_eq!(
            notified_mode.settings.model,
            collaboration_mode.settings.model
        );
        assert_eq!(
            notified_mode.settings.reasoning_effort,
            collaboration_mode.settings.reasoning_effort
        );
        assert_eq!(
            notification.thread_settings.personality,
            Some(Personality::Pragmatic)
        );

        app.handle_app_server_event(
            &app_server,
            codex_app_server_client::AppServerEvent::ServerNotification(Box::new(
                ServerNotification::ThreadSettingsUpdated(notification),
            )),
        )
        .await;
        let updated_session = app
            .primary_session_configured
            .as_ref()
            .expect("primary session should be updated from notification");
        assert_eq!(updated_session.model, initial_model);
        assert_eq!(updated_session.reasoning_effort, initial_effort);
        let updated_mode = updated_session
            .collaboration_mode
            .as_deref()
            .expect("collaboration mode should be cached");
        assert_eq!(updated_mode.mode, collaboration_mode.mode);
        assert_eq!(
            updated_mode.settings.model,
            collaboration_mode.settings.model
        );
        assert_eq!(
            updated_mode.settings.reasoning_effort,
            collaboration_mode.settings.reasoning_effort
        );
        assert_eq!(updated_session.personality, Some(Personality::Pragmatic));
        assert_eq!(updated_session.service_tier, Some(service_tier));
        assert_eq!(updated_session.approval_policy, AskForApproval::OnRequest);
        assert_eq!(
            updated_session.approvals_reviewer,
            ApprovalsReviewer::AutoReview
        );
        assert_eq!(
            updated_session
                .active_permission_profile
                .as_ref()
                .expect("active profile")
                .id,
            codex_protocol::models::BUILT_IN_PERMISSION_PROFILE_WORKSPACE
        );
    })
    .await;
}

#[tokio::test]
async fn thread_setting_update_params_sync_model_and_default_reasoning() {
    let mut app = make_test_app().await;
    let thread_id = ThreadId::new();
    app.active_thread_id = Some(thread_id);

    app.chat_widget.set_model("gpt-5.4");
    let params = app
        .active_thread_model_setting_update_params("gpt-5.4".to_string())
        .expect("active thread should produce update params");

    assert_eq!(params.thread_id, thread_id.to_string());
    assert_eq!(params.model, Some("gpt-5.4".to_string()));
    assert_eq!(
        params
            .collaboration_mode
            .as_ref()
            .expect("collaboration mode should sync with model")
            .settings
            .model,
        "gpt-5.4"
    );

    app.chat_widget
        .set_reasoning_effort(Some(ReasoningEffortConfig::Low));
    app.chat_widget
        .set_collaboration_mask(CollaborationModeMask {
            name: "Plan".to_string(),
            mode: Some(ModeKind::Plan),
            model: Some("gpt-plan".to_string()),
            reasoning_effort: Some(Some(ReasoningEffortConfig::Medium)),
            developer_instructions: None,
        });
    app.on_update_reasoning_effort(Some(ReasoningEffortConfig::High));

    let params = app
        .active_thread_reasoning_setting_update_params(Some(ReasoningEffortConfig::High))
        .expect("active thread should produce update params");

    assert_eq!(params.thread_id, thread_id.to_string());
    assert_eq!(params.effort, Some(ReasoningEffortConfig::High));
    let collaboration_mode = params
        .collaboration_mode
        .expect("collaboration mode should sync with reasoning");
    assert_eq!(collaboration_mode.mode, ModeKind::Default);
    assert_eq!(
        collaboration_mode.settings.reasoning_effort,
        Some(ReasoningEffortConfig::High)
    );
}

#[tokio::test]
async fn inactive_thread_settings_notification_updates_cached_collaboration_mode() {
    let mut app = make_test_app().await;
    let primary_thread_id = ThreadId::new();
    let inactive_thread_id = ThreadId::new();
    let primary_session = test_thread_session(primary_thread_id, test_path_buf("/tmp/main"));
    let inactive_session = test_thread_session(inactive_thread_id, test_path_buf("/tmp/inactive"));
    let collaboration_mode = CollaborationMode {
        mode: ModeKind::Plan,
        settings: Settings {
            model: "gpt-plan".to_string(),
            reasoning_effort: Some(ReasoningEffortConfig::High),
            developer_instructions: Some("draft a plan first".to_string()),
        },
    };

    app.primary_thread_id = Some(primary_thread_id);
    app.active_thread_id = Some(primary_thread_id);
    app.primary_session_configured = Some(primary_session.clone());
    app.thread_event_channels.insert(
        primary_thread_id,
        ThreadEventChannel::new_with_session(
            THREAD_EVENT_CHANNEL_CAPACITY,
            primary_session,
            Vec::new(),
        ),
    );
    app.thread_event_channels.insert(
        inactive_thread_id,
        ThreadEventChannel::new_with_session(
            THREAD_EVENT_CHANNEL_CAPACITY,
            inactive_session,
            Vec::new(),
        ),
    );

    let notification = ThreadSettingsUpdatedNotification {
        thread_id: inactive_thread_id.to_string(),
        thread_settings: ThreadSettings {
            cwd: test_absolute_path("/tmp/thread-settings"),
            approval_policy: AskForApproval::OnRequest,
            approvals_reviewer: codex_app_server_protocol::ApprovalsReviewer::AutoReview,
            sandbox_policy: codex_app_server_protocol::SandboxPolicy::ReadOnly {
                network_access: false,
            },
            active_permission_profile: Some(
                codex_app_server_protocol::ActivePermissionProfile::read_only(),
            ),
            model: "gpt-plan".to_string(),
            model_provider: "openai".to_string(),
            service_tier: None,
            effort: collaboration_mode.settings.reasoning_effort.clone(),
            summary: None,
            collaboration_mode: collaboration_mode.clone(),
            multi_agent_mode: Default::default(),
            personality: Some(Personality::Pragmatic),
        },
    };
    app.enqueue_thread_notification(
        inactive_thread_id,
        ServerNotification::ThreadSettingsUpdated(notification),
    )
    .await
    .expect("settings notification should be cached");

    let cached_session = app
        .thread_event_channels
        .get(&inactive_thread_id)
        .expect("inactive thread channel")
        .store
        .lock()
        .await
        .session
        .clone()
        .expect("inactive session should remain cached");
    assert_eq!(cached_session.model, "gpt-test");
    assert_eq!(cached_session.personality, Some(Personality::Pragmatic));
    assert_eq!(
        cached_session.collaboration_mode.as_deref(),
        Some(&collaboration_mode)
    );

    app.chat_widget.handle_thread_session(cached_session);
    assert_eq!(
        app.chat_widget.active_collaboration_mode_kind(),
        ModeKind::Plan
    );
    assert_eq!(app.chat_widget.current_model(), "gpt-plan");
    assert_eq!(
        app.chat_widget.current_collaboration_mode().model(),
        "gpt-test"
    );
    assert_eq!(
        app.chat_widget.current_reasoning_effort(),
        Some(ReasoningEffortConfig::High)
    );
    assert_eq!(
        app.chat_widget.config_ref().personality,
        Some(Personality::Pragmatic)
    );
}

#[tokio::test]
async fn clear_only_ui_reset_preserves_chat_session_state() {
    let mut app = make_test_app().await;
    let thread_id = ThreadId::new();
    app.chat_widget
        .handle_thread_session(crate::session_state::ThreadSessionState {
            thread_id,
            forked_from_id: None,
            fork_parent_title: None,
            thread_name: Some("keep me".to_string()),
            model: "gpt-test".to_string(),
            model_provider_id: "test-provider".to_string(),
            service_tier: None,
            approval_policy: AskForApproval::Never,
            approvals_reviewer: ApprovalsReviewer::User,
            permission_profile: PermissionProfile::read_only(),
            active_permission_profile: None,
            cwd: test_path_buf("/tmp/project").abs(),
            runtime_workspace_roots: Vec::new(),
            instruction_source_paths: Vec::new(),
            reasoning_effort: None,
            collaboration_mode: None,
            personality: None,
            message_history: None,
            network_proxy: None,
            rollout_path: Some(PathBuf::new()),
            provider_account: None,
        });
    app.chat_widget
        .apply_external_edit("draft prompt".to_string());
    app.transcript_cells = vec![Arc::new(UserHistoryCell {
        message: "old message".to_string(),
        text_elements: Vec::new(),
        local_image_paths: Vec::new(),
        remote_image_urls: Vec::new(),
    }) as Arc<dyn HistoryCell>];
    app.overlay = Some(Overlay::new_transcript(
        app.transcript_cells.clone(),
        crate::keymap::RuntimeKeymap::defaults().pager,
    ));
    app.deferred_history_lines = vec![Line::from("stale buffered line").into()];
    app.has_emitted_history_lines = true;
    app.backtrack.primed = true;
    app.backtrack.overlay_preview_active = true;
    app.backtrack.nth_user_message = 0;
    app.backtrack_render_pending = true;

    app.reset_app_ui_state_after_clear();

    assert!(app.overlay.is_none());
    assert!(app.transcript_cells.is_empty());
    assert!(app.deferred_history_lines.is_empty());
    assert!(!app.has_emitted_history_lines);
    assert!(!app.backtrack.primed);
    assert!(!app.backtrack.overlay_preview_active);
    assert!(!app.backtrack_render_pending);
    assert_eq!(app.chat_widget.thread_id(), Some(thread_id));
    assert_eq!(app.chat_widget.composer_text_with_pending(), "draft prompt");
}

#[tokio::test]
async fn clear_only_ui_reset_allows_active_skill_warning_to_render_again() {
    let mut app = make_test_app().await;
    let error = SkillErrorInfo {
        path: test_path_buf("/tmp/project/.codex/skills/abc/SKILL.md"),
        message: "invalid description".to_string(),
    };

    assert_eq!(
        app.skill_load_warnings
            .newly_active_errors(std::slice::from_ref(&error)),
        vec![error.clone()]
    );
    assert_eq!(
        app.skill_load_warnings
            .newly_active_errors(std::slice::from_ref(&error)),
        Vec::<SkillErrorInfo>::new()
    );

    app.reset_app_ui_state_after_clear();

    assert_eq!(
        app.skill_load_warnings
            .newly_active_errors(std::slice::from_ref(&error)),
        vec![error]
    );
}

#[tokio::test]
async fn backtrack_esc_does_not_steal_empty_vim_insert_escape() {
    let mut app = make_test_app().await;
    let esc = crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Esc, KeyModifiers::NONE);

    assert!(app.chat_widget.composer_is_empty());
    assert!(app.should_handle_backtrack_esc(esc));

    app.chat_widget.toggle_vim_mode_and_notify();
    assert!(app.should_handle_backtrack_esc(esc));

    app.chat_widget
        .handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('i'),
            KeyModifiers::NONE,
        ));
    assert!(app.chat_widget.should_handle_vim_insert_escape(esc));
    assert!(!app.should_handle_backtrack_esc(esc));

    app.chat_widget.handle_key_event(esc);

    assert!(!app.backtrack.primed);
    assert!(!app.chat_widget.should_handle_vim_insert_escape(esc));
    assert!(app.should_handle_backtrack_esc(esc));
}

#[tokio::test]
async fn side_conversations_reject_backtrack_esc_without_stealing_vim_insert_escape() {
    let mut app = make_test_app().await;
    let esc = crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Esc, KeyModifiers::NONE);

    app.chat_widget
        .set_side_conversation_active(/*active*/ true);
    assert!(app.chat_widget.composer_is_empty());
    assert!(!app.should_handle_backtrack_esc(esc));
    assert!(app.should_reject_side_backtrack_esc(esc));

    app.chat_widget.toggle_vim_mode_and_notify();
    app.chat_widget
        .handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('i'),
            KeyModifiers::NONE,
        ));

    assert!(app.chat_widget.should_handle_vim_insert_escape(esc));
    assert!(!app.should_handle_backtrack_esc(esc));
    assert!(!app.should_reject_side_backtrack_esc(esc));
}

#[tokio::test]
async fn side_backtrack_rejection_reports_unavailable_message_snapshot() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    app.backtrack.primed = true;

    app.reject_side_backtrack_esc();

    assert!(!app.backtrack.primed);
    let cell = match app_event_rx.try_recv() {
        Ok(AppEvent::InsertHistoryCell(cell)) => cell,
        other => panic!("expected InsertHistoryCell event, got {other:?}"),
    };
    let rendered = cell
        .display_lines(/*width*/ 80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert_app_snapshot!(
        "side_backtrack_rejection_reports_unavailable_message",
        rendered
    );
}
#[tokio::test]
async fn provider_manager_reuses_policy_health_after_setup_handle_is_cleared() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    let host = crate::provider_status_host::ProviderStatusHost::from_config(
        &app.config,
        crate::provider_status_host::ProviderAccountMetadata {
            openai: codex_login::OpenAiAuthMetadata::Account,
            ..Default::default()
        },
    );
    app.model_catalog.set_provider_policy(
        crate::chatwidget::provider_model_policy::ProviderModelPolicy::new(
            host.clone(),
            codex_provider_auth::ProviderRuntimeAuthorizations::default(),
        ),
    );
    app.shared_provider_status_host = None;
    host.begin_credential_attempt("recreated-host".into(), "openai");
    assert!(host.reject_credential_attempt("recreated-host").is_some());
    let reopened = app.reusable_provider_status_host().unwrap();
    assert_eq!(
        reopened.resolve_provider("openai").unwrap().configuration,
        codex_provider_auth::ProviderConfigurationState::RecoveryRequired
    );
    reopened.credential_changed("openai");
    assert_eq!(
        host.resolve_provider("openai").unwrap().configuration,
        codex_provider_auth::ProviderConfigurationState::Configured
    );
}

#[path = "tests/provider_picker_refresh.rs"]
mod provider_picker_refresh;

#[tokio::test]
async fn provider_manager_open_preserves_lazy_command_authorization_without_shared_state() {
    let home = tempfile::tempdir().unwrap();
    let mut config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await
        .unwrap();
    let provider = codex_model_provider_info::ModelProviderInfo {
        name: "Command".into(),
        auth: Some(
            serde_json::from_value(serde_json::json!({
                "command": home.path().join("pf55-command")
            }))
            .unwrap(),
        ),
        ..Default::default()
    };
    config.model_provider_id = "command".into();
    config.model_provider = provider.clone();
    config.model_providers.insert("command".into(), provider);
    let mut shared = crate::provider_status_host::ProviderStatusHost::from_config(
        &config,
        crate::provider_status_host::ProviderAccountMetadata::default(),
    );
    let mut authorizations = codex_provider_auth::ProviderRuntimeAuthorizations::default();
    authorizations.set(
        "command",
        codex_provider_auth::ProviderRuntimeAuthorization::Authorized,
    );
    shared.set_runtime_authorizations(authorizations);

    let reused =
        super::provider_management_status::provider_manager_status_host(&config, Some(shared));
    assert_eq!(
        reused.resolve_provider("command").unwrap().availability,
        codex_provider_auth::ProviderAvailabilityState::Ready
    );
    let fallback = super::provider_management_status::provider_manager_status_host(
        &config, /*shared*/ None,
    );
    assert_eq!(
        fallback.resolve_provider("command").unwrap().availability,
        codex_provider_auth::ProviderAvailabilityState::Ready
    );
}

#[test]
fn provider_manager_claude_intent_uses_typed_status_and_recovery_source() {
    use codex_provider_auth::ProviderAvailabilityState;
    use codex_provider_auth::ProviderConfigurationState;
    use codex_provider_auth::ProviderCurrentState;
    use codex_provider_auth::ProviderEligibilityState;
    use codex_provider_auth::ProviderStatusSnapshot;
    use codex_provider_auth::claude_account_flow::ClaudeAccountIntent;
    use codex_provider_auth::claude_account_flow::ClaudeUnauthorizedRecoverySource as Source;

    let catalog = codex_provider_auth::ProviderCatalog::from_runtime_providers(
        &codex_model_provider_info::built_in_model_providers(/*openai_base_url*/ None),
    );
    let provider_id = catalog
        .get(codex_model_provider_info::CLAUDE_PLAN_PROVIDER_ID)
        .expect("Claude Account catalog entry")
        .id
        .clone();
    let status = |configuration| ProviderStatusSnapshot {
        id: provider_id.clone(),
        methods: Vec::new(),
        configuration,
        eligibility: ProviderEligibilityState::Active,
        current: ProviderCurrentState::NotCurrent,
        availability: ProviderAvailabilityState::Ready,
    };

    assert_eq!(
        super::provider_management::claude_intent_for_status(
            &status(ProviderConfigurationState::NotConfigured),
            Source::ManagedToken,
        ),
        Some(ClaudeAccountIntent::Add),
    );
    for source in [
        Source::ManagedToken,
        Source::ClaudeCodeLogin,
        Source::Environment,
    ] {
        assert_eq!(
            super::provider_management::claude_intent_for_status(
                &status(ProviderConfigurationState::RecoveryRequired),
                source,
            ),
            Some(ClaudeAccountIntent::UnauthorizedRecovery { source }),
        );
    }
    assert_eq!(
        super::provider_management::claude_intent_for_status(
            &status(ProviderConfigurationState::RecoveryRequired),
            Source::Unknown,
        ),
        Some(ClaudeAccountIntent::Add),
    );
    assert_eq!(
        super::provider_management::claude_intent_for_status(
            &status(ProviderConfigurationState::Configured),
            Source::ManagedToken,
        ),
        Some(ClaudeAccountIntent::UnauthorizedRecovery {
            source: Source::ManagedToken,
        }),
    );
}

#[tokio::test]
async fn stale_provider_manager_claude_recovery_source_is_inert() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let app_server = start_config_write_test_app_server(&app).await?;
    app.open_provider_manager(&app_server);
    let (generation, status_host, statuses) = time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(AppEvent::ProviderManagerStatusesResolved {
                generation,
                status_host,
                statuses,
                ..
            }) = app_event_rx.recv().await
            {
                break (generation, status_host, statuses);
            }
        }
    })
    .await
    .expect("timed out waiting for provider statuses");
    app.provider_manager_statuses_resolved(
        generation,
        status_host,
        statuses,
        /*accounts*/ None,
        &app_server,
    );
    let (provider_id, target, mut status) = {
        let host = app.provider_management_host.as_ref().unwrap();
        let entry = host
            .status_host()
            .catalog()
            .entries()
            .iter()
            .find(|entry| {
                entry.setup_capabilities.iter().any(|capability| {
                    *capability == codex_provider_auth::ProviderSetupCapability::ClaudeAccount
                })
            })
            .unwrap();
        let status = host
            .statuses()
            .iter()
            .find(|status| status.id == entry.id)
            .unwrap()
            .clone();
        (
            entry.id.clone(),
            codex_provider_auth::claude_account_flow::ClaudeAccountTarget::from_catalog_entry(
                entry,
            )
            .unwrap(),
            status,
        )
    };
    let host = app.provider_management_host.as_mut().unwrap();
    host.dispatch(
        codex_provider_auth::ProviderManagementAction::BeginAuthentication {
            provider_id: provider_id.clone(),
        },
    );
    let stale_attempt = host.authenticating_attempt().unwrap().0;
    host.dispatch(
        codex_provider_auth::ProviderManagementAction::AuthenticationCancelled {
            provider_id: provider_id.clone(),
        },
    );
    host.dispatch(
        codex_provider_auth::ProviderManagementAction::BeginAuthentication { provider_id },
    );
    let current_attempt = app
        .provider_management_host
        .as_ref()
        .unwrap()
        .authenticating_attempt()
        .unwrap();
    while app_event_rx.try_recv().is_ok() {}
    status.configuration = codex_provider_auth::ProviderConfigurationState::RecoveryRequired;
    app.provider_manager_claude_recovery_source_resolved(
        stale_attempt,
        target,
        status,
        codex_provider_auth::claude_account_flow::ClaudeUnauthorizedRecoverySource::ManagedToken,
    );

    assert_eq!(
        app.provider_management_host
            .as_ref()
            .unwrap()
            .authenticating_attempt(),
        Some(current_attempt),
    );
    assert!(app_event_rx.try_recv().is_err());
    Ok(())
}

#[tokio::test]
async fn provider_manager_stale_api_key_completion_is_inert_and_current_settles() -> Result<()> {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let app_server = start_config_write_test_app_server(&app).await?;
    app.open_provider_manager(&app_server);
    let (generation, status_host, statuses) = time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(AppEvent::ProviderManagerStatusesResolved {
                generation,
                status_host,
                statuses,
                ..
            }) = app_event_rx.recv().await
            {
                break (generation, status_host, statuses);
            }
        }
    })
    .await
    .expect("timed out waiting for provider statuses");
    app.provider_manager_statuses_resolved(
        generation,
        status_host.clone(),
        statuses,
        /*accounts*/ None,
        &app_server,
    );
    let initial_status_count = app
        .provider_management_host
        .as_ref()
        .unwrap()
        .statuses()
        .len();
    app.provider_manager_statuses_resolved(
        generation.wrapping_add(1),
        status_host.clone(),
        Vec::new(),
        /*accounts*/ None,
        &app_server,
    );
    assert_eq!(
        app.provider_management_host
            .as_ref()
            .unwrap()
            .statuses()
            .len(),
        initial_status_count
    );
    let (provider_id, capability, target) = {
        let host = app.provider_management_host.as_ref().unwrap();
        let (entry, capability) = host
            .status_host()
            .catalog()
            .entries()
            .iter()
            .find_map(|entry| {
                entry
                    .setup_capabilities
                    .iter()
                    .find(|capability| {
                        matches!(
                            capability,
                            codex_provider_auth::ProviderSetupCapability::ApiKey { .. }
                        )
                    })
                    .cloned()
                    .map(|capability| (entry.clone(), capability))
            })
            .unwrap();
        let target =
            codex_provider_auth::ApiKeyAuthTarget::from_catalog_capability(&entry, &capability)
                .unwrap();
        (entry.id, capability, target)
    };
    app.provider_manager_begin_authentication(provider_id.clone(), capability.clone());
    let authenticating_status_count = app
        .provider_management_host
        .as_ref()
        .unwrap()
        .statuses()
        .len();
    app.provider_manager_statuses_resolved(
        generation,
        status_host.clone(),
        Vec::new(),
        /*accounts*/ None,
        &app_server,
    );
    assert!(matches!(
        app.provider_management_host.as_ref().unwrap().phase(),
        codex_provider_auth::ProviderManagementPhase::Authenticating { .. }
    ));
    assert_eq!(
        app.provider_management_host
            .as_ref()
            .unwrap()
            .statuses()
            .len(),
        authenticating_status_count
    );
    let (first_attempt, _) = app
        .provider_management_host
        .as_ref()
        .unwrap()
        .authenticating_attempt()
        .unwrap();
    app.provider_manager_authentication_cancelled(provider_id.clone());
    app.provider_manager_begin_authentication(provider_id.clone(), capability);
    let (current_attempt, _) = app
        .provider_management_host
        .as_ref()
        .unwrap()
        .authenticating_attempt()
        .unwrap();
    assert_ne!(first_attempt, current_attempt);

    let status = codex_provider_auth::ProviderStatusSnapshot {
        id: provider_id,
        methods: Vec::new(),
        configuration: codex_provider_auth::ProviderConfigurationState::Configured,
        eligibility: codex_provider_auth::ProviderEligibilityState::Active,
        current: codex_provider_auth::ProviderCurrentState::NotCurrent,
        availability: codex_provider_auth::ProviderAvailabilityState::Ready,
    };
    app.provider_manager_api_key_finished(first_attempt, target.clone(), Some(status.clone()));
    assert_eq!(
        app.provider_management_host
            .as_ref()
            .unwrap()
            .authenticating_attempt(),
        Some((current_attempt, target.provider_id.clone()))
    );

    let canary = "pf54-secret-canary-do-not-log";
    let debug = format!(
        "{:?}",
        AppEvent::SaveProviderManagerApiKey {
            attempt_id: current_attempt,
            target: target.clone(),
            api_key: crate::app_event::ProviderApiKeySecret::new(canary.to_string()),
        }
    );
    assert!(!debug.contains(canary));
    app.provider_manager_api_key_finished(current_attempt, target, Some(status));
    assert!(matches!(
        app.provider_management_host.as_ref().unwrap().phase(),
        codex_provider_auth::ProviderManagementPhase::Persisting { .. }
    ));
    app.provider_manager_statuses_resolved(
        generation,
        status_host,
        Vec::new(),
        /*accounts*/ None,
        &app_server,
    );
    assert!(matches!(
        app.provider_management_host.as_ref().unwrap().phase(),
        codex_provider_auth::ProviderManagementPhase::Persisting { .. }
    ));
    Ok(())
}

#[tokio::test]
async fn provider_status_jobs_do_not_block_the_app_task() {
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    let started_at = std::time::Instant::now();

    super::provider_management_status::spawn_provider_status_job(move || {
        std::thread::sleep(Duration::from_millis(250));
        finished_tx.send(()).unwrap();
    });
    assert!(
        started_at.elapsed() < Duration::from_millis(100),
        "scheduling provider status resolution blocked the app task"
    );

    time::timeout(Duration::from_secs(1), async {
        tokio::task::spawn_blocking(move || finished_rx.recv())
            .await
            .unwrap()
    })
    .await
    .expect("background provider-status job never completed")
    .unwrap();
}

#[test]
fn embedded_provider_key_save_is_visible_to_one_bulk_status_refresh() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_stack_size(8 * 1024 * 1024)
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let home = tempdir()?;
        let provider_id = "pf54-managed";
        let env_key = "PF54_MANAGED_KEY";
        let provider = codex_model_provider_info::ModelProviderInfo {
            name: "PF54 Managed".into(),
            env_key: Some(env_key.into()),
            ..Default::default()
        };
        let mut config = crate::legacy_core::config::ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .build()
            .await?;
        config.model_provider_id = provider_id.into();
        config.model_provider = provider.clone();
        config.model_providers =
            codex_model_provider_info::built_in_model_providers(/*openai_base_url*/ None);
        config.model_providers.insert(provider_id.into(), provider);

        let app_server = crate::start_embedded_app_server_for_picker(&config).await?;
        let status_host = crate::provider_status_host::ProviderStatusHost::from_config(
            &config,
            crate::provider_status_host::ProviderAccountMetadata::default(),
        );
        let entry = status_host.catalog().get(provider_id).unwrap();
        let capability = entry.setup_capabilities.iter().next().unwrap();
        let target =
            codex_provider_auth::ApiKeyAuthTarget::from_catalog_capability(entry, capability)
                .unwrap();
        let canary = "pf54-embedded-save-secret-canary";
        let status = time::timeout(
            Duration::from_secs(60),
            crate::provider_auth_effect_executor::ProviderAuthEffectExecutor::persist_api_key(
                app_server.request_handle(),
                status_host.clone(),
                target,
                codex_provider_auth::ApiKeySecret::new(canary.to_string()),
            ),
        )
        .await
        .expect("embedded provider-key persistence timed out")
        .expect("embedded provider-key persistence failed");
        assert_eq!(
            status.configuration,
            codex_provider_auth::ProviderConfigurationState::Configured
        );

        let refresh_host = status_host.clone();
        // An upper bound only; Bazel CI runners needed more than 30s.
        let statuses = time::timeout(Duration::from_secs(90), async move {
            tokio::task::spawn_blocking(move || refresh_host.resolve())
                .await
                .unwrap()
        })
        .await
        .expect("bulk provider-status refresh timed out");
        let managed = statuses.get(provider_id).unwrap();
        assert_eq!(
            managed.configuration,
            codex_provider_auth::ProviderConfigurationState::Configured
        );
        assert_eq!(
            managed.availability,
            codex_provider_auth::ProviderAvailabilityState::Ready
        );
        assert!(!format!("{status:?} {statuses:?} {status_host:?}").contains(canary));
        app_server.shutdown().await?;
        Ok(())
    })
}

async fn start_config_write_test_app_server(app: &App) -> Result<AppServerSession> {
    Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await
}

fn claude_pane_text_op(text: &str) -> Op {
    Op::UserTurn {
        items: vec![UserInput::Text {
            text: text.to_string(),
            text_elements: Vec::new(),
        }],
        cwd: std::path::PathBuf::from("/tmp"),
        approval_policy: codex_app_server_protocol::AskForApproval::Never,
        approvals_reviewer: None,
        active_permission_profile: None,
        model: "glm-5.2".to_string(),
        effort: None,
        summary: None,
        service_tier: None,
        final_output_json_schema: None,
        collaboration_mode: None,
        personality: None,
    }
}

/// Round-3 B5 regression: a spawned Claude worker pane must not steal the
/// operator's active control surface. Only user-created panes activate.
#[tokio::test]
async fn spawned_claude_worker_pane_does_not_become_active() {
    let mut app = make_test_app().await;

    let user_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            /*spawn_role*/ None,
            /*spawn_nickname*/ None,
        )
        .expect("create user pane");
    assert_eq!(
        app.claude_panes.active_claude_pane_id(),
        Some(user_pane_id.as_str()),
        "a user-created Claude pane should become active"
    );

    let orc_pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            Some(crate::spawn_orchestration::SpawnRole::Orc),
            Some("Krimp".to_string()),
        )
        .expect("create Claude Orc pane");
    assert_ne!(orc_pane_id, user_pane_id);
    assert_eq!(
        app.claude_panes.active_claude_pane_id(),
        Some(user_pane_id.as_str()),
        "a spawned Claude worker must not steal the active control surface"
    );
}

/// Round-3 B5 regression: recognized slash commands stay global while a
/// Claude pane is active — they must never be forwarded to the worker as a
/// task, even when entered with unsupported inline args.
#[tokio::test]
async fn slash_commands_stay_global_while_claude_pane_is_active() {
    let (mut app, mut rx, _op_rx) = make_test_app_with_channels().await;

    let pane_id = app
        .claude_panes
        .create_pane_with_role(
            crate::claude_panes::ClaudeProviderProfileKind::ClaudePlan,
            app.config.cwd.to_path_buf(),
            app.config.codex_home.as_ref(),
            /*spawn_role*/ None,
            /*spawn_nickname*/ None,
        )
        .expect("create user pane");
    assert_eq!(
        app.claude_panes.active_claude_pane_id(),
        Some(pane_id.as_str())
    );

    // Bare recognized command: consumed by the control plane, opens the picker.
    let consumed = app.try_submit_active_claude_pane_op(&claude_pane_text_op("/panes"));
    assert!(consumed, "/panes op should be consumed");

    // Recognized command with unsupported inline args: still a command, never
    // worker-task text (this was the exact round-3 swallow path).
    let consumed = app.try_submit_active_claude_pane_op(&claude_pane_text_op("/panes extra junk"));
    assert!(consumed, "/panes with args should be consumed");

    let mut picker_events = 0;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, AppEvent::OpenPanePicker) {
            picker_events += 1;
        }
    }
    assert_eq!(
        picker_events, 2,
        "both slash inputs must dispatch the pane picker instead of becoming Claude turns"
    );
}

/// The slash-input guard recognizes commands and rejects everything else.
#[tokio::test]
async fn try_dispatch_slash_input_only_claims_recognized_commands() {
    let (mut app, mut rx, _op_rx) = make_test_app_with_channels().await;

    assert!(app.chat_widget.try_dispatch_slash_input("/panes"));
    assert!(app.chat_widget.try_dispatch_slash_input("  /panes  "));
    assert!(
        !app.chat_widget.try_dispatch_slash_input("hello world"),
        "plain text must not be claimed by the slash guard"
    );
    assert!(
        !app.chat_widget
            .try_dispatch_slash_input("/no-such-command-xyz"),
        "unrecognized commands must fall through to normal handling"
    );

    let mut picker_events = 0;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, AppEvent::OpenPanePicker) {
            picker_events += 1;
        }
    }
    assert_eq!(picker_events, 2);
}

#[tokio::test]
async fn superseded_saved_thread_requires_unique_live_replacement() {
    // Nicknames recycle once the per-role roster is exhausted; a stale saved thread must not be
    // superseded by an unrelated live worker that merely shares its nickname and role.
    let mut app = make_test_app().await;
    let stale_troll_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000310").expect("valid thread id");
    let live_troll_a_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000311").expect("valid thread id");
    let live_troll_b_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000312").expect("valid thread id");

    app.upsert_agent_picker_thread(
        stale_troll_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ true,
    );
    app.upsert_agent_picker_thread(
        live_troll_a_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_node.insert(
        crate::spawn_orchestration::thread_node_id(live_troll_a_thread_id),
        "pane:codex-main".to_string(),
    );
    app.spawn_parent_by_node.insert(
        crate::spawn_orchestration::thread_node_id(stale_troll_thread_id),
        "pane:codex-main".to_string(),
    );

    let stale_entry = app
        .agent_navigation
        .get(&stale_troll_thread_id)
        .cloned()
        .expect("stale entry");
    assert!(
        app.replacement_for_superseded_saved_native_spawn_thread(
            stale_troll_thread_id,
            &stale_entry,
        ) == Some(live_troll_a_thread_id),
        "a single live nickname+role match supersedes the stale thread"
    );

    // A second live thread with the same nickname+role makes the mapping ambiguous.
    app.upsert_agent_picker_thread(
        live_troll_b_thread_id,
        Some("Burzum".to_string()),
        Some("troll".to_string()),
        /*is_closed*/ false,
    );
    app.spawn_parent_by_node.insert(
        crate::spawn_orchestration::thread_node_id(live_troll_b_thread_id),
        "pane:codex-main".to_string(),
    );
    assert_eq!(
        app.replacement_for_superseded_saved_native_spawn_thread(
            stale_troll_thread_id,
            &stale_entry,
        ),
        None,
        "duplicate live replacements must not supersede the stale thread"
    );
}

#[tokio::test]
async fn stale_nazgul_binding_is_cleared_and_dispatch_fails_loudly() {
    let mut app = make_test_app().await;
    let main_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000321").expect("valid thread id");
    app.primary_thread_id = Some(main_thread_id);
    let missing_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000320").expect("valid thread id");
    app.spawn_nazgul_pane_id = Some(crate::spawn_orchestration::thread_node_id(
        missing_thread_id,
    ));
    app.spawn_native_endpoint_by_node.insert(
        crate::spawn_orchestration::thread_node_id(missing_thread_id),
        missing_thread_id,
    );

    app.clear_stale_nazgul_binding();
    assert_eq!(app.spawn_nazgul_pane_id, None);
    assert!(app.spawn_nazgul_rebind_required);

    let Err(error) = app.resolve_spawn_task_target("Nazgul") else {
        panic!("a stale binding must not fall through to Codex Main");
    };
    assert!(error.contains("Rebind a root pane"));

    app.set_spawn_nazgul_pane_binding(CODEX_MAIN_PANE_ID.to_string());
    assert!(!app.spawn_nazgul_rebind_required);
    assert!(matches!(
        app.resolve_spawn_task_target("Nazgul"),
        Ok(crate::spawn_orchestration::SpawnTaskTarget::Native(thread_id))
            if thread_id == main_thread_id
    ));
}

#[tokio::test]
async fn closed_nazgul_binding_is_stale_and_requires_rebind() {
    let mut app = make_test_app().await;
    let closed_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000322").expect("valid thread id");
    app.upsert_agent_picker_thread(
        closed_thread_id,
        Some("Angmar".to_string()),
        Some("nazgul".to_string()),
        /*is_closed*/ true,
    );
    app.spawn_nazgul_pane_id = Some(crate::spawn_orchestration::thread_node_id(closed_thread_id));

    app.clear_stale_nazgul_binding();

    assert_eq!(app.spawn_nazgul_pane_id, None);
    assert!(app.spawn_nazgul_rebind_required);
    assert!(app.resolve_spawn_task_target("Nazgul").is_err());
}

#[tokio::test]
async fn background_history_insertion_requests_a_frame_without_user_input() -> Result<()> {
    let mut app = make_test_app().await;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut draw_requests = tui.subscribe_draw_requests();

    app.insert_history_cell(
        &mut tui,
        Box::new(PlainHistoryCell::new(vec![Line::from(
            "asynchronous vault result",
        )])),
    );

    tokio::time::timeout(Duration::from_millis(250), draw_requests.recv())
        .await
        .expect("history insertion should schedule a frame")
        .expect("draw notification channel should remain open");
    Ok(())
}

/// PF-84: every thread of the session reports its usage per account through
/// `thread/tokenUsage/updated`; `/status` adds them up across a thread switch.
/// A report without account usage (named accounts off) or from a thread of an
/// earlier session records nothing, and a new session starts afresh.
#[tokio::test]
async fn thread_account_usage_reaches_status_and_resets_with_the_session() {
    let (mut app, _rx, _op_rx) = make_test_app_with_channels().await;
    let app_server = crate::start_embedded_app_server_for_picker(app.chat_widget.config_ref())
        .await
        .expect("embedded app server");
    let report = |thread_id: ThreadId, account: &str, total_tokens: i64| {
        let ServerNotification::ThreadTokenUsageUpdated(mut update) =
            token_usage_notification_with_total(
                thread_id,
                "turn-1",
                total_tokens,
                /*model_context_window*/ None,
            )
        else {
            unreachable!("token usage notification");
        };
        update.account_usage = Some(vec![codex_app_server_protocol::ThreadAccountTokenUsage {
            provider_id: "zai".to_string(),
            account: account.to_string(),
            usage: update.token_usage.total.clone(),
        }]);
        ServerNotification::ThreadTokenUsageUpdated(update)
    };
    let coordinator = ThreadId::new();
    let worker = ThreadId::new();
    app.primary_thread_id = Some(coordinator);
    app.agent_navigation.upsert(
        worker, /*agent_nickname*/ None, /*agent_role*/ None, /*is_closed*/ false,
    );
    for notification in [
        report(coordinator, "default", /*total_tokens*/ 9),
        report(worker, "work", /*total_tokens*/ 7),
        // A thread this session does not know (an earlier session's agent).
        report(ThreadId::new(), "work", /*total_tokens*/ 100),
        // Named accounts off: no account usage.
        token_usage_notification_with_total(
            worker, "turn-1", /*total_tokens*/ 50, /*model_context_window*/ None,
        ),
    ] {
        app.handle_app_server_event(
            &app_server,
            codex_app_server_client::AppServerEvent::ServerNotification(Box::new(notification)),
        )
        .await;
    }
    let totals = |app: &App| {
        app.chat_widget
            .session_account_usage
            .for_provider("zai", "default")
            .rows
            .into_iter()
            .map(|(account, usage)| (account, usage.total_tokens))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        totals(&app),
        vec![("default".to_string(), 9), ("work".to_string(), 7)]
    );

    // A thread that already reported still counts after it leaves the picker
    // (a discarded side conversation finishing its interrupted turn).
    app.agent_navigation.remove(worker);
    app.handle_app_server_event(
        &app_server,
        codex_app_server_client::AppServerEvent::ServerNotification(Box::new(report(
            worker, "work", /*total_tokens*/ 8,
        ))),
    )
    .await;
    let expected = vec![("default".to_string(), 9), ("work".to_string(), 8)];
    assert_eq!(totals(&app), expected);

    // A thread switch rebuilds the chat widget and keeps the session's totals.
    let replacement = ChatWidget::new_with_app_event(ChatWidgetInit {
        config: app.config.clone(),
        frame_requester: crate::tui::FrameRequester::test_dummy(),
        app_event_tx: app.app_event_tx.clone(),
        workspace_command_runner: None,
        initial_user_message: None,
        enhanced_keys_supported: app.enhanced_keys_supported,
        has_chatgpt_account: app.chat_widget.has_chatgpt_account(),
        has_codex_backend_auth: app.chat_widget.has_codex_backend_auth(),
        model_catalog: app.model_catalog.clone(),
        feedback: app.feedback.clone(),
        is_first_run: false,
        status_account_display: app.chat_widget.status_account_display().cloned(),
        runtime_model_provider_base_url: app
            .chat_widget
            .runtime_model_provider_base_url()
            .map(str::to_string),
        initial_plan_type: app.chat_widget.current_plan_type(),
        model: Some(app.chat_widget.current_model().to_string()),
        startup_tooltip_override: None,
        status_line_invalid_items_warned: app.status_line_invalid_items_warned.clone(),
        terminal_title_invalid_items_warned: app.terminal_title_invalid_items_warned.clone(),
        session_telemetry: app.session_telemetry.clone(),
    });
    app.replace_chat_widget(replacement);
    assert_eq!(totals(&app), expected);

    app.reset_thread_event_state();
    assert_eq!(totals(&app), vec![("default".to_string(), 0)]);
}
