use std::fs::File;
use std::io;
use std::ops::Deref;

/// Owns a refresh lock without letting duplicate descriptors extend its scope.
pub(super) struct ClaudeRefreshLock(File);

impl ClaudeRefreshLock {
    pub(super) fn lock(file: File) -> io::Result<Self> {
        file.lock()?;
        Ok(Self(file))
    }
}

impl Deref for ClaudeRefreshLock {
    type Target = File;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for ClaudeRefreshLock {
    fn drop(&mut self) {
        // Other copies must not retain the lock after its refresh scope ends.
        let _ = self.0.unlock();
    }
}
