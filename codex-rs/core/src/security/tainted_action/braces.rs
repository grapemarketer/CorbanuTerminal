//! #1636: bounded shell brace expansion of one word, for git's checks on the
//! quote-aware reading (`nested.rs`). Unlike the word lexer's expansion
//! (`shell::expand_braces`), it expands every group as bash does: ranges
//! (`{u..u}`), a group after one that does not expand (`{c}{a,b}`,
//! `${N}{a,b}`), and nested groups, left to right. Quotes are already
//! removed from the text it gets, so quoted braces expand too: that can only
//! add words.

use super::work;

/// The words `text` expands to, in the shell's order, or `None` past `limit`
/// words.
pub(super) fn expand(text: &str, limit: usize) -> Option<Vec<String>> {
    let mut done = Vec::new();
    let mut pending = vec![text.to_string()];
    while let Some(word) = pending.pop() {
        work::step();
        match first_group(&word, limit)? {
            None => done.push(word),
            Some((open, close, alternatives)) => {
                let (prefix, suffix) = (&word[..open], &word[close + 1..]);
                // Reversed, so the first alternative is expanded first.
                for alternative in alternatives.iter().rev() {
                    work::step();
                    pending.push(format!("{prefix}{alternative}{suffix}"));
                }
            }
        }
        if done.len() + pending.len() > limit {
            return None;
        }
    }
    Some(done)
}

/// The first group that expands, as bash finds it: the first `{` whose
/// matching `}` holds a comma outside nested groups or is a range. Its byte
/// span and alternatives (`Some(None)`: none), or `None` for a range past
/// `limit`.
#[allow(clippy::type_complexity)]
fn first_group(word: &str, limit: usize) -> Option<Option<(usize, usize, Vec<String>)>> {
    let bytes = word.as_bytes();
    // The matching `}` of each `{`, and whether a comma is directly inside.
    let mut close = vec![None; bytes.len()];
    let mut comma = vec![false; bytes.len()];
    let mut stack: Vec<usize> = Vec::new();
    for (at, byte) in bytes.iter().enumerate() {
        work::step();
        match byte {
            b'{' => stack.push(at),
            b'}' => {
                if let Some(open) = stack.pop() {
                    close[open] = Some(at);
                }
            }
            b',' => {
                if let Some(open) = stack.last() {
                    comma[*open] = true;
                }
            }
            _ => {}
        }
    }
    for open in 0..bytes.len() {
        work::step();
        let Some(end) = close[open] else {
            continue;
        };
        let inner = &word[open + 1..end];
        if comma[open] {
            return Some(Some((open, end, split_top(inner))));
        }
        if !inner.contains('{') {
            match range(inner, limit) {
                Range::Not => {}
                Range::Words(words) => return Some(Some((open, end, words))),
                Range::TooMany => return None,
            }
        }
    }
    Some(None)
}

/// `inner` split at commas outside nested groups.
fn split_top(inner: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (at, ch) in inner.char_indices() {
        work::step();
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(inner[start..at].to_string());
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(inner[start..].to_string());
    parts
}

enum Range {
    Not,
    Words(Vec<String>),
    TooMany,
}

/// A bash sequence `x..y[..step]` of integers or of single characters.
fn range(inner: &str, limit: usize) -> Range {
    let parts: Vec<&str> = inner.split("..").collect();
    let (from, to, step) = match parts.as_slice() {
        [from, to] => (*from, *to, None),
        [from, to, step] => (*from, *to, Some(*step)),
        _ => return Range::Not,
    };
    let step = match step.map(integer) {
        None => 1,
        Some(Integer::Value(step)) => step.unsigned_abs().max(1),
        Some(Integer::Huge) => return Range::TooMany,
        Some(Integer::Not) => return Range::Not,
    };
    match (integer(from), integer(to)) {
        (Integer::Value(from_value), Integer::Value(to_value)) => {
            let count = from_value.abs_diff(to_value) / step + 1;
            if count > limit as u64 {
                return Range::TooMany;
            }
            let padded = |text: &str| {
                text.trim_start_matches('-').len() > 1
                    && text.trim_start_matches('-').starts_with('0')
            };
            let width = if padded(from) || padded(to) {
                from.len().max(to.len())
            } else {
                0
            };
            let mut words = Vec::new();
            let mut value = i128::from(from_value);
            let up = to_value >= from_value;
            for _ in 0..count {
                work::step();
                words.push(if width > 0 {
                    let digits = format!(
                        "{:0width$}",
                        value.unsigned_abs(),
                        width = width - usize::from(value < 0)
                    );
                    if value < 0 {
                        format!("-{digits}")
                    } else {
                        digits
                    }
                } else {
                    value.to_string()
                });
                value = if up {
                    value + i128::from(step)
                } else {
                    value - i128::from(step)
                };
            }
            Range::Words(words)
        }
        (Integer::Huge, _) | (_, Integer::Huge) => Range::TooMany,
        _ => {
            let (mut from_chars, mut to_chars) = (from.chars(), to.chars());
            let (Some(first), None, Some(last), None) = (
                from_chars.next(),
                from_chars.next(),
                to_chars.next(),
                to_chars.next(),
            ) else {
                return Range::Not;
            };
            let (first, last) = (u32::from(first), u32::from(last));
            let count = u64::from(first.abs_diff(last)) / step + 1;
            if count > limit as u64 {
                return Range::TooMany;
            }
            let mut words = Vec::new();
            for index in 0..count {
                work::step();
                let offset = u32::try_from(index * step).unwrap_or(u32::MAX);
                let code = if last >= first {
                    first.saturating_add(offset)
                } else {
                    first.saturating_sub(offset)
                };
                words.extend(char::from_u32(code).map(String::from));
            }
            Range::Words(words)
        }
    }
}

enum Integer {
    Not,
    Value(i64),
    Huge,
}

fn integer(text: &str) -> Integer {
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Integer::Not;
    }
    text.parse().map_or(Integer::Huge, Integer::Value)
}

#[cfg(test)]
mod tests {
    use super::expand;
    use pretty_assertions::assert_eq;

    #[test]
    fn expands_as_bash_does() {
        for (text, words) in [
            ("a", vec!["a"]),
            ("{a,b}", vec!["a", "b"]),
            ("x{a,b}y", vec!["xay", "xby"]),
            ("{a,b}{c,d}", vec!["ac", "ad", "bc", "bd"]),
            ("{a,{b,c}}", vec!["a", "b", "c"]),
            ("{c}{a,b}", vec!["{c}a", "{c}b"]),
            ("${N}{--u,}", vec!["${N}--u", "${N}"]),
            ("--{u..u}pload", vec!["--upload"]),
            ("{1..3}", vec!["1", "2", "3"]),
            ("{3..1}", vec!["3", "2", "1"]),
            ("{1..7..3}", vec!["1", "4", "7"]),
            ("{01..03}", vec!["01", "02", "03"]),
            ("{-1..1}", vec!["-1", "0", "1"]),
            ("{a..c}", vec!["a", "b", "c"]),
            ("{x{a,b}}", vec!["{xa}", "{xb}"]),
            ("{a,}", vec!["a", ""]),
            ("{}", vec!["{}"]),
            ("}x{", vec!["}x{"]),
            ("x,}", vec!["x,}"]),
            ("stash@{0}", vec!["stash@{0}"]),
            ("{a..b..c}", vec!["{a..b..c}"]),
            ("{ab..c}", vec!["{ab..c}"]),
        ] {
            assert_eq!(
                expand(text, 64),
                Some(words.iter().map(ToString::to_string).collect()),
                "{text}"
            );
        }
        for text in [
            "{1..65}",
            "{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}",
            "{1..99999999999999999999}",
        ] {
            assert_eq!(expand(text, 64), None, "{text}");
        }
    }
}
