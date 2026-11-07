use codex_provider_auth::CredentialControl;
use codex_provider_auth::ExplicitProviderSelection;
use codex_provider_auth::ProviderActivationPolicy;
use codex_provider_auth::ProviderAvailabilityState;
use codex_provider_auth::ProviderCatalog;
use codex_provider_auth::ProviderCatalogEntry;
use codex_provider_auth::ProviderConfigurationState;
use codex_provider_auth::ProviderCurrentState;
use codex_provider_auth::ProviderEligibilityState;
use codex_provider_auth::ProviderMethodState;
use codex_provider_auth::ProviderSetupCapability;
use codex_provider_auth::ProviderStatusSnapshot;

use super::*;
use crate::provider_named_accounts::AccountsView;
use crate::provider_named_accounts::NamedAccountRow;
use crate::provider_named_accounts::ProviderAccountEvent;

const MANAGER_VIEW_ID: &str = "provider-manager";

#[cfg(test)]
#[path = "provider_manager_status_account_tests.rs"]
mod status_account_tests;

impl ChatWidget {
    pub(crate) fn open_provider_manager_api_key(
        &mut self,
        attempt_id: codex_provider_auth::ProviderManagementAttemptId,
        target: codex_provider_auth::ApiKeyAuthTarget,
        display_name: String,
    ) {
        let tx = self.app_event_tx.clone();
        let cancel_tx = self.app_event_tx.clone();
        let cancelled_provider = target.provider_id.clone();
        let view = crate::bottom_pane::vault_secret_entry::VaultSecretEntryView::new_fixed_secret_with_cancel(
            format!("provider:{}", target.provider_id),
            format!("Add {display_name}"),
            "API key — masked".to_string(),
            crate::provider_auth_presentation::api_key_guidance(&target.storage),
            Box::new(move |_label, secret| {
                tx.send(AppEvent::SaveProviderManagerApiKey {
                    attempt_id,
                    target: target.clone(),
                    api_key: crate::app_event::ProviderApiKeySecret::new(secret),
                });
            }),
            Box::new(move || cancel_tx.send(AppEvent::ProviderManagerApiKeyCancelled {
                attempt_id, provider_id: cancelled_provider.clone(),
            })),
        )
        .rejecting_shown_errors(self.shown_error_texts());
        self.bottom_pane.show_view(Box::new(view));
    }

    pub(crate) fn open_provider_manager(
        &mut self,
        catalog: &ProviderCatalog,
        statuses: &[ProviderStatusSnapshot],
        focused_provider: Option<&codex_provider_auth::ProviderCatalogId>,
        accounts: Option<&AccountsView>,
    ) {
        let manager_present = self.provider_manager_selected_index().is_some();
        let mut header = ColumnRenderable::new();
        header.push(Line::from("Providers".bold()));
        header.push(Line::from(
            "Configure providers and control whether they are eligible for use.",
        ));
        header.push(Line::from(
            "Configured means credentials are present, not verified by the provider.",
        ));
        let rows = provider_manager_rows(catalog, statuses, accounts);
        let items = rows
            .iter()
            .filter_map(|row| match row {
                ManagerRow::Provider { status_index, .. } => {
                    let status = statuses.get(*status_index)?;
                    let entry = catalog.get(status.id.as_str())?;
                    Some(provider_item(entry, status, accounts))
                }
                ManagerRow::Account {
                    status_index,
                    account_index,
                    ..
                } => {
                    let status = statuses.get(*status_index)?;
                    let entry = catalog.get(status.id.as_str())?;
                    let accounts = accounts?;
                    let account = accounts.rows.get(*account_index)?;
                    Some(account_item(entry, account, accounts))
                }
            })
            .collect();
        let params = SelectionViewParams {
            view_id: Some(MANAGER_VIEW_ID),
            header: Box::new(header),
            items,
            footer_note: Some("r recover credentials · Enter manage · Esc back".into()),
            initial_selected_idx: focused_provider.and_then(|provider_id| {
                rows.iter().position(|row| {
                    matches!(row, ManagerRow::Provider { .. }) && row.provider_id() == provider_id
                })
            }),
            ..Default::default()
        };
        if manager_present {
            let _replaced = self.replace_selection_view_if_present(MANAGER_VIEW_ID, params);
        } else {
            self.show_selection_view(params);
        }
    }

    /// PF-84-S04: what can be done with one named account.
    pub(crate) fn open_provider_account_actions(
        &mut self,
        entry: &ProviderCatalogEntry,
        account: &NamedAccountRow,
        accounts: &AccountsView,
    ) {
        let provider_id = account.provider_id.clone();
        let name = account.name.clone();
        let mut header = ColumnRenderable::new();
        header.push(Line::from(
            format!("{} · {}", entry.display_name, account.name).bold(),
        ));
        header.push(Line::from(format!(
            "{}{}",
            crate::provider_named_accounts::row_detail(account),
            accounts.markers(&provider_id, name.as_str())
        )));
        let mut items = Vec::new();
        if accounts.session_account(&provider_id) != name.as_str() {
            items.push(account_event_item(
                "Start a new session on this account",
                "Leaves this conversation as it is; the new session's requests to this provider use this account.",
                ProviderAccountEvent::UseForSession {
                    provider_id: provider_id.clone(),
                    name: Some(name.clone()),
                },
            ));
        }
        if accounts.default_account(&provider_id) != name.as_str() {
            items.push(account_event_item(
                "Make default",
                "New sessions use this account; this session is unchanged.",
                ProviderAccountEvent::MakeDefault {
                    provider_id: provider_id.clone(),
                    name: Some(name.clone()),
                },
            ));
        }
        items.push(account_event_item(
            "Rename",
            "Give this account another name; its credential is unchanged.",
            ProviderAccountEvent::RenameStart {
                provider_id: provider_id.clone(),
                name: name.clone(),
            },
        ));
        items.push(account_event_item(
            "Remove",
            "Delete this account's credential from the vault; other accounts are kept.",
            ProviderAccountEvent::RemoveStart { provider_id, name },
        ));
        self.show_selection_view(SelectionViewParams {
            header: Box::new(header),
            items,
            ..Default::default()
        });
    }

    /// PF-84-S04: asks for a new account's name.
    pub(crate) fn open_provider_account_name_prompt(
        &mut self,
        provider_display_name: &str,
        provider_id: String,
        rename_from: Option<codex_vault::ProviderAccountName>,
        methods: Vec<crate::provider_named_accounts::AddAccountMethod>,
    ) {
        let tx = self.app_event_tx.clone();
        let refused_tx = self.app_event_tx.clone();
        let title = match &rename_from {
            Some(from) => format!("Rename {provider_display_name} account `{from}`"),
            None => format!("Add a {provider_display_name} account"),
        };
        let view = CustomPromptView::new(
            title,
            "Account name: lowercase letters, digits or '-' (for example work)".to_string(),
            /*initial_text*/ String::new(),
            /*context_label*/ None,
            Box::new(move |raw: String| {
                let name = match codex_vault::ProviderAccountName::parse(&raw) {
                    Ok(name) => name,
                    Err(error) => {
                        tx.send(AppEvent::ProviderAccount(ProviderAccountEvent::Finished {
                            result: Err(error.to_string()),
                        }));
                        return;
                    }
                };
                let event = match (&rename_from, methods.as_slice()) {
                    (Some(from), _) => ProviderAccountEvent::Rename {
                        provider_id: provider_id.clone(),
                        from: from.clone(),
                        to: name,
                    },
                    (None, [method]) => ProviderAccountEvent::AddNamed {
                        provider_id: provider_id.clone(),
                        name,
                        method: *method,
                    },
                    (None, _) => {
                        tx.send(AppEvent::ProviderAccount(
                            ProviderAccountEvent::OpenMethodChoice {
                                provider_id: provider_id.clone(),
                                name,
                                methods: methods.clone(),
                            },
                        ));
                        return;
                    }
                };
                tx.send(AppEvent::ProviderAccount(event));
            }),
        )
        // #1289: Enter submits the name even right after fast typing; a
        // newline would leave the next (secret) input in this clear field.
        // Input sent together with the submit (a key) is held, not passed
        // on to whatever the submit opens, and the name is refused.
        .with_submit_mode(
            crate::bottom_pane::custom_prompt_view::CustomPromptSubmitMode::EnterSingleLine,
        )
        .with_on_refused(Box::new(move || {
            refused_tx.send(AppEvent::ProviderAccount(ProviderAccountEvent::Finished {
                result: Err(codex_vault::ProviderAccountError::InvalidName.to_string()),
            }));
        }));
        self.bottom_pane.show_view(Box::new(view));
    }

    /// PF-84-S04: how a new account's credential is entered.
    pub(crate) fn open_provider_account_method_choice(
        &mut self,
        provider_id: String,
        name: codex_vault::ProviderAccountName,
        methods: &[crate::provider_named_accounts::AddAccountMethod],
    ) {
        let mut header = ColumnRenderable::new();
        header.push(Line::from(format!("Add account `{name}`").bold()));
        let items = methods
            .iter()
            .map(|method| {
                account_event_item(
                    method.label(),
                    if *method
                        == crate::provider_named_accounts::AddAccountMethod::ChatgptDeviceCode
                    {
                        "Shows a code to enter in your browser; saved to this account only."
                    } else if method.is_secret() {
                        "Paste it into masked entry."
                    } else if *method
                        == crate::provider_named_accounts::AddAccountMethod::AwsProfile
                    {
                        "Enter the profile name; it is not secret."
                    } else {
                        "Enter the path; it is not secret."
                    },
                    ProviderAccountEvent::AddNamed {
                        provider_id: provider_id.clone(),
                        name: name.clone(),
                        method: *method,
                    },
                )
            })
            .collect();
        self.show_selection_view(SelectionViewParams {
            header: Box::new(header),
            items,
            ..Default::default()
        });
    }

    /// PF-84-S04: masked (or path) entry for a new account's credential.
    pub(crate) fn open_provider_account_value_entry(
        &mut self,
        provider_display_name: &str,
        provider_id: String,
        name: codex_vault::ProviderAccountName,
        method: crate::provider_named_accounts::AddAccountMethod,
    ) {
        use crate::provider_named_accounts::AccountValue;
        use crate::provider_named_accounts::AddAccountMethod;
        let tx = self.app_event_tx.clone();
        let title = format!("{provider_display_name} account `{name}`");
        if matches!(
            method,
            AddAccountMethod::ClaudeConfigDir | AddAccountMethod::AwsProfile
        ) {
            let placeholder = if method == AddAccountMethod::AwsProfile {
                "AWS profile name from ~/.aws/config (SSO: run aws sso login yourself)"
            } else {
                "Path of the CLAUDE_CONFIG_DIR you signed in with (claude /login)"
            };
            let view = CustomPromptView::new(
                title,
                placeholder.to_string(),
                /*initial_text*/ String::new(),
                /*context_label*/ None,
                Box::new(move |path: String| {
                    tx.send(AppEvent::ProviderAccount(ProviderAccountEvent::Save {
                        provider_id: provider_id.clone(),
                        name: name.clone(),
                        method,
                        value: AccountValue::new(path),
                    }));
                }),
            );
            self.bottom_pane.show_view(Box::new(view));
            return;
        }
        let view = crate::bottom_pane::vault_secret_entry::VaultSecretEntryView::new_fixed_secret(
            format!("account:{provider_id}:{name}"),
            title,
            format!("{} — masked", method.label()),
            "Paste the value and press Enter. It is saved to the vault and never shown."
                .to_string(),
            Box::new(move |_label, secret| {
                tx.send(AppEvent::ProviderAccount(ProviderAccountEvent::Save {
                    provider_id,
                    name,
                    method,
                    value: AccountValue::new(secret),
                }));
            }),
        )
        .rejecting_shown_errors(self.shown_error_texts());
        self.bottom_pane.show_view(Box::new(view));
    }

    /// PF-84-S04: removing an account in use needs a replacement first.
    pub(crate) fn open_provider_account_removal(
        &mut self,
        entry: &ProviderCatalogEntry,
        provider_id: String,
        name: codex_vault::ProviderAccountName,
        accounts: &AccountsView,
    ) {
        let mut header = ColumnRenderable::new();
        header.push(Line::from(
            format!("Remove {} account `{name}`", entry.display_name).bold(),
        ));
        let mut items = Vec::new();
        if accounts.in_use(&provider_id, name.as_str()) {
            header.push(Line::from(
                "It is in use. Choose the account that replaces it first; removal follows.",
            ));
            let mut replacements = vec![None];
            replacements.extend(
                accounts
                    .rows
                    .iter()
                    .filter(|row| row.provider_id == provider_id && row.name != name)
                    .map(|row| Some(row.name.clone())),
            );
            for replacement in replacements {
                let label = replacement
                    .as_ref()
                    .map_or(codex_vault::DEFAULT_PROVIDER_ACCOUNT, |name| name.as_str())
                    .to_string();
                items.push(account_event_item(
                    &format!("Replace with `{label}`"),
                    "Takes over this session and new sessions, then the account is removed.",
                    ProviderAccountEvent::Remove {
                        provider_id: provider_id.clone(),
                        name: name.clone(),
                        replacement: Some(replacement),
                    },
                ));
            }
        } else {
            header.push(Line::from(
                "Its credential is deleted from the vault. Other accounts are kept.",
            ));
            items.push(account_event_item(
                "Remove",
                "This cannot be undone.",
                ProviderAccountEvent::Remove {
                    provider_id,
                    name,
                    replacement: None,
                },
            ));
        }
        items.push(SelectionItem {
            name: "Keep it".to_string(),
            description: Some("Nothing changes.".to_string()),
            dismiss_on_select: true,
            ..Default::default()
        });
        self.show_selection_view(SelectionViewParams {
            header: Box::new(header),
            items,
            ..Default::default()
        });
    }

    pub(crate) fn provider_manager_selected_index(&self) -> Option<usize> {
        self.selected_index_for_present_view(MANAGER_VIEW_ID)
    }

    pub(crate) fn open_provider_manager_actions(
        &mut self,
        entry: &ProviderCatalogEntry,
        status: &ProviderStatusSnapshot,
        account_provider: Option<(&str, &AccountsView)>,
    ) {
        let mut header = ColumnRenderable::new();
        header.push(Line::from(entry.display_name.clone().bold()));
        header.push(Line::from(status_description(status)));
        if is_unchecked_local_runtime(status) {
            header.push(Line::from(
                "Local server: not checked; the first request shows whether it is running.",
            ));
        } else if status.configuration == ProviderConfigurationState::Configured {
            header.push(Line::from(
                "Credentials are present; a successful request is needed to verify access.",
            ));
        }
        if let Some(note) = credential_control_note(status) {
            header.push(Line::from(note));
        }
        let mut items = Vec::new();
        match (status.configuration, status.eligibility) {
            (ProviderConfigurationState::Configured, ProviderEligibilityState::Active) => {
                items.push(policy_item(
                    status.id.clone(),
                    ProviderActivationPolicy::Inactive,
                    "Deactivate",
                    if status.current == ProviderCurrentState::Current {
                        "Choose an exact usable replacement first; credentials are kept."
                    } else {
                        "Exclude this provider from use; credentials are kept."
                    },
                ));
            }
            (ProviderConfigurationState::Configured, ProviderEligibilityState::Inactive) => {
                items.push(policy_item(
                    status.id.clone(),
                    ProviderActivationPolicy::Active,
                    "Reactivate",
                    "Make this configured provider eligible for use again.",
                ));
            }
            _ => {}
        }
        for capability in entry.setup_capabilities.iter() {
            if interactive(capability)
                && (status.configuration != ProviderConfigurationState::Configured
                    || matches!(
                        capability,
                        ProviderSetupCapability::ClaudeAccount
                            | ProviderSetupCapability::OpenAiAccount
                            | ProviderSetupCapability::ApiKey { .. }
                    ))
            {
                let provider_id = status.id.clone();
                let capability = capability.clone();
                items.push(SelectionItem {
                    name: setup_label(status, &capability),
                    description: Some(setup_description(&capability)),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::ProviderManagerBeginAuthentication {
                            provider_id: provider_id.clone(),
                            capability: capability.clone(),
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                });
            }
        }
        if let Some((provider_id, accounts)) = account_provider {
            push_default_account_items(&mut items, provider_id, accounts);
        }
        self.show_selection_view(SelectionViewParams {
            header: Box::new(header),
            items,
            ..Default::default()
        });
    }

    pub(crate) fn open_provider_replacement(
        &mut self,
        target_provider_id: codex_provider_auth::ProviderCatalogId,
        catalog: &ProviderCatalog,
        candidates: Vec<ExplicitProviderSelection>,
    ) {
        let mut header = ColumnRenderable::new();
        header.push(Line::from("Choose replacement".bold()));
        header.push(Line::from(
            "The replacement is persisted and made current before deactivation.",
        ));
        let items = candidates
            .into_iter()
            .filter_map(|replacement| {
                let entry = catalog.get(replacement.provider_id.as_str())?;
                let name = format!("{} — {}", entry.display_name, replacement.model);
                let target_provider_id = target_provider_id.clone();
                Some(SelectionItem {
                    name,
                    description: Some(format!(
                        "Exact provider: {}",
                        replacement.runtime_provider_id.as_str()
                    )),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::ProviderManagerChooseReplacement {
                            target_provider_id: target_provider_id.clone(),
                            replacement: replacement.clone(),
                        });
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                })
            })
            .collect();
        let cancelled_target = target_provider_id;
        self.show_selection_view(SelectionViewParams {
            header: Box::new(header),
            items,
            on_cancel: Some(Box::new(move |tx| {
                tx.send(AppEvent::ProviderManagerCancelReplacement {
                    target_provider_id: cancelled_target.clone(),
                });
            })),
            ..Default::default()
        });
    }
}

/// One `/providers` row: a provider (its `default` account) or, while named
/// accounts are on, one of its named accounts listed under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ManagerRow {
    Provider {
        provider_id: codex_provider_auth::ProviderCatalogId,
        status_index: usize,
    },
    Account {
        provider_id: codex_provider_auth::ProviderCatalogId,
        status_index: usize,
        account_index: usize,
    },
}

impl ManagerRow {
    pub(crate) fn provider_id(&self) -> &codex_provider_auth::ProviderCatalogId {
        match self {
            Self::Provider { provider_id, .. } | Self::Account { provider_id, .. } => provider_id,
        }
    }
}

/// The rows `/providers` shows, in order: exactly one per (provider, account).
pub(crate) fn provider_manager_rows(
    catalog: &ProviderCatalog,
    statuses: &[ProviderStatusSnapshot],
    accounts: Option<&AccountsView>,
) -> Vec<ManagerRow> {
    let mut rows = Vec::new();
    for (status_index, status) in statuses.iter().enumerate() {
        let Some(entry) = catalog.get(status.id.as_str()) else {
            continue;
        };
        rows.push(ManagerRow::Provider {
            provider_id: status.id.clone(),
            status_index,
        });
        let Some(accounts) = accounts else {
            continue;
        };
        let runtime_ids = entry
            .runtime_provider_ids
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>();
        for (account_index, account) in accounts.rows.iter().enumerate() {
            if runtime_ids.contains(&account.provider_id) {
                rows.push(ManagerRow::Account {
                    provider_id: status.id.clone(),
                    status_index,
                    account_index,
                });
            }
        }
    }
    rows
}

fn provider_item(
    entry: &ProviderCatalogEntry,
    status: &ProviderStatusSnapshot,
    accounts: Option<&AccountsView>,
) -> SelectionItem {
    let provider_id = status.id.clone();
    let recovery_id = status.id.clone();
    let mut name = entry.display_name.clone();
    let mut description = status_description(status);
    if let Some(accounts) = accounts {
        let runtime_ids = entry
            .runtime_provider_ids
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>();
        if let Some(account) = accounts.rows_for(&runtime_ids).next() {
            name.push_str(" · default");
            // #609: when a named account decided the status, it is that
            // account's, not the default account's this row is named for.
            if let Some(status_account) = accounts.status_account(status.id.as_str()) {
                description = match accounts.status_account_detail(status.id.as_str()) {
                    // #609: and what it holds (an OpenAI API key or a
                    // ChatGPT sign-in), from the account registry.
                    Some(detail) => format!(
                        "{description} (status of account `{status_account}`, which holds {detail})"
                    ),
                    None => format!("{description} (status of account `{status_account}`)"),
                };
            }
            description.push_str(
                &accounts.markers(&account.provider_id, codex_vault::DEFAULT_PROVIDER_ACCOUNT),
            );
        }
    }
    SelectionItem {
        name,
        description: Some(description),
        actions: vec![Box::new(move |tx| {
            tx.send(AppEvent::OpenProviderManagerActions {
                provider_id: provider_id.clone(),
            });
        })],
        dismiss_on_select: false,
        selected_shortcuts: vec![crate::bottom_pane::SelectionShortcutAction {
            key: crate::key_hint::plain(KeyCode::Char('r')),
            action: Box::new(move |tx| {
                tx.send(AppEvent::OpenProviderManagerRecovery {
                    provider_id: recovery_id.clone(),
                })
            }),
            dismiss_on_select: false,
        }],
        ..Default::default()
    }
}

fn account_item(
    entry: &ProviderCatalogEntry,
    account: &NamedAccountRow,
    accounts: &AccountsView,
) -> SelectionItem {
    let provider_id = account.provider_id.clone();
    let name = account.name.clone();
    SelectionItem {
        name: format!("{} · {}", entry.display_name, account.name),
        description: Some(format!(
            "Named account · {}{}",
            crate::provider_named_accounts::row_detail(account),
            accounts.markers(&account.provider_id, account.name.as_str())
        )),
        actions: vec![Box::new(move |tx| {
            tx.send(AppEvent::ProviderAccount(
                ProviderAccountEvent::OpenActions {
                    provider_id: provider_id.clone(),
                    name: name.clone(),
                },
            ));
        })],
        dismiss_on_select: false,
        ..Default::default()
    }
}

fn account_event_item(name: &str, description: &str, event: ProviderAccountEvent) -> SelectionItem {
    let event = std::sync::Mutex::new(Some(event));
    SelectionItem {
        name: name.to_string(),
        description: Some(description.to_string()),
        actions: vec![Box::new(move |tx| {
            if let Some(event) = event.lock().ok().and_then(|mut event| event.take()) {
                tx.send(AppEvent::ProviderAccount(event));
            }
        })],
        dismiss_on_select: true,
        ..Default::default()
    }
}

/// Provider actions for its accounts: add one, and move this session or new
/// sessions back to the `default` account.
fn push_default_account_items(
    items: &mut Vec<SelectionItem>,
    provider_id: &str,
    accounts: &AccountsView,
) {
    items.push(account_event_item(
        "Add another account",
        if provider_id == codex_model_provider_info::AMAZON_BEDROCK_PROVIDER_ID {
            "Name it, then enter the AWS profile it uses (not secret)."
        } else if provider_id == codex_model_provider_info::OPENAI_PROVIDER_ID {
            "Name it, then sign in with ChatGPT or paste an OpenAI API key."
        } else {
            "Name it, then paste its key or token into masked entry."
        },
        ProviderAccountEvent::AddStart {
            provider_id: provider_id.to_string(),
        },
    ));
    if accounts.session_account(provider_id) != codex_vault::DEFAULT_PROVIDER_ACCOUNT {
        items.push(account_event_item(
            "Start a new session on the default account",
            "Leaves this conversation as it is; the new session's requests use this provider's default account.",
            ProviderAccountEvent::UseForSession {
                provider_id: provider_id.to_string(),
                name: None,
            },
        ));
    }
    if accounts.default_account(provider_id) != codex_vault::DEFAULT_PROVIDER_ACCOUNT {
        items.push(account_event_item(
            "Make `default` the default for new sessions",
            "New sessions use this provider's default account again.",
            ProviderAccountEvent::MakeDefault {
                provider_id: provider_id.to_string(),
                name: None,
            },
        ));
    }
}

fn policy_item(
    provider_id: codex_provider_auth::ProviderCatalogId,
    policy: ProviderActivationPolicy,
    name: &str,
    description: &str,
) -> SelectionItem {
    SelectionItem {
        name: name.to_string(),
        description: Some(description.to_string()),
        actions: vec![Box::new(move |tx| {
            tx.send(AppEvent::ProviderManagerRequestPolicy {
                provider_id: provider_id.clone(),
                policy,
            });
        })],
        dismiss_on_select: true,
        ..Default::default()
    }
}

fn status_description(status: &ProviderStatusSnapshot) -> String {
    let state = match status.configuration {
        ProviderConfigurationState::Configured => match status.eligibility {
            // Ollama and LM Studio are never probed (no credential, no startup
            // request), so their row must not claim more than that.
            ProviderEligibilityState::Active if is_unchecked_local_runtime(status) => {
                "Enabled · local (not checked)"
            }
            ProviderEligibilityState::Active => "Enabled · configured",
            ProviderEligibilityState::Inactive => "Inactive",
            _ => "Configured",
        },
        ProviderConfigurationState::RecoveryRequired => "Credential needs attention · r recover",
        ProviderConfigurationState::NotConfigured => "Not configured",
        ProviderConfigurationState::Checking => "Checking",
        ProviderConfigurationState::Unavailable => "Unavailable",
    };
    let current = (status.current == ProviderCurrentState::Current).then_some(" · current");
    let unavailable =
        (status.availability != ProviderAvailabilityState::Ready).then_some(" · unavailable");
    format!(
        "{state}{}{}",
        current.unwrap_or(""),
        unavailable.unwrap_or("")
    )
}

/// A local runtime (Ollama, LM Studio) is reported configured without any
/// probe: nothing is checked until a request is sent to it.
pub(super) fn is_unchecked_local_runtime(status: &ProviderStatusSnapshot) -> bool {
    status.methods.iter().any(|method| {
        matches!(method.capability, ProviderSetupCapability::Local { .. })
            && matches!(
                method.state,
                ProviderMethodState::Configured {
                    source: codex_provider_auth::ProviderCredentialSource::Local,
                    ..
                }
            )
    })
}

fn credential_control_note(status: &ProviderStatusSnapshot) -> Option<&'static str> {
    status.methods.iter().find_map(|method| match method.state {
        ProviderMethodState::Configured {
            control: CredentialControl::ExternalEnvironment,
            ..
        } => Some("Environment-backed credential: deactivate here; unset it outside Corbanu."),
        ProviderMethodState::Configured {
            control: CredentialControl::ExternalProvider,
            ..
        } => Some("Externally managed credential: deactivate here; remove it at its provider."),
        ProviderMethodState::Configured {
            control: CredentialControl::ManagedByCorbanu,
            ..
        } => Some("Deactivation keeps the managed credential; this screen never deletes it."),
        _ => None,
    })
}

fn interactive(capability: &ProviderSetupCapability) -> bool {
    matches!(
        capability,
        ProviderSetupCapability::OpenAiAccount
            | ProviderSetupCapability::ApiKey { .. }
            | ProviderSetupCapability::ClaudeAccount
            | ProviderSetupCapability::CorbanuPlan
    )
}

fn setup_label(status: &ProviderStatusSnapshot, capability: &ProviderSetupCapability) -> String {
    let verb = match status.configuration {
        ProviderConfigurationState::Configured => "Replace",
        ProviderConfigurationState::RecoveryRequired => "Recover",
        _ => "Set up",
    };
    format!("{verb} with {}", setup_description(capability))
}

fn setup_description(capability: &ProviderSetupCapability) -> String {
    match capability {
        ProviderSetupCapability::OpenAiAccount => "OpenAI account".to_string(),
        ProviderSetupCapability::ApiKey { .. } => "API key".to_string(),
        ProviderSetupCapability::ClaudeAccount => "Claude account".to_string(),
        ProviderSetupCapability::CorbanuPlan => "Corbanu API".to_string(),
        ProviderSetupCapability::Local { .. } => "local provider".to_string(),
        ProviderSetupCapability::CommandAuth { .. } => "external command".to_string(),
        ProviderSetupCapability::StatusOnly { .. } => "external configuration".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn type_keys(chat: &mut ChatWidget, keys: &[crossterm::event::KeyCode]) {
        for key in keys {
            chat.handle_key_event(crossterm::event::KeyEvent::new(
                *key,
                crossterm::event::KeyModifiers::NONE,
            ));
        }
    }

    /// #1289: the name prompt submits once input has been quiet for
    /// `SINGLE_LINE_SUBMIT_HOLD`; wait that out and run what the app's draw
    /// loop runs (paste-burst flush, then the pre-draw tick).
    fn wait_out_submit_hold(chat: &mut ChatWidget) {
        std::thread::sleep(
            crate::bottom_pane::custom_prompt_view::SINGLE_LINE_SUBMIT_HOLD
                + std::time::Duration::from_millis(50),
        );
        if !chat.handle_paste_burst_tick(crate::tui::FrameRequester::test_dummy()) {
            chat.pre_draw_tick();
        }
    }

    fn open_zai_name_prompt(chat: &mut ChatWidget) {
        chat.open_provider_account_name_prompt(
            "Z.AI",
            "zai".to_string(),
            /*rename_from*/ None,
            vec![crate::provider_named_accounts::AddAccountMethod::ApiKey],
        );
    }

    /// #1289: typing the name and pressing Enter at once (as fast as a paste)
    /// moves on to the key entry; the Enter is not taken as a newline.
    #[tokio::test]
    async fn add_account_name_enter_right_after_typing_submits() {
        use crossterm::event::KeyCode;
        let (mut chat, _tx, mut rx, _op_rx) =
            crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
        chat.open_provider_account_name_prompt(
            "Z.AI",
            "zai".to_string(),
            /*rename_from*/ None,
            vec![crate::provider_named_accounts::AddAccountMethod::ApiKey],
        );
        let mut keys: Vec<KeyCode> = "work".chars().map(KeyCode::Char).collect();
        keys.push(KeyCode::Enter);
        type_keys(&mut chat, &keys);
        wait_out_submit_hold(&mut chat);

        let Ok(AppEvent::ProviderAccount(ProviderAccountEvent::AddNamed {
            provider_id,
            name,
            method,
        })) = rx.try_recv()
        else {
            let popup =
                crate::chatwidget::tests::helpers::render_bottom_popup(&chat, /*width*/ 80);
            panic!("one Enter must submit the name; the prompt still shows:\n{popup}");
        };
        assert_eq!(
            (provider_id.as_str(), name.as_str(), method),
            (
                "zai",
                "work",
                crate::provider_named_accounts::AddAccountMethod::ApiKey
            )
        );
    }

    /// #1289: a name holding a newline (a paste) is refused before the key
    /// entry opens, and the refusal does not echo what was entered.
    #[tokio::test]
    async fn add_account_name_with_a_newline_is_refused_before_key_entry() {
        let (mut chat, _tx, mut rx, _op_rx) =
            crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
        chat.open_provider_account_name_prompt(
            "Z.AI",
            "zai".to_string(),
            /*rename_from*/ None,
            vec![crate::provider_named_accounts::AddAccountMethod::ApiKey],
        );
        chat.handle_paste("work\npf1289-not-a-real-key".to_string());
        let shown =
            crate::chatwidget::tests::helpers::render_bottom_popup(&chat, /*width*/ 80);
        assert!(
            !shown.contains("pf1289"),
            "only the first pasted line may be shown:\n{shown}"
        );
        type_keys(&mut chat, &[crossterm::event::KeyCode::Enter]);
        wait_out_submit_hold(&mut chat);

        let Ok(AppEvent::ProviderAccount(ProviderAccountEvent::Finished { result })) =
            rx.try_recv()
        else {
            panic!("a name with a newline must be refused");
        };
        let error = result.expect_err("refused");
        assert!(error.starts_with("invalid account name"), "{error}");
        assert!(!error.contains("pf1289"), "{error}");
        assert!(rx.try_recv().is_err(), "nothing may follow the refusal");
    }

    /// #1337 review: Ctrl+J (an editor newline key, and what a raw-mode
    /// terminal can report for a pasted line feed) submits the name like
    /// Enter. The key typed next goes to the masked entry, never in clear.
    #[tokio::test]
    async fn add_account_name_ctrl_j_submits_and_the_key_stays_masked() {
        use crossterm::event::KeyCode;
        use crossterm::event::KeyEvent;
        use crossterm::event::KeyModifiers;
        let (mut chat, _tx, mut rx, _op_rx) =
            crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
        open_zai_name_prompt(&mut chat);
        let keys: Vec<KeyCode> = "work".chars().map(KeyCode::Char).collect();
        type_keys(&mut chat, &keys);
        chat.handle_key_event(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        wait_out_submit_hold(&mut chat);

        let Ok(AppEvent::ProviderAccount(ProviderAccountEvent::AddNamed {
            provider_id,
            name,
            method,
        })) = rx.try_recv()
        else {
            let keys: Vec<KeyCode> = "sk-fake-0000".chars().map(KeyCode::Char).collect();
            type_keys(&mut chat, &keys);
            let popup =
                crate::chatwidget::tests::helpers::render_bottom_popup(&chat, /*width*/ 80);
            panic!("Ctrl+J must submit the name; after typing a key the prompt shows:\n{popup}");
        };
        // What the app does with AddNamed for an API-key account.
        chat.open_provider_account_value_entry("Z.AI", provider_id, name, method);
        let keys: Vec<KeyCode> = "sk-fake-0000".chars().map(KeyCode::Char).collect();
        type_keys(&mut chat, &keys);

        let shown =
            crate::chatwidget::tests::helpers::render_bottom_popup(&chat, /*width*/ 80);
        assert!(!shown.contains("sk-fake"), "{shown}");
        assert_eq!(chat.composer_text_with_pending(), "");
    }

    /// #1337 review: a key sent in the same burst as the name and its Enter
    /// reaches nothing: not the chat composer, not a view under the prompt.
    /// The prompt keeps it out, refuses with the fixed text and saves nothing.
    #[tokio::test]
    async fn add_account_name_keys_right_after_enter_reach_nothing() {
        use crossterm::event::KeyCode;
        let (mut chat, _tx, mut rx, _op_rx) =
            crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
        open_zai_name_prompt(&mut chat);
        let mut keys: Vec<KeyCode> = "work".chars().map(KeyCode::Char).collect();
        keys.push(KeyCode::Enter);
        keys.extend("sk-fake-0000".chars().map(KeyCode::Char));
        type_keys(&mut chat, &keys);

        let shown =
            crate::chatwidget::tests::helpers::render_bottom_popup(&chat, /*width*/ 80);
        assert!(!shown.contains("sk-fake"), "{shown}");
        wait_out_submit_hold(&mut chat);

        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        let shown =
            crate::chatwidget::tests::helpers::render_bottom_popup(&chat, /*width*/ 80);
        assert_eq!(
            chat.composer_text_with_pending(),
            "",
            "events: {events:?}\n{shown}"
        );
        assert!(!shown.contains("sk-fake"), "{shown}");
        assert!(!chat.has_active_view(), "{shown}");
        let [AppEvent::ProviderAccount(ProviderAccountEvent::Finished { result: Err(error) })] =
            events.as_slice()
        else {
            panic!("only the refusal may be sent: {events:?}");
        };
        assert!(error.starts_with("invalid account name"), "{error}");
        assert!(!error.contains("sk-fake"), "{error}");
    }

    #[tokio::test]
    async fn configured_credentials_can_be_replaced_while_active_or_inactive() {
        for provider in ["claude-plan", "anthropic", "openai"] {
            for eligibility in [
                ProviderEligibilityState::Active,
                ProviderEligibilityState::Inactive,
            ] {
                let (mut chat, _tx, mut rx, _op_rx) =
                    crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
                let catalog =
                    ProviderCatalog::from_runtime_providers(&chat.config_ref().model_providers);
                let entry = catalog.get(provider).expect("built-in provider");
                let status = ProviderStatusSnapshot {
                    id: entry.id.clone(),
                    methods: Vec::new(),
                    configuration: ProviderConfigurationState::Configured,
                    eligibility,
                    current: ProviderCurrentState::NotCurrent,
                    availability: ProviderAvailabilityState::Ready,
                };
                chat.open_provider_manager_actions(entry, &status, /*account_provider*/ None);
                insta::assert_snapshot!(
                    format!("provider_replace_{provider}_{eligibility:?}"),
                    crate::chatwidget::tests::helpers::render_bottom_popup(
                        &chat, /*width*/ 90
                    )
                );
                for key in [
                    crossterm::event::KeyCode::Down,
                    crossterm::event::KeyCode::Enter,
                ] {
                    chat.handle_key_event(crossterm::event::KeyEvent::new(
                        key,
                        crossterm::event::KeyModifiers::NONE,
                    ));
                }
                let event = rx.try_recv().expect("replacement action");
                let AppEvent::ProviderManagerBeginAuthentication {
                    provider_id,
                    capability,
                } = event
                else {
                    panic!("replacement must start authentication");
                };
                assert_eq!(provider_id, entry.id);
                assert_eq!(capability, entry.setup_capabilities.primary);
            }
        }
    }

    #[tokio::test]
    async fn provider_manager_uses_shared_status_copy_snapshot() {
        let (mut chat, _tx, _rx, _op_rx) =
            crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
        let catalog = ProviderCatalog::from_runtime_providers(&chat.config_ref().model_providers);
        let statuses = catalog
            .entries()
            .iter()
            .take(3)
            .enumerate()
            .map(|(index, entry)| ProviderStatusSnapshot {
                id: entry.id.clone(),
                methods: Vec::new(),
                configuration: if index == 2 {
                    ProviderConfigurationState::RecoveryRequired
                } else {
                    ProviderConfigurationState::Configured
                },
                eligibility: if index == 1 {
                    ProviderEligibilityState::Inactive
                } else {
                    ProviderEligibilityState::Active
                },
                current: if index == 0 {
                    ProviderCurrentState::Current
                } else {
                    ProviderCurrentState::NotCurrent
                },
                availability: ProviderAvailabilityState::Ready,
            })
            .collect::<Vec<_>>();
        chat.open_provider_manager(
            &catalog, &statuses, /*focused_provider*/ None, /*accounts*/ None,
        );
        let rendered =
            crate::chatwidget::tests::helpers::render_bottom_popup(&chat, /*width*/ 80);
        insta::assert_snapshot!("provider_manager_shared_status", rendered);
    }

    /// PF-84-S04: one row per (provider, account), with markers and actions.
    #[tokio::test]
    async fn provider_manager_lists_one_row_per_account() {
        use crate::provider_named_accounts::AccountsView;
        use crate::provider_named_accounts::NamedAccountRow;
        let (mut chat, _tx, _rx, _op_rx) =
            crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
        let catalog = ProviderCatalog::from_runtime_providers(&chat.config_ref().model_providers);
        let zai = catalog.get("zai").expect("zai entry").clone();
        let claude = catalog.get("claude-plan").expect("claude entry").clone();
        let statuses = [&claude, &zai]
            .iter()
            .map(|entry| ProviderStatusSnapshot {
                id: entry.id.clone(),
                methods: Vec::new(),
                configuration: ProviderConfigurationState::Configured,
                eligibility: ProviderEligibilityState::Active,
                current: ProviderCurrentState::NotCurrent,
                availability: ProviderAvailabilityState::Ready,
            })
            .collect::<Vec<_>>();
        let account = |provider: &str, name: &str, kind, fingerprint: &str| NamedAccountRow {
            provider_id: provider.to_string(),
            name: codex_vault::ProviderAccountName::parse(name).unwrap(),
            kinds: vec![kind],
            fingerprint: Some(fingerprint.to_string()),
            aws_profile: None,
            sign_in: None,
        };
        let accounts = AccountsView {
            rows: vec![
                account(
                    "claude-plan",
                    "work",
                    codex_vault::ProviderAccountKind::ClaudeOauthToken,
                    "a1b2c3d4e5f6",
                ),
                account(
                    "zai",
                    "fake",
                    codex_vault::ProviderAccountKind::ApiKey,
                    "0f1e2d3c4b5a",
                ),
            ],
            session: std::collections::BTreeMap::from([("zai".to_string(), "fake".to_string())]),
            defaults: std::collections::BTreeMap::from([(
                "claude-plan".to_string(),
                "work".to_string(),
            )]),
            status_accounts: std::collections::BTreeMap::from([(
                "zai".to_string(),
                "fake".to_string(),
            )]),
            status_account_details: std::collections::BTreeMap::new(),
        };
        let rows = provider_manager_rows(&catalog, &statuses, Some(&accounts));
        assert_eq!(rows.len(), 4, "{rows:?}");
        assert_eq!(
            provider_manager_rows(&catalog, &statuses, /*accounts*/ None).len(),
            2
        );
        chat.open_provider_manager(
            &catalog,
            &statuses,
            /*focused_provider*/ None,
            Some(&accounts),
        );
        let rendered =
            crate::chatwidget::tests::helpers::render_bottom_popup(&chat, /*width*/ 100);
        insta::assert_snapshot!("provider_manager_named_accounts", rendered);
        // #609: the row names an account only when the status host that
        // computed the statuses says that account decided them, whichever
        // account this session runs on.
        let unqualified = AccountsView {
            status_accounts: std::collections::BTreeMap::new(),
            ..accounts.clone()
        };
        chat.open_provider_manager(
            &catalog,
            &statuses,
            /*focused_provider*/ None,
            Some(&unqualified),
        );
        let rendered =
            crate::chatwidget::tests::helpers::render_bottom_popup(&chat, /*width*/ 100);
        assert!(!rendered.contains("status of account"), "{rendered}");
        // #609: and what that account holds, when the status host knows it.
        let detailed = AccountsView {
            status_account_details: std::collections::BTreeMap::from([(
                "zai".to_string(),
                "an OpenAI API key".to_string(),
            )]),
            ..accounts.clone()
        };
        chat.open_provider_manager(
            &catalog,
            &statuses,
            /*focused_provider*/ None,
            Some(&detailed),
        );
        let rendered =
            crate::chatwidget::tests::helpers::render_bottom_popup(&chat, /*width*/ 200);
        assert!(
            rendered.contains("(status of account `fake`, which holds an OpenAI API key)"),
            "{rendered}"
        );
        chat.open_provider_manager(
            &catalog,
            &statuses,
            /*focused_provider*/ None,
            Some(&accounts),
        );

        chat.open_provider_account_actions(&zai, &accounts.rows[1], &accounts);
        let rendered =
            crate::chatwidget::tests::helpers::render_bottom_popup(&chat, /*width*/ 100);
        insta::assert_snapshot!("provider_manager_account_actions", rendered);
        // D174: choosing another account starts a new session, and says so.
        chat.open_provider_account_actions(&claude, &accounts.rows[0], &accounts);
        let rendered =
            crate::chatwidget::tests::helpers::render_bottom_popup(&chat, /*width*/ 120);
        insta::assert_snapshot!("provider_manager_account_actions_new_session", rendered);
        let mut items = Vec::new();
        push_default_account_items(&mut items, "zai", &accounts);
        assert!(
            items
                .iter()
                .any(|item| item.name == "Start a new session on the default account"),
            "{:?}",
            items
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>()
        );

        chat.open_provider_account_removal(
            &zai,
            "zai".to_string(),
            accounts.rows[1].name.clone(),
            &accounts,
        );
        let rendered =
            crate::chatwidget::tests::helpers::render_bottom_popup(&chat, /*width*/ 100);
        assert!(
            rendered.contains("choose the account that replaces it first")
                || rendered.contains("Choose the account that replaces it first"),
            "{rendered}"
        );
        assert!(rendered.contains("Replace with `default`"), "{rendered}");
    }

    #[tokio::test]
    async fn refresh_preserves_provider_selection_by_identity_and_falls_back_when_removed() {
        let (mut chat, _tx, _rx, _op_rx) =
            crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
        let catalog = ProviderCatalog::from_runtime_providers(&chat.config_ref().model_providers);
        let statuses = catalog
            .entries()
            .iter()
            .take(3)
            .map(|entry| ProviderStatusSnapshot {
                id: entry.id.clone(),
                methods: Vec::new(),
                configuration: ProviderConfigurationState::Configured,
                eligibility: ProviderEligibilityState::Active,
                current: ProviderCurrentState::NotCurrent,
                availability: ProviderAvailabilityState::Ready,
            })
            .collect::<Vec<_>>();
        let focused_provider = statuses[2].id.clone();

        chat.open_provider_manager(
            &catalog, &statuses, /*focused_provider*/ None, /*accounts*/ None,
        );
        chat.open_provider_manager(
            &catalog,
            &statuses,
            Some(&focused_provider),
            /*accounts*/ None,
        );
        assert_eq!(chat.provider_manager_selected_index(), Some(2));

        chat.open_provider_manager(
            &catalog,
            &statuses[..2],
            Some(&focused_provider),
            /*accounts*/ None,
        );
        assert_eq!(chat.provider_manager_selected_index(), Some(0));
    }

    #[tokio::test]
    async fn opening_provider_actions_keeps_manager_as_the_parent_view() {
        let (mut chat, _tx, mut rx, _op_rx) =
            crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
        let catalog = ProviderCatalog::from_runtime_providers(&chat.config_ref().model_providers);
        let entry = &catalog.entries()[0];
        let statuses = [ProviderStatusSnapshot {
            id: entry.id.clone(),
            methods: Vec::new(),
            configuration: ProviderConfigurationState::Configured,
            eligibility: ProviderEligibilityState::Active,
            current: ProviderCurrentState::NotCurrent,
            availability: ProviderAvailabilityState::Ready,
        }];
        chat.open_provider_manager(
            &catalog, &statuses, /*focused_provider*/ None, /*accounts*/ None,
        );

        chat.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));

        assert!(matches!(
            rx.try_recv(),
            Ok(AppEvent::OpenProviderManagerActions { provider_id }) if provider_id == entry.id
        ));
        assert_eq!(chat.provider_manager_selected_index(), Some(0));
    }

    /// #1288: /providers shows eight rows at a time; when there are more, the
    /// popup says which row is selected out of how many.
    #[tokio::test]
    async fn provider_manager_shows_position_when_rows_exceed_the_window() {
        let (mut chat, _tx, _rx, _op_rx) =
            crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
        let catalog = ProviderCatalog::from_runtime_providers(&chat.config_ref().model_providers);
        let statuses = catalog
            .entries()
            .iter()
            .map(|entry| ProviderStatusSnapshot {
                id: entry.id.clone(),
                methods: Vec::new(),
                configuration: ProviderConfigurationState::Configured,
                eligibility: ProviderEligibilityState::Active,
                current: ProviderCurrentState::NotCurrent,
                availability: ProviderAvailabilityState::Ready,
            })
            .collect::<Vec<_>>();
        let total = provider_manager_rows(&catalog, &statuses, /*accounts*/ None).len();
        assert!(
            total > crate::bottom_pane::popup_consts::MAX_POPUP_ROWS,
            "{total} rows"
        );

        chat.open_provider_manager(
            &catalog, &statuses, /*focused_provider*/ None, /*accounts*/ None,
        );
        let rendered =
            crate::chatwidget::tests::helpers::render_bottom_popup(&chat, /*width*/ 80);
        assert!(rendered.contains(&format!("1 / {total}")), "{rendered}");

        chat.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Down,
            crossterm::event::KeyModifiers::NONE,
        ));
        let rendered =
            crate::chatwidget::tests::helpers::render_bottom_popup(&chat, /*width*/ 80);
        assert!(rendered.contains(&format!("2 / {total}")), "{rendered}");

        chat.open_provider_manager(
            &catalog,
            &statuses[..3],
            /*focused_provider*/ None,
            /*accounts*/ None,
        );
        let rendered =
            crate::chatwidget::tests::helpers::render_bottom_popup(&chat, /*width*/ 80);
        assert!(!rendered.contains(" / 3"), "{rendered}");
    }

    /// Ollama and LM Studio are listed without any probe, so their rows and
    /// action header say "local (not checked)" rather than "configured".
    #[tokio::test]
    async fn provider_manager_marks_local_runtimes_not_checked() {
        let (mut chat, _tx, _rx, _op_rx) =
            crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
        let catalog = ProviderCatalog::from_runtime_providers(&chat.config_ref().model_providers);
        let mut metadata = codex_provider_auth::ProviderMetadataSnapshot::default();
        for id in [
            codex_model_provider_info::OLLAMA_OSS_PROVIDER_ID,
            codex_model_provider_info::LMSTUDIO_OSS_PROVIDER_ID,
        ] {
            metadata.insert(
                catalog.get(id).expect("local runtime entry"),
                codex_provider_auth::ProviderMetadata::Local(
                    codex_provider_auth::LocalProviderMetadata::Available,
                ),
            );
        }
        let resolved = codex_provider_auth::ProviderStatusResolver::resolve(
            &catalog,
            &metadata,
            &codex_provider_auth::ProviderEligibilitySnapshot::Loaded(Default::default()),
            &codex_provider_auth::CurrentProviderSelection::None,
        );
        let openai = catalog
            .get(codex_model_provider_info::OPENAI_PROVIDER_ID)
            .expect("OpenAI entry");
        let statuses = vec![
            ProviderStatusSnapshot {
                id: openai.id.clone(),
                methods: Vec::new(),
                configuration: ProviderConfigurationState::Configured,
                eligibility: ProviderEligibilityState::Active,
                current: ProviderCurrentState::NotCurrent,
                availability: ProviderAvailabilityState::Ready,
            },
            resolved
                .get(codex_model_provider_info::OLLAMA_OSS_PROVIDER_ID)
                .expect("Ollama status")
                .clone(),
            resolved
                .get(codex_model_provider_info::LMSTUDIO_OSS_PROVIDER_ID)
                .expect("LM Studio status")
                .clone(),
        ];

        chat.open_provider_manager(
            &catalog, &statuses, /*focused_provider*/ None, /*accounts*/ None,
        );
        insta::assert_snapshot!(
            "provider_manager_local_runtimes",
            crate::chatwidget::tests::helpers::render_bottom_popup(&chat, /*width*/ 80)
        );

        let ollama = catalog
            .get(codex_model_provider_info::OLLAMA_OSS_PROVIDER_ID)
            .expect("Ollama entry");
        chat.open_provider_manager_actions(ollama, &statuses[1], /*account_provider*/ None);
        insta::assert_snapshot!(
            "provider_actions_local_runtime",
            crate::chatwidget::tests::helpers::render_bottom_popup(&chat, /*width*/ 80)
        );
    }

    #[test]
    fn external_credential_copy_never_claims_deletion() {
        let status = ProviderStatusSnapshot {
            id: ProviderCatalog::from_runtime_providers(
                &codex_model_provider_info::built_in_model_providers(/*openai_base_url*/ None),
            )
            .entries()[0]
                .id
                .clone(),
            methods: vec![codex_provider_auth::ProviderMethodStatus {
                capability: ProviderSetupCapability::OpenAiAccount,
                state: ProviderMethodState::Configured {
                    source: codex_provider_auth::ProviderCredentialSource::Environment,
                    control: CredentialControl::ExternalEnvironment,
                    availability: codex_provider_auth::ConfiguredAvailability::Ready,
                },
            }],
            configuration: ProviderConfigurationState::Configured,
            eligibility: ProviderEligibilityState::Active,
            current: ProviderCurrentState::NotCurrent,
            availability: ProviderAvailabilityState::Ready,
        };
        let copy = credential_control_note(&status).unwrap();
        assert!(copy.contains("unset it outside Corbanu"));
        assert!(!copy.to_ascii_lowercase().contains("delete"));
    }
}
