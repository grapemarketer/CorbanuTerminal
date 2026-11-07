use codex_file_system::ExecFileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_utils_path_uri::PathUri;
use pretty_assertions::assert_eq;
use std::io;

fn database_entry_wire(directory: &str, pattern: &str) -> serde_json::Value {
    serde_json::json!({
        "path": {
            "type": "glob_pattern",
            "pattern": pattern,
            "scan": {"type": "database_entries", "directory": directory}
        },
        "access": "deny"
    })
}

#[test]
fn database_scan_wire_forwards_windows_and_posix_directories_without_native_conversion() {
    for (directory, pattern) in [
        ("file:///C:/database", r"C:\database\*.sqlite*"),
        ("file:///tmp/database", "/tmp/database/*.sqlite*"),
    ] {
        let wire = database_entry_wire(directory, pattern);
        let decoded: ExecFileSystemSandboxEntry = serde_json::from_value(wire.clone()).unwrap();
        let forwarded = serde_json::to_value(decoded).unwrap();
        assert_eq!(forwarded, wire);
    }
}

#[test]
fn database_scan_native_conversion_accepts_only_the_receiving_hosts_path_convention() {
    for (directory, pattern, native) in [
        (
            "file:///C:/database",
            r"C:\database\*.sqlite*",
            cfg!(windows),
        ),
        (
            "file:///tmp/database",
            "/tmp/database/*.sqlite*",
            cfg!(unix),
        ),
    ] {
        let wire = database_entry_wire(directory, pattern);
        let decoded: ExecFileSystemSandboxEntry = serde_json::from_value(wire.clone()).unwrap();
        let converted = FileSystemSandboxEntry::try_from(decoded);
        if native {
            let expected = FileSystemSandboxEntry::new(
                FileSystemPath::database_entries(
                    PathUri::parse(directory).unwrap().to_abs_path().unwrap(),
                ),
                FileSystemAccessMode::Deny,
            );
            assert_eq!(converted.unwrap(), expected);
            assert_eq!(
                serde_json::to_value(ExecFileSystemSandboxEntry::from(expected)).unwrap(),
                wire
            );
        } else {
            assert_eq!(converted.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        }
    }
}

#[test]
fn ordinary_exec_glob_without_scan_remains_opaque_on_every_host() {
    for pattern in [r"C:\database\*.sqlite*", "/tmp/database/*.sqlite*"] {
        let wire = serde_json::json!({
            "path": {"type": "glob_pattern", "pattern": pattern},
            "access": "deny"
        });
        let decoded: ExecFileSystemSandboxEntry = serde_json::from_value(wire.clone()).unwrap();
        let expected = FileSystemSandboxEntry::new(
            FileSystemPath::GlobPattern {
                pattern: pattern.to_string(),
                scan: None,
            },
            FileSystemAccessMode::Deny,
        );
        assert_eq!(FileSystemSandboxEntry::try_from(decoded).unwrap(), expected);
        assert_eq!(
            serde_json::to_value(ExecFileSystemSandboxEntry::from(expected)).unwrap(),
            wire
        );
    }
}
