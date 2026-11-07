use keyring::Entry;
use keyring::Error as KeyringError;
use std::error::Error;
use std::fmt;
use std::fmt::Debug;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::PoisonError;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tracing::trace;

#[cfg(target_os = "macos")]
mod macos_interaction;
#[cfg(any(target_os = "macos", test))]
mod prompt_budget;

/// Whether the user's default macOS keychain is known to be locked.
///
/// The login keychain unlocks with the GUI session, but a process started from
/// an SSH login gets its own security session, where it usually stays locked.
/// Credential reads then fail without any visible prompt. This query never
/// prompts and never reads a credential. It is `false` on other platforms and
/// whenever the state cannot be determined.
pub fn default_keychain_locked() -> bool {
    #[cfg(target_os = "macos")]
    {
        #[link(name = "Security", kind = "framework")]
        unsafe extern "C" {
            fn SecKeychainGetStatus(keychain: *const std::ffi::c_void, status: *mut u32) -> i32;
        }
        const UNLOCKED: u32 = 1; // kSecUnlockStateStatus
        let mut status = 0;
        // SAFETY: A null keychain selects the default keychain; Security.framework
        // writes one status word into this valid pointer.
        let result = unsafe { SecKeychainGetStatus(std::ptr::null(), &mut status) };
        result == 0 && status & UNLOCKED == 0
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

fn with_native_interaction<T>(
    service: &str,
    account: &str,
    callback: impl FnOnce() -> Result<T, CredentialStoreError>,
) -> Result<T, CredentialStoreError> {
    #[cfg(target_os = "macos")]
    {
        macos_interaction::run(service, account, callback)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (service, account);
        callback()
    }
}

const DEFAULT_KEYRING_OPERATION_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub enum CredentialStoreError {
    Other(KeyringError),
    Message(String),
}

impl CredentialStoreError {
    pub fn new(error: KeyringError) -> Self {
        Self::Other(error)
    }

    pub fn message(&self) -> String {
        match self {
            Self::Other(error) => error.to_string(),
            Self::Message(message) => message.clone(),
        }
    }

    pub fn into_error(self) -> KeyringError {
        match self {
            Self::Other(error) => error,
            Self::Message(message) => {
                KeyringError::PlatformFailure(Box::new(std::io::Error::other(message)))
            }
        }
    }

    pub fn from_message(message: impl Into<String>) -> Self {
        Self::Message(message.into())
    }
}

impl fmt::Display for CredentialStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Other(error) => write!(f, "{error}"),
            Self::Message(message) => write!(f, "{message}"),
        }
    }
}

impl Error for CredentialStoreError {}

/// Shared credential store abstraction for keyring-backed implementations.
pub trait KeyringStore: Debug + Send + Sync {
    fn load(&self, service: &str, account: &str) -> Result<Option<String>, CredentialStoreError>;
    fn save(&self, service: &str, account: &str, value: &str) -> Result<(), CredentialStoreError>;
    fn delete(&self, service: &str, account: &str) -> Result<bool, CredentialStoreError>;
}

#[derive(Debug, Clone, Copy)]
pub struct DefaultKeyringStore;

#[derive(Debug, Default)]
struct KeyringOperationGate {
    state: Mutex<KeyringOperationState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
struct KeyringOperationState {
    in_flight: Option<u64>,
    next_generation: u64,
    circuit_open: bool,
}

impl KeyringOperationGate {
    fn acquire(&self, operation: &str, deadline: Instant) -> Result<u64, CredentialStoreError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if state.circuit_open {
                return Err(CredentialStoreError::from_message(format!(
                    "OS keyring {operation} unavailable because a prior keyring operation timed out"
                )));
            }
            if state.in_flight.is_none() {
                state.next_generation = state.next_generation.wrapping_add(1);
                let generation = state.next_generation;
                state.in_flight = Some(generation);
                return Ok(generation);
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(CredentialStoreError::from_message(format!(
                    "OS keyring {operation} timed out waiting for another keyring operation"
                )));
            }
            let (next_state, wait_result) = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner);
            state = next_state;
            if wait_result.timed_out() && state.in_flight.is_some() {
                return Err(CredentialStoreError::from_message(format!(
                    "OS keyring {operation} timed out waiting for another keyring operation"
                )));
            }
        }
    }

    fn mark_timed_out(&self, generation: u64) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.in_flight == Some(generation) {
            state.circuit_open = true;
            self.changed.notify_all();
        }
    }

    fn finish(&self, generation: u64, completed_successfully: bool) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.in_flight == Some(generation) {
            state.in_flight = None;
            if completed_successfully {
                state.circuit_open = false;
            }
            self.changed.notify_all();
        }
    }
}

#[derive(Debug)]
struct KeyringOperationPermit {
    gate: Arc<KeyringOperationGate>,
    generation: u64,
    finished: bool,
}

impl KeyringOperationPermit {
    fn finish(mut self, completed_successfully: bool) {
        self.gate.finish(self.generation, completed_successfully);
        self.finished = true;
    }
}

impl Drop for KeyringOperationPermit {
    fn drop(&mut self) {
        if !self.finished {
            self.gate
                .finish(self.generation, /*completed_successfully*/ false);
        }
    }
}

fn default_keyring_operation_gate() -> Arc<KeyringOperationGate> {
    static GATE: OnceLock<Arc<KeyringOperationGate>> = OnceLock::new();
    Arc::clone(GATE.get_or_init(|| Arc::new(KeyringOperationGate::default())))
}

fn run_bounded_keyring_operation<T, F>(
    gate: Arc<KeyringOperationGate>,
    operation: &'static str,
    timeout: Duration,
    callback: F,
) -> Result<T, CredentialStoreError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, CredentialStoreError> + Send + 'static,
{
    let acquire_deadline = Instant::now() + timeout;
    let generation = gate.acquire(operation, acquire_deadline)?;
    let permit = KeyringOperationPermit {
        gate: Arc::clone(&gate),
        generation,
        finished: false,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    let spawn_result = thread::Builder::new()
        .name(format!("codex-keyring-{operation}"))
        .spawn(move || {
            let result = callback();
            let completed_successfully = result.is_ok();
            permit.finish(completed_successfully);
            let _ = sender.send(result);
        });
    if let Err(error) = spawn_result {
        gate.finish(generation, /*completed_successfully*/ false);
        return Err(CredentialStoreError::from_message(format!(
            "failed to start OS keyring {operation}: {error}"
        )));
    }

    match receiver.recv_timeout(timeout) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            gate.mark_timed_out(generation);
            Err(CredentialStoreError::from_message(format!(
                "OS keyring {operation} timed out after {} seconds; OS keyring access is disabled for this process",
                timeout.as_secs()
            )))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(CredentialStoreError::from_message(
            format!("OS keyring {operation} worker stopped unexpectedly"),
        )),
    }
}

impl KeyringStore for DefaultKeyringStore {
    fn load(&self, service: &str, account: &str) -> Result<Option<String>, CredentialStoreError> {
        reject_fixture_keyring_access()?;
        let service = service.to_string();
        let account = account.to_string();
        run_bounded_keyring_operation(
            default_keyring_operation_gate(),
            "load",
            DEFAULT_KEYRING_OPERATION_TIMEOUT,
            move || {
                with_native_interaction(&service, &account, || {
                    trace!("keyring.load start, service={service}, account={account}");
                    let entry =
                        Entry::new(&service, &account).map_err(CredentialStoreError::new)?;
                    match entry.get_password() {
                        Ok(password) => {
                            trace!("keyring.load success, service={service}, account={account}");
                            Ok(Some(password))
                        }
                        Err(keyring::Error::NoEntry) => {
                            trace!("keyring.load no entry, service={service}, account={account}");
                            Ok(None)
                        }
                        Err(error) => {
                            trace!(
                                "keyring.load error, service={service}, account={account}, error={error}"
                            );
                            Err(CredentialStoreError::new(error))
                        }
                    }
                })
            },
        )
    }

    fn save(&self, service: &str, account: &str, value: &str) -> Result<(), CredentialStoreError> {
        reject_fixture_keyring_access()?;
        let service = service.to_string();
        let account = account.to_string();
        let value = value.to_string();
        run_bounded_keyring_operation(
            default_keyring_operation_gate(),
            "save",
            DEFAULT_KEYRING_OPERATION_TIMEOUT,
            move || {
                with_native_interaction(&service, &account, || {
                    trace!(
                        "keyring.save start, service={service}, account={account}, value_len={}",
                        value.len()
                    );
                    let entry =
                        Entry::new(&service, &account).map_err(CredentialStoreError::new)?;
                    match entry.set_password(&value) {
                        Ok(()) => {
                            trace!("keyring.save success, service={service}, account={account}");
                            Ok(())
                        }
                        Err(error) => {
                            trace!(
                                "keyring.save error, service={service}, account={account}, error={error}"
                            );
                            Err(CredentialStoreError::new(error))
                        }
                    }
                })
            },
        )
    }

    fn delete(&self, service: &str, account: &str) -> Result<bool, CredentialStoreError> {
        reject_fixture_keyring_access()?;
        let service = service.to_string();
        let account = account.to_string();
        run_bounded_keyring_operation(
            default_keyring_operation_gate(),
            "delete",
            DEFAULT_KEYRING_OPERATION_TIMEOUT,
            move || {
                with_native_interaction(&service, &account, || {
                    trace!("keyring.delete start, service={service}, account={account}");
                    let entry =
                        Entry::new(&service, &account).map_err(CredentialStoreError::new)?;
                    match entry.delete_credential() {
                        Ok(()) => {
                            trace!("keyring.delete success, service={service}, account={account}");
                            Ok(true)
                        }
                        Err(keyring::Error::NoEntry) => {
                            trace!("keyring.delete no entry, service={service}, account={account}");
                            Ok(false)
                        }
                        Err(error) => {
                            trace!(
                                "keyring.delete error, service={service}, account={account}, error={error}"
                            );
                            Err(CredentialStoreError::new(error))
                        }
                    }
                })
            },
        )
    }
}

// Debug fixtures exercise the existing profile-local fallback without ever calling
// the native keyring. Release builds cannot enable this test isolation override.
fn reject_fixture_keyring_access() -> Result<(), CredentialStoreError> {
    if cfg!(debug_assertions) && std::env::var_os("CORBANU_TEST_NO_NATIVE_KEYRING").is_some() {
        return Err(CredentialStoreError::from_message(
            "Native keyring is unavailable in an isolated test fixture".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod bounded_operation_tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;

    #[test]
    fn isolated_fixture_never_accesses_native_keyring() {
        if std::env::var_os("CORBANU_TEST_NO_NATIVE_KEYRING").is_some() {
            for result in [
                DefaultKeyringStore
                    .load("codex", "fixture-canary")
                    .map(|_| ()),
                DefaultKeyringStore.save("codex", "fixture-canary", "synthetic-value"),
                DefaultKeyringStore
                    .delete("codex", "fixture-canary")
                    .map(|_| ()),
            ] {
                assert!(
                    result
                        .unwrap_err()
                        .message()
                        .contains("isolated test fixture")
                );
            }
            return;
        }
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "bounded_operation_tests::isolated_fixture_never_accesses_native_keyring",
            ])
            .env("CORBANU_TEST_NO_NATIVE_KEYRING", "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[test]
    fn bounded_operation_returns_its_result() {
        let gate = Arc::new(KeyringOperationGate::default());
        // 10s, not 1s: a no-op worker took over 1s on a loaded Windows runner.
        let result = run_bounded_keyring_operation(gate, "load", Duration::from_secs(10), || {
            Ok("stored-value".to_string())
        });

        assert_eq!(result.expect("bounded operation"), "stored-value");
    }

    #[test]
    fn timeout_opens_circuit_without_starting_another_worker() {
        let gate = Arc::new(KeyringOperationGate::default());
        let gate_for_recovery = Arc::clone(&gate);
        let (release_sender, release_receiver) = mpsc::channel();
        let error = run_bounded_keyring_operation(
            Arc::clone(&gate),
            "load",
            Duration::from_millis(20),
            move || {
                release_receiver.recv().expect("release first worker");
                Ok(())
            },
        )
        .expect_err("blocked operation must time out");
        assert!(error.to_string().contains("timed out"));

        let follow_up_ran = Arc::new(AtomicBool::new(false));
        let follow_up_ran_in_worker = Arc::clone(&follow_up_ran);
        // Timeouts and deadlines below are 10s and the fail-fast bound 2s:
        // a loaded Windows nightly runner took over 1s to run a no-op worker.
        // Each still separates "failed fast"/"recovered" from waiting it out.
        let started = Instant::now();
        let error =
            run_bounded_keyring_operation(gate, "save", Duration::from_secs(10), move || {
                follow_up_ran_in_worker.store(true, Ordering::SeqCst);
                Ok(())
            })
            .expect_err("open circuit must fail fast");

        assert!(
            error
                .to_string()
                .contains("prior keyring operation timed out")
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!follow_up_ran.load(Ordering::SeqCst));
        release_sender.send(()).expect("release first worker");

        let recovery_deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let recovered = {
                let state = gate_for_recovery
                    .state
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                state.in_flight.is_none() && !state.circuit_open
            };
            if recovered {
                break;
            }
            assert!(
                Instant::now() < recovery_deadline,
                "late success should close the process circuit"
            );
            thread::sleep(Duration::from_millis(5));
        }
        run_bounded_keyring_operation(
            gate_for_recovery,
            "load",
            Duration::from_secs(10),
            || Ok(()),
        )
        .expect("keyring should be retried after late success");
    }

    #[test]
    fn serialized_operation_receives_its_own_execution_timeout() {
        let gate = Arc::new(KeyringOperationGate::default());
        let first_gate = Arc::clone(&gate);
        // The queued save waits ~3s (inside its 5s acquire budget) and then
        // runs ~3s (inside a fresh 5s execution budget); charging the wait to
        // execution would time it out. Each side keeps ~2s of slack: 400ms was
        // not enough for a loaded Windows nightly runner.
        let first = thread::spawn(move || {
            run_bounded_keyring_operation(first_gate, "load", Duration::from_secs(10), || {
                thread::sleep(Duration::from_secs(3));
                Ok(())
            })
        });
        let wait_deadline = Instant::now() + Duration::from_secs(1);
        while gate
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .in_flight
            .is_none()
        {
            assert!(
                Instant::now() < wait_deadline,
                "first operation did not start"
            );
            thread::sleep(Duration::from_millis(5));
        }

        run_bounded_keyring_operation(gate, "save", Duration::from_secs(5), || {
            thread::sleep(Duration::from_secs(3));
            Ok(())
        })
        .expect("queue wait must not consume the operation execution timeout");
        first
            .join()
            .expect("first worker thread")
            .expect("first result");
    }
}

pub mod tests {
    use super::CredentialStoreError;
    use super::KeyringStore;
    use keyring::Error as KeyringError;
    use keyring::credential::CredentialApi as _;
    use keyring::mock::MockCredential;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::PoisonError;

    #[derive(Default, Clone, Debug)]
    pub struct MockKeyringStore {
        credentials: Arc<Mutex<HashMap<String, Arc<MockCredential>>>>,
    }

    impl MockKeyringStore {
        pub fn credential(&self, account: &str) -> Arc<MockCredential> {
            let mut guard = self
                .credentials
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            guard
                .entry(account.to_string())
                .or_insert_with(|| Arc::new(MockCredential::default()))
                .clone()
        }

        pub fn saved_value(&self, account: &str) -> Option<String> {
            let credential = {
                let guard = self
                    .credentials
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                guard.get(account).cloned()
            }?;
            credential.get_password().ok()
        }

        pub fn set_error(&self, account: &str, error: KeyringError) {
            let credential = self.credential(account);
            credential.set_error(error);
        }

        pub fn contains(&self, account: &str) -> bool {
            let guard = self
                .credentials
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            guard.contains_key(account)
        }
    }

    impl KeyringStore for MockKeyringStore {
        fn load(
            &self,
            _service: &str,
            account: &str,
        ) -> Result<Option<String>, CredentialStoreError> {
            let credential = {
                let guard = self
                    .credentials
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                guard.get(account).cloned()
            };

            let Some(credential) = credential else {
                return Ok(None);
            };

            match credential.get_password() {
                Ok(password) => Ok(Some(password)),
                Err(KeyringError::NoEntry) => Ok(None),
                Err(error) => Err(CredentialStoreError::new(error)),
            }
        }

        fn save(
            &self,
            _service: &str,
            account: &str,
            value: &str,
        ) -> Result<(), CredentialStoreError> {
            let credential = self.credential(account);
            credential
                .set_password(value)
                .map_err(CredentialStoreError::new)
        }

        fn delete(&self, _service: &str, account: &str) -> Result<bool, CredentialStoreError> {
            let credential = {
                let guard = self
                    .credentials
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                guard.get(account).cloned()
            };

            let Some(credential) = credential else {
                return Ok(false);
            };

            let removed = match credential.delete_credential() {
                Ok(()) => Ok(true),
                Err(KeyringError::NoEntry) => Ok(false),
                Err(error) => Err(CredentialStoreError::new(error)),
            }?;

            let mut guard = self
                .credentials
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            guard.remove(account);
            Ok(removed)
        }
    }
}
