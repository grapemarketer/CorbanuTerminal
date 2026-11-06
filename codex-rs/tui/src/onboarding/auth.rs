//! Authentication step UI and state transitions used by onboarding.
//!
//! This module owns the auth-step state machine (ChatGPT login/device-code/API
//! key), renders the corresponding UI, and handles auth-scoped keyboard input.
//! It intentionally does not decide onboarding flow completion; the enclosing
//! onboarding screen coordinates step progression.

#![allow(clippy::unwrap_used)]

use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::AccountLoginCompletedNotification;
use codex_app_server_protocol::AccountUpdatedNotification;
use codex_app_server_protocol::AuthMode as ApiAuthMode;
use codex_app_server_protocol::CancelLoginAccountParams;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::LoginAccountParams;
use codex_app_server_protocol::LoginAccountResponse;
use codex_login::OPENAI_API_KEY_ENV_VAR;
use codex_login::read_openai_api_key_from_env;
use codex_model_provider_info::CLAUDE_PLAN_PROVIDER_ID;
use codex_model_provider_info::OPENAI_PROVIDER_ID;
use codex_protocol::auth::AuthMode;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::prelude::Widget;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Borders;
use ratatui::widgets::Paragraph;
use ratatui::widgets::WidgetRef;
use ratatui::widgets::Wrap;

use codex_protocol::config_types::ForcedLoginMethod;
use std::cell::Cell;
use std::sync::Arc;
use std::sync::RwLock;
use uuid::Uuid;

use crate::LoginStatus;
use crate::app_event::AppEvent;
use crate::app_event::ClaudeSubscriptionTokenSecret;
use crate::app_event_sender::AppEventSender;
use crate::chatwidget::claude_code_login::ClaudeCodeLoginInput;
use crate::key_hint::KeyBinding;
use crate::key_hint::KeyBindingListExt;
use crate::motion::MotionMode;
use crate::motion::shimmer_text;
use crate::onboarding::keys;
use crate::onboarding::onboarding_screen::KeyboardHandler;
use crate::onboarding::onboarding_screen::StepStateProvider;
use crate::onboarding::provider_setup::DeferredProviderSetup;
use crate::onboarding::provider_setup::ProviderSetupAction;
use crate::onboarding::provider_setup::ProviderSetupPhase;
use crate::onboarding::provider_setup::ProviderSetupSession;
use crate::provider_status_host::ProviderStatusHost;
use crate::selection_list::list_position_label;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::mark_buffer_hyperlinks;
use crate::terminal_hyperlinks::visible_lines;
use crate::tui::FrameRequester;
use codex_provider_auth::ApiKeyAuthTarget;
use codex_provider_auth::ApiKeyFlowIntent;
use codex_provider_auth::ApiKeyFlowStart;
use codex_provider_auth::ApiKeySecret;
use codex_provider_auth::ManagedApiKeyMetadata;
use codex_provider_auth::ProviderAuthAction;
use codex_provider_auth::ProviderAuthFlowSnapshot;
use codex_provider_auth::ProviderAvailabilityState;
use codex_provider_auth::ProviderConfigurationState;
use codex_provider_auth::ProviderEligibilityState;
use codex_provider_auth::ProviderSetupCapability;
use codex_provider_auth::ProviderStatusCatalog;

/// Marks buffer cells that have cyan+underlined style as an OSC 8 hyperlink.
///
/// Terminal emulators recognise the OSC 8 escape sequence and treat the entire
/// marked region as a single clickable link, regardless of row wrapping.  This
/// is necessary because ratatui's cell-based rendering emits `MoveTo` at every
/// row boundary, which breaks normal terminal URL detection for long URLs that
/// wrap across multiple rows.
pub(crate) fn mark_url_hyperlink(buf: &mut Buffer, area: Rect, url: &str) {
    crate::terminal_hyperlinks::mark_url_hyperlink(buf, area, url);
}

/// Marks any underlined buffer cells as an OSC 8 hyperlink.
pub(crate) fn mark_underlined_hyperlink(buf: &mut Buffer, area: Rect, url: &str) {
    crate::terminal_hyperlinks::mark_underlined_hyperlink(buf, area, url);
}

use super::onboarding_screen::StepState;

mod headless_chatgpt_login;
mod named_account;
#[cfg(test)]
#[path = "auth/named_account_tests.rs"]
mod named_account_tests;

#[derive(Clone)]
pub(crate) enum SignInState {
    PickMode,
    ChatGptContinueInBrowser(ContinueInBrowserState),
    #[allow(dead_code)]
    ChatGptDeviceCode(ContinueWithDeviceCodeState),
    ChatGptSuccessMessage,
    ChatGptSuccess,
    ApiKeyEntry(ApiKeyInputState),
    ApiKeySaving,
    ApiKeyConfigured {
        provider: Option<String>,
    },
    ClaudeAuthMethodChoice {
        highlighted: ClaudeAuthMethod,
    },
    ClaudeManagedTokenEntry(ApiKeyInputState),
    ClaudeManagedTokenSaving,
    ClaudeCodeLoginChecking,
    ClaudeCodeLoginPending {
        input_tx: tokio::sync::mpsc::UnboundedSender<ClaudeCodeLoginInput>,
    },
    ClaudeCodeLoginReady {
        verification_url: String,
        input_tx: tokio::sync::mpsc::UnboundedSender<ClaudeCodeLoginInput>,
    },
    ClaudeCodeLoginCodeEntry {
        input_tx: tokio::sync::mpsc::UnboundedSender<ClaudeCodeLoginInput>,
        value: String,
    },
    ClaudeAccountConfigured {
        source: codex_provider_auth::ClaudeCredentialSource,
    },
    /// PF-84-S04: what to do with a configured provider's single row.
    ConfiguredProviderChoice {
        entry_index: usize,
        highlighted: usize,
    },
    AccountNameEntry {
        entry_index: usize,
        value: String,
    },
    AccountValueEntry {
        entry_index: usize,
        name: codex_vault::ProviderAccountName,
        /// Masked on screen; edited in place and wiped when dropped.
        value: zeroize::Zeroizing<String>,
    },
    AccountSaving,
    AccountSaved {
        message: String,
    },
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignInOption {
    ChatGpt,
    DeviceCode,
    AnthropicAccount,
    ApiKey,
    ProviderApiKey(usize),
    ProviderRuntime(usize),
    CorbanuPlan,
    Done,
}

#[derive(Clone, Copy)]
enum ConfiguredProviderMethod {
    OpenAiAccount,
    ApiKey,
    Claude(codex_provider_auth::ClaudeCredentialSource),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ClaudeAuthMethod {
    #[default]
    ManagedToken,
    ClaudeCodeLogin,
}

const API_KEY_DISABLED_MESSAGE: &str = "API key login is disabled.";
const API_KEY_MASK_CHAR: char = '•';

fn onboarding_request_id() -> codex_app_server_protocol::RequestId {
    codex_app_server_protocol::RequestId::String(Uuid::new_v4().to_string())
}

fn masked_api_key(value: &str) -> String {
    std::iter::repeat_n(API_KEY_MASK_CHAR, value.chars().count()).collect()
}

fn read_non_empty_env_var(env_var: &str) -> Option<String> {
    std::env::var(env_var)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn read_api_key_from_env(provider_env_key: Option<&str>) -> Option<(String, String)> {
    match provider_env_key {
        Some(env_var) => read_non_empty_env_var(env_var).map(|value| (env_var.to_string(), value)),
        None => {
            read_openai_api_key_from_env().map(|value| (OPENAI_API_KEY_ENV_VAR.to_string(), value))
        }
    }
}

pub(super) async fn cancel_login_attempt(
    request_handle: &AppServerRequestHandle,
    login_id: String,
) {
    let _ = request_handle
        .request_typed::<codex_app_server_protocol::CancelLoginAccountResponse>(
            ClientRequest::CancelLoginAccount {
                request_id: onboarding_request_id(),
                params: CancelLoginAccountParams { login_id },
            },
        )
        .await;
}

#[derive(Clone, Default)]
pub(crate) struct ApiKeyInputState {
    value: String,
    prepopulated_env_var: Option<String>,
}

impl ApiKeyInputState {
    pub(crate) fn from_env(provider_env_key: Option<&str>) -> Self {
        match read_api_key_from_env(provider_env_key) {
            Some((env_var, value)) => Self {
                value,
                prepopulated_env_var: Some(env_var),
            },
            None => Self::default(),
        }
    }
}

#[derive(Clone)]
/// Used to manage the lifecycle of SpawnedLogin and ensure it gets cleaned up.
pub(crate) struct ContinueInBrowserState {
    login_id: String,
    auth_url: String,
}

#[derive(Clone)]
pub(crate) struct ContinueWithDeviceCodeState {
    request_id: String,
    login_id: Option<String>,
    verification_url: Option<String>,
    user_code: Option<String>,
}

impl ContinueWithDeviceCodeState {
    pub(crate) fn pending(request_id: String) -> Self {
        Self {
            request_id,
            login_id: None,
            verification_url: None,
            user_code: None,
        }
    }

    pub(crate) fn ready(
        request_id: String,
        login_id: String,
        verification_url: String,
        user_code: String,
    ) -> Self {
        Self {
            request_id,
            login_id: Some(login_id),
            verification_url: Some(verification_url),
            user_code: Some(user_code),
        }
    }

    pub(crate) fn login_id(&self) -> Option<&str> {
        self.login_id.as_deref()
    }

    pub(crate) fn is_showing_copyable_auth(&self) -> bool {
        self.verification_url
            .as_deref()
            .is_some_and(|url| !url.is_empty())
            && self
                .user_code
                .as_deref()
                .is_some_and(|user_code| !user_code.is_empty())
    }
}

impl KeyboardHandler for AuthModeWidget {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if self.handle_named_account_key_event(&key_event) {
            return;
        }
        if self.handle_api_key_entry_key_event(&key_event) {
            return;
        }
        if self.handle_claude_auth_key_event(&key_event) {
            return;
        }

        if keys::MOVE_UP.is_pressed(key_event) {
            self.move_highlight(/*delta*/ -1);
            return;
        }
        if keys::MOVE_DOWN.is_pressed(key_event) {
            self.move_highlight(/*delta*/ 1);
            return;
        }
        if keys::SELECT_FIRST.is_pressed(key_event) {
            self.select_option_by_index(/*index*/ 0);
            return;
        }
        if keys::SELECT_SECOND.is_pressed(key_event) {
            self.select_option_by_index(/*index*/ 1);
            return;
        }
        if keys::SELECT_THIRD.is_pressed(key_event) {
            self.select_option_by_index(/*index*/ 2);
            return;
        }
        if keys::CONFIRM.is_pressed(key_event) {
            let sign_in_state = { (*self.sign_in_state.read().unwrap()).clone() };
            match sign_in_state {
                SignInState::PickMode => {
                    let highlighted = self.effective_highlight(&self.displayed_sign_in_options());
                    self.handle_sign_in_option(highlighted);
                }
                SignInState::ChatGptSuccessMessage | SignInState::ChatGptSuccess => {
                    self.finish_openai_account_setup();
                }
                SignInState::ApiKeyConfigured { provider } => {
                    self.record_configured(
                        provider.as_deref().unwrap_or(OPENAI_PROVIDER_ID),
                        ConfiguredProviderMethod::ApiKey,
                    );
                }
                SignInState::ClaudeAccountConfigured { source } => {
                    self.record_configured(
                        CLAUDE_PLAN_PROVIDER_ID,
                        ConfiguredProviderMethod::Claude(source),
                    );
                }
                _ => {}
            }
            return;
        }
        if keys::CANCEL.is_pressed(key_event) {
            tracing::info!("Cancel onboarding auth step");
            self.cancel_active_attempt();
        }
    }

    fn handle_paste(&mut self, pasted: String) {
        if self.handle_named_account_paste(&pasted) {
            return;
        }
        if !self.handle_claude_auth_paste(&pasted) {
            let _ = self.handle_api_key_entry_paste(pasted);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct ApiKeyProviderOption {
    pub target: ApiKeyAuthTarget,
    pub capability: ProviderSetupCapability,
    pub name: String,
}

impl ApiKeyProviderOption {
    pub(crate) fn environment_variable(&self) -> Option<&str> {
        match &self.target.storage {
            codex_provider_auth::ApiKeyStorage::OpenAiAuth => None,
            codex_provider_auth::ApiKeyStorage::EnvironmentVariable { env_key } => Some(env_key),
        }
    }
}

#[cfg(test)]
pub(crate) fn catalog_sign_in_options(
    host: &ProviderStatusHost,
    api_key_options: &[ApiKeyProviderOption],
) -> Vec<SignInOption> {
    let statuses = host.resolve();
    catalog_sign_in_options_from_statuses(host, &statuses, api_key_options)
}

pub(crate) fn catalog_sign_in_options_from_statuses(
    host: &ProviderStatusHost,
    statuses: &ProviderStatusCatalog,
    api_key_options: &[ApiKeyProviderOption],
) -> Vec<SignInOption> {
    let mut options = Vec::new();
    for (entry_index, entry) in host.catalog().entries().iter().enumerate() {
        let status = statuses.get(entry.id.as_str());
        let has_noninteractive_capability = entry.setup_capabilities.iter().any(|capability| {
            matches!(
                capability,
                ProviderSetupCapability::Local { .. }
                    | ProviderSetupCapability::CommandAuth { .. }
                    | ProviderSetupCapability::StatusOnly { .. }
            )
        });
        let offer_existing = status.is_some_and(|status| {
            super::provider_setup::provider_should_offer_existing_selection(
                status,
                has_noninteractive_capability,
            )
        }) && !entry.runtime_provider_ids.is_empty();
        if offer_existing {
            // PF-84-S04: a configured provider gets exactly one row; its setup
            // methods ("Replace with ...") and "Add another account" sit behind it.
            options.push(SignInOption::ProviderRuntime(entry_index));
        } else {
            options.extend(entry_setup_options(entry, api_key_options));
        }
    }
    options
}

/// The setup rows one catalog entry offers while it is not configured.
pub(crate) fn entry_setup_options(
    entry: &codex_provider_auth::ProviderCatalogEntry,
    api_key_options: &[ApiKeyProviderOption],
) -> Vec<SignInOption> {
    entry
        .setup_capabilities
        .iter()
        .filter_map(|capability| match capability {
            ProviderSetupCapability::OpenAiAccount => Some(SignInOption::DeviceCode),
            ProviderSetupCapability::ClaudeAccount => Some(SignInOption::AnthropicAccount),
            ProviderSetupCapability::CorbanuPlan => Some(SignInOption::CorbanuPlan),
            ProviderSetupCapability::ApiKey { .. } => api_key_options
                .iter()
                .position(|candidate| {
                    candidate.target.provider_id == entry.id && candidate.capability == *capability
                })
                .map(SignInOption::ProviderApiKey),
            ProviderSetupCapability::Local { .. }
            | ProviderSetupCapability::CommandAuth { .. }
            | ProviderSetupCapability::StatusOnly { .. } => None,
        })
        .collect()
}

#[derive(Clone)]
#[allow(dead_code)]
pub(crate) struct AuthModeWidget {
    pub request_frame: FrameRequester,
    pub highlighted_mode: SignInOption,
    pub error: Arc<RwLock<Option<String>>>,
    pub sign_in_state: Arc<RwLock<SignInState>>,
    pub login_status: LoginStatus,
    pub replace_provider_with_openai_on_login: bool,
    pub app_server_request_handle: AppServerRequestHandle,
    pub forced_login_method: Option<ForcedLoginMethod>,
    pub api_key_provider_id: String,
    pub api_key_provider_name: String,
    pub api_key_env_var: Option<String>,
    pub api_key_provider_options: Vec<ApiKeyProviderOption>,
    pub selected_api_key_option: Option<ApiKeyProviderOption>,
    pub animations_enabled: bool,
    pub animations_suppressed: Cell<bool>,
    pub codex_home: std::path::PathBuf,
    pub claude_event_tx: AppEventSender,
    pub provider_setup_session: Arc<RwLock<ProviderSetupSession>>,
    pub provider_status_host: ProviderStatusHost,
    pub provider_statuses: Arc<RwLock<ProviderStatusCatalog>>,
    pub provider_auth_action_tx: tokio::sync::mpsc::UnboundedSender<ProviderAuthAction>,
    /// PF-84-S04: provider id -> how it takes a named account; empty while
    /// the `named_accounts` feature is off.
    pub named_account_methods:
        std::collections::BTreeMap<String, Vec<crate::provider_named_accounts::AddAccountMethod>>,
}

impl AuthModeWidget {
    pub(crate) fn set_animations_suppressed(&self, suppressed: bool) {
        self.animations_suppressed.set(suppressed);
    }

    pub(crate) fn should_suppress_animations(&self) -> bool {
        matches!(
            &*self.sign_in_state.read().unwrap(),
            SignInState::ChatGptContinueInBrowser(_)
                | SignInState::ChatGptDeviceCode(_)
                | SignInState::ClaudeManagedTokenEntry(_)
                | SignInState::ClaudeCodeLoginReady { .. }
                | SignInState::ClaudeCodeLoginCodeEntry { .. }
        )
    }

    pub(crate) fn cancel_active_attempt(&self) {
        let mut sign_in_state = self.sign_in_state.write().unwrap();
        match &*sign_in_state {
            SignInState::ChatGptContinueInBrowser(state) => {
                let request_handle = self.app_server_request_handle.clone();
                let login_id = state.login_id.clone();
                tokio::spawn(async move {
                    cancel_login_attempt(&request_handle, login_id).await;
                });
            }
            SignInState::ChatGptDeviceCode(state) => {
                if let Some(login_id) = state.login_id().map(str::to_owned) {
                    let request_handle = self.app_server_request_handle.clone();
                    tokio::spawn(async move {
                        cancel_login_attempt(&request_handle, login_id).await;
                    });
                }
            }
            SignInState::ClaudeCodeLoginPending { input_tx }
            | SignInState::ClaudeCodeLoginReady { input_tx, .. }
            | SignInState::ClaudeCodeLoginCodeEntry { input_tx, .. } => {
                let _ = input_tx.send(ClaudeCodeLoginInput::Cancel);
            }
            _ => return,
        }
        *sign_in_state = SignInState::PickMode;
        drop(sign_in_state);
        self.provider_setup_session
            .write()
            .unwrap()
            .dispatch(ProviderSetupAction::AuthCancelled);
        self.set_error(/*message*/ None);
        self.request_frame.schedule_frame();
    }

    fn set_error(&self, message: Option<String>) {
        *self.error.write().unwrap() = message;
    }

    fn error_message(&self) -> Option<String> {
        self.error.read().unwrap().clone()
    }

    /// Returns whether a text field (an API key, a Claude token or code, or a
    /// named account's name or value) has focus, or its value is being saved.
    /// While it does, every printable key is text, never a shortcut: a
    /// non-bracketed paste with a newline starts the save while the rest of
    /// the value is still arriving.
    pub(crate) fn is_text_entry_active(&self) -> bool {
        // Exhaustive on purpose: a new state must decide whether it takes text.
        self.sign_in_state.read().is_ok_and(|guard| match &*guard {
            SignInState::ApiKeyEntry(_)
            | SignInState::ApiKeySaving
            | SignInState::ClaudeManagedTokenEntry(_)
            | SignInState::ClaudeManagedTokenSaving
            | SignInState::ClaudeCodeLoginCodeEntry { .. }
            | SignInState::AccountNameEntry { .. }
            | SignInState::AccountValueEntry { .. }
            | SignInState::AccountSaving => true,
            SignInState::PickMode
            | SignInState::ChatGptContinueInBrowser(_)
            | SignInState::ChatGptDeviceCode(_)
            | SignInState::ChatGptSuccessMessage
            | SignInState::ChatGptSuccess
            | SignInState::ApiKeyConfigured { .. }
            | SignInState::ClaudeAuthMethodChoice { .. }
            | SignInState::ClaudeCodeLoginChecking
            | SignInState::ClaudeCodeLoginPending { .. }
            | SignInState::ClaudeCodeLoginReady { .. }
            | SignInState::ClaudeAccountConfigured { .. }
            | SignInState::ConfiguredProviderChoice { .. }
            | SignInState::AccountSaved { .. }
            | SignInState::Complete => false,
        })
    }

    /// The focused text field's current value, for tests.
    #[cfg(test)]
    pub(crate) fn text_entry_value(&self) -> Option<String> {
        match &*self.sign_in_state.read().unwrap() {
            SignInState::ApiKeyEntry(state) | SignInState::ClaudeManagedTokenEntry(state) => {
                Some(state.value.clone())
            }
            SignInState::ClaudeCodeLoginCodeEntry { value, .. }
            | SignInState::AccountNameEntry { value, .. } => Some(value.clone()),
            SignInState::AccountValueEntry { value, .. } => Some(value.to_string()),
            _ => None,
        }
    }

    fn confirm_binding(&self) -> KeyBinding {
        keys::CONFIRM[0]
    }

    fn cancel_binding(&self) -> KeyBinding {
        keys::CANCEL[0]
    }

    fn is_api_login_allowed(&self) -> bool {
        !matches!(self.forced_login_method, Some(ForcedLoginMethod::Chatgpt))
    }

    fn provider_picker_enabled(&self) -> bool {
        self.forced_login_method.is_none()
            && !catalog_sign_in_options_from_statuses(
                &self.provider_status_host,
                &self.provider_statuses.read().unwrap(),
                &self.api_key_provider_options,
            )
            .is_empty()
    }

    fn is_chatgpt_login_allowed(&self) -> bool {
        !matches!(self.forced_login_method, Some(ForcedLoginMethod::Api))
    }

    fn is_device_code_login_allowed(&self) -> bool {
        self.provider_picker_enabled() || self.is_chatgpt_login_allowed()
    }

    fn displayed_sign_in_options(&self) -> Vec<SignInOption> {
        if self.provider_picker_enabled() {
            let mut options = catalog_sign_in_options_from_statuses(
                &self.provider_status_host,
                &self.provider_statuses.read().unwrap(),
                &self.api_key_provider_options,
            );
            options.push(SignInOption::Done);
            return options;
        }
        if self.forced_api_uses_corbanu_route() {
            return vec![SignInOption::CorbanuPlan, SignInOption::Done];
        }

        if self.is_chatgpt_login_allowed() {
            let mut options = vec![SignInOption::ChatGpt];
            options.push(SignInOption::DeviceCode);
            if self.is_api_login_allowed() {
                options.push(SignInOption::ApiKey);
            }
            options
        } else if self.is_api_login_allowed() {
            vec![SignInOption::ApiKey]
        } else {
            Vec::new()
        }
    }

    fn selectable_sign_in_options(&self) -> Vec<SignInOption> {
        if self.provider_picker_enabled() {
            return self.displayed_sign_in_options();
        }
        if self.forced_api_uses_corbanu_route() {
            return self.displayed_sign_in_options();
        }

        let mut options = Vec::new();
        if self.is_chatgpt_login_allowed() {
            options.push(SignInOption::ChatGpt);
            options.push(SignInOption::DeviceCode);
        }
        if self.is_api_login_allowed() {
            options.push(SignInOption::ApiKey);
        }
        options
    }

    fn move_highlight(&mut self, delta: isize) {
        let options = self.selectable_sign_in_options();
        if options.is_empty() {
            return;
        }

        let current_index = options
            .iter()
            .position(|option| *option == self.effective_highlight(&options))
            .unwrap_or(0);
        let next_index =
            (current_index as isize + delta).rem_euclid(options.len() as isize) as usize;
        self.highlighted_mode = options[next_index];
    }

    fn select_option_by_index(&mut self, index: usize) {
        let options = self.displayed_sign_in_options();
        if let Some(option) = options.get(index).copied() {
            self.handle_sign_in_option(option);
        }
    }

    fn handle_sign_in_option(&mut self, option: SignInOption) {
        match option {
            SignInOption::ChatGpt => {
                if self.is_chatgpt_login_allowed() {
                    self.begin_provider(OPENAI_PROVIDER_ID);
                    self.start_chatgpt_login();
                }
            }
            SignInOption::DeviceCode => {
                if self.is_device_code_login_allowed() {
                    self.begin_provider(OPENAI_PROVIDER_ID);
                    self.start_device_code_login();
                }
            }
            SignInOption::AnthropicAccount => {
                if self.provider_picker_enabled() {
                    self.begin_provider(CLAUDE_PLAN_PROVIDER_ID);
                    self.set_error(/*message*/ None);
                    *self.sign_in_state.write().unwrap() = SignInState::ClaudeAuthMethodChoice {
                        highlighted: ClaudeAuthMethod::ManagedToken,
                    };
                    self.request_frame.schedule_frame();
                }
            }
            SignInOption::ApiKey => {
                if self.is_api_login_allowed() {
                    self.begin_provider(&self.api_key_entry_provider_id());
                    self.start_api_key_entry();
                } else {
                    self.disallow_api_login();
                }
            }
            SignInOption::ProviderApiKey(index) => {
                if self.select_provider_api_key_option(index) {
                    self.begin_provider(&self.api_key_provider_id.clone());
                    self.start_api_key_entry();
                }
            }
            SignInOption::ProviderRuntime(index) => {
                if !self.open_configured_choice(index) {
                    self.select_existing_provider(index);
                }
            }
            SignInOption::CorbanuPlan => {
                let queued = !self
                    .provider_setup_session
                    .read()
                    .unwrap()
                    .snapshot()
                    .queued_corbanu;
                self.provider_setup_session
                    .write()
                    .unwrap()
                    .dispatch(ProviderSetupAction::QueueCorbanu(queued));
                self.request_frame.schedule_frame();
            }
            SignInOption::Done => {
                let transition = self
                    .provider_setup_session
                    .write()
                    .unwrap()
                    .dispatch(ProviderSetupAction::Done);
                if transition.applied {
                    *self.sign_in_state.write().unwrap() = SignInState::Complete;
                } else {
                    self.set_error(Some(
                        "Configure a usable provider or queue Corbanu API before Done.".to_string(),
                    ));
                }
                self.request_frame.schedule_frame();
            }
        }
    }

    /// The catalog provider the API-key field saves to. Setup must begin for
    /// this provider, or its completion is ignored and onboarding loops (#462).
    fn api_key_entry_provider_id(&self) -> String {
        self.selected_api_key_option.as_ref().map_or_else(
            || OPENAI_PROVIDER_ID.to_string(),
            |option| option.target.provider_id.as_str().to_string(),
        )
    }

    /// Begins provider setup when onboarding opens straight on API-key entry
    /// (config already names the provider), as choosing the row would.
    pub(crate) fn begin_initial_api_key_entry(&mut self) {
        if !matches!(
            &*self.sign_in_state.read().unwrap(),
            SignInState::ApiKeyEntry(_)
        ) {
            return;
        }
        if self.forced_api_uses_corbanu_route() {
            // #1571: there is no key to paste for this provider; open on its
            // Corbanu API row instead of a prompt that cannot save.
            self.highlighted_mode = SignInOption::CorbanuPlan;
            *self.sign_in_state.write().unwrap() = SignInState::PickMode;
            return;
        }
        self.begin_provider(&self.api_key_entry_provider_id());
    }

    /// #1571: API-key sign-in is forced (`forced_login_method = "api"`) and the
    /// configured provider has no pasted-key option, but is set up through the
    /// Corbanu API wallet row (Corbanu Plan). That row is its API-key setup.
    fn forced_api_uses_corbanu_route(&self) -> bool {
        matches!(self.forced_login_method, Some(ForcedLoginMethod::Api))
            && self.selected_api_key_option.is_none()
            && self
                .provider_status_host
                .catalog()
                .entries()
                .iter()
                .any(|entry| {
                    (entry.id.as_str() == self.api_key_provider_id
                        || entry
                            .runtime_provider_ids
                            .iter()
                            .any(|runtime_id| runtime_id.as_str() == self.api_key_provider_id))
                        && entry
                            .setup_capabilities
                            .iter()
                            .any(|capability| *capability == ProviderSetupCapability::CorbanuPlan)
                })
    }

    fn begin_provider(&self, provider_id: &str) {
        let Some(entry) = self.provider_status_host.catalog().get(provider_id) else {
            return;
        };
        self.provider_setup_session
            .write()
            .unwrap()
            .dispatch(ProviderSetupAction::Begin {
                provider_id: entry.id.clone(),
            });
    }

    /// How a setup row reads behind a configured provider ("Replace with ...").
    fn sign_in_option_method(&self, option: SignInOption) -> String {
        match option {
            SignInOption::ChatGpt | SignInOption::DeviceCode => {
                "OpenAI account sign-in".to_string()
            }
            SignInOption::AnthropicAccount => "Claude account sign-in".to_string(),
            SignInOption::ApiKey | SignInOption::ProviderApiKey(_) => "a new API key".to_string(),
            SignInOption::CorbanuPlan => "Corbanu API".to_string(),
            SignInOption::ProviderRuntime(_) | SignInOption::Done => "setup".to_string(),
        }
    }

    fn select_existing_provider(&mut self, index: usize) {
        let Some(entry) = self.provider_status_host.catalog().entries().get(index) else {
            self.set_error(Some(
                "Selected provider is no longer available.".to_string(),
            ));
            return;
        };
        let Some(runtime_provider_id) = entry.runtime_provider_ids.first().cloned() else {
            self.set_error(Some("Selected provider has no runtime.".to_string()));
            return;
        };
        let provider_id = entry.id.clone();
        let transition = self.provider_setup_session.write().unwrap().dispatch(
            ProviderSetupAction::SelectExisting {
                provider_id,
                runtime_provider_id,
            },
        );
        if transition.applied {
            self.set_error(/*message*/ None);
        } else {
            self.set_error(Some("Selected provider could not be applied.".to_string()));
        }
        self.request_frame.schedule_frame();
    }

    fn record_configured(&self, provider_id: &str, method: ConfiguredProviderMethod) {
        let Some(entry) = self.provider_status_host.catalog().get(provider_id) else {
            self.set_error(Some(format!(
                "Configured provider `{provider_id}` is not in the catalog"
            )));
            return;
        };
        let Some(runtime_provider_id) = entry.runtime_provider_ids.first().cloned() else {
            self.set_error(Some(format!(
                "Configured provider `{provider_id}` has no runtime"
            )));
            return;
        };
        let provider_id = entry.id.clone();
        match method {
            ConfiguredProviderMethod::OpenAiAccount => {
                self.provider_status_host.mark_openai_account()
            }
            ConfiguredProviderMethod::ApiKey => {}
            ConfiguredProviderMethod::Claude(source) => {
                self.provider_status_host.mark_claude_configured(source)
            }
        }
        let transition = self.provider_setup_session.write().unwrap().dispatch(
            ProviderSetupAction::AuthConfigured {
                provider_id,
                runtime_provider_id: runtime_provider_id.clone(),
            },
        );
        for effect in transition.effects {
            match effect {
                crate::onboarding::provider_setup::ProviderSetupEffect::Activate(id) => {
                    if !self.provider_status_host.activate(id.as_str()) {
                        self.fail_provider_attempt("Provider activation could not be persisted.");
                        return;
                    }
                    let Some(status) = self.provider_status_host.resolve_provider(id.as_str())
                    else {
                        self.fail_provider_attempt("Provider status could not be reconciled.");
                        return;
                    };
                    if status.configuration != ProviderConfigurationState::Configured
                        || status.eligibility != ProviderEligibilityState::Active
                    {
                        self.fail_provider_attempt(
                            "Provider activation did not reconcile to configured and active.",
                        );
                        return;
                    }
                    let usable = status.availability == ProviderAvailabilityState::Ready;
                    let resolved = self.provider_setup_session.write().unwrap().dispatch(
                        ProviderSetupAction::ActivationResolved {
                            provider_id: id,
                            runtime_provider_id: runtime_provider_id.clone(),
                            usable,
                        },
                    );
                    for resolved_effect in resolved.effects {
                        if let crate::onboarding::provider_setup::ProviderSetupEffect::PersistInitialSelection(runtime) = resolved_effect {
                            debug_assert_eq!(runtime, runtime_provider_id);
                        }
                    }
                    if !usable {
                        self.set_error(Some(
                            "Provider is configured and active, but is not currently usable."
                                .to_string(),
                        ));
                    }
                }
                crate::onboarding::provider_setup::ProviderSetupEffect::PersistInitialSelection(
                    _,
                ) => {
                    unreachable!("selection follows activation resolution")
                }
                crate::onboarding::provider_setup::ProviderSetupEffect::BeginDeferred(_)
                | crate::onboarding::provider_setup::ProviderSetupEffect::Finish
                | crate::onboarding::provider_setup::ProviderSetupEffect::ReturnToProviderList => {}
            }
        }
        *self.provider_statuses.write().unwrap() = self.provider_status_host.resolve();
        self.set_error(/*message*/ None);
        if self.provider_picker_enabled() {
            *self.sign_in_state.write().unwrap() = SignInState::PickMode;
        } else {
            let done = self
                .provider_setup_session
                .write()
                .unwrap()
                .dispatch(ProviderSetupAction::Done);
            *self.sign_in_state.write().unwrap() = if done.applied {
                SignInState::Complete
            } else {
                SignInState::PickMode
            };
        }
        self.request_frame.schedule_frame();
    }

    fn finish_openai_account_setup(&self) {
        if self.replace_provider_with_openai_on_login {
            self.record_configured(OPENAI_PROVIDER_ID, ConfiguredProviderMethod::OpenAiAccount);
            return;
        }

        self.provider_status_host.mark_openai_account();
        *self.sign_in_state.write().unwrap() = SignInState::Complete;
        self.request_frame.schedule_frame();
    }

    fn provider_status_description(&self, provider_id: &str, fallback: &str) -> String {
        let session = self.provider_setup_session.read().unwrap();
        let Some(entry) = self.provider_status_host.catalog().get(provider_id) else {
            return fallback.to_string();
        };
        let local_runtime = entry
            .setup_capabilities
            .iter()
            .any(|capability| matches!(capability, ProviderSetupCapability::Local { .. }));
        let snapshot = session.snapshot();
        // Ollama and LM Studio are never probed, so neither "ready" nor
        // "unavailable" would be true: say what is known.
        if local_runtime && snapshot.configured.contains(&entry.id) {
            if snapshot.usable.contains(&entry.id) {
                "Local (not checked) · active".to_string()
            } else {
                "Local (not checked)".to_string()
            }
        } else if snapshot.usable.contains(&entry.id) {
            "Configured · active · ready".to_string()
        } else if snapshot.configured.contains(&entry.id) {
            "Configured · unavailable".to_string()
        } else {
            fallback.to_string()
        }
    }

    fn ambient_api_key_required(&self) -> bool {
        matches!(self.forced_login_method, Some(ForcedLoginMethod::Api))
    }

    fn provider_api_key_required(&self) -> bool {
        self.ambient_api_key_required()
            || self.api_key_env_var.is_some()
            || self.provider_picker_enabled()
    }

    fn api_key_provider_label(&self) -> String {
        if self.api_key_provider_name.is_empty() {
            "provider".to_string()
        } else {
            self.api_key_provider_name.clone()
        }
    }

    /// The highlighted row. A setup row that turned into its provider's single
    /// configured row (PF-84-S04) keeps the focus on that row.
    fn effective_highlight(&self, options: &[SignInOption]) -> SignInOption {
        if options.contains(&self.highlighted_mode) {
            return self.highlighted_mode;
        }
        self.provider_status_host
            .catalog()
            .entries()
            .iter()
            .enumerate()
            .find(|(entry_index, entry)| {
                options.contains(&SignInOption::ProviderRuntime(*entry_index))
                    && entry_setup_options(entry, &self.api_key_provider_options)
                        .contains(&self.highlighted_mode)
            })
            .map_or(self.highlighted_mode, |(entry_index, _)| {
                SignInOption::ProviderRuntime(entry_index)
            })
    }

    fn select_provider_api_key_option(&mut self, index: usize) -> bool {
        let Some(option) = self.api_key_provider_options.get(index).cloned() else {
            return false;
        };
        self.api_key_provider_id = option.target.provider_id.as_str().to_string();
        self.api_key_env_var = option.environment_variable().map(str::to_string);
        self.api_key_provider_name = option.name.clone();
        self.selected_api_key_option = Some(option);
        self.set_error(/*message*/ None);
        true
    }

    fn disallow_api_login(&mut self) {
        self.highlighted_mode = self
            .displayed_sign_in_options()
            .into_iter()
            .next()
            .unwrap_or(SignInOption::ApiKey);
        self.set_error(Some(API_KEY_DISABLED_MESSAGE.to_string()));
        *self.sign_in_state.write().unwrap() = SignInState::PickMode;
        self.request_frame.schedule_frame();
    }

    fn render_pick_mode(&self, area: Rect, buf: &mut Buffer) {
        let header_lines: Vec<Line> = if self.provider_picker_enabled() {
            vec![
                Line::from(vec![
                    "  ".into(),
                    "Choose a provider account for Corbanu Terminal".into(),
                ]),
                "".into(),
            ]
        } else if self.provider_api_key_required() {
            vec![
                Line::from(vec![
                    "  ".into(),
                    format!(
                        "Connect a {} API key to use Corbanu Terminal",
                        self.api_key_provider_label()
                    )
                    .into(),
                ]),
                "".into(),
            ]
        } else {
            vec![
                Line::from(vec![
                    "  ".into(),
                    "Sign in with ChatGPT to use OpenAI-hosted models".into(),
                ]),
                Line::from(vec![
                    "  ".into(),
                    "or connect an API key for usage-based billing".into(),
                ]),
                "".into(),
            ]
        };

        let highlighted_row = self.effective_highlight(&self.displayed_sign_in_options());
        let create_mode_item = |idx: usize,
                                selected_mode: SignInOption,
                                text: &str,
                                description: &str|
         -> Vec<Line<'static>> {
            let is_selected = highlighted_row == selected_mode;
            let caret = if is_selected { ">" } else { " " };

            let line1 = if is_selected {
                Line::from(vec![
                    format!("{caret} {index}. ", index = idx + 1).cyan().dim(),
                    text.to_string().cyan(),
                ])
            } else {
                format!("  {index}. {text}", index = idx + 1).into()
            };

            let line2 = if is_selected {
                Line::from(format!("     {description}"))
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::DIM)
            } else {
                Line::from(format!("     {description}"))
                    .style(Style::default().add_modifier(Modifier::DIM))
            };

            vec![line1, line2]
        };

        let chatgpt_description = if !self.is_chatgpt_login_allowed() {
            "ChatGPT login is disabled"
        } else {
            "Usage included with Plus, Pro, Business, and Enterprise plans"
        };
        let device_code_description = "Sign in from another device with a one-time code";

        let displayed_options = self.displayed_sign_in_options();
        let mut option_lines = Vec::new();
        for (idx, option) in displayed_options.iter().copied().enumerate() {
            match option {
                SignInOption::ChatGpt => {
                    option_lines.extend(create_mode_item(
                        idx,
                        option,
                        "Sign in with ChatGPT",
                        chatgpt_description,
                    ));
                }
                SignInOption::DeviceCode => {
                    let (text, description) = if self.provider_picker_enabled() {
                        ("Provider: OpenAI Codex Account", "Sign in with device code")
                    } else {
                        ("Sign in with Device Code", device_code_description)
                    };
                    let description =
                        self.provider_status_description(OPENAI_PROVIDER_ID, description);
                    option_lines.extend(create_mode_item(idx, option, text, &description));
                }
                SignInOption::AnthropicAccount => {
                    let description = self.provider_status_description(
                        CLAUDE_PLAN_PROVIDER_ID,
                        "Use Claude subscription access; long-lived token recommended or Claude Code login",
                    );
                    option_lines.extend(create_mode_item(
                        idx,
                        option,
                        "Provider: Anthropic Claude Account",
                        &description,
                    ));
                }
                SignInOption::ApiKey => {
                    let text = if self.provider_api_key_required() {
                        format!("Provide your {} API key", self.api_key_provider_label())
                    } else {
                        "Provide your own API key".to_string()
                    };
                    option_lines.extend(create_mode_item(
                        idx,
                        option,
                        &text,
                        "Pay for what you use",
                    ));
                }
                SignInOption::ProviderApiKey(provider_index) => {
                    if let Some(provider) = self.api_key_provider_options.get(provider_index) {
                        let storage_description = match &provider.target.storage {
                            codex_provider_auth::ApiKeyStorage::OpenAiAuth => {
                                "Use the OpenAI auth store".to_string()
                            }
                            codex_provider_auth::ApiKeyStorage::EnvironmentVariable { env_key } => {
                                format!("Use a key stored as {env_key}")
                            }
                        };
                        let description = self.provider_status_description(
                            provider.target.provider_id.as_str(),
                            &storage_description,
                        );
                        let text =
                            crate::onboarding::onboarding_screen::provider_api_key_display_name(
                                provider,
                            );
                        option_lines.extend(create_mode_item(idx, option, &text, &description));
                    }
                }
                SignInOption::ProviderRuntime(provider_index) => {
                    if let Some(provider) = self
                        .provider_status_host
                        .catalog()
                        .entries()
                        .get(provider_index)
                    {
                        let selected = self
                            .provider_setup_session
                            .read()
                            .unwrap()
                            .snapshot()
                            .first_fresh_runtime
                            .as_ref()
                            .is_some_and(|runtime| provider.runtime_provider_ids.contains(runtime));
                        let description = self.provider_status_description(
                            provider.id.as_str(),
                            if selected {
                                "Selected as the current provider"
                            } else {
                                "Use this provider without an enrollment step"
                            },
                        );
                        option_lines.extend(create_mode_item(
                            idx,
                            option,
                            &format!("Provider: {}", provider.display_name),
                            &description,
                        ));
                    }
                }
                SignInOption::CorbanuPlan => {
                    let queued = self
                        .provider_setup_session
                        .read()
                        .unwrap()
                        .snapshot()
                        .queued_corbanu;
                    option_lines.extend(create_mode_item(
                        idx,
                        option,
                        if queued {
                            "Corbanu API (queued)"
                        } else {
                            "Corbanu API"
                        },
                        "Queue protected wallet and API-key setup; it runs after Done",
                    ));
                }
                SignInOption::Done => {
                    option_lines.extend(create_mode_item(
                        idx,
                        option,
                        "Done",
                        if self.provider_setup_session.read().unwrap().can_finish() {
                            "Finish provider setup"
                        } else {
                            "Configure a usable provider or queue Corbanu API first"
                        },
                    ));
                }
            }
            option_lines.push("".into());
        }

        let mut footer_lines = Vec::new();
        if !self.is_api_login_allowed() {
            footer_lines.push(
                "  API key login is disabled by this workspace. Sign in with ChatGPT to continue."
                    .dim()
                    .into(),
            );
            footer_lines.push("".into());
        }
        let instructions_line = footer_lines.len();
        footer_lines.push(Line::from(vec![
            "  Use ↑/↓ to choose · Press ".dim(),
            self.confirm_binding().into(),
            " to continue".dim(),
        ]));
        if let Some(err) = self.error_message() {
            footer_lines.push("".into());
            footer_lines.push(err.red().into());
        }

        let header_height = (header_lines.len() as u16).min(area.height);
        let footer_height = (footer_lines.len() as u16).min(area.height - header_height);
        let options_height = area.height - header_height - footer_height;
        let header_area = Rect::new(area.x, area.y, area.width, header_height);
        let options_area = Rect::new(area.x, area.y + header_height, area.width, options_height);
        let footer_area = Rect::new(
            area.x,
            area.bottom() - footer_height,
            area.width,
            footer_height,
        );

        Paragraph::new(header_lines)
            .wrap(Wrap { trim: false })
            .render(header_area, buf);

        let highlighted = self.effective_highlight(&displayed_options);
        let selected_option_index = displayed_options
            .iter()
            .position(|option| *option == highlighted)
            .unwrap_or(0);
        let selected_end_line = selected_option_index.saturating_mul(3).saturating_add(2);
        let option_scroll = selected_end_line
            .saturating_sub(options_height.saturating_sub(1) as usize)
            .min(u16::MAX as usize) as u16;
        // #1288: rows exist beyond the visible window.
        if option_lines.len() > options_height as usize
            && let Some(line) = footer_lines.get_mut(instructions_line)
        {
            let position = format!(
                " · {}",
                list_position_label(Some(selected_option_index), displayed_options.len())
            )
            .dim();
            if line.width() + position.width() <= area.width as usize {
                line.push_span(position);
            }
        }
        Paragraph::new(option_lines)
            .wrap(Wrap { trim: false })
            .scroll((option_scroll, 0))
            .render(options_area, buf);

        Paragraph::new(footer_lines)
            .wrap(Wrap { trim: false })
            .render(footer_area, buf);
    }

    fn render_continue_in_browser(&self, area: Rect, buf: &mut Buffer) {
        let mut spans = vec!["  ".into()];
        if self.animations_enabled && !self.animations_suppressed.get() {
            // Schedule a follow-up frame to keep the shimmer animation going.
            self.request_frame
                .schedule_frame_in(std::time::Duration::from_millis(100));
            spans.extend(shimmer_text(
                "Finish signing in via your browser",
                MotionMode::Animated,
            ));
        } else {
            spans.push("Finish signing in via your browser".into());
        }
        let mut lines = vec![spans.into(), "".into()];

        let sign_in_state = self.sign_in_state.read().unwrap();
        let auth_url = if let SignInState::ChatGptContinueInBrowser(state) = &*sign_in_state
            && !state.auth_url.is_empty()
        {
            lines.push(crate::provider_auth_presentation::OPENAI_BROWSER_GUIDANCE.into());
            lines.push("".into());
            lines.push(Line::from(vec![
                "  ".into(),
                state.auth_url.as_str().cyan().underlined(),
            ]));
            lines.push("".into());
            lines.push(Line::from(vec![
                "  On a remote or headless machine? Press ".into(),
                self.cancel_binding().into(),
                " and choose ".into(),
                "Sign in with Device Code".cyan(),
                ".".into(),
            ]));
            lines.push("".into());
            Some(state.auth_url.clone())
        } else {
            None
        };

        lines.push(Line::from(vec![
            "  Press ".dim(),
            self.cancel_binding().into(),
            " to cancel".dim(),
        ]));
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);

        // Wrap cyan+underlined URL cells with OSC 8 so the terminal treats
        // the entire region as a single clickable hyperlink.
        if let Some(url) = &auth_url {
            mark_url_hyperlink(buf, area, url);
        }
    }

    fn render_claude_method_choice(
        &self,
        area: Rect,
        buf: &mut Buffer,
        highlighted: ClaudeAuthMethod,
    ) {
        use crate::chatwidget::claude_auth_presentation as copy;
        let option = |selected: bool, label: &str, description: &str| {
            let marker = if selected { ">" } else { " " };
            vec![
                if selected {
                    format!("{marker} {label}").cyan().into()
                } else {
                    format!("{marker} {label}").into()
                },
                format!("    {description}").dim().into(),
                "".into(),
            ]
        };
        let mut lines: Vec<Line> = vec![
            copy::METHOD_TITLE.bold().into(),
            copy::METHOD_SUBTITLE.into(),
            "".into(),
        ];
        lines.extend(option(
            highlighted == ClaudeAuthMethod::ManagedToken,
            copy::MANAGED_TOKEN_METHOD_NAME,
            copy::MANAGED_TOKEN_METHOD_DESCRIPTION,
        ));
        lines.extend(option(
            highlighted == ClaudeAuthMethod::ClaudeCodeLogin,
            copy::CLAUDE_CODE_LOGIN_METHOD_NAME,
            copy::CLAUDE_CODE_LOGIN_METHOD_DESCRIPTION,
        ));
        lines.push(copy::METHOD_FOOTER.dim().into());
        lines.push(Line::from(vec![
            "  Use ↑/↓ to choose · Press ".dim(),
            self.confirm_binding().into(),
            " to continue · Press ".dim(),
            self.cancel_binding().into(),
            " to go back".dim(),
        ]));
        if let Some(error) = self.error_message() {
            lines.push("".into());
            lines.push(error.red().into());
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_claude_secret_entry(
        &self,
        area: Rect,
        buf: &mut Buffer,
        value: &str,
        authorization_code: bool,
    ) {
        let (title, guidance, field) = if authorization_code {
            (
                "Claude Code Plan login",
                "Paste the one-time authorization code from the browser.",
                "Authorization code",
            )
        } else {
            (
                crate::chatwidget::claude_auth_presentation::MANAGED_TOKEN_ENTRY_TITLE,
                crate::chatwidget::claude_auth_presentation::MANAGED_TOKEN_ENTRY_GUIDANCE,
                crate::chatwidget::claude_auth_presentation::MANAGED_TOKEN_ENTRY_LABEL,
            )
        };
        let [intro_area, input_area, footer_area] = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(3),
            Constraint::Min(3),
        ])
        .areas(area);
        Paragraph::new(vec![
            format!("> {title}").bold().into(),
            "".into(),
            guidance.into(),
        ])
        .wrap(Wrap { trim: false })
        .render(intro_area, buf);
        let content: Line = if value.is_empty() {
            format!("Paste {field}").dim().into()
        } else {
            masked_api_key(value).into()
        };
        Paragraph::new(content)
            .block(
                Block::default()
                    .title(field)
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .render(input_area, buf);
        let mut footer = vec![
            Line::from(vec![
                "  Press ".dim(),
                self.confirm_binding().into(),
                " to continue".dim(),
            ]),
            Line::from(vec![
                "  Press ".dim(),
                self.cancel_binding().into(),
                " to go back".dim(),
            ]),
        ];
        if let Some(error) = self.error_message() {
            footer.push(error.red().into());
        }
        Paragraph::new(footer)
            .wrap(Wrap { trim: false })
            .render(footer_area, buf);
    }

    fn render_claude_code_login(
        &self,
        area: Rect,
        buf: &mut Buffer,
        verification_url: Option<&str>,
    ) {
        let mut lines: Vec<Line> = vec!["  Claude Code Plan".bold().into(), "".into()];
        if let Some(url) = verification_url {
            lines.push("  Sign in with your Claude subscription in the browser:".into());
            lines.push("".into());
            lines.push(Line::from(vec!["  ".into(), url.cyan().underlined()]));
            lines.push("".into());
            lines.push(Line::from(vec![
                "  Press ".dim(),
                self.confirm_binding().into(),
                " to paste the one-time code, or ".dim(),
                self.cancel_binding().into(),
                " to cancel".dim(),
            ]));
        } else {
            lines.push("  Starting Claude Code login...".dim().into());
            lines.push("".into());
            lines.push(Line::from(vec![
                "  Press ".dim(),
                self.cancel_binding().into(),
                " to cancel".dim(),
            ]));
        }
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
        if let Some(url) = verification_url {
            mark_url_hyperlink(buf, area, url);
        }
    }

    fn render_chatgpt_success_message(&self, area: Rect, buf: &mut Buffer) {
        let mut docs_line = HyperlinkLine::new(Line::from("  For more details see the ").dim());
        docs_line.push_span(
            "Codex docs".underlined(),
            Some("https://developers.openai.com/codex/security"),
        );
        let mut preferences_line =
            HyperlinkLine::new(Line::from("  Uses your plan's rate limits and ").dim());
        preferences_line.push_span(
            "training data preferences".underlined(),
            Some("https://chatgpt.com/#settings"),
        );

        let lines = vec![
            HyperlinkLine::new(
                "✓ Signed in with your ChatGPT account"
                    .fg(Color::Green)
                    .into(),
            ),
            "".into(),
            "  Before you start:".into(),
            "".into(),
            "  Decide how much autonomy you want to grant Codex".into(),
            docs_line,
            "".into(),
            "  Codex can make mistakes".into(),
            HyperlinkLine::new(
                "  Review the code it writes and commands it runs"
                    .dim()
                    .into(),
            ),
            "".into(),
            "  Powered by your ChatGPT account".into(),
            preferences_line,
            "".into(),
            HyperlinkLine::new(Line::from(vec![
                "  Press ".fg(Color::Cyan),
                self.confirm_binding().into(),
                " to continue".fg(Color::Cyan),
            ])),
        ];

        Paragraph::new(visible_lines(lines.clone()))
            .wrap(Wrap { trim: false })
            .render(area, buf);
        mark_buffer_hyperlinks(buf, area, &lines, /*scroll_rows*/ 0);
    }

    fn render_chatgpt_success(&self, area: Rect, buf: &mut Buffer) {
        let lines = vec![
            "✓ Signed in with your ChatGPT account"
                .fg(Color::Green)
                .into(),
        ];

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_api_key_configured(&self, area: Rect, buf: &mut Buffer) {
        let configured_message = if self.provider_api_key_required() {
            format!("✓ {} API key configured", self.api_key_provider_label())
        } else {
            "✓ API key configured".to_string()
        };
        let usage_message = if self.provider_api_key_required() {
            format!(
                "  Corbanu Terminal will use {} with your API key.",
                self.api_key_provider_label()
            )
        } else {
            "  Codex will use usage-based billing with your API key.".to_string()
        };
        let lines = vec![
            configured_message.fg(Color::Green).into(),
            "".into(),
            usage_message.into(),
        ];

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .render(area, buf);
    }

    fn render_api_key_saving(&self, area: Rect, buf: &mut Buffer) {
        let provider = if self.provider_api_key_required() {
            format!("{} API key", self.api_key_provider_label())
        } else {
            "API key".to_string()
        };
        Paragraph::new(vec![
            format!("  Saving {provider} securely…").into(),
            "".into(),
            "  Keep Corbanu Terminal open while the encrypted vault is updated."
                .dim()
                .into(),
        ])
        .wrap(Wrap { trim: false })
        .render(area, buf);
    }

    fn render_api_key_entry(&self, area: Rect, buf: &mut Buffer, state: &ApiKeyInputState) {
        let [intro_area, input_area, footer_area] = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(3),
            Constraint::Min(2),
        ])
        .areas(area);

        let mut intro_lines: Vec<Line> = vec![
            Line::from(vec![
                "> ".into(),
                if self.provider_api_key_required() {
                    format!("Use your {} API key", self.api_key_provider_label())
                } else {
                    "Use your own API key for usage-based billing".to_string()
                }
                .bold(),
            ]),
            "".into(),
            crate::provider_auth_presentation::api_key_guidance(
                &self
                    .selected_api_key_option
                    .as_ref()
                    .map(|option| option.target.storage.clone())
                    .unwrap_or(codex_provider_auth::ApiKeyStorage::OpenAiAuth),
            )
            .into(),
            "".into(),
        ];
        if let Some(env_var) = state.prepopulated_env_var.as_ref() {
            intro_lines.push(format!("  Detected {env_var} environment variable.").into());
            intro_lines.push(
                "  Paste a different key if you prefer to use another account."
                    .dim()
                    .into(),
            );
            intro_lines.push("".into());
        }
        Paragraph::new(intro_lines)
            .wrap(Wrap { trim: false })
            .render(intro_area, buf);

        let content_line: Line = if state.value.is_empty() {
            vec!["Paste or type your API key".dim()].into()
        } else {
            Line::from(masked_api_key(&state.value))
        };
        Paragraph::new(content_line)
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title("API key")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .render(input_area, buf);

        let mut footer_lines: Vec<Line> = vec![
            Line::from(vec![
                "  Press ".dim(),
                self.confirm_binding().into(),
                " to save".dim(),
            ]),
            Line::from(vec![
                "  Press ".dim(),
                self.cancel_binding().into(),
                " to go back".dim(),
            ]),
        ];
        if let Some(error) = self.error_message() {
            footer_lines.push("".into());
            footer_lines.push(error.red().into());
        }
        Paragraph::new(footer_lines)
            .wrap(Wrap { trim: false })
            .render(footer_area, buf);
    }

    fn handle_claude_auth_key_event(&mut self, key_event: &KeyEvent) -> bool {
        let state = self.sign_in_state.read().unwrap().clone();
        match state {
            SignInState::ClaudeAuthMethodChoice { highlighted } => {
                let next = if keys::MOVE_UP.is_pressed(*key_event)
                    || keys::MOVE_DOWN.is_pressed(*key_event)
                {
                    Some(match highlighted {
                        ClaudeAuthMethod::ManagedToken => ClaudeAuthMethod::ClaudeCodeLogin,
                        ClaudeAuthMethod::ClaudeCodeLogin => ClaudeAuthMethod::ManagedToken,
                    })
                } else {
                    None
                };
                if let Some(highlighted) = next {
                    *self.sign_in_state.write().unwrap() =
                        SignInState::ClaudeAuthMethodChoice { highlighted };
                } else if keys::CONFIRM.is_pressed(*key_event) {
                    match highlighted {
                        ClaudeAuthMethod::ManagedToken => {
                            *self.sign_in_state.write().unwrap() =
                                SignInState::ClaudeManagedTokenEntry(ApiKeyInputState::default());
                        }
                        ClaudeAuthMethod::ClaudeCodeLogin => {
                            self.start_claude_code_login_selection();
                        }
                    }
                } else if keys::CANCEL.is_pressed(*key_event) {
                    *self.sign_in_state.write().unwrap() = SignInState::PickMode;
                }
                self.request_frame.schedule_frame();
                true
            }
            SignInState::ClaudeManagedTokenEntry(mut input) => {
                if keys::CANCEL.is_pressed(*key_event) {
                    *self.sign_in_state.write().unwrap() = SignInState::ClaudeAuthMethodChoice {
                        highlighted: ClaudeAuthMethod::ManagedToken,
                    };
                } else if keys::CONFIRM.is_pressed(*key_event) {
                    let token = input.value.trim().to_string();
                    if token.is_empty() {
                        self.set_error(Some("Subscription token cannot be empty".to_string()));
                    } else {
                        self.save_claude_managed_token(token);
                    }
                } else {
                    match key_event.code {
                        KeyCode::Backspace => {
                            input.value.pop();
                        }
                        KeyCode::Char(character)
                            if key_event.kind == KeyEventKind::Press
                                && !key_event.modifiers.intersects(
                                    KeyModifiers::SUPER | KeyModifiers::CONTROL | KeyModifiers::ALT,
                                ) =>
                        {
                            input.value.push(character);
                        }
                        _ => return true,
                    }
                    self.set_error(/*message*/ None);
                    *self.sign_in_state.write().unwrap() =
                        SignInState::ClaudeManagedTokenEntry(input);
                }
                self.request_frame.schedule_frame();
                true
            }
            SignInState::ClaudeCodeLoginPending { input_tx } => {
                if keys::CANCEL.is_pressed(*key_event) {
                    let _ = input_tx.send(ClaudeCodeLoginInput::Cancel);
                    *self.sign_in_state.write().unwrap() = SignInState::ClaudeAuthMethodChoice {
                        highlighted: ClaudeAuthMethod::ClaudeCodeLogin,
                    };
                }
                self.request_frame.schedule_frame();
                true
            }
            SignInState::ClaudeCodeLoginReady { input_tx, .. } => {
                if keys::CANCEL.is_pressed(*key_event) {
                    let _ = input_tx.send(ClaudeCodeLoginInput::Cancel);
                    *self.sign_in_state.write().unwrap() = SignInState::ClaudeAuthMethodChoice {
                        highlighted: ClaudeAuthMethod::ClaudeCodeLogin,
                    };
                } else if keys::CONFIRM.is_pressed(*key_event) {
                    *self.sign_in_state.write().unwrap() = SignInState::ClaudeCodeLoginCodeEntry {
                        input_tx,
                        value: String::new(),
                    };
                }
                self.request_frame.schedule_frame();
                true
            }
            SignInState::ClaudeCodeLoginCodeEntry {
                input_tx,
                mut value,
            } => {
                if keys::CANCEL.is_pressed(*key_event) {
                    let _ = input_tx.send(ClaudeCodeLoginInput::Cancel);
                    *self.sign_in_state.write().unwrap() = SignInState::ClaudeAuthMethodChoice {
                        highlighted: ClaudeAuthMethod::ClaudeCodeLogin,
                    };
                } else if keys::CONFIRM.is_pressed(*key_event) {
                    let code = value.trim().to_string();
                    if code.is_empty() {
                        self.set_error(Some("Authorization code cannot be empty".to_string()));
                    } else {
                        let _ = input_tx.send(ClaudeCodeLoginInput::AuthorizationCode(
                            zeroize::Zeroizing::new(code),
                        ));
                        *self.sign_in_state.write().unwrap() =
                            SignInState::ClaudeCodeLoginPending { input_tx };
                    }
                } else {
                    match key_event.code {
                        KeyCode::Backspace => {
                            value.pop();
                        }
                        KeyCode::Char(character)
                            if key_event.kind == KeyEventKind::Press
                                && !key_event.modifiers.intersects(
                                    KeyModifiers::SUPER | KeyModifiers::CONTROL | KeyModifiers::ALT,
                                ) =>
                        {
                            value.push(character);
                        }
                        _ => return true,
                    }
                    self.set_error(/*message*/ None);
                    *self.sign_in_state.write().unwrap() =
                        SignInState::ClaudeCodeLoginCodeEntry { input_tx, value };
                }
                self.request_frame.schedule_frame();
                true
            }
            SignInState::ClaudeManagedTokenSaving | SignInState::ClaudeCodeLoginChecking => true,
            _ => false,
        }
    }

    fn handle_claude_auth_paste(&mut self, pasted: &str) -> bool {
        let pasted = pasted.trim();
        if pasted.is_empty() {
            return false;
        }
        let mut state = self.sign_in_state.write().unwrap();
        match &mut *state {
            SignInState::ClaudeManagedTokenEntry(input) => input.value.push_str(pasted),
            SignInState::ClaudeCodeLoginCodeEntry { value, .. } => value.push_str(pasted),
            _ => return false,
        }
        drop(state);
        self.set_error(/*message*/ None);
        self.request_frame.schedule_frame();
        true
    }

    fn save_claude_managed_token(&mut self, token: String) {
        *self.sign_in_state.write().unwrap() = SignInState::ClaudeManagedTokenSaving;
        let codex_home = self.codex_home.clone();
        let tx = self.claude_event_tx.clone();
        tokio::spawn(async move {
            let result = crate::chatwidget::claude_code_login::enroll_managed_subscription_token(
                codex_home,
                ClaudeSubscriptionTokenSecret::new(token),
            )
            .await;
            tx.send(AppEvent::ClaudeManagedSubscriptionTokenSaved { result });
        });
    }

    fn start_claude_code_login_selection(&mut self) {
        *self.sign_in_state.write().unwrap() = SignInState::ClaudeCodeLoginChecking;
        let codex_home = self.codex_home.clone();
        let tx = self.claude_event_tx.clone();
        tokio::spawn(async move {
            let result = crate::chatwidget::claude_code_login::select_existing_claude_code_login(
                std::path::Path::new("claude"),
                /*health_executable*/ None,
                codex_home.as_path(),
                std::time::Duration::from_secs(10),
            )
            .await;
            tx.send(AppEvent::ClaudeCodePlanLoginSelectionChecked { result });
        });
    }

    pub(crate) fn handle_claude_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::ProviderAccountMetadataResolved { metadata } => {
                self.provider_status_host.update_account_metadata(metadata);
                let statuses = self.provider_status_host.resolve();
                let can_refresh = self
                    .provider_setup_session
                    .read()
                    .unwrap()
                    .can_refresh_statuses();
                if can_refresh {
                    self.provider_setup_session
                        .write()
                        .unwrap()
                        .refresh_from_statuses(statuses.entries());
                }
                *self.provider_statuses.write().unwrap() = statuses;
            }
            AppEvent::ClaudeManagedSubscriptionTokenSaved { result } => match result {
                Ok(_) => {
                    self.set_error(/*message*/ None);
                    *self.sign_in_state.write().unwrap() = SignInState::ClaudeAccountConfigured {
                        source: codex_provider_auth::ClaudeCredentialSource::Managed,
                    };
                }
                Err(error) => {
                    self.set_error(Some(error));
                    *self.sign_in_state.write().unwrap() = SignInState::ClaudeAuthMethodChoice {
                        highlighted: ClaudeAuthMethod::ManagedToken,
                    };
                }
            },
            AppEvent::ClaudeCodePlanLoginSelectionChecked { result } => match result {
                Ok(true) => {
                    self.set_error(/*message*/ None);
                    *self.sign_in_state.write().unwrap() = SignInState::ClaudeAccountConfigured {
                        source: codex_provider_auth::ClaudeCredentialSource::ClaudeCodeLogin,
                    };
                }
                Ok(false) => {
                    let input_tx = crate::chatwidget::claude_code_login::start(
                        self.claude_event_tx.clone(),
                        self.codex_home.clone(),
                    );
                    *self.sign_in_state.write().unwrap() =
                        SignInState::ClaudeCodeLoginPending { input_tx };
                }
                Err(error) => {
                    self.set_error(Some(error));
                    *self.sign_in_state.write().unwrap() = SignInState::ClaudeAuthMethodChoice {
                        highlighted: ClaudeAuthMethod::ClaudeCodeLogin,
                    };
                }
            },
            AppEvent::ClaudeCodePlanLoginReady {
                verification_url,
                input_tx,
            } => {
                *self.sign_in_state.write().unwrap() = SignInState::ClaudeCodeLoginReady {
                    verification_url,
                    input_tx,
                };
            }
            AppEvent::OpenClaudeCodePlanLoginCodeEntry { input_tx } => {
                *self.sign_in_state.write().unwrap() = SignInState::ClaudeCodeLoginCodeEntry {
                    input_tx,
                    value: String::new(),
                };
            }
            AppEvent::ClaudeCodePlanLoginFinished { result } => {
                let login_is_active = matches!(
                    &*self.sign_in_state.read().unwrap(),
                    SignInState::ClaudeCodeLoginPending { .. }
                        | SignInState::ClaudeCodeLoginReady { .. }
                        | SignInState::ClaudeCodeLoginCodeEntry { .. }
                );
                if !login_is_active {
                    return;
                }
                match result {
                    Some(Ok(_)) => {
                        self.set_error(/*message*/ None);
                        *self.sign_in_state.write().unwrap() =
                            SignInState::ClaudeAccountConfigured {
                                source:
                                    codex_provider_auth::ClaudeCredentialSource::ClaudeCodeLogin,
                            };
                    }
                    Some(Err(error)) => {
                        self.set_error(Some(error));
                        *self.sign_in_state.write().unwrap() =
                            SignInState::ClaudeAuthMethodChoice {
                                highlighted: ClaudeAuthMethod::ClaudeCodeLogin,
                            };
                    }
                    None => {
                        *self.sign_in_state.write().unwrap() =
                            SignInState::ClaudeAuthMethodChoice {
                                highlighted: ClaudeAuthMethod::ClaudeCodeLogin,
                            };
                    }
                }
            }
            _ => return,
        }
        self.request_frame.schedule_frame();
    }

    fn handle_api_key_entry_key_event(&mut self, key_event: &KeyEvent) -> bool {
        let mut should_save: Option<String> = None;
        let mut should_request_frame = false;

        {
            let mut guard = self.sign_in_state.write().unwrap();
            if let SignInState::ApiKeyEntry(state) = &mut *guard {
                if keys::CANCEL.is_pressed(*key_event) {
                    *guard = SignInState::PickMode;
                    self.provider_setup_session
                        .write()
                        .unwrap()
                        .dispatch(ProviderSetupAction::AuthCancelled);
                    self.set_error(/*message*/ None);
                    should_request_frame = true;
                } else if keys::CONFIRM.is_pressed(*key_event) {
                    let trimmed = state.value.trim().to_string();
                    if trimmed.is_empty() {
                        self.set_error(Some("API key cannot be empty".to_string()));
                        should_request_frame = true;
                    } else {
                        should_save = Some(trimmed);
                    }
                } else {
                    match key_event.code {
                        KeyCode::Backspace => {
                            if state.prepopulated_env_var.is_some() {
                                state.value.clear();
                                state.prepopulated_env_var = None;
                            } else {
                                state.value.pop();
                            }
                            self.set_error(/*message*/ None);
                            should_request_frame = true;
                        }
                        KeyCode::Char(c)
                            if key_event.kind == KeyEventKind::Press
                                && !key_event.modifiers.contains(KeyModifiers::SUPER)
                                && !key_event.modifiers.contains(KeyModifiers::CONTROL)
                                && !key_event.modifiers.contains(KeyModifiers::ALT) =>
                        {
                            if state.prepopulated_env_var.is_some() {
                                state.value.clear();
                                state.prepopulated_env_var = None;
                            }
                            state.value.push(c);
                            self.set_error(/*message*/ None);
                            should_request_frame = true;
                        }
                        _ => {}
                    }
                }
                // handled; let guard drop before potential save
            } else {
                return false;
            }
        }

        if let Some(api_key) = should_save {
            self.save_api_key(api_key);
        } else if should_request_frame {
            self.request_frame.schedule_frame();
        }
        true
    }

    fn handle_api_key_entry_paste(&mut self, pasted: String) -> bool {
        let trimmed = pasted.trim();
        if trimmed.is_empty() {
            return false;
        }

        let mut guard = self.sign_in_state.write().unwrap();
        if let SignInState::ApiKeyEntry(state) = &mut *guard {
            if state.prepopulated_env_var.is_some() {
                state.value = trimmed.to_string();
                state.prepopulated_env_var = None;
            } else {
                state.value.push_str(trimmed);
            }
            self.set_error(/*message*/ None);
        } else {
            return false;
        }

        drop(guard);
        self.request_frame.schedule_frame();
        true
    }

    fn start_api_key_entry(&mut self) {
        if !self.is_api_login_allowed() {
            self.disallow_api_login();
            return;
        }
        self.set_error(/*message*/ None);
        let prefill_from_env = read_api_key_from_env(self.api_key_env_var.as_deref());
        let mut guard = self.sign_in_state.write().unwrap();
        match &mut *guard {
            SignInState::ApiKeyEntry(state) => {
                if state.value.is_empty() {
                    if let Some((env_var, prefill)) = prefill_from_env.as_ref() {
                        state.value = prefill.clone();
                        state.prepopulated_env_var = Some(env_var.clone());
                    } else {
                        state.prepopulated_env_var = None;
                    }
                }
            }
            _ => {
                *guard = SignInState::ApiKeyEntry(ApiKeyInputState {
                    value: prefill_from_env
                        .as_ref()
                        .map(|(_, value)| value.clone())
                        .unwrap_or_default(),
                    prepopulated_env_var: prefill_from_env.map(|(env_var, _)| env_var),
                });
            }
        }
        drop(guard);
        self.request_frame.schedule_frame();
    }

    fn save_api_key(&mut self, api_key: String) {
        if !self.is_api_login_allowed() {
            self.disallow_api_login();
            return;
        }
        self.set_error(/*message*/ None);
        {
            let mut state = self.sign_in_state.write().unwrap();
            if matches!(&*state, SignInState::ApiKeySaving) {
                return;
            }
            *state = SignInState::ApiKeySaving;
        }
        let Some(option) = self.selected_api_key_option.as_ref() else {
            self.fail_provider_attempt("Provider API-key selection is no longer available.");
            return;
        };
        let Some(entry) = self
            .provider_status_host
            .catalog()
            .get(option.target.provider_id.as_str())
        else {
            self.fail_provider_attempt("Provider is missing from the shared catalog.");
            return;
        };
        if !entry
            .setup_capabilities
            .iter()
            .any(|capability| capability == &option.capability)
        {
            self.fail_provider_attempt("Provider API-key capability changed.");
            return;
        }
        let target = option.target.clone();
        let metadata = self.provider_status_host.api_key_metadata(&target);
        let intent = if matches!(metadata.managed, ManagedApiKeyMetadata::Stored { .. }) {
            ApiKeyFlowIntent::Replace
        } else {
            ApiKeyFlowIntent::Add
        };
        for action in [
            ProviderAuthAction::StartApiKey(ApiKeyFlowStart {
                target,
                intent,
                metadata,
            }),
            ProviderAuthAction::SetApiKey(ApiKeySecret::new(api_key)),
            ProviderAuthAction::Submit,
        ] {
            let _ = self.provider_auth_action_tx.send(action);
        }
        self.request_frame.schedule_frame();
    }

    pub(crate) fn on_api_key_configured(&self, provider_id: &str) {
        *self.sign_in_state.write().unwrap() = SignInState::ApiKeyConfigured {
            provider: Some(provider_id.to_string()),
        };
        self.request_frame.schedule_frame();
    }

    pub(crate) fn on_provider_auth_snapshot(&self, snapshot: &ProviderAuthFlowSnapshot) {
        if let Some(failure) = crate::provider_account_feedback::account_failure(snapshot) {
            self.fail_provider_attempt(failure.message);
            return;
        }
        if matches!(
            snapshot,
            ProviderAuthFlowSnapshot::Blocked { .. } | ProviderAuthFlowSnapshot::Failed { .. }
        ) {
            self.fail_provider_attempt("API-key setup did not complete.");
        }
    }

    fn fail_provider_attempt(&self, message: &str) {
        self.provider_setup_session
            .write()
            .unwrap()
            .dispatch(ProviderSetupAction::AuthFailed);
        self.set_error(Some(message.to_string()));
        *self.sign_in_state.write().unwrap() = SignInState::PickMode;
        self.request_frame.schedule_frame();
    }

    fn handle_existing_chatgpt_login(&mut self) -> bool {
        if matches!(
            self.login_status,
            LoginStatus::AuthMode(auth_mode) if auth_mode.has_chatgpt_account()
        ) {
            *self.sign_in_state.write().unwrap() = SignInState::ChatGptSuccess;
            self.request_frame.schedule_frame();
            true
        } else {
            false
        }
    }

    /// Kicks off the ChatGPT auth flow and keeps the UI state consistent with the attempt.
    fn start_chatgpt_login(&mut self) {
        // If we're already authenticated with ChatGPT, don't start a new login –
        // just proceed to the success message flow.
        if self.handle_existing_chatgpt_login() {
            return;
        }

        self.set_error(/*message*/ None);
        let request_handle = self.app_server_request_handle.clone();
        let sign_in_state = self.sign_in_state.clone();
        let error = self.error.clone();
        let request_frame = self.request_frame.clone();
        tokio::spawn(async move {
            match request_handle
                .request_typed::<LoginAccountResponse>(ClientRequest::LoginAccount {
                    request_id: onboarding_request_id(),
                    params: LoginAccountParams::Chatgpt {
                        app_brand: None,
                        codex_streamlined_login: false,
                        use_hosted_login_success_page: false,
                    },
                })
                .await
            {
                Ok(LoginAccountResponse::Chatgpt { login_id, auth_url }) => {
                    maybe_open_auth_url_in_browser(&request_handle, &auth_url);
                    *error.write().unwrap() = None;
                    *sign_in_state.write().unwrap() =
                        SignInState::ChatGptContinueInBrowser(ContinueInBrowserState {
                            login_id,
                            auth_url,
                        });
                }
                Ok(other) => {
                    *sign_in_state.write().unwrap() = SignInState::PickMode;
                    *error.write().unwrap() = Some(format!(
                        "Unexpected account/login/start response: {other:?}"
                    ));
                }
                Err(err) => {
                    *sign_in_state.write().unwrap() = SignInState::PickMode;
                    *error.write().unwrap() = Some(err.to_string());
                }
            }
            request_frame.schedule_frame();
        });
    }

    fn start_device_code_login(&mut self) {
        if self.handle_existing_chatgpt_login() {
            return;
        }

        self.set_error(/*message*/ None);
        headless_chatgpt_login::start_headless_chatgpt_login(self);
    }

    pub(crate) fn on_account_login_completed(
        &mut self,
        notification: AccountLoginCompletedNotification,
    ) {
        let Some(login_id) = notification.login_id else {
            return;
        };
        let guard = self.sign_in_state.read().unwrap();
        let is_matching_login = matches!(
            &*guard,
            SignInState::ChatGptContinueInBrowser(state) if state.login_id == login_id
        ) || matches!(
            &*guard,
            SignInState::ChatGptDeviceCode(state) if state.login_id() == Some(login_id.as_str())
        );
        drop(guard);
        if !is_matching_login {
            return;
        }

        if notification.success {
            self.set_error(/*message*/ None);
            *self.sign_in_state.write().unwrap() = SignInState::ChatGptSuccessMessage;
        } else {
            self.set_error(notification.error);
            *self.sign_in_state.write().unwrap() = SignInState::PickMode;
        }
        self.request_frame.schedule_frame();
    }

    pub(crate) fn on_account_updated(&mut self, notification: AccountUpdatedNotification) {
        self.login_status = notification
            .auth_mode
            .map(|auth_mode| {
                LoginStatus::AuthMode(match auth_mode {
                    ApiAuthMode::ApiKey => AuthMode::ApiKey,
                    ApiAuthMode::Chatgpt => AuthMode::Chatgpt,
                    ApiAuthMode::ChatgptAuthTokens => AuthMode::ChatgptAuthTokens,
                    ApiAuthMode::Headers => AuthMode::Headers,
                    ApiAuthMode::AgentIdentity => AuthMode::AgentIdentity,
                    ApiAuthMode::PersonalAccessToken => AuthMode::PersonalAccessToken,
                    ApiAuthMode::BedrockApiKey => AuthMode::BedrockApiKey,
                })
            })
            .unwrap_or(LoginStatus::NotAuthenticated);
    }

    pub(crate) fn configured_provider(&self) -> Option<String> {
        self.provider_setup_session
            .read()
            .unwrap()
            .snapshot()
            .first_fresh_runtime
            .as_ref()
            .map(ToString::to_string)
    }

    pub(crate) fn deferred_provider_setup(&self) -> Option<DeferredProviderSetup> {
        match &self.provider_setup_session.read().unwrap().snapshot().phase {
            ProviderSetupPhase::Deferred(deferred) => Some(deferred.clone()),
            _ => None,
        }
    }
}

impl StepStateProvider for AuthModeWidget {
    fn get_step_state(&self) -> StepState {
        let sign_in_state = self.sign_in_state.read().unwrap();
        match &*sign_in_state {
            SignInState::PickMode
            | SignInState::ApiKeyEntry(_)
            | SignInState::ApiKeySaving
            | SignInState::ChatGptContinueInBrowser(_)
            | SignInState::ChatGptDeviceCode(_)
            | SignInState::ChatGptSuccessMessage
            | SignInState::ClaudeAuthMethodChoice { .. }
            | SignInState::ClaudeManagedTokenEntry(_)
            | SignInState::ClaudeManagedTokenSaving
            | SignInState::ClaudeCodeLoginChecking
            | SignInState::ClaudeCodeLoginPending { .. }
            | SignInState::ClaudeCodeLoginReady { .. }
            | SignInState::ClaudeCodeLoginCodeEntry { .. }
            | SignInState::ConfiguredProviderChoice { .. }
            | SignInState::AccountNameEntry { .. }
            | SignInState::AccountValueEntry { .. }
            | SignInState::AccountSaving
            | SignInState::AccountSaved { .. } => StepState::InProgress,
            SignInState::ChatGptSuccess
            | SignInState::ApiKeyConfigured { .. }
            | SignInState::ClaudeAccountConfigured { .. } => StepState::InProgress,
            SignInState::Complete => StepState::Complete,
        }
    }
}

impl WidgetRef for AuthModeWidget {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let sign_in_state = self.sign_in_state.read().unwrap();
        match &*sign_in_state {
            SignInState::PickMode => {
                self.render_pick_mode(area, buf);
            }
            SignInState::ChatGptContinueInBrowser(_) => {
                self.render_continue_in_browser(area, buf);
            }
            SignInState::ChatGptDeviceCode(state) => {
                headless_chatgpt_login::render_device_code_login(self, area, buf, state);
            }
            SignInState::ChatGptSuccessMessage => {
                self.render_chatgpt_success_message(area, buf);
            }
            SignInState::ChatGptSuccess => {
                self.render_chatgpt_success(area, buf);
            }
            SignInState::ApiKeyEntry(state) => {
                self.render_api_key_entry(area, buf, state);
            }
            SignInState::ApiKeySaving => {
                self.render_api_key_saving(area, buf);
            }
            SignInState::ApiKeyConfigured { .. } => {
                self.render_api_key_configured(area, buf);
            }
            SignInState::ClaudeAuthMethodChoice { highlighted } => {
                self.render_claude_method_choice(area, buf, *highlighted);
            }
            SignInState::ClaudeManagedTokenEntry(state) => {
                self.render_claude_secret_entry(
                    area,
                    buf,
                    &state.value,
                    /*authorization_code*/ false,
                );
            }
            SignInState::ClaudeManagedTokenSaving => {
                Paragraph::new("  Saving the Claude subscription token securely…")
                    .render(area, buf);
            }
            SignInState::ClaudeCodeLoginChecking => {
                Paragraph::new("  Checking the current Claude Code login…").render(area, buf);
            }
            SignInState::ClaudeCodeLoginPending { .. } => {
                self.render_claude_code_login(area, buf, /*verification_url*/ None);
            }
            SignInState::ClaudeCodeLoginReady {
                verification_url, ..
            } => {
                self.render_claude_code_login(area, buf, Some(verification_url));
            }
            SignInState::ClaudeCodeLoginCodeEntry { value, .. } => {
                self.render_claude_secret_entry(area, buf, value, /*authorization_code*/ true);
            }
            SignInState::ClaudeAccountConfigured { .. } => {
                Paragraph::new("✓ Anthropic Claude account configured".fg(Color::Green))
                    .render(area, buf);
            }
            SignInState::ConfiguredProviderChoice { .. }
            | SignInState::AccountNameEntry { .. }
            | SignInState::AccountValueEntry { .. }
            | SignInState::AccountSaving
            | SignInState::AccountSaved { .. } => {
                self.render_named_account_state(area, buf, &sign_in_state);
            }
            SignInState::Complete => {}
        }
    }
}

pub(super) fn maybe_open_auth_url_in_browser(request_handle: &AppServerRequestHandle, url: &str) {
    if !matches!(request_handle, AppServerRequestHandle::InProcess(_)) {
        return;
    }

    if let Err(err) = webbrowser::open(url) {
        tracing::warn!("failed to open browser for login URL: {err}");
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::legacy_core::config::ConfigBuilder;
    use codex_app_server_client::AppServerRequestHandle;
    use codex_app_server_client::DEFAULT_IN_PROCESS_CHANNEL_CAPACITY;
    use codex_app_server_client::InProcessAppServerClient;
    use codex_app_server_client::InProcessClientStartArgs;
    use codex_arg0::Arg0DispatchPaths;
    use codex_cloud_config::cloud_config_bundle_loader_for_storage;
    use codex_config::types::AuthCredentialsStoreMode;
    use codex_login::AuthKeyringBackendKind;

    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use tempfile::TempDir;

    const PRODUCTION_LENGTH_AUTH_URL: &str = concat!(
        "https://auth.openai.com/oauth/authorize?",
        "response_type=code&",
        "client_id=app_EMoamEEZ73f0CkXaXp7hrann&",
        "redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback&",
        "scope=openid%20profile%20email%20offline_access%20",
        "api.connectors.read%20api.connectors.invoke&",
        "code_challenge=1YM3Z8QbrLbdt9C3eX3j7UQ4GmFRmKz4OeVYwD6s5xA&",
        "code_challenge_method=S256&",
        "id_token_add_organizations=true&",
        "codex_cli_simplified_flow=true&",
        "state=8cHjQ4nVx2Yp7Lm9Rk3Wf6Ta1Bs5Du0Ei4Go7Nz2PqM&",
        "originator=codex_cli_rs"
    );

    pub(crate) async fn widget_forced_chatgpt() -> (AuthModeWidget, TempDir) {
        let codex_home = TempDir::new().unwrap();
        let codex_home_path = codex_home.path().to_path_buf();
        let config = ConfigBuilder::default()
            .codex_home(codex_home_path.clone())
            .build()
            .await
            .unwrap();
        let provider_status_host = ProviderStatusHost::from_config(
            &config,
            crate::provider_status_host::ProviderAccountMetadata::default(),
        );
        let provider_statuses = provider_status_host.resolve();
        let provider_setup_session = Arc::new(RwLock::new(ProviderSetupSession::from_statuses(
            provider_statuses.entries(),
        )));
        let (provider_auth_action_tx, _provider_auth_action_rx) =
            tokio::sync::mpsc::unbounded_channel();
        let auth_route_config = config.auth_route_config();
        let client = InProcessAppServerClient::start(InProcessClientStartArgs {
            arg0_paths: Arg0DispatchPaths::default(),
            config: Arc::new(config.clone()),
            cli_overrides: Vec::new(),
            loader_overrides: Default::default(),
            strict_config: false,
            cloud_config_bundle: cloud_config_bundle_loader_for_storage(
                codex_home_path.clone(),
                /*enable_codex_api_key_env*/ false,
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default(),
                "https://chatgpt.com/backend-api/".to_string(),
                auth_route_config,
            )
            .await,
            feedback: codex_feedback::CodexFeedback::new(),
            log_db: None,
            state_db: None,
            environment_manager: Arc::new(
                codex_app_server_client::EnvironmentManager::default_for_tests(),
            ),
            config_warnings: Vec::new(),
            session_source: serde_json::from_value(serde_json::json!("cli"))
                .expect("cli session source should deserialize"),
            enable_codex_api_key_env: false,
            client_name: "test".to_string(),
            client_version: "test".to_string(),
            experimental_api: true,
            mcp_server_openai_form_elicitation: false,
            opt_out_notification_methods: Vec::new(),
            channel_capacity: DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
        })
        .await
        .unwrap();
        let (claude_event_tx, _claude_event_rx) = tokio::sync::mpsc::unbounded_channel();
        let widget = AuthModeWidget {
            request_frame: FrameRequester::test_dummy(),
            highlighted_mode: SignInOption::ChatGpt,
            error: Arc::new(RwLock::new(None)),
            sign_in_state: Arc::new(RwLock::new(SignInState::PickMode)),
            login_status: LoginStatus::NotAuthenticated,
            replace_provider_with_openai_on_login: true,
            app_server_request_handle: AppServerRequestHandle::InProcess(client.request_handle()),
            forced_login_method: Some(ForcedLoginMethod::Chatgpt),
            api_key_provider_id: "openai".to_string(),
            api_key_provider_name: "OpenAI".to_string(),
            api_key_env_var: None,
            api_key_provider_options: Vec::new(),
            selected_api_key_option: None,
            animations_enabled: true,
            animations_suppressed: std::cell::Cell::new(false),
            codex_home: codex_home_path,
            claude_event_tx: AppEventSender::new(claude_event_tx),
            provider_setup_session,
            provider_status_host,
            provider_statuses: Arc::new(RwLock::new(provider_statuses)),
            provider_auth_action_tx,
            named_account_methods: std::collections::BTreeMap::new(),
        };
        (widget, codex_home)
    }

    pub(super) fn buffer_text(buf: &Buffer, area: Rect) -> String {
        let mut out = String::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn api_key_option(id: &str, name: &str, env_key: &str) -> ApiKeyProviderOption {
        let catalog = codex_provider_auth::ProviderCatalog::from_runtime_providers(
            &std::collections::HashMap::from([(
                id.to_string(),
                codex_model_provider_info::ModelProviderInfo {
                    name: name.to_string(),
                    env_key: Some(env_key.to_string()),
                    ..Default::default()
                },
            )]),
        );
        let entry = catalog.entries().first().expect("catalog entry");
        let capability = entry
            .setup_capabilities
            .iter()
            .find(|capability| matches!(capability, ProviderSetupCapability::ApiKey { .. }))
            .expect("API-key capability")
            .clone();
        ApiKeyProviderOption {
            target: ApiKeyAuthTarget::from_catalog_capability(entry, &capability)
                .expect("API-key target"),
            capability,
            name: name.to_string(),
        }
    }

    #[tokio::test]
    async fn provider_key_picker_shows_codex_account_and_provider_keys() {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;
        widget.forced_login_method = None;
        widget.highlighted_mode = SignInOption::ProviderApiKey(0);
        widget.api_key_provider_options = vec![
            api_key_option("openai", "OpenAI", "OPENAI_API_KEY"),
            api_key_option("ambient", "Ambient", "AMBIENT_API_KEY"),
            api_key_option("zai", "Z.AI", "ZAI_API_KEY"),
            api_key_option("openrouter", "OpenRouter", "OPENROUTER_API_KEY"),
            api_key_option("baseten", "Baseten", "BASETEN_API_KEY"),
            api_key_option("vercel", "Vercel", "AI_GATEWAY_API_KEY"),
        ];

        assert_eq!(
            widget.displayed_sign_in_options(),
            vec![
                SignInOption::DeviceCode,
                SignInOption::ProviderApiKey(0),
                SignInOption::AnthropicAccount,
                SignInOption::CorbanuPlan,
                SignInOption::ProviderApiKey(1),
                SignInOption::ProviderApiKey(2),
                SignInOption::ProviderApiKey(3),
                SignInOption::ProviderApiKey(4),
                SignInOption::ProviderApiKey(5),
                SignInOption::ProviderRuntime(12),
                SignInOption::ProviderRuntime(13),
                SignInOption::ProviderRuntime(14),
                SignInOption::Done,
            ]
        );

        let area = Rect::new(0, 0, 80, 32);
        let mut buf = Buffer::empty(area);
        widget.render_pick_mode(area, &mut buf);
        let rendered = buffer_text(&buf, area);
        let visible = rendered
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n");
        insta::assert_snapshot!("multi_provider_picker", visible);

        assert!(
            rendered.contains("Provider: OpenAI Codex Account"),
            "rendered:\n{rendered}"
        );
        assert!(
            rendered.contains("Provider: OpenAI API Key"),
            "rendered:\n{rendered}"
        );
        assert!(
            rendered.contains("Provider: Anthropic Claude Account"),
            "rendered:\n{rendered}"
        );
        assert!(
            rendered.contains("Provider: Ambient API Key"),
            "rendered:\n{rendered}"
        );
        assert!(
            rendered.contains("Provider: Z.AI API Key"),
            "rendered:\n{rendered}"
        );
        assert!(
            rendered.contains("Provider: OpenRouter API Key"),
            "rendered:\n{rendered}"
        );
        assert!(
            rendered.contains("Provider: Baseten API Key"),
            "rendered:\n{rendered}"
        );
        assert!(
            rendered.contains("Provider: Vercel API Key"),
            "rendered:\n{rendered}"
        );
        assert!(!rendered.contains("ChatGPT"), "rendered:\n{rendered}");
        assert!(rendered.contains("device code"), "rendered:\n{rendered}");
    }

    #[tokio::test]
    async fn provider_key_picker_scrolls_to_the_highlighted_provider_in_a_short_terminal() {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;
        widget.forced_login_method = None;
        widget.api_key_provider_options = vec![
            api_key_option("anthropic", "Anthropic", "ANTHROPIC_API_KEY"),
            api_key_option("ambient", "Ambient", "AMBIENT_API_KEY"),
            api_key_option("kimi-code", "Kimi Code", "KIMI_API_KEY"),
            api_key_option("zai", "Z.AI", "ZAI_API_KEY"),
            api_key_option("openrouter", "OpenRouter", "OPENROUTER_API_KEY"),
            api_key_option("meta", "Meta", "MODEL_API_KEY"),
            api_key_option("baseten", "Baseten", "BASETEN_API_KEY"),
            api_key_option("vercel", "Vercel", "AI_GATEWAY_API_KEY"),
            api_key_option("deepseek", "DeepSeek", "DEEPSEEK_API_KEY"),
        ];
        widget.highlighted_mode = SignInOption::ProviderApiKey(8);

        let area = Rect::new(0, 0, 80, 14);
        let mut buf = Buffer::empty(area);
        widget.render_pick_mode(area, &mut buf);
        let rendered = buffer_text(&buf, area);

        assert!(
            rendered.contains("Provider: DeepSeek API Key"),
            "highlighted provider must stay visible:\n{rendered}"
        );
        assert!(
            rendered.contains("Use ↑/↓ to choose"),
            "navigation instructions must remain visible:\n{rendered}"
        );
    }

    /// #1288: when the providers do not fit, the footer says which row is
    /// selected out of how many, and follows the selection.
    #[tokio::test]
    async fn provider_picker_shows_position_when_rows_exceed_the_window() {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;
        widget.forced_login_method = None;
        widget.highlighted_mode = SignInOption::ProviderApiKey(0);
        widget.api_key_provider_options = vec![
            api_key_option("openai", "OpenAI", "OPENAI_API_KEY"),
            api_key_option("ambient", "Ambient", "AMBIENT_API_KEY"),
            api_key_option("zai", "Z.AI", "ZAI_API_KEY"),
            api_key_option("openrouter", "OpenRouter", "OPENROUTER_API_KEY"),
            api_key_option("baseten", "Baseten", "BASETEN_API_KEY"),
            api_key_option("vercel", "Vercel", "AI_GATEWAY_API_KEY"),
        ];
        let total = widget.displayed_sign_in_options().len();
        let render = |widget: &AuthModeWidget, height: u16| {
            let area = Rect::new(0, 0, 80, height);
            let mut buf = Buffer::empty(area);
            widget.render_pick_mode(area, &mut buf);
            buffer_text(&buf, area)
        };

        let rendered = render(&widget, /*height*/ 32);
        assert!(
            rendered.contains(&format!(
                "Use ↑/↓ to choose · Press enter to continue · 2 / {total}"
            )),
            "rendered:\n{rendered}"
        );

        widget.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let rendered = render(&widget, /*height*/ 32);
        assert!(
            rendered.contains(&format!("· 3 / {total}")),
            "rendered:\n{rendered}"
        );

        let rendered = render(&widget, /*height*/ 60);
        assert!(
            rendered.contains("Provider: Amazon Bedrock") && rendered.contains("Done"),
            "every row fits:\n{rendered}"
        );
        assert!(
            !rendered.contains(&format!(" / {total}")),
            "no position when every row fits:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn forced_api_policy_rejects_device_code_selection() {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;
        widget.forced_login_method = Some(ForcedLoginMethod::Api);
        widget.highlighted_mode = SignInOption::DeviceCode;
        widget.api_key_provider_options = vec![api_key_option("zai", "Z.AI", "ZAI_API_KEY")];

        widget.handle_sign_in_option(SignInOption::DeviceCode);

        assert!(matches!(
            &*widget.sign_in_state.read().unwrap(),
            SignInState::PickMode
        ));
    }

    #[tokio::test]
    async fn api_key_entry_masks_entered_key_when_rendered() {
        let (widget, _tmp) = widget_forced_chatgpt().await;
        let area = Rect::new(0, 0, 80, 12);
        let mut buf = Buffer::empty(area);
        let state = ApiKeyInputState {
            value: "sk-visible-secret".to_string(),
            prepopulated_env_var: None,
        };

        widget.render_api_key_entry(area, &mut buf, &state);
        let rendered = buffer_text(&buf, area);

        assert!(
            !rendered.contains("sk-visible-secret"),
            "rendered:\n{rendered}"
        );
        assert!(
            rendered.contains("•••••••••••••••••"),
            "rendered:\n{rendered}"
        );
        assert!(
            rendered.contains("configured OpenAI credential store and never adds it to chat"),
            "rendered:\n{rendered}"
        );
    }

    #[tokio::test]
    async fn api_key_flow_disabled_when_chatgpt_forced() {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;

        widget.start_api_key_entry();

        assert_eq!(
            widget.error_message().as_deref(),
            Some(API_KEY_DISABLED_MESSAGE)
        );
        assert!(matches!(
            &*widget.sign_in_state.read().unwrap(),
            SignInState::PickMode
        ));
    }

    #[tokio::test]
    async fn saving_api_key_is_blocked_when_chatgpt_forced() {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;

        widget.save_api_key("sk-test".to_string());

        assert_eq!(
            widget.error_message().as_deref(),
            Some(API_KEY_DISABLED_MESSAGE)
        );
        assert!(matches!(
            &*widget.sign_in_state.read().unwrap(),
            SignInState::PickMode
        ));
        assert_eq!(widget.login_status, LoginStatus::NotAuthenticated);
    }

    /// #462: with the picker off (config names an API-key provider), the
    /// "Provide your ... API key" row begins setup for that provider, so the
    /// saved key's completion is not ignored.
    #[tokio::test]
    async fn api_key_row_begins_setup_for_the_selected_provider() {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;
        widget.forced_login_method = Some(ForcedLoginMethod::Api);
        let option = api_key_option("zai", "Z.AI", "ZAI_API_KEY");
        widget.api_key_provider_options = vec![option.clone()];
        widget.selected_api_key_option = Some(option);
        assert!(!widget.provider_picker_enabled());

        widget.handle_sign_in_option(SignInOption::ApiKey);

        let entry = widget
            .provider_status_host
            .catalog()
            .get("zai")
            .expect("Z.AI catalog entry");
        assert_eq!(
            widget
                .provider_setup_session
                .read()
                .unwrap()
                .snapshot()
                .phase,
            crate::onboarding::provider_setup::ProviderSetupPhase::Authenticating {
                provider_id: entry.id.clone(),
            }
        );
        assert!(matches!(
            &*widget.sign_in_state.read().unwrap(),
            SignInState::ApiKeyEntry(_)
        ));
    }

    /// #1571: `forced_login_method = "api"` with `model_provider =
    /// "pfterminal-plan"`. No pasted-key option names the Plan, so onboarding
    /// must not open the generic key prompt (OpenAI wording, and saving fails
    /// with "Provider API-key selection is no longer available"). It offers the
    /// Plan's Corbanu API wallet row, which is API-key setup, and Done.
    #[tokio::test]
    async fn issue1571_forced_api_with_corbanu_plan_offers_the_wallet_row() {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;
        // As OnboardingScreen::new builds the widget for that config.
        widget.forced_login_method = Some(ForcedLoginMethod::Api);
        widget.api_key_provider_id =
            codex_model_provider_info::PFTERMINAL_PLAN_PROVIDER_ID.to_string();
        widget.api_key_provider_name = "Corbanu Plan".to_string();
        widget.api_key_env_var =
            Some(codex_model_provider_info::PFTERMINAL_PLAN_API_KEY_ENV_VAR.to_string());
        widget.api_key_provider_options = vec![api_key_option("zai", "Z.AI", "ZAI_API_KEY")];
        widget.selected_api_key_option = None;
        widget.highlighted_mode = SignInOption::ApiKey;
        *widget.sign_in_state.write().unwrap() =
            SignInState::ApiKeyEntry(ApiKeyInputState::from_env(/*provider_env_key*/ None));

        widget.begin_initial_api_key_entry();

        let area = Rect::new(0, 0, 88, 12);
        let mut buf = Buffer::empty(area);
        widget.render_ref(area, &mut buf);
        let visible = buffer_text(&buf, area)
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!visible.contains("OpenAI"), "rendered:\n{visible}");
        insta::assert_snapshot!(visible, @r###"
  Connect a Corbanu Plan API key to use Corbanu Terminal

> 1. Corbanu API
     Queue protected wallet and API-key setup; it runs after Done

  2. Done
     Configure a usable provider or queue Corbanu API first




  Use ↑/↓ to choose · Press enter to continue
"###);
        assert_eq!(
            widget.displayed_sign_in_options(),
            vec![SignInOption::CorbanuPlan, SignInOption::Done]
        );
        assert_eq!(
            widget.selectable_sign_in_options(),
            vec![SignInOption::CorbanuPlan, SignInOption::Done]
        );

        // The row queues the wallet setup and Done finishes onboarding with it.
        widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            widget
                .provider_setup_session
                .read()
                .unwrap()
                .snapshot()
                .queued_corbanu
        );
        widget.handle_sign_in_option(SignInOption::Done);
        assert_eq!(widget.get_step_state(), StepState::Complete);
        assert!(widget.deferred_provider_setup().is_some());
        assert_eq!(widget.error_message(), None);
    }

    /// #1571 guard: forced API-key sign-in for a provider that has a pasted-key
    /// option still opens that provider's key prompt.
    #[tokio::test]
    async fn issue1571_forced_api_with_a_key_provider_keeps_the_key_prompt() {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;
        widget.forced_login_method = Some(ForcedLoginMethod::Api);
        let option = api_key_option("zai", "Z.AI", "ZAI_API_KEY");
        widget.api_key_provider_id = "zai".to_string();
        widget.api_key_provider_name = "Z.AI".to_string();
        widget.api_key_env_var = Some("ZAI_API_KEY".to_string());
        widget.api_key_provider_options = vec![option.clone()];
        widget.selected_api_key_option = Some(option);
        widget.highlighted_mode = SignInOption::ApiKey;
        *widget.sign_in_state.write().unwrap() =
            SignInState::ApiKeyEntry(ApiKeyInputState::from_env(/*provider_env_key*/ None));

        widget.begin_initial_api_key_entry();

        assert!(matches!(
            &*widget.sign_in_state.read().unwrap(),
            SignInState::ApiKeyEntry(_)
        ));
        assert_eq!(
            widget.displayed_sign_in_options(),
            vec![SignInOption::ApiKey]
        );
    }

    #[tokio::test]
    async fn provider_api_key_save_is_single_flight() {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;
        widget.forced_login_method = None;
        widget.api_key_provider_options =
            vec![api_key_option("deepseek", "DeepSeek", "DEEPSEEK_API_KEY")];
        assert!(widget.select_provider_api_key_option(/*index*/ 0));
        *widget.sign_in_state.write().unwrap() =
            SignInState::ApiKeyEntry(ApiKeyInputState::default());

        widget.save_api_key("first-key".to_string());
        widget.save_api_key("second-key".to_string());

        assert!(matches!(
            &*widget.sign_in_state.read().unwrap(),
            SignInState::ApiKeySaving
        ));
    }

    #[tokio::test]
    async fn configured_provider_reads_first_usable_runtime_from_shared_session() {
        let (widget, _tmp) = widget_forced_chatgpt().await;
        let entry = widget
            .provider_status_host
            .catalog()
            .get(codex_model_provider_info::OPENROUTER_PROVIDER_ID)
            .expect("OpenRouter catalog entry");
        let provider_id = entry.id.clone();
        let runtime_provider_id = entry
            .runtime_provider_ids
            .first()
            .expect("OpenRouter runtime")
            .clone();
        let mut session = widget.provider_setup_session.write().unwrap();
        session.dispatch(ProviderSetupAction::Begin {
            provider_id: provider_id.clone(),
        });
        session.dispatch(ProviderSetupAction::AuthConfigured {
            provider_id: provider_id.clone(),
            runtime_provider_id: runtime_provider_id.clone(),
        });
        session.dispatch(ProviderSetupAction::ActivationResolved {
            provider_id,
            runtime_provider_id,
            usable: true,
        });
        drop(session);

        assert_eq!(
            widget.configured_provider().as_deref(),
            Some(codex_model_provider_info::OPENROUTER_PROVIDER_ID)
        );
    }

    #[tokio::test]
    async fn explicit_noninteractive_provider_option_records_the_replacement() {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;
        widget.forced_login_method = None;
        let option = catalog_sign_in_options(
            &widget.provider_status_host,
            &widget.api_key_provider_options,
        )
        .into_iter()
        .find(|option| {
            let SignInOption::ProviderRuntime(index) = option else {
                return false;
            };
            widget
                .provider_status_host
                .catalog()
                .entries()
                .get(*index)
                .is_some_and(|entry| {
                    entry.runtime_provider_ids.iter().any(|runtime| {
                        runtime.as_str() == codex_model_provider_info::OLLAMA_OSS_PROVIDER_ID
                    })
                })
        })
        .expect("Ollama explicit-selection option");

        widget.handle_sign_in_option(option);

        assert_eq!(
            widget.configured_provider().as_deref(),
            Some(codex_model_provider_info::OLLAMA_OSS_PROVIDER_ID)
        );
        assert!(widget.provider_setup_session.read().unwrap().can_finish());
    }

    /// Ollama and LM Studio are never probed, so their onboarding rows do not
    /// claim "ready".
    #[tokio::test]
    async fn local_runtime_rows_say_not_checked() {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;
        widget.forced_login_method = None;
        let option = catalog_sign_in_options(
            &widget.provider_status_host,
            &widget.api_key_provider_options,
        )
        .into_iter()
        .find(|option| {
            let SignInOption::ProviderRuntime(index) = option else {
                return false;
            };
            widget
                .provider_status_host
                .catalog()
                .entries()
                .get(*index)
                .is_some_and(|entry| {
                    entry.runtime_provider_ids.iter().any(|runtime| {
                        runtime.as_str() == codex_model_provider_info::OLLAMA_OSS_PROVIDER_ID
                    })
                })
        })
        .expect("Ollama explicit-selection option");
        widget.highlighted_mode = option;

        let area = Rect::new(0, 0, 80, 32);
        let mut buf = Buffer::empty(area);
        widget.render_pick_mode(area, &mut buf);
        let visible = buffer_text(&buf, area)
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n");
        insta::assert_snapshot!("local_runtime_rows_not_checked", visible);

        // Selected, it is active, still not checked.
        widget.handle_sign_in_option(option);
        let mut buf = Buffer::empty(area);
        widget.render_pick_mode(area, &mut buf);
        let rendered = buffer_text(&buf, area);
        assert!(
            rendered.contains("Local (not checked) · active"),
            "rendered:\n{rendered}"
        );
        assert!(!rendered.contains("ready"), "rendered:\n{rendered}");
    }

    #[tokio::test]
    async fn configured_explicit_openai_account_reads_provider_success_state() {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;
        widget.replace_provider_with_openai_on_login = true;
        widget.begin_provider(codex_model_provider_info::OPENAI_PROVIDER_ID);
        *widget.sign_in_state.write().unwrap() = SignInState::ChatGptSuccess;
        widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(
            widget.configured_provider().as_deref(),
            Some(codex_model_provider_info::OPENAI_PROVIDER_ID)
        );
    }

    #[tokio::test]
    async fn configured_openai_api_key_reads_provider_success_state() {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;
        widget.begin_provider(codex_model_provider_info::OPENAI_PROVIDER_ID);
        widget.provider_status_host.mark_openai_api_key();
        *widget.sign_in_state.write().unwrap() = SignInState::ApiKeyConfigured { provider: None };
        widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(
            widget.configured_provider().as_deref(),
            Some(codex_model_provider_info::OPENAI_PROVIDER_ID)
        );
    }

    #[tokio::test]
    async fn anthropic_account_opens_recommended_method_choice_and_cancel_is_inert() {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;
        widget.forced_login_method = None;
        widget.api_key_provider_options = vec![api_key_option(
            codex_model_provider_info::ANTHROPIC_PROVIDER_ID,
            "Anthropic",
            codex_model_provider_info::ANTHROPIC_API_KEY_ENV_VAR,
        )];

        widget.handle_sign_in_option(SignInOption::AnthropicAccount);

        assert!(matches!(
            &*widget.sign_in_state.read().unwrap(),
            SignInState::ClaudeAuthMethodChoice {
                highlighted: ClaudeAuthMethod::ManagedToken
            }
        ));
        assert_eq!(widget.configured_provider(), None);

        widget.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(matches!(
            &*widget.sign_in_state.read().unwrap(),
            SignInState::PickMode
        ));
        assert_eq!(widget.configured_provider(), None);
    }

    #[tokio::test]
    async fn forced_chatgpt_policy_rejects_anthropic_account_dispatch() {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;
        widget.api_key_provider_options = vec![api_key_option(
            codex_model_provider_info::ANTHROPIC_PROVIDER_ID,
            "Anthropic",
            codex_model_provider_info::ANTHROPIC_API_KEY_ENV_VAR,
        )];

        widget.handle_sign_in_option(SignInOption::AnthropicAccount);

        assert!(matches!(
            &*widget.sign_in_state.read().unwrap(),
            SignInState::PickMode
        ));
        assert_eq!(widget.configured_provider(), None);
    }

    #[tokio::test]
    async fn anthropic_account_method_choice_snapshot() {
        let (widget, _tmp) = widget_forced_chatgpt().await;
        let area = Rect::new(0, 0, 88, 16);
        let mut buf = Buffer::empty(area);

        widget.render_claude_method_choice(area, &mut buf, ClaudeAuthMethod::ManagedToken);

        let visible = buffer_text(&buf, area)
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n");
        insta::assert_snapshot!(visible, @r###"
Claude Plan authentication
Choose one source; Corbanu never falls back to another account.

> Long-lived subscription token (Recommended)
    Run `claude setup-token` in a private terminal, then paste its approximately
one-year token here (Pro, Max, Team, or Enterprise).

  Claude Code login
    Use Claude Code's rotating login state; reauthorization may be needed more often.

Your account and billing path change only after success. Esc keeps the current method.
  Use ↑/↓ to choose · Press enter to continue · Press esc to go back
"###);
    }

    #[tokio::test]
    async fn claude_auth_success_returns_plan_provider_only_after_success() {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;
        widget.begin_provider(codex_model_provider_info::CLAUDE_PLAN_PROVIDER_ID);
        *widget.sign_in_state.write().unwrap() = SignInState::ClaudeManagedTokenSaving;
        assert_eq!(widget.configured_provider(), None);

        widget.handle_claude_event(AppEvent::ClaudeManagedSubscriptionTokenSaved {
            result: Ok("saved".to_string()),
        });
        widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(
            widget.configured_provider().as_deref(),
            Some(codex_model_provider_info::CLAUDE_PLAN_PROVIDER_ID)
        );
        assert_eq!(widget.get_step_state(), StepState::Complete);
    }

    #[tokio::test]
    async fn stale_claude_login_completion_cannot_replace_a_cancelled_state() {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;
        *widget.sign_in_state.write().unwrap() = SignInState::PickMode;

        widget.handle_claude_event(AppEvent::ClaudeCodePlanLoginFinished {
            result: Some(Ok("selected".to_string())),
        });

        assert!(matches!(
            &*widget.sign_in_state.read().unwrap(),
            SignInState::PickMode
        ));
        assert_eq!(widget.configured_provider(), None);
    }

    #[tokio::test]
    async fn existing_chatgpt_login_shortcut_does_not_request_provider_persistence() {
        for auth_mode in [
            AuthMode::Chatgpt,
            AuthMode::ChatgptAuthTokens,
            AuthMode::PersonalAccessToken,
        ] {
            let (mut widget, _tmp) = widget_forced_chatgpt().await;
            widget.login_status = LoginStatus::AuthMode(auth_mode);

            let handled = widget.handle_existing_chatgpt_login();

            assert_eq!(handled, true);
            assert!(matches!(
                &*widget.sign_in_state.read().unwrap(),
                SignInState::ChatGptSuccess
            ));
            assert_eq!(widget.configured_provider(), None);
        }
    }

    #[tokio::test]
    async fn cancel_active_attempt_resets_browser_login_state() {
        let (widget, _tmp) = widget_forced_chatgpt().await;
        *widget.error.write().unwrap() = Some("still logging in".to_string());
        *widget.sign_in_state.write().unwrap() =
            SignInState::ChatGptContinueInBrowser(ContinueInBrowserState {
                login_id: "login-1".to_string(),
                auth_url: "https://auth.example.com".to_string(),
            });

        widget.cancel_active_attempt();

        assert_eq!(widget.error_message(), None);
        assert!(matches!(
            &*widget.sign_in_state.read().unwrap(),
            SignInState::PickMode
        ));
    }

    #[tokio::test]
    async fn cancel_active_attempt_notifies_device_code_login() {
        let (widget, _tmp) = widget_forced_chatgpt().await;
        *widget.error.write().unwrap() = Some("still logging in".to_string());
        *widget.sign_in_state.write().unwrap() =
            SignInState::ChatGptDeviceCode(ContinueWithDeviceCodeState::ready(
                "request-1".to_string(),
                "login-1".to_string(),
                "https://chatgpt.com/device".to_string(),
                "ABCD-EFGH".to_string(),
            ));

        widget.cancel_active_attempt();

        assert_eq!(widget.error_message(), None);
        assert!(matches!(
            &*widget.sign_in_state.read().unwrap(),
            SignInState::PickMode
        ));
    }

    /// Collects all buffer cell symbols that contain the OSC 8 open sequence
    /// for the given URL.  Returns the concatenated "inner" characters.
    fn collect_osc8_chars(buf: &Buffer, area: Rect, url: &str) -> String {
        let open = format!("\x1B]8;;{url}\x07");
        let close = "\x1B]8;;\x07";
        let mut chars = String::new();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let sym = buf[(x, y)].symbol();
                if let Some(rest) = sym.strip_prefix(open.as_str())
                    && let Some(ch) = rest.strip_suffix(close)
                {
                    chars.push_str(ch);
                }
            }
        }
        chars
    }

    #[test]
    fn continue_in_browser_preserves_long_link_and_footer_at_narrow_width() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (widget, _tmp) = runtime.block_on(widget_forced_chatgpt());
        widget.set_animations_suppressed(/*suppressed*/ true);
        *widget.sign_in_state.write().unwrap() =
            SignInState::ChatGptContinueInBrowser(ContinueInBrowserState {
                login_id: "login-1".to_string(),
                auth_url: PRODUCTION_LENGTH_AUTH_URL.to_string(),
            });

        let width = 44;
        let height = 30;
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        widget.render_continue_in_browser(area, &mut buf);

        let found = collect_osc8_chars(&buf, area, PRODUCTION_LENGTH_AUTH_URL);
        assert_eq!(
            found, PRODUCTION_LENGTH_AUTH_URL,
            "OSC 8 hyperlink should cover the full URL"
        );

        let mut terminal = crate::custom_terminal::Terminal::with_options(
            crate::test_backend::VT100Backend::new(width, height),
        )
        .expect("terminal");
        terminal.set_viewport_area(area);

        terminal
            .draw(|frame| widget.render_continue_in_browser(area, frame.buffer_mut()))
            .expect("draw");

        let contents = terminal.backend().to_string();
        insta::assert_snapshot!("continue_in_browser_narrow_long_url", contents);
        assert!(contents.contains("On a remote or headless machine?"));
        assert!(contents.contains("Press esc to cancel"));
    }

    #[test]
    fn chatgpt_success_message_renders_osc8_hyperlinks() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (widget, _tmp) = runtime.block_on(widget_forced_chatgpt());
        let area = Rect::new(0, 0, 80, 14);
        let mut buf = Buffer::empty(area);

        widget.render_chatgpt_success_message(area, &mut buf);

        assert_eq!(
            collect_osc8_chars(&buf, area, "https://developers.openai.com/codex/security"),
            "Codex docs"
        );
        assert_eq!(
            collect_osc8_chars(&buf, area, "https://chatgpt.com/#settings"),
            "training data preferences"
        );
        assert_eq!(
            (0..37).map(|x| buf[(x, 5)].modifier).collect::<Vec<_>>(),
            [
                vec![Modifier::DIM; 27],
                vec![Modifier::DIM | Modifier::UNDERLINED; 10],
            ]
            .concat()
        );
        assert_eq!(
            (0..60).map(|x| buf[(x, 11)].modifier).collect::<Vec<_>>(),
            [
                vec![Modifier::DIM; 35],
                vec![Modifier::DIM | Modifier::UNDERLINED; 25],
            ]
            .concat()
        );

        let visible = (area.top()..area.bottom())
            .map(|y| {
                let row = (area.left()..area.right())
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>();
                crate::terminal_hyperlinks::strip_osc8(&row)
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        insta::assert_snapshot!(visible, @r###"
        ✓ Signed in with your ChatGPT account

          Before you start:

          Decide how much autonomy you want to grant Codex
          For more details see the Codex docs

          Codex can make mistakes
          Review the code it writes and commands it runs

          Powered by your ChatGPT account
          Uses your plan's rate limits and training data preferences

          Press enter to continue
        "###);
    }

    #[test]
    fn auth_widget_suppresses_animations_when_device_code_is_visible() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (widget, _tmp) = runtime.block_on(widget_forced_chatgpt());
        *widget.sign_in_state.write().unwrap() =
            SignInState::ChatGptDeviceCode(ContinueWithDeviceCodeState::ready(
                "request-1".to_string(),
                "login-1".to_string(),
                "https://chatgpt.com/device".to_string(),
                "ABCD-EFGH".to_string(),
            ));

        assert_eq!(widget.should_suppress_animations(), true);
    }

    #[tokio::test]
    async fn device_code_guidance_keeps_numbered_steps_and_cancel_visible() {
        let (widget, _tmp) = widget_forced_chatgpt().await;
        widget.set_animations_suppressed(/*suppressed*/ true);
        let state = ContinueWithDeviceCodeState::ready(
            "request-1".to_string(),
            "login-1".to_string(),
            "https://example.com/device".to_string(),
            "ABCD-EFGH".to_string(),
        );
        let area = Rect::new(0, 0, 76, 22);
        let mut terminal = crate::custom_terminal::Terminal::with_options(
            crate::test_backend::VT100Backend::new(area.width, area.height),
        )
        .expect("terminal");
        terminal.set_viewport_area(area);
        terminal
            .draw(|frame| {
                headless_chatgpt_login::render_device_code_login(
                    &widget,
                    area,
                    frame.buffer_mut(),
                    &state,
                );
            })
            .expect("draw");
        let contents = terminal.backend().to_string();
        assert!(contents.contains("1. On a remote or headless machine"));
        assert!(contents.contains("2. Enter this one-time code"));
        assert!(contents.contains("Press esc to cancel"));
        insta::assert_snapshot!("device_code_numbered_guidance", contents);
    }

    #[test]
    fn auth_widget_suppresses_animations_while_requesting_device_code() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (widget, _tmp) = runtime.block_on(widget_forced_chatgpt());
        *widget.sign_in_state.write().unwrap() = SignInState::ChatGptDeviceCode(
            ContinueWithDeviceCodeState::pending("request-1".to_string()),
        );

        assert_eq!(widget.should_suppress_animations(), true);
    }

    #[tokio::test]
    async fn device_code_login_completion_advances_to_success_message() {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;
        widget.begin_provider(codex_model_provider_info::OPENAI_PROVIDER_ID);
        *widget.sign_in_state.write().unwrap() =
            SignInState::ChatGptDeviceCode(ContinueWithDeviceCodeState::ready(
                "request-1".to_string(),
                "login-1".to_string(),
                "https://chatgpt.com/device".to_string(),
                "ABCD-EFGH".to_string(),
            ));

        widget.on_account_login_completed(AccountLoginCompletedNotification {
            login_id: Some("login-1".to_string()),
            success: true,
            error: None,
        });

        assert!(matches!(
            &*widget.sign_in_state.read().unwrap(),
            SignInState::ChatGptSuccessMessage
        ));
        widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            widget.configured_provider().as_deref(),
            Some(codex_model_provider_info::OPENAI_PROVIDER_ID)
        );
    }

    #[tokio::test]
    async fn custom_openai_auth_provider_is_not_replaced_after_login() {
        let (mut widget, _tmp) = widget_forced_chatgpt().await;
        widget.replace_provider_with_openai_on_login = false;
        *widget.sign_in_state.write().unwrap() =
            SignInState::ChatGptDeviceCode(ContinueWithDeviceCodeState::ready(
                "request-1".to_string(),
                "login-1".to_string(),
                "https://chatgpt.com/device".to_string(),
                "ABCD-EFGH".to_string(),
            ));

        widget.on_account_login_completed(AccountLoginCompletedNotification {
            login_id: Some("login-1".to_string()),
            success: true,
            error: None,
        });

        widget.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            &*widget.sign_in_state.read().unwrap(),
            SignInState::Complete
        ));
        assert_eq!(widget.configured_provider(), None);
    }

    #[test]
    fn mark_url_hyperlink_wraps_cyan_underlined_cells() {
        let url = "https://example.com";
        let area = Rect::new(0, 0, 20, 1);
        let mut buf = Buffer::empty(area);

        // Manually write some cyan+underlined characters to simulate a rendered URL.
        for (i, ch) in "example".chars().enumerate() {
            let cell = &mut buf[(i as u16, 0)];
            cell.set_symbol(&ch.to_string());
            cell.fg = Color::Cyan;
            cell.modifier = Modifier::UNDERLINED;
        }
        // Leave a plain cell that should NOT be marked.
        buf[(7, 0)].set_symbol("X");

        mark_url_hyperlink(&mut buf, area, url);

        // Each cyan+underlined cell should now carry the OSC 8 wrapper.
        let found = collect_osc8_chars(&buf, area, url);
        assert_eq!(found, "example");

        // The plain "X" cell should be untouched.
        assert_eq!(buf[(7, 0)].symbol(), "X");
    }

    #[test]
    fn mark_url_hyperlink_sanitizes_control_chars() {
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);

        // One cyan+underlined cell to mark.
        let cell = &mut buf[(0, 0)];
        cell.set_symbol("a");
        cell.fg = Color::Cyan;
        cell.modifier = Modifier::UNDERLINED;

        // URL contains ESC and BEL that could break the OSC 8 sequence.
        let malicious_url = "https://evil.com/\x1B]8;;\x07injected";
        mark_url_hyperlink(&mut buf, area, malicious_url);

        let sym = buf[(0, 0)].symbol().to_string();
        // The sanitized URL retains `]` (printable) but strips ESC and BEL.
        let sanitized = "https://evil.com/]8;;injected";
        assert!(
            sym.contains(sanitized),
            "symbol should contain sanitized URL, got: {sym:?}"
        );
        // The injected close-sequence must not survive: \x1B and \x07 are gone.
        assert!(
            !sym.contains("\x1B]8;;\x07injected"),
            "symbol must not contain raw control chars from URL"
        );
    }
}
