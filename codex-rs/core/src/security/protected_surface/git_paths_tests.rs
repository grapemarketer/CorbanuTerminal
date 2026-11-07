#![cfg(unix)]

use super::*;
use pretty_assertions::assert_eq;

fn abs(path: &Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path(path).unwrap()
}

#[test]
fn pf_23_s02_hooks_path_resolves_like_git() {
    let dir = tempfile::tempdir().unwrap();
    let base = abs(&dir.path().canonicalize().unwrap());
    let home = base.join("home");
    let config = base.join("config");
    let read = |text: &str| {
        std::fs::write(config.as_path(), text).unwrap();
        hooks_path(&config, &base, Some(&home))
    };
    assert_eq!(
        read("[core]\n  hookspath = tools/hooks\n"),
        Some(base.join("tools/hooks"))
    );
    assert_eq!(
        read("[core]\nhooksPath = /opt/hooks\n"),
        Some(abs(Path::new("/opt/hooks")))
    );
    assert_eq!(
        read("[core] hooksPath = \".husky\" # managed by husky\n"),
        Some(base.join(".husky"))
    );
    assert_eq!(
        read("[core] # comment\n\thooksPath = a ; old\n[CORE]\nhooksPath = b\n"),
        Some(base.join("b"))
    );
    assert_eq!(
        read("[core]\nhooksPath = ~/.githooks\n"),
        Some(home.join(".githooks"))
    );
    assert_eq!(read("[user]\nhooksPath = x\n"), None);
    assert_eq!(read("[core \"x\"]\nhooksPath = x\n"), None);
    assert_eq!(read("[core]\nhooksPath = ~other/hooks\n"), None);
}

/// Every folder below `.git/modules` has its entries protected, without
/// following links; past the limits the whole `modules` folder is.
#[test]
fn pf_23_s02_module_walk_is_bounded_and_does_not_follow_links() {
    let dir = tempfile::tempdir().unwrap();
    let git = abs(&dir.path().canonicalize().unwrap()).join(".git");
    for module in ["modules/a", "modules/libs/b"] {
        for entry in ["objects", "hooks"] {
            std::fs::create_dir_all(git.join(module).join(entry)).unwrap();
        }
        std::fs::write(git.join(module).join("config"), "").unwrap();
    }
    // A link is never followed (`/` would walk the whole disk).
    std::os::unix::fs::symlink("/", git.join("modules/root-link").as_path()).unwrap();
    // No HEAD anywhere: entries are protected whether or not one exists.
    let paths = module_paths(&git);
    for protected in [
        "modules/a/hooks",
        "modules/a/config",
        "modules/libs/b/hooks",
    ] {
        assert!(
            paths.contains(&git.join(protected)),
            "{protected}: {paths:?}"
        );
    }
    assert!(
        paths
            .iter()
            .all(|path| !path.as_path().starts_with(git.join("modules/root-link"))),
        "{paths:?}"
    );
    assert!(
        paths
            .iter()
            .all(|path| !path.as_path().starts_with(git.join("modules/a/objects"))),
        "{paths:?}"
    );

    // A fake `objects` in an intermediate folder hides nothing below it.
    std::fs::create_dir_all(git.join("modules/libs/objects").as_path()).unwrap();
    assert!(module_paths(&git).contains(&git.join("modules/libs/b/hooks")));
    // A submodule named like a git folder is still walked.
    std::fs::create_dir_all(git.join("modules/hooks/hooks").as_path()).unwrap();
    assert!(module_paths(&git).contains(&git.join("modules/hooks/hooks")));

    // A folder that cannot be read closes the whole of `modules`.
    {
        use std::os::unix::fs::PermissionsExt;
        let libs = git.join("modules/libs");
        std::fs::set_permissions(libs.as_path(), std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads it anyway; there is nothing to check then.
        let unreadable = std::fs::read_dir(libs.as_path()).is_err();
        let paths = module_paths(&git);
        std::fs::set_permissions(libs.as_path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        if unreadable {
            assert_eq!(paths, vec![git.join("modules")]);
        }
    }

    for n in 0..=MAX_MODULE_DIRS {
        std::fs::create_dir_all(git.join(format!("modules/fake/f{n}")).as_path()).unwrap();
    }
    assert_eq!(module_paths(&git), vec![git.join("modules")]);
}

/// An unreadable workspace folder or `.git` hides nothing: the `.git` becomes
/// read-only whole, and what this process found before (the repository's
/// and the home's `hooksPath`, a worktree's main git folder) still holds.
#[test]
fn pf_23_s02_unreadable_repository_fails_closed() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let base = abs(&dir.path().canonicalize().unwrap());
    let home = base.join("home");
    std::fs::create_dir_all(home.join(".githooks").as_path()).unwrap();
    std::fs::write(
        home.join(".gitconfig").as_path(),
        "[core]\n\thooksPath = ~/.githooks\n",
    )
    .unwrap();
    let main = base.join("main/.git");
    std::fs::create_dir_all(main.join("hooks").as_path()).unwrap();
    std::fs::create_dir_all(main.join("worktrees/w").as_path()).unwrap();
    std::fs::write(main.join("worktrees/w/commondir").as_path(), "../..\n").unwrap();
    std::fs::write(
        main.join("config").as_path(),
        "[core]\n\thooksPath = .husky\n",
    )
    .unwrap();
    let repo = base.join("repo");
    std::fs::create_dir_all(repo.join(".husky").as_path()).unwrap();
    std::fs::write(
        repo.join(".git").as_path(),
        format!("gitdir: {}\n", main.join("worktrees/w").display()),
    )
    .unwrap();
    let cwd = repo.join("src");
    std::fs::create_dir_all(cwd.as_path()).unwrap();
    let expected = [
        repo.join(".husky"),
        home.join(".githooks"),
        main.join("hooks"),
        main.join("config"),
    ];
    let paths = git_persistence_paths(&cwd, Some(home.as_path()));
    for path in &expected {
        assert!(paths.contains(path), "{}: {paths:?}", path.display());
    }

    let lock = |path: &AbsolutePathBuf, mode| {
        std::fs::set_permissions(path.as_path(), std::fs::Permissions::from_mode(mode)).unwrap()
    };
    for locked in [&main, &repo] {
        lock(locked, 0o000);
        let unreadable = std::fs::read_dir(locked.as_path()).is_err();
        let paths = git_persistence_paths(&cwd, Some(home.as_path()));
        lock(locked, 0o755);
        if unreadable {
            for path in &expected {
                assert!(paths.contains(path), "{}: {paths:?}", path.display());
            }
        }
    }
}

/// #650: a worktree's repository paths include what git reads from every
/// linked worktree's folder in the common one, also the other worktrees';
/// past the limit, or when the folder cannot be read, `worktrees` whole.
#[test]
fn issue_650_other_worktrees_folders_are_protected() {
    let dir = tempfile::tempdir().unwrap();
    let base = abs(&dir.path().canonicalize().unwrap());
    let common = base.join("main/.git");
    for name in ["w", "other"] {
        let admin = common.join("worktrees").join(name);
        std::fs::create_dir_all(admin.as_path()).unwrap();
        std::fs::write(admin.join("commondir").as_path(), "../..\n").unwrap();
        std::fs::write(admin.join("gitdir").as_path(), "x\n").unwrap();
    }
    let repo = base.join("repo");
    std::fs::create_dir_all(repo.as_path()).unwrap();
    std::fs::write(
        repo.join(".git").as_path(),
        format!("gitdir: {}\n", common.join("worktrees/w").display()),
    )
    .unwrap();
    let paths = git_persistence_paths(&repo, /*user_home*/ None);
    for protected in [
        "worktrees/other/commondir",
        "worktrees/other/config.worktree",
        "worktrees/other/gitdir",
        "worktrees/w/commondir",
    ] {
        assert!(
            paths.contains(&common.join(protected)),
            "{protected}: {paths:?}"
        );
    }
    // The main worktree finds them the same way.
    std::fs::create_dir_all(base.join("main/src").as_path()).unwrap();
    let paths = git_persistence_paths(&base.join("main/src"), /*user_home*/ None);
    assert!(
        paths.contains(&common.join("worktrees/other/commondir")),
        "{paths:?}"
    );

    for n in 0..=MAX_MODULE_DIRS {
        std::fs::create_dir_all(common.join(format!("worktrees/f{n}")).as_path()).unwrap();
    }
    assert_eq!(
        worktree_admin_paths(&common),
        vec![common.join("worktrees")]
    );
}

/// #675: the other linked worktrees' own `.git` files, as their admin
/// folders' `gitdir` names them (absolute, or relative to the admin folder),
/// are protected too, from a linked worktree and from the main one. A
/// `gitdir` naming a folder, another file name or nothing there adds
/// nothing, and one that is a FIFO or too long is not read (or waited on).
#[test]
fn issue_675_other_worktrees_dot_git_files_are_protected() {
    let dir = tempfile::tempdir().unwrap();
    let base = abs(&dir.path().canonicalize().unwrap());
    let common = base.join("main/.git");
    let gitdirs = [
        ("w", format!("{}\n", base.join("w/.git").display())),
        ("other", format!("{}\n", base.join("other/.git").display())),
        ("relative", "../../../../relative/.git\n".to_string()),
        ("folder", format!("{}\n", common.display())),
        (
            "renamed",
            format!("{}\n", base.join("renamed/git").display()),
        ),
        ("gone", format!("{}\n", base.join("gone/.git").display())),
        (
            "long",
            format!("{}{}\n", "/".repeat(5000), base.join("long/.git").display()),
        ),
    ];
    for (name, gitdir) in &gitdirs {
        let admin = common.join("worktrees").join(name);
        std::fs::create_dir_all(admin.as_path()).unwrap();
        std::fs::write(admin.join("commondir").as_path(), "../..\n").unwrap();
        std::fs::write(admin.join("gitdir").as_path(), gitdir).unwrap();
        std::fs::create_dir_all(base.join(name).as_path()).unwrap();
        let file = if *name == "renamed" { "git" } else { ".git" };
        if *name != "gone" {
            std::fs::write(
                base.join(name).join(file).as_path(),
                format!("gitdir: {}\n", admin.display()),
            )
            .unwrap();
        }
    }
    let fifo = common.join("worktrees/fifo");
    std::fs::create_dir_all(fifo.as_path()).unwrap();
    let fifo_gitdir =
        std::ffi::CString::new(fifo.join("gitdir").to_string_lossy().as_bytes()).unwrap();
    // SAFETY: a NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(fifo_gitdir.as_ptr(), 0o600) }, 0);

    let protected = [base.join("other/.git"), base.join("relative/.git")];
    let unprotected = [
        base.join("renamed/git"),
        base.join("gone/.git"),
        base.join("long/.git"),
        common.clone(),
    ];
    for root in [base.join("w"), base.join("main")] {
        let paths = git_persistence_paths(&root, /*user_home*/ None);
        for path in &protected {
            assert!(paths.contains(path), "{}: {paths:?}", path.display());
        }
        for path in &unprotected {
            assert!(!paths.contains(path), "{}: {paths:?}", path.display());
        }
    }

    // A relative `gitdir` is relative to the admin folder's real location,
    // also when the repository folder is reached through a link elsewhere.
    std::fs::create_dir_all(base.join("deep").as_path()).unwrap();
    std::os::unix::fs::symlink(
        base.join("main").as_path(),
        base.join("deep/alias").as_path(),
    )
    .unwrap();
    let paths = worktree_admin_paths(&base.join("deep/alias/.git"));
    assert!(paths.contains(&base.join("relative/.git")), "{paths:?}");

    // Past the limit: `worktrees` whole, and the `.git` files found so far.
    // #1468: every extra admin folder names a `.git` file, so whatever the
    // listing order, all but the few visited earlier are found and kept.
    let mut extra = Vec::new();
    for n in 0..=MAX_MODULE_DIRS {
        let admin = common.join(format!("worktrees/f{n}"));
        let dot_git = base.join(format!("f{n}/.git"));
        std::fs::create_dir_all(admin.as_path()).unwrap();
        std::fs::create_dir_all(base.join(format!("f{n}")).as_path()).unwrap();
        std::fs::write(dot_git.as_path(), format!("gitdir: {}\n", admin.display())).unwrap();
        std::fs::write(
            admin.join("gitdir").as_path(),
            format!("{}\n", dot_git.display()),
        )
        .unwrap();
        extra.push(dot_git);
    }
    let paths = worktree_admin_paths(&common);
    assert_eq!(paths.first(), Some(&common.join("worktrees")));
    let found = [&protected[..], &[base.join("w/.git")], &extra[..]].concat();
    assert!(
        paths[1..].iter().all(|path| found.contains(path)),
        "{paths:?}"
    );
    // At most the 8 earlier admin folders took a visit before the limit.
    assert!(
        paths[1..]
            .iter()
            .filter(|path| extra.contains(path))
            .count()
            >= MAX_MODULE_DIRS - 8,
        "{} found",
        paths.len() - 1
    );
}

/// #1468: a submodule's linked worktrees (`modules/m/worktrees/n`) get the
/// same protection as the repository's own (#650, #675): the admin folder's
/// `gitdir` and the `.git` file it names. A `gitdir` spelled `.GIT` names
/// that file too, where the volume ignores case. Past the walk's limit the
/// `.git` files found are kept with `modules`.
#[test]
fn issue_1468_submodule_worktrees_dot_git_files_are_protected() {
    let dir = tempfile::tempdir().unwrap();
    let base = abs(&dir.path().canonicalize().unwrap());
    let git = base.join("main/.git");
    let admin = git.join("modules/m/worktrees/n");
    std::fs::create_dir_all(admin.as_path()).unwrap();
    std::fs::write(admin.join("commondir").as_path(), "../..\n").unwrap();
    let dot_git = base.join("n/.git");
    std::fs::create_dir_all(base.join("n").as_path()).unwrap();
    std::fs::write(dot_git.as_path(), format!("gitdir: {}\n", admin.display())).unwrap();
    std::fs::write(
        admin.join("gitdir").as_path(),
        format!("{}\n", dot_git.display()),
    )
    .unwrap();

    let paths = module_paths(&git);
    for path in [
        admin.join("gitdir"),
        admin.join("commondir"),
        admin.join("config.worktree"),
        dot_git.clone(),
    ] {
        if cfg!(target_os = "linux") && path.as_path().ends_with("config.worktree") {
            continue;
        }
        assert!(paths.contains(&path), "{}: {paths:?}", path.display());
    }
    // Through the repository's own paths too.
    let paths = git_persistence_paths(&base.join("main"), /*user_home*/ None);
    assert!(paths.contains(&dot_git), "{paths:?}");

    // `.GIT`: the same file where the volume ignores case.
    std::fs::write(
        admin.join("gitdir").as_path(),
        format!("{}\n", base.join("n/.GIT").display()),
    )
    .unwrap();
    let case_blind = std::fs::symlink_metadata(base.join("n/.GIT").as_path()).is_ok();
    let paths = module_paths(&git);
    assert_eq!(
        paths.contains(&base.join("n/.GIT")),
        case_blind,
        "{paths:?}"
    );

    // Past the limit: `modules` whole, with the `.git` file found before
    // (the extra folders are below the admin folder, so it comes first).
    std::fs::write(
        admin.join("gitdir").as_path(),
        format!("{}\n", dot_git.display()),
    )
    .unwrap();
    for n in 0..=MAX_MODULE_DIRS {
        std::fs::create_dir_all(admin.join(format!("x/f{n}")).as_path()).unwrap();
    }
    assert_eq!(module_paths(&git), vec![git.join("modules"), dot_git]);
}
