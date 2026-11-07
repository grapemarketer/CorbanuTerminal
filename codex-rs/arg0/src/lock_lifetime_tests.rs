use super::Arg0DispatchPaths;
use super::Arg0PathEntryGuard;
use super::LOCK_FILENAME;
use super::try_lock_dir;
use pretty_assertions::assert_eq;
use std::fs::File;
use std::fs::OpenOptions;
use std::fs::TryLockError;
use std::io;
use std::path::Path;

#[test]
fn issue928_arg0_session_guard_releases_with_retained_descriptor() -> io::Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let lock_path = temp_dir.path().join(LOCK_FILENAME);
    create_lock(temp_dir.path())?;
    let lock = try_lock_dir(temp_dir.path())?
        .ok_or_else(|| io::Error::other("fixture lock was not acquired"))?;
    let retained_descriptor = lock.try_clone()?;
    let contender = open_contender(&lock_path)?;
    let guard = Arg0PathEntryGuard::new(temp_dir, lock, Arg0DispatchPaths::default());
    assert!(matches!(
        contender.try_lock(),
        Err(TryLockError::WouldBlock)
    ));

    drop(guard);

    assert!(!lock_path.exists());
    contender.try_lock()?;
    contender.unlock()?;
    drop(retained_descriptor);
    Ok(())
}

#[test]
fn issue928_arg0_directory_lock_releases_with_retained_descriptor() -> io::Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let lock_path = temp_dir.path().join(LOCK_FILENAME);
    create_lock(temp_dir.path())?;
    let original_contents = b"preserve lock contents";
    std::fs::write(&lock_path, original_contents)?;
    let lock = try_lock_dir(temp_dir.path())?
        .ok_or_else(|| io::Error::other("fixture lock was not acquired"))?;
    let retained_descriptor = lock.try_clone()?;
    let contender = open_contender(&lock_path)?;
    assert!(matches!(
        contender.try_lock(),
        Err(TryLockError::WouldBlock)
    ));

    drop(lock);

    contender.try_lock()?;
    assert_eq!(std::fs::read(&lock_path)?, original_contents);
    contender.unlock()?;
    drop(retained_descriptor);
    Ok(())
}

#[test]
fn issue928_arg0_acquired_lock_releases_on_error_before_handoff() -> io::Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let lock_path = temp_dir.path().join(LOCK_FILENAME);
    create_lock(temp_dir.path())?;
    let mut retained_descriptor = None;
    let setup_result: io::Result<()> = (|| {
        let lock = try_lock_dir(temp_dir.path())?
            .ok_or_else(|| io::Error::other("fixture lock was not acquired"))?;
        retained_descriptor = Some(lock.try_clone()?);
        Err(io::Error::other("synthetic setup error after acquisition"))
    })();
    assert!(setup_result.is_err());
    let retained_descriptor = retained_descriptor
        .ok_or_else(|| io::Error::other("fixture did not retain the descriptor"))?;
    let contender = open_contender(&lock_path)?;

    contender.try_lock()?;
    contender.unlock()?;
    drop(retained_descriptor);
    Ok(())
}

#[test]
fn issue928_arg0_closing_duplicate_keeps_session_guard_locked() -> io::Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let lock_path = temp_dir.path().join(LOCK_FILENAME);
    create_lock(temp_dir.path())?;
    let lock = try_lock_dir(temp_dir.path())?
        .ok_or_else(|| io::Error::other("fixture lock was not acquired"))?;
    let duplicate = lock.try_clone()?;
    let contender = open_contender(&lock_path)?;
    let guard = Arg0PathEntryGuard::new(temp_dir, lock, Arg0DispatchPaths::default());

    drop(duplicate);

    assert!(matches!(
        contender.try_lock(),
        Err(TryLockError::WouldBlock)
    ));
    assert!(lock_path.exists());
    drop(guard);
    contender.try_lock()?;
    contender.unlock()?;
    Ok(())
}

fn create_lock(dir: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(LOCK_FILENAME))
}

fn open_contender(path: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open(path)
}
