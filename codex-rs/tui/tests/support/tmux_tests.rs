use std::panic::catch_unwind;
use std::time::Duration;

use anyhow::Result;

use super::*;

#[test]
fn interrupted_test_process_does_not_orphan_tmux() -> Result<()> {
    const CHILD_ENV: &str = "CORBANU_TMUX_WATCHDOG_CHILD";
    if let Some(report) = std::env::var_os(CHILD_ENV) {
        let server = TmuxServer::start("watchdog-child")?;
        let session = server.new_session(SessionSpec::new(
            "watchdog",
            TerminalSize::new(/*columns*/ 40, /*rows*/ 8),
            command_for_shell("trap '' HUP TERM; printf 'ready'; while :; do sleep 1; done"),
        ))?;
        session
            .primary_pane()
            .wait_stable_contains("ready", Duration::from_secs(3))?;
        std::fs::write(
            report,
            format!(
                "{}\n{}",
                server.socket_root().display(),
                session.primary_pane().pid
            ),
        )?;
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    if !TmuxServer::should_run("interrupted test watchdog")? {
        return Ok(());
    }
    let dir = tempfile::tempdir()?;
    let report = dir.path().join("child");
    let mut child = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "support::tmux::tests::interrupted_test_process_does_not_orphan_tmux",
        ])
        .env(CHILD_ENV, &report)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(8);
    while !report.exists() && Instant::now() < deadline {
        sleep(Duration::from_millis(50));
    }
    child.kill()?;
    child.wait()?;
    let report = std::fs::read_to_string(report)?;
    let pid: u32 = report.lines().nth(1).context("pane pid")?.parse()?;
    wait_for_process_exit(pid, Duration::from_secs(5));
    assert!(
        !process_is_running(pid),
        "watchdog must terminate a pane that ignores HUP and TERM"
    );
    Ok(())
}

fn server_or_skip(scenario: &str) -> Result<Option<TmuxServer>> {
    if !TmuxServer::should_run(scenario)? {
        return Ok(None);
    }
    Ok(Some(TmuxServer::start(scenario)?))
}

#[test]
fn attachment_hint_targets_only_owned_session_and_preserves_cleanup() -> Result<()> {
    let Some(server) = server_or_skip("attachment")? else {
        return Ok(());
    };
    let socket_root = server.socket_root();
    let session = server.new_session(SessionSpec::new(
        "owned",
        TerminalSize::new(/*columns*/ 40, /*rows*/ 8),
        command_for_shell("printf ready; sleep 30"),
    ))?;
    let command = session.attachment_command();
    pretty_assertions::assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        vec![
            OsStr::new("-L"),
            OsStr::new(&server.socket_name),
            OsStr::new("attach-session"),
            OsStr::new("-t"),
            OsStr::new(&session.name)
        ]
    );
    assert!(
        command
            .get_envs()
            .any(|(key, value)| key == "TMUX_TMPDIR" && value == Some(socket_root.as_os_str()))
    );
    assert!(session.is_running());
    drop(session);
    drop(server);
    assert!(!socket_root.exists());
    Ok(())
}

#[test]
fn servers_are_isolated_and_cleanup_their_private_sessions() -> Result<()> {
    let Some(first) = server_or_skip("servers_are_isolated")? else {
        return Ok(());
    };
    let Some(second) = server_or_skip("servers_are_isolated")? else {
        return Ok(());
    };
    let first_socket_root = first.socket_root();
    let second_socket_root = second.socket_root();
    let first_session = first.new_session(SessionSpec::new(
        "isolation",
        TerminalSize::new(/*columns*/ 40, /*rows*/ 8),
        command_for_shell("printf 'first\n'; sleep 30"),
    ))?;
    let second_session = second.new_session(SessionSpec::new(
        "isolation",
        TerminalSize::new(/*columns*/ 40, /*rows*/ 8),
        command_for_shell("printf 'second\n'; sleep 30"),
    ))?;

    assert!(first.has_session(&first_session.name));
    assert!(!first.has_session(&second_session.name));
    assert!(second.has_session(&second_session.name));
    assert!(!second.has_session(&first_session.name));

    drop(first_session);
    drop(second_session);
    drop(first);
    drop(second);
    assert!(!first_socket_root.exists());
    assert!(!second_socket_root.exists());
    Ok(())
}

#[test]
fn literal_text_is_distinct_from_named_enter_key() -> Result<()> {
    let Some(server) = server_or_skip("literal_text_is_distinct")? else {
        return Ok(());
    };
    let session = server.new_session(SessionSpec::new(
        "input",
        TerminalSize::new(/*columns*/ 40, /*rows*/ 8),
        command_for_shell("IFS= read -r line; printf 'LINE:%s\n' \"$line\"; sleep 30"),
    ))?;
    let pane = session.primary_pane();

    pane.send_literal("Enter")?;
    let before_key = pane.capture_viewport()?;
    assert!(!before_key.contains("LINE:Enter"));
    pane.send_key(TmuxKey::Enter)?;
    let after_key = pane.wait_stable_contains("LINE:Enter", Duration::from_secs(/*secs*/ 3))?;
    assert!(after_key.contains("LINE:Enter"));
    Ok(())
}

/// The od lines of a capture, whitespace-normalized: BSD od pads its columns
/// ("2f  78").
fn od_reads(capture: &str) -> Vec<String> {
    capture
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty() && line != "ready")
        .collect()
}

#[test]
fn input_after_escape_is_not_read_with_the_escape() -> Result<()> {
    let Some(server) = server_or_skip("escape_then_text")? else {
        return Ok(());
    };
    // Polls the terminal every 150 ms without blocking (`min 0 time 0`) and
    // prints the bytes of each nonempty read(2) on its own line, like a busy
    // TUI. Without a gap, Esc and the text that follows it land in one read,
    // which crossterm parses as Alt+`/`. The first read waits a second, longer
    // than the minimum gap, like a reader descheduled on a loaded runner: the
    // text must still wait until the Esc has been read.
    let session = server.new_session(SessionSpec::new(
        "escape",
        TerminalSize::new(/*columns*/ 40, /*rows*/ 8),
        command_for_shell(
            "stty raw -echo min 0 time 0; printf 'ready\r\n'; sleep 1; \
             while :; do sleep 0.15; dd bs=64 count=1 2>/dev/null | od -An -tx1; printf '\r'; done",
        ),
    ))?;
    let pane = session.primary_pane();
    pane.wait_stable_contains("ready", Duration::from_secs(/*secs*/ 3))?;

    pane.send_key(TmuxKey::Escape)?;
    pane.send_literal("/x")?;
    let capture = pane.wait_stable_until(
        "a read containing 2f 78",
        Duration::from_secs(/*secs*/ 3),
        |capture| od_reads(capture).iter().any(|read| read.contains("2f 78")),
    )?;
    let reads = od_reads(&capture);
    pretty_assertions::assert_eq!(reads, vec!["1b", "2f 78"]);
    Ok(())
}

#[test]
fn viewport_and_scrollback_are_captured_separately() -> Result<()> {
    let Some(server) = server_or_skip("viewport_and_scrollback")? else {
        return Ok(());
    };
    let session = server.new_session(SessionSpec::new(
        "capture",
        TerminalSize::new(/*columns*/ 40, /*rows*/ 5),
        command_for_shell(
            "printf 'old-line\n'; i=1; while [ $i -le 12 ]; do printf 'line-%02d\n' $i; i=$((i + 1)); done; sleep 30",
        ),
    ))?;
    let pane = session.primary_pane();

    let viewport = pane.wait_stable_contains("line-12", Duration::from_secs(/*secs*/ 3))?;
    let scrollback = pane.capture_scrollback_tail(/*lines*/ 20)?;
    assert!(!viewport.contains("old-line"));
    assert!(scrollback.contains("old-line"));
    assert!(scrollback.contains("line-12"));
    Ok(())
}

#[test]
fn command_failures_include_diagnostics_and_artifacts() -> Result<()> {
    if !TmuxServer::should_run("command failure artifacts")? {
        return Ok(());
    }
    let artifact_root = tempfile::tempdir()?;
    let server = TmuxServer::start_with_artifact_root(
        "command_failures_include_diagnostics",
        artifact_root.path().to_path_buf(),
    )?;
    let artifact_dir = server.artifact_dir();
    let mut command = server.command();
    let error = server
        .checked_output(
            command.arg("display-message").arg("-t").arg("missing-pane"),
            /*pane_id*/ None,
        )
        .expect_err("missing pane should fail")
        .to_string();

    for expected in [
        "tmux command failed",
        "display-message",
        "status:",
        "stdout:",
        "stderr:",
        "reproduction:",
    ] {
        assert!(error.contains(expected));
    }
    assert!(artifact_dir.join("manifest.json").is_file());
    Ok(())
}

#[test]
fn wait_failures_write_complete_redacted_artifacts() -> Result<()> {
    if !TmuxServer::should_run("timeout artifacts")? {
        return Ok(());
    }
    let artifact_root = tempfile::tempdir()?;
    let attachment_root = tempfile::tempdir()?;
    let config = attachment_root.path().join("config.toml");
    let candidate_log = attachment_root.path().join("candidate.log");
    std::fs::write(&config, "model = \"test\"\n")?;
    std::fs::write(&candidate_log, "candidate fixture log\n")?;
    let server = TmuxServer::start_with_artifact_root(
        "wait_failures_write_complete",
        artifact_root.path().to_path_buf(),
    )?;
    server.register_artifact("config.toml", &config);
    server.register_artifact("candidate.log", &candidate_log);
    let artifact_dir = server.artifact_dir();
    let session = server.new_session(SessionSpec::new(
        "timeout",
        TerminalSize::new(/*columns*/ 40, /*rows*/ 8),
        CommandSpec::new("sh")
            .env("OPENAI_API_KEY", "must-not-leak")
            .arg("-c")
            .arg("IFS= read -r line; printf 'visible:%s\\n' \"$line\"; sleep 30"),
    ))?;
    let pane = session.primary_pane();
    pane.send_literal("ok")?;
    pane.send_key(TmuxKey::Enter)?;
    pane.wait_stable_contains("visible:ok", Duration::from_secs(/*secs*/ 3))?;
    let error = pane
        .wait_stable_contains("missing-sentinel", Duration::from_millis(/*millis*/ 250))
        .expect_err("missing sentinel should time out")
        .to_string();

    assert!(error.contains("missing-sentinel"));
    assert!(error.contains("last viewport:"));
    assert!(error.contains("visible:ok"));
    let mut files = std::fs::read_dir(&artifact_dir)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().to_string()))
        .collect::<std::io::Result<Vec<_>>>()?;
    files.sort();
    assert_eq!(
        files,
        vec![
            "candidate.log",
            "command-log.txt",
            "config.toml",
            "dimensions.txt",
            "input-events.txt",
            "manifest.json",
            "pane-metadata.txt",
            "reason.txt",
            "reproduce.sh",
            "scrollback.txt",
            "viewport.txt",
        ]
    );
    let command_log = std::fs::read_to_string(artifact_dir.join("command-log.txt"))?;
    assert!(command_log.contains("OPENAI_API_KEY=<redacted>"));
    assert!(!command_log.contains("must-not-leak"));
    assert_eq!(
        std::fs::read_to_string(artifact_dir.join("input-events.txt"))?,
        "literal bytes=2\nkey Enter"
    );
    assert_eq!(
        std::fs::read_to_string(artifact_dir.join("reproduce.sh"))?,
        "CORBANU_TMUX_REQUIRED=1 just test -p codex-tui --test all \
         wait_failures_write_complete"
    );
    assert!(std::fs::read_to_string(artifact_dir.join("pane-metadata.txt"))?.contains("size=40x8"));
    Ok(())
}

#[test]
fn successful_session_does_not_emit_artifacts() -> Result<()> {
    if !TmuxServer::should_run("success artifact laziness")? {
        return Ok(());
    }
    let artifact_root = tempfile::tempdir()?;
    let server = TmuxServer::start_with_artifact_root(
        "successful_session_does_not_emit",
        artifact_root.path().to_path_buf(),
    )?;
    let artifact_dir = server.artifact_dir();
    let _session = server.new_session(SessionSpec::new(
        "success",
        TerminalSize::new(/*columns*/ 40, /*rows*/ 8),
        command_for_shell("sleep 30"),
    ))?;
    assert!(!artifact_dir.exists());
    Ok(())
}

#[test]
fn panic_unwind_cleans_up_and_writes_artifacts() -> Result<()> {
    if !TmuxServer::should_run("panic cleanup artifacts")? {
        return Ok(());
    }
    let artifact_root = tempfile::tempdir()?;
    let server = TmuxServer::start_with_artifact_root(
        "panic_unwind_cleans_up",
        artifact_root.path().to_path_buf(),
    )?;
    let socket_root = server.socket_root();
    let artifact_dir = server.artifact_dir();
    let result = catch_unwind(move || {
        let _session = server
            .new_session(SessionSpec::new(
                "panic",
                TerminalSize::new(/*columns*/ 40, /*rows*/ 8),
                command_for_shell("sleep 30"),
            ))
            .expect("session should start");
        panic!("intentional unwind");
    });

    assert!(result.is_err());
    assert!(!socket_root.exists());
    assert!(artifact_dir.join("manifest.json").is_file());
    assert!(
        std::fs::read_to_string(artifact_dir.join("reason.txt"))?
            .contains("panicked while tmux session was active")
    );
    Ok(())
}
