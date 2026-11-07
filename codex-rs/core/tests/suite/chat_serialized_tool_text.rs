//! #1654: on Chat Completions, an answer that opens with JSON naming
//! "function_call" and goes on in prose is one assistant message: the turn
//! completes on the first request, with no stream error and no retry.

use std::time::Duration;

use codex_login::CodexAuth;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::WireApi;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::chat_completions_sse;
use core_test_support::responses::sse_response;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event_with_timeout;
use pretty_assertions::assert_eq;
use wiremock::Mock;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

const MODEL: &str = "glm-5.3-flash";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chat_answer_opening_with_function_call_json_is_one_message() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let answer = concat!(
        r#"{"function_call": "auto"} "#,
        "The legacy `function_call` parameter let an API caller control whether the model called a function."
    );
    let server = start_mock_server().await;
    // Every request gets the same answer, as a provider would on a retry.
    Mock::given(method("POST"))
        .and(path_regex(".*/chat/completions$"))
        .respond_with(sse_response(chat_completions_sse(MODEL, answer)))
        .mount(&server)
        .await;
    let endpoint = format!("{}/v1", server.uri());
    let test = test_codex()
        .with_auth(CodexAuth::from_api_key("synthetic-chat-1654"))
        .with_config(move |config| {
            config.model = Some(MODEL.into());
            config.model_provider_id = "openai".into();
            config.model_provider = ModelProviderInfo {
                request_max_retries: Some(0),
                stream_max_retries: Some(2),
                wire_api: WireApi::Chat,
                supports_websockets: false,
                ..ModelProviderInfo::create_openai_provider(Some(endpoint))
            };
            config.model_catalog = Some(
                codex_models_manager::bundled_models_response().expect("bundled model catalog"),
            );
        })
        .build(&server)
        .await?;

    test.codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "What did the legacy function_call parameter do?".into(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
            additional_context: Default::default(),
            thread_settings: Default::default(),
        })
        .await?;

    let mut errors = Vec::new();
    let mut messages = Vec::new();
    loop {
        match wait_for_event_with_timeout(&test.codex, |_| true, Duration::from_secs(30)).await {
            EventMsg::Error(error) => errors.push(error.message),
            EventMsg::StreamError(error) => errors.push(error.message),
            EventMsg::AgentMessage(message) => messages.push(message.message),
            EventMsg::TurnComplete(_) | EventMsg::TurnAborted(_) => break,
            _ => {}
        }
    }

    assert_eq!(errors, Vec::<String>::new());
    assert_eq!(messages, vec![answer.to_string()]);
    let chat_requests = server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|request| request.url.path().ends_with("/chat/completions"))
        .count();
    assert_eq!(chat_requests, 1);
    Ok(())
}
