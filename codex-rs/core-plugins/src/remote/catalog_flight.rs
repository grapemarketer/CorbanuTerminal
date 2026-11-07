//! Shares one remote plugin catalog download between concurrent callers.
//!
//! App-server startup refreshes the catalog while the TUI issues several `plugin/list` requests;
//! without sharing, each of them downloaded the whole (tens of MB) catalog in parallel.

use super::RemotePluginCatalogError;
use super::RemotePluginDirectoryItem;
use super::RemotePluginScope;
use super::RemotePluginServiceConfig;
use super::catalog_cache;
use codex_login::CodexAuth;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Instant;
use tokio::sync::watch;

type SharedPlugins = Arc<Vec<RemotePluginDirectoryItem>>;

/// Which earlier or concurrent downloads a caller may use instead of starting its own.
#[derive(Debug, Clone, Copy)]
pub(super) struct CatalogFetchReuse {
    /// Join an in-flight download only if it started at or after this instant (`None`: any).
    pub join_started_since: Option<Instant>,
    /// Read the disk cache instead if a download that started at or after this instant has
    /// completed and written it (`None`: never).
    pub reuse_completed_since: Option<Instant>,
}

impl CatalogFetchReuse {
    /// Accept only downloads that started at or after `requested_at`.
    pub(super) fn started_since(requested_at: Instant) -> Self {
        Self {
            join_started_since: Some(requested_at),
            reuse_completed_since: Some(requested_at),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FlightKey {
    codex_home: Option<PathBuf>,
    identity: String,
}

#[derive(Clone)]
enum FlightState {
    Running,
    Done(Option<SharedPlugins>),
}

struct InFlight {
    id: u64,
    started_at: Instant,
    state: watch::Receiver<FlightState>,
}

#[derive(Default)]
struct FlightEntry {
    in_flight: Option<InFlight>,
    last_success_started_at: Option<Instant>,
    /// Downloads that started before this instant must not be reused (cache invalidated).
    not_before: Option<Instant>,
}

#[derive(Default)]
struct Flights {
    next_id: u64,
    entries: HashMap<FlightKey, FlightEntry>,
}

fn flights() -> &'static Mutex<Flights> {
    static FLIGHTS: OnceLock<Mutex<Flights>> = OnceLock::new();
    FLIGHTS.get_or_init(Mutex::default)
}

fn lock_flights() -> std::sync::MutexGuard<'static, Flights> {
    match flights().lock() {
        Ok(guard) => guard,
        Err(err) => err.into_inner(),
    }
}

fn later(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

fn flight_key(
    codex_home: Option<&Path>,
    config: &RemotePluginServiceConfig,
    auth: &CodexAuth,
    scope: RemotePluginScope,
) -> Option<FlightKey> {
    Some(FlightKey {
        codex_home: codex_home.map(Path::to_path_buf),
        identity: catalog_cache::cache_identity(config, auth, scope)?,
    })
}

/// Marks downloads that started before now as unusable for later callers of this catalog.
pub(super) fn invalidate(
    codex_home: &Path,
    config: &RemotePluginServiceConfig,
    auth: &CodexAuth,
    scope: RemotePluginScope,
) {
    let Some(key) = flight_key(Some(codex_home), config, auth, scope) else {
        return;
    };
    let mut flights = lock_flights();
    let entry = flights.entries.entry(key).or_default();
    entry.not_before = Some(Instant::now());
    entry.last_success_started_at = None;
}

enum Plan {
    Join(watch::Receiver<FlightState>),
    ReuseDisk,
    Lead(u64, watch::Sender<FlightState>, Instant),
}

/// Fetches the directory catalog for `scope` and writes the disk cache when `codex_home` is set,
/// sharing the download with concurrent callers for the same home, account and scope.
pub(super) async fn fetch_directory_plugins_shared(
    codex_home: Option<&Path>,
    config: &RemotePluginServiceConfig,
    auth: &CodexAuth,
    scope: RemotePluginScope,
    reuse: CatalogFetchReuse,
) -> Result<SharedPlugins, RemotePluginCatalogError> {
    let Some(key) = flight_key(codex_home, config, auth, scope) else {
        return fetch_and_write(/*key*/ None, codex_home, config, auth, scope).await;
    };

    let plan = {
        let mut flights = lock_flights();
        let next_id = flights.next_id;
        let entry = flights.entries.entry(key.clone()).or_default();
        let join_bound = later(reuse.join_started_since, entry.not_before);
        let reuse_bound = reuse
            .reuse_completed_since
            .map(|since| since.max(entry.not_before.unwrap_or(since)));
        if let Some(in_flight) = entry.in_flight.as_ref()
            && join_bound.is_none_or(|bound| in_flight.started_at >= bound)
        {
            Plan::Join(in_flight.state.clone())
        } else if codex_home.is_some()
            && let (Some(bound), Some(completed)) = (reuse_bound, entry.last_success_started_at)
            && completed >= bound
        {
            Plan::ReuseDisk
        } else {
            // Any older download still running is not fresh enough for this caller; later
            // callers join this one instead. The older leader only clears its own entry.
            let started_at = Instant::now();
            let (sender, receiver) = watch::channel(FlightState::Running);
            entry.in_flight = Some(InFlight {
                id: next_id,
                started_at,
                state: receiver,
            });
            flights.next_id = next_id.wrapping_add(1);
            Plan::Lead(next_id, sender, started_at)
        }
    };

    match plan {
        Plan::Join(mut state) => {
            if let Ok(state) = state
                .wait_for(|state| matches!(state, FlightState::Done(_)))
                .await
                && let FlightState::Done(Some(plugins)) = &*state
            {
                return Ok(Arc::clone(plugins));
            }
            // The shared download failed or was cancelled; report this caller's own result.
            fetch_and_write(Some(&key), codex_home, config, auth, scope).await
        }
        Plan::ReuseDisk => {
            if let Some(codex_home) = codex_home
                && let Some(cached) =
                    catalog_cache::load_cached_directory_plugins(codex_home, config, auth, scope)
            {
                return Ok(Arc::new(cached.plugins));
            }
            fetch_and_write(Some(&key), codex_home, config, auth, scope).await
        }
        Plan::Lead(id, sender, started_at) => {
            let mut guard = LeaderGuard {
                key,
                id,
                started_at,
                sender,
                succeeded: None,
            };
            let plugins =
                fetch_and_write(Some(&guard.key), codex_home, config, auth, scope).await?;
            guard.succeeded = Some(Arc::clone(&plugins));
            Ok(plugins)
        }
    }
}

async fn fetch_and_write(
    key: Option<&FlightKey>,
    codex_home: Option<&Path>,
    config: &RemotePluginServiceConfig,
    auth: &CodexAuth,
    scope: RemotePluginScope,
) -> Result<SharedPlugins, RemotePluginCatalogError> {
    let started_at = Instant::now();
    let plugins = super::fetch_directory_plugins_for_scope(config, auth, scope).await?;
    // A download that started before the cache was invalidated must not replace a newer one.
    let invalidated = key.is_some_and(|key| {
        lock_flights()
            .entries
            .get(key)
            .and_then(|entry| entry.not_before)
            .is_some_and(|not_before| started_at < not_before)
    });
    if !invalidated && let Some(codex_home) = codex_home {
        catalog_cache::write_cached_directory_plugins(codex_home, config, auth, scope, &plugins);
    }
    Ok(Arc::new(plugins))
}

/// Publishes the leader's outcome, including failure or cancellation, to joined callers.
struct LeaderGuard {
    key: FlightKey,
    id: u64,
    started_at: Instant,
    sender: watch::Sender<FlightState>,
    succeeded: Option<SharedPlugins>,
}

impl Drop for LeaderGuard {
    fn drop(&mut self) {
        let outcome = self.succeeded.take();
        {
            let mut flights = lock_flights();
            if let Some(entry) = flights.entries.get_mut(&self.key) {
                if entry
                    .in_flight
                    .as_ref()
                    .is_some_and(|in_flight| in_flight.id == self.id)
                {
                    entry.in_flight = None;
                }
                if outcome.is_some()
                    && entry
                        .not_before
                        .is_none_or(|not_before| self.started_at >= not_before)
                {
                    entry.last_success_started_at =
                        later(entry.last_success_started_at, Some(self.started_at));
                }
            }
        }
        self.sender.send_replace(FlightState::Done(outcome));
    }
}

#[cfg(test)]
#[path = "catalog_flight_tests.rs"]
mod tests;
