use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;

use codex_login::OpenAiAuthMetadata;
use codex_login::ProviderAccountKind;
use codex_login::ProviderAccountSignIn;
use codex_model_provider_info::CORBANU_API_KEY_ENV_VARS;
use codex_model_provider_info::NamedProviderAccount;
use codex_model_provider_info::PFTERMINAL_PLAN_API_KEY_ENV_VAR;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_provider_auth::ApiKeyAuthTarget;
use codex_provider_auth::ApiKeyCredentialMetadata;
use codex_provider_auth::ApiKeyStorage;
use codex_provider_auth::ClaudeCredentialMetadata;
use codex_provider_auth::CommandAuthMetadata;
use codex_provider_auth::ConfiguredAvailability;
use codex_provider_auth::CorbanuCredentialSource;
use codex_provider_auth::CorbanuPlanMetadata;
use codex_provider_auth::CurrentProviderSelection;
use codex_provider_auth::EnvironmentCredentialMetadata;
use codex_provider_auth::LocalProviderMetadata;
use codex_provider_auth::ManagedApiKeyMetadata;
use codex_provider_auth::ProviderActivationPolicy;
use codex_provider_auth::ProviderCatalog;
use codex_provider_auth::ProviderCatalogEntry;
use codex_provider_auth::ProviderEligibilityError;
use codex_provider_auth::ProviderEligibilitySnapshot;
use codex_provider_auth::ProviderEligibilityStore;
use codex_provider_auth::ProviderMetadata;
use codex_provider_auth::ProviderMetadataSnapshot;
use codex_provider_auth::ProviderRuntimeAuthorizations;
use codex_provider_auth::ProviderSetupCapability;
use codex_provider_auth::ProviderStatusCatalog;
use codex_provider_auth::ProviderStatusResolver;
use codex_provider_auth::ProviderStatusSnapshot;

use crate::chatwidget::claude_code_login::ClaudeCodePlanStatus;
use crate::legacy_core::config::Config;

type ManagedSnapshot = std::io::Result<codex_login::ProviderApiKeyStorageMetadataSnapshot>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProviderAccountMetadata {
    pub(crate) openai: OpenAiAuthMetadata,
    pub(crate) claude: ClaudeCredentialMetadata,
    pub(crate) corbanu: CorbanuPlanMetadata,
}

impl Default for ProviderAccountMetadata {
    fn default() -> Self {
        Self {
            openai: OpenAiAuthMetadata::Missing,
            claude: ClaudeCredentialMetadata::NotConfigured,
            corbanu: CorbanuPlanMetadata::NotConfigured,
        }
    }
}

impl ProviderAccountMetadata {
    pub(crate) async fn discover(config: &Config) -> Self {
        let codex_home = config.codex_home.to_path_buf();
        let openai = codex_login::openai_auth_metadata_from_config(
            config, /*enable_codex_api_key_env*/ true,
        );
        let claude_status = crate::chatwidget::claude_code_login::current_status_with_timeout(
            codex_home.as_path(),
            std::time::Duration::from_secs(10),
        );
        let (openai, claude_status) = tokio::join!(openai, claude_status);
        let claude = claude_credential_metadata(claude_status);
        Self {
            openai,
            claude,
            corbanu: CorbanuPlanMetadata::NotConfigured,
        }
    }
}

/// Maps the `/providers` Claude status to the metadata that gates Claude Plan
/// models and panes.
fn claude_credential_metadata(status: ClaudeCodePlanStatus) -> ClaudeCredentialMetadata {
    match status {
        ClaudeCodePlanStatus::ManagedToken { stored: true } => {
            ClaudeCredentialMetadata::Configured {
                source: codex_provider_auth::ClaudeCredentialSource::Managed,
            }
        }
        ClaudeCodePlanStatus::EnvironmentToken { available: true }
        | ClaudeCodePlanStatus::EnvironmentTokenWithoutSelection => {
            ClaudeCredentialMetadata::Configured {
                source: codex_provider_auth::ClaudeCredentialSource::Environment,
            }
        }
        ClaudeCodePlanStatus::SignedIn { .. } => ClaudeCredentialMetadata::Configured {
            source: codex_provider_auth::ClaudeCredentialSource::ClaudeCodeLogin,
        },
        ClaudeCodePlanStatus::ManagedToken { stored: false }
        | ClaudeCodePlanStatus::EnvironmentToken { available: false }
        | ClaudeCodePlanStatus::InvalidSelection
        | ClaudeCodePlanStatus::NeedsReauthorization => {
            ClaudeCredentialMetadata::RecoveryRequired {
                reason: codex_provider_auth::ProviderRecoveryReason::UnhealthyClaudeSelection,
            }
        }
        ClaudeCodePlanStatus::SelectionRequired {
            existing_source_detected: true,
        } => ClaudeCredentialMetadata::RecoveryRequired {
            reason: codex_provider_auth::ProviderRecoveryReason::AmbiguousClaudeSources,
        },
        ClaudeCodePlanStatus::SelectionRequired {
            existing_source_detected: false,
        }
        | ClaudeCodePlanStatus::SignedOut => ClaudeCredentialMetadata::NotConfigured,
        ClaudeCodePlanStatus::Checking
        | ClaudeCodePlanStatus::Unavailable
        | ClaudeCodePlanStatus::Error => ClaudeCredentialMetadata::Unavailable,
    }
}

/// Metadata-only adapter shared by onboarding and the later provider manager.
#[derive(Clone, Debug)]
pub(crate) struct ProviderStatusHost {
    catalog: ProviderCatalog,
    codex_home: PathBuf,
    current: Arc<RwLock<CurrentProviderSelection>>,
    account: Arc<RwLock<ProviderAccountMetadata>>,
    authorizations: ProviderRuntimeAuthorizations,
    health: Arc<RwLock<codex_provider_auth::ProviderCredentialHealth>>,
    /// PF-84 (#417): catalog entries whose runtime provider runs on a named
    /// account, and whether the vault holds that account. Such an entry's
    /// status comes from the account, never from the default credential.
    named_accounts: Arc<BTreeMap<String, bool>>,
    /// #609: the name of each of those accounts, by entry id.
    named_account_names: Arc<BTreeMap<String, String>>,
    /// #665: for API-key entries served by several connections where one runs
    /// on a named account, each connection's own credential (by entry id,
    /// then runtime provider id). Each connection authenticates with its own.
    connection_credentials: Arc<BTreeMap<String, BTreeMap<String, ConnectionCredential>>>,
    /// PF-84 (#609): the catalog entry of the session's own provider when it
    /// runs on a named sign-in account. Startup checked that account before
    /// the session began (with a remote app server, the server checks it at
    /// thread start), so it counts as signed in even when this home's vault
    /// does not hold it. Kept apart from the default login's metadata, which
    /// a refresh re-reads.
    session_sign_in_entry: Option<String>,
    /// #609: by entry id, what the enrolled named sign-in account the entry
    /// runs on holds (a ChatGPT sign-in or an OpenAI API key), as its vault
    /// metadata records it; absent when not recorded (shown as a ChatGPT
    /// sign-in, as before). Never read from the sign-in itself.
    named_sign_ins: Arc<BTreeMap<String, ProviderAccountSignIn>>,
    /// #609: the configured `forced_login_method`, which can rule out a
    /// named sign-in account by what it holds.
    forced_login_method: Option<ForcedLoginMethod>,
    /// Reads an API-key environment variable (tests replace it).
    environment: fn(&str) -> EnvironmentCredentialMetadata,
}

/// The credential one connection (runtime provider) authenticates with.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ConnectionCredential {
    /// A named account and whether the vault holds it.
    NamedAccount { enrolled: bool },
    /// The provider's default key (environment, broker or stored).
    DefaultKey { env_key: String },
}

/// The vault account kinds a named account of `provider` can be enrolled as.
fn named_account_kinds(
    provider: &codex_model_provider_info::ModelProviderInfo,
) -> &'static [ProviderAccountKind] {
    if provider.is_claude_plan() {
        &[
            ProviderAccountKind::ClaudeOauthToken,
            ProviderAccountKind::ClaudeConfigDir,
        ]
    } else if provider.auth.is_some() {
        &[ProviderAccountKind::Command]
    } else if codex_model_provider::named_sign_in_account(provider).is_some() {
        // PF-84-S06: an OpenAI ChatGPT or API-key sign-in.
        &[ProviderAccountKind::ChatgptAuth]
    } else {
        &[ProviderAccountKind::ApiKey]
    }
}

/// #665: for an API-key entry served by several connections (`zai` and
/// `zai-anthropic` share `ZAI_API_KEY`) where at least one runs on a named
/// account, the credential each connection really uses.
fn connection_credentials(
    catalog: &ProviderCatalog,
    config: &Config,
    holds: impl Fn(&NamedProviderAccount, ProviderAccountKind) -> bool,
) -> BTreeMap<String, BTreeMap<String, ConnectionCredential>> {
    let mut by_entry = BTreeMap::new();
    for entry in catalog.entries() {
        let Some(ProviderSetupCapability::ApiKey {
            storage: ApiKeyStorage::EnvironmentVariable { env_key },
        }) = entry.setup_capabilities.iter().next()
        else {
            continue;
        };
        let connections = entry
            .runtime_provider_ids
            .iter()
            .filter_map(|id| {
                config
                    .model_providers
                    .get(id.as_str())
                    .map(|provider| (id.as_str(), provider))
            })
            .collect::<Vec<_>>();
        if connections.len() < 2
            || connections
                .iter()
                .all(|(_, provider)| provider.account.is_none())
        {
            continue;
        }
        let mut credentials = BTreeMap::new();
        for (id, provider) in connections {
            let credential = match &provider.account {
                Some(account) => ConnectionCredential::NamedAccount {
                    enrolled: named_account_kinds(provider)
                        .iter()
                        .any(|kind| holds(account, *kind)),
                },
                None => ConnectionCredential::DefaultKey {
                    env_key: env_key.clone(),
                },
            };
            credentials.insert(id.to_string(), credential);
        }
        by_entry.insert(entry.id.as_str().to_string(), credentials);
    }
    by_entry
}

/// PF-84: for each catalog entry whose runtime provider selects a named
/// account, whether `holds` reports the account as enrolled (metadata only).
fn named_account_enrollment(
    catalog: &ProviderCatalog,
    config: &Config,
    holds: impl Fn(&NamedProviderAccount, ProviderAccountKind) -> bool,
) -> BTreeMap<String, bool> {
    let mut enrollment = BTreeMap::new();
    for entry in catalog.entries() {
        let selected = entry.runtime_provider_ids.iter().find_map(|id| {
            config
                .model_providers
                .get(id.as_str())
                .and_then(|provider| Some((provider, provider.account.as_ref()?)))
        });
        let Some((provider, account)) = selected else {
            continue;
        };
        let enrolled = named_account_kinds(provider)
            .iter()
            .any(|kind| holds(account, *kind));
        enrollment.insert(entry.id.as_str().to_string(), enrolled);
    }
    enrollment
}

/// #609: the named account each catalog entry's status follows, by entry id.
/// Shared entries follow an enrolled connection in catalog order; other
/// entries keep the selection from [`named_account_enrollment`].
fn named_account_names(
    catalog: &ProviderCatalog,
    config: &Config,
    connection_credentials: &BTreeMap<String, BTreeMap<String, ConnectionCredential>>,
) -> BTreeMap<String, String> {
    catalog
        .entries()
        .iter()
        .filter_map(|entry| {
            let account = entry.runtime_provider_ids.iter().find_map(|id| {
                if let Some(connections) = connection_credentials.get(entry.id.as_str())
                    && connections.get(id.as_str())
                        != Some(&ConnectionCredential::NamedAccount { enrolled: true })
                {
                    return None;
                }
                config
                    .model_providers
                    .get(id.as_str())
                    .and_then(|provider| provider.account.as_ref())
            })?;
            Some((entry.id.as_str().to_string(), account.name.clone()))
        })
        .collect()
}

impl ProviderStatusHost {
    /// #609: by entry id, the named account whose enrollment decided the
    /// entry's status in [`Self::resolve`]. An entry whose connections run on
    /// different accounts (#665) counts only when an enrolled named account
    /// set it up; otherwise its default key did. Setup kinds the account does
    /// not decide never count.
    pub(crate) fn status_accounts(&self) -> BTreeMap<String, String> {
        self.named_account_names
            .iter()
            .filter(|(entry_id, _)| {
                self.connection_credentials
                    .get(entry_id.as_str())
                    .is_none_or(|connections| {
                        connections.values().any(|credential| {
                            *credential == ConnectionCredential::NamedAccount { enrolled: true }
                        })
                    })
            })
            .filter(|(entry_id, _)| {
                self.catalog.get(entry_id.as_str()).is_some_and(|entry| {
                    matches!(
                        entry.setup_capabilities.iter().next(),
                        Some(
                            ProviderSetupCapability::OpenAiAccount
                                | ProviderSetupCapability::ApiKey { .. }
                                | ProviderSetupCapability::ClaudeAccount
                        )
                    )
                })
            })
            .map(|(entry_id, name)| (entry_id.clone(), name.clone()))
            .collect()
    }

    /// #609: by entry id, what the named sign-in account in
    /// [`Self::status_accounts`] holds, as its metadata records it, and why
    /// `forced_login_method` rules it out: shown on the `/providers` row.
    pub(crate) fn status_account_details(&self) -> BTreeMap<String, String> {
        let status_accounts = self.status_accounts();
        self.named_sign_ins
            .iter()
            .filter(|(entry_id, _)| status_accounts.contains_key(entry_id.as_str()))
            .map(|(entry_id, sign_in)| {
                let detail = match (sign_in, self.forced_login_method) {
                    (ProviderAccountSignIn::ApiKey, Some(ForcedLoginMethod::Chatgpt)) => {
                        "an OpenAI API key; this configuration requires a ChatGPT sign-in"
                    }
                    (ProviderAccountSignIn::Chatgpt, Some(ForcedLoginMethod::Api)) => {
                        "a ChatGPT sign-in; this configuration requires an API key"
                    }
                    (ProviderAccountSignIn::ApiKey, _) => "an OpenAI API key",
                    (ProviderAccountSignIn::Chatgpt, _) => "a ChatGPT sign-in",
                };
                (entry_id.clone(), detail.to_string())
            })
            .collect()
    }

    pub(crate) fn from_config(config: &Config, account: ProviderAccountMetadata) -> Self {
        let codex_home = config.codex_home.to_path_buf();
        Self::from_config_with_account_registry(config, account, |named, kind| {
            codex_login::provider_account_holds(&codex_home, &named.provider_id, &named.name, kind)
                .unwrap_or(false)
        })
        .with_named_sign_ins(config, |named| {
            codex_login::provider_account_sign_in(&codex_home, &named.provider_id, &named.name)
                .ok()
                .flatten()
        })
    }

    /// #609: records what each enrolled named sign-in account an entry runs
    /// on holds, as `recorded` reports it from the account registry's
    /// metadata (the sign-in itself is never read).
    pub(crate) fn with_named_sign_ins(
        mut self,
        config: &Config,
        recorded: impl Fn(&NamedProviderAccount) -> Option<ProviderAccountSignIn>,
    ) -> Self {
        let mut sign_ins = BTreeMap::new();
        for entry in self.catalog.entries() {
            if self.named_accounts.get(entry.id.as_str()) != Some(&true) {
                continue;
            }
            let selected = entry.runtime_provider_ids.iter().find_map(|id| {
                config
                    .model_providers
                    .get(id.as_str())
                    .and_then(|provider| Some((provider, provider.account.as_ref()?)))
            });
            if let Some((provider, account)) = selected
                && codex_model_provider::named_sign_in_account(provider).is_some()
                && let Some(sign_in) = recorded(account)
            {
                sign_ins.insert(entry.id.as_str().to_string(), sign_in);
            }
        }
        self.named_sign_ins = Arc::new(sign_ins);
        self
    }

    pub(crate) fn from_config_with_account_registry(
        config: &Config,
        account: ProviderAccountMetadata,
        holds: impl Fn(&NamedProviderAccount, ProviderAccountKind) -> bool,
    ) -> Self {
        let catalog = ProviderCatalog::from_runtime_providers(&config.model_providers);
        let named_accounts = Arc::new(named_account_enrollment(&catalog, config, &holds));
        let connection_credentials = Arc::new(connection_credentials(&catalog, config, &holds));
        let named_account_names = Arc::new(named_account_names(
            &catalog,
            config,
            &connection_credentials,
        ));
        let session_sign_in_entry =
            codex_model_provider::named_sign_in_account(&config.model_provider)
                .and_then(|_| {
                    catalog.entries().iter().find(|entry| {
                        entry
                            .runtime_provider_ids
                            .iter()
                            .any(|id| id.as_str() == config.model_provider_id)
                    })
                })
                .map(|entry| entry.id.as_str().to_string());
        Self {
            named_accounts,
            named_account_names,
            connection_credentials,
            session_sign_in_entry,
            named_sign_ins: Arc::default(),
            // As the named account's manager checks it: only what the
            // operator configured, not the API-key onboarding default a
            // third-party session provider implies (#571).
            forced_login_method: config.configured_forced_login_method,
            environment: environment_metadata,
            catalog,
            codex_home: config.codex_home.to_path_buf(),
            current: Arc::new(RwLock::new(CurrentProviderSelection::runtime_id(
                config.model_provider_id.clone(),
            ))),
            account: Arc::new(RwLock::new(account)),
            authorizations: ProviderRuntimeAuthorizations::default(),
            health: Arc::default(),
        }
    }
    /// Tests: read API-key environment variables through `environment`.
    #[cfg(test)]
    pub(crate) fn with_environment(
        mut self,
        environment: fn(&str) -> EnvironmentCredentialMetadata,
    ) -> Self {
        self.environment = environment;
        self
    }
    pub(crate) fn set_runtime_authorizations(&mut self, value: ProviderRuntimeAuthorizations) {
        self.authorizations = value;
    }
    pub(crate) fn catalog(&self) -> &ProviderCatalog {
        &self.catalog
    }
    pub(crate) fn update_account_metadata(&self, account: ProviderAccountMetadata) {
        *self
            .account
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = account;
    }
    pub(crate) fn mark_openai_api_key(&self) {
        self.credential_changed("openai");
        self.account
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .openai = OpenAiAuthMetadata::ApiKey;
    }
    pub(crate) fn mark_openai_account(&self) {
        self.credential_changed("openai");
        self.account
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .openai = OpenAiAuthMetadata::Account;
    }
    pub(crate) fn mark_claude_configured(
        &self,
        source: codex_provider_auth::ClaudeCredentialSource,
    ) {
        self.credential_changed(codex_model_provider_info::CLAUDE_PLAN_PROVIDER_ID);
        self.account
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .claude = ClaudeCredentialMetadata::Configured { source };
    }
    pub(crate) fn api_key_metadata(&self, target: &ApiKeyAuthTarget) -> ApiKeyCredentialMetadata {
        match &target.storage {
            ApiKeyStorage::EnvironmentVariable { env_key } => {
                self.environment_api_key_metadata(env_key, /*snapshot*/ None)
            }
            ApiKeyStorage::OpenAiAuth => {
                let openai = self
                    .account
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .openai;
                ApiKeyCredentialMetadata {
                    environment: EnvironmentCredentialMetadata::Missing,
                    managed: if openai == OpenAiAuthMetadata::ApiKey {
                        ManagedApiKeyMetadata::Stored {
                            source: codex_login::ProviderApiKeyStorageSource::EncryptedVault,
                        }
                    } else {
                        ManagedApiKeyMetadata::Missing
                    },
                }
            }
        }
    }

    pub(crate) fn resolve(&self) -> ProviderStatusCatalog {
        let account = *self
            .account
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let managed_keys = self.catalog.entries().iter().filter_map(|entry| {
            match entry.setup_capabilities.iter().next() {
                Some(ProviderSetupCapability::ApiKey {
                    storage: ApiKeyStorage::EnvironmentVariable { env_key },
                }) if (self.environment)(env_key) == EnvironmentCredentialMetadata::Missing => {
                    Some(env_key.as_str())
                }
                _ => None,
            }
        });
        let managed = codex_login::provider_api_key_metadata_snapshot_from_auth_storage(
            &self.codex_home,
            managed_keys,
        );
        let mut metadata = ProviderMetadataSnapshot::default();
        for entry in self.catalog.entries() {
            metadata.insert(entry, self.metadata_for(entry, account, Some(&managed)));
        }
        let mut statuses = ProviderStatusResolver::resolve(
            &self.catalog,
            &metadata,
            &ProviderEligibilitySnapshot::from(
                ProviderEligibilityStore::new(&self.codex_home).load(),
            ),
            &self
                .current
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        self.authorizations
            .apply_to_status_catalog(&self.catalog, &mut statuses);
        self.health
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .apply(&mut statuses);
        statuses
    }

    /// #665: connections, of an entry whose connections run on different
    /// accounts, whose own credential is missing (`zai-anthropic` on the
    /// default account with no default Z.AI key while `zai` runs on an
    /// enrolled named account). They cannot authenticate, even when their
    /// entry is set up by another connection.
    pub(crate) fn connections_without_credential(&self) -> std::collections::BTreeSet<String> {
        self.connection_credentials
            .values()
            .flatten()
            .filter(|(_, credential)| {
                !self.connection_has_credential(credential, /*managed*/ None)
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn connection_has_credential(
        &self,
        credential: &ConnectionCredential,
        managed: Option<&ManagedSnapshot>,
    ) -> bool {
        match credential {
            ConnectionCredential::NamedAccount { enrolled } => *enrolled,
            ConnectionCredential::DefaultKey { env_key } => {
                let metadata = self.environment_api_key_metadata(env_key, managed);
                metadata.environment == EnvironmentCredentialMetadata::Present
                    || matches!(metadata.managed, ManagedApiKeyMetadata::Stored { .. })
            }
        }
    }

    pub(crate) fn begin_credential_attempt(&self, scope: String, runtime_provider: &str) {
        self.begin_account_credential_attempt(scope, runtime_provider, /*account*/ None);
    }

    /// PF-84: an attempt on `account` (a named account of `runtime_provider`),
    /// or on the default credential when `None`.
    pub(crate) fn begin_account_credential_attempt(
        &self,
        scope: String,
        runtime_provider: &str,
        account: Option<codex_provider_auth::NamedCredentialAccount>,
    ) {
        let Some(entry) = self.catalog.entries().iter().find(|entry| {
            entry
                .runtime_provider_ids
                .iter()
                .any(|id| id.as_str() == runtime_provider)
        }) else {
            return;
        };
        if let Some(status) = self.resolve_provider(entry.id.as_str()) {
            self.health
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .begin_for_account(scope, &status, account);
        }
    }

    pub(crate) fn reject_credential_attempt(
        &self,
        scope: &str,
    ) -> Option<codex_provider_auth::RejectedCredential> {
        self.health
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reject_credential(scope)
    }

    pub(crate) fn finish_credential_attempt(&self, scope: &str) {
        self.health
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .finish(scope);
    }

    pub(crate) fn credential_changed(&self, provider: &str) {
        self.health
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .credential_changed(provider);
    }

    pub(crate) fn resolve_provider(&self, provider_id: &str) -> Option<ProviderStatusSnapshot> {
        self.resolve().get(provider_id).cloned()
    }
    pub(crate) fn resolve_target(&self, target: &ApiKeyAuthTarget) -> ProviderStatusSnapshot {
        self.resolve_provider(target.provider_id.as_str())
            .unwrap_or_else(|| panic!("catalog target disappeared: {}", target.provider_id))
    }

    pub(crate) fn persist_policy(
        &self,
        provider_id: &str,
        policy: ProviderActivationPolicy,
    ) -> Result<(), ProviderEligibilityError> {
        let entry = self
            .catalog
            .get(provider_id)
            .ok_or(ProviderEligibilityError::WriteUnavailable)?;
        let store = ProviderEligibilityStore::new(&self.codex_home);
        let mut eligibility = store.load()?;
        eligibility.set_policy(entry, policy);
        store.save(&eligibility)
    }

    pub(crate) fn activate(&self, provider_id: &str) -> bool {
        self.persist_policy(provider_id, ProviderActivationPolicy::Active)
            .is_ok()
    }
    pub(crate) fn set_current_runtime(&self, runtime_provider_id: impl Into<String>) {
        *self
            .current
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            CurrentProviderSelection::runtime_id(runtime_provider_id);
    }

    fn metadata_for(
        &self,
        entry: &ProviderCatalogEntry,
        account: ProviderAccountMetadata,
        managed: Option<&ManagedSnapshot>,
    ) -> ProviderMetadata {
        let first = entry.setup_capabilities.iter().next();
        // #665: connections on different accounts. The entry is set up when
        // any connection has its own credential; the others are refused one
        // by one (`connections_without_credential`).
        if let Some(connections) = self.connection_credentials.get(entry.id.as_str())
            && let Some(ProviderSetupCapability::ApiKey {
                storage: ApiKeyStorage::EnvironmentVariable { env_key },
            }) = first
        {
            let named_enrolled = connections.values().any(|credential| {
                *credential == ConnectionCredential::NamedAccount { enrolled: true }
            });
            let default_key = connections.values().any(|credential| {
                matches!(credential, ConnectionCredential::DefaultKey { .. })
                    && self.connection_has_credential(credential, managed)
            });
            return ProviderMetadata::ApiKey(if default_key && !named_enrolled {
                self.environment_api_key_metadata(env_key, managed)
            } else {
                ApiKeyCredentialMetadata {
                    environment: EnvironmentCredentialMetadata::Missing,
                    managed: if named_enrolled {
                        ManagedApiKeyMetadata::Stored {
                            source: codex_login::ProviderApiKeyStorageSource::EncryptedVault,
                        }
                    } else {
                        ManagedApiKeyMetadata::Missing
                    },
                }
            });
        }
        let named = self.named_accounts.get(entry.id.as_str()).copied();
        match first {
            // PF-84 (#571): OpenAI on a named sign-in account (selected for
            // this session) is configured by that account, not the default
            // login, so `/model` offers OpenAI from a session on another
            // provider with no default OpenAI login.
            Some(ProviderSetupCapability::OpenAiAccount)
            | Some(ProviderSetupCapability::ApiKey {
                storage: ApiKeyStorage::OpenAiAuth,
            }) => ProviderMetadata::OpenAi(match (named, account.openai) {
                (Some(true), _) => self.named_sign_in_metadata(entry.id.as_str()),
                // Startup already vetted the session's own named account;
                // the local vault may not hold it (a remote app server), so
                // that answer stands. The default login's state says nothing
                // about a named account (#609), and a refresh re-reads it.
                (Some(false), _)
                    if self.session_sign_in_entry.as_deref() == Some(entry.id.as_str()) =>
                {
                    codex_login::OpenAiAuthMetadata::Account
                }
                (Some(false), _) => codex_login::OpenAiAuthMetadata::Missing,
                (None, metadata) => metadata,
            }),
            Some(ProviderSetupCapability::ApiKey {
                storage: ApiKeyStorage::EnvironmentVariable { .. },
            }) if named.is_some() => {
                let enrolled = named == Some(true);
                // A named account ignores the environment and the default key.
                ProviderMetadata::ApiKey(ApiKeyCredentialMetadata {
                    environment: EnvironmentCredentialMetadata::Missing,
                    managed: if enrolled {
                        ManagedApiKeyMetadata::Stored {
                            source: codex_login::ProviderApiKeyStorageSource::EncryptedVault,
                        }
                    } else {
                        ManagedApiKeyMetadata::Missing
                    },
                })
            }
            Some(ProviderSetupCapability::ApiKey {
                storage: ApiKeyStorage::EnvironmentVariable { env_key },
            }) => ProviderMetadata::ApiKey(self.environment_api_key_metadata(env_key, managed)),
            Some(ProviderSetupCapability::ClaudeAccount) if named.is_some() => {
                ProviderMetadata::Claude(if named == Some(true) {
                    ClaudeCredentialMetadata::Configured {
                        source: codex_provider_auth::ClaudeCredentialSource::Managed,
                    }
                } else {
                    ClaudeCredentialMetadata::NotConfigured
                })
            }
            Some(ProviderSetupCapability::ClaudeAccount) => {
                ProviderMetadata::Claude(account.claude)
            }
            Some(ProviderSetupCapability::CorbanuPlan) => {
                ProviderMetadata::CorbanuPlan(corbanu_metadata(&self.codex_home, account.corbanu))
            }
            Some(ProviderSetupCapability::Local { .. }) => {
                // Local runtimes have no credential enrollment boundary. Availability is
                // established by the actual request, matching the pre-unification behavior.
                ProviderMetadata::Local(LocalProviderMetadata::Available)
            }
            Some(ProviderSetupCapability::CommandAuth { .. }) => {
                ProviderMetadata::CommandAuth(CommandAuthMetadata::StatusOnly)
            }
            Some(ProviderSetupCapability::StatusOnly { .. }) => ProviderMetadata::StatusOnly,
            None => ProviderMetadata::Unavailable,
        }
    }

    /// #609: an enrolled named sign-in account's status, from what its
    /// metadata records it holds: an OpenAI API key is reported as one, and
    /// a sign-in `forced_login_method` rules out is not configured (its use
    /// is refused with the reason). Not recorded: a ChatGPT sign-in, as
    /// before.
    fn named_sign_in_metadata(&self, entry_id: &str) -> codex_login::OpenAiAuthMetadata {
        use codex_login::OpenAiAuthMetadata;
        match (
            self.named_sign_ins.get(entry_id).copied(),
            self.forced_login_method,
        ) {
            (Some(ProviderAccountSignIn::ApiKey), Some(ForcedLoginMethod::Chatgpt))
            | (Some(ProviderAccountSignIn::Chatgpt), Some(ForcedLoginMethod::Api)) => {
                OpenAiAuthMetadata::Missing
            }
            (Some(ProviderAccountSignIn::ApiKey), _) => OpenAiAuthMetadata::ApiKey,
            (Some(ProviderAccountSignIn::Chatgpt) | None, _) => OpenAiAuthMetadata::Account,
        }
    }

    fn environment_api_key_metadata(
        &self,
        env_key: &str,
        snapshot: Option<&ManagedSnapshot>,
    ) -> ApiKeyCredentialMetadata {
        let environment = (self.environment)(env_key);
        prioritized_api_key_metadata(environment, || match snapshot {
            Some(Ok(snapshot)) => snapshot
                .get(env_key)
                .map(|metadata| ManagedApiKeyMetadata::from(Ok(metadata)))
                .unwrap_or(ManagedApiKeyMetadata::Unavailable),
            Some(Err(_)) => ManagedApiKeyMetadata::Unavailable,
            None => ManagedApiKeyMetadata::from(
                codex_login::provider_api_key_metadata_from_auth_storage(&self.codex_home, env_key),
            ),
        })
    }
}

fn prioritized_api_key_metadata(
    environment: EnvironmentCredentialMetadata,
    read_managed: impl FnOnce() -> ManagedApiKeyMetadata,
) -> ApiKeyCredentialMetadata {
    let managed = if environment == EnvironmentCredentialMetadata::Missing {
        read_managed()
    } else {
        ManagedApiKeyMetadata::Missing
    };
    ApiKeyCredentialMetadata {
        environment,
        managed,
    }
}

fn corbanu_metadata(
    codex_home: &std::path::Path,
    supplied: CorbanuPlanMetadata,
) -> CorbanuPlanMetadata {
    corbanu_metadata_with_environment(codex_home, supplied, environment_metadata)
}

fn corbanu_metadata_with_environment(
    codex_home: &std::path::Path,
    supplied: CorbanuPlanMetadata,
    read_environment: impl Fn(&str) -> EnvironmentCredentialMetadata,
) -> CorbanuPlanMetadata {
    if CORBANU_API_KEY_ENV_VARS
        .into_iter()
        .any(|name| read_environment(name) == EnvironmentCredentialMetadata::Present)
    {
        return CorbanuPlanMetadata::Configured {
            source: CorbanuCredentialSource::Environment,
            availability: ConfiguredAvailability::Ready,
        };
    }
    if supplied != CorbanuPlanMetadata::NotConfigured {
        return supplied;
    }
    match codex_login::provider_api_key_metadata_from_auth_storage(
        codex_home,
        PFTERMINAL_PLAN_API_KEY_ENV_VAR,
    ) {
        Ok(codex_login::ProviderApiKeyStorageMetadata::Stored { .. }) => {
            CorbanuPlanMetadata::Configured {
                source: CorbanuCredentialSource::Managed,
                availability: ConfiguredAvailability::Ready,
            }
        }
        Ok(codex_login::ProviderApiKeyStorageMetadata::Missing)
        | Ok(codex_login::ProviderApiKeyStorageMetadata::Suppressed) => {
            CorbanuPlanMetadata::NotConfigured
        }
        Err(_) => CorbanuPlanMetadata::Unavailable,
    }
}

fn environment_metadata(env_key: &str) -> EnvironmentCredentialMetadata {
    environment_metadata_from(
        std::env::var(env_key),
        crate::legacy_core::broker_holds_environment_key(env_key),
    )
}

/// #625: a key the credential broker took out of this process's environment
/// (Aggressive's `broker_model_auth`) is still the provider's key: the broker
/// sends its model requests with it.
fn environment_metadata_from(
    value: Result<String, std::env::VarError>,
    held_by_broker: bool,
) -> EnvironmentCredentialMetadata {
    match value {
        Ok(value) if value.trim().is_empty() => EnvironmentCredentialMetadata::Invalid,
        Ok(_) => EnvironmentCredentialMetadata::Present,
        Err(std::env::VarError::NotPresent) if held_by_broker => {
            EnvironmentCredentialMetadata::Present
        }
        Err(std::env::VarError::NotPresent) => EnvironmentCredentialMetadata::Missing,
        Err(std::env::VarError::NotUnicode(_)) => EnvironmentCredentialMetadata::Invalid,
    }
}

#[cfg(test)]
#[path = "provider_status_host_tests.rs"]
mod tests;
