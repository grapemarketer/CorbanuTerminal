use super::*;
use crate::sandboxing::SandboxPermissions;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use std::path::PathBuf;

const HOME: &str = "/home/fixture/.corbanu";
const USER_HOME: &str = "/home/fixture";

fn fixture(path: &str) -> PathBuf {
    if cfg!(windows) && !Path::new(path).is_absolute() {
        PathBuf::from(format!("C:{}", path.replace('/', "\\")))
    } else {
        PathBuf::from(path)
    }
}

fn abs(path: &str) -> PathUri {
    PathUri::from_abs_path(
        &AbsolutePathBuf::from_absolute_path_checked(fixture(path)).expect("absolute"),
    )
}

fn script(line: &str) -> Option<ProtectedActionKind> {
    let action = ApprovalAction::Shell {
        id: "call".into(),
        environment_id: "local".into(),
        command: ["bash", "-lc", line]
            .into_iter()
            .map(str::to_string)
            .collect(),
        cwd: abs("/work"),
        sandbox_permissions: SandboxPermissions::UseDefault,
        additional_permissions: None,
        justification: None,
    };
    classify_with(&action, &fixture(HOME), Some(&fixture(USER_HOME)))
}

#[cfg(unix)]
fn in_workspace(files: &[(&str, &str)], line: &str) -> Option<ProtectedActionKind> {
    let root = tempfile::tempdir().expect("tempdir");
    let work = root.path();
    std::fs::create_dir_all(work.join("sub")).expect("sub");
    for (name, text) in files {
        let path = work.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("folder");
        }
        std::fs::write(path, text).expect("file");
    }
    let cwd = work.to_string_lossy().into_owned();
    let action = ApprovalAction::Shell {
        id: "call".into(),
        environment_id: "local".into(),
        command: ["bash", "-lc", line]
            .into_iter()
            .map(str::to_string)
            .collect(),
        cwd: abs(&cwd),
        sandbox_permissions: SandboxPermissions::UseDefault,
        additional_permissions: None,
        justification: None,
    };
    classify_with(&action, &fixture(HOME), Some(work))
}

/// #1773: sudo's chroot option, unambiguous value-taking long-option
/// prefixes and clusters with an unknown letter before a known value option
/// all leave the real program after the option's value.
#[test]
fn issue_1773_sudo_value_options_keep_the_real_program() {
    let words = |line: &str| line.split(' ').map(str::to_string).collect::<Vec<_>>();
    for (line, expected) in [
        ("sudo -R dir ./x", vec![0, 3]),
        ("sudo --chroot dir ./x", vec![0, 3]),
        ("sudo -R x sudo ./x", vec![0, 3, 4]),
        ("sudo --us root ./x", vec![0, 3]),
        ("sudo -xu root ./x", vec![0, 3]),
        ("sudo --us root sudo ./x", vec![0, 3, 4]),
        ("sudo -R x sudo -A true", vec![0, 3, 5]),
        ("sudo -R x sudo -e f", vec![0, 3, 5]),
        ("sudo --us root sudo -A true", vec![0, 3, 5]),
        // Main's separate `env` wrapper disappears once `-R` consumes it.
        ("sudo -R env -A true", vec![0, 4]),
    ] {
        assert_eq!(indirect::programs(&words(line)), expected, "{line}");
    }
}

/// #1773: every line that was quiet on main follows the actual script after
/// sudo has consumed its value option. Harmless scripts remain quiet.
#[cfg(unix)]
#[test]
fn issue_1773_sudo_value_options_read_the_script_that_runs() {
    let harmful = "echo x >> ~/.zshenv\n";
    let mut wrong = Vec::new();
    for line in [
        "sudo -R x ./x",
        "sudo -R sub ./x",
        "sudo --us root ./x",
        "sudo -xu root ./x",
        "sudo -R x sudo ./x",
        "sudo --us root sudo ./x",
    ] {
        if in_workspace(&[("x", harmful), ("sub/x", harmful)], line).is_none() {
            wrong.push(line);
        }
    }
    assert_eq!(wrong, Vec::<&str>::new(), "should ask");

    for line in [
        "sudo -R x ./x",
        "sudo -R sub ./x",
        "sudo --us root ./x",
        "sudo -xu root ./x",
    ] {
        if in_workspace(&[("x", "true\n"), ("sub/x", "true\n")], line).is_some() {
            wrong.push(line);
        }
    }
    assert_eq!(wrong, Vec::<&str>::new(), "should be quiet");
}

/// #1773: after those options, a nested sudo's helper options are read rather
/// than hidden behind the value that programs skipped on main.
#[test]
fn issue_1773_sudo_value_options_do_not_hide_helpers() {
    use ProtectedActionKind::UnseenCode;

    for line in [
        "sudo -R x sudo -A true",
        "sudo -R x sudo -e f",
        "sudo --us root sudo -A true",
    ] {
        assert_eq!(script(line), Some(UnseenCode), "{line}");
    }
    for line in [
        "sudo -R x true",
        "sudo --us root true",
        "sudo -xu root true",
    ] {
        assert_eq!(script(line), None, "{line}");
    }
}

/// #1773: sudo option scanning stays linear when it crosses repeated chroot,
/// abbreviated-long and unknown-letter forms.
#[test]
fn issue_1773_sudo_value_option_checks_are_linear() {
    for (label, unit) in [
        ("sudo chroot options", "-R x "),
        ("sudo abbreviated long options", "--us root "),
        ("sudo unknown-letter clusters", "-xu root "),
    ] {
        work::assert_linear(label, |scale| {
            let line = format!("sudo {}./x", unit.repeat(400 * scale));
            let _ = script(&line);
        });
    }
}

/// #1482: a wrapper's option that takes a value consumes it inside a short
/// cluster and after a long option given as a separate word, so the value is
/// not read as the command and the command after it is judged. The
/// unambiguous --chr prefix now names --chroot, so its following -C is the
/// command rather than an option.
#[cfg(unix)]
#[test]
fn issue_1482_wrapper_option_values_are_not_the_command() {
    let evil = "echo x >> ~/.zshenv\n";
    let words = |line: &str| line.split(' ').map(str::to_string).collect::<Vec<_>>();
    for (line, at, name) in [
        ("sudo -nC 3 sh x", 3, "sh"),
        ("sudo -nC3 sh x", 2, "sh"),
        ("sudo -EnC 3 bash x", 3, "bash"),
        ("sudo -u root -nC 3 sh x", 5, "sh"),
        ("sudo -nD sub ./run.sh", 3, "run.sh"),
        ("sudo -nDsub ./run.sh", 2, "run.sh"),
        ("sudo --chdir sub ./run.sh", 3, "run.sh"),
        ("sudo --chdir=sub ./run.sh", 2, "run.sh"),
        ("sudo --close-from 3 sh x", 3, "sh"),
        ("sudo --user root sh x", 3, "sh"),
        ("sudo --chr -C 3 ./x", 3, "3"),
        ("sudo -C=3 sh x", 2, "sh"),
        ("sudo --chdir bash x", 3, "x"),
        ("env -iu X sh x", 3, "sh"),
        ("env -iC sub ./run.sh", 3, "run.sh"),
        ("flock -nw 5 lock sh x", 4, "sh"),
        ("timeout -s9 5 sh x", 3, "sh"),
        ("taskset -c 0 sh x", 3, "sh"),
        ("taskset -ac 0 sh x", 3, "sh"),
        ("taskset --cpu-list 0 sh x", 3, "sh"),
    ] {
        assert_eq!(
            indirect::command_word(&words(line)),
            Some((at, name.to_string())),
            "{line}"
        );
    }
    let mut wrong = Vec::new();
    let there = [("run.sh", "true\n"), ("sub/run.sh", evil)];
    for (files, line) in [
        (&[("x", evil)][..], "sudo -nC 3 sh x"),
        (&[("x", evil)][..], "sudo -nC3 sh x"),
        (&[("x", evil)][..], "sudo -EnC 3 bash x"),
        (&[("x", evil)][..], "sudo -u root -nC 3 sh x"),
        (&[("x", evil)][..], "sudo --close-from 3 sh x"),
        (&there[..], "sudo -nD sub ./run.sh"),
        (&there[..], "sudo -nDsub ./run.sh"),
        (&there[..], "sudo --chdir sub ./run.sh"),
        (&there[..], "env -iC sub ./run.sh"),
    ] {
        if in_workspace(files, line).is_none() {
            wrong.push(line);
        }
    }
    assert_eq!(wrong, Vec::<&str>::new(), "should ask");
    for line in [
        "sudo -nC 3 sh x",
        "sudo -nC3 sh x",
        "sudo -u root -nC 3 sh x",
        "sudo --close-from 3 sh x",
    ] {
        if in_workspace(&[("x", "true\n")], line).is_some() {
            wrong.push(line);
        }
    }
    assert_eq!(wrong, Vec::<&str>::new(), "should be quiet");
}
