//! Opt-in, bounded synthetic operator fixtures. Never point these at a personal home.
use crate::support::chat_ready::wait_session_configured;
use crate::support::tmux::CommandSpec;
use crate::support::tmux::SessionSpec;
use crate::support::tmux::TerminalSize;
use crate::support::tmux::TmuxKey;
use crate::support::tmux::TmuxPane;
use crate::support::tmux::TmuxServer;
use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use codex_protocol::ThreadId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::protocol::SessionSource;
use serde_json::Value;
use serde_json::json;
use std::fs;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;
use tempfile::tempdir;

#[path = "memory_human_fixture_support.rs"]
mod fixture_support;
use fixture_support::Artifacts;
use fixture_support::fake_provider;
use fixture_support::publish_attachment;
use fixture_support::routing;
use fixture_support::source_failure_reasons;
use fixture_support::write_json;

const CANARY: &str = "PF30S04_SYNTHETIC_ROLLOUT_CANARY";
const FOREGROUND: &str = "HUMAN_FOREGROUND synthetic fixture";
const READY: Duration = Duration::from_secs(45);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Case {
    Startup,
    ProviderSwitch,
    PendingExit,
    Cancel,
    Timeout,
}
#[derive(Clone, Copy, PartialEq)]
enum Driver {
    Human,
    Rehearsal,
}
#[derive(Debug, PartialEq)]
enum Outcome {
    Complete,
    Cancelled,
    TimedOut,
}

#[derive(Clone, Copy)]
struct PendingExitProof {
    elapsed: Option<Duration>,
    window: Duration,
    canaries: usize,
    outputs: usize,
    failed: bool,
}

impl PendingExitProof {
    fn is_pending(self) -> bool {
        // Polling observes the request after it arrives; reserve one second so
        // this cannot extend the fake response's actual pending window.
        self.elapsed
            .is_some_and(|elapsed| elapsed + Duration::from_secs(1) < self.window)
            && self.canaries == 1
            && self.outputs == 0
            && !self.failed
    }
}

#[test]
fn memory_human_pending_exit_rejects_unproven_lifecycle() {
    let valid = PendingExitProof {
        elapsed: Some(Duration::from_secs(1)),
        window: Duration::from_secs(30),
        canaries: 1,
        outputs: 0,
        failed: false,
    };
    assert!(valid.is_pending());
    for invalid in [
        PendingExitProof {
            elapsed: None,
            ..valid
        },
        PendingExitProof {
            elapsed: Some(Duration::from_secs(29)),
            ..valid
        },
        PendingExitProof {
            failed: true,
            ..valid
        },
        PendingExitProof {
            outputs: 1,
            ..valid
        },
        PendingExitProof {
            canaries: 0,
            ..valid
        },
    ] {
        assert!(!invalid.is_pending());
    }
}

#[test]
fn memory_human_pending_exit_only_allows_exact_owner_denial() {
    let source = ThreadId::new();
    let other = ThreadId::new();
    let owner =
        format!("WARN Phase 1 job failed for thread {source}: stage-one memory owner terminated");
    let provider =
        format!("WARN Phase 1 job failed for thread {source}: stage-one memory provider changed");
    let unrelated =
        format!("WARN Phase 1 job failed for thread {source}: synthetic request failure");
    pretty_assertions::assert_eq!(
        source_failure_reasons(&owner, source),
        vec!["owner_terminated"]
    );
    pretty_assertions::assert_eq!(source_failure_reasons(&owner, other), Vec::<&str>::new());
    for (log, expected) in [
        (provider.clone(), vec!["provider_changed"]),
        (unrelated.clone(), vec!["other_source_failure"]),
        (
            format!("{owner}; synthetic failure"),
            vec!["other_source_failure"],
        ),
        (
            format!("{owner}\n{provider}"),
            vec!["owner_terminated", "provider_changed"],
        ),
        (
            format!("{owner}\n{unrelated}"),
            vec!["owner_terminated", "other_source_failure"],
        ),
    ] {
        let reasons = source_failure_reasons(&log, source);
        pretty_assertions::assert_eq!(reasons, expected);
        assert!(reasons.iter().any(|reason| *reason != "owner_terminated"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit operator opt-in; see QA human-fixture guide"]
async fn human_memory_fixture() -> Result<()> {
    ensure!(
        std::env::var("CORBANU_MEMORY_HUMAN_OPT_IN").as_deref() == Ok("1"),
        "explicit opt-in required"
    );
    let case = match std::env::var("CORBANU_MEMORY_HUMAN_CASE")?.as_str() {
        "startup" => Case::Startup,
        "provider-switch" => Case::ProviderSwitch,
        "pending-exit" => Case::PendingExit,
        _ => anyhow::bail!("unknown fixture case"),
    };
    let root = std::path::PathBuf::from(
        std::env::var_os("CORBANU_MEMORY_HUMAN_EVIDENCE")
            .context("new evidence directory required")?,
    );
    fs::create_dir(&root).context("evidence directory must not already exist")?;
    let artifacts = Artifacts::load()?;
    let expected = std::env::var("CORBANU_MEMORY_CANDIDATE_SHA256")?;
    ensure!(
        artifacts.identity["candidate_sha256"] == expected,
        "candidate hash mismatch"
    );
    let result = run_case(
        &artifacts,
        case,
        Driver::Human,
        &root,
        Duration::from_secs(600),
        Duration::from_secs(120),
    )
    .await;
    write_json(
        &root,
        "finished.json",
        &json!({"outcome": format!("{result:?}"), "human_acceptance": false}),
    )?;
    ensure!(
        result? == Outcome::Complete,
        "fixture not completed; leave human check unticked"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_memory_human_fixture_rehearsal() -> Result<()> {
    if !TmuxServer::should_run("human memory rehearsal")? {
        return Ok(());
    }
    let artifacts = Artifacts::load()?;
    for case in [
        Case::Startup,
        Case::ProviderSwitch,
        Case::PendingExit,
        Case::Cancel,
        Case::Timeout,
    ] {
        let root = tempdir()?;
        let limit = if case == Case::Timeout {
            Duration::from_millis(500)
        } else {
            Duration::from_secs(70)
        };
        let result = run_case(
            &artifacts,
            case,
            Driver::Rehearsal,
            root.path(),
            limit,
            Duration::from_secs(30),
        )
        .await;
        save_rehearsal_artifacts(root.path(), case)?;
        let result = result?;
        let expected = match case {
            Case::Cancel => Outcome::Cancelled,
            Case::Timeout => Outcome::TimedOut,
            _ => Outcome::Complete,
        };
        pretty_assertions::assert_eq!(result, expected);
        let ready: Value = serde_json::from_slice(&fs::read(root.path().join("ready.json"))?)?;
        ensure!(
            !Path::new(ready["home"].as_str().unwrap()).exists(),
            "disposable home leaked"
        );
        ensure!(
            !Path::new(ready["socket_dir"].as_str().unwrap()).exists(),
            "private TMUX leaked"
        );
    }
    Ok(())
}

async fn run_case(
    artifacts: &Artifacts,
    case: Case,
    driver: Driver,
    root: &Path,
    limit: Duration,
    delay: Duration,
) -> Result<Outcome> {
    let home = tempdir()?;
    let repo = codex_utils_cargo_bin::repo_root()?;
    let binary = &artifacts.binary;
    let a = fake_provider("A", case, delay).await;
    let b = fake_provider("B", case, delay).await;
    let models = serde_json::to_value(codex_models_manager::bundled_models_response()?)?;
    let template = models["models"]
        .as_array()
        .context("model catalog")?
        .iter()
        .find(|model| model["slug"] == "gpt-5.6-terra")
        .context("fixture metadata template")?;
    let synthetic = ["a", "b"].map(|id| {
        let mut model = template.clone();
        model["slug"] = json!(format!("memory-fixture-{id}"));
        model["display_name"] = json!(format!("Memory Fixture {id} model"));
        model["orchestration"] = json!({"status":"disabled", "provider_id":format!("memory-{id}"),
            "capability":"balanced", "reason":"Synthetic manual fixture; no delegation"});
        model
    });
    write_json(
        home.path(),
        "fixture-models.json",
        &json!({"models":synthetic}),
    )?;
    let config = format!(
        r#"model = "memory-fixture-a"
model_provider = "memory-a"
model_catalog_json = {:?}
cli_auth_credentials_store = "file"
check_for_update_on_startup = false
suppress_unstable_features_warning = true
log_dir = {:?}
[features]
sqlite = true
memories = true
code_mode_host = false
# No curated plugin sync (#1198): its `git fetch` from github.com runs in its
# own process group, outlives a TUI killed at startup (the Timeout case) and
# writes into this home while it is being removed.
plugins = false
[memories]
generate_memories = true
min_rollout_idle_hours = 0
extract_model = "memory-fixture-a"
consolidation_model = "memory-fixture-a"
[security]
version = 1
level = "permissive"
[model_providers.memory-a]
name = "Memory Fixture A"
base_url = "{}/v1"
env_key = "MEMORY_FIXTURE_A_KEY"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
[model_providers.memory-b]
name = "Memory Fixture B"
base_url = "{}/v1"
env_key = "MEMORY_FIXTURE_B_KEY"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
[projects.{}]
trust_level = "trusted"
[tui]
animations = false
"#,
        home.path().join("fixture-models.json"),
        home.path().join("logs"),
        a.uri(),
        b.uri(),
        serde_json::to_string(&repo.display().to_string())?
    );
    fs::write(home.path().join("config.toml"), config)?;
    // This disposable fixture has only two eligible synthetic routes. Keep
    // unrelated built-in providers out of its replacement/model menus.
    let catalog = codex_provider_auth::ProviderCatalog::from_runtime_providers(
        &codex_model_provider_info::built_in_model_providers(/*openai_base_url*/ None),
    );
    let mut eligibility = codex_provider_auth::ProviderEligibility::default();
    for entry in catalog.entries() {
        eligibility.set_policy(
            entry,
            codex_provider_auth::ProviderActivationPolicy::Inactive,
        );
    }
    codex_provider_auth::ProviderEligibilityStore::new(home.path()).save(&eligibility)?;
    fs::write(
        home.path().join("auth.json"),
        r#"{"OPENAI_API_KEY":"synthetic-memory-test","tokens":null,"last_refresh":null}"#,
    )?;
    let db = codex_state::StateRuntime::init(
        codex_state::SqliteConfig::new_for_testing(
            codex_utils_absolute_path::AbsolutePathBuf::try_from(home.path())?,
        ),
        "memory-a".into(),
    )
    .await?;
    db.mark_backfill_complete(/*last_watermark*/ None).await?;
    let source = ThreadId::new();
    let timestamp = chrono::Utc::now() - chrono::Duration::hours(2);
    let rollout = home.path().join(format!("rollout-{source}.jsonl"));
    let line = RolloutLine {
        timestamp: timestamp.to_rfc3339(),
        ordinal: None,
        item: RolloutItem::ResponseItem(ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::InputText {
                text: CANARY.into(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }),
    };
    fs::write(&rollout, format!("{}\n", serde_json::to_string(&line)?))?;
    let mut metadata =
        codex_state::ThreadMetadataBuilder::new(source, rollout, timestamp, SessionSource::Cli);
    metadata.cwd = repo.clone();
    let mut metadata = metadata.build("openai");
    metadata.preview = Some(CANARY.into());
    metadata.first_user_message = Some(CANARY.into());
    db.upsert_thread(&metadata).await?;
    db.set_thread_memory_mode(source, "enabled").await?;
    let tmux = TmuxServer::start("memory-human")?;
    let spec = |name| {
        SessionSpec::new(
            name,
            TerminalSize::new(/*columns*/ 140, /*rows*/ 44),
            CommandSpec::new("env")
                .arg("-i")
                .arg("PATH=/usr/bin:/bin")
                .arg("TERM=xterm-256color")
                .arg(format!("HOME={}", home.path().display()))
                .arg(format!("CODEX_HOME={}", home.path().display()))
                .arg(format!("CORBANU_HOME={}", home.path().display()))
                .arg("CORBANU_TEST_NO_NATIVE_KEYRING=1")
                .arg("MEMORY_FIXTURE_A_KEY=synthetic-a")
                .arg("MEMORY_FIXTURE_B_KEY=synthetic-b")
                .arg("RUST_LOG=trace")
                .arg(binary)
                .arg("--no-alt-screen")
                .arg("-C")
                .arg(&repo),
        )
        .current_dir(&repo)
    };
    let mut session = tmux.new_session(spec("memory-human"))?;
    wait_session_configured(session.primary_pane(), READY)?;
    publish_attachment(&session, root, home.path(), artifacts)?;
    let mut keys = Vec::new();
    let start = Instant::now();
    let mut pending = None;
    let mut pending_exit_elapsed = None;
    let mut switched = false;
    let mut restarted = false;
    if driver == Driver::Rehearsal && !matches!(case, Case::Cancel | Case::Timeout) {
        submit(session.primary_pane(), FOREGROUND, &mut keys)?;
    }
    loop {
        let routes = routing(&a, &b).await;
        let canaries = routes.iter().filter(|r| r["kind"] == "memory").count();
        let outputs = db
            .memories()
            .list_stage1_outputs_for_global(/*n*/ 100)
            .await?
            .iter()
            .filter(|o| o.thread_id == source)
            .count();
        let log = fs::read_to_string(home.path().join("logs/codex-tui.log")).unwrap_or_default();
        if canaries > 0 && pending.is_none() {
            pending = Some(Instant::now());
        }
        let denied = log.contains("stage-one memory provider changed");
        let source_failures = source_failure_reasons(&log, source);
        let foreground_b = routes.iter().any(|r| {
            r["endpoint"] == "B" && r["kind"] == "foreground" && r["model"] == "memory-fixture-b"
        });
        write_json(
            root,
            "status.json",
            &json!({"case":format!("{case:?}"), "source":source.to_string(), "canary_requests":canaries, "source_outputs":outputs,
            "provider_change_denied":denied, "foreground_b":foreground_b, "restarted":restarted,
            "source_failure_reasons":source_failures,
            "pending_exit_elapsed_ms":pending_exit_elapsed,
            "pending_window_remaining_seconds":pending.map(|time| delay.saturating_sub(time.elapsed()).as_secs()), "routes":routes}),
        )?;
        fs::write(root.join("input-events.txt"), keys.join("\n"))?;
        if root.join("cancel").exists() || (driver == Driver::Rehearsal && case == Case::Cancel) {
            return Ok(Outcome::Cancelled);
        }
        if start.elapsed() >= limit {
            return Ok(Outcome::TimedOut);
        }
        if session.is_running() {
            let pane = session.primary_pane();
            match pane.capture_viewport() {
                Ok(view) => fs::write(
                    root.join(if restarted {
                        "restart.txt"
                    } else {
                        "worker.txt"
                    }),
                    view,
                )?,
                Err(_) if !session.is_running() => continue,
                Err(error) => return Err(error),
            }
            if driver == Driver::Rehearsal {
                if case == Case::ProviderSwitch && canaries == 1 && !switched {
                    switch_provider(pane, root, &mut keys)?;
                    switched = true;
                    submit(pane, FOREGROUND, &mut keys)?;
                }
                let exit = match case {
                    Case::Startup => outputs == 1,
                    Case::ProviderSwitch => denied && foreground_b,
                    Case::PendingExit => canaries == 1,
                    Case::Cancel | Case::Timeout => false,
                };
                if exit {
                    pane.wait_stable_contains(
                        if switched {
                            "B foreground complete"
                        } else {
                            "A foreground complete"
                        },
                        READY,
                    )?;
                    submit(pane, "/exit", &mut keys)?;
                    session.wait_for_exit(READY)?;
                }
            }
        } else if case == Case::PendingExit && !restarted {
            if pending_exit_elapsed.is_none() {
                let proof = PendingExitProof {
                    elapsed: pending.map(|time| time.elapsed()),
                    window: delay,
                    canaries,
                    outputs,
                    // Only exact expected owner termination may accompany a
                    // pending owner exit. Mixed/unrelated source failures deny.
                    failed: denied
                        || source_failures
                            .iter()
                            .any(|reason| *reason != "owner_terminated"),
                };
                ensure!(
                    proof.is_pending(),
                    "exit missed pending window or job already failed; not exercised"
                );
                pending_exit_elapsed = proof.elapsed.map(|elapsed| elapsed.as_millis());
                write_json(
                    root,
                    "pending-exit.json",
                    &json!({"elapsed_ms":pending_exit_elapsed,
                    "response_delay_ms":delay.as_millis(), "source":source, "pending_proven":true}),
                )?;
            }
            if driver == Driver::Rehearsal || root.join("restart").exists() {
                session = tmux.new_session(spec("memory-restart"))?;
                restarted = true;
                wait_session_configured(session.primary_pane(), READY)?;
                publish_attachment(&session, root, home.path(), artifacts)?;
                if driver == Driver::Rehearsal {
                    submit(session.primary_pane(), "/status", &mut keys)?;
                    session
                        .primary_pane()
                        .wait_stable_contains("Security:", READY)?;
                    fs::write(
                        root.join("restart.txt"),
                        session.primary_pane().capture_viewport()?,
                    )?;
                    submit(session.primary_pane(), "/exit", &mut keys)?;
                    session.wait_for_exit(READY)?;
                }
            }
        } else {
            let complete = match case {
                Case::Startup => canaries == 1 && outputs == 1,
                Case::ProviderSwitch => canaries == 1 && outputs == 0 && denied && foreground_b,
                Case::PendingExit => restarted && canaries == 1 && outputs == 0,
                Case::Cancel | Case::Timeout => false,
            };
            ensure!(
                complete,
                "operator exited before required job/routing proof; not exercised"
            );
            write_json(
                root,
                "outcome.json",
                &json!({"outcome":"complete", "source_outputs":outputs, "canary_requests":canaries, "human_acceptance":false}),
            )?;
            fs::write(root.join("trace.log"), log)?;
            return Ok(Outcome::Complete);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn submit(pane: &TmuxPane<'_>, text: &str, keys: &mut Vec<String>) -> Result<()> {
    pane.send_literal(text)?;
    keys.push(format!("literal: {text}"));
    pane.wait_stable_contains(text, READY)?;
    pane.send_key(TmuxKey::Enter)?;
    keys.push("key: Enter".into());
    Ok(())
}
fn switch_provider(pane: &TmuxPane<'_>, root: &Path, keys: &mut Vec<String>) -> Result<()> {
    submit(pane, "/providers", keys)?;
    pane.wait_stable_contains("Configure providers and control", READY)?;
    select_label(pane, "Memory Fixture A", keys)?;
    select_label(pane, "Deactivate", keys)?;
    pane.wait_stable_contains("Choose replacement", READY)?;
    select_label(pane, "Memory Fixture B — memory-fixture-a", keys)?;
    pane.wait_stable_contains("Configure providers and control", READY)?;
    fs::write(
        root.join("provider-replacement.txt"),
        pane.capture_viewport()?,
    )?;
    pane.send_key(TmuxKey::Escape)?;
    keys.push("key: Escape (provider replacement complete)".into());
    submit(pane, "/model", keys)?;
    pane.wait_stable_contains("Select Model", READY)?;
    fs::write(root.join("model-picker.txt"), pane.capture_viewport()?)?;
    select_label(pane, "Memory Fixture b model", keys)?;
    pane.wait_stable_contains("Select Reasoning", READY)?;
    pane.send_key(TmuxKey::Enter)?;
    keys.push("key: Enter (default reasoning)".into());
    pane.wait_stable_until("effort selected", READY, |capture| {
        !capture.contains("Select Reasoning")
    })?;
    Ok(())
}

fn select_label(pane: &TmuxPane<'_>, label: &str, keys: &mut Vec<String>) -> Result<()> {
    for _ in 0..64 {
        let capture = pane.capture_viewport()?;
        let selected = capture
            .lines()
            .find(|line| {
                let line = line.trim();
                (line.starts_with('>') || line.starts_with('›'))
                    && line
                        .chars()
                        .skip(1)
                        .find(|c| !c.is_whitespace())
                        .is_some_and(|c| c.is_ascii_digit())
            })
            .unwrap_or("")
            .to_owned();
        if selected.contains(label) {
            pane.send_key(TmuxKey::Enter)?;
            keys.push(format!("key: Enter ({label})"));
            pane.wait_stable_until("selection applied", READY, |next| {
                !next.lines().any(|line| line == selected)
            })?;
            return Ok(());
        }
        pane.send_key(TmuxKey::Down)?;
        keys.push("key: Down".into());
        pane.wait_stable_until("picker selection redraw", Duration::from_secs(5), |next| {
            !next.lines().any(|line| line == selected)
        })?;
    }
    anyhow::bail!(
        "fake provider B absent from model picker: {}",
        pane.capture_viewport()?
    )
}

fn save_rehearsal_artifacts(root: &Path, case: Case) -> Result<()> {
    if let Some(evidence) = std::env::var_os("CORBANU_MEMORY_REHEARSAL_EVIDENCE") {
        let target = std::path::PathBuf::from(evidence).join(format!("{case:?}"));
        fs::create_dir_all(&target)?;
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                fs::copy(entry.path(), target.join(entry.file_name()))?;
            }
        }
    }
    Ok(())
}
