use codex_provider_auth::ProviderManagementPhase;

use super::App;
use super::AppEvent;
use super::AppServerSession;

pub(super) fn spawn_provider_status_job(job: impl FnOnce() + Send + 'static) {
    tokio::task::spawn_blocking(job);
}

/// PF-84-S04: named-account rows for `/providers` while the feature is on.
/// A vault that cannot be read shows no rows rather than blocking the screen.
fn load_named_accounts(
    enabled: bool,
    codex_home: &std::path::Path,
) -> Option<Vec<crate::provider_named_accounts::NamedAccountRow>> {
    enabled.then(|| {
        crate::provider_named_accounts::load_rows(codex_home).unwrap_or_else(|error| {
            tracing::warn!("{error}");
            Vec::new()
        })
    })
}

pub(super) fn provider_manager_status_host(
    config: &crate::legacy_core::config::Config,
    shared: Option<crate::provider_status_host::ProviderStatusHost>,
) -> crate::provider_status_host::ProviderStatusHost {
    shared.unwrap_or_else(|| {
        crate::provider_status_host::ProviderStatusHost::from_config(
            config,
            crate::provider_status_host::ProviderAccountMetadata {
                claude: codex_provider_auth::ClaudeCredentialMetadata::Checking,
                ..Default::default()
            },
        )
    })
}

fn sync_provider_manager_model_policy(
    catalog: &crate::model_catalog::ModelCatalog,
    config: &crate::legacy_core::config::Config,
) {
    catalog.sync_runtime_models(
        config.model_providers.keys().map(String::as_str),
        config.model.as_deref(),
        &config.model_provider_id,
    );
    catalog.refresh_provider_policy();
}

/// PF-84-S06 (#572): Bedrock's models are listed for a session on Bedrock
/// only, because its default account has no setup in the TUI. A named
/// Bedrock account (an AWS profile) can be chosen from a session on another
/// provider (`/providers` → *Start a new session on this account*, `--account
/// amazon-bedrock:<name>` or `[provider_accounts]`), so while the vault holds
/// one, `/model` offers Bedrock's models too; they go again once none is
/// left. The vault read blocks, so it runs off the runtime and reports back
/// with [`AppEvent::NamedBedrockAccountChecked`]; only the newest check is
/// applied, on the UI thread.
pub(super) fn check_named_bedrock_account(
    catalog: &crate::model_catalog::ModelCatalog,
    config: &crate::legacy_core::config::Config,
    tx: &crate::app_event_sender::AppEventSender,
) {
    let generation = catalog.begin_provider_models_check();
    let config = config.clone();
    let tx = tx.clone();
    spawn_provider_status_job(move || {
        let offered = holds_named_bedrock_account(&config);
        tx.send(AppEvent::NamedBedrockAccountChecked {
            generation,
            offered,
        });
    });
}

/// Whether a named Bedrock account can be reached from this session's
/// config (feature on, Bedrock configured, an AWS-profile account in the
/// vault); `None` when the vault cannot be read. Blocking (it reads the vault
/// index); no credential value is read.
pub(super) fn holds_named_bedrock_account(
    config: &crate::legacy_core::config::Config,
) -> Option<bool> {
    use codex_model_provider_info::AMAZON_BEDROCK_PROVIDER_ID;
    if !config
        .features
        .enabled(codex_features::Feature::NamedAccounts)
        || !config
            .model_providers
            .get(AMAZON_BEDROCK_PROVIDER_ID)
            .is_some_and(codex_model_provider_info::ModelProviderInfo::is_amazon_bedrock)
    {
        return Some(false);
    }
    crate::provider_named_accounts::holds_named_account(
        &config.codex_home,
        AMAZON_BEDROCK_PROVIDER_ID,
        codex_vault::ProviderAccountKind::AwsProfile,
    )
}

/// Offers Bedrock's models (or withdraws the ones offered earlier, except
/// `current_model`, the Bedrock model the session is on).
pub(super) fn apply_named_bedrock_models(
    catalog: &crate::model_catalog::ModelCatalog,
    offered: bool,
    current_model: Option<&str>,
) {
    catalog.set_provider_models(
        codex_model_provider_info::AMAZON_BEDROCK_PROVIDER_ID,
        if offered {
            codex_model_provider::amazon_bedrock_model_presets()
        } else {
            Vec::new()
        },
        current_model,
    );
}

impl App {
    /// Applies the newest named-Bedrock-account check (#572); withdrawing
    /// Bedrock's models keeps the one the session is on.
    pub(super) fn named_bedrock_account_checked(&mut self, generation: u64, offered: Option<bool>) {
        let Some(offered) = offered else {
            return;
        };
        if !self
            .model_catalog
            .is_latest_provider_models_check(generation)
        {
            return;
        }
        let on_bedrock =
            self.config.model_provider_id == codex_model_provider_info::AMAZON_BEDROCK_PROVIDER_ID;
        let current_model = on_bedrock.then(|| self.chat_widget.current_model().to_string());
        apply_named_bedrock_models(&self.model_catalog, offered, current_model.as_deref());
        self.model_catalog.refresh_provider_policy();
    }

    pub(super) fn reusable_provider_status_host(
        &self,
    ) -> Option<crate::provider_status_host::ProviderStatusHost> {
        self.shared_provider_status_host.clone().or_else(|| {
            self.model_catalog
                .provider_policy()
                .map(|policy| policy.host())
        })
    }

    pub(super) fn open_provider_manager(&mut self, _app_server: &AppServerSession) {
        sync_provider_manager_model_policy(&self.model_catalog, &self.config);
        check_named_bedrock_account(&self.model_catalog, &self.config, &self.app_event_tx);
        self.check_named_accounts();
        let generation = self.next_provider_management_generation();
        self.provider_management_host = None;
        let config = self.config.clone();
        let shared_status_host = self.reusable_provider_status_host();
        let tx = self.app_event_tx.clone();
        spawn_provider_status_job(move || {
            let status_host = provider_manager_status_host(&config, shared_status_host);
            let statuses = status_host.resolve().entries().to_vec();
            let accounts = load_named_accounts(
                config
                    .features
                    .enabled(codex_features::Feature::NamedAccounts),
                &config.codex_home,
            );
            tx.send(AppEvent::ProviderManagerStatusesResolved {
                generation,
                status_host,
                statuses,
                accounts,
            });
        });
    }

    pub(super) fn provider_manager_statuses_resolved(
        &mut self,
        generation: u64,
        status_host: crate::provider_status_host::ProviderStatusHost,
        statuses: Vec<codex_provider_auth::ProviderStatusSnapshot>,
        accounts: Option<Vec<crate::provider_named_accounts::NamedAccountRow>>,
        app_server: &AppServerSession,
    ) {
        if generation != self.provider_management_generation {
            return;
        }
        if self
            .provider_management_host
            .as_ref()
            .is_some_and(|host| !matches!(host.phase(), ProviderManagementPhase::Browsing))
        {
            return;
        }
        // The asynchronous discovery may have settled after the picker cached
        // its eligibility snapshot. Publish the same status result to both UIs.
        self.model_catalog.update_provider_statuses(&statuses);
        let selected_index = self.chat_widget.provider_manager_selected_index();
        let selected_provider =
            selected_index.and_then(|index| self.provider_manager_row_provider(index));
        if let Some(host) = self.provider_management_host.as_mut() {
            if let Some(provider_id) = selected_provider {
                host.remember_focused_provider(provider_id);
            }
            let accounts_changed = host.named_accounts() != accounts.as_deref();
            host.set_named_accounts(accounts);
            // A late discovery result still refreshes policy, but must not
            // reopen a manager the user dismissed (for example to open /model).
            if (host.apply_statuses(statuses).applied || accounts_changed)
                && selected_index.is_some()
            {
                self.render_provider_manager();
            }
            return;
        }
        let host = crate::provider_management_host::ProviderManagementHost::new_with_statuses(
            &self.config,
            app_server.request_handle(),
            self.app_event_tx.clone(),
            status_host,
            statuses,
        );
        let mut host = host;
        host.set_named_accounts(accounts);
        self.provider_management_host = Some(host);
        self.render_provider_manager();
        let config = self.config.clone();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let metadata =
                crate::provider_status_host::ProviderAccountMetadata::discover(&config).await;
            tx.send(AppEvent::ProviderManagerMetadataResolved {
                generation,
                metadata,
            });
        });
    }

    fn next_provider_management_generation(&mut self) -> u64 {
        self.provider_management_generation = self.provider_management_generation.wrapping_add(1);
        if self.provider_management_generation == 0 {
            self.provider_management_generation = 1;
        }
        self.provider_management_generation
    }

    pub(super) fn schedule_provider_manager_refresh(&mut self) {
        let Some(host) = self.provider_management_host.as_ref() else {
            return;
        };
        if !matches!(host.phase(), ProviderManagementPhase::Browsing) {
            return;
        }
        let status_host = host.status_host().clone();
        let generation = self.next_provider_management_generation();
        let worker_host = status_host.clone();
        let tx = self.app_event_tx.clone();
        let named_accounts_enabled = self
            .config
            .features
            .enabled(codex_features::Feature::NamedAccounts);
        let codex_home = self.config.codex_home.to_path_buf();
        spawn_provider_status_job(move || {
            let statuses = worker_host.resolve().entries().to_vec();
            let accounts = load_named_accounts(named_accounts_enabled, &codex_home);
            tx.send(AppEvent::ProviderManagerStatusesResolved {
                generation,
                status_host,
                statuses,
                accounts,
            });
        });
    }

    pub(super) fn render_provider_manager(&mut self) {
        let Some(host) = self.provider_management_host.as_ref() else {
            return;
        };
        let catalog = host.status_host().catalog().clone();
        let statuses = host.statuses().to_vec();
        let focused_provider = host.focused_provider().cloned();
        let accounts = self.provider_manager_accounts_view();
        self.chat_widget.open_provider_manager(
            &catalog,
            &statuses,
            focused_provider.as_ref(),
            accounts.as_ref(),
        );
    }

    /// PF-84-S04: the accounts `/providers` shows; `None` while the feature is off.
    pub(super) fn provider_manager_accounts_view(
        &self,
    ) -> Option<crate::provider_named_accounts::AccountsView> {
        let host = self.provider_management_host.as_ref()?;
        let rows = host.named_accounts()?;
        Some(
            crate::provider_named_accounts::AccountsView::new(
                rows.to_vec(),
                self.chat_widget.config_ref(),
                &self.config,
            )
            .with_status_accounts(host.status_host()),
        )
    }

    /// The catalog provider of a `/providers` row, account rows included.
    pub(super) fn provider_manager_row_provider(
        &self,
        index: usize,
    ) -> Option<codex_provider_auth::ProviderCatalogId> {
        let host = self.provider_management_host.as_ref()?;
        let accounts = self.provider_manager_accounts_view();
        crate::chatwidget::provider_manager_rows(
            host.status_host().catalog(),
            host.statuses(),
            accounts.as_ref(),
        )
        .get(index)
        .map(|row| row.provider_id().clone())
    }

    pub(super) fn provider_manager_metadata_resolved(
        &mut self,
        generation: u64,
        metadata: crate::provider_status_host::ProviderAccountMetadata,
    ) {
        if generation != self.provider_management_generation {
            return;
        }
        let can_refresh = self
            .provider_management_host
            .as_mut()
            .is_some_and(|host| host.update_account_metadata(metadata));
        if can_refresh {
            self.schedule_provider_manager_refresh();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn manager_open_syncs_configured_runtime_models_before_policy_refresh() {
        let mut config = crate::legacy_core::config::ConfigBuilder::default()
            .build()
            .await
            .unwrap();
        config.model = Some("shared-model".to_string());
        config.model_provider_id = "manager-added".to_string();
        config.model_providers.insert(
            "manager-added".to_string(),
            codex_model_provider_info::ModelProviderInfo {
                name: "Manager Added".to_string(),
                ..Default::default()
            },
        );
        let catalog = crate::model_catalog::ModelCatalog::new(Vec::new());

        sync_provider_manager_model_policy(&catalog, &config);
        sync_provider_manager_model_policy(&catalog, &config);

        let matches = catalog
            .try_list_models()
            .unwrap()
            .into_iter()
            .filter(|preset| preset.provider_id.as_deref() == Some("manager-added"))
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].model, "shared-model");
    }

    /// #1193: the background check caches which providers have named
    /// accounts (this one off the UI thread); with named accounts off it
    /// does not run.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn named_account_check_caches_the_answer_off_the_ui_thread() {
        let home = tempfile::TempDir::new().unwrap();
        let mut config = crate::legacy_core::config::ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .build()
            .await
            .unwrap();
        codex_vault::Vault::new(home.path().to_path_buf())
            .create_provider_account(
                "zai",
                &codex_vault::ProviderAccountName::parse("work").unwrap(),
                codex_vault::ProviderAccountKind::ApiKey,
                "test-key-not-a-secret",
            )
            .unwrap();
        let catalog = std::sync::Arc::new(crate::model_catalog::ModelCatalog::new(Vec::new()));

        config
            .features
            .disable(codex_features::Feature::NamedAccounts)
            .unwrap();
        crate::provider_named_accounts::check_named_account_providers(&catalog, &config);
        assert_eq!(catalog.begin_named_account_check(), 1, "no check ran");

        config
            .features
            .enable(codex_features::Feature::NamedAccounts)
            .unwrap();
        crate::provider_named_accounts::check_named_account_providers(&catalog, &config);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while catalog.provider_has_named_account("zai").is_none() {
            assert!(std::time::Instant::now() < deadline, "check never answered");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(catalog.provider_has_named_account("zai"), Some(true));
        assert_eq!(catalog.provider_has_named_account("kimi"), Some(false));
    }
}
