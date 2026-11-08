//! D420/D421 (#1775): repositories below the top of a legacy session's
//! writable roots, and roots that are bare repositories. A new one counts
//! unless it has the plain `git init`/`git clone` shape; an existing one
//! counts when what git would run from it changes.

use super::*;
use pretty_assertions::assert_eq;

const INIT_CONFIG: &str = "[core]\r\n\trepositoryformatversion = 0\r\n\tfilemode = false\r\n\tbare = false\r\n\tlogallrefupdates = true\r\n\tsymlinks = false\r\n\tignorecase = true\r\n";

fn canonical(path: &Path) -> PathBuf {
    dunce::canonicalize(path).expect("canonical")
}

/// The shape `git init` leaves in `dir`, with `config`.
fn init_shape(dir: &Path, config: &str) {
    for folder in [
        "hooks",
        "info",
        "objects\\info",
        "objects\\pack",
        "refs\\heads",
        "refs\\tags",
    ] {
        fs::create_dir_all(dir.join(folder)).expect("create folder");
    }
    fs::write(dir.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    fs::write(dir.join("config"), config).expect("config");
    fs::write(dir.join("description"), "Unnamed repository\n").expect("description");
    fs::write(dir.join("hooks\\pre-commit.sample"), "#!/bin/sh\n").expect("sample hook");
}

/// What the records hold for `path`, sorted.
fn changes(path: &Path) -> Option<Vec<String>> {
    crate::planted_git_changes()
        .expect("records")
        .into_iter()
        .find(|(recorded, _)| recorded == path)
        .map(|(_, mut changes)| {
            changes.sort();
            changes
        })
}

#[test]
fn legacy_repositories_are_watched() {
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let root = test_root.path().join("root");
    fs::create_dir_all(&root).expect("create root");
    let root = canonical(&root);
    let kept = root.join("kept\\.git");
    let untouched = root.join("untouched\\.git");
    let replaced = root.join("replaced\\.git");
    init_shape(&kept, INIT_CONFIG);
    init_shape(&untouched, INIT_CONFIG);
    init_shape(&replaced, INIT_CONFIG);
    let plain = root.join("cloned\\.git");
    let planted = root.join("planted\\.git");
    let bare = root.join("scratch");
    let mark = crate::planted_records_mark();

    let lease =
        crate::integrity_leases::acquire(std::slice::from_ref(&root), /*log_dir*/ None)
            .expect("lease");
    // D421: a fresh `git init` shape is recorded but not asked about.
    init_shape(&plain, INIT_CONFIG);
    // A new repository with a hook.
    init_shape(&planted, INIT_CONFIG);
    fs::write(planted.join("hooks\\post-checkout"), "evil").expect("hook");
    // An existing one whose config gains a program, and one rewritten with
    // the same content.
    fs::write(
        kept.join("config"),
        format!("{INIT_CONFIG}\tfsmonitor = evil.exe\r\n"),
    )
    .expect("config");
    fs::write(untouched.join("config"), INIT_CONFIG).expect("same config");
    fs::write(untouched.join("HEAD"), "ref: refs/heads/other\n").expect("bookkeeping");
    // A repository replaced by a plain one: only new ones pass as plain.
    fs::remove_dir_all(&replaced).expect("remove");
    init_shape(&replaced, INIT_CONFIG);
    // A bare repository planted in a plain folder (gap 2).
    init_shape(&bare, &format!("{INIT_CONFIG}\tpager = evil.exe\r\n"));
    // Can't be confirmed while the session runs.
    let refused_live = crate::confirm_planted_project_metadata(&kept).is_err();
    drop(lease);

    let after = (
        changes(&plain),
        changes(&planted),
        changes(&kept),
        changes(&untouched),
        changes(&bare),
        changes(&replaced),
    );
    let told: Vec<PathBuf> = crate::planted_records_since(mark)
        .into_iter()
        .filter(|record| !record.git_changes.is_empty())
        .map(|record| record.path)
        .collect();
    // Confirmed, it stays quiet until it changes again.
    let confirmed = crate::confirm_planted_project_metadata(&kept).expect("confirm");
    drop(
        crate::integrity_leases::acquire(std::slice::from_ref(&root), /*log_dir*/ None)
            .expect("unchanged session"),
    );
    let after_unchanged = changes(&kept);
    let lease =
        crate::integrity_leases::acquire(std::slice::from_ref(&root), /*log_dir*/ None)
            .expect("changing session");
    fs::write(kept.join("hooks\\pre-push"), "evil").expect("hook");
    // D421: once there, a plain repository's later changes count.
    fs::write(plain.join("hooks\\post-merge"), "evil").expect("hook");
    drop(lease);
    let after_changing = (changes(&kept), changes(&plain));

    assert_eq!(
        (
            refused_live,
            after,
            told.contains(&planted) && told.contains(&kept) && told.contains(&bare),
            confirmed,
            after_unchanged,
            after_changing,
        ),
        (
            true,
            (
                None,
                Some(vec!["new repository".to_string()]),
                Some(vec!["config changed".to_string()]),
                None,
                Some(vec!["new repository".to_string()]),
                Some(vec!["replaced".to_string()]),
            ),
            true,
            true,
            None,
            (
                Some(vec!["hooks/pre-push added".to_string()]),
                Some(vec!["hooks/post-merge added".to_string()]),
            ),
        )
    );
}

/// Gap 2 (#1775): a writable root (`%TEMP%` too) that becomes a bare
/// repository counts; one that already is one counts when its config
/// changes.
#[test]
fn legacy_root_becoming_a_bare_repository_is_recorded() {
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let base = test_root.path().join("roots");
    for dir in ["becomes", "already"] {
        fs::create_dir_all(base.join(dir)).expect("create root");
    }
    let base = canonical(&base);
    let (becomes, already) = (base.join("becomes"), base.join("already"));
    init_shape(&already, INIT_CONFIG);
    let roots = [becomes.clone(), already.clone()];

    let lease = crate::integrity_leases::acquire(&roots, /*log_dir*/ None).expect("lease");
    init_shape(&becomes, &format!("{INIT_CONFIG}\tpager = evil.exe\r\n"));
    fs::write(
        already.join("config"),
        format!("{INIT_CONFIG}\teditor = evil.exe\r\n"),
    )
    .expect("config");
    drop(lease);

    assert_eq!(
        (changes(&becomes), changes(&already)),
        (
            Some(vec!["new repository".to_string()]),
            Some(vec!["config changed".to_string()]),
        )
    );
}

/// D422 (#1775): a hooks folder `core.hooksPath` names in the work tree
/// (Husky v9's `.husky\_`, whose stubs run `.husky\<hook>`) is watched: a
/// new hook there counts, a rewrite with the same content doesn't.
#[test]
fn legacy_hooks_path_targets_are_watched() {
    let _guard = legacy_process_test_guard();
    let test_root = user_owned_test_root();
    let root = test_root.path().join("husky");
    fs::create_dir_all(root.join(".husky\\_")).expect("create husky");
    let root = canonical(&root);
    init_shape(
        &root.join(".git"),
        &format!("{INIT_CONFIG}\thooksPath = .husky/_\r\n"),
    );
    fs::write(root.join(".husky\\_\\pre-commit"), "stub").expect("stub");
    let (husky, stubs) = (root.join(".husky"), root.join(".husky\\_"));

    let lease =
        crate::integrity_leases::acquire(std::slice::from_ref(&root), /*log_dir*/ None)
            .expect("lease");
    fs::write(stubs.join("pre-commit"), "stub").expect("same stub");
    fs::write(husky.join("README.md"), "not a hook").expect("not a hook");
    fs::write(husky.join("post-checkout"), "evil").expect("hook");
    drop(lease);

    assert_eq!(
        (changes(&husky), changes(&stubs)),
        (
            Some(vec!["core.hooksPath: post-checkout added".to_string()]),
            None,
        )
    );
}
