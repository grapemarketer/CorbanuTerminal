//! #1261: whether a drive letter is a local fixed disk, for callers that
//! must never send a file lookup to a network share.

/// File attributes a lookup must not go past: links, junctions and mount
/// points (reparse points), and files whose data lives elsewhere (offline or
/// cloud files, which a read would fetch).
#[cfg(target_os = "windows")]
pub const REMOTE_OR_LINK_ATTRIBUTES: u32 = {
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_OFFLINE;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_RECALL_ON_OPEN;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    FILE_ATTRIBUTE_REPARSE_POINT
        | FILE_ATTRIBUTE_OFFLINE
        | FILE_ATTRIBUTE_RECALL_ON_OPEN
        | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
};

/// A system file: Cygwin, MSYS2 and Git Bash read one starting `!<symlink>`
/// (and Interix one starting `IntxLNK`) as a link to follow.
#[cfg(target_os = "windows")]
pub const SYSTEM_FILE: u32 = windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_SYSTEM;

/// `CreateFileW` flags: open a link itself, never what it points to, and
/// open a folder as well as a file.
#[cfg(target_os = "windows")]
pub const OPEN_ITSELF: u32 = windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT
    | windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;

/// `CreateFileW` sharing that lets others read and write but not delete or
/// rename what is open (so a folder held open cannot be swapped).
#[cfg(target_os = "windows")]
pub const SHARE_NOT_DELETE: u32 = windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ
    | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE;

/// Whether drive `letter` is a fixed disk volume on this machine: its DOS
/// device names a hard disk volume (not a network redirector, a `subst`
/// folder or anything else) and Windows reports it as fixed. Neither call
/// does I/O on the drive itself.
#[cfg(target_os = "windows")]
pub fn is_local_fixed(letter: char) -> bool {
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    use windows_sys::Win32::Storage::FileSystem::QueryDosDeviceW;
    /// `DRIVE_FIXED` from `GetDriveTypeW`.
    const DRIVE_FIXED: u32 = 3;
    if !letter.is_ascii_alphabetic() {
        return false;
    }
    let device = crate::winutil::to_wide(format!("{letter}:"));
    let mut target = [0u16; 1024];
    // SAFETY: a NUL-terminated wide string and a buffer of the stated length.
    let written =
        unsafe { QueryDosDeviceW(device.as_ptr(), target.as_mut_ptr(), target.len() as u32) };
    if written == 0 {
        return false;
    }
    let end = target
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(target.len());
    let target = String::from_utf16_lossy(&target[..end]);
    let volume = r"\Device\HarddiskVolume";
    let names_a_volume = target
        .get(..volume.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(volume));
    if !names_a_volume {
        return false;
    }
    let root = crate::winutil::to_wide(format!("{letter}:\\"));
    // SAFETY: a NUL-terminated wide string.
    (unsafe { GetDriveTypeW(root.as_ptr()) }) == DRIVE_FIXED
}

/// Elsewhere no drive letter is proven local.
#[cfg(not(target_os = "windows"))]
pub fn is_local_fixed(_letter: char) -> bool {
    false
}
