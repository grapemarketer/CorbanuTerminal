use super::*;
use pretty_assertions::assert_eq;
use std::fs;

const INIT_CONFIG: &str = "[core]\n\trepositoryformatversion = 0\n\tfilemode = false\n\tbare = false\n\tlogallrefupdates = true\n\tsymlinks = false\n\tignorecase = true\n";
const CLONE_CONFIG: &str = "[core]\n\trepositoryformatversion = 0\n\tfilemode = false\n\tbare = false\n\tlogallrefupdates = true\n\tsymlinks = false\n\tignorecase = true\n[remote \"origin\"]\n\turl = https://example.com/a.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n";

/// The shape `git init` leaves in `dir` (Git for Windows).
fn init_shape(dir: &Path, config: &str) {
    for folder in [
        "hooks",
        "info",
        "objects/info",
        "objects/pack",
        "refs/heads",
        "refs/tags",
    ] {
        fs::create_dir_all(dir.join(folder)).expect("create folder");
    }
    fs::write(dir.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    fs::write(dir.join("config"), config).expect("config");
    fs::write(dir.join("description"), "Unnamed repository\n").expect("description");
    fs::write(dir.join("info/exclude"), "# git ls-files\n").expect("exclude");
    for hook in [
        "pre-commit.sample",
        "post-update.sample",
        "fsmonitor-watchman.sample",
    ] {
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
            plain(
                "fsmonitor",
                &format!("{INIT_CONFIG}\tfsmonitor = evil.exe\n"),
                &none
            ),
            plain(
                "include",
                &format!("{INIT_CONFIG}[include]\n\tpath = x\n"),
                &none
            ),
            plain(
                "ext",
                &CLONE_CONFIG.replace("https://example.com/a.git", "ext::sh -c evil"),
                &none,
            ),
            plain("same-line", "[core] fsmonitor = evil.exe\n", &none),
            plain("continued", "[core]\n\tbare = \\\nfalse\n", &none),
            plain("commondir", INIT_CONFIG, &write("commondir", "..")),
            plain(
                "rebase",
                INIT_CONFIG,
                &write("rebase-merge/git-rebase-todo", "exec evil")
            ),
            plain(
                "module",
                INIT_CONFIG,
                &write("modules/m/HEAD", "ref: refs/heads/main")
            ),
            plain("remotes", INIT_CONFIG, &write("remotes/origin", "URL: x")),
            plain(
                "worktree-config",
                INIT_CONFIG,
                &write("config.worktree", "")
            ),
            is_plain(&gitdir_file),
        ],
        [
            true, true, true, false, false, false, false, false, false, false, false, false, false,
            false, false,
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
    std::os::unix::fs::symlink(base.join("elsewhere"), git.join("COMMIT_EDITMSG")).expect("link");
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
    fs::write(
        split.join("HEAD"),
        "0123456789abcdef0123456789abcdef01234567\n",
    )
    .expect("HEAD");
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

/// Git's own parsing (#1849 review): a detached `HEAD` with anything after
/// 40 hex digits is valid, and `commondir`/`gitdir:` values keep their
/// leading blanks (only trailing line ends go).
#[test]
fn heads_and_paths_parse_like_git() {
    let (_dir, base) = tempdir();
    let junk_head = base.join("junk-head");
    init_shape(&junk_head, INIT_CONFIG);
    fs::write(
        junk_head.join("HEAD"),
        "0123456789abcdef0123456789abcdef01234567 trailing junk",
    )
    .expect("HEAD");
    // ` c` (leading blank) is the folder git uses.
    let spaced = base.join("spaced");
    init_shape(&spaced.join(" c"), INIT_CONFIG);
    fs::write(spaced.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    fs::write(spaced.join("commondir"), " c\r\n").expect("commondir");
    let file = base.join("checkout").join(".git");
    fs::create_dir_all(file.parent().expect("parent")).expect("checkout");
    fs::write(&file, "gitdir:  x\n").expect(".git file");
    let no_space = base.join("other").join(".git");
    fs::create_dir_all(no_space.parent().expect("parent")).expect("other");
    fs::write(&no_space, "gitdir:x\n").expect(".git file");
    assert_eq!(
        (
            is_git_directory(&junk_head),
            is_git_directory(&spaced),
            gitdir_target(&file),
            gitdir_target(&no_space),
        ),
        (true, true, Some(base.join("checkout").join(" x")), None)
    );
}

/// #1849 review: an empty module folder added, a hard-linked config and a
/// hooks folder turned into a link all count.
#[cfg(unix)]
#[test]
fn links_and_empty_modules_count() {
    let (_dir, base) = tempdir();
    let roots = [base.clone()];
    let git = base.join("repo").join(".git");
    init_shape(&git, INIT_CONFIG);
    let before = fingerprint(&git, &roots);
    fs::create_dir_all(git.join("modules").join("aaa")).expect("empty module");
    let module = changes(&before, &fingerprint(&git, &roots));
    let before = fingerprint(&git, &roots);
    fs::hard_link(git.join("config"), base.join("elsewhere-config")).expect("hard link");
    let hard = changes(&before, &fingerprint(&git, &roots));
    let before = fingerprint(&git, &roots);
    fs::rename(git.join("hooks"), base.join("hooks-real")).expect("move hooks");
    std::os::unix::fs::symlink(base.join("hooks-real"), git.join("hooks")).expect("link");
    let linked = changes(&before, &fingerprint(&git, &roots));
    let before = fingerprint(&git, &roots);
    std::os::unix::fs::symlink(base.join("msg"), git.join("COMMIT_EDITMSG")).expect("link");
    let written = changes(&before, &fingerprint(&git, &roots));
    assert_eq!(
        (
            module,
            hard,
            linked.contains(&"hooks/ added".to_string()),
            written
        ),
        (
            vec!["modules/aaa/ added".to_string()],
            vec!["config changed".to_string()],
            true,
            vec!["COMMIT_EDITMSG added".to_string()],
        )
    );
}

/// #1849 review 2: a module named with a slash (`modules\vendor\foo`) is
/// watched, a `commondir` loop is looked at once, and a quoted `ext::` URL
/// isn't plain.
#[test]
fn slashed_modules_loops_and_quoted_urls() {
    let (_dir, base) = tempdir();
    let roots = [base.clone()];
    let git = base.join("repo").join(".git");
    init_shape(&git, INIT_CONFIG);
    let module = git.join("modules").join("vendor").join("foo");
    init_shape(&module, INIT_CONFIG);
    // Modules whose `commondir` leads back to the repository.
    for n in 0..50 {
        let looped = git.join("modules").join(format!("loop{n}"));
        fs::create_dir_all(&looped).expect("loop module");
        fs::write(looped.join("commondir"), "../..").expect("commondir");
    }
    let before = fingerprint(&git, &roots);
    fs::write(module.join("hooks").join("post-checkout"), "evil").expect("hook");
    let changed = changes(&before, &fingerprint(&git, &roots));
    assert_eq!(
        (
            changed,
            before.len() < 2_000,
            plain_config(&CLONE_CONFIG.replace("https://example.com/a.git", "\"e\"xt::sh -c evil")),
        ),
        (
            vec!["modules/vendor/foo/hooks/post-checkout added".to_string()],
            true,
            false
        )
    );
}

/// #1849 review 3: a module whose name has a segment git uses for its own
/// folders (`vendor/refs`) is watched when its parent isn't a git folder,
/// and a module path too deep to look through counts as changed.
#[test]
fn modules_named_like_git_folders_and_too_deep() {
    let (_dir, base) = tempdir();
    let roots = [base.clone()];
    let git = base.join("repo").join(".git");
    init_shape(&git, INIT_CONFIG);
    let module = git.join("modules").join("vendor").join("refs");
    init_shape(&module, INIT_CONFIG);
    let before = fingerprint(&git, &roots);
    fs::write(module.join("hooks").join("post-checkout"), "evil").expect("hook");
    let changed = changes(&before, &fingerprint(&git, &roots));

    let deep = base.join("deep").join(".git");
    init_shape(&deep, INIT_CONFIG);
    let mut path = deep.join("modules");
    for segment in 0..8 {
        path = path.join(format!("s{segment}"));
    }
    fs::create_dir_all(&path).expect("deep module");
    let deep_before = fingerprint(&deep, &roots);
    let deep_again = changes(&deep_before, &fingerprint(&deep, &roots));
    assert_eq!(
        (
            changed,
            deep_again
                .iter()
                .any(|change| change.ends_with("(more) changed"))
        ),
        (
            vec!["modules/vendor/refs/hooks/post-checkout added".to_string()],
            true
        )
    );
}
