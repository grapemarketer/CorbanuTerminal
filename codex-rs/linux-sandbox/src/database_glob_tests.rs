//! #732: real directory fixtures for both expansion engines; no sandbox launch.
use super::*;
use codex_protocol::permissions::FileSystemSandboxEntry;
use pretty_assertions::assert_eq;
use std::os::unix::fs::PermissionsExt;

type Scanner = fn(&Path, &[String], Option<usize>) -> Result<Vec<AbsolutePathBuf>>;

fn scans(root: &Path, paths: Vec<FileSystemPath>) -> Vec<FileSystemDenyGlob> {
    FileSystemSandboxPolicy::restricted(
        paths
            .into_iter()
            .map(|path| FileSystemSandboxEntry::new(path, FileSystemAccessMode::Deny))
            .collect(),
    )
    .get_unreadable_glob_scans_with_cwd(root)
}

fn generic(root: &Path, suffix: &str) -> FileSystemPath {
    FileSystemPath::GlobPattern {
        pattern: format!("{}/{suffix}", root.display()),
        scan: None,
    }
}

fn require_ripgrep() {
    assert!(
        Command::new("rg")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success()),
        "this regression requires real ripgrep; a fallback-only run cannot qualify it"
    );
}

#[test]
fn database_entries_are_top_level_in_both_engines_including_escaped_roots() {
    require_ripgrep();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("database[1]*");
    std::fs::create_dir_all(root.join("project")).unwrap();
    let names = [
        "state.sqlite",
        "state.sqlite-wal",
        "state.sqlite-shm",
        "state.sqlite-journal",
    ];
    for name in names {
        std::fs::write(root.join(name), "fixture").unwrap();
    }
    std::fs::write(root.join("project/fixture.sqlite"), "project fixture").unwrap();
    let patterns = scans(
        &root,
        vec![FileSystemPath::database_entries(
            AbsolutePathBuf::from_absolute_path(&root).unwrap(),
        )],
    );
    let expected: BTreeSet<_> = names
        .into_iter()
        .map(|name| AbsolutePathBuf::from_absolute_path(root.join(name)).unwrap())
        .collect();
    for scanner in [ripgrep_files as Scanner, glob_files] {
        assert_eq!(
            expand_unreadable_globs(&patterns, &root, /*max_depth*/ None, scanner)
                .unwrap()
                .into_iter()
                .collect::<BTreeSet<_>>(),
            expected
        );
    }
}

#[test]
fn same_root_generic_globs_keep_recursive_matches_in_both_engines() {
    require_ripgrep();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    std::fs::create_dir_all(root.join("project/deep")).unwrap();
    for name in [
        "state.sqlite",
        "project/fixture.sqlite",
        "project/deep/private.env",
    ] {
        std::fs::write(root.join(name), "fixture").unwrap();
    }
    let database =
        FileSystemPath::database_entries(AbsolutePathBuf::from_absolute_path(root).unwrap());
    let patterns = scans(root, vec![database.clone(), generic(root, "**/*.env")]);
    let expected = ["state.sqlite", "project/deep/private.env"]
        .map(|name| AbsolutePathBuf::from_absolute_path(root.join(name)).unwrap())
        .into_iter()
        .collect::<BTreeSet<_>>();
    for scanner in [ripgrep_files as Scanner, glob_files] {
        assert_eq!(
            expand_unreadable_globs(&patterns, root, /*max_depth*/ None, scanner)
                .unwrap()
                .into_iter()
                .collect::<BTreeSet<_>>(),
            expected
        );
        let recursive_sqlite = scans(root, vec![database.clone(), generic(root, "**/*.sqlite*")]);
        let expected = ["state.sqlite", "project/fixture.sqlite"]
            .map(|name| AbsolutePathBuf::from_absolute_path(root.join(name)).unwrap())
            .into_iter()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            expand_unreadable_globs(&recursive_sqlite, root, /*max_depth*/ None, scanner)
                .unwrap()
                .into_iter()
                .collect::<BTreeSet<_>>(),
            expected
        );
    }
}

struct RestorePermissions {
    path: PathBuf,
    permissions: fs::Permissions,
}
impl Drop for RestorePermissions {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.path, self.permissions.clone());
    }
}

#[test]
fn database_scan_does_not_enter_unreadable_descendant_but_generic_scan_still_fails() {
    require_ripgrep();
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let unreadable = root.join("system-private");
    fs::create_dir(&unreadable).unwrap();
    fs::write(root.join("state.sqlite"), "fixture").unwrap();
    fs::write(unreadable.join("hidden.sqlite"), "fixture").unwrap();
    let _restore = RestorePermissions {
        path: unreadable.clone(),
        permissions: fs::metadata(&unreadable).unwrap().permissions(),
    };
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
    let error = fs::read_dir(&unreadable).expect_err("permission precondition: requires a non-root Linux user without DAC override; readable descendants do not qualify this regression");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    let database =
        FileSystemPath::database_entries(AbsolutePathBuf::from_absolute_path(root).unwrap());
    let bounded = scans(root, vec![database.clone()]);
    // Identical text remains a separate ordinary scan, including the fallback's
    // existing fatal behavior on an unreadable descendant.
    let mixed = scans(root, vec![database, generic(root, "*.sqlite*")]);
    assert_eq!(mixed.len(), 2);
    for scanner in [ripgrep_files as Scanner, glob_files] {
        assert_eq!(
            expand_unreadable_globs(&bounded, root, /*max_depth*/ None, scanner).unwrap(),
            vec![AbsolutePathBuf::from_absolute_path(root.join("state.sqlite")).unwrap()]
        );
        assert!(expand_unreadable_globs(&mixed, root, /*max_depth*/ None, scanner).is_err());
        assert!(
            scanner(&unreadable, &["*.sqlite*".to_string()], Some(1)).is_err(),
            "an unreadable required root must still fail"
        );
    }
}
