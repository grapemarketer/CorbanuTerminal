//! #1704: commands writing zsh's and bash's tables of code run by name
//! (`code_tables.rs`), and, from round 8 of #1698's review, words the
//! classifier cannot see where such a writer reads its options.

use super::kind;
use super::work;
use pretty_assertions::assert_eq;

/// #1704, round 8 of #1698's review: where a writer reads its options, a
/// word the classifier cannot see may be an option taking a name (`o=-A;
/// set $o commands ls ./x` defines `ls`) or, unquoted, several words
/// (`set $(cat f)`); so may an unquoted one before a name (`zstyle -a
/// $(cat f)`). One word with a word after it asks too (`"$o"` may be
/// `-Acommands`). Read with quotes, in text read as shell and in `eval` and
/// `trap` text. `zselect` and `zsystem flock` read options after operands.
/// Only adds prompts.
#[test]
fn issue1704_hidden_words_where_writers_read_options_ask() {
    for shell in ["bash", "zsh"] {
        let line = |script: &str| kind(&[shell, "-lc", script]);
        for script in [
            // Round 8.
            "o=$(cat f); set $o commands ls ./x; ls",
            "o=$(cat f); zstyle $o :x y functions; f",
            "set $(cat f); ls",
            "zstyle -a $(cat f); f",
            "zselect -t 0 -r 0 -A $n",
            "zsystem flock -t 0 -f $n file",
            // The class: an option, an option's value, a pattern.
            "set -$o commands ls ./x",
            "set -e \"$o\" commands ls ./x",
            "set -o $(cat f)",
            "set ?A commands ls ./x",
            "set *~a/b",
            "set ^*.png",
            "set -.#A commands ls ./x",
            "set -e? commands ls ./x",
            "print -r* x",
            "print -l *a.b]",
            // One word holding an option and its name, or several in
            // quotes or zsh's own spellings (review 1).
            "o=$(cat f); set \"$o\" ls ./x; ls",
            "o=$(cat f); print \"$o\" 'echo pwned'; f",
            "printf \"$fmt\" x",
            "o=$(cat f); set $=o; ls",
            "o=$(cat f); set \"$=o\"; ls",
            "o=-A; set $~o commands ls ./x",
            "set $^o commands ls ./x",
            "set -- $(cat f); set \"$@\"; ls",
            "set \"${(@f)$(<f)}\"; ls",
            "mapfile -t a < f; printf \"${a[@]}\"; ls",
            "eval 'set ?A commands ls ./x'",
            "trap 'set ?A commands ls ./x' EXIT",
            "zstyle -$o :x y functions",
            "zstyle \"$o\" :x y functions",
            "zstyle -s $c y n",
            "printf $o 'BASH_CMDS[ls]' ./x",
            "printf \"$o\" 'BASH_CMDS[ls]' ./x",
            "print $o 'functions[f]' 'tee -a ~/.zshrc'",
            "print -r $(cat f)",
            "zstat $o functions x",
            "zstat \"$o\" \"$n\" x",
            "stat -f %m $(cat f)",
            "zformat $o $n x",
            "pcre_match $o $n x",
            "compadd -X $(cat f) x",
            "sysread -i $fd line",
            // Before a name several words may move.
            "getopts $(cat f)",
            "zlistattr $(cat f)",
            "zgetattr x $(cat f)",
            "zgetattr \"$f\" \"$attr\" value",
            // Options after operands.
            "zselect 0 -A $n",
            "zselect -r 0 -a$n",
            "zsystem flock -f $n x",
            "zsystem flock -t 1 -f \"$(cat n)\" x",
            // Behind wrappers, `eval`, `sh -c`.
            "builtin zstyle $o :x y functions",
            "eval 'zstyle $o :x y functions'",
            "sh -c 'zstyle $o :x y functions'",
        ] {
            assert!(line(script).is_some(), "{shell} {script:?}");
        }
        for script in [
            "set -- $(cat f)",
            "set -euo pipefail",
            "set x $y",
            "printf '%s\\n' \"$x\"",
            "printf \"$fmt\"",
            "printf -- \"$x\"",
            "printf '* -text\\n' > f",
            "printf '<%s>' x",
            "print -r -- $x",
            "print -r \"$x\"",
            "stat -c %s \"$f\"",
            "stat -f %m \"$f\"",
            "stat -f %m ~/x",
            "set -e */x",
            "print -l *.png",
            "set x.# y",
            "zstat -A info x \"$f\"",
            "zgetattr \"$f\" user.y value",
            "zstyle ':completion:*' menu select",
            "zstyle -s \"$c\" y n",
            "zstyle :x y \"$v\"",
            "wait $pid",
            "wait -n $!",
            "zselect -t 10 -r $fd",
            "zsystem flock -t 1 \"$f\"",
            "getopts ab: opt",
            // awk programs, read again as code, are not shell.
            "gh pr checks 1 | awk -F'\\t' '{print $2}' | sort",
            "awk '/^x$/{getline; print NR\": \"$0}' f",
        ] {
            assert_eq!(line(script), None, "{shell} {script:?}");
        }
    }
}

/// #1704: reading a writer's options stays linear in its words.
#[test]
fn issue1704_option_checks_are_linear() {
    work::assert_linear("many options", |scale| {
        let line = format!("set {}x $y", "-e -o pipefail ".repeat(200 * scale));
        let _ = kind(&["zsh", "-lc", &line]);
    });
    work::assert_linear("options among operands", |scale| {
        let line = format!("zselect {}", "0 -t 1 ".repeat(200 * scale));
        let _ = kind(&["zsh", "-lc", &line]);
    });
    work::assert_linear("words after one unseen", |scale| {
        let line = format!("print \"$o\" {}", "x ".repeat(400 * scale));
        let _ = kind(&["zsh", "-lc", &line]);
    });
    work::assert_linear("unseen words where options take no names", |scale| {
        let line = format!("zgetattr {}x", "\"$a\" ".repeat(400 * scale));
        let _ = kind(&["zsh", "-lc", &line]);
    });
    work::assert_linear("eval text", |scale| {
        let line = format!("eval 'set -e {}x'", "\"$a\" y ".repeat(200 * scale));
        let _ = kind(&["zsh", "-lc", &line]);
    });
    work::assert_linear("many zstyles", |scale| {
        let _ = kind(&["zsh", "-lc", &"zstyle -a :x y z; ".repeat(200 * scale)]);
    });
}

/// #1704: a command writing variables named by its operands (zparseopts,
/// zstyle, zsh modules, `read`, `set -A`, declarations, `printf -v`) asks
/// when a word names a table of code run by name (zsh's `functions`, alias
/// tables, `commands`, bash's `BASH_ALIASES`/`BASH_CMDS`), or where it
/// takes a name from a word the classifier cannot see (an expansion, a
/// glob); so does a write to one in an expansion. The review-6 and
/// review-7 forms (each ran its payload in zsh 5.9) and the class. Only
/// adds prompts.
#[test]
fn issue1704_code_table_writers_fail_closed() {
    for shell in ["bash", "zsh"] {
        let line = |script: &str| kind(&[shell, "-lc", script]);
        for script in [
            // Review 6.
            "set -- -f 'tee -a ~/.zshrc'; zparseopts -a functions -- f:; -f",
            "set -- --x 'tee -a ~/.zshrc'; zparseopts -- -x:=functions; --x",
            "zstyle :x y f 'tee -a ~/.zshrc'; zstyle -a :x y functions; f",
            // Attached, among other options, other specs and writers.
            "zparseopts -afunctions -- f:",
            "zparseopts -E -D -a functions -- f:",
            "zparseopts -D -- f:=functions",
            "zparseopts -A aliases -- f:",
            "zparseopts -a dis_functions -- f:",
            "zstyle -g functions :x y",
            "zstyle -ga :x y aliases",
            "pcre_match -a functions x y",
            "zformat -a functions : x:y",
            "zregexparse i p functions x",
            "zselect -A functions 0",
            "getopts f: functions",
            "vared functions",
            "let 'functions[f]=1'",
            "read -A galiases < x",
            "mapfile -t BASH_ALIASES < x",
            "set -A saliases sh 'tee -a ~/.zshrc'",
            "set -A commands ls ./x",
            "printf -v functions %s x",
            "print -v 'aliases[x]' 'tee -a ~/.zshrc'",
            "typeset -A functions=(f 'tee -a ~/.zshrc')",
            "local -A aliases=(x y)",
            "declare -n r=BASH_CMDS",
            "typeset -n r=aliases",
            // Behind modifiers, wrappers, `eval`, `trap`, `repeat`.
            "noglob zparseopts -a functions -- f:",
            "builtin zstyle -a :x y functions",
            "IFS=: noglob builtin read -A functions < x",
            "eval 'zparseopts -a functions -- f:'",
            "eval -- zstyle -a :x y functions",
            "trap 'zparseopts -a functions -- f:' EXIT",
            "repeat 1 zparseopts -a functions -- f:",
            "alias z='zparseopts -a functions -- f:'",
            // The name from a value.
            "t=functions; zparseopts -a $t -- f:",
            "t=aliases; set -A \"$t\" x 'tee -a ~/.zshrc'",
            // Review 7: a name the classifier cannot see, where one goes.
            "set -- -f 'tee -a ~/.zshrc'; n=$(printf %s func tions); zparseopts -a $n -- f:; -f",
            "zstyle :x y f 'tee -a ~/.zshrc'; zstyle -a :x y \"$(printf %s func tions)\"; f",
            "n=$(printf %s com mands); set -A $n ls ./x; ls",
            "zparseopts -a ${:-func}tions -- f:",
            "touch functions; zparseopts -a function[s] -- f:",
            "zparseopts -D -- \"f:=$n\"",
            "zparseopts -a functi{on,}s -- f:",
            "zstyle -g $n :x y",
            "set -eA `cat n` x",
            "set +A $n x",
            "zformat -f $n x y",
            "pcre_match -a$n x y",
            "zstat -A $n x",
            "getln -A $n",
            "wait -n -p $n",
            "compgen -V $n -W x",
            "eval 'zparseopts -a $n -- f:'",
            "zlistattr x $n",
            "zgetattr x user.y $n",
            // The ~ ^ + flags before a table's name.
            ": ${~functions[f]::=./x}; f",
            ": ${^commands[ls]::=./x}; ls",
            ": ${^^~~aliases[x]::=./x}; x",
            // zsh/attr writes attribute names (chosen by whoever sets
            // them) into an array by name.
            "zlistattr x functions",
            "zgetattr x user.y functions",
            // In expansions, and tables of programs.
            ": ${(AA)=aliases::=x 'tee -a ~/.zshrc'}",
            ": ${BASH_ALIASES[x]:='tee -a ~/.zshrc'}",
            ": ${commands[ls]::=./x}; ls",
            ": ${BASH_CMDS[ls]:=./x}; ls",
            "commands[ls]=./x; ls",
            "BASH_CMDS[ls]=./x; ls",
            "commands=(ls ./x)",
            // A value naming a table may name it later.
            "x=commands; eval \"$x[ls]=./x\"; ls",
            "local x=functions",
        ] {
            assert!(line(script).is_some(), "{shell} {script:?}");
        }
        for script in [
            "firebase deploy --only=functions",
            "ls -A functions",
            "set -- functions",
            "grep -rn functions src",
            "jq '.functions[0]' f.json",
            "zparseopts -D -E -a opts -- f: -x:=xs",
            "zstyle ':completion:*:functions' ignored-patterns '_*'",
            "read -r myfunctions < x",
            "printf '%s\\n' functions",
            "stat functions/index.js",
            "echo ${commands[git]}",
            "(( $+commands[git] )) && echo yes",
            "aliases[ll]='ls -l'",
            // Operands that are not names stay quiet.
            "set -- \"$@\"",
            "set -e -o pipefail -- $args",
            "zparseopts -D -E -a opts -- h=help -verbose=verbose",
            "zstyle ':x' y \"$v\"",
            "zstyle -s ':completion:*' menu style",
            "zstat -A info \"$f\"",
            "zlistattr \"$f\" attrs",
            "stat -c %A \"$f\"",
            "wait $pid",
        ] {
            assert_eq!(line(script), None, "{shell} {script:?}");
        }
    }
}
