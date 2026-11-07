//! PF-84-S06 slice B: named ChatGPT/OpenAI sign-in accounts.
//!
//! A named account keeps its whole sign-in (`auth.json` contents: ChatGPT
//! tokens or an OpenAI API-key login) in the home's encrypted vault under
//! `provider/<provider>/accounts/<name>/chatgpt_auth`. It adds no keychain
//! item and never reads or writes the `default` account's `auth.json` or
//! `Codex Auth` item. Refreshed tokens are written back to that label only.

use std::fmt;
use std::path::Path;

use codex_protocol::config_types::ForcedLoginMethod;
use codex_vault::ProviderAccountKind;
use codex_vault::ProviderAccountName;
use codex_vault::ProviderAccountSignIn;
use codex_vault::Vault;
use codex_vault::VaultKeyStorage;
use zeroize::Zeroizing;

use super::storage::AuthDotJson;
use super::storage::AuthStorageBackend;

/// A named sign-in account of an OpenAI-auth provider.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NamedChatgptAccount {
    pub provider_id: String,
    pub name: String,
}

impl fmt::Display for NamedChatgptAccount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.provider_id, self.name)
    }
}

/// One named account's sign-in, in the vault.
pub(super) struct VaultAccountAuthStorage {
    vault: Vault,
    provider_id: String,
    name: ProviderAccountName,
}

impl fmt::Debug for VaultAccountAuthStorage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VaultAccountAuthStorage")
            .field("provider_id", &self.provider_id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl VaultAccountAuthStorage {
    pub(super) fn new(codex_home: &Path, account: &NamedChatgptAccount) -> std::io::Result<Self> {
        Self::with_vault(Vault::new(codex_home.to_path_buf()), account)
    }

    pub(super) fn with_vault(vault: Vault, account: &NamedChatgptAccount) -> std::io::Result<Self> {
        let name = ProviderAccountName::parse(&account.name)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        codex_vault::validate_provider_account_provider_id(&account.provider_id)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        Ok(Self {
            vault,
            provider_id: account.provider_id.clone(),
            name,
        })
    }
}

/// #609: what a sign-in holds, for its vault metadata (never the value).
/// ChatGPT tokens count first, as in [`sign_in_restriction`]. `None` for a
/// sign-in with neither: status then shows it as a ChatGPT sign-in, as
/// before, and its manager still checks it at use.
pub(super) fn sign_in_kind(auth: &AuthDotJson) -> Option<ProviderAccountSignIn> {
    if auth.tokens.is_some() {
        Some(ProviderAccountSignIn::Chatgpt)
    } else if auth.openai_api_key.is_some() {
        Some(ProviderAccountSignIn::ApiKey)
    } else {
        None
    }
}

impl VaultAccountAuthStorage {
    /// Saves a first sign-in; fails, changing nothing, when the account
    /// already exists (a concurrent add never overwrites it).
    pub(super) fn create(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        let raw = Zeroizing::new(serde_json::to_string(auth).map_err(std::io::Error::other)?);
        self.vault
            .create_provider_account_with_sign_in(
                &self.provider_id,
                &self.name,
                ProviderAccountKind::ChatgptAuth,
                &raw,
                sign_in_kind(auth),
            )
            .map_err(std::io::Error::other)
    }

    /// #609: records what a sign-in read from the vault holds when its
    /// metadata does not say so yet (saved by an earlier build). Best
    /// effort: status then shows the account as a ChatGPT sign-in, as before.
    fn record_sign_in_kind(&self, auth: &AuthDotJson) {
        let Some(sign_in) = sign_in_kind(auth) else {
            return;
        };
        if let Err(error) =
            self.vault
                .record_provider_account_sign_in(&self.provider_id, &self.name, sign_in)
        {
            tracing::debug!(%error, "could not record what account `{}` holds", self.name);
        }
    }
}

impl AuthStorageBackend for VaultAccountAuthStorage {
    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        // Never initialise a vault (and its key) just to read.
        if self.vault.key_storage() == VaultKeyStorage::NotInitialized {
            return Ok(None);
        }
        let Some((raw, recorded)) = self
            .vault
            .read_provider_account_sign_in(&self.provider_id, &self.name)
            .map_err(std::io::Error::other)?
        else {
            return Ok(None);
        };
        let auth = self.parse(&raw)?;
        // #609: a sign-in saved before its kind was recorded gets it on its
        // next use (the index only; nothing is written once it is there).
        if recorded.is_none() {
            self.record_sign_in_kind(&auth);
        }
        Ok(Some(auth))
    }

    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        let raw = Zeroizing::new(serde_json::to_string(auth).map_err(std::io::Error::other)?);
        self.vault
            .write_provider_account_with_sign_in(
                &self.provider_id,
                &self.name,
                ProviderAccountKind::ChatgptAuth,
                &raw,
                sign_in_kind(auth),
            )
            .map_err(std::io::Error::other)
    }

    fn delete(&self) -> std::io::Result<bool> {
        if self.vault.key_storage() == VaultKeyStorage::NotInitialized {
            return Ok(false);
        }
        self.vault
            .remove_provider_account(&self.provider_id, &self.name)
            .map_err(std::io::Error::other)
    }

    /// #1311 (D171): read and saved under one vault lock, and only while the
    /// account still holds a sign-in.
    fn update(
        &self,
        change: &mut dyn FnMut(&mut AuthDotJson) -> std::io::Result<()>,
    ) -> std::io::Result<Option<AuthDotJson>> {
        if self.vault.key_storage() == VaultKeyStorage::NotInitialized {
            return Ok(None);
        }
        let mut updated = None;
        let found = self.vault.update_provider_account_sign_in(
            &self.provider_id,
            &self.name,
            |raw| -> std::io::Result<_> {
                let mut auth = self.parse(raw)?;
                change(&mut auth)?;
                let raw =
                    Zeroizing::new(serde_json::to_string(&auth).map_err(std::io::Error::other)?);
                let kind = sign_in_kind(&auth);
                updated = Some(auth);
                Ok((raw, kind))
            },
        );
        let found = found.map_err(std::io::Error::other)??;
        Ok(updated.filter(|_| found))
    }

    fn is_named_account(&self) -> bool {
        true
    }
}

impl VaultAccountAuthStorage {
    /// The stored sign-in; the parse error never quotes the value.
    fn parse(&self, raw: &str) -> std::io::Result<AuthDotJson> {
        serde_json::from_str(raw).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "the stored sign-in of account `{}` is unreadable ({:?} at line {}, column {})",
                    self.name,
                    error.classify(),
                    error.line(),
                    error.column()
                ),
            )
        })
    }
}

/// Saves `auth` as the named account's sign-in, replacing an earlier one
/// (or, with `must_be_new`, failing if the account exists). The default
/// login is never touched.
pub fn save_named_account_auth(
    codex_home: &Path,
    account: &NamedChatgptAccount,
    auth: &AuthDotJson,
    must_be_new: bool,
) -> std::io::Result<()> {
    let storage = VaultAccountAuthStorage::new(codex_home, account)?;
    if must_be_new {
        storage.create(auth)
    } else {
        storage.save(auth)
    }
}

/// Why a stored sign-in may not be used under the configuration's login
/// restrictions (`forced_login_method`, `forced_chatgpt_workspace_id`).
pub(super) fn sign_in_restriction(
    account: &NamedChatgptAccount,
    auth: &AuthDotJson,
    forced_login_method: Option<ForcedLoginMethod>,
    forced_workspace: Option<&[String]>,
) -> Option<String> {
    let name = &account.name;
    let is_api_key = auth.tokens.is_none() && auth.openai_api_key.is_some();
    match (forced_login_method, is_api_key) {
        (Some(ForcedLoginMethod::Chatgpt), true) => {
            return Some(format!(
                "account `{name}` holds an OpenAI API key, but this configuration requires a \
                 ChatGPT sign-in; sign in with `corbanu login --account {name}`"
            ));
        }
        (Some(ForcedLoginMethod::Api), false) => {
            return Some(format!(
                "account `{name}` holds a ChatGPT sign-in, but this configuration requires an \
                 API key; use `corbanu login --account {name} --with-api-key`"
            ));
        }
        _ => {}
    }
    let workspaces = forced_workspace?;
    let workspace = auth.tokens.as_ref().and_then(|tokens| {
        tokens
            .account_id
            .clone()
            .or(tokens.id_token.chatgpt_account_id.clone())
    });
    match workspace {
        Some(workspace) if workspaces.contains(&workspace) => None,
        _ if is_api_key => None,
        _ => Some(format!(
            "account `{name}` is signed in to a ChatGPT workspace this configuration does not \
             allow; sign in again with `corbanu login --account {name}`"
        )),
    }
}

/// Why `account` cannot be used under these login restrictions, checked
/// before a session starts. `None` when it can (or holds no sign-in).
pub fn named_account_restriction_error(
    codex_home: &Path,
    account: &NamedChatgptAccount,
    forced_login_method: Option<ForcedLoginMethod>,
    forced_workspace: Option<&[String]>,
) -> Option<String> {
    let auth = VaultAccountAuthStorage::new(codex_home, account)
        .ok()?
        .load()
        .ok()
        .flatten()?;
    sign_in_restriction(account, &auth, forced_login_method, forced_workspace)
}

/// The sign-in a named account holds now, kept so its tokens can be
/// revoked after a new sign-in replaces it. Never printed.
pub struct PreviousSignIn(AuthDotJson);

impl fmt::Debug for PreviousSignIn {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PreviousSignIn(<redacted>)")
    }
}

/// The account's current sign-in, if any (unreadable counts as none).
pub fn current_named_sign_in(
    codex_home: &Path,
    account: &NamedChatgptAccount,
) -> Option<PreviousSignIn> {
    VaultAccountAuthStorage::new(codex_home, account)
        .ok()?
        .load()
        .ok()
        .flatten()
        .map(PreviousSignIn)
}

/// Revokes a replaced sign-in's tokens (best effort), unless the account
/// still holds the same refresh token or the new sign-in is the same ChatGPT
/// user (revoking one sign-in of a user can end the user's other sessions).
pub async fn revoke_replaced_sign_in(
    codex_home: &Path,
    account: &NamedChatgptAccount,
    previous: PreviousSignIn,
    auth_route_config: &crate::AuthRouteConfig,
) {
    let refresh = |auth: &AuthDotJson| {
        auth.tokens
            .as_ref()
            .map(|tokens| tokens.refresh_token.clone())
    };
    let current = current_named_sign_in(codex_home, account);
    let same_refresh = current.as_ref().and_then(|current| refresh(&current.0))
        == refresh(&previous.0)
        && refresh(&previous.0).is_some();
    let same_user = current
        .as_ref()
        .is_some_and(|current| same_chatgpt_user(&current.0, &previous.0));
    if same_refresh || same_user {
        return;
    }
    if let Err(error) =
        super::revoke::revoke_auth_tokens(Some(&previous.0), auth_route_config).await
    {
        tracing::warn!("failed to revoke a replaced named-account sign-in: {error}");
    }
}

/// An OpenAI API-key sign-in for a named account.
pub fn login_named_account_with_api_key(
    codex_home: &Path,
    account: &NamedChatgptAccount,
    api_key: &str,
) -> std::io::Result<()> {
    save_named_account_auth(
        codex_home,
        account,
        &AuthDotJson {
            auth_mode: Some(codex_protocol::auth::AuthMode::ApiKey),
            openai_api_key: Some(api_key.to_string()),
            tokens: None,
            last_refresh: None,
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
        },
        /*must_be_new*/ false,
    )
}

/// Revokes the named account's tokens (best effort) and deletes only its
/// sign-in. `Ok(false)` when it had none.
pub async fn logout_named_account_with_revoke(
    codex_home: &Path,
    account: &NamedChatgptAccount,
    auth_route_config: &crate::AuthRouteConfig,
) -> std::io::Result<bool> {
    let storage = VaultAccountAuthStorage::new(codex_home, account)?;
    // An unreadable sign-in is still deleted; only the revoke is skipped.
    if let Ok(auth) = storage.load()
        && let Err(error) =
            super::revoke::revoke_auth_tokens(auth.as_ref(), auth_route_config).await
    {
        tracing::warn!("failed to revoke named account tokens during logout: {error}");
    }
    storage.delete()
}

/// Revokes the tokens of a sign-in that could not be saved (best effort),
/// unless the account already holds a sign-in of the same ChatGPT user
/// (revoking one sign-in of a user can end the user's other sessions).
pub(crate) async fn revoke_unsaved_sign_in(
    codex_home: &Path,
    account: &NamedChatgptAccount,
    auth: &AuthDotJson,
    auth_route_config: &crate::AuthRouteConfig,
) -> std::io::Result<()> {
    let existing = current_named_sign_in(codex_home, account);
    if existing.is_some_and(|existing| same_chatgpt_user(&existing.0, auth)) {
        return Ok(());
    }
    super::revoke::revoke_auth_tokens(Some(auth), auth_route_config).await
}

/// Whether two sign-ins belong to the same (known) ChatGPT user.
fn same_chatgpt_user(a: &AuthDotJson, b: &AuthDotJson) -> bool {
    let user = |auth: &AuthDotJson| {
        auth.tokens
            .as_ref()
            .and_then(|tokens| tokens.id_token.chatgpt_user_id.clone())
    };
    user(a).is_some() && user(a) == user(b)
}

/// What may be shown about a named sign-in: never a token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedSignInSummary {
    /// `ChatGPT sign-in` or `OpenAI API key`.
    pub kind: &'static str,
    /// The first letter and domain of the email, e.g. `t…@example.com`.
    pub email_hint: Option<String>,
    pub plan: Option<String>,
}

/// The first letter and the domain of `email`.
pub fn email_hint(email: &str) -> String {
    match email.split_once('@') {
        Some((local, domain)) => {
            let first = local.chars().next().map(String::from).unwrap_or_default();
            format!("{first}…@{domain}")
        }
        None => "…".to_string(),
    }
}

pub(super) fn summarize(auth: &AuthDotJson) -> NamedSignInSummary {
    match auth.tokens.as_ref() {
        Some(tokens) => NamedSignInSummary {
            kind: "ChatGPT sign-in",
            email_hint: tokens.id_token.email.as_deref().map(email_hint),
            plan: tokens.id_token.get_chatgpt_plan_type(),
        },
        None => NamedSignInSummary {
            kind: "OpenAI API key",
            email_hint: None,
            plan: None,
        },
    }
}

/// The summary of a named account's sign-in, `None` when it has none.
pub fn named_account_summary(
    codex_home: &Path,
    account: &NamedChatgptAccount,
) -> std::io::Result<Option<NamedSignInSummary>> {
    Ok(VaultAccountAuthStorage::new(codex_home, account)?
        .load()?
        .as_ref()
        .map(summarize))
}

/// Other sign-ins of this home (the `default` login and the provider's other
/// named accounts) that belong to the same ChatGPT user as `account`'s
/// stored sign-in. Signing the same user in twice is allowed (Travis,
/// 2026-10-11: warn, don't refuse), but both accounts then share that user's
/// limits and a sign-out of one can end the other's session.
pub fn same_user_sign_ins(
    codex_home: &Path,
    account: &NamedChatgptAccount,
    default_auth: Option<&AuthDotJson>,
) -> std::io::Result<Vec<String>> {
    let user = |auth: &AuthDotJson| {
        auth.tokens
            .as_ref()
            .and_then(|tokens| tokens.id_token.chatgpt_user_id.clone())
    };
    let Some(target) = VaultAccountAuthStorage::new(codex_home, account)?
        .load()?
        .as_ref()
        .and_then(user)
    else {
        return Ok(Vec::new());
    };
    let mut same = Vec::new();
    if default_auth.and_then(user).as_deref() == Some(target.as_str()) {
        same.push(codex_vault::DEFAULT_PROVIDER_ACCOUNT.to_string());
    }
    let vault = Vault::new(codex_home.to_path_buf());
    if vault.key_storage() == VaultKeyStorage::NotInitialized {
        return Ok(same);
    }
    for other in vault
        .list_provider_accounts()
        .map_err(std::io::Error::other)?
    {
        if other.provider_id != account.provider_id
            || other.name.as_str() == account.name
            || !other.kinds.contains(&ProviderAccountKind::ChatgptAuth)
        {
            continue;
        }
        let other_account = NamedChatgptAccount {
            provider_id: other.provider_id.clone(),
            name: other.name.as_str().to_string(),
        };
        let other_user = VaultAccountAuthStorage::new(codex_home, &other_account)?
            .load()?
            .as_ref()
            .and_then(user);
        if other_user.as_deref() == Some(target.as_str()) {
            same.push(other_account.name);
        }
    }
    Ok(same)
}

#[cfg(test)]
#[path = "named_chatgpt_tests.rs"]
mod tests;
