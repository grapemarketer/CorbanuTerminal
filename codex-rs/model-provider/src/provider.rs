use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use codex_api::ApiError;
use codex_api::Provider;
use codex_api::SharedAuthProvider;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::validate_provider_auth_command_for_account;
use codex_model_provider_info::AMBIENT_DEFAULT_MODEL;
use codex_model_provider_info::ANTHROPIC_DEFAULT_MODEL;
use codex_model_provider_info::BASETEN_DEFAULT_MODEL;
use codex_model_provider_info::CLAUDE_PLAN_MODEL;
use codex_model_provider_info::DEEPSEEK_DEFAULT_MODEL;
use codex_model_provider_info::KIMI_CODE_K3_MODEL;
use codex_model_provider_info::META_DEFAULT_MODEL;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::OPENROUTER_DEFAULT_MODEL;
use codex_model_provider_info::VERCEL_DEFAULT_MODEL;
use codex_model_provider_info::WireApi;
use codex_model_provider_info::ZAI_DEFAULT_MODEL;
use codex_models_manager::bundled_models_response;
use codex_models_manager::manager::OpenAiModelsManager;
use codex_models_manager::manager::SharedModelsManager;
use codex_models_manager::manager::StaticModelsManager;
use codex_protocol::account::ProviderAccount;
use codex_protocol::error::CodexErr;
use codex_protocol::openai_models::ModelsResponse;

use crate::amazon_bedrock::AmazonBedrockModelProvider;
use crate::auth::ProviderAuthScope;
use crate::auth::ResolvedProviderAuth;
use crate::auth::auth_manager_for_provider;
use crate::auth::named_sign_in_refusal;
use crate::auth::resolve_provider_auth;
use crate::auth::resolve_provider_auth_for_scope;
use crate::models_endpoint::OpenAiModelsEndpoint;

/// Optional provider-backed features that Codex may expose at runtime.
///
/// These capabilities are a provider-owned upper bound. Callers can disable
/// more functionality through normal config, but should not expose a feature
/// that the active provider marks unsupported here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub namespace_tools: bool,
    pub image_generation: bool,
    pub web_search: bool,
}

impl Default for ProviderCapabilities {
    fn default() -> Self {
        Self {
            namespace_tools: true,
            image_generation: true,
            web_search: true,
        }
    }
}

/// Current app-visible account state for a model provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderAccountState {
    pub account: Option<ProviderAccount>,
    pub requires_openai_auth: bool,
}

/// Error returned when a provider cannot construct its app-visible account state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAccountError {
    MissingChatgptAccountDetails,
    UnsupportedBedrockApiKeyAuth,
}

impl fmt::Display for ProviderAccountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingChatgptAccountDetails => {
                write!(f, "plan type is required for chatgpt authentication")
            }
            Self::UnsupportedBedrockApiKeyAuth => {
                write!(
                    f,
                    "Bedrock API key auth is only supported by the Amazon Bedrock model provider"
                )
            }
        }
    }
}

impl std::error::Error for ProviderAccountError {}

pub type ProviderAccountResult = std::result::Result<ProviderAccountState, ProviderAccountError>;

/// Default model used for automatic approval review when a provider does not
/// require a backend-specific model ID.
pub const DEFAULT_APPROVAL_REVIEW_PREFERRED_MODEL: &str = "codex-auto-review";

/// Default model used for memory extraction when a provider does not require a
/// backend-specific model ID.
pub const DEFAULT_MEMORY_EXTRACTION_PREFERRED_MODEL: &str = "gpt-5.6-luna";

/// Default model used for memory consolidation when a provider does not require
/// a backend-specific model ID.
pub const DEFAULT_MEMORY_CONSOLIDATION_PREFERRED_MODEL: &str = "gpt-5.6-terra";

fn bundled_static_model_catalog() -> ModelsResponse {
    bundled_models_response()
        .unwrap_or_else(|err| panic!("bundled models.json should parse: {err}"))
}

/// Runtime provider abstraction used by model execution.
///
/// Implementations own provider-specific behavior for a model backend. The
/// `ModelProviderInfo` returned by `info` is the serialized/configured provider
/// metadata used by the default OpenAI-compatible implementation.
pub trait ModelProvider: fmt::Debug + Send + Sync {
    /// Returns the configured provider metadata.
    fn info(&self) -> &ModelProviderInfo;

    /// Returns the provider-owned capability upper bounds.
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    /// Returns the preferred model used for automatic approval review.
    ///
    /// Providers that require backend-specific model IDs should override this.
    fn approval_review_preferred_model(&self) -> &'static str {
        DEFAULT_APPROVAL_REVIEW_PREFERRED_MODEL
    }

    /// Returns the preferred model used for memory extraction.
    ///
    /// Providers that require backend-specific model IDs should override this.
    fn memory_extraction_preferred_model(&self) -> &'static str {
        DEFAULT_MEMORY_EXTRACTION_PREFERRED_MODEL
    }

    /// Returns the preferred model used for memory consolidation.
    ///
    /// Providers that require backend-specific model IDs should override this.
    fn memory_consolidation_preferred_model(&self) -> &'static str {
        DEFAULT_MEMORY_CONSOLIDATION_PREFERRED_MODEL
    }

    /// Resolve a provider-owned background helper choice without ever sending an OpenAI-only
    /// default model to an unrelated endpoint.
    fn resolve_background_helper_model(
        &self,
        preferred_model: &str,
        openai_default_model: &str,
        active_model: &str,
    ) -> String {
        if preferred_model == openai_default_model
            && !provider_uses_first_party_auth_path(self.info())
        {
            active_model.to_string()
        } else {
            preferred_model.to_string()
        }
    }

    /// Returns whether requests made through this provider should include attestation.
    fn supports_attestation(&self) -> bool {
        false
    }

    /// Returns the provider-scoped auth manager, when this provider uses one.
    ///
    /// TODO(celia-oai): Make auth manager access internal to this crate so callers
    /// resolve provider-specific auth only through `ModelProvider`. We first need
    /// to think through whether Codex should have a unified provider-specific auth
    /// manager throughout the codebase; that is a larger refactor than this change.
    fn auth_manager(&self) -> Option<Arc<AuthManager>>;

    /// Returns the current provider-scoped auth value, if one is configured.
    fn auth(&self) -> ModelProviderFuture<'_, Option<CodexAuth>>;

    /// Returns the current app-visible account state for this provider.
    fn account_state(&self) -> ProviderAccountResult;

    /// Maps an API client error into the provider's user-facing error representation.
    fn map_api_error(&self, error: ApiError) -> CodexErr {
        codex_api::map_api_error(error)
    }

    /// Returns provider configuration adapted for the API client.
    fn api_provider(&self) -> ModelProviderFuture<'_, codex_protocol::error::Result<Provider>> {
        Box::pin(async move {
            let auth = self.auth().await;
            self.info()
                .to_api_provider(auth.as_ref().map(CodexAuth::auth_mode))
        })
    }

    /// Returns the provider base URL that will be used at request time.
    fn runtime_base_url(
        &self,
    ) -> ModelProviderFuture<'_, codex_protocol::error::Result<Option<String>>> {
        Box::pin(async { Ok(self.info().base_url.clone()) })
    }

    /// Returns the auth provider used to attach request credentials.
    fn api_auth(
        &self,
    ) -> ModelProviderFuture<'_, codex_protocol::error::Result<SharedAuthProvider>> {
        Box::pin(async move {
            let auth = self.auth().await;
            resolve_provider_auth(auth.as_ref(), self.info())
        })
    }

    /// Returns request credentials, optionally scoped to a Codex session task.
    fn api_auth_for_scope(
        &self,
        scope: ProviderAuthScope,
    ) -> ModelProviderFuture<'_, codex_protocol::error::Result<ResolvedProviderAuth>> {
        Box::pin(async move {
            if !provider_uses_first_party_auth_path(self.info()) {
                return self.api_auth().await.map(ResolvedProviderAuth::new);
            }
            let auth = self.auth().await;
            resolve_provider_auth_for_scope(self.auth_manager(), auth.as_ref(), self.info(), scope)
                .await
        })
    }

    /// Creates the model manager implementation appropriate for this provider.
    fn models_manager(
        &self,
        codex_home: PathBuf,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager;

    /// Creates a model manager with caching disabled.
    ///
    /// Providers that fetch model catalogs should override this method. The default uses an
    /// authoritative in-memory catalog so hosted callers cannot accidentally write to disk.
    fn models_manager_without_cache(
        &self,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        let model_catalog = config_model_catalog
            .or_else(|| codex_models_manager::bundled_models_response().ok())
            .unwrap_or_default();
        Arc::new(StaticModelsManager::new(self.auth_manager(), model_catalog))
    }
}

pub type ModelProviderFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Shared runtime model provider handle.
pub type SharedModelProvider = Arc<dyn ModelProvider>;

fn provider_uses_first_party_auth_path(provider: &ModelProviderInfo) -> bool {
    provider.uses_first_party_openai_auth()
}

/// Drops the ambient `OPENAI_API_KEY` fallback for providers that don't use
/// OpenAI sign-in (local OSS servers, custom endpoints), so a key exported in
/// the shell is never sent to them. Saved logins keep their existing behavior.
pub(crate) fn auth_for_provider(
    auth: Option<CodexAuth>,
    provider: &ModelProviderInfo,
) -> Option<CodexAuth> {
    auth.filter(|auth| {
        !auth.is_openai_api_key_env_fallback() || provider_uses_first_party_auth_path(provider)
    })
}

fn configured_provider_helper_model(info: &ModelProviderInfo) -> Option<&'static str> {
    // Provider identity is a transport contract, not a credential-variable convention. Custom
    // providers may intentionally reuse a built-in key variable, and must not thereby inherit a
    // model that only exists on the built-in endpoint.
    let matches = |expected: ModelProviderInfo| {
        info.name == expected.name
            && info.base_url == expected.base_url
            && info.wire_api == expected.wire_api
    };

    if matches(ModelProviderInfo::create_ambient_provider())
        || matches(ModelProviderInfo::create_pfterminal_plan_provider())
    {
        Some(AMBIENT_DEFAULT_MODEL)
    } else if matches(ModelProviderInfo::create_kimi_code_provider()) {
        Some(KIMI_CODE_K3_MODEL)
    } else if matches(ModelProviderInfo::create_claude_plan_provider()) {
        Some(CLAUDE_PLAN_MODEL)
    } else if matches(ModelProviderInfo::create_anthropic_provider()) {
        Some(ANTHROPIC_DEFAULT_MODEL)
    } else if matches(ModelProviderInfo::create_zai_provider())
        || matches(ModelProviderInfo::create_zai_anthropic_provider())
    {
        Some(ZAI_DEFAULT_MODEL)
    } else if matches(ModelProviderInfo::create_openrouter_provider())
        || matches(ModelProviderInfo::create_openrouter_anthropic_provider())
    {
        Some(OPENROUTER_DEFAULT_MODEL)
    } else if matches(ModelProviderInfo::create_deepseek_provider()) {
        Some(DEEPSEEK_DEFAULT_MODEL)
    } else if matches(ModelProviderInfo::create_meta_provider()) {
        Some(META_DEFAULT_MODEL)
    } else if matches(ModelProviderInfo::create_baseten_provider())
        || matches(ModelProviderInfo::create_baseten_anthropic_provider())
    {
        Some(BASETEN_DEFAULT_MODEL)
    } else if matches(ModelProviderInfo::create_vercel_provider())
        || matches(ModelProviderInfo::create_vercel_anthropic_provider())
        || matches(ModelProviderInfo::create_vercel_anthropic_fast_provider())
    {
        Some(VERCEL_DEFAULT_MODEL)
    } else {
        None
    }
}

/// Creates the default runtime model provider for configured provider metadata.
pub fn create_model_provider(
    provider_info: ModelProviderInfo,
    auth_manager: Option<Arc<AuthManager>>,
) -> SharedModelProvider {
    if provider_info.is_amazon_bedrock() {
        Arc::new(AmazonBedrockModelProvider::new(provider_info, auth_manager))
    } else {
        Arc::new(ConfiguredModelProvider::new(provider_info, auth_manager))
    }
}

/// Runtime model provider backed by configured `ModelProviderInfo`.
#[derive(Clone, Debug)]
struct ConfiguredModelProvider {
    info: ModelProviderInfo,
    auth_manager: Option<Arc<AuthManager>>,
    /// PF-84: the Corbanu home holding the named-account registry (the
    /// command-auth manager has no home of its own).
    account_home: Option<PathBuf>,
    /// PF-84: why the selected command account cannot be used.
    account_error: Option<String>,
}

/// PF-84: a named account of an `auth.command` provider (including Claude
/// Plan) must be enrolled, checked from vault metadata, so an unenrolled
/// account fails at once instead of spawning its command on every retry.
fn command_account_error(
    provider_info: &ModelProviderInfo,
    account_home: Option<&std::path::Path>,
) -> Option<String> {
    provider_info.auth.as_ref()?;
    let account = provider_info.account.as_ref()?;
    let Some(home) = account_home else {
        return Some(format!(
            "account `{}` of provider `{}` cannot be verified: no Corbanu home is available",
            account.name, account.provider_id
        ));
    };
    crate::auth::external_bearer_account(provider_info, Some(home))?
        .ensure_enrolled()
        .err()
        .map(|error| error.to_string())
}

impl ConfiguredModelProvider {
    fn new(provider_info: ModelProviderInfo, auth_manager: Option<Arc<AuthManager>>) -> Self {
        let account_home = auth_manager
            .as_ref()
            .map(|auth_manager| auth_manager.codex_home().to_path_buf());
        let account_error = command_account_error(&provider_info, account_home.as_deref());
        // PF-84: the account-bound manager itself refuses an unenrolled
        // account before running the command; `account_error` only gives
        // requests the precise message early.
        let auth_manager = auth_manager_for_provider(auth_manager, &provider_info);
        Self {
            info: provider_info,
            auth_manager,
            account_home,
            account_error,
        }
    }

    fn provider_env_auth(&self, provider_key_id: &str) -> Option<CodexAuth> {
        // PF-27-S05: the broker reads this key; Core holds only a placeholder.
        if crate::model_key_broker::model_key_broker_required() {
            return Some(CodexAuth::from_api_key(
                crate::model_key_broker::BROKERED_KEY_PLACEHOLDER,
            ));
        }
        // PF-84: a named account reads only its own stored key.
        if let Some(account) = self.info.account.as_ref() {
            return self
                .auth_manager
                .as_ref()
                .and_then(|auth_manager| {
                    auth_manager
                        .provider_account_api_key(&account.provider_id, &account.name)
                        .unwrap_or_else(|error| {
                            tracing::warn!(%error, "named account key unavailable");
                            None
                        })
                })
                .map(|api_key| CodexAuth::from_api_key(&api_key));
        }
        if let Ok(api_key) = std::env::var(provider_key_id)
            && !api_key.trim().is_empty()
        {
            return Some(CodexAuth::from_api_key(&api_key));
        }

        self.auth_manager
            .as_ref()
            .and_then(|auth_manager| auth_manager.provider_api_key(provider_key_id).ok())
            .flatten()
            .map(|api_key| CodexAuth::from_api_key(&api_key))
    }
}

impl ConfiguredModelProvider {
    /// Why a request that found no sign-in is refused instead of being sent
    /// without credentials; `None` when it may go out unauthenticated.
    fn missing_sign_in_refusal(&self) -> Option<CodexErr> {
        // PF-84-S06 (#571): a named OpenAI sign-in account without a usable
        // sign-in (D171, #1286: also one removed or signed out while the
        // session ran) is refused with the reason, never sent
        // unauthenticated while the session claims the account.
        if let Some(refusal) = named_sign_in_refusal(&self.info, self.auth_manager.as_deref()) {
            return Some(refusal);
        }
        // PF-84-S06 (#570): the default login this process hides (the
        // restrictions rule it out) is refused with the reason, not replaced
        // by an unauthenticated request.
        if self.info.requires_openai_auth
            && self.info.env_key.is_none()
            && self.info.auth.is_none()
            && self.info.account.is_none()
            && let Some(reason) = self
                .auth_manager
                .as_ref()
                .and_then(|auth_manager| auth_manager.hidden_default_login_reason())
        {
            return Some(CodexErr::Fatal(reason));
        }
        None
    }
}

impl ModelProvider for ConfiguredModelProvider {
    fn info(&self) -> &ModelProviderInfo {
        &self.info
    }

    fn capabilities(&self) -> ProviderCapabilities {
        if self.info.is_ambient()
            || self.info.is_kimi_code()
            || self.info.is_baseten()
            || self.info.is_vercel()
        {
            ProviderCapabilities {
                namespace_tools: false,
                image_generation: false,
                web_search: false,
            }
        } else if self.info.is_zai() || self.info.is_openrouter() {
            ProviderCapabilities {
                namespace_tools: false,
                image_generation: false,
                web_search: true,
            }
        } else if self.info.wire_api == WireApi::Anthropic {
            // Anthropic Messages accepts ordinary function tools but has no
            // Responses namespace-tool container. Advertising namespace
            // support here registers collaboration handlers under namespaced
            // keys, then the Anthropic serializer drops the container and
            // leaves the model with neither visible nor callable agent tools.
            ProviderCapabilities {
                namespace_tools: false,
                image_generation: false,
                web_search: true,
            }
        } else if self.info.wire_api == WireApi::Chat {
            // Chat Completions has no namespace container either. Core offers
            // namespaced tools under flat names (#515, #547).
            ProviderCapabilities {
                namespace_tools: false,
                image_generation: false,
                ..ProviderCapabilities::default()
            }
        } else {
            ProviderCapabilities::default()
        }
    }

    fn approval_review_preferred_model(&self) -> &'static str {
        configured_provider_helper_model(&self.info)
            .unwrap_or(DEFAULT_APPROVAL_REVIEW_PREFERRED_MODEL)
    }

    fn memory_extraction_preferred_model(&self) -> &'static str {
        configured_provider_helper_model(&self.info)
            .unwrap_or(DEFAULT_MEMORY_EXTRACTION_PREFERRED_MODEL)
    }

    fn memory_consolidation_preferred_model(&self) -> &'static str {
        configured_provider_helper_model(&self.info)
            .unwrap_or(DEFAULT_MEMORY_CONSOLIDATION_PREFERRED_MODEL)
    }

    fn auth_manager(&self) -> Option<Arc<AuthManager>> {
        self.auth_manager.clone()
    }

    fn supports_attestation(&self) -> bool {
        self.auth_manager
            .as_ref()
            .and_then(|auth_manager| auth_manager.auth_cached())
            .is_some_and(|auth| auth.is_chatgpt_auth())
    }

    fn auth(&self) -> ModelProviderFuture<'_, Option<CodexAuth>> {
        Box::pin(async move {
            let auth_manager = self.auth_manager.as_ref()?;

            if let Some(provider_key_id) = self.info.env_key.as_deref() {
                return self.provider_env_auth(provider_key_id);
            }

            auth_for_provider(auth_manager.auth().await, &self.info)
        })
    }

    fn api_auth(
        &self,
    ) -> ModelProviderFuture<'_, codex_protocol::error::Result<SharedAuthProvider>> {
        Box::pin(async move {
            if let Some(message) = &self.account_error {
                return Err(CodexErr::Fatal(message.clone()));
            }
            let mut auth = self.auth().await;
            if auth.is_none()
                && let Some(command_auth) = self.info.auth.as_ref()
            {
                // AuthManager's compatibility API represents provider-command failures as
                // `None`. Never turn that into an anonymous upstream request: rerun the
                // command through its validating path so the actionable helper error reaches
                // the user. A transient first failure may recover here, in which case resolve
                // once more and use the recovered credential.
                validate_provider_auth_command_for_account(
                    command_auth,
                    self.info
                        .account
                        .as_ref()
                        .map(|account| account.name.as_str()),
                )
                .await
                .map_err(|error| match error.kind() {
                    // PF-84: an outdated helper refused the account; retrying cannot help.
                    std::io::ErrorKind::Unsupported => CodexErr::Fatal(error.to_string()),
                    _ => error.into(),
                })?;
                auth = self.auth().await;
                if auth.is_none() {
                    return Err(std::io::Error::other(format!(
                        "provider auth command `{}` produced no usable credential",
                        command_auth.command
                    ))
                    .into());
                }
            }
            if auth.is_none()
                && let Some(error) = self.missing_sign_in_refusal()
            {
                return Err(error);
            }
            resolve_provider_auth(auth.as_ref(), self.info())
        })
    }

    fn api_auth_for_scope(
        &self,
        scope: ProviderAuthScope,
    ) -> ModelProviderFuture<'_, codex_protocol::error::Result<ResolvedProviderAuth>> {
        Box::pin(async move {
            if !provider_uses_first_party_auth_path(self.info()) {
                return self.api_auth().await.map(ResolvedProviderAuth::new);
            }
            let auth = self.auth().await;
            // D171 (#1286): the first-party path refuses a missing sign-in
            // as `api_auth` does, instead of sending the request without it.
            if auth.is_none()
                && let Some(error) = self.missing_sign_in_refusal()
            {
                return Err(error);
            }
            resolve_provider_auth_for_scope(self.auth_manager(), auth.as_ref(), self.info(), scope)
                .await
        })
    }

    fn account_state(&self) -> ProviderAccountResult {
        let account = if self.info.env_key.is_some()
            && crate::model_key_broker::model_key_broker_required()
        {
            // PF-27-S05: Core does not read the key to check for it; a missing
            // key is reported when a request is made.
            Some(ProviderAccount::ApiKey)
        } else if let (Some(account), Some(_)) = (self.info.account.as_ref(), &self.info.env_key) {
            // PF-84: only the named account's own key counts.
            self.account_home
                .as_deref()
                .and_then(|home| {
                    codex_login::provider_account_holds(
                        home,
                        &account.provider_id,
                        &account.name,
                        codex_login::ProviderAccountKind::ApiKey,
                    )
                    .inspect_err(
                        |error| tracing::warn!(%error, "named account registry unavailable"),
                    )
                    .ok()
                })
                .filter(|holds| *holds)
                .map(|_| ProviderAccount::ApiKey)
        } else if let Some(provider_key_id) = self.info.env_key.as_deref() {
            let stored_key = self
                .auth_manager
                .as_ref()
                .and_then(|auth_manager| auth_manager.provider_api_key(provider_key_id).ok())
                .flatten();
            let env_key = self.info.api_key().ok().flatten();
            if stored_key.is_some() || env_key.is_some() {
                Some(ProviderAccount::ApiKey)
            } else {
                None
            }
        } else if self.info.requires_openai_auth {
            self.auth_manager
                .as_ref()
                .and_then(|auth_manager| {
                    let auth = auth_manager.auth_cached()?;
                    if auth_manager.refresh_failure_for_auth(&auth).is_some() {
                        return None;
                    }
                    if matches!(auth, CodexAuth::Headers(_)) {
                        return None;
                    }
                    Some(auth)
                })
                .map(|auth| match &auth {
                    CodexAuth::ApiKey(_) => Ok(ProviderAccount::ApiKey),
                    CodexAuth::BedrockApiKey(_) => {
                        Err(ProviderAccountError::UnsupportedBedrockApiKeyAuth)
                    }
                    CodexAuth::Chatgpt(_)
                    | CodexAuth::ChatgptAuthTokens(_)
                    | CodexAuth::Headers(_)
                    | CodexAuth::AgentIdentity(_)
                    | CodexAuth::PersonalAccessToken(_) => {
                        let email = auth.get_account_email();
                        let plan_type = auth.account_plan_type();

                        plan_type
                            .map(|plan_type| ProviderAccount::Chatgpt { email, plan_type })
                            .ok_or(ProviderAccountError::MissingChatgptAccountDetails)
                    }
                })
                .transpose()?
        } else {
            None
        };

        Ok(ProviderAccountState {
            account,
            requires_openai_auth: self.info.requires_openai_auth,
        })
    }

    fn models_manager(
        &self,
        codex_home: PathBuf,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        let static_catalog = if self.info.is_anthropic() || self.info.is_claude_plan() {
            Some(config_model_catalog.unwrap_or_else(bundled_static_model_catalog))
        } else {
            config_model_catalog
        };

        match static_catalog {
            Some(model_catalog) => Arc::new(StaticModelsManager::new(
                self.auth_manager.clone(),
                model_catalog,
            )),
            None => {
                let endpoint = Arc::new(OpenAiModelsEndpoint::new(
                    self.info.clone(),
                    self.auth_manager.clone(),
                ));
                Arc::new(OpenAiModelsManager::new(
                    codex_home,
                    endpoint,
                    self.auth_manager.clone(),
                ))
            }
        }
    }

    fn models_manager_without_cache(
        &self,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        match config_model_catalog {
            Some(model_catalog) => Arc::new(StaticModelsManager::new(
                self.auth_manager.clone(),
                model_catalog,
            )),
            None => {
                let endpoint = Arc::new(OpenAiModelsEndpoint::new(
                    self.info.clone(),
                    self.auth_manager.clone(),
                ));
                Arc::new(OpenAiModelsManager::new_without_cache(
                    endpoint,
                    self.auth_manager.clone(),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use codex_http_client::HttpClientFactory;
    use codex_http_client::OutboundProxyPolicy;
    use codex_login::AuthCredentialsStoreMode;
    use codex_login::AuthKeyringBackendKind;
    use codex_login::auth::AgentIdentityAuthPolicy;
    use codex_login::auth::BedrockApiKeyAuth;
    use codex_login::login_with_provider_api_key;
    use codex_model_provider_info::AMBIENT_DEFAULT_MODEL;
    use codex_model_provider_info::ANTHROPIC_API_KEY_ENV_VAR;
    use codex_model_provider_info::BASETEN_DEFAULT_MODEL;
    use codex_model_provider_info::CLAUDE_FABLE_5_1_MODEL;
    use codex_model_provider_info::CLAUDE_FABLE_5_1_PLAN_MODEL;
    use codex_model_provider_info::CLAUDE_FABLE_5_MODEL;
    use codex_model_provider_info::CLAUDE_FABLE_5_PLAN_MODEL;
    use codex_model_provider_info::ModelProviderAwsAuthInfo;
    use codex_model_provider_info::OPENROUTER_DEFAULT_MODEL;
    use codex_model_provider_info::ProviderRuntimePolicy;
    use codex_model_provider_info::VERCEL_DEFAULT_MODEL;
    use codex_model_provider_info::WireApi;
    use codex_model_provider_info::create_oss_provider_with_base_url;
    use codex_models_manager::manager::RefreshStrategy;
    use codex_protocol::account::PlanType;
    use codex_protocol::config_types::ModelProviderAuthInfo;
    use codex_protocol::openai_models::ModelInfo;
    use codex_protocol::openai_models::ModelsResponse;
    use codex_protocol::protocol::SessionSource;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header_regex;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    use super::*;
    use crate::auth::AgentIdentitySessionFallback;

    struct EnvVarGuard {
        key: String,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: impl Into<String>, value: &str) -> Self {
            let key = key.into();
            let previous = std::env::var(&key).ok();
            // Tests use a unique variable name and restore it on drop.
            unsafe { std::env::set_var(&key, value) };
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => unsafe { std::env::set_var(&self.key, value) },
                None => unsafe { std::env::remove_var(&self.key) },
            }
        }
    }

    fn provider_info_with_command_auth() -> ModelProviderInfo {
        ModelProviderInfo {
            auth: Some(ModelProviderAuthInfo {
                command: "print-token".to_string(),
                args: Vec::new(),
                timeout_ms: NonZeroU64::new(5_000).expect("timeout should be non-zero"),
                refresh_interval_ms: 300_000,
                cwd: std::env::current_dir()
                    .expect("current dir should be available")
                    .try_into()
                    .expect("current dir should be absolute"),
            }),
            requires_openai_auth: false,
            ..ModelProviderInfo::create_openai_provider(/*base_url*/ None)
        }
    }

    fn test_codex_home() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("codex-model-provider-test-{}", std::process::id()))
    }

    fn provider_for(base_url: String) -> ModelProviderInfo {
        ModelProviderInfo {
            account: None,
            name: "mock".into(),
            base_url: Some(base_url),
            env_key: None,
            env_key_instructions: None,
            experimental_bearer_token: None,
            auth: None,
            aws: None,
            wire_api: WireApi::Responses,
            query_params: None,
            http_headers: None,
            env_http_headers: None,
            chat_completions_provider: None,
            request_max_retries: Some(0),
            stream_max_retries: Some(0),
            stream_idle_timeout_ms: Some(5_000),
            stream_actionable_timeout_ms: None,
            stream_long_failure_retry_threshold_ms: None,
            stream_long_failure_max_retries: None,
            runtime_policy: ProviderRuntimePolicy::default(),
            websocket_connect_timeout_ms: None,
            requires_openai_auth: false,
            supports_websockets: false,
            supports_standalone_web_search: false,
            billing: None,
        }
    }

    fn remote_model(slug: &str) -> ModelInfo {
        serde_json::from_value(json!({
            "slug": slug,
            "display_name": slug,
            "description": null,
            "default_reasoning_level": "medium",
            "supported_reasoning_levels": [],
            "shell_type": "shell_command",
            "visibility": "list",
            "supported_in_api": true,
            "priority": 0,
            "upgrade": null,
            "base_instructions": "base instructions",
            "support_verbosity": false,
            "default_verbosity": null,
            "apply_patch_tool_type": null,
            "truncation_policy": {"mode": "bytes", "limit": 10_000},
            "supports_parallel_tool_calls": false,
            "supports_image_detail_original": false,
            "context_window": 272_000,
            "max_context_window": 272_000,
            "experimental_supported_tools": [],
        }))
        .expect("valid model")
    }

    fn bedrock_api_key_auth() -> CodexAuth {
        CodexAuth::BedrockApiKey(BedrockApiKeyAuth {
            api_key: "bedrock-api-key-test".to_string(),
            region: "us-east-1".to_string(),
        })
    }

    #[tokio::test]
    async fn scoped_auth_ignores_scope_for_non_openai_provider() {
        let provider = create_model_provider(
            create_oss_provider_with_base_url("http://localhost:11434/v1", WireApi::Responses),
            /*auth_manager*/ None,
        );

        let auth = provider
            .api_auth_for_scope(ProviderAuthScope {
                agent_identity_policy: AgentIdentityAuthPolicy::JwtOnly,
                session_source: SessionSource::Cli,
                agent_identity_session_fallback: AgentIdentitySessionFallback::default(),
            })
            .await
            .expect("auth should resolve");

        assert!(auth.auth.to_auth_headers().is_empty());
    }

    #[tokio::test]
    async fn openai_api_key_env_fallback_reaches_only_first_party_openai_auth() {
        let env_fallback = || {
            Some(AuthManager::from_auth_for_testing(
                CodexAuth::from_api_key_env("sk-env", codex_login::OPENAI_API_KEY_ENV_VAR),
            ))
        };
        let oss = create_model_provider(
            create_oss_provider_with_base_url("http://localhost:11434/v1", WireApi::Responses),
            env_fallback(),
        );
        let openai = create_model_provider(
            ModelProviderInfo::create_openai_provider(/*base_url*/ None),
            env_fallback(),
        );

        assert!(oss.auth().await.is_none());
        assert!(
            oss.api_auth()
                .await
                .expect("oss auth")
                .to_auth_headers()
                .is_empty()
        );
        assert_eq!(
            openai
                .auth()
                .await
                .and_then(|auth| auth.api_key().map(str::to_string)),
            Some("sk-env".to_string())
        );

        // A saved key keeps its existing behavior on other providers.
        let stored = create_model_provider(
            create_oss_provider_with_base_url("http://localhost:11434/v1", WireApi::Responses),
            Some(AuthManager::from_auth_for_testing(CodexAuth::from_api_key(
                "sk-stored",
            ))),
        );
        assert!(stored.auth().await.is_some());
    }

    #[test]
    fn configured_provider_uses_default_capabilities() {
        let provider = create_model_provider(
            ModelProviderInfo::create_openai_provider(/*base_url*/ None),
            /*auth_manager*/ None,
        );

        assert_eq!(provider.capabilities(), ProviderCapabilities::default());
    }

    #[test]
    fn ambient_provider_disables_responses_only_capabilities() {
        let provider = create_model_provider(
            ModelProviderInfo::create_ambient_provider(),
            /*auth_manager*/ None,
        );

        assert_eq!(
            provider.capabilities(),
            ProviderCapabilities {
                namespace_tools: false,
                image_generation: false,
                web_search: false,
            }
        );
        assert_eq!(
            provider.approval_review_preferred_model(),
            AMBIENT_DEFAULT_MODEL
        );
        assert_eq!(
            provider.memory_extraction_preferred_model(),
            AMBIENT_DEFAULT_MODEL
        );
        assert_eq!(
            provider.memory_consolidation_preferred_model(),
            AMBIENT_DEFAULT_MODEL
        );
    }

    #[test]
    fn claude_plan_provider_uses_plan_model_for_helper_tasks() {
        let provider = create_model_provider(
            ModelProviderInfo::create_claude_plan_provider(),
            /*auth_manager*/ None,
        );

        assert_eq!(
            provider.approval_review_preferred_model(),
            CLAUDE_PLAN_MODEL
        );
        assert_eq!(
            provider.memory_extraction_preferred_model(),
            CLAUDE_PLAN_MODEL
        );
        assert_eq!(
            provider.memory_consolidation_preferred_model(),
            CLAUDE_PLAN_MODEL
        );
    }

    #[test]
    fn chat_wire_providers_disable_namespace_tools() {
        let mut provider_info = ModelProviderInfo::create_openai_provider(/*base_url*/ None);
        provider_info.wire_api = WireApi::Chat;
        let provider = create_model_provider(provider_info, /*auth_manager*/ None);

        assert_eq!(
            provider.capabilities(),
            ProviderCapabilities {
                namespace_tools: false,
                image_generation: false,
                web_search: true,
            }
        );
    }

    #[test]
    fn anthropic_wire_providers_disable_responses_only_namespace_tools() {
        let expected = ProviderCapabilities {
            namespace_tools: false,
            image_generation: false,
            web_search: true,
        };

        for provider_info in [
            ModelProviderInfo::create_claude_plan_provider(),
            ModelProviderInfo::create_anthropic_provider(),
        ] {
            let provider = create_model_provider(provider_info, /*auth_manager*/ None);
            assert_eq!(provider.capabilities(), expected);
        }
    }

    #[tokio::test]
    async fn claude_plan_provider_uses_static_model_catalog() {
        let provider = create_model_provider(
            ModelProviderInfo::create_claude_plan_provider(),
            /*auth_manager*/ None,
        );

        let manager =
            provider.models_manager(test_codex_home(), /*config_model_catalog*/ None);
        let catalog = manager
            .raw_model_catalog(
                RefreshStrategy::Online,
                HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            )
            .await;

        assert!(
            catalog
                .models
                .iter()
                .any(|model| model.slug == CLAUDE_PLAN_MODEL),
            "Claude Plan should not poll Anthropic /models during startup"
        );
        assert!(
            catalog
                .models
                .iter()
                .all(|model| model.slug != "claude-opus-4-8-plan"),
            "deprecated Claude Opus 4.8 Plan should not remain in the static catalog"
        );
        assert!(
            catalog
                .models
                .iter()
                .any(|model| model.slug == CLAUDE_FABLE_5_1_PLAN_MODEL),
            "Claude Fable 5.1 Plan should be available in the static Claude Plan catalog"
        );
        assert!(
            catalog
                .models
                .iter()
                .any(|model| model.slug == CLAUDE_FABLE_5_PLAN_MODEL),
            "Claude Fable Plan should be available in the static Claude Plan catalog"
        );
    }

    #[tokio::test]
    async fn anthropic_provider_uses_static_model_catalog() {
        let provider = create_model_provider(
            ModelProviderInfo::create_anthropic_provider(),
            /*auth_manager*/ None,
        );

        let manager =
            provider.models_manager(test_codex_home(), /*config_model_catalog*/ None);
        let catalog = manager
            .raw_model_catalog(
                RefreshStrategy::Online,
                HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            )
            .await;

        assert!(
            catalog
                .models
                .iter()
                .any(|model| model.slug == "claude-opus-5"),
            "Anthropic API-key models should not be refreshed through OpenAI-compatible /models"
        );
        assert!(
            catalog
                .models
                .iter()
                .all(|model| model.slug != "claude-opus-4-8"),
            "deprecated Claude Opus 4.8 should not remain in the static catalog"
        );
        assert!(
            catalog
                .models
                .iter()
                .any(|model| model.slug == CLAUDE_FABLE_5_1_MODEL),
            "Claude Fable 5.1 should be available in the static Anthropic API-key catalog"
        );
        assert!(
            catalog
                .models
                .iter()
                .any(|model| model.slug == CLAUDE_FABLE_5_MODEL),
            "Claude Fable should be available in the static Anthropic API-key catalog"
        );
    }

    #[test]
    fn zai_provider_exposes_native_web_search_only() {
        let provider = create_model_provider(
            ModelProviderInfo::create_zai_provider(),
            /*auth_manager*/ None,
        );

        assert_eq!(
            provider.capabilities(),
            ProviderCapabilities {
                namespace_tools: false,
                image_generation: false,
                web_search: true,
            }
        );
    }

    #[test]
    fn openrouter_provider_enables_server_web_search_and_uses_openrouter_defaults() {
        let provider = create_model_provider(
            ModelProviderInfo::create_openrouter_provider(),
            /*auth_manager*/ None,
        );

        assert_eq!(
            provider.capabilities(),
            ProviderCapabilities {
                namespace_tools: false,
                image_generation: false,
                web_search: true,
            }
        );
        assert_eq!(
            provider.approval_review_preferred_model(),
            OPENROUTER_DEFAULT_MODEL
        );
        assert_eq!(
            provider.memory_extraction_preferred_model(),
            OPENROUTER_DEFAULT_MODEL
        );
        assert_eq!(
            provider.memory_consolidation_preferred_model(),
            OPENROUTER_DEFAULT_MODEL
        );
    }

    #[test]
    fn baseten_provider_disables_hosted_tools_and_uses_baseten_defaults() {
        let provider = create_model_provider(
            ModelProviderInfo::create_baseten_provider(),
            /*auth_manager*/ None,
        );

        assert_eq!(
            provider.capabilities(),
            ProviderCapabilities {
                namespace_tools: false,
                image_generation: false,
                web_search: false,
            }
        );
        assert_eq!(
            provider.approval_review_preferred_model(),
            BASETEN_DEFAULT_MODEL
        );
        assert_eq!(
            provider.memory_extraction_preferred_model(),
            BASETEN_DEFAULT_MODEL
        );
        assert_eq!(
            provider.memory_consolidation_preferred_model(),
            BASETEN_DEFAULT_MODEL
        );
    }

    #[test]
    fn kimi_code_provider_disables_hosted_tools_and_uses_k3_defaults() {
        let provider = create_model_provider(
            ModelProviderInfo::create_kimi_code_provider(),
            /*auth_manager*/ None,
        );

        assert_eq!(
            provider.capabilities(),
            ProviderCapabilities {
                namespace_tools: false,
                image_generation: false,
                web_search: false,
            }
        );
        assert_eq!(
            provider.approval_review_preferred_model(),
            KIMI_CODE_K3_MODEL
        );
        assert_eq!(
            provider.memory_extraction_preferred_model(),
            KIMI_CODE_K3_MODEL
        );
        assert_eq!(
            provider.memory_consolidation_preferred_model(),
            KIMI_CODE_K3_MODEL
        );
    }

    #[test]
    fn vercel_provider_disables_hosted_tools_and_uses_vercel_defaults() {
        let provider = create_model_provider(
            ModelProviderInfo::create_vercel_provider(),
            /*auth_manager*/ None,
        );

        assert_eq!(
            provider.capabilities(),
            ProviderCapabilities {
                namespace_tools: false,
                image_generation: false,
                web_search: false,
            }
        );
        assert_eq!(
            provider.approval_review_preferred_model(),
            VERCEL_DEFAULT_MODEL
        );
        assert_eq!(
            provider.memory_extraction_preferred_model(),
            VERCEL_DEFAULT_MODEL
        );
        assert_eq!(
            provider.memory_consolidation_preferred_model(),
            VERCEL_DEFAULT_MODEL
        );
    }

    #[test]
    fn configured_provider_uses_default_approval_review_preferred_model() {
        let provider = create_model_provider(
            ModelProviderInfo::create_openai_provider(/*base_url*/ None),
            /*auth_manager*/ None,
        );

        assert_eq!(
            provider.approval_review_preferred_model(),
            DEFAULT_APPROVAL_REVIEW_PREFERRED_MODEL
        );
    }

    #[test]
    fn configured_builtin_providers_keep_helper_models_on_their_own_backends() {
        let cases = [
            (
                ModelProviderInfo::create_ambient_provider(),
                AMBIENT_DEFAULT_MODEL,
            ),
            (
                ModelProviderInfo::create_pfterminal_plan_provider(),
                AMBIENT_DEFAULT_MODEL,
            ),
            (
                ModelProviderInfo::create_kimi_code_provider(),
                KIMI_CODE_K3_MODEL,
            ),
            (
                ModelProviderInfo::create_claude_plan_provider(),
                CLAUDE_PLAN_MODEL,
            ),
            (
                ModelProviderInfo::create_anthropic_provider(),
                ANTHROPIC_DEFAULT_MODEL,
            ),
            (ModelProviderInfo::create_zai_provider(), ZAI_DEFAULT_MODEL),
            (
                ModelProviderInfo::create_zai_anthropic_provider(),
                ZAI_DEFAULT_MODEL,
            ),
            (
                ModelProviderInfo::create_openrouter_provider(),
                OPENROUTER_DEFAULT_MODEL,
            ),
            (
                ModelProviderInfo::create_openrouter_anthropic_provider(),
                OPENROUTER_DEFAULT_MODEL,
            ),
            (
                ModelProviderInfo::create_deepseek_provider(),
                DEEPSEEK_DEFAULT_MODEL,
            ),
            (
                ModelProviderInfo::create_meta_provider(),
                META_DEFAULT_MODEL,
            ),
            (
                ModelProviderInfo::create_baseten_provider(),
                BASETEN_DEFAULT_MODEL,
            ),
            (
                ModelProviderInfo::create_baseten_anthropic_provider(),
                BASETEN_DEFAULT_MODEL,
            ),
            (
                ModelProviderInfo::create_vercel_provider(),
                VERCEL_DEFAULT_MODEL,
            ),
            (
                ModelProviderInfo::create_vercel_anthropic_provider(),
                VERCEL_DEFAULT_MODEL,
            ),
            (
                ModelProviderInfo::create_vercel_anthropic_fast_provider(),
                VERCEL_DEFAULT_MODEL,
            ),
        ];

        for (provider_info, expected_model) in cases {
            let provider = create_model_provider(provider_info, /*auth_manager*/ None);
            assert_eq!(provider.approval_review_preferred_model(), expected_model);
            assert_eq!(provider.memory_extraction_preferred_model(), expected_model);
            assert_eq!(
                provider.memory_consolidation_preferred_model(),
                expected_model
            );
        }
    }

    #[test]
    fn custom_provider_reusing_builtin_credentials_does_not_inherit_builtin_models() {
        let provider = create_model_provider(
            ModelProviderInfo {
                name: "Private gateway".to_string(),
                base_url: Some("https://private.example/v1".to_string()),
                env_key: Some("OPENROUTER_API_KEY".to_string()),
                ..ModelProviderInfo::default()
            },
            /*auth_manager*/ None,
        );

        assert_eq!(
            provider.approval_review_preferred_model(),
            DEFAULT_APPROVAL_REVIEW_PREFERRED_MODEL
        );
        assert_eq!(
            provider.memory_extraction_preferred_model(),
            DEFAULT_MEMORY_EXTRACTION_PREFERRED_MODEL
        );
        assert_eq!(
            provider.memory_consolidation_preferred_model(),
            DEFAULT_MEMORY_CONSOLIDATION_PREFERRED_MODEL
        );
        assert_eq!(
            provider.resolve_background_helper_model(
                DEFAULT_MEMORY_EXTRACTION_PREFERRED_MODEL,
                DEFAULT_MEMORY_EXTRACTION_PREFERRED_MODEL,
                "private-active-model",
            ),
            "private-active-model"
        );
    }

    #[test]
    fn custom_provider_named_openai_still_uses_its_active_helper_model() {
        let provider = create_model_provider(
            ModelProviderInfo {
                name: "OpenAI".to_string(),
                base_url: Some("https://private.example/v1".to_string()),
                ..ModelProviderInfo::default()
            },
            /*auth_manager*/ None,
        );

        assert_eq!(
            provider.resolve_background_helper_model(
                DEFAULT_APPROVAL_REVIEW_PREFERRED_MODEL,
                DEFAULT_APPROVAL_REVIEW_PREFERRED_MODEL,
                "private-active-model",
            ),
            "private-active-model"
        );
    }

    #[tokio::test]
    async fn configured_provider_runtime_base_url_uses_configured_base_url() {
        let provider = create_model_provider(
            provider_for("https://example.test/v1".to_string()),
            /*auth_manager*/ None,
        );

        assert_eq!(
            provider
                .runtime_base_url()
                .await
                .expect("runtime base URL should resolve"),
            Some("https://example.test/v1".to_string())
        );
    }

    #[test]
    fn create_model_provider_builds_command_auth_manager_without_base_manager() {
        let provider = create_model_provider(
            provider_info_with_command_auth(),
            /*auth_manager*/ None,
        );

        let auth_manager = provider
            .auth_manager()
            .expect("command auth provider should have an auth manager");

        assert!(auth_manager.has_external_auth());
    }

    #[tokio::test]
    async fn command_auth_failure_cannot_fall_back_to_anonymous_provider_requests() {
        let mut provider_info = provider_info_with_command_auth();
        let command_auth = provider_info.auth.as_mut().expect("command auth");
        command_auth.command = std::env::temp_dir()
            .join(format!(
                "missing-provider-auth-helper-{}",
                std::process::id()
            ))
            .to_string_lossy()
            .into_owned();
        let expected_command = command_auth.command.clone();
        let provider = create_model_provider(provider_info, /*auth_manager*/ None);

        let error = match provider.api_auth().await {
            Ok(_) => panic!("missing command auth must not yield anonymous request auth"),
            Err(error) => error,
        };

        assert!(
            error.to_string().contains(&expected_command),
            "provider helper failure should identify the failed command: {error}"
        );
    }

    #[test]
    fn create_model_provider_does_not_use_openai_auth_manager_for_amazon_bedrock_provider() {
        let provider = create_model_provider(
            ModelProviderInfo::create_amazon_bedrock_provider(Some(ModelProviderAwsAuthInfo {
                profile: Some("codex-bedrock".to_string()),
                region: None,
            })),
            Some(AuthManager::from_auth_for_testing(CodexAuth::from_api_key(
                "openai-api-key",
            ))),
        );

        assert!(provider.auth_manager().is_none());
    }

    #[tokio::test]
    async fn create_model_provider_uses_managed_auth_for_amazon_bedrock_provider() {
        let auth = bedrock_api_key_auth();
        let provider = create_model_provider(
            ModelProviderInfo::create_amazon_bedrock_provider(/*aws*/ None),
            Some(AuthManager::from_auth_for_testing(auth.clone())),
        );

        assert_eq!(provider.auth().await, Some(auth));
    }

    #[tokio::test]
    async fn configured_provider_prefers_env_key_over_stored_provider_key() {
        let env_key = format!("PFT_PROVIDER_ENV_PRECEDENCE_{}", std::process::id());
        let codex_home = test_codex_home().join(&env_key);
        std::fs::create_dir_all(&codex_home).expect("temp codex home should be created");
        let _guard = EnvVarGuard::set(env_key.clone(), "env-provider-key");
        login_with_provider_api_key(
            &codex_home,
            &env_key,
            "stored-provider-key",
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("stored provider key should be written");

        let provider = create_model_provider(
            ModelProviderInfo {
                env_key: Some(env_key),
                ..provider_for("https://example.test/v1".to_string())
            },
            Some(AuthManager::from_auth_for_testing_with_home(
                CodexAuth::create_dummy_chatgpt_auth_for_testing(),
                codex_home,
            )),
        );

        let auth = provider
            .api_auth()
            .await
            .expect("provider auth should resolve");

        assert_eq!(
            auth.to_auth_headers()
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer env-provider-key")
        );
    }

    #[tokio::test]
    async fn pf_27_s05_brokered_provider_hands_out_a_placeholder_not_the_key() {
        let env_key = format!("PFT_PROVIDER_BROKERED_{}", std::process::id());
        let codex_home = test_codex_home().join(&env_key);
        std::fs::create_dir_all(&codex_home).expect("temp codex home should be created");
        let _guard = EnvVarGuard::set(env_key.clone(), "env-provider-key");
        login_with_provider_api_key(
            &codex_home,
            &env_key,
            "stored-provider-key",
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("stored provider key should be written");
        let provider = create_model_provider(
            ModelProviderInfo {
                env_key: Some(env_key.clone()),
                ..provider_for("https://example.test/v1".to_string())
            },
            Some(AuthManager::from_auth_for_testing_with_home(
                CodexAuth::create_dummy_chatgpt_auth_for_testing(),
                codex_home,
            )),
        );
        let broker =
            std::sync::Arc::new(crate::model_key_broker::test_support::RecordingBroker::default());
        crate::model_key_broker::set_test_model_key_broker(Some(broker.clone()));

        let auth = provider.auth().await.expect("placeholder auth");
        let api_auth = provider.api_auth().await.expect("brokered auth");
        let account = provider.account_state().expect("account state");
        crate::model_key_broker::set_test_model_key_broker(/*broker*/ None);

        assert_eq!(auth.api_key(), Some(crate::BROKERED_KEY_PLACEHOLDER));
        assert!(api_auth.to_auth_headers().is_empty());
        assert_eq!(account.account, Some(ProviderAccount::ApiKey));
        let uses = broker.uses.lock().expect("uses");
        assert_eq!(uses.len(), 1);
        assert!(matches!(
            &uses[0].source,
            crate::BrokeredKeySource::ProviderKey { provider_key_id, .. } if *provider_key_id == env_key
        ));
        let debug = format!("{uses:?}");
        assert!(!debug.contains("env-provider-key") && !debug.contains("stored-provider-key"));
    }

    #[tokio::test]
    async fn configured_provider_observes_provider_key_saved_after_missing_lookup() {
        let env_key = format!("PFT_PROVIDER_LATE_KEY_{}", std::process::id());
        let codex_home = test_codex_home().join(&env_key);
        std::fs::create_dir_all(&codex_home).expect("temp codex home should be created");
        unsafe { std::env::remove_var(&env_key) };

        let provider = create_model_provider(
            ModelProviderInfo {
                env_key: Some(env_key.clone()),
                ..provider_for("https://example.test/v1".to_string())
            },
            Some(AuthManager::from_auth_for_testing_with_home(
                CodexAuth::create_dummy_chatgpt_auth_for_testing(),
                codex_home.clone(),
            )),
        );

        let missing_auth = match provider.api_auth().await {
            Ok(_) => panic!("provider auth should fail before key exists"),
            Err(err) => err,
        };
        assert!(
            missing_auth.to_string().contains(&env_key),
            "error should name missing provider key {env_key}: {missing_auth}"
        );

        login_with_provider_api_key(
            &codex_home,
            &env_key,
            "stored-provider-key",
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("stored provider key should be written");

        let auth = provider
            .api_auth()
            .await
            .expect("same provider instance should observe newly stored key");

        assert_eq!(
            auth.to_auth_headers()
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer stored-provider-key")
        );
    }

    #[tokio::test]
    async fn anthropic_provider_env_key_uses_x_api_key_header() {
        let _guard = EnvVarGuard::set(ANTHROPIC_API_KEY_ENV_VAR, "env-anthropic-key");
        let provider = create_model_provider(
            ModelProviderInfo::create_anthropic_provider(),
            Some(AuthManager::from_auth_for_testing(
                CodexAuth::create_dummy_chatgpt_auth_for_testing(),
            )),
        );

        let auth = provider
            .api_auth()
            .await
            .expect("provider auth should resolve");
        let headers = auth.to_auth_headers();

        assert_eq!(headers.get(http::header::AUTHORIZATION), None);
        assert_eq!(
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok()),
            Some("env-anthropic-key")
        );
    }

    #[test]
    fn openai_provider_returns_unauthenticated_openai_account_state() {
        let provider = create_model_provider(
            ModelProviderInfo::create_openai_provider(/*base_url*/ None),
            /*auth_manager*/ None,
        );

        assert_eq!(
            provider.account_state(),
            Ok(ProviderAccountState {
                account: None,
                requires_openai_auth: true,
            })
        );
    }

    #[test]
    fn openai_provider_returns_api_key_account_state() {
        let provider = create_model_provider(
            ModelProviderInfo::create_openai_provider(/*base_url*/ None),
            Some(AuthManager::from_auth_for_testing(CodexAuth::from_api_key(
                "openai-api-key",
            ))),
        );

        assert_eq!(
            provider.account_state(),
            Ok(ProviderAccountState {
                account: Some(ProviderAccount::ApiKey),
                requires_openai_auth: true,
            })
        );
    }

    #[test]
    fn openai_provider_returns_chatgpt_account_state_without_email() {
        let provider = create_model_provider(
            ModelProviderInfo::create_openai_provider(/*base_url*/ None),
            Some(AuthManager::from_auth_for_testing(
                CodexAuth::create_dummy_chatgpt_auth_for_testing(),
            )),
        );

        assert_eq!(
            provider.account_state(),
            Ok(ProviderAccountState {
                account: Some(ProviderAccount::Chatgpt {
                    email: None,
                    plan_type: PlanType::Unknown,
                }),
                requires_openai_auth: true,
            })
        );
    }

    #[test]
    fn openai_provider_rejects_bedrock_api_key_account_state() {
        let provider = create_model_provider(
            ModelProviderInfo::create_openai_provider(/*base_url*/ None),
            Some(AuthManager::from_auth_for_testing(bedrock_api_key_auth())),
        );

        assert_eq!(
            provider.account_state(),
            Err(ProviderAccountError::UnsupportedBedrockApiKeyAuth)
        );
    }

    #[test]
    fn custom_non_openai_provider_returns_no_account_state() {
        let provider = create_model_provider(
            ModelProviderInfo {
                name: "Custom".to_string(),
                base_url: Some("http://localhost:1234/v1".to_string()),
                wire_api: WireApi::Responses,
                requires_openai_auth: false,
                ..Default::default()
            },
            /*auth_manager*/ None,
        );

        assert_eq!(
            provider.account_state(),
            Ok(ProviderAccountState {
                account: None,
                requires_openai_auth: false,
            })
        );
    }

    #[test]
    fn amazon_bedrock_provider_returns_bedrock_account_state() {
        let provider = create_model_provider(
            ModelProviderInfo::create_amazon_bedrock_provider(/*aws*/ None),
            /*auth_manager*/ None,
        );

        assert_eq!(
            provider.account_state(),
            Ok(ProviderAccountState {
                account: Some(ProviderAccount::AmazonBedrock {
                    uses_codex_managed_credentials: false,
                }),
                requires_openai_auth: false,
            })
        );
    }

    #[tokio::test]
    async fn amazon_bedrock_provider_creates_static_models_manager() {
        let provider = create_model_provider(
            ModelProviderInfo::create_amazon_bedrock_provider(/*aws*/ None),
            /*auth_manager*/ None,
        );
        let manager =
            provider.models_manager(test_codex_home(), /*config_model_catalog*/ None);
        let uncached_manager =
            provider.models_manager_without_cache(/*config_model_catalog*/ None);

        let catalog = manager
            .raw_model_catalog(
                RefreshStrategy::Online,
                HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            )
            .await;
        let uncached_catalog = uncached_manager
            .raw_model_catalog(
                RefreshStrategy::Online,
                HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            )
            .await;
        assert_eq!(uncached_catalog, catalog);
        let models = catalog
            .models
            .iter()
            .map(|model| (model.slug.as_str(), model.display_name.as_str()))
            .collect::<Vec<_>>();

        assert_eq!(
            models,
            vec![
                ("openai.gpt-5.6-sol", "GPT-5.6 Sol"),
                ("openai.gpt-5.6-terra", "GPT-5.6 Terra"),
                ("openai.gpt-5.6-luna", "GPT-5.6 Luna"),
                ("openai.gpt-5.5", "GPT-5.5"),
                ("openai.gpt-5.4", "GPT-5.4"),
            ]
        );

        let available_models = manager
            .list_models(
                RefreshStrategy::Online,
                HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            )
            .await;
        assert_eq!(
            available_models
                .iter()
                .map(|preset| preset.model.as_str())
                .collect::<Vec<_>>(),
            vec![
                "openai.gpt-5.6-sol",
                "openai.gpt-5.6-terra",
                "openai.gpt-5.6-luna",
                "openai.gpt-5.5",
                "openai.gpt-5.4",
            ]
        );

        let default_model = available_models
            .iter()
            .find(|preset| preset.is_default)
            .expect("Bedrock catalog should have a default model");

        assert_eq!(default_model.model, "openai.gpt-5.6-sol");
    }

    #[tokio::test]
    async fn configured_bedrock_catalog_only_allows_default_service_tier() {
        let configured_model = codex_models_manager::bundled_models_response()
            .expect("bundled models should parse")
            .models
            .into_iter()
            .find(|model| model.slug == "gpt-5.5")
            .expect("bundled models should include GPT-5.5");
        assert!(!configured_model.additional_speed_tiers.is_empty());
        assert!(!configured_model.service_tiers.is_empty());

        let provider = create_model_provider(
            ModelProviderInfo::create_amazon_bedrock_provider(/*aws*/ None),
            /*auth_manager*/ None,
        );
        let manager = provider.models_manager(
            test_codex_home(),
            Some(ModelsResponse {
                models: vec![configured_model],
            }),
        );

        let catalog = manager
            .raw_model_catalog(
                RefreshStrategy::Online,
                HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            )
            .await;

        assert_eq!(catalog.models.len(), 1);
        assert_eq!(catalog.models[0].slug, "gpt-5.5");
        assert_eq!(
            catalog.models[0].additional_speed_tiers,
            Vec::<String>::new()
        );
        assert_eq!(catalog.models[0].service_tiers, Vec::new());
        assert_eq!(catalog.models[0].default_service_tier, None);
    }

    #[tokio::test]
    async fn configured_provider_models_manager_uses_provider_bearer_token() {
        let server = MockServer::start().await;
        let remote_models = vec![remote_model("provider-model")];

        Mock::given(method("GET"))
            .and(path("/models"))
            .and(header_regex("Authorization", "Bearer provider-token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_json(ModelsResponse {
                        models: remote_models.clone(),
                    }),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mut provider_info = provider_for(server.uri());
        provider_info.experimental_bearer_token = Some("provider-token".to_string());
        let provider = create_model_provider(
            provider_info,
            Some(AuthManager::from_auth_for_testing(
                CodexAuth::create_dummy_chatgpt_auth_for_testing(),
            )),
        );

        let manager =
            provider.models_manager(test_codex_home(), /*config_model_catalog*/ None);
        let catalog = manager
            .raw_model_catalog(
                RefreshStrategy::Online,
                HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            )
            .await;

        assert!(
            catalog
                .models
                .iter()
                .any(|model| model.slug == "provider-model")
        );
    }
}
