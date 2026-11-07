use super::*;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::credential_file_globs;
use pretty_assertions::assert_eq;
use std::time::Duration;
use std::time::Instant;

fn deny_globs(globs: Vec<String>) -> FileSystemSandboxPolicy {
    FileSystemSandboxPolicy::restricted(
        globs
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

fn root(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().canonicalize().unwrap()
}

/// A child that only waits to be stopped.
fn sleeper() -> std::process::Child {
    std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap()
}

/// Stops `child` the way the helper stops bubblewrap's supervisor.
fn killer(child: &std::process::Child) -> impl FnOnce() + Send + 'static {
    let pid = child.id() as libc::pid_t;
    // SAFETY: plain syscall; the test reaps `child` only afterwards.
    move || unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
}

/// Waits for `child` to exit, up to a few seconds; whether it was killed.
fn killed(child: &mut std::process::Child) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return !status.success();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}

#[test]
fn watch_roots_are_the_existing_outermost_credential_folders() {
    let dir = tempfile::tempdir().unwrap();
    let work = root(&dir);
    std::fs::create_dir_all(work.join("nested")).unwrap();
    let mut globs = credential_file_globs(&work.to_string_lossy());
    globs.extend(credential_file_globs(
        &work.join("nested").to_string_lossy(),
    ));
    globs.extend(credential_file_globs("/"));
    globs.extend(credential_file_globs(
        &work.join("missing").to_string_lossy(),
    ));
    globs.push(format!("{}/**/*.pem", work.display()));
    assert_eq!(
        credential_watch_roots(&deny_globs(globs), &work),
        vec![work.clone()]
    );
    assert_eq!(
        credential_watch_roots(&deny_globs(Vec::new()), &work),
        Vec::<PathBuf>::new()
    );
    assert!(
        CredentialWatcher::for_policy(&deny_globs(Vec::new()), &work)
            .unwrap()
            .is_none()
    );
}

/// A credential file created below the folder while the command runs, also
/// in a folder created after the watch started, stops the command.
#[test]
fn a_credential_file_created_meanwhile_stops_the_command() {
    for (folder, name) in [("", ".env"), ("app/deep", ".env.local"), ("x", ".envrc")] {
        let dir = tempfile::tempdir().unwrap();
        let work = root(&dir);
        let policy = deny_globs(credential_file_globs(&work.to_string_lossy()));
        let watcher = CredentialWatcher::for_policy(&policy, &work)
            .unwrap()
            .unwrap();
        let mut child = sleeper();
        let monitor = watcher.start(killer(&child), &[]);
        std::fs::create_dir_all(work.join(folder)).unwrap();
        std::fs::write(work.join(folder).join(name), "API_KEY=dummy").unwrap();
        assert!(killed(&mut child), "{folder}/{name}");
        let found = monitor.stop().expect("stopped");
        assert!(found.ends_with(name), "{found}");
    }
}

/// A folder moved in with a credential file inside stops the command too.
#[test]
fn a_folder_moved_in_with_a_credential_file_stops_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let work = root(&dir);
    std::fs::create_dir(work.join("in")).unwrap();
    let outside = tempfile::tempdir_in(work.join("..")).unwrap();
    std::fs::create_dir_all(outside.path().join("pkg")).unwrap();
    std::fs::write(outside.path().join("pkg/.env"), "API_KEY=dummy").unwrap();
    let policy = deny_globs(credential_file_globs(&work.join("in").to_string_lossy()));
    let watcher = CredentialWatcher::for_policy(&policy, &work)
        .unwrap()
        .unwrap();
    let mut child = sleeper();
    let monitor = watcher.start(killer(&child), &[]);
    std::fs::rename(outside.path().join("pkg"), work.join("in/pkg")).unwrap();
    assert!(killed(&mut child));
    assert!(monitor.stop().expect("stopped").ends_with("pkg/.env"));
}

/// Other names, and bubblewrap's own placeholders, do not stop it.
#[test]
fn other_names_and_placeholders_do_not_stop_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let work = root(&dir);
    let policy = deny_globs(credential_file_globs(&work.to_string_lossy()));
    let watcher = CredentialWatcher::for_policy(&policy, &work)
        .unwrap()
        .unwrap();
    let mut child = sleeper();
    let monitor = watcher.start(killer(&child), &[work.join(".env")]);
    for name in ["env", "x.env", ".environment", ".env"] {
        std::fs::write(work.join(name), "").unwrap();
    }
    std::fs::create_dir(work.join("sub")).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(child.try_wait().unwrap(), None);
    assert_eq!(monitor.stop(), None);
    child.kill().unwrap();
    child.wait().unwrap();
}

/// Existing credential files are reported in every letter case, so the
/// sandbox hides them (the glob expansion matches the exact case only).
#[test]
fn existing_credential_files_are_reported_in_any_case() {
    let dir = tempfile::tempdir().unwrap();
    let work = root(&dir);
    std::fs::create_dir_all(work.join("app")).unwrap();
    std::fs::write(work.join(".env.development"), "API_KEY=dummy").unwrap();
    std::fs::write(work.join("app/.ENV.Local"), "API_KEY=dummy").unwrap();
    let policy = deny_globs(credential_file_globs(&work.to_string_lossy()));
    let watcher = CredentialWatcher::for_policy(&policy, &work)
        .unwrap()
        .unwrap();
    let mut existing = watcher.existing_files().to_vec();
    existing.sort();
    assert_eq!(
        existing,
        vec![work.join(".env.development"), work.join("app/.ENV.Local")]
    );
}

/// Puts an unsafe credential entry in a folder.
type Plant = fn(&Path);

/// A credential name that is not a plain file refuses the command: what the
/// person writes to it would land where the command can read it.
#[test]
fn unsafe_credential_entries_refuse_the_command() {
    let cases: [(&str, Plant); 5] = [
        ("is a link", |work| {
            std::os::unix::fs::symlink("planted", work.join(".env")).unwrap();
        }),
        ("is a link", |work| {
            std::fs::create_dir(work.join("d")).unwrap();
            std::os::unix::fs::symlink("d", work.join(".env")).unwrap();
        }),
        ("is a link", |work| {
            std::fs::write(work.join(".env.real"), "").unwrap();
            std::os::unix::fs::symlink(".env.real", work.join(".env")).unwrap();
        }),
        ("has other hard links", |work| {
            std::fs::write(work.join("notes"), "").unwrap();
            std::fs::hard_link(work.join("notes"), work.join(".env.local")).unwrap();
        }),
        ("is not a regular file", |work| {
            let path = std::ffi::CString::new(work.join(".envrc").as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        }),
    ];
    for (reason, plant) in cases {
        let dir = tempfile::tempdir().unwrap();
        let work = root(&dir);
        std::fs::create_dir(work.join("app")).unwrap();
        plant(&work.join("app"));
        let policy = deny_globs(credential_file_globs(&work.to_string_lossy()));
        let Err(refusal) = CredentialWatcher::for_policy(&policy, &work) else {
            panic!("{reason}: not refused");
        };
        assert!(refusal.contains(reason), "{refusal}");
        assert!(refused_message(&refusal).starts_with("Operation not permitted"));
        // #442 (Travis, 2026-10-12): it names the entry and how to fix it.
        assert!(
            refusal.contains(&work.join("app").display().to_string()),
            "{refusal}"
        );
        assert!(
            refusal.contains("or move it out of the workspace"),
            "{refusal}"
        );
        if reason == "is a link" {
            assert!(
                refusal.contains("replace the link with a regular file"),
                "{refusal}"
            );
            assert!(refusal.contains(" to "), "{refusal}");
        }
    }
}

/// #442 (Travis, 2026-10-12): `.env.example`, `.env.sample` and
/// `.env.template` (any case) are not credential files: existing ones are
/// not hidden, links with those names do not refuse the command, and one
/// created while it runs does not stop it. Another credential name still does.
#[test]
fn credential_templates_are_neither_hidden_nor_stop_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let work = root(&dir);
    std::fs::write(work.join(".env.example"), "API_KEY=example").unwrap();
    std::fs::write(work.join(".ENV.SAMPLE"), "API_KEY=sample").unwrap();
    std::fs::write(work.join("notes"), "").unwrap();
    std::os::unix::fs::symlink("notes", work.join(".env.template")).unwrap();
    let policy = deny_globs(credential_file_globs(&work.to_string_lossy()));
    let watcher = CredentialWatcher::for_policy(&policy, &work)
        .unwrap()
        .unwrap();
    assert_eq!(watcher.existing_files(), &[] as &[std::path::PathBuf]);
    let mut child = sleeper();
    let monitor = watcher.start(killer(&child), &[]);
    std::fs::create_dir(work.join("app")).unwrap();
    std::fs::write(work.join("app/.Env.Template"), "").unwrap();
    std::fs::write(work.join(".env.sample"), "").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(child.try_wait().unwrap(), None);
    std::fs::write(work.join("app/.env.example.local"), "API_KEY=dummy").unwrap();
    assert!(killed(&mut child));
    assert_eq!(
        monitor.stop(),
        Some(work.join("app/.env.example.local").display().to_string())
    );

    // A person's own `**/.env.*` glob alone (not the whole rule) keeps
    // covering the templates: an existing one is hidden.
    std::fs::remove_file(work.join(".env.template")).unwrap();
    let own = deny_globs(vec![format!("{}/**/.env.*", work.display())]);
    let watcher = CredentialWatcher::for_policy(&own, &work).unwrap().unwrap();
    assert!(
        watcher
            .existing_files()
            .contains(&work.join(".env.example")),
        "{:?}",
        watcher.existing_files()
    );
}

/// Folders with a credential name (a `.env` virtualenv) and short-lived
/// build folders do not stop the command.
#[test]
fn credential_named_and_temporary_folders_do_not_stop_the_command() {
    let dir = tempfile::tempdir().unwrap();
    let work = root(&dir);
    let policy = deny_globs(credential_file_globs(&work.to_string_lossy()));
    let watcher = CredentialWatcher::for_policy(&policy, &work)
        .unwrap()
        .unwrap();
    let mut child = sleeper();
    let monitor = watcher.start(killer(&child), &[]);
    std::fs::create_dir_all(work.join(".env/bin")).unwrap();
    std::fs::write(work.join(".env/bin/activate"), "").unwrap();
    for round in 0..200 {
        let tmp = work.join(format!("tmp{round}"));
        std::fs::create_dir_all(tmp.join("a/b")).unwrap();
        std::fs::remove_dir_all(&tmp).unwrap();
    }
    std::fs::create_dir(work.join("old")).unwrap();
    std::fs::rename(work.join("old"), work.join("new")).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(child.try_wait().unwrap(), None);
    assert_eq!(monitor.stop(), None);
    // Removed folders no longer count toward the limit.
    let watcher = CredentialWatcher::for_policy(&policy, &work)
        .unwrap()
        .unwrap();
    let baseline = watcher.folders.len();
    let mut watcher = watcher;
    for round in 0..50 {
        std::fs::create_dir(work.join(format!("gone{round}"))).unwrap();
    }
    let mut buffer = [0_u8; 64 * 1024];
    let deadline = Instant::now() + Duration::from_secs(5);
    while watcher.folders.len() < baseline + 50 && Instant::now() < deadline {
        let read = unsafe {
            libc::read(
                watcher.fd.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        if let Ok(read) = usize::try_from(read) {
            assert_eq!(watcher.matches_in(&buffer[..read]), None);
        }
    }
    assert_eq!(watcher.folders.len(), baseline + 50);
    for round in 0..50 {
        std::fs::remove_dir(work.join(format!("gone{round}"))).unwrap();
    }
    while watcher.folders.len() > baseline && Instant::now() < deadline {
        let read = unsafe {
            libc::read(
                watcher.fd.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        if let Ok(read) = usize::try_from(read) {
            assert_eq!(watcher.matches_in(&buffer[..read]), None);
        }
    }
    assert_eq!(watcher.folders.len(), baseline);
    child.kill().unwrap();
    child.wait().unwrap();
}
