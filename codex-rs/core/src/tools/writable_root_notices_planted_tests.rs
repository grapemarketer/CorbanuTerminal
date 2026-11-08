use super::planted_notice_from;
use codex_protocol::ThreadId;
use pretty_assertions::assert_eq;
use std::path::PathBuf;

/// D424 (#1775): after a run, the folders recorded as possibly planted in
/// the run's writable roots are told once, including those recorded before
/// this run's mark but after the thread's first run (a command it left
/// running ended meanwhile); one in a root the run doesn't have waits for a
/// run that has it.
#[test]
fn planted_records_in_the_runs_roots_are_told_once() {
    let root = PathBuf::from("/planted-tests/root");
    let other = PathBuf::from("/planted-tests/other");
    let git = root.join("sub").join(".git");
    let codex = PathBuf::from("/PLANTED-TESTS/ROOT/.codex");
    let elsewhere = other.join(".git");
    let log = &[(1, elsewhere.clone()), (2, git.clone()), (3, codex.clone())];
    let since_until = |until: u64| {
        move |from: u64| -> Vec<(u64, PathBuf)> {
            log.iter()
                .filter(|(seq, _)| *seq > from && *seq <= until)
                .cloned()
                .collect()
        }
    };
    let only_root = [root.clone()];
    let thread = ThreadId::new();

    // The thread's first run (mark 0) ends when only 1 and 2 are recorded.
    let first = planted_notice_from(thread, /*mark*/ 0, &only_root, since_until(2));
    // A later run (mark 3) still tells 3, recorded meanwhile.
    let later = planted_notice_from(thread, /*mark*/ 3, &only_root, since_until(3));
    let again = planted_notice_from(thread, /*mark*/ 3, &only_root, since_until(3));
    // A run with the other root tells 1, skipped so far.
    let other_root = planted_notice_from(thread, /*mark*/ 3, &[other], since_until(3));

    assert_eq!(
        (first.clone(), later, again, other_root),
        (
            codex_config::planted_after_command_notice(&[git]),
            codex_config::planted_after_command_notice(&[codex]),
            None,
            codex_config::planted_after_command_notice(&[elsewhere]),
        )
    );
    assert!(first.is_some_and(|notice| notice.contains("Possibly planted git folder: ")));
}
