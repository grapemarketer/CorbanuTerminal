use crate::history_cell::CompositeHistoryCell;
use crate::history_cell::HistoryCell;
use crate::history_cell::PlainHistoryCell;
use crate::history_cell::plain_lines;
use crate::history_cell::with_border_with_inner_width;
use crate::legacy_core::config::Config;
use crate::line_truncation::line_width;
use crate::provider_named_accounts::NamedSignInState;
use crate::token_usage::TokenUsage;
use crate::token_usage::TokenUsageInfo;
use crate::version::CODEX_CLI_VERSION;
use crate::width::display_width;
use chrono::DateTime;
use chrono::Local;
use codex_app_server_protocol::AskForApproval;
use codex_model_provider_info::WireApi;
use codex_product_brand::PRODUCT_NAME;
use codex_protocol::ThreadId;
use codex_protocol::account::PlanType;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::models::ActivePermissionProfile;
use codex_protocol::models::BUILT_IN_PERMISSION_PROFILE_DANGER_FULL_ACCESS;
use codex_protocol::models::BUILT_IN_PERMISSION_PROFILE_READ_ONLY;
use codex_protocol::models::BUILT_IN_PERMISSION_PROFILE_WORKSPACE;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ReasoningEffort;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_sandbox_summary::summarize_permission_profile;
use ratatui::prelude::*;
use ratatui::style::Stylize;
use std::collections::BTreeSet;
use std::path::PathBuf;
use url::Url;

use super::account::StatusAccountDisplay;
use super::account_usage::StatusAccountUsage;
use super::format::FieldFormatter;
use super::format::push_label;
use super::format::truncate_line_to_width;
use super::helpers::compose_account_display;
use super::helpers::compose_model_display;
use super::helpers::format_directory_display;
use super::helpers::format_tokens_compact;
use super::rate_limits::RateLimitSnapshotDisplay;
use super::rate_limits::StatusRateLimitData;
use super::rate_limits::StatusRateLimitRow;
use super::rate_limits::StatusRateLimitValue;
use super::rate_limits::compose_rate_limit_data;
use super::rate_limits::compose_rate_limit_data_many;
use super::rate_limits::format_status_limit_summary;
use super::rate_limits::render_status_limit_progress_bar;
use super::remote_connection::RemoteConnectionStatus;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_lines;
use crate::wrapping::word_wrap_lines;
use std::sync::Arc;
use std::sync::RwLock;

const CHATGPT_USAGE_URL: &str = "https://chatgpt.com/codex/settings/usage";

#[derive(Debug, Clone)]
struct StatusContextWindowData {
    percent_remaining: i64,
    tokens_in_context: i64,
    window: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct StatusTokenUsageData {
    total: i64,
    input: i64,
    output: i64,
    context_window: Option<StatusContextWindowData>,
}

#[derive(Debug)]
struct StatusRateLimitState {
    rate_limits: StatusRateLimitData,
    refreshing_rate_limits: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct StatusHistoryHandle {
    rate_limit_state: Arc<RwLock<StatusRateLimitState>>,
}

impl StatusHistoryHandle {
    pub(crate) fn finish_rate_limit_refresh(
        &self,
        rate_limits: &[RateLimitSnapshotDisplay],
        now: DateTime<Local>,
    ) {
        let rate_limits = if rate_limits.len() <= 1 {
            compose_rate_limit_data(rate_limits.first(), now)
        } else {
            compose_rate_limit_data_many(rate_limits, now)
        };
        #[expect(clippy::expect_used)]
        let mut state = self
            .rate_limit_state
            .write()
            .expect("status history rate-limit state poisoned");
        state.rate_limits = rate_limits;
        state.refreshing_rate_limits = false;
    }
}

#[derive(Debug)]
struct StatusHistoryCell {
    model_name: String,
    model_details: Vec<String>,
    directory: PathBuf,
    permissions: String,
    security: String,
    agents_summary: Arc<RwLock<String>>,
    collaboration_mode: Option<String>,
    model_provider: Option<String>,
    remote_connection: Option<RemoteConnectionStatus>,
    show_chatgpt_usage_link: bool,
    account: Option<StatusAccountDisplay>,
    account_value: Option<String>,
    thread_name: Option<String>,
    session_id: Option<String>,
    forked_from: Option<String>,
    token_usage: StatusTokenUsageData,
    /// PF-84: the active provider's per-account token summary, only while
    /// named accounts are on.
    account_usage: Option<StatusAccountUsage>,
    rate_limit_state: Arc<RwLock<StatusRateLimitState>>,
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn new_status_output(
    config: &Config,
    account_display: Option<&StatusAccountDisplay>,
    token_info: Option<&TokenUsageInfo>,
    total_usage: &TokenUsage,
    session_id: &Option<ThreadId>,
    thread_name: Option<String>,
    forked_from: Option<ThreadId>,
    rate_limits: Option<&RateLimitSnapshotDisplay>,
    _plan_type: Option<PlanType>,
    now: DateTime<Local>,
    model_name: &str,
    collaboration_mode: Option<&str>,
    reasoning_effort_override: Option<Option<ReasoningEffort>>,
) -> CompositeHistoryCell {
    let snapshots = rate_limits.map(std::slice::from_ref).unwrap_or_default();
    new_status_output_with_rate_limits(
        config,
        account_display,
        token_info,
        total_usage,
        session_id,
        thread_name,
        forked_from,
        snapshots,
        _plan_type,
        now,
        model_name,
        collaboration_mode,
        reasoning_effort_override,
        /*refreshing_rate_limits*/ false,
    )
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn new_status_output_with_rate_limits(
    config: &Config,
    account_display: Option<&StatusAccountDisplay>,
    token_info: Option<&TokenUsageInfo>,
    total_usage: &TokenUsage,
    session_id: &Option<ThreadId>,
    thread_name: Option<String>,
    forked_from: Option<ThreadId>,
    rate_limits: &[RateLimitSnapshotDisplay],
    _plan_type: Option<PlanType>,
    now: DateTime<Local>,
    model_name: &str,
    collaboration_mode: Option<&str>,
    reasoning_effort_override: Option<Option<ReasoningEffort>>,
    refreshing_rate_limits: bool,
) -> CompositeHistoryCell {
    new_status_output_with_rate_limits_handle(
        config,
        /*runtime_model_provider_base_url*/ None,
        /*remote_connection*/ None,
        account_display,
        token_info,
        total_usage,
        session_id,
        thread_name,
        forked_from,
        rate_limits,
        _plan_type,
        now,
        model_name,
        collaboration_mode,
        reasoning_effort_override,
        "<none>".to_string(),
        refreshing_rate_limits,
        /*account_usage*/ None,
        /*named_sign_in*/ None,
    )
    .0
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn new_status_output_with_rate_limits_handle(
    config: &Config,
    runtime_model_provider_base_url: Option<&str>,
    remote_connection: Option<&RemoteConnectionStatus>,
    account_display: Option<&StatusAccountDisplay>,
    token_info: Option<&TokenUsageInfo>,
    total_usage: &TokenUsage,
    session_id: &Option<ThreadId>,
    thread_name: Option<String>,
    forked_from: Option<ThreadId>,
    rate_limits: &[RateLimitSnapshotDisplay],
    _plan_type: Option<PlanType>,
    now: DateTime<Local>,
    model_name: &str,
    collaboration_mode: Option<&str>,
    reasoning_effort_override: Option<Option<ReasoningEffort>>,
    agents_summary: String,
    refreshing_rate_limits: bool,
    account_usage: Option<StatusAccountUsage>,
    named_sign_in: Option<NamedSignInState>,
) -> (CompositeHistoryCell, StatusHistoryHandle) {
    let command = PlainHistoryCell::new(vec!["/status".magenta().into()]);
    let (card, handle) = StatusHistoryCell::new(
        config,
        runtime_model_provider_base_url,
        remote_connection,
        account_display,
        token_info,
        total_usage,
        session_id,
        thread_name,
        forked_from,
        rate_limits,
        _plan_type,
        now,
        model_name,
        collaboration_mode,
        reasoning_effort_override,
        agents_summary,
        refreshing_rate_limits,
        account_usage,
        named_sign_in,
    );

    (
        CompositeHistoryCell::new(vec![Box::new(command), Box::new(card)]),
        handle,
    )
}

impl StatusHistoryCell {
    #[allow(clippy::too_many_arguments)]
    fn new(
        config: &Config,
        runtime_model_provider_base_url: Option<&str>,
        remote_connection: Option<&RemoteConnectionStatus>,
        account_display: Option<&StatusAccountDisplay>,
        token_info: Option<&TokenUsageInfo>,
        total_usage: &TokenUsage,
        session_id: &Option<ThreadId>,
        thread_name: Option<String>,
        forked_from: Option<ThreadId>,
        rate_limits: &[RateLimitSnapshotDisplay],
        _plan_type: Option<PlanType>,
        now: DateTime<Local>,
        model_name: &str,
        collaboration_mode: Option<&str>,
        reasoning_effort_override: Option<Option<ReasoningEffort>>,
        agents_summary: String,
        refreshing_rate_limits: bool,
        account_usage: Option<StatusAccountUsage>,
        named_sign_in: Option<NamedSignInState>,
    ) -> (Self, StatusHistoryHandle) {
        let approval_policy = AskForApproval::from(config.permissions.approval_policy.value());
        let permission_profile = config.permissions.effective_permission_profile();
        let workspace_roots = config.effective_workspace_roots();
        let mut config_entries = vec![
            ("workdir", config.cwd.display().to_string()),
            ("model", model_name.to_string()),
            ("provider", config.model_provider_id.clone()),
            (
                "approval",
                config.permissions.approval_policy.value().to_string(),
            ),
            (
                "sandbox",
                summarize_permission_profile(
                    &permission_profile,
                    &config.cwd,
                    workspace_roots.as_slice(),
                ),
            ),
        ];
        if config.model_provider.wire_api == WireApi::Responses {
            let effort_value = reasoning_effort_override
                .unwrap_or_else(|| config.model_reasoning_effort.clone())
                .map(|effort| effort.to_string())
                .unwrap_or_else(|| "none".to_string());
            config_entries.push(("reasoning effort", effort_value));
            config_entries.push((
                "reasoning summaries",
                config
                    .model_reasoning_summary
                    .map(|summary| summary.to_string())
                    .unwrap_or_else(|| "auto".to_string()),
            ));
        }
        let (model_name, model_details) = compose_model_display(model_name, &config_entries);
        let approval = config_entries
            .iter()
            .find(|(k, _)| *k == "approval")
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| "<unknown>".to_string());
        let active_permission_profile = config.permissions.active_permission_profile();
        let sandbox =
            status_permission_summary(&permission_profile, &config.cwd, workspace_roots.as_slice());
        let workspace_root_suffix = workspace_root_suffix(workspace_roots.as_slice(), &config.cwd);
        let approval = status_approval_label(approval_policy, config.approvals_reviewer, &approval);
        let permissions = status_permissions_label(
            active_permission_profile.as_ref(),
            &permission_profile,
            approval_policy,
            &sandbox,
            &approval,
            workspace_root_suffix.as_deref(),
        );
        // PF-84 (#555): name the account the usage rows mark active, which
        // is the one the app server reported for the thread when it did.
        let active_account = account_usage
            .as_ref()
            .and_then(|usage| usage.rows.first())
            .map(|(account, _)| account.as_str());
        // D86/D125: the account and its usage rows appear only when this
        // provider has a named account in the home (a stamped account
        // always is one), so homes without any show what they did.
        let provider_has_named_account = config.model_provider.account.is_some()
            || account_usage
                .as_ref()
                .is_some_and(StatusAccountUsage::provider_has_named_account);
        // PF-84-S06: a named sign-in account describes itself; D171 (#1286):
        // one removed or signed out while the session ran is not presented
        // as the session's account. #1251: from the answer the background
        // check cached (`None` while unknown), never the vault.
        let named_sign_in = codex_model_provider::named_sign_in_account(&config.model_provider)
            .map(|account| (account, named_sign_in));
        // With a remote app server the account lives on the remote host, so
        // this home's vault does not tell.
        let session_account_gone = remote_connection.is_none()
            && named_sign_in
                .as_ref()
                .is_some_and(|(_, state)| state == &Some(NamedSignInState::NoSignIn));
        let model_provider = format_model_provider(
            config,
            runtime_model_provider_base_url,
            active_account,
            provider_has_named_account,
            session_account_gone,
        );
        let account_usage = account_usage.filter(|_| provider_has_named_account);
        let show_chatgpt_usage_link = config.model_provider.requires_openai_auth;
        let account = compose_account_display(account_display);
        let account_value =
            status_account_value(config, account_display, named_sign_in, remote_connection);
        let security = crate::security::view::status_summary(
            config.security_level,
            crate::legacy_core::security_level_change::configured_level(config),
            *session_id,
        );
        let session_id = session_id.as_ref().map(std::string::ToString::to_string);
        let forked_from = forked_from.map(|id| id.to_string());
        let default_usage = TokenUsage::default();
        let (context_usage, context_window) = match token_info {
            Some(info) => (&info.last_token_usage, info.model_context_window),
            None => (&default_usage, config.model_context_window),
        };
        let context_window = context_window.map(|window| StatusContextWindowData {
            percent_remaining: context_usage.percent_of_context_window_remaining(window),
            tokens_in_context: context_usage.tokens_in_context_window(),
            window,
        });

        let token_usage = StatusTokenUsageData {
            total: total_usage.blended_total(),
            input: total_usage.non_cached_input(),
            output: total_usage.output_tokens,
            context_window,
        };
        let rate_limits = if rate_limits.len() <= 1 {
            compose_rate_limit_data(rate_limits.first(), now)
        } else {
            compose_rate_limit_data_many(rate_limits, now)
        };
        let rate_limit_state = Arc::new(RwLock::new(StatusRateLimitState {
            rate_limits,
            refreshing_rate_limits,
        }));
        let agents_summary = Arc::new(RwLock::new(agents_summary));

        (
            Self {
                model_name,
                model_details,
                directory: config.cwd.to_path_buf(),
                permissions,
                security,
                collaboration_mode: collaboration_mode.map(ToString::to_string),
                model_provider,
                remote_connection: remote_connection.cloned(),
                show_chatgpt_usage_link,
                account,
                account_value,
                thread_name,
                session_id,
                forked_from,
                token_usage,
                account_usage,
                agents_summary,
                rate_limit_state: rate_limit_state.clone(),
            },
            StatusHistoryHandle { rate_limit_state },
        )
    }

    fn token_usage_spans(&self) -> Vec<Span<'static>> {
        let total_fmt = format_tokens_compact(self.token_usage.total);
        let input_fmt = format_tokens_compact(self.token_usage.input);
        let output_fmt = format_tokens_compact(self.token_usage.output);

        vec![
            Span::from(total_fmt),
            Span::from(" total "),
            Span::from(" (").dim(),
            Span::from(input_fmt).dim(),
            Span::from(" input").dim(),
            Span::from(" + ").dim(),
            Span::from(output_fmt).dim(),
            Span::from(" output").dim(),
            Span::from(")").dim(),
        ]
    }

    fn context_window_spans(&self) -> Option<Vec<Span<'static>>> {
        let context = self.token_usage.context_window.as_ref()?;
        let percent = context.percent_remaining;
        let used_fmt = format_tokens_compact(context.tokens_in_context);
        let window_fmt = format_tokens_compact(context.window);

        Some(vec![
            Span::from(format!("{percent}% left")),
            Span::from(" (").dim(),
            Span::from(used_fmt).dim(),
            Span::from(" used / ").dim(),
            Span::from(window_fmt).dim(),
            Span::from(")").dim(),
        ])
    }

    fn rate_limit_lines(
        &self,
        state: &StatusRateLimitState,
        available_inner_width: usize,
        formatter: &FieldFormatter,
    ) -> Vec<Line<'static>> {
        match &state.rate_limits {
            StatusRateLimitData::Available(rows_data) => {
                if rows_data.is_empty() {
                    return vec![formatter.line(
                        "Limits",
                        vec![Span::from("not available for this account").dim()],
                    )];
                }

                self.rate_limit_row_lines(rows_data, available_inner_width, formatter)
            }
            StatusRateLimitData::Stale(rows_data) => {
                let mut lines =
                    self.rate_limit_row_lines(rows_data, available_inner_width, formatter);
                lines.push(formatter.line(
                    "Warning",
                    vec![Span::from(if state.refreshing_rate_limits {
                        "limits may be stale - run /status again shortly."
                    } else {
                        "limits may be stale - start new turn to refresh."
                    })
                    .dim()],
                ));
                lines
            }
            StatusRateLimitData::Unavailable => {
                vec![formatter.line(
                    "Limits",
                    vec![Span::from("not available for this account").dim()],
                )]
            }
            StatusRateLimitData::Missing => {
                vec![formatter.line(
                    "Limits",
                    vec![Span::from(if state.refreshing_rate_limits {
                        "refresh requested; run /status again shortly."
                    } else {
                        "data not available yet"
                    })
                    .dim()],
                )]
            }
        }
    }

    fn rate_limit_row_lines(
        &self,
        rows: &[StatusRateLimitRow],
        available_inner_width: usize,
        formatter: &FieldFormatter,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::with_capacity(rows.len().saturating_mul(2));

        for row in rows {
            match &row.value {
                StatusRateLimitValue::Window {
                    percent_used,
                    resets_at,
                    details,
                } => {
                    let percent_remaining = (100.0 - percent_used).clamp(0.0, 100.0);
                    let summary = format_status_limit_summary(percent_remaining);
                    let full_value_spans = vec![
                        Span::from(render_status_limit_progress_bar(percent_remaining)),
                        Span::from(" "),
                        Span::from(summary.clone()),
                    ];
                    // On narrow terminals, keep the percentage visible rather than
                    // letting the fixed-width progress bar crowd out the reset time.
                    let value_spans = if line_width(&Line::from(full_value_spans.clone()))
                        <= formatter.value_width(available_inner_width)
                    {
                        full_value_spans
                    } else {
                        vec![Span::from(summary)]
                    };
                    let base_spans = formatter.full_spans(row.label.as_str(), value_spans);
                    let base_line = Line::from(base_spans.clone());

                    if let Some(resets_at) = resets_at.as_ref() {
                        let resets_span = Span::from(format!("(resets {resets_at})")).dim();
                        let mut inline_spans = base_spans.clone();
                        inline_spans.push(Span::from(" ").dim());
                        inline_spans.push(resets_span.clone());

                        if line_width(&Line::from(inline_spans.clone())) <= available_inner_width {
                            lines.push(Line::from(inline_spans));
                        } else {
                            lines.push(base_line);
                            let reset_text = format!("(resets {resets_at})");
                            let reset_width = formatter.value_width(available_inner_width).max(1);
                            let wrap_options =
                                textwrap::Options::new(reset_width).break_words(false);
                            // Reset timestamps are the actionable part of this row, so wrap them
                            // onto continuation lines instead of truncating partial times/dates.
                            lines.extend(
                                textwrap::wrap(reset_text.as_str(), wrap_options)
                                    .into_iter()
                                    .map(|wrapped| {
                                        formatter.continuation(vec![
                                            Span::from(wrapped.into_owned()).dim(),
                                        ])
                                    }),
                            );
                        }
                    } else {
                        lines.push(base_line);
                    }
                    if let Some(details) = details {
                        let detail_width = formatter.value_width(available_inner_width).max(1);
                        let wrap_options = textwrap::Options::new(detail_width).break_words(false);
                        lines.extend(
                            textwrap::wrap(details.as_str(), wrap_options)
                                .into_iter()
                                .map(|wrapped| {
                                    formatter
                                        .continuation(vec![Span::from(wrapped.into_owned()).dim()])
                                }),
                        );
                    }
                }
                StatusRateLimitValue::Text(text) => {
                    let label = row.label.clone();
                    let spans =
                        formatter.full_spans(label.as_str(), vec![Span::from(text.clone())]);
                    lines.push(Line::from(spans));
                }
            }
        }

        lines
    }

    fn collect_rate_limit_labels(
        &self,
        state: &StatusRateLimitState,
        seen: &mut BTreeSet<String>,
        labels: &mut Vec<String>,
    ) {
        match &state.rate_limits {
            StatusRateLimitData::Available(rows) => {
                if rows.is_empty() {
                    push_label(labels, seen, "Limits");
                } else {
                    for row in rows {
                        push_label(labels, seen, row.label.as_str());
                    }
                }
            }
            StatusRateLimitData::Stale(rows) => {
                for row in rows {
                    push_label(labels, seen, row.label.as_str());
                }
                push_label(labels, seen, "Warning");
            }
            StatusRateLimitData::Unavailable => push_label(labels, seen, "Limits"),
            StatusRateLimitData::Missing => push_label(labels, seen, "Limits"),
        }
    }
}

fn status_permission_summary(
    permission_profile: &PermissionProfile,
    cwd: &AbsolutePathBuf,
    workspace_roots: &[AbsolutePathBuf],
) -> String {
    let summary = summarize_permission_profile(permission_profile, cwd, workspace_roots);
    if let Some(details) = summary.strip_prefix("read-only") {
        if details.contains("(network access enabled)") {
            return "read-only with network access".to_string();
        }
        return "read-only".to_string();
    }
    if let Some(details) = summary.strip_prefix("workspace-write") {
        if details.contains("(network access enabled)") {
            return "workspace with network access".to_string();
        }
        return "workspace".to_string();
    }
    if summary == "custom permissions (network access enabled)" {
        return "custom permissions with network access".to_string();
    }
    summary
}

fn workspace_root_suffix(
    workspace_roots: &[AbsolutePathBuf],
    cwd: &AbsolutePathBuf,
) -> Option<String> {
    let extra_roots = workspace_roots
        .iter()
        .filter(|root| *root != cwd)
        .map(|root| root.to_string_lossy().to_string())
        .collect::<Vec<_>>();
    if extra_roots.is_empty() {
        None
    } else {
        Some(format!(" [{}]", extra_roots.join(", ")))
    }
}

fn status_permissions_label(
    active_permission_profile: Option<&ActivePermissionProfile>,
    permission_profile: &PermissionProfile,
    approval_policy: AskForApproval,
    sandbox: &str,
    approval: &str,
    workspace_root_suffix: Option<&str>,
) -> String {
    let active_id = active_permission_profile.map(|active| active.id.as_str());
    match active_id {
        Some(BUILT_IN_PERMISSION_PROFILE_READ_ONLY) => {
            let label = if sandbox == "read-only with network access" {
                "Read Only with network access"
            } else {
                "Read Only"
            };
            return format!("{label} ({approval})");
        }
        Some(BUILT_IN_PERMISSION_PROFILE_WORKSPACE) => match sandbox {
            "workspace" => {
                return format!(
                    "Workspace{} ({approval})",
                    workspace_root_suffix.unwrap_or("")
                );
            }
            "workspace with network access" => {
                return format!(
                    "Workspace with network access{} ({approval})",
                    workspace_root_suffix.unwrap_or("")
                );
            }
            _ => {}
        },
        Some(BUILT_IN_PERMISSION_PROFILE_DANGER_FULL_ACCESS)
            if permission_profile == &PermissionProfile::Disabled =>
        {
            return if approval_policy == AskForApproval::Never {
                "Full Access".to_string()
            } else {
                format!("No Sandbox ({approval})")
            };
        }
        Some(id) => {
            let sandbox = decorate_workspace_sandbox_label(sandbox, workspace_root_suffix);
            return format!("Profile {id} ({sandbox}, {approval})");
        }
        None => {}
    }

    if sandbox == "read-only" {
        return format!("Read Only ({approval})");
    }
    if approval_policy == AskForApproval::OnRequest && sandbox == "workspace" {
        return format!(
            "Workspace{} ({approval})",
            workspace_root_suffix.unwrap_or("")
        );
    }
    if approval_policy == AskForApproval::Never
        && permission_profile == &PermissionProfile::Disabled
    {
        return "Full Access".to_string();
    }
    let sandbox = decorate_workspace_sandbox_label(sandbox, workspace_root_suffix);
    format!("Custom ({sandbox}, {approval})")
}

fn decorate_workspace_sandbox_label(sandbox: &str, workspace_root_suffix: Option<&str>) -> String {
    match workspace_root_suffix {
        Some(suffix) if sandbox.starts_with("workspace") => format!("{sandbox}{suffix}"),
        _ => sandbox.to_string(),
    }
}

fn status_approval_label(
    approval_policy: AskForApproval,
    approvals_reviewer: ApprovalsReviewer,
    approval: &str,
) -> String {
    if approval_policy == AskForApproval::OnRequest {
        return match approvals_reviewer {
            ApprovalsReviewer::AutoReview => "Approve for me".to_string(),
            ApprovalsReviewer::User => "Ask for approval".to_string(),
        };
    }

    approval.to_string()
}

fn status_account_value(
    config: &Config,
    account_display: Option<&StatusAccountDisplay>,
    named_sign_in: Option<(codex_login::NamedChatgptAccount, Option<NamedSignInState>)>,
    remote_connection: Option<&RemoteConnectionStatus>,
) -> Option<String> {
    let provider = &config.model_provider;
    // PF-84-S06: a named sign-in account describes itself, never the default
    // login (kind, email hint and plan only).
    if let Some((account, state)) = named_sign_in {
        return Some(match state {
            Some(NamedSignInState::SignedIn(summary)) => format!(
                "{} (account {})",
                crate::provider_named_accounts::describe_sign_in(&summary),
                account.name
            ),
            // D171 (#1286): removed or signed out, also while the session ran.
            // Short, as the line is cut at the card's width.
            Some(NamedSignInState::NoSignIn) if remote_connection.is_none() => format!(
                "account {name} has no sign-in: `corbanu login --account {name}` or `/providers`",
                name = account.name
            ),
            Some(NamedSignInState::NoSignIn | NamedSignInState::Unreadable) => {
                format!("account {} is not signed in", account.name)
            }
            // #1251: not known yet; the background check fills it in.
            None => format!("account {}", account.name),
        });
    }
    if provider.auth.is_some() {
        return Some(format!("{} account connected", provider.name));
    }
    if provider.aws.is_some() {
        return Some(format!("{} AWS credentials", provider.name));
    }

    let environment_key_present = provider
        .env_key
        .as_deref()
        .and_then(std::env::var_os)
        .is_some_and(|value| !value.is_empty());
    if environment_key_present || provider.experimental_bearer_token.is_some() {
        return Some(format!("{} API key configured", provider.name));
    }

    account_display.map(|account| match account {
        StatusAccountDisplay::ChatGpt { email, plan } => match (email, plan) {
            (Some(email), Some(plan)) => format!("{email} ({plan})"),
            (Some(email), None) => email.clone(),
            (None, Some(plan)) => plan.clone(),
            (None, None) => "ChatGPT".to_string(),
        },
        // Only providers that use OpenAI sign-in send the environment key.
        StatusAccountDisplay::ApiKey {
            env_var: Some(env_var),
        } if provider.uses_first_party_openai_auth() => {
            format!("{env_var} from your environment (billed per use)")
        }
        StatusAccountDisplay::ApiKey { .. } if provider.uses_first_party_openai_auth() => {
            "OpenAI API key configured (run corbanu login to use ChatGPT)".to_string()
        }
        // Other providers report their own stored key here (#462).
        StatusAccountDisplay::ApiKey { .. } => format!("{} API key configured", provider.name),
        StatusAccountDisplay::NotConfigured { provider } => {
            format!("{provider} not configured (set it up in /providers)")
        }
    })
}

impl HistoryCell for StatusHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(Line::from(vec![
            Span::from(format!("{}>_ ", FieldFormatter::INDENT)).dim(),
            Span::from(PRODUCT_NAME).bold(),
            Span::from(" ").dim(),
            Span::from(format!("(v{CODEX_CLI_VERSION})")).dim(),
        ]));

        let available_inner_width = usize::from(width.saturating_sub(4));
        if available_inner_width == 0 {
            return Vec::new();
        }

        let account_value = self.account_value.clone();

        let mut labels: Vec<String> =
            vec!["Model", "Directory", "Permissions", "Security", "Agents.md"]
                .into_iter()
                .map(str::to_string)
                .collect();
        let mut seen: BTreeSet<String> = labels.iter().cloned().collect();
        let thread_name = self.thread_name.as_deref().filter(|name| !name.is_empty());
        #[expect(clippy::expect_used)]
        let rate_limit_state = self
            .rate_limit_state
            .read()
            .expect("status history rate-limit state poisoned");
        #[expect(clippy::expect_used)]
        let agents_summary = self
            .agents_summary
            .read()
            .expect("status history agents summary state poisoned")
            .clone();

        if self.model_provider.is_some() {
            push_label(&mut labels, &mut seen, "Model provider");
        }
        if account_value.is_some() {
            push_label(&mut labels, &mut seen, "Account");
        }
        if thread_name.is_some() {
            push_label(&mut labels, &mut seen, "Thread name");
        }
        if self.session_id.is_some() {
            push_label(&mut labels, &mut seen, "Session");
        }
        if self.session_id.is_some() && self.forked_from.is_some() {
            push_label(&mut labels, &mut seen, "Forked from");
        }
        if self.collaboration_mode.is_some() {
            push_label(&mut labels, &mut seen, "Collaboration mode");
        }
        push_label(&mut labels, &mut seen, "Token usage");
        if self.token_usage.context_window.is_some() {
            push_label(&mut labels, &mut seen, "Context window");
        }
        let account_usage_lines = self
            .account_usage
            .as_ref()
            .filter(|_| !matches!(self.account, Some(StatusAccountDisplay::ChatGpt { .. })))
            .map(StatusAccountUsage::lines)
            .unwrap_or_default();
        if !account_usage_lines.is_empty() {
            push_label(&mut labels, &mut seen, "Account usage");
        }

        self.collect_rate_limit_labels(&rate_limit_state, &mut seen, &mut labels);

        let formatter = FieldFormatter::from_labels(labels.iter().map(String::as_str));
        let value_width = formatter.value_width(available_inner_width);

        let note_first_line = Line::from(vec![
            Span::from("Visit ").cyan(),
            CHATGPT_USAGE_URL.cyan().underlined(),
            Span::from(" for up-to-date").cyan(),
        ]);
        let note_second_line = Line::from(vec![
            Span::from("information on rate limits and credits").cyan(),
        ]);
        let note_lines = adaptive_wrap_lines(
            [note_first_line, note_second_line],
            RtOptions::new(available_inner_width),
        );
        lines.push(Line::from(Vec::<Span<'static>>::new()));
        // The ChatGPT usage page only applies to providers backed by OpenAI auth;
        // providers like Bedrock manage limits and billing elsewhere.
        if self.show_chatgpt_usage_link {
            lines.extend(note_lines);
            lines.push(Line::from(Vec::<Span<'static>>::new()));
        }
        if let Some(remote_connection) = self.remote_connection.as_ref() {
            let wrapped_remote = word_wrap_lines(
                [Line::from(vec![
                    Span::from(remote_connection.address.clone()),
                    Span::from(" (").dim(),
                    Span::from(remote_connection.version.clone()).dim(),
                    Span::from(")").dim(),
                ])],
                RtOptions::new(value_width.max(1)),
            );
            let mut wrapped_remote = wrapped_remote.into_iter();
            if let Some(first) = wrapped_remote.next() {
                lines.push(formatter.line("Remote", first.spans));
                lines.extend(wrapped_remote.map(|line| formatter.continuation(line.spans)));
            }
            lines.push(Line::from(Vec::<Span<'static>>::new()));
        }

        let mut model_spans = vec![Span::from(self.model_name.clone())];
        if !self.model_details.is_empty() {
            model_spans.push(Span::from(" (").dim());
            model_spans.push(Span::from(self.model_details.join(", ")).dim());
            model_spans.push(Span::from(")").dim());
        }

        let directory_value = format_directory_display(&self.directory, Some(value_width));

        lines.push(formatter.line("Model", model_spans));
        if let Some(model_provider) = self.model_provider.as_ref() {
            lines.push(formatter.line("Model provider", vec![Span::from(model_provider.clone())]));
        }
        lines.push(formatter.line("Directory", vec![Span::from(directory_value)]));
        // Config reflects session settings, not the active task's captured authority.
        let permissions = format!("Next turn: {}", self.permissions);
        let mut permission_lines = textwrap::wrap(&permissions, value_width.max(1)).into_iter();
        if let Some(first) = permission_lines.next() {
            lines.push(formatter.line("Permissions", vec![Span::from(first.into_owned())]));
            lines.extend(
                permission_lines
                    .map(|line| formatter.continuation(vec![Span::from(line.into_owned())])),
            );
        }
        let mut security_lines = textwrap::wrap(&self.security, value_width.max(1)).into_iter();
        if let Some(first) = security_lines.next() {
            lines.push(formatter.line("Security", vec![Span::from(first.into_owned())]));
            lines.extend(
                security_lines
                    .map(|line| formatter.continuation(vec![Span::from(line.into_owned())])),
            );
        }
        lines.push(formatter.line("Agents.md", vec![Span::from(agents_summary)]));

        if let Some(account_value) = account_value {
            lines.push(formatter.line("Account", vec![Span::from(account_value)]));
        }

        if let Some(thread_name) = thread_name {
            lines.push(formatter.line("Thread name", vec![Span::from(thread_name.to_string())]));
        }
        if let Some(collab_mode) = self.collaboration_mode.as_ref() {
            lines.push(formatter.line("Collaboration mode", vec![Span::from(collab_mode.clone())]));
        }
        if let Some(session) = self.session_id.as_ref() {
            lines.push(formatter.line("Session", vec![Span::from(session.clone())]));
        }
        if self.session_id.is_some()
            && let Some(forked_from) = self.forked_from.as_ref()
        {
            lines.push(formatter.line("Forked from", vec![Span::from(forked_from.clone())]));
        }

        lines.push(Line::from(Vec::<Span<'static>>::new()));
        // Hide token usage only for ChatGPT subscribers
        if !matches!(self.account, Some(StatusAccountDisplay::ChatGpt { .. })) {
            lines.push(formatter.line("Token usage", self.token_usage_spans()));
        }

        if let Some(spans) = self.context_window_spans() {
            lines.push(formatter.line("Context window", spans));
        }
        // One account is one line, wrapped to the card; several are a table.
        let account_usage_lines: Vec<Line<'static>> = if account_usage_lines.len() == 1 {
            word_wrap_lines(
                account_usage_lines.into_iter().map(Line::from),
                RtOptions::new(value_width.max(1)),
            )
        } else {
            account_usage_lines.into_iter().map(Line::from).collect()
        };
        let mut account_usage_lines = account_usage_lines.into_iter();
        if let Some(first) = account_usage_lines.next() {
            lines.push(formatter.line("Account usage", first.spans));
            lines.extend(account_usage_lines.map(|line| formatter.continuation(line.spans)));
        }

        lines.extend(self.rate_limit_lines(&rate_limit_state, available_inner_width, &formatter));

        let content_width = lines.iter().map(line_width).max().unwrap_or(0);
        let inner_width = content_width.min(available_inner_width);
        let truncated_lines: Vec<Line<'static>> = lines
            .into_iter()
            .map(|line| truncate_line_to_width(line, inner_width))
            .collect();

        with_border_with_inner_width(truncated_lines, inner_width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(self.display_lines(u16::MAX))
    }

    fn display_hyperlink_lines(
        &self,
        width: u16,
    ) -> Vec<crate::terminal_hyperlinks::HyperlinkLine> {
        let mut lines =
            crate::terminal_hyperlinks::plain_hyperlink_lines(self.display_lines(width));
        for line in &mut lines {
            let visible = line
                .line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>();
            if let Some(start_byte) = visible.find(CHATGPT_USAGE_URL) {
                let start = display_width(&visible[..start_byte]);
                line.hyperlinks
                    .push(crate::terminal_hyperlinks::TerminalHyperlink::web(
                        start..start + display_width(CHATGPT_USAGE_URL),
                        CHATGPT_USAGE_URL.to_string(),
                    ));
            }
        }
        lines
    }

    fn transcript_hyperlink_lines(
        &self,
        width: u16,
    ) -> Vec<crate::terminal_hyperlinks::HyperlinkLine> {
        self.display_hyperlink_lines(width)
    }
}

fn format_model_provider(
    config: &Config,
    runtime_base_url: Option<&str>,
    active_account: Option<&str>,
    provider_has_named_account: bool,
    session_account_gone: bool,
) -> Option<String> {
    let provider = &config.model_provider;
    let name = provider.name.trim();
    let provider_name = if name.is_empty() {
        config.model_provider_id.as_str()
    } else {
        name
    };
    let base_url = runtime_base_url.and_then(sanitize_base_url);
    // PF-84-S04: with named accounts on, name the account this session's
    // requests use, `default` included. D86: only when this provider has a
    // named account in the home, so homes without any show what they did.
    // #1193: that answer comes from a cache filled off the UI thread.
    let account = (config
        .features
        .enabled(codex_features::Feature::NamedAccounts)
        && crate::legacy_core::config::supports_named_accounts(provider)
        && provider_has_named_account)
        .then(|| {
            active_account.unwrap_or_else(|| {
                provider
                    .account
                    .as_ref()
                    .map_or(codex_vault::DEFAULT_PROVIDER_ACCOUNT, |account| {
                        account.name.as_str()
                    })
            })
        });
    let is_default_openai = provider.is_openai() && base_url.is_none();
    if is_default_openai && account.is_none() {
        return None;
    }

    let mut value = match base_url {
        Some(base_url) => format!("{provider_name} - {base_url}"),
        None => provider_name.to_string(),
    };
    match account {
        // D171 (#1286): the session's account was removed or signed out; the
        // Account line names it and its recovery.
        Some(account)
            if session_account_gone
                && provider
                    .account
                    .as_ref()
                    .is_some_and(|session| session.name == account) =>
        {
            value.push_str(" · account unavailable");
        }
        Some(account) => value.push_str(&format!(" · account {account}")),
        None => {}
    }
    Some(value)
}

fn sanitize_base_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let Ok(mut url) = Url::parse(trimmed) else {
        return None;
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string().trim_end_matches('/').to_string()).filter(|value| !value.is_empty())
}
