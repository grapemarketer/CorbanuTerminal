use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::thread::sleep;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use anyhow::Result;
use tempfile::TempDir;

use super::tmux_artifacts::ArtifactRecorder;
use super::tmux_artifacts::FailureCapture;
pub(crate) use super::tmux_command::CommandSpec;
use super::tmux_command::render_command;
use super::tmux_process::TmuxProcesses;
use super::tmux_process::is_running as process_is_running;
use super::tmux_process::parse_report as parse_process_report;
use super::tmux_process::wait_for_exit as wait_for_process_exit;

static NEXT_SERVER_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);
const STABLE_CAPTURE_INTERVAL: Duration = Duration::from_millis(100);
/// How often to recheck whether the pane's program has read its pending input.
const INPUT_DRAIN_INTERVAL: Duration = Duration::from_millis(10);
/// Minimum gap between a bare Esc and the next input. If the TUI reads Esc
/// together with what follows, crossterm parses the pair as Alt+<next key>:
/// Esc then `/exit` types `exit`, and Esc then Enter is Alt+Enter.
const ESCAPE_SETTLE_MIN: Duration = Duration::from_millis(300);
/// Upper bound on waiting for the Esc to be read and the viewport to settle.
/// Busy screens (a running turn) may never settle; input then proceeds after
/// this.
const ESCAPE_SETTLE_MAX: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalSize {
    pub(crate) columns: u16,
    pub(crate) rows: u16,
}

impl TerminalSize {
    pub(crate) fn new(columns: u16, rows: u16) -> Self {
        Self { columns, rows }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TmuxKey {
    Down,
    Right,
    Enter,
    Escape,
    CtrlC,
}

impl TmuxKey {
    fn name(self) -> &'static str {
        match self {
            Self::Down => "Down",
            Self::Right => "Right",
            Self::Enter => "Enter",
            Self::Escape => "Escape",
            Self::CtrlC => "C-c",
        }
    }
}

#[derive(Debug)]
pub(crate) struct SessionSpec {
    name_prefix: String,
    size: TerminalSize,
    command: CommandSpec,
    current_dir: Option<PathBuf>,
}

impl SessionSpec {
    pub(crate) fn new(
        name_prefix: impl Into<String>,
        size: TerminalSize,
        command: CommandSpec,
    ) -> Self {
        Self {
            name_prefix: name_prefix.into(),
            size,
            command,
            current_dir: None,
        }
    }

    pub(crate) fn current_dir(mut self, current_dir: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(current_dir.into());
        self
    }
}

#[derive(Debug)]
pub(crate) struct TmuxServer {
    socket_name: String,
    socket_dir: TempDir,
    pub(super) artifacts: ArtifactRecorder,
    pub(super) processes: TmuxProcesses,
    cleanup_guard: std::process::Child,
}

impl TmuxServer {
    pub(crate) fn is_available() -> bool {
        Command::new("tmux")
            .arg("-V")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    pub(crate) fn should_run(scenario: &str) -> Result<bool> {
        if Self::is_available() {
            return Ok(true);
        }
        anyhow::ensure!(
            std::env::var_os("CORBANU_TMUX_REQUIRED").as_deref() != Some(OsStr::new("1")),
            "tmux is required for {scenario} but is unavailable on PATH"
        );
        eprintln!("skipping {scenario} because tmux is unavailable");
        Ok(false)
    }

    pub(crate) fn start(scenario: &str) -> Result<Self> {
        Self::start_with_artifact_root(scenario, ArtifactRecorder::default_root())
    }

    pub(super) fn start_with_artifact_root(scenario: &str, artifact_root: PathBuf) -> Result<Self> {
        anyhow::ensure!(Self::is_available(), "tmux is unavailable on PATH");
        let id = NEXT_SERVER_ID.fetch_add(1, Ordering::Relaxed);
        let socket_dir = tempfile::Builder::new()
            .prefix("cdx-tmux-")
            .tempdir_in("/tmp")
            .context("create private tmux socket directory")?;
        let socket_name = format!("codex-tui-test-{}-{id}", std::process::id());
        // The pipe closes even when nextest terminates this process without running
        // Drop. Keep the watchdog outside tmux so a killed test cannot orphan it.
        let cleanup_guard = Command::new("sh")
            .args([
                "-c",
                super::tmux_process::CLEANUP_WATCHDOG,
                "tmux-test-cleanup",
            ])
            .arg(&socket_name)
            .env("TMUX_TMPDIR", socket_dir.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .context("start tmux cleanup watchdog")?;
        Ok(Self {
            socket_name,
            cleanup_guard,
            socket_dir,
            artifacts: ArtifactRecorder::new(artifact_root, scenario, id),
            processes: TmuxProcesses::default(),
        })
    }

    pub(crate) fn new_session(&self, spec: SessionSpec) -> Result<TmuxSession<'_>> {
        let session_id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        let session_name = format!("{}-{session_id}", spec.name_prefix);
        let mut command = self.command();
        command
            .arg("new-session")
            .arg("-d")
            .arg("-P")
            .arg("-F")
            .arg("#{pane_id}\t#{pane_pid}\t#{pid}")
            .arg("-x")
            .arg(spec.size.columns.to_string())
            .arg("-y")
            .arg(spec.size.rows.to_string())
            .arg("-s")
            .arg(&session_name);
        if let Some(current_dir) = spec.current_dir {
            command.arg("-c").arg(current_dir);
        }
        command.arg("--");
        spec.command.append_to(&mut command);

        let output = self.checked_output(&mut command, /*pane_id*/ None)?;
        let (pane_id, pane_pid, server_pid) = parse_process_report(&output, "pane")?;
        self.processes.record(pane_pid, server_pid);
        self.artifacts.record_dimensions(format!(
            "{pane_id} {}x{} initial",
            spec.size.columns, spec.size.rows
        ));
        Ok(TmuxSession {
            server: self,
            name: session_name,
            primary_pane: TmuxPane::new(self, pane_id, pane_pid),
        })
    }

    pub(super) fn command(&self) -> Command {
        let mut command = Command::new("tmux");
        command.env("TMUX_TMPDIR", self.socket_dir.path());
        command.arg("-L").arg(&self.socket_name);
        command
    }

    fn socket_root(&self) -> PathBuf {
        self.socket_dir.path().to_path_buf()
    }

    pub(crate) fn register_artifact(&self, label: &str, path: impl AsRef<Path>) {
        self.artifacts
            .register_attachment(label, path.as_ref().to_path_buf());
    }

    pub(crate) fn artifact_dir(&self) -> PathBuf {
        self.artifacts.directory().to_path_buf()
    }

    fn has_session(&self, session_name: &str) -> bool {
        self.command()
            .arg("has-session")
            .arg("-t")
            .arg(session_name)
            .output()
            .is_ok_and(|output| output.status.success())
    }

    pub(super) fn checked_output(
        &self,
        command: &mut Command,
        pane_id: Option<&str>,
    ) -> Result<Output> {
        let rendered = render_command(command);
        self.artifacts.record_command(rendered.clone());
        let output = command
            .output()
            .with_context(|| format!("failed to start tmux command: {rendered}"))?;
        if output.status.success() {
            return Ok(output);
        }

        let message = format!(
            "tmux command failed: {rendered}\nstatus: {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            stdout_text(&output),
            String::from_utf8_lossy(&output.stderr)
        );
        let artifact = self.emit_failure(&message, pane_id);
        anyhow::bail!(
            "{message}\nreproduction: {}\nartifacts: {}",
            self.artifacts.directory().join("reproduce.sh").display(),
            artifact.display()
        );
    }

    pub(super) fn emit_failure(&self, reason: &str, pane_id: Option<&str>) -> PathBuf {
        let capture = pane_id.map_or_else(String::new, |pane| {
            self.capture_for_artifact(pane, &["capture-pane", "-p", "-t", pane])
        });
        let scrollback = pane_id.map_or_else(String::new, |pane| {
            self.capture_for_artifact(pane, &["capture-pane", "-p", "-S", "-200", "-t", pane])
        });
        let metadata = pane_id.map_or_else(String::new, |pane| {
            self.capture_for_artifact(
                pane,
                &[
                    "display-message",
                    "-p",
                    "-t",
                    pane,
                    "#{pane_id}\tpid=#{pane_pid}\tsize=#{pane_width}x#{pane_height}\tcommand=#{pane_current_command}\tdead=#{pane_dead}\tstatus=#{pane_dead_status}",
                ],
            )
        });
        self.artifacts
            .emit(FailureCapture {
                reason: reason.to_string(),
                viewport: capture,
                scrollback,
                pane_metadata: metadata,
            })
            .unwrap_or_else(|error| {
                eprintln!("failed to write tmux artifacts: {error:#}");
                self.artifacts.directory().to_path_buf()
            })
    }

    fn capture_for_artifact(&self, pane_id: &str, args: &[&str]) -> String {
        match self.command().args(args).output() {
            Ok(output) if output.status.success() => stdout_text(&output),
            Ok(output) => format!(
                "artifact capture failed for pane {pane_id}: status={:?}\nstderr:\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            ),
            Err(error) => format!("artifact capture failed for pane {pane_id}: {error}"),
        }
    }
}

impl Drop for TmuxServer {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.emit_failure(
                "test panicked while tmux server was active",
                /*pane_id*/ None,
            );
        }
        self.cleanup_guard.stdin.take();
        let _ = self.cleanup_guard.wait();
        self.processes.wait_for_cleanup();
    }
}

#[derive(Debug)]
pub(crate) struct TmuxSession<'a> {
    pub(super) server: &'a TmuxServer,
    pub(super) name: String,
    primary_pane: TmuxPane<'a>,
}

impl<'a> TmuxSession<'a> {
    /// Attachment targets only this owned private fixture; callers must keep it alive.
    pub(crate) fn attachment_command(&self) -> Command {
        let mut command = self.server.command();
        command.args(["attach-session", "-t", &self.name]);
        command
    }

    pub(crate) fn is_running(&self) -> bool {
        self.server.has_session(&self.name) || process_is_running(self.primary_pane.pid)
    }

    pub(crate) fn primary_pane(&self) -> &TmuxPane<'a> {
        &self.primary_pane
    }

    pub(crate) fn wait_for_exit(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !self.server.has_session(&self.name) && !process_is_running(self.primary_pane.pid) {
                return Ok(());
            }
            sleep(STABLE_CAPTURE_INTERVAL);
        }

        let reason = format!("timed out waiting for session {:?} to exit", self.name);
        let artifact = self
            .server
            .emit_failure(&reason, Some(self.primary_pane.id.as_str()));
        anyhow::bail!("{reason}; artifacts: {}", artifact.display());
    }
}

impl Drop for TmuxSession<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.server.emit_failure(
                "test panicked while tmux session was active",
                Some(self.primary_pane.id.as_str()),
            );
        }
        if self.server.has_session(&self.name) {
            let _ = self
                .server
                .command()
                .arg("kill-session")
                .arg("-t")
                .arg(&self.name)
                .output();
        }
        super::tmux_process::terminate_pane_group(self.primary_pane.pid);
    }
}

#[derive(Debug)]
pub(crate) struct TmuxPane<'a> {
    pub(super) server: &'a TmuxServer,
    pub(super) id: String,
    pub(super) pid: u32,
    /// When a bare Esc was sent and no input has followed it yet.
    escape_sent_at: Mutex<Option<Instant>>,
}

impl<'a> TmuxPane<'a> {
    pub(super) fn new(server: &'a TmuxServer, id: String, pid: u32) -> Self {
        Self {
            server,
            id,
            pid,
            escape_sent_at: Mutex::new(None),
        }
    }

    /// Keeps a bare Esc from reaching the TUI in the same read as the next
    /// input (see [`ESCAPE_SETTLE_MIN`]). After the minimum gap, waits until
    /// the pane's program has read the Esc from its terminal, so a reader that
    /// was descheduled for longer than the gap (a loaded CI runner) still reads
    /// it alone. Two equal captures afterwards are a heuristic that it has also
    /// finished reacting to the Esc.
    fn settle_after_escape(&self) -> Result<()> {
        let sent_at = self
            .escape_sent_at
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some(sent_at) = sent_at else {
            return Ok(());
        };
        sleep(ESCAPE_SETTLE_MIN.saturating_sub(sent_at.elapsed()));
        let deadline = Instant::now() + ESCAPE_SETTLE_MAX;
        self.wait_for_input_drained(deadline)?;
        let mut previous = self.capture_viewport()?;
        while Instant::now() < deadline {
            sleep(STABLE_CAPTURE_INTERVAL);
            let current = self.capture_viewport()?;
            if current == previous {
                break;
            }
            previous = current;
        }
        Ok(())
    }

    /// Waits until the pane's terminal has no unread input, or `deadline`.
    /// In canonical mode the kernel only counts complete lines, so a lone Esc
    /// reads as drained, which is the old behavior.
    fn wait_for_input_drained(&self, deadline: Instant) -> Result<()> {
        let mut command = self.server.command();
        let output = self.server.checked_output(
            command
                .arg("display-message")
                .arg("-p")
                .arg("-t")
                .arg(&self.id)
                .arg("#{pane_tty}"),
            Some(self.id.as_str()),
        )?;
        let tty = stdout_text(&output).trim().to_string();
        if tty.is_empty() {
            return Ok(());
        }
        // Opening the pane's terminal never reads from it, and O_NOCTTY keeps
        // it from becoming this process's controlling terminal.
        let terminal = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
            .open(&tty)
            .with_context(|| format!("failed to open pane terminal {tty}"))?;
        loop {
            let mut pending: libc::c_int = 0;
            // SAFETY: FIONREAD writes one c_int to `pending`, which outlives
            // the call, and `terminal` keeps the descriptor open.
            let result = unsafe { libc::ioctl(terminal.as_raw_fd(), libc::FIONREAD, &mut pending) };
            if result == -1 {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("failed to query unread input on {tty}"));
            }
            if pending == 0 || Instant::now() >= deadline {
                return Ok(());
            }
            sleep(INPUT_DRAIN_INTERVAL);
        }
    }

    pub(crate) fn send_literal(&self, text: &str) -> Result<()> {
        self.settle_after_escape()?;
        self.server
            .artifacts
            .record_input(format!("literal bytes={}", text.len()));
        let mut command = self.server.command();
        self.server.checked_output(
            command
                .arg("send-keys")
                .arg("-t")
                .arg(&self.id)
                .arg("-l")
                .arg("--")
                .arg(text),
            Some(self.id.as_str()),
        )?;
        Ok(())
    }

    /// Send a secret-bearing literal without recording its value in the
    /// command log or failure reason. The pane still receives real key input.
    pub(crate) fn send_secret_literal(&self, text: &str) -> Result<()> {
        self.settle_after_escape()?;
        self.server.artifacts.record_input(format!(
            "secret literal bytes={} value=<redacted>",
            text.len()
        ));
        self.server
            .artifacts
            .record_command("tmux send-keys -l <redacted secret literal>".to_string());
        let output = self
            .server
            .command()
            .arg("send-keys")
            .arg("-t")
            .arg(&self.id)
            .arg("-l")
            .arg("--")
            .arg(text)
            .output()
            .context("failed to send redacted secret literal through tmux")?;
        if output.status.success() {
            return Ok(());
        }
        let reason = format!(
            "tmux could not send a redacted secret literal: status={:?}",
            output.status.code()
        );
        let artifact = self.server.emit_failure(&reason, Some(self.id.as_str()));
        anyhow::bail!("{reason}; artifacts: {}", artifact.display());
    }

    pub(crate) fn send_key(&self, key: TmuxKey) -> Result<()> {
        self.settle_after_escape()?;
        self.server
            .artifacts
            .record_input(format!("key {}", key.name()));
        let mut command = self.server.command();
        self.server.checked_output(
            command
                .arg("send-keys")
                .arg("-t")
                .arg(&self.id)
                .arg(key.name()),
            Some(self.id.as_str()),
        )?;
        if key == TmuxKey::Escape {
            *self
                .escape_sent_at
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Instant::now());
        }
        Ok(())
    }

    pub(crate) fn capture_viewport(&self) -> Result<String> {
        let mut command = self.server.command();
        let output = self.server.checked_output(
            command
                .arg("capture-pane")
                .arg("-p")
                .arg("-t")
                .arg(&self.id),
            Some(self.id.as_str()),
        )?;
        Ok(stdout_text(&output))
    }

    pub(crate) fn capture_scrollback_tail(&self, lines: usize) -> Result<String> {
        let mut command = self.server.command();
        let output = self.server.checked_output(
            command
                .arg("capture-pane")
                .arg("-p")
                .arg("-S")
                .arg(format!("-{lines}"))
                .arg("-t")
                .arg(&self.id),
            Some(self.id.as_str()),
        )?;
        Ok(stdout_text(&output))
    }

    pub(crate) fn wait_stable_contains(&self, needle: &str, timeout: Duration) -> Result<String> {
        self.wait_stable_until(
            &format!("viewport containing {needle:?}"),
            timeout,
            |capture| capture.contains(needle),
        )
    }

    pub(crate) fn wait_stable_until(
        &self,
        description: &str,
        timeout: Duration,
        condition: impl Fn(&str) -> bool,
    ) -> Result<String> {
        let deadline = Instant::now() + timeout;
        let mut previous_matching_capture = None;
        let mut last_capture = String::new();

        while Instant::now() < deadline {
            last_capture = self.capture_viewport()?;
            if condition(&last_capture) {
                if previous_matching_capture.as_deref() == Some(last_capture.as_str()) {
                    return Ok(last_capture);
                }
                previous_matching_capture = Some(last_capture.clone());
            } else {
                previous_matching_capture = None;
            }
            sleep(STABLE_CAPTURE_INTERVAL);
        }

        let reason = format!("timed out waiting for stable {description}");
        let artifact = self.server.emit_failure(&reason, Some(self.id.as_str()));
        anyhow::bail!(
            "{reason}; last viewport:\n{last_capture}\nartifacts: {}",
            artifact.display()
        );
    }

    pub(crate) fn close(self) -> Result<()> {
        let mut command = self.server.command();
        self.server.checked_output(
            command.arg("kill-pane").arg("-t").arg(&self.id),
            Some(self.id.as_str()),
        )?;
        Ok(())
    }
}

fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn command_for_shell(script: &str) -> CommandSpec {
    CommandSpec::new(OsStr::new("sh")).arg("-c").arg(script)
}

#[cfg(test)]
#[path = "tmux_tests.rs"]
mod tests;
