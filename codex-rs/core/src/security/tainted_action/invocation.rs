//! Where a shell or interpreter invocation gets the code it runs. Options are
//! read per family and only up to the first operand, as the program itself
//! reads them: `bash -e p.sh` runs `p.sh`, `node -r ./p.js x.js` preloads
//! `p.js`, `perl -pe '...' f` runs inline code.

use super::shell::SHELLS;
use super::work;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Code<'a> {
    /// Inline code: the word after the flag. `None` when the code is not
    /// among these words (a shell's `-c` script given as its own argument is
    /// lexed as the next command; `xargs sh -c` gets it from its input).
    Inline(Option<&'a str>),
    /// A script file.
    File(&'a str),
    /// A Python module (`python -m name`): read when it is a local file.
    Module(&'a str),
    /// Code from stdin.
    Stdin,
    /// A subcommand that runs project tasks or tools (`bun install`,
    /// `deno task x`); not code the classifier can read.
    Tool,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Invocation<'a> {
    pub(super) code: Code<'a>,
    /// Files loaded before the code (`node -r ./p.js`, `ruby -r ./p.rb`,
    /// `bash --rcfile f`).
    pub(super) preloads: Vec<&'a str>,
}

/// Parse an invocation of `name` (a lowercase basename) with `args`.
/// `None` when `name` is not a shell or interpreter.
pub(super) fn parse<'a>(name: &str, args: &'a [String]) -> Option<Invocation<'a>> {
    let mut start_up = Vec::new();
    let mut invocation = parse_options(name, args, &mut start_up)?;
    // #1422: a shell running a script file reads `--rcfile`/`--init-file`
    // only when interactive (`-i`). Any `i` letter in an option word
    // counts, wherever it is. With `-c`, bash also reads it when it seems
    // to run under sshd, and from its input it may be interactive. An `o`
    // or `O` in an option word takes the next word (bash `-ov vi`), which
    // `Family::option` reads as the script, so then it is judged too.
    if !start_up.is_empty() && (!matches!(invocation.code, Code::File(_)) || may_start_up(args)) {
        invocation.preloads.extend(start_up);
    }
    Some(invocation)
}

/// `parse`, setting aside in `start_up` the start-up files that a shell
/// reads only when interactive.
fn parse_options<'a>(
    name: &str,
    args: &'a [String],
    start_up: &mut Vec<&'a str>,
) -> Option<Invocation<'a>> {
    if matches!(name, "source" | ".") {
        let code = args.first().map_or(Code::Stdin, |file| Code::File(file));
        return Some(Invocation {
            code,
            preloads: Vec::new(),
        });
    }
    let family = Family::of(name)?;
    let mut preloads = Vec::new();
    let mut index = 0;
    let mut subcommand_seen = false;
    while let Some(arg) = args.get(index) {
        work::step();
        let next = args.get(index + 1).map(String::as_str);
        index += 1;
        if arg == "--" {
            if let Some(file) = next {
                return Some(Invocation {
                    code: Code::File(file),
                    preloads,
                });
            }
            break;
        }
        if arg == "-" {
            return Some(Invocation {
                code: Code::Stdin,
                preloads,
            });
        }
        // #1405: bash also reads `-rcfile f` as `--rcfile f`; other shells
        // read its letters, so they are read too (`-rcfile` holds `-c`).
        if family == Family::Shell && matches!(arg.as_str(), "-rcfile" | "-init-file") {
            preloads.extend(next);
        }
        let is_option = arg.starts_with('-') || (family == Family::Shell && arg.starts_with('+'));
        if !is_option {
            // Subcommands that pick what runs (`deno run x.ts`, `bun run x`).
            match (family, subcommand_seen, arg.as_str()) {
                (Family::Deno, false, "run") | (Family::Tsx, false, "watch") => {
                    subcommand_seen = true;
                    continue;
                }
                (Family::Deno, false, "eval") => {
                    return Some(Invocation {
                        code: Code::Inline(next),
                        preloads,
                    });
                }
                (Family::Deno, false, word) if !word.contains(['.', '/']) => {
                    return Some(Invocation {
                        code: Code::Tool,
                        preloads,
                    });
                }
                (Family::Bun, false, "run") => {
                    subcommand_seen = true;
                    continue;
                }
                (Family::Bun, _, word) if !word.contains(['.', '/']) => {
                    // A package.json script or a bun tool.
                    return Some(Invocation {
                        code: Code::Tool,
                        preloads,
                    });
                }
                _ => {
                    return Some(Invocation {
                        code: Code::File(arg),
                        preloads,
                    });
                }
            }
        }
        match family.option(arg) {
            Opt::Inline => {
                return Some(Invocation {
                    code: Code::Inline(next),
                    preloads,
                });
            }
            Opt::InlineAttached(code) => {
                return Some(Invocation {
                    code: Code::Inline(Some(code)),
                    preloads,
                });
            }
            Opt::File => {
                return Some(Invocation {
                    code: next.map_or(Code::Stdin, Code::File),
                    preloads,
                });
            }
            Opt::FileAttached(file) => {
                return Some(Invocation {
                    code: Code::File(file),
                    preloads,
                });
            }
            Opt::Module => {
                return Some(Invocation {
                    code: next.map_or(Code::Stdin, Code::Module),
                    preloads,
                });
            }
            Opt::ModuleAttached(module) => {
                return Some(Invocation {
                    code: Code::Module(module),
                    preloads,
                });
            }
            Opt::Preload => {
                preloads.extend(next);
                index += 1;
            }
            Opt::PreloadAttached(file) => preloads.push(file),
            Opt::StartUp => {
                start_up.extend(next);
                index += 1;
            }
            Opt::StartUpAttached(file) => start_up.push(file),
            Opt::Value => index += 1,
            Opt::Stdin => {
                return Some(Invocation {
                    code: Code::Stdin,
                    preloads,
                });
            }
            Opt::Flag => {}
        }
    }
    Some(Invocation {
        code: Code::Stdin,
        preloads,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    Shell,
    Python,
    Node,
    Deno,
    Bun,
    Tsx,
    PerlRuby,
    Php,
    Lua,
    Osascript,
    Pwsh,
    /// Tcl, expect, swift, R: `-c`/`-e` code, `-f` file, else a file or stdin.
    Generic,
}

enum Opt<'a> {
    Inline,
    InlineAttached(&'a str),
    File,
    FileAttached(&'a str),
    Module,
    ModuleAttached(&'a str),
    Preload,
    PreloadAttached(&'a str),
    /// #1422: a start-up file an interactive shell reads.
    StartUp,
    StartUpAttached(&'a str),
    Value,
    Stdin,
    Flag,
}

impl Family {
    fn of(name: &str) -> Option<Self> {
        if SHELLS.contains(&name) {
            return Some(Self::Shell);
        }
        // `python3.12`, `perl5.34`, `node18`: the version does not matter.
        let base = name.trim_end_matches(|ch: char| ch.is_ascii_digit() || ch == '.');
        Some(match base {
            "python" | "pypy" => Self::Python,
            "node" | "nodejs" | "ts-node" => Self::Node,
            "deno" => Self::Deno,
            "bun" => Self::Bun,
            "tsx" => Self::Tsx,
            "perl" | "ruby" => Self::PerlRuby,
            "php" => Self::Php,
            "lua" | "luajit" => Self::Lua,
            "osascript" => Self::Osascript,
            "pwsh" | "powershell" => Self::Pwsh,
            "tclsh" | "wish" | "expect" | "swift" | "rscript" => Self::Generic,
            _ => return None,
        })
    }

    /// How this family reads one option word: long options by name, short
    /// groups one letter at a time, with attached values (`-c$X`, `-Ilib`).
    fn option<'a>(self, arg: &'a str) -> Opt<'a> {
        if self == Self::Pwsh {
            return match arg.to_lowercase().as_str() {
                "-c" | "-command" => Opt::Inline,
                "-f" | "-file" => Opt::File,
                _ => Opt::Flag,
            };
        }
        let scripted = matches!(self, Self::Node | Self::Deno | Self::Bun | Self::Tsx);
        if let Some(long) = arg.strip_prefix("--") {
            let (key, attached) = match long.split_once('=') {
                Some((key, value)) => (key, Some(value)),
                None => (long, None),
            };
            return match key {
                "command" if self == Self::Shell => Opt::Inline,
                // #1405: an interactive shell sources its start-up file
                // first (`bash --rcfile f -i`, yash `--profile=f -l`).
                // #1422: for a script file, only when interactive; as
                // written (bash's spellings). Before #1405 they were values,
                // not judged.
                "rcfile" | "init-file" if self == Self::Shell => {
                    attached.map_or(Opt::StartUp, Opt::StartUpAttached)
                }
                _ if self == Self::Shell && start_up_file(key) => {
                    attached.map_or(Opt::Preload, Opt::PreloadAttached)
                }
                "check-hash-based-pycs" if self == Self::Python => {
                    attached.map_or(Opt::Value, |_| Opt::Flag)
                }
                "eval" | "print" if scripted => attached.map_or(Opt::Inline, Opt::InlineAttached),
                "require" | "import" | "loader" | "experimental-loader" | "preload" if scripted => {
                    attached.map_or(Opt::Preload, Opt::PreloadAttached)
                }
                "env-file" | "inspect-port" | "title" | "cwd" if scripted => {
                    attached.map_or(Opt::Value, |_| Opt::Flag)
                }
                _ => Opt::Flag,
            };
        }
        let group = arg.trim_start_matches(['-', '+']);
        for (at, letter) in group.char_indices() {
            work::step();
            let rest = &group[at + letter.len_utf8()..];
            let attached = (!rest.is_empty()).then_some(rest);
            let inline = attached.map_or(Opt::Inline, Opt::InlineAttached);
            let value = attached.map_or(Opt::Value, |_| Opt::Flag);
            let preload = attached.map_or(Opt::Preload, Opt::PreloadAttached);
            let file = attached.map_or(Opt::File, Opt::FileAttached);
            return match (self, letter) {
                (Self::Shell, 'c') => Opt::Inline,
                (Self::Shell, 's') => Opt::Stdin,
                (Self::Shell, 'o' | 'O') => value,
                (Self::Python, 'c') => inline,
                (Self::Python, 'm') => attached.map_or(Opt::Module, Opt::ModuleAttached),
                (Self::Python, 'W' | 'X') => value,
                (_, 'e' | 'p') if scripted => inline,
                (_, 'r') if scripted => preload,
                (Self::PerlRuby, 'e' | 'E') => inline,
                (Self::PerlRuby, 'r') => preload,
                (Self::PerlRuby, 'I') => value,
                // Letters whose value can only be attached.
                (Self::PerlRuby, 'M' | 'm' | 'x' | 'C' | 'l' | 'i' | 'F' | '0' | 'd' | 'D') => {
                    Opt::Flag
                }
                (Self::Php, 'r') => inline,
                (Self::Php, 'f') => file,
                (Self::Php, 'd' | 'c' | 'z') => value,
                (Self::Lua, 'e') => inline,
                (Self::Lua, 'l') => value,
                (Self::Osascript, 'e') => inline,
                (Self::Osascript, 'l' | 's') => value,
                (Self::Generic, 'c' | 'e') => inline,
                (Self::Generic, 'f') => file,
                _ => continue,
            };
        }
        Opt::Flag
    }
}

/// #1405: whether a shell's long option `key` names a start-up file it
/// sources: `--rcfile`, `--init-file`, yash `--profile`. yash also takes
/// any case, ignores other than letters and digits, and takes a prefix of
/// two letters or more (`--rc=f`, `--RC-FILE=f`, `--prof f`).
fn start_up_file(key: &str) -> bool {
    let key = normalized(key);
    key.len() >= 2
        && ["rcfile", "initfile", "profile"]
            .iter()
            .any(|name| name.starts_with(key.as_str()))
}

/// A long option name as yash compares it: letters and digits, lowercased.
fn normalized(key: &str) -> String {
    key.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

/// #1422: whether a shell invoked with `args` and a script file may read
/// its start-up file: it may be interactive (an option word with an `i`
/// letter, `-i`, `-il`, `-rcfile`, or a long one yash may read as
/// `--interactive`, `--in`), or an option word holds `o`/`O`, whose value
/// may be the word read as the script. Read loosely, so the file is
/// judged: any word counts, operands too.
fn may_start_up(args: &[String]) -> bool {
    args.iter().any(|arg| {
        work::step();
        if let Some(long) = arg.strip_prefix("--") {
            let key = normalized(long.split_once('=').map_or(long, |(key, _)| key));
            key.len() >= 2 && "interactive".starts_with(key.as_str())
        } else {
            arg.starts_with(['-', '+']) && arg.contains(['i', 'o', 'O'])
        }
    })
}
