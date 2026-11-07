//! #609 review-round-2: a shared provider entry must name an account whose
//! enrollment supplies its status, without changing either connection's access.
//! Source-aware fixtures frozen before the production attribution change.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use super::provider_item;
use crate::legacy_core::config::Config;
use crate::legacy_core::config::ConfigBuilder;
use crate::provider_named_accounts::AccountsView;
use crate::provider_named_accounts::NamedAccountRow;
use crate::provider_status_host::ProviderAccountMetadata;
use crate::provider_status_host::ProviderStatusHost;
use codex_login::ProviderAccountKind;
use codex_model_provider_info::NamedProviderAccount;
use codex_provider_auth::EnvironmentCredentialMetadata;
use codex_provider_auth::ProviderConfigurationState;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tempfile::tempdir;

async fn connection_config(accounts: [Option<&str>; 2]) -> (TempDir, Config) {
    let home = tempdir().unwrap();
    let mut config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await
        .unwrap();
    config.cli_auth_credentials_store_mode = codex_login::AuthCredentialsStoreMode::File;
    config.model_providers =
        codex_model_provider_info::built_in_model_providers(/*openai_base_url*/ None);
    for (id, name) in ["zai", "zai-anthropic"].into_iter().zip(accounts) {
        config.model_providers.get_mut(id).unwrap().account =
            name.map(|name| NamedProviderAccount {
                provider_id: id.to_string(),
                name: name.to_string(),
            });
    }
    // Keep this unrelated session off the entry whose row is inspected.
    config.model_provider_id = "openai".into();
    config.model_provider = config.model_providers["openai"].clone();
    (home, config)
}

fn status_host(config: &Config, enrolled: &[(&str, &str)]) -> ProviderStatusHost {
    ProviderStatusHost::from_config_with_account_registry(
        config,
        ProviderAccountMetadata::default(),
        |account, kind| {
            kind == ProviderAccountKind::ApiKey
                && enrolled.contains(&(account.provider_id.as_str(), account.name.as_str()))
        },
    )
    .with_environment(|_| EnvironmentCredentialMetadata::Missing)
}

fn expected_name(name: &str) -> BTreeMap<String, String> {
    BTreeMap::from([("zai".to_string(), name.to_string())])
}

#[tokio::test]
async fn shared_entry_names_the_enrolled_connection_not_the_first_account() {
    for (accounts, enrolled_id, enrolled_name, missing_id) in [
        (
            [Some("work"), Some("spare")],
            "zai-anthropic",
            "spare",
            "zai",
        ),
        (
            [Some("spare"), Some("work")],
            "zai",
            "spare",
            "zai-anthropic",
        ),
        (
            [Some("first"), Some("second")],
            "zai",
            "first",
            "zai-anthropic",
        ),
        (
            [Some("first"), Some("second")],
            "zai-anthropic",
            "second",
            "zai",
        ),
    ] {
        let (_home, config) = connection_config(accounts).await;
        let host = status_host(&config, &[(enrolled_id, enrolled_name)]);
        assert_eq!(
            host.resolve_provider("zai").unwrap().configuration,
            ProviderConfigurationState::Configured
        );
        assert_eq!(
            host.connections_without_credential(),
            BTreeSet::from([missing_id.to_string()])
        );
        assert_eq!(host.status_accounts(), expected_name(enrolled_name));
    }
}

#[tokio::test]
async fn two_enrolled_accounts_keep_the_existing_first_account_preference() {
    let (_home, config) = connection_config([Some("work"), Some("spare")]).await;
    let host = status_host(&config, &[("zai", "work"), ("zai-anthropic", "spare")]);
    assert_eq!(host.status_accounts(), expected_name("work"));
    assert_eq!(host.connections_without_credential(), BTreeSet::new());
    assert_eq!(
        host.resolve_provider("zai").unwrap().configuration,
        ProviderConfigurationState::Configured
    );
}

#[tokio::test]
async fn no_enrolled_connection_has_no_named_status_and_stays_unconfigured() {
    let (_home, config) = connection_config([Some("work"), Some("spare")]).await;
    let host = status_host(&config, &[]).with_environment(|name| {
        if name == "ZAI_API_KEY" {
            EnvironmentCredentialMetadata::Present
        } else {
            EnvironmentCredentialMetadata::Missing
        }
    });
    assert_eq!(host.status_accounts(), BTreeMap::new());
    assert_eq!(
        host.connections_without_credential(),
        BTreeSet::from(["zai".to_string(), "zai-anthropic".to_string()])
    );
    // A default key is not a substitute for either selected named account.
    assert_eq!(
        host.resolve_provider("zai").unwrap().configuration,
        ProviderConfigurationState::NotConfigured
    );
}

#[tokio::test]
async fn default_key_does_not_attribute_status_to_an_unenrolled_named_connection() {
    let (_home, config) = connection_config([Some("work"), /*default*/ None]).await;
    let host = status_host(&config, &[]).with_environment(|name| {
        if name == "ZAI_API_KEY" {
            EnvironmentCredentialMetadata::Present
        } else {
            EnvironmentCredentialMetadata::Missing
        }
    });
    assert_eq!(host.status_accounts(), BTreeMap::new());
    assert_eq!(
        host.connections_without_credential(),
        BTreeSet::from(["zai".to_string()])
    );
    assert_eq!(
        host.resolve_provider("zai").unwrap().configuration,
        ProviderConfigurationState::Configured
    );
}

#[tokio::test]
async fn single_connection_status_still_names_its_account_when_not_configured() {
    let (_home, mut config) = connection_config([Some("work"), /*default*/ None]).await;
    config.model_providers.remove("zai-anthropic");
    for (enrolled, expected) in [
        (Vec::new(), ProviderConfigurationState::NotConfigured),
        (
            vec![("zai", "work")],
            ProviderConfigurationState::Configured,
        ),
    ] {
        let host = status_host(&config, &enrolled);
        assert_eq!(host.status_accounts(), expected_name("work"));
        assert_eq!(
            host.resolve_provider("zai").unwrap().configuration,
            expected
        );
    }
}

#[tokio::test]
async fn provider_row_uses_the_account_from_the_same_host_as_its_configured_status() {
    let (_home, config) = connection_config([Some("work"), Some("spare")]).await;
    let host = status_host(&config, &[("zai-anthropic", "spare")]);
    let accounts = AccountsView::new(
        [("zai", "work"), ("zai-anthropic", "spare")]
            .into_iter()
            .map(|(provider, name)| NamedAccountRow {
                provider_id: provider.to_string(),
                name: codex_vault::ProviderAccountName::parse(name).unwrap(),
                kinds: vec![codex_vault::ProviderAccountKind::ApiKey],
                fingerprint: None,
                aws_profile: None,
                sign_in: None,
            })
            .collect(),
        &config,
        &config,
    )
    .with_status_accounts(&host);
    let status = host.resolve_provider("zai").unwrap();
    let item = provider_item(host.catalog().get("zai").unwrap(), &status, Some(&accounts));
    assert_eq!(item.name, "Z.AI · default");
    insta::assert_snapshot!(item.description.unwrap(), @"Enabled · configured (status of account `spare`) · default for new sessions");
    assert_eq!(
        host.connections_without_credential(),
        BTreeSet::from(["zai".to_string()])
    );
}
