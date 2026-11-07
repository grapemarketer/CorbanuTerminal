use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use codex_protocol::ThreadId;
use tracing::warn;

use crate::ThreadStoreError;
use crate::ThreadStoreResult;

const WRITER_LOCK_DIR: &str = "thread-writer-locks";
const COORDINATION_LOCK_FILE: &str = ".coordination.lock";

pub(super) struct WriterLockCoordinator {
    directory: PathBuf,
    cleanup_attempted: AtomicBool,
}

pub(super) struct WriterLockGuard {
    coordinator: Arc<WriterLockCoordinator>,
    path: PathBuf,
    file: Option<File>,
}

impl WriterLockCoordinator {
    pub(super) fn new(codex_home: &Path) -> Self {
        Self {
            directory: codex_home.join(WRITER_LOCK_DIR),
            cleanup_attempted: AtomicBool::new(false),
        }
    }

    pub(super) fn acquire(
        self: &Arc<Self>,
        thread_id: ThreadId,
    ) -> ThreadStoreResult<WriterLockGuard> {
        let coordination_lock = self.lock_coordination()?;
        if !self.cleanup_attempted.swap(true, Ordering::Relaxed)
            && let Err(err) = self.remove_stale_thread_locks()
        {
            warn!("failed to clean up stale thread writer locks: {err}");
        }

        let path = self.directory.join(format!("{thread_id}.lock"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|err| ThreadStoreError::Internal {
                message: format!(
                    "failed to open thread writer lock {}: {err}",
                    path.display()
                ),
            })?;

        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(ThreadStoreError::Conflict {
                    message: format!("thread {thread_id} already has an active writer"),
                });
            }
            Err(std::fs::TryLockError::Error(err)) => {
                return Err(ThreadStoreError::Internal {
                    message: format!(
                        "failed to acquire thread writer lock {}: {err}",
                        path.display()
                    ),
                });
            }
        }

        drop(coordination_lock);
        Ok(WriterLockGuard {
            coordinator: Arc::clone(self),
            path,
            file: Some(file),
        })
    }

    fn lock_coordination(&self) -> ThreadStoreResult<CoordinationLock> {
        fs::create_dir_all(&self.directory).map_err(|err| ThreadStoreError::Internal {
            message: format!(
                "failed to create thread writer lock directory {}: {err}",
                self.directory.display()
            ),
        })?;
        let path = self.directory.join(COORDINATION_LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|err| ThreadStoreError::Internal {
                message: format!(
                    "failed to open thread writer coordination lock {}: {err}",
                    path.display()
                ),
            })?;
        file.lock().map_err(|err| ThreadStoreError::Internal {
            message: format!(
                "failed to acquire thread writer coordination lock {}: {err}",
                path.display()
            ),
        })?;
        Ok(CoordinationLock(file))
    }

    fn remove_stale_thread_locks(&self) -> io::Result<()> {
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(thread_id) = file_name.strip_suffix(".lock") else {
                continue;
            };
            if ThreadId::from_string(thread_id).is_err() {
                continue;
            }

            let path = entry.path();
            let file = match OpenOptions::new().read(true).write(true).open(&path) {
                Ok(file) => file,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => {
                    warn!(
                        "failed to inspect thread writer lock {}: {err}",
                        path.display()
                    );
                    continue;
                }
            };
            match file.try_lock() {
                Ok(()) => {
                    // Release before close even if another descriptor still exists.
                    if let Err(err) = file.unlock() {
                        warn!(
                            "failed to unlock stale thread writer lock {}: {err}",
                            path.display()
                        );
                    }
                    drop(file);
                    if let Err(err) = fs::remove_file(&path)
                        && err.kind() != io::ErrorKind::NotFound
                    {
                        warn!(
                            "failed to remove stale thread writer lock {}: {err}",
                            path.display()
                        );
                    }
                }
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(std::fs::TryLockError::Error(err)) => {
                    warn!(
                        "failed to inspect thread writer lock {}: {err}",
                        path.display()
                    );
                }
            }
        }
        Ok(())
    }
}

/// The held coordination lock, unlocked when dropped.
struct CoordinationLock(File);

impl Drop for CoordinationLock {
    fn drop(&mut self) {
        // #928: closing the file releases the lock only once no other copy of
        // the descriptor is open, and a child another thread forked keeps one
        // until it starts its program. Unlock now so a waiter waits only for
        // this holder.
        let _ = self.0.unlock();
    }
}

impl WriterLockGuard {
    fn cleanup(&mut self) {
        let coordination_lock = self.coordinator.lock_coordination();
        // Closing alone can leave the lock held through an inherited descriptor.
        // Release it even if coordinating the pathname cleanup failed. On the
        // successful path, the coordination lock still protects cleanup below.
        if let Some(file) = self.file.take()
            && let Err(err) = file.unlock()
        {
            warn!(
                "failed to unlock thread writer lock {}: {err}",
                self.path.display()
            );
        }
        let coordination_lock = match coordination_lock {
            Ok(lock) => lock,
            Err(err) => {
                warn!("failed to coordinate thread writer lock cleanup: {err}");
                return;
            }
        };

        // The writer file is closed before deletion so cleanup works on Windows too.
        if let Err(err) = fs::remove_file(&self.path)
            && err.kind() != io::ErrorKind::NotFound
        {
            warn!(
                "failed to remove thread writer lock {}: {err}",
                self.path.display()
            );
        }
        drop(coordination_lock);
    }
}

impl Drop for WriterLockGuard {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[cfg(test)]
#[path = "writer_lock_tests.rs"]
mod tests;
