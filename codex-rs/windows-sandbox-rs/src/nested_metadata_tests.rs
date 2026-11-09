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
    assert!(found.unlistable.is_empty(), "{:?}", found.unlistable);
    let found = found.entries;
    let mut paths: Vec<(PathBuf, bool)> = found
        .iter()
        .map(|entry| (entry.path.clone(), entry.identity.is_some()))
        .collect();
    paths.sort();
    let mut expected = vec![
        (root.join("a\\.codex"), true),
        // Under the name Core opens.
        (root.join("a\\b\\.agents"), true),
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
    let before = find(std::slice::from_ref(&root)).entries;
    std::fs::remove_dir(root.join("replaced\\.codex")).expect("remove");
    std::fs::create_dir(root.join("replaced\\.codex")).expect("recreate");
    std::fs::create_dir_all(root.join("new\\.agents")).expect("create new");

    let mut appeared = appeared(&before, find(std::slice::from_ref(&root)).entries);
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

/// A name NTFS may fold to `.agents` (`ſ` upper-cases to `S`) is found
/// under the name Core opens, and never matches; a folder the walk can't
/// list (here a root that is a file) is reported.
#[test]
fn find_reports_folded_names_and_unlistable_folders() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dunce::canonicalize(dir.path()).expect("canonical");
    std::fs::create_dir_all(root.join("a\\.agent\u{17f}")).expect("create folded name");
    // Walked into: only `.git` itself is skipped.
    std::fs::create_dir_all(root.join(".g\u{131}t\\b\\.codex")).expect("create .gıt");
    let not_a_folder = dir.path().join("file");
    std::fs::write(&not_a_folder, "x").expect("write file");
    let mut found = find(&[root.clone(), not_a_folder.clone()]);
    found.entries.sort_by(|a, b| a.path.cmp(&b.path));

    assert_eq!(
        (
            found
                .entries
                .iter()
                .map(|entry| (entry.path.clone(), entry.identity.is_some()))
                .collect::<Vec<_>>(),
            found.unlistable,
            folds_to(".AGENT\u{17f}", ".agents"),
            folds_to(".agentss", ".agents"),
        ),
        (
            vec![
                (root.join(".g\u{131}t\\b\\.codex"), true),
                (root.join("a\\.agents"), false),
            ],
            vec![not_a_folder],
            true,
            false,
        )
    );
}

/// #1877 item 2: a folder holding a folder whose name isn't valid Unicode
/// (not walked) is unlistable, so what is below it counts as planted; one
/// holding only such a file isn't. Either gets one note, however many such
/// names it holds.
#[test]
fn find_reports_folders_holding_names_that_are_not_unicode() {
    use std::os::windows::ffi::OsStringExt as _;
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dunce::canonicalize(dir.path()).expect("canonical");
    // Lone surrogates: valid on NTFS, not valid Unicode.
    let odd = |n: u8| std::ffi::OsString::from_wide(&[0xD800, u16::from(n)]);
    let (folders, files) = (root.join("folders"), root.join("files"));
    for n in *b"xy" {
        std::fs::create_dir_all(folders.join(odd(n)).join("a\\.codex")).expect("odd folder");
        std::fs::create_dir_all(&files).expect("files");
        std::fs::write(files.join(odd(n)), "x").expect("odd file");
    }

    let mut found = find(std::slice::from_ref(&root));
    found.notes.sort();

    assert_eq!(
        (found.entries, found.unlistable, found.notes),
        (
            Vec::new(),
            vec![folders.clone()],
            vec![files, folders]
                .into_iter()
                .map(|folder| {
                    (
                        folder,
                        "holds an entry whose name isn't valid Unicode, so it isn't checked"
                            .to_string(),
                    )
                })
                .collect::<Vec<_>>(),
        )
    );
}

/// #1877: the walk on several threads finds what one walk finds, in the same
/// order, in a branching tree deeper than [`SPLIT_DEPTH`] with every kind of
/// entry, over overlapping and repeated roots.
#[test]
fn find_matches_one_walk() {
    use std::os::windows::ffi::OsStringExt as _;
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dunce::canonicalize(dir.path()).expect("canonical");
    let outside = tempfile::tempdir().expect("outside");
    let mut path = root.clone();
    for level in 0..8 {
        for branch in 0..3 {
            let folder = path.join(format!("b{branch}"));
            std::fs::create_dir_all(folder.join("leaf")).expect("folder");
            if (level + branch) % 2 == 0 {
                std::fs::create_dir_all(folder.join(".codex")).expect(".codex");
            }
            if (level + branch) % 3 == 0 {
                std::fs::create_dir_all(folder.join(".git")).expect(".git");
            }
            if (level + branch) % 4 == 1 {
                std::fs::create_dir_all(folder.join(".agent\u{17f}")).expect("folded");
            }
            if (level + branch) % 5 == 2 {
                std::fs::create_dir_all(
                    folder.join(std::ffi::OsString::from_wide(&[0xD800, u16::from(b'x')])),
                )
                .expect("odd");
            }
            if (level + branch) % 4 == 3 {
                junction(&folder.join("link"), outside.path());
            }
        }
        path = path.join("b1");
    }
    let roots = vec![root.clone(), root.join("b1\\b1"), root];

    let parallel = find(&roots);
    let mut budget = usize::MAX;
    let one = find_bounded(&roots, &mut budget);
    let summary = |found: Found| {
        (
            found
                .entries
                .into_iter()
                .map(|entry| (entry.path, entry.identity))
                .collect::<Vec<_>>(),
            found.unlistable,
            found.repos,
            found.links,
            found.notes,
        )
    };
    let parallel = summary(parallel);
    assert!(
        parallel.0.len() > 10
            && !parallel.2.is_empty()
            && !parallel.3.is_empty()
            && !parallel.4.is_empty()
    );
    assert_eq!(parallel, summary(one));
}

/// D332: a root's top-level `.git` is reported when it exists, a folder or
/// file with its identity and a junction without one; a missing one isn't.
/// One counts as appeared in a session unless it is there, as the same
/// file, from the start; a `.git` below the top of a root isn't D332's.
#[test]
fn top_level_git_is_reported_and_compared() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = dunce::canonicalize(dir.path()).expect("canonical");
    let (none, folder, file, linked, target) = (
        base.join("none"),
        base.join("folder"),
        base.join("file"),
        base.join("linked"),
        base.join("target"),
    );
    for path in [&none, &folder, &file, &linked, &target] {
        std::fs::create_dir_all(path).expect("create");
    }
    std::fs::create_dir(folder.join(".git")).expect(".git folder");
    std::fs::write(file.join(".git"), b"gitdir: elsewhere").expect(".git file");
    junction(&linked.join(".git"), &target);
    let roots = vec![none.clone(), folder.clone(), file.clone(), linked.clone()];

    let before = top_level_git(&roots);
    let reported: Vec<(PathBuf, bool)> = before
        .iter()
        .map(|entry| (entry.path.clone(), entry.identity.is_some()))
        .collect();
    let appeared = |path: &Path| git_appeared_in_session(&roots, &before, path);
    let unchanged = (
        appeared(&none.join(".git")),
        appeared(&folder.join(".git")),
        appeared(&file.join(".git")),
        appeared(&linked.join(".git")),
        appeared(&folder.join("sub\\.git")),
        appeared(&folder.join(".codex")),
    );
    std::fs::create_dir(none.join(".git")).expect("plant");
    std::fs::remove_dir(folder.join(".git")).expect("remove");
    std::fs::create_dir(folder.join(".git")).expect("replace");
    let changed = (
        appeared(&none.join(".git")),
        appeared(&folder.join(".GIT")),
        crate::nested_metadata::appeared(&before, top_level_git(&roots)),
    );

    assert_eq!(
        (reported, unchanged, changed),
        (
            vec![
                (folder.join(".git"), true),
                (file.join(".git"), true),
                (linked.join(".git"), false),
            ],
            (true, false, false, true, false, false),
            (
                true,
                true,
                vec![none.join(".git"), folder.join(".git"), linked.join(".git")]
            ),
        )
    );
}

/// #1896 item 3: what is inside a folder is the same until something below
/// it changes; a `.codex` that is a file is its content, also stable.
#[test]
fn contents_stay_equal_until_something_inside_changes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dunce::canonicalize(dir.path()).expect("canonical");
    let folder = root.join("a\\.codex");
    std::fs::create_dir_all(folder.join("deep\\er")).expect("create");
    std::fs::write(folder.join("config.toml"), "x\n").expect("write");
    std::fs::write(folder.join("deep\\er\\p.md"), "p\n").expect("write");
    std::fs::create_dir_all(root.join("b")).expect("create");
    std::fs::write(root.join("b\\.codex"), "file\n").expect("write");

    let before = contents(&folder);
    let file_before = contents(&root.join("b\\.codex"));
    let unchanged = (
        contents(&folder) == before,
        contents(&root.join("b\\.codex")) == file_before,
    );
    std::fs::remove_file(folder.join("deep\\er\\p.md")).expect("delete");
    let deleted = contents(&folder) != before;
    std::fs::write(folder.join("deep\\er\\p.md"), "p\n").expect("restore");
    let restored = contents(&folder) == before;
    std::fs::write(folder.join("deep\\er\\p.md"), "q\n").expect("change");
    let changed = contents(&folder) != before;

    assert_eq!(
        (unchanged, deleted, restored, changed),
        ((true, true), true, true, true)
    );
}

/// #1909: a file inside a `.codex` folder swapped for a link after it was
/// looked at is never read through.
#[test]
fn contents_do_not_read_a_link_swapped_in() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dunce::canonicalize(dir.path()).expect("canonical");
    if !crate::git_watch::tests::symlinks_allowed(&root) {
        return;
    }
    let folder = root.join("a\\.codex");
    std::fs::create_dir_all(&folder).expect("create");
    std::fs::write(folder.join("p.md"), "p\n").expect("write");
    let secret = root.join("secret");
    std::fs::write(&secret, "secret\n").expect("write");
    let secret_print = contents(&secret);

    let prints = crate::git_watch::tests::with_before_read(
        move |path| {
            if path.ends_with("p.md") {
                crate::git_watch::tests::swap_for_link(path, &secret);
            }
        },
        || contents(&folder),
    );

    assert_eq!(
        (
            prints.get("p.md").map(String::as_str),
            prints.values().any(|print| Some(print) == secret_print.get("")),
        ),
        (Some("unreadable"), false)
    );
}

/// #1909 (#1902 item 5): a junction inside a `.codex` folder is recorded by
/// its target, and nothing behind it is read.
#[test]
fn contents_record_a_junction_by_its_target() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dunce::canonicalize(dir.path()).expect("canonical");
    let folder = root.join("a\\.codex");
    std::fs::create_dir_all(&folder).expect("create");
    let outside = root.join("outside");
    std::fs::create_dir_all(&outside).expect("create");
    std::fs::write(outside.join("s.md"), "s\n").expect("write");
    junction(&folder.join("j"), &outside);

    let prints = contents(&folder)
        .into_iter()
        .map(|(name, print)| (name, print.replace(r"\\?\", "")))
        .collect::<Vec<_>>();

    assert_eq!(
        prints,
        vec![("j".to_string(), format!("link to {}", outside.display()))]
    );
}

/// #1896 item 3: a legacy entry is written without `contents`, and a record
/// from before it reads back as one.
#[test]
fn entries_without_contents_serialize_as_before() {
    let entry = NestedMetadata {
        path: PathBuf::from("C:\\w\\a\\.codex"),
        identity: None,
        contents: None,
    };
    let json = serde_json::to_string(&entry).expect("serialize");
    let read: NestedMetadata =
        serde_json::from_str(r#"{"path":"C:\\w\\a\\.codex","identity":null}"#)
            .expect("deserialize");
    assert_eq!(
        (json.as_str(), read),
        (r#"{"path":"C:\\w\\a\\.codex","identity":null}"#, entry)
    );
}
