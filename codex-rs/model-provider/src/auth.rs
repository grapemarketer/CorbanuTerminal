use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use codex_agent_identity::AgentIdentityKey;
use codex_agent_identity::authorization_header_for_agent_task;
use codex_api::AgentIdentityTelemetry;
use codex_api::AuthProvider;
use codex_api::SharedAuthProvider;
use codex_login::AuthHeaders;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::ExternalBearerCachePolicy;
use codex_login::auth::AgentIdentityAuth;
use codex_login::auth::AgentIdentityAuthError;
use codex_login::auth::AgentIdentityAuthPolicy;
use codex_login::claude_auth_selection_revision_path;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::error::CodexErr;
use codex_protocol::protocol::SessionSource;
use http::HeaderMap;
use http::HeaderName;
use http::HeaderValue;

use crate::bearer_auth_provider::BearerAuthProvider;

const BEDROCK_API_KEY_UNSUPPORTED_MESSAGE: &str =
    "Bedrock API key auth is only supported by the Amazon Bedrock model provider";

#[derive(Clone, Debug)]
pub struct ProviderAuthScope {
    pub agent_identity_policy: AgentIdentityAuthPolicy,
    pub session_source: SessionSource,
    pub agent_identity_session_fallback: AgentIdentitySessionFallback,
}

#[derive(Clone, Debug, Default)]
pub struct AgentIdentitySessionFallback {
    engaged: Arc<AtomicBool>,
}

impl AgentIdentitySessionFallback {
    pub fn is_engaged(&self) -> bool {
        self.engaged.load(Ordering::Relaxed)
    }

    fn engage(&self) -> bool {
        !self.engaged.swap(true, Ordering::Relaxed)
    }
}

/// Provider auth resolved for a request, plus metadata describing the effective auth.
#[derive(Clone)]
pub struct ResolvedProviderAuth {
    pub auth: SharedAuthProvider,
    pub agent_identity_telemetry: Option<AgentIdentityTelemetry>,
}

impl ResolvedProviderAuth {
    pub(crate) fn new(auth: SharedAuthProvider) -> Self {
        Self {
            auth,
            agent_identity_telemetry: None,
        }
    }

    fn for_agent_identity(auth: AgentIdentityAuth) -> Self {
        let agent_identity_telemetry = agent_identity_telemetry(&auth);
        Self {
            auth: Arc::new(AgentIdentityAuthProvider { auth }),
            agent_identity_telemetry: Some(agent_identity_telemetry),
        }
    }
}

pub(crate) fn agent_identity_telemetry(auth: &AgentIdentityAuth) -> AgentIdentityTelemetry {
    AgentIdentityTelemetry {
        agent_id: auth.record().agent_runtime_id.clone(),
        task_id: auth.run_task_id().to_string(),
    }
}

#[derive(Clone, Debug)]
struct AgentIdentityAuthProvider {
    auth: AgentIdentityAuth,
}

#[derive(Clone)]
struct ApiKeyHeaderAuthProvider {
    header_name: HeaderName,
    api_key: String,
}

impl std::fmt::Debug for ApiKeyHeaderAuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKeyHeaderAuthProvider")
            .field("header_name", &self.header_name)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl ApiKeyHeaderAuthProvider {
    fn new(header_name: &'static str, api_key: String) -> codex_protocol::error::Result<Self> {
        Ok(Self {
            header_name: HeaderName::from_static(header_name),
            api_key,
        })
    }
}

impl AuthProvider for ApiKeyHeaderAuthProvider {
    fn add_auth_headers(&self, headers: &mut HeaderMap) {
        if let Ok(mut header) = HeaderValue::from_str(&self.api_key) {
            header.set_sensitive(true);
            let _ = headers.insert(self.header_name.clone(), header);
        }
    }
}

impl AuthProvider for AgentIdentityAuthProvider {
    fn add_auth_headers(&self, headers: &mut HeaderMap) {
        let record = self.auth.record();
        let header_value = authorization_header_for_agent_task(
            AgentIdentityKey {
                agent_runtime_id: &record.agent_runtime_id,
                private_key_pkcs8_base64: &record.agent_private_key,
            },
            self.auth.run_task_id(),
        )
        .map_err(std::io::Error::other);

        if let Ok(header_value) = header_value
            && let Ok(mut header) = HeaderValue::from_str(&header_value)
        {
            header.set_sensitive(true);
            let _ = headers.insert(http::header::AUTHORIZATION, header);
        }

        if let Ok(header) = HeaderValue::from_str(self.auth.account_id()) {
            let _ = headers.insert("ChatGPT-Account-ID", header);
        }

        if self.auth.is_fedramp_account() {
            let _ = headers.insert("X-OpenAI-Fedramp", HeaderValue::from_static("true"));
        }
    }
}

#[derive(Clone, Debug)]
struct HeaderAuthProvider {
    auth: AuthHeaders,
}

impl AuthProvider for HeaderAuthProvider {
    fn add_auth_headers(&self, headers: &mut HeaderMap) {
        headers.extend(self.auth.headers().clone());
    }
}

struct AuthManagerAuthProvider {
    auth_manager: Arc<AuthManager>,
    // Startup auth is only the account-scoped identity anchor. Request
    // headers always come from the current AuthManager snapshot below.
    expected_auth: CodexAuth,
}

impl AuthProvider for AuthManagerAuthProvider {
    fn add_auth_headers(&self, headers: &mut HeaderMap) {
        let Some(auth) = self
            .auth_manager
            .auth_cached()
            .filter(CodexAuth::uses_codex_backend)
        else {
            return;
        };
        // The caller's account-scoped state was built for the expected
        // identity. Follow token refreshes for that identity, but never cross
        // an account or workspace boundary without rebuilding that state.
        if auth.get_account_id() != self.expected_auth.get_account_id()
            || auth.get_chatgpt_user_id() != self.expected_auth.get_chatgpt_user_id()
            || auth.is_workspace_account() != self.expected_auth.is_workspace_account()
        {
            return;
        }
        auth_provider_from_auth(&auth).add_auth_headers(headers);
    }
}

// Some providers are meant to send no auth headers. Examples include local OSS
// providers and custom test providers with `requires_openai_auth = false`.
#[derive(Clone, Debug)]
struct UnauthenticatedAuthProvider;

impl AuthProvider for UnauthenticatedAuthProvider {
    fn add_auth_headers(&self, _headers: &mut HeaderMap) {}
}

pub fn unauthenticated_auth_provider() -> SharedAuthProvider {
    Arc::new(UnauthenticatedAuthProvider)
}

/// Returns the provider-scoped auth manager when this provider uses command-backed auth.
///
/// Providers without custom auth continue using the caller-supplied base manager, when present.
pub(crate) fn auth_manager_for_provider(
    auth_manager: Option<Arc<AuthManager>>,
    provider: &ModelProviderInfo,
) -> Option<Arc<AuthManager>> {
    let cache_policy = external_bearer_cache_policy(auth_manager.as_deref(), provider);
    match provider.auth.clone() {
        Some(config) => {
            let account = external_bearer_account(
                provider,
                auth_manager.as_deref().map(AuthManager::codex_home),
            );
            Some(AuthManager::external_bearer_only_for_account(
                config,
                cache_policy,
                account,
            ))
        }
        // PF-84-S06: a named sign-in account uses its own manager (its own
        // vault store and refresh lock), never the default login.
        None => match named_sign_in_account(provider) {
            Some(account) => {
                auth_manager.map(|auth_manager| auth_manager.for_named_account(account))
            }
            // A session swapped to a named account's manager must not lend
            // that account to an OpenAI-auth provider on its default login.
            None if provider.requires_openai_auth => {
                auth_manager.map(|auth_manager| auth_manager.default_account_manager())
            }
            None => auth_manager,
        },
    }
}

/// D171 (#1286): why a request on `provider`'s named OpenAI sign-in account
/// that found no sign-in is refused, with the account's recovery; `None` when
/// `provider` selects no such account. Reads the vault through
/// `auth_manager`.
pub fn named_sign_in_refusal(
    provider: &ModelProviderInfo,
    auth_manager: Option<&AuthManager>,
) -> Option<CodexErr> {
    let account = named_sign_in_account(provider)?;
    let reason = auth_manager
        .and_then(AuthManager::named_sign_in_unavailable)
        .unwrap_or_else(|| {
            format!(
                "account `{name}` has no usable sign-in; sign in again with \
                 `corbanu login --account {name}`",
                name = account.name
            )
        });
    Some(CodexErr::Fatal(format!("{}: {reason}", provider.name)))
}

/// PF-84-S06: the named ChatGPT/OpenAI sign-in account `provider` selects.
pub fn named_sign_in_account(
    provider: &ModelProviderInfo,
) -> Option<codex_login::NamedChatgptAccount> {
    if !provider.requires_openai_auth || provider.aws.is_some() || provider.auth.is_some() {
        return None;
    }
    let account = provider.account.as_ref()?;
    Some(codex_login::NamedChatgptAccount {
        provider_id: account.provider_id.clone(),
        name: account.name.clone(),
    })
}

/// PF-84-S06 (#658): the OpenAI sign-in a session on `provider` runs on: a
/// named account, the default login, or none (a provider without OpenAI
/// sign-in).
pub fn session_sign_in(provider: &ModelProviderInfo) -> codex_login::SessionSignIn {
    if !provider.requires_openai_auth || provider.aws.is_some() || provider.auth.is_some() {
        return codex_login::SessionSignIn::NotOpenAi;
    }
    match named_sign_in_account(provider) {
        Some(account) => codex_login::SessionSignIn::Named(account),
        None => codex_login::SessionSignIn::Default,
    }
}

/// PF-84: the named account `provider`'s auth command must serve, checked
/// against the registry in `registry_home` before the command runs.
pub(crate) fn external_bearer_account(
    provider: &ModelProviderInfo,
    registry_home: Option<&std::path::Path>,
) -> Option<codex_login::auth::ExternalBearerAccount> {
    let account = provider.account.as_ref()?;
    Some(codex_login::auth::ExternalBearerAccount {
        provider_id: account.provider_id.clone(),
        name: account.name.clone(),
        registry_home: registry_home.map(std::path::Path::to_path_buf),
        server: if provider.is_claude_plan()
            && provider
                .auth
                .as_ref()
                .is_some_and(codex_login::auth::is_claude_plan_token_helper)
        {
            codex_login::auth::ExternalBearerAccountServer::ClaudePlanHelper
        } else {
            codex_login::auth::ExternalBearerAccountServer::Command
        },
    })
}

fn external_bearer_cache_policy(
    auth_manager: Option<&AuthManager>,
    provider: &ModelProviderInfo,
) -> ExternalBearerCachePolicy {
    if provider.is_claude_plan() {
        // Bind the cache to the persisted source revision. A missing marker is
        // intentionally uncached for compatibility with legacy installations.
        auth_manager.map_or(ExternalBearerCachePolicy::FreshPerRequest, |manager| {
            ExternalBearerCachePolicy::InvalidateOnChange(claude_auth_selection_revision_path(
                manager.codex_home(),
            ))
        })
    } else {
        ExternalBearerCachePolicy::Timed
    }
}

pub(crate) fn resolve_provider_auth(
    auth: Option<&CodexAuth>,
    provider: &ModelProviderInfo,
) -> codex_protocol::error::Result<SharedAuthProvider> {
    if matches!(auth, Some(CodexAuth::BedrockApiKey(_))) {
        return Err(CodexErr::UnsupportedOperation(
            BEDROCK_API_KEY_UNSUPPORTED_MESSAGE.to_string(),
        ));
    }

    // PF-27-S05: in a brokered process, plain keys and sign-in tokens are
    // attached by the broker only; before the broker is running they are not
    // attached at all.
    if crate::model_key_broker::model_key_broker_required()
        && let Some(request) = brokered_auth_request(auth, provider)?
    {
        return match crate::model_key_broker::model_key_broker() {
            Some(broker) => broker.auth(request),
            None => Err(CodexErr::Fatal(
                crate::model_key_broker::BROKER_NOT_RUNNING.to_string(),
            )),
        };
    }

    if provider.env_key.is_some()
        && let Some(auth) = auth
    {
        return auth_provider_from_provider_key_auth(auth, provider);
    }

    if let Some(auth) = bearer_auth_for_provider(provider)? {
        return Ok(auth);
    }

    Ok(match auth {
        // Command and AWS auth are not brokered: sent as before.
        Some(auth) if provider.auth.is_some() || provider.aws.is_some() => {
            direct_auth_provider_from_auth(auth)
        }
        Some(auth) => auth_provider_from_auth(auth),
        None => unauthenticated_auth_provider(),
    })
}

pub(crate) async fn resolve_provider_auth_for_scope(
    auth_manager: Option<Arc<AuthManager>>,
    auth: Option<&CodexAuth>,
    provider: &ModelProviderInfo,
    scope: ProviderAuthScope,
) -> codex_protocol::error::Result<ResolvedProviderAuth> {
    let ProviderAuthScope {
        agent_identity_policy,
        session_source,
        agent_identity_session_fallback,
    } = scope;
    if let Some(CodexAuth::AgentIdentity(agent_identity_auth)) = auth {
        return Ok(ResolvedProviderAuth::for_agent_identity(
            agent_identity_auth.clone(),
        ));
    }

    if !should_bootstrap_chatgpt_agent_identity(agent_identity_policy, auth)
        || agent_identity_session_fallback.is_engaged()
    {
        return resolve_provider_auth(auth, provider).map(ResolvedProviderAuth::new);
    }

    let Some(auth_manager) = auth_manager else {
        return resolve_provider_auth(auth, provider).map(ResolvedProviderAuth::new);
    };

    match auth_manager
        .agent_identity_auth(agent_identity_policy, session_source)
        .await
    {
        Ok(Some(agent_identity_auth)) => Ok(ResolvedProviderAuth::for_agent_identity(
            agent_identity_auth,
        )),
        Ok(None) => resolve_provider_auth(auth, provider).map(ResolvedProviderAuth::new),
        Err(err) => {
            if let Some(AgentIdentityAuthError::BootstrapUnavailable {
                operation,
                attempts,
                message,
            }) = err
                .get_ref()
                .and_then(|source| source.downcast_ref::<AgentIdentityAuthError>())
            {
                let newly_engaged = agent_identity_session_fallback.engage();
                tracing::warn!(
                    operation,
                    attempts = *attempts,
                    error = %message,
                    newly_engaged,
                    "agent identity bootstrap unavailable; using ChatGPT bearer auth for this session"
                );
                resolve_provider_auth(auth, provider).map(ResolvedProviderAuth::new)
            } else {
                Err(err.into())
            }
        }
    }
}

/// PF-27-S05: the brokered form of what [`resolve_provider_auth`] would
/// attach, or `None` for auth that is not brokered.
fn brokered_auth_request(
    auth: Option<&CodexAuth>,
    provider: &ModelProviderInfo,
) -> codex_protocol::error::Result<Option<crate::model_key_broker::BrokeredAuthRequest>> {
    use crate::model_key_broker::BrokeredAuthRequest;
    use crate::model_key_broker::BrokeredKeySource;
    if provider.aws.is_some() || provider.auth.is_some() {
        return Ok(None);
    }
    let header = if provider.api_key_header_name().is_some() {
        ProviderApiKeyHeader::XApiKey
    } else {
        ProviderApiKeyHeader::Bearer
    };
    let base_url = provider
        .to_api_provider(auth.map(CodexAuth::auth_mode))?
        .base_url;
    let request = |header, source, extra_headers| {
        Ok(Some(BrokeredAuthRequest {
            base_url: base_url.clone(),
            header,
            source,
            extra_headers,
        }))
    };
    if let Some(provider_key_id) = provider.env_key.clone() {
        // Core never reads this key; `auth` is at most the placeholder. A
        // sign-in login used for this provider is brokered below with the
        // provider's header, as direct auth would send it.
        if matches!(auth, None | Some(CodexAuth::ApiKey(_))) {
            // PF-84: a named account's key comes only from that account.
            let env_vars = if provider.account.is_some() {
                Vec::new()
            } else {
                provider
                    .api_key_env_vars()
                    .into_iter()
                    .map(str::to_string)
                    .collect()
            };
            return request(
                header,
                BrokeredKeySource::ProviderKey {
                    provider_key_id,
                    env_vars,
                    account: provider.account.clone(),
                },
                HeaderMap::new(),
            );
        }
    } else if let Some(token) = provider.experimental_bearer_token.clone() {
        let key = ProviderApiKey {
            value: token,
            header: ProviderApiKeyHeader::Bearer,
        };
        return request(
            ProviderApiKeyHeader::Bearer,
            BrokeredKeySource::Value {
                key,
                slot: Some("experimental-bearer-token".to_string()),
            },
            HeaderMap::new(),
        );
    }
    let Some(auth) = auth else {
        return Ok(None);
    };
    // One stable slot per login: a refreshed token or a new key replaces the
    // broker's previous copy instead of adding another.
    let slot = match auth {
        CodexAuth::ApiKey(_) => "api-key-login".to_string(),
        CodexAuth::Chatgpt(_)
        | CodexAuth::ChatgptAuthTokens(_)
        | CodexAuth::PersonalAccessToken(_) => {
            format!("sign-in:{}", auth.get_account_id().unwrap_or_default())
        }
        CodexAuth::AgentIdentity(_) | CodexAuth::Headers(_) | CodexAuth::BedrockApiKey(_) => {
            return Ok(None);
        }
    };
    let Ok(token) = auth.get_token() else {
        return Ok(None);
    };
    // An env-key provider sends the login in its own header, alone; otherwise
    // the non-secret headers direct auth sends go with the bearer token.
    let (header, extra_headers) = if provider.env_key.is_some() {
        (header, HeaderMap::new())
    } else {
        let mut extra_headers = direct_auth_provider_from_auth(auth).to_auth_headers();
        extra_headers.remove(http::header::AUTHORIZATION);
        (ProviderApiKeyHeader::Bearer, extra_headers)
    };
    let key = ProviderApiKey {
        value: token,
        header,
    };
    request(
        header,
        BrokeredKeySource::Value {
            key,
            slot: Some(slot),
        },
        extra_headers,
    )
}

/// PF-27-S05: header a plain provider API key is sent in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderApiKeyHeader {
    /// `Authorization: Bearer <key>`.
    Bearer,
    /// `x-api-key: <key>`.
    XApiKey,
}

/// PF-27-S05: a plain API key that [`resolve_provider_auth`] would attach as
/// a single request header, so it can be handed to a credential broker.
pub struct ProviderApiKey {
    pub value: String,
    pub header: ProviderApiKeyHeader,
}

impl std::fmt::Debug for ProviderApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderApiKey")
            .field("value", &"<redacted>")
            .field("header", &self.header)
            .finish()
    }
}

/// Returns the provider's API key when its auth is exactly one key in one
/// header (environment or stored provider key, `experimental_bearer_token`,
/// or an OpenAI API-key login). Sign-in tokens, agent identity, command or
/// header auth and AWS auth return `None`: they are not brokered.
pub fn provider_api_key(
    auth: Option<&CodexAuth>,
    provider: &ModelProviderInfo,
) -> codex_protocol::error::Result<Option<ProviderApiKey>> {
    if provider.aws.is_some()
        || provider.auth.is_some()
        || matches!(auth, Some(CodexAuth::BedrockApiKey(_)))
    {
        return Ok(None);
    }
    let header = if provider.api_key_header_name().is_some() {
        ProviderApiKeyHeader::XApiKey
    } else {
        ProviderApiKeyHeader::Bearer
    };
    let key = |value: String| ProviderApiKey { value, header };
    if provider.env_key.is_some()
        && let Some(auth) = auth
    {
        return Ok(match auth {
            CodexAuth::ApiKey(_) => auth.get_token().ok().map(key),
            _ => None,
        });
    }
    if let Some(api_key) = provider.api_key()? {
        return Ok(Some(key(api_key)));
    }
    if let Some(token) = provider.experimental_bearer_token.clone() {
        return Ok(Some(ProviderApiKey {
            value: token,
            header: ProviderApiKeyHeader::Bearer,
        }));
    }
    Ok(match auth {
        Some(CodexAuth::ApiKey(_)) => {
            auth.and_then(|auth| auth.get_token().ok())
                .map(|value| ProviderApiKey {
                    value,
                    header: ProviderApiKeyHeader::Bearer,
                })
        }
        _ => None,
    })
}

fn should_bootstrap_chatgpt_agent_identity(
    agent_identity_policy: AgentIdentityAuthPolicy,
    auth: Option<&CodexAuth>,
) -> bool {
    agent_identity_policy == AgentIdentityAuthPolicy::ChatGptAuth
        && matches!(auth, Some(CodexAuth::Chatgpt(_)))
}

fn bearer_auth_for_provider(
    provider: &ModelProviderInfo,
) -> codex_protocol::error::Result<Option<SharedAuthProvider>> {
    if let Some(api_key) = provider.api_key()? {
        return api_key_auth_provider(provider, api_key).map(Some);
    }

    if let Some(token) = provider.experimental_bearer_token.clone() {
        return Ok(Some(Arc::new(BearerAuthProvider::new(token))));
    }

    Ok(None)
}

fn auth_provider_from_provider_key_auth(
    auth: &CodexAuth,
    provider: &ModelProviderInfo,
) -> codex_protocol::error::Result<SharedAuthProvider> {
    if provider.api_key_header_name().is_some()
        && let Ok(token) = auth.get_token()
    {
        return api_key_auth_provider(provider, token);
    }

    Ok(auth_provider_from_auth(auth))
}

fn api_key_auth_provider(
    provider: &ModelProviderInfo,
    api_key: String,
) -> codex_protocol::error::Result<SharedAuthProvider> {
    if let Some(header_name) = provider.api_key_header_name() {
        return Ok(Arc::new(ApiKeyHeaderAuthProvider::new(
            header_name,
            api_key,
        )?));
    }

    Ok(Arc::new(BearerAuthProvider::new(api_key)))
}

/// Builds request-header auth for a first-party Codex auth snapshot.
///
/// PF-27-S05: with a credential broker installed, an API key or sign-in token
/// is attached only by the broker, which header-only callers cannot use, so
/// they get no credential.
pub fn auth_provider_from_auth(auth: &CodexAuth) -> SharedAuthProvider {
    if crate::model_key_broker::model_key_broker_required()
        && matches!(
            auth,
            CodexAuth::ApiKey(_)
                | CodexAuth::Chatgpt(_)
                | CodexAuth::ChatgptAuthTokens(_)
                | CodexAuth::PersonalAccessToken(_)
        )
    {
        return unauthenticated_auth_provider();
    }
    direct_auth_provider_from_auth(auth)
}

fn direct_auth_provider_from_auth(auth: &CodexAuth) -> SharedAuthProvider {
    match auth {
        CodexAuth::AgentIdentity(auth) => {
            Arc::new(AgentIdentityAuthProvider { auth: auth.clone() })
        }
        CodexAuth::Headers(auth) => Arc::new(HeaderAuthProvider { auth: auth.clone() }),
        CodexAuth::BedrockApiKey(_) => unreachable!("{BEDROCK_API_KEY_UNSUPPORTED_MESSAGE}"),
        CodexAuth::ApiKey(_)
        | CodexAuth::Chatgpt(_)
        | CodexAuth::ChatgptAuthTokens(_)
        | CodexAuth::PersonalAccessToken(_) => Arc::new(BearerAuthProvider {
            token: auth.get_token().ok(),
            account_id: auth.get_account_id(),
            is_fedramp_account: auth.is_fedramp_account(),
        }),
    }
}

/// Builds request-header auth that reads the current managed auth snapshot on
/// every request while remaining scoped to the expected auth identity.
///
/// Callers with account-scoped state should pass the same snapshot that keyed
/// that state so a later account switch cannot reuse it.
pub fn auth_provider_from_auth_manager(
    auth_manager: Arc<AuthManager>,
    expected_auth: &CodexAuth,
) -> SharedAuthProvider {
    Arc::new(AuthManagerAuthProvider {
        auth_manager,
        expected_auth: expected_auth.clone(),
    })
}

#[cfg(test)]
mod tests {
    use codex_agent_identity::generate_agent_key_material;
    use codex_login::AuthCredentialsStoreMode;
    use codex_login::AuthKeyringBackendKind;
    use codex_login::auth::AgentIdentityAuthRecord;
    use codex_login::auth::BedrockApiKeyAuth;
    use codex_login::auth::login_with_chatgpt_auth_tokens;
    use codex_model_provider_info::WireApi;
    use codex_model_provider_info::create_oss_provider_with_base_url;
    use codex_protocol::account::PlanType;
    use codex_protocol::error::CodexErrorDetails;
    use http::header::AUTHORIZATION;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use std::path::Path;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;

    static NEXT_CODEX_HOME_ID: AtomicUsize = AtomicUsize::new(0);
    const TEST_CHATGPT_ID_TOKEN: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJlbWFpbCI6InVzZXJAZXhhbXBsZS5jb20iLCJlbWFpbF92ZXJpZmllZCI6dHJ1ZSwiaHR0cHM6Ly9hcGkub3BlbmFpLmNvbS9hdXRoIjp7ImNoYXRncHRfdXNlcl9pZCI6InVzZXItMTIzNDUiLCJ1c2VyX2lkIjoidXNlci0xMjM0NSIsImNoYXRncHRfcGxhbl90eXBlIjoicHJvIiwiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjb3VudC0xMjMifX0.c2ln";

    async fn agent_identity_auth(chatgpt_account_is_fedramp: bool) -> AgentIdentityAuth {
        let key_material = generate_agent_key_material().expect("generate key material");
        AgentIdentityAuth::from_record(
            AgentIdentityAuthRecord {
                agent_runtime_id: "agent-runtime-1".to_string(),
                agent_private_key: key_material.private_key_pkcs8_base64,
                account_id: "account-1".to_string(),
                chatgpt_user_id: "user-1".to_string(),
                email: Some("agent@example.com".to_string()),
                plan_type: PlanType::Plus,
                chatgpt_account_is_fedramp,
                task_id: Some("task-run-1".to_string()),
            },
            "https://auth.openai.com/api/accounts",
            &codex_login::test_support::transport_default_auth_route_config(),
        )
        .await
        .expect("agent identity auth record should include task id")
    }

    #[test]
    fn claude_plan_cache_tracks_the_persisted_selection_revision() {
        let claude_plan = ModelProviderInfo::create_claude_plan_provider();
        assert_eq!(
            external_bearer_cache_policy(/*auth_manager*/ None, &claude_plan),
            ExternalBearerCachePolicy::FreshPerRequest
        );

        let codex_home = test_codex_home();
        let auth_manager = AuthManager::from_auth_for_testing_with_home(
            CodexAuth::from_api_key("test-api-key"),
            codex_home.clone(),
        );
        assert_eq!(
            external_bearer_cache_policy(Some(auth_manager.as_ref()), &claude_plan),
            ExternalBearerCachePolicy::InvalidateOnChange(claude_auth_selection_revision_path(
                &codex_home
            ))
        );

        let mut custom_provider = claude_plan;
        custom_provider.name = "Custom command-auth provider".to_string();
        assert_eq!(
            external_bearer_cache_policy(Some(auth_manager.as_ref()), &custom_provider),
            ExternalBearerCachePolicy::Timed
        );
    }

    fn provider_auth_scope(
        policy: AgentIdentityAuthPolicy,
        fallback: AgentIdentitySessionFallback,
    ) -> ProviderAuthScope {
        ProviderAuthScope {
            agent_identity_policy: policy,
            session_source: SessionSource::Cli,
            agent_identity_session_fallback: fallback,
        }
    }

    fn test_codex_home() -> PathBuf {
        let id = NEXT_CODEX_HOME_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "codex-model-provider-agent-identity-{pid}-{id}",
            pid = std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp codex home");
        path
    }

    fn write_chatgpt_auth_json(codex_home: &Path) {
        let auth_json = json!({
            "tokens": {
                "id_token": TEST_CHATGPT_ID_TOKEN,
                "access_token": "test-access-token",
                "refresh_token": "test-refresh-token",
                "account_id": "account-123"
            },
            "last_refresh": "2099-01-01T00:00:00Z"
        });
        std::fs::write(
            codex_home.join("auth.json"),
            serde_json::to_string_pretty(&auth_json).expect("serialize auth.json"),
        )
        .expect("write auth.json");
    }

    async fn chatgpt_auth_manager(
        agent_identity_authapi_base_url: String,
    ) -> (PathBuf, Arc<AuthManager>, CodexAuth) {
        let codex_home = test_codex_home();
        write_chatgpt_auth_json(&codex_home);
        let auth_manager = AuthManager::shared(
            codex_home.clone(),
            /*enable_codex_api_key_env*/ false,
            AuthCredentialsStoreMode::File,
            /*forced_chatgpt_workspace_id*/ None,
            /*chatgpt_base_url*/ None,
            AuthKeyringBackendKind::default(),
            codex_login::test_support::transport_default_auth_route_config(),
        )
        .await;
        let auth = auth_manager.auth().await.expect("auth should load");
        let auth_manager = AuthManager::from_auth_for_testing_with_agent_identity_authapi_base_url(
            auth.clone(),
            agent_identity_authapi_base_url,
        );
        (codex_home, auth_manager, auth)
    }

    async fn mount_transient_agent_registration(
        server: &MockServer,
        status: u16,
        registration_count: Arc<AtomicUsize>,
    ) {
        Mock::given(method("POST"))
            .and(path("/v1/agent/register"))
            .respond_with(move |_request: &wiremock::Request| {
                registration_count.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(status)
            })
            .mount(server)
            .await;
    }

    #[test]
    fn unauthenticated_auth_provider_adds_no_headers() {
        let provider =
            create_oss_provider_with_base_url("http://localhost:11434/v1", WireApi::Responses);
        let auth = resolve_provider_auth(/*auth*/ None, &provider).expect("auth should resolve");

        assert!(auth.to_auth_headers().is_empty());
    }

    #[test]
    fn header_auth_adds_predefined_headers() {
        let mut expected = HeaderMap::new();
        expected.insert(
            http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer external"),
        );
        expected.insert("x-external-auth", HeaderValue::from_static("enabled"));
        let auth = CodexAuth::Headers(AuthHeaders::new(expected.clone()));

        let actual = auth_provider_from_auth(&auth).to_auth_headers();

        assert_eq!(actual, expected);
    }

    /// The brokered key and header must match what direct auth would send.
    fn assert_brokered_matches_direct(
        auth: Option<&CodexAuth>,
        provider: &ModelProviderInfo,
        expected_header: ProviderApiKeyHeader,
    ) {
        let key = provider_api_key(auth, provider)
            .expect("resolve key")
            .expect("plain API key");
        assert_eq!(key.header, expected_header);
        let direct = resolve_provider_auth(auth, provider)
            .expect("direct auth")
            .to_auth_headers();
        let (name, value) = match expected_header {
            ProviderApiKeyHeader::Bearer => ("authorization", format!("Bearer {}", key.value)),
            ProviderApiKeyHeader::XApiKey => ("x-api-key", key.value.clone()),
        };
        assert_eq!(
            direct.get(name).and_then(|value| value.to_str().ok()),
            Some(value.as_str())
        );
        assert!(!format!("{key:?}").contains(&key.value));
    }

    #[test]
    fn pf_27_s05_plain_api_keys_are_extracted_with_their_header() {
        let api_key = CodexAuth::from_api_key("sk-pf27s05-openai");
        assert_brokered_matches_direct(
            Some(&api_key),
            &ModelProviderInfo::create_openai_provider(/*base_url*/ None),
            ProviderApiKeyHeader::Bearer,
        );
        assert_brokered_matches_direct(
            Some(&CodexAuth::from_api_key("sk-ant-pf27s05")),
            &ModelProviderInfo::create_anthropic_provider(),
            ProviderApiKeyHeader::XApiKey,
        );
        assert_brokered_matches_direct(
            Some(&CodexAuth::from_api_key("zai-pf27s05")),
            &ModelProviderInfo::create_zai_provider(),
            ProviderApiKeyHeader::Bearer,
        );
        let mut bearer =
            create_oss_provider_with_base_url("https://llm.example/v1", WireApi::Responses);
        bearer.experimental_bearer_token = Some("bearer-pf27s05".to_string());
        assert_brokered_matches_direct(/*auth*/ None, &bearer, ProviderApiKeyHeader::Bearer);
    }

    use crate::model_key_broker::test_support::with_recording_broker;

    #[test]
    fn pf_84_named_account_key_is_brokered_without_env_keys() {
        with_recording_broker(|broker| {
            let mut zai = ModelProviderInfo::create_zai_provider();
            zai.account = Some(codex_model_provider_info::NamedProviderAccount {
                provider_id: "zai".to_string(),
                name: "work".to_string(),
            });
            resolve_provider_auth(/*auth*/ None, &zai).expect("brokered auth");
            let uses = broker.uses.lock().expect("uses");
            assert_eq!(uses.len(), 1);
            let crate::model_key_broker::BrokeredKeySource::ProviderKey {
                provider_key_id,
                env_vars,
                account,
            } = &uses[0].source
            else {
                panic!("expected a named provider key: {:?}", uses[0]);
            };
            assert_eq!(provider_key_id, zai.env_key.as_deref().expect("env key"));
            assert!(env_vars.is_empty(), "a named account never takes env keys");
            assert_eq!(account, &zai.account);
        });
    }

    #[test]
    fn pf_27_s05_brokered_provider_keys_are_named_not_read() {
        with_recording_broker(|broker| {
            let zai = ModelProviderInfo::create_zai_provider();
            let placeholder = CodexAuth::from_api_key(crate::BROKERED_KEY_PLACEHOLDER);
            for auth in [None, Some(&placeholder)] {
                let headers = resolve_provider_auth(auth, &zai)
                    .expect("brokered auth")
                    .to_auth_headers();
                assert!(headers.is_empty(), "Core attaches nothing: {headers:?}");
            }
            let uses = broker.uses.lock().expect("uses");
            assert_eq!(uses.len(), 2);
            for request in uses.iter() {
                assert_eq!(request.header, ProviderApiKeyHeader::Bearer);
                assert_eq!(
                    request.base_url,
                    zai.to_api_provider(/*auth_mode*/ None)
                        .expect("provider")
                        .base_url
                );
                let crate::model_key_broker::BrokeredKeySource::ProviderKey {
                    provider_key_id,
                    env_vars,
                    account: None,
                } = &request.source
                else {
                    panic!("expected a named provider key: {request:?}");
                };
                assert_eq!(provider_key_id, zai.env_key.as_deref().expect("env key"));
                assert_eq!(env_vars, &zai.api_key_env_vars());
            }
        });
    }

    #[test]
    fn pf_27_s05_held_values_and_sign_in_tokens_go_to_the_broker() {
        with_recording_broker(|broker| {
            let openai = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
            let api_key = CodexAuth::from_api_key("sk-pf27s05-login");
            let chatgpt = CodexAuth::create_dummy_chatgpt_auth_for_testing();
            let mut bearer =
                create_oss_provider_with_base_url("https://llm.example/v1", WireApi::Responses);
            bearer.experimental_bearer_token = Some("bearer-pf27s05".to_string());
            for (auth, provider) in [
                (Some(&api_key), &openai),
                (Some(&chatgpt), &openai),
                (None, &bearer),
            ] {
                assert!(
                    resolve_provider_auth(auth, provider)
                        .expect("brokered auth")
                        .to_auth_headers()
                        .is_empty()
                );
            }
            let uses = broker.uses.lock().expect("uses");
            let values: Vec<(String, Option<String>)> = uses
                .iter()
                .map(|request| match &request.source {
                    crate::model_key_broker::BrokeredKeySource::Value { key, slot } => {
                        (key.value.clone(), slot.clone())
                    }
                    other => panic!("expected a held value: {other:?}"),
                })
                .collect();
            let chatgpt_token = chatgpt.get_token().expect("token");
            let account = chatgpt.get_account_id().unwrap_or_default();
            assert_eq!(
                values,
                vec![
                    (
                        "sk-pf27s05-login".to_string(),
                        Some("api-key-login".to_string())
                    ),
                    (chatgpt_token, Some(format!("sign-in:{account}"))),
                    (
                        "bearer-pf27s05".to_string(),
                        Some("experimental-bearer-token".to_string())
                    ),
                ]
            );
            // The sign-in token's non-secret headers go along; the token does not.
            let chatgpt_headers = &uses[1].extra_headers;
            assert!(chatgpt_headers.get(AUTHORIZATION).is_none());
            assert_eq!(
                chatgpt_headers
                    .get("ChatGPT-Account-ID")
                    .and_then(|value| value.to_str().ok()),
                chatgpt.get_account_id().as_deref()
            );
            assert!(!format!("{uses:?}").contains("sk-pf27s05-login"));
        });
    }

    /// A sign-in login used for an env-key provider is attached by the broker
    /// with the provider's header, never by Core.
    #[test]
    fn pf_27_s05_sign_in_for_an_env_key_provider_is_brokered() {
        with_recording_broker(|broker| {
            let mut provider =
                create_oss_provider_with_base_url("https://llm.example/v1", WireApi::Responses);
            // Anthropic-style keys go as `x-api-key`.
            provider.env_key = Some("ANTHROPIC_API_KEY".to_string());
            let chatgpt = CodexAuth::create_dummy_chatgpt_auth_for_testing();
            let headers = resolve_provider_auth(Some(&chatgpt), &provider)
                .expect("brokered auth")
                .to_auth_headers();
            assert!(headers.is_empty(), "Core attaches nothing: {headers:?}");
            let uses = broker.uses.lock().expect("uses");
            assert_eq!(uses.len(), 1);
            assert_eq!(uses[0].header, ProviderApiKeyHeader::XApiKey);
            assert!(uses[0].extra_headers.is_empty());
            let crate::model_key_broker::BrokeredKeySource::Value { key, slot } = &uses[0].source
            else {
                panic!("expected the sign-in token: {:?}", uses[0]);
            };
            assert_eq!(key.value, chatgpt.get_token().expect("token"));
            assert!(
                slot.as_deref()
                    .is_some_and(|slot| slot.starts_with("sign-in:"))
            );
        });
    }

    /// Before the process's broker runs, brokered credentials are not sent.
    #[test]
    fn pf_27_s05_required_broker_not_running_fails_closed() {
        crate::model_key_broker::set_test_broker_required(/*required*/ true);
        let mut provider =
            create_oss_provider_with_base_url("https://llm.example/v1", WireApi::Responses);
        provider.experimental_bearer_token = Some("bearer-pf27s05".to_string());
        let result = resolve_provider_auth(/*auth*/ None, &provider);
        crate::model_key_broker::set_test_broker_required(/*required*/ false);
        let Err(error) = result else {
            panic!("a credential must not be attached before the broker runs");
        };
        assert!(error.to_string().contains("not running yet"), "{error}");
    }

    /// Command-backed provider auth is not brokered: with the broker
    /// required it is still sent, unchanged.
    #[test]
    fn pf_27_s05_command_auth_is_sent_as_before() {
        with_recording_broker(|broker| {
            let provider = ModelProviderInfo {
                auth: Some(codex_protocol::config_types::ModelProviderAuthInfo {
                    command: "print-token".to_string(),
                    args: Vec::new(),
                    timeout_ms: std::num::NonZeroU64::new(5_000).expect("timeout"),
                    refresh_interval_ms: 300_000,
                    cwd: std::env::current_dir()
                        .expect("cwd")
                        .try_into()
                        .expect("absolute cwd"),
                }),
                requires_openai_auth: false,
                ..ModelProviderInfo::create_openai_provider(/*base_url*/ None)
            };
            let token = CodexAuth::from_api_key("pf27s05-command-token");
            let headers = resolve_provider_auth(Some(&token), &provider)
                .expect("command auth")
                .to_auth_headers();
            assert_eq!(
                headers
                    .get(AUTHORIZATION)
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer pf27s05-command-token")
            );
            assert!(broker.uses.lock().expect("uses").is_empty());
        });
    }

    /// Flag off (no broker, not required): direct auth is unchanged.
    #[test]
    fn pf_27_s05_flag_off_direct_auth_is_unchanged() {
        let mut provider =
            create_oss_provider_with_base_url("https://llm.example/v1", WireApi::Responses);
        provider.experimental_bearer_token = Some("bearer-pf27s05".to_string());
        let headers = resolve_provider_auth(/*auth*/ None, &provider)
            .expect("direct auth")
            .to_auth_headers();
        assert_eq!(
            headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer bearer-pf27s05")
        );
        let api_key = CodexAuth::from_api_key("sk-pf27s05-direct");
        let headers = auth_provider_from_auth(&api_key).to_auth_headers();
        assert_eq!(
            headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer sk-pf27s05-direct")
        );
    }

    #[test]
    fn pf_27_s05_header_only_first_party_auth_gets_no_credential() {
        with_recording_broker(|_| {
            let headers = CodexAuth::Headers(AuthHeaders::new(HeaderMap::from_iter([(
                HeaderName::from_static("x-custom"),
                HeaderValue::from_static("kept"),
            )])));
            for auth in [
                CodexAuth::from_api_key("sk-pf27s05-header-only"),
                CodexAuth::create_dummy_chatgpt_auth_for_testing(),
            ] {
                assert!(auth_provider_from_auth(&auth).to_auth_headers().is_empty());
            }
            // Auth that is not brokered is unchanged.
            assert_eq!(
                auth_provider_from_auth(&headers)
                    .to_auth_headers()
                    .get("x-custom")
                    .and_then(|value| value.to_str().ok()),
                Some("kept")
            );
            let openai = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
            assert_eq!(
                resolve_provider_auth(Some(&headers), &openai)
                    .expect("header auth")
                    .to_auth_headers()
                    .get("x-custom")
                    .and_then(|value| value.to_str().ok()),
                Some("kept")
            );
        });
    }

    #[test]
    fn pf_27_s05_sign_in_and_other_auth_are_not_brokered() {
        let openai = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
        let chatgpt = CodexAuth::create_dummy_chatgpt_auth_for_testing();
        let headers = CodexAuth::Headers(AuthHeaders::new(HeaderMap::new()));
        let bedrock = CodexAuth::BedrockApiKey(BedrockApiKeyAuth {
            api_key: "bedrock-api-key-test".to_string(),
            region: "us-east-1".to_string(),
        });
        for auth in [None, Some(&chatgpt), Some(&headers), Some(&bedrock)] {
            assert!(
                provider_api_key(auth, &openai)
                    .expect("resolve key")
                    .is_none()
            );
        }
    }

    #[test]
    fn openai_provider_rejects_bedrock_api_key_auth() {
        let provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
        let auth = CodexAuth::BedrockApiKey(BedrockApiKeyAuth {
            api_key: "bedrock-api-key-test".to_string(),
            region: "us-east-1".to_string(),
        });

        match resolve_provider_auth(Some(&auth), &provider) {
            Err(err) => match err.details() {
                CodexErrorDetails::UnsupportedOperation(message) => {
                    assert_eq!(message, BEDROCK_API_KEY_UNSUPPORTED_MESSAGE);
                }
                details => panic!("unexpected auth error: {details:?}"),
            },
            Ok(_) => panic!("Bedrock API key auth should be rejected"),
        }
    }

    #[tokio::test]
    async fn auth_manager_provider_follows_refreshes_but_not_account_switches() {
        let codex_home = test_codex_home();
        login_with_chatgpt_auth_tokens(
            &codex_home,
            "header.e30.first",
            "test-account",
            /*chatgpt_plan_type*/ None,
        )
        .expect("save initial auth");
        let auth_manager = Arc::new(
            AuthManager::new(
                codex_home.clone(),
                /*enable_codex_api_key_env*/ false,
                AuthCredentialsStoreMode::Ephemeral,
                /*forced_chatgpt_workspace_id*/ None,
                /*chatgpt_base_url*/ None,
                AuthKeyringBackendKind::default(),
                codex_login::test_support::transport_default_auth_route_config(),
            )
            .await,
        );
        let expected_auth = auth_manager
            .auth_cached()
            .expect("initial auth should be cached");
        let provider = auth_provider_from_auth_manager(Arc::clone(&auth_manager), &expected_auth);

        assert_eq!(
            provider.to_auth_headers().get(AUTHORIZATION),
            Some(&HeaderValue::from_static("Bearer header.e30.first"))
        );

        login_with_chatgpt_auth_tokens(
            &codex_home,
            "header.e30.reloaded",
            "test-account",
            /*chatgpt_plan_type*/ None,
        )
        .expect("save reloaded auth");
        auth_manager.reload().await;

        assert_eq!(
            provider.to_auth_headers().get(AUTHORIZATION),
            Some(&HeaderValue::from_static("Bearer header.e30.reloaded"))
        );

        login_with_chatgpt_auth_tokens(
            &codex_home,
            "header.e30.other-account",
            "other-account",
            /*chatgpt_plan_type*/ None,
        )
        .expect("save switched-account auth");
        auth_manager.reload().await;

        assert!(provider.to_auth_headers().is_empty());
    }

    #[tokio::test]
    async fn first_party_run_scope_uses_agent_assertion_and_exposes_telemetry() {
        let auth = CodexAuth::AgentIdentity(
            agent_identity_auth(/*chatgpt_account_is_fedramp*/ false).await,
        );
        let provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None);

        let auth = resolve_provider_auth_for_scope(
            /*auth_manager*/ None,
            Some(&auth),
            &provider,
            provider_auth_scope(
                AgentIdentityAuthPolicy::JwtOnly,
                AgentIdentitySessionFallback::default(),
            ),
        )
        .await
        .expect("auth should resolve");

        assert_eq!(
            auth.agent_identity_telemetry,
            Some(AgentIdentityTelemetry {
                agent_id: "agent-runtime-1".to_string(),
                task_id: "task-run-1".to_string(),
            })
        );
        let headers = auth.auth.to_auth_headers();
        assert!(
            headers
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("AgentAssertion "))
        );
    }

    #[tokio::test]
    async fn agent_identity_auth_provider_preserves_account_routing_headers() {
        let auth = agent_identity_auth(/*chatgpt_account_is_fedramp*/ true).await;
        let provider = auth_provider_from_auth(&CodexAuth::AgentIdentity(auth));

        let headers = provider.to_auth_headers();

        assert!(
            headers
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("AgentAssertion "))
        );
        assert_eq!(
            headers
                .get("ChatGPT-Account-ID")
                .and_then(|value| value.to_str().ok()),
            Some("account-1")
        );
        assert_eq!(
            headers
                .get("X-OpenAI-Fedramp")
                .and_then(|value| value.to_str().ok()),
            Some("true")
        );
    }

    #[tokio::test]
    async fn chatgpt_bootstrap_unavailable_uses_session_bearer_fallback() {
        let server = MockServer::start().await;
        let registration_count = Arc::new(AtomicUsize::new(0));
        mount_transient_agent_registration(
            &server,
            /*status*/ 503,
            Arc::clone(&registration_count),
        )
        .await;
        let (_codex_home, auth_manager, auth) = chatgpt_auth_manager(server.uri()).await;
        let provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
        let fallback = AgentIdentitySessionFallback::default();

        let provider_auth = resolve_provider_auth_for_scope(
            Some(auth_manager),
            Some(&auth),
            &provider,
            provider_auth_scope(AgentIdentityAuthPolicy::ChatGptAuth, fallback.clone()),
        )
        .await
        .expect("fallback should resolve bearer auth");

        let headers = provider_auth.auth.to_auth_headers();
        assert_eq!(
            headers
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer test-access-token")
        );
        assert_eq!(
            headers
                .get("ChatGPT-Account-ID")
                .and_then(|value| value.to_str().ok()),
            Some("account-123")
        );
        assert!(fallback.is_engaged());
        assert_eq!(registration_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn chatgpt_session_fallback_skips_later_agent_identity_bootstrap() {
        let server = MockServer::start().await;
        let registration_count = Arc::new(AtomicUsize::new(0));
        mount_transient_agent_registration(
            &server,
            /*status*/ 503,
            Arc::clone(&registration_count),
        )
        .await;
        let (_codex_home, auth_manager, auth) = chatgpt_auth_manager(server.uri()).await;
        let provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
        let fallback = AgentIdentitySessionFallback::default();

        resolve_provider_auth_for_scope(
            Some(Arc::clone(&auth_manager)),
            Some(&auth),
            &provider,
            provider_auth_scope(AgentIdentityAuthPolicy::ChatGptAuth, fallback.clone()),
        )
        .await
        .expect("first fallback should resolve bearer auth");
        resolve_provider_auth_for_scope(
            Some(auth_manager),
            Some(&auth),
            &provider,
            provider_auth_scope(AgentIdentityAuthPolicy::ChatGptAuth, fallback),
        )
        .await
        .expect("second fallback should resolve bearer auth");

        assert_eq!(registration_count.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn chatgpt_sessions_share_bootstrap_failure_cooldown() {
        let server = MockServer::start().await;
        let registration_count = Arc::new(AtomicUsize::new(0));
        mount_transient_agent_registration(
            &server,
            /*status*/ 503,
            Arc::clone(&registration_count),
        )
        .await;
        let (_codex_home, auth_manager, auth) = chatgpt_auth_manager(server.uri()).await;
        let provider = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
        let first_fallback = AgentIdentitySessionFallback::default();
        let second_fallback = AgentIdentitySessionFallback::default();

        resolve_provider_auth_for_scope(
            Some(Arc::clone(&auth_manager)),
            Some(&auth),
            &provider,
            provider_auth_scope(AgentIdentityAuthPolicy::ChatGptAuth, first_fallback.clone()),
        )
        .await
        .expect("first session fallback should resolve bearer auth");
        resolve_provider_auth_for_scope(
            Some(auth_manager),
            Some(&auth),
            &provider,
            provider_auth_scope(
                AgentIdentityAuthPolicy::ChatGptAuth,
                second_fallback.clone(),
            ),
        )
        .await
        .expect("second session fallback should resolve bearer auth");

        assert!(first_fallback.is_engaged());
        assert!(second_fallback.is_engaged());
        assert_eq!(registration_count.load(Ordering::SeqCst), 3);
    }
}

#[cfg(test)]
#[path = "named_sign_in_tests.rs"]
mod named_sign_in_tests;
