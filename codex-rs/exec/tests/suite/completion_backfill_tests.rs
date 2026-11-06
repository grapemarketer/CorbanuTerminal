use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex_exec::test_codex_exec;
use pretty_assertions::assert_eq;
use serde_json::json;

const PARENT_PROMPT: &str = "spawn a child and wait for it";
const CHILD_PROMPT: &str = "child: finish first";
const SPAWN_CALL_ID: &str = "spawn-call";
const WAIT_CALL_ID: &str = "wait-call";

fn body_contains(request: &wiremock::Request, text: &str) -> bool {
    std::str::from_utf8(&request.body).is_ok_and(|body| body.contains(text))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ignores_unrelated_turn_completion_before_backfilling_primary_turn() -> anyhow::Result<()> {
    streamed_usage("none", Some(200)).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issue290_forked_child_without_authoritative_baseline_is_unknown() -> anyhow::Result<()> {
    streamed_usage("all", Some(200)).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn issue290_missing_child_usage_makes_the_aggregate_unknown() -> anyhow::Result<()> {
    streamed_usage("none", /*child_tokens*/ None).await
}

async fn streamed_usage(fork_turns: &str, child_tokens: Option<i64>) -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let spawn_args = json!({
        "message": CHILD_PROMPT,
        "task_name": "worker",
        "fork_turns": fork_turns,
    })
    .to_string();
    let _parent_turn = responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, PARENT_PROMPT),
        responses::sse(vec![
            responses::ev_response_created("resp-parent-1"),
            responses::ev_function_call_with_namespace(
                SPAWN_CALL_ID,
                "collaboration",
                "spawn_agent",
                &spawn_args,
            ),
            responses::ev_completed_with_tokens("resp-parent-1", /*total_tokens*/ 100),
        ]),
    )
    .await;
    let _child_turn = responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, CHILD_PROMPT) && !body_contains(request, SPAWN_CALL_ID)
        },
        responses::sse(vec![
            responses::ev_response_created("resp-child"),
            responses::ev_assistant_message("msg-child", "child done"),
            match child_tokens {
                Some(tokens) => responses::ev_completed_with_tokens("resp-child", tokens),
                None => json!({
                    "type": "response.completed",
                    "response": {"id": "resp-child"},
                }),
            },
        ]),
    )
    .await;
    let _parent_wait = responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            body_contains(request, SPAWN_CALL_ID) && !body_contains(request, WAIT_CALL_ID)
        },
        responses::sse(vec![
            responses::ev_response_created("resp-parent-2"),
            responses::ev_function_call_with_namespace(
                WAIT_CALL_ID,
                "collaboration",
                "wait_agent",
                "{}",
            ),
            responses::ev_completed_with_tokens("resp-parent-2", /*total_tokens*/ 300),
        ]),
    )
    .await;
    let _parent_completion = responses::mount_sse_once_match(
        &server,
        |request: &wiremock::Request| body_contains(request, WAIT_CALL_ID),
        responses::sse(vec![
            responses::ev_response_created("resp-parent-3"),
            responses::ev_assistant_message("msg-parent", "parent done"),
            responses::ev_completed_with_tokens("resp-parent-3", /*total_tokens*/ 400),
        ]),
    )
    .await;

    let test = test_codex_exec();
    let mock_provider = format!(
        "model_providers.mock_provider={{name=\"Mock provider for test\",base_url=\"{}/v1\",wire_api=\"responses\",supports_websockets=false}}",
        server.uri()
    );
    let output = test
        .cmd()
        .env(
            "RUST_LOG",
            "codex_app_server::message_processor=trace,codex_app_server::outgoing_message=trace",
        )
        .arg("--skip-git-repo-check")
        .arg("--json")
        .arg("-c")
        .arg(mock_provider)
        .args([
            "-c",
            "model_provider=\"mock_provider\"",
            "-c",
            "features.multi_agent=true",
            "-c",
            "features.multi_agent_v2=true",
            "-c",
            "features.enable_request_compression=false",
        ])
        .arg(PARENT_PROMPT)
        .output()?;
    assert!(output.status.success(), "exec run failed: {output:?}");

    let stdout = String::from_utf8(output.stdout)?;
    assert!(
        stdout.contains("parent done"),
        "primary completion was not processed: {stdout}"
    );
    let events = stdout
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let completions = events
        .iter()
        .filter(|event| event["type"] == "turn.completed")
        .collect::<Vec<_>>();
    assert_eq!(completions.len(), 1, "child completion must not end exec");
    assert_eq!(
        completions[0]["usage"],
        child_tokens
            .filter(|_| fork_turns == "none")
            .map(|tokens| json!({
                "input_tokens": 800 + tokens,
                "cached_input_tokens": 0,
                "cache_write_input_tokens": 0,
                "output_tokens": 0,
                "reasoning_output_tokens": 0,
            }))
            .unwrap_or(serde_json::Value::Null),
        "root 100 + 300 + 400 and child 200 must each be counted once"
    );
    let stderr = String::from_utf8(output.stderr)?;
    let lines = stderr.lines().collect::<Vec<_>>();
    let turn_completions = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            line.contains("app-server event: turn/completed")
                .then_some(index)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        turn_completions.len(),
        2,
        "expected the child completion before the primary completion: {stderr}"
    );

    let [child_completion, primary_completion] = turn_completions.as_slice() else {
        unreachable!("checked turn/completed count")
    };
    assert_eq!(
        lines[*child_completion + 1..*primary_completion]
            .iter()
            .filter(|line| line.contains("app-server typed request"))
            .count(),
        0,
        "the unrelated completion must not issue thread/read: {stderr}"
    );
    assert_eq!(
        lines[*primary_completion + 1..]
            .iter()
            .filter(|line| line.contains("app-server typed request"))
            .count(),
        2,
        "the primary completion should issue thread/read and thread/unsubscribe: {stderr}"
    );

    assert_eq!(
        lines
            .iter()
            .filter(|line| line.contains("app-server typed request"))
            .count(),
        6,
        "one metadata read resolves child ancestry; only the primary completion backfills items: {stderr}"
    );

    Ok(())
}
