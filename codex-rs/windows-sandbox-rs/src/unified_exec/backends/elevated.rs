use super::windows_common::finish_driver_spawn;
use super::windows_common::make_runner_resizer;
use super::windows_common::start_runner_pipe_writer_from;
use super::windows_common::start_runner_stdin_writer;
use super::windows_common::start_runner_stdout_reader;
use crate::identity::SandboxCreds;
use crate::identity::refresh_logon_sandbox_creds;
use crate::integrity_leases::IntegrityLease;
use crate::ipc_framed::EmptyPayload;
use crate::ipc_framed::FramedMessage;
use crate::ipc_framed::IPC_PROTOCOL_VERSION;
use crate::ipc_framed::Message;
use crate::ipc_framed::SpawnRequest;
use crate::resolved_permissions::ResolvedWindowsSandboxPermissions;
use crate::runner_client::RunnerTransport;
use crate::runner_client::retry_runner_spawn_once;
use crate::runner_client::spawn_runner_transport;
use crate::spawn_prep::prepare_elevated_spawn_context_for_permissions;
use anyhow::Result;
use codex_protocol::models::PermissionProfile;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_pty::ProcessDriver;
use codex_utils_pty::SpawnedProcess;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::oneshot;

struct RunnerTransportRequest {
    permissions: ResolvedWindowsSandboxPermissions,
    codex_home: PathBuf,
    cwd: PathBuf,
    env_map: HashMap<String, String>,
    logs_base_dir: Option<PathBuf>,
    spawn_request: SpawnRequest,
    read_roots_override: Option<Vec<PathBuf>>,
    read_roots_include_platform_defaults: bool,
    write_roots_override: Option<Vec<PathBuf>>,
    deny_read_override: crate::DenyReadTargets,
    deny_write_paths_override: Vec<PathBuf>,
    proxy_enforced: bool,
    proxy_settings_mode: crate::WindowsSandboxProxySettingsMode,
}

fn spawn_runner_transport_with_retry<T>(
    sandbox_creds: SandboxCreds,
    request: &RunnerTransportRequest,
    mut spawn: impl FnMut(&Path, &Path, &SandboxCreds, Option<&Path>, SpawnRequest) -> Result<T>,
    refresh: impl FnOnce(
        &ResolvedWindowsSandboxPermissions,
        &Path,
        &HashMap<String, String>,
        &Path,
        Option<&[PathBuf]>,
        bool,
        Option<&[PathBuf]>,
        &crate::DenyReadTargets,
        &[PathBuf],
        bool,
        crate::WindowsSandboxProxySettingsMode,
    ) -> Result<SandboxCreds>,
) -> Result<T> {
    retry_runner_spawn_once(
        sandbox_creds,
        &request.spawn_request.command,
        |sandbox_creds| {
            spawn(
                &request.codex_home,
                &request.cwd,
                &sandbox_creds,
                request.logs_base_dir.as_deref(),
                request.spawn_request.clone(),
            )
        },
        || {
            refresh(
                &request.permissions,
                &request.cwd,
                &request.env_map,
                &request.codex_home,
                request.read_roots_override.as_deref(),
                request.read_roots_include_platform_defaults,
                request.write_roots_override.as_deref(),
                &request.deny_read_override,
                &request.deny_write_paths_override,
                request.proxy_enforced,
                request.proxy_settings_mode,
            )
        },
    )
}

/// The runner, started off the async thread. `planted_watch` comes back with
/// it; if the runner can't start (or the caller is gone), it ends there too
/// (#1896 item 4: ending it walks the writable roots).
async fn spawn_runner_transport_task(
    sandbox_creds: SandboxCreds,
    request: RunnerTransportRequest,
    planted_watch: Option<IntegrityLease>,
) -> Result<(RunnerTransport, Option<IntegrityLease>)> {
    tokio::task::spawn_blocking(move || -> Result<_> {
        let transport = spawn_runner_transport_with_retry(
            sandbox_creds,
            &request,
            spawn_runner_transport,
            refresh_logon_sandbox_creds,
        )?;
        Ok((transport, planted_watch))
    })
    .await
    .map_err(|err| anyhow::anyhow!("runner handshake task failed: {err}"))?
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn spawn_windows_sandbox_session_elevated_for_permission_profile(
    permission_profile: &PermissionProfile,
    workspace_roots: &[AbsolutePathBuf],
    codex_home: &Path,
    command: Vec<String>,
    cwd: &Path,
    mut env_map: HashMap<String, String>,
    proxy_enforced: bool,
    network_proxy_restricting_sid: Option<String>,
    proxy_settings_mode: crate::WindowsSandboxProxySettingsMode,
    timeout_ms: Option<u64>,
    read_roots_override: Option<&[PathBuf]>,
    read_roots_include_platform_defaults: bool,
    write_roots_override: Option<&[PathBuf]>,
    deny_read_override: &crate::DenyReadTargets,
    deny_write_paths_override: &[AbsolutePathBuf],
    tty: bool,
    stdin_open: bool,
    use_private_desktop: bool,
) -> Result<SpawnedProcess> {
    let deny_write_paths_override = deny_write_paths_override
        .iter()
        .map(AbsolutePathBuf::to_path_buf)
        .collect::<Vec<_>>();
    let permissions =
        ResolvedWindowsSandboxPermissions::try_from_permission_profile_for_workspace_roots(
            permission_profile,
            workspace_roots,
        )?;
    // #442 (Travis, 2026-10-12): a credential file that appears while the
    // command runs stops it, as on Linux. The watch starts before the runner;
    // its stop is queued for the runner if it comes first. It starts before the
    // sandbox (and the deny ACEs) are set up for this command.
    let (outbound_tx, outbound_rx) = std::sync::mpsc::channel::<FramedMessage>();
    // #1896 item 4: the credential watch's first scan, setup's checks and the
    // start of the planted-folder watch (D425), which walks the writable
    // roots, block for a while, so they run off the async thread, in the same
    // order.
    let (credential_watch, elevated, prepared_env) = {
        let stop_tx = outbound_tx.clone();
        let (file_system, _) = permission_profile.to_runtime_permissions();
        let known: Vec<PathBuf> = deny_read_override
            .paths()
            .into_iter()
            .map(AbsolutePathBuf::into_path_buf)
            .collect();
        let permissions = permissions.clone();
        let codex_home = codex_home.to_path_buf();
        let cwd = cwd.to_path_buf();
        let command = command.clone();
        let read_roots_override = read_roots_override.map(<[PathBuf]>::to_vec);
        let write_roots_override = write_roots_override.map(<[PathBuf]>::to_vec);
        let deny_read_override = deny_read_override.clone();
        let deny_write_paths_override = deny_write_paths_override.clone();
        let mut env_map = std::mem::take(&mut env_map);
        tokio::task::spawn_blocking(move || -> Result<_> {
            let credential_watch = crate::credential_watch::CredentialWatch::start(
                &file_system,
                &cwd,
                &known,
                Box::new(move || {
                    let _ = stop_tx.send(FramedMessage {
                        version: IPC_PROTOCOL_VERSION,
                        message: Message::Terminate {
                            payload: EmptyPayload::default(),
                        },
                    });
                }),
            )
            .map_err(anyhow::Error::msg)?;
            let elevated = prepare_elevated_spawn_context_for_permissions(
                permissions,
                &codex_home,
                &cwd,
                &mut env_map,
                &command,
                read_roots_override.as_deref(),
                read_roots_include_platform_defaults,
                write_roots_override.as_deref(),
                &deny_read_override,
                &deny_write_paths_override,
                proxy_enforced,
                proxy_settings_mode,
            )?;
            Ok((credential_watch, elevated, env_map))
        })
        .await
        .map_err(|err| anyhow::anyhow!("elevated spawn preparation task failed: {err}"))??
    };
    env_map = prepared_env;

    let sandbox_creds = elevated.sandbox_creds;
    let planted_watch = elevated.planted_watch;
    let request = RunnerTransportRequest {
        permissions,
        codex_home: codex_home.to_path_buf(),
        cwd: cwd.to_path_buf(),
        env_map: env_map.clone(),
        logs_base_dir: elevated.logs_base_dir,
        spawn_request: SpawnRequest {
            command,
            cwd: cwd.to_path_buf(),
            env: env_map,
            permission_profile: permission_profile.clone(),
            workspace_roots: workspace_roots.to_vec(),
            codex_home: elevated.sandbox_base,
            real_codex_home: codex_home.to_path_buf(),
            cap_sids: elevated.cap_sids,
            network_proxy_restricting_sid,
            timeout_ms,
            tty,
            stdin_open,
            use_private_desktop,
        },
        read_roots_override: read_roots_override.map(<[PathBuf]>::to_vec),
        read_roots_include_platform_defaults,
        write_roots_override: write_roots_override.map(<[PathBuf]>::to_vec),
        deny_read_override: deny_read_override.clone(),
        deny_write_paths_override,
        proxy_enforced,
        proxy_settings_mode,
    };
    let (transport, planted_watch) =
        spawn_runner_transport_task(sandbox_creds, request, planted_watch).await?;
    let (pipe_write, pipe_read) = transport.into_files();

    let (writer_tx, writer_rx) = mpsc::channel::<Vec<u8>>(128);
    let (stdout_tx, stdout_rx) = broadcast::channel::<Vec<u8>>(256);
    let stderr_rx = if tty {
        None
    } else {
        Some(broadcast::channel::<Vec<u8>>(256))
    };
    let (exit_tx, exit_rx) = oneshot::channel::<i32>();

    start_runner_pipe_writer_from(pipe_write, outbound_rx);
    let writer_handle = start_runner_stdin_writer(writer_rx, outbound_tx.clone(), tty, stdin_open);
    let terminator = {
        let outbound_tx = outbound_tx.clone();
        Some(Box::new(move || {
            let _ = outbound_tx.send(FramedMessage {
                version: IPC_PROTOCOL_VERSION,
                message: Message::Terminate {
                    payload: EmptyPayload::default(),
                },
            });
        }) as Box<dyn FnMut() + Send + Sync>)
    };

    start_runner_stdout_reader(
        pipe_read,
        stdout_tx,
        stderr_rx.as_ref().map(|(tx, _rx)| tx.clone()),
        exit_tx,
        credential_watch,
        planted_watch,
    );

    Ok(finish_driver_spawn(
        ProcessDriver {
            writer_tx,
            stdout_rx,
            stderr_rx: stderr_rx.map(|(_tx, rx)| rx),
            exit_rx,
            terminator,
            writer_handle: Some(writer_handle),
            resizer: if tty {
                Some(make_runner_resizer(outbound_tx))
            } else {
                None
            },
            tty,
        },
        stdin_open,
    ))
}

#[cfg(test)]
#[path = "elevated_tests.rs"]
mod tests;
