use crate::winutil::to_wide;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use std::ffi::c_void;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Foundation::HLOCAL;
use windows_sys::Win32::Foundation::LUID;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::AdjustTokenPrivileges;
use windows_sys::Win32::Security::Authorization::EXPLICIT_ACCESS_W;
use windows_sys::Win32::Security::Authorization::GRANT_ACCESS;
use windows_sys::Win32::Security::Authorization::SE_KERNEL_OBJECT;
use windows_sys::Win32::Security::Authorization::SetEntriesInAclW;
use windows_sys::Win32::Security::Authorization::SetSecurityInfo;
use windows_sys::Win32::Security::Authorization::TRUSTEE_IS_SID;
use windows_sys::Win32::Security::Authorization::TRUSTEE_IS_UNKNOWN;
use windows_sys::Win32::Security::Authorization::TRUSTEE_W;
use windows_sys::Win32::Security::CopySid;
use windows_sys::Win32::Security::CreateRestrictedToken;
use windows_sys::Win32::Security::CreateWellKnownSid;
use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
use windows_sys::Win32::Security::EqualSid;
use windows_sys::Win32::Security::GetLengthSid;
use windows_sys::Win32::Security::GetTokenInformation;
use windows_sys::Win32::Security::LookupPrivilegeValueW;
use windows_sys::Win32::Security::PROTECTED_DACL_SECURITY_INFORMATION;
use windows_sys::Win32::Security::SetTokenInformation;

use windows_sys::Win32::Security::ACL;
use windows_sys::Win32::Security::SID_AND_ATTRIBUTES;
use windows_sys::Win32::Security::TOKEN_ADJUST_DEFAULT;
use windows_sys::Win32::Security::TOKEN_ADJUST_PRIVILEGES;
use windows_sys::Win32::Security::TOKEN_ADJUST_SESSIONID;
use windows_sys::Win32::Security::TOKEN_ASSIGN_PRIMARY;
use windows_sys::Win32::Security::TOKEN_DUPLICATE;
use windows_sys::Win32::Security::TOKEN_GROUPS;
use windows_sys::Win32::Security::TOKEN_PRIVILEGES;
use windows_sys::Win32::Security::TOKEN_QUERY;
use windows_sys::Win32::Security::TOKEN_USER;
use windows_sys::Win32::Security::TokenDefaultDacl;
use windows_sys::Win32::Security::TokenGroups;
use windows_sys::Win32::Security::TokenUser;
use windows_sys::Win32::Storage::FileSystem::READ_CONTROL;
use windows_sys::Win32::Storage::FileSystem::WRITE_DAC;
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::System::Threading::OpenProcessToken;

const DISABLE_MAX_PRIVILEGE: u32 = 0x01;
const LUA_TOKEN: u32 = 0x04;
const WRITE_RESTRICTED: u32 = 0x08;
const GENERIC_ALL: u32 = 0x1000_0000;
const GENERIC_EXECUTE: u32 = 0x2000_0000;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
/// The one standard right every object type maps the same way.
const SYNCHRONIZE: u32 = 0x0010_0000;
const WIN_WORLD_SID: i32 = 1;
const WIN_LOCAL_SYSTEM_SID: i32 = 22;
const WIN_CREATOR_OWNER_RIGHTS_SID: i32 = 71;
const SE_GROUP_LOGON_ID: u32 = 0xC0000000;
const SE_GROUP_ENABLED: u32 = 0x0000_0004;

#[repr(C)]
struct TokenDefaultDaclInfo {
    default_dacl: *mut ACL,
}

/// Sets a permissive default DACL so sandboxed processes can create pipes/IPC objects
/// without hitting ACCESS_DENIED when PowerShell builds pipelines.
unsafe fn set_default_dacl(h_token: HANDLE, sids: &[*mut c_void]) -> Result<()> {
    let grants: Vec<(*mut c_void, u32)> = sids.iter().map(|sid| (*sid, GENERIC_ALL)).collect();
    set_default_dacl_grants(h_token, &grants)
}

/// An ACL of exactly `grants` (SID, access mask); free it with `LocalFree`.
unsafe fn acl_from_grants(grants: &[(*mut c_void, u32)]) -> Result<*mut ACL> {
    let entries: Vec<EXPLICIT_ACCESS_W> = grants
        .iter()
        .map(|(sid, access)| EXPLICIT_ACCESS_W {
            grfAccessPermissions: *access,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: 0,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: 0,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_UNKNOWN,
                ptstrName: *sid as *mut u16,
            },
        })
        .collect();
    let mut p_new_dacl: *mut ACL = std::ptr::null_mut();
    let res = SetEntriesInAclW(
        entries.len() as u32,
        entries.as_ptr(),
        std::ptr::null_mut(),
        &mut p_new_dacl,
    );
    if res != ERROR_SUCCESS {
        return Err(anyhow!("SetEntriesInAclW failed: {res}"));
    }
    Ok(p_new_dacl)
}

/// Sets the token's default DACL to exactly `grants` (SID, access mask).
unsafe fn set_default_dacl_grants(h_token: HANDLE, grants: &[(*mut c_void, u32)]) -> Result<()> {
    if grants.is_empty() {
        return Ok(());
    }
    let p_new_dacl = acl_from_grants(grants)?;
    let mut info = TokenDefaultDaclInfo {
        default_dacl: p_new_dacl,
    };
    let ok = SetTokenInformation(
        h_token,
        TokenDefaultDacl,
        &mut info as *mut _ as *mut c_void,
        std::mem::size_of::<TokenDefaultDaclInfo>() as u32,
    );
    if ok == 0 {
        let err = GetLastError();
        if !p_new_dacl.is_null() {
            LocalFree(p_new_dacl as HLOCAL);
        }
        return Err(anyhow!(
            "SetTokenInformation(TokenDefaultDacl) failed: {err}",
        ));
    }
    if !p_new_dacl.is_null() {
        LocalFree(p_new_dacl as HLOCAL);
    }
    Ok(())
}

/// Replaces kernel object `handle`'s DACL with exactly `grants`, protected
/// from inheritance. `handle` needs `WRITE_DAC`.
unsafe fn set_object_dacl(handle: HANDLE, grants: &[(*mut c_void, u32)]) -> Result<()> {
    let dacl = acl_from_grants(grants)?;
    let res = SetSecurityInfo(
        handle,
        SE_KERNEL_OBJECT,
        DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        dacl,
        std::ptr::null(),
    );
    if !dacl.is_null() {
        LocalFree(dacl as HLOCAL);
    }
    if res != ERROR_SUCCESS {
        return Err(anyhow!("SetSecurityInfo(DACL) failed: {res}"));
    }
    Ok(())
}

/// The elevated sandbox's users' group (setup creates it and adds both
/// sandbox accounts).
pub(crate) const SANDBOX_USERS_GROUP: &str = "CodexSandboxUsers";

/// Whether `token` holds `sid` as an enabled group. Needs `TOKEN_QUERY`
/// only (unlike `CheckTokenMembership`, which duplicates the token: the
/// hardened runner can't open its own token for that).
unsafe fn token_has_enabled_group(token: HANDLE, sid: *mut c_void) -> Result<bool> {
    let mut needed: u32 = 0;
    GetTokenInformation(token, TokenGroups, std::ptr::null_mut(), 0, &mut needed);
    if needed == 0 {
        return Err(anyhow!(
            "GetTokenInformation(TokenGroups) failed: {}",
            GetLastError()
        ));
    }
    let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    if GetTokenInformation(
        token,
        TokenGroups,
        buffer.as_mut_ptr().cast(),
        needed,
        &mut needed,
    ) == 0
    {
        return Err(anyhow!(
            "GetTokenInformation(TokenGroups) failed: {}",
            GetLastError()
        ));
    }
    let groups = buffer.as_ptr() as *const TOKEN_GROUPS;
    // The entries run past the declared one-element array: derive their
    // pointer from the buffer, not from a reference to that array.
    let entries = std::slice::from_raw_parts(
        std::ptr::addr_of!((*groups).Groups).cast::<SID_AND_ATTRIBUTES>(),
        (*groups).GroupCount as usize,
    );
    Ok(entries
        .iter()
        .any(|entry| entry.Attributes & SE_GROUP_ENABLED != 0 && EqualSid(entry.Sid, sid) != 0))
}

/// The sandbox users' group SID; fails unless `token` (the elevated
/// runner's, opened with `TOKEN_QUERY`) holds it enabled. Setup creates the
/// group and adds both sandbox accounts (upstream #8483), and every setup
/// version Core accepts has, so a runner without it fails closed rather than
/// fall back to a default DACL that doesn't keep its commands apart.
unsafe fn sandbox_users_group_of(token: HANDLE) -> Result<Vec<u8>> {
    let mut group = crate::winutil::resolve_sid(SANDBOX_USERS_GROUP)
        .with_context(|| format!("resolve the {SANDBOX_USERS_GROUP} group; rerun sandbox setup"))?;
    if !token_has_enabled_group(token, group.as_mut_ptr() as *mut c_void)? {
        return Err(anyhow!(
            "the sandbox account isn't in the {SANDBOX_USERS_GROUP} group; rerun sandbox setup"
        ));
    }
    Ok(group)
}

/// D324 (#1648): gives objects the elevated runner creates from now on
/// without explicit security (the threads Windows starts in it, pipes, job
/// objects, the ConPTY console host) a default DACL of `GENERIC_ALL` for
/// SYSTEM, `GENERIC_READ | GENERIC_WRITE | GENERIC_EXECUTE` for the sandbox
/// users' group, and `READ_CONTROL` for `OWNER RIGHTS`; the sandbox account
/// itself gets nothing. The runner is unrestricted, so the group gives it
/// what it needs on them. Its commands, and every other elevated command,
/// hold the group only as a normal group, not as a restricting SID, so they
/// get no write right. Rights outside the generic read, write and execute
/// mappings, such as `TOKEN_DUPLICATE`, aren't write rights, so the normal
/// check alone decides them; the group has no `GENERIC_ALL`, so the
/// runner's process and threads don't grant them. A ConPTY console host's
/// own token still opens for `TOKEN_DUPLICATE` (measured), but a command
/// impersonating it gets identification level only, so it can't act as it
/// (writes and process creation fail with 1346, measured). Before, they got
/// full access through the sandbox account. Fails if the runner isn't in the
/// group.
pub fn restrict_current_default_dacl() -> Result<()> {
    // SAFETY: the token handle is closed below.
    unsafe {
        let mut token: HANDLE = 0;
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_ADJUST_DEFAULT,
            &mut token,
        ) == 0
        {
            return Err(anyhow!("OpenProcessToken failed: {}", GetLastError()));
        }
        let result = restrict_runner_default_dacl(token);
        CloseHandle(token);
        result
    }
}

/// [`restrict_current_default_dacl`] on the runner's `token`, opened with
/// `TOKEN_QUERY | TOKEN_ADJUST_DEFAULT`: Core does it too while the runner
/// is still suspended, where it may (#1721).
///
/// # Safety
/// `token` must be a valid token handle with those rights.
pub(crate) unsafe fn restrict_runner_default_dacl(token: HANDLE) -> Result<()> {
    let mut group = sandbox_users_group_of(token)?;
    let mut system = well_known_sid(WIN_LOCAL_SYSTEM_SID)?;
    let mut owner_rights = well_known_sid(WIN_CREATOR_OWNER_RIGHTS_SID)?;
    set_default_dacl_grants(
        token,
        &[
            (system.as_mut_ptr() as *mut c_void, GENERIC_ALL),
            (
                group.as_mut_ptr() as *mut c_void,
                GENERIC_READ | GENERIC_WRITE | GENERIC_EXECUTE,
            ),
            (owner_rights.as_mut_ptr() as *mut c_void, READ_CONTROL),
        ],
    )
}

/// D324 (#1648): limits the elevated runner's own token to `TOKEN_QUERY` for
/// its user, the sandbox account its commands share. Its token DACL gave
/// that account full access, and the account is a restricting SID of every
/// elevated command's token, so any elevated command could adjust a
/// runner's token (its default DACL, groups and privileges). Open what the
/// runner needs from its token first ([`get_current_token_for_restriction`]):
/// afterwards it can't either.
pub fn restrict_current_token_access() -> Result<()> {
    // SAFETY: the pseudo-handle is valid; the token handle and SID buffers
    // live until the end of this block.
    unsafe {
        let mut token: HANDLE = 0;
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | READ_CONTROL | WRITE_DAC,
            &mut token,
        ) == 0
        {
            return Err(anyhow!("OpenProcessToken failed: {}", GetLastError()));
        }
        let result = (|| {
            let mut user = get_user_sid_bytes(token)?;
            let mut system = well_known_sid(WIN_LOCAL_SYSTEM_SID)?;
            let mut owner_rights = well_known_sid(WIN_CREATOR_OWNER_RIGHTS_SID)?;
            set_object_dacl(
                token,
                &[
                    (user.as_mut_ptr() as *mut c_void, TOKEN_QUERY),
                    (system.as_mut_ptr() as *mut c_void, GENERIC_ALL),
                    (owner_rights.as_mut_ptr() as *mut c_void, READ_CONTROL),
                ],
            )
        })();
        CloseHandle(token);
        result
    }
}

pub unsafe fn world_sid() -> Result<Vec<u8>> {
    well_known_sid(WIN_WORLD_SID)
}

unsafe fn well_known_sid(kind: i32) -> Result<Vec<u8>> {
    let mut size: u32 = 0;
    CreateWellKnownSid(kind, std::ptr::null_mut(), std::ptr::null_mut(), &mut size);
    let mut buf: Vec<u8> = vec![0u8; size as usize];
    let ok = CreateWellKnownSid(
        kind,
        std::ptr::null_mut(),
        buf.as_mut_ptr() as *mut c_void,
        &mut size,
    );
    if ok == 0 {
        return Err(anyhow!("CreateWellKnownSid failed: {}", GetLastError()));
    }
    Ok(buf)
}

/// # Safety
/// Caller is responsible for freeing the returned SID with `LocalFree`.
pub unsafe fn convert_string_sid_to_sid(s: &str) -> Option<*mut c_void> {
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn ConvertStringSidToSidW(StringSid: *const u16, Sid: *mut *mut c_void) -> i32;
    }
    let mut psid: *mut c_void = std::ptr::null_mut();
    let ok = unsafe { ConvertStringSidToSidW(to_wide(s).as_ptr(), &mut psid) };
    if ok != 0 { Some(psid) } else { None }
}

/// Owns a SID allocated by `ConvertStringSidToSidW` and releases it with `LocalFree`.
pub struct LocalSid {
    psid: *mut c_void,
}

impl LocalSid {
    pub fn from_string(sid: &str) -> Result<Self> {
        let psid = unsafe { convert_string_sid_to_sid(sid) }
            .ok_or_else(|| anyhow!("invalid SID string: {sid}"))?;
        Ok(Self { psid })
    }

    pub fn as_ptr(&self) -> *mut c_void {
        self.psid
    }
}

impl Drop for LocalSid {
    fn drop(&mut self) {
        if !self.psid.is_null() {
            unsafe {
                LocalFree(self.psid as HLOCAL);
            }
        }
    }
}

/// This process's token, opened only to read it (`TOKEN_QUERY`): the
/// elevated runner can't open its own for more once hardened (D324).
///
/// # Safety
/// Caller must close the returned token handle.
pub unsafe fn get_current_token_for_query() -> Result<HANDLE> {
    let mut token: HANDLE = 0;
    if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
        return Err(anyhow!(
            "OpenProcessToken(TOKEN_QUERY) failed: {}",
            GetLastError()
        ));
    }
    Ok(token)
}

/// # Safety
/// Caller must close the returned token handle.
pub unsafe fn get_current_token_for_restriction() -> Result<HANDLE> {
    let desired = TOKEN_DUPLICATE
        | TOKEN_QUERY
        | TOKEN_ASSIGN_PRIMARY
        | TOKEN_ADJUST_DEFAULT
        | TOKEN_ADJUST_SESSIONID
        | TOKEN_ADJUST_PRIVILEGES;
    let mut h: HANDLE = 0;
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(
            ProcessHandle: HANDLE,
            DesiredAccess: u32,
            TokenHandle: *mut HANDLE,
        ) -> i32;
    }
    let ok = unsafe { OpenProcessToken(GetCurrentProcess(), desired, &mut h) };
    if ok == 0 {
        return Err(anyhow!("OpenProcessToken failed: {}", GetLastError()));
    }
    Ok(h)
}

pub unsafe fn get_logon_sid_bytes(h_token: HANDLE) -> Result<Vec<u8>> {
    unsafe fn scan_token_groups_for_logon(h: HANDLE) -> Option<Vec<u8>> {
        let mut needed: u32 = 0;
        GetTokenInformation(h, TokenGroups, std::ptr::null_mut(), 0, &mut needed);
        if needed == 0 {
            return None;
        }
        let mut buf: Vec<u8> = vec![0u8; needed as usize];
        let ok = GetTokenInformation(
            h,
            TokenGroups,
            buf.as_mut_ptr() as *mut c_void,
            needed,
            &mut needed,
        );
        if ok == 0 || (needed as usize) < std::mem::size_of::<u32>() {
            return None;
        }
        let group_count = std::ptr::read_unaligned(buf.as_ptr() as *const u32) as usize;
        // TOKEN_GROUPS layout is: DWORD GroupCount; SID_AND_ATTRIBUTES Groups[];
        // On 64-bit, Groups is aligned to pointer alignment after 4-byte GroupCount.
        let after_count = unsafe { buf.as_ptr().add(std::mem::size_of::<u32>()) } as usize;
        let align = std::mem::align_of::<SID_AND_ATTRIBUTES>();
        let aligned = (after_count + (align - 1)) & !(align - 1);
        let groups_ptr = aligned as *const SID_AND_ATTRIBUTES;
        for i in 0..group_count {
            let entry: SID_AND_ATTRIBUTES = std::ptr::read_unaligned(groups_ptr.add(i));
            if (entry.Attributes & SE_GROUP_LOGON_ID) == SE_GROUP_LOGON_ID {
                let sid = entry.Sid;
                let sid_len = GetLengthSid(sid);
                if sid_len == 0 {
                    return None;
                }
                let mut out = vec![0u8; sid_len as usize];
                if CopySid(sid_len, out.as_mut_ptr() as *mut c_void, sid) == 0 {
                    return None;
                }
                return Some(out);
            }
        }
        None
    }

    if let Some(v) = scan_token_groups_for_logon(h_token) {
        return Ok(v);
    }

    #[repr(C)]
    struct TOKEN_LINKED_TOKEN {
        linked_token: HANDLE,
    }
    const TOKEN_LINKED_TOKEN_CLASS: i32 = 19; // TokenLinkedToken
    let mut ln_needed: u32 = 0;
    GetTokenInformation(
        h_token,
        TOKEN_LINKED_TOKEN_CLASS,
        std::ptr::null_mut(),
        0,
        &mut ln_needed,
    );
    if ln_needed >= std::mem::size_of::<TOKEN_LINKED_TOKEN>() as u32 {
        let mut ln_buf: Vec<u8> = vec![0u8; ln_needed as usize];
        let ok = GetTokenInformation(
            h_token,
            TOKEN_LINKED_TOKEN_CLASS,
            ln_buf.as_mut_ptr() as *mut c_void,
            ln_needed,
            &mut ln_needed,
        );
        if ok != 0 {
            let lt: TOKEN_LINKED_TOKEN =
                std::ptr::read_unaligned(ln_buf.as_ptr() as *const TOKEN_LINKED_TOKEN);
            if lt.linked_token != 0 {
                let res = scan_token_groups_for_logon(lt.linked_token);
                CloseHandle(lt.linked_token);
                if let Some(v) = res {
                    return Ok(v);
                }
            }
        }
    }

    Err(anyhow!("Logon SID not present on token"))
}

pub(crate) unsafe fn get_user_sid_bytes(h_token: HANDLE) -> Result<Vec<u8>> {
    let mut needed: u32 = 0;
    GetTokenInformation(h_token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
    if needed == 0 {
        return Err(anyhow!("TokenUser size query returned 0"));
    }
    let mut user_buf: Vec<u8> = vec![0u8; needed as usize];
    let ok = GetTokenInformation(
        h_token,
        TokenUser,
        user_buf.as_mut_ptr() as *mut c_void,
        needed,
        &mut needed,
    );
    if ok == 0 || (needed as usize) < std::mem::size_of::<TOKEN_USER>() {
        return Err(anyhow!(
            "GetTokenInformation(TokenUser) failed: {}",
            GetLastError()
        ));
    }
    let token_user: TOKEN_USER = std::ptr::read_unaligned(user_buf.as_ptr() as *const TOKEN_USER);
    let sid_len = GetLengthSid(token_user.User.Sid);
    if sid_len == 0 {
        return Err(anyhow!(
            "GetLengthSid(TokenUser) failed: {}",
            GetLastError()
        ));
    }
    let mut user_sid_bytes = vec![0u8; sid_len as usize];
    if CopySid(
        sid_len,
        user_sid_bytes.as_mut_ptr() as *mut c_void,
        token_user.User.Sid,
    ) == 0
    {
        return Err(anyhow!("CopySid(TokenUser) failed: {}", GetLastError()));
    }
    Ok(user_sid_bytes)
}

unsafe fn enable_single_privilege(h_token: HANDLE, name: &str) -> Result<()> {
    let mut luid = LUID {
        LowPart: 0,
        HighPart: 0,
    };
    let ok = LookupPrivilegeValueW(std::ptr::null(), to_wide(name).as_ptr(), &mut luid);
    if ok == 0 {
        return Err(anyhow!("LookupPrivilegeValueW failed: {}", GetLastError()));
    }
    let mut tp: TOKEN_PRIVILEGES = std::mem::zeroed();
    tp.PrivilegeCount = 1;
    tp.Privileges[0].Luid = luid;
    tp.Privileges[0].Attributes = 0x00000002; // SE_PRIVILEGE_ENABLED
    let ok2 = AdjustTokenPrivileges(
        h_token,
        0,
        &tp,
        0,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    );
    if ok2 == 0 {
        return Err(anyhow!("AdjustTokenPrivileges failed: {}", GetLastError()));
    }
    let err = GetLastError();
    if err != 0 {
        return Err(anyhow!("AdjustTokenPrivileges error {err}"));
    }
    Ok(())
}

/// A read-only restricted token with the pre-D259 default DACL (no
/// per-command identity): legacy commands use
/// [`create_legacy_token_with_caps_from`] instead. Kept for tests that probe
/// Core's or the broker's objects from a restricted token.
///
/// # Safety
/// Caller must close the returned token handle; base_token must be a valid primary token.
pub unsafe fn create_readonly_token_with_cap_from(
    base_token: HANDLE,
    psid_capability: *mut c_void,
) -> Result<(HANDLE, *mut c_void)> {
    let new_token = create_token_with_caps_from(base_token, &[psid_capability], &[])?;
    Ok((new_token, psid_capability))
}

/// Create a restricted token that includes all provided capability SIDs.
///
/// # Safety
/// Caller must close the returned token handle; base_token must be a valid primary token.
pub unsafe fn create_workspace_write_token_with_caps_from(
    base_token: HANDLE,
    psid_capabilities: &[*mut c_void],
) -> Result<HANDLE> {
    create_token_with_caps_from(base_token, psid_capabilities, &[])
}

/// D259 (#1507): a legacy (non-elevated) command's token: the capability
/// SIDs plus `psid_command`, a SID unique to this command, as restricting
/// SIDs. Its default DACL, which the objects its processes create without
/// explicit security get (their processes, threads, tokens, pipes, events,
/// sections), grants full access only to the user, SYSTEM and
/// `psid_command`; Everyone, the logon SID and the capability SIDs get only
/// `SYNCHRONIZE` (D429). A write-restricted token needs a restricting SID granted
/// each write right, so one legacy command can still write to its own
/// objects, but not to another command's: it can't open them with
/// `PROCESS_CREATE_PROCESS` (to start a child in that command's job),
/// `WRITE_DAC`, `PROCESS_DUP_HANDLE` or memory writes. Before D259 the default
/// DACL granted Everyone and the logon SID, both restricting SIDs of every
/// legacy token, `GENERIC_ALL`. Not closed: `TOKEN_DUPLICATE` isn't a write
/// right, so another legacy command can still impersonate this command's
/// token, and with it (it carries `psid_command`) write to this command's
/// processes and writable roots (#1560).
///
/// D429 (#1803): until D429 the shared SIDs got read and execute, which on a
/// process is `PROCESS_VM_READ`, `PROCESS_QUERY_INFORMATION` and
/// `PROCESS_TERMINATE` and on a thread `THREAD_GET_CONTEXT`, so an elevated
/// command (another account, which holds Everyone and the logon SID) could
/// read a legacy command's memory and registers. `SYNCHRONIZE` is the only
/// right granted to them because one default DACL covers every object type:
/// `PROCESS_QUERY_LIMITED_INFORMATION` (0x1000) is `THREAD_RESUME` on a
/// thread, and `THREAD_QUERY_LIMITED_INFORMATION` (0x800) is
/// `PROCESS_SUSPEND_RESUME` on a process. Read rights pass a
/// write-restricted token on its normal SIDs alone, and every legacy command
/// holds the user SID, which keeps full access here (Core and the command
/// itself need it), so this doesn't stop another legacy command reading this
/// one (#1838).
///
/// # Safety
/// Caller must close the returned token handle; base_token must be a valid primary token,
/// and every SID pointer must be valid.
pub(crate) unsafe fn create_legacy_token_with_caps_from(
    base_token: HANDLE,
    psid_capabilities: &[*mut c_void],
    psid_command: *mut c_void,
) -> Result<HANDLE> {
    create_token(
        base_token,
        psid_capabilities,
        &[],
        DefaultDacl::Legacy { psid_command },
    )
}

/// Create a restricted token that includes all provided capability SIDs, the token user SID, and
/// any additional restricting SIDs.
///
/// This is intended for the elevated sandbox backend, where the token user is the dedicated
/// sandbox account rather than the real signed-in user.
///
/// # Safety
/// Caller must close the returned token handle; base_token must be a valid primary token.
pub unsafe fn create_workspace_write_token_with_caps_and_user_from(
    base_token: HANDLE,
    psid_capabilities: &[*mut c_void],
    additional_restricting_sids: &[*mut c_void],
) -> Result<HANDLE> {
    create_token_with_caps_user_and_additional_restrictions_from(
        base_token,
        psid_capabilities,
        additional_restricting_sids,
    )
}

/// Create a restricted token that includes all provided capability SIDs.
///
/// # Safety
/// Caller must close the returned token handle; base_token must be a valid primary token.
pub unsafe fn create_readonly_token_with_caps_from(
    base_token: HANDLE,
    psid_capabilities: &[*mut c_void],
) -> Result<HANDLE> {
    create_token_with_caps_from(base_token, psid_capabilities, &[])
}

/// Create a restricted token that includes all provided capability SIDs, the token user SID, and
/// any additional restricting SIDs.
///
/// This is intended for the elevated sandbox backend, where the token user is the dedicated
/// sandbox account rather than the real signed-in user.
///
/// # Safety
/// Caller must close the returned token handle; base_token must be a valid primary token.
pub unsafe fn create_readonly_token_with_caps_and_user_from(
    base_token: HANDLE,
    psid_capabilities: &[*mut c_void],
    additional_restricting_sids: &[*mut c_void],
) -> Result<HANDLE> {
    create_token_with_caps_user_and_additional_restrictions_from(
        base_token,
        psid_capabilities,
        additional_restricting_sids,
    )
}

/// D324 (#1648): an elevated command's token. Every elevated command runs as
/// the same sandbox account (per network mode), which is also a restricting
/// SID of its token, and the runners Core starts from one logon session share
/// their logon SID (the secondary logon service gives each the caller's), so
/// neither identifies one command. As D259 does for a legacy command, the
/// token gets a fresh SID of its own as an extra restricting SID, never
/// persisted or named by any ACL. Its default DACL grants full access only to
/// SYSTEM, the sandbox users' group (a normal group of every elevated token,
/// never a restricting SID, so it passes only the normal access check) and
/// that command SID: a write right needs both checks, so only this command
/// and its runner may write to its objects. The logon SID, the sandbox
/// account, Everyone and the capability SIDs get read and execute, and so
/// does `OWNER RIGHTS`: the sandbox account owns every elevated command's
/// objects, and an owner is otherwise always allowed `WRITE_DAC`. Before D324
/// the logon SID, Everyone and the capability SIDs, restricting SIDs of every
/// elevated token, had `GENERIC_ALL`, so any elevated command could open
/// another's processes for every write right. Not closed (as #1560 for
/// legacy): `TOKEN_DUPLICATE` isn't a write right, so the group still lets
/// another elevated command impersonate this command's token.
///
/// Fails if the runner isn't in the sandbox users' group.
unsafe fn create_token_with_caps_user_and_additional_restrictions_from(
    base_token: HANDLE,
    psid_capabilities: &[*mut c_void],
    additional_restricting_sids: &[*mut c_void],
) -> Result<HANDLE> {
    let mut group = sandbox_users_group_of(base_token)?;
    create_elevated_token_with_group(
        base_token,
        psid_capabilities,
        additional_restricting_sids,
        group.as_mut_ptr() as *mut c_void,
    )
}

/// Builds the token for
/// [`create_token_with_caps_user_and_additional_restrictions_from`], with
/// `psid_group` as the sandbox users' group in its default DACL and no check
/// that `base_token` holds it. Private: production reaches it only through
/// that function, after D324's group check. This module's tests call it
/// directly with a group the test process holds, because they run as a user
/// outside the sandbox users' group (#1795).
unsafe fn create_elevated_token_with_group(
    base_token: HANDLE,
    psid_capabilities: &[*mut c_void],
    additional_restricting_sids: &[*mut c_void],
    psid_group: *mut c_void,
) -> Result<HANDLE> {
    let mut user_sid_bytes = get_user_sid_bytes(base_token)?;
    let psid_user = user_sid_bytes.as_mut_ptr() as *mut c_void;
    let mut extra_restricting_sids = Vec::with_capacity(additional_restricting_sids.len() + 1);
    extra_restricting_sids.push(psid_user);
    extra_restricting_sids.extend_from_slice(additional_restricting_sids);
    // The token copies the SID, so it can go once the token exists.
    let command = LocalSid::from_string(&crate::cap::new_command_identity_sid_string())?;
    extra_restricting_sids.push(command.as_ptr());
    create_token(
        base_token,
        psid_capabilities,
        &extra_restricting_sids,
        DefaultDacl::Elevated {
            psid_user,
            psid_group,
            psid_command: command.as_ptr(),
        },
    )
}

unsafe fn create_token_with_caps_from(
    base_token: HANDLE,
    psid_capabilities: &[*mut c_void],
    extra_restricting_sids: &[*mut c_void],
) -> Result<HANDLE> {
    create_token(
        base_token,
        psid_capabilities,
        extra_restricting_sids,
        DefaultDacl::Shared,
    )
}

/// Which default DACL [`create_token`] gives the new token.
#[derive(Clone, Copy)]
enum DefaultDacl {
    /// `GENERIC_ALL` to the logon SID, Everyone and the capability SIDs (the
    /// pre-D259 DACL, kept for tests).
    Shared,
    /// D259: a legacy command's, with its command SID (also added as a
    /// restricting SID).
    Legacy { psid_command: *mut c_void },
    /// D324: an elevated command's; `psid_user` is the sandbox account,
    /// `psid_group` the sandbox users' group and `psid_command` the command's
    /// SID (already among the restricting SIDs).
    Elevated {
        psid_user: *mut c_void,
        psid_group: *mut c_void,
        psid_command: *mut c_void,
    },
}

unsafe fn create_token(
    base_token: HANDLE,
    psid_capabilities: &[*mut c_void],
    extra_restricting_sids: &[*mut c_void],
    default_dacl: DefaultDacl,
) -> Result<HANDLE> {
    if psid_capabilities.is_empty() {
        return Err(anyhow!("no capability SIDs provided"));
    }
    let mut logon_sid_bytes = get_logon_sid_bytes(base_token)?;
    let psid_logon = logon_sid_bytes.as_mut_ptr() as *mut c_void;
    let mut everyone = world_sid()?;
    let psid_everyone = everyone.as_mut_ptr() as *mut c_void;

    let mut extra_restricting_sids = extra_restricting_sids.to_vec();
    if let DefaultDacl::Legacy { psid_command } = default_dacl {
        extra_restricting_sids.push(psid_command);
    }

    // Exact order: Capabilities..., ExtraRestricting..., Logon, Everyone
    let mut entries: Vec<SID_AND_ATTRIBUTES> =
        vec![std::mem::zeroed(); psid_capabilities.len() + extra_restricting_sids.len() + 2];
    for (i, psid) in psid_capabilities.iter().enumerate() {
        entries[i].Sid = *psid;
        entries[i].Attributes = 0;
    }
    let extras_idx = psid_capabilities.len();
    for (i, psid) in extra_restricting_sids.iter().enumerate() {
        entries[extras_idx + i].Sid = *psid;
        entries[extras_idx + i].Attributes = 0;
    }
    let logon_idx = extras_idx + extra_restricting_sids.len();
    entries[logon_idx].Sid = psid_logon;
    entries[logon_idx].Attributes = 0;
    entries[logon_idx + 1].Sid = psid_everyone;
    entries[logon_idx + 1].Attributes = 0;

    let mut new_token: HANDLE = 0;
    // WRITE_RESTRICTED: restricting SIDs are only consulted for write accesses.
    // Observed on Windows (#158): generic-write rights, DELETE, and WRITE_DAC are
    // restricted, but FILE_DELETE_CHILD is not. NTFS lets a caller delete or move
    // an object it lacks DELETE on when the parent grants FILE_DELETE_CHILD, so a
    // parent that grants the user full control lets the sandbox delete or move
    // children that capability-SID ACEs protect. Legacy sessions close that route
    // with Low integrity outside writable roots (`integrity.rs`) and with
    // Everyone deny ACEs for protected paths inside them (`delete_child_route_denies`).
    let flags = DISABLE_MAX_PRIVILEGE | LUA_TOKEN | WRITE_RESTRICTED;
    let ok = CreateRestrictedToken(
        base_token,
        flags,
        0,
        std::ptr::null(),
        0,
        std::ptr::null(),
        entries.len() as u32,
        entries.as_mut_ptr(),
        &mut new_token,
    );
    if ok == 0 {
        return Err(anyhow!("CreateRestrictedToken failed: {}", GetLastError()));
    }

    // Additional restricting SIDs are identity markers, not capabilities. Deliberately
    // exclude them from the default DACL so possessing a route identity cannot grant
    // object access.
    let dacl = match default_dacl {
        DefaultDacl::Legacy { psid_command } => set_legacy_default_dacl(
            new_token,
            base_token,
            psid_command,
            psid_logon,
            psid_everyone,
            psid_capabilities,
        ),
        DefaultDacl::Elevated {
            psid_user,
            psid_group,
            psid_command,
        } => set_elevated_default_dacl(
            new_token,
            psid_user,
            psid_group,
            psid_command,
            psid_logon,
            psid_everyone,
            psid_capabilities,
        ),
        DefaultDacl::Shared => {
            let mut dacl_sids: Vec<*mut c_void> = Vec::with_capacity(psid_capabilities.len() + 2);
            dacl_sids.push(psid_logon);
            dacl_sids.push(psid_everyone);
            dacl_sids.extend_from_slice(psid_capabilities);
            set_default_dacl(new_token, &dacl_sids)
        }
    };
    if let Err(err) = dacl {
        CloseHandle(new_token);
        return Err(err);
    }

    if let Err(err) = enable_single_privilege(new_token, "SeChangeNotifyPrivilege") {
        CloseHandle(new_token);
        return Err(err);
    }
    Ok(new_token)
}

/// D259: sets a legacy command's default DACL (see
/// [`create_legacy_token_with_caps_from`]) on `token`; the user SID comes
/// from `base_token`.
unsafe fn set_legacy_default_dacl(
    token: HANDLE,
    base_token: HANDLE,
    psid_command: *mut c_void,
    psid_logon: *mut c_void,
    psid_everyone: *mut c_void,
    psid_capabilities: &[*mut c_void],
) -> Result<()> {
    let mut user = get_user_sid_bytes(base_token)?;
    let mut system = well_known_sid(WIN_LOCAL_SYSTEM_SID)?;
    let mut grants = vec![
        (user.as_mut_ptr() as *mut c_void, GENERIC_ALL),
        (system.as_mut_ptr() as *mut c_void, GENERIC_ALL),
        (psid_command, GENERIC_ALL),
        (psid_logon, SYNCHRONIZE),
        (psid_everyone, SYNCHRONIZE),
    ];
    grants.extend(psid_capabilities.iter().map(|sid| (*sid, SYNCHRONIZE)));
    set_default_dacl_grants(token, &grants)
}

/// D324: sets an elevated command's default DACL (see
/// [`create_token_with_caps_user_and_additional_restrictions_from`]) on
/// `token`.
unsafe fn set_elevated_default_dacl(
    token: HANDLE,
    psid_user: *mut c_void,
    psid_group: *mut c_void,
    psid_command: *mut c_void,
    psid_logon: *mut c_void,
    psid_everyone: *mut c_void,
    psid_capabilities: &[*mut c_void],
) -> Result<()> {
    let mut system = well_known_sid(WIN_LOCAL_SYSTEM_SID)?;
    let mut owner_rights = well_known_sid(WIN_CREATOR_OWNER_RIGHTS_SID)?;
    let read_execute = GENERIC_READ | GENERIC_EXECUTE;
    let mut grants = vec![
        (system.as_mut_ptr() as *mut c_void, GENERIC_ALL),
        (psid_group, GENERIC_ALL),
        (psid_command, GENERIC_ALL),
        (psid_logon, read_execute),
        (owner_rights.as_mut_ptr() as *mut c_void, read_execute),
        (psid_user, read_execute),
        (psid_everyone, read_execute),
    ];
    grants.extend(psid_capabilities.iter().map(|sid| (*sid, read_execute)));
    set_default_dacl_grants(token, &grants)
}

#[cfg(test)]
#[path = "token_tests.rs"]
mod tests;
