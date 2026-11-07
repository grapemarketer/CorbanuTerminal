use super::*;
use codex_tools::JsonSchema;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;

fn tool(name: &str) -> ResponsesApiTool {
    ResponsesApiTool {
        name: name.to_string(),
        description: format!("{name} description"),
        strict: false,
        defer_loading: Some(true),
        parameters: JsonSchema::object(
            BTreeMap::new(),
            /*required*/ None,
            /*additional_properties*/ None,
        ),
        output_schema: None,
    }
}

fn mcp(namespace: &str, name: &str) -> (ToolName, ResponsesApiTool) {
    (ToolName::namespaced(namespace, name), tool(name))
}

fn flatten(
    tools: Vec<(ToolName, ResponsesApiTool)>,
    reserved: &[&str],
) -> (Vec<String>, BTreeMap<String, ToolName>) {
    let reserved = reserved
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    let (tools, names) = flatten_namespaced_tools(tools, &reserved);
    (
        tools.into_iter().map(|tool| tool.name).collect(),
        sorted_mapping(&names),
    )
}

#[test]
fn mcp_tools_keep_their_code_mode_names_and_round_trip() {
    let (names, mapping) = flatten(
        vec![
            mcp("mcp__orders", "lookup_order"),
            mcp("mcp__orders", "cancel_order"),
            mcp("codex_apps__gmail_", "send"),
        ],
        &["exec_command"],
    );

    assert_eq!(
        names,
        vec![
            "mcp__orders__lookup_order",
            "mcp__orders__cancel_order",
            "codex_apps__gmail_send",
        ]
    );
    assert_eq!(
        mapping,
        BTreeMap::from([
            (
                "codex_apps__gmail_send".to_string(),
                ToolName::namespaced("codex_apps__gmail_", "send"),
            ),
            (
                "mcp__orders__cancel_order".to_string(),
                ToolName::namespaced("mcp__orders", "cancel_order"),
            ),
            (
                "mcp__orders__lookup_order".to_string(),
                ToolName::namespaced("mcp__orders", "lookup_order"),
            ),
        ])
    );
}

#[test]
fn flattened_tools_are_no_longer_deferred() {
    let reserved = HashSet::new();
    let (tools, _) = flatten_namespaced_tools(vec![mcp("mcp__a", "b")], &reserved);
    assert_eq!(tools[0].defer_loading, None);
    assert_eq!(tools[0].description, "b description");
}

#[test]
fn names_are_portable_on_every_wire() {
    let long_namespace = format!("mcp__{}", "s".repeat(70));
    let (names, mapping) = flatten(
        vec![
            mcp(&long_namespace, "tool"),
            mcp("9lives", "jump"),
            mcp("mcp__dotted.server", "tool"),
        ],
        &[],
    );

    for name in &names {
        assert!(is_portable_tool_name(name), "{name} is not portable");
        assert!(name.len() <= MAX_PORTABLE_TOOL_NAME_LEN);
    }
    assert_eq!(names[1], "_9lives__jump");
    assert_eq!(names[2], "mcp__dotted_server__tool");
    assert_eq!(names[0].len(), MAX_PORTABLE_TOOL_NAME_LEN);
    assert_eq!(
        mapping.get(&names[0]),
        Some(&ToolName::namespaced(long_namespace, "tool"))
    );
}

#[test]
fn collisions_and_reserved_names_get_distinct_hashed_names() {
    let (names, mapping) = flatten(
        vec![
            // Both prefer `mcp__a__b_c` after sanitizing.
            mcp("mcp__a", "b_c"),
            mcp("mcp__a", "b.c"),
            // Would shadow a registered plain tool.
            mcp("exec", "command"),
        ],
        &["exec__command"],
    );

    assert_eq!(mapping.len(), 3);
    assert!(names[0].starts_with("mcp__a__b_c_"), "{}", names[0]);
    assert!(names[1].starts_with("mcp__a__b_c_"), "{}", names[1]);
    assert_ne!(names[0], names[1]);
    assert!(names[2].starts_with("exec__command_"), "{}", names[2]);
    assert!(!mapping.contains_key("exec__command"));
    for name in &names {
        assert!(is_portable_tool_name(name), "{name} is not portable");
    }
}

#[test]
fn mapping_does_not_depend_on_registration_order() {
    let tools = vec![
        mcp("mcp__a", "b_c"),
        mcp("mcp__a", "b.c"),
        mcp("mcp__z", "tool"),
        mcp(&format!("mcp__{}", "x".repeat(80)), "t"),
    ];
    let mut reversed = tools.clone();
    reversed.reverse();

    assert_eq!(flatten(tools, &[]).1, flatten(reversed, &[]).1);
}

#[test]
fn portable_name_rules() {
    assert!(is_portable_tool_name("mcp__server__tool-1"));
    assert!(is_portable_tool_name("_x"));
    assert!(is_portable_tool_name(&"a".repeat(64)));
    assert!(!is_portable_tool_name(&"a".repeat(65)));
    assert!(!is_portable_tool_name(""));
    assert!(!is_portable_tool_name("1tool"));
    assert!(!is_portable_tool_name("-tool"));
    assert!(!is_portable_tool_name("tool.name"));
    assert!(!is_portable_tool_name("tool name"));
}

#[test]
fn flat_name_for_matches_the_offered_name_without_collisions() {
    let long_namespace = format!("mcp__{}", "s".repeat(70));
    let tools = vec![
        mcp("mcp__orders", "lookup"),
        mcp(&long_namespace, "tool"),
        mcp("9lives", "jump"),
    ];
    let expected: Vec<String> = tools
        .iter()
        .map(|(tool_name, _)| flat_tool_name_for(tool_name))
        .collect();
    assert_eq!(flatten(tools, &[]).0, expected);
    assert_eq!(expected[0], "mcp__orders__lookup");
    assert_eq!(
        preferred_flat_tool_name(&ToolName::plain("exec_command")),
        "exec_command"
    );
}

/// #688: a call of a tool this request does not offer replays under a name no
/// offered tool has, even when an offered tool took its preferred name.
#[test]
fn unoffered_tool_name_never_matches_an_offered_tool() {
    let orders = ToolName::namespaced("mcp__orders", "lookup");
    assert_eq!(
        unoffered_flat_tool_name(&orders, &HashSet::from(["lookup"])),
        "mcp__orders__lookup"
    );

    let hashed = unoffered_flat_tool_name(&orders, &HashSet::from(["mcp__orders__lookup"]));
    assert_ne!(hashed, "mcp__orders__lookup");
    assert!(is_portable_tool_name(&hashed), "{hashed}");

    // Even the first hashed form is skipped when an offered tool has it.
    let offered = HashSet::from(["mcp__orders__lookup", hashed.as_str()]);
    let next = unoffered_flat_tool_name(&orders, &offered);
    assert!(
        !offered.contains(next.as_str()) && is_portable_tool_name(&next),
        "{next}"
    );
}
