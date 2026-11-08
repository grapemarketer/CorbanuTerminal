use super::*;
use pretty_assertions::assert_eq;

#[test]
fn records_after_a_mark_are_reported_with_their_numbers() {
    let a = PathBuf::from(r"Z:\planted_log_tests\a\.git");
    let b = PathBuf::from(r"Z:\planted_log_tests\b\.codex");
    let before = planted_records_mark();
    record([&a, &b]);
    let after = planted_records_mark();
    let since_before: Vec<PathBuf> = planted_records_since(before)
        .into_iter()
        .map(|(_, path)| path)
        .filter(|path| path.to_string_lossy().starts_with(r"Z:\planted_log_tests\"))
        .collect();
    assert_eq!(since_before, vec![a.clone(), b]);
    assert!(
        !planted_records_since(after)
            .iter()
            .any(|(_, path)| *path == a)
    );
}
