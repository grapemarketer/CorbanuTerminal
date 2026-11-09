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

/// D446: whether `path` is on a network: a UNC path (`\\server\share`,
/// `\\?\UNC\server\share`) or, on Windows, on a drive letter Windows reports
/// as remote (a mapped network drive). Device paths (`\\.\`) and verbatim
/// drive paths (`\\?\C:\`) are judged by their drive.
pub fn is_network_path(path: &std::path::Path) -> bool {
    let text = path.to_string_lossy();
    let rest = match text
        .strip_prefix(r"\\?\")
        .or_else(|| text.strip_prefix(r"\\.\"))
    {
        Some(rest) => {
            if rest
                .get(..4)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(r"UNC\"))
            {
                return true;
            }
            rest
        }
        None if text.starts_with(r"\\") || text.starts_with("//") => return true,
        None => &text,
    };
    let mut chars = rest.chars();
    match (chars.next(), chars.next()) {
        (Some(letter), Some(':')) => is_remote_drive(letter),
        _ => false,
    }
}

#[cfg(target_os = "windows")]
fn is_remote_drive(letter: char) -> bool {
    use windows_sys::Win32::Storage::FileSystem::GetDriveTypeW;
    /// `DRIVE_REMOTE` from `GetDriveTypeW`.
    const DRIVE_REMOTE: u32 = 4;
    if !letter.is_ascii_alphabetic() {
        return false;
    }
    let root = crate::winutil::to_wide(format!("{letter}:\\"));
    // SAFETY: a NUL-terminated wide string.
    (unsafe { GetDriveTypeW(root.as_ptr()) }) == DRIVE_REMOTE
}

#[cfg(not(target_os = "windows"))]
fn is_remote_drive(_letter: char) -> bool {
    false
}

#[cfg(test)]
mod network_path_tests {
    use super::is_network_path;
    use std::path::Path;

    #[test]
    fn d446_unc_paths_are_network_paths() {
        for path in [
            r"\\server\share\work",
            r"\\localhost\C$\work",
            r"\\127.0.0.1\share",
            r"\\?\UNC\server\share\work",
            r"\\?\unc\server\share",
            r"\\.\UNC\server\share",
            "//server/share/work",
        ] {
            assert!(is_network_path(Path::new(path)), "{path}");
        }
        for path in [r"\\?\C:\work", r"\\.\C:\work", "relative", "/home/work"] {
            assert!(!is_network_path(Path::new(path)), "{path}");
        }
    }

    /// The system drive is local.
    #[cfg(target_os = "windows")]
    #[test]
    fn d446_the_system_drive_is_not_a_network_path() {
        let drive = std::env::var("SystemDrive").expect("SystemDrive");
        assert!(!is_network_path(Path::new(&format!(r"{drive}\Windows"))));
        assert!(!is_network_path(Path::new(&format!(
            r"\\?\{drive}\Windows"
        ))));
    }
}
