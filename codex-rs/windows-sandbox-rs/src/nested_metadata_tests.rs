use super::*;
use pretty_assertions::assert_eq;

fn junction(link: &Path, target: &Path) {
    let status = std::process::Command::new("cmd")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .stdout(std::process::Stdio::null())
        .status()
        .expect("mklink");
    assert!(status.success(), "mklink /J failed");
}

/// Finds nested entries of either name and any case, not the top-level
/// ones, nothing inside `.git` or behind a junction; a junction named
/// `.codex` is found but never matches.
#[test]
fn find_reports_nested_entries_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dunce::canonicalize(dir.path()).expect("canonical");
    let outside = tempfile::tempdir().expect("outside");
    for path in [
        ".codex\\inner\\.codex",
        "a\\.codex",
        "a\\b\\.AGENTS",
        ".git\\x\\.codex",
        "target\\.codex",
        "j",
    ] {
        std::fs::create_dir_all(root.join(path)).expect("create");
    }
    std::fs::create_dir_all(outside.path().join("c\\.codex")).expect("create outside");
    junction(&root.join("link"), outside.path());
    junction(&root.join("j\\.codex"), &root.join("target"));

    let found = find(std::slice::from_ref(&root));
    let mut paths: Vec<(PathBuf, bool)> = found
        .iter()
        .map(|entry| (entry.path.clone(), entry.identity.is_some()))
        .collect();
    paths.sort();
    let mut expected = vec![
        (root.join("a\\.codex"), true),
        (root.join("a\\b\\.AGENTS"), true),
        (root.join("j\\.codex"), false),
        (root.join("target\\.codex"), true),
    ];
    expected.sort();
    assert_eq!(paths, expected);
    assert_eq!(to_protect(&found).len(), 3, "the junction is not protected");
}

/// The same file in the same place matches; a new one, a replaced one and
/// one without an identity don't.
#[test]
fn appeared_compares_path_and_identity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dunce::canonicalize(dir.path()).expect("canonical");
    for path in ["kept\\.codex", "replaced\\.codex"] {
        std::fs::create_dir_all(root.join(path)).expect("create");
    }
    let before = find(std::slice::from_ref(&root));
    std::fs::remove_dir(root.join("replaced\\.codex")).expect("remove");
    std::fs::create_dir(root.join("replaced\\.codex")).expect("recreate");
    std::fs::create_dir_all(root.join("new\\.agents")).expect("create new");

    let mut appeared = appeared(&before, find(std::slice::from_ref(&root)));
    appeared.sort();
    assert_eq!(
        appeared,
        vec![root.join("new\\.agents"), root.join("replaced\\.codex")]
    );
    let roots = [root.clone()];
    assert_eq!(
        (
            appeared_in_session(&roots, &before, &root.join("KEPT\\.codex")),
            appeared_in_session(&roots, &before, &root.join("replaced\\.codex")),
            appeared_in_session(&roots, &before, &root.join(".codex")),
        ),
        (false, true, false)
    );
}
