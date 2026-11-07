//! A test-only count of the classifier's work, for its linearity tests
//! (#1102).
//!
//! Every loop in the classifier (`tainted_action.rs` and the modules under
//! `tainted_action/`) steps a counter once per iteration; a test below holds
//! the code to that rule. The `codex-core` unit-test binary also counts the
//! bytes each thread allocates, which covers library calls that copy text
//! (`to_string`, `format!`, `collect`) outside any loop of ours. A linearity
//! test classifies the same input at two sizes and compares both counts,
//! which, unlike a wall-clock budget, do not depend on the machine or its
//! load. Reverse searches also charge the bytes in the searched suffix,
//! through the returned byte offset (or all text on a miss), and substring
//! needle setup. This exposes repeated prefix scans without a timing bound.
//! Other unwrapped library scans and recursion remain outside the counter.
//! Outside tests counters are no-ops.

#[cfg(test)]
use std::alloc::GlobalAlloc;
#[cfg(test)]
use std::alloc::Layout;
#[cfg(test)]
use std::alloc::System;
#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
thread_local! {
    static STEPS: Cell<u64> = const { Cell::new(0) };
    static BYTES: Cell<u64> = const { Cell::new(0) };
}

/// One loop iteration of classifier work.
#[inline(always)]
pub(super) fn step() {
    #[cfg(test)]
    STEPS.with(|steps| steps.set(steps.get() + 1));
}

/// A reverse substring search whose scan is visible to the work counter.
#[inline(always)]
pub(super) fn rfind(text: &str, pattern: &str) -> Option<usize> {
    let found = text.rfind(pattern);
    scanned(
        text.len()
            .saturating_sub(found.unwrap_or_default())
            .saturating_add(pattern.len()),
    );
    found
}

/// A reverse character search with the same result as `str::rfind`.
#[inline(always)]
pub(super) fn rfind_by(text: &str, predicate: impl FnMut(char) -> bool) -> Option<usize> {
    let found = text.rfind(predicate);
    scanned(text.len().saturating_sub(found.unwrap_or_default()));
    found
}

/// Conservative scan-byte work, separate from the allocator's byte count.
#[inline(always)]
fn scanned(bytes: usize) {
    #[cfg(test)]
    STEPS.with(|steps| steps.set(steps.get() + bytes as u64));
    #[cfg(not(test))]
    let _ = bytes;
}

/// The system allocator, counting the bytes each thread asks for.
#[cfg(test)]
struct Counting;

#[cfg(test)]
#[global_allocator]
static COUNTING: Counting = Counting;

#[cfg(test)]
fn allocated(bytes: usize) {
    // `try_with`: a thread may still free or allocate as it exits.
    let _ = BYTES.try_with(|count| count.set(count.get() + bytes as u64));
}

#[cfg(test)]
// SAFETY: every call is passed to `System` unchanged; counting allocates
// nothing (the counters are constant-initialised thread locals).
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocated(layout.size());
        // SAFETY: the caller's contract for `alloc`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        allocated(layout.size());
        // SAFETY: the caller's contract for `alloc_zeroed`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's contract for `dealloc`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        allocated(new_size);
        // SAFETY: the caller's contract for `realloc`.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Work counted on this thread.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Work {
    /// Loop iterations plus charged search bytes.
    pub(super) steps: u64,
    /// Bytes allocated (a reallocation counts its new size).
    pub(super) bytes: u64,
}

#[cfg(test)]
fn now() -> Work {
    Work {
        steps: STEPS.with(Cell::get),
        bytes: BYTES.with(Cell::get),
    }
}

/// The work `run` does on this thread.
#[cfg(test)]
pub(super) fn measure(run: impl FnOnce()) -> Work {
    let before = now();
    run();
    let after = now();
    Work {
        steps: after.steps - before.steps,
        bytes: after.bytes - before.bytes,
    }
}

/// Input sizes a linearity test compares: `run(1)` and `run(SCALE)`.
#[cfg(test)]
pub(super) const SCALE: u64 = 4;

/// Linear work grows at most `SCALE` times (fixed costs make it less), work
/// growing with the square of the input `SCALE * SCALE` times. The bound
/// leaves room for `n log n` and fails anything near quadratic.
#[cfg(test)]
const MAX_GROWTH: u64 = 6;

/// Asserts that `run(scale)`, which classifies an input `scale` times as
/// long, does work (loop steps and allocated bytes, each) that grows
/// linearly with `scale`.
#[cfg(test)]
pub(super) fn assert_linear(label: &str, run: impl Fn(usize)) {
    let small = measure(|| run(1));
    let large = measure(|| run(SCALE as usize));
    assert!(small.steps > 0, "{label}: no classifier loop counted");
    for (counted, small, large) in [
        ("steps", small.steps, large.steps),
        ("bytes", small.bytes, large.bytes),
    ] {
        assert!(
            large <= MAX_GROWTH * small.max(1),
            "{label}: {small} {counted} at 1x, {large} at {SCALE}x (linear work grows at \
             most {SCALE}x, quadratic about {}x)",
            SCALE * SCALE
        );
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    /// The classifier's sources, which must step the counter in every loop.
    const SOURCES: &[(&str, &str)] = &[
        ("tainted_action.rs", include_str!("../tainted_action.rs")),
        ("aliases.rs", include_str!("aliases.rs")),
        ("arithmetic.rs", include_str!("arithmetic.rs")),
        ("certain_cd.rs", include_str!("certain_cd.rs")),
        ("grammar.rs", include_str!("grammar.rs")),
        ("git_config.rs", include_str!("git_config.rs")),
        ("indirect.rs", include_str!("indirect.rs")),
        ("invocation.rs", include_str!("invocation.rs")),
        ("launched_git.rs", include_str!("launched_git.rs")),
        ("launched_input.rs", include_str!("launched_input.rs")),
        ("loader_variables.rs", include_str!("loader_variables.rs")),
        ("mktemp.rs", include_str!("mktemp.rs")),
        ("named_shell.rs", include_str!("named_shell.rs")),
        ("nested.rs", include_str!("nested.rs")),
        ("code_variables.rs", include_str!("code_variables.rs")),
        ("outbound.rs", include_str!("outbound.rs")),
        ("rerun.rs", include_str!("rerun.rs")),
        ("outputs.rs", include_str!("outputs.rs")),
        ("git_helpers.rs", include_str!("git_helpers.rs")),
        ("paths.rs", include_str!("paths.rs")),
        ("quoted.rs", include_str!("quoted.rs")),
        ("sed.rs", include_str!("sed.rs")),
        ("shadow.rs", include_str!("shadow.rs")),
        ("shell.rs", include_str!("shell.rs")),
        ("unknown_folder.rs", include_str!("unknown_folder.rs")),
        ("windows_paths.rs", include_str!("windows_paths.rs")),
        ("windows_reads.rs", include_str!("windows_reads.rs")),
        ("windows_shell.rs", include_str!("windows_shell.rs")),
    ];

    /// Where loops start in `line`, as their keyword (`for`, `while` or
    /// `loop`) and its offset: at the start of the line or after `=`, `=>`,
    /// `{`, `(`, `,`, `;` or `|`, labelled or not, in code outside strings,
    /// character literals and comments.
    fn loop_starts(line: &str) -> Vec<(&'static str, usize)> {
        let bytes = line.as_bytes();
        let mut found = Vec::new();
        // Whether a statement or expression may start here.
        let mut opens = true;
        let mut at = 0;
        while at < bytes.len() {
            match bytes[at] {
                b' ' | b'\t' | b'\n' | b'\r' => {}
                b'/' if bytes.get(at + 1) == Some(&b'/') => break,
                b'"' => {
                    at += 1;
                    while at < bytes.len() && bytes[at] != b'"' {
                        at += if bytes[at] == b'\\' { 2 } else { 1 };
                    }
                    opens = false;
                }
                b'\'' if bytes.get(at + 2) == Some(&b'\'') => {
                    at += 2;
                    opens = false;
                }
                b'\'' if bytes.get(at + 1) == Some(&b'\\') => {
                    at += bytes[at + 3..]
                        .iter()
                        .position(|&byte| byte == b'\'')
                        .unwrap_or(0)
                        + 3;
                    opens = false;
                }
                b'\'' if opens => {
                    // A label (`'outer: loop`) keeps the position open.
                    let name = bytes[at + 1..]
                        .iter()
                        .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'_')
                        .count();
                    if name > 0 && bytes.get(at + 1 + name) == Some(&b':') {
                        at += name + 1;
                    } else {
                        opens = false;
                    }
                }
                byte => {
                    if opens
                        && let Some(keyword) = ["for ", "while ", "loop {", "loop{"]
                            .into_iter()
                            .find(|keyword| bytes[at..].starts_with(keyword.as_bytes()))
                    {
                        found.push((keyword.trim_end_matches('{').trim_end(), at));
                    }
                    opens = matches!(byte, b'=' | b'>' | b'{' | b'(' | b',' | b';' | b'|');
                }
            }
            at += 1;
        }
        found
    }

    /// Whether the body of the loop starting `header` (which runs on to
    /// the end of the file) begins with `work::step();`. The body opens at
    /// the first brace outside brackets, quotes and character literals,
    /// after `in` for a `for` loop (a pattern may hold braces).
    fn steps_first(keyword: &str, header: &str) -> bool {
        let bytes = header.as_bytes();
        let mut depth = 0usize;
        let mut seen_in = keyword != "for";
        let mut at = keyword.len();
        while at < bytes.len() {
            match bytes[at] {
                b'"' => {
                    at += 1;
                    while at < bytes.len() && bytes[at] != b'"' {
                        at += if bytes[at] == b'\\' { 2 } else { 1 };
                    }
                }
                b'\'' if bytes.get(at + 2) == Some(&b'\'') => at += 2,
                b'\'' if bytes.get(at + 1) == Some(&b'\\') => {
                    at += bytes[at + 3..]
                        .iter()
                        .position(|&byte| byte == b'\'')
                        .unwrap_or(0)
                        + 3;
                }
                b'{' if depth == 0 && seen_in => {
                    return header[at + 1..].trim_start().starts_with("work::step();");
                }
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                _ if depth == 0 && bytes[at..].starts_with(b" in ") => seen_in = true,
                _ => {}
            }
            at += 1;
        }
        false
    }

    /// Loops in `source` (up to its tests) whose body does not start with
    /// `work::step();`, as `file:line`.
    fn unstepped(file: &str, source: &str) -> Vec<String> {
        let mut missing = Vec::new();
        let mut offset = 0;
        for (at, line) in source.split_inclusive('\n').enumerate() {
            let code = line.trim_start();
            if code.starts_with("mod tests") || code.starts_with("#[path = ") {
                break;
            }
            for (keyword, from) in loop_starts(line) {
                if !steps_first(keyword, &source[offset + from..]) {
                    missing.push(format!("{file}:{}", at + 1));
                }
            }
            offset += line.len();
        }
        missing
    }

    #[test]
    fn every_classifier_loop_steps_the_counter() {
        let missing: Vec<String> = SOURCES
            .iter()
            .flat_map(|(file, source)| unstepped(file, source))
            .collect();
        assert_eq!(
            missing,
            Vec::<String>::new(),
            "start these loops' bodies with `work::step();` (see work.rs)"
        );
        // The check finds loops of each form.
        let loops = "for x in y {\n    work::step();\n}\nwhile a\n    && b\n{\n    f();\n}\n\
                     'outer: loop {\n}\n// for x in y {\nlet f = |x| x;\n\
                     for P { a } in s.split('{').chain(['\\'', '\\u{7d}']) {\n    work::step();\n}\n\
                     while s == \"{\" {}\nfor P { a } in x {\n    f(a);\n}\n\
                     let v = 'l: loop {\n    work::step();\n};\nlet w = loop {\n    break;\n};\n\
                     match x { _ => while y {} }\nimpl A for B {}\nf(\"= loop {\", '\"', x)\n";
        assert_eq!(
            unstepped("f.rs", loops),
            vec![
                "f.rs:4", "f.rs:9", "f.rs:16", "f.rs:17", "f.rs:23", "f.rs:26"
            ]
        );
    }

    /// A module added to the classifier is checked too.
    #[test]
    fn every_classifier_module_is_checked() {
        for (file, source) in SOURCES {
            for line in source.lines() {
                let code = line.trim_start();
                let code = code.strip_prefix("pub(super) ").unwrap_or(code);
                let code = code.strip_prefix("pub(crate) ").unwrap_or(code);
                let Some(module) = code
                    .strip_prefix("mod ")
                    .and_then(|rest| rest.strip_suffix(';'))
                else {
                    continue;
                };
                assert!(
                    module == "work"
                        || module.ends_with("tests")
                        || SOURCES
                            .iter()
                            .any(|(checked, _)| *checked == format!("{module}.rs")),
                    "{file}: add `{module}.rs` to SOURCES in work.rs"
                );
            }
        }
    }

    #[test]
    fn work_counts_this_thread_only() {
        let other_thread = 1 << 20;
        let counted = super::measure(|| {
            for _ in 0..3 {
                super::step();
            }
            drop(vec![0u8; 1000]);
            std::thread::spawn(move || {
                super::step();
                drop(vec![0u8; other_thread]);
            })
            .join()
            .unwrap();
        });
        assert_eq!(counted.steps, 3);
        assert!(
            (1000..other_thread as u64).contains(&counted.bytes),
            "{counted:?}"
        );
    }

    #[test]
    fn reverse_search_preserves_byte_offsets_and_predicates() {
        for text in [
            "",
            "plain",
            "a=b=c",
            "a\n\u{1f344}b\n",
            "\u{1f344}\u{1f344}",
        ] {
            for pattern in ["", "=", "\n", "zz", "\u{1f344}"] {
                assert_eq!(super::rfind(text, pattern), text.rfind(pattern));
            }
            let whitespace = |ch: char| ch.is_ascii_whitespace();
            assert_eq!(super::rfind_by(text, whitespace), text.rfind(whitespace));
        }
    }

    #[test]
    fn reverse_search_counts_nonallocating_scan_work() {
        let text = "x".repeat(/*n*/ 4096);
        let counted = super::measure(|| {
            assert_eq!(super::rfind(&text, "zz"), None);
        });
        assert!(
            counted.steps > 0,
            "reverse search was invisible: {counted:?}"
        );
    }

    #[test]
    fn single_reverse_searches_stay_linear() {
        super::assert_linear("single reverse searches", |scale| {
            let text = "x".repeat(1000 * scale);
            assert_eq!(super::rfind(&text, "zz"), None);
            assert_eq!(super::rfind_by(&text, |ch| ch == 'z'), None);
        });
    }

    #[test]
    #[should_panic(expected = "quadratic reverse search")]
    fn repeated_reverse_prefix_search_fails_linearity_guard() {
        super::assert_linear("quadratic reverse search", |scale| {
            let text = "x".repeat(1000 * scale);
            for at in 0..text.len() {
                super::step();
                std::hint::black_box(super::rfind(&text[..at], "zz"));
            }
        });
    }
}
