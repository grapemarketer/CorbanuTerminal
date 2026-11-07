//! PF-84-S04: named accounts in `/providers` and onboarding.
//!
//! Only names, kinds and salted 12-hex fingerprints are ever shown. A value goes
//! from masked entry straight to the vault; events carry it in a redacting
//! wrapper and nothing here logs or renders it.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;
use std::path::PathBuf;

use codex_model_provider_info::CLAUDE_PLAN_PROVIDER_ID;
use codex_model_provider_info::ModelProviderInfo;
use codex_vault::DEFAULT_PROVIDER_ACCOUNT;
use codex_vault::ProviderAccountKind;
use codex_vault::ProviderAccountName;
use codex_vault::Vault;
use codex_vault::VaultKeyStorage;
use zeroize::Zeroizing;

use crate::legacy_core::config::Config;

/// One named account of one provider (runtime provider id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NamedAccountRow {
    pub(crate) provider_id: String,
    pub(crate) name: ProviderAccountName,
    pub(crate) kinds: Vec<ProviderAccountKind>,
    pub(crate) fingerprint: Option<String>,
    /// PF-84-S06: the AWS profile name an `aws_profile` account uses; not
    /// secret, so it is shown.
    pub(crate) aws_profile: Option<String>,
    /// PF-84-S06: a sign-in account's kind, email hint and plan (no token).
    pub(crate) sign_in: Option<String>,
}

/// What `/providers` knows about accounts: the named rows, the account this
/// session uses and the account new sessions use, per provider id. A provider
/// missing from a map uses its `default` account.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AccountsView {
    pub(crate) rows: Vec<NamedAccountRow>,
    pub(crate) session: BTreeMap<String, String>,
    pub(crate) defaults: BTreeMap<String, String>,
    /// #609: by catalog entry id, the named account a provider's shown
    /// status was computed from (from the status host that computed it).
    pub(crate) status_accounts: BTreeMap<String, String>,
    /// #609: by catalog entry id, what that account holds when the status
    /// host knows it (an OpenAI API key or a ChatGPT sign-in, and why the
    /// configuration rules it out), from the account registry's metadata.
    pub(crate) status_account_details: BTreeMap<String, String>,
}

impl AccountsView {
    pub(crate) fn new(
        rows: Vec<NamedAccountRow>,
        session_config: &Config,
        current_config: &Config,
    ) -> Self {
        let session = session_config
            .model_providers
            .iter()
            .filter_map(|(id, provider)| {
                provider
                    .account
                    .as_ref()
                    .map(|account| (id.clone(), account.name.clone()))
            })
            .collect();
        let defaults = persisted_defaults(current_config);
        Self {
            rows,
            session,
            defaults,
            status_accounts: BTreeMap::new(),
            status_account_details: BTreeMap::new(),
        }
    }

    /// #609: which named account each entry's status is about, as reported
    /// by the status host whose statuses are shown with this view.
    pub(crate) fn with_status_accounts(
        mut self,
        status_host: &crate::provider_status_host::ProviderStatusHost,
    ) -> Self {
        self.status_accounts = status_host.status_accounts();
        self.status_account_details = status_host.status_account_details();
        self
    }

    /// The named account `entry_id`'s shown status was computed from.
    pub(crate) fn status_account(&self, entry_id: &str) -> Option<&str> {
        self.status_accounts.get(entry_id).map(String::as_str)
    }

    /// #609: what [`Self::status_account`] holds, when known.
    pub(crate) fn status_account_detail(&self, entry_id: &str) -> Option<&str> {
        self.status_account_details
            .get(entry_id)
            .map(String::as_str)
    }

    pub(crate) fn session_account(&self, provider_id: &str) -> &str {
        self.session
            .get(provider_id)
            .map_or(DEFAULT_PROVIDER_ACCOUNT, String::as_str)
    }

    pub(crate) fn default_account(&self, provider_id: &str) -> &str {
        self.defaults
            .get(provider_id)
            .map_or(DEFAULT_PROVIDER_ACCOUNT, String::as_str)
    }

    /// Named accounts of any of `provider_ids`, in a stable order.
    pub(crate) fn rows_for<'a>(
        &'a self,
        provider_ids: &'a [String],
    ) -> impl Iterator<Item = &'a NamedAccountRow> + 'a {
        self.rows
            .iter()
            .filter(move |row| provider_ids.contains(&row.provider_id))
    }

    pub(crate) fn find(&self, provider_id: &str, name: &str) -> Option<&NamedAccountRow> {
        self.rows
            .iter()
            .find(|row| row.provider_id == provider_id && row.name.as_str() == name)
    }

    /// ` · this session · default` style markers for one account.
    pub(crate) fn markers(&self, provider_id: &str, name: &str) -> String {
        let mut markers = String::new();
        if self.session_account(provider_id) == name {
            markers.push_str(" · this session");
        }
        if self.default_account(provider_id) == name {
            markers.push_str(" · default for new sessions");
        }
        markers
    }

    /// Whether removing or renaming `name` would pull it from under this
    /// session or from new sessions.
    pub(crate) fn in_use(&self, provider_id: &str, name: &str) -> bool {
        self.session_account(provider_id) == name || self.default_account(provider_id) == name
    }
}

/// `[provider_accounts]` as new sessions see it (config files and `-c`).
pub(crate) fn persisted_defaults(config: &Config) -> BTreeMap<String, String> {
    let effective = config.config_layer_stack.effective_config();
    let Some(table) = effective
        .get("provider_accounts")
        .and_then(|value| value.as_table())
    else {
        return BTreeMap::new();
    };
    table
        .iter()
        .filter_map(|(provider_id, value)| {
            let name = value.as_str()?.trim();
            (!name.is_empty() && name != DEFAULT_PROVIDER_ACCOUNT)
                .then(|| (provider_id.clone(), name.to_string()))
        })
        .collect()
}

/// Whether the vault holds a named account of `provider_id` of `kind`;
/// `None` when the vault cannot be read. Blocking (it reads the vault
/// index); no credential value is read.
pub(crate) fn holds_named_account(
    codex_home: &Path,
    provider_id: &str,
    kind: ProviderAccountKind,
) -> Option<bool> {
    holds_named_account_where(codex_home, provider_id, |kinds| kinds.contains(&kind))
}

/// D86/#1193: the providers with at least one named account in the vault
/// (none when it is not initialised); `None` when the vault cannot be read.
/// Blocking (it reads the vault index), so it runs off the UI thread; no
/// credential value is read.
pub(crate) fn named_account_providers(codex_home: &Path) -> Option<BTreeSet<String>> {
    let vault = Vault::new(codex_home.to_path_buf());
    if vault.key_storage() == VaultKeyStorage::NotInitialized {
        return Some(BTreeSet::new());
    }
    vault
        .list_provider_accounts()
        .inspect_err(|error| tracing::warn!(%error, "could not list named accounts"))
        .ok()
        .map(|accounts| {
            accounts
                .into_iter()
                .map(|account| account.provider_id)
                .collect()
        })
}

/// #1251: what a named sign-in account is signed in with, as the background
/// check found it in the vault.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NamedSignInState {
    /// Its kind, email hint and plan (no token).
    SignedIn(codex_login::NamedSignInSummary),
    /// Removed or signed out (D171).
    NoSignIn,
    /// The vault could not be read.
    Unreadable,
}

/// #1251: reads `account`'s sign-in summary from the vault. Blocking, so it
/// runs off the UI thread.
pub(crate) fn named_sign_in_state(
    codex_home: &Path,
    account: &codex_login::NamedChatgptAccount,
) -> NamedSignInState {
    match codex_login::named_account_summary(codex_home, account) {
        Ok(Some(summary)) => NamedSignInState::SignedIn(summary),
        Ok(None) => NamedSignInState::NoSignIn,
        Err(error) => {
            tracing::warn!(%error, "could not read a named account's sign-in");
            NamedSignInState::Unreadable
        }
    }
}

/// D86/#1193: works out off the UI thread which providers have a named
/// account in this home and caches the answer in `catalog`, where `/status`
/// reads it (until it is known, `/status` names the account rather than hide
/// it). #1251: the same job caches the session's named sign-in summary
/// (until it is known, `/status` names the account alone). Runs at session
/// start, when a session is configured, after every account change, when
/// `/providers` opens and after each `/status` (so a change made by another
/// process shows on the next one); only the newest answer is kept. With
/// named accounts off and no named sign-in, or outside a Tokio runtime, the
/// vault is not read.
pub(crate) fn check_named_account_providers(
    catalog: &std::sync::Arc<crate::model_catalog::ModelCatalog>,
    config: &Config,
) {
    let named_accounts = config
        .features
        .enabled(codex_features::Feature::NamedAccounts);
    let sign_in = codex_model_provider::named_sign_in_account(&config.model_provider);
    if !named_accounts && sign_in.is_none() {
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let generation = catalog.begin_named_account_check();
    let catalog = std::sync::Arc::clone(catalog);
    let codex_home = config.codex_home.to_path_buf();
    runtime.spawn_blocking(move || {
        if named_accounts {
            catalog.set_named_account_providers(generation, named_account_providers(&codex_home));
        }
        if let Some(account) = sign_in {
            let state = named_sign_in_state(&codex_home, &account);
            catalog.set_named_sign_in(generation, account, state);
        }
    });
}

fn holds_named_account_where(
    codex_home: &Path,
    provider_id: &str,
    kinds_match: impl Fn(&[ProviderAccountKind]) -> bool,
) -> Option<bool> {
    let vault = Vault::new(codex_home.to_path_buf());
    if vault.key_storage() == VaultKeyStorage::NotInitialized {
        return Some(false);
    }
    vault
        .list_provider_accounts()
        .inspect_err(|error| tracing::warn!(%error, "could not list named accounts"))
        .ok()
        .map(|accounts| {
            accounts
                .iter()
                .any(|account| account.provider_id == provider_id && kinds_match(&account.kinds))
        })
}

/// Lists named accounts with their fingerprints. Blocking; vault metadata
/// and a salted hash only.
pub(crate) fn load_rows(codex_home: &Path) -> Result<Vec<NamedAccountRow>, String> {
    let vault = Vault::new(codex_home.to_path_buf());
    if vault.key_storage() == VaultKeyStorage::NotInitialized {
        return Ok(Vec::new());
    }
    let accounts = vault
        .list_provider_accounts()
        .map_err(|error| format!("Could not read named accounts: {error}"))?;
    Ok(accounts
        .into_iter()
        .map(|account| {
            let fingerprint = vault
                .provider_account_fingerprint(&account.provider_id, &account.name)
                .ok()
                .flatten();
            let aws_profile = account
                .kinds
                .contains(&ProviderAccountKind::AwsProfile)
                .then(|| {
                    vault
                        .read_provider_account(
                            &account.provider_id,
                            &account.name,
                            ProviderAccountKind::AwsProfile,
                        )
                        .ok()
                        .flatten()
                        .map(|profile| profile.as_str().to_string())
                })
                .flatten();
            let sign_in = account
                .kinds
                .contains(&ProviderAccountKind::ChatgptAuth)
                .then(|| {
                    codex_login::named_account_summary(
                        codex_home,
                        &codex_login::NamedChatgptAccount {
                            provider_id: account.provider_id.clone(),
                            name: account.name.as_str().to_string(),
                        },
                    )
                    .ok()
                    .flatten()
                    .map(|summary| describe_sign_in(&summary))
                })
                .flatten();
            NamedAccountRow {
                provider_id: account.provider_id,
                name: account.name,
                kinds: account.kinds,
                fingerprint,
                aws_profile,
                sign_in,
            }
        })
        .collect())
}

/// How a new named account of a provider is entered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AddAccountMethod {
    ApiKey,
    ClaudeToken,
    ClaudeConfigDir,
    Command,
    AwsProfile,
    /// A ChatGPT sign-in by device code, into the account's vault store.
    ChatgptDeviceCode,
    /// An OpenAI API-key sign-in (masked entry).
    OpenaiApiKey,
}

impl AddAccountMethod {
    pub(crate) fn kind(self) -> ProviderAccountKind {
        match self {
            Self::ApiKey => ProviderAccountKind::ApiKey,
            Self::ClaudeToken => ProviderAccountKind::ClaudeOauthToken,
            Self::ClaudeConfigDir => ProviderAccountKind::ClaudeConfigDir,
            Self::Command => ProviderAccountKind::Command,
            Self::AwsProfile => ProviderAccountKind::AwsProfile,
            Self::ChatgptDeviceCode | Self::OpenaiApiKey => ProviderAccountKind::ChatgptAuth,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ApiKey => "API key",
            Self::ClaudeToken => "Claude subscription token (claude setup-token)",
            Self::ClaudeConfigDir => "Claude Code login (its CLAUDE_CONFIG_DIR)",
            Self::Command => "External command account",
            Self::AwsProfile => "AWS profile (from ~/.aws/config)",
            Self::ChatgptDeviceCode => "Sign in with ChatGPT (device code)",
            Self::OpenaiApiKey => "OpenAI API key",
        }
    }

    /// Masked entry; otherwise the value is a path or nothing at all.
    pub(crate) fn is_secret(self) -> bool {
        matches!(self, Self::ApiKey | Self::ClaudeToken | Self::OpenaiApiKey)
    }
}

/// The ways a provider can take a named account; empty when it cannot hold
/// one.
pub(crate) fn add_methods(
    provider_id: &str,
    provider: &ModelProviderInfo,
) -> Vec<AddAccountMethod> {
    if !crate::legacy_core::config::supports_named_accounts(provider)
        || codex_vault::validate_provider_account_provider_id(provider_id).is_err()
    {
        return Vec::new();
    }
    if provider_id == CLAUDE_PLAN_PROVIDER_ID {
        vec![
            AddAccountMethod::ClaudeToken,
            AddAccountMethod::ClaudeConfigDir,
        ]
    } else if provider.is_claude_plan() {
        Vec::new()
    } else if provider.requires_openai_auth {
        if provider_id != codex_model_provider_info::OPENAI_PROVIDER_ID {
            return Vec::new();
        }
        vec![
            AddAccountMethod::ChatgptDeviceCode,
            AddAccountMethod::OpenaiApiKey,
        ]
    } else if provider.env_key.is_some() {
        vec![AddAccountMethod::ApiKey]
    } else if provider.auth.is_some() {
        vec![AddAccountMethod::Command]
    } else if provider.is_amazon_bedrock() {
        vec![AddAccountMethod::AwsProfile]
    } else {
        Vec::new()
    }
}

/// The first provider id of a catalog entry that can hold named accounts.
pub(crate) fn account_provider_id<'a>(
    config: &Config,
    runtime_provider_ids: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    runtime_provider_ids.into_iter().find_map(|id| {
        config
            .model_providers
            .get(id)
            .filter(|provider| !add_methods(id, provider).is_empty())
            .map(|_| id.to_string())
    })
}

pub(crate) fn kinds_label(kinds: &[ProviderAccountKind]) -> String {
    kinds
        .iter()
        .map(|kind| match kind {
            ProviderAccountKind::ApiKey => "API key",
            ProviderAccountKind::ClaudeOauthToken => "subscription token",
            ProviderAccountKind::ClaudeConfigDir => "Claude Code login",
            ProviderAccountKind::ChatgptAuth => "ChatGPT sign-in",
            ProviderAccountKind::AwsProfile => "AWS profile",
            ProviderAccountKind::Command => "external command",
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// `token · fp 1a2b3c4d5e6f` for one row; an AWS profile is named.
pub(crate) fn row_detail(row: &NamedAccountRow) -> String {
    let mut detail = row
        .sign_in
        .clone()
        .unwrap_or_else(|| kinds_label(&row.kinds));
    if let Some(profile) = row.aws_profile.as_deref() {
        detail.push_str(&format!(" `{profile}`"));
    }
    if let Some(fingerprint) = row.fingerprint.as_deref() {
        detail.push_str(&format!(" · fp {fingerprint}"));
    }
    detail
}

/// `ChatGPT · t…@example.com · Plus` (never a token).
pub(crate) fn describe_sign_in(summary: &codex_login::NamedSignInSummary) -> String {
    [
        Some(summary.kind.to_string()),
        summary.email_hint.clone(),
        summary.plan.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

/// A value on its way from masked entry to the vault.
pub(crate) struct AccountValue(Zeroizing<String>);

impl AccountValue {
    pub(crate) fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }
}

impl fmt::Debug for AccountValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted account value>")
    }
}

/// Saves a new account; an existing name is refused. Blocking.
pub(crate) fn save_account(
    codex_home: PathBuf,
    provider_id: &str,
    name: &ProviderAccountName,
    method: AddAccountMethod,
    value: AccountValue,
) -> Result<String, String> {
    let mut cleaned = Zeroizing::new(value.0.trim().to_string());
    if method == AddAccountMethod::ClaudeToken {
        cleaned.retain(|character| !matches!(character, '\r' | '\n'));
    }
    let cleaned = match method {
        AddAccountMethod::Command => Zeroizing::new("1".to_string()),
        _ if cleaned.is_empty() => return Err("The value cannot be empty.".to_string()),
        AddAccountMethod::AwsProfile => Zeroizing::new(
            codex_vault::validate_aws_profile_name(&cleaned).map_err(|error| error.to_string())?,
        ),
        // The account holds a whole sign-in (`auth.json` contents).
        AddAccountMethod::OpenaiApiKey => Zeroizing::new(
            serde_json::to_string(&codex_login::AuthDotJson {
                auth_mode: Some(codex_protocol::auth::AuthMode::ApiKey),
                openai_api_key: Some(cleaned.to_string()),
                tokens: None,
                last_refresh: None,
                agent_identity: None,
                personal_access_token: None,
                bedrock_api_key: None,
            })
            .map_err(|error| error.to_string())?,
        ),
        AddAccountMethod::ChatgptDeviceCode => {
            return Err("A ChatGPT sign-in is saved by its sign-in flow.".to_string());
        }
        _ => cleaned,
    };
    // #609: an OpenAI API-key sign-in records what it holds, so status can
    // tell it from a ChatGPT sign-in without reading it.
    let sign_in = (method == AddAccountMethod::OpenaiApiKey)
        .then_some(codex_vault::ProviderAccountSignIn::ApiKey);
    // Adding never replaces an existing account's credential.
    Vault::new(codex_home)
        .create_provider_account_with_sign_in(provider_id, name, method.kind(), &cleaned, sign_in)
        .map_err(|error| format!("Could not save {provider_id} account `{name}`: {error}"))?;
    Ok(format!("Saved {provider_id} account `{name}`."))
}

/// Renames one account. Blocking.
pub(crate) fn rename_account(
    codex_home: PathBuf,
    provider_id: &str,
    from: &ProviderAccountName,
    to: &ProviderAccountName,
) -> Result<String, String> {
    Vault::new(codex_home)
        .rename_provider_account(provider_id, from, to)
        .map_err(|error| format!("Could not rename {provider_id} account `{from}`: {error}"))?;
    Ok(format!("Renamed {provider_id} account `{from}` to `{to}`."))
}

/// Removes one account and only its labels; a sign-in account's tokens are
/// revoked first (best effort). Blocking; call it off the async runtime.
pub(crate) fn remove_account(
    codex_home: PathBuf,
    provider_id: &str,
    name: &ProviderAccountName,
    auth_route_config: &codex_login::AuthRouteConfig,
) -> Result<String, String> {
    let vault = Vault::new(codex_home.clone());
    let is_sign_in = vault
        .provider_account_kinds(provider_id, name)
        .is_ok_and(|kinds| kinds.contains(&ProviderAccountKind::ChatgptAuth));
    if is_sign_in && let Ok(runtime) = tokio::runtime::Handle::try_current() {
        let account = codex_login::NamedChatgptAccount {
            provider_id: provider_id.to_string(),
            name: name.as_str().to_string(),
        };
        return match runtime.block_on(codex_login::logout_named_account_with_revoke(
            &codex_home,
            &account,
            auth_route_config,
        )) {
            Ok(true) => Ok(format!(
                "Signed out and removed {provider_id} account `{name}`."
            )),
            Ok(false) => Err(format!("{provider_id} has no account `{name}`.")),
            Err(error) => Err(format!(
                "Could not remove {provider_id} account `{name}`: {error}"
            )),
        };
    }
    match vault.remove_provider_account(provider_id, name) {
        Ok(true) => Ok(format!("Removed {provider_id} account `{name}`.")),
        Ok(false) => Err(format!("{provider_id} has no account `{name}`.")),
        Err(error) => Err(format!(
            "Could not remove {provider_id} account `{name}`: {error}"
        )),
    }
}

/// `/providers` account actions; routed by the app.
#[derive(Debug)]
pub(crate) enum ProviderAccountEvent {
    OpenActions {
        provider_id: String,
        name: ProviderAccountName,
    },
    AddStart {
        provider_id: String,
    },
    OpenMethodChoice {
        provider_id: String,
        name: ProviderAccountName,
        methods: Vec<AddAccountMethod>,
    },
    AddNamed {
        provider_id: String,
        name: ProviderAccountName,
        method: AddAccountMethod,
    },
    Save {
        provider_id: String,
        name: ProviderAccountName,
        method: AddAccountMethod,
        value: AccountValue,
    },
    UseForSession {
        provider_id: String,
        name: Option<ProviderAccountName>,
    },
    MakeDefault {
        provider_id: String,
        name: Option<ProviderAccountName>,
    },
    RenameStart {
        provider_id: String,
        name: ProviderAccountName,
    },
    Rename {
        provider_id: String,
        from: ProviderAccountName,
        to: ProviderAccountName,
    },
    RemoveStart {
        provider_id: String,
        name: ProviderAccountName,
    },
    /// `replacement` is the account (None = default) that takes over this
    /// session and new sessions first, when the removed one is in use.
    Remove {
        provider_id: String,
        name: ProviderAccountName,
        replacement: Option<Option<ProviderAccountName>>,
    },
    Finished {
        result: Result<String, String>,
    },
}

#[cfg(test)]
#[path = "provider_named_accounts_tests.rs"]
mod tests;
