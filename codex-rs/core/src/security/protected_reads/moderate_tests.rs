// Real folders, `/bin/bash` and the Unix sandboxes' protected paths.
#![cfg(unix)]

use super::*;
use crate::security::launch_contract::KeptFile;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::NetworkSandboxPolicy;
use pretty_assertions::assert_eq;

/// A Corbanu home holding `auth.json`, a project beside it, and the inputs
/// a Moderate session captures for them.
struct Fixture {
    _dirs: (tempfile::TempDir, tempfile::TempDir),
    home: PathBuf,
    project: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        let (home_path, project_path) = (
            home.path().canonicalize().expect("home path"),
            project.path().canonicalize().expect("project path"),
        );
        std::fs::write(home_path.join("auth.json"), "{}").expect("auth.json");
        // Before any explanation: on Linux, building the rules for a profile
        // that can write the home creates its missing runtime folders.
        std::fs::create_dir(home_path.join("skills")).expect("skills");
        std::fs::create_dir(project_path.join(".codex")).expect(".codex");
        Self {
            _dirs: (home, project),
            home: home_path,
            project: project_path,
        }
    }

    /// The workspace-write profile without the temporary folders, so a
    /// write beside the project is the person's own sandbox's denial.
    fn profile() -> PermissionProfile {
        PermissionProfile::workspace_write_with(
            &[],
            NetworkSandboxPolicy::Restricted,
            /*exclude_tmpdir_env_var*/ true,
            /*exclude_slash_tmp*/ true,
        )
    }

    fn inputs(&self, profile: PermissionProfile) -> ReadPolicyInputs {
        ReadPolicyInputs {
            codex_home: self.home.clone(),
            sqlite_home: self.home.clone(),
            turn_cwd: AbsolutePathBuf::from_absolute_path(&self.project).expect("cwd"),
            workspace_roots: Vec::new(),
            profile,
            user_home: dirs::home_dir(),
            keychain_closed: false,
        }
    }

    fn contract(&self) -> LaunchContract {
        LaunchContract::capture(
            &AbsolutePathBuf::from_absolute_path(&self.home).expect("home"),
            std::iter::empty(),
            /*hardened*/ false,
        )
    }

    fn explain(
        &self,
        profile: PermissionProfile,
        contract: Option<&LaunchContract>,
        script: &str,
        line: &str,
    ) -> Option<String> {
        explain(
            &self.inputs(profile),
            contract,
            &[
                "/bin/bash".to_string(),
                "-c".to_string(),
                script.to_string(),
            ],
            &self.project,
            &[line.to_string()],
        )
    }
}

/// #748 (c6, c3): a read of Corbanu's sign-in (or vault, or config) that
/// Moderate's rules denied names Moderate and /security; when secretless
/// launch keeps it at every level too, that is named instead.
#[test]
fn issue_748_moderate_read_denial_names_the_level() {
    let fixture = Fixture::new();
    let auth = format!("{}/auth.json", fixture.home.display());
    let script = format!("cat '{auth}'");
    let line = format!("cat: {auth}: Operation not permitted");

    let explained = fixture.explain(Fixture::profile(), /*contract*/ None, &script, &line);
    assert_eq!(
        explained,
        Some(read_explanation(&auth, WayForward::Permissive))
    );
    let explained = explained.expect("explained");
    assert!(
        explained.starts_with("Security level Moderate blocked access to"),
        "{explained}"
    );
    assert!(explained.contains("Permissive in /security"), "{explained}");

    // Not Moderate's alone: a person's approval may have lifted its rules.
    let contract = fixture.contract();
    let explained = fixture.explain(Fixture::profile(), Some(&contract), &script, &line);
    assert_eq!(explained, Some(kept_read_explanation(&auth)));
    let explained = explained.expect("explained");
    assert!(
        explained.starts_with("Secretless agent launch blocked access to"),
        "{explained}"
    );
    assert!(explained.contains("Moderate included"), "{explained}");
    assert!(!explained.contains("Permissive"), "{explained}");

    // A failure that names no denied path is not the level's.
    assert_eq!(
        fixture.explain(
            Fixture::profile(),
            /*contract*/ None,
            &script,
            "cat: /etc/other.txt: Operation not permitted"
        ),
        None
    );
}

/// #748: with the person's own full access, a write to the project's
/// `.codex` that Moderate's rules denied names Moderate. Under secretless
/// launch no level runs the command outside a sandbox, so then switching to
/// Permissive is not offered. A write to Corbanu's home that secretless
/// launch keeps read-only is attributed to it.
#[test]
fn issue_748_moderate_write_denial_names_the_level() {
    let fixture = Fixture::new();
    let script = "printf x > .codex/p.toml";
    let line = "bash: .codex/p.toml: Operation not permitted";
    assert_eq!(
        fixture.explain(
            PermissionProfile::Disabled,
            /*contract*/ None,
            script,
            line
        ),
        Some(write_explanation(".codex/p.toml", WayForward::Permissive))
    );
    let contract = fixture.contract();
    let explained = fixture.explain(PermissionProfile::Disabled, Some(&contract), script, line);
    assert_eq!(
        explained,
        Some(write_explanation(".codex/p.toml", WayForward::NoLevel))
    );
    let explained = explained.expect("explained");
    assert!(explained.contains("only inside a sandbox"), "{explained}");
    assert!(!explained.contains("Permissive"), "{explained}");

    // A runtime entry of Corbanu's home: Moderate's, and secretless
    // launch's when it is on.
    let skill = format!("{}/skills/x.md", fixture.home.display());
    let skill_script = format!("printf x > '{skill}'");
    let skill_line = format!("bash: {skill}: Operation not permitted");
    assert_eq!(
        fixture.explain(
            PermissionProfile::Disabled,
            /*contract*/ None,
            &skill_script,
            &skill_line
        ),
        Some(write_explanation(&skill, WayForward::Permissive))
    );
    assert_eq!(
        fixture.explain(
            PermissionProfile::Disabled,
            Some(&contract),
            &skill_script,
            &skill_line
        ),
        Some(kept_write_explanation(&skill))
    );
    // Workspace-write keeps `.codex` read-only itself: not Moderate's.
    assert_eq!(
        fixture.explain(Fixture::profile(), /*contract*/ None, script, line),
        None
    );
}

/// #748: what Moderate's rules did not add is not attributed to the level:
/// a write outside the project (the person's own sandbox), and a read the
/// person's own profile already denies.
#[test]
fn issue_748_denials_moderate_did_not_add_are_not_attributed() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.explain(
            Fixture::profile(),
            /*contract*/ None,
            "printf x > ../outside-notes.txt",
            "bash: ../outside-notes.txt: Operation not permitted"
        ),
        None
    );

    let auth = fixture.home.join("auth.json");
    let (mut file_system, network) = Fixture::profile().to_runtime_permissions();
    file_system.entries.push(FileSystemSandboxEntry::new(
        FileSystemPath::Path {
            path: AbsolutePathBuf::from_absolute_path(&auth).expect("auth"),
        },
        FileSystemAccessMode::Deny,
    ));
    let own_denial = PermissionProfile::from_runtime_permissions(&file_system, network);
    let auth = auth.display().to_string();
    assert_eq!(
        fixture.explain(
            own_denial,
            /*contract*/ None,
            &format!("cat '{auth}'"),
            &format!("cat: {auth}: Operation not permitted")
        ),
        None
    );
}

/// #841 (MOD-30): a command naming Corbanu's own files that the secretless
/// launch contract keeps unreadable at every level is recognised, so the
/// prompt can say approving cannot unlock them; a project file is not.
#[test]
fn issue_841_reads_kept_by_the_launch_contract_are_recognised() {
    let fixture = Fixture::new();
    let contract = fixture.contract();
    let inputs = fixture.inputs(Fixture::profile());
    let named = |script: String| {
        super::super::kept_read_note(
            &inputs,
            &contract,
            &["/bin/bash".to_string(), "-c".to_string(), script],
            &fixture.project,
        )
        .is_some()
    };
    assert!(named(format!(
        "cat '{}/config.toml'",
        fixture.home.display()
    )));
    assert!(named(format!("cat '{}/auth.json'", fixture.home.display())));
    assert!(!named("cat README.md".to_string()));
}

/// #1077 (MOD-09 item 7): the prompt names the file approving cannot unlock
/// and what it is: a CLI token such as `~/.aws/credentials` is not called
/// one of Corbanu's own files. A file approval does unlock (`~/.ssh/config`)
/// gets no note.
#[test]
fn issue_1077_the_kept_read_note_names_the_file_and_what_it_is() {
    let fixture = Fixture::new();
    let contract = fixture.contract();
    let inputs = fixture.inputs(Fixture::profile());
    let note = |script: &str| {
        super::super::kept_read_note(
            &inputs,
            &contract,
            &[
                "/bin/bash".to_string(),
                "-c".to_string(),
                script.to_string(),
            ],
            &fixture.project,
        )
    };
    let notes = [
        "cat ~/.aws/credentials",
        "cat ~/.ssh/config ~/.netrc",
        "cat \"$CODEX_HOME/config.toml\"",
        "cat ~/.ssh/config",
    ]
    .map(|script| {
        format!(
            "{script}\n{}",
            note(script).unwrap_or_else(|| "(no note)".to_string())
        )
    });
    insta::assert_snapshot!(notes.join("\n\n"));
    if cfg!(target_os = "macos") {
        assert_eq!(
            note("ls ~/Library/Keychains"),
            Some(
                "`~/Library/Keychains` is part of your keychains: secretless agent launch keeps \
                 it unreadable to agent commands at every security level, even if you approve, \
                 so approving cannot unlock it. If you approve, the command runs, but that \
                 read fails."
                    .to_string()
            )
        );
    }
}

/// The Google Cloud CLI's folder on Windows (`%APPDATA%\gcloud`) holds its
/// credentials and config, so the note calls it that folder, not a CLI
/// token file; a file inside it is "in" the folder.
#[test]
fn the_kept_read_note_names_the_google_cloud_cli_folder() {
    let notes = [
        (r"%APPDATA%\gcloud", KeptFile::GoogleCloudCliFolder),
        (
            r"%APPDATA%\gcloud\credentials.db",
            KeptFile::InGoogleCloudCliFolder,
        ),
        ("~/.netrc", KeptFile::CliToken),
    ]
    .map(|(path, kept)| super::super::moderate_kept_read_note(path, kept));
    insta::assert_snapshot!(notes.join("\n\n"));
}

/// #839: what an explained denial was, for the inspector's recent denials:
/// Moderate's own, or kept by the secretless launch contract.
#[test]
fn issue_839_explained_denials_say_what_was_denied() {
    let fixture = Fixture::new();
    let auth = format!("{}/auth.json", fixture.home.display());
    let command = [
        "/bin/bash".to_string(),
        "-c".to_string(),
        format!("cat '{auth}'"),
    ];
    let lines = [format!("cat: {auth}: Operation not permitted")];
    let denied = |contract: Option<&LaunchContract>| {
        explain_denied(
            &fixture.inputs(Fixture::profile()),
            contract,
            &command,
            &fixture.project,
            &lines,
            /*failed*/ true,
        )
        .map(|(denied, _)| denied)
    };
    assert_eq!(denied(None), Some(Denied::Read));
    let contract = fixture.contract();
    assert_eq!(denied(Some(&contract)), Some(Denied::KeptRead));
    assert_eq!(
        Denied::Read.inspected(),
        (
            "reading a protected file",
            "denied by the sandbox under Moderate"
        )
    );
}

/// #1312: a command that exited 0 is explained only by a protected path it
/// names itself (a later command hid the failure); a denial line in its
/// output alone (a search printing old denial lines) explains nothing.
#[test]
fn issue_1312_exit_zero_needs_a_path_the_command_names() {
    let fixture = Fixture::new();
    let auth = format!("{}/auth.json", fixture.home.display());
    let lines = [format!("notes.md:3:cat: {auth}: Operation not permitted")];
    let denied = |script: String, failed: bool| {
        explain_denied(
            &fixture.inputs(Fixture::profile()),
            /*contract*/ None,
            &["/bin/bash".to_string(), "-c".to_string(), script],
            &fixture.project,
            &lines,
            failed,
        )
        .map(|(denied, _)| denied)
    };
    let named = format!("cat '{auth}'; echo \"exit=$?\"");
    assert_eq!(denied(named.clone(), /*failed*/ true), Some(Denied::Read));
    assert_eq!(denied(named, /*failed*/ false), Some(Denied::Read));
    // The output alone: explained only for a failed command (#621).
    let search = "rg -n 'not permitted' notes.md".to_string();
    assert_eq!(denied(search.clone(), /*failed*/ true), Some(Denied::Read));
    assert_eq!(denied(search, /*failed*/ false), None);
}

/// #1087 (MOD-09 item 5): an in-process file tool's read (the image tool's)
/// that Moderate's rules refused names Moderate; one the secretless launch
/// contract keeps (already in the tool's own permissions) is attributed to
/// the contract, as for a command; what neither refused, such as a project
/// file the contract's fail-closed profile denies, is not explained.
#[test]
fn issue_1087_file_tool_denials_name_the_level() {
    use codex_security_policy::SecurityLevel;
    let fixture = Fixture::new();
    let inputs = fixture.inputs(Fixture::profile());
    let own = Fixture::profile().file_system_sandbox_policy();
    let protected = inputs
        .denials()
        .apply(&inputs.profile)
        .expect("the rules apply")
        .file_system_sandbox_policy();
    let denied_all = FileSystemSandboxPolicy::restricted(Vec::new());
    let contract = fixture.contract();
    let auth = fixture.home.join("auth.json");
    let shown = auth.display().to_string();
    let image = fixture.project.join("a.png");
    let explain =
        |level, contract: Option<&LaunchContract>, own: &FileSystemSandboxPolicy, path: &Path| {
            super::super::explain_file_tool_denial(
                level,
                &inputs,
                contract,
                own,
                &protected,
                &fixture.project,
                path,
            )
        };

    assert_eq!(
        explain(SecurityLevel::Moderate, None, &own, &auth),
        Some((
            "reading a protected file",
            Some("denied by the sandbox under Moderate"),
            read_explanation(&shown, WayForward::Permissive),
        ))
    );
    assert_eq!(explain(SecurityLevel::Moderate, None, &own, &image), None);
    assert_eq!(
        explain(SecurityLevel::Moderate, Some(&contract), &denied_all, &auth),
        Some((
            "reading Corbanu's own files or CLI tokens",
            Some(
                "denied by the sandbox; secretless agent launch keeps it at every level, even when you approve"
            ),
            kept_read_explanation(&shown),
        ))
    );
    assert_eq!(
        explain(
            SecurityLevel::Moderate,
            Some(&contract),
            &denied_all,
            &image
        ),
        None
    );
    assert_eq!(
        explain(SecurityLevel::Aggressive, None, &own, &auth),
        Some((
            "reading a protected file",
            None,
            super::super::explanation(&shown),
        ))
    );
}

/// #1139 (D94, MOD-11): under Moderate, a write into the project's `.git`
/// (a hook, its config) that the sandbox stops names Moderate and the way
/// forward, both where the command names the path and where only git's
/// denial line reports it; under secretless launch, no level change is
/// offered. Nothing outside a writable folder's `.git` is attributed.
#[test]
fn issue_1139_moderate_git_folder_write_names_the_level() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.project.join(".git/hooks")).expect(".git/hooks");
    std::fs::write(fixture.project.join(".git/config"), "").expect(".git/config");

    let hook = "printf 'echo hi\\n' > .git/hooks/pre-commit";
    let hook_line = "bash: line 1: .git/hooks/pre-commit: Operation not permitted";
    let explained = fixture.explain(Fixture::profile(), /*contract*/ None, hook, hook_line);
    assert_eq!(
        explained,
        Some(git_write_explanation(".git/hooks/pre-commit"))
    );
    insta::assert_snapshot!(
        "moderate_git_folder_write_explanation",
        explained.expect("explained")
    );

    let config = "git config user.name injected";
    let config_line = "error: could not lock config file .git/config: Operation not permitted";
    assert_eq!(
        fixture.explain(
            Fixture::profile(),
            /*contract*/ None,
            config,
            config_line
        ),
        Some(git_write_explanation(".git/config"))
    );

    let contract = fixture.contract();
    let explained = fixture.explain(Fixture::profile(), Some(&contract), config, config_line);
    assert_eq!(explained, Some(kept_git_write_explanation(".git/config")));
    insta::assert_snapshot!(
        "moderate_git_folder_write_kept_explanation",
        explained.expect("explained")
    );

    // Not a `.git` the sandbox keeps inside a writable folder: one in a
    // folder the person's own sandbox cannot write. With the person's own
    // full access, Moderate's own rules keep git config read-only and say so
    // as before (#748).
    let other = format!("{}/repo/.git/config", fixture.home.display());
    assert_eq!(
        fixture.explain(
            Fixture::profile(),
            /*contract*/ None,
            &format!("printf x > '{other}'"),
            &format!("bash: {other}: Operation not permitted")
        ),
        None
    );
    assert_eq!(
        fixture.explain(
            PermissionProfile::Disabled,
            /*contract*/ None,
            config,
            config_line
        ),
        Some(write_explanation(".git/config", WayForward::Permissive))
    );
    // #1139 review: a command that changed folder may have written another
    // repository's `.git`; a relative path it reports is not attributed.
    let outside = format!("{}/repo", fixture.home.display());
    for (script, line) in [
        (
            format!("cd '{outside}' && git config user.name x"),
            config_line,
        ),
        (
            format!("git -C '{outside}' config user.name x"),
            config_line,
        ),
        (
            format!("GIT_DIR='{outside}/.git' git config user.name x"),
            config_line,
        ),
        (
            format!("cd '{outside}' && printf x > .git/hooks/pre-commit"),
            hook_line,
        ),
    ] {
        assert_eq!(
            fixture.explain(Fixture::profile(), /*contract*/ None, &script, line),
            None,
            "{script}"
        );
    }
    // An absolute path in the project still is.
    let project_config = format!("{}/.git/config", fixture.project.display());
    assert_eq!(
        fixture.explain(
            Fixture::profile(),
            /*contract*/ None,
            &format!(
                "cd /tmp && git -C '{}' config user.name x",
                fixture.project.display()
            ),
            &format!("error: could not lock config file {project_config}: Operation not permitted")
        ),
        Some(git_write_explanation(&project_config))
    );

    // A denied write that involves no `.git` is not attributed either.
    assert_eq!(
        fixture.explain(
            Fixture::profile(),
            /*contract*/ None,
            "printf x > notes/a.txt",
            "bash: notes/a.txt: Operation not permitted"
        ),
        None
    );
}

/// #1323: under Moderate, a search that exited 0 is not explained by its
/// pattern when its output prints an old denial line naming it; a path the
/// search reads, and a failed search, are explained as before.
#[test]
fn issue_1323_a_search_pattern_is_not_a_denied_path() {
    let fixture = Fixture::new();
    let auth = format!("{}/auth.json", fixture.home.display());
    let denied = |script: String, line: String, failed: bool| {
        explain_denied(
            &fixture.inputs(Fixture::profile()),
            /*contract*/ None,
            &["/bin/bash".to_string(), "-c".to_string(), script],
            &fixture.project,
            &[line],
            failed,
        )
        .map(|(denied, _)| denied)
    };
    let old_line = format!("notes.md:3:cat: {auth}: Operation not permitted");
    let search = format!("rg -F '{auth}' notes.md");
    assert_eq!(denied(search.clone(), old_line.clone(), false), None);
    assert_eq!(denied(search, old_line, true), Some(Denied::Read));
    let read = format!("grep -c token '{auth}'; echo \"exit=$?\"");
    let line = format!("grep: {auth}: Operation not permitted");
    assert_eq!(denied(read, line, false), Some(Denied::Read));
}
