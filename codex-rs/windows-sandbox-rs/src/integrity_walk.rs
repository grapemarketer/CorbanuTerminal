//! D311 part 2 (#1542): `SetNamedSecurityInfoW(path, LABEL_SECURITY_INFORMATION)`
//! without Windows' tree walk, with the same result.
//!
//! Every #158 label write (a writable root's Low label, a protected path's
//! Medium label, putting a root's label back) rewrites the label of every
//! object below, recomputing each from its parent's: about 2.5 s each way on
//! a 101,000-file workspace hosted, 8 s on a desktop. This walk does the
//! same work faster:
//! - each object's label is what Windows computes from its parent's new
//!   descriptor and its own (`CreatePrivateObjectSecurityEx`, auto-inherit),
//!   cached per folder by the inputs it depends on, and is written only if
//!   it differs;
//! - folders are walked on several threads (D312: half the CPUs, at most 8;
//!   label writes contend), each held open while its entries are listed and
//!   opened through its handle (`crate::acl::walk`), so a folder swapped for
//!   a link meanwhile can't redirect the walk, and long paths need nothing
//!   special;
//! - links (name-surrogate reparse points) are labelled as objects and not
//!   followed, as Windows' walk does; other reparse points, such as cloud-file
//!   placeholder folders, are walked into, as Windows' walk does too.
//!
//! Any failure leaves the caller to make the `SetNamedSecurityInfoW` call,
//! which rewrites every label below `path` from the top again, so a partial
//! walk leaves nothing behind.

use crate::acl::FILE_MAPPING;
use crate::acl::walk::Handle;
use crate::acl::walk::Kind;
use crate::acl::walk::Subfolder;
use crate::acl::walk::label_threads;
use crate::acl::walk::list;
use crate::acl::walk::open_relative;
use crate::acl::walk::par_walk;
use anyhow::Result;
use anyhow::bail;
use std::collections::HashMap;
use std::ffi::OsString;
use std::ffi::c_void;
use std::os::windows::ffi::OsStringExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Security::ACE_HEADER;
use windows_sys::Win32::Security::ACL;
use windows_sys::Win32::Security::ACL_SIZE_INFORMATION;
use windows_sys::Win32::Security::AclSizeInformation;
use windows_sys::Win32::Security::CreatePrivateObjectSecurityEx;
use windows_sys::Win32::Security::DestroyPrivateObjectSecurity;
use windows_sys::Win32::Security::GROUP_SECURITY_INFORMATION;
use windows_sys::Win32::Security::GetAce;
use windows_sys::Win32::Security::GetAclInformation;
use windows_sys::Win32::Security::GetKernelObjectSecurity;
use windows_sys::Win32::Security::GetLengthSid;
use windows_sys::Win32::Security::GetSecurityDescriptorControl;
use windows_sys::Win32::Security::GetSecurityDescriptorGroup;
use windows_sys::Win32::Security::GetSecurityDescriptorLength;
use windows_sys::Win32::Security::GetSecurityDescriptorOwner;
use windows_sys::Win32::Security::GetSecurityDescriptorSacl;
use windows_sys::Win32::Security::INHERITED_ACE;
use windows_sys::Win32::Security::InitializeSecurityDescriptor;
use windows_sys::Win32::Security::LABEL_SECURITY_INFORMATION;
use windows_sys::Win32::Security::OWNER_SECURITY_INFORMATION;
use windows_sys::Win32::Security::SE_SACL_AUTO_INHERIT_REQ;
use windows_sys::Win32::Security::SE_SACL_AUTO_INHERITED;
use windows_sys::Win32::Security::SE_SACL_PRESENT;
use windows_sys::Win32::Security::SE_SACL_PROTECTED;
use windows_sys::Win32::Security::SECURITY_DESCRIPTOR;
use windows_sys::Win32::Security::SEF_AVOID_OWNER_CHECK;
use windows_sys::Win32::Security::SEF_AVOID_PRIVILEGE_CHECK;
use windows_sys::Win32::Security::SEF_SACL_AUTO_INHERIT;
use windows_sys::Win32::Security::SetKernelObjectSecurity;
use windows_sys::Win32::Security::SetSecurityDescriptorControl;
use windows_sys::Win32::Security::SetSecurityDescriptorGroup;
use windows_sys::Win32::Security::SetSecurityDescriptorOwner;
use windows_sys::Win32::Security::SetSecurityDescriptorSacl;
use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
use windows_sys::Win32::Storage::FileSystem::FILE_LIST_DIRECTORY;
use windows_sys::Win32::Storage::FileSystem::READ_CONTROL;
use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
use windows_sys::Win32::Storage::FileSystem::WRITE_OWNER;
use windows_sys::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;
use windows_sys::Win32::System::SystemServices::SYSTEM_MANDATORY_LABEL_ACE_TYPE;

/// The SACL control bits that decide (or show) a label.
const SACL_BITS: u16 = SE_SACL_PROTECTED | SE_SACL_AUTO_INHERITED | SE_SACL_PRESENT;

/// Sets the label SACL `sacl` (`path`'s explicit label entries) on `path`
/// and recomputes the label of every object below it, as
/// `SetNamedSecurityInfoW(path, LABEL_SECURITY_INFORMATION, sacl)` does.
/// Follows a link at `path` itself, as that call does. On an error, part of
/// the tree may be written; the caller then makes that call.
///
/// # Safety
/// `sacl` must be a valid ACL; calls Win32 security APIs on `path`.
pub(super) unsafe fn set_label_tree(path: &Path, sacl: *mut ACL) -> Result<()> {
    let Some(parent_path) = path.parent() else {
        bail!("{} has no parent", path.display());
    };
    // By name, as `SetNamedSecurityInfoW` finds the parent.
    let parent =
        Sd::read(Handle::open_with(parent_path, READ_CONTROL, FILE_FLAG_BACKUP_SEMANTICS)?.0)?;
    let is_dir = path.is_dir();
    let access = READ_CONTROL
        | WRITE_OWNER
        | if is_dir {
            FILE_LIST_DIRECTORY | SYNCHRONIZE
        } else {
            0
        };
    let root = Handle::open_with(path, access, FILE_FLAG_BACKUP_SEMANTICS)?;
    if root.kind()?.is_dir() != is_dir {
        bail!("{} changed while being labelled", path.display());
    }
    let current = Sd::read(root.0)?;
    let current_state = label_state(current.ptr())?;
    // `path` as `SetNamedSecurityInfoW` presents it: its owner and group,
    // the given entries and its own protection.
    let mut creator: SECURITY_DESCRIPTOR = std::mem::zeroed();
    let pcreator = &mut creator as *mut SECURITY_DESCRIPTOR as *mut c_void;
    let (mut owner, mut group): (*mut c_void, *mut c_void) =
        (std::ptr::null_mut(), std::ptr::null_mut());
    let mut defaulted = 0;
    if InitializeSecurityDescriptor(pcreator, SECURITY_DESCRIPTOR_REVISION) == 0
        || GetSecurityDescriptorOwner(current.ptr(), &mut owner, &mut defaulted) == 0
        || GetSecurityDescriptorGroup(current.ptr(), &mut group, &mut defaulted) == 0
        || SetSecurityDescriptorOwner(pcreator, owner, 0) == 0
        || SetSecurityDescriptorGroup(pcreator, group, 0) == 0
        || SetSecurityDescriptorSacl(pcreator, 1, sacl, 0) == 0
        || SetSecurityDescriptorControl(
            pcreator,
            SE_SACL_PROTECTED | SE_SACL_AUTO_INHERITED,
            current_state.control & (SE_SACL_PROTECTED | SE_SACL_AUTO_INHERITED),
        ) == 0
    {
        bail!(
            "building {}'s descriptor failed: {}",
            path.display(),
            GetLastError()
        );
    }
    let computed = compute(parent.ptr(), pcreator, is_dir)?;
    let computed_state = label_state(computed.ptr())?;
    if computed_state != current_state {
        write_label(root.0, &computed, &computed_state)?;
    }
    if !is_dir {
        return Ok(());
    }
    let mutant = mutant();
    par_walk(
        vec![Folder {
            at: Place::Open(root),
            sd: computed,
            path: path.to_path_buf(),
        }],
        label_threads(),
        |folder: Folder, found: &mut Vec<Folder>| {
            set_mutant(mutant);
            label_folder(folder, found)
        },
    )
}

/// A folder whose entries still need labelling, with its descriptor as now
/// written.
struct Folder {
    at: Place,
    sd: Sd,
    /// For messages (and the links-followed mutant) only.
    path: PathBuf,
}

enum Place {
    /// The folder the walk starts at, held open.
    Open(Handle),
    /// A subfolder, opened again through its folder when walked.
    Below(Subfolder),
}

/// Labels the entries of `folder` from its descriptor, adding its
/// subfolders (not links) to `found`.
unsafe fn label_folder(folder: Folder, found: &mut Vec<Folder>) -> Result<()> {
    let dir = Arc::new(match folder.at {
        Place::Open(handle) => handle,
        Place::Below(subfolder) => subfolder.reopen(READ_CONTROL)?,
    });
    let mut computed_for: HashMap<Vec<u8>, (Sd, LabelState)> = HashMap::new();
    for (name, listed) in list(dir.0)? {
        let path = folder.path.join(OsString::from_wide(&name));
        let is_dir = listed.is_dir();
        let follow = is_mutant(Mutant::LinksFollowed) && listed.kind() == Kind::Link && is_dir;
        let walk_into = follow || listed.kind() == Kind::Folder;
        let access = READ_CONTROL | WRITE_OWNER;
        let child = if follow {
            Handle::open_with(&path, access | SYNCHRONIZE, FILE_FLAG_BACKUP_SEMANTICS)?
        } else {
            // The entry itself, links not followed.
            open_relative(dir.0, &name, access)?
        };
        if !follow && !child.kind()?.same_kind(listed) {
            bail!("{} changed while being labelled", path.display());
        }
        let sd = Sd::read(child.0)?;
        let state = label_state(sd.ptr())?;
        let key = computation_key(&sd, &state, is_dir)?;
        if !computed_for.contains_key(&key) {
            let computed = compute(folder.sd.ptr(), sd.ptr(), is_dir)?;
            let computed_state = label_state(computed.ptr())?;
            computed_for.insert(key.clone(), (computed, computed_state));
        }
        let (computed, computed_state) = &computed_for[&key];
        if *computed_state != state && !(is_mutant(Mutant::FilesUnwritten) && !is_dir) {
            write_label(child.0, computed, computed_state)?;
        }
        if walk_into && !is_mutant(Mutant::SubfoldersUnwalked) {
            let at = if follow {
                Place::Open(Handle::open_with(
                    &path,
                    READ_CONTROL | FILE_LIST_DIRECTORY | SYNCHRONIZE,
                    FILE_FLAG_BACKUP_SEMANTICS,
                )?)
            } else {
                Place::Below(Subfolder::new(&dir, name, &child)?)
            };
            found.push(Folder {
                at,
                sd: Sd::copy_of(computed.ptr()),
                path,
            });
        }
    }
    Ok(())
}

/// A self-relative descriptor (owner, group, label) in aligned storage.
struct Sd(Vec<u64>);

impl Sd {
    unsafe fn read(handle: HANDLE) -> Result<Self> {
        let info =
            OWNER_SECURITY_INFORMATION | GROUP_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION;
        let mut buffer = vec![0_u64; 64];
        loop {
            let mut needed = 0_u32;
            if GetKernelObjectSecurity(
                handle,
                info,
                buffer.as_mut_ptr() as *mut c_void,
                (buffer.len() * 8) as u32,
                &mut needed,
            ) != 0
            {
                return Ok(Self(buffer));
            }
            let err = GetLastError();
            if err != ERROR_INSUFFICIENT_BUFFER {
                bail!("GetKernelObjectSecurity failed: {err}");
            }
            buffer = vec![0_u64; (needed as usize).div_ceil(8)];
        }
    }

    unsafe fn copy_of(psd: *mut c_void) -> Self {
        let len = GetSecurityDescriptorLength(psd) as usize;
        let mut buffer = vec![0_u64; len.div_ceil(8)];
        std::ptr::copy_nonoverlapping(psd as *const u8, buffer.as_mut_ptr() as *mut u8, len);
        Self(buffer)
    }

    fn ptr(&self) -> *mut c_void {
        self.0.as_ptr() as *mut c_void
    }
}

/// What decides a label: the SACL control bits and the label entries.
#[derive(PartialEq, Eq, Clone, Debug)]
struct LabelState {
    control: u16,
    aces: Vec<Vec<u8>>,
}

unsafe fn sacl_of(psd: *mut c_void) -> Result<*mut ACL> {
    let mut present = 0;
    let mut defaulted = 0;
    let mut sacl: *mut ACL = std::ptr::null_mut();
    if GetSecurityDescriptorSacl(psd, &mut present, &mut sacl, &mut defaulted) == 0 {
        bail!("GetSecurityDescriptorSacl failed: {}", GetLastError());
    }
    Ok(if present == 0 {
        std::ptr::null_mut()
    } else {
        sacl
    })
}

unsafe fn control_of(psd: *mut c_void) -> Result<u16> {
    let mut control = 0_u16;
    let mut revision = 0_u32;
    if GetSecurityDescriptorControl(psd, &mut control, &mut revision) == 0 {
        bail!("GetSecurityDescriptorControl failed: {}", GetLastError());
    }
    Ok(control)
}

unsafe fn label_state(psd: *mut c_void) -> Result<LabelState> {
    let sacl = sacl_of(psd)?;
    let mut aces = Vec::new();
    if !sacl.is_null() {
        let mut info: ACL_SIZE_INFORMATION = std::mem::zeroed();
        if GetAclInformation(
            sacl,
            &mut info as *mut _ as *mut c_void,
            std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
            AclSizeInformation,
        ) == 0
        {
            bail!("GetAclInformation failed: {}", GetLastError());
        }
        for i in 0..info.AceCount {
            let mut ace: *mut c_void = std::ptr::null_mut();
            if GetAce(sacl, i, &mut ace) == 0 {
                bail!("GetAce failed: {}", GetLastError());
            }
            let hdr = &*(ace as *const ACE_HEADER);
            if u32::from(hdr.AceType) == SYSTEM_MANDATORY_LABEL_ACE_TYPE {
                aces.push(
                    std::slice::from_raw_parts(ace as *const u8, usize::from(hdr.AceSize)).to_vec(),
                );
            }
        }
    }
    Ok(LabelState {
        control: control_of(psd)? & SACL_BITS,
        aces,
    })
}

/// The inputs besides the parent's descriptor that decide the computed
/// label: kind, SACL bits, owner, group and explicit label entries (all of
/// them if the SACL is protected).
unsafe fn computation_key(child: &Sd, state: &LabelState, is_dir: bool) -> Result<Vec<u8>> {
    let mut key = vec![u8::from(is_dir)];
    key.extend_from_slice(&state.control.to_le_bytes());
    for owner in [true, false] {
        let mut sid: *mut c_void = std::ptr::null_mut();
        let mut defaulted = 0;
        let ok = if owner {
            GetSecurityDescriptorOwner(child.ptr(), &mut sid, &mut defaulted)
        } else {
            GetSecurityDescriptorGroup(child.ptr(), &mut sid, &mut defaulted)
        };
        if ok == 0 {
            bail!("reading the owner or group failed: {}", GetLastError());
        }
        if sid.is_null() {
            key.extend_from_slice(&0_u32.to_le_bytes());
        } else {
            let len = GetLengthSid(sid) as usize;
            key.extend_from_slice(&(len as u32).to_le_bytes());
            key.extend_from_slice(std::slice::from_raw_parts(sid as *const u8, len));
        }
    }
    let protected = state.control & SE_SACL_PROTECTED != 0;
    for ace in &state.aces {
        if protected || u32::from(ace[1]) & INHERITED_ACE == 0 {
            key.extend_from_slice(&(ace.len() as u32).to_le_bytes());
            key.extend_from_slice(ace);
        }
    }
    Ok(key)
}

/// The descriptor Windows computes for `current` below `parent`
/// (auto-inherit), as a copy.
unsafe fn compute(parent: *mut c_void, current: *mut c_void, is_dir: bool) -> Result<Sd> {
    let mut sd: *mut c_void = std::ptr::null_mut();
    if CreatePrivateObjectSecurityEx(
        parent,
        current,
        &mut sd,
        std::ptr::null(),
        i32::from(is_dir),
        SEF_SACL_AUTO_INHERIT | SEF_AVOID_OWNER_CHECK | SEF_AVOID_PRIVILEGE_CHECK,
        0,
        &FILE_MAPPING,
    ) == 0
    {
        bail!("CreatePrivateObjectSecurityEx failed: {}", GetLastError());
    }
    let copy = Sd::copy_of(sd);
    DestroyPrivateObjectSecurity(&sd);
    Ok(copy)
}

/// Writes the label SACL of `computed` (and its protection and
/// auto-inherited bits, `state`'s) to `handle`.
unsafe fn write_label(handle: HANDLE, computed: &Sd, state: &LabelState) -> Result<()> {
    let mut sd: SECURITY_DESCRIPTOR = std::mem::zeroed();
    let psd = &mut sd as *mut SECURITY_DESCRIPTOR as *mut c_void;
    let sacl = sacl_of(computed.ptr())?;
    if InitializeSecurityDescriptor(psd, SECURITY_DESCRIPTOR_REVISION) == 0
        || SetSecurityDescriptorSacl(psd, i32::from(!sacl.is_null()), sacl, 0) == 0
    {
        bail!("building the descriptor failed: {}", GetLastError());
    }
    let mut bits = state.control & (SE_SACL_PROTECTED | SE_SACL_AUTO_INHERITED);
    if is_mutant(Mutant::AutoInheritDropped) {
        bits &= !SE_SACL_AUTO_INHERITED;
    }
    // Asks the file system to keep the auto-inherited flag.
    if bits & SE_SACL_AUTO_INHERITED != 0 {
        bits |= SE_SACL_AUTO_INHERIT_REQ;
    }
    if SetSecurityDescriptorControl(
        psd,
        SE_SACL_PROTECTED | SE_SACL_AUTO_INHERITED | SE_SACL_AUTO_INHERIT_REQ,
        bits,
    ) == 0
    {
        bail!("SetSecurityDescriptorControl failed: {}", GetLastError());
    }
    if SetKernelObjectSecurity(handle, LABEL_SECURITY_INFORMATION, psd) == 0 {
        bail!("SetKernelObjectSecurity(label) failed: {}", GetLastError());
    }
    Ok(())
}

/// Deliberately broken versions of the walk, which the regression tests must
/// catch (D311), and a switch to Windows' walk for the tests' reference
/// side. Set per calling thread, so tests running in parallel are
/// unaffected; never set outside tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum Mutant {
    /// Files below the root are not written.
    FilesUnwritten,
    /// Subfolders are not walked.
    SubfoldersUnwalked,
    /// Written objects lose the auto-inherited flag.
    AutoInheritDropped,
    /// Links to folders are followed (and labelled through).
    LinksFollowed,
    /// Not broken: Windows' walk only (the reference side).
    WindowsWalk,
}

#[cfg(test)]
thread_local! {
    pub(crate) static MUTANT: std::cell::Cell<Option<Mutant>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn mutant() -> Option<Mutant> {
    MUTANT.with(std::cell::Cell::get)
}

#[cfg(not(test))]
fn mutant() -> Option<Mutant> {
    None
}

#[cfg(test)]
fn set_mutant(mutant: Option<Mutant>) {
    MUTANT.with(|cell| cell.set(mutant));
}

#[cfg(not(test))]
fn set_mutant(_mutant: Option<Mutant>) {}

pub(super) fn is_mutant(which: Mutant) -> bool {
    mutant() == Some(which)
}
