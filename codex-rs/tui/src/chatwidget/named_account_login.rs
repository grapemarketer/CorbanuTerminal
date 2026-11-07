//! Dismissing named-account sign-in cancels pending authorization. Once saving
//! starts, the task keeps ownership of persistence and delivers its final result.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;
use std::time::Duration;
use std::time::Instant;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;

use super::ChatWidget;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::ViewCompletion;
use crate::provider_named_accounts::ProviderAccountEvent;
use crate::render::renderable::Renderable;

pub(crate) struct NamedAccountLoginCode {
    pub(crate) verification_url: String,
    pub(crate) user_code: String,
}

const WAITING: u8 = 0;
const PERSISTING: u8 = 1;
const CANCELLED: u8 = 2;
const FINISHED: u8 = 3;

/// Arbitrates cancellation against the first non-abortable credential write.
#[derive(Clone, Default)]
pub(crate) struct NamedAccountLoginState(Arc<AtomicU8>);

impl NamedAccountLoginState {
    pub(crate) fn begin_persistence(&self) -> std::io::Result<()> {
        self.0
            .compare_exchange(WAITING, PERSISTING, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::Interrupted, "Sign-in cancelled"))
    }

    fn cancel_pending(&self) -> bool {
        self.0
            .compare_exchange(WAITING, CANCELLED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn is_persisting(&self) -> bool {
        self.0.load(Ordering::Acquire) == PERSISTING
    }

    pub(crate) fn finish(&self, sender: &AppEventSender, result: Result<String, String>) {
        if matches!(
            self.0.swap(FINISHED, Ordering::AcqRel),
            WAITING | PERSISTING
        ) {
            sender.send(AppEvent::ProviderAccount(ProviderAccountEvent::Finished {
                result,
            }));
        }
    }
}

impl ChatWidget {
    pub(crate) fn open_named_account_login(
        &mut self,
        account_label: String,
        codes: UnboundedReceiver<NamedAccountLoginCode>,
        task: JoinHandle<()>,
        state: NamedAccountLoginState,
    ) {
        self.bottom_pane.show_view(Box::new(NamedAccountLoginView {
            account_label,
            app_event_tx: self.app_event_tx.clone(),
            codes,
            code: None,
            task,
            state,
            completion: None,
        }));
    }
}

struct NamedAccountLoginView {
    account_label: String,
    app_event_tx: AppEventSender,
    codes: UnboundedReceiver<NamedAccountLoginCode>,
    code: Option<NamedAccountLoginCode>,
    task: JoinHandle<()>,
    state: NamedAccountLoginState,
    completion: Option<ViewCompletion>,
}

impl NamedAccountLoginView {
    fn poll_updates(&mut self) -> bool {
        if self.completion.is_some() {
            return false;
        }
        let mut changed = false;
        while let Ok(code) = self.codes.try_recv() {
            self.code = Some(code);
            changed = true;
        }
        if let Poll::Ready(result) =
            Pin::new(&mut self.task).poll(&mut Context::from_waker(Waker::noop()))
        {
            if result.is_err() {
                self.state.finish(
                    &self.app_event_tx,
                    Err(format!(
                        "Sign-in of {} did not complete.",
                        self.account_label
                    )),
                );
            }
            self.completion = Some(ViewCompletion::Accepted);
            self.code = None;
            changed = true;
        }
        changed
    }

    fn cancel(&mut self) {
        // A result already delivered by the task wins over a late Escape.
        self.poll_updates();
        if self.completion.is_some() {
            return;
        }
        if self.state.cancel_pending() {
            self.task.abort();
            self.app_event_tx
                .send(AppEvent::ProviderAccount(ProviderAccountEvent::Finished {
                    result: Ok(format!("Cancelled sign-in of {}.", self.account_label)),
                }));
        }
        self.codes.close();
        self.code = None;
        self.completion = Some(ViewCompletion::Cancelled);
    }

    fn lines(&self, width: u16) -> Vec<Line<'static>> {
        if self.completion.is_some() {
            return Vec::new();
        }
        let mut lines = vec![
            Line::from(format!("Sign in {}", self.account_label).bold()),
            Line::from(""),
        ];
        if self.state.is_persisting() {
            lines.push(Line::from("Finishing the sign-in…").dim());
            lines.push(
                Line::from("Esc or Ctrl-C closes this screen; the sign-in result will appear.")
                    .dim(),
            );
            return crate::wrapping::word_wrap_lines(&lines, width.max(1) as usize);
        } else if let Some(code) = &self.code {
            lines.extend([
                Line::from("Open this link in your browser and sign in:"),
                Line::from(code.verification_url.clone().cyan().underlined()),
                Line::from(""),
                Line::from("Enter this one-time code:"),
                Line::from(code.user_code.clone().cyan().bold()),
                Line::from(""),
                Line::from("Device codes are a common phishing target. Never share this code.")
                    .dim(),
                Line::from("Waiting for the sign-in…").dim(),
            ]);
        } else {
            lines.push(Line::from("Requesting a one-time device code…").dim());
        }
        lines.push(Line::from("Esc or Ctrl-C cancels the sign-in.").dim());
        crate::wrapping::word_wrap_lines(&lines, width.max(1) as usize)
    }
}

impl Drop for NamedAccountLoginView {
    fn drop(&mut self) {
        // A pending request stops, but a started save and its real result must
        // survive dismissal. The task delivers that result through the app.
        if self.state.cancel_pending() {
            self.task.abort();
        }
    }
}

impl BottomPaneView for NamedAccountLoginView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        if key_event.code == KeyCode::Esc {
            self.cancel();
        }
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.cancel();
        CancellationEvent::Handled
    }

    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }

    fn is_complete(&self) -> bool {
        self.completion.is_some()
    }

    fn completion(&self) -> Option<ViewCompletion> {
        self.completion
    }

    fn pre_draw_tick(&mut self, _now: Instant) -> bool {
        self.poll_updates()
    }

    fn next_frame_delay(&self) -> Option<Duration> {
        self.completion
            .is_none()
            .then_some(Duration::from_millis(50))
    }
}

impl Renderable for NamedAccountLoginView {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self.lines(area.width)).render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.lines(width).len() as u16
    }
}

#[cfg(test)]
#[path = "named_account_login_tests.rs"]
mod tests;
