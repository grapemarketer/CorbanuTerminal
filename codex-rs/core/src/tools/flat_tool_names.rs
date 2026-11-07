//! Flat, provider-valid names for namespaced tools on wires without namespaces.
//!
//! Chat Completions and Anthropic Messages accept only top-level function tools,
//! while MCP tools (`mcp__server` + `tool`) and other grouped tools (`clock` +
//! `sleep`, ...) are registered under a namespace. On those wires each such tool
//! is offered under one flat name, and a call to that name is mapped back to the
//! namespaced [`ToolName`] before dispatch, so the same handler, approvals and
//! security gates run as on the Responses wire.

use std::collections::HashMap;
use std::collections::HashSet;

use codex_tools::ResponsesApiTool;
use codex_tools::ToolName;
use codex_tools::code_mode_name_for_tool_name;
use sha1::Digest;
use sha1::Sha1;

/// Longest name every supported function-calling wire accepts.
///
/// OpenAI and OpenAI-compatible Chat Completions providers (Z.AI, OpenRouter,
/// Kimi, Vercel, ...), Anthropic Messages and Bedrock Converse all cap tool
/// names at 64 characters of `[A-Za-z0-9_-]`; Gemini-backed routes also require
/// a leading letter or underscore. A name satisfying [`is_portable_tool_name`]
/// is therefore valid on all of them.
pub(crate) const MAX_PORTABLE_TOOL_NAME_LEN: usize = 64;
const HASH_SUFFIX_HEX_LEN: usize = 12;

/// Flat names offered for namespaced tools, and the reverse mapping used to
/// route a call back to the namespaced tool.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct FlatToolNames {
    by_flat_name: HashMap<String, ToolName>,
}

impl FlatToolNames {
    /// Returns the namespaced tool a flat name stands for.
    pub(crate) fn resolve(&self, flat_name: &str) -> Option<&ToolName> {
        self.by_flat_name.get(flat_name)
    }

    /// The flat name this request offers for a namespaced tool, if any.
    pub(crate) fn flat_name_for(&self, tool_name: &ToolName) -> Option<&str> {
        self.by_flat_name
            .iter()
            .find(|(_, offered)| *offered == tool_name)
            .map(|(flat, _)| flat.as_str())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.by_flat_name.is_empty()
    }
}

/// Returns whether `name` is a valid tool name on every supported wire.
pub(crate) fn is_portable_tool_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    name.len() <= MAX_PORTABLE_TOOL_NAME_LEN
        && (first.is_ascii_alphabetic() || first == '_')
        && chars.all(is_portable_char)
}

fn is_portable_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// The flat name a namespaced tool prefers: namespace and name joined the way
/// code mode joins them (`mcp__server__tool`), with characters outside the
/// portable set replaced. Plain names are returned as they are. The result can
/// still be too long or collide; [`flatten_namespaced_tools`] resolves both.
fn preferred_flat_tool_name(tool_name: &ToolName) -> String {
    let joined = code_mode_name_for_tool_name(tool_name);
    if tool_name.namespace.is_none() {
        joined
    } else {
        portable_chars(&joined)
    }
}

/// Replaces characters outside the portable set and ensures a valid first
/// character. The result is ASCII but may be longer than the limit.
fn portable_chars(name: &str) -> String {
    let mut flat: String = name
        .chars()
        .map(|c| if is_portable_char(c) { c } else { '_' })
        .collect();
    if !flat.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        flat.insert(0, '_');
    }
    flat
}

/// The flat name a namespaced tool gets when nothing collides with it: its
/// preferred name when that is portable, otherwise the hashed form.
#[cfg(test)]
pub(crate) fn flat_tool_name_for(tool_name: &ToolName) -> String {
    let preferred = preferred_flat_tool_name(tool_name);
    if is_portable_tool_name(&preferred) {
        preferred
    } else {
        hashed_flat_name(&preferred, &tool_identity(tool_name), /*attempt*/ 0)
    }
}

/// A flat name for replaying a call of a namespaced tool this request does not
/// offer: its preferred name when that is portable and no offered tool has it,
/// otherwise the hashed form, so the call never reads as an offered tool's.
pub(crate) fn unoffered_flat_tool_name(tool_name: &ToolName, offered: &HashSet<&str>) -> String {
    let preferred = preferred_flat_tool_name(tool_name);
    if is_portable_tool_name(&preferred) && !offered.contains(preferred.as_str()) {
        return preferred;
    }
    let identity = tool_identity(tool_name);
    let mut attempt = 0_u32;
    loop {
        let candidate = hashed_flat_name(&preferred, &identity, attempt);
        if !offered.contains(candidate.as_str()) {
            return candidate;
        }
        attempt = attempt.saturating_add(1);
    }
}

/// Gives every namespaced tool a unique, portable flat name.
///
/// `reserved` holds the names of the top-level tools sent alongside, which a
/// flat name must never shadow. The mapping depends only on the set of tools,
/// not their order: a tool keeps its preferred name when that name is portable
/// and taken by nothing else; otherwise it gets a truncated name with a hash of
/// its namespaced identity. Returns the renamed tools in input order.
pub(crate) fn flatten_namespaced_tools(
    tools: Vec<(ToolName, ResponsesApiTool)>,
    reserved: &HashSet<String>,
) -> (Vec<ResponsesApiTool>, FlatToolNames) {
    let preferred: Vec<String> = tools
        .iter()
        .map(|(tool_name, _)| preferred_flat_tool_name(tool_name))
        .collect();
    let mut preferred_counts = HashMap::<&str, usize>::new();
    for name in &preferred {
        *preferred_counts.entry(name.as_str()).or_default() += 1;
    }

    let mut used: HashSet<String> = reserved.clone();
    let mut flat_names: Vec<Option<String>> = vec![None; tools.len()];
    for (index, name) in preferred.iter().enumerate() {
        if is_portable_tool_name(name)
            && preferred_counts.get(name.as_str()) == Some(&1)
            && !reserved.contains(name)
        {
            used.insert(name.clone());
            flat_names[index] = Some(name.clone());
        }
    }

    // Hash the rest in identity order so retries after a (practically
    // impossible) hash collision do not depend on registration order.
    let mut pending: Vec<usize> = (0..tools.len())
        .filter(|index| flat_names[*index].is_none())
        .collect();
    pending.sort_by(|left, right| tools[*left].0.cmp(&tools[*right].0));
    for index in pending {
        let identity = tool_identity(&tools[index].0);
        let mut attempt = 0_u32;
        let name = loop {
            let candidate = hashed_flat_name(&preferred[index], &identity, attempt);
            if used.insert(candidate.clone()) {
                break candidate;
            }
            attempt = attempt.saturating_add(1);
        };
        flat_names[index] = Some(name);
    }

    let mut by_flat_name = HashMap::with_capacity(tools.len());
    let renamed = tools
        .into_iter()
        .zip(flat_names)
        .map(|((tool_name, mut tool), flat_name)| {
            let flat_name = flat_name.unwrap_or_else(|| unreachable!("every tool gets a name"));
            by_flat_name.insert(flat_name.clone(), tool_name);
            tool.name = flat_name;
            // Without namespaces there is no tool search, so nothing is deferred.
            tool.defer_loading = None;
            tool
        })
        .collect();
    (renamed, FlatToolNames { by_flat_name })
}

fn tool_identity(tool_name: &ToolName) -> String {
    format!(
        "{}\0{}",
        tool_name.namespace.as_deref().unwrap_or_default(),
        tool_name.name
    )
}

fn hashed_flat_name(preferred: &str, identity: &str, attempt: u32) -> String {
    let mut hasher = Sha1::new();
    hasher.update(identity.as_bytes());
    if attempt > 0 {
        hasher.update(format!("\0{attempt}").as_bytes());
    }
    let digest = format!("{:x}", hasher.finalize());
    let suffix = &digest[..HASH_SUFFIX_HEX_LEN];
    let mut prefix = portable_chars(preferred);
    prefix.truncate(MAX_PORTABLE_TOOL_NAME_LEN - HASH_SUFFIX_HEX_LEN - 1);
    format!("{prefix}_{suffix}")
}

/// Sorted `(flat name, namespaced tool)` pairs, for tests and diagnostics.
#[cfg(test)]
pub(crate) fn sorted_mapping(
    names: &FlatToolNames,
) -> std::collections::BTreeMap<String, ToolName> {
    names
        .by_flat_name
        .iter()
        .map(|(flat, tool_name)| (flat.clone(), tool_name.clone()))
        .collect()
}

#[cfg(test)]
#[path = "flat_tool_names_tests.rs"]
mod tests;
