use std::any::TypeId;

use codex_exec_server::ExecServerError;
use pretty_assertions::assert_eq;
use rmcp::transport::DynamicTransportError;
use rmcp::transport::streamable_http_client::StreamableHttpError;

use crate::http_client_adapter::StreamableHttpClientAdapterError;

use super::*;

#[test]
fn retryable_initialize_error_includes_discovery_and_initialized_notification_context() {
    let contexts = [
        "send discover request",
        "send initialize request",
        "send initialized notification",
        "receive initialize response",
    ];

    assert_eq!(
        contexts.map(|context| {
            RmcpClient::is_retryable_client_initialize_error(&retryable_initialize_error(context))
        }),
        [true, true, true, false],
    );
}

#[test]
fn retryable_initialize_error_looks_through_a_failed_legacy_fallback() {
    let fallback_failed = |fallback| rmcp::service::ClientInitializeError::LegacyFallbackFailed {
        discover: Box::new(rmcp::service::ClientInitializeError::ConnectionClosed(
            "discover".to_string(),
        )),
        fallback: Box::new(fallback),
    };

    assert_eq!(
        [
            fallback_failed(retryable_initialize_error("send initialize request")),
            fallback_failed(retryable_initialize_error("receive initialize response")),
        ]
        .map(|error| RmcpClient::is_retryable_client_initialize_error(&error)),
        [true, false],
    );
}

#[test]
fn handshake_error_names_both_phases_of_a_failed_legacy_fallback() {
    let error = HandshakeError {
        source: rmcp::service::ClientInitializeError::LegacyFallbackFailed {
            discover: Box::new(rmcp::service::ClientInitializeError::ConnectionClosed(
                "discover response".to_string(),
            )),
            fallback: Box::new(retryable_initialize_error("send initialize request")),
        },
    };

    let message = error.to_string();
    assert!(
        message.starts_with("handshaking with MCP server failed: ")
            && message.contains("error sending request for url, when send initialize request")
            && message.ends_with(
                "(legacy initialize after server/discover failed: connection closed: discover response)"
            ),
        "unexpected handshake error: {message}"
    );
}

#[test]
fn retryable_streamable_http_error_includes_remote_body_stream_failure() {
    let errors = [
        StreamableHttpError::Client(StreamableHttpClientAdapterError::HttpRequest(
            ExecServerError::HttpRequest("error sending request for url".to_string()),
        )),
        StreamableHttpError::Client(StreamableHttpClientAdapterError::HttpRequest(
            ExecServerError::Server {
                code: JSON_RPC_INTERNAL_ERROR_CODE,
                message: "http/request failed: error sending request for url".to_string(),
            },
        )),
        StreamableHttpError::Client(StreamableHttpClientAdapterError::HttpRequest(
            ExecServerError::Protocol(
                "http response stream `http-1` failed: exec-server transport disconnected"
                    .to_string(),
            ),
        )),
        StreamableHttpError::Client(StreamableHttpClientAdapterError::HttpRequest(
            ExecServerError::Protocol(
                "http response stream `http-1` received seq 2, expected 1".to_string(),
            ),
        )),
        StreamableHttpError::UnexpectedServerResponse("HTTP 502: upstream failure".into()),
        StreamableHttpError::UnexpectedServerResponse("HTTP 400: bad request".into()),
    ];

    assert_eq!(
        errors.map(|error| RmcpClient::is_retryable_streamable_http_error(&error)),
        [true, true, true, false, true, false],
    );
}

fn retryable_initialize_error(context: &'static str) -> rmcp::service::ClientInitializeError {
    rmcp::service::ClientInitializeError::TransportError {
        error: DynamicTransportError::from_parts(
            "streamable_http",
            TypeId::of::<()>(),
            Box::new(StreamableHttpError::Client(
                StreamableHttpClientAdapterError::HttpRequest(ExecServerError::HttpRequest(
                    "error sending request for url".to_string(),
                )),
            )),
        ),
        context: context.into(),
    }
}
