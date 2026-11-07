use pretty_assertions::assert_eq;

use super::*;
use crate::security::aggressive;
use crate::security::level::LevelContext;

fn context(boundary: Option<Boundary>) -> LevelContext {
    LevelContext {
        codex_home: PathBuf::from("/corbanu"),
        picker_enabled: true,
        active: ChosenLevel::Aggressive,
        preflight_enabled: true,
        boundary,
    }
}

fn clean_preflight(home: &Path) -> Preflight {
    Preflight::run(
        &file_sources(home, /*home*/ None, /*cwd*/ None),
        ReadinessFlags {
            secretless_launch: true,
            credential_broker: true,
            output_gate: true,
            untrusted_content: true,
        },
    )
}

#[test]
fn pf_29_s01_receipt_round_trip_and_corrupt_receipts_fail_closed() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(load_receipt(home.path()), Ok(None));
    save_receipt(home.path(), &clean_preflight(home.path())).unwrap();
    let receipt = load_receipt(home.path()).unwrap().unwrap();
    assert_eq!(receipt.activated_at, None);

    std::fs::write(receipt_path(home.path()), "version = 1\nsaved_at = \"x\"").unwrap();
    assert!(load_receipt(home.path()).is_err());
    // Version 1 stored seconds; it must not be read as milliseconds.
    for version in [1, 9] {
        std::fs::write(
            receipt_path(home.path()),
            format!("version = {version}\nsaved_at = 1\nactivated_at = 1791323285\nfindings = []"),
        )
        .unwrap();
        assert!(load_receipt(home.path()).is_err(), "version {version}");
    }

    remove_receipt(home.path()).unwrap();
    remove_receipt(home.path()).unwrap();
    assert_eq!(load_receipt(home.path()), Ok(None));
}

#[test]
fn pf_29_s01_isolation_only_after_a_preflight_and_lands_in_the_profile() {
    let root = tempfile::tempdir().unwrap();
    let corbanu = root.path().join("corbanu");
    let home = root.path().join("home");
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::create_dir_all(&corbanu).unwrap();
    std::fs::write(home.join(".ssh/id_rsa"), "fake").unwrap();

    assert_eq!(
        isolation_paths(&corbanu, Some(&home), /*cwd*/ None),
        Vec::<PathBuf>::new()
    );
    // Even an unreadable receipt asks for isolation.
    std::fs::write(receipt_path(&corbanu), "garbage").unwrap();
    std::fs::write(corbanu.join("state_5.sqlite"), "db").unwrap();
    let paths = isolation_paths(&corbanu, Some(&home), /*cwd*/ None);
    for path in [
        home.join(".ssh"),
        home.join(".ssh/id_rsa"),
        // Corbanu's stores even before startup creates them, and every
        // database (with its -wal/-shm files) through one glob.
        corbanu.join("sessions"),
        corbanu.join("history.jsonl"),
        corbanu.join("*.sqlite*"),
    ] {
        assert!(
            paths.contains(&path),
            "{} missing from {paths:?}",
            path.display()
        );
    }
    assert!(!paths.contains(&corbanu.join("state_5.sqlite")));
    assert!(
        corbanu.join("sessions").is_dir(),
        "store folders exist from launch"
    );

    // #651: a home with glob syntax: its database glob is escaped (it
    // matches the databases there only), its stores stay as written (a deny
    // key with glob syntax is a pattern, which launch then refuses, failing
    // closed).
    let odd = root.path().join("corbanu[1]");
    std::fs::create_dir_all(&odd).unwrap();
    std::fs::write(receipt_path(&odd), "garbage").unwrap();
    std::fs::write(odd.join("state_5.sqlite"), "db").unwrap();
    let odd_paths = isolation_paths(&odd, Some(&home), /*cwd*/ None);
    assert!(
        odd_paths.contains(&root.path().join("corbanu[[]1]").join("*.sqlite*")),
        "{odd_paths:?}"
    );
    assert!(odd_paths.contains(&odd.join("sessions")));
    assert!(!odd_paths.contains(&odd.join("state_5.sqlite")));
    assert!(!odd_paths.contains(&odd.join("*.sqlite*")));

    // #651: a database folder set apart in the user config, relative to the
    // config file's folder as Core resolves it: its databases and what
    // Corbanu keeps there (when present).
    let databases = root.path().join("db");
    std::fs::create_dir_all(databases.join("accounting-pending")).unwrap();
    std::fs::write(corbanu.join("config.toml"), "sqlite_home = \"../db\"\n").unwrap();
    let with_folder = isolation_paths(&corbanu, Some(&home), /*cwd*/ None);
    assert!(
        with_folder.contains(&databases.join("*.sqlite*")),
        "{with_folder:?}"
    );
    assert!(with_folder.contains(&databases.join("accounting-pending")));
    assert!(!with_folder.contains(&databases.join("gpu-controller.lock")));
    std::fs::remove_file(corbanu.join("config.toml")).unwrap();
    // On the command line, against the working folder.
    let given = override_database_isolation(
        &corbanu,
        &[(
            "sqlite_home".to_string(),
            toml::Value::String("db".to_string()),
        )],
        Some(root.path()),
    );
    assert!(given.contains(&databases.join("*.sqlite*")), "{given:?}");
    // `~` is expanded as Core expands it.
    if let Some(user_home) = dirs::home_dir() {
        assert_eq!(resolve_folder("~/db", root.path()), user_home.join("db"));
    }
    assert_eq!(
        override_database_isolation(
            &corbanu,
            &[(
                "sqlite_home".to_string(),
                toml::Value::String(".".to_string())
            )],
            Some(&corbanu),
        ),
        Vec::<PathBuf>::new()
    );

    let mut overrides = aggressive::base_overrides(&corbanu, &corbanu);
    aggressive::deny_reads(&mut overrides, &paths);
    let profile = overrides
        .iter()
        .find(|(key, _)| key == &format!("permissions.{}", aggressive::PROFILE_ID))
        .map(|(_, value)| value.clone())
        .unwrap();
    // The keys are the paths as the inventory spells them, with this
    // platform's separators.
    for path in [home.join(".ssh").join("id_rsa"), corbanu.join("*.sqlite*")] {
        assert_eq!(
            profile["filesystem"][path.to_string_lossy().as_ref()],
            toml::Value::String("deny".to_string())
        );
    }
}

/// #1441: SQLite creates a missing `-wal` through a dangling symlink, so
/// where it would land is on the launch deny list.
#[cfg(unix)]
#[test]
fn pf_29_s01_dangling_database_side_file_target_is_denied_at_launch() {
    let root = tempfile::tempdir().unwrap();
    let corbanu = root.path().join("corbanu");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&corbanu).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(receipt_path(&corbanu), "garbage").unwrap();
    std::fs::write(corbanu.join("state_5.sqlite"), "db").unwrap();
    std::os::unix::fs::symlink(outside.join("wal"), corbanu.join("state_5.sqlite-wal")).unwrap();

    let paths = isolation_paths(&corbanu, /*home*/ None, /*cwd*/ None);
    let target = std::fs::canonicalize(&outside).unwrap().join("wal");
    assert!(paths.contains(&target), "{paths:?}");
}

#[test]
fn pf_29_s01_resume_refused_for_conversations_before_activation() {
    let now = now_ms();
    let fresh = ThreadId::new();
    let created = thread_created_at(&fresh).unwrap();
    assert!((created - now).abs() <= 2_000, "{created} vs {now}");

    // Millisecond precision: a thread one millisecond older is refused.
    let at_creation = context(Some(Boundary::Clean {
        activated_at: created + 1,
    }));
    assert!(refusal_for(&at_creation, &fresh).is_some());
    let clean_before = context(Some(Boundary::Clean {
        activated_at: created,
    }));
    assert_eq!(refusal_for(&clean_before, &fresh), None);
    let clean_after = context(Some(Boundary::Clean {
        activated_at: now + 60_000,
    }));
    assert!(
        refusal_for(&clean_after, &fresh)
            .unwrap()
            .contains("recorded before protected mode was activated")
    );
    for boundary in [
        Boundary::NotClean {
            blockers: vec!["x".to_string()],
        },
        Boundary::Unverified("corrupt".to_string()),
    ] {
        assert!(refusal_for(&context(Some(boundary)), &fresh).is_some());
    }
    // Flag off (no boundary) or Permissive: today's behaviour.
    assert_eq!(refusal_for(&context(/*boundary*/ None), &fresh), None);
    let mut permissive = context(Some(Boundary::Unverified("x".to_string())));
    permissive.active = ChosenLevel::Permissive;
    assert_eq!(refusal_for(&permissive, &fresh), None);
}

/// #444 (AGG-32): a config layer present at start was reported as "not
/// found when Corbanu Terminal started".
#[test]
fn sec_444_undenied_config_layer_is_not_called_missing_at_start() {
    let line = "Isolation: agent commands can still read /etc/codex/config.toml";
    let layer = super::not_denied_at_launch(line, /*config_layer*/ true);
    assert!(!layer.contains("not found"), "{layer}");
    assert!(!layer.contains("not there"), "{layer}");
    assert!(layer.contains("config files are read only after the sandbox is set up"));
    let file = super::not_denied_at_launch(line, /*config_layer*/ false);
    assert!(file.contains("it was not there when Corbanu Terminal started"));
}

// Only owned fixture roots enter this launch-audit phase. Calling the whole
// audit would also inventory the process home and environment.
async fn launch_isolation_config(home: &Path, cwd: &Path, denied: &[PathBuf]) -> Config {
    use crate::legacy_core::config::ConfigBuilder;
    use crate::legacy_core::config::ConfigOverrides;

    let mut filesystem = toml::map::Map::new();
    for path in [home, cwd] {
        filesystem.insert(
            path.to_str().unwrap().to_string(),
            toml::Value::String("read".to_string()),
        );
    }
    for path in denied {
        filesystem.insert(
            path.to_str().unwrap().to_string(),
            toml::Value::String("deny".to_string()),
        );
    }
    let mut network = toml::map::Map::new();
    network.insert("enabled".to_string(), toml::Value::Boolean(false));
    let mut profile = toml::map::Map::new();
    profile.insert("filesystem".to_string(), toml::Value::Table(filesystem));
    profile.insert("network".to_string(), toml::Value::Table(network));
    ConfigBuilder::default()
        .codex_home(home.to_path_buf())
        .harness_overrides(ConfigOverrides {
            cwd: Some(cwd.to_path_buf()),
            default_permissions: Some("launch-audit-fixture".to_string()),
            ..Default::default()
        })
        .cli_overrides(vec![(
            "permissions.launch-audit-fixture".to_string(),
            toml::Value::Table(profile),
        )])
        .loader_overrides(codex_config::LoaderOverrides::without_managed_config_for_tests())
        .build()
        .await
        .unwrap()
}

#[tokio::test]
async fn issue_951_launch_isolation_batch_preserves_messages_and_order() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let denied = cwd.path().join("denied");
    std::fs::create_dir(&denied).unwrap();
    let database_pattern = database_glob(home.path());
    let config = launch_isolation_config(
        home.path(),
        cwd.path(),
        &[denied.clone(), database_pattern.clone()],
    )
    .await;
    let ordinary = cwd.path().join("ordinary.key");
    let layer = cwd.path().join("config.toml");
    let unmatched_pattern = cwd.path().join("other-db").join("*.sqlite*");
    let literal_glob_syntax = cwd.path().join("literal[1]");
    let covered_database = home.path().join("state_5.sqlite-wal");
    let paths = vec![
        ordinary.clone(),
        denied.join("secret.key"),
        layer.clone(),
        database_pattern,
        covered_database,
        unmatched_pattern.clone(),
        ordinary.clone(),
        literal_glob_syntax.clone(),
    ];
    let layers = vec![layer.clone()];
    let missing = |path: &Path| {
        format!(
            "Isolation: agent commands can still read {}; it was not there when Corbanu Terminal started (another folder, or a new file). Remove it, or restart from the folder that holds it",
            path.display()
        )
    };
    let expected = vec![
        missing(&ordinary),
        format!(
            "Isolation: agent commands can still read {}; this config file holds a secret, and config files are read only after the sandbox is set up, so it was not denied. Move the secret into the vault (or remove it) and restart",
            layer.display()
        ),
        missing(&unmatched_pattern),
        missing(&ordinary),
        missing(&literal_glob_syntax),
    ];
    assert_eq!(
        launch_isolation_blockers(&config, &paths, &layers),
        expected
    );
    // This reference retains the old caller's per-path verdicts and wording.
    let singleton_messages = paths
        .iter()
        .flat_map(|path| {
            verify_isolation(&config, std::slice::from_ref(path))
                .into_iter()
                .map(|line| not_denied_at_launch(&line, layers.contains(path)))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        launch_isolation_blockers(&config, &paths, &layers),
        singleton_messages
    );
    assert_eq!(
        launch_isolation_blockers(&config, &[], &layers),
        Vec::<String>::new()
    );
    assert_eq!(
        launch_isolation_blockers(&config, &[denied.join("another.key")], &layers),
        Vec::<String>::new()
    );
}

// macOS deliberately keeps the Seatbelt verdict path. The normalization
// counter is available to dependent debug tests, including after PR1004.
#[cfg(all(debug_assertions, not(target_os = "macos")))]
#[tokio::test]
async fn issue_951_launch_isolation_prepares_one_effective_read_set() {
    use codex_protocol::permissions::effective_path_normalizations_on_this_thread;

    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let denied = (0..20)
        .map(|index| cwd.path().join(format!("denied-{index}")))
        .collect::<Vec<_>>();
    for path in &denied {
        std::fs::create_dir(path).unwrap();
    }
    let config = launch_isolation_config(home.path(), cwd.path(), &denied).await;
    let paths = (0..32)
        .map(|index| cwd.path().join(format!("readable-{index}.key")))
        .collect::<Vec<_>>();

    // Measure the actual policy cost instead of assuming how profile entries
    // expand. All counter reads and calls stay on this thread, with no await.
    let before = effective_path_normalizations_on_this_thread();
    assert_eq!(verify_isolation(&config, &paths[..1]).len(), 1);
    let singleton_cost = effective_path_normalizations_on_this_thread() - before;
    assert!(singleton_cost > 1, "fixture must normalize policy entries");
    let before = effective_path_normalizations_on_this_thread();
    let blockers = launch_isolation_blockers(&config, &paths, &[]);
    let batch_cost = effective_path_normalizations_on_this_thread() - before;
    assert_eq!(blockers.len(), paths.len());
    assert_eq!(batch_cost, singleton_cost - 1 + paths.len() as u64);
}
