use super::*;
use crate::models::PermissionProfile;
use crate::permissions::FileSystemAccessMode;
use crate::permissions::FileSystemSandboxEntry;
use crate::permissions::FileSystemSandboxPolicy;
use crate::permissions::NetworkSandboxPolicy;
use pretty_assertions::assert_eq;

#[test]
fn generated_and_identical_generic_globs_survive_profile_transport_and_merging() {
    let root =
        AbsolutePathBuf::from_absolute_path(std::env::temp_dir().join("database[1]*")).unwrap();
    let generated = FileSystemPath::database_entries(root.clone());
    let FileSystemPath::GlobPattern { pattern, .. } = &generated else {
        panic!("glob")
    };
    let generic = FileSystemPath::GlobPattern {
        pattern: pattern.clone(),
        scan: None,
    };
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry::new(generated, FileSystemAccessMode::Deny),
        FileSystemSandboxEntry::new(generic, FileSystemAccessMode::Deny),
    ]);
    let scoped = FileSystemSandboxPolicy::restricted(vec![policy.entries[0].clone()]);
    assert!(!scoped.is_semantically_equivalent_to(&policy, root.as_path()));
    let mut merged = FileSystemSandboxPolicy::unrestricted();
    merged.preserve_deny_read_restrictions_from(&policy);
    merged.preserve_deny_read_restrictions_from(&policy);
    let profile =
        PermissionProfile::from_runtime_permissions(&merged, NetworkSandboxPolicy::Restricted);
    let wire = serde_json::to_vec(&profile).unwrap();
    let decoded: PermissionProfile = serde_json::from_slice(&wire).unwrap();
    let transported = decoded
        .file_system_sandbox_policy()
        .materialize_project_roots_with_cwd(root.as_path())
        .materialize_project_roots_with_workspace_roots(std::slice::from_ref(&root));
    let scans = transported.get_unreadable_glob_scans_with_cwd(root.as_path());
    assert_eq!(scans.len(), 2);
    assert_eq!(
        scans,
        policy.get_unreadable_glob_scans_with_cwd(root.as_path())
    );
    assert_eq!(scans[0].database_entries_directory(), Ok(None));
    assert_eq!(scans[1].database_entries_directory(), Ok(Some(&root)));
}

#[test]
fn an_ordinary_glob_defaults_to_no_scan_hint_and_stays_wire_compatible() {
    let wire = serde_json::json!({"type": "glob_pattern", "pattern": "**/*.sqlite*"});
    let path: FileSystemPath = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(
        path,
        FileSystemPath::GlobPattern {
            pattern: "**/*.sqlite*".to_string(),
            scan: None
        }
    );
    assert_eq!(serde_json::to_value(path).unwrap(), wire);
}

#[test]
fn inconsistent_database_scan_metadata_fails_validation() {
    let directory = AbsolutePathBuf::from_absolute_path(std::env::temp_dir()).unwrap();
    let glob = FileSystemDenyGlob {
        pattern: "**/*.env".to_string(),
        scan: Some(FileSystemGlobScan::DatabaseEntries { directory }),
    };
    assert!(glob.database_entries_directory().is_err());
}
