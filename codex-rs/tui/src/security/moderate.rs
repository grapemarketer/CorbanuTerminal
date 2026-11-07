//! #750: Moderate as a person chooses it in `/security` (Travis, 2026-10-14,
//! decisions D1–D6). Moderate is Core's level: it adds no launch-time
//! overrides (D1, today's controls), so the person's own sandbox, approval,
//! network and web search settings stay. What it adds is enforced by Core and
//! listed here, each row saying only what this build enforces:
//!
//! - commands never leave the sandbox (D3, #747), remembered rules included
//!   (D6: they still skip the prompt);
//! - the protected-file rules and the fresh-approval checks after untrusted
//!   content (PF-23, #239, #513). Core refuses every model request under a
//!   protected level without the `source_envelopes` labels, so Moderate is
//!   not saved without them;
//! - model keys through the credential broker, from the next start (D2);
//! - spawned agents with Moderate enforced (D5), and `corbanu exec` or
//!   `review` started by an agent command (#766).
//!
//! Core's rules apply as soon as the person confirms; the broker at the next
//! start (D4).

use codex_features::Feature;

use crate::legacy_core::config::Config;

/// The feature flags the Moderate rows depend on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Facts {
    /// `source_envelopes`: required (Core refuses model requests under a
    /// protected level without it).
    pub(crate) untrusted_content: bool,
    /// `secretless_agent_launch`.
    pub(crate) secretless_launch: bool,
    /// `secret_output_gate`.
    pub(crate) output_gate: bool,
}

impl Facts {
    pub(crate) fn from_config(config: &Config) -> Self {
        Self {
            untrusted_content: config.features.enabled(Feature::SourceEnvelopes),
            secretless_launch: config.features.enabled(Feature::SecretlessAgentLaunch)
                || codex_protocol::secretless_launch::is_armed(),
            output_gate: config.features.enabled(Feature::SecretOutputGate),
        }
    }
}

/// The sandbox row (D3, D6).
pub(crate) const SANDBOX: &str = "agent commands always stay in your own sandbox, even when you approve a request to run outside it, and \"retry without sandbox?\" is not offered; commands you allowed with \"don't ask again\" skip the prompt but stay in it. Your own sandbox, approval, network and web search settings are kept (if your settings turn the sandbox off, commands outside the protected-file rules run without one)";

/// Why Moderate cannot be saved, if it cannot.
pub(crate) fn blocker(facts: Facts) -> Option<&'static str> {
    (!facts.untrusted_content).then_some(
        "Moderate needs the untrusted-content labels (the source_envelopes feature): without them Corbanu refuses every model request under Moderate. Turn source_envelopes on and restart Corbanu Terminal first.",
    )
}

/// What Core's Moderate rules do in a running session, as the confirmation
/// and the saved screen name them.
pub(crate) const EFFECTS: &str = "agent commands always stay in the sandbox; unless you approve that one command, they cannot read credential files in your home folder or Corbanu's sign-in, vault, config, history and state, or write files that run code later (Corbanu's security state and keys stay closed to them even then, and, with secretless_agent_launch on, so do the CLI tokens and Corbanu files it keeps); after untrusted content, protected actions need your fresh approval";

/// The rows of the Moderate review, in order: control and Moderate's value.
pub(crate) fn rows(facts: Facts) -> Vec<(&'static str, String)> {
    vec![
        ("Sandbox", SANDBOX.to_string()),
        (
            "Protected files",
            "agent commands cannot read the credential files in your home folder (such as .ssh, .aws, .gnupg, .kube, .docker, .netrc, .npmrc and the macOS keychain folder) or Corbanu's sign-in, vault, config, history and state, and cannot write files that run code later (your shell and git start-up files, git hooks and config, the project's .codex and .agents) or Corbanu's own files. Your fresh approval of one command lifts this for that command, except Corbanu's runtime files, its security state and keys (such as security_state.json and source-origin.key), and what secretless_agent_launch keeps; a remembered or automatic approval never does. A project's .env files are not covered (Aggressive covers them)".to_string(),
        ),
        (
            "Untrusted content",
            "tool, MCP, web, agent and memory output reaches the model labelled untrusted; after it, protected actions (the vault, credentials, security settings, disclosure, value transfer, code you have not seen, MCP calls that change or send data, starting an agent, tools Corbanu has not classified) need your fresh approval, and cached, remembered or automatic approvals don't count; with approvals off they are refused".to_string(),
        ),
        (
            "Secrets",
            format!(
                "{}; {}",
                if facts.secretless_launch {
                    "only allowlisted environment variables reach agent commands, and the vault store, sign-in files, config and CLI tokens (such as ~/.aws/credentials and ~/.netrc) stay unreadable to them even when you approve (secretless_agent_launch)"
                } else {
                    "agent commands get your environment as configured (secretless_agent_launch off)"
                },
                if facts.output_gate {
                    "managed secrets and stored vault credentials are removed from command output (secret_output_gate)"
                } else {
                    "command output is not filtered (secret_output_gate off)"
                }
            ),
        ),
        (
            super::aggressive::MODEL_KEYS_LABEL,
            format!(
                "from the next start, {}",
                super::aggressive::MODEL_KEYS_ROW
            ),
        ),
        (
            "Child agents",
            "agents started in this Corbanu Terminal (/spawn and the agent tools), and on macOS and Linux `corbanu exec` or `review` started by an agent command, run with Moderate enforced; Claude panes are refused because Claude Code runs outside the sandbox, unless contained_external_agents and secretless_agent_launch run them in it".to_string(),
        ),
        (
            "Kill switch",
            "/security → g stops the work in every pane and refuses model turns until you turn it off".to_string(),
        ),
    ]
}

/// #739: [`REMOVED`]'s protected-files row while protected launch
/// (secretless_agent_launch) stays on: its launch contract keeps Corbanu's
/// own files and some home credential files unreadable to agent commands.
pub(crate) const REMOVED_PROTECTED_FILES_UNDER_PROTECTED_LAUNCH: &str = "most credential files in your home folder are no longer denied to agent commands, and files that run code later are no longer read-only; your own sandbox settings decide, except that Corbanu's sign-in, vault, config and history, and a few CLI token files (such as .netrc, .aws/credentials and .npmrc), stay unreadable to them (secretless_agent_launch)";

/// What a move from Moderate to Permissive removes at the next start.
pub(crate) const REMOVED: [(&str, &str); 4] = [
    (
        "Sandbox",
        "a command you approve to run outside the sandbox, a retry without it and a remembered rule that skips it run outside it again",
    ),
    (
        "Protected files",
        "credential files in your home folder and Corbanu's sign-in, vault, config, history and state are no longer denied to agent commands, and files that run code later are no longer read-only; your own sandbox settings decide",
    ),
    (
        "Untrusted content",
        "protected actions after untrusted content no longer need your fresh approval",
    ),
    (
        "Model keys",
        "Corbanu reads provider keys itself again, unless your own config turns broker_model_auth on",
    ),
];

/// What Aggressive enforces that Moderate does not keep, shown before a move
/// from Aggressive to Moderate.
pub(crate) const REMOVED_FROM_AGGRESSIVE: [(&str, &str); 5] = [
    (
        "Sandbox",
        "your own sandbox applies instead of Aggressive's (writes only in the current folder, no /tmp); commands still never leave it",
    ),
    (
        "Approvals",
        "your own approval settings apply instead of approving every command that is not known read-only",
    ),
    (
        "Network",
        "your own network and web search settings apply instead of both off",
    ),
    (
        "Vault and environment",
        "the rule that refuses `corbanu vault …` to agent commands is removed, secret-like environment variables are no longer removed unless your settings remove them, and a project's .env files are no longer denied",
    ),
    (
        "Nested agents",
        "the nested-agents setting no longer applies",
    ),
];

/// The inspector's nested-agents row under Moderate (D5, #749, #766).
pub(crate) const NESTED: &str = "on macOS and Linux, `corbanu exec` and `review` started by an agent command run with Moderate enforced, also pointed at another Corbanu home; with this Corbanu home, which they cannot write inside the sandbox, they run in a Corbanu home of their own, without your config, sign-in or model keys. Interactive sessions, `app-server`, `mcp-server` and credential commands they start are refused";

/// What stays as configured under Moderate.
pub(crate) const UNCHANGED: &str = "Unchanged: model and provider, your approval, sandbox, network and web search settings, MCP servers, apps and hooks (they run outside the sandbox), and wallet scopes.";

#[cfg(test)]
#[path = "moderate_tests.rs"]
mod tests;
