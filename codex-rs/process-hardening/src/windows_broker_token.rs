//! PF-27-S08: the token the Windows credential broker runs under.
//!
//! A restricted copy of the starting process's token, chosen over an
//! AppContainer (see the PF-27-S08 record and evidence):
//! - write-restricted to a fresh random capability SID that no object grants,
//!   plus the logon SID and Everyone. Every write access check then also
//!   needs an entry for one of those: the broker can write only objects that
//!   grant them, such as its own pipes (which name the capability SID). The
//!   logon SID is there because the window station and desktop grant it and
//!   a process that loads `user32` does not start without them (measured:
//!   `0xC0000142`); Everyone because devices such as the network stack grant
//!   it;
//! - low integrity, so the default no-write-up label of everything at medium
//!   or above refuses it too, and the no-read-up label of processes and
//!   threads keeps it out of the user's other processes beyond query-limited
//!   access. WMI refuses it; Task Scheduler does not show it the task folders.
//!   (Untrusted integrity was measured too: the broker does not start.);
//! - every privilege but `SeChangeNotifyPrivilege` removed, Administrators
//!   (if present) and INTERACTIVE made deny-only. INTERACTIVE because COM's
//!   default launch permission grants it: without it the broker cannot launch
//!   an out-of-process COM server that keeps the defaults (measured: with it,
//!   `MMC20.Application` launched).
//!
//! Reads are unchanged: the broker still loads its image, reads the
//! certificate store, resolves names and connects out (loopback too, which
//! an AppContainer would refuse), and it can read Credential Manager. Known
//! gap (as for the sandbox's write-restricted token, #158): write
//! restriction does not cover `FILE_DELETE_CHILD`, so in a folder at low
//! integrity that grants the user full control (`LocalLow`, `Temp\Low`) the
//! broker can delete a file, though not create, change or rename one.
//! The same holds in `%TEMP%` (and any other writable root) while a legacy
//! sandbox session holds it at Low integrity (#158 option (b), the sandbox's
//! `integrity_leases`), until its label is put back once no session uses it
//! (#1610).
//! Only a restricted version of the caller's own token, so starting a process
//! with it needs no privilege.

use crate::windows_process_access::SecurityDescriptor;
use crate::windows_process_access::current_user_sid_string;
use crate::windows_process_access::thread_dacl_sddl;
use std::ffi::c_void;
use std::io;
use std::os::windows::io::AsRawHandle as _;
use std::os::windows::io::FromRawHandle as _;
use std::os::windows::io::OwnedHandle;
use std::ptr;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Foundation::HLOCAL;
use windows_sys::Win32::Foundation::LUID;
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Foundation::PSID;
use windows_sys::Win32::Security::ACE_HEADER;
use windows_sys::Win32::Security::ACL;
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
use windows_sys::Win32::Security::CreateRestrictedToken;
use windows_sys::Win32::Security::DISABLE_MAX_PRIVILEGE;
use windows_sys::Win32::Security::EqualSid;
use windows_sys::Win32::Security::GetAce;
use windows_sys::Win32::Security::GetLengthSid;
use windows_sys::Win32::Security::GetSidSubAuthority;
use windows_sys::Win32::Security::GetSidSubAuthorityCount;
use windows_sys::Win32::Security::GetTokenInformation;
use windows_sys::Win32::Security::SID_AND_ATTRIBUTES;
use windows_sys::Win32::Security::SetTokenInformation;
use windows_sys::Win32::Security::TOKEN_ADJUST_DEFAULT;
use windows_sys::Win32::Security::TOKEN_ASSIGN_PRIMARY;
use windows_sys::Win32::Security::TOKEN_DEFAULT_DACL;
use windows_sys::Win32::Security::TOKEN_DUPLICATE;
use windows_sys::Win32::Security::TOKEN_GROUPS;
use windows_sys::Win32::Security::TOKEN_INFORMATION_CLASS;
use windows_sys::Win32::Security::TOKEN_MANDATORY_LABEL;
use windows_sys::Win32::Security::TOKEN_PRIVILEGES;
use windows_sys::Win32::Security::TOKEN_QUERY;
use windows_sys::Win32::Security::TOKEN_USER;
use windows_sys::Win32::Security::TokenDefaultDacl;
use windows_sys::Win32::Security::TokenGroups;
use windows_sys::Win32::Security::TokenIntegrityLevel;
use windows_sys::Win32::Security::TokenPrivileges;
use windows_sys::Win32::Security::TokenRestrictedSids;
use windows_sys::Win32::Security::TokenUser;
use windows_sys::Win32::Security::WRITE_RESTRICTED;
use windows_sys::Win32::System::Threading::GetCurrentProcess;
use windows_sys::Win32::System::Threading::OpenProcessToken;

/// The low mandatory level (`S-1-16-4096`).
const LOW_INTEGRITY_SID: &str = "S-1-16-4096";
const LOW_INTEGRITY_RID: u32 = 0x1000;
const ADMINISTRATORS_SID: &str = "S-1-5-32-544";
/// INTERACTIVE: COM's default launch permission grants it (with
/// Administrators and SYSTEM), so without it the broker cannot launch an
/// out-of-process COM server that keeps the defaults.
const INTERACTIVE_SID: &str = "S-1-5-4";
/// Made deny-only in the broker token.
const DENY_ONLY_SIDS: [&str; 2] = [ADMINISTRATORS_SID, INTERACTIVE_SID];
const EVERYONE_SID: &str = "S-1-1-0";
/// `SE_GROUP_LOGON_ID`.
const SE_GROUP_LOGON_ID: u32 = 0xC000_0000;
/// `SE_GROUP_INTEGRITY`.
const SE_GROUP_INTEGRITY: u32 = 0x20;
const SE_GROUP_ENABLED: u32 = 0x4;
const SE_GROUP_USE_FOR_DENY_ONLY: u32 = 0x10;
/// The LUID of `SeChangeNotifyPrivilege` (bypass traverse checking), the one
/// privilege `DISABLE_MAX_PRIVILEGE` keeps.
const SE_CHANGE_NOTIFY_PRIVILEGE: i64 = 23;

/// The default DACL of the broker's token: what objects it creates without
/// a descriptor get, its threads included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BrokerDefaultDacl {
    /// The protected thread DACL (PF-27-S07): no other process of the user
    /// can open those objects. The broker uses this.
    Protected,
    /// The user and the capability SID get full access, so a program that
    /// reopens what it creates (PowerShell) runs. Probes only.
    #[cfg(test)]
    OwnedByCapability,
}

/// What the broker token is built from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BrokerTokenOptions {
    pub(crate) default_dacl: BrokerDefaultDacl,
}

/// The broker's token.
pub(crate) const BROKER_TOKEN: BrokerTokenOptions = BrokerTokenOptions {
    default_dacl: BrokerDefaultDacl::Protected,
};

/// A new primary token for the broker (see the module docs), with a fresh
/// capability SID.
pub(crate) fn create_broker_token(options: BrokerTokenOptions) -> io::Result<OwnedHandle> {
    let default_dacl = options.default_dacl;
    // TOKEN_ASSIGN_PRIMARY: the restricted token is used as a primary token
    // in CreateProcessAsUserW. TOKEN_ADJUST_DEFAULT: the restricted token's
    // default DACL is set below, and that right is inherited from the base.
    let base = open_current_token(
        TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY | TOKEN_ADJUST_DEFAULT,
    )?;
    let capability = LocalSid::from_string(&random_capability_sid()?)?;
    let everyone = LocalSid::from_string(EVERYONE_SID)?;
    let deny_only = DENY_ONLY_SIDS
        .iter()
        .map(|sid| LocalSid::from_string(sid))
        .collect::<io::Result<Vec<_>>>()?;
    let disable: Vec<SID_AND_ATTRIBUTES> = deny_only
        .iter()
        .map(|sid| SID_AND_ATTRIBUTES {
            Sid: sid.0,
            Attributes: 0,
        })
        .collect();
    let groups_buffer = token_information(base.as_raw_handle() as HANDLE, TokenGroups)?;
    let logon = groups(&groups_buffer)
        .iter()
        .find(|group| group.Attributes & SE_GROUP_LOGON_ID == SE_GROUP_LOGON_ID)
        .map(|group| group.Sid);
    let logon = logon.ok_or_else(|| io::Error::other("the token has no logon SID"))?;
    // The logon SID: the window station and desktop grant it, and a process
    // that loads `user32` cannot start without them. Everyone: devices such
    // as the network stack grant it. Low integrity still keeps both from
    // writing anything of the user's at medium or above.
    let restrict = [capability.0, logon, everyone.0].map(|sid| SID_AND_ATTRIBUTES {
        Sid: sid,
        Attributes: 0,
    });
    let mut raw: HANDLE = 0;
    // SAFETY: every array outlives the call; Administrators is ignored if
    // the token does not have it.
    let ok = unsafe {
        CreateRestrictedToken(
            base.as_raw_handle() as HANDLE,
            DISABLE_MAX_PRIVILEGE | WRITE_RESTRICTED,
            disable.len() as u32,
            disable.as_ptr(),
            /*deleteprivilegecount*/ 0,
            ptr::null(),
            restrict.len() as u32,
            restrict.as_ptr(),
            &mut raw,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: returned by the call above; owned from here on.
    let token = unsafe { OwnedHandle::from_raw_handle(raw as _) };
    set_low_integrity(&token)?;
    let user = current_user_sid_string()?;
    let sddl = match default_dacl {
        BrokerDefaultDacl::Protected => thread_dacl_sddl(&user),
        #[cfg(test)]
        BrokerDefaultDacl::OwnedByCapability => format!(
            "D:(A;;GA;;;{user})(A;;GA;;;{})(A;;GA;;;SY)",
            sid_string(capability.0)?
        ),
    };
    set_default_dacl(token.as_raw_handle() as HANDLE, &sddl)?;
    Ok(token)
}

/// Checks that the current process runs under a broker token: low integrity
/// or below, write-restricted to SIDs it does not otherwise hold, no
/// privilege but `SeChangeNotifyPrivilege`, Administrators and INTERACTIVE
/// deny-only.
pub fn current_token_is_broker_token() -> io::Result<()> {
    let token = open_current_token(TOKEN_QUERY)?;
    let token = token.as_raw_handle() as HANDLE;

    let label = token_information(token, TokenIntegrityLevel)?;
    // SAFETY: the buffer holds a TOKEN_MANDATORY_LABEL whose SID points
    // into it.
    let level = unsafe {
        let sid = (*label.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()).Label.Sid;
        let count = u32::from(*GetSidSubAuthorityCount(sid));
        *GetSidSubAuthority(sid, count.saturating_sub(1))
    };
    if level > LOW_INTEGRITY_RID {
        return Err(io::Error::other(format!(
            "integrity level 0x{level:x} is above low"
        )));
    }

    let restricted = token_information(token, TokenRestrictedSids)?;
    let restricted = groups(&restricted);
    if restricted.is_empty() {
        return Err(io::Error::other("the token is not restricted"));
    }
    let user_buffer = token_information(token, TokenUser)?;
    // SAFETY: the buffer holds a TOKEN_USER whose SID points into it.
    let user = unsafe { (*user_buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let group_buffer = token_information(token, TokenGroups)?;
    let held = groups(&group_buffer);
    // Every restricting SID is a capability (held by nothing else in the
    // token), the logon SID or Everyone; at least one is a capability.
    let everyone = LocalSid::from_string(EVERYONE_SID)?;
    let mut capabilities = 0;
    for restricting in restricted {
        // SAFETY: both SIDs point into live buffers.
        let same = |sid: PSID| unsafe { EqualSid(restricting.Sid, sid) != 0 };
        if same(everyone.0) {
            continue;
        }
        match held.iter().find(|group| same(group.Sid)) {
            Some(group) if group.Attributes & SE_GROUP_LOGON_ID == SE_GROUP_LOGON_ID => {}
            None if !same(user) => capabilities += 1,
            _ => {
                return Err(io::Error::other(format!(
                    "restricting SID {} is one the token holds",
                    sid_string(restricting.Sid)?
                )));
            }
        }
    }
    if capabilities == 0 {
        return Err(io::Error::other("no capability among the restricting SIDs"));
    }

    for deny_only in DENY_ONLY_SIDS {
        let deny_only_sid = LocalSid::from_string(deny_only)?;
        let usable = held.iter().any(|group| {
            // SAFETY: both SIDs are live.
            let same = unsafe { EqualSid(group.Sid, deny_only_sid.0) != 0 };
            same && (group.Attributes & SE_GROUP_ENABLED != 0
                || group.Attributes & SE_GROUP_USE_FOR_DENY_ONLY == 0)
        });
        if usable {
            return Err(io::Error::other(format!("{deny_only} is enabled")));
        }
    }

    let privileges = token_information(token, TokenPrivileges)?;
    // SAFETY: the buffer holds a TOKEN_PRIVILEGES with `PrivilegeCount`
    // entries.
    let extra = unsafe {
        let list = privileges.as_ptr().cast::<TOKEN_PRIVILEGES>();
        let count = (*list).PrivilegeCount as usize;
        std::slice::from_raw_parts((*list).Privileges.as_ptr(), count)
            .iter()
            .filter(|privilege| luid_value(privilege.Luid) != SE_CHANGE_NOTIFY_PRIVILEGE)
            .count()
    };
    if extra != 0 {
        return Err(io::Error::other(format!(
            "{extra} privileges besides SeChangeNotify"
        )));
    }
    Ok(())
}

/// The capability SIDs of the current process's token (restricting SIDs
/// that are neither Everyone nor held by the token otherwise), as strings;
/// none for an unrestricted token. The broker's pipes grant them, so the
/// broker can create its own pipe instances.
pub fn current_capability_sid_strings() -> io::Result<Vec<String>> {
    let token = open_current_token(TOKEN_QUERY)?;
    let token = token.as_raw_handle() as HANDLE;
    let restricted = token_information(token, TokenRestrictedSids)?;
    let user_buffer = token_information(token, TokenUser)?;
    // SAFETY: the buffer holds a TOKEN_USER whose SID points into it.
    let user = unsafe { (*user_buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    let group_buffer = token_information(token, TokenGroups)?;
    let held = groups(&group_buffer);
    let everyone = LocalSid::from_string(EVERYONE_SID)?;
    groups(&restricted)
        .iter()
        .filter(|restricting| {
            // SAFETY: every SID points into a live buffer.
            let same = |sid: PSID| unsafe { EqualSid(restricting.Sid, sid) != 0 };
            !same(everyone.0) && !same(user) && !held.iter().any(|group| same(group.Sid))
        })
        .map(|restricting| sid_string(restricting.Sid))
        .collect()
}

/// True if the current token's default DACL is the protected thread DACL
/// (the broker's token gets it before the broker runs).
pub(crate) fn default_dacl_is_protected() -> io::Result<bool> {
    let token = open_current_token(TOKEN_QUERY)?;
    let buffer = token_information(token.as_raw_handle() as HANDLE, TokenDefaultDacl)?;
    // SAFETY: the buffer holds a TOKEN_DEFAULT_DACL pointing into it.
    let actual = unsafe { (*buffer.as_ptr().cast::<TOKEN_DEFAULT_DACL>()).DefaultDacl };
    let expected = SecurityDescriptor::from_sddl(&thread_dacl_sddl(&current_user_sid_string()?))?;
    let expected = expected.dacl()?;
    // SAFETY: both ACLs are live for the comparison.
    Ok(!actual.is_null() && unsafe { same_aces(actual, expected) })
}

/// Compares two ACLs entry by entry.
unsafe fn same_aces(a: *const ACL, b: *const ACL) -> bool {
    // SAFETY: the caller passes valid ACLs.
    unsafe {
        if (*a).AceCount != (*b).AceCount {
            return false;
        }
        (0..u32::from((*a).AceCount)).all(|index| {
            let mut x: *mut c_void = ptr::null_mut();
            let mut y: *mut c_void = ptr::null_mut();
            if GetAce(a, index, &mut x) == 0 || GetAce(b, index, &mut y) == 0 {
                return false;
            }
            let size = (*x.cast::<ACE_HEADER>()).AceSize;
            size == (*y.cast::<ACE_HEADER>()).AceSize
                && std::slice::from_raw_parts(x.cast::<u8>(), usize::from(size))
                    == std::slice::from_raw_parts(y.cast::<u8>(), usize::from(size))
        })
    }
}

fn luid_value(luid: LUID) -> i64 {
    (i64::from(luid.HighPart) << 32) | i64::from(luid.LowPart)
}

fn open_current_token(access: u32) -> io::Result<OwnedHandle> {
    let mut token: HANDLE = 0;
    // SAFETY: the pseudo-handle is valid; `token` is a valid out pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: opened above; owned from here on.
    Ok(unsafe { OwnedHandle::from_raw_handle(token as _) })
}

fn set_low_integrity(token: &OwnedHandle) -> io::Result<()> {
    let low = LocalSid::from_string(LOW_INTEGRITY_SID)?;
    let label = TOKEN_MANDATORY_LABEL {
        Label: SID_AND_ATTRIBUTES {
            Sid: low.0,
            Attributes: SE_GROUP_INTEGRITY,
        },
    };
    // SAFETY: `label` and the SID it points to outlive the call.
    let ok = unsafe {
        SetTokenInformation(
            token.as_raw_handle() as HANDLE,
            TokenIntegrityLevel,
            (&label as *const TOKEN_MANDATORY_LABEL).cast(),
            std::mem::size_of::<TOKEN_MANDATORY_LABEL>() as u32 + GetLengthSid(low.0),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn set_default_dacl(token: HANDLE, sddl: &str) -> io::Result<()> {
    let descriptor = SecurityDescriptor::from_sddl(sddl)?;
    let value = TOKEN_DEFAULT_DACL {
        DefaultDacl: descriptor.dacl()?.cast_mut(),
    };
    // SAFETY: `value` points into `descriptor`, which outlives the call.
    let ok = unsafe {
        SetTokenInformation(
            token,
            TokenDefaultDacl,
            (&value as *const TOKEN_DEFAULT_DACL).cast(),
            std::mem::size_of::<TOKEN_DEFAULT_DACL>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// A `TOKEN_GROUPS`-shaped buffer's entries.
fn groups(buffer: &[u64]) -> &[SID_AND_ATTRIBUTES] {
    // SAFETY: the buffer was filled with a TOKEN_GROUPS (`GroupCount`
    // entries) by GetTokenInformation.
    unsafe {
        let list = buffer.as_ptr().cast::<TOKEN_GROUPS>();
        std::slice::from_raw_parts((*list).Groups.as_ptr(), (*list).GroupCount as usize)
    }
}

/// The token information of `class`, in u64 storage so the structures in it
/// are aligned.
fn token_information(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<u64>> {
    let mut needed = 0_u32;
    // SAFETY: a size query with a null buffer.
    unsafe { GetTokenInformation(token, class, ptr::null_mut(), 0, &mut needed) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0_u64; (needed as usize).div_ceil(8)];
    // SAFETY: `buffer` holds at least `needed` bytes.
    let ok = unsafe {
        GetTokenInformation(
            token,
            class,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(buffer)
}

fn sid_string(sid: PSID) -> io::Result<String> {
    let mut wide: *mut u16 = ptr::null_mut();
    // SAFETY: `sid` is a valid SID; `wide` is freed below.
    if unsafe { ConvertSidToStringSidW(sid, &mut wide) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `wide` is a NUL-terminated string allocated by the call.
    let text = unsafe {
        let len = (0..).take_while(|&i| *wide.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(wide, len))
    };
    // SAFETY: allocated with LocalAlloc by ConvertSidToStringSidW.
    unsafe { LocalFree(wide as HLOCAL) };
    Ok(text)
}

/// A random SID in the same form the sandbox uses for its capability SIDs;
/// no object grants it unless told to.
fn random_capability_sid() -> io::Result<String> {
    let mut parts = [0_u32; 4];
    // SAFETY: `parts` is a writable buffer of the size passed.
    if unsafe {
        RtlGenRandom(
            parts.as_mut_ptr().cast(),
            std::mem::size_of_val(&parts) as u32,
        )
    } == 0
    {
        return Err(io::Error::other("RtlGenRandom failed"));
    }
    let [a, b, c, d] = parts;
    Ok(format!("S-1-5-21-{a}-{b}-{c}-{d}"))
}

#[link(name = "advapi32")]
unsafe extern "system" {
    /// `RtlGenRandom`; exported under this name.
    #[link_name = "SystemFunction036"]
    fn RtlGenRandom(buffer: *mut c_void, length: u32) -> u8;
}

/// A SID allocated by `ConvertStringSidToSidW`, freed on drop.
struct LocalSid(PSID);

impl LocalSid {
    fn from_string(sid: &str) -> io::Result<Self> {
        let wide: Vec<u16> = sid.encode_utf16().chain(std::iter::once(0)).collect();
        let mut raw: PSID = ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated; `raw` is freed on drop.
        if unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut raw) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(raw))
    }
}

impl Drop for LocalSid {
    fn drop(&mut self) {
        // SAFETY: allocated with LocalAlloc by ConvertStringSidToSidW.
        unsafe { LocalFree(self.0 as HLOCAL) };
    }
}

#[cfg(test)]
#[path = "windows_broker_token_tests.rs"]
mod tests;
