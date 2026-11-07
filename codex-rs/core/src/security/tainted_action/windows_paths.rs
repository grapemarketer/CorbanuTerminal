//! #1240: Windows path rules for the post-taint classifier's Windows view.
//! The view never touches the filesystem (a lookup of a Windows path can
//! follow a link to a network share); it judges paths as written.
//!
//! On Windows the classifier judges each action twice: once exactly as on
//! every other platform, and once with these rules, keeping the stronger
//! finding. They read `\` as a separator, drive (`C:\`, `C:`), UNC
//! (`\\host\share`) and verbatim (`\\?\C:\`) roots, and the Windows
//! spellings of the homes (`%USERPROFILE%`, `$env:USERPROFILE`,
//! `${env:CODEX_HOME}`, `%HOMEDRIVE%%HOMEPATH%`). A normalized path is
//! lowercase-insensitive like every path the classifier matches, uses `/`,
//! and starts with `/` and its root: `/c:/users/fixture`, or
//! `/unc:/host/share/...` for a network share. Win32 drops trailing dots and
//! spaces from a name and `:stream` names a stream of the file, so both are
//! dropped too (`auth.json.`, `.netrc::$DATA` are the files themselves).

use super::work;

/// Environment variables that name a home, as cmd (`%NAME%`) and PowerShell
/// (`$env:NAME`) spell them. `HOME` and `PWD` are also PowerShell variables
/// (`$HOME`, `$PWD`).
pub(super) const HOME_VARIABLES: &[&str] = &[
    "USERPROFILE",
    "HOME",
    "HOMEDRIVE",
    "HOMEPATH",
    "CODEX_HOME",
    "CORBANU_HOME",
    "PFTERMINAL_HOME",
    "CD",
    "PWD",
    "USERNAME",
    "SYSTEMDRIVE",
    "APPDATA",
    "LOCALAPPDATA",
    // #1261 (D168): see `PLACE_VARIABLES`.
    "PROGRAMDATA",
    "ALLUSERSPROFILE",
    "WINDIR",
    "SYSTEMROOT",
    "PROGRAMFILES",
    "PROGRAMW6432",
    "PROGRAMFILES(X86)",
];
/// #1261 (D168): variables naming places that run code at start-up, whose
/// values are only usual ones (PowerShell 7's `$PSHOME`, a OneDrive
/// `Documents` differ): expanded only when [`Values::places`] is set, as an
/// extra reading of a word besides the one that keeps them unknown. Their
/// PowerShell spellings (`$env:ProgramData`, `$PROFILE`, `$PSHOME`) are
/// rewritten as cmd's like the home variables'.
pub(super) const PLACE_VARIABLES: &[&str] = &[
    "PROGRAMDATA",
    "ALLUSERSPROFILE",
    "WINDIR",
    "SYSTEMROOT",
    "PROGRAMFILES",
    "PROGRAMW6432",
    "PROGRAMFILES(X86)",
    "PROFILE",
    "PSHOME",
];
/// Longest variable name read in a spelling; past it the text is left alone.
const MAX_NAME: usize = 64;

/// What a home variable stands for.
pub(crate) struct Values<'a> {
    pub(crate) codex_home: &'a str,
    pub(crate) user_home: Option<&'a str>,
    pub(crate) cwd: &'a str,
    /// #1261: also expand [`PLACE_VARIABLES`].
    pub(crate) places: bool,
}

impl Values<'_> {
    /// `folder` at the root of the home's drive.
    fn system_folder(&self, folder: &str) -> Option<String> {
        let home = self.user_home?;
        drive(&home.replace('\\', "/")).map(|_| format!("{}\\{folder}", &home[..2]))
    }

    fn get(&self, name: &str) -> Option<String> {
        match name {
            "USERPROFILE" | "HOME" => self.user_home.map(str::to_string),
            // The system drive: the home's, as usual.
            "HOMEDRIVE" | "SYSTEMDRIVE" => self.user_home.and_then(|home| {
                let slashed = home.replace('\\', "/");
                drive(&slashed).map(|_| home[..2].to_string())
            }),
            "HOMEPATH" => self.user_home.map(|home| {
                let slashed = home.replace('\\', "/");
                if drive(&slashed).is_some() {
                    home[2..].to_string()
                } else {
                    home.to_string()
                }
            }),
            "CODEX_HOME" | "CORBANU_HOME" | "PFTERMINAL_HOME" => {
                (!self.codex_home.is_empty()).then(|| self.codex_home.to_string())
            }
            "CD" | "PWD" => (!self.cwd.is_empty()).then(|| self.cwd.to_string()),
            // Their usual places in a profile (`C:\Users\%USERNAME%`).
            "USERNAME" => self.user_home.and_then(|home| {
                normalize(home)
                    .rsplit_once('/')
                    .map(|(_, name)| name.to_string())
                    .filter(|name| !name.is_empty() && !name.ends_with(':'))
            }),
            "APPDATA" => self
                .user_home
                .map(|home| format!("{home}\\AppData\\Roaming")),
            "LOCALAPPDATA" => self.user_home.map(|home| format!("{home}\\AppData\\Local")),
            // #1261 (D168): their usual places; machine-wide ones on the
            // system drive (the home's, as for `SYSTEMDRIVE`).
            "PROGRAMDATA" | "ALLUSERSPROFILE" if self.places => self.system_folder("ProgramData"),
            "WINDIR" | "SYSTEMROOT" if self.places => self.system_folder("Windows"),
            "PROGRAMFILES" | "PROGRAMW6432" if self.places => self.system_folder("Program Files"),
            "PROGRAMFILES(X86)" if self.places => self.system_folder("Program Files (x86)"),
            "PSHOME" if self.places => {
                self.system_folder(r"Windows\System32\WindowsPowerShell\v1.0")
            }
            "PROFILE" if self.places => self.user_home.map(|home| {
                format!(r"{home}\Documents\WindowsPowerShell\Microsoft.PowerShell_profile.ps1")
            }),
            _ => None,
        }
    }
}

/// The text with PowerShell spellings of the home variables rewritten as
/// cmd's (`$env:USERPROFILE`, `${env:HOME}`, `$($env:X)`, `$HOME`, `$PWD`
/// become `%USERPROFILE%`, `%HOME%`, `%X%`, `%HOME%`, `%PWD%`; #1261: also
/// `$PROFILE` and `$PSHOME`), in any letter case. Other text is kept.
/// Linear: each `$` reads at most a name's length.
pub(super) fn cmd_spellings(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < chars.len() {
        work::step();
        if chars[at] == '$' {
            // `$($env:X)`: a subexpression holding only the variable.
            if chars.get(at + 1) == Some(&'(')
                && let Some((name, end)) = variable_at(&chars, at + 2)
                && chars.get(end) == Some(&')')
            {
                out.push_str(&format!("%{name}%"));
                at = end + 1;
                continue;
            }
            if let Some((name, end)) = variable_at(&chars, at) {
                out.push_str(&format!("%{name}%"));
                at = end;
                continue;
            }
        }
        out.push(chars[at]);
        at += 1;
    }
    out
}

/// #1261: whether `text` uses one of [`PLACE_VARIABLES`], in any spelling.
pub(super) fn names_a_place(text: &str) -> bool {
    if !text.contains(['%', '$']) {
        return false;
    }
    let text = cmd_spellings(text).to_lowercase();
    PLACE_VARIABLES.iter().any(|name| {
        work::step();
        text.contains(&format!("%{}%", name.to_lowercase()))
    })
}

/// A PowerShell home variable starting at `chars[at]` (a `$`): its name in
/// upper case and the index after it.
fn variable_at(chars: &[char], at: usize) -> Option<(String, usize)> {
    if chars.get(at) != Some(&'$') {
        return None;
    }
    let mut index = at + 1;
    let braced = chars.get(index) == Some(&'{');
    if braced {
        index += 1;
    }
    let env = chars.get(index..index + 4).is_some_and(|prefix| {
        prefix
            .iter()
            .collect::<String>()
            .eq_ignore_ascii_case("env:")
    });
    if env {
        index += 4;
    }
    let start = index;
    while index < chars.len()
        && index - start <= MAX_NAME
        && (chars[index].is_ascii_alphanumeric() || chars[index] == '_')
    {
        work::step();
        index += 1;
    }
    let name: String = chars[start..index]
        .iter()
        .collect::<String>()
        .to_uppercase();
    if braced {
        if chars.get(index) != Some(&'}') {
            return None;
        }
        index += 1;
    }
    let known = if env {
        HOME_VARIABLES.contains(&name.as_str())
    } else {
        matches!(name.as_str(), "HOME" | "PWD" | "PROFILE" | "PSHOME")
    };
    known.then_some((name, index))
}

/// Expand the home variables in `word`, PowerShell (see [`cmd_spellings`])
/// or cmd (`%USERPROFILE%`) spelled, in any letter case. Unknown names are
/// kept as written.
pub(crate) fn expand(word: &str, values: &Values<'_>) -> String {
    let word = cmd_spellings(word);
    if !word.contains('%') {
        return word;
    }
    let chars: Vec<char> = word.chars().collect();
    let mut out = String::with_capacity(word.len());
    let mut at = 0;
    while at < chars.len() {
        work::step();
        if chars[at] == '%' {
            let close = chars[at + 1..]
                .iter()
                .take(MAX_NAME + 1)
                .position(|ch| *ch == '%');
            if let Some(close) = close {
                let name: String = chars[at + 1..at + 1 + close]
                    .iter()
                    .collect::<String>()
                    .to_uppercase();
                if let Some(value) = values.get(&name) {
                    out.push_str(&value);
                    at += close + 2;
                    continue;
                }
            }
        }
        out.push(chars[at]);
        at += 1;
    }
    out
}

/// Whether text uses a home variable in a way these rules do not expand:
/// cmd's substring and replacement forms (`%USERPROFILE:~0%`,
/// `%HOMEPATH:a=b%`), .NET's user folders
/// (`[Environment]::GetFolderPath(...)`) and .NET lookups of a home
/// variable (`[Environment]::GetEnvironmentVariable('USERPROFILE')`).
pub(super) fn hides_a_home(text: &str) -> bool {
    let lower = text.to_lowercase();
    let names_a_home = |spelling: &dyn Fn(&str) -> String| {
        HOME_VARIABLES.iter().any(|name| {
            work::step();
            lower.contains(&spelling(&name.to_lowercase()))
        })
    };
    names_a_home(&|name| format!("%{name}:"))
        || lower.contains("getfolderpath")
        || (["getenvironmentvariable", "expandenvironmentvariables"]
            .iter()
            .any(|method| lower.contains(method))
            && (names_a_home(&|name| format!("'{name}'"))
                || names_a_home(&|name| format!("\"{name}\""))
                || names_a_home(&|name| format!("%{name}%"))))
}

/// The drive letter of a path starting `X:` (after `\` became `/`).
fn drive(slashed: &str) -> Option<char> {
    let mut chars = slashed.chars();
    let letter = chars.next()?;
    (letter.is_ascii_alphabetic() && chars.next() == Some(':')).then_some(letter)
}

/// The root of `slashed` (`\` already `/`) and the rest after it: a drive
/// (`c:`, also as `/c:` from [`normalize`]), a network share (`unc:/h/s`;
/// an administrative share `\\h\c$` is drive `c:`), or none.
fn root(slashed: &str) -> (String, &str) {
    let strip = |prefix: &str| {
        slashed
            .get(..prefix.len())
            .filter(|head| head.eq_ignore_ascii_case(prefix))
            .map(|_| &slashed[prefix.len()..])
    };
    fn unc(rest: &str) -> (String, &str) {
        let mut parts = rest.splitn(3, '/');
        let host = parts.next().unwrap_or_default();
        let share = parts.next().unwrap_or_default();
        let tail = parts.next().unwrap_or_default();
        let mut share_chars = share.chars();
        if let (Some(letter), Some('$'), None) =
            (share_chars.next(), share_chars.next(), share_chars.next())
            && letter.is_ascii_alphabetic()
        {
            return (format!("{letter}:"), tail);
        }
        (format!("unc:/{host}/{share}"), tail)
    }
    for prefix in ["//?/unc/", "//./unc/", "/unc:/"] {
        work::step();
        if let Some(rest) = strip(prefix) {
            return unc(rest);
        }
    }
    for prefix in ["//?/", "//./"] {
        work::step();
        if let Some(rest) = strip(prefix) {
            return match drive(rest) {
                Some(_) => (rest[..2].to_string(), &rest[2..]),
                None => unc(rest),
            };
        }
    }
    if let Some(rest) = slashed.strip_prefix("//") {
        return unc(rest);
    }
    if drive(slashed).is_some() {
        return (slashed[..2].to_string(), &slashed[2..]);
    }
    if let Some(rest) = slashed.strip_prefix('/')
        && drive(rest).is_some()
        && rest[2..].chars().next().is_none_or(|ch| ch == '/')
    {
        return (rest[..2].to_string(), &rest[2..]);
    }
    (String::new(), slashed)
}

/// Resolve `.`, `..`, separators, trailing dots and spaces, and stream
/// names without touching the filesystem. Keeps the letter case.
pub(super) fn normalize(path: &str) -> String {
    let slashed = path.replace('\\', "/");
    let (root, rest) = root(&slashed);
    let mut parts: Vec<&str> = Vec::new();
    for segment in rest.split('/') {
        work::step();
        match segment {
            "" | "." => {}
            ".." => {
                // A root is never left.
                parts.pop();
            }
            segment => {
                let name = segment
                    .split(':')
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches(['.', ' ']);
                if !name.is_empty() {
                    parts.push(name);
                }
            }
        }
    }
    let mut out = String::from("/");
    if !root.is_empty() {
        out.push_str(&root);
        if !parts.is_empty() {
            out.push('/');
        }
    }
    out.push_str(&parts.join("/"));
    out
}

/// Make `word` absolute against `cwd`, as Windows does: a drive or share
/// root is absolute, `\x` is on the working folder's drive, `C:x` is
/// relative to the working folder when it is on drive `C:` (else to the
/// drive's root, the classifier cannot know that drive's folder).
pub(super) fn join(cwd: &str, word: &str) -> String {
    let slashed = word.replace('\\', "/");
    let (root, rest) = root(&slashed);
    if cwd.is_empty() {
        return normalize(word);
    }
    let cwd = normalize(cwd);
    let (cwd_root, _) = self::root(&cwd);
    if !root.is_empty() {
        let drive_relative = drive(&slashed).is_some() && !rest.starts_with('/');
        if drive_relative && cwd_root.eq_ignore_ascii_case(&root) {
            return normalize(&format!("{cwd}/{rest}"));
        }
        return normalize(&format!("/{root}/{rest}"));
    }
    if slashed.starts_with('/') {
        if cwd_root.is_empty() {
            return normalize(&slashed);
        }
        return normalize(&format!("/{cwd_root}{slashed}"));
    }
    normalize(&format!("{cwd}/{slashed}"))
}

/// The folder holding `path` (normalized), for `~name`.
pub(super) fn parent(path: &str) -> String {
    let path = normalize(path);
    match path.rsplit_once('/') {
        Some((parent, _)) if !parent.is_empty() => parent.to_string(),
        _ => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn issue_1240_windows_paths_normalize() {
        for (path, expected) in [
            (r"C:\Users\fixture\.corbanu", "/C:/Users/fixture/.corbanu"),
            ("C:/Users/fixture/", "/C:/Users/fixture"),
            ("/c:/users/fixture", "/c:/users/fixture"),
            (r"C:\", "/C:"),
            (r"C:\a\..\..\b", "/C:/b"),
            (r"C:\a\.\b\\c", "/C:/a/b/c"),
            (r"\\?\C:\Users\x", "/C:/Users/x"),
            (r"\\.\C:\Users\x", "/C:/Users/x"),
            (
                r"\\server\share\.ssh\id_rsa",
                "/unc:/server/share/.ssh/id_rsa",
            ),
            (r"\\?\UNC\server\share\x", "/unc:/server/share/x"),
            (r"\\localhost\c$\Users\x", "/c:/Users/x"),
            ("/unc:/server/share/x", "/unc:/server/share/x"),
            (r"C:\x\auth.json.", "/C:/x/auth.json"),
            (r"C:\x\.netrc::$DATA", "/C:/x/.netrc"),
            (r"C:\x\.ssh. \id_rsa", "/C:/x/.ssh/id_rsa"),
            (r"\work\x", "/work/x"),
            ("relative", "/relative"),
        ] {
            assert_eq!(normalize(path), expected, "{path}");
            assert_eq!(normalize(&normalize(path)), expected, "{path} twice");
        }
    }

    #[test]
    fn issue_1240_windows_paths_join() {
        for (cwd, word, expected) in [
            (r"C:\work", "auth.json", "/C:/work/auth.json"),
            (r"C:\work", r"..\x", "/C:/x"),
            (r"C:\work", r"D:\x", "/D:/x"),
            (r"C:\work", r"\x\y", "/C:/x/y"),
            (r"C:\work", "C:x", "/C:/work/x"),
            (r"C:\work", "c:x", "/C:/work/x"),
            (r"C:\work", "D:x", "/D:/x"),
            (r"C:\work", "D:", "/D:"),
            (r"C:\work", "C:", "/C:/work"),
            (r"C:\work", r"\\h\s\x", "/unc:/h/s/x"),
            ("/c:/work", "x", "/c:/work/x"),
            (r"\\h\s\work", r"\x", "/unc:/h/s/x"),
            ("", r"C:\x", "/C:/x"),
            ("/work", r"\x", "/x"),
        ] {
            assert_eq!(join(cwd, word), expected, "{cwd} + {word}");
        }
    }

    #[test]
    fn issue_1240_windows_home_variables_expand() {
        let values = Values {
            codex_home: r"C:\Users\fixture\.corbanu",
            user_home: Some(r"C:\Users\fixture"),
            cwd: r"C:\work",
            places: false,
        };
        for (word, expected) in [
            (r"%USERPROFILE%\.ssh", r"C:\Users\fixture\.ssh"),
            (r"%userprofile%\.ssh", r"C:\Users\fixture\.ssh"),
            (r"%HOMEDRIVE%%HOMEPATH%\.ssh", r"C:\Users\fixture\.ssh"),
            (r"$env:USERPROFILE\.ssh", r"C:\Users\fixture\.ssh"),
            (r"$Env:UserProfile\.ssh", r"C:\Users\fixture\.ssh"),
            (r"${env:USERPROFILE}\.ssh", r"C:\Users\fixture\.ssh"),
            (r"$($env:USERPROFILE)\.ssh", r"C:\Users\fixture\.ssh"),
            (r"$HOME\.ssh", r"C:\Users\fixture\.ssh"),
            (r"${home}\.ssh", r"C:\Users\fixture\.ssh"),
            (
                r"$env:CODEX_HOME\auth.json",
                r"C:\Users\fixture\.corbanu\auth.json",
            ),
            (
                r"%CORBANU_HOME%\auth.json",
                r"C:\Users\fixture\.corbanu\auth.json",
            ),
            (r"%CD%\x", r"C:\work\x"),
            (r"C:\Users\%USERNAME%\.ssh", r"C:\Users\fixture\.ssh"),
            (
                r"%APPDATA%\..\..\.bashrc",
                r"C:\Users\fixture\AppData\Roaming\..\..\.bashrc",
            ),
            (r"$env:LOCALAPPDATA\x", r"C:\Users\fixture\AppData\Local\x"),
            (r"$PWD\x", r"C:\work\x"),
            // Unknown or longer names stay.
            (r"%TEMP%\x", r"%TEMP%\x"),
            (r"$env:USERPROFILEX\x", r"$env:USERPROFILEX\x"),
            (r"$HOMEX\x", r"$HOMEX\x"),
            ("100%", "100%"),
        ] {
            assert_eq!(expand(word, &values), expected, "{word}");
        }
        assert!(hides_a_home(r"type %USERPROFILE:~0%\.bashrc"));
        assert!(hides_a_home("[Environment]::GetFolderPath('UserProfile')"));
        assert!(hides_a_home(
            "[Environment]::GetEnvironmentVariable(\"USERPROFILE\")"
        ));
        assert!(hides_a_home(
            "[Environment]::ExpandEnvironmentVariables('%HOMEPATH%')"
        ));
        assert!(!hides_a_home(
            "[Environment]::GetEnvironmentVariable('PATH')"
        ));
        assert!(!hides_a_home(r"type %USERPROFILE%\.bashrc"));
    }
}
