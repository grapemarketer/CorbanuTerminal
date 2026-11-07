use super::*;
use codex_security_policy::SecurityLevel;
use pretty_assertions::assert_eq;

fn shell(script: &str) -> Vec<String> {
    vec!["/bin/zsh".into(), "-lc".into(), script.into()]
}

/// #1296: a Windows PowerShell command, as Corbanu runs one.
fn powershell(script: &str) -> Vec<String> {
    vec![
        "powershell.exe".into(),
        "-NoProfile".into(),
        "-Command".into(),
        script.into(),
    ]
}

/// #1296: a cmd command, as Corbanu runs one.
fn cmd(script: &str) -> Vec<String> {
    vec!["cmd.exe".into(), "/c".into(), script.into()]
}

/// The word the command used for the first denied path it names, through
/// quoting, redirections, `--flag=` values and home variables.
#[test]
fn issue_442_finds_the_denied_path_the_command_names() {
    let cwd = Path::new("/work/project");
    let user_home = Path::new("/Users/someone");
    let codex_home = Path::new("/Users/someone/.corbanu");
    let denied = |path: &Path| {
        [
            Path::new("/work/project/.env"),
            Path::new("/Users/someone/.ssh"),
            Path::new("/Users/someone/.corbanu/auth.json"),
        ]
        .iter()
        .any(|denied| path.starts_with(denied))
    };
    let word = |script: &str| {
        denied_word(
            &shell(script),
            cwd,
            Some(user_home),
            codex_home,
            denied,
            /*denial_lines*/ None,
            /*skip_patterns*/ false,
        )
    };
    assert_eq!(word("cat .env"), Some(".env".to_string()));
    assert_eq!(word("head -3 '.env' | sort"), Some(".env".to_string()));
    assert_eq!(word("wc -l <.env;"), Some(".env".to_string()));
    assert_eq!(word("tool --file=.env"), Some(".env".to_string()));
    assert_eq!(word("cat ~/.ssh/config"), Some("~/.ssh/config".to_string()));
    assert_eq!(
        word("cat $HOME/.ssh/id_ed25519"),
        Some("$HOME/.ssh/id_ed25519".to_string())
    );
    assert_eq!(
        word("cat \"$CORBANU_HOME/auth.json\""),
        Some("$CORBANU_HOME/auth.json".to_string())
    );
    assert_eq!(word("cat notes.txt"), None);
    assert_eq!(word("cat $UNKNOWN/.env.example"), None);
    // Tied to the denial the output reports: an unrelated failure in a
    // command that also names `.env` is not attributed to it.
    let lines = ["grep: ./other.txt: Permission denied".to_string()];
    assert_eq!(
        denied_word(
            &shell("grep -r x . --exclude=.env"),
            cwd,
            Some(user_home),
            codex_home,
            denied,
            Some(&lines),
            /*skip_patterns*/ false,
        ),
        None
    );
    let lines = ["cat: .env: Operation not permitted".to_string()];
    assert_eq!(
        denied_word(
            &shell("cat .env"),
            cwd,
            Some(user_home),
            codex_home,
            denied,
            Some(&lines),
            /*skip_patterns*/ false,
        ),
        Some(".env".to_string())
    );
}

/// #506 review: a denial line names the folder as the shell resolved it, so
/// `ls "$CODEX_HOME"` and `ls home/tmp/..` are attributed to the home; the
/// listing explanation names the path as the command spelled it.
#[test]
fn issue_506_listing_denial_is_attributed_through_variables_and_dot_dot() {
    // #1296: on Windows, PowerShell and cmd spellings and denial texts.
    let (cwd, user_home, codex_home) = if cfg!(windows) {
        (r"C:\work\project", r"C:\users\me", r"C:\users\me\.corbanu")
    } else {
        ("/work/project", "/users/me", "/users/me/.corbanu")
    };
    let (cwd, user_home, codex_home) =
        (Path::new(cwd), Path::new(user_home), Path::new(codex_home));
    let denied = |path: &Path| {
        AbsolutePathBuf::from_absolute_path(path).is_ok_and(|path| path.as_path() == codex_home)
    };
    let (cases, other_command, other_line) = if cfg!(windows) {
        let line = r"Get-ChildItem : Access to the path 'C:\users\me\.corbanu' is denied.";
        (
            vec![
                (
                    powershell("Get-ChildItem -Force \"$env:CODEX_HOME\""),
                    line,
                    "$env:CODEX_HOME",
                ),
                (
                    powershell("Get-ChildItem ${env:CORBANU_HOME}"),
                    line,
                    "${env:CORBANU_HOME}",
                ),
                (
                    powershell("gci $env:corbanu_home"),
                    line,
                    "$env:corbanu_home",
                ),
                (
                    powershell(r"Get-ChildItem ~\.corbanu\tmp\.."),
                    line,
                    r"~\.corbanu\tmp\..",
                ),
                (
                    powershell(r"Get-ChildItem $HOME\.corbanu"),
                    line,
                    r"$HOME\.corbanu",
                ),
                (
                    powershell(r"Get-ChildItem C:\users\me\.corbanu"),
                    line,
                    r"C:\users\me\.corbanu",
                ),
                // PowerShell 7 writes no space before the colon.
                (
                    powershell(r"Get-ChildItem $env:USERPROFILE\.corbanu"),
                    r"Get-ChildItem: Access to the path 'C:\users\me\.corbanu' is denied.",
                    r"$env:USERPROFILE\.corbanu",
                ),
                // cmd names no path: the path the command names.
                (cmd("dir %CODEX_HOME%"), "Access is denied.", "%CODEX_HOME%"),
                (
                    cmd(r"dir %UserProfile%\.corbanu"),
                    "Access is denied.",
                    r"%UserProfile%\.corbanu",
                ),
            ],
            powershell("Get-ChildItem $env:CODEX_HOME other"),
            r"Get-ChildItem : Access to the path 'C:\users\me\other' is denied.",
        )
    } else {
        let line = "ls: /users/me/.corbanu: Operation not permitted";
        (
            vec![
                (shell("ls -la \"$CODEX_HOME\""), line, "$CODEX_HOME"),
                (shell("ls ${CORBANU_HOME}"), line, "${CORBANU_HOME}"),
                (shell("ls ~/.corbanu/tmp/.."), line, "~/.corbanu/tmp/.."),
            ],
            shell("ls \"$CODEX_HOME\" other"),
            "ls: /users/me/other: Operation not permitted",
        )
    };
    for (command, line, expected) in cases {
        assert_eq!(
            denied_word(
                &command,
                cwd,
                Some(user_home),
                codex_home,
                denied,
                Some(&[line.to_string()]),
                /*skip_patterns*/ false,
            ),
            Some(expected.to_string()),
            "{command:?}"
        );
    }
    assert_eq!(
        denied_word(
            &other_command,
            cwd,
            Some(user_home),
            codex_home,
            denied,
            Some(&[other_line.to_string()]),
            /*skip_patterns*/ false,
        ),
        None
    );
    let text = listing_explanation("$CODEX_HOME");
    assert!(
        text.starts_with("Security level Aggressive blocked listing `$CODEX_HOME`"),
        "{text}"
    );
    assert!(text.contains("not even once"), "{text}");
}

/// #554: `find ~` or `ls -aR ~` reports the home's listing denial below the
/// folder the command names; the line is attributed to the home as it
/// spells it, in BSD and GNU forms and relative to the folder a `cd` names.
#[test]
fn issue_554_recursive_listing_of_a_parent_is_attributed_to_the_home() {
    // #1296: on Windows, PowerShell and cmd spellings and denial texts.
    let (cwd, user_home, codex_home) = if cfg!(windows) {
        (r"C:\work\project", r"C:\users\me", r"C:\users\me\.corbanu")
    } else {
        ("/work/project", "/users/me", "/users/me/.corbanu")
    };
    let (cwd, user_home, codex_home) =
        (Path::new(cwd), Path::new(user_home), Path::new(codex_home));
    let unlistable = |path: &Path| {
        AbsolutePathBuf::from_absolute_path(path).is_ok_and(|path| path.as_path() == codex_home)
    };
    let found = |command: &[String], line: &str| {
        unlistable_path_in_lines(
            command,
            cwd,
            Some(user_home),
            codex_home,
            &[line.to_string()],
            &unlistable,
        )
    };
    if cfg!(windows) {
        let home = r"C:\users\me\.corbanu";
        let denied = |program: &str, path: &str| {
            format!("{program} : Access to the path '{path}' is denied.")
        };
        for (command, line) in [
            (
                powershell("Get-ChildItem -Recurse ~ -Filter *.toml"),
                denied("Get-ChildItem", home),
            ),
            (
                powershell("Get-ChildItem -Recurse -Force $env:USERPROFILE"),
                denied("Get-ChildItem", home),
            ),
            (
                powershell(r"gci -r C:\users\me"),
                denied("Get-ChildItem", home),
            ),
            // PowerShell 7 writes no space before the colon.
            (
                powershell("Get-ChildItem -Recurse $HOME"),
                format!("Get-ChildItem: Access to the path '{home}' is denied."),
            ),
        ] {
            assert_eq!(
                found(&command, &line),
                Some(home.to_string()),
                "{command:?}"
            );
        }
        for (command, line) in [
            // Another folder, or a name that only starts like the home's.
            (
                powershell("Get-ChildItem -Recurse ~"),
                denied("Get-ChildItem", r"C:\users\me\Application Data"),
            ),
            (
                powershell("Get-ChildItem -Recurse ~"),
                denied("Get-ChildItem", r"C:\users\me\.corbanu-old"),
            ),
            // A path below the home, not the home itself.
            (
                powershell("Get-ChildItem -Recurse ~"),
                denied("Get-ChildItem", r"C:\users\me\.corbanu\vault"),
            ),
            // cmd's `dir /s` names no path, and a profile holds folders it
            // is always denied (`Application Data`): nothing to attribute.
            (cmd(r"dir /s C:\users\me"), "Access is denied.".to_string()),
        ] {
            assert_eq!(found(&command, &line), None, "{command:?}: {line}");
        }
        return;
    }
    for (script, line, expected) in [
        (
            "find ~ -name '*.toml'",
            "find: /users/me/.corbanu: Operation not permitted",
            "/users/me/.corbanu",
        ),
        (
            "ls -aR \"$HOME\"",
            "ls: /users/me/.corbanu: Operation not permitted",
            "/users/me/.corbanu",
        ),
        (
            "find /users/me",
            "find: \u{2018}/users/me/.corbanu\u{2019}: Permission denied",
            "/users/me/.corbanu",
        ),
        (
            "ls -aR /users/me",
            "ls: cannot open directory '/users/me/.corbanu': Permission denied",
            "/users/me/.corbanu",
        ),
        (
            "cd ~ && find .",
            "find: ./.corbanu: Operation not permitted",
            "./.corbanu",
        ),
        (
            "cd ~; find .",
            "find: ./.corbanu: Operation not permitted",
            "./.corbanu",
        ),
        (
            "ls -aR ../../users/me",
            "ls: ../../users/me/.corbanu: Operation not permitted",
            "../../users/me/.corbanu",
        ),
        // #1194: the program's name was split off (written separately, with
        // the command's output merged in between).
        (
            "find ~",
            "/users/me/.corbanu: Operation not permitted",
            "/users/me/.corbanu",
        ),
        (
            "cd ~ && ls -aR .",
            "./.corbanu: Operation not permitted",
            "./.corbanu",
        ),
    ] {
        assert_eq!(
            found(&shell(script), line),
            Some(expected.to_string()),
            "{script}"
        );
    }
    for (script, line) in [
        // Another folder, or a name that only starts like the home's.
        (
            "find ~",
            "find: /users/me/Library/Mail: Operation not permitted",
        ),
        (
            "find ~",
            "find: /users/me/.corbanu-old: Operation not permitted",
        ),
        // A path below the home, not the home itself.
        (
            "find ~",
            "find: /users/me/.corbanu/vault: Operation not permitted",
        ),
        // Relative to a folder the command does not name.
        ("find .", "find: ./.corbanu: Operation not permitted"),
        // #1194: with the program's name split off, still only the home.
        (
            "find ~",
            "/users/me/.corbanu/vault: Operation not permitted",
        ),
        ("find ~", "/users/me/.corbanu-old: Operation not permitted"),
    ] {
        assert_eq!(found(&shell(script), line), None, "{script}: {line}");
    }
}

/// #442 (milestone AGG-09): under Aggressive a denied read of a protected
/// file is explained where it failed, naming the level and /security; it is
/// not explained under Permissive or for a failure that is not a denial.
#[tokio::test]
async fn issue_442_denied_protected_read_is_explained_under_aggressive() {
    let (session, turn) = crate::session::tests::make_session_and_context().await;
    #[allow(deprecated)]
    let cwd = turn.cwd.clone();
    let home = turn.config.codex_home.to_path_buf();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), "x").unwrap();
    let command = shell(&format!("cat '{}'", home.join("auth.json").display()));
    let denial = "cat: auth.json: Operation not permitted\n";
    let explain = async |exit_code, output: &str| {
        denial_explanation(
            &session,
            &turn,
            &command,
            cwd.as_path(),
            Some(exit_code),
            output,
        )
        .await
    };
    assert_eq!(explain(1, denial).await, None, "Permissive: unchanged");

    let client = (*session.services.model_client())
        .clone()
        .with_ingress_level(SecurityLevel::Aggressive)
        .with_source_envelopes(/*enabled*/ true);
    session.services.replace_model_client(client);
    let explanation = explain(1, denial)
        .await
        .expect("explained under Aggressive");
    assert!(
        explanation.starts_with("Security level Aggressive blocked access to"),
        "{explanation}"
    );
    assert!(
        explanation.contains("cannot be granted, not even once"),
        "{explanation}"
    );
    assert!(
        explanation.contains("switch to Permissive in /security"),
        "{explanation}"
    );
    // #1312: a denial is explained also when the command as a whole
    // exited 0 (a later command on the line hid the failure).
    assert_eq!(explain(0, denial).await, Some(explanation));
    assert_eq!(
        explain(1, "cat: auth.json: No such file or directory\n").await,
        None
    );
    assert_eq!(
        protected_path_in_command(&session, &turn, &command, cwd.as_path()).await,
        Some(home.join("auth.json").display().to_string())
    );
}

/// #554: under Aggressive, `find` of the folder above the Corbanu home is
/// explained like a direct listing of the home (#506); not under Permissive,
/// and not for a denial of another folder.
#[tokio::test]
async fn issue_554_find_above_the_home_is_explained_under_aggressive() {
    let (session, turn) = crate::session::tests::make_session_and_context().await;
    #[allow(deprecated)]
    let cwd = turn.cwd.clone();
    let home = turn.config.codex_home.to_path_buf();
    std::fs::create_dir_all(&home).unwrap();
    let parent = home.parent().expect("the home has a parent").to_path_buf();
    let command = shell(&format!("find '{}' -name '*.toml'", parent.display()));
    let denial = format!("find: {}: Operation not permitted\n", home.display());
    let explain = async |output: &str| {
        denial_explanation(&session, &turn, &command, cwd.as_path(), Some(1), output).await
    };
    assert_eq!(explain(&denial).await, None, "Permissive: unchanged");

    let client = (*session.services.model_client())
        .clone()
        .with_ingress_level(SecurityLevel::Aggressive)
        .with_source_envelopes(/*enabled*/ true);
    session.services.replace_model_client(client);
    let explanation = explain(&denial).await.expect("explained under Aggressive");
    assert_eq!(
        explanation,
        listing_explanation(&home.display().to_string())
    );
    let other = format!(
        "find: {}: Operation not permitted\n",
        parent.join("elsewhere").display()
    );
    assert_eq!(explain(&other).await, None);
}

/// #1194: BSD `find` and `ls` write `find: ` and the rest of the denial line
/// separately, and unified exec merges stdout and stderr as they arrive, so
/// the listing they flush at exit can land in between. The denial is still
/// explained as #554's.
#[tokio::test]
async fn issue_1194_a_split_denial_line_is_still_explained() {
    let (session, turn) = crate::session::tests::make_session_and_context().await;
    #[allow(deprecated)]
    let cwd = turn.cwd.clone();
    let home = turn.config.codex_home.to_path_buf();
    std::fs::create_dir_all(&home).unwrap();
    let parent = home.parent().expect("the home has a parent").to_path_buf();
    let client = (*session.services.model_client())
        .clone()
        .with_ingress_level(SecurityLevel::Aggressive)
        .with_source_envelopes(/*enabled*/ true);
    session.services.replace_model_client(client);
    let (parent, home) = (parent.display(), home.display());
    for (command, program, listing) in [
        (
            format!("find '{parent}'"),
            "find",
            format!("{parent}\n{home}\n"),
        ),
        (
            format!("ls -aR '{parent}'"),
            "ls",
            format!("{parent}:\n.\n..\n{home}\n\n"),
        ),
    ] {
        let command = shell(&command);
        let output = format!("{program}: {listing}{home}: Operation not permitted\n");
        let explanation =
            denial_explanation(&session, &turn, &command, cwd.as_path(), Some(1), &output).await;
        assert_eq!(
            explanation,
            Some(listing_explanation(&home.to_string())),
            "{output}"
        );
    }
}

/// #621 (milestone AGG-R07): a path inside code an interpreter runs is found
/// like a path argument, so no grant is offered for it; code that names no
/// protected path is left alone.
#[test]
fn issue_621_finds_the_denied_path_inside_interpreter_code() {
    // #1296: on Windows, PowerShell and cmd spellings.
    let (cwd, user_home, codex_home, env) = if cfg!(windows) {
        (
            r"C:\work\project",
            r"C:\Users\someone",
            r"C:\Users\someone\.corbanu",
            r"C:\work\project\.env",
        )
    } else {
        (
            "/work/project",
            "/Users/someone",
            "/Users/someone/.corbanu",
            "/work/project/.env",
        )
    };
    let (cwd, user_home, codex_home, env) = (
        Path::new(cwd),
        Path::new(user_home),
        Path::new(codex_home),
        Path::new(env),
    );
    let denied = |path: &Path| {
        AbsolutePathBuf::from_absolute_path(path)
            .is_ok_and(|path| path.as_path().starts_with(codex_home) || path.as_path() == env)
    };
    let word = |command: &[String]| {
        denied_word(
            command,
            cwd,
            Some(user_home),
            codex_home,
            denied,
            /*denial_lines*/ None,
            /*skip_patterns*/ false,
        )
    };
    let (found, not_found) = if cfg!(windows) {
        (
            vec![
                (
                    powershell(
                        r#"python -c "import os; print(os.listdir('C:\Users\someone\.corbanu'))""#,
                    ),
                    r"C:\Users\someone\.corbanu",
                ),
                (
                    powershell(
                        r#"python -c 'print(open(r"C:\Users\someone\.corbanu\auth.json").read())'"#,
                    ),
                    r"C:\Users\someone\.corbanu\auth.json",
                ),
                (
                    powershell(
                        r#"node -e "console.log(require('fs').readdirSync('$env:CORBANU_HOME'))""#,
                    ),
                    "$env:CORBANU_HOME",
                ),
                (
                    cmd(r#"python -c "print(open(r'%USERPROFILE%\.corbanu\auth.json').read())""#),
                    r"%USERPROFILE%\.corbanu\auth.json",
                ),
                (
                    powershell(r#"python -c "print(open('.env').read())""#),
                    ".env",
                ),
            ],
            vec![
                powershell(r#"python -c "print(1 + 2)""#),
                powershell(r#"python -c "import os; print(os.listdir('C:\work\project'))""#),
                powershell(r#"node -e "console.log('hello, world')""#),
            ],
        )
    } else {
        (
            vec![
                (
                    shell("python3 -c \"import os; print(os.listdir('/Users/someone/.corbanu'))\""),
                    "/Users/someone/.corbanu",
                ),
                (
                    shell("python -c 'print(open(\"/Users/someone/.corbanu/auth.json\").read())'"),
                    "/Users/someone/.corbanu/auth.json",
                ),
                (
                    shell("node -e \"console.log(require('fs').readdirSync('$CORBANU_HOME'))\""),
                    "$CORBANU_HOME",
                ),
                (
                    shell("perl -e 'opendir(D, \"~/.corbanu\") or die; print readdir(D)'"),
                    "~/.corbanu",
                ),
                (
                    shell("ruby -e 'puts Dir.entries(\"/Users/someone/.corbanu\")'"),
                    "/Users/someone/.corbanu",
                ),
                (shell("python3 -c \"print(open('.env').read())\""), ".env"),
            ],
            vec![
                shell("python3 -c \"print(1 + 2)\""),
                shell("python3 -c \"import os; print(os.listdir('/work/project'))\""),
                shell("node -e \"console.log('hello, world')\""),
            ],
        )
    };
    for (command, expected) in found {
        assert_eq!(word(&command), Some(expected.to_string()), "{command:?}");
    }
    for command in not_found {
        assert_eq!(word(&command), None, "{command:?}");
    }
}

/// #1296: Windows' denial texts, word splitting and home spellings are read
/// only on Windows (`windows`); elsewhere nothing changes.
#[test]
fn issue_1296_windows_texts_are_read_only_on_windows() {
    for line in [
        r"Get-Content : Access to the path 'C:\Users\me\.corbanu\auth.json' is denied.",
        r"Get-Content: Access to the path 'C:\Users\me\.corbanu\auth.json' is denied.",
        "Access is denied.",
        "access is denied",
    ] {
        assert!(is_denial_line(line, /*windows*/ true), "{line}");
        assert!(!is_denial_line(line, /*windows*/ false), "{line}");
    }
    for line in [
        "cat: .env: Operation not permitted",
        "PermissionError: [Errno 13] Permission denied: 'auth.json'",
    ] {
        assert!(is_denial_line(line, /*windows*/ true), "{line}");
        assert!(is_denial_line(line, /*windows*/ false), "{line}");
    }
    assert!(!is_denial_line(
        r"Get-Content : Cannot find path 'C:\x' because it does not exist.",
        /*windows*/ true
    ));
    assert!(names_no_path("Access is denied.\r", /*windows*/ true));
    assert!(!names_no_path("Access is denied.", /*windows*/ false));
    assert!(!names_no_path(
        r"Access to the path 'C:\x' is denied.",
        /*windows*/ true
    ));

    let words = |command: Vec<String>, windows| command_words(&command, windows);
    assert_eq!(
        words(
            powershell(r#"Get-Content C:\Users\me\.env 'C:\a b\x' "$env:HOME\y""#),
            /*windows*/ true
        ),
        vec![
            "powershell.exe",
            "-NoProfile",
            "-Command",
            "Get-Content",
            r"C:\Users\me\.env",
            r"C:\a b\x",
            r"$env:HOME\y",
        ]
    );
    // A POSIX shell, or any command off Windows, keeps POSIX quoting.
    let bash = vec![
        "C:\\Git\\bin\\bash.exe".to_string(),
        "-lc".into(),
        r"cat a\ b".into(),
    ];
    assert_eq!(words(bash, /*windows*/ true)[1..], ["-lc", "cat", "a b"]);
    assert_eq!(
        words(powershell(r"cat C:\x"), /*windows*/ false),
        vec!["powershell.exe", "-NoProfile", "-Command", "cat", "C:x"]
    );

    let cwd = Path::new(r"C:\work");
    let (user_home, codex_home) = (
        Path::new(r"C:\Users\me"),
        Path::new(r"C:\Users\me\.corbanu"),
    );
    let expand = |word: &str, windows| expand(word, cwd, Some(user_home), codex_home, windows);
    for (word, expected) in [
        (r"$env:USERPROFILE\.ssh", r"C:\Users\me\.ssh"),
        (r"${env:userprofile}\.ssh", r"C:\Users\me\.ssh"),
        (r"$HOME\.ssh", r"C:\Users\me\.ssh"),
        (r"%UserProfile%\.ssh", r"C:\Users\me\.ssh"),
        (r"%CODEX_HOME%\auth.json", r"C:\Users\me\.corbanu\auth.json"),
        (
            r"$env:CORBANU_HOME\auth.json",
            r"C:\Users\me\.corbanu\auth.json",
        ),
        (r"$CODEX_HOME\auth.json", r"C:\Users\me\.corbanu\auth.json"),
        (r"~\.ssh\config", r"C:\Users\me\.ssh\config"),
        (r"$PWD\.env", r"C:\work\.env"),
        (r".\.env", r".\.env"),
    ] {
        assert_eq!(
            expand(word, /*windows*/ true),
            Some(expected.to_string()),
            "{word}"
        );
    }
    assert_eq!(expand(r"$env:UNKNOWN\.env", /*windows*/ true), None);
    assert_eq!(expand(r"~other\.ssh", /*windows*/ true), None);
    // Off Windows, only the POSIX spellings.
    assert_eq!(expand(r"$env:USERPROFILE\.ssh", /*windows*/ false), None);
    assert_eq!(expand(r"~\.ssh", /*windows*/ false), None);
    assert_eq!(
        expand(r"%USERPROFILE%\.ssh", /*windows*/ false),
        Some(r"%USERPROFILE%\.ssh".to_string())
    );
}

/// #1296: the Windows spellings of a protected path a command names are
/// found (so no grant is offered for it): unquoted backslash paths, `~\`,
/// PowerShell's `$env:`, `${env:}` and `$HOME`, cmd's `%NAME%` (any letter
/// case) and `.\`; a path a POSIX shell names keeps POSIX quoting.
#[cfg(windows)]
#[test]
fn issue_1296_windows_spellings_of_a_protected_path_are_found() {
    let cwd = Path::new(r"C:\work\project");
    let user_home = Path::new(r"C:\Users\someone");
    let codex_home = Path::new(r"C:\Users\someone\.corbanu");
    let denied = |path: &Path| {
        AbsolutePathBuf::from_absolute_path(path).is_ok_and(|path| {
            [
                Path::new(r"C:\work\project\.env"),
                Path::new(r"C:\Users\someone\.ssh"),
                Path::new(r"C:\Users\someone\.corbanu\auth.json"),
            ]
            .iter()
            .any(|denied| path.as_path().starts_with(denied))
        })
    };
    let word = |command: &[String]| {
        denied_word(
            command,
            cwd,
            Some(user_home),
            codex_home,
            denied,
            /*denial_lines*/ None,
            /*skip_patterns*/ false,
        )
    };
    for (command, expected) in [
        (
            powershell(r"Get-Content C:\Users\someone\.corbanu\auth.json"),
            r"C:\Users\someone\.corbanu\auth.json",
        ),
        (
            cmd(r"type C:\Users\someone\.corbanu\auth.json"),
            r"C:\Users\someone\.corbanu\auth.json",
        ),
        (
            powershell(r"Get-Content 'C:\Users\someone\.corbanu\auth.json'"),
            r"C:\Users\someone\.corbanu\auth.json",
        ),
        (
            powershell("Get-Content C:/Users/someone/.corbanu/auth.json"),
            "C:/Users/someone/.corbanu/auth.json",
        ),
        (
            powershell(r"Get-Content $env:USERPROFILE\.corbanu\auth.json"),
            r"$env:USERPROFILE\.corbanu\auth.json",
        ),
        (
            powershell(r"Get-Content ${env:userprofile}\.ssh\id_ed25519"),
            r"${env:userprofile}\.ssh\id_ed25519",
        ),
        (
            powershell(r"Get-Content $HOME\.ssh\config"),
            r"$HOME\.ssh\config",
        ),
        (
            powershell(r"Get-Content ~\.corbanu\auth.json"),
            r"~\.corbanu\auth.json",
        ),
        (
            powershell(r"Get-Content $env:CODEX_HOME\auth.json"),
            r"$env:CODEX_HOME\auth.json",
        ),
        (
            cmd(r"type %USERPROFILE%\.corbanu\auth.json"),
            r"%USERPROFILE%\.corbanu\auth.json",
        ),
        (
            cmd(r"type %corbanu_home%\auth.json"),
            r"%corbanu_home%\auth.json",
        ),
        (powershell(r"Get-Content .\.env"), r".\.env"),
        (powershell(r"Get-Content -Path .env"), ".env"),
        (cmd(r"type .\.env"), r".\.env"),
        // A POSIX shell (Git for Windows) keeps POSIX quoting.
        (
            vec!["bash.exe".into(), "-lc".into(), "cat ~/.ssh/config".into()],
            "~/.ssh/config",
        ),
    ] {
        assert_eq!(word(&command), Some(expected.to_string()), "{command:?}");
    }
    for command in [
        powershell(r"Get-Content C:\work\project\notes.txt"),
        powershell(r"Get-Content $env:UNKNOWN\.env.example"),
        cmd(r"type %UNKNOWN%\.ssh\config"),
        cmd(r"type %USERPROFILE:~0%\.ssh\config"),
    ] {
        assert_eq!(word(&command), None, "{command:?}");
    }
}

/// #1296: under Aggressive on Windows, a protected read PowerShell or cmd
/// was denied is explained where it failed (Windows PowerShell's, PowerShell
/// 7's and cmd's denial texts), and a recursive listing of the folder above
/// the Corbanu home like a listing of the home (#554); not under Permissive
/// or for a failure that is not a denial.
#[cfg(windows)]
#[tokio::test]
async fn issue_1296_powershell_and_cmd_denials_are_explained_under_aggressive() {
    let (session, turn) = crate::session::tests::make_session_and_context().await;
    #[allow(deprecated)]
    let cwd = turn.cwd.clone();
    let home = turn.config.codex_home.to_path_buf();
    std::fs::create_dir_all(&home).unwrap();
    let auth = home.join("auth.json");
    std::fs::write(&auth, "x").unwrap();
    let parent = home.parent().expect("the home has a parent").to_path_buf();
    let (auth, home, parent) = (
        auth.display().to_string(),
        home.display().to_string(),
        parent.display().to_string(),
    );
    let ps_denied =
        |path: &str| format!("Get-Content : Access to the path '{path}' is denied.\r\n");
    let explain = async |command: &[String], exit_code, output: &str| {
        denial_explanation(
            &session,
            &turn,
            command,
            cwd.as_path(),
            Some(exit_code),
            output,
        )
        .await
    };
    let quoted = powershell(&format!("Get-Content '{auth}'"));
    assert_eq!(
        explain(&quoted, 1, &ps_denied(&auth)).await,
        None,
        "Permissive: unchanged"
    );

    let client = (*session.services.model_client())
        .clone()
        .with_ingress_level(SecurityLevel::Aggressive)
        .with_source_envelopes(/*enabled*/ true);
    session.services.replace_model_client(client);
    for (command, output, shown) in [
        (quoted.clone(), ps_denied(&auth), auth.clone()),
        (
            powershell(&format!("Get-Content {auth}")),
            format!("Get-Content: Access to the path '{auth}' is denied.\r\n"),
            auth.clone(),
        ),
        (
            powershell(r"Get-Content $env:CODEX_HOME\auth.json"),
            ps_denied(&auth),
            r"$env:CODEX_HOME\auth.json".to_string(),
        ),
        (
            cmd(&format!("type \"{auth}\"")),
            "Access is denied.\r\n".to_string(),
            auth.clone(),
        ),
        (
            cmd(r"type %CODEX_HOME%\auth.json"),
            "Access is denied.\r\n".to_string(),
            r"%CODEX_HOME%\auth.json".to_string(),
        ),
    ] {
        assert_eq!(
            explain(&command, 1, &output).await,
            Some(explanation(&shown)),
            "{command:?}: {output}"
        );
        // #1312: the command names the path, so also when a later command
        // on the line made it exit 0.
        assert_eq!(
            explain(&command, 0, &output).await,
            Some(explanation(&shown)),
            "{command:?}"
        );
    }
    let listing = powershell(&format!("Get-ChildItem -Recurse '{parent}'"));
    assert_eq!(
        explain(
            &listing,
            1,
            &format!("Get-ChildItem : Access to the path '{home}' is denied.\r\n"),
        )
        .await,
        Some(listing_explanation(&home))
    );
    let missing = format!(
        "Get-Content : Cannot find path '{}' because it does not exist.\r\n",
        cwd.join("missing.txt").display()
    );
    assert_eq!(explain(&quoted, 1, &missing).await, None);
    assert_eq!(
        protected_path_in_command(
            &session,
            &turn,
            &powershell(&format!("Get-Content {auth}")),
            cwd.as_path()
        )
        .await,
        Some(auth.clone())
    );
    assert_eq!(
        protected_path_in_command(
            &session,
            &turn,
            &cmd(r"type %CODEX_HOME%\auth.json"),
            cwd.as_path()
        )
        .await,
        Some(r"%CODEX_HOME%\auth.json".to_string())
    );
}

/// An Aggressive session (with the source labels it turns on).
#[cfg(unix)]
fn aggressive(session: &Session) {
    let client = (*session.services.model_client())
        .clone()
        .with_ingress_level(SecurityLevel::Aggressive)
        .with_source_envelopes(/*enabled*/ true);
    session.services.replace_model_client(client);
}

/// The recent denials the inspector shows for `session` (not the
/// process-wide ones other tests in this process may have recorded).
#[cfg(unix)]
fn recent_denials(session: &Session, turn: &TurnContext) -> Vec<(&'static str, &'static str)> {
    crate::security::inspection::observe(
        turn.config.codex_home.as_path(),
        SecurityLevel::Aggressive,
        Some(session.thread_id()),
        crate::security::aggressive::now_unix_seconds(),
    )
    .denials
    .into_iter()
    .filter(|denial| denial.thread == Some(session.thread_id()))
    .map(|denial| (denial.reason, denial.outcome))
    .collect()
}

/// #621 and #622: under Aggressive, a protected read or home listing an
/// interpreter makes is explained where it failed, also when the command
/// built the path at run time (the denial line names it), and it is listed
/// under the inspector's recent denials. The approval prompt for the listing
/// offers no grant (`protected_note_in_command`; Windows does not enforce
/// the listing denial, so it says nothing there).
#[cfg(unix)]
#[tokio::test]
async fn issue_621_622_interpreter_denials_are_explained_and_listed() {
    let (session, turn) = crate::session::tests::make_session_and_context().await;
    #[allow(deprecated)]
    let cwd = turn.cwd.clone();
    let home = turn.config.codex_home.to_path_buf();
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("auth.json"), "x").unwrap();
    let listing = shell(&format!(
        "python3 -c \"import os; print(os.listdir('{}'))\"",
        home.display()
    ));
    let listing_denied = format!(
        "Traceback (most recent call last):\n  File \"<string>\", line 1, in <module>\n\
         PermissionError: [Errno 1] Operation not permitted: '{}'\n",
        home.display()
    );
    // The path is built at run time: no word names it.
    let built = shell("python3 -c \"import os; open(os.environ['P'] + '/auth.json').read()\"");
    let built_denied = format!(
        "PermissionError: [Errno 1] Operation not permitted: '{}'\n",
        home.join("auth.json").display()
    );
    let explain = async |command: &[String], output: &str| {
        denial_explanation(&session, &turn, command, cwd.as_path(), Some(1), output).await
    };
    assert_eq!(explain(&listing, &listing_denied).await, None, "Permissive");
    assert_eq!(recent_denials(&session, &turn), Vec::new());

    aggressive(&session);
    assert_eq!(
        explain(&listing, &listing_denied).await,
        Some(listing_explanation(&home.display().to_string()))
    );
    assert_eq!(
        explain(&built, &built_denied).await,
        Some(explanation(&home.join("auth.json").display().to_string()))
    );
    let note = protected_note_in_command(&session, &turn, &listing, cwd.as_path())
        .await
        .expect("the listing is recognised before it runs");
    assert!(
        note.explanation()
            .contains("cannot be granted, not even once"),
        "{}",
        note.explanation()
    );
    let outcome = "denied by the sandbox; never granted under Aggressive";
    assert_eq!(
        recent_denials(&session, &turn),
        vec![
            ("reading a protected file", outcome),
            ("listing the Corbanu home", outcome),
        ]
    );
    // A failure that is not a denial of a protected path is not listed.
    assert_eq!(
        explain(
            &built,
            "PermissionError: [Errno 13] Permission denied: '/elsewhere/file'\n"
        )
        .await,
        None
    );
    assert_eq!(recent_denials(&session, &turn).len(), 2);
}

/// #620 (milestone AGG-18, AGG-R17): under Aggressive a start-up file
/// outside the current folder (`~/.zshrc`) can never be written, not even
/// with Grant once, so the approval prompt offers no grant and says why;
/// a denied write is explained the same way where it failed. A write in the
/// current folder, and Permissive sessions, are left alone.
#[cfg(unix)]
#[tokio::test]
async fn issue_620_startup_file_outside_the_folder_is_never_granted() {
    let (session, turn) = crate::session::tests::make_session_and_context().await;
    #[allow(deprecated)]
    let cwd = turn.cwd.clone();
    let user_home = dirs::home_dir().expect("a home folder");
    let append = shell("echo '# grant-once-1' >> ~/.zshrc");
    let denied = format!(
        "zsh:1: operation not permitted: {}\n",
        user_home.join(".zshrc").display()
    );
    let note = async |command: &[String]| {
        protected_note_in_command(&session, &turn, command, cwd.as_path())
            .await
            .map(|note| note.explanation())
    };
    assert_eq!(note(&append).await, None, "Permissive");

    aggressive(&session);
    let explained = note(&append).await.expect("no grant for ~/.zshrc");
    assert_eq!(explained, startup_explanation("~/.zshrc"));
    assert!(
        explained.contains("cannot be granted, not even once"),
        "{explained}"
    );
    assert!(explained.contains("Permissive in /security"), "{explained}");
    assert_eq!(
        note(&shell("echo '# x' >> \"$HOME/.bash_profile\"")).await,
        Some(startup_explanation("$HOME/.bash_profile"))
    );
    assert_eq!(
        note(&shell(
            "cp notes.txt ~/.bashrc && tee -a ~/.zshenv < notes.txt"
        ))
        .await,
        Some(startup_explanation("~/.bashrc"))
    );
    assert_eq!(
        note(&shell("printf x>>~/.zshrc")).await,
        Some(startup_explanation("~/.zshrc"))
    );
    // Only where the command writes it: reading one, running a program from
    // `~/.local/bin` or naming one in a message is not a start-up file write
    // (#740 explains a write no grant lifts instead; an unknown program
    // keeps the grant).
    for script in [
        "~/.local/bin/pre-commit install",
        "git commit -m 'document ~/.zshrc > notes'",
    ] {
        assert_eq!(note(&shell(script)).await, None, "{script}");
    }
    // This session's profile may not write its folder at all: then a write
    // there is outside the writable folders, else read-only or plain.
    for (script, word) in [
        ("cat ~/.gitconfig >> .git/config", ".git/config"),
        (
            "grep x ~/.bashrc > .git/hooks/pre-commit 2>&1",
            ".git/hooks/pre-commit",
        ),
        ("echo '# x' >> notes.txt", "notes.txt"),
    ] {
        let explained = note(&shell(script)).await;
        assert!(
            [
                outside_folder_explanation(word),
                read_only_in_folder_explanation(word),
                NOTHING_TO_LIFT.to_string(),
            ]
            .iter()
            .any(|expected| explained.as_ref() == Some(expected))
                && (word == "notes.txt" || explained != Some(NOTHING_TO_LIFT.to_string())),
            "{script}: {explained:?}"
        );
    }

    assert_eq!(
        denial_explanation(&session, &turn, &append, cwd.as_path(), Some(1), &denied).await,
        Some(startup_explanation("~/.zshrc"))
    );
    assert_eq!(
        recent_denials(&session, &turn),
        vec![(
            "writing a start-up file outside the current folder",
            "denied by the sandbox; never granted under Aggressive"
        )]
    );
}

/// #620: a start-up file reached through links counts by every name on the
/// way (`./rc -> ~/.zshrc -> ~/dotfiles/zshrc`, as dotfile managers link
/// it), and is writable where any of those names is (a workspace that is
/// the home folder keeps Grant once).
#[cfg(unix)]
#[test]
fn issue_620_start_up_files_through_links() {
    use codex_protocol::permissions::NetworkSandboxPolicy;
    let user_home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let home = user_home.path();
    std::fs::create_dir(home.join("dotfiles")).unwrap();
    std::fs::write(home.join("dotfiles/zshrc"), "").unwrap();
    std::os::unix::fs::symlink(home.join("dotfiles/zshrc"), home.join(".zshrc")).unwrap();
    std::os::unix::fs::symlink(home.join(".zshrc"), work.path().join("rc")).unwrap();
    let inputs = |folder: &Path| {
        let folder = AbsolutePathBuf::from_absolute_path(folder.canonicalize().unwrap()).unwrap();
        ReadPolicyInputs {
            codex_home: home.join(".corbanu"),
            sqlite_home: home.join(".corbanu"),
            turn_cwd: folder.clone(),
            workspace_roots: vec![folder],
            // Aggressive's sandbox: the current folder only.
            profile: PermissionProfile::workspace_write_with(
                &[],
                NetworkSandboxPolicy::Restricted,
                /*exclude_tmpdir_env_var*/ true,
                /*exclude_slash_tmp*/ true,
            ),
            user_home: Some(home.to_path_buf()),
            keychain_closed: false,
        }
    };
    let in_work = inputs(work.path());
    assert!(in_work.unwritable_startup_file(&work.path().join("rc")));
    assert!(in_work.unwritable_startup_file(&home.join(".zshrc")));
    assert!(in_work.unwritable_startup_file(&home.join(".bash_profile")));
    assert!(!in_work.unwritable_startup_file(&work.path().join("notes.txt")));
    assert!(!in_work.unwritable_startup_file(&home.join("notes.txt")));
    let in_home = inputs(home);
    assert!(!in_home.unwritable_startup_file(&home.join(".zshrc")));
    assert!(!in_home.unwritable_startup_file(&home.join(".bash_profile")));
}

/// #620: the words a command writes to, and not the ones it only reads or
/// runs.
#[test]
fn issue_620_write_targets() {
    let targets = |script: &str| write_targets(&shell(script));
    assert_eq!(targets("echo x >> ~/.zshrc"), vec!["~/.zshrc"]);
    assert_eq!(targets("printf x>>~/.zshrc 2>&1"), vec!["~/.zshrc"]);
    assert_eq!(
        targets("cmd &> log; tee -a a b < in"),
        vec!["log", "a", "b"]
    );
    assert_eq!(targets("cp -r src ~/.config/fish"), vec!["~/.config/fish"]);
    assert_eq!(targets("sed -i 's/a/b/' f g"), vec!["f", "g"]);
    assert_eq!(targets("dd if=x of=y"), vec!["y"]);
    assert_eq!(targets("FOO=1 sudo touch t"), vec!["t"]);
    for script in [
        "cat ~/.zshrc",
        "tee out < ~/.bashrc",
        "cd .git/hooks && ln -s ~/.local/bin/pre-commit",
        "~/.local/bin/tool --flag",
        "git commit -m 'document ~/.zshrc'",
        "echo hi 2>&1",
    ] {
        assert!(
            targets(script)
                .iter()
                .all(|target| !target.contains(".zshrc")
                    && !target.contains(".bashrc")
                    && !target.contains("pre-commit")),
            "{script}: {:?}",
            targets(script)
        );
    }
    assert_eq!(
        targets("cmd >&out <>rw; mv ~/.zshrc x"),
        vec!["out", "rw", "~/.zshrc", "x"]
    );
    assert_eq!(
        write_targets(&[
            "env".to_string(),
            "bash".to_string(),
            "-c".to_string(),
            "echo x >> ~/.zshrc".to_string(),
        ]),
        vec!["~/.zshrc"]
    );
    assert_eq!(targets("bash -c 'tee -a ~/.zshrc < in'"), vec!["~/.zshrc"]);
    // Not through a shell, `>` in an argument is text.
    assert_eq!(
        write_targets(&[
            "git".to_string(),
            "commit".to_string(),
            "-m".to_string(),
            "x >~/.zshrc".to_string(),
        ]),
        Vec::<String>::new()
    );
    // Not through a shell: one simple command.
    assert_eq!(
        write_targets(&["tee".to_string(), "-a".to_string(), "/u/.zshrc".to_string()]),
        vec!["/u/.zshrc"]
    );
}

/// #584: the word a command writes that no grant lifts (the project's
/// `.codex` and `.agents`, the Corbanu home), when it writes no path a grant
/// lifts: through `..`, a link as the last part, and a Corbanu home reached
/// through a link, whose `worktrees` folder stays writable.
#[cfg(unix)]
#[test]
fn issue_584_only_never_writable_words() {
    use codex_protocol::permissions::NetworkSandboxPolicy;
    let base = tempfile::tempdir().unwrap();
    let base = base.path().canonicalize().unwrap();
    let work = base.join("work");
    let home = base.join("corbanu-home");
    for folder in [
        work.join(".codex"),
        work.join(".agents"),
        work.join(".git/hooks"),
        work.join("sub"),
        home.join("worktrees/x"),
    ] {
        std::fs::create_dir_all(&folder).unwrap();
    }
    std::os::unix::fs::symlink(work.join(".codex/config.toml"), work.join("settings")).unwrap();
    let linked_home = base.join("linked-home");
    std::os::unix::fs::symlink(&home, &linked_home).unwrap();
    let cwd = AbsolutePathBuf::from_absolute_path(&work).unwrap();
    let profile = PermissionProfile::workspace_write_with(
        &[],
        NetworkSandboxPolicy::Restricted,
        /*exclude_tmpdir_env_var*/ true,
        /*exclude_slash_tmp*/ true,
    );
    let word = |codex_home: &Path, script: &str| {
        let denials = ReadDenials::for_turn(
            codex_home,
            codex_home,
            &cwd,
            std::slice::from_ref(&cwd),
            &profile,
        );
        only_never_writable_word(
            &shell(script),
            &work,
            /*user_home*/ None,
            codex_home,
            &denials,
        )
    };
    for (script, expected) in [
        ("printf x > .codex/config.toml", Some(".codex/config.toml")),
        (
            "printf x >> sub/../.agents/notes.md",
            Some("sub/../.agents/notes.md"),
        ),
        ("printf x > settings", Some("settings")),
        (
            "cp notes.txt .agents/notes.md && tee .codex/a",
            Some(".agents/notes.md"),
        ),
        // A git hook beside it: Grant once still lifts that one.
        (
            "printf x > .git/hooks/pre-commit; printf x > .codex/a",
            None,
        ),
        ("printf x > notes.txt", None),
        ("cat .codex/config.toml", None),
    ] {
        assert_eq!(word(&home, script).as_deref(), expected, "{script}");
    }
    // A session working in a Corbanu worktree (#583): the worktree's own
    // `.codex` stays never writable (the most specific entry wins), its
    // other files are writable; also when the home is reached through a link.
    for spelled in [&home, &linked_home] {
        let worktree = spelled.join("worktrees/x");
        std::fs::create_dir_all(worktree.join(".codex")).unwrap();
        let cwd = AbsolutePathBuf::from_absolute_path(&worktree).unwrap();
        for codex_home in [&home, &linked_home] {
            let denials = ReadDenials::for_turn(
                codex_home,
                codex_home,
                &cwd,
                std::slice::from_ref(&cwd),
                &profile,
            );
            let word = |script: &str| {
                only_never_writable_word(
                    &shell(script),
                    &worktree,
                    /*user_home*/ None,
                    codex_home,
                    &denials,
                )
            };
            assert_eq!(
                word("printf x > .codex/a").as_deref(),
                Some(".codex/a"),
                "{spelled:?} {codex_home:?}"
            );
            assert_eq!(
                word("printf x > notes.md"),
                None,
                "{spelled:?} {codex_home:?}"
            );
        }
    }
    // The Corbanu home itself, also spelled through a link, but not its
    // writable `worktrees` folder (#583), whichever spelling is used.
    for codex_home in [&home, &linked_home] {
        let into_home = format!("printf x > {}/notes.md", linked_home.display());
        assert!(word(codex_home, &into_home).is_some(), "{codex_home:?}");
        for spelled in [&home, &linked_home] {
            let into_worktrees = format!("printf x > {}/worktrees/x/notes.md", spelled.display());
            assert_eq!(
                word(codex_home, &into_worktrees),
                None,
                "{codex_home:?} {into_worktrees}"
            );
        }
    }
}

/// #902: a test harness can only lengthen the lookups' bound; it is 2 s
/// otherwise. #997: this crate's unit tests always get the test bound.
#[test]
fn issue_902_unhurried_lookups_only_lengthen_the_bound() {
    let outside_unit_tests = || lookup_timeout_in(/*unit_test*/ false);
    assert_eq!(LOOKUP_TIMEOUT, Duration::from_secs(/*secs*/ 2));
    assert_eq!(outside_unit_tests(), LOOKUP_TIMEOUT);
    unhurried_lookups_for_tests(/*enabled*/ true);
    let unhurried = outside_unit_tests();
    let unit_test = lookup_timeout();
    unhurried_lookups_for_tests(/*enabled*/ false);
    assert_eq!(unhurried, TEST_LOOKUP_TIMEOUT);
    assert_eq!(unit_test, TEST_LOOKUP_TIMEOUT);
    assert!(TEST_LOOKUP_TIMEOUT > LOOKUP_TIMEOUT);
    assert_eq!(outside_unit_tests(), LOOKUP_TIMEOUT);
    assert_eq!(lookup_timeout(), TEST_LOOKUP_TIMEOUT);
}

/// #997: a lookup slower than the production bound still finishes in this
/// crate's unit tests, so a loaded machine does not drop the explanations
/// and withheld grant offers they check.
#[tokio::test]
async fn issue_997_unit_test_lookups_outlast_the_production_bound() {
    let slow = off_thread(|| {
        std::thread::sleep(LOOKUP_TIMEOUT + Duration::from_millis(/*millis*/ 500));
        Some("found")
    });
    assert_eq!(slow.await, Some("found"));
}

/// #1312 (local-model run 3, AGG-20P-A): under Aggressive, a denied write
/// outside the current folder is explained where it failed even when a
/// later command on the same line makes the whole command exit 0, as it is
/// when the denied write is the last command; and so is a start-up file. A
/// command with no denial line, or no known exit, is left alone, and so is
/// one that exited 0 whose output alone reports a denial.
#[cfg(unix)]
#[tokio::test]
async fn issue_1312_denial_hidden_by_a_later_command_is_explained() {
    let (session, turn) = crate::session::tests::make_session_and_context().await;
    #[allow(deprecated)]
    let cwd = turn.cwd.clone();
    let user_home = dirs::home_dir().expect("a home folder");
    let explain = async |script: &str, exit_code: Option<i32>, output: &str| {
        denial_explanation(
            &session,
            &turn,
            &shell(script),
            cwd.as_path(),
            exit_code,
            output,
        )
        .await
    };
    let outside = "echo x >> ../outside-notes.txt; echo \"exit=$?\"";
    let outside_denied = "/bin/bash: ../outside-notes.txt: Operation not permitted\nexit=1\n";
    let startup = "echo '# x' >> ~/.zshrc; echo \"exit=$?\"";
    let startup_denied = format!(
        "zsh:1: operation not permitted: {}\nexit=1\n",
        user_home.join(".zshrc").display()
    );
    assert_eq!(
        explain(outside, Some(0), outside_denied).await,
        None,
        "Permissive"
    );

    aggressive(&session);
    for exit_code in [Some(1), Some(0)] {
        assert_eq!(
            explain(outside, exit_code, outside_denied).await,
            Some(outside_folder_explanation("../outside-notes.txt")),
            "{exit_code:?}"
        );
        assert_eq!(
            explain(startup, exit_code, &startup_denied).await,
            Some(startup_explanation("~/.zshrc")),
            "{exit_code:?}"
        );
    }
    assert_eq!(
        explain(outside, Some(0), "exit=0\n").await,
        None,
        "no denial"
    );
    assert_eq!(
        explain(outside, None, outside_denied).await,
        None,
        "no exit"
    );
    // A denial line in the output alone, which the command does not name (a
    // search printing old denial lines), explains nothing when it exited 0;
    // a failed command is still explained by it (#621).
    let auth = turn.config.codex_home.join("auth.json");
    std::fs::create_dir_all(turn.config.codex_home.as_path()).unwrap();
    std::fs::write(&auth, "x").unwrap();
    let search = "rg -n 'not permitted' notes.md";
    let printed = format!(
        "notes.md:3:cat: {}: Operation not permitted\n",
        auth.display()
    );
    let before = recent_denials(&session, &turn).len();
    assert_eq!(
        explain(search, Some(0), &printed).await,
        None,
        "output alone"
    );
    assert_eq!(
        recent_denials(&session, &turn).len(),
        before,
        "nothing listed"
    );
    assert_eq!(
        explain(search, Some(1), &printed).await,
        Some(explanation(&auth.display().to_string()))
    );
    let denials = recent_denials(&session, &turn);
    for reason in [
        "writing outside the current folder",
        "writing a start-up file outside the current folder",
    ] {
        assert!(
            denials.contains(&(
                reason,
                "denied by the sandbox; never granted under Aggressive"
            )),
            "{denials:?}"
        );
    }
}

/// #1323: under Aggressive, a search that exited 0 is not explained by its
/// pattern (`rg ".env" logs/` printing an old denial line naming `.env`),
/// and lists no recent denial; a protected path it reads, or one a command
/// before a later one that hid its failure reads, still is (#1312), and a
/// failed search is explained as before.
#[cfg(unix)]
#[tokio::test]
async fn issue_1323_a_search_pattern_is_not_a_denied_path() {
    let (session, turn) = crate::session::tests::make_session_and_context().await;
    #[allow(deprecated)]
    let cwd = turn.cwd.clone();
    let explain = async |script: &str, exit_code: i32, output: &str| {
        denial_explanation(
            &session,
            &turn,
            &shell(script),
            cwd.as_path(),
            Some(exit_code),
            output,
        )
        .await
    };
    aggressive(&session);
    let auth = turn.config.codex_home.join("auth.json");
    std::fs::create_dir_all(turn.config.codex_home.as_path()).unwrap();
    std::fs::write(&auth, "x").unwrap();
    let auth = auth.display().to_string();
    let old_line = format!("logs/x.log:5:cat: {auth}: Permission denied\n");
    let before = recent_denials(&session, &turn).len();
    for search in [
        format!("rg '{auth}' logs/"),
        format!("grep -rn -e '{auth}' logs/"),
        format!("cat logs/x.log | grep -F '{auth}'"),
    ] {
        assert_eq!(explain(&search, 0, &old_line).await, None, "{search}");
    }
    assert_eq!(
        recent_denials(&session, &turn).len(),
        before,
        "nothing listed"
    );
    let read = Some(explanation(&auth));
    let denied = format!("cat: {auth}: Permission denied\nexit=1\n");
    let cat = format!("cat '{auth}'; echo \"exit=$?\"");
    assert_eq!(explain(&cat, 0, &denied).await, read);
    let grep_denied = format!("grep: {auth}: Permission denied\nexit=2\n");
    let grep = format!("grep token '{auth}'; echo \"exit=$?\"");
    assert_eq!(explain(&grep, 0, &grep_denied).await, read);
    // A pattern that runs a command: what that command opens is explained.
    let substituted = format!("grep \"$(cat '{auth}')\" notes.md; echo \"exit=$?\"");
    assert_eq!(explain(&substituted, 0, &denied).await, read);
    // A later line's read is not the search's pattern.
    let lines = format!("rg token logs/\ncat -e '{auth}'\necho done");
    assert_eq!(explain(&lines, 0, &denied).await, read);
    // A failed search: unchanged.
    let search = format!("rg '{auth}' logs/");
    assert_eq!(explain(&search, 1, &old_line).await, read);
}
