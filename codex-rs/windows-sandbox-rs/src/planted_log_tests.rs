use super::*;
use pretty_assertions::assert_eq;

#[test]
fn records_after_a_mark_are_reported_in_order() {
    let a = PathBuf::from(r"Z:\planted_log_tests\a\.git");
    let b = PathBuf::from(r"Z:\planted_log_tests\b\.codex");
    let before = planted_records_mark();
    record([&a, &b]);
    record_git(&[(a.clone(), vec!["config changed".to_string()])]);
    let after = planted_records_mark();
    let since_before: Vec<(PathBuf, Vec<String>)> = planted_records_since(before)
        .into_iter()
        .filter(|record| {
            record
                .path
                .to_string_lossy()
                .starts_with(r"Z:\planted_log_tests\")
        })
        .map(|record| (record.path, record.git_changes))
        .collect();
    assert_eq!(
        since_before,
        vec![
            (a.clone(), Vec::new()),
            (b, Vec::new()),
            (a.clone(), vec!["config changed".to_string()]),
        ]
    );
    assert!(
        !planted_records_since(after)
            .iter()
            .any(|record| record.path == a)
    );
}
