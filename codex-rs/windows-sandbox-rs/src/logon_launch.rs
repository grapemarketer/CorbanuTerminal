//! Starts a process as the elevated sandbox's user with
//! `CreateProcessWithLogonW` (the secondary logon service).
//!
//! #295: the service opens the caller's process as the caller and, from a
//! normal session, needs `PROCESS_QUERY_INFORMATION`, `PROCESS_DUP_HANDLE`
//! and `PROCESS_CREATE_PROCESS` on it (measured on Windows 11 26200). Core's
//! protected process DACL (secretless agent launch) grants the user none of
//! them, and granting them would undo it: `PROCESS_DUP_HANDLE` amounts to full
//! access. So when Core's DACL is protected and the call is refused with
//! `ERROR_ACCESS_DENIED`, Core starts the command runner as itself in
//! launcher mode ([`LOGON_LAUNCH_ARG`]), an ordinary short-lived process that
//! makes the call, and duplicates the new process's handle out of it.
//!
//! The launcher inherits only its own two pipes and an environment holding
//! just `SystemRoot`. The pipe ends are never inheritable in Core's own
//! handle table (#307), where any process Core started on another thread
//! meanwhile (`std::process::Command` always inherits) would get them too:
//! they are duplicated, inheritable, into a holder process that never runs
//! (`codex_process_hardening::HandleHolder`, shared with `spawn_protected`),
//! and the launcher is started as the holder's child; Core starts nothing
//! else from the holder. (Like the launcher, the holder has the default DACL,
//! so this user's other processes could open it; the sandbox's users cannot.)
//! The launcher receives
//! the sandbox user's password, which is not a secret from this user (it is
//! stored under `.sandbox-secrets`, which this user can read). Its binary is
//! in the sandbox's helper directory, which the sandbox's users can only read
//! and execute. Core checks that the returned handle is the process the
//! launcher named, and the launcher ends that process unless Core
//! acknowledges it.
//!
//! #345: outside the interactive window station (an SSH session, a service)
//! the new process starts suspended, and Core gives its logon SID access to
//! this window station and desktop (`crate::window_station`) before resuming
//! it.

use crate::proc_thread_attr::ProcThreadAttributeList;
use crate::window_station::WindowAccess;
use crate::window_station::WindowAccessState;
use crate::window_station::current_window_station_name;
use crate::window_station::is_interactive_window_station;
use crate::window_station::process_logon_sid;
use crate::winutil::quote_windows_arg;
use crate::winutil::to_wide;
use anyhow::Context;
use codex_process_hardening::HandleHolder;
use serde::Deserialize;
use serde::Serialize;
use std::ffi::c_void;
use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::os::windows::io::FromRawHandle;
use std::path::Path;
use std::path::PathBuf;
use std::ptr;
use std::sync::mpsc;
use std::time::Duration;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::DUPLICATE_SAME_ACCESS;
use windows_sys::Win32::Foundation::DuplicateHandle;
use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Foundation::HLOCAL;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::ACL;
use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows_sys::Win32::Security::Authorization::GetSecurityInfo;
use windows_sys::Win32::Security::Authorization::SDDL_REVISION_1;
use windows_sys::Win32::Security::Authorization::SE_KERNEL_OBJECT;
use windows_sys::Win32::Security::Authorization::SetSecurityInfo;
use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
use windows_sys::Win32::Security::GetSecurityDescriptorControl;
use windows_sys::Win32::Security::GetSecurityDescriptorDacl;
use windows_sys::Win32::Security::PROTECTED_DACL_SECURITY_INFORMATION;
use windows_sys::Win32::Security::PSECURITY_DESCRIPTOR;
use windows_sys::Win32::Security::SE_DACL_PROTECTED;
use windows_sys::Win32::Security::TOKEN_ADJUST_DEFAULT;
use windows_sys::Win32::Security::TOKEN_QUERY;
use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
use windows_sys::Win32::System::Diagnostics::Debug::SetErrorMode;
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::CREATE_BREAKAWAY_FROM_JOB;
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;
use windows_sys::Win32::System::Threading::CREATE_UNICODE_ENVIRONMENT;
use windows_sys::Win32::System::Threading::CreateProcessW;
use windows_sys::Win32::System::Threading::CreateProcessWithLogonW;
use windows_sys::Win32::System::Threading::DETACHED_PROCESS;
use windows_sys::Win32::System::Threading::EXTENDED_STARTUPINFO_PRESENT;
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::System::Threading::GetProcessId;
use windows_sys::Win32::System::Threading::INFINITE;
use windows_sys::Win32::System::Threading::OpenProcessToken;
use windows_sys::Win32::System::Threading::PROCESS_INFORMATION;
use windows_sys::Win32::System::Threading::ResumeThread;
use windows_sys::Win32::System::Threading::STARTF_USESTDHANDLES;
use windows_sys::Win32::System::Threading::STARTUPINFOEXW;
use windows_sys::Win32::System::Threading::STARTUPINFOW;
use windows_sys::Win32::System::Threading::TerminateProcess;
use windows_sys::Win32::System::Threading::WaitForSingleObject;

/// No error dialogs from the new process (`SEM_FAILCRITICALERRORS |
/// SEM_NOGPFAULTERRORBOX`).
const ERROR_MODE_FLAGS: u32 = 0x0001 | 0x0002;

/// The command runner's first argument for [`run_logon_launcher`].
pub const LOGON_LAUNCH_ARG: &str = "--logon-launch";

/// How long Core waits for the launcher's reply, and for it to exit.
const LAUNCHER_REPLY_TIMEOUT: Duration = Duration::from_secs(30);
const LAUNCHER_EXIT_TIMEOUT_MS: u32 = 5_000;
/// Core's acknowledgement: it holds its own handle to the started process.
const ACK: &str = "ack";

#[derive(Serialize, Deserialize)]
struct LauncherRequest {
    username: String,
    password: String,
    application: PathBuf,
    command_line: String,
    cwd: PathBuf,
    /// Start it suspended and hand Core its main thread too (#345).
    #[serde(default)]
    suspended: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LauncherReply {
    /// `process` and `thread` (when suspended, else 0) are handle values in
    /// the launcher's handle table.
    Started {
        pid: u32,
        process: usize,
        #[serde(default)]
        thread: usize,
    },
    Failed {
        code: u32,
    },
}

/// `CreateProcessWithLogonW` failed with this Win32 error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogonError {
    pub code: u32,
}

impl std::fmt::Display for LogonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CreateProcessWithLogonW failed: {}", self.code)
    }
}

impl std::error::Error for LogonError {}

/// What to start, as which local user. The new process gets the user's own
/// environment and no window.
pub struct LogonLaunchRequest<'a> {
    pub username: &'a str,
    pub password: &'a str,
    pub application: &'a Path,
    pub command_line: &'a str,
    pub cwd: &'a Path,
}

/// A started process. The caller owns `process` and must close it.
#[derive(Debug)]
pub struct LaunchedProcess {
    pub pid: u32,
    pub process: HANDLE,
    /// Started through the launcher (see the module docs).
    pub via_launcher: bool,
    /// The new process's logon's access to this process's non-interactive
    /// window station and desktop (#341, #345); removed when dropped.
    pub window_access: Option<WindowAccess>,
    /// Why the new process's logon could not be given that access, if it
    /// could not.
    pub window_access_error: Option<String>,
    /// Why its default DACL could not be restricted before it ran (D324,
    /// #1721), if it could not; it restricts it itself first thing.
    pub default_dacl_error: Option<String>,
}

/// A process just started, still suspended if `thread` is not 0.
struct Started {
    pid: u32,
    process: HANDLE,
    thread: HANDLE,
    via_launcher: bool,
}

/// Starts `request`. When this process's DACL is protected and the secondary
/// logon service refuses it, `launcher_exe` (the command runner) makes the
/// call instead; see the module docs.
///
/// The new process starts suspended. D324 (#1648, #1721): before it runs,
/// its process and main thread get a DACL only SYSTEM may open (Core keeps
/// its handles), and, where this process may open its token for it, the
/// default DACL the runner gives itself
/// ([`crate::token::restrict_current_default_dacl`]). The logon's DACLs give
/// the sandbox account full access, and every elevated command's token holds
/// that account as a restricting SID, so until then any of them could open
/// the runner (unrestricted) for writing. The runner restricts them itself
/// first thing, and lets the sandbox account query its process again. What
/// remains is the moment between the service creating the process and this
/// call.
///
/// Outside the interactive window station (an SSH session, a service), its
/// logon SID also gets access to this process's window station and desktop,
/// which it starts on, before it runs (#341, #345). If the access can't be
/// given, it runs anyway (the window station may admit it), and the error
/// is in [`LaunchedProcess::window_access_error`].
pub fn create_process_with_logon(
    request: &LogonLaunchRequest<'_>,
    launcher_exe: &Path,
) -> anyhow::Result<LaunchedProcess> {
    let started = create_process_with_logon_any(request, launcher_exe, /*suspended*/ true)?;
    let mut launched = LaunchedProcess {
        pid: started.pid,
        process: started.process,
        via_launcher: started.via_launcher,
        window_access: None,
        window_access_error: None,
        default_dacl_error: None,
    };
    // Ends the process if anything below fails or panics before it runs.
    let suspended = Suspended {
        process: launched.process,
        thread: started.thread,
    };
    match protect_before_start(started.process, started.thread) {
        Ok(error) => launched.default_dacl_error = error,
        Err(err) => {
            drop(suspended);
            // SAFETY: the process was ended; the handle is not used again.
            unsafe { CloseHandle(launched.process) };
            return Err(err);
        }
    }
    if !current_window_station_name().is_some_and(|name| is_interactive_window_station(&name)) {
        match process_logon_sid(started.process).and_then(|sid| WindowAccess::grant_runner(&sid)) {
            Ok(access) => launched.window_access = access,
            Err(err) => launched.window_access_error = Some(format!("{err:#}")),
        }
    }
    if let Err(err) = suspended.resume() {
        // SAFETY: the process was ended; the handle is not used again.
        unsafe { CloseHandle(launched.process) };
        return Err(err).context("resume the started process");
    }
    Ok(launched)
}

/// The DACL a suspended runner's process and main thread get from Core:
/// SYSTEM only, and `READ_CONTROL` for `OWNER RIGHTS` (the sandbox account
/// owns them).
const STARTING_RUNNER_DACL: &str = "D:P(A;;GA;;;SY)(A;;RC;;;OW)";

/// See [`create_process_with_logon`]: protects a suspended `process` and its
/// main `thread`. Returns why its default DACL stays the logon's until it
/// runs, if it does.
fn protect_before_start(process: HANDLE, thread: HANDLE) -> anyhow::Result<Option<String>> {
    if thread == 0 {
        anyhow::bail!("the runner was not started suspended");
    }
    set_kernel_object_dacl(process, STARTING_RUNNER_DACL)
        .context("protect the runner's process before it runs")?;
    set_kernel_object_dacl(thread, STARTING_RUNNER_DACL)
        .context("protect the runner's main thread before it runs")?;
    let mut token: HANDLE = 0;
    // SAFETY: `process` is a live process handle; the token is closed below.
    unsafe {
        if OpenProcessToken(process, TOKEN_QUERY | TOKEN_ADJUST_DEFAULT, &mut token) == 0 {
            return Ok(Some(format!("OpenProcessToken failed: {}", GetLastError())));
        }
        let result = crate::token::restrict_runner_default_dacl(token);
        CloseHandle(token);
        Ok(result.err().map(|err| format!("{err:#}")))
    }
}

/// Replaces kernel object `handle`'s DACL with `sddl`'s, protected from
/// inheritance.
fn set_kernel_object_dacl(handle: HANDLE, sddl: &str) -> anyhow::Result<()> {
    let wide = to_wide(sddl);
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `wide` is NUL-terminated; the descriptor is freed below.
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        ) == 0
        {
            anyhow::bail!("parse {sddl}: {}", GetLastError());
        }
        let mut present = 0;
        let mut defaulted = 0;
        let mut dacl: *mut ACL = ptr::null_mut();
        let status = if GetSecurityDescriptorDacl(
            descriptor,
            &mut present,
            &mut dacl,
            &mut defaulted,
        ) == 0
        {
            GetLastError()
        } else {
            SetSecurityInfo(
                handle,
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                dacl,
                ptr::null(),
            )
        };
        LocalFree(descriptor as HLOCAL);
        if status != ERROR_SUCCESS {
            anyhow::bail!("SetSecurityInfo failed: {status}");
        }
    }
    Ok(())
}

/// A process started suspended: ended on drop unless [`Suspended::resume`]
/// succeeded. Closes the thread handle either way.
struct Suspended {
    process: HANDLE,
    thread: HANDLE,
}

impl Suspended {
    fn resume(self) -> std::io::Result<()> {
        // SAFETY: `self.thread` is the process's suspended main thread.
        if unsafe { ResumeThread(self.thread) } == u32::MAX {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: returned with the process and not used again.
        unsafe { CloseHandle(self.thread) };
        std::mem::forget(self);
        Ok(())
    }
}

impl Drop for Suspended {
    fn drop(&mut self) {
        // SAFETY: the process never ran; both handles are live.
        unsafe {
            TerminateProcess(self.process, 1);
            CloseHandle(self.thread);
        }
    }
}

/// Tests: always start through the launcher, as a hardened Core at medium
/// integrity does.
#[cfg(test)]
pub(crate) static FORCE_LAUNCHER: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn create_process_with_logon_any(
    request: &LogonLaunchRequest<'_>,
    launcher_exe: &Path,
    suspended: bool,
) -> anyhow::Result<Started> {
    #[cfg(test)]
    if FORCE_LAUNCHER.load(std::sync::atomic::Ordering::SeqCst) {
        return create_process_with_logon_via_launcher(request, launcher_exe, suspended);
    }
    match create_process_with_logon_here(request, suspended) {
        // Only an absolute path to the installed runner: a bare name would be
        // looked up in the working directory (the workspace), and the launcher
        // runs as the real user. A runner next to Core's own executable is
        // trusted like Core itself.
        Err(LogonError {
            code: ERROR_ACCESS_DENIED,
        }) if current_process_dacl_is_protected()
            && launcher_exe.is_absolute()
            && launcher_exe.is_file() =>
        {
            create_process_with_logon_via_launcher(request, launcher_exe, suspended)
        }
        result => result.map_err(Into::into),
    }
}

/// True once this process's DACL is protected (process hardening).
fn current_process_dacl_is_protected() -> bool {
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: queries this process's DACL into a buffer freed below.
    let status = unsafe {
        GetSecurityInfo(
            GetCurrentProcess(),
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return false;
    }
    let mut control = 0;
    let mut revision = 0;
    // SAFETY: `descriptor` was returned by GetSecurityInfo above.
    let read = unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) };
    // SAFETY: allocated by GetSecurityInfo.
    unsafe { LocalFree(descriptor as HLOCAL) };
    read != 0 && control & SE_DACL_PROTECTED != 0
}

/// A Windows handle closed on drop.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: owned by this value.
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// A pipe whose `child` end the launcher gets. Neither end is inheritable
/// here.
fn launcher_pipe(child_reads: bool) -> anyhow::Result<(OwnedHandle, File)> {
    let (mut read, mut write) = (0, 0);
    // SAFETY: creates an anonymous pipe; both ends are owned below.
    if unsafe { CreatePipe(&mut read, &mut write, ptr::null(), 0) } == 0 {
        return Err(std::io::Error::last_os_error()).context("create launcher pipe");
    }
    let (child, parent) = if child_reads {
        (read, write)
    } else {
        (write, read)
    };
    let child = OwnedHandle(child);
    // SAFETY: `parent` is a fresh pipe end owned by the returned File.
    let parent = unsafe { File::from_raw_handle(parent as _) };
    Ok((child, parent))
}

/// Starts the launcher with only its two pipe ends inherited.
fn spawn_launcher(launcher_exe: &Path) -> anyhow::Result<(OwnedHandle, File, BufReader<File>)> {
    spawn_helper(launcher_exe, LOGON_LAUNCH_ARG, /*outlive_jobs*/ false)
}

/// Starts `exe` (the command runner) as this user in the helper mode `arg`,
/// inheriting only its own two pipes; see the module docs. With
/// `outlive_jobs`, outside any job Core is in, when that job allows it (so
/// ending Core's job does not end it).
fn spawn_helper(
    exe: &Path,
    arg: &str,
    outlive_jobs: bool,
) -> anyhow::Result<(OwnedHandle, File, BufReader<File>)> {
    let launcher_exe = exe;
    let (child_stdin, stdin) = launcher_pipe(/*child_reads*/ true)?;
    let (child_stdout, stdout) = launcher_pipe(/*child_reads*/ false)?;
    let mut environment = Vec::new();
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        let mut entry = std::ffi::OsString::from("SystemRoot=");
        entry.push(system_root);
        environment.extend(to_wide(entry));
    }
    environment.push(0);
    let mut command_line = to_wide(format!(
        "{} {arg}",
        quote_windows_arg(&launcher_exe.to_string_lossy())
    ));
    let application = to_wide(launcher_exe);
    // #307: the pipe ends are inheritable only in a holder's table, and the
    // launcher is started as its child.
    let holder = HandleHolder::start(launcher_exe).context("start the launcher's handle holder")?;
    let held_stdin = holder
        .hold(child_stdin.0)
        .context("hand a launcher pipe to the holder")?;
    let held_stdout = holder
        .hold(child_stdout.0)
        .context("hand a launcher pipe to the holder")?;
    drop((child_stdin, child_stdout));
    let mut attributes = ProcThreadAttributeList::new(/*attr_count*/ 2)?;
    attributes.set_parent_process(holder.raw())?;
    attributes.set_handle_list(vec![held_stdin, held_stdout])?;
    // SAFETY: zeroed POD with its size set, as the API requires.
    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    // Values in the holder's table, as the parent-process attribute requires.
    startup.StartupInfo.hStdInput = held_stdin;
    startup.StartupInfo.hStdOutput = held_stdout;
    // No stderr: nothing but the reply may reach the reply pipe.
    startup.StartupInfo.hStdError = 0;
    startup.lpAttributeList = attributes.as_mut_ptr();
    // Never the workspace: the launcher runs in its own directory.
    let cwd = launcher_exe.parent().map(to_wide);
    // SAFETY: zeroed POD filled in by the call.
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: every pointer refers to a live buffer above; the launcher
    // inherits from the holder, and only the two pipe ends in the list.
    let create = |flags: u32, command_line: &mut Vec<u16>, info: &mut PROCESS_INFORMATION| unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            /*binherithandles*/ 1,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT | flags,
            environment.as_ptr().cast(),
            cwd.as_ref().map_or(ptr::null(), Vec::as_ptr),
            &startup.StartupInfo,
            info,
        )
    };
    let mut ok = if outlive_jobs {
        create(CREATE_BREAKAWAY_FROM_JOB, &mut command_line, &mut info)
    } else {
        0
    };
    // A job that doesn't allow breaking away refuses it.
    // SAFETY: read right after the call.
    if !outlive_jobs || (ok == 0 && unsafe { GetLastError() } == ERROR_ACCESS_DENIED) {
        ok = create(0, &mut command_line, &mut info);
    }
    if ok == 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("start logon launcher {}", launcher_exe.display()));
    }
    drop(holder);
    // SAFETY: returned by the call above and not used again.
    unsafe { CloseHandle(info.hThread) };
    Ok((OwnedHandle(info.hProcess), stdin, BufReader::new(stdout)))
}

fn create_process_with_logon_via_launcher(
    request: &LogonLaunchRequest<'_>,
    launcher_exe: &Path,
    suspended: bool,
) -> anyhow::Result<Started> {
    let (launcher, mut stdin, mut stdout) = spawn_launcher(launcher_exe)?;
    let result = (|| -> anyhow::Result<Started> {
        let mut line = serde_json::to_vec(&LauncherRequest {
            username: request.username.to_string(),
            password: request.password.to_string(),
            application: request.application.to_path_buf(),
            command_line: request.command_line.to_string(),
            cwd: request.cwd.to_path_buf(),
            suspended,
        })?;
        line.push(b'\n');
        stdin.write_all(&line).context("send launch request")?;
        stdin.flush().context("send launch request")?;
        let (reply_tx, reply_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reply = String::new();
            let read = stdout.read_line(&mut reply).map(|_| reply);
            let _ = reply_tx.send(read);
        });
        let reply = match reply_rx.recv_timeout(LAUNCHER_REPLY_TIMEOUT) {
            Ok(reply) => reply.context("read launcher reply")?,
            Err(mpsc::RecvTimeoutError::Timeout) => anyhow::bail!("logon launcher did not reply"),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("logon launcher exited without replying")
            }
        };
        match serde_json::from_str(&reply).context("parse launcher reply")? {
            LauncherReply::Failed { code } => Err(LogonError { code }.into()),
            LauncherReply::Started {
                pid,
                process,
                thread,
            } => {
                let local = duplicate_from(&launcher, process)
                    .context("duplicate the started process's handle from the launcher")?;
                // SAFETY: `local.0` is a live process handle.
                if unsafe { GetProcessId(local.0) } != pid {
                    anyhow::bail!("logon launcher returned a handle to another process");
                }
                let local_thread = match (suspended, thread) {
                    (false, _) => OwnedHandle(0),
                    (true, 0) => {
                        // SAFETY: `local.0` is the started process, checked above.
                        unsafe { TerminateProcess(local.0, 1) };
                        anyhow::bail!("logon launcher did not return the suspended thread");
                    }
                    (true, thread) => match duplicate_from(&launcher, thread) {
                        Ok(thread) => thread,
                        Err(err) => {
                            // SAFETY: as above.
                            unsafe { TerminateProcess(local.0, 1) };
                            return Err(err).context(
                                "duplicate the started process's thread from the launcher",
                            );
                        }
                    },
                };
                if let Err(err) = writeln!(stdin, "{ACK}").and_then(|()| stdin.flush()) {
                    // The launcher is gone and cannot end it; Core can.
                    // SAFETY: `local.0` is the started process, checked above.
                    unsafe { TerminateProcess(local.0, 1) };
                    return Err(err).context("acknowledge the started process");
                }
                let process = local.0;
                std::mem::forget(local);
                let thread = local_thread.0;
                std::mem::forget(local_thread);
                Ok(Started {
                    pid,
                    process,
                    thread,
                    via_launcher: true,
                })
            }
        }
    })();
    drop(stdin);
    // SAFETY: `launcher.0` is a live process handle.
    unsafe {
        if WaitForSingleObject(launcher.0, LAUNCHER_EXIT_TIMEOUT_MS) != 0 {
            TerminateProcess(launcher.0, 1);
        }
    }
    result
}

/// The command runner's first argument for [`run_window_access_reaper`].
pub const WINDOW_ACCESS_REAPER_ARG: &str = "--window-access-reaper";

/// What Core hands a reaper: the runner (a handle value in the reaper's
/// table, `SYNCHRONIZE` only) and the access its logon holds.
#[derive(Serialize, Deserialize)]
struct ReaperRequest {
    runner: usize,
    access: WindowAccessState,
    /// Where the reaper logs edits that failed or went unlocked.
    #[serde(default)]
    log_dir: Option<PathBuf>,
}

/// #345: a runner's window access, held by a reaper process instead of Core
/// so that it goes when the runner exits even if Core has exited first.
///
/// The reaper is the command runner started as this user in reaper mode
/// ([`WINDOW_ACCESS_REAPER_ARG`]), like the logon launcher: only its own two
/// pipes, almost no environment, its own directory. It waits for the runner
/// and then removes the entries; Core tells it to narrow them once the
/// runner reports its command started.
pub(crate) struct WindowAccessReaper {
    stdin: File,
}

impl WindowAccessReaper {
    /// Hands `access` for `runner` to a new reaper. On failure Core keeps it.
    pub(crate) fn start(
        access: WindowAccess,
        runner: HANDLE,
        reaper_exe: &Path,
    ) -> Result<Self, (WindowAccess, anyhow::Error)> {
        match Self::hand_over(&access, runner, reaper_exe) {
            Ok(reaper) => {
                access.disarm();
                Ok(reaper)
            }
            Err(err) => Err((access, err)),
        }
    }

    fn hand_over(access: &WindowAccess, runner: HANDLE, reaper_exe: &Path) -> anyhow::Result<Self> {
        if !reaper_exe.is_absolute() || !reaper_exe.is_file() {
            anyhow::bail!("no runner at {} to reap with", reaper_exe.display());
        }
        let (reaper, mut stdin, mut stdout) = spawn_helper(
            reaper_exe,
            WINDOW_ACCESS_REAPER_ARG,
            /*outlive_jobs*/ true,
        )?;
        let result = (|| -> anyhow::Result<()> {
            let mut remote: HANDLE = 0;
            // SAFETY: `runner` is a live process handle; `reaper.0` is our own
            // child, opened with full access.
            if unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    runner,
                    reaper.0,
                    &mut remote,
                    SYNCHRONIZE,
                    /*binherithandle*/ 0,
                    /*dwoptions*/ 0,
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error())
                    .context("hand the runner's handle to the reaper");
            }
            let mut line = serde_json::to_vec(&ReaperRequest {
                runner: remote as usize,
                access: access.state().clone(),
                log_dir: access.log_dir().map(Path::to_path_buf),
            })?;
            line.push(b'\n');
            stdin
                .write_all(&line)
                .context("send the reaper its request")?;
            stdin.flush().context("send the reaper its request")?;
            let (reply_tx, reply_rx) = mpsc::channel();
            std::thread::spawn(move || {
                let mut reply = String::new();
                let read = stdout.read_line(&mut reply).map(|_| reply);
                let _ = reply_tx.send(read);
            });
            match reply_rx.recv_timeout(LAUNCHER_REPLY_TIMEOUT) {
                Ok(Ok(reply)) if reply.trim() == ACK => Ok(()),
                Ok(Ok(reply)) => anyhow::bail!("the reaper replied {:?}", reply.trim()),
                Ok(Err(err)) => Err(err).context("read the reaper's reply"),
                Err(_) => anyhow::bail!("the reaper did not reply"),
            }
        })();
        if result.is_err() {
            // SAFETY: our own child; it holds nothing yet.
            unsafe { TerminateProcess(reaper.0, 1) };
        }
        result.map(|()| Self { stdin })
    }

    /// See [`WindowAccess::narrow_for_commands`].
    pub(crate) fn narrow_for_commands(
        &mut self,
        commands_use_this_desktop: bool,
    ) -> anyhow::Result<()> {
        let line = if commands_use_this_desktop {
            "narrow shared"
        } else {
            "narrow private"
        };
        writeln!(self.stdin, "{line}")
            .and_then(|()| self.stdin.flush())
            .context("tell the reaper to narrow the access")
    }
}

/// The command runner's reaper mode: takes over a runner's window access
/// from Core (one request on stdin, acknowledged on stdout), narrows it when
/// Core says so, and removes it once the runner exits, whether or not Core
/// is still running.
pub fn run_window_access_reaper() -> anyhow::Result<()> {
    let mut stdin = std::io::stdin().lock();
    let mut line = String::new();
    stdin
        .read_line(&mut line)
        .context("read the reaper request")?;
    let request: ReaperRequest = serde_json::from_str(&line).context("parse the reaper request")?;
    let runner = OwnedHandle(request.runner as HANDLE);
    let log_dir = request.log_dir;
    let mut reaped = WindowAccess::from_state(request.access);
    reaped.set_log_dir(log_dir.as_deref());
    let access = std::sync::Arc::new(std::sync::Mutex::new(Some(reaped)));
    {
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{ACK}")?;
        stdout.flush()?;
    }
    drop(stdin);
    let narrowing = std::sync::Arc::clone(&access);
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else {
                break;
            };
            let shared = match line.trim() {
                "narrow shared" => true,
                "narrow private" => false,
                _ => continue,
            };
            let mut access = narrowing
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(access) = access.as_mut()
                && let Err(err) = access.narrow_for_commands(shared)
            {
                crate::logging::log_note(
                    &format!("reaper: could not narrow the runner's window access: {err:#}"),
                    log_dir.as_deref(),
                );
            }
        }
    });
    // SAFETY: `runner` is the handle Core gave this process.
    unsafe { WaitForSingleObject(runner.0, INFINITE) };
    drop(
        access
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take(),
    );
    // The stdin thread may still be blocked reading.
    std::process::exit(0);
}

/// Duplicates `handle`, a handle value in `launcher`'s table, into this
/// process with the same access.
fn duplicate_from(launcher: &OwnedHandle, handle: usize) -> anyhow::Result<OwnedHandle> {
    let mut local: HANDLE = 0;
    // SAFETY: `launcher` is our own child, opened with full access; `handle`
    // is a handle in its table that it keeps open until it reads our
    // acknowledgement.
    let ok = unsafe {
        DuplicateHandle(
            launcher.0,
            handle as HANDLE,
            GetCurrentProcess(),
            &mut local,
            /*dwdesiredaccess*/ 0,
            /*binherithandle*/ 0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(OwnedHandle(local))
}

/// The command runner's launcher mode: reads one launch request from stdin,
/// starts it with `CreateProcessWithLogonW`, writes the reply to stdout, and
/// keeps the new process's handle open until Core acknowledges it. Without
/// that acknowledgement (Core failed or went away) it ends the new process.
pub fn run_logon_launcher() -> anyhow::Result<()> {
    let mut stdin = std::io::stdin().lock();
    let mut line = String::new();
    stdin.read_line(&mut line).context("read launch request")?;
    let request: LauncherRequest = serde_json::from_str(&line).context("parse launch request")?;
    let result = create_process_with_logon_here(
        &LogonLaunchRequest {
            username: &request.username,
            password: &request.password,
            application: &request.application,
            command_line: &request.command_line,
            cwd: &request.cwd,
        },
        request.suspended,
    );
    let reply = match &result {
        Ok(launched) => LauncherReply::Started {
            pid: launched.pid,
            process: launched.process as usize,
            thread: launched.thread as usize,
        },
        Err(err) => LauncherReply::Failed { code: err.code },
    };
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &reply)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    let Ok(launched) = result else {
        return Ok(());
    };
    let mut ack = String::new();
    let acknowledged = stdin.read_line(&mut ack).is_ok() && ack.trim() == ACK;
    // SAFETY: returned by CreateProcessWithLogonW and not used after this.
    unsafe {
        if !acknowledged {
            TerminateProcess(launched.process, 1);
        }
        CloseHandle(launched.process);
        if launched.thread != 0 {
            CloseHandle(launched.thread);
        }
    }
    Ok(())
}

/// Calls `CreateProcessWithLogonW` from this process. When `suspended`, the
/// new process's main thread is returned too, not yet running.
fn create_process_with_logon_here(
    request: &LogonLaunchRequest<'_>,
    suspended: bool,
) -> Result<Started, LogonError> {
    let user = to_wide(request.username);
    let domain = to_wide(".");
    let password = to_wide(request.password);
    let application = to_wide(request.application);
    let mut command_line = to_wide(request.command_line);
    let cwd = to_wide(request.cwd);
    // SAFETY: zeroed POD with its size set, as the API requires.
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    // SAFETY: zeroed POD filled in by the call.
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: only changes this process's error mode; restored below.
    let previous_error_mode = unsafe { SetErrorMode(ERROR_MODE_FLAGS) };
    // SAFETY: every pointer refers to a live, NUL-terminated buffer above.
    let ok = unsafe {
        CreateProcessWithLogonW(
            user.as_ptr(),
            domain.as_ptr(),
            password.as_ptr(),
            /*dwlogonflags*/ 0,
            application.as_ptr(),
            command_line.as_mut_ptr(),
            // D324 (#1721): no console, so no console host, which the runner
            // would start with the logon's default DACL before it runs.
            DETACHED_PROCESS
                | CREATE_UNICODE_ENVIRONMENT
                | if suspended { CREATE_SUSPENDED } else { 0 },
            ptr::null::<c_void>(),
            cwd.as_ptr(),
            &startup,
            &mut info,
        )
    };
    // SAFETY: read right after the call, before anything else can set it.
    let code = unsafe { GetLastError() };
    // SAFETY: as above.
    unsafe { SetErrorMode(previous_error_mode) };
    if ok == 0 {
        return Err(LogonError { code });
    }
    let thread = if suspended {
        info.hThread
    } else {
        if info.hThread != 0 {
            // SAFETY: returned by the call above and not used again.
            unsafe { CloseHandle(info.hThread) };
        }
        0
    };
    Ok(Started {
        pid: info.dwProcessId,
        process: info.hProcess,
        thread,
        via_launcher: false,
    })
}

#[cfg(test)]
#[path = "logon_launch_tests.rs"]
mod tests;
