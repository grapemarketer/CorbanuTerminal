use super::*;
use crate::mcp_skill_dependencies::maybe_install_mcp_dependencies;
use crate::skills::model::SkillDependencies;
use crate::skills::model::SkillToolDependency;
use codex_config::load_global_mcp_servers;
use pretty_assertions::assert_eq;

fn stdio_dependency(name: &str) -> SkillToolDependency {
    SkillToolDependency {
        r#type: "mcp".to_string(),
        value: name.to_string(),
        description: None,
        transport: Some("stdio".to_string()),
        command: Some(format!("{name}-server")),
        url: None,
    }
}

/// #1666: with `features.non_prefixed_mcp_tool_names` on, a skill's MCP
/// server dependency that would take a reserved tool namespace is skipped
/// with a warning and not written to the config; the others still install.
#[tokio::test]
async fn skill_dependency_install_skips_servers_in_reserved_namespaces() {
    let codex_home = tempfile::tempdir().expect("create codex home");
    let cwd = tempfile::tempdir().expect("create cwd");
    let (session, turn_context, rx) = make_session_and_context_with_auth_config_home_and_rx(
        CodexAuth::from_api_key("Test API Key"),
        Vec::new(),
        codex_home.path(),
        cwd.path(),
        |config| {
            for feature in [
                Feature::SkillMcpDependencyInstall,
                Feature::NonPrefixedMcpToolNames,
            ] {
                config
                    .features
                    .set_enabled(feature, /*enabled*/ true)
                    .expect("enable feature");
            }
        },
        /*start_submission_loop*/ false,
    )
    .await;
    let skill = SkillMetadata {
        name: "needs-servers".to_string(),
        description: "desc".to_string(),
        short_description: None,
        interface: None,
        dependencies: Some(SkillDependencies {
            tools: ["web", "python", "web_mcp"]
                .into_iter()
                .map(stdio_dependency)
                .collect(),
        }),
        policy: None,
        path_to_skills_md: cwd.path().join("SKILL.md").abs(),
        scope: SkillScope::Repo,
        plugin_id: None,
        remote_plugin_id: None,
    };

    maybe_install_mcp_dependencies(
        &session,
        &turn_context,
        turn_context.config.as_ref(),
        &[skill],
        /*elicitation_reviewer*/ None,
    )
    .await;

    let mut warnings = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let EventMsg::Warning(warning) = event.msg {
            warnings.push(warning.message);
        }
    }
    warnings.sort();
    assert_eq!(
        warnings,
        vec![
            "Skipped installing the skill's MCP server `python`: without the `mcp__` prefix (`features.non_prefixed_mcp_tool_names`), it would take a tool namespace reserved by the Responses API. To install it, list only other servers in `features.non_prefixed_mcp_tool_names.server_names`, or turn `features.non_prefixed_mcp_tool_names` off.".to_string(),
            "Skipped installing the skill's MCP server `web`: without the `mcp__` prefix (`features.non_prefixed_mcp_tool_names`), it would take a built-in tool namespace. To install it, list only other servers in `features.non_prefixed_mcp_tool_names.server_names`, or turn `features.non_prefixed_mcp_tool_names` off.".to_string(),
        ]
    );
    let servers = load_global_mcp_servers(codex_home.path())
        .await
        .expect("load MCP servers");
    let mut names: Vec<&str> = servers.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(names, vec!["web_mcp"]);
}
