#[path = "accounting_responses_support.rs"]
pub(super) mod support;
use codex_core::config::AccountingMode;
use codex_core::config::PriceAuthority;
use codex_protocol::protocol::EventMsg;
use codex_state::accounting::*;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use support::*;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[tokio::test]
async fn accounting_chatgpt_subscription_off_route_collects_without_economics() -> anyhow::Result<()>
{
    let server = MockServer::start().await;
    let endpoint = format!("{}/v1", server.uri());
    let mock = responses::mount_sse_once(&server, success(usage(Some(0)))).await;
    let mode = AccountingMode::Provider {
        scope: uuid::Uuid::new_v4(),
        provider_id: "openai".into(),
        wire_api: codex_model_provider_info::WireApi::Responses,
        approved_endpoint: endpoint.clone(),
        approved_query: None,
        pricing: PriceAuthority::Unavailable,
        basis_source: Default::default(),
        account: None,
    };
    let test = builder(endpoint, mode)
        .with_auth(codex_login::CodexAuth::from_external_chatgpt_tokens(
            "header.e30.synthetic",
            "synthetic-account",
            /*chatgpt_plan_type*/ None,
        )?)
        .build_with_auto_env(&server)
        .await?;
    test.submit_turn("subscription accounting").await?;
    let db = test.codex.state_db().unwrap();
    let records = wait_attempts(&db, /*count*/ 1).await?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].provider, "openai");
    // This fixture talks to a wiremock endpoint, which is nobody's own route.
    // Tokens are still collected; no economics of either kind are claimed for a
    // destination no table declares: it is bound as not declared.
    let prices: Vec<Snapshot> = payloads(&db, "draft_accounting_price_snapshots").await?;
    assert_eq!(
        prices.iter().map(|p| p.basis).collect::<Vec<_>>(),
        vec![codex_state::accounting::Basis::Undeclared]
    );
    wait_observations(&db, /*count*/ 1).await?;
    let total = totals(&db, &records[0]).await?;
    assert_eq!(total.measured[0].known, 100);
    assert_eq!(total.known_usd, Decimal::default());
    assert_eq!(total.unknown_estimates, 1);
    assert_eq!(total.equivalent_usd, Decimal::default());
    assert_eq!(total.plan_attempts, 0);
    assert_eq!(total.undeclared_attempts, 1);
    assert_eq!(mock.requests().len(), 1);
    stop(&test).await;
    Ok(())
}

#[tokio::test]
async fn accounting_responses_native_off_and_unsupported() -> anyhow::Result<()> {
    for variant in 0..4 {
        let server = MockServer::start().await;
        let endpoint = format!("{}/v1", server.uri());
        let mock = responses::mount_sse_once(&server, success(usage(Some(0)))).await;
        let mode = if variant < 2 {
            AccountingMode::Disabled
        } else {
            enabled(&endpoint)
        };
        let test = builder(endpoint, mode)
            .with_config(move |config| {
                if variant == 1 {
                    config
                        .features
                        .disable(codex_features::Feature::Sqlite)
                        .unwrap();
                }
                if variant == 2 {
                    config.model_provider_id = "compatible-fixture".into();
                }
                if variant == 3 {
                    config.model_provider.experimental_bearer_token =
                        Some("synthetic-override".into());
                }
            })
            .build_with_auto_env(&server)
            .await?;
        test.submit_turn("fixture").await?;
        assert_eq!(mock.requests().len(), 1);
        if let Some(db) = test.codex.state_db() {
            absent(&db).await?;
        }
        stop(&test).await;
    }
    Ok(())
}

#[tokio::test]
async fn accounting_responses_native_complete_and_partial_goldens() -> anyhow::Result<()> {
    for write in [Some(0), None] {
        let server = MockServer::start().await;
        let endpoint = format!("{}/v1", server.uri());
        let mock = responses::mount_sse_once(&server, success(usage(write))).await;
        let test = builder(endpoint.clone(), enabled(&endpoint))
            .build_with_auto_env(&server)
            .await?;
        test.submit_turn("fixture").await?;
        let db = test.codex.state_db().unwrap();
        let records = attempts(&db).await?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].thread_id, test.session_configured.thread_id);
        assert_eq!(
            (&records[0].provider, &records[0].model, records[0].dialect),
            (
                &"openai".to_string(),
                &"gpt-5.6-terra".to_string(),
                Dialect::Inclusive
            )
        );
        let request = mock.single_request();
        assert_eq!(request.path(), "/v1/responses");
        assert_eq!(request.body_json()["model"], "gpt-5.6-terra");
        let expected = [
            100,
            if write.is_some() { 80 } else { 0 },
            20,
            0,
            40,
            10,
            140,
        ];
        let actual = totals(&db, &records[0]).await?;
        assert_eq!(
            actual,
            DayTotals {
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
            }
        );
        let evidence = observations(&db).await?;
        assert_eq!(evidence.len(), 1);
        assert_eq!(
            evidence[0].patch.write,
            write.map_or(Presence::Missing, |_| Presence::Number(
                0.try_into().unwrap()
            ))
        );
        let prices: Vec<Snapshot> = payloads(&db, "draft_accounting_price_snapshots").await?;
        assert_eq!(prices.len(), 1);
        assert_eq!(prices[0].provider, "openai");
        stop(&test).await;
    }
    Ok(())
}

/// #361: an OpenAI API-key conversation on GPT-5.6 Luna, priced at the
/// Standard rates OpenAI publishes (developers.openai.com/api/docs/pricing,
/// read 2026-10-09): $0.20 input, $0.02 cached input, $0.25 cache writes and
/// $1.20 output per million tokens. The usage is the PF-60-S05 re-run's
/// `captures/mac/mac-r1-t1-conv.txt`, whose published-rate recompute is
/// $0.00625965: 9,594 input tokens, then 3 uncached input + 17,337 cache
/// writes + 5 output tokens.
#[tokio::test]
async fn accounting_responses_luna_prices_cache_writes_at_the_published_rate() -> anyhow::Result<()>
{
    let server = MockServer::start().await;
    let endpoint = format!("{}/v1", server.uri());
    let first = json!({"input_tokens":9594,"input_tokens_details":{"cached_tokens":0,"cache_write_tokens":0},
        "output_tokens":0,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":9594});
    let second = json!({"input_tokens":17340,"input_tokens_details":{"cached_tokens":0,"cache_write_tokens":17337},
        "output_tokens":5,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":17345});
    let mock = responses::mount_sse_sequence(&server, vec![success(first), success(second)]).await;
    let test = builder(endpoint.clone(), enabled(&endpoint))
        .with_config(|config| config.model = Some("gpt-5.6-luna".into()))
        .build_with_auto_env(&server)
        .await?;
    test.submit_turn("first").await?;
    test.submit_turn("second").await?;
    assert_eq!(mock.requests().len(), 2);
    let db = test.codex.state_db().unwrap();
    let records = attempts(&db).await?;
    assert_eq!(records.len(), 2);
    let prices: Vec<Snapshot> = payloads(&db, "draft_accounting_price_snapshots").await?;
    assert_eq!(prices.len(), 2);
    for price in &prices {
        assert_eq!(
            price.rates,
            Rates {
                noncached: Some("0.2".to_string().try_into()?),
                read: Some("0.02".to_string().try_into()?),
                write: Some("0.25".to_string().try_into()?),
                output: Some("1.2".to_string().try_into()?),
            }
        );
    }
    let total = totals(&db, &records[0]).await?;
    assert_eq!(total.attempts, 2);
    assert_eq!(total.unknown_estimates, 0);
    // 9594*0.20 + 3*0.20 + 17337*0.25 + 5*1.20 per million tokens.
    assert_eq!(total.known_usd, "0.00625965".to_string().try_into()?);
    stop(&test).await;
    Ok(())
}

/// #361: above 272K input tokens OpenAI prices the whole GPT-5.6 request at
/// 2x input, cache and cache-write rates and 1.5x output. The tier needs
/// ledger format 3, which the first such record upgrades the ledger to.
#[tokio::test]
async fn accounting_responses_long_context_request_prices_at_the_long_context_rates()
-> anyhow::Result<()> {
    let server = MockServer::start().await;
    let endpoint = format!("{}/v1", server.uri());
    let usage = |input: i64| {
        json!({"input_tokens":input,"input_tokens_details":{"cached_tokens":200000,"cache_write_tokens":1000},
            "output_tokens":100,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":input + 100})
    };
    let mock = responses::mount_sse_sequence(
        &server,
        vec![success(usage(272_000)), success(usage(272_001))],
    )
    .await;
    let test = builder(endpoint.clone(), enabled(&endpoint))
        .with_config(|config| config.model = Some("gpt-5.6-luna".into()))
        .build_with_auto_env(&server)
        .await?;
    test.submit_turn("at the threshold").await?;
    let db = test.codex.state_db().unwrap();
    let records = attempts(&db).await?;
    // At the threshold: 71000*0.20 + 200000*0.02 + 1000*0.25 + 100*1.20.
    assert_eq!(
        totals(&db, &records[0]).await?.known_usd,
        "0.01857".to_string().try_into()?
    );
    test.submit_turn("above the threshold").await?;
    assert_eq!(mock.requests().len(), 2);
    let records = attempts(&db).await?;
    assert_eq!(records.len(), 2);
    // Above: 71001*0.40 + 200000*0.04 + 1000*0.50 + 100*1.80 (0.0370804), plus the first.
    let total = totals(&db, &records[0]).await?;
    assert_eq!(total.unknown_estimates, 0);
    assert_eq!(total.known_usd, "0.0556504".to_string().try_into()?);
    let mut conn = connection(&db).await?;
    let format: i64 = sqlx::query_scalar("SELECT count(*) FROM _accounting_migrations")
        .fetch_one(&mut conn)
        .await?;
    assert_eq!(format, 3);
    stop(&test).await;
    Ok(())
}

#[tokio::test]
async fn accounting_responses_native_separate_and_terminal_usage() -> anyhow::Result<()> {
    for kind in [
        "response.completed",
        "response.failed",
        "response.incomplete",
    ] {
        let server = MockServer::start().await;
        let endpoint = format!("{}/v1", server.uri());
        let payload = responses::sse(vec![
            event("response.usage", json!({"input_tokens":7})),
            event(kind, json!({"output_tokens":3})),
        ]);
        let mock = responses::mount_sse_once(&server, payload).await;
        let test = builder(endpoint.clone(), enabled(&endpoint))
            .with_config(|config| config.model_provider.stream_max_retries = Some(0))
            .build_with_auto_env(&server)
            .await?;
        submit(&test).await?;
        let output = terminal(&test).await?;
        let db = test.codex.state_db().unwrap();
        assert_eq!(mock.requests().len(), 1);
        let patches = observations(&db).await?;
        assert_eq!(patches.len(), 2);
        assert_eq!(patches[0].source, patches[1].source);
        assert_eq!(patches[0].patch.input, Presence::Number(7.try_into()?));
        assert_eq!(patches[1].patch.output, Presence::Number(3.try_into()?));
        assert!(output.iter().any(|e| matches!(e, EventMsg::Error(_))));
        stop(&test).await;
    }
    Ok(())
}

#[tokio::test]
async fn accounting_responses_native_unknown_prices_and_tiers() -> anyhow::Result<()> {
    for (model, tier) in [
        ("codex-auto-review", None),
        ("remote-only-fixture", None),
        ("gpt-5.6-sol", Some("priority")),
    ] {
        let server = MockServer::start().await;
        let endpoint = format!("{}/v1", server.uri());
        let mock = responses::mount_sse_once(&server, success(usage(Some(0)))).await;
        let test = builder(endpoint.clone(), enabled(&endpoint))
            .with_config(move |config| config.model = Some(model.into()))
            .build_with_auto_env(&server)
            .await?;
        test.submit_turn_with_service_tier("fixture", tier).await?;
        let db = test.codex.state_db().unwrap();
        let records = attempts(&db).await?;
        assert_eq!(mock.single_request().body_json()["model"], model);
        assert_eq!(records.len(), 1);
        // No price: the attempt is bound only to its pay-per-use basis, by a
        // record with no rates (PF-60-S05).
        let bound: Vec<Snapshot> = payloads(&db, "draft_accounting_price_snapshots").await?;
        assert!(!bound.is_empty());
        assert!(bound.iter().all(|snapshot| snapshot.basis
            == codex_state::accounting::Basis::Billed
            && snapshot.rates == codex_state::accounting::Rates::default()));
        let before = totals(&db, &records[0]).await?;
        assert_eq!(
            before.measured[6],
            Metric {
                known: 140,
                unknown: 0
            }
        );
        assert_eq!(before.unknown_estimates, 1);
        let home = test.home.clone();
        let rollout = test.codex.rollout_path().unwrap();
        stop(&test).await;
        drop(test);
        let reopened = builder(endpoint.clone(), enabled(&endpoint))
            .resume(&server, home, rollout)
            .await?;
        assert_eq!(
            totals(&reopened.codex.state_db().unwrap(), &records[0]).await?,
            before
        );
        assert_eq!(mock.requests().len(), 1);
        stop(&reopened).await;
    }
    Ok(())
}

#[tokio::test]
async fn accounting_responses_native_endpoint_mismatch_is_sent_unrecorded() -> anyhow::Result<()> {
    for suffix in ["/wrong", "?unexpected=1", ":1234", "/v1"] {
        let server = MockServer::start().await;
        let endpoint = format!("{}/v1", server.uri());
        let mock = responses::mount_sse_once(&server, success(usage(Some(0)))).await;
        let test = builder(endpoint.clone(), enabled(&format!("{endpoint}{suffix}")))
            .build_with_auto_env(&server)
            .await?;
        submit(&test).await?;
        core_test_support::assert_accounting_gap(&terminal(&test).await?);
        assert_eq!(mock.requests().len(), 1);
        absent(&test.codex.state_db().unwrap()).await?;
        stop(&test).await;
    }
    Ok(())
}

#[tokio::test]
async fn accounting_responses_native_no_usage_is_unknown() -> anyhow::Result<()> {
    for payload in [
        success(Value::Null),
        responses::sse(vec![responses::ev_response_created("reused-provider-id")]),
    ] {
        let server = MockServer::start().await;
        let endpoint = format!("{}/v1", server.uri());
        let mock = responses::mount_sse_once(&server, payload).await;
        let test = builder(endpoint.clone(), enabled(&endpoint))
            .with_config(|config| config.model_provider.stream_max_retries = Some(0))
            .build_with_auto_env(&server)
            .await?;
        submit(&test).await?;
        terminal(&test).await?;
        let db = test.codex.state_db().unwrap();
        assert_eq!(mock.requests().len(), 1);
        let records = attempts(&db).await?;
        assert_eq!(records.len(), 1);
        assert!(observations(&db).await?.is_empty());
        assert_eq!(
            totals(&db, &records[0]).await?,
            DayTotals {
                measured: std::array::from_fn(|_| Metric {
                    known: 0,
                    unknown: 1
                }),
                known_usd: "0".to_string().try_into()?,
                unknown_estimates: 1,
                attempts: 1,
                ..Default::default()
            }
        );
        stop(&test).await;
    }
    Ok(())
}

#[tokio::test]
async fn accounting_responses_native_ws_fallback_http_segment() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let endpoint = format!("{}/v1", server.uri());
    Mock::given(method("GET"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(426))
        .mount(&server)
        .await;
    let mock = responses::mount_sse_once(&server, success(usage(Some(0)))).await;
    let test = builder(endpoint.clone(), enabled(&endpoint))
        .with_config(|config| config.model_provider.supports_websockets = true)
        .build_with_auto_env(&server)
        .await?;
    test.submit_turn("fixture").await?;
    assert_eq!(mock.requests().len(), 1);
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.method.as_str() == "GET")
            .count(),
        1
    );
    let db = test.codex.state_db().unwrap();
    assert_eq!(attempts(&db).await?.len(), 1);
    assert_eq!(observations(&db).await?.len(), 1);
    stop(&test).await;
    Ok(())
}

#[tokio::test]
async fn accounting_responses_native_sampling_and_auxiliary_scope() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let endpoint = format!("{}/v1", server.uri());
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("tools"),
                responses::ev_shell_command_call("shell-fixture", "echo accounting"),
                event("response.completed", usage(Some(0))),
            ]),
            success(usage(Some(0))),
        ],
    )
    .await;
    let compact = responses::mount_compact_json_once(
        &server,
        json!({"output":[{
            "type":"compaction","encrypted_content":"synthetic-summary"
        }]}),
    )
    .await;
    let test = builder(endpoint.clone(), enabled(&endpoint))
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
    test.submit_turn("fixture").await?;
    let db = test.codex.state_db().unwrap();
    let records = attempts(&db).await?;
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].turn, records[1].turn);
    assert_ne!(records[0].request_id, records[1].request_id);
    assert_eq!(records[1].retry_of, None);
    assert_eq!(mock.requests().len(), 2);
    test.codex
        .submit(codex_protocol::protocol::Op::Compact)
        .await?;
    terminal(&test).await?;
    assert_eq!(compact.single_request().path(), "/v1/responses/compact");
    // The legacy compaction endpoint records too, under its own `compact:`
    // turn. This assertion used to pin the opposite.
    let after = wait_attempts(&db, /*count*/ 3).await?;
    assert_eq!(after[..2], records[..]);
    assert!(after[2].turn.starts_with("compact:"), "{}", after[2].turn);
    // That fixture's compact body states no usage, so the compaction records an
    // attempt with its tokens unknown rather than a third observation.
    assert_eq!(observations(&db).await?.len(), 2);
    stop(&test).await;
    Ok(())
}

/// The ledger is written by the collector, not by the event stream, so reading a
/// count the instant a turn completes is a race: observed failing under the
/// parallel full-suite run and passing in isolation. Wait for the ledger rather
/// than for the protocol, and name what was found when the wait runs out.
async fn wait_attempts(
    db: &codex_state::StateRuntime,
    count: usize,
) -> anyhow::Result<Vec<Attempt>> {
    let mut seen = Vec::new();
    let waited = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            seen = attempts(db).await?;
            if seen.len() >= count {
                return anyhow::Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    if waited.is_err() {
        anyhow::bail!(
            "waited for {count} attempts, ledger holds {:?}",
            seen.iter().map(|record| &record.turn).collect::<Vec<_>>()
        );
    }
    waited??;
    Ok(seen)
}

/// A remote-compaction-v2 response: the compaction item plus terminal usage.
fn compaction(usage: Value) -> String {
    responses::sse(vec![
        responses::ev_response_created("compaction-response"),
        json!({"type":"response.output_item.done",
            "item":{"type":"compaction","encrypted_content":"synthetic-compaction"}}),
        json!({"type":"response.completed",
            "response":{"id":"compaction-response","usage":usage}}),
    ])
}

/// An operator `/compact` is inference they paid for, so it must land in the
/// ledger under its own turn identity rather than escaping collection.
#[tokio::test]
async fn accounting_records_manual_compaction() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let endpoint = format!("{}/v1", server.uri());
    let mock = responses::mount_sse_sequence(
        &server,
        vec![success(usage(Some(0))), compaction(usage(Some(0)))],
    )
    .await;
    let test = builder(endpoint.clone(), enabled(&endpoint))
        .with_config(|config| {
            config
                .features
                .disable(codex_features::Feature::TokenBudget)
                .unwrap();
            config
                .features
                .enable(codex_features::Feature::RemoteCompactionV2)
                .unwrap();
        })
        .build_with_auto_env(&server)
        .await?;
    test.submit_turn("fixture").await?;
    let db = test.codex.state_db().unwrap();
    let turn = wait_attempts(&db, /*count*/ 1).await?;
    assert_eq!(turn.len(), 1);
    // The day total is thread-wide, so it reads the turn's tokens alone here.
    wait_observations(&db, /*count*/ 1).await?;
    assert_eq!(totals(&db, &turn[0]).await?.measured[0].known, 100);
    test.codex
        .submit(codex_protocol::protocol::Op::Compact)
        .await?;
    terminal(&test).await?;
    let records = wait_attempts(&db, /*count*/ 2).await?;
    assert_eq!(records.len(), 2);
    let compaction = &records[1];
    assert!(
        compaction.turn.starts_with("compact:"),
        "compaction recorded under {}",
        compaction.turn
    );
    assert_ne!(compaction.turn, turn[0].turn);
    // The compaction's own tokens are what moved the day total from 100 to 200.
    wait_observations(&db, /*count*/ 2).await?;
    assert_eq!(totals(&db, compaction).await?.measured[0].known, 200);
    assert_eq!(mock.requests().len(), 2);
    stop(&test).await;
    Ok(())
}

/// Auto-compaction runs on the turn's own client session rather than a session
/// of its own. Collecting only the standalone shape - the first version of this
/// - left every automatic compaction unrecorded while the record claimed
/// otherwise, so the borrowed shape is proven here too.
#[tokio::test]
async fn accounting_records_auto_compaction() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let endpoint = format!("{}/v1", server.uri());
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            success(usage(Some(0))),
            compaction(usage(Some(0))),
            success(usage(Some(0))),
        ],
    )
    .await;
    let test = builder(endpoint.clone(), enabled(&endpoint))
        .with_config(|config| {
            // The first turn reports 140 total tokens, so the next turn compacts
            // before it runs.
            config.model_auto_compact_token_limit = Some(50);
            config
                .features
                .disable(codex_features::Feature::TokenBudget)
                .unwrap();
            config
                .features
                .enable(codex_features::Feature::RemoteCompactionV2)
                .unwrap();
        })
        .build_with_auto_env(&server)
        .await?;
    test.submit_turn("fixture").await?;
    test.submit_turn("fixture after compaction").await?;
    let db = test.codex.state_db().unwrap();
    let records = wait_attempts(&db, /*count*/ 3).await?;
    assert_eq!(records.len(), 3);
    assert_eq!(mock.requests().len(), 3);
    let compactions: Vec<&Attempt> = records
        .iter()
        .filter(|record| record.turn.starts_with("compact:"))
        .collect();
    assert_eq!(
        compactions.len(),
        1,
        "turns recorded: {:?}",
        records
            .iter()
            .map(|record| &record.turn)
            .collect::<Vec<_>>()
    );
    // Thread-wide day total: both turns and the compaction between them.
    wait_observations(&db, /*count*/ 3).await?;
    assert_eq!(totals(&db, compactions[0]).await?.measured[0].known, 300);
    stop(&test).await;
    Ok(())
}

#[tokio::test]
async fn accounting_responses_native_ws_only_no_install() -> anyhow::Result<()> {
    let server = responses::start_websocket_server(vec![vec![
        vec![
            responses::ev_response_created("warm"),
            responses::ev_completed("warm"),
        ],
        vec![
            responses::ev_response_created("fixture"),
            responses::ev_assistant_message("m", "fixture"),
            event("response.completed", usage(Some(0))),
        ],
    ]])
    .await;
    let mut builder = builder(
        "http://127.0.0.1:1/v1".into(),
        enabled("https://api.openai.com/v1"),
    )
    .with_config(|config| config.model_provider.supports_websockets = true);
    let test = builder.build_with_websocket_server(&server).await?;
    test.submit_turn("fixture").await?;
    assert_eq!(server.handshakes().len(), 1);
    assert_eq!(server.single_connection().len(), 2);
    absent(&test.codex.state_db().unwrap()).await?;
    stop(&test).await;
    server.shutdown().await;
    Ok(())
}

/// A registered agent identity is a credential, not a reason to stop counting.
/// The client used to exclude these sessions outright, so an agent-identity run
/// recorded nothing at all on any provider while the policy that decides its
/// economics already treated it as subscription capacity.
#[tokio::test]
async fn accounting_agent_identity_session_collects() -> anyhow::Result<()> {
    let key = codex_agent_identity::generate_agent_key_material()?;
    let record = codex_login::auth::AgentIdentityAuthRecord {
        agent_runtime_id: "fixture-runtime".into(),
        agent_private_key: key.private_key_pkcs8_base64,
        account_id: "fixture-account".into(),
        chatgpt_user_id: "fixture-user".into(),
        email: None,
        plan_type: codex_protocol::account::PlanType::Pro,
        chatgpt_account_is_fedramp: false,
        // Already registered: construction must not reach the network.
        task_id: Some("fixture-task".into()),
    };
    let identity = codex_login::auth::AgentIdentityAuth::from_record(
        record,
        "https://agent-identity.invalid",
        &codex_login::AuthRouteConfig::from_http_client_factory(
            codex_http_client::HttpClientFactory::new(
                codex_http_client::OutboundProxyPolicy::ReqwestDefault,
            ),
        ),
    )
    .await?;
    let auth = codex_login::CodexAuth::AgentIdentity(identity);
    assert_eq!(
        auth.auth_mode(),
        codex_protocol::auth::AuthMode::AgentIdentity
    );

    let server = MockServer::start().await;
    let endpoint = format!("{}/v1", server.uri());
    let mock = responses::mount_sse_once(&server, success(usage(Some(0)))).await;
    let mode = AccountingMode::Provider {
        scope: uuid::Uuid::new_v4(),
        provider_id: "openai".into(),
        wire_api: codex_model_provider_info::WireApi::Responses,
        approved_endpoint: endpoint.clone(),
        approved_query: None,
        pricing: PriceAuthority::Unavailable,
        basis_source: Default::default(),
        account: None,
    };
    let test = builder(endpoint.clone(), mode)
        .with_auth(auth)
        .build_with_auto_env(&server)
        .await?;
    test.submit_turn("agent identity accounting").await?;
    let db = test.codex.state_db().unwrap();
    let records = wait_attempts(&db, /*count*/ 1).await?;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].provider, "openai");
    wait_observations(&db, /*count*/ 1).await?;
    assert_eq!(totals(&db, &records[0]).await?.measured[0].known, 100);
    assert_eq!(mock.requests().len(), 1);
    stop(&test).await;
    Ok(())
}

/// The legacy compaction endpoint answers with one JSON body rather than a
/// stream, so it never met the streaming collector. It is reachable by turning
/// `remote_compaction_v2` off - a Stable, default-on feature - and every such
/// compaction was paid for and recorded nowhere.
#[tokio::test]
async fn accounting_records_legacy_compaction() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let endpoint = format!("{}/v1", server.uri());
    let mock = responses::mount_sse_once(&server, success(usage(Some(0)))).await;
    let compact = responses::mount_compact_json_once(
        &server,
        json!({
            "output":[{"type":"compaction","encrypted_content":"synthetic-summary"}],
            "usage":{"input_tokens":100,"input_tokens_details":{"cached_tokens":20},
                "output_tokens":40,"output_tokens_details":{"reasoning_tokens":10},
                "total_tokens":140}
        }),
    )
    .await;
    let test = builder(endpoint.clone(), enabled(&endpoint))
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
    test.submit_turn("fixture").await?;
    let db = test.codex.state_db().unwrap();
    let turn = wait_attempts(&db, /*count*/ 1).await?;
    test.codex
        .submit(codex_protocol::protocol::Op::Compact)
        .await?;
    terminal(&test).await?;
    let records = wait_attempts(&db, /*count*/ 2).await?;
    assert_eq!(records.len(), 2);
    let compaction = &records[1];
    assert!(
        compaction.turn.starts_with("compact:"),
        "legacy compaction recorded under {}",
        compaction.turn
    );
    assert_ne!(compaction.turn, turn[0].turn);
    assert_eq!(compact.single_request().path(), "/v1/responses/compact");
    // The endpoint's own body carried the numbers, so they are recorded: the
    // turn's 100 input tokens plus the compaction's own 100.
    wait_observations(&db, /*count*/ 2).await?;
    assert_eq!(totals(&db, compaction).await?.measured[0].known, 200);
    assert_eq!(mock.requests().len(), 1);
    stop(&test).await;
    Ok(())
}

/// A collected request never follows a redirect on its recording client: a
/// response from somewhere else would be attributed to the approved endpoint.
/// Accounting never blocks the request either, so a redirected compaction is
/// resent once, unrecorded, on the ordinary client, and the turn says so.
#[tokio::test]
async fn accounting_legacy_compaction_redirect_is_resent_unrecorded() -> anyhow::Result<()> {
    for status in [301, 302, 303, 307, 308] {
        let origin = MockServer::start().await;
        let target = MockServer::start().await;
        let endpoint = format!("{}/v1", origin.uri());
        let turn = responses::mount_sse_once(&origin, success(usage(Some(0)))).await;
        Mock::given(method("POST"))
            .and(path("/v1/responses/compact"))
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header("location", format!("{}/v1/responses/compact", target.uri())),
            )
            .mount(&origin)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/responses/compact"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "output":[{"type":"compaction","encrypted_content":"elsewhere"}]
            })))
            .mount(&target)
            .await;
        let test = builder(endpoint.clone(), enabled(&endpoint))
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
            .build_with_auto_env(&origin)
            .await?;
        test.submit_turn("fixture").await?;
        let db = test.codex.state_db().unwrap();
        wait_attempts(&db, /*count*/ 1).await?;
        test.codex
            .submit(codex_protocol::protocol::Op::Compact)
            .await?;
        let events = terminal(&test).await?;
        assert!(
            events
                .iter()
                .any(|event| matches!(event, EventMsg::Warning(warning)
                if warning.message.starts_with("Developer accounting could not record"))),
            "{status}: {events:?}"
        );
        assert_eq!(
            target.received_requests().await.unwrap().len(),
            1,
            "{status}: the unrecorded resend follows the redirect once"
        );
        assert_eq!(turn.requests().len(), 1);
        // Only the turn's request and the refused compaction send were admitted.
        assert_eq!(attempts(&db).await?.len(), 2);
        stop(&test).await;
    }
    Ok(())
}

/// PF-60-S05 AC7: compaction is best effort. With the ledger refusing the
/// admission, or refusing the usage after the paid response arrived, the
/// compaction still finishes, the provider's answer is used, and the turn
/// warns once that a request went unrecorded.
#[tokio::test]
async fn accounting_store_failure_never_fails_compaction() -> anyhow::Result<()> {
    for (table, recorded) in [
        ("draft_accounting_attempts", 1),
        ("draft_accounting_observations", 2),
    ] {
        let server = MockServer::start().await;
        let endpoint = format!("{}/v1", server.uri());
        responses::mount_sse_once(&server, success(usage(Some(0)))).await;
        let compact = responses::mount_compact_json_once(
            &server,
            json!({
                "output":[{"type":"compaction","encrypted_content":"synthetic-summary"}],
                "usage":{"input_tokens":100,"output_tokens":40,"total_tokens":140}
            }),
        )
        .await;
        let test = builder(endpoint.clone(), enabled(&endpoint))
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
        test.submit_turn("fixture").await?;
        let db = test.codex.state_db().unwrap();
        wait_attempts(&db, /*count*/ 1).await?;
        let mut conn = connection(&db).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE TRIGGER refuse_{table} BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT, 'fixture-store-refusal'); END"
        )))
        .execute(&mut conn)
        .await?;
        test.codex
            .submit(codex_protocol::protocol::Op::Compact)
            .await?;
        let events = terminal(&test).await?;
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, EventMsg::Warning(warning)
                    if warning.message.starts_with("Developer accounting could not record")))
                .count(),
            1,
            "{table}: {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, EventMsg::Error(_))),
            "{table}: {events:?}"
        );
        assert_eq!(compact.single_request().path(), "/v1/responses/compact");
        assert_eq!(attempts(&db).await?.len(), recorded, "{table}");
        sqlx::Connection::close(conn).await?;
        stop(&test).await;
    }
    Ok(())
}

/// #314 (D228, Travis 2026-10-28): a turn whose request the ledger refuses
/// still runs and warns, and leaves a durable marker for its conversation's
/// UTC day, so `/cost` says that day's totals may be incomplete rather than
/// showing the turns it did record as the whole day.
#[tokio::test]
async fn accounting_unrecorded_turn_marks_its_day_incomplete() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let endpoint = format!("{}/v1", server.uri());
    let mock = responses::mount_sse_repeating(&server, success(usage(Some(0)))).await;
    let test = builder(endpoint.clone(), enabled(&endpoint))
        .build_with_auto_env(&server)
        .await?;
    test.submit_turn("recorded").await?;
    let db = test.codex.state_db().unwrap();
    wait_attempts(&db, /*count*/ 1).await?;
    let owner = test.session_configured.thread_id;
    let day = |db: std::sync::Arc<codex_state::StateRuntime>| async move {
        let now = chrono::Utc::now().timestamp_millis();
        match AccountingStore::inspect_day(&db, owner, now / 86_400_000, now).await? {
            InspectionDay::Ready(view) => anyhow::Ok(view),
            other => anyhow::bail!("{other:?}"),
        }
    };
    let view = day(db.clone()).await?;
    assert!(!view.recording_gap, "every request so far was recorded");

    let mut conn = connection(&db).await?;
    sqlx::raw_sql(
        "CREATE TRIGGER refuse_attempts BEFORE INSERT ON draft_accounting_attempts
            BEGIN SELECT RAISE(ABORT, 'fixture-store-refusal'); END",
    )
    .execute(&mut conn)
    .await?;
    submit(&test).await?;
    let events = terminal(&test).await?;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, EventMsg::Warning(warning)
            if warning.message.starts_with("Developer accounting could not record"))),
        "{events:?}"
    );
    assert_eq!(mock.requests().len(), 2, "the unrecorded turn still ran");
    let marked: Vec<(String, i64)> =
        sqlx::query_as("SELECT thread_id, utc_day FROM draft_accounting_gap_days")
            .fetch_all(&mut conn)
            .await?;
    // The days the turn ran on (one, unless it crossed midnight UTC).
    let today = chrono::Utc::now().timestamp_millis() / 86_400_000;
    assert!(
        marked
            .iter()
            .all(|(thread, _)| *thread == owner.to_string())
            && marked.iter().any(|(_, day)| *day == today),
        "{marked:?}"
    );
    sqlx::Connection::close(conn).await?;
    let view = day(db.clone()).await?;
    assert_eq!(view.totals.attempts, 1);
    assert!(view.recording_gap, "the day's totals may be incomplete");
    stop(&test).await;
    Ok(())
}

/// PF-60-S05 AC6: a ledger in a newer format turns collection off for the
/// session with one warning, and every turn still runs.
#[tokio::test]
async fn accounting_newer_ledger_format_turns_collection_off_once() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let endpoint = format!("{}/v1", server.uri());
    let first = responses::mount_sse_repeating(&server, success(usage(Some(0)))).await;
    let test = builder(endpoint.clone(), enabled(&endpoint))
        .build_with_auto_env(&server)
        .await?;
    let db = test.codex.state_db().unwrap();
    let mut conn = connection(&db).await?;
    sqlx::raw_sql(
        "CREATE TABLE _accounting_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL,
            installed_on TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP, success BOOLEAN NOT NULL,
            checksum BLOB NOT NULL, execution_time BIGINT NOT NULL);
         INSERT INTO _accounting_migrations (version, description, success, checksum, execution_time)
            VALUES (99, 'future', 1, X'00', 0);",
    )
    .execute(&mut conn)
    .await?;
    sqlx::Connection::close(conn).await?;
    let mut warnings = Vec::new();
    for _ in 0..2 {
        submit(&test).await?;
        warnings.extend(
            terminal(&test)
                .await?
                .into_iter()
                .filter_map(|event| match event {
                    // Only accounting's own warnings: a host may add others
                    // (for example a missing code-mode host on Linux).
                    EventMsg::Warning(warning)
                        if warning.message.starts_with("Developer accounting") =>
                    {
                        Some(warning.message)
                    }
                    _ => None,
                }),
        );
    }
    assert_eq!(
        warnings,
        vec!["Developer accounting is off for this session: this home's cost ledger was written by a newer Corbanu build. Requests are sent as usual; /cost does not include them.".to_string()]
    );
    assert_eq!(first.requests().len(), 2);
    let mut conn = connection(&db).await?;
    let attempts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_schema WHERE name = 'draft_accounting_attempts'",
    )
    .fetch_one(&mut conn)
    .await?;
    assert_eq!(attempts, 0, "nothing was written to the newer ledger");
    sqlx::Connection::close(conn).await?;
    stop(&test).await;
    Ok(())
}

/// PF-60-S05 AC8: a guardian approval review is paid inference for the
/// conversation it reviews, so its request is recorded under that
/// conversation's thread, labelled as a review, and the conversation's totals
/// include it. The first review runs on the reusable trunk reviewer, which is
/// a persisted thread of its own; it still records under the parent.
#[cfg(not(target_os = "windows"))]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accounting_guardian_review_is_recorded_under_the_reviewed_conversation()
-> anyhow::Result<()> {
    use codex_core::config::Constrained;
    use codex_core::sandboxing::SandboxPermissions;
    use codex_protocol::config_types::ApprovalsReviewer;
    use codex_protocol::protocol::AskForApproval;
    use codex_protocol::protocol::Op;
    use codex_protocol::protocol::SandboxPolicy;
    use codex_protocol::user_input::UserInput;
    use core_test_support::responses::ev_assistant_message;
    use core_test_support::responses::ev_completed;
    use core_test_support::responses::ev_function_call;
    use core_test_support::responses::ev_response_created;
    use core_test_support::responses::sse;
    core_test_support::skip_if_no_network!(Ok(()));
    core_test_support::skip_if_sandbox!(Ok(()));

    let server = MockServer::start().await;
    let endpoint = format!("{}/v1", server.uri());
    let approval_policy = AskForApproval::OnRequest;
    let sandbox_policy = SandboxPolicy::WorkspaceWrite {
        writable_roots: vec![],
        network_access: false,
        exclude_tmpdir_env_var: true,
        exclude_slash_tmp: true,
    };
    let sandbox_for_config = sandbox_policy.clone();
    let test = builder(endpoint.clone(), enabled(&endpoint))
        .with_config(move |config| {
            config.permissions.approval_policy = Constrained::allow_any(approval_policy);
            config
                .set_legacy_sandbox_policy(sandbox_for_config)
                .expect("set sandbox policy");
        })
        .build_with_auto_env(&server)
        .await?;
    let output_file = test.cwd.path().join("guardian-denied.txt");
    let tool_args = json!({
        "cmd": format!("printf should-not-run > {}", output_file.display()),
        "yield_time_ms": 1_000_u64,
        "sandbox_permissions": SandboxPermissions::RequireEscalated,
        "justification": "Exercise Guardian accounting.",
    });
    responses::mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-parent-tool"),
                ev_function_call(
                    "exec-call",
                    "exec_command",
                    &serde_json::to_string(&tool_args)?,
                ),
                ev_completed("resp-parent-tool"),
            ]),
            sse(vec![
                ev_response_created("resp-guardian"),
                ev_assistant_message(
                    "msg-guardian",
                    &json!({
                        "risk_level": "high",
                        "user_authorization": "low",
                        "outcome": "deny",
                        "rationale": "Fixture denial.",
                    })
                    .to_string(),
                ),
                ev_completed("resp-guardian"),
            ]),
            sse(vec![
                ev_response_created("resp-parent-after"),
                ev_assistant_message("msg-parent-after", "denied"),
                ev_completed("resp-parent-after"),
            ]),
        ],
    )
    .await;
    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "run a command the reviewer denies".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: codex_protocol::protocol::ThreadSettingsOverrides {
                approval_policy: Some(approval_policy),
                approvals_reviewer: Some(ApprovalsReviewer::AutoReview),
                sandbox_policy: Some(sandbox_policy),
                ..Default::default()
            },
        })
        .await?;
    core_test_support::wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let db = test.codex.state_db().unwrap();
    let records = wait_attempts(&db, /*count*/ 3).await?;
    let parent = test.session_configured.thread_id;
    assert_eq!(
        records
            .iter()
            .map(|attempt| (
                attempt.thread_id == parent,
                attempt.turn.starts_with("review:")
            ))
            .collect::<Vec<_>>(),
        vec![(true, false), (true, true), (true, false)],
        "{records:#?}"
    );
    assert!(!output_file.exists());
    stop(&test).await;
    Ok(())
}

/// PF-60-S04: a side conversation - an ephemeral fork of a persisted
/// conversation, `/side` in the TUI - is paid inference for that conversation.
/// Its request is recorded under the conversation's thread, labelled `side:`,
/// and counts in the conversation's totals; the conversation's own requests
/// keep their labels.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accounting_side_conversation_is_recorded_under_its_conversation() -> anyhow::Result<()> {
    use codex_protocol::protocol::Op;
    use codex_protocol::user_input::UserInput;
    let server = MockServer::start().await;
    let endpoint = format!("{}/v1", server.uri());
    responses::mount_sse_sequence(
        &server,
        vec![success(usage(Some(0))), success(usage(Some(0)))],
    )
    .await;
    let test = builder(endpoint.clone(), enabled(&endpoint))
        .build_with_auto_env(&server)
        .await?;
    test.submit_turn("main conversation").await?;
    let db = test.codex.state_db().unwrap();
    wait_attempts(&db, /*count*/ 1).await?;
    let mut side_config = test.config.clone();
    side_config.ephemeral = true;
    let side = test
        .thread_manager
        .fork_thread(
            codex_core::ForkSnapshot::Interrupted,
            side_config,
            test.codex.rollout_path().unwrap(),
            Some(codex_protocol::protocol::ThreadSource::Feature(
                codex_protocol::protocol::ThreadSource::SIDE_CONVERSATION.to_string(),
            )),
            /*parent_trace*/ None,
        )
        .await?;
    side.thread
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "side question".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;
    core_test_support::wait_for_event(&side.thread, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let records = wait_attempts(&db, /*count*/ 2).await?;
    let parent = test.session_configured.thread_id;
    assert_ne!(side.thread_id, parent);
    assert_eq!(
        records
            .iter()
            .map(|attempt| (
                attempt.thread_id == parent,
                attempt.turn.starts_with("side:")
            ))
            .collect::<Vec<_>>(),
        vec![(true, false), (true, true)],
        "{records:#?}"
    );
    wait_observations(&db, /*count*/ 2).await?;
    // The conversation's day counts both requests' tokens, each once.
    let total = totals(&db, &records[0]).await?;
    assert_eq!(total.measured[0].known, 200);
    assert_eq!(total.measured[4].known, 80);
    test.thread_manager
        .shutdown_all_threads_bounded(std::time::Duration::from_secs(3))
        .await;
    Ok(())
}
