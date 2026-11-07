//! Refresh-capable access to Claude Code subscription credentials.

use std::fs::OpenOptions;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use codex_vault::ClaudeAuthHealth;
use codex_vault::ClaudeAuthResolution;
use codex_vault::ClaudeAuthSelection;
use codex_vault::ClaudeAuthSource;
use codex_vault::ClaudeAuthSourceMetadata;
use codex_vault::ClaudeAuthStoreKind;
use codex_vault::ENVIRONMENT_CLAUDE_AUTH_SOURCE_ID;
use codex_vault::MANAGED_CLAUDE_AUTH_SOURCE_ID;
use codex_vault::ManagedClaudeTokenStatus;
use codex_vault::Vault;
use codex_vault::claude_auth_selection_sentinel_path;
use codex_vault::claude_code_macos_keychain_service;
use codex_vault::claude_environment_token_authority_id;
use codex_vault::credentials_file_claude_auth_source_id;
use codex_vault::macos_keychain_claude_auth_source_id;
use codex_vault::resolve_claude_auth_source;
use serde::Deserialize;
use tokio::process::Command;
use zeroize::Zeroizing;

#[path = "claude_refresh_lock.rs"]
mod refresh_lock;

use refresh_lock::ClaudeRefreshLock;

const CLAUDE_CREDENTIALS_FILE: &str = ".credentials.json";
const CLAUDE_REFRESH_LOCK_FILE: &str = ".pfterminal-oauth-refresh.lock";
const CLAUDE_CUSTOM_OAUTH_REFRESH_LOCK_FILE: &str = ".pfterminal-custom-oauth-refresh.lock";
const MIN_TOKEN_VALIDITY_MS: u64 = 60_000;
const CLAUDE_REFRESH_TIMEOUT: Duration = Duration::from_secs(30);
const CLAUDE_STATUS_TIMEOUT: Duration = Duration::from_secs(10);
const CLAUDE_STATUS_ENV_REMOVE: [&str; 8] = [
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
    "ANTHROPIC_MODEL",
];
// `CLAUDE_CODE_CUSTOM_OAUTH_URL` is intentionally preserved. Claude Code uses it to select
// the matching custom-OAuth profile and macOS Keychain service; removing it would verify a
// different login slot than the one Corbanu selected.
#[cfg(all(target_os = "macos", not(test)))]
const CLAUDE_KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(all(target_os = "macos", test))]
const CLAUDE_KEYCHAIN_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(target_os = "macos")]
// `security` reports errSecItemNotFound (-25300) as the shell exit status 44.
const SECURITY_ERR_SEC_ITEM_NOT_FOUND_EXIT: i32 = 44;

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct MissingMacosKeychainCredential;

#[cfg(target_os = "macos")]
impl std::fmt::Display for MissingMacosKeychainCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("macOS Keychain did not contain current Claude Code credentials")
    }
}

#[cfg(target_os = "macos")]
impl std::error::Error for MissingMacosKeychainCredential {}

#[derive(Deserialize)]
struct ClaudeCodeCredentials {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ClaudeCodeOauthCredentials>,
}

impl std::fmt::Debug for ClaudeCodeCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClaudeCodeCredentials")
            .field("claude_ai_oauth_present", &self.claude_ai_oauth.is_some())
            .finish()
    }
}

#[derive(Deserialize)]
struct ClaudeCodeOauthCredentials {
    #[serde(
        default,
        rename = "accessToken",
        deserialize_with = "deserialize_optional_zeroizing_string"
    )]
    access_token: Option<Zeroizing<String>>,
    #[serde(
        default,
        rename = "refreshToken",
        deserialize_with = "deserialize_optional_zeroizing_string"
    )]
    refresh_token: Option<Zeroizing<String>>,
    #[serde(rename = "expiresAt")]
    expires_at: Option<u64>,
    #[serde(default)]
    scopes: Vec<String>,
}

fn deserialize_optional_zeroizing_string<'de, D>(
    deserializer: D,
) -> Result<Option<Zeroizing<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(|value| value.map(Zeroizing::new))
}

#[derive(Deserialize)]
struct ClaudeAuthStatus {
    #[serde(rename = "loggedIn")]
    logged_in: bool,
    #[serde(rename = "authMethod")]
    auth_method: Option<String>,
    email: Option<String>,
    #[serde(rename = "orgId")]
    organization_id: Option<String>,
    #[serde(rename = "subscriptionType")]
    subscription_type: Option<String>,
}

#[allow(dead_code)] // The other variant is exercised by cross-platform fixtures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlatformCredentialStore {
    MacosKeychain,
    CredentialsFile,
}

impl PlatformCredentialStore {
    fn source_id(self, config_dir: &Path) -> Result<String> {
        match self {
            Self::MacosKeychain => Ok(macos_keychain_claude_auth_source_id(
                &claude_keychain_service(config_dir),
            )),
            Self::CredentialsFile => credentials_file_claude_auth_source_id(config_dir)
                .context("failed to identify Claude Code's credentials-file profile"),
        }
    }

    fn metadata_store(self) -> ClaudeAuthStoreKind {
        match self {
            Self::MacosKeychain => ClaudeAuthStoreKind::MacosKeychain,
            Self::CredentialsFile => {
                #[cfg(target_os = "macos")]
                {
                    ClaudeAuthStoreKind::LegacyCredentialsFile
                }
                #[cfg(not(target_os = "macos"))]
                {
                    ClaudeAuthStoreKind::CredentialsFile
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
const CURRENT_PLATFORM_STORE: PlatformCredentialStore = PlatformCredentialStore::MacosKeychain;
#[cfg(not(target_os = "macos"))]
const CURRENT_PLATFORM_STORE: PlatformCredentialStore = PlatformCredentialStore::CredentialsFile;

/// PF-84: the token of one named Claude account: its stored subscription
/// token, else the Claude Code login in the account's own config directory.
/// Never the home's selection, `CLAUDE_CODE_OAUTH_TOKEN` or another account.
pub(crate) async fn resolve_claude_account_access_token(
    codex_home: &Path,
    name: &codex_vault::ProviderAccountName,
) -> Result<Zeroizing<String>> {
    const CLAUDE_PLAN_PROVIDER_ID: &str = "claude-plan";
    let vault = Vault::new(codex_home.to_path_buf());
    if let Some(token) = vault
        .read_provider_account(
            CLAUDE_PLAN_PROVIDER_ID,
            name,
            codex_vault::ProviderAccountKind::ClaudeOauthToken,
        )
        .context("failed to read the Claude account")?
    {
        return Ok(token);
    }
    if let Some(config_dir) = vault
        .read_provider_account(
            CLAUDE_PLAN_PROVIDER_ID,
            name,
            codex_vault::ProviderAccountKind::ClaudeConfigDir,
        )
        .context("failed to read the Claude account")?
    {
        let requested = PathBuf::from(config_dir.as_str());
        if !requested.is_absolute() || !requested.is_dir() {
            return Err(anyhow!(
                "Claude account `{name}` uses the Claude Code config directory {}, which is \
                 not an existing absolute directory; add the account again with the right \
                 `--value`",
                requested.display()
            ));
        }
        let config_dir = claude_config_dir_for_profile(Path::new(""), Some(requested))?;
        let store = preferred_platform_store(&config_dir, /*security*/ None).await?;
        return resolve_claude_code_login_access_token(&config_dir, store).await;
    }
    Err(anyhow!(
        "Claude account `{name}` is not configured; add it with \
         `corbanu account add claude-plan {name} --kind claude-token` or choose another account"
    ))
}

pub(crate) async fn resolve_claude_oauth_access_token(
    codex_home: &Path,
) -> Result<Zeroizing<String>> {
    let explicit_selection_present = claude_auth_selection_sentinel_path(codex_home).exists();
    if let Some(token) = legacy_environment_token_without_explicit_selection(
        codex_home,
        nonempty_env("CLAUDE_CODE_OAUTH_TOKEN"),
    ) {
        return Ok(Zeroizing::new(token));
    }
    let vault = Vault::new(codex_home.to_path_buf());
    let selection = vault
        .load_claude_auth_selection()
        .context("failed to load the selected Claude authentication method")?;
    ensure_explicit_selection_is_available(explicit_selection_present, selection.as_ref())?;
    resolve_claude_oauth_access_token_for_selection(selection.as_ref(), &vault).await
}

fn ensure_explicit_selection_is_available(
    explicit_selection_present: bool,
    selection: Option<&ClaudeAuthSelection>,
) -> Result<()> {
    if explicit_selection_present && selection.is_none() {
        return Err(anyhow!(
            "the selected Claude authentication method is unavailable; run Claude authentication setup again"
        ));
    }
    Ok(())
}

fn legacy_environment_token_without_explicit_selection(
    codex_home: &Path,
    environment_token: Option<String>,
) -> Option<String> {
    (!claude_auth_selection_sentinel_path(codex_home).exists())
        .then_some(environment_token)
        .flatten()
}

async fn resolve_claude_oauth_access_token_for_selection(
    selection: Option<&ClaudeAuthSelection>,
    vault: &Vault,
) -> Result<Zeroizing<String>> {
    if let Some(selection) = selection {
        if selection.source == ClaudeAuthSource::ClaudeCodeLogin {
            return with_verified_claude_login_authority(
                selection,
                /*claude_executable*/ None,
                || resolve_selected_source_access_token(selection, vault),
            )
            .await;
        }
        return resolve_selected_source_access_token(selection, vault).await;
    }

    // Existing installations have no persisted selection. Preserve their exact
    // historical env-first behavior until the user confirms a method.
    if let Some(token) = nonempty_env("CLAUDE_CODE_OAUTH_TOKEN") {
        return Ok(Zeroizing::new(token));
    }

    resolve_current_platform_claude_oauth_access_token().await
}

async fn resolve_selected_source_access_token(
    selection: &ClaudeAuthSelection,
    vault: &Vault,
) -> Result<Zeroizing<String>> {
    let discovered = discover_selected_claude_auth_source(selection, vault).await?;
    let metadata = resolve_selected_claude_auth_source(selection, &discovered)?;
    match metadata.source {
        ClaudeAuthSource::ManagedSubscriptionToken => vault
            .load_managed_claude_subscription_token()
            .context(
                "the selected managed Claude subscription token is unavailable; run Claude authentication setup again",
            ),
        ClaudeAuthSource::EnvironmentToken => {
            selected_environment_token(selection).map(Zeroizing::new)
        }
        ClaudeAuthSource::ClaudeCodeLogin => {
            resolve_selected_claude_code_login_access_token(&metadata.source_id).await
        }
    }
}

fn selected_environment_token(selection: &ClaudeAuthSelection) -> Result<String> {
    resolve_selected_environment_token(selection, nonempty_env("CLAUDE_CODE_OAUTH_TOKEN"))
}

fn environment_token_health(
    selection: &ClaudeAuthSelection,
    token: Option<String>,
) -> ClaudeAuthHealth {
    match token {
        None => ClaudeAuthHealth::Missing,
        Some(token) => {
            if resolve_selected_environment_token(selection, Some(token)).is_ok() {
                ClaudeAuthHealth::Healthy
            } else {
                ClaudeAuthHealth::NeedsReauthorization
            }
        }
    }
}

fn resolve_selected_environment_token(
    selection: &ClaudeAuthSelection,
    token: Option<String>,
) -> Result<String> {
    let token = token.ok_or_else(|| {
        anyhow!(
            "the selected CLAUDE_CODE_OAUTH_TOKEN source is missing or blank; set it again or explicitly choose another Claude authentication method"
        )
    })?;
    let expected = selection.authority_id.as_deref().ok_or_else(|| {
        anyhow!(
            "the selected CLAUDE_CODE_OAUTH_TOKEN predates authority binding; explicitly choose it again"
        )
    })?;
    if expected != claude_environment_token_authority_id(&token) {
        return Err(anyhow!(
            "the selected CLAUDE_CODE_OAUTH_TOKEN changed; explicitly choose it again or select another Claude authentication method"
        ));
    }
    Ok(token)
}

async fn with_verified_claude_login_authority<T, Resolve, ResolveFuture>(
    selection: &ClaudeAuthSelection,
    claude_executable: Option<&Path>,
    resolve: Resolve,
) -> Result<T>
where
    Resolve: FnOnce() -> ResolveFuture,
    ResolveFuture: std::future::Future<Output = Result<T>>,
{
    // Fail before credential discovery or a refresh can mutate an externally replaced slot.
    verify_selected_claude_login_authority(selection, claude_executable).await?;
    let value = resolve().await?;
    // Recheck immediately before use so a replacement during the wider I/O window also fails.
    verify_selected_claude_login_authority(selection, claude_executable).await?;
    Ok(value)
}

async fn verify_selected_claude_login_authority(
    selection: &ClaudeAuthSelection,
    claude_executable: Option<&Path>,
) -> Result<()> {
    let config_dir_override = absolute_claude_config_dir_override()?;
    let custom_oauth_url =
        nonempty_env("CLAUDE_CODE_CUSTOM_OAUTH_URL").map(std::ffi::OsString::from);
    verify_selected_claude_login_authority_with_profile(
        selection,
        claude_executable,
        config_dir_override.as_deref(),
        custom_oauth_url.as_deref(),
    )
    .await
}

async fn verify_selected_claude_login_authority_with_profile(
    selection: &ClaudeAuthSelection,
    claude_executable: Option<&Path>,
    config_dir_override: Option<&Path>,
    custom_oauth_url: Option<&std::ffi::OsStr>,
) -> Result<()> {
    let security = claude_test_security_executable();
    verify_selected_claude_login_authority_with_profile_and_security(
        selection,
        claude_executable,
        config_dir_override,
        custom_oauth_url,
        security.as_deref(),
    )
    .await
}

async fn verify_selected_claude_login_authority_with_profile_and_security(
    selection: &ClaudeAuthSelection,
    claude_executable: Option<&Path>,
    config_dir_override: Option<&Path>,
    custom_oauth_url: Option<&std::ffi::OsStr>,
    security: Option<&Path>,
) -> Result<()> {
    let expected = selection.authority_id.as_deref().ok_or_else(|| {
        anyhow!(
            "the selected Claude Code login predates account binding; explicitly choose Claude Code login again"
        )
    })?;
    let config_dir = match config_dir_override {
        Some(config_dir) => config_dir.to_path_buf(),
        None => claude_config_dir()?,
    };
    let store = platform_store_for_source_id(&config_dir, &selection.source_id)?;
    verify_selected_platform_store_is_current(&config_dir, store, security).await?;
    let neutral_cwd = tempfile::tempdir().context("failed to isolate Claude account status")?;
    let executable = claude_executable.unwrap_or_else(|| Path::new("claude"));
    let mut command = Command::new(executable);
    command
        .args(["auth", "status", "--json"])
        .current_dir(neutral_cwd.path())
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(config_dir) = config_dir_override {
        command.env("CLAUDE_CONFIG_DIR", config_dir);
    } else {
        command.env_remove("CLAUDE_CONFIG_DIR");
    }
    for name in CLAUDE_STATUS_ENV_REMOVE {
        command.env_remove(name);
    }
    command.env_remove("CLAUDE_CODE_CUSTOM_OAUTH_URL");
    if let Some(custom_oauth_url) = custom_oauth_url {
        command.env("CLAUDE_CODE_CUSTOM_OAUTH_URL", custom_oauth_url);
    }
    let output = tokio::time::timeout(CLAUDE_STATUS_TIMEOUT, command.output())
        .await
        .map_err(|_| anyhow!("Claude Code account verification timed out"))?
        .context("failed to start Claude Code account verification")?;
    if !output.status.success() {
        return Err(anyhow!(
            "Claude Code could not verify the selected subscription account; explicitly choose Claude Code login again"
        ));
    }
    let status: ClaudeAuthStatus = serde_json::from_slice(&output.stdout)
        .map_err(|_| anyhow!("Claude Code returned invalid account status metadata"))?;
    if !status.logged_in || status.auth_method.as_deref() != Some("claude.ai") {
        return Err(anyhow!(
            "Claude Code is not signed in to the selected subscription account"
        ));
    }
    let actual = codex_vault::claude_login_authority_id(
        status.email.as_deref().unwrap_or_default(),
        status.organization_id.as_deref(),
        status.subscription_type.as_deref(),
    )
    .map_err(|_| anyhow!("Claude Code did not report a complete account identity"))?;
    if actual != expected {
        return Err(anyhow!(
            "the Claude Code account or subscription changed; explicitly choose Claude Code login again"
        ));
    }
    Ok(())
}

fn absolute_claude_config_dir_override() -> Result<Option<PathBuf>> {
    let Some(config_dir) = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    else {
        return Ok(None);
    };
    if config_dir.is_absolute() {
        Ok(Some(config_dir))
    } else {
        Ok(Some(std::path::absolute(config_dir).context(
            "failed to resolve the exact Claude Code configuration profile",
        )?))
    }
}

fn resolve_selected_claude_auth_source(
    selection: &ClaudeAuthSelection,
    discovered: &[ClaudeAuthSourceMetadata],
) -> Result<ClaudeAuthSourceMetadata> {
    match resolve_claude_auth_source(Some(selection), discovered) {
        ClaudeAuthResolution::Selected(metadata) => Ok(metadata),
        ClaudeAuthResolution::UnhealthySelected(metadata) => {
            Err(unhealthy_selected_source_error(&metadata))
        }
        ClaudeAuthResolution::MissingSelected(_) => Err(anyhow!(
            "the selected Claude authentication source no longer matches the current platform or profile; explicitly choose the intended method again"
        )),
        ClaudeAuthResolution::Conflict { .. } => Err(anyhow!(
            "the selected Claude authentication source is ambiguous; explicitly choose one current source again"
        )),
        ClaudeAuthResolution::SelectionRequired { .. } => {
            Err(anyhow!("Claude authentication selection is required"))
        }
    }
}

async fn discover_selected_claude_auth_source(
    selection: &ClaudeAuthSelection,
    vault: &Vault,
) -> Result<Vec<ClaudeAuthSourceMetadata>> {
    let source = selection.source;
    let metadata = match source {
        ClaudeAuthSource::ManagedSubscriptionToken => ClaudeAuthSourceMetadata {
            source: ClaudeAuthSource::ManagedSubscriptionToken,
            source_id: MANAGED_CLAUDE_AUTH_SOURCE_ID.to_string(),
            store: ClaudeAuthStoreKind::CorbanuVault,
            health: match vault.managed_claude_subscription_token_status() {
                Ok(ManagedClaudeTokenStatus::Stored { .. }) => ClaudeAuthHealth::Healthy,
                Ok(ManagedClaudeTokenStatus::Missing) => ClaudeAuthHealth::NeedsEnrollment,
                Err(_) => ClaudeAuthHealth::Unavailable,
            },
            account_hint: None,
        },
        ClaudeAuthSource::EnvironmentToken => ClaudeAuthSourceMetadata {
            source: ClaudeAuthSource::EnvironmentToken,
            source_id: ENVIRONMENT_CLAUDE_AUTH_SOURCE_ID.to_string(),
            store: ClaudeAuthStoreKind::Environment,
            health: environment_token_health(selection, nonempty_env("CLAUDE_CODE_OAUTH_TOKEN")),
            account_hint: None,
        },
        ClaudeAuthSource::ClaudeCodeLogin => {
            let config_dir = claude_config_dir()?;
            let credentials_path = config_dir.join(CLAUDE_CREDENTIALS_FILE);
            let store = platform_store_for_source_id(&config_dir, &selection.source_id)?;
            let health = match read_credentials(
                store,
                &config_dir,
                &credentials_path,
                /*security*/ None,
            )
            .await
            {
                Ok(credentials) => credentials_health(&credentials),
                Err(error) => credentials_error_health(&error),
            };
            ClaudeAuthSourceMetadata {
                source: ClaudeAuthSource::ClaudeCodeLogin,
                source_id: store.source_id(&config_dir)?,
                store: store.metadata_store(),
                health,
                account_hint: None,
            }
        }
    };
    Ok(vec![metadata])
}

fn credentials_error_health(error: &anyhow::Error) -> ClaudeAuthHealth {
    if error.downcast_ref::<serde_json::Error>().is_some() {
        ClaudeAuthHealth::Malformed
    } else if is_missing_macos_keychain_credential(error)
        || error
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::NotFound)
    {
        ClaudeAuthHealth::Missing
    } else {
        ClaudeAuthHealth::Unavailable
    }
}

#[cfg(target_os = "macos")]
fn is_missing_macos_keychain_credential(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<MissingMacosKeychainCredential>()
        .is_some()
}

#[cfg(not(target_os = "macos"))]
fn is_missing_macos_keychain_credential(_error: &anyhow::Error) -> bool {
    false
}

fn unhealthy_selected_source_error(metadata: &ClaudeAuthSourceMetadata) -> anyhow::Error {
    match metadata.health {
        ClaudeAuthHealth::NeedsEnrollment => anyhow!(
            "the selected managed Claude subscription token is missing; run Claude authentication setup again"
        ),
        ClaudeAuthHealth::NeedsReauthorization => match metadata.source {
            ClaudeAuthSource::EnvironmentToken => anyhow!(
                "the selected CLAUDE_CODE_OAUTH_TOKEN changed or predates authority binding; explicitly choose it again or select another Claude authentication method"
            ),
            _ => anyhow!(
                "the selected Claude Code login needs reauthorization; run `claude auth login` again or explicitly choose another method"
            ),
        },
        ClaudeAuthHealth::Missing => match metadata.source {
            ClaudeAuthSource::EnvironmentToken => anyhow!(
                "the selected CLAUDE_CODE_OAUTH_TOKEN source is missing or blank; set it again or explicitly choose another Claude authentication method"
            ),
            _ => anyhow!(
                "the selected Claude authentication source is missing; restore it or explicitly choose another method"
            ),
        },
        ClaudeAuthHealth::Malformed => anyhow!(
            "the selected Claude Code credential record is malformed; run `claude auth login` again"
        ),
        ClaudeAuthHealth::Unavailable => anyhow!(
            "the selected Claude authentication source is unavailable; restore access or explicitly choose another method"
        ),
        ClaudeAuthHealth::Healthy => {
            anyhow!("the selected Claude authentication source is healthy")
        }
    }
}

async fn resolve_current_platform_claude_oauth_access_token() -> Result<Zeroizing<String>> {
    let config_dir = claude_config_dir()?;

    let store = preferred_platform_store(&config_dir, /*security*/ None).await?;

    resolve_claude_code_login_access_token(&config_dir, store).await
}

async fn resolve_selected_claude_code_login_access_token(
    source_id: &str,
) -> Result<Zeroizing<String>> {
    let config_dir = claude_config_dir()?;
    let store = platform_store_for_source_id(&config_dir, source_id)?;
    resolve_claude_code_login_access_token(&config_dir, store).await
}

async fn resolve_claude_code_login_access_token(
    config_dir: &Path,
    store: PlatformCredentialStore,
) -> Result<Zeroizing<String>> {
    let force_refresh =
        nonempty_env("PFTERMINAL_PROVIDER_AUTH_FORCE_REFRESH").as_deref() == Some("1");
    resolve_stored_claude_oauth_access_token(
        config_dir,
        /*claude_executable*/ None,
        current_time_ms(),
        force_refresh,
        store,
        /*security*/ None,
    )
    .await
}

/// Validate the exact platform credential record used by provider resolution without
/// returning or printing credential material. The TUI calls this before persisting a
/// compatibility selection so a superficial CLI status cannot commit an unhealthy source.
pub(crate) async fn verify_current_platform_claude_login_health(
    selected_source_id: Option<&str>,
) -> Result<String> {
    let config_dir = claude_config_dir()?;
    let security = claude_test_security_executable();
    verify_current_platform_claude_login_health_with_profile(
        &config_dir,
        selected_source_id,
        security.as_deref(),
    )
    .await
}

async fn verify_current_platform_claude_login_health_with_profile(
    config_dir: &Path,
    selected_source_id: Option<&str>,
    security: Option<&Path>,
) -> Result<String> {
    let store = match selected_source_id {
        Some(source_id) => platform_store_for_source_id(config_dir, source_id)?,
        None => preferred_platform_store(config_dir, security).await?,
    };
    verify_selected_platform_store_is_current(config_dir, store, security).await?;
    verify_claude_login_health(config_dir, store, security).await?;
    store.source_id(config_dir)
}

/// A persisted legacy credentials-file selection is only valid on macOS while
/// the matching Keychain item is demonstrably absent. Otherwise `claude auth
/// status` would attest one store while provider resolution reads another.
async fn verify_selected_platform_store_is_current(
    config_dir: &Path,
    store: PlatformCredentialStore,
    security: Option<&Path>,
) -> Result<()> {
    #[cfg(target_os = "macos")]
    if store == PlatformCredentialStore::CredentialsFile {
        return match probe_macos_keychain_credential_presence(config_dir, security).await {
            Ok(MacosKeychainCredentialPresence::Absent) => Ok(()),
            Ok(MacosKeychainCredentialPresence::Present) => Err(anyhow!(
                "the selected legacy Claude credentials file is no longer valid because the matching macOS Keychain credential exists; explicitly choose Claude Code login again"
            )),
            Err(_) => Err(anyhow!(
                "could not prove the matching macOS Keychain credential is absent; explicitly choose Claude Code login again"
            )),
        };
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (config_dir, store, security);
    Ok(())
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MacosKeychainCredentialPresence {
    Present,
    Absent,
}

fn platform_store_for_source_id(
    config_dir: &Path,
    source_id: &str,
) -> Result<PlatformCredentialStore> {
    if source_id == CURRENT_PLATFORM_STORE.source_id(config_dir)? {
        return Ok(CURRENT_PLATFORM_STORE);
    }
    #[cfg(target_os = "macos")]
    if source_id == PlatformCredentialStore::CredentialsFile.source_id(config_dir)? {
        return Ok(PlatformCredentialStore::CredentialsFile);
    }
    Err(anyhow!(
        "the selected Claude Code login no longer matches this platform profile"
    ))
}

async fn preferred_platform_store(
    config_dir: &Path,
    security: Option<&Path>,
) -> Result<PlatformCredentialStore> {
    #[cfg(target_os = "macos")]
    {
        let credentials_path = config_dir.join(CLAUDE_CREDENTIALS_FILE);
        match read_credentials(
            PlatformCredentialStore::MacosKeychain,
            config_dir,
            &credentials_path,
            security,
        )
        .await
        {
            Ok(_) => return Ok(PlatformCredentialStore::MacosKeychain),
            Err(error)
                if error
                    .downcast_ref::<MissingMacosKeychainCredential>()
                    .is_some() =>
            {
                match tokio::fs::metadata(&credentials_path).await {
                    Ok(_) => return Ok(PlatformCredentialStore::CredentialsFile),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(error).context("failed to inspect legacy Claude credentials");
                    }
                }
            }
            Err(_) => return Ok(PlatformCredentialStore::MacosKeychain),
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (config_dir, security);
    Ok(CURRENT_PLATFORM_STORE)
}

#[cfg(all(target_os = "macos", debug_assertions))]
fn claude_test_security_executable() -> Option<PathBuf> {
    std::env::var_os("CORBANU_TEST_CLAUDE_SECURITY_EXECUTABLE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(not(all(target_os = "macos", debug_assertions)))]
fn claude_test_security_executable() -> Option<PathBuf> {
    None
}

async fn verify_claude_login_health(
    config_dir: &Path,
    store: PlatformCredentialStore,
    security: Option<&Path>,
) -> Result<()> {
    let credentials_path = config_dir.join(CLAUDE_CREDENTIALS_FILE);
    let health = match read_credentials(store, config_dir, &credentials_path, security).await {
        Ok(credentials) => credentials_health(&credentials),
        Err(error) => credentials_error_health(&error),
    };
    let metadata = ClaudeAuthSourceMetadata {
        source: ClaudeAuthSource::ClaudeCodeLogin,
        source_id: store.source_id(config_dir)?,
        store: store.metadata_store(),
        health,
        account_hint: None,
    };
    if health == ClaudeAuthHealth::Healthy {
        Ok(())
    } else {
        Err(unhealthy_selected_source_error(&metadata))
    }
}

fn claude_config_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| {
        anyhow!("cannot determine the home directory for Claude Code credentials")
    })?;
    claude_config_dir_for_profile(
        &home,
        std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
    )
}

fn claude_config_dir_for_profile(home: &Path, configured: Option<PathBuf>) -> Result<PathBuf> {
    match configured {
        Some(config_dir) => std::path::absolute(config_dir)
            .context("failed to resolve the exact Claude Code configuration profile"),
        None => Ok(home.join(".claude")),
    }
}

async fn resolve_stored_claude_oauth_access_token(
    config_dir: &Path,
    claude_executable: Option<&Path>,
    now_ms: u64,
    force_refresh: bool,
    store: PlatformCredentialStore,
    security: Option<&Path>,
) -> Result<Zeroizing<String>> {
    let credentials_path = config_dir.join(CLAUDE_CREDENTIALS_FILE);
    let credentials = read_credentials(store, config_dir, &credentials_path, security).await?;
    if !force_refresh && let Some(access_token) = usable_access_token(&credentials, now_ms) {
        return Ok(access_token);
    }
    let original_access_token = credentials
        .claude_ai_oauth
        .as_ref()
        .and_then(|oauth| oauth.access_token.clone());

    let _refresh_lock = acquire_refresh_lock(config_dir, store).await?;

    // Another PFTerminal process may have refreshed while this process waited.
    let credentials = read_credentials(store, config_dir, &credentials_path, security).await?;
    if let Some(access_token) = usable_access_token(&credentials, current_time_ms().max(now_ms))
        && (!force_refresh || Some(access_token.clone()) != original_access_token)
    {
        return Ok(access_token);
    }

    let refresh_status =
        refresh_with_claude_cli(config_dir, &credentials, claude_executable).await?;

    let refreshed = read_credentials(store, config_dir, &credentials_path, security).await?;
    if let Some(access_token) = usable_access_token(&refreshed, current_time_ms().max(now_ms))
        && Some(access_token.clone()) != original_access_token
    {
        return Ok(access_token);
    }
    if !refresh_status.success() {
        return Err(anyhow!(
            "Claude Code OAuth refresh failed with status {refresh_status}. Run `claude /login` again."
        ));
    }
    Err(anyhow!(
        "Claude Code completed OAuth refresh but did not persist a new usable access token. Run `claude /login` again."
    ))
}

async fn read_credentials(
    store: PlatformCredentialStore,
    config_dir: &Path,
    path: &Path,
    security: Option<&Path>,
) -> Result<ClaudeCodeCredentials> {
    let (contents, source) = match store {
        PlatformCredentialStore::CredentialsFile => {
            let contents = tokio::fs::read(path).await.with_context(|| {
                format!(
                    "Claude Code credentials were not available at {}. Run `claude auth login` with your Claude subscription.",
                    path.display()
                )
            })?;
            (Zeroizing::new(contents), path.display().to_string())
        }
        PlatformCredentialStore::MacosKeychain => {
            #[cfg(target_os = "macos")]
            {
                let contents = read_macos_keychain_credentials(config_dir, security).await?;
                (contents, "the macOS Keychain".to_string())
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = (config_dir, security);
                return Err(anyhow!(
                    "macOS Keychain Claude credentials are unavailable on this platform"
                ));
            }
        }
    };
    serde_json::from_slice(&contents).with_context(|| {
        format!(
            "Claude Code credentials for {source} are not valid JSON. Run `claude auth login` again."
        )
    })
}

#[cfg(target_os = "macos")]
async fn read_macos_keychain_credentials(
    config_dir: &Path,
    security: Option<&Path>,
) -> Result<Zeroizing<Vec<u8>>> {
    if cfg!(debug_assertions)
        && security.is_none()
        && std::env::var_os("CORBANU_TEST_NO_NATIVE_KEYRING").is_some()
    {
        return Err(MissingMacosKeychainCredential.into());
    }
    let account = std::env::var("USER")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(whoami::username);
    let service = claude_keychain_service(config_dir);
    let executable = security.unwrap_or_else(|| Path::new("/usr/bin/security"));
    let output = tokio::time::timeout(
        CLAUDE_KEYCHAIN_TIMEOUT,
        Command::new(executable)
            .args([
                "find-generic-password",
                "-a",
                account.as_str(),
                "-w",
                "-s",
                service.as_str(),
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "macOS Keychain lookup timed out"))?
    .with_context(|| format!("failed to start `{}`", executable.display()))?;
    if !output.status.success() {
        return if output.status.code() == Some(SECURITY_ERR_SEC_ITEM_NOT_FOUND_EXIT) {
            Err(MissingMacosKeychainCredential.into())
        } else {
            Err(anyhow!(
                "macOS Keychain lookup failed with status {}",
                output.status
            ))
        };
    }
    Ok(Zeroizing::new(output.stdout))
}

/// Check only whether the selected Keychain item exists. This deliberately
/// omits `-w` and discards all command output, so legacy-source validation
/// never materializes Keychain credential bytes.
#[cfg(target_os = "macos")]
async fn probe_macos_keychain_credential_presence(
    config_dir: &Path,
    security: Option<&Path>,
) -> Result<MacosKeychainCredentialPresence> {
    if cfg!(debug_assertions)
        && security.is_none()
        && std::env::var_os("CORBANU_TEST_NO_NATIVE_KEYRING").is_some()
    {
        return Ok(MacosKeychainCredentialPresence::Absent);
    }
    let account = std::env::var("USER")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(whoami::username);
    let service = claude_keychain_service(config_dir);
    let executable = security.unwrap_or_else(|| Path::new("/usr/bin/security"));
    let status = tokio::time::timeout(
        CLAUDE_KEYCHAIN_TIMEOUT,
        Command::new(executable)
            .args([
                "find-generic-password",
                "-a",
                account.as_str(),
                "-s",
                service.as_str(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "macOS Keychain lookup timed out"))?
    .with_context(|| format!("failed to start `{}`", executable.display()))?;
    if status.success() {
        Ok(MacosKeychainCredentialPresence::Present)
    } else if status.code() == Some(SECURITY_ERR_SEC_ITEM_NOT_FOUND_EXIT) {
        Ok(MacosKeychainCredentialPresence::Absent)
    } else {
        Err(anyhow!("macOS Keychain lookup failed with status {status}"))
    }
}

fn claude_keychain_service(config_dir: &Path) -> String {
    claude_code_macos_keychain_service(
        config_dir,
        std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|value| !value.is_empty())
            .is_some(),
        nonempty_env("CLAUDE_CODE_CUSTOM_OAUTH_URL").is_some(),
    )
}

fn usable_access_token(
    credentials: &ClaudeCodeCredentials,
    now_ms: u64,
) -> Option<Zeroizing<String>> {
    let oauth = credentials.claude_ai_oauth.as_ref()?;
    let expires_at = oauth.expires_at?;
    if expires_at <= now_ms.saturating_add(MIN_TOKEN_VALIDITY_MS) {
        return None;
    }
    oauth
        .access_token
        .as_ref()
        .map(|token| token.as_str())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(|token| Zeroizing::new(token.to_string()))
}

fn credentials_health(credentials: &ClaudeCodeCredentials) -> ClaudeAuthHealth {
    let Some(oauth) = credentials.claude_ai_oauth.as_ref() else {
        return ClaudeAuthHealth::NeedsReauthorization;
    };
    let has_refresh = oauth
        .refresh_token
        .as_ref()
        .map(|token| token.as_str())
        .map(str::trim)
        .is_some_and(|token| !token.is_empty());
    if has_refresh && !oauth.scopes.is_empty() {
        ClaudeAuthHealth::Healthy
    } else {
        ClaudeAuthHealth::NeedsReauthorization
    }
}

async fn refresh_with_claude_cli(
    config_dir: &Path,
    credentials: &ClaudeCodeCredentials,
    claude_executable: Option<&Path>,
) -> Result<std::process::ExitStatus> {
    let oauth = credentials.claude_ai_oauth.as_ref().ok_or_else(|| {
        anyhow!(
            "Claude Code OAuth credentials are missing. Run `claude /login` and choose a Claude subscription account."
        )
    })?;
    let refresh_token = oauth
        .refresh_token
        .as_ref()
        .map(|token| token.as_str())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            anyhow!("Claude Code OAuth refresh token is missing. Run `claude /login` again.")
        })?;
    if oauth.scopes.is_empty() {
        return Err(anyhow!(
            "Claude Code OAuth scopes are missing. Run `claude /login` again."
        ));
    }

    let executable = match claude_executable {
        Some(path) => path.to_path_buf(),
        None => which::which("claude")
            .context("Claude Code executable was not found on PATH; cannot refresh OAuth token")?,
    };
    let mut command = Command::new(&executable);
    command.args(["auth", "login"]);
    if claude_config_dir_is_default(config_dir) {
        // Claude Code's default profile has no config hash in its Keychain
        // service. Avoid creating a different service for the same directory.
        command.env_remove("CLAUDE_CONFIG_DIR");
    } else {
        command.env("CLAUDE_CONFIG_DIR", config_dir);
    }
    command
        .env("CLAUDE_CODE_OAUTH_REFRESH_TOKEN", refresh_token)
        .env("CLAUDE_CODE_OAUTH_SCOPES", oauth.scopes.join(" "))
        .env_remove("CLAUDE_CODE_OAUTH_TOKEN")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    tokio::time::timeout(CLAUDE_REFRESH_TIMEOUT, command.status())
        .await
        .map_err(|_| anyhow!("Claude Code OAuth refresh timed out after 30 seconds"))?
        .with_context(|| {
            format!(
                "failed to start `{}` for OAuth refresh",
                executable.display()
            )
        })
}

fn claude_config_dir_is_default(config_dir: &Path) -> bool {
    if std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .is_some()
    {
        return false;
    }
    dirs::home_dir().is_some_and(|home| config_dir == home.join(".claude"))
}

async fn acquire_refresh_lock(
    config_dir: &Path,
    store: PlatformCredentialStore,
) -> Result<ClaudeRefreshLock> {
    let lock_path = refresh_lock_path(config_dir, store)?;
    let lock_dir = lock_path
        .parent()
        .ok_or_else(|| anyhow!("Claude OAuth refresh lock has no parent directory"))?;
    tokio::fs::create_dir_all(lock_dir).await.with_context(|| {
        format!(
            "failed to create Claude Code config directory {}",
            lock_dir.display()
        )
    })?;
    tokio::task::spawn_blocking(move || -> io::Result<ClaudeRefreshLock> {
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        ClaudeRefreshLock::lock(lock_file)
    })
    .await
    .context("Claude OAuth refresh lock task failed")?
    .context("failed to acquire Claude OAuth refresh lock")
}

fn refresh_lock_path(config_dir: &Path, store: PlatformCredentialStore) -> Result<PathBuf> {
    Ok(refresh_lock_path_for_store(
        config_dir,
        store,
        nonempty_env("CLAUDE_CODE_CUSTOM_OAUTH_URL").is_some(),
    ))
}

fn refresh_lock_path_for_store(
    config_dir: &Path,
    store: PlatformCredentialStore,
    custom_oauth: bool,
) -> PathBuf {
    match store {
        PlatformCredentialStore::CredentialsFile => config_dir.join(CLAUDE_REFRESH_LOCK_FILE),
        PlatformCredentialStore::MacosKeychain => config_dir.join(if custom_oauth {
            CLAUDE_CUSTOM_OAUTH_REFRESH_LOCK_FILE
        } else {
            CLAUDE_REFRESH_LOCK_FILE
        }),
    }
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(all(test, unix))]
#[path = "claude_refresh_lock_tests.rs"]
mod refresh_lock_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn source_metadata(
        source: ClaudeAuthSource,
        source_id: &str,
        health: ClaudeAuthHealth,
    ) -> ClaudeAuthSourceMetadata {
        ClaudeAuthSourceMetadata {
            source,
            source_id: source_id.to_string(),
            store: ClaudeAuthStoreKind::Environment,
            health,
            account_hint: None,
        }
    }

    #[cfg(unix)]
    fn fake_claude_status(
        directory: &Path,
        email: &str,
        organization_id: &str,
        subscription_type: &str,
    ) -> (PathBuf, PathBuf, PathBuf) {
        let executable = directory.join(format!("status-claude-{organization_id}"));
        let cwd_log = directory.join(format!("status-cwd-{organization_id}"));
        let custom_oauth_log = directory.join(format!("status-custom-oauth-{organization_id}"));
        let script = format!(
            r#"#!/bin/sh
set -eu
[ "$1 $2 $3" = "auth status --json" ]
[ -z "${{CLAUDE_CODE_OAUTH_TOKEN+x}}" ]
[ -z "${{ANTHROPIC_API_KEY+x}}" ]
[ -z "${{ANTHROPIC_BASE_URL+x}}" ]
printf '%s' "$PWD" > '{}'
printf '%s' "${{CLAUDE_CODE_CUSTOM_OAUTH_URL-}}" > '{}'
printf '%s\n' '{{"loggedIn":true,"authMethod":"claude.ai","email":"{email}","orgId":"{organization_id}","subscriptionType":"{subscription_type}"}}'
"#,
            cwd_log.display(),
            custom_oauth_log.display()
        );
        std::fs::write(&executable, script).expect("write fake status Claude");
        let mut permissions = std::fs::metadata(&executable)
            .expect("fake status Claude metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions)
            .expect("make fake status Claude executable");
        (executable, cwd_log, custom_oauth_log)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn selected_claude_account_authority_isolated_and_fails_closed_on_drift() {
        let directory = tempfile::tempdir().expect("status fixture");
        let expected = codex_vault::claude_login_authority_id(
            "fixture@example.invalid",
            Some("org-work"),
            Some("max"),
        )
        .expect("authority");
        let config_dir = claude_config_dir().expect("current Claude profile");
        let source_id = CURRENT_PLATFORM_STORE
            .source_id(&config_dir)
            .expect("current platform source id");
        let selection =
            ClaudeAuthSelection::new_claude_code_login(source_id, expected).expect("selection");
        let (matching, matching_cwd, custom_oauth_log) = fake_claude_status(
            directory.path(),
            "fixture@example.invalid",
            "org-work",
            "max",
        );
        let (drifted, _, _) = fake_claude_status(
            directory.path(),
            "other@example.invalid",
            "org-personal",
            "team",
        );
        let (incomplete, _, _) = fake_claude_status(directory.path(), "", "org-incomplete", "max");

        let custom_oauth_url = std::ffi::OsStr::new("https://oauth.example.invalid");
        verify_selected_claude_login_authority_with_profile(
            &selection,
            Some(&matching),
            /*config_dir_override*/ None,
            Some(custom_oauth_url),
        )
        .await
        .expect("matching authority");
        let observed_cwd = std::fs::read_to_string(matching_cwd).expect("status cwd");
        assert_ne!(Path::new(&observed_cwd), std::env::current_dir().unwrap());
        assert_eq!(
            std::fs::read_to_string(custom_oauth_log).expect("custom OAuth profile"),
            custom_oauth_url.to_string_lossy()
        );
        let error = verify_selected_claude_login_authority(&selection, Some(&drifted))
            .await
            .expect_err("account drift must fail closed");
        assert!(
            error
                .to_string()
                .contains("account or subscription changed")
        );
        assert!(!error.to_string().contains("fixture@example.invalid"));
        assert!(!error.to_string().contains("other@example.invalid"));

        let refresh_executable = directory.path().join("must-not-refresh");
        let refresh_marker = directory.path().join("unexpected-refresh-side-effect");
        std::fs::write(
            &refresh_executable,
            format!(
                "#!/bin/sh\nprintf invoked > '{}'\n",
                refresh_marker.display()
            ),
        )
        .expect("write forbidden refresh fixture");
        let mut permissions = std::fs::metadata(&refresh_executable)
            .expect("forbidden refresh fixture metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&refresh_executable, permissions)
            .expect("make forbidden refresh fixture executable");
        let error = with_verified_claude_login_authority(&selection, Some(&drifted), || async {
            Command::new(&refresh_executable)
                .status()
                .await
                .context("run forbidden refresh fixture")?;
            Ok("never-returned")
        })
        .await
        .expect_err("authority drift must block refresh before side effects");
        assert!(
            error
                .to_string()
                .contains("account or subscription changed")
        );
        assert!(!refresh_marker.exists());

        let error = verify_selected_claude_login_authority(&selection, Some(&incomplete))
            .await
            .expect_err("missing subscription identity must fail closed");
        assert!(error.to_string().contains("complete account identity"));
    }

    #[test]
    fn production_selection_path_uses_exact_typed_resolution() {
        let selection = ClaudeAuthSelection::new_at(
            ClaudeAuthSource::EnvironmentToken,
            ENVIRONMENT_CLAUDE_AUTH_SOURCE_ID,
            /*selected_at*/ 10,
        )
        .expect("selection");
        let selected = source_metadata(
            ClaudeAuthSource::EnvironmentToken,
            ENVIRONMENT_CLAUDE_AUTH_SOURCE_ID,
            ClaudeAuthHealth::Healthy,
        );
        assert_eq!(
            resolve_selected_claude_auth_source(&selection, std::slice::from_ref(&selected))
                .expect("exact healthy source"),
            selected
        );

        let drifted = source_metadata(
            ClaudeAuthSource::EnvironmentToken,
            "environment:different-account-slot",
            ClaudeAuthHealth::Healthy,
        );
        let error = resolve_selected_claude_auth_source(&selection, &[drifted])
            .expect_err("profile drift must fail closed");
        assert!(error.to_string().contains("no longer matches"));

        let error = resolve_selected_claude_auth_source(&selection, &[selected.clone(), selected])
            .expect_err("duplicate identities must fail as a conflict");
        assert!(error.to_string().contains("ambiguous"));
    }

    #[test]
    fn relative_config_override_resolves_the_persisted_platform_source() {
        let relative = PathBuf::from("target/claude-relative-profile-fixture");
        let absolute = std::path::absolute(&relative).expect("absolute fixture profile");
        let persisted_source_id = CURRENT_PLATFORM_STORE
            .source_id(&absolute)
            .expect("persisted platform source id");
        let runtime_config_dir =
            claude_config_dir_for_profile(Path::new("/unused-home"), Some(relative))
                .expect("runtime profile");

        assert_eq!(runtime_config_dir, absolute);
        assert_eq!(
            platform_store_for_source_id(&runtime_config_dir, &persisted_source_id)
                .expect("resolve persisted platform source"),
            CURRENT_PLATFORM_STORE,
        );
    }

    #[test]
    fn default_profile_identity_uses_the_shared_home_resolver() {
        let config_dir_overridden = std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|value| !value.is_empty())
            .is_some();
        let home = dirs::home_dir().expect("home directory");

        assert_eq!(
            claude_config_dir_is_default(&home.join(".claude")),
            !config_dir_overridden
        );
        assert!(!claude_config_dir_is_default(&home.join("other-claude")));
    }

    #[test]
    fn selected_environment_token_is_bound_and_fails_closed_on_drift() {
        let selection = ClaudeAuthSelection::new_environment_token("selected-environment-token")
            .expect("selection");

        assert_eq!(
            resolve_selected_environment_token(
                &selection,
                Some("selected-environment-token".to_string()),
            )
            .expect("matching token"),
            "selected-environment-token"
        );
        let error = resolve_selected_environment_token(
            &selection,
            Some("replaced-environment-token".to_string()),
        )
        .expect_err("environment token drift must fail closed");
        assert!(error.to_string().contains("changed"));
        assert!(!error.to_string().contains("replaced-environment-token"));
        assert!(!error.to_string().contains("selected-environment-token"));

        let legacy = ClaudeAuthSelection::new_at(
            ClaudeAuthSource::EnvironmentToken,
            ENVIRONMENT_CLAUDE_AUTH_SOURCE_ID,
            /*selected_at*/ 10,
        )
        .expect("legacy selection");
        let error = resolve_selected_environment_token(
            &legacy,
            Some("selected-environment-token".to_string()),
        )
        .expect_err("unbound explicit selection must fail closed");
        assert!(error.to_string().contains("predates authority binding"));
    }

    #[test]
    fn legacy_environment_token_bypasses_vault_only_before_explicit_selection() {
        let codex_home = tempfile::tempdir().expect("codex home");
        let token = "legacy-environment-token".to_string();

        assert_eq!(
            legacy_environment_token_without_explicit_selection(
                codex_home.path(),
                Some(token.clone()),
            ),
            Some(token.clone())
        );

        std::fs::write(claude_auth_selection_sentinel_path(codex_home.path()), "")
            .expect("selection sentinel");
        assert_eq!(
            legacy_environment_token_without_explicit_selection(codex_home.path(), Some(token),),
            None
        );
    }

    #[test]
    fn explicit_selection_sentinel_without_selection_fails_closed() {
        ensure_explicit_selection_is_available(
            /*explicit_selection_present*/ true, /*selection*/ None,
        )
        .expect_err("missing explicit selection must fail closed");
        ensure_explicit_selection_is_available(
            /*explicit_selection_present*/ false, /*selection*/ None,
        )
        .expect("legacy installation may resolve its historical sources");

        let selection = ClaudeAuthSelection::new(
            ClaudeAuthSource::ManagedSubscriptionToken,
            MANAGED_CLAUDE_AUTH_SOURCE_ID,
        )
        .expect("selection");
        ensure_explicit_selection_is_available(
            /*explicit_selection_present*/ true,
            Some(&selection),
        )
        .expect("persisted explicit selection is available");
    }

    #[test]
    fn missing_selected_environment_token_reaches_production_recovery_wording() {
        let selection = ClaudeAuthSelection::new_environment_token("selected-environment-token")
            .expect("selection");
        let metadata = source_metadata(
            ClaudeAuthSource::EnvironmentToken,
            ENVIRONMENT_CLAUDE_AUTH_SOURCE_ID,
            environment_token_health(&selection, /*token*/ None),
        );

        let error = resolve_selected_claude_auth_source(&selection, &[metadata])
            .expect_err("missing environment token must fail closed");

        assert!(error.to_string().contains("missing or blank"));
        assert!(!error.to_string().contains("changed"));
    }

    #[test]
    fn credentials_file_profile_change_fails_closed_in_production_resolution() {
        let work_profile = tempfile::tempdir().expect("work profile");
        let personal_profile = tempfile::tempdir().expect("personal profile");
        let selected_source_id = PlatformCredentialStore::CredentialsFile
            .source_id(work_profile.path())
            .expect("work source id");
        let selection = ClaudeAuthSelection::new_at(
            ClaudeAuthSource::ClaudeCodeLogin,
            selected_source_id,
            /*selected_at*/ 10,
        )
        .expect("selection");
        let discovered = ClaudeAuthSourceMetadata {
            source: ClaudeAuthSource::ClaudeCodeLogin,
            source_id: PlatformCredentialStore::CredentialsFile
                .source_id(personal_profile.path())
                .expect("personal source id"),
            store: ClaudeAuthStoreKind::CredentialsFile,
            health: ClaudeAuthHealth::Healthy,
            account_hint: None,
        };

        let error = resolve_selected_claude_auth_source(&selection, &[discovered])
            .expect_err("changing credentials-file profiles must fail closed");
        assert!(error.to_string().contains("no longer matches"));
    }

    #[tokio::test]
    async fn metadata_only_login_health_rejects_blank_refresh_credentials() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let now_ms = current_time_ms();
        write_credentials(temp_dir.path(), "live-access", "", now_ms + 120_000);

        let error = verify_claude_login_health(
            temp_dir.path(),
            PlatformCredentialStore::CredentialsFile,
            /*security*/ None,
        )
        .await
        .expect_err("blank refresh token must not be selected");

        assert!(error.to_string().contains("needs reauthorization"));
    }

    #[tokio::test]
    async fn valid_access_token_does_not_require_claude_cli() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let now_ms = current_time_ms();
        write_credentials(
            temp_dir.path(),
            "valid-access",
            "valid-refresh",
            now_ms + 120_000,
        );

        let token = resolve_stored_claude_oauth_access_token(
            temp_dir.path(),
            Some(Path::new("/does/not/exist")),
            now_ms,
            /*force_refresh*/ false,
            PlatformCredentialStore::CredentialsFile,
            /*security*/ None,
        )
        .await
        .expect("valid token should be returned without starting Claude");

        assert_eq!(token.as_str(), "valid-access");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn expiring_access_token_is_refreshed_by_claude_cli() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let now_ms = current_time_ms();
        write_credentials(
            temp_dir.path(),
            "expiring-access",
            "rotating-refresh",
            now_ms + 30_000,
        );
        let claude = fake_refreshing_claude(
            temp_dir.path(),
            now_ms + 600_000,
            /*sleep_seconds*/ 0,
            /*exit_code*/ 0,
        );

        let token = resolve_stored_claude_oauth_access_token(
            temp_dir.path(),
            Some(&claude),
            now_ms,
            /*force_refresh*/ false,
            PlatformCredentialStore::CredentialsFile,
            /*security*/ None,
        )
        .await
        .expect("Claude CLI should refresh expiring credentials");

        assert_eq!(token.as_str(), "refreshed-access");
        let refresh_log =
            std::fs::read_to_string(temp_dir.path().join("refresh.log")).expect("refresh log");
        assert_eq!(
            refresh_log,
            "rotating-refresh|user:profile user:inference\n"
        );
        let persisted = read_credentials(
            PlatformCredentialStore::CredentialsFile,
            temp_dir.path(),
            &temp_dir.path().join(CLAUDE_CREDENTIALS_FILE),
            /*security*/ None,
        )
        .await
        .expect("persisted credentials");
        assert_eq!(
            persisted
                .claude_ai_oauth
                .and_then(|oauth| oauth.refresh_token)
                .as_ref()
                .map(|token| token.as_str()),
            Some("refreshed-refresh")
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_refreshes_share_one_claude_exchange() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let now_ms = current_time_ms();
        write_credentials(
            temp_dir.path(),
            "expired-access",
            "single-use-refresh",
            now_ms.saturating_sub(1),
        );
        let claude = fake_refreshing_claude(
            temp_dir.path(),
            now_ms + 600_000,
            /*sleep_seconds*/ 1,
            /*exit_code*/ 0,
        );
        let config_a = temp_dir.path().to_path_buf();
        let config_b = config_a.clone();
        let claude_a = claude.clone();
        let claude_b = claude.clone();

        let (first, second) = tokio::join!(
            resolve_stored_claude_oauth_access_token(
                &config_a,
                Some(&claude_a),
                now_ms,
                /*force_refresh*/ false,
                PlatformCredentialStore::CredentialsFile,
                /*security*/ None,
            ),
            resolve_stored_claude_oauth_access_token(
                &config_b,
                Some(&claude_b),
                now_ms,
                /*force_refresh*/ false,
                PlatformCredentialStore::CredentialsFile,
                /*security*/ None,
            ),
        );

        assert_eq!(first.expect("first refresh").as_str(), "refreshed-access");
        assert_eq!(second.expect("second refresh").as_str(), "refreshed-access");
        let refresh_log =
            std::fs::read_to_string(temp_dir.path().join("refresh.log")).expect("refresh log");
        assert_eq!(refresh_log.lines().count(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn forced_refresh_replaces_an_unexpired_rejected_token() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let now_ms = current_time_ms();
        write_credentials(
            temp_dir.path(),
            "rejected-but-unexpired-access",
            "refresh-after-401",
            now_ms + 600_000,
        );
        let claude = fake_refreshing_claude(
            temp_dir.path(),
            now_ms + 900_000,
            /*sleep_seconds*/ 0,
            /*exit_code*/ 0,
        );

        let token = resolve_stored_claude_oauth_access_token(
            temp_dir.path(),
            Some(&claude),
            now_ms,
            /*force_refresh*/ true,
            PlatformCredentialStore::CredentialsFile,
            /*security*/ None,
        )
        .await
        .expect("401 recovery should force Claude credential rotation");

        assert_eq!(token.as_str(), "refreshed-access");
        assert_eq!(
            std::fs::read_to_string(temp_dir.path().join("refresh.log"))
                .expect("refresh log")
                .lines()
                .count(),
            1
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rotated_credentials_win_over_nonzero_claude_exit() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let now_ms = current_time_ms();
        write_credentials(
            temp_dir.path(),
            "expired-access",
            "refresh-before-nonzero-exit",
            now_ms.saturating_sub(1),
        );
        let claude = fake_refreshing_claude(
            temp_dir.path(),
            now_ms + 600_000,
            /*sleep_seconds*/ 0,
            /*exit_code*/ 1,
        );

        let token = resolve_stored_claude_oauth_access_token(
            temp_dir.path(),
            Some(&claude),
            now_ms,
            /*force_refresh*/ false,
            PlatformCredentialStore::CredentialsFile,
            /*security*/ None,
        )
        .await
        .expect("persisted credential rotation is the refresh authority");

        assert_eq!(token.as_str(), "refreshed-access");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_refresh_preserves_credentials_and_redacts_refresh_token() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let now_ms = current_time_ms();
        write_credentials(
            temp_dir.path(),
            "expired-access",
            "never-print-this-refresh-token",
            now_ms.saturating_sub(1),
        );
        let credentials_path = temp_dir.path().join(CLAUDE_CREDENTIALS_FILE);
        let before = std::fs::read(&credentials_path).expect("credentials before refresh");
        let claude = fake_failing_claude(temp_dir.path());

        let error = resolve_stored_claude_oauth_access_token(
            temp_dir.path(),
            Some(&claude),
            now_ms,
            /*force_refresh*/ false,
            PlatformCredentialStore::CredentialsFile,
            /*security*/ None,
        )
        .await
        .expect_err("failed Claude exchange should surface");

        assert!(error.to_string().contains("OAuth refresh failed"));
        assert!(!error.to_string().contains("never-print-this-refresh-token"));
        assert_eq!(
            std::fs::read(credentials_path).expect("credentials after refresh"),
            before
        );
    }

    #[tokio::test]
    async fn blank_refresh_token_requires_reauthorization_without_starting_claude() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let now_ms = current_time_ms();
        write_credentials(
            temp_dir.path(),
            "expired-access",
            "",
            now_ms.saturating_sub(1),
        );

        let error = resolve_stored_claude_oauth_access_token(
            temp_dir.path(),
            Some(Path::new("/does/not/exist")),
            now_ms,
            /*force_refresh*/ false,
            PlatformCredentialStore::CredentialsFile,
            /*security*/ None,
        )
        .await
        .expect_err("blank rotating refresh token must require login");

        assert!(error.to_string().contains("refresh token is missing"));
        assert!(!error.to_string().contains("expired-access"));
    }

    #[test]
    fn platform_fixture_health_classifies_stale_live_and_blank_refresh_records() {
        let now_ms = 1_000_000;
        let refreshable: ClaudeCodeCredentials = serde_json::from_value(json!({
            "claudeAiOauth": {
                "accessToken": "stale-access",
                "refreshToken": "rotating-refresh",
                "expiresAt": now_ms - 1,
                "scopes": ["user:inference"]
            }
        }))
        .expect("refreshable fixture");
        let blank_refresh: ClaudeCodeCredentials = serde_json::from_value(json!({
            "claudeAiOauth": {
                "accessToken": "stale-access",
                "refreshToken": "  ",
                "expiresAt": now_ms - 1,
                "scopes": ["user:inference"]
            }
        }))
        .expect("blank refresh fixture");
        let live_access_blank_refresh: ClaudeCodeCredentials = serde_json::from_value(json!({
            "claudeAiOauth": {
                "accessToken": "live-access",
                "refreshToken": "  ",
                "expiresAt": now_ms + MIN_TOKEN_VALIDITY_MS + 1,
                "scopes": ["user:inference"]
            }
        }))
        .expect("live access with blank refresh fixture");

        assert_eq!(credentials_health(&refreshable), ClaudeAuthHealth::Healthy);
        assert_eq!(
            credentials_health(&blank_refresh),
            ClaudeAuthHealth::NeedsReauthorization
        );
        assert_eq!(
            credentials_health(&live_access_blank_refresh),
            ClaudeAuthHealth::NeedsReauthorization
        );
    }

    #[test]
    fn platform_fixture_preserves_missing_optional_token_fields() {
        let missing_access: ClaudeCodeCredentials = serde_json::from_value(json!({
            "claudeAiOauth": {
                "refreshToken": "rotating-refresh",
                "expiresAt": 1_000_000,
                "scopes": ["user:inference"]
            }
        }))
        .expect("missing access token remains optional");
        let missing_refresh: ClaudeCodeCredentials = serde_json::from_value(json!({
            "claudeAiOauth": {
                "accessToken": "live-access",
                "expiresAt": 1_000_000,
                "scopes": ["user:inference"]
            }
        }))
        .expect("missing refresh token remains optional");

        assert!(
            missing_access
                .claude_ai_oauth
                .as_ref()
                .is_some_and(|oauth| oauth.access_token.is_none())
        );
        assert!(
            missing_refresh
                .claude_ai_oauth
                .as_ref()
                .is_some_and(|oauth| oauth.refresh_token.is_none())
        );
    }

    #[tokio::test]
    async fn credentials_file_fixture_distinguishes_missing_and_malformed_records() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let path = temp_dir.path().join(CLAUDE_CREDENTIALS_FILE);
        let missing = read_credentials(
            PlatformCredentialStore::CredentialsFile,
            temp_dir.path(),
            &path,
            /*security*/ None,
        )
        .await
        .expect_err("missing fixture");
        assert_eq!(
            credentials_error_health(&missing),
            ClaudeAuthHealth::Missing
        );

        std::fs::write(&path, b"not-json").expect("malformed fixture");
        let malformed = read_credentials(
            PlatformCredentialStore::CredentialsFile,
            temp_dir.path(),
            &path,
            /*security*/ None,
        )
        .await
        .expect_err("malformed fixture");
        assert_eq!(
            credentials_error_health(&malformed),
            ClaudeAuthHealth::Malformed
        );
    }

    #[test]
    fn refresh_lock_follows_the_authoritative_store_identity() {
        let config_a = Path::new("/fixture/config-a");
        let config_b = Path::new("/fixture/config-b");

        assert_ne!(
            refresh_lock_path_for_store(
                config_a,
                PlatformCredentialStore::CredentialsFile,
                /*custom_oauth*/ false
            ),
            refresh_lock_path_for_store(
                config_b,
                PlatformCredentialStore::CredentialsFile,
                /*custom_oauth*/ false
            )
        );
        let keychain_lock = refresh_lock_path_for_store(
            config_a,
            PlatformCredentialStore::MacosKeychain,
            /*custom_oauth*/ false,
        );
        assert_ne!(
            keychain_lock,
            refresh_lock_path_for_store(
                config_b,
                PlatformCredentialStore::MacosKeychain,
                /*custom_oauth*/ false,
            )
        );
        assert_ne!(
            keychain_lock,
            refresh_lock_path_for_store(
                config_a,
                PlatformCredentialStore::MacosKeychain,
                /*custom_oauth*/ true,
            )
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn current_claude_keychain_credentials_are_supported() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let now_ms = current_time_ms();
        let security = fake_keychain_security(temp_dir.path(), now_ms + 600_000);
        write_credentials(
            temp_dir.path(),
            "legacy-file-access",
            "legacy-file-refresh",
            now_ms + 600_000,
        );

        let credentials = read_credentials(
            PlatformCredentialStore::MacosKeychain,
            temp_dir.path(),
            &temp_dir.path().join(CLAUDE_CREDENTIALS_FILE),
            Some(&security),
        )
        .await
        .expect("keychain credentials");

        assert_eq!(
            usable_access_token(&credentials, now_ms)
                .as_ref()
                .map(|token| token.as_str()),
            Some("keychain-access")
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn missing_keychain_selects_the_exact_legacy_file_profile() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let now_ms = current_time_ms();
        write_credentials(
            temp_dir.path(),
            "legacy-file-access",
            "legacy-file-refresh",
            now_ms + 600_000,
        );
        let security = temp_dir.path().join("missing-security");
        std::fs::write(&security, "#!/bin/sh\nexit 44\n").expect("write missing security");
        let mut permissions = std::fs::metadata(&security)
            .expect("missing security metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&security, permissions).expect("make missing security executable");

        let store = preferred_platform_store(temp_dir.path(), Some(&security))
            .await
            .expect("select legacy file");
        let source_id = store.source_id(temp_dir.path()).expect("legacy source id");

        assert_eq!(store, PlatformCredentialStore::CredentialsFile);
        assert_eq!(
            store.metadata_store(),
            ClaudeAuthStoreKind::LegacyCredentialsFile
        );
        assert_eq!(
            platform_store_for_source_id(temp_dir.path(), &source_id)
                .expect("resolve exact source"),
            PlatformCredentialStore::CredentialsFile
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn persisted_legacy_file_requires_proven_keychain_absence() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let now_ms = current_time_ms();
        write_credentials(
            temp_dir.path(),
            "legacy-file-access",
            "legacy-file-refresh",
            now_ms + 600_000,
        );
        let missing_security =
            fake_security_exit(temp_dir.path(), "missing-security", /*exit_code*/ 44);
        let source_id = PlatformCredentialStore::CredentialsFile
            .source_id(temp_dir.path())
            .expect("legacy source id");
        verify_current_platform_claude_login_health_with_profile(
            temp_dir.path(),
            Some(&source_id),
            Some(&missing_security),
        )
        .await
        .expect("typed item-not-found permits the selected legacy file");

        let present_security = fake_keychain_security(temp_dir.path(), now_ms + 600_000);
        let denied_security =
            fake_security_exit(temp_dir.path(), "denied-security", /*exit_code*/ 7);
        let unavailable_security = temp_dir.path().join("unavailable-security");
        for security in [
            present_security.as_path(),
            denied_security.as_path(),
            unavailable_security.as_path(),
        ] {
            let error = verify_current_platform_claude_login_health_with_profile(
                temp_dir.path(),
                Some(&source_id),
                Some(security),
            )
            .await
            .expect_err("legacy file must fail closed unless Keychain absence is proven");
            assert!(!error.to_string().contains("legacy-file-access"));
        }
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn legacy_keychain_presence_probe_never_requests_credential_bytes() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let secret_read_marker = temp_dir.path().join("secret-read-requested");
        let security = temp_dir.path().join("metadata-only-security");
        std::fs::write(
            &security,
            format!(
                "#!/bin/sh\nfor argument in \"$@\"; do\n  [ \"$argument\" != \"-w\" ] || {{ touch '{}'; exit 7; }}\ndone\nexit 44\n",
                secret_read_marker.display(),
            ),
        )
        .expect("write metadata-only security fixture");
        let mut permissions = std::fs::metadata(&security)
            .expect("security fixture metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&security, permissions).expect("make security fixture executable");

        assert_eq!(
            probe_macos_keychain_credential_presence(temp_dir.path(), Some(&security))
                .await
                .expect("presence probe"),
            MacosKeychainCredentialPresence::Absent
        );
        assert!(!secret_read_marker.exists());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn present_keychain_blocks_legacy_selection_before_status_invocation() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let now_ms = current_time_ms();
        write_credentials(
            temp_dir.path(),
            "legacy-file-access",
            "legacy-file-refresh",
            now_ms + 600_000,
        );
        let marker = temp_dir.path().join("status-invoked");
        let status = temp_dir.path().join("status-claude");
        std::fs::write(
            &status,
            format!(
                "#!/bin/sh\ntouch '{}'\nprintf '{{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"email\":\"fixture@example.invalid\",\"orgId\":\"org-fixture\",\"subscriptionType\":\"max\"}}\\n'\n",
                marker.display()
            ),
        )
        .expect("write status fixture");
        let mut permissions = std::fs::metadata(&status)
            .expect("status metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&status, permissions).expect("make status executable");
        let selection = ClaudeAuthSelection::new_claude_code_login(
            PlatformCredentialStore::CredentialsFile
                .source_id(temp_dir.path())
                .expect("legacy source id"),
            codex_vault::claude_login_authority_id(
                "fixture@example.invalid",
                Some("org-fixture"),
                Some("max"),
            )
            .expect("authority"),
        )
        .expect("selection");
        let security = fake_keychain_security(temp_dir.path(), now_ms + 600_000);

        let error = verify_selected_claude_login_authority_with_profile_and_security(
            &selection,
            Some(&status),
            Some(temp_dir.path()),
            /*custom_oauth_url*/ None,
            Some(&security),
        )
        .await
        .expect_err("present Keychain must block the legacy selection before status");

        assert!(error.to_string().contains("no longer valid"));
        assert!(!marker.exists());
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn malformed_or_unavailable_keychain_never_falls_back_to_legacy_file() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let now_ms = current_time_ms();
        write_credentials(
            temp_dir.path(),
            "legacy-file-access",
            "legacy-file-refresh",
            now_ms + 600_000,
        );
        let malformed_security = temp_dir.path().join("malformed-security");
        std::fs::write(&malformed_security, "#!/bin/sh\nprintf 'not-json\\n'\n")
            .expect("write malformed security");
        let mut permissions = std::fs::metadata(&malformed_security)
            .expect("malformed security metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&malformed_security, permissions)
            .expect("make malformed security executable");
        let denied_security = temp_dir.path().join("denied-security");
        std::fs::write(&denied_security, "#!/bin/sh\nexit 7\n").expect("write denied security");
        let mut permissions = std::fs::metadata(&denied_security)
            .expect("denied security metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&denied_security, permissions)
            .expect("make denied security executable");

        let unavailable_security = temp_dir.path().join("unavailable-security");
        for security in [
            malformed_security.as_path(),
            denied_security.as_path(),
            unavailable_security.as_path(),
        ] {
            assert_eq!(
                preferred_platform_store(temp_dir.path(), Some(security))
                    .await
                    .expect("retain authoritative Keychain store"),
                PlatformCredentialStore::MacosKeychain
            );
        }
    }

    fn write_credentials(
        config_dir: &Path,
        access_token: &str,
        refresh_token: &str,
        expires_at: u64,
    ) {
        let credentials = json!({
            "claudeAiOauth": {
                "accessToken": access_token,
                "refreshToken": refresh_token,
                "expiresAt": expires_at,
                "scopes": ["user:profile", "user:inference"]
            }
        });
        std::fs::write(
            config_dir.join(CLAUDE_CREDENTIALS_FILE),
            serde_json::to_vec(&credentials).expect("serialize credentials"),
        )
        .expect("write credentials");
    }

    #[cfg(unix)]
    fn fake_refreshing_claude(
        config_dir: &Path,
        expires_at: u64,
        sleep_seconds: u64,
        exit_code: i32,
    ) -> PathBuf {
        let executable = config_dir.join("fake-claude");
        let script = format!(
            r#"#!/bin/sh
set -eu
[ "$1 $2" = "auth login" ]
printf '%s|%s\n' "$CLAUDE_CODE_OAUTH_REFRESH_TOKEN" "$CLAUDE_CODE_OAUTH_SCOPES" >> "$CLAUDE_CONFIG_DIR/refresh.log"
sleep {sleep_seconds}
printf '%s\n' '{{"claudeAiOauth":{{"accessToken":"refreshed-access","refreshToken":"refreshed-refresh","expiresAt":{expires_at},"scopes":["user:profile","user:inference"]}}}}' > "$CLAUDE_CONFIG_DIR/{CLAUDE_CREDENTIALS_FILE}"
exit {exit_code}
"#
        );
        std::fs::write(&executable, script).expect("write fake Claude");
        let mut permissions = std::fs::metadata(&executable)
            .expect("fake Claude metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions).expect("make fake Claude executable");
        executable
    }

    #[cfg(unix)]
    fn fake_failing_claude(config_dir: &Path) -> PathBuf {
        let executable = config_dir.join("failing-claude");
        std::fs::write(&executable, "#!/bin/sh\nexit 7\n").expect("write failing Claude");
        let mut permissions = std::fs::metadata(&executable)
            .expect("failing Claude metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions).expect("make fake Claude executable");
        executable
    }

    #[cfg(target_os = "macos")]
    fn fake_keychain_security(config_dir: &Path, expires_at: u64) -> PathBuf {
        let executable = config_dir.join("fake-security");
        let script = format!(
            r#"#!/bin/sh
set -eu
[ "$1" = "find-generic-password" ]
printf '%s\n' '{{"claudeAiOauth":{{"accessToken":"keychain-access","refreshToken":"keychain-refresh","expiresAt":{expires_at},"scopes":["user:profile","user:inference"]}}}}'
"#
        );
        std::fs::write(&executable, script).expect("write fake security");
        let mut permissions = std::fs::metadata(&executable)
            .expect("fake security metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions).expect("make fake security executable");
        executable
    }

    #[cfg(target_os = "macos")]
    fn fake_security_exit(config_dir: &Path, name: &str, exit_code: i32) -> PathBuf {
        let executable = config_dir.join(name);
        std::fs::write(&executable, format!("#!/bin/sh\nexit {exit_code}\n"))
            .expect("write security fixture");
        let mut permissions = std::fs::metadata(&executable)
            .expect("security fixture metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions)
            .expect("make security fixture executable");
        executable
    }
}
