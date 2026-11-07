// Independent stored-ZIP encoder driving the actual consumer entry point.
use pretty_assertions::assert_eq;

fn extract(bytes: &[u8], destination: &std::path::Path) -> Result<(), String> {
    super::extract_zipball_to_dir(bytes, destination)
}

fn archive(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut central = Vec::new();
    for (name, data, mode) in entries {
        let offset = bytes.len() as u32;
        let mut crc = !0u32;
        for byte in *data {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
            }
        }
        crc = !crc;
        bytes.extend(0x0403_4b50u32.to_le_bytes());
        for word in [20u16, 0, 0, 0, 0x21] {
            bytes.extend(word.to_le_bytes());
        }
        for word in [crc, data.len() as u32, data.len() as u32] {
            bytes.extend(word.to_le_bytes());
        }
        bytes.extend((name.len() as u16).to_le_bytes());
        bytes.extend(0u16.to_le_bytes());
        bytes.extend(name.as_bytes());
        bytes.extend(*data);
        central.extend(0x0201_4b50u32.to_le_bytes());
        for word in [0x0314u16, 20, 0, 0, 0, 0x21] {
            central.extend(word.to_le_bytes());
        }
        for word in [crc, data.len() as u32, data.len() as u32] {
            central.extend(word.to_le_bytes());
        }
        for word in [name.len() as u16, 0, 0, 0, 0] {
            central.extend(word.to_le_bytes());
        }
        central.extend((mode << 16).to_le_bytes());
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let start = bytes.len() as u32;
    let size = central.len() as u32;
    bytes.extend(central);
    bytes.extend(0x0605_4b50u32.to_le_bytes());
    for word in [0u16, 0, entries.len() as u16, entries.len() as u16] {
        bytes.extend(word.to_le_bytes());
    }
    bytes.extend(size.to_le_bytes());
    bytes.extend(start.to_le_bytes());
    bytes.extend(0u16.to_le_bytes());
    bytes
}

#[test]
fn archive_migration_valid_nested_regular_files() {
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("destination");
    std::fs::create_dir(&dest).unwrap();
    let bytes = archive(&[
        ("bundle/nested/hello.txt", b"hello\n", 0o100755),
        ("bundle/empty", b"", 0o100644),
        ("bundle/nested/binary", b"\0\xff\x01", 0o100644),
        ("bundle/empty-dir/", b"", 0o40755),
    ]);
    extract(&bytes, &dest).unwrap();
    assert_eq!(
        std::fs::read(dest.join("nested/hello.txt")).unwrap(),
        b"hello\n"
    );
    assert_eq!(std::fs::read(dest.join("empty")).unwrap(), b"");
    assert_eq!(
        std::fs::read(dest.join("nested/binary")).unwrap(),
        b"\0\xff\x01"
    );
    println!("ZIP_CASE=valid_nested_regular_files; regular bytes match");
}

#[test]
fn archive_migration_unsafe_member_names() {
    for relative in [
        "bundle/../../outside/sentinel",
        "bundle/nested/../../../outside/sentinel",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("destination");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&dest).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("sentinel"), b"unchanged").unwrap();
        assert!(extract(&archive(&[(relative, b"bad", 0o100644)]), &dest).is_err());
        assert_eq!(
            std::fs::read(outside.join("sentinel")).unwrap(),
            b"unchanged"
        );
        println!("ZIP_CASE=parent_traversal; rejected; outside unchanged");
    }
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("destination");
    std::fs::create_dir(&dest).unwrap();
    let outside = temp.path().join("sentinel");
    std::fs::write(&outside, b"unchanged").unwrap();
    assert!(
        extract(
            &archive(&[(outside.to_str().unwrap(), b"bad", 0o100644)]),
            &dest
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&outside).unwrap(), b"unchanged");
    println!("ZIP_CASE=absolute_owned_path; rejected; outside unchanged");
    for rooted in [
        "/abs/x",
        "\\abs\\x",
        "C:\\abs\\x",
        "C:abs\\x",
        "\\\\server\\share\\x",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("destination");
        std::fs::create_dir(&dest).unwrap();
        assert!(
            extract(&archive(&[(rooted, b"bad", 0o100644)]), &dest).is_err(),
            "{rooted}"
        );
    }
}

#[test]
fn archive_migration_archive_symlink_order_and_chain() {
    for order in [0, 1, 2] {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("destination");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&dest).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("sentinel"), b"unchanged").unwrap();
        let target = outside.to_str().unwrap().as_bytes();
        let entries = match order {
            0 => vec![
                ("bundle/link", target, 0o120777),
                ("bundle/link/sentinel", b"bad".as_slice(), 0o100644),
            ],
            1 => vec![
                ("bundle/link/sentinel", b"bad".as_slice(), 0o100644),
                ("bundle/link", target, 0o120777),
            ],
            _ => vec![
                ("bundle/link", b"next".as_slice(), 0o120777),
                ("bundle/next", target, 0o120777),
                ("bundle/link/sentinel", b"bad".as_slice(), 0o100644),
            ],
        };
        let result = extract(&archive(&entries), &dest);
        assert_eq!(
            std::fs::read(outside.join("sentinel")).unwrap(),
            b"unchanged"
        );
        if let Ok(metadata) = std::fs::symlink_metadata(dest.join("link")) {
            assert!(!metadata.file_type().is_symlink());
        }
        println!(
            "ZIP_CASE=archive_symlink_order_{order}; consumer_ok={}; outside unchanged; no materialized symlink",
            result.is_ok()
        );
    }
}

#[cfg(unix)]
fn check_preexisting_link(parent_link: bool) {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("destination");
    let outside = temp.path().join("outside");
    std::fs::create_dir(&dest).unwrap();
    std::fs::create_dir(&outside).unwrap();
    let sentinel = outside.join("sentinel");
    std::fs::write(&sentinel, b"unchanged").unwrap();
    std::fs::set_permissions(&sentinel, std::fs::Permissions::from_mode(0o640)).unwrap();
    let name = if parent_link {
        std::os::unix::fs::symlink(&outside, dest.join("link")).unwrap();
        "bundle/link/sentinel"
    } else {
        std::os::unix::fs::symlink(&sentinel, dest.join("sentinel")).unwrap();
        "bundle/sentinel"
    };
    let result = extract(&archive(&[(name, b"bad", 0o106777)]), &dest);
    let bytes = std::fs::read(&sentinel).unwrap();
    let mode = std::fs::metadata(&sentinel).unwrap().permissions().mode() & 0o7777;
    println!(
        "ZIP_CASE=preexisting_link; parent={parent_link}; consumer_ok={}; outside_mode={mode:o}; outside_unchanged={}",
        result.is_ok(),
        bytes == b"unchanged"
    );
    assert_eq!((bytes, mode), (b"unchanged".to_vec(), 0o640));
}

#[cfg(unix)]
#[test]
fn archive_migration_preexisting_parent_symlink_does_not_escape() {
    check_preexisting_link(/*parent_link*/ true);
}

#[cfg(unix)]
#[test]
fn archive_migration_preexisting_file_symlink_does_not_escape() {
    check_preexisting_link(/*parent_link*/ false);
}

#[cfg(unix)]
#[test]
fn archive_migration_privileged_modes_are_not_granted() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("destination");
    std::fs::create_dir(&dest).unwrap();
    extract(
        &archive(&[
            ("bundle/tool", b"fixture, never executed", 0o106755),
            ("bundle/dir/", b"", 0o42755),
        ]),
        &dest,
    )
    .unwrap();
    let mode = std::fs::metadata(dest.join("tool"))
        .unwrap()
        .permissions()
        .mode()
        & 0o7777;
    println!("ZIP_CASE=privileged_modes; tool_mode={mode:o}; no fixture executes");
    assert_eq!(
        mode & 0o777,
        0o755,
        "ordinary executable permissions preserved"
    );
    assert_eq!(mode & 0o6000, 0);
    if dest.join("dir").exists() {
        assert_eq!(
            std::fs::metadata(dest.join("dir"))
                .unwrap()
                .permissions()
                .mode()
                & 0o6000,
            0
        );
    }
}

#[test]
fn archive_migration_invalid_truncated_and_corrupt_fail() {
    let name = "bundle/file";
    let valid = archive(&[(name, b"payload", 0o100644)]);
    let mut corrupt = valid.clone();
    corrupt[30 + name.len()] ^= 0xff;
    let payload_start = 30 + name.len();
    let payload_cut = payload_start + b"payload".len() / 2;
    let cases = [
        ("empty", Vec::new()),
        ("invalid", b"not a ZIP".to_vec()),
        ("header_truncated", valid[..33].to_vec()),
        ("payload_truncated", valid[..payload_cut].to_vec()),
        ("directory_truncated", valid[..valid.len() - 12].to_vec()),
        ("corrupt_payload", corrupt),
    ];
    for (case, bytes) in &cases {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("destination");
        std::fs::create_dir(&dest).unwrap();
        let result = extract(bytes, &dest);
        println!(
            "ZIP_CASE=invalid_truncated_corrupt_{case}; consumer_ok={}",
            result.is_ok()
        );
        assert!(result.is_err());
    }
}

#[test]
fn archive_migration_file_directory_conflict_propagates() {
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("destination");
    std::fs::create_dir(&dest).unwrap();
    std::fs::write(dest.join("blocked"), b"existing").unwrap();
    let result = extract(
        &archive(&[
            ("bundle/first", b"partial", 0o100644),
            ("bundle/blocked/child", b"bad", 0o100644),
        ]),
        &dest,
    );
    assert!(result.is_err());
    assert_eq!(std::fs::read(dest.join("blocked")).unwrap(), b"existing");
    println!(
        "ZIP_CASE=consumer_failure; error propagates; existing owned sentinel unchanged; partial direct-helper outputs allowed"
    );
}

#[test]
fn archive_migration_bzip2_regular_file() {
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("destination");
    std::fs::create_dir(&dest).unwrap();
    // Independently precompressed method-12 ZIP; no Rust zip writer roundtrip.
    let bytes = include_bytes!("fixtures/archive-bzip2.zip");
    extract(bytes, &dest).unwrap();
    assert_eq!(
        std::fs::read(dest.join("nested/bzip2.bin")).unwrap(),
        b"owned synthetic bzip2 archive fixture\n\0\xff\x01"
    );
    println!("ZIP_CASE=bzip2_regular_file; method12; exact owned synthetic payload");
}

#[test]
fn archive_migration_symlink_members_are_skipped() {
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("destination");
    let outside = temp.path().join("outside");
    std::fs::create_dir(&dest).unwrap();
    std::fs::write(&outside, b"unchanged").unwrap();
    let bytes = archive(&[
        ("bundle/link", b"../outside", 0o120777),
        ("bundle/regular", b"../outside", 0o100755),
    ]);
    extract(&bytes, &dest).unwrap();
    assert_eq!(std::fs::read(dest.join("regular")).unwrap(), b"../outside");
    assert_eq!(std::fs::read(&outside).unwrap(), b"unchanged");
    assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 1);
    assert!(std::fs::symlink_metadata(dest.join("link")).is_err());
    println!("ZIP_CASE=symlink_member_skipped; no link payload output; regular file preserved");
}

#[test]
fn archive_migration_unsafe_symlink_member_names_are_rejected() {
    for name in ["bundle/../../outside", "/bundle/link"] {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("destination");
        std::fs::create_dir(&dest).unwrap();
        assert!(extract(&archive(&[(name, b"target", 0o120777)]), &dest).is_err());
        assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0);
    }
}

#[test]
fn archive_migration_disabled_lzma_is_rejected_without_output() {
    let temp = tempfile::tempdir().unwrap();
    let dest = temp.path().join("destination");
    let outside = temp.path().join("outside");
    std::fs::create_dir(&dest).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("sentinel"), b"unchanged").unwrap();
    // Valid method-14 ZIP generated and independently decoded with Python zipfile.
    let bytes = include_bytes!("fixtures/archive-lzma.zip");
    let result = extract(bytes, &dest);
    assert!(result.is_err(), "LZMA must remain disabled: {result:?}");
    assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 2);
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    assert_eq!(
        std::fs::read(outside.join("sentinel")).unwrap(),
        b"unchanged"
    );
    println!("ZIP_CASE=disabled_lzma; method14; rejected; no destination or outside writes");
}
