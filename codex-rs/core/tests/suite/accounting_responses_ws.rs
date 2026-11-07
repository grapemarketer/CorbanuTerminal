use super::accounting_responses_ws_support::*;
use codex_core::config::AccountingMode;
use codex_protocol::protocol::EventMsg;
use codex_state::accounting::*;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;

#[tokio::test]
async fn accounting_responses_ws_native_off_and_http_only_compatibility() -> anyhow::Result<()> {
    for mode in [
        AccountingMode::Disabled,
        super::accounting_responses::support::enabled("https://api.openai.com/v1"),
    ] {
        let server = MockServer::start().await;
        let mut gate = Gate::start().await?;
        let test = builder(gate.endpoint.clone(), mode)
            .build_with_auto_env(&server)
            .await?;
        submit(&test).await?;
        gate.next().await?.complete().await?;
        terminal(&test).await?;
        absent(&test.codex.state_db().unwrap()).await?;
        counts(&gate, (1, 1, 1, 0));
        stop(&test).await;
    }
    Ok(())
}
#[tokio::test]
async fn accounting_responses_ws_native_complete_and_partial_goldens() -> anyhow::Result<()> {
    for write in [Some(0), None] {
        let server = MockServer::start().await;
        let mut gate = Gate::start().await?;
        let test = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
            .build_with_auto_env(&server)
            .await?;
        submit(&test).await?;
        let held = gate.next().await?;
        assert_eq!(held.body["model"], "gpt-5.6-terra");
        held.send(success(write)).await?;
        terminal(&test).await?;
        let db = test.codex.state_db().unwrap();
        let records = turn_attempts(&db).await?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].thread_id, test.session_configured.thread_id);
        let expected = [
            100,
            if write.is_some() { 80 } else { 0 },
            20,
            0,
            40,
            10,
            140,
        ];
        assert_eq!(
            totals(&db, &records[0]).await?,
            with_prewarm(DayTotals {
                measured: std::array::from_fn(|i| Metric {
                    known: expected[i],
                    unknown: i64::from(write.is_none() && (i == 1 || i == 3))
                }),
                known_usd: if write.is_some() {
                    "0.000644"
                } else {
                    "0.000484"
                }
                .to_string()
                .try_into()?,
                unknown_estimates: i64::from(write.is_none()),
                attempts: 1,
                ..Default::default()
            })
        );
        assert_eq!(turn_observations(&db).await?.len(), 1);
        counts(&gate, (1, 1, 1, 0));
        stop(&test).await;
    }
    Ok(())
}
#[tokio::test]
async fn accounting_responses_ws_native_prewarm_preconnect_and_cached_reuse() -> anyhow::Result<()>
{
    let server = MockServer::start().await;
    let mut gate = Gate::start().await?;
    let test = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
        .build_with_auto_env(&server)
        .await?;
    submit(&test).await?;
    let held = gate.next().await?;
    assert_eq!(held.body["previous_response_id"], "reused-provider-id");
    let db = test.codex.state_db().unwrap();
    // The turn has reported nothing yet; the startup prewarm already has.
    assert!(turn_observations(&db).await?.is_empty());
    held.complete().await?;
    terminal(&test).await?;
    assert_eq!(turn_observations(&db).await?.len(), 1);
    // The day holds the turn's 100 input tokens and the startup prewarm's 999.
    assert_eq!(
        totals(&db, &turn_attempts(&db).await?[0]).await?.measured[0].known,
        1099
    );
    counts(&gate, (1, 1, 1, 0));
    stop(&test).await;
    Ok(())
}
#[tokio::test]
async fn accounting_responses_ws_native_incremental_sampling_and_turn_identity()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    let mut gate = Gate::start().await?;
    let test = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
        .build_with_auto_env(&server)
        .await?;
    submit(&test).await?;
    let first = gate.next().await?;
    first
        .send(vec![
            core_test_support::responses::ev_response_created("tool"),
            core_test_support::responses::ev_shell_command_call("fixture-call", "echo fixture"),
            event("response.completed", usage(Some(0))),
        ])
        .await?;
    let second = gate.next().await?;
    assert_eq!(second.body["previous_response_id"], "reused-provider-id");
    assert!(
        second.body["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["type"] == "function_call_output")
    );
    second.complete().await?;
    terminal(&test).await?;
    let db = test.codex.state_db().unwrap();
    let records = turn_attempts(&db).await?;
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].turn, records[1].turn);
    assert_ne!(records[0].request_id, records[1].request_id);
    assert_eq!(records[1].retry_of, None);
    counts(&gate, (1, 1, 2, 0));
    stop(&test).await;
    Ok(())
}
#[tokio::test]
async fn accounting_responses_ws_native_upgrade_required_http_fallback() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(426))
        .mount(&server)
        .await;
    let mock = core_test_support::responses::mount_sse_once(
        &server,
        core_test_support::responses::sse(success(Some(0))),
    )
    .await;
    let endpoint = format!("{}/v1", server.uri());
    let test = builder(endpoint.clone(), enabled(&endpoint))
        .build_with_auto_env(&server)
        .await?;
    test.submit_turn("fixture").await?;
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.method.as_str() == "GET")
            .count(),
        1
    );
    assert_eq!(mock.requests().len(), 1);
    let db = test.codex.state_db().unwrap();
    assert_eq!(turn_attempts(&db).await?.len(), 1);
    stop(&test).await;
    Ok(())
}
#[tokio::test]
async fn accounting_responses_ws_native_ws_prefix_then_http_fallback() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let mut gate = Gate::start().await?;
    let test = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
        .build_with_auto_env(&server)
        .await?;
    submit(&test).await?;
    let first = gate.next().await?;
    first
        .send(vec![event("response.usage", usage(Some(0)))])
        .await?;
    let db = test.codex.state_db().unwrap();
    wait_turn_observations(&db, /*count*/ 1).await?;
    drop(first);
    let http = gate.next().await?;
    assert!(http.http);
    http.complete().await?;
    drop(http);
    terminal(&test).await?;
    let records = turn_attempts(&db).await?;
    assert_eq!(records.len(), 2);
    chain(&records);
    assert_eq!(
        totals(&db, &records[0]).await?.known_usd,
        // The two turn attempts, plus the startup prewarm's own $0.011988.
        "0.013276".to_string().try_into()?
    );
    counts(&gate, (1, 1, 1, 1));
    stop(&test).await;
    Ok(())
}
#[tokio::test]
async fn accounting_responses_ws_native_connection_limit_reconnect() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let mut gate = Gate::start().await?;
    let test = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
        .with_config(|config| config.model_provider.stream_max_retries = Some(1))
        .build_with_auto_env(&server)
        .await?;
    submit(&test).await?;
    let first = gate.next().await?;
    first.send(vec![json!({"type":"error","error":{"code":"websocket_connection_limit_reached","message":"fixture reconnect"}})]).await?;
    drop(first);
    let next = gate.next().await?;
    assert!(!next.http);
    next.complete().await?;
    terminal(&test).await?;
    let db = test.codex.state_db().unwrap();
    let records = turn_attempts(&db).await?;
    assert_eq!(records.len(), 2);
    chain(&records);
    assert_eq!(
        totals(&db, &records[0]).await?.unknown_estimates,
        1 + PREWARM_UNKNOWN_ESTIMATES
    );
    counts(&gate, (2, 1, 2, 0));
    stop(&test).await;
    Ok(())
}
#[tokio::test]
async fn accounting_responses_ws_native_previous_response_missing_full_retry() -> anyhow::Result<()>
{
    let server = MockServer::start().await;
    let mut gate = Gate::start().await?;
    let test = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
        .with_config(|config| config.model_provider.stream_max_retries = Some(1))
        .build_with_auto_env(&server)
        .await?;
    submit(&test).await?;
    let first = gate.next().await?;
    assert!(first.body["previous_response_id"].is_string());
    first.send(vec![json!({"type":"error","error":{"code":"previous_response_not_found","message":"fixture full retry"}})]).await?;
    drop(first);
    let next = gate.next().await?;
    assert!(!next.http);
    assert!(next.body["previous_response_id"].is_null());
    assert!(!next.body["input"].as_array().unwrap().is_empty());
    next.complete().await?;
    terminal(&test).await?;
    let records = turn_attempts(&test.codex.state_db().unwrap()).await?;
    assert_eq!(records.len(), 2);
    chain(&records);
    counts(&gate, (2, 1, 2, 0));
    stop(&test).await;
    Ok(())
}
#[tokio::test]
async fn accounting_responses_ws_native_fallback_http_transport_retry() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let mut gate = Gate::start().await?;
    let test = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
        .build_with_auto_env(&server)
        .await?;
    submit(&test).await?;
    drop(gate.next().await?);
    let failed = gate.next().await?;
    assert!(failed.http);
    failed.send(vec![json!({"fixture_status":503})]).await?;
    drop(failed);
    let http = gate.next().await?;
    assert!(http.http);
    http.complete().await?;
    drop(http);
    terminal(&test).await?;
    let db = test.codex.state_db().unwrap();
    let records = turn_attempts(&db).await?;
    assert_eq!(records.len(), 3);
    chain(&records);
    assert_eq!(
        totals(&db, &records[0]).await?.unknown_estimates,
        2 + PREWARM_UNKNOWN_ESTIMATES
    );
    counts(&gate, (1, 1, 1, 2));
    stop(&test).await;
    Ok(())
}
#[tokio::test]
async fn accounting_responses_ws_native_handshake_and_postdispatch_errors() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let endpoint = format!("{}/v1", server.uri());
    let test = builder(endpoint.clone(), enabled(&endpoint))
        .build_with_auto_env(&server)
        .await?;
    submit(&test).await?;
    terminal(&test).await?;
    let db = test.codex.state_db().unwrap();
    assert!(turn_attempts(&db).await?.is_empty());
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .all(|r| r.method.as_str() == "GET")
    );
    stop(&test).await;
    let mut gate = Gate::start().await?;
    let test = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
        .build_with_auto_env(&server)
        .await?;
    submit(&test).await?;
    gate.next().await?.send(vec![json!({"type":"error","error":{"code":"invalid_request_error","message":"fixture"},"status":400})]).await?;
    terminal(&test).await?;
    let db = test.codex.state_db().unwrap();
    assert_eq!(turn_attempts(&db).await?.len(), 1);
    assert!(turn_observations(&db).await?.is_empty());
    gate.no_pending().await;
    stop(&test).await;
    Ok(())
}
#[tokio::test]
async fn accounting_responses_ws_native_redirects_are_never_attributed() -> anyhow::Result<()> {
    for status in [301, 302, 303, 307, 308] {
        for fallback in [false, true] {
            let origin = MockServer::start().await;
            let target = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(
                    ResponseTemplate::new(if fallback { 426 } else { status })
                        .insert_header("location", format!("{}/v1/responses", target.uri())),
                )
                .mount(&origin)
                .await;
            Mock::given(method("POST"))
                .respond_with(
                    ResponseTemplate::new(status)
                        .insert_header("location", format!("{}/v1/responses", target.uri())),
                )
                .mount(&origin)
                .await;
            let endpoint = format!("{}/v1", origin.uri());
            let test = builder(endpoint.clone(), enabled(&endpoint))
                .build_with_auto_env(&origin)
                .await?;
            submit(&test).await?;
            let events = terminal(&test).await?;
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, EventMsg::Error(_)))
            );
            // A websocket handshake never follows a redirect, recorded or not.
            // An HTTP fallback that is redirected is resent once, unrecorded,
            // on the ordinary client, which follows it to the target.
            let target_requests = target.received_requests().await.unwrap().len();
            let requests = origin.received_requests().await.unwrap();
            let handshakes = requests
                .iter()
                .filter(|r| r.method.as_str() == "GET")
                .count();
            let posts = requests
                .iter()
                .filter(|r| r.method.as_str() == "POST")
                .count();
            eprintln!(
                "redirect status={status} fallback={fallback}: handshakes={handshakes}, posts={posts}, target={target_requests}"
            );
            assert_eq!(target_requests, usize::from(fallback));
            assert_eq!(posts, 2 * usize::from(fallback));
            let db = test.codex.state_db().unwrap();
            assert_eq!(turn_attempts(&db).await?.len(), usize::from(fallback));
            stop(&test).await;
        }
    }
    Ok(())
}
#[tokio::test]
async fn accounting_responses_ws_native_endpoint_is_unrecorded_and_a_switched_auth_reconnects()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    let mut gate = Gate::start().await?;
    let test = builder(
        gate.endpoint.clone(),
        enabled(&format!("{}/wrong", gate.endpoint)),
    )
    .build_with_auto_env(&server)
    .await?;
    submit(&test).await?;
    gate.next().await?.complete().await?;
    core_test_support::assert_accounting_gap(&terminal(&test).await?);
    gate.no_pending().await;
    absent(&test.codex.state_db().unwrap()).await?;
    assert_eq!(gate.counts.lock().unwrap().2, 1);
    stop(&test).await;

    let mut gate = Gate::start().await?;
    gate.hold_warmup
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let subscription = codex_login::CodexAuth::from_external_chatgpt_tokens(
        "header.e30.synthetic",
        "synthetic-account",
        /*chatgpt_plan_type*/ None,
    )?;
    let test = builder(gate.endpoint.clone(), enabled(&gate.endpoint))
        .with_auth(subscription)
        .build_with_auto_env(&server)
        .await?;
    submit(&test).await?;
    let warmup = gate.next().await?;
    assert_eq!(warmup.body["generate"], false);
    test.thread_manager
        .auth_manager()
        .set_external_auth(std::sync::Arc::new(StaticAuth(
            codex_login::CodexAuth::from_api_key("synthetic-ws-switch"),
        )))
        .await?;
    warmup.complete().await?;
    gate.next().await?.complete().await?;
    // #1311 (D171): the request is never sent on the connection opened with
    // the subscription's tokens; a new one is opened with the API key, so the
    // request is attributable and recorded, with no accounting gap.
    let events = terminal(&test).await?;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, EventMsg::Error(_))
                || matches!(
                    event,
                    EventMsg::Warning(warning)
                        if warning.message.starts_with("Developer accounting could not record")
                )),
        "{events:?}"
    );
    counts(&gate, (2, 1, 1, 0));
    assert_eq!(
        turn_attempts(&test.codex.state_db().unwrap()).await?.len(),
        1
    );
    stop(&test).await;
    Ok(())
}
