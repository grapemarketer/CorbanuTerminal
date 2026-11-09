use super::windows_common::finish_driver_spawn;
use crate::capture_pipes::OUTPUT_DRAIN_TIMEOUT;
use crate::capture_pipes::PipeReader;
use crate::capture_pipes::finish_each;
use crate::conpty::ConptyInstance;
use crate::conpty::spawn_conpty_process_as_user;
use crate::desktop::LaunchDesktop;
use crate::integrity_leases::IntegrityLease;
use crate::logging::log_failure;
use crate::logging::log_note;
use crate::logging::log_success;
use crate::process::ConsoleMode;
use crate::process::StderrMode;
use crate::process::StdinMode;
use crate::process::spawn_process_with_pipes;
use crate::spawn_prep::LegacyAclSids;
use crate::spawn_prep::SpawnContext;
use crate::spawn_prep::SpawnPrepOptions;
use crate::spawn_prep::allow_null_device_for_workspace_write;
use crate::spawn_prep::apply_legacy_session_acl_rules;
use crate::spawn_prep::legacy_session_capability_roots;
use crate::spawn_prep::prepare_legacy_session_security;
use crate::spawn_prep::prepare_legacy_spawn_context;
use anyhow::Result;
use codex_protocol::models::PermissionProfile;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_pty::JobObject;
use codex_utils_pty::ProcessDriver;
use codex_utils_pty::SpawnedProcess;
use codex_utils_pty::TerminalSize;
use codex_utils_pty::WindowsTtyInputNormalizer;
use std::collections::HashMap;
use std::path::Path;
use std::ptr;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::WriteFile;
use windows_sys::Win32::System::Console::COORD;
use windows_sys::Win32::System::Console::ResizePseudoConsole;
use windows_sys::Win32::System::Threading::GetExitCodeProcess;
use windows_sys::Win32::System::Threading::INFINITE;
use windows_sys::Win32::System::Threading::PROCESS_INFORMATION;
use windows_sys::Win32::System::Threading::TerminateProcess;
use windows_sys::Win32::System::Threading::WaitForSingleObject;

const WAIT_TIMEOUT: u32 = 0x0000_0102;

/// Reads one output pipe into its channel. Its only work besides reading is
/// a channel send, so cancelling its read cannot cut a chunk short.
type OutputReader = PipeReader<broadcast::Sender<Vec<u8>>>;

/// The session's output readers: the ConPTY's, or stdout's and stderr's.
enum OutputReaders {
    Pty(OutputReader),
    Pipes([OutputReader; 2]),
}

impl OutputReaders {
    /// Once the command has exited, lets the readers read what is left for
    /// at most [`OUTPUT_DRAIN_TIMEOUT`], then cancels them (#1861): a process
    /// the command left running may hold the pipes' write ends. Their
    /// channels close when this returns.
    fn finish(self) {
        match self {
            Self::Pty(reader) => drop(finish_each([reader], OUTPUT_DRAIN_TIMEOUT)),
            Self::Pipes(readers) => drop(finish_each(readers, OUTPUT_DRAIN_TIMEOUT)),
        }
    }
}

struct LegacyProcessHandles {
    process: PROCESS_INFORMATION,
    job: Arc<JobObject>,
    output_readers: OutputReaders,
    writer_handle: tokio::task::JoinHandle<()>,
    hpc: Option<HANDLE>,
    conpty_owner: Option<ConptyInstance>,
    token_handle: HANDLE,
    desktop: Option<LaunchDesktop>,
}

#[allow(clippy::too_many_arguments)]
fn spawn_legacy_process(
    h_token: HANDLE,
    command: &[String],
    cwd: &Path,
    env_map: &HashMap<String, String>,
    use_private_desktop: bool,
    tty: bool,
    stdin_open: bool,
    stdout_tx: broadcast::Sender<Vec<u8>>,
    stderr_tx: Option<broadcast::Sender<Vec<u8>>>,
    writer_rx: mpsc::Receiver<Vec<u8>>,
    logs_base_dir: Option<&Path>,
) -> Result<LegacyProcessHandles> {
    let (pi, job, output_readers, writer_handle, hpc, conpty_owner, desktop) = if tty {
        let (pi, mut conpty) = spawn_conpty_process_as_user(
            h_token,
            command,
            cwd,
            env_map,
            use_private_desktop,
            logs_base_dir,
        )?;
        let job = conpty
            .job()
            .ok_or_else(|| anyhow::anyhow!("spawned ConPTY is missing its process job"))?;
        let hpc = conpty.raw_handle();
        let output_readers =
            OutputReaders::Pty(spawn_output_reader(conpty.take_output_read(), stdout_tx));
        let writer_handle = spawn_input_writer(
            Some(conpty.take_input_write()),
            writer_rx,
            /*normalize_newlines*/ true,
        );
        (
            pi,
            job,
            output_readers,
            writer_handle,
            hpc,
            Some(conpty),
            None,
        )
    } else {
        let pipe_handles = spawn_process_with_pipes(
            h_token,
            command,
            cwd,
            env_map,
            if stdin_open {
                StdinMode::Open
            } else {
                StdinMode::Closed
            },
            StderrMode::Separate,
            ConsoleMode::Inherit,
            use_private_desktop,
            logs_base_dir,
        )?;
        let stdout_reader = spawn_output_reader(pipe_handles.stdout_read, stdout_tx);
        let Some(stderr_read) = pipe_handles.stderr_read else {
            anyhow::bail!("separate stderr handle should be present");
        };
        let Some(stderr_tx) = stderr_tx else {
            anyhow::bail!("separate stderr channel should be present");
        };
        let stderr_reader = spawn_output_reader(stderr_read, stderr_tx);
        let output_readers = OutputReaders::Pipes([stdout_reader, stderr_reader]);
        let writer_handle = spawn_input_writer(
            pipe_handles.stdin_write,
            writer_rx,
            /*normalize_newlines*/ false,
        );
        (
            pipe_handles.process,
            pipe_handles.job(),
            output_readers,
            writer_handle,
            None,
            None,
            Some(pipe_handles.desktop),
        )
    };
    Ok(LegacyProcessHandles {
        process: pi,
        job,
        output_readers,
        writer_handle,
        hpc,
        conpty_owner,
        token_handle: h_token,
        desktop,
    })
}

fn spawn_output_reader(output_read: HANDLE, output_tx: broadcast::Sender<Vec<u8>>) -> OutputReader {
    PipeReader::spawn_with(output_read, output_tx, |output_tx, chunk| {
        let _ = output_tx.send(chunk.to_vec());
    })
}

fn spawn_input_writer(
    input_write: Option<HANDLE>,
    mut writer_rx: mpsc::Receiver<Vec<u8>>,
    normalize_newlines: bool,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let mut windows_input = WindowsTtyInputNormalizer::default();
        while let Some(bytes) = writer_rx.blocking_recv() {
            let Some(handle) = input_write else {
                continue;
            };
            let bytes = if normalize_newlines {
                windows_input.normalize(&bytes)
            } else {
                bytes
            };
            if write_all_handle(handle, &bytes).is_err() {
                break;
            }
        }
        if let Some(handle) = input_write {
            unsafe {
                CloseHandle(handle);
            }
        }
    })
}

fn terminate_job_or_process(
    job: &JobObject,
    process_handle: &Arc<StdMutex<Option<HANDLE>>>,
    logs_base_dir: Option<&Path>,
) {
    if let Err(job_err) = job.terminate() {
        log_note(
            &format!("legacy spawn failed to terminate process tree: {job_err}"),
            logs_base_dir,
        );
        if let Ok(guard) = process_handle.lock()
            && let Some(handle) = guard.as_ref()
            && unsafe { TerminateProcess(*handle, 1) } == 0
        {
            log_note(
                &format!(
                    "legacy spawn failed to terminate root process: {}",
                    unsafe { GetLastError() }
                ),
                logs_base_dir,
            );
        }
    }
}

fn write_all_handle(handle: HANDLE, mut bytes: &[u8]) -> Result<()> {
    while !bytes.is_empty() {
        let mut written = 0u32;
        let ok = unsafe {
            WriteFile(
                handle,
                bytes.as_ptr() as *const _,
                bytes.len() as u32,
                &mut written,
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            let err = unsafe { GetLastError() } as i32;
            return Err(anyhow::anyhow!("WriteFile failed: {err}"));
        }
        if written == 0 {
            anyhow::bail!("WriteFile returned success but wrote 0 bytes");
        }
        bytes = &bytes[written as usize..];
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn finalize_exit(
    exit_tx: oneshot::Sender<i32>,
    process_handle: Arc<StdMutex<Option<HANDLE>>>,
    thread_handle: HANDLE,
    output_readers: OutputReaders,
    logs_base_dir: Option<&Path>,
    command: Vec<String>,
) {
    let exit_code = {
        let mut raw_exit = 1u32;
        if let Ok(guard) = process_handle.lock()
            && let Some(handle) = guard.as_ref()
        {
            unsafe {
                WaitForSingleObject(*handle, INFINITE);
                GetExitCodeProcess(*handle, &mut raw_exit);
            }
        }
        raw_exit as i32
    };

    output_readers.finish();
    let _ = exit_tx.send(exit_code);

    unsafe {
        if thread_handle != 0 && thread_handle != INVALID_HANDLE_VALUE {
            CloseHandle(thread_handle);
        }
        if let Ok(mut guard) = process_handle.lock()
            && let Some(handle) = guard.take()
        {
            CloseHandle(handle);
        }
    }

    if exit_code == 0 {
        log_success(&command, logs_base_dir);
    } else {
        log_failure(&command, &format!("exit code {exit_code}"), logs_base_dir);
    }
}

fn resize_conpty_handle(hpc: &Arc<StdMutex<Option<HANDLE>>>, size: TerminalSize) -> Result<()> {
    let guard = hpc
        .lock()
        .map_err(|_| anyhow::anyhow!("failed to lock ConPTY handle"))?;
    let hpc = guard
        .as_ref()
        .copied()
        .ok_or_else(|| anyhow::anyhow!("process is not attached to a PTY"))?;
    let result = unsafe {
        ResizePseudoConsole(
            hpc,
            COORD {
                X: size.cols as i16,
                Y: size.rows as i16,
            },
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "failed to resize console: HRESULT {result}"
        ))
    }
}

/// A legacy session prepared for its command.
struct PreparedLegacySession {
    common: SpawnContext,
    h_token: HANDLE,
    /// Held until the command has exited (#158).
    integrity_lease: Option<IntegrityLease>,
    env_map: HashMap<String, String>,
}

/// A legacy session's preparation before its command starts: the spawn
/// context, the restricted token, and the ACL rules with the lease that holds
/// them.
#[allow(clippy::too_many_arguments)]
fn prepare_legacy_session(
    permission_profile: &PermissionProfile,
    workspace_roots: &[AbsolutePathBuf],
    codex_home: &Path,
    command: &[String],
    cwd: &Path,
    mut env_map: HashMap<String, String>,
    additional_deny_read_paths: &[AbsolutePathBuf],
    additional_deny_write_paths: &[AbsolutePathBuf],
) -> Result<PreparedLegacySession> {
    let common = prepare_legacy_spawn_context(
        permission_profile,
        workspace_roots,
        codex_home,
        cwd,
        &mut env_map,
        command,
        SpawnPrepOptions {
            inherit_path: false,
            add_git_safe_directory: false,
        },
    )?;
    if !common.permissions.has_full_disk_read_access() {
        anyhow::bail!("Restricted read-only access requires the elevated Windows sandbox backend");
    }
    // WRITE_RESTRICTED tokens consult restricting SIDs only for writes, so this
    // backend cannot make capability-SID deny-read ACLs authoritative.
    if !additional_deny_read_paths.is_empty() {
        anyhow::bail!("deny-read overrides require the elevated Windows sandbox backend");
    }
    let additional_deny_write_paths = additional_deny_write_paths
        .iter()
        .map(AbsolutePathBuf::to_path_buf)
        .collect::<Vec<_>>();
    let capability_roots = legacy_session_capability_roots(
        &common.permissions,
        &common.current_dir,
        &env_map,
        codex_home,
    );
    let security = prepare_legacy_session_security(
        common.uses_write_capabilities,
        codex_home,
        cwd,
        capability_roots,
    )?;
    allow_null_device_for_workspace_write(common.uses_write_capabilities);

    // Held until the command has exited (#158).
    let integrity_lease = match apply_legacy_session_acl_rules(
        &common.permissions,
        codex_home,
        &common.current_dir,
        &env_map,
        &[],
        &additional_deny_write_paths,
        LegacyAclSids {
            readonly_sid: security.readonly_sid.as_ref(),
            readonly_sid_str: security.readonly_sid_str.as_deref(),
            write_root_sids: &security.write_root_sids,
        },
        /*session*/ true,
    ) {
        Ok(lease) => lease,
        Err(err) => {
            unsafe {
                CloseHandle(security.h_token);
            }
            return Err(err);
        }
    };
    Ok(PreparedLegacySession {
        common,
        h_token: security.h_token,
        integrity_lease,
        env_map,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn spawn_windows_sandbox_session_legacy(
    permission_profile: &PermissionProfile,
    workspace_roots: &[AbsolutePathBuf],
    codex_home: &Path,
    command: Vec<String>,
    cwd: &Path,
    env_map: HashMap<String, String>,
    timeout_ms: Option<u64>,
    additional_deny_read_paths: &[AbsolutePathBuf],
    additional_deny_write_paths: &[AbsolutePathBuf],
    tty: bool,
    stdin_open: bool,
    use_private_desktop: bool,
) -> Result<SpawnedProcess> {
    // #1909: preparing a session (its capability SIDs and token, ACLs, and
    // the planted-folder watch's start, which walks the writable roots)
    // blocks for a while, so it runs off the async thread, in the same order.
    let PreparedLegacySession {
        common,
        h_token,
        integrity_lease,
        env_map,
    } = {
        let permission_profile = permission_profile.clone();
        let workspace_roots = workspace_roots.to_vec();
        let codex_home = codex_home.to_path_buf();
        let cwd = cwd.to_path_buf();
        let command = command.clone();
        let additional_deny_read_paths = additional_deny_read_paths.to_vec();
        let additional_deny_write_paths = additional_deny_write_paths.to_vec();
        tokio::task::spawn_blocking(move || {
            prepare_legacy_session(
                &permission_profile,
                &workspace_roots,
                &codex_home,
                &command,
                &cwd,
                env_map,
                &additional_deny_read_paths,
                &additional_deny_write_paths,
            )
        })
        .await
        .map_err(|err| anyhow::anyhow!("legacy spawn preparation task failed: {err}"))??
    };

    let (writer_tx, writer_rx) = mpsc::channel::<Vec<u8>>(128);
    let (stdout_tx, stdout_rx) = broadcast::channel::<Vec<u8>>(256);
    let stderr_rx = if tty {
        None
    } else {
        Some(broadcast::channel::<Vec<u8>>(256))
    };
    let (exit_tx, exit_rx) = oneshot::channel::<i32>();

    // Starting the process (its desktop, console and pipes) runs off the
    // async thread too.
    let (spawned, integrity_lease) = {
        let command = command.clone();
        let cwd = cwd.to_path_buf();
        let stderr_tx = stderr_rx.as_ref().map(|(tx, _rx)| tx.clone());
        let logs_base_dir = common.logs_base_dir.clone();
        tokio::task::spawn_blocking(move || {
            match spawn_legacy_process(
                h_token,
                &command,
                &cwd,
                &env_map,
                use_private_desktop,
                tty,
                stdin_open,
                stdout_tx,
                stderr_tx,
                writer_rx,
                logs_base_dir.as_deref(),
            ) {
                Ok(handles) => (Ok(handles), integrity_lease),
                Err(err) => {
                    unsafe {
                        CloseHandle(h_token);
                    }
                    // Ending the watch walks the writable roots again.
                    drop(integrity_lease);
                    (Err(err), None)
                }
            }
        })
        .await
        .map_err(|err| anyhow::anyhow!("legacy spawn task failed: {err}"))?
    };
    let LegacyProcessHandles {
        process: pi,
        job,
        output_readers,
        writer_handle,
        hpc,
        mut conpty_owner,
        token_handle,
        desktop,
    } = spawned?;
    let hpc_handle = hpc.map(|hpc| Arc::new(StdMutex::new(Some(hpc))));

    let process_handle = Arc::new(StdMutex::new(Some(pi.hProcess)));
    let wait_handle = Arc::clone(&process_handle);
    let job_for_wait = Arc::clone(&job);
    let command_for_wait = command.clone();
    let hpc_for_wait = hpc_handle.clone();
    let wait_logs_base_dir = common.logs_base_dir.clone();
    std::thread::spawn(move || {
        let _desktop = desktop;
        let timeout = timeout_ms.map(|ms| ms as u32).unwrap_or(INFINITE);
        let wait_res = unsafe { WaitForSingleObject(pi.hProcess, timeout) };
        let mut descendants_preserved = false;
        if wait_res == WAIT_TIMEOUT {
            terminate_job_or_process(&job_for_wait, &wait_handle, wait_logs_base_dir.as_deref());
        } else if let Err(err) = job_for_wait.preserve_descendants() {
            log_note(
                &format!("legacy spawn failed to preserve descendants after root exit: {err}"),
                wait_logs_base_dir.as_deref(),
            );
        } else {
            descendants_preserved = true;
        }
        if let Some(hpc) = hpc_for_wait
            && let Ok(mut guard) = hpc.lock()
        {
            let _ = guard.take();
        }
        drop(conpty_owner.take());
        unsafe {
            if token_handle != 0 && token_handle != INVALID_HANDLE_VALUE {
                CloseHandle(token_handle);
            }
        }
        // Labels are put back before the exit is reported, unless processes
        // the command left running still use them.
        if let Some(lease) = integrity_lease {
            lease.release_after_job(Arc::clone(&job_for_wait), descendants_preserved);
        }
        finalize_exit(
            exit_tx,
            wait_handle,
            pi.hThread,
            output_readers,
            wait_logs_base_dir.as_deref(),
            command_for_wait,
        );
    });

    let terminator = {
        let job = Arc::clone(&job);
        let process_handle = Arc::clone(&process_handle);
        let logs_base_dir = common.logs_base_dir;
        Some(Box::new(move || {
            terminate_job_or_process(&job, &process_handle, logs_base_dir.as_deref());
        }) as Box<dyn FnMut() + Send + Sync>)
    };

    let driver = ProcessDriver {
        writer_tx,
        stdout_rx,
        stderr_rx: stderr_rx.map(|(_tx, rx)| rx),
        exit_rx,
        terminator,
        writer_handle: Some(writer_handle),
        resizer: hpc_handle.map(|hpc| {
            Box::new(move |size| resize_conpty_handle(&hpc, size))
                as Box<dyn FnMut(TerminalSize) -> Result<()> + Send>
        }),
        tty,
    };

    Ok(finish_driver_spawn(driver, stdin_open))
}
