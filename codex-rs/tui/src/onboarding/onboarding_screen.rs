//! Onboarding screen orchestration and top-level keyboard routing.
//!
//! The onboarding flow is a small state machine over visible steps
//! (welcome/auth/trust). This module decides which step receives key/paste
//! events and enforces flow-level safety rules that cut across individual step
//! widgets.
//!
//! In particular, onboarding quit handling has a text-entry guard: while any
//! text field has focus (an API key, a Claude token or code, or a named
//! account's name or value) or its value is saving, the printable `q` quit key
//! is text, even in an empty field. Control chords (Ctrl+C, Ctrl+D) remain explicit exits. Quitting
//! on a printable key mid-entry would drop the rest of a typed or non-bracketed
//! pasted secret onto the user's shell (#461).

use codex_app_server_client::AppServerEvent;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::ServerNotification;
use codex_exec_server::LOCAL_FS;
use codex_git_utils::resolve_root_git_project_for_trust;
#[cfg(target_os = "windows")]
use codex_protocol::config_types::WindowsSandboxLevel;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::style::Color;
use ratatui::widgets::Clear;
use ratatui::widgets::WidgetRef;

use codex_model_provider_info::AMBIENT_API_KEY_ENV_VAR;
use codex_model_provider_info::ANTHROPIC_API_KEY_ENV_VAR;
use codex_model_provider_info::BASETEN_API_KEY_ENV_VAR;
use codex_model_provider_info::DEEPSEEK_API_KEY_ENV_VAR;
use codex_model_provider_info::KIMI_CODE_API_KEY_ENV_VAR;
use codex_model_provider_info::META_API_KEY_ENV_VAR;
use codex_model_provider_info::OPENROUTER_API_KEY_ENV_VAR;
use codex_model_provider_info::VERCEL_API_KEY_ENV_VAR;
use codex_model_provider_info::ZAI_API_KEY_ENV_VAR;
use codex_protocol::config_types::ForcedLoginMethod;

use crate::LoginStatus;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::app_server_session::AppServerSession;
use crate::config_update::format_config_error;
use crate::config_update::write_trusted_project;
use crate::key_hint::KeyBindingListExt;
use crate::legacy_core::config::Config;
use crate::onboarding::auth::ApiKeyInputState;
use crate::onboarding::auth::ApiKeyProviderOption;
use crate::onboarding::auth::AuthModeWidget;
use crate::onboarding::auth::SignInOption;
use crate::onboarding::auth::SignInState;
use crate::onboarding::auth::catalog_sign_in_options_from_statuses;
use crate::onboarding::keys;
use crate::onboarding::provider_setup::DeferredProviderSetup;
use crate::onboarding::provider_setup::ProviderSetupSession;
use crate::onboarding::trust_directory::TrustDirectorySelection;
use crate::onboarding::trust_directory::TrustDirectoryWidget;
use crate::onboarding::welcome::WelcomeWidget;
use crate::provider_auth_effect_executor::ProviderAuthEffectExecutor;
use crate::provider_status_host::ProviderAccountMetadata;
use crate::provider_status_host::ProviderStatusHost;
use crate::tui::FrameRequester;
use crate::tui::Tui;
use crate::tui::TuiEvent;
use color_eyre::eyre::Result;
use std::sync::Arc;
use std::sync::RwLock;

#[allow(clippy::large_enum_variant)]
enum Step {
    Welcome(WelcomeWidget),
    Auth(AuthModeWidget),
    TrustDirectory(TrustDirectoryWidget),
}

pub(crate) trait KeyboardHandler {
    fn handle_key_event(&mut self, key_event: KeyEvent);
    fn handle_paste(&mut self, _pasted: String) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepState {
    Hidden,
    InProgress,
    Complete,
}

pub(crate) trait StepStateProvider {
    fn get_step_state(&self) -> StepState;
}

pub(crate) struct OnboardingScreen {
    request_frame: FrameRequester,
    steps: Vec<Step>,
    is_done: bool,
    should_exit: bool,
    provider_auth_controller: codex_provider_auth::ProviderAuthController,
    provider_auth_executor: Option<ProviderAuthEffectExecutor>,
    provider_auth_action_rx:
        tokio::sync::mpsc::UnboundedReceiver<codex_provider_auth::ProviderAuthAction>,
}

pub(crate) struct OnboardingScreenArgs {
    pub show_trust_screen: bool,
    pub show_login_screen: bool,
    pub login_status: LoginStatus,
    pub app_server_request_handle: Option<AppServerRequestHandle>,
    pub config: Config,
}

pub(crate) struct OnboardingResult {
    pub directory_trust_persisted: bool,
    pub should_exit: bool,
    pub configured_provider: Option<String>,
    pub deferred_provider_setup: Option<DeferredProviderSetup>,
}

fn provider_api_key_options(host: &ProviderStatusHost) -> Vec<ApiKeyProviderOption> {
    host.catalog()
        .entries()
        .iter()
        .flat_map(|entry| {
            entry.setup_capabilities.iter().filter_map(|capability| {
                let codex_provider_auth::ProviderSetupCapability::ApiKey { .. } = capability else {
                    return None;
                };
                let target = codex_provider_auth::ApiKeyAuthTarget::from_catalog_capability(
                    entry, capability,
                )
                .ok()?;
                Some(ApiKeyProviderOption {
                    target,
                    capability: capability.clone(),
                    name: entry.display_name.clone(),
                })
            })
        })
        .collect()
}

pub(crate) fn provider_api_key_display_name(provider: &ApiKeyProviderOption) -> String {
    let codex_provider_auth::ApiKeyStorage::EnvironmentVariable { env_key } =
        &provider.target.storage
    else {
        return "Provider: OpenAI API Key".to_string();
    };
    match env_key.as_str() {
        ANTHROPIC_API_KEY_ENV_VAR => "Provider: Anthropic API Key".to_string(),
        AMBIENT_API_KEY_ENV_VAR => "Provider: Ambient API Key".to_string(),
        KIMI_CODE_API_KEY_ENV_VAR => "Provider: Kimi Code API Key".to_string(),
        ZAI_API_KEY_ENV_VAR => "Provider: Z.AI API Key".to_string(),
        DEEPSEEK_API_KEY_ENV_VAR => "Provider: DeepSeek API Key".to_string(),
        OPENROUTER_API_KEY_ENV_VAR => "Provider: OpenRouter API Key".to_string(),
        META_API_KEY_ENV_VAR => "Provider: Meta API Key".to_string(),
        BASETEN_API_KEY_ENV_VAR => "Provider: Baseten API Key".to_string(),
        VERCEL_API_KEY_ENV_VAR => "Provider: Vercel API Key".to_string(),
        _ => format!("Provider: {} {}", provider.name, env_key),
    }
}

fn initial_sign_in_option(
    catalog_options: &[SignInOption],
    forced_login_method: Option<ForcedLoginMethod>,
) -> SignInOption {
    match forced_login_method {
        Some(ForcedLoginMethod::Api) => SignInOption::ApiKey,
        Some(ForcedLoginMethod::Chatgpt) => SignInOption::ChatGpt,
        None => catalog_options
            .first()
            .copied()
            .unwrap_or(SignInOption::ChatGpt),
    }
}

impl OnboardingScreen {
    pub(crate) async fn new(
        tui: &mut Tui,
        args: OnboardingScreenArgs,
        claude_event_tx: AppEventSender,
    ) -> Self {
        let OnboardingScreenArgs {
            show_trust_screen,
            show_login_screen,
            login_status,
            app_server_request_handle,
            config,
        } = args;
        let cwd = config.cwd.to_path_buf();
        let provider_status_host = ProviderStatusHost::from_config(
            &config,
            ProviderAccountMetadata {
                openai: openai_metadata(login_status),
                claude: codex_provider_auth::ClaudeCredentialMetadata::Checking,
                ..ProviderAccountMetadata::default()
            },
        );
        let provider_statuses = provider_status_host.resolve();
        let provider_setup_session = Arc::new(RwLock::new(ProviderSetupSession::from_statuses(
            provider_statuses.entries(),
        )));
        let (provider_auth_action_tx, provider_auth_action_rx) =
            tokio::sync::mpsc::unbounded_channel();
        let discovery_config = config.clone();
        let discovery_tx = claude_event_tx.clone();
        tokio::spawn(async move {
            let metadata = ProviderAccountMetadata::discover(&discovery_config).await;
            discovery_tx.send(AppEvent::ProviderAccountMetadataResolved { metadata });
        });
        let provider_auth_executor = app_server_request_handle.clone().map(|request_handle| {
            ProviderAuthEffectExecutor::new(
                request_handle,
                provider_auth_action_tx.clone(),
                provider_status_host.clone(),
            )
        });
        let codex_home = config.codex_home.to_path_buf();
        let forced_login_method = config.forced_login_method;
        let mut api_key_provider_id = config.model_provider_id.clone();
        let mut api_key_provider_name = config.model_provider.name.clone();
        let mut api_key_env_var = config.model_provider.env_key.clone();
        let api_key_provider_options = provider_api_key_options(&provider_status_host);
        // The first option stands in only when the screen will name that option too (the
        // override below). Otherwise the screen names the configured provider, and saving the
        // key to another provider would report success for the wrong one.
        let first_option_stands_in =
            api_key_env_var.is_none() || config.model_provider.requires_openai_auth;
        let selected_provider_index = api_key_provider_options
            .iter()
            .position(|provider| provider.target.provider_id.as_str() == api_key_provider_id)
            .or_else(|| first_option_stands_in.then_some(0));
        let selected_api_key_option = selected_provider_index
            .and_then(|index| api_key_provider_options.get(index))
            .cloned();
        if let Some(provider) = selected_api_key_option.as_ref()
            && first_option_stands_in
        {
            api_key_provider_id = provider.target.provider_id.as_str().to_string();
            api_key_provider_name = provider.name.clone();
            api_key_env_var = provider.environment_variable().map(str::to_string);
        }
        let mut steps: Vec<Step> = Vec::new();
        steps.push(Step::Welcome(WelcomeWidget::new(
            !matches!(login_status, LoginStatus::NotAuthenticated),
            tui.frame_requester(),
            config.animations,
        )));
        if show_login_screen {
            let catalog_options = catalog_sign_in_options_from_statuses(
                &provider_status_host,
                &provider_statuses,
                &api_key_provider_options,
            );
            let has_multiple_provider_options = catalog_options.len() > 1;
            // #1563: a chosen but unset Corbanu Plan starts on its own setup row.
            let highlighted_mode = if forced_login_method.is_none()
                && config.model_provider_id
                    == codex_model_provider_info::PFTERMINAL_PLAN_PROVIDER_ID
                && catalog_options.contains(&SignInOption::CorbanuPlan)
            {
                SignInOption::CorbanuPlan
            } else {
                initial_sign_in_option(&catalog_options, forced_login_method)
            };
            let initial_sign_in_state =
                if matches!(forced_login_method, Some(ForcedLoginMethod::Api))
                    || (!has_multiple_provider_options && api_key_env_var.is_some())
                {
                    SignInState::ApiKeyEntry(ApiKeyInputState::from_env(api_key_env_var.as_deref()))
                } else {
                    SignInState::PickMode
                };
            let named_account_methods = if config
                .features
                .enabled(codex_features::Feature::NamedAccounts)
            {
                config
                    .model_providers
                    .iter()
                    .map(|(id, provider)| {
                        (
                            id.clone(),
                            crate::provider_named_accounts::add_methods(id, provider)
                                .into_iter()
                                // Onboarding takes pasted secrets; paths, AWS profiles and OpenAI sign-ins are in /providers.
                                .filter(|method| {
                                    !matches!(
                                        method,
                                        crate::provider_named_accounts::AddAccountMethod::ClaudeConfigDir
                                            | crate::provider_named_accounts::AddAccountMethod::AwsProfile
                                            | crate::provider_named_accounts::AddAccountMethod::ChatgptDeviceCode
                                            | crate::provider_named_accounts::AddAccountMethod::OpenaiApiKey
                                    )
                                })
                                .collect::<Vec<_>>(),
                        )
                    })
                    .filter(|(_, methods)| !methods.is_empty())
                    .collect()
            } else {
                std::collections::BTreeMap::new()
            };
            if let Some(app_server_request_handle) = app_server_request_handle {
                let mut widget = AuthModeWidget {
                    request_frame: tui.frame_requester(),
                    highlighted_mode,
                    error: Arc::new(RwLock::new(None)),
                    sign_in_state: Arc::new(RwLock::new(initial_sign_in_state)),
                    login_status,
                    replace_provider_with_openai_on_login: config.model_provider_id
                        == codex_model_provider_info::OPENAI_PROVIDER_ID
                        || !config.model_provider.requires_openai_auth,
                    app_server_request_handle,
                    forced_login_method,
                    api_key_provider_id,
                    api_key_provider_name,
                    api_key_env_var,
                    api_key_provider_options,
                    selected_api_key_option,
                    animations_enabled: config.animations,
                    animations_suppressed: std::cell::Cell::new(false),
                    codex_home,
                    claude_event_tx,
                    provider_setup_session,
                    provider_status_host,
                    provider_statuses: Arc::new(RwLock::new(provider_statuses)),
                    provider_auth_action_tx,
                    named_account_methods,
                };
                widget.begin_initial_api_key_entry();
                steps.push(Step::Auth(widget));
            } else {
                tracing::warn!("skipping onboarding login step without app-server request handle");
            }
        }
        #[cfg(target_os = "windows")]
        let show_windows_create_sandbox_hint =
            crate::windows_sandbox::level_from_config(&config) == WindowsSandboxLevel::Disabled;
        #[cfg(not(target_os = "windows"))]
        let show_windows_create_sandbox_hint = false;
        let highlighted = TrustDirectorySelection::Trust;
        if show_trust_screen {
            let trust_target = resolve_root_git_project_for_trust(LOCAL_FS.as_ref(), &config.cwd)
                .await
                .map(Into::into)
                .unwrap_or_else(|| cwd.clone());
            steps.push(Step::TrustDirectory(TrustDirectoryWidget {
                cwd,
                trust_target,
                show_windows_create_sandbox_hint,
                should_quit: false,
                selection: None,
                highlighted,
                error: None,
            }))
        }
        Self {
            request_frame: tui.frame_requester(),
            steps,
            is_done: false,
            should_exit: false,
            provider_auth_controller: codex_provider_auth::ProviderAuthController::default(),
            provider_auth_executor,
            provider_auth_action_rx,
        }
    }

    fn current_steps_mut(&mut self) -> Vec<&mut Step> {
        let states = self
            .steps
            .iter()
            .map(StepStateProvider::get_step_state)
            .collect::<Vec<_>>();
        let welcome_index = self
            .steps
            .iter()
            .position(|step| matches!(step, Step::Welcome(_)));
        let visible_indices = visible_step_indices(&states, welcome_index);
        self.steps
            .iter_mut()
            .enumerate()
            .filter_map(|(index, step)| visible_indices.contains(&index).then_some(step))
            .collect()
    }

    fn current_steps(&self) -> Vec<&Step> {
        let states = self
            .steps
            .iter()
            .map(StepStateProvider::get_step_state)
            .collect::<Vec<_>>();
        let welcome_index = self
            .steps
            .iter()
            .position(|step| matches!(step, Step::Welcome(_)));
        let visible_indices = visible_step_indices(&states, welcome_index);
        self.steps
            .iter()
            .enumerate()
            .filter_map(|(index, step)| visible_indices.contains(&index).then_some(step))
            .collect()
    }

    fn should_suppress_animations(&self) -> bool {
        // Freeze the whole onboarding screen when auth is showing copyable login
        // material so terminal selection is not interrupted by redraws.
        self.current_steps().into_iter().any(|step| match step {
            Step::Auth(widget) => widget.should_suppress_animations(),
            Step::Welcome(_) | Step::TrustDirectory(_) => false,
        })
    }

    fn is_auth_in_progress(&self) -> bool {
        self.steps.iter().any(|step| {
            matches!(step, Step::Auth(_)) && matches!(step.get_step_state(), StepState::InProgress)
        })
    }

    pub(crate) fn is_done(&self) -> bool {
        self.is_done
            || !self
                .steps
                .iter()
                .any(|step| matches!(step.get_step_state(), StepState::InProgress))
    }

    pub fn should_exit(&self) -> bool {
        self.should_exit
    }

    fn configured_provider(&self) -> Option<String> {
        self.steps.iter().find_map(|step| {
            if let Step::Auth(widget) = step {
                return widget.configured_provider();
            }
            None
        })
    }

    fn deferred_provider_setup(&self) -> Option<DeferredProviderSetup> {
        self.steps.iter().find_map(|step| match step {
            Step::Auth(widget) => widget.deferred_provider_setup(),
            Step::Welcome(_) | Step::TrustDirectory(_) => None,
        })
    }

    fn cancel_auth_if_active(&self) {
        for step in &self.steps {
            if let Step::Auth(widget) = step {
                widget.cancel_active_attempt();
            }
        }
    }

    fn auth_widget_mut(&mut self) -> Option<&mut AuthModeWidget> {
        self.steps.iter_mut().find_map(|step| match step {
            Step::Auth(widget) => Some(widget),
            Step::Welcome(_) | Step::TrustDirectory(_) => None,
        })
    }

    fn handle_app_server_notification(&mut self, notification: ServerNotification) {
        match notification {
            ServerNotification::AccountLoginCompleted(notification) => {
                if let Some(widget) = self.auth_widget_mut() {
                    widget.on_account_login_completed(notification);
                }
            }
            ServerNotification::AccountUpdated(notification) => {
                if let Some(widget) = self.auth_widget_mut() {
                    widget.on_account_updated(notification);
                }
            }
            _ => {}
        }
    }

    fn handle_claude_event(&mut self, event: AppEvent) {
        if let Some(widget) = self.auth_widget_mut() {
            widget.handle_claude_event(event);
        }
    }

    fn handle_provider_auth_action(&mut self, action: codex_provider_auth::ProviderAuthAction) {
        let transition = self.provider_auth_controller.dispatch(action);
        if let Some(widget) = self.auth_widget_mut() {
            widget.on_provider_auth_snapshot(&transition.snapshot);
        }
        for effect in transition.effects {
            match effect {
                codex_provider_auth::ProviderAuthEffect::Complete(
                    codex_provider_auth::ProviderAuthCompletion::Configured { target, .. },
                ) => {
                    if let Some(widget) = self.auth_widget_mut() {
                        widget.on_api_key_configured(target.provider_id.as_str());
                    }
                }
                effect => {
                    if let Some(executor) = &self.provider_auth_executor {
                        let _ = executor.execute(effect);
                    }
                }
            }
        }
    }

    fn text_entry_active(&self) -> bool {
        self.steps.iter().any(|step| {
            if let Step::Auth(widget) = step {
                widget.is_text_entry_active()
            } else {
                false
            }
        })
    }
}

impl KeyboardHandler for OnboardingScreen {
    /// Route key events to onboarding steps while preserving text-entry safety.
    ///
    /// While a text field has focus, printable quit bindings are text. Control
    /// quit chords still work as emergency exits.
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if !matches!(key_event.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return;
        }
        let should_quit = key_event.kind == KeyEventKind::Press
            && keys::QUIT.is_pressed(key_event)
            && !is_text_key_in_text_field(key_event, self.text_entry_active());
        if should_quit {
            if self.is_auth_in_progress() {
                self.cancel_auth_if_active();
                // If the user cancels the auth menu, exit the app rather than
                // leave the user at a prompt in an unauthed state.
                self.should_exit = true;
            }
            self.is_done = true;
        } else {
            if let Some(Step::Welcome(widget)) = self
                .steps
                .iter_mut()
                .find(|step| matches!(step, Step::Welcome(_)))
            {
                widget.handle_key_event(key_event);
            }
            if let Some(active_step) = self.current_steps_mut().into_iter().last() {
                active_step.handle_key_event(key_event);
            }
            if self.steps.iter().any(|step| {
                if let Step::TrustDirectory(widget) = step {
                    widget.should_quit()
                } else {
                    false
                }
            }) {
                self.should_exit = true;
                self.is_done = true;
            }
        }
        self.request_frame.schedule_frame();
    }

    fn handle_paste(&mut self, pasted: String) {
        if pasted.is_empty() {
            return;
        }

        if let Some(active_step) = self.current_steps_mut().into_iter().last() {
            active_step.handle_paste(pasted);
        }
        self.request_frame.schedule_frame();
    }
}

/// Returns `true` when a key is text for the focused text field, so it must
/// never act as a shortcut: a printable character without control/alt.
fn is_text_key_in_text_field(key_event: KeyEvent, text_entry_active: bool) -> bool {
    text_entry_active
        && matches!(key_event.code, KeyCode::Char(_))
        && !key_event
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

impl WidgetRef for &OnboardingScreen {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let suppress_animations = self.should_suppress_animations();
        for step in self.current_steps() {
            match step {
                Step::Welcome(widget) => widget.set_animations_suppressed(suppress_animations),
                Step::Auth(widget) => widget.set_animations_suppressed(suppress_animations),
                Step::TrustDirectory(_) => {}
            }
        }

        Clear.render(area, buf);
        // Render steps top-to-bottom, measuring each step's height dynamically.
        let mut y = area.y;
        let bottom = area.y.saturating_add(area.height);
        let width = area.width;

        // Helper to scan a temporary buffer and return number of used rows.
        fn used_rows(tmp: &Buffer, width: u16, height: u16) -> u16 {
            if width == 0 || height == 0 {
                return 0;
            }
            let mut last_non_empty: Option<u16> = None;
            for yy in 0..height {
                let mut any = false;
                for xx in 0..width {
                    let cell = &tmp[(xx, yy)];
                    let has_symbol = !cell.symbol().trim().is_empty();
                    let has_style = cell.fg != Color::Reset
                        || cell.bg != Color::Reset
                        || !cell.modifier.is_empty();
                    if has_symbol || has_style {
                        any = true;
                        break;
                    }
                }
                if any {
                    last_non_empty = Some(yy);
                }
            }
            last_non_empty.map(|v| v + 2).unwrap_or(0)
        }

        let mut i = 0usize;
        let current_steps = self.current_steps();

        while i < current_steps.len() && y < bottom {
            let step = &current_steps[i];
            let max_h = bottom.saturating_sub(y);
            if max_h == 0 || width == 0 {
                break;
            }
            let scratch_area = Rect::new(0, 0, width, max_h);
            let mut scratch = Buffer::empty(scratch_area);
            if let Step::Welcome(widget) = step {
                widget.update_layout_area(scratch_area);
            }
            step.render_ref(scratch_area, &mut scratch);
            let h = used_rows(&scratch, width, max_h).min(max_h);
            if h > 0 {
                let target = Rect {
                    x: area.x,
                    y,
                    width,
                    height: h,
                };
                Clear.render(target, buf);
                step.render_ref(target, buf);
                y = y.saturating_add(h);
            }
            i += 1;
        }
    }
}

impl KeyboardHandler for Step {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match self {
            Step::Welcome(widget) => widget.handle_key_event(key_event),
            Step::Auth(widget) => widget.handle_key_event(key_event),
            Step::TrustDirectory(widget) => widget.handle_key_event(key_event),
        }
    }

    fn handle_paste(&mut self, pasted: String) {
        match self {
            Step::Welcome(_) => {}
            Step::Auth(widget) => widget.handle_paste(pasted),
            Step::TrustDirectory(widget) => widget.handle_paste(pasted),
        }
    }
}

impl StepStateProvider for Step {
    fn get_step_state(&self) -> StepState {
        match self {
            Step::Welcome(w) => w.get_step_state(),
            Step::Auth(w) => w.get_step_state(),
            Step::TrustDirectory(w) => w.get_step_state(),
        }
    }
}

fn visible_step_indices(states: &[StepState], welcome_index: Option<usize>) -> Vec<usize> {
    let active_index = states
        .iter()
        .position(|state| *state == StepState::InProgress);
    states
        .iter()
        .enumerate()
        .filter_map(|(index, state)| {
            if *state == StepState::Hidden {
                return None;
            }
            match active_index {
                Some(active_index) if Some(index) == welcome_index || index == active_index => {
                    Some(index)
                }
                Some(_) => None,
                None => Some(index),
            }
        })
        .collect()
}

impl WidgetRef for Step {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        match self {
            Step::Welcome(widget) => {
                widget.render_ref(area, buf);
            }
            Step::Auth(widget) => {
                widget.render_ref(area, buf);
            }
            Step::TrustDirectory(widget) => {
                widget.render_ref(area, buf);
            }
        }
    }
}

pub(crate) async fn run_onboarding_app(
    args: OnboardingScreenArgs,
    mut app_server: Option<&mut AppServerSession>,
    tui: &mut Tui,
) -> Result<OnboardingResult> {
    use tokio_stream::StreamExt;

    let app_server_request_handle = args.app_server_request_handle.clone();
    let (claude_event_tx, mut claude_event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut onboarding_screen =
        OnboardingScreen::new(tui, args, AppEventSender::new(claude_event_tx)).await;
    let mut directory_trust_persisted = false;
    // One-time guard to fully clear the screen after ChatGPT login success message is shown
    let mut did_full_clear_after_success = false;

    tui.draw(u16::MAX, |frame| {
        frame.render_widget_ref(&onboarding_screen, frame.area());
    })?;

    let tui_events = tui.event_stream();
    tokio::pin!(tui_events);

    while !onboarding_screen.is_done() {
        tokio::select! {
            event = tui_events.next() => {
                if let Some(event) = event {
                    match event {
                        TuiEvent::Key(key_event) => {
                            onboarding_screen.handle_key_event(key_event);
                            if !directory_trust_persisted {
                                directory_trust_persisted = persist_selected_trust(
                                    &mut onboarding_screen,
                                    app_server_request_handle.clone(),
                                )
                                .await;
                            }
                        }
                        TuiEvent::Paste(text) => {
                            onboarding_screen.handle_paste(text);
                        }
                        TuiEvent::Draw | TuiEvent::Resize => {
                            if !did_full_clear_after_success
                                && onboarding_screen.steps.iter().any(|step| {
                                    if let Step::Auth(w) = step {
                                        w.sign_in_state.read().is_ok_and(|g| {
                                            matches!(&*g, super::auth::SignInState::ChatGptSuccessMessage)
                                        })
                                    } else {
                                        false
                                    }
                                })
                            {
                                // Reset any lingering SGR (underline/color) before clearing
                                let _ = ratatui::crossterm::execute!(
                                    std::io::stdout(),
                                    ratatui::crossterm::style::SetAttribute(
                                        ratatui::crossterm::style::Attribute::Reset
                                    ),
                                    ratatui::crossterm::style::SetAttribute(
                                        ratatui::crossterm::style::Attribute::NoUnderline
                                    ),
                                    ratatui::crossterm::style::SetForegroundColor(
                                        ratatui::crossterm::style::Color::Reset
                                    ),
                                    ratatui::crossterm::style::SetBackgroundColor(
                                        ratatui::crossterm::style::Color::Reset
                                    )
                                );
                                let _ = tui.terminal.clear();
                                did_full_clear_after_success = true;
                            }
                            let _ = tui.draw(u16::MAX, |frame| {
                                frame.render_widget_ref(&onboarding_screen, frame.area());
                            });
                        }
                    }
                }
            }
            event = async {
                match app_server.as_mut() {
                    Some(app_server) => app_server.next_event().await,
                    None => None,
                }
            }, if app_server.is_some() => {
                if let Some(event) = event {
                    match event {
                        AppServerEvent::ServerNotification(notification) => {
                            onboarding_screen.handle_app_server_notification(*notification);
                        }
                        AppServerEvent::Disconnected { message } => {
                            return Err(color_eyre::eyre::eyre!(message));
                        }
                        AppServerEvent::Lagged { .. }
                        | AppServerEvent::ServerRequest(_) => {}
                    }
                }
            }
            event = claude_event_rx.recv() => {
                if let Some(event) = event {
                    onboarding_screen.handle_claude_event(event);
                }
            }
            action = onboarding_screen.provider_auth_action_rx.recv() => {
                if let Some(action) = action {
                    onboarding_screen.handle_provider_auth_action(action);
                }
            }
        }
    }
    // `TuiEventStream` shares the broker-owned crossterm source. The onboarding stream can leave
    // that source registered with its now-stale task waker when this sequential screen exits.
    // Recreate the source before App installs its own stream so the painted composer also accepts
    // keyboard input without requiring a process restart.
    tui.resume_events();
    Ok(OnboardingResult {
        directory_trust_persisted,
        should_exit: onboarding_screen.should_exit(),
        configured_provider: onboarding_screen.configured_provider(),
        deferred_provider_setup: onboarding_screen.deferred_provider_setup(),
    })
}

fn openai_metadata(login_status: LoginStatus) -> codex_login::OpenAiAuthMetadata {
    match login_status {
        LoginStatus::NotAuthenticated => codex_login::OpenAiAuthMetadata::Missing,
        LoginStatus::AuthMode(mode) if mode.has_chatgpt_account() => {
            codex_login::OpenAiAuthMetadata::Account
        }
        LoginStatus::AuthMode(codex_protocol::auth::AuthMode::ApiKey) => {
            codex_login::OpenAiAuthMetadata::ApiKey
        }
        LoginStatus::AuthMode(codex_protocol::auth::AuthMode::Headers)
        | LoginStatus::AuthMode(codex_protocol::auth::AuthMode::AgentIdentity) => {
            codex_login::OpenAiAuthMetadata::ExternallyManaged
        }
        LoginStatus::AuthMode(_) => codex_login::OpenAiAuthMetadata::Unsupported,
    }
}

async fn persist_selected_trust(
    onboarding_screen: &mut OnboardingScreen,
    request_handle: Option<AppServerRequestHandle>,
) -> bool {
    let Some((trust_step_index, trust_target)) = onboarding_screen
        .steps
        .iter()
        .enumerate()
        .find_map(|(index, step)| {
            if let Step::TrustDirectory(widget) = step
                && widget.selection == Some(TrustDirectorySelection::Trust)
            {
                return Some((index, widget.trust_target.clone()));
            }
            None
        })
    else {
        return false;
    };

    let result = match request_handle {
        Some(request_handle) => write_trusted_project(request_handle, &trust_target)
            .await
            .map(|_| ()),
        None => Err(color_eyre::eyre::eyre!("app server unavailable")),
    };

    match result {
        Ok(()) => true,
        Err(error) => {
            let error = format_config_error(&error);
            tracing::error!(
                "failed to persist trusted project state for {}: {error}",
                trust_target.display()
            );
            if let Step::TrustDirectory(widget) = &mut onboarding_screen.steps[trust_step_index] {
                widget.selection = None;
                widget.error = Some(format!(
                    "Failed to set trust for {}: {error}",
                    trust_target.display()
                ));
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::KeyboardHandler;
    use super::OnboardingScreen;
    use super::Step;
    use super::StepState;
    use super::StepStateProvider;
    use super::initial_sign_in_option;
    use super::is_text_key_in_text_field;
    use super::persist_selected_trust;
    use super::provider_api_key_options;
    use super::visible_step_indices;
    use crate::onboarding::auth::SignInOption;
    use crate::onboarding::auth::catalog_sign_in_options;
    use crate::onboarding::trust_directory::TrustDirectorySelection;
    use crate::onboarding::trust_directory::TrustDirectoryWidget;
    use crate::provider_status_host::ProviderStatusHost;
    use crate::tui::FrameRequester;
    use codex_protocol::config_types::ForcedLoginMethod;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;

    #[test]
    fn printable_quit_key_is_text_in_a_text_field_even_when_empty() {
        // #461: an empty field must not quit either, or a value that starts
        // with `q` ends the session and the rest reaches the shell.
        assert!(is_text_key_in_text_field(
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            /*text_entry_active*/ true,
        ));
        assert!(is_text_key_in_text_field(
            KeyEvent::new(KeyCode::Char('Q'), KeyModifiers::SHIFT),
            /*text_entry_active*/ true,
        ));
    }

    #[test]
    fn control_quit_chords_still_quit_from_a_text_field() {
        for key in ['c', 'd'] {
            assert!(!is_text_key_in_text_field(
                KeyEvent::new(KeyCode::Char(key), KeyModifiers::CONTROL),
                /*text_entry_active*/ true,
            ));
        }
    }

    #[test]
    fn printable_quit_key_is_a_shortcut_outside_text_fields() {
        assert!(!is_text_key_in_text_field(
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            /*text_entry_active*/ false,
        ));
    }

    #[test]
    fn forced_chatgpt_never_defaults_to_the_hidden_anthropic_option() {
        assert_eq!(
            initial_sign_in_option(
                &[SignInOption::AnthropicAccount],
                Some(ForcedLoginMethod::Chatgpt),
            ),
            SignInOption::ChatGpt
        );
    }

    #[test]
    fn forced_api_key_never_defaults_to_the_hidden_anthropic_option() {
        assert_eq!(
            initial_sign_in_option(
                &[SignInOption::AnthropicAccount],
                Some(ForcedLoginMethod::Api),
            ),
            SignInOption::ApiKey
        );
    }

    #[tokio::test]
    async fn catalog_surface_keeps_openai_alternatives_corbanu_deferred_and_custom_typed() {
        let home = tempfile::tempdir().unwrap();
        let mut config = crate::legacy_core::config::ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .build()
            .await
            .unwrap();
        config.model_providers.insert(
            "pf53-custom".to_string(),
            codex_model_provider_info::ModelProviderInfo {
                name: "PF53 Custom".to_string(),
                env_key: Some("PF53_CUSTOM_API_KEY".to_string()),
                ..Default::default()
            },
        );
        let host = ProviderStatusHost::from_config(
            &config,
            crate::provider_status_host::ProviderAccountMetadata::default(),
        );
        let api_options = provider_api_key_options(&host);
        let visible = catalog_sign_in_options(&host, &api_options);

        assert!(matches!(visible.first(), Some(SignInOption::DeviceCode)));
        let Some(SignInOption::ProviderApiKey(openai_index)) = visible.get(1) else {
            panic!("OpenAI API key must immediately follow the account capability: {visible:?}");
        };
        assert_eq!(
            api_options[*openai_index].target.provider_id.as_str(),
            codex_model_provider_info::OPENAI_PROVIDER_ID
        );
        assert_eq!(
            api_options[*openai_index].target.storage,
            codex_provider_auth::ApiKeyStorage::OpenAiAuth
        );
        assert_eq!(
            visible
                .iter()
                .filter(|option| matches!(option, SignInOption::CorbanuPlan))
                .count(),
            1
        );
        assert!(!api_options.iter().any(|option| {
            option.target.provider_id.as_str()
                == codex_model_provider_info::CORBANU_PLAN_PROVIDER_ID
        }));

        let custom = api_options
            .iter()
            .find(|option| option.target.provider_id.as_str() == "pf53-custom")
            .expect("custom catalog API-key option");
        assert_eq!(custom.target.runtime_provider_id.as_str(), "pf53-custom");
        assert_eq!(
            custom.target.storage,
            codex_provider_auth::ApiKeyStorage::EnvironmentVariable {
                env_key: "PF53_CUSTOM_API_KEY".to_string(),
            }
        );
        assert_eq!(
            custom.capability,
            codex_provider_auth::ProviderSetupCapability::ApiKey {
                storage: codex_provider_auth::ApiKeyStorage::EnvironmentVariable {
                    env_key: "PF53_CUSTOM_API_KEY".to_string(),
                },
            }
        );
    }

    #[test]
    fn onboarding_keeps_welcome_and_the_current_required_step_visible() {
        assert_eq!(
            visible_step_indices(
                &[
                    StepState::Complete,
                    StepState::Complete,
                    StepState::InProgress,
                ],
                Some(0),
            ),
            vec![0, 2],
            "a completed auth panel must not hide the active trust step"
        );
        assert_eq!(
            visible_step_indices(&[StepState::Complete, StepState::InProgress], Some(0),),
            vec![0, 1],
            "welcome and active auth should remain visible together"
        );
        assert_eq!(
            visible_step_indices(
                &[
                    StepState::Hidden,
                    StepState::Complete,
                    StepState::InProgress
                ],
                Some(0),
            ),
            vec![2],
            "hidden welcome and completed intermediate steps must not consume the viewport"
        );
        assert_eq!(
            visible_step_indices(&[StepState::Complete, StepState::Complete], Some(0),),
            vec![0, 1],
            "the final completed frame remains renderable before onboarding exits"
        );
    }

    #[tokio::test]
    async fn trust_persistence_failure_keeps_trust_step_in_progress() {
        let (_provider_auth_action_tx, provider_auth_action_rx) =
            tokio::sync::mpsc::unbounded_channel();
        let mut onboarding_screen = OnboardingScreen {
            request_frame: FrameRequester::test_dummy(),
            steps: vec![Step::TrustDirectory(TrustDirectoryWidget {
                cwd: PathBuf::from("/workspace/project"),
                trust_target: PathBuf::from("/workspace/project"),
                show_windows_create_sandbox_hint: false,
                should_quit: false,
                selection: Some(TrustDirectorySelection::Trust),
                highlighted: TrustDirectorySelection::Trust,
                error: None,
            })],
            is_done: false,
            should_exit: false,
            provider_auth_controller: codex_provider_auth::ProviderAuthController::default(),
            provider_auth_executor: None,
            provider_auth_action_rx,
        };

        let persisted =
            persist_selected_trust(&mut onboarding_screen, /*request_handle*/ None).await;

        assert!(!persisted);
        let Step::TrustDirectory(widget) = &onboarding_screen.steps[0] else {
            panic!("trust step should remain present");
        };
        assert_eq!(widget.selection, None);
        assert_eq!(widget.get_step_state(), super::StepState::InProgress);
        assert!(
            widget
                .error
                .as_deref()
                .is_some_and(|error| error.contains("app server unavailable"))
        );
    }

    fn auth_screen(widget: crate::onboarding::auth::AuthModeWidget) -> OnboardingScreen {
        let (_provider_auth_action_tx, provider_auth_action_rx) =
            tokio::sync::mpsc::unbounded_channel();
        OnboardingScreen {
            request_frame: FrameRequester::test_dummy(),
            steps: vec![Step::Auth(widget)],
            is_done: false,
            should_exit: false,
            provider_auth_controller: codex_provider_auth::ProviderAuthController::default(),
            provider_auth_executor: None,
            provider_auth_action_rx,
        }
    }

    /// #461: every onboarding text field takes `q` as text, from the first
    /// character on; nothing typed ends onboarding.
    #[tokio::test]
    async fn every_onboarding_text_field_types_q_instead_of_quitting() {
        use crate::onboarding::auth::SignInState;

        let typed = "qprobe-q-key-Q";
        let (claude_input_tx, _claude_input_rx) = tokio::sync::mpsc::unbounded_channel();
        let states = [
            ("API key", SignInState::ApiKeyEntry(Default::default())),
            (
                "Claude token",
                SignInState::ClaudeManagedTokenEntry(Default::default()),
            ),
            (
                "Claude code",
                SignInState::ClaudeCodeLoginCodeEntry {
                    input_tx: claude_input_tx,
                    value: String::new(),
                },
            ),
            (
                "account name",
                SignInState::AccountNameEntry {
                    entry_index: 0,
                    value: String::new(),
                },
            ),
            (
                "account value",
                SignInState::AccountValueEntry {
                    entry_index: 0,
                    name: codex_vault::ProviderAccountName::parse("probe").expect("name"),
                    value: zeroize::Zeroizing::new(String::new()),
                },
            ),
        ];
        for (field, state) in states {
            let (widget, _home) = crate::onboarding::auth::tests::widget_forced_chatgpt().await;
            *widget.sign_in_state.write().unwrap() = state;
            let mut screen = auth_screen(widget);
            for character in typed.chars() {
                let modifiers = if character.is_ascii_uppercase() {
                    KeyModifiers::SHIFT
                } else {
                    KeyModifiers::NONE
                };
                screen.handle_key_event(KeyEvent::new(KeyCode::Char(character), modifiers));
                assert!(
                    !screen.is_done() && !screen.should_exit(),
                    "{field}: {character:?} ended onboarding"
                );
            }
            let Step::Auth(widget) = &screen.steps[0] else {
                panic!("auth step");
            };
            assert_eq!(widget.text_entry_value().as_deref(), Some(typed), "{field}");
        }
    }

    /// A non-bracketed paste with a newline starts the save while the rest of
    /// the value is still arriving; those keys must not quit either.
    #[tokio::test]
    async fn saving_a_typed_value_does_not_quit_on_q() {
        use crate::onboarding::auth::SignInState;

        for state in [
            SignInState::ApiKeySaving,
            SignInState::ClaudeManagedTokenSaving,
            SignInState::AccountSaving,
        ] {
            let (widget, _home) = crate::onboarding::auth::tests::widget_forced_chatgpt().await;
            *widget.sign_in_state.write().unwrap() = state;
            let mut screen = auth_screen(widget);
            screen.handle_key_event(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
            assert!(!screen.is_done() && !screen.should_exit());
        }
    }

    /// Ctrl+C still leaves onboarding from a text field.
    #[tokio::test]
    async fn ctrl_c_still_quits_from_an_onboarding_text_field() {
        use crate::onboarding::auth::SignInState;

        let (widget, _home) = crate::onboarding::auth::tests::widget_forced_chatgpt().await;
        *widget.sign_in_state.write().unwrap() = SignInState::AccountNameEntry {
            entry_index: 0,
            value: String::new(),
        };
        let mut screen = auth_screen(widget);
        screen.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(screen.is_done());
    }

    /// Outside a text field, `q` is still the quit shortcut.
    #[tokio::test]
    async fn q_still_quits_from_the_provider_list() {
        let (widget, _home) = crate::onboarding::auth::tests::widget_forced_chatgpt().await;
        let mut screen = auth_screen(widget);
        screen.handle_key_event(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(screen.is_done());
    }

    /// #462: when config already names the provider, onboarding opens on its
    /// key entry. Saving must complete that provider's setup, not loop back.
    #[tokio::test]
    async fn preset_provider_key_entry_completes_setup_for_that_provider() {
        use crate::onboarding::auth::SignInState;
        use crate::onboarding::provider_setup::ProviderSetupAction;
        use crate::onboarding::provider_setup::ProviderSetupPhase;

        let (mut widget, _home) = crate::onboarding::auth::tests::widget_forced_chatgpt().await;
        widget.forced_login_method = None;
        let options = provider_api_key_options(&widget.provider_status_host);
        let kimi = options
            .iter()
            .find(|option| option.target.provider_id.as_str() != "openai")
            .expect("a non-OpenAI API-key provider")
            .clone();
        widget.api_key_provider_options = vec![kimi.clone()];
        widget.selected_api_key_option = Some(kimi.clone());
        *widget.sign_in_state.write().unwrap() = SignInState::ApiKeyEntry(Default::default());

        widget.begin_initial_api_key_entry();

        let entry = widget
            .provider_status_host
            .catalog()
            .get(kimi.target.provider_id.as_str())
            .expect("catalog entry");
        assert_eq!(
            widget
                .provider_setup_session
                .read()
                .unwrap()
                .snapshot()
                .phase,
            ProviderSetupPhase::Authenticating {
                provider_id: entry.id.clone()
            }
        );
        let configured = widget.provider_setup_session.write().unwrap().dispatch(
            ProviderSetupAction::AuthConfigured {
                provider_id: entry.id.clone(),
                runtime_provider_id: entry.runtime_provider_ids.first().expect("runtime").clone(),
            },
        );
        assert!(configured.applied, "the saved key's completion was ignored");
    }
}
