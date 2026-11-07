use super::*;
use pretty_assertions::assert_eq;

#[test]
fn planted_paths_match_canonically() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dunce::canonicalize(dir.path()).expect("canonical");
    let dot_codex = root.join(".codex");
    std::fs::create_dir(&dot_codex).expect("create .codex");
    std::fs::create_dir(root.join("sub")).expect("create sub");
    let planted = PlantedProjectMetadata::Known(vec![dot_codex.clone()]);
    assert_eq!(
        (
            planted.contains(&dot_codex),
            planted.contains(&root.join("sub").join("..").join(".codex")),
            planted.contains(&root.join(".agents")),
            PlantedProjectMetadata::Known(Vec::new()).contains(&dot_codex),
            PlantedProjectMetadata::Unreadable("damaged".to_string()).contains(&dot_codex),
        ),
        (true, true, false, false, true)
    );
}

/// A planted `.codex` that is a link stays planted: the folder itself is
/// never followed.
#[cfg(unix)]
#[test]
fn planted_link_is_not_followed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dunce::canonicalize(dir.path()).expect("canonical");
    std::fs::create_dir(root.join("target")).expect("create target");
    std::os::unix::fs::symlink(root.join("target"), root.join(".codex")).expect("link");
    let planted = PlantedProjectMetadata::Known(vec![root.join(".codex")]);
    assert_eq!(
        (
            planted.contains(&root.join(".codex")),
            planted.contains(&root.join("target"))
        ),
        (true, false)
    );
}

/// The same with a directory junction, which needs no privilege on Windows.
#[cfg(windows)]
#[test]
fn planted_junction_is_not_followed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dunce::canonicalize(dir.path()).expect("canonical");
    std::fs::create_dir(root.join("target")).expect("create target");
    let status = std::process::Command::new("cmd")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(root.join(".codex"))
        .arg(root.join("target"))
        .status()
        .expect("mklink");
    assert!(status.success());
    let planted = PlantedProjectMetadata::Known(vec![root.join(".codex")]);
    assert_eq!(
        (
            planted.contains(&root.join(".codex")),
            planted.contains(&root.join("target"))
        ),
        (true, false)
    );
}

/// D332: a recorded `.git` gets its own warning, and so does any when the
/// records can't be read; one not recorded gets none.
#[test]
fn planted_git_gets_a_warning() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dunce::canonicalize(dir.path()).expect("canonical");
    let dot_git = root.join(".git");
    std::fs::create_dir(&dot_git).expect("create .git");
    let warning = PlantedProjectMetadata::Known(vec![dot_git.clone()]).git_warning(&dot_git);
    assert_eq!(
        (
            warning.as_deref().map(|warning| {
                warning.starts_with(&format!(
                    "{PLANTED_GIT_WARNING_PREFIX}{}.",
                    dot_git.display()
                ))
            }),
            PlantedProjectMetadata::Known(Vec::new()).git_warning(&dot_git),
            PlantedProjectMetadata::Unreadable("damaged".to_string())
                .git_warning(&dot_git)
                .is_some_and(|warning| warning.contains("(damaged)")),
            is_git_dir(&dot_git),
            is_git_dir(&root.join(".GIT")),
            is_git_dir(&root.join(".codex")),
        ),
        (Some(true), None, true, true, true, false)
    );
}
