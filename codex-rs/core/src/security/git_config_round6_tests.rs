use super::ProtectedActionKind;
use super::script;
use pretty_assertions::assert_eq;

#[test]
fn issue_1161_round6_split_value_read_markers_do_not_hide_writes() {
    for command in [
        "git config --comment 'x -z --get' core.pager sh",
        "git config -f ~/.zshenv --comment x,-z,--get 'x.$(touch pwned).y' z",
        "git config -f 'l -z user.name' -e",
        "git config --comment 'x --get' core.pager sh",
        "git config --comment x,--get core.pager sh",
        "git config --comment='x -l' filter.a.clean /tmp/x",
        "git config --comment 'x --list' diff.x.textconv /tmp/x",
    ] {
        assert_eq!(
            script(command),
            Some(ProtectedActionKind::Persistence),
            "{command}"
        );
    }
}

#[test]
fn issue_1161_round6_config_reads_stay_quiet() {
    for command in [
        "git config -z --comment note --get pager.log",
        "git config --type bool --get pull.rebase",
        "git config -f file --get core.pager",
    ] {
        assert_eq!(script(command), None, "{command}");
    }
}

#[test]
fn issue_1161_round6_bundled_list_flags_stop_at_value_options() {
    for command in ["git config -lfe", "git config -zlfe", "git config -ltbool"] {
        assert_eq!(script(command), None, "{command}");
    }
    for command in [
        "git config -fl core.pager sh",
        "git config -tl core.pager sh",
        "git config --comment 'x -zlfe' core.pager sh",
    ] {
        assert_eq!(
            script(command),
            Some(ProtectedActionKind::Persistence),
            "{command}"
        );
    }
}
