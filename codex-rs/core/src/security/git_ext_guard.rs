//! D447 (#1876, #1910): an extra layer under the post-taint classifier. A
//! command run after untrusted content gets git's `protocol.ext.allow=never`
//! through `GIT_CONFIG_COUNT` / `GIT_CONFIG_KEY_<n>` / `GIT_CONFIG_VALUE_<n>`,
//! appended after the entries already in its environment. Environment config
//! outranks every config file, so an `ext::` remote stored in `.git/config`
//! or `.gitmodules`, or one the classifier can't see on the line, can't run
//! even where a repository's own config allows `ext`.
//!
//! It needs git 2.31 or later (older git ignores `GIT_CONFIG_COUNT`). It
//! doesn't stop `git remote-ext` or `--upload-pack`, which run a program
//! without the `ext` transport (the classifier covers them, #1897), nor
//! config git reads after the environment's entries: `-c` and
//! `--config-env` on the line, or `GIT_CONFIG_PARAMETERS`. A command session
//! started before the untrusted content keeps the environment it started
//! with. Entries that only the shell snapshot or a remote executor's own
//! environment adds aren't seen here: a `GIT_ALLOW_PROTOCOL` from there
//! keeps `ext`, and the entry may take the index of one of theirs.

use std::collections::HashMap;

use crate::session::session::Session;

const COUNT: &str = "GIT_CONFIG_COUNT";
const ALLOW_PROTOCOL: &str = "GIT_ALLOW_PROTOCOL";
const KEY: &str = "protocol.ext.allow";
const VALUE: &str = "never";

/// Whether D447 applies: post-taint checks are on and untrusted content has
/// entered the session.
pub(crate) fn applies(session: &Session) -> bool {
    session
        .services
        .model_client()
        .post_taint_state()
        .is_some_and(|state| state.taint_generation > 0)
}

/// Appends `protocol.ext.allow=never` to `env`'s git config entries, and
/// drops `ext` from a `GIT_ALLOW_PROTOCOL` list (which git would otherwise
/// follow instead; a list left empty allows no transport at all). `overrides`
/// gets the same values: the shell snapshot restores them after it is
/// sourced, so a snapshot's own `GIT_CONFIG_COUNT` can't drop the entry. A
/// count git can't read is left as is: git refuses to run with one.
pub(crate) fn apply(env: &mut HashMap<String, String>, overrides: &mut HashMap<String, String>) {
    let count = match env.get(COUNT) {
        None => Some(0),
        Some(count) => git_count(count),
    };
    if let Some(count) = count
        && let Some(next) = count.checked_add(1)
    {
        for (name, value) in [
            (format!("GIT_CONFIG_KEY_{count}"), KEY.to_string()),
            (format!("GIT_CONFIG_VALUE_{count}"), VALUE.to_string()),
            (COUNT.to_string(), next.to_string()),
        ] {
            env.insert(name.clone(), value.clone());
            overrides.insert(name, value);
        }
    }
    if let Some(allowed) = env.get(ALLOW_PROTOCOL) {
        let kept = allowed
            .split(':')
            .filter(|protocol| *protocol != "ext")
            .collect::<Vec<_>>()
            .join(":");
        env.insert(ALLOW_PROTOCOL.to_string(), kept.clone());
        overrides.insert(ALLOW_PROTOCOL.to_string(), kept);
    }
}

/// `GIT_CONFIG_COUNT` as git reads it (`strtoul`, then nothing after the
/// number): an empty value is 0; leading white space and one `+` are
/// allowed.
fn git_count(value: &str) -> Option<usize> {
    if value.is_empty() {
        return Some(0);
    }
    let rest = value.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '\x0b');
    let rest = rest.strip_prefix('+').unwrap_or(rest);
    if !rest.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

#[cfg(test)]
#[path = "git_ext_guard_tests.rs"]
mod tests;
