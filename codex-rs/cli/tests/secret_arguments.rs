//! D175: `corbanu login` and `corbanu account` read keys and tokens from
//! stdin only; a value passed on the command line by mistake is not repeated
//! in the usage error.

use std::path::Path;

use anyhow::Result;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use tempfile::TempDir;

const KEY_CANARY: &str = "d175-key-canary-not-a-secret";

fn corbanu(codex_home: &Path) -> Result<assert_cmd::Command> {
    let mut command = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("corbanu")?);
    command
        .env_remove("CORBANU_HOME")
        .env_remove("PFTERMINAL_HOME")
        .env("CODEX_HOME", codex_home)
        .env("CORBANU_TEST_NO_NATIVE_KEYRING", "1");
    Ok(command)
}

#[test]
fn a_key_on_the_command_line_is_not_repeated_in_the_usage_error() -> Result<()> {
    let home = TempDir::new()?;
    for args in [
        vec!["account", "add", "zai", "work", KEY_CANARY],
        vec![
            "account",
            "add",
            "zai",
            "work",
            "--enable",
            "named_accounts",
            KEY_CANARY,
        ],
        vec!["login", "--with-api-key", KEY_CANARY],
        vec!["login", "--account", "work", "--with-api-key", KEY_CANARY],
    ] {
        corbanu(home.path())?
            .args(&args)
            .write_stdin("")
            .assert()
            .code(2)
            .stdout(contains(KEY_CANARY).not())
            .stderr(
                contains("not shown, in case it is a key or token")
                    .and(contains("read from stdin"))
                    .and(contains(KEY_CANARY).not()),
            );
    }
    // A value attached to an on/off flag is not shown either.
    for flag in ["--with-api-key", "--with-access-token"] {
        corbanu(home.path())?
            .arg("login")
            .arg(format!("{flag}={KEY_CANARY}"))
            .write_stdin("")
            .assert()
            .code(2)
            .stderr(contains("not shown").and(contains(KEY_CANARY).not()));
    }
    // An unknown flag is named without the value assigned to it.
    corbanu(home.path())?
        .args(["account", "add", "zai", "work"])
        .arg(format!("--key={KEY_CANARY}"))
        .write_stdin("")
        .assert()
        .code(2)
        .stderr(contains("--key").and(contains(KEY_CANARY).not()));
    Ok(())
}

/// Other usage errors read as before: an unknown flag is named, and another
/// command's unexpected value is repeated.
#[test]
fn other_usage_errors_are_unchanged() -> Result<()> {
    let home = TempDir::new()?;
    corbanu(home.path())?
        .args(["account", "add", "zai", "work", "--bogus"])
        .assert()
        .code(2)
        .stderr(contains("--bogus").and(contains("not shown").not()));
    corbanu(home.path())?
        .args(["logout", "stray-value"])
        .assert()
        .code(2)
        .stderr(contains("stray-value"));
    Ok(())
}
