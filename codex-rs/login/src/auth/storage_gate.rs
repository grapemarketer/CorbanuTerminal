//! PF-28-S01: while the secret output gate is armed, sign-in tokens enter it
//! whenever a sign-in store loads or saves them, so a login, a token refresh
//! or a keyring-held credential is protected from the moment this process
//! holds it. Registrations are process-wide (one per field, however many
//! stores are created). A changed value is admitted first; the value before
//! it stays protected (it may still be valid) and the one before that is
//! retired, which bounds the registry across refreshes.

use super::AgentIdentityStorage;
use super::AuthDotJson;
use super::AuthStorageBackend;
use codex_secret_broker::output_gate;
use codex_secret_broker::output_gate::SecretClass;
use codex_secret_broker::output_gate::SecretHandle;
use sha2::Digest;
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use zeroize::Zeroizing;

/// Values shorter than this are not credentials (and would redact ordinary
/// text).
const MIN_TOKEN_BYTES: usize = 8;

/// Per field: the current value's handle and digest (never the value) and
/// the previous value's handle.
#[derive(Clone, Copy)]
struct Registered {
    current: SecretHandle,
    digest: [u8; 32],
    previous: Option<SecretHandle>,
}

static REGISTERED: LazyLock<Mutex<HashMap<&'static str, Registered>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Debug)]
pub(super) struct GatedAuthStorage {
    inner: Arc<dyn AuthStorageBackend>,
}

impl GatedAuthStorage {
    pub(super) fn wrap(inner: Arc<dyn AuthStorageBackend>) -> Arc<dyn AuthStorageBackend> {
        Arc::new(Self { inner })
    }

    /// Registers every token in `auth` while the gate is armed. A credential
    /// the gate cannot protect is not used.
    fn protect(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        let Some(gate) = output_gate::active() else {
            return Ok(());
        };
        let mut registered = match REGISTERED.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        for (field, value) in auth_values(auth) {
            let value = value.trim();
            if value.len() < MIN_TOKEN_BYTES {
                continue;
            }
            let digest: [u8; 32] = Sha256::digest(value.as_bytes()).into();
            let known = registered.get(field).copied();
            if known.is_some_and(|known| known.digest == digest) {
                continue;
            }
            let label = format!("auth.json:{field}");
            let handle = gate
                .register(&label, SecretClass::Operational, value)
                .map_err(|err| {
                    std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!("secret_output_gate cannot protect the sign-in {field}: {err}"),
                    )
                })?;
            if let Some(retired) = known.and_then(|known| known.previous) {
                gate.retire(retired);
            }
            registered.insert(
                field,
                Registered {
                    current: handle,
                    digest,
                    previous: known.map(|known| known.current),
                },
            );
        }
        Ok(())
    }
}

impl AuthStorageBackend for GatedAuthStorage {
    fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
        let auth = self.inner.load()?;
        if let Some(auth) = &auth {
            self.protect(auth)?;
        }
        Ok(auth)
    }

    fn save(&self, auth: &AuthDotJson) -> std::io::Result<()> {
        self.protect(auth)?;
        self.inner.save(auth)
    }

    fn delete(&self) -> std::io::Result<bool> {
        self.inner.delete()
    }

    fn update(
        &self,
        change: &mut dyn FnMut(&mut AuthDotJson) -> std::io::Result<()>,
    ) -> std::io::Result<Option<AuthDotJson>> {
        self.inner.update(&mut |auth| {
            self.protect(auth)?;
            change(auth)?;
            self.protect(auth)
        })
    }

    fn is_named_account(&self) -> bool {
        self.inner.is_named_account()
    }
}

fn auth_values(auth: &AuthDotJson) -> Vec<(&'static str, Zeroizing<String>)> {
    let mut values = Vec::new();
    let mut push = |field: &'static str, value: Option<&String>| {
        if let Some(value) = value {
            values.push((field, Zeroizing::new(value.clone())));
        }
    };
    push("OPENAI_API_KEY", auth.openai_api_key.as_ref());
    push("personal_access_token", auth.personal_access_token.as_ref());
    push(
        "bedrock_api_key",
        auth.bedrock_api_key.as_ref().map(|key| &key.api_key),
    );
    if let Some(tokens) = &auth.tokens {
        push("id_token", Some(&tokens.id_token.raw_jwt));
        push("access_token", Some(&tokens.access_token));
        push("refresh_token", Some(&tokens.refresh_token));
    }
    match &auth.agent_identity {
        Some(AgentIdentityStorage::Jwt(jwt)) => push("agent_identity", Some(jwt)),
        Some(AgentIdentityStorage::Record(record)) => {
            push("agent_private_key", Some(&record.agent_private_key));
        }
        None => {}
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_secret_broker::output_gate::OutputSink;

    #[derive(Debug, Default)]
    struct Memory(Mutex<Option<AuthDotJson>>);

    impl AuthStorageBackend for Memory {
        fn load(&self) -> std::io::Result<Option<AuthDotJson>> {
            Ok(self.0.lock().expect("lock").clone())
        }
        fn save(&self, auth: &AuthDotJson) -> std::io::Result<()> {
            *self.0.lock().expect("lock") = Some(auth.clone());
            Ok(())
        }
        fn delete(&self) -> std::io::Result<bool> {
            Ok(self.0.lock().expect("lock").take().is_some())
        }
    }

    fn api_key_auth(key: &str) -> AuthDotJson {
        AuthDotJson {
            auth_mode: None,
            openai_api_key: Some(key.to_string()),
            tokens: None,
            last_refresh: None,
            agent_identity: None,
            personal_access_token: None,
            bedrock_api_key: None,
        }
    }

    /// Set in the child process that runs
    /// [`pf_28_s01_login_and_refresh_after_arm_are_protected`]'s body.
    const PF28_CHILD_ENV: &str = "CODEX_LOGIN_PF28_GATE_CHILD";

    #[test]
    fn pf_28_s01_login_and_refresh_after_arm_are_protected() {
        // The gate and its registrations are process-wide. Under nextest each
        // test has its own process, but `cargo test` and Bazel run the whole
        // binary in one, where other tests' sign-ins retire these values. So
        // the body runs in a child process of its own.
        if std::env::var_os(PF28_CHILD_ENV).is_none() {
            let test_name = format!(
                "{}::pf_28_s01_login_and_refresh_after_arm_are_protected",
                module_path!()
                    .split_once("::")
                    .map_or(module_path!(), |(_, path)| path)
            );
            // #1201: no provider-auth helper runs while this copy starts.
            let _no_helpers = crate::TEST_CHILD_PROCESSES
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args(["--exact", &test_name, "--test-threads=1"])
                .env(PF28_CHILD_ENV, "1")
                .output()
                .expect("run the test in a child process");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "{stdout}{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        // Armed before any sign-in, as Core does.
        output_gate::global().arm().expect("arm");
        let scrub = |text: &str| output_gate::scrub_if_armed(OutputSink::ToolResult, text);
        // Production creates a store per load or save; registrations are
        // shared. Synthetic values only.
        let save = |key: &str| {
            GatedAuthStorage::wrap(Arc::new(Memory::default()))
                .save(&api_key_auth(key))
                .expect("save");
        };
        save("sk-pf28-login-after-arm-1111");
        assert_eq!(
            scrub("k=sk-pf28-login-after-arm-1111").as_deref(),
            Some("k=[REDACTED:auth.json:OPENAI_API_KEY]")
        );
        // A refresh: the new value and the previous one are protected.
        save("sk-pf28-refreshed-key-2222");
        assert!(scrub("sk-pf28-refreshed-key-2222").is_some());
        assert!(scrub("sk-pf28-login-after-arm-1111").is_some());
        // The next refresh retires the oldest.
        save("sk-pf28-refreshed-key-3333");
        assert!(scrub("sk-pf28-refreshed-key-3333").is_some());
        assert!(scrub("sk-pf28-refreshed-key-2222").is_some());
        assert_eq!(scrub("sk-pf28-login-after-arm-1111"), None);
        // The same value again changes nothing.
        save("sk-pf28-refreshed-key-3333");
        assert!(scrub("sk-pf28-refreshed-key-2222").is_some());
    }
}
