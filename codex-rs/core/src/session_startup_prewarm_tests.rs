use super::is_reopen;
use codex_protocol::ThreadId;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::InternalSessionSource;
use codex_protocol::protocol::ResumedHistory;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use pretty_assertions::assert_eq;
use std::sync::Arc;

/// #403 (review M1): only a conversation the user resumes or forks is a
/// reopen. A sub-agent spawned with copied history, a guardian review forked
/// from its snapshot, or an internal worker is not, whatever its history.
#[test]
fn only_the_users_resume_or_fork_is_a_reopen() {
    let resumed = || {
        InitialHistory::Resumed(ResumedHistory {
            conversation_id: ThreadId::new(),
            history: Arc::new(Vec::new()),
            rollout_path: None,
        })
    };
    let forked = || InitialHistory::Forked(Vec::new());
    let spawned = SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id: ThreadId::new(),
        depth: 1,
        agent_path: None,
        agent_nickname: None,
        agent_role: None,
        agent_class: None,
    });
    let guardian = SessionSource::SubAgent(SubAgentSource::Other("guardian".to_string()));
    let review = SessionSource::SubAgent(SubAgentSource::Review);
    let internal = SessionSource::Internal(InternalSessionSource::MemoryConsolidation);
    let cases = [
        // The user's own client: resume, restart, `/side`, editing a message.
        (resumed(), SessionSource::Cli, true),
        (forked(), SessionSource::Cli, true),
        (resumed(), SessionSource::VSCode, true),
        (forked(), SessionSource::Exec, true),
        // Never a reopen: a new or cleared conversation.
        (InitialHistory::New, SessionSource::Cli, false),
        (InitialHistory::Cleared, SessionSource::Cli, false),
        // Workers given copied history.
        (forked(), spawned.clone(), false),
        (resumed(), spawned, false),
        (forked(), guardian, false),
        (forked(), review, false),
        (forked(), internal, false),
    ];
    for (history, source, expected) in cases {
        assert_eq!(
            is_reopen(&history, &source, /*reopened_by_prompt_edit*/ false),
            expected,
            "{history:?} from {source:?}"
        );
    }
}

/// #1038: editing the first message continues in a conversation with no
/// history. The client marks it, and it is a reopen like any other edit; the
/// mark makes nothing else a reopen.
#[test]
fn a_first_message_edit_is_a_reopen() {
    let spawned = SessionSource::SubAgent(SubAgentSource::Review);
    let cases = [
        (InitialHistory::New, SessionSource::Cli, true, true),
        (InitialHistory::New, SessionSource::Cli, false, false),
        (InitialHistory::Cleared, SessionSource::Cli, true, false),
        (InitialHistory::New, spawned, true, false),
        (
            InitialHistory::Forked(Vec::new()),
            SessionSource::Cli,
            true,
            true,
        ),
    ];
    for (history, source, edited, expected) in cases {
        assert_eq!(
            is_reopen(&history, &source, edited),
            expected,
            "{history:?} from {source:?}, edited={edited}"
        );
    }
}
