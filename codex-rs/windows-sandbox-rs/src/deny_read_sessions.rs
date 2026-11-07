//! #323: a machine-wide registry of the live sessions that rely on the
//! elevated sandbox's deny-read entries.
//!
//! The entries are for the sandbox's users group, which is machine-wide, so
//! sessions on different `CODEX_HOME`s (or with different rules on one) rely
//! on the same entry: one adds it, and the others find it there. A sync
//! removes only entries its own `CODEX_HOME` added, once none of their rules
//! is configured (#304), and nothing while a contract on that home is armed
//! (#301). That still let one session remove an entry another live session's
//! commands relied on.
//!
//! So each Core process that runs elevated-sandbox commands with deny-read
//! rules, or arms the secretless launch contract, registers itself here for
//! its lifetime: a session file it holds locked. A sync removes entries only
//! while no other session is registered, and holds [`SYNC_LOCK_FILE`] from
//! that check until its state is stored; registration takes the same lock,
//! so a session registering meanwhile waits, and its launch then adds back
//! whatever was removed. Anything that keeps the check from succeeding (no
//! registry, one this module doesn't trust, a lock that can't be had, a
//! session file that can't be probed) keeps the entries: they stay recorded
//! and a later sync removes them.
//!
//! The registry is `%ProgramData%\CorbanuTerminalSandbox\deny-read-sessions`,
//! created by the elevated setup ([`ensure_registry`]): both folders owned by
//! Administrators, with protected DACLs. Users may list the registry, add
//! files to it and read each other's files; only a file's creator (and
//! Administrators and SYSTEM) may change or delete it; the sandbox's users
//! may do nothing. A folder that is a link or is owned by anyone else is not
//! trusted, since its owner could empty it or point it elsewhere. A session
//! file is open without delete sharing while its session lives, so no one
//! can delete or rename it; a process that holds a stale one, or
//! [`SYNC_LOCK_FILE`], only keeps entries from being removed.
//!
//! One process can launch with more than one rule set (different threads'
//! configuration): once it has, its own session no longer counts as its own
//! in its syncs, so it removes nothing until it exits.

use crate::winutil::string_from_sid_bytes;
use crate::winutil::to_wide;
use anyhow::Context;
use anyhow::Result;
use rand::Rng;
use rand::SeedableRng;
use rand::rngs::SmallRng;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeSet;
use std::ffi::c_void;
use std::fs::File;
use std::os::windows::fs::MetadataExt as _;
use std::os::windows::fs::OpenOptionsExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;
use windows_sys::Win32::Foundation::HLOCAL;
use windows_sys::Win32::Foundation::LocalFree;

/// Under `%ProgramData%`.
const REGISTRY_PARENT: &str = "CorbanuTerminalSandbox";
const REGISTRY_DIR: &str = "deny-read-sessions";
/// Held exclusively while a session registers, and while a sync checks for
/// other sessions and removes entries.
pub(crate) const SYNC_LOCK_FILE: &str = "sync.lock";
const SESSION_PREFIX: &str = "session-";
const SESSION_SUFFIX: &str = ".lock";
/// Each holder keeps [`SYNC_LOCK_FILE`] for milliseconds.
pub const DENY_READ_SYNC_LOCK_WAIT: Duration = Duration::from_secs(2);
const ADMINISTRATORS: &str = "S-1-5-32-544";
const SYSTEM: &str = "S-1-5-18";
const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;
const FILE_SHARE_DELETE: u32 = 0x4;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const FILE_READ_ATTRIBUTES: u32 = 0x80;
const READ_CONTROL: u32 = 0x0002_0000;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const DELETE: u32 = 0x0001_0000;

/// Where a sync looks for other sessions, and which one is the caller's.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DenyReadSessions {
    /// `None`: no registry to check, so the sync removes nothing.
    #[serde(default)]
    pub registry: Option<PathBuf>,
    /// The caller's own session file, which the check skips.
    #[serde(default)]
    pub own: Option<String>,
    /// Tests only: also trust a registry this user owns. Never sent to the
    /// setup helper.
    #[doc(hidden)]
    #[serde(skip)]
    pub trust_this_user: bool,
}

impl DenyReadSessions {
    /// This process's view: the machine-wide registry, and this process's
    /// session in it. `rules` is the launch's deny-read rule set, serialized
    /// (empty for none), if its sync runs: the rule set is counted, and a
    /// non-empty one registers the process first.
    /// A failed registration is logged to `log_dir`.
    pub fn for_this_process(rules: Option<String>, log_dir: Option<&Path>) -> Self {
        let registry = registry_dir();
        let (sessions, error) = THIS_PROCESS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .view(registry, rules, /*trust_this_user*/ false);
        if let Some(err) = error {
            crate::logging::log_note(
                &format!(
                    "deny-read sessions: this session is not registered, so another Corbanu session may remove a read protection its commands rely on: {err:#}"
                ),
                log_dir,
            );
        }
        sessions
    }

    /// Tests only: a registry in a folder this user created.
    #[doc(hidden)]
    pub fn in_test_registry(registry: &Path, own: Option<&Registration>) -> Self {
        Self {
            registry: Some(registry.to_path_buf()),
            own: own.map(|registration| registration.name.clone()),
            trust_this_user: true,
        }
    }
}

/// `%ProgramData%\CorbanuTerminalSandbox\deny-read-sessions`.
pub fn registry_dir() -> Option<PathBuf> {
    program_data().map(|dir| dir.join(REGISTRY_PARENT).join(REGISTRY_DIR))
}

fn program_data() -> Option<PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::FOLDERID_ProgramData;
    use windows_sys::Win32::UI::Shell::SHGetKnownFolderPath;
    let mut path: *mut u16 = std::ptr::null_mut();
    // SAFETY: `path` receives a string the call allocates, freed below.
    let status = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, 0, 0, &mut path) };
    let result = (status == 0 && !path.is_null()).then(|| {
        // SAFETY: a NUL-terminated string from the call.
        let len = (0..).take_while(|&i| unsafe { *path.add(i) } != 0).count();
        // SAFETY: as above.
        PathBuf::from(String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(path, len)
        }))
    });
    // SAFETY: allocated by SHGetKnownFolderPath (null is allowed).
    unsafe { CoTaskMemFree(path.cast()) };
    result
}

/// Elevated setup: creates the registry, or repairs it, and its
/// [`SYNC_LOCK_FILE`]. A folder that is a link or that someone else owns is
/// moved aside first. `sandbox_group` (a SID string) is denied everything.
pub fn ensure_registry(sandbox_group: Option<&str>) -> Result<PathBuf> {
    let registry = registry_dir().context("no ProgramData folder")?;
    let parent = registry.parent().context("registry has no parent")?;
    let deny = sandbox_group.map_or_else(String::new, |group| format!("(D;OICI;FA;;;{group})"));
    let admins = "(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";
    let label = "S:(ML;OICI;NW;;;ME)";
    // Users: list and traverse.
    ensure_dir(
        parent,
        &format!("O:BAD:P{deny}{admins}(A;;0x1200a9;;;BU){label}"),
    )?;
    // Users: also add files; a file's creator gets full control, other users
    // read it. The medium label lets a normal session's Core write files an
    // elevated Core created.
    ensure_dir(
        &registry,
        &format!(
            "O:BAD:P{deny}{admins}(A;;0x1200ab;;;BU)(A;OIIO;FA;;;CO)(A;OIIO;0x120089;;;BU){label}"
        ),
    )?;
    let lock = registry.join(SYNC_LOCK_FILE);
    if let Ok(existing) =
        open_no_follow(&lock, READ_CONTROL | FILE_READ_ATTRIBUTES, /*flags*/ 0)
        && !(is_plain_file(&existing)?
            && owner_is_trusted(&existing, /*trust_this_user*/ false)?)
    {
        drop(existing);
        std::fs::remove_file(&lock).with_context(|| format!("remove {}", lock.display()))?;
    }
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&lock)
        .with_context(|| format!("create {}", lock.display()))?;
    check_registry(&registry, /*trust_this_user*/ false)?;
    Ok(registry)
}

/// Makes `dir` a folder with the security `sddl` describes.
fn ensure_dir(dir: &Path, sddl: &str) -> Result<()> {
    let descriptor = Descriptor::parse(sddl)?;
    match open_no_follow(
        dir,
        READ_CONTROL | FILE_READ_ATTRIBUTES,
        FILE_FLAG_BACKUP_SEMANTICS,
    ) {
        Ok(existing) => {
            let trusted = existing.metadata()?.is_dir()
                && !is_link(&existing)?
                && owner_is_trusted(&existing, /*trust_this_user*/ false)?;
            drop(existing);
            if trusted {
                return descriptor.apply_to(dir);
            }
            let mut rng = SmallRng::from_entropy();
            let mut aside = dir.as_os_str().to_owned();
            aside.push(format!(".untrusted-{:016x}", rng.r#gen::<u64>()));
            std::fs::rename(dir, &aside)
                .with_context(|| format!("move the untrusted {} aside", dir.display()))?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err).with_context(|| format!("open {}", dir.display())),
    }
    descriptor.create_dir(dir)
}

/// A parsed security descriptor.
struct Descriptor(*mut c_void);

impl Descriptor {
    fn parse(sddl: &str) -> Result<Self> {
        use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
        let sddl = to_wide(sddl);
        let mut descriptor: *mut c_void = std::ptr::null_mut();
        // SAFETY: parses `sddl` into a descriptor freed on drop.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error()).context("parse a security descriptor");
        }
        Ok(Self(descriptor))
    }

    fn create_dir(&self, dir: &Path) -> Result<()> {
        use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
        use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        };
        let path = to_wide(dir);
        // SAFETY: valid path and attributes.
        if unsafe { CreateDirectoryW(path.as_ptr(), &attributes) } == 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("create {}", dir.display()));
        }
        Ok(())
    }

    /// Sets this descriptor's owner, DACL (protected) and label on `dir`.
    fn apply_to(&self, dir: &Path) -> Result<()> {
        use windows_sys::Win32::Foundation::ERROR_SUCCESS;
        use windows_sys::Win32::Security::ACL;
        use windows_sys::Win32::Security::Authorization::SE_FILE_OBJECT;
        use windows_sys::Win32::Security::Authorization::SetNamedSecurityInfoW;
        use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
        use windows_sys::Win32::Security::GetSecurityDescriptorDacl;
        use windows_sys::Win32::Security::GetSecurityDescriptorOwner;
        use windows_sys::Win32::Security::GetSecurityDescriptorSacl;
        use windows_sys::Win32::Security::LABEL_SECURITY_INFORMATION;
        use windows_sys::Win32::Security::OWNER_SECURITY_INFORMATION;
        use windows_sys::Win32::Security::PROTECTED_DACL_SECURITY_INFORMATION;
        let mut owner: *mut c_void = std::ptr::null_mut();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sacl: *mut ACL = std::ptr::null_mut();
        let (mut present, mut defaulted) = (0, 0);
        // SAFETY: the pointers point into `self.0`, which outlives the call.
        let status = unsafe {
            if GetSecurityDescriptorOwner(self.0, &mut owner, &mut defaulted) == 0
                || GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted) == 0
                || GetSecurityDescriptorSacl(self.0, &mut present, &mut sacl, &mut defaulted) == 0
            {
                u32::MAX
            } else {
                let path = to_wide(dir);
                SetNamedSecurityInfoW(
                    path.as_ptr(),
                    SE_FILE_OBJECT,
                    OWNER_SECURITY_INFORMATION
                        | DACL_SECURITY_INFORMATION
                        | PROTECTED_DACL_SECURITY_INFORMATION
                        | LABEL_SECURITY_INFORMATION,
                    owner,
                    std::ptr::null_mut(),
                    dacl,
                    sacl,
                )
            }
        };
        if status != ERROR_SUCCESS {
            anyhow::bail!("set the security of {}: {status}", dir.display());
        }
        Ok(())
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe { LocalFree(self.0 as HLOCAL) };
    }
}

/// Opens `path` itself (never what a link points to) for `access`, sharing
/// everything.
fn open_no_follow(path: &Path, access: u32, flags: u32) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .access_mode(access)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | flags)
        .open(path)
}

fn is_link(file: &File) -> Result<bool> {
    Ok(file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

/// A regular file with one name.
fn is_plain_file(file: &File) -> Result<bool> {
    Ok(file.metadata()?.is_file() && !is_link(file)? && crate::acl::file_link_count(file)? == 1)
}

/// Whether `file` (open with `READ_CONTROL`) is owned by Administrators or
/// SYSTEM (or, with `trust_this_user`, by this user).
fn owner_is_trusted(file: &File, trust_this_user: bool) -> Result<bool> {
    let owner = owner_sid(file)?;
    Ok(owner == ADMINISTRATORS
        || owner == SYSTEM
        || (trust_this_user && current_user_sid().is_ok_and(|user| user == owner)))
}

/// The owner of `file` (open with `READ_CONTROL`), as a SID string.
fn owner_sid(file: &File) -> Result<String> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::Security::Authorization::GetSecurityInfo;
    use windows_sys::Win32::Security::Authorization::SE_FILE_OBJECT;
    use windows_sys::Win32::Security::GetLengthSid;
    use windows_sys::Win32::Security::OWNER_SECURITY_INFORMATION;
    let mut owner: *mut c_void = std::ptr::null_mut();
    let mut descriptor: *mut c_void = std::ptr::null_mut();
    // SAFETY: the handle is open with READ_CONTROL; `descriptor` is freed below.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle() as _,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        anyhow::bail!("read the owner: {status}");
    }
    let owner = (!owner.is_null()).then(|| {
        // SAFETY: `owner` points into `descriptor`, a valid SID.
        let sid =
            unsafe { std::slice::from_raw_parts(owner as *const u8, GetLengthSid(owner) as usize) };
        string_from_sid_bytes(sid)
    });
    // SAFETY: allocated by GetSecurityInfo; `owner` is not used after this.
    unsafe { LocalFree(descriptor as HLOCAL) };
    owner.context("no owner")?.map_err(anyhow::Error::msg)
}

fn current_user_sid() -> Result<String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    // SAFETY: the token is closed right after reading its user.
    let sid = unsafe {
        let token = crate::token::get_current_token_for_query()?;
        let sid = crate::token::get_user_sid_bytes(token);
        CloseHandle(token);
        sid?
    };
    string_from_sid_bytes(&sid).map_err(anyhow::Error::msg)
}

/// Fails unless `registry` and its parent are folders, not links, owned by
/// Administrators or SYSTEM (or, with `trust_this_user`, by this user).
fn check_registry(registry: &Path, trust_this_user: bool) -> Result<()> {
    let parent = registry.parent().context("registry has no parent")?;
    for dir in [parent, registry] {
        let file = open_no_follow(
            dir,
            READ_CONTROL | FILE_READ_ATTRIBUTES,
            FILE_FLAG_BACKUP_SEMANTICS,
        )
        .with_context(|| format!("open {}", dir.display()))?;
        if !file.metadata()?.is_dir() || is_link(&file)? {
            anyhow::bail!("{} is not a folder", dir.display());
        }
        if !owner_is_trusted(&file, trust_this_user)? {
            anyhow::bail!(
                "{} is not owned by Administrators; the elevated sandbox setup repairs it",
                dir.display()
            );
        }
    }
    Ok(())
}

static THIS_PROCESS: Mutex<ProcessSessions> = Mutex::new(ProcessSessions::new());

/// This process's registration, and the rule sets it has launched with.
#[derive(Debug)]
struct ProcessSessions {
    registration: Option<Registration>,
    rule_sets: BTreeSet<String>,
    /// When a launch last failed to register: launches don't retry for
    /// [`REGISTER_RETRY_AFTER`], so a held [`SYNC_LOCK_FILE`] doesn't slow
    /// each one down.
    failed_at: Option<Instant>,
}

const REGISTER_RETRY_AFTER: Duration = Duration::from_secs(10);

impl ProcessSessions {
    const fn new() -> Self {
        Self {
            registration: None,
            rule_sets: BTreeSet::new(),
            failed_at: None,
        }
    }

    /// Registers once in `registry` (retrying after a failure).
    fn register(&mut self, registry: &Path, trust_this_user: bool, wait: Duration) -> Result<&str> {
        if self.registration.is_none() {
            self.registration = Some(Registration::register(registry, trust_this_user, wait)?);
        }
        Ok(self
            .registration
            .as_ref()
            .map_or("", |registered| &registered.name))
    }

    /// See [`DenyReadSessions::for_this_process`]; also returns why the
    /// process could not register.
    fn view(
        &mut self,
        registry: Option<PathBuf>,
        rules: Option<String>,
        trust_this_user: bool,
    ) -> (DenyReadSessions, Option<anyhow::Error>) {
        let mut error = None;
        // A launch without rules counts as a rule set too: its sync would
        // remove every entry this home added.
        if let Some(rules) = rules {
            let registers = !rules.is_empty();
            self.rule_sets.insert(rules);
            let cooling = self
                .failed_at
                .is_some_and(|failed| failed.elapsed() < REGISTER_RETRY_AFTER);
            if registers && self.registration.is_none() && cooling {
                error = Some(anyhow::anyhow!("not retried yet after a failure"));
            } else if registers {
                let registered = match registry.as_deref() {
                    Some(registry) => self
                        .register(registry, trust_this_user, DENY_READ_SYNC_LOCK_WAIT)
                        .map(drop),
                    None => Err(anyhow::anyhow!("no ProgramData folder")),
                };
                self.failed_at = registered.is_err().then(Instant::now);
                error = registered.err();
            }
        }
        // With more than one rule set, this process may rely on an entry
        // its own sync would remove.
        let own = (self.rule_sets.len() <= 1)
            .then(|| {
                self.registration
                    .as_ref()
                    .map(|registered| registered.name.clone())
            })
            .flatten();
        let sessions = DenyReadSessions {
            registry,
            own,
            trust_this_user,
        };
        (sessions, error)
    }
}

/// Registers this process once (retrying after a failure, waiting up to
/// `wait` for [`SYNC_LOCK_FILE`]) and keeps the registration until it exits.
/// Returns its session file's name.
pub fn register_this_process(wait: Duration) -> Result<String> {
    let registry = registry_dir().context("no ProgramData folder")?;
    THIS_PROCESS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .register(&registry, /*trust_this_user*/ false, wait)
        .map(str::to_string)
}

/// A session's file, locked until dropped or the process exits (then
/// deleted).
#[derive(Debug)]
pub struct Registration {
    file: Option<File>,
    path: PathBuf,
    name: String,
}

impl Registration {
    /// Creates and locks a new session file in `registry` (which must be
    /// trusted), under [`SYNC_LOCK_FILE`] so no sync is between its check and
    /// its removals.
    pub fn register(registry: &Path, trust_this_user: bool, wait: Duration) -> Result<Self> {
        check_registry(registry, trust_this_user)?;
        let _sync = SyncLock::acquire(registry, wait)?
            .context("another process holds the deny-read sync lock")?;
        let mut rng = SmallRng::from_entropy();
        let name = format!(
            "{SESSION_PREFIX}{}-{:032x}{SESSION_SUFFIX}",
            std::process::id(),
            rng.r#gen::<u128>()
        );
        let path = registry.join(&name);
        // Shared for reading only: no one else may write, delete or rename
        // it while it is held. Gone with the last handle, however this
        // process exits.
        let file = std::fs::OpenOptions::new()
            // `write` for `create_new`; `access_mode` adds `DELETE`.
            .write(true)
            .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
            .create_new(true)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_DELETE_ON_CLOSE)
            .open(&path)
            .with_context(|| format!("create {}", path.display()))?;
        file.try_lock()
            .map_err(|err| anyhow::anyhow!("lock {}: {err}", path.display()))?;
        Ok(Self {
            file: Some(file),
            path,
            name,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = std::fs::remove_file(&self.path);
    }
}

/// [`SYNC_LOCK_FILE`], held exclusively.
struct SyncLock {
    _file: File,
}

impl SyncLock {
    /// `None` if it is still held by another process after `wait`.
    fn acquire(registry: &Path, wait: Duration) -> Result<Option<Self>> {
        let path = registry.join(SYNC_LOCK_FILE);
        let open = |write: bool| {
            std::fs::OpenOptions::new()
                .read(true)
                .write(write)
                .create(write)
                .truncate(false)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(&path)
        };
        // Another user's (read-only here) does as well: locking needs reads.
        let file = open(true)
            .or_else(|_| open(false))
            .with_context(|| format!("open {}", path.display()))?;
        if !is_plain_file(&file)? {
            anyhow::bail!("{} is not a regular file with one name", path.display());
        }
        let deadline = Instant::now() + wait;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Some(Self { _file: file })),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
                Err(std::fs::TryLockError::Error(err)) => {
                    return Err(err).with_context(|| format!("lock {}", path.display()));
                }
            }
        }
    }
}

/// Held by a sync from its check for other sessions until its state is
/// stored: no other session is registered, and none can register meanwhile.
pub(crate) struct NoOtherSessions {
    _sync: SyncLock,
}

/// `Some` when no session other than `sessions.own` is registered; `None`
/// when one is, or it can't be told (then nothing may be removed).
pub(crate) fn lock_out_other_sessions(sessions: &DenyReadSessions) -> Option<NoOtherSessions> {
    let registry = sessions.registry.as_deref()?;
    check_registry(registry, sessions.trust_this_user).ok()?;
    let sync = SyncLock::acquire(registry, DENY_READ_SYNC_LOCK_WAIT).ok()??;
    for entry in std::fs::read_dir(registry).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let name = name.to_str()?;
        if !(name.starts_with(SESSION_PREFIX) && name.ends_with(SESSION_SUFFIX)) {
            continue;
        }
        if sessions.own.as_deref() == Some(name) {
            continue;
        }
        if session_is_live(&entry.path())? {
            return None;
        }
    }
    Some(NoOtherSessions { _sync: sync })
}

/// Whether a session still holds `path`; `None` if that can't be told.
/// A dead session's file is deleted when this user may.
fn session_is_live(path: &Path) -> Option<bool> {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    {
        Ok(file) => file,
        // Deleted since it was listed: that session is gone.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Some(false),
        // Being deleted (its holder just closed it): gone.
        Err(err) if err.raw_os_error() == Some(5) && !path.exists() => return Some(false),
        Err(_) => return None,
    };
    match file.try_lock() {
        Ok(()) => {
            drop(file);
            let _ = std::fs::remove_file(path);
            Some(false)
        }
        Err(std::fs::TryLockError::WouldBlock) => Some(true),
        Err(std::fs::TryLockError::Error(_)) => None,
    }
}

#[cfg(test)]
#[path = "deny_read_sessions_tests.rs"]
mod tests;
