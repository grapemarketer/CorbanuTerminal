use super::new_status_output;
use super::new_status_output_with_rate_limits;
use super::new_status_output_with_rate_limits_handle;
use super::rate_limit_snapshot_display;
use super::rate_limits::RateLimitSnapshotDisplay;
use super::rate_limits::RateLimitWindowDisplay;
use super::rate_limits::SpendControlLimitSnapshotDisplay;
use super::rate_limits::StatusRateLimitData;
use super::rate_limits::compose_rate_limit_data_many;
use crate::history_cell::HistoryCell;
use crate::history_cell::PlainHistoryCell;
use crate::keymap::RuntimeKeymap;
use crate::legacy_core::config::Config;
use crate::legacy_core::config::ConfigBuilder;
use crate::legacy_core::config::PermissionProfileSnapshot;
use crate::pager_overlay::TranscriptOverlay;
use crate::status::StatusAccountDisplay;
use crate::status::remote_connection::RemoteConnectionStatus;
use crate::test_support::PathBufExt;
use crate::test_support::test_path_buf;
use crate::token_usage::TokenUsage;
use crate::token_usage::TokenUsageInfo;
use app_test_support::ChatGptAuthFixture;
use app_test_support::write_chatgpt_auth;
use app_test_support::write_models_cache;
use chrono::Duration as ChronoDuration;
use chrono::Local;
use chrono::TimeZone;
use chrono::Utc;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::CreditsSnapshot;
use codex_app_server_protocol::RateLimitSnapshot;
use codex_app_server_protocol::RateLimitWindow;
use codex_app_server_protocol::SpendControlLimitSnapshot;
use codex_config::LoaderOverrides;
use codex_config::types::AuthCredentialsStoreMode;
use codex_model_provider_info::ModelProviderAwsAuthInfo;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::test_support::construct_model_info_offline_for_tests;
use codex_models_manager::test_support::get_model_offline_for_tests;
use codex_protocol::ThreadId;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::models::ActivePermissionProfile;
use codex_protocol::models::BUILT_IN_PERMISSION_PROFILE_WORKSPACE;
use codex_protocol::models::ManagedFileSystemPermissions;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_utils_absolute_path::AbsolutePathBuf;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use ratatui::prelude::*;
use std::sync::Arc;
use tempfile::TempDir;
use unicode_width::UnicodeWidthStr;

#[test]
fn stale_monthly_limit_marks_fresh_rolling_snapshot_stale() {
    let now = Local::now();
    let snapshot = RateLimitSnapshotDisplay {
        limit_name: "codex".to_string(),
        captured_at: now,
        primary: Some(RateLimitWindowDisplay {
            used_percent: 20.0,
            resets_at: Some("soon".to_string()),
            window_minutes: Some(300),
        }),
        secondary: None,
        credits: None,
        individual_limit: Some(SpendControlLimitSnapshotDisplay {
            captured_at: now - ChronoDuration::minutes(20),
            percent_remaining: 68.0,
            used: "8,000".to_string(),
            limit: "25,000".to_string(),
            resets_at: Some("later".to_string()),
        }),
    };

    assert!(matches!(
        compose_rate_limit_data_many(&[snapshot], now),
        StatusRateLimitData::Stale(_)
    ));
}

fn app_server_workspace_write_profile(network_enabled: bool) -> PermissionProfile {
    PermissionProfile::Managed {
        network: if network_enabled {
            NetworkSandboxPolicy::Enabled
        } else {
            NetworkSandboxPolicy::Restricted
        },
        file_system: ManagedFileSystemPermissions::Restricted {
            entries: vec![
                FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::Root,
                    },
                    access: FileSystemAccessMode::Read,
                    missing_path_behavior: None,
                },
                FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::ProjectRoots { subpath: None },
                    },
                    access: FileSystemAccessMode::Write,
                    missing_path_behavior: None,
                },
                FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::SlashTmp,
                    },
                    access: FileSystemAccessMode::Write,
                    missing_path_behavior: None,
                },
                FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::Tmpdir,
                    },
                    access: FileSystemAccessMode::Write,
                    missing_path_behavior: None,
                },
            ],
            glob_scan_max_depth: None,
        },
    }
}

async fn test_config(temp_home: &TempDir) -> Config {
    let mut config = ConfigBuilder::default()
        .codex_home(temp_home.path().to_path_buf())
        .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
        .build()
        .await
        .expect("load config");
    config.approvals_reviewer = ApprovalsReviewer::User;
    config
        .permissions
        .set_permission_profile(app_server_workspace_write_profile(
            /*network_enabled*/ true,
        ))
        .expect("set permission profile");
    config
}

fn set_workspace_cwd(config: &mut Config, cwd: AbsolutePathBuf) {
    config.cwd = cwd.clone();
    config.workspace_roots = vec![cwd];
    config
        .permissions
        .set_workspace_roots(config.workspace_roots.clone());
}

fn test_status_account_display() -> Option<StatusAccountDisplay> {
    None
}

fn token_info_for(model_slug: &str, config: &Config, usage: &TokenUsage) -> TokenUsageInfo {
    let context_window =
        construct_model_info_offline_for_tests(model_slug, &config.to_models_manager_config())
            .context_window;
    TokenUsageInfo {
        total_token_usage: usage.clone(),
        last_token_usage: usage.clone(),
        model_context_window: context_window,
    }
}

fn render_lines(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

fn sanitize_directory(lines: Vec<String>) -> Vec<String> {
    let frame_width = lines
        .iter()
        .find(|line| line.starts_with('╭'))
        .map(|line| UnicodeWidthStr::width(line.as_str()));
    lines
        .into_iter()
        .map(|line| {
            let line = super::snapshot_helpers::normalize_snapshot_version(&line);
            if let (Some(frame_width), Some(dir_pos), Some(pipe_idx)) =
                (frame_width, line.find("Directory: "), line.rfind('│'))
            {
                let prefix = &line[..dir_pos + "Directory: ".len()];
                let suffix = &line[pipe_idx..];
                let replacement = "[[workspace]]";
                let content_width = frame_width.saturating_sub(
                    UnicodeWidthStr::width(prefix) + UnicodeWidthStr::width(suffix),
                );
                let mut rebuilt = prefix.to_string();
                rebuilt.push_str(replacement);
                let replacement_width = UnicodeWidthStr::width(replacement);
                if content_width > replacement_width {
                    rebuilt.push_str(&" ".repeat(content_width - replacement_width));
                }
                rebuilt.push_str(suffix);
                rebuilt
            } else {
                line
            }
        })
        .collect()
}

fn buffer_to_text(buffer: &Buffer, width: u16) -> String {
    let lines = buffer
        .content
        .chunks(usize::from(width))
        .map(|row| {
            row.iter()
                .map(|cell| {
                    let symbol = cell.symbol();
                    symbol
                        .strip_prefix("\x1b]8;;")
                        .and_then(|symbol| symbol.split_once('\x07'))
                        .and_then(|(_, symbol)| symbol.strip_suffix("\x1b]8;;\x07"))
                        .unwrap_or(symbol)
                })
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>();
    sanitize_directory(lines).join("\n")
}

fn reset_at_from(captured_at: &chrono::DateTime<chrono::Local>, seconds: i64) -> i64 {
    (*captured_at + ChronoDuration::seconds(seconds))
        .with_timezone(&Utc)
        .timestamp()
}

fn permissions_text_for(config: &Config) -> Option<String> {
    let usage = TokenUsage::default();
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let composite = new_status_output(
        config,
        test_status_account_display().as_ref(),
        /*token_info*/ None,
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    render_lines(&composite.display_lines(/*width*/ 240))
        .iter()
        .find(|line| line.contains("Permissions:"))
        .and_then(|line| {
            line.split("Permissions:")
                .nth(1)
                .map(str::trim)
                .map(|text| text.trim_end_matches('│'))
                .map(str::trim)
                .and_then(|text| text.strip_prefix("Next turn: "))
                .map(ToString::to_string)
        })
}

#[tokio::test]
async fn status_permissions_compact_next_turn_label_at_narrow_widths() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    let mut sections = Vec::new();
    for (profile, active, policy) in [
        (
            PermissionProfile::Disabled,
            ActivePermissionProfile::new(":danger-full-access"),
            AskForApproval::Never,
        ),
        (
            PermissionProfile::read_only(),
            ActivePermissionProfile::read_only(),
            AskForApproval::OnRequest,
        ),
    ] {
        config
            .permissions
            .approval_policy
            .set(policy.to_core())
            .unwrap();
        config
            .permissions
            .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
                profile, active,
            ))
            .unwrap();
        let cell = new_status_output(
            &config,
            /*account_display*/ None,
            /*token_info*/ None,
            &TokenUsage::default(),
            &None,
            /*thread_name*/ None,
            /*forked_from*/ None,
            /*rate_limits*/ None,
            /*_plan_type*/ None,
            Local::now(),
            "gpt-5.2",
            /*collaboration_mode*/ None,
            /*reasoning_effort_override*/ None,
        );
        // The renderer has no running-turn snapshot: retain the scope qualifier
        // without an unconditional paragraph or a guessed authority comparison.
        for width in [42, 46, 70, 100] {
            let lines = render_lines(&cell.display_lines(width));
            assert!(lines.iter().all(|line| line.width() <= usize::from(width)));
            let section = lines
                .iter()
                .skip_while(|line| !line.contains("Permissions:"))
                .take_while(|line| !line.contains("Security:"))
                .map(|line| line.trim_matches(['│', ' ']))
                .collect::<Vec<_>>();
            assert!(section.len() <= 4, "width {width}: {section:?}");
            sections.push(format!(
                "width {width}: {} lines\n{}",
                section.len(),
                section.join("\n")
            ));
        }
    }
    insta::assert_snapshot!(
        "status_permissions_distinguish_next_turn_from_unreported_running_authority",
        sections.join("\n\n")
    );
}

#[tokio::test]
async fn status_snapshot_includes_reasoning_details() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    config.model_provider_id = "openai".to_string();
    config.model_reasoning_summary = Some(ReasoningSummary::Detailed);
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());
    config
        .permissions
        .set_permission_profile(PermissionProfile::workspace_write())
        .expect("set permission profile");

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 1_200,
        cached_input_tokens: 200,
        output_tokens: 900,
        reasoning_output_tokens: 150,
        total_tokens: 2_250,
    };

    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 72,
            window_duration_mins: Some(300),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 600)),
        }),
        secondary: Some(RateLimitWindow {
            used_percent: 45,
            window_duration_mins: Some(10080),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 1_200)),
        }),
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);

    let reasoning_effort_override = Some(Some(ReasoningEffort::High));
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        reasoning_effort_override,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_shows_chatgpt_plan_without_email() {
    let temp_home = TempDir::new().expect("temp home");
    write_models_cache(temp_home.path()).expect("write models cache");
    std::fs::write(
        temp_home.path().join("config.toml"),
        "model_provider = \"openai\"\n",
    )
    .expect("write OpenAI provider config");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    config.cli_auth_credentials_store_mode = AuthCredentialsStoreMode::File;
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    write_chatgpt_auth(
        temp_home.path(),
        ChatGptAuthFixture::new("access-chatgpt").plan_type("enterprise_cbp_automation"),
        AuthCredentialsStoreMode::File,
    )
    .expect("write email-less ChatGPT auth");
    let mut app_server = crate::start_embedded_app_server_for_picker(&config)
        .await
        .expect("start embedded app server");
    let bootstrap = app_server
        .bootstrap(&config)
        .await
        .expect("bootstrap app server session");
    app_server.shutdown().await.expect("shut down app server");
    let account_display = bootstrap
        .status_account_display
        .expect("bootstrap should return ChatGPT account display");
    assert_eq!(
        account_display,
        StatusAccountDisplay::ChatGpt {
            email: None,
            plan: Some("Enterprise (Automation)".to_string()),
        }
    );
    let usage = TokenUsage::default();
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let model_slug = get_model_offline_for_tests(config.model.as_deref());

    let composite = new_status_output(
        &config,
        Some(&account_display),
        /*token_info*/ None,
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let sanitized =
        sanitize_directory(render_lines(&composite.display_lines(/*width*/ 80))).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_permissions_non_default_workspace_write_uses_workspace_label() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    config.model_provider_id = "openai".to_string();
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());
    config
        .permissions
        .set_permission_profile(app_server_workspace_write_profile(
            /*network_enabled*/ true,
        ))
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config).as_deref(),
        Some("Custom (workspace with network access, Ask for approval)")
    );
}

#[tokio::test]
async fn status_permissions_named_read_only_profile_shows_builtin_label() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::read_only(),
            ActivePermissionProfile::read_only(),
        ))
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config).as_deref(),
        Some("Read Only (Ask for approval)")
    );
}

#[tokio::test]
async fn status_permissions_read_only_profile_shows_additional_writable_roots() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    let extra_root = test_path_buf("/workspace/extra").abs();
    let file_system_policy = PermissionProfile::read_only()
        .file_system_sandbox_policy()
        .with_additional_writable_roots(config.cwd.as_path(), std::slice::from_ref(&extra_root));
    config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::from_runtime_permissions(
                &file_system_policy,
                NetworkSandboxPolicy::Restricted,
            ),
            ActivePermissionProfile::read_only(),
        ))
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config).as_deref(),
        Some("Read Only (Ask for approval)")
    );
}

#[tokio::test]
async fn status_permissions_named_workspace_profile_shows_builtin_label() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::workspace_write(),
            ActivePermissionProfile::new(BUILT_IN_PERMISSION_PROFILE_WORKSPACE),
        ))
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config).as_deref(),
        Some("Workspace (Ask for approval)")
    );
}

#[tokio::test]
async fn status_permissions_workspace_auto_review_shows_reviewer_label() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.approvals_reviewer = ApprovalsReviewer::AutoReview;
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::workspace_write(),
            ActivePermissionProfile::new(BUILT_IN_PERMISSION_PROFILE_WORKSPACE),
        ))
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config).as_deref(),
        Some("Workspace (Approve for me)")
    );
}

#[tokio::test]
async fn status_permissions_named_profile_shows_additional_writable_roots() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    let extra_root = test_path_buf("/workspace/extra").abs();
    config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::workspace_write_with(
                std::slice::from_ref(&extra_root),
                NetworkSandboxPolicy::Restricted,
                /*exclude_tmpdir_env_var*/ false,
                /*exclude_slash_tmp*/ false,
            ),
            ActivePermissionProfile::new(BUILT_IN_PERMISSION_PROFILE_WORKSPACE),
        ))
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config).as_deref(),
        Some("Workspace (Ask for approval)")
    );
}

#[tokio::test]
async fn status_permissions_workspace_roots_show_additional_directories() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    let extra_root = test_path_buf("/workspace/extra").abs();
    config.workspace_roots = vec![config.cwd.clone(), extra_root.clone()];
    config
        .permissions
        .set_workspace_roots(config.workspace_roots.clone());
    config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::workspace_write(),
            ActivePermissionProfile::new(":workspace"),
        ))
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config),
        Some(format!(
            "Workspace [{}] (Ask for approval)",
            extra_root.display()
        ))
    );
}

#[tokio::test]
async fn status_permissions_workspace_roots_include_profile_defined_directories() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    let profile_root = test_path_buf("/workspace/shared").abs();
    config
        .permissions
        .set_permission_profile_from_session_snapshot(
            PermissionProfileSnapshot::active_with_profile_workspace_roots(
                PermissionProfile::workspace_write_with(
                    std::slice::from_ref(&profile_root),
                    NetworkSandboxPolicy::Restricted,
                    /*exclude_tmpdir_env_var*/ false,
                    /*exclude_slash_tmp*/ false,
                ),
                ActivePermissionProfile::new(":workspace"),
                vec![profile_root.clone()],
            ),
        )
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config),
        Some(format!(
            "Workspace [{}] (Ask for approval)",
            profile_root.display()
        ))
    );
}

#[tokio::test]
async fn status_permissions_broadened_workspace_profile_shows_builtin_label() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::workspace_write_with(
                &[],
                NetworkSandboxPolicy::Enabled,
                /*exclude_tmpdir_env_var*/ false,
                /*exclude_slash_tmp*/ false,
            ),
            ActivePermissionProfile::new(BUILT_IN_PERMISSION_PROFILE_WORKSPACE),
        ))
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config).as_deref(),
        Some("Workspace with network access (Ask for approval)")
    );
}

#[tokio::test]
async fn status_permissions_user_defined_profile_shows_name() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::read_only(),
            ActivePermissionProfile::new("locked"),
        ))
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config).as_deref(),
        Some("Profile locked (read-only, Ask for approval)")
    );
}

#[tokio::test]
async fn status_snapshot_shows_active_user_defined_profile() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());
    config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::read_only(),
            ActivePermissionProfile::new("locked"),
        ))
        .expect("set permission profile");

    let usage = TokenUsage::default();
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);

    let composite = new_status_output(
        &config,
        test_status_account_display().as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_model_provider_uses_bedrock_runtime_base_url_and_gates_usage_link() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model_provider_id = "amazon-bedrock".to_string();
    config.model_provider =
        ModelProviderInfo::create_amazon_bedrock_provider(Some(ModelProviderAwsAuthInfo {
            profile: None,
            region: Some("eu-west-1".to_string()),
        }));
    config.model_provider.base_url =
        Some("https://bedrock-mantle.us-east-1.api.aws/openai/v1".to_string());
    let usage = TokenUsage::default();
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let runtime_base_url = "https://bedrock-mantle.eu-west-1.api.aws/openai/v1";

    let (composite, _handle) = new_status_output_with_rate_limits_handle(
        &config,
        Some(runtime_base_url),
        /*remote_connection*/ None,
        test_status_account_display().as_ref(),
        /*token_info*/ None,
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ &[],
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
        "<none>".to_string(),
        /*refreshing_rate_limits*/ false,
        /*account_usage*/ None,
        /*named_sign_in*/ None,
    );
    let rendered = render_lines(&composite.display_lines(/*width*/ 120)).join("\n");

    assert!(
        rendered.contains(&format!("Amazon Bedrock - {runtime_base_url}")),
        "expected /status to render runtime Bedrock URL, got: {rendered}"
    );
    assert!(
        !rendered.contains("bedrock-mantle.us-east-1"),
        "expected /status to ignore configured Bedrock base URL, got: {rendered}"
    );
    assert!(
        !rendered.contains("https://chatgpt.com/codex/settings/usage"),
        "expected /status to hide ChatGPT usage link for Bedrock, got: {rendered}"
    );

    config.model_provider_id = "openai-proxy".to_string();
    config.model_provider = ModelProviderInfo {
        name: "OpenAI Proxy".to_string(),
        base_url: Some("https://openai-proxy.example/v1".to_string()),
        requires_openai_auth: true,
        ..ModelProviderInfo::default()
    };
    let (composite, _handle) = new_status_output_with_rate_limits_handle(
        &config,
        /*runtime_model_provider_base_url*/ None,
        /*remote_connection*/ None,
        test_status_account_display().as_ref(),
        /*token_info*/ None,
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ &[],
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
        "<none>".to_string(),
        /*refreshing_rate_limits*/ false,
        /*account_usage*/ None,
        /*named_sign_in*/ None,
    );
    let rendered = render_lines(&composite.display_lines(/*width*/ 120)).join("\n");

    assert!(
        rendered.contains("https://chatgpt.com/codex/settings/usage"),
        "expected /status to show ChatGPT usage link for OpenAI-auth proxy, got: {rendered}"
    );

    let wide_destinations: Vec<String> = composite
        .display_hyperlink_lines(/*width*/ 120)
        .into_iter()
        .flat_map(|line| line.hyperlinks.into_iter())
        .map(|link| link.destination)
        .collect();
    assert_eq!(
        wide_destinations,
        vec!["https://chatgpt.com/codex/settings/usage"]
    );

    let narrow_destinations: Vec<String> = composite
        .display_hyperlink_lines(/*width*/ 24)
        .into_iter()
        .flat_map(|line| line.hyperlinks.into_iter())
        .map(|link| link.destination)
        .collect();
    assert_eq!(narrow_destinations, Vec::<String>::new());
}

#[tokio::test]
async fn status_snapshot_shows_auto_review_permissions() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());
    config.approvals_reviewer = ApprovalsReviewer::AutoReview;
    config
        .permissions
        .set_permission_profile_from_session_snapshot(PermissionProfileSnapshot::active(
            PermissionProfile::workspace_write(),
            ActivePermissionProfile::new(BUILT_IN_PERMISSION_PROFILE_WORKSPACE),
        ))
        .expect("set permission profile");

    let usage = TokenUsage::default();
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);

    let composite = new_status_output(
        &config,
        test_status_account_display().as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_permissions_full_disk_managed_with_network_is_danger_full_access() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    config
        .permissions
        .set_permission_profile(PermissionProfile::Managed {
            network: NetworkSandboxPolicy::Enabled,
            file_system: ManagedFileSystemPermissions::Unrestricted,
        })
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config).as_deref(),
        Some("Custom (danger-full-access, Ask for approval)")
    );
}

#[tokio::test]
async fn status_permissions_full_disk_managed_without_network_is_external_sandbox() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config
        .permissions
        .approval_policy
        .set(AskForApproval::OnRequest.to_core())
        .expect("set approval policy");
    config
        .permissions
        .set_permission_profile(PermissionProfile::Managed {
            network: NetworkSandboxPolicy::Restricted,
            file_system: ManagedFileSystemPermissions::Unrestricted,
        })
        .expect("set permission profile");

    assert_eq!(
        permissions_text_for(&config).as_deref(),
        Some("Custom (external-sandbox, Ask for approval)")
    );
}

#[tokio::test]
async fn status_snapshot_includes_forked_from() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    config.model_provider_id = "openai".to_string();
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 800,
        cached_input_tokens: 0,
        output_tokens: 400,
        reasoning_output_tokens: 0,
        total_tokens: 1_200,
    };

    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 8, 9, 10, 11, 12)
        .single()
        .expect("valid time");

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let session_id =
        ThreadId::from_string("0f0f3c13-6cf9-4aa4-8b80-7d49c2f1be2e").expect("session id");
    let forked_from =
        ThreadId::from_string("e9f18a88-8081-4e51-9d4e-8af5cde2d8dd").expect("forked id");

    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &Some(session_id),
        /*thread_name*/ None,
        Some(forked_from),
        /*rate_limits*/ None,
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_includes_monthly_limit() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    config.model_provider_id = "openai".to_string();
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 800,
        cached_input_tokens: 0,
        output_tokens: 400,
        reasoning_output_tokens: 0,
        total_tokens: 1_200,
    };

    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 5, 6, 7, 8, 9)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 12,
            window_duration_mins: Some(43_200),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 86_400)),
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_includes_enterprise_monthly_credit_limit() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    config.model_provider_id = "openai".to_string();
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 800,
        cached_input_tokens: 0,
        output_tokens: 400,
        reasoning_output_tokens: 0,
        total_tokens: 1_200,
    };
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 5, 6, 7, 8, 9)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: None,
        secondary: None,
        credits: None,
        individual_limit: Some(SpendControlLimitSnapshot {
            limit: "25000".to_string(),
            used: "8000".to_string(),
            remaining_percent: 68,
            resets_at: reset_at_from(&captured_at, /*seconds*/ 86_400),
        }),
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 92));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);

    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 46));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(
        "status_snapshot_wraps_enterprise_monthly_credit_details_in_narrow_terminal",
        sanitized
    );
}

#[tokio::test]
async fn status_snapshot_uses_generic_limit_labels_for_unsupported_windows() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    config.model_provider_id = "openai".to_string();
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 800,
        cached_input_tokens: 0,
        output_tokens: 400,
        reasoning_output_tokens: 0,
        total_tokens: 1_200,
    };

    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 5, 6, 7, 8, 9)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 35,
            window_duration_mins: Some(2 * 60),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 86_400)),
        }),
        secondary: Some(RateLimitWindow {
            used_percent: 50,
            window_duration_mins: Some(3 * 60),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 172_800)),
        }),
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_shows_unlimited_credits() {
    let temp_home = TempDir::new().expect("temp home");
    let config = test_config(&temp_home).await;
    let account_display = test_status_account_display();
    let usage = TokenUsage::default();
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 2, 3, 4, 5, 6)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: None,
        secondary: None,
        credits: Some(CreditsSnapshot {
            has_credits: true,
            unlimited: true,
            balance: None,
        }),
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let rendered = render_lines(&composite.display_lines(/*width*/ 120));
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("Credits:") && line.contains("Unlimited")),
        "expected Credits: Unlimited line, got {rendered:?}"
    );
}

#[tokio::test]
async fn status_snapshot_shows_positive_credits() {
    let temp_home = TempDir::new().expect("temp home");
    let config = test_config(&temp_home).await;
    let account_display = test_status_account_display();
    let usage = TokenUsage::default();
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 3, 4, 5, 6, 7)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: None,
        secondary: None,
        credits: Some(CreditsSnapshot {
            has_credits: true,
            unlimited: false,
            balance: Some("12.5".to_string()),
        }),
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let rendered = render_lines(&composite.display_lines(/*width*/ 120));
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("Credits:") && line.contains("13 credits")),
        "expected Credits line with rounded credits, got {rendered:?}"
    );
}

#[tokio::test]
async fn status_snapshot_shows_available_credits_without_display_balance() {
    let temp_home = TempDir::new().expect("temp home");
    let config = test_config(&temp_home).await;
    let account_display = test_status_account_display();
    let usage = TokenUsage::default();
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 4, 5, 6, 7, 8)
        .single()
        .expect("timestamp");
    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    for balance in [
        None,
        Some(String::new()),
        Some("0".to_string()),
        Some("not-a-number".to_string()),
        Some("inf".to_string()),
    ] {
        let snapshot = RateLimitSnapshot {
            limit_id: None,
            limit_name: None,
            primary: None,
            secondary: None,
            credits: Some(CreditsSnapshot {
                has_credits: true,
                unlimited: false,
                balance,
            }),
            individual_limit: None,
            spend_control_reached: None,
            plan_type: None,
            rate_limit_reached_type: None,
        };
        let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
        let composite = new_status_output(
            &config,
            account_display.as_ref(),
            Some(&token_info),
            &usage,
            &None,
            /*thread_name*/ None,
            /*forked_from*/ None,
            Some(&rate_display),
            None,
            captured_at,
            &model_slug,
            /*collaboration_mode*/ None,
            /*reasoning_effort_override*/ None,
        );
        let rendered = render_lines(&composite.display_lines(/*width*/ 120));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("Credits:") && line.contains("Available")),
            "expected Credits: Available line, got {rendered:?}"
        );
    }
}

#[tokio::test]
async fn status_snapshot_respects_unlimited_without_has_credits_flag() {
    let temp_home = TempDir::new().expect("temp home");
    let config = test_config(&temp_home).await;
    let account_display = test_status_account_display();
    let usage = TokenUsage::default();
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 5, 6, 7, 8, 9)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: None,
        secondary: None,
        credits: Some(CreditsSnapshot {
            has_credits: false,
            unlimited: true,
            balance: None,
        }),
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let rendered = render_lines(&composite.display_lines(/*width*/ 120));
    assert!(
        rendered
            .iter()
            .any(|line| line.contains("Credits:") && line.contains("Unlimited")),
        "expected Credits: Unlimited line, got {rendered:?}"
    );
}

#[tokio::test]
async fn status_card_token_usage_excludes_cached_tokens() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 1_200,
        cached_input_tokens: 200,
        output_tokens: 900,
        reasoning_output_tokens: 0,
        total_tokens: 2_100,
    };

    let now = chrono::Local
        .with_ymd_and_hms(2024, 1, 1, 0, 0, 0)
        .single()
        .expect("timestamp");

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        None,
        now,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let rendered = render_lines(&composite.display_lines(/*width*/ 120));

    assert!(
        rendered.iter().all(|line| !line.contains("cached")),
        "cached tokens should not be displayed, got: {rendered:?}"
    );
}

#[tokio::test]
async fn status_snapshot_truncates_in_narrow_terminal() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    config.model_provider_id = "openai".to_string();
    config.model_reasoning_summary = Some(ReasoningSummary::Detailed);
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 1_200,
        cached_input_tokens: 200,
        output_tokens: 900,
        reasoning_output_tokens: 150,
        total_tokens: 2_250,
    };

    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 72,
            window_duration_mins: Some(300),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 600)),
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let reasoning_effort_override = Some(Some(ReasoningEffort::High));
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        reasoning_effort_override,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 70));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");

    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_truncates_halfwidth_kana_in_narrow_terminal() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account = StatusAccountDisplay::ChatGpt {
        email: Some("ｶﾞﾊﾟｶﾞﾊﾟｶﾞﾊﾟ@example.com".to_string()),
        plan: Some("ｶﾞﾊﾟ plan".to_string()),
    };
    let usage = TokenUsage::default();
    let now = chrono::Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let composite = new_status_output(
        &config,
        Some(&account),
        /*token_info*/ None,
        &usage,
        &None,
        Some("ｶﾞﾊﾟｶﾞﾊﾟｶﾞﾊﾟｶﾞﾊﾟ thread".to_string()),
        /*forked_from*/ None,
        /*rate_limits*/ None,
        /*plan_type*/ None,
        now,
        "ｶﾞﾊﾟｶﾞﾊﾟｶﾞﾊﾟｶﾞﾊﾟ-model",
        Some("ｶﾞﾊﾟ collaboration mode"),
        /*reasoning_effort_override*/ None,
    );
    let rendered_lines = render_lines(&composite.display_lines(/*width*/ 42));
    let sanitized = sanitize_directory(rendered_lines).join("\n");

    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_uses_command_backed_provider_account_identity() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model_provider_id = "claude-plan".to_string();
    config.model_provider = ModelProviderInfo::create_claude_plan_provider();
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account = StatusAccountDisplay::ApiKey { env_var: None };
    let usage = TokenUsage::default();
    let now = chrono::Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let composite = new_status_output(
        &config,
        Some(&account),
        /*token_info*/ None,
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        /*plan_type*/ None,
        now,
        "claude-opus-5-plan",
        Some("Default"),
        /*reasoning_effort_override*/ None,
    );
    let rendered_lines = render_lines(&composite.display_lines(/*width*/ 100));
    let sanitized = sanitize_directory(rendered_lines).join("\n");

    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_shows_missing_limits_message() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 500,
        cached_input_tokens: 0,
        output_tokens: 250,
        reasoning_output_tokens: 0,
        total_tokens: 750,
    };

    let now = chrono::Local
        .with_ymd_and_hms(2024, 2, 3, 4, 5, 6)
        .single()
        .expect("timestamp");

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        None,
        now,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_names_openai_api_key_from_environment() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.4".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account = StatusAccountDisplay::ApiKey {
        env_var: Some("OPENAI_API_KEY".to_string()),
    };
    let usage = TokenUsage::default();
    let now = chrono::Local
        .with_ymd_and_hms(2024, 2, 3, 4, 5, 6)
        .single()
        .expect("timestamp");
    let composite = new_status_output(
        &config,
        Some(&account),
        /*token_info*/ None,
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        /*plan_type*/ None,
        now,
        "gpt-5.4",
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 100));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_does_not_name_openai_api_key_for_other_providers() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    // A local provider that never receives the OPENAI_API_KEY fallback.
    config.model_provider.requires_openai_auth = false;
    config.model_provider.name = "Local Test".to_string();
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account = StatusAccountDisplay::ApiKey {
        env_var: Some("OPENAI_API_KEY".to_string()),
    };
    let composite = new_status_output(
        &config,
        Some(&account),
        /*token_info*/ None,
        &TokenUsage::default(),
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        /*plan_type*/ None,
        chrono::Local::now(),
        "local-model",
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let rendered = render_lines(&composite.display_lines(/*width*/ 120)).join("\n");

    assert!(!rendered.contains("from your environment"), "{rendered}");
    // #462: a non-OpenAI provider's stored key is not an OpenAI key.
    assert!(!rendered.contains("OpenAI API key"), "{rendered}");
    assert!(
        rendered.contains("Local Test API key configured"),
        "{rendered}"
    );
}

/// #462: a Z.AI-only home (key in the vault, no OpenAI credential) must not
/// claim an OpenAI API key.
#[tokio::test]
async fn status_names_the_vault_key_provider_not_openai() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model_provider_id = "zai".to_string();
    config.model_provider = ModelProviderInfo {
        name: "Z.AI".to_string(),
        // Unset in the test environment: the key lives in the vault.
        env_key: Some("PF84_ISSUE_462_UNSET_API_KEY".to_string()),
        ..Default::default()
    };
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account = StatusAccountDisplay::ApiKey { env_var: None };
    let composite = new_status_output(
        &config,
        Some(&account),
        /*token_info*/ None,
        &TokenUsage::default(),
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        /*plan_type*/ None,
        chrono::Local::now(),
        "glm-5.3-flash",
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let rendered = render_lines(&composite.display_lines(/*width*/ 120)).join("\n");

    assert!(!rendered.contains("OpenAI"), "{rendered}");
    assert!(rendered.contains("Z.AI API key configured"), "{rendered}");
}

#[tokio::test]
async fn status_snapshot_uses_default_reasoning_when_config_empty() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 500,
        cached_input_tokens: 0,
        output_tokens: 250,
        reasoning_output_tokens: 0,
        total_tokens: 750,
    };

    let now = chrono::Local
        .with_ymd_and_hms(2024, 2, 3, 4, 5, 6)
        .single()
        .expect("timestamp");
    let remote_connection = RemoteConnectionStatus {
        address: "unix:///tmp/codex-home/app-server-control/app-server-control.sock".to_string(),
        version: "v0.133.0".to_string(),
    };

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let (composite, _) = new_status_output_with_rate_limits_handle(
        &config,
        /*runtime_model_provider_base_url*/ None,
        Some(&remote_connection),
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        &[],
        None,
        now,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ Some(Some(ReasoningEffort::Medium)),
        "<none>".to_string(),
        /*refreshing_rate_limits*/ false,
        /*account_usage*/ None,
        /*named_sign_in*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_shows_refreshing_limits_notice() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let usage = TokenUsage {
        input_tokens: 500,
        cached_input_tokens: 0,
        output_tokens: 250,
        reasoning_output_tokens: 0,
        total_tokens: 750,
    };
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 6, 7, 8, 9, 10)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 45,
            window_duration_mins: Some(300),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 900)),
        }),
        secondary: Some(RateLimitWindow {
            used_percent: 30,
            window_duration_mins: Some(10_080),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 2_700)),
        }),
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output_with_rate_limits(
        &config,
        /*account_display*/ None,
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        std::slice::from_ref(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
        /*refreshing_rate_limits*/ true,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn transcript_overlay_remeasures_status_after_rate_limit_refresh() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());
    let usage = TokenUsage::default();
    let now = Local
        .with_ymd_and_hms(2024, 6, 7, 8, 9, 10)
        .single()
        .expect("timestamp");
    let model_slug = get_model_offline_for_tests(config.model.as_deref());

    let (status, handle) = new_status_output_with_rate_limits_handle(
        &config,
        /*runtime_model_provider_base_url*/ None,
        /*remote_connection*/ None,
        /*account_display*/ None,
        /*token_info*/ None,
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ &[],
        None,
        now,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
        "<none>".to_string(),
        /*refreshing_rate_limits*/ true,
        /*account_usage*/ None,
        /*named_sign_in*/ None,
    );
    let mut overlay =
        TranscriptOverlay::new(vec![Arc::new(status)], RuntimeKeymap::defaults().pager);
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 80, /*height*/ 30,
    );
    let mut buffer = Buffer::empty(area);
    overlay.render(area, &mut buffer);
    let before = buffer_to_text(&buffer, area.width);

    handle.finish_rate_limit_refresh(
        &[RateLimitSnapshotDisplay {
            limit_name: "spark".to_string(),
            captured_at: now,
            primary: Some(RateLimitWindowDisplay {
                used_percent: 45.0,
                resets_at: Some("soon".to_string()),
                window_minutes: Some(300),
            }),
            secondary: Some(RateLimitWindowDisplay {
                used_percent: 30.0,
                resets_at: Some("later".to_string()),
                window_minutes: Some(10_080),
            }),
            credits: None,
            individual_limit: None,
        }],
        now,
    );
    overlay.insert_cell(Arc::new(PlainHistoryCell::new(vec!["next message".into()])));
    buffer = Buffer::empty(area);
    overlay.render(area, &mut buffer);
    let after = buffer_to_text(&buffer, area.width);

    assert!(
        after.contains("spark limit"),
        "status output was clipped: {after:?}"
    );
    assert!(
        after.contains("5h limit"),
        "status output was clipped: {after:?}"
    );
    assert!(
        after.contains("Weekly limit"),
        "status output was clipped: {after:?}"
    );
    insta::assert_snapshot!(
        "transcript_overlay_status_rate_limit_refresh",
        format!("before:\n{before}\n\nafter:\n{after}")
    );
}

#[tokio::test]
async fn status_snapshot_includes_credits_and_limits() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 1_500,
        cached_input_tokens: 100,
        output_tokens: 600,
        reasoning_output_tokens: 0,
        total_tokens: 2_200,
    };

    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 7, 8, 9, 10, 11)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 45,
            window_duration_mins: Some(300),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 900)),
        }),
        secondary: Some(RateLimitWindow {
            used_percent: 30,
            window_duration_mins: Some(10_080),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 2_700)),
        }),
        credits: Some(CreditsSnapshot {
            has_credits: true,
            unlimited: false,
            balance: None,
        }),
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_shows_unavailable_limits_message() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 500,
        cached_input_tokens: 0,
        output_tokens: 250,
        reasoning_output_tokens: 0,
        total_tokens: 750,
    };

    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: None,
        secondary: None,
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 6, 7, 8, 9, 10)
        .single()
        .expect("timestamp");
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_treats_refreshing_empty_limits_as_unavailable() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let usage = TokenUsage {
        input_tokens: 500,
        cached_input_tokens: 0,
        output_tokens: 250,
        reasoning_output_tokens: 0,
        total_tokens: 750,
    };

    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: None,
        secondary: None,
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 6, 7, 8, 9, 10)
        .single()
        .expect("timestamp");
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output_with_rate_limits(
        &config,
        /*account_display*/ None,
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        std::slice::from_ref(&rate_display),
        None,
        captured_at,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
        /*refreshing_rate_limits*/ true,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_shows_stale_limits_message() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex-max".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 1_200,
        cached_input_tokens: 200,
        output_tokens: 900,
        reasoning_output_tokens: 150,
        total_tokens: 2_250,
    };

    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 72,
            window_duration_mins: Some(300),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 600)),
        }),
        secondary: Some(RateLimitWindow {
            used_percent: 40,
            window_duration_mins: Some(10_080),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 1_800)),
        }),
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
    let now = captured_at + ChronoDuration::minutes(20);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        now,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_snapshot_cached_limits_hide_credits_without_flag() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model = Some("gpt-5.1-codex".to_string());
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());

    let account_display = test_status_account_display();
    let usage = TokenUsage {
        input_tokens: 900,
        cached_input_tokens: 200,
        output_tokens: 350,
        reasoning_output_tokens: 0,
        total_tokens: 1_450,
    };

    let captured_at = chrono::Local
        .with_ymd_and_hms(2024, 9, 10, 11, 12, 13)
        .single()
        .expect("timestamp");
    let snapshot = RateLimitSnapshot {
        limit_id: None,
        limit_name: None,
        primary: Some(RateLimitWindow {
            used_percent: 60,
            window_duration_mins: Some(300),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 1_200)),
        }),
        secondary: Some(RateLimitWindow {
            used_percent: 35,
            window_duration_mins: Some(10_080),
            resets_at: Some(reset_at_from(&captured_at, /*seconds*/ 2_400)),
        }),
        credits: Some(CreditsSnapshot {
            has_credits: false,
            unlimited: false,
            balance: Some("80".to_string()),
        }),
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };
    let rate_display = rate_limit_snapshot_display(&snapshot, captured_at);
    let now = captured_at + ChronoDuration::minutes(20);

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = token_info_for(&model_slug, &config, &usage);
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        Some(&rate_display),
        None,
        now,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    let sanitized = sanitize_directory(rendered_lines).join("\n");
    assert_snapshot!(sanitized);
}

#[tokio::test]
async fn status_context_window_uses_last_usage() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = test_config(&temp_home).await;
    config.model_context_window = Some(272_000);

    let account_display = test_status_account_display();
    let total_usage = TokenUsage {
        input_tokens: 12_800,
        cached_input_tokens: 0,
        output_tokens: 879,
        reasoning_output_tokens: 0,
        total_tokens: 102_000,
    };
    let last_usage = TokenUsage {
        input_tokens: 12_800,
        cached_input_tokens: 0,
        output_tokens: 879,
        reasoning_output_tokens: 0,
        total_tokens: 13_679,
    };

    let now = chrono::Local
        .with_ymd_and_hms(2024, 6, 1, 12, 0, 0)
        .single()
        .expect("timestamp");

    let model_slug = get_model_offline_for_tests(config.model.as_deref());
    let token_info = TokenUsageInfo {
        total_token_usage: total_usage.clone(),
        last_token_usage: last_usage,
        model_context_window: config.model_context_window,
    };
    let composite = new_status_output(
        &config,
        account_display.as_ref(),
        Some(&token_info),
        &total_usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        /*rate_limits*/ None,
        None,
        now,
        &model_slug,
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
    );
    let rendered_lines = render_lines(&composite.display_lines(/*width*/ 80));
    let context_line = rendered_lines
        .into_iter()
        .find(|line| line.contains("Context window"))
        .expect("context line");

    assert!(
        context_line.contains("13.7K used / 272K"),
        "expected context line to reflect last usage tokens, got: {context_line}"
    );
    assert!(
        !context_line.contains("102K"),
        "context line should not use total aggregated tokens, got: {context_line}"
    );
}

/// PF-84: a Z.AI session on `account` with named accounts on or off.
async fn named_account_status(
    named_accounts: bool,
    account: Option<&str>,
    account_usage: Option<super::StatusAccountUsage>,
    width: u16,
) -> String {
    let temp_home = TempDir::new().expect("temp home");
    named_account_status_in(&temp_home, named_accounts, account, account_usage, width).await
}

/// D86: adds named API-key accounts (provider id, name) to `home`'s vault.
fn enroll_named_accounts(home: &TempDir, accounts: &[(&str, &str)]) {
    let vault = codex_vault::Vault::new(home.path().to_path_buf());
    for (provider_id, name) in accounts {
        let name = codex_vault::ProviderAccountName::parse(name).expect("account name");
        vault
            .create_provider_account(
                provider_id,
                &name,
                codex_vault::ProviderAccountKind::ApiKey,
                "test-key-not-a-secret",
            )
            .expect("add named account");
    }
}

/// [`named_account_status`] in `temp_home`, whose vault the test prepared.
async fn named_account_status_in(
    temp_home: &TempDir,
    named_accounts: bool,
    account: Option<&str>,
    account_usage: Option<super::StatusAccountUsage>,
    width: u16,
) -> String {
    let mut config = test_config(temp_home).await;
    config.model_provider_id = "zai".to_string();
    config.model_provider = ModelProviderInfo {
        name: "Z.AI".to_string(),
        // Unset in the test environment: the key lives in the vault.
        env_key: Some("PF84_STATUS_ACCOUNT_USAGE_UNSET_KEY".to_string()),
        account: account.map(|name| codex_model_provider_info::NamedProviderAccount {
            provider_id: "zai".to_string(),
            name: name.to_string(),
        }),
        ..Default::default()
    };
    if named_accounts {
        config
            .features
            .enable(codex_features::Feature::NamedAccounts)
            .expect("enable named accounts");
    }
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());
    // #1193: what the background check found in this home's vault, unless
    // the test set it (a vault it cannot read leaves it unknown).
    let account_usage = account_usage.map(|mut summary| {
        if summary.named_account_in_home.is_none() {
            summary.named_account_in_home =
                crate::provider_named_accounts::named_account_providers(temp_home.path())
                    .map(|providers| providers.contains("zai"));
        }
        summary
    });
    // The session's own token usage: what the account rows add up to.
    let usage = account_usage
        .as_ref()
        .map(|summary| {
            summary
                .rows
                .iter()
                .fold(TokenUsage::default(), |mut total, (_, usage)| {
                    total.input_tokens += usage.input_tokens;
                    total.cached_input_tokens += usage.cached_input_tokens;
                    total.output_tokens += usage.output_tokens;
                    total.reasoning_output_tokens += usage.reasoning_output_tokens;
                    total.total_tokens += usage.total_tokens;
                    total
                })
        })
        .unwrap_or_else(|| {
            account_tokens(
                /*input*/ 47_100, /*cached*/ 12_000, /*output*/ 6_247,
                /*reasoning*/ 300,
            )
        });
    let now = chrono::Local
        .with_ymd_and_hms(2026, 10, 11, 4, 5, 6)
        .single()
        .expect("timestamp");
    let account_display = StatusAccountDisplay::ApiKey { env_var: None };
    let (composite, _) = new_status_output_with_rate_limits_handle(
        &config,
        /*runtime_model_provider_base_url*/ None,
        /*remote_connection*/ None,
        Some(&account_display),
        /*token_info*/ None,
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        &[],
        /*plan_type*/ None,
        now,
        "glm-5.3-flash",
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
        "<none>".to_string(),
        /*refreshing_rate_limits*/ false,
        account_usage,
        /*named_sign_in*/ None,
    );
    let mut rendered_lines = render_lines(&composite.display_lines(width));
    if cfg!(windows) {
        for line in &mut rendered_lines {
            *line = line.replace('\\', "/");
        }
    }
    sanitize_directory(rendered_lines).join("\n")
}

fn account_tokens(input: i64, cached: i64, output: i64, reasoning: i64) -> TokenUsage {
    TokenUsage {
        input_tokens: input,
        cached_input_tokens: cached,
        output_tokens: output,
        reasoning_output_tokens: reasoning,
        total_tokens: input + output,
    }
}

/// PF-84: with named accounts off, `/status` has no account usage.
#[tokio::test]
async fn status_snapshot_account_usage_flag_off() {
    let rendered = named_account_status(
        /*named_accounts*/ false, /*account*/ None, /*account_usage*/ None,
        /*width*/ 100,
    )
    .await;
    assert!(!rendered.contains("Account usage"), "{rendered}");
    assert_snapshot!(rendered);
}

#[tokio::test]
async fn status_snapshot_account_usage_no_usage_yet() {
    let rendered = named_account_status(
        /*named_accounts*/ true,
        Some("work"),
        Some(super::StatusAccountUsage {
            named_account_in_home: None,
            rows: vec![("work".to_string(), TokenUsage::default())],
        }),
        /*width*/ 100,
    )
    .await;
    assert_snapshot!(rendered);
}

#[tokio::test]
async fn status_snapshot_account_usage_one_account() {
    let rendered = named_account_status(
        /*named_accounts*/ true,
        Some("work"),
        Some(super::StatusAccountUsage {
            named_account_in_home: None,
            rows: vec![(
                "work".to_string(),
                account_tokens(
                    /*input*/ 47_100, /*cached*/ 12_000, /*output*/ 6_247,
                    /*reasoning*/ 300,
                ),
            )],
        }),
        // Narrow enough that the one account's line wraps.
        /*width*/ 80,
    )
    .await;
    assert_snapshot!(rendered);
}

#[tokio::test]
async fn status_snapshot_account_usage_several_accounts() {
    let rendered = named_account_status(
        /*named_accounts*/ true,
        /*account*/ None,
        Some(super::StatusAccountUsage {
            named_account_in_home: None,
            rows: vec![
                (
                    "default".to_string(),
                    account_tokens(
                        /*input*/ 40_100, /*cached*/ 12_000, /*output*/ 6_015,
                        /*reasoning*/ 300,
                    ),
                ),
                (
                    "work".to_string(),
                    account_tokens(
                        /*input*/ 7_000, /*cached*/ 0, /*output*/ 232,
                        /*reasoning*/ 0,
                    ),
                ),
            ],
        }),
        /*width*/ 100,
    )
    .await;
    assert_snapshot!(rendered);
}

fn model_provider_line(rendered: &str) -> &str {
    rendered
        .lines()
        .find(|line| line.contains("Model provider:"))
        .expect("model provider line")
}

fn default_account_usage() -> Option<super::StatusAccountUsage> {
    Some(super::StatusAccountUsage {
        named_account_in_home: None,
        rows: vec![(
            codex_vault::DEFAULT_PROVIDER_ACCOUNT.to_string(),
            account_tokens(
                /*input*/ 47_100, /*cached*/ 12_000, /*output*/ 6_247,
                /*reasoning*/ 300,
            ),
        )],
    })
}

/// D86: with named accounts on, a provider with no named account in the
/// home shows no account, as with them off.
#[tokio::test]
async fn status_snapshot_account_line_hidden_without_named_accounts() {
    let rendered = named_account_status(
        /*named_accounts*/ true,
        /*account*/ None,
        default_account_usage(),
        /*width*/ 100,
    )
    .await;
    let line = model_provider_line(&rendered);
    assert!(
        line.contains("Z.AI") && !line.contains("account"),
        "{rendered}"
    );
    // D125: nor the "Account usage" line: the card is exactly as with named
    // accounts off.
    assert!(!rendered.contains("Account usage"), "{rendered}");
    let flag_off = named_account_status(
        /*named_accounts*/ false, /*account*/ None, /*account_usage*/ None,
        /*width*/ 100,
    )
    .await;
    assert_eq!(rendered, flag_off);
    assert_snapshot!(rendered);
}

/// D86: once the provider has a named account, the line names the account
/// the session uses, `default` included.
#[tokio::test]
async fn status_snapshot_account_line_names_default_when_provider_has_named_account() {
    let home = TempDir::new().expect("temp home");
    enroll_named_accounts(&home, &[("zai", "work")]);
    let rendered = named_account_status_in(
        &home,
        /*named_accounts*/ true,
        /*account*/ None,
        default_account_usage(),
        /*width*/ 100,
    )
    .await;
    assert!(
        model_provider_line(&rendered).contains("Z.AI · account default"),
        "{rendered}"
    );
    assert_snapshot!(rendered);
}

/// D86: another provider's named account does not count; a vault that
/// cannot be read does (the account is named rather than hidden).
#[tokio::test]
async fn account_line_counts_only_this_providers_named_accounts() {
    let home = TempDir::new().expect("temp home");
    enroll_named_accounts(&home, &[("kimi", "work")]);
    let rendered = named_account_status_in(
        &home,
        /*named_accounts*/ true,
        /*account*/ None,
        default_account_usage(),
        /*width*/ 100,
    )
    .await;
    assert!(
        !model_provider_line(&rendered).contains("account"),
        "{rendered}"
    );
    assert!(!rendered.contains("Account usage"), "{rendered}");

    std::fs::write(
        home.path().join("secrets").join("local.age"),
        b"not an age file",
    )
    .expect("corrupt the vault");
    let rendered = named_account_status_in(
        &home,
        /*named_accounts*/ true,
        /*account*/ None,
        default_account_usage(),
        /*width*/ 100,
    )
    .await;
    assert!(
        model_provider_line(&rendered).contains("Z.AI · account default"),
        "{rendered}"
    );
    assert!(rendered.contains("Account usage:"), "{rendered}");
}

/// D86: with named accounts off, an enrolled named account changes nothing.
#[tokio::test]
async fn account_line_stays_off_with_named_accounts_off() {
    let home = TempDir::new().expect("temp home");
    enroll_named_accounts(&home, &[("zai", "work")]);
    let rendered = named_account_status_in(
        &home, /*named_accounts*/ false, /*account*/ None, /*account_usage*/ None,
        /*width*/ 100,
    )
    .await;
    let line = model_provider_line(&rendered);
    assert!(
        line.contains("Z.AI") && !line.contains("account"),
        "{rendered}"
    );
}

/// D86: the default OpenAI provider in a home without named accounts has no
/// "Model provider" line: it renders exactly as with named accounts off.
#[tokio::test]
async fn default_openai_status_is_unchanged_without_named_accounts() {
    async fn render(named_accounts: bool) -> String {
        let temp_home = TempDir::new().expect("temp home");
        let mut config = test_config(&temp_home).await;
        if named_accounts {
            config
                .features
                .enable(codex_features::Feature::NamedAccounts)
                .expect("enable named accounts");
        } else {
            config
                .features
                .disable(codex_features::Feature::NamedAccounts)
                .expect("disable named accounts");
        }
        set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());
        let usage = account_tokens(
            /*input*/ 1_000, /*cached*/ 0, /*output*/ 200, /*reasoning*/ 0,
        );
        let now = chrono::Local
            .with_ymd_and_hms(2026, 10, 11, 4, 5, 6)
            .single()
            .expect("timestamp");
        let account_display = StatusAccountDisplay::ChatGpt {
            email: Some("user@example.com".to_string()),
            plan: Some("Plus".to_string()),
        };
        let (composite, _) = new_status_output_with_rate_limits_handle(
            &config,
            /*runtime_model_provider_base_url*/ None,
            /*remote_connection*/ None,
            Some(&account_display),
            /*token_info*/ None,
            &usage,
            &None,
            /*thread_name*/ None,
            /*forked_from*/ None,
            &[],
            /*plan_type*/ None,
            now,
            "gpt-5.1-codex-max",
            /*collaboration_mode*/ None,
            /*reasoning_effort_override*/ None,
            "<none>".to_string(),
            /*refreshing_rate_limits*/ false,
            // What `status_account_usage` passes: rows only with the flag on.
            named_accounts.then(|| super::StatusAccountUsage {
                // The background check found no named account.
                named_account_in_home: Some(false),
                rows: vec![(
                    codex_vault::DEFAULT_PROVIDER_ACCOUNT.to_string(),
                    usage.clone(),
                )],
            }),
            /*named_sign_in*/ None,
        );
        sanitize_directory(render_lines(&composite.display_lines(/*width*/ 100))).join("\n")
    }
    let on = render(/*named_accounts*/ true).await;
    assert!(!on.contains("Model provider"), "{on}");
    assert_eq!(on, render(/*named_accounts*/ false).await);
}

/// D171 (#1286): `/status` of an OpenAI session on the named ChatGPT account
/// `work`, signed in when `signed_in`. #1251: with `known`, the background
/// check has cached what this home's vault holds for it; without, not yet.
async fn named_sign_in_status(signed_in: bool, remote: bool, known: bool) -> String {
    let temp_home = TempDir::new().expect("temp home");
    if signed_in {
        app_test_support::write_named_chatgpt_auth(
            temp_home.path(),
            "openai",
            "work",
            ChatGptAuthFixture::new("work-access-not-a-secret")
                .account_id("acct-work")
                .plan_type("plus"),
        )
        .expect("named sign-in");
    }
    let mut config = test_config(&temp_home).await;
    config.model_provider_id = "openai".to_string();
    config.model_provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
    config.model_provider.account = Some(codex_model_provider_info::NamedProviderAccount {
        provider_id: "openai".to_string(),
        name: "work".to_string(),
    });
    config
        .features
        .enable(codex_features::Feature::NamedAccounts)
        .expect("enable named accounts");
    set_workspace_cwd(&mut config, test_path_buf("/workspace/tests").abs());
    let usage = account_tokens(
        /*input*/ 1_000, /*cached*/ 0, /*output*/ 20, /*reasoning*/ 0,
    );
    let account_usage = super::StatusAccountUsage {
        named_account_in_home: Some(true),
        rows: vec![("work".to_string(), usage.clone())],
    };
    let now = chrono::Local
        .with_ymd_and_hms(2026, 10, 22, 4, 5, 6)
        .single()
        .expect("timestamp");
    let remote_connection = remote.then(|| RemoteConnectionStatus {
        address: "wss://remote.example".to_string(),
        version: "0.0.0".to_string(),
    });
    let (composite, _) = new_status_output_with_rate_limits_handle(
        &config,
        /*runtime_model_provider_base_url*/ None,
        remote_connection.as_ref(),
        /*account_display*/ None,
        /*token_info*/ None,
        &usage,
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        &[],
        /*plan_type*/ None,
        now,
        "gpt-5.5",
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
        "<none>".to_string(),
        /*refreshing_rate_limits*/ false,
        Some(account_usage),
        known.then(|| {
            crate::provider_named_accounts::named_sign_in_state(
                temp_home.path(),
                &codex_login::NamedChatgptAccount {
                    provider_id: "openai".to_string(),
                    name: "work".to_string(),
                },
            )
        }),
    );
    sanitize_directory(render_lines(&composite.display_lines(/*width*/ 100))).join("\n")
}

/// D171 (#1286): once the session's ChatGPT account is removed or signed
/// out, `/status` no longer presents the session as running on it: the
/// provider line says the account is unavailable and the Account line names
/// it with its recovery.
#[tokio::test]
async fn status_does_not_present_a_removed_sign_in_account_as_the_sessions() {
    let signed_in = named_sign_in_status(
        /*signed_in*/ true, /*remote*/ false, /*known*/ true,
    )
    .await;
    assert!(signed_in.contains("OpenAI · account work"), "{signed_in}");
    assert!(!signed_in.contains("unavailable"), "{signed_in}");

    // A remote app server's account lives on its host: this vault cannot tell.
    let remote = named_sign_in_status(
        /*signed_in*/ false, /*remote*/ true, /*known*/ true,
    )
    .await;
    assert!(remote.contains("OpenAI · account work"), "{remote}");
    assert!(!remote.contains("unavailable"), "{remote}");

    let removed = named_sign_in_status(
        /*signed_in*/ false, /*remote*/ false, /*known*/ true,
    )
    .await;
    assert!(!removed.contains("· account work"), "{removed}");
    assert!(
        removed.contains("OpenAI · account unavailable"),
        "{removed}"
    );
    assert!(
        removed.contains(
            "account work has no sign-in: `corbanu login --account work` or `/providers`"
        ),
        "{removed}"
    );
}

/// #1251: `/status` takes the named sign-in summary from the background
/// check's cached answer, never the vault. While it is not known yet, the
/// Account line names the account alone, whatever the vault holds.
#[tokio::test]
async fn status_snapshot_named_sign_in_summary_not_known_yet() {
    let signed_in = named_sign_in_status(
        /*signed_in*/ true, /*remote*/ false, /*known*/ true,
    )
    .await;
    assert!(signed_in.contains("ChatGPT sign-in"), "{signed_in}");
    assert!(signed_in.contains("(account work)"), "{signed_in}");

    let unknown = named_sign_in_status(
        /*signed_in*/ true, /*remote*/ false, /*known*/ false,
    )
    .await;
    assert!(!unknown.contains("ChatGPT sign-in"), "{unknown}");
    assert!(unknown.contains("OpenAI · account work"), "{unknown}");
    // Removed or signed out is not assumed while unknown.
    let removed_unknown = named_sign_in_status(
        /*signed_in*/ false, /*remote*/ false, /*known*/ false,
    )
    .await;
    assert_eq!(removed_unknown, unknown);
    assert_snapshot!(unknown);
}
