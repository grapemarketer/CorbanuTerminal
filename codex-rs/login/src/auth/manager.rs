use chrono::Utc;
use http::StatusCode;
use serde::Deserialize;
use serde::Serialize;
#[cfg(test)]
use serial_test::serial;
use std::collections::HashMap;
use std::collections::HashSet;
use std::env;
use std::fmt::Debug;
use std::future::Future;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::Semaphore;
use tokio::sync::watch;
use tracing::instrument;

use codex_agent_identity::ChatGptEnvironment;
use codex_protocol::auth::AuthMode;
use codex_protocol::config_types::ForcedLoginMethod;
use codex_protocol::config_types::ModelProviderAuthInfo;

use super::access_token::CodexAccessToken;
use super::access_token::classify_codex_access_token;
use super::agent_identity::ManagedChatGptAgentIdentityBinding;
use super::agent_identity::agent_identity_authapi_base_url;
use super::agent_identity::classify_bootstrap_error;
use super::agent_identity::record_matches_managed_chatgpt_binding;
use super::agent_identity::record_needs_task_registration;
use super::agent_identity::register_managed_chatgpt_agent_identity;
use super::agent_identity::require_agent_identity_authapi_base_url;
use super::agent_identity::verified_record_from_jwt;
use super::external_bearer::BearerTokenRefresher;
use super::external_bearer::ExternalBearerCachePolicy;
use super::revoke::revoke_auth_tokens;
use crate::auth::AuthHeaders;
pub use crate::auth::agent_identity::AgentIdentityAuth;
pub use crate::auth::agent_identity::AgentIdentityAuthError;
pub use crate::auth::bedrock_api_key::BedrockApiKeyAuth;
use crate::auth::named_chatgpt::NamedChatgptAccount;
pub use crate::auth::personal_access_token::PersonalAccessTokenAuth;
pub use crate::auth::storage::AgentIdentityAuthRecord;
pub use crate::auth::storage::AgentIdentityStorage;
pub use crate::auth::storage::AuthDotJson;
pub use crate::auth::storage::AuthKeyringBackendKind;
use crate::auth::storage::AuthStorageBackend;
use crate::auth::storage::create_auth_storage;
use crate::auth::util::try_parse_error_message;
use crate::default_client::create_client;
use crate::default_client::create_default_auth_client;
use crate::outbound_proxy::AuthRouteConfig;
use crate::token_data::TokenData;
use crate::token_data::parse_chatgpt_jwt_claims;
use crate::token_data::parse_jwt_expiration;
use codex_config::types::AuthCredentialsStoreMode;
use codex_http_client::HttpClient;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
pub use codex_model_provider_info::AMBIENT_API_KEY_ENV_VAR;
pub use codex_model_provider_info::KIMI_CODE_API_KEY_ENV_VAR;
use codex_protocol::account::PlanType as AccountPlanType;
use codex_protocol::auth::PlanType as InternalPlanType;
use codex_protocol::auth::RefreshTokenFailedError;
use codex_protocol::auth::RefreshTokenFailedReason;
use codex_protocol::protocol::SessionSource;
use serde_json::Value;
use thiserror::Error;

/// Authentication mechanism used by the current user.
#[derive(Debug, Clone)]
pub enum CodexAuth {
    ApiKey(ApiKeyAuth),
    Chatgpt(ChatgptAuth),
    ChatgptAuthTokens(ChatgptAuthTokens),
    Headers(AuthHeaders),
    AgentIdentity(AgentIdentityAuth),
    PersonalAccessToken(PersonalAccessTokenAuth),
    BedrockApiKey(BedrockApiKeyAuth),
}

/// Metadata-only location of a stored provider API key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderApiKeyStorageSource {
    EncryptedVault,
    LegacyPlaintext,
}

/// Metadata-only state of a provider API key in Corbanu-managed storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderApiKeyStorageMetadata {
    Stored { source: ProviderApiKeyStorageSource },
    Missing,
    Suppressed,
}

/// Secret-free provider-key metadata loaded with one vault-index and one legacy metadata read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderApiKeyStorageMetadataSnapshot {
    entries: HashMap<String, ProviderApiKeyStorageMetadata>,
}

impl ProviderApiKeyStorageMetadataSnapshot {
    pub fn get(&self, provider_key_id: &str) -> Option<ProviderApiKeyStorageMetadata> {
        self.entries.get(provider_key_id).copied()
    }
}

/// Metadata-only state of the primary OpenAI authentication boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiAuthMetadata {
    Missing,
    Account,
    ApiKey,
    EnvironmentApiKey,
    ExternallyManaged,
    RecoveryRequired,
    Unsupported,
}

/// Policy for resolving Agent Identity auth from a broader Codex auth snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentIdentityAuthPolicy {
    /// Use Agent Identity auth only when the current auth is already Agent Identity.
    JwtOnly,
    /// Allow managed ChatGPT auth to register or reuse Agent Identity auth.
    ChatGptAuth,
}

const AGENT_IDENTITY_BOOTSTRAP_FAILURE_COOLDOWN: Duration = Duration::from_secs(60 * 60);

#[derive(Debug)]
struct CachedAgentIdentityBootstrapFailure {
    account_id: String,
    authapi_base_url: String,
    retry_at: Instant,
    error: AgentIdentityAuthError,
}

#[derive(Debug, Default)]
struct AgentIdentityBootstrapCooldown {
    failure: Option<CachedAgentIdentityBootstrapFailure>,
}

impl AgentIdentityBootstrapCooldown {
    fn error_for(
        &mut self,
        account_id: &str,
        authapi_base_url: &str,
        now: Instant,
    ) -> Option<AgentIdentityAuthError> {
        let error = self
            .failure
            .as_ref()
            .filter(|failure| {
                failure.account_id == account_id
                    && failure.authapi_base_url == authapi_base_url
                    && failure.retry_at > now
            })
            .map(|failure| failure.error.clone());
        if error.is_none() {
            self.clear();
        }
        error
    }

    fn record_failure(
        &mut self,
        account_id: String,
        authapi_base_url: String,
        error: AgentIdentityAuthError,
        now: Instant,
    ) {
        self.failure = Some(CachedAgentIdentityBootstrapFailure {
            account_id,
            authapi_base_url,
            retry_at: now + AGENT_IDENTITY_BOOTSTRAP_FAILURE_COOLDOWN,
            error,
        });
    }

    fn clear(&mut self) {
        self.failure = None;
    }
}

impl PartialEq for CodexAuth {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Headers(a), Self::Headers(b)) => a == b,
            (Self::PersonalAccessToken(a), Self::PersonalAccessToken(b)) => a == b,
            (Self::BedrockApiKey(a), Self::BedrockApiKey(b)) => a == b,
            _ => self.api_auth_mode() == other.api_auth_mode(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ApiKeyAuth {
    api_key: String,
    /// The environment variable the key was read from, when it did not come
    /// from Corbanu's own storage.
    env_var: Option<&'static str>,
}

#[derive(Debug, Clone)]
pub struct ChatgptAuth {
    state: ChatgptAuthState,
    storage: Arc<dyn AuthStorageBackend>,
}

#[derive(Debug, Clone)]
pub struct ChatgptAuthTokens {
    state: ChatgptAuthState,
}

#[derive(Debug, Clone)]
struct ChatgptAuthState {
    auth_dot_json: Arc<Mutex<Option<AuthDotJson>>>,
    client: HttpClient,
}

const TOKEN_REFRESH_INTERVAL: i64 = 8;
const CHATGPT_ACCESS_TOKEN_REFRESH_WINDOW_MINUTES: i64 = 5;

const REFRESH_TOKEN_EXPIRED_MESSAGE: &str = "Your access token could not be refreshed because your refresh token has expired. Please log out and sign in again.";
const REFRESH_TOKEN_REUSED_MESSAGE: &str = "Your access token could not be refreshed because your refresh token was already used. Please log out and sign in again.";
const REFRESH_TOKEN_INVALIDATED_MESSAGE: &str = "Your access token could not be refreshed because your refresh token was revoked. Please log out and sign in again.";
const REFRESH_TOKEN_UNKNOWN_MESSAGE: &str =
    "Your access token could not be refreshed. Please log out and sign in again.";
const REFRESH_TOKEN_ACCOUNT_MISMATCH_MESSAGE: &str = "Your access token could not be refreshed because you have since logged out or signed in to another account. Please sign in again.";
const REFRESH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub(super) const REVOKE_TOKEN_URL: &str = "https://auth.openai.com/oauth/revoke";
pub const REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR: &str = "CODEX_REFRESH_TOKEN_URL_OVERRIDE";
pub const REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR: &str = "CODEX_REVOKE_TOKEN_URL_OVERRIDE";
pub const CLIENT_ID_OVERRIDE_ENV_VAR: &str = "CODEX_APP_SERVER_LOGIN_CLIENT_ID";
static NEXT_DUMMY_AUTH_ID: AtomicU64 = AtomicU64::new(1);
static PROVIDER_API_KEY_STORAGE_REVISION: AtomicU64 = AtomicU64::new(1);
static NEXT_PROVIDER_AUTH_TEMP_ID: AtomicU64 = AtomicU64::new(1);

/// In-process revision of stored provider keys; bumped by every write or delete.
pub fn provider_api_key_storage_revision() -> u64 {
    PROVIDER_API_KEY_STORAGE_REVISION.load(Ordering::Acquire)
}

fn mark_provider_api_key_storage_changed() {
    PROVIDER_API_KEY_STORAGE_REVISION.fetch_add(1, Ordering::AcqRel);
}

#[derive(Debug, Error)]
pub enum RefreshTokenError {
    #[error("{0}")]
    Permanent(#[from] RefreshTokenFailedError),
    #[error(transparent)]
    Transient(#[from] std::io::Error),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalAuthRefreshReason {
    Unauthorized,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalAuthRefreshContext {
    pub reason: ExternalAuthRefreshReason,
    pub previous_account_id: Option<String>,
}

/// Pluggable auth provider used by `AuthManager` for externally managed auth flows.
///
/// Implementations own the current auth value and any source-specific refresh mechanism.
pub trait ExternalAuth: Send + Sync {
    /// Returns the provider's current auth value.
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth>;

    /// Refreshes auth and makes the returned value current for future `resolve()` calls.
    fn refresh(&self, context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth>;
}

pub type ExternalAuthFuture<'a, T> = Pin<Box<dyn Future<Output = std::io::Result<T>> + Send + 'a>>;

impl RefreshTokenError {
    pub fn failed_reason(&self) -> Option<RefreshTokenFailedReason> {
        match self {
            Self::Permanent(error) => Some(error.reason),
            Self::Transient(_) => None,
        }
    }
}

impl From<RefreshTokenError> for std::io::Error {
    fn from(err: RefreshTokenError) -> Self {
        match err {
            RefreshTokenError::Permanent(failed) => std::io::Error::other(failed),
            RefreshTokenError::Transient(inner) => inner,
        }
    }
}

impl CodexAuth {
    async fn from_auth_dot_json(
        codex_home: &Path,
        auth_dot_json: AuthDotJson,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        chatgpt_base_url: Option<&str>,
        keyring_backend_kind: AuthKeyringBackendKind,
        agent_identity_authapi_base_url: Option<&str>,
        auth_route_config: &AuthRouteConfig,
    ) -> std::io::Result<Self> {
        Self::from_auth_dot_json_with_storage(
            codex_home,
            auth_dot_json,
            auth_credentials_store_mode,
            chatgpt_base_url,
            keyring_backend_kind,
            agent_identity_authapi_base_url,
            auth_route_config,
            /*storage*/ None,
        )
        .await
    }

    /// Like [`Self::from_auth_dot_json`]; `storage` (a named account's vault
    /// store, PF-84-S06) replaces the store a ChatGPT login refreshes into.
    #[allow(clippy::too_many_arguments)]
    async fn from_auth_dot_json_with_storage(
        codex_home: &Path,
        auth_dot_json: AuthDotJson,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        chatgpt_base_url: Option<&str>,
        keyring_backend_kind: AuthKeyringBackendKind,
        agent_identity_authapi_base_url: Option<&str>,
        auth_route_config: &AuthRouteConfig,
        storage: Option<Arc<dyn AuthStorageBackend>>,
    ) -> std::io::Result<Self> {
        let auth_mode = auth_dot_json.resolved_mode();
        if auth_mode == AuthMode::ApiKey {
            let Some(api_key) = auth_dot_json.openai_api_key.as_deref() else {
                return Err(std::io::Error::other("API key auth is missing a key."));
            };
            return Ok(Self::from_api_key(api_key));
        }
        if auth_mode == AuthMode::AgentIdentity {
            let Some(agent_identity) = auth_dot_json.agent_identity.clone() else {
                return Err(std::io::Error::other(
                    "agent identity auth is missing agent identity auth material.",
                ));
            };
            let base_url = chatgpt_base_url
                .unwrap_or(ChatGptEnvironment::default().chatgpt_base_url())
                .trim_end_matches('/')
                .to_string();
            let agent_identity_authapi_base_url =
                require_agent_identity_authapi_base_url(agent_identity_authapi_base_url)?;
            match agent_identity {
                AgentIdentityStorage::Jwt(jwt) => {
                    let auth = AgentIdentityAuth::from_jwt(
                        &jwt,
                        &base_url,
                        agent_identity_authapi_base_url,
                        auth_route_config,
                    )
                    .await?;
                    return Ok(Self::AgentIdentity(auth));
                }
                AgentIdentityStorage::Record(record) => {
                    let auth = AgentIdentityAuth::from_record(
                        record,
                        agent_identity_authapi_base_url,
                        auth_route_config,
                    )
                    .await?;
                    return Ok(Self::AgentIdentity(auth));
                }
            }
        }
        if auth_mode == AuthMode::PersonalAccessToken {
            let Some(personal_access_token) = auth_dot_json.personal_access_token.as_deref() else {
                return Err(std::io::Error::other(
                    "personal access token auth is missing a personal access token.",
                ));
            };
            return Self::from_personal_access_token(personal_access_token, auth_route_config)
                .await;
        }
        if auth_mode == AuthMode::BedrockApiKey {
            let Some(auth) = auth_dot_json.bedrock_api_key else {
                return Err(std::io::Error::other(
                    "Bedrock API key auth is missing a Bedrock API key.",
                ));
            };
            return Ok(Self::BedrockApiKey(auth));
        }
        if auth_mode == AuthMode::Headers {
            return Err(std::io::Error::other(
                "externally provided auth cannot be loaded from auth storage.",
            ));
        }

        let storage_mode = auth_dot_json.storage_mode(auth_credentials_store_mode);
        let client = create_default_auth_client(&refresh_token_endpoint(), auth_route_config)?;
        let state = ChatgptAuthState {
            auth_dot_json: Arc::new(Mutex::new(Some(auth_dot_json))),
            client,
        };

        match auth_mode {
            AuthMode::Chatgpt => {
                let storage = storage.unwrap_or_else(|| {
                    create_auth_storage(
                        codex_home.to_path_buf(),
                        storage_mode,
                        keyring_backend_kind,
                    )
                });
                Ok(Self::Chatgpt(ChatgptAuth { state, storage }))
            }
            AuthMode::ChatgptAuthTokens => Ok(Self::ChatgptAuthTokens(ChatgptAuthTokens { state })),
            AuthMode::ApiKey => unreachable!("api key mode is handled above"),
            AuthMode::Headers => {
                unreachable!("externally provided auth is never loaded from auth storage")
            }
            AuthMode::AgentIdentity => unreachable!("agent identity mode is handled above"),
            AuthMode::PersonalAccessToken => {
                unreachable!("personal access token mode is handled above")
            }
            AuthMode::BedrockApiKey => unreachable!("bedrock api key mode is handled above"),
        }
    }

    pub async fn from_auth_storage(
        codex_home: &Path,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        chatgpt_base_url: Option<&str>,
        keyring_backend_kind: AuthKeyringBackendKind,
        auth_route_config: &AuthRouteConfig,
    ) -> std::io::Result<Option<Self>> {
        let agent_identity_authapi_base_url =
            agent_identity_authapi_base_url(chatgpt_base_url).ok();
        load_auth(
            codex_home,
            /*enable_codex_api_key_env*/ false,
            auth_credentials_store_mode,
            /*forced_chatgpt_workspace_id*/ None,
            chatgpt_base_url,
            keyring_backend_kind,
            agent_identity_authapi_base_url.as_deref(),
            auth_route_config,
        )
        .await
    }

    pub async fn from_agent_identity_jwt(
        jwt: &str,
        chatgpt_base_url: Option<&str>,
        auth_route_config: &AuthRouteConfig,
    ) -> std::io::Result<Self> {
        let agent_identity_authapi_base_url = agent_identity_authapi_base_url(chatgpt_base_url)?;
        Self::from_agent_identity_jwt_with_authapi_base_url(
            jwt,
            chatgpt_base_url,
            &agent_identity_authapi_base_url,
            auth_route_config,
        )
        .await
    }

    async fn from_agent_identity_jwt_with_authapi_base_url(
        jwt: &str,
        chatgpt_base_url: Option<&str>,
        agent_identity_authapi_base_url: &str,
        auth_route_config: &AuthRouteConfig,
    ) -> std::io::Result<Self> {
        let base_url = chatgpt_base_url
            .unwrap_or(ChatGptEnvironment::default().chatgpt_base_url())
            .trim_end_matches('/')
            .to_string();
        Ok(Self::AgentIdentity(
            AgentIdentityAuth::from_jwt(
                jwt,
                &base_url,
                agent_identity_authapi_base_url,
                auth_route_config,
            )
            .await?,
        ))
    }

    pub async fn from_personal_access_token(
        access_token: &str,
        auth_route_config: &AuthRouteConfig,
    ) -> std::io::Result<Self> {
        Ok(Self::PersonalAccessToken(
            PersonalAccessTokenAuth::load(access_token, auth_route_config).await?,
        ))
    }

    /// Returns the effective backend auth mode.
    ///
    /// Externally managed ChatGPT tokens are normalized to [`AuthMode::Chatgpt`].
    pub fn auth_mode(&self) -> AuthMode {
        match self {
            Self::ApiKey(_) => AuthMode::ApiKey,
            Self::Chatgpt(_) | Self::ChatgptAuthTokens(_) => AuthMode::Chatgpt,
            Self::Headers(_) => AuthMode::Headers,
            Self::AgentIdentity(_) => AuthMode::AgentIdentity,
            Self::PersonalAccessToken(_) => AuthMode::PersonalAccessToken,
            Self::BedrockApiKey(_) => AuthMode::BedrockApiKey,
        }
    }

    /// Returns the precise kind of credentials backing this authentication.
    pub fn api_auth_mode(&self) -> AuthMode {
        match self {
            Self::ApiKey(_) => AuthMode::ApiKey,
            Self::Chatgpt(_) => AuthMode::Chatgpt,
            Self::ChatgptAuthTokens(_) => AuthMode::ChatgptAuthTokens,
            Self::Headers(_) => AuthMode::Headers,
            Self::AgentIdentity(_) => AuthMode::AgentIdentity,
            Self::PersonalAccessToken(_) => AuthMode::PersonalAccessToken,
            Self::BedrockApiKey(_) => AuthMode::BedrockApiKey,
        }
    }

    pub fn is_api_key_auth(&self) -> bool {
        self.auth_mode() == AuthMode::ApiKey
    }

    pub fn is_personal_access_token_auth(&self) -> bool {
        self.auth_mode() == AuthMode::PersonalAccessToken
    }

    pub fn is_chatgpt_auth(&self) -> bool {
        self.api_auth_mode().has_chatgpt_account()
    }

    pub fn uses_codex_backend(&self) -> bool {
        self.api_auth_mode().uses_codex_backend()
    }

    pub fn is_external_chatgpt_tokens(&self) -> bool {
        matches!(self, Self::ChatgptAuthTokens(_))
    }

    fn supports_unauthorized_recovery(&self) -> bool {
        matches!(
            self,
            Self::Chatgpt(_) | Self::ChatgptAuthTokens(_) | Self::Headers(_)
        )
    }

    /// Returns `None` if `auth_mode() != AuthMode::ApiKey`.
    pub fn api_key(&self) -> Option<&str> {
        match self {
            Self::ApiKey(auth) => Some(auth.api_key.as_str()),
            Self::Chatgpt(_)
            | Self::ChatgptAuthTokens(_)
            | Self::Headers(_)
            | Self::AgentIdentity(_)
            | Self::PersonalAccessToken(_)
            | Self::BedrockApiKey(_) => None,
        }
    }

    /// Returns `Err` if token-backed ChatGPT auth is unavailable.
    pub fn get_token_data(&self) -> Result<TokenData, std::io::Error> {
        let auth_dot_json: Option<AuthDotJson> = self.get_current_auth_json();
        match auth_dot_json {
            Some(AuthDotJson {
                tokens: Some(tokens),
                last_refresh: Some(_),
                ..
            }) => Ok(tokens),
            _ => Err(std::io::Error::other("Token data is not available.")),
        }
    }

    /// Returns the token string used for bearer authentication.
    pub fn get_token(&self) -> Result<String, std::io::Error> {
        match self {
            Self::ApiKey(auth) => Ok(auth.api_key.clone()),
            Self::Chatgpt(_) | Self::ChatgptAuthTokens(_) => {
                let access_token = self.get_token_data()?.access_token;
                Ok(access_token)
            }
            Self::AgentIdentity(_) => Err(std::io::Error::other(
                "agent identity auth does not expose a bearer token",
            )),
            Self::Headers(_) => Err(std::io::Error::other(
                "header auth does not expose a bearer token",
            )),
            Self::PersonalAccessToken(auth) => Ok(auth.access_token().to_string()),
            Self::BedrockApiKey(_) => Err(std::io::Error::other(
                "Bedrock API key auth does not expose a Codex bearer token",
            )),
        }
    }

    /// Returns `None` if Codex backend auth does not expose an account id.
    pub fn get_account_id(&self) -> Option<String> {
        match self {
            Self::Headers(_) => None,
            Self::AgentIdentity(auth) => Some(auth.account_id().to_string()),
            Self::PersonalAccessToken(auth) => Some(auth.account_id().to_string()),
            _ => self.get_current_token_data().and_then(|t| t.account_id),
        }
    }

    /// Returns false if Codex backend auth omits the FedRAMP claim.
    pub fn is_fedramp_account(&self) -> bool {
        match self {
            Self::Headers(_) => false,
            Self::AgentIdentity(auth) => auth.is_fedramp_account(),
            Self::PersonalAccessToken(auth) => auth.is_fedramp_account(),
            _ => self
                .get_current_token_data()
                .is_some_and(|t| t.id_token.is_fedramp_account()),
        }
    }

    /// Returns `None` if Codex backend auth does not expose an account email.
    pub fn get_account_email(&self) -> Option<String> {
        match self {
            Self::Headers(_) => None,
            Self::AgentIdentity(auth) => auth.email().map(str::to_string),
            Self::PersonalAccessToken(auth) => auth.email().map(str::to_string),
            _ => self.get_current_token_data().and_then(|t| t.id_token.email),
        }
    }

    /// Returns `None` if Codex backend auth does not expose a ChatGPT user id.
    pub fn get_chatgpt_user_id(&self) -> Option<String> {
        match self {
            Self::Headers(_) => None,
            Self::AgentIdentity(auth) => Some(auth.chatgpt_user_id().to_string()),
            Self::PersonalAccessToken(auth) => Some(auth.chatgpt_user_id().to_string()),
            _ => self
                .get_current_token_data()
                .and_then(|t| t.id_token.chatgpt_user_id),
        }
    }

    /// Account-facing plan classification derived from the current auth.
    /// Returns a high-level `AccountPlanType` (e.g., Free/Plus/Pro/Team/…)
    /// for UI or product decisions based on the user's subscription.
    pub fn account_plan_type(&self) -> Option<AccountPlanType> {
        if matches!(self, Self::Headers(_)) {
            return None;
        }
        if let Self::AgentIdentity(auth) = self {
            return Some(auth.plan_type());
        }
        if let Self::PersonalAccessToken(auth) = self {
            return Some(auth.plan_type());
        }

        self.get_current_token_data().map(|t| {
            t.id_token
                .chatgpt_plan_type
                .map(AccountPlanType::from)
                .unwrap_or(AccountPlanType::Unknown)
        })
    }

    pub fn is_workspace_account(&self) -> bool {
        self.account_plan_type()
            .is_some_and(AccountPlanType::is_workspace_account)
    }

    /// Returns `None` if token-backed ChatGPT auth is unavailable.
    fn get_current_auth_json(&self) -> Option<AuthDotJson> {
        let state = match self {
            Self::Chatgpt(auth) => &auth.state,
            Self::ChatgptAuthTokens(auth) => &auth.state,
            Self::ApiKey(_)
            | Self::Headers(_)
            | Self::AgentIdentity(_)
            | Self::PersonalAccessToken(_)
            | Self::BedrockApiKey(_) => return None,
        };
        #[expect(clippy::unwrap_used)]
        state.auth_dot_json.lock().unwrap().clone()
    }

    /// Returns `None` if token-backed ChatGPT auth is unavailable.
    fn get_current_token_data(&self) -> Option<TokenData> {
        self.get_current_auth_json().and_then(|t| t.tokens)
    }

    fn stored_managed_chatgpt_agent_identity_record(
        &self,
        account_id: &str,
    ) -> Option<AgentIdentityAuthRecord> {
        self.get_current_auth_json()
            .and_then(|auth| auth.agent_identity)
            .and_then(|identity| identity.as_record().cloned())
            .filter(|identity| identity.account_id == account_id)
    }

    fn persist_managed_chatgpt_agent_identity_record(
        &self,
        record: AgentIdentityAuthRecord,
    ) -> std::io::Result<()> {
        if let Self::Chatgpt(chatgpt_auth) = self {
            chatgpt_auth.persist_agent_identity_record(record)?;
        }
        Ok(())
    }

    async fn agent_identity_auth(
        &self,
        policy: AgentIdentityAuthPolicy,
        agent_identity_authapi_base_url: Option<&str>,
        forced_chatgpt_workspace_id: Option<Vec<String>>,
        auth_route_config: &AuthRouteConfig,
        session_source: SessionSource,
    ) -> std::io::Result<Option<AgentIdentityAuth>> {
        match self {
            Self::AgentIdentity(auth) => Ok(Some(auth.clone())),
            Self::ApiKey(_)
            | Self::ChatgptAuthTokens(_)
            | Self::Headers(_)
            | Self::PersonalAccessToken(_)
            | Self::BedrockApiKey(_) => Ok(None),
            Self::Chatgpt(_) => {
                if policy == AgentIdentityAuthPolicy::JwtOnly {
                    return Ok(None);
                }
                self.ensure_managed_chatgpt_agent_identity(
                    require_agent_identity_authapi_base_url(agent_identity_authapi_base_url)?,
                    forced_chatgpt_workspace_id,
                    auth_route_config,
                    session_source,
                )
                .await
                .map(Some)
            }
        }
    }

    async fn ensure_managed_chatgpt_agent_identity(
        &self,
        agent_identity_authapi_base_url: &str,
        forced_chatgpt_workspace_id: Option<Vec<String>>,
        auth_route_config: &AuthRouteConfig,
        session_source: SessionSource,
    ) -> std::io::Result<AgentIdentityAuth> {
        let binding =
            ManagedChatGptAgentIdentityBinding::from_auth(self, forced_chatgpt_workspace_id)
                .ok_or_else(|| std::io::Error::other("ChatGPT auth is unavailable"))?;

        // JWT auth is loaded as CodexAuth::AgentIdentity; this path only reuses
        // records created by the managed ChatGPT Agent Identity bootstrap.
        if let Some(record) = self.stored_managed_chatgpt_agent_identity_record(&binding.account_id)
            && record_matches_managed_chatgpt_binding(&record, &binding)
        {
            let should_persist = record_needs_task_registration(&record);
            let auth = AgentIdentityAuth::from_record(
                record,
                agent_identity_authapi_base_url,
                auth_route_config,
            )
            .await
            .map_err(|err| classify_bootstrap_error("agent task registration", err))?;
            if should_persist {
                self.persist_managed_chatgpt_agent_identity_record(auth.record().clone())?;
            }
            return Ok(auth);
        }

        let auth = register_managed_chatgpt_agent_identity(
            binding,
            agent_identity_authapi_base_url,
            session_source,
            auth_route_config,
        )
        .await?;
        self.persist_managed_chatgpt_agent_identity_record(auth.record().clone())?;
        Ok(auth)
    }

    /// Consider this private to integration tests.
    pub fn create_dummy_chatgpt_auth_for_testing() -> Self {
        let auth_dot_json = AuthDotJson {
            auth_mode: Some(AuthMode::Chatgpt),
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: Default::default(),
                access_token: "Access Token".to_string(),
                refresh_token: "test".to_string(),
                account_id: Some("account_id".to_string()),
            }),
            last_refresh: Some(Utc::now()),
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
        };

        let state = ChatgptAuthState {
            auth_dot_json: Arc::new(Mutex::new(Some(auth_dot_json))),
            client: create_client(),
        };
        let dummy_auth_id = NEXT_DUMMY_AUTH_ID.fetch_add(1, Ordering::Relaxed);
        let storage = create_auth_storage(
            PathBuf::from(format!("dummy-chatgpt-auth-{dummy_auth_id}")),
            AuthCredentialsStoreMode::Ephemeral,
            AuthKeyringBackendKind::default(),
        );
        Self::Chatgpt(ChatgptAuth { state, storage })
    }

    /// Constructs in-memory ChatGPT auth from externally managed tokens.
    pub fn from_external_chatgpt_tokens(
        access_token: &str,
        chatgpt_account_id: &str,
        chatgpt_plan_type: Option<&str>,
    ) -> std::io::Result<Self> {
        let auth_dot_json = AuthDotJson::from_external_access_token(
            access_token,
            chatgpt_account_id,
            chatgpt_plan_type,
        )?;
        let state = ChatgptAuthState {
            auth_dot_json: Arc::new(Mutex::new(Some(auth_dot_json))),
            client: create_client(),
        };
        Ok(Self::ChatgptAuthTokens(ChatgptAuthTokens { state }))
    }

    pub fn from_api_key(api_key: &str) -> Self {
        Self::ApiKey(ApiKeyAuth {
            api_key: api_key.to_owned(),
            env_var: None,
        })
    }

    /// An API key read from `env_var` rather than from stored credentials.
    pub fn from_api_key_env(api_key: &str, env_var: &'static str) -> Self {
        Self::ApiKey(ApiKeyAuth {
            api_key: api_key.to_owned(),
            env_var: Some(env_var),
        })
    }

    /// The environment variable an API key login was read from, if any.
    pub fn api_key_env_var(&self) -> Option<&'static str> {
        match self {
            Self::ApiKey(auth) => auth.env_var,
            _ => None,
        }
    }

    /// Whether this is the ambient `OPENAI_API_KEY` fallback. Model providers
    /// send it only on the first-party OpenAI auth path.
    pub fn is_openai_api_key_env_fallback(&self) -> bool {
        self.api_key_env_var() == Some(OPENAI_API_KEY_ENV_VAR)
    }
}

impl ManagedChatGptAgentIdentityBinding {
    fn from_auth(auth: &CodexAuth, forced_workspace_id: Option<Vec<String>>) -> Option<Self> {
        if !auth.is_chatgpt_auth() {
            return None;
        }

        let token_data = auth.get_token_data().ok()?;
        let forced_workspace_id =
            forced_workspace_id
                .as_deref()
                .and_then(|workspace_ids| match workspace_ids {
                    [workspace_id] if !workspace_id.is_empty() => Some(workspace_id.clone()),
                    _ => None,
                });
        let account_id = forced_workspace_id
            .or(token_data
                .account_id
                .clone()
                .filter(|value| !value.is_empty()))
            .or(token_data.id_token.chatgpt_account_id.clone())?;
        let chatgpt_user_id = token_data
            .id_token
            .chatgpt_user_id
            .clone()
            .filter(|value| !value.is_empty())?;

        Some(Self {
            account_id,
            chatgpt_user_id,
            email: token_data.id_token.email.clone(),
            plan_type: auth.account_plan_type().unwrap_or(AccountPlanType::Unknown),
            chatgpt_account_is_fedramp: auth.is_fedramp_account(),
            access_token: token_data.access_token,
        })
    }
}

impl ChatgptAuth {
    fn current_auth_json(&self) -> Option<AuthDotJson> {
        #[expect(clippy::unwrap_used)]
        self.state.auth_dot_json.lock().unwrap().clone()
    }

    fn current_token_data(&self) -> Option<TokenData> {
        self.current_auth_json().and_then(|auth| auth.tokens)
    }

    fn storage(&self) -> &Arc<dyn AuthStorageBackend> {
        &self.storage
    }

    fn client(&self) -> &HttpClient {
        &self.state.client
    }

    fn persist_agent_identity_record(
        &self,
        record: AgentIdentityAuthRecord,
    ) -> std::io::Result<()> {
        persist_agent_identity_record(&self.state.auth_dot_json, &self.storage, record)
    }
}

fn persist_agent_identity_record(
    auth_dot_json: &Arc<Mutex<Option<AuthDotJson>>>,
    storage: &Arc<dyn AuthStorageBackend>,
    record: AgentIdentityAuthRecord,
) -> std::io::Result<()> {
    let mut guard = auth_dot_json
        .lock()
        .map_err(|_| std::io::Error::other("failed to lock auth state"))?;
    // D171 (#1311): a named account's record (and its private key) is written
    // only onto the stored sign-in of the ChatGPT account and user it was made
    // for, in one vault step, and never brings back an account another process
    // removed.
    let named = storage.is_named_account();
    let updated = storage.update(&mut |auth| {
        let stored_binding = auth.tokens.as_ref().map(|tokens| {
            (
                tokens.account_id.as_deref(),
                tokens.id_token.chatgpt_user_id.as_deref(),
            )
        });
        let record_binding = (
            Some(record.account_id.as_str()),
            Some(record.chatgpt_user_id.as_str()),
        );
        if named && stored_binding != Some(record_binding) {
            return Err(std::io::Error::other(
                "the named account's sign-in changed; its agent identity was not saved",
            ));
        }
        auth.agent_identity = Some(AgentIdentityStorage::Record(record.clone()));
        Ok(())
    })?;
    let auth = match updated {
        Some(auth) => auth,
        None if named => {
            return Err(std::io::Error::other(
                "the named account has no sign-in (it may have been removed or signed out); \
                 its agent identity was not saved",
            ));
        }
        None => {
            let mut auth = guard
                .clone()
                .ok_or_else(|| std::io::Error::other("auth data is not available"))?;
            auth.agent_identity = Some(AgentIdentityStorage::Record(record));
            storage.save(&auth)?;
            auth
        }
    };
    *guard = Some(auth);
    Ok(())
}

pub const OPENAI_API_KEY_ENV_VAR: &str = "OPENAI_API_KEY";
pub const CODEX_API_KEY_ENV_VAR: &str = "CODEX_API_KEY";
pub const CODEX_ACCESS_TOKEN_ENV_VAR: &str = "CODEX_ACCESS_TOKEN";

/// Shown once per session when a request is authorised by the `OPENAI_API_KEY`
/// fallback, and again whenever the credential in use switches back to it.
pub const OPENAI_API_KEY_ENV_FALLBACK_NOTICE: &str = "Using OPENAI_API_KEY from your \
    environment — billed per use to that API key. Sign in or unset it to change.";

/// Whether `OPENAI_API_KEY` is the last-resort OpenAI credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenAiApiKeyEnv {
    /// Used only when no external or stored OpenAI credential exists.
    Fallback,
    /// Not read: storage-only loads, and configs that require ChatGPT login.
    Ignore,
}

impl OpenAiApiKeyEnv {
    fn for_forced_login_method(forced_login_method: Option<ForcedLoginMethod>) -> Self {
        if forced_login_method == Some(ForcedLoginMethod::Chatgpt) {
            Self::Ignore
        } else {
            Self::Fallback
        }
    }
}

pub fn read_openai_api_key_from_env() -> Option<String> {
    env::var(OPENAI_API_KEY_ENV_VAR)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn read_ambient_api_key_from_env() -> Option<String> {
    read_non_empty_env_var(AMBIENT_API_KEY_ENV_VAR)
}

pub fn read_codex_api_key_from_env() -> Option<String> {
    read_non_empty_env_var(CODEX_API_KEY_ENV_VAR)
}

pub fn read_codex_access_token_from_env() -> Option<String> {
    read_non_empty_env_var(CODEX_ACCESS_TOKEN_ENV_VAR)
}

fn auth_uses_api_key_from_env(auth: &CodexAuth) -> bool {
    if auth.api_key_env_var().is_some() {
        return true;
    }
    let Some(api_key) = auth.api_key() else {
        return false;
    };

    read_codex_api_key_from_env().as_deref() == Some(api_key)
        || read_ambient_api_key_from_env().as_deref() == Some(api_key)
}

fn read_non_empty_env_var(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Delete the auth.json file inside `codex_home` if it exists. Returns `Ok(true)`
/// if a file was removed, `Ok(false)` if no auth file was present.
pub fn logout(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<bool> {
    let storage = create_auth_storage(
        codex_home.to_path_buf(),
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    storage.delete()
}

/// Delete Codex auth plus provider credentials from legacy storage and the vault.
///
/// This is intentionally separate from `logout`: normal account logout must not erase
/// provider API keys that the user stored under `/providers`.
pub fn logout_all_credentials(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<bool> {
    let removed_auth = logout(
        codex_home,
        auth_credentials_store_mode,
        keyring_backend_kind,
    )?;
    let removed_provider = delete_provider_auth_if_exists(codex_home)?;
    // Also wipe any provider keys stored in the encrypted vault (best-effort: a missing
    // keyring in CI must not fail logout).
    let removed_vault_provider =
        super::provider_key_vault::delete_all_provider_keys(codex_home).unwrap_or(false);
    if removed_provider || removed_vault_provider {
        mark_provider_api_key_storage_changed();
    }
    Ok(removed_auth || removed_provider || removed_vault_provider)
}

pub async fn logout_with_revoke(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
    auth_route_config: &AuthRouteConfig,
) -> std::io::Result<bool> {
    logout_with_revoke_common(
        codex_home,
        auth_credentials_store_mode,
        keyring_backend_kind,
        /*include_provider_credentials*/ false,
        auth_route_config,
    )
    .await
}

pub async fn logout_with_revoke_all_credentials(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
    auth_route_config: &AuthRouteConfig,
) -> std::io::Result<bool> {
    logout_with_revoke_common(
        codex_home,
        auth_credentials_store_mode,
        keyring_backend_kind,
        /*include_provider_credentials*/ true,
        auth_route_config,
    )
    .await
}

async fn logout_with_revoke_common(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
    include_provider_credentials: bool,
    auth_route_config: &AuthRouteConfig,
) -> std::io::Result<bool> {
    let auth_dot_json = match load_auth_dot_json(
        codex_home,
        auth_credentials_store_mode,
        keyring_backend_kind,
    ) {
        Ok(auth_dot_json) => auth_dot_json,
        Err(err) => {
            tracing::warn!("failed to load stored auth during logout: {err}");
            None
        }
    };
    if let Err(err) = revoke_auth_tokens(auth_dot_json.as_ref(), auth_route_config).await {
        tracing::warn!("failed to revoke auth tokens during logout: {err}");
    }
    logout_all_stores(
        codex_home,
        auth_credentials_store_mode,
        keyring_backend_kind,
        include_provider_credentials,
    )
}

/// Writes an `auth.json` that contains only the API key.
pub fn login_with_api_key(
    codex_home: &Path,
    api_key: &str,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<()> {
    let auth_dot_json = AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some(api_key.to_string()),
        tokens: None,
        last_refresh: None,
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
    };
    save_auth(
        codex_home,
        &auth_dot_json,
        auth_credentials_store_mode,
        keyring_backend_kind,
    )
}

pub fn login_with_provider_api_key(
    codex_home: &Path,
    provider_key_id: &str,
    api_key: &str,
    _auth_credentials_store_mode: AuthCredentialsStoreMode,
    _keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<()> {
    let provider_key_id = provider_key_id.trim();
    if provider_key_id.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "provider key id cannot be empty",
        ));
    }

    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "provider API key cannot be empty",
        ));
    }

    save_provider_api_key(codex_home, provider_key_id, api_key)?;
    mark_provider_api_key_storage_changed();
    Ok(())
}

/// Return the provider key identifier represented by a namespaced vault label.
///
/// Provider keys are normalized to lowercase in the vault, while the auth manager indexes them
/// by their environment-style uppercase identifier. Keeping the reverse mapping here ensures
/// generic `/vault` mutations use the same cache-invalidation boundary as `/providers`.
pub fn provider_api_key_id_from_vault_label(label: &str) -> Option<String> {
    let provider_key_id = label
        .trim()
        .strip_prefix(super::provider_key_vault::PROVIDER_LABEL_PREFIX)?;
    if provider_key_id.is_empty()
        || !provider_key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return None;
    }
    Some(provider_key_id.to_ascii_uppercase())
}

/// Remove one provider API key from both encrypted and legacy storage.
///
/// This is intentionally narrower than logout: account authentication and every other provider
/// credential remain untouched.
pub fn delete_provider_api_key(codex_home: &Path, provider_key_id: &str) -> std::io::Result<bool> {
    let provider_key_id = provider_key_id.trim();
    if provider_key_id.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "provider key id cannot be empty",
        ));
    }
    let removed_vault = super::provider_key_vault::delete_provider_key(codex_home, provider_key_id);
    let removed_legacy = legacy_delete_provider_key(codex_home, provider_key_id)?;
    let removed = match removed_vault {
        Ok(removed_vault) => {
            legacy_clear_provider_key_tombstone(codex_home, provider_key_id)?;
            removed_vault || removed_legacy
        }
        Err(error) => {
            // An encrypted vault can become temporarily unavailable (for example after a
            // copied CODEX_HOME loses its keyring binding). Persist a non-secret tombstone so
            // the inaccessible copy cannot silently become active again if the vault later
            // recovers. A subsequent explicit login clears the tombstone.
            tracing::warn!(
                provider_key_id,
                %error,
                "provider vault was unavailable during credential deletion; suppressing the stale vault entry"
            );
            legacy_mark_provider_key_deleted(codex_home, provider_key_id)? || removed_legacy
        }
    };
    if removed {
        mark_provider_api_key_storage_changed();
    }
    Ok(removed)
}

/// Immediately suppress one stored provider key without waiting for encrypted-vault cleanup.
///
/// This durable tombstone is useful at destructive UI boundaries: readers stop returning the
/// credential as soon as this function succeeds, while the potentially slower encrypted-vault
/// deletion can finish in the background. A later explicit provider login clears the tombstone.
pub fn suppress_provider_api_key(
    codex_home: &Path,
    provider_key_id: &str,
) -> std::io::Result<bool> {
    let provider_key_id = provider_key_id.trim();
    if provider_key_id.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "provider key id cannot be empty",
        ));
    }
    let inserted = legacy_mark_provider_key_deleted(codex_home, provider_key_id)?;
    if inserted {
        mark_provider_api_key_storage_changed();
    }
    Ok(inserted)
}

/// Writes an `auth.json` that contains only the access token.
pub async fn login_with_access_token(
    codex_home: &Path,
    access_token: &str,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    forced_chatgpt_workspace_id: Option<&[String]>,
    chatgpt_base_url: Option<&str>,
    keyring_backend_kind: AuthKeyringBackendKind,
    auth_route_config: &AuthRouteConfig,
) -> std::io::Result<()> {
    let auth_dot_json = match classify_codex_access_token(access_token) {
        CodexAccessToken::PersonalAccessToken(access_token) => {
            let auth = PersonalAccessTokenAuth::load(access_token, auth_route_config).await?;
            ensure_personal_access_token_workspace_allowed(forced_chatgpt_workspace_id, &auth)?;
            AuthDotJson {
                // Infer PAT auth from the credential field so older Codex builds can still
                // deserialize auth.json after a rollback.
                auth_mode: None,
                openai_api_key: None,
                tokens: None,
                last_refresh: None,
                agent_identity: None,
                personal_access_token: Some(access_token.to_string()),
                bedrock_api_key: None,
            }
        }
        CodexAccessToken::AgentIdentityJwt(jwt) => {
            let base_url = chatgpt_base_url
                .unwrap_or(ChatGptEnvironment::default().chatgpt_base_url())
                .trim_end_matches('/')
                .to_string();
            verified_record_from_jwt(jwt, &base_url, auth_route_config).await?;
            AuthDotJson {
                auth_mode: Some(AuthMode::AgentIdentity),
                openai_api_key: None,
                tokens: None,
                last_refresh: None,
                agent_identity: Some(AgentIdentityStorage::Jwt(jwt.to_string())),
                personal_access_token: None,
                bedrock_api_key: None,
            }
        }
    };
    save_auth(
        codex_home,
        &auth_dot_json,
        auth_credentials_store_mode,
        keyring_backend_kind,
    )
}

fn ensure_personal_access_token_workspace_allowed(
    expected_workspace_ids: Option<&[String]>,
    auth: &PersonalAccessTokenAuth,
) -> std::io::Result<()> {
    crate::server::ensure_workspace_account_allowed(expected_workspace_ids, auth.account_id())
        .map_err(|message| std::io::Error::new(std::io::ErrorKind::PermissionDenied, message))
}

/// Writes an in-memory auth payload for externally managed ChatGPT tokens.
pub fn login_with_chatgpt_auth_tokens(
    codex_home: &Path,
    access_token: &str,
    chatgpt_account_id: &str,
    chatgpt_plan_type: Option<&str>,
) -> std::io::Result<()> {
    let auth_dot_json = AuthDotJson::from_external_access_token(
        access_token,
        chatgpt_account_id,
        chatgpt_plan_type,
    )?;
    save_auth(
        codex_home,
        &auth_dot_json,
        AuthCredentialsStoreMode::Ephemeral,
        AuthKeyringBackendKind::default(),
    )
}

/// Persist the provided auth payload using the specified backend.
pub fn save_auth(
    codex_home: &Path,
    auth: &AuthDotJson,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<()> {
    let storage = create_auth_storage(
        codex_home.to_path_buf(),
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    storage.save(auth)
}

/// Load the raw stored auth payload without applying environment overrides.
///
/// Returns `None` when no credentials are stored. Prefer `AuthManager` for
/// ordinary production reads; this helper is for tests and write-side
/// maintenance that must inspect the exact payload in storage.
pub fn load_auth_dot_json(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<Option<AuthDotJson>> {
    let storage = create_auth_storage(
        codex_home.to_path_buf(),
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    storage.load()
}

pub fn provider_api_key_from_auth_storage(
    codex_home: &Path,
    provider_key_id: &str,
    _auth_credentials_store_mode: AuthCredentialsStoreMode,
    _keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<Option<String>> {
    if legacy_provider_key_is_deleted(codex_home, provider_key_id)? {
        return Ok(None);
    }
    // Prefer the encrypted vault; fall back to the legacy plaintext store for compatibility.
    if let Some(key) = super::provider_key_vault::read_provider_key(codex_home, provider_key_id)? {
        return Ok(Some(key));
    }
    legacy_provider_key(codex_home, provider_key_id)
}

/// Classify the configured OpenAI credential without exposing its secret or
/// handing callers a long-lived [`AuthManager`].
pub async fn openai_auth_metadata_from_config(
    config: &impl AuthManagerConfig,
    enable_codex_api_key_env: bool,
) -> OpenAiAuthMetadata {
    AuthManager::shared_from_config(config, enable_codex_api_key_env)
        .await
        .openai_auth_metadata()
}

/// Inspect provider-key storage without revealing or decrypting the credential.
pub fn provider_api_key_metadata_from_auth_storage(
    codex_home: &Path,
    provider_key_id: &str,
) -> std::io::Result<ProviderApiKeyStorageMetadata> {
    if legacy_provider_key_is_deleted(codex_home, provider_key_id)? {
        return Ok(ProviderApiKeyStorageMetadata::Suppressed);
    }
    super::provider_key_vault::provider_key_metadata(codex_home, provider_key_id)
}

/// Inspect several provider keys without repeatedly opening the encrypted vault or legacy file.
///
/// Tombstones retain precedence over encrypted and legacy storage, matching the single-key API.
/// Only storage metadata is loaded; credential values are never read.
pub fn provider_api_key_metadata_snapshot_from_auth_storage(
    codex_home: &Path,
    provider_key_ids: impl IntoIterator<Item = impl AsRef<str>>,
) -> std::io::Result<ProviderApiKeyStorageMetadataSnapshot> {
    let provider_key_ids = provider_key_ids
        .into_iter()
        .map(|provider_key_id| provider_key_id.as_ref().to_string())
        .collect::<Vec<_>>();
    if provider_key_ids.is_empty() {
        return Ok(ProviderApiKeyStorageMetadataSnapshot {
            entries: HashMap::new(),
        });
    }
    let legacy = load_provider_auth(codex_home)?;
    let vault_ids = super::provider_key_vault::stored_provider_key_ids(codex_home);
    let entries = provider_key_ids
        .into_iter()
        .map(|provider_key_id| {
            let metadata = if legacy.deleted_api_keys.contains(&provider_key_id) {
                ProviderApiKeyStorageMetadata::Suppressed
            } else if vault_ids.contains(&provider_key_id.trim().to_ascii_uppercase()) {
                ProviderApiKeyStorageMetadata::Stored {
                    source: ProviderApiKeyStorageSource::EncryptedVault,
                }
            } else if legacy
                .api_keys
                .get(&provider_key_id)
                .is_some_and(|value| !value.trim().is_empty())
            {
                ProviderApiKeyStorageMetadata::Stored {
                    source: ProviderApiKeyStorageSource::LegacyPlaintext,
                }
            } else {
                ProviderApiKeyStorageMetadata::Missing
            };
            (provider_key_id, metadata)
        })
        .collect();
    Ok(ProviderApiKeyStorageMetadataSnapshot { entries })
}

/// Read a provider key from the legacy plaintext `provider_auth.json` store only.
pub(super) fn legacy_provider_key(
    codex_home: &Path,
    provider_key_id: &str,
) -> std::io::Result<Option<String>> {
    Ok(load_provider_auth(codex_home)?
        .api_keys
        .get(provider_key_id)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string))
}

pub(super) fn legacy_provider_key_is_present(
    codex_home: &Path,
    provider_key_id: &str,
) -> std::io::Result<bool> {
    Ok(load_provider_auth(codex_home)?
        .api_keys
        .get(provider_key_id)
        .is_some_and(|value| !value.trim().is_empty()))
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct ProviderAuthDotJson {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    api_keys: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    deleted_api_keys: HashSet<String>,
}

fn provider_auth_file(codex_home: &Path) -> PathBuf {
    codex_home.join("provider_auth.json")
}

fn delete_provider_auth_if_exists(codex_home: &Path) -> std::io::Result<bool> {
    match std::fs::remove_file(provider_auth_file(codex_home)) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

fn load_provider_auth(codex_home: &Path) -> std::io::Result<ProviderAuthDotJson> {
    let provider_auth_file = provider_auth_file(codex_home);
    let contents = match std::fs::read_to_string(provider_auth_file) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProviderAuthDotJson::default());
        }
        Err(err) => return Err(err),
    };

    serde_json::from_str(&contents).map_err(std::io::Error::other)
}

fn save_provider_auth(
    codex_home: &Path,
    provider_auth: &ProviderAuthDotJson,
) -> std::io::Result<()> {
    let provider_auth_file = provider_auth_file(codex_home);
    if let Some(parent) = provider_auth_file.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut json_data = serde_json::to_string_pretty(provider_auth)?;
    json_data.push('\n');
    let temporary = provider_auth_file.with_file_name(format!(
        ".provider_auth.json.{}.{}.tmp",
        std::process::id(),
        NEXT_PROVIDER_AUTH_TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(json_data.as_bytes())?;
    file.sync_all()?;
    if let Err(error) = std::fs::rename(&temporary, &provider_auth_file) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    #[cfg(unix)]
    if let Some(parent) = provider_auth_file.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn save_provider_api_key(
    codex_home: &Path,
    provider_key_id: &str,
    api_key: &str,
) -> std::io::Result<()> {
    legacy_clear_provider_key_tombstone(codex_home, provider_key_id)?;
    super::provider_key_vault::write_provider_key(codex_home, provider_key_id, api_key)
}

/// Write a provider key to the legacy plaintext `provider_auth.json` store only.
pub(super) fn legacy_save_provider_key(
    codex_home: &Path,
    provider_key_id: &str,
    api_key: &str,
) -> std::io::Result<()> {
    let mut provider_auth = load_provider_auth(codex_home)?;
    provider_auth.deleted_api_keys.remove(provider_key_id);
    provider_auth
        .api_keys
        .insert(provider_key_id.to_string(), api_key.to_string());
    save_provider_auth(codex_home, &provider_auth)
}

/// Remove a single provider key from the legacy plaintext `provider_auth.json`.
///
/// Used after a vault write succeeds so re-saving a provider key migrates it off plaintext
/// storage. Removes the file entirely when it becomes empty. Returns `true` if a key was removed.
pub(super) fn legacy_delete_provider_key(
    codex_home: &Path,
    provider_key_id: &str,
) -> std::io::Result<bool> {
    let mut provider_auth = load_provider_auth(codex_home)?;
    let removed = provider_auth.api_keys.remove(provider_key_id).is_some();
    if removed {
        if provider_auth.api_keys.is_empty() && provider_auth.deleted_api_keys.is_empty() {
            delete_provider_auth_if_exists(codex_home)?;
        } else {
            save_provider_auth(codex_home, &provider_auth)?;
        }
    }
    Ok(removed)
}

fn legacy_provider_key_is_deleted(
    codex_home: &Path,
    provider_key_id: &str,
) -> std::io::Result<bool> {
    Ok(load_provider_auth(codex_home)?
        .deleted_api_keys
        .contains(provider_key_id))
}

fn legacy_mark_provider_key_deleted(
    codex_home: &Path,
    provider_key_id: &str,
) -> std::io::Result<bool> {
    let mut provider_auth = load_provider_auth(codex_home)?;
    provider_auth.api_keys.remove(provider_key_id);
    let inserted = provider_auth
        .deleted_api_keys
        .insert(provider_key_id.to_string());
    save_provider_auth(codex_home, &provider_auth)?;
    Ok(inserted)
}

fn legacy_clear_provider_key_tombstone(
    codex_home: &Path,
    provider_key_id: &str,
) -> std::io::Result<bool> {
    let mut provider_auth = load_provider_auth(codex_home)?;
    let removed = provider_auth.deleted_api_keys.remove(provider_key_id);
    if removed {
        if provider_auth.api_keys.is_empty() && provider_auth.deleted_api_keys.is_empty() {
            delete_provider_auth_if_exists(codex_home)?;
        } else {
            save_provider_auth(codex_home, &provider_auth)?;
        }
    }
    Ok(removed)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthConfig {
    pub codex_home: PathBuf,
    pub auth_credentials_store_mode: AuthCredentialsStoreMode,
    pub keyring_backend_kind: AuthKeyringBackendKind,
    pub forced_login_method: Option<ForcedLoginMethod>,
    pub chatgpt_base_url: Option<String>,
    pub forced_chatgpt_workspace_id: Option<Vec<String>>,
    pub auth_route_config: AuthRouteConfig,
}

/// Enforces configured login restrictions using auth-owned HTTP settings.
pub async fn enforce_login_restrictions(config: &AuthConfig) -> std::io::Result<()> {
    let agent_identity_authapi_base_url =
        agent_identity_authapi_base_url(config.chatgpt_base_url.as_deref()).ok();
    enforce_login_restrictions_with_agent_identity_authapi_base_url(
        config,
        agent_identity_authapi_base_url.as_deref(),
    )
    .await
}

async fn enforce_login_restrictions_with_agent_identity_authapi_base_url(
    config: &AuthConfig,
    agent_identity_authapi_base_url: Option<&str>,
) -> std::io::Result<()> {
    let Some(auth) = load_auth_for_restrictions(config, agent_identity_authapi_base_url).await?
    else {
        return Ok(());
    };
    match login_restriction_violation(
        &auth,
        config.forced_login_method,
        config.forced_chatgpt_workspace_id.as_deref(),
    ) {
        None => Ok(()),
        Some(LoginRestrictionViolation::Method(reason)) => {
            let message = format!("{reason} Logging out.");
            if auth_uses_api_key_from_env(&auth) {
                return Err(std::io::Error::other(message));
            }
            logout_and_continue(
                &config.codex_home,
                message,
                config.auth_credentials_store_mode,
                config.keyring_backend_kind,
            )
        }
        Some(LoginRestrictionViolation::Workspace(reason)) => logout_with_message(
            &config.codex_home,
            format!("{reason} Logging out."),
            config.auth_credentials_store_mode,
            config.keyring_backend_kind,
        ),
    }
}

/// PF-84-S06 (#570): set when this process skipped the startup enforcement
/// of the login restrictions on the default login, because its session runs
/// (or a resumed or forked thread may run) on a named account.
static DEFAULT_LOGIN_LEFT_UNENFORCED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// PF-84-S06 (#570): records that this process left the default login in
/// place instead of enforcing the login restrictions on it at startup. Every
/// default-login manager [`AuthManager::shared_from_config`] builds afterwards
/// treats a login the restrictions rule out as absent, and a thread on the
/// default login is refused while it is ruled out. Call before any manager
/// is built.
pub fn leave_default_login_unenforced() {
    DEFAULT_LOGIN_LEFT_UNENFORCED.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// PF-84-S06 (#595): whether this process left the default login in place
/// (see [`leave_default_login_unenforced`]). An app server reports it at
/// initialize, so a client reusing it knows a ruled-out default login is
/// neither logged out nor used there.
pub fn default_login_left_unenforced() -> bool {
    DEFAULT_LOGIN_LEFT_UNENFORCED.load(std::sync::atomic::Ordering::SeqCst)
}

fn default_login_unusable_message(reason: &str) -> String {
    format!(
        "The default OpenAI login cannot be used: {reason} It was left unchanged; sign in with \
         `corbanu login`, or choose a named account with `--account` or /providers."
    )
}

async fn load_auth_for_restrictions(
    config: &AuthConfig,
    agent_identity_authapi_base_url: Option<&str>,
) -> std::io::Result<Option<CodexAuth>> {
    load_auth_with_openai_api_key_env(
        &config.codex_home,
        /*enable_codex_api_key_env*/ true,
        OpenAiApiKeyEnv::for_forced_login_method(config.forced_login_method),
        config.auth_credentials_store_mode,
        /*forced_chatgpt_workspace_id*/ None,
        config.chatgpt_base_url.as_deref(),
        config.keyring_backend_kind,
        agent_identity_authapi_base_url,
        &config.auth_route_config,
    )
    .await
}

/// How a login breaks the configured login restrictions. A wrong method is
/// logged out and the session continues; a wrong workspace ends it.
enum LoginRestrictionViolation {
    Method(String),
    Workspace(String),
}

impl LoginRestrictionViolation {
    fn reason(&self) -> &str {
        match self {
            Self::Method(reason) | Self::Workspace(reason) => reason,
        }
    }
}

fn login_restriction_violation(
    auth: &CodexAuth,
    forced_login_method: Option<ForcedLoginMethod>,
    forced_chatgpt_workspace_id: Option<&[String]>,
) -> Option<LoginRestrictionViolation> {
    if let Some(required_method) = forced_login_method {
        let method_violation = match (required_method, auth.auth_mode()) {
            (ForcedLoginMethod::Api, AuthMode::ApiKey)
            | (ForcedLoginMethod::Api, AuthMode::BedrockApiKey) => None,
            (ForcedLoginMethod::Chatgpt, AuthMode::Chatgpt)
            | (ForcedLoginMethod::Chatgpt, AuthMode::ChatgptAuthTokens)
            | (ForcedLoginMethod::Chatgpt, AuthMode::Headers)
            | (ForcedLoginMethod::Chatgpt, AuthMode::AgentIdentity)
            | (ForcedLoginMethod::Chatgpt, AuthMode::PersonalAccessToken) => None,
            (ForcedLoginMethod::Api, AuthMode::Chatgpt)
            | (ForcedLoginMethod::Api, AuthMode::ChatgptAuthTokens)
            | (ForcedLoginMethod::Api, AuthMode::Headers)
            | (ForcedLoginMethod::Api, AuthMode::AgentIdentity)
            | (ForcedLoginMethod::Api, AuthMode::PersonalAccessToken) => {
                Some("API key login is required, but ChatGPT is currently being used.")
            }
            (ForcedLoginMethod::Chatgpt, AuthMode::ApiKey)
            | (ForcedLoginMethod::Chatgpt, AuthMode::BedrockApiKey) => {
                Some("ChatGPT login is required, but an API key is currently being used.")
            }
        };
        if let Some(reason) = method_violation {
            return Some(LoginRestrictionViolation::Method(reason.to_string()));
        }
    }

    let expected_account_ids = forced_chatgpt_workspace_id?;
    let chatgpt_account_id = match auth {
        CodexAuth::ApiKey(_) | CodexAuth::Headers(_) | CodexAuth::BedrockApiKey(_) => {
            return None;
        }
        CodexAuth::AgentIdentity(_) | CodexAuth::PersonalAccessToken(_) => auth.get_account_id(),
        CodexAuth::Chatgpt(_) | CodexAuth::ChatgptAuthTokens(_) => match auth.get_token_data() {
            Ok(data) => data.id_token.chatgpt_account_id,
            Err(err) => {
                return Some(LoginRestrictionViolation::Workspace(format!(
                    "Failed to load ChatGPT credentials while enforcing workspace restrictions: {err}."
                )));
            }
        },
    };

    // workspace is the external identifier for account id.
    let chatgpt_account_id = chatgpt_account_id.as_deref();
    if chatgpt_account_id.is_some_and(|actual| {
        expected_account_ids
            .iter()
            .any(|expected| expected == actual)
    }) {
        return None;
    }
    let expected_workspaces = expected_account_ids.join(", ");
    Some(LoginRestrictionViolation::Workspace(
        match chatgpt_account_id {
            Some(actual) => format!(
                "Login is restricted to workspace(s) {expected_workspaces}, but current credentials belong to {actual}."
            ),
            None => format!(
                "Login is restricted to workspace(s) {expected_workspaces}, but current credentials lack a workspace identifier."
            ),
        },
    ))
}

fn logout_with_message(
    codex_home: &Path,
    message: String,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<()> {
    // External auth tokens live in the ephemeral store, but persistent auth may still exist
    // from earlier logins. Clear both so a forced logout truly removes all active account auth.
    let removal_result = logout_all_stores(
        codex_home,
        auth_credentials_store_mode,
        keyring_backend_kind,
        /*include_provider_credentials*/ false,
    );
    let error_message = match removal_result {
        Ok(_) => message,
        Err(err) => format!("{message}. Failed to remove auth.json: {err}"),
    };
    Err(std::io::Error::other(error_message))
}

fn logout_and_continue(
    codex_home: &Path,
    message: String,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<()> {
    match logout_all_stores(
        codex_home,
        auth_credentials_store_mode,
        keyring_backend_kind,
        /*include_provider_credentials*/ false,
    ) {
        Ok(_) => {
            tracing::warn!("{message}");
            Ok(())
        }
        Err(err) => Err(std::io::Error::other(format!(
            "{message}. Failed to remove auth.json: {err}"
        ))),
    }
}

fn logout_all_stores(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
    include_provider_credentials: bool,
) -> std::io::Result<bool> {
    if auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
        let removed_ephemeral = logout(
            codex_home,
            AuthCredentialsStoreMode::Ephemeral,
            AuthKeyringBackendKind::default(),
        )?;
        let removed_provider = include_provider_credentials
            && logout_all_credentials(
                codex_home,
                AuthCredentialsStoreMode::File,
                keyring_backend_kind,
            )?;
        return Ok(removed_ephemeral || removed_provider);
    }
    let removed_ephemeral = logout(
        codex_home,
        AuthCredentialsStoreMode::Ephemeral,
        AuthKeyringBackendKind::default(),
    )?;
    let removed_managed = logout(
        codex_home,
        auth_credentials_store_mode,
        keyring_backend_kind,
    )?;
    let removed_provider = include_provider_credentials
        && logout_all_credentials(
            codex_home,
            auth_credentials_store_mode,
            keyring_backend_kind,
        )?;
    Ok(removed_ephemeral || removed_managed || removed_provider)
}

/// Loads auth without the `OPENAI_API_KEY` fallback.
#[allow(clippy::too_many_arguments)]
async fn load_auth(
    codex_home: &Path,
    enable_codex_api_key_env: bool,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    forced_chatgpt_workspace_id: Option<&[String]>,
    chatgpt_base_url: Option<&str>,
    keyring_backend_kind: AuthKeyringBackendKind,
    agent_identity_authapi_base_url: Option<&str>,
    auth_route_config: &AuthRouteConfig,
) -> std::io::Result<Option<CodexAuth>> {
    load_auth_with_openai_api_key_env(
        codex_home,
        enable_codex_api_key_env,
        OpenAiApiKeyEnv::Ignore,
        auth_credentials_store_mode,
        forced_chatgpt_workspace_id,
        chatgpt_base_url,
        keyring_backend_kind,
        agent_identity_authapi_base_url,
        auth_route_config,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn load_auth_with_openai_api_key_env(
    codex_home: &Path,
    enable_codex_api_key_env: bool,
    openai_api_key_env: OpenAiApiKeyEnv,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    forced_chatgpt_workspace_id: Option<&[String]>,
    chatgpt_base_url: Option<&str>,
    keyring_backend_kind: AuthKeyringBackendKind,
    agent_identity_authapi_base_url: Option<&str>,
    auth_route_config: &AuthRouteConfig,
) -> std::io::Result<Option<CodexAuth>> {
    // Precedence (docs/authentication.md, "OpenAI API keys"): CODEX_API_KEY
    // (exec only), then external or stored OpenAI credentials, then
    // OPENAI_API_KEY. A shell-exported OPENAI_API_KEY therefore never replaces
    // a ChatGPT sign-in or a saved key, so it can't switch the billing basis.
    if enable_codex_api_key_env && let Some(api_key) = read_codex_api_key_from_env() {
        return Ok(Some(CodexAuth::from_api_key_env(
            api_key.as_str(),
            CODEX_API_KEY_ENV_VAR,
        )));
    }
    if enable_codex_api_key_env && let Some(api_key) = read_ambient_api_key_from_env() {
        return Ok(Some(CodexAuth::from_api_key_env(
            api_key.as_str(),
            AMBIENT_API_KEY_ENV_VAR,
        )));
    }
    let openai_api_key_env_auth = || match openai_api_key_env {
        OpenAiApiKeyEnv::Fallback => read_openai_api_key_from_env()
            .map(|api_key| CodexAuth::from_api_key_env(api_key.as_str(), OPENAI_API_KEY_ENV_VAR)),
        OpenAiApiKeyEnv::Ignore => None,
    };

    // External ChatGPT auth tokens live in the in-memory (ephemeral) store. Always check this
    // first so external auth takes precedence over any persisted credentials.
    let ephemeral_storage = create_auth_storage(
        codex_home.to_path_buf(),
        AuthCredentialsStoreMode::Ephemeral,
        AuthKeyringBackendKind::default(),
    );
    if let Some(auth_dot_json) = ephemeral_storage.load()?
        && auth_dot_json.has_primary_auth_material()
    {
        let auth = CodexAuth::from_auth_dot_json(
            codex_home,
            auth_dot_json,
            AuthCredentialsStoreMode::Ephemeral,
            chatgpt_base_url,
            keyring_backend_kind,
            agent_identity_authapi_base_url,
            auth_route_config,
        )
        .await?;
        if let CodexAuth::PersonalAccessToken(auth) = &auth {
            ensure_personal_access_token_workspace_allowed(forced_chatgpt_workspace_id, auth)?;
        }
        return Ok(Some(auth));
    }

    if let Some(access_token) = read_codex_access_token_from_env() {
        return match classify_codex_access_token(&access_token) {
            CodexAccessToken::PersonalAccessToken(access_token) => {
                let auth = PersonalAccessTokenAuth::load(access_token, auth_route_config).await?;
                ensure_personal_access_token_workspace_allowed(forced_chatgpt_workspace_id, &auth)?;
                Ok(Some(CodexAuth::PersonalAccessToken(auth)))
            }
            CodexAccessToken::AgentIdentityJwt(jwt) => {
                CodexAuth::from_agent_identity_jwt_with_authapi_base_url(
                    jwt,
                    chatgpt_base_url,
                    require_agent_identity_authapi_base_url(agent_identity_authapi_base_url)?,
                    auth_route_config,
                )
            }
            .await
            .map(Some),
        };
    }

    // If the caller explicitly requested ephemeral auth, there is no persisted fallback.
    if auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
        return Ok(openai_api_key_env_auth());
    }

    // Fall back to the configured persistent store (file/keyring/auto) for managed auth.
    let storage = create_auth_storage(
        codex_home.to_path_buf(),
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    let auth_dot_json = match storage.load()? {
        Some(auth) if auth.has_primary_auth_material() => auth,
        Some(_) | None => match load_pfterminal_legacy_codex_auth(
            codex_home,
            auth_credentials_store_mode,
            keyring_backend_kind,
        )? {
            Some(auth) => auth,
            None => return Ok(openai_api_key_env_auth()),
        },
    };

    let auth = CodexAuth::from_auth_dot_json(
        codex_home,
        auth_dot_json,
        auth_credentials_store_mode,
        chatgpt_base_url,
        keyring_backend_kind,
        agent_identity_authapi_base_url,
        auth_route_config,
    )
    .await?;
    if let CodexAuth::PersonalAccessToken(auth) = &auth {
        ensure_personal_access_token_workspace_allowed(forced_chatgpt_workspace_id, auth)?;
    }
    Ok(Some(auth))
}

fn load_pfterminal_legacy_codex_auth(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
) -> std::io::Result<Option<AuthDotJson>> {
    let Some(legacy_codex_home) = legacy_codex_home_for_product_home(codex_home) else {
        return Ok(None);
    };
    let storage = create_auth_storage(
        legacy_codex_home.clone(),
        auth_credentials_store_mode,
        keyring_backend_kind,
    );
    let auth = storage
        .load()?
        .filter(AuthDotJson::has_primary_auth_material);
    if auth.is_some() {
        tracing::info!(
            product_home = %codex_home.display(),
            legacy_codex_home = %legacy_codex_home.display(),
            "loaded legacy Codex auth for Corbanu Terminal"
        );
    }
    Ok(auth)
}

pub fn legacy_codex_home_for_product_home(codex_home: &Path) -> Option<PathBuf> {
    let home_name = codex_home.file_name()?.to_str()?;
    if home_name != ".corbanu" && home_name != ".pfterminal" {
        return None;
    }
    Some(codex_home.parent()?.join(".codex"))
}

// Persist refreshed tokens into auth storage and update last_refresh.
// D171 (#1311): read and saved in one step, and for a named account only onto
// the stored sign-in that was refreshed (`refreshed_from` is its refresh
// token), so a removal or a new sign-in by another process meanwhile is kept.
fn persist_tokens(
    storage: &Arc<dyn AuthStorageBackend>,
    refreshed_from: &str,
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
) -> std::io::Result<AuthDotJson> {
    let id_token = id_token
        .map(|id_token| parse_chatgpt_jwt_claims(&id_token).map_err(std::io::Error::other))
        .transpose()?;
    let named = storage.is_named_account();
    storage
        .update(&mut |auth_dot_json| {
            if named
                && auth_dot_json
                    .tokens
                    .as_ref()
                    .is_none_or(|tokens| tokens.refresh_token != refreshed_from)
            {
                return Err(std::io::Error::other(
                    "the named account's sign-in changed while it was refreshed; \
                     the refreshed tokens were not saved",
                ));
            }
            let tokens = auth_dot_json.tokens.get_or_insert_with(TokenData::default);
            if let Some(id_token) = id_token.clone() {
                tokens.id_token = id_token;
            }
            if let Some(access_token) = access_token.clone() {
                tokens.access_token = access_token;
            }
            if let Some(refresh_token) = refresh_token.clone() {
                tokens.refresh_token = refresh_token;
            }
            auth_dot_json.last_refresh = Some(Utc::now());
            Ok(())
        })?
        .ok_or(std::io::Error::other("Token data is not available."))
}

// Requests refreshed ChatGPT OAuth tokens from the auth service using a refresh token.
// The caller is responsible for persisting any returned tokens.
async fn request_chatgpt_token_refresh(
    refresh_token: String,
    client: &HttpClient,
) -> Result<RefreshResponse, RefreshTokenError> {
    let refresh_request = RefreshRequest {
        client_id: oauth_client_id(),
        grant_type: "refresh_token",
        refresh_token,
    };
    let endpoint = refresh_token_endpoint();

    // Use shared client factory to include standard headers
    let response = client
        .post(endpoint.as_str())
        .header("Content-Type", "application/json")
        .json(&refresh_request)
        .send()
        .await
        .map_err(|err| RefreshTokenError::Transient(std::io::Error::other(err)))?;

    let status = response.status();
    if status.is_success() {
        let refresh_response = response
            .json::<RefreshResponse>()
            .await
            .map_err(|err| RefreshTokenError::Transient(std::io::Error::other(err)))?;
        Ok(refresh_response)
    } else {
        let body = response.text().await.unwrap_or_default();
        tracing::error!("Failed to refresh token: {status}: {body}");
        let failed = classify_refresh_token_failure(&body);
        if status == StatusCode::UNAUTHORIZED || failed.reason != RefreshTokenFailedReason::Other {
            Err(RefreshTokenError::Permanent(failed))
        } else {
            let message = try_parse_error_message(&body);
            Err(RefreshTokenError::Transient(std::io::Error::other(
                format!("Failed to refresh token: {status}: {message}"),
            )))
        }
    }
}

fn classify_refresh_token_failure(body: &str) -> RefreshTokenFailedError {
    let code = extract_refresh_token_error_code(body);

    let normalized_code = code.as_deref().map(str::to_ascii_lowercase);
    let reason = match normalized_code.as_deref() {
        Some("refresh_token_expired") => RefreshTokenFailedReason::Expired,
        Some("refresh_token_reused") => RefreshTokenFailedReason::Exhausted,
        Some("refresh_token_invalidated") => RefreshTokenFailedReason::Revoked,
        _ => RefreshTokenFailedReason::Other,
    };

    if reason == RefreshTokenFailedReason::Other {
        tracing::warn!(
            backend_code = normalized_code.as_deref(),
            backend_body = body,
            "Encountered unknown response while refreshing token"
        );
    }

    let message = match reason {
        RefreshTokenFailedReason::Expired => REFRESH_TOKEN_EXPIRED_MESSAGE.to_string(),
        RefreshTokenFailedReason::Exhausted => REFRESH_TOKEN_REUSED_MESSAGE.to_string(),
        RefreshTokenFailedReason::Revoked => REFRESH_TOKEN_INVALIDATED_MESSAGE.to_string(),
        RefreshTokenFailedReason::Other => REFRESH_TOKEN_UNKNOWN_MESSAGE.to_string(),
    };

    RefreshTokenFailedError::new(reason, message)
}

fn extract_refresh_token_error_code(body: &str) -> Option<String> {
    if body.trim().is_empty() {
        return None;
    }

    let Value::Object(map) = serde_json::from_str::<Value>(body).ok()? else {
        return None;
    };

    if let Some(error_value) = map.get("error") {
        match error_value {
            Value::Object(obj) => {
                if let Some(code) = obj.get("code").and_then(Value::as_str) {
                    return Some(code.to_string());
                }
            }
            Value::String(code) => {
                return Some(code.to_string());
            }
            _ => {}
        }
    }

    map.get("code").and_then(Value::as_str).map(str::to_string)
}

#[derive(Serialize)]
struct RefreshRequest {
    client_id: String,
    grant_type: &'static str,
    refresh_token: String,
}

#[derive(Deserialize, Clone)]
struct RefreshResponse {
    id_token: Option<String>,
    access_token: Option<String>,
    refresh_token: Option<String>,
}

// Shared constant for token refresh (client id used for oauth token refresh flow)
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

pub fn oauth_client_id() -> String {
    std::env::var(CLIENT_ID_OVERRIDE_ENV_VAR)
        .ok()
        .filter(|client_id| !client_id.trim().is_empty())
        .unwrap_or_else(|| CLIENT_ID.to_string())
}

fn refresh_token_endpoint() -> String {
    std::env::var(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR)
        .unwrap_or_else(|_| REFRESH_TOKEN_URL.to_string())
}

impl AuthDotJson {
    fn has_primary_auth_material(&self) -> bool {
        self.auth_mode.is_some()
            || self.openai_api_key.is_some()
            || self.tokens.is_some()
            || self
                .agent_identity
                .as_ref()
                .is_some_and(AgentIdentityStorage::has_auth_material)
            || self.personal_access_token.is_some()
            || self.bedrock_api_key.is_some()
    }

    fn from_external_access_token(
        access_token: &str,
        chatgpt_account_id: &str,
        chatgpt_plan_type: Option<&str>,
    ) -> std::io::Result<Self> {
        let mut token_info =
            parse_chatgpt_jwt_claims(access_token).map_err(std::io::Error::other)?;
        token_info.chatgpt_account_id = Some(chatgpt_account_id.to_string());
        token_info.chatgpt_plan_type = chatgpt_plan_type
            .map(InternalPlanType::from_raw_value)
            .or(token_info.chatgpt_plan_type)
            .or(Some(InternalPlanType::Unknown("unknown".to_string())));
        let tokens = TokenData {
            id_token: token_info,
            access_token: access_token.to_string(),
            refresh_token: String::new(),
            account_id: Some(chatgpt_account_id.to_string()),
        };

        Ok(Self {
            auth_mode: Some(AuthMode::ChatgptAuthTokens),
            openai_api_key: None,
            tokens: Some(tokens),
            last_refresh: Some(Utc::now()),
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
        })
    }

    pub(super) fn resolved_mode(&self) -> AuthMode {
        if let Some(mode) = self.auth_mode {
            return mode;
        }
        if self.personal_access_token.is_some() {
            return AuthMode::PersonalAccessToken;
        }
        if self.bedrock_api_key.is_some() {
            return AuthMode::BedrockApiKey;
        }
        if self.openai_api_key.is_some() {
            return AuthMode::ApiKey;
        }
        AuthMode::Chatgpt
    }

    fn storage_mode(
        &self,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> AuthCredentialsStoreMode {
        if self.resolved_mode() == AuthMode::ChatgptAuthTokens {
            AuthCredentialsStoreMode::Ephemeral
        } else {
            auth_credentials_store_mode
        }
    }
}

/// Internal cached auth state.
#[derive(Clone)]
struct CachedAuth {
    auth: Option<CodexAuth>,
    /// Permanent refresh failure cached for the current auth snapshot so
    /// later refresh attempts for the same credentials fail fast without network.
    permanent_refresh_failure: Option<AuthScopedRefreshFailure>,
}

#[derive(Clone)]
struct AuthScopedRefreshFailure {
    auth: CodexAuth,
    error: RefreshTokenFailedError,
}

impl Debug for CachedAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachedAuth")
            .field(
                "auth_mode",
                &self.auth.as_ref().map(CodexAuth::api_auth_mode),
            )
            .field(
                "permanent_refresh_failure",
                &self
                    .permanent_refresh_failure
                    .as_ref()
                    .map(|failure| failure.error.reason),
            )
            .finish()
    }
}

enum UnauthorizedRecoveryStep {
    Reload,
    RefreshToken,
    ExternalRefresh,
    Done,
}

enum ReloadOutcome {
    /// Reload was performed and the cached auth changed
    ReloadedChanged,
    /// Reload was performed and the cached auth remained the same
    ReloadedNoChange,
    /// Reload was skipped (missing or mismatched account id)
    Skipped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnauthorizedRecoveryMode {
    Managed,
    External,
}

// UnauthorizedRecovery is a state machine that handles an attempt to refresh the authentication when requests
// to API fail with 401 status code.
// The client calls next() every time it encounters a 401 error, one time per retry.
// For API key based authentication, we don't do anything and let the error bubble to the user.
//
// For ChatGPT based authentication, we:
// 1. Attempt to reload the auth data from disk. We only reload if the account id matches the one the current process is running as.
// 2. Attempt to refresh the token using OAuth token refresh flow.
// If after both steps the server still responds with 401 we let the error bubble to the user.
//
// For external auth sources, UnauthorizedRecovery retries once by asking the
// configured provider to refresh and caching the returned auth through the same
// path used by other auth sources.
pub struct UnauthorizedRecovery {
    manager: Arc<AuthManager>,
    step: UnauthorizedRecoveryStep,
    expected_account_id: Option<String>,
    mode: UnauthorizedRecoveryMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnauthorizedRecoveryStepResult {
    auth_state_changed: Option<bool>,
}

impl UnauthorizedRecoveryStepResult {
    pub fn auth_state_changed(&self) -> Option<bool> {
        self.auth_state_changed
    }
}

impl UnauthorizedRecovery {
    fn new(manager: Arc<AuthManager>) -> Self {
        let cached_auth = manager.auth_cached();
        let expected_account_id = cached_auth.as_ref().and_then(CodexAuth::get_account_id);
        let mode = if manager.has_external_auth() {
            UnauthorizedRecoveryMode::External
        } else {
            UnauthorizedRecoveryMode::Managed
        };
        let step = match mode {
            UnauthorizedRecoveryMode::Managed => UnauthorizedRecoveryStep::Reload,
            UnauthorizedRecoveryMode::External => UnauthorizedRecoveryStep::ExternalRefresh,
        };
        Self {
            manager,
            step,
            expected_account_id,
            mode,
        }
    }

    pub fn has_next(&self) -> bool {
        if self.manager.has_external_api_key_auth() {
            return !matches!(self.step, UnauthorizedRecoveryStep::Done);
        }

        if !self
            .manager
            .auth_cached()
            .as_ref()
            .is_some_and(CodexAuth::supports_unauthorized_recovery)
        {
            return false;
        }

        if self.mode == UnauthorizedRecoveryMode::External && !self.manager.has_external_auth() {
            return false;
        }

        !matches!(self.step, UnauthorizedRecoveryStep::Done)
    }

    pub fn unavailable_reason(&self) -> &'static str {
        if self.manager.has_external_api_key_auth() {
            return if matches!(self.step, UnauthorizedRecoveryStep::Done) {
                "recovery_exhausted"
            } else {
                "ready"
            };
        }

        if self
            .manager
            .auth_cached()
            .as_ref()
            .is_some_and(CodexAuth::is_personal_access_token_auth)
        {
            return "not_refreshable_auth";
        }

        if !self
            .manager
            .auth_cached()
            .as_ref()
            .is_some_and(CodexAuth::supports_unauthorized_recovery)
        {
            return "not_chatgpt_auth";
        }

        if self.mode == UnauthorizedRecoveryMode::External && !self.manager.has_external_auth() {
            return "no_external_auth";
        }

        if matches!(self.step, UnauthorizedRecoveryStep::Done) {
            return "recovery_exhausted";
        }

        "ready"
    }

    pub fn mode_name(&self) -> &'static str {
        match self.mode {
            UnauthorizedRecoveryMode::Managed => "managed",
            UnauthorizedRecoveryMode::External => "external",
        }
    }

    pub fn step_name(&self) -> &'static str {
        match self.step {
            UnauthorizedRecoveryStep::Reload => "reload",
            UnauthorizedRecoveryStep::RefreshToken => "refresh_token",
            UnauthorizedRecoveryStep::ExternalRefresh => "external_refresh",
            UnauthorizedRecoveryStep::Done => "done",
        }
    }

    pub async fn next(&mut self) -> Result<UnauthorizedRecoveryStepResult, RefreshTokenError> {
        if !self.has_next() {
            return Err(RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                RefreshTokenFailedReason::Other,
                "No more recovery steps available.",
            )));
        }

        match self.step {
            UnauthorizedRecoveryStep::Reload => {
                match self
                    .manager
                    .reload_if_account_id_matches(self.expected_account_id.as_deref())
                    .await
                {
                    ReloadOutcome::ReloadedChanged => {
                        self.step = UnauthorizedRecoveryStep::RefreshToken;
                        return Ok(UnauthorizedRecoveryStepResult {
                            auth_state_changed: Some(true),
                        });
                    }
                    ReloadOutcome::ReloadedNoChange => {
                        self.step = UnauthorizedRecoveryStep::RefreshToken;
                        return Ok(UnauthorizedRecoveryStepResult {
                            auth_state_changed: Some(false),
                        });
                    }
                    ReloadOutcome::Skipped => {
                        self.step = UnauthorizedRecoveryStep::Done;
                        return Err(RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                            RefreshTokenFailedReason::Other,
                            REFRESH_TOKEN_ACCOUNT_MISMATCH_MESSAGE.to_string(),
                        )));
                    }
                }
            }
            UnauthorizedRecoveryStep::RefreshToken => {
                self.manager.refresh_token_from_authority().await?;
                self.step = UnauthorizedRecoveryStep::Done;
                return Ok(UnauthorizedRecoveryStepResult {
                    auth_state_changed: Some(true),
                });
            }
            UnauthorizedRecoveryStep::ExternalRefresh => {
                self.manager.refresh_token_from_authority().await?;
                self.step = UnauthorizedRecoveryStep::Done;
                return Ok(UnauthorizedRecoveryStepResult {
                    auth_state_changed: Some(true),
                });
            }
            UnauthorizedRecoveryStep::Done => {}
        }
        Ok(UnauthorizedRecoveryStepResult {
            auth_state_changed: None,
        })
    }
}

/// Central manager providing a single source of truth for auth.json derived
/// authentication data. It loads once (or on preference change) and then
/// hands out cloned `CodexAuth` values so the rest of the program has a
/// consistent snapshot.
///
/// External modifications to `auth.json` will NOT be observed until
/// `reload()` is called explicitly. This matches the design goal of avoiding
/// different parts of the program seeing inconsistent auth data mid‑run.
pub struct AuthManager {
    codex_home: PathBuf,
    inner: RwLock<CachedAuth>,
    auth_change_tx: watch::Sender<u64>,
    enable_codex_api_key_env: bool,
    openai_api_key_env: OpenAiApiKeyEnv,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    keyring_backend_kind: AuthKeyringBackendKind,
    forced_chatgpt_workspace_id: RwLock<Option<Vec<String>>>,
    chatgpt_base_url: Option<String>,
    agent_identity_authapi_base_url: Option<String>,
    refresh_lock: Semaphore,
    agent_identity_lock: Semaphore,
    agent_identity_bootstrap_cooldown: Mutex<AgentIdentityBootstrapCooldown>,
    external_auth: RwLock<Option<Arc<dyn ExternalAuth>>>,
    provider_api_key_cache: RwLock<ProviderApiKeyCache>,
    auth_route_config: AuthRouteConfig,
    /// PF-84-S06: the named sign-in account this manager serves. It loads,
    /// refreshes and logs out only that account's vault store, never the
    /// default login, the environment or the ephemeral store.
    named_account: Option<NamedChatgptAccount>,
    /// D171 (#1286): for a named-account manager, its vault handle (kept so
    /// the decrypted store stays cached) and the store marker last seen
    /// holding the account.
    named_store: Option<named_account::NamedAccountStore>,
    /// The login method the configuration requires (`forced_login_method` as
    /// configured, not the API-key-only onboarding default a third-party
    /// session provider implies); a named account's sign-in of the other
    /// kind is refused.
    forced_login_method: Option<ForcedLoginMethod>,
    /// For a named-account manager, the manager of the default login it was
    /// created from (providers without a named account go back to it).
    base_manager: Option<Arc<AuthManager>>,
    /// PF-84-S06 (#570): the session runs on a named sign-in account, so the
    /// default login was not checked (and logged out) at startup; one the
    /// login restrictions rule out is treated as absent instead.
    hide_restricted_default_login: bool,
    /// Why the default login is hidden right now, if it is.
    hidden_default_login_reason: RwLock<Option<String>>,
}

#[derive(Debug)]
struct ProviderApiKeyCache {
    storage_revision: u64,
    entries: HashMap<String, Option<String>>,
}

impl ProviderApiKeyCache {
    fn new() -> Self {
        Self {
            storage_revision: provider_api_key_storage_revision(),
            entries: HashMap::new(),
        }
    }

    fn clear_to_current_revision(&mut self) {
        self.storage_revision = provider_api_key_storage_revision();
        self.entries.clear();
    }
}

/// Configuration view required to construct a shared [`AuthManager`].
///
/// Implementations should return the auth-related config values for the
/// already-resolved runtime configuration. The primary implementation is
/// `codex_core::config::Config`, but this trait keeps `codex-login` independent
/// from `codex-core`.
pub trait AuthManagerConfig {
    /// Returns the Codex home directory used for auth storage.
    fn codex_home(&self) -> PathBuf;

    /// Returns the CLI auth credential storage mode for auth loading.
    fn cli_auth_credentials_store_mode(&self) -> AuthCredentialsStoreMode;

    /// Returns the backend to use when CLI auth keyring storage is selected.
    fn auth_keyring_backend_kind(&self) -> AuthKeyringBackendKind;

    /// Returns the workspace IDs that ChatGPT auth should be restricted to, if any.
    fn forced_chatgpt_workspace_id(&self) -> Option<Vec<String>>;

    /// Returns the ChatGPT backend base URL used for first-party backend authorization.
    fn chatgpt_base_url(&self) -> String;

    /// Returns route-selection settings for auth-owned clients.
    fn auth_route_config(&self) -> AuthRouteConfig;

    /// Returns the login method the configuration requires, if any. A required
    /// ChatGPT login stops `OPENAI_API_KEY` from being used as a fallback.
    fn forced_login_method(&self) -> Option<ForcedLoginMethod> {
        None
    }

    /// The login method the configuration itself requires (`forced_login_method`
    /// as configured). Unlike [`AuthManagerConfig::forced_login_method`] it
    /// omits the API-key-only default a third-party session provider implies
    /// for onboarding, which must not refuse a named OpenAI account (#571).
    fn configured_forced_login_method(&self) -> Option<ForcedLoginMethod> {
        self.forced_login_method()
    }

    /// PF-84-S06 (#570): whether the session runs on a named OpenAI sign-in
    /// account. Its startup then leaves the default login alone, so the
    /// default login's manager must not use one the restrictions rule out.
    fn runs_on_named_sign_in_account(&self) -> bool {
        false
    }
}

impl Debug for AuthManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthManager")
            .field("codex_home", &self.codex_home)
            .field("inner", &self.inner)
            .field("enable_codex_api_key_env", &self.enable_codex_api_key_env)
            .field(
                "auth_credentials_store_mode",
                &self.auth_credentials_store_mode,
            )
            .field("keyring_backend_kind", &self.keyring_backend_kind)
            .field(
                "forced_chatgpt_workspace_id",
                &self.forced_chatgpt_workspace_id,
            )
            .field("chatgpt_base_url", &self.chatgpt_base_url)
            .field("auth_route_config", &self.auth_route_config)
            .field("has_external_auth", &self.has_external_auth())
            .finish_non_exhaustive()
    }
}

fn default_agent_identity_authapi_base_url() -> Option<String> {
    agent_identity_authapi_base_url(/*chatgpt_base_url*/ None).ok()
}

impl AuthManager {
    /// Auth storage root used by provider-scoped credential caches.
    pub fn codex_home(&self) -> &Path {
        &self.codex_home
    }

    /// Create a new manager loading the initial auth using the provided
    /// preferred auth method. Errors loading auth are swallowed; `auth()` will
    /// simply return `None` in that case so callers can treat it as an
    /// unauthenticated state. `OPENAI_API_KEY` is not read; model sessions use
    /// [`AuthManager::shared_from_config`], which adds it as a fallback.
    pub async fn new(
        codex_home: PathBuf,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        forced_chatgpt_workspace_id: Option<Vec<String>>,
        chatgpt_base_url: Option<String>,
        keyring_backend_kind: AuthKeyringBackendKind,
        auth_route_config: AuthRouteConfig,
    ) -> Self {
        Self::new_with_openai_api_key_env(
            codex_home,
            enable_codex_api_key_env,
            OpenAiApiKeyEnv::Ignore,
            auth_credentials_store_mode,
            forced_chatgpt_workspace_id,
            chatgpt_base_url,
            keyring_backend_kind,
            auth_route_config,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn new_with_openai_api_key_env(
        codex_home: PathBuf,
        enable_codex_api_key_env: bool,
        openai_api_key_env: OpenAiApiKeyEnv,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        forced_chatgpt_workspace_id: Option<Vec<String>>,
        chatgpt_base_url: Option<String>,
        keyring_backend_kind: AuthKeyringBackendKind,
        auth_route_config: AuthRouteConfig,
    ) -> Self {
        let agent_identity_authapi_base_url =
            agent_identity_authapi_base_url(chatgpt_base_url.as_deref()).ok();
        let managed_auth = load_auth_with_openai_api_key_env(
            &codex_home,
            enable_codex_api_key_env,
            openai_api_key_env,
            auth_credentials_store_mode,
            forced_chatgpt_workspace_id.as_deref(),
            chatgpt_base_url.as_deref(),
            keyring_backend_kind,
            agent_identity_authapi_base_url.as_deref(),
            &auth_route_config,
        )
        .await
        .ok()
        .flatten();
        let (auth_change_tx, _auth_change_rx) = watch::channel(0);
        Self {
            codex_home,
            inner: RwLock::new(CachedAuth {
                auth: managed_auth,
                permanent_refresh_failure: None,
            }),
            auth_change_tx,
            enable_codex_api_key_env,
            openai_api_key_env,
            auth_credentials_store_mode,
            keyring_backend_kind,
            forced_chatgpt_workspace_id: RwLock::new(forced_chatgpt_workspace_id),
            chatgpt_base_url,
            agent_identity_authapi_base_url,
            refresh_lock: Semaphore::new(/*permits*/ 1),
            agent_identity_lock: Semaphore::new(/*permits*/ 1),
            agent_identity_bootstrap_cooldown: Mutex::default(),
            external_auth: RwLock::new(None),
            provider_api_key_cache: RwLock::new(ProviderApiKeyCache::new()),
            named_account: None,
            named_store: None,
            forced_login_method: None,
            base_manager: None,
            hide_restricted_default_login: false,
            hidden_default_login_reason: RwLock::new(None),
            auth_route_config,
        }
    }

    /// Create an AuthManager with a specific CodexAuth, for testing only.
    pub fn from_auth_for_testing(auth: CodexAuth) -> Arc<Self> {
        let cached = CachedAuth {
            auth: Some(auth),
            permanent_refresh_failure: None,
        };
        let (auth_change_tx, _auth_change_rx) = watch::channel(0);

        Arc::new(Self {
            codex_home: PathBuf::from("non-existent"),
            inner: RwLock::new(cached),
            auth_change_tx,
            enable_codex_api_key_env: false,
            openai_api_key_env: OpenAiApiKeyEnv::Ignore,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            keyring_backend_kind: AuthKeyringBackendKind::default(),
            forced_chatgpt_workspace_id: RwLock::new(None),
            chatgpt_base_url: None,
            agent_identity_authapi_base_url: default_agent_identity_authapi_base_url(),
            refresh_lock: Semaphore::new(/*permits*/ 1),
            agent_identity_lock: Semaphore::new(/*permits*/ 1),
            agent_identity_bootstrap_cooldown: Mutex::default(),
            external_auth: RwLock::new(None),
            provider_api_key_cache: RwLock::new(ProviderApiKeyCache::new()),
            named_account: None,
            named_store: None,
            forced_login_method: None,
            base_manager: None,
            hide_restricted_default_login: false,
            hidden_default_login_reason: RwLock::new(None),
            auth_route_config: crate::test_support::transport_default_auth_route_config(),
        })
    }

    /// Create an AuthManager with a specific CodexAuth and codex home, for testing only.
    pub fn from_auth_for_testing_with_home(auth: CodexAuth, codex_home: PathBuf) -> Arc<Self> {
        let cached = CachedAuth {
            auth: Some(auth),
            permanent_refresh_failure: None,
        };
        let (auth_change_tx, _auth_change_rx) = watch::channel(0);
        Arc::new(Self {
            codex_home,
            inner: RwLock::new(cached),
            auth_change_tx,
            enable_codex_api_key_env: false,
            openai_api_key_env: OpenAiApiKeyEnv::Ignore,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            keyring_backend_kind: AuthKeyringBackendKind::default(),
            forced_chatgpt_workspace_id: RwLock::new(None),
            chatgpt_base_url: None,
            agent_identity_authapi_base_url: default_agent_identity_authapi_base_url(),
            refresh_lock: Semaphore::new(/*permits*/ 1),
            agent_identity_lock: Semaphore::new(/*permits*/ 1),
            agent_identity_bootstrap_cooldown: Mutex::default(),
            external_auth: RwLock::new(None),
            provider_api_key_cache: RwLock::new(ProviderApiKeyCache::new()),
            named_account: None,
            named_store: None,
            forced_login_method: None,
            base_manager: None,
            hide_restricted_default_login: false,
            hidden_default_login_reason: RwLock::new(None),
            auth_route_config: crate::test_support::transport_default_auth_route_config(),
        })
    }

    /// Create an AuthManager with a specific CodexAuth and Agent Identity AuthAPI base URL, for testing only.
    #[doc(hidden)]
    pub fn from_auth_for_testing_with_agent_identity_authapi_base_url(
        auth: CodexAuth,
        agent_identity_authapi_base_url: String,
    ) -> Arc<Self> {
        let cached = CachedAuth {
            auth: Some(auth),
            permanent_refresh_failure: None,
        };
        let (auth_change_tx, _auth_change_rx) = watch::channel(0);
        Arc::new(Self {
            codex_home: PathBuf::from("non-existent"),
            inner: RwLock::new(cached),
            auth_change_tx,
            enable_codex_api_key_env: false,
            openai_api_key_env: OpenAiApiKeyEnv::Ignore,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            keyring_backend_kind: AuthKeyringBackendKind::default(),
            forced_chatgpt_workspace_id: RwLock::new(None),
            chatgpt_base_url: None,
            agent_identity_authapi_base_url: Some(
                agent_identity_authapi_base_url
                    .trim_end_matches('/')
                    .to_string(),
            ),
            refresh_lock: Semaphore::new(/*permits*/ 1),
            agent_identity_lock: Semaphore::new(/*permits*/ 1),
            agent_identity_bootstrap_cooldown: Mutex::default(),
            external_auth: RwLock::new(None),
            provider_api_key_cache: RwLock::new(ProviderApiKeyCache::new()),
            named_account: None,
            named_store: None,
            forced_login_method: None,
            base_manager: None,
            hide_restricted_default_login: false,
            hidden_default_login_reason: RwLock::new(None),
            auth_route_config: crate::test_support::transport_default_auth_route_config(),
        })
    }

    pub fn external_bearer_only(config: ModelProviderAuthInfo) -> Arc<Self> {
        Self::external_bearer_only_with_cache_policy(config, ExternalBearerCachePolicy::Timed)
    }

    pub fn external_bearer_only_with_cache_policy(
        config: ModelProviderAuthInfo,
        cache_policy: ExternalBearerCachePolicy,
    ) -> Arc<Self> {
        Self::external_bearer_only_for_account(config, cache_policy, /*account*/ None)
    }

    /// PF-84: like [`Self::external_bearer_only_with_cache_policy`], for a
    /// named account the command receives in `CORBANU_PROVIDER_ACCOUNT`.
    pub fn external_bearer_only_for_account(
        config: ModelProviderAuthInfo,
        cache_policy: ExternalBearerCachePolicy,
        account: Option<super::external_bearer::ExternalBearerAccount>,
    ) -> Arc<Self> {
        let (auth_change_tx, _auth_change_rx) = watch::channel(0);
        Arc::new(Self {
            codex_home: PathBuf::from("non-existent"),
            inner: RwLock::new(CachedAuth {
                auth: None,
                permanent_refresh_failure: None,
            }),
            auth_change_tx,
            enable_codex_api_key_env: false,
            openai_api_key_env: OpenAiApiKeyEnv::Ignore,
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            keyring_backend_kind: AuthKeyringBackendKind::default(),
            forced_chatgpt_workspace_id: RwLock::new(None),
            chatgpt_base_url: None,
            agent_identity_authapi_base_url: default_agent_identity_authapi_base_url(),
            refresh_lock: Semaphore::new(/*permits*/ 1),
            agent_identity_lock: Semaphore::new(/*permits*/ 1),
            agent_identity_bootstrap_cooldown: Mutex::default(),
            external_auth: RwLock::new(Some(Arc::new(BearerTokenRefresher::new(
                config,
                cache_policy,
                account,
            )) as Arc<dyn ExternalAuth>)),
            provider_api_key_cache: RwLock::new(ProviderApiKeyCache::new()),
            named_account: None,
            named_store: None,
            forced_login_method: None,
            base_manager: None,
            hide_restricted_default_login: false,
            hidden_default_login_reason: RwLock::new(None),
            // External bearer auth refreshes by running the provider's command and never makes
            // auth-owned HTTP requests, so this route is intentionally inert.
            auth_route_config: AuthRouteConfig::from_http_client_factory(HttpClientFactory::new(
                OutboundProxyPolicy::ReqwestDefault,
            )),
        })
    }

    /// Current cached auth (clone) without attempting a refresh.
    ///
    /// D171 (#1286): a named account's sign-in is dropped once the account
    /// was removed or signed out (by any process); [`AuthManager::auth`]
    /// picks up a later sign-in.
    pub fn auth_cached(&self) -> Option<CodexAuth> {
        loop {
            match self.held_named_sign_in() {
                named_account::HeldSignIn::Current => {}
                named_account::HeldSignIn::Changed => self.reload_named_sign_in_sync(),
                named_account::HeldSignIn::Gone { epoch } => {
                    if !self.clear_named_sign_in_if_unchanged(epoch) {
                        // The replacement may itself have been removed since
                        // it was installed. Check it before returning it.
                        continue;
                    }
                }
            }
            return self.auth_cached_unchecked();
        }
    }

    /// The cached auth as it is, for the guarded reloads that compare it with
    /// the stored one.
    fn auth_cached_unchecked(&self) -> Option<CodexAuth> {
        self.inner
            .read()
            .ok()
            .and_then(|cached| cached.auth.clone())
    }

    /// Subscribes to cached auth changes that can affect request recovery.
    pub fn auth_change_receiver(&self) -> watch::Receiver<u64> {
        self.auth_change_tx.subscribe()
    }

    pub fn refresh_failure_for_auth(&self, auth: &CodexAuth) -> Option<RefreshTokenFailedError> {
        self.inner.read().ok().and_then(|cached| {
            cached
                .permanent_refresh_failure
                .as_ref()
                .filter(|failure| Self::auths_equal_for_refresh(Some(auth), Some(&failure.auth)))
                .map(|failure| failure.error.clone())
        })
    }

    /// Current cached auth (clone). May be `None` if not logged in or load failed.
    /// For managed ChatGPT auth that needs a proactive refresh, first performs
    /// a guarded reload and then refreshes only if the on-disk auth is unchanged.
    #[instrument(level = "trace", skip_all)]
    pub async fn auth(&self) -> Option<CodexAuth> {
        // A named account loads lazily, and picks up a sign-in added later.
        // D171 (#1286): `auth_cached` drops a sign-in held in memory once the
        // account was removed or replaced (by any process), so the next
        // request is refused with its recovery or runs on the new sign-in.
        if self.named_account.is_some() && self.auth_cached().is_none() {
            self.reload().await;
        }
        if self.has_external_auth() {
            self.reload().await;
            return self.auth_cached();
        }

        let auth = self.auth_cached()?;
        if Self::should_refresh_proactively(&auth)
            && let Err(err) = self.refresh_token().await
        {
            tracing::error!("Failed to refresh token: {}", err);
            // D171: a named account removed meanwhile is not used.
            if self.named_account.is_some() {
                return self.auth_cached();
            }
            return Some(auth);
        }
        self.auth_cached()
    }

    pub async fn agent_identity_auth(
        &self,
        policy: AgentIdentityAuthPolicy,
        session_source: SessionSource,
    ) -> std::io::Result<Option<AgentIdentityAuth>> {
        let Some(auth) = self.auth().await else {
            return Ok(None);
        };
        if policy == AgentIdentityAuthPolicy::ChatGptAuth && matches!(auth, CodexAuth::Chatgpt(_)) {
            let _bootstrap_permit = self
                .agent_identity_lock
                .acquire()
                .await
                .map_err(std::io::Error::other)?;
            let forced_chatgpt_workspace_id = self.forced_chatgpt_workspace_id();
            let cooldown_key = ManagedChatGptAgentIdentityBinding::from_auth(
                &auth,
                forced_chatgpt_workspace_id.clone(),
            )
            .and_then(|binding| {
                self.agent_identity_authapi_base_url
                    .as_ref()
                    .map(|base_url| (binding.account_id, base_url.clone()))
            });
            if let Some((account_id, authapi_base_url)) = cooldown_key.as_ref()
                && let Ok(mut cooldown) = self.agent_identity_bootstrap_cooldown.lock()
                && let Some(error) =
                    cooldown.error_for(account_id, authapi_base_url, Instant::now())
            {
                tracing::warn!("agent identity bootstrap retry suppressed during shared cooldown");
                return Err(std::io::Error::other(error));
            }

            let result = auth
                .agent_identity_auth(
                    policy,
                    self.agent_identity_authapi_base_url.as_deref(),
                    forced_chatgpt_workspace_id,
                    &self.auth_route_config,
                    session_source,
                )
                .await;
            if let Ok(mut cooldown) = self.agent_identity_bootstrap_cooldown.lock() {
                if let (Err(err), Some((account_id, authapi_base_url))) = (&result, cooldown_key)
                    && let Some(error) = AgentIdentityAuthError::bootstrap_unavailable(err).cloned()
                {
                    cooldown.record_failure(account_id, authapi_base_url, error, Instant::now());
                } else {
                    cooldown.clear();
                }
            }
            return result;
        }
        auth.agent_identity_auth(
            policy,
            self.agent_identity_authapi_base_url.as_deref(),
            self.forced_chatgpt_workspace_id(),
            &self.auth_route_config,
            session_source,
        )
        .await
    }

    /// Reloads auth from the active source. Returns whether the auth value changed.
    pub async fn reload(&self) -> bool {
        tracing::info!("Reloading auth");
        let new_auth = self.load_auth().await;
        self.set_cached_auth(new_auth)
    }

    async fn reload_if_account_id_matches(
        &self,
        expected_account_id: Option<&str>,
    ) -> ReloadOutcome {
        let expected_account_id = match expected_account_id {
            Some(account_id) => account_id,
            None => {
                tracing::info!("Skipping auth reload because no account id is available.");
                return ReloadOutcome::Skipped;
            }
        };

        let new_auth = self.load_auth().await;
        let new_account_id = new_auth.as_ref().and_then(CodexAuth::get_account_id);

        if new_account_id.as_deref() != Some(expected_account_id) {
            let found_account_id = new_account_id.as_deref().unwrap_or("unknown");
            tracing::info!(
                "Skipping auth reload due to account id mismatch (expected: {expected_account_id}, found: {found_account_id})"
            );
            return ReloadOutcome::Skipped;
        }

        tracing::info!("Reloading auth for account {expected_account_id}");
        let cached_before_reload = self.auth_cached_unchecked();
        let auth_changed =
            !Self::auths_equal_for_refresh(cached_before_reload.as_ref(), new_auth.as_ref());
        self.set_cached_auth(new_auth);
        if auth_changed {
            ReloadOutcome::ReloadedChanged
        } else {
            ReloadOutcome::ReloadedNoChange
        }
    }

    fn auths_equal_for_refresh(a: Option<&CodexAuth>, b: Option<&CodexAuth>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => match (a.api_auth_mode(), b.api_auth_mode()) {
                (AuthMode::ApiKey, AuthMode::ApiKey) => a.api_key() == b.api_key(),
                (AuthMode::Chatgpt, AuthMode::Chatgpt)
                | (AuthMode::ChatgptAuthTokens, AuthMode::ChatgptAuthTokens) => {
                    a.get_current_auth_json() == b.get_current_auth_json()
                }
                (AuthMode::Headers, AuthMode::Headers) => a == b,
                (AuthMode::AgentIdentity, AuthMode::AgentIdentity) => match (a, b) {
                    (CodexAuth::AgentIdentity(a), CodexAuth::AgentIdentity(b)) => {
                        a.record() == b.record()
                    }
                    _ => false,
                },
                (AuthMode::PersonalAccessToken, AuthMode::PersonalAccessToken) => a == b,
                (AuthMode::BedrockApiKey, AuthMode::BedrockApiKey) => a == b,
                _ => false,
            },
            _ => false,
        }
    }

    fn auths_equal(a: Option<&CodexAuth>, b: Option<&CodexAuth>) -> bool {
        match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    /// Records a permanent refresh failure only if the failed refresh was
    /// attempted against the auth snapshot that is still cached.
    fn record_permanent_refresh_failure_if_unchanged(
        &self,
        attempted_auth: &CodexAuth,
        error: &RefreshTokenFailedError,
    ) {
        if let Ok(mut guard) = self.inner.write() {
            let current_auth_matches =
                Self::auths_equal_for_refresh(Some(attempted_auth), guard.auth.as_ref());
            if current_auth_matches {
                guard.permanent_refresh_failure = Some(AuthScopedRefreshFailure {
                    auth: attempted_auth.clone(),
                    error: error.clone(),
                });
            }
        }
    }

    async fn load_auth(&self) -> Option<CodexAuth> {
        if self.named_account.is_some() {
            return self.load_named_account_auth().await;
        }
        if let Some(external_auth) = self.external_auth() {
            self.clear_hidden_default_login_reason();
            return match self.resolve_external_auth(&external_auth).await {
                Ok(auth) => Some(auth),
                Err(err) => {
                    tracing::error!("Failed to resolve external auth: {err}");
                    None
                }
            };
        }

        let forced_chatgpt_workspace_id = self.forced_chatgpt_workspace_id();
        let auth = load_auth_with_openai_api_key_env(
            &self.codex_home,
            self.enable_codex_api_key_env,
            self.openai_api_key_env,
            self.auth_credentials_store_mode,
            forced_chatgpt_workspace_id.as_deref(),
            self.chatgpt_base_url.as_deref(),
            self.keyring_backend_kind,
            self.agent_identity_authapi_base_url.as_deref(),
            &self.auth_route_config,
        )
        .await
        .ok()
        .flatten();
        self.without_restricted_default_login(auth)
    }

    /// PF-84-S06 (#570): whether this default-login manager treats a login
    /// the restrictions rule out as absent, because the process left it in
    /// place at startup (see [`leave_default_login_unenforced`]).
    pub fn hides_restricted_default_login(&self) -> bool {
        self.hide_restricted_default_login && self.named_account.is_none()
    }

    /// PF-84-S06 (#570): why the default login this manager hides cannot be
    /// used, while it hides one. Requests on the default login are refused
    /// with it instead of being sent without credentials.
    pub fn hidden_default_login_reason(&self) -> Option<String> {
        self.hidden_default_login_reason
            .read()
            .ok()
            .and_then(|reason| reason.clone())
    }

    /// PF-84-S06 (#570): drops a default login the login restrictions rule
    /// out when this process left it in place for a named-account session.
    fn without_restricted_default_login(&self, auth: Option<CodexAuth>) -> Option<CodexAuth> {
        if !self.hides_restricted_default_login() {
            return auth;
        }
        let forced_workspace = self.forced_chatgpt_workspace_id();
        let reason = auth.as_ref().and_then(|auth| {
            login_restriction_violation(auth, self.forced_login_method, forced_workspace.as_deref())
                .map(|violation| default_login_unusable_message(violation.reason()))
        });
        if let Some(reason) = &reason {
            tracing::warn!("the default login is not used in this process: {reason}");
        }
        let hidden = reason.is_some();
        if let Ok(mut current) = self.hidden_default_login_reason.write() {
            *current = reason;
        }
        if hidden { None } else { auth }
    }

    fn set_cached_auth(&self, new_auth: Option<CodexAuth>) -> bool {
        if let Ok(mut guard) = self.inner.write() {
            self.replace_cached_auth(&mut guard, new_auth)
        } else {
            false
        }
    }

    /// Replaces auth while the caller holds the cache write lock, including
    /// invalidation and notification for every guarded cache update.
    fn replace_cached_auth(&self, cached: &mut CachedAuth, new_auth: Option<CodexAuth>) -> bool {
        let previous = cached.auth.as_ref();
        let changed = !AuthManager::auths_equal(previous, new_auth.as_ref());
        let auth_changed_for_refresh = !Self::auths_equal_for_refresh(previous, new_auth.as_ref());
        self.clear_provider_api_key_cache();
        if auth_changed_for_refresh {
            cached.permanent_refresh_failure = None;
        }
        tracing::info!("Reloaded auth, changed: {changed}");
        cached.auth = new_auth;
        // #1326: every cache installation invalidates earlier observations,
        // including a reinstall of the same credentials after removal.
        // Equality still controls the return value and refresh notifications.
        if let Some(store) = self.named_store.as_ref() {
            store.forget();
        }
        if auth_changed_for_refresh {
            self.auth_change_tx.send_modify(|revision| *revision += 1);
        }
        changed
    }

    pub async fn set_external_auth(
        &self,
        external_auth: Arc<dyn ExternalAuth>,
    ) -> Result<(), RefreshTokenError> {
        let auth = self.resolve_external_auth(&external_auth).await?;
        *self.external_auth.write().map_err(|_| {
            RefreshTokenError::Transient(std::io::Error::other("external auth lock is poisoned"))
        })? = Some(external_auth);
        self.commit_external_auth(auth)
    }

    pub fn clear_external_auth(&self) {
        if let Ok(mut external_auth) = self.external_auth.write()
            && external_auth.take().is_some()
        {
            self.clear_hidden_default_login_reason();
            self.set_cached_auth(/*new_auth*/ None);
        }
    }

    fn clear_hidden_default_login_reason(&self) {
        if let Ok(mut reason) = self.hidden_default_login_reason.write() {
            *reason = None;
        }
    }

    pub fn set_forced_chatgpt_workspace_id(&self, workspace_id: Option<Vec<String>>) {
        if let Ok(mut guard) = self.forced_chatgpt_workspace_id.write()
            && *guard != workspace_id
        {
            *guard = workspace_id;
        }
    }

    pub fn forced_chatgpt_workspace_id(&self) -> Option<Vec<String>> {
        self.forced_chatgpt_workspace_id
            .read()
            .ok()
            .and_then(|guard| guard.clone())
    }

    pub fn has_external_auth(&self) -> bool {
        self.external_auth().is_some()
    }

    pub fn is_external_chatgpt_auth_active(&self) -> bool {
        self.auth_cached()
            .as_ref()
            .is_some_and(CodexAuth::is_external_chatgpt_tokens)
    }

    pub fn codex_api_key_env_enabled(&self) -> bool {
        self.enable_codex_api_key_env
    }

    /// Convenience constructor returning an `Arc` wrapper.
    pub async fn shared(
        codex_home: PathBuf,
        enable_codex_api_key_env: bool,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
        forced_chatgpt_workspace_id: Option<Vec<String>>,
        chatgpt_base_url: Option<String>,
        keyring_backend_kind: AuthKeyringBackendKind,
        auth_route_config: AuthRouteConfig,
    ) -> Arc<Self> {
        Arc::new(
            Self::new(
                codex_home,
                enable_codex_api_key_env,
                auth_credentials_store_mode,
                forced_chatgpt_workspace_id,
                chatgpt_base_url,
                keyring_backend_kind,
                auth_route_config,
            )
            .await,
        )
    }

    /// Convenience constructor returning an `Arc` wrapper from resolved config.
    pub async fn shared_from_config(
        config: &impl AuthManagerConfig,
        enable_codex_api_key_env: bool,
    ) -> Arc<Self> {
        let mut manager = Self::new_with_openai_api_key_env(
            config.codex_home(),
            enable_codex_api_key_env,
            OpenAiApiKeyEnv::for_forced_login_method(config.forced_login_method()),
            config.cli_auth_credentials_store_mode(),
            config.forced_chatgpt_workspace_id(),
            Some(config.chatgpt_base_url()),
            config.auth_keyring_backend_kind(),
            config.auth_route_config(),
        )
        .await;
        manager.forced_login_method = config.configured_forced_login_method();
        manager.hide_restricted_default_login =
            config.runs_on_named_sign_in_account() || default_login_left_unenforced();
        if manager.hide_restricted_default_login
            && let Ok(mut cached) = manager.inner.write()
        {
            cached.auth = manager.without_restricted_default_login(cached.auth.take());
        }
        Arc::new(manager)
    }

    pub fn unauthorized_recovery(self: &Arc<Self>) -> UnauthorizedRecovery {
        UnauthorizedRecovery::new(Arc::clone(self))
    }

    fn external_auth(&self) -> Option<Arc<dyn ExternalAuth>> {
        self.external_auth
            .read()
            .ok()
            .and_then(|external_auth| external_auth.as_ref().map(Arc::clone))
    }

    fn has_external_api_key_auth(&self) -> bool {
        self.has_external_auth()
            && self
                .auth_cached()
                .as_ref()
                .is_some_and(CodexAuth::is_api_key_auth)
    }

    async fn resolve_external_auth(
        &self,
        external_auth: &Arc<dyn ExternalAuth>,
    ) -> Result<CodexAuth, RefreshTokenError> {
        let auth = external_auth
            .resolve()
            .await
            .map_err(RefreshTokenError::Transient)?;
        self.validate_external_auth(&auth)?;
        Ok(auth)
    }

    /// Attempt to refresh the token by first performing a guarded reload from
    /// the active auth source. If the loaded token differs from the cached token,
    /// we can assume that the source already refreshed it. Otherwise, ask the
    /// token authority to refresh.
    pub async fn refresh_token(&self) -> Result<(), RefreshTokenError> {
        let _refresh_guard = self.refresh_lock.acquire().await.map_err(|_| {
            RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                RefreshTokenFailedReason::Other,
                REFRESH_TOKEN_UNKNOWN_MESSAGE.to_string(),
            ))
        })?;
        // D171: as held, so another process's refresh is picked up below.
        let auth_before_reload = self.auth_cached_unchecked();
        if auth_before_reload
            .as_ref()
            .is_some_and(|auth| auth.is_api_key_auth() || auth.is_personal_access_token_auth())
        {
            return Ok(());
        }
        let expected_account_id = auth_before_reload
            .as_ref()
            .and_then(CodexAuth::get_account_id);

        match self
            .reload_if_account_id_matches(expected_account_id.as_deref())
            .await
        {
            ReloadOutcome::ReloadedChanged => {
                tracing::info!("Skipping token refresh because auth changed after guarded reload.");
                Ok(())
            }
            ReloadOutcome::ReloadedNoChange => self.refresh_token_from_authority_impl().await,
            ReloadOutcome::Skipped => {
                Err(RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                    RefreshTokenFailedReason::Other,
                    REFRESH_TOKEN_ACCOUNT_MISMATCH_MESSAGE.to_string(),
                )))
            }
        }
    }

    /// Attempt to refresh the current auth token from the authority that issued
    /// it and update the shared cache. If the token refresh fails, returns the
    /// error to the caller.
    pub async fn refresh_token_from_authority(&self) -> Result<(), RefreshTokenError> {
        let _refresh_guard = self.refresh_lock.acquire().await.map_err(|_| {
            RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                RefreshTokenFailedReason::Other,
                REFRESH_TOKEN_UNKNOWN_MESSAGE.to_string(),
            ))
        })?;
        self.refresh_token_from_authority_impl().await
    }

    async fn refresh_token_from_authority_impl(&self) -> Result<(), RefreshTokenError> {
        tracing::info!("Refreshing token");

        let auth = match self.auth_cached() {
            Some(auth) => auth,
            None => return Ok(()),
        };
        if let Some(error) = self.refresh_failure_for_auth(&auth) {
            return Err(RefreshTokenError::Permanent(error));
        }

        let attempted_auth = auth.clone();
        let result = if self.has_external_auth() {
            self.refresh_external_auth(ExternalAuthRefreshReason::Unauthorized)
                .await
        } else {
            match auth {
                CodexAuth::Chatgpt(chatgpt_auth) => {
                    let token_data = chatgpt_auth.current_token_data().ok_or_else(|| {
                        RefreshTokenError::Transient(std::io::Error::other(
                            "Token data is not available.",
                        ))
                    })?;
                    self.refresh_and_persist_chatgpt_token(&chatgpt_auth, token_data.refresh_token)
                        .await
                }
                CodexAuth::ApiKey(_)
                | CodexAuth::ChatgptAuthTokens(_)
                | CodexAuth::Headers(_)
                | CodexAuth::AgentIdentity(_)
                | CodexAuth::PersonalAccessToken(_)
                | CodexAuth::BedrockApiKey(_) => Ok(()),
            }
        };
        // PF-84-S06: a named account's refresh failure names how to recover it.
        let result = match (result, self.named_account.as_ref()) {
            (Err(RefreshTokenError::Permanent(error)), Some(account)) => {
                Err(RefreshTokenError::Permanent(RefreshTokenFailedError::new(
                    error.reason,
                    format!(
                        "{} (account `{}`: sign in again with `corbanu login --account {}`; \
                         the default login is unchanged)",
                        error.message, account.name, account.name
                    ),
                )))
            }
            (result, _) => result,
        };
        if let Err(RefreshTokenError::Permanent(error)) = &result {
            self.record_permanent_refresh_failure_if_unchanged(&attempted_auth, error);
        }
        result
    }

    /// Log out by deleting the on‑disk auth.json (if present). Returns Ok(true)
    /// if a file was removed, Ok(false) if no auth file existed. On success,
    /// reloads the in‑memory auth cache so callers immediately observe the
    /// unauthenticated state.
    pub async fn logout(&self) -> std::io::Result<bool> {
        if let Some(result) = self.logout_named_account(/*revoke*/ false).await {
            return result;
        }
        let removed = logout_all_stores(
            &self.codex_home,
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
            /*include_provider_credentials*/ false,
        )?;
        // Always reload to clear any cached auth (even if file absent).
        self.clear_external_auth();
        self.reload().await;
        Ok(removed)
    }

    pub async fn logout_all_credentials(&self) -> std::io::Result<bool> {
        if let Some(result) = self.logout_named_account(/*revoke*/ false).await {
            return result;
        }
        let removed = logout_all_stores(
            &self.codex_home,
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
            /*include_provider_credentials*/ true,
        )?;
        self.reload().await;
        Ok(removed)
    }

    pub async fn logout_with_revoke(&self) -> std::io::Result<bool> {
        if let Some(result) = self.logout_named_account(/*revoke*/ true).await {
            return result;
        }
        let auth_dot_json = self
            .auth_cached()
            .and_then(|auth| auth.get_current_auth_json());
        if let Err(err) = revoke_auth_tokens(auth_dot_json.as_ref(), &self.auth_route_config).await
        {
            tracing::warn!("failed to revoke auth tokens during logout: {err}");
        }
        let result = logout_all_stores(
            &self.codex_home,
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
            /*include_provider_credentials*/ false,
        )?;
        // Always reload to clear any cached auth (even if file absent).
        self.clear_external_auth();
        self.reload().await;
        Ok(result)
    }

    /// Returns the precise kind of credentials backing the current authentication.
    pub fn get_api_auth_mode(&self) -> Option<AuthMode> {
        self.auth_cached().as_ref().map(CodexAuth::api_auth_mode)
    }

    /// Returns the effective backend auth mode for the current authentication.
    pub fn auth_mode(&self) -> Option<AuthMode> {
        self.auth_cached().as_ref().map(CodexAuth::auth_mode)
    }

    /// Whether `OPENAI_API_KEY` is read as the last-resort OpenAI credential.
    pub fn openai_api_key_env_enabled(&self) -> bool {
        self.openai_api_key_env == OpenAiApiKeyEnv::Fallback
    }

    /// Inspect the primary OpenAI auth boundary without returning credential material.
    pub fn openai_auth_metadata(&self) -> OpenAiAuthMetadata {
        let Some(auth) = self.auth_cached() else {
            return OpenAiAuthMetadata::Missing;
        };
        if auth.api_key_env_var().is_some()
            || (self.enable_codex_api_key_env && auth_uses_api_key_from_env(&auth))
        {
            return OpenAiAuthMetadata::EnvironmentApiKey;
        }
        if auth.is_external_chatgpt_tokens() {
            return OpenAiAuthMetadata::ExternallyManaged;
        }
        if self.refresh_failure_for_auth(&auth).is_some() {
            return OpenAiAuthMetadata::RecoveryRequired;
        }
        match auth.api_auth_mode() {
            AuthMode::Chatgpt | AuthMode::ChatgptAuthTokens | AuthMode::PersonalAccessToken => {
                OpenAiAuthMetadata::Account
            }
            AuthMode::ApiKey => OpenAiAuthMetadata::ApiKey,
            AuthMode::Headers | AuthMode::AgentIdentity => OpenAiAuthMetadata::ExternallyManaged,
            AuthMode::BedrockApiKey => OpenAiAuthMetadata::Unsupported,
        }
    }

    /// PF-84: the API key of one named account (vault only, uncached, no
    /// environment or default-account fallback).
    pub fn provider_account_api_key(
        &self,
        provider_id: &str,
        account_name: &str,
    ) -> std::io::Result<Option<String>> {
        super::provider_key_vault::provider_account_api_key(
            &self.codex_home,
            provider_id,
            account_name,
        )
    }

    pub fn provider_api_key(&self, provider_key_id: &str) -> std::io::Result<Option<String>> {
        let storage_revision = provider_api_key_storage_revision();
        if let Some(cached) = self.provider_api_key_cache.read().ok().and_then(|cache| {
            (cache.storage_revision == storage_revision)
                .then(|| cache.entries.get(provider_key_id).cloned())
                .flatten()
        }) {
            return Ok(cached);
        }

        let key = provider_api_key_from_auth_storage(
            &self.codex_home,
            provider_key_id,
            self.auth_credentials_store_mode,
            self.keyring_backend_kind,
        )?;
        if let Ok(mut cache) = self.provider_api_key_cache.write() {
            if cache.storage_revision != storage_revision {
                cache.storage_revision = storage_revision;
                cache.entries.clear();
            }
            cache
                .entries
                .insert(provider_key_id.to_string(), key.clone());
        }
        Ok(key)
    }

    fn clear_provider_api_key_cache(&self) {
        if let Ok(mut cache) = self.provider_api_key_cache.write() {
            cache.clear_to_current_revision();
        }
    }

    pub fn current_auth_uses_codex_backend(&self) -> bool {
        self.get_api_auth_mode()
            .is_some_and(AuthMode::uses_codex_backend)
    }

    fn should_refresh_proactively(auth: &CodexAuth) -> bool {
        let chatgpt_auth = match auth {
            CodexAuth::Chatgpt(chatgpt_auth) => chatgpt_auth,
            _ => return false,
        };

        let auth_dot_json = match chatgpt_auth.current_auth_json() {
            Some(auth_dot_json) => auth_dot_json,
            None => return false,
        };
        if let Some(tokens) = auth_dot_json.tokens.as_ref()
            && let Ok(Some(expires_at)) = parse_jwt_expiration(&tokens.access_token)
        {
            return expires_at
                <= Utc::now()
                    + chrono::Duration::minutes(CHATGPT_ACCESS_TOKEN_REFRESH_WINDOW_MINUTES);
        }
        let last_refresh = match auth_dot_json.last_refresh {
            Some(last_refresh) => last_refresh,
            None => return false,
        };
        last_refresh < Utc::now() - chrono::Duration::days(TOKEN_REFRESH_INTERVAL)
    }

    async fn refresh_external_auth(
        &self,
        reason: ExternalAuthRefreshReason,
    ) -> Result<(), RefreshTokenError> {
        let Some(external_auth) = self.external_auth() else {
            return Err(RefreshTokenError::Transient(std::io::Error::other(
                "external auth is not configured",
            )));
        };
        let previous_account_id = self
            .auth_cached()
            .as_ref()
            .and_then(CodexAuth::get_account_id);
        let context = ExternalAuthRefreshContext {
            reason,
            previous_account_id,
        };

        let refreshed = external_auth
            .refresh(context)
            .await
            .map_err(RefreshTokenError::Transient)?;
        self.validate_external_auth(&refreshed)?;
        self.commit_external_auth(refreshed)?;
        Ok(())
    }

    fn commit_external_auth(&self, auth: CodexAuth) -> Result<(), RefreshTokenError> {
        if auth.is_external_chatgpt_tokens() {
            let auth_dot_json = auth.get_current_auth_json().ok_or_else(|| {
                RefreshTokenError::Transient(std::io::Error::other(
                    "external ChatGPT auth tokens are missing auth state",
                ))
            })?;
            // App/connectors paths still construct independent AuthManagers from Config. Mirror
            // external ChatGPT auth into the process-local store so those managers see it too.
            save_auth(
                &self.codex_home,
                &auth_dot_json,
                AuthCredentialsStoreMode::Ephemeral,
                AuthKeyringBackendKind::default(),
            )
            .map_err(RefreshTokenError::Transient)?;
        }

        self.clear_hidden_default_login_reason();
        self.set_cached_auth(Some(auth));
        Ok(())
    }

    fn validate_external_auth(&self, auth: &CodexAuth) -> Result<(), RefreshTokenError> {
        if let Some(account_id) = auth.get_account_id()
            && let Some(expected_workspace_ids) = self.forced_chatgpt_workspace_id()
            && !expected_workspace_ids.contains(&account_id)
        {
            return Err(RefreshTokenError::Transient(std::io::Error::other(
                format!(
                    "external auth returned workspace {account_id:?}, expected one of {expected_workspace_ids:?}"
                ),
            )));
        }
        Ok(())
    }

    // Refreshes ChatGPT OAuth tokens, persists the updated auth state, and
    // reloads the in-memory cache so callers immediately observe new tokens.
    async fn refresh_and_persist_chatgpt_token(
        &self,
        auth: &ChatgptAuth,
        refresh_token: String,
    ) -> Result<(), RefreshTokenError> {
        let refresh_response =
            request_chatgpt_token_refresh(refresh_token.clone(), auth.client()).await?;

        persist_tokens(
            auth.storage(),
            &refresh_token,
            refresh_response.id_token,
            refresh_response.access_token,
            refresh_response.refresh_token,
        )
        .map_err(RefreshTokenError::from)?;
        self.reload().await;

        Ok(())
    }
}

#[path = "manager_named_account.rs"]
mod named_account;

#[path = "manager_session_account.rs"]
mod session_account;
pub use session_account::SessionSignIn;
pub use session_account::SessionSignInRegistration;

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
