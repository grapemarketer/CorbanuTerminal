//! Sampling-local lazy bootstrap shared by admitted Responses transports.
use super::FAILURE;
use super::Sampling;
use super::failure;
use crate::config::AccountingMode;
use crate::session::session::Session;
use codex_login::CodexAuth;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::WireApi;
use codex_protocol::auth::AuthMode;
use codex_protocol::error::CodexErr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use tokio::sync::OnceCell;
use uuid::Uuid;

pub(crate) type Slot = Arc<Mutex<Option<Arc<DeferredResponsesSampling>>>>;

pub(crate) struct DeferredResponsesSampling {
    request: Uuid,
    session: Arc<Session>,
    /// The thread its attempts are recorded under.
    owner: codex_protocol::ThreadId,
    /// The ledger that thread lives in; the session's own when `None`.
    owner_db: Option<codex_rollout::state_db::StateDbHandle>,
    turn: String,
    mode: AccountingMode,
    sampling: OnceCell<Arc<Sampling>>,
    failed: AtomicBool,
}

/// The sampling attached to `slot`. A poisoned slot is closed and the request
/// goes unrecorded.
pub(crate) fn read(slot: &Slot) -> Result<Option<Arc<DeferredResponsesSampling>>, CodexErr> {
    Ok(slot
        .lock()
        .map(|value| value.clone())
        .unwrap_or_else(|poison| {
            if let Some(value) = poison.into_inner().take() {
                value.reject();
            }
            super::gap("read sampling slot", "slot lock poisoned");
            None
        }))
}

pub(crate) struct Scope(Slot);
impl Scope {
    pub(crate) fn attach(
        slot: Slot,
        value: Option<Arc<DeferredResponsesSampling>>,
    ) -> Result<Self, CodexErr> {
        match slot.lock() {
            Ok(mut slot) => *slot = value,
            Err(poison) => {
                // Leave the slot empty: this turn's requests go unrecorded.
                if let Some(stale) = poison.into_inner().take() {
                    stale.reject();
                }
                if let Some(value) = value {
                    value.reject();
                }
                super::gap("attach sampling", "slot lock poisoned");
            }
        }
        Ok(Self(slot))
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        match self.0.lock() {
            Ok(mut value) => *value = None,
            Err(poison) => {
                if let Some(value) = poison.into_inner().take() {
                    value.reject();
                }
            }
        }
    }
}

struct Bootstrap<'a>(&'a DeferredResponsesSampling, bool);
impl Drop for Bootstrap<'_> {
    fn drop(&mut self) {
        if !self.1 {
            self.0.reject();
        }
    }
}

impl DeferredResponsesSampling {
    #[cfg(test)]
    pub(crate) fn new(session: Arc<Session>, turn: String, mode: AccountingMode) -> Arc<Self> {
        Arc::new(Self {
            request: Uuid::new_v4(),
            owner: session.thread_id,
            owner_db: None,
            session,
            turn,
            mode,
            sampling: OnceCell::new(),
            failed: AtomicBool::new(false),
        })
    }

    /// A sampling recorded under `owner`: the session's own thread, or the
    /// conversation an ephemeral review fork works for.
    pub(crate) fn new_for(
        session: Arc<Session>,
        owner: crate::accounting::AccountingOwner,
        turn: String,
        mode: AccountingMode,
    ) -> Arc<Self> {
        Arc::new(Self {
            request: Uuid::new_v4(),
            session,
            owner: owner.thread,
            owner_db: Some(owner.db),
            turn,
            mode,
            sampling: OnceCell::new(),
            failed: AtomicBool::new(false),
        })
    }

    #[track_caller]
    pub(crate) fn reject(&self) {
        if !self.failed.swap(true, Ordering::AcqRel) {
            tracing::warn!(
                target: "codex_core::accounting",
                at = %std::panic::Location::caller(),
                "accounting sampling closed; later requests in this turn are sent unrecorded"
            );
        }
        if let Some(sampling) = self.sampling.get() {
            sampling.reject();
        }
    }

    pub(crate) fn check(&self) -> Result<(), CodexErr> {
        if self.failed.load(Ordering::Acquire) {
            return Err(CodexErr::Fatal(FAILURE.into()));
        }
        self.sampling.get().map_or(Ok(()), |value| value.check())
    }

    /// Whether this sampling has stopped recording.
    pub(crate) fn is_closed(&self) -> bool {
        self.check().is_err()
    }

    /// Whether this sampling stopped its turn (`Sampling::halt`).
    pub(crate) fn is_halted(&self) -> bool {
        self.sampling.get().is_some_and(|value| value.is_halted())
    }

    /// Leave this request unrecorded. A sampling already in flight closes, so
    /// the rest of the turn goes unrecorded too; the request itself proceeds.
    pub(crate) fn exclude(&self) -> Result<(), CodexErr> {
        if self.sampling.initialized() {
            self.reject();
        }
        Ok(())
    }

    /// The accounting mode this collector was attached with.
    #[cfg(test)]
    pub(crate) fn mode(&self) -> &AccountingMode {
        &self.mode
    }

    pub(super) fn provider_mode(&self) -> bool {
        matches!(self.mode, AccountingMode::Provider { .. })
    }

    /// The approved websocket route, or `None` when this turn's websocket
    /// requests are not recorded - including after accounting failed.
    pub(crate) fn websocket_endpoint(&self) -> Result<Option<String>, CodexErr> {
        if self.is_closed() {
            return Ok(None);
        }
        let endpoint = self.try_websocket_endpoint();
        if let Err(error) = &endpoint {
            super::gap("websocket endpoint", error);
            return Ok(None);
        }
        endpoint
    }

    fn try_websocket_endpoint(&self) -> Result<Option<String>, CodexErr> {
        match &self.mode {
            AccountingMode::DirectOpenAiResponses {
                approved_endpoint, ..
            } => super::websocket::endpoint(approved_endpoint, /*query*/ None)
                .map(Some)
                .inspect_err(|_| self.reject()),
            AccountingMode::Provider {
                approved_endpoint,
                approved_query,
                wire_api: WireApi::Responses,
                ..
            } => super::websocket::endpoint(approved_endpoint, approved_query.as_deref())
                .map(Some)
                .inspect_err(|_| self.reject()),
            _ => Ok(None),
        }
    }

    pub(crate) async fn resolve(
        &self,
        provider: &ModelProviderInfo,
        auth: Option<&CodexAuth>,
        endpoint: &str,
    ) -> Result<Option<Arc<Sampling>>, CodexErr> {
        self.resolve_path(
            provider,
            auth.map(CodexAuth::auth_mode),
            endpoint,
            "responses",
        )
        .await
    }

    /// Resolve against a specific path under the approved endpoint.
    ///
    /// Compaction has its own path on the same approved route. Pinning it
    /// against `responses` would read as a route change and reject the turn,
    /// which is how the legacy compaction endpoint stayed uncollected.
    ///
    /// Eligibility needs only the resolved auth mode, never the credential itself.
    ///
    /// Accounting never stops a request: when it fails, this sampling closes
    /// and the request, like every later one in the turn, is sent unrecorded.
    pub(crate) async fn resolve_path(
        &self,
        provider: &ModelProviderInfo,
        auth_mode: Option<AuthMode>,
        endpoint: &str,
        path: &str,
    ) -> Result<Option<Arc<Sampling>>, CodexErr> {
        if self.is_closed() {
            return Ok(None);
        }
        match self
            .try_resolve_path(provider, auth_mode, endpoint, path)
            .await
        {
            Ok(sampling) => Ok(sampling),
            Err(error) => {
                self.reject();
                super::gap("resolve sampling", error);
                Ok(None)
            }
        }
    }

    async fn try_resolve_path(
        &self,
        provider: &ModelProviderInfo,
        auth_mode: Option<AuthMode>,
        endpoint: &str,
        path: &str,
    ) -> Result<Option<Arc<Sampling>>, CodexErr> {
        self.check()?;
        let admitted = if matches!(self.mode, AccountingMode::Provider { .. }) {
            eligible(provider, auth_mode)
        } else {
            legacy_eligible(provider, auth_mode)
        };
        if !admitted {
            if self.sampling.initialized() {
                self.reject();
                self.check()?;
            }
            return Ok(None);
        }
        let (AccountingMode::DirectOpenAiResponsesHttp {
            approved_endpoint, ..
        }
        | AccountingMode::DirectOpenAiResponses {
            approved_endpoint, ..
        }
        | AccountingMode::Provider {
            approved_endpoint,
            wire_api: WireApi::Responses,
            ..
        }) = &self.mode
        else {
            self.reject();
            return Err(CodexErr::Fatal(
                failure("resolve sampling", "turn mode does not match this wire").into(),
            ));
        };
        let pinned = super::pinned_route(
            approved_endpoint,
            super::canonical_query(provider).as_deref(),
            path,
        );
        if super::canonical_route(endpoint) != super::canonical_route(&pinned) {
            self.reject();
            return Err(CodexErr::Fatal(
                failure(
                    "resolve sampling",
                    "request route differs from the approved route",
                )
                .into(),
            ));
        }
        let mode = if let AccountingMode::Provider { provider_id, .. } = &self.mode {
            super::turn_mode(
                &self.mode,
                provider_id,
                provider,
                auth_mode,
                approved_endpoint,
            )
        } else {
            self.mode.clone()
        };
        // A rebound mode that disagrees with the sampling already in flight would
        // record this turn's economics under the wrong authority.
        if let Some(existing) = self.sampling.get()
            && matches!(mode, AccountingMode::Provider { .. })
            && (super::pricing_for(&mode) != existing.pricing
                || super::basis_source_for(&mode) != existing.basis_source
                || !existing.same_account(&mode))
        {
            self.reject();
            return Err(CodexErr::Fatal(
                failure("resolve sampling", "turn economics changed mid-sampling").into(),
            ));
        }
        let result = self
            .sampling
            .get_or_try_init(|| async {
                let mut completion = Bootstrap(self, false);
                self.session
                    .try_ensure_rollout_materialized()
                    .await
                    .map_err(|error| {
                        CodexErr::Fatal(failure("materialize rollout", error).into())
                    })?;
                let runtime = self
                    .owner_db
                    .clone()
                    .or_else(|| self.session.state_db())
                    .ok_or_else(|| {
                        CodexErr::Fatal(failure("open sampling", "no state database").into())
                    })?;
                let sampling = Sampling::start_request(
                    runtime,
                    self.owner,
                    self.turn.clone(),
                    &mode,
                    self.request,
                    (path != "responses").then_some(path),
                )
                .await?;
                completion.1 = true;
                Ok::<_, CodexErr>(sampling)
            })
            .await;
        match result {
            Ok(sampling) => {
                self.check()?;
                Ok(Some(Arc::clone(sampling)))
            }
            Err(error) => {
                // `start_request` logged its own cause; this records the outcome.
                tracing::debug!(target: "codex_core::accounting", %error, "sampling did not open");
                self.reject();
                Err(CodexErr::Fatal(FAILURE.into()))
            }
        }
    }
}

pub(super) fn eligible(provider: &ModelProviderInfo, _auth_mode: Option<AuthMode>) -> bool {
    provider.wire_api == WireApi::Responses && super::route_refusal(provider).is_none()
}

pub(super) fn legacy_eligible(provider: &ModelProviderInfo, auth_mode: Option<AuthMode>) -> bool {
    provider.wire_api == WireApi::Responses
        && matches!(auth_mode, Some(AuthMode::ApiKey))
        && provider.auth.is_none()
        && provider.experimental_bearer_token.is_none()
        && provider.api_key_header_name().is_none()
        && !provider.http_headers.as_ref().is_some_and(|headers| {
            headers.keys().any(|name| {
                name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("api-key")
            })
        })
        && !provider.env_http_headers.as_ref().is_some_and(|headers| {
            headers.keys().any(|name| {
                name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("api-key")
            })
        })
}

pub(super) fn patch(
    usage: codex_api::ResponsesUsagePatch,
    provider_id: &str,
) -> anyhow::Result<codex_state::accounting::Patch> {
    use codex_api::ResponsesTokenPresence;
    use codex_state::accounting::Presence;
    let presence = |value| {
        Ok::<_, anyhow::Error>(match value {
            ResponsesTokenPresence::Missing => Presence::Missing,
            ResponsesTokenPresence::Null => Presence::Null,
            ResponsesTokenPresence::Number(value) => Presence::Number(value.try_into()?),
        })
    };
    // #814: apply #786's per-report Chat rule here too. An impossible
    // reasoning subset must not make replay discard otherwise valid usage
    // and the provider-reported charge. Split/cumulative reports are unchanged.
    let output = presence(usage.output_tokens)?;
    let mut reasoning = presence(usage.reasoning_tokens)?;
    if let (Presence::Number(reasoning_count), Presence::Number(output_count)) = (reasoning, output)
        && i64::from(reasoning_count) > i64::from(output_count)
    {
        tracing::debug!(
            target: "codex_core::accounting",
            "reasoning count above the output count; recorded as not reported"
        );
        reasoning = Presence::Missing;
    }
    Ok(codex_state::accounting::Patch {
        input: presence(usage.input_tokens)?,
        read: presence(usage.cached_tokens)?,
        write: presence(usage.cache_write_tokens)?,
        output,
        reasoning,
        total: presence(usage.total_tokens)?,
        // The Vercel AI Gateway states what it charged with every response.
        // Other routes carry no such figure. A figure the exact decimal type
        // cannot hold is dropped rather than rounded.
        billed_usd: (provider_id == codex_model_provider_info::VERCEL_PROVIDER_ID)
            .then_some(usage.billed_usd)
            .flatten()
            .and_then(|text| codex_state::accounting::Decimal::try_from(text).ok()),
        refused_status: None,
        session_ended_at_ms: None,
    })
}

#[cfg(test)]
#[path = "accounting_responses_tests.rs"]
mod tests;
