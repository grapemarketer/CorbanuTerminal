#![cfg(unix)]

mod common;

use anyhow::Context;
use codex_exec_server::ExecResponse;
use codex_exec_server::InitializeParams;
use codex_exec_server::InitializeResponse;
use codex_exec_server::ProcessId;
use codex_exec_server::ReadResponse;
use codex_exec_server::TerminateResponse;
use codex_exec_server::WriteResponse;
use codex_exec_server::WriteStatus;
use codex_exec_server_protocol::JSONRPCError;
use codex_exec_server_protocol::JSONRPCMessage;
use codex_exec_server_protocol::JSONRPCResponse;
use codex_exec_server_protocol::ProcessSandboxType;
use codex_utils_path_uri::PathUri;
use common::exec_server::exec_server;
use pretty_assertions::assert_eq;
use std::time::Duration;
use tokio::time::Instant;

/// `SESSION_ALREADY_ATTACHED_ERROR_CODE` in `src/rpc.rs` (crate-private).
const SESSION_ALREADY_ATTACHED_ERROR_CODE: i64 = -32010;
/// How long a resume may keep hitting "already attached" while the server
/// finishes detaching the closed connection. The resumed process must outlive
/// it (see the `sleep 30` below).
const RESUME_ATTACHED_RETRY_BUDGET: Duration = Duration::from_secs(20);
const RESUME_ATTACHED_RETRY_INTERVAL: Duration = Duration::from_millis(50);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_server_starts_process_over_websocket() -> anyhow::Result<()> {
    let mut server = exec_server().await?;
    let initialize_id = server
        .send_request(
            "initialize",
            serde_json::to_value(InitializeParams {
                client_name: "exec-server-test".to_string(),
                resume_session_id: None,
            })?,
        )
        .await?;
    let _ = server
        .wait_for_event(|event| {
            matches!(
                event,
                JSONRPCMessage::Response(JSONRPCResponse { id, .. }) if id == &initialize_id
            )
        })
        .await?;

    server
        .send_notification("initialized", serde_json::json!({}))
        .await?;

    let process_start_id = server
        .send_request(
            "process/start",
            serde_json::json!({
                "processId": "proc-1",
                "argv": ["true"],
                "cwd": PathUri::from_host_native_path(std::env::current_dir()?)?,
                "env": {},
                "tty": false,
                "pipeStdin": false,
                "arg0": null
            }),
        )
        .await?;
    let response = server
        .wait_for_event(|event| {
            matches!(
                event,
                JSONRPCMessage::Response(JSONRPCResponse { id, .. }) if id == &process_start_id
            )
        })
        .await?;
    let JSONRPCMessage::Response(JSONRPCResponse { id, result }) = response else {
        panic!("expected process/start response");
    };
    assert_eq!(id, process_start_id);
    let process_start_response: ExecResponse = serde_json::from_value(result)?;
    assert_eq!(
        process_start_response,
        ExecResponse {
            process_id: ProcessId::from("proc-1"),
            sandbox_type: Some(ProcessSandboxType::None),
        }
    );

    server.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_server_defaults_omitted_pipe_stdin_to_closed_stdin() -> anyhow::Result<()> {
    let mut server = exec_server().await?;
    let initialize_id = server
        .send_request(
            "initialize",
            serde_json::to_value(InitializeParams {
                client_name: "exec-server-test".to_string(),
                resume_session_id: None,
            })?,
        )
        .await?;
    let _ = server
        .wait_for_event(|event| {
            matches!(
                event,
                JSONRPCMessage::Response(JSONRPCResponse { id, .. }) if id == &initialize_id
            )
        })
        .await?;

    server
        .send_notification("initialized", serde_json::json!({}))
        .await?;

    let process_start_id = server
        .send_request(
            "process/start",
            serde_json::json!({
                "processId": "proc-default-stdin",
                "argv": [
                    "/bin/sh",
                    "-c",
                    "sleep 0.3; if IFS= read -r line; then printf 'read:%s\\n' \"$line\"; else printf 'eof\\n'; fi"
                ],
                "cwd": PathUri::from_host_native_path(std::env::current_dir()?)?,
                "env": {},
                "tty": false,
                "arg0": null
            }),
        )
        .await?;
    let response = server
        .wait_for_event(|event| {
            matches!(
                event,
                JSONRPCMessage::Response(JSONRPCResponse { id, .. }) if id == &process_start_id
            )
        })
        .await?;
    let JSONRPCMessage::Response(JSONRPCResponse { result, .. }) = response else {
        panic!("expected process/start response");
    };
    let process_start_response: ExecResponse = serde_json::from_value(result)?;
    assert_eq!(
        process_start_response,
        ExecResponse {
            process_id: ProcessId::from("proc-default-stdin"),
            sandbox_type: Some(ProcessSandboxType::None),
        }
    );

    let write_id = server
        .send_request(
            "process/write",
            serde_json::json!({
                "processId": "proc-default-stdin",
                "chunk": "aWdub3JlZAo=",
                "writeId": "write-default-stdin"
            }),
        )
        .await?;
    let response = server
        .wait_for_event(|event| {
            matches!(
                event,
                JSONRPCMessage::Response(JSONRPCResponse { id, .. }) if id == &write_id
            )
        })
        .await?;
    let JSONRPCMessage::Response(JSONRPCResponse { result, .. }) = response else {
        panic!("expected process/write response");
    };
    let write_response: WriteResponse = serde_json::from_value(result)?;
    assert_eq!(
        write_response,
        WriteResponse {
            status: WriteStatus::StdinClosed
        }
    );

    server.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_server_dedupes_retried_process_write_ids() -> anyhow::Result<()> {
    let mut server = exec_server().await?;
    let initialize_id = server
        .send_request(
            "initialize",
            serde_json::to_value(InitializeParams {
                client_name: "exec-server-test".to_string(),
                resume_session_id: None,
            })?,
        )
        .await?;
    let _ = server
        .wait_for_event(|event| {
            matches!(
                event,
                JSONRPCMessage::Response(JSONRPCResponse { id, .. }) if id == &initialize_id
            )
        })
        .await?;

    server
        .send_notification("initialized", serde_json::json!({}))
        .await?;

    let process_start_id = server
        .send_request(
            "process/start",
            serde_json::json!({
                "processId": "proc-write-id",
                "argv": [
                    "/bin/sh",
                    "-c",
                    "IFS= read -r first; printf 'line:%s\\n' \"$first\"; IFS= read -r second; printf 'line:%s\\n' \"$second\""
                ],
                "cwd": PathUri::from_host_native_path(std::env::current_dir()?)?,
                "env": {},
                "tty": false,
                "pipeStdin": true,
                "arg0": null
            }),
        )
        .await?;
    let _ = server
        .wait_for_event(|event| {
            matches!(
                event,
                JSONRPCMessage::Response(JSONRPCResponse { id, .. }) if id == &process_start_id
            )
        })
        .await?;

    for (write_id, chunk) in [
        ("write-1", "Zmlyc3QK"),
        ("write-1", "Zmlyc3QK"),
        ("write-2", "c2Vjb25kCg=="),
    ] {
        let request_id = server
            .send_request(
                "process/write",
                serde_json::json!({
                    "processId": "proc-write-id",
                    "chunk": chunk,
                    "writeId": write_id
                }),
            )
            .await?;
        let response = server
            .wait_for_event(|event| {
                matches!(
                    event,
                    JSONRPCMessage::Response(JSONRPCResponse { id, .. }) if id == &request_id
                )
            })
            .await?;
        let JSONRPCMessage::Response(JSONRPCResponse { result, .. }) = response else {
            panic!("expected process/write response");
        };
        let write_response: WriteResponse = serde_json::from_value(result)?;
        assert_eq!(
            write_response,
            WriteResponse {
                status: WriteStatus::Accepted
            }
        );
    }

    let mut after_seq = None;
    let mut output = Vec::new();
    for _ in 0..5 {
        let read_id = server
            .send_request(
                "process/read",
                serde_json::json!({
                    "processId": "proc-write-id",
                    "afterSeq": after_seq,
                    "maxBytes": null,
                    "waitMs": 1000
                }),
            )
            .await?;
        let response = server
            .wait_for_event(|event| {
                matches!(
                    event,
                    JSONRPCMessage::Response(JSONRPCResponse { id, .. }) if id == &read_id
                )
            })
            .await?;
        let JSONRPCMessage::Response(JSONRPCResponse { result, .. }) = response else {
            panic!("expected process/read response");
        };
        let read_response: ReadResponse = serde_json::from_value(result)?;
        output.extend(
            read_response
                .chunks
                .into_iter()
                .flat_map(|chunk| chunk.chunk.into_inner()),
        );
        after_seq = Some(read_response.next_seq.saturating_sub(1));
        if read_response.closed || output.ends_with(b"line:second\n") {
            break;
        }
    }

    assert_eq!(
        String::from_utf8(output)?,
        "line:first\nline:second\n".to_string()
    );

    server.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exec_server_resumes_detached_session_without_killing_processes() -> anyhow::Result<()> {
    let mut server = exec_server().await?;
    let initialize_id = server
        .send_request(
            "initialize",
            serde_json::to_value(InitializeParams {
                client_name: "exec-server-test".to_string(),
                resume_session_id: None,
            })?,
        )
        .await?;
    let response = server
        .wait_for_event(|event| {
            matches!(
                event,
                JSONRPCMessage::Response(JSONRPCResponse { id, .. }) if id == &initialize_id
            )
        })
        .await
        .context("waiting for the initialize response")?;
    let JSONRPCMessage::Response(JSONRPCResponse { result, .. }) = response else {
        panic!("expected initialize response");
    };
    let initialize_response: InitializeResponse = serde_json::from_value(result)?;

    server
        .send_notification("initialized", serde_json::json!({}))
        .await?;

    let process_start_id = server
        .send_request(
            "process/start",
            serde_json::json!({
                "processId": "proc-resume",
                "argv": ["/bin/sh", "-c", "sleep 30"],
                "cwd": PathUri::from_host_native_path(std::env::current_dir()?)?,
                "env": {},
                "tty": false,
                "pipeStdin": false,
                "arg0": null
            }),
        )
        .await?;
    let _ = server
        .wait_for_event(|event| {
            matches!(
                event,
                JSONRPCMessage::Response(JSONRPCResponse { id, .. }) if id == &process_start_id
            )
        })
        .await
        .context("waiting for the process/start response")?;

    server.disconnect_websocket().await?;
    server.reconnect_websocket().await?;

    // The server detaches the old connection only after it has processed the
    // close, so a fast resume can see "already attached" (#796). The real
    // client retries that error (`client_recovery.rs`); so does the test.
    let resume_deadline = Instant::now() + RESUME_ATTACHED_RETRY_BUDGET;
    let result = loop {
        let resume_initialize_id = server
            .send_request(
                "initialize",
                serde_json::to_value(InitializeParams {
                    client_name: "exec-server-test".to_string(),
                    resume_session_id: Some(initialize_response.session_id.clone()),
                })?,
            )
            .await?;
        let response = server
            .wait_for_event(|event| match event {
                JSONRPCMessage::Response(JSONRPCResponse { id, .. })
                | JSONRPCMessage::Error(JSONRPCError { id, .. }) => id == &resume_initialize_id,
                _ => false,
            })
            .await
            .context("waiting for the resume initialize response")?;
        match response {
            JSONRPCMessage::Response(JSONRPCResponse { result, .. }) => break result,
            JSONRPCMessage::Error(JSONRPCError { error, .. })
                if error.code == SESSION_ALREADY_ATTACHED_ERROR_CODE
                    && Instant::now() < resume_deadline =>
            {
                tokio::time::sleep(RESUME_ATTACHED_RETRY_INTERVAL).await;
            }
            other => anyhow::bail!("resume initialize failed: {other:?}"),
        }
    };
    let resumed_response: InitializeResponse = serde_json::from_value(result)?;
    assert_eq!(resumed_response, initialize_response);

    server
        .send_notification("initialized", serde_json::json!({}))
        .await?;

    let process_read_id = server
        .send_request(
            "process/read",
            serde_json::json!({
                "processId": "proc-resume",
                "afterSeq": null,
                "maxBytes": null,
                "waitMs": 0
            }),
        )
        .await?;
    let response = server
        .wait_for_event(|event| {
            matches!(
                event,
                JSONRPCMessage::Response(JSONRPCResponse { id, .. }) if id == &process_read_id
            )
        })
        .await
        .context("waiting for the process/read response")?;
    let JSONRPCMessage::Response(JSONRPCResponse { result, .. }) = response else {
        panic!("expected process/read response");
    };
    let process_read_response: ReadResponse = serde_json::from_value(result)?;
    assert!(process_read_response.failure.is_none());
    assert!(!process_read_response.exited);
    assert!(!process_read_response.closed);

    let terminate_id = server
        .send_request(
            "process/terminate",
            serde_json::json!({
                "processId": "proc-resume"
            }),
        )
        .await?;
    let response = server
        .wait_for_event(|event| {
            matches!(
                event,
                JSONRPCMessage::Response(JSONRPCResponse { id, .. }) if id == &terminate_id
            )
        })
        .await
        .context("waiting for the process/terminate response")?;
    let JSONRPCMessage::Response(JSONRPCResponse { result, .. }) = response else {
        panic!("expected process/terminate response");
    };
    let terminate_response: TerminateResponse = serde_json::from_value(result)?;
    assert_eq!(terminate_response, TerminateResponse { running: true });

    server.shutdown().await?;
    Ok(())
}
