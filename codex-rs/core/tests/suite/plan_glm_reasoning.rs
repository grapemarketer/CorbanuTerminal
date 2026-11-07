//! #1489: the Corbanu Plan gateway routes `z-ai/glm-5.2` to Ambient and
//! forwards the request body unchanged, so the Plan picker's Standard and Deep
//! choices must reach the wire as Ambient's `reasoning` object.

use codex_login::CodexAuth;
use codex_model_provider_info::AMBIENT_DEFAULT_MODEL;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::PFTERMINAL_PLAN_PROVIDER_ID;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::chat_completions_sse;
use core_test_support::responses::mount_chat_completions_sequence;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

async fn plan_glm_request_body(effort: ReasoningEffort) -> anyhow::Result<Value> {
    let server = start_mock_server().await;
    let chat = mount_chat_completions_sequence(
        &server,
        vec![chat_completions_sse(AMBIENT_DEFAULT_MODEL, "done")],
    )
    .await;
    let endpoint = format!("{}/v1", server.uri());
    let test = test_codex()
        .with_auth(CodexAuth::from_api_key("synthetic-plan-glm"))
        .with_config(move |config| {
            let mut provider = ModelProviderInfo::create_pfterminal_plan_provider();
            provider.base_url = Some(endpoint);
            provider.env_key = None;
            provider.experimental_bearer_token = Some("synthetic-plan-glm".into());
            provider.request_max_retries = Some(0);
            provider.stream_max_retries = Some(0);
            config.model_provider_id = PFTERMINAL_PLAN_PROVIDER_ID.into();
            config.model_provider = provider;
            config.model = Some(AMBIENT_DEFAULT_MODEL.into());
            config.model_reasoning_effort = Some(effort);
            config.model_catalog = Some(
                codex_models_manager::bundled_models_response().expect("bundled model catalog"),
            );
        })
        .build(&server)
        .await?;
    // Default thread settings keep the configured effort; `submit_turn` would
    // override it with the collaboration mode's.
    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "fixture".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let requests = chat.requests();
    assert_eq!(requests.len(), 1);
    Ok(requests[0].body_json())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_glm_5_2_standard_and_deep_send_ambient_reasoning() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    for (effort, wire_effort) in [
        (ReasoningEffort::Medium, "high"),
        (ReasoningEffort::XHigh, "max"),
    ] {
        let body = plan_glm_request_body(effort.clone()).await?;
        assert_eq!(body["model"], json!(AMBIENT_DEFAULT_MODEL));
        assert_eq!(
            (
                &body["reasoning"],
                body.get("reasoning_effort"),
                body.get("enable_thinking"),
            ),
            (&json!({"enabled": true, "effort": wire_effort}), None, None),
            "{effort:?}"
        );
    }
    Ok(())
}
