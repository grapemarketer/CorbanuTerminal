use super::*;
use pretty_assertions::assert_eq;
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
use windows_sys::Win32::Storage::FileSystem::FILE_LIST_DIRECTORY;
use windows_sys::Win32::Storage::FileSystem::FILE_READ_DATA;
use windows_sys::Win32::Storage::FileSystem::READ_CONTROL;
use windows_sys::Win32::Storage::FileSystem::ReadFile;

fn wide(name: &str) -> Vec<u16> {
    std::ffi::OsStr::new(name).encode_wide().collect()
}

fn names(dir: &Handle) -> Vec<String> {
    // SAFETY: a folder opened for listing.
    let mut out: Vec<String> = unsafe { list(dir.0) }
        .expect("list")
        .into_iter()
        .map(|(name, _)| String::from_utf16_lossy(&name))
        .collect();
    out.sort();
    out
}

fn open_dir(path: &Path) -> Handle {
    // SAFETY: an existing folder.
    unsafe { Handle::open(path, READ_CONTROL | FILE_LIST_DIRECTORY | SYNCHRONIZE) }
        .expect("open folder")
}

fn read_all(handle: &Handle) -> String {
    let mut buffer = [0_u8; 64];
    let mut read = 0_u32;
    // SAFETY: a file opened for reading; the buffer outlives the call.
    let ok = unsafe {
        ReadFile(
            handle.0,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            &mut read,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(ok, 0, "ReadFile");
    String::from_utf8_lossy(&buffer[..read as usize]).into_owned()
}

#[test]
fn par_walk_visits_every_item_once_on_several_threads() {
    let visited = AtomicUsize::new(0);
    let sum = AtomicUsize::new(0);
    // A tree of numbers: each n < 1000 adds 2n+1 and 2n+2.
    par_walk(vec![0_usize], 4, |n, found| {
        visited.fetch_add(1, Ordering::SeqCst);
        sum.fetch_add(n, Ordering::SeqCst);
        for child in [2 * n + 1, 2 * n + 2] {
            if child < 1000 {
                found.push(child);
            }
        }
        Ok(())
    })
    .expect("walk");
    assert_eq!(visited.load(Ordering::SeqCst), 1000);
    assert_eq!(sum.load(Ordering::SeqCst), (0..1000).sum::<usize>());
}

#[test]
fn par_walk_stops_at_an_error_or_a_panic() {
    for threads in [1, 4] {
        let err = par_walk(vec![0_usize], threads, |n, found| {
            if n == 50 {
                bail!("item 50");
            }
            found.extend([2 * n + 1, 2 * n + 2].into_iter().filter(|c| *c < 1000));
            Ok(())
        })
        .expect_err("an error");
        assert_eq!(err.to_string(), "item 50");
        // A panicking item is an error, not a hang.
        let err = par_walk(vec![0_usize], threads, |n, found| {
            assert_ne!(n, 7, "deliberate panic");
            found.extend([2 * n + 1, 2 * n + 2].into_iter().filter(|c| *c < 100));
            Ok(())
        })
        .expect_err("a panic");
        assert_eq!(err.to_string(), "the walk panicked");
    }
}

#[test]
fn list_and_open_relative_see_entries_without_following_links() {
    let base = tempfile::tempdir().expect("base");
    let dir = base.path().join("d");
    std::fs::create_dir_all(dir.join("sub")).expect("sub");
    std::fs::write(dir.join("file.txt"), b"plain").expect("file");
    let target = base.path().join("target");
    std::fs::create_dir_all(&target).expect("target");
    let output = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(dir.join("jn"))
        .arg(&target)
        .output()
        .expect("mklink");
    assert!(output.status.success(), "mklink: {output:?}");
    let handle = open_dir(&dir);
    // SAFETY: a folder opened for listing.
    let listed = unsafe { list(handle.0) }.expect("list");
    let attributes = |name: &str| {
        listed
            .iter()
            .find(|(n, _)| *n == wide(name))
            .map(|(_, a)| a & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT))
    };
    assert_eq!(names(&handle), vec!["file.txt", "jn", "sub"]);
    assert_eq!(attributes("sub"), Some(FILE_ATTRIBUTE_DIRECTORY));
    assert_eq!(
        attributes("jn"),
        Some(FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)
    );
    // SAFETY: entries of the open folder.
    unsafe {
        let link = open_relative(handle.0, &wide("jn"), READ_CONTROL).expect("open link");
        assert_ne!(
            link.attributes().expect("attributes") & FILE_ATTRIBUTE_REPARSE_POINT,
            0,
            "the link itself, not its target"
        );
        let file = open_relative(handle.0, &wide("file.txt"), FILE_READ_DATA).expect("open file");
        assert_eq!(read_all(&file), "plain");
        // Names are matched exactly.
        assert!(open_relative(handle.0, &wide("missing"), READ_CONTROL).is_err());
    }
}

/// D313: a folder moved away and replaced by a link to elsewhere while it is
/// held open: its entries still open from the folder that was listed.
#[test]
fn open_relative_is_not_redirected_by_a_swapped_folder() {
    let base = tempfile::tempdir().expect("base");
    let dir = base.path().join("d");
    std::fs::create_dir_all(&dir).expect("dir");
    std::fs::write(dir.join("x"), b"listed").expect("file");
    let elsewhere = base.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("elsewhere");
    std::fs::write(elsewhere.join("x"), b"elsewhere").expect("file");
    let handle = open_dir(&dir);
    std::fs::rename(&dir, base.path().join("moved")).expect("move the folder away");
    let output = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&dir)
        .arg(&elsewhere)
        .output()
        .expect("mklink");
    assert!(output.status.success(), "mklink: {output:?}");
    assert_eq!(
        std::fs::read_to_string(dir.join("x")).expect("by path"),
        "elsewhere"
    );
    // SAFETY: an entry of the open folder.
    let file = unsafe { open_relative(handle.0, &wide("x"), FILE_READ_DATA) }.expect("open");
    assert_eq!(read_all(&file), "listed");
}

/// D313: entries far past `MAX_PATH` open relative to their folder.
#[test]
fn open_relative_reaches_past_max_path() {
    let base = tempfile::tempdir().expect("base");
    let mut handle = open_dir(base.path());
    let mut depth_path = base.path().to_path_buf();
    let name = "a-folder-name-of-forty-characters-000000";
    // `create_dir_all` handles long paths itself.
    let mut deep = std::fs::canonicalize(base.path()).expect("canonical");
    for _ in 0..12 {
        deep = deep.join(name);
    }
    std::fs::create_dir_all(&deep).expect("deep dirs");
    std::fs::write(deep.join("leaf"), b"deep").expect("leaf");
    for _ in 0..12 {
        depth_path = depth_path.join(name);
        // SAFETY: an entry of the open folder.
        handle =
            unsafe { open_relative(handle.0, &wide(name), READ_CONTROL | FILE_LIST_DIRECTORY) }
                .expect("open a deep folder");
    }
    assert!(depth_path.as_os_str().len() > 480);
    assert_eq!(names(&handle), vec!["leaf"]);
    // SAFETY: an entry of the open folder.
    let leaf = unsafe { open_relative(handle.0, &wide("leaf"), FILE_READ_DATA) }.expect("leaf");
    assert_eq!(read_all(&leaf), "deep");
}
