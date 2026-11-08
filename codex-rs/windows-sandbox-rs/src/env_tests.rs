//! #1805: Windows environment names ignore case. A caller's `Path` (the
//! spelling `std::env::vars()` gives on Windows) is the same variable as the
//! sandbox's `PATH`, so a command must never get both.

use super::*;
use crate::process::make_env_block;
use pretty_assertions::assert_eq;

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn spellings(env_map: &HashMap<String, String>, name: &str) -> Vec<String> {
    let mut keys: Vec<String> = env_map
        .keys()
        .filter(|key| key.eq_ignore_ascii_case(name))
        .cloned()
        .collect();
    keys.sort();
    keys
}

/// The entries of an environment block, in order.
fn block_entries(block: &[u16]) -> Vec<String> {
    String::from_utf16(block)
        .expect("UTF-16 block")
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect()
}

#[test]
fn inherit_path_env_keeps_the_callers_path_in_any_spelling() {
    let mut env_map = env(&[("Path", r"C:\caller"), ("PathExt", ".EXE")]);

    inherit_path_env(&mut env_map);

    assert_eq!(spellings(&env_map, "PATH"), vec!["Path".to_string()]);
    assert_eq!(spellings(&env_map, "PATHEXT"), vec!["PathExt".to_string()]);
    assert_eq!(env_map.get("Path"), Some(&r"C:\caller".to_string()));
    assert_eq!(env_map.get("PathExt"), Some(&".EXE".to_string()));
}

#[test]
fn inherit_path_env_adds_the_parents_path_when_the_caller_has_none() {
    let mut env_map = env(&[("TEMP", r"C:\temp")]);

    inherit_path_env(&mut env_map);

    assert_eq!(env_map.get("PATH").cloned(), std::env::var("PATH").ok());
}

#[test]
fn prepend_path_extends_the_callers_path_and_replaces_its_spelling() {
    let mut env_map = env(&[("Path", r"C:\caller"), ("TEMP", r"C:\temp")]);

    prepend_path(&mut env_map, r"C:\denybin");

    assert_eq!(
        env_map,
        env(&[("PATH", r"C:\denybin;C:\caller"), ("TEMP", r"C:\temp")])
    );
}

#[test]
fn reorder_pathext_reads_and_replaces_any_spelling() {
    let mut env_map = env(&[("PathExt", ".EXE;.BAT;.CMD")]);

    reorder_pathext_for_stubs(&mut env_map);

    assert_eq!(env_map, env(&[("PATHEXT", ".BAT;.CMD;.EXE")]));
}

#[test]
fn get_ignoring_case_prefers_the_spelling_the_block_keeps() {
    let env_map = env(&[("path", "lower"), ("Path", "mixed"), ("PATH", "upper")]);

    assert_eq!(
        get_ignoring_case(&env_map, "Path"),
        Some(&"upper".to_string())
    );
    assert_eq!(get_ignoring_case(&env_map, "PATHEXT"), None);
}

#[test]
fn make_env_block_holds_each_name_once() {
    let env_map = env(&[
        ("Path", "mixed"),
        ("path", "lower"),
        ("PATH", "upper"),
        ("Temp", r"C:\temp"),
        ("ComSpec", "cmd.exe"),
    ]);

    let block = make_env_block(&env_map);

    assert_eq!(
        block_entries(&block),
        vec![
            "ComSpec=cmd.exe".to_string(),
            "PATH=upper".to_string(),
            r"Temp=C:\temp".to_string(),
        ]
    );
    assert_eq!(&block[block.len() - 2..], &[0, 0]);
}
