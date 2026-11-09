// PF-13-S02 connects this staged trusted boundary to vault resolution.
#[allow(dead_code)]
pub(crate) mod credential_capability;
// Registration only: downstream single-feature sprints own these leaf modules.
pub(crate) mod aggressive;
#[allow(dead_code)]
pub(crate) mod authoritative_state;
#[cfg(target_os = "linux")]
#[allow(dead_code)]
pub(crate) mod authoritative_state_anchor;
pub(crate) mod broker_client;
pub(crate) mod browser_isolation;
pub(crate) mod confidentiality;
pub mod confirmed_approvals;
pub(crate) mod disclosure_gate;
mod effective_policy;
pub(crate) mod git_ext_guard;
pub(crate) mod git_postcheck;
pub mod grant_offer;
pub(crate) mod ingress;
pub mod inspection;
mod integration;
pub mod inventory;
pub(crate) mod kill_switch_stop;
pub(crate) mod launch_contract;
pub mod level_change;
pub mod migration;
pub mod preflight;
pub(crate) mod protected_additional;
pub(crate) mod protected_links;
pub(crate) mod protected_reads;
mod protected_runtime;
pub(crate) mod protected_surface;
pub(crate) mod recovery;
pub(crate) mod refusal_notice;
pub mod revocation_change;
pub(crate) mod taint;
pub(crate) mod tainted_action;
pub(crate) mod ui_events;
pub(crate) mod user_namespaces;

pub(crate) use effective_policy::EffectivePolicyInitialization;
pub(crate) use effective_policy::EffectivePolicyView;
pub(crate) use effective_policy::PersistedHumanSecurityState;
pub(crate) use effective_policy::SecurityPolicyError;
pub(crate) use effective_policy::TrustedSecurityController;
pub(crate) use effective_policy::transition;

#[cfg(test)]
#[path = "effective_policy_tests.rs"]
mod effective_policy_tests;

#[cfg(test)]
#[path = "integration_tests.rs"]
mod integration_tests;

#[cfg(test)]
#[path = "authoritative_state_tests.rs"]
mod authoritative_state_tests;

#[cfg(all(test, target_os = "linux"))]
#[path = "authoritative_state_anchor_tests.rs"]
mod authoritative_state_anchor_tests;

#[cfg(test)]
#[path = "protected_runtime_tests.rs"]
mod protected_runtime_tests;

#[cfg(test)]
#[path = "pf_29_s01_tests.rs"]
mod pf_29_s01_tests;

#[cfg(test)]
#[path = "pf_29_s02_tests.rs"]
mod pf_29_s02_tests;
