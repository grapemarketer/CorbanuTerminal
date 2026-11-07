use codex_model_provider_info::canonical_catalog_provider;
use codex_model_provider_info::corrected_catalog_provider;
use codex_protocol::openai_models::InputModality;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ReasoningEffort;
use std::convert::Infallible;
use std::sync::RwLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

pub(crate) fn include_runtime_model(
    models: &mut Vec<ModelPreset>,
    model: &str,
    runtime_provider_id: &str,
) {
    let model = model.trim();
    let runtime_provider_id = runtime_provider_id.trim();
    if model.is_empty()
        || runtime_provider_id.is_empty()
        || corrected_catalog_provider(model, runtime_provider_id).is_some()
        || models.iter().any(|preset| {
            preset.model == model
                && preset
                    .provider_id
                    .as_deref()
                    .or_else(|| canonical_catalog_provider(&preset.model))
                    == Some(runtime_provider_id)
        })
    {
        return;
    }

    let mut preset = models
        .iter()
        .find(|preset| preset.model == model)
        .cloned()
        .unwrap_or_else(|| ModelPreset {
            id: format!("{runtime_provider_id}:{model}"),
            model: model.to_string(),
            provider_id: None,
            orchestration: None,
            display_name: model.to_string(),
            description: "Configured runtime provider".to_string(),
            default_reasoning_effort: ReasoningEffort::None,
            supported_reasoning_efforts: Vec::new(),
            supports_personality: false,
            additional_speed_tiers: Vec::new(),
            service_tiers: Vec::new(),
            default_service_tier: None,
            is_default: false,
            upgrade: None,
            show_in_picker: true,
            multi_agent_version: None,
            availability_nux: None,
            supported_in_api: true,
            input_modalities: vec![InputModality::Text],
        });
    preset.id = format!("{runtime_provider_id}:{model}");
    preset.provider_id = Some(runtime_provider_id.to_string());
    models.push(preset);
}

#[derive(Debug)]
pub(crate) struct ModelCatalog {
    models: RwLock<Vec<ModelPreset>>,
    /// Ids [`ModelCatalog::set_provider_models`] added, per provider.
    provider_model_ids: RwLock<std::collections::HashMap<String, Vec<String>>>,
    /// Numbers the background checks that decide those models; only the
    /// newest one's answer is applied.
    provider_models_check: std::sync::atomic::AtomicU64,
    /// Ids of the models the app server's `model/list` returned; the rest
    /// (runtime and GPU providers) are added here.
    listed_ids: RwLock<Vec<String>>,
    provider_policy: RwLock<Option<crate::chatwidget::provider_model_policy::ProviderModelPolicy>>,
    session_recovery_only: AtomicBool,
    /// D86/#1193: the newest background answer to "which providers have a
    /// named account in this home", with the number of the check that
    /// produced it; `None` until one has answered or when the vault could
    /// not be read. `/status` reads this instead of the vault.
    named_account_providers: RwLock<(u64, Option<std::collections::BTreeSet<String>>)>,
    /// Numbers the background checks that compute it.
    named_account_check: std::sync::atomic::AtomicU64,
    /// #1251: per named sign-in account, the newest background answer to
    /// "what is this account signed in with", with the number of the check
    /// that produced it. `/status` reads this instead of the vault.
    named_sign_ins: RwLock<
        std::collections::BTreeMap<
            codex_login::NamedChatgptAccount,
            (u64, crate::provider_named_accounts::NamedSignInState),
        >,
    >,
}

impl ModelCatalog {
    pub(crate) fn new(models: Vec<ModelPreset>) -> Self {
        Self {
            models: RwLock::new(models),
            provider_model_ids: RwLock::new(std::collections::HashMap::new()),
            provider_models_check: std::sync::atomic::AtomicU64::new(0),
            listed_ids: RwLock::new(Vec::new()),
            provider_policy: RwLock::new(None),
            session_recovery_only: AtomicBool::new(false),
            named_account_providers: RwLock::new((0, None)),
            named_account_check: std::sync::atomic::AtomicU64::new(0),
            named_sign_ins: RwLock::new(std::collections::BTreeMap::new()),
        }
    }

    /// Records which of the models came from `model/list`.
    pub(crate) fn mark_listed(&self, listed: &[ModelPreset]) {
        *self
            .listed_ids
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            listed.iter().map(|preset| preset.id.clone()).collect();
    }

    /// PF-84: replaces the `model/list` models with a fresh list (fetched
    /// with the sign-in of the account the session now uses), keeping the
    /// models added for runtime and GPU providers.
    pub(crate) fn replace_listed_models(&self, listed: Vec<ModelPreset>) {
        let mut listed_ids = self
            .listed_ids
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut models = self
            .models
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let new_ids: Vec<String> = listed.iter().map(|preset| preset.id.clone()).collect();
        // An added model the new list now carries itself is dropped.
        let added: Vec<ModelPreset> = models
            .drain(..)
            .filter(|preset| {
                !listed_ids.contains(&preset.id)
                    && !listed.iter().any(|listed| {
                        listed.id == preset.id
                            || (listed.model == preset.model
                                && listed.provider_id == preset.provider_id)
                    })
            })
            .collect();
        *models = listed;
        models.extend(added);
        *listed_ids = new_ids;
    }

    pub(crate) fn set_session_recovery_only(&self, session_only: bool) {
        self.session_recovery_only
            .store(session_only, Ordering::Release);
    }

    pub(crate) fn take_session_recovery_only(&self) -> bool {
        self.session_recovery_only.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn set_provider_policy(
        &self,
        policy: crate::chatwidget::provider_model_policy::ProviderModelPolicy,
    ) {
        *self
            .provider_policy
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(policy);
    }

    pub(crate) fn provider_policy(
        &self,
    ) -> Option<crate::chatwidget::provider_model_policy::ProviderModelPolicy> {
        self.provider_policy
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn has_provider_policy(&self) -> bool {
        self.provider_policy
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    pub(crate) fn update_current_provider(&self, runtime_provider_id: &str) {
        if let Some(policy) = self
            .provider_policy
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
        {
            policy.set_current_runtime(runtime_provider_id);
        }
    }

    pub(crate) fn refresh_provider_policy(&self) {
        if let Some(policy) = self
            .provider_policy
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
        {
            policy.refresh();
        }
    }

    pub(crate) fn update_provider_statuses(
        &self,
        statuses: &[codex_provider_auth::ProviderStatusSnapshot],
    ) {
        if let Some(policy) = self
            .provider_policy
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
        {
            policy.update_statuses(statuses);
        }
    }

    pub(crate) fn sync_runtime_models<'a>(
        &self,
        runtime_provider_ids: impl IntoIterator<Item = &'a str>,
        preferred_model: Option<&str>,
        current_provider: &str,
    ) {
        let mut runtime_provider_ids = runtime_provider_ids.into_iter().collect::<Vec<_>>();
        runtime_provider_ids.sort_unstable();
        let builtins =
            codex_model_provider_info::built_in_model_providers(/*openai_base_url*/ None);
        let mut models = self
            .models
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for runtime_provider_id in runtime_provider_ids {
            let Some(model) = codex_model_provider_info::resolve_model_for_provider(
                // Never project a curated model onto unrelated runtimes or seed
                // idle local servers. Preserve the established shared opaque-model
                // flow for explicitly configured custom gateways.
                preferred_model
                    .filter(|model| {
                        runtime_provider_id == current_provider
                            || (canonical_catalog_provider(model).is_none()
                                && !builtins.contains_key(runtime_provider_id))
                    })
                    .map(str::to_owned),
                runtime_provider_id,
            ) else {
                continue;
            };
            include_runtime_model(&mut models, &model, runtime_provider_id);
        }
    }

    /// #1193: starts a new named-account check and returns its number.
    pub(crate) fn begin_named_account_check(&self) -> u64 {
        self.named_account_check.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// #1193: records check `generation`'s answer (`None`: the vault could
    /// not be read) unless a newer check has answered already.
    pub(crate) fn set_named_account_providers(
        &self,
        generation: u64,
        providers: Option<std::collections::BTreeSet<String>>,
    ) {
        let mut cached = self
            .named_account_providers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if generation > cached.0 {
            *cached = (generation, providers);
        }
    }

    /// #1193: whether `provider_id` has a named account in this home, from
    /// the cached answer; `None` while unknown. Never reads the vault.
    pub(crate) fn provider_has_named_account(&self, provider_id: &str) -> Option<bool> {
        self.named_account_providers
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .1
            .as_ref()
            .map(|providers| providers.contains(provider_id))
    }

    /// #1251: records check `generation`'s answer for `account` unless a
    /// newer check has answered for it already.
    pub(crate) fn set_named_sign_in(
        &self,
        generation: u64,
        account: codex_login::NamedChatgptAccount,
        state: crate::provider_named_accounts::NamedSignInState,
    ) {
        let mut cached = self
            .named_sign_ins
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cached
            .get(&account)
            .is_none_or(|(newest, _)| generation > *newest)
        {
            cached.insert(account, (generation, state));
        }
    }

    /// #1251: what `account` is signed in with, from the cached answer;
    /// `None` while unknown. Never reads the vault.
    pub(crate) fn named_sign_in(
        &self,
        account: &codex_login::NamedChatgptAccount,
    ) -> Option<crate::provider_named_accounts::NamedSignInState> {
        self.named_sign_ins
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(account)
            .map(|(_, state)| state.clone())
    }

    /// Starts a new provider-models check and returns its number.
    pub(crate) fn begin_provider_models_check(&self) -> u64 {
        self.provider_models_check.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Whether `generation` is the newest provider-models check.
    pub(crate) fn is_latest_provider_models_check(&self, generation: u64) -> bool {
        self.provider_models_check.load(Ordering::SeqCst) == generation
    }

    /// Sets the models offered for `runtime_provider_id` beyond its own list:
    /// the ones added by an earlier call are removed (except `current_model`,
    /// the one the session is on, which stays as it was), then `presets` are
    /// added as models of that provider, skipping models the catalog already
    /// lists for it. They never become the default.
    pub(crate) fn set_provider_models(
        &self,
        runtime_provider_id: &str,
        presets: Vec<ModelPreset>,
        current_model: Option<&str>,
    ) {
        let mut added = self
            .provider_model_ids
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut models = self
            .models
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = added.remove(runtime_provider_id).unwrap_or_default();
        let kept = current_model
            .map(|model| format!("{runtime_provider_id}:{model}"))
            .filter(|id| previous.contains(id));
        models.retain(|preset| !previous.contains(&preset.id) || Some(&preset.id) == kept.as_ref());
        let mut ids = kept.into_iter().collect::<Vec<_>>();
        for mut preset in presets {
            if models.iter().any(|listed| {
                listed.model == preset.model
                    && listed
                        .provider_id
                        .as_deref()
                        .or_else(|| canonical_catalog_provider(&listed.model))
                        == Some(runtime_provider_id)
            }) {
                continue;
            }
            preset.id = format!("{runtime_provider_id}:{}", preset.model);
            preset.provider_id = Some(runtime_provider_id.to_string());
            preset.is_default = false;
            ids.push(preset.id.clone());
            models.push(preset);
        }
        if !ids.is_empty() {
            added.insert(runtime_provider_id.to_string(), ids);
        }
    }

    pub(crate) fn preset_is_selectable(&self, preset: &ModelPreset) -> bool {
        self.provider_policy()
            .is_none_or(|policy| policy.preset_is_selectable(preset))
    }

    pub(crate) fn provider_is_selectable(&self, provider: &str, model: &str) -> bool {
        self.provider_policy()
            .is_none_or(|policy| policy.provider_is_selectable(provider, model))
    }

    pub(crate) fn provider_use_decision(
        &self,
        provider: &str,
        model: &str,
        context: codex_provider_auth::ProviderUseContext,
    ) -> Option<codex_provider_auth::ProviderUseDecision> {
        self.provider_policy()
            .map(|policy| policy.assess(provider, model, context))
    }

    pub(crate) fn current_requires_recovery(&self, provider: &str, model: &str) -> bool {
        self.provider_policy().is_some_and(|policy| {
            matches!(
                policy.current(provider, model),
                codex_provider_auth::CurrentSelectionDecision::RequireExplicitRecovery { .. }
            )
        })
    }

    pub(crate) fn try_list_models(&self) -> Result<Vec<ModelPreset>, Infallible> {
        Ok(self
            .models
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone())
    }

    pub(crate) fn provider_for_model(&self, model: &str) -> Option<String> {
        self.models
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|preset| preset.model == model)
            .and_then(|preset| preset.provider_id.clone())
    }

    pub(crate) fn replace_gpu_models(&self, runtime_models: Vec<ModelPreset>) {
        let mut models = self
            .models
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        models.retain(|preset| {
            !preset
                .provider_id
                .as_deref()
                .is_some_and(|provider_id| provider_id.starts_with("gpu-"))
        });
        models.extend(runtime_models);
    }
}

#[cfg(test)]
#[path = "model_catalog_tests.rs"]
mod tests;
