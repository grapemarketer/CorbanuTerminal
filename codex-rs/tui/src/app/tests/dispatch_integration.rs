use super::*;

use core_test_support::responses::ResponseMock;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call_with_namespace;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_response_sequence;
use core_test_support::responses::mount_sse_repeating;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::sse_completed;
use wiremock::MockServer;
use wiremock::ResponseTemplate;

/// D106 (#1176): an assignment's birth brief for a Manager in a pane the
/// person opened goes to that pane's mailbox, as for an agent-opened pane.
#[tokio::test]
async fn operator_assignment_birth_brief_routes_through_the_mailbox() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(
        &app,
        "operator-birth-brief",
        "Manager must coordinate the operator-owned Worker pane.",
    );
    let manager_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000463").expect("manager id");
    let worker_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000464").expect("worker id");
    app.upsert_agent_picker_thread(
        manager_thread_id,
        Some("Manager".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        worker_thread_id,
        Some("Worker".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    let manager_node = crate::spawn_orchestration::thread_node_id(manager_thread_id);
    let worker_node = crate::spawn_orchestration::thread_node_id(worker_thread_id);

    app.handle_orchestrate_command(format!(
        "attach {worker_node} operator-birth-brief --mode review --holder {manager_node} --for 1h"
    ));

    let mut manager_tasks = Vec::new();
    while let Ok(event) = app_event_rx.try_recv() {
        match event {
            AppEvent::SubmitSpawnAgentTask {
                thread_id, task, ..
            } if thread_id == manager_thread_id => {
                manager_tasks.push(task);
            }
            AppEvent::CodexOp(_) | AppEvent::SubmitThreadOp { .. } => {
                panic!("an agent's task must never reach the Manager as the person's input");
            }
            _ => {}
        }
    }
    pretty_assertions::assert_eq!(manager_tasks.len(), 1);
    assert!(manager_tasks[0].contains("Worker"));
}

#[tokio::test]
async fn native_manager_host_adapter_dispatches_only_from_completed_assignment_turn() {
    let (mut app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    write_test_whip(
        &app,
        "native-manager-dispatch",
        "Manager must dispatch the implementation to Worker.",
    );
    let manager_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000465").expect("manager id");
    let worker_thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000466").expect("worker id");
    app.upsert_agent_picker_thread(
        manager_thread_id,
        Some("Manager".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    app.upsert_agent_picker_thread(
        worker_thread_id,
        Some("Worker".to_string()),
        /*agent_role*/ None,
        /*is_closed*/ false,
    );
    let manager_node = crate::spawn_orchestration::thread_node_id(manager_thread_id);
    let worker_node = crate::spawn_orchestration::thread_node_id(worker_thread_id);
    app.handle_orchestrate_command(format!(
        "attach {worker_node} native-manager-dispatch --mode review --holder {manager_node} --for 1h"
    ));
    while app_event_rx.try_recv().is_ok() {}

    let dispatch = format!(
        "```pfterminal-send-task\n{{\"target\":\"{worker_node}\",\"task\":\"Review the branch and report concrete defects.\"}}\n```"
    );
    let ServerNotification::ItemCompleted(mut injected_instruction) = item_completed_notification(
        manager_thread_id,
        "manager-dispatch-1",
        "injected-manager-brief",
        &dispatch,
    ) else {
        unreachable!("helper returns ItemCompleted");
    };
    let ThreadItem::AgentMessage { phase, .. } = &mut injected_instruction.item else {
        unreachable!("helper returns AgentMessage");
    };
    *phase = Some(codex_protocol::models::MessagePhase::Commentary);
    app.enqueue_thread_notification(
        manager_thread_id,
        ServerNotification::ItemCompleted(injected_instruction),
    )
    .await
    .expect("enqueue injected Manager instruction");
    assert!(
        drain_spawn_agent_task_for(&mut app_event_rx, worker_thread_id).is_none(),
        "an injected inter-agent instruction must never execute its example host block"
    );
    assert!(matches!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|whip| &whip.kind),
        Some(crate::orchestrate::WhipKind::Assignment {
            phase: crate::orchestrate::AssignmentPhase::Drafting,
            ..
        })
    ));

    app.enqueue_thread_notification(
        manager_thread_id,
        item_completed_notification(
            manager_thread_id,
            "manager-dispatch-1",
            "agent-message-1",
            &dispatch,
        ),
    )
    .await
    .expect("enqueue inactive Manager message completion");
    assert!(
        drain_spawn_agent_task_for(&mut app_event_rx, worker_thread_id).is_none(),
        "native Manager output is display text, never an inter-agent transport"
    );

    let completed = turn_completed_with_agent_message(
        manager_thread_id,
        "manager-dispatch-1",
        TurnStatus::Completed,
        &dispatch,
    );
    app.enqueue_thread_notification(manager_thread_id, completed.clone())
        .await
        .expect("enqueue inactive Manager completion");
    let worker_task = drain_spawn_agent_task_for(&mut app_event_rx, worker_thread_id)
        .expect("completed Manager output should dispatch through the assignment host adapter");
    assert!(worker_task.contains("Review the branch and report concrete defects."));
    assert!(matches!(
        app.orchestrate_whips
            .get("assignment-1")
            .map(|whip| &whip.kind),
        Some(crate::orchestrate::WhipKind::Assignment {
            phase: crate::orchestrate::AssignmentPhase::Executing,
            ..
        })
    ));

    app.enqueue_thread_notification(manager_thread_id, completed)
        .await
        .expect("replay inactive Manager completion");
    assert!(
        drain_spawn_agent_task_for(&mut app_event_rx, worker_thread_id).is_none(),
        "terminal notification replay must not dispatch the same host task twice"
    );
}

struct RealDispatchFixture {
    app: App,
    app_events: tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    server: AppServerSession,
    tui: crate::tui::Tui,
    root: ThreadId,
}

impl RealDispatchFixture {
    async fn start(mock: &MockServer, max_threads: usize) -> Result<Self> {
        let (mut app, app_events, _op_rx) = make_test_app_with_channels().await;
        if let Ok(crash_home) = std::env::var("PFTERMINAL_DISPATCH_CRASH_HOME") {
            app.config.codex_home = codex_utils_absolute_path::AbsolutePathBuf::try_from(
                std::path::PathBuf::from(crash_home),
            )?;
        }
        let mut provider = app.config.model_provider.clone();
        provider.name = "Dispatch integration mock".to_string();
        provider.base_url = Some(format!("{}/v1", mock.uri()));
        provider.env_key = None;
        provider.experimental_bearer_token = None;
        provider.supports_websockets = false;
        provider.requires_openai_auth = false;
        provider.request_max_retries = Some(0);
        provider.stream_max_retries = Some(0);
        app.config.model = Some("dispatch-mock-model".to_string());
        app.config.model_provider_id = "dispatch_mock".to_string();
        app.config.model_provider = provider.clone();
        app.config
            .model_providers
            .insert(app.config.model_provider_id.clone(), provider);
        std::fs::write(
            app.config.codex_home.join("config.toml"),
            format!(
                r#"model = "dispatch-mock-model"
model_provider = "dispatch_mock"

[model_providers.dispatch_mock]
name = "Dispatch integration mock"
base_url = "{}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
supports_websockets = false
"#,
                mock.uri()
            ),
        )?;
        app.config.multi_agent_v2.max_concurrent_threads_per_session = max_threads;
        if let Some(env_key) = app.config.model_provider.env_key.as_deref() {
            std::fs::write(
                app.config.codex_home.join("provider_auth.json"),
                format!(r#"{{"api_keys":{{"{env_key}":"qualification-key"}}}}"#),
            )?;
        }
        let mut server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
        let root = server.start_thread(&app.config).await?;
        let root_id = root.session.thread_id;
        app.primary_thread_id = Some(root_id);
        app.active_thread_id = Some(root_id);
        app.primary_session_configured = Some(root.session);
        let tui = crate::tui::Tui::for_dispatch_integration_test()?;
        Ok(Self {
            app,
            app_events,
            server,
            tui,
            root: root_id,
        })
    }

    async fn spawn_agent_under(
        &mut self,
        nickname: &str,
        role: &str,
        parent: ThreadId,
    ) -> Result<ThreadId> {
        let config = self.app.native_spawn_agent_config()?;
        let started = self
            .server
            .spawn_agent_thread(
                &config,
                parent,
                role.to_string(),
                Some(nickname.to_string()),
                self.app
                    .config
                    .model
                    .clone()
                    .unwrap_or_else(|| "gpt-5".to_string()),
                Some(self.app.config.model_provider_id.clone()),
                self.app.config.model_reasoning_effort.clone(),
                /*base_instructions*/ None,
            )
            .await?;
        let thread_id = started.session.thread_id;
        self.app
            .register_spawn_agent_pane(
                thread_id,
                parent,
                crate::spawn_orchestration::thread_node_id(parent),
                Some(nickname.to_string()),
                role,
                started,
                /*persist_layout*/ false,
            )
            .await;
        Ok(thread_id)
    }

    async fn spawn_agent(&mut self, nickname: &str, role: &str) -> Result<ThreadId> {
        self.spawn_agent_under(nickname, role, self.root).await
    }

    async fn spawn_target(&mut self, nickname: &str) -> Result<ThreadId> {
        self.spawn_agent(nickname, "orc").await
    }

    async fn route_once(&mut self) -> Result<()> {
        while let Ok(event) = self.app_events.try_recv() {
            self.app
                .handle_event(&mut self.tui, &mut self.server, event)
                .await?;
        }
        if let Ok(Some(event)) = tokio::time::timeout(
            std::time::Duration::from_millis(10),
            self.server.next_event(),
        )
        .await
        {
            self.app.handle_app_server_event(&self.server, event).await;
        }
        tokio::task::yield_now().await;
        Ok(())
    }

    async fn route_until(
        &mut self,
        timeout: std::time::Duration,
        mut done: impl FnMut(&App) -> bool,
    ) -> Result<()> {
        tokio::time::timeout(timeout, async {
            while !done(&self.app) {
                self.route_once().await?;
            }
            Ok::<_, color_eyre::Report>(())
        })
        .await
        .map_err(|_| color_eyre::eyre::eyre!("real dispatch event path timed out"))??;
        Ok(())
    }
}

fn pending_agent_message_text(request: &core_test_support::responses::ResponsesRequest) -> String {
    let mut messages = Vec::new();
    for item in request.input().into_iter().rev() {
        if item.get("type").and_then(serde_json::Value::as_str) == Some("message")
            && item.get("role").and_then(serde_json::Value::as_str) == Some("assistant")
        {
            break;
        }

        let item_type = item.get("type").and_then(serde_json::Value::as_str);
        let is_native_mail = item_type == Some("agent_message");
        let is_adapted_mail = item_type == Some("message")
            && item.get("role").and_then(serde_json::Value::as_str) == Some("user")
            && item
                .get("content")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|content| {
                    content.iter().any(|span| {
                        span.get("text")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|text| text.contains("<inter_agent_message "))
                    })
                });
        if !is_native_mail && !is_adapted_mail {
            continue;
        }
        let text = item
            .get("content")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|content| content.get("text").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            messages.push(text);
        }
    }
    messages.reverse();
    messages.join("\n")
}

fn run_dispatch_integration<F, Fut>(test: F) -> Result<()>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + 'static,
{
    std::thread::Builder::new()
        .name("dispatch-integration".to_string())
        .stack_size(32 * 1024 * 1024)
        .spawn(move || -> Result<()> {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(32 * 1024 * 1024)
                .enable_all()
                .build()?;
            tokio::task::LocalSet::new().block_on(&runtime, test())
        })?
        .join()
        .map_err(|_| color_eyre::eyre::eyre!("dispatch integration thread panicked"))?
}

#[test]
fn duplicate_nazgul_creation_is_rejected_before_spawning_a_thread() -> Result<()> {
    run_dispatch_integration(|| async {
        let mock = MockServer::start().await;
        let mut fixture = RealDispatchFixture::start(&mock, /*max_threads*/ 4).await?;
        fixture.app.ensure_custom_spawn_root(CODEX_MAIN_PANE_ID)?;
        fixture
            .app
            .set_spawn_nazgul_pane_binding(CODEX_MAIN_PANE_ID.to_string());

        let channels_before = fixture.app.thread_event_channels.len();
        let navigation_before = fixture.app.agent_navigation.ordered_threads().len();
        let spawn_edges_before = fixture.app.spawn_parent_by_thread.len();
        fixture
            .app
            .handle_event(
                &mut fixture.tui,
                &mut fixture.server,
                AppEvent::CreateSpawnAgent {
                    role: crate::spawn_orchestration::SpawnRole::Nazgul,
                    parent_node_id: None,
                    agent_nickname: Some("Duplicate".to_string()),
                    model: "dispatch-mock-model".to_string(),
                    provider: Some("dispatch_mock".to_string()),
                    effort: None,
                },
            )
            .await?;

        pretty_assertions::assert_eq!(fixture.app.thread_event_channels.len(), channels_before);
        pretty_assertions::assert_eq!(
            fixture.app.agent_navigation.ordered_threads().len(),
            navigation_before
        );
        pretty_assertions::assert_eq!(fixture.app.spawn_parent_by_thread.len(), spawn_edges_before);
        pretty_assertions::assert_eq!(
            fixture.app.spawn_nazgul_pane_id.as_deref(),
            Some(CODEX_MAIN_PANE_ID)
        );
        fixture.server.shutdown().await?;
        Ok(())
    })
}

#[test]
fn direct_orc_task_reaches_provider_through_core_mailbox_without_prompt_wrapper() -> Result<()> {
    run_dispatch_integration(|| async {
        let mock = MockServer::start().await;
        let responses = mount_sse_repeating(&mock, sse_completed("orc-direct-response")).await;
        let mut fixture = RealDispatchFixture::start(&mock, /*max_threads*/ 3).await?;
        let target = fixture.spawn_target("Snaga").await?;
        fixture
            .app
            .app_event_tx
            .send(AppEvent::SubmitSpawnAgentTask {
                thread_id: target,
                task: "run the direct acceptance command".to_string(),
                sent_while_kill_switch_on: false,
            });

        fixture
            .route_until(std::time::Duration::from_secs(20), |app| {
                !responses.requests().is_empty()
                    && app
                        .agent_navigation
                        .get(&target)
                        .is_some_and(|entry| !entry.is_running)
            })
            .await?;

        let presented = responses
            .requests()
            .iter()
            .map(pending_agent_message_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(presented.contains("run the direct acceptance command"));
        assert!(!presented.contains("<pfterminal_spawn_orc_task_context>"));
        fixture.server.shutdown().await?;
        Ok(())
    })
}

/// Whether `item` is a user message whose text is exactly `text`.
fn is_user_text(item: &serde_json::Value, text: &str) -> bool {
    item.get("type").and_then(serde_json::Value::as_str) == Some("message")
        && item.get("role").and_then(serde_json::Value::as_str) == Some("user")
        && item
            .get("content")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|content| {
                content
                    .iter()
                    .any(|span| span.get("text").and_then(serde_json::Value::as_str) == Some(text))
            })
}

/// The developer messages of `request` (settings such as approval and
/// sandbox are given to the model there), without the TUI's per-turn spawn
/// context, which a typed turn carries too.
fn developer_texts(request: &core_test_support::responses::ResponsesRequest) -> Vec<String> {
    request
        .input()
        .into_iter()
        .filter(|item| item.get("role").and_then(serde_json::Value::as_str) == Some("developer"))
        .map(|item| item.to_string())
        .filter(|text| !text.contains("<pfterminal_spawn_context>"))
        .collect()
}

/// D95 (#1146): a task the person types in Spawn status's "Send task to"
/// prompt reaches the Orc as the person's own input, a plain user message as
/// if typed in its pane, with no inter-agent wrapper. A task another agent
/// sends the same Orc is unchanged: it still arrives as an inter-agent
/// message (which source envelopes label untrusted). The person's task sends
/// no settings: even with the TUI's own approval policy and the pane's cached
/// settings changed, the Orc's settings (its developer messages) stay as
/// they were.
#[test]
fn spawn_status_person_task_reaches_child_as_user_input() -> Result<()> {
    run_dispatch_integration(|| async {
        let mock = MockServer::start().await;
        let responses = mount_sse_repeating(&mock, sse_completed("orc-person-response")).await;
        let mut fixture = RealDispatchFixture::start(&mock, /*max_threads*/ 3).await?;
        let target = fixture.spawn_target("Snaga").await?;
        let idle = move |app: &App| {
            app.agent_navigation
                .get(&target)
                .is_some_and(|entry| !entry.is_running)
        };

        // A task from an agent: unchanged.
        let agent_task = "agent relayed task 1146";
        fixture
            .app
            .app_event_tx
            .send(AppEvent::SubmitSpawnAgentTask {
                thread_id: target,
                task: agent_task.to_string(),
                sent_while_kill_switch_on: false,
            });
        fixture
            .route_until(std::time::Duration::from_secs(20), |app| {
                !responses.requests().is_empty() && idle(app)
            })
            .await?;
        let agent_request = responses.requests().into_iter().last().expect("request");
        let items = agent_request.input();
        let serialized = serde_json::to_string(&items)?;
        assert!(
            pending_agent_message_text(&agent_request).contains(agent_task),
            "{serialized}"
        );
        assert!(
            !items.iter().any(|item| is_user_text(item, agent_task)),
            "{serialized}"
        );
        assert!(
            serialized.contains(&format!(
                "<inter_agent_message sender=\\\"/root\\\" recipient=\\\"/root/orc_snaga\\\">\\n{agent_task}\\n</inter_agent_message>"
            )),
            "{serialized}"
        );

        // Settings the person's task must not send: the TUI's own approval
        // policy and the pane's cached session.
        fixture.app.config.permissions.approval_policy = codex_config::Constrained::allow_any(
            codex_app_server_protocol::AskForApproval::Never.to_core(),
        );
        if let Some(channel) = fixture.app.thread_event_channels.get(&target) {
            let mut store = channel.store.lock().await;
            if let Some(session) = store.session.as_mut() {
                session.approval_policy = codex_app_server_protocol::AskForApproval::Never;
                session.permission_profile = codex_protocol::models::PermissionProfile::Disabled;
                session.active_permission_profile = None;
                session.model = "not-the-orc-model".to_string();
            }
        }

        // What the prompt does with the text the person typed.
        let person_task = "run the person's acceptance command 1146";
        let before = responses.requests().len();
        (crate::spawn_orchestration::spawn_agent_task_prompt_submit(
            fixture.app.app_event_tx.clone(),
            target,
        ))(format!("  {person_task}\n"));
        fixture
            .route_until(std::time::Duration::from_secs(20), |app| {
                responses.requests().len() > before && idle(app)
            })
            .await?;
        let person_request = responses.requests().into_iter().last().expect("request");
        let items = person_request.input();
        let serialized = serde_json::to_string(&items)?;
        assert!(
            items.iter().any(|item| is_user_text(item, person_task)),
            "{serialized}"
        );
        assert!(
            !pending_agent_message_text(&person_request).contains(person_task),
            "{serialized}"
        );
        assert!(
            !serialized.contains(&format!("{person_task}\\n</inter_agent_message>")),
            "{serialized}"
        );
        assert!(
            !serialized.contains("corbanu_untrusted_data"),
            "{serialized}"
        );
        // No settings changed: the same developer messages as before, and the
        // Orc's own model.
        pretty_assertions::assert_eq!(
            developer_texts(&person_request),
            developer_texts(&agent_request),
            "{serialized}"
        );
        pretty_assertions::assert_eq!(
            person_request.body_json()["model"],
            agent_request.body_json()["model"]
        );
        fixture.server.shutdown().await?;
        Ok(())
    })
}

/// D95 review: a pane its parent controls takes no typed input, so a task the
/// person sends it from Spawn status is not made their input: it goes as
/// before, as an inter-agent message.
#[test]
fn spawn_status_person_task_to_parent_owned_pane_stays_a_message() -> Result<()> {
    run_dispatch_integration(|| async {
        let mock = MockServer::start().await;
        let responses = mount_sse_repeating(&mock, sse_completed("orc-owned-response")).await;
        let mut fixture = RealDispatchFixture::start(&mock, /*max_threads*/ 3).await?;
        let target = fixture.spawn_target("Snaga").await?;
        fixture.app.agent_navigation.mark_parent_owned(target);

        let task = "task for a parent-owned pane 1146";
        (crate::spawn_orchestration::spawn_agent_task_prompt_submit(
            fixture.app.app_event_tx.clone(),
            target,
        ))(task.to_string());
        fixture
            .route_until(std::time::Duration::from_secs(20), |app| {
                !responses.requests().is_empty()
                    && app
                        .agent_navigation
                        .get(&target)
                        .is_some_and(|entry| !entry.is_running)
            })
            .await?;
        let request = responses.requests().into_iter().last().expect("request");
        let items = request.input();
        let serialized = serde_json::to_string(&items)?;
        assert!(
            pending_agent_message_text(&request).contains(task),
            "{serialized}"
        );
        assert!(
            !items.iter().any(|item| is_user_text(item, task)),
            "{serialized}"
        );
        fixture.server.shutdown().await?;
        Ok(())
    })
}

/// Creates a pane as the person does in `/panes` and returns its thread,
/// leaving Main selected.
async fn create_person_opened_pane(fixture: &mut RealDispatchFixture) -> Result<ThreadId> {
    fixture
        .app
        .handle_event(
            &mut fixture.tui,
            &mut fixture.server,
            AppEvent::CreateCodexPane {
                model: "dispatch-worker-model".to_string(),
                provider: Some("dispatch_mock".to_string()),
                effort: None,
                display_name: Some("Worker".to_string()),
            },
        )
        .await?;
    let target = fixture
        .app
        .active_thread_id
        .expect("created operator pane is selected");
    assert_ne!(target, fixture.root);
    let entry = fixture
        .app
        .agent_navigation
        .get(&target)
        .expect("pane entry")
        .clone();
    assert!(
        fixture
            .app
            .is_operator_owned_codex_user_pane(target, &entry)
    );
    fixture.app.active_thread_id = Some(fixture.root);
    Ok(target)
}

/// D106 (#1176): an agent's task for a pane the person opened in `/panes`
/// reaches it as an inter-agent message from its mailbox (labelled untrusted
/// data), not as the person's input, and runs on that pane's own model. A
/// task the person sends the same pane from Spawn status is still their own
/// input (D95).
#[test]
fn agent_task_to_person_opened_pane_arrives_as_mailbox_data() -> Result<()> {
    run_dispatch_integration(|| async {
        let mock = MockServer::start().await;
        let responses = mount_sse_repeating(&mock, sse_completed("operator-pane-response")).await;
        let mut fixture = RealDispatchFixture::start(&mock, /*max_threads*/ 3).await?;
        let target = create_person_opened_pane(&mut fixture).await?;
        let idle = move |app: &App| {
            app.agent_navigation
                .get(&target)
                .is_some_and(|entry| !entry.is_running)
        };

        // The route agents' tasks take (assignment fires, model dispatch).
        // A /spawn crew that is read-only does not stop it: the pane is not
        // part of that crew.
        fixture.app.spawn_legacy_read_only = true;
        let agent_task = "agent task for a person-opened pane 1176";
        fixture
            .app
            .send_native_task_event(target, agent_task.to_string());
        fixture
            .route_until(std::time::Duration::from_secs(20), |app| {
                !responses.requests().is_empty() && idle(app)
            })
            .await?;
        let requests = responses.requests();
        pretty_assertions::assert_eq!(requests.len(), 1);
        let agent_request = &requests[0];
        let items = agent_request.input();
        let serialized = serde_json::to_string(&items)?;
        assert!(
            pending_agent_message_text(agent_request).contains(agent_task),
            "{serialized}"
        );
        assert!(
            !items.iter().any(|item| is_user_text(item, agent_task)),
            "{serialized}"
        );
        assert!(
            !agent_request
                .message_input_texts("user")
                .iter()
                .any(|text| text == agent_task),
            "{serialized}"
        );
        // #1248: the sender's `/root` is Main's tree, not the pane's own; the
        // recipient is told which tree sent it.
        let root = fixture.root;
        assert!(
            serialized.contains(&format!(
                "<inter_agent_message sender=\\\"/root\\\" recipient=\\\"/root\\\">\\nSender: agent /root in another pane's agent tree (thread {root}), not this pane's /root\\n{agent_task}\\n</inter_agent_message>"
            )),
            "{serialized}"
        );
        pretty_assertions::assert_eq!(
            agent_request
                .body_json()
                .get("model")
                .and_then(serde_json::Value::as_str),
            Some("dispatch-worker-model")
        );

        fixture.app.spawn_legacy_read_only = false;

        // The person's own task from Spawn status: unchanged, their input.
        let person_task = "person task for a person-opened pane 1176";
        (crate::spawn_orchestration::spawn_agent_task_prompt_submit(
            fixture.app.app_event_tx.clone(),
            target,
        ))(person_task.to_string());
        fixture
            .route_until(std::time::Duration::from_secs(20), |app| {
                responses.requests().len() > 1 && idle(app)
            })
            .await?;
        let person_request = responses.requests().into_iter().last().expect("request");
        let items = person_request.input();
        let serialized = serde_json::to_string(&items)?;
        assert!(
            items.iter().any(|item| is_user_text(item, person_task)),
            "{serialized}"
        );
        assert!(
            !pending_agent_message_text(&person_request).contains(person_task),
            "{serialized}"
        );
        fixture.server.shutdown().await?;
        Ok(())
    })
}

#[test]
fn cold_restored_orc_task_is_readmitted_through_core_mailbox() -> Result<()> {
    run_dispatch_integration(|| async {
        let mock = MockServer::start().await;
        let responses = mount_sse_repeating(&mock, sse_completed("cold-restored-response")).await;
        let mut fixture = RealDispatchFixture::start(&mock, /*max_threads*/ 3).await?;
        let target = fixture.spawn_target("Snaga").await?;
        let root_seed = AppCommand::user_turn(
            vec![codex_app_server_protocol::UserInput::Text {
                text: "seed the root rollout before restart".to_string(),
                text_elements: Vec::new(),
            }],
            fixture.app.config.cwd.to_path_buf(),
            codex_app_server_protocol::AskForApproval::Never,
            /*active_permission_profile*/ None,
            fixture.app.config.model.clone().expect("fixture model"),
            fixture.app.config.model_reasoning_effort.clone(),
            fixture.app.config.model_reasoning_summary,
            /*service_tier*/ None,
            /*final_output_json_schema*/ None,
            /*collaboration_mode*/ None,
            fixture.app.config.personality,
        );
        fixture
            .app
            .submit_thread_op(&mut fixture.server, fixture.root, root_seed)
            .await?;
        fixture
            .route_until(std::time::Duration::from_secs(20), |_app| {
                !responses.requests().is_empty()
            })
            .await?;
        for _ in 0..10 {
            fixture.route_once().await?;
        }
        let requests_after_root_seed = responses.requests().len();
        fixture
            .app
            .app_event_tx
            .send(AppEvent::SubmitSpawnAgentTask {
                thread_id: target,
                task: "seed the child rollout before restart".to_string(),
                sent_while_kill_switch_on: false,
            });
        fixture
            .route_until(std::time::Duration::from_secs(20), |app| {
                responses.requests().len() > requests_after_root_seed
                    && app
                        .agent_navigation
                        .get(&target)
                        .is_some_and(|entry| !entry.is_running)
            })
            .await?;
        let requests_before_restart = responses.requests().len();

        fixture.server.shutdown().await?;
        let mut restarted = Box::pin(crate::start_embedded_app_server_for_picker(
            &fixture.app.config,
        ))
        .await?;
        let root = restarted
            .resume_thread(
                fixture.app.config.clone(),
                fixture.root,
                crate::app_server_session::ResumeModelSettings::RestoreFromThread,
                crate::app_server_session::ResumePermissionSettings::RestoreFromThread,
            )
            .await?;
        fixture.app.primary_session_configured = Some(root.session);
        let restored_target = restarted
            .resume_thread(
                fixture.app.config.clone(),
                target,
                crate::app_server_session::ResumeModelSettings::RestoreFromThread,
                crate::app_server_session::ResumePermissionSettings::RestoreFromThread,
            )
            .await?;
        let channel = fixture.app.ensure_thread_channel(target);
        channel.mark_live();
        channel
            .set_session(restored_target.session, restored_target.turns)
            .await;
        fixture.server = restarted;

        fixture
            .app
            .app_event_tx
            .send(AppEvent::SubmitSpawnAgentTask {
                thread_id: target,
                task: "run the cold-restored acceptance command".to_string(),
                sent_while_kill_switch_on: false,
            });
        fixture
            .route_until(std::time::Duration::from_secs(20), |app| {
                responses
                    .requests()
                    .into_iter()
                    .skip(requests_before_restart)
                    .any(|request| {
                        pending_agent_message_text(&request)
                            .contains("run the cold-restored acceptance command")
                    })
                    && app
                        .agent_navigation
                        .get(&target)
                        .is_some_and(|entry| !entry.is_running)
            })
            .await?;

        let presented = responses
            .requests()
            .into_iter()
            .skip(requests_before_restart)
            .map(|request| pending_agent_message_text(&request))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(presented.contains("run the cold-restored acceptance command"));
        assert!(!presented.contains("<pfterminal_spawn_orc_task_context>"));
        assert!(
            !fixture.app.spawn_processed_dispatch_origins.is_empty(),
            "Core mailbox admission must commit dispatch identity"
        );
        fixture.server.shutdown().await?;
        Ok(())
    })
}

fn fifo_delivery_complete(
    app: &App,
    target: ThreadId,
    responses: &ResponseMock,
    tasks: &[&str],
) -> bool {
    let presented = responses
        .requests()
        .iter()
        .map(pending_agent_message_text)
        .collect::<Vec<_>>();
    // Idle can be observed between coalesced turns. Delivery is complete only
    // when every expected task has also crossed a provider boundary.
    !presented.is_empty()
        && tasks
            .iter()
            .all(|task| presented.iter().any(|body| body.contains(task)))
        && app
            .agent_navigation
            .get(&target)
            .is_some_and(|entry| !entry.is_running)
}

#[test]
#[allow(
    clippy::print_stdout,
    reason = "Capture controlled task-loss and recovery evidence in the test output"
)]
fn fifo_completion_rejects_a_dropped_task_and_waits_for_late_delivery() -> Result<()> {
    run_dispatch_integration(|| async {
        let mock = MockServer::start().await;
        let responses = mount_sse_repeating(&mock, sse_completed("fifo-regression-response")).await;
        let mut fixture = RealDispatchFixture::start(&mock, /*max_threads*/ 4).await?;
        let target = fixture.spawn_target("FIFO regression target").await?;
        let tasks = ["first controlled FIFO task", "second controlled FIFO task"];
        fixture
            .app
            .app_event_tx
            .send(AppEvent::SubmitSpawnAgentTask {
                thread_id: target,
                task: tasks[0].to_string(),
                sent_while_kill_switch_on: false,
            });
        fixture
            .route_until(std::time::Duration::from_secs(20), |app| {
                fifo_delivery_complete(app, target, &responses, &tasks[..1])
            })
            .await?;

        // Deliberately lose one queued assignment before mailbox admission.
        // The first turn is idle, but the expected work is still incomplete.
        fixture
            .app
            .app_event_tx
            .send(AppEvent::SubmitSpawnAgentTask {
                thread_id: target,
                task: tasks[1].to_string(),
                sent_while_kill_switch_on: false,
            });
        let mut dropped = 0;
        while let Ok(event) = fixture.app_events.try_recv() {
            match event {
                AppEvent::SubmitSpawnAgentTask {
                    thread_id, task, ..
                } if thread_id == target && task == tasks[1] => {
                    dropped += 1;
                }
                event => {
                    fixture
                        .app
                        .handle_event(&mut fixture.tui, &mut fixture.server, event)
                        .await?;
                }
            }
        }
        pretty_assertions::assert_eq!(dropped, 1);
        pretty_assertions::assert_eq!(responses.requests().len(), 1);
        let missing = fixture
            .route_until(std::time::Duration::from_millis(100), |app| {
                fifo_delivery_complete(app, target, &responses, &tasks)
            })
            .await;
        pretty_assertions::assert_eq!(
            missing
                .expect_err("a lost mailbox task must not be accepted as completed")
                .to_string(),
            "real dispatch event path timed out"
        );
        println!("FIFO lost-task control: one task dropped; incomplete delivery timed out");

        // A late delivery must still be routed, not mistaken for the earlier
        // turn's idle state. Re-send the deliberately dropped task.
        fixture
            .app
            .app_event_tx
            .send(AppEvent::SubmitSpawnAgentTask {
                thread_id: target,
                task: tasks[1].to_string(),
                sent_while_kill_switch_on: false,
            });
        fixture
            .route_until(std::time::Duration::from_secs(20), |app| {
                fifo_delivery_complete(app, target, &responses, &tasks)
            })
            .await?;
        let presented = responses
            .requests()
            .iter()
            .map(pending_agent_message_text)
            .collect::<Vec<_>>();
        let boundaries = tasks.map(|task| presented.iter().position(|body| body.contains(task)));
        pretty_assertions::assert_eq!(boundaries, [Some(0), Some(1)]);
        println!("FIFO late-delivery control: provider boundaries {boundaries:?}");
        fixture.server.shutdown().await?;
        Ok(())
    })
}

#[test]
fn real_event_path_preserves_fifo_when_mailbox_coalesces_turns() -> Result<()> {
    run_dispatch_integration(|| async {
        let mock = MockServer::start().await;
        let responses = mount_sse_repeating(&mock, sse_completed("dispatch-response")).await;
        let mut fixture = RealDispatchFixture::start(&mock, /*max_threads*/ 4).await?;
        let target = fixture.spawn_target("FIFO target").await?;
        let tasks = [
            "arbitrary punctuation: []{} <> & unicode λ",
            "second task\nwith a newline and no protocol markers",
            "third task: exact-once/FIFO? yes!",
        ];
        for task in tasks {
            fixture
                .app
                .app_event_tx
                .send(AppEvent::SubmitSpawnAgentTask {
                    thread_id: target,
                    task: task.to_string(),
                    sent_while_kill_switch_on: false,
                });
        }

        fixture
            .route_until(std::time::Duration::from_secs(20), |app| {
                fifo_delivery_complete(app, target, &responses, &tasks)
            })
            .await?;

        let requests = responses.requests();
        assert!(
            (1..=tasks.len()).contains(&requests.len()),
            "mailbox coalescing must use between one and one request per task"
        );
        let presented = requests
            .iter()
            .map(pending_agent_message_text)
            .collect::<Vec<_>>();
        let first_request_by_task = tasks
            .iter()
            .map(|task| {
                presented
                    .iter()
                    .position(|body| body.contains(task))
                    .expect("every mailbox task must reach a provider boundary")
            })
            .collect::<Vec<_>>();
        pretty_assertions::assert_eq!(first_request_by_task.first(), Some(&0));
        pretty_assertions::assert_eq!(
            first_request_by_task.last(),
            Some(&(requests.len().saturating_sub(1)))
        );
        assert!(
            first_request_by_task
                .windows(2)
                .all(|pair| pair[0] <= pair[1]),
            "mailbox tasks crossed provider request boundaries out of order"
        );
        for (index, pair) in first_request_by_task.windows(2).enumerate() {
            if pair[0] == pair[1] {
                let earlier = presented[pair[0]].find(tasks[index]).expect("earlier task");
                let later = presented[pair[1]]
                    .find(tasks[index + 1])
                    .expect("later task");
                assert!(
                    earlier < later,
                    "coalesced mailbox work must retain FIFO order"
                );
            }
        }
        fixture.server.shutdown().await?;
        Ok(())
    })
}

#[test]
fn native_assistant_text_replay_never_becomes_mailbox_work() -> Result<()> {
    run_dispatch_integration(|| async {
        let mock = MockServer::start().await;
        let mut fixture = RealDispatchFixture::start(&mock, /*max_threads*/ 4).await?;
        let source = fixture.spawn_agent("ReplayManager", "troll").await?;
        let target = fixture.spawn_target("ReplayTarget").await?;
        let task = "native assistant text must never enter the mailbox";
        let notification = turn_completed_with_agent_message(
            source,
            "completed-source-turn",
            TurnStatus::Completed,
            &format!(
                "<pfterminal_send_task target=\"ReplayTarget\">\n{task}\n</pfterminal_send_task>"
            ),
        );
        fixture
            .app
            .update_spawn_status_for_thread_notification(&notification);
        fixture
            .app
            .update_spawn_status_for_thread_notification(&notification);

        for _ in 0..10 {
            fixture.route_once().await?;
        }
        assert!(
            mock.received_requests()
                .await
                .unwrap_or_default()
                .is_empty(),
            "native assistant prose must not be parsed as an assignment"
        );
        pretty_assertions::assert_eq!(
            fixture.app.spawn_processed_terminal_turns.len(),
            1,
            "the completed source turn must have one durable replay marker"
        );
        assert!(fixture.app.agent_navigation.get(&target).is_some());
        fixture.server.shutdown().await?;
        Ok(())
    })
}

fn delayed_sse_response(id: &str, delay: std::time::Duration) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(sse_completed(id))
        .set_delay(delay)
}

#[test]
fn three_real_turns_saturate_and_one_release_schedules_one_followup() -> Result<()> {
    run_dispatch_integration(|| async {
        let mock = MockServer::start().await;
        let responses = mount_response_sequence(
            &mock,
            vec![
                delayed_sse_response("capacity-1", std::time::Duration::from_millis(700)),
                delayed_sse_response("capacity-2", std::time::Duration::from_millis(1800)),
                delayed_sse_response("capacity-3", std::time::Duration::from_millis(1800)),
                delayed_sse_response("capacity-4", std::time::Duration::from_millis(120)),
                delayed_sse_response("capacity-5", std::time::Duration::from_millis(120)),
                delayed_sse_response("capacity-6", std::time::Duration::from_millis(120)),
            ],
        )
        .await;
        // Four includes the root, leaving exactly three child execution slots.
        let mut fixture = RealDispatchFixture::start(&mock, /*max_threads*/ 4).await?;
        let targets = [
            fixture.spawn_target("CapacityA").await?,
            fixture.spawn_target("CapacityB").await?,
            fixture.spawn_target("CapacityC").await?,
        ];
        for (index, target) in targets.iter().enumerate() {
            fixture
                .app
                .app_event_tx
                .send(AppEvent::SubmitSpawnAgentTask {
                    thread_id: *target,
                    task: format!("capacity-target-{index}-initial"),
                    sent_while_kill_switch_on: false,
                });
        }

        fixture
            .route_until(std::time::Duration::from_secs(5), |app| {
                responses.requests().len() == 3
                    && targets.iter().all(|target| {
                        app.agent_navigation
                            .get(target)
                            .is_some_and(|entry| entry.is_running)
                    })
            })
            .await?;
        for (index, target) in targets.iter().enumerate() {
            fixture
                .app
                .app_event_tx
                .send(AppEvent::SubmitSpawnAgentTask {
                    thread_id: *target,
                    task: format!("capacity-target-{index}-followup"),
                    sent_while_kill_switch_on: false,
                });
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        for _ in 0..5 {
            fixture.route_once().await?;
        }
        pretty_assertions::assert_eq!(
            responses.requests().len(),
            3,
            "queued follow-ups must not start while all three core slots are active"
        );

        fixture
            .route_until(std::time::Duration::from_secs(2), |_| {
                responses.requests().len() == 4
            })
            .await?;
        let first_four = responses
            .requests()
            .iter()
            .map(pending_agent_message_text)
            .collect::<Vec<_>>();
        pretty_assertions::assert_eq!(
            first_four
                .iter()
                .filter(|text| text.contains("followup"))
                .count(),
            1,
            "one released execution slot must schedule exactly one queued target"
        );

        fixture
            .route_until(std::time::Duration::from_secs(8), |_| {
                responses.requests().len() == 6
            })
            .await?;
        pretty_assertions::assert_eq!(responses.requests().len(), 6);
        fixture.server.shutdown().await?;
        Ok(())
    })
}

#[test]
fn mailbox_delivery_wakes_waiting_target_without_turn_start_fallback() -> Result<()> {
    run_dispatch_integration(|| async {
        let mock = MockServer::start().await;
        let mut fixture = RealDispatchFixture::start(&mock, /*max_threads*/ 4).await?;
        let target = fixture.spawn_agent("WaitingManager", "troll").await?;
        let _child = fixture
            .spawn_agent_under("WaitingChild", "orc", target)
            .await?;
        let responses = mount_sse_sequence(
            &mock,
            vec![
                sse(vec![
                    ev_response_created("wait-response"),
                    ev_function_call_with_namespace(
                        "wait-call",
                        "collaboration",
                        "wait_agent",
                        r#"{"timeout_ms":10000}"#,
                    ),
                    ev_completed("wait-response"),
                ]),
                sse_completed("steered-response"),
            ],
        )
        .await;
        let initial_task = "enter a real wait until more work arrives";
        fixture
            .app
            .app_event_tx
            .send(AppEvent::SubmitSpawnAgentTask {
                thread_id: target,
                task: initial_task.to_string(),
                sent_while_kill_switch_on: false,
            });
        fixture
            .route_until(std::time::Duration::from_secs(10), |app| {
                responses.requests().len() == 1
                    && app.spawn_waiting_for_agents_by_thread.contains_key(&target)
            })
            .await
            .wrap_err("waiting target never entered the real wait state")?;

        let steer_task = "continue from the wait using this mailbox task";
        fixture
            .app
            .app_event_tx
            .send(AppEvent::SubmitSpawnAgentTask {
                thread_id: target,
                task: steer_task.to_string(),
                sent_while_kill_switch_on: false,
            });
        fixture
            .route_until(std::time::Duration::from_secs(10), |_| {
                responses.requests().len() == 2
            })
            .await
            .wrap_err("mailbox delivery did not wake the waiting target")?;

        let requests = responses.requests();
        pretty_assertions::assert_eq!(
            requests.len(),
            2,
            "mailbox delivery must wake the existing wait without duplicate provider turns"
        );
        assert!(requests[0].body_json().to_string().contains(initial_task));
        assert!(!requests[0].body_json().to_string().contains(steer_task));
        assert!(
            requests[1].body_json().to_string().contains(steer_task),
            "the mailbox task must appear in the follow-up provider request; users={:?}",
            requests[1].message_input_texts("user")
        );
        assert!(
            fixture.app.spawn_processed_dispatch_origins.len() >= 2,
            "canonical mailbox admissions must retain their stable source origins"
        );
        fixture.server.shutdown().await?;
        Ok(())
    })
}

#[test]
fn queue_bound_rejection_is_visible_and_accepts_nothing() -> Result<()> {
    run_dispatch_integration(|| async {
        let mock = MockServer::start().await;
        let mut fixture = RealDispatchFixture::start(&mock, /*max_threads*/ 3).await?;
        let target = fixture.spawn_target("BoundedTarget").await?;
        fixture
            .app
            .app_event_tx
            .send(AppEvent::SubmitSpawnAgentTask {
                thread_id: target,
                task: "x".repeat(crate::dispatch_queue::MAX_DISPATCH_TASK_BYTES + 1),
                sent_while_kill_switch_on: false,
            });
        for _ in 0..20 {
            fixture.route_once().await?;
        }

        assert!(
            mock.received_requests()
                .await
                .unwrap_or_default()
                .is_empty()
        );
        let rendered = fixture
            .app
            .transcript_cells
            .iter()
            .map(|cell| lines_to_single_string(&cell.display_lines(/*width*/ 120)))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains("maximum") && rendered.contains("Could not admit task"),
            "bound rejection must be visible to the source; rendered={rendered:?}"
        );
        fixture.server.shutdown().await?;
        Ok(())
    })
}

#[test]
fn low_context_agent_compacts_and_continues_real_dispatch() -> Result<()> {
    run_dispatch_integration(|| async {
        let mock = MockServer::start().await;
        let responses = mount_sse_sequence(
            &mock,
            vec![
                sse_completed("before-compact"),
                sse(vec![
                    ev_response_created("compact-response"),
                    ev_assistant_message("compact-message", "durable compact summary"),
                    ev_completed("compact-response"),
                ]),
                sse_completed("after-compact"),
            ],
        )
        .await;
        let mut fixture = RealDispatchFixture::start(&mock, /*max_threads*/ 3).await?;
        let target = fixture.spawn_agent("CompactManager", "troll").await?;
        let before = "dispatch before real compaction";
        fixture
            .app
            .app_event_tx
            .send(AppEvent::SubmitSpawnAgentTask {
                thread_id: target,
                task: before.to_string(),
                sent_while_kill_switch_on: false,
            });
        fixture
            .route_until(std::time::Duration::from_secs(10), |_| {
                responses.requests().len() == 1
            })
            .await?;

        fixture
            .app
            .handle_app_server_event(
                &fixture.server,
                codex_app_server_client::AppServerEvent::ServerNotification(Box::new(
                    token_usage_notification_with_total(
                        target,
                        "pressure-turn",
                        /*total_tokens*/ 99_000,
                        Some(100_000),
                    ),
                )),
            )
            .await;
        assert!(
            fixture
                .app
                .spawn_context_left_by_thread
                .get(&target)
                .is_some_and(|left| *left <= 1)
        );

        fixture.server.thread_compact_start(target).await?;
        fixture
            .route_until(std::time::Duration::from_secs(10), |app| {
                responses.requests().len() == 2
                    && app
                        .agent_navigation
                        .get(&target)
                        .is_some_and(|entry| !entry.is_running)
            })
            .await?;

        let after = "dispatch after real compaction";
        fixture
            .app
            .app_event_tx
            .send(AppEvent::SubmitSpawnAgentTask {
                thread_id: target,
                task: after.to_string(),
                sent_while_kill_switch_on: false,
            });
        fixture
            .route_until(std::time::Duration::from_secs(10), |_| {
                responses.requests().len() == 3
            })
            .await?;

        let requests = responses.requests();
        assert!(pending_agent_message_text(&requests[0]).contains(before));
        assert!(pending_agent_message_text(&requests[2]).contains(after));
        pretty_assertions::assert_eq!(requests.len(), 3);
        fixture.server.shutdown().await?;
        Ok(())
    })
}

/// #660: routes events for `duration`, whatever happens.
async fn route_for(fixture: &mut RealDispatchFixture, duration: std::time::Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + duration;
    while tokio::time::Instant::now() < deadline {
        fixture.route_once().await?;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    Ok(())
}

/// #660: agent send paths stamp whether the switch was on when they sent.
#[tokio::test]
async fn issue_660_agent_sends_stamp_the_kill_switch() {
    use crate::legacy_core::security_revocation::HumanRevocation;
    let (app, mut app_event_rx, _op_rx) = make_test_app_with_channels().await;
    let thread_id =
        ThreadId::from_string("00000000-0000-0000-0000-000000000660").expect("thread id");
    let stamps = |rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>| {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|event| match event {
                AppEvent::SubmitSpawnAgentTask {
                    sent_while_kill_switch_on,
                    ..
                } => Some(sent_while_kill_switch_on),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    while app_event_rx.try_recv().is_ok() {}
    super::issue_507_switch(&app, HumanRevocation::KillSwitchOn);
    app.send_native_task_event(thread_id, "pf660 while on".to_string());
    pretty_assertions::assert_eq!(stamps(&mut app_event_rx), vec![true]);
    super::issue_507_switch(&app, HumanRevocation::KillSwitchOff);
    app.send_native_task_event(thread_id, "pf660 while off".to_string());
    pretty_assertions::assert_eq!(stamps(&mut app_event_rx), vec![false]);
}

/// #660: a task another agent sent to a managed Codex pane while the switch
/// was on, handled only after it was turned off, waits in the mailbox for the
/// pane's next turn instead of starting one (nothing restarts after
/// switch-off, #507).
#[test]
fn issue_660_agent_task_sent_while_on_and_handled_after_off_waits() -> Result<()> {
    run_dispatch_integration(|| async {
        use crate::legacy_core::security_revocation::HumanRevocation;
        let mock = MockServer::start().await;
        let responses = mount_sse_repeating(&mock, sse_completed("pf660-response")).await;
        let mut fixture = RealDispatchFixture::start(&mock, /*max_threads*/ 3).await?;
        let target = fixture.spawn_target("Snaga").await?;
        super::issue_507_switch(&fixture.app, HumanRevocation::KillSwitchOn);
        let sent_while_kill_switch_on = fixture.app.kill_switch_on_now();
        assert!(sent_while_kill_switch_on);
        super::issue_507_switch(&fixture.app, HumanRevocation::KillSwitchOff);
        fixture
            .app
            .app_event_tx
            .send(AppEvent::SubmitSpawnAgentTask {
                thread_id: target,
                task: "pf660 queued".to_string(),
                sent_while_kill_switch_on,
            });
        route_for(&mut fixture, std::time::Duration::from_secs(2)).await?;
        assert!(responses.requests().is_empty(), "no turn started");
        assert!(
            fixture
                .app
                .agent_navigation
                .get(&target)
                .is_some_and(|entry| !entry.is_running)
        );

        // Control: a task sent after switch-off starts the pane's turn, and
        // the waiting task goes with it.
        fixture
            .app
            .app_event_tx
            .send(AppEvent::SubmitSpawnAgentTask {
                thread_id: target,
                task: "pf660 after".to_string(),
                sent_while_kill_switch_on: false,
            });
        fixture
            .route_until(std::time::Duration::from_secs(20), |_| {
                !responses.requests().is_empty()
            })
            .await?;
        let presented = responses
            .requests()
            .iter()
            .map(|request| request.body_json().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(presented.contains("pf660 queued"), "{presented}");
        assert!(presented.contains("pf660 after"), "{presented}");
        fixture.server.shutdown().await?;
        Ok(())
    })
}

/// #660 follow-up: the transcript text added since `from` cells.
fn transcript_since(app: &App, from: usize) -> String {
    app.transcript_cells
        .iter()
        .skip(from)
        .map(|cell| lines_to_single_string(&cell.display_lines(/*width*/ 400)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// #660 follow-up (Travis, 2026-10-13): when the switch is turned off, a
/// managed Codex pane whose task waits is named in the same "Nothing
/// restarted" notice Claude panes get. Once the task went with the pane's
/// next turn, the pane is no longer named.
#[test]
fn issue_660_switch_off_names_a_codex_pane_whose_task_waits() -> Result<()> {
    run_dispatch_integration(|| async {
        use crate::legacy_core::security_revocation::HumanRevocation;
        let mock = MockServer::start().await;
        let responses = mount_sse_repeating(&mock, sse_completed("pf660n-response")).await;
        let mut fixture = RealDispatchFixture::start(&mock, /*max_threads*/ 3).await?;
        let target = fixture.spawn_target("Snaga").await?;
        let label = fixture.app.thread_label(target);

        super::issue_507_switch(&fixture.app, HumanRevocation::KillSwitchOn);
        fixture
            .app
            .on_kill_switch_changed(/*on*/ true, /*elsewhere*/ false);
        fixture
            .app
            .app_event_tx
            .send(AppEvent::SubmitSpawnAgentTask {
                thread_id: target,
                task: "pf660 waits".to_string(),
                sent_while_kill_switch_on: fixture.app.kill_switch_on_now(),
            });
        fixture
            .route_until(std::time::Duration::from_secs(20), |app| {
                transcript_since(app, /*from*/ 0).contains("waits: the security kill switch is on")
            })
            .await?;
        assert!(responses.requests().is_empty(), "no turn started");

        let from = fixture.app.transcript_cells.len();
        super::issue_507_switch(&fixture.app, HumanRevocation::KillSwitchOff);
        fixture
            .app
            .on_kill_switch_changed(/*on*/ false, /*elsewhere*/ false);
        route_for(&mut fixture, std::time::Duration::from_millis(200)).await?;
        let notice = transcript_since(&fixture.app, from);
        assert!(
            notice.contains("Nothing restarted") && notice.contains(&format!("`{label}`")),
            "{notice}"
        );
        assert!(
            responses.requests().is_empty(),
            "switch-off started nothing"
        );

        // The person's next task starts the pane's turn; the waiting task
        // goes with it, so the next switch-off names nothing.
        fixture
            .app
            .app_event_tx
            .send(AppEvent::SubmitSpawnAgentTask {
                thread_id: target,
                task: "pf660 next".to_string(),
                sent_while_kill_switch_on: false,
            });
        fixture
            .route_until(std::time::Duration::from_secs(20), |app| {
                !responses.requests().is_empty()
                    && app
                        .agent_navigation
                        .get(&target)
                        .is_some_and(|entry| !entry.is_running)
            })
            .await?;
        super::issue_507_switch(&fixture.app, HumanRevocation::KillSwitchOn);
        fixture
            .app
            .on_kill_switch_changed(/*on*/ true, /*elsewhere*/ false);
        route_for(&mut fixture, std::time::Duration::from_millis(200)).await?;
        let from = fixture.app.transcript_cells.len();
        super::issue_507_switch(&fixture.app, HumanRevocation::KillSwitchOff);
        fixture
            .app
            .on_kill_switch_changed(/*on*/ false, /*elsewhere*/ false);
        route_for(&mut fixture, std::time::Duration::from_millis(200)).await?;
        let notice = transcript_since(&fixture.app, from);
        assert!(!notice.contains("Nothing restarted"), "{notice}");
        fixture.server.shutdown().await?;
        Ok(())
    })
}

/// D106 review: an agent's task for a pane the person opened that Core has
/// not loaded fails, as it did when the pane had no live session; the pane
/// is never re-created as a saved agent, also when it is bound as the
/// Nazgul.
#[test]
fn agent_task_to_unloaded_person_opened_pane_fails_without_recreating_it() -> Result<()> {
    run_dispatch_integration(|| async {
        let mock = MockServer::start().await;
        let responses = mount_sse_repeating(&mock, sse_completed("pf1176-unloaded")).await;
        let mut fixture = RealDispatchFixture::start(&mock, /*max_threads*/ 3).await?;
        let pane = ThreadId::from_string("00000000-0000-0000-0000-000000011760")?;
        fixture.app.upsert_agent_picker_thread(
            pane,
            Some("Restored".to_string()),
            /*agent_role*/ None,
            /*is_closed*/ false,
        );
        let pane_node = crate::spawn_orchestration::thread_node_id(pane);
        fixture.app.set_spawn_nazgul_pane_binding(pane_node.clone());
        let entry = fixture
            .app
            .agent_navigation
            .get(&pane)
            .expect("pane entry")
            .clone();
        assert!(fixture.app.is_operator_owned_codex_user_pane(pane, &entry));
        let root = fixture.root;
        let other_threads = move |app: &App| {
            let mut threads = app
                .thread_event_channels
                .keys()
                .copied()
                .filter(|thread_id| *thread_id != pane && *thread_id != root)
                .collect::<Vec<_>>();
            threads.sort_by_key(ToString::to_string);
            threads
        };
        // Let start-up work settle before taking the snapshot.
        route_for(&mut fixture, std::time::Duration::from_secs(2)).await?;
        let threads_before = other_threads(&fixture.app);
        let navigation_before = fixture.app.agent_navigation.ordered_threads().len();

        let before = fixture.app.transcript_cells.len();
        fixture
            .app
            .send_native_task_event(pane, "pf1176 unloaded".to_string());
        route_for(&mut fixture, std::time::Duration::from_secs(2)).await?;
        let said = transcript_since(&fixture.app, before);
        assert!(said.contains("Could not admit task"), "{said}");
        assert!(said.contains("thread not found"), "{said}");
        assert!(!said.contains("materialize"), "{said}");
        assert!(responses.requests().is_empty(), "no turn started");
        // No thread was started or resumed in the pane's place.
        pretty_assertions::assert_eq!(other_threads(&fixture.app), threads_before);
        pretty_assertions::assert_eq!(
            fixture.app.agent_navigation.ordered_threads().len(),
            navigation_before
        );
        pretty_assertions::assert_eq!(
            fixture.app.spawn_nazgul_pane_id.as_deref(),
            Some(pane_node.as_str())
        );
        fixture.server.shutdown().await?;
        Ok(())
    })
}

/// D106/D107 (#1176): with the kill switch on, an agent's task for a pane
/// the person opened waits in that pane's mailbox, as for an agent-opened
/// pane: no turn starts, and turning the switch off starts nothing. The
/// person's own Spawn status task is refused while the switch is on (D107).
/// The waiting task goes with the pane's next turn, still as an inter-agent
/// message, while the person's task is their input.
#[test]
fn agent_task_to_person_opened_pane_waits_in_mailbox_while_kill_switch_on() -> Result<()> {
    run_dispatch_integration(|| async {
        use crate::legacy_core::security_revocation::HumanRevocation;
        let mock = MockServer::start().await;
        let responses = mount_sse_repeating(&mock, sse_completed("pf1176-operator")).await;
        let mut fixture = RealDispatchFixture::start(&mock, /*max_threads*/ 3).await?;
        let target = create_person_opened_pane(&mut fixture).await?;

        super::issue_507_switch(&fixture.app, HumanRevocation::KillSwitchOn);
        fixture
            .app
            .send_native_task_event(target, "pf1176 agent queued".to_string());
        route_for(&mut fixture, std::time::Duration::from_secs(2)).await?;
        assert!(responses.requests().is_empty(), "no turn started");
        assert!(
            fixture
                .app
                .agent_navigation
                .get(&target)
                .is_some_and(|entry| !entry.is_running)
        );
        pretty_assertions::assert_eq!(
            fixture.app.kill_switch_held_native_tasks.clone(),
            vec![target]
        );

        // D107: the person's Spawn status task is refused while it is on.
        let before = fixture.app.transcript_cells.len();
        (crate::spawn_orchestration::spawn_agent_task_prompt_submit(
            fixture.app.app_event_tx.clone(),
            target,
        ))("pf1176 person while on".to_string());
        route_for(&mut fixture, std::time::Duration::from_secs(1)).await?;
        assert!(responses.requests().is_empty(), "no turn started");
        let said = transcript_since(&fixture.app, before);
        assert!(said.contains("the security kill switch is on"), "{said}");

        super::issue_507_switch(&fixture.app, HumanRevocation::KillSwitchOff);
        Box::pin(fixture.app.handle_event(
            &mut fixture.tui,
            &mut fixture.server,
            AppEvent::KillSwitchTurnedOff,
        ))
        .await?;
        route_for(&mut fixture, std::time::Duration::from_secs(2)).await?;
        assert!(
            responses.requests().is_empty(),
            "switch-off started nothing"
        );

        // The person's next task starts the pane's turn; the waiting task
        // goes with it as an inter-agent message, not as their input.
        let person_task = "pf1176 person after";
        (crate::spawn_orchestration::spawn_agent_task_prompt_submit(
            fixture.app.app_event_tx.clone(),
            target,
        ))(person_task.to_string());
        fixture
            .route_until(std::time::Duration::from_secs(20), |_| {
                !responses.requests().is_empty()
            })
            .await?;
        let request = responses.requests().into_iter().next().expect("request");
        let items = request.input();
        let serialized = serde_json::to_string(&items)?;
        assert!(
            items.iter().any(|item| is_user_text(item, person_task)),
            "{serialized}"
        );
        assert!(
            pending_agent_message_text(&request).contains("pf1176 agent queued")
                || serialized.contains(&format!(
                    "<inter_agent_message sender=\\\"/root\\\" recipient=\\\"/root\\\">\\nSender: agent /root in another pane's agent tree (thread {}), not this pane's /root\\npf1176 agent queued\\n</inter_agent_message>",
                    fixture.root
                )),
            "{serialized}"
        );
        assert!(
            !items
                .iter()
                .any(|item| is_user_text(item, "pf1176 agent queued")),
            "{serialized}"
        );
        assert!(
            !serialized.contains("pf1176 person while on"),
            "{serialized}"
        );
        fixture.server.shutdown().await?;
        Ok(())
    })
}
