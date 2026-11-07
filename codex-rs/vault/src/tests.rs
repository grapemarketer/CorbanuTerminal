use std::sync::Arc;
use std::sync::Barrier;

use chrono::Utc;
use codex_keyring_store::tests::MockKeyringStore;
use codex_security_policy::ActorChain;
use codex_security_policy::AuthorizationContext;
use codex_security_policy::AuthorizationRequest;
use codex_security_policy::BoundedText;
use codex_security_policy::PolicyAction;
use codex_security_policy::PolicyPrincipal;
use codex_security_policy::PrincipalKind;
use codex_security_policy::ProtectedResource;
use codex_security_policy::ResourceKind;
use codex_security_policy::compose_existing_decision;
use codex_security_policy::permissive_decision;
use pretty_assertions::assert_eq;

use super::*;

fn test_vault() -> (tempfile::TempDir, Arc<MockKeyringStore>, Vault) {
    let dir = tempfile::tempdir().expect("tempdir");
    let keyring = Arc::new(MockKeyringStore::default());
    let vault = Vault::new_with_keyring_store(dir.path().to_path_buf(), keyring.clone());
    (dir, keyring, vault)
}

fn api_key_entry(label: &str, secret: &str) -> AddCredential {
    AddCredential {
        label: label.to_string(),
        credential_type: CredentialType::ApiKey,
        provider: Some("ambient".to_string()),
        notes: Some("primary key".to_string()),
        revocation_notes: Some("rotate at https://console.example.com".to_string()),
        secret: secret.to_string(),
    }
}

#[test]
fn permissive_security_composition_preserves_vault_programmatic_use_decisions() {
    let human = PolicyPrincipal::new(PrincipalKind::Human, "human:jim").expect("human");
    let request = AuthorizationRequest::new(
        ActorChain::new(vec![human]).expect("actor chain"),
        ProtectedResource::new(ResourceKind::VaultCredential, "vault:representative")
            .expect("resource"),
        PolicyAction::Use,
        AuthorizationContext {
            now_unix_seconds: 1,
            session_id: BoundedText::new("session:vault-test").expect("session id"),
            task_id: BoundedText::new("task:vault-use").expect("task id"),
            purpose: BoundedText::new("permissive-compatibility").expect("purpose"),
            operation: BoundedText::new("vault.use").expect("operation"),
            destination: None,
            quantity: None,
            grant_id: None,
        },
    )
    .expect("request");
    let permissive = permissive_decision(&request).expect("Permissive decision");

    for (credential_type, expected_allow) in [
        (CredentialType::ApiKey, true),
        (CredentialType::BearerToken, true),
        (CredentialType::BasicAuth, true),
        (CredentialType::OauthClient, true),
        (CredentialType::CryptoPrivateKey, false),
        (CredentialType::SeedPhrase, false),
        (CredentialType::KeystoreJson, false),
        (CredentialType::RpcKey, true),
        (CredentialType::ExchangeKey, true),
        (CredentialType::DeploymentKey, true),
        (CredentialType::ManualSecret, true),
    ] {
        let existing_allow = credential_type.permits_programmatic_use();
        assert_eq!(existing_allow, expected_allow);
        assert_eq!(
            compose_existing_decision(existing_allow, &permissive),
            expected_allow,
            "Permissive changed the vault decision for {credential_type:?}"
        );
    }
}

#[test]
fn add_then_reveal_round_trips() {
    let (_dir, _keyring, vault) = test_vault();
    vault
        .add(api_key_entry("ambient/prod", "sk-secret-123"))
        .unwrap();

    assert!(vault.exists("ambient/prod").unwrap());
    let meta = vault.show("ambient/prod").unwrap();
    assert_eq!(meta.label, "ambient/prod");
    assert_eq!(meta.credential_type, CredentialType::ApiKey);
    assert_eq!(meta.provider.as_deref(), Some("ambient"));
    assert_eq!(meta.storage_backend, StorageBackend::EncryptedSecrets);

    // Listing must NOT include the raw secret.
    let listed = vault.list().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0], meta);
    let listed_json = serde_json::to_string(&listed).unwrap();
    assert!(!listed_json.contains("sk-secret-123"));

    // Reveal returns the raw value.
    assert_eq!(vault.reveal("ambient/prod").unwrap(), "sk-secret-123");
}

#[test]
fn add_duplicate_label_is_rejected() {
    let (_dir, _keyring, vault) = test_vault();
    vault.add(api_key_entry("dupe", "one")).unwrap();
    let err = vault
        .add(api_key_entry("dupe", "two"))
        .expect_err("duplicate label should be rejected");
    assert!(
        matches!(err, VaultError::CredentialExists { ref label } if label == "dupe"),
        "unexpected error: {err:?}"
    );
    // Original secret is preserved.
    assert_eq!(vault.reveal("dupe").unwrap(), "one");
}

#[test]
fn add_empty_secret_is_rejected() {
    let (_dir, _keyring, vault) = test_vault();
    let mut entry = api_key_entry("empty", "x");
    entry.secret = "   ".to_string();
    let err = vault
        .add(entry)
        .expect_err("empty secret should be rejected");
    assert!(
        matches!(err, VaultError::EmptySecret),
        "unexpected: {err:?}"
    );
}

#[test]
fn invalid_labels_are_rejected() {
    let (_dir, _keyring, vault) = test_vault();
    for bad in ["", "    ", "has space", "has@symbol", &"x".repeat(129)] {
        let err = vault
            .add(api_key_entry(bad, "secret"))
            .err()
            .unwrap_or_else(|| panic!("expected label {bad:?} to be rejected"));
        assert!(
            matches!(err, VaultError::InvalidLabel(_)),
            "unexpected: {err:?}"
        );
    }
}

#[test]
fn label_is_trimmed_and_case_preserved() {
    let (_dir, _keyring, vault) = test_vault();
    vault.add(api_key_entry("  MyLabel  ", "secret")).unwrap();
    assert!(vault.exists("  MyLabel  ").unwrap());
    // Lookup uses the normalized (trimmed) form.
    assert!(vault.exists("MyLabel").unwrap());
    assert_eq!(vault.reveal("MyLabel").unwrap(), "secret");
}

#[test]
fn update_changes_secret_and_metadata() {
    let (_dir, _keyring, vault) = test_vault();
    vault.add(api_key_entry("k1", "old-secret")).unwrap();
    let before = vault.show("k1").unwrap();
    let updated = vault
        .update(
            "k1",
            Some("new-secret".to_string()),
            Some(Some("openrouter".to_string())),
            /*notes*/ None,
            /*revocation_notes*/ None,
        )
        .unwrap();
    assert_eq!(vault.reveal("k1").unwrap(), "new-secret");
    assert_eq!(updated.provider.as_deref(), Some("openrouter"));
    // created_at preserved, updated_at advanced (or equal if same second).
    assert_eq!(updated.created_at, before.created_at);
    assert!(updated.updated_at >= before.updated_at);
}

#[test]
fn update_missing_label_errors() {
    let (_dir, _keyring, vault) = test_vault();
    let err = vault
        .update(
            "nope",
            Some("x".to_string()),
            /*provider*/ None,
            /*notes*/ None,
            /*revocation_notes*/ None,
        )
        .expect_err("missing label should error");
    assert!(
        matches!(err, VaultError::NotFound { ref label } if label == "nope"),
        "unexpected: {err:?}"
    );
}

#[test]
fn delete_removes_credential_and_secret() {
    let (_dir, _keyring, vault) = test_vault();
    vault.add(api_key_entry("to-remove", "secret")).unwrap();
    assert!(vault.delete("to-remove").unwrap());
    assert!(!vault.exists("to-remove").unwrap());
    // Deleting again reports false (already gone).
    assert!(!vault.delete("to-remove").unwrap());
}

#[test]
fn delete_many_removes_present_credentials_and_ignores_missing_labels() {
    let (_dir, _keyring, vault) = test_vault();
    vault.add(api_key_entry("first", "secret-1")).unwrap();

    assert_eq!(
        vault
            .delete_many(&["first".into(), "missing".into()])
            .unwrap(),
        1
    );
    assert!(!vault.exists("first").unwrap());
}

#[test]
fn reveal_missing_label_errors() {
    let (_dir, _keyring, vault) = test_vault();
    let err = vault
        .reveal("ghost")
        .expect_err("missing label should error");
    assert!(
        matches!(err, VaultError::NotFound { ref label } if label == "ghost"),
        "unexpected: {err:?}"
    );
}

fn assert_programmatic_use_round_trip(credential_type: CredentialType, label: &str) {
    let (_dir, _keyring, vault) = test_vault();
    let secret = "programmatic-use-fixture";
    vault
        .add(AddCredential {
            label: label.to_string(),
            credential_type,
            provider: None,
            notes: None,
            revocation_notes: None,
            secret: secret.to_string(),
        })
        .unwrap();

    assert_eq!(
        vault
            .reveal_for_programmatic_use(label, SecurityLevel::Permissive)
            .unwrap(),
        secret
    );
}

#[test]
fn programmatic_use_accepts_manual_secrets_under_arbitrary_labels() {
    assert_programmatic_use_round_trip(CredentialType::ManualSecret, "infrastructure/primary");
}

#[test]
fn programmatic_use_accepts_api_keys_under_arbitrary_labels() {
    assert_programmatic_use_round_trip(CredentialType::ApiKey, "external-service/key");
}

#[test]
fn programmatic_use_accepts_deployment_credentials_under_arbitrary_labels() {
    assert_programmatic_use_round_trip(CredentialType::DeploymentKey, "hosting/deploy");
}

#[test]
fn programmatic_use_denies_raw_export_outside_permissive_without_reading_the_label() {
    let (_dir, _keyring, vault) = test_vault();
    let label = "provider.openai";
    let secret = "RAW-EXPORT-CANARY";
    vault
        .add(AddCredential {
            label: label.to_string(),
            credential_type: CredentialType::ApiKey,
            provider: Some("openai".to_string()),
            notes: None,
            revocation_notes: None,
            secret: secret.to_string(),
        })
        .expect("add credential");

    for level in [SecurityLevel::Moderate, SecurityLevel::Aggressive] {
        let error = vault
            .reveal_for_programmatic_use(label, level)
            .expect_err("protected level must deny raw export");
        assert!(matches!(
            error,
            VaultError::ProgrammaticUseSecurityLevelDenied {
                level: denied_level
            } if denied_level == level
        ));
        let message = format!("{error:?} {error}");
        assert!(!message.contains(label));
        assert!(!message.contains(secret));
    }

    assert_eq!(
        vault
            .reveal_for_programmatic_use(label, SecurityLevel::Permissive)
            .expect("permissive compatibility"),
        secret
    );
}

#[test]
fn programmatic_use_rejects_key_custody_material_without_deleting_it() {
    let (_dir, _keyring, vault) = test_vault();
    let credential_types = [CredentialType::CryptoPrivateKey, CredentialType::SeedPhrase];

    for (index, credential_type) in credential_types.into_iter().enumerate() {
        let label = format!("custody-{index}");
        let secret = format!("custody-material-{index}");
        vault
            .add(AddCredential {
                label: label.clone(),
                credential_type,
                provider: None,
                notes: None,
                revocation_notes: None,
                secret: secret.clone(),
            })
            .unwrap();

        let error = vault
            .reveal_for_programmatic_use(&label, SecurityLevel::Permissive)
            .expect_err("key custody material must require explicit user access");
        assert!(matches!(
            error,
            VaultError::ProgrammaticUseDenied {
                label: denied_label,
                credential_type: denied_type,
            } if denied_label == label && denied_type == credential_type
        ));
        assert_eq!(vault.reveal(&label).unwrap(), secret);
    }
}

#[test]
fn multiple_credential_types_are_supported() {
    let (_dir, _keyring, vault) = test_vault();
    vault
        .add(AddCredential {
            label: "bearer".to_string(),
            credential_type: CredentialType::BearerToken,
            provider: None,
            notes: None,
            revocation_notes: None,
            secret: "token-abc".to_string(),
        })
        .unwrap();
    vault
        .add(AddCredential {
            label: "seed".to_string(),
            credential_type: CredentialType::SeedPhrase,
            provider: None,
            notes: None,
            revocation_notes: None,
            secret: "abandon amount bridge".to_string(),
        })
        .unwrap();
    let listed = vault.list().unwrap();
    assert_eq!(listed.len(), 2);
    // Sorted by label.
    assert_eq!(listed[0].label, "bearer");
    assert_eq!(listed[1].label, "seed");
    assert_eq!(vault.reveal("seed").unwrap(), "abandon amount bridge");
}

#[test]
fn timestamp_format_is_iso8601() {
    let now = Utc::now().timestamp();
    let formatted = format_timestamp(now);
    assert!(
        formatted.ends_with('Z'),
        "expected UTC 'Z' suffix: {formatted}"
    );
    assert!(
        formatted.contains('T'),
        "expected ISO-8601 'T' separator: {formatted}"
    );
}

#[test]
fn vault_index_is_encrypted_at_rest() {
    let (dir, _keyring, vault) = test_vault();
    vault
        .add(api_key_entry("secret-label", "plaintext-should-not-appear"))
        .unwrap();

    // The managed-secrets file must exist and must NOT contain the plaintext secret or label.
    let secrets_file = dir.path().join("secrets").join("local.age");
    assert!(
        secrets_file.exists(),
        "expected encrypted secrets file to exist"
    );
    let bytes = std::fs::read(&secrets_file).unwrap();
    let contents = String::from_utf8_lossy(&bytes);
    assert!(
        !contents.contains("plaintext-should-not-appear"),
        "raw secret leaked into the secrets file"
    );
}

#[test]
fn persistence_across_vault_instances() {
    // A vault persists into the age-encrypted secrets file. A second Vault instance
    // pointed at the same codex_home AND the same OS keyring store reads it back.
    // (The keyring holds the encryption passphrase, so it must be shared.)
    let (dir, keyring, vault) = test_vault();
    vault.add(api_key_entry("persist", "value-1")).unwrap();

    let vault2 = Vault::new_with_keyring_store(dir.path().to_path_buf(), keyring);
    assert_eq!(vault2.reveal("persist").unwrap(), "value-1");
}

#[test]
fn distinct_labels_do_not_collide_at_secret_layer() {
    // Regression: separator and case variants must map to DISTINCT secret records so adding one
    // never overwrites another's stored secret.
    let labels = ["a/b", "a.b", "a_b", "a-b", "MyLabel", "mylabel"];
    let secret_names = labels
        .into_iter()
        .map(|label| secret_name_for(label).expect("valid secret name"))
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(secret_names.len(), labels.len());

    // Exercise the original separator-collision failure end to end through encrypted storage.
    // The exhaustive uniqueness assertion above keeps the expensive age integration below the
    // repository's per-test timeout while still covering every collision class.
    let (_dir, _keyring, vault) = test_vault();
    for (label, secret) in [("a/b", "slash"), ("a.b", "dot")] {
        vault
            .add(api_key_entry(label, secret))
            .unwrap_or_else(|e| panic!("adding {label:?} failed: {e:?}"));
    }

    // Each label independently resolves to its own secret — no clobbering.
    assert_eq!(vault.reveal("a/b").unwrap(), "slash");
    assert_eq!(vault.reveal("a.b").unwrap(), "dot");

    // Both metadata entries survive.
    assert_eq!(vault.list().unwrap().len(), 2);
}

#[test]
fn concurrent_unique_adds_preserve_every_credential() {
    const WRITERS: usize = 2;
    let (_dir, _keyring, vault) = test_vault();
    let vault = Arc::new(vault);
    let barrier = Arc::new(Barrier::new(WRITERS));
    let handles = (0..WRITERS)
        .map(|index| {
            let vault = Arc::clone(&vault);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                let label = format!("concurrent/{index:02}");
                let secret = format!("secret-{index:02}");
                vault.add(api_key_entry(&label, &secret))
            })
        })
        .collect::<Vec<_>>();

    for handle in handles {
        handle.join().expect("writer thread").expect("vault add");
    }

    let listed = vault.list().expect("list concurrent credentials");
    assert_eq!(
        listed
            .into_iter()
            .map(|credential| credential.label)
            .collect::<Vec<_>>(),
        vec!["concurrent/00".to_string(), "concurrent/01".to_string()]
    );
}

#[test]
fn wrong_key_fails_closed_without_replacing_encrypted_state() {
    let (dir, _keyring, vault) = test_vault();
    vault
        .add(api_key_entry("protected", "original-secret"))
        .expect("seed vault");
    let encrypted_path = dir.path().join("secrets").join("local.age");
    let encrypted_before = std::fs::read(&encrypted_path).expect("encrypted vault");

    let wrong_key_vault = Vault::new_with_keyring_store(
        dir.path().to_path_buf(),
        Arc::new(MockKeyringStore::default()),
    );
    assert!(wrong_key_vault.list().is_err());
    assert_eq!(
        std::fs::read(&encrypted_path).expect("encrypted vault after failed read"),
        encrypted_before
    );
    assert_eq!(
        vault.reveal("protected").expect("original key still works"),
        "original-secret"
    );
}

#[test]
fn corrupt_encrypted_state_fails_closed() {
    let (dir, keyring, vault) = test_vault();
    vault
        .add(api_key_entry("protected", "original-secret"))
        .expect("seed vault");
    let encrypted_path = dir.path().join("secrets").join("local.age");
    let mut bytes = std::fs::read(&encrypted_path).expect("encrypted vault");
    let payload_offset = bytes.len() / 2;
    // XOR always changes the byte; assigning b'!' was a no-op when the
    // ciphertext byte already was b'!' (~1 in 256 runs), so the vault stayed valid.
    bytes[payload_offset] ^= 0xff;
    std::fs::write(&encrypted_path, bytes).expect("corrupt encrypted vault fixture");

    let reopened = Vault::new_with_keyring_store(dir.path().to_path_buf(), keyring);
    assert!(reopened.list().is_err());
    assert!(reopened.reveal("protected").is_err());
}

#[test]
fn copied_vault_in_another_home_cannot_use_the_original_keyring_account() {
    let (original, keyring, vault) = test_vault();
    vault
        .add(api_key_entry("protected", "synthetic-home-binding-canary"))
        .expect("seed original encrypted vault");
    let copied = tempfile::tempdir().expect("alternate home");
    let original_file = original.path().join("secrets/local.age");
    let copied_file = copied.path().join("secrets/local.age");
    let ciphertext = std::fs::read(&original_file).expect("encrypted original");
    std::fs::create_dir(copied.path().join("secrets")).expect("copied secrets directory");
    std::fs::write(&copied_file, &ciphertext).expect("copy ciphertext, not keyring material");
    assert_ne!(
        codex_secrets::compute_keyring_account(original.path()),
        codex_secrets::compute_keyring_account(copied.path())
    );
    let copied_vault = Vault::new_with_keyring_store(copied.path().to_path_buf(), keyring.clone());
    assert!(
        copied_vault
            .reveal_for_programmatic_use("protected", SecurityLevel::Permissive)
            .is_err()
    );
    assert_eq!(
        std::fs::read(&copied_file).expect("unchanged ciphertext"),
        ciphertext
    );
    let reopened = Vault::new_with_keyring_store(original.path().to_path_buf(), keyring);
    assert_eq!(
        reopened
            .reveal("protected")
            .expect("original account still works"),
        "synthetic-home-binding-canary"
    );
}

#[test]
fn canonical_home_alias_preserves_keyring_identity_and_label_case() {
    let (home, keyring, vault) = test_vault();
    vault
        .add(api_key_entry("Case", "first-value"))
        .expect("upper-case label");
    vault
        .add(api_key_entry("case", "second-value"))
        .expect("lower-case label");
    let alias = home.path().join(".");
    let reopened = Vault::new_with_keyring_store(alias.clone(), keyring);
    assert_eq!(
        codex_secrets::compute_keyring_account(&alias),
        codex_secrets::compute_keyring_account(home.path())
    );
    assert_eq!(
        reopened
            .reveal(" Case ")
            .expect("trimmed case-preserving lookup"),
        "first-value"
    );
    assert_eq!(
        reopened.reveal("case").expect("distinct label"),
        "second-value"
    );
}

#[test]
fn pf_28_s01_revealed_values_are_registered_with_the_output_gate() {
    use codex_secret_broker::output_gate;
    use codex_secret_broker::output_gate::OutputSink;
    // Process-wide gate: arming here only affects this test binary.
    output_gate::global().arm().expect("arm");
    let directory = tempfile::tempdir().expect("tempdir");
    let vault = Vault::new_with_keyring_store(
        directory.path().to_path_buf(),
        Arc::new(MockKeyringStore::default()),
    );
    let add = |label: &str, credential_type, secret: &str| {
        vault
            .add(AddCredential {
                label: label.to_string(),
                credential_type,
                provider: None,
                notes: None,
                revocation_notes: None,
                secret: secret.to_string(),
            })
            .expect("add");
    };
    add(
        "pf28-basic",
        CredentialType::BasicAuth,
        "pf28user:pf28-basic-password-9",
    );
    add(
        "pf28-seed",
        CredentialType::SeedPhrase,
        "pf28 legal winner thank year wave sausage worth useful legal winner thank yellow",
    );
    let scrub = |text: &str| output_gate::scrub_if_armed(OutputSink::ToolResult, text);
    // #434: storing a value with the gate armed protects it at once, before
    // any reveal (the person may put the same value in a file).
    assert_eq!(
        scrub("pw pf28-basic-password-9").as_deref(),
        Some("pw [REDACTED:vault:pf28-basic]")
    );
    vault
        .update(
            "pf28-basic",
            Some("pf28user:pf28-rotated-password-3".to_string()),
            /*provider*/ None,
            /*notes*/ None,
            /*revocation_notes*/ None,
        )
        .expect("update");
    assert_eq!(
        scrub("pw pf28-rotated-password-3").as_deref(),
        Some("pw [REDACTED:vault:pf28-basic]")
    );
    vault
        .reveal_for_programmatic_use("pf28-basic", SecurityLevel::Permissive)
        .expect("reveal");
    vault.reveal("pf28-seed").expect("human reveal");
    let withheld =
        scrub("pf28 legal winner thank year wave sausage worth useful legal winner thank yellow")
            .expect("withheld");
    assert!(withheld.starts_with("[WITHHELD: output contained a protected seed phrase"));
    // Part of the phrase is withheld too.
    assert!(
        scrub("first words: wave sausage worth").is_some_and(|text| text.starts_with("[WITHHELD:"))
    );

    // A hex key matches with or without `0x`, in either case.
    let key = "0xA1B2C3D4E5F60718293A4B5C6D7E8F90A1B2C3D4E5F60718293A4B5C6D7E8F90";
    add("pf28-key", CredentialType::CryptoPrivateKey, key);
    vault.reveal("pf28-key").expect("key reveal");
    let bare = key[2..].to_ascii_lowercase();
    assert!(scrub(&format!("k={bare}")).is_some_and(|text| text.starts_with("[WITHHELD:")));

    // Keystore scaffolding lines are not key material.
    add(
        "pf28-keystore",
        CredentialType::KeystoreJson,
        "{\n  \"version\": 3,\n  \"kdf\": \"scrypt\",\n  \"ciphertext\": \"pf28c1phert3xtAAAABBBBCCCCDDDD\"\n}",
    );
    vault.reveal("pf28-keystore").expect("keystore reveal");
    assert_eq!(scrub("  \"kdf\": \"scrypt\","), None);
    assert!(
        scrub("  \"ciphertext\": \"pf28c1phert3xtAAAABBBBCCCCDDDD\"")
            .is_some_and(|text| text.starts_with("[WITHHELD:"))
    );
}

/// Debug builds with `CORBANU_TEST_NO_NATIVE_KEYRING` set (every test, tmux
/// and demo run) never reach the OS keyring: the default vault keeps its key
/// in the profile's `0600` fallback file instead. Runs in a child test process
/// so this process's environment is not changed.
#[cfg(debug_assertions)]
#[test]
fn isolated_fixture_vault_keeps_its_key_in_the_profile() {
    const CHILD_ENV: &str = "CODEX_VAULT_KEYRING_ISOLATION_CHILD";
    const TEST: &str = "tests::isolated_fixture_vault_keeps_its_key_in_the_profile";
    if std::env::var_os(CHILD_ENV).is_none() {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD_ENV, "1")
            .env("CORBANU_TEST_NO_NATIVE_KEYRING", "1")
            .output()
            .expect("run child test");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let vault = Vault::new(dir.path().to_path_buf());
    vault
        .add(api_key_entry("isolated", "synthetic-isolated-value"))
        .expect("add credential without the OS keyring");
    assert_eq!(
        vault.reveal("isolated").expect("reveal"),
        "synthetic-isolated-value"
    );
    assert_eq!(vault.key_storage(), VaultKeyStorage::LocalFileFallback);
    let fallback = std::fs::read_dir(dir.path().join("secrets").join("keyring-fallback"))
        .expect("fallback dir in the profile")
        .count();
    assert_eq!(fallback, 1);
}

/// #434: with the output gate armed, a stored provider-account secret is
/// protected at once, but its non-secret parts (a Claude config folder) are
/// not, and neither is custody material until a person reveals it.
#[test]
fn issue_434_only_secrets_are_protected_while_stored() {
    use codex_secret_broker::output_gate;
    use codex_secret_broker::output_gate::OutputSink;
    output_gate::global().arm().expect("arm");
    let directory = tempfile::tempdir().expect("tempdir");
    let vault = Vault::new_with_keyring_store(
        directory.path().to_path_buf(),
        Arc::new(MockKeyringStore::default()),
    );
    let name = ProviderAccountName::parse("issue434").expect("name");
    vault
        .write_provider_account(
            "claude-plan",
            &name,
            ProviderAccountKind::ClaudeConfigDir,
            "/tmp/issue434-claude-config-folder",
        )
        .expect("config folder");
    vault
        .write_provider_account(
            "zai",
            &name,
            ProviderAccountKind::ApiKey,
            "issue434-account-key-0001",
        )
        .expect("api key");
    vault
        .add(AddCredential {
            label: "issue434-seed".to_string(),
            credential_type: CredentialType::SeedPhrase,
            provider: None,
            notes: None,
            revocation_notes: None,
            // The output gate is process-global: words a sibling test reveals
            // (`pf28-seed`) stay protected, so this phrase must not share them.
            secret:
                "i434 letter advice cage absurd amount doctor acoustic avoid letter advice cage above"
                    .to_string(),
        })
        .expect("seed");
    let scrub = |text: &str| output_gate::scrub_if_armed(OutputSink::ToolResult, text);
    assert_eq!(scrub("ls /tmp/issue434-claude-config-folder"), None);
    assert!(scrub("key issue434-account-key-0001").is_some_and(|text| !text.contains("0001")));
    assert_eq!(scrub("words: cage absurd amount doctor"), None);
    assert_eq!(
        vault.protect_stored_values().expect("read"),
        Vec::<String>::new()
    );
    assert_eq!(scrub("ls /tmp/issue434-claude-config-folder"), None);
    assert_eq!(scrub("words: cage absurd amount doctor"), None);
}
