use std::collections::BTreeSet;

use codex_protocol::openai_models::ModelPreset;
use codex_provider_auth::CurrentSelectionDecision;
use codex_provider_auth::ProviderRuntimeAuthorizations;
use codex_provider_auth::ProviderRuntimeSelectionPolicy;
use codex_provider_auth::ProviderStatusCatalog;
use codex_provider_auth::ProviderUseBlocker;
use codex_provider_auth::ProviderUseContext;
use codex_provider_auth::ProviderUseDecision;

use crate::provider_status_host::ProviderStatusHost;

/// Explains why a turn cannot use the current provider.
///
/// A locked login keychain, typical of an SSH session, makes stored provider
/// logins unreadable. That looks like an unavailable provider, but the repair is
/// to unlock the keychain rather than to change anything in `/providers`.
pub(crate) fn current_provider_blocked_message() -> String {
    if codex_keyring_store::default_keychain_locked() {
        "Your macOS login keychain is locked, so saved provider logins can't be read. This is \
         usual over SSH. Run `security unlock-keychain login.keychain-db`, then restart Corbanu."
            .to_string()
    } else {
        "The current provider is unavailable or inactive. Choose an active provider and model, \
         or repair it in /providers."
            .to_string()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ProviderModelPolicy {
    host: ProviderStatusHost,
    statuses: ProviderStatusCatalog,
    authorizations: ProviderRuntimeAuthorizations,
    /// #665: connections that cannot authenticate although their provider
    /// entry is ready (see `ProviderStatusHost::connections_without_credential`):
    /// not offered, and a session on one is asked to recover.
    without_credential: BTreeSet<String>,
}

impl ProviderModelPolicy {
    pub(crate) fn new(
        host: ProviderStatusHost,
        authorizations: ProviderRuntimeAuthorizations,
    ) -> Self {
        let statuses = host.resolve();
        let without_credential = host.connections_without_credential();
        Self {
            host,
            statuses,
            authorizations,
            without_credential,
        }
    }

    pub(crate) fn host(&self) -> ProviderStatusHost {
        self.host.clone()
    }

    pub(crate) fn refresh(&mut self) {
        self.statuses = self.host.resolve();
        self.without_credential = self.host.connections_without_credential();
    }

    pub(crate) fn update_statuses(
        &mut self,
        statuses: &[codex_provider_auth::ProviderStatusSnapshot],
    ) {
        self.statuses.update(statuses);
    }

    pub(crate) fn set_current_runtime(&mut self, runtime_provider_id: &str) {
        self.host
            .set_current_runtime(runtime_provider_id.to_string());
        self.refresh();
    }

    pub(crate) fn assess(
        &self,
        runtime_provider_id: &str,
        model: &str,
        context: ProviderUseContext,
    ) -> ProviderUseDecision {
        let decision = ProviderRuntimeSelectionPolicy::assess(
            self.host.catalog(),
            &self.statuses,
            &self.authorizations,
            runtime_provider_id,
            model,
            context,
        );
        match decision {
            ProviderUseDecision::Ready(_)
            | ProviderUseDecision::RequiresRuntimeAuthorization(_)
                if self.without_credential.contains(runtime_provider_id) =>
            {
                ProviderUseDecision::Blocked {
                    requested_runtime_provider_id: runtime_provider_id.to_string(),
                    reason: ProviderUseBlocker::NotConfigured,
                }
            }
            decision => decision,
        }
    }

    pub(crate) fn current(
        &self,
        runtime_provider_id: &str,
        model: &str,
    ) -> CurrentSelectionDecision {
        match ProviderRuntimeSelectionPolicy::current(
            self.host.catalog(),
            &self.statuses,
            &self.authorizations,
            runtime_provider_id,
            model,
        ) {
            CurrentSelectionDecision::Preserve(_)
                if self.without_credential.contains(runtime_provider_id) =>
            {
                CurrentSelectionDecision::RequireExplicitRecovery {
                    requested_runtime_provider_id: runtime_provider_id.to_string(),
                    requested_model: model.to_string(),
                    reason: ProviderUseBlocker::NotConfigured,
                }
            }
            decision => decision,
        }
    }

    pub(crate) fn preset_is_selectable(&self, preset: &ModelPreset) -> bool {
        let Some(provider) = preset
            .provider_id
            .as_deref()
            .or_else(|| codex_model_provider_info::canonical_catalog_provider(&preset.model))
        else {
            return false;
        };
        matches!(
            self.assess(provider, &preset.model, ProviderUseContext::ModelPicker),
            ProviderUseDecision::Ready(_)
        )
    }

    pub(crate) fn provider_is_selectable(&self, provider: &str, model: &str) -> bool {
        matches!(
            self.assess(provider, model, ProviderUseContext::ModelPicker),
            ProviderUseDecision::Ready(_)
        )
    }

    /// #1563: the `/providers` name of the current provider's entry when the
    /// turn gate refuses it as not configured.
    pub(crate) fn not_configured_entry_name(
        &self,
        runtime_provider_id: &str,
        model: &str,
    ) -> Option<String> {
        if !matches!(
            self.current(runtime_provider_id, model),
            CurrentSelectionDecision::RequireExplicitRecovery {
                reason: ProviderUseBlocker::NotConfigured,
                ..
            }
        ) {
            return None;
        }
        let canonical = codex_model_provider_info::canonical_provider_id(runtime_provider_id);
        self.host
            .catalog()
            .entries()
            .iter()
            .find(|entry| {
                entry.runtime_provider_ids.iter().any(|id| {
                    id.as_str() == runtime_provider_id
                        || codex_model_provider_info::canonical_provider_id(id.as_str())
                            == canonical
                })
            })
            .map(|entry| entry.display_name.clone())
    }

    pub(crate) fn has_ready_configured_provider(&self) -> bool {
        self.statuses.entries().iter().any(|status| {
            status.configuration == codex_provider_auth::ProviderConfigurationState::Configured
                && status.eligibility == codex_provider_auth::ProviderEligibilityState::Active
                && status.availability == codex_provider_auth::ProviderAvailabilityState::Ready
                && self
                    .host
                    .catalog()
                    .get(status.id.as_str())
                    .is_some_and(|entry| {
                        entry.setup_capabilities.iter().any(|capability| {
                            matches!(
                                capability,
                                codex_provider_auth::ProviderSetupCapability::ApiKey { .. }
                                    | codex_provider_auth::ProviderSetupCapability::OpenAiAccount
                                    | codex_provider_auth::ProviderSetupCapability::ClaudeAccount
                                    | codex_provider_auth::ProviderSetupCapability::CorbanuPlan
                            )
                        })
                    })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_model_provider_info::ModelProviderInfo;
    use tempfile::tempdir;

    #[tokio::test]
    async fn custom_provider_selection_tracks_shared_active_policy() {
        let home = tempdir().unwrap();
        let env_key = "PF55_CUSTOM_POLICY_KEY";
        let mut config = crate::legacy_core::config::ConfigBuilder::default()
            .codex_home(home.path().to_path_buf())
            .build()
            .await
            .unwrap();
        config.model_provider_id = "custom".into();
        config.model = Some("custom-model".into());
        config.model_provider = ModelProviderInfo {
            name: "Custom".into(),
            env_key: Some(env_key.into()),
            ..Default::default()
        };
        config
            .model_providers
            .insert("custom".into(), config.model_provider.clone());
        codex_login::login_with_provider_api_key(
            home.path(),
            env_key,
            "pf55-secret-canary",
            config.cli_auth_credentials_store_mode,
            config.auth_keyring_backend_kind(),
        )
        .unwrap();
        let host = ProviderStatusHost::from_config(
            &config,
            crate::provider_status_host::ProviderAccountMetadata::default(),
        );
        let mut policy =
            ProviderModelPolicy::new(host.clone(), ProviderRuntimeAuthorizations::default());

        assert!(policy.provider_is_selectable("custom", "custom-model"));
        host.persist_policy(
            "custom",
            codex_provider_auth::ProviderActivationPolicy::Inactive,
        )
        .unwrap();
        policy.refresh();
        assert!(!policy.provider_is_selectable("custom", "custom-model"));
        assert!(matches!(
            policy.current("custom", "custom-model"),
            CurrentSelectionDecision::RequireExplicitRecovery {
                reason: codex_provider_auth::ProviderUseBlocker::Inactive,
                ..
            }
        ));
        assert!(!format!("{policy:?}").contains("pf55-secret-canary"));
    }
}
