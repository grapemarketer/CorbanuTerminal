//! #1309 follow-up: the git-config check (`git config -f FILE`) and the
//! output-file check (`--output=FILE`) on the line's own words share one
//! rule: a file named relative to the folder asks when the folder is
//! unknown. Each case below was quiet on main for at least one of the two
//! writes; the same writes inside `sh -c '…'` already asked.

use super::ProtectedActionKind;
use super::script;
use super::work;
use pretty_assertions::assert_eq;

/// The git-config check: a relative `-f` file, with the comma value that
/// makes the word lexer read the write as a read (#1283).
const GIT_CONFIG: &str = "git config -f .zshenv --comment x,--get user.name x";
/// The output-file check: a relative `--output=` file.
const OUTPUT_FILE: &str = "git log --output=.zshenv";

/// `{W}` is the write.
const UNKNOWN: &[&str] = &[
    // Variables neither reading knows.
    "cd \"$X\"; {W}",
    "cd \"$XDG_CONFIG_HOME\"; {W}",
    "printf -v D %s ~; cd \"$D\"; {W}",
    "read D <<< ~; cd \"$D\"; {W}",
    "for D in ~; do cd \"$D\"; {W}; done",
    // Where `cd` looks things up.
    "CDPATH=~ cd .; {W}",
    "export CDPATH=~; cd .; {W}",
    "CDPATH=/home cd fixture; {W}",
    "OLDPWD=~; cd -; {W}",
    "shopt -s cdable_vars; h=~; cd h; {W}",
    // A `cd` that runs later than the write, or in a function.
    "for i in 1 2; do {W}; cd ~; done",
    "while :; do {W}; cd ~; done",
    "trap \"{W}\" EXIT; cd ~",
    "f() { {W}; }; cd ~; f",
    "f() { {W}; }; D=~; cd \"$D\"; f",
    "go() { cd ~; }; cd /tmp; go; {W}",
    "go() { cd \"$1\"; }; go ~; {W}",
    "eval 'cd ~'; {W}",
    // Positional parameters.
    "sh -c 'cd \"$1\"; {W}' sh ~",
    "bash -c 'cd \"$1\" && {W}' bash ~",
    "set -- ~; cd \"$1\"; {W}",
    // A wrapper's folder option, attached or clustered.
    "D=~; env -C\"$D\" {W}",
    "env -C~ {W}",
    "env -iC ~ {W}",
    "env --chd ~ {W}",
    "nsenter -w\"$HOME\" {W}",
    "nsenter -W\"$HOME\" {W}",
    "nsenter --wdns=\"$HOME\" {W}",
    // The wrappers #1309 names, and the other folder options and
    // elsewhere starters #1310/#1523/#1530 read for nested scripts.
    "sudo -D ~ {W}",
    "sudo -D~ {W}",
    "nsenter --wd=\"$HOME\" {W}",
    "docker exec -w ~ c {W}",
    "podman exec -w ~ c {W}",
    "systemd-run --working-directory=\"$HOME\" {W}",
    "systemd-run -p WorkingDirectory=~ {W}",
    "systemd-run {W}",
    "unshare -w ~ {W}",
    "chroot ~ {W}",
    "schroot -c x -d ~ -- {W}",
    "apptainer exec --pwd ~ img {W}",
    "singularity exec --pwd ~ img {W}",
    "buildah run --workingdir ~ c {W}",
    "run0 -D ~ {W}",
    "run0 --chdir=\"$HOME\" {W}",
    "sudo -i {W}",
    "machinectl shell .host {W}",
    "pkexec {W}",
    "find ~/.bashrc -execdir {W} \\;",
    "find ~/.bashrc -okdir {W} \\;",
];

/// Known folders, and files not named relative to the folder: quiet
/// before and after.
const KNOWN: &[&str] = &[
    "{W}",
    "cd /tmp; {W}",
    "cd /tmp && {W}",
    "cd /tmp; sh -c '{W}'",
    "f() { {W}; }; f",
    "for i in 1 2; do {W}; done",
    "sudo {W}",
    "env A=1 {W}",
    "nice -n 5 {W}",
];

#[test]
fn issue_1309_line_writes_in_unknown_folders_ask() {
    let mut quiet = Vec::new();
    for template in UNKNOWN {
        for write in [GIT_CONFIG, OUTPUT_FILE] {
            let line = template.replace("{W}", write);
            if script(&line).is_none() {
                quiet.push(line);
            }
        }
    }
    assert_eq!(quiet, Vec::<String>::new());
    // Where the folder is unknown, the write asks as a persistence write.
    assert_eq!(
        script("sudo -D ~ git log --output=.zshenv"),
        Some(ProtectedActionKind::Persistence)
    );
}

#[test]
fn issue_1309_line_writes_in_known_folders_stay_quiet() {
    let mut asked = Vec::new();
    for template in KNOWN {
        for write in [GIT_CONFIG, OUTPUT_FILE] {
            let line = template.replace("{W}", write);
            if let Some(kind) = script(&line) {
                asked.push(format!("{line} -> {kind:?}"));
            }
        }
    }
    for line in [
        // Git's own `-C` is followed, not unknown.
        "git -C /tmp log --output=.zshenv",
        "git -C /tmp config -f .zshenv --comment x,--get user.name x",
        // An absolute file is the same file in any folder.
        "sudo -D ~ git log --output=/tmp/log.txt",
        "f() { git log --output=/tmp/log.txt; }; cd ~; f",
    ] {
        if let Some(kind) = script(line) {
            asked.push(format!("{line} -> {kind:?}"));
        }
    }
    assert_eq!(asked, Vec::<String>::new());
}

/// The new readings stay linear.
#[test]
fn issue_1309_line_writes_checks_are_linear() {
    for (label, prefix, unit, suffix) in [
        (
            "wrapper words",
            "docker exec -w ~ c ",
            "git ",
            "log --output=out.txt",
        ),
        (
            "wrapper options",
            "sudo -D ~ ",
            "-E ",
            "git log --output=out.txt",
        ),
        (
            "deferred writes",
            "f() { :; }; ",
            "git log --output=out.txt; ",
            "cd ~",
        ),
    ] {
        work::assert_linear(label, |scale| {
            let line = format!("{prefix}{}{suffix}", unit.repeat(200 * scale));
            let _ = script(&line);
        });
    }
}
