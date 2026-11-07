//! Corbanu Terminal credential vault.
//!
//! This crate provides a labeled credential store built on top of [`codex_secrets::SecretsManager`].
//! Credentials are encrypted at rest using the existing age-encrypted secrets substrate (the same
//! one Codex uses for managed secrets and auth material), keyed by an OS keyring-stored passphrase.
//!
//! # Design
//!
//! The vault stores two kinds of records in the managed-secrets namespace:
//!
//! 1. A **metadata index** (`VAULT_INDEX`) — a JSON map of `label -> VaultCredentialMeta`. This
//!    contains no secret material and is used to list and show credentials without ever touching
//!    raw secrets.
//! 2. One **secret record** per label (`vault/<label>`) holding the raw secret value.
//!
//! Both records live inside the age-encrypted `secrets/local.age` file, so everything is encrypted
//! at rest. Raw secret material is only returned by explicit [`Vault::reveal`] / [`Vault::export`]
//! calls; listing and metadata access never touch it.
//!
//! # v0 scope
//!
//! This is a v0 slice: it covers encrypted labeled storage and a provider-key resolver/writer that
//! is migration-compatible with the legacy plaintext `provider_auth.json`. Lock/unlock is implicit
//! — the OS keyring passphrase gates decryption — so the vault is "unlocked" for the duration that
//! the keyring is available, with no repeated secret entry.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;

use anyhow::Context;
use anyhow::Result;
use chrono::DateTime;
use chrono::Utc;
use codex_keyring_store::KeyringStore;
use codex_secrets::LocalSecretsNamespace;
use codex_secrets::SecretName;
use codex_secrets::SecretScope;
use codex_secrets::SecretsBackendKind;
use codex_secrets::SecretsManager;
use codex_secrets::local_keyring_fallback_path;
use codex_security_policy::SecurityLevel;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use strum_macros::AsRefStr;
use strum_macros::Display;
use thiserror::Error;
use zeroize::Zeroizing;

mod capability;
mod claude_auth;
mod credential_panic;
mod provider_accounts;

pub use capability::ScopedCredentialCallbackError;
pub use capability::ScopedCredentialError;
pub use capability::SystemVaultBrokerClock;
pub use capability::VaultBrokerBackend;
pub use capability::VaultBrokerBackendError;
pub use capability::VaultBrokerClock;
pub use capability::VaultBrokerTransport;
pub use capability::VaultCredentialRef;
pub use claude_auth::CREDENTIALS_FILE_CLAUDE_AUTH_SOURCE_ID;
pub use claude_auth::ClaudeAuthHealth;
pub use claude_auth::ClaudeAuthResolution;
pub use claude_auth::ClaudeAuthSelection;
pub use claude_auth::ClaudeAuthSource;
pub use claude_auth::ClaudeAuthSourceMetadata;
pub use claude_auth::ClaudeAuthStoreKind;
pub use claude_auth::ClaudeSubscriptionTokenError;
pub use claude_auth::ENVIRONMENT_CLAUDE_AUTH_SOURCE_ID;
pub use claude_auth::MACOS_KEYCHAIN_CLAUDE_AUTH_SOURCE_ID;
pub use claude_auth::MANAGED_CLAUDE_AUTH_SOURCE_ID;
pub use claude_auth::MANAGED_CLAUDE_TOKEN_LABEL;
pub use claude_auth::ManagedClaudeTokenStatus;
pub use claude_auth::claude_auth_selection_revision_path;
pub use claude_auth::claude_auth_selection_sentinel_path;
pub use claude_auth::claude_code_macos_keychain_service;
pub use claude_auth::claude_environment_token_authority_id;
pub use claude_auth::claude_login_authority_id;
pub use claude_auth::credentials_file_claude_auth_source_id;
pub use claude_auth::macos_keychain_claude_auth_source_id;
pub use claude_auth::resolve_claude_auth_source;
pub use credential_panic::scoped_credential_callback_active;
pub use provider_accounts::DEFAULT_PROVIDER_ACCOUNT;
pub use provider_accounts::ProviderAccountError;
pub use provider_accounts::ProviderAccountKind;
pub use provider_accounts::ProviderAccountMeta;
pub use provider_accounts::ProviderAccountName;
pub use provider_accounts::ProviderAccountSignIn;
pub use provider_accounts::RecordedSignIn;
pub use provider_accounts::is_provider_managed_account_label;
pub use provider_accounts::parse_provider_account_label;
pub use provider_accounts::parse_provider_account_selection;
pub use provider_accounts::provider_account_label;
pub use provider_accounts::validate_aws_profile_name;
pub use provider_accounts::validate_provider_account_provider_id;

#[cfg(test)]
mod lock_failure_tests;
#[cfg(test)]
mod tests;

/// Stable secret name holding the vault metadata index.
const VAULT_INDEX_SECRET_NAME: &str = "VAULT_INDEX";

/// File used to serialize complete vault transactions across threads and processes.
const VAULT_LOCK_FILE_NAME: &str = ".vault.lock";

/// Version of the on-disk vault index schema. Bumped on breaking changes to
/// [`VaultIndex`] / [`VaultCredentialMeta`].
const VAULT_SCHEMA_VERSION: u32 = 1;

/// Canonical scope used for all vault records. Vault entries are global rather than
/// environment-scoped so credentials can be resolved from any working directory.
const VAULT_SCOPE: SecretScope = SecretScope::Global;

/// Errors returned by vault operations.
#[derive(Debug, Error)]
pub enum VaultError {
    /// A credential with the given label already exists.
    #[error("credential labeled {label:?} already exists")]
    CredentialExists { label: String },
    /// No credential with the given label was found.
    #[error("no credential labeled {label:?}")]
    NotFound { label: String },
    /// The supplied label failed validation.
    #[error("invalid credential label: {0}")]
    InvalidLabel(String),
    /// The raw secret value was empty.
    #[error("secret value must not be empty")]
    EmptySecret,
    /// #855: the value is the text of a vault error message (copied from the
    /// transcript and pasted in place of the credential), not a credential.
    #[error(
        "the value entered is the text of a vault error message, not a credential; nothing was stored (copy the credential again and paste it)"
    )]
    ErrorTextAsSecret,
    /// #855: the vault's storage could not be reached (for example the
    /// Corbanu home became unreadable). Nothing was read or written.
    #[error("failed to {operation} {}: {error}", path.display())]
    StorageUnavailable {
        operation: &'static str,
        path: PathBuf,
        /// Shown in the message, so not also reported as the error's source.
        error: std::io::Error,
    },
    /// A provider-managed credential is available only to its scoped integration.
    #[error("credential labeled {label:?} can only be used by its provider integration")]
    ProviderManagedCredential { label: String },
    /// The credential is intentionally excluded from generic automation.
    #[error(
        "credential labeled {label:?} has type {credential_type}, which cannot be used by vault auth-helper"
    )]
    ProgrammaticUseDenied {
        label: String,
        credential_type: CredentialType,
    },
    /// Raw programmatic export is unavailable outside Permissive.
    #[error(
        "vault auth-helper is unavailable under {level}; use a brokered credential label instead"
    )]
    ProgrammaticUseSecurityLevelDenied { level: SecurityLevel },
    /// An underlying storage/encryption error occurred.
    #[error(transparent)]
    Storage(#[from] anyhow::Error),
}

/// The supported credential types for the v0 vault.
///
/// These cover API keys, tokens, auth tuples, and keying material used across
/// model providers, exchanges, and deployment flows.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Display, AsRefStr,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum CredentialType {
    /// A plain API key string (for example `sk-...`).
    ApiKey,
    /// A bearer token used in an `Authorization: Bearer` header.
    BearerToken,
    /// A username/password pair encoded as `username:password`.
    BasicAuth,
    /// OAuth client credentials (client id + secret + optional metadata).
    OauthClient,
    /// A PEM/DER-encoded private key.
    CryptoPrivateKey,
    /// A BIP-39 seed phrase (space- or newline-separated words).
    SeedPhrase,
    /// A JSON keystore blob (for example geth/web3 keystore files).
    KeystoreJson,
    /// An RPC endpoint key.
    RpcKey,
    /// An exchange API key (often key + secret + passphrase).
    ExchangeKey,
    /// A deployment key / CI secret.
    DeploymentKey,
    /// Any other manually-managed secret string.
    ManualSecret,
}

impl CredentialType {
    /// Human-readable description shown in vault listings.
    pub fn description(self) -> &'static str {
        match self {
            CredentialType::ApiKey => "API key",
            CredentialType::BearerToken => "bearer token",
            CredentialType::BasicAuth => "basic auth (username:password)",
            CredentialType::OauthClient => "OAuth client credentials",
            CredentialType::CryptoPrivateKey => "crypto private key",
            CredentialType::SeedPhrase => "seed phrase",
            CredentialType::KeystoreJson => "keystore JSON",
            CredentialType::RpcKey => "RPC key",
            CredentialType::ExchangeKey => "exchange key",
            CredentialType::DeploymentKey => "deployment key",
            CredentialType::ManualSecret => "manual secret",
        }
    }

    /// Whether this credential class may be resolved for use-time automation.
    ///
    /// Key custody material stays behind explicit user reveal/export flows. Other
    /// credentials may be passed directly to a command without requiring a
    /// provider-specific label or integration.
    pub fn permits_programmatic_use(self) -> bool {
        match self {
            CredentialType::ApiKey
            | CredentialType::BearerToken
            | CredentialType::BasicAuth
            | CredentialType::OauthClient
            | CredentialType::RpcKey
            | CredentialType::ExchangeKey
            | CredentialType::DeploymentKey
            | CredentialType::ManualSecret => true,
            CredentialType::CryptoPrivateKey
            | CredentialType::SeedPhrase
            | CredentialType::KeystoreJson => false,
        }
    }
}

/// Storage backend that backs a credential. v0 always uses the encrypted
/// secrets-managed store; the enum leaves room for future backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum StorageBackend {
    /// Encrypted age store managed by `codex_secrets`.
    #[default]
    EncryptedSecrets,
}

/// Non-secret metadata for a single stored credential.
///
/// This is safe to surface in listings and `/vault show` output. Raw secret
/// material is never included here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VaultCredentialMeta {
    /// Human-readable label, unique within the vault.
    pub label: String,
    /// Categorization of the credential.
    #[serde(rename = "type")]
    pub credential_type: CredentialType,
    /// Optional provider/service this credential is for (for example "ambient", "openrouter").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Free-form notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Optional revocation / recovery guidance (for example "rotate at https://...").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation_notes: Option<String>,
    /// When the credential was first stored (Unix seconds, UTC).
    pub created_at: i64,
    /// When the credential was last updated (Unix seconds, UTC).
    pub updated_at: i64,
    /// Where the credential is physically stored.
    #[serde(default)]
    pub storage_backend: StorageBackend,
    /// PF-84 (#609): for a named OpenAI sign-in account, whether it holds a
    /// ChatGPT sign-in or an OpenAI API key (never the value). Recorded when
    /// the sign-in is saved, or on its next use for one saved before.
    /// A value this build does not know (from a later one) reads as not
    /// recorded, so it never makes the index unreadable.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "provider_accounts::lenient_sign_in"
    )]
    pub sign_in: Option<ProviderAccountSignIn>,
}

/// On-disk vault index: a versioned map of label -> metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct VaultIndex {
    version: u32,
    #[serde(default)]
    credentials: BTreeMap<String, VaultCredentialMeta>,
}

impl Default for VaultIndex {
    fn default() -> Self {
        Self {
            version: VAULT_SCHEMA_VERSION,
            credentials: BTreeMap::new(),
        }
    }
}

/// Request to add a credential to the vault.
#[derive(Debug, Clone)]
pub struct AddCredential {
    pub label: String,
    pub credential_type: CredentialType,
    pub provider: Option<String>,
    pub notes: Option<String>,
    pub revocation_notes: Option<String>,
    /// The raw secret value. This is only held in memory until stored.
    pub secret: String,
}

/// The Corbanu Terminal credential vault.
///
/// Wraps [`SecretsManager`] (managed-secrets namespace) to provide labeled,
/// encrypted-at-rest credential storage with explicit reveal/export controls.
#[derive(Clone)]
pub struct Vault {
    secrets: SecretsManager,
    codex_home: PathBuf,
    uses_default_keyring: bool,
}

/// See [`Vault::storage_marker`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VaultStorageMarker {
    len: u64,
    modified: std::time::SystemTime,
    inode: u64,
}

/// Non-secret description of where the encrypted vault passphrase is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultKeyStorage {
    NotInitialized,
    OperatingSystemKeyring,
    LocalFileFallback,
    ConfiguredKeyring,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault").finish_non_exhaustive()
    }
}

impl Vault {
    /// Create a vault backed by the default OS keyring store.
    pub fn new(codex_home: PathBuf) -> Self {
        let secrets = SecretsManager::new(codex_home.clone(), SecretsBackendKind::Local);
        Self {
            secrets,
            codex_home,
            uses_default_keyring: true,
        }
    }

    /// Create a vault backed by a custom keyring store (used by tests and
    /// environments without an OS keyring).
    pub fn new_with_keyring_store(
        codex_home: PathBuf,
        keyring_store: Arc<dyn KeyringStore>,
    ) -> Self {
        let secrets = SecretsManager::new_with_keyring_store_and_namespace(
            codex_home.clone(),
            SecretsBackendKind::Local,
            keyring_store,
            LocalSecretsNamespace::ManagedSecrets,
        );
        Self {
            secrets,
            codex_home,
            uses_default_keyring: false,
        }
    }

    /// D171 (#1286): a cheap change marker of the encrypted store (size,
    /// modification time and, on Unix, inode of `secrets/local.age`), read
    /// without the vault lock or the passphrase. Every write replaces the
    /// file, so an unchanged marker means no process changed the vault since
    /// it was taken. `None` when the store is missing or its metadata cannot
    /// be read.
    pub fn storage_marker(&self) -> Option<VaultStorageMarker> {
        let metadata = std::fs::metadata(self.codex_home.join("secrets").join("local.age")).ok()?;
        #[cfg(unix)]
        let inode = std::os::unix::fs::MetadataExt::ino(&metadata);
        #[cfg(not(unix))]
        let inode = 0;
        Some(VaultStorageMarker {
            len: metadata.len(),
            modified: metadata.modified().ok()?,
            inode,
        })
    }

    /// Report the active passphrase storage mechanism without opening or
    /// exposing the passphrase itself.
    pub fn key_storage(&self) -> VaultKeyStorage {
        if !self.uses_default_keyring {
            return VaultKeyStorage::ConfiguredKeyring;
        }
        if local_keyring_fallback_path(&self.codex_home).is_file() {
            VaultKeyStorage::LocalFileFallback
        } else if self.codex_home.join("secrets").join("local.age").is_file() {
            VaultKeyStorage::OperatingSystemKeyring
        } else {
            VaultKeyStorage::NotInitialized
        }
    }

    /// Add a new credential. Returns [`VaultError::CredentialExists`] if the label is taken.
    pub fn add(&self, entry: AddCredential) -> Result<(), VaultError> {
        let AddCredential {
            label: entry_label,
            credential_type,
            provider,
            notes,
            revocation_notes,
            secret,
        } = entry;
        let label = normalize_label(&entry_label)?;
        let secret = Zeroizing::new(secret);
        if label == MANAGED_CLAUDE_TOKEN_LABEL || is_provider_managed_account_label(&label) {
            return Err(VaultError::ProviderManagedCredential { label });
        }
        if secret.trim().is_empty() {
            return Err(VaultError::EmptySecret);
        }
        ensure_not_error_text(&secret)?;
        self.with_storage_lock(|| {
            let mut index = self.load_index()?;
            if index.credentials.contains_key(&label) {
                return Err(VaultError::CredentialExists { label });
            }
            protect_stored(&label, credential_type, &secret)?;

            let now = Utc::now().timestamp();
            let meta = VaultCredentialMeta {
                label: label.clone(),
                credential_type,
                provider,
                notes,
                revocation_notes,
                created_at: now,
                updated_at: now,
                storage_backend: StorageBackend::EncryptedSecrets,
                sign_in: None,
            };

            index.credentials.insert(label.clone(), meta);
            let updates = vec![
                (
                    VAULT_SCOPE.clone(),
                    secret_name_for(&label)?,
                    secret.to_string(),
                ),
                index_secret_entry(&index)?,
            ];
            self.secrets.apply_batch(&updates, &[])?;
            Ok(())
        })
    }

    /// Update an existing credential's secret and optional metadata fields.
    /// Only fields that are `Some` are overwritten.
    pub fn update(
        &self,
        label: &str,
        secret: Option<String>,
        provider: Option<Option<String>>,
        notes: Option<Option<String>>,
        revocation_notes: Option<Option<String>>,
    ) -> Result<VaultCredentialMeta, VaultError> {
        let label = normalize_label(label)?;
        if label == MANAGED_CLAUDE_TOKEN_LABEL || is_provider_managed_account_label(&label) {
            drop(secret.map(Zeroizing::new));
            return Err(VaultError::ProviderManagedCredential { label });
        }
        self.with_storage_lock(|| {
            let mut index = self.load_index()?;
            let meta = index
                .credentials
                .get_mut(&label)
                .ok_or_else(|| VaultError::NotFound {
                    label: label.clone(),
                })?;

            let secret_update = if let Some(secret) = secret {
                let secret = Zeroizing::new(secret);
                if secret.trim().is_empty() {
                    return Err(VaultError::EmptySecret);
                }
                ensure_not_error_text(&secret)?;
                protect_stored(&label, meta.credential_type, &secret)?;
                Some((
                    VAULT_SCOPE.clone(),
                    secret_name_for(&label)?,
                    secret.to_string(),
                ))
            } else {
                None
            };
            if let Some(provider) = provider {
                meta.provider = provider;
            }
            if let Some(notes) = notes {
                meta.notes = notes;
            }
            if let Some(revocation_notes) = revocation_notes {
                meta.revocation_notes = revocation_notes;
            }
            meta.updated_at = Utc::now().timestamp();
            let updated = meta.clone();
            let mut updates = Vec::with_capacity(2);
            if let Some(secret_update) = secret_update {
                updates.push(secret_update);
            }
            updates.push(index_secret_entry(&index)?);
            self.secrets.apply_batch(&updates, &[])?;
            Ok(updated)
        })
    }

    /// Delete a credential and its secret. Returns `true` if anything was removed.
    pub fn delete(&self, label: &str) -> Result<bool, VaultError> {
        let normalized = normalize_label(label)?;
        if normalized == MANAGED_CLAUDE_TOKEN_LABEL {
            return self.remove_managed_claude_subscription_token();
        }
        self.delete_normalized(&normalized)
    }

    fn delete_normalized(&self, normalized: &str) -> Result<bool, VaultError> {
        self.with_storage_lock(|| {
            let mut index = self.load_index()?;
            let removed_meta = index.credentials.remove(normalized).is_some();
            let updates = if removed_meta {
                vec![index_secret_entry(&index)?]
            } else {
                Vec::new()
            };
            let deletes = vec![(VAULT_SCOPE.clone(), secret_name_for(normalized)?)];
            let removed_secrets = self.secrets.apply_batch(&updates, &deletes)?;
            Ok(removed_meta || removed_secrets > 0)
        })
    }

    /// Delete several credentials in one encrypted-store transaction.
    pub fn delete_many(&self, labels: &[String]) -> Result<usize, VaultError> {
        let normalized = labels
            .iter()
            .map(|label| normalize_label(label))
            .collect::<Result<Vec<_>, _>>()?;
        if normalized
            .iter()
            .any(|label| label == MANAGED_CLAUDE_TOKEN_LABEL)
        {
            return Err(VaultError::ProviderManagedCredential {
                label: MANAGED_CLAUDE_TOKEN_LABEL.to_string(),
            });
        }
        self.with_storage_lock(|| {
            let mut index = self.load_index()?;
            let mut secret_entries = Vec::with_capacity(normalized.len());
            let mut removed_metadata = 0;
            for label in &normalized {
                removed_metadata += usize::from(index.credentials.remove(label).is_some());
                secret_entries.push((VAULT_SCOPE.clone(), secret_name_for(label)?));
            }
            let updates = if removed_metadata > 0 {
                vec![index_secret_entry(&index)?]
            } else {
                Vec::new()
            };
            let removed_secrets = self.secrets.apply_batch(&updates, &secret_entries)?;
            Ok(removed_metadata.max(removed_secrets))
        })
    }

    /// List metadata for all stored credentials (no secret material).
    pub fn list(&self) -> Result<Vec<VaultCredentialMeta>, VaultError> {
        self.with_storage_lock(|| {
            let index = self.load_index()?;
            Ok(index.credentials.into_values().collect())
        })
    }

    /// Return metadata for a single credential (no secret material).
    pub fn show(&self, label: &str) -> Result<VaultCredentialMeta, VaultError> {
        let normalized = normalize_label(label)?;
        self.with_storage_lock(|| {
            let index = self.load_index()?;
            index
                .credentials
                .get(&normalized)
                .cloned()
                .ok_or_else(|| VaultError::NotFound {
                    label: normalized.clone(),
                })
        })
    }

    /// Reveal the raw secret value for a credential. Only call this from an
    /// explicit user action (`/vault credential reveal` / `/vault credential export`).
    pub fn reveal(&self, label: &str) -> Result<String, VaultError> {
        let normalized = normalize_label(label)?;
        if normalized == MANAGED_CLAUDE_TOKEN_LABEL
            || is_provider_managed_account_label(&normalized)
        {
            return Err(VaultError::ProviderManagedCredential { label: normalized });
        }
        self.with_storage_lock(|| {
            // Validate the label exists in the index before attempting decryption.
            let index = self.load_index()?;
            let Some(credential_type) = index
                .credentials
                .get(&normalized)
                .map(|meta| meta.credential_type)
            else {
                return Err(VaultError::NotFound { label: normalized });
            };
            let secret = self
                .read_secret(&normalized)?
                .ok_or_else(|| VaultError::NotFound {
                    label: normalized.clone(),
                })?;
            protect_revealed(&normalized, credential_type, &secret)?;
            Ok(secret)
        })
    }

    /// Resolve an operational credential for immediate programmatic use.
    ///
    /// The caller must keep the returned value out of model context, logs, and
    /// persistent process state. Key custody material is rejected even when it
    /// exists in the vault.
    pub fn reveal_for_programmatic_use(
        &self,
        label: &str,
        security_level: SecurityLevel,
    ) -> Result<String, VaultError> {
        if security_level != SecurityLevel::Permissive {
            return Err(VaultError::ProgrammaticUseSecurityLevelDenied {
                level: security_level,
            });
        }
        let normalized = normalize_label(label)?;
        if normalized == MANAGED_CLAUDE_TOKEN_LABEL
            || is_provider_managed_account_label(&normalized)
        {
            return Err(VaultError::ProviderManagedCredential { label: normalized });
        }
        self.with_storage_lock(|| {
            let index = self.load_index()?;
            let meta = index
                .credentials
                .get(&normalized)
                .ok_or_else(|| VaultError::NotFound {
                    label: normalized.clone(),
                })?;
            if !meta.credential_type.permits_programmatic_use() {
                return Err(VaultError::ProgrammaticUseDenied {
                    label: normalized,
                    credential_type: meta.credential_type,
                });
            }
            let credential_type = meta.credential_type;
            let secret = self
                .read_secret(&normalized)?
                .ok_or_else(|| VaultError::NotFound {
                    label: normalized.clone(),
                })?;
            protect_revealed(&normalized, credential_type, &secret)?;
            Ok(secret)
        })
    }

    /// #434: registers every stored value with the armed output gate, so a
    /// value stored before this process started (or by another process) is
    /// removed from output although this process never revealed it. Custody
    /// material (seed phrases, private keys, keystores) and the non-secret
    /// parts of provider accounts are not loaded ([`protected_while_stored`]).
    /// The vault file is decrypted once. Returns the labels that could not be
    /// protected (for example a value too short to tell apart from ordinary
    /// text). Does nothing while the gate is not armed or the vault does not
    /// exist.
    pub fn protect_stored_values(&self) -> Result<Vec<String>, VaultError> {
        if codex_secret_broker::output_gate::active().is_none()
            || self.key_storage() == VaultKeyStorage::NotInitialized
        {
            return Ok(Vec::new());
        }
        let values = self.with_storage_lock(|| {
            let index = self.load_index()?;
            let wanted = index
                .credentials
                .values()
                .filter(|meta| protected_while_stored(&meta.label, meta.credential_type))
                .map(|meta| {
                    Ok((
                        meta.label.clone(),
                        meta.credential_type,
                        secret_name_for(&meta.label)?,
                    ))
                })
                .collect::<Result<Vec<_>, VaultError>>()?;
            let names = wanted
                .iter()
                .map(|(_, _, name)| (VAULT_SCOPE.clone(), name.clone()))
                .collect::<Vec<_>>();
            let secrets = self.secrets.get_many(&names)?;
            Ok(wanted
                .into_iter()
                .zip(secrets)
                .filter_map(|((label, credential_type, _), secret)| {
                    secret.map(|secret| (label, credential_type, Zeroizing::new(secret)))
                })
                .collect::<Vec<_>>())
        })?;
        // Registered outside the vault lock. One value the gate cannot admit
        // does not leave the others unprotected; it is reported by label.
        Ok(values
            .iter()
            .filter(|(label, credential_type, secret)| {
                register_with_output_gate(label, *credential_type, secret).is_err()
            })
            .map(|(label, _, _)| label.clone())
            .collect())
    }

    /// Whether a credential with the given label exists.
    pub fn exists(&self, label: &str) -> Result<bool, VaultError> {
        let normalized = normalize_label(label)?;
        self.with_storage_lock(|| {
            let index = self.load_index()?;
            Ok(index.credentials.contains_key(&normalized))
        })
    }

    // ---- index helpers ----

    fn with_storage_lock<T>(
        &self,
        operation: impl FnOnce() -> Result<T, VaultError>,
    ) -> Result<T, VaultError> {
        let secrets_dir = self.codex_home.join("secrets");
        std::fs::create_dir_all(&secrets_dir).map_err(|source| {
            storage_unavailable("create vault secrets directory", &secrets_dir, source)
        })?;
        let lock_path = secrets_dir.join(VAULT_LOCK_FILE_NAME);
        let lock_file = match open_lock_file(&lock_path) {
            // PF-27-S09: the Windows credential broker's token cannot write
            // the user's files. A read-only handle locks the existing lock
            // file just as exclusively; writes to the vault itself still fail.
            #[cfg(windows)]
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                OpenOptions::new().read(true).open(&lock_path)
            }
            opened => opened,
        }
        .map_err(|source| storage_unavailable("open vault lock", &lock_path, source))?;
        lock_file
            .lock()
            .map_err(|source| storage_unavailable("lock vault", &lock_path, source))?;
        operation()
    }

    fn load_index(&self) -> Result<VaultIndex, VaultError> {
        let name = index_secret_name()?;
        match self.secrets.get(&VAULT_SCOPE, &name)? {
            Some(serialized) => {
                let mut index: VaultIndex = serde_json::from_str(&serialized)
                    .context("failed to deserialize vault index")?;
                if index.version == 0 {
                    index.version = VAULT_SCHEMA_VERSION;
                }
                if index.version > VAULT_SCHEMA_VERSION {
                    return Err(VaultError::Storage(anyhow::anyhow!(
                        "vault index version {} is newer than supported version {}",
                        index.version,
                        VAULT_SCHEMA_VERSION
                    )));
                }
                Ok(index)
            }
            None => Ok(VaultIndex::default()),
        }
    }

    // ---- per-credential secret helpers ----

    fn read_secret(&self, label: &str) -> Result<Option<String>, VaultError> {
        let name = secret_name_for(label)?;
        Ok(self.secrets.get(&VAULT_SCOPE, &name)?)
    }
}

fn open_lock_file(lock_path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(test)]
    if let Some(errno) = lock_failure_tests::injected_lock_open_errno() {
        return Err(std::io::Error::from_raw_os_error(errno));
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)
}

/// Receives a vault storage failure caused by missing permission (EPERM or
/// EACCES): the operation, the path and the error. Never a credential.
pub type StorageFailureReporter = fn(operation: &str, path: &Path, error: &std::io::Error);

static STORAGE_FAILURE_REPORTER: OnceLock<StorageFailureReporter> = OnceLock::new();

/// #855: installs the process-wide reporter for vault storage failures caused
/// by missing permission (the `corbanu` binary records them in the #616
/// home-access log). Only the first call takes effect; without one, they are
/// only logged.
pub fn set_storage_failure_reporter(reporter: StorageFailureReporter) {
    let _ = STORAGE_FAILURE_REPORTER.set(reporter);
}

fn storage_unavailable(operation: &'static str, path: &Path, source: std::io::Error) -> VaultError {
    tracing::warn!(
        operation,
        path = %path.display(),
        errno = source.raw_os_error(),
        error = %source,
        "vault storage unavailable"
    );
    if source.kind() == std::io::ErrorKind::PermissionDenied
        && let Some(report) = STORAGE_FAILURE_REPORTER.get()
    {
        report(operation, path, &source);
    }
    VaultError::StorageUnavailable {
        operation,
        path: path.to_path_buf(),
        error: source,
    }
}

/// #855: phrases from the vault's own error messages, as shown in the
/// transcript. A value containing one is the text of an error that was copied
/// and pasted in place of the credential (the entry field is masked, so the
/// person cannot see it); stored, it would fail much later, far from the
/// cause.
const ERROR_TEXT_MARKERS: &[&str] = &[
    "failed to add vault credential",
    "failed to replace vault credential",
    "failed to open vault lock",
    "failed to lock vault",
    "failed to create vault secrets directory",
    "vault add task failed",
    "vault replacement task failed",
];

/// Whether `value` is (or contains) the text of a vault error message rather
/// than a credential, ignoring ASCII case and line wrapping. See
/// [`ERROR_TEXT_MARKERS`].
pub fn is_vault_error_text(value: &str) -> bool {
    let value = normalized_for_matching(value);
    ERROR_TEXT_MARKERS
        .iter()
        .any(|marker| value.contains(marker))
}

/// `value` with ASCII letters lowercased and every run of whitespace (such as
/// the line breaks of an error copied from a wrapped transcript) made one
/// space, for matching against error text. A matching helper only: it
/// neither sanitizes nor redacts. The copy is zeroized when dropped and never
/// reallocated, since it can hold a credential.
pub fn normalized_for_matching(value: &str) -> Zeroizing<String> {
    let mut normalized = Zeroizing::new(String::with_capacity(value.len()));
    let mut words = value.split_whitespace();
    if let Some(first) = words.next() {
        normalized.extend(first.chars().map(|c| c.to_ascii_lowercase()));
        for word in words {
            normalized.push(' ');
            normalized.extend(word.chars().map(|c| c.to_ascii_lowercase()));
        }
    }
    normalized
}

pub(crate) fn ensure_not_error_text(secret: &str) -> Result<(), VaultError> {
    if is_vault_error_text(secret) {
        Err(VaultError::ErrorTextAsSecret)
    } else {
        Ok(())
    }
}

/// Validate and canonicalize a credential label.
///
/// Labels must be 1..=128 chars and may contain ASCII alphanumerics, `-`, `_`,
/// `.`, and `/` (so provider-scoped labels like `ambient/api-key` work). They
/// are trimmed for stable lookups; case-distinct labels remain distinct.
fn normalize_label(raw: &str) -> Result<String, VaultError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(VaultError::InvalidLabel(
            "label must not be empty".to_string(),
        ));
    }
    if trimmed.len() > 128 {
        return Err(VaultError::InvalidLabel(
            "label must be at most 128 chars".to_string(),
        ));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '/')
    {
        return Err(VaultError::InvalidLabel(
            "label may only contain a-z, 0-9, '-', '_', '.', '/'".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

/// Convert a label into a collision-resistant `SecretName`.
///
/// The label is SHA-256 hashed and rendered as uppercase hex (A-F, 0-9), which satisfies the
/// `SecretName` charset (`A-Z`, `0-9`, `_`). Hashing (rather than uppercasing the raw label)
/// guarantees that distinct labels like `a/b`, `a.b`, `a_b`, `a-b`, `MyLabel`, and `mylabel` map
/// to distinct secret records, so adding one can never overwrite another's stored secret.
fn secret_name_for(label: &str) -> Result<SecretName, VaultError> {
    let mut hasher = Sha256::new();
    hasher.update(label.as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:X}");
    // 64 hex chars is collision-resistant; truncate to a still-ample 32 chars (128 bits).
    let fingerprint = hex.get(..32).unwrap_or(hex.as_str());
    let candidate = format!("VAULT_{fingerprint}");
    SecretName::new(&candidate).map_err(|err| VaultError::InvalidLabel(err.to_string()))
}

fn index_secret_name() -> Result<SecretName, VaultError> {
    SecretName::new(VAULT_INDEX_SECRET_NAME).map_err(|err| {
        VaultError::Storage(anyhow::anyhow!("invalid vault index secret name: {err}"))
    })
}

fn index_secret_entry(index: &VaultIndex) -> Result<(SecretScope, SecretName, String), VaultError> {
    let name = index_secret_name()?;
    let serialized = serde_json::to_string(index).context("failed to serialize vault index")?;
    Ok((VAULT_SCOPE.clone(), name, serialized))
}

/// PF-28-S01: once the output gate is armed, every value this process reveals
/// is registered before it is returned, so no model, tool, transcript or log
/// sink can carry it. Seed phrases and private keys are withheld whole. If the
/// gate cannot admit the value, the credential is not released.
fn protect_revealed(
    label: &str,
    credential_type: CredentialType,
    secret: &str,
) -> Result<(), VaultError> {
    register_with_output_gate(label, credential_type, secret).map_err(|err| {
        VaultError::Storage(anyhow::anyhow!(
            "vault:{label} was not released because the secret output gate cannot protect it: {err}"
        ))
    })
}

/// #434: a value stored while the output gate is armed is protected at once,
/// not only after this process reveals it, because the person can put the
/// same value in a file an agent reads. If the gate cannot admit it, it is not
/// stored. A value too short to protect is stored unprotected, as before:
/// it cannot be told apart from ordinary text and is never released while
/// the gate is armed ([`protect_revealed`]).
pub(crate) fn protect_stored(
    label: &str,
    credential_type: CredentialType,
    secret: &str,
) -> Result<(), VaultError> {
    use codex_secret_broker::output_gate::RegisterError;
    if !protected_while_stored(label, credential_type) {
        return Ok(());
    }
    match register_with_output_gate(label, credential_type, secret) {
        // Stored, but never released while the gate is armed
        // (`protect_revealed`); the next vault read reports it by label.
        Ok(()) | Err(RegisterError::TooShort | RegisterError::CapacityExhausted { .. }) => Ok(()),
        Err(err) => Err(VaultError::Storage(anyhow::anyhow!(
            "vault:{label} was not stored because the secret output gate cannot protect it: {err}"
        ))),
    }
}

/// #434: whether a stored value is protected by the output gate while it is
/// stored, not only once revealed. Not custody material: seed phrases,
/// private keys and keystores are never released to agents and are not
/// registered with the gate (and so held for the session) unless a person
/// reveals them. Not the non-secret parts of a provider account (a Claude
/// config folder, an AWS profile name, a command account marker), which would
/// hide ordinary text.
fn protected_while_stored(label: &str, credential_type: CredentialType) -> bool {
    !matches!(
        credential_type,
        CredentialType::SeedPhrase
            | CredentialType::CryptoPrivateKey
            | CredentialType::KeystoreJson
    ) && parse_provider_account_label(label).is_none_or(|(_, _, kind)| kind.is_secret())
}

fn register_with_output_gate(
    label: &str,
    credential_type: CredentialType,
    secret: &str,
) -> Result<(), codex_secret_broker::output_gate::RegisterError> {
    use codex_secret_broker::output_gate;
    use codex_secret_broker::output_gate::SecretClass;
    let Some(gate) = output_gate::active() else {
        return Ok(());
    };
    let class = match credential_type {
        CredentialType::SeedPhrase => SecretClass::SeedPhrase,
        CredentialType::CryptoPrivateKey | CredentialType::KeystoreJson => SecretClass::PrivateKey,
        _ => SecretClass::Operational,
    };
    let label = format!("vault:{label}");
    let mut parts: Vec<Zeroizing<String>> = vec![Zeroizing::new(secret.to_string())];
    if credential_type == CredentialType::SeedPhrase {
        let words: Vec<&str> = secret.split_whitespace().collect();
        parts.push(Zeroizing::new(words.join(" ")));
        parts.push(Zeroizing::new(words.join("\n")));
        parts.extend(
            words
                .windows(SEED_WINDOW_WORDS)
                .map(|window| Zeroizing::new(window.join(" "))),
        );
    } else {
        // A hex key is printed with or without `0x`, in either case.
        let trimmed = secret.trim();
        let hex = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
            .unwrap_or(trimmed);
        if credential_type == CredentialType::CryptoPrivateKey
            && hex.len() >= 32
            && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            for form in [hex.to_ascii_lowercase(), hex.to_ascii_uppercase()] {
                parts.push(Zeroizing::new(format!("0x{form}")));
                parts.push(Zeroizing::new(form));
            }
        }
        if credential_type == CredentialType::BasicAuth
            && let Some((_, password)) = secret.split_once(':')
        {
            parts.push(Zeroizing::new(password.to_string()));
        }
        // Multi-part values (key, secret, passphrase on separate lines).
        parts.extend(
            secret
                .lines()
                .map(str::trim)
                .filter(|line| has_key_material(line) && *line != secret.trim())
                .map(|line| Zeroizing::new(line.to_string())),
        );
    }
    let entries: Vec<(&str, SecretClass, &str)> = parts
        .iter()
        .enumerate()
        .filter(|(index, part)| *index == 0 || part.trim().len() >= output_gate::MIN_VALUE_BYTES)
        .map(|(_, part)| (label.as_str(), class, part.as_str()))
        .collect();
    gate.register_all(&entries).map(|_| ())
}

/// Any run of three consecutive words of a seed phrase, so part of a phrase
/// is withheld as well as the whole.
const SEED_WINDOW_WORDS: usize = 3;
/// Shortest run of key characters for a line of a multi-line value to count
/// as key material rather than scaffolding (PEM headers, JSON fields).
const KEY_MATERIAL_RUN: usize = 16;

fn has_key_material(line: &str) -> bool {
    let mut run = 0;
    for byte in line.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=') {
            run += 1;
            if run >= KEY_MATERIAL_RUN {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// Format a Unix-seconds timestamp as an ISO-8601 string for display.
pub fn format_timestamp(seconds: i64) -> String {
    DateTime::<Utc>::from_timestamp(seconds, 0)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, /*use_z*/ true))
        .unwrap_or_else(|| format!("unix {seconds}"))
}
