use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use crate::SkillInjections;
use crate::StateDbHandle;
use crate::build_skill_injections;
use crate::client::ModelClient;
use crate::client::ModelClientSession;
use crate::client_common::Prompt;
use crate::client_common::ResponseEvent;
use crate::collect_explicit_skill_mentions;
use crate::compact::InitialContextInjection;
use crate::compact::run_inline_auto_compact_task;
use crate::compact::should_use_remote_compact_task;
use crate::compact_remote::run_inline_remote_auto_compact_task;
use crate::compact_remote_v2::run_inline_remote_auto_compact_task as run_inline_remote_auto_compact_task_v2;
use crate::connectors;
use crate::context::ContextualUserFragment;
use crate::context::SubagentTurnBudgetFinalization;
use crate::context::TurnCompletionContinuation;
use crate::environment_selection::TurnEnvironmentSnapshot;
use crate::feedback_tags;
use crate::hook_runtime::inspect_pending_input;
use crate::hook_runtime::record_additional_contexts;
use crate::hook_runtime::record_pending_input;
use crate::hook_runtime::run_legacy_after_agent_hook;
use crate::hook_runtime::run_pending_session_start_hooks;
use crate::hook_runtime::run_turn_stop_hooks;
use crate::injection::ToolMentionKind;
use crate::injection::app_id_from_path;
use crate::injection::tool_kind_for_path;
use crate::mcp_skill_dependencies::maybe_prompt_and_install_mcp_dependencies;
use crate::mentions::build_connector_slug_counts;
use crate::mentions::build_skill_name_counts;
use crate::mentions::collect_explicit_app_ids;
use crate::mentions::collect_explicit_plugin_mentions;
use crate::mentions::collect_tool_mentions_from_messages;
use crate::plugins::build_plugin_injections;
use crate::provider_throttle::format_provider_request_block;
use crate::provider_throttle::notify_state_db_fallback;
use crate::provider_throttle::provider_request_key;
use crate::provider_throttle::provider_request_result_from_error;
use crate::provider_throttle::provider_request_result_kind;
use crate::provider_throttle::record_unleased_provider_request_result;
use crate::provider_throttle::turn_auth;
use crate::responses_metadata::CodexResponsesMetadata;
use crate::responses_metadata::CodexResponsesRequestKind;
use crate::responses_retry::ResponsesStreamRequest;
use crate::responses_retry::ensure_gpu_runtime_provider_active;
use crate::responses_retry::guard_same_request_idle_retry;
use crate::responses_retry::handle_retryable_response_stream_error;
use crate::session::PreviousTurnSettings;
use crate::session::TurnInput;
use crate::session::output_text_stream::PendingOutputText;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::turn_completion::CompletionAction;
use crate::session::turn_completion::CompletionProgressState;
use crate::session::turn_completion::assess_turn_completion;
use crate::session::turn_completion::objective_from_turn_input;
use crate::session::turn_context::TurnContext;
use crate::stream_events_utils::HandleOutputCtx;
use crate::stream_events_utils::TurnItemContributorPolicy;
use crate::stream_events_utils::finalize_non_tool_response_item;
use crate::stream_events_utils::handle_non_tool_response_item;
use crate::stream_events_utils::handle_output_item_done;
use crate::stream_events_utils::last_assistant_message_from_item;
use crate::stream_events_utils::mark_thread_memory_mode_polluted_if_external_context;
use crate::stream_events_utils::raw_assistant_output_text_from_item;
use crate::stream_events_utils::record_completed_response_item_with_finalized_facts;
use crate::tasks::emit_compact_metric;
use crate::tools::ToolRouter;
use crate::tools::context::SharedTurnDiffTracker;
use crate::tools::parallel::ToolCallRuntime;
use crate::tools::registry::ToolArgumentDiffConsumer;
use crate::tools::router::ToolSuggestCandidates;
use crate::tools::router::ToolSuggestPresentation;
use crate::tools::spec_plan::build_tool_router;
use crate::tools::spec_plan::tool_suggest_enabled;
use crate::turn_diff_tracker::TurnDiffTracker;
use crate::turn_timing::now_unix_timestamp_ms;
use crate::turn_timing::record_turn_ttft_metric;
use crate::util::error_or_panic;
use codex_analytics::AppInvocation;
use codex_analytics::CompactionPhase;
use codex_analytics::CompactionReason;
use codex_analytics::InvocationType;
use codex_analytics::TurnResolvedConfigFact;
use codex_analytics::build_track_events_context;
use codex_async_utils::OrCancelExt;
use codex_core_plugins::RecommendedPluginCandidatesInput;
use codex_core_skills::injection::InjectedHostSkillPrompts;
use codex_extension_api::ExtensionData;
use codex_extension_api::TurnInputContext;
use codex_extension_api::TurnInputEnvironment;
use codex_features::Feature;
use codex_file_system::FindUpErrorPolicy;
use codex_file_system::find_nearest_ancestor_with_markers;
use codex_login::CodexAuth;
use codex_mcp::ToolInfo;
use codex_model_provider_info::ChatStopSemantics;
use codex_protocol::ResponseItemId;
use codex_protocol::config_types::AutoCompactTokenLimitScope;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::ServiceTier;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::items::PlanItem;
use codex_protocol::items::TurnItem;
use codex_protocol::items::build_hook_prompt_message;
use codex_protocol::models::BaseInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::MessagePhase;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::AgentMessageContentDeltaEvent;
use codex_protocol::protocol::AgentMessageKind;
use codex_protocol::protocol::AgentReasoningSectionBreakEvent;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::ErrorEvent;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ModelResponseCompletedEvent;
use codex_protocol::protocol::PlanDeltaEvent;
use codex_protocol::protocol::RawResponseCompletedEvent;
use codex_protocol::protocol::ReasoningContentDeltaEvent;
use codex_protocol::protocol::ReasoningRawContentDeltaEvent;
use codex_protocol::protocol::SafetyBufferingEvent;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TurnDiffEvent;
use codex_protocol::protocol::WarningEvent;
use codex_protocol::user_input::UserInput;
use codex_state::ProviderRequestKey;
use codex_state::ProviderRequestLease;
use codex_state::ProviderRequestLeaseDecision;
use codex_state::ProviderRequestPreflight;
use codex_state::ProviderRequestResult;
use codex_tools::DiscoverableTool;
use codex_tools::ToolName;
use codex_tools::filter_request_plugin_install_discoverable_tools_for_client;
use codex_utils_path_uri::PathUri;
use codex_utils_stream_parser::AssistantTextChunk;
use codex_utils_stream_parser::AssistantTextStreamParser;
use codex_utils_stream_parser::ProposedPlanSegment;
use codex_utils_stream_parser::extract_proposed_plan_text;
use codex_utils_stream_parser::strip_citations;
use futures::prelude::*;
use futures::stream::FuturesOrdered;
use regex_lite::Regex;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use tracing::Instrument;
use tracing::debug;
use tracing::error;
use tracing::field;
use tracing::info;
use tracing::instrument;
use tracing::trace;
use tracing::trace_span;
use tracing::warn;

const POST_SAMPLING_TOKEN_ESTIMATE_TARGET: &str = "codex_core::post_sampling_token_estimate";
const PROVIDER_REQUEST_LEASE_TTL_MS: i64 = 10 * 60 * 1000;
const THIRD_PARTY_PREFLIGHT_WARNING_INPUT_TOKENS: i64 = 32_000;
const THIRD_PARTY_PREFLIGHT_WARNING_REQUEST_BYTES: i64 = 128 * 1024;
const THIRD_PARTY_CACHE_HEALTH_MIN_INPUT_TOKENS: i64 = 8_000;
const THIRD_PARTY_CACHE_HEALTHY_HIT_RATE: f64 = 0.70;
const MAX_SERVER_SIDE_MODEL_CONTINUATIONS: u64 = 5;

fn trace_turn_timing(label: &str, start: Instant) {
    if std::env::var_os("PFTERMINAL_TRACE_STREAM_TIMING").is_some() {
        debug!(
            target: "corbanu_turn",
            label,
            elapsed_ms = start.elapsed().as_millis(),
            "pfterminal turn timing"
        );
    }
}

/// Why a turn is refused before anything of it is recorded: the kill switch
/// (#445), or (#1331, D171) a named sign-in account removed or signed out
/// while the session ran on it, whose model request would be refused with
/// the same error.
pub(crate) async fn turn_refusal(sess: &Session, turn_context: &TurnContext) -> Option<CodexErr> {
    if let Err(err) = sess.services.model_client().refuse_under_kill_switch() {
        return Some(err);
    }
    codex_model_provider::named_sign_in_account(turn_context.provider.info())?;
    if turn_context.provider.auth().await.is_some() {
        return None;
    }
    turn_context.provider.api_auth().await.err()
}

/// Takes initial turn input and runs a loop where, at each sampling request,
/// the model replies with either:
///
/// - requested function calls
/// - an assistant message
///
/// While it is possible for the model to return multiple of these items in a
/// single sampling request, in practice, we generally one item per sampling request:
///
/// - If the model requests a function call, we execute it and send the output
///   back to the model in the next sampling request.
/// - If the model sends only an assistant message, we record it in the
///   conversation history and consider the turn complete.
///
pub(crate) async fn run_turn(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    turn_extension_data: Arc<codex_extension_api::ExtensionData>,
    input: Vec<TurnInput>,
    prewarmed_client_session: Option<ModelClientSession>,
    cancellation_token: CancellationToken,
) -> CodexResult<Option<String>> {
    let turn_started_at = Instant::now();
    trace_turn_timing("run_turn_start", turn_started_at);
    // Issue #445: with the kill switch on, the turn is refused before its
    // input is recorded, so nothing of it reaches the model later either.
    // #1331 (D171): so is a turn on a named account that was removed or
    // signed out meanwhile, so it is not recorded as a turn on that account.
    if let Some(err) = turn_refusal(sess.as_ref(), turn_context.as_ref()).await {
        sess.track_turn_codex_error(turn_context.as_ref(), &err);
        sess.emit_turn_error_lifecycle(turn_context.as_ref(), err.to_codex_protocol_error())
            .await;
        let event = EventMsg::Error(err.to_error_event(/*message_prefix*/ None));
        sess.send_event(&turn_context, event).await;
        return Ok(None);
    }
    let provider_info = turn_context.provider.info();
    let mut client_session = prewarmed_client_session
        .filter(|session| ModelClient::session_matches_provider(session, provider_info))
        .unwrap_or_else(|| {
            sess.services
                .model_client()
                .new_session_for_provider(provider_info)
        });
    // TODO(ccunningham): Pre-turn compaction runs before context updates and the
    // new user message are recorded. Estimate pending incoming items (context
    // diffs/full reinjection + user input) and trigger compaction preemptively
    // when they would push the thread over the compaction threshold.
    if let Err(err) = run_pre_sampling_compact(
        &sess,
        &turn_context,
        &mut client_session,
        &cancellation_token,
    )
    .await
    {
        if matches!(err.details(), CodexErrorDetails::TurnAborted) {
            run_hooks_and_record_inputs(&sess, &turn_context, &input).await;
            return Err(err);
        }
        let error = err.to_codex_protocol_error();
        sess.emit_turn_error_lifecycle(turn_context.as_ref(), error.clone())
            .await;
        error!("Failed to run pre-sampling compact");
        return Ok(None);
    }
    trace_turn_timing("after_pre_sampling_compact", turn_started_at);

    let user_input = turn_user_input(&input);
    let (required_servers, mentioned_plugins) =
        match required_mcp_servers_for_input(&sess, turn_context.as_ref(), &user_input)
            .or_cancel(&cancellation_token)
            .await
        {
            Ok(requirements) => requirements,
            Err(err) => {
                run_hooks_and_record_inputs(&sess, &turn_context, &input).await;
                return Err(err.into());
            }
        };

    // run_turn owns the step used to seed context and make the first sampling request.
    let first_step_context = match sess
        .capture_step_context_with_required_mcp_servers(
            Arc::clone(&turn_context),
            &cancellation_token,
            &required_servers,
        )
        .await
    {
        Ok(step_context) => step_context,
        Err(err) if matches!(err.details(), CodexErrorDetails::TurnAborted) => {
            run_hooks_and_record_inputs(&sess, &turn_context, &input).await;
            return Err(err);
        }
        Err(err) => return Err(err),
    };
    // Keep the exact model-visible state used by this turn and its inline compactions.
    let (world_state, display_roots) = tokio::join!(
        sess.record_context_updates_and_set_reference_context_item(first_step_context.as_ref()),
        turn_diff_display_roots(first_step_context.as_ref()),
    );
    let mut world_state = world_state?;

    let Some((injection_items, explicitly_enabled_connectors)) = build_skills_and_plugins(
        &sess,
        first_step_context.as_ref(),
        &user_input,
        &mentioned_plugins,
        &cancellation_token,
    )
    .await
    else {
        return Ok(None);
    };
    trace_turn_timing("after_build_skills_and_plugins", turn_started_at);

    if run_pending_session_start_hooks(&sess, &turn_context).await {
        return Ok(None);
    }
    trace_turn_timing("after_session_start_hooks", turn_started_at);
    let mut can_drain_pending_input = input.is_empty();
    if run_hooks_and_record_inputs(&sess, &turn_context, &input).await {
        return Ok(None);
    }
    trace_turn_timing("after_input_hooks", turn_started_at);

    sess.merge_connector_selection(explicitly_enabled_connectors.clone())
        .await;
    sess.set_previous_turn_settings(Some(PreviousTurnSettings {
        model: turn_context.model_info.slug.clone(),
        model_provider: Some(turn_context.config.model_provider_id.clone()),
        comp_hash: turn_context.model_info.comp_hash.clone(),
        realtime_active: Some(turn_context.realtime_active),
    }))
    .await;
    for response_item in injection_items {
        sess.record_conversation_items(&turn_context, std::slice::from_ref(&response_item))
            .await;
    }
    trace_turn_timing("after_injection_items", turn_started_at);

    track_turn_resolved_config_analytics(&sess, &turn_context, &input).await;
    trace_turn_timing("after_config_analytics", turn_started_at);

    let mut last_agent_message: Option<String> = None;
    let mut stop_hook_active = false;
    let mut consecutive_server_side_model_continuations = 0_u64;
    let completion_objective = objective_from_turn_input(&input);
    let mut completion_progress = CompletionProgressState::default();
    let mut pending_completion_continuation = false;
    let subagent_model_request_limit = turn_context.session_source.is_non_root_agent().then_some(
        turn_context
            .config
            .multi_agent_v2
            .max_subagent_model_requests_per_turn,
    );
    let mut model_request_count = 0_usize;
    // Although from the perspective of codex.rs, TurnDiffTracker has the lifecycle of a Task which contains
    // many turns, from the perspective of the user, it is a single turn.
    let turn_diff_tracker = Arc::new(tokio::sync::Mutex::new(
        TurnDiffTracker::with_environment_display_roots(display_roots),
    ));

    // `ModelClientSession` is turn-scoped and caches WebSocket + sticky routing state, so we reuse
    // one instance across retries within this turn.
    // Pending input is drained into history before building the next model request.
    // However, we defer that drain until after sampling in two cases:
    // 1. At the start of a turn, so the fresh turn input in `input` gets sampled first.
    // 2. After auto-compact, when model/tool continuation needs to resume before any steer.

    let mut next_step_context = Some(first_step_context);
    loop {
        if subagent_model_request_limit.is_some_and(|limit| model_request_count >= limit) {
            let limit = subagent_model_request_limit.unwrap_or_default();
            let message = format!(
                "Sub-agent turn stopped after the configured limit of {limit} model requests. \
                 Review the last reported progress and send a focused follow-up task if more work \
                 is required."
            );
            sess.send_event(
                &turn_context,
                EventMsg::Warning(WarningEvent {
                    message: message.clone(),
                }),
            )
            .await;
            last_agent_message.get_or_insert(message);
            break;
        }
        // Note that pending_input would be something like a message the user
        // submitted through the UI while the model was running. Though the UI
        // may support this, the model might not.
        let pending_input = if can_drain_pending_input {
            sess.input_queue
                .get_pending_input(&sess.active_turn)
                .await
                .0
        } else {
            Vec::new()
        };

        if run_hooks_and_record_inputs(&sess, &turn_context, &pending_input).await {
            break;
        }

        let window_id = sess.current_window_id().await;
        super::rollout_budget::maybe_record_reminder(
            sess.as_ref(),
            turn_context.as_ref(),
            &window_id,
        )
        .await;

        // Capture once so context, advertised tools, and tool calls share one request view.
        let step_context = match next_step_context.take() {
            Some(step_context) => step_context,
            None if pending_input.is_empty() => {
                sess.capture_step_context(Arc::clone(&turn_context), &cancellation_token)
                    .await?
            }
            None => {
                let pending_user_input = turn_user_input(&pending_input);
                let (required_servers, _) = required_mcp_servers_for_input(
                    &sess,
                    turn_context.as_ref(),
                    &pending_user_input,
                )
                .or_cancel(&cancellation_token)
                .await?;
                sess.capture_step_context_with_required_mcp_servers(
                    Arc::clone(&turn_context),
                    &cancellation_token,
                    &required_servers,
                )
                .await?
            }
        };
        let sampling_request_result: CodexResult<_> = async {
            super::time_reminder::maybe_record_current_time_reminder(
                sess.as_ref(),
                turn_context.as_ref(),
                &window_id,
            )
            .await?;

            world_state = sess
                .record_step_world_state_if_changed(&world_state, step_context.as_ref())
                .await?;

            // Construct the input that we will send to the model.
            let mut sampling_request_input: Vec<ResponseItem> =
                async { sess.clone_history().await.for_turn_prompt(&turn_context) }
                    .instrument(trace_span!("run_turn.prepare_sampling_request_input"))
                    .await;
            if std::mem::take(&mut pending_completion_continuation) {
                sampling_request_input
                    .push(ContextualUserFragment::into(TurnCompletionContinuation));
            }
            if subagent_model_request_limit
                .is_some_and(|limit| model_request_count.saturating_add(1) == limit)
            {
                sampling_request_input.push(ContextualUserFragment::into(
                    SubagentTurnBudgetFinalization {
                        max_model_requests: subagent_model_request_limit.unwrap_or_default(),
                    },
                ));
            }
            trace_turn_timing("after_prepare_sampling_request_input", turn_started_at);

            let responses_metadata = turn_context.turn_metadata_state.to_responses_metadata(
                sess.installation_id.clone(),
                window_id.clone(),
                CodexResponsesRequestKind::Turn,
            );
            model_request_count = model_request_count.saturating_add(1);
            run_sampling_request(
                Arc::clone(&sess),
                Arc::clone(&step_context),
                Arc::clone(&turn_extension_data),
                Arc::clone(&turn_diff_tracker),
                &mut client_session,
                &responses_metadata,
                sampling_request_input,
                cancellation_token.child_token(),
            )
            .await
        }
        .await;
        match sampling_request_result {
            Ok((sampling_request_output, sampling_request_input)) => {
                let SamplingRequestResult {
                    needs_follow_up: model_needs_follow_up,
                    last_agent_message: sampling_request_last_agent_message,
                    token_usage: _,
                    server_side_model_continuation,
                    made_tool_progress,
                    provider_stopped,
                } = sampling_request_output;
                if let Some(message) = sampling_request_last_agent_message.as_ref() {
                    last_agent_message = Some(message.clone());
                }
                if made_tool_progress {
                    completion_progress.note_tool_progress();
                }
                if server_side_model_continuation {
                    consecutive_server_side_model_continuations += 1;
                    if consecutive_server_side_model_continuations
                        > MAX_SERVER_SIDE_MODEL_CONTINUATIONS
                    {
                        return Err(CodexErr::Stream(format!(
                            "model requested more than {MAX_SERVER_SIDE_MODEL_CONTINUATIONS} \
                             server-side continuations in one turn"
                        )));
                    }
                } else {
                    consecutive_server_side_model_continuations = 0;
                }
                if model_needs_follow_up {
                    sess.input_queue
                        .accept_mailbox_delivery_for_current_turn(
                            &sess.active_turn,
                            &turn_context.sub_id,
                        )
                        .await;
                }
                can_drain_pending_input = true;
                let (has_pending_input, token_status) = async {
                    let has_pending_input =
                        sess.input_queue.has_pending_input(&sess.active_turn).await;
                    let token_status = super::context_window::context_window_token_status(
                        sess.as_ref(),
                        turn_context.as_ref(),
                    )
                    .await;
                    (has_pending_input, token_status)
                }
                .instrument(trace_span!("run_turn.collect_post_sampling_state"))
                .await;
                let needs_follow_up = model_needs_follow_up || has_pending_input;
                let token_limit_reached = token_status.token_limit_reached;

                trace!(
                    turn_id = %turn_context.sub_id,
                    total_usage_tokens = token_status.active_context_tokens,
                    auto_compact_scope_tokens = token_status.auto_compact_scope_tokens,
                    auto_compact_scope_limit = ?token_status.auto_compact_scope_limit,
                    auto_compact_limit_scope = ?turn_context.config.model_auto_compact_token_limit_scope,
                    auto_compact_window_prefill_tokens = ?token_status.auto_compact_window_prefill_tokens,
                    full_context_window_limit = ?token_status.full_context_window_limit,
                    full_context_window_limit_reached = token_status.full_context_window_limit_reached,
                    token_limit_reached,
                    model_needs_follow_up,
                    has_pending_input,
                    needs_follow_up,
                    "post sampling token usage"
                );
                if tracing::event_enabled!(
                    target: POST_SAMPLING_TOKEN_ESTIMATE_TARGET,
                    tracing::Level::TRACE,
                    turn_id,
                    estimated_token_count,
                    message
                ) {
                    let estimated_token_count =
                        sess.get_estimated_token_count(turn_context.as_ref()).await;
                    trace!(
                        target: POST_SAMPLING_TOKEN_ESTIMATE_TARGET,
                        turn_id = %turn_context.sub_id,
                        estimated_token_count = ?estimated_token_count,
                        "post sampling token estimate"
                    );
                }

                let should_roll_over = needs_follow_up
                    && (sess.take_new_context_window_request().await || token_limit_reached);
                let allow_auto_compact_fallback = !should_roll_over && !token_limit_reached;
                super::token_budget::maybe_record(
                    sess.as_ref(),
                    turn_context.as_ref(),
                    token_status.base_window_tokens_remaining,
                    allow_auto_compact_fallback,
                )
                .await;

                // as long as compaction works well in getting us way below the token limit, we shouldn't worry about being in an infinite loop.
                if should_roll_over {
                    if let Err(err) = run_auto_compact(
                        &sess,
                        Arc::clone(&step_context),
                        /*fallback_step_context*/ None,
                        &mut client_session,
                        InitialContextInjection::BeforeLastUserMessage {
                            world_state: Arc::clone(&world_state),
                            step_context: Arc::clone(&step_context),
                        },
                        CompactionReason::ContextLimit,
                        CompactionPhase::MidTurn,
                    )
                    .await
                    {
                        if matches!(err.details(), CodexErrorDetails::TurnAborted) {
                            return Err(err);
                        }
                        let error = err.to_codex_protocol_error();
                        sess.emit_turn_error_lifecycle(turn_context.as_ref(), error.clone())
                            .await;
                        return Ok(None);
                    }
                    if run_pending_session_start_hooks(&sess, &turn_context).await {
                        return Ok(None);
                    }
                    can_drain_pending_input = !model_needs_follow_up;
                    continue;
                }

                if !needs_follow_up
                    && provider_stopped
                    && turn_context.provider.info().chat_stop_semantics()
                        == ChatStopSemantics::AmbiguousForActionTurns
                {
                    let assistant_response = sampling_request_last_agent_message
                        .as_deref()
                        .filter(|response| !response.trim().is_empty());
                    let (action, decision_source, classifier_latency_ms) =
                        if let Some(assistant_response) = assistant_response {
                            let assessment_metadata =
                                turn_context.turn_metadata_state.to_responses_metadata(
                                    sess.installation_id.clone(),
                                    window_id.clone(),
                                    CodexResponsesRequestKind::Turn,
                                );
                            // Keep the semantic check independent of primary-stream teardown.
                            // Reusing the primary turn session can leave the classifier queued
                            // behind transport cleanup until most or all of its timeout is gone.
                            let mut assessment_client_session = sess
                                .services
                                .model_client()
                                .new_session_for_provider(turn_context.provider.info());
                            // The classifier is a second model request the
                            // operator paid for, on a session of its own, so it
                            // is collected like any other - under the turn it
                            // assesses. Best effort: an assessment that cannot
                            // be recorded still runs.
                            let _accounting = match crate::accounting::attach_turn(
                                &sess,
                                turn_context.as_ref(),
                                &assessment_client_session,
                                crate::accounting::assessment_turn_label(&turn_context.sub_id),
                            )
                            .await
                            {
                                Ok(scopes) => Some(scopes),
                                Err(error) => {
                                    tracing::warn!(
                                        %error,
                                        "accounting: completion assessment proceeding unrecorded"
                                    );
                                    None
                                }
                            };
                            let assessment_started_at = Instant::now();
                            match assess_turn_completion(
                                sess.as_ref(),
                                turn_context.as_ref(),
                                &mut assessment_client_session,
                                &assessment_metadata,
                                &completion_objective,
                                assistant_response,
                                &cancellation_token,
                            )
                            .await
                            {
                                Ok(assessment) => (
                                    completion_progress.decide(assessment, assistant_response),
                                    "classifier",
                                    assessment_started_at.elapsed().as_millis(),
                                ),
                                Err(err)
                                    if matches!(err.details(), CodexErrorDetails::TurnAborted) =>
                                {
                                    return Err(CodexErr::TurnAborted);
                                }
                                Err(err) => {
                                    warn!(
                                        turn_id = %turn_context.sub_id,
                                        error = %err,
                                        "turn completion assessment failed"
                                    );
                                    (
                                        completion_progress
                                            .fallback_after_failed_assessment(assistant_response),
                                        "fallback",
                                        assessment_started_at.elapsed().as_millis(),
                                    )
                                }
                            }
                        } else {
                            (completion_progress.decide_empty_stop(), "deterministic", 0)
                        };
                    info!(
                        turn_id = %turn_context.sub_id,
                        provider_id = %turn_context.config.model_provider_id,
                        finish_reason = "stop",
                        completion_decision = ?action,
                        decision_source,
                        classifier_latency_ms,
                        semantic_continuation_count =
                            completion_progress.continuation_count(),
                        consecutive_no_progress_count =
                            completion_progress.consecutive_no_progress_count(),
                        completion_reason = completion_progress.decision_reason(),
                        progress_reset_reason = completion_progress.progress_reset_reason(),
                        "resolved ambiguous provider stop"
                    );
                    match action {
                        CompletionAction::Accept => {}
                        CompletionAction::Continue => {
                            pending_completion_continuation = true;
                            continue;
                        }
                        CompletionAction::AcceptWithWarning => {
                            sess.send_event(
                                &turn_context,
                                EventMsg::Warning(WarningEvent {
                                    message: "Corbanu Terminal could not verify whether this action was complete; review the result before relying on it.".to_string(),
                                }),
                            )
                            .await;
                        }
                        CompletionAction::StopStalled => {
                            sess.send_event(
                                &turn_context,
                                EventMsg::Warning(WarningEvent {
                                    message: "Corbanu Terminal stopped automatic continuation because the model repeatedly ended without measurable progress. Review the result or continue manually.".to_string(),
                                }),
                            )
                            .await;
                        }
                    }
                }

                if !needs_follow_up {
                    if sampling_request_last_agent_message.is_some() {
                        last_agent_message = sampling_request_last_agent_message;
                    }
                    let stop_outcome = run_turn_stop_hooks(
                        &sess,
                        &turn_context,
                        stop_hook_active,
                        last_agent_message.clone(),
                    )
                    .await;
                    if stop_outcome.should_block {
                        if let Some(hook_prompt_message) =
                            build_hook_prompt_message(&stop_outcome.continuation_fragments)
                        {
                            sess.record_response_item_and_emit_turn_item(
                                &turn_context,
                                hook_prompt_message,
                            )
                            .await;
                            sess.input_queue
                                .accept_mailbox_delivery_for_current_turn(
                                    &sess.active_turn,
                                    &turn_context.sub_id,
                                )
                                .await;
                            stop_hook_active = true;
                            continue;
                        } else {
                            sess.send_event(
                                &turn_context,
                                EventMsg::Warning(WarningEvent {
                                    message: "Stop hook requested continuation without a prompt; ignoring the block.".to_string(),
                                }),
                            )
                            .await;
                        }
                    }
                    if stop_outcome.should_stop {
                        break;
                    }
                    if run_legacy_after_agent_hook(
                        &sess,
                        &turn_context,
                        &sampling_request_input,
                        last_agent_message.clone(),
                    )
                    .await
                    {
                        return Ok(None);
                    }
                    break;
                }
                continue;
            }
            Err(err) if matches!(err.details(), CodexErrorDetails::TurnAborted) => {
                return Err(err);
            }
            Err(codex_error)
                if matches!(
                    codex_error.details(),
                    CodexErrorDetails::InvalidImageRequest()
                ) =>
            {
                sess.track_turn_codex_error(turn_context.as_ref(), &codex_error);
                let error = CodexErrorInfo::BadRequest;
                sess.emit_turn_error_lifecycle(turn_context.as_ref(), error.clone())
                    .await;
                let event = EventMsg::Error(ErrorEvent {
                    message: "Invalid image in your last message. Please remove it and try again."
                        .to_string(),
                    codex_error_info: Some(error),
                });
                sess.send_event(&turn_context, event).await;
                break;
            }
            Err(e) => {
                info!("Turn error: {e:#}");
                let error = e.to_codex_protocol_error();
                sess.emit_turn_error_lifecycle(turn_context.as_ref(), error.clone())
                    .await;
                sess.track_turn_codex_error(turn_context.as_ref(), &e);
                let event = EventMsg::Error(e.to_error_event(/*message_prefix*/ None));
                sess.send_event(&turn_context, event).await;
                // let the user continue the conversation
                break;
            }
        }
    }

    Ok(last_agent_message)
}

pub(crate) fn turn_inputs_are_terminal_result_only<'a>(
    input: impl Iterator<Item = &'a TurnInput>,
) -> bool {
    let mut saw_terminal_result = false;
    for item in input {
        match item {
            TurnInput::InterAgentCommunication(communication)
                if communication.kind == Some(AgentMessageKind::TerminalResult) =>
            {
                saw_terminal_result = true;
            }
            TurnInput::ResponseItem(_) => {}
            TurnInput::UserInput { .. } | TurnInput::InterAgentCommunication(_) => return false,
        }
    }
    saw_terminal_result
}

pub(crate) fn turn_inputs_expect_parent_completion<'a>(
    input: impl Iterator<Item = &'a TurnInput>,
) -> bool {
    let mut saw_triggering_collaboration_work = false;
    for item in input {
        match item {
            TurnInput::UserInput { .. } => return false,
            TurnInput::InterAgentCommunication(communication) if communication.trigger_turn => {
                saw_triggering_collaboration_work = true;
            }
            TurnInput::ResponseItem(_) | TurnInput::InterAgentCommunication(_) => {}
        }
    }
    saw_triggering_collaboration_work
}

#[instrument(level = "trace", skip_all)]
async fn turn_diff_display_roots(step_context: &StepContext) -> Vec<(String, PathUri)> {
    let mut display_roots = Vec::new();
    for turn_environment in step_context.environments.turn_environments() {
        let cwd = turn_environment.cwd();
        // A turn cwd is expected to be a directory. If it is a file, the failed `<cwd>/.git` probe
        // is ignored and ancestor search continues from its parent.
        let root = find_nearest_ancestor_with_markers(
            turn_environment.environment.get_filesystem().as_ref(),
            cwd,
            vec![".git".to_string()],
            FindUpErrorPolicy::Ignore,
            /*sandbox*/ None,
        )
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| cwd.clone());
        display_roots.push((turn_environment.environment_id.clone(), root));
    }
    display_roots
}

#[instrument(level = "trace", skip_all)]
pub(crate) async fn run_hooks_and_record_inputs(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    input: &[TurnInput],
) -> bool {
    let mut blocked_input = false;
    let mut accepted_user_input = false;
    for input_item in input {
        let hook_outcome = inspect_pending_input(sess, turn_context, input_item).await;
        if hook_outcome.should_stop {
            blocked_input = true;
            // A blocked prompt is never recorded: drop any agent mark for it.
            if let TurnInput::UserInput { content, .. } = input_item {
                sess.services.model_client().discard_agent_input(content);
            }
            record_additional_contexts(sess, turn_context, hook_outcome.additional_contexts).await;
        } else {
            if matches!(input_item, TurnInput::UserInput { content, .. } if !content.is_empty()) {
                accepted_user_input = true;
            }
            record_explicit_tool_budgets_from_input(turn_context, input_item);
            record_pending_input(
                sess,
                turn_context,
                input_item.clone(),
                hook_outcome.additional_contexts,
            )
            .await;
        }
    }
    blocked_input && !accepted_user_input
}
fn record_explicit_tool_budgets_from_input(turn_context: &TurnContext, input_item: &TurnInput) {
    let TurnInput::UserInput { content, .. } = input_item else {
        return;
    };
    if let Some(limit) = explicit_shell_command_budget_from_user_input(content) {
        turn_context.set_explicit_shell_command_budget(limit);
    }
}

fn explicit_shell_command_budget_from_user_input(content: &[UserInput]) -> Option<u64> {
    content
        .iter()
        .filter_map(|input| match input {
            UserInput::Text { text, .. } => explicit_shell_command_budget_from_text(text),
            UserInput::Image { .. }
            | UserInput::LocalImage { .. }
            | UserInput::Skill { .. }
            | UserInput::Mention { .. } => None,
            _ => None,
        })
        .min()
}

fn explicit_shell_command_budget_from_text(text: &str) -> Option<u64> {
    let lower = text.to_ascii_lowercase();
    [
        r"\bat most\s+([0-9]{1,3})\s+(shell\s+)?commands?\b",
        r"\bno more than\s+([0-9]{1,3})\s+(shell\s+)?commands?\b",
        r"\bmaximum( of)?\s+([0-9]{1,3})\s+(shell\s+)?commands?\b",
        r"\bmax\s+([0-9]{1,3})\s+(shell\s+)?commands?\b",
        r"\buse\s+([0-9]{1,3})\s+or fewer\s+(shell\s+)?commands?\b",
    ]
    .into_iter()
    .filter_map(|pattern| parse_budget_match(pattern, &lower))
    .filter(|limit| *limit > 0)
    .min()
}

fn parse_budget_match(pattern: &str, text: &str) -> Option<u64> {
    let regex = Regex::new(pattern).ok()?;
    let captures = regex.captures(text)?;
    captures
        .iter()
        .flatten()
        .filter_map(|matched| matched.as_str().parse::<u64>().ok())
        .next()
}

fn turn_user_input(input: &[TurnInput]) -> Vec<UserInput> {
    input
        .iter()
        .filter_map(|item| match item {
            TurnInput::UserInput { content, .. } => Some(content.as_slice()),
            TurnInput::ResponseItem(_) | TurnInput::InterAgentCommunication(_) => None,
        })
        .flatten()
        .cloned()
        .collect()
}

async fn required_mcp_servers_for_input(
    sess: &Arc<Session>,
    turn_context: &TurnContext,
    user_input: &[UserInput],
) -> (Vec<String>, Vec<crate::plugins::PluginCapabilitySummary>) {
    if crate::guardian::is_guardian_reviewer_source(&turn_context.session_source) {
        return (Vec::new(), Vec::new());
    }

    // Plugin capabilities depend on authentication, so project them only after
    // the runtime has aligned the plugin manager with its current account.
    sess.refresh_mcp_if_dirty().await;
    let loaded_plugins = sess
        .services
        .plugins_manager
        .plugins_for_config(&turn_context.config.plugins_config_input())
        .await;
    let current_config = sess.services.mcp_runtime.current_config();
    let mentioned_plugins =
        collect_explicit_plugin_mentions(user_input, loaded_plugins.capability_summaries());
    let mut required_servers = mentioned_plugins
        .iter()
        .flat_map(|plugin| plugin.mcp_server_names.iter().cloned())
        .collect::<HashSet<_>>();

    let messages = user_input
        .iter()
        .filter_map(|input| match input {
            UserInput::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mentions = collect_tool_mentions_from_messages(&messages);
    let paths = user_input
        .iter()
        .filter_map(|input| match input {
            UserInput::Mention { path, .. } => Some(path.clone()),
            _ => None,
        })
        .chain(mentions.paths);
    required_servers.extend(paths.filter_map(|path| {
        path.strip_prefix("mcp://")
            .filter(|server| !server.is_empty())
            .map(str::to_string)
    }));

    let connector_slug_counts = if turn_context.apps_enabled() && !mentions.plain_names.is_empty() {
        let cached_connectors =
            connectors::list_cached_accessible_connectors_from_mcp_tools(&turn_context.config)
                .await;
        let accessible_connectors = match cached_connectors {
            Some(connectors) => connectors,
            None => sess
                .services
                .mcp_runtime
                .current_binding()
                .await
                .map(|binding| connectors::accessible_connectors_from_mcp_tools(binding.tools()))
                .unwrap_or_default(),
        };
        let connector_ids = current_config
            .iter()
            .flat_map(|config| config.connector_snapshot.connector_ids())
            .map(|connector_id| connector_id.0.clone());
        build_connector_slug_counts(
            &codex_connectors::merge::merge_plugin_connectors_with_accessible(
                connector_ids,
                accessible_connectors,
            ),
        )
    } else {
        HashMap::new()
    };
    let skills_outcome = turn_context.turn_skills.snapshot.outcome();
    let mentioned_skills = collect_explicit_skill_mentions(
        user_input,
        &skills_outcome.skills,
        &skills_outcome.disabled_paths,
        &connector_slug_counts,
    );
    for skill in mentioned_skills {
        if let Some(dependencies) = skill.dependencies {
            required_servers.extend(
                dependencies
                    .tools
                    .into_iter()
                    .filter(|tool| tool.r#type.eq_ignore_ascii_case("mcp"))
                    .map(|tool| tool.value),
            );
        }
        if let Some(plugin_id) = skill.plugin_id.as_deref()
            && let Some(plugin) = loaded_plugins
                .capability_summaries()
                .iter()
                .find(|plugin| plugin.config_name == plugin_id)
        {
            required_servers.extend(plugin.mcp_server_names.iter().cloned());
        }
    }

    (required_servers.into_iter().collect(), mentioned_plugins)
}

#[instrument(level = "trace", skip_all)]
async fn build_skills_and_plugins(
    sess: &Arc<Session>,
    step_context: &StepContext,
    user_input: &[UserInput],
    mentioned_plugins: &[crate::plugins::PluginCapabilitySummary],
    cancellation_token: &CancellationToken,
) -> Option<(Vec<ResponseItem>, HashSet<String>)> {
    let turn_context = step_context.turn.as_ref();
    // Guardian input embeds the parent transcript as untrusted evidence. Do not interpret skill or
    // plugin mentions from that generated prompt as requests to inject additional instructions.
    if crate::guardian::is_guardian_reviewer_source(&turn_context.session_source) {
        return Some((Vec::new(), HashSet::new()));
    }

    let tracking = build_track_events_context(
        turn_context.model_info.slug.clone(),
        sess.thread_id.to_string(),
        turn_context.sub_id.clone(),
        turn_context.originator.clone(),
    );
    let connector_snapshot = step_context.mcp.config().connector_snapshot.clone();
    let mcp_tools = if turn_context.apps_enabled() || !mentioned_plugins.is_empty() {
        // Plugin mentions need raw MCP/app inventory even when app tools
        // are normally hidden so we can describe the plugin's currently
        // usable capabilities for this turn.
        step_context.mcp.tools().to_vec()
    } else {
        Vec::new()
    };
    let available_connectors = if turn_context.apps_enabled() {
        let connectors = codex_connectors::merge::merge_plugin_connectors_with_accessible(
            connector_snapshot
                .connector_ids()
                .iter()
                .map(|connector_id| connector_id.0.clone()),
            connectors::accessible_connectors_from_mcp_tools(&mcp_tools),
        );
        connectors::with_app_enabled_state(connectors, &turn_context.config)
    } else {
        Vec::new()
    };
    let skills_outcome = turn_context.turn_skills.snapshot.outcome();
    let connector_slug_counts = build_connector_slug_counts(&available_connectors);
    let extension_injection_items =
        build_extension_turn_input_items(sess, step_context, user_input, cancellation_token)
            .await?;
    let skill_name_counts_lower =
        build_skill_name_counts(&skills_outcome.skills, &skills_outcome.disabled_paths).1;
    let mentioned_skills = collect_explicit_skill_mentions(
        user_input,
        &skills_outcome.skills,
        &skills_outcome.disabled_paths,
        &connector_slug_counts,
    );
    maybe_prompt_and_install_mcp_dependencies(
        sess,
        turn_context,
        cancellation_token,
        &mentioned_skills,
        Some(sess.mcp_elicitation_reviewer()),
    )
    .await;

    let injected_host_skill_prompts = turn_context
        .extension_data
        .get::<InjectedHostSkillPrompts>();
    let SkillInjections {
        items: skill_injections,
        warnings: skill_warnings,
    } = build_skill_injections(
        &mentioned_skills,
        Some(skills_outcome),
        Some(&turn_context.session_telemetry),
        &sess.services.analytics_events_client,
        tracking.clone(),
    )
    .await;

    for message in skill_warnings {
        sess.send_event(turn_context, EventMsg::Warning(WarningEvent { message }))
            .await;
    }

    let skill_items: Vec<ResponseItem> = skill_injections
        .iter()
        .map(|skill| ContextualUserFragment::into(crate::context::SkillInstructions::from(skill)))
        .collect();
    let skill_connector_ids = collect_explicit_app_ids_from_skill_items(
        &skill_items,
        &available_connectors,
        &skill_name_counts_lower,
    );
    let plugin_items =
        build_plugin_injections(mentioned_plugins, &mcp_tools, &available_connectors);
    let mut explicitly_enabled_connectors = collect_explicit_app_ids(user_input);
    explicitly_enabled_connectors.extend(skill_connector_ids);
    let connector_names_by_id = available_connectors
        .iter()
        .map(|connector| (connector.id.as_str(), connector.name.as_str()))
        .collect::<HashMap<&str, &str>>();
    let mentioned_app_invocations = explicitly_enabled_connectors
        .iter()
        .map(|connector_id| AppInvocation {
            connector_id: Some(connector_id.clone()),
            app_name: connector_names_by_id
                .get(connector_id.as_str())
                .map(|name| (*name).to_string()),
            invocation_type: Some(InvocationType::Explicit),
        })
        .collect::<Vec<_>>();
    sess.services
        .analytics_events_client
        .track_app_mentioned(tracking.clone(), mentioned_app_invocations);
    for summary in mentioned_plugins {
        if let Some(plugin) = sess
            .services
            .plugins_manager
            .telemetry_metadata_for_capability_summary(summary)
        {
            sess.services
                .analytics_events_client
                .track_plugin_used(tracking.clone(), plugin);
        }
    }

    let mut injection_items: Vec<ResponseItem> = match injected_host_skill_prompts {
        Some(injected_host_skill_prompts) => skill_injections
            .iter()
            .filter(|skill| !injected_host_skill_prompts.contains_path(&skill.path))
            .map(|skill| {
                ContextualUserFragment::into(crate::context::SkillInstructions::from(skill))
            })
            .collect(),
        None => skill_items,
    };
    injection_items.extend(plugin_items);
    injection_items.extend(extension_injection_items);
    Some((injection_items, explicitly_enabled_connectors))
}

#[tracing::instrument(
    level = "trace",
    skip_all,
    fields(user_input_count = user_input.len())
)]
async fn build_extension_turn_input_items(
    sess: &Arc<Session>,
    step_context: &StepContext,
    user_input: &[UserInput],
    cancellation_token: &CancellationToken,
) -> Option<Vec<ResponseItem>> {
    let turn_context = step_context.turn.as_ref();
    let contributors = sess.services.extensions.turn_input_contributors().to_vec();
    if contributors.is_empty() {
        return Some(Vec::new());
    }

    let environments = turn_context
        .environments
        .turn_environments()
        .enumerate()
        .filter_map(|(index, environment)| {
            // TODO(anp): Migrate extension turn-input environments to PathUri so foreign cwd
            // values are not omitted from extension context.
            Some(TurnInputEnvironment {
                environment_id: environment.environment_id.clone(),
                cwd: environment.cwd().to_abs_path().ok()?.into_path_buf(),
                is_primary: index == 0,
            })
        })
        .collect::<Vec<_>>();

    let input = TurnInputContext {
        turn_id: turn_context.sub_id.to_string(),
        user_input: user_input.to_vec(),
        environments,
    };
    let extension_metrics =
        super::extension_metrics::from_session_telemetry(turn_context.session_telemetry.clone());

    let mut items = Vec::new();
    for contributor in contributors {
        let contributed_fragments = contributor
            .contribute(
                input.clone(),
                Some(Arc::clone(&extension_metrics)),
                &sess.services.session_extension_data,
                &sess.services.thread_extension_data,
                turn_context.extension_data.as_ref(),
            )
            .or_cancel(cancellation_token)
            .await
            .ok()?;
        items.extend(
            contributed_fragments
                .into_iter()
                .map(ContextualUserFragment::into_boxed_response_item),
        );
    }

    Some(items)
}

#[tracing::instrument(
    level = "trace",
    skip_all,
    fields(input_count = input.len())
)]
async fn track_turn_resolved_config_analytics(
    sess: &Session,
    turn_context: &TurnContext,
    input: &[TurnInput],
) {
    let thread_config = {
        let state = sess.state.lock().await;
        state.session_configuration.thread_config_snapshot()
    };
    let is_first_turn = {
        let mut state = sess.state.lock().await;
        state.take_next_turn_is_first()
    };
    sess.services
        .analytics_events_client
        .track_turn_resolved_config(TurnResolvedConfigFact {
            turn_id: turn_context.sub_id.clone(),
            thread_id: sess.thread_id.to_string(),
            num_input_images: input
                .iter()
                .filter_map(|item| match item {
                    TurnInput::UserInput { content, .. } => Some(content.as_slice()),
                    TurnInput::ResponseItem(_) | TurnInput::InterAgentCommunication(_) => None,
                })
                .flatten()
                .filter(|item| {
                    matches!(item, UserInput::Image { .. } | UserInput::LocalImage { .. })
                })
                .count(),
            submission_type: None,
            ephemeral: thread_config.ephemeral,
            session_source: thread_config.session_source,
            model: turn_context.model_info.slug.clone(),
            model_provider: turn_context.config.model_provider_id.clone(),
            permission_profile: turn_context.permission_profile(),
            #[allow(deprecated)]
            permission_profile_cwd: turn_context.cwd.to_path_buf(),
            reasoning_effort: turn_context.reasoning_effort.clone(),
            reasoning_summary: Some(turn_context.reasoning_summary),
            service_tier: turn_context
                .config
                .service_tier
                .as_deref()
                .and_then(ServiceTier::from_request_value),
            approval_policy: turn_context.approval_policy.value(),
            approvals_reviewer: turn_context.config.approvals_reviewer,
            sandbox_network_access: turn_context.network_sandbox_policy().is_enabled(),
            collaboration_mode: turn_context.mode,
            personality: turn_context.personality,
            workspace_kind: turn_context.turn_metadata_state.workspace_kind(),
            is_first_turn,
        });
}

#[instrument(level = "trace", skip_all)]
async fn run_pre_sampling_compact(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    client_session: &mut ModelClientSession,
    cancellation_token: &CancellationToken,
) -> CodexResult<()> {
    maybe_run_previous_model_inline_compact(sess, turn_context, client_session, cancellation_token)
        .await?;
    let token_status =
        super::context_window::context_window_token_status(sess.as_ref(), turn_context.as_ref())
            .await;
    // Compact if the configured auto-compaction budget or usable context window is exhausted.
    if token_status.token_limit_reached {
        // Pre-turn compaction runs before run_turn creates the normal sampling step.
        let step_context = sess
            .capture_step_context(Arc::clone(turn_context), cancellation_token)
            .await?;
        run_auto_compact(
            sess,
            step_context,
            /*fallback_step_context*/ None,
            client_session,
            InitialContextInjection::DoNotInject,
            CompactionReason::ContextLimit,
            CompactionPhase::PreTurn,
        )
        .await?;
    }
    Ok(())
}

/// Returns true only when both turns declare compaction compatibility hashes and they differ.
/// A missing hash does not provide enough information to trigger compaction.
fn comp_hash_changed(previous: Option<&str>, current: Option<&str>) -> bool {
    previous
        .zip(current)
        .is_some_and(|(previous, current)| previous != current)
}

/// Captures the current model's request-scoped state for retrying previous-model compaction.
///
/// Returns `None` when the active authentication does not use the Codex backend, the provider is
/// not OpenAI, or the previous and current model are the same.
async fn capture_current_model_fallback_step_context(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    previous_model: &str,
    cancellation_token: &CancellationToken,
) -> CodexResult<Option<Arc<StepContext>>> {
    let uses_codex_backend = turn_context
        .auth_manager
        .as_deref()
        .is_some_and(codex_login::AuthManager::current_auth_uses_codex_backend);
    if !uses_codex_backend
        || !turn_context.provider.info().is_openai()
        || previous_model == turn_context.model_info.slug
    {
        return Ok(None);
    }
    sess.capture_step_context(Arc::clone(turn_context), cancellation_token)
        .await
        .map(Some)
}

/// Runs pre-sampling compaction against the previous model when its compaction compatibility
/// hash changed or when switching to a smaller context-window model.
///
/// Returns `Err(_)` only when compaction was attempted and failed.
async fn maybe_run_previous_model_inline_compact(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    client_session: &mut ModelClientSession,
    cancellation_token: &CancellationToken,
) -> CodexResult<()> {
    let Some(previous_turn_settings) = sess.previous_turn_settings().await else {
        return Ok(());
    };
    let should_compact_for_comp_hash_change = comp_hash_changed(
        previous_turn_settings.comp_hash.as_deref(),
        turn_context.model_info.comp_hash.as_deref(),
    );
    let previous_model = previous_turn_settings.model;
    let previous_model_turn_context = Arc::new(
        turn_context
            .with_model(previous_model.clone(), &sess.services.models_manager)
            .await,
    );

    if should_compact_for_comp_hash_change {
        let step_context = sess
            .capture_step_context(Arc::clone(&previous_model_turn_context), cancellation_token)
            .await?;
        let fallback_step_context = capture_current_model_fallback_step_context(
            sess,
            turn_context,
            previous_model.as_str(),
            cancellation_token,
        )
        .await?;
        run_auto_compact(
            sess,
            step_context,
            fallback_step_context,
            client_session,
            InitialContextInjection::DoNotInject,
            CompactionReason::CompHashChanged,
            CompactionPhase::PreTurn,
        )
        .await?;
        return Ok(());
    }

    let Some(old_context_window) = previous_model_turn_context.model_context_window() else {
        return Ok(());
    };
    let Some(new_context_window) = turn_context.model_context_window() else {
        return Ok(());
    };
    let active_context_tokens = sess.get_total_token_usage().await;
    let previous_model_limit_reached = match turn_context
        .config
        .model_auto_compact_token_limit_scope
    {
        AutoCompactTokenLimitScope::Total => {
            let new_auto_compact_limit = turn_context
                .model_info
                .auto_compact_token_limit()
                .unwrap_or(i64::MAX);
            active_context_tokens > new_auto_compact_limit
                || active_context_tokens >= new_context_window
        }
        AutoCompactTokenLimitScope::BodyAfterPrefix => active_context_tokens >= new_context_window,
    };
    let should_run = previous_model_limit_reached
        && previous_model_turn_context.model_info.slug != turn_context.model_info.slug
        && old_context_window > new_context_window;
    if should_run {
        let step_context = sess
            .capture_step_context(Arc::clone(&previous_model_turn_context), cancellation_token)
            .await?;
        let fallback_step_context = capture_current_model_fallback_step_context(
            sess,
            turn_context,
            previous_model.as_str(),
            cancellation_token,
        )
        .await?;
        run_auto_compact(
            sess,
            step_context,
            fallback_step_context,
            client_session,
            InitialContextInjection::DoNotInject,
            CompactionReason::ModelDownshift,
            CompactionPhase::PreTurn,
        )
        .await?;
    }
    Ok(())
}

#[instrument(
    level = "trace",
    skip_all,
    fields(reason = ?reason, phase = ?phase)
)]
async fn run_auto_compact(
    sess: &Arc<Session>,
    step_context: Arc<StepContext>,
    fallback_step_context: Option<Arc<StepContext>>,
    client_session: &mut ModelClientSession,
    initial_context_injection: InitialContextInjection,
    reason: CompactionReason,
    phase: CompactionPhase,
) -> CodexResult<()> {
    let turn_context = &step_context.turn;
    let _profile_guard = turn_context.turn_timing_state.begin_compaction();
    if turn_context.config.features.enabled(Feature::TokenBudget) {
        // Compaction is the reset request, so force a new context window
        // instead of consuming a pending `new_context` tool request.
        crate::compact_token_budget::run_inline_auto_compact_task(
            Arc::clone(sess),
            step_context,
            initial_context_injection,
        )
        .await?;
        return Ok(());
    }

    if should_use_remote_compact_task(turn_context.provider.info()) {
        if turn_context
            .config
            .features
            .enabled(Feature::RemoteCompactionV2)
        {
            emit_compact_metric(
                &sess.services.session_telemetry,
                "remote_v2",
                /*manual*/ false,
            );
            run_inline_remote_auto_compact_task_v2(
                Arc::clone(sess),
                step_context,
                fallback_step_context,
                client_session,
                initial_context_injection,
                reason,
                phase,
            )
            .await?;
            return Ok(());
        }
        emit_compact_metric(
            &sess.services.session_telemetry,
            "remote",
            /*manual*/ false,
        );
        run_inline_remote_auto_compact_task(
            Arc::clone(sess),
            step_context,
            fallback_step_context,
            client_session.turn_state(),
            initial_context_injection,
            reason,
            phase,
        )
        .await?;
    } else {
        emit_compact_metric(
            &sess.services.session_telemetry,
            "local",
            /*manual*/ false,
        );
        run_inline_auto_compact_task(
            Arc::clone(sess),
            Arc::clone(turn_context),
            initial_context_injection,
            reason,
            phase,
        )
        .await?;
    }
    Ok(())
}

pub(super) fn collect_explicit_app_ids_from_skill_items(
    skill_items: &[ResponseItem],
    connectors: &[connectors::AppInfo],
    skill_name_counts_lower: &HashMap<String, usize>,
) -> HashSet<String> {
    if skill_items.is_empty() || connectors.is_empty() {
        return HashSet::new();
    }

    let skill_messages = skill_items
        .iter()
        .filter_map(|item| match item {
            ResponseItem::Message { content, .. } => {
                content.iter().find_map(|content_item| match content_item {
                    ContentItem::InputText { text } => Some(text.clone()),
                    _ => None,
                })
            }
            _ => None,
        })
        .collect::<Vec<String>>();
    if skill_messages.is_empty() {
        return HashSet::new();
    }

    let mentions = collect_tool_mentions_from_messages(&skill_messages);
    let mention_names_lower = mentions
        .plain_names
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .collect::<HashSet<String>>();
    let mut connector_ids = mentions
        .paths
        .iter()
        .filter(|path| tool_kind_for_path(path) == ToolMentionKind::App)
        .filter_map(|path| app_id_from_path(path).map(str::to_string))
        .collect::<HashSet<String>>();

    let connector_slug_counts = build_connector_slug_counts(connectors);
    for connector in connectors {
        let slug = codex_connectors::metadata::connector_mention_slug(connector);
        let connector_count = connector_slug_counts.get(&slug).copied().unwrap_or(0);
        let skill_count = skill_name_counts_lower.get(&slug).copied().unwrap_or(0);
        if connector_count == 1 && skill_count == 0 && mention_names_lower.contains(&slug) {
            connector_ids.insert(connector.id.clone());
        }
    }

    connector_ids
}

#[instrument(level = "trace", skip_all)]
pub(crate) fn build_prompt(
    input: Vec<ResponseItem>,
    router: &ToolRouter,
    turn_context: &TurnContext,
    base_instructions: BaseInstructions,
) -> Prompt {
    Prompt {
        input,
        tools: router.model_visible_specs(),
        parallel_tool_calls: turn_context.model_info.supports_parallel_tool_calls,
        base_instructions,
        output_schema: turn_context.final_output_json_schema.clone(),
        output_schema_strict: !crate::guardian::is_guardian_reviewer_source(
            &turn_context.session_source,
        ),
        producer: Some(turn_context.output_producer()),
        flat_tool_names: router.flat_tool_names().clone(),
    }
}

/// Tells the user, once per switch, that requests are billed to the
/// `OPENAI_API_KEY` from their environment. Only providers that use OpenAI
/// sign-in send that key. Root threads and spawned agents (which may use a
/// different provider from their parent) announce in their own thread. Review
/// and compaction sub-sessions and internal sessions stay quiet: the review and
/// compact tasks announce on the thread that started them.
pub(crate) async fn notify_openai_api_key_env_fallback(sess: &Session, turn_context: &TurnContext) {
    use codex_protocol::protocol::SessionSource;
    use codex_protocol::protocol::SubAgentSource;
    if matches!(
        turn_context.session_source,
        SessionSource::Internal(_)
            | SessionSource::SubAgent(
                SubAgentSource::Review
                    | SubAgentSource::Compact
                    | SubAgentSource::MemoryConsolidation
                    | SubAgentSource::Other(_)
            )
    ) {
        return;
    }
    let in_use = turn_context.provider.info().uses_first_party_openai_auth()
        && turn_context
            .provider
            .auth()
            .await
            .is_some_and(|auth| auth.is_openai_api_key_env_fallback());
    let newly_in_use = sess
        .state
        .lock()
        .await
        .note_openai_api_key_env_fallback(in_use);
    if newly_in_use {
        sess.send_event(
            turn_context,
            EventMsg::Warning(WarningEvent {
                message: codex_login::OPENAI_API_KEY_ENV_FALLBACK_NOTICE.to_string(),
            }),
        )
        .await;
    }
}

/// #578: retries of one request without an item the provider reported
/// missing.
const MAX_UNRESOLVABLE_ITEM_RETRIES: usize = 8;

/// #578: whether to send a failed request again without an item a stateless
/// Responses host reported missing (`Item with id '…' not found`). Each ID is
/// retried once, at most [`MAX_UNRESOLVABLE_ITEM_RETRIES`] per request; the
/// client leaves the ID out of its later requests.
pub(crate) fn retry_without_unresolvable_item(
    client_session: &ModelClientSession,
    err: &CodexErr,
    retries: &mut usize,
) -> bool {
    if *retries >= MAX_UNRESOLVABLE_ITEM_RETRIES {
        return false;
    }
    let Some(item_id) = client_session.note_unresolvable_item_reference(err) else {
        return false;
    };
    *retries += 1;
    warn!(
        item_id,
        "the provider could not find an item this request referenced; retrying without it"
    );
    true
}

#[allow(clippy::too_many_arguments)]
#[allow(deprecated)]
#[instrument(level = "trace",
    skip_all,
    fields(
        turn_id = %step_context.turn.sub_id,
        model = %step_context.turn.model_info.slug,
        cwd = %step_context.turn.cwd.display()
    )
)]
async fn run_sampling_request(
    sess: Arc<Session>,
    step_context: Arc<StepContext>,
    turn_store: Arc<codex_extension_api::ExtensionData>,
    turn_diff_tracker: SharedTurnDiffTracker,
    client_session: &mut ModelClientSession,
    responses_metadata: &CodexResponsesMetadata,
    input: Vec<ResponseItem>,
    cancellation_token: CancellationToken,
) -> CodexResult<(SamplingRequestResult, Vec<ResponseItem>)> {
    let sampling_started_at = Instant::now();
    let turn_context = Arc::clone(&step_context.turn);
    let router = Arc::clone(&step_context.tool_router);
    notify_openai_api_key_env_fallback(&sess, &turn_context).await;

    // Accounting observes this turn's requests; it never stops one. A turn whose
    // collection cannot attach, or fails later, runs unrecorded and says so.
    let scopes = match crate::accounting::attach_turn(
        &sess,
        &turn_context,
        client_session,
        turn_context.sub_id.clone(),
    )
    .await
    {
        Ok(scopes) => Some(scopes),
        Err(error) => {
            crate::accounting::gap("attach turn", error);
            None
        }
    };

    let base_instructions = sess.get_base_instructions().await;
    trace_turn_timing("after_get_base_instructions", sampling_started_at);

    let tool_runtime = ToolCallRuntime::new(
        Arc::clone(&router),
        Arc::clone(&sess),
        Arc::clone(&step_context),
        Arc::clone(&turn_diff_tracker),
    );
    let _code_mode_worker = sess.services.code_mode_service.start_turn_worker(
        &sess,
        Arc::clone(&step_context),
        Arc::clone(&router),
        Arc::clone(&turn_diff_tracker),
    );
    let max_retries = turn_context.provider.info().stream_max_retries();
    let mut retries = 0;
    let mut same_request_idle_failures = 0;
    let mut unresolvable_item_retries = 0;
    let mut home_wait = crate::home_access::HomeWait::default();
    let mut initial_input = Some(input);
    let mut original_input = None;
    let mut executed_tool_calls_by_output = HashMap::new();
    loop {
        let prompt_input = if let Some(input) = initial_input.take() {
            input
        } else {
            sess.clone_history().await.for_turn_prompt(&turn_context)
        };
        let mut prompt_input = prompt_input;
        if let Some(executed_tool_calls) = sess.services.executed_tool_calls.as_ref()
            && executed_tool_calls
                .attach_pending_to_prompt(&mut prompt_input, &mut executed_tool_calls_by_output)
        {
            codex_protocol::models::bound_executed_tool_calls_for_prompt(&mut prompt_input);
        }
        let prompt = build_prompt(
            prompt_input,
            router.as_ref(),
            turn_context.as_ref(),
            base_instructions.clone(),
        );
        trace_turn_timing("after_build_prompt", sampling_started_at);
        let same_turn_attempt_index = retries + 1;
        ensure_gpu_runtime_provider_active(&sess, &turn_context).await?;
        let attempt_started_at = Instant::now();
        let attempt_result = try_run_sampling_request(
            tool_runtime.clone(),
            Arc::clone(&sess),
            Arc::clone(&turn_context),
            Arc::clone(&turn_store),
            client_session,
            responses_metadata,
            Arc::clone(&turn_diff_tracker),
            &prompt,
            same_turn_attempt_index,
            cancellation_token.child_token(),
        )
        .await;
        let attempt_elapsed = attempt_started_at.elapsed();
        if scopes
            .as_ref()
            .is_some_and(crate::accounting::TurnScopes::halted)
        {
            return Err(CodexErr::Fatal(crate::accounting::MEMORY_DENIAL.into()));
        }
        crate::accounting::warn_if_unrecorded(&sess, &turn_context, scopes.as_ref()).await;
        let err = match attempt_result {
            Ok(output) => {
                return Ok((output, original_input.unwrap_or(prompt.input)));
            }
            Err(err) => match err.details() {
                CodexErrorDetails::ContextWindowExceeded => {
                    sess.set_total_tokens_full(&turn_context).await;
                    return Err(err);
                }
                CodexErrorDetails::UsageLimitReached(e) => {
                    let rate_limits = e.rate_limits.clone();
                    if let Some(rate_limits) = rate_limits {
                        sess.update_rate_limits(&turn_context, *rate_limits).await;
                    }
                    return Err(err);
                }
                _ => err,
            },
        };

        if original_input.is_none() {
            original_input = Some(prompt.input);
        }

        // #578: a stateless Responses host could not find an item this
        // request referenced. Retry without it (each ID once) rather than
        // failing this and every later turn.
        if retry_without_unresolvable_item(client_session, &err, &mut unresolvable_item_retries) {
            turn_context.turn_timing_state.record_sampling_retry();
            sess.send_event(
                &turn_context,
                EventMsg::Warning(WarningEvent {
                    message: "The provider could not find an earlier item this request referred \
                              to (for example after a provider switch); it was left out and the \
                              request was sent again."
                        .to_string(),
                }),
            )
            .await;
            continue;
        }

        // #616: an unreadable Corbanu home (e.g. the auth helper could not
        // read it) is waited out within exec's window, without using retries.
        let err = match crate::home_access::wait_if_home_unreadable(
            &mut home_wait,
            &sess,
            &turn_context,
            err,
        )
        .await
        {
            Ok(()) => {
                turn_context.turn_timing_state.record_sampling_retry();
                continue;
            }
            Err(err) => err,
        };

        if !err.is_retryable() {
            return Err(err);
        }
        guard_same_request_idle_retry(&err, &mut same_request_idle_failures)?;

        handle_retryable_response_stream_error(
            &mut retries,
            max_retries,
            err,
            client_session,
            &sess,
            &turn_context,
            ResponsesStreamRequest::Sampling,
            attempt_elapsed,
        )
        .await?;
        turn_context.turn_timing_state.record_sampling_retry();
    }
}

pub(crate) struct PreparedToolRecommendations {
    auth: Option<CodexAuth>,
    endpoint_candidates: Option<Vec<DiscoverableTool>>,
}

#[instrument(level = "trace", skip_all)]
pub(crate) async fn prepare_tool_recommendations(
    sess: &Session,
    turn_context: &TurnContext,
) -> PreparedToolRecommendations {
    let loaded_plugins = sess
        .services
        .plugins_manager
        .plugins_for_config(&turn_context.config.plugins_config_input())
        .instrument(trace_span!("built_tools.load_plugins"))
        .await;
    let tool_suggest_is_enabled = tool_suggest_enabled(turn_context);
    let auth = if tool_suggest_is_enabled {
        sess.services.auth_manager().auth().await
    } else {
        None
    };
    let endpoint_candidates = if tool_suggest_is_enabled {
        let plugins_config = turn_context.config.plugins_config_input();
        sess.services
            .plugins_manager
            .recommended_plugin_candidates_for_config(RecommendedPluginCandidatesInput {
                plugins_config: &plugins_config,
                loaded_plugins: &loaded_plugins,
                auth: auth.as_ref(),
                disabled_tools: &turn_context.config.tool_suggest.disabled_tools,
                app_server_client_name: turn_context.app_server_client_name.as_deref(),
            })
            .await
    } else {
        None
    };

    PreparedToolRecommendations {
        auth,
        endpoint_candidates,
    }
}

#[instrument(level = "trace",
    skip_all,
    fields(
        turn_id = %turn_context.sub_id,
        model = %turn_context.model_info.slug,
        apps_enabled = turn_context.apps_enabled()
    )
)]
pub(crate) async fn built_tools(
    sess: &Session,
    turn_context: &TurnContext,
    environments: &TurnEnvironmentSnapshot,
    mcp: &codex_mcp::McpBinding,
    step_store: &ExtensionData,
    prepared_recommendations: PreparedToolRecommendations,
) -> (Vec<ToolInfo>, Arc<ToolRouter>) {
    let all_mcp_tools = mcp.tools().to_vec();
    let connector_snapshot = mcp.config().connector_snapshot.clone();

    let apps_enabled = turn_context.apps_enabled();
    let accessible_connectors =
        apps_enabled.then(|| connectors::accessible_connectors_from_mcp_tools(&all_mcp_tools));
    let accessible_connectors_with_enabled_state =
        accessible_connectors.as_ref().map(|connectors| {
            connectors::with_app_enabled_state(connectors.clone(), &turn_context.config)
        });
    let connectors = if apps_enabled {
        let connectors = codex_connectors::merge::merge_plugin_connectors_with_accessible(
            connector_snapshot
                .connector_ids()
                .iter()
                .map(|connector_id| connector_id.0.clone()),
            accessible_connectors.clone().unwrap_or_default(),
        );
        Some(connectors::with_app_enabled_state(
            connectors,
            &turn_context.config,
        ))
    } else {
        None
    };
    let tool_suggest_is_enabled = tool_suggest_enabled(turn_context);
    let PreparedToolRecommendations {
        auth,
        endpoint_candidates: endpoint_recommended_plugin_candidates,
    } = prepared_recommendations;
    let tool_suggest_candidates =
        if let Some(recommended_plugin_candidates) = endpoint_recommended_plugin_candidates {
            Some(ToolSuggestCandidates {
                tools: recommended_plugin_candidates,
                presentation: ToolSuggestPresentation::RecommendationContext,
            })
        } else {
            let loaded_plugin_app_connector_ids = connector_snapshot
                .connector_ids()
                .iter()
                .map(|connector_id| connector_id.0.clone())
                .collect::<Vec<_>>();
            async {
                if apps_enabled && tool_suggest_is_enabled {
                    if let Some(accessible_connectors) =
                        accessible_connectors_with_enabled_state.as_ref()
                    {
                        match connectors::list_tool_suggest_discoverable_tools_with_auth(
                            &turn_context.config,
                            sess.services.plugins_manager.as_ref(),
                            auth.as_ref(),
                            accessible_connectors.as_slice(),
                            &loaded_plugin_app_connector_ids,
                        )
                        .await
                        .map(|discoverable_tools| {
                            filter_request_plugin_install_discoverable_tools_for_client(
                                discoverable_tools,
                                turn_context.app_server_client_name.as_deref(),
                            )
                        }) {
                            Ok(discoverable_tools) if discoverable_tools.is_empty() => None,
                            Ok(discoverable_tools) => Some(ToolSuggestCandidates {
                                tools: discoverable_tools,
                                presentation: ToolSuggestPresentation::ListTool,
                            }),
                            Err(err) => {
                                warn!("failed to load discoverable tool suggestions: {err:#}");
                                None
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            .instrument(trace_span!("built_tools.load_discoverable_tools"))
            .await
        };
    let tool_router = Arc::new(build_tool_router(
        sess,
        turn_context,
        environments,
        mcp,
        connectors.as_deref(),
        step_store,
        tool_suggest_candidates.as_ref(),
    ));
    notify_omitted_tools(sess, turn_context, tool_router.omitted_tools()).await;
    (all_mcp_tools, tool_router)
}

/// Most left-out tools named in the #688 notice; the rest are counted.
const OMITTED_TOOLS_NAMED: usize = 10;

/// #688: tells the person, once per tool per session, which tools a wire
/// without namespaces could not carry because of its tool-count limit. Review,
/// compaction and internal sub-sessions stay quiet: their tools are the
/// parent's, which the parent's own turns report.
async fn notify_omitted_tools(sess: &Session, turn_context: &TurnContext, omitted: &[ToolName]) {
    use codex_protocol::protocol::SessionSource;
    use codex_protocol::protocol::SubAgentSource;
    if omitted.is_empty()
        || matches!(
            turn_context.session_source,
            SessionSource::Internal(_)
                | SessionSource::SubAgent(
                    SubAgentSource::Review
                        | SubAgentSource::Compact
                        | SubAgentSource::MemoryConsolidation
                        | SubAgentSource::Other(_)
                )
        )
    {
        return;
    }
    let newly_omitted = sess.state.lock().await.unreported_omitted_tools(omitted);
    if newly_omitted.is_empty() {
        return;
    }
    let names: Vec<String> = newly_omitted
        .iter()
        .map(codex_tools::code_mode_name_for_tool_name)
        .collect();
    warn!(
        omitted = names.len(),
        first_omitted = %names[0],
        "too many tools for a provider without namespace tools; omitting the rest"
    );
    sess.send_event(
        turn_context,
        EventMsg::Warning(WarningEvent {
            message: omitted_tools_notice(&names),
        }),
    )
    .await;
    // Only once shown: an interrupted turn reports them again next time.
    sess.state
        .lock()
        .await
        .mark_omitted_tools_reported(newly_omitted);
}

fn omitted_tools_notice(names: &[String]) -> String {
    let limit = crate::tools::spec_plan::MAX_TOOLS_WITHOUT_NAMESPACES;
    let mut listed = names
        .iter()
        .take(OMITTED_TOOLS_NAMED)
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > OMITTED_TOOLS_NAMED {
        listed.push_str(&format!(" and {} more", names.len() - OMITTED_TOOLS_NAMED));
    }
    let count = names.len();
    format!(
        "Corbanu sends at most {limit} tools per request to providers without tool namespaces, so these are not offered to the model ({count}): {listed}. Turn off MCP servers or tools you don't need, or use a provider with tool namespaces (such as OpenAI), to offer them."
    )
}

#[derive(Debug)]
struct SamplingRequestResult {
    needs_follow_up: bool,
    last_agent_message: Option<String>,
    token_usage: Option<TokenUsage>,
    server_side_model_continuation: bool,
    made_tool_progress: bool,
    provider_stopped: bool,
}

/// Ephemeral per-response state for streaming a single proposed plan.
/// This is intentionally not persisted or stored in session/state since it
/// only exists while a response is actively streaming. The final plan text
/// is extracted from the completed assistant message.
/// Tracks a single proposed plan item across a streaming response.
struct ProposedPlanItemState {
    item_id: String,
    started: bool,
    completed: bool,
}

/// Aggregated state used only while streaming a plan-mode response.
/// Includes per-item parsers, deferred agent message bookkeeping, and the plan item lifecycle.
struct PlanModeStreamState {
    /// Agent message items started by the model but deferred until we see non-plan text.
    pending_agent_message_items: HashMap<String, TurnItem>,
    /// Agent message items whose start notification has been emitted.
    started_agent_message_items: HashSet<String>,
    /// Leading whitespace buffered until we see non-whitespace text for an item.
    leading_whitespace_by_item: HashMap<String, String>,
    /// Tracks plan item lifecycle while streaming plan output.
    plan_item_state: ProposedPlanItemState,
}

impl PlanModeStreamState {
    fn new(turn_id: &str) -> Self {
        Self {
            pending_agent_message_items: HashMap::new(),
            started_agent_message_items: HashSet::new(),
            leading_whitespace_by_item: HashMap::new(),
            plan_item_state: ProposedPlanItemState::new(turn_id),
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct AssistantMessageStreamParsers {
    plan_mode: bool,
    parsers_by_item: HashMap<String, AssistantTextStreamParser>,
}

type ParsedAssistantTextDelta = AssistantTextChunk;

impl AssistantMessageStreamParsers {
    pub(super) fn new(plan_mode: bool) -> Self {
        Self {
            plan_mode,
            parsers_by_item: HashMap::new(),
        }
    }

    fn parser_mut(&mut self, item_id: &str) -> &mut AssistantTextStreamParser {
        let plan_mode = self.plan_mode;
        self.parsers_by_item
            .entry(item_id.to_string())
            .or_insert_with(|| AssistantTextStreamParser::new(plan_mode))
    }

    pub(super) fn seed_item_text(&mut self, item_id: &str, text: &str) -> ParsedAssistantTextDelta {
        if text.is_empty() {
            return ParsedAssistantTextDelta::default();
        }
        self.parser_mut(item_id).push_str(text)
    }

    pub(super) fn parse_delta(&mut self, item_id: &str, delta: &str) -> ParsedAssistantTextDelta {
        self.parser_mut(item_id).push_str(delta)
    }

    pub(super) fn finish_item(&mut self, item_id: &str) -> ParsedAssistantTextDelta {
        let Some(mut parser) = self.parsers_by_item.remove(item_id) else {
            return ParsedAssistantTextDelta::default();
        };
        parser.finish()
    }

    fn drain_finished(&mut self) -> Vec<(String, ParsedAssistantTextDelta)> {
        let parsers_by_item = std::mem::take(&mut self.parsers_by_item);
        parsers_by_item
            .into_iter()
            .map(|(item_id, mut parser)| (item_id, parser.finish()))
            .collect()
    }
}

impl ProposedPlanItemState {
    fn new(turn_id: &str) -> Self {
        Self {
            item_id: format!("{turn_id}-plan"),
            started: false,
            completed: false,
        }
    }

    async fn start(&mut self, sess: &Session, turn_context: &TurnContext) {
        if self.started || self.completed {
            return;
        }
        self.started = true;
        let item = TurnItem::Plan(PlanItem {
            id: self.item_id.clone(),
            text: String::new(),
        });
        sess.emit_turn_item_started(turn_context, &item).await;
    }

    async fn push_delta(&mut self, sess: &Session, turn_context: &TurnContext, delta: &str) {
        if self.completed {
            return;
        }
        if delta.is_empty() {
            return;
        }
        let event = PlanDeltaEvent {
            thread_id: sess.thread_id.to_string(),
            turn_id: turn_context.sub_id.clone(),
            item_id: self.item_id.clone(),
            delta: delta.to_string(),
        };
        sess.send_event(turn_context, EventMsg::PlanDelta(event))
            .await;
    }

    async fn complete_with_text(
        &mut self,
        sess: &Session,
        turn_context: &TurnContext,
        text: String,
    ) {
        if self.completed || !self.started {
            return;
        }
        self.completed = true;
        let item = TurnItem::Plan(PlanItem {
            id: self.item_id.clone(),
            text,
        });
        sess.emit_turn_item_completed(turn_context, item).await;
    }
}

/// In plan mode we defer agent message starts until the parser emits non-plan
/// text. The parser buffers each line until it can rule out a tag prefix, so
/// plan-only outputs never show up as empty assistant messages.
async fn maybe_emit_pending_agent_message_start(
    sess: &Session,
    turn_context: &TurnContext,
    state: &mut PlanModeStreamState,
    item_id: &str,
) {
    if state.started_agent_message_items.contains(item_id) {
        return;
    }
    if let Some(item) = state.pending_agent_message_items.remove(item_id) {
        sess.emit_turn_item_started(turn_context, &item).await;
        state
            .started_agent_message_items
            .insert(item_id.to_string());
    }
}

/// Agent messages are text-only today; concatenate all text entries.
pub(super) fn agent_message_text(item: &codex_protocol::items::AgentMessageItem) -> String {
    item.content
        .iter()
        .map(|entry| match entry {
            codex_protocol::items::AgentMessageContent::Text { text } => text.as_str(),
        })
        .collect()
}

pub(super) fn realtime_text_for_event(msg: &EventMsg) -> Option<(String, Option<MessagePhase>)> {
    match msg {
        EventMsg::AgentMessage(event) => Some((event.message.clone(), event.phase.clone())),
        EventMsg::ItemCompleted(event) => match &event.item {
            TurnItem::AgentMessage(item) => Some((agent_message_text(item), item.phase.clone())),
            _ => None,
        },
        EventMsg::Error(_)
        | EventMsg::Warning(_)
        | EventMsg::GuardianWarning(_)
        | EventMsg::RealtimeConversationStarted(_)
        | EventMsg::RealtimeConversationSdp(_)
        | EventMsg::RealtimeConversationRealtime(_)
        | EventMsg::RealtimeConversationClosed(_)
        | EventMsg::ModelReroute(_)
        | EventMsg::ModelVerification(_)
        | EventMsg::TurnModerationMetadata(_)
        | EventMsg::SafetyBuffering(_)
        | EventMsg::ContextCompacted(_)
        | EventMsg::ThreadRolledBack(_)
        | EventMsg::TurnStarted(_)
        | EventMsg::ThreadSettingsApplied(_)
        | EventMsg::TurnComplete(_)
        | EventMsg::ModelResponseCompleted(_)
        | EventMsg::TokenCount(_)
        | EventMsg::UserMessage(_)
        | EventMsg::AgentReasoning(_)
        | EventMsg::AgentReasoningRawContent(_)
        | EventMsg::AgentReasoningSectionBreak(_)
        | EventMsg::SessionConfigured(_)
        | EventMsg::EnvironmentConnected(_)
        | EventMsg::EnvironmentDisconnected(_)
        | EventMsg::ThreadGoalUpdated(_)
        | EventMsg::McpStartupUpdate(_)
        | EventMsg::McpStartupComplete(_)
        | EventMsg::McpToolCallBegin(_)
        | EventMsg::McpToolCallEnd(_)
        | EventMsg::WebSearchBegin(_)
        | EventMsg::WebSearchEnd(_)
        | EventMsg::ExecCommandBegin(_)
        | EventMsg::ExecCommandOutputDelta(_)
        | EventMsg::TerminalInteraction(_)
        | EventMsg::ExecCommandEnd(_)
        | EventMsg::PatchApplyBegin(_)
        | EventMsg::PatchApplyUpdated(_)
        | EventMsg::PatchApplyEnd(_)
        | EventMsg::ImageGenerationBegin(_)
        | EventMsg::ImageGenerationEnd(_)
        | EventMsg::ViewImageToolCall(_)
        | EventMsg::ExecApprovalRequest(_)
        | EventMsg::ExecApprovalRequestWithdrawn(_)
        | EventMsg::RequestPermissions(_)
        | EventMsg::RequestUserInput(_)
        | EventMsg::DynamicToolCallRequest(_)
        | EventMsg::DynamicToolCallResponse(_)
        | EventMsg::GuardianAssessment(_)
        | EventMsg::ElicitationRequest(_)
        | EventMsg::ApplyPatchApprovalRequest(_)
        | EventMsg::DeprecationNotice(_)
        | EventMsg::StreamError(_)
        | EventMsg::TurnDiff(_)
        | EventMsg::RealtimeConversationListVoicesResponse(_)
        | EventMsg::PlanUpdate(_)
        | EventMsg::TurnAborted(_)
        | EventMsg::ShutdownComplete
        | EventMsg::EnteredReviewMode(_)
        | EventMsg::ExitedReviewMode(_)
        | EventMsg::RawResponseItem(_)
        | EventMsg::RawResponseCompleted(_)
        | EventMsg::ItemStarted(_)
        | EventMsg::HookStarted(_)
        | EventMsg::HookCompleted(_)
        | EventMsg::AgentMessageContentDelta(_)
        | EventMsg::PlanDelta(_)
        | EventMsg::ReasoningContentDelta(_)
        | EventMsg::ReasoningRawContentDelta(_)
        | EventMsg::CollabAgentSpawnBegin(_)
        | EventMsg::CollabAgentSpawnEnd(_)
        | EventMsg::CollabAgentInteractionBegin(_)
        | EventMsg::CollabAgentInteractionEnd(_)
        | EventMsg::CollabWaitingBegin(_)
        | EventMsg::CollabWaitingEnd(_)
        | EventMsg::CollabCloseBegin(_)
        | EventMsg::CollabCloseEnd(_)
        | EventMsg::CollabResumeBegin(_)
        | EventMsg::CollabResumeEnd(_)
        | EventMsg::SubAgentActivity(_) => None,
    }
}

/// Split the stream into normal assistant text vs. proposed plan content.
/// Normal text becomes AgentMessage deltas; plan content becomes PlanDelta +
/// TurnItem::Plan.
async fn handle_plan_segments(
    sess: &Session,
    turn_context: &TurnContext,
    state: &mut PlanModeStreamState,
    item_id: &str,
    segments: Vec<ProposedPlanSegment>,
) {
    for segment in segments {
        match segment {
            ProposedPlanSegment::Normal(delta) => {
                if delta.is_empty() {
                    continue;
                }
                let has_non_whitespace = delta.chars().any(|ch| !ch.is_whitespace());
                if !has_non_whitespace && !state.started_agent_message_items.contains(item_id) {
                    let entry = state
                        .leading_whitespace_by_item
                        .entry(item_id.to_string())
                        .or_default();
                    entry.push_str(&delta);
                    continue;
                }
                let delta = if !state.started_agent_message_items.contains(item_id) {
                    if let Some(prefix) = state.leading_whitespace_by_item.remove(item_id) {
                        format!("{prefix}{delta}")
                    } else {
                        delta
                    }
                } else {
                    delta
                };
                maybe_emit_pending_agent_message_start(sess, turn_context, state, item_id).await;

                let event = AgentMessageContentDeltaEvent {
                    thread_id: sess.thread_id.to_string(),
                    turn_id: turn_context.sub_id.clone(),
                    item_id: item_id.to_string(),
                    delta,
                };
                sess.send_event(turn_context, EventMsg::AgentMessageContentDelta(event))
                    .await;
            }
            ProposedPlanSegment::ProposedPlanStart => {
                if !state.plan_item_state.completed {
                    state.plan_item_state.start(sess, turn_context).await;
                }
            }
            ProposedPlanSegment::ProposedPlanDelta(delta) => {
                if !state.plan_item_state.completed {
                    if !state.plan_item_state.started {
                        state.plan_item_state.start(sess, turn_context).await;
                    }
                    state
                        .plan_item_state
                        .push_delta(sess, turn_context, &delta)
                        .await;
                }
            }
            ProposedPlanSegment::ProposedPlanEnd => {}
        }
    }
}

async fn emit_streamed_assistant_text_delta(
    sess: &Session,
    turn_context: &TurnContext,
    plan_mode_state: Option<&mut PlanModeStreamState>,
    item_id: &str,
    parsed: ParsedAssistantTextDelta,
) {
    if parsed.is_empty() {
        return;
    }
    if !parsed.citations.is_empty() {
        // Citation extraction is intentionally local for now; we strip citations from display text
        // but do not yet surface them in protocol events.
        let _citations = parsed.citations;
    }
    if let Some(state) = plan_mode_state {
        if !parsed.plan_segments.is_empty() {
            handle_plan_segments(sess, turn_context, state, item_id, parsed.plan_segments).await;
        }
        return;
    }
    if parsed.visible_text.is_empty() {
        return;
    }
    let event = AgentMessageContentDeltaEvent {
        thread_id: sess.thread_id.to_string(),
        turn_id: turn_context.sub_id.clone(),
        item_id: item_id.to_string(),
        delta: parsed.visible_text,
    };
    sess.send_event(turn_context, EventMsg::AgentMessageContentDelta(event))
        .await;
}

async fn emit_output_text_delta_for_active_item(
    sess: &Session,
    turn_context: &TurnContext,
    plan_mode_state: Option<&mut PlanModeStreamState>,
    parsers: &mut AssistantMessageStreamParsers,
    active_item: &TurnItem,
    delta: String,
) {
    let item_id = active_item.id();
    if matches!(active_item, TurnItem::AgentMessage(_)) {
        let parsed = parsers.parse_delta(&item_id, &delta);
        emit_streamed_assistant_text_delta(sess, turn_context, plan_mode_state, &item_id, parsed)
            .await;
    } else {
        let event = AgentMessageContentDeltaEvent {
            thread_id: sess.thread_id.to_string(),
            turn_id: turn_context.sub_id.clone(),
            item_id,
            delta,
        };
        sess.send_event(turn_context, EventMsg::AgentMessageContentDelta(event))
            .await;
    }
}

/// Flush buffered assistant text parser state when an assistant message item ends.
async fn flush_assistant_text_segments_for_item(
    sess: &Session,
    turn_context: &TurnContext,
    plan_mode_state: Option<&mut PlanModeStreamState>,
    parsers: &mut AssistantMessageStreamParsers,
    item_id: &str,
) {
    let parsed = parsers.finish_item(item_id);
    emit_streamed_assistant_text_delta(sess, turn_context, plan_mode_state, item_id, parsed).await;
}

/// Flush any remaining buffered assistant text parser state at response completion.
async fn flush_assistant_text_segments_all(
    sess: &Session,
    turn_context: &TurnContext,
    mut plan_mode_state: Option<&mut PlanModeStreamState>,
    parsers: &mut AssistantMessageStreamParsers,
) {
    for (item_id, parsed) in parsers.drain_finished() {
        emit_streamed_assistant_text_delta(
            sess,
            turn_context,
            plan_mode_state.as_deref_mut(),
            &item_id,
            parsed,
        )
        .await;
    }
}

/// Emit completion for plan items by parsing the finalized assistant message.
async fn maybe_complete_plan_item_from_message(
    sess: &Session,
    turn_context: &TurnContext,
    state: &mut PlanModeStreamState,
    item: &ResponseItem,
) {
    if let ResponseItem::Message { role, content, .. } = item
        && role == "assistant"
    {
        let mut text = String::new();
        for entry in content {
            if let ContentItem::OutputText { text: chunk } = entry {
                text.push_str(chunk);
            }
        }
        if let Some(plan_text) = extract_proposed_plan_text(&text) {
            let (plan_text, _citations) = strip_citations(&plan_text);
            if !state.plan_item_state.started {
                state.plan_item_state.start(sess, turn_context).await;
            }
            state
                .plan_item_state
                .complete_with_text(sess, turn_context, plan_text)
                .await;
        }
    }
}

/// Emit a completed agent message in plan mode, respecting deferred starts.
async fn emit_agent_message_in_plan_mode(
    sess: &Session,
    turn_context: &TurnContext,
    agent_message: codex_protocol::items::AgentMessageItem,
    state: &mut PlanModeStreamState,
) {
    let agent_message_id = agent_message.id.clone();
    let text = agent_message_text(&agent_message);
    if text.trim().is_empty() {
        state.pending_agent_message_items.remove(&agent_message_id);
        state.started_agent_message_items.remove(&agent_message_id);
        return;
    }

    maybe_emit_pending_agent_message_start(sess, turn_context, state, &agent_message_id).await;

    if !state
        .started_agent_message_items
        .contains(&agent_message_id)
    {
        let start_item = state
            .pending_agent_message_items
            .remove(&agent_message_id)
            .unwrap_or_else(|| {
                TurnItem::AgentMessage(codex_protocol::items::AgentMessageItem {
                    id: agent_message_id.clone(),
                    content: Vec::new(),
                    phase: None,
                    memory_citation: None,
                })
            });
        sess.emit_turn_item_started(turn_context, &start_item).await;
        state
            .started_agent_message_items
            .insert(agent_message_id.clone());
    }

    sess.emit_turn_item_completed(turn_context, TurnItem::AgentMessage(agent_message))
        .await;
    state.started_agent_message_items.remove(&agent_message_id);
}

/// Emit completion for a plan-mode turn item, handling agent messages specially.
async fn emit_turn_item_in_plan_mode(
    sess: &Session,
    turn_context: &TurnContext,
    turn_item: TurnItem,
    previously_active_item: Option<&TurnItem>,
    state: &mut PlanModeStreamState,
) {
    match turn_item {
        TurnItem::AgentMessage(agent_message) => {
            emit_agent_message_in_plan_mode(sess, turn_context, agent_message, state).await;
        }
        _ => {
            if previously_active_item.is_none() {
                sess.emit_turn_item_started(turn_context, &turn_item).await;
            }
            sess.emit_turn_item_completed(turn_context, turn_item).await;
        }
    }
}

/// Handle a completed assistant response item in plan mode, returning true if handled.
async fn handle_assistant_item_done_in_plan_mode(
    sess: &Session,
    turn_context: &TurnContext,
    turn_store: &codex_extension_api::ExtensionData,
    item: &ResponseItem,
    state: &mut PlanModeStreamState,
    previously_active_item: Option<&TurnItem>,
    last_agent_message: &mut Option<String>,
) -> bool {
    if let ResponseItem::Message { role, .. } = item
        && role == "assistant"
    {
        maybe_complete_plan_item_from_message(sess, turn_context, state, item).await;

        let mut finalized_facts = None;
        if let Some(finalized_turn_item) = finalize_non_tool_response_item(
            sess,
            TurnItemContributorPolicy::Run(turn_store),
            item,
            /*plan_mode*/ true,
        )
        .await
        {
            finalized_facts = Some(finalized_turn_item.facts.clone());
            emit_turn_item_in_plan_mode(
                sess,
                turn_context,
                finalized_turn_item.turn_item,
                previously_active_item,
                state,
            )
            .await;
        }
        let final_last_agent_message = finalized_facts
            .as_ref()
            .and_then(|facts| facts.last_agent_message.clone());

        record_completed_response_item_with_finalized_facts(
            sess,
            turn_context,
            item,
            finalized_facts.as_ref(),
        )
        .await;
        if let Some(agent_message) = final_last_agent_message {
            *last_agent_message = Some(agent_message);
        }
        return true;
    }
    false
}

#[instrument(level = "trace", skip_all)]
async fn drain_in_flight(
    in_flight: &mut FuturesOrdered<AbortOnDropHandle<CodexResult<ResponseInputItem>>>,
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
) -> CodexResult<()> {
    // A tool that reports a fatal error (for example the repeated-call or
    // malformed-call guards) ends the turn once every other in-flight call has
    // recorded its output.
    let mut fatal = None;
    while let Some(res) = in_flight.next().await {
        match res {
            Ok(Ok(response_input)) => {
                let call_id = match &response_input {
                    ResponseInputItem::FunctionCallOutput { call_id, .. }
                    | ResponseInputItem::McpToolCallOutput { call_id, .. }
                    | ResponseInputItem::CustomToolCallOutput { call_id, .. }
                    | ResponseInputItem::ToolSearchOutput { call_id, .. } => Some(call_id.clone()),
                    ResponseInputItem::Message { .. } => None,
                };
                let response_item = response_input.into();
                sess.record_conversation_items(&turn_context, std::slice::from_ref(&response_item))
                    .await;
                if let Some(call_id) = call_id {
                    turn_context.pending_tool_calls.complete(&call_id);
                }
                mark_thread_memory_mode_polluted_if_external_context(
                    sess.as_ref(),
                    turn_context.as_ref(),
                    &response_item,
                )
                .await;
            }
            Ok(Err(err)) if matches!(err.details(), CodexErrorDetails::Fatal(_)) => {
                fatal.get_or_insert(err);
            }
            Ok(Err(err)) => {
                error_or_panic(format!("in-flight tool future failed during drain: {err}"));
            }
            Err(err) => error_or_panic(format!("in-flight tool task failed during drain: {err}")),
        }
    }
    fatal.map_or(Ok(()), Err)
}
async fn acquire_provider_request_lease(
    sess: &Session,
    turn_context: &TurnContext,
    client_session: &ModelClientSession,
    prompt: &Prompt,
    responses_metadata: &CodexResponsesMetadata,
    cancellation_token: &CancellationToken,
) -> CodexResult<ProviderRequestThrottle> {
    let preflight_started_at = Instant::now();
    trace_turn_timing("provider_preflight_start", preflight_started_at);
    let Some(state_db) = sess.state_db() else {
        return Ok(ProviderRequestThrottle::default());
    };

    let (codex_home, auth_mode) = turn_auth(sess, turn_context);
    let Ok(key) = provider_request_key(
        &state_db,
        &codex_home,
        turn_context.config.model_provider_id.as_str(),
        turn_context.provider.info(),
        turn_context.model_info.slug.as_str(),
        || auth_mode,
    )
    .boxed()
    .or_cancel(cancellation_token)
    .await
    else {
        return Err(CodexErr::TurnAborted);
    };
    let token_info = sess.token_usage_info().await;
    trace_turn_timing("provider_preflight_after_token_info", preflight_started_at);
    let last_token_usage = token_info
        .as_ref()
        .map(|info| info.last_token_usage.clone());
    let last_cached_input_tokens = last_token_usage
        .as_ref()
        .map(TokenUsage::cached_input)
        .unwrap_or(0);
    let input_tokens = match sess.get_estimated_token_count(turn_context).await {
        Some(tokens) => tokens,
        None => token_info
            .as_ref()
            .map(|info| info.total_token_usage.input_tokens)
            .unwrap_or_else(|| rough_prompt_token_estimate(prompt)),
    };
    trace_turn_timing(
        "provider_preflight_after_input_tokens",
        preflight_started_at,
    );
    let request_bytes = client_session
        .serialized_request_body_bytes(
            prompt,
            &turn_context.model_info,
            turn_context.reasoning_effort.clone(),
            turn_context.reasoning_summary,
            turn_context.config.service_tier.clone(),
            responses_metadata,
            turn_context
                .auth_manager
                .as_ref()
                .and_then(|auth_manager| auth_manager.auth_mode()),
        )
        .map(|bytes| i64::try_from(bytes).unwrap_or(i64::MAX))
        .unwrap_or_else(|err| {
            warn!(
                turn_id = %turn_context.sub_id,
                provider = %key.provider_id,
                model = %key.model,
                error = %err,
                "failed to serialize provider request for hammer-reduction preflight"
            );
            0
        });
    trace_turn_timing(
        "provider_preflight_after_request_bytes",
        preflight_started_at,
    );
    let preflight = ProviderRequestPreflight {
        input_tokens,
        cached_input_tokens: last_cached_input_tokens,
        request_bytes,
        thread_id: Some(sess.thread_id.to_string()),
        turn_id: Some(turn_context.sub_id.clone()),
    };
    maybe_emit_provider_request_pressure_warning(
        sess,
        turn_context,
        &key,
        &preflight,
        last_token_usage.as_ref(),
    )
    .await;
    trace_turn_timing("provider_preflight_after_warning", preflight_started_at);
    let now_ms = now_unix_timestamp_ms();
    if !provider_request_active_lease_needed(turn_context, &preflight, last_token_usage.as_ref()) {
        // A busy state DB is waited for briefly, then the throttle goes on from
        // this process's own state; an interrupt must not wait with it.
        // Boxed (type-erased) throughout: the turn's future is otherwise too
        // deep for rustc's `Send` check.
        let Ok(cooldown) = state_db
            .check_provider_request_throttle(&key, &preflight, now_ms)
            .boxed()
            .or_cancel(cancellation_token)
            .await
        else {
            return Err(CodexErr::TurnAborted);
        };
        notify_state_db_fallback(sess, turn_context, cooldown.state_db_fallback).await;
        if let Some(block) = cooldown.decision {
            warn!(
                turn_id = %turn_context.sub_id,
                provider = %key.provider_id,
                model = %key.model,
                key_fingerprint = %key.key_fingerprint,
                reason = ?block.reason,
                remaining_ms = block.remaining_ms,
                last_status = ?block.last_status,
                last_request_bytes = block.last_request_bytes,
                "provider request blocked by local hammer-reduction state"
            );
            return Err(CodexErr::InvalidRequest(format_provider_request_block(
                &key, &block,
            )));
        }
        trace_turn_timing("provider_preflight_no_lease_needed", preflight_started_at);
        return Ok(ProviderRequestThrottle {
            unleased_key: Some(key),
            lease: None,
        });
    }
    let owner = format!(
        "pid:{}:thread:{}:turn:{}",
        std::process::id(),
        sess.thread_id,
        turn_context.sub_id
    );
    // An interrupt can land after the lease write committed: dropping this guard
    // then releases whatever this owner holds. Release matches key and owner
    // only, so the lease's end time is not needed.
    let provisional_lease = ProviderRequestLeaseGuard::provisional(
        sess,
        ProviderRequestLease {
            key: key.clone(),
            owner: owner.clone(),
            lease_until_ms: now_ms,
            in_state_db: true,
        },
    );
    let Ok(decision) = state_db
        .acquire_provider_request_throttle_lease(
            &key,
            &preflight,
            &owner,
            PROVIDER_REQUEST_LEASE_TTL_MS,
            now_ms,
        )
        .boxed()
        .or_cancel(cancellation_token)
        .await
    else {
        return Err(CodexErr::TurnAborted);
    };
    provisional_lease.disarm();
    notify_state_db_fallback(sess, turn_context, decision.state_db_fallback).await;

    match decision.decision {
        ProviderRequestLeaseDecision::Acquired(lease) => {
            trace_turn_timing("provider_preflight_acquired_lease", preflight_started_at);
            info!(
                turn_id = %turn_context.sub_id,
                provider = %key.provider_id,
                model = %key.model,
                key_fingerprint = %key.key_fingerprint,
                owner = %lease.owner,
                input_tokens = preflight.input_tokens,
                cached_input_tokens = preflight.cached_input_tokens,
                request_bytes = preflight.request_bytes,
                lease_until_ms = lease.lease_until_ms,
                "provider request preflight acquired lease"
            );
            Ok(ProviderRequestThrottle {
                unleased_key: None,
                lease: Some(lease),
            })
        }
        ProviderRequestLeaseDecision::Blocked(block) => {
            warn!(
                turn_id = %turn_context.sub_id,
                provider = %key.provider_id,
                model = %key.model,
                key_fingerprint = %key.key_fingerprint,
                attempted_owner = %owner,
                lease_owner = ?block.lease_owner,
                reason = ?block.reason,
                remaining_ms = block.remaining_ms,
                last_status = ?block.last_status,
                last_request_bytes = block.last_request_bytes,
                "provider request blocked by local hammer-reduction state"
            );
            Err(CodexErr::InvalidRequest(format_provider_request_block(
                &key, &block,
            )))
        }
    }
}

async fn maybe_emit_provider_request_pressure_warning(
    sess: &Session,
    turn_context: &TurnContext,
    key: &ProviderRequestKey,
    preflight: &ProviderRequestPreflight,
    last_token_usage: Option<&TokenUsage>,
) {
    if !provider_uses_request_lease(
        turn_context.config.model_provider_id.as_str(),
        turn_context.provider.info().is_openai(),
    ) {
        return;
    }
    if preflight.input_tokens < THIRD_PARTY_PREFLIGHT_WARNING_INPUT_TOKENS
        && preflight.request_bytes < THIRD_PARTY_PREFLIGHT_WARNING_REQUEST_BYTES
    {
        return;
    }
    if third_party_cache_looks_healthy(last_token_usage) {
        info!(
            turn_id = %turn_context.sub_id,
            provider = %key.provider_id,
            model = %key.model,
            input_tokens = preflight.input_tokens,
            last_cached_input_tokens = preflight.cached_input_tokens,
            request_bytes = preflight.request_bytes,
            "third-party provider request is large but recent prompt-cache hit is healthy"
        );
        return;
    }
    if !third_party_cache_miss_is_known(last_token_usage) {
        info!(
            turn_id = %turn_context.sub_id,
            provider = %key.provider_id,
            model = %key.model,
            input_tokens = preflight.input_tokens,
            request_bytes = preflight.request_bytes,
            "third-party provider request is large; waiting for provider cache telemetry before warning"
        );
        return;
    }
    if turn_context
        .provider_cache_pressure_warning_emitted
        .swap(true, Ordering::Relaxed)
    {
        info!(
            turn_id = %turn_context.sub_id,
            provider = %key.provider_id,
            model = %key.model,
            input_tokens = preflight.input_tokens,
            cached_input_tokens = preflight.cached_input_tokens,
            request_bytes = preflight.request_bytes,
            "suppressing duplicate third-party provider cache pressure warning for turn"
        );
        return;
    }
    let Some(cache_details) = cache_hit_details(last_token_usage) else {
        return;
    };
    let message = provider_cache_pressure_warning_message(key, preflight, cache_details);
    sess.send_event(turn_context, EventMsg::Warning(WarningEvent { message }))
        .await;
}

fn third_party_cache_looks_healthy(last_token_usage: Option<&TokenUsage>) -> bool {
    cache_hit_rate(last_token_usage).is_some_and(|rate| rate >= THIRD_PARTY_CACHE_HEALTHY_HIT_RATE)
}

fn third_party_cache_miss_is_known(last_token_usage: Option<&TokenUsage>) -> bool {
    cache_hit_rate(last_token_usage).is_some_and(|rate| rate < THIRD_PARTY_CACHE_HEALTHY_HIT_RATE)
}

#[derive(Debug, Clone, Copy)]
struct CacheHitDetails {
    input_tokens: i64,
    cached_input_tokens: i64,
    hit_rate: f64,
}

fn provider_cache_pressure_warning_message(
    key: &ProviderRequestKey,
    preflight: &ProviderRequestPreflight,
    cache_details: CacheHitDetails,
) -> String {
    if cache_details.cached_input_tokens <= 0 {
        return format!(
            "Provider cache miss: {}/{} is about to send input={} request_bytes={} after the previous large request had cached_input=0/{} (0.0%). This may hit third-party provider limits; compact or start a fresh thread if this was a tiny follow-up.",
            key.provider_id,
            key.model,
            preflight.input_tokens,
            preflight.request_bytes,
            cache_details.input_tokens,
        );
    }

    format!(
        "Provider cache low hit rate: {}/{} is about to send input={} request_bytes={} after the previous large request reused cached_input={}/{} ({:.1}%). This may hit third-party provider limits; compact or start a fresh thread if this was a tiny follow-up.",
        key.provider_id,
        key.model,
        preflight.input_tokens,
        preflight.request_bytes,
        cache_details.cached_input_tokens,
        cache_details.input_tokens,
        cache_details.hit_rate * 100.0,
    )
}

fn provider_request_active_lease_needed(
    turn_context: &TurnContext,
    preflight: &ProviderRequestPreflight,
    last_token_usage: Option<&TokenUsage>,
) -> bool {
    if !provider_request_lease_applies_to_session(&turn_context.session_source) {
        return false;
    }
    if !provider_uses_request_lease(
        turn_context.config.model_provider_id.as_str(),
        turn_context.provider.info().is_openai(),
    ) {
        return false;
    }
    let large_request = preflight.input_tokens >= THIRD_PARTY_PREFLIGHT_WARNING_INPUT_TOKENS
        || preflight.request_bytes >= THIRD_PARTY_PREFLIGHT_WARNING_REQUEST_BYTES;
    large_request && !third_party_cache_looks_healthy(last_token_usage)
}

/// The cross-process lease exists to stop many autonomous sub-agents from
/// hammering one metered third-party key. A human-driven session is
/// control-plane work, not worker execution: it must stay able to reach its
/// manager while sub-agents saturate that key, so it never waits on the
/// shared lease. This mirrors the worker/control-plane split that
/// `AgentControl` already applies to execution capacity, and is neutral to
/// provider, model, and role name.
fn provider_request_lease_applies_to_session(session_source: &SessionSource) -> bool {
    matches!(session_source, SessionSource::SubAgent(_))
}

fn provider_uses_request_lease(provider_id: &str, is_openai: bool) -> bool {
    // Runtime GPU providers represent user-owned, explicitly capacity-bounded inference.
    // The cross-process hammer-reduction lease is for metered third-party APIs and would
    // otherwise serialize GPU tool turns for ten minutes after a process interruption.
    !is_openai && !provider_id.starts_with("gpu-")
}

fn cache_hit_rate(last_token_usage: Option<&TokenUsage>) -> Option<f64> {
    cache_hit_details(last_token_usage).map(|details| details.hit_rate)
}

fn cache_hit_details(last_token_usage: Option<&TokenUsage>) -> Option<CacheHitDetails> {
    let usage = last_token_usage?;
    let input = usage.input_tokens.max(0);
    if input < THIRD_PARTY_CACHE_HEALTH_MIN_INPUT_TOKENS {
        return None;
    }
    let cached = usage.cached_input().max(0);
    Some(CacheHitDetails {
        input_tokens: input,
        cached_input_tokens: cached,
        hit_rate: cached as f64 / input as f64,
    })
}

async fn record_provider_request_result_for_lease(
    sess: &Session,
    lease: Option<&ProviderRequestLease>,
    result: ProviderRequestResult,
) -> bool {
    let Some(lease) = lease else {
        return true;
    };
    let Some(state_db) = sess.state_db() else {
        return true;
    };
    let result_kind = provider_request_result_kind(&result);
    let recorded = state_db
        .record_provider_request_throttle_result(lease, result, now_unix_timestamp_ms())
        .boxed()
        .await;
    trace!(
        provider = %lease.key.provider_id,
        model = %lease.key.model,
        key_fingerprint = %lease.key.key_fingerprint,
        owner = %lease.owner,
        result = result_kind,
        state_db_released = recorded.decision,
        state_db_fallback = ?recorded.state_db_fallback,
        "recorded provider request result"
    );
    recorded.decision
}

/// How the throttle let a request go: under a lease, or without one.
#[derive(Default)]
struct ProviderRequestThrottle {
    /// The key whose cooldown records this request's result when it holds
    /// no lease (#476).
    unleased_key: Option<ProviderRequestKey>,
    lease: Option<ProviderRequestLease>,
}

struct ProviderRequestLeaseGuard {
    state_db: Option<StateDbHandle>,
    runtime_handle: tokio::runtime::Handle,
    lease: Option<ProviderRequestLease>,
    unleased_key: Option<ProviderRequestKey>,
    /// The lease may never have been written; releasing nothing is expected.
    provisional: bool,
}

impl ProviderRequestLeaseGuard {
    fn new(sess: &Session, throttle: ProviderRequestThrottle) -> Self {
        Self {
            state_db: sess.state_db(),
            runtime_handle: sess.services.runtime_handle.clone(),
            lease: throttle.lease,
            unleased_key: throttle.unleased_key,
            provisional: false,
        }
    }

    fn provisional(sess: &Session, lease: ProviderRequestLease) -> Self {
        let mut guard = Self::new(
            sess,
            ProviderRequestThrottle {
                unleased_key: None,
                lease: Some(lease),
            },
        );
        guard.provisional = true;
        guard
    }

    /// Forget the lease without releasing it.
    fn disarm(mut self) {
        self.lease = None;
    }

    /// On an interrupt the lease stays here, and dropping the guard releases it.
    async fn record_result(
        &mut self,
        sess: &Session,
        result: ProviderRequestResult,
        cancellation_token: &CancellationToken,
    ) {
        if let Some(key) = self.unleased_key.take() {
            if let Some(state_db) = self.state_db.as_ref() {
                record_unleased_provider_request_result(state_db, &key, result)
                    .boxed()
                    .or_cancel(cancellation_token)
                    .await
                    .ok();
            }
            return;
        }
        let Some(lease) = self.lease.clone() else {
            return;
        };
        if let Ok(true) = record_provider_request_result_for_lease(sess, Some(&lease), result)
            .or_cancel(cancellation_token)
            .await
        {
            self.lease = None;
        }
    }
}

impl Drop for ProviderRequestLeaseGuard {
    fn drop(&mut self) {
        let Some(lease) = self.lease.take() else {
            return;
        };
        let Some(state_db) = self.state_db.clone() else {
            return;
        };
        let provisional = self.provisional;
        std::mem::drop(self.runtime_handle.spawn(async move {
            match state_db
                .release_provider_request_throttle_lease(&lease, now_unix_timestamp_ms())
                .await
            {
                Ok(0) if provisional => trace!(
                    provider = %lease.key.provider_id,
                    model = %lease.key.model,
                    owner = %lease.owner,
                    "interrupted before a provider request lease was written"
                ),
                Ok(0) => warn!(
                    provider = %lease.key.provider_id,
                    model = %lease.key.model,
                    key_fingerprint = %lease.key.key_fingerprint,
                    owner = %lease.owner,
                    "provider request lease guard drop did not match active lease owner"
                ),
                Ok(rows_affected) => trace!(
                    provider = %lease.key.provider_id,
                    model = %lease.key.model,
                    key_fingerprint = %lease.key.key_fingerprint,
                    owner = %lease.owner,
                    rows_affected,
                    "released provider request lease on guard drop"
                ),
                Err(err) => warn!(
                    provider = %lease.key.provider_id,
                    model = %lease.key.model,
                    key_fingerprint = %lease.key.key_fingerprint,
                    owner = %lease.owner,
                    error = %err,
                    "failed to release provider request lease on guard drop"
                ),
            }
        }));
    }
}

fn provider_request_result_from_outcome(
    outcome: &CodexResult<SamplingRequestResult>,
) -> ProviderRequestResult {
    match outcome {
        Ok(result) => {
            let token_usage = result.token_usage.as_ref();
            ProviderRequestResult::Success {
                input_tokens: token_usage.map(|usage| usage.input_tokens),
                cached_input_tokens: token_usage.map(TokenUsage::cached_input),
            }
        }
        Err(err) => provider_request_result_from_error(err),
    }
}

fn rough_prompt_token_estimate(prompt: &Prompt) -> i64 {
    let input_bytes = serde_json::to_vec(&prompt.input).map_or(0, |bytes| bytes.len());
    i64::try_from(input_bytes / 4).unwrap_or(i64::MAX)
}

fn assign_missing_streamed_response_item_id(
    item: &mut ResponseItem,
    active_item: Option<&TurnItem>,
) {
    if item.id().is_some_and(|id| !id.is_empty()) {
        return;
    }

    let active_item_id = active_item
        .map(|item| ResponseItemId::from_server(item.id()))
        .filter(|item_id| !item_id.is_empty());
    item.set_id(active_item_id);
    Session::assign_missing_response_item_id(item);
}

#[allow(clippy::too_many_arguments)]
#[instrument(level = "trace",
    skip_all,
    fields(
        turn_id = %turn_context.sub_id,
        model = %turn_context.model_info.slug
    )
)]
async fn try_run_sampling_request(
    tool_runtime: ToolCallRuntime,
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    turn_store: Arc<codex_extension_api::ExtensionData>,
    client_session: &mut ModelClientSession,
    responses_metadata: &CodexResponsesMetadata,
    turn_diff_tracker: SharedTurnDiffTracker,
    prompt: &Prompt,
    same_turn_attempt_index: u64,
    cancellation_token: CancellationToken,
) -> CodexResult<SamplingRequestResult> {
    let try_started_at = Instant::now();
    trace_turn_timing("try_run_sampling_request_start", try_started_at);
    feedback_tags!(
        model = turn_context.model_info.slug.clone(),
        approval_policy = turn_context.approval_policy.value(),
        sandbox_policy = &turn_context.sandbox_policy(),
        effort = turn_context.reasoning_effort,
        auth_mode = turn_context
            .auth_manager
            .as_ref()
            .and_then(|auth_manager| auth_manager.auth_mode()),
        features = sess.features.enabled_features(),
    );
    let inference_trace = sess.services.rollout_thread_trace.inference_trace_context(
        turn_context.sub_id.as_str(),
        turn_context.model_info.slug.as_str(),
        turn_context.provider.info().name.as_str(),
    );
    let provider_request_throttle = acquire_provider_request_lease(
        sess.as_ref(),
        turn_context.as_ref(),
        client_session,
        prompt,
        responses_metadata,
        &cancellation_token,
    )
    .await?;
    trace_turn_timing("after_provider_request_lease", try_started_at);
    let mut provider_request_lease_guard =
        ProviderRequestLeaseGuard::new(sess.as_ref(), provider_request_throttle);
    let sampling_timing_guard = turn_context.turn_timing_state.begin_sampling();
    let uses_sequential_cutoff_reasoning_summaries = turn_context
        .config
        .features
        .enabled(Feature::ConcurrentReasoningSummaries)
        && turn_context.provider.info().is_openai();
    let stream_result = client_session
        .stream_with_same_turn_attempt(
            prompt,
            &turn_context.model_info,
            &turn_context.session_telemetry,
            turn_context.reasoning_effort.clone(),
            turn_context.reasoning_summary,
            turn_context.config.service_tier.clone(),
            responses_metadata,
            &inference_trace,
            same_turn_attempt_index,
        )
        .instrument(trace_span!("stream_request"))
        .or_cancel(&cancellation_token)
        .await;
    let mut stream = match stream_result {
        Ok(Ok(stream)) => stream,
        Ok(Err(err)) => {
            provider_request_lease_guard
                .record_result(
                    sess.as_ref(),
                    provider_request_result_from_error(&err),
                    &cancellation_token,
                )
                .await;
            return Err(err);
        }
        Err(err) => {
            let err = CodexErr::from(err);
            provider_request_lease_guard
                .record_result(
                    sess.as_ref(),
                    provider_request_result_from_error(&err),
                    &cancellation_token,
                )
                .await;
            return Err(err);
        }
    };
    // Tools called from this response that read the context usage wait until
    // `response.completed` records it (#564).
    let mut response_usage = Some(sess.begin_response_usage());
    let mut in_flight: FuturesOrdered<AbortOnDropHandle<CodexResult<ResponseInputItem>>> =
        FuturesOrdered::new();
    let mut needs_follow_up = false;
    let mut last_agent_message: Option<String> = None;
    let mut active_item: Option<TurnItem> = None;
    let mut active_tool_argument_diff_consumer: Option<(
        String,
        Box<dyn ToolArgumentDiffConsumer>,
    )> = None;
    let mut should_emit_turn_diff = false;
    let mut should_emit_token_count = false;
    let mut saw_client_tool_call = false;
    let reasoning_effort = turn_context.effective_reasoning_effort_for_tracing();
    let plan_mode = turn_context.mode == ModeKind::Plan;
    let mut assistant_message_stream_parsers = AssistantMessageStreamParsers::new(plan_mode);
    let mut pending_output_text = PendingOutputText::default();
    let mut plan_mode_state = plan_mode.then(|| PlanModeStreamState::new(&turn_context.sub_id));
    let defer_streamed_turn_items_for_contributors =
        !sess.services.extensions.turn_item_contributors().is_empty();
    let mut active_item_is_streaming_to_client = false;
    let receiving_span = trace_span!("receiving_stream");
    let outcome: CodexResult<SamplingRequestResult> = loop {
        let handle_responses = trace_span!(
            parent: &receiving_span,
            "handle_responses",
            otel.name = field::Empty,
            tool_name = field::Empty,
            from = field::Empty,
            codex.request.reasoning_effort = %reasoning_effort,
            gen_ai.usage.input_tokens = field::Empty,
            gen_ai.usage.cache_read.input_tokens = field::Empty,
            gen_ai.usage.cache_write.input_tokens = field::Empty,
            gen_ai.usage.output_tokens = field::Empty,
            codex.usage.reasoning_output_tokens = field::Empty,
            codex.usage.total_tokens = field::Empty,
        );

        let event = match stream
            .next()
            .instrument(trace_span!(parent: &handle_responses, "receiving"))
            .or_cancel(&cancellation_token)
            .await
        {
            Ok(event) => event,
            Err(codex_async_utils::CancelErr::Cancelled) => {
                break Err(CodexErr::TurnAborted);
            }
        };

        let event = match event {
            Some(Ok(event)) => event,
            Some(Err(err)) => break Err(err),
            None => {
                break Err(CodexErr::Stream(
                    "stream closed before response.completed".into(),
                ));
            }
        };

        sess.services
            .session_telemetry
            .record_responses(&handle_responses, &event);
        record_turn_ttft_metric(&turn_context, &event).await;

        match event {
            ResponseEvent::Created => {}
            ResponseEvent::OutputItemDone(mut item) => {
                assign_missing_streamed_response_item_id(&mut item, active_item.as_ref());
                // #578: later requests to another provider adapt this item.
                item.set_producer(&turn_context.output_producer());
                let completed_item_id = item.id().map(|item_id| item_id.as_str().to_string());
                if let Some((_, mut consumer)) = active_tool_argument_diff_consumer.take()
                    && let Ok(Some(event)) = consumer.finish()
                {
                    sess.send_event(&turn_context, event).await;
                }
                let completes_active_item = active_item.as_ref().is_some_and(|active| {
                    completed_item_id
                        .as_deref()
                        .is_none_or(|completed_id| active.id() == completed_id)
                });
                if active_item.is_some() && !completes_active_item {
                    warn!(
                        active_item_id = active_item.as_ref().map(TurnItem::id),
                        completed_item_id,
                        "received output item completion while a different item was active"
                    );
                }
                let previously_active_item =
                    completes_active_item.then(|| active_item.take()).flatten();
                let previously_streamed_item = if active_item_is_streaming_to_client {
                    previously_active_item
                } else {
                    None
                };
                if completes_active_item {
                    active_item_is_streaming_to_client = false;
                }
                if let Some(completed_item_id) = completed_item_id.as_deref() {
                    let discarded_len =
                        pending_output_text.discard_for_completed_item(completed_item_id);
                    if discarded_len > 0 {
                        warn!(
                            item_id = completed_item_id,
                            discarded_len,
                            "discarding buffered early output text because the completed item is canonical"
                        );
                    }
                }
                if let Some(previous) = previously_streamed_item.as_ref()
                    && matches!(previous, TurnItem::AgentMessage(_))
                {
                    let item_id = previous.id();
                    flush_assistant_text_segments_for_item(
                        &sess,
                        &turn_context,
                        plan_mode_state.as_mut(),
                        &mut assistant_message_stream_parsers,
                        &item_id,
                    )
                    .await;
                }
                if let Some(state) = plan_mode_state.as_mut()
                    && handle_assistant_item_done_in_plan_mode(
                        &sess,
                        &turn_context,
                        turn_store.as_ref(),
                        &item,
                        state,
                        previously_streamed_item.as_ref(),
                        &mut last_agent_message,
                    )
                    .await
                {
                    continue;
                }

                let mut ctx = HandleOutputCtx {
                    sess: sess.clone(),
                    turn_context: turn_context.clone(),
                    turn_store: Arc::clone(&turn_store),
                    tool_runtime: tool_runtime.clone(),
                    cancellation_token: cancellation_token.child_token(),
                };

                let preempt_for_mailbox_mail = match &item {
                    ResponseItem::Message { role, phase, .. } => {
                        role == "assistant" && matches!(phase, Some(MessagePhase::Commentary))
                    }
                    ResponseItem::Reasoning { .. } => true,
                    ResponseItem::AgentMessage { .. } => false,
                    ResponseItem::AdditionalTools { .. }
                    | ResponseItem::LocalShellCall { .. }
                    | ResponseItem::FunctionCall { .. }
                    | ResponseItem::ToolSearchCall { .. }
                    | ResponseItem::FunctionCallOutput { .. }
                    | ResponseItem::CustomToolCall { .. }
                    | ResponseItem::CustomToolCallOutput { .. }
                    | ResponseItem::ToolSearchOutput { .. }
                    | ResponseItem::WebSearchCall { .. }
                    | ResponseItem::ImageGenerationCall { .. }
                    | ResponseItem::Compaction { .. }
                    | ResponseItem::CompactionTrigger { .. }
                    | ResponseItem::ContextCompaction { .. }
                    | ResponseItem::Other => false,
                };

                let output_result =
                    match handle_output_item_done(&mut ctx, item, previously_streamed_item)
                        .instrument(handle_responses)
                        .await
                    {
                        Ok(output_result) => output_result,
                        Err(err) => break Err(err),
                    };
                if let Some(tool_future) = output_result.tool_future {
                    saw_client_tool_call = true;
                    // Start tool execution as soon as its completed call item arrives. Keeping
                    // only an unpolled future in `FuturesOrdered` defers all work until the
                    // provider emits `response.completed`, defeating streamed tool dispatch.
                    in_flight.push_back(AbortOnDropHandle::new(tokio::spawn(tool_future)));
                }
                if let Some(agent_message) = output_result.last_agent_message {
                    last_agent_message = Some(agent_message);
                }
                needs_follow_up |= output_result.needs_follow_up;
                if preempt_for_mailbox_mail
                    && sess.input_queue.has_trigger_turn_mailbox_items().await
                {
                    sess.input_queue
                        .accept_mailbox_delivery_for_current_turn(
                            &sess.active_turn,
                            &turn_context.sub_id,
                        )
                        .await;
                }
            }
            ResponseEvent::OutputItemAdded(mut item) => {
                assign_missing_streamed_response_item_id(&mut item, /*active_item*/ None);
                if let ResponseItem::CustomToolCall {
                    call_id,
                    name,
                    namespace,
                    ..
                } = &item
                {
                    let tool_name = ToolName::new(namespace.clone(), name.as_str());
                    active_tool_argument_diff_consumer = tool_runtime
                        .create_diff_consumer(&tool_name)
                        .map(|consumer| (call_id.clone(), consumer));
                } else if matches!(&item, ResponseItem::FunctionCall { .. }) {
                    active_tool_argument_diff_consumer = None;
                }
                if let Some(turn_item) = handle_non_tool_response_item(
                    sess.as_ref(),
                    TurnItemContributorPolicy::Skip,
                    &item,
                    plan_mode,
                )
                .await
                {
                    let mut turn_item = turn_item;
                    let stream_item_to_client = !defer_streamed_turn_items_for_contributors;
                    let mut seeded_parsed: Option<ParsedAssistantTextDelta> = None;
                    let mut seeded_item_id: Option<String> = None;
                    if stream_item_to_client
                        && matches!(turn_item, TurnItem::AgentMessage(_))
                        && let Some(raw_text) = raw_assistant_output_text_from_item(&item)
                    {
                        let item_id = turn_item.id();
                        let mut seeded =
                            assistant_message_stream_parsers.seed_item_text(&item_id, &raw_text);
                        if let TurnItem::AgentMessage(agent_message) = &mut turn_item {
                            agent_message.content =
                                vec![codex_protocol::items::AgentMessageContent::Text {
                                    text: if plan_mode {
                                        String::new()
                                    } else {
                                        std::mem::take(&mut seeded.visible_text)
                                    },
                                }];
                        }
                        seeded_parsed = plan_mode.then_some(seeded);
                        seeded_item_id = Some(item_id);
                    }
                    if stream_item_to_client {
                        if let Some(state) = plan_mode_state.as_mut()
                            && matches!(turn_item, TurnItem::AgentMessage(_))
                        {
                            let item_id = turn_item.id();
                            state
                                .pending_agent_message_items
                                .insert(item_id, turn_item.clone());
                        } else {
                            sess.emit_turn_item_started(&turn_context, &turn_item).await;
                        }
                        if let (Some(state), Some(item_id), Some(parsed)) = (
                            plan_mode_state.as_mut(),
                            seeded_item_id.as_deref(),
                            seeded_parsed,
                        ) {
                            emit_streamed_assistant_text_delta(
                                &sess,
                                &turn_context,
                                Some(state),
                                item_id,
                                parsed,
                            )
                            .await;
                        }
                    }
                    active_item = Some(turn_item);
                    active_item_is_streaming_to_client = stream_item_to_client;
                    if let Some(active) = active_item.as_ref() {
                        let active_item_id = active.id();
                        if let Some(mut pending_delta) =
                            pending_output_text.take_for_item(&active_item_id)
                        {
                            if let Some(seeded_text) = raw_assistant_output_text_from_item(&item) {
                                if pending_delta.starts_with(&seeded_text) {
                                    pending_delta.drain(..seeded_text.len());
                                } else if seeded_text.starts_with(&pending_delta) {
                                    pending_delta.clear();
                                } else if !seeded_text.is_empty() && !pending_delta.is_empty() {
                                    break Err(CodexErr::Stream(format!(
                                        "early output text for item `{active_item_id}` could not be \
                                         reconciled with the text in output_item.added"
                                    )));
                                }
                            }
                            if stream_item_to_client && !pending_delta.is_empty() {
                                emit_output_text_delta_for_active_item(
                                    sess.as_ref(),
                                    &turn_context,
                                    plan_mode_state.as_mut(),
                                    &mut assistant_message_stream_parsers,
                                    active,
                                    pending_delta,
                                )
                                .await;
                            }
                        }
                    }
                }
            }
            ResponseEvent::ServerModel(server_model) => {
                sess.record_server_model_identity(&turn_context, &server_model);
                if !turn_context
                    .server_model_warning_emitted
                    .load(Ordering::Relaxed)
                    && sess
                        .maybe_warn_on_server_model_mismatch(&turn_context, server_model)
                        .await
                {
                    turn_context
                        .server_model_warning_emitted
                        .store(true, Ordering::Relaxed);
                }
            }
            ResponseEvent::ModelVerifications(verifications) => {
                if !turn_context
                    .model_verification_emitted
                    .swap(true, Ordering::Relaxed)
                {
                    sess.emit_model_verification(&turn_context, verifications)
                        .await;
                }
            }
            ResponseEvent::TurnModerationMetadata(metadata) => {
                sess.emit_turn_moderation_metadata(&turn_context, metadata)
                    .await;
            }
            ResponseEvent::SafetyBuffering(buffering) => {
                sess.send_event(
                    &turn_context,
                    EventMsg::SafetyBuffering(SafetyBufferingEvent {
                        model: turn_context.model_info.slug.clone(),
                        use_cases: buffering.use_cases,
                        reasons: buffering.reasons,
                        show_buffering_ui: buffering.show_buffering_ui,
                        faster_model: buffering.faster_model,
                    }),
                )
                .await;
            }
            ResponseEvent::ServerReasoningIncluded(included) => {
                sess.set_server_reasoning_included(included).await;
            }
            ResponseEvent::RateLimits(snapshot) => {
                // Update internal state with latest rate limits, but defer sending until
                // token usage is available to avoid duplicate TokenCount events.
                sess.record_rate_limits_info(snapshot).await;
                should_emit_token_count = true;
            }
            ResponseEvent::ModelsEtag(etag) => {
                // Update internal state with latest models etag. PF-84: only
                // a response from the provider and account the process list
                // follows says anything about that list.
                if sess
                    .services
                    .models_manager
                    .lists_for_provider(turn_context.provider.info())
                {
                    sess.services
                        .models_manager
                        .refresh_if_new_etag(etag, turn_context.config.http_client_factory())
                        .await;
                }
            }
            ResponseEvent::Completed {
                response_id,
                token_usage,
                end_turn,
                finish_reason,
            } => {
                if !pending_output_text.is_empty() {
                    break Err(CodexErr::Stream(format!(
                        "response completed with {} bytes of output text that could not be matched \
                         to item `{}`",
                        pending_output_text.len(),
                        pending_output_text.item_id().unwrap_or("<missing>")
                    )));
                }
                flush_assistant_text_segments_all(
                    &sess,
                    &turn_context,
                    plan_mode_state.as_mut(),
                    &mut assistant_message_stream_parsers,
                )
                .await;
                if !response_id.is_empty() {
                    sess.send_event(
                        &turn_context,
                        EventMsg::ModelResponseCompleted(ModelResponseCompletedEvent {
                            turn_id: turn_context.sub_id.clone(),
                            response_id: response_id.clone(),
                            model: turn_context.model_info.slug.clone(),
                            model_provider_id: turn_context.config.model_provider_id.clone(),
                            finish_reason: finish_reason
                                .as_ref()
                                .map(|reason| reason.as_str().to_string()),
                        }),
                    )
                    .await;
                }
                sess.send_event(
                    &turn_context,
                    EventMsg::RawResponseCompleted(RawResponseCompletedEvent {
                        response_id,
                        token_usage: token_usage.clone(),
                    }),
                )
                .await;
                let budget_result = sess
                    .record_token_usage_info(&turn_context, token_usage.as_ref())
                    .await;
                response_usage.take();
                should_emit_token_count = true;
                should_emit_turn_diff = true;
                if let Err(err) = budget_result {
                    break Err(err);
                }
                match finish_reason.as_ref() {
                    Some(codex_api::CompletionFinishReason::Length) => {
                        break Err(CodexErr::InvalidRequest(
                            "the model provider stopped generation at its output or context \
                             limit; the response may contain an incomplete tool call and the turn \
                             was stopped without executing further model work"
                                .to_string(),
                        ));
                    }
                    Some(codex_api::CompletionFinishReason::ContentFilter) => {
                        break Err(CodexErr::InvalidRequest(
                            "the model provider stopped generation because content was filtered; \
                             the turn was not completed"
                                .to_string(),
                        ));
                    }
                    Some(codex_api::CompletionFinishReason::ProviderError(reason)) => {
                        break Err(CodexErr::Stream(format!(
                            "the model provider ended the completion with retryable finish \
                                 reason `{reason}`"
                        )));
                    }
                    Some(codex_api::CompletionFinishReason::Unknown(reason)) => {
                        break Err(CodexErr::InvalidRequest(format!(
                            "the model provider returned an unknown completion finish reason \
                             `{reason}`; the turn was not marked complete"
                        )));
                    }
                    _ => {}
                }
                if let Some(false) = end_turn {
                    needs_follow_up = true;
                }
                let server_side_model_continuation = matches!(end_turn, Some(false))
                    && !saw_client_tool_call
                    && in_flight.is_empty();
                let provider_stopped =
                    matches!(finish_reason, Some(codex_api::CompletionFinishReason::Stop));
                let completed_token_usage = token_usage.clone();
                break Ok(SamplingRequestResult {
                    needs_follow_up,
                    last_agent_message,
                    token_usage: completed_token_usage,
                    server_side_model_continuation,
                    made_tool_progress: saw_client_tool_call,
                    provider_stopped,
                });
            }
            ResponseEvent::OutputTextDelta { item_id, delta } => {
                // In review child threads, suppress assistant text deltas; the
                // UI will show a selection popup from the final ReviewOutput.
                if let Some(active) = active_item.as_ref() {
                    let active_item_id = active.id();
                    if item_id
                        .as_deref()
                        .is_none_or(|delta_item_id| delta_item_id == active_item_id.as_str())
                    {
                        if !active_item_is_streaming_to_client {
                            continue;
                        }
                        emit_output_text_delta_for_active_item(
                            sess.as_ref(),
                            &turn_context,
                            plan_mode_state.as_mut(),
                            &mut assistant_message_stream_parsers,
                            active,
                            delta,
                        )
                        .await;
                    } else {
                        let was_empty = pending_output_text.is_empty();
                        if let Err(message) = pending_output_text.push(item_id.clone(), delta) {
                            break Err(CodexErr::Stream(message));
                        }
                        if was_empty {
                            warn!(
                                delta_item_id = item_id,
                                active_item_id,
                                "buffering output text delta until its item becomes active"
                            );
                        }
                    }
                } else {
                    let was_empty = pending_output_text.is_empty();
                    if let Err(message) = pending_output_text.push(item_id.clone(), delta) {
                        break Err(CodexErr::Stream(message));
                    }
                    if was_empty {
                        warn!(
                            delta_item_id = item_id,
                            "buffering output text delta before its item becomes active"
                        );
                    }
                }
            }
            ResponseEvent::ToolCallInputDelta {
                item_id: _,
                call_id,
                delta,
            } => {
                let Some((active_call_id, consumer)) = active_tool_argument_diff_consumer.as_mut()
                else {
                    continue;
                };
                let call_id = match call_id {
                    Some(call_id) if call_id.as_str() != active_call_id.as_str() => continue,
                    Some(call_id) => call_id,
                    None => active_call_id.clone(),
                };
                if let Some(event) = consumer.consume_diff(turn_context.as_ref(), call_id, &delta) {
                    sess.send_event(&turn_context, event).await;
                }
            }
            ResponseEvent::ReasoningSummaryDelta {
                delta,
                summary_index,
            } => {
                if uses_sequential_cutoff_reasoning_summaries {
                    continue;
                }
                if let Some(active) = active_item.as_ref() {
                    if !active_item_is_streaming_to_client {
                        continue;
                    }
                    let event = ReasoningContentDeltaEvent {
                        thread_id: sess.thread_id.to_string(),
                        turn_id: turn_context.sub_id.clone(),
                        item_id: active.id(),
                        delta,
                        summary_index,
                    };
                    sess.send_event(&turn_context, EventMsg::ReasoningContentDelta(event))
                        .await;
                } else {
                    error_or_panic("ReasoningSummaryDelta without active item".to_string());
                }
            }
            ResponseEvent::ReasoningSummaryPartAdded { summary_index } => {
                if uses_sequential_cutoff_reasoning_summaries {
                    continue;
                }
                if let Some(active) = active_item.as_ref() {
                    if !active_item_is_streaming_to_client {
                        continue;
                    }
                    let event =
                        EventMsg::AgentReasoningSectionBreak(AgentReasoningSectionBreakEvent {
                            item_id: active.id(),
                            summary_index,
                        });
                    sess.send_event(&turn_context, event).await;
                } else {
                    error_or_panic("ReasoningSummaryPartAdded without active item".to_string());
                }
            }
            ResponseEvent::ReasoningSummaryDone {
                item_id,
                text,
                summary_index,
            } => {
                if !uses_sequential_cutoff_reasoning_summaries {
                    continue;
                }
                let Some(active) = active_item.as_ref() else {
                    continue;
                };
                if !active_item_is_streaming_to_client || active.id() != item_id {
                    continue;
                }
                if summary_index > 0 {
                    sess.send_event(
                        &turn_context,
                        EventMsg::AgentReasoningSectionBreak(AgentReasoningSectionBreakEvent {
                            item_id: item_id.clone(),
                            summary_index,
                        }),
                    )
                    .await;
                }
                let event = ReasoningContentDeltaEvent {
                    thread_id: sess.thread_id.to_string(),
                    turn_id: turn_context.sub_id.clone(),
                    item_id,
                    delta: text,
                    summary_index,
                };
                sess.send_event(&turn_context, EventMsg::ReasoningContentDelta(event))
                    .await;
            }
            ResponseEvent::ReasoningContentDelta {
                delta,
                content_index,
            } => {
                if let Some(active) = active_item.as_ref() {
                    if !active_item_is_streaming_to_client {
                        continue;
                    }
                    let event = ReasoningRawContentDeltaEvent {
                        thread_id: sess.thread_id.to_string(),
                        turn_id: turn_context.sub_id.clone(),
                        item_id: active.id(),
                        delta,
                        content_index,
                    };
                    sess.send_event(&turn_context, EventMsg::ReasoningRawContentDelta(event))
                        .await;
                } else {
                    error_or_panic("ReasoningRawContentDelta without active item".to_string());
                }
            }
        }
    };
    drop(sampling_timing_guard);
    // No more usage arrives for this response. Tools still running (and waiting
    // for it) must not wait for the drain below, which waits for them.
    drop(response_usage);

    flush_assistant_text_segments_all(
        &sess,
        &turn_context,
        plan_mode_state.as_mut(),
        &mut assistant_message_stream_parsers,
    )
    .await;

    provider_request_lease_guard
        .record_result(
            sess.as_ref(),
            provider_request_result_from_outcome(&outcome),
            &cancellation_token,
        )
        .await;

    let tool_blocking_timing_guard = if in_flight.is_empty() {
        None
    } else {
        Some(turn_context.turn_timing_state.begin_tool_blocking())
    };
    drain_in_flight(&mut in_flight, sess.clone(), turn_context.clone()).await?;
    drop(tool_blocking_timing_guard);

    if should_emit_token_count {
        // A tool call such as request_user_input can intentionally pause the turn. Emit token
        // counts only after pending tools resolve so clients do not see progress events while the
        // turn is waiting on the user. This also needs to happen before returning cancellation so
        // token usage already recorded from the completed response is still persisted.
        sess.send_token_count_event(&turn_context).await;
    }

    if cancellation_token.is_cancelled() {
        return Err(CodexErr::TurnAborted);
    }

    if should_emit_turn_diff {
        let unified_diff = {
            let tracker = turn_diff_tracker.lock().await;
            tracker.get_unified_diff()
        };
        if let Some(unified_diff) = unified_diff {
            let msg = EventMsg::TurnDiff(TurnDiffEvent { unified_diff });
            sess.clone().send_event(&turn_context, msg).await;
        }
    }

    outcome
}

pub(crate) fn get_last_assistant_message_from_turn(responses: &[ResponseItem]) -> Option<String> {
    for item in responses.iter().rev() {
        if let Some(message) = last_assistant_message_from_item(item, /*plan_mode*/ false) {
            return Some(message);
        }
    }
    None
}

#[cfg(test)]
impl Session {
    pub(crate) async fn lock_state_for_accounting_fixture(&self) -> impl Drop + '_ {
        self.state.lock().await
    }
}

#[cfg(test)]
#[path = "turn_tests.rs"]
mod tests;
