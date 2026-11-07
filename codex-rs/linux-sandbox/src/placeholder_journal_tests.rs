use super::*;
use pretty_assertions::assert_eq;
use std::os::unix::fs::PermissionsExt;

struct Setup {
    _dir: tempfile::TempDir,
    base: PathBuf,
    journal: Journal,
}

impl Setup {
    fn new() -> Self {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let base = dir.path().canonicalize().expect("canonical");
        std::fs::create_dir(base.join("home")).expect("home");
        std::fs::create_dir(base.join("corbanu")).expect("corbanu home");
        let journal =
            open_journal(&base.join("corbanu/sandbox-placeholders")).expect("record folder");
        Self {
            _dir: dir,
            base,
            journal,
        }
    }

    fn records(&self) -> Vec<String> {
        let mut names = self
            .journal
            .list()
            .expect("list")
            .into_iter()
            .filter(|name| name.ends_with(".rec"))
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn report(&self) -> String {
        std::fs::read_to_string(self.base.join("corbanu/sandbox-placeholders/kept.log"))
            .unwrap_or_default()
    }

    /// Makes a placeholder at `path` as a run does, and writes the record of
    /// a run (`owner`) that made it.
    fn made_by(&self, owner: Owner, path: &Path, kind: Kind) -> Entry {
        let identity = make_placeholder(path, kind).expect("placeholder");
        let entry = Entry::Made {
            kind,
            path: path.to_path_buf(),
            real: path.to_path_buf(),
            identity,
        };
        self.write(owner, vec![entry.clone()]);
        entry
    }

    fn write(&self, owner: Owner, entries: Vec<Entry>) {
        let stem = format!("{}-{}-1", owner.pid, owner.start.unwrap_or(0));
        self.journal
            .write_named(&stem, &Record { owner, entries })
            .expect("record");
    }

    fn sweep(&mut self) {
        self.journal.sweep(&|_, _| false);
    }

    fn left_folder_with_children(&self, children: usize) -> PathBuf {
        let folder = self.base.join("home/.git");
        let paths = (0..children)
            .map(|index| folder.join(format!("child-{index}")))
            .collect::<Vec<_>>();
        let mut planned = vec![Planned {
            kind: Kind::Directory,
            path: &folder,
            real: &folder,
            makes: true,
        }];
        planned.extend(paths.iter().map(|path| Planned {
            kind: Kind::File,
            path,
            real: path,
            makes: true,
        }));
        let mut maker =
            open_journal(&self.base.join("corbanu/sandbox-placeholders")).expect("maker");
        maker.register(&planned, &[]);
        let records = self.records();
        assert_eq!(records.len(), 1);
        let record = maker.read(&records[0]).expect("record");
        maker.remove_record().expect("unlink");
        self.write(ended(), record.entries);
        folder
    }
}

/// A run that has ended: its pid is gone (or names another process, which
/// started at another time).
fn ended() -> Owner {
    let mut child = std::process::Command::new("true").spawn().expect("spawn");
    child.wait().expect("wait");
    Owner {
        host: own_host().cloned(),
        boot: own_boot().cloned(),
        pid: child.id() as libc::pid_t,
        start: Some(1),
        pidns: crate::placeholder_registry::own_pid_namespace(),
    }
}

fn this_run() -> Owner {
    own_owner()
}

fn this_run_with_start(start: Option<u64>) -> Owner {
    Owner {
        start,
        ..this_run()
    }
}

/// Waits past the file system's timestamp granularity, so a file made now
/// cannot have the times of one made just before.
fn tick() {
    std::thread::sleep(Duration::from_millis(50));
}

#[test]
fn a_record_round_trips_and_one_not_written_by_this_version_is_not_read() {
    let record = Record {
        owner: Owner {
            host: Some("h1".to_string()),
            boot: Some("b1".to_string()),
            pid: 42,
            start: Some(7),
            pidns: None,
        },
        entries: vec![
            Entry::Uses {
                path: PathBuf::from("/a/b c"),
                real: PathBuf::from("/real/b c"),
            },
            Entry::Pending {
                kind: Kind::Directory,
                path: PathBuf::from("/a/.git"),
                real: PathBuf::from("/a/.git"),
            },
            Entry::Made {
                kind: Kind::File,
                path: PathBuf::from("/home/u/.pfterminal"),
                real: PathBuf::from("/home/u/.pfterminal"),
                identity: Identity {
                    dev: 1,
                    ino: 2,
                    mode: 0o100444,
                    uid: 1000,
                    size: 0,
                    mtime: (3, 4),
                    ctime: (5, 6),
                },
            },
        ],
    };
    let text = serialize(&record);
    assert_eq!(parse(&text), Some(record));

    let made = text
        .lines()
        .find(|line| line.starts_with("made "))
        .expect("made line")
        .to_string();
    for changed in [
        text.replace(HEADER, "corbanu-sandbox-placeholders 2"),
        text.replace("end\n", ""),
        format!("{text}uses 2f 2f\n"),
        text.replace(&made, &format!("{made} 9")),
        // A relative path, a path with `..`, and two names.
        text.replace(&hex(Path::new("/a/.git")), &hex(Path::new("a/.git"))),
        text.replace(&hex(Path::new("/a/.git")), &hex(Path::new("/a/../.git"))),
        text.replace(
            &format!(
                "{} {}",
                hex(Path::new("/a/.git")),
                hex(Path::new("/a/.git"))
            ),
            &format!(
                "{} {}",
                hex(Path::new("/a/.git")),
                hex(Path::new("/a/.svn"))
            ),
        ),
        text.replace("pid 42", "pid x"),
    ] {
        assert_eq!(parse(&changed), None, "{changed}");
    }
}

#[test]
fn a_run_is_live_until_it_ends_its_boot_ends_or_its_pid_is_reused() {
    assert!(owner_is_live(&this_run()));
    assert!(!owner_is_live(&ended()));
    let another_boot = Owner {
        boot: Some("another-boot".to_string()),
        ..this_run()
    };
    assert!(!owner_is_live(&another_boot));
    let reused_pid = Owner {
        start: Some(1),
        ..this_run()
    };
    assert!(!owner_is_live(&reused_pid));
    // A run on another machine sharing the home is taken as running.
    let other_machine = Owner {
        host: Some("another-machine".to_string()),
        boot: Some("another-boot".to_string()),
        ..ended()
    };
    assert!(owner_is_live(&other_machine));
    // One that has exited, not yet reaped, still counts (as `kill(pid, 0)`
    // does for the temporary folder's registry).
    let mut zombie = std::process::Command::new("true").spawn().expect("spawn");
    let zombie_pid = zombie.id() as libc::pid_t;
    let is_zombie = || {
        std::fs::read_to_string(format!("/proc/{zombie_pid}/stat")).is_ok_and(|stat| {
            stat.rsplit(')')
                .next()
                .is_some_and(|rest| rest.trim_start().starts_with('Z'))
        })
    };
    while !is_zombie() {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(owner_is_live(&Owner {
        pid: zombie_pid,
        start: process_start(zombie_pid).expect("stat"),
        ..this_run()
    }));
    zombie.wait().expect("reap");
    // One whose machine is not known is taken as running.
    assert!(owner_is_live(&Owner {
        host: None,
        ..ended()
    }));
    // One whose PID namespace is not known is taken as running.
    let unknown_namespace = Owner {
        pidns: None,
        ..ended()
    };
    assert!(owner_is_live(&unknown_namespace));
    // A run in another PID namespace is taken as running.
    let other_namespace = Owner {
        pidns: Some(1),
        ..ended()
    };
    assert!(owner_is_live(&other_namespace));
}

#[test]
fn a_placeholder_a_run_made_and_left_is_removed() {
    let mut setup = Setup::new();
    let file = setup.base.join("home/.pfterminal");
    let folder = setup.base.join("home/.git");
    setup.made_by(ended(), &file, Kind::File);
    setup.made_by(
        Owner {
            boot: Some("another-boot".to_string()),
            ..this_run()
        },
        &folder,
        Kind::Directory,
    );
    let status = std::fs::symlink_metadata(&file).expect("made");
    assert!(status.is_file() && status.len() == 0);
    assert_eq!(status.permissions().mode() & 0o777, 0o444);
    assert_eq!(setup.records().len(), 2);

    setup.sweep();

    assert!(std::fs::symlink_metadata(&file).is_err());
    assert!(std::fs::symlink_metadata(&folder).is_err());
    assert_eq!(setup.records(), Vec::<String>::new());
    assert_eq!(setup.report(), "");
}

#[test]
fn a_sweep_limits_successful_removals_across_records_and_resumes() {
    let mut setup = Setup::new();
    for group in 0..2 {
        let entries = (0..40)
            .map(|index| {
                let path = setup.base.join(format!("home/group-{group}-{index}"));
                let identity = make_placeholder(&path, Kind::File).expect("placeholder");
                Entry::Made {
                    kind: Kind::File,
                    path: path.clone(),
                    real: path,
                    identity,
                }
            })
            .collect();
        setup.write(ended(), entries);
    }
    assert_eq!(setup.records().len(), 2);

    setup.sweep();

    assert_eq!(
        std::fs::read_dir(setup.base.join("home"))
            .expect("home")
            .count(),
        16
    );
    let remaining = setup
        .records()
        .iter()
        .map(|name| setup.journal.read(name).expect("deferred record"))
        .flat_map(|record| record.entries)
        .collect::<Vec<_>>();
    assert_eq!(remaining.len(), 16);
    assert!(remaining.iter().all(|entry| entry.paths().1.is_file()));
    assert_eq!(setup.report(), "");

    setup.sweep();

    assert_eq!(
        std::fs::read_dir(setup.base.join("home"))
            .expect("home")
            .count(),
        0
    );
    assert_eq!(setup.records(), Vec::<String>::new());
    assert_eq!(setup.report(), "");
}

/// Makes `count` placeholders in `home`, named `<prefix>-<index>`, and
/// returns their paths and entries.
fn placeholders(setup: &Setup, prefix: &str, count: usize) -> (Vec<PathBuf>, Vec<Entry>) {
    let paths = (0..count)
        .map(|index| setup.base.join(format!("home/{prefix}-{index}")))
        .collect::<Vec<_>>();
    let entries = paths
        .iter()
        .map(|path| Entry::Made {
            kind: Kind::File,
            path: path.clone(),
            real: path.clone(),
            identity: make_placeholder(path, Kind::File).expect("placeholder"),
        })
        .collect();
    (paths, entries)
}

/// Writes the record of an ended run that made `count` placeholders.
fn ended_with_placeholders(setup: &Setup, prefix: &str, count: usize) -> Vec<PathBuf> {
    let (paths, entries) = placeholders(setup, prefix, count);
    setup.write(ended(), entries);
    paths
}

/// Makes the sweep read `points` as the mount points, and returns how many
/// times it has read them.
fn read_mounts_as(
    setup: &mut Setup,
    points: Option<HashSet<Vec<u8>>>,
) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = reads.clone();
    setup.journal.mount_points = Box::new(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        points.clone()
    });
    reads
}

fn recorded_entries(setup: &Setup) -> usize {
    setup
        .records()
        .iter()
        .map(|name| setup.journal.read(name).expect("record").entries.len())
        .sum()
}

#[test]
fn placeholders_mounted_over_do_not_hold_up_the_ones_after_them() {
    let mut setup = Setup::new();
    // One record, in this order (same depth; the sort is stable).
    let (mounted, mut entries) = placeholders(&setup, "mounted", /*count*/ 70);
    let (removable, more) = placeholders(&setup, "removable", /*count*/ 3);
    entries.extend(more);
    setup.write(ended(), entries);
    let points = mounted
        .iter()
        .map(|path| path.as_os_str().as_bytes().to_vec())
        .collect();
    let reads = read_mounts_as(&mut setup, Some(points));

    setup.sweep();

    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 73);
    assert!(mounted.iter().all(|path| path.is_file()));
    assert!(removable.iter().all(|path| !path.exists()));
    assert_eq!(recorded_entries(&setup), 70);
    assert_eq!(setup.report(), "");

    read_mounts_as(&mut setup, Some(HashSet::new()));
    setup.sweep();
    setup.sweep();

    assert!(mounted.iter().all(|path| !path.exists()));
    assert_eq!(setup.records(), Vec::<String>::new());
    assert_eq!(setup.report(), "");
}

#[test]
fn a_parent_that_fails_after_its_children_keeps_the_identity_they_left() {
    let mut setup = Setup::new();
    let folder = setup.left_folder_with_children(/*children*/ 3);
    // The children's reads succeed; the parent's cannot be made.
    let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = reads.clone();
    setup.journal.mount_points = Box::new(move || {
        (counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 3).then(HashSet::new)
    });

    setup.sweep();

    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 4);
    assert!(folder.is_dir());
    assert_eq!(std::fs::read_dir(&folder).expect("folder").count(), 0);
    assert_eq!(recorded_entries(&setup), 1);

    read_mounts_as(&mut setup, Some(HashSet::new()));
    setup.sweep();

    assert!(!folder.exists(), "{}", setup.report());
    assert_eq!(setup.records(), Vec::<String>::new());
    assert_eq!(setup.report(), "");
}

#[test]
fn a_sweep_stops_once_the_mount_points_cannot_be_read() {
    let mut setup = Setup::new();
    let first = ended_with_placeholders(&setup, "first", /*count*/ 3);
    let second = ended_with_placeholders(&setup, "second", /*count*/ 3);
    let reads = read_mounts_as(&mut setup, /*points*/ None);

    setup.sweep();

    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(first.iter().chain(&second).all(|path| path.is_file()));
    assert_eq!(recorded_entries(&setup), 6);
    assert_eq!(setup.report(), "");
}

#[test]
fn placeholders_settled_before_the_mount_read_do_not_use_up_the_sweep() {
    let mut setup = Setup::new();
    // One record, in this order (same depth; the sort is stable).
    let (gone, mut entries) = placeholders(&setup, "gone", /*count*/ 10);
    for path in &gone {
        std::fs::remove_file(path).expect("removed by the person");
    }
    let (removable, more) = placeholders(&setup, "removable", /*count*/ 64);
    entries.extend(more);
    setup.write(ended(), entries);
    let reads = read_mounts_as(&mut setup, Some(HashSet::new()));

    setup.sweep();

    assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 64);
    assert!(removable.iter().all(|path| !path.exists()));
    assert_eq!(setup.records(), Vec::<String>::new());
    assert_eq!(setup.report(), "");
}

#[test]
fn a_deferred_parent_keeps_the_identity_its_removed_children_left() {
    let mut setup = Setup::new();
    let folder = setup.left_folder_with_children(/*children*/ 64);

    setup.sweep();

    assert!(folder.is_dir(), "the parent is deferred after 64 removals");
    assert_eq!(std::fs::read_dir(&folder).expect("folder").count(), 0);
    let names = setup.records();
    assert_eq!(names.len(), 1);
    let record = setup.journal.read(&names[0]).expect("deferred record");
    let found = crate::placeholder_registry::find_without_links(&folder)
        .expect("lookup")
        .expect("parent");
    assert_eq!(
        record.entries,
        vec![Entry::Made {
            kind: Kind::Directory,
            path: folder.clone(),
            real: folder.clone(),
            identity: Identity::of(&found.status),
        }]
    );

    setup.sweep();

    assert!(!folder.exists(), "{}", setup.report());
    assert_eq!(setup.records(), Vec::<String>::new());
    assert_eq!(setup.report(), "");
}

#[test]
fn a_deferred_parent_changed_between_sweeps_is_still_kept() {
    let mut setup = Setup::new();
    let folder = setup.left_folder_with_children(/*children*/ 64);

    setup.sweep();

    assert!(folder.is_dir());
    tick();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700))
        .expect("the person changes the parent");
    setup.sweep();

    assert_eq!(
        std::fs::symlink_metadata(&folder)
            .expect("kept parent")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(setup.records(), Vec::<String>::new(), "reported once");
    assert!(setup.report().contains(".git"), "{}", setup.report());
}

#[test]
fn a_persons_own_empty_read_only_file_at_that_path_is_kept() {
    let mut setup = Setup::new();
    let path = setup.base.join("home/.pfterminal");
    setup.made_by(ended(), &path, Kind::File);
    std::fs::remove_file(&path).expect("the person removes it");
    tick();
    std::fs::write(&path, "").expect("and makes their own");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).expect("read-only");
    let own = std::fs::symlink_metadata(&path).expect("own").ino();

    setup.sweep();

    assert_eq!(std::fs::symlink_metadata(&path).expect("kept").ino(), own);
    assert_eq!(setup.records(), Vec::<String>::new(), "reported once");
    assert!(setup.report().contains(".pfterminal"), "{}", setup.report());
}

#[test]
fn a_placeholder_filled_replaced_or_changed_since_is_kept() {
    let mut setup = Setup::new();
    let filled = setup.base.join("home/filled");
    let replaced = setup.base.join("home/replaced");
    let changed = setup.base.join("home/changed");
    let folder = setup.base.join("home/folder");
    for path in [&filled, &replaced, &changed] {
        setup.made_by(ended(), path, Kind::File);
    }
    setup.made_by(ended(), &folder, Kind::Directory);
    tick();
    std::fs::set_permissions(&filled, std::fs::Permissions::from_mode(0o644)).expect("writable");
    std::fs::write(&filled, "data").expect("filled");
    let other = setup.base.join("home/other");
    std::fs::write(&other, "").expect("other");
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o444)).expect("read-only");
    std::fs::rename(&other, &replaced).expect("replaced");
    std::fs::set_permissions(&changed, std::fs::Permissions::from_mode(0o400)).expect("changed");
    std::fs::write(folder.join("kept"), "x").expect("filled folder");

    setup.sweep();

    assert_eq!(std::fs::read_to_string(&filled).expect("kept"), "data");
    assert!(replaced.is_file() && changed.is_file() && folder.join("kept").is_file());
    assert_eq!(setup.records(), Vec::<String>::new());
    let report = setup.report();
    for name in ["filled", "replaced", "changed", "folder"] {
        assert!(
            report.contains(&format!("home/{name}:")),
            "{name}: {report}"
        );
    }
}

#[test]
fn a_placeholder_a_live_run_uses_is_kept_until_it_ends() {
    let mut setup = Setup::new();
    let path = setup.base.join("home/.pfterminal");
    let inside = setup.base.join("home/.git");
    let maker = ended();
    setup.made_by(maker.clone(), &path, Kind::File);
    setup.made_by(
        Owner {
            pid: maker.pid,
            start: Some(2),
            ..maker
        },
        &inside,
        Kind::Directory,
    );
    // A live run mounts over the file, and has a placeholder of its own in
    // the folder.
    let live = Owner {
        start: own_owner().start,
        ..this_run()
    };
    let live_stem = format!("{}-{}-1", live.pid, live.start.unwrap_or(0));
    setup.write(
        live,
        vec![
            Entry::Uses {
                path: path.clone(),
                real: path.clone(),
            },
            Entry::Uses {
                path: inside.join("hooks"),
                real: inside.join("hooks"),
            },
        ],
    );

    setup.sweep();
    assert!(path.is_file() && inside.is_dir(), "kept while used");
    assert_eq!(setup.records().len(), 3);

    setup
        .journal
        .unlink(&format!("{live_stem}.rec"))
        .expect("the live run ends");
    setup.sweep();
    assert!(std::fs::symlink_metadata(&path).is_err());
    assert!(std::fs::symlink_metadata(&inside).is_err());
    assert_eq!(setup.records(), Vec::<String>::new());

    // A live run of the temporary folder's registry keeps it too.
    setup.made_by(ended(), &path, Kind::File);
    setup.journal.sweep(&|spelled, _| spelled == path);
    assert!(path.is_file());
    assert_eq!(setup.records().len(), 1);
}

#[test]
fn a_pending_placeholder_is_never_removed() {
    let mut setup = Setup::new();
    let path = setup.base.join("home/.pfterminal");
    std::fs::write(&path, "").expect("bubblewrap made it");
    setup.write(
        ended(),
        vec![Entry::Pending {
            kind: Kind::File,
            path: path.clone(),
            real: path.clone(),
        }],
    );

    setup.sweep();

    assert!(path.is_file());
    assert_eq!(setup.records(), Vec::<String>::new());
    assert!(setup.report().contains(".pfterminal"));
}

#[test]
fn removal_does_not_follow_a_link_above_the_placeholder() {
    let mut setup = Setup::new();
    let folder = setup.base.join("home/folder");
    std::fs::create_dir(&folder).expect("folder");
    let path = folder.join("x");
    setup.made_by(ended(), &path, Kind::File);
    // The folder moves, and a link to it takes its place: the placeholder,
    // as made, is reached through the link.
    let moved = setup.base.join("home/moved");
    std::fs::rename(&folder, &moved).expect("move");
    std::os::unix::fs::symlink(&moved, &folder).expect("link");
    assert!(
        std::fs::symlink_metadata(&path)
            .expect("through the link")
            .is_file()
    );

    setup.sweep();

    assert!(moved.join("x").is_file(), "not removed through the link");
    assert_eq!(setup.records(), Vec::<String>::new());
}

#[test]
fn a_run_records_and_makes_its_placeholders_then_finishes() {
    let mut setup = Setup::new();
    let file = setup.base.join("home/.pfterminal");
    let folder = setup.base.join("home/.git");
    let held = setup.base.join("home/held");
    std::fs::write(&held, "").expect("held");
    let planned = [
        Planned {
            kind: Kind::File,
            path: &file,
            real: &file,
            makes: true,
        },
        Planned {
            kind: Kind::Directory,
            path: &folder,
            real: &folder,
            makes: true,
        },
    ];
    setup
        .journal
        .register(&planned, &[(held.clone(), held.clone())]);

    assert!(file.is_file() && folder.is_dir());
    let records = setup.records();
    assert_eq!(records.len(), 1);
    let record = setup.journal.read(&records[0]).expect("record");
    assert_eq!(record.owner, own_owner());
    let made = record
        .entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Made { path, identity, .. } => Some((path.clone(), identity.ino)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        made,
        vec![
            (
                file.clone(),
                std::fs::symlink_metadata(&file).expect("file").ino()
            ),
            (
                folder.clone(),
                std::fs::symlink_metadata(&folder).expect("folder").ino()
            ),
        ]
    );
    // An empty file it mounts at that no run recorded is not recorded.
    assert!(!uses(&record, &held, &held));
    // Another run's sweep leaves a live run's placeholders.
    let mut other = open_journal(&setup.base.join("corbanu/sandbox-placeholders")).expect("other");
    other.sweep(&|_, _| false);
    assert!(file.is_file() && folder.is_dir());

    // Once its command has ended it uses nothing: only what it made stays.
    setup.journal.leave();
    let record = setup.journal.read(&records[0]).expect("record");
    assert_eq!(record.entries.len(), 2);
    assert!(
        record
            .entries
            .iter()
            .all(|entry| matches!(entry, Entry::Made { .. }))
    );
    assert!(!uses(&record, &held, &held));

    // Its cleanup removed the file; another run still holds the folder.
    std::fs::remove_file(&file).expect("cleanup");
    setup.journal.finish();
    let records = setup.records();
    assert_eq!(records.len(), 1);
    let record = setup.journal.read(&records[0]).expect("record");
    assert!(matches!(
        record.entries.as_slice(),
        [Entry::Made { path, .. }] if *path == folder
    ));
    std::fs::remove_dir(&folder).expect("the holder removes it");
    setup.journal.finish();
    assert_eq!(setup.records(), Vec::<String>::new());

    // Never over something already there.
    std::fs::write(&file, "").expect("someone's file");
    setup.journal.register(&planned[..1], &[]);
    let records = setup.records();
    let record = setup.journal.read(&records[0]).expect("record");
    assert!(
        record
            .entries
            .iter()
            .all(|entry| !matches!(entry, Entry::Made { .. }))
    );
    setup.journal.finish();
    assert_eq!(setup.records(), Vec::<String>::new());
}

#[test]
fn a_record_folder_of_another_kind_is_not_used() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let base = dir.path().canonicalize().expect("canonical");
    // The home itself is never made.
    assert!(open_journal(&base.join("missing/sandbox-placeholders")).is_err());
    assert!(!base.join("missing").exists());
    // Not through a link.
    std::fs::create_dir(base.join("target")).expect("target");
    std::fs::create_dir(base.join("home")).expect("home");
    std::os::unix::fs::symlink(base.join("target"), base.join("home/sandbox-placeholders"))
        .expect("link");
    assert!(open_journal(&base.join("home/sandbox-placeholders")).is_err());
    // A folder others may write is made private.
    std::fs::create_dir(base.join("home/open")).expect("open");
    std::fs::set_permissions(
        base.join("home/open"),
        std::fs::Permissions::from_mode(0o777),
    )
    .expect("mode");
    open_journal(&base.join("home/open")).expect("journal");
    assert_eq!(
        std::fs::metadata(base.join("home/open"))
            .expect("open")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn an_unreadable_record_of_a_live_run_keeps_every_placeholder() {
    let mut setup = Setup::new();
    let path = setup.base.join("home/.pfterminal");
    setup.made_by(ended(), &path, Kind::File);
    // A newer version's record of a run that is running.
    let live = this_run();
    let unreadable = setup.base.join(format!(
        "corbanu/sandbox-placeholders/{}-{}-9.rec",
        live.pid,
        live.start.unwrap_or(0)
    ));
    std::fs::write(&unreadable, "corbanu-sandbox-placeholders 2\n").expect("record");
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o600)).expect("mode");

    setup.sweep();
    assert!(path.is_file(), "kept while that run runs");
    assert!(setup.journal.used_by_live_run(&path, &path));

    // Once that run has ended, it no longer counts.
    let ended = ended();
    std::fs::rename(
        &unreadable,
        setup.base.join(format!(
            "corbanu/sandbox-placeholders/{}-1-9.rec",
            ended.pid
        )),
    )
    .expect("rename");
    assert!(!setup.journal.used_by_live_run(&path, &path));
    setup.sweep();
    assert!(std::fs::symlink_metadata(&path).is_err());
}

// A proc entry supplied to the real scanner, with an owned root inode and
// a mountinfo file. These fixtures do not chroot, mount, or create processes.
fn write_mount_view(process: &Path, namespace: &str, root: &Path, point: &str) {
    std::fs::create_dir_all(process.join("ns")).expect("process namespace folder");
    std::os::unix::fs::symlink(namespace, process.join("ns/mnt")).expect("namespace link");
    std::os::unix::fs::symlink(root, process.join("root")).expect("process root link");
    std::fs::write(
        process.join("mountinfo"),
        format!("1 0 0:1 / {point} rw - tmpfs tmpfs rw\n"),
    )
    .expect("mountinfo");
}

#[test]
fn different_roots_in_one_mount_namespace_are_all_scanned() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let root_a = temp.path().join("root-a");
    let root_b = temp.path().join("root-b");
    std::fs::create_dir(&root_a).expect("first root");
    std::fs::create_dir(&root_b).expect("second root");
    let first = temp.path().join("101");
    let second = temp.path().join("102");
    write_mount_view(&first, "mnt:[1]", &root_a, "/first-root-mount");
    write_mount_view(&second, "mnt:[1]", &root_b, "/second-root-mount");

    assert_eq!(
        mount_points_for_processes([first, second].into_iter()),
        Some(HashSet::from([
            b"/first-root-mount".to_vec(),
            b"/second-root-mount".to_vec(),
        ])),
    );
}

#[test]
fn identical_mount_views_are_deduplicated_without_hiding_other_namespaces() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let root = temp.path().join("root");
    std::fs::create_dir(&root).expect("root");
    let first = temp.path().join("101");
    let duplicate = temp.path().join("102");
    let other_namespace = temp.path().join("103");
    write_mount_view(&first, "mnt:[1]", &root, "/first-namespace-mount");
    write_mount_view(&duplicate, "mnt:[1]", &root, "/unused");
    write_mount_view(&other_namespace, "mnt:[2]", &root, "/other-namespace-mount");
    // Reading a directory as mountinfo fails. The duplicate must not be read.
    std::fs::remove_file(duplicate.join("mountinfo")).expect("remove duplicate file");
    std::fs::create_dir(duplicate.join("mountinfo")).expect("unreadable duplicate");

    assert_eq!(
        mount_points_for_processes([first, duplicate, other_namespace].into_iter()),
        Some(HashSet::from([
            b"/first-namespace-mount".to_vec(),
            b"/other-namespace-mount".to_vec(),
        ])),
    );
}

#[test]
fn unidentified_roots_do_not_hide_another_process_mount_view() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let unavailable_root = temp.path().join("absent-root");
    let first = temp.path().join("101");
    let second = temp.path().join("102");
    write_mount_view(&first, "mnt:[1]", &unavailable_root, "/first-view");
    write_mount_view(&second, "mnt:[1]", &unavailable_root, "/second-view");

    assert_eq!(
        mount_points_for_processes([first, second].into_iter()),
        Some(HashSet::from([
            b"/first-view".to_vec(),
            b"/second-view".to_vec(),
        ])),
    );
}

#[test]
fn an_unreadable_mount_view_still_fails_closed() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let root_a = temp.path().join("root-a");
    let root_b = temp.path().join("root-b");
    std::fs::create_dir(&root_a).expect("first root");
    std::fs::create_dir(&root_b).expect("second root");
    let first = temp.path().join("101");
    let second = temp.path().join("102");
    write_mount_view(&first, "mnt:[1]", &root_a, "/first-view");
    write_mount_view(&second, "mnt:[1]", &root_b, "/unreadable-view");
    std::fs::remove_file(second.join("mountinfo")).expect("remove mountinfo file");
    std::fs::create_dir(second.join("mountinfo")).expect("unreadable view");

    assert_eq!(
        mount_points_for_processes([first, second].into_iter()),
        None
    );
}

#[test]
fn a_mount_point_in_any_of_this_users_namespaces_is_found() {
    assert_eq!(unescape_mount_point(br"/a\040b\134c"), b"/a b\\c".to_vec());
    assert_eq!(unescape_mount_point(br"/plain\x"), br"/plain\x".to_vec());
    // This process's own `/proc` is a mount point; a fresh folder is not.
    assert!(mounted_anywhere(&[Path::new("/proc")], mount_points()));
    let dir = tempfile::TempDir::new().expect("tempdir");
    assert!(!mounted_anywhere(&[dir.path()], mount_points()));
    assert!(mounted_anywhere(&[dir.path()], /*points*/ None));
}

// A fake `/proc/<pid>` in mount namespace `namespace`, with a `mountinfo`
// listing `point`, or none (the process has ended) when `point` is `None`.
fn fake_process(proc_dir: &Path, pid: &str, namespace: &str, point: Option<&str>) -> PathBuf {
    let process = proc_dir.join(pid);
    std::fs::create_dir_all(process.join("ns")).expect("ns folder");
    std::os::unix::fs::symlink(namespace, process.join("ns/mnt")).expect("ns link");
    // All fake processes share one root, so their views dedupe by namespace.
    std::os::unix::fs::symlink(proc_dir, process.join("root")).expect("root link");
    if let Some(point) = point {
        std::fs::write(
            process.join("mountinfo"),
            format!("1 0 0:1 / {point} rw - tmpfs tmpfs rw\n"),
        )
        .expect("mountinfo");
    }
    process
}

#[test]
fn a_process_that_ends_mid_scan_does_not_hide_its_namespace() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let ended = fake_process(temp.path(), "101", "mnt:[1]", /*point*/ None);
    let live = fake_process(temp.path(), "102", "mnt:[1]", Some("/live-view"));
    let other = fake_process(temp.path(), "103", "mnt:[2]", Some("/other-view"));

    assert_eq!(
        mount_points_for_processes([ended, live, other].into_iter()),
        Some(HashSet::from([
            b"/live-view".to_vec(),
            b"/other-view".to_vec(),
        ])),
    );
}

#[test]
fn a_namespace_whose_view_was_read_is_read_once() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let first = fake_process(temp.path(), "101", "mnt:[1]", Some("/first-view"));
    // Reading a folder as `mountinfo` fails the whole scan, so this view
    // must not be read: its namespace was already read through `first`.
    let duplicate = fake_process(temp.path(), "102", "mnt:[1]", /*point*/ None);
    std::fs::create_dir(duplicate.join("mountinfo")).expect("unreadable view");

    assert_eq!(
        mount_points_for_processes([first, duplicate].into_iter()),
        Some(HashSet::from([b"/first-view".to_vec()])),
    );
}

#[test]
fn an_unreadable_view_still_fails_the_scan() {
    let temp = tempfile::TempDir::new().expect("tempdir");
    let unreadable = fake_process(temp.path(), "101", "mnt:[1]", /*point*/ None);
    std::fs::create_dir(unreadable.join("mountinfo")).expect("unreadable view");
    let live = fake_process(temp.path(), "102", "mnt:[1]", Some("/live-view"));

    assert_eq!(
        mount_points_for_processes([unreadable, live].into_iter()),
        None
    );
}

#[test]
fn a_held_lock_leaves_the_run_without_the_record() {
    let mut setup = Setup::new();
    let left = setup.base.join("home/left");
    setup.made_by(ended(), &left, Kind::File);
    let mut holder =
        open_journal(&setup.base.join("corbanu/sandbox-placeholders")).expect("holder");
    let held = holder.lock().expect("lock").expect("held");

    let path = setup.base.join("home/.pfterminal");
    setup.journal.register(
        &[Planned {
            kind: Kind::File,
            path: &path,
            real: &path,
            makes: true,
        }],
        &[],
    );
    assert!(
        !path.exists(),
        "nothing made (bubblewrap makes it, as before)"
    );
    assert!(setup.journal.unavailable);
    assert!(setup.journal.used_by_live_run(&left, &left), "kept");
    setup.sweep();
    assert!(left.is_file(), "nothing removed");
    assert_eq!(setup.records().len(), 1);

    // No other descriptor can be opened to take any lock on it: it is
    // `0200` (and, in a sandbox, on a read-only bind).
    drop(held);
    let lock = setup.base.join("corbanu/sandbox-placeholders/lock");
    assert_eq!(
        std::fs::metadata(&lock).expect("lock").permissions().mode() & 0o7777,
        0o200
    );
    // SAFETY: `geteuid` has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        assert!(File::open(&lock).is_err());
    }
}

#[test]
fn the_record_folder_is_found_through_another_path_to_it() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let base = dir.path().canonicalize().expect("canonical");
    let record = base.join("home/.corbanu/sandbox-placeholders");
    std::fs::create_dir_all(&record).expect("record");
    std::os::unix::fs::symlink(base.join("home"), base.join("alias")).expect("alias");
    assert_eq!(
        exposed_below(&record, &base.join("alias")),
        Some(base.join("alias/.corbanu/sandbox-placeholders"))
    );
    assert_eq!(
        exposed_below(&record, &base.join("home")),
        Some(record.clone())
    );
    assert_eq!(exposed_below(&record, &record), Some(record.clone()));
    std::fs::create_dir(record.join("inside")).expect("inside");
    assert_eq!(
        exposed_below(&record, &record.join("inside")),
        Some(record.join("inside"))
    );
    std::fs::create_dir(base.join("other")).expect("other");
    assert_eq!(exposed_below(&record, &base.join("other")), None);
}

#[test]
fn a_placeholder_folder_holding_another_of_the_runs_placeholders_is_removed() {
    let mut setup = Setup::new();
    let folder = setup.base.join("home/.git");
    let inside = folder.join("hooks");
    let maker = ended();
    let mut journal_of_the_maker =
        open_journal(&setup.base.join("corbanu/sandbox-placeholders")).expect("maker");
    journal_of_the_maker.register(
        &[
            Planned {
                kind: Kind::Directory,
                path: &folder,
                real: &folder,
                makes: true,
            },
            Planned {
                kind: Kind::File,
                path: &inside,
                real: &inside,
                makes: true,
            },
        ],
        &[],
    );
    assert!(inside.is_file());
    // The maker is killed: its record, written as by that run.
    let records = setup.records();
    let record = journal_of_the_maker.read(&records[0]).expect("record");
    journal_of_the_maker.remove_record().expect("unlink");
    setup.write(maker, record.entries);

    setup.sweep();

    assert!(
        std::fs::symlink_metadata(&folder).is_err(),
        "{}",
        setup.report()
    );
    assert_eq!(setup.report(), "");
    assert_eq!(setup.records(), Vec::<String>::new());
}

#[test]
fn a_read_only_record_folder_keeps_no_record() {
    // SAFETY: `geteuid` has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let dir = tempfile::TempDir::new().expect("tempdir");
    let record = dir.path().join("sandbox-placeholders");
    std::fs::create_dir(&record).expect("record");
    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o500)).expect("mode");
    assert!(open_journal(&record).is_err(), "as before: no record");
    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o700)).expect("mode");
}

#[test]
fn a_path_another_run_made_is_recorded_as_used_and_nothing_else() {
    let mut setup = Setup::new();
    let made = setup.base.join("home/made");
    let other = setup.base.join("home/other");
    std::fs::write(&other, "").expect("other");
    setup.made_by(ended(), &made, Kind::File);
    // Nothing to make and nothing recorded mounted at: no record at all.
    setup
        .journal
        .register(&[], &[(other.clone(), other.clone())]);
    assert_eq!(setup.records().len(), 1);
    // Another run's placeholder mounted at: recorded as used.
    setup.journal.register(&[], &[(made.clone(), made.clone())]);
    let records = setup.records();
    assert_eq!(records.len(), 2);
    let own = format!("{}.rec", setup.journal.name);
    let record = setup.journal.read(&own).expect("record");
    assert!(uses(&record, &made, &made) && !uses(&record, &other, &other));
    let mut sweeper =
        open_journal(&setup.base.join("corbanu/sandbox-placeholders")).expect("sweeper");
    sweeper.sweep(&|_, _| false);
    assert!(made.is_file(), "kept while used");
}

#[test]
fn a_child_of_a_pending_placeholder_folder_is_recorded_as_used() {
    let mut setup = Setup::new();
    let pending = setup.base.join("home/pending");
    let prefix_sibling = setup.base.join("home/pending2");
    let child = pending.join("child");
    let sibling_child = prefix_sibling.join("child");
    std::fs::create_dir(&pending).expect("pending folder");
    std::fs::create_dir(&prefix_sibling).expect("prefix sibling folder");
    std::fs::write(&child, "").expect("child");
    std::fs::write(&sibling_child, "").expect("sibling child");
    setup.write(
        this_run_with_start(Some(7)),
        vec![Entry::Pending {
            kind: Kind::Directory,
            path: pending.clone(),
            real: pending.clone(),
        }],
    );
    let pending_name = setup.records().pop().expect("pending record name");
    let pending_record = setup.journal.read(&pending_name).expect("pending record");
    setup.journal.register(
        &[],
        &[
            (child.clone(), child.clone()),
            (sibling_child.clone(), sibling_child),
        ],
    );
    let own = format!("{}.rec", setup.journal.name);
    let record = setup.journal.read(&own).expect("record");
    assert_eq!(
        record.entries,
        vec![Entry::Uses {
            path: child.clone(),
            real: child,
        }]
    );
    assert!(uses(&record, &pending, &pending));
    assert!(!uses(&record, &prefix_sibling, &prefix_sibling));
    assert_eq!(setup.journal.read(&pending_name), Some(pending_record));
}

#[test]
fn a_path_another_run_was_about_to_make_is_recorded_as_used() {
    let mut setup = Setup::new();
    let pending = setup.base.join("home/pending");
    let sibling = setup.base.join("home/sibling");
    let prefix_sibling = setup.base.join("home/pending2");
    std::fs::write(&pending, "").expect("bubblewrap made it");
    std::fs::write(&sibling, "").expect("sibling");
    std::fs::write(&prefix_sibling, "").expect("prefix sibling");
    // Another run's record (another start time than this process's): it
    // was about to make the placeholder when it was recorded.
    setup.write(
        this_run_with_start(Some(7)),
        vec![Entry::Pending {
            kind: Kind::File,
            path: pending.clone(),
            real: pending.clone(),
        }],
    );
    setup.journal.register(
        &[],
        &[
            (pending.clone(), pending.clone()),
            (sibling.clone(), sibling.clone()),
            (prefix_sibling.clone(), prefix_sibling.clone()),
        ],
    );
    let own = format!("{}.rec", setup.journal.name);
    let record = setup.journal.read(&own).expect("record");
    assert!(uses(&record, &pending, &pending));
    assert!(!uses(&record, &sibling, &sibling));
    assert!(!uses(&record, &prefix_sibling, &prefix_sibling));
    assert_eq!(
        record.entries,
        vec![Entry::Uses {
            path: pending.clone(),
            real: pending,
        }]
    );
}

/// #745: a placeholder this run made that is still there but changed (a
/// folder another run made a placeholder in, which its cleanup could then
/// not remove) stays recorded, so a later sweep reports it instead of it
/// being forgotten; one that is gone is dropped.
#[test]
fn a_changed_placeholder_stays_recorded_after_the_run() {
    let mut setup = Setup::new();
    let folder = setup.base.join("home/.git");
    let file = setup.base.join("home/.pfterminal");
    let planned = [
        Planned {
            kind: Kind::Directory,
            path: &folder,
            real: &folder,
            makes: true,
        },
        Planned {
            kind: Kind::File,
            path: &file,
            real: &file,
            makes: true,
        },
    ];
    setup.journal.register(&planned, &[]);
    tick();
    std::fs::write(folder.join("hooks"), "").expect("another run's placeholder");
    std::fs::remove_file(&file).expect("its cleanup");
    setup.journal.leave();
    setup.journal.finish();
    let records = setup.records();
    assert_eq!(records.len(), 1);
    let record = setup.journal.read(&records[0]).expect("record");
    assert!(matches!(
        record.entries.as_slice(),
        [Entry::Made { path, .. }] if *path == folder
    ));

    // (A sweep then reports it and leaves it: see
    // `a_placeholder_filled_replaced_or_changed_since_is_kept`.)
}

/// #745: another run's recorded placeholder (made, or about to be made) is
/// named as one; any other path is not.
#[test]
fn another_runs_recorded_placeholder_is_named_as_one() {
    let mut setup = Setup::new();
    let made = setup.base.join("home/.git");
    let pending = setup.base.join("home/.codex");
    setup.made_by(ended(), &made, Kind::Directory);
    setup.write(
        ended(),
        vec![Entry::Pending {
            kind: Kind::Directory,
            path: pending.clone(),
            real: pending.clone(),
        }],
    );
    assert!(setup.journal.recorded_by_another_run(&made, /*real*/ None));
    assert!(
        setup
            .journal
            .recorded_by_another_run(&pending, /*real*/ None)
    );
    let other = setup.base.join("home/.agents");
    assert!(!setup.journal.recorded_by_another_run(&other, Some(&other)));
}

/// #607: the record says what was made at a path. What is there now is
/// recorded otherwise when it is another file (the person replaced it), or
/// when another run's placeholder changed since; this run's own counts while
/// it is the same file. A path no record names is not.
#[test]
fn a_replaced_or_changed_placeholder_is_recorded_otherwise() {
    let mut setup = Setup::new();
    let status = |path: &Path| {
        crate::placeholder_registry::find_without_links(path)
            .expect("find")
            .expect("there")
            .status
    };
    let theirs = setup.base.join("home/.pfterminal");
    setup.made_by(ended(), &theirs, Kind::File);
    let own = setup.base.join("home/.git");
    let planned = [Planned {
        kind: Kind::Directory,
        path: &own,
        real: &own,
        makes: true,
    }];
    setup.journal.register(&planned, &[]);
    for path in [&theirs, &own] {
        assert!(!setup.journal.recorded_otherwise(path, path, &status(path)));
    }

    tick();
    std::fs::set_permissions(&theirs, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    std::fs::set_permissions(&own, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    assert!(
        setup
            .journal
            .recorded_otherwise(&theirs, &theirs, &status(&theirs))
    );
    assert!(!setup.journal.recorded_otherwise(&own, &own, &status(&own)));

    // Made while the placeholders are there, so other inodes.
    let new_file = setup.base.join("home/new-file");
    let new_folder = setup.base.join("home/new-folder");
    std::fs::write(&new_file, "").expect("the person's own");
    std::fs::create_dir(&new_folder).expect("the person's own");
    std::fs::rename(&new_file, &theirs).expect("replaced");
    std::fs::rename(&new_folder, &own).expect("replaced");
    for path in [&theirs, &own] {
        assert!(setup.journal.recorded_otherwise(path, path, &status(path)));
    }

    let other = setup.base.join("home/other");
    std::fs::write(&other, "").expect("other");
    assert!(
        !setup
            .journal
            .recorded_otherwise(&other, &other, &status(&other))
    );
}
