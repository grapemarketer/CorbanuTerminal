use super::*;
use crate::PolicyRootStore;
use codex_security_policy::BoundedText;
use codex_security_policy::PolicyPrincipal;
use codex_security_policy::PrincipalKind;
use pretty_assertions::assert_eq;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn owner() -> JournalOwner {
    JournalOwner::new(
        PolicyPrincipal::new(PrincipalKind::Service, "test-controller").unwrap(),
        /*owner_generation*/ 1,
        BoundedText::new("test-key").unwrap(),
    )
    .unwrap()
}

fn checkpoint(sequence: u64) -> IntegrityCheckpoint {
    let owner = owner();
    IntegrityCheckpoint {
        schema_version: 1,
        sequence,
        record_sha256: "a".repeat(64),
        producer: owner.producer().clone(),
        owner_generation: 1,
        integrity_key_id: owner.integrity_key_id().clone(),
        policy_generation: 1,
        run_generation: 1,
    }
}

/// Reopens a root this test just dropped. `ControllerRoot` holds an `flock`
/// on its lock file; a sibling test that forks (to spawn a child) while the
/// descriptor is open keeps a duplicate of it until the child execs, so the
/// lock can outlive `drop` for a moment and `open` reports `Conflict` (#1448).
/// Waits that out; any other result is returned as is.
fn reopen(
    registry: &std::path::Path,
    storage: &std::path::Path,
) -> Result<ControllerRoot, RootError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match ControllerRoot::open(registry, storage) {
            Err(RootError::Conflict) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            result => return result,
        }
    }
}

fn fixture() -> (tempfile::TempDir, ControllerRoot) {
    let temp = tempfile::tempdir().unwrap();
    for name in ["registry", "storage"] {
        fs::create_dir(temp.path().join(name)).unwrap();
        fs::set_permissions(temp.path().join(name), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let root = ControllerRoot::enroll(
        &temp.path().join("registry"),
        &temp.path().join("storage"),
        Enrollment::journal(&owner()).0,
    )
    .unwrap();
    (temp, root)
}

#[test]
fn pf20_s03_enrollment_cas_restart_and_wrong_namespace() {
    let (temp, root) = fixture();
    assert_eq!(IntegrityRootStore::load(&root), Ok(None));
    assert_eq!(root.load_policy(), Err(RootError::Invalid));
    let first = checkpoint(/*sequence*/ 1);
    root.compare_and_store(/*expected*/ None, &first).unwrap();
    assert_eq!(
        root.compare_and_store(/*expected*/ None, &checkpoint(/*sequence*/ 2)),
        Err(IntegrityRootError::Conflict)
    );
    let mut wrong = first.clone();
    wrong.record_sha256 = "b".repeat(64);
    assert_eq!(
        root.compare_and_store(Some(&wrong), &checkpoint(/*sequence*/ 2)),
        Err(IntegrityRootError::Conflict)
    );
    root.compare_and_store(Some(&first), &checkpoint(/*sequence*/ 2))
        .unwrap();
    assert_eq!(
        ControllerRoot::open(&temp.path().join("registry"), &temp.path().join("storage"))
            .unwrap_err(),
        RootError::Conflict
    );
    drop(root);
    let restarted = reopen(&temp.path().join("registry"), &temp.path().join("storage")).unwrap();
    assert_eq!(
        IntegrityRootStore::load(&restarted),
        Ok(Some(checkpoint(/*sequence*/ 2)))
    );
    assert_eq!(
        ControllerRoot::enroll(
            &temp.path().join("registry"),
            &temp.path().join("storage"),
            Enrollment::journal(&owner()).0
        )
        .unwrap_err(),
        RootError::Unavailable
    );
}

#[test]
fn pf20_s03_foreign_binding_generation_and_sequence_never_commit() {
    let (_temp, root) = fixture();
    let first = checkpoint(/*sequence*/ 1);
    root.compare_and_store(/*expected*/ None, &first).unwrap();
    let mut wrong = checkpoint(/*sequence*/ 2);
    wrong.owner_generation = 2;
    assert_eq!(
        root.compare_and_store(Some(&first), &wrong),
        Err(IntegrityRootError::Invalid)
    );
    wrong = checkpoint(/*sequence*/ 2);
    wrong.integrity_key_id = BoundedText::new("foreign").unwrap();
    assert_eq!(
        root.compare_and_store(Some(&first), &wrong),
        Err(IntegrityRootError::Invalid)
    );
    wrong = checkpoint(/*sequence*/ 2);
    wrong.run_generation = 0;
    assert_eq!(
        root.compare_and_store(Some(&first), &wrong),
        Err(IntegrityRootError::Invalid)
    );
    assert_eq!(
        root.compare_and_store(Some(&first), &checkpoint(/*sequence*/ 3)),
        Err(IntegrityRootError::Invalid)
    );
    assert_eq!(IntegrityRootStore::load(&root), Ok(Some(first)));
}

#[test]
fn pf20_s03_open_directory_permission_drift_latches_unavailable() {
    for name in ["registry", "storage"] {
        let (temp, root) = fixture();
        fs::set_permissions(temp.path().join(name), fs::Permissions::from_mode(0o750)).unwrap();
        assert!(IntegrityRootStore::load(&root).is_err(), "{name}");
        fs::set_permissions(temp.path().join(name), fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            IntegrityRootStore::load(&root).is_err(),
            "failure must latch"
        );
    }
}

#[test]
fn pf20_s03_successor_overflow_and_generation_regression_deny() {
    let binding = Enrollment::journal(&owner()).0;
    let old = Checkpoint::Journal(checkpoint(u64::MAX));
    let next = Checkpoint::Journal(checkpoint(/*sequence*/ 1));
    assert_eq!(
        next.validate_successor(Some(&old), &binding),
        Err(RootError::Invalid)
    );
    let mut old = checkpoint(/*sequence*/ 1);
    old.policy_generation = 3;
    old.run_generation = 4;
    for (policy, run) in [(2, 4), (3, 3)] {
        let mut next = checkpoint(/*sequence*/ 2);
        next.policy_generation = policy;
        next.run_generation = run;
        assert_eq!(
            Checkpoint::Journal(next)
                .validate_successor(Some(&Checkpoint::Journal(old.clone())), &binding),
            Err(RootError::Invalid)
        );
    }
}

#[test]
fn pf20_s03_non_regular_root_record_never_blocks_before_rejection() {
    use std::os::unix::ffi::OsStrExt;
    let (temp, root) = fixture();
    let path = temp.path().join("storage/head");
    fs::remove_file(&path).unwrap();
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: a valid NUL-terminated path inside this test's private directory.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(IntegrityRootStore::load(&root).is_err());
}

#[test]
fn pf20_s03_lost_corrupt_key_registry_head_and_partial_enrollment_deny() {
    for target in [
        "registry/enrollment",
        "registry/complete",
        "storage/key",
        "storage/head",
        "storage/lock",
    ] {
        let (temp, root) = fixture();
        drop(root);
        fs::remove_file(temp.path().join(target)).unwrap();
        assert!(
            reopen(&temp.path().join("registry"), &temp.path().join("storage")).is_err(),
            "{target}"
        );
    }
    for target in ["registry/complete", "storage/key", "storage/head"] {
        let (temp, root) = fixture();
        fs::write(temp.path().join(target), b"torn").unwrap();
        assert!(IntegrityRootStore::load(&root).is_err(), "{target}");
        drop(root);
        assert!(reopen(&temp.path().join("registry"), &temp.path().join("storage")).is_err());
    }
    let (temp, root) = fixture();
    drop(root);
    fs::remove_file(temp.path().join("registry/complete")).unwrap();
    assert!(
        ControllerRoot::enroll(
            &temp.path().join("registry"),
            &temp.path().join("storage"),
            Enrollment::journal(&owner()).0
        )
        .is_err()
    );
}

#[test]
fn pf20_s03_torn_pending_and_symlink_replacements_latch_unavailable() {
    let (temp, root) = fixture();
    fs::write(temp.path().join("storage/pending"), b"partial").unwrap();
    assert_eq!(
        IntegrityRootStore::load(&root),
        Err(IntegrityRootError::Timeout)
    );
    fs::remove_file(temp.path().join("storage/pending")).unwrap();
    assert_eq!(
        IntegrityRootStore::load(&root),
        Err(IntegrityRootError::Unavailable)
    );
    drop(root);
    let root = reopen(&temp.path().join("registry"), &temp.path().join("storage")).unwrap();
    fs::rename(
        temp.path().join("storage/head"),
        temp.path().join("storage/head-copy"),
    )
    .unwrap();
    std::os::unix::fs::symlink("head-copy", temp.path().join("storage/head")).unwrap();
    assert!(IntegrityRootStore::load(&root).is_err());
}

#[test]
fn pf20_s03_policy_anchor_retains_exact_existing_payload() {
    let (temp, old) = fixture();
    drop(old);
    let policy_registry = temp.path().join("policy-registry");
    let policy_storage = temp.path().join("policy-storage");
    for dir in [&policy_registry, &policy_storage] {
        fs::create_dir(dir).unwrap();
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let owner =
        AuthoritativeStateOwner::new("1".repeat(64), "controller", /*owner_generation*/ 1).unwrap();
    let root = ControllerRoot::enroll(
        &policy_registry,
        &policy_storage,
        Enrollment::policy(owner.clone()).unwrap().0,
    )
    .unwrap();
    assert_eq!(root.load_policy(), Ok(None));
    assert_eq!(
        IntegrityRootStore::load(&root),
        Err(IntegrityRootError::Invalid)
    );
    let value = PolicyCheckpoint {
        schema_version: 1,
        revision: 1,
        owner,
        state_sha256: "a".repeat(64),
        commit_sha256: "b".repeat(64),
    };
    root.compare_policy(/*expected*/ None, &value).unwrap();
    assert_eq!(root.load_policy(), Ok(Some(value.clone())));
    drop(root);
    assert_eq!(
        reopen(&policy_registry, &policy_storage)
            .unwrap()
            .load_policy(),
        Ok(Some(value))
    );
}

#[test]
fn pf20_s03_real_second_process_cannot_acquire_controller_lock() {
    let (temp, _root) = fixture();
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "store::tests::lock_child", "--ignored"])
        .env("CORBANU_ANCHOR_LOCK_FIXTURE", temp.path())
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
#[ignore = "invoked by real subprocess fixture"]
fn lock_child() {
    let path = std::path::PathBuf::from(std::env::var_os("CORBANU_ANCHOR_LOCK_FIXTURE").unwrap());
    assert_eq!(
        ControllerRoot::open(&path.join("registry"), &path.join("storage")).unwrap_err(),
        RootError::Conflict
    );
}

#[test]
fn pf20_s03_data_rollback_rejected_by_real_pf41_journal() {
    use codex_security_audit::JournalConfig;
    use codex_security_audit::ReferenceJournal;
    use codex_security_policy::RevocationState;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use std::sync::Arc;
    let (temp, root) = fixture();
    root.compare_and_store(/*expected*/ None, &checkpoint(/*sequence*/ 1))
        .unwrap();
    // An empty restored data directory cannot overrule the intact controller's
    // sequence-one root. This is real journal recovery, not a mocked consumer.
    let journal_path =
        AbsolutePathBuf::from_absolute_path_checked(temp.path().join("restored-journal")).unwrap();
    let mut journal = ReferenceJournal::new(
        journal_path,
        owner(),
        Arc::new(root),
        JournalConfig::default(),
    );
    let report = journal.recover(
        /*expected_policy_generation*/ 1,
        /*expected_run_generation*/ 1,
        &RevocationState::default(),
    );
    assert!(matches!(
        report.state,
        codex_security_audit::RecoveryState::Blocked(_)
    ));
}

#[test]
fn pf20_s03_every_failed_durability_boundary_withholds_and_latches() {
    use crate::linux::Fault;
    for fault in [
        Fault::NoSpace,
        Fault::ShortWrite,
        Fault::FileSync,
        Fault::DirectorySync,
        Fault::AfterDurable,
    ] {
        let (temp, root) = fixture();
        root.state.lock().unwrap().directory.fault.set(Some(fault));
        let error = if matches!(fault, Fault::DirectorySync | Fault::AfterDurable) {
            IntegrityRootError::Timeout
        } else {
            IntegrityRootError::Unavailable
        };
        assert_eq!(
            root.compare_and_store(/*expected*/ None, &checkpoint(/*sequence*/ 1)),
            Err(error)
        );
        assert_eq!(
            IntegrityRootStore::load(&root),
            Err(IntegrityRootError::Unavailable)
        );
        drop(root);
        let reopened = reopen(&temp.path().join("registry"), &temp.path().join("storage"));
        match fault {
            Fault::NoSpace => assert_eq!(IntegrityRootStore::load(&reopened.unwrap()), Ok(None)),
            Fault::ShortWrite | Fault::FileSync => {
                assert_eq!(reopened.unwrap_err(), RootError::Ambiguous)
            }
            // This is a process-restart observation, not physical power-loss
            // qualification: the kernel still retains the rename here.
            Fault::DirectorySync | Fault::AfterDurable => assert_eq!(
                IntegrityRootStore::load(&reopened.unwrap()),
                Ok(Some(checkpoint(/*sequence*/ 1)))
            ),
        }
    }
}
