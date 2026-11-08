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

#[derive(Default)]
struct Log {
    /// Counts records in this process.
    seq: u64,
    entries: VecDeque<(u64, PathBuf)>,
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
        log.seq += 1;
        let seq = log.seq;
        log.entries.push_back((seq, path.clone()));
        if log.entries.len() > KEEP {
            log.entries.pop_front();
        }
    }
}

/// Marks now, for [`planted_records_since`].
pub fn planted_records_mark() -> u64 {
    log().seq
}

/// The folders recorded as possibly planted after `mark`, with each
/// record's number, oldest first (always none off Windows).
pub fn planted_records_since(mark: u64) -> Vec<(u64, PathBuf)> {
    log()
        .entries
        .iter()
        .filter(|(seq, _)| *seq > mark)
        .cloned()
        .collect()
}

#[cfg(test)]
#[path = "planted_log_tests.rs"]
mod tests;
