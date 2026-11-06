//! PF-27-S08 probes: what a process under the broker token can still do.
//!
//! Each test re-executes this test binary (filtered to the entry test below)
//! with `spawn_protected`: once under the broker token, and once, as the
//! positive control, with a copy of this process's token (the PF-27-S07
//! broker). The child reports what it could do; the parent compares.
// The reports are the evidence of the measured runs (`--nocapture`).
#![allow(clippy::print_stderr, clippy::print_stdout)]

use crate::windows_broker_token::BROKER_TOKEN;
use crate::windows_broker_token::BrokerTokenOptions;
use crate::windows_protected_spawn::Confinement;
use crate::windows_protected_spawn::spawn_protected_with;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read as _;
use std::path::Path;
use std::path::PathBuf;

const ROLE_ENV: &str = "CODEX_PF27S08_ROLE";
const DIRS_ENV: &str = "CODEX_PF27S08_DIRS";
const CREDENTIAL_ENV: &str = "CODEX_PF27S08_CREDENTIAL";
const CHILD_TEST: &str = "windows_broker_token::tests::pf_27_s08_child_entry";
const REPORT_PREFIX: &str = "pf27s08-report:";
const ERROR_ACCESS_DENIED: i32 = 5;

type Report = BTreeMap<String, String>;

#[test]
fn pf_27_s08_child_entry() {
    let report = match std::env::var(ROLE_ENV).as_deref() {
        Ok("token") => token_report(),
        Ok("files") => file_report(),
        Ok("credential") => credential_report(),
        Ok("network") => network_report(),
        Ok("tokendiag") => token_diagnostics(),
        _ => return,
    };
    let line: Vec<String> = report.iter().map(|(k, v)| format!("{k}={v}")).collect();
    // Own line: libtest prints the test name without a newline first.
    println!("\n{REPORT_PREFIX}{}", line.join(","));
}

/// The broker token is low integrity, write-restricted to a SID the token
/// does not otherwise hold, without privileges, and the broker reports it
/// (`token+dacl+job`). Control: without it the check fails and the broker
/// reports `dacl+job`, which Core refuses.
#[test]
fn pf_27_s08_broker_runs_under_its_token_and_reports_it() {
    assert!(
        super::current_token_is_broker_token().is_err(),
        "this test process is not confined"
    );
    let confined = run_child("token", &[], Confinement::Broker(BROKER_TOKEN));
    eprintln!("pf27s08: broker token: {confined:?}");
    assert_eq!(confined["token"], "ok", "{confined:?}");
    assert_eq!(confined["containment"], "token+dacl+job", "{confined:?}");
    assert_eq!(confined["capability_sids"], "1", "{confined:?}");
    let control = run_child("token", &[], Confinement::SameToken);
    assert_ne!(control["token"], "ok", "{control:?}");
    assert_eq!(control["containment"], "dacl+job", "{control:?}");
    assert_eq!(control["capability_sids"], "0", "{control:?}");
}

/// The broker cannot create, modify, rename or delete a file the user can
/// write, in the user's temporary directory or in the low-integrity
/// `LocalLow` folder (writable at low integrity, so only the write
/// restriction stops it). Control: the PF-27-S07 broker can.
#[test]
fn pf_27_s08_broker_cannot_write_the_users_files() {
    let root = std::env::temp_dir().join(format!("pf27s08-files-{}", std::process::id()));
    let local_low = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .map(|home| home.join("AppData").join("LocalLow"))
        .filter(|dir| dir.is_dir())
        .map(|dir| dir.join(format!("pf27s08-files-{}", std::process::id())));
    let mut bases = vec![("temp", root)];
    bases.extend(local_low.map(|dir| ("locallow", dir)));
    for (role, confinement, expected) in [
        ("confined", Confinement::Broker(BROKER_TOKEN), "denied"),
        ("control", Confinement::SameToken, "ok"),
    ] {
        let dirs: Vec<PathBuf> = bases
            .iter()
            .map(|(label, base)| {
                let dir = base.join(role);
                prepare_files(&dir);
                std::path::PathBuf::from(format!("{label}={}", dir.display()))
            })
            .collect();
        let joined = std::env::join_paths(&dirs).expect("dirs");
        let report = run_child("files", &[(DIRS_ENV, joined)], confinement);
        eprintln!("pf27s08: files ({role}): {report:?}");
        let unexpected = report.iter().any(|(key, value)| {
            value != expected && !(role == "confined" && key == "locallow_delete")
        });
        if unexpected {
            let dirs: Vec<PathBuf> = bases.iter().map(|(_, base)| base.join(role)).collect();
            print_file_diagnostics(&dirs, confinement);
        }
        assert_eq!(
            report.len(),
            bases.len() * FILE_OPERATIONS.len(),
            "{report:?}"
        );
        for (key, value) in &report {
            // Measured gap, see the module docs: a parent that grants the
            // user FILE_DELETE_CHILD at low integrity.
            if role == "confined" && key == "locallow_delete" {
                eprintln!("pf27s08: confined locallow_delete={value}");
                continue;
            }
            assert_eq!(value, expected, "{role} {key}: {report:?}");
        }
    }
    // The confined child left everything as it was.
    for (label, base) in &bases {
        let dir = base.join("confined");
        assert_eq!(read(&dir.join("existing.txt")), "original");
        assert!(dir.join("rename.txt").is_file());
        assert!(*label == "locallow" || dir.join("delete.txt").is_file());
        assert!(!dir.join("new.txt").exists());
        assert!(!dir.join("subdir").exists());
    }
    for (_, base) in &bases {
        // The control made this file read-only.
        let existing = base.join("control").join("existing.txt");
        if let Ok(meta) = std::fs::metadata(&existing) {
            let mut permissions = meta.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            let _ = std::fs::set_permissions(&existing, permissions);
        }
        let _ = std::fs::remove_dir_all(base);
    }
}

/// A process under the broker token still resolves names and connects out
/// (the broker's upstreams; TLS over it is the network-proxy suite and the
/// real-Windows gate). Skipped without network access (positive control).
#[test]
fn pf_27_s08_broker_still_reaches_the_network() {
    let control = run_child("network", &[], Confinement::SameToken);
    if control["connect"] != "ok" {
        eprintln!("pf27s08: no network for the control ({control:?}); skipped");
        return;
    }
    let confined = run_child("network", &[], Confinement::Broker(BROKER_TOKEN));
    eprintln!("pf27s08: network under the broker token: {confined:?}");
    assert_eq!(confined["resolve"], "ok", "{confined:?}");
    assert_eq!(confined["connect"], "ok", "{confined:?}");
}

/// Credential Manager under the broker token: measured, not asserted. It
/// can read, write and delete the user's generic credentials, as the macOS
/// and Linux brokers can read the OS keyring (PF-27-S05); whether the
/// Windows broker should is the open key-path decision in the PF-27-S08
/// record. Control: the PF-27-S07 broker reads the same synthetic credential.
/// Skipped where this session has no Credential Manager (a service logon
/// without a profile).
#[test]
fn pf_27_s08_credential_manager_under_the_broker_token() {
    let target = format!("codex-pf27s08-probe-{}", std::process::id());
    if let Err(err) = credential::write(&target, b"pf27s08-synthetic-credential") {
        eprintln!("pf27s08: no Credential Manager in this session ({err}); skipped");
        return;
    }
    let env = [(CREDENTIAL_ENV, OsString::from(&target))];
    let control = run_child("credential", &env, Confinement::SameToken);
    let confined = run_child("credential", &env, Confinement::Broker(BROKER_TOKEN));
    let _ = credential::delete(&target);
    eprintln!("pf27s08: credential control={control:?} confined={confined:?}");
    assert_eq!(control["read"], "ok", "{control:?}");
}

/// The broker cannot ask WMI (`Win32_Process.Create`), Task Scheduler
/// (`Schedule.Service`) or an out-of-process COM server (`MMC20.Application`,
/// whose activation starts `mmc.exe`) to run something. The same script runs
/// under the broker token (with a default DACL it can work with) and, as the
/// positive control, under this process's own token. Every mechanism the
/// control can use must fail confined, and the control must reach WMI.
/// (`cscript`, because PowerShell at low integrity runs in constrained
/// language mode and cannot make the calls at all.)
#[test]
fn pf_27_s08_broker_cannot_start_work_through_wmi_tasks_or_com() {
    let control = run_script_probe(/*confined*/ false);
    eprintln!("pf27s08: WMI/tasks/COM control: {control:?}");
    let confined = run_script_probe(/*confined*/ true);
    eprintln!("pf27s08: WMI/tasks/COM under the broker token: {confined:?}");
    assert_eq!(control.len(), 3, "{control:?}");
    assert_eq!(confined.len(), 3, "{confined:?}");
    assert_eq!(
        control.get("wmi").map(String::as_str),
        Some("ok"),
        "{control:?}"
    );
    for (name, outcome) in &control {
        if outcome == "ok" {
            assert_ne!(confined[name], "ok", "{name}: {confined:?}");
        }
    }
}

fn run_child(role: &str, env: &[(&str, OsString)], confinement: Confinement) -> Report {
    try_run_child(role, env, confinement).unwrap_or_else(|err| panic!("{err}"))
}

fn try_run_child(
    role: &str,
    env: &[(&str, OsString)],
    confinement: Confinement,
) -> Result<Report, String> {
    let program = std::env::current_exe().expect("test binary");
    let args = [CHILD_TEST, "--exact", "--nocapture", "--test-threads=1"].map(OsString::from);
    let mut vars: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(name, _)| {
            !name
                .to_str()
                .is_some_and(|name| name.to_ascii_uppercase().starts_with("CODEX_PF27S08_"))
        })
        .collect();
    vars.push((ROLE_ENV.into(), role.into()));
    vars.extend(
        env.iter()
            .map(|(name, value)| ((*name).into(), value.clone())),
    );
    let (mut child, mut stdout) = spawn_protected_with(&program, &args, &vars, confinement)
        .map_err(|err| format!("{role} child ({confinement:?}) did not start: {err}"))?;
    let mut output = String::new();
    let _ = stdout.read_to_string(&mut output);
    let status = child.wait();
    if !output.contains(REPORT_PREFIX) {
        return Err(format!(
            "{role} child ({confinement:?}) printed no report; exit {status:?}:\n{output}"
        ));
    }
    Ok(decode(&output))
}

fn decode(output: &str) -> Report {
    let line = output
        .lines()
        .find_map(|line| line.find(REPORT_PREFIX).map(|start| &line[start..]))
        .and_then(|line| line.strip_prefix(REPORT_PREFIX))
        .unwrap_or_else(|| panic!("child printed no report:\n{output}"));
    line.trim()
        .split(',')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn token_report() -> Report {
    let mut report = Report::new();
    let token = match super::current_token_is_broker_token() {
        Ok(()) => "ok".to_string(),
        Err(err) => format!("no:{}", err.to_string().replace([',', '='], " ")),
    };
    report.insert("token".to_string(), token);
    let capabilities = super::current_capability_sid_strings()
        .map(|sids| sids.len().to_string())
        .unwrap_or_else(|_| "error".to_string());
    report.insert("capability_sids".to_string(), capabilities);
    let dir = std::env::temp_dir();
    let containment = crate::contain_credential_broker(&dir).mechanism;
    report.insert("containment".to_string(), containment);
    report
}

const FILE_OPERATIONS: &[&str] = &[
    "create",
    "overwrite",
    "append",
    "attributes",
    "rename",
    "delete",
    "mkdir",
];

fn prepare_files(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).expect("probe dir");
    std::fs::write(dir.join("existing.txt"), "original").expect("existing");
    std::fs::write(dir.join("rename.txt"), "x").expect("rename");
    std::fs::write(dir.join("delete.txt"), "x").expect("delete");
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// #1610: when a file probe's result is unexpected, print why it may have
/// been allowed: the security descriptor (owner, group, DACL, mandatory
/// label) of the probe files, the probe directory and each of its
/// ancestors, the volume's file system, and the tokens of this process and
/// of a child under `confinement` (integrity, groups, restricting SIDs).
fn print_file_diagnostics(dirs: &[PathBuf], confinement: Confinement) {
    for dir in dirs {
        let files = ["existing.txt", "rename.txt", "delete.txt"].map(|name| dir.join(name));
        let ancestors = dir.ancestors().map(Path::to_path_buf);
        for path in files.into_iter().chain(ancestors) {
            eprintln!("pf27s08-diag: sd {}: {}", path.display(), diag::sddl(&path));
        }
        eprintln!(
            "pf27s08-diag: volume of {}: {}",
            dir.display(),
            diag::volume(dir)
        );
    }
    eprintln!("pf27s08-diag: this process: {:?}", token_diagnostics());
    match try_run_child("tokendiag", &[], confinement) {
        Ok(report) => eprintln!("pf27s08-diag: child ({confinement:?}): {report:?}"),
        Err(err) => eprintln!("pf27s08-diag: child ({confinement:?}): {err}"),
    }
}

/// The current token's user, integrity level, groups and restricting SIDs
/// (`SID:attributes`, `;`-separated, so a child can report them).
fn token_diagnostics() -> Report {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Security::TOKEN_MANDATORY_LABEL;
    use windows_sys::Win32::Security::TOKEN_QUERY;
    use windows_sys::Win32::Security::TOKEN_USER;
    use windows_sys::Win32::Security::TokenGroups;
    use windows_sys::Win32::Security::TokenIntegrityLevel;
    use windows_sys::Win32::Security::TokenRestrictedSids;
    use windows_sys::Win32::Security::TokenUser;

    let clean = |text: String| text.replace([',', '='], " ");
    let mut report = Report::new();
    let token = match super::open_current_token(TOKEN_QUERY) {
        Ok(token) => token,
        Err(err) => {
            report.insert("error".to_string(), clean(err.to_string()));
            return report;
        }
    };
    let token = token.as_raw_handle() as HANDLE;
    let sid = |sid| super::sid_string(sid).unwrap_or_else(|err| clean(format!("?{err}")));
    let list = |class| match super::token_information(token, class) {
        Ok(buffer) => super::groups(&buffer)
            .iter()
            .map(|group| format!("{}:{:x}", sid(group.Sid), group.Attributes))
            .collect::<Vec<_>>()
            .join(";"),
        Err(err) => clean(format!("error {err}")),
    };
    let user = super::token_information(token, TokenUser).map(|buffer| {
        // SAFETY: the buffer holds a TOKEN_USER whose SID points into it.
        sid(unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid })
    });
    let integrity = super::token_information(token, TokenIntegrityLevel).map(|buffer| {
        // SAFETY: the buffer holds a TOKEN_MANDATORY_LABEL whose SID points
        // into it.
        sid(unsafe { (*buffer.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()).Label.Sid })
    });
    for (key, value) in [("user", user), ("integrity", integrity)] {
        let value = value.unwrap_or_else(|err| clean(format!("error {err}")));
        report.insert(key.to_string(), value);
    }
    report.insert("groups".to_string(), list(TokenGroups));
    report.insert("restricted".to_string(), list(TokenRestrictedSids));
    report
}

/// Security descriptor and volume queries for the #1610 diagnostics.
mod diag {
    use std::os::windows::ffi::OsStrExt as _;
    use std::path::Path;
    use std::ptr;
    use windows_sys::Win32::Foundation::HLOCAL;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::ConvertSecurityDescriptorToStringSecurityDescriptorW;
    use windows_sys::Win32::Security::Authorization::GetNamedSecurityInfoW;
    use windows_sys::Win32::Security::Authorization::SDDL_REVISION_1;
    use windows_sys::Win32::Security::Authorization::SE_FILE_OBJECT;
    use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
    use windows_sys::Win32::Security::GROUP_SECURITY_INFORMATION;
    use windows_sys::Win32::Security::LABEL_SECURITY_INFORMATION;
    use windows_sys::Win32::Security::OWNER_SECURITY_INFORMATION;
    use windows_sys::Win32::Security::PSECURITY_DESCRIPTOR;
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;
    use windows_sys::Win32::Storage::FileSystem::GetVolumePathNameW;

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    fn utf16(buffer: &[u16]) -> String {
        let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..len])
    }

    /// Owner, group, DACL and mandatory label as SDDL (no `S:` part: no
    /// explicit label, so medium by default).
    pub(super) fn sddl(path: &Path) -> String {
        let info = OWNER_SECURITY_INFORMATION
            | GROUP_SECURITY_INFORMATION
            | DACL_SECURITY_INFORMATION
            | LABEL_SECURITY_INFORMATION;
        let name = wide(path);
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `name` is NUL-terminated; `descriptor` is freed below.
        let status = unsafe {
            GetNamedSecurityInfoW(
                name.as_ptr(),
                SE_FILE_OBJECT,
                info,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return format!("error {status}");
        }
        let mut text: *mut u16 = ptr::null_mut();
        // SAFETY: `descriptor` is valid; `text` is freed below.
        let ok = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                info,
                &mut text,
                ptr::null_mut(),
            )
        };
        let result = if ok == 0 {
            format!("error {}", std::io::Error::last_os_error())
        } else {
            // SAFETY: `text` is a NUL-terminated string from the call.
            let result = unsafe {
                let len = (0..).take_while(|&i| *text.add(i) != 0).count();
                String::from_utf16_lossy(std::slice::from_raw_parts(text, len))
            };
            // SAFETY: allocated with LocalAlloc by the call.
            unsafe { LocalFree(text as HLOCAL) };
            result
        };
        // SAFETY: allocated with LocalAlloc by GetNamedSecurityInfoW.
        unsafe { LocalFree(descriptor as HLOCAL) };
        result
    }

    /// The volume's mount point, file system and file system flags.
    pub(super) fn volume(path: &Path) -> String {
        let name = wide(path);
        let mut root = [0_u16; 520];
        let mut file_system = [0_u16; 64];
        let mut flags = 0_u32;
        // SAFETY: every buffer is writable for the length passed.
        let ok = unsafe {
            GetVolumePathNameW(name.as_ptr(), root.as_mut_ptr(), root.len() as u32) != 0
                && GetVolumeInformationW(
                    root.as_ptr(),
                    ptr::null_mut(),
                    0,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &mut flags,
                    file_system.as_mut_ptr(),
                    file_system.len() as u32,
                ) != 0
        };
        if !ok {
            return format!("error {}", std::io::Error::last_os_error());
        }
        format!("{} {} flags 0x{flags:x}", utf16(&root), utf16(&file_system))
    }
}

fn outcome(result: std::io::Result<()>) -> String {
    match result {
        Ok(()) => "ok".to_string(),
        Err(err) if err.raw_os_error() == Some(ERROR_ACCESS_DENIED) => "denied".to_string(),
        Err(err) => format!("error:{}", err.raw_os_error().unwrap_or(-1)),
    }
}

fn file_report() -> Report {
    use std::io::Write as _;
    let mut report = Report::new();
    let dirs = std::env::var_os(DIRS_ENV).expect("dirs");
    for entry in std::env::split_paths(&dirs) {
        let entry = entry.to_string_lossy().into_owned();
        let (label, dir) = entry.split_once('=').expect("label=dir");
        let dir = Path::new(dir);
        let existing = dir.join("existing.txt");
        let results = [
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(dir.join("new.txt"))
                .map(drop),
            std::fs::OpenOptions::new()
                .write(true)
                .open(&existing)
                .and_then(|mut file| file.write_all(b"changed")),
            std::fs::OpenOptions::new()
                .append(true)
                .open(&existing)
                .and_then(|mut file| file.write_all(b"appended")),
            std::fs::metadata(&existing).and_then(|meta| {
                let mut permissions = meta.permissions();
                permissions.set_readonly(true);
                std::fs::set_permissions(&existing, permissions)
            }),
            std::fs::rename(dir.join("rename.txt"), dir.join("renamed.txt")),
            std::fs::remove_file(dir.join("delete.txt")),
            std::fs::create_dir(dir.join("subdir")),
        ];
        for (name, result) in FILE_OPERATIONS.iter().zip(results) {
            report.insert(format!("{label}_{name}"), outcome(result));
        }
    }
    report
}

fn network_report() -> Report {
    use std::net::ToSocketAddrs as _;
    let mut report = Report::new();
    let addresses: Vec<std::net::SocketAddr> = match ("github.com", 443).to_socket_addrs() {
        Ok(addresses) => addresses.collect(),
        Err(_) => Vec::new(),
    };
    report.insert(
        "resolve".to_string(),
        if addresses.is_empty() { "failed" } else { "ok" }.to_string(),
    );
    let connected = addresses.iter().any(|address| {
        std::net::TcpStream::connect_timeout(address, std::time::Duration::from_secs(10)).is_ok()
    });
    report.insert(
        "connect".to_string(),
        if connected { "ok" } else { "failed" }.to_string(),
    );
    report
}

fn credential_report() -> Report {
    let target = std::env::var(CREDENTIAL_ENV).expect("credential target");
    let read = match credential::read(&target) {
        Ok(true) => "ok".to_string(),
        Ok(false) => "wrong_value".to_string(),
        Err(code) => format!("error:{code}"),
    };
    // PF-27-S08 review item: also measure whether the broker can write or
    // delete the user's credentials (a write outside its runtime state).
    let write_target = format!("{target}-write-{}", std::process::id());
    let write = match credential::write(&write_target, b"pf27s08-write-probe") {
        Ok(()) => {
            let _ = credential::delete(&write_target);
            "ok".to_string()
        }
        Err(code) => format!("error:{}", code.raw_os_error().unwrap_or(-1)),
    };
    let delete = match credential::delete(&format!("{target}-nope-{0}", std::process::id())) {
        Ok(()) => "ok".to_string(),
        Err(code) => format!("error:{}", code.raw_os_error().unwrap_or(-1)),
    };
    Report::from([
        ("read".to_string(), read),
        ("write".to_string(), write),
        ("delete".to_string(), delete),
    ])
}

/// Credential Manager calls (declared here: the crate's `windows-sys`
/// features do not include them).
mod credential {
    use std::ffi::c_void;

    const CRED_TYPE_GENERIC: u32 = 1;
    const CRED_PERSIST_LOCAL_MACHINE: u32 = 2;
    const EXPECTED: &[u8] = b"pf27s08-synthetic-credential";

    #[repr(C)]
    struct Credential {
        flags: u32,
        kind: u32,
        target_name: *mut u16,
        comment: *mut u16,
        last_written: [u32; 2],
        blob_size: u32,
        blob: *mut u8,
        persist: u32,
        attribute_count: u32,
        attributes: *mut c_void,
        target_alias: *mut u16,
        user_name: *mut u16,
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn CredWriteW(credential: *const Credential, flags: u32) -> i32;
        fn CredReadW(target: *const u16, kind: u32, flags: u32, out: *mut *mut Credential) -> i32;
        fn CredDeleteW(target: *const u16, kind: u32, flags: u32) -> i32;
        fn CredFree(buffer: *mut c_void);
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain([0]).collect()
    }

    pub(super) fn write(target: &str, value: &[u8]) -> std::io::Result<()> {
        let mut name = wide(target);
        let mut user = wide("pf27s08");
        let mut blob = value.to_vec();
        let credential = Credential {
            flags: 0,
            kind: CRED_TYPE_GENERIC,
            target_name: name.as_mut_ptr(),
            comment: std::ptr::null_mut(),
            last_written: [0, 0],
            blob_size: blob.len() as u32,
            blob: blob.as_mut_ptr(),
            persist: CRED_PERSIST_LOCAL_MACHINE,
            attribute_count: 0,
            attributes: std::ptr::null_mut(),
            target_alias: std::ptr::null_mut(),
            user_name: user.as_mut_ptr(),
        };
        // SAFETY: every pointer refers to a live buffer above.
        if unsafe { CredWriteW(&credential, 0) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// Ok(true) if the credential reads back with the synthetic value;
    /// Err(Win32 error) if the read fails.
    pub(super) fn read(target: &str) -> Result<bool, i32> {
        let name = wide(target);
        let mut out: *mut Credential = std::ptr::null_mut();
        // SAFETY: `name` is NUL-terminated; `out` is freed below.
        if unsafe { CredReadW(name.as_ptr(), CRED_TYPE_GENERIC, 0, &mut out) } == 0 {
            return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(-1));
        }
        // SAFETY: CredReadW returned a credential with `blob_size` bytes.
        let matches = unsafe {
            std::slice::from_raw_parts((*out).blob, (*out).blob_size as usize) == EXPECTED
        };
        // SAFETY: allocated by CredReadW.
        unsafe { CredFree(out.cast()) };
        Ok(matches)
    }

    pub(super) fn delete(target: &str) -> std::io::Result<()> {
        let name = wide(target);
        // SAFETY: `name` is NUL-terminated.
        if unsafe { CredDeleteW(name.as_ptr(), CRED_TYPE_GENERIC, 0) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

/// The script probe: one `name=outcome` per mechanism, where outcome is
/// `ok` or `failed:<hex error> <description>`.
const SCRIPT_PROBE: &str = r#"
On Error Resume Next
Dim sysroot, task, out, svc, rc, pid, ts, folder, def, action, mmc
sysroot = CreateObject("WScript.Shell").ExpandEnvironmentStrings("%SystemRoot%")
task = "codex-pf27s08-" & WScript.Arguments(0)
Function Outcome(name)
  If Err.Number = 0 Then
    Outcome = name & "=ok"
  Else
    Outcome = name & "=failed:" & Hex(Err.Number) & " " & _
      Replace(Replace(Replace(Err.Description, ",", " "), "=", " "), vbCrLf, " ")
  End If
  Err.Clear
End Function
Set svc = GetObject("winmgmts:{impersonationLevel=impersonate}!\\.\root\cimv2")
If Err.Number = 0 Then
  rc = svc.Get("Win32_Process").Create(sysroot & "\System32\cmd.exe /d /c exit 0", Null, Null, pid)
  If Err.Number = 0 And rc <> 0 Then Err.Raise 5, "Win32_Process.Create", "ReturnValue " & rc
End If
out = Outcome("wmi")
Set ts = CreateObject("Schedule.Service")
If Err.Number = 0 Then ts.Connect
If Err.Number = 0 Then Set folder = ts.GetFolder("\")
If Err.Number = 0 Then
  Set def = ts.NewTask(0)
  Set action = def.Actions.Create(0)
  action.Path = sysroot & "\System32\cmd.exe"
  action.Arguments = "/d /c exit 0"
  folder.RegisterTaskDefinition task, def, 6, Null, Null, 3
End If
out = out & "," & Outcome("task")
folder.DeleteTask task, 0
Err.Clear
Set mmc = GetObject("new:{49B2791A-B1AE-4C90-9B8E-E860BA07F889}")
If Err.Number = 0 Then mmc.Quit
out = out & "," & Outcome("com")
WScript.Echo vbCrLf & "pf27s08-report:" & out
"#;

fn run_script_probe(confined: bool) -> Report {
    use crate::windows_broker_token::BrokerDefaultDacl;
    use crate::windows_broker_token::create_broker_token;
    use std::collections::HashMap;
    use std::os::windows::io::IntoRawHandle as _;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Threading::TerminateProcess;
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    let label = if confined { "confined" } else { "control" };
    let script =
        std::env::temp_dir().join(format!("pf27s08-probe-{}-{label}.vbs", std::process::id()));
    std::fs::write(&script, SCRIPT_PROBE).expect("write the probe script");
    let system_root = std::env::var("SystemRoot").expect("SystemRoot");
    let argv = vec![
        format!(r"{system_root}\System32\cscript.exe"),
        "//NoLogo".to_string(),
        script.to_string_lossy().into_owned(),
        format!("{}-{label}", std::process::id()),
    ];
    let env: HashMap<String, String> = std::env::vars()
        .filter(|(name, _)| !name.to_ascii_uppercase().starts_with("CODEX_PF27S08_"))
        .collect();
    let token: HANDLE = if confined {
        create_broker_token(BrokerTokenOptions {
            default_dacl: BrokerDefaultDacl::OwnedByCapability,
        })
        .expect("broker token")
        .into_raw_handle() as HANDLE
    } else {
        // SAFETY: returns an owned handle to this process's token.
        unsafe { codex_windows_sandbox::get_current_token_for_restriction() }.expect("token")
    };
    let cwd = std::env::temp_dir();
    let spawned = codex_windows_sandbox::spawn_process_with_pipes(
        token,
        &argv,
        &cwd,
        &env,
        codex_windows_sandbox::StdinMode::Closed,
        codex_windows_sandbox::StderrMode::MergeStdout,
        codex_windows_sandbox::ConsoleMode::NoWindow,
        /*use_private_desktop*/ false,
        /*logs_base_dir*/ None,
    )
    .expect("spawn cscript");
    let (sender, receiver) = std::sync::mpsc::channel::<Vec<u8>>();
    let reader = codex_windows_sandbox::read_handle_loop(spawned.stdout_read, move |chunk| {
        let _ = sender.send(chunk.to_vec());
    });
    let mut code = 0_u32;
    // SAFETY: the handles stay valid until closed below.
    unsafe {
        const WAIT_TIMEOUT: u32 = 0x102;
        if WaitForSingleObject(spawned.process.hProcess, 180_000) == WAIT_TIMEOUT {
            TerminateProcess(spawned.process.hProcess, 1);
        }
        windows_sys::Win32::System::Threading::GetExitCodeProcess(
            spawned.process.hProcess,
            &mut code,
        );
        CloseHandle(spawned.process.hThread);
        CloseHandle(spawned.process.hProcess);
        CloseHandle(token);
    }
    let _ = reader.join();
    let output: Vec<u8> = receiver.try_iter().flatten().collect();
    let _ = std::fs::remove_file(&script);
    let output = String::from_utf8_lossy(&output);
    assert!(
        output.contains(REPORT_PREFIX),
        "{label} cscript exited 0x{code:x} without a report:\n{output}"
    );
    decode(&output)
}
