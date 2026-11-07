#![allow(clippy::unwrap_used)]
use super::accounting_responses_ws_support::*;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_state::accounting::*;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;
use wiremock::MockServer;

#[tokio::test]
async fn accounting_responses_ws_native_admission_and_guard_barriers() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let mut gate = Gate::start().await?;
    let test = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
        .build_with_auto_env(&server)
        .await?;
    let db = test.codex.state_db().unwrap();
    let mut lock = connection(&db).await?;
    sqlx::query("BEGIN IMMEDIATE").execute(&mut lock).await?;
    submit(&test).await?;
    assert!(
        tokio::time::timeout(Duration::from_millis(150), gate.next())
            .await
            .is_err()
    );
    assert_eq!(gate.counts.lock().unwrap().2, 0);
    sqlx::query("ROLLBACK").execute(&mut lock).await?;
    gate.next().await?.complete().await?;
    terminal(&test).await?;
    sqlx::query("CREATE TRIGGER reject_ws_attempt BEFORE INSERT ON draft_accounting_attempts BEGIN SELECT RAISE(ABORT, 'fixture'); END").execute(&mut lock).await?;
    // A failed admission sends the request unrecorded.
    submit(&test).await?;
    gate.next().await?.complete().await?;
    core_test_support::assert_accounting_gap(&terminal(&test).await?);
    gate.no_pending().await;
    assert_eq!(turn_attempts(&db).await?.len(), 1);
    let guard = test
        .codex
        .stage_one_memory_client(codex_protocol::ThreadId::new(), &test.config.model_provider)
        .await;
    assert!(guard.is_err());
    gate.no_pending().await;
    stop(&test).await;
    // The live-binding fixture exists only in debug builds.
    #[cfg(debug_assertions)]
    live_binding_denial(/*prime_denial*/ true).await?;
    Ok(())
}

#[cfg(debug_assertions)]
#[tokio::test]
async fn accounting_responses_ws_native_late_clone_binding_stops_running_dispatch()
-> anyhow::Result<()> {
    live_binding_denial(/*prime_denial*/ false).await
}

#[cfg(debug_assertions)]
async fn live_binding_denial(prime_denial: bool) -> anyhow::Result<()> {
    let server = MockServer::start().await;
    for prefix in [false, true] {
        let mut gate = Gate::start().await?;
        let test = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
            .with_config(|config| {
                config.security_level = codex_security_policy::SecurityLevel::Permissive;
                config.model_provider.stream_max_retries = Some(2);
                let mut changed = config.model_provider.clone();
                changed.name = "changed fixture".into();
                config
                    .model_providers
                    .insert("changed-fixture".into(), changed);
            })
            .build_with_auto_env(&server)
            .await?;
        let memory = test
            .codex
            .stage_one_memory_client(
                test.session_configured.thread_id,
                &test.config.model_provider,
            )
            .await?;
        submit(&test).await?;
        let held = gate.next().await?;
        let db = test.codex.state_db().unwrap();
        if prefix {
            held.send(vec![event("response.usage", usage(Some(0)))])
                .await?;
            wait_turn_observations(&db, /*count*/ 1).await?;
        }
        let records = turn_attempts(&db).await?;
        let before = turn_observations(&db).await?;
        let total = totals(&db, &records[0]).await?;
        test.codex.attach_stage_one_binding_for_fixture(&memory)?;
        test.codex
            .submit(Op::ThreadSettings {
                thread_settings: codex_protocol::protocol::ThreadSettingsOverrides {
                    model_provider: Some("changed-fixture".into()),
                    ..Default::default()
                },
            })
            .await?;
        tokio::time::timeout(Duration::from_secs(2), async {
            while test.codex.config_snapshot().await.model_provider_id != "changed-fixture" {
                tokio::task::yield_now().await;
            }
        })
        .await?;
        if prime_denial {
            assert!(matches!(
                memory.check_completion().await,
                Err(codex_core::memory_stage_one::StageOneMemoryError::Denied(
                    codex_core::memory_stage_one::StageOneMemoryDenial::ProviderChanged
                ))
            ));
        }
        held.complete().await?;
        let events = terminal(&test).await?;
        assert!(events.iter().any(|event| matches!(event, EventMsg::Error(error)
            if error.message == "Fatal error: Stage-one memory denied this request while it was streaming; it was not re-sent")),
            "live stage-one denial must stop the already-admitted sampling request");
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, EventMsg::ModelResponseCompleted(_)))
        );
        gate.no_pending().await;
        assert_eq!(turn_attempts(&db).await?, records);
        assert_eq!(
            turn_observations(&db).await?,
            before,
            "live denial must preserve only committed usage"
        );
        assert_eq!(totals(&db, &records[0]).await?, total);
        assert_eq!(
            total.unknown_estimates,
            i64::from(!prefix) + PREWARM_UNKNOWN_ESTIMATES
        );
        counts(&gate, (1, 1, 1, 0));
        stop(&test).await;
    }
    Ok(())
}
#[tokio::test]
async fn accounting_responses_ws_native_observation_failure_no_repair() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let mut gate = Gate::start().await?;
    let test = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
        .build_with_auto_env(&server)
        .await?;
    submit(&test).await?;
    let held = gate.next().await?;
    let db = test.codex.state_db().unwrap();
    held.send(vec![event("response.usage", usage(Some(0)))])
        .await?;
    wait_turn_observations(&db, /*count*/ 1).await?;
    let before = turn_observations(&db).await?;
    sqlx::query("CREATE TRIGGER reject_ws_observation BEFORE INSERT ON draft_accounting_observations BEGIN SELECT RAISE(ABORT, 'fixture'); END").execute(&mut connection(&db).await?).await?;
    held.complete().await?;
    core_test_support::assert_accounting_gap(&terminal(&test).await?);
    gate.no_pending().await;
    assert_eq!(turn_observations(&db).await?, before);
    assert_eq!(turn_attempts(&db).await?.len(), 1);
    counts(&gate, (1, 1, 1, 0));
    stop(&test).await;
    Ok(())
}
#[tokio::test]
async fn accounting_responses_ws_native_cancel_before_and_after_dispatch() -> anyhow::Result<()> {
    for dispatched in [false, true] {
        let server = MockServer::start().await;
        let mut gate = Gate::start().await?;
        let test = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
            .build_with_auto_env(&server)
            .await?;
        let db = test.codex.state_db().unwrap();
        let mut lock = connection(&db).await?;
        if !dispatched {
            sqlx::query("BEGIN IMMEDIATE").execute(&mut lock).await?;
        }
        submit(&test).await?;
        let held = if dispatched {
            Some(gate.next().await?)
        } else {
            assert!(
                tokio::time::timeout(Duration::from_millis(150), gate.next())
                    .await
                    .is_err()
            );
            None
        };
        test.codex.submit(Op::Interrupt).await?;
        assert!(
            terminal(&test)
                .await?
                .iter()
                .any(|e| matches!(e, EventMsg::TurnAborted(_)))
        );
        if !dispatched {
            sqlx::query("ROLLBACK").execute(&mut lock).await?;
        }
        drop(held);
        gate.no_pending().await;
        assert_eq!(gate.counts.lock().unwrap().2, usize::from(dispatched));
        if dispatched {
            assert_eq!(turn_attempts(&db).await?.len(), 1);
            assert!(turn_observations(&db).await?.is_empty());
        }
        stop(&test).await;
    }
    Ok(())
}
#[tokio::test]
async fn accounting_responses_ws_native_two_reopens_and_original_prices() -> anyhow::Result<()> {
    for (prefix, model) in [
        (false, "gpt-5.6-terra"),
        (true, "gpt-5.6-terra"),
        (true, "codex-auto-review"),
    ] {
        let server = MockServer::start().await;
        let mut gate = Gate::start().await?;
        let mode = enabled(&gate.endpoint);
        let test = builder(gate.endpoint.clone(), mode.clone())
            .with_config(move |config| config.model = Some(model.into()))
            .build_with_auto_env(&server)
            .await?;
        submit(&test).await?;
        let held = gate.next().await?;
        let db = test.codex.state_db().unwrap();
        if prefix {
            held.send(vec![
                event("response.usage", usage(Some(0))),
                json!({"type":"response.created","response":{"id":"same"}}),
                event("response.usage", usage(Some(0))),
            ])
            .await?;
            wait_turn_observations(&db, /*count*/ 2).await?;
        }
        test.codex.submit(Op::Interrupt).await?;
        terminal(&test).await?;
        drop(held);
        let records = turn_attempts(&db).await?;
        let patches = turn_observations(&db).await?;
        let prices: Vec<Snapshot> = payloads::<Snapshot>(&db, "draft_accounting_price_snapshots")
            .await?
            .into_iter()
            // An unpriced attempt is bound to a pay-per-use record with no
            // rates (PF-60-S05); only priced records are counted here.
            .filter(|snapshot| snapshot.rates != codex_state::accounting::Rates::default())
            .collect();
        let before = totals(&db, &records[0]).await?;
        if prefix {
            // The turn's own money, plus the startup prewarm's $0.011988 - which
            // exists only where the catalogue prices this model at all.
            let priced = model == "gpt-5.6-terra";
            assert_eq!(
                before.known_usd,
                with_prewarms(
                    if priced { "0.000644" } else { "0" }
                        .to_string()
                        .try_into()?,
                    usize::from(priced)
                )?
            );
        }
        assert_eq!(prices.is_empty(), model == "codex-auto-review");
        let home = test.home.clone();
        let rollout = test.codex.rollout_path().unwrap();
        stop(&test).await;
        drop(test);
        db.close().await;
        for index in 0..2 {
            let reopened = builder(gate.endpoint.clone(), mode.clone())
                .with_config(move |config| config.model = Some(model.into()))
                .resume(&server, home.clone(), rollout.clone())
                .await?;
            let db = reopened.codex.state_db().unwrap();
            assert_eq!(turn_attempts(&db).await?, records);
            assert_eq!(turn_observations(&db).await?, patches);
            // Each reopen runs its own startup prewarm, which records its own
            // attempt and its own price, so the day legitimately grows. What
            // must not change is the turn's own evidence and the prices bound
            // to it: an original binding cannot be replaced by a later catalog.
            let current: Vec<Snapshot> = payloads(&db, "draft_accounting_price_snapshots").await?;
            for price in &prices {
                assert!(current.contains(price), "original price replaced on reopen");
            }
            // The day's own totals are not asserted here: a reopened session
            // starts its own prewarm, and whether that has landed by this point
            // is a race this test is not about.
            gate.no_pending().await;
            if index == 1 {
                submit(&reopened).await?;
                gate.next().await?.complete().await?;
                terminal(&reopened).await?;
                let fresh = turn_attempts(&db).await?;
                assert_eq!(fresh.len(), 2);
                assert_ne!(fresh[1].request_id, records[0].request_id);
                assert_eq!(fresh[1].retry_of, None);
            }
            stop(&reopened).await;
            drop(reopened);
            db.close().await;
        }
    }
    Ok(())
}
#[tokio::test]
async fn accounting_responses_ws_native_spawned_role_children_and_fork() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let mut gate = Gate::start().await?;
    let test = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
        .with_config(|config| {
            for feature in [
                codex_features::Feature::Collab,
                codex_features::Feature::MultiAgentV2,
            ] {
                config.features.enable(feature).unwrap();
            }
            config
                .features
                .disable(codex_features::Feature::CodeModeOnly)
                .unwrap();
            config.agents_enabled = true;
            config.model_provider.stream_idle_timeout_ms = Some(30000);
            config.model_provider.stream_max_retries = Some(1);
            for model in &mut config.model_catalog.as_mut().unwrap().models {
                model.multi_agent_version = Some(codex_protocol::protocol::MultiAgentVersion::V2);
                model.tool_mode = None;
                model.use_responses_lite = false;
            }
            let role = config.codex_home.join("ws-child.toml");
            std::fs::write(&role, "developer_instructions = \"ws role fixture\"\n").unwrap();
            config.agent_roles.insert(
                "fixture".into(),
                codex_core::config::AgentRoleConfig {
                    config_file: Some(role.to_path_buf()),
                    description: None,
                    nickname_candidates: None,
                },
            );
        })
        .build_with_auto_env(&server)
        .await?;
    submit(&test).await?;
    let root = gate.next().await?;
    let (namespace, tool) = root.body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|item| {
            if item["name"]
                .as_str()
                .is_some_and(|name| name.ends_with("spawn_agent"))
            {
                return Some((None, item["name"].as_str().unwrap()));
            }
            item["tools"].as_array()?.iter().find_map(|tool| {
                let name = tool["name"].as_str()?;
                name.ends_with("spawn_agent")
                    .then_some((item["name"].as_str(), name))
            })
        })
        .expect("native spawn tool");
    let mut events = vec![responses::ev_response_created("root")];
    for (index, role) in ["default", "fixture"].iter().enumerate() {
        let arguments = json!({"task_name":format!("child_{index}"),"message":"fixture","agent_type":role,"fork_turns":"none"}).to_string();
        events.push(match namespace {
            Some(namespace) => responses::ev_function_call_with_namespace(
                &format!("spawn-{index}"),
                namespace,
                tool,
                &arguments,
            ),
            None => responses::ev_function_call(&format!("spawn-{index}"), tool, &arguments),
        });
    }
    events.push(event("response.completed", usage(Some(0))));
    root.send(events).await?;
    let mut held = Vec::new();
    for _ in 0..3 {
        held.push(gate.next().await?);
    }
    let db = test.codex.state_db().unwrap();
    let records = turn_attempts(&db).await?;
    assert_eq!(records.len(), 4);
    let owners: std::collections::HashSet<_> = records.iter().map(|a| a.thread_id).collect();
    assert_eq!(owners.len(), 3);
    let edges: Vec<(String, String)> =
        sqlx::query_as("SELECT parent_thread_id, child_thread_id FROM thread_spawn_edges")
            .fetch_all(&mut connection(&db).await?)
            .await?;
    assert_eq!(edges.len(), 2);
    for (parent, child) in edges {
        assert_eq!(parent, test.session_configured.thread_id.to_string());
        assert!(owners.contains(&codex_protocol::ThreadId::from_string(&child)?));
    }
    let role_index = held
        .iter()
        .position(|request| request.body.to_string().contains("ws role fixture"))
        .expect("role reload");
    drop(held.remove(role_index));
    let retried = gate.next().await?;
    assert!(retried.body.to_string().contains("ws role fixture"));
    let retries = turn_attempts(&db).await?;
    assert_eq!(retries.len(), 5);
    let retry = retries.last().unwrap();
    let predecessor = records
        .iter()
        .find(|attempt| Some(attempt.attempt_id) == retry.retry_of)
        .unwrap();
    assert_ne!(retry.thread_id, test.session_configured.thread_id);
    assert_eq!(
        (retry.thread_id, retry.request_id),
        (predecessor.thread_id, predecessor.request_id)
    );
    held.push(retried);
    for held in held {
        held.complete().await?;
    }
    terminal(&test).await?;
    wait_observations(&db, /*count*/ 4).await?;
    let before = turn_attempts(&db).await?;
    test.thread_manager
        .fork_thread(
            codex_core::ForkSnapshot::Interrupted,
            test.config.clone(),
            test.codex.rollout_path().unwrap(),
            /*thread_source*/ None,
            /*parent_trace*/ None,
        )
        .await?;
    assert_eq!(turn_attempts(&db).await?, before);
    test.thread_manager
        .shutdown_all_threads_bounded(Duration::from_secs(3))
        .await;
    Ok(())
}
#[tokio::test]
async fn accounting_responses_ws_native_delete_rejects_late_usage() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let mut gate = Gate::start().await?;
    let test = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
        // The held stream stays silent while an unrelated thread starts and finishes a turn;
        // the fixture's 2s idle timeout failed it on loaded runners.
        .with_config(|config| config.model_provider.stream_idle_timeout_ms = Some(30_000))
        .build_with_auto_env(&server)
        .await?;
    submit(&test).await?;
    let held = gate.next().await?;
    let db = test.codex.state_db().unwrap();
    assert_eq!(turn_attempts(&db).await?.len(), 1);
    let unrelated = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
        .with_home(test.home.clone())
        .build_with_auto_env(&server)
        .await?;
    submit(&unrelated).await?;
    gate.next().await?.complete().await?;
    terminal(&unrelated).await?;
    let unrelated_records: Vec<_> = turn_attempts(&db)
        .await?
        .into_iter()
        .filter(|a| a.thread_id == unrelated.session_configured.thread_id)
        .collect();
    assert_eq!(unrelated_records.len(), 1);
    let unrelated_usage = turn_observations(&db).await?;
    db.delete_thread(test.session_configured.thread_id).await?;
    held.complete().await?;
    terminal(&test).await?;
    assert_eq!(turn_attempts(&db).await?, unrelated_records);
    assert_eq!(turn_observations(&db).await?, unrelated_usage);
    // Deletion must take the prewarm rows too: they are attempts of the deleted
    // thread like any other, so name them rather than filtering them away.
    for attempt in attempts(&db).await? {
        assert_eq!(
            attempt.thread_id, unrelated.session_configured.thread_id,
            "deleted thread left {} behind",
            attempt.turn
        );
    }
    gate.no_pending().await;
    let home = unrelated.home.clone();
    let rollout = unrelated.codex.rollout_path().unwrap();
    let owner = unrelated.session_configured.thread_id;
    stop(&unrelated).await;
    drop(unrelated);
    let off = builder(
        gate.endpoint.clone(),
        codex_core::config::AccountingMode::Disabled,
    )
    .resume(&server, home, rollout)
    .await?;
    off.codex.state_db().unwrap().delete_thread(owner).await?;
    assert!(turn_attempts(&db).await?.is_empty());
    assert!(observations(&db).await?.is_empty());
    stop(&off).await;
    stop(&test).await;
    Ok(())
}
#[tokio::test]
async fn accounting_responses_ws_native_unknown_prices_and_no_usage() -> anyhow::Result<()> {
    for usage_present in [false, true] {
        let server = MockServer::start().await;
        let mut gate = Gate::start().await?;
        let test = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
            .with_config(|config| config.model = Some("codex-auto-review".into()))
            .build_with_auto_env(&server)
            .await?;
        submit(&test).await?;
        let held = gate.next().await?;
        if usage_present {
            held.complete().await?;
        } else {
            held.send(vec![
                responses::ev_response_created("same"),
                json!({"type":"response.completed","response":{"id":"same"}}),
            ])
            .await?;
        }
        terminal(&test).await?;
        let db = test.codex.state_db().unwrap();
        let records = turn_attempts(&db).await?;
        assert_eq!(records.len(), 1);
        // No price: the attempt is bound only to its pay-per-use basis, by a
        // record with no rates (PF-60-S05).
        let bound: Vec<Snapshot> = payloads(&db, "draft_accounting_price_snapshots").await?;
        assert!(!bound.is_empty());
        assert!(bound.iter().all(|snapshot| snapshot.basis
            == codex_state::accounting::Basis::Billed
            && snapshot.rates == codex_state::accounting::Rates::default()));
        assert_eq!(
            turn_observations(&db).await?.len(),
            usize::from(usage_present)
        );
        let total = totals(&db, &records[0]).await?;
        // The turn's own incomplete estimate, plus the startup prewarm's.
        assert_eq!(total.unknown_estimates, 1 + PREWARM_UNKNOWN_ESTIMATES);
        // The day's totals also carry the startup prewarm's 1998 tokens.
        assert_eq!(
            total.measured[6],
            Metric {
                known: 1998 + if usage_present { 140 } else { 0 },
                unknown: i64::from(!usage_present)
            }
        );
        stop(&test).await;
    }
    Ok(())
}
#[tokio::test]
async fn accounting_responses_ws_native_auxiliary_scope_and_event_parity() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let mut gate = Gate::start().await?;
    let test = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
        .with_config(|config| {
            config
                .features
                .disable(codex_features::Feature::TokenBudget)
                .unwrap();
            config
                .features
                .disable(codex_features::Feature::RemoteCompactionV2)
                .unwrap();
        })
        .build_with_auto_env(&server)
        .await?;
    submit(&test).await?;
    gate.next().await?.complete().await?;
    let events = terminal(&test).await?;
    assert!(!events.iter().any(|e| matches!(e, EventMsg::Error(_))));
    let db = test.codex.state_db().unwrap();
    let before = turn_attempts(&db).await?;
    test.codex.submit(Op::Compact).await?;
    let compact_events = terminal(&test).await?;
    assert!(
        !compact_events
            .iter()
            .any(|event| matches!(event, EventMsg::Error(_)))
    );
    // The legacy compaction endpoint records its own `compact:` attempt; the
    // turn's own rows are unchanged.
    let after = turn_attempts(&db).await?;
    assert_eq!(after[..before.len()], before[..]);
    assert_eq!(after.len(), before.len() + 1);
    assert!(
        after[before.len()].turn.starts_with("compact:"),
        "{}",
        after[before.len()].turn
    );
    assert_eq!(turn_observations(&db).await?.len(), 1);
    counts(&gate, (1, 1, 1, 1));
    stop(&test).await;
    metadata_terminal_matrix(&server, 0..2).await
}

// The 72-case terminal-metadata matrix is split by mask so each test stays well
// inside the per-test timeout; together they cover every case.
#[tokio::test]
async fn accounting_responses_ws_native_event_parity_masks_2_3() -> anyhow::Result<()> {
    metadata_terminal_matrix(&MockServer::start().await, 2..4).await
}
#[tokio::test]
async fn accounting_responses_ws_native_event_parity_masks_4_5() -> anyhow::Result<()> {
    metadata_terminal_matrix(&MockServer::start().await, 4..6).await
}
#[tokio::test]
async fn accounting_responses_ws_native_event_parity_masks_6_7() -> anyhow::Result<()> {
    metadata_terminal_matrix(&MockServer::start().await, 6..8).await
}

async fn metadata_terminal_matrix(
    server: &MockServer,
    masks: std::ops::Range<u8>,
) -> anyhow::Result<()> {
    for mask in masks {
        for usage_kind in 0..3 {
            for kind in [
                "response.completed",
                "response.failed",
                "response.incomplete",
            ] {
                let mut gate = Gate::start().await?;
                let test = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
                    .build_with_auto_env(server)
                    .await?;
                let mut metadata = json!({});
                let mut expected = Vec::new();
                if mask & 1 != 0 {
                    metadata["openai_verification_recommendation"] =
                        json!(["trusted_access_for_cyber"]);
                    expected.push(json!({"type":"model_verification","verifications":["trusted_access_for_cyber"]}));
                }
                if mask & 2 != 0 {
                    metadata["openai_chatgpt_moderation_metadata"] =
                        json!({"presentation":"inline"});
                    expected.push(json!({"type":"turn_moderation_metadata","metadata":{"presentation":"inline"}}));
                }
                let mut end = json!({"type":kind,"response":{"id":"same","error":{"code":"invalid_prompt","message":"fixture terminal"},"incomplete_details":{"reason":"max_output_tokens"}},"metadata":metadata});
                if mask & 4 != 0 {
                    end["safety_buffering"] = json!({"use_cases":["cyber"],"reasons":["user_risk"],"retry_model":"gpt-fast-wire"});
                    expected.push(json!({"type":"safety_buffering","model":"gpt-5.6-terra","use_cases":["cyber"],"reasons":["user_risk"],"show_buffering_ui":true,"faster_model":"gpt-fast-wire"}));
                }
                if usage_kind != 0 {
                    end["response"]["usage"] = if usage_kind == 2 {
                        usage(Some(0))
                    } else {
                        serde_json::Value::Null
                    };
                }
                submit(&test).await?;
                gate.next()
                    .await?
                    .send(vec![
                        responses::ev_response_created("same"),
                        json!({"type":"response.metadata","metadata":metadata}),
                        end,
                    ])
                    .await?;
                let fallback = kind == "response.incomplete";
                if fallback {
                    let retry = gate.next().await?;
                    assert!(
                        retry.http,
                        "incomplete response retains native HTTP fallback"
                    );
                    retry.send(vec![json!({"type":"response.failed","response":{"error":{"code":"invalid_prompt","message":"fixture fallback"}}})]).await?;
                }
                let events = terminal(&test).await?;
                let actual: Vec<_> = events
                    .iter()
                    .filter(|event| {
                        matches!(
                            event,
                            EventMsg::ModelVerification(_)
                                | EventMsg::TurnModerationMetadata(_)
                                | EventMsg::SafetyBuffering(_)
                        )
                    })
                    .map(serde_json::to_value)
                    .collect::<Result<_, _>>()?;
                assert_eq!(
                    actual, expected,
                    "metadata parity: mask={mask}, usage={usage_kind}, kind={kind}"
                );
                assert_eq!(
                    events.iter().any(|e| matches!(e, EventMsg::Error(_))),
                    kind != "response.completed"
                );
                let db = test.codex.state_db().unwrap();
                let records = turn_attempts(&db).await?;
                assert_eq!(records.len(), 1 + usize::from(fallback));
                chain(&records);
                assert_eq!(
                    turn_observations(&db).await?.len(),
                    usize::from(usage_kind == 2),
                    "metadata terminal must not fabricate usage evidence: mask={mask}, usage={usage_kind}, kind={kind}"
                );
                assert_eq!(
                    totals(&db, &records[0]).await?,
                    with_prewarm(DayTotals {
                        measured: std::array::from_fn(|i| Metric {
                            known: if usage_kind == 2 {
                                [100, 80, 20, 0, 40, 10, 140][i]
                            } else {
                                0
                            },
                            unknown: i64::from(usage_kind != 2) + i64::from(fallback),
                        }),
                        known_usd: if usage_kind == 2 { "0.000644" } else { "0" }
                            .to_string()
                            .try_into()?,
                        unknown_estimates: i64::from(usage_kind != 2) + i64::from(fallback),
                        attempts: 1 + i64::from(fallback),
                        ..Default::default()
                    })
                );
                eprintln!("metadata matrix passed: mask={mask}, usage={usage_kind}, kind={kind}");
                gate.no_pending().await;
                counts(&gate, (1, 1, 1, usize::from(fallback)));
                stop(&test).await;
            }
        }
    }
    Ok(())
}

/// #1038: editing the first message continues in a conversation with no
/// history. Marked by the client, its warm-up is a reopen warm-up all the
/// same: recorded under the reopen label, announced, and skipped with
/// `skip_reopen_warmup`, as when a later message is edited.
#[tokio::test]
async fn accounting_first_message_edit_warmup_is_a_reopen_warmup() -> anyhow::Result<()> {
    const NOTICE: &str = "Reopening or forking this conversation sent a warm-up request";
    for skip in [false, true] {
        let server = MockServer::start().await;
        let mut gate = Gate::start().await?;
        let test = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
            .with_config(move |config| {
                config.skip_reopen_warmup = skip;
                config.reopened_by_prompt_edit = true;
            })
            .build_with_auto_env(&server)
            .await?;
        // The first turn waits for any warm-up, so by its end a warm-up was
        // either sent and announced, or never sent.
        submit(&test).await?;
        gate.next().await?.complete().await?;
        let events = terminal(&test).await?;
        let announced = events.iter().any(
            |event| matches!(event, EventMsg::Warning(warning) if warning.message.contains(NOTICE)),
        );
        let db = test.codex.state_db().unwrap();
        let prewarms: Vec<String> = attempts(&db)
            .await?
            .into_iter()
            .map(|attempt| attempt.turn)
            .filter(|turn| turn.starts_with("prewarm:"))
            .collect();
        let warmups = gate.counts.lock().unwrap().1;
        if skip {
            assert!(!announced, "a skipped warm-up is not announced");
            assert!(prewarms.is_empty(), "{prewarms:?}");
            assert_eq!(warmups, 0, "no warm-up sent");
        } else {
            assert!(announced, "the warm-up is announced: {events:?}");
            assert_eq!(prewarms.len(), 1, "{prewarms:?}");
            assert!(prewarms[0].starts_with("prewarm:reopen:"), "{prewarms:?}");
            assert_eq!(warmups, 1);
        }
        stop(&test).await;
    }
    Ok(())
}

/// #403 (Travis, 2026-10-14, option C): reopening a conversation sends its
/// warm-up as a reopen warm-up, recorded under its own label and announced
/// once sent. With `skip_reopen_warmup` set, a reopen sends no warm-up at all,
/// while a new conversation still warms up as before.
#[tokio::test]
async fn accounting_reopen_warmup_is_labelled_announced_and_skippable() -> anyhow::Result<()> {
    const NOTICE: &str = "Reopening or forking this conversation sent a warm-up request";
    for skip in [false, true] {
        let server = MockServer::start().await;
        let mut gate = Gate::start().await?;
        let test = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
            .with_config(move |config| config.skip_reopen_warmup = skip)
            .build_with_auto_env(&server)
            .await?;
        submit(&test).await?;
        gate.next().await?.complete().await?;
        let events = terminal(&test).await?;
        assert!(
            !events.iter().any(
                |event| matches!(event, EventMsg::Warning(warning) if warning.message.contains(NOTICE))
            ),
            "a new conversation is not a reopen"
        );
        let warmups_before = gate.counts.lock().unwrap().1;
        assert_eq!(
            warmups_before, 1,
            "a new conversation warms up (skip={skip})"
        );
        let labels = |attempts: Vec<Attempt>| {
            attempts
                .into_iter()
                .map(|attempt| attempt.turn)
                .filter(|turn| turn.starts_with("prewarm:"))
                .collect::<Vec<_>>()
        };
        let db = test.codex.state_db().unwrap();
        let new_prewarms = labels(attempts(&db).await?);
        assert_eq!(new_prewarms.len(), 1);
        assert!(!new_prewarms[0].starts_with("prewarm:reopen:"));
        let home = test.home.clone();
        let rollout = test.codex.rollout_path().unwrap();
        stop(&test).await;
        drop(test);
        db.close().await;

        let reopened = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
            .with_config(move |config| config.skip_reopen_warmup = skip)
            .resume(&server, home, rollout)
            .await?;
        // The first turn after a reopen waits for any warm-up, so by its end a
        // warm-up was either sent and announced, or never sent.
        submit(&reopened).await?;
        gate.next().await?.complete().await?;
        let events = terminal(&reopened).await?;
        let announced = events.iter().any(
            |event| matches!(event, EventMsg::Warning(warning) if warning.message.contains(NOTICE)),
        );
        let db = reopened.codex.state_db().unwrap();
        let reopen_prewarms: Vec<String> = labels(attempts(&db).await?)
            .into_iter()
            .filter(|turn| turn.starts_with("prewarm:reopen:"))
            .collect();
        let warmups_after = gate.counts.lock().unwrap().1;
        if skip {
            assert!(!announced, "a skipped warm-up is not announced");
            assert!(reopen_prewarms.is_empty(), "{reopen_prewarms:?}");
            assert_eq!(warmups_after, warmups_before, "no warm-up sent on reopen");
        } else {
            assert!(announced, "the reopen warm-up is announced: {events:?}");
            assert_eq!(reopen_prewarms.len(), 1, "{reopen_prewarms:?}");
            assert_eq!(warmups_after, warmups_before + 1);
        }
        stop(&reopened).await;
    }
    Ok(())
}
