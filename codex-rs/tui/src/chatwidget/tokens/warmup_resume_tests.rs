//! Issue912: a billed warm-up is not proof that a conversation has saved turns.
//! Uses actual cost pages and the rollout reader used by resume, with owned data.
use super::*;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::RolloutLine;
use codex_protocol::protocol::SessionMeta;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::protocol::SessionSource;
use codex_state::accounting::OtherConversations;
use pretty_assertions::assert_eq;

fn warmup() -> ObservationQuote {
    let mut q = quote();
    q.attempt.turn = format!("side:prewarm:reopen:{}", q.attempt.thread_id);
    q.attempt.retry_of = None;
    q
}

fn warmup_packet() -> Inspection {
    let InspectionDay::Ready(mut view) = packet() else {
        unreachable!()
    };
    let q = warmup();
    view.requests = std::collections::BTreeMap::from([(q.attempt.request_id, vec![q])]);
    view.totals = DayTotals::from_quotes(view.requests.values().flatten()).unwrap();
    view.own_totals = view.totals.clone();
    view
}

async fn saved_turn_control(owner: ThreadId, append_turn: bool) -> bool {
    let home = tempfile::tempdir().unwrap();
    let meta = RolloutItem::SessionMeta(SessionMetaLine {
        meta: SessionMeta {
            id: owner,
            timestamp: "2026-10-16T01:00:00Z".into(),
            cwd: home.path().to_path_buf(),
            originator: "synthetic-cost-fixture".into(),
            cli_version: "test".into(),
            source: SessionSource::Cli,
            ..SessionMeta::default()
        },
        git: None,
    });
    let mut jsonl = serde_json::to_string(&RolloutLine {
        timestamp: "2026-10-16T01:00:00Z".into(),
        ordinal: None,
        item: meta,
    })
    .unwrap();
    jsonl.push('\n');
    if append_turn {
        jsonl.push_str(
            &serde_json::json!({
                "timestamp": "2026-10-16T01:00:01Z", "type": "event_msg",
                "payload": {"type": "user_message", "message": "synthetic saved turn",
                            "images": null, "local_images": [], "text_elements": []}
            })
            .to_string(),
        );
        jsonl.push('\n');
    }
    let path = home.path().join("synthetic-rollout.jsonl");
    std::fs::write(&path, jsonl).unwrap();
    codex_rollout::read_thread_item_from_rollout(path)
        .await
        .is_some()
}

fn assert_explained(lines: &[String]) {
    let text = lines.join("\n");
    assert!(
        text.contains("no saved turns")
            && text.contains("/resume")
            && text.contains("still billed"),
        "warm-up-only resume explanation missing:\n{text}"
    );
    assert!(
        !text.contains("open that conversation with /resume"),
        "the unconditional resume step is replaced by the qualified one"
    );
}

// The current conversation is already open, so the /resume note would not help
// there; the #403 warm-up line still names and counts the charge.
#[tokio::test]
async fn issue_912_own_warmup_cost_names_the_warmup_without_a_resume_note() {
    let pages = inspection_pages(Ok(InspectionDay::Ready(warmup_packet())));
    assert!(
        pages[0]
            .text
            .iter()
            .any(|line| line.contains("Reopen warm-up: 1 request, estimated at least $0.000180"))
    );
    assert!(!pages[0].text.join("\n").contains("no saved turns"));
}

#[tokio::test]
async fn issue_912_other_warmup_cost_qualifies_resume_advice() {
    let InspectionDay::Ready(mut view) = packet() else {
        unreachable!()
    };
    let mut q = warmup();
    q.attempt.thread_id = ThreadId::from_string(&Uuid::from_u128(912).to_string()).unwrap();
    q.attempt.request_id = Uuid::from_u128(9122);
    q.attempt.attempt_id = Uuid::from_u128(9121);
    assert!(!saved_turn_control(q.attempt.thread_id, /*append_turn*/ false).await);
    view.other_conversations = Some(OtherConversations {
        conversations: 1,
        requests: std::collections::BTreeMap::from([(q.attempt.request_id, vec![q])]),
        ..OtherConversations::default()
    });
    let pages = inspection_pages(Ok(InspectionDay::Ready(view)));
    assert!(pages[0].text.iter().any(|line| line.contains(
        "Other conversations on this day, not included above: 1 request in 1 conversation."
    )));
    assert!(
        pages[0]
            .text
            .iter()
            .any(|line| line.contains("Other conversations' estimated cost: at least $0.000180"))
    );
    assert_explained(&pages[0].text);
    // #1000: the note points at warm-ups the section itself counts.
    assert!(
        pages[0]
            .text
            .iter()
            .any(|line| line == "Of these, 1 request is a reopen warm-up."),
        "{:#?}",
        pages[0].text
    );
}

/// #1000: the warm-up count names requests, not attempts, and leaves other
/// requests of those conversations out.
#[tokio::test]
async fn issue_912_other_warmups_are_counted_by_request() {
    let InspectionDay::Ready(mut view) = packet() else {
        unreachable!()
    };
    let other = ThreadId::from_string(&Uuid::from_u128(912).to_string()).unwrap();
    let mut first = warmup();
    first.attempt.thread_id = other;
    first.attempt.request_id = Uuid::from_u128(9122);
    first.attempt.attempt_id = Uuid::from_u128(9121);
    let mut retry = first.clone();
    retry.attempt.attempt_id = Uuid::from_u128(9123);
    let mut second = first.clone();
    second.attempt.request_id = Uuid::from_u128(9124);
    second.attempt.attempt_id = Uuid::from_u128(9125);
    let mut normal = quote();
    normal.attempt.thread_id = other;
    normal.attempt.request_id = Uuid::from_u128(9126);
    normal.attempt.attempt_id = Uuid::from_u128(9127);
    view.other_conversations = Some(OtherConversations {
        conversations: 1,
        requests: std::collections::BTreeMap::from([
            (first.attempt.request_id, vec![first, retry]),
            (second.attempt.request_id, vec![second]),
            (normal.attempt.request_id, vec![normal]),
        ]),
        unavailable: 1,
        ..OtherConversations::default()
    });
    let pages = inspection_pages(Ok(InspectionDay::Ready(view)));
    let text = &pages[0].text;
    let position = |wanted: &str| text.iter().position(|line| line.starts_with(wanted));
    let count = position("Of these, 2 requests are reopen warm-ups.");
    // The count sits with the requests it counts, not after the line about
    // conversations that could not be read.
    assert!(
        count.is_some() && count < position("1 conversation could not be read in full"),
        "{text:#?}"
    );
    assert_explained(text);
}

#[tokio::test]
async fn issue_912_normal_saved_conversation_stays_resumable() {
    assert!(saved_turn_control(quote().attempt.thread_id, /*append_turn*/ true).await);
    let pages = inspection_pages(Ok(packet()));
    assert!(
        !pages[0]
            .text
            .iter()
            .any(|line| line.contains("no saved turns"))
    );
    assert!(
        pages[0].links.iter().any(
            |(label, _)| label.starts_with("Request 1 · ") && !label.contains("Reopen warm-up")
        )
    );
}

#[tokio::test]
async fn issue_912_mixed_saved_turn_and_warmup_does_not_assert_unresumable() {
    assert!(saved_turn_control(quote().attempt.thread_id, /*append_turn*/ true).await);
    let mut view = warmup_packet();
    let mut normal = quote();
    normal.attempt.attempt_id = Uuid::from_u128(9121);
    normal.attempt.request_id = Uuid::from_u128(9122);
    view.requests
        .insert(normal.attempt.request_id, vec![normal]);
    view.totals = DayTotals::from_quotes(view.requests.values().flatten()).unwrap();
    view.own_totals = view.totals.clone();
    let pages = inspection_pages(Ok(InspectionDay::Ready(view)));
    assert_eq!(
        pages[0]
            .links
            .iter()
            .filter(|(label, _)| label.contains("Reopen warm-up"))
            .count(),
        1
    );
    assert!(
        pages[0]
            .links
            .iter()
            .any(|(label, _)| label.starts_with("Request ") && !label.contains("Reopen warm-up"))
    );
    // The resume note is only for other conversations: this one is open.
    let text = pages[0].text.join("\n");
    assert!(!text.contains("no saved turns"), "{text}");
    assert!(!text.contains("Of these,"), "{text}");
}

#[cfg(feature = "developer-accounting")]
#[tokio::test]
async fn issue_912_warmup_resume_explanation_snapshots() {
    use crate::app_event::AppEvent;
    for width in [100, 40] {
        let mut view = {
            let InspectionDay::Ready(mut view) = packet() else {
                unreachable!()
            };
            let mut q = warmup();
            q.attempt.thread_id = ThreadId::from_string(&Uuid::from_u128(912).to_string()).unwrap();
            q.attempt.request_id = Uuid::from_u128(9122);
            q.attempt.attempt_id = Uuid::from_u128(9121);
            view.other_conversations = Some(OtherConversations {
                conversations: 1,
                requests: std::collections::BTreeMap::from([(q.attempt.request_id, vec![q])]),
                ..OtherConversations::default()
            });
            view
        };
        let (mut chat, mut rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
        chat.on_terminal_resize(width);
        chat.open_accounting_command(
            "requests 1970-01-01",
            NaiveDate::from_ymd_opt(1970, 1, 1).unwrap(),
        );
        let AppEvent::LoadAccountingInspector {
            generation,
            thread,
            day: requested,
            ..
        } = rx.try_recv().unwrap()
        else {
            panic!("expected inspector load");
        };
        if let Some(owner) = thread {
            view.owner = owner;
            for quote in view.requests.values_mut().flatten() {
                quote.attempt.thread_id = owner;
            }
        }
        chat.finish_accounting_inspector(
            generation,
            thread,
            requested,
            Ok(InspectionDay::Ready(view)),
        );
        insta::assert_snapshot!(
            format!("issue912_other_{width}"),
            render_bottom_popup_with_height(&chat, width, /*height*/ 60)
        );
        let mut explanation = String::new();
        let mut visible = false;
        for _ in 0..40 {
            explanation = render_bottom_popup_with_height(&chat, width, /*height*/ 60);
            let words = explanation.split_whitespace().collect::<Vec<_>>().join(" ");
            if words.contains("no saved turns") && words.contains("included in the recorded cost") {
                visible = true;
                break;
            }
            chat.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        assert!(
            visible,
            "explanation unreachable at {width}:\n{explanation}"
        );
        insta::assert_snapshot!(
            format!("issue912_other_{width}_explanation_visible"),
            explanation
        );
    }
}
