//! PF-84 named accounts per provider.
//!
//! An account is a `(provider id, name)` pair. Today's credentials are the
//! implicit `default` account and keep their existing labels; nothing here
//! touches them. Named accounts live in the same per-home vault under
//! `provider/<provider id>/accounts/<name>/<kind>`, so they add no keychain
//! item: the vault key stays the home's single item. The set of these labels is
//! the encrypted account registry.

use std::collections::BTreeMap;
use std::fmt;

use chrono::Utc;
use codex_secrets::SecretName;
use codex_secrets::SecretScope;
use sha2::Digest;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::CredentialType;
use crate::StorageBackend;
use crate::VAULT_SCOPE;
use crate::Vault;
use crate::VaultCredentialMeta;
use crate::VaultError;
use crate::index_secret_entry;
use crate::normalize_label;
use crate::secret_name_for;

/// The implicit account that owns today's (unqualified) credentials.
pub const DEFAULT_PROVIDER_ACCOUNT: &str = "default";
const ACCOUNT_LABEL_PREFIX: &str = "provider/";
const ACCOUNTS_SEGMENT: &str = "accounts";
const MAX_ACCOUNT_NAME_BYTES: usize = 32;
const MAX_PROVIDER_ID_BYTES: usize = 48;
const MAX_AWS_PROFILE_BYTES: usize = 64;

/// Why an account name or provider id was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderAccountError {
    #[error(
        "invalid account name: use 1-32 lowercase letters, digits or '-', starting with a letter or digit"
    )]
    InvalidName,
    #[error("`default` names today's credentials; choose another account name")]
    ReservedName,
    #[error(
        "this provider id cannot hold named accounts: use lowercase letters, digits, '-', '_' or '.'"
    )]
    InvalidProviderId,
    #[error(
        "invalid AWS profile name: use 1-64 letters, digits or '-', '_', '.', '+', '@', starting with a letter or digit"
    )]
    InvalidAwsProfile,
}

/// A validated, non-default account name: `[a-z0-9][a-z0-9-]{0,31}`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderAccountName(String);

impl ProviderAccountName {
    pub fn parse(raw: &str) -> Result<Self, ProviderAccountError> {
        let name = raw.trim();
        if name == DEFAULT_PROVIDER_ACCOUNT {
            return Err(ProviderAccountError::ReservedName);
        }
        let mut bytes = name.bytes();
        let valid_first = bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
        let valid_rest =
            bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if !valid_first || !valid_rest || name.len() > MAX_ACCOUNT_NAME_BYTES {
            return Err(ProviderAccountError::InvalidName);
        }
        Ok(Self(name.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProviderAccountName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl fmt::Debug for ProviderAccountName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, formatter)
    }
}

/// Parses an account selection: `default` (or empty) is `None`.
pub fn parse_provider_account_selection(
    raw: &str,
) -> Result<Option<ProviderAccountName>, ProviderAccountError> {
    let raw = raw.trim();
    if raw.is_empty() || raw == DEFAULT_PROVIDER_ACCOUNT {
        return Ok(None);
    }
    ProviderAccountName::parse(raw).map(Some)
}

/// Validates the AWS profile name an `aws_profile` account stores. The name
/// is not secret; it is checked so it reads cleanly in lists and messages.
pub fn validate_aws_profile_name(raw: &str) -> Result<String, ProviderAccountError> {
    let profile = raw.trim();
    let valid = !profile.is_empty()
        && profile.len() <= MAX_AWS_PROFILE_BYTES
        && profile.as_bytes()[0].is_ascii_alphanumeric()
        && profile.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+' | b'@')
        });
    if valid {
        Ok(profile.to_string())
    } else {
        Err(ProviderAccountError::InvalidAwsProfile)
    }
}

/// Validates a provider id for use in account labels.
pub fn validate_provider_account_provider_id(
    provider_id: &str,
) -> Result<(), ProviderAccountError> {
    let valid = !provider_id.is_empty()
        && provider_id.len() <= MAX_PROVIDER_ID_BYTES
        && provider_id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
        && provider_id.as_bytes()[0].is_ascii_alphanumeric();
    if valid {
        Ok(())
    } else {
        Err(ProviderAccountError::InvalidProviderId)
    }
}

/// One kind of material a named account holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProviderAccountKind {
    /// A provider API key (env-key providers).
    ApiKey,
    /// A Claude subscription OAuth token (`claude setup-token`).
    ClaudeOauthToken,
    /// The `CLAUDE_CONFIG_DIR` of a Claude Code login (not secret).
    ClaudeConfigDir,
    /// A ChatGPT sign-in (`auth.json` contents), kept only in the vault.
    ChatgptAuth,
    /// An AWS profile name (not secret).
    AwsProfile,
    /// Marks an account of an `auth.command` provider; the command receives
    /// the account name in `CORBANU_PROVIDER_ACCOUNT` (not secret).
    Command,
}

impl ProviderAccountKind {
    pub const ALL: [Self; 6] = [
        Self::ApiKey,
        Self::ClaudeOauthToken,
        Self::ClaudeConfigDir,
        Self::ChatgptAuth,
        Self::AwsProfile,
        Self::Command,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::ClaudeOauthToken => "claude_oauth_token",
            Self::ClaudeConfigDir => "claude_config_dir",
            Self::ChatgptAuth => "chatgpt_auth",
            Self::AwsProfile => "aws_profile",
            Self::Command => "command",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == raw)
    }

    /// Subscription material that only its provider integration may read;
    /// generic `/vault` reveal and `vault auth-helper` refuse it.
    pub fn is_provider_managed(self) -> bool {
        matches!(self, Self::ClaudeOauthToken | Self::ChatgptAuth)
    }

    /// Whether the stored value is a secret (an API key, token or sign-in),
    /// not a folder, profile name or marker.
    pub fn is_secret(self) -> bool {
        matches!(
            self,
            Self::ApiKey | Self::ClaudeOauthToken | Self::ChatgptAuth
        )
    }

    fn credential_type(self) -> CredentialType {
        match self {
            Self::ApiKey => CredentialType::ApiKey,
            Self::ClaudeOauthToken | Self::ChatgptAuth => CredentialType::BearerToken,
            Self::ClaudeConfigDir | Self::AwsProfile | Self::Command => {
                CredentialType::ManualSecret
            }
        }
    }
}

/// The vault label for one kind of a named account.
pub fn provider_account_label(
    provider_id: &str,
    name: &ProviderAccountName,
    kind: ProviderAccountKind,
) -> Result<String, ProviderAccountError> {
    validate_provider_account_provider_id(provider_id)?;
    Ok(format!(
        "{ACCOUNT_LABEL_PREFIX}{provider_id}/{ACCOUNTS_SEGMENT}/{name}/{}",
        kind.as_str()
    ))
}

/// Splits a named-account label into its parts, or `None` for any other label.
pub fn parse_provider_account_label(
    label: &str,
) -> Option<(String, ProviderAccountName, ProviderAccountKind)> {
    let rest = label.strip_prefix(ACCOUNT_LABEL_PREFIX)?;
    let mut parts = rest.split('/');
    let (provider_id, segment, name, kind) =
        (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || segment != ACCOUNTS_SEGMENT {
        return None;
    }
    validate_provider_account_provider_id(provider_id).ok()?;
    Some((
        provider_id.to_string(),
        ProviderAccountName::parse(name).ok()?,
        ProviderAccountKind::parse(kind)?,
    ))
}

/// Whether generic vault reveal/edit must refuse this label.
pub fn is_provider_managed_account_label(label: &str) -> bool {
    parse_provider_account_label(label).is_some_and(|(_, _, kind)| kind.is_provider_managed())
}

/// PF-84 (#609): what a named OpenAI sign-in account (`chatgpt_auth`)
/// holds, recorded in its vault metadata so status can tell the two apart
/// without reading the sign-in itself. Not secret.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAccountSignIn {
    /// ChatGPT tokens.
    Chatgpt,
    /// An OpenAI API key.
    ApiKey,
}

/// A named account's sign-in with what its metadata records it holds
/// (`None`: not recorded).
pub type RecordedSignIn = (Zeroizing<String>, Option<ProviderAccountSignIn>);

/// Reads [`ProviderAccountSignIn`] leniently: a value this build does not
/// know reads as not recorded instead of failing the whole vault index.
pub(crate) fn lenient_sign_in<'de, D>(
    deserializer: D,
) -> Result<Option<ProviderAccountSignIn>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <Option<serde_json::Value> as serde::Deserialize>::deserialize(deserializer)?;
    Ok(value.and_then(|value| serde_json::from_value(value).ok()))
}

/// Metadata for one named account: no secret material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderAccountMeta {
    pub provider_id: String,
    pub name: ProviderAccountName,
    pub kinds: Vec<ProviderAccountKind>,
}

impl Vault {
    /// Lists named accounts (all providers) from vault metadata only.
    pub fn list_provider_accounts(&self) -> Result<Vec<ProviderAccountMeta>, VaultError> {
        let mut accounts: BTreeMap<(String, ProviderAccountName), Vec<ProviderAccountKind>> =
            BTreeMap::new();
        for entry in self.list()? {
            if let Some((provider_id, name, kind)) = parse_provider_account_label(&entry.label)
                && entry.credential_type == kind.credential_type()
            {
                accounts.entry((provider_id, name)).or_default().push(kind);
            }
        }
        Ok(accounts
            .into_iter()
            .map(|((provider_id, name), mut kinds)| {
                kinds.sort();
                ProviderAccountMeta {
                    provider_id,
                    name,
                    kinds,
                }
            })
            .collect())
    }

    /// The kinds a named account holds; empty when the account does not exist.
    pub fn provider_account_kinds(
        &self,
        provider_id: &str,
        name: &ProviderAccountName,
    ) -> Result<Vec<ProviderAccountKind>, VaultError> {
        Ok(self
            .list_provider_accounts()?
            .into_iter()
            .find(|account| account.provider_id == provider_id && &account.name == name)
            .map(|account| account.kinds)
            .unwrap_or_default())
    }

    /// Reads one kind of a named account for its provider integration.
    /// `Ok(None)` when the account does not hold that kind.
    pub fn read_provider_account(
        &self,
        provider_id: &str,
        name: &ProviderAccountName,
        kind: ProviderAccountKind,
    ) -> Result<Option<Zeroizing<String>>, VaultError> {
        let label = account_label(provider_id, name, kind)?;
        self.with_storage_lock(|| {
            // An entry of another type under this label (for example one added
            // by hand through `/vault`) is not this account's material.
            let index = self.load_index()?;
            if index
                .credentials
                .get(&label)
                .is_none_or(|meta| meta.credential_type != kind.credential_type())
            {
                return Ok(None);
            }
            Ok(self
                .read_secret(&label)?
                .filter(|value| !value.trim().is_empty())
                .map(Zeroizing::new))
        })
    }

    /// PF-84 (#609): a named account's `chatgpt_auth` sign-in with what its
    /// metadata records it holds (`None` when not recorded), from one read
    /// of the index. `Ok(None)` when the account holds no sign-in.
    pub fn read_provider_account_sign_in(
        &self,
        provider_id: &str,
        name: &ProviderAccountName,
    ) -> Result<Option<RecordedSignIn>, VaultError> {
        let kind = ProviderAccountKind::ChatgptAuth;
        let label = account_label(provider_id, name, kind)?;
        self.with_storage_lock(|| {
            let index = self.load_index()?;
            let Some(meta) = index
                .credentials
                .get(&label)
                .filter(|meta| meta.credential_type == kind.credential_type())
            else {
                return Ok(None);
            };
            Ok(self
                .read_secret(&label)?
                .filter(|value| !value.trim().is_empty())
                .map(|value| (Zeroizing::new(value), meta.sign_in)))
        })
    }

    /// Creates a new named account holding one kind. Fails, changing
    /// nothing, when the account already holds anything: adding never
    /// replaces another account's credential.
    pub fn create_provider_account(
        &self,
        provider_id: &str,
        name: &ProviderAccountName,
        kind: ProviderAccountKind,
        value: &str,
    ) -> Result<(), VaultError> {
        self.create_provider_account_with_sign_in(
            provider_id,
            name,
            kind,
            value,
            /*sign_in*/ None,
        )
    }

    /// [`Self::create_provider_account`], recording what a sign-in holds
    /// (#609) in the account's metadata.
    pub fn create_provider_account_with_sign_in(
        &self,
        provider_id: &str,
        name: &ProviderAccountName,
        kind: ProviderAccountKind,
        value: &str,
        sign_in: Option<ProviderAccountSignIn>,
    ) -> Result<(), VaultError> {
        let existing = ProviderAccountKind::ALL
            .into_iter()
            .map(|kind| account_label(provider_id, name, kind))
            .collect::<Result<Vec<_>, _>>()?;
        self.store_provider_account(provider_id, name, kind, value, sign_in, Some(&existing))
    }

    /// Stores (or replaces) one kind of a named account.
    pub fn write_provider_account(
        &self,
        provider_id: &str,
        name: &ProviderAccountName,
        kind: ProviderAccountKind,
        value: &str,
    ) -> Result<(), VaultError> {
        self.write_provider_account_with_sign_in(
            provider_id,
            name,
            kind,
            value,
            /*sign_in*/ None,
        )
    }

    /// [`Self::write_provider_account`], recording what a sign-in holds
    /// (#609) in the account's metadata (`None`: not known).
    pub fn write_provider_account_with_sign_in(
        &self,
        provider_id: &str,
        name: &ProviderAccountName,
        kind: ProviderAccountKind,
        value: &str,
        sign_in: Option<ProviderAccountSignIn>,
    ) -> Result<(), VaultError> {
        self.store_provider_account(
            provider_id,
            name,
            kind,
            value,
            sign_in,
            /*refuse_if_any*/ None,
        )
    }

    /// #1311 (D171): rewrites a named account's `chatgpt_auth` sign-in only
    /// while the account still holds one, reading and writing it under one
    /// storage lock, so a removal by another process is never undone by a
    /// save that read the sign-in before it. `change` gets the stored value
    /// and returns the new one with what it holds (#609). `Ok(false)`,
    /// writing nothing, when the account holds no sign-in; `change`'s error
    /// is returned inside, also writing nothing.
    pub fn update_provider_account_sign_in<E>(
        &self,
        provider_id: &str,
        name: &ProviderAccountName,
        change: impl FnOnce(&str) -> Result<(Zeroizing<String>, Option<ProviderAccountSignIn>), E>,
    ) -> Result<Result<bool, E>, VaultError> {
        let kind = ProviderAccountKind::ChatgptAuth;
        let label = account_label(provider_id, name, kind)?;
        self.with_storage_lock(|| {
            let mut index = self.load_index()?;
            if !index
                .credentials
                .get(&label)
                .is_some_and(|meta| meta.credential_type == kind.credential_type())
            {
                return Ok(Ok(false));
            }
            let Some(stored) = self
                .read_secret(&label)?
                .filter(|value| !value.trim().is_empty())
                .map(Zeroizing::new)
            else {
                return Ok(Ok(false));
            };
            let (value, sign_in) = match change(&stored) {
                Ok(changed) => changed,
                Err(error) => return Ok(Err(error)),
            };
            Self::check_account_value(&label, kind, &value)?;
            self.store_locked(&mut index, &label, provider_id, name, kind, &value, sign_in)?;
            Ok(Ok(true))
        })
    }

    /// PF-84 (#609): what a named account's `chatgpt_auth` sign-in holds, as
    /// recorded in its metadata; `None` when it holds none or it was not
    /// recorded. Reads the vault index only, never the sign-in.
    pub fn provider_account_sign_in(
        &self,
        provider_id: &str,
        name: &ProviderAccountName,
    ) -> Result<Option<ProviderAccountSignIn>, VaultError> {
        let kind = ProviderAccountKind::ChatgptAuth;
        let label = account_label(provider_id, name, kind)?;
        self.with_storage_lock(|| {
            Ok(self
                .load_index()?
                .credentials
                .get(&label)
                .filter(|meta| meta.credential_type == kind.credential_type())
                .and_then(|meta| meta.sign_in))
        })
    }

    /// PF-84 (#609): records what a named account's `chatgpt_auth` sign-in
    /// holds, for one saved before it was recorded. Writes only the vault
    /// index, and only when the account holds a sign-in with nothing
    /// recorded: a save records its own kind, which a reader that raced it
    /// must not overwrite. Returns whether it wrote.
    pub fn record_provider_account_sign_in(
        &self,
        provider_id: &str,
        name: &ProviderAccountName,
        sign_in: ProviderAccountSignIn,
    ) -> Result<bool, VaultError> {
        let kind = ProviderAccountKind::ChatgptAuth;
        let label = account_label(provider_id, name, kind)?;
        self.with_storage_lock(|| {
            let mut index = self.load_index()?;
            let Some(meta) = index
                .credentials
                .get_mut(&label)
                .filter(|meta| meta.credential_type == kind.credential_type())
            else {
                return Ok(false);
            };
            if meta.sign_in.is_some() {
                return Ok(false);
            }
            meta.sign_in = Some(sign_in);
            self.secrets
                .apply_batch(&[index_secret_entry(&index)?], &[])?;
            Ok(true)
        })
    }

    /// Writes one kind under the storage lock. With `refuse_if_any`, fails
    /// when any of those labels already exists, checked under the same lock.
    fn store_provider_account(
        &self,
        provider_id: &str,
        name: &ProviderAccountName,
        kind: ProviderAccountKind,
        value: &str,
        sign_in: Option<ProviderAccountSignIn>,
        refuse_if_any: Option<&[String]>,
    ) -> Result<(), VaultError> {
        let label = account_label(provider_id, name, kind)?;
        Self::check_account_value(&label, kind, value)?;
        self.with_storage_lock(|| {
            let mut index = self.load_index()?;
            if refuse_if_any.is_some_and(|labels| {
                labels
                    .iter()
                    .any(|label| index.credentials.contains_key(label))
            }) {
                return Err(VaultError::InvalidLabel(format!(
                    "{provider_id} already has an account `{name}`"
                )));
            }
            self.store_locked(&mut index, &label, provider_id, name, kind, value, sign_in)
        })
    }

    /// The checks every stored account value passes.
    fn check_account_value(
        label: &str,
        kind: ProviderAccountKind,
        value: &str,
    ) -> Result<(), VaultError> {
        if value.trim().is_empty() {
            return Err(VaultError::EmptySecret);
        }
        if kind.is_secret() {
            crate::ensure_not_error_text(value)?;
        }
        crate::protect_stored(label, kind.credential_type(), value)
    }

    /// Writes one kind and the index; the caller holds the storage lock.
    #[allow(clippy::too_many_arguments)]
    fn store_locked(
        &self,
        index: &mut crate::VaultIndex,
        label: &str,
        provider_id: &str,
        name: &ProviderAccountName,
        kind: ProviderAccountKind,
        value: &str,
        sign_in: Option<ProviderAccountSignIn>,
    ) -> Result<(), VaultError> {
        let now = Utc::now().timestamp();
        let created_at = index
            .credentials
            .get(label)
            .map_or(now, |meta| meta.created_at);
        index.credentials.insert(
            label.to_string(),
            VaultCredentialMeta {
                label: label.to_string(),
                credential_type: kind.credential_type(),
                provider: Some(provider_id.to_string()),
                notes: Some(format!("{provider_id} account {name}")),
                revocation_notes: None,
                created_at,
                updated_at: now,
                storage_backend: StorageBackend::EncryptedSecrets,
                // Only a sign-in has one.
                sign_in: sign_in.filter(|_| kind == ProviderAccountKind::ChatgptAuth),
            },
        );
        let mut updates = vec![
            (
                VAULT_SCOPE.clone(),
                secret_name_for(label)?,
                value.to_string(),
            ),
            index_secret_entry(index)?,
        ];
        updates.extend(self.missing_fingerprint_salt()?);
        self.secrets.apply_batch(&updates, &[])?;
        Ok(())
    }

    /// Removes every kind of a named account in one transaction. Returns
    /// whether anything was removed.
    pub fn remove_provider_account(
        &self,
        provider_id: &str,
        name: &ProviderAccountName,
    ) -> Result<bool, VaultError> {
        let labels = ProviderAccountKind::ALL
            .into_iter()
            .map(|kind| account_label(provider_id, name, kind))
            .collect::<Result<Vec<_>, _>>()?;
        self.with_storage_lock(|| {
            let mut index = self.load_index()?;
            let mut removed = false;
            let mut deletes = Vec::with_capacity(labels.len());
            for label in &labels {
                removed |= index.credentials.remove(label).is_some();
                deletes.push((VAULT_SCOPE.clone(), secret_name_for(label)?));
            }
            if !removed {
                return Ok(false);
            }
            let updates = vec![index_secret_entry(&index)?];
            self.secrets.apply_batch(&updates, &deletes)?;
            Ok(true)
        })
    }

    /// Renames a named account in one transaction: every kind moves to the
    /// new name and nothing else changes. Fails when `to` already exists or
    /// `from` does not.
    pub fn rename_provider_account(
        &self,
        provider_id: &str,
        from: &ProviderAccountName,
        to: &ProviderAccountName,
    ) -> Result<(), VaultError> {
        let moves = ProviderAccountKind::ALL
            .into_iter()
            .map(|kind| {
                Ok((
                    account_label(provider_id, from, kind)?,
                    account_label(provider_id, to, kind)?,
                ))
            })
            .collect::<Result<Vec<_>, VaultError>>()?;
        self.with_storage_lock(|| {
            let mut index = self.load_index()?;
            if moves
                .iter()
                .any(|(_, target)| index.credentials.contains_key(target))
            {
                return Err(VaultError::InvalidLabel(format!(
                    "{provider_id} already has an account `{to}`"
                )));
            }
            let now = Utc::now().timestamp();
            let mut updates = Vec::new();
            let mut deletes = Vec::new();
            for (source, target) in &moves {
                let Some(mut meta) = index.credentials.remove(source) else {
                    continue;
                };
                let value = self.read_secret(source)?.ok_or_else(|| {
                    VaultError::Storage(anyhow::anyhow!(
                        "{provider_id} account `{from}` is incomplete"
                    ))
                })?;
                meta.label = target.clone();
                meta.notes = Some(format!("{provider_id} account {to}"));
                meta.updated_at = now;
                index.credentials.insert(target.clone(), meta);
                updates.push((VAULT_SCOPE.clone(), secret_name_for(target)?, value));
                deletes.push((VAULT_SCOPE.clone(), secret_name_for(source)?));
            }
            if updates.is_empty() {
                return Err(VaultError::InvalidLabel(format!(
                    "{provider_id} has no account `{from}`"
                )));
            }
            updates.push(index_secret_entry(&index)?);
            updates.extend(self.missing_fingerprint_salt()?);
            self.secrets.apply_batch(&updates, &deletes)?;
            Ok(())
        })
    }

    /// A 12-hex fingerprint of a named account's material, salted per home so
    /// it cannot be reversed or correlated across machines. Two accounts that
    /// hold the same material share a fingerprint. `Ok(None)` when the account
    /// holds nothing, or when the vault has no salt yet.
    ///
    /// This is a read: it never writes the vault (#474). The salt is created,
    /// and never leaves the vault, when an account is added or renamed, in the
    /// same write. A vault whose accounts were all added before fingerprints
    /// existed has none until its next such write.
    pub fn provider_account_fingerprint(
        &self,
        provider_id: &str,
        name: &ProviderAccountName,
    ) -> Result<Option<String>, VaultError> {
        let labels = ProviderAccountKind::ALL
            .into_iter()
            .map(|kind| Ok((account_label(provider_id, name, kind)?, kind)))
            .collect::<Result<Vec<_>, VaultError>>()?;
        self.with_storage_lock(|| {
            let index = self.load_index()?;
            let mut material = Vec::new();
            for (label, kind) in &labels {
                if index
                    .credentials
                    .get(label)
                    .is_some_and(|meta| meta.credential_type == kind.credential_type())
                    && let Some(value) = self.read_secret(label)?
                {
                    // A sign-in's tokens rotate on refresh; its identity does not.
                    let value = if *kind == ProviderAccountKind::ChatgptAuth {
                        let raw = Zeroizing::new(value);
                        sign_in_identity(&raw).unwrap_or_else(|| raw.to_string())
                    } else {
                        value
                    };
                    material.push((*kind, Zeroizing::new(value)));
                }
            }
            if material.is_empty() {
                return Ok(None);
            }
            let Some(salt) = self.account_fingerprint_salt()? else {
                return Ok(None);
            };
            Ok(Some(fingerprint_digest(&salt, provider_id, &material)))
        })
    }

    /// REG-02: the fingerprint a named account of `provider_id` holding this
    /// sign-in (`auth.json` contents) has, so the default login can be told
    /// apart from, or matched with, a named account without showing any part
    /// of it. Same reads as [`Self::provider_account_fingerprint`]: it never
    /// writes the vault, and is `Ok(None)` when the vault has no salt yet.
    pub fn sign_in_fingerprint(
        &self,
        provider_id: &str,
        sign_in: &str,
    ) -> Result<Option<String>, VaultError> {
        let identity =
            Zeroizing::new(sign_in_identity(sign_in).unwrap_or_else(|| sign_in.to_string()));
        self.with_storage_lock(|| {
            let Some(salt) = self.account_fingerprint_salt()? else {
                return Ok(None);
            };
            Ok(Some(fingerprint_digest(
                &salt,
                provider_id,
                &[(ProviderAccountKind::ChatgptAuth, identity.clone())],
            )))
        })
    }

    /// Reads the per-home fingerprint salt. Callers hold the lock.
    fn account_fingerprint_salt(&self) -> Result<Option<Zeroizing<String>>, VaultError> {
        Ok(self
            .secrets
            .get(&VAULT_SCOPE, &fingerprint_salt_name()?)?
            .map(Zeroizing::new))
    }

    /// The write that creates the fingerprint salt, when the vault has none.
    /// Callers hold the lock and add it to a write they make anyway.
    fn missing_fingerprint_salt(
        &self,
    ) -> Result<Option<(SecretScope, SecretName, String)>, VaultError> {
        if self.account_fingerprint_salt()?.is_some() {
            return Ok(None);
        }
        let salt = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        Ok(Some((VAULT_SCOPE.clone(), fingerprint_salt_name()?, salt)))
    }
}

/// The 12-hex salted digest of an account's material.
fn fingerprint_digest(
    salt: &str,
    provider_id: &str,
    material: &[(ProviderAccountKind, Zeroizing<String>)],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"corbanu-provider-account-fingerprint-v1\0");
    hasher.update(salt.as_bytes());
    hasher.update(b"\0");
    hasher.update(provider_id.as_bytes());
    for (kind, value) in material {
        hasher.update(b"\0");
        hasher.update(kind.as_str().as_bytes());
        hasher.update(b"\0");
        hasher.update(value.trim().as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn fingerprint_salt_name() -> Result<SecretName, VaultError> {
    SecretName::new(FINGERPRINT_SALT_SECRET_NAME)
        .map_err(|error| VaultError::Storage(anyhow::anyhow!("invalid salt secret name: {error}")))
}

/// The per-home fingerprint salt. It is not a credential, so it stays out of
/// the credential index and never shows in `/vault`.
const FINGERPRINT_SALT_SECRET_NAME: &str = "VAULT_PROVIDER_ACCOUNT_FINGERPRINT_SALT";

fn account_label(
    provider_id: &str,
    name: &ProviderAccountName,
    kind: ProviderAccountKind,
) -> Result<String, VaultError> {
    let label = provider_account_label(provider_id, name, kind)
        .map_err(|error| VaultError::InvalidLabel(error.to_string()))?;
    normalize_label(&label)
}

#[cfg(test)]
#[path = "provider_accounts_tests.rs"]
mod tests;

/// The stable identity of a stored sign-in (`auth.json` contents), so its
/// fingerprint survives token refreshes: the ChatGPT workspace and user, or
/// the API key of an API-key sign-in. `None` when it cannot be read.
fn sign_in_identity(raw: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(raw).ok()?;
    if let Some(tokens) = json.get("tokens").filter(|tokens| !tokens.is_null()) {
        let workspace = tokens.get("account_id").and_then(serde_json::Value::as_str);
        let user = tokens
            .get("id_token")
            .and_then(serde_json::Value::as_str)
            .and_then(|jwt| jwt.split('.').nth(1))
            .and_then(base64url_decode)
            .and_then(|payload| serde_json::from_slice::<serde_json::Value>(&payload).ok())
            .and_then(|claims| {
                claims
                    .get("https://api.openai.com/auth")
                    .and_then(|auth| auth.get("chatgpt_user_id"))
                    .or_else(|| claims.get("sub"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            });
        if workspace.is_none() && user.is_none() {
            return None;
        }
        return Some(format!(
            "chatgpt\0{}\0{}",
            workspace.unwrap_or_default(),
            user.unwrap_or_default()
        ));
    }
    json.get("OPENAI_API_KEY")
        .and_then(serde_json::Value::as_str)
        .map(|key| format!("api-key\0{key}"))
}

/// Unpadded base64url, as in a JWT.
fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(input.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in input.trim_end_matches('=').bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(output)
}
