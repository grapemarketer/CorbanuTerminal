use crate::logging;
use crate::token::get_current_token_for_query;
use crate::token::get_logon_sid_bytes;
use crate::window_station::WindowAccess;
use crate::window_station::current_logon_sid;
use crate::winutil::format_last_error;
use crate::winutil::to_wide;
use anyhow::Result;
use rand::Rng;
use rand::SeedableRng;
use rand::rngs::SmallRng;
use std::ffi::c_void;
use std::path::Path;
use std::ptr;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Foundation::HLOCAL;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::EXPLICIT_ACCESS_W;
use windows_sys::Win32::Security::Authorization::GRANT_ACCESS;
use windows_sys::Win32::Security::Authorization::SE_WINDOW_OBJECT;
use windows_sys::Win32::Security::Authorization::SetEntriesInAclW;
use windows_sys::Win32::Security::Authorization::SetSecurityInfo;
use windows_sys::Win32::Security::Authorization::TRUSTEE_IS_SID;
use windows_sys::Win32::Security::Authorization::TRUSTEE_IS_UNKNOWN;
use windows_sys::Win32::Security::Authorization::TRUSTEE_W;
use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
use windows_sys::Win32::System::StationsAndDesktops::CloseDesktop;
use windows_sys::Win32::System::StationsAndDesktops::CreateDesktopW;
use windows_sys::Win32::System::StationsAndDesktops::DESKTOP_CREATEMENU;
use windows_sys::Win32::System::StationsAndDesktops::DESKTOP_CREATEWINDOW;
use windows_sys::Win32::System::StationsAndDesktops::DESKTOP_DELETE;
use windows_sys::Win32::System::StationsAndDesktops::DESKTOP_ENUMERATE;
use windows_sys::Win32::System::StationsAndDesktops::DESKTOP_HOOKCONTROL;
use windows_sys::Win32::System::StationsAndDesktops::DESKTOP_JOURNALPLAYBACK;
use windows_sys::Win32::System::StationsAndDesktops::DESKTOP_JOURNALRECORD;
use windows_sys::Win32::System::StationsAndDesktops::DESKTOP_READ_CONTROL;
use windows_sys::Win32::System::StationsAndDesktops::DESKTOP_READOBJECTS;
use windows_sys::Win32::System::StationsAndDesktops::DESKTOP_SWITCHDESKTOP;
use windows_sys::Win32::System::StationsAndDesktops::DESKTOP_WRITE_DAC;
use windows_sys::Win32::System::StationsAndDesktops::DESKTOP_WRITE_OWNER;
use windows_sys::Win32::System::StationsAndDesktops::DESKTOP_WRITEOBJECTS;

const DESKTOP_ALL_ACCESS: u32 = DESKTOP_READOBJECTS
    | DESKTOP_CREATEWINDOW
    | DESKTOP_CREATEMENU
    | DESKTOP_HOOKCONTROL
    | DESKTOP_JOURNALRECORD
    | DESKTOP_JOURNALPLAYBACK
    | DESKTOP_ENUMERATE
    | DESKTOP_WRITEOBJECTS
    | DESKTOP_SWITCHDESKTOP
    | DESKTOP_DELETE
    | DESKTOP_READ_CONTROL
    | DESKTOP_WRITE_DAC
    | DESKTOP_WRITE_OWNER;

pub struct LaunchDesktop {
    _private_desktop: Option<PrivateDesktop>,
    /// #345: this process's logon's access to its own non-interactive
    /// desktop, for commands started there; removed on drop.
    _window_access: Option<WindowAccess>,
    startup_name: Vec<u16>,
}

impl LaunchDesktop {
    /// In an interactive session, `Winsta0\Default` or a private desktop in
    /// `Winsta0`, as before. In an SSH session or a service (a non-interactive
    /// window station), where a child sent to `Winsta0` fails to start (#341),
    /// this process's own window station and desktop, or a private desktop in
    /// that window station (where it is created).
    ///
    /// On this process's own non-interactive desktop, this process's logon
    /// SID gets `DESKTOP_ACCESS` (read and write objects) there while the
    /// value lives: the commands' restricted tokens have it as a restricting
    /// SID, unlike the user SID the desktop grants, and fail to start
    /// without it (#345). The elevated sandbox's runner can't change Core's
    /// desktop; Core gave the runner's logon that access before starting it.
    ///
    /// `low_integrity` (the legacy sandbox, #158): the desktop the command
    /// starts on gets a Low label, without which a Low-integrity process
    /// fails to start (`0xC0000142`). The interactive `Winsta0\Default`
    /// already admits Low-integrity processes.
    pub fn prepare(
        use_private_desktop: bool,
        low_integrity: bool,
        logs_base_dir: Option<&Path>,
    ) -> Result<Self> {
        let own_station = crate::window_station::current_window_station_name()
            .filter(|name| !crate::window_station::is_interactive_window_station(name));
        let station = own_station.as_deref().unwrap_or("Winsta0");
        if use_private_desktop {
            let private_desktop = PrivateDesktop::create(low_integrity, logs_base_dir)?;
            let startup_name = to_wide(format!("{station}\\{}", private_desktop.name));
            Ok(Self {
                _private_desktop: Some(private_desktop),
                _window_access: None,
                startup_name,
            })
        } else {
            let (desktop, window_access) = match own_station {
                Some(_) => {
                    let desktop = crate::window_station::current_desktop_name()
                        .unwrap_or_else(|| "Default".to_string());
                    if low_integrity && let Err(err) = crate::integrity::label_desktop_low(&desktop)
                    {
                        logging::debug_log(
                            &format!("could not label this process's desktop Low: {err:#}"),
                            logs_base_dir,
                        );
                    }
                    (desktop, grant_own_desktop(logs_base_dir))
                }
                None => ("Default".to_string(), None),
            };
            Ok(Self {
                _private_desktop: None,
                _window_access: window_access,
                startup_name: to_wide(format!("{station}\\{desktop}")),
            })
        }
    }

    pub fn startup_info_desktop(&self) -> *mut u16 {
        self.startup_name.as_ptr() as *mut u16
    }
}

static IS_COMMAND_RUNNER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The elevated sandbox's command runner calls this first: Core gave its
/// logon the access to Core's desktop, which the runner can't change.
pub fn mark_as_command_runner() {
    IS_COMMAND_RUNNER.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// See [`LaunchDesktop::prepare`]. A failure is logged, not fatal.
fn grant_own_desktop(logs_base_dir: Option<&Path>) -> Option<WindowAccess> {
    if IS_COMMAND_RUNNER.load(std::sync::atomic::Ordering::SeqCst) {
        return None;
    }
    match current_logon_sid().and_then(|sid| WindowAccess::grant_desktop(&sid)) {
        Ok(access) => access,
        Err(err) => {
            logging::debug_log(
                &format!("could not give this logon its own desktop: {err:#}"),
                logs_base_dir,
            );
            None
        }
    }
}

struct PrivateDesktop {
    handle: isize,
    name: String,
}

impl PrivateDesktop {
    fn create(low_integrity: bool, logs_base_dir: Option<&Path>) -> Result<Self> {
        let mut rng = SmallRng::from_entropy();
        let name = format!("CodexSandboxDesktop-{:x}", rng.r#gen::<u128>());
        let name_wide = to_wide(&name);
        let handle = unsafe {
            CreateDesktopW(
                name_wide.as_ptr(),
                ptr::null(),
                ptr::null_mut(),
                0,
                DESKTOP_ALL_ACCESS,
                ptr::null_mut(),
            )
        };
        if handle == 0 {
            let err = unsafe { GetLastError() } as i32;
            logging::debug_log(
                &format!(
                    "CreateDesktopW failed for {name}: {} ({})",
                    err,
                    format_last_error(err),
                ),
                logs_base_dir,
            );
            return Err(anyhow::anyhow!("CreateDesktopW failed: {err}"));
        }

        unsafe {
            if let Err(err) = grant_desktop_access(handle, logs_base_dir) {
                let _ = CloseDesktop(handle);
                return Err(err);
            }
            // #158: a Low-integrity command can only attach to a Low desktop.
            if low_integrity && let Err(err) = crate::integrity::label_window_object_low(handle) {
                let _ = CloseDesktop(handle);
                return Err(err);
            }
        }

        Ok(Self { handle, name })
    }
}

unsafe fn grant_desktop_access(handle: isize, logs_base_dir: Option<&Path>) -> Result<()> {
    let token = get_current_token_for_query()?;
    let mut logon_sid = get_logon_sid_bytes(token)?;
    CloseHandle(token);

    let entries = [EXPLICIT_ACCESS_W {
        grfAccessPermissions: DESKTOP_ALL_ACCESS,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: 0,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: logon_sid.as_mut_ptr() as *mut c_void as *mut u16,
        },
    }];

    let mut updated_dacl = ptr::null_mut();
    let set_entries_code = SetEntriesInAclW(
        entries.len() as u32,
        entries.as_ptr(),
        ptr::null_mut(),
        &mut updated_dacl,
    );
    if set_entries_code != ERROR_SUCCESS {
        logging::debug_log(
            &format!("SetEntriesInAclW failed for private desktop: {set_entries_code}"),
            logs_base_dir,
        );
        return Err(anyhow::anyhow!(
            "SetEntriesInAclW failed for private desktop: {set_entries_code}"
        ));
    }

    let set_security_code = SetSecurityInfo(
        handle,
        SE_WINDOW_OBJECT,
        DACL_SECURITY_INFORMATION,
        ptr::null_mut(),
        ptr::null_mut(),
        updated_dacl,
        ptr::null_mut(),
    );
    if !updated_dacl.is_null() {
        LocalFree(updated_dacl as HLOCAL);
    }
    if set_security_code != ERROR_SUCCESS {
        logging::debug_log(
            &format!("SetSecurityInfo failed for private desktop: {set_security_code}"),
            logs_base_dir,
        );
        return Err(anyhow::anyhow!(
            "SetSecurityInfo failed for private desktop: {set_security_code}"
        ));
    }

    Ok(())
}

impl Drop for PrivateDesktop {
    fn drop(&mut self) {
        unsafe {
            if self.handle != 0 {
                let _ = CloseDesktop(self.handle);
            }
        }
    }
}
