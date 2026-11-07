//! PF-84-S06 slice B: [`AuthManager`]s bound to one named ChatGPT/OpenAI
//! sign-in account. One manager (and so one refresh lock) is shared per
//! (home, account) in a process; other processes see a refresh through the
//! guarded reload from the vault.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::Weak;

use codex_protocol::auth::AuthMode;
use tokio::sync::Semaphore;
use tokio::sync::watch;

use super::AuthManager;
use super::CachedAuth;
use super::ChatgptAuth;
use super::ChatgptAuthState;
use super::CodexAuth;
use super::OpenAiApiKeyEnv;
use super::ProviderApiKeyCache;
use super::create_default_auth_client;
use super::refresh_token_endpoint;
use super::revoke_auth_tokens;
use crate::auth::named_chatgpt::NamedChatgptAccount;
use crate::auth::named_chatgpt::VaultAccountAuthStorage;
use crate::auth::named_chatgpt::sign_in_restriction;
use crate::auth::storage::AuthDotJson;
use crate::auth::storage::AuthStorageBackend;

type Registry = HashMap<(PathBuf, NamedChatgptAccount), Weak<AuthManager>>;

/// D171 (#1286): see [`AuthManager::held_named_sign_in`].
pub(super) struct NamedAccountStore {
    vault: codex_vault::Vault,
    seen: Mutex<Seen>,
    /// Reads of the stored sign-in, and whether `seen` was free during each.
    #[cfg(test)]
    reads: Mutex<Vec<bool>>,
    /// One-shot interleaving after a Gone observation, before its cache clear.
    #[cfg(test)]
    before_clear: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

/// What the store was seen holding, and the cache installation generation
/// (an answer read before an installation is not recorded after it).
struct Seen {
    state: StoreSeen,
    epoch: u64,
}

impl NamedAccountStore {
    fn new(vault: codex_vault::Vault) -> Self {
        Self {
            vault,
            seen: Mutex::new(Seen {
                state: StoreSeen::Unknown,
                epoch: 0,
            }),
            #[cfg(test)]
            reads: Mutex::new(Vec::new()),
            #[cfg(test)]
            before_clear: Mutex::new(None),
        }
    }

    fn seen(&self) -> std::sync::MutexGuard<'_, Seen> {
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Drops what the store was last seen holding (cached auth was installed,
    /// even if it has the same credentials as before).
    pub(super) fn forget(&self) {
        let mut seen = self.seen();
        seen.state = StoreSeen::Unknown;
        seen.epoch = seen.epoch.wrapping_add(1);
    }

    /// Records what a read found, unless cached auth was installed since it
    /// started.
    fn record(&self, epoch: u64, state: StoreSeen) {
        let mut seen = self.seen();
        if seen.epoch == epoch {
            seen.state = state;
        }
    }

    /// The account's stored sign-in, read through this handle (#1311: with
    /// `seen` released, so other callers never wait on the decryption).
    fn read(&self, account: &NamedChatgptAccount) -> std::io::Result<Option<AuthDotJson>> {
        #[cfg(test)]
        {
            let seen_free = self.seen.try_lock().is_ok();
            self.reads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(seen_free);
        }
        VaultAccountAuthStorage::with_vault(self.vault.clone(), account)
            .and_then(|storage| storage.load())
    }
}

/// D171: see [`AuthManager::held_named_sign_in`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HeldSignIn {
    Current,
    Changed,
    Gone { epoch: u64 },
}

/// What the store held, at which marker, the last time it was read.
#[derive(Clone, Copy)]
enum StoreSeen {
    Unknown,
    /// The sign-in this manager holds.
    Holds(codex_vault::VaultStorageMarker),
    /// No sign-in, or another one.
    Gone(codex_vault::VaultStorageMarker),
    /// #1311: the store could not be read; the sign-in is kept and the store
    /// is read again only once it changes.
    Unreadable(codex_vault::VaultStorageMarker),
}

/// Whether `stored` is the sign-in `held` was loaded from (the same ChatGPT
/// tokens or API key). Kinds without either are not compared.
fn holds_sign_in(held: &CodexAuth, stored: &AuthDotJson) -> bool {
    if let Some(current) = held.get_current_auth_json() {
        let tokens = |auth: &AuthDotJson| {
            auth.tokens
                .as_ref()
                .map(|tokens| (tokens.refresh_token.clone(), tokens.account_id.clone()))
        };
        return tokens(&current) == tokens(stored)
            && current.openai_api_key == stored.openai_api_key;
    }
    match held.api_key() {
        Some(key) => stored.tokens.is_none() && stored.openai_api_key.as_deref() == Some(key),
        None => true,
    }
}

static NAMED_ACCOUNT_MANAGERS: LazyLock<Mutex<Registry>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

impl AuthManager {
    /// The manager of one named sign-in account of this home, created on
    /// first use with this manager's settings. It never reads the default
    /// login, `OPENAI_API_KEY`, `CODEX_API_KEY` or externally provided tokens.
    /// A ChatGPT or API-key sign-in is loaded here; other kinds load on the
    /// first [`AuthManager::auth`].
    pub fn for_named_account(self: &Arc<Self>, account: NamedChatgptAccount) -> Arc<AuthManager> {
        let base = self.default_account_manager();
        let key = (base.codex_home.clone(), account.clone());
        let mut registry = NAMED_ACCOUNT_MANAGERS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        registry.retain(|_, manager| manager.strong_count() > 0);
        if let Some(manager) = registry.get(&key).and_then(Weak::upgrade) {
            return manager;
        }
        let manager = Arc::new(base.new_named_account_manager(account, Arc::clone(&base)));
        let auth = manager.load_named_account_auth_sync();
        manager.set_cached_auth(auth);
        registry.insert(key, Arc::downgrade(&manager));
        manager
    }

    /// [`AuthManager::for_named_account`] with any sign-in kind loaded.
    pub async fn for_named_account_loaded(
        self: &Arc<Self>,
        account: NamedChatgptAccount,
    ) -> Arc<AuthManager> {
        let manager = self.for_named_account(account);
        if manager.auth_cached().is_none() {
            manager.reload().await;
        }
        manager
    }

    /// The manager of the default login: this one, or the one a named
    /// account's manager was created from.
    pub fn default_account_manager(self: &Arc<Self>) -> Arc<AuthManager> {
        self.base_manager
            .clone()
            .unwrap_or_else(|| Arc::clone(self))
    }

    /// The named account this manager serves, if any.
    pub fn named_account(&self) -> Option<&NamedChatgptAccount> {
        self.named_account.as_ref()
    }

    /// D171 (#1286): whether the sign-in this manager holds is still the
    /// account's stored one. [`HeldSignIn::Gone`] once the account was
    /// removed or signed out by any process; [`HeldSignIn::Changed`] when
    /// the store holds another sign-in for it (another process refreshed it,
    /// or it was removed and signed in again). While the store is unchanged
    /// since the last answer this is one file-metadata read; otherwise the
    /// sign-in is read through this manager's own vault handle (whose
    /// decrypted store stays cached), without holding what was last seen. A
    /// store that cannot be read keeps the sign-in and is read again once it
    /// changes (#1311). Always `Current` for the default
    /// login's manager and while nothing is held.
    pub(super) fn held_named_sign_in(&self) -> HeldSignIn {
        let (Some(account), Some(store)) = (self.named_account.as_ref(), self.named_store.as_ref())
        else {
            return HeldSignIn::Current;
        };
        // Taken before the held sign-in is read: once cached auth is
        // installed, what was seen (and this answer) no longer applies.
        let epoch = store.seen().epoch;
        let Some(held) = self.auth_cached_unchecked() else {
            return HeldSignIn::Current;
        };
        let marker = store.vault.storage_marker();
        let state = {
            let seen = store.seen();
            if seen.epoch == epoch {
                seen.state
            } else {
                StoreSeen::Unknown
            }
        };
        match (marker, state) {
            (Some(marker), StoreSeen::Holds(seen) | StoreSeen::Unreadable(seen))
                if marker == seen =>
            {
                return HeldSignIn::Current;
            }
            (Some(marker), StoreSeen::Gone(seen)) if marker == seen => {
                return HeldSignIn::Gone { epoch };
            }
            _ => {}
        }
        let Some(marker) = marker else {
            store.record(epoch, StoreSeen::Unknown);
            return HeldSignIn::Gone { epoch };
        };
        if store.vault.key_storage() == codex_vault::VaultKeyStorage::NotInitialized {
            store.record(epoch, StoreSeen::Gone(marker));
            return HeldSignIn::Gone { epoch };
        }
        let (held_sign_in, state) = match store.read(account) {
            Ok(Some(stored)) if stored.has_primary_auth_material() => {
                if holds_sign_in(&held, &stored) {
                    (HeldSignIn::Current, StoreSeen::Holds(marker))
                } else {
                    (HeldSignIn::Changed, StoreSeen::Unknown)
                }
            }
            Ok(_) => (HeldSignIn::Gone { epoch }, StoreSeen::Gone(marker)),
            Err(error) => {
                tracing::warn!(%account, %error, "named account sign-in unreadable; keeping it");
                (HeldSignIn::Current, StoreSeen::Unreadable(marker))
            }
        };
        store.record(epoch, state);
        held_sign_in
    }

    /// #1326: a `Gone` answer applies only to the held sign-in it checked.
    /// Check and clear under the cache write lock so a concurrent replacement
    /// cannot be removed between them. This keeps the setter's lock order:
    /// cache first, then `seen`, with no vault read under either lock.
    /// Returns whether the clear applied; otherwise recheck the current sign-in.
    pub(super) fn clear_named_sign_in_if_unchanged(&self, epoch: u64) -> bool {
        let Some(store) = self.named_store.as_ref() else {
            return false;
        };
        #[cfg(test)]
        {
            let before_clear = store.before_clear.lock().expect("before-clear hook").take();
            if let Some(before_clear) = before_clear {
                before_clear();
            }
        }
        if let Ok(mut cached) = self.inner.write() {
            // Release `seen` before the replacement invalidates it.
            let unchanged = store.seen().epoch == epoch;
            if unchanged {
                self.replace_cached_auth(&mut cached, /*new_auth*/ None);
                return true;
            }
        }
        false
    }

    /// Loads the named account's current sign-in, built without awaiting
    /// (see [`AuthManager::held_named_sign_in`]).
    pub(super) fn reload_named_sign_in_sync(&self) {
        let auth = self.load_named_account_auth_sync();
        self.set_cached_auth(auth);
    }

    /// PF-84-S06 (#571): why this named account's manager has no usable
    /// sign-in: none is stored, it cannot be read, or the login restrictions
    /// refuse it. `None` for the default login's manager or when a sign-in
    /// is available. Requests on the account are refused with this instead
    /// of being sent without credentials.
    pub fn named_sign_in_unavailable(&self) -> Option<String> {
        let account = self.named_account.as_ref()?;
        match self.named_account_auth_dot_json() {
            Ok(Some(_)) => None,
            // D171 (#1286): also a session whose account was removed or
            // signed out while it ran.
            Ok(None) => Some(format!(
                "account `{name}` has no sign-in (it may have been removed or signed out); \
                 sign in with `corbanu login --account {name}` (add `--with-api-key` for an API key), \
                 or pick another account (`/providers` in the TUI, `--account` on the \
                 command line)",
                name = account.name
            )),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                Some(error.to_string())
            }
            Err(error) => Some(format!(
                "the sign-in of account `{}` cannot be read: {error}",
                account.name
            )),
        }
    }

    fn new_named_account_manager(
        &self,
        account: NamedChatgptAccount,
        base: Arc<AuthManager>,
    ) -> AuthManager {
        let (auth_change_tx, _auth_change_rx) = watch::channel(0);
        AuthManager {
            codex_home: self.codex_home.clone(),
            inner: RwLock::new(CachedAuth {
                auth: None,
                permanent_refresh_failure: None,
            }),
            auth_change_tx,
            enable_codex_api_key_env: false,
            openai_api_key_env: OpenAiApiKeyEnv::Ignore,
            auth_credentials_store_mode: self.auth_credentials_store_mode,
            keyring_backend_kind: self.keyring_backend_kind,
            forced_chatgpt_workspace_id: RwLock::new(self.forced_chatgpt_workspace_id()),
            chatgpt_base_url: self.chatgpt_base_url.clone(),
            agent_identity_authapi_base_url: self.agent_identity_authapi_base_url.clone(),
            refresh_lock: Semaphore::new(/*permits*/ 1),
            agent_identity_lock: Semaphore::new(/*permits*/ 1),
            agent_identity_bootstrap_cooldown: Default::default(),
            external_auth: RwLock::new(None),
            provider_api_key_cache: RwLock::new(ProviderApiKeyCache::new()),
            auth_route_config: self.auth_route_config.clone(),
            named_store: Some(NamedAccountStore::new(codex_vault::Vault::new(
                self.codex_home.clone(),
            ))),
            named_account: Some(account),
            forced_login_method: self.forced_login_method,
            base_manager: Some(base),
            hide_restricted_default_login: false,
            hidden_default_login_reason: RwLock::new(None),
        }
    }

    fn named_account_storage(&self) -> std::io::Result<Option<Arc<dyn AuthStorageBackend>>> {
        let Some(account) = self.named_account.as_ref() else {
            return Ok(None);
        };
        Ok(Some(Arc::new(VaultAccountAuthStorage::new(
            &self.codex_home,
            account,
        )?)))
    }

    /// The stored sign-in, when it is usable under this configuration's
    /// login restrictions (a refused one is logged and treated as absent).
    fn named_account_auth_dot_json(
        &self,
    ) -> std::io::Result<Option<(Arc<dyn AuthStorageBackend>, AuthDotJson)>> {
        let Some(storage) = self.named_account_storage()? else {
            return Ok(None);
        };
        let Some(auth_dot_json) = storage
            .load()?
            .filter(AuthDotJson::has_primary_auth_material)
        else {
            return Ok(None);
        };
        let forced_workspace = self.forced_chatgpt_workspace_id();
        if let Some(account) = self.named_account.as_ref()
            && let Some(reason) = sign_in_restriction(
                account,
                &auth_dot_json,
                self.forced_login_method,
                forced_workspace.as_deref(),
            )
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                reason,
            ));
        }
        Ok(Some((storage, auth_dot_json)))
    }

    /// A ChatGPT or API-key sign-in, built without awaiting so a manager
    /// handed to synchronous readers is loaded from the start.
    fn load_named_account_auth_sync(&self) -> Option<CodexAuth> {
        let account = self.named_account.as_ref()?;
        let result = (|| -> std::io::Result<Option<CodexAuth>> {
            let Some((storage, auth_dot_json)) = self.named_account_auth_dot_json()? else {
                return Ok(None);
            };
            match auth_dot_json.resolved_mode() {
                AuthMode::ApiKey => Ok(auth_dot_json
                    .openai_api_key
                    .as_deref()
                    .map(CodexAuth::from_api_key)),
                AuthMode::Chatgpt => {
                    let client = create_default_auth_client(
                        &refresh_token_endpoint(),
                        &self.auth_route_config,
                    )?;
                    Ok(Some(CodexAuth::Chatgpt(ChatgptAuth {
                        state: ChatgptAuthState {
                            auth_dot_json: Arc::new(std::sync::Mutex::new(Some(auth_dot_json))),
                            client,
                        },
                        storage,
                    })))
                }
                // Other kinds load on the first `auth()`.
                _ => Ok(None),
            }
        })();
        result.unwrap_or_else(|error| {
            tracing::warn!(%account, %error, "named account sign-in unavailable");
            None
        })
    }

    /// The named account's sign-in from its vault store only. `None` when the
    /// account has none, it is refused by a login restriction, or this is not
    /// a named-account manager.
    pub(super) async fn load_named_account_auth(&self) -> Option<CodexAuth> {
        let account = self.named_account.as_ref()?;
        match self.try_load_named_account_auth().await {
            Ok(auth) => auth,
            Err(error) => {
                tracing::warn!(%account, %error, "named account sign-in unavailable");
                None
            }
        }
    }

    async fn try_load_named_account_auth(&self) -> std::io::Result<Option<CodexAuth>> {
        let Some((storage, auth_dot_json)) = self.named_account_auth_dot_json()? else {
            return Ok(None);
        };
        let mode = auth_dot_json.storage_mode(self.auth_credentials_store_mode);
        CodexAuth::from_auth_dot_json_with_storage(
            &self.codex_home,
            auth_dot_json,
            mode,
            self.chatgpt_base_url.as_deref(),
            self.keyring_backend_kind,
            self.agent_identity_authapi_base_url.as_deref(),
            &self.auth_route_config,
            Some(storage),
        )
        .await
        .map(Some)
    }

    /// Logs out of the named account only: revokes the tokens stored now
    /// (best effort) and deletes its vault store, also when the stored
    /// sign-in cannot be read. `None` when this is not a named manager.
    pub(super) async fn logout_named_account(&self, revoke: bool) -> Option<std::io::Result<bool>> {
        let storage = match self.named_account_storage() {
            Ok(storage) => storage?,
            Err(error) => return Some(Err(error)),
        };
        if revoke
            && let Ok(Some(auth_dot_json)) = storage.load()
            && let Err(error) =
                revoke_auth_tokens(Some(&auth_dot_json), &self.auth_route_config).await
        {
            tracing::warn!("failed to revoke named account tokens during logout: {error}");
        }
        let removed = storage.delete();
        self.reload().await;
        Some(removed)
    }
}

#[cfg(test)]
#[path = "manager_named_account_tests.rs"]
mod tests;
