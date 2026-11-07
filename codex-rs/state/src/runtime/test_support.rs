#[cfg(test)]
use chrono::DateTime;
#[cfg(test)]
use chrono::Utc;
#[cfg(test)]
use codex_protocol::ThreadId;
#[cfg(test)]
use codex_protocol::openai_models::ReasoningEffort;
#[cfg(test)]
use codex_protocol::protocol::AskForApproval;
#[cfg(test)]
use codex_protocol::protocol::SandboxPolicy;
#[cfg(test)]
use codex_protocol::protocol::ThreadHistoryMode;
#[cfg(test)]
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use std::time::SystemTime;
#[cfg(test)]
use std::time::UNIX_EPOCH;
#[cfg(test)]
use uuid::Uuid;

#[cfg(test)]
use crate::ThreadMetadata;

#[cfg(test)]
pub(crate) fn unique_temp_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!(
        "codex-state-runtime-test-{nanos}-{}",
        Uuid::new_v4()
    ))
}

#[cfg(test)]
pub(super) fn test_thread_metadata(
    codex_home: &Path,
    thread_id: ThreadId,
    cwd: PathBuf,
) -> ThreadMetadata {
    let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).expect("timestamp");
    ThreadMetadata {
        id: thread_id,
        rollout_path: codex_home.join(format!("rollout-{thread_id}.jsonl")),
        created_at: now,
        updated_at: now,
        recency_at: now,
        source: "cli".to_string(),
        history_mode: ThreadHistoryMode::Legacy,
        thread_source: None,
        agent_nickname: None,
        agent_role: None,
        agent_path: None,
        model_provider: "test-provider".to_string(),
        model: Some("gpt-5".to_string()),
        reasoning_effort: Some(ReasoningEffort::Medium),
        cwd,
        cli_version: "0.0.0".to_string(),
        title: String::new(),
        name: None,
        preview: Some("hello".to_string()),
        sandbox_policy: crate::extract::enum_to_string(&SandboxPolicy::new_read_only_policy()),
        approval_mode: crate::extract::enum_to_string(&AskForApproval::OnRequest),
        tokens_used: 0,
        first_user_message: Some("hello".to_string()),
        archived_at: None,
        section: None,
        section_position: None,
        section_entered_at: None,
        git_sha: None,
        git_branch: None,
        git_origin_url: None,
    }
}

/// Lets the child of `fork_child_holding_descriptors` start its program when
/// dropped, on every path, so a failing check cannot leave it waiting.
#[cfg(all(test, unix))]
pub(crate) struct ReleaseChild(std::io::PipeWriter);

#[cfg(all(test, unix))]
impl Drop for ReleaseChild {
    fn drop(&mut self) {
        use std::io::Write;
        let _ = self.0.write_all(b"r");
    }
}

/// How long the child of `fork_child_holding_descriptors` waits for its
/// release before it fails the spawn instead of waiting on.
#[cfg(all(test, unix))]
const RELEASE_WAIT_MS: libc::c_int = 60_000;

/// Held by `fork_child_holding_descriptors` from its pipes to the fork report.
#[cfg(all(test, unix))]
static FORKING_CHILD: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Fork a child, from another thread as any spawn does, that keeps a copy of
/// every descriptor of this process open until the returned `ReleaseChild` is
/// dropped, and return once it has forked. #928: a lock released only by
/// closing its file stays held through that copy.
#[cfg(all(test, unix))]
pub(crate) fn fork_child_holding_descriptors() -> std::io::Result<(
    ReleaseChild,
    std::thread::JoinHandle<std::io::Result<std::process::ExitStatus>>,
)> {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    // One fork at a time, with its pipes made under the lock: a child then
    // holds copies only of earlier children's release write ends, so if this
    // process dies the latest child sees EOF first and its exec closes the
    // copies the earlier ones wait on. Without it, concurrent tests in one
    // process (plain `cargo test`) can fork children that hold each other's
    // write ends and wait out `RELEASE_WAIT_MS` off Linux.
    let forking = FORKING_CHILD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // The child reports the fork, then waits for the release byte, so its
    // copies stay open until the caller's checks have run.
    let (mut forked, mut forked_tx) = std::io::pipe()?;
    let (mut release_rx, release_tx) = std::io::pipe()?;
    let release_tx_fd = release_tx.as_raw_fd();
    let release = ReleaseChild(release_tx);
    let parent = std::process::id();
    let spawner = std::thread::spawn(move || {
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "exit 0"]);
        // SAFETY: the hook makes only async-signal-safe calls and does not
        // allocate, so it is safe after fork.
        unsafe {
            command.pre_exec(move || {
                wait_for_release_in_child(release_tx_fd, parent, &mut forked_tx, &mut release_rx)
            });
        }
        command.status()
    });
    let mut byte = [0; 1];
    forked.read_exact(&mut byte)?;
    drop(forking);
    Ok((release, spawner))
}

/// Runs in the forked child before exec: reports the fork, then waits for the
/// release byte, without outliving the test process (#1252). The child closes
/// its copy of the write end, so the read sees EOF once this process dies; on
/// Linux the death of the forking thread also kills it. Elsewhere nothing
/// kills it, so once this process is gone (EOF, or `RELEASE_WAIT_MS` passed
/// and the child was re-parented) it goes on to run its program and exit 0:
/// failing the hook there would abort the child, since std cannot report the
/// error to a dead parent. A timeout while this process lives fails the spawn,
/// which fails the test.
#[cfg(all(test, unix))]
fn wait_for_release_in_child(
    release_tx_fd: std::os::fd::RawFd,
    parent: u32,
    forked_tx: &mut std::io::PipeWriter,
    release_rx: &mut std::io::PipeReader,
) -> std::io::Result<()> {
    use std::io::Read;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    // SAFETY: closes this child's own copy of the write end, which nothing
    // else in the child uses; the parent's copy stays open.
    if unsafe { libc::close(release_tx_fd) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: plain integer arguments; the kernel reads the signal as an
        // unsigned long.
        if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // The parent may have died before the signal was armed.
        // SAFETY: no arguments.
        if unsafe { libc::getppid() } as u32 != parent {
            return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
        }
    }
    forked_tx.write_all(b"f")?;
    let mut poll_fd = libc::pollfd {
        fd: release_rx.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: `poll_fd` is one valid, writable `pollfd`.
        match unsafe { libc::poll(&mut poll_fd, 1, RELEASE_WAIT_MS) } {
            0 => {
                #[cfg(not(target_os = "linux"))]
                {
                    // SAFETY: no arguments.
                    if unsafe { libc::getppid() } as u32 != parent {
                        return Ok(());
                    }
                }
                // std reports a hook error to the spawner by its OS error code.
                return Err(std::io::Error::from_raw_os_error(libc::ETIMEDOUT));
            }
            -1 => {
                let err = std::io::Error::last_os_error();
                if err.kind() != std::io::ErrorKind::Interrupted {
                    return Err(err);
                }
            }
            // Readable, or EOF if every write end is closed.
            #[cfg(target_os = "linux")]
            _ => return release_rx.read_exact(&mut [0; 1]),
            #[cfg(not(target_os = "linux"))]
            _ => {
                return match release_rx.read_exact(&mut [0; 1]) {
                    Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => Ok(()),
                    result => result,
                };
            }
        }
    }
}
