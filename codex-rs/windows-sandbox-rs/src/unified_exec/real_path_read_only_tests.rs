//! #1780: a read-only folder inside the working folder stays read-only when
//! one of the two is spelled through a link (a junction or folder symbolic
//! link) and the other by its real path.

use super::*;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use pretty_assertions::assert_eq;

/// Makes `link` a junction (`/J`) or folder symbolic link (`/D`) to `target`.
pub(super) fn make_link(flag: &str, link: &Path, target: &Path) -> bool {
    std::process::Command::new(system32_exe("cmd.exe"))
        .args(["/d", "/c", "mklink", flag])
        .arg(link)
        .arg(target)
        .output()
        .is_ok_and(|out| out.status.success())
}

/// The issue's profile: the root readable, the project roots writable,
/// `docs` read-only.
fn profile_with_read_only(docs: &Path) -> PermissionProfile {
    PermissionProfile::from_runtime_permissions(
        &FileSystemSandboxPolicy::restricted(vec![
            FileSystemSandboxEntry::new(
                FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                FileSystemAccessMode::Read,
            ),
            FileSystemSandboxEntry::new(
                FileSystemPath::Special {
                    value: FileSystemSpecialPath::project_roots(/*subpath*/ None),
                },
                FileSystemAccessMode::Write,
            ),
            FileSystemSandboxEntry::new(
                FileSystemPath::Path {
                    path: AbsolutePathBuf::from_absolute_path(docs).expect("absolute"),
                },
                FileSystemAccessMode::Read,
            ),
        ]),
        NetworkSandboxPolicy::Restricted,
    )
}

/// In a fresh test root: `real\docs`, and `linked` a link of kind `flag` to
/// `real`. Runs a legacy command in the working folder `linked` (or `real`)
/// with `docs` read-only as spelled through `linked` (or by its real path),
/// writing a file in `real\docs` and one in the working folder. Returns
/// whether it started, `docs` was written and the working folder was, or
/// `None` when the link can't be made.
fn run_case(flag: &str, cwd_via_link: bool, docs_via_link: bool) -> Option<(bool, bool, bool)> {
    let test_root = user_owned_test_root();
    let base = dunce::canonicalize(test_root.path()).expect("canonical test root");
    let real = base.join("real");
    fs::create_dir_all(real.join("docs")).expect("create docs");
    let linked = base.join("linked");
    if !make_link(flag, &linked, &real) {
        return None;
    }
    let cwd = if cwd_via_link { &linked } else { &real };
    let docs = (if docs_via_link { &linked } else { &real }).join("docs");
    let probe = real.join("docs").join("probe.txt");
    let started = super::carve_out_link::run_script(
        &profile_with_read_only(&docs),
        cwd,
        &[],
        HashMap::new(),
        &format!(
            "echo x> \"{}\"\r\necho y> \"{}\"\r\n",
            probe.display(),
            cwd.join("ok.txt").display()
        ),
    );
    let outcome = (
        started.is_ok(),
        probe.exists(),
        real.join("ok.txt").exists(),
    );
    println!(
        "sec-win-1780 legacy: link {flag}, cwd via link={cwd_via_link}, docs via link={docs_via_link}: (started, docs written, workspace written)={outcome:?} {started:?}"
    );
    let _ = fs::remove_dir(&linked);
    Some(outcome)
}

/// The issue's case, and its reverse: the working folder and the read-only
/// folder inside it spelled one through a junction, the other by the real
/// path. The read-only folder used to be writable in the first.
#[test]
fn sec_win_1780_legacy_read_only_path_spelled_apart_from_cwd_stays_read_only() {
    let _guard = legacy_process_test_guard();
    let outcomes = [(true, false), (false, true)]
        .map(|(cwd_via_link, docs_via_link)| run_case("/J", cwd_via_link, docs_via_link));
    assert_eq!(outcomes, [Some((true, false, true)); 2]);
}

/// The spellings that were already protected stay so: both through the
/// junction, and a folder symbolic link either way. Making a symbolic link
/// needs a privilege a normal session may lack; the elevated CI run sets
/// `CORBANU_TEST_REQUIRE_SYMLINKS` so the symbolic link cases can't skip.
#[test]
fn sec_win_1780_legacy_read_only_path_stays_read_only_through_links() {
    let _guard = legacy_process_test_guard();
    assert_eq!(run_case("/J", true, true), Some((true, false, true)));
    for (cwd_via_link, docs_via_link) in [(true, false), (true, true), (false, true)] {
        let outcome = run_case("/D", cwd_via_link, docs_via_link);
        if outcome.is_none() && std::env::var_os("CORBANU_TEST_REQUIRE_SYMLINKS").is_none() {
            println!("sec-win-1780 legacy: skipped: no symbolic link privilege");
            continue;
        }
        assert_eq!(
            outcome,
            Some((true, false, true)),
            "symbolic link, cwd via link={cwd_via_link}, docs via link={docs_via_link}"
        );
    }
}
