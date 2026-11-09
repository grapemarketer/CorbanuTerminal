//! D424 (#1775): the possibly planted folders this process recorded, in
//! order, so Core can tell the person right after a command ends instead of
//! only at the next start. Core takes a [`planted_records_mark`] before a run
//! and asks for what was recorded since ([`planted_records_since`]). Display
//! only: the records themselves live with the integrity-label records.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::MutexGuard;

/// The newest records kept; older ones are only in the records file.
const KEEP: usize = 1024;

/// One record: its number in this process, the folder, and (D420) for a
/// repository whose contents changed, what changed (empty otherwise).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlantedRecord {
    pub seq: u64,
    pub path: PathBuf,
    pub git_changes: Vec<String>,
}

#[derive(Default)]
struct Log {
    /// Counts records in this process.
    seq: u64,
    entries: VecDeque<PlantedRecord>,
}

static LOG: LazyLock<Mutex<Log>> = LazyLock::new(Default::default);

fn log() -> MutexGuard<'static, Log> {
    LOG.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Notes that `paths` were just recorded as possibly planted.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn record<'a>(paths: impl IntoIterator<Item = &'a PathBuf>) {
    let mut log = log();
    for path in paths {
        log.push(path.clone(), Vec::new());
    }
}

/// D420: notes that these repositories were just recorded as changed, with
/// what changed.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) fn record_git(changed: &[(PathBuf, Vec<String>)]) {
    let mut log = log();
    for (path, changes) in changed {
        log.push(path.clone(), changes.clone());
    }
}

impl Log {
    fn push(&mut self, path: PathBuf, git_changes: Vec<String>) {
        self.seq += 1;
        self.entries.push_back(PlantedRecord {
            seq: self.seq,
            path,
            git_changes,
        });
        if self.entries.len() > KEEP {
            self.entries.pop_front();
        }
    }
}

/// Marks now, for [`planted_records_since`].
pub fn planted_records_mark() -> u64 {
    log().seq
}

/// The records made after `mark`, oldest first (always none off Windows).
pub fn planted_records_since(mark: u64) -> Vec<PlantedRecord> {
    log()
        .entries
        .iter()
        .filter(|record| record.seq > mark)
        .cloned()
        .collect()
}

#[cfg(test)]
#[path = "planted_log_tests.rs"]
mod tests;
