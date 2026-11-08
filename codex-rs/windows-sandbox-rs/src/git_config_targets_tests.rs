use super::*;
use pretty_assertions::assert_eq;
use std::fs;

fn tempdir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dunce::canonicalize(dir.path()).expect("canonical");
    (dir, path)
}

#[test]
fn config_entries_parse_like_git() {
    let text = concat!(
        "# comment\n",
        "[core]\n",
        "\thooksPath = .husky/_ ; trailing comment\n",
        "\tbare\n",
        "[remote \"Origin\"]\n",
        "\turl = \"C:\\\\a b\" # c\n",
        "[alias]\n",
        "\tx = !\"./run me.sh\" \\\n",
        "  --flag\n",
        "[includeIf \"gitdir:~/w/\"]\n",
        "\tpath = extra.cfg\n",
        "[Diff.pdf]\n",
        "\ttextconv = ./tools/conv\n",
    );
    assert_eq!(
        parse(text)
            .into_iter()
            .map(|entry| (entry.name, entry.value))
            .collect::<Vec<_>>(),
        vec![
            ("core.hookspath".to_string(), ".husky/_".to_string()),
            ("core.bare".to_string(), "true".to_string()),
            ("remote.Origin.url".to_string(), "C:\\a b".to_string()),
            // Git keeps the blanks around a continuation, as here.
            ("alias.x".to_string(), "!./run me.sh   --flag".to_string()),
            (
                "includeif.gitdir:~/w/.path".to_string(),
                "extra.cfg".to_string()
            ),
            ("diff.pdf.textconv".to_string(), "./tools/conv".to_string()),
        ]
    );
}

/// D422: `core.hooksPath` (with Husky's `.husky` hooks), includes and
/// program keys naming files in the writable roots are targets; a bare
/// program name, a path outside the roots and one inside a `.git` aren't.
#[test]
fn targets_are_what_configs_name_inside_the_roots() {
    let (_dir, base) = tempdir();
    let work = base.join("proj");
    let git = work.join(".git");
    fs::create_dir_all(&git).expect("git folder");
    fs::write(
        git.join("config"),
        concat!(
            "[core]\n\thooksPath = .husky/_\n\tpager = less\n\tfsmonitor = ./fsmon.exe\n",
            "[include]\n\tpath = ../team.gitconfig\n",
            "[alias]\n\tst = !\"tools/st.sh\" -s\n\tlg = log\n",
            "[filter \"lfs\"]\n\tsmudge = git-lfs smudge\n",
            "[gpg]\n\tprogram = /elsewhere/gpg.exe\n[gpg \"x\"]\n\tprogram = ../../outside/gpg.exe\n",
            "[diff \"x\"]\n\ttextconv = .git/hooks/conv\n",
        ),
    )
    .expect("config");
    fs::write(
        work.join("team.gitconfig"),
        "[sequence]\n\teditor = ./seq-editor\n",
    )
    .expect("include");
    let found = targets(&git, Some(&work), &[], None, std::slice::from_ref(&base));
    let shown: Vec<(PathBuf, String, TargetKind)> = found
        .into_iter()
        .map(|target| {
            (
                target
                    .path
                    .strip_prefix(&work)
                    .expect("in work")
                    .to_path_buf(),
                target.why,
                target.kind,
            )
        })
        .collect();
    assert_eq!(
        shown,
        vec![
            (
                PathBuf::from(".husky"),
                "core.hooksPath".to_string(),
                TargetKind::HuskyHooks
            ),
            (
                Path::new(".husky").join("_"),
                "core.hooksPath".to_string(),
                TargetKind::Hooks
            ),
            (
                PathBuf::from("fsmon.exe"),
                "core.fsmonitor".to_string(),
                TargetKind::File
            ),
            (
                PathBuf::from("team.gitconfig"),
                "include.path".to_string(),
                TargetKind::File
            ),
            (
                PathBuf::from("seq-editor"),
                "sequence.editor".to_string(),
                TargetKind::File
            ),
            (
                PathBuf::from("tools/st.sh"),
                "alias.st".to_string(),
                TargetKind::File
            ),
        ]
    );
}

/// D422: what changed in a target since the session started; Husky's
/// `.husky` counts only files named after git hooks, and a rewrite with the
/// same content isn't a change.
#[test]
fn target_changes_compare_contents() {
    let (_dir, base) = tempdir();
    let work = base.join("proj");
    let git = work.join(".git");
    fs::create_dir_all(&git).expect("git folder");
    fs::create_dir_all(work.join(".husky").join("_")).expect("husky");
    fs::write(git.join("config"), "[core]\n\thooksPath = .husky/_\n").expect("config");
    fs::write(work.join(".husky/_/pre-commit"), "stub").expect("stub");
    let repos = [(git.clone(), Some(work.clone()))];
    let roots = [base.clone()];
    let before = target_states(&repos, &roots);
    fs::write(work.join(".husky/_/pre-commit"), "stub").expect("same stub");
    fs::write(work.join(".husky/README.md"), "notes").expect("not a hook");
    let unchanged = target_changes(&before, target_states(&repos, &roots));
    fs::write(work.join(".husky/post-checkout"), "evil").expect("hook");
    fs::write(work.join(".husky/_/pre-commit"), "evil").expect("stub changed");
    let changed = target_changes(&before, target_states(&repos, &roots));
    assert_eq!(
        (unchanged, changed),
        (
            Vec::new(),
            vec![
                (
                    work.join(".husky"),
                    vec!["core.hooksPath: post-checkout added".to_string()]
                ),
                (
                    work.join(".husky").join("_"),
                    vec!["core.hooksPath: pre-commit changed".to_string()]
                ),
            ]
        )
    );
}
