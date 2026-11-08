use super::*;
use pretty_assertions::assert_eq;
use std::fs;

fn tempdir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dunce::canonicalize(dir.path()).expect("canonical");
    (dir, path)
}

/// `(path below base, why, kind)` of each target.
fn shown(found: Vec<Target>, base: &Path) -> Vec<(PathBuf, String, TargetKind)> {
    found
        .into_iter()
        .map(|target| {
            (
                target
                    .path
                    .strip_prefix(base)
                    .expect("below base")
                    .to_path_buf(),
                target.why,
                target.kind,
            )
        })
        .collect()
}

#[test]
fn config_entries_parse_like_git() {
    let text = concat!(
        "\u{FEFF}[core]\r\n",
        "\thooksPath = .husky/_ ; trailing comment\r\n",
        "\tbare\n",
        "# comment\n",
        "[remote \"Origin\"]\n",
        "\turl = \"C:\\\\a b\" # c\n",
        "[alias]\n",
        "\tx = !\"./run me.sh\" \\\n",
        "\t--flag\n",
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
            // Git keeps the blanks around a continuation, as spaces.
            ("alias.x".to_string(), "!./run me.sh  --flag".to_string()),
            (
                "includeif.gitdir:~/w/.path".to_string(),
                "extra.cfg".to_string()
            ),
            ("diff.pdf.textconv".to_string(), "./tools/conv".to_string()),
        ]
    );
}

/// D422: `core.hooksPath` (with Husky's folders), includes and every path in
/// a program value are targets, also inside a nested repository's `.git`;
/// a bare program name, a URL, a path outside the roots and one inside a
/// root's own `.git` aren't.
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
            "[alias]\n\tst = !\"tools/st.sh\" -s\n\tlg = log\n\tf = \"!f() { sh ./x.sh \\\"$@\\\"; }; f\"\n",
            "[filter \"lfs\"]\n\tsmudge = git-lfs smudge\n",
            "[gpg]\n\tprogram = /elsewhere/gpg.exe\n",
            "[gpg \"x\"]\n\tprogram = ../../outside/gpg.exe\n",
            "[credential]\n\thelper = node scripts/cred.js https://example.com/x\n",
            "[diff \"x\"]\n\ttextconv = .git/conv\n",
        ),
    )
    .expect("config");
    fs::write(
        work.join("team.gitconfig"),
        "[sequence]\n\teditor = ./seq-editor\n",
    )
    .expect("include");
    let found = targets(&git, Some(&work), &[], &[], std::slice::from_ref(&base));
    let file = |path: &str, why: &str| (PathBuf::from(path), why.to_string(), TargetKind::File);
    assert_eq!(
        shown(found, &work),
        vec![
            (
                PathBuf::from(".husky"),
                "core.hooksPath".to_string(),
                TargetKind::HuskyHooks
            ),
            (
                Path::new(".husky").join("_").join("_"),
                "core.hooksPath".to_string(),
                TargetKind::Hooks
            ),
            (
                Path::new(".husky").join("_"),
                "core.hooksPath".to_string(),
                TargetKind::Hooks
            ),
            file("fsmon.exe", "core.fsmonitor"),
            file("team.gitconfig", "include.path"),
            file("seq-editor", "sequence.editor"),
            file("tools/st.sh", "alias.st"),
            file("x.sh", "alias.f"),
            file("scripts/cred.js", "credential.helper"),
            file(".git/conv", "diff.x.textconv"),
        ]
    );
}

/// D422: what changed in a target since the session started; Husky's
/// folder counts only files named after git hooks, a rewrite with the same
/// content isn't a change, and a target that appears is reported with what
/// is in it.
#[test]
fn target_changes_compare_contents() {
    let (_dir, base) = tempdir();
    let work = base.join("proj");
    let git = work.join(".git");
    fs::create_dir_all(&git).expect("git folder");
    fs::create_dir_all(work.join(".husky").join("_")).expect("husky");
    fs::write(
        git.join("config"),
        "[core]\n\thooksPath = .husky/_\n[alias]\n\tx = !./x.sh\n",
    )
    .expect("config");
    fs::write(work.join(".husky/_/pre-commit"), "stub").expect("stub");
    let repos = [(git, Some(work.clone()))];
    let roots = [base];
    let states = || target_states_with(&repos, &roots, &[], &[]);
    let before = states();
    fs::write(work.join(".husky/_/pre-commit"), "stub").expect("same stub");
    fs::write(work.join(".husky/README.md"), "notes").expect("not a hook");
    let unchanged = target_changes(&before, states());
    fs::write(work.join(".husky/post-checkout"), "evil").expect("hook");
    fs::write(work.join(".husky/_/pre-commit"), "evil").expect("stub changed");
    fs::write(work.join("x.sh"), "evil").expect("script appears");
    let changed = target_changes(&before, states());
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
                (work.join("x.sh"), vec!["alias.x: added".to_string()]),
            ]
        )
    );
}

/// D422 review: a global config (and what it includes, `~/` expanded) that
/// lies in a writable root is a target; its relative `core.hooksPath`
/// applies to every repository, so a hook in a new repository's folder
/// counts; Husky v8's `_` folder and a custom Husky v9 folder count; an
/// oversized hooks folder counts every time.
#[test]
fn global_configs_new_repositories_and_husky_layouts() {
    let (_dir, base) = tempdir();
    let home = base.join("home");
    fs::create_dir_all(&home).expect("home");
    fs::write(
        home.join(".gitconfig"),
        "[core]\n\thooksPath = .githooks\n[include]\n\tpath = ~/extra.cfg\n",
    )
    .expect("global");
    fs::write(home.join("extra.cfg"), "[user]\n\tname = x\n").expect("included");
    let outside = [home.join(".gitconfig")];
    let homes = [home.clone()];
    let roots = [base.clone()];
    let repo = base.join("new");
    fs::create_dir_all(repo.join(".git")).expect("repo");
    let repos = [(repo.join(".git"), Some(repo.clone()))];
    let states = || target_states_with(&repos, &roots, &outside, &homes);
    let before = states();
    fs::create_dir_all(repo.join(".githooks")).expect("hooks");
    fs::write(repo.join(".githooks").join("pre-commit"), "evil").expect("hook");
    fs::write(home.join("extra.cfg"), "[core]\n\tfsmonitor = evil.exe\n").expect("edit");
    let mut changed = target_changes(&before, states());
    changed.sort();

    let v8 = base.join("v8");
    fs::create_dir_all(v8.join(".git")).expect("v8 repo");
    fs::create_dir_all(v8.join(".config").join("husky").join("_")).expect("husky");
    fs::write(
        v8.join(".git").join("config"),
        "[core]\n\thooksPath = .config/husky\n",
    )
    .expect("v8 config");
    let v8_targets = targets(&v8.join(".git"), Some(&v8), &[], &[], &roots);
    let big = base.join("big");
    fs::create_dir_all(big.join(".git")).expect("big repo");
    fs::create_dir_all(big.join("hooks")).expect("big hooks");
    fs::write(
        big.join(".git").join("config"),
        "[core]\n\thooksPath = hooks\n",
    )
    .expect("big");
    for n in 0..1100 {
        fs::write(big.join("hooks").join(format!("f{n}")), "x").expect("file");
    }
    let big_repos = [(big.join(".git"), Some(big.clone()))];
    let big_before = target_states_with(&big_repos, &roots, &[], &[]);
    let big_again = target_changes(
        &big_before,
        target_states_with(&big_repos, &roots, &[], &[]),
    );

    assert_eq!(
        (changed, shown(v8_targets, &v8), big_again.len(),),
        (
            vec![
                (
                    home.join("extra.cfg"),
                    vec!["git config: changed".to_string()]
                ),
                (
                    repo.join(".githooks"),
                    vec!["core.hooksPath: pre-commit added".to_string()]
                ),
            ],
            vec![
                (
                    Path::new(".config").join("husky").join("_"),
                    "core.hooksPath".to_string(),
                    TargetKind::Hooks
                ),
                (
                    Path::new(".config").join("husky"),
                    "core.hooksPath".to_string(),
                    TargetKind::Hooks
                ),
            ],
            1,
        )
    );
}

/// A `.git` file's repository is its `gitdir:` folder; a bare one has no
/// work tree.
#[test]
fn repo_dirs_follow_git_files() {
    let (_dir, base) = tempdir();
    let checkout = base.join("wt");
    fs::create_dir_all(&checkout).expect("checkout");
    fs::write(checkout.join(".git"), "gitdir: ../main/.git/worktrees/wt\n").expect(".git file");
    let bare = base.join("bare");
    fs::create_dir_all(&bare).expect("bare");
    assert_eq!(
        repo_dirs(&[checkout.join(".git"), bare.clone()]),
        vec![
            (
                checkout.join("../main/.git/worktrees/wt"),
                Some(checkout.clone())
            ),
            (bare, None),
        ]
    );
}
