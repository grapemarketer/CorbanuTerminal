use super::planted_notice_from;
use codex_protocol::ThreadId;
use pretty_assertions::assert_eq;
use std::path::PathBuf;

/// D424 (#1775): after a run, the folders recorded as possibly planted in
/// the run's writable roots are told once, including those recorded after
/// the thread was last told (a command it left running ended meanwhile);
/// those in other folders aren't.
#[test]
fn planted_records_in_the_runs_roots_are_told_once() {
    let root = PathBuf::from("/planted-tests/root");
    let writable = [root.clone()];
    let git = root.join("sub").join(".git");
    let codex = PathBuf::from("/PLANTED-TESTS/ROOT/.codex");
    let elsewhere = PathBuf::from("/planted-tests/other/.git");
    let log = &[(1, elsewhere), (2, git.clone()), (3, codex.clone())];
    let since_until = |until: u64| {
        move |from: u64| -> Vec<PathBuf> {
            log.iter()
                .filter(|(seq, _)| *seq > from && *seq <= until)
                .map(|(_, path)| path.clone())
                .collect()
        }
    };
    let since = since_until(3);
    let thread = ThreadId::new();

    let first = planted_notice_from(thread, 0, &writable, || 3, since);
    let again = planted_notice_from(thread, 3, &writable, || 3, since);
    // Told up to 1 only (say a background command recorded 2 and 3 after
    // the last notice): the next run, with a later mark, still tells them.
    let other_thread = ThreadId::new();
    let _ = planted_notice_from(other_thread, 0, &writable, || 1, since_until(1));
    let later = planted_notice_from(other_thread, 3, &writable, || 3, since);

    let expected = codex_config::planted_after_command_notice(&[git, codex]);
    assert_eq!(
        (first.clone(), again, later),
        (expected.clone(), None, expected)
    );
    assert!(first.is_some_and(|notice| notice.contains("Possibly planted git folder: ")));
}
