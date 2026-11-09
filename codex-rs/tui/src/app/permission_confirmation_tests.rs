use super::super::test_support::pending_confirmation;
use super::*;
use pretty_assertions::assert_eq;

/// Whether `selection_id` is the pending selection's. Every request arms its
/// own 20 s deadline (`request_permission_confirmation`), so once a test has
/// run that long, an earlier selection's deadline arrives while a later one
/// is pending (#1854: loaded Windows CI). The app ignores such a completion
/// (`finish_permission_confirmation`), so the checks below do too.
fn is_pending(app: &App, selection_id: uuid::Uuid) -> bool {
    app.pending_permission_confirmation
        .as_ref()
        .is_some_and(|pending| pending.selection_id == selection_id)
}

#[tokio::test]
async fn permission_confirmation_native_requests_carry_effective_defaults() {
    Box::pin(async {
        let (mut app, mut events, mut ops) =
            super::super::tests::make_test_app_with_channels().await;
        std::fs::write(app.config.codex_home.join("config.toml"),
            "default_permissions = \":danger-full-access\"\n[permissions.named-workspace]\nextends = \":workspace\"\n[permissions.named-read]\nextends = \":read-only\"\n").unwrap();
        let mut server = crate::start_embedded_app_server_for_picker(&app.config)
            .await
            .unwrap();
        let started = server.start_thread(&app.config).await.unwrap();
        let thread_id = started.session.thread_id;
        app.active_thread_id = Some(thread_id);
        app.chat_widget.handle_thread_session(started.session);
        while events.try_recv().is_ok() {}
        while ops.try_recv().is_ok() {}
        for profile in [
            ":danger-full-access",
            ":read-only",
            ":read-only",
            "named-workspace",
            "named-read",
            ":read-only",
            "missing-profile",
        ] {
            let params = ThreadSettingsUpdateParams {
                thread_id: thread_id.to_string(),
                permissions: Some(profile.into()),
                ..Default::default()
            };
            app.request_permission_confirmation(&mut server, params.clone(), profile.into(), /*persist_reviewer*/ None);
            let pending = app.pending_permission_confirmation.clone();
            app.request_permission_confirmation(
                &mut server,
                params,
                "refused second request".into(),
                /*persist_reviewer*/ None,
            );
            assert_eq!(app.pending_permission_confirmation, pending);
            if profile == ":danger-full-access" {
                app.chat_widget.restore_user_message_to_composer(
                    crate::chatwidget::UserMessage::from("draft while confirmation is pending"),
                );
                app.chat_widget
                    .handle_key_event(crossterm::event::KeyEvent::new(
                        crossterm::event::KeyCode::Enter,
                        crossterm::event::KeyModifiers::NONE,
                    ));
                let op = std::iter::from_fn(|| ops.try_recv().ok())
                    .find(|op| matches!(op, crate::app_command::AppCommand::UserTurn { .. }))
                    .expect("composer submitted a turn");
                assert!(
                    app.try_submit_active_thread_op_via_app_server(&mut server, thread_id, &op)
                        .await
                        .unwrap()
                );
                assert_eq!(
                    app.chat_widget.composer_text_with_pending(),
                    "draft while confirmation is pending"
                );
                assert_eq!(app.pending_permission_confirmation, pending);
            }
            tokio::time::timeout(std::time::Duration::from_secs(20), async {
                while app.pending_permission_confirmation.is_some() {
                    tokio::select! {
                        event = events.recv() => {
                            if let Some(AppEvent::PermissionConfirmationCompleted { selection_id, result }) = event
                                && is_pending(&app, selection_id)
                            {
                            assert!(matches!(
                                (&result, profile),
                                (PermissionConfirmationResult::Failed(_), "missing-profile")
                                    | (PermissionConfirmationResult::Applied, ":danger-full-access" | ":read-only" | "named-workspace" | "named-read")
                            ), "{profile}: {result:?}");
                            app.finish_permission_confirmation(selection_id, result);
                            }
                        }
                        event = server.next_event() => {
                            if let Some(codex_app_server_client::AppServerEvent::ServerNotification(notification)) = event {
                            app.handle_thread_event_now(ThreadBufferedEvent::Notification(notification));
                            }
                        }
                    }
                }
            })
            .await
            .unwrap_or_else(|error| panic!("{profile}: {:?}: {error}", app.pending_permission_confirmation));
            let expected = if profile == "missing-profile" { ":read-only" } else { profile };
            app.refresh_in_memory_config_from_disk().await.unwrap();
            let fresh = app.fresh_session_config();
            assert_eq!(fresh.permissions.active_permission_profile().unwrap().id, expected);
            let next = server.start_thread(&fresh).await.unwrap();
            assert_eq!(next.session.active_permission_profile, fresh.permissions.active_permission_profile());
            assert_eq!(next.session.permission_profile, fresh.permissions.effective_permission_profile());
            assert!(
                ops.try_recv().is_err(),
                "selection must not interrupt, approve or replay work"
            );
        }
        server.shutdown().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn permission_confirmation_f07_conflicts_are_refused_and_old_completion_cannot_win() {
    Box::pin(async {
        for order in [
            [":danger-full-access", ":read-only", ":danger-full-access"],
            [":read-only", ":danger-full-access", ":read-only"],
        ] {
            let (mut app, mut events, mut ops) =
                super::super::tests::make_test_app_with_channels().await;
            let mut server = crate::start_embedded_app_server_for_picker(&app.config)
                .await
                .unwrap();
            let started = server.start_thread(&app.config).await.unwrap();
            let thread = started.session.thread_id;
            app.active_thread_id = Some(thread);
            app.chat_widget.handle_thread_session(started.session);
            // Establish the command-channel baseline before any selection.
            // Session initialization may request ordinary metadata such as skills.
            let startup_ops = std::iter::from_fn(|| ops.try_recv().ok()).collect::<Vec<_>>();
            assert!(!startup_ops.is_empty(), "session initialization must request skills");
            assert!(startup_ops.iter().all(|op| matches!(op,
                crate::app_command::AppCommand::ListSkills { .. }
            )), "unexpected pre-selection command: {startup_ops:?}");
            let mut previous = None;
            for profile in order {
                while events.try_recv().is_ok() {}
                let requested = ThreadSettingsUpdateParams {
                    thread_id: thread.to_string(),
                    permissions: Some(profile.into()),
                    ..Default::default()
                };
                app.request_permission_confirmation(
                    &mut server, requested.clone(), profile.into(), /*persist_reviewer*/ None,
                );
                let pending = app.pending_permission_confirmation.clone().unwrap();
                let before = RuntimePermissionProfileOverride::from_config(&app.fresh_session_config());
                if let Some(old) = previous {
                    app.finish_permission_confirmation(old, PermissionConfirmationResult::Applied);
                    assert_eq!(app.pending_permission_confirmation, Some(pending.clone()));
                    assert_eq!(RuntimePermissionProfileOverride::from_config(&app.fresh_session_config()), before);
                }
                let conflicting = if profile == ":read-only" {
                    ":danger-full-access"
                } else {
                    ":read-only"
                };
                app.request_permission_confirmation(
                    &mut server,
                    ThreadSettingsUpdateParams {
                        permissions: Some(conflicting.into()),
                        ..requested
                    },
                    "refused-conflict".into(),
                    /*persist_reviewer*/ None,
                );
                assert_eq!(app.pending_permission_confirmation, Some(pending.clone()));
                let mut messages = Vec::new();
                tokio::time::timeout(std::time::Duration::from_secs(20), async {
                    while app.pending_permission_confirmation.is_some() {
                        tokio::select! {
                            event = events.recv() => match event {
                                Some(AppEvent::PermissionConfirmationCompleted { selection_id, result })
                                    if is_pending(&app, selection_id) =>
                                {
                                    assert!(matches!(result, PermissionConfirmationResult::Applied));
                                    app.finish_permission_confirmation(selection_id, result);
                                }
                                Some(AppEvent::InsertHistoryCell(cell)) => {
                                    messages.push(cell.display_lines(/*width*/ 120).iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"));
                                }
                                _ => {}
                            },
                            event = server.next_event() => {
                                if let Some(codex_app_server_client::AppServerEvent::ServerNotification(notification)) = event {
                                    app.handle_thread_event_now(ThreadBufferedEvent::Notification(notification));
                                }
                            }
                        }
                    }
                }).await.unwrap();
                while let Ok(event) = events.try_recv() {
                    if let AppEvent::InsertHistoryCell(cell) = event {
                        messages.push(cell.display_lines(/*width*/ 120).iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"));
                    }
                }
                let messages = messages.join("\n");
                assert!(messages.contains("A permission selection is still pending"));
                assert!(!messages.contains("Permissions requested: refused-conflict"));
                assert!(messages.contains(&format!("Permissions confirmed for new turns: {profile}")));
                assert_eq!(app.fresh_session_config().permissions.active_permission_profile().unwrap().id, profile);
                let unexpected = ops.try_recv().ok();
                assert!(unexpected.is_none(), "selection must not approve, interrupt or replay: {unexpected:?}");
                previous = Some(pending.selection_id);
            }
            server.shutdown().await.unwrap();
        }
    }).await;
}

#[tokio::test]
async fn permission_confirmation_f10_request_does_not_optimistically_apply_or_persist() {
    Box::pin(async {
        for (initial, policy, requested) in [
            (":read-only", "untrusted", ":danger-full-access"),
            (":danger-full-access", "never", ":read-only"),
        ] {
            let home = tempfile::tempdir().unwrap();
            let config_path = home.path().join("config.toml");
            let saved = format!(
                "default_permissions = \"{initial}\"\napproval_policy = \"{policy}\"\ncli_auth_credentials_store = \"file\"\n"
            );
            std::fs::write(&config_path, &saved).unwrap();
            let (mut app, mut events, _ops) =
                super::super::tests::make_test_app_with_channels().await;
            app.config = ConfigBuilder::default()
                .codex_home(home.path().to_path_buf())
                .harness_overrides(ConfigOverrides {
                    cwd: Some(home.path().to_path_buf()),
                    ..Default::default()
                })
                .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
                .build()
                .await
                .unwrap();
            let before = RuntimePermissionProfileOverride::from_config(&app.config);
            let before_policy = app.config.permissions.approval_policy.value();
            let mut server = crate::start_embedded_app_server_for_picker(&app.config)
                .await
                .unwrap();
            let started = server.start_thread(&app.config).await.unwrap();
            app.active_thread_id = Some(started.session.thread_id);
            app.chat_widget.handle_thread_session(started.session);
            // Full-access startup may persist project trust before selection.
            // Freeze the initialized disk state, not the pre-startup input.
            let saved = std::fs::read_to_string(&config_path).unwrap();
            while events.try_recv().is_ok() {}
            app.request_permission_confirmation(
                &mut server,
                ThreadSettingsUpdateParams {
                    thread_id: app.active_thread_id.unwrap().to_string(),
                    permissions: Some(requested.into()),
                    ..Default::default()
                },
                requested.into(),
                /*persist_reviewer*/ None,
            );
            let pending = app.pending_permission_confirmation.clone().unwrap();
            assert!(!pending.observed);
            // Observe the real RPC reply without delivering it or the settings
            // notification to the TUI. This is unconfirmed UI state, not proof
            // that backend application is still pending.
            tokio::time::timeout(std::time::Duration::from_secs(20), async {
                loop {
                    if let Some(AppEvent::PermissionConfirmationCompleted { selection_id, result }) =
                        events.recv().await
                    {
                        assert_eq!(selection_id, pending.selection_id);
                        assert!(matches!(result, PermissionConfirmationResult::Applied));
                        break;
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(app.pending_permission_confirmation, Some(pending));
            assert_eq!(app.runtime_permission_profile_override, None);
            assert_eq!(app.runtime_approval_policy_override, None);
            assert_eq!(
                RuntimePermissionProfileOverride::from_config(&app.fresh_session_config()),
                before
            );
            server.shutdown().await.unwrap();
            drop(app);
            assert_eq!(std::fs::read_to_string(&config_path).unwrap(), saved);

            // This checks request-time persistence only. Confirmed selection
            // also leaves saved launch config unchanged, so disk reload and a
            // fresh thread cannot distinguish confirmation or prove recovery
            // of a pending change in the affected thread.
            let reloaded = ConfigBuilder::default()
                .codex_home(home.path().to_path_buf())
                .harness_overrides(ConfigOverrides {
                    cwd: Some(home.path().to_path_buf()),
                    ..Default::default()
                })
                .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
                .build()
                .await
                .unwrap();
            assert_eq!(RuntimePermissionProfileOverride::from_config(&reloaded), before);
            assert_eq!(reloaded.permissions.approval_policy.value(), before_policy);
            let mut restarted = crate::start_embedded_app_server_for_picker(&reloaded)
                .await
                .unwrap();
            let fresh = restarted.start_thread(&reloaded).await.unwrap();
            assert_eq!(
                (fresh.session.active_permission_profile, fresh.session.permission_profile,
                 fresh.session.approval_policy),
                (Some(ActivePermissionProfile::new(initial)),
                 reloaded.permissions.effective_permission_profile(),
                 AskForApproval::from(before_policy))
            );
            restarted.shutdown().await.unwrap();
        }
    })
    .await;
}

#[tokio::test]
async fn permission_confirmation_hint_matches_retained_steer_and_latest_submission() {
    use crate::app_command::AppCommand;
    use codex_app_server_client::TypedRequestError;
    use codex_app_server_protocol::JSONRPCErrorError;
    use codex_app_server_protocol::ServerNotification;
    use codex_app_server_protocol::Turn;
    use codex_app_server_protocol::TurnCompletedNotification;
    use codex_app_server_protocol::TurnStartedNotification;
    use codex_app_server_protocol::TurnStatus;

    let (mut app, mut events, mut ops) = super::super::tests::make_test_app_with_channels().await;
    let thread_id = ThreadId::new();
    app.active_thread_id = Some(thread_id);
    app.chat_widget
        .handle_thread_session(super::super::tests::test_thread_session(
            thread_id,
            app.config.cwd.to_path_buf(),
        ));
    let turn = |status| Turn {
        id: "captured-turn".into(),
        items_view: codex_app_server_protocol::TurnItemsView::Full,
        items: Vec::new(),
        status,
        error: None,
        started_at: None,
        completed_at: None,
        duration_ms: None,
    };
    app.chat_widget.handle_server_notification(
        ServerNotification::TurnStarted(TurnStartedNotification {
            thread_id: thread_id.to_string(),
            turn: turn(TurnStatus::InProgress),
        }),
        /*replay_kind*/ None,
    );
    app.chat_widget
        .set_permission_profile_with_active_profile(
            PermissionProfile::Disabled,
            Some(ActivePermissionProfile::new(":danger-full-access")),
        )
        .unwrap();
    app.chat_widget.set_approval_policy(AskForApproval::Never);
    let pending = PendingPermissionConfirmation {
        observed: true,
        label: "Full Access".into(),
        requested: ThreadSettingsUpdateParams {
            permissions: Some(":danger-full-access".into()),
            ..Default::default()
        },
        ..pending_confirmation(thread_id)
    };
    let selection_id = pending.selection_id;
    app.pending_permission_confirmation = Some(pending);
    while events.try_recv().is_ok() {}
    while ops.try_recv().is_ok() {}
    app.finish_permission_confirmation(selection_id, PermissionConfirmationResult::Applied);
    let rendered_hint = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event {
            AppEvent::InsertHistoryCell(cell) => Some(cell.display_lines(/*width*/ 80)),
            _ => None,
        })
        .flatten()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = "retain this prompt through the authority change";
    app.chat_widget
        .restore_user_message_to_composer(crate::chatwidget::UserMessage::from(prompt));
    app.chat_widget
        .handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ));
    let attempted = std::iter::from_fn(|| ops.try_recv().ok())
        .find_map(|op| match op {
            AppCommand::UserTurn { items, .. } => Some(items),
            _ => None,
        })
        .expect("prompt attempts steering while the turn runs");
    app.note_thread_steer_failure(
        thread_id,
        TypedRequestError::Server {
            method: "turn/steer".into(),
            source: JSONRPCErrorError {
                code: -32600,
                message: "authorization changed".into(),
                data: Some(serde_json::json!({"code": "authorizationChanged"})),
            },
        },
    );
    let held = app.chat_widget.queued_user_message_texts();
    assert!(!app.chat_widget.maybe_send_next_queued_input());
    assert!(
        ops.try_recv().is_err(),
        "held input must not submit during the running turn"
    );

    // A subsequent selection must govern the retained prompt, not its send-time settings.
    app.chat_widget
        .set_permission_profile_with_active_profile(
            PermissionProfile::read_only(),
            Some(ActivePermissionProfile::read_only()),
        )
        .unwrap();
    app.chat_widget
        .set_approval_policy(AskForApproval::OnRequest);
    app.chat_widget.handle_server_notification(
        ServerNotification::TurnCompleted(TurnCompletedNotification {
            thread_id: thread_id.to_string(),
            turn: turn(TurnStatus::Completed),
        }),
        /*replay_kind*/ None,
    );
    let submitted = std::iter::from_fn(|| ops.try_recv().ok())
        .filter_map(|op| match op {
            AppCommand::UserTurn {
                items,
                approval_policy,
                active_permission_profile,
                ..
            } => Some((items, approval_policy, active_permission_profile)),
            _ => None,
        })
        .collect::<Vec<_>>();
    // One contract assertion couples the visible claim to retention and exactly-once replay.
    assert_eq!(
        (rendered_hint.contains("a prompt sent here is held until it finishes, then runs with the latest permissions"),
         held, submitted, app.chat_widget.queued_user_message_texts()),
        (true, vec![prompt.to_string()],
         vec![(attempted, AskForApproval::OnRequest, Some(ActivePermissionProfile::read_only()))],
         Vec::<String>::new()),
    );
}

#[test]
fn permission_confirmation_outcomes_are_explicit() {
    let messages = [
        PermissionConfirmationResult::Applied,
        PermissionConfirmationResult::Unsupported,
        PermissionConfirmationResult::Failed("rejected by requirements".into()),
        PermissionConfirmationResult::Uncertain("connection lost".into()),
    ]
    .iter()
    .map(|result| {
        let (message, hint) = permission_confirmation_message("Full Access", result);
        crate::history_cell::new_info_event(message, Some(hint))
            .display_lines(/*width*/ 80)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    })
    .collect::<Vec<_>>()
    .join("\n\n");
    insta::assert_snapshot!(messages, @r"
    • Permissions confirmed for new turns: Full Access. If a running turn has different permissions, a prompt sent here is held until it finishes, then runs with the latest permissions. Running work, granted approvals and pending approvals are unchanged. Shared services keep their existing refresh behavior.

    • Permissions unconfirmed: Full Access. This server does not provide application confirmation. The request may have been accepted. No automatic retry was made; reconnect to a server that supports confirmation to verify a new selection.

    • Permission selection failed: Full Access. rejected by requirements The rejected selection was not saved as a future-turn override. Choose again after resolving the error.

    • Permission outcome uncertain: Full Access. No automatic retry was made; running authority is unchanged. Reconnect and inspect session settings before choosing again. connection lost
    ");
}

#[tokio::test]
async fn permission_confirmation_stale_and_failed_results_do_not_mutate_config() {
    let mut app = super::super::tests::make_test_app().await;
    let thread_id = ThreadId::new();
    app.active_thread_id = Some(thread_id);
    let selection_id = uuid::Uuid::new_v4();
    let before = app.config.permissions.permission_profile().clone();
    let pending = Some(PendingPermissionConfirmation {
        selection_id,
        persist_reviewer: Some(ApprovalsReviewer::User),
        ..pending_confirmation(thread_id)
    });
    for (result, expected) in [
        (PermissionConfirmationResult::Uncertain("lost".into()), None),
        (PermissionConfirmationResult::Unsupported, None),
        (PermissionConfirmationResult::Failed("denied".into()), None),
    ] {
        app.pending_permission_confirmation = pending.clone();
        assert_eq!(
            app.finish_permission_confirmation(
                uuid::Uuid::new_v4(),
                PermissionConfirmationResult::Applied
            ),
            None
        );
        assert_eq!(app.pending_permission_confirmation, pending);
        assert_eq!(
            app.finish_permission_confirmation(selection_id, result),
            expected
        );
        assert!(app.pending_permission_confirmation.is_none());
        assert_eq!(app.config.permissions.permission_profile(), &before);
        assert_eq!(app.runtime_permission_profile_override, None);
        assert_eq!(
            app.finish_permission_confirmation(selection_id, PermissionConfirmationResult::Applied),
            None
        );
    }
    app.pending_permission_confirmation = pending;
    app.active_thread_id = Some(ThreadId::new());
    app.finish_permission_confirmation(selection_id, PermissionConfirmationResult::Applied);
    assert!(app.pending_permission_confirmation.is_none());
    assert_eq!(app.runtime_permission_profile_override, None);
}

#[tokio::test]
async fn permission_confirmation_orders_profiles_and_newer_observations() {
    for named in [false, true] {
        for reply_first in [false, true] {
            let (mut app, mut events, _ops) =
                super::super::tests::make_test_app_with_channels().await;
            let thread_id = ThreadId::new();
            app.active_thread_id = Some(thread_id);
            for (id, policy) in [
                (":danger-full-access", AskForApproval::Never),
                (":read-only", AskForApproval::OnRequest),
            ] {
                let profile_id = if named {
                    if id == ":read-only" {
                        "named-read"
                    } else {
                        "named-workspace"
                    }
                    .to_string()
                } else {
                    id.into()
                };
                let profile = if id == ":read-only" {
                    PermissionProfile::read_only()
                } else if named {
                    PermissionProfile::workspace_write()
                } else {
                    PermissionProfile::Disabled
                };
                let mut session = super::super::tests::test_thread_session(
                    thread_id,
                    app.config.cwd.to_path_buf(),
                );
                session.permission_profile = profile.clone();
                session.active_permission_profile =
                    Some(ActivePermissionProfile::new(profile_id.clone()));
                session.approval_policy = policy;
                let selection_id = uuid::Uuid::new_v4();
                app.pending_permission_confirmation = Some(PendingPermissionConfirmation {
                    selection_id,
                    label: profile_id.clone(),
                    requested: ThreadSettingsUpdateParams {
                        permissions: Some(profile_id),
                        approval_policy: Some(policy),
                        ..Default::default()
                    },
                    ..pending_confirmation(thread_id)
                });
                if reply_first {
                    let before =
                        RuntimePermissionProfileOverride::from_config(&app.fresh_session_config());
                    app.finish_permission_confirmation(
                        selection_id,
                        PermissionConfirmationResult::Applied,
                    );
                    assert_eq!(
                        RuntimePermissionProfileOverride::from_config(&app.fresh_session_config()),
                        before
                    );
                    assert!(app.pending_permission_confirmation.is_some());
                }
                app.chat_widget.handle_thread_session(session.clone());
                app.observe_permission_confirmation();
                if reply_first {
                    assert!(std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(event,
                        AppEvent::PermissionConfirmationCompleted { selection_id: id, result: PermissionConfirmationResult::Applied } if id == selection_id)));
                }
                app.finish_permission_confirmation(
                    selection_id,
                    PermissionConfirmationResult::Applied,
                );
                let fresh = app.fresh_session_config();
                assert_eq!(fresh.permissions.permission_profile(), &profile);
                assert_eq!(
                    fresh.permissions.active_permission_profile(),
                    session.active_permission_profile
                );
                assert_eq!(
                    AskForApproval::from(fresh.permissions.approval_policy.value()),
                    policy
                );
                let mut reloaded = super::super::tests::make_test_app().await.config.clone();
                app.apply_runtime_policy_overrides(&mut reloaded);
                assert_eq!(
                    RuntimePermissionProfileOverride::from_config(&reloaded),
                    RuntimePermissionProfileOverride::from_config(&fresh)
                );
                // An older reply cannot reinstall full access over a later observation.
                app.pending_permission_confirmation = Some(PendingPermissionConfirmation {
                    selection_id,
                    label: "old request".into(),
                    persist_reviewer: Some(ApprovalsReviewer::User),
                    requested: ThreadSettingsUpdateParams {
                        permissions: Some(":danger-full-access".into()),
                        ..Default::default()
                    },
                    observed: true,
                    ..pending_confirmation(thread_id)
                });
                session.permission_profile = PermissionProfile::read_only();
                session.active_permission_profile = Some(ActivePermissionProfile::read_only());
                app.chat_widget.handle_thread_session(session);
                while events.try_recv().is_ok() {}
                assert_eq!(
                    app.finish_permission_confirmation(
                        selection_id,
                        PermissionConfirmationResult::Applied
                    ),
                    None
                );
                assert_eq!(
                    app.fresh_session_config().permissions.permission_profile(),
                    &PermissionProfile::read_only()
                );
                let message = std::iter::from_fn(|| events.try_recv().ok())
                    .filter_map(|event| match event {
                        AppEvent::InsertHistoryCell(cell) => Some(
                            cell.display_lines(/*width*/ 80)
                                .iter()
                                .map(ToString::to_string)
                                .collect::<Vec<_>>()
                                .join("\n"),
                        ),
                        _ => None,
                    })
                    .last()
                    .unwrap();
                insta::allow_duplicates! {
                    insta::assert_snapshot!(message, @"• Permission request applied: old request. Newer settings govern new turns. If a running turn has different permissions, a prompt sent here is held until it finishes, then runs with the latest permissions. Running work, granted approvals and pending approvals are unchanged. Shared services keep their existing refresh behavior. Held initial input was restored without submission. Check /status for the latest next-turn settings.");
                }
            }
        }
    }
}

#[tokio::test]
async fn permission_confirmation_dispatch_preserves_policy_for_fresh_threads() {
    let (mut app, _events, _ops) = super::super::tests::make_test_app_with_channels().await;
    let mut server = crate::start_embedded_app_server_for_picker(&app.config)
        .await
        .unwrap();
    let mut tui = crate::tui::test_support::make_test_tui().unwrap();
    let thread_id = ThreadId::new();
    app.active_thread_id = Some(thread_id);
    // Keep the launch configuration different from both confirmed selections.
    // Reloading it exercises the same override path used by /new.
    for (profile, active_profile, policy) in [
        (
            PermissionProfile::Disabled,
            ActivePermissionProfile::new(":danger-full-access"),
            AskForApproval::Never,
        ),
        (
            PermissionProfile::workspace_write(),
            ActivePermissionProfile::new(":workspace"),
            AskForApproval::OnRequest,
        ),
        (
            PermissionProfile::Disabled,
            ActivePermissionProfile::new(":danger-full-access"),
            AskForApproval::Never,
        ),
    ] {
        let mut session =
            super::super::tests::test_thread_session(thread_id, app.config.cwd.to_path_buf());
        session.permission_profile = profile.clone();
        session.active_permission_profile = Some(active_profile.clone());
        session.approval_policy = policy;
        app.chat_widget.handle_thread_session(session);
        let selection_id = uuid::Uuid::new_v4();
        app.pending_permission_confirmation = Some(PendingPermissionConfirmation {
            selection_id,
            requested: ThreadSettingsUpdateParams {
                permissions: Some(active_profile.id.clone()),
                approval_policy: Some(policy),
                ..Default::default()
            },
            ..pending_confirmation(thread_id)
        });
        app.observe_permission_confirmation();
        Box::pin(app.handle_event(
            &mut tui,
            &mut server,
            AppEvent::PermissionConfirmationCompleted {
                selection_id,
                result: PermissionConfirmationResult::Applied,
            },
        ))
        .await
        .unwrap();

        assert!(app.pending_permission_confirmation.is_none());
        assert_eq!(app.config.permissions.permission_profile(), &profile);
        assert_eq!(app.runtime_approval_policy_override, Some(policy));
        assert_eq!(
            app.runtime_permission_profile_override,
            Some(RuntimePermissionProfileOverride::from_config(&app.config))
        );
        app.refresh_in_memory_config_from_disk().await.unwrap();
        let fresh = app.fresh_session_config();
        let next = server.start_thread(&fresh).await.unwrap();
        assert_eq!(
            (
                next.session.permission_profile,
                next.session.active_permission_profile,
                next.session.approval_policy,
            ),
            (
                fresh.permissions.effective_permission_profile(),
                Some(active_profile),
                policy,
            )
        );
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn permission_confirmation_held_input_resumes_only_after_matching_success() {
    for result in [
        PermissionConfirmationResult::Applied,
        PermissionConfirmationResult::Unsupported,
        PermissionConfirmationResult::Failed("denied".into()),
        PermissionConfirmationResult::Uncertain("lost".into()),
    ] {
        let (mut app, mut events, _ops) = super::super::tests::make_test_app_with_channels().await;
        let success = matches!(result, PermissionConfirmationResult::Applied);
        app.chat_widget = ChatWidget::new_with_app_event(crate::chatwidget::ChatWidgetInit {
            config: app.config.clone(),
            frame_requester: crate::tui::FrameRequester::test_dummy(),
            app_event_tx: app.app_event_tx.clone(),
            workspace_command_runner: None,
            initial_user_message: Some(crate::chatwidget::UserMessage::from("held initial prompt")),
            enhanced_keys_supported: false,
            has_chatgpt_account: false,
            has_codex_backend_auth: false,
            model_catalog: app.model_catalog.clone(),
            feedback: codex_feedback::CodexFeedback::new(),
            is_first_run: false,
            status_account_display: None,
            runtime_model_provider_base_url: None,
            initial_plan_type: None,
            model: app.config.model.clone(),
            startup_tooltip_override: None,
            status_line_invalid_items_warned: app.status_line_invalid_items_warned.clone(),
            terminal_title_invalid_items_warned: app.terminal_title_invalid_items_warned.clone(),
            session_telemetry: app.session_telemetry.clone(),
        });
        let thread_id = ThreadId::new();
        app.active_thread_id = Some(thread_id);
        let mut session =
            super::super::tests::test_thread_session(thread_id, app.config.cwd.to_path_buf());
        session.active_permission_profile = Some(ActivePermissionProfile::read_only());
        app.chat_widget
            .set_initial_user_message_submit_suppressed(/*suppressed*/ true);
        app.chat_widget.handle_thread_session(session);
        app.chat_widget
            .set_initial_user_message_submit_suppressed(/*suppressed*/ false);
        while events.try_recv().is_ok() {}
        let selection_id = uuid::Uuid::new_v4();
        app.pending_permission_confirmation = Some(PendingPermissionConfirmation {
            selection_id,
            requested: ThreadSettingsUpdateParams {
                permissions: Some(":read-only".into()),
                ..Default::default()
            },
            ..pending_confirmation(thread_id)
        });
        app.finish_permission_confirmation(
            uuid::Uuid::new_v4(),
            PermissionConfirmationResult::Applied,
        );
        assert!(events.try_recv().is_err());
        if success || matches!(result, PermissionConfirmationResult::Uncertain(_)) {
            app.finish_permission_confirmation(selection_id, PermissionConfirmationResult::Applied);
            assert!(
                events.try_recv().is_err(),
                "reply alone cannot release held input"
            );
            if success {
                app.observe_permission_confirmation();
            }
        }
        let mut server = crate::start_embedded_app_server_for_picker(&app.config)
            .await
            .unwrap();
        let mut tui = crate::tui::test_support::make_test_tui().unwrap();
        Box::pin(app.handle_event(
            &mut tui,
            &mut server,
            AppEvent::PermissionConfirmationCompleted {
                selection_id,
                result,
            },
        ))
        .await
        .unwrap();
        server.shutdown().await.unwrap();
        let submissions = std::iter::from_fn(|| events.try_recv().ok())
            .filter(|event| {
                matches!(
                    event,
                    AppEvent::CodexOp(crate::app_command::AppCommand::UserTurn { .. })
                )
            })
            .count();
        assert_eq!(submissions, usize::from(success));
        assert_eq!(
            app.chat_widget.composer_text_with_pending(),
            if success { "" } else { "held initial prompt" }
        );
        app.chat_widget.submit_initial_user_message_if_pending();
        app.finish_permission_confirmation(selection_id, PermissionConfirmationResult::Applied);
        assert!(
            !std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(
                event,
                AppEvent::CodexOp(crate::app_command::AppCommand::UserTurn { .. })
            )),
            "recovery/duplicate completion must not auto-send"
        );
    }
}
