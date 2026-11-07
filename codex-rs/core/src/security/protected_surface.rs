//! PF-23-S01: the typed protected surface at the Core tool dispatch boundary.
//!
//! Every tool route a model can call is listed here with how a protected
//! action on it is checked once the session holds untrusted content (PF-30):
//! through the shared approval seam (`tools/orchestrator.rs`), by a check of
//! its own (MCP tool calls, typing into a running process), only through the
//! tools it calls (code mode), or not at all because it reaches no protected
//! resource. A route missing from the list is unclassified and needs a fresh
//! human approval after untrusted content, refused when approvals are off.
//!
//! Like PF-30-S03, none of this relaxes anything: without `source_envelopes`
//! and a protected level, or before untrusted content, nothing changes.

use crate::security::tainted_action::ProtectedActionKind;
use crate::security::tainted_action::classify;
use crate::tools::sandboxing::ApprovalAction;
use codex_tools::ToolName;
use codex_utils_path_uri::PathUri;
use serde_json::Value;
use std::path::Path;

pub(crate) mod confined;
mod gate;
mod git_paths;
mod read_denials;
mod typed;
mod upload_read;

pub(crate) use gate::Admission;
pub(crate) use gate::Route;
pub(crate) use gate::admit;
pub(crate) use gate::ask_human;
pub(crate) use gate::ask_human_answer;
pub(crate) use gate::check_dispatch;
#[cfg(test)]
pub(crate) use gate::question_id;
pub(crate) use read_denials::ReadDenials;
pub(crate) use read_denials::create_missing_database_folder;
pub(crate) use read_denials::post_taint_read_policy;
pub(crate) use read_denials::protect_file_tool_context;
pub(crate) use read_denials::readable_under;
pub(crate) use typed::TypedWindow;
pub(crate) use typed::lock as lock_typed_input;
pub(crate) use typed::note_interrupt;
pub(crate) use upload_read::read_checked;

/// Who provides a tool. Only first-party code says `Builtin` or `Extension`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolOrigin {
    /// A Core tool handler.
    Builtin,
    /// A first-party extension crate (`ext/`).
    Extension,
    /// An MCP server tool.
    Mcp,
    /// A tool the client defined for this thread.
    Dynamic,
}

/// How a protected action on a route is checked after untrusted content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Coverage {
    /// Shell, exec and patch tools: the shared approval seam classifies the
    /// exact command or patch (PF-30-S03).
    ApprovalSeam,
    /// Checked by the route itself: MCP tool calls, `write_stdin`.
    OwnCheck,
    /// Code mode: the cell has no host access of its own; each tool it calls
    /// is dispatched and checked like a direct call.
    NestedCallsOnly,
    /// Hands work to another agent, which inherits the taint and the level,
    /// or waits on one.
    ChildAgent,
    /// Starts a new agent: asks the human after untrusted content, naming it
    /// as an agent launch (D320, #1653).
    AgentLaunch,
    /// Restarts a finished agent: asks the human after untrusted content,
    /// the same way as starting one (D334, #1702).
    AgentRestart,
    /// Wakes an agent with a follow-up task: goes ahead while the agent is
    /// running, and asks like [`Coverage::AgentRestart`] otherwise, since it
    /// then starts a new turn (D334, #1702).
    AgentFollowup,
    /// Changes what the agent may do (permissions, plugins): asks the human
    /// itself unless an automatic reviewer would answer.
    PolicyRequest,
    /// Reads, reports or waits; reaches no protected resource.
    NoProtectedEffect,
    /// Not on this list: needs a fresh human approval after untrusted content.
    Unclassified,
}

const APPROVAL_SEAM: &[&str] = &[
    "shell",
    "shell_command",
    "exec_command",
    "apply_patch",
    "structured_edit",
    "structured_write",
];
/// D320 (#1653): starting an agent, on every route. `spawn_agent_v1` is the
/// plain-function alias for providers without namespace tools (Chat
/// Completions, Claude-style).
const AGENT_LAUNCH: &[&str] = &["spawn_agent", "spawn_agent_plaintext", "spawn_agent_v1"];
/// D334 (#1702): resuming a closed agent asks like starting one, on every
/// route.
const AGENT_RESTART: &[&str] = &["resume_agent", "resume_agent_v1"];
const AGENT_FOLLOWUP: &[&str] = &["followup_task", "followup_task_plaintext"];
/// The `_v1` names are the plain-function aliases of the Responses names
/// (D320, D334): each is treated like the name it stands for.
const CHILD_AGENT: &[&str] = &[
    "send_input",
    "send_input_v1",
    "send_message",
    "send_message_plaintext",
    "close_agent",
    "close_agent_v1",
    "interrupt_agent",
    "list_agents",
    "wait_agent",
    "wait_agent_v1",
];
const NO_PROTECTED_EFFECT: &[&str] = &[
    "update_plan",
    "request_user_input",
    "view_image",
    "sleep",
    "curr_time",
    "get_context_remaining",
    "new_context",
    "tool_search",
    "list_mcp_resources",
    "list_mcp_resource_templates",
    "read_mcp_resource",
    "wait_for_environment",
    "list_available_plugins_to_install",
    "test_sync_tool",
];
/// First-party extension tools, by namespace and name.
const EXTENSION_NO_PROTECTED_EFFECT: &[(Option<&str>, &str)] = &[
    (None, "get_goal"),
    (None, "create_goal"),
    (None, "update_goal"),
    (Some("web"), "run"),
    (Some("image_gen"), "imagegen"),
    (Some("memories"), "list"),
    (Some("memories"), "read"),
    (Some("memories"), "search"),
    // A note keeps its source; recalled memory is never trusted (PF-30-S02).
    (Some("memories"), "add_ad_hoc_note"),
    (Some("skills"), "list"),
    (Some("skills"), "read"),
];

/// The route matrix.
pub(crate) fn coverage(origin: ToolOrigin, name: &ToolName) -> Coverage {
    let plain = name.name.as_str();
    match origin {
        ToolOrigin::Mcp => Coverage::OwnCheck,
        ToolOrigin::Dynamic => Coverage::Unclassified,
        ToolOrigin::Extension => {
            let listed = EXTENSION_NO_PROTECTED_EFFECT
                .iter()
                .any(|(namespace, tool)| *namespace == name.namespace.as_deref() && *tool == plain);
            if listed {
                Coverage::NoProtectedEffect
            } else {
                Coverage::Unclassified
            }
        }
        ToolOrigin::Builtin => match plain {
            _ if APPROVAL_SEAM.contains(&plain) => Coverage::ApprovalSeam,
            "write_stdin" => Coverage::OwnCheck,
            "exec" | "wait" => Coverage::NestedCallsOnly,
            "request_permissions" | "request_plugin_install" => Coverage::PolicyRequest,
            _ if AGENT_LAUNCH.contains(&plain) => Coverage::AgentLaunch,
            _ if AGENT_RESTART.contains(&plain) => Coverage::AgentRestart,
            _ if AGENT_FOLLOWUP.contains(&plain) => Coverage::AgentFollowup,
            _ if CHILD_AGENT.contains(&plain) => Coverage::ChildAgent,
            _ if NO_PROTECTED_EFFECT.contains(&plain) => Coverage::NoProtectedEffect,
            _ => Coverage::Unclassified,
        },
    }
}

/// One MCP tool call, as the host will send it.
pub(crate) struct McpCall<'a> {
    pub(crate) server: &'a str,
    pub(crate) tool: &'a str,
    pub(crate) title: Option<&'a str>,
    /// The tool may change or send data: it is not marked read-only and is
    /// marked (or not ruled out as) destructive or open-world.
    pub(crate) acts_outside: bool,
    pub(crate) arguments: Option<&'a Value>,
    pub(crate) cwd: &'a PathUri,
}

/// Argument keys whose string value is a command line.
const COMMAND_KEYS: &[&str] = &[
    "command",
    "cmd",
    "script",
    "shell",
    "commandline",
    "command_line",
];
/// Tool-name words that move value on their own.
const TRANSFER_WORDS: &[&str] = &[
    "transfer",
    "pay",
    "payment",
    "payout",
    "swap",
    "withdraw",
    "withdrawal",
    "purchase",
    "buy",
    "sell",
    "trade",
    "remit",
];
/// What `send` moves when it is a value transfer (`send_sol`, `sendTokens`).
const SENT_VALUE_WORDS: &[&str] = &[
    "sol",
    "eth",
    "btc",
    "usdc",
    "usdt",
    "token",
    "tokens",
    "funds",
    "money",
    "crypto",
    "coin",
    "coins",
    "lamports",
    "transaction",
];
/// A tool name starting with one of these reads (unless it also acts).
const READ_VERBS: &[&str] = &[
    "get", "list", "fetch", "read", "search", "view", "show", "describe", "quote", "estimate",
    "preview", "simulate", "check", "lookup", "find", "query",
];
const ACTION_VERBS: &[&str] = &[
    "send", "execute", "submit", "place", "confirm", "sign", "create", "initiate", "approve",
];
/// Argument strings read from one call; more fail closed.
const MAX_ARGUMENT_STRINGS: usize = 1024;
const MAX_ARGUMENT_DEPTH: usize = 16;

/// The protected kind of an MCP tool call. The tool's own description of
/// itself can only add protection: a read-only tool is still judged by the
/// paths and commands in its arguments.
pub(crate) fn classify_mcp(call: &McpCall<'_>, codex_home: &Path) -> Option<ProtectedActionKind> {
    let names = [Some(call.tool), call.title].into_iter().flatten();
    let words: Vec<String> = names.flat_map(name_words).collect();
    let has = |list: &[&str]| words.iter().any(|word| list.contains(&word.as_str()));
    // `get_swap_quote`, `list_buy_orders`: a read of value, not a move.
    // `quote_and_swap`, `checkThenWithdraw`: a read joined to an act.
    let reads = name_words(call.tool)
        .first()
        .is_some_and(|word| READ_VERBS.contains(&word.as_str()))
        && !has(ACTION_VERBS)
        && !has(&["and", "then"]);
    let sends_value = has(&["send"]) && has(SENT_VALUE_WORDS);
    if !reads && (sends_value || has(TRANSFER_WORDS)) {
        return Some(ProtectedActionKind::ValueTransfer);
    }
    let mut found = call.acts_outside.then_some(ProtectedActionKind::Disclosure);
    let mut strings = Vec::new();
    if let Some(arguments) = call.arguments
        && !argument_strings(arguments, /*key*/ None, /*depth*/ 0, &mut strings)
    {
        return strongest(found, Some(ProtectedActionKind::UnseenCode));
    }
    let exec = |command: Vec<String>| ApprovalAction::ExecCommand {
        id: String::new(),
        environment_id: call.server.to_string(),
        command,
        cwd: call.cwd.clone(),
        sandbox_permissions: crate::sandboxing::SandboxPermissions::UseDefault,
        additional_permissions: None,
        justification: None,
        tty: false,
    };
    let mut paths = vec!["mcp-arguments".to_string()];
    for (key, text) in strings {
        if key.is_some_and(|key| COMMAND_KEYS.contains(&key.to_lowercase().as_str())) {
            let command = vec!["sh".to_string(), "-c".to_string(), text.to_string()];
            found = strongest(found, classify(&exec(command), codex_home));
        } else if text.contains('/') || text.starts_with(['~', '.', '$']) {
            paths.push(text.to_string());
        }
    }
    if paths.len() > 1 {
        found = strongest(found, classify(&exec(paths), codex_home));
    }
    found
}

/// The protected kind of typing `chars` into a running process started as
/// `process_command`: that of the process itself, or of the text read as
/// code by the program it most likely is (a shell unless it is a known
/// interpreter).
pub(crate) fn classify_typed_input(
    process_command: &str,
    chars: &str,
    cwd: &PathUri,
    codex_home: &Path,
) -> Option<ProtectedActionKind> {
    let exec = |command: Vec<String>| ApprovalAction::ExecCommand {
        id: String::new(),
        environment_id: String::new(),
        command,
        cwd: cwd.clone(),
        sandbox_permissions: crate::sandboxing::SandboxPermissions::UseDefault,
        additional_permissions: None,
        justification: None,
        tty: true,
    };
    let sh = |text: &str| vec!["sh".to_string(), "-c".to_string(), text.to_string()];
    let process = classify(&exec(sh(process_command)), codex_home);
    // The first interpreter named anywhere in the process command
    // (`env python3`, `cd x && node`, `uv run python`).
    let words = shlex::split(process_command).unwrap_or_default();
    let interpreter = words.iter().find_map(|word| interpreter_of(word));
    // Only a plain REPL (`python3 -i`, `node`) reads typed text as code
    // alone; anything else (`nix-shell -p python3`, `python3 x.py; bash`,
    // a pty spawned by `-c`) may hand it to a shell too.
    let plain_repl = {
        let rest: Vec<&String> = words
            .iter()
            .skip_while(|word| {
                word.contains('=') || matches!(word.as_str(), "env" | "exec" | "command" | "nohup")
            })
            .collect();
        rest.first()
            .is_some_and(|word| interpreter_of(word).is_some())
            && rest[1..].iter().all(|arg| {
                // Options only, none that runs code (`-c`, `-c'...'`, `--eval=`).
                let code_flag = match arg.strip_prefix("--") {
                    Some(long) => ["eval", "print", "require", "command"]
                        .iter()
                        .any(|flag| long.starts_with(flag)),
                    None => arg
                        .chars()
                        .nth(1)
                        .is_some_and(|letter| "cemprx".contains(letter)),
                };
                arg.starts_with('-') && !code_flag
            })
            && !program_is(process_command, "ipython")
    };
    // Line editing the host does not replay (Tab completion, history keys,
    // Ctrl-U/Ctrl-A, escape sequences) can turn typed text into anything.
    let edits = chars
        .chars()
        .any(|ch| (ch.is_control() && ch != '\n' && ch != '\r') || ch == '\u{7f}');
    let as_code = interpreter.and_then(|(name, flag)| {
        classify(
            &exec(vec![name.to_string(), flag.to_string(), chars.to_string()]),
            codex_home,
        )
    });
    let typed = if plain_repl {
        as_code
    } else {
        // Shell input, with history re-execution (`!!`, `^a^b`, `fc -s`, `r`)
        // unreadable, and for a named interpreter also its code.
        let as_shell = classify(&exec(sh(chars)), codex_home);
        let history = shell_history(chars).then_some(ProtectedActionKind::UnseenCode);
        strongest(strongest(as_shell, history), as_code)
    };
    let typed = if edits {
        strongest(typed, Some(ProtectedActionKind::UnseenCode))
    } else {
        typed
    };
    strongest(process, typed)
}

/// Single keys that stop or end input rather than add to it (Ctrl-C,
/// Ctrl-D, Ctrl-Z, Ctrl-\); sent alone they are not judged.
pub(crate) fn is_interrupt(chars: &str) -> bool {
    matches!(chars, "\u{3}" | "\u{4}" | "\u{1a}" | "\u{1c}")
}

fn program_is(process_command: &str, name: &str) -> bool {
    shlex::split(process_command)
        .unwrap_or_default()
        .iter()
        .any(|word| {
            word.rsplit('/')
                .next()
                .unwrap_or_default()
                .eq_ignore_ascii_case(name)
        })
}

/// The interpreter a word names, with its inline-code flag.
fn interpreter_of(word: &str) -> Option<(&'static str, &'static str)> {
    let name = word.rsplit('/').next().unwrap_or_default().to_lowercase();
    let python = name
        .strip_prefix("python")
        .is_some_and(|version| version.chars().all(|ch| ch.is_ascii_digit() || ch == '.'));
    match name.as_str() {
        _ if python || name == "ipython" => Some(("python3", "-c")),
        "node" | "deno" | "bun" => Some(("node", "-e")),
        "ruby" | "irb" => Some(("ruby", "-e")),
        "perl" => Some(("perl", "-e")),
        _ => None,
    }
}

fn shell_history(chars: &str) -> bool {
    let bytes = chars.as_bytes();
    chars.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with('^') || line.starts_with("fc ") || line == "r" || line.starts_with("r ")
    }) || bytes.windows(2).any(|pair| {
        pair[0] == b'!' && (pair[1].is_ascii_alphanumeric() || b"!-?#$".contains(&pair[1]))
    })
}

fn strongest(
    left: Option<ProtectedActionKind>,
    right: Option<ProtectedActionKind>,
) -> Option<ProtectedActionKind> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.strongest(right)),
        (left, right) => left.or(right),
    }
}

/// Lowercase words of a tool name: split at punctuation and camel case.
fn name_words(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for ch in name.chars() {
        if (!ch.is_alphanumeric() || (ch.is_uppercase() && previous_lower)) && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        if ch.is_alphanumeric() {
            current.extend(ch.to_lowercase());
        }
        previous_lower = ch.is_lowercase() || ch.is_ascii_digit();
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Every string in `value` with the object key it sits under. False when the
/// value is too large or too deep to read in full.
fn argument_strings<'a>(
    value: &'a Value,
    key: Option<&'a str>,
    depth: usize,
    out: &mut Vec<(Option<&'a str>, &'a str)>,
) -> bool {
    if depth > MAX_ARGUMENT_DEPTH || out.len() > MAX_ARGUMENT_STRINGS {
        return false;
    }
    match value {
        Value::String(text) => {
            out.push((key, text));
            out.len() <= MAX_ARGUMENT_STRINGS
        }
        Value::Array(items) => items
            .iter()
            .all(|item| argument_strings(item, key, depth + 1, out)),
        Value::Object(map) => map.iter().all(|(name, item)| {
            out.push((None, name));
            argument_strings(item, Some(name), depth + 1, out)
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) => true,
    }
}

#[cfg(test)]
#[path = "protected_surface_tests.rs"]
mod tests;
