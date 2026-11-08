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

/// D424 (#1775): a recorded `.git` is shown when it is at or above the
/// working folder (as before), below it, or in a writable root (such as
/// `%TEMP%`); not elsewhere, and recorded `.codex`/`.agents` folders aren't
/// (Core only loads those from the working folder and above).
#[test]
fn git_dirs_to_show_cover_writable_roots_and_below_cwd() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dunce::canonicalize(dir.path()).expect("canonical");
    let project = base.join("project");
    let cwd = project.join("app");
    let temp = base.join("temp");
    for folder in [&cwd, &temp, &base.join("elsewhere")] {
        std::fs::create_dir_all(folder).expect("create folder");
    }
    let above = project.join(".git");
    let below = cwd.join("vendor").join("lib").join(".git");
    let in_temp = temp.join(".git");
    let nested_in_temp = temp.join("scratch").join(".git");
    let elsewhere = base.join("elsewhere").join(".git");
    let codex_below = cwd.join("sub").join(".codex");
    let planted = PlantedProjectMetadata::Known(vec![
        above.clone(),
        below.clone(),
        in_temp.clone(),
        nested_in_temp.clone(),
        elsewhere,
        codex_below,
    ]);
    assert_eq!(
        (
            planted.git_dirs_to_show(std::slice::from_ref(&temp), &cwd),
            planted.git_dirs_to_show(&[], &cwd),
        ),
        (
            vec![above.clone(), below.clone(), in_temp, nested_in_temp],
            vec![above, below],
        )
    );
}

/// D424: the startup warnings beyond the project are those entries'
/// warnings; with unreadable records, one for each writable root's `.git`
/// that exists.
#[test]
fn git_warnings_beyond_project_name_each_entry() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dunce::canonicalize(dir.path()).expect("canonical");
    let temp = base.join("temp");
    let empty = base.join("empty");
    std::fs::create_dir_all(temp.join(".git")).expect("create .git");
    std::fs::create_dir_all(&empty).expect("create root");
    let roots = [temp.clone(), empty];
    let known = PlantedProjectMetadata::Known(vec![temp.join(".git")])
        .git_warnings_beyond_project(&roots, &base);
    let unreadable = PlantedProjectMetadata::Unreadable("damaged".to_string())
        .git_warnings_beyond_project(&roots, &base);
    let named = |warnings: &[String]| {
        warnings
            .iter()
            .map(|warning| {
                warning.starts_with(&format!(
                    "{PLANTED_GIT_WARNING_PREFIX}{}.",
                    temp.join(".git").display()
                ))
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        (named(&known), named(&unreadable)),
        (vec![true], vec![true])
    );
}

/// D424: the notice after a command names what was recorded, by kind, at
/// most 20 of each.
#[test]
fn notice_after_a_command_names_the_recorded_folders() {
    let git: Vec<PathBuf> = (0..22)
        .map(|n| PathBuf::from(format!("/w/r{n}/.git")))
        .collect();
    let notice = planted_after_command_notice(
        &[git, vec![PathBuf::from("/w/r0/.codex")]].concat(),
        &[PlantedGitChange {
            path: PathBuf::from("/w/bare"),
            changes: vec!["config changed".to_string()],
        }],
    )
    .expect("notice");
    assert_eq!(
        (
            notice.starts_with(&format!(
                "{PLANTED_GIT_WARNING_PREFIX}/w/r0/.git, /w/r1/.git"
            )),
            notice.contains("/w/r19/.git and 3 more."),
            notice.contains(&format!(
                "\n{PLANTED_PROJECT_METADATA_WARNING_PREFIX}/w/r0/.codex."
            )),
            planted_after_command_notice(&[], &[]),
            planted_after_command_notice(
                &[],
                &[PlantedGitChange {
                    path: PathBuf::from("/w/bare"),
                    changes: (1..=7).map(|n| format!("hooks/h{n} added")).collect(),
                }],
            )
            .is_some_and(|notice| notice.starts_with(&format!(
                "{PLANTED_GIT_WARNING_PREFIX}/w/bare (hooks/h1 added, hooks/h2 added, hooks/h3 added, hooks/h4 added, hooks/h5 added and 2 more)."
            ))),
        ),
        (true, true, true, None, true)
    );
}

/// D420: a changed repository is shown where a recorded `.git` would be.
#[test]
fn git_changes_to_show_use_the_same_scope() {
    let change = |path: &str| PlantedGitChange {
        path: PathBuf::from(path),
        changes: vec!["new repository".to_string()],
    };
    let shown = git_changes_to_show(
        vec![
            change("/w/project/vendor/.git"),
            change("/w/temp/scratch"),
            change("/w/elsewhere/.git"),
        ],
        &[PathBuf::from("/w/temp")],
        Path::new("/w/project"),
    );
    assert_eq!(
        shown,
        vec![change("/w/project/vendor/.git"), change("/w/temp/scratch")]
    );
}
