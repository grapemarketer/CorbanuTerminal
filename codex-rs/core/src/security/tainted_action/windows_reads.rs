//! #1261: the files the first judgment opens on Windows (script and sed
//! program files). A lookup never reaches a network share or a mapped drive.
//! A file is opened only through a plain path on a drive proven to be a
//! local fixed disk, after every folder on the way and the file itself were
//! checked not to be a link, junction, mount point or remote (cloud or
//! offline) file; the file is then opened without following a link. Anything
//! else fails closed, and the classifier asks.
//!
//! A plain path is a word relative to the working folder, which is a
//! drive-letter path, with ordinary names only: no network share or device
//! path (`\\server\share`, `\\?\`, `\\.\`, anything starting with two
//! separators), no drive or drive root in the word (Git Bash and WSL may read
//! them otherwise), no `..`, stream (`:`), wildcard, brace or other reserved
//! character, no name ending in a dot or space (Win32 drops them, a `\\?\`
//! path keeps them: two spellings of different files) and no device name
//! (`CON`, `NUL`, `COM1`, ...).

use super::work;
#[cfg(test)]
use std::cell::RefCell;
#[cfg(windows)]
use std::path::PathBuf;

#[cfg(test)]
thread_local! {
    /// Every filesystem call made here, in order, for the tests.
    static TOUCHED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// The filesystem calls made on this thread since the last call.
#[cfg(test)]
pub(super) fn take_touched() -> Vec<String> {
    TOUCHED.with(|touched| std::mem::take(&mut *touched.borrow_mut()))
}

/// What a lookup of a plain path found.
#[derive(Debug)]
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) enum Located {
    /// A regular local file, opened for reading.
    File(std::fs::File),
    /// The file, or a folder on the way, is not there.
    Missing,
    /// Not looked up, or something on the way is not plainly local.
    Refused,
}

fn separator(ch: char) -> bool {
    ch == '/' || ch == '\\'
}

/// Whether `text` starts with two separators: a network share (`\\h\s`,
/// `//h/s`) or a device or verbatim path (`\\.\`, `\\?\`).
pub(super) fn starts_at_a_share(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(separator) && chars.next().is_some_and(separator)
}

/// The upper-case drive letter of `text` starting `X:`, and the rest.
fn drive(text: &str) -> Option<(char, &str)> {
    let mut chars = text.chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    (chars.next() == Some(':')).then(|| (letter.to_ascii_uppercase(), &text[2..]))
}

/// A working folder's drive and the rest: `C:\work` as the action gives it,
/// or `/C:/work` as the classifier's Windows path rules spell a folder it
/// followed (`cd`). Nothing else (a share, a drive-relative `C:`) is plain.
fn folder(cwd: &str) -> Option<(char, &str)> {
    if let Some((letter, rest)) = drive(cwd) {
        return rest.starts_with(separator).then_some((letter, rest));
    }
    let (letter, rest) = drive(cwd.strip_prefix('/')?)?;
    (rest.is_empty() || rest.starts_with('/')).then_some((letter, rest))
}

/// Whether `word` (home spellings expanded) is a plain spelling of a place
/// relative to the working folder: no share or device path, no drive
/// (`C:\x`, `C:x`) or drive root (`\x`, `/x`), which Git Bash or WSL may
/// read otherwise, and only plain names and `.` (not `..`: Git Bash and WSL
/// have a root above a drive's).
pub(super) fn plain_spelling(word: &str) -> bool {
    if word.starts_with(separator) || drive(word).is_some() {
        return false;
    }
    word.split(separator).all(|name| {
        work::step();
        matches!(name, "" | ".") || plain_name(name)
    })
}

/// Whether `word` starts with a drive and no separator (`C:x`): relative to
/// that drive's own current folder, which the classifier does not know.
pub(super) fn drive_relative(word: &str) -> bool {
    drive(word).is_some_and(|(_, rest)| !rest.starts_with(separator))
}

/// Whether a word read as a file is spelled with something the first
/// judgment expands but a POSIX shell may not, or reads otherwise: a
/// variable (`$HOME`, `%USERPROFILE%`) or `~` (WSL's home is not Windows').
pub(super) fn expanded_spelling(word: &str) -> bool {
    word.starts_with('~') || word.contains(['$', '%'])
}

/// Whether Windows could open `name` as an ordinary file or folder name.
fn plain_name(name: &str) -> bool {
    name != ".."
        && !name.ends_with(['.', ' '])
        && !name.chars().any(|ch| {
            ch < ' '
                || matches!(
                    ch,
                    ':' | '*' | '?' | '"' | '<' | '>' | '|' | '[' | ']' | '{' | '}'
                )
        })
        && !device_name(name)
}

/// A DOS device name, with any extension (`nul.txt` is `NUL`).
fn device_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$"
    ) {
        return true;
    }
    let mut chars = stem.chars();
    let prefix: String = chars.by_ref().take(3).collect();
    let number: Vec<char> = chars.collect();
    matches!(prefix.as_str(), "COM" | "LPT")
        && matches!(number.as_slice(), [digit] if digit.is_ascii_digit() || matches!(digit, '¹' | '²' | '³'))
}

/// `word` (home spellings already expanded) made absolute against `cwd`, as
/// the drive letter and the names below the drive's root, when `cwd` is a
/// plain path and `word` a plain relative one (see the module comment): a
/// word with a drive (`C:\x`, `C:x`) or from the drive's root (`\x`) is
/// not, as Git Bash and WSL may read it otherwise.
pub(super) fn plain_path(word: &str, cwd: &str) -> Option<(char, Vec<String>)> {
    if starts_at_a_share(word) || starts_at_a_share(cwd) {
        return None;
    }
    let (cwd_drive, cwd_rest) = folder(cwd)?;
    // A drive or the drive's root: Git Bash and WSL may read it otherwise.
    if drive(word).is_some() || word.starts_with(separator) {
        return None;
    }
    let (letter, path) = (cwd_drive, format!("{cwd_rest}\\{word}"));
    // A Cygwin shortcut link (`x.lnk`), which Git Bash follows.
    if path
        .rsplit(separator)
        .next()
        .is_some_and(|name| name.to_ascii_lowercase().ends_with(".lnk"))
    {
        return None;
    }
    let mut names = Vec::new();
    for name in path.split(separator) {
        work::step();
        match name {
            "" | "." => {}
            name if plain_name(name) => names.push(name.to_string()),
            _ => return None,
        }
    }
    Some((letter, names))
}

/// Record a filesystem call (tests only).
#[cfg(windows)]
fn touch(path: &str) {
    #[cfg(test)]
    TOUCHED.with(|touched| touched.borrow_mut().push(path.to_string()));
    #[cfg(not(test))]
    let _ = path;
}

/// Open `drive:\names...` without following a link: the drive must be a
/// local fixed disk, and every folder on the way and the file must be no
/// link, junction, mount point or remote file. Each is opened in turn, by
/// itself (`FILE_FLAG_OPEN_REPARSE_POINT`), checked through its handle and
/// held open without delete sharing until the file is open, so no folder
/// checked can be swapped for a link before the next is opened. `lookup` is
/// asked before each filesystem call and is false once the lookup budget is
/// spent.
#[cfg(windows)]
pub(super) fn locate(drive: char, names: &[String], lookup: &dyn Fn() -> bool) -> Located {
    use codex_windows_sandbox::local_drive;
    use std::os::windows::fs::MetadataExt;
    use std::os::windows::fs::OpenOptionsExt;
    if names.is_empty() || !lookup() {
        return Located::Refused;
    }
    touch(&format!("{drive}:"));
    if !local_drive::is_local_fixed(drive) {
        return Located::Refused;
    }
    let mut path = PathBuf::from(format!("{drive}:\\"));
    let mut held: Vec<std::fs::File> = Vec::with_capacity(names.len());
    for (at, name) in names.iter().enumerate() {
        work::step();
        path.push(name);
        if !lookup() {
            return Located::Refused;
        }
        touch(&path.to_string_lossy());
        let handle = match std::fs::OpenOptions::new()
            .read(true)
            .share_mode(local_drive::SHARE_NOT_DELETE)
            .custom_flags(local_drive::OPEN_ITSELF)
            .open(&path)
        {
            Ok(handle) => handle,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Located::Missing;
            }
            Err(_) => return Located::Refused,
        };
        let Ok(metadata) = handle.metadata() else {
            return Located::Refused;
        };
        if metadata.file_attributes() & local_drive::REMOTE_OR_LINK_ATTRIBUTES != 0 {
            return Located::Refused;
        }
        if at + 1 == names.len() {
            // A system file may be a Cygwin link (see `local_drive`).
            return if metadata.is_file()
                && metadata.file_attributes() & local_drive::SYSTEM_FILE == 0
            {
                Located::File(handle)
            } else {
                Located::Refused
            };
        }
        if !metadata.is_dir() {
            return Located::Refused;
        }
        held.push(handle);
    }
    Located::Refused
}

/// Elsewhere no drive is proven local: nothing is looked up.
#[cfg(not(windows))]
pub(super) fn locate(_drive: char, _names: &[String], _lookup: &dyn Fn() -> bool) -> Located {
    Located::Refused
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn plain(word: &str, cwd: &str) -> Option<String> {
        plain_path(word, cwd).map(|(drive, names)| format!("{drive}:\\{}", names.join("\\")))
    }

    /// #1261: plain paths are made absolute as Windows does.
    #[test]
    fn issue_1261_plain_paths() {
        for (word, cwd, expected) in [
            ("run.sh", r"C:\work", r"C:\work\run.sh"),
            (r".\tools\run.sh", r"c:\work", r"C:\work\tools\run.sh"),
            ("./run.sh", "/C:/work/sub", r"C:\work\sub\run.sh"),
            ("run.sh", r"C:\", r"C:\run.sh"),
            ("run.sh", "/C:", r"C:\run.sh"),
            (r"PROGRA~1\run.sh", r"C:\", r"C:\PROGRA~1\run.sh"),
            ("comx/run.sh", r"C:\work", r"C:\work\comx\run.sh"),
        ] {
            assert_eq!(
                plain(word, cwd).as_deref(),
                Some(expected),
                "{word} in {cwd}"
            );
        }
    }

    /// #1261: a share, device or verbatim path, a doubtful drive and any
    /// name Windows reads differently are never looked up.
    #[test]
    fn issue_1261_network_and_doubtful_paths_are_not_plain() {
        for (word, cwd) in [
            // Network shares, administrative shares and device paths.
            (r"\\server\share\run.sh", r"C:\work"),
            ("//server/share/run.sh", r"C:\work"),
            (r"\/server\share\run.sh", r"C:\work"),
            (r"\\server\c$\run.sh", r"C:\work"),
            (r"\\localhost\c$\work\run.sh", r"C:\work"),
            (r"\\?\UNC\server\share\run.sh", r"C:\work"),
            (r"\\?\C:\work\run.sh", r"C:\work"),
            (r"\\.\C:\work\run.sh", r"C:\work"),
            (r"\\.\pipe\run", r"C:\work"),
            (r"\\?\GLOBALROOT\Device\Mup\server\share\run.sh", r"C:\work"),
            // A working folder on a share.
            ("run.sh", r"\\server\share\work"),
            ("run.sh", r"\\?\UNC\server\share\work"),
            ("run.sh", "/unc:/server/share/work"),
            ("run.sh", "//server/share"),
            // Working folders that are not a drive's.
            ("run.sh", "C:"),
            ("run.sh", "work"),
            ("run.sh", ""),
            ("run.sh", "/work"),
            // From the current drive's root.
            (r"\x\run.sh", r"C:\work"),
            ("/home/fixture/run.sh", r"C:\work"),
            // A drive (WSL reads it as a relative name).
            (r"D:\x\run.sh", r"C:\work"),
            ("d:/x/run.sh", r"C:\work"),
            ("C:run.sh", r"C:\work"),
            // Globs and braces the shell expands.
            ("r[u]n.sh", r"C:\work"),
            ("{run,x}.sh", r"C:\work"),
            // Another drive's working folder is not known.
            ("D:run.sh", r"C:\work"),
            // Names Windows reads differently.
            (r"..\run.sh", r"C:\work"),
            ("run.sh:hidden", r"C:\work"),
            ("run.sh::$DATA", r"C:\work"),
            ("run.sh.", r"C:\work"),
            ("run.sh ", r"C:\work"),
            ("dir./run.sh", r"C:\work"),
            ("r*n.sh", r"C:\work"),
            ("r?n.sh", r"C:\work"),
            ("a|b", r"C:\work"),
            ("/C:/work/run.sh", r"C:\work"),
            // Devices.
            ("CON", r"C:\work"),
            ("nul.sh", r"C:\work"),
            ("Aux .txt", r"C:\work"),
            ("com1", r"C:\work"),
            ("LPT9.sh", r"C:\work"),
            ("COM¹", r"C:\work"),
            ("conin$", r"C:\work"),
            ("tools/NUL/run.sh", r"C:\work"),
            ("con.sh.d/run.sh", r"C:\work"),
            // A shortcut, which Cygwin may follow as a link.
            ("tool.lnk", r"C:\work"),
            ("sub/TOOL.LNK", r"C:\work"),
        ] {
            assert_eq!(plain(word, cwd), None, "{word} in {cwd}");
        }
    }

    /// #1261: where no drive is proven local (every platform but Windows)
    /// nothing is looked up.
    #[cfg(not(windows))]
    #[test]
    fn issue_1261_no_lookups_off_windows() {
        let names = vec!["work".to_string(), "run.sh".to_string()];
        assert!(matches!(locate('C', &names, &|| true), Located::Refused));
        assert_eq!(take_touched(), Vec::<String>::new());
    }
}
