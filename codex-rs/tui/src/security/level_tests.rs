use pretty_assertions::assert_eq;

use super::*;
use crate::legacy_core::security_level_change::SecurityLevel;

fn write_state(home: &Path, contents: &str) {
    std::fs::write(state_path(home), contents).unwrap();
}

#[test]
fn absent_state_is_permissive_and_touches_nothing() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(load(home.path()), StoredLevel::Absent);
    assert_eq!(load(home.path()).enforced(), ChosenLevel::Permissive);
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn unknown_or_corrupt_state_fails_visibly_and_enforces_aggressive() {
    let home = tempfile::tempdir().unwrap();
    for contents in [
        "version = 1\nlevel = \"relaxed\"\n",
        "version = 1\nlevel = \"Moderate\"\n",
        "version = 1\nlevel = \"Permissive\"\n",
        "version = 2\nlevel = \"permissive\"\n",
        "version = 1\nlevel = \"permissive\"\nextra = true\n",
        "level = \"permissive\"\n",
        "not toml",
    ] {
        write_state(home.path(), contents);
        let stored = load(home.path());
        assert!(
            matches!(&stored, StoredLevel::Invalid(reason) if reason.contains(STATE_FILE)),
            "{contents}: {stored:?}"
        );
        assert_eq!(stored.enforced(), ChosenLevel::Aggressive, "{contents}");
    }
}

#[test]
fn aggressive_round_trip_restores_permissive_files_exactly() {
    let home = tempfile::tempdir().unwrap();
    let config = "approval_policy = \"never\"\nsandbox_mode = \"danger-full-access\"\n";
    std::fs::write(home.path().join("config.toml"), config).unwrap();
    std::fs::create_dir_all(home.path().join(RULES_DIR)).unwrap();
    let user_rules = "prefix_rule(pattern=[\"git\", \"push\"], decision=\"allow\")\n";
    std::fs::write(
        home.path().join(RULES_DIR).join("default.rules"),
        user_rules,
    )
    .unwrap();

    save(home.path(), ChosenLevel::Aggressive, NestedAgents::Refuse).unwrap();
    assert_eq!(
        load(home.path()),
        StoredLevel::Chosen(ChosenLevel::Aggressive)
    );
    assert_eq!(
        std::fs::read_to_string(rules_path(home.path())).unwrap(),
        rules_contents()
    );

    save(home.path(), ChosenLevel::Permissive, NestedAgents::Refuse).unwrap();
    assert_eq!(
        load(home.path()),
        StoredLevel::Chosen(ChosenLevel::Permissive)
    );
    // This session stays Aggressive until restart, so its later threads keep
    // the vault rule; the next launch removes it.
    assert!(rules_path(home.path()).exists());
    sync_rules(home.path(), ChosenLevel::Permissive).unwrap();
    let mut rules = std::fs::read_dir(home.path().join(RULES_DIR))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    rules.sort();
    assert_eq!(
        (
            std::fs::read_to_string(home.path().join("config.toml")).unwrap(),
            rules,
            std::fs::read_to_string(home.path().join(RULES_DIR).join("default.rules")).unwrap(),
        ),
        (
            config.to_string(),
            vec!["default.rules".to_string()],
            user_rules.to_string()
        )
    );
}

#[test]
fn rules_forbid_every_vault_entry_point() {
    let rules = rules_contents();
    for program in VAULT_PROGRAMS {
        assert!(
            rules.contains(&format!(
                "pattern = [\"{program}\", \"vault\"], decision = \"forbidden\""
            )),
            "{program}"
        );
    }
}

#[test]
fn status_line_reports_active_and_pending_levels() {
    assert_eq!(
        [
            status_line(
                ChosenLevel::Permissive,
                &StoredLevel::Absent,
                SecurityLevel::Permissive
            ),
            status_line(
                ChosenLevel::Aggressive,
                &StoredLevel::Chosen(ChosenLevel::Aggressive),
                SecurityLevel::Aggressive
            ),
            status_line(
                ChosenLevel::Aggressive,
                &StoredLevel::Chosen(ChosenLevel::Permissive),
                SecurityLevel::Permissive
            ),
            status_line(
                ChosenLevel::Aggressive,
                &StoredLevel::Invalid("x".into()),
                SecurityLevel::Permissive
            ),
        ],
        [
            "Permissive active (/security)".to_string(),
            "Aggressive active (/security)".to_string(),
            "Aggressive active; Permissive saved for next start (/security)".to_string(),
            "Aggressive active; stored level unreadable, Aggressive enforced (/security)"
                .to_string(),
        ]
    );
}

#[test]
fn deleted_state_file_next_to_the_rule_file_is_invalid() {
    let home = tempfile::tempdir().unwrap();
    save(home.path(), ChosenLevel::Aggressive, NestedAgents::Refuse).unwrap();
    std::fs::remove_file(state_path(home.path())).unwrap();
    assert_eq!(load(home.path()).enforced(), ChosenLevel::Aggressive);
    assert!(matches!(load(home.path()), StoredLevel::Invalid(_)));
}

#[test]
fn nested_agents_setting_round_trips_and_fails_closed() {
    let home = tempfile::tempdir().unwrap();
    save(home.path(), ChosenLevel::Aggressive, NestedAgents::Refuse).unwrap();
    assert_eq!(
        std::fs::read_to_string(state_path(home.path())).unwrap(),
        "version = 1\nlevel = \"aggressive\"\n"
    );
    save(home.path(), ChosenLevel::Aggressive, NestedAgents::Pass).unwrap();
    assert_eq!(
        load_state(home.path()),
        (
            StoredLevel::Chosen(ChosenLevel::Aggressive),
            NestedAgents::Pass
        )
    );
    // Saving Permissive resets it.
    save(home.path(), ChosenLevel::Permissive, NestedAgents::Pass).unwrap();
    assert_eq!(
        std::fs::read_to_string(state_path(home.path())).unwrap(),
        "version = 1\nlevel = \"permissive\"\n"
    );
    write_state(
        home.path(),
        "version = 1\nlevel = \"aggressive\"\nnested_agents = \"allow\"\n",
    );
    let (stored, nested) = load_state(home.path());
    assert!(
        matches!(&stored, StoredLevel::Invalid(reason) if reason.contains("nested_agents")),
        "{stored:?}"
    );
    assert_eq!(
        (stored.enforced(), nested),
        (ChosenLevel::Aggressive, NestedAgents::Refuse)
    );
}

#[test]
fn claude_panes_are_refused_while_a_protected_level_is_active_or_saved() {
    use ChosenLevel::Aggressive;
    use ChosenLevel::Permissive;
    assert_eq!(
        external_agent_block_reason_in(Permissive, Permissive, Permissive),
        None
    );
    for (active, stored, live) in [
        (Aggressive, Aggressive, Permissive),
        (Aggressive, Permissive, Permissive),
        (Permissive, Aggressive, Permissive),
        // #1468: Core's live level counts too.
        (Permissive, Permissive, Aggressive),
    ] {
        let reason = external_agent_block_reason_in(active, stored, live)
            .expect("a protected level refuses Claude panes");
        assert!(
            reason.starts_with("Claude panes are off under security level Aggressive")
                && reason.contains("outside Corbanu's sandbox")
                && reason.contains("/security"),
            "{reason}"
        );
    }
}

/// #1468 (from #807's review): a level in force only in Core's live policy
/// refuses Claude panes and permission changes as a saved or active one
/// does; Moderate refuses panes but leaves `/permissions` alone.
#[test]
fn issue_1468_core_live_level_counts_for_panes_and_permission_changes() {
    use ChosenLevel::Aggressive;
    use ChosenLevel::Moderate;
    use ChosenLevel::Permissive;
    let reason = external_agent_block_reason_in(Permissive, Permissive, Moderate)
        .expect("live Moderate refuses Claude panes");
    assert!(
        reason.starts_with("Claude panes are off under security level Moderate"),
        "{reason}"
    );
    // The message names the strictest level.
    let reason = external_agent_block_reason_in(Moderate, Permissive, Aggressive)
        .expect("a protected level refuses Claude panes");
    assert!(
        reason.starts_with("Claude panes are off under security level Aggressive"),
        "{reason}"
    );
    {
        let _levels = test_levels::set_with_live(Permissive, Permissive, Aggressive);
        assert!(external_agent_block_reason().is_some());
    }
    let _levels = test_levels::set(Permissive, Permissive);
    assert_eq!(external_agent_block_reason(), None);

    assert_eq!(
        permission_change_block_reason_in([Permissive, Permissive, Permissive]),
        None
    );
    assert_eq!(
        permission_change_block_reason_in([Permissive, Permissive, Moderate]),
        None
    );
    for levels in [
        [Aggressive, Permissive, Permissive],
        [Permissive, Aggressive, Permissive],
        [Permissive, Permissive, Aggressive],
    ] {
        assert!(
            permission_change_block_reason_in(levels)
                .is_some_and(|reason| reason.starts_with("Security level Aggressive manages")),
            "{levels:?}"
        );
    }
}

/// #218: the refusal lifts only with `contained_external_agents` on, the
/// secretless launch contract armed, and a platform sandbox.
#[test]
fn contained_panes_lift_the_refusal_only_when_containment_is_ready() {
    use crate::claude_panes::containment::ContainmentSettings;
    use crate::claude_panes::containment::test_settings;
    let _levels = test_levels::set(ChosenLevel::Aggressive, ChosenLevel::Aggressive);
    let feature = |enabled| {
        test_settings::set(ContainmentSettings {
            enabled,
            linux_sandbox_exe: None,
            state_root: None,
        })
    };
    for (enabled, armed) in [(false, false), (false, true), (true, false)] {
        let _feature = feature(enabled);
        let _armed = test_settings::set_contract_armed(armed);
        assert!(
            external_agent_block_reason().is_some(),
            "feature {enabled}, contract armed {armed}"
        );
    }
    let _feature = feature(true);
    let _armed = test_settings::set_contract_armed(/*armed*/ true);
    let has_sandbox =
        codex_sandboxing::get_platform_sandbox(/*windows_sandbox_enabled*/ false).is_some();
    assert_eq!(external_agent_block_reason().is_none(), has_sandbox);
}

#[test]
fn smoke_runs_without_a_launch_context_read_the_stored_level() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(external_agent_block_reason_for_home(home.path()), None);
    save(home.path(), ChosenLevel::Aggressive, NestedAgents::Refuse).unwrap();
    assert!(external_agent_block_reason_for_home(home.path()).is_some());
}

/// #441: the rule file follows the nested-launch setting, and both forms are
/// recognised as Corbanu's own.
#[test]
fn issue_441_rules_follow_the_nested_agents_setting() {
    let home = tempfile::tempdir().unwrap();
    save(home.path(), ChosenLevel::Aggressive, NestedAgents::Pass).unwrap();
    let pass = std::fs::read_to_string(rules_path(home.path())).unwrap();
    assert!(!pass.contains("\"exec\""), "{pass}");
    save(home.path(), ChosenLevel::Aggressive, NestedAgents::Refuse).unwrap();
    let refuse = std::fs::read_to_string(rules_path(home.path())).unwrap();
    assert_eq!(refuse, rules_contents());
    for program in VAULT_PROGRAMS {
        for subcommand in ["exec", "e", "review"] {
            assert!(
                refuse.contains(&format!(
                    "pattern = [\"{program}\", \"{subcommand}\"], decision = \"forbidden\""
                )),
                "{program} {subcommand}"
            );
        }
    }
    // Saving Permissive leaves the file to the next launch; a session still
    // enforcing Aggressive recognises it either way.
    save(home.path(), ChosenLevel::Aggressive, NestedAgents::Pass).unwrap();
    save(home.path(), ChosenLevel::Permissive, NestedAgents::Refuse).unwrap();
    let kept = std::fs::read_to_string(rules_path(home.path())).unwrap();
    assert_eq!(kept, pass);
    assert!(codex_security_level::level::is_rules_contents(&kept));
    // The vault-only file earlier versions wrote is the "pass" form.
    assert!(!pass.contains("nested agent launches"), "{pass}");
    assert!(codex_security_level::level::is_rules_contents(&pass));
    assert!(codex_security_level::level::is_rules_contents(&refuse));
    assert!(!codex_security_level::level::is_rules_contents("# mine\n"));
}

/// #750: Moderate that Core keeps (config, or Core's record over a
/// Permissive file) is the next start's level, not "Permissive saved".
#[test]
fn issue_750_status_line_counts_the_moderate_core_keeps() {
    assert_eq!(
        [
            status_line(
                ChosenLevel::Moderate,
                &StoredLevel::Absent,
                SecurityLevel::Moderate
            ),
            status_line(
                ChosenLevel::Moderate,
                &StoredLevel::Chosen(ChosenLevel::Permissive),
                SecurityLevel::Permissive
            ),
            status_line(
                ChosenLevel::Permissive,
                &StoredLevel::Chosen(ChosenLevel::Moderate),
                SecurityLevel::Moderate
            ),
        ],
        [
            "Moderate active (/security)".to_string(),
            "Moderate active; Permissive saved for next start (/security)".to_string(),
            "Permissive active; Moderate saved for next start (/security)".to_string(),
        ]
    );
}
