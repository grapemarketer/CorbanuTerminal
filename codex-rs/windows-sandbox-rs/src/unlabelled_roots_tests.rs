use super::*;
use pretty_assertions::assert_eq;

#[test]
fn a_failed_root_is_reported_after_an_earlier_mark_until_labelled() {
    let root = PathBuf::from(r"Z:\unlabelled_roots_tests\a");
    let before = unlabelled_roots_mark();
    record(&root, /*labelled*/ false);
    let after = unlabelled_roots_mark();
    assert!(unlabelled_roots_since(before).contains(&root));
    // Not failed again since: a later run doesn't report it.
    assert!(!unlabelled_roots_since(after).contains(&root));
    record(&root, /*labelled*/ false);
    assert!(unlabelled_roots_since(after).contains(&root));
    record(&root, /*labelled*/ true);
    assert!(!unlabelled_roots_since(before).contains(&root));
}

#[test]
fn labelling_an_unknown_root_reports_nothing() {
    let root = PathBuf::from(r"Z:\unlabelled_roots_tests\b");
    let before = unlabelled_roots_mark();
    record(&root, /*labelled*/ true);
    assert!(!unlabelled_roots_since(before).contains(&root));
}

#[test]
fn warning_names_the_root_the_reason_and_both_fixes() {
    let root = PathBuf::from(r"D:\shared\repo");
    assert_eq!(
        unlabelled_writable_root_warning(&root),
        format!(
            "`{}` is read-only to commands in the non-admin Windows sandbox, although it is a \
             writable folder: that sandbox runs commands at Low integrity, and the folder could \
             not be labelled Low. Labelling needs permission to change the folder's owner \
             settings (which owning it gives) on a file system that holds integrity labels, such \
             as NTFS. To let commands write there, own the folder, or switch to the default \
             sandbox by running /setup-default-sandbox, or set `sandbox = \"elevated\"` under \
             `[windows]` in config.toml.",
            root.display()
        )
    );
}
