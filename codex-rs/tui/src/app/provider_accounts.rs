//! PF-84-S04: named-account actions from `/providers`.

use codex_vault::DEFAULT_PROVIDER_ACCOUNT;
use codex_vault::ProviderAccountName;

use super::App;
use super::AppEvent;
use super::AppServerSession;
use crate::legacy_core::config::edit::ConfigEdit;
use crate::legacy_core::config::edit::ConfigEditsBuilder;
use crate::provider_named_accounts::AccountsView;
use crate::provider_named_accounts::ProviderAccountEvent;
use crate::tui;

fn account_label(name: Option<&ProviderAccountName>) -> &str {
    name.map_or(DEFAULT_PROVIDER_ACCOUNT, ProviderAccountName::as_str)
}

impl App {
    pub(super) async fn handle_provider_account_event(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        event: ProviderAccountEvent,
    ) {
        match event {
            ProviderAccountEvent::OpenActions { provider_id, name } => {
                let Some(accounts) = self.provider_manager_accounts_view() else {
                    return;
                };
                let (Some(entry), Some(account)) = (
                    self.provider_account_catalog_entry(&provider_id),
                    accounts.find(&provider_id, name.as_str()).cloned(),
                ) else {
                    return;
                };
                self.chat_widget
                    .open_provider_account_actions(&entry, &account, &accounts);
            }
            ProviderAccountEvent::AddStart { provider_id } => {
                let methods = self.provider_account_methods(&provider_id);
                if methods.is_empty() {
                    self.chat_widget.add_error_message(format!(
                        "{provider_id} cannot hold named accounts yet."
                    ));
                    return;
                }
                let display_name = self.provider_account_display_name(&provider_id);
                self.chat_widget.open_provider_account_name_prompt(
                    &display_name,
                    provider_id,
                    /*rename_from*/ None,
                    methods,
                );
            }
            ProviderAccountEvent::OpenMethodChoice {
                provider_id,
                name,
                methods,
            } => {
                self.chat_widget
                    .open_provider_account_method_choice(provider_id, name, &methods);
            }
            ProviderAccountEvent::AddNamed {
                provider_id,
                name,
                method,
            } => {
                // An early hint only; the vault refuses an existing name again
                // under its lock when the value is saved.
                if self.provider_account_exists(&provider_id, &name) {
                    self.chat_widget.add_error_message(format!(
                        "{provider_id} already has an account `{name}`. Choose another name, or remove it first."
                    ));
                    return;
                }
                if method == crate::provider_named_accounts::AddAccountMethod::ChatgptDeviceCode {
                    self.start_named_chatgpt_sign_in(provider_id, name);
                    return;
                }
                if method == crate::provider_named_accounts::AddAccountMethod::Command {
                    self.app_event_tx
                        .send(AppEvent::ProviderAccount(ProviderAccountEvent::Save {
                            provider_id,
                            name,
                            method,
                            value: crate::provider_named_accounts::AccountValue::new(String::new()),
                        }));
                    return;
                }
                let display_name = self.provider_account_display_name(&provider_id);
                self.chat_widget.open_provider_account_value_entry(
                    &display_name,
                    provider_id,
                    name,
                    method,
                );
            }
            ProviderAccountEvent::Save {
                provider_id,
                name,
                method,
                value,
            } => {
                if !self
                    .provider_account_methods(&provider_id)
                    .contains(&method)
                {
                    self.chat_widget.add_error_message(format!(
                        "{provider_id} does not take that kind of account."
                    ));
                    return;
                }
                let codex_home = self.config.codex_home.to_path_buf();
                self.run_provider_account_job(move || {
                    crate::provider_named_accounts::save_account(
                        codex_home,
                        &provider_id,
                        &name,
                        method,
                        value,
                    )
                    .map(|message| {
                        format!(
                            "{message} Use it from /providers, `--account {name}` or a spawn `account`."
                        )
                    })
                });
            }
            ProviderAccountEvent::UseForSession { provider_id, name } => {
                self.use_provider_account_for_session(tui, app_server, provider_id, name)
                    .await;
            }
            ProviderAccountEvent::MakeDefault { provider_id, name } => {
                match self
                    .persist_default_provider_account(&provider_id, name.as_ref())
                    .await
                {
                    Ok(()) => self.chat_widget.add_info_message(
                        format!(
                            "New sessions use {provider_id} account `{}`. This session is unchanged.",
                            account_label(name.as_ref())
                        ),
                        /*hint*/ None,
                    ),
                    Err(error) => self.chat_widget.add_error_message(error),
                }
                self.schedule_provider_manager_refresh();
            }
            ProviderAccountEvent::RenameStart { provider_id, name } => {
                if self.provider_account_usage().session_account(&provider_id) == name.as_str() {
                    self.chat_widget.add_error_message(format!(
                        "This session uses {provider_id} account `{name}`. Start a new session on another account first, then rename it."
                    ));
                    return;
                }
                let display_name = self.provider_account_display_name(&provider_id);
                self.chat_widget.open_provider_account_name_prompt(
                    &display_name,
                    provider_id,
                    Some(name),
                    Vec::new(),
                );
            }
            ProviderAccountEvent::Rename {
                provider_id,
                from,
                to,
            } => {
                let result = self.rename_provider_account(&provider_id, &from, &to).await;
                self.provider_account_finished(result);
            }
            ProviderAccountEvent::RemoveStart { provider_id, name } => {
                let Some(entry) = self.provider_account_catalog_entry(&provider_id) else {
                    return;
                };
                let mut accounts = self.provider_account_usage();
                if let Some(view) = self.provider_manager_accounts_view() {
                    accounts.rows = view.rows;
                }
                self.chat_widget.open_provider_account_removal(
                    &entry,
                    provider_id,
                    name,
                    &accounts,
                );
            }
            ProviderAccountEvent::Remove {
                provider_id,
                name,
                replacement,
            } => {
                self.remove_provider_account(tui, app_server, provider_id, name, replacement)
                    .await;
            }
            ProviderAccountEvent::Finished { result } => self.provider_account_finished(result),
        }
    }

    fn provider_account_finished(&mut self, result: Result<String, String>) {
        match result {
            Ok(message) => self.chat_widget.add_info_message(message, /*hint*/ None),
            Err(message) => self.chat_widget.add_error_message(message),
        }
        // #572: an added (or the last removed) Bedrock account changes
        // whether Bedrock is reachable.
        super::provider_management_status::check_named_bedrock_account(
            &self.model_catalog,
            &self.config,
            &self.app_event_tx,
        );
        // D86/#1193: and whether the provider has a named account (`/status`).
        self.check_named_accounts();
        self.schedule_provider_manager_refresh();
    }

    /// D86/#1193/#1251: refreshes, off the UI thread, what `/status` reads:
    /// which providers have named accounts and the named sign-in summary of
    /// this config's account and, when it differs, the session's (a resumed
    /// thread or a changed default can leave the two apart).
    pub(super) fn check_named_accounts(&self) {
        crate::provider_named_accounts::check_named_account_providers(
            &self.model_catalog,
            &self.config,
        );
        let session = self.chat_widget.config_ref();
        let session_account = codex_model_provider::named_sign_in_account(&session.model_provider);
        if session_account.is_some()
            && session_account
                != codex_model_provider::named_sign_in_account(&self.config.model_provider)
        {
            crate::provider_named_accounts::check_named_account_providers(
                &self.model_catalog,
                session,
            );
        }
    }

    /// PF-84-S06: signs a named account in by device code. The code and URL
    /// are shown here; the sign-in is saved to that account's vault store
    /// only, and the same ChatGPT user under another account gets a warning.
    fn start_named_chatgpt_sign_in(&mut self, provider_id: String, name: ProviderAccountName) {
        let account = codex_login::NamedChatgptAccount {
            provider_id: provider_id.clone(),
            name: name.as_str().to_string(),
        };
        let mut opts = codex_login::ServerOptions::new(
            self.config.codex_home.to_path_buf(),
            codex_login::CLIENT_ID.to_string(),
            self.config.forced_chatgpt_workspace_id.clone(),
            self.config.cli_auth_credentials_store_mode,
            self.config.auth_keyring_backend_kind(),
            self.config.auth_route_config(),
        );
        opts.open_browser = false;
        opts.named_account = Some(account.clone());
        // Adding never replaces an existing account (or a concurrent add).
        opts.named_account_must_be_new = true;
        // QA only: debug builds can sign in against a mock issuer.
        if cfg!(debug_assertions)
            && let Ok(issuer) = std::env::var("CORBANU_TEST_LOGIN_ISSUER")
        {
            opts.issuer = issuer;
        }
        let codex_home = self.config.codex_home.to_path_buf();
        let store_mode = self.config.cli_auth_credentials_store_mode;
        let keyring_kind = self.config.auth_keyring_backend_kind();
        let account_label = format!("{provider_id} account `{name}`");
        let (code_tx, codes) = tokio::sync::mpsc::unbounded_channel();
        let state = crate::chatwidget::named_account_login::NamedAccountLoginState::default();
        let task_state = state.clone();
        let tx = self.app_event_tx.clone();
        let task = tokio::spawn(async move {
            let result = async {
                let code = codex_login::request_device_code(&opts)
                    .await
                    .map_err(|error| {
                        format!(
                            "Could not start the sign-in of {provider_id} account `{name}`: {error}"
                        )
                    })?;
                code_tx
                    .send(
                        crate::chatwidget::named_account_login::NamedAccountLoginCode {
                            verification_url: code.verification_url.clone(),
                            user_code: code.user_code.clone(),
                        },
                    )
                    .map_err(|_| "The sign-in screen was closed.".to_string())?;
                codex_login::complete_device_code_login_with_persistence_guard(opts, code, || {
                    task_state.begin_persistence()
                })
                .await
                .map_err(|error| {
                    format!("Sign-in of {provider_id} account `{name}` failed: {error}")
                })?;
                // Vault and default-login reads block; keep them off the runtime.
                let message = tokio::task::spawn_blocking(move || {
                    signed_in_message(
                        &codex_home,
                        &provider_id,
                        &name,
                        &account,
                        store_mode,
                        keyring_kind,
                    )
                })
                .await
                .unwrap_or_else(|_| "Signed in.".to_string());
                Ok(message)
            }
            .await;
            task_state.finish(&tx, result);
        });
        self.chat_widget
            .open_named_account_login(account_label, codes, task, state);
    }

    fn run_provider_account_job(
        &self,
        job: impl FnOnce() -> Result<String, String> + Send + 'static,
    ) {
        let tx = self.app_event_tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = job();
            tx.send(AppEvent::ProviderAccount(ProviderAccountEvent::Finished {
                result,
            }));
        });
    }

    /// Which account this session and new sessions use, read from the live
    /// configs so in-use checks never depend on `/providers` being open.
    pub(super) fn provider_account_usage(&self) -> AccountsView {
        AccountsView::new(Vec::new(), self.chat_widget.config_ref(), &self.config)
    }

    fn provider_account_methods(
        &self,
        provider_id: &str,
    ) -> Vec<crate::provider_named_accounts::AddAccountMethod> {
        use crate::provider_named_accounts::AddAccountMethod;
        use codex_protocol::config_types::ForcedLoginMethod;
        let mut methods = self
            .config
            .model_providers
            .get(provider_id)
            .map(|provider| crate::provider_named_accounts::add_methods(provider_id, provider))
            .unwrap_or_default();
        // A required login method rules out the other kind of sign-in. Only
        // the configured one: the API-key-only default a third-party session
        // provider implies is about the default login (#573).
        match self.config.configured_forced_login_method {
            Some(ForcedLoginMethod::Chatgpt) => {
                methods.retain(|method| *method != AddAccountMethod::OpenaiApiKey);
            }
            Some(ForcedLoginMethod::Api) => {
                methods.retain(|method| *method != AddAccountMethod::ChatgptDeviceCode);
            }
            None => {}
        }
        methods
    }

    fn provider_account_exists(&self, provider_id: &str, name: &ProviderAccountName) -> bool {
        self.provider_manager_accounts_view()
            .is_some_and(|accounts| accounts.find(provider_id, name.as_str()).is_some())
    }

    fn provider_account_catalog_entry(
        &self,
        provider_id: &str,
    ) -> Option<codex_provider_auth::ProviderCatalogEntry> {
        let host = self.provider_management_host.as_ref()?;
        host.status_host()
            .catalog()
            .entries()
            .iter()
            .find(|entry| {
                entry
                    .runtime_provider_ids
                    .iter()
                    .any(|id| id.as_str() == provider_id)
            })
            .cloned()
    }

    fn provider_account_display_name(&self, provider_id: &str) -> String {
        self.provider_account_catalog_entry(provider_id)
            .map_or_else(|| provider_id.to_string(), |entry| entry.display_name)
    }

    /// Renames in the vault; a default that named the old account follows.
    /// The session's own account is never renamed under it.
    pub(super) async fn rename_provider_account(
        &mut self,
        provider_id: &str,
        from: &ProviderAccountName,
        to: &ProviderAccountName,
    ) -> Result<String, String> {
        let accounts = self.provider_account_usage();
        if accounts.session_account(provider_id) == from.as_str() {
            return Err(format!(
                "This session uses {provider_id} account `{from}`; it was not renamed."
            ));
        }
        let codex_home = self.config.codex_home.to_path_buf();
        let message = tokio::task::spawn_blocking({
            let (provider_id, from, to) = (provider_id.to_string(), from.clone(), to.clone());
            move || {
                crate::provider_named_accounts::rename_account(codex_home, &provider_id, &from, &to)
            }
        })
        .await
        .unwrap_or_else(|error| Err(error.to_string()))?;
        if accounts.default_account(provider_id) != from.as_str() {
            return Ok(message);
        }
        match self
            .persist_default_provider_account(provider_id, Some(to))
            .await
        {
            Ok(()) => Ok(format!("{message} It stays the default for new sessions.")),
            Err(error) => Err(format!(
                "{message} But the default for new sessions still names `{from}`, so they refuse to start until you choose another default. {error}"
            )),
        }
    }

    /// Writes `[provider_accounts] <provider> = <name>` (or clears it for the
    /// `default` account), reloads the config new sessions start from, and
    /// checks that the change is what new sessions now see.
    pub(super) async fn persist_default_provider_account(
        &mut self,
        provider_id: &str,
        name: Option<&ProviderAccountName>,
    ) -> Result<(), String> {
        let segments = vec!["provider_accounts".to_string(), provider_id.to_string()];
        let edit = match name {
            Some(name) => ConfigEdit::SetPath {
                segments,
                value: name.as_str().into(),
            },
            None => ConfigEdit::ClearPath { segments },
        };
        ConfigEditsBuilder::for_config(&self.config)
            .with_edits([edit])
            .apply()
            .await
            .map_err(|error| {
                format!("Could not save the default {provider_id} account: {error}")
            })?;
        self.refresh_in_memory_config_from_disk()
            .await
            .map_err(|error| format!("Saved, but the config could not be reloaded: {error}"))?;
        // A project config or `-c` entry outranks the user's config.toml.
        let effective = crate::provider_named_accounts::persisted_defaults(&self.config)
            .get(provider_id)
            .cloned();
        if effective.as_deref() != name.map(ProviderAccountName::as_str) {
            return Err(format!(
                "Saved to config.toml, but another config layer (a project config or `-c`) sets provider_accounts.{provider_id} = \"{}\", so new sessions still use that.",
                effective.as_deref().unwrap_or(DEFAULT_PROVIDER_ACCOUNT)
            ));
        }
        Ok(())
    }

    /// Points the next session's `provider_id` requests at `name` and rebuilds
    /// the config. Returns the previous selection to restore on failure. One
    /// session selection covers one provider, so a selection held for another
    /// provider is never silently dropped.
    pub(super) async fn select_session_provider_account(
        &mut self,
        provider_id: &str,
        name: Option<&ProviderAccountName>,
    ) -> Result<Option<String>, String> {
        if let Some(existing) = self.harness_overrides.provider_account.as_deref() {
            let (existing_provider, existing_name) =
                crate::legacy_core::config::split_account_selection(
                    existing,
                    &self.config.model_provider_id,
                );
            // Dropping it is fine only when that changes nothing for its provider.
            let configured_default =
                crate::provider_named_accounts::persisted_defaults(&self.config)
                    .get(existing_provider)
                    .cloned()
                    .unwrap_or_else(|| DEFAULT_PROVIDER_ACCOUNT.to_string());
            if existing_provider != provider_id && existing_name != configured_default {
                return Err(format!(
                    "This session already selects {existing_provider} account `{existing_name}`, and one session selection covers one provider. Start a new session on {existing_provider} account `{configured_default}` (its default for new sessions) first, or make {provider_id} account `{}` the default for new sessions.",
                    account_label(name)
                ));
            }
        }
        let selection = format!("{provider_id}:{}", account_label(name));
        let previous = self.harness_overrides.provider_account.replace(selection);
        if let Err(error) = self.refresh_in_memory_config_from_disk().await {
            self.harness_overrides.provider_account = previous;
            return Err(format!(
                "Could not use {provider_id} account `{}`: {error:#}",
                account_label(name)
            ));
        }
        Ok(previous)
    }

    /// Starts a new session whose requests to `provider_id` use `name`. The
    /// running thread keeps its account: a live thread never changes account.
    /// When the new session does not start, the previous selection and config
    /// are restored; the old session has already ended, as with `/new`.
    async fn use_provider_account_for_session(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        provider_id: String,
        name: Option<ProviderAccountName>,
    ) -> bool {
        let previous = match self
            .select_session_provider_account(&provider_id, name.as_ref())
            .await
        {
            Ok(previous) => previous,
            Err(error) => {
                self.chat_widget.add_error_message(error);
                return false;
            }
        };
        let started = self
            .start_fresh_session_with_summary_hint(
                tui, app_server, /*session_start_source*/ None,
                /*initial_user_message*/ None, /*new_thread_name*/ None,
            )
            .await;
        if !started {
            self.harness_overrides.provider_account = previous;
            self.refresh_in_memory_config_from_disk_best_effort("restoring the session account")
                .await;
            self.chat_widget.add_error_message(format!(
                "The new session did not start, so no session runs on {provider_id} account `{}`; the account selection was not changed.",
                account_label(name.as_ref())
            ));
            return false;
        }
        self.chat_widget.add_info_message(
            format!(
                "New session: requests to {provider_id} use account `{}`.",
                account_label(name.as_ref())
            ),
            /*hint*/ None,
        );
        // PF-84: the model list follows the new session's account.
        let _ = self.refresh_model_list(app_server).await;
        true
    }

    async fn remove_provider_account(
        &mut self,
        tui: &mut tui::Tui,
        app_server: &mut AppServerSession,
        provider_id: String,
        name: ProviderAccountName,
        replacement: Option<Option<ProviderAccountName>>,
    ) {
        let accounts = self.provider_account_usage();
        if accounts.in_use(&provider_id, name.as_str()) {
            // PF-54: an account in use is replaced before it is removed.
            let Some(replacement) = replacement else {
                self.chat_widget.add_error_message(format!(
                    "{provider_id} account `{name}` is in use; choose a replacement first."
                ));
                return;
            };
            // Switch the session first: if that fails nothing has changed.
            if accounts.session_account(&provider_id) == name.as_str()
                && !self
                    .use_provider_account_for_session(
                        tui,
                        app_server,
                        provider_id.clone(),
                        replacement.clone(),
                    )
                    .await
            {
                self.chat_widget
                    .add_error_message(format!("{provider_id} account `{name}` was not removed."));
                return;
            }
            if accounts.default_account(&provider_id) == name.as_str()
                && let Err(error) = self
                    .persist_default_provider_account(&provider_id, replacement.as_ref())
                    .await
            {
                self.chat_widget.add_error_message(format!(
                    "{error} {provider_id} account `{name}` was not removed."
                ));
                return;
            }
        }
        let codex_home = self.config.codex_home.to_path_buf();
        let auth_route_config = self.config.auth_route_config();
        self.run_provider_account_job(move || {
            crate::provider_named_accounts::remove_account(
                codex_home,
                &provider_id,
                &name,
                &auth_route_config,
            )
        });
    }
}

/// The message after a named sign-in: its kind, email hint and plan, and a
/// warning when the same ChatGPT user is signed in under another account.
fn signed_in_message(
    codex_home: &std::path::Path,
    provider_id: &str,
    name: &ProviderAccountName,
    account: &codex_login::NamedChatgptAccount,
    store_mode: codex_login::AuthCredentialsStoreMode,
    keyring_kind: codex_login::AuthKeyringBackendKind,
) -> String {
    let summary = codex_login::named_account_summary(codex_home, account)
        .ok()
        .flatten()
        .map(|summary| {
            format!(
                " ({})",
                crate::provider_named_accounts::describe_sign_in(&summary)
            )
        })
        .unwrap_or_default();
    let mut message = format!(
        "Signed in {provider_id} account `{name}`{summary}. Use it from /providers, \
         `--account {name}` or a spawn `account`."
    );
    let default_auth = codex_login::load_auth_dot_json(codex_home, store_mode, keyring_kind)
        .ok()
        .flatten();
    let same = codex_login::same_user_sign_ins(codex_home, account, default_auth.as_ref())
        .unwrap_or_default();
    if !same.is_empty() {
        message.push_str(&format!(
            " Warning: this is the same ChatGPT user as account(s) {}; they share that \
             user's limits, and signing out of one can end the other's session.",
            same.iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    message
}

#[cfg(test)]
#[path = "provider_accounts_tests.rs"]
mod tests;
