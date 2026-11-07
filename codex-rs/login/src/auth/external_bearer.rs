use super::manager::CodexAuth;
use super::manager::ExternalAuth;
use super::manager::ExternalAuthFuture;
use super::manager::ExternalAuthRefreshContext;
use codex_protocol::config_types::ModelProviderAuthInfo;
use std::fmt;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;
use tokio::process::Command;
use tokio::sync::Mutex;

#[derive(Clone)]
pub(crate) struct BearerTokenRefresher {
    state: Arc<ExternalBearerAuthState>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ExternalBearerCachePolicy {
    #[default]
    Timed,
    FreshPerRequest,
    InvalidateOnChange(PathBuf),
}

impl BearerTokenRefresher {
    /// `account` (PF-84) is the named account the command must serve; it is
    /// passed as `CORBANU_PROVIDER_ACCOUNT` and never inherited otherwise.
    pub(crate) fn new(
        config: ModelProviderAuthInfo,
        cache_policy: ExternalBearerCachePolicy,
        account: Option<ExternalBearerAccount>,
    ) -> Self {
        Self {
            state: Arc::new(ExternalBearerAuthState::new(config, cache_policy, account)),
        }
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "external bearer cache misses intentionally hold cached_token across the provider command to avoid duplicate refreshes"
    )]
    async fn resolve(&self) -> io::Result<CodexAuth> {
        let access_token = {
            let mut cached = self.state.cached_token.lock().await;
            let revision_before = self.state.cache_revision();
            if self.state.cache_policy != ExternalBearerCachePolicy::FreshPerRequest
                && let Some(cached_token) = cached.as_ref()
            {
                let should_use_cached_token = match self.state.config.refresh_interval() {
                    Some(refresh_interval) => cached_token.fetched_at.elapsed() < refresh_interval,
                    None => true,
                };
                if should_use_cached_token && cached_token.revision == revision_before {
                    return Ok(CodexAuth::from_api_key(cached_token.access_token.as_str()));
                }
            }

            let (access_token, revision_after) = self
                .state
                .run_for_stable_revision(/*force_refresh*/ false, revision_before)
                .await?;
            if self.state.cache_policy != ExternalBearerCachePolicy::FreshPerRequest
                && revision_after.is_some()
            {
                *cached = Some(CachedExternalBearerToken {
                    access_token: access_token.clone(),
                    fetched_at: Instant::now(),
                    revision: revision_after,
                });
            } else {
                *cached = None;
            }
            access_token
        };
        Ok(CodexAuth::from_api_key(access_token.as_str()))
    }

    async fn refresh(&self, _context: ExternalAuthRefreshContext) -> io::Result<CodexAuth> {
        let revision_before = self.state.cache_revision();
        let (access_token, revision_after) = self
            .state
            .run_for_stable_revision(/*force_refresh*/ true, revision_before)
            .await?;
        if self.state.cache_policy != ExternalBearerCachePolicy::FreshPerRequest
            && revision_after.is_some()
        {
            let mut cached = self.state.cached_token.lock().await;
            *cached = Some(CachedExternalBearerToken {
                access_token: access_token.clone(),
                fetched_at: Instant::now(),
                revision: revision_after,
            });
        } else {
            *self.state.cached_token.lock().await = None;
        }
        Ok(CodexAuth::from_api_key(access_token.as_str()))
    }
}

impl ExternalAuth for BearerTokenRefresher {
    fn resolve(&self) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(BearerTokenRefresher::resolve(self))
    }

    fn refresh(&self, context: ExternalAuthRefreshContext) -> ExternalAuthFuture<'_, CodexAuth> {
        Box::pin(BearerTokenRefresher::refresh(self, context))
    }
}

impl fmt::Debug for BearerTokenRefresher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BearerTokenRefresher")
            .finish_non_exhaustive()
    }
}

/// PF-84: the named account an `auth.command` provider serves.
#[derive(Clone, Debug)]
pub struct ExternalBearerAccount {
    pub provider_id: String,
    pub name: String,
    /// The home whose registry must hold this account before the command
    /// runs. `None` skips the check (no home is known).
    pub registry_home: Option<PathBuf>,
    pub server: ExternalBearerAccountServer,
}

/// Which command serves a named account, and so how the account reaches it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalBearerAccountServer {
    /// A user `auth.command`: the account is passed in `CORBANU_PROVIDER_ACCOUNT`
    /// only, and the registry must hold a `command` entry for it.
    Command,
    /// Corbanu's own Claude Plan helper (`corbanu internal-claude-oauth-token`):
    /// the registry must hold a Claude token or config dir for the account.
    /// The account also goes in argv (see [`is_claude_plan_token_helper`]).
    ClaudePlanHelper,
}

impl ExternalBearerAccountServer {
    fn enrolled_kinds(self) -> &'static [codex_vault::ProviderAccountKind] {
        match self {
            Self::Command => &[codex_vault::ProviderAccountKind::Command],
            Self::ClaudePlanHelper => &[
                codex_vault::ProviderAccountKind::ClaudeOauthToken,
                codex_vault::ProviderAccountKind::ClaudeConfigDir,
            ],
        }
    }

    fn add_hint(self) -> &'static str {
        match self {
            Self::Command => "--kind command",
            Self::ClaudePlanHelper => "--kind claude-token (or --kind claude-config-dir)",
        }
    }
}

impl ExternalBearerAccount {
    /// Refuses an account the registry does not hold, without running the command.
    pub fn ensure_enrolled(&self) -> io::Result<()> {
        let Some(home) = self.registry_home.as_deref() else {
            return Ok(());
        };
        for kind in self.server.enrolled_kinds() {
            let enrolled = super::provider_key_vault::provider_account_holds(
                home,
                &self.provider_id,
                &self.name,
                *kind,
            )
            .map_err(|error| {
                io::Error::other(format!(
                    "account `{}` of provider `{}` is unavailable: {error}",
                    self.name, self.provider_id
                ))
            })?;
            if enrolled {
                return Ok(());
            }
        }
        Err(io::Error::other(format!(
            "account `{name}` of provider `{id}` is not configured; add it with \
             `corbanu account add {id} {name} {hint}` or choose another account",
            name = self.name,
            id = self.provider_id,
            hint = self.server.add_hint(),
        )))
    }
}

struct ExternalBearerAuthState {
    config: ModelProviderAuthInfo,
    account: Option<ExternalBearerAccount>,
    cache_policy: ExternalBearerCachePolicy,
    cached_token: Mutex<Option<CachedExternalBearerToken>>,
}

impl ExternalBearerAuthState {
    fn new(
        config: ModelProviderAuthInfo,
        cache_policy: ExternalBearerCachePolicy,
        account: Option<ExternalBearerAccount>,
    ) -> Self {
        Self {
            config,
            account,
            cache_policy,
            cached_token: Mutex::new(None),
        }
    }

    fn cache_revision(&self) -> Option<Vec<u8>> {
        match &self.cache_policy {
            ExternalBearerCachePolicy::Timed => Some(Vec::new()),
            ExternalBearerCachePolicy::FreshPerRequest => None,
            ExternalBearerCachePolicy::InvalidateOnChange(path) => {
                let revision = std::fs::read(path).ok()?;
                (!revision.is_empty() && revision.len() <= 4096).then_some(revision)
            }
        }
    }

    async fn run_for_stable_revision(
        &self,
        force_refresh: bool,
        mut revision_before: Option<Vec<u8>>,
    ) -> io::Result<(String, Option<Vec<u8>>)> {
        for _ in 0..3 {
            if let Some(account) = &self.account {
                account.ensure_enrolled()?;
            }
            let access_token = run_provider_auth_command(
                &self.config,
                force_refresh,
                self.account.as_ref().map(|account| account.name.as_str()),
            )
            .await?;
            let revision_after = self.cache_revision();
            if !matches!(
                &self.cache_policy,
                ExternalBearerCachePolicy::InvalidateOnChange(_)
            ) || revision_before == revision_after
            {
                return Ok((access_token, revision_after));
            }
            revision_before = revision_after;
        }
        Err(io::Error::other(
            "provider authentication selection changed repeatedly during resolution",
        ))
    }
}

struct CachedExternalBearerToken {
    access_token: String,
    fetched_at: Instant,
    revision: Option<Vec<u8>>,
}

/// PF-84: names the account a provider auth command must serve.
pub const PROVIDER_ACCOUNT_ENV_VAR: &str = "CORBANU_PROVIDER_ACCOUNT";

/// The hidden `corbanu` subcommand that prints a Claude Plan token.
pub use codex_model_provider_info::CLAUDE_PLAN_TOKEN_HELPER_SUBCOMMAND;

/// Whether `config` runs Corbanu's own Claude Plan token helper, which takes
/// the account in argv as well (#414).
pub fn is_claude_plan_token_helper(config: &ModelProviderAuthInfo) -> bool {
    config.args.first().map(String::as_str) == Some(CLAUDE_PLAN_TOKEN_HELPER_SUBCOMMAND)
}

async fn run_provider_auth_command(
    config: &ModelProviderAuthInfo,
    force_refresh: bool,
    account: Option<&str>,
) -> io::Result<String> {
    let program = resolve_provider_auth_program(&config.command, &config.cwd)?;
    let mut command = Command::new(&program);
    command.args(&config.args);
    // A helper that predates named accounts ignores the environment variable
    // and would print the default account's token; in argv it is rejected.
    let helper_account = account.filter(|_| is_claude_plan_token_helper(config));
    if let Some(account) = helper_account {
        command.args(["--account", account, "--enable", "named_accounts"]);
    }
    command
        .current_dir(config.cwd.as_path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    match account {
        Some(account) => command.env(PROVIDER_ACCOUNT_ENV_VAR, account),
        None => command.env_remove(PROVIDER_ACCOUNT_ENV_VAR),
    };
    if force_refresh {
        command.env("PFTERMINAL_PROVIDER_AUTH_FORCE_REFRESH", "1");
    } else {
        command.env_remove("PFTERMINAL_PROVIDER_AUTH_FORCE_REFRESH");
    }

    // Test-only timing diagnostics for #1201. Never log the helper's configuration,
    // account, stdout or stderr: even a failing auth command can contain a token.
    // Remove these diagnostics or migrate them to tracing once the native Windows lane
    // has stayed clean under the longer test-helper timeout (#1201).
    #[cfg(test)]
    let (attempt_id, attempt_started) = {
        use std::sync::atomic::AtomicU64;
        use std::sync::atomic::Ordering;

        static NEXT_ATTEMPT_ID: AtomicU64 = AtomicU64::new(1);
        let attempt_id = NEXT_ATTEMPT_ID.fetch_add(1, Ordering::Relaxed);
        let attempt_started = Instant::now();
        eprintln!(
            "provider_auth_helper attempt={attempt_id} phase=dispatch force_refresh={force_refresh}"
        );
        (attempt_id, attempt_started)
    };

    let output = tokio::time::timeout(config.timeout(), command.output()).await;
    #[cfg(test)]
    {
        let outcome = match &output {
            Err(_) => "timeout",
            Ok(Err(_)) => "io_error",
            Ok(Ok(output)) if output.status.success() => "success",
            Ok(Ok(_)) => "nonzero_exit",
        };
        let elapsed_us = attempt_started.elapsed().as_micros();
        eprintln!(
            "provider_auth_helper attempt={attempt_id} phase=complete outcome={outcome} elapsed_us={elapsed_us}"
        );
    }
    let output = output
        .map_err(|_| {
            io::Error::other(format!(
                "provider auth command `{}` timed out after {} ms",
                config.command,
                config.timeout_ms.get()
            ))
        })?
        .map_err(|err| {
            io::Error::other(format!(
                "provider auth command `{}` failed to start: {err}",
                config.command
            ))
        })?;

    if !output.status.success() {
        let status = output.status;
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stderr_suffix = helper_stderr_suffix(config, &stderr);
        if let Some(account) = helper_account
            && stderr.contains("unexpected argument '--account'")
        {
            // Retrying cannot help; `Unsupported` marks the failure permanent.
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "provider auth command `{}` predates named accounts, so account `{}` was \
                     refused instead of using the default account; update it or put a current \
                     `corbanu` first on PATH",
                    config.command, account,
                ),
            ));
        }
        let message = format!(
            "provider auth command `{}` exited with status {status}{stderr_suffix}",
            config.command
        );
        // #616: Corbanu's own helper could not read the Corbanu home. That is
        // usually temporary, so callers may wait for access to return.
        if is_corbanu_internal_helper(config)
            && status.code() == Some(codex_protocol::home_unreadable::HOME_UNREADABLE_EXIT_CODE)
        {
            return Err(codex_protocol::home_unreadable::home_unreadable_io_error(
                message,
            ));
        }
        return Err(io::Error::other(message));
    }

    let stdout = String::from_utf8(output.stdout).map_err(|_| {
        io::Error::other(format!(
            "provider auth command `{}` wrote non-UTF-8 data to stdout",
            config.command
        ))
    })?;
    let access_token = stdout.trim().to_string();
    if access_token.is_empty() {
        return Err(io::Error::other(format!(
            "provider auth command `{}` produced an empty token",
            config.command
        )));
    }

    Ok(access_token)
}

/// The part of a failed helper's stderr that may go into its error (#548).
/// The error is shown and logged, so another program's stderr, which may
/// echo anything, is withheld. Corbanu's own `internal-*` helpers write
/// actionable messages; theirs is kept (the end, where the error is),
/// capped and with credential-shaped values redacted.
fn helper_stderr_suffix(config: &ModelProviderAuthInfo, stderr: &str) -> String {
    const MAX_CHARS: usize = 500;
    if stderr.is_empty() {
        return String::new();
    }
    if !is_corbanu_internal_helper(config) {
        return " (its output is withheld; run the configured auth command yourself to see it)"
            .to_string();
    }
    // Redacted whole first: a cut could split a value from its name.
    let redacted = codex_log_guard::redact_credentials(stderr);
    let skip = redacted.chars().count().saturating_sub(MAX_CHARS);
    let tail: String = redacted.chars().skip(skip).collect();
    format!(": {tail}")
}

/// The hidden subcommand that prints a GPU rental endpoint's token.
pub const GPU_ENDPOINT_TOKEN_HELPER_SUBCOMMAND: &str = "internal-gpu-endpoint-token";

/// Whether `config` runs one of Corbanu's own hidden helper subcommands.
/// The program name varies (`corbanu`, a debug or legacy name, or
/// `/proc/self/exe` after an in-place update), so the subcommand decides.
fn is_corbanu_internal_helper(config: &ModelProviderAuthInfo) -> bool {
    config.args.first().is_some_and(|arg| {
        [
            CLAUDE_PLAN_TOKEN_HELPER_SUBCOMMAND,
            GPU_ENDPOINT_TOKEN_HELPER_SUBCOMMAND,
        ]
        .contains(&arg.as_str())
    })
}

/// Verifies that a provider's external bearer-token command can currently
/// produce a usable token without exposing that token to the caller.
pub async fn validate_provider_auth_command(config: &ModelProviderAuthInfo) -> io::Result<()> {
    validate_provider_auth_command_for_account(config, /*account*/ None).await
}

/// PF-84: [`validate_provider_auth_command`] for a named account. The caller
/// checks enrollment first (see [`ExternalBearerAccount::ensure_enrolled`]).
pub async fn validate_provider_auth_command_for_account(
    config: &ModelProviderAuthInfo,
    account: Option<&str>,
) -> io::Result<()> {
    run_provider_auth_command(config, /*force_refresh*/ false, account)
        .await
        .map(drop)
}

fn resolve_provider_auth_program(command: &str, cwd: &Path) -> io::Result<PathBuf> {
    let path = Path::new(command);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    if path.components().count() > 1 {
        return Ok(cwd.join(path));
    }

    Ok(PathBuf::from(command))
}
