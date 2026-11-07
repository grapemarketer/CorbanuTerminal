//! D311/D313 (#1542): the pieces of our own tree walk, shared by D261's
//! below-check (and, later, the #158 label walk):
//! - [`par_walk`]: several threads take folders from a shared queue;
//! - each folder stays open while it is walked: [`list`] reads its entries
//!   through its handle and [`open_relative`] opens each entry relative to
//!   that handle, never following a link. A folder swapped for a link (or
//!   moved) meanwhile can't redirect the walk, and paths past `MAX_PATH`
//!   need no special form, because only one name is ever opened at a time.

use crate::winutil::to_wide;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use std::ffi::c_void;
use std::path::Path;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::PoisonError;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Foundation::ERROR_NO_MORE_FILES;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION;
use windows_sys::Win32::Storage::FileSystem::CreateFileW;
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
use windows_sys::Win32::Storage::FileSystem::FILE_LIST_DIRECTORY;
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE;
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE;
use windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandle;
use windows_sys::Win32::Storage::FileSystem::GetFileInformationByHandleEx;
use windows_sys::Win32::Storage::FileSystem::OPEN_EXISTING;
use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;

/// Most threads either walk uses (D312).
const MAX_THREADS: usize = 8;

fn cpus() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

/// Threads for a read-mostly check (D312): one per CPU, at most 8.
pub(crate) fn check_threads() -> usize {
    cpus().clamp(1, MAX_THREADS)
}

/// Runs `visit` on every item, the seeds and those it adds to its second
/// argument, on up to `threads` threads (the calling thread is one of them).
/// Stops at the first error or panic, which it returns; the remaining items
/// are dropped unvisited.
pub(crate) fn par_walk<T: Send>(
    seed: Vec<T>,
    threads: usize,
    visit: impl Fn(T, &mut Vec<T>) -> Result<()> + Sync,
) -> Result<()> {
    struct State<T> {
        queue: Vec<T>,
        active: usize,
        error: Option<anyhow::Error>,
    }
    let state = Mutex::new(State {
        queue: seed,
        active: 0,
        error: None,
    });
    let ready = Condvar::new();
    let worker = || {
        loop {
            let item = {
                let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
                loop {
                    if st.error.is_some() || (st.queue.is_empty() && st.active == 0) {
                        ready.notify_all();
                        return;
                    }
                    if let Some(item) = st.queue.pop() {
                        st.active += 1;
                        break item;
                    }
                    st = ready.wait(st).unwrap_or_else(PoisonError::into_inner);
                }
            };
            let mut found = Vec::new();
            // A panic must not leave the others waiting on this item.
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| visit(item, &mut found)))
                    .unwrap_or_else(|_| Err(anyhow!("the walk panicked")));
            let mut st = state.lock().unwrap_or_else(PoisonError::into_inner);
            st.active -= 1;
            match result {
                Ok(()) => st.queue.append(&mut found),
                Err(err) => {
                    if st.error.is_none() {
                        st.error = Some(err);
                    }
                }
            }
            drop(st);
            ready.notify_all();
        }
    };
    std::thread::scope(|scope| {
        for _ in 1..threads.max(1) {
            // A thread that can't start just leaves more for the others.
            let _ = std::thread::Builder::new().spawn_scoped(scope, worker);
        }
        worker();
    });
    match state
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
        .error
    {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

/// An open file or folder handle, closed on drop. `Send` so that folders can
/// move between the walk's threads.
pub(crate) struct Handle(pub(crate) HANDLE);

// SAFETY: a kernel handle may be used from any thread. Shared folder
// handles ([`Subfolder`]) are only used to open entries relative to them,
// which is safe concurrently; a folder is listed only by the one thread
// walking it.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

/// Volume serial number and 128-bit file ID.
pub(crate) type FileId = (u64, [u8; 16]);

impl Handle {
    /// Opens `path` with `access`, the link itself if it is one.
    pub(crate) unsafe fn open(path: &Path, access: u32) -> Result<Self> {
        Self::open_with(
            path,
            access,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
        )
    }

    pub(crate) unsafe fn open_with(path: &Path, access: u32, flags: u32) -> Result<Self> {
        let handle = CreateFileW(
            to_wide(path).as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            flags,
            0,
        );
        if handle == INVALID_HANDLE_VALUE {
            let err = std::io::Error::last_os_error();
            bail!("open {}: {err}", path.display());
        }
        Ok(Self(handle))
    }

    pub(crate) unsafe fn identity(&self) -> Result<FileId> {
        super::file_identity(self.0).map_err(|err| anyhow!("reading a file ID failed: {err}"))
    }

    pub(crate) unsafe fn attributes(&self) -> Result<u32> {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(self.0, &mut info) == 0 {
            bail!("GetFileInformationByHandle failed: {}", GetLastError());
        }
        Ok(info.dwFileAttributes)
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: a handle this type opened.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// A plain subfolder a walk found and still has to walk, not held open (a
/// folder with many subfolders would otherwise hold a handle for each): the
/// folder it was listed in (held, shared), its name and its identity.
pub(crate) struct Subfolder {
    parent: Arc<Handle>,
    name: Vec<u16>,
    identity: FileId,
}

impl Subfolder {
    /// `name` in `parent`, as `opened` (its handle) is now.
    pub(crate) unsafe fn new(
        parent: &Arc<Handle>,
        name: Vec<u16>,
        opened: &Handle,
    ) -> Result<Self> {
        Ok(Self {
            parent: Arc::clone(parent),
            name,
            identity: opened.identity()?,
        })
    }

    /// Opens it again through its folder for listing (plus `access`),
    /// failing unless it is still the same plain folder.
    pub(crate) unsafe fn reopen(&self, access: u32) -> Result<Handle> {
        let handle = open_relative(self.parent.0, &self.name, access | FILE_LIST_DIRECTORY)?;
        if handle.attributes()? & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)
            != FILE_ATTRIBUTE_DIRECTORY
            || handle.identity()? != self.identity
        {
            bail!(
                "{} changed while being walked",
                String::from_utf16_lossy(&self.name)
            );
        }
        Ok(handle)
    }
}

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: HANDLE,
    object_name: *const UnicodeString,
    attributes: u32,
    security_descriptor: *const c_void,
    security_quality_of_service: *const c_void,
}

#[repr(C)]
struct IoStatusBlock {
    status: usize,
    information: usize,
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtOpenFile(
        file_handle: *mut HANDLE,
        desired_access: u32,
        object_attributes: *const ObjectAttributes,
        io_status_block: *mut IoStatusBlock,
        share_access: u32,
        open_options: u32,
    ) -> i32;
}

const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
const FILE_OPEN_FOR_BACKUP_INTENT: u32 = 0x4000;
const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_READ_ATTRIBUTES: u32 = 0x80;
const FILE_FULL_DIRECTORY_INFO: i32 = 14;
const FILE_FULL_DIRECTORY_RESTART_INFO: i32 = 15;

/// Opens the entry `name` of the open folder `dir` with `access` (plus
/// attribute reads and synchronous I/O), the entry itself if it is a link.
/// The name is matched exactly, as [`list`] returned it.
pub(crate) unsafe fn open_relative(dir: HANDLE, name: &[u16], access: u32) -> Result<Handle> {
    // One plain name: never empty, a path or a stream (`name:stream`).
    let separators = [u16::from(b'\\'), u16::from(b'/'), u16::from(b':')];
    if name.is_empty() || name.iter().any(|c| *c == 0 || separators.contains(c)) {
        bail!("{:?} is not a plain name", String::from_utf16_lossy(name));
    }
    let bytes = u16::try_from(std::mem::size_of_val(name))
        .map_err(|_| anyhow!("a name of {} characters is too long", name.len()))?;
    let mut name_buf = name.to_vec();
    let unicode = UnicodeString {
        length: bytes,
        maximum_length: bytes,
        buffer: name_buf.as_mut_ptr(),
    };
    let attributes = ObjectAttributes {
        length: std::mem::size_of::<ObjectAttributes>() as u32,
        root_directory: dir,
        object_name: &unicode,
        attributes: 0,
        security_descriptor: std::ptr::null(),
        security_quality_of_service: std::ptr::null(),
    };
    let mut io = IoStatusBlock {
        status: 0,
        information: 0,
    };
    let mut handle: HANDLE = 0;
    let status = NtOpenFile(
        &mut handle,
        access | SYNCHRONIZE | FILE_READ_ATTRIBUTES,
        &attributes,
        &mut io,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_FOR_BACKUP_INTENT | FILE_OPEN_REPARSE_POINT,
    );
    if status != 0 || handle == 0 {
        if handle != 0 {
            CloseHandle(handle);
        }
        bail!(
            "opening {} failed: {status:#x}",
            String::from_utf16_lossy(name)
        );
    }
    Ok(Handle(handle))
}

/// The entries of the folder open as `dir` (with list access and
/// synchronous I/O) as (name, attributes), without `.` and `..`. A link's
/// attributes are its own.
pub(crate) unsafe fn list(dir: HANDLE) -> Result<Vec<(Vec<u16>, u32)>> {
    // `FILE_FULL_DIR_INFO`'s offsets.
    const NEXT: usize = 0;
    const ATTRIBUTES: usize = 56;
    const NAME_BYTES: usize = 60;
    const NAME: usize = 68;
    let mut out = Vec::new();
    let mut buffer = vec![0_u64; 8192];
    let mut class = FILE_FULL_DIRECTORY_RESTART_INFO;
    loop {
        if GetFileInformationByHandleEx(
            dir,
            class,
            buffer.as_mut_ptr() as *mut c_void,
            (buffer.len() * 8) as u32,
        ) == 0
        {
            let err = GetLastError();
            if err == ERROR_NO_MORE_FILES {
                return Ok(out);
            }
            bail!("listing a folder failed: {err}");
        }
        class = FILE_FULL_DIRECTORY_INFO;
        let base = buffer.as_ptr() as *const u8;
        let mut offset = 0_usize;
        loop {
            let entry = base.add(offset);
            let next = std::ptr::read_unaligned(entry.add(NEXT) as *const u32) as usize;
            let attributes = std::ptr::read_unaligned(entry.add(ATTRIBUTES) as *const u32);
            let name_len =
                std::ptr::read_unaligned(entry.add(NAME_BYTES) as *const u32) as usize / 2;
            let name: Vec<u16> = (0..name_len)
                .map(|i| std::ptr::read_unaligned(entry.add(NAME + 2 * i) as *const u16))
                .collect();
            let dot = u16::from(b'.');
            if name != [dot] && name != [dot, dot] {
                out.push((name, attributes));
            }
            if next == 0 {
                break;
            }
            offset += next;
        }
    }
}

#[cfg(test)]
#[path = "acl_walk_tests.rs"]
mod tests;
