//! #442 (Travis, 2026-10-12): the Windows credential watch. These tests run
//! on every system (CI's Windows runners included); on Windows they use
//! `ReadDirectoryChangesW`.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::credential_file_globs;
use pretty_assertions::assert_eq;

use super::*;

/// The stated latency: the command is stopped within this of the file
/// appearing.
const LATENCY: Duration = Duration::from_secs(2);

fn root(dir: &tempfile::TempDir) -> PathBuf {
    dunce::canonicalize(dir.path()).unwrap()
}

fn policy(work: &Path) -> FileSystemSandboxPolicy {
    FileSystemSandboxPolicy::restricted(
        credential_file_globs(&work.to_string_lossy())
            .into_iter()
            .map(|pattern| {
                FileSystemSandboxEntry::new(
                    FileSystemPath::GlobPattern {
                        pattern,
                        scan: None,
                    },
                    FileSystemAccessMode::Deny,
                )
            })
            .collect(),
    )
}

/// A watch whose stop sets a flag.
fn watch(work: &Path, known: &[PathBuf]) -> (CredentialWatch, Arc<AtomicBool>) {
    let stopped = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stopped);
    let watch = CredentialWatch::start(
        &policy(work),
        work,
        known,
        Box::new(move || flag.store(true, Ordering::SeqCst)),
    )
    .expect("started")
    .expect("credential globs");
    // Let the backend settle before the test acts.
    std::thread::sleep(Duration::from_millis(200));
    (watch, stopped)
}

fn within(limit: Duration, done: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    done()
}

#[test]
fn credential_files_created_meanwhile_stop_the_command() {
    for (folder, name) in [("", ".env"), ("app/deep", ".env.local"), ("x", ".ENVRC")] {
        let dir = tempfile::tempdir().unwrap();
        let work = root(&dir);
        let (watch, stopped) = watch(&work, &[]);
        std::fs::create_dir_all(work.join(folder)).unwrap();
        std::fs::write(work.join(folder).join(name), "API_KEY=dummy").unwrap();
        assert!(
            within(LATENCY, || stopped.load(Ordering::SeqCst)),
            "{folder}/{name}"
        );
        let found = watch.found().expect("found");
        assert!(found.ends_with(name), "{found}");
    }
}

#[test]
fn a_folder_moved_in_with_a_credential_file_stops_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let base = root(&dir);
    let work = base.join("work");
    std::fs::create_dir_all(base.join("outside/pkg")).unwrap();
    std::fs::create_dir(&work).unwrap();
    std::fs::write(base.join("outside/pkg/.env"), "API_KEY=dummy").unwrap();
    let (watch, stopped) = watch(&work, &[]);
    std::fs::rename(base.join("outside/pkg"), work.join("pkg")).unwrap();
    assert!(within(LATENCY, || stopped.load(Ordering::SeqCst)));
    assert!(watch.found().expect("found").ends_with(".env"));
}

/// A denied `.env` replaced by a new file (an editor's save by rename) has
/// the folder's ACL: it stops the command too.
#[test]
fn a_known_credential_file_replaced_stops_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let work = root(&dir);
    std::fs::write(work.join(".env"), "API_KEY=old").unwrap();
    let (watch, stopped) = watch(&work, &[work.join(".env")]);
    std::fs::write(work.join("save.tmp"), "API_KEY=new").unwrap();
    std::fs::rename(work.join("save.tmp"), work.join(".env")).unwrap();
    assert!(within(LATENCY, || stopped.load(Ordering::SeqCst)));
    assert!(watch.found().expect("found").ends_with(".env"));
}

/// Templates, other names and credential-named folders (a `.env`
/// virtualenv) do not stop the command.
#[test]
fn templates_other_names_and_folders_do_not_stop_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let work = root(&dir);
    let (watch, stopped) = watch(&work, &[]);
    for name in [
        ".env.example",
        ".ENV.SAMPLE",
        ".env.template",
        "env",
        "x.env",
        ".environment",
    ] {
        std::fs::write(work.join(name), "").unwrap();
    }
    std::fs::create_dir_all(work.join(".env/Scripts")).unwrap();
    std::fs::write(work.join(".env/Scripts/activate"), "").unwrap();
    std::fs::create_dir_all(work.join("app")).unwrap();
    std::fs::write(work.join("app/.Env.Template"), "").unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(!stopped.load(Ordering::SeqCst), "{:?}", watch.found());
    assert_eq!(watch.found(), None);
}

/// A credential file the deny list does not hold refuses the command; one it
/// holds (in any letter case on Windows) does not.
#[test]
fn an_unknown_credential_file_at_the_start_refuses_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let work = root(&dir);
    std::fs::create_dir(work.join("app")).unwrap();
    std::fs::write(work.join("app/.env.local"), "API_KEY=dummy").unwrap();
    std::fs::write(work.join(".env.example"), "").unwrap();
    let refused = CredentialWatch::start(&policy(&work), &work, &[], Box::new(|| {}));
    let Err(refusal) = refused else {
        panic!("not refused");
    };
    assert!(refusal.starts_with("Operation not permitted"), "{refusal}");
    assert!(refusal.contains(".env.local"), "{refusal}");
    assert!(refusal.contains("run it again"), "{refusal}");

    let known = [work.join("app/.env.local")];
    assert!(
        CredentialWatch::start(&policy(&work), &work, &known, Box::new(|| {}))
            .expect("started")
            .is_some()
    );
}

/// No credential globs, no watch.
#[test]
fn no_credential_globs_no_watch() {
    let dir = tempfile::tempdir().unwrap();
    let work = root(&dir);
    let none = FileSystemSandboxPolicy::restricted(Vec::new());
    assert!(
        CredentialWatch::start(&none, &work, &[], Box::new(|| {}))
            .expect("started")
            .is_none()
    );
    assert_eq!(credential_watch_roots(&none, &work), Vec::<PathBuf>::new());
    assert_eq!(credential_watch_roots(&policy(&work), &work), vec![work]);
}

/// End to end with a real process: the stop ends it within the latency.
#[test]
fn a_running_process_is_stopped() {
    let dir = tempfile::tempdir().unwrap();
    let work = root(&dir);
    let mut command = if cfg!(windows) {
        let mut command = std::process::Command::new("ping");
        command.args(["-n", "60", "127.0.0.1"]);
        command
    } else {
        let mut command = std::process::Command::new("sleep");
        command.arg("60");
        command
    };
    let child = Arc::new(Mutex::new(
        command.stdout(std::process::Stdio::null()).spawn().unwrap(),
    ));
    let to_kill = Arc::clone(&child);
    let watch = CredentialWatch::start(
        &policy(&work),
        &work,
        &[],
        Box::new(move || {
            let _ = to_kill.lock().unwrap().kill();
        }),
    )
    .unwrap()
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let started = Instant::now();
    std::fs::write(work.join(".env"), "API_KEY=dummy").unwrap();
    assert!(within(LATENCY, || {
        child.lock().unwrap().try_wait().unwrap().is_some()
    }));
    assert!(started.elapsed() < LATENCY);
    assert!(stopped_message(&watch.found().unwrap()).contains("appeared while it ran"));
}

/// A person's own `**/.env.*` glob alone (not the whole credential rule)
/// keeps covering the templates: one created meanwhile stops the command.
#[test]
fn own_env_star_glob_still_covers_templates() {
    let dir = tempfile::tempdir().unwrap();
    let work = root(&dir);
    let own = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry::new(
        FileSystemPath::GlobPattern {
            scan: None,
            pattern: format!(
                "{}{sep}**{sep}.env.*",
                work.to_string_lossy(),
                sep = std::path::MAIN_SEPARATOR
            ),
        },
        FileSystemAccessMode::Deny,
    )]);
    let stopped = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stopped);
    let watch = CredentialWatch::start(
        &own,
        &work,
        &[],
        Box::new(move || flag.store(true, Ordering::SeqCst)),
    )
    .unwrap()
    .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    std::fs::write(work.join(".env.example"), "").unwrap();
    assert!(within(LATENCY, || stopped.load(Ordering::SeqCst)));
    assert!(watch.found().unwrap().ends_with(".env.example"));
}

/// Names come from the policy: under a person's own `**/.env.*` alone, an
/// existing `.env` or `.envrc` (not denied by it) does not refuse commands.
#[test]
fn only_the_denied_patterns_count() {
    let dir = tempfile::tempdir().unwrap();
    let work = root(&dir);
    std::fs::write(work.join(".env"), "").unwrap();
    std::fs::write(work.join(".envrc"), "").unwrap();
    let own = FileSystemSandboxPolicy::restricted(vec![FileSystemSandboxEntry::new(
        FileSystemPath::GlobPattern {
            scan: None,
            pattern: format!(
                "{}{sep}**{sep}.env.*",
                work.to_string_lossy(),
                sep = std::path::MAIN_SEPARATOR
            ),
        },
        FileSystemAccessMode::Deny,
    )]);
    assert!(
        CredentialWatch::start(&own, &work, &[], Box::new(|| {}))
            .expect("not refused")
            .is_some()
    );
}

/// When changes were lost, everything is scanned again: a denied file that
/// is still the same file is fine; one saved over it, or a new credential
/// file, stops the command. A failed watch stops it too.
#[test]
fn lost_changes_rescan_by_identity_and_failures_stop() {
    let dir = tempfile::tempdir().unwrap();
    let work = root(&dir);
    std::fs::write(work.join(".env"), "API_KEY=old").unwrap();
    let names = Names::for_policy(&policy(&work), &work).unwrap();
    let known: HashMap<PathBuf, Option<FileId>> = [(
        key(&real_spelling(&work.join(".env"))),
        file_id(&work.join(".env")),
    )]
    .into_iter()
    .collect();
    assert_eq!(
        must_stop(Change::Lost, &known, &names, /*max_depth*/ None),
        None
    );
    std::fs::write(work.join("save.tmp"), "API_KEY=new").unwrap();
    std::fs::rename(work.join("save.tmp"), work.join(".env")).unwrap();
    assert!(
        must_stop(Change::Lost, &known, &names, /*max_depth*/ None)
            .expect("stopped")
            .ends_with(".env")
    );
    assert!(
        must_stop(
            Change::Failed("boom".to_string()),
            &known,
            &names,
            /*max_depth*/ None
        )
        .expect("stopped")
        .contains("the watch failed")
    );
}

/// The output of a stopped command says why and is a failure.
#[test]
fn stopped_output_reports_the_stop() {
    let mut stderr = b"partial".to_vec();
    assert_eq!(
        stopped_output(
            Some("C:/w/.env".to_string()),
            /*exit_code*/ 0,
            &mut stderr
        ),
        1
    );
    let text = String::from_utf8(stderr).unwrap();
    assert!(
        text.starts_with("partial\nOperation not permitted"),
        "{text}"
    );
    assert!(text.contains("(C:/w/.env) appeared while it ran"), "{text}");
    let mut untouched = b"out".to_vec();
    assert_eq!(
        stopped_output(/*found*/ None, /*exit_code*/ 0, &mut untouched),
        0
    );
    assert_eq!(untouched, b"out");
    assert_eq!(
        stopped_output(Some("x".to_string()), /*exit_code*/ 7, &mut Vec::new()),
        7
    );
}

/// On Windows paths match in any letter case: a denied file named with
/// another case is still known.
#[cfg(windows)]
#[test]
fn known_files_match_in_any_case() {
    let dir = tempfile::tempdir().unwrap();
    let work = root(&dir);
    std::fs::write(work.join(".env"), "").unwrap();
    let upper = PathBuf::from(work.to_string_lossy().to_uppercase()).join(".ENV");
    assert!(
        CredentialWatch::start(&policy(&work), &work, &[upper], Box::new(|| {}))
            .expect("not refused")
            .is_some()
    );
}

/// The change records Windows returns: created and renamed-to names are
/// read; a truncated record or an offset past the end counts as lost.
#[test]
fn notify_records_are_parsed_and_malformed_ones_are_lost() {
    fn record(next: u32, action: u32, name: &str) -> Vec<u8> {
        let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&next.to_le_bytes());
        bytes.extend_from_slice(&action.to_le_bytes());
        bytes.extend_from_slice(&u32::try_from(units.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(&units);
        while bytes.len() % 4 != 0 {
            bytes.push(0);
        }
        bytes
    }
    let first = record(/*next*/ 0, /*action*/ 1, "app\\.env");
    let mut both = record(
        u32::try_from(first.len()).unwrap(),
        /*action*/ 1,
        "app\\.env",
    );
    both.extend(record(/*next*/ 0, /*action*/ 5, ".envrc"));
    assert_eq!(
        parse_notify_entries(&both),
        Some(vec![
            (1, "app\\.env".to_string()),
            (5, ".envrc".to_string())
        ])
    );
    assert_eq!(parse_notify_entries(&first[..first.len() - 4]), None);
    assert_eq!(parse_notify_entries(&first[..6]), None);
    let past_end = record(/*next*/ 4096, /*action*/ 1, ".env");
    assert_eq!(parse_notify_entries(&past_end), None);
}

/// A file at a denied path that did not exist when it was denied (the
/// folder the sandbox made there was replaced) is new.
#[test]
fn a_file_at_a_denied_path_that_was_missing_is_new() {
    let dir = tempfile::tempdir().unwrap();
    let work = root(&dir);
    let names = Names::for_policy(&policy(&work), &work).unwrap();
    let known: HashMap<PathBuf, Option<FileId>> =
        [(key(&work.join(".env")), None)].into_iter().collect();
    std::fs::write(work.join(".env"), "API_KEY=dummy").unwrap();
    assert!(
        must_stop(Change::Lost, &known, &names, /*max_depth*/ None)
            .expect("stopped")
            .ends_with(".env")
    );
}

/// Nested folders: an outer folder's own `.env.*` glob keeps covering the
/// templates inside an inner folder under the whole rule, and the inner
/// folder's `.env` does not count in the outer folder.
#[test]
fn nested_folders_keep_their_own_patterns() {
    let dir = tempfile::tempdir().unwrap();
    let work = root(&dir);
    std::fs::create_dir(work.join("inner")).unwrap();
    let sep = std::path::MAIN_SEPARATOR;
    let mut entries = vec![FileSystemSandboxEntry::new(
        FileSystemPath::GlobPattern {
            scan: None,
            pattern: format!("{}{sep}**{sep}.env.*", work.to_string_lossy()),
        },
        FileSystemAccessMode::Deny,
    )];
    entries.extend(
        credential_file_globs(&work.join("inner").to_string_lossy())
            .into_iter()
            .map(|pattern| {
                FileSystemSandboxEntry::new(
                    FileSystemPath::GlobPattern {
                        pattern,
                        scan: None,
                    },
                    FileSystemAccessMode::Deny,
                )
            }),
    );
    let names = Names::for_policy(&FileSystemSandboxPolicy::restricted(entries), &work).unwrap();
    assert_eq!(names.roots, vec![work.clone()]);
    assert!(names.is_credential(&work.join("inner").join(".env.example")));
    assert!(names.is_credential(&work.join("inner").join(".env")));
    assert!(!names.is_credential(&work.join(".env")));
    assert!(names.is_credential(&work.join(".env.local")));
}

/// The deny list's depth counts from each covered folder, as the resolver
/// scans each glob from its own folder.
#[test]
fn depth_counts_from_each_covered_folder() {
    let dir = tempfile::tempdir().unwrap();
    let work = root(&dir);
    std::fs::create_dir_all(work.join("a").join("b").join("c")).unwrap();
    std::fs::create_dir_all(work.join("x").join("y")).unwrap();
    let sep = std::path::MAIN_SEPARATOR;
    let inner = work.join("a").join("b");
    let mut policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry::new(
            FileSystemPath::GlobPattern {
                scan: None,
                pattern: format!("{}{sep}**{sep}.env", work.to_string_lossy()),
            },
            FileSystemAccessMode::Deny,
        ),
        FileSystemSandboxEntry::new(
            FileSystemPath::GlobPattern {
                scan: None,
                pattern: format!("{}{sep}**{sep}.env.*", inner.to_string_lossy()),
            },
            FileSystemAccessMode::Deny,
        ),
    ]);
    policy.glob_scan_max_depth = Some(2);
    let names = Names::for_policy(&policy, &work).unwrap();
    std::fs::write(work.join("x").join("y").join(".env"), "").unwrap();
    assert_eq!(
        must_stop(Change::Lost, &HashMap::new(), &names, Some(2)),
        None
    );
    std::fs::write(inner.join("c").join(".env.local"), "").unwrap();
    assert!(
        must_stop(Change::Lost, &HashMap::new(), &names, Some(2))
            .expect("found")
            .ends_with(".env.local")
    );
}

/// Long (`\\?\`) and UNC spellings compare as the plain paths the scan
/// builds.
#[cfg(windows)]
#[test]
fn long_and_unc_paths_compare_plainly() {
    assert_eq!(key(Path::new(r"\\?\C:\A\B")), key(Path::new(r"c:\a\b")));
    assert_eq!(
        key(Path::new(r"\\?\UNC\srv\sh\x")),
        PathBuf::from(r"\\srv\sh\x")
    );
}

/// A denied `.env` more than 260 characters deep does not refuse commands.
#[cfg(windows)]
#[test]
fn a_deep_denied_file_is_known() {
    let dir = tempfile::tempdir().unwrap();
    let work = root(&dir);
    let mut deep = work.clone();
    for part in 0..6 {
        deep = deep.join(format!("{part}-{}", "d".repeat(48)));
    }
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(deep.join(".env"), "API_KEY=dummy").unwrap();
    assert!(deep.join(".env").to_string_lossy().len() > 260);
    assert!(
        CredentialWatch::start(&policy(&work), &work, &[deep.join(".env")], Box::new(|| {}))
            .expect("not refused")
            .is_some()
    );
}
