use pretty_assertions::assert_eq;

use super::ExecServerNetworkPolicyDecision;
use super::ExecServerNetworkPolicyRequest;
use super::ExecServerNetworkProtocol;
use super::NetworkPolicyRequestParams;
use super::NetworkPolicyRequestResponse;
use crate::ProcessId;

#[test]
fn network_policy_request_uses_stable_json_shapes() {
    let request = NetworkPolicyRequestParams {
        process_id: ProcessId::from("process-1"),
        request: ExecServerNetworkPolicyRequest {
            protocol: ExecServerNetworkProtocol::HttpsConnect,
            host: "example.com".to_string(),
            port: 443,
        },
    };
    let request_json = serde_json::json!({
        "processId": "process-1",
        "request": {
            "protocol": "https_connect",
            "host": "example.com",
            "port": 443,
        },
    });
    assert_eq!(
        serde_json::to_value(&request).expect("serialize policy request"),
        request_json
    );
    let decoded_request: NetworkPolicyRequestParams =
        serde_json::from_value(request_json.clone()).expect("deserialize policy request");
    assert_eq!(
        serde_json::to_value(decoded_request).expect("reserialize policy request"),
        request_json
    );

    let decision = NetworkPolicyRequestResponse {
        decision: ExecServerNetworkPolicyDecision::Allow,
    };
    let decision_json = serde_json::json!({
        "decision": {"type": "allow"},
    });
    assert_eq!(
        serde_json::to_value(&decision).expect("serialize policy decision"),
        decision_json
    );
    assert_eq!(
        serde_json::from_value::<NetworkPolicyRequestResponse>(decision_json)
            .expect("deserialize policy decision"),
        decision
    );

    for (decision, decision_json) in [
        (
            ExecServerNetworkPolicyDecision::Deny {
                reason: "not_allowed".to_string(),
            },
            serde_json::json!({"type": "deny", "reason": "not_allowed"}),
        ),
        (
            ExecServerNetworkPolicyDecision::Ask {
                reason: "not_allowed".to_string(),
            },
            serde_json::json!({"type": "ask", "reason": "not_allowed"}),
        ),
    ] {
        let response = NetworkPolicyRequestResponse { decision };
        assert_eq!(
            serde_json::to_value(&response).expect("serialize policy decision"),
            serde_json::json!({"decision": decision_json})
        );
    }
}

#[test]
fn network_denied_notification_uses_a_stable_json_shape() {
    let notification = super::NetworkDeniedNotification {
        process_id: ProcessId::from("process-1"),
        protocol: "https_connect".to_string(),
        host: "blocked.example".to_string(),
        port: Some(443),
        reason: "denied".to_string(),
    };
    let json = serde_json::json!({
        "processId": "process-1",
        "protocol": "https_connect",
        "host": "blocked.example",
        "port": 443,
        "reason": "denied",
    });
    assert_eq!(
        serde_json::to_value(&notification).expect("serialize denial"),
        json
    );
    assert_eq!(
        serde_json::from_value::<super::NetworkDeniedNotification>(json)
            .expect("deserialize denial"),
        notification
    );
}
