//! PF-24 profile copy and the future authenticated PF-24/25 observation seam.
//! `/security` currently explores configuration intent only. The state below is
//! not connected to a live event channel; no policy mutation is activated.

pub(crate) mod aggressive;
pub(crate) mod confirm;
pub(crate) mod current;
pub(crate) mod grant_view;
pub(crate) mod home_link;
pub(crate) mod inspector;
pub(crate) mod launch;
pub(crate) mod level;
pub(crate) mod migration;
pub(crate) mod moderate;
pub(crate) mod nested;
pub(crate) mod preflight;
pub mod restart;
pub(crate) mod revocation_view;
pub(crate) mod view;

use codex_protocol::security::SecurityControlAction;
use codex_protocol::security::SecurityControlRequest;
use codex_protocol::security::SecurityInspectorEvent;
use codex_protocol::security::SecurityRequestError;

#[allow(dead_code)]
#[derive(Default)]
pub(crate) struct SecurityViewState {
    observation: Option<SecurityInspectorEvent>,
}

#[allow(dead_code)]
impl SecurityViewState {
    /// Only accept observations from the trusted Core event channel.
    pub(crate) fn observe(&mut self, event: SecurityInspectorEvent) {
        self.observation = Some(event);
    }

    /// Disconnect/resume must hide stale health and disable proposal preparation
    /// until the native event channel supplies a fresh observation.
    pub(crate) fn invalidate(&mut self) {
        self.observation = None;
    }

    pub(crate) fn observation(&self) -> Option<&SecurityInspectorEvent> {
        self.observation.as_ref()
    }

    pub(crate) fn prepare_request(
        &self,
        action: SecurityControlAction,
    ) -> Result<Option<SecurityControlRequest>, SecurityRequestError> {
        self.observation
            .as_ref()
            .map(|event| SecurityControlRequest::new(event.epoch, action))
            .transpose()
    }
}

#[cfg(test)]
#[path = "view_state_tests.rs"]
mod tests;
