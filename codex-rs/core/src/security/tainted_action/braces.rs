//! #1636: bounded shell brace expansion of one word, for git's checks on the
//! quote-aware reading (`nested.rs`). Unlike the word lexer's expansion
//! (`shell::expand_braces`), it expands every group as bash does: ranges
//! (`{u..u}`), a group after one that does not expand (`{c}{a,b}`,
//! `${N}{a,b}`), nested groups, left to right, and bash's reading of a
//! group with `..` and a nested comma group (`{-u../x{,}}` is `-u../x`
//! twice; zsh keeps those braces, a word that is no option either way).
//! It gets text with quotes and backslashes removed, so it may expand braces
//! the shell leaves as written (`'{a,b}'`), or leave a group the shell
//! expands where an escaped brace pairs with an unquoted one; D258's check
//! covers lines with quotes or backslashes.

use super::work;

/// Groups one word may expand, per word it may expand to.
const STEPS_PER_WORD: usize = 4;
/// Longest word expanded: no option or command name is longer.
const MAX_WORD_BYTES: usize = 4096;

/// The words a word expands to, in the shell's order.
pub(super) struct Expansion {
    pub(super) words: Vec<String>,
    /// Past a bound: some words are missing, and some may still hold groups
    /// not expanded (a word past `MAX_WORD_BYTES` is kept as written).
    pub(super) truncated: bool,
}

/// `text`'s expansion, up to `limit` words. `old_bash`: as bash 3.2 (the
/// macOS `/bin/bash`) expands, where the outermost `}` closes a group only
/// after a `,` or a `..` not right before it, and `{}` starting a word
/// opens none: `{-b},-u./x}` is `-b}` and `-u./x`. Groups inside a first
/// group that does not expand are read as by later bash.
pub(super) fn expand(text: &str, limit: usize, old_bash: bool) -> Expansion {
    if text.len() > MAX_WORD_BYTES {
        return Expansion {
            words: vec![text.to_string()],
            truncated: true,
        };
    }
    let mut done = Vec::new();
    // In reverse order: the last is expanded next.
    let mut pending = vec![text.to_string()];
    let mut steps = STEPS_PER_WORD * limit;
    let mut truncated = false;
    while let Some(word) = pending.pop() {
        work::step();
        let Some(group) = first_group(&word, old_bash) else {
            done.push(word);
            continue;
        };
        if steps == 0 {
            pending.push(word);
            truncated = true;
            break;
        }
        steps -= 1;
        let room = limit.saturating_sub(done.len() + pending.len()).max(1);
        let mut alternatives = group.alternatives(&word[group.open + 1..group.close], room + 1);
        if alternatives.len() > room {
            alternatives.truncate(room);
            truncated = true;
        }
        let (prefix, suffix) = (&word[..group.open], &word[group.close + 1..]);
        for alternative in alternatives.iter().rev() {
            work::step();
            pending.push(format!("{prefix}{alternative}{suffix}"));
        }
        if truncated {
            break;
        }
    }
    while let Some(word) = pending.pop() {
        work::step();
        done.push(word);
    }
    done.truncate(limit.max(1));
    Expansion {
        words: done,
        truncated,
    }
}

#[derive(Clone, Copy)]
enum Kind {
    /// `{a,b}`: split at its commas outside nested groups.
    Commas,
    /// bash: `..` outside nested groups and a comma in one (`{x..{a,b}}`):
    /// the braces go.
    Dropped,
    /// `{1..3}`, `{a..c}`.
    Range,
}

/// A group that expands: the byte offsets of its braces.
struct Group {
    open: usize,
    close: usize,
    kind: Kind,
}

impl Group {
    /// Up to `room` of its alternatives.
    fn alternatives(&self, inner: &str, room: usize) -> Vec<String> {
        match self.kind {
            Kind::Commas => split_top(inner, room),
            Kind::Dropped => vec![inner.to_string()],
            Kind::Range => range(inner, room).unwrap_or_default(),
        }
    }
}

/// An open `{` while scanning a word.
struct Open {
    at: usize,
    /// A comma outside nested groups.
    comma: bool,
    /// A comma anywhere inside.
    any_comma: bool,
    /// `..` outside nested groups.
    dots: bool,
    /// bash 3.2: such a `..` not right before a `}`.
    open_dots: bool,
    nested: bool,
    previous_dot: bool,
}

/// The first group that expands, as bash finds it: the first `{` whose
/// matching `}` holds a comma outside nested groups or is a range (or bash
/// drops its braces, see [`Kind::Dropped`]). One pass: groups close inner
/// first, and none opened later can come before one already found once no
/// group is open around it. `old_bash`: see [`expand`].
fn first_group(word: &str, old_bash: bool) -> Option<Group> {
    let bytes = word.as_bytes();
    let mut stack: Vec<Open> = Vec::new();
    let mut best: Option<Group> = None;
    for (at, &byte) in bytes.iter().enumerate() {
        work::step();
        let next = bytes.get(at + 1).copied();
        let plain = match byte {
            // bash 3.2: `{}` starting a word opens no group, and the
            // outermost open group closes only after a `,` or a `..`.
            b'{' => old_bash && at == 0 && next == Some(b'}'),
            b'}' => {
                old_bash
                    && stack.len() == 1
                    && stack
                        .last()
                        .is_some_and(|open| !(open.comma || open.open_dots))
            }
            _ => true,
        };
        if plain {
            if let Some(open) = stack.last_mut() {
                open.comma |= byte == b',';
                open.any_comma |= byte == b',';
                let dots = byte == b'.' && open.previous_dot;
                open.dots |= dots;
                open.open_dots |= dots && next != Some(b'}');
                open.previous_dot = byte == b'.';
            }
            continue;
        }
        if byte == b'{' {
            if let Some(open) = stack.last_mut() {
                open.nested = true;
                open.previous_dot = false;
            }
            stack.push(Open {
                at,
                comma: false,
                any_comma: false,
                dots: false,
                open_dots: false,
                nested: false,
                previous_dot: false,
            });
            continue;
        }
        let Some(open) = stack.pop() else {
            continue;
        };
        if let Some(outer) = stack.last_mut() {
            outer.any_comma |= open.any_comma;
            outer.previous_dot = false;
        }
        let inner = &word[open.at + 1..at];
        let kind = if open.comma {
            Some(Kind::Commas)
        } else if !open.nested && range(inner, /*room*/ 0).is_some() {
            Some(Kind::Range)
        } else if open.dots && open.any_comma {
            Some(Kind::Dropped)
        } else {
            None
        };
        if let Some(kind) = kind
            && best.as_ref().is_none_or(|best| open.at < best.open)
        {
            best = Some(Group {
                open: open.at,
                close: at,
                kind,
            });
        }
        if stack.is_empty() && best.is_some() {
            break;
        }
    }
    best
}

/// `inner` split at commas outside nested groups: the first `room` parts.
fn split_top(inner: &str, room: usize) -> Vec<String> {
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
                if parts.len() == room {
                    return parts;
                }
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(inner[start..].to_string());
    parts
}

/// A bash sequence `x..y[..step]` of integers or of single characters: its
/// first `room` words, or `None` when `inner` is no sequence. With `room`
/// 0, only whether it is one.
fn range(inner: &str, room: usize) -> Option<Vec<String>> {
    let mut parts = inner.split("..");
    let (from, to, step) = (parts.next()?, parts.next()?, parts.next());
    if parts.next().is_some() {
        return None;
    }
    let step = match step {
        None => 1,
        Some(step) => integer(step)?.map_or(u64::MAX, |step| step.unsigned_abs().max(1)),
    };
    let mut words = Vec::new();
    match (integer(from), integer(to)) {
        (Some(from_value), Some(to_value)) => {
            // Past i64: as many words as any bound allows.
            let (Some(first), Some(last)) = (from_value, to_value) else {
                for _ in 0..room {
                    work::step();
                    words.push(from.to_string());
                }
                return Some(words);
            };
            let pad = |text: &str| {
                let digits = text.trim_start_matches('-');
                digits.len() > 1 && digits.starts_with('0')
            };
            let width = if pad(from) || pad(to) {
                from.len().max(to.len())
            } else {
                0
            };
            let mut value = i128::from(first);
            let (end, up) = (i128::from(last), last >= first);
            let within = |value: i128| if up { value <= end } else { value >= end };
            while words.len() < room && within(value) {
                work::step();
                let digits = format!(
                    "{:0width$}",
                    value.unsigned_abs(),
                    width = width.saturating_sub(usize::from(value < 0))
                );
                words.push(if value < 0 {
                    format!("-{digits}")
                } else {
                    digits
                });
                value = if up {
                    value + i128::from(step)
                } else {
                    value - i128::from(step)
                };
            }
        }
        (None, None) => {
            let (mut from_chars, mut to_chars) = (from.chars(), to.chars());
            let (Some(first), None, Some(last), None) = (
                from_chars.next(),
                from_chars.next(),
                to_chars.next(),
                to_chars.next(),
            ) else {
                return None;
            };
            let (mut code, end, up) = (
                u64::from(u32::from(first)),
                u64::from(u32::from(last)),
                last >= first,
            );
            let within = |code: u64| if up { code <= end } else { code >= end };
            while words.len() < room && within(code) {
                work::step();
                words.extend(
                    u32::try_from(code)
                        .ok()
                        .and_then(char::from_u32)
                        .map(String::from),
                );
                code = if up {
                    code.saturating_add(step)
                } else if code < step {
                    break;
                } else {
                    code - step
                };
            }
        }
        _ => return None,
    }
    Some(words)
}

/// An integer as bash reads one in a sequence: `Some(None)` past i64.
fn integer(text: &str) -> Option<Option<i64>> {
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
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
            ("{a..}", vec!["{a..}"]),
            ("{x.{c,d}.y}", vec!["{x.c.y}", "{x.d.y}"]),
            // bash drops these braces (zsh keeps them).
            (
                "{--upload-pack=../x{,}}",
                vec!["--upload-pack=../x", "--upload-pack=../x"],
            ),
            ("{x{1..2}..{c,d}}", vec!["x1..c", "x1..d", "x2..c", "x2..d"]),
            ("{x{a..b}{c,d}}", vec!["{xac}", "{xad}", "{xbc}", "{xbd}"]),
        ] {
            let expansion = expand(text, /*limit*/ 64, /*old_bash*/ false);
            assert_eq!(
                (expansion.words, expansion.truncated),
                (words.iter().map(ToString::to_string).collect(), false),
                "{text}"
            );
        }
        for (text, first) in [
            ("{1..65}", "1"),
            ("{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}", "aaaaaaa"),
            ("{1..99999999999999999999}", "1"),
            ("--x{1..99999999999999999999}", "--x1"),
        ] {
            let expansion = expand(text, /*limit*/ 64, /*old_bash*/ false);
            assert!(expansion.truncated, "{text}");
            assert_eq!(expansion.words.len(), 64, "{text}");
            assert_eq!(expansion.words[0], first, "{text}");
        }
        let chain = "{u..u}".repeat(300);
        let expansion = expand(&chain, /*limit*/ 64, /*old_bash*/ false);
        assert!(expansion.truncated);
        assert_eq!(expansion.words.len(), 1);
        let long = format!("--{}", "x".repeat(5000));
        assert!(expand(&long, /*limit*/ 64, /*old_bash*/ false).truncated);
        // bash 3.2.
        for (text, words) in [
            ("{-b},-u./x}", vec!["-b}", "-u./x"]),
            ("-{},u./x}", vec!["-}", "-u./x"]),
            ("{-u}u,}u", vec!["-u}uu", "u"]),
            ("{a,b}", vec!["a", "b"]),
            ("{a..c}", vec!["a", "b", "c"]),
            ("{x}", vec!["{x}"]),
            ("{},a}", vec!["{},a}"]),
            ("x{},a}", vec!["x}", "xa"]),
            ("{a..}b,c}", vec!["a..}b", "c"]),
            ("{x{a},b}", vec!["x{a}", "b"]),
            ("{a}{b,c}", vec!["{a}b", "{a}c"]),
        ] {
            let expansion = expand(text, /*limit*/ 64, /*old_bash*/ true);
            assert_eq!(
                expansion.words,
                words.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "{text}"
            );
        }
    }
}
