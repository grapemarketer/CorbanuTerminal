use serde::Deserialize;
use serde::Serialize;

use crate::ProcessId;

pub const NETWORK_POLICY_REQUEST_METHOD: &str = "network/policyRequest";
pub const MAX_NETWORK_POLICY_HOST_BYTES: usize = 253;
pub const MAX_NETWORK_POLICY_PROCESS_ID_BYTES: usize = 256;
pub const MAX_NETWORK_POLICY_REASON_BYTES: usize = 1024;
/// Executor → client notification: the executor's proxy refused a request on
/// its own (#722, D278). Sent only for launches that set
/// `reportDenials`, so the client ends the command as failed.
pub const NETWORK_DENIED_METHOD: &str = "network/denied";
pub const MAX_NETWORK_DENIED_PROTOCOL_BYTES: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPolicyRequestParams {
    pub process_id: ProcessId,
    pub request: ExecServerNetworkPolicyRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecServerNetworkPolicyRequest {
    pub protocol: ExecServerNetworkProtocol,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecServerNetworkProtocol {
    Http,
    HttpsConnect,
    Socks5Tcp,
    Socks5Udp,
}

/// One request the executor's proxy denied without asking the client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkDeniedNotification {
    pub process_id: ProcessId,
    /// Proxy protocol label, e.g. `http` or `https_connect`.
    pub protocol: String,
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Policy reason code, e.g. `denied` or `not_allowed_local`.
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPolicyRequestResponse {
    pub decision: ExecServerNetworkPolicyDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecServerNetworkPolicyDecision {
    Allow,
    Deny { reason: String },
    Ask { reason: String },
}

#[cfg(test)]
#[path = "network_policy_tests.rs"]
mod tests;
