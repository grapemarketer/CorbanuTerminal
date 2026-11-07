use std::fs;
use std::time::Duration;

use anyhow::Result;
use anyhow::ensure;
use tempfile::tempdir;

use crate::support::chat_ready::wait_session_configured;
use crate::support::tmux::CommandSpec;
use crate::support::tmux::SessionSpec;
use crate::support::tmux::TerminalSize;
use crate::support::tmux::TmuxKey;
use crate::support::tmux::TmuxPane;
use crate::support::tmux::TmuxServer;

const TIMEOUT: Duration = Duration::from_secs(45);

#[test]
fn tmux_security_profiles_are_observation_only_at_normal_and_narrow_widths() -> Result<()> {
    if !TmuxServer::should_run("PF-24 read-only profiles")? {
        return Ok(());
    }
    let repo = codex_utils_cargo_bin::repo_root()?;
    let binary = codex_utils_cargo_bin::cargo_bin("codex")?;
    for (level, width) in [("permissive", 120), ("moderate", 40), ("aggressive", 80)] {
        let home = tempdir()?;
        let config = format!(
            "model = \"gpt-5.6-terra\"\nmodel_provider = \"openai\"\ncli_auth_credentials_store = \"file\"\ncheck_for_update_on_startup = false\nsuppress_unstable_features_warning = true\n[security]\nversion = 1\nlevel = \"{level}\"\n[projects.{}]\ntrust_level = \"trusted\"\n[tui]\nanimations = false\n",
            serde_json::to_string(&repo.display().to_string())?
        );
        fs::write(home.path().join("config.toml"), &config)?;
        fs::write(
            home.path().join("auth.json"),
            r#"{"OPENAI_API_KEY":"security-ui-synthetic-fixture","tokens":null,"last_refresh":null}"#,
        )?;
        let tmux = TmuxServer::start(&format!("security_profiles_{level}"))?;
        tmux.register_artifact("config.toml", home.path().join("config.toml"));
        tmux.register_artifact("codex-tui.log", home.path().join("logs/codex-tui.log"));
        let session = tmux.new_session(
            SessionSpec::new(
                level,
                TerminalSize::new(width, /*rows*/ 48),
                CommandSpec::new(&binary)
                    .env("CODEX_HOME", home.path())
                    .env("CORBANU_HOME", home.path())
                    .env("OPENAI_API_KEY", "security-ui-synthetic-fixture")
                    .env("RUST_LOG", "trace")
                    .arg("-c")
                    .arg(format!("log_dir={:?}", home.path().join("logs")))
                    .arg("--no-alt-screen")
                    .arg("-C")
                    .arg(&repo),
            )
            .current_dir(&repo),
        )?;
        let pane = session.primary_pane();
        wait_session_configured(pane, TIMEOUT)?;
        command(pane, "/security")?;
        pane.wait_stable_contains("Security profiles", TIMEOUT)?;
        for _ in 0..3 {
            pane.send_key(TmuxKey::Down)?;
            pane.wait_stable_contains("Effective protection: unverified", TIMEOUT)?;
            pane.send_key(TmuxKey::Enter)?;
            pane.wait_stable_contains("Nothing changed.", TIMEOUT)?;
        }
        capture(pane, &format!("{level}-{width}-profile"))?;
        pane.send_key(TmuxKey::Escape)?;
        pane.wait_stable_until("profile view closes", TIMEOUT, |text| {
            !text.contains("Security profiles — read only")
        })?;
        command(pane, "/status")?;
        pane.wait_stable_contains("Security:", TIMEOUT)?;
        capture(pane, &format!("{level}-{width}-status"))?;
        ensure!(
            fs::read_to_string(home.path().join("config.toml"))? == config,
            "profile exploration changed configuration"
        );
        command(pane, "/security")?;
        pane.wait_stable_contains("Security profiles", TIMEOUT)?;
        pane.send_key(TmuxKey::Escape)?;
        // As above: typing before the view closes sends /exit to the view.
        pane.wait_stable_until("profile view closes", TIMEOUT, |text| {
            !text.contains("Security profiles — read only")
        })?;
        command(pane, "/exit")?;
        session.wait_for_exit(TIMEOUT)?;
    }
    Ok(())
}

fn capture(pane: &TmuxPane<'_>, name: &str) -> Result<()> {
    if let Some(directory) = std::env::var_os("CORBANU_SECURITY_UI_EVIDENCE") {
        let directory = std::path::PathBuf::from(directory);
        fs::create_dir_all(&directory)?;
        fs::write(
            directory.join(format!("{name}.txt")),
            pane.capture_viewport()?,
        )?;
    }
    Ok(())
}

#[test]
fn tmux_security_unknown_config_fails_without_permissive_fallback() -> Result<()> {
    if !TmuxServer::should_run("PF-24 unknown configuration")? {
        return Ok(());
    }
    let repo = codex_utils_cargo_bin::repo_root()?;
    let binary = codex_utils_cargo_bin::cargo_bin("codex")?;
    let home = tempdir()?;
    fs::write(
        home.path().join("config.toml"),
        "[security]\nversion = 1\nlevel = \"unknown\"\n",
    )?;
    let tmux = TmuxServer::start("security_unknown_config")?;
    // Keep the disposable error terminal visible after Corbanu exits, so the
    // typed driver can inspect startup rejection and dismiss it with Enter.
    let session = tmux.new_session(SessionSpec::new(
        "security-invalid",
        TerminalSize::new(/*columns*/ 80, /*rows*/ 24),
        CommandSpec::new("sh")
            .env("CODEX_HOME", home.path())
            .env("CORBANU_HOME", home.path())
            .env("RUST_LOG", "trace")
            .arg("-c")
            .arg("\"$@\"; printf '\\nPress Enter to close fixture\\n'; read -r reply")
            .arg("security-error-fixture")
            .arg(binary)
            .arg("--no-alt-screen")
            .arg("-C")
            .arg(repo),
    ))?;
    let pane = session.primary_pane();
    // The error embeds the config path, so where the viewport splits words
    // across rows depends on the TMPDIR length; match with whitespace removed.
    let text = pane.wait_stable_until(
        "viewport containing \"unknown variant\"",
        TIMEOUT,
        |capture| without_whitespace(capture).contains("unknownvariant"),
    )?;
    ensure!(
        !without_whitespace(&text).contains("Requested:Permissive"),
        "unknown state fell back to Permissive"
    );
    capture(pane, "unknown-config-error")?;
    pane.wait_stable_contains("Press Enter to close fixture", TIMEOUT)?;
    pane.send_key(TmuxKey::Enter)?;
    session.wait_for_exit(TIMEOUT)?;
    Ok(())
}

fn without_whitespace(text: &str) -> String {
    text.split_whitespace().collect()
}

fn command(pane: &TmuxPane<'_>, command: &str) -> Result<()> {
    pane.send_literal(command)?;
    pane.wait_stable_contains(command, TIMEOUT)?;
    pane.send_key(TmuxKey::Enter)
}
