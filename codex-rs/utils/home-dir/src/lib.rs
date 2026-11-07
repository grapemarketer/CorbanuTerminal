use codex_utils_absolute_path::AbsolutePathBuf;
use dirs::home_dir;
use std::path::PathBuf;
use std::sync::Once;

const DEFAULT_HOME_DIR: &str = ".corbanu";
const LEGACY_HOME_DIR: &str = ".pfterminal";

/// Returns the path to the Codex configuration directory, which can be specified by the
/// `CORBANU_HOME`, `PFTERMINAL_HOME`, or `CODEX_HOME` environment variable.
/// Existing PFTerminal state is used in place; fresh installations default to
/// `~/.corbanu`.
///
/// - If an explicit home variable is set, the value must exist and be a directory. The
///   value will be canonicalized and this function will Err otherwise.
/// - If no explicit home is set, this function does not verify that the
///   directory exists.
pub fn find_codex_home() -> std::io::Result<AbsolutePathBuf> {
    let corbanu_home_env = std::env::var("CORBANU_HOME")
        .ok()
        .filter(|val| !val.is_empty());
    let pfterminal_home_env = std::env::var("PFTERMINAL_HOME")
        .ok()
        .filter(|val| !val.is_empty());
    let codex_home_env = std::env::var("CODEX_HOME")
        .ok()
        .filter(|val| !val.is_empty());
    static CHECKED: Once = Once::new();
    CHECKED.call_once(|| {
        if let Some(warning) = home_override_conflict(
            corbanu_home_env.as_deref(),
            pfterminal_home_env.as_deref(),
            codex_home_env.as_deref(),
        ) {
            eprintln!("{warning}");
        }
    });
    find_codex_home_from_env(
        corbanu_home_env.as_deref(),
        pfterminal_home_env.as_deref(),
        codex_home_env.as_deref(),
        home_dir(),
    )
}

/// Describes home variables that name different directories, so an inherited
/// `CORBANU_HOME` cannot silently beat a caller's `CODEX_HOME`. Precedence is
/// unchanged; the message names only variables and paths.
fn home_override_conflict(
    corbanu_home_env: Option<&str>,
    pfterminal_home_env: Option<&str>,
    codex_home_env: Option<&str>,
) -> Option<String> {
    home_variables_conflict(&[
        ("CORBANU_HOME", corbanu_home_env),
        ("PFTERMINAL_HOME", pfterminal_home_env),
        ("CODEX_HOME", codex_home_env),
    ])
}

/// The one-line warning when home variables, listed in precedence order, name
/// different directories: the first set variable wins, and the first listed
/// variable is the one to set to choose another home. Names variables and
/// paths only.
pub fn home_variables_conflict(variables: &[(&str, Option<&str>)]) -> Option<String> {
    let (top, _) = *variables.first()?;
    let set: Vec<(&str, &str)> = variables
        .iter()
        .filter_map(|(name, value)| value.map(|value| (*name, value)))
        .collect();
    let (winner, winner_value) = *set.first()?;
    let same_dir = |value: &str| {
        let canonical =
            |value: &str| std::fs::canonicalize(value).unwrap_or_else(|_| PathBuf::from(value));
        canonical(value) == canonical(winner_value)
    };
    let ignored: Vec<String> = set[1..]
        .iter()
        .filter(|(_, value)| !same_dir(value))
        .map(|(name, value)| format!("{name} ({value})"))
        .collect();
    if ignored.is_empty() {
        return None;
    }
    Some(format!(
        "warning: {winner} ({winner_value}) overrides {}; using {winner_value}. \
         To use another home, set {top} to it.",
        ignored.join(" and ")
    ))
}

fn find_codex_home_from_env(
    corbanu_home_env: Option<&str>,
    pfterminal_home_env: Option<&str>,
    codex_home_env: Option<&str>,
    user_home: Option<PathBuf>,
) -> std::io::Result<AbsolutePathBuf> {
    let home_override = corbanu_home_env
        .map(|value| ("CORBANU_HOME", value))
        .or_else(|| pfterminal_home_env.map(|value| ("PFTERMINAL_HOME", value)))
        .or_else(|| codex_home_env.map(|value| ("CODEX_HOME", value)));

    match home_override {
        Some(home_override) => resolve_home_override(home_override),
        None => resolve_default_home(user_home),
    }
}

/// #594: the Corbanu home as named, before any link in it is resolved: the
/// first set home variable (made absolute against the current folder) or the
/// default folder in the user's home, with the variable's name. `None` when
/// neither is known.
pub fn given_codex_home() -> Option<(Option<&'static str>, PathBuf)> {
    let var = |name: &'static str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(|value| (Some(name), PathBuf::from(value)))
    };
    let (variable, path) = var("CORBANU_HOME")
        .or_else(|| var("PFTERMINAL_HOME"))
        .or_else(|| var("CODEX_HOME"))
        .or_else(|| {
            resolve_default_home(home_dir())
                .ok()
                .map(|home| (None, home.into_path_buf()))
        })?;
    Some((variable, std::path::absolute(&path).unwrap_or(path)))
}

fn resolve_home_override((variable, value): (&str, &str)) -> std::io::Result<AbsolutePathBuf> {
    let path = PathBuf::from(value);
    let metadata = std::fs::metadata(&path).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{variable} points to {value:?}, but that path does not exist"),
        ),
        _ => std::io::Error::new(
            err.kind(),
            format!("failed to read {variable} {value:?}: {err}"),
        ),
    })?;

    if !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{variable} points to {value:?}, but that path is not a directory"),
        ));
    }

    let canonical = path.canonicalize().map_err(|err| {
        std::io::Error::new(
            err.kind(),
            format!("failed to canonicalize {variable} {value:?}: {err}"),
        )
    })?;
    AbsolutePathBuf::from_absolute_path(canonical)
}

fn resolve_default_home(user_home: Option<PathBuf>) -> std::io::Result<AbsolutePathBuf> {
    let user_home = user_home.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not find home directory",
        )
    })?;
    let corbanu_home = user_home.join(DEFAULT_HOME_DIR);
    let legacy_home = user_home.join(LEGACY_HOME_DIR);
    let selected = match (corbanu_home.is_dir(), legacy_home.is_dir()) {
        (true, true) => corbanu_home,
        (false, true) => legacy_home,
        (true, false) | (false, false) => corbanu_home,
    };
    AbsolutePathBuf::from_absolute_path(selected)
}

#[cfg(test)]
mod tests {
    use super::DEFAULT_HOME_DIR;
    use super::LEGACY_HOME_DIR;
    use super::find_codex_home_from_env;
    use super::home_override_conflict;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::io::ErrorKind;
    use tempfile::TempDir;

    #[test]
    fn find_codex_home_env_missing_path_is_fatal() {
        let temp_home = TempDir::new().expect("temp home");
        let missing = temp_home.path().join("missing-codex-home");
        let missing_str = missing
            .to_str()
            .expect("missing codex home path should be valid utf-8");

        let err = find_codex_home_from_env(
            /*corbanu_home_env*/ None,
            /*pfterminal_home_env*/ None,
            Some(missing_str),
            /*user_home*/ None,
        )
        .expect_err("missing CODEX_HOME");
        assert_eq!(err.kind(), ErrorKind::NotFound);
        assert!(
            err.to_string().contains("CODEX_HOME"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn find_codex_home_env_file_path_is_fatal() {
        let temp_home = TempDir::new().expect("temp home");
        let file_path = temp_home.path().join("codex-home.txt");
        fs::write(&file_path, "not a directory").expect("write temp file");
        let file_str = file_path
            .to_str()
            .expect("file codex home path should be valid utf-8");

        let err = find_codex_home_from_env(
            /*corbanu_home_env*/ None,
            /*pfterminal_home_env*/ None,
            Some(file_str),
            /*user_home*/ None,
        )
        .expect_err("file CODEX_HOME");
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(
            err.to_string().contains("not a directory"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn find_codex_home_env_valid_directory_canonicalizes() {
        let temp_home = TempDir::new().expect("temp home");
        let temp_str = temp_home
            .path()
            .to_str()
            .expect("temp codex home path should be valid utf-8");

        let resolved = find_codex_home_from_env(
            /*corbanu_home_env*/ None,
            /*pfterminal_home_env*/ None,
            Some(temp_str),
            /*user_home*/ None,
        )
        .expect("valid CODEX_HOME");
        let expected = temp_home
            .path()
            .canonicalize()
            .expect("canonicalize temp home");
        let expected = AbsolutePathBuf::from_absolute_path(expected).expect("absolute home");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn find_codex_home_without_env_uses_default_home_dir() {
        let user_home = TempDir::new().expect("user home");
        let resolved = find_codex_home_from_env(
            /*corbanu_home_env*/ None,
            /*pfterminal_home_env*/ None,
            /*codex_home_env*/ None,
            Some(user_home.path().to_path_buf()),
        )
        .expect("default Corbanu home");
        let expected = user_home.path().join(DEFAULT_HOME_DIR);
        let expected = AbsolutePathBuf::from_absolute_path(expected).expect("absolute home");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn pfterminal_home_beats_codex_home() {
        let pfterminal_home = TempDir::new().expect("PFTerminal home");
        let codex_home = TempDir::new().expect("Codex home");
        let resolved = find_codex_home_from_env(
            /*corbanu_home_env*/ None,
            pfterminal_home.path().to_str(),
            codex_home.path().to_str(),
            /*user_home*/ None,
        )
        .expect("resolve preferred home");
        let expected = AbsolutePathBuf::from_absolute_path(
            pfterminal_home
                .path()
                .canonicalize()
                .expect("canonical home"),
        )
        .expect("absolute home");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn corbanu_home_beats_legacy_and_codex_overrides() {
        let corbanu_home = TempDir::new().expect("Corbanu home");
        let pfterminal_home = TempDir::new().expect("PFTerminal home");
        let codex_home = TempDir::new().expect("Codex home");
        let resolved = find_codex_home_from_env(
            corbanu_home.path().to_str(),
            pfterminal_home.path().to_str(),
            codex_home.path().to_str(),
            /*user_home*/ None,
        )
        .expect("resolve Corbanu home");
        let expected = AbsolutePathBuf::from_absolute_path(
            corbanu_home.path().canonicalize().expect("canonical home"),
        )
        .expect("absolute home");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn existing_legacy_home_is_used_in_place() {
        let user_home = TempDir::new().expect("user home");
        let legacy_home = user_home.path().join(LEGACY_HOME_DIR);
        fs::create_dir(&legacy_home).expect("legacy home");

        let resolved = find_codex_home_from_env(
            /*corbanu_home_env*/ None,
            /*pfterminal_home_env*/ None,
            /*codex_home_env*/ None,
            Some(user_home.path().to_path_buf()),
        )
        .expect("resolve legacy home");
        assert_eq!(resolved.as_path(), legacy_home);
    }

    #[test]
    fn new_home_wins_when_both_homes_exist_without_modifying_legacy() {
        let user_home = TempDir::new().expect("user home");
        let corbanu_home = user_home.path().join(DEFAULT_HOME_DIR);
        let legacy_home = user_home.path().join(LEGACY_HOME_DIR);
        fs::create_dir(&corbanu_home).expect("Corbanu home");
        fs::create_dir(&legacy_home).expect("legacy home");
        let sentinel = legacy_home.join("vault-sentinel");
        fs::write(&sentinel, "preserve").expect("legacy sentinel");

        let resolved = find_codex_home_from_env(
            /*corbanu_home_env*/ None,
            /*pfterminal_home_env*/ None,
            /*codex_home_env*/ None,
            Some(user_home.path().to_path_buf()),
        )
        .expect("resolve Corbanu home");
        assert_eq!(resolved.as_path(), corbanu_home);
        assert_eq!(fs::read_to_string(sentinel).expect("sentinel"), "preserve");
    }

    #[test]
    fn matching_home_variables_do_not_warn() {
        let temp_home = TempDir::new().expect("temp home");
        let home = temp_home.path().to_str().expect("utf-8 path");
        assert_eq!(
            home_override_conflict(Some(home), /*pfterminal_home_env*/ None, Some(home)),
            None
        );
        assert_eq!(
            home_override_conflict(
                /*corbanu_home_env*/ None,
                /*pfterminal_home_env*/ None,
                Some(home)
            ),
            None
        );
    }

    #[test]
    fn conflicting_home_variables_name_the_winner() {
        let warning = home_override_conflict(
            Some("/homes/coordinator"),
            /*pfterminal_home_env*/ None,
            Some("/homes/worker"),
        );
        assert_eq!(
            warning.as_deref(),
            Some(
                "warning: CORBANU_HOME (/homes/coordinator) overrides CODEX_HOME (/homes/worker); \
                 using /homes/coordinator. To use another home, set CORBANU_HOME to it."
            )
        );
    }

    #[test]
    fn same_directory_spelled_differently_does_not_warn() {
        let temp_home = TempDir::new().expect("temp home");
        let home = temp_home.path().join("home");
        fs::create_dir(&home).expect("home dir");
        let link = temp_home.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&home, &link).expect("symlink");
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&home, &link).expect("symlink");
        let home = home.to_str().expect("utf-8 path");
        let trailing = format!("{home}/");
        let link = link.to_str().expect("utf-8 path");
        assert_eq!(
            home_override_conflict(Some(home), Some(&trailing), Some(link)),
            None
        );
    }

    #[test]
    fn every_ignored_variable_is_named_even_when_missing() {
        let warning = home_override_conflict(
            Some("/homes/a"),
            Some("/homes/b-missing"),
            Some("/homes/c-missing"),
        );
        assert_eq!(
            warning.as_deref(),
            Some(
                "warning: CORBANU_HOME (/homes/a) overrides PFTERMINAL_HOME (/homes/b-missing) \
                 and CODEX_HOME (/homes/c-missing); using /homes/a. To use another home, set \
                 CORBANU_HOME to it."
            )
        );
    }
}
