//! User-facing warning for sessions whose sandboxed commands run under the
//! legacy (non-admin, restricted-token) Windows sandbox backend.
//!
//! Since #705 that backend runs commands at Low integrity, so they can't
//! create, change, delete or move files outside their writable roots except
//! where objects are labelled Low (`AppData\LocalLow`, objects the user
//! labelled Low, and roots another legacy session is using at that moment).
//! It still can't stop a command from reading files, its network block is
//! only proxy and tool settings, and some process-launch routes leave the
//! command's job (#1507). The elevated backend protects more, so the warning
//! tells users how to switch to it. It never changes how commands are
//! sandboxed.

use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::models::PermissionProfile;

use crate::permission_profile_supports_windows_restricted_token_sandbox;

/// Warning shown once per session when commands run under the legacy backend.
/// The link points at the shipped guidance in `docs/sandbox.md`.
pub const LEGACY_WINDOWS_SANDBOX_WARNING: &str = concat!(
    "Corbanu Terminal is using the non-admin Windows sandbox. ",
    "It blocks sandboxed commands from creating, changing, deleting or moving files ",
    "outside the folders they may write to, except in places marked low-integrity ",
    "(such as AppData\\LocalLow) and in folders another non-admin sandbox session is using. ",
    "It does not stop commands from reading your files, its network block is only proxy ",
    "and tool settings that a program can ignore, and a few ways of starting programs ",
    "get partly outside it. ",
    "Switch to the default sandbox (one-time Administrator setup) by running ",
    "/setup-default-sandbox, or set `sandbox = \"elevated\"` under `[windows]` in config.toml. ",
    "Learn more: ",
    "https://github.com/CorbanuCore/CorbanuTerminal/blob/main/docs/sandbox.md#windows-non-admin-sandbox",
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HostPlatform {
    Windows,
    Other,
}

impl HostPlatform {
    fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Other
        }
    }
}

/// Returns the legacy-backend warning when this host will run sandboxed
/// commands for `permission_profile` under the legacy Windows backend.
///
/// `windows_sandbox_level` is the level that selects the backend:
/// `RestrictedToken` is the legacy backend, `Elevated` is not affected, and
/// `Disabled` means sessions do not use a Windows sandbox at all.
pub fn legacy_windows_sandbox_warning(
    windows_sandbox_level: WindowsSandboxLevel,
    permission_profile: &PermissionProfile,
) -> Option<String> {
    legacy_windows_sandbox_warning_for_host(
        HostPlatform::current(),
        windows_sandbox_level,
        permission_profile,
    )
}

fn legacy_windows_sandbox_warning_for_host(
    host: HostPlatform,
    windows_sandbox_level: WindowsSandboxLevel,
    permission_profile: &PermissionProfile,
) -> Option<String> {
    let uses_legacy_backend = match windows_sandbox_level {
        WindowsSandboxLevel::RestrictedToken => true,
        WindowsSandboxLevel::Elevated | WindowsSandboxLevel::Disabled => false,
    };
    (host == HostPlatform::Windows
        && uses_legacy_backend
        && permission_profile_supports_windows_restricted_token_sandbox(permission_profile))
    .then(|| LEGACY_WINDOWS_SANDBOX_WARNING.to_string())
}

#[cfg(test)]
#[path = "windows_legacy_warning_tests.rs"]
mod tests;
