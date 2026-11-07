use codex_app_server_protocol::JSONRPCErrorError;

pub(crate) const TURN_TRANSITION_PENDING_REQUEST_ERROR_REASON: &str = "turnTransition";

/// Core stopped waiting for the request (#722, D279).
pub(crate) const WITHDRAWN_PENDING_REQUEST_ERROR_REASON: &str = "withdrawn";

pub(crate) fn is_turn_transition_server_request_error(error: &JSONRPCErrorError) -> bool {
    pending_request_error_reason(error) == Some(TURN_TRANSITION_PENDING_REQUEST_ERROR_REASON)
}

pub(crate) fn is_withdrawn_server_request_error(error: &JSONRPCErrorError) -> bool {
    pending_request_error_reason(error) == Some(WITHDRAWN_PENDING_REQUEST_ERROR_REASON)
}

fn pending_request_error_reason(error: &JSONRPCErrorError) -> Option<&str> {
    error
        .data
        .as_ref()
        .and_then(|data| data.get("reason"))
        .and_then(serde_json::Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::is_turn_transition_server_request_error;
    use codex_app_server_protocol::JSONRPCErrorError;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn turn_transition_error_is_detected() {
        let error = JSONRPCErrorError {
            code: -1,
            message: "client request resolved because the turn state was changed".to_string(),
            data: Some(json!({ "reason": "turnTransition" })),
        };

        assert_eq!(is_turn_transition_server_request_error(&error), true);
    }

    #[test]
    fn unrelated_error_is_not_detected() {
        let error = JSONRPCErrorError {
            code: -1,
            message: "boom".to_string(),
            data: Some(json!({ "reason": "other" })),
        };

        assert_eq!(is_turn_transition_server_request_error(&error), false);
    }
}
