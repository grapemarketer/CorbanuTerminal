//! Slash-command dispatch and local-recall handoff for `ChatWidget`.
//!
//! `ChatComposer` parses slash input and stages recognized command text for local
//! Up-arrow recall before returning an input result. This module owns the app-level
//! dispatch step and records the staged entry once the command has been handled, so
//! slash-command recall follows the same submitted-input rule as ordinary text.

use super::*;
use crate::app_event::ThreadGoalSetMode;
use crate::bottom_pane::prompt_args::parse_slash_name;
use crate::bottom_pane::slash_commands::BuiltinCommandFlags;
use crate::bottom_pane::slash_commands::ServiceTierCommand;
use crate::bottom_pane::slash_commands::SlashCommandItem;
use crate::bottom_pane::slash_commands::find_slash_command;
use crate::goal_display::GOAL_USAGE;
use crate::goal_files::GoalDraft;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlashCommandDispatchSource {
    Live,
    Queued,
}

struct PreparedSlashCommandArgs {
    args: String,
    text_elements: Vec<TextElement>,
    pending_pastes: Vec<(String, String)>,
    local_images: Vec<LocalImageAttachment>,
    remote_image_urls: Vec<String>,
    mention_bindings: Vec<MentionBinding>,
    source: SlashCommandDispatchSource,
}

const SIDE_STARTING_CONTEXT_LABEL: &str = "Side starting...";
const SIDE_SLASH_COMMAND_UNAVAILABLE_HINT: &str =
    "Press Ctrl+C to return to the main thread first.";
const GOAL_USAGE_HINT: &str = "Example: /goal improve benchmark coverage";
const RAW_USAGE: &str = "Usage: /raw [on|off]";
/// Account usage is an OpenAI account API and needs that sign-in. Where costs
/// are recorded locally (`/cost`, developer builds only), the message points
/// there; other builds have no per-request cost view to point at.
const USAGE_CHATGPT_LOGIN_REQUIRED: &str = if cfg!(feature = "developer-accounting") {
    "Sign in with ChatGPT to view OpenAI account usage. For what your turns cost on this provider, run `/cost`."
} else {
    "Sign in with ChatGPT to view OpenAI account usage."
};

fn tasknode_new_chat_id() -> String {
    format!("chat_{}", uuid::Uuid::new_v4().simple())
}

impl ChatWidget {
    fn open_mkdocs_viewer(&mut self, args: Option<String>) {
        let tx = self.app_event_tx.clone();
        let cwd = self
            .current_cwd
            .clone()
            .unwrap_or_else(|| self.config.cwd.to_path_buf());
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                crate::mkdocs_viewer::load_mkdocs_site(&cwd, args.as_deref())
                    .map_err(|err| err.to_string())
            })
            .await
            .unwrap_or_else(|err| Err(format!("Failed to build MkDocs view: {err}")));
            tx.send(AppEvent::MkDocsResult(result));
        });
    }

    /// Dispatch a bare slash command and record its staged local-history entry.
    ///
    /// The composer stages history before returning `InputResult::Command`; this wrapper commits
    /// that staged entry after dispatch so slash-command recall follows the same "submitted input"
    /// rule as normal text.
    pub(super) fn handle_slash_command_dispatch(&mut self, cmd: SlashCommand) {
        self.dispatch_command(cmd);
        if cmd == SlashCommand::Goal {
            self.bottom_pane.drain_pending_submission_state();
        }
        self.bottom_pane.record_pending_slash_command_history();
    }

    pub(super) fn handle_service_tier_command_dispatch(&mut self, command: ServiceTierCommand) {
        if self.active_side_conversation {
            self.add_error_message(format!(
                "'/{}' is unavailable in side conversations. {SIDE_SLASH_COMMAND_UNAVAILABLE_HINT}",
                command.name
            ));
            self.bottom_pane.drain_pending_submission_state();
            self.bottom_pane.record_pending_slash_command_history();
            return;
        }
        self.toggle_service_tier_from_ui(command);
        self.bottom_pane.record_pending_slash_command_history();
    }

    /// Dispatch an inline slash command and record its staged local-history entry.
    ///
    /// Inline command arguments may later be prepared through the normal submission pipeline, but
    /// local command recall still tracks the original command invocation. Treating this wrapper as
    /// the only input-result entry point avoids double-recording commands with inline args.
    pub(super) fn handle_slash_command_with_args_dispatch(
        &mut self,
        cmd: SlashCommand,
        args: String,
        text_elements: Vec<TextElement>,
    ) {
        self.dispatch_command_with_args(cmd, args, text_elements);
        self.bottom_pane.record_pending_slash_command_history();
    }

    fn apply_plan_slash_command(&mut self) -> bool {
        if !self.collaboration_modes_enabled() {
            self.add_info_message(
                "Collaboration modes are disabled.".to_string(),
                Some("Enable collaboration modes to use /plan.".to_string()),
            );
            return false;
        }
        if let Some(mask) = collaboration_modes::plan_mask(self.model_catalog.as_ref()) {
            self.set_collaboration_mask_from_user_action(mask);
            true
        } else {
            self.add_info_message(
                "Plan mode unavailable right now.".to_string(),
                /*hint*/ None,
            );
            false
        }
    }

    fn request_side_conversation(
        &mut self,
        parent_thread_id: ThreadId,
        user_message: Option<UserMessage>,
    ) {
        self.set_side_conversation_context_label(Some(SIDE_STARTING_CONTEXT_LABEL.to_string()));
        self.request_redraw();
        self.app_event_tx.send(AppEvent::StartSide {
            parent_thread_id,
            user_message,
        });
    }

    fn request_empty_side_conversation(&mut self, cmd: SlashCommand) {
        let Some(parent_thread_id) = self.thread_id else {
            let command = cmd.command();
            self.add_error_message(format!(
                "'/{command}' is unavailable before the session starts."
            ));
            return;
        };

        self.request_side_conversation(parent_thread_id, /*user_message*/ None);
    }

    fn emit_raw_output_mode_changed(&self, enabled: bool) {
        self.app_event_tx
            .send(AppEvent::RawOutputModeChanged { enabled });
    }

    fn slash_command_blocked_by_active_task(&self, cmd: SlashCommand) -> bool {
        (!cmd.available_during_task()
            && (self.turn_lifecycle.agent_turn_running
                || self.review.is_review_mode
                || (self.bottom_pane.is_task_running()
                    && (self.mcp_startup_status.is_none()
                        || self.input_queue.user_turn_pending_start))))
            || (cmd == SlashCommand::Resume
                && (self.input_queue.user_turn_pending_start
                    || self.turn_lifecycle.agent_turn_running))
    }

    pub(super) fn dispatch_command(&mut self, cmd: SlashCommand) {
        if !self.ensure_slash_command_allowed_in_side_conversation(cmd) {
            return;
        }
        if !self.ensure_side_command_allowed_outside_review(cmd) {
            return;
        }
        if self.slash_command_blocked_by_active_task(cmd) {
            let message = format!(
                "'/{}' is disabled while a task is in progress.",
                cmd.command()
            );
            self.add_to_history(history_cell::new_error_event(message));
            self.bottom_pane.drain_pending_submission_state();
            self.request_redraw();
            return;
        }

        match cmd {
            SlashCommand::Feedback => {
                if !self.config.feedback_enabled {
                    let params = crate::bottom_pane::feedback_disabled_params();
                    self.bottom_pane.show_selection_view(params);
                    self.request_redraw();
                    return;
                }
                // Step 1: pick a category (UI built in feedback_view)
                let params =
                    crate::bottom_pane::feedback_selection_params(self.app_event_tx.clone());
                self.bottom_pane.show_selection_view(params);
                self.request_redraw();
            }
            SlashCommand::New => {
                self.app_event_tx.send(AppEvent::NewSession { name: None });
            }
            SlashCommand::Archive => {
                self.bottom_pane.show_selection_view(SelectionViewParams {
                    title: Some("Archive this session?".to_string()),
                    subtitle: Some(
                        "This will archive the current session and exit Corbanu Terminal."
                            .to_string(),
                    ),
                    footer_hint: Some(standard_popup_hint_line()),
                    items: vec![
                        SelectionItem {
                            name: "No, don't archive".to_string(),
                            description: Some("Return to the current session".to_string()),
                            dismiss_on_select: true,
                            ..Default::default()
                        },
                        SelectionItem {
                            name: "Yes, archive and exit".to_string(),
                            description: Some("Archive this session now".to_string()),
                            actions: vec![Box::new(|tx| {
                                tx.send(AppEvent::ArchiveCurrentThread);
                            })],
                            dismiss_on_select: true,
                            ..Default::default()
                        },
                    ],
                    allow_number_shortcuts: false,
                    ..Default::default()
                });
                self.request_redraw();
            }
            SlashCommand::Delete => {
                self.bottom_pane.show_selection_view(SelectionViewParams {
                    title: Some("Delete this session?".to_string()),
                    subtitle: Some(
                        "Cannot be undone. Subagent threads will also be deleted.".to_string(),
                    ),
                    footer_hint: Some(standard_popup_hint_line()),
                    items: vec![
                        SelectionItem {
                            name: "No, keep this session".to_string(),
                            description: Some("Return to the current session".to_string()),
                            dismiss_on_select: true,
                            ..Default::default()
                        },
                        SelectionItem {
                            name: "Yes, delete and exit".to_string(),
                            description: Some("Permanently delete this session now".to_string()),
                            actions: vec![Box::new(|tx| {
                                tx.send(AppEvent::DeleteCurrentThread);
                            })],
                            dismiss_on_select: true,
                            ..Default::default()
                        },
                    ],
                    allow_number_shortcuts: false,
                    ..Default::default()
                });
                self.request_redraw();
            }
            SlashCommand::Clear => {
                self.app_event_tx.send(AppEvent::ClearUi { name: None });
            }
            SlashCommand::Resume => {
                self.app_event_tx.send(AppEvent::OpenResumePicker);
            }
            SlashCommand::Fork => {
                self.app_event_tx
                    .send(AppEvent::ForkCurrentSession { name: None });
            }
            SlashCommand::App => {
                let Some(thread_id) = self.thread_id else {
                    self.add_error_message(
                        "Session is still starting; try /app again in a moment.".to_string(),
                    );
                    return;
                };
                self.app_event_tx
                    .send(AppEvent::OpenDesktopThread { thread_id });
            }
            SlashCommand::Init => {
                const INIT_PROMPT: &str = include_str!("../../prompt_for_init_command.md");
                self.submit_user_message(INIT_PROMPT.to_string().into());
            }
            SlashCommand::Compact => {
                if self.blocks_direct_input {
                    self.add_error_message(PARENT_OWNED_INPUT_MESSAGE.to_string());
                    return;
                }
                self.clear_token_usage();
                if !self.bottom_pane.is_task_running() {
                    self.bottom_pane.set_task_running(/*running*/ true);
                }
                self.input_queue.user_turn_pending_start = true;
                self.app_event_tx.compact();
            }
            SlashCommand::Review => {
                self.open_review_popup();
                if self.mcp_startup_status.is_some() {
                    self.defer_input_until_settings_applied();
                }
            }
            SlashCommand::Rename => {
                self.session_telemetry
                    .counter("codex.thread.rename", /*inc*/ 1, &[]);
                self.show_rename_prompt();
            }
            SlashCommand::Model => {
                self.open_model_popup();
                self.defer_input_until_settings_applied();
            }
            SlashCommand::Gpu => {
                self.app_event_tx.send(AppEvent::OpenGpuMenu);
            }
            SlashCommand::Personality => {
                self.open_personality_popup();
                self.defer_input_until_settings_applied();
            }
            SlashCommand::Plan => {
                self.apply_plan_slash_command();
            }
            SlashCommand::Goal => {
                if !self.config.features.enabled(Feature::Goals) {
                    return;
                }
                if let Some(thread_id) = self.thread_id {
                    self.app_event_tx
                        .send(AppEvent::OpenThreadGoalMenu { thread_id });
                    self.append_message_history_entry("/goal".to_string());
                } else {
                    self.add_info_message(
                        GOAL_USAGE.to_string(),
                        Some(GOAL_USAGE_HINT.to_string()),
                    );
                }
            }
            SlashCommand::Side | SlashCommand::Btw => {
                self.request_empty_side_conversation(cmd);
            }
            SlashCommand::Agent | SlashCommand::MultiAgents => {
                self.app_event_tx.send(AppEvent::OpenAgentPicker);
            }
            SlashCommand::Spawn => {
                self.app_event_tx.send(AppEvent::OpenSpawnRolePicker);
            }
            SlashCommand::Orchestrate => {
                self.app_event_tx.send(AppEvent::HandleOrchestrateCommand {
                    args: String::new(),
                });
            }
            SlashCommand::Tasknode => {
                self.app_event_tx.send(AppEvent::OpenTaskNodeMenu);
            }
            SlashCommand::Panes => {
                self.app_event_tx.send(AppEvent::OpenPanePicker);
            }
            SlashCommand::Permissions => {
                self.open_permissions_popup();
                self.defer_input_until_settings_applied();
            }
            SlashCommand::Security => {
                let core = crate::bottom_pane::security_level_picker::CoreLevels {
                    thread: self.thread_id(),
                    // #1047: a remote app server (or the local daemon) runs
                    // this window's sessions in its own process.
                    sessions_in_this_process: self.remote_connection.is_none(),
                    configured: crate::legacy_core::security_level_change::configured_level(
                        &self.config,
                    ),
                    outside_user_config:
                        crate::legacy_core::security_level_change::non_user_level_floor(
                            &self.config,
                        ),
                };
                // #691/#783: only Windows needs its sandbox set up before
                // Moderate or Aggressive can be chosen.
                #[cfg(target_os = "windows")]
                let windows_sandbox_steps =
                    crate::windows_sandbox::protected_level_setup_steps(&self.config)
                        .map(str::to_string);
                #[cfg(not(target_os = "windows"))]
                let windows_sandbox_steps = None;
                self.bottom_pane.show_view(Box::new(
                    crate::bottom_pane::security_view::SecurityView::new(
                        Some(self.config.security_level),
                        || {
                            crate::security::current::current_values(
                                &self.config,
                                crate::security::current::level_now(&self.config, core.thread),
                            )
                        },
                        || {
                            self.config
                                .features
                                .enabled(codex_features::Feature::ProtectedModePreflight)
                                .then(|| {
                                    crate::security::preflight::PreflightInput::from_config(
                                        &self.config,
                                    )
                                })
                        },
                        self.bottom_pane.list_keymap(),
                    )
                    .with_confirmation(core, self.app_event_tx.clone())
                    .with_moderate_facts(crate::security::moderate::Facts::from_config(
                        &self.config,
                    ))
                    .with_before_restart(|| crate::security::current::before_restart(&self.config))
                    .with_windows_sandbox_steps(windows_sandbox_steps)
                    .with_aggressive_home_link(crate::security::home_link::aggressive_blocker(
                        &self.config.codex_home,
                        self.config.cwd.as_path(),
                    ))
                    .with_inspector(|| {
                        crate::security::inspector::InspectorInput::from_config(
                            &self.config,
                            core.thread,
                            core.configured,
                            core.outside_user_config,
                        )
                    }),
                ));
            }
            SlashCommand::Vim => {
                self.toggle_vim_mode_and_notify();
            }
            SlashCommand::Keymap => {
                self.open_keymap_picker();
            }
            SlashCommand::ElevateSandbox => {
                #[cfg(target_os = "windows")]
                {
                    let windows_sandbox_level =
                        crate::windows_sandbox::level_from_config(&self.config);
                    let windows_degraded_sandbox_enabled =
                        matches!(windows_sandbox_level, WindowsSandboxLevel::RestrictedToken);
                    if !windows_degraded_sandbox_enabled {
                        // This command should not be visible/recognized outside degraded mode,
                        // but guard anyway in case something dispatches it directly.
                        return;
                    }

                    let Some(preset) = builtin_approval_presets()
                        .into_iter()
                        .find(|preset| preset.id == "auto")
                    else {
                        // Avoid panicking in interactive UI; treat this as a recoverable
                        // internal error.
                        self.add_error_message(
                            "Internal error: missing the 'auto' approval preset.".to_string(),
                        );
                        return;
                    };

                    if let Err(err) = self
                        .config
                        .permissions
                        .approval_policy
                        .can_set(&preset.approval)
                    {
                        self.add_error_message(err.to_string());
                        return;
                    }

                    self.session_telemetry.counter(
                        "codex.windows_sandbox.setup_elevated_sandbox_command",
                        /*inc*/ 1,
                        &[],
                    );
                    self.app_event_tx
                        .send(AppEvent::BeginWindowsSandboxElevatedSetup {
                            preset,
                            profile_selection: None,
                        });
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = &self.session_telemetry;
                    // Not supported; on non-Windows this command should never be reachable.
                }
            }
            SlashCommand::SandboxReadRoot => {
                self.add_error_message(
                    "Usage: /sandbox-add-read-dir <absolute-directory-path>".to_string(),
                );
            }
            SlashCommand::Experimental => {
                self.open_experimental_popup();
            }
            SlashCommand::AutoReview => {
                self.open_auto_review_denials_popup();
            }
            SlashCommand::Memories => {
                self.open_memories_popup();
            }
            SlashCommand::Quit | SlashCommand::Exit => {
                self.request_quit_without_confirmation();
            }
            SlashCommand::Logout => {
                self.app_event_tx.send(AppEvent::Logout);
            }
            SlashCommand::Copy => {
                self.copy_last_agent_markdown();
            }
            SlashCommand::Raw => {
                let enabled = self.toggle_raw_output_mode_and_notify();
                self.emit_raw_output_mode_changed(enabled);
            }
            SlashCommand::Diff => {
                self.add_diff_in_progress();
                let tx = self.app_event_tx.clone();
                let runner = self.workspace_command_runner.clone();
                let cwd = self
                    .current_cwd
                    .clone()
                    .unwrap_or_else(|| self.config.cwd.to_path_buf());
                tokio::spawn(async move {
                    let text = match runner {
                        Some(runner) => match get_git_diff(runner.as_ref(), &cwd).await {
                            Ok((is_git_repo, diff_text)) => {
                                if is_git_repo {
                                    diff_text
                                } else {
                                    "`/diff` — _not inside a git repository_".to_string()
                                }
                            }
                            Err(e) => format!("Failed to compute diff: {e}"),
                        },
                        None => "Failed to compute diff: workspace command runner unavailable"
                            .to_string(),
                    };
                    tx.send(AppEvent::DiffResult(text));
                });
            }
            SlashCommand::Docs => {
                self.open_mkdocs_viewer(/*args*/ None);
            }
            SlashCommand::Mention => {
                self.insert_str("@");
            }
            SlashCommand::Skills => {
                self.open_skills_menu();
            }
            SlashCommand::Import => {
                self.app_event_tx
                    .send(AppEvent::OpenExternalAgentConfigMigration);
            }
            SlashCommand::Hooks => {
                self.add_hooks_output();
            }
            SlashCommand::Status => {
                if self.should_prefetch_rate_limits() {
                    let request_id = self.next_status_refresh_request_id;
                    self.next_status_refresh_request_id =
                        self.next_status_refresh_request_id.wrapping_add(1);
                    self.add_status_output(/*refreshing_rate_limits*/ true, Some(request_id));
                    self.app_event_tx.send(AppEvent::RefreshRateLimits {
                        origin: RateLimitRefreshOrigin::StatusCommand { request_id },
                    });
                } else {
                    self.add_status_output(
                        /*refreshing_rate_limits*/ false, /*request_id*/ None,
                    );
                }
            }
            SlashCommand::Usage => {
                if self.ensure_usage_command_available() {
                    self.open_usage_menu();
                }
            }
            // No arguments means the current UTC day, which is the question an
            // operator actually has when they type this.
            SlashCommand::Cost => {
                self.open_cost_command("", chrono::Utc::now().date_naive());
            }
            SlashCommand::Ide => {
                self.handle_ide_command();
            }
            SlashCommand::DebugConfig => {
                self.add_debug_config_output();
            }
            SlashCommand::Title => {
                self.open_terminal_title_setup();
            }
            SlashCommand::Statusline => {
                self.open_status_line_setup();
            }
            SlashCommand::Theme => {
                self.open_theme_picker();
            }
            SlashCommand::Pets => {
                self.open_pets_picker();
            }
            SlashCommand::Ps => {
                self.add_ps_output();
            }
            SlashCommand::Stop => {
                self.clean_background_terminals();
            }
            SlashCommand::MemoryDrop => {
                self.add_app_server_stub_message("Memory maintenance");
            }
            SlashCommand::MemoryUpdate => {
                self.add_app_server_stub_message("Memory maintenance");
            }
            SlashCommand::Vault => {
                // Bare `/vault` opens the action menu. Subcommands flow through the inline-args
                // path and still render command output.
                self.open_vault_menu();
            }
            SlashCommand::Wallet => {
                self.open_wallet_menu();
            }
            SlashCommand::Providers => {
                self.app_event_tx.send(AppEvent::OpenProviderManager);
            }
            SlashCommand::Telegram => {
                self.open_telegram_menu();
            }
            SlashCommand::Mcp => {
                self.add_mcp_output(McpServerStatusDetail::ToolsAndAuthOnly);
            }
            SlashCommand::Apps => {
                self.add_connectors_output();
            }
            SlashCommand::Plugins => {
                self.add_plugins_output();
            }
            SlashCommand::Rollout => {
                if let Some(path) = self.rollout_path() {
                    self.add_info_message(
                        format!("Current rollout path: {}", path.display()),
                        /*hint*/ None,
                    );
                } else {
                    self.add_info_message(
                        "Rollout path is not available yet.".to_string(),
                        /*hint*/ None,
                    );
                }
            }
            SlashCommand::TestApproval => {
                use std::collections::HashMap;

                use crate::approval_events::ApplyPatchApprovalRequestEvent;
                use crate::approval_events::ApprovalResponseDestination;
                use crate::diff_model::FileChange;

                self.on_apply_patch_approval_request(
                    "1".to_string(),
                    ApplyPatchApprovalRequestEvent {
                        call_id: "1".to_string(),
                        turn_id: "turn-1".to_string(),
                        changes: HashMap::from([
                            (
                                PathBuf::from("/tmp/test.txt"),
                                FileChange::Add {
                                    content: "test".to_string(),
                                },
                            ),
                            (
                                PathBuf::from("/tmp/test2.txt"),
                                FileChange::Update {
                                    unified_diff: "+test\n-test2".to_string(),
                                    move_path: None,
                                },
                            ),
                        ]),
                        reason: None,
                        grant_root: Some(PathBuf::from("/tmp")),
                        response_destination: ApprovalResponseDestination::Local,
                    },
                );
            }
        }
    }

    /// Run an inline slash command.
    ///
    /// Branches that prepare arguments should pass `record_history: false` to the composer because
    /// the staged slash-command entry is the recall record; using the normal submission-history
    /// path as well would make a single command appear twice during Up-arrow navigation.
    pub(super) fn dispatch_command_with_args(
        &mut self,
        cmd: SlashCommand,
        args: String,
        text_elements: Vec<TextElement>,
    ) {
        if !self.ensure_slash_command_allowed_in_side_conversation(cmd) {
            return;
        }
        if !self.ensure_side_command_allowed_outside_review(cmd) {
            return;
        }
        if !cmd.supports_inline_args() {
            self.add_info_message(
                format!(
                    "'/{}' does not take inline arguments; running the bare command.",
                    cmd.command()
                ),
                /*hint*/ None,
            );
            self.bottom_pane
                .set_composer_text(String::new(), Vec::new(), Vec::new());
            self.bottom_pane.set_composer_pending_pastes(Vec::new());
            self.bottom_pane.drain_pending_submission_state();
            self.dispatch_command(cmd);
            return;
        }
        if self.slash_command_blocked_by_active_task(cmd) {
            let message = format!(
                "'/{}' is disabled while a task is in progress.",
                cmd.command()
            );
            self.add_to_history(history_cell::new_error_event(message));
            self.request_redraw();
            return;
        }

        let trimmed = args.trim();
        if trimmed.is_empty() {
            self.dispatch_command(cmd);
            return;
        }

        if cmd == SlashCommand::Goal {
            self.dispatch_prepared_command_with_args(
                cmd,
                PreparedSlashCommandArgs {
                    args,
                    text_elements,
                    pending_pastes: self.bottom_pane.composer_pending_pastes(),
                    local_images: self.bottom_pane.composer_local_images(),
                    remote_image_urls: self.bottom_pane.remote_image_urls(),
                    mention_bindings: Vec::new(),
                    source: SlashCommandDispatchSource::Live,
                },
            );
            return;
        }

        let Some((prepared_args, prepared_elements)) =
            self.prepare_live_inline_args(args, text_elements)
        else {
            return;
        };
        self.dispatch_prepared_command_with_args(
            cmd,
            PreparedSlashCommandArgs {
                args: prepared_args,
                text_elements: prepared_elements,
                pending_pastes: Vec::new(),
                local_images: Vec::new(),
                remote_image_urls: Vec::new(),
                mention_bindings: Vec::new(),
                source: SlashCommandDispatchSource::Live,
            },
        );
    }

    fn prepare_live_inline_args(
        &mut self,
        args: String,
        text_elements: Vec<TextElement>,
    ) -> Option<(String, Vec<TextElement>)> {
        if self.bottom_pane.composer_text().is_empty() {
            Some((args, text_elements))
        } else {
            self.bottom_pane
                .prepare_inline_args_submission(/*record_history*/ false)
        }
    }

    fn clear_live_goal_submission(&mut self) {
        self.bottom_pane
            .set_composer_text(String::new(), Vec::new(), Vec::new());
        self.bottom_pane.set_composer_pending_pastes(Vec::new());
        self.bottom_pane.drain_pending_submission_state();
    }

    fn prepared_inline_user_message(
        &mut self,
        args: String,
        text_elements: Vec<TextElement>,
        mut local_images: Vec<LocalImageAttachment>,
        mut remote_image_urls: Vec<String>,
        mut mention_bindings: Vec<MentionBinding>,
        source: SlashCommandDispatchSource,
    ) -> UserMessage {
        if source == SlashCommandDispatchSource::Live {
            local_images = self
                .bottom_pane
                .take_recent_submission_images_with_placeholders();
            remote_image_urls = self.take_remote_image_urls();
            mention_bindings = self.bottom_pane.take_recent_submission_mention_bindings();
        }
        UserMessage {
            text: args,
            local_images,
            remote_image_urls,
            text_elements,
            mention_bindings,
        }
    }

    fn dispatch_prepared_command_with_args(
        &mut self,
        cmd: SlashCommand,
        prepared: PreparedSlashCommandArgs,
    ) {
        let PreparedSlashCommandArgs {
            args,
            text_elements,
            pending_pastes,
            local_images,
            remote_image_urls,
            mention_bindings,
            source,
        } = prepared;
        let trimmed = args.trim();
        match cmd {
            SlashCommand::Usage => {
                if trimmed.split_whitespace().next() == Some("requests") {
                    // `/cost` is developer-only (PF-60-S03): a build that
                    // records no costs says so instead of opening a view it
                    // could never fill.
                    if cfg!(feature = "developer-accounting") {
                        self.open_accounting_command(trimmed, chrono::Utc::now().date_naive());
                    } else {
                        self.add_info_message(
                            tokens::NO_COST_HISTORY.to_string(),
                            /*hint*/ None,
                        );
                    }
                } else if self.ensure_usage_command_available() {
                    match tokens::TokenActivityView::parse(trimmed) {
                        Some(view) => self.add_token_activity_output(view),
                        None => self.add_error_message(
                            if cfg!(feature = "developer-accounting") {
                                "Usage: /usage [daily|weekly|cumulative] or /usage requests [YYYY-MM-DD]"
                            } else {
                                "Usage: /usage [daily|weekly|cumulative]"
                            }
                            .to_string(),
                        ),
                    }
                }
            }
            // The recorded-request view, reachable by its own name. It reads
            // this machine's ledger for whatever provider served the turn, so
            // unlike account usage it needs no particular sign-in - and it was
            // previously reachable only as an undocumented argument to a
            // command that hid itself without one.
            SlashCommand::Cost => {
                self.open_cost_command(trimmed, chrono::Utc::now().date_naive());
            }
            SlashCommand::Ide => {
                self.handle_ide_command_args(trimmed);
            }
            SlashCommand::Mcp => match trimmed.to_ascii_lowercase().as_str() {
                "verbose" => self.add_mcp_output(McpServerStatusDetail::Full),
                _ => self.add_error_message("Usage: /mcp [verbose]".to_string()),
            },
            SlashCommand::Vault => {
                // `/vault credential add` opens the secure (masked) entry overlay rather than
                // accepting a secret as chat text. Reject extra args so accidental inline secrets
                // do not enter local slash-command recall.
                let parts = trimmed.split_whitespace().collect::<Vec<_>>();
                let credential_action = parts
                    .first()
                    .is_some_and(|part| part.eq_ignore_ascii_case("credential"));
                if credential_action
                    && parts
                        .get(1)
                        .is_some_and(|part| part.eq_ignore_ascii_case("add"))
                {
                    if parts.len() != 2 {
                        self.bottom_pane.clear_pending_slash_command_history();
                        self.add_error_message(
                            "Do not type vault secrets inline. Run `/vault credential add` with no arguments and enter the secret in the secure modal.".to_string(),
                        );
                        return;
                    }
                    self.open_vault_credential_add();
                    return;
                }
                if credential_action
                    && parts.get(1).is_some_and(|part| {
                        part.eq_ignore_ascii_case("reveal") || part.eq_ignore_ascii_case("export")
                    })
                {
                    if let [_, _, label] = parts.as_slice() {
                        self.reveal_vault_secret((*label).to_string());
                    } else {
                        self.add_error_message(
                            "Usage: /vault credential reveal <label>".to_string(),
                        );
                    }
                    return;
                }
                let direct_delete = parts.first().is_some_and(|part| {
                    part.eq_ignore_ascii_case("delete") || part.eq_ignore_ascii_case("rm")
                });
                let credential_delete = credential_action
                    && parts
                        .get(1)
                        .is_some_and(|part| part.eq_ignore_ascii_case("delete"));
                if direct_delete || credential_delete {
                    let label = if direct_delete {
                        (parts.len() == 2).then(|| parts[1])
                    } else {
                        (parts.len() == 3).then(|| parts[2])
                    };
                    if let Some(label) = label {
                        self.confirm_vault_credential_delete(label.to_string());
                    } else {
                        self.add_error_message(
                            "Usage: /vault credential delete <label>".to_string(),
                        );
                    }
                    return;
                }
                let codex_home = self.config.codex_home.clone();
                let args = trimmed.to_string();
                let tx = self.app_event_tx.clone();
                tokio::spawn(async move {
                    let task = tokio::task::spawn_blocking(move || {
                        crate::vault_command::handle_vault_command(&codex_home, &args)
                    });
                    let lines = match task.await {
                        Ok(lines) => lines,
                        Err(err) => vec![Line::from(format!("Vault task failed: {err}"))],
                    };
                    tx.send(AppEvent::InsertHistoryCell(Box::new(
                        PlainHistoryCell::new(lines),
                    )));
                });
            }
            SlashCommand::Wallet => match trimmed.to_ascii_lowercase().as_str() {
                "status" => self.open_wallet_menu(),
                "create" => self.open_wallet_create(),
                "restore" => self.open_wallet_restore(),
                "lock" => self.lock_wallet(),
                "unlock" => self.open_wallet_unlock(
                    codex_wallet_daemon::UnlockPolicy::Timed {
                        duration_seconds: 15 * 60,
                    },
                    crate::app_event::WalletUnlockContinuation::WalletMenu,
                ),
                _ => self.add_error_message(
                    "Usage: /wallet [status|create|restore|unlock|lock]".to_string(),
                ),
            },
            SlashCommand::Telegram => match trimmed.to_ascii_lowercase().as_str() {
                "" | "status" => self.open_telegram_menu(),
                "connect" => self.open_telegram_token_entry(),
                "start" => self.app_event_tx.send(AppEvent::StartTelegramConnector),
                "stop" => self.app_event_tx.send(AppEvent::StopTelegramConnector),
                "disconnect" => self.app_event_tx.send(AppEvent::ConfirmTelegramDisconnect),
                _ => self.add_error_message(
                    "Usage: /telegram [status|connect|start|stop|disconnect]".to_string(),
                ),
            },
            SlashCommand::Gpu => {
                let mut parts = trimmed.split_whitespace();
                match (parts.next(), parts.next(), parts.next()) {
                    (None | Some("status"), None, None) => {
                        self.app_event_tx.send(AppEvent::OpenGpuMenu);
                    }
                    (Some("stop"), Some(rental_id), None) => {
                        self.app_event_tx.send(AppEvent::DisableGpuServing {
                            rental_id: rental_id.to_string(),
                        });
                    }
                    (Some("terminate"), Some(rental_id), None) => {
                        self.app_event_tx.send(AppEvent::TerminateGpuRental {
                            rental_id: rental_id.to_string(),
                        });
                    }
                    _ => self.add_error_message(
                        "Usage: /gpu [status|stop <rental-id>|terminate <rental-id>]".to_string(),
                    ),
                }
            }
            SlashCommand::Spawn => {
                let mut parts = trimmed.splitn(2, ' ');
                let action = parts.next().unwrap_or_default().to_ascii_lowercase();
                let _rest = parts.next().unwrap_or_default().trim();
                match action.as_str() {
                    "" => self.app_event_tx.send(AppEvent::OpenSpawnRolePicker),
                    "status" => self.app_event_tx.send(AppEvent::OpenSpawnStatus),
                    "nazgul" => self.app_event_tx.send(AppEvent::OpenSpawnNazgulPicker),
                    "troll" => self.app_event_tx.send(AppEvent::OpenSpawnParentPicker {
                        role: crate::spawn_orchestration::SpawnRole::Troll,
                    }),
                    "orc" => self.app_event_tx.send(AppEvent::OpenSpawnParentPicker {
                        role: crate::spawn_orchestration::SpawnRole::Orc,
                    }),
                    _ => self
                        .add_error_message("Usage: /spawn [status|nazgul|troll|orc]".to_string()),
                }
            }
            SlashCommand::Orchestrate => {
                self.app_event_tx.send(AppEvent::HandleOrchestrateCommand {
                    args: trimmed.to_string(),
                });
            }
            SlashCommand::Tasknode => {
                let mut parts = trimmed.splitn(2, ' ');
                let action = parts.next().unwrap_or_default().to_ascii_lowercase();
                let rest = parts.next().unwrap_or_default().trim();
                match action.as_str() {
                    "" => self.app_event_tx.send(AppEvent::OpenTaskNodeMenu),
                    "link" => self.app_event_tx.send(AppEvent::OpenTaskNodeLink),
                    "status" => self.app_event_tx.send(AppEvent::OpenTaskNodeStatus),
                    "tasks" | "outstanding" => {
                        self.app_event_tx.send(AppEvent::OpenTaskNodeTaskList {
                            tab: if rest.is_empty() {
                                "outstanding".to_string()
                            } else {
                                rest.to_string()
                            },
                        })
                    }
                    "task" => {
                        if rest.is_empty() {
                            self.add_error_message("Usage: /tasknode task <task-id>".to_string());
                        } else {
                            self.app_event_tx.send(AppEvent::OpenTaskNodeTaskActions {
                                task_id: rest.to_string(),
                            });
                        }
                    }
                    "verification" => self.app_event_tx.send(AppEvent::OpenTaskNodeTaskList {
                        tab: "verification".to_string(),
                    }),
                    "refused" => self.app_event_tx.send(AppEvent::OpenTaskNodeTaskList {
                        tab: "refused".to_string(),
                    }),
                    "rewarded" => self.app_event_tx.send(AppEvent::OpenTaskNodeTaskList {
                        tab: "rewarded".to_string(),
                    }),
                    "request" => {
                        if rest.is_empty() {
                            self.app_event_tx.send(AppEvent::OpenTaskNodeTaskRequestPrompt);
                        } else {
                            self.app_event_tx.send(AppEvent::SubmitTaskNodeTaskRequest {
                                detail: rest.to_string(),
                            });
                        }
                    }
                    "context" => self.app_event_tx.send(AppEvent::OpenTaskNodeContext),
                    "team" | "team-context" => self.app_event_tx.send(AppEvent::OpenTaskNodeTeamContext),
                    "chat" => {
                        if rest.is_empty() {
                            self.app_event_tx.send(AppEvent::OpenTaskNodeChat);
                        } else {
                            self.app_event_tx.send(AppEvent::SubmitTaskNodeChat {
                                conversation_id: tasknode_new_chat_id(),
                                title: "New chat".to_string(),
                                message: rest.to_string(),
                            });
                        }
                    }
                    "requests" => self.app_event_tx.send(AppEvent::OpenTaskNodeRequestList),
                    "balance" => self.app_event_tx.send(AppEvent::OpenTaskNodeBalance),
                    "rewards" => self.app_event_tx.send(AppEvent::OpenTaskNodeRewards),
                    "logout" => self.app_event_tx.send(AppEvent::LogoutTaskNode),
                    _ => self.add_error_message(
                        "Usage: /tasknode [link|status|tasks|task|request|context|team|chat|requests|verification|balance|rewards|logout]"
                            .to_string(),
                    ),
                }
            }
            SlashCommand::Keymap => match trimmed.to_ascii_lowercase().as_str() {
                "" => self.open_keymap_picker(),
                "debug" => {
                    match crate::keymap::RuntimeKeymap::from_config(&self.config.tui_keymap) {
                        Ok(runtime_keymap) => self.open_keymap_debug(&runtime_keymap),
                        Err(err) => {
                            self.add_error_message(format!(
                                "Invalid `tui.keymap` configuration: {err}"
                            ));
                        }
                    }
                }
                _ => self.add_error_message("Usage: /keymap [debug]".to_string()),
            },
            SlashCommand::Raw => match trimmed.to_ascii_lowercase().as_str() {
                "on" => {
                    self.set_raw_output_mode_and_notify(/*enabled*/ true);
                    self.emit_raw_output_mode_changed(/*enabled*/ true);
                }
                "off" => {
                    self.set_raw_output_mode_and_notify(/*enabled*/ false);
                    self.emit_raw_output_mode_changed(/*enabled*/ false);
                }
                _ => self.add_error_message(RAW_USAGE.to_string()),
            },
            SlashCommand::Docs => {
                self.open_mkdocs_viewer(Some(trimmed.to_string()));
            }
            SlashCommand::Rename if !trimmed.is_empty() => {
                if !self.ensure_thread_rename_allowed() {
                    return;
                }
                self.session_telemetry
                    .counter("codex.thread.rename", /*inc*/ 1, &[]);
                let Some(name) = normalize_thread_name(&args) else {
                    self.add_error_message("Thread name cannot be empty.".to_string());
                    return;
                };
                self.app_event_tx.rename_current_pane(name);
            }
            SlashCommand::New if !trimmed.is_empty() => {
                self.app_event_tx.send(AppEvent::NewSession {
                    name: Some(trimmed.to_string()),
                });
            }
            SlashCommand::Clear if !trimmed.is_empty() => {
                self.app_event_tx.send(AppEvent::ClearUi {
                    name: Some(trimmed.to_string()),
                });
            }
            SlashCommand::Fork if !trimmed.is_empty() => {
                self.app_event_tx.send(AppEvent::ForkCurrentSession {
                    name: Some(trimmed.to_string()),
                });
            }
            SlashCommand::Plan if !trimmed.is_empty() => {
                if !self.apply_plan_slash_command() {
                    return;
                }
                let user_message = self.prepared_inline_user_message(
                    args,
                    text_elements,
                    local_images,
                    remote_image_urls,
                    mention_bindings,
                    source,
                );
                if self.is_session_configured() {
                    self.reasoning_buffer.clear();
                    self.reasoning_header = None;
                    self.reasoning_summary_parts.clear();
                    self.set_status_header(String::from("Working"));
                    self.submit_user_message(user_message);
                } else {
                    self.queue_user_message(user_message);
                }
            }
            SlashCommand::Goal if !trimmed.is_empty() => {
                if !self.config.features.enabled(Feature::Goals) {
                    if source == SlashCommandDispatchSource::Live {
                        self.clear_live_goal_submission();
                    }
                    return;
                }
                enum GoalControlCommand {
                    Clear,
                    SetStatus(AppThreadGoalStatus),
                }
                let control_command = match trimmed.to_ascii_lowercase().as_str() {
                    "clear" => Some(GoalControlCommand::Clear),
                    "edit" => {
                        self.app_event_tx.send(AppEvent::OpenThreadGoalEditor {
                            thread_id: self.thread_id,
                        });
                        if source == SlashCommandDispatchSource::Live {
                            self.clear_live_goal_submission();
                        }
                        return;
                    }
                    "pause" => Some(GoalControlCommand::SetStatus(AppThreadGoalStatus::Paused)),
                    "resume" => Some(GoalControlCommand::SetStatus(AppThreadGoalStatus::Active)),
                    _ => None,
                };
                if let Some(command) = control_command {
                    let Some(thread_id) = self.thread_id else {
                        self.add_info_message(
                            GOAL_USAGE.to_string(),
                            Some(
                                "The session must start before you can change a goal.".to_string(),
                            ),
                        );
                        if source == SlashCommandDispatchSource::Live {
                            self.clear_live_goal_submission();
                        }
                        return;
                    };
                    match command {
                        GoalControlCommand::Clear => {
                            self.app_event_tx
                                .send(AppEvent::ClearThreadGoal { thread_id });
                        }
                        GoalControlCommand::SetStatus(status) => {
                            self.app_event_tx
                                .send(AppEvent::SetThreadGoalStatus { thread_id, status });
                        }
                    }
                    self.append_message_history_entry(format!("/goal {trimmed}"));
                    if source == SlashCommandDispatchSource::Live {
                        self.clear_live_goal_submission();
                    }
                    return;
                }
                let draft = GoalDraft {
                    objective: args,
                    text_elements,
                    pending_pastes,
                    local_images,
                    remote_image_urls,
                };
                let Some(thread_id) = self.thread_id else {
                    if source == SlashCommandDispatchSource::Live {
                        const GOAL_PREFIX: &str = "/goal ";
                        let text_elements = draft
                            .text_elements
                            .into_iter()
                            .map(|element| {
                                element.map_range(|range| ByteRange {
                                    start: range.start + GOAL_PREFIX.len(),
                                    end: range.end + GOAL_PREFIX.len(),
                                })
                            })
                            .collect();
                        self.queue_user_message_with_options(
                            UserMessage {
                                text: format!("{GOAL_PREFIX}{}", draft.objective),
                                local_images: draft.local_images,
                                remote_image_urls: draft.remote_image_urls,
                                text_elements,
                                mention_bindings: Vec::new(),
                            },
                            QueuedInputAction::ParseSlash,
                            draft.pending_pastes,
                        );
                        self.clear_live_goal_submission();
                    } else {
                        self.add_info_message(
                            GOAL_USAGE.to_string(),
                            Some("The session must start before you can set a goal.".to_string()),
                        );
                    }
                    return;
                };
                let history_objective = draft.objective.clone();
                self.app_event_tx.send(AppEvent::SetThreadGoalDraft {
                    thread_id,
                    draft,
                    mode: ThreadGoalSetMode::ConfirmIfExists,
                });
                self.append_message_history_entry(format!("/goal {history_objective}"));
                if source == SlashCommandDispatchSource::Live {
                    self.clear_live_goal_submission();
                }
            }
            SlashCommand::Side | SlashCommand::Btw if !trimmed.is_empty() => {
                let Some(parent_thread_id) = self.thread_id else {
                    let command = cmd.command();
                    self.add_error_message(format!(
                        "'/{command}' is unavailable before the session starts."
                    ));
                    return;
                };
                let user_message = self.prepared_inline_user_message(
                    args,
                    text_elements,
                    local_images,
                    remote_image_urls,
                    mention_bindings,
                    source,
                );
                self.request_side_conversation(parent_thread_id, Some(user_message));
            }
            SlashCommand::Review if !trimmed.is_empty() => {
                self.submit_op(AppCommand::review(ReviewTarget::Custom {
                    instructions: args,
                }));
            }
            SlashCommand::Resume if !trimmed.is_empty() => {
                self.app_event_tx
                    .send(AppEvent::ResumeSessionByIdOrName(args));
            }
            SlashCommand::SandboxReadRoot if !trimmed.is_empty() => {
                self.app_event_tx
                    .send(AppEvent::BeginWindowsSandboxGrantReadRoot { path: args });
            }
            SlashCommand::Pets
                if matches!(
                    args.trim().to_ascii_lowercase().as_str(),
                    "disable" | "disabled" | "hide" | "hidden" | "off" | "none"
                ) =>
            {
                self.app_event_tx.send(AppEvent::PetDisabled);
            }
            SlashCommand::Pets if !trimmed.is_empty() => {
                self.select_pet_by_id(args);
            }
            _ => self.dispatch_command(cmd),
        }
        if source == SlashCommandDispatchSource::Live && cmd != SlashCommand::Goal {
            self.bottom_pane.drain_pending_submission_state();
        }
    }

    pub(super) fn submit_queued_slash_prompt(
        &mut self,
        queued_message: QueuedUserMessage,
    ) -> QueueDrain {
        let QueuedUserMessage {
            user_message,
            pending_pastes,
            ..
        } = queued_message;
        let UserMessage {
            text,
            local_images,
            remote_image_urls,
            text_elements,
            mention_bindings,
        } = user_message;
        let Some((name, rest, rest_offset)) = parse_slash_name(&text) else {
            self.submit_user_message(UserMessage {
                text,
                local_images,
                remote_image_urls,
                text_elements,
                mention_bindings,
            });
            return QueueDrain::Stop;
        };

        if name.contains('/') {
            self.submit_user_message(UserMessage {
                text,
                local_images,
                remote_image_urls,
                text_elements,
                mention_bindings,
            });
            return QueueDrain::Stop;
        }

        let service_tier_commands = self.current_model_service_tier_commands();
        let Some(command) =
            find_slash_command(name, self.builtin_command_flags(), &service_tier_commands)
        else {
            self.add_info_message(
                format!(
                    r#"Unrecognized command '/{name}'. Type "/" for a list of supported commands."#
                ),
                /*hint*/ None,
            );
            return QueueDrain::Continue;
        };

        if rest.is_empty() {
            return match command {
                SlashCommandItem::Builtin(cmd) => {
                    self.dispatch_command(cmd);
                    self.queued_command_drain_result(cmd)
                }
                SlashCommandItem::ServiceTier(command) => {
                    self.handle_service_tier_command_dispatch(command);
                    QueueDrain::Continue
                }
            };
        }

        if !command.supports_inline_args() {
            // A recognized command must never degrade into user text: with an
            // external worker pane active, that text would be forwarded to the
            // worker as a task, silently swallowing control-plane commands.
            self.add_info_message(
                format!("'/{name}' does not take inline arguments; running the bare command."),
                /*hint*/ None,
            );
            return match command {
                SlashCommandItem::Builtin(cmd) => {
                    self.dispatch_command(cmd);
                    self.queued_command_drain_result(cmd)
                }
                SlashCommandItem::ServiceTier(command) => {
                    self.handle_service_tier_command_dispatch(command);
                    QueueDrain::Continue
                }
            };
        }
        let cmd = match command {
            SlashCommandItem::Builtin(cmd) => cmd,
            SlashCommandItem::ServiceTier(command) => {
                self.handle_service_tier_command_dispatch(command);
                return QueueDrain::Continue;
            }
        };

        let trimmed_start = rest.trim_start();
        let leading_trimmed = rest.len().saturating_sub(trimmed_start.len());
        let trimmed_rest = trimmed_start.trim_end();
        let args_elements = Self::slash_command_args_elements(
            trimmed_rest,
            rest_offset + leading_trimmed,
            &text_elements,
        );
        self.dispatch_prepared_command_with_args(
            cmd,
            PreparedSlashCommandArgs {
                args: trimmed_rest.to_string(),
                text_elements: args_elements,
                pending_pastes,
                local_images,
                remote_image_urls,
                mention_bindings,
                source: SlashCommandDispatchSource::Queued,
            },
        );
        self.queued_command_drain_result(cmd)
    }

    /// If `text` is a recognized slash command, dispatch it through the normal
    /// slash-command path and return true. External-pane input forwarding uses
    /// this to keep control-plane commands global: a slash command entered
    /// while a Claude worker pane is active must act on Corbanu Terminal itself,
    /// never become a task forwarded to the worker.
    pub(crate) fn try_dispatch_slash_input(&mut self, text: &str) -> bool {
        let trimmed = text.trim();
        let Some((name, _rest, _rest_offset)) = parse_slash_name(trimmed) else {
            return false;
        };
        if name.contains('/') {
            return false;
        }
        let service_tier_commands = self.current_model_service_tier_commands();
        if find_slash_command(name, self.builtin_command_flags(), &service_tier_commands).is_none()
        {
            return false;
        }
        let user_message = UserMessage {
            text: trimmed.to_string(),
            local_images: Vec::new(),
            remote_image_urls: Vec::new(),
            text_elements: Vec::new(),
            mention_bindings: Vec::new(),
        };
        self.submit_queued_slash_prompt(QueuedUserMessage::from(user_message));
        true
    }

    fn builtin_command_flags(&self) -> BuiltinCommandFlags {
        #[cfg(target_os = "windows")]
        let allow_elevate_sandbox = {
            let windows_sandbox_level = crate::windows_sandbox::level_from_config(&self.config);
            matches!(windows_sandbox_level, WindowsSandboxLevel::RestrictedToken)
        };
        #[cfg(not(target_os = "windows"))]
        let allow_elevate_sandbox = false;

        BuiltinCommandFlags {
            collaboration_modes_enabled: self.collaboration_modes_enabled(),
            connectors_enabled: self.connectors_enabled(),
            plugins_command_enabled: self.config.features.enabled(Feature::Plugins),
            token_activity_command_enabled: self.has_codex_backend_auth
                || self.pfterminal_plan_key_is_linked(),
            goal_command_enabled: self.config.features.enabled(Feature::Goals),
            service_tier_commands_enabled: self.fast_mode_enabled(),
            personality_command_enabled: self.config.features.enabled(Feature::Personality),
            allow_elevate_sandbox,
            side_conversation_active: self.active_side_conversation,
        }
    }

    fn ensure_usage_command_available(&mut self) -> bool {
        if self.has_codex_backend_auth || self.pfterminal_plan_key_is_linked() {
            return true;
        }
        self.add_error_message(USAGE_CHATGPT_LOGIN_REQUIRED.to_string());
        false
    }

    fn queued_command_drain_result(&self, cmd: SlashCommand) -> QueueDrain {
        if self.is_user_turn_pending_or_running() || !self.bottom_pane.no_modal_or_popup_active() {
            return QueueDrain::Stop;
        }
        match cmd {
            SlashCommand::Ide
            | SlashCommand::Status
            | SlashCommand::Usage
            | SlashCommand::Cost
            | SlashCommand::DebugConfig
            | SlashCommand::Ps
            | SlashCommand::Stop
            | SlashCommand::MemoryDrop
            | SlashCommand::MemoryUpdate
            | SlashCommand::Mcp
            | SlashCommand::Apps
            | SlashCommand::Plugins
            | SlashCommand::Providers
            | SlashCommand::Panes
            | SlashCommand::Spawn
            | SlashCommand::Orchestrate
            | SlashCommand::Tasknode
            | SlashCommand::Rollout
            | SlashCommand::Vault
            | SlashCommand::Wallet
            | SlashCommand::Telegram
            | SlashCommand::Gpu
            | SlashCommand::Copy
            | SlashCommand::Raw
            | SlashCommand::Vim
            | SlashCommand::Diff
            | SlashCommand::Docs
            | SlashCommand::App
            | SlashCommand::Rename
            | SlashCommand::TestApproval => QueueDrain::Continue,
            SlashCommand::Feedback
            | SlashCommand::New
            | SlashCommand::Archive
            | SlashCommand::Delete
            | SlashCommand::Clear
            | SlashCommand::Resume
            | SlashCommand::Fork
            | SlashCommand::Init
            | SlashCommand::Compact
            | SlashCommand::Review
            | SlashCommand::Model
            | SlashCommand::Personality
            | SlashCommand::Plan
            | SlashCommand::Goal
            | SlashCommand::Side
            | SlashCommand::Btw
            | SlashCommand::Keymap
            | SlashCommand::Agent
            | SlashCommand::MultiAgents
            | SlashCommand::Permissions
            | SlashCommand::Security
            | SlashCommand::ElevateSandbox
            | SlashCommand::SandboxReadRoot
            | SlashCommand::Experimental
            | SlashCommand::AutoReview
            | SlashCommand::Memories
            | SlashCommand::Quit
            | SlashCommand::Exit
            | SlashCommand::Logout
            | SlashCommand::Mention
            | SlashCommand::Skills
            | SlashCommand::Import
            | SlashCommand::Hooks
            | SlashCommand::Title
            | SlashCommand::Statusline
            | SlashCommand::Theme
            | SlashCommand::Pets => QueueDrain::Stop,
        }
    }

    fn slash_command_args_elements(
        rest: &str,
        rest_offset: usize,
        text_elements: &[TextElement],
    ) -> Vec<TextElement> {
        if rest.is_empty() || text_elements.is_empty() {
            return Vec::new();
        }
        text_elements
            .iter()
            .filter_map(|elem| {
                if elem.byte_range.end <= rest_offset {
                    return None;
                }
                let start = elem.byte_range.start.saturating_sub(rest_offset);
                let mut end = elem.byte_range.end.saturating_sub(rest_offset);
                if start >= rest.len() {
                    return None;
                }
                end = end.min(rest.len());
                (start < end).then_some(elem.map_range(|_| ByteRange { start, end }))
            })
            .collect()
    }

    fn ensure_slash_command_allowed_in_side_conversation(&mut self, cmd: SlashCommand) -> bool {
        if !self.active_side_conversation || cmd.available_in_side_conversation() {
            return true;
        }
        self.add_error_message(format!(
            "'/{}' is unavailable in side conversations. {SIDE_SLASH_COMMAND_UNAVAILABLE_HINT}",
            cmd.command()
        ));
        self.bottom_pane.drain_pending_submission_state();
        false
    }

    fn ensure_side_command_allowed_outside_review(&mut self, cmd: SlashCommand) -> bool {
        if !matches!(cmd, SlashCommand::Side | SlashCommand::Btw) || !self.review.is_review_mode {
            return true;
        }

        let command = cmd.command();
        self.add_error_message(format!(
            "'/{command}' is unavailable while code review is running."
        ));
        self.bottom_pane.drain_pending_submission_state();
        false
    }
}
