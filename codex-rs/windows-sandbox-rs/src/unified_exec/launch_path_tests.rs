//! #1507: process-creation paths other than `CreateProcess` that could start
//! a legacy command's child outside the command's job, or under a token
//! other than its Low-integrity restricted one. #733 made the job refuse
//! breakaway; these routes don't ask the job.
//!
//! Each test runs a launcher in a legacy session that tries one route to
//! start [`launch_path_report_entry`], which writes where it ended up. A
//! route keeps containment when Windows refuses the launch or when the
//! report shows the child in the launcher's job, at Low integrity, with a
//! restricted token.
//!
//! Measured on the hosted Windows runner (elevated and in a normal session;
//! no Explorer running), #1507: WMI, the Task Scheduler and `ShellWindows`
//! refuse a legacy command with the errors their tests expect, and so does
//! naming another legacy session's process as the parent since D259 gave
//! each command its own identity in its default DACL (unless the launcher
//! impersonates the other command's token first, #1560).
//! `ShellBrowserWindow` and `MMC20.Application` start the child outside the
//! launcher's job; those tests are ignored until D260 is measured on a real
//! desktop.
//!
//! Refusals are matched on the step and the error code (HRESULT, or WMI's
//! status name), never on message text, which Windows localizes. Every
//! launch ends what it left running outside the sandbox, and the tests check
//! that the Task Scheduler task and the roots' label records are gone.

use super::*;
use crate::proc_thread_attr::ProcThreadAttributeList;
use crate::winutil::quote_windows_arg;
use crate::winutil::to_wide;
use pretty_assertions::assert_eq;
use windows_sys::Win32::Foundation::BOOL;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Security::IsTokenRestricted;
use windows_sys::Win32::Security::TOKEN_QUERY;
use windows_sys::Win32::System::Diagnostics::ToolHelp::CreateToolhelp32Snapshot;
use windows_sys::Win32::System::Diagnostics::ToolHelp::PROCESSENTRY32W;
use windows_sys::Win32::System::Diagnostics::ToolHelp::Process32FirstW;
use windows_sys::Win32::System::Diagnostics::ToolHelp::Process32NextW;
use windows_sys::Win32::System::Diagnostics::ToolHelp::TH32CS_SNAPPROCESS;
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::JobObjects::JOBOBJECT_BASIC_PROCESS_ID_LIST;
use windows_sys::Win32::System::JobObjects::JobObjectBasicProcessIdList;
use windows_sys::Win32::System::JobObjects::QueryInformationJobObject;
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
use windows_sys::Win32::System::Threading::CreateProcessW;
use windows_sys::Win32::System::Threading::EXTENDED_STARTUPINFO_PRESENT;
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::System::Threading::GetProcessTimes;
use windows_sys::Win32::System::Threading::OpenProcessToken;
use windows_sys::Win32::System::Threading::PROCESS_CREATE_PROCESS;
use windows_sys::Win32::System::Threading::PROCESS_INFORMATION;
use windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;
use windows_sys::Win32::System::Threading::PROCESS_TERMINATE;
use windows_sys::Win32::System::Threading::STARTUPINFOEXW;
use windows_sys::Win32::System::Threading::STARTUPINFOW;
use windows_sys::Win32::System::Threading::TerminateProcess;

const REPORT_ENTRY: &str = "unified_exec::tests::launch_paths::launch_path_report_entry";
const LAUNCHER_ENTRY: &str = "unified_exec::tests::launch_paths::launch_path_launcher_entry";
const HOLD_ENTRY: &str = "unified_exec::tests::launch_paths::launch_path_hold_entry";
/// The report entry gets its settings as extra libtest filters, because
/// brokers such as WMI start it without the caller's environment.
const REPORT_ARG: &str = "codex-1507-report=";
const LAUNCHER_ARG: &str = "codex-1507-launcher=";
/// The test process, which runs outside the session.
const HARNESS_ARG: &str = "codex-1507-harness=";
/// Replaced with the launcher's process id inside the session.
const LAUNCHER_PLACEHOLDER: &str = "@LAUNCHER@";
const LAUNCH_ENV: &str = "CODEX_1507_LAUNCH";
const PARENT_ENV: &str = "CODEX_1507_PARENT";
const COMMAND_ENV: &str = "CODEX_1507_CMD";
const EXE_ENV: &str = "CODEX_1507_EXE";
const ARGS_ENV: &str = "CODEX_1507_ARGS";
const DIR_ENV: &str = "CODEX_1507_DIR";
const TASK_ENV: &str = "CODEX_1507_TASK";
const HOLD_ENV: &str = "CODEX_1507_HOLD";
const REPORT_ENV: &str = "CODEX_1507_REPORT";
const LAUNCH_RESULT: &str = "launch: ";
const NEW_PROCESSES: &str = "new: ";
/// The start of a report from a child that stayed in the session.
const CONTAINED: &str = "in_launcher_job=true in_any_job=true low=true restricted=true";

/// What the started child saw of itself. It is in the launcher's job when
/// its own job lists the launcher but not the test process: a job lists the
/// processes of the jobs nested in it, so a job that also holds the test
/// process is one the session's job is nested in, outside the session.
fn own_containment(launcher: u32, harness: Option<u32>) -> String {
    // SAFETY: the pseudo handle needs no closing; the token handle is closed below.
    unsafe {
        let mut in_job: BOOL = 0;
        let in_any_job = IsProcessInJob(GetCurrentProcess(), 0, &mut in_job) != 0 && in_job != 0;
        // A null job handle queries the caller's innermost job.
        let mut buffer = vec![0usize; 1026];
        let in_launcher_job = QueryInformationJobObject(
            0,
            JobObjectBasicProcessIdList,
            buffer.as_mut_ptr().cast(),
            u32::try_from(buffer.len() * std::mem::size_of::<usize>()).unwrap_or(u32::MAX),
            std::ptr::null_mut(),
        ) != 0
            && {
                let list = &*(buffer.as_ptr() as *const JOBOBJECT_BASIC_PROCESS_ID_LIST);
                // The ids follow the two counts, at the second `usize`.
                let count = (list.NumberOfProcessIdsInList as usize).min(buffer.len() - 1);
                let ids = &buffer[1..=count];
                harness.is_some_and(|harness| {
                    ids.contains(&(launcher as usize)) && !ids.contains(&(harness as usize))
                })
            };
        let (low, restricted) = token_state(GetCurrentProcess());
        format!(
            "in_launcher_job={in_launcher_job} in_any_job={in_any_job} low={low} restricted={restricted}"
        )
    }
}

/// Whether `process`'s token is Low integrity and restricted.
///
/// # Safety
/// `process` must be a process handle with query access.
unsafe fn token_state(process: HANDLE) -> (bool, bool) {
    let mut token: HANDLE = 0;
    // SAFETY: per the caller; the token handle is closed below.
    unsafe {
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            return (false, false);
        }
        let state = (
            crate::integrity::token_is_low_integrity(token),
            IsTokenRestricted(token) != 0,
        );
        CloseHandle(token);
        state
    }
}

/// Where process `pid` runs, seen from outside the sandbox.
fn inspect_pid(name: &str, pid: u32) -> String {
    // SAFETY: the handle is checked and closed here.
    unsafe {
        let process = windows_sys::Win32::System::Threading::OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        );
        if process == 0 {
            return format!("{name}:{pid} gone");
        }
        let mut in_job: BOOL = 0;
        let in_any_job = IsProcessInJob(process, 0, &mut in_job) != 0 && in_job != 0;
        let (low, restricted) = token_state(process);
        CloseHandle(process);
        format!("{name}:{pid} in_any_job={in_any_job} low={low} restricted={restricted}")
    }
}

/// Programs a launch can leave running outside any job besides this test
/// binary (the report entry): the COM servers that start programs, and the
/// `rundll32` that `ShellBrowserWindow` starts the program through (measured
/// on the hosted runner, #1507).
const ESCAPE_IMAGES: &[&str] = &["mmc.exe", "explorer.exe", "dllhost.exe", "rundll32.exe"];

/// Ends what a launch left running outside the sandbox: processes started
/// since the sweep began, at Low integrity, in no job, running this test
/// binary or one of [`ESCAPE_IMAGES`]. Only a legacy command starts such
/// processes; another test's sandboxed processes are in their session's job.
/// [`EscapeSweep::finish`] reports the result; dropping the sweep (a panic)
/// still ends them.
struct EscapeSweep {
    since: u64,
    finished: bool,
}

/// What a sweep did.
#[derive(Debug, Default)]
struct SweepResult {
    ended: Vec<String>,
    /// Processes that are still running, or a failure to look.
    left: Vec<String>,
}

impl EscapeSweep {
    fn start() -> Self {
        // FILETIME counts 100 ns since 1601; the Unix epoch is this many later.
        const UNIX_EPOCH_AS_FILETIME: u64 = 116_444_736_000_000_000;
        let since_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after 1970");
        // A second early: creation times are kept at clock-tick resolution.
        const MARGIN: u64 = 10_000_000;
        let since = UNIX_EPOCH_AS_FILETIME
            + u64::try_from(since_unix.as_nanos() / 100).expect("time fits in u64")
            - MARGIN;
        Self {
            since,
            finished: false,
        }
    }

    fn finish(mut self) -> SweepResult {
        self.finished = true;
        end_escaped_processes(self.since)
    }
}

impl Drop for EscapeSweep {
    fn drop(&mut self) {
        if !self.finished {
            let _ = end_escaped_processes(self.since);
        }
    }
}

fn end_escaped_processes(since: u64) -> SweepResult {
    let test_image = std::env::current_exe().ok().and_then(|exe| {
        exe.file_name()
            .map(|name| name.to_string_lossy().to_lowercase())
    });
    let mut result = SweepResult::default();
    // SAFETY: the snapshot handle is checked and closed here; the entry is
    // sized as the API requires.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            result
                .left
                .push(format!("process snapshot failed: {}", GetLastError()));
            return result;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..len]).to_lowercase();
            if entry.th32ProcessID != std::process::id()
                && (test_image.as_deref() == Some(name.as_str())
                    || ESCAPE_IMAGES.contains(&name.as_str()))
            {
                end_if_escaped(&name, entry.th32ProcessID, since, &mut result);
            }
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
    }
    result
}

/// Ends process `pid` if it is one of the sweep's (see [`EscapeSweep`]).
///
/// # Safety
/// Calls Win32 process APIs; the handle is checked and closed here.
unsafe fn end_if_escaped(name: &str, pid: u32, since: u64, result: &mut SweepResult) {
    use windows_sys::Win32::System::Threading::OpenProcess;
    // SAFETY: per the caller.
    unsafe {
        let query = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        // Gone, or not this user's (so not a legacy command's).
        if query == 0 {
            return;
        }
        let query = OwnedHandle::from_raw_handle(query as _);
        let raw = query.as_raw_handle() as HANDLE;
        let mut created: FILETIME = std::mem::zeroed();
        let mut exited: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        let mut in_job: BOOL = 0;
        let ours = GetProcessTimes(raw, &mut created, &mut exited, &mut kernel, &mut user) != 0
            && ((u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
                >= since
            && token_state(raw).0
            && IsProcessInJob(raw, 0, &mut in_job) != 0
            && in_job == 0;
        if !ours {
            return;
        }
        let access = PROCESS_TERMINATE | windows_sys::Win32::System::Threading::PROCESS_SYNCHRONIZE;
        let process = OpenProcess(access, 0, pid);
        if process == 0 {
            result
                .left
                .push(format!("{name}:{pid} open error={}", GetLastError()));
            return;
        }
        let process = OwnedHandle::from_raw_handle(process as _);
        if TerminateProcess(process.as_raw_handle() as HANDLE, 1) == 0 {
            result
                .left
                .push(format!("{name}:{pid} terminate error={}", GetLastError()));
        } else if let Err(err) = wait_for_process_exit(&process, Duration::from_secs(/*secs*/ 10)) {
            result
                .left
                .push(format!("{name}:{pid} did not exit: {err}"));
        } else {
            result.ended.push(format!("{name}:{pid}"));
        }
    }
}

/// Whether the integrity-label registry still records `root` once its
/// legacy sessions ended: `None` when it doesn't.
fn labels_still_recorded(root: &Path) -> Option<String> {
    let canonical = match dunce::canonicalize(root) {
        Ok(canonical) => canonical,
        Err(err) => return Some(format!("canonicalize {}: {err}", root.display())),
    };
    match crate::integrity_leases::recorded_roots() {
        Ok(recorded) if !recorded.contains(&canonical) => None,
        Ok(recorded) => Some(format!("{} in {recorded:?}", canonical.display())),
        Err(err) => Some(format!("read the label records: {err:#}")),
    }
}

/// Writes [`own_containment`] to the report path in its arguments. Does
/// nothing otherwise.
#[test]
fn launch_path_report_entry() {
    let args: Vec<String> = std::env::args().collect();
    let Some(report) = args.iter().find_map(|arg| arg.strip_prefix(REPORT_ARG)) else {
        return;
    };
    let launcher = args
        .iter()
        .find_map(|arg| arg.strip_prefix(LAUNCHER_ARG))
        .and_then(|pid| pid.parse().ok())
        .unwrap_or(0);
    let harness = args
        .iter()
        .find_map(|arg| arg.strip_prefix(HARNESS_ARG))
        .and_then(|pid| pid.parse().ok());
    fs::write(report, own_containment(launcher, harness)).expect("write launch report");
}

/// Waits (up to a minute) in a second legacy session, so a launcher can try
/// to start a child under it. Does nothing otherwise.
#[test]
fn launch_path_hold_entry() {
    let Some(dir) = std::env::var_os(HOLD_ENV).map(PathBuf::from) else {
        return;
    };
    fs::write(dir.join("hold.pid"), std::process::id().to_string()).expect("write hold pid");
    wait_for_path(&dir.join("hold.release"), Duration::from_secs(/*secs*/ 60));
}

/// Starts the report entry from inside a legacy session: as a plain child
/// (the control), or with `PROC_THREAD_ATTRIBUTE_PARENT_PROCESS` naming the
/// process in `CODEX_1507_PARENT`. Does nothing otherwise.
#[test]
fn launch_path_launcher_entry() {
    let Some(mode) = std::env::var_os(LAUNCH_ENV) else {
        return;
    };
    let command = std::env::var(COMMAND_ENV)
        .expect("report command")
        .replace(LAUNCHER_PLACEHOLDER, &std::process::id().to_string());
    let parent_pid: Option<u32> = (mode == "parent").then(|| {
        std::env::var(PARENT_ENV)
            .expect("parent pid")
            .parse()
            .expect("numeric parent pid")
    });
    // SAFETY: every pointer refers to a live local; handles are closed below.
    let outcome = unsafe {
        let mut attributes = None;
        let mut parent: HANDLE = 0;
        if let Some(pid) = parent_pid {
            parent =
                windows_sys::Win32::System::Threading::OpenProcess(PROCESS_CREATE_PROCESS, 0, pid);
            if parent == 0 {
                let error = GetLastError();
                println!("\n{LAUNCH_RESULT}started=false open_parent_error={error}");
                return;
            }
            let mut list = ProcThreadAttributeList::new(/*attr_count*/ 1).expect("attribute list");
            list.set_parent_process(parent).expect("parent attribute");
            attributes = Some(list);
        }
        let mut startup: STARTUPINFOEXW = std::mem::zeroed();
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        let mut flags = CREATE_NO_WINDOW;
        if let Some(list) = attributes.as_mut() {
            startup.lpAttributeList = list.as_mut_ptr();
            flags |= EXTENDED_STARTUPINFO_PRESENT;
        } else {
            startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        }
        let mut command_line = to_wide(&command);
        let mut info: PROCESS_INFORMATION = std::mem::zeroed();
        let started = CreateProcessW(
            std::ptr::null(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            /*binherithandles*/ 0,
            flags,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::addr_of!(startup).cast::<STARTUPINFOW>(),
            &mut info,
        ) != 0;
        let error = GetLastError();
        if parent != 0 {
            CloseHandle(parent);
        }
        if started {
            let process = OwnedHandle::from_raw_handle(info.hProcess as _);
            CloseHandle(info.hThread);
            let _ = wait_for_process_exit(&process, Duration::from_secs(/*secs*/ 30));
            "started=true".to_string()
        } else {
            format!("started=false create_error={error}")
        }
    };
    println!("\n{LAUNCH_RESULT}{outcome}");
}

/// The report entry's command line, with the launcher's id still a
/// placeholder.
fn report_args(report: &Path) -> String {
    [
        "--exact".to_string(),
        REPORT_ENTRY.to_string(),
        "--nocapture".to_string(),
        quote_windows_arg(&format!("{REPORT_ARG}{}", report.display())),
        format!("{LAUNCHER_ARG}{LAUNCHER_PLACEHOLDER}"),
        format!("{HARNESS_ARG}{}", std::process::id()),
    ]
    .join(" ")
}

fn test_exe() -> String {
    std::env::current_exe()
        .expect("test exe")
        .display()
        .to_string()
}

fn report_command(report: &Path) -> String {
    format!("{} {}", quote_windows_arg(&test_exe()), report_args(report))
}

/// One launch attempt: what the launcher said and what the child reported.
struct LaunchOutcome {
    launch: Option<String>,
    report: Option<String>,
    /// Processes the launch started that weren't there before, as seen
    /// from outside the sandbox (PowerShell launches only).
    new_processes: Vec<String>,
    /// What the launch left running outside the sandbox, ended afterwards.
    sweep: SweepResult,
    /// The test root, if still recorded in the integrity-label registry
    /// after the test's sessions ended.
    labels_left: Option<String>,
    stdout: String,
    log: String,
}

impl LaunchOutcome {
    /// Contained: refused with an error that contains every part of
    /// `refusal` (the step and error measured, so a broken probe doesn't pass
    /// as a refusal), or started in the launcher's job, Low and restricted.
    fn assert_contained(&self, route: &str, refusal: &[&str]) {
        let refused = self.launch.as_deref().is_some_and(|launch| {
            launch.starts_with("started=false") && refusal.iter().all(|part| launch.contains(part))
        });
        let contained = self
            .report
            .as_deref()
            .is_some_and(|report| report.starts_with(CONTAINED));
        println!(
            "\nlaunch-path {route}: launch={:?} report={:?} new={:?} sweep={:?}",
            self.launch, self.report, self.new_processes, self.sweep
        );
        assert!(
            (refused && self.report.is_none()) || contained,
            "{route} left the session: launch={:?} report={:?} new={:?}\nstdout={:?}\n{}",
            self.launch,
            self.report,
            self.new_processes,
            self.stdout,
            self.log
        );
        self.assert_cleaned_up(route);
    }

    /// Fails if a process the launch left outside the sandbox kept running,
    /// or the test root is still recorded in the integrity-label registry.
    fn assert_cleaned_up(&self, route: &str) {
        assert!(
            self.sweep.left.is_empty() && self.labels_left.is_none(),
            "{route} did not clean up: sweep={:?} labels_left={:?}",
            self.sweep,
            self.labels_left
        );
    }
}

/// Runs `argv` in a legacy session on `root` with `env` added to this
/// process's environment, then waits for the report the launch produced.
async fn run_launch(
    root: &Path,
    codex_home: &Path,
    argv: Vec<String>,
    env: Vec<(&str, String)>,
    report: &Path,
) -> LaunchOutcome {
    let sweep = EscapeSweep::start();
    let mut env_map: HashMap<String, String> = std::env::vars().collect();
    for (key, value) in env {
        env_map.insert(key.to_string(), value);
    }
    let spawned = spawn_windows_sandbox_session_legacy(
        &PermissionProfile::workspace_write(),
        workspace_roots_for(root).as_slice(),
        codex_home,
        argv,
        root,
        env_map,
        /*timeout_ms*/ Some(90_000),
        &[],
        &[],
        /*tty*/ false,
        /*stdin_open*/ false,
        /*use_private_desktop*/ true,
    )
    .await
    .expect("spawn legacy launcher");
    let (stdout, _exit_code) =
        collect_stdout_and_exit(spawned, codex_home, Duration::from_secs(/*secs*/ 120)).await;
    let stdout = String::from_utf8_lossy(&stdout).into_owned();
    let launch = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix(LAUNCH_RESULT))
        .map(str::to_string);
    // Brokers start the child asynchronously.
    let wait = if launch
        .as_deref()
        .is_some_and(|launch| launch.starts_with("started=true"))
    {
        Duration::from_secs(/*secs*/ 30)
    } else {
        Duration::from_secs(/*secs*/ 5)
    };
    let deadline = Instant::now() + wait;
    let mut report_text = None;
    while Instant::now() < deadline {
        if let Ok(text) = fs::read_to_string(report)
            && !text.is_empty()
        {
            report_text = Some(text.trim().to_string());
            break;
        }
        tokio::time::sleep(Duration::from_millis(/*millis*/ 100)).await;
    }
    let new_processes = stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix(NEW_PROCESSES))
        .unwrap_or_default()
        .split(',')
        .filter_map(|entry| entry.rsplit_once(':'))
        .filter_map(|(name, pid)| pid.parse().ok().map(|pid| inspect_pid(name, pid)))
        .collect();
    LaunchOutcome {
        launch,
        report: report_text,
        new_processes,
        sweep: sweep.finish(),
        // Set once all of the test's sessions have ended.
        labels_left: None,
        stdout,
        log: sandbox_log(codex_home),
    }
}

fn launcher_argv() -> Vec<String> {
    vec![
        test_exe(),
        "--exact".to_string(),
        LAUNCHER_ENTRY.to_string(),
        "--nocapture".to_string(),
        "--test-threads=1".to_string(),
    ]
}

/// The control: a plain child of a legacy command stays in its job, Low and
/// restricted, and the report entry sees that.
#[test]
fn legacy_launch_plain_child_stays_in_session() {
    let _guard = legacy_process_test_guard();
    let root = user_owned_test_root();
    let runtime = current_thread_runtime();
    let mut outcome = runtime.block_on(async {
        let codex_home = sandbox_home("launch-control");
        let report = root.path().join("control.report");
        run_launch(
            root.path(),
            codex_home.path(),
            launcher_argv(),
            vec![
                (LAUNCH_ENV, "plain".to_string()),
                (COMMAND_ENV, report_command(&report)),
            ],
            &report,
        )
        .await
    });
    outcome.labels_left = labels_still_recorded(root.path());
    println!(
        "\nlaunch-path control: report={:?} sweep={:?}",
        outcome.report, outcome.sweep
    );
    assert_eq!(
        (
            outcome.launch.as_deref(),
            outcome
                .report
                .as_deref()
                .is_some_and(|report| report.starts_with(CONTAINED)),
        ),
        (Some("started=true"), true),
        "report={:?}\nstdout={:?}\n{}",
        outcome.report,
        outcome.stdout,
        outcome.log
    );
    outcome.assert_cleaned_up("control");
}

/// #1507 route 1: a legacy command names a process of another, concurrent
/// legacy session as its child's parent. The child would join that
/// session's job, outside the launcher's. Since D259 each command's default
/// DACL keeps other legacy commands from opening its processes with
/// `PROCESS_CREATE_PROCESS`, so the launch is refused. A launcher that first
/// impersonates the other command's token gets that right back (#1560,
/// `command_identity::legacy_command_cannot_borrow_another_workspaces_command_token`).
#[test]
fn legacy_launch_under_another_session_process_stays_in_session() {
    let _guard = legacy_process_test_guard();
    let root = user_owned_test_root();
    let runtime = current_thread_runtime();
    let mut outcome = runtime.block_on(async {
        let codex_home = sandbox_home("launch-parent");
        let hold_dir = root.path().join("hold");
        fs::create_dir_all(&hold_dir).expect("create hold dir");
        // Releases the holding session however this test ends.
        let release = ReleaseHolder(hold_dir.join("hold.release"));
        let mut env_map: HashMap<String, String> = std::env::vars().collect();
        env_map.insert(HOLD_ENV.to_string(), hold_dir.display().to_string());
        let holder = spawn_windows_sandbox_session_legacy(
            &PermissionProfile::workspace_write(),
            workspace_roots_for(root.path()).as_slice(),
            codex_home.path(),
            vec![
                test_exe(),
                "--exact".to_string(),
                HOLD_ENTRY.to_string(),
                "--nocapture".to_string(),
                "--test-threads=1".to_string(),
            ],
            root.path(),
            env_map,
            /*timeout_ms*/ Some(90_000),
            &[],
            &[],
            /*tty*/ false,
            /*stdin_open*/ false,
            /*use_private_desktop*/ true,
        )
        .await
        .expect("spawn holding session");
        let pid_path = hold_dir.join("hold.pid");
        let deadline = Instant::now() + Duration::from_secs(/*secs*/ 60);
        let holder_pid = loop {
            if let Ok(pid) = fs::read_to_string(&pid_path)
                && !pid.is_empty()
            {
                break pid;
            }
            assert!(
                Instant::now() < deadline,
                "holding session never started\n{}",
                sandbox_log(codex_home.path())
            );
            tokio::time::sleep(Duration::from_millis(/*millis*/ 100)).await;
        };
        let report = root.path().join("parent.report");
        let outcome = run_launch(
            root.path(),
            codex_home.path(),
            launcher_argv(),
            vec![
                (LAUNCH_ENV, "parent".to_string()),
                (PARENT_ENV, holder_pid),
                (COMMAND_ENV, report_command(&report)),
            ],
            &report,
        )
        .await;
        fs::write(&release.0, "").expect("release holder");
        let _ =
            collect_stdout_and_exit(holder, codex_home.path(), Duration::from_secs(/*secs*/ 90))
                .await;
        outcome
    });
    outcome.labels_left = labels_still_recorded(root.path());
    outcome.assert_contained("parent-process attribute", &["open_parent_error=5"]);
}

/// Writes the holding session's release file when dropped.
struct ReleaseHolder(PathBuf);

impl Drop for ReleaseHolder {
    fn drop(&mut self) {
        let _ = fs::write(&self.0, "");
    }
}

/// Runs a Windows PowerShell script as the legacy command. The script sees
/// the report command in `CODEX_1507_CMD` (and its parts in `CODEX_1507_EXE`,
/// `CODEX_1507_ARGS`, `CODEX_1507_DIR`) and prints one `launch: ` line.
fn run_powershell_launch(
    name: &str,
    script: &str,
    extra_env: Vec<(&str, String)>,
) -> LaunchOutcome {
    let _guard = legacy_process_test_guard();
    let root = user_owned_test_root();
    let runtime = current_thread_runtime();
    let mut outcome = runtime.block_on(async {
        let codex_home = sandbox_home(name);
        let report = root.path().join(format!("{name}.report"));
        let encoded = BASE64.encode(
            script
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        let mut env = vec![
            (COMMAND_ENV, report_command(&report)),
            (REPORT_ENV, report.display().to_string()),
            (EXE_ENV, test_exe()),
            (ARGS_ENV, report_args(&report)),
            (DIR_ENV, root.path().display().to_string()),
        ];
        env.extend(extra_env);
        run_launch(
            root.path(),
            codex_home.path(),
            vec![
                system32_exe("WindowsPowerShell\\v1.0\\powershell.exe")
                    .display()
                    .to_string(),
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-EncodedCommand".to_string(),
                encoded,
            ],
            env,
            &report,
        )
        .await
    });
    outcome.labels_left = labels_still_recorded(root.path());
    outcome
}

/// Shared PowerShell prologue: `$cmd`, `$exe`, `$args1` and `$dir`, with
/// the launcher's id filled in, and `Report-Error`.
const POWERSHELL_PROLOGUE: &str = r#"
$ErrorActionPreference = 'Stop'
$cmd = $env:CODEX_1507_CMD.Replace('@LAUNCHER@', "$PID")
$args1 = $env:CODEX_1507_ARGS.Replace('@LAUNCHER@', "$PID")
$exe = $env:CODEX_1507_EXE
$dir = $env:CODEX_1507_DIR
$before = @(Get-Process | ForEach-Object Id)
$step = 'create'
# The codes are the HRESULT of each exception in the chain and, for WMI,
# the CIM status name; neither is localized, unlike the message.
function Report-Error($err) {
  $codes = @()
  for ($e = $err.Exception; $null -ne $e; $e = $e.InnerException) {
    $codes += 'hr=0x{0:X8}' -f $e.HResult
    if ($null -ne $e.PSObject.Properties['NativeErrorCode']) { $codes += "cim=$($e.NativeErrorCode)" }
  }
  "launch: started=false error=[$step] codes=[$($codes -join ' ')] " + ($err.Exception.Message -replace '\s+', ' ')
}
# Keeps the session (and its job) alive until the child reports, then lists
# the processes the launch added.
function Finish-Launch {
  $deadline = (Get-Date).AddSeconds(30)
  while (-not (Test-Path -LiteralPath $env:CODEX_1507_REPORT) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 200 }
  $new = Get-Process | Where-Object { $before -notcontains $_.Id -and $_.Id -ne $PID } | ForEach-Object { "$($_.ProcessName):$($_.Id)" }
  "new: " + ($new -join ',')
}
"#;

/// #1507 route 2: WMI's `Win32_Process.Create` starts the process from the
/// WMI provider host, outside the caller's job.
#[test]
fn legacy_launch_through_wmi_stays_in_session() {
    let script = format!(
        r#"{POWERSHELL_PROLOGUE}
try {{
  $r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{{ CommandLine = $cmd }}
  if ($r.ReturnValue -eq 0) {{ "launch: started=true" }} else {{ "launch: started=false return_value=$($r.ReturnValue)" }}
}} catch {{ Report-Error $_ }}
Finish-Launch
"#
    );
    run_powershell_launch("launch-wmi", &script, Vec::new()).assert_contained(
        "WMI Win32_Process.Create",
        &["error=[create] ", "cim=AccessDenied"],
    );
}

/// #1507 route 3: the Task Scheduler service starts a registered task as
/// the task's principal, outside the caller's job.
#[test]
fn legacy_launch_through_task_scheduler_stays_in_session() {
    let task = format!("codex-1507-{}", std::process::id());
    // Logon type 3: the caller's interactive token; flag 6: create or update.
    let script = format!(
        r#"{POWERSHELL_PROLOGUE}
try {{
  $service = New-Object -ComObject Schedule.Service
  $step = 'connect'
  $service.Connect()
  $step = 'folder'
  $folder = $service.GetFolder('\')
  $step = 'define'
  $definition = $service.NewTask(0)
  $action = $definition.Actions.Create(0)
  $action.Path = $exe
  $action.Arguments = $args1
  $action.WorkingDirectory = $dir
  $step = 'register'
  $null = $folder.RegisterTaskDefinition($env:CODEX_1507_TASK, $definition, 6, $null, $null, 3)
  $step = 'run'
  $null = $folder.GetTask($env:CODEX_1507_TASK).Run($null)
  "launch: started=true"
}} catch {{ Report-Error $_ }}
Finish-Launch
"#
    );
    let scheduled = ScheduledTask(task.clone());
    let outcome = run_powershell_launch("launch-schtasks", &script, vec![(TASK_ENV, task)]);
    let removed = scheduled.remove();
    outcome.assert_contained("Task Scheduler", &["error=[folder] ", "hr=0x80070003"]);
    assert_eq!(removed, Ok(()), "Task Scheduler task {}", scheduled.0);
}

/// Deletes the task named in `CODEX_1507_TASK` and prints `absent` once
/// looking it up fails with "not found" (HRESULT 0x80070002).
const REMOVE_TASK_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
function Codes($err) { for ($e = $err.Exception; $null -ne $e; $e = $e.InnerException) { '{0:X8}' -f $e.HResult } }
$service = New-Object -ComObject Schedule.Service
$service.Connect()
$folder = $service.GetFolder('\')
try { $folder.DeleteTask($env:CODEX_1507_TASK, 0) } catch { }
try { $null = $folder.GetTask($env:CODEX_1507_TASK); 'present' } catch {
  $codes = @(Codes $_)
  if ($codes -contains '80070002') { 'absent' } else { 'lookup error ' + ($codes -join ',') }
}
"#;

/// A Task Scheduler task the sandbox might have registered, removed when
/// dropped.
struct ScheduledTask(String);

impl ScheduledTask {
    /// Deletes the task, then checks that the Task Scheduler no longer has
    /// it.
    fn remove(&self) -> Result<(), String> {
        let encoded = BASE64.encode(
            REMOVE_TASK_SCRIPT
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        let output =
            std::process::Command::new(system32_exe("WindowsPowerShell\\v1.0\\powershell.exe"))
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-EncodedCommand",
                    encoded.as_str(),
                ])
                .env(TASK_ENV, &self.0)
                .output()
                .map_err(|err| format!("run powershell: {err}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim() == "absent" {
            Ok(())
        } else {
            Err(format!(
                "stdout={stdout:?} stderr={:?}",
                String::from_utf8_lossy(&output.stderr)
            ))
        }
    }
}

impl Drop for ScheduledTask {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}

/// #1507 route 4: out-of-process COM servers that start programs on the
/// caller's behalf, from their own process. Each is tried in turn.
fn com_launch(name: &str, clsid: &str, invoke: &str, refusal: &[&str]) {
    let script = format!(
        r#"{POWERSHELL_PROLOGUE}
try {{
  $o = [Activator]::CreateInstance([Type]::GetTypeFromCLSID('{clsid}'))
  $step = 'invoke'
  {invoke}
  "launch: started=true"
}} catch {{ Report-Error $_ }}
Finish-Launch
"#
    );
    run_powershell_launch(name, &script, Vec::new()).assert_contained(name, refusal);
}

/// Explorer's `ShellWindows`: `ShellExecute` runs in Explorer.
#[test]
fn legacy_launch_through_com_shell_windows_stays_in_session() {
    com_launch(
        "launch-com-shellwindows",
        "9BA05972-F6A8-11CF-A442-00A0C90A8F39",
        "$w = $o.Item(); if ($null -eq $w) { throw 'no shell window' }; $w.Document.Application.ShellExecute($exe, $args1, $dir, $null, 0)",
        // Refused at activation, before any Explorer window is needed.
        &["error=[create] ", "hr=0x80070005"],
    );
}

/// Explorer's `ShellBrowserWindow`: `ShellExecute` runs in Explorer.
#[test]
#[ignore = "#1507: starts the program outside the job with an unrestricted Low token (hosted runner, no Explorer running); closing it needs a product decision"]
fn legacy_launch_through_com_shell_browser_window_stays_in_session() {
    com_launch(
        "launch-com-shellbrowserwindow",
        "C08AFD90-F2A1-11D1-8455-00A0C91F3880",
        "$o.Document.Application.ShellExecute($exe, $args1, $dir, $null, 0)",
        &["hr=0x80070005"],
    );
}

/// `MMC20.Application`: `ExecuteShellCommand` runs in MMC.
#[test]
#[ignore = "#1507: MMC starts as the caller, outside its job, and runs the program there (still Low and restricted); closing it needs a product decision"]
fn legacy_launch_through_com_mmc_stays_in_session() {
    com_launch(
        "launch-com-mmc",
        "49B2791A-B1AE-4C90-9B8E-E860BA07F889",
        "$o.Document.ActiveView.ExecuteShellCommand($exe, $dir, $args1, '7')",
        &["hr=0x80070005"],
    );
}
