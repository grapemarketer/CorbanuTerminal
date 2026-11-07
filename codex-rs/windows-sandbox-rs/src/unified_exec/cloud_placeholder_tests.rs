//! #1668: cloud-file placeholder folders (OneDrive Files On-Demand and other
//! Cloud Files sync providers) at or above an explicit read-only path get
//! the same protection as plain folders.

use super::carve_out_link::run_script;
use super::*;
use crate::winutil::to_wide;
use std::ffi::c_void;
use windows_sys::Win32::System::LibraryLoader::GetProcAddress;
use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;

/// `CF_SYNC_REGISTRATION`.
#[repr(C)]
struct SyncRegistration {
    struct_size: u32,
    provider_name: *const u16,
    provider_version: *const u16,
    sync_root_identity: *const c_void,
    sync_root_identity_length: u32,
    file_identity: *const c_void,
    file_identity_length: u32,
    provider_id: [u32; 4],
}

/// `CF_SYNC_POLICIES`: hydration and population are (primary, modifier).
#[repr(C)]
struct SyncPolicies {
    struct_size: u32,
    hydration: [u16; 2],
    population: [u16; 2],
    in_sync: u32,
    hard_link: u32,
    placeholder_management: u32,
}

/// `CF_PLACEHOLDER_CREATE_INFO`, with `CF_FS_METADATA` inlined.
#[repr(C)]
struct PlaceholderCreateInfo {
    relative_file_name: *const u16,
    creation_time: i64,
    last_access_time: i64,
    last_write_time: i64,
    change_time: i64,
    file_attributes: u32,
    file_size: i64,
    file_identity: *const c_void,
    file_identity_length: u32,
    flags: u32,
    result: i32,
    create_usn: i64,
}

type RegisterFn =
    unsafe extern "system" fn(*const u16, *const SyncRegistration, *const SyncPolicies, u32) -> i32;
type UnregisterFn = unsafe extern "system" fn(*const u16) -> i32;
type CreatePlaceholdersFn =
    unsafe extern "system" fn(*const u16, *mut PlaceholderCreateInfo, u32, u32, *mut u32) -> i32;

/// `name` from cldapi.dll (loaded at run time, so nothing links against it).
fn cloud_api(name: &str) -> unsafe extern "system" fn() -> isize {
    let library = to_wide("cldapi.dll");
    let name = std::ffi::CString::new(name).expect("name");
    // SAFETY: plain library and symbol lookups with NUL-terminated names.
    unsafe {
        let module = LoadLibraryW(library.as_ptr());
        assert!(module != 0, "load cldapi.dll");
        GetProcAddress(module, name.as_ptr().cast()).expect("cldapi symbol")
    }
}

/// A Cloud Files sync root with no provider connected, as OneDrive's folder is
/// while OneDrive isn't running; unregistered when dropped.
struct SyncRoot(Vec<u16>);

impl SyncRoot {
    fn register(path: &Path) -> Self {
        let wide = to_wide(path);
        let name = to_wide("CorbanuSandboxTest");
        let version = to_wide("1");
        let registration = SyncRegistration {
            struct_size: std::mem::size_of::<SyncRegistration>() as u32,
            provider_name: name.as_ptr(),
            provider_version: version.as_ptr(),
            sync_root_identity: std::ptr::null(),
            sync_root_identity_length: 0,
            file_identity: std::ptr::null(),
            file_identity_length: 0,
            provider_id: [std::process::id(), 0x1668, 0, 0],
        };
        // Full hydration; folders are always fully populated, so listing
        // them never asks the (absent) provider.
        let policies = SyncPolicies {
            struct_size: std::mem::size_of::<SyncPolicies>() as u32,
            hydration: [2, 0],
            population: [3, 0],
            in_sync: 0,
            hard_link: 0,
            placeholder_management: 0,
        };
        // SAFETY: the signature is `CfRegisterSyncRoot`'s; every pointer
        // outlives the call.
        let hr = unsafe {
            let register: RegisterFn = std::mem::transmute(cloud_api("CfRegisterSyncRoot"));
            register(wide.as_ptr(), &registration, &policies, 0)
        };
        assert_eq!(hr, 0, "CfRegisterSyncRoot {}", path.display());
        Self(wide)
    }

    /// Creates folder placeholder `name` in the sync root, marked fully
    /// populated (as OneDrive's are once listed).
    fn placeholder_folder(&self, name: &str) {
        let relative = to_wide(name);
        let identity = [0u8; 4];
        let mut info = PlaceholderCreateInfo {
            relative_file_name: relative.as_ptr(),
            creation_time: 0,
            last_access_time: 0,
            last_write_time: 0,
            change_time: 0,
            file_attributes: 0x10,
            file_size: 0,
            file_identity: identity.as_ptr().cast(),
            file_identity_length: identity.len() as u32,
            // DISABLE_ON_DEMAND_POPULATION | MARK_IN_SYNC
            flags: 0x3,
            result: 0,
            create_usn: 0,
        };
        let mut created = 0u32;
        // SAFETY: the signature is `CfCreatePlaceholders`'s; every pointer
        // outlives the call.
        let hr = unsafe {
            let create: CreatePlaceholdersFn =
                std::mem::transmute(cloud_api("CfCreatePlaceholders"));
            create(self.0.as_ptr(), &mut info, 1, 0, &mut created)
        };
        assert_eq!((hr, info.result, created), (0, 0, 1), "placeholder {name}");
    }
}

impl Drop for SyncRoot {
    fn drop(&mut self) {
        // SAFETY: the signature is `CfUnregisterSyncRoot`'s.
        unsafe {
            let unregister: UnregisterFn = std::mem::transmute(cloud_api("CfUnregisterSyncRoot"));
            unregister(self.0.as_ptr());
        }
    }
}

/// A reparse point that isn't a link (a junction, symbolic link or mount
/// point), as cloud placeholders are.
fn is_placeholder(path: &Path) -> bool {
    crate::workspace_acl::is_reparse_point(path)
        && fs::symlink_metadata(path).is_ok_and(|meta| !meta.file_type().is_symlink())
}

/// A command can't move a placeholder folder above an explicit read-only
/// path (the sync root itself, or one inside it), nor a placeholder that is
/// the read-only path, and recreate the read-only path. Before, D263 skipped
/// every reparse point, so placeholders got none of its denies.
#[test]
fn legacy_cloud_placeholders_at_or_above_a_read_only_path_cannot_be_moved() {
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let workspace = test_root.path().join("workspace");
    let cloud = workspace.join("cloud");
    fs::create_dir_all(&cloud).expect("create sync root");
    let sync_root = SyncRoot::register(&cloud);
    sync_root.placeholder_folder("a");
    sync_root.placeholder_folder("p");
    fs::create_dir_all(cloud.join("a\\b\\c")).expect("create carve-out");
    fs::write(cloud.join("a\\b\\c\\keep.txt"), "original").expect("seed carve-out file");
    fs::write(cloud.join("p\\keep.txt"), "original").expect("seed placeholder file");
    assert_eq!(
        [&cloud, &cloud.join("a"), &cloud.join("p")].map(|path| is_placeholder(path)),
        [true; 3]
    );
    let carve_outs = [cloud.join("a\\b\\c"), cloud.join("p")]
        .map(|path| AbsolutePathBuf::from_absolute_path(path).expect("absolute"));
    let started = run_script(
        &PermissionProfile::workspace_write(),
        &workspace,
        &carve_outs,
        HashMap::from([("C".to_string(), cloud.to_string_lossy().into_owned())]),
        concat!(
            "echo after> \"%C%\\a\\other.txt\"\r\n",
            "ren \"%C%\\a\" a-moved\r\n",
            "ren \"%C%\\p\" p-moved\r\n",
            "ren \"%C%\" cloud-moved\r\n",
            "mkdir \"%C%\\a\\b\\c\"\r\n",
            "echo planted> \"%C%\\a\\b\\c\\keep.txt\"\r\n",
            "mkdir \"%C%\\p\"\r\n",
            "echo planted> \"%C%\\p\\keep.txt\"\r\n",
        ),
    );
    let moved: Vec<&str> = ["cloud\\a-moved", "cloud\\p-moved", "cloud-moved"]
        .into_iter()
        .filter(|name| workspace.join(name).exists())
        .collect();
    let kept =
        ["a\\b\\c\\keep.txt", "p\\keep.txt"].map(|file| fs::read_to_string(cloud.join(file)).ok());
    drop(sync_root);

    assert_eq!(
        (
            started,
            moved,
            kept,
            fs::read_to_string(cloud.join("a\\other.txt")).ok(),
        ),
        (
            Ok(0),
            Vec::<&str>::new(),
            [Some("original".to_string()), Some("original".to_string())],
            Some("after\r\n".to_string()),
        )
    );
}
