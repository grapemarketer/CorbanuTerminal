#![cfg(any(unix, windows))]

use super::*;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

fn history_fixture(contents: &str) -> (TempDir, File, File, File) {
    let directory = TempDir::new().expect("synthetic history directory");
    let path = directory.path().join(HISTORY_FILENAME);
    std::fs::write(&path, contents).expect("write synthetic history");
    let original = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open operation descriptor");
    let retained = original
        .try_clone()
        .expect("duplicate open file description");
    let competitor = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("independent open file description");
    (directory, original, retained, competitor)
}

fn assert_released(competitor: &File) {
    assert!(
        competitor.try_lock().is_ok(),
        "operation returned but retained duplicate still holds its advisory lock"
    );
    competitor.unlock().expect("release competing control lock");
}

// This control relies on Unix open-file-description lock ownership.
#[cfg(unix)]
#[test]
fn issue928_native_control_duplicate_retains_lock_until_explicit_unlock() {
    let (_directory, original, retained, competitor) = history_fixture("");
    original.try_lock().expect("take native exclusive lock");
    drop(original);
    assert!(matches!(
        competitor.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
    retained.unlock().expect("explicitly release retained lock");
    assert_released(&competitor);
}

#[test]
fn issue928_append_releases_lock_with_retained_descriptor() {
    let (directory, original, retained, competitor) = history_fixture("");
    let line = "{\"session_id\":\"synthetic\",\"ts\":1,\"text\":\"hello\"}\n";
    append_history_line(original, line, /*history_max_bytes*/ None).expect("append history");
    assert_eq!(
        std::fs::read_to_string(directory.path().join(HISTORY_FILENAME)).unwrap(),
        line
    );
    assert_released(&competitor);
    assert!(retained.metadata().is_ok(), "duplicate remains open");
}

#[test]
fn issue928_trim_releases_lock_with_retained_descriptor() {
    let first = "{\"session_id\":\"old\",\"ts\":1,\"text\":\"first\"}\n";
    let last = "{\"session_id\":\"new\",\"ts\":2,\"text\":\"last\"}\n";
    let (directory, original, retained, competitor) = history_fixture(first);
    append_history_line(original, last, Some(last.len())).expect("append and trim history");
    assert_eq!(
        std::fs::read_to_string(directory.path().join(HISTORY_FILENAME)).unwrap(),
        last
    );
    assert_released(&competitor);
    assert!(retained.metadata().is_ok(), "duplicate remains open");
}

#[test]
fn issue928_append_error_releases_lock_with_retained_descriptor() {
    let (directory, original, _retained, competitor) = history_fixture("");
    drop(original);
    let read_only = File::open(directory.path().join(HISTORY_FILENAME)).unwrap();
    let retained = read_only.try_clone().unwrap();
    let result = append_history_line(read_only, "synthetic\n", /*history_max_bytes*/ None);
    assert!(
        result.is_err(),
        "a read-only descriptor must fail its write"
    );
    assert_eq!(
        std::fs::read_to_string(directory.path().join(HISTORY_FILENAME)).unwrap(),
        ""
    );
    assert_released(&competitor);
    assert!(retained.metadata().is_ok(), "duplicate remains open");
}

#[test]
fn issue928_lookup_releases_shared_lock_with_retained_descriptor() {
    let (directory, original, retained, competitor) =
        history_fixture("{\"session_id\":\"synthetic\",\"ts\":1,\"text\":\"hello\"}\n");
    let log_id = log_identity(&original.metadata().unwrap()).unwrap();
    assert_eq!(
        lookup_history_entry_from_file(original, log_id, /*offset*/ 0),
        Some(HistoryEntry {
            session_id: "synthetic".to_string(),
            ts: 1,
            text: "hello".to_string(),
        })
    );
    assert_released(&competitor);
    assert!(retained.metadata().is_ok(), "duplicate remains open");
    assert!(directory.path().join(HISTORY_FILENAME).exists());
}

#[test]
fn issue928_lookup_failure_exits_release_shared_lock() {
    for (contents, offset) in [("malformed\n", 0), ("", 0)] {
        let (_directory, original, retained, competitor) = history_fixture(contents);
        assert_eq!(
            lookup_history_entry_from_file(original, /*log_id*/ 0, offset),
            None
        );
        assert_released(&competitor);
        assert!(retained.metadata().is_ok(), "duplicate remains open");
    }

    let (directory, original, retained, competitor) = history_fixture("synthetic\n");
    let actual_id = log_identity(&original.metadata().unwrap()).unwrap();
    assert_eq!(
        lookup_history_entry_from_file(original, actual_id.wrapping_add(1), /*offset*/ 0),
        None
    );
    assert_released(&competitor);
    assert!(retained.metadata().is_ok(), "duplicate remains open");

    let write_only = OpenOptions::new()
        .write(true)
        .open(directory.path().join(HISTORY_FILENAME))
        .unwrap();
    let retained_write_only = write_only.try_clone().unwrap();
    assert_eq!(
        lookup_history_entry_from_file(write_only, /*log_id*/ 0, /*offset*/ 0),
        None
    );
    assert_released(&competitor);
    assert!(retained_write_only.metadata().is_ok());
}

#[test]
fn issue1003_lookup_batch_releases_shared_lock() {
    let (directory, original, retained, competitor) = history_fixture(concat!(
        "{\"session_id\":\"synthetic\",\"ts\":0,\"text\":\"oldest\"}\n",
        "{\"session_id\":\"synthetic\",\"ts\":1,\"text\":\"newest\"}\n",
    ));
    let config = HistoryConfig::new(directory.path(), &codex_config::types::History::default());
    let log_id = log_identity(&original.metadata().unwrap()).unwrap();
    let batch = batch::lookup_batch_from_file(
        original,
        log_id,
        HistoryBatchCursor::new(/*end_offset*/ 1),
        &config,
    )
    .expect("read history batch");
    assert_eq!(
        batch
            .entries
            .into_iter()
            .map(|row| (row.offset, row.entry.map(|entry| entry.text)))
            .collect::<Vec<_>>(),
        vec![
            (1, Some("newest".to_string())),
            (0, Some("oldest".to_string())),
        ]
    );
    assert_released(&competitor);
    assert!(retained.metadata().is_ok(), "duplicate remains open");
}

#[test]
fn issue1003_lookup_batch_failure_exits_release_shared_lock() {
    let (directory, original, _retained, competitor) =
        history_fixture("{\"session_id\":\"synthetic\",\"ts\":0,\"text\":\"hello\"}\n");
    let config = HistoryConfig::new(directory.path(), &codex_config::types::History::default());
    let log_id = log_identity(&original.metadata().unwrap()).unwrap();
    drop(original);
    let write_only = OpenOptions::new()
        .write(true)
        .open(directory.path().join(HISTORY_FILENAME))
        .unwrap();
    let retained = write_only.try_clone().unwrap();
    let result = batch::lookup_batch_from_file(
        write_only,
        log_id,
        HistoryBatchCursor::new(/*end_offset*/ 0),
        &config,
    );
    assert!(
        result.is_err(),
        "a write-only descriptor must fail the batch scan read"
    );
    assert_released(&competitor);
    assert!(retained.metadata().is_ok(), "duplicate remains open");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn issue928_concurrent_public_appends_preserve_all_entries() {
    let directory = TempDir::new().unwrap();
    let config = HistoryConfig::new(directory.path(), &codex_config::types::History::default());
    let mut children = Vec::new();
    for id in 0..24 {
        let config = config.clone();
        children.push(tokio::spawn(async move {
            append_entry(&format!("entry-{id}"), format!("session-{id}"), &config).await
        }));
    }
    for child in children {
        child.await.unwrap().expect("concurrent append");
    }
    let contents = std::fs::read_to_string(directory.path().join(HISTORY_FILENAME)).unwrap();
    let mut actual = contents
        .lines()
        .map(|line| {
            let entry: HistoryEntry = serde_json::from_str(line).unwrap();
            (entry.session_id, entry.text)
        })
        .collect::<Vec<_>>();
    let mut expected = (0..24)
        .map(|id| (format!("session-{id}"), format!("entry-{id}")))
        .collect::<Vec<_>>();
    actual.sort();
    expected.sort();
    assert_eq!(actual, expected);
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(directory.path().join(HISTORY_FILENAME))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[tokio::test]
async fn issue928_disabled_persistence_creates_no_history_file() {
    let directory = TempDir::new().unwrap();
    let config = HistoryConfig {
        codex_home: directory.path().to_path_buf(),
        persistence: HistoryPersistence::None,
        max_bytes: None,
    };
    append_entry("synthetic", "session", &config).await.unwrap();
    assert!(!directory.path().join(HISTORY_FILENAME).exists());
}
