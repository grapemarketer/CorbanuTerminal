use super::unlabelled_writable_root_warnings;
use codex_protocol::ThreadId;
use pretty_assertions::assert_eq;
use std::path::PathBuf;

/// #734: a writable root the non-admin sandbox couldn't label is named once
/// per thread, with the shared warning text.
#[test]
fn an_unlabelled_writable_root_is_warned_once_per_thread() {
    let root = PathBuf::from("/unlabelled-tests/once");
    let thread = ThreadId::new();
    let failed = [root.clone()];
    let writable = [PathBuf::from("/unlabelled-tests/other"), root.clone()];
    let expected = vec![codex_windows_sandbox::unlabelled_writable_root_warning(
        &root,
    )];
    assert_eq!(
        unlabelled_writable_root_warnings(thread, &failed, &writable),
        expected
    );
    assert_eq!(
        unlabelled_writable_root_warnings(thread, &failed, &writable),
        Vec::<String>::new()
    );
    assert_eq!(
        unlabelled_writable_root_warnings(ThreadId::new(), &failed, &writable),
        expected
    );
}

/// A root another thread's run couldn't label, which isn't writable in this
/// run, says nothing here.
#[test]
fn an_unlabelled_root_outside_this_runs_writable_roots_is_not_warned() {
    let mine = PathBuf::from("/unlabelled-tests/mine");
    let other = PathBuf::from("/unlabelled-tests/theirs");
    assert_eq!(
        unlabelled_writable_root_warnings(ThreadId::new(), &[other], &[mine]),
        Vec::<String>::new()
    );
}
