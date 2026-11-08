//! #932 phase 1: the differential harness. Every string literal in the
//! classifier's sources and tests, the cases in `grammar_corpus.txt` and a
//! fixed set of generated texts are read by the hand-written reader
//! (`quoted.rs`) and by the grammar (`grammar.rs`), and the two readings are
//! compared by the commands they find. With `CORBANU_GRAMMAR_SHELLS=1` each
//! text is also checked (`-n`: read, never run) by the bash, zsh and dash
//! found on the machine. `CORBANU_GRAMMAR_REPORT=<file>` writes every
//! disagreement there; the summary is printed either way.
//!
//! The harness reports; it does not judge. It compares command names only
//! (not arguments or data marks; `grammar_tests.rs` covers those), and in
//! CI it asserts only that the grammar reads the whole corpus without a
//! panic.

use super::Refusal;
use super::read;
use crate::security::tainted_action::quoted;
use crate::security::tainted_action::shadow;
use crate::security::tainted_action::shadow::Comparison;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::process::Command;
use std::process::Stdio;

const SOURCES: &[&str] = &[
    include_str!("../tainted_action.rs"),
    include_str!("../tainted_action_tests.rs"),
    include_str!("../issue_1276_to_1620_tests.rs"),
    include_str!("../tainted_action_refusal_tests.rs"),
    include_str!("arithmetic_tests.rs"),
    include_str!("arithmetic.rs"),
    include_str!("indirect.rs"),
    include_str!("invocation.rs"),
    include_str!("named_shell.rs"),
    include_str!("nested.rs"),
    include_str!("outbound.rs"),
    include_str!("paths.rs"),
    include_str!("quoted.rs"),
    include_str!("sed.rs"),
    include_str!("shell.rs"),
    include_str!("grammar_tests.rs"),
];

const CORPUS: &str = include_str!("grammar_corpus.txt");

/// Generated texts: a fixed seed, so every run reads the same ones.
const GENERATED: usize = 3000;

const FRAGMENTS: &[&str] = &[
    "echo",
    "bash -c",
    "sh -c",
    "zsh -c",
    "sed 1e\\ id",
    "'",
    "\"",
    "`",
    "$(",
    ")",
    "((",
    "))",
    "$((",
    "${x:-",
    "}",
    "'}'",
    "case x in a)",
    ";;",
    "esac",
    "<<EOF\n",
    "\nEOF\n",
    "<<'E'\n",
    "\nE\n",
    "\\\n",
    "$'\\x24(id)'",
    ";",
    "|",
    "&&",
    "#",
    "\n",
    "{",
    "(",
    "[",
    "]",
    "a[",
    "x",
    "corbanu vault get k",
    "eval",
    "\"$x\"",
    "x=$(cat f)",
    "repeat 1",
    "coproc",
    "<(",
    ">(",
    "$[",
    "\\",
    "~",
    "*(e:id:)",
    "${(e)x}",
    "=git",
    "!",
    "do",
    "done",
    "for i in a;",
    "if",
    "then",
    "fi",
    "f()",
    "<<<",
    ">f",
    "2>&1",
    "\"$(",
    "'$('",
    "$x",
    "${x}",
    "\t",
];

/// Shells for `CORBANU_GRAMMAR_SHELLS=1`, with flags that skip startup files.
const SHELLS: &[(&str, &[&str])] = &[
    ("/bin/bash", &["--norc", "--noprofile"]),
    ("/opt/homebrew/bin/bash", &["--norc", "--noprofile"]),
    ("/bin/zsh", &["-f"]),
    ("/usr/bin/zsh", &["-f"]),
    ("/bin/dash", &[]),
    ("/usr/bin/dash", &[]),
];

/// Rust string literals in `source`, decoded (raw strings included).
fn string_literals(source: &str) -> Vec<String> {
    let chars: Vec<char> = source.chars().collect();
    let mut found = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        let ident = |at: usize| chars[at].is_alphanumeric() || chars[at] == '_';
        // `r"…"`, `r#"…"#`, and the byte string forms `br"…"`.
        let previous_ident =
            at > 0 && ident(at - 1) && !(chars[at - 1] == 'b' && (at < 2 || !ident(at - 2)));
        match chars[at] {
            '/' if chars.get(at + 1) == Some(&'/') => {
                while at < chars.len() && chars[at] != '\n' {
                    at += 1;
                }
            }
            '/' if chars.get(at + 1) == Some(&'*') => {
                at += 2;
                while at + 1 < chars.len() && !(chars[at] == '*' && chars[at + 1] == '/') {
                    at += 1;
                }
                at += 2;
            }
            'r' if !previous_ident && matches!(chars.get(at + 1), Some('"' | '#')) => {
                let hashes = chars[at + 1..].iter().take_while(|ch| **ch == '#').count();
                let open = at + 1 + hashes;
                if chars.get(open) != Some(&'"') {
                    at += 1;
                    continue;
                }
                let mut end = open + 1;
                while end < chars.len()
                    && !(chars[end] == '"'
                        && chars[end + 1..]
                            .iter()
                            .take(hashes)
                            .filter(|ch| **ch == '#')
                            .count()
                            == hashes)
                {
                    end += 1;
                }
                found.push(chars[open + 1..end.min(chars.len())].iter().collect());
                at = end + 1 + hashes;
            }
            '"' => {
                let mut text = String::new();
                at += 1;
                while at < chars.len() && chars[at] != '"' {
                    if chars[at] == '\\' && at + 1 < chars.len() {
                        at += 1;
                        match chars[at] {
                            'n' => text.push('\n'),
                            't' => text.push('\t'),
                            'r' => text.push('\r'),
                            '0' => text.push('\0'),
                            'x' => {
                                let hex: String = chars[at + 1..].iter().take(2).collect();
                                text.extend(
                                    u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32),
                                );
                                at += 2;
                            }
                            'u' => {
                                let close = chars[at..].iter().position(|ch| *ch == '}');
                                let hex: String =
                                    chars[at + 2..at + close.unwrap_or(2)].iter().collect();
                                text.extend(
                                    u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32),
                                );
                                at += close.unwrap_or(0);
                            }
                            '\n' => {
                                while chars.get(at + 1).is_some_and(|ch| ch.is_whitespace()) {
                                    at += 1;
                                }
                            }
                            escaped => text.push(escaped),
                        }
                    } else {
                        text.push(chars[at]);
                    }
                    at += 1;
                }
                found.push(text);
            }
            // A character literal (`'"'`, `'\''`), not a lifetime.
            '\'' if chars.get(at + 1) == Some(&'\\') => {
                at += 3;
                while at < chars.len() && chars[at] != '\'' {
                    at += 1;
                }
            }
            '\'' if chars.get(at + 2) == Some(&'\'') => at += 2,
            _ => {}
        }
        at += 1;
    }
    found
}

/// One case per line of `grammar_corpus.txt`: `#` starts a comment line,
/// `\n`, `\t` and `\\` are escapes.
fn corpus_cases() -> Vec<String> {
    CORPUS
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut text = String::new();
            let mut chars = line.chars();
            while let Some(ch) = chars.next() {
                match (ch, chars.clone().next()) {
                    ('\\', Some('n')) => text.push('\n'),
                    ('\\', Some('t')) => text.push('\t'),
                    ('\\', Some('\\')) => text.push('\\'),
                    _ => {
                        text.push(ch);
                        continue;
                    }
                }
                chars.next();
            }
            text
        })
        .collect()
}

fn generated_cases() -> Vec<String> {
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    (0..GENERATED)
        .map(|_| {
            let count = 1 + next() % 10;
            let mut text = String::new();
            for _ in 0..count {
                text.push_str(FRAGMENTS[(next() % FRAGMENTS.len() as u64) as usize]);
                if next() % 2 == 0 {
                    text.push(' ');
                }
            }
            text
        })
        .collect()
}

fn all_cases() -> Vec<(&'static str, String)> {
    let mut seen = BTreeSet::new();
    let mut cases = Vec::new();
    let literals = SOURCES.iter().flat_map(|source| string_literals(source));
    for (origin, text) in literals
        .map(|text| ("tests", text))
        .chain(corpus_cases().into_iter().map(|text| ("corpus", text)))
        .chain(
            generated_cases()
                .into_iter()
                .map(|text| ("generated", text)),
        )
    {
        if (2..=4096).contains(&text.len()) && seen.insert(text.clone()) {
            cases.push((origin, text));
        }
    }
    cases
}

/// Whether `shell` reads `text` without a syntax error (`-n`: nothing runs).
fn shell_accepts(shell: &str, flags: &[&str], text: &str) -> bool {
    Command::new(shell)
        .args(flags)
        .args(["-n", "-c", "--", text])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn escaped(text: &str) -> String {
    text.escape_debug().to_string()
}

#[test]
fn grammar_and_reader_differential() {
    let cases = all_cases();
    assert!(cases.len() > 2000, "corpus too small: {}", cases.len());
    let shells: Vec<(&str, &[&str])> = if std::env::var_os("CORBANU_GRAMMAR_SHELLS").is_some() {
        SHELLS
            .iter()
            .copied()
            .filter(|(shell, _)| std::path::Path::new(shell).exists())
            .collect()
    } else {
        Vec::new()
    };
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut report = String::new();
    let mut max_checks_per_byte = 0.0f64;
    for (origin, text) in &cases {
        let reading = quoted::commands(text);
        let mut unlimited = usize::MAX;
        let (comparison, _) = shadow::compare(text, &reading, &mut unlimited);
        let reader = shadow::reader_names(&reading);
        let grammar =
            read(text).map(|grammar| (shadow::grammar_names(&grammar), grammar.zsh_unsure));
        if let Ok(reading) = read(text) {
            max_checks_per_byte =
                max_checks_per_byte.max(reading.parse_checks as f64 / text.len() as f64);
        }
        let category = format!("{comparison:?}");
        let zsh = matches!(grammar, Ok((_, true)));
        // Phase 3: lines whose zsh-only checks no longer ask under zsh.
        let proves = crate::security::tainted_action::named_shell::grammar_proves_data(&[
            "/bin/zsh".to_string(),
            "-lc".to_string(),
            text.clone(),
        ]);
        if proves {
            *counts
                .entry(format!("{origin}: phase 3 proves data under zsh"))
                .or_default() += 1;
            let _ = writeln!(
                report,
                "{origin}\tphase 3 proves data\t\t\t\t{}",
                escaped(text)
            );
        }
        *counts.entry(format!("{origin}: {category}")).or_default() += 1;
        if zsh {
            *counts.entry(format!("{origin}: zsh-unsure")).or_default() += 1;
        }
        let mut accepted = Vec::new();
        for (shell, flags) in &shells {
            accepted.push((*shell, shell_accepts(shell, flags, text)));
        }
        let rejecting: Vec<&str> = accepted
            .iter()
            .filter(|(_, accepts)| !accepts)
            .map(|(shell, _)| *shell)
            .collect();
        let shell_category = match (&grammar, rejecting.is_empty()) {
            _ if shells.is_empty() => None,
            (Ok(_), false) => Some(format!(
                "grammar reads, rejected by {}",
                rejecting.join(" ")
            )),
            (Err(Refusal::Syntax), true) => Some("grammar syntax error, shells accept".to_string()),
            _ => None,
        };
        if let Some(shell_category) = &shell_category {
            *counts
                .entry(format!("{origin}: {shell_category}"))
                .or_default() += 1;
        }
        if comparison != Comparison::Agree || shell_category.is_some() {
            let _ = writeln!(
                report,
                "{origin}\t{category}\t{}\treader={reader:?}\tgrammar={grammar:?}\t{}",
                shell_category.unwrap_or_default(),
                escaped(text)
            );
        }
    }
    let mut summary = format!(
        "#932 differential: {} cases, max parse checks per byte {max_checks_per_byte:.3}\n",
        cases.len()
    );
    for (category, count) in &counts {
        let _ = writeln!(summary, "{count:6}  {category}");
    }
    #[expect(clippy::print_stdout, reason = "the summary is the harness's output")]
    {
        println!("{summary}");
    }
    if let Some(path) = std::env::var_os("CORBANU_GRAMMAR_REPORT") {
        std::fs::write(path, format!("{summary}\n{report}")).expect("write the report");
    }
}

#[test]
fn literals_are_extracted_and_decoded() {
    let source = r##"let a = "x \"y\"\n"; // "not"
        let b = r#"p "q""#; let c = '"'; let d = '\''; /* "no" */ let e = "\u{e9}";"##;
    assert_eq!(
        string_literals(source),
        vec![
            "x \"y\"\n".to_string(),
            "p \"q\"".to_string(),
            "\u{e9}".to_string()
        ]
    );
}
