//! D243 (#1412): `corbanu resume --last` reopens the conversation that was on
//! screen when the user last quit the TUI.
//!
//! Opening a conversation deliberately leaves its `updated_at` alone until a
//! turn is appended (upstream #22236), so the newest-turn lookup alone can
//! pick a different conversation than the one the user had open. On quit the
//! TUI records the on-screen thread here; `resume --last` prefers it unless a
//! thread in scope was updated after that quit (newest-turn semantics then
//! apply as upstream). `/resume` ordering and `fork --last` are unchanged.

use std::path::Path;
use std::path::PathBuf;

use codex_protocol::ThreadId;
use serde::Deserialize;
use serde::Serialize;

const QUIT_THREAD_RECORD_FILE: &str = "tui_quit_thread.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct QuitThreadRecord {
    pub(crate) thread_id: ThreadId,
    /// Unix seconds when the TUI quit with this thread on screen.
    pub(crate) quit_at: i64,
}

fn record_path(codex_home: &Path) -> PathBuf {
    codex_home.join(QUIT_THREAD_RECORD_FILE)
}

/// Records `thread_id` as the conversation on screen at quit. Best effort: a
/// failure only means `resume --last` falls back to the newest turn.
pub(crate) fn record_quit_thread(codex_home: &Path, thread_id: ThreadId, quit_at: i64) {
    let record = QuitThreadRecord { thread_id, quit_at };
    if let Err(err) = write_record(codex_home, &record) {
        tracing::warn!(%err, "failed to record the thread on screen at quit");
    }
}

fn write_record(codex_home: &Path, record: &QuitThreadRecord) -> std::io::Result<()> {
    let json = serde_json::to_vec(record).map_err(std::io::Error::other)?;
    let mut tmp = tempfile::NamedTempFile::new_in(codex_home)?;
    std::io::Write::write_all(&mut tmp, &json)?;
    tmp.persist(record_path(codex_home))
        .map(|_| ())
        .map_err(|err| err.error)
}

pub(crate) fn read_quit_thread_record(codex_home: &Path) -> Option<QuitThreadRecord> {
    let bytes = std::fs::read(record_path(codex_home)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Whether the quit record still describes the most recent use: no thread in
/// scope was updated after the quit (ties go to the record).
pub(crate) fn quit_record_is_current(
    record: &QuitThreadRecord,
    newest_updated_at: Option<i64>,
) -> bool {
    newest_updated_at.is_none_or(|updated_at| updated_at <= record.quit_at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn record_round_trips_and_overwrites() {
        let home = tempfile::tempdir().expect("tempdir");
        assert_eq!(read_quit_thread_record(home.path()), None);

        let first = ThreadId::new();
        let second = ThreadId::new();
        record_quit_thread(home.path(), first, /*quit_at*/ 100);
        record_quit_thread(home.path(), second, /*quit_at*/ 200);

        assert_eq!(
            read_quit_thread_record(home.path()),
            Some(QuitThreadRecord {
                thread_id: second,
                quit_at: 200,
            })
        );
    }

    #[test]
    fn corrupt_record_is_ignored() {
        let home = tempfile::tempdir().expect("tempdir");
        std::fs::write(record_path(home.path()), b"not json").expect("write");
        assert_eq!(read_quit_thread_record(home.path()), None);
    }

    #[test]
    fn newer_turn_after_quit_wins() {
        let record = QuitThreadRecord {
            thread_id: ThreadId::new(),
            quit_at: 100,
        };
        assert!(quit_record_is_current(
            &record, /*newest_updated_at*/ None
        ));
        assert!(quit_record_is_current(&record, Some(99)));
        assert!(quit_record_is_current(&record, Some(100)));
        assert!(!quit_record_is_current(&record, Some(101)));
    }
}
