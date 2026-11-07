#![cfg(unix)]
use super::*;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;

/// `base/user` (the user's home, holding the link `.corbanu`) and
/// `base/dotfiles/corbanu` (the real home).
struct Layout {
    _temp: tempfile::TempDir,
    user: PathBuf,
    real: PathBuf,
    link: PathBuf,
}

fn layout() -> Layout {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let user = base.join("user");
    let real = base.join("dotfiles/corbanu");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::create_dir_all(&real).unwrap();
    let link = user.join(".corbanu");
    symlink(&real, &link).unwrap();
    Layout {
        _temp: temp,
        user,
        real,
        link,
    }
}

fn writable(root: &Path) -> FileSystemSandboxPolicy {
    FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry::new(
            FileSystemPath::Path {
                path: AbsolutePathBuf::from_absolute_path("/").unwrap(),
            },
            FileSystemAccessMode::Read,
        ),
        FileSystemSandboxEntry::new(
            FileSystemPath::Path {
                path: AbsolutePathBuf::from_absolute_path(root).unwrap(),
            },
            FileSystemAccessMode::Write,
        ),
    ])
}

fn exposed(link: &Path, folder: &Path) -> ExposedLink {
    ExposedLink {
        link: link.to_path_buf(),
        folder: folder.to_path_buf(),
    }
}

#[test]
fn issue_594_links_are_listed_also_inside_a_links_target() {
    let layout = layout();
    // `user/dots` -> `../dotfiles`; `user/.corbanu2` -> `dots/corbanu`.
    let dots = layout.user.join("dots");
    symlink("../dotfiles", &dots).unwrap();
    let second = layout.user.join(".corbanu2");
    symlink("dots/corbanu", &second).unwrap();
    assert_eq!(
        traversed_links(&second),
        vec![exposed(&second, &layout.user), exposed(&dots, &layout.user)]
    );
    assert_eq!(
        traversed_links(&layout.link),
        vec![exposed(&layout.link, &layout.user)]
    );
    assert_eq!(traversed_links(&layout.real), Vec::new());
}

#[test]
fn issue_594_a_link_in_a_writable_folder_is_exposed_and_elsewhere_not() {
    let layout = layout();
    let cwd = layout.user.clone();
    assert_eq!(
        exposed_links(&layout.link, &writable(&layout.user), &cwd),
        vec![exposed(&layout.link, &layout.user)]
    );
    assert_eq!(
        exposed_links(&layout.link, &FileSystemSandboxPolicy::unrestricted(), &cwd),
        vec![exposed(&layout.link, &layout.user)]
    );
    // Only the real home's folder is writable: the link's folder is not.
    let elsewhere = layout.real.parent().unwrap();
    assert_eq!(
        exposed_links(&layout.link, &writable(elsewhere), elsewhere),
        Vec::new()
    );
    // The real home named directly has no link.
    assert_eq!(
        exposed_links(&layout.real, &writable(&layout.user), &cwd),
        Vec::new()
    );
}

#[test]
fn issue_594_aggressive_is_exposed_only_from_a_folder_holding_the_link() {
    let layout = layout();
    assert_eq!(
        exposed_links_under_aggressive(&layout.link, /*policy*/ None, &layout.user),
        vec![exposed(&layout.link, &layout.user)]
    );
    // From a parent of the link's folder too (it is inside the workspace).
    let parent = layout.user.parent().unwrap();
    assert_eq!(
        exposed_links_under_aggressive(&layout.link, /*policy*/ None, parent),
        vec![exposed(&layout.link, &layout.user)]
    );
    let project = layout.real.parent().unwrap().join("project");
    std::fs::create_dir_all(&project).unwrap();
    assert_eq!(
        exposed_links_under_aggressive(&layout.link, /*policy*/ None, &project),
        Vec::new()
    );
}

/// The user's own folder is exposed even when it is not writable to them
/// (a command could make it writable); a folder they neither own nor can
/// write, with no such folder above it, is not.
#[test]
fn issue_594_a_read_only_folder_of_the_users_own_is_still_exposed() {
    let layout = layout();
    let original = std::fs::metadata(&layout.user).unwrap().permissions();
    std::fs::set_permissions(&layout.user, std::fs::Permissions::from_mode(0o555)).unwrap();
    let found = exposed_links(
        &layout.link,
        &FileSystemSandboxPolicy::unrestricted(),
        &layout.user,
    );
    std::fs::set_permissions(&layout.user, original).unwrap();
    assert_eq!(found, vec![exposed(&layout.link, &layout.user)]);
    // SAFETY: `geteuid` has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        assert!(!user_controls(Path::new("/")));
    }
}

/// #594 review: a writable root (or the current folder) named through a
/// link still covers the link's real folder.
#[test]
fn issue_594_a_writable_root_named_through_a_link_is_matched() {
    let layout = layout();
    let alias = layout.user.parent().unwrap().join("alias");
    symlink(&layout.user, &alias).unwrap();
    assert_eq!(
        exposed_links(&layout.link, &writable(&alias), &alias),
        vec![exposed(&layout.link, &layout.user)]
    );
    assert_eq!(
        exposed_links_under_aggressive(&layout.link, /*policy*/ None, &alias),
        vec![exposed(&layout.link, &layout.user)]
    );
    assert_eq!(
        exposed_links_under_aggressive(&layout.link, Some(&writable(&alias)), &alias),
        vec![exposed(&layout.link, &layout.user)]
    );
}

#[test]
fn issue_594_only_the_home_this_process_uses_is_checked() {
    let layout = layout();
    let all = |given: &Path| traversed_links(given);
    assert_eq!(
        check_given(&layout.link, &layout.real, all),
        Some((
            layout.real.clone(),
            vec![exposed(&layout.link, &layout.user)]
        ))
    );
    // The link spelling of the home is the same home.
    assert_eq!(
        check_given(&layout.link, &layout.link, all).map(|(real, _)| real),
        Some(layout.real.clone())
    );
    let other = layout.user.join("other");
    std::fs::create_dir_all(&other).unwrap();
    assert_eq!(check_given(&layout.link, &other, all), None);
    assert_eq!(check_given(&layout.real, &layout.real, all), None);
    assert_eq!(
        check_given(&layout.user.join("missing"), &layout.real, all),
        None
    );
}

fn sample() -> (PathBuf, Vec<ExposedLink>) {
    (
        PathBuf::from("/home/user/dotfiles/corbanu"),
        vec![exposed(
            Path::new("/home/user/.corbanu"),
            Path::new("/home/user"),
        )],
    )
}

/// The Moderate start-up warning as the chat shows it.
#[test]
fn issue_594_moderate_startup_warning_snapshot() {
    use crate::history_cell::HistoryCell;
    let (real, links) = sample();
    let cell = crate::history_cell::new_warning_event(moderate_warning(&real, &links));
    let rendered = cell
        .display_lines(/*width*/ 80)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!("issue_594_moderate_startup_warning", rendered);
}

/// Aggressive's refusal at launch, as `verify_aggressive` reports it.
#[test]
fn issue_594_aggressive_launch_refusal_snapshot() {
    let (real, links) = sample();
    let refusal =
        super::super::launch::not_fully_applied(&real, &[aggressive_refusal(&real, &links)]);
    insta::assert_snapshot!("issue_594_aggressive_launch_refusal", refusal);
}

/// On Linux, Moderate's start-up check names the link and the real folder;
/// with the real folder given, nothing is reported.
#[cfg(target_os = "linux")]
#[test]
fn issue_594_moderate_startup_warning_on_linux() {
    let layout = layout();
    let policy = writable(&layout.user);
    GIVEN_FOR_TEST.with(|given| *given.borrow_mut() = Some(layout.link.clone()));
    let warning = moderate_startup_warning(&layout.real, &policy, &layout.user);
    GIVEN_FOR_TEST.with(|given| *given.borrow_mut() = Some(layout.real.clone()));
    let fixed = moderate_startup_warning(&layout.real, &policy, &layout.user);
    assert_eq!(
        warning,
        Some(moderate_warning(
            &layout.real,
            &[exposed(&layout.link, &layout.user)]
        ))
    );
    assert_eq!(fixed, None);
}
