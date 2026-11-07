use crate::support::chat_ready::wait_session_configured;
use crate::support::tmux::CommandSpec;
use crate::support::tmux::SessionSpec;
use crate::support::tmux::TerminalSize;
use crate::support::tmux::TmuxKey;
use crate::support::tmux::TmuxPane;
use crate::support::tmux::TmuxServer;
use anyhow::Result;
use anyhow::ensure;
use codex_protocol::ThreadId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::protocol::SessionSource;
use core_test_support::responses;
use std::fs;
use std::time::Duration;
use tempfile::tempdir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;

const CANARY: &str = "PF30S04_SYNTHETIC_ROLLOUT_CANARY";
const TIMEOUT: Duration = Duration::from_secs(45);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tmux_memory_worker_policy_canary_permissive_and_protected() -> Result<()> {
    if !TmuxServer::should_run("PF30S04 memory dispatch")? {
        return Ok(());
    }
    let repo = codex_utils_cargo_bin::repo_root()?;
    let binary = codex_utils_cargo_bin::cargo_bin("codex")?;
    for (level, cancel_pending) in [
        ("permissive", false),
        ("moderate", false),
        ("aggressive", false),
        ("permissive", true),
    ] {
        let case = if cancel_pending {
            "cancel-pending"
        } else {
            level
        };
        let mut input_events = Vec::new();
        let home = tempdir()?;
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::method("POST"))
            .respond_with(move |request: &wiremock::Request| {
                let body = String::from_utf8_lossy(&request.body);
                let text = if body.contains(CANARY) {
                    r#"{"raw_memory":"synthetic extracted result","rollout_summary":"synthetic summary","rollout_slug":"fixture"}"#
                } else { "foreground fixture complete" };
                let response = ResponseTemplate::new(200).insert_header("content-type", "text/event-stream")
                    .set_body_string(responses::sse(vec![responses::ev_response_created("fixture"),
                        responses::ev_assistant_message("fixture-message", text), responses::ev_completed("fixture")]));
                if cancel_pending && body.contains(CANARY) { response.set_delay(Duration::from_secs(30)) } else { response }
            }).mount(&server).await;
        let config = format!(
            "model = \"gpt-5.4\"\nmodel_provider = \"openai\"\nopenai_base_url = \"{}/v1\"\ncli_auth_credentials_store = \"file\"\ncheck_for_update_on_startup = false\nsuppress_unstable_features_warning = true\nlog_dir = {:?}\n[features]\nsqlite = true\nmemories = true\nbroker_model_auth = false\n[memories]\ngenerate_memories = true\nmin_rollout_idle_hours = 0\n[security]\nversion = 1\nlevel = \"{level}\"\n[projects.{}]\ntrust_level = \"trusted\"\n[tui]\nanimations = false\n",
            server.uri(),
            home.path().join("logs"),
            serde_json::to_string(&repo.display().to_string())?
        );
        fs::write(home.path().join("config.toml"), config)?;
        fs::write(
            home.path().join("auth.json"),
            r#"{"OPENAI_API_KEY":"synthetic-memory-test","tokens":null,"last_refresh":null}"#,
        )?;
        let db = codex_state::StateRuntime::init(
            codex_state::SqliteConfig::new_for_testing(
                codex_utils_absolute_path::AbsolutePathBuf::try_from(home.path())?,
            ),
            "openai".into(),
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
        // Startup scans deliberately exclude empty previews, just like real
        // persisted conversations. The canary must be an eligible source.
        metadata.preview = Some(CANARY.into());
        metadata.first_user_message = Some(CANARY.into());
        db.upsert_thread(&metadata).await?;
        db.set_thread_memory_mode(source, "enabled").await?;
        let tmux = TmuxServer::start(&format!("memory_policy_{case}"))?;
        tmux.register_artifact("codex-tui.log", home.path().join("logs/codex-tui.log"));
        let session = tmux.new_session(
            SessionSpec::new(
                level,
                TerminalSize::new(/*columns*/ 120, /*rows*/ 42),
                CommandSpec::new(&binary)
                    .env("CODEX_HOME", home.path())
                    .env("CORBANU_HOME", home.path())
                    .env("OPENAI_API_KEY", "synthetic-memory-test")
                    .env("RUST_LOG", "trace")
                    .arg("--no-alt-screen")
                    .arg("-C")
                    .arg(&repo),
            )
            .current_dir(&repo),
        )?;
        let pane = session.primary_pane();
        pane.wait_stable_contains("Corbanu Terminal", TIMEOUT)?;
        submit(
            pane,
            "Run the synthetic foreground fixture.",
            &mut input_events,
        )?;
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            let outputs = db
                .memories()
                .list_stage1_outputs_for_global(/*n*/ 10)
                .await?;
            let log =
                fs::read_to_string(home.path().join("logs/codex-tui.log")).unwrap_or_default();
            let done = if cancel_pending {
                server
                    .received_requests()
                    .await
                    .unwrap()
                    .iter()
                    .any(|r| String::from_utf8_lossy(&r.body).contains(CANARY))
            } else if level == "permissive" {
                outputs.iter().any(|out| out.thread_id == source)
            } else {
                log.contains("protected stage-one memory input is unavailable")
            };
            if done {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                let viewport = pane.capture_viewport()?;
                let retained = home.keep();
                anyhow::bail!(
                    "memory worker did not finish {case}; retained fixture: {}; viewport:\n{viewport}",
                    retained.display()
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let requests = server.received_requests().await.unwrap();
        let canaries = requests
            .iter()
            .filter(|r| String::from_utf8_lossy(&r.body).contains(CANARY))
            .count();
        ensure!(
            canaries == usize::from(level == "permissive"),
            "unexpected canary dispatch count {canaries} for {level}"
        );
        if level != "permissive" {
            ensure!(
                db.memories()
                    .list_stage1_outputs_for_global(/*n*/ 10)
                    .await?
                    .is_empty(),
                "denied output persisted"
            );
        }
        if let Some(root) = std::env::var_os("CORBANU_MEMORY_EVIDENCE") {
            let root = std::path::PathBuf::from(root);
            fs::create_dir_all(&root)?;
            fs::write(
                root.join(format!("{case}-worker.txt")),
                pane.capture_viewport()?,
            )?;
            fs::write(
                root.join(format!("{case}-counts.txt")),
                format!("canary_requests={canaries}\n"),
            )?;
        }
        if level == "permissive" {
            pane.wait_stable_contains("foreground fixture complete", TIMEOUT)?;
        }
        submit(pane, "/exit", &mut input_events)?;
        session.wait_for_exit(TIMEOUT)?;
        if cancel_pending {
            ensure!(
                db.memories()
                    .list_stage1_outputs_for_global(/*n*/ 10)
                    .await?
                    .is_empty(),
                "cancelled memory output persisted"
            );
        }
        let restarted = tmux.new_session(
            SessionSpec::new(
                "restart",
                TerminalSize::new(/*columns*/ 120, /*rows*/ 42),
                CommandSpec::new(&binary)
                    .env("CODEX_HOME", home.path())
                    .env("CORBANU_HOME", home.path())
                    .env("OPENAI_API_KEY", "synthetic-memory-test")
                    .env("RUST_LOG", "trace")
                    .arg("--no-alt-screen")
                    .arg("-C")
                    .arg(&repo),
            )
            .current_dir(&repo),
        )?;
        let pane = restarted.primary_pane();
        wait_session_configured(pane, TIMEOUT)?;
        submit(pane, "/status", &mut input_events)?;
        pane.wait_stable_contains("Security:", TIMEOUT)?;
        let after_restart = server.received_requests().await.unwrap();
        ensure!(
            after_restart
                .iter()
                .filter(|r| String::from_utf8_lossy(&r.body).contains(CANARY))
                .count()
                == canaries,
            "restart unexpectedly dispatched a raw rollout"
        );
        if let Some(root) = std::env::var_os("CORBANU_MEMORY_EVIDENCE") {
            let root = std::path::PathBuf::from(root);
            fs::write(
                root.join(format!("{case}-restart.txt")),
                pane.capture_viewport()?,
            )?;
        }
        submit(pane, "/exit", &mut input_events)?;
        restarted.wait_for_exit(TIMEOUT)?;
        if let Some(root) = std::env::var_os("CORBANU_MEMORY_EVIDENCE") {
            let root = std::path::PathBuf::from(root);
            fs::write(
                root.join(format!("{case}-input-events.txt")),
                input_events.join("\n"),
            )?;
            fs::write(
                root.join(format!("{case}-outcomes.txt")),
                format!(
                    "canary_requests={canaries}\nstage1_outputs={}\n",
                    db.memories()
                        .list_stage1_outputs_for_global(/*n*/ 10)
                        .await?
                        .len()
                ),
            )?;
            fs::copy(
                home.path().join("logs/codex-tui.log"),
                root.join(format!("{case}-trace.log")),
            )?;
        }
    }
    Ok(())
}

fn submit(pane: &TmuxPane<'_>, text: &str, events: &mut Vec<String>) -> Result<()> {
    pane.send_literal(text)?;
    events.push(format!("literal: {text}"));
    pane.wait_stable_contains(text, TIMEOUT)?;
    pane.send_key(TmuxKey::Enter)?;
    events.push("key: Enter".into());
    Ok(())
}
