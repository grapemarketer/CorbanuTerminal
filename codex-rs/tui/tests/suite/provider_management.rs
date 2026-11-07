use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use core_test_support::responses;
use tempfile::tempdir;
use uuid::Uuid;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

use crate::support::chat_ready::session_configured;
use crate::support::file_hash::sha256_file;
use crate::support::secret_scan;
use crate::support::tmux::CommandSpec;
use crate::support::tmux::SessionSpec;
use crate::support::tmux::TerminalSize;
use crate::support::tmux::TmuxKey;
use crate::support::tmux::TmuxPane;
use crate::support::tmux::TmuxServer;

const READY_TIMEOUT: Duration = Duration::from_secs(45);
const PRIMARY_PROVIDER: &str = "pf54-primary";
const SECONDARY_PROVIDER: &str = "pf54-secondary";
const MANAGED_PROVIDER: &str = "pf54-managed";
const ENV_PROVIDER: &str = "pf54-environment";

#[path = "provider_reauthentication.rs"]
mod reauthentication;

#[path = "provider_reauthentication_apps.rs"]
mod reauthentication_apps;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_astra_selection_cancel_restart_and_request() -> Result<()> {
    if !TmuxServer::should_run("Astra OpenAI selection, cancel, restart and request")? {
        return Ok(());
    }
    let fixture = Fixture::new("astra-selector", /*openai_auth*/ true).await?;
    let config_path = fixture.home.path().join("config.toml");
    let mut config: toml::Value = toml::from_str(&fs::read_to_string(&config_path)?)?;
    let settings = config.as_table_mut().context("fixture config is a table")?;
    settings.insert("model".into(), "gpt-5.6-sol".into());
    settings.insert("model_provider".into(), "openai".into());
    settings.insert("model_reasoning_effort".into(), "low".into());
    settings.insert("check_for_update_on_startup".into(), false.into());
    fs::write(&config_path, toml::to_string(&config)?)?;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(
                    response("astra selector request succeeded"),
                    "text/event-stream",
                ),
        )
        .mount(&fixture.server)
        .await;
    let tmux = fixture.tmux()?;
    for restart in [false, true] {
        let session = tmux.new_session(SessionSpec::new(
            "astra-selector",
            TerminalSize::new(/*columns*/ 140, /*rows*/ 44),
            CommandSpec::new(&fixture.binary)
                .env("CODEX_HOME", fixture.home.path())
                .env("CORBANU_HOME", fixture.home.path())
                .env("PFTERMINAL_HOME", fixture.home.path())
                .env("RUST_LOG", "trace")
                .arg("-c")
                .arg(format!("openai_base_url=\"{}/v1\"", fixture.server.uri()))
                .arg("-c")
                .arg("analytics.enabled=false")
                .arg("-c")
                .arg("tui.animations=false")
                .arg("-c")
                .arg(format!("log_dir={:?}", fixture.home.path().join("log")))
                .arg("--no-alt-screen")
                .arg("-C")
                .arg(&fixture.repo_root),
        ))?;
        let pane = session.primary_pane();
        wait_chat_ready(pane)?;
        if !restart {
            wait_session_configured(pane)?;
            let before = fs::read_to_string(&config_path)?;
            open_model_picker(pane)?;
            pane.wait_stable_contains("GPT-5.6-Sol", READY_TIMEOUT)?;
            select_label(pane, "GPT-6 Astra")?;
            pane.wait_stable_contains("Select Reasoning Level for GPT-6 Astra", READY_TIMEOUT)?;
            capture_success("astra-reasoning-cancel", &fixture, pane, &[])?;
            pane.send_key(TmuxKey::Escape)?;
            // Returning from effort selection may reveal its parent picker.
            close_manager(pane)?;
            ensure!(
                fs::read_to_string(&config_path)? == before,
                "cancel changed selection"
            );
            open_model_picker(pane)?;
            select_label(pane, "GPT-6 Astra")?;
            pane.wait_stable_contains("Select Reasoning Level for GPT-6 Astra", READY_TIMEOUT)?;
            select_label(pane, "High")?;
            wait_chat_ready(pane)?;
            pane.wait_stable_contains("GPT-6 Astra", READY_TIMEOUT)?;
        }
        let selected: toml::Value = toml::from_str(&fs::read_to_string(&config_path)?)?;
        ensure!(
            (
                selected["model"].as_str(),
                selected["model_provider"].as_str(),
                selected["model_reasoning_effort"].as_str()
            ) == (Some("gpt-6-astra"), Some("openai"), Some("high")),
            "Astra model/provider/effort did not persist"
        );
        submit_and_wait(
            pane,
            "Check the selected model.",
            "astra selector request succeeded",
        )?;
        capture_success(
            &format!("astra-selected-restart-{restart}"),
            &fixture,
            pane,
            &[],
        )?;
        exit_tui(pane)?;
        session.wait_for_exit(READY_TIMEOUT)?;
    }
    let requests = fixture.server.received_requests().await.unwrap_or_default();
    let requests = requests
        .iter()
        .filter(|request| request.url.path() == "/v1/responses")
        .collect::<Vec<_>>();
    ensure!(requests.len() == 2, "expected one request per process");
    for request in requests {
        let body: serde_json::Value = serde_json::from_slice(&request.body)?;
        ensure!(body["model"] == "gpt-6-astra" && body["reasoning"]["effort"] == "high");
        ensure!(
            request
                .headers
                .get("version")
                .and_then(|value| value.to_str().ok())
                == Some(codex_model_provider_info::OPENAI_CODEX_COMPAT_VERSION)
        );
        let version = request.headers["version"]
            .to_str()?
            .split('.')
            .map(str::parse::<u32>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ensure!(
            version >= vec![0, 153, 0],
            "Astra requires native client 0.153.0 or newer"
        );
        ensure!(
            request
                .headers
                .contains_key("x-openai-internal-codex-responses-lite")
        );
        ensure!(body.get("instructions").is_none());
        ensure!(body.get("tools").is_none());
        ensure!(body["parallel_tool_calls"] == false);
        ensure!(
            body["input"]
                .as_array()
                .context("native input")?
                .iter()
                .any(|item| item["type"] == "additional_tools"
                    && item["tools"]
                        .as_array()
                        .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "exec")))
        );
        for unsupported in [
            "temperature",
            "top_p",
            "top_logprobs",
            "logprobs",
            "prompt_cache_retention",
        ] {
            ensure!(
                body.get(unsupported).is_none(),
                "unsupported Astra parameter {unsupported}"
            );
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_ambient_model_picker_offers_only_glm() -> Result<()> {
    if !TmuxServer::should_run("Ambient GLM-only picker")? {
        return Ok(());
    }
    let fixture = Fixture::new("ambient-glm-only", /*openai_auth*/ true).await?;
    let tmux = fixture.tmux()?;
    let session = tmux.new_session(SessionSpec::new(
        "ambient-glm-only",
        TerminalSize::new(/*columns*/ 140, /*rows*/ 44),
        CommandSpec::new(&fixture.binary)
            .env("CODEX_HOME", fixture.home.path())
            .env("CORBANU_HOME", fixture.home.path())
            .env("AMBIENT_API_KEY", "ambient-picker-synthetic-fixture")
            .env("RUST_LOG", "trace")
            .arg("-c")
            .arg("tui.animations=false")
            .arg("-c")
            .arg("model_provider=\"ambient\"")
            .arg("-m")
            .arg("z-ai/glm-5.2")
            .arg("-c")
            .arg(format!("log_dir={:?}", fixture.home.path().join("logs")))
            .arg("--no-alt-screen")
            .arg("-C")
            .arg(&fixture.repo_root),
    ))?;
    let pane = session.primary_pane();
    wait_chat_ready(pane)?;
    wait_session_configured(pane)?;
    for _ in 0..2 {
        open_model_picker(pane)?;
        pane.wait_stable_contains("[Ambient]", READY_TIMEOUT)?;
        let capture = pane.wait_stable_contains("Ambient GLM 5.2", READY_TIMEOUT)?;
        ensure!(
            !capture.contains("Kimi K2.7"),
            "retired Ambient option is visible"
        );
        capture_success("ambient-glm-only", &fixture, pane, &[])?;
        pane.send_key(TmuxKey::Escape)?;
        pane.wait_stable_until("model picker cancelled", READY_TIMEOUT, |capture| {
            !capture.contains("Select Model")
        })?;
    }
    exit_tui(pane)?;
    session.wait_for_exit(READY_TIMEOUT)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_shared_and_custom_catalog_have_management_status_parity() -> Result<()> {
    if !TmuxServer::should_run("PF-54 shared/custom catalog parity")? {
        return Ok(());
    }
    let fixture = Fixture::new("catalog-parity", /*openai_auth*/ true).await?;
    let tmux = fixture.tmux()?;
    let session = tmux.new_session(fixture.session("pf54-catalog-parity"))?;
    let pane = session.primary_pane();
    wait_chat_ready(pane)?;

    open_manager(pane)?;
    pane.wait_stable_contains("OpenAI", READY_TIMEOUT)?;
    inspect_provider(pane, "PF54 Primary", "Enabled · configured · current")?;
    open_manager(pane)?;
    inspect_provider(pane, "PF54 Managed", "Not configured")?;
    open_manager(pane)?;
    inspect_provider(
        pane,
        "PF54 Broken Environment",
        "Credential needs attention",
    )?;

    capture_success("catalog-parity", &fixture, pane, &[])?;
    close_overlay_and_exit(pane)?;
    session.wait_for_exit(READY_TIMEOUT)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_pf50_api_key_setup_and_recovery_are_reused() -> Result<()> {
    if !TmuxServer::should_run("PF-54 PF-50 API-key setup/recovery")? {
        return Ok(());
    }
    let fixture = Fixture::new("api-setup-recovery", /*openai_auth*/ true).await?;
    let tmux = fixture.tmux()?;
    let session = tmux.new_session(fixture.session("pf54-api-setup-recovery"))?;
    let pane = session.primary_pane();
    wait_chat_ready(pane)?;
    let canary = synthetic_canary("api-key");

    open_manager(pane)?;
    select_label(pane, "PF54 Managed")?;
    pane.wait_stable_contains("Not configured", READY_TIMEOUT)?;
    select_label(pane, "Set up with API key")?;
    pane.wait_stable_contains("API key — masked", READY_TIMEOUT)?;
    pane.wait_stable_contains("encrypted vault", READY_TIMEOUT)?;
    pane.send_secret_literal(&canary)?;
    pane.send_key(TmuxKey::Enter)?;
    wait_manager_row(
        pane,
        fixture.home.path(),
        "PF54 Managed",
        "Enabled · configured",
    )?;
    inspect_provider(pane, "PF54 Managed", "Enabled · configured")?;

    open_manager(pane)?;
    select_label(pane, "PF54 Broken Environment")?;
    pane.wait_stable_contains("Credential needs attention", READY_TIMEOUT)?;
    select_label(pane, "Recover with API key")?;
    pane.wait_stable_contains("API key — masked", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Escape)?;
    pane.wait_stable_until("API recovery cancellation", READY_TIMEOUT, |capture| {
        !capture.contains("API key — masked")
    })?;

    capture_success("api-setup-recovery", &fixture, pane, &[&canary])?;
    close_overlay_and_exit(pane)?;
    session.wait_for_exit(READY_TIMEOUT)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_pf51_openai_account_cancel_and_retry_are_correlated() -> Result<()> {
    if !TmuxServer::should_run("PF-54 PF-51 OpenAI cancel/retry")? {
        return Ok(());
    }
    let fixture = Fixture::new("openai-cancel-retry", /*openai_auth*/ false).await?;
    let tmux = fixture.tmux()?;
    let session = tmux.new_session(fixture.session("pf54-openai-cancel-retry"))?;
    let pane = session.primary_pane();
    wait_chat_ready(pane)?;

    begin_openai_account_auth(pane)?;
    cancel_account_auth(pane)?;
    begin_openai_account_auth(pane)?;
    pane.wait_stable_contains("OpenAI account login", READY_TIMEOUT)?;
    cancel_account_auth(pane)?;

    fixture.server.reset().await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&fixture.server)
        .await;
    open_manager(pane)?;
    select_label(pane, "OpenAI")?;
    select_label(pane, "Set up with OpenAI account")?;
    pane.wait_stable_contains("Authentication needs attention", READY_TIMEOUT)?;
    pane.wait_stable_contains("OpenAI could not start login", READY_TIMEOUT)?;
    fixture.server.reset().await;
    mock_openai_device_code(&fixture.server).await;
    select_label(pane, "Retry authentication")?;
    pane.wait_stable_contains("OpenAI account login", READY_TIMEOUT)?;
    pane.wait_stable_contains("remote or headless", READY_TIMEOUT)?;
    cancel_account_auth(pane)?;

    capture_success("openai-cancel-retry", &fixture, pane, &[])?;
    exit_tui(pane)?;
    session.wait_for_exit(READY_TIMEOUT)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_pf52_claude_recovery_cancel_and_retry_are_reused() -> Result<()> {
    if !TmuxServer::should_run("PF-54 PF-52 Claude recovery/cancel")? {
        return Ok(());
    }
    let fixture = Fixture::new("claude-recovery-cancel", /*openai_auth*/ true).await?;
    let selection = codex_vault::ClaudeAuthSelection::new(
        codex_vault::ClaudeAuthSource::ManagedSubscriptionToken,
        "pf54-missing-managed-token",
    )
    .map_err(anyhow::Error::msg)?;
    codex_vault::Vault::new(fixture.home.path().to_path_buf())
        .save_claude_auth_selection(&selection)?;
    let tmux = fixture.tmux()?;
    let session = tmux.new_session(fixture.session("pf54-claude-recovery-cancel"))?;
    let pane = session.primary_pane();
    wait_chat_ready(pane)?;
    let canary = synthetic_canary("claude-managed-token");
    // The local token format check accepts arbitrary whitespace-free strings;
    // embedded whitespace makes this a deterministic rejection before storage.
    let invalid_canary = format!("invalid {canary}");

    open_manager(pane)?;
    focus_label(pane, "Claude Account")?;
    pane.wait_stable_until("Claude recovery status", READY_TIMEOUT, |capture| {
        selected_row(capture).is_some_and(|row| row.contains("Credential needs attention"))
    })?;
    pane.send_key(TmuxKey::Enter)?;
    select_label(pane, "Recover with Claude account")?;
    pane.wait_stable_contains("Claude Plan authentication", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Escape)?;
    pane.wait_stable_contains("Configure providers and control", READY_TIMEOUT)?;
    close_manager(pane)?;

    open_manager(pane)?;
    focus_label(pane, "Claude Account")?;
    pane.wait_stable_until("Claude retry recovery status", READY_TIMEOUT, |capture| {
        selected_row(capture).is_some_and(|row| row.contains("Credential needs attention"))
    })?;
    pane.send_key(TmuxKey::Enter)?;
    select_label(pane, "Recover with Claude account")?;
    pane.wait_stable_contains("Claude Plan authentication", READY_TIMEOUT)?;
    pane.wait_stable_contains("claude setup-token", READY_TIMEOUT)?;
    select_label(pane, "Long-lived subscription token (Recommended)")?;
    pane.wait_stable_contains("Long-lived token — masked", READY_TIMEOUT)?;
    pane.wait_stable_contains("claude setup-token", READY_TIMEOUT)?;
    pane.send_secret_literal(&canary)?;
    pane.send_key(TmuxKey::Escape)?;
    pane.wait_stable_contains("Configure providers and control", READY_TIMEOUT)?;

    // A rejected token must remain visibly recoverable without leaking its value.
    close_manager(pane)?;
    open_manager(pane)?;
    select_label(pane, "Claude Account")?;
    select_label(pane, "Recover with Claude account")?;
    select_label(pane, "Long-lived subscription token (Recommended)")?;
    pane.wait_stable_contains("Long-lived token — masked", READY_TIMEOUT)?;
    pane.send_secret_literal(&invalid_canary)?;
    pane.wait_stable_contains(
        &format!("••••••• {}", "•".repeat(canary.chars().count())),
        READY_TIMEOUT,
    )?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Authentication needs attention", READY_TIMEOUT)?;
    pane.wait_stable_contains("subscription token was not accepted", READY_TIMEOUT)?;
    select_label(pane, "Retry authentication")?;
    pane.wait_stable_contains("Long-lived token — masked", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Escape)?;
    pane.wait_stable_contains("Configure providers and control", READY_TIMEOUT)?;

    // A successful local save must return to the manager, not reopen a blank
    // token form. This synthetic value never makes a live inference request.
    close_manager(pane)?;
    open_manager(pane)?;
    select_label(pane, "Claude Account")?;
    select_label(pane, "Recover with Claude account")?;
    select_label(pane, "Long-lived subscription token (Recommended)")?;
    pane.wait_stable_contains("Long-lived token — masked", READY_TIMEOUT)?;
    pane.send_secret_literal(&canary)?;
    pane.wait_stable_contains(&"•".repeat(canary.chars().count()), READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Configure providers and control", READY_TIMEOUT)?;
    ensure!(
        !pane
            .capture_viewport()?
            .contains("Save Claude subscription token")
    );

    capture_success(
        "claude-recovery-cancel",
        &fixture,
        pane,
        &[&canary, &invalid_canary],
    )?;
    close_overlay_and_exit(pane)?;
    session.wait_for_exit(READY_TIMEOUT)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_noncurrent_deactivate_reactivate_restart_retains_request_credential() -> Result<()> {
    if !TmuxServer::should_run("PF-54 noncurrent retention/restart/request")? {
        return Ok(());
    }
    let fixture = Fixture::new("noncurrent-retention", /*openai_auth*/ true).await?;
    let _response = responses::mount_sse_once(
        &fixture.server,
        response("pf54 retained credential request succeeded"),
    )
    .await;
    let tmux = fixture.tmux()?;
    let canary = synthetic_canary("retained-provider-key");
    let first = tmux.new_session(fixture.session("pf54-retention-first"))?;
    let pane = first.primary_pane();
    wait_chat_ready(pane)?;
    configure_managed_provider(pane, fixture.home.path(), &canary)?;
    open_manager(pane)?;
    select_label(pane, "PF54 Managed")?;
    select_label(pane, "Deactivate")?;
    pane.wait_stable_contains("Inactive", READY_TIMEOUT)?;
    capture_success("noncurrent-inactive", &fixture, pane, &[&canary])?;
    close_manager(pane)?;
    open_model_picker(pane)?;
    pane.wait_stable_contains("[Other]", READY_TIMEOUT)?;
    let inactive_model_count = numbered_selection_row_count(&pane.capture_viewport()?);
    pane.send_key(TmuxKey::Escape)?;
    wait_chat_ready(pane)?;
    exit_tui(pane)?;
    first.wait_for_exit(READY_TIMEOUT)?;

    let second = tmux.new_session(fixture.session("pf54-retention-reactivate"))?;
    let pane = second.primary_pane();
    wait_chat_ready(pane)?;
    open_manager(pane)?;
    inspect_provider(pane, "PF54 Managed", "Inactive")?;
    open_model_picker(pane)?;
    pane.wait_stable_contains("[Other]", READY_TIMEOUT)?;
    ensure!(
        numbered_selection_row_count(&pane.capture_viewport()?) == inactive_model_count,
        "inactive provider changed picker membership across restart"
    );
    pane.send_key(TmuxKey::Escape)?;
    wait_chat_ready(pane)?;
    open_manager(pane)?;
    select_label(pane, "PF54 Managed")?;
    select_label(pane, "Reactivate")?;
    pane.wait_stable_contains("Enabled · configured", READY_TIMEOUT)?;
    close_manager(pane)?;
    open_model_picker(pane)?;
    pane.wait_stable_contains("[Other]", READY_TIMEOUT)?;
    pane.wait_stable_until(
        "reactivated provider picker membership",
        READY_TIMEOUT,
        |capture| numbered_selection_row_count(capture) == inactive_model_count + 1,
    )?;
    pane.send_key(TmuxKey::Escape)?;
    wait_chat_ready(pane)?;
    exit_tui(pane)?;
    second.wait_for_exit(READY_TIMEOUT)?;

    select_config_provider(fixture.home.path(), MANAGED_PROVIDER)?;
    let third = tmux.new_session(fixture.session("pf54-retention-request"))?;
    let pane = third.primary_pane();
    wait_chat_ready(pane)?;
    submit_and_wait(
        pane,
        "prove retained PF54 provider credential",
        "pf54 retained credential request succeeded",
    )?;
    ensure_authorization_seen(&fixture.server, &canary).await?;
    capture_success("noncurrent-retention-request", &fixture, pane, &[&canary])?;
    exit_tui(pane)?;
    third.wait_for_exit(READY_TIMEOUT)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_current_deactivate_cancel_is_inert() -> Result<()> {
    if !TmuxServer::should_run("PF-54 current deactivate cancellation")? {
        return Ok(());
    }
    let fixture = Fixture::new("current-cancel", /*openai_auth*/ true).await?;
    let original = fs::read(fixture.home.path().join("config.toml"))?;
    let tmux = fixture.tmux()?;
    let session = tmux.new_session(fixture.session("pf54-current-cancel"))?;
    let pane = session.primary_pane();
    wait_chat_ready(pane)?;

    open_manager(pane)?;
    select_label(pane, "PF54 Primary")?;
    pane.wait_stable_contains("Enabled · configured · current", READY_TIMEOUT)?;
    select_label(pane, "Deactivate")?;
    pane.wait_stable_contains("Choose replacement", READY_TIMEOUT)?;
    pane.wait_stable_contains("PF54 Secondary — fixture-model", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Escape)?;
    pane.wait_stable_contains("Configure providers and control", READY_TIMEOUT)?;

    ensure!(
        !fixture
            .home
            .path()
            .join("provider-eligibility.json")
            .exists(),
        "cancelled current-provider deactivation persisted eligibility"
    );
    ensure!(
        fs::read(fixture.home.path().join("config.toml"))? == original,
        "cancelled current-provider deactivation changed config"
    );
    capture_success("current-cancel", &fixture, pane, &[])?;
    close_overlay_and_exit(pane)?;
    session.wait_for_exit(READY_TIMEOUT)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_current_exact_replacement_is_persisted_before_deactivation() -> Result<()> {
    if !TmuxServer::should_run("PF-54 exact replacement ordering/restart")? {
        return Ok(());
    }
    let fixture = Fixture::new("current-replacement", /*openai_auth*/ true).await?;
    let tmux = fixture.tmux()?;
    let first = tmux.new_session(fixture.session("pf54-current-replacement"))?;
    let pane = first.primary_pane();
    wait_chat_ready(pane)?;

    open_manager(pane)?;
    select_label(pane, "PF54 Primary")?;
    select_label(pane, "Deactivate")?;
    pane.wait_stable_contains("Choose replacement", READY_TIMEOUT)?;
    select_label(pane, "PF54 Secondary — fixture-model")?;
    wait_replacement_order(fixture.home.path(), SECONDARY_PROVIDER)?;
    pane.wait_stable_contains("Configure providers and control", READY_TIMEOUT)?;
    inspect_provider(pane, "PF54 Primary", "Inactive")?;
    capture_success("current-replacement", &fixture, pane, &[])?;
    close_overlay_and_exit(pane)?;
    first.wait_for_exit(READY_TIMEOUT)?;

    let second = tmux.new_session(fixture.session("pf54-current-replacement-restart"))?;
    let pane = second.primary_pane();
    wait_chat_ready(pane)?;
    open_manager(pane)?;
    inspect_provider(pane, "PF54 Secondary", "Enabled · configured · current")?;
    open_manager(pane)?;
    inspect_provider(pane, "PF54 Primary", "Inactive")?;
    capture_success("current-replacement-restart", &fixture, pane, &[])?;
    close_overlay_and_exit(pane)?;
    second.wait_for_exit(READY_TIMEOUT)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_environment_copy_and_eligibility_never_delete_credentials() -> Result<()> {
    if !TmuxServer::should_run("PF-54 environment-backed management copy")? {
        return Ok(());
    }
    let fixture = Fixture::new("environment-copy", /*openai_auth*/ true).await?;
    let config_before = fs::read(fixture.home.path().join("config.toml"))?;
    let auth_before = fs::read(fixture.home.path().join("auth.json"))?;
    let tmux = fixture.tmux()?;
    let session = tmux.new_session(fixture.session("pf54-environment-copy"))?;
    let pane = session.primary_pane();
    wait_chat_ready(pane)?;

    open_manager(pane)?;
    select_label(pane, "PF54 Environment")?;
    pane.wait_stable_contains(
        "Environment-backed credential: deactivate here; unset it outside Corbanu.",
        READY_TIMEOUT,
    )?;
    select_label(pane, "Deactivate")?;
    pane.wait_stable_contains("Inactive", READY_TIMEOUT)?;
    select_label(pane, "PF54 Environment")?;
    select_label(pane, "Reactivate")?;
    pane.wait_stable_contains("Enabled · configured", READY_TIMEOUT)?;

    ensure!(fs::read(fixture.home.path().join("config.toml"))? == config_before);
    ensure!(fs::read(fixture.home.path().join("auth.json"))? == auth_before);
    ensure!(
        fs::read_to_string(fixture.home.path().join("provider-eligibility.json"))?
            .contains("inactive_identities")
    );
    capture_success("environment-copy", &fixture, pane, &[])?;
    close_overlay_and_exit(pane)?;
    session.wait_for_exit(READY_TIMEOUT)?;
    Ok(())
}

struct Fixture {
    repo_root: PathBuf,
    binary: PathBuf,
    home: tempfile::TempDir,
    server: MockServer,
    scenario: &'static str,
}

impl Fixture {
    async fn new(scenario: &'static str, openai_auth: bool) -> Result<Self> {
        let repo_root = codex_utils_cargo_bin::repo_root()?;
        let binary = codex_binary(&repo_root)?;
        let home = tempdir()?;
        let server = MockServer::start().await;
        mock_openai_device_code(&server).await;
        write_config(home.path(), &repo_root, &server.uri())?;
        if openai_auth {
            fs::write(
                home.path().join("auth.json"),
                r#"{"OPENAI_API_KEY":"pf54-openai-fixture","tokens":null,"last_refresh":null}"#,
            )?;
        }
        Ok(Self {
            repo_root,
            binary,
            home,
            server,
            scenario,
        })
    }

    fn tmux(&self) -> Result<TmuxServer> {
        let tmux = TmuxServer::start(&format!("pf54_{}", self.scenario.replace('-', "_")))?;
        register_evidence(&tmux, self.home.path(), &self.binary)?;
        Ok(tmux)
    }

    fn session(&self, name: &str) -> SessionSpec {
        session_spec(
            name,
            &self.binary,
            &self.repo_root,
            self.home.path(),
            &self.server.uri(),
        )
    }
}

fn begin_openai_account_auth(pane: &TmuxPane<'_>) -> Result<()> {
    open_manager(pane)?;
    select_label(pane, "OpenAI")?;
    pane.wait_stable_contains("Not configured", READY_TIMEOUT)?;
    select_label(pane, "Set up with OpenAI account")?;
    pane.wait_stable_contains("OpenAI account login", READY_TIMEOUT)?;
    Ok(())
}

fn cancel_account_auth(pane: &TmuxPane<'_>) -> Result<()> {
    pane.send_key(TmuxKey::Escape)?;
    pane.wait_stable_contains("Configure providers and control", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Escape)?;
    wait_chat_ready(pane)?;
    Ok(())
}

fn configure_managed_provider(pane: &TmuxPane<'_>, home: &Path, canary: &str) -> Result<()> {
    open_manager(pane)?;
    select_label(pane, "PF54 Managed")?;
    select_label(pane, "Set up with API key")?;
    pane.wait_stable_contains("API key — masked", READY_TIMEOUT)?;
    pane.send_secret_literal(canary)?;
    pane.send_key(TmuxKey::Enter)?;
    wait_manager_row(pane, home, "PF54 Managed", "Enabled · configured")?;
    pane.send_key(TmuxKey::Escape)?;
    wait_chat_ready(pane)?;
    Ok(())
}

fn wait_manager_row(pane: &TmuxPane<'_>, home: &Path, provider: &str, status: &str) -> Result<()> {
    let eligibility = home.join("provider-eligibility.json");
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    while std::time::Instant::now() < deadline && !eligibility.is_file() {
        std::thread::sleep(Duration::from_millis(50));
    }
    ensure!(
        eligibility.is_file(),
        "provider setup never reached durable eligibility persistence"
    );
    let managed =
        match codex_login::provider_api_key_metadata_from_auth_storage(home, "PF54_MANAGED_KEY") {
            Ok(codex_login::ProviderApiKeyStorageMetadata::Stored { source }) => {
                format!("Stored({source:?})")
            }
            Ok(codex_login::ProviderApiKeyStorageMetadata::Missing) => "Missing".to_string(),
            Ok(codex_login::ProviderApiKeyStorageMetadata::Suppressed) => "Suppressed".to_string(),
            Err(error) => format!("Error({:?})", error.kind()),
        };
    fs::write(home.join("post-save-provider-metadata.txt"), managed)?;
    pane.wait_stable_contains("Configure providers and control", READY_TIMEOUT)?;
    focus_label(pane, provider)?;
    pane.wait_stable_until("provider status refresh", READY_TIMEOUT, |capture| {
        selected_row(capture)
            .is_some_and(|selected| selected.contains(provider) && selected.contains(status))
    })?;
    Ok(())
}

fn inspect_provider(pane: &TmuxPane<'_>, provider: &str, expected: &str) -> Result<()> {
    select_label(pane, provider)?;
    pane.wait_stable_contains(expected, READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Escape)?;
    pane.wait_stable_contains("Configure providers and control", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Escape)?;
    wait_chat_ready(pane)?;
    Ok(())
}

fn open_manager(pane: &TmuxPane<'_>) -> Result<()> {
    pane.send_literal("/providers")?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Configure providers and control", READY_TIMEOUT)?;
    Ok(())
}

fn close_manager(pane: &TmuxPane<'_>) -> Result<()> {
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    while std::time::Instant::now() < deadline {
        if chat_footer_visible(&pane.capture_viewport()?) {
            return Ok(());
        }
        pane.send_key(TmuxKey::Escape)?;
        std::thread::sleep(Duration::from_millis(150));
    }
    anyhow::bail!("timed out waiting for provider manager to close")
}

fn open_model_picker(pane: &TmuxPane<'_>) -> Result<()> {
    pane.send_literal("/model")?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Select Model", READY_TIMEOUT)?;
    Ok(())
}

pub(super) fn select_label(pane: &TmuxPane<'_>, label: &str) -> Result<()> {
    focus_label(pane, label)?;
    pane.send_key(TmuxKey::Enter)?;
    Ok(())
}

fn focus_label(pane: &TmuxPane<'_>, label: &str) -> Result<()> {
    for _ in 0..64 {
        let capture = pane.capture_viewport()?;
        let selected = selected_row(&capture);
        if selected
            .as_deref()
            .and_then(selected_title)
            .is_some_and(|title| selected_title_matches(title, label))
        {
            return Ok(());
        }
        pane.send_key(TmuxKey::Down)?;
        pane.wait_stable_until(
            "selection redraw after Down",
            Duration::from_secs(5),
            |next| selected_row(next) != selected,
        )?;
    }
    anyhow::bail!(
        "could not select {label:?}; last capture:\n{}",
        pane.capture_viewport()?
    )
}

fn selected_row(capture: &str) -> Option<String> {
    capture
        .lines()
        // Submitted chat prompts use the same cursor; the active menu is below them.
        .rev()
        .find(|line| strip_selection_cursor(line).is_some())
        .map(str::trim)
        .map(str::to_owned)
}

fn selected_title(row: &str) -> Option<&str> {
    let selected = strip_selection_cursor(row)?;
    if let Some((number, title)) = selected.split_once(". ")
        && number.chars().all(|character| character.is_ascii_digit())
    {
        return Some(title.trim());
    }
    Some(selected.trim())
}

fn strip_selection_cursor(row: &str) -> Option<&str> {
    let trimmed = row.trim();
    let remainder = trimmed
        .strip_prefix('>')
        .or_else(|| trimmed.strip_prefix('›'))?;
    remainder
        .chars()
        .next()
        .is_some_and(char::is_whitespace)
        .then(|| remainder.trim_start())
}

fn selected_title_matches(title: &str, requested: &str) -> bool {
    title == requested
        || title
            .strip_prefix(requested)
            .is_some_and(|inline| inline.starts_with("  "))
}

fn numbered_selection_row_count(capture: &str) -> usize {
    capture
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            let row = trimmed
                .strip_prefix('>')
                .or_else(|| trimmed.strip_prefix('›'))
                .unwrap_or(trimmed)
                .trim_start();
            row.split_once(". ")
        })
        .filter(|(number, _)| number.chars().all(|character| character.is_ascii_digit()))
        .count()
}

fn wait_chat_ready(pane: &TmuxPane<'_>) -> Result<()> {
    pane.wait_stable_until("chat ready", READY_TIMEOUT, |capture| {
        chat_footer_visible(capture)
            || (capture.contains("/model to change")
                && !capture.contains("Press enter to confirm or esc to go back"))
    })?;
    Ok(())
}

/// The footer `wait_chat_ready` accepts is drawn before the session is configured, and a
/// `/model` entered in that window is either refused ("disabled until startup completes") or
/// queued, depending on keystroke timing. Wait for the configured header, which is visible
/// right after startup, before the first `/model`.
fn wait_session_configured(pane: &TmuxPane<'_>) -> Result<()> {
    pane.wait_stable_until(
        "configured session header",
        READY_TIMEOUT,
        session_configured,
    )?;
    Ok(())
}

fn chat_footer_visible(capture: &str) -> bool {
    capture
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(3)
        .any(|line| line.contains("Corbanu Terminal · TPS:"))
}

fn submit_and_wait(pane: &TmuxPane<'_>, prompt: &str, response: &str) -> Result<()> {
    wait_chat_ready(pane)?;
    pane.send_literal(prompt)?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains(response, READY_TIMEOUT)?;
    Ok(())
}

fn close_overlay_and_exit(pane: &TmuxPane<'_>) -> Result<()> {
    pane.send_key(TmuxKey::Escape)?;
    pane.wait_stable_until("management overlay closed", READY_TIMEOUT, |capture| {
        !capture.contains("Configure providers and control")
    })?;
    exit_tui(pane)
}

fn exit_tui(pane: &TmuxPane<'_>) -> Result<()> {
    pane.send_literal("/exit")?;
    pane.wait_stable_contains("/exit", Duration::from_secs(5))?;
    pane.send_key(TmuxKey::Enter)?;
    Ok(())
}

fn session_spec(
    name: &str,
    binary: &Path,
    repo_root: &Path,
    home: &Path,
    login_issuer: &str,
) -> SessionSpec {
    SessionSpec::new(
        name,
        TerminalSize::new(/*columns*/ 140, /*rows*/ 44),
        CommandSpec::new(binary)
            .env("CODEX_HOME", home)
            .env("CORBANU_HOME", home)
            .env("PFTERMINAL_HOME", home)
            .env("PF54_PRIMARY_KEY", "pf54-primary-environment-fixture")
            .env("PF54_SECONDARY_KEY", "pf54-secondary-environment-fixture")
            .env("PF54_ENV_KEY", "pf54-external-environment-fixture")
            .env("PF54_BROKEN_KEY", "")
            .env("CODEX_APP_SERVER_LOGIN_ISSUER", login_issuer)
            .env(
                "CODEX_REFRESH_TOKEN_URL_OVERRIDE",
                format!("{login_issuer}/oauth/token"),
            )
            .env("RUST_LOG", "trace")
            .arg("-c")
            .arg("analytics.enabled=false")
            .arg("-c")
            .arg("tui.animations=false")
            .arg("--no-alt-screen")
            .arg("-C")
            .arg(repo_root),
    )
    .current_dir(repo_root)
}

async fn mock_openai_device_code(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"device_auth_id":"pf54-device","user_code":"PF54-CODE","interval":"1"}"#,
            "application/json",
        ))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(ResponseTemplate::new(403))
        .mount(server)
        .await;
}

fn write_config(home: &Path, repo_root: &Path, server_uri: &str) -> Result<()> {
    fs::create_dir_all(home.join("log"))?;
    fs::write(
        home.join("config.toml"),
        format!(
            r#"model = "fixture-model"
model_provider = "{PRIMARY_PROVIDER}"
cli_auth_credentials_store = "file"
suppress_unstable_features_warning = true
log_dir = "{}"

[model_providers.{PRIMARY_PROVIDER}]
name = "PF54 Primary"
base_url = "{server_uri}/v1"
env_key = "PF54_PRIMARY_KEY"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0

[model_providers.{SECONDARY_PROVIDER}]
name = "PF54 Secondary"
base_url = "{server_uri}/v1"
env_key = "PF54_SECONDARY_KEY"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0

[model_providers.{MANAGED_PROVIDER}]
name = "PF54 Managed"
base_url = "{server_uri}/v1"
env_key = "PF54_MANAGED_KEY"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0

[model_providers.{ENV_PROVIDER}]
name = "PF54 Environment"
base_url = "{server_uri}/v1"
env_key = "PF54_ENV_KEY"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0

[model_providers.pf54-broken]
name = "PF54 Broken Environment"
base_url = "{server_uri}/v1"
env_key = "PF54_BROKEN_KEY"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0

[projects."{}"]
trust_level = "trusted"
"#,
            home.join("log").display(),
            repo_root.display(),
        ),
    )
    .context("write PF-54 tmux config")
}

fn select_config_provider(home: &Path, provider: &str) -> Result<()> {
    let path = home.join("config.toml");
    let config = fs::read_to_string(&path)?;
    let updated = config.replacen(
        &format!("model_provider = \"{PRIMARY_PROVIDER}\""),
        &format!("model_provider = \"{provider}\""),
        1,
    );
    ensure!(
        updated != config,
        "current provider config was not replaced"
    );
    fs::write(path, updated)?;
    Ok(())
}

fn wait_replacement_order(home: &Path, replacement: &str) -> Result<()> {
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    let config_path = home.join("config.toml");
    let eligibility_path = home.join("provider-eligibility.json");
    while std::time::Instant::now() < deadline {
        let selected = fs::read_to_string(&config_path)
            .is_ok_and(|config| config.contains(&format!("model_provider = \"{replacement}\"")));
        if eligibility_path.exists() && !selected {
            anyhow::bail!("old provider was deactivated before replacement persistence");
        }
        if eligibility_path.exists() && selected {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    anyhow::bail!("timed out waiting for ordered replacement persistence")
}

async fn ensure_authorization_seen(server: &MockServer, canary: &str) -> Result<()> {
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    while std::time::Instant::now() < deadline {
        if server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .any(|request| {
                request.headers.get("authorization").is_some_and(|value| {
                    value
                        .to_str()
                        .is_ok_and(|value| value == format!("Bearer {canary}"))
                })
            })
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!("retained managed provider credential was not used by the request")
}

fn register_evidence(tmux: &TmuxServer, home: &Path, binary: &Path) -> Result<()> {
    let metadata = fs::metadata(binary)?;
    let modified = metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let hash = sha256_file(binary)?;
    fs::write(
        home.join("binary.sha256"),
        format!("{hash}  {}\n", binary.display()),
    )?;
    fs::write(
        home.join("binary.metadata"),
        format!(
            "size={}\nmtime_unix={}.{:09}\n",
            metadata.len(),
            modified.as_secs(),
            modified.subsec_nanos()
        ),
    )?;
    tmux.register_artifact("binary.sha256", home.join("binary.sha256"));
    tmux.register_artifact("binary.metadata", home.join("binary.metadata"));
    tmux.register_artifact("config.toml", home.join("config.toml"));
    tmux.register_artifact(
        "provider-eligibility.json",
        home.join("provider-eligibility.json"),
    );
    tmux.register_artifact(
        "post-save-provider-metadata.txt",
        home.join("post-save-provider-metadata.txt"),
    );
    tmux.register_artifact("codex-tui.log", home.join("log/codex-tui.log"));
    Ok(())
}

fn capture_success(
    scenario: &str,
    fixture: &Fixture,
    pane: &TmuxPane<'_>,
    canaries: &[&str],
) -> Result<()> {
    let viewport = pane.capture_viewport()?;
    let scrollback = pane.capture_scrollback_tail(/*lines*/ 4_000)?;
    let directory = PathBuf::from("target/tmux-artifacts").join(format!("pf54-{scenario}"));
    fs::create_dir_all(&directory)?;
    fs::write(directory.join("viewport.txt"), &viewport)?;
    fs::write(directory.join("scrollback.txt"), &scrollback)?;
    fs::copy(
        fixture.home.path().join("binary.sha256"),
        directory.join("binary.sha256"),
    )?;
    fs::copy(
        fixture.home.path().join("binary.metadata"),
        directory.join("binary.metadata"),
    )?;
    if fixture
        .home
        .path()
        .join("provider-eligibility.json")
        .is_file()
    {
        fs::copy(
            fixture.home.path().join("provider-eligibility.json"),
            directory.join("provider-eligibility.json"),
        )?;
    }
    for canary in canaries {
        ensure!(
            !viewport.contains(canary) && !scrollback.contains(canary),
            "secret canary appeared in terminal evidence"
        );
        ensure!(
            !tree_contains_except_custody(fixture.home.path(), canary.as_bytes())?,
            "secret canary appeared outside credential custody"
        );
        ensure!(
            !tree_contains(&directory, canary.as_bytes())?,
            "secret canary appeared in emitted success artifacts"
        );
    }
    Ok(())
}

fn tree_contains_except_custody(root: &Path, needle: &[u8]) -> Result<bool> {
    // These exact home-level files are credential custody, not transcripts.
    // Files with the same names elsewhere remain subject to the leak scan.
    let custody = [root.join("provider_auth.json"), root.join("auth.json")];
    secret_scan::tree_contains(root, needle, &|path| {
        custody.iter().any(|file| path == file)
    })
}

fn tree_contains(root: &Path, needle: &[u8]) -> Result<bool> {
    secret_scan::tree_contains(root, needle, &|_| false)
}

fn synthetic_canary(label: &str) -> String {
    format!("pf54-{label}-{}", Uuid::new_v4())
}

fn response(text: &str) -> String {
    responses::sse(vec![
        responses::ev_response_created("pf54-response"),
        responses::ev_assistant_message("pf54-message", text),
        responses::ev_completed("pf54-response"),
    ])
}

fn codex_binary(_repo_root: &Path) -> Result<PathBuf> {
    // Respect CARGO_BIN_EXE_codex so handoff tests exercise the staged package,
    // including sibling helpers, rather than a different build-tree binary.
    Ok(codex_utils_cargo_bin::cargo_bin("codex")?)
}

#[test]
fn selection_parser_accepts_exact_and_inline_replacement_rows() {
    assert_eq!(
        selected_row("› submitted prompt\nProviders\n› 1. OpenAI\n  r recover"),
        Some("› 1. OpenAI".to_string())
    );
    assert_eq!(selected_title("> 12. PF54 Managed"), Some("PF54 Managed"));
    assert!(selected_title_matches(
        "PF54 Secondary — fixture-model  Exact provider: pf54-secondary",
        "PF54 Secondary — fixture-model"
    ));
}

#[test]
fn binary_mtime_is_representable_for_evidence() {
    assert!(SystemTime::now().duration_since(UNIX_EPOCH).is_ok());
}
