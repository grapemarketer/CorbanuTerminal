use super::*;
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

struct NotifyDrop(Option<oneshot::Sender<()>>);

impl Drop for NotifyDrop {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

fn view_with_task<F>(
    sign_in: impl FnOnce(NamedAccountLoginState) -> F,
) -> (
    NamedAccountLoginView,
    UnboundedSender<NamedAccountLoginCode>,
    UnboundedReceiver<AppEvent>,
)
where
    F: Future<Output = Result<String, String>> + Send + 'static,
{
    let (events, rx) = tokio::sync::mpsc::unbounded_channel();
    let (codes, code_rx) = tokio::sync::mpsc::unbounded_channel();
    let sender = AppEventSender::new(events);
    let state = NamedAccountLoginState::default();
    let future = sign_in(state.clone());
    let task_state = state.clone();
    let task_sender = sender.clone();
    let task = tokio::spawn(async move {
        task_state.finish(&task_sender, future.await);
    });
    (
        NamedAccountLoginView {
            account_label: "openai account `work`".to_string(),
            app_event_tx: sender,
            codes: code_rx,
            code: None,
            task,
            state,
            completion: None,
        },
        codes,
        rx,
    )
}

async fn pending_view() -> (
    NamedAccountLoginView,
    UnboundedSender<NamedAccountLoginCode>,
    UnboundedReceiver<AppEvent>,
    oneshot::Receiver<()>,
) {
    let (started, running) = oneshot::channel();
    let (dropped, stopped) = oneshot::channel();
    let (view, codes, events) = view_with_task(move |_state| async move {
        let _drop = NotifyDrop(Some(dropped));
        let _ = started.send(());
        std::future::pending().await
    });
    running.await.expect("sign-in task started");
    (view, codes, events, stopped)
}

fn send_code(codes: &UnboundedSender<NamedAccountLoginCode>) {
    assert!(
        codes
            .send(NamedAccountLoginCode {
                verification_url: "https://example.com/device".to_string(),
                user_code: "TEST-CODE".to_string(),
            })
            .is_ok(),
        "waiting view receives the code"
    );
}

fn feedback(events: &mut UnboundedReceiver<AppEvent>) -> Result<String, String> {
    match events.try_recv().expect("one account result") {
        AppEvent::ProviderAccount(ProviderAccountEvent::Finished { result }) => result,
        other => panic!("unexpected event: {other:?}"),
    }
}

async fn assert_stopped(stopped: oneshot::Receiver<()>) {
    tokio::time::timeout(Duration::from_secs(1), stopped)
        .await
        .expect("sign-in future stops promptly")
        .expect("sign-in future was dropped");
}

#[tokio::test]
async fn issue1290_escape_stops_both_requesting_and_waiting_tasks() {
    for ready in [false, true] {
        let (mut view, codes, mut events, stopped) = pending_view().await;
        if ready {
            send_code(&codes);
            view.pre_draw_tick(Instant::now());
        }
        view.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_stopped(stopped).await;
        assert_eq!(view.completion(), Some(ViewCompletion::Cancelled));
        assert!(view.lines(/*width*/ 80).is_empty());
        assert_eq!(
            feedback(&mut events),
            Ok("Cancelled sign-in of openai account `work`.".to_string())
        );
        assert!(codes.is_closed());
        assert_eq!(
            view.state.begin_persistence().unwrap_err().kind(),
            std::io::ErrorKind::Interrupted
        );
        view.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!view.pre_draw_tick(Instant::now()));
        assert!(events.try_recv().is_err());
    }
}

#[tokio::test]
async fn issue1290_ctrl_c_and_view_drop_stop_pending_sign_in() {
    let (mut view, codes, mut events, stopped) = pending_view().await;
    send_code(&codes);
    view.pre_draw_tick(Instant::now());
    assert_eq!(view.on_ctrl_c(), CancellationEvent::Handled);
    assert_stopped(stopped).await;
    assert_eq!(view.completion(), Some(ViewCompletion::Cancelled));
    assert_eq!(
        feedback(&mut events),
        Ok("Cancelled sign-in of openai account `work`.".to_string())
    );

    let (view, codes, mut events, stopped) = pending_view().await;
    drop(view);
    assert_stopped(stopped).await;
    assert!(codes.is_closed());
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn issue1290_chatwidget_routes_escape_and_ctrl_c_to_the_named_login() {
    for key in [
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    ] {
        let (mut chat, _sender, mut events, _ops) =
            crate::chatwidget::tests::make_chatwidget_manual_with_sender().await;
        while events.try_recv().is_ok() {}
        let (started, running) = oneshot::channel();
        let (dropped, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _drop = NotifyDrop(Some(dropped));
            let _ = started.send(());
            std::future::pending().await
        });
        running.await.expect("sign-in task started");
        let (codes, code_rx) = tokio::sync::mpsc::unbounded_channel();
        chat.open_named_account_login(
            "openai account `work`".to_string(),
            code_rx,
            task,
            NamedAccountLoginState::default(),
        );
        send_code(&codes);
        chat.bottom_pane.pre_draw_tick();
        assert!(!chat.bottom_pane.no_modal_or_popup_active());
        chat.handle_key_event(key);
        assert_stopped(stopped).await;
        assert!(chat.bottom_pane.no_modal_or_popup_active());
        assert_eq!(
            feedback(&mut events),
            Ok("Cancelled sign-in of openai account `work`.".to_string())
        );
        assert!(events.try_recv().is_err());
    }
}

#[tokio::test]
async fn issue1290_enter_keeps_the_cancellable_waiting_screen() {
    let (mut view, codes, mut events, stopped) = pending_view().await;
    send_code(&codes);
    view.pre_draw_tick(Instant::now());
    view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(!view.is_complete());
    assert!(view.code.is_some());
    assert!(events.try_recv().is_err());
    view.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_stopped(stopped).await;
}

#[tokio::test]
async fn issue1290_finished_success_and_failure_win_over_late_escape_once() {
    for expected in [
        Ok("Signed in openai account `work`.".to_string()),
        Err("The device code expired. Try signing in again.".to_string()),
    ] {
        let result = expected.clone();
        let (mut view, codes, mut events) = view_with_task(move |_state| async move { result });
        send_code(&codes);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !view.task.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("sign-in result ready");
        view.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(view.completion(), Some(ViewCompletion::Accepted));
        assert_eq!(feedback(&mut events), expected);
        assert!(view.lines(/*width*/ 80).is_empty());
        assert!(!view.pre_draw_tick(Instant::now()));
        assert!(events.try_recv().is_err());
    }
}

#[tokio::test]
async fn issue1290_persistence_error_survives_dismissal_without_closing_a_retry() {
    for drop_view in [false, true] {
        let (started, running) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let (mut first, _codes, mut first_events) = view_with_task(move |state| async move {
            state
                .begin_persistence()
                .expect("persistence won cancellation");
            let _ = started.send(());
            released.await.expect("release persistence error");
            Err("The account could not be saved.".to_string())
        });
        running.await.expect("persistence started");
        if !drop_view {
            first.on_ctrl_c();
        }
        drop(first);
        assert!(
            first_events.try_recv().is_err(),
            "dismissal must not claim cancellation"
        );
        let (mut retry, codes, mut events, stopped) = pending_view().await;
        send_code(&codes);
        retry.pre_draw_tick(Instant::now());
        release
            .send(())
            .expect("persistence task still owns the result");
        let event = tokio::time::timeout(Duration::from_secs(1), first_events.recv())
            .await
            .expect("failure delivered after dismissal")
            .expect("result event");
        match event {
            AppEvent::ProviderAccount(ProviderAccountEvent::Finished { result }) => {
                assert_eq!(result, Err("The account could not be saved.".to_string()));
            }
            other => panic!("unexpected event: {other:?}"),
        }
        retry.pre_draw_tick(Instant::now());
        assert!(!retry.is_complete());
        assert_eq!(retry.code.as_ref().unwrap().user_code, "TEST-CODE");
        assert!(events.try_recv().is_err());
        assert!(first_events.try_recv().is_err());
        retry.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_stopped(stopped).await;
    }
}

#[tokio::test]
async fn issue1290_cancelled_attempt_cannot_deliver_into_a_retry() {
    let (mut first, old_codes, mut old_events, stopped) = pending_view().await;
    first.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_stopped(stopped).await;
    assert!(feedback(&mut old_events).is_ok());
    let (mut retry, codes, mut events, stopped) = pending_view().await;
    assert!(
        old_codes
            .send(NamedAccountLoginCode {
                verification_url: "https://example.com/stale".to_string(),
                user_code: "STALE-CODE".to_string(),
            })
            .is_err()
    );
    send_code(&codes);
    retry.pre_draw_tick(Instant::now());
    assert_eq!(retry.code.as_ref().unwrap().user_code, "TEST-CODE");
    assert!(old_events.try_recv().is_err());
    assert!(events.try_recv().is_err());
    retry.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_stopped(stopped).await;
}

#[tokio::test]
async fn issue1290_escape_drops_real_device_request_and_token_poll_futures() {
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    for waiting_for_token in [false, true] {
        let server = MockServer::start().await;
        let home = tempfile::tempdir().expect("isolated sign-in home");
        let mut opts = codex_login::ServerOptions::new(
            home.path().to_path_buf(),
            "issue1290-client".to_string(),
            /*forced_chatgpt_workspace_id*/ None,
            codex_login::AuthCredentialsStoreMode::File,
            codex_login::AuthKeyringBackendKind::default(),
            codex_login::test_support::transport_default_auth_route_config(),
        );
        opts.issuer = server.uri();
        opts.open_browser = false;
        opts.named_account = Some(codex_login::NamedChatgptAccount {
            provider_id: "openai".to_string(),
            name: "work".to_string(),
        });
        opts.named_account_must_be_new = true;
        let user_code = ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_auth_id": "issue1290-auth",
            "user_code": "TEST-CODE",
            "interval": "1",
        }));
        Mock::given(method("POST"))
            .and(path("/api/accounts/deviceauth/usercode"))
            .respond_with(if waiting_for_token {
                user_code
            } else {
                user_code.set_delay(Duration::from_secs(10))
            })
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/accounts/deviceauth/token"))
            .respond_with(ResponseTemplate::new(403).set_delay(Duration::from_secs(10)))
            .mount(&server)
            .await;
        let (dropped, stopped) = oneshot::channel();
        let (mut view, _codes, mut events) = view_with_task(move |state| async move {
            let _drop = NotifyDrop(Some(dropped));
            let code = codex_login::request_device_code(&opts)
                .await
                .map_err(|error| error.to_string())?;
            codex_login::complete_device_code_login_with_persistence_guard(opts, code, || {
                state.begin_persistence()
            })
            .await
            .map_err(|error| error.to_string())?;
            Ok("Unexpected sign-in".to_string())
        });
        let expected_path = if waiting_for_token {
            "/api/accounts/deviceauth/token"
        } else {
            "/api/accounts/deviceauth/usercode"
        };
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if server
                    .received_requests()
                    .await
                    .expect("recorded requests")
                    .iter()
                    .any(|request| request.url.path() == expected_path)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("real device-auth request reached the isolated issuer");
        view.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_stopped(stopped).await;
        assert_eq!(
            feedback(&mut events),
            Ok("Cancelled sign-in of openai account `work`.".to_string())
        );
        assert!(
            std::fs::read_dir(home.path())
                .expect("home contents")
                .next()
                .is_none()
        );
        assert!(events.try_recv().is_err());
    }
}

fn render(view: &NamedAccountLoginView, width: u16) -> String {
    let area = Rect::new(0, 0, width, view.desired_height(width));
    let mut buffer = Buffer::empty(area);
    view.render(area, &mut buffer);
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn issue1290_named_account_login_snapshots() {
    let (mut view, codes, _events, stopped) = pending_view().await;
    insta::assert_snapshot!(
        "issue1290_requesting_device_code",
        render(&view, /*width*/ 80)
    );
    send_code(&codes);
    view.pre_draw_tick(Instant::now());
    insta::assert_snapshot!(
        "issue1290_waiting_for_device_sign_in",
        render(&view, /*width*/ 80)
    );
    insta::assert_snapshot!(
        "issue1290_waiting_for_device_sign_in_narrow",
        render(&view, /*width*/ 32)
    );
    view.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert_stopped(stopped).await;
}

#[tokio::test]
async fn issue1290_finishing_sign_in_snapshot() {
    let (mut view, _codes, _events, stopped) = pending_view().await;
    view.state.begin_persistence().expect("persistence won");
    insta::assert_snapshot!("issue1290_finishing_sign_in", render(&view, /*width*/ 80));
    // This renderer fixture has no save to finish; release its synthetic task.
    view.task.abort();
    assert_stopped(stopped).await;
    view.pre_draw_tick(Instant::now());
}
