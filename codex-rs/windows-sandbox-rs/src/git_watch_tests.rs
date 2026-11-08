use super::*;
use pretty_assertions::assert_eq;
use std::fs;

const INIT_CONFIG: &str = "[core]\n\trepositoryformatversion = 0\n\tfilemode = false\n\tbare = false\n\tlogallrefupdates = true\n\tsymlinks = false\n\tignorecase = true\n";
const CLONE_CONFIG: &str = "[core]\n\trepositoryformatversion = 0\n\tfilemode = false\n\tbare = false\n\tlogallrefupdates = true\n\tsymlinks = false\n\tignorecase = true\n[remote \"origin\"]\n\turl = https://example.com/a.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n";

/// The shape `git init` leaves in `dir` (Git for Windows).
fn init_shape(dir: &Path, config: &str) {
    for folder in ["hooks", "info", "objects/info", "objects/pack", "refs/heads", "refs/tags"] {
        fs::create_dir_all(dir.join(folder)).expect("create folder");
    }
    fs::write(dir.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    fs::write(dir.join("config"), config).expect("config");
    fs::write(dir.join("description"), "Unnamed repository\n").expect("description");
    fs::write(dir.join("info/exclude"), "# git ls-files\n").expect("exclude");
    for hook in ["pre-commit.sample", "post-update.sample", "fsmonitor-watchman.sample"] {
        fs::write(dir.join("hooks").join(hook), "#!/bin/sh\n").expect("hook");
    }
}

fn tempdir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dunce::canonicalize(dir.path()).expect("canonical");
    (dir, path)
}

/// D421: `git init` and `git clone` shapes are plain; any hook, include,
/// unknown key, `ext::` URL, `commondir`, rebase state, submodule or
/// legacy remote isn't, and neither is a `.git` file.
#[test]
fn only_init_and_clone_shapes_are_plain() {
    let (_dir, base) = tempdir();
    let plain = |name: &str, config: &str, extra: &dyn Fn(&Path)| {
        let git = base.join(name).join(".git");
        init_shape(&git, config);
        extra(&git);
        is_plain(&git)
    };
    let write = |relative: &'static str, text: &'static str| {
        move |git: &Path| {
            let path = git.join(relative);
            fs::create_dir_all(path.parent().expect("parent")).expect("parent folder");
            fs::write(path, text).expect("write");
        }
    };
    let none = |_: &Path| {};
    let gitdir_file = base.join("file").join(".git");
    fs::create_dir_all(gitdir_file.parent().expect("parent")).expect("folder");
    fs::write(&gitdir_file, "gitdir: ../x\n").expect("gitdir file");

    assert_eq!(
        [
            plain("init", INIT_CONFIG, &none),
            plain("clone", CLONE_CONFIG, &none),
            plain("empty-branches", INIT_CONFIG, &|git: &Path| {
                fs::create_dir(git.join("branches")).expect("branches");
            }),
            plain("hook", INIT_CONFIG, &write("hooks/post-checkout", "evil")),
            plain("fsmonitor", &format!("{INIT_CONFIG}\tfsmonitor = evil.exe\n"), &none),
            plain("include", &format!("{INIT_CONFIG}[include]\n\tpath = x\n"), &none),
            plain(
                "ext",
                &CLONE_CONFIG.replace("https://example.com/a.git", "ext::sh -c evil"),
                &none,
            ),
            plain("same-line", "[core] fsmonitor = evil.exe\n", &none),
            plain("continued", "[core]\n\tbare = \\\nfalse\n", &none),
            plain("commondir", INIT_CONFIG, &write("commondir", "..")),
            plain("rebase", INIT_CONFIG, &write("rebase-merge/git-rebase-todo", "exec evil")),
            plain("module", INIT_CONFIG, &write("modules/m/HEAD", "ref: refs/heads/main")),
            plain("remotes", INIT_CONFIG, &write("remotes/origin", "URL: x")),
            plain("worktree-config", INIT_CONFIG, &write("config.worktree", "")),
            is_plain(&gitdir_file),
        ],
        [
            true, true, true, false, false, false, false, false, false, false, false, false,
            false, false, false,
        ]
    );
}

/// D421: a link anywhere inside makes a repository not plain.
#[cfg(unix)]
#[test]
fn a_link_inside_is_not_plain() {
    let (_dir, base) = tempdir();
    let git = base.join(".git");
    init_shape(&git, INIT_CONFIG);
    std::os::unix::fs::symlink(base.join("elsewhere"), git.join("COMMIT_EDITMSG"))
        .expect("link");
    assert!(!is_plain(&git));
}

/// Git's own rule for a git folder, `commondir` included.
#[test]
fn git_directories_follow_gits_rule() {
    let (_dir, base) = tempdir();
    let bare = base.join("bare");
    init_shape(&bare, INIT_CONFIG);
    let split = base.join("split");
    fs::create_dir_all(&split).expect("split");
    fs::write(split.join("HEAD"), "0123456789abcdef0123456789abcdef01234567\n").expect("HEAD");
    fs::write(split.join("commondir"), "../bare\n").expect("commondir");
    let no_refs = base.join("no-refs");
    fs::create_dir_all(no_refs.join("objects")).expect("objects");
    fs::write(no_refs.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    let bad_head = base.join("bad-head");
    init_shape(&bad_head, INIT_CONFIG);
    fs::write(bad_head.join("HEAD"), "not a ref\n").expect("HEAD");
    assert_eq!(
        [
            is_git_directory(&bare),
            is_git_directory(&split),
            is_git_directory(&no_refs),
            is_git_directory(&bad_head),
        ],
        [true, true, false, false]
    );
}

/// D420: a fingerprint changes with what git would run (by content), not
/// with a rewrite to the same content or git's bookkeeping; a `.git` file's
/// `gitdir:` target and its `commondir` are followed inside the roots, and
/// submodules' config and hooks count.
#[test]
fn fingerprints_track_what_git_would_run() {
    let (_dir, base) = tempdir();
    let roots = [base.clone()];
    let git = base.join("repo").join(".git");
    init_shape(&git, INIT_CONFIG);
    init_shape(&git.join("modules").join("sub"), INIT_CONFIG);
    let before = fingerprint(&git, &roots);
    // Bookkeeping and a same-content rewrite.
    fs::write(git.join("HEAD"), "ref: refs/heads/other\n").expect("HEAD");
    fs::write(git.join("index"), "index").expect("index");
    fs::write(git.join("config"), INIT_CONFIG).expect("same config");
    let unchanged = changes(&before, &fingerprint(&git, &roots));
    fs::write(git.join("hooks/post-checkout"), "evil").expect("hook");
    fs::write(
        git.join("modules/sub/config"),
        format!("{INIT_CONFIG}\tfsmonitor = evil.exe\n"),
    )
    .expect("module config");
    fs::remove_file(git.join("hooks/pre-commit.sample")).expect("remove sample");
    let changed = changes(&before, &fingerprint(&git, &roots));

    // A worktree's `.git` file, its git folder and the shared one.
    let main = base.join("main").join(".git");
    init_shape(&main, INIT_CONFIG);
    let worktree_git = main.join("worktrees").join("wt");
    fs::create_dir_all(&worktree_git).expect("worktree git folder");
    fs::write(worktree_git.join("commondir"), "../..\n").expect("commondir");
    let checkout = base.join("wt");
    fs::create_dir_all(&checkout).expect("checkout");
    fs::write(
        checkout.join(".git"),
        format!("gitdir: {}\n", worktree_git.display()),
    )
    .expect(".git file");
    let file_before = fingerprint(&checkout.join(".git"), &roots);
    fs::write(main.join("hooks/pre-push"), "evil").expect("shared hook");
    let file_changed = changes(&file_before, &fingerprint(&checkout.join(".git"), &roots));
    // Outside the roots, the target isn't looked into.
    let outside = fingerprint(&checkout.join(".git"), &[base.join("elsewhere")]);

    assert_eq!(
        (
            unchanged,
            changed,
            file_changed,
            outside.keys().cloned().collect::<Vec<_>>()
        ),
        (
            Vec::<String>::new(),
            vec![
                "hooks/post-checkout added".to_string(),
                "modules/sub/config changed".to_string(),
                "hooks/pre-commit.sample removed".to_string(),
            ],
            vec!["gitdir/commondir/hooks/pre-push added".to_string()],
            vec![".git".to_string()],
        )
    );
}
