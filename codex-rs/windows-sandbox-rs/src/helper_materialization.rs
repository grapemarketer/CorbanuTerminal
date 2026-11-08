use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;
use tempfile::NamedTempFile;

use crate::logging::log_note;
use crate::sandbox_bin_dir;

const DEV_BUILD_VERSION_SENTINEL: &str = "0.0.0";
pub(crate) const BIN_DIRNAME: &str = "bin";
pub(crate) const RESOURCES_DIRNAME: &str = "codex-resources";

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum HelperExecutable {
    CommandRunner,
}

impl HelperExecutable {
    fn file_name(self) -> &'static str {
        match self {
            Self::CommandRunner => "codex-command-runner.exe",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::CommandRunner => "command-runner",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CopyOutcome {
    Reused,
    ReCopied,
}

static HELPER_PATH_CACHE: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();

pub(crate) fn helper_bin_dir(codex_home: &Path) -> PathBuf {
    sandbox_bin_dir(codex_home)
}

pub(crate) fn legacy_lookup(kind: HelperExecutable) -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(candidate) = bundled_executable_path_for_exe(&exe, kind.file_name())
    {
        return candidate;
    }
    PathBuf::from(kind.file_name())
}

pub(crate) fn resolve_helper_for_launch(
    kind: HelperExecutable,
    codex_home: &Path,
    log_dir: Option<&Path>,
) -> PathBuf {
    match copy_helper_if_needed(kind, codex_home, log_dir) {
        Ok(path) => {
            keep_helper_bin_dir_medium(codex_home, log_dir);
            log_note(
                &format!(
                    "helper launch resolution: using copied {} path {}",
                    kind.label(),
                    path.display()
                ),
                log_dir,
            );
            path
        }
        Err(err) => {
            let fallback = legacy_lookup(kind);
            log_note(
                &format!(
                    "helper copy failed for {}: {err:#}; falling back to legacy path {}",
                    kind.label(),
                    fallback.display()
                ),
                log_dir,
            );
            keep_fallback_helper_medium(&fallback, log_dir);
            fallback
        }
    }
}

/// #1819: an executable starts at the lower of its token's and its file's
/// integrity. While a legacy session uses a writable root holding this codex
/// home (`%TEMP%`, or a workspace above it), the root's inheritable Low label
/// reaches the copied helpers, so the runner started at Low and could not
/// open the parent's pipe for writing. An explicit Medium label (what an
/// unlabelled file has anyway) keeps the root's Low label off them, and Low
/// programs out of them. A link is skipped: its label would land on the link
/// itself, not on the folder it points at.
fn keep_helper_bin_dir_medium(codex_home: &Path, log_dir: Option<&Path>) {
    let dir = helper_bin_dir(codex_home);
    if crate::workspace_acl::is_link(&dir) {
        log_note(
            &format!(
                "helper copy: {} is a link (or can't be inspected); not labelling it Medium, so a legacy session's Low label may reach the runner (#1819)",
                dir.display()
            ),
            log_dir,
        );
        return;
    }
    // SAFETY: Win32 security calls on a folder of this codex home.
    if let Err(err) = unsafe { crate::integrity::label_protected_path_medium(&dir) } {
        log_note(
            &format!(
                "helper copy: could not label {} Medium; a legacy session's Low label may reach it: {err:#}",
                dir.display()
            ),
            log_dir,
        );
    }
}

/// #1819, for the runner next to `codex.exe` used when copying failed (say a
/// dev build inside a legacy workspace): if its label is Low, most likely
/// inherited from a folder a legacy session made writable, it would start at
/// Low and time out opening the parent's pipes. Only that file (not its
/// folder) gets an explicit Medium label; a link is left alone.
fn keep_fallback_helper_medium(fallback: &Path, log_dir: Option<&Path>) {
    // A bare name is found on `PATH` at launch.
    if !fallback.is_absolute() || !fallback.exists() {
        return;
    }
    let cause = "the likely cause is a Low label inherited from a folder a legacy session made writable (#1819)";
    if crate::workspace_acl::is_link(fallback) {
        log_note(
            &format!(
                "helper fallback: {} is a link; not checking its label. If the runner times out connecting its pipes, {cause}",
                fallback.display()
            ),
            log_dir,
        );
        return;
    }
    // SAFETY: reads the label of the fallback runner file.
    let note = match unsafe { crate::integrity::effective_label_is_low(fallback) } {
        Ok(false) => return,
        // SAFETY: labels only the fallback runner file, which is no link.
        Ok(true) => match unsafe { crate::integrity::label_protected_path_medium(fallback) } {
            Ok(()) => format!(
                "helper fallback: {} was labelled Low; labelled it Medium so the runner can open its pipes ({cause})",
                fallback.display()
            ),
            Err(err) => format!(
                "helper fallback: {} is labelled Low and could not be labelled Medium ({err:#}); the runner will start at Low and time out connecting its pipes ({cause})",
                fallback.display()
            ),
        },
        Err(err) => format!(
            "helper fallback: could not read the label of {} ({err:#}); if the runner times out connecting its pipes, {cause}",
            fallback.display()
        ),
    };
    log_note(&note, log_dir);
}

pub fn resolve_current_exe_for_launch(codex_home: &Path, fallback_executable: &str) -> PathBuf {
    let source = match std::env::current_exe() {
        Ok(path) => path,
        Err(_) => return PathBuf::from(fallback_executable),
    };
    resolve_exe_for_launch(&source, codex_home)
}

pub fn resolve_exe_for_launch(source: &Path, codex_home: &Path) -> PathBuf {
    let Some(file_name) = source.file_name() else {
        return source.to_path_buf();
    };
    let destination = helper_bin_dir(codex_home).join(file_name);
    match copy_from_source_if_needed(source, &destination) {
        Ok(_) => destination,
        Err(err) => {
            let sandbox_log_dir = crate::sandbox_dir(codex_home);
            log_note(
                &format!(
                    "helper copy failed for executable: {err:#}; falling back to legacy path {}",
                    source.display()
                ),
                Some(&sandbox_log_dir),
            );
            source.to_path_buf()
        }
    }
}

pub(crate) fn copy_helper_if_needed(
    kind: HelperExecutable,
    codex_home: &Path,
    log_dir: Option<&Path>,
) -> Result<PathBuf> {
    let cache_key = format!("{}|{}", kind.file_name(), codex_home.display());
    if let Some(path) = cached_helper_path(&cache_key) {
        log_note(
            &format!(
                "helper copy: using in-memory cache for {} -> {}",
                kind.label(),
                path.display()
            ),
            log_dir,
        );
        return Ok(path);
    }

    let source = sibling_source_path(kind)?;
    let destination = helper_destination_for_source(kind, codex_home, &source)?;
    log_note(
        &format!(
            "helper copy: validating {} source={} destination={}",
            kind.label(),
            source.display(),
            destination.display()
        ),
        log_dir,
    );
    let outcome = copy_from_source_if_needed(&source, &destination)?;
    let action = match outcome {
        CopyOutcome::Reused => "reused",
        CopyOutcome::ReCopied => "recopied",
    };
    log_note(
        &format!(
            "helper copy: {} {} source={} destination={}",
            action,
            kind.label(),
            source.display(),
            destination.display()
        ),
        log_dir,
    );
    store_helper_path(cache_key, destination.clone());
    Ok(destination)
}

fn cached_helper_path(cache_key: &str) -> Option<PathBuf> {
    let cache = HELPER_PATH_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let guard = cache.lock().ok()?;
    guard.get(cache_key).cloned()
}

fn store_helper_path(cache_key: String, path: PathBuf) {
    let cache = HELPER_PATH_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(mut guard) = cache.lock() {
        guard.insert(cache_key, path);
    }
}

fn sibling_source_path(kind: HelperExecutable) -> Result<PathBuf> {
    let exe = std::env::current_exe().context("resolve current executable for helper lookup")?;
    bundled_executable_path_for_exe(&exe, kind.file_name()).ok_or_else(|| {
        anyhow!(
            "helper not found next to current executable or under {RESOURCES_DIRNAME}: {}",
            exe.display()
        )
    })
}

pub(crate) fn bundled_executable_path_for_exe(exe: &Path, file_name: &str) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let direct_candidate = dir.join(file_name);
    if direct_candidate.is_file() {
        return Some(direct_candidate);
    }

    if dir.file_name() == Some(OsStr::new(BIN_DIRNAME))
        && let Some(package_dir) = dir.parent()
    {
        let package_resource_candidate = package_dir.join(RESOURCES_DIRNAME).join(file_name);
        if package_resource_candidate.is_file() {
            return Some(package_resource_candidate);
        }
    }

    let resource_candidate = dir.join(RESOURCES_DIRNAME).join(file_name);
    resource_candidate.is_file().then_some(resource_candidate)
}

fn helper_destination_for_source(
    kind: HelperExecutable,
    codex_home: &Path,
    source: &Path,
) -> Result<PathBuf> {
    let suffix = helper_version_suffix(source)?;
    let file_name = materialized_file_name(kind, &suffix);
    Ok(helper_bin_dir(codex_home).join(file_name))
}

fn materialized_file_name(kind: HelperExecutable, suffix: &str) -> String {
    let source_name = kind.file_name();
    let path = Path::new(source_name);
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(source_name);
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| format!(".{ext}"))
        .unwrap_or_default();
    format!("{stem}-{suffix}{extension}")
}

fn helper_version_suffix(source: &Path) -> Result<String> {
    let version = env!("CARGO_PKG_VERSION");
    if version == DEV_BUILD_VERSION_SENTINEL {
        dev_build_suffix(source)
    } else {
        Ok(version.to_string())
    }
}

fn dev_build_suffix(source: &Path) -> Result<String> {
    let metadata = fs::metadata(source)
        .with_context(|| format!("read helper source metadata {}", source.display()))?;
    let modified = metadata
        .modified()
        .with_context(|| format!("read helper source mtime {}", source.display()))?;
    let duration = modified
        .duration_since(UNIX_EPOCH)
        .with_context(|| format!("convert helper source mtime {}", source.display()))?;
    Ok(format!("{}-{:x}", metadata.len(), duration.as_secs(),))
}

fn copy_from_source_if_needed(source: &Path, destination: &Path) -> Result<CopyOutcome> {
    if destination_is_fresh(source, destination)? {
        return Ok(CopyOutcome::Reused);
    }

    let destination_dir = destination.parent().ok_or_else(|| {
        anyhow!(
            "helper destination has no parent: {}",
            destination.display()
        )
    })?;
    fs::create_dir_all(destination_dir).with_context(|| {
        format!(
            "create helper destination directory {}",
            destination_dir.display()
        )
    })?;

    let temp_path = NamedTempFile::new_in(destination_dir)
        .with_context(|| {
            format!(
                "create temporary helper file in {}",
                destination_dir.display()
            )
        })?
        .into_temp_path();
    let temp_path_buf = temp_path.to_path_buf();

    let mut source_file = fs::File::open(source)
        .with_context(|| format!("open helper source for read {}", source.display()))?;
    let mut temp_file = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&temp_path_buf)
        .with_context(|| format!("open temporary helper file {}", temp_path_buf.display()))?;

    // Write into a temp file created inside `.sandbox-bin` so the copied helper keeps the
    // destination directory's inherited ACLs instead of reusing the source file's descriptor.
    std::io::copy(&mut source_file, &mut temp_file).with_context(|| {
        format!(
            "copy helper from {} to {}",
            source.display(),
            temp_path_buf.display()
        )
    })?;
    temp_file
        .flush()
        .with_context(|| format!("flush temporary helper file {}", temp_path_buf.display()))?;
    drop(temp_file);

    if destination.exists() {
        fs::remove_file(destination).with_context(|| {
            format!("remove stale helper destination {}", destination.display())
        })?;
    }

    match fs::rename(&temp_path_buf, destination) {
        Ok(()) => Ok(CopyOutcome::ReCopied),
        Err(rename_err) => {
            if destination_is_fresh(source, destination)? {
                Ok(CopyOutcome::Reused)
            } else {
                Err(rename_err).with_context(|| {
                    format!(
                        "rename helper temp file {} to {}",
                        temp_path_buf.display(),
                        destination.display()
                    )
                })
            }
        }
    }
}

fn destination_is_fresh(source: &Path, destination: &Path) -> Result<bool> {
    let source_meta = fs::metadata(source)
        .with_context(|| format!("read helper source metadata {}", source.display()))?;
    let destination_meta = match fs::metadata(destination) {
        Ok(meta) => meta,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => {
            return Err(err).with_context(|| {
                format!("read helper destination metadata {}", destination.display())
            });
        }
    };

    if source_meta.len() != destination_meta.len() {
        return Ok(false);
    }

    let source_modified = source_meta
        .modified()
        .with_context(|| format!("read helper source mtime {}", source.display()))?;
    let destination_modified = destination_meta
        .modified()
        .with_context(|| format!("read helper destination mtime {}", destination.display()))?;

    Ok(destination_modified >= source_modified)
}

#[cfg(test)]
mod tests {
    use super::BIN_DIRNAME;
    use super::CopyOutcome;
    use super::DEV_BUILD_VERSION_SENTINEL;
    use super::HelperExecutable;
    use super::RESOURCES_DIRNAME;
    use super::bundled_executable_path_for_exe;
    use super::copy_from_source_if_needed;
    use super::destination_is_fresh;
    use super::dev_build_suffix;
    use super::helper_bin_dir;
    use super::helper_version_suffix;
    use super::materialized_file_name;
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::path::Path;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn copy_from_source_if_needed_copies_missing_destination() {
        let tmp = TempDir::new().expect("tempdir");
        let source = tmp.path().join("source.exe");
        let destination = tmp.path().join("bin").join("helper.exe");

        fs::write(&source, b"runner-v1").expect("write source");

        let outcome = copy_from_source_if_needed(&source, &destination).expect("copy helper");

        assert_eq!(CopyOutcome::ReCopied, outcome);
        assert_eq!(
            b"runner-v1".as_slice(),
            fs::read(&destination).expect("read destination")
        );
    }

    #[test]
    fn destination_is_fresh_uses_size_and_mtime() {
        let tmp = TempDir::new().expect("tempdir");
        let source = tmp.path().join("source.exe");
        let destination = tmp.path().join("destination.exe");

        fs::write(&destination, b"same-size").expect("write destination");
        std::thread::sleep(std::time::Duration::from_secs(1));
        fs::write(&source, b"same-size").expect("write source");
        assert!(!destination_is_fresh(&source, &destination).expect("stale metadata"));

        fs::write(&destination, b"same-size").expect("rewrite destination");
        assert!(destination_is_fresh(&source, &destination).expect("fresh metadata"));
    }

    #[test]
    fn copy_from_source_if_needed_reuses_fresh_destination() {
        let tmp = TempDir::new().expect("tempdir");
        let source = tmp.path().join("source.exe");
        let destination = tmp.path().join("bin").join("helper.exe");

        fs::write(&source, b"runner-v1").expect("write source");
        copy_from_source_if_needed(&source, &destination).expect("initial copy");

        let outcome = copy_from_source_if_needed(&source, &destination).expect("revalidate helper");

        assert_eq!(CopyOutcome::Reused, outcome);
        assert_eq!(
            b"runner-v1".as_slice(),
            fs::read(&destination).expect("read destination")
        );
    }

    #[test]
    fn helper_bin_dir_is_under_sandbox_bin() {
        let codex_home = Path::new(r"C:\Users\example\.codex");

        assert_eq!(
            PathBuf::from(r"C:\Users\example\.codex\.sandbox-bin"),
            helper_bin_dir(codex_home)
        );
    }

    #[test]
    fn copy_runner_into_shared_bin_dir() {
        let tmp = TempDir::new().expect("tempdir");
        let codex_home = tmp.path().join("codex-home");
        let source_dir = tmp.path().join("sibling-source");
        fs::create_dir_all(&source_dir).expect("create source dir");
        let runner_source = source_dir.join("codex-command-runner.exe");
        fs::write(&runner_source, b"runner").expect("runner");
        let runner_suffix = helper_version_suffix(&runner_source).expect("runner suffix");
        let runner_destination = helper_bin_dir(&codex_home).join(materialized_file_name(
            HelperExecutable::CommandRunner,
            &runner_suffix,
        ));

        let runner_outcome =
            copy_from_source_if_needed(&runner_source, &runner_destination).expect("runner copy");

        assert_eq!(CopyOutcome::ReCopied, runner_outcome);
        assert_eq!(
            b"runner".as_slice(),
            fs::read(&runner_destination).expect("read runner")
        );
    }

    #[test]
    fn helper_source_lookup_checks_resource_dir() {
        let tmp = TempDir::new().expect("tempdir");
        let release_dir = tmp.path().join("release");
        let resources_dir = release_dir.join(RESOURCES_DIRNAME);
        fs::create_dir_all(&resources_dir).expect("create resources dir");
        let exe = release_dir.join("codex.exe");
        let helper = resources_dir.join("codex-command-runner.exe");
        fs::write(&exe, b"codex").expect("write exe");
        fs::write(&helper, b"runner").expect("write helper");

        let resolved =
            bundled_executable_path_for_exe(&exe, /*file_name*/ "codex-command-runner.exe")
                .expect("helper path");

        assert_eq!(resolved, helper);
    }

    #[test]
    fn helper_source_lookup_checks_package_resource_dir_for_bin_exe() {
        let tmp = TempDir::new().expect("tempdir");
        let package_dir = tmp.path().join("package");
        let bin_dir = package_dir.join(BIN_DIRNAME);
        let resources_dir = package_dir.join(RESOURCES_DIRNAME);
        fs::create_dir_all(&bin_dir).expect("create bin dir");
        fs::create_dir_all(&resources_dir).expect("create resources dir");
        let exe = bin_dir.join("codex.exe");
        let helper = resources_dir.join("codex-command-runner.exe");
        fs::write(&exe, b"codex").expect("write exe");
        fs::write(&helper, b"runner").expect("write helper");

        let resolved =
            bundled_executable_path_for_exe(&exe, /*file_name*/ "codex-command-runner.exe")
                .expect("helper path");

        assert_eq!(resolved, helper);
    }

    #[test]
    fn helper_source_lookup_prefers_package_resource_dir_over_bin_resource_dir() {
        let tmp = TempDir::new().expect("tempdir");
        let package_dir = tmp.path().join("package");
        let bin_dir = package_dir.join(BIN_DIRNAME);
        let package_resources_dir = package_dir.join(RESOURCES_DIRNAME);
        let bin_resources_dir = bin_dir.join(RESOURCES_DIRNAME);
        fs::create_dir_all(&package_resources_dir).expect("create package resources dir");
        fs::create_dir_all(&bin_resources_dir).expect("create bin resources dir");
        let exe = bin_dir.join("codex.exe");
        let package_helper = package_resources_dir.join("codex-command-runner.exe");
        let bin_helper = bin_resources_dir.join("codex-command-runner.exe");
        fs::write(&exe, b"codex").expect("write exe");
        fs::write(&package_helper, b"package runner").expect("write package helper");
        fs::write(&bin_helper, b"bin runner").expect("write bin helper");

        let resolved =
            bundled_executable_path_for_exe(&exe, /*file_name*/ "codex-command-runner.exe")
                .expect("helper path");

        assert_eq!(resolved, package_helper);
    }

    #[test]
    fn helper_source_lookup_prefers_direct_sibling_over_resource_dir() {
        let tmp = TempDir::new().expect("tempdir");
        let release_dir = tmp.path().join("release");
        let resources_dir = release_dir.join(RESOURCES_DIRNAME);
        fs::create_dir_all(&resources_dir).expect("create resources dir");
        let exe = release_dir.join("codex.exe");
        let sibling_helper = release_dir.join("codex-command-runner.exe");
        let resource_helper = resources_dir.join("codex-command-runner.exe");
        fs::write(&exe, b"codex").expect("write exe");
        fs::write(&sibling_helper, b"sibling runner").expect("write sibling helper");
        fs::write(&resource_helper, b"resource runner").expect("write resource helper");

        let resolved =
            bundled_executable_path_for_exe(&exe, /*file_name*/ "codex-command-runner.exe")
                .expect("helper path");

        assert_eq!(resolved, sibling_helper);
    }

    #[test]
    fn helper_version_suffix_uses_cli_version_or_dev_build_metadata() {
        let tmp = TempDir::new().expect("tempdir");
        let source = tmp.path().join("source.exe");
        fs::write(&source, b"runner-v1").expect("write source");
        let suffix = helper_version_suffix(&source).expect("suffix");

        if env!("CARGO_PKG_VERSION") == DEV_BUILD_VERSION_SENTINEL {
            assert_eq!(suffix, dev_build_suffix(&source).expect("dev build suffix"));
        } else {
            assert_eq!(suffix, env!("CARGO_PKG_VERSION"));
        }
    }

    #[test]
    fn materialized_file_name_adds_suffix_before_extension() {
        let file_name = materialized_file_name(HelperExecutable::CommandRunner, "test-suffix");

        assert_eq!(file_name, "codex-command-runner-test-suffix.exe");
    }

    fn read_logs(dir: &Path) -> String {
        fs::read_dir(dir)
            .expect("read log dir")
            .map(|entry| fs::read_to_string(entry.expect("log entry").path()).expect("read log"))
            .collect()
    }

    /// #1819 follow-up: a `.sandbox-bin` that is a junction is not labelled
    /// (Windows would label the link itself, not its target, so the fix
    /// would silently not apply); the log says so.
    #[test]
    fn sec_win_1819_helper_bin_dir_link_is_not_labelled() {
        let tmp = TempDir::new().expect("tempdir");
        let codex_home = tmp.path().join("home");
        let target = tmp.path().join("elsewhere");
        let logs = tmp.path().join("logs");
        fs::create_dir_all(&codex_home).expect("home");
        fs::create_dir_all(&target).expect("target");
        fs::create_dir_all(&logs).expect("logs");
        let link = helper_bin_dir(&codex_home);
        let output = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output()
            .expect("mklink");
        assert!(output.status.success(), "mklink: {output:?}");
        // SAFETY: reads the label of a folder this test made.
        let before = unsafe { crate::integrity::read_explicit_labels(&target) }.expect("labels");

        keep_helper_bin_dir_medium(&codex_home, Some(&logs));

        let log = read_logs(&logs);
        assert!(
            log.contains("is a link (or can't be inspected); not labelling it Medium"),
            "{log}"
        );
        assert!(!log.contains("could not label"), "{log}");
        assert!(crate::workspace_acl::is_link(&link));
        // SAFETY: reads the label of a folder this test made.
        let after = unsafe { crate::integrity::read_explicit_labels(&target) }.expect("labels");
        assert_eq!(before, after);
    }

    /// #1819 follow-up: the runner next to `codex.exe`, used when copying it
    /// failed, gets a Medium label when it inherited a legacy root's Low
    /// label, and the log names that cause; one that isn't Low is left alone.
    #[test]
    fn sec_win_1819_low_fallback_runner_is_labelled_medium() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path().join("legacy-root");
        let logs = tmp.path().join("logs");
        fs::create_dir_all(&root).expect("root");
        fs::create_dir_all(&logs).expect("logs");
        // SAFETY: labels a folder this test made, as a legacy session does.
        unsafe { crate::integrity::label_writable_root_low(&root) }.expect("label root Low");
        let runner = root.join("codex-command-runner.exe");
        fs::write(&runner, b"runner").expect("write runner");
        let plain = tmp.path().join("plain-runner.exe");
        fs::write(&plain, b"runner").expect("write plain runner");
        // SAFETY: reads labels of files this test made.
        unsafe {
            assert!(crate::integrity::effective_label_is_low(&runner).expect("label"));
            assert!(!crate::integrity::effective_label_is_low(&plain).expect("label"));
        }
        // SAFETY: reads the label of a file this test made.
        let plain_before =
            unsafe { crate::integrity::read_explicit_labels(&plain) }.expect("labels");

        keep_fallback_helper_medium(&plain, Some(&logs));
        keep_fallback_helper_medium(&runner, Some(&logs));

        let log = read_logs(&logs);
        assert!(
            log.contains("was labelled Low; labelled it Medium")
                && log.contains("inherited from a folder a legacy session made writable"),
            "{log}"
        );
        assert!(!log.contains("plain-runner"), "{log}");
        // SAFETY: reads labels of files this test made.
        unsafe {
            assert!(!crate::integrity::effective_label_is_low(&runner).expect("label"));
            assert_eq!(
                crate::integrity::read_explicit_labels(&runner).expect("labels"),
                vec![crate::integrity::LabelAce {
                    flags: 0,
                    mask: 1,
                    sid: "S-1-16-8192".to_string(),
                }]
            );
            assert_eq!(
                crate::integrity::read_explicit_labels(&plain).expect("labels"),
                plain_before
            );
        }
    }
}
