//! Nonsecret established-route provenance and sampling-only pump admission.
use super::Sampling;
use super::failure;
use super::responses::DeferredResponsesSampling;
use codex_api::ApiError;
use codex_api::ResponsesUsageObserver;
use codex_api::ResponsesWebsocketAdmission;
use codex_login::CodexAuth;
use codex_model_provider_info::ModelProviderInfo;
use codex_protocol::error::CodexErr;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Captured using already-resolved setup, including prewarm, which is accounted like any other request.
/// Coupled to connection replacement; never contains credentials or fingerprints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Provenance {
    endpoint: Option<String>,
    api_key: bool,
    eligible: bool,
    auth_mode: Option<codex_protocol::auth::AuthMode>,
}

impl Provenance {
    /// An agent identity no longer disqualifies a route. It is a credential, and
    /// which economics it admits is decided once, in `turn_mode`; refusing it
    /// here left those sessions with no collection at all to apply economics to.
    pub(crate) fn capture(
        provider: &ModelProviderInfo,
        auth: Option<&CodexAuth>,
        api: &codex_api::Provider,
    ) -> Self {
        let auth_mode = auth.map(CodexAuth::auth_mode);
        Self {
            endpoint: api
                .websocket_url_for_path("responses")
                .ok()
                .map(|url| url.to_string()),
            api_key: super::responses::legacy_eligible(provider, auth_mode),
            eligible: super::responses::eligible(provider, auth_mode),
            auth_mode,
        }
    }

    pub(crate) fn validate(
        &self,
        deferred: &DeferredResponsesSampling,
        cached: Option<&Self>,
    ) -> Result<bool, CodexErr> {
        let Some(expected) = deferred.websocket_endpoint()? else {
            return Ok(false);
        };
        // Unsupported current routes remain excluded until accounting has started.
        // resolve() performs the initialized-route check, without a new auth read.
        if !(if deferred.provider_mode() {
            self.eligible
        } else {
            self.api_key
        }) {
            deferred.exclude()?;
            return Ok(false);
        }
        // Compare canonically: the client emits configured query parameters in
        // `HashMap` order while the pin sorts them, so byte equality would reject
        // any provider that configures more than one.
        if self
            .endpoint
            .as_deref()
            .map(super::canonical_route)
            .as_deref()
            != Some(super::canonical_route(&expected).as_str())
            || cached.is_some_and(|old| old != self)
        {
            // Not attributable to the approved route: send it unrecorded.
            deferred.reject();
            super::gap(
                "websocket admission",
                "connection route or provenance differs from the approved one",
            );
            return Ok(false);
        }
        Ok(true)
    }
}

pub(super) fn endpoint(base: &str, query: Option<&str>) -> Result<String, CodexErr> {
    // Configured query parameters are part of the route on this lane too. The
    // client's own `websocket_url_for_path` carries them, so a pin built without
    // them can never match and the turn would be rejected outright.
    let raw = super::pinned_route(base, query, "responses");
    let mut url = url::Url::parse(&raw).map_err(|error| {
        CodexErr::Fatal(
            failure(
                "websocket endpoint",
                format_args!("approved endpoint: {error}"),
            )
            .into(),
        )
    })?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !matches!(url.scheme(), "http" | "https")
        || url.as_str() != raw
    {
        return Err(CodexErr::Fatal(
            failure(
                "websocket endpoint",
                "approved endpoint has an unsupported shape",
            )
            .into(),
        ));
    }
    if url.query().is_some() && query.is_none() {
        return Err(CodexErr::Fatal(
            failure(
                "websocket endpoint",
                "unconfigured query on the approved endpoint",
            )
            .into(),
        ));
    }
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    url.set_scheme(scheme).map_err(|_| {
        CodexErr::Fatal(failure("websocket endpoint", "cannot switch to a websocket scheme").into())
    })?;
    Ok(url.to_string())
}

pub(crate) struct Admission {
    sampling: Arc<Sampling>,
    established: Provenance,
    expected: String,
    binding: Arc<std::sync::OnceLock<Arc<crate::memory_stage_one::StageOneMemoryBinding>>>,
}

impl Admission {
    // Callers hold the admission behind the trait object this returns.
    #[allow(clippy::new_ret_no_self)]
    pub(crate) fn new(
        sampling: Arc<Sampling>,
        established: Provenance,
        expected: String,
        binding: Arc<std::sync::OnceLock<Arc<crate::memory_stage_one::StageOneMemoryBinding>>>,
    ) -> Arc<dyn ResponsesWebsocketAdmission> {
        Arc::new(Self {
            sampling,
            established,
            expected,
            binding,
        })
    }
}

impl ResponsesWebsocketAdmission for Admission {
    fn check(&self) -> Pin<Box<dyn Future<Output = Result<(), ApiError>> + Send + '_>> {
        Box::pin(async move {
            if let Some(binding) = self.binding.get()
                && binding.check_stream().is_err()
            {
                self.sampling.halt();
                tracing::warn!(
                    target: "codex_core::accounting",
                    "stage-one memory denied a recorded websocket response mid-stream"
                );
                return Err(ApiError::Stream(super::MEMORY_DENIAL.into()));
            }
            Ok(())
        })
    }

    fn admit(
        &self,
        model: String,
        tier: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<Arc<dyn ResponsesUsageObserver>, ApiError>> + Send + '_>>
    {
        Box::pin(async move {
            let result = async {
                anyhow::ensure!(
                    self.established.eligible
                        && self
                            .established
                            .endpoint
                            .as_deref()
                            .map(super::canonical_route)
                            .as_deref()
                            == Some(super::canonical_route(&self.expected).as_str()),
                    "websocket connection is not the admitted route"
                );
                self.sampling
                    .admit_with_tier(&model, &self.sampling.endpoint, tier.as_deref())
                    .await
            }
            .await;
            match result {
                Ok(attempt) => Ok(super::transport::ResponseEvidence::admitted(
                    self.sampling.clone(),
                    attempt,
                ) as Arc<dyn ResponsesUsageObserver>),
                Err(error) => {
                    // Send it anyway, unrecorded, with an observer that
                    // records nothing.
                    self.sampling.reject();
                    super::gap("admit attempt", format_args!("{error:#}"));
                    let evidence = super::transport::ResponseEvidence::new(self.sampling.clone());
                    evidence.exclude();
                    Ok(evidence as Arc<dyn ResponsesUsageObserver>)
                }
            }
        })
    }
}

#[cfg(test)]
#[path = "accounting_websocket_tests.rs"]
mod tests;
