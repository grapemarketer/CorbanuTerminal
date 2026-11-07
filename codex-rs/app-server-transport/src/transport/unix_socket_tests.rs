use super::AppServerTransport;
use super::CHANNEL_CAPACITY;
use super::TransportEvent;
use super::acquire_app_server_startup_lock;
use super::app_server_control_socket_path;
use super::start_control_socket_acceptor;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::JSONRPCNotification;
use codex_core::config::find_codex_home;
use codex_uds::UnixStream;
use codex_utils_absolute_path::AbsolutePathBuf;
use futures::SinkExt;
use futures::StreamExt;
use pretty_assertions::assert_eq;
use std::io::Result as IoResult;
use std::path::Path;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::client_async;
use tokio_tungstenite::tungstenite::Bytes;
use tokio_tungstenite::tungstenite::Message as WebSocketMessage;
use tokio_util::sync::CancellationToken;

#[test]
fn listen_unix_socket_parses_as_unix_socket_transport() {
    assert_eq!(
        AppServerTransport::from_listen_url("unix://"),
        Ok(AppServerTransport::UnixSocket {
            socket_path: default_control_socket_path()
        })
    );
}

#[test]
fn listen_unix_socket_accepts_absolute_custom_path() {
    assert_eq!(
        AppServerTransport::from_listen_url("unix:///tmp/codex.sock"),
        Ok(AppServerTransport::UnixSocket {
            socket_path: absolute_path("/tmp/codex.sock")
        })
    );
}

#[test]
fn listen_unix_socket_accepts_relative_custom_path() {
    assert_eq!(
        AppServerTransport::from_listen_url("unix://codex.sock"),
        Ok(AppServerTransport::UnixSocket {
            socket_path: AbsolutePathBuf::relative_to_current_dir("codex.sock")
                .expect("relative path should resolve")
        })
    );
}

#[tokio::test]
async fn control_socket_acceptor_upgrades_and_forwards_websocket_text_messages_and_pings() {
    let temp_dir = tempfile::TempDir::new().expect("temp dir");
    let socket_path = test_socket_path(temp_dir.path());
    let (transport_event_tx, mut transport_event_rx) =
        mpsc::channel::<TransportEvent>(CHANNEL_CAPACITY);
    let shutdown_token = CancellationToken::new();
    let accept_handle = start_control_socket_acceptor(
        socket_path.clone(),
        transport_event_tx,
        shutdown_token.clone(),
    )
    .await
    .expect("control socket acceptor should start");

    let stream = connect_to_socket(socket_path.as_path())
        .await
        .expect("client should connect");
    let (mut websocket, response) = client_async("ws://localhost/rpc", stream)
        .await
        .expect("websocket upgrade should complete");
    assert_eq!(response.status().as_u16(), 101);

    let opened = timeout(Duration::from_secs(1), transport_event_rx.recv())
        .await
        .expect("connection opened event should arrive")
        .expect("connection opened event");
    let connection_id = match opened {
        TransportEvent::ConnectionOpened { connection_id, .. } => connection_id,
        _ => panic!("expected connection opened event"),
    };

    let notification = JSONRPCMessage::Notification(JSONRPCNotification {
        method: "initialized".to_string(),
        params: None,
    });
    websocket
        .send(WebSocketMessage::Text(
            serde_json::to_string(&notification)
                .expect("notification should serialize")
                .into(),
        ))
        .await
        .expect("notification should send");

    let incoming = timeout(Duration::from_secs(1), transport_event_rx.recv())
        .await
        .expect("incoming message event should arrive")
        .expect("incoming message event");
    assert_eq!(
        match incoming {
            TransportEvent::IncomingMessage {
                connection_id: incoming_connection_id,
                message,
            } => (incoming_connection_id, message),
            _ => panic!("expected incoming message event"),
        },
        (connection_id, notification)
    );

    websocket
        .send(WebSocketMessage::Ping(Bytes::from_static(b"check")))
        .await
        .expect("ping should send");
    let pong = timeout(Duration::from_secs(1), websocket.next())
        .await
        .expect("pong should arrive")
        .expect("pong frame")
        .expect("pong should be valid");
    assert_eq!(pong, WebSocketMessage::Pong(Bytes::from_static(b"check")));

    websocket.close(None).await.expect("close should send");
    let closed = timeout(Duration::from_secs(1), transport_event_rx.recv())
        .await
        .expect("connection closed event should arrive")
        .expect("connection closed event");
    assert!(matches!(
        closed,
        TransportEvent::ConnectionClosed {
            connection_id: closed_connection_id,
        } if closed_connection_id == connection_id
    ));

    shutdown_token.cancel();
    accept_handle.await.expect("acceptor should join");
    assert_socket_path_removed(socket_path.as_path());
}

#[tokio::test]
async fn app_server_startup_lock_serializes_waiters() {
    let temp_dir = tempfile::TempDir::new().expect("temp dir");
    let lock_path = test_startup_lock_path(temp_dir.path());
    let first_lock = acquire_app_server_startup_lock(lock_path.clone())
        .await
        .expect("first startup lock should succeed");
    let mut second_lock = tokio::spawn(acquire_app_server_startup_lock(lock_path));

    assert!(
        timeout(Duration::from_millis(100), &mut second_lock)
            .await
            .is_err()
    );

    drop(first_lock);
    second_lock
        .await
        .expect("second startup lock task should join")
        .expect("second startup lock should succeed");
}

/// #928: a dropped startup lock is free at once, even while a child that
/// another thread forked still holds a copy of its descriptor.
#[cfg(unix)]
#[tokio::test]
async fn app_server_startup_lock_is_released_while_a_forked_child_holds_its_descriptor()
-> anyhow::Result<()> {
    use std::io::Read;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    /// Lets the forked child start its program when dropped, on every path.
    struct ReleaseChild(std::io::PipeWriter);

    impl Drop for ReleaseChild {
        fn drop(&mut self) {
            let _ = self.0.write_all(b"r");
        }
    }

    let temp_dir = tempfile::TempDir::new()?;
    let lock_path = test_startup_lock_path(temp_dir.path());
    let held = acquire_app_server_startup_lock(lock_path.clone()).await?;

    // A child forked now has a copy of every descriptor, the lock's too, until
    // it starts its program; this one reports the fork, then waits for the
    // release byte, so its copy stays open until the checks below have run.
    let (mut forked, mut forked_tx) = std::io::pipe()?;
    let (mut release_rx, release_tx) = std::io::pipe()?;
    let release_tx_fd = release_tx.as_raw_fd();
    let release = ReleaseChild(release_tx);
    let parent = std::process::id();
    let spawner = std::thread::spawn(move || {
        let mut command = std::process::Command::new("/bin/sh");
        command.args(["-c", "exit 0"]);
        // SAFETY: the hook makes only async-signal-safe calls and does not
        // allocate, so it is safe after fork.
        unsafe {
            command.pre_exec(move || {
                wait_for_release_in_child(release_tx_fd, parent, &mut forked_tx, &mut release_rx)
            });
        }
        command.status()
    });
    let mut byte = [0; 1];
    forked.read_exact(&mut byte)?;

    let other = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path.as_path())?;
    assert!(other.try_lock().is_err(), "the lock is held");
    drop(held);
    assert!(
        other.try_lock().is_ok(),
        "the dropped startup lock is still held through the forked child's copy"
    );
    drop(release);
    let status = spawner.join().expect("spawner thread")?;
    assert!(status.success(), "{status:?}");
    Ok(())
}

/// How long the forked child waits for its release before it fails the spawn
/// instead of waiting on.
#[cfg(unix)]
const RELEASE_WAIT_MS: libc::c_int = 60_000;

/// Runs in the forked child before exec: reports the fork, then waits for the
/// release byte, without outliving the test process (#991). The child closes
/// its copy of the write end, so the read sees EOF once this process dies; on
/// Linux the death of the forking thread also kills it. Elsewhere nothing
/// kills it, so once this process is gone (EOF, or `RELEASE_WAIT_MS` passed
/// and the child was re-parented) it goes on to run its program and exit 0:
/// failing the hook there would abort the child, since std cannot report the
/// error to a dead parent. A timeout while this process lives fails the spawn,
/// which fails the test.
#[cfg(unix)]
fn wait_for_release_in_child(
    release_tx_fd: std::os::fd::RawFd,
    parent: u32,
    forked_tx: &mut std::io::PipeWriter,
    release_rx: &mut std::io::PipeReader,
) -> std::io::Result<()> {
    use std::io::Read;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    // SAFETY: closes this child's own copy of the write end, which nothing
    // else in the child uses; the parent's copy stays open.
    if unsafe { libc::close(release_tx_fd) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: plain integer arguments; the kernel reads the signal as an
        // unsigned long.
        if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // The parent may have died before the signal was armed.
        // SAFETY: no arguments.
        if unsafe { libc::getppid() } as u32 != parent {
            return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
        }
    }
    forked_tx.write_all(b"f")?;
    let mut poll_fd = libc::pollfd {
        fd: release_rx.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: `poll_fd` is one valid, writable `pollfd`.
        match unsafe { libc::poll(&mut poll_fd, 1, RELEASE_WAIT_MS) } {
            0 => {
                #[cfg(not(target_os = "linux"))]
                {
                    // SAFETY: no arguments.
                    if unsafe { libc::getppid() } as u32 != parent {
                        return Ok(());
                    }
                }
                // std reports a hook error to the spawner by its OS error code.
                return Err(std::io::Error::from_raw_os_error(libc::ETIMEDOUT));
            }
            -1 => {
                let err = std::io::Error::last_os_error();
                if err.kind() != std::io::ErrorKind::Interrupted {
                    return Err(err);
                }
            }
            // Readable, or EOF if every write end is closed.
            #[cfg(target_os = "linux")]
            _ => return release_rx.read_exact(&mut [0; 1]),
            #[cfg(not(target_os = "linux"))]
            _ => {
                return match release_rx.read_exact(&mut [0; 1]) {
                    Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => Ok(()),
                    result => result,
                };
            }
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn control_socket_file_is_private_after_bind() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = tempfile::TempDir::new().expect("temp dir");
    let socket_path = test_socket_path(temp_dir.path());
    let (transport_event_tx, _transport_event_rx) =
        mpsc::channel::<TransportEvent>(CHANNEL_CAPACITY);
    let shutdown_token = CancellationToken::new();
    let accept_handle = start_control_socket_acceptor(
        socket_path.clone(),
        transport_event_tx,
        shutdown_token.clone(),
    )
    .await
    .expect("control socket acceptor should start");

    let metadata = tokio::fs::metadata(socket_path.as_path())
        .await
        .expect("socket metadata should exist");
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);

    shutdown_token.cancel();
    accept_handle.await.expect("acceptor should join");
}

fn absolute_path(path: &str) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path(path).expect("absolute path")
}

fn default_control_socket_path() -> AbsolutePathBuf {
    let codex_home = find_codex_home().expect("codex home");
    app_server_control_socket_path(&codex_home).expect("default control socket path")
}

fn test_socket_path(temp_dir: &Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path(
        temp_dir
            .join("app-server-control")
            .join("app-server-control.sock"),
    )
    .expect("socket path should resolve")
}

fn test_startup_lock_path(temp_dir: &Path) -> AbsolutePathBuf {
    AbsolutePathBuf::from_absolute_path(
        temp_dir
            .join("app-server-control")
            .join("app-server-startup.lock"),
    )
    .expect("startup lock path should resolve")
}

async fn connect_to_socket(socket_path: &Path) -> IoResult<UnixStream> {
    UnixStream::connect(socket_path).await
}

#[cfg(unix)]
fn assert_socket_path_removed(socket_path: &Path) {
    assert!(!socket_path.exists());
}

#[cfg(windows)]
fn assert_socket_path_removed(_socket_path: &Path) {
    // uds_windows uses a regular filesystem path as its rendezvous point,
    // but there is no Unix socket filesystem node to assert on.
}
