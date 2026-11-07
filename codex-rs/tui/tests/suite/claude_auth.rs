use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use tempfile::TempDir;
use tempfile::tempdir;

use super::provider_management::select_label;
use crate::support::chat_ready;
use crate::support::secret_scan;
use crate::support::tmux::CommandSpec;
use crate::support::tmux::SessionSpec;
use crate::support::tmux::TerminalSize;
use crate::support::tmux::TmuxKey;
use crate::support::tmux::TmuxPane;
use crate::support::tmux::TmuxServer;

const READY_TIMEOUT: Duration = Duration::from_secs(30);

#[test]
fn tmux_first_run_anthropic_account_selects_claude_login_after_success() -> Result<()> {
    if !TmuxServer::should_run("first-run Anthropic account onboarding")? {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex = codex_binary(&repo_root)?;
    let codex_home = tempdir()?;
    let log_dir = codex_home.path().join("log");
    fs::create_dir_all(&log_dir)?;
    write_test_config(codex_home.path(), &repo_root)?;
    fs::remove_file(codex_home.path().join("auth.json"))?;
    let fake = FakeClaude::new()?;
    let login_fixture = CompatibilityLoginFixture::new()?;
    login_fixture.verify_hidden_health_command(&codex)?;

    let tmux = TmuxServer::start("tmux_claude_auth_first_run")?;
    tmux.register_artifact("config.toml", codex_home.path().join("config.toml"));
    tmux.register_artifact("codex-tui.log", log_dir.join("codex-tui.log"));
    let session = tmux.new_session(session_spec(
        "codex-claude-auth-first-run",
        &codex,
        &repo_root,
        codex_home.path(),
        &log_dir,
        &fake,
        Some(&login_fixture),
        /*provide_openai_fixture*/ false,
    ))?;
    let pane = session.primary_pane();
    pane.wait_stable_contains("Provider: Anthropic Claude Account", READY_TIMEOUT)?;

    select_label(pane, "Provider: Anthropic Claude Account")?;
    pane.wait_stable_contains("Long-lived subscription token (Recommended)", READY_TIMEOUT)?;
    select_label(pane, "Claude Code login")?;
    pane.wait_stable_contains("Anthropic Claude account configured", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Configured · active · ready", READY_TIMEOUT)?;
    select_label(pane, "Done")?;
    pane.wait_stable_until(
        "first-run Claude provider persistence",
        Duration::from_secs(45),
        |_| {
            fs::read_to_string(codex_home.path().join("config.toml"))
                .is_ok_and(|config| config.contains("model_provider = \"claude-plan\""))
        },
    )?;

    open_providers(pane)?;
    wait_claude_configured(pane)?;
    pane.send_key(TmuxKey::Escape)?;
    let viewport = pane.capture_viewport()?;
    let scrollback = pane.capture_scrollback_tail(/*lines*/ 2_000)?;
    exit_tui(pane)?;
    session.wait_for_exit(READY_TIMEOUT)?;

    let config = fs::read_to_string(codex_home.path().join("config.toml"))?;
    ensure!(
        config.contains("model_provider = \"claude-plan\""),
        "successful first-run Claude auth did not select claude-plan"
    );
    let selection = codex_vault::Vault::new(codex_home.path().to_path_buf())
        .load_claude_auth_selection()?
        .context("first-run Claude login selection was not persisted")?;
    ensure!(
        selection.source == codex_vault::ClaudeAuthSource::ClaudeCodeLogin,
        "first-run flow selected an unexpected Claude source"
    );
    ensure!(!viewport.is_empty() && !scrollback.is_empty());
    Ok(())
}

#[test]
fn tmux_claude_auth_managed_success_cancel_failure_recovery_and_resume() -> Result<()> {
    if !TmuxServer::should_run("Claude managed-auth state machine")? {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex = codex_binary(&repo_root)?;
    let codex_home = tempdir()?;
    let log_dir = codex_home.path().join("log");
    fs::create_dir_all(&log_dir)?;
    write_test_config(codex_home.path(), &repo_root)?;
    let fake = FakeClaude::new()?;
    let canary = synthetic_canary();

    let tmux = TmuxServer::start("tmux_claude_auth_managed")?;
    tmux.register_artifact("config.toml", codex_home.path().join("config.toml"));
    tmux.register_artifact("codex-tui.log", log_dir.join("codex-tui.log"));
    let session = tmux.new_session(session_spec(
        "codex-claude-auth-managed",
        &codex,
        &repo_root,
        codex_home.path(),
        &log_dir,
        &fake,
        /*login_fixture*/ None,
        /*provide_openai_fixture*/ true,
    ))?;
    let pane = session.primary_pane();
    chat_ready::wait_session_configured(pane, READY_TIMEOUT)?;

    open_claude_auth_choice(pane)?;
    pane.send_key(TmuxKey::Escape)?;
    pane.wait_stable_until("method picker cancelled", READY_TIMEOUT, |capture| {
        !capture.contains("Claude Plan authentication")
    })?;

    open_claude_auth_choice(pane)?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Long-lived token — masked", READY_TIMEOUT)?;
    pane.send_secret_literal(&canary)?;
    pane.wait_stable_contains("••", Duration::from_secs(10))?;
    pane.send_key(TmuxKey::Enter)?;
    wait_claude_configured(pane)?;
    assert_managed_resolver_returns(&codex, codex_home.path(), &canary)?;

    open_claude_auth_choice(pane)?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Long-lived token — masked", READY_TIMEOUT)?;
    pane.send_secret_literal("not a valid token")?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Authentication needs attention", READY_TIMEOUT)?;
    select_label(pane, "Retry authentication")?;
    pane.wait_stable_contains("Long-lived token — masked", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Escape)?;
    pane.wait_stable_until("masked replacement cancelled", READY_TIMEOUT, |capture| {
        !capture.contains("Long-lived token — masked")
    })?;
    assert_managed_resolver_returns(&codex, codex_home.path(), &canary)?;

    open_providers(pane)?;
    wait_claude_configured(pane)?;
    pane.send_key(TmuxKey::Escape)?;
    let first_viewport = pane.capture_viewport()?;
    let first_scrollback = pane.capture_scrollback_tail(/*lines*/ 2_000)?;
    assert_claude_model_catalog(pane)?;
    exit_tui(pane)?;
    session.wait_for_exit(READY_TIMEOUT)?;

    let resumed = tmux.new_session(session_spec(
        "codex-claude-auth-resumed",
        &codex,
        &repo_root,
        codex_home.path(),
        &log_dir,
        &fake,
        /*login_fixture*/ None,
        /*provide_openai_fixture*/ true,
    ))?;
    let resumed_pane = resumed.primary_pane();
    chat_ready::wait_session_configured(resumed_pane, READY_TIMEOUT)?;
    open_providers(resumed_pane)?;
    wait_claude_configured(resumed_pane)?;
    let resumed_viewport = resumed_pane.capture_viewport()?;
    let resumed_scrollback = resumed_pane.capture_scrollback_tail(/*lines*/ 2_000)?;
    resumed_pane.send_key(TmuxKey::Escape)?;
    assert_claude_model_catalog(resumed_pane)?;
    exit_tui(resumed_pane)?;
    resumed.wait_for_exit(READY_TIMEOUT)?;

    for surface in [
        first_viewport.as_str(),
        first_scrollback.as_str(),
        resumed_viewport.as_str(),
        resumed_scrollback.as_str(),
    ] {
        ensure!(
            !surface.contains(&canary),
            "secret canary appeared in a terminal capture"
        );
    }
    ensure!(
        !tree_contains(codex_home.path(), canary.as_bytes())?,
        "secret canary appeared in the isolated home or trace logs"
    );
    if tmux.artifact_dir().is_dir() {
        ensure!(
            !tree_contains(&tmux.artifact_dir(), canary.as_bytes())?,
            "secret canary appeared in tmux artifacts"
        );
    }
    Ok(())
}

// Real key-driven coverage of the provider-manager -> model-picker boundary.
// Credential configuration alone did not previously refresh picker eligibility.
fn assert_claude_model_catalog(pane: &TmuxPane) -> Result<()> {
    // The caller's Escape may still be closing the provider manager; typing into
    // it selects a provider instead of opening the model picker.
    pane.wait_stable_until("provider manager closed", READY_TIMEOUT, |text| {
        !text.contains("Configure providers and control")
            && !text.contains("Press enter to confirm or esc to go back")
    })?;
    pane.wait_stable_contains("Corbanu Terminal · TPS:", READY_TIMEOUT)?;
    pane.wait_stable_until(
        "session configured",
        READY_TIMEOUT,
        chat_ready::session_configured,
    )?;
    pane.send_literal("/model")?;
    pane.wait_stable_contains("/model", Duration::from_secs(10))?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Select Model", READY_TIMEOUT)?;
    for _ in 0..16 {
        let capture = pane.capture_viewport()?;
        if capture.contains("[Claude Plan]") {
            for model in [
                "claude-opus-5-plan",
                "claude-fable-5-1-plan",
                "claude-fable-5-plan",
            ] {
                ensure!(
                    capture.contains(model),
                    "missing subscription model: {model}"
                );
            }
            ensure!(
                !capture.contains("(current)"),
                "OpenAI session incorrectly marked Claude current"
            );
            pane.send_key(TmuxKey::Escape)?;
            return Ok(());
        }
        pane.send_key(TmuxKey::Right)?;
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!("Claude Plan tab missing after successful provider configuration")
}

#[test]
fn tmux_claude_auth_compatibility_selects_existing_login() -> Result<()> {
    if !TmuxServer::should_run("Claude compatibility-login migration")? {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex = codex_binary(&repo_root)?;
    let codex_home = tempdir()?;
    let log_dir = codex_home.path().join("log");
    fs::create_dir_all(&log_dir)?;
    write_test_config(codex_home.path(), &repo_root)?;
    let fake = FakeClaude::new()?;
    let login_fixture = CompatibilityLoginFixture::new()?;
    login_fixture.verify_hidden_health_command(&codex)?;

    let tmux = TmuxServer::start("tmux_claude_auth_compatibility")?;
    tmux.register_artifact("config.toml", codex_home.path().join("config.toml"));
    tmux.register_artifact("codex-tui.log", log_dir.join("codex-tui.log"));
    let session = tmux.new_session(session_spec(
        "codex-claude-auth-compatibility",
        &codex,
        &repo_root,
        codex_home.path(),
        &log_dir,
        &fake,
        Some(&login_fixture),
        /*provide_openai_fixture*/ true,
    ))?;
    let pane = session.primary_pane();
    chat_ready::wait_session_configured(pane, READY_TIMEOUT)?;
    open_claude_auth_choice(pane)?;
    select_label(pane, "Claude Code login")?;
    wait_claude_configured(pane)?;
    exit_tui(pane)?;
    session.wait_for_exit(READY_TIMEOUT)?;

    let selection = codex_vault::Vault::new(codex_home.path().to_path_buf())
        .load_claude_auth_selection()?
        .context("compatibility selection was not persisted")?;
    ensure!(
        selection.source == codex_vault::ClaudeAuthSource::ClaudeCodeLogin,
        "compatibility flow selected an unexpected source"
    );
    Ok(())
}

fn open_claude_auth_choice(pane: &TmuxPane<'_>) -> Result<()> {
    open_providers(pane)?;
    select_label(pane, "Claude Account")?;
    pane.wait_stable_until("Claude management actions", READY_TIMEOUT, |text| {
        text.contains("Deactivate")
            || text.contains("Reactivate")
            || text.contains("with Claude account")
    })?;
    // Enter manages eligibility. Recovery/rotation is the advertised `r`
    // shortcut, including when a saved credential is currently healthy.
    pane.send_key(TmuxKey::Escape)?;
    pane.wait_stable_contains("r recover credentials", READY_TIMEOUT)?;
    pane.send_literal("r")?;
    pane.wait_stable_contains("Recover the selected Claude credential", READY_TIMEOUT)?;
    select_label(pane, "Recover the selected Claude credential")?;
    pane.wait_stable_contains("Long-lived subscription token (Recommended)", READY_TIMEOUT)?;
    Ok(())
}

fn open_providers(pane: &TmuxPane<'_>) -> Result<()> {
    if pane
        .capture_viewport()?
        .contains("Configure providers and control")
    {
        pane.send_key(TmuxKey::Escape)?;
        pane.wait_stable_until("manager closed", READY_TIMEOUT, |text| {
            !text.contains("Configure providers and control")
        })?;
    }
    pane.wait_stable_contains("Corbanu Terminal · TPS:", READY_TIMEOUT)?;
    pane.send_literal("/providers")?;
    pane.wait_stable_contains("/providers", Duration::from_secs(10))?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Configure providers and control", READY_TIMEOUT)?;
    Ok(())
}

fn wait_claude_configured(pane: &TmuxPane<'_>) -> Result<()> {
    pane.wait_stable_until("Claude configured in manager", READY_TIMEOUT, |text| {
        text.lines()
            .any(|line| line.contains("Claude Account") && line.contains("Enabled · configured"))
    })?;
    Ok(())
}

fn exit_tui(pane: &TmuxPane<'_>) -> Result<()> {
    if pane
        .capture_viewport()?
        .contains("Configure providers and control")
    {
        pane.send_key(TmuxKey::Escape)?;
    }
    pane.wait_stable_contains("Corbanu Terminal · TPS:", READY_TIMEOUT)?;
    pane.send_literal("/exit")?;
    pane.wait_stable_contains("/exit", Duration::from_secs(10))?;
    pane.send_key(TmuxKey::Enter)?;
    Ok(())
}

fn assert_managed_resolver_returns(codex: &Path, codex_home: &Path, canary: &str) -> Result<()> {
    let output = Command::new(codex)
        .env("CORBANU_HOME", codex_home)
        .env("PFTERMINAL_HOME", codex_home)
        .env("CODEX_HOME", codex_home)
        .arg("internal-claude-oauth-token")
        .output()
        .context("run candidate managed-token resolver")?;
    ensure!(
        output.status.success(),
        "candidate managed-token resolver failed"
    );
    ensure!(
        output.stdout == canary.as_bytes(),
        "candidate resolver returned an unexpected token"
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn session_spec(
    name: &str,
    codex: &Path,
    repo_root: &Path,
    codex_home: &Path,
    log_dir: &Path,
    fake: &FakeClaude,
    login_fixture: Option<&CompatibilityLoginFixture>,
    provide_openai_fixture: bool,
) -> SessionSpec {
    let mut command = CommandSpec::new(codex.to_path_buf())
        .env("CORBANU_HOME", codex_home)
        .env("PFTERMINAL_HOME", codex_home)
        .env("CODEX_HOME", codex_home)
        .env("PATH", fake.path())
        // A navigation regression must fail locally, never start real OAuth.
        .env("CODEX_APP_SERVER_LOGIN_ISSUER", "http://127.0.0.1:9")
        .env("RUST_LOG", "trace")
        .arg("-c")
        .arg(format!("log_dir=\"{}\"", log_dir.display()))
        .arg("-c")
        .arg("analytics.enabled=false")
        .arg("-c")
        .arg("tui.animations=false")
        .arg("--no-alt-screen")
        .arg("-C")
        .arg(repo_root);
    if provide_openai_fixture {
        command = command.env("OPENAI_API_KEY", "tmux-claude-auth-openai-fixture");
    }
    if let Some(login_fixture) = login_fixture {
        command = login_fixture.apply_to(command);
    }
    SessionSpec::new(
        name,
        TerminalSize::new(/*columns*/ 140, /*rows*/ 44),
        command,
    )
    .current_dir(repo_root)
}

struct CompatibilityLoginFixture {
    _directory: TempDir,
    home: PathBuf,
    config_dir: PathBuf,
    security_executable: PathBuf,
}

impl CompatibilityLoginFixture {
    fn new() -> Result<Self> {
        let directory = tempdir()?;
        let home = directory.path().join("home");
        let config_dir = directory.path().join("claude-profile");
        fs::create_dir_all(&home)?;
        fs::create_dir_all(&config_dir)?;
        let credentials = r#"{"claudeAiOauth":{"accessToken":"fixture-access","refreshToken":"fixture-refresh","expiresAt":4102444800000,"scopes":["user:profile","user:inference"]}}"#;
        fs::write(config_dir.join(".credentials.json"), credentials)?;

        let security_executable = directory.path().join("security-fixture");
        fs::write(
            &security_executable,
            format!(
                "#!/bin/sh\n[ \"$1\" = \"find-generic-password\" ] || exit 2\nprintf '%s\\n' '{credentials}'\n"
            ),
        )?;
        let mut permissions = fs::metadata(&security_executable)?.permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&security_executable, permissions)?;

        Ok(Self {
            _directory: directory,
            home,
            config_dir,
            security_executable,
        })
    }

    fn apply_to(&self, command: CommandSpec) -> CommandSpec {
        command
            .env("HOME", &self.home)
            .env("USER", "corbanu-claude-auth-fixture")
            .env("CLAUDE_CONFIG_DIR", &self.config_dir)
            .env(
                "CORBANU_TEST_CLAUDE_SECURITY_EXECUTABLE",
                &self.security_executable,
            )
    }

    fn verify_hidden_health_command(&self, codex: &Path) -> Result<()> {
        let output = Command::new(codex)
            .env("CORBANU_HOME", &self.home)
            .env("PFTERMINAL_HOME", &self.home)
            .env("CODEX_HOME", &self.home)
            .env("HOME", &self.home)
            .env("USER", "corbanu-claude-auth-fixture")
            .env("CLAUDE_CONFIG_DIR", &self.config_dir)
            .env(
                "CORBANU_TEST_CLAUDE_SECURITY_EXECUTABLE",
                &self.security_executable,
            )
            .arg("internal-claude-login-health")
            .output()
            .context("run hidden Claude login health command against isolated fixture")?;
        ensure!(
            output.status.success(),
            "isolated Claude login fixture failed hidden health verification"
        );
        let source_id = String::from_utf8(output.stdout)
            .context("hidden Claude login health command returned invalid metadata")?;
        ensure!(
            source_id.trim().starts_with("claude-login:"),
            "hidden Claude login health command did not return a source identity"
        );
        ensure!(
            !source_id.contains("fixture-access") && !source_id.contains("fixture-refresh"),
            "hidden Claude login health command exposed fixture credential data"
        );
        Ok(())
    }
}

struct FakeClaude {
    _directory: TempDir,
    bin_dir: PathBuf,
}

impl FakeClaude {
    fn new() -> Result<Self> {
        let directory = tempdir()?;
        let bin_dir = directory.path().join("bin");
        fs::create_dir_all(&bin_dir)?;
        let executable = bin_dir.join("claude");
        fs::write(
            &executable,
            r#"#!/bin/sh
if [ "$1 $2 $3" = "auth status --json" ]; then
  printf '{"loggedIn":true,"authMethod":"claude.ai","email":"fixture@example.invalid","orgId":"org-tmux-fixture","subscriptionType":"max"}\n'
  exit 0
fi
exit 2
"#,
        )?;
        let mut permissions = fs::metadata(&executable)?.permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&executable, permissions)?;
        Ok(Self {
            _directory: directory,
            bin_dir,
        })
    }

    fn path(&self) -> OsString {
        let mut value = self.bin_dir.as_os_str().to_os_string();
        if let Some(existing) = std::env::var_os("PATH") {
            value.push(":");
            value.push(existing);
        }
        value
    }
}

fn synthetic_canary() -> String {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("synthetic-claude-oauth-{}-{nonce}", std::process::id())
}

fn tree_contains(root: &Path, needle: &[u8]) -> Result<bool> {
    secret_scan::tree_contains(root, needle, &|_| false)
}

fn codex_binary(repo_root: &Path) -> Result<PathBuf> {
    for binary in ["corbanu", "pfterminal", "codex"] {
        if let Ok(path) = codex_utils_cargo_bin::cargo_bin(binary) {
            return Ok(path);
        }
    }
    for binary in ["corbanu", "pfterminal", "codex"] {
        let fallback = repo_root.join("codex-rs/target/debug").join(binary);
        if fallback.is_file() {
            return Ok(fallback);
        }
    }
    anyhow::bail!(
        "Corbanu CLI binary with internal Claude-auth helpers is unavailable; build `corbanu` first"
    )
}

fn write_test_config(codex_home: &Path, repo_root: &Path) -> Result<()> {
    let repo_root = repo_root.display();
    let config = format!(
        "model = \"gpt-5.6-terra\"\nmodel_provider = \"openai\"\n\
         suppress_unstable_features_warning = true\n\n\
         [projects.\"{repo_root}\"]\ntrust_level = \"trusted\"\n"
    );
    fs::write(codex_home.join("config.toml"), config)
        .context("write Claude-auth test configuration")?;
    fs::write(
        codex_home.join("auth.json"),
        r#"{"OPENAI_API_KEY":"tmux-claude-auth-openai-fixture","tokens":null,"last_refresh":null}"#,
    )
    .context("write Claude-auth test authentication")
}
