use super::*;
use crate::environment_selection::TurnEnvironmentState;
use crate::security::protected_surface;
use crate::security::tainted_action::ProtectedActionKind;
use crate::session::tests::make_session_and_context;
use crate::session::turn_context::TurnEnvironment;
use pretty_assertions::assert_eq;
use rmcp::model::ToolAnnotations;
use serde_json::json;
use std::sync::Arc;

fn metadata() -> McpToolApprovalMetadata {
    McpToolApprovalMetadata {
        annotations: None,
        connector_id: None,
        link_id: None,
        connector_name: None,
        connector_description: None,
        connected_account_email: None,
        plugin_id: None,
        tool_title: None,
        tool_description: None,
        mcp_app_resource_uri: None,
        codex_apps_meta: None,
        openai_file_input_optional_fields: None,
    }
}

#[tokio::test]
async fn shared_context_preserves_both_mcp_call_views() {
    let (_session, turn) = make_session_and_context().await;
    let step = StepContext::for_test(Arc::new(turn));
    let expected_cwd = step
        .environments
        .primary()
        .expect("primary environment")
        .cwd();
    let hints = [None, Some(false), Some(true)];
    let mut all_annotations = vec![None];
    for read_only in hints {
        for destructive in hints {
            for open_world in hints {
                all_annotations.push(Some(ToolAnnotations::from_raw(
                    /*title*/ None,
                    read_only,
                    destructive,
                    /*idempotent_hint*/ None,
                    open_world,
                )));
            }
        }
    }
    for annotations in all_annotations {
        for (tool, title, arguments) in [
            ("search_issues", None, json!({"query": "ordinary text"})),
            ("lookup", Some("Transfer funds"), json!({})),
            ("transfer", None, json!({"amount": 0})),
        ] {
            let invocation = McpInvocation {
                server: "fixture".to_string(),
                tool: tool.to_string(),
                arguments: Some(arguments),
            };
            let mut metadata = metadata();
            metadata.annotations = annotations.clone();
            metadata.tool_title = title.map(str::to_string);
            let context = McpCallContext::new(&step, &metadata);
            for arguments in [None, invocation.arguments.as_ref()] {
                let call = context.call(&invocation, arguments);
                // The original inline construction is the equivalence reference.
                let original = protected_surface::McpCall {
                    server: &invocation.server,
                    tool: &invocation.tool,
                    title: metadata.tool_title.as_deref(),
                    acts_outside: requires_mcp_tool_approval(metadata.annotations.as_ref()),
                    arguments,
                    cwd: expected_cwd,
                };
                assert_eq!(call.server, original.server);
                assert_eq!(call.tool, original.tool);
                assert_eq!(call.title, original.title);
                assert_eq!(call.acts_outside, original.acts_outside);
                assert_eq!(call.arguments, original.arguments);
                assert_eq!(call.cwd, original.cwd);
                assert_eq!(
                    protected_surface::classify_mcp(&call, step.turn.config.codex_home.as_path()),
                    protected_surface::classify_mcp(
                        &original,
                        step.turn.config.codex_home.as_path()
                    ),
                    "{tool}, title={title:?}, annotations={annotations:?}, arguments={arguments:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn actual_call_arguments_do_not_leak_into_tool_only_approval() {
    let (_session, turn) = make_session_and_context().await;
    let step = StepContext::for_test(Arc::new(turn));
    let mut metadata = metadata();
    metadata.annotations = Some(ToolAnnotations::from_raw(
        /*title*/ None,
        /*read_only_hint*/ Some(true),
        /*destructive_hint*/ Some(false),
        /*idempotent_hint*/ None,
        /*open_world_hint*/ Some(false),
    ));
    // Use slash spelling in this fixture so MCP's path collector sees it on
    // every host; retain the actual Windows drive and configured home.
    let protected_path = step
        .turn
        .config
        .codex_home
        .as_path()
        .join("config.toml")
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let invocation = McpInvocation {
        server: "fixture".to_string(),
        tool: "read_file".to_string(),
        arguments: Some(json!({
            "path": protected_path,
        })),
    };
    let context = McpCallContext::new(&step, &metadata);
    assert_eq!(
        protected_surface::classify_mcp(
            &context.call(&invocation, invocation.arguments.as_ref()),
            step.turn.config.codex_home.as_path(),
        ),
        Some(ProtectedActionKind::SecurityPolicy)
    );
    assert_eq!(
        protected_surface::classify_mcp(
            &context.call(&invocation, /*arguments*/ None),
            step.turn.config.codex_home.as_path(),
        ),
        None
    );
}

#[tokio::test]
async fn shared_context_uses_primary_step_cwd_and_turn_fallback() -> anyhow::Result<()> {
    let (_session, turn) = make_session_and_context().await;
    #[allow(deprecated)]
    let fallback = PathUri::from_abs_path(&turn.cwd);
    let mut step = StepContext::for_test(Arc::new(turn));
    let environment = step
        .environments
        .primary()
        .expect("primary environment")
        .environment
        .clone();
    let primary_cwd = PathUri::parse("file:///C:/remote/primary")?;
    let secondary_cwd = PathUri::parse("file:///C:/remote/secondary")?;
    Arc::get_mut(&mut step)
        .expect("unique step")
        .environments
        .environments = vec![
        TurnEnvironmentState::Ready(TurnEnvironment::new(
            "primary".to_string(),
            Arc::clone(&environment),
            primary_cwd.clone(),
            Vec::new(),
            /*shell*/ None,
        )),
        TurnEnvironmentState::Ready(TurnEnvironment::new(
            "secondary".to_string(),
            environment,
            secondary_cwd,
            Vec::new(),
            /*shell*/ None,
        )),
    ];
    let invocation = McpInvocation {
        server: "secondary".to_string(),
        tool: "read_file".to_string(),
        arguments: None,
    };
    let metadata = metadata();
    let context = McpCallContext::new(&step, &metadata);
    assert_ne!(primary_cwd, fallback);
    assert_eq!(
        context.call(&invocation, /*arguments*/ None).cwd,
        &primary_cwd
    );

    Arc::get_mut(&mut step)
        .expect("unique step")
        .environments
        .environments
        .clear();
    let fallback_context = McpCallContext::new(&step, &metadata);
    assert_eq!(
        fallback_context.call(&invocation, /*arguments*/ None).cwd,
        &fallback
    );
    // The earlier snapshot remains usable after the step's environments change.
    assert_eq!(
        context.call(&invocation, /*arguments*/ None).cwd,
        &primary_cwd
    );
    Ok(())
}
