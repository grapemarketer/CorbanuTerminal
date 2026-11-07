use super::McpToolApprovalMetadata;
use super::requires_mcp_tool_approval;
use crate::security::protected_surface::McpCall;
use crate::session::step_context::StepContext;
use codex_protocol::protocol::McpInvocation;
use codex_utils_path_uri::PathUri;
use serde_json::Value;

/// The shared tool metadata and step cwd for both post-taint MCP checks.
/// Owned so the actual-call check can move it into the blocking classifier.
pub(super) struct McpCallContext {
    cwd: PathUri,
    title: Option<String>,
    acts_outside: bool,
}

impl McpCallContext {
    pub(super) fn new(step_context: &StepContext, metadata: &McpToolApprovalMetadata) -> Self {
        #[allow(deprecated)]
        let cwd = step_context.environments.primary().map_or_else(
            || PathUri::from_abs_path(&step_context.turn.cwd),
            |environment| environment.cwd().clone(),
        );
        Self {
            cwd,
            title: metadata.tool_title.clone(),
            acts_outside: requires_mcp_tool_approval(metadata.annotations.as_ref()),
        }
    }

    /// Actual calls include their arguments; lasting-approval options are
    /// judged on the tool alone, so that caller explicitly supplies `None`.
    pub(super) fn call<'a>(
        &'a self,
        invocation: &'a McpInvocation,
        arguments: Option<&'a Value>,
    ) -> McpCall<'a> {
        McpCall {
            server: &invocation.server,
            tool: &invocation.tool,
            title: self.title.as_deref(),
            acts_outside: self.acts_outside,
            arguments,
            cwd: &self.cwd,
        }
    }
}

#[cfg(test)]
#[path = "post_taint_tests.rs"]
mod tests;
