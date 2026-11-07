//! Runs under the isolated test runner (`CORBANU_TEST_NO_NATIVE_KEYRING=1`).

use std::sync::Arc;

use codex_protocol::config_types::ForcedLoginMethod;
use pretty_assertions::assert_eq;

use super::*;
use crate::auth::named_chatgpt::login_named_account_with_api_key;

fn account(name: &str) -> NamedChatgptAccount {
    NamedChatgptAccount {
        provider_id: "openai".to_string(),
        name: name.to_string(),
    }
}

/// The manager itself refuses a sign-in the login restrictions rule out, and
/// a named manager hands default-account providers back to its base.
#[tokio::test]
#[serial_test::serial(codex_auth_env)]
async fn restricted_sign_ins_never_load_and_the_base_stays_the_default() {
    let home = tempfile::tempdir().expect("home");
    login_named_account_with_api_key(home.path(), &account("work"), "sk-canary-restricted")
        .expect("sign-in");
    let mut base = AuthManager::new(
        home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        crate::AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        crate::AuthKeyringBackendKind::default(),
        crate::test_support::transport_default_auth_route_config(),
    )
    .await;
    base.forced_login_method = Some(ForcedLoginMethod::Chatgpt);
    let base = Arc::new(base);
    let work = base.for_named_account_loaded(account("work")).await;
    assert_eq!(
        (
            work.auth().await.is_none(),
            Arc::ptr_eq(&work.default_account_manager(), &base),
        ),
        (true, true)
    );
}

/// An unreadable stored sign-in can still be signed out (no revoke).
#[tokio::test]
async fn an_unreadable_sign_in_is_still_removed() {
    let home = tempfile::tempdir().expect("home");
    let name = codex_vault::ProviderAccountName::parse("work").expect("name");
    codex_vault::Vault::new(home.path().to_path_buf())
        .write_provider_account(
            "openai",
            &name,
            codex_vault::ProviderAccountKind::ChatgptAuth,
            "not json",
        )
        .expect("write");
    let base = AuthManager::from_auth_for_testing_with_home(
        CodexAuth::from_api_key("sk-default"),
        home.path().to_path_buf(),
    );
    let work = base.for_named_account(account("work"));
    assert_eq!(work.logout_with_revoke().await.expect("logout"), true);
    assert!(
        codex_vault::Vault::new(home.path().to_path_buf())
            .provider_account_kinds("openai", &name)
            .expect("kinds")
            .is_empty()
    );
}

fn chatgpt_sign_in(access_token: &str) -> crate::auth::storage::AuthDotJson {
    chatgpt_sign_in_of(access_token, "chatgpt-work")
}

fn chatgpt_sign_in_of(access_token: &str, user: &str) -> crate::auth::storage::AuthDotJson {
    let claims = serde_json::json!({
        "email": "work@example.com",
        "https://api.openai.com/auth": {
            "chatgpt_user_id": user,
            "chatgpt_account_id": "ws-work",
            "chatgpt_plan_type": "plus",
        },
    });
    let b64 = |data: &[u8]| {
        use base64::Engine;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
    };
    let jwt = format!(
        "{}.{}.{}",
        b64(b"{}"),
        b64(claims.to_string().as_bytes()),
        b64(b"sig")
    );
    crate::auth::storage::AuthDotJson {
        auth_mode: Some(AuthMode::Chatgpt),
        openai_api_key: None,
        // Not a JWT and no last refresh: no refresh is attempted.
        tokens: Some(crate::token_data::TokenData {
            id_token: crate::token_data::parse_chatgpt_jwt_claims(&jwt).expect("claims"),
            access_token: access_token.to_string(),
            refresh_token: "refresh-canary".to_string(),
            account_id: Some("ws-work".to_string()),
        }),
        last_refresh: None,
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
    }
}

fn access_token(auth: Option<CodexAuth>) -> Option<String> {
    auth.and_then(|auth| auth.get_current_auth_json())
        .and_then(|auth| auth.tokens)
        .map(|tokens| tokens.access_token)
}

/// D171 (#1286): a ChatGPT sign-in held in memory is dropped once another
/// process removes the account: the next `auth()` has none (so the request is
/// refused with the recovery), it never falls back to the default login, and
/// signing the account in again is picked up.
#[tokio::test]
async fn a_removed_accounts_sign_in_is_dropped_from_memory() {
    let home = tempfile::tempdir().expect("home");
    crate::auth::named_chatgpt::save_named_account_auth(
        home.path(),
        &account("work"),
        &chatgpt_sign_in("access-before"),
        /*must_be_new*/ true,
    )
    .expect("sign-in");
    let base = AuthManager::from_auth_for_testing_with_home(
        CodexAuth::from_api_key("sk-default-canary"),
        home.path().to_path_buf(),
    );
    let work = base.for_named_account(account("work"));
    assert_eq!(
        access_token(work.auth().await),
        Some("access-before".to_string())
    );

    // What `corbanu account remove openai work` does in another process (its
    // own vault handle; this process's managers are not told).
    let name = codex_vault::ProviderAccountName::parse("work").expect("name");
    assert!(
        codex_vault::Vault::new(home.path().to_path_buf())
            .remove_provider_account("openai", &name)
            .expect("remove")
    );

    // Synchronous readers (connector headers, MCP refresh) see it too.
    assert!(work.auth_cached().is_none());
    assert!(work.auth().await.is_none());
    let reason = work.named_sign_in_unavailable().expect("a reason");
    assert!(
        reason.contains("account `work` has no sign-in (it may have been removed or signed out)")
            && reason.contains("corbanu login --account work")
            && reason.contains("`/providers`"),
        "{reason}"
    );
    assert!(!reason.contains("sk-default-canary"), "{reason}");
    // The default login is untouched and never lent to the account.
    assert_eq!(
        base.auth()
            .await
            .and_then(|auth| auth.api_key().map(str::to_string)),
        Some("sk-default-canary".to_string())
    );

    crate::auth::named_chatgpt::save_named_account_auth(
        home.path(),
        &account("work"),
        &chatgpt_sign_in("access-after"),
        /*must_be_new*/ true,
    )
    .expect("sign in again");
    assert_eq!(
        access_token(work.auth().await),
        Some("access-after".to_string())
    );
}

/// D171 (#1286): an account removed and signed in again (here with other
/// tokens) by another process before the session's next request is not
/// served from the old sign-in: synchronous readers never see it again and
/// `auth()` returns the new one.
#[tokio::test]
async fn a_replaced_accounts_old_sign_in_is_not_used() {
    let home = tempfile::tempdir().expect("home");
    let save = |access: &str| {
        crate::auth::named_chatgpt::save_named_account_auth(
            home.path(),
            &account("work"),
            &chatgpt_sign_in(access),
            /*must_be_new*/ true,
        )
        .expect("sign-in");
    };
    save("access-old");
    let base = AuthManager::from_auth_for_testing_with_home(
        CodexAuth::from_api_key("sk-default-canary"),
        home.path().to_path_buf(),
    );
    let work = base.for_named_account(account("work"));
    assert_eq!(
        access_token(work.auth().await),
        Some("access-old".to_string())
    );

    let name = codex_vault::ProviderAccountName::parse("work").expect("name");
    assert!(
        codex_vault::Vault::new(home.path().to_path_buf())
            .remove_provider_account("openai", &name)
            .expect("remove")
    );
    let mut replacement = chatgpt_sign_in("access-new");
    if let Some(tokens) = replacement.tokens.as_mut() {
        tokens.refresh_token = "refresh-new".to_string();
    }
    crate::auth::named_chatgpt::save_named_account_auth(
        home.path(),
        &account("work"),
        &replacement,
        /*must_be_new*/ true,
    )
    .expect("sign in again");

    assert_ne!(
        access_token(work.auth_cached()),
        Some("access-old".to_string())
    );
    assert_eq!(
        access_token(work.auth().await),
        Some("access-new".to_string())
    );
    // Unchanged store: the same sign-in stays.
    assert_eq!(
        access_token(work.auth_cached()),
        Some("access-new".to_string())
    );
}

fn vault(home: &std::path::Path) -> codex_vault::Vault {
    codex_vault::Vault::new(home.to_path_buf())
}

fn work_name() -> codex_vault::ProviderAccountName {
    codex_vault::ProviderAccountName::parse("work").expect("name")
}

fn stored_sign_in(home: &std::path::Path) -> Option<AuthDotJson> {
    VaultAccountAuthStorage::new(home, &account("work"))
        .expect("storage")
        .load()
        .expect("load")
}

/// What `corbanu account remove openai work` does in another process: its
/// own vault handle, on its own thread.
fn remove_in_another_process(home: &std::path::Path) -> std::thread::JoinHandle<bool> {
    let home = home.to_path_buf();
    std::thread::spawn(move || {
        vault(&home)
            .remove_provider_account("openai", &work_name())
            .expect("remove")
    })
}

/// The account's store, with another process removing the account right
/// after this one read the sign-in it is about to save.
#[derive(Debug)]
struct RemovedAfterRead {
    inner: VaultAccountAuthStorage,
    home: std::path::PathBuf,
}

impl AuthStorageBackend for RemovedAfterRead {
    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        let auth = self.inner.load()?;
        assert!(
            remove_in_another_process(&self.home)
                .join()
                .expect("remover")
        );
        Ok(auth)
    }

    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        self.inner.save(auth)
    }

    fn delete(&self) -> std::io::Result<bool> {
        self.inner.delete()
    }

    fn update(
        &self,
        change: &mut dyn FnMut(&mut AuthDotJson) -> std::io::Result<()>,
    ) -> std::io::Result<Option<AuthDotJson>> {
        let mut remover = None;
        let updated = self.inner.update(&mut |auth| {
            // The other process starts once this one has read the sign-in.
            remover = Some(remove_in_another_process(&self.home));
            change(auth)
        });
        if let Some(remover) = remover {
            assert!(remover.join().expect("remover"));
        }
        updated
    }

    fn is_named_account(&self) -> bool {
        self.inner.is_named_account()
    }
}

/// #1311 (D171): a refresh whose save races a removal by another process
/// never brings the account back: the sign-in is read and saved under one
/// vault lock, so the removal lands after the save, or the save finds no
/// account and writes nothing.
#[test]
fn a_refresh_save_racing_a_removal_never_brings_the_account_back() {
    let home = tempfile::tempdir().expect("home");
    crate::auth::named_chatgpt::save_named_account_auth(
        home.path(),
        &account("work"),
        &chatgpt_sign_in("access-before"),
        /*must_be_new*/ true,
    )
    .expect("sign-in");
    let storage: Arc<dyn AuthStorageBackend> = Arc::new(RemovedAfterRead {
        inner: VaultAccountAuthStorage::new(home.path(), &account("work")).expect("storage"),
        home: home.path().to_path_buf(),
    });

    let _ = super::super::persist_tokens(
        &storage,
        "refresh-canary",
        /*id_token*/ None,
        Some("access-refreshed".to_string()),
        Some("refresh-refreshed".to_string()),
    );

    assert_eq!(stored_sign_in(home.path()), None);
}

/// #1311 (D171): refreshed tokens are saved only onto the sign-in they were
/// refreshed from: one removed and signed in again meanwhile is kept, and an
/// account removed meanwhile is not recreated.
#[test]
fn refreshed_tokens_never_replace_a_newer_sign_in_or_recreate_the_account() {
    let home = tempfile::tempdir().expect("home");
    let mut replacement = chatgpt_sign_in("access-new");
    if let Some(tokens) = replacement.tokens.as_mut() {
        tokens.refresh_token = "refresh-new".to_string();
    }
    crate::auth::named_chatgpt::save_named_account_auth(
        home.path(),
        &account("work"),
        &replacement,
        /*must_be_new*/ true,
    )
    .expect("sign-in");
    let storage: Arc<dyn AuthStorageBackend> =
        Arc::new(VaultAccountAuthStorage::new(home.path(), &account("work")).expect("storage"));
    let persist = || {
        super::super::persist_tokens(
            &storage,
            "refresh-canary",
            /*id_token*/ None,
            Some("access-refreshed".to_string()),
            Some("refresh-refreshed".to_string()),
        )
    };

    assert!(persist().is_err());
    assert_eq!(stored_sign_in(home.path()), Some(replacement));

    assert!(
        vault(home.path())
            .remove_provider_account("openai", &work_name())
            .expect("remove")
    );
    assert!(persist().is_err());
    assert_eq!(stored_sign_in(home.path()), None);
}

fn agent_identity_record(account_id: &str) -> crate::auth::storage::AgentIdentityAuthRecord {
    crate::auth::storage::AgentIdentityAuthRecord {
        agent_runtime_id: "runtime-canary".to_string(),
        agent_private_key: "private-key-canary".to_string(),
        account_id: account_id.to_string(),
        chatgpt_user_id: "chatgpt-work".to_string(),
        email: None,
        plan_type: codex_protocol::account::PlanType::Plus,
        chatgpt_account_is_fedramp: false,
        task_id: None,
    }
}

/// #1311 (D171): saving an agent identity for a named account another
/// process removed meanwhile is refused; the sign-in held in memory is never
/// written back. One for another ChatGPT account's or user's sign-in, saved
/// under the name meanwhile, is refused too.
#[test]
fn an_agent_identity_save_never_brings_a_removed_account_back() {
    let home = tempfile::tempdir().expect("home");
    crate::auth::named_chatgpt::save_named_account_auth(
        home.path(),
        &account("work"),
        &chatgpt_sign_in("access-held"),
        /*must_be_new*/ true,
    )
    .expect("sign-in");
    let held = Arc::new(std::sync::Mutex::new(Some(chatgpt_sign_in("access-held"))));
    let storage: Arc<dyn AuthStorageBackend> =
        Arc::new(VaultAccountAuthStorage::new(home.path(), &account("work")).expect("storage"));
    assert!(
        remove_in_another_process(home.path())
            .join()
            .expect("remover")
    );

    let saved = super::super::persist_agent_identity_record(
        &held,
        &storage,
        agent_identity_record("ws-work"),
    );

    assert!(saved.is_err());
    assert_eq!(stored_sign_in(home.path()), None);

    let mut other = chatgpt_sign_in("access-other");
    if let Some(tokens) = other.tokens.as_mut() {
        tokens.account_id = Some("ws-other".to_string());
    }
    crate::auth::named_chatgpt::save_named_account_auth(
        home.path(),
        &account("work"),
        &other,
        /*must_be_new*/ true,
    )
    .expect("another sign-in");
    let saved = super::super::persist_agent_identity_record(
        &held,
        &storage,
        agent_identity_record("ws-work"),
    );
    assert!(saved.is_err());
    assert_eq!(stored_sign_in(home.path()), Some(other));

    // Another user of the same workspace: the removed account's key is not
    // written onto that sign-in either.
    let colleague = chatgpt_sign_in_of("access-colleague", "chatgpt-colleague");
    assert!(
        vault(home.path())
            .remove_provider_account("openai", &work_name())
            .expect("remove")
    );
    crate::auth::named_chatgpt::save_named_account_auth(
        home.path(),
        &account("work"),
        &colleague,
        /*must_be_new*/ true,
    )
    .expect("a colleague's sign-in");
    let saved = super::super::persist_agent_identity_record(
        &held,
        &storage,
        agent_identity_record("ws-work"),
    );
    assert!(saved.is_err());
    assert_eq!(stored_sign_in(home.path()), Some(colleague));

    // The account's own sign-in takes it.
    assert!(
        vault(home.path())
            .remove_provider_account("openai", &work_name())
            .expect("remove")
    );
    crate::auth::named_chatgpt::save_named_account_auth(
        home.path(),
        &account("work"),
        &chatgpt_sign_in("access-held"),
        /*must_be_new*/ true,
    )
    .expect("sign in again");
    super::super::persist_agent_identity_record(&held, &storage, agent_identity_record("ws-work"))
        .expect("saved");
    assert!(
        stored_sign_in(home.path())
            .and_then(|auth| auth.agent_identity)
            .is_some()
    );
}

/// #1311: the stored sign-in is read with `seen` released (other callers
/// never wait on the decryption), and one that cannot be read is read again
/// only once the store changes, not on every call.
#[test]
fn an_unreadable_store_is_read_once_per_change_without_holding_seen() {
    let home = tempfile::tempdir().expect("home");
    crate::auth::named_chatgpt::save_named_account_auth(
        home.path(),
        &account("work"),
        &chatgpt_sign_in("access-held"),
        /*must_be_new*/ true,
    )
    .expect("sign-in");
    let base = AuthManager::from_auth_for_testing_with_home(
        CodexAuth::from_api_key("sk-default-canary"),
        home.path().to_path_buf(),
    );
    let work = base.for_named_account(account("work"));
    let reads = || {
        work.named_store
            .as_ref()
            .expect("named store")
            .reads
            .lock()
            .expect("reads")
            .clone()
    };
    assert!(work.auth_cached().is_some());
    assert!(work.auth_cached().is_some());
    assert_eq!(reads(), vec![true]);

    vault(home.path())
        .write_provider_account(
            "openai",
            &work_name(),
            codex_vault::ProviderAccountKind::ChatgptAuth,
            "not json",
        )
        .expect("unreadable");
    for _ in 0..3 {
        assert_eq!(
            access_token(work.auth_cached()),
            Some("access-held".to_string())
        );
    }
    assert_eq!(reads(), vec![true, true]);

    assert!(
        vault(home.path())
            .remove_provider_account("openai", &work_name())
            .expect("remove")
    );
    assert!(work.auth_cached().is_none());
    assert_eq!(reads(), vec![true, true, true]);
}

/// #1311: an answer read before the held sign-in changed is not recorded
/// after the change.
#[test]
fn a_read_from_before_the_held_sign_in_changed_is_not_recorded() {
    let home = tempfile::tempdir().expect("home");
    login_named_account_with_api_key(home.path(), &account("work"), "sk-canary-epoch")
        .expect("sign-in");
    let store = NamedAccountStore::new(vault(home.path()));
    let marker = store.vault.storage_marker().expect("marker");
    let epoch = store.seen().epoch;
    store.forget();
    store.record(epoch, StoreSeen::Holds(marker));
    assert!(matches!(store.seen().state, StoreSeen::Unknown));
    let epoch = store.seen().epoch;
    store.record(epoch, StoreSeen::Holds(marker));
    assert!(matches!(store.seen().state, StoreSeen::Holds(_)));
}

/// #1326: pause at the real check/clear boundary, install a later sign-in,
/// then apply the old answer. Reinstalling A, replacing A with B, and an
/// A -> B -> A replacement all keep the new cache without a spurious event.
#[test]
fn a_stale_gone_answer_never_clears_a_replacement_sign_in() {
    for replacements in [
        &["sk-canary-original"][..],
        &["sk-canary-replacement"][..],
        &["sk-canary-intermediate", "sk-canary-original"][..],
    ] {
        let home = tempfile::tempdir().expect("home");
        login_named_account_with_api_key(home.path(), &account("work"), "sk-canary-original")
            .expect("sign-in");
        let base = AuthManager::from_auth_for_testing_with_home(
            CodexAuth::from_api_key("sk-default-canary"),
            home.path().to_path_buf(),
        );
        let work = base.for_named_account(account("work"));
        assert!(
            vault(home.path())
                .remove_provider_account("openai", &work_name())
                .expect("remove")
        );
        let HeldSignIn::Gone { epoch } = work.held_named_sign_in() else {
            panic!("the removed sign-in must be gone");
        };

        // These are the updates another thread can complete after the
        // check in auth_cached and before its conditional cache clear.
        for replacement in replacements {
            login_named_account_with_api_key(home.path(), &account("work"), replacement)
                .expect("replacement sign-in");
            work.reload_named_sign_in_sync();
        }
        let expected = Some(replacements.last().expect("replacement").to_string());
        let revision = *work.auth_change_receiver().borrow();

        work.clear_named_sign_in_if_unchanged(epoch);

        assert_eq!(
            (
                work.auth_cached()
                    .and_then(|auth| auth.api_key().map(str::to_string)),
                *work.auth_change_receiver().borrow()
            ),
            (expected, revision)
        );
    }
}

/// The current Gone answer still clears the removed sign-in and notifies
/// subscribers; the generation guard must not suppress real removals.
#[test]
fn a_current_gone_answer_clears_and_notifies_once() {
    let home = tempfile::tempdir().expect("home");
    login_named_account_with_api_key(home.path(), &account("work"), "sk-canary-removed")
        .expect("sign-in");
    let base = AuthManager::from_auth_for_testing_with_home(
        CodexAuth::from_api_key("sk-default-canary"),
        home.path().to_path_buf(),
    );
    let work = base.for_named_account(account("work"));
    assert!(
        vault(home.path())
            .remove_provider_account("openai", &work_name())
            .expect("remove")
    );
    let revision = *work.auth_change_receiver().borrow();

    assert_eq!(
        (work.auth_cached(), *work.auth_change_receiver().borrow()),
        (None, revision + 1)
    );
    assert_eq!(
        (work.auth_cached(), *work.auth_change_receiver().borrow()),
        (None, revision + 1)
    );
}

/// A replacement installed after the Gone check must be checked in this same
/// call, even when reinstalling identical credentials did not notify subscribers.
#[test]
fn auth_cached_rechecks_a_skipped_clear_before_returning() {
    for remove_replacement in [true, false] {
        let home = tempfile::tempdir().expect("home");
        login_named_account_with_api_key(home.path(), &account("work"), "sk-canary-reinstalled")
            .expect("sign-in");
        let base = AuthManager::from_auth_for_testing_with_home(
            CodexAuth::from_api_key("sk-default-canary"),
            home.path().to_path_buf(),
        );
        let work = base.for_named_account(account("work"));
        assert!(
            vault(home.path())
                .remove_provider_account("openai", &work_name())
                .expect("remove")
        );
        let revision = *work.auth_change_receiver().borrow();
        let store = work.named_store.as_ref().expect("named store");
        let weak_work = Arc::downgrade(&work);
        let fixture_home = home.path().to_path_buf();
        *store.before_clear.lock().expect("before-clear hook") = Some(Box::new(move || {
            let work = weak_work.upgrade().expect("live manager");
            login_named_account_with_api_key(
                &fixture_home,
                &account("work"),
                "sk-canary-reinstalled",
            )
            .expect("replacement sign-in");
            work.reload_named_sign_in_sync();
            assert_eq!(*work.auth_change_receiver().borrow(), revision);
            if remove_replacement {
                assert!(
                    vault(&fixture_home)
                        .remove_provider_account("openai", &work_name())
                        .expect("remove replacement")
                );
            }
        }));

        // Retain the first result: a second call would hide the stale return.
        let auth = work
            .auth_cached()
            .and_then(|auth| auth.api_key().map(str::to_string));
        let expected = if remove_replacement {
            (None, revision + 1)
        } else {
            (Some("sk-canary-reinstalled".to_string()), revision)
        };
        assert_eq!((auth, *work.auth_change_receiver().borrow()), expected);
        assert!(
            store
                .before_clear
                .lock()
                .expect("before-clear hook")
                .is_none()
        );
    }
}
