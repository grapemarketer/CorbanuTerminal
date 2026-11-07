#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

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

use crate::support::file_hash::sha256_file;
use crate::support::secret_scan;
use crate::support::tmux::CommandSpec;
use crate::support::tmux::SessionSpec;
use crate::support::tmux::TerminalSize;
use crate::support::tmux::TmuxKey;
use crate::support::tmux::TmuxPane;
use crate::support::tmux::TmuxServer;

const READY_TIMEOUT: Duration = Duration::from_secs(45);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_corbanu_env_aliases_restart_account_read_and_legacy_daemon_recovery() -> Result<()> {
    corbanu_env_alias_restart(/*index*/ 0).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_corbanu_env_plan_alias_restart_and_account_read() -> Result<()> {
    corbanu_env_alias_restart(/*index*/ 1).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_corbanu_env_legacy_alias_restart_and_account_read() -> Result<()> {
    corbanu_env_alias_restart(/*index*/ 2).await
}

async fn corbanu_env_alias_restart(index: usize) -> Result<()> {
    use tokio::io::AsyncBufReadExt;
    use tokio::io::AsyncWriteExt;
    use tokio::io::BufReader;

    if !TmuxServer::should_run("PF-57 Corbanu environment aliases and daemon recovery")? {
        return Ok(());
    }
    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let binary = codex_binary(&repo_root)?;
    let alias = codex_model_provider_info::CORBANU_API_KEY_ENV_VARS[index];
    let home = tempdir()?;
    let server = MockServer::start().await;
    mount_corbanu_api_gateway(&server).await;
    let canary = synthetic_canary("environment-alias");
    Mock::given(method("GET"))
        .and(path("/v1/account"))
        .and(wiremock::matchers::header(
            "authorization",
            format!("Bearer {canary}"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "walletAddress": "fixture-wallet",
            "corbanuApi": {
                "balanceMicrousd": "12200000", "reservedMicrousd": "0",
                "availableMicrousd": "12200000", "balanceUsd": "12.20",
                "reservedUsd": "0", "availableUsd": "12.20"
            }
        })))
        .mount(&server)
        .await;
    write_config(home.path(), &repo_root, &server.uri())?;
    let config_path = home.path().join("config.toml");
    let config = fs::read_to_string(&config_path)?
        .replace("model = \"gpt-5.4\"", "model = \"corbanu/glm-5.3-flash\"")
        .replace(
            "model_provider = \"openai\"",
            "model_provider = \"pfterminal-plan\"",
        );
    fs::write(config_path, config)?;

    // No wallet, RPC, signing or payment: only the private socket's read-only
    // status and revocation boundary is mocked for this UI regression.
    let run_dir = home.path().join("wallet/run");
    fs::create_dir_all(&run_dir)?;
    let listener = tokio::net::UnixListener::bind(run_dir.join("walletd.sock"))?;
    let compatible = Arc::new(std::sync::atomic::AtomicBool::new(index != 0));
    let locks = Arc::new(AtomicUsize::new(0));
    let mock_daemon = tokio::spawn({
        let compatible = Arc::clone(&compatible);
        let locks = Arc::clone(&locks);
        async move {
            loop {
                let (stream, _) = listener.accept().await.expect("accept mock daemon");
                let (read, mut write) = tokio::io::split(stream);
                let mut line = String::new();
                BufReader::new(read)
                    .read_line(&mut line)
                    .await
                    .expect("read mock daemon request");
                let request: serde_json::Value =
                    serde_json::from_str(&line).expect("parse mock daemon request");
                let response = match request["type"].as_str() {
                    Some("ping") => serde_json::json!({"type": "pong"}),
                    Some("protocol_version") if compatible.load(Ordering::SeqCst) => {
                        serde_json::json!({"type": "protocol_version", "version": 1})
                    }
                    Some("lock") => {
                        locks.fetch_add(1, Ordering::SeqCst);
                        serde_json::json!({"type": "locked"})
                    }
                    Some("status") => serde_json::json!({
                            "type": "status", "wallet_exists": true, "address": "fixture-wallet",
                        "network": null, "locked": true, "busy": false, "expires_in_seconds": null
                    }),
                    _ => {
                        serde_json::json!({"type": "error", "code": "invalid_request", "message": "request was malformed"})
                    }
                };
                write
                    .write_all(format!("{response}\n").as_bytes())
                    .await
                    .expect("write mock daemon response");
            }
        }
    });
    let tmux = TmuxServer::start(&format!("pf57_env_alias_{index}"))?;
    register_evidence(&tmux, home.path(), &binary)?;
    for restart in [false, true] {
        let mut command = CommandSpec::new(&binary)
            .env("CODEX_HOME", home.path())
            .env("CORBANU_HOME", home.path())
            .env("PFTERMINAL_HOME", home.path())
            .env("RUST_LOG", "trace")
            .env("PFTERMINAL_PLAN_GATEWAY_URL", server.uri())
            .env("CORBANU_API_BASE_URL", format!("{}/v1", server.uri()));
        for name in codex_model_provider_info::CORBANU_API_KEY_ENV_VARS {
            command = command.env(name, if name == alias { canary.as_str() } else { " " });
        }
        let session = tmux.new_session(
            SessionSpec::new(
                "env-alias",
                TerminalSize::new(/*columns*/ 160, /*rows*/ 50),
                command
                    .arg("-c")
                    .arg("analytics.enabled=false")
                    .arg("-c")
                    .arg("tui.animations=false")
                    .arg("--no-alt-screen")
                    .arg("-C")
                    .arg(&repo_root),
            )
            .current_dir(&repo_root),
        )?;
        let pane = session.primary_pane();
        wait_chat_ready(pane)?;
        ensure!(
            !pane
                .capture_viewport()?
                .contains("Choose a provider account"),
            "environment-only Corbanu account was incorrectly sent to onboarding"
        );
        pane.send_literal("/wallet")?;
        pane.send_key(TmuxKey::Enter)?;
        if index == 0 && !restart {
            pane.wait_stable_contains("daemon_upgrade_required", READY_TIMEOUT)?;
            pane.send_key(TmuxKey::Escape)?;
            pane.send_literal("/wallet lock")?;
            pane.send_key(TmuxKey::Enter)?;
            pane.wait_stable_contains(
                "Wallet locked in every Corbanu Terminal process",
                READY_TIMEOUT,
            )?;
            ensure!(
                locks.load(Ordering::SeqCst) > 0,
                "TUI revocation did not reach the legacy daemon"
            );
            compatible.store(true, Ordering::SeqCst);
            select_label(pane, "Retry")?;
        }
        pane.wait_stable_contains("Corbanu API", READY_TIMEOUT)?;
        select_label(pane, "Corbanu API")?;
        pane.wait_stable_contains("$12.20 available", READY_TIMEOUT)?;
        capture_success_evidence(
            &format!("env-alias-{index}-restart-{restart}"),
            &binary,
            home.path(),
            pane,
            &server,
            &[&canary],
        )
        .await?;
        pane.send_key(TmuxKey::Escape)?;
        pane.send_key(TmuxKey::Escape)?;
        wait_chat_ready(pane)?;
        exit_tui(pane)?;
        session.wait_for_exit(READY_TIMEOUT)?;
    }
    ensure!(
        !home.path().join("provider_auth.json").exists(),
        "environment credentials must not be persisted"
    );
    mock_daemon.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_configure_many_preserves_first_default_restart_and_request() -> Result<()> {
    if !TmuxServer::should_run("PF-53 configure-many default restart request")? {
        return Ok(());
    }
    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let binary = codex_binary(&repo_root)?;
    let home = tempdir()?;
    let server = MockServer::start().await;
    let _first = responses::mount_sse_once(&server, response("pf53 first request")).await;
    let _restart = responses::mount_sse_once(&server, response("pf53 restart request")).await;
    write_config(home.path(), &repo_root, &server.uri())?;

    let tmux = TmuxServer::start("pf53_configure_many_restart_request")?;
    register_evidence(&tmux, home.path(), &binary)?;
    let session = tmux.new_session(session_spec(
        "pf53-configure-many",
        &binary,
        &repo_root,
        home.path(),
    ))?;
    let pane = session.primary_pane();
    pane.wait_stable_contains("Provider: OpenAI Codex Account", READY_TIMEOUT)?;

    let loopback_canary = synthetic_canary("loopback");
    let zai_canary = synthetic_canary("zai");
    configure_api_key(
        pane,
        "Provider: PF53 Loopback PF53_LOOPBACK_API_KEY",
        &loopback_canary,
    )?;
    pane.wait_stable_contains("Configured · active · ready", READY_TIMEOUT)?;
    configure_api_key(pane, "Provider: Z.AI API Key", &zai_canary)?;
    pane.wait_stable_contains("Configured · active · ready", READY_TIMEOUT)?;
    select_label(pane, "Done")?;
    // The provider list itself reads "... for Corbanu Terminal", so wait for the
    // chat header instead: the TUI persists the onboarding selection before it
    // builds the chat widget, so the config is final once the header is drawn.
    wait_chat_ready(pane)?;

    let config = fs::read_to_string(home.path().join("config.toml"))?;
    ensure!(
        config.contains("model_provider = \"pf53-loopback\""),
        "first successful provider was not preserved as the default:\n{config}"
    );

    submit_and_wait(pane, "PF53 request one", "pf53 first request")?;
    exit_tui(pane)?;
    session.wait_for_exit(READY_TIMEOUT)?;

    let resumed = tmux.new_session(session_spec(
        "pf53-configure-many-restart",
        &binary,
        &repo_root,
        home.path(),
    ))?;
    let resumed_pane = resumed.primary_pane();
    let restarted = resumed_pane.wait_stable_until(
        "chat header or onboarding after restart",
        READY_TIMEOUT,
        |capture| {
            capture.contains("/model to change") || capture.contains("Choose a provider account")
        },
    )?;
    ensure!(
        !restarted.contains("Choose a provider account"),
        "restart unexpectedly reopened onboarding"
    );
    submit_and_wait(
        resumed_pane,
        "PF53 request after restart",
        "pf53 restart request",
    )?;
    capture_success_evidence(
        "configure-many-restart-request",
        &binary,
        home.path(),
        resumed_pane,
        &server,
        &[loopback_canary.as_str(), zai_canary.as_str()],
    )
    .await?;
    exit_tui(resumed_pane)?;
    resumed.wait_for_exit(READY_TIMEOUT)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_deferred_corbanu_api_cancel_with_fallback_continues_to_chat() -> Result<()> {
    if !TmuxServer::should_run("PF-53 deferred Corbanu API fallback cancellation")? {
        return Ok(());
    }
    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let binary = codex_binary(&repo_root)?;
    let home = tempdir()?;
    let server = MockServer::start().await;
    write_config(home.path(), &repo_root, &server.uri())?;

    let tmux = TmuxServer::start("pf53_deferred_fallback_cancel")?;
    register_evidence(&tmux, home.path(), &binary)?;
    let session = tmux.new_session(session_spec(
        "pf53-deferred-fallback",
        &binary,
        &repo_root,
        home.path(),
    ))?;
    let pane = session.primary_pane();
    pane.wait_stable_contains("Provider: OpenAI Codex Account", READY_TIMEOUT)?;
    let fallback_canary = synthetic_canary("fallback");
    configure_api_key(pane, "Provider: Ambient API Key", &fallback_canary)?;
    pane.wait_stable_contains("Configured · active · ready", READY_TIMEOUT)?;
    select_label(pane, "Corbanu API")?;
    pane.wait_stable_contains("Corbanu API (queued)", READY_TIMEOUT)?;
    select_label(pane, "Done")?;
    pane.wait_stable_contains("Create Solana wallet", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Escape)?;
    pane.wait_stable_contains("Corbanu Terminal", READY_TIMEOUT)?;
    wait_chat_ready(pane)?;
    ensure!(
        !pane.capture_viewport()?.contains("Set up providers"),
        "fallback cancellation reopened provider setup despite a usable provider"
    );
    capture_success_evidence(
        "deferred-fallback-cancel",
        &binary,
        home.path(),
        pane,
        &server,
        &[fallback_canary.as_str()],
    )
    .await?;
    exit_tui(pane)?;
    session.wait_for_exit(READY_TIMEOUT)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_only_corbanu_api_cancel_returns_to_shared_provider_list() -> Result<()> {
    if !TmuxServer::should_run("PF-53 only-Corbanu-API cancellation return")? {
        return Ok(());
    }
    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let binary = codex_binary(&repo_root)?;
    let home = tempdir()?;
    let server = MockServer::start().await;
    write_config(home.path(), &repo_root, &server.uri())?;

    let tmux = TmuxServer::start("pf53_only_plan_cancel")?;
    register_evidence(&tmux, home.path(), &binary)?;
    let session = tmux.new_session(session_spec(
        "pf53-only-plan",
        &binary,
        &repo_root,
        home.path(),
    ))?;
    let pane = session.primary_pane();
    pane.wait_stable_contains("Provider: OpenAI Codex Account", READY_TIMEOUT)?;

    select_label(pane, "Provider: Ambient API Key")?;
    pane.wait_stable_contains("never adds it to chat.", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Escape)?;
    pane.wait_stable_contains("Provider: Ambient API Key", READY_TIMEOUT)?;
    select_label(pane, "Corbanu API")?;
    pane.wait_stable_contains("Corbanu API (queued)", READY_TIMEOUT)?;
    select_label(pane, "Done")?;
    pane.wait_stable_contains("Create Solana wallet", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Escape)?;
    pane.wait_stable_contains("Set up providers", READY_TIMEOUT)?;
    pane.wait_stable_contains("Configure more than one provider", READY_TIMEOUT)?;
    capture_success_evidence("only-plan-return", &binary, home.path(), pane, &server, &[]).await?;
    // With no usable provider, cancellation intentionally remains in the shared setup host.
    // Let the harness-owned session drop perform deterministic process cleanup.
    drop(session);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_fresh_wallet_api_handoff_preserves_existing_current_provider() -> Result<()> {
    if !TmuxServer::should_run("PF-53 fresh wallet Corbanu API handoff")? {
        return Ok(());
    }
    fresh_wallet_api_handoff(/*has_fallback*/ true).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_fresh_wallet_api_cancel_without_fallback_returns_to_setup() -> Result<()> {
    if !TmuxServer::should_run("PF-56 fresh wallet Corbanu API no-fallback cancellation")? {
        return Ok(());
    }
    fresh_wallet_api_handoff(/*has_fallback*/ false).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_fresh_wallet_api_key_success_activates_without_restart() -> Result<()> {
    if !TmuxServer::should_run("PF-56 Corbanu API key success and in-session activation")? {
        return Ok(());
    }
    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let binary = codex_binary(&repo_root)?;
    let home = tempdir()?;
    let server = MockServer::start().await;
    mount_corbanu_api_gateway(&server).await;
    write_config(home.path(), &repo_root, &server.uri())?;

    let tmux = TmuxServer::start("pf56_fresh_wallet_api_key_success")?;
    register_evidence(&tmux, home.path(), &binary)?;
    let session = tmux.new_session(session_spec_with_gateway(
        "fresh-wallet-api-key-success",
        &binary,
        &repo_root,
        home.path(),
        &server.uri(),
    ))?;
    let pane = session.primary_pane();
    pane.wait_stable_contains("Provider: OpenAI Codex Account", READY_TIMEOUT)?;
    select_label(pane, "Corbanu API")?;
    select_label(pane, "Done")?;

    let passphrase = synthetic_canary("wallet-key-success-passphrase");
    pane.wait_stable_contains("Create Solana wallet", READY_TIMEOUT)?;
    pane.send_secret_literal(&passphrase)?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Confirm the value", READY_TIMEOUT)?;
    pane.send_secret_literal(&passphrase)?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Wallet recovery — secure view", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Unlock wallet", READY_TIMEOUT)?;
    pane.send_secret_literal(&passphrase)?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Create API key", READY_TIMEOUT)?;
    let before_key_creation = pane.capture_scrollback_tail(/*lines*/ 4_000)?;
    ensure!(
        !before_key_creation.contains("inactive or unavailable"),
        "deferred Corbanu setup attempted model selection before key creation:\n{before_key_creation}"
    );
    select_label(pane, "Create API key")?;
    pane.wait_stable_contains("Corbanu API cbn_live_pf53 — shown once", READY_TIMEOUT)?;
    pane.wait_stable_contains("cbn_live_pf53_success_key", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Enter)?;
    wait_chat_ready(pane)?;

    let config = wait_for_config_values(
        home.path(),
        &[
            "model_provider = \"pfterminal-plan\"",
            "model = \"corbanu/glm-5.3-flash\"",
        ],
    )
    .await?;
    let scrollback = pane.capture_scrollback_tail(/*lines*/ 4_000)?;
    ensure!(
        !scrollback.contains("inactive or unavailable"),
        "newly stored Corbanu API key was not activated in the current process:\n{config}\nredacted terminal scrollback:\n{scrollback}"
    );
    ensure!(
        scrollback.contains("Model changed to corbanu/glm-5.3-flash via Corbanu API"),
        "successful Corbanu API activation did not use the product label:\n{scrollback}"
    );
    let provider_auth = codex_login::provider_api_key_from_auth_storage(
        home.path(),
        codex_model_provider_info::PFTERMINAL_PLAN_API_KEY_ENV_VAR,
        codex_config::types::AuthCredentialsStoreMode::File,
        codex_config::types::AuthKeyringBackendKind::default(),
    )?;
    ensure!(
        provider_auth.as_deref() == Some("cbn_live_pf53_success_key"),
        "one-time Corbanu API key was not stored in provider custody"
    );

    capture_success_evidence(
        "fresh-wallet-api-key-success",
        &binary,
        home.path(),
        pane,
        &server,
        &[passphrase.as_str()],
    )
    .await?;
    exit_tui(pane)?;
    session.wait_for_exit(READY_TIMEOUT)?;
    Ok(())
}

async fn fresh_wallet_api_handoff(has_fallback: bool) -> Result<()> {
    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let binary = codex_binary(&repo_root)?;
    let home = tempdir()?;
    let server = MockServer::start().await;
    mount_corbanu_api_gateway(&server).await;
    write_config(home.path(), &repo_root, &server.uri())?;

    let scenario = if has_fallback {
        "fresh-wallet-api-handoff"
    } else {
        "fresh-wallet-api-no-fallback"
    };
    let tmux = TmuxServer::start(&format!("pf56_{scenario}"))?;
    register_evidence(&tmux, home.path(), &binary)?;
    let session = tmux.new_session(session_spec_with_gateway(
        scenario,
        &binary,
        &repo_root,
        home.path(),
        &server.uri(),
    ))?;
    let pane = session.primary_pane();
    pane.wait_stable_contains("Provider: OpenAI Codex Account", READY_TIMEOUT)?;
    let ambient_canary = has_fallback.then(|| synthetic_canary("existing-current"));
    if let Some(canary) = ambient_canary.as_deref() {
        configure_api_key(pane, "Provider: Ambient API Key", canary)?;
    }
    select_label(pane, "Corbanu API")?;
    select_label(pane, "Done")?;

    let passphrase = synthetic_canary("wallet-passphrase");
    pane.wait_stable_contains("Create Solana wallet", READY_TIMEOUT)?;
    pane.send_secret_literal(&passphrase)?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Confirm the value", READY_TIMEOUT)?;
    pane.send_secret_literal(&passphrase)?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Wallet recovery — secure view", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Unlock wallet", READY_TIMEOUT)?;
    pane.send_secret_literal(&passphrase)?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Corbanu API", READY_TIMEOUT)?;
    pane.wait_stable_contains("Top up balance", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Escape)?;

    let config = fs::read_to_string(home.path().join("config.toml"))?;
    if has_fallback {
        wait_chat_ready(pane)?;
        ensure!(
            config.contains("model_provider = \"ambient\""),
            "cancelled deferred Corbanu API setup overrode the usable provider:\n{config}"
        );
    } else {
        pane.wait_stable_contains("Set up providers", READY_TIMEOUT)?;
        ensure!(
            !config.contains("model_provider = \"pfterminal-plan\""),
            "cancelled Corbanu API setup selected a provider without a key:\n{config}"
        );
    }
    let mut canaries = vec![passphrase.as_str()];
    if let Some(canary) = ambient_canary.as_deref() {
        canaries.insert(0, canary);
    }
    capture_success_evidence(scenario, &binary, home.path(), pane, &server, &canaries).await?;
    if has_fallback {
        exit_tui(pane)?;
        session.wait_for_exit(READY_TIMEOUT)?;
    } else {
        drop(session);
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_locked_wallet_api_unlocks_and_cancels() -> Result<()> {
    if !TmuxServer::should_run("PF-53 locked wallet Corbanu API handoff")? {
        return Ok(());
    }
    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let binary = codex_binary(&repo_root)?;
    let home = tempdir()?;
    let server = MockServer::start().await;
    mount_corbanu_api_gateway(&server).await;
    write_config(home.path(), &repo_root, &server.uri())?;
    let passphrase = synthetic_canary("locked-wallet-passphrase");
    codex_wallet::Wallet::new(home.path().to_path_buf())
        .create(&passphrase, codex_wallet::Network::Mainnet)
        .context("create locked wallet fixture")?;

    let tmux = TmuxServer::start("pf53_locked_wallet_failure_retry")?;
    register_evidence(&tmux, home.path(), &binary)?;
    let session = tmux.new_session(session_spec_with_gateway(
        "pf53-locked-wallet-retry",
        &binary,
        &repo_root,
        home.path(),
        &server.uri(),
    ))?;
    let pane = session.primary_pane();
    pane.wait_stable_contains("Provider: OpenAI Codex Account", READY_TIMEOUT)?;
    let ambient_canary = synthetic_canary("locked-fallback");
    configure_api_key(pane, "Provider: Ambient API Key", &ambient_canary)?;
    select_label(pane, "Corbanu API")?;
    select_label(pane, "Done")?;
    pane.wait_stable_contains("Unlock wallet", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Escape)?;
    wait_chat_ready(pane)?;
    pane.send_literal("/wallet")?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Corbanu API", READY_TIMEOUT)?;
    select_label(pane, "Corbanu API")?;
    pane.wait_stable_contains("Manage API keys", READY_TIMEOUT)?;
    select_label(pane, "Manage API keys")?;
    pane.wait_stable_contains("Unlock wallet", READY_TIMEOUT)?;
    pane.send_secret_literal(&passphrase)?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Corbanu API", READY_TIMEOUT)?;
    pane.wait_stable_contains("Top up balance", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Escape)?;
    pane.wait_stable_contains("Wallet", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Escape)?;
    wait_chat_ready(pane)?;
    capture_success_evidence(
        "locked-wallet-failure-retry",
        &binary,
        home.path(),
        pane,
        &server,
        &[ambient_canary.as_str(), passphrase.as_str()],
    )
    .await?;
    exit_tui(pane)?;
    session.wait_for_exit(READY_TIMEOUT)?;
    Ok(())
}

/// #461: onboarding text fields take every printable key. Values typed key by key (as a
/// non-bracketed paste or typing arrives) that contain `q`, even as the first character, must
/// not quit the TUI and spill the rest of a secret onto the shell.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_onboarding_text_fields_type_q_without_quitting() -> Result<()> {
    if !TmuxServer::should_run("PF-84 #461 onboarding text fields type q")? {
        return Ok(());
    }
    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let binary = codex_binary(&repo_root)?;
    let home = tempdir()?;
    let server = MockServer::start().await;
    write_config(home.path(), &repo_root, &server.uri())?;
    let config_path = home.path().join("config.toml");
    let mut config = fs::read_to_string(&config_path)?;
    config.push_str("\n[features]\nnamed_accounts = true\n");
    fs::write(&config_path, config)?;

    let tmux = TmuxServer::start("pf84_461_onboarding_q")?;
    register_evidence(&tmux, home.path(), &binary)?;
    let session = tmux.new_session(session_spec(
        "pf84-461-onboarding-q",
        &binary,
        &repo_root,
        home.path(),
    ))?;
    let pane = session.primary_pane();
    pane.wait_stable_contains("Provider: OpenAI Codex Account", READY_TIMEOUT)?;

    // The default-key field, with `q` as its first character.
    let default_key = format!("q{}", synthetic_canary("q-default"));
    configure_api_key(pane, "Provider: Z.AI API Key", &default_key)?;
    ensure!(session.is_running(), "typing the default key quit the TUI");

    select_label(pane, "Provider: Z.AI")?;
    // The chooser's "> Z.AI" header defeats `select_label`; the action is one row down.
    pane.wait_stable_contains("> Use configured credentials", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Down)?;
    pane.wait_stable_contains("> Add another account", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Name it:", READY_TIMEOUT)?;
    pane.send_literal("qa-quota")?;
    pane.wait_stable_contains("qa-quota", READY_TIMEOUT)?;
    ensure!(session.is_running(), "typing the account name quit the TUI");
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("never shown.", READY_TIMEOUT)?;

    let account_key = format!("q{}-qqq", synthetic_canary("q-account"));
    pane.send_secret_literal(&account_key)?;
    let masked = account_key.chars().count();
    pane.wait_stable_until("the whole masked value", READY_TIMEOUT, |capture| {
        capture.matches('•').count() >= masked
    })?;
    ensure!(session.is_running(), "typing the account key quit the TUI");
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("--account qa-quota", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Choose a provider account", READY_TIMEOUT)?;
    select_label(pane, "Done")?;
    wait_chat_ready(pane)?;

    let scrollback = pane.capture_scrollback_tail(/*lines*/ 4_000)?;
    for canary in [default_key.as_str(), account_key.as_str()] {
        ensure!(
            !scrollback.contains(canary.strip_prefix('q').unwrap_or(canary)),
            "a typed secret appeared in the terminal"
        );
        ensure!(
            !tree_contains_except_custody(home.path(), canary.as_bytes())?,
            "a typed secret appeared in plain text under the isolated home"
        );
    }
    exit_tui(pane)?;
    session.wait_for_exit(READY_TIMEOUT)?;
    Ok(())
}

/// #462: when config already names the provider, onboarding opens on that provider's key
/// entry. Saving the key must continue to chat instead of looping back to key entry.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_preset_provider_key_entry_continues_to_chat() -> Result<()> {
    if !TmuxServer::should_run("PF-84 #462 preset provider onboarding")? {
        return Ok(());
    }
    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let binary = codex_binary(&repo_root)?;
    let home = tempdir()?;
    // Nothing listens here, so the provider never looks reachable before its setup completes;
    // a reachable endpoint let a status refresh hide the loop. No request is made.
    write_config(home.path(), &repo_root, "http://127.0.0.1:9")?;
    let config_path = home.path().join("config.toml");
    let config = fs::read_to_string(&config_path)?
        .replace("model = \"gpt-5.4\"", "model = \"pf84-preset\"")
        // A built-in API-key provider (Kimi, Z.AI) named in config forces the API path;
        // the loopback provider stands in for it without a real endpoint.
        .replace(
            "model_provider = \"openai\"",
            "model_provider = \"pf53-loopback\"\nforced_login_method = \"api\"",
        );
    fs::write(&config_path, config)?;

    let tmux = TmuxServer::start("pf84_462_preset_provider")?;
    register_evidence(&tmux, home.path(), &binary)?;
    let session = tmux.new_session(session_spec(
        "pf84-462-preset-provider",
        &binary,
        &repo_root,
        home.path(),
    ))?;
    let pane = session.primary_pane();
    pane.wait_stable_contains("Use your PF53 Loopback API key", READY_TIMEOUT)?;
    // Type at a person's pace: background provider discovery finishes first, as it does for a
    // person. When it finished after the save instead, its refresh hid the loop.
    std::thread::sleep(Duration::from_secs(3));
    let key = synthetic_canary("preset");
    pane.send_secret_literal(&key)?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("API key configured", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Enter)?;
    let after_save = pane.wait_stable_until(
        "chat, or the key-entry loop, after the key is saved",
        READY_TIMEOUT,
        |capture| {
            capture.contains("/model to change")
                || capture.contains("Provide your PF53 Loopback API key")
        },
    )?;
    ensure!(
        !after_save.contains("Provide your PF53 Loopback API key"),
        "onboarding looped back to key entry:\n{after_save}"
    );
    wait_chat_ready(pane)?;
    ensure!(
        !tree_contains_except_custody(home.path(), key.as_bytes())?,
        "the saved key appeared in plain text under the isolated home"
    );
    exit_tui(pane)?;
    session.wait_for_exit(READY_TIMEOUT)?;
    Ok(())
}

fn configure_api_key(pane: &TmuxPane<'_>, label: &str, secret: &str) -> Result<()> {
    select_label(pane, label)?;
    pane.wait_stable_contains("never adds it to chat.", READY_TIMEOUT)?;
    pane.send_secret_literal(secret)?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("API key configured", READY_TIMEOUT)?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("Choose a provider account", READY_TIMEOUT)?;
    Ok(())
}

fn select_label(pane: &TmuxPane<'_>, label: &str) -> Result<()> {
    for _ in 0..32 {
        let capture = pane.capture_viewport()?;
        let selected = selected_row(&capture);
        if selected
            .as_deref()
            .and_then(selected_title)
            .is_some_and(|title| selected_title_matches(title, label))
        {
            pane.send_key(TmuxKey::Enter)?;
            return Ok(());
        }
        pane.send_key(TmuxKey::Down)?;
        pane.wait_stable_until(
            "provider selection to redraw after Down",
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

#[test]
fn selected_title_strips_only_the_cursor_and_numeric_prefix() {
    assert_eq!(selected_title("  > 12. Corbanu API  "), Some("Corbanu API"));
    assert_eq!(
        selected_title("  › 1. Starter — 1.00 USDC  1,000 tokens/week  "),
        Some("Starter — 1.00 USDC  1,000 tokens/week")
    );
    assert_eq!(
        selected_title("> 3. Provider: Corbanu API Key"),
        Some("Provider: Corbanu API Key")
    );
    assert_eq!(
        selected_title("› Cancel  Return to provider setup"),
        Some("Cancel  Return to provider setup")
    );
    assert_eq!(
        selected_title("> Pay 1.00 USDC  Purchase Starter"),
        Some("Pay 1.00 USDC  Purchase Starter")
    );
    assert_eq!(selected_title(">_ Corbanu Terminal"), None);
    assert_eq!(selected_title("  12. Corbanu API"), None);
    assert_eq!(selected_title("> Corbanu API"), Some("Corbanu API"));
}

#[test]
fn selected_row_accepts_provider_and_plan_cursors_but_rejects_header_marker() {
    assert_eq!(
        selected_row(">_ Corbanu Terminal\n> 2. Provider: Ambient API Key"),
        Some("> 2. Provider: Ambient API Key".to_string())
    );
    assert_eq!(
        selected_row(">_ Corbanu Terminal\n› 1. Starter — 1.00 USDC  1,000 tokens/week"),
        Some("› 1. Starter — 1.00 USDC  1,000 tokens/week".to_string())
    );
    assert_eq!(selected_row(">_ Corbanu Terminal"), None);
}

#[test]
fn selected_title_match_allows_only_exact_or_inline_column_metadata() {
    assert!(selected_title_matches("Corbanu API", "Corbanu API"));
    assert!(selected_title_matches(
        "Starter — 1.00 USDC  1,000 tokens/week",
        "Starter — 1.00 USDC",
    ));
    assert!(!selected_title_matches(
        "Provider: Corbanu API Key",
        "Corbanu API",
    ));
    assert!(!selected_title_matches(
        "Starter — 1.00 USDC extra",
        "Starter — 1.00 USDC",
    ));
}

#[cfg(unix)]
#[test]
fn canary_tree_scan_skips_unix_sockets_and_scans_regular_files() -> Result<()> {
    use std::os::unix::net::UnixListener;

    let directory = tempdir()?;
    let canary = synthetic_canary("walker");
    let _socket = UnixListener::bind(directory.path().join("wallet-daemon.sock"))?;

    assert!(!tree_contains_except_custody(
        directory.path(),
        canary.as_bytes()
    )?);

    fs::write(directory.path().join("observable.log"), canary.as_bytes())?;
    assert!(tree_contains_except_custody(
        directory.path(),
        canary.as_bytes()
    )?);
    Ok(())
}

fn submit_and_wait(pane: &TmuxPane<'_>, prompt: &str, response: &str) -> Result<()> {
    wait_chat_ready(pane)?;
    pane.send_literal(prompt)?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains(response, READY_TIMEOUT)?;
    Ok(())
}

fn wait_chat_ready(pane: &TmuxPane<'_>) -> Result<()> {
    pane.wait_stable_until(
        "chat composer without a selection overlay",
        READY_TIMEOUT,
        |capture| {
            capture.contains("/model to change")
                && !capture.contains("Press enter to confirm or esc to go back")
                && !capture.contains("Vault credential — secure view")
                && !capture.contains("Press Enter or Esc to clear")
        },
    )?;
    Ok(())
}

fn exit_tui(pane: &TmuxPane<'_>) -> Result<()> {
    pane.send_literal("/exit")?;
    pane.send_key(TmuxKey::Enter)?;
    Ok(())
}

fn session_spec(name: &str, binary: &Path, repo_root: &Path, home: &Path) -> SessionSpec {
    SessionSpec::new(
        name,
        TerminalSize::new(/*columns*/ 140, /*rows*/ 44),
        CommandSpec::new(binary)
            .env("CODEX_HOME", home)
            .env("CORBANU_HOME", home)
            .env("PFTERMINAL_HOME", home)
            .env(
                "RUST_LOG",
                "warn,codex_tui=debug,codex_login=debug,codex_secrets=debug",
            )
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

fn session_spec_with_gateway(
    name: &str,
    binary: &Path,
    repo_root: &Path,
    home: &Path,
    gateway: &str,
) -> SessionSpec {
    SessionSpec::new(
        name,
        TerminalSize::new(/*columns*/ 140, /*rows*/ 44),
        CommandSpec::new(binary)
            .env("CODEX_HOME", home)
            .env("CORBANU_HOME", home)
            .env("PFTERMINAL_HOME", home)
            .env("PFTERMINAL_PLAN_GATEWAY_URL", gateway)
            .env(
                "RUST_LOG",
                "warn,codex_tui=debug,codex_login=debug,codex_secrets=debug",
            )
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

#[allow(dead_code)]
async fn mount_plan_gateway(server: &MockServer, plan_key: &str, flaky_plans: bool) {
    let plan_requests = Arc::new(AtomicUsize::new(0));
    Mock::given(method("GET"))
        .and(path("/v1/plans"))
        .respond_with({
            let plan_requests = Arc::clone(&plan_requests);
            let uri = server.uri();
            move |_request: &wiremock::Request| {
                let attempt = plan_requests.fetch_add(1, Ordering::SeqCst);
                if flaky_plans && attempt < 1 {
                    return ResponseTemplate::new(503);
                }
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "plans": [{
                        "id": "starter",
                        "priceUsdc": "1.00",
                        "amountAtomic": "1000000",
                        "weeklyTokenLimit": 1000,
                        "monthlyTokenLimit": 4000
                    }],
                    "payment": {
                        "network": "solana:5eykt4UsFv8P8NJdTREpY1vzqKqZKvdp",
                        "asset": "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v",
                        "payTo": "G3s13pAE8f72jPPWSvwEfLr6Gg1WRh6Nv7i98HNMoVcd",
                        "rpcUrl": format!("{uri}/rpc")
                    }
                }))
            }
        })
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/rpc"))
        .respond_with(|request: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap_or_default();
            match body.get("method").and_then(serde_json::Value::as_str) {
                Some("getBalance") => ResponseTemplate::new(200).set_body_json(
                    serde_json::json!({"jsonrpc":"2.0","id":1,"result":{"value":1_000_000_000u64}}),
                ),
                Some("getTokenAccountsByOwner") => {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc":"2.0","id":1,
                        "result":{"value":[{
                            "account":{"data":{"parsed":{"info":{
                                "tokenAmount":{"amount":"5000000"}
                            }}}}
                        }]}
                    }))
                }
                Some("getLatestBlockhash") => {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "jsonrpc":"2.0","id":1,
                        "result":{"value":{"blockhash":"11111111111111111111111111111111"}}
                    }))
                }
                _ => ResponseTemplate::new(400),
            }
        })
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/subscriptions/starter"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"transaction":"pf53-loopback-settlement"})),
        )
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/keys/challenge"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"challenge":"pf53-loopback-ownership"})),
        )
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/keys/wallet"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id":"pf53-loopback-key-id",
            "key":plan_key,
            "displayPrefix":"pf53..."
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/account"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "walletAddress":"pf53-loopback-wallet",
            "period":{
                "transaction":"pf53-loopback-settlement",
                "planId":"starter",
                "startsAt":"2026-09-01T00:00:00Z",
                "endsAt":"2026-10-01T00:00:00Z",
                "monthlyLimitTokens":4000,
                "monthlyUsedTokens":0,
                "monthlyReservedTokens":0
            },
            "weekly":{
                "endsAt":"2026-09-08T00:00:00Z",
                "limitTokens":1000,
                "usedTokens":0,
                "reservedTokens":0
            },
            "monthlyRemainingTokens":4000,
            "weeklyRemainingTokens":1000,
            "queuedPeriods":[]
        })))
        .mount(server)
        .await;
}

#[allow(dead_code)]
async fn redacted_request_transcript(server: &MockServer) -> String {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(index, request)| {
            let rpc_method = serde_json::from_slice::<serde_json::Value>(&request.body)
                .ok()
                .and_then(|body| body.get("method").cloned())
                .and_then(|method| method.as_str().map(str::to_owned));
            match rpc_method {
                Some(rpc_method) => format!(
                    "{index}: {} {} rpc.method={rpc_method}",
                    request.method,
                    request.url.path()
                ),
                None => format!("{index}: {} {}", request.method, request.url.path()),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[allow(dead_code)]
async fn wait_for_request_path(server: &MockServer, expected_path: &str) -> Result<()> {
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    while std::time::Instant::now() < deadline {
        let requests = server.received_requests().await.unwrap_or_default();
        if requests
            .iter()
            .any(|request| request.url.path() == expected_path)
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let transcript = redacted_request_transcript(server).await;
    anyhow::bail!(
        "timed out waiting for loopback request path {expected_path}\nredacted loopback request transcript:\n{transcript}"
    )
}

#[allow(dead_code)]
async fn wait_for_rpc_method(server: &MockServer, method_name: &str) -> Result<()> {
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    while std::time::Instant::now() < deadline {
        let requests = server.received_requests().await.unwrap_or_default();
        if requests.iter().any(|request| {
            serde_json::from_slice::<serde_json::Value>(&request.body)
                .ok()
                .and_then(|body| body.get("method").cloned())
                .and_then(|method| method.as_str().map(str::to_owned))
                .as_deref()
                == Some(method_name)
        }) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    anyhow::bail!("timed out waiting for loopback RPC method {method_name}")
}

fn write_config(home: &Path, repo_root: &Path, server_uri: &str) -> Result<()> {
    fs::create_dir_all(home.join("log"))?;
    fs::write(
        home.join("config.toml"),
        format!(
            r#"model = "gpt-5.4"
model_provider = "openai"
suppress_unstable_features_warning = true
log_dir = "{}"

[model_providers.pf53-loopback]
name = "PF53 Loopback"
base_url = "{server_uri}/v1"
env_key = "PF53_LOOPBACK_API_KEY"
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
    .context("write PF-53 tmux config")
}

fn register_evidence(tmux: &TmuxServer, home: &Path, binary: &Path) -> Result<()> {
    let hash = sha256_file(binary)?;
    fs::write(
        home.join("binary.sha256"),
        format!("{hash}  {}\n", binary.display()),
    )?;
    tmux.register_artifact("binary.sha256", home.join("binary.sha256"));
    tmux.register_artifact("config.toml", home.join("config.toml"));
    tmux.register_artifact("codex-tui.log", home.join("log/codex-tui.log"));
    Ok(())
}

async fn mount_corbanu_api_gateway(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "id": "corbanu/glm-5.3-flash",
                "displayName": "GLM 5.3 Flash",
                "recommended": true,
                "balanceRate": "standard",
                "privacy": "corbanu-controlled",
                "pricing": {
                    "inputUsd": "0.10",
                    "outputUsd": "0.30",
                    "cacheReadUsd": "0.01",
                    "cacheWriteUsd": "0.10",
                    "version": "pf53-test"
                }
            }]
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/wallet/challenge"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"challenge":"pf53-api-account"})),
        )
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/wallet/execute"))
        .respond_with(|request: &wiremock::Request| {
            let operation = serde_json::from_slice::<serde_json::Value>(&request.body)
                .ok()
                .and_then(|body| body.pointer("/operation/kind").cloned())
                .and_then(|kind| kind.as_str().map(str::to_owned));
            if operation.as_deref() == Some("create_key") {
                return ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": "pf53-success-key-id",
                    "key": "cbn_live_pf53_success_key",
                    "displayPrefix": "cbn_live_pf53"
                }));
            }
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "balance": {
                    "balanceMicrousd": "0",
                    "reservedMicrousd": "0",
                    "availableMicrousd": "0",
                    "balanceUsd": "0",
                    "reservedUsd": "0",
                    "availableUsd": "0"
                },
                "keys": [],
                "models": [{
                    "id": "corbanu/glm-5.3-flash",
                    "displayName": "GLM 5.3 Flash",
                    "recommended": true,
                    "balanceRate": "standard",
                    "privacy": "corbanu-controlled",
                    "pricing": {
                        "inputUsd": "0.10",
                        "outputUsd": "0.30",
                        "cacheReadUsd": "0.01",
                        "cacheWriteUsd": "0.10",
                        "version": "pf53-test"
                    }
                }]
            }))
        })
        .mount(server)
        .await;
}

async fn wait_for_config_values(home: &Path, expected: &[&str]) -> Result<String> {
    let path = home.join("config.toml");
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    loop {
        let config = fs::read_to_string(&path)?;
        if expected.iter().all(|value| config.contains(value)) {
            return Ok(config);
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!(
                "timed out waiting for config values {expected:?}; final config:\n{config}"
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn capture_success_evidence(
    scenario: &str,
    binary: &Path,
    home: &Path,
    pane: &TmuxPane<'_>,
    server: &MockServer,
    canaries: &[&str],
) -> Result<()> {
    let viewport = pane.capture_viewport()?;
    let scrollback = pane.capture_scrollback_tail(/*lines*/ 4_000)?;
    let directory = PathBuf::from("target/tmux-artifacts").join(format!("pf53-{scenario}"));
    fs::create_dir_all(&directory)?;
    fs::write(directory.join("viewport.txt"), &viewport)?;
    fs::write(directory.join("scrollback.txt"), &scrollback)?;
    let binary_hash = sha256_file(binary)?;
    fs::write(
        directory.join("binary.sha256"),
        format!("{binary_hash}  {}\n", binary.display()),
    )?;

    let requests = server.received_requests().await.unwrap_or_default();
    let request_bodies = requests
        .iter()
        .flat_map(|request| request.body.iter().copied())
        .collect::<Vec<_>>();
    for canary in canaries {
        let needle = canary.as_bytes();
        ensure!(
            !viewport.contains(canary) && !scrollback.contains(canary),
            "secret canary appeared in terminal evidence"
        );
        ensure!(
            !request_bodies
                .windows(needle.len())
                .any(|window| window == needle),
            "secret canary appeared in a loopback request body"
        );
        ensure!(
            !tree_contains_except_custody(home, needle)?,
            "secret canary appeared in observable isolated-home files"
        );
        ensure!(
            !tree_contains_except_custody(&directory, needle)?,
            "secret canary appeared in emitted success artifacts"
        );
    }
    Ok(())
}

fn synthetic_canary(label: &str) -> String {
    format!("pf53-{label}-{}", Uuid::new_v4())
}

/// Custody files keep their secret by design; every other file must not contain it.
fn tree_contains_except_custody(root: &Path, needle: &[u8]) -> Result<bool> {
    secret_scan::tree_contains(root, needle, &|path| {
        path.file_name().and_then(|name| name.to_str()) == Some("provider_auth.json")
    })
}

fn response(text: &str) -> String {
    responses::sse(vec![
        responses::ev_response_created("pf53-response"),
        responses::ev_assistant_message("pf53-message", text),
        responses::ev_completed("pf53-response"),
    ])
}

fn codex_binary(repo_root: &Path) -> Result<PathBuf> {
    // These scenarios qualify the branded product. Prefer its freshly built binary instead of a
    // potentially stale upstream `codex` sibling left in the shared target directory.
    for name in ["corbanu", "pfterminal", "codex"] {
        if let Ok(binary) = codex_utils_cargo_bin::cargo_bin(name) {
            return Ok(binary);
        }
    }
    for name in ["corbanu", "pfterminal", "codex"] {
        let binary = repo_root.join("codex-rs/target/debug").join(name);
        if binary.is_file() {
            return Ok(binary);
        }
    }
    anyhow::bail!("build the Corbanu CLI binary before running PF-53 tmux tests")
}
