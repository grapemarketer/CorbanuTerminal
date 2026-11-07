use std::fs::File;
use std::fs::TryLockError;
use std::ops::Deref;

/// Owns an arg0 file lock independently of duplicated file descriptors.
pub(super) struct Arg0FileLock(File);

impl Arg0FileLock {
    pub(super) fn try_lock(file: File) -> Result<Self, TryLockError> {
        file.try_lock()?;
        Ok(Self(file))
    }
}

impl Deref for Arg0FileLock {
    type Target = File;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for Arg0FileLock {
    fn drop(&mut self) {
        // A duplicated or inherited descriptor must not extend this lock's scope.
        let _ = self.0.unlock();
    }
}
