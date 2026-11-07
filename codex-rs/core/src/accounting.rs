//! Native sampling ownership glue. Replay, exact money and retention stay in state.

// Reject optimized release builds; the package entrypoint also rejects marked
// developer binaries, including debug profiles such as dev-small.
#[cfg(all(feature = "developer-accounting", not(debug_assertions)))]
compile_error!("developer-accounting is debug-only and must not be enabled in distribution builds");

use crate::config::AccountingMode;
use crate::config::PriceAuthority;
use codex_api::AnthropicTokenPresence;
use codex_api::AnthropicUsagePatch;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_state::StateRuntime;
use codex_state::accounting::AccountingStore;
use codex_state::accounting::Attempt;
use codex_state::accounting::Dialect;
use codex_state::accounting::LedgerWrite;
use codex_state::accounting::Observation;
use codex_state::accounting::Patch;
use codex_state::accounting::Presence;
use codex_state::accounting::WriteOutcome;
use sha2::Digest;
use sha2::Sha256;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering;
use uuid::Uuid;

#[cfg(test)]
#[path = "accounting_basis_tests.rs"]
mod basis_tests;
#[path = "accounting_chat.rs"]
pub(crate) mod chat;
#[cfg(test)]
#[path = "accounting_policy_tests.rs"]
mod policy_tests;
#[path = "accounting_prices.rs"]
mod prices;
#[path = "accounting_realtime.rs"]
pub(crate) mod realtime;
#[path = "accounting_responses.rs"]
pub(crate) mod responses;
#[path = "accounting_transport.rs"]
pub(crate) mod transport;
#[path = "accounting_websocket.rs"]
pub(crate) mod websocket;

/// Build-time developer opt-in only; normal builds do not contain this selector.
/// Existing collectors still enforce endpoint and authentication eligibility.
#[cfg(feature = "developer-accounting")]
pub(crate) fn developer_accounting_mode(
    provider_id: &str,
    provider: &codex_model_provider_info::ModelProviderInfo,
) -> AccountingMode {
    use codex_model_provider_info::WireApi;
    // Retain a byte marker in executable data even when symbols are stripped.
    // scripts/build_codex_package.py refuses any input carrying this marker.
    std::hint::black_box(b"CORBANU_DEVELOPER_ACCOUNTING_NOT_FOR_DISTRIBUTION");
    let scope = Uuid::new_v4();
    let approved_endpoint = provider.base_url.clone().unwrap_or_else(|| {
        match provider.wire_api {
            WireApi::Anthropic => codex_model_provider_info::ANTHROPIC_BASE_URL,
            WireApi::Responses | WireApi::Chat => "https://api.openai.com/v1",
        }
        .into()
    });
    if route_refusal(provider).is_some() {
        return AccountingMode::Disabled;
    }
    AccountingMode::Provider {
        scope,
        provider_id: provider_id.into(),
        wire_api: provider.wire_api,
        approved_endpoint,
        approved_query: canonical_query(provider),
        pricing: PriceAuthority::Unavailable,
        basis_source: Default::default(),
        account: None,
    }
}

/// Shared admission policy. Authentication changes economics, not token attribution.
pub(super) fn route_refusal(
    provider: &codex_model_provider_info::ModelProviderInfo,
) -> Option<&'static str> {
    // Nothing is refused on provider shape any more.
    //
    // AWS signing changes how a request is authenticated, not where it goes or
    // which dialect it speaks. Configured query parameters are part of the route
    // and are pinned with it. A configured routing preference names the provider
    // that serves and bills the request. What remains unattributable is a
    // REQUEST-level routing key the configuration did not ask for, and that is
    // caught per request rather than per provider.
    let _ = provider;
    None
}

/// Bind developer collection to this turn, including provider switches and the
/// authentication-dependent default endpoint. Never read credentials here.
/// Canonical form of a request URL for route comparison.
///
/// `ApiProvider::url_for_path` appends configured query parameters by iterating a
/// `HashMap`, so their order is not stable and the string cannot be reconstructed
/// and compared byte for byte. Compare the path exactly and the parameters as a
/// sorted set instead, which is order-independent and still rejects any parameter
/// the configuration did not ask for.
pub(crate) fn canonical_route(url: &str) -> String {
    let (path, query) = match url.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (url, None),
    };
    let mut parameters: Vec<&str> = query
        .map(|query| query.split('&').filter(|item| !item.is_empty()).collect())
        .unwrap_or_default();
    parameters.sort_unstable();
    if parameters.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", parameters.join("&"))
    }
}

/// Canonical `k=v&k=v` for a provider's configured query parameters.
/// A bounded turn label for a compaction of `sub_id`.
pub(crate) fn compaction_turn_label(sub_id: &str) -> String {
    scoped_turn_label("compact:", sub_id)
}

/// A turn label for one realtime call.
///
/// Creating the call is a model request the operator paid for. Its response
/// carries no usage and the call's sideband reports are not recorded, so the
/// call is recorded with its tokens unknown rather than left invisible.
pub(crate) fn realtime_turn_label(sub_id: &str) -> String {
    scoped_turn_label("realtime:", sub_id)
}

/// The turn-label prefix of one realtime WebSocket session (#824): a
/// `realtime:` label, so anything that reads realtime work reads it, told
/// apart from a WebRTC call so `/cost` can name it a session with a start and
/// an end. The TUI's `/cost` matches it by value;
/// `realtime_session_label_is_stable` pins it.
pub(crate) const REALTIME_SESSION_LABEL: &str = "realtime:ws:";

/// A turn label for one realtime WebSocket session (#824). The session is
/// one request, with the tokens its finished responses reported (#889 D223)
/// and no price.
pub(crate) fn realtime_session_turn_label(sub_id: &str) -> String {
    scoped_turn_label(REALTIME_SESSION_LABEL, sub_id)
}

/// A turn label for one stage-one memory extraction.
///
/// Extraction is a model call the operator paid for, on a client session of its
/// own. Each extraction is its own turn: they are independent passes over
/// different material, not retries of one another.
pub(crate) fn memory_turn_label() -> String {
    format!("memory:{}", Uuid::new_v4())
}

/// A turn label for one startup prewarm.
///
/// Prewarm is inference the operator paid for - it primes the model's cache, and
/// the provider charges for the prompt - so it is recorded as its own turn
/// rather than escaping collection or being folded into the first real turn.
/// Every prewarm gets its own identity: a session's startup submission id is a
/// constant, so deriving the label from it would merge the primings of every
/// reopen of a thread into one turn.
pub(crate) fn prewarm_turn_label() -> String {
    format!("prewarm:{}", Uuid::new_v4())
}

/// #403: the warm-up a conversation the user reopens (resume, restart) or
/// forks (`/side`, editing a message) sends, told apart from a new
/// conversation's so `/cost` can name it. Still a `prewarm:` label, so anything
/// that reads warm-ups reads it. The TUI's `/cost` matches the prefix by value;
/// `reopen_prewarm_label_is_stable` pins it.
pub(crate) fn reopen_prewarm_turn_label() -> String {
    format!("{REOPEN_PREWARM_LABEL}{}", Uuid::new_v4())
}

/// The turn-label prefix of a reopen warm-up; the TUI's `/cost` matches it.
pub(crate) const REOPEN_PREWARM_LABEL: &str = "prewarm:reopen:";

/// A bounded turn label for the completion assessment of `sub_id`.
///
/// The classifier is a second model request the operator paid for, issued on its
/// own client session so it does not queue behind the turn's transport teardown.
/// It belongs to the turn it assesses and is recorded under that turn's id.
pub(crate) fn assessment_turn_label(sub_id: &str) -> String {
    scoped_turn_label("assess:", sub_id)
}

/// A bounded `prefix` + `sub_id` turn identity.
///
/// `Attempt::validate` caps a turn identity at 128 bytes, so a long submission id
/// would make the labelled turn unrecordable while the ordinary turn recorded
/// fine. Keep the prefix and as much of the id as fits.
fn scoped_turn_label(prefix: &str, sub_id: &str) -> String {
    const LIMIT: usize = 128;
    // Nested prefixes (`side:agent:review:`) stay far below this; the digest
    // form below needs room for `-` and 16 hex digits.
    debug_assert!(
        prefix.len() <= LIMIT - 17,
        "turn label prefix too long: {prefix}"
    );
    let room = LIMIT.saturating_sub(prefix.len());
    if sub_id.len() <= room {
        return format!("{prefix}{sub_id}");
    }
    // Truncation alone would merge two submissions that share a long prefix into
    // one turn identity, so keep a digest of the whole id in the part that fits.
    let digest = format!("{:x}", Sha256::digest(sub_id.as_bytes()));
    let digest = &digest[..16];
    let mut end = room.saturating_sub(digest.len() + 1);
    while end > 0 && !sub_id.is_char_boundary(end) {
        end -= 1;
    }
    format!("{prefix}{}-{digest}", &sub_id[..end])
}

/// Guards that keep this turn's collectors attached to a client session.
///
/// Dropping this detaches them, so the session can be reused for work that is
/// not part of the turn.
pub(crate) struct TurnScopes {
    pub(crate) anthropic: Option<Arc<Sampling>>,
    pub(crate) responses: Option<Arc<responses::DeferredResponsesSampling>>,
    pub(crate) chat: Option<Arc<chat::DeferredChatSampling>>,
    /// A collector for this turn never opened.
    unrecorded: bool,
    /// The ledger and conversation this turn records under, to notice records
    /// the ledger writer refused after their request stopped waiting.
    ledger: Option<(Arc<StateRuntime>, ThreadId)>,
    /// #314 (D228): the last UTC day this unit's gap marker covers, once one
    /// is written or on its way; `-1` before.
    gap_marked_through: AtomicI64,
    /// When this unit attached (Unix ms): its unrecorded requests were sent
    /// between then and when it is marked.
    attached_at_ms: i64,
    _anthropic_scope: SamplingScope,
    _responses_scope: responses::Scope,
    _chat_scope: chat::Scope,
}

impl TurnScopes {
    /// Whether this turn has stopped recording its requests: a collector never
    /// opened, or one closed after an accounting failure. Its requests still go
    /// to the provider; they are only missing from the ledger.
    pub(crate) fn stopped(&self) -> bool {
        self.unrecorded
            || self
                .anthropic
                .as_ref()
                .is_some_and(|value| value.is_closed())
            || self
                .responses
                .as_ref()
                .is_some_and(|value| value.is_closed())
            || self.chat.as_ref().is_some_and(|value| value.is_closed())
    }

    /// #314 (D228): durably mark the UTC days this unit ran on (attach to
    /// now) as days on which accounting failed to record one of this
    /// conversation's requests, again only once the UTC day has moved on, so
    /// `/cost` says those days' totals may be incomplete. Nothing when the ledger was never reached; a
    /// failed mark is logged. A record the ledger writer refuses marks its own
    /// day (`accounting_writer::refuse`).
    pub(crate) async fn mark_gap(&self) {
        if let Some((db, owner, attached)) = self.claim_gap_mark() {
            write_gap_mark(&db, owner, attached).await;
        }
    }

    fn claim_gap_mark(&self) -> Option<(Arc<StateRuntime>, ThreadId, i64)> {
        let (db, owner) = self.ledger.as_ref()?;
        let today = now().div_euclid(86_400_000);
        (self.gap_marked_through.fetch_max(today, Ordering::AcqRel) < today)
            .then(|| (Arc::clone(db), *owner, self.attached_at_ms))
    }

    /// Whether a collector stopped this turn (`Sampling::halt`).
    pub(crate) fn halted(&self) -> bool {
        self.anthropic
            .as_ref()
            .is_some_and(|value| value.is_halted())
            || self
                .responses
                .as_ref()
                .is_some_and(|value| value.is_halted())
            || self.chat.as_ref().is_some_and(|value| value.is_halted())
    }
}

/// #314 (D228): a unit that ends with requests unrecorded and no turn-end
/// check (a startup warm-up, a completion assessment, a memory extraction, a
/// realtime session) still marks its days; the write is handed to the runtime
/// as the unit's scopes are dropped, so one dropped as the process exits can
/// be lost. A memory-denial stop (`halted`) is not an accounting failure.
impl Drop for TurnScopes {
    fn drop(&mut self) {
        if !self.stopped() || self.halted() {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if let Some((db, owner, attached)) = self.claim_gap_mark() {
            handle.spawn(async move { write_gap_mark(&db, owner, attached).await });
        }
    }
}

async fn write_gap_mark(db: &StateRuntime, owner: ThreadId, attached_at_ms: i64) {
    if let Err(error) = AccountingStore::record_gap(db, owner, attached_at_ms, now()).await {
        tracing::warn!(
            target: LOG_TARGET,
            cause = %format_args!("{error:#}"),
            "accounting: could not mark this day's totals as incomplete"
        );
    }
}

/// Warn once per turn when `scopes` - or a collector that never attached
/// (`None`) - left a model request unrecorded, including a record the ledger
/// writer refused after its request stopped waiting for it
/// (`SessionNotices::refused_since`).
pub(crate) async fn warn_if_unrecorded(
    session: &crate::session::session::Session,
    turn_context: &crate::session::turn_context::TurnContext,
    scopes: Option<&TurnScopes>,
) {
    let ledger = scopes.and_then(|scopes| scopes.ledger.as_ref());
    // Always read, so a refusal this turn already reports is not reported
    // again by the next one.
    let refused = session.services.accounting_notices.refused_since(ledger);
    let unrecorded = scopes.is_none_or(TurnScopes::stopped) || refused;
    // A refused record marked its own day as the writer refused it.
    if let Some(scopes) = scopes.filter(|scopes| scopes.stopped()) {
        scopes.mark_gap().await;
    }
    if unrecorded
        && !turn_context
            .accounting_gap_warning_emitted
            .swap(true, Ordering::Relaxed)
    {
        session
            .send_event(
                turn_context,
                codex_protocol::protocol::EventMsg::Warning(
                    codex_protocol::protocol::WarningEvent {
                        message: GAP_WARNING.to_string(),
                    },
                ),
            )
            .await;
    }
}

/// Bind collection for one turn on one client session.
///
/// Both the ordinary turn path and compaction use this: a compaction request is
/// inference the operator paid for, so it must be recorded like any other turn
/// rather than silently escaping collection.
pub(crate) async fn attach_turn(
    session: &Arc<crate::session::session::Session>,
    turn_context: &crate::session::turn_context::TurnContext,
    client_session: &crate::client::ModelClientSession,
    turn: String,
) -> Result<TurnScopes, CodexErr> {
    let (auth_mode, endpoint) = if matches!(
        turn_context.config.accounting,
        AccountingMode::Provider { .. }
    ) {
        let auth = turn_context.provider.auth().await;
        let api = turn_context.provider.api_provider().await?;
        (
            auth.as_ref().map(codex_login::CodexAuth::auth_mode),
            api.base_url,
        )
    } else {
        (None, String::new())
    };
    let scopes = attach_scopes(
        session,
        &turn_context.config.accounting,
        &turn_context.config.model_provider_id,
        turn_context.provider.info(),
        auth_mode,
        &endpoint,
        client_session,
        turn,
    )
    .await?;
    let notices = &session.services.accounting_notices;
    if notices.format.load(Ordering::Relaxed) == FORMAT_NEWER
        && !notices.format_warned.swap(true, Ordering::Relaxed)
    {
        session
            .send_event(
                turn_context,
                codex_protocol::protocol::EventMsg::Warning(
                    codex_protocol::protocol::WarningEvent {
                        message: NEWER_LEDGER_WARNING.to_string(),
                    },
                ),
            )
            .await;
    }
    Ok(scopes)
}

/// Shown once per session when the home's ledger is in a newer format.
pub(crate) const NEWER_LEDGER_WARNING: &str = "Developer accounting is off for this session: this home's cost ledger was written by a newer Corbanu build. \
     Requests are sent as usual; /cost does not include them.";

/// Where a session's paid requests are recorded: the thread they are
/// attributed to and the ledger that thread lives in.
#[derive(Clone)]
pub(crate) struct AccountingOwner {
    pub(crate) thread: ThreadId,
    pub(crate) db: codex_rollout::state_db::StateDbHandle,
    /// Prefixed to the turn label of work recorded for another conversation
    /// (`review:`, `consolidation:`, `agent:`, `code-review:`, `side:`,
    /// `fork:`, nested as in `side:review:`); empty for a session's own.
    pub(crate) label_prefix: String,
}

/// The conversation an internal worker with no thread of its own (memory
/// consolidation) was started by; its paid requests are recorded there
/// (PF-60-S04). Attached to the worker's thread extension data when it is
/// built, through [`crate::CodexThread::record_worker_requests_here`].
pub(crate) struct StartedBy(pub(crate) ThreadId);

/// What a session has already been told about its ledger; each notice is
/// given once per session. Held on the session, so it ends with it.
#[derive(Debug, Default)]
pub(crate) struct SessionNotices {
    /// `FORMAT_NEWER` once the ledger was found in a newer format.
    pub(crate) format: std::sync::atomic::AtomicU8,
    pub(crate) format_warned: std::sync::atomic::AtomicBool,
    pub(crate) excluded: std::sync::atomic::AtomicBool,
    /// The ledger writer's refusal count for this session's records when it
    /// last reported them (`refused_since`); set when its first turn attaches.
    pub(crate) refusals_seen: std::sync::atomic::AtomicU64,
    pub(crate) refusals_seen_set: std::sync::atomic::AtomicBool,
}

const FORMAT_NEWER: u8 = 1;

/// Whether the session's ledger is in a format newer than this build's. Once
/// it is, collection stays off for the session. Anything else is probed again
/// on the next turn - one read of the applied migrations - because another
/// build sharing this home may upgrade the ledger at any time.
async fn ledger_refused(
    session: &crate::session::session::Session,
    db: &codex_rollout::state_db::StateDbHandle,
) -> bool {
    let notices = &session.services.accounting_notices;
    if notices.format.load(Ordering::Relaxed) == FORMAT_NEWER {
        return true;
    }
    match AccountingStore::check_format(db).await {
        Ok(()) => false,
        Err(error) if codex_state::accounting::is_newer_format(&error) => {
            tracing::warn!(
                target: LOG_TARGET,
                "accounting: ledger written in a newer format; collection off for this session"
            );
            notices.format.store(FORMAT_NEWER, Ordering::Relaxed);
            true
        }
        Err(_) => false,
    }
}

impl SessionNotices {
    /// Start counting the ledger writer's refusals of this session's records
    /// (`refused_since`), once: refusals before its first turn are not its own.
    fn count_refusals_from(&self, db: &StateRuntime, owner: ThreadId) {
        if !self.refusals_seen_set.swap(true, Ordering::Relaxed) {
            self.refusals_seen
                .store(db.accounting_refusals(owner), Ordering::Relaxed);
        }
    }

    /// Whether the ledger writer refused any of this session's records since
    /// the session last looked. A record can be refused after its request
    /// stopped waiting for it; the turn that notices reports the gap.
    pub(crate) fn refused_since(&self, ledger: Option<&(Arc<StateRuntime>, ThreadId)>) -> bool {
        let Some((db, owner)) = ledger else {
            return false;
        };
        let refused = db.accounting_refusals(*owner);
        refused > self.refusals_seen.swap(refused, Ordering::Relaxed)
    }
}

/// Name, once per session, a session whose paid requests are not collected.
fn excluded_once(session: &crate::session::session::Session, reason: &'static str) {
    if !session
        .services
        .accounting_notices
        .excluded
        .swap(true, Ordering::Relaxed)
    {
        // The module path is `LOG_TARGET`; a dotted field name cannot follow
        // an explicit target in this macro.
        tracing::warn!(
            accounting.excluded = reason,
            "accounting: this session's model requests are not recorded"
        );
    }
}

/// Why a session's requests are not recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Uncollected {
    /// Not collected by design (an ephemeral session, a reviewer of a
    /// conversation that is not persisted): named once in the log.
    Excluded(&'static str),
    /// The ledger could not be reached: each such turn warns that requests
    /// went unrecorded.
    Failed(&'static str),
    /// The ledger is in a newer format: collection is off for the session.
    NewerFormat,
}

/// The owner a collecting session records under. A session excluded by design
/// is named once in the log; a ledger that cannot be reached is logged as a
/// gap.
pub(crate) async fn collecting_owner(
    session: &crate::session::session::Session,
) -> Result<AccountingOwner, Uncollected> {
    let owner = session
        .accounting_owner()
        .await
        .inspect_err(|why| match *why {
            Uncollected::Excluded(reason) => excluded_once(session, reason),
            Uncollected::Failed(cause) => gap("open sampling", cause),
            Uncollected::NewerFormat => {}
        })?;
    if ledger_refused(session, &owner.db).await {
        return Err(Uncollected::NewerFormat);
    }
    Ok(owner)
}

/// Bind collection for one unit of inference on one client session.
///
/// Every path that opens a client session of its own goes through here: the
/// ordinary turn, compaction, startup prewarm, the completion classifier and
/// stage-one memory extraction. Each of those is inference the operator paid
/// for, so each is recorded rather than escaping collection; what differs is
/// only the turn identity it records under.
#[expect(clippy::too_many_arguments)]
pub(crate) async fn attach_scopes(
    session: &Arc<crate::session::session::Session>,
    accounting: &AccountingMode,
    provider_id: &str,
    provider: &codex_model_provider_info::ModelProviderInfo,
    auth_mode: Option<codex_protocol::auth::AuthMode>,
    resolved_endpoint: &str,
    client_session: &crate::client::ModelClientSession,
    turn: String,
) -> Result<TurnScopes, CodexErr> {
    // A persisted session records under its own thread. A session with no
    // thread of its own (a guardian reviewer, a memory consolidation agent, a
    // sub-agent, a `/side` conversation or another temporary fork) records
    // under the saved conversation it works for, labelled as such
    // (`Session::accounting_owner`). Any other session (`exec --ephemeral` and
    // work for it) is left uncollected - named once in the log - and its turns
    // run.
    // A ledger that cannot be reached leaves this turn's requests unrecorded;
    // it never stops them.
    let mut candidate = match accounting {
        AccountingMode::Provider { .. } => turn_mode(
            accounting,
            provider_id,
            provider,
            auth_mode,
            resolved_endpoint,
        ),
        _ => accounting.clone(),
    };
    // The owner is only looked up when this route would be collected, so a
    // session using a route that collects nothing is neither named nor warned.
    let collected = [
        codex_model_provider_info::WireApi::Anthropic,
        codex_model_provider_info::WireApi::Responses,
        codex_model_provider_info::WireApi::Chat,
    ]
    .into_iter()
    .any(|wire| collects(&candidate, provider_id, provider, wire));
    let mut unrecorded = false;
    let owner = if !collected {
        None
    } else {
        match collecting_owner(session).await {
            Ok(owner) => Some(owner),
            Err(Uncollected::Failed(_)) => {
                unrecorded = true;
                None
            }
            Err(Uncollected::Excluded(_) | Uncollected::NewerFormat) => None,
        }
    };
    let ledger = owner
        .as_ref()
        .map(|owner| (Arc::clone(&owner.db), owner.thread));
    let turn = match &owner {
        Some(owner) if !owner.label_prefix.is_empty() => {
            scoped_turn_label(&owner.label_prefix, &turn)
        }
        Some(_) | None => turn,
    };
    let mode = if owner.is_some() {
        bind_account_fingerprint(
            &mut candidate,
            provider,
            session.get_config().await.codex_home.to_path_buf(),
        )
        .await;
        candidate
    } else {
        AccountingMode::Disabled
    };
    let collects_wire = |wire| collects(&mode, provider_id, provider, wire);
    // A sampling that cannot open leaves this turn's Messages requests
    // unrecorded; it never stops them.
    let anthropic = if collects_wire(codex_model_provider_info::WireApi::Anthropic) {
        let opened = async {
            session
                .try_ensure_rollout_materialized()
                .await
                .map_err(|error| CodexErr::Fatal(failure("materialize rollout", error).into()))?;
            let owner = owner.as_ref().ok_or_else(|| {
                CodexErr::Fatal(failure("open sampling", "no accounting owner").into())
            })?;
            Sampling::start(owner.db.clone(), owner.thread, turn.clone(), &mode).await
        }
        .await;
        match opened {
            Ok(sampling) => Some(sampling),
            Err(error) => {
                gap("open sampling", error);
                unrecorded = true;
                None
            }
        }
    } else {
        None
    };
    let _anthropic_scope =
        SamplingScope::attach(Arc::clone(&client_session.accounting), anthropic.clone())?;
    let responses = owner
        .clone()
        .filter(|_| collects_wire(codex_model_provider_info::WireApi::Responses))
        .map(|owner| {
            responses::DeferredResponsesSampling::new_for(
                Arc::clone(session),
                owner,
                turn.clone(),
                mode.clone(),
            )
        });
    let _responses_scope = responses::Scope::attach(
        Arc::clone(&client_session.responses_accounting),
        responses.clone(),
    )?;
    let chat = owner
        .filter(|_| collects_wire(codex_model_provider_info::WireApi::Chat))
        .map(|owner| {
            chat::DeferredChatSampling::new_for(
                Arc::clone(session),
                owner,
                turn.clone(),
                mode.clone(),
            )
        });
    let _chat_scope =
        chat::Scope::attach(Arc::clone(&client_session.chat_accounting), chat.clone())?;
    if let Some((db, owner)) = ledger.as_ref() {
        session
            .services
            .accounting_notices
            .count_refusals_from(db, *owner);
    }
    Ok(TurnScopes {
        gap_marked_through: AtomicI64::new(-1),
        attached_at_ms: now(),
        ledger,
        anthropic,
        responses,
        chat,
        unrecorded,
        _anthropic_scope,
        _responses_scope,
        _chat_scope,
    })
}

pub(crate) fn canonical_query(
    provider: &codex_model_provider_info::ModelProviderInfo,
) -> Option<String> {
    let parameters = provider.query_params.as_ref()?;
    if parameters.is_empty() {
        return None;
    }
    let mut items: Vec<String> = parameters
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    items.sort();
    Some(items.join("&"))
}

/// The route the client will actually request for `path`, including configured
/// query parameters. Compared through `canonical_route`.
pub(crate) fn pinned_route(approved_endpoint: &str, query: Option<&str>, path: &str) -> String {
    // A path can carry query of its own: realtime call creation appends the
    // pairs that select its architecture, and they are part of the route the
    // request is admitted against. Both sets belong in the pin, and
    // `canonical_route` sorts them, so the order they merge in does not matter.
    let (path, path_query) = match path.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (path, None),
    };
    // An empty path is the approved endpoint itself: a realtime session whose
    // base URL already names its path (#824).
    let base = approved_endpoint.trim_end_matches('/');
    let route = if path.is_empty() {
        base.to_string()
    } else {
        format!("{base}/{path}")
    };
    let query: Vec<&str> = [query, path_query]
        .into_iter()
        .flatten()
        .filter(|query| !query.is_empty())
        .collect();
    if query.is_empty() {
        route
    } else {
        format!("{route}?{}", query.join("&"))
    }
}

pub(crate) fn turn_mode(
    mode: &AccountingMode,
    provider_id: &str,
    provider: &codex_model_provider_info::ModelProviderInfo,
    auth_mode: Option<codex_protocol::auth::AuthMode>,
    resolved_endpoint: &str,
) -> AccountingMode {
    let AccountingMode::Provider {
        scope,
        provider_id: bound_provider,
        account: bound_account,
        ..
    } = mode
    else {
        return mode.clone();
    };
    if route_refusal(provider).is_some() {
        return AccountingMode::Disabled;
    }
    // Only the existing native API-key authority can supply monetary rates.
    // Only credential-bearing headers disqualify a route from monetary rates. The
    // built-in providers always set benign originator and version headers, so
    // requiring the maps to be absent entirely would silence pricing on exactly
    // the metered API-key routes that can be priced. This keeps the predicate the
    // legacy eligibility used.
    let credential_header = |headers: &Option<std::collections::HashMap<String, String>>| {
        headers.as_ref().is_some_and(|headers| {
            headers.keys().any(|name| {
                name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("api-key")
            })
        })
    };
    // Any built-in provider, at its own default route, in its own dialect. The
    // catalogue states exact rates per provider row, so restricting money to two
    // of those providers left every other provider recording tokens with no
    // economics at all. What must hold is that the request really went to the
    // route those rates are quoted for. Both bases need this; nothing the
    // catalogue says about a route survives being pointed somewhere else.
    // Resolved under the turn's own authentication, because a provider's route
    // can differ by credential: a ChatGPT plan turn goes to the Codex route, not
    // to the API-key one, and both are that provider's own.
    let own_route =
        codex_model_provider_info::built_in_model_providers(/*openai_base_url*/ None)
            .get(provider_id)
            .is_some_and(|built_in| {
                built_in.wire_api == provider.wire_api
                    && built_in
                        .to_api_provider(auth_mode)
                        .is_ok_and(|api| api.base_url == resolved_endpoint)
            });
    // Which economics apply is the billing basis declared for the route and
    // credential actually used (PF-60-S05): never inferred from the kind of
    // login, and never from whether per-token rates happen to be available.
    // A route the table does not know is undeclared, not guessed.
    let credential = codex_model_provider_info::BillingCredential::of(provider, auth_mode);
    let declaration = codex_model_provider_info::declared_billing(
        provider_id,
        provider,
        credential,
        resolved_endpoint,
        own_route,
    );
    let basis_source = match declaration {
        codex_model_provider_info::BillingDeclaration::UserConfig(_) => {
            codex_state::accounting::BasisSource::UserConfig
        }
        codex_model_provider_info::BillingDeclaration::BuiltIn(_)
        | codex_model_provider_info::BillingDeclaration::NotDeclared => {
            codex_state::accounting::BasisSource::BuiltIn
        }
    };
    use codex_model_provider_info::BillingBasis;
    let pricing = match declaration.basis() {
        None => PriceAuthority::Undeclared,
        Some(BillingBasis::Local) => PriceAuthority::Local,
        // The catalogue's plan figures are quoted for the built-in route only.
        Some(BillingBasis::Subscription) if own_route => PriceAuthority::PlanRate,
        Some(BillingBasis::Subscription) => PriceAuthority::PlanBasis,
        Some(BillingBasis::PayPerUse) => {
            // Deliberately NOT gated on `api_key_header_name`: the built-in
            // Anthropic provider declares `x-api-key` as its own credential
            // header. What disqualifies per-token rates is a route other than
            // the one the catalogue quotes, or a credential this client cannot
            // attribute to the account the catalogue quotes.
            let attributable = provider.aws.is_none()
                && provider.experimental_bearer_token.is_none()
                && !credential_header(&provider.http_headers)
                && !credential_header(&provider.env_http_headers);
            if own_route && attributable {
                PriceAuthority::ApiKeyRates
            } else if let Some(route) = crate::config::PricedRoute::at(resolved_endpoint)
                && attributable
                && provider.wire_api == codex_model_provider_info::WireApi::Chat
                && credential == codex_model_provider_info::BillingCredential::ApiKey
            {
                // #368: a provider the user configured at a route whose
                // operator publishes per-token prices for it. The provider id
                // is the user's own name, so the route is the key.
                PriceAuthority::RouteRates(route)
            } else {
                PriceAuthority::Unavailable
            }
        }
    };
    AccountingMode::Provider {
        scope: *scope,
        provider_id: provider_id.into(),
        wire_api: provider.wire_api,
        approved_endpoint: resolved_endpoint.into(),
        approved_query: canonical_query(provider),
        pricing,
        basis_source,
        account: attempt_account(
            bound_provider,
            provider_id,
            provider,
            bound_account.as_ref(),
        ),
    }
}

/// PF-84: the serving provider's selected named account as the ledger records
/// it, keeping a fingerprint already bound for that same account. `None` for
/// the `default` account.
fn attempt_account(
    bound_provider: &str,
    provider_id: &str,
    provider: &codex_model_provider_info::ModelProviderInfo,
    bound: Option<&codex_state::accounting::AttemptAccount>,
) -> Option<codex_state::accounting::AttemptAccount> {
    let selected = provider.account.as_ref()?;
    Some(codex_state::accounting::AttemptAccount {
        name: selected.name.clone(),
        // Kept only for the same account of the same provider.
        fingerprint: bound
            .filter(|bound| bound.name == selected.name && bound_provider == provider_id)
            .and_then(|bound| bound.fingerprint.clone()),
    })
}

/// Binds the account's per-home fingerprint once per turn. The vault decrypts
/// the account's material to hash it, and only the salted hash is kept. A
/// fingerprint that cannot be read within `FINGERPRINT_BUDGET` (a busy vault
/// lock, no vault) is left out and the account recorded by name alone: the
/// request is never held for it.
pub(crate) async fn bind_account_fingerprint(
    mode: &mut AccountingMode,
    provider: &codex_model_provider_info::ModelProviderInfo,
    codex_home: std::path::PathBuf,
) {
    let AccountingMode::Provider {
        account: Some(account),
        ..
    } = mode
    else {
        return;
    };
    if account.fingerprint.is_some() {
        return;
    }
    let Some(selected) = provider.account.clone() else {
        return;
    };
    const FINGERPRINT_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);
    let lookup = tokio::task::spawn_blocking(move || {
        let name = codex_vault::ProviderAccountName::parse(&selected.name).ok()?;
        let vault = codex_vault::Vault::new(codex_home);
        // Never initialise a vault (or its key) just to fingerprint.
        if vault.key_storage() == codex_vault::VaultKeyStorage::NotInitialized {
            return None;
        }
        vault
            .provider_account_fingerprint(&selected.provider_id, &name)
            .ok()
            .flatten()
    });
    let fingerprint = tokio::time::timeout(FINGERPRINT_BUDGET, lookup)
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    if fingerprint.is_none() {
        tracing::warn!(
            target: LOG_TARGET,
            "account fingerprint unavailable; recording the account by name only"
        );
    }
    account.fingerprint = fingerprint;
}

pub(crate) fn collects(
    mode: &AccountingMode,
    provider_id: &str,
    provider: &codex_model_provider_info::ModelProviderInfo,
    wire: codex_model_provider_info::WireApi,
) -> bool {
    use codex_model_provider_info::WireApi;
    if provider.wire_api != wire || route_refusal(provider).is_some() {
        return false;
    }
    match mode {
        AccountingMode::Provider {
            provider_id: bound,
            wire_api,
            ..
        } => bound == provider_id && *wire_api == wire,
        AccountingMode::DirectAnthropic { .. } => {
            provider_id == "anthropic" && wire == WireApi::Anthropic
        }
        AccountingMode::DirectOpenAiChat { .. } => provider_id == "openai" && wire == WireApi::Chat,
        AccountingMode::DirectOpenAiResponses { .. }
        | AccountingMode::DirectOpenAiResponsesHttp { .. } => {
            provider_id == "openai" && wire == WireApi::Responses
        }
        AccountingMode::Disabled => false,
    }
}

/// The error chain an accounting failure carries. It never reaches the user:
/// every accounting failure sends the request unrecorded (`gap`).
pub(crate) const FAILURE: &str = "developer accounting failed";

/// A stage-one memory denial observed while a recorded websocket response was
/// streaming. It is a privacy guard, not an accounting failure, and the one
/// case that stops the turn (`Sampling::halt`).
pub(crate) const MEMORY_DENIAL: &str =
    "Stage-one memory denied this request while it was streaming; it was not re-sent";

/// Where accounting diagnostics go. Exec prints errors on this target to stderr
/// and the TUI keeps them in its log database.
const LOG_TARGET: &str = "codex_core::accounting";

/// Record why accounting failed, and return `FAILURE` for the error chain.
///
/// Most callers then send the request unrecorded rather than stop it, so the
/// cause - a busy state database, a route mismatch, a ledger validation error -
/// is logged here or lost. `step` names where it happened; `cause` is the
/// underlying error chain. Neither ever carries prompt content or credentials.
pub(crate) fn failure(step: &'static str, cause: impl std::fmt::Display) -> &'static str {
    tracing::error!(target: LOG_TARGET, step, cause = %cause, "{FAILURE}");
    FAILURE
}

/// Record that a request goes to the provider unrecorded because accounting
/// failed. Accounting observes model requests; it never blocks one. The
/// provider still bills such a request, so the gap is logged here and the turn
/// warns once (`TurnScopes::stopped`).
pub(crate) fn gap(step: &'static str, cause: impl std::fmt::Display) {
    tracing::warn!(
        target: LOG_TARGET,
        step,
        cause = %cause,
        "accounting: request sent unrecorded"
    );
}

/// The warning a turn shows once when any of its requests went unrecorded.
pub(crate) const GAP_WARNING: &str = "Developer accounting could not record one or more model requests in this turn. \
     They were sent anyway; /cost does not include them. \
     The cause is in the log (target codex_core::accounting).";

/// Fixtures shared by the accounting unit tests.
#[cfg(test)]
pub(crate) mod tests_support {
    /// Take the state DB's write lock on a connection of another pool, as
    /// another Corbanu process would. `ROLLBACK` on it releases the lock.
    pub(crate) async fn hold_write_lock(
        db: &codex_state::StateRuntime,
    ) -> anyhow::Result<sqlx::pool::PoolConnection<sqlx::Sqlite>> {
        let pool = db
            .sqlite()
            .open_read_write_pool(&db.sqlite().state_db_path())
            .await?;
        let mut connection = pool.acquire().await?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *connection)
            .await?;
        Ok(connection)
    }
}

pub(crate) type Slot = Arc<Mutex<Option<Arc<Sampling>>>>;

/// The sampling this request is recorded on, if any. A closed sampling or a
/// poisoned slot records nothing, so the request goes out unrecorded on the
/// ordinary client.
pub(crate) fn read_slot(slot: &Slot) -> Result<Option<Arc<Sampling>>, CodexErr> {
    match slot.lock() {
        Ok(value) => Ok(value.clone().filter(|sampling| !sampling.is_closed())),
        Err(poison) => {
            if let Some(stale) = poison.into_inner().take() {
                stale.reject();
            }
            gap("read sampling slot", "slot lock poisoned");
            Ok(None)
        }
    }
}

pub(crate) struct SamplingScope(Slot);

impl SamplingScope {
    pub(crate) fn attach(slot: Slot, sampling: Option<Arc<Sampling>>) -> Result<Self, CodexErr> {
        {
            match slot.lock() {
                Ok(mut value) => *value = sampling,
                Err(poison) => {
                    // Leave the slot empty: this turn's requests go unrecorded.
                    if let Some(stale) = poison.into_inner().take() {
                        stale.reject();
                    }
                    if let Some(incoming) = sampling {
                        incoming.reject();
                    }
                    gap("attach sampling", "slot lock poisoned");
                }
            }
        }
        Ok(Self(slot))
    }
}

impl Drop for SamplingScope {
    fn drop(&mut self) {
        match self.0.lock() {
            Ok(mut value) => *value = None,
            Err(poison) => {
                if let Some(stale) = poison.into_inner().take() {
                    stale.reject();
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pricing {
    Anthropic,
    Responses,
    Chat,
    /// Pay per use on a priced route a user-configured provider reached (#368).
    Route(crate::config::PricedRoute),
    /// Subscription capacity: record the plan rate and the API equivalent, not spend.
    Plan,
    /// Subscription capacity with no catalogue figure.
    PlanBasis,
    /// Pay per use with no attributable rate: tokens only.
    Unavailable,
    /// The user's own machine: no charge.
    Local,
    /// No declared basis: neither spent nor subscription.
    Undeclared,
}

/// The economics a mode admits, shared by admission and by the guards that check
/// a rebound mode still agrees with the sampling already in flight.
fn pricing_for(mode: &AccountingMode) -> Pricing {
    match mode {
        AccountingMode::Provider {
            wire_api, pricing, ..
        } => match pricing {
            PriceAuthority::ApiKeyRates => match wire_api {
                codex_model_provider_info::WireApi::Anthropic => Pricing::Anthropic,
                codex_model_provider_info::WireApi::Responses => Pricing::Responses,
                codex_model_provider_info::WireApi::Chat => Pricing::Chat,
            },
            // Route rates are quoted for one dialect, Chat Completions.
            PriceAuthority::RouteRates(route) => match wire_api {
                codex_model_provider_info::WireApi::Chat => Pricing::Route(*route),
                codex_model_provider_info::WireApi::Anthropic
                | codex_model_provider_info::WireApi::Responses => Pricing::Unavailable,
            },
            PriceAuthority::PlanRate => Pricing::Plan,
            PriceAuthority::PlanBasis => Pricing::PlanBasis,
            PriceAuthority::Unavailable => Pricing::Unavailable,
            PriceAuthority::Local => Pricing::Local,
            PriceAuthority::Undeclared => Pricing::Undeclared,
        },
        AccountingMode::DirectAnthropic { .. } => Pricing::Anthropic,
        AccountingMode::DirectOpenAiChat { .. } => Pricing::Chat,
        AccountingMode::DirectOpenAiResponsesHttp { .. }
        | AccountingMode::DirectOpenAiResponses { .. } => Pricing::Responses,
        AccountingMode::Disabled => Pricing::Unavailable,
    }
}

/// Where a mode's basis was declared.
fn basis_source_for(mode: &AccountingMode) -> codex_state::accounting::BasisSource {
    match mode {
        AccountingMode::Provider { basis_source, .. } => *basis_source,
        AccountingMode::Disabled
        | AccountingMode::DirectAnthropic { .. }
        | AccountingMode::DirectOpenAiResponsesHttp { .. }
        | AccountingMode::DirectOpenAiChat { .. }
        | AccountingMode::DirectOpenAiResponses { .. } => {
            codex_state::accounting::BasisSource::BuiltIn
        }
    }
}

pub(crate) struct Sampling {
    runtime: Arc<StateRuntime>,
    owner: ThreadId,
    turn: String,
    request: Uuid,
    scope: Uuid,
    endpoint: String,
    provider: String,
    /// PF-84: the named account the requests are sent on; `None` is `default`.
    account: Option<codex_state::accounting::AttemptAccount>,
    dialect: Dialect,
    pricing: Pricing,
    /// The priced route the requests go to, if any (#368): its usage fields
    /// have the meaning its operator documents, however the provider is named.
    pub(super) route: Option<crate::config::PricedRoute>,
    /// Pinned to OpenAI's image generation endpoint under API-key rates: priced
    /// from OpenAI's published image rates rather than the chat-model
    /// catalogue, and its body read as an Images API usage report. Any other
    /// economics (a plan, no price) keeps the ordinary reading unchanged.
    image_generation: bool,
    basis_source: codex_state::accounting::BasisSource,
    previous: Mutex<Option<Uuid>>,
    /// Held from reading `previous` until this admission replaces it, so
    /// concurrent admissions chain as retries one after another.
    admission: tokio::sync::Semaphore,
    failed: AtomicBool,
    /// The turn must stop: see `halt`.
    halted: AtomicBool,
}

// A record handed to the ledger writer is written even if the request that
// handed it over is cancelled. Never reuse that sampling with a guessed
// predecessor; fail it closed.
struct Completion<'a> {
    sampling: &'a Sampling,
    complete: bool,
}

impl Drop for Completion<'_> {
    fn drop(&mut self) {
        if !self.complete {
            tracing::warn!(
                target: LOG_TARGET,
                "accounting write did not complete; closing this sampling"
            );
            self.sampling.reject();
        }
    }
}

impl Sampling {
    pub(crate) async fn start(
        runtime: Arc<StateRuntime>,
        owner: ThreadId,
        turn: String,
        mode: &AccountingMode,
    ) -> Result<Arc<Self>, CodexErr> {
        Self::start_request(
            runtime,
            owner,
            turn,
            mode,
            Uuid::new_v4(),
            /*path_override*/ None,
        )
        .await
    }

    /// Start a sampling pinned to `path` under the mode's approved endpoint.
    ///
    /// Clients an extension owns have their own endpoints under the same route.
    pub(crate) async fn start_at_path(
        runtime: Arc<StateRuntime>,
        owner: ThreadId,
        turn: String,
        mode: &AccountingMode,
        path: &str,
    ) -> Result<Arc<Self>, CodexErr> {
        Self::start_request(runtime, owner, turn, mode, Uuid::new_v4(), Some(path)).await
    }

    /// `path_override` pins this sampling to a different path under the same
    /// approved endpoint. Compaction's own endpoint is the one case: pinning it
    /// to the dialect's default path would read as a route change and refuse
    /// the request, which is how that endpoint stayed uncollected.
    async fn start_request(
        runtime: Arc<StateRuntime>,
        owner: ThreadId,
        turn: String,
        mode: &AccountingMode,
        request: Uuid,
        path_override: Option<&str>,
    ) -> Result<Arc<Self>, CodexErr> {
        let mut approved_query = None;
        let (scope, approved_endpoint, provider, dialect, path) = match mode {
            AccountingMode::Provider {
                scope,
                provider_id,
                wire_api,
                approved_endpoint,
                approved_query: query,
                ..
            } => {
                use codex_model_provider_info::WireApi;
                let (dialect, path) = match wire_api {
                    WireApi::Anthropic => (Dialect::NativeAnthropic, "messages"),
                    WireApi::Responses => (Dialect::Inclusive, "responses"),
                    WireApi::Chat => (Dialect::Inclusive, "chat/completions"),
                };
                approved_query = query.clone();
                (
                    scope,
                    approved_endpoint,
                    provider_id.as_str(),
                    dialect,
                    path,
                )
            }
            AccountingMode::DirectAnthropic {
                scope,
                approved_endpoint,
            } => (
                scope,
                approved_endpoint,
                "anthropic",
                Dialect::NativeAnthropic,
                "messages",
            ),
            AccountingMode::DirectOpenAiResponsesHttp {
                scope,
                approved_endpoint,
            }
            | AccountingMode::DirectOpenAiResponses {
                scope,
                approved_endpoint,
            } => (
                scope,
                approved_endpoint,
                "openai",
                Dialect::Inclusive,
                "responses",
            ),
            AccountingMode::DirectOpenAiChat {
                scope,
                approved_endpoint,
            } => (
                scope,
                approved_endpoint,
                "openai",
                Dialect::Inclusive,
                "chat/completions",
            ),
            AccountingMode::Disabled => {
                return Err(CodexErr::Fatal(
                    failure("open sampling", "accounting is disabled for this turn").into(),
                ));
            }
        };
        // Never log the endpoint itself: it is configuration, and may carry
        // material that does not belong in a log.
        let endpoint = url::Url::parse(approved_endpoint).map_err(|error| {
            CodexErr::Fatal(
                failure("open sampling", format_args!("approved endpoint: {error}")).into(),
            )
        })?;
        if !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
            || !matches!(endpoint.scheme(), "http" | "https")
        {
            return Err(CodexErr::Fatal(
                failure(
                    "open sampling",
                    "approved endpoint has an unsupported shape",
                )
                .into(),
            ));
        }
        // The ledger writer installs or checks the ledger now, unless the state
        // DB is busy: then the first record does, and a request never waits.
        runtime.open_accounting().await;
        Ok(Arc::new(Self {
            runtime,
            owner,
            turn,
            request,
            scope: *scope,
            endpoint: pinned_route(
                approved_endpoint,
                approved_query.as_deref(),
                path_override.unwrap_or(path),
            ),
            provider: provider.into(),
            account: match mode {
                AccountingMode::Provider { account, .. } => account.clone(),
                _ => None,
            },
            dialect,
            pricing: pricing_for(mode),
            route: crate::config::PricedRoute::at(approved_endpoint),
            image_generation: path_override == Some(prices::IMAGE_GENERATIONS_PATH)
                && pricing_for(mode) == Pricing::Responses
                && provider == codex_model_provider_info::OPENAI_PROVIDER_ID,
            basis_source: basis_source_for(mode),
            previous: Mutex::new(None),
            admission: tokio::sync::Semaphore::const_new(1),
            failed: AtomicBool::new(false),
            halted: AtomicBool::new(false),
        }))
    }

    /// PF-84: whether `mode` sends on the account this sampling records.
    pub(crate) fn same_account(&self, mode: &AccountingMode) -> bool {
        let name = |account: Option<&codex_state::accounting::AttemptAccount>| {
            account.map(|account| account.name.clone())
        };
        let bound = match mode {
            AccountingMode::Provider { account, .. } => account.as_ref(),
            _ => None,
        };
        name(bound) == name(self.account.as_ref())
    }

    pub(crate) fn check(&self) -> Result<(), CodexErr> {
        if self.is_closed() {
            Err(CodexErr::Fatal(FAILURE.into()))
        } else {
            Ok(())
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    /// Close this sampling and stop its turn with `MEMORY_DENIAL`: a stage-one
    /// memory denial seen mid-stream. Every accounting failure only closes the
    /// sampling.
    #[track_caller]
    pub(crate) fn halt(&self) {
        self.halted.store(true, Ordering::Release);
        self.reject();
    }

    pub(crate) fn is_halted(&self) -> bool {
        self.halted.load(Ordering::Acquire)
    }

    /// Close this sampling: it records nothing more, and every later request on
    /// it is sent unrecorded. The first close is logged with its caller.
    #[track_caller]
    pub(crate) fn reject(&self) {
        if !self.failed.swap(true, Ordering::AcqRel) {
            tracing::warn!(
                target: LOG_TARGET,
                at = %std::panic::Location::caller(),
                "accounting sampling closed; later requests in this turn are sent unrecorded"
            );
        }
    }

    async fn admit(&self, model: &str, endpoint: &str) -> anyhow::Result<Attempt> {
        self.admit_with_tier(model, endpoint, /*tier*/ None).await
    }

    pub(crate) async fn admit_with_tier(
        &self,
        model: &str,
        endpoint: &str,
        tier: Option<&str>,
    ) -> anyhow::Result<Attempt> {
        self.admit_priced(model, endpoint, tier, /*rated*/ true)
            .await
    }

    /// Admit a request this sampling's basis covers but no rate prices
    /// (#889 D223): a realtime session, whose tokens are audio and text at
    /// realtime rates no catalogue row states. Its record keeps its basis and
    /// binds no rate, so its reported tokens read "no price", never a figure
    /// from a text model's rates.
    pub(crate) async fn admit_without_rates(
        &self,
        model: &str,
        endpoint: &str,
    ) -> anyhow::Result<Attempt> {
        self.admit_priced(model, endpoint, /*tier*/ None, /*rated*/ false)
            .await
    }

    async fn admit_priced(
        &self,
        model: &str,
        endpoint: &str,
        tier: Option<&str>,
        rated: bool,
    ) -> anyhow::Result<Attempt> {
        self.check()?;
        anyhow::ensure!(
            canonical_route(endpoint) == canonical_route(&self.endpoint),
            "accounting route mismatch"
        );
        let _admission = self.admission.acquire().await?;
        self.check()?;
        let mut completion = Completion {
            sampling: self,
            complete: false,
        };
        let previous = *self.previous.lock().map_err(|_| {
            self.reject();
            anyhow::anyhow!("previous-attempt lock poisoned")
        })?;
        // Read once: a deferred admission is written later with this same time.
        let dispatched_at = now();
        let attempt = Attempt {
            attempt_id: Uuid::new_v4(),
            request_id: self.request,
            thread_id: self.owner,
            turn: self.turn.clone(),
            retry_of: previous,
            provider: self.provider.clone(),
            model: model.into(),
            scope: self.scope,
            dialect: self.dialect,
            dispatched_at_ms: dispatched_at.try_into()?,
            account: self.account.clone(),
        };
        use codex_state::accounting::Basis;
        let basis_only = |basis| {
            prices::basis_only(
                basis,
                self.basis_source,
                model,
                &self.provider,
                self.scope,
                dispatched_at,
            )
        };
        let mut prices = match self.pricing {
            // Unrated (`admit_without_rates`): subscription work keeps its
            // basis with no plan rate; pay per use falls to the basis-only
            // record below. Local, undeclared and unpriced work bind no rate
            // either way.
            Pricing::Plan if !rated => basis_only(Basis::PlanEquivalent)?,
            Pricing::Anthropic | Pricing::Responses | Pricing::Chat | Pricing::Route(_)
                if !rated =>
            {
                Vec::new()
            }
            Pricing::Unavailable => Vec::new(),
            // Subscription work stays subscription work with no catalogue
            // figure: a plan record with no rates, never a missing record that
            // would read as pay per use.
            Pricing::Plan => {
                let plan =
                    prices::plan_original(model, &self.provider, self.scope, dispatched_at, tier)?;
                if plan.is_empty() {
                    basis_only(Basis::PlanEquivalent)?
                } else {
                    plan
                }
            }
            Pricing::PlanBasis => basis_only(Basis::PlanEquivalent)?,
            Pricing::Local => basis_only(Basis::Local)?,
            Pricing::Undeclared => basis_only(Basis::Undeclared)?,
            Pricing::Anthropic => {
                prices::anthropic_original(model, &self.provider, self.scope, dispatched_at)?
            }
            Pricing::Responses if self.image_generation => prices::image_generation_original(
                model,
                &self.provider,
                self.scope,
                dispatched_at,
                tier,
            )?,
            Pricing::Responses => {
                prices::responses_original(model, &self.provider, self.scope, dispatched_at, tier)?
            }
            Pricing::Chat => {
                prices::chat_original(model, &self.provider, self.scope, dispatched_at)?
            }
            Pricing::Route(route) => {
                prices::route_original(route, model, &self.provider, self.scope, dispatched_at)?
            }
        };
        // Pay per use with no attributable rate still binds its basis and
        // where it was declared: a record with no rates, which prices nothing
        // and reads as "no price", so no attempt is left without a basis.
        if prices.is_empty() {
            prices = basis_only(Basis::Billed)?;
        }
        for snapshot in &mut prices {
            snapshot.basis_source = self.basis_source;
        }
        self.record(
            "admit attempt",
            LedgerWrite::Admit {
                owner: self.owner,
                attempt: attempt.clone(),
                prices,
            },
        )
        .await?;
        *self.previous.lock().map_err(|_| {
            self.reject();
            anyhow::anyhow!("previous-attempt lock poisoned")
        })? = Some(attempt.attempt_id);
        completion.complete = true;
        Ok(attempt)
    }

    async fn observe(
        &self,
        attempt: &Attempt,
        source: Uuid,
        position: i64,
        usage: AnthropicUsagePatch,
    ) -> anyhow::Result<()> {
        self.check()?;
        let patch = Patch {
            input: presence(usage.input_tokens)?,
            read: presence(usage.cache_read_input_tokens)?,
            write: presence(usage.cache_creation_input_tokens)?,
            output: presence(usage.output_tokens)?,
            // The Corbanu API states what it charged on its Messages route.
            // No other Anthropic-dialect route carries such a figure.
            billed_usd: (self.provider
                == codex_model_provider_info::PFTERMINAL_PLAN_ANTHROPIC_PROVIDER_ID)
                .then_some(usage.corbanu_charge_usd)
                .flatten()
                .and_then(|text| codex_state::accounting::Decimal::try_from(text).ok()),
            ..Patch::default()
        };
        self.observe_patch(attempt, source, position, patch).await
    }

    async fn observe_patch(
        &self,
        attempt: &Attempt,
        source: Uuid,
        position: i64,
        patch: Patch,
    ) -> anyhow::Result<()> {
        let position = position.try_into()?;
        let observation = Observation {
            source,
            revision: position,
            sequence: position,
            patch,
        };
        self.check()?;
        let mut completion = Completion {
            sampling: self,
            complete: false,
        };
        self.record(
            "observe usage",
            LedgerWrite::Observe {
                owner: self.owner,
                attempt: attempt.clone(),
                observations: vec![observation],
            },
        )
        .await?;
        completion.complete = true;
        Ok(())
    }
}

impl Sampling {
    /// Hand `write` to this process's ledger writer. It waits only while the
    /// write meets no other connection's lock; a record put off for a busy
    /// state DB is written later and the request goes ahead. An error is a
    /// record the ledger refused.
    async fn record(&self, step: &'static str, write: LedgerWrite) -> anyhow::Result<()> {
        match self
            .runtime
            .record_accounting(write)
            .await
            .map_err(|error| error.context(step))?
        {
            WriteOutcome::Written => {}
            WriteOutcome::Deferred => tracing::debug!(
                target: LOG_TARGET,
                step,
                "accounting: state DB busy; record queued and the request goes ahead"
            ),
        }
        Ok(())
    }
}

fn presence(value: AnthropicTokenPresence) -> anyhow::Result<Presence> {
    Ok(match value {
        AnthropicTokenPresence::Missing => Presence::Missing,
        AnthropicTokenPresence::Null => Presence::Null,
        AnthropicTokenPresence::Number(value) => Presence::Number(value.try_into()?),
    })
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
