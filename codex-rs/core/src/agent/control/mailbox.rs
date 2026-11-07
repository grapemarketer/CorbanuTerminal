use super::*;
use codex_protocol::protocol::AgentMessageKind;
use codex_state::AgentMailboxAdmission;
use codex_state::AgentMailboxPhase;

/// #1545: a delivery attempt id starts with the mailbox incarnation of the recipient session it
/// was reserved for. Recovery can then tell a `Submitted` row handed to the recipient's current
/// session (which moves the row on when it applies it) from one handed to a session that has
/// since been replaced (an in-process reload or root resume, or an earlier process), whose
/// in-memory queue was dropped.
fn new_attempt_id(recipient_incarnation: &str) -> String {
    format!("{recipient_incarnation}/{}", uuid::Uuid::now_v7())
}

fn reserved_for_incarnation(attempt_id: Option<&str>, recipient_incarnation: &str) -> bool {
    attempt_id
        .and_then(|attempt_id| attempt_id.split_once('/'))
        .is_some_and(|(incarnation, _)| incarnation == recipient_incarnation)
}

impl AgentControl {
    pub(crate) async fn recover_inter_agent_communications(
        &self,
        agent_id: ThreadId,
    ) -> CodexResult<()> {
        let state = self.upgrade()?;
        let thread = state.get_thread(agent_id).await?;
        let Some(state_db) = thread.state_db() else {
            return Ok(());
        };
        let messages = state_db
            .list_recoverable_agent_messages(agent_id)
            .await
            .map_err(|err| {
                CodexErr::Fatal(format!(
                    "failed to read recoverable mailbox for agent {agent_id}: {err}"
                ))
            })?;
        for message in messages {
            let message_id = message.communication.message_id.clone().ok_or_else(|| {
                CodexErr::Fatal(format!(
                    "recoverable mailbox row for agent {agent_id} has no message_id"
                ))
            })?;
            match message.phase {
                AgentMailboxPhase::Admitted => {
                    let _ = state_db
                        .transition_agent_message(
                            &message_id,
                            AgentMailboxPhase::Admitted,
                            AgentMailboxPhase::Ready,
                            crate::turn_timing::now_unix_timestamp_ms(),
                        )
                        .await
                        .map_err(|err| {
                            CodexErr::Fatal(format!(
                                "failed to recover admitted agent message {message_id}: {err}"
                            ))
                        })?;
                }
                // #1058's order writes Submitted before the hand-off, so a Submitting row is an
                // attempt that was reserved but never handed over, and is requeued. An unapplied
                // Submitted row is requeued too, unless its attempt names the recipient's current
                // session (#1545, below). Either is quarantined instead when the recipient's
                // rollout already applied it (a row written by an older build that marked
                // Submitted only after the hand-off).
                AgentMailboxPhase::Submitting | AgentMailboxPhase::Submitted => {
                    let phase = if message.phase == AgentMailboxPhase::Submitting {
                        "submitting"
                    } else {
                        "submitted"
                    };
                    if thread
                        .session
                        .has_applied_agent_message_id(&message_id)
                        .await
                    {
                        let _ = state_db
                            .transition_agent_message(
                                &message_id,
                                message.phase,
                                AgentMailboxPhase::UnknownOutcome,
                                crate::turn_timing::now_unix_timestamp_ms(),
                            )
                            .await
                            .map_err(|err| {
                                CodexErr::Fatal(format!(
                                    "failed to quarantine applied {phase} agent message \
                                     {message_id}: {err}"
                                ))
                            })?;
                        warn!(
                            %message_id,
                            %agent_id,
                            "agent mailbox message was applied before process recovery but its \
                             provider outcome is unknown; automatic replay is disabled"
                        );
                        continue;
                    }
                    // #1545: a Submitted row whose attempt names the recipient's current session
                    // was handed to that session by a live sender (it writes Submitted before the
                    // hand-off), and the session moves it on when it applies it. Requeueing it
                    // would deliver it twice under the same message id. A row handed to a session
                    // that has since been replaced is requeued below, as is a Submitting row,
                    // which was never handed over.
                    if message.phase == AgentMailboxPhase::Submitted
                        && reserved_for_incarnation(
                            message.attempt_id.as_deref(),
                            &thread.session.mailbox_incarnation,
                        )
                    {
                        tracing::debug!(
                            %message_id,
                            %agent_id,
                            "agent mailbox message was handed to the recipient's current session; \
                             recovery leaves it to that session"
                        );
                        continue;
                    }
                    let _ = state_db
                        .transition_agent_message(
                            &message_id,
                            message.phase,
                            AgentMailboxPhase::Ready,
                            crate::turn_timing::now_unix_timestamp_ms(),
                        )
                        .await
                        .map_err(|err| {
                            CodexErr::Fatal(format!(
                                "failed to requeue {phase} agent message {message_id}: {err}"
                            ))
                        })?;
                }
                AgentMailboxPhase::ProviderRunning => {
                    let _ = state_db
                        .transition_agent_message(
                            &message_id,
                            AgentMailboxPhase::ProviderRunning,
                            AgentMailboxPhase::UnknownOutcome,
                            crate::turn_timing::now_unix_timestamp_ms(),
                        )
                        .await
                        .map_err(|err| {
                            CodexErr::Fatal(format!(
                                "failed to quarantine ambiguous agent message {message_id}: {err}"
                            ))
                        })?;
                    warn!(
                        %message_id,
                        %agent_id,
                        "agent mailbox outcome became ambiguous across process recovery; automatic replay is disabled"
                    );
                    continue;
                }
                AgentMailboxPhase::UnknownOutcome => continue,
                AgentMailboxPhase::Ready | AgentMailboxPhase::RetryableFailure => {}
                AgentMailboxPhase::Completed
                | AgentMailboxPhase::Applied
                | AgentMailboxPhase::Cancelled
                | AgentMailboxPhase::TerminalFailure => continue,
            }
            self.send_persisted_inter_agent_communication(
                agent_id,
                message.communication,
                /*parent_turn_id*/ None,
            )
            .await?;
        }
        Ok(())
    }

    pub(crate) async fn admit_inter_agent_communication(
        &self,
        agent_id: ThreadId,
        communication: &InterAgentCommunication,
    ) -> CodexResult<()> {
        communication
            .validate_mailbox_body()
            .map_err(CodexErr::InvalidRequest)?;
        let message_id = communication.message_id.as_deref().ok_or_else(|| {
            CodexErr::InvalidRequest("agent mailbox message has no message_id".to_string())
        })?;
        let state = self.upgrade()?;
        let thread = state.get_thread(agent_id).await?;
        let Some(state_db) = thread.state_db() else {
            return Ok(());
        };
        let admission = state_db
            .admit_agent_message(
                agent_id,
                communication,
                crate::turn_timing::now_unix_timestamp_ms(),
            )
            .await
            .map_err(|err| {
                CodexErr::Fatal(format!("failed to admit agent message {message_id}: {err}"))
            })?;
        if matches!(
            admission,
            AgentMailboxAdmission::Inserted
                | AgentMailboxAdmission::Existing(AgentMailboxPhase::Admitted)
        ) {
            let _ = state_db
                .transition_agent_message(
                    message_id,
                    AgentMailboxPhase::Admitted,
                    AgentMailboxPhase::Ready,
                    crate::turn_timing::now_unix_timestamp_ms(),
                )
                .await
                .map_err(|err| {
                    CodexErr::Fatal(format!("failed to ready agent message {message_id}: {err}"))
                })?;
        }
        Ok(())
    }

    pub(crate) async fn send_persisted_inter_agent_communication(
        &self,
        agent_id: ThreadId,
        mut communication: InterAgentCommunication,
        parent_turn_id: Option<String>,
    ) -> CodexResult<String> {
        let message_id = communication.ensure_message_identity().to_string();
        communication
            .validate_mailbox_body()
            .map_err(CodexErr::InvalidRequest)?;
        if communication.kind.is_none() {
            communication.kind = Some(if communication.trigger_turn {
                AgentMessageKind::FollowUp
            } else {
                AgentMessageKind::Informational
            });
        }
        let last_task_message = last_task_message_from_communication(&communication);
        let state = self.upgrade()?;
        let thread = state.get_thread(agent_id).await.ok();
        let state_db = thread.as_ref().and_then(|thread| thread.state_db());
        if let Some((thread, state_db)) = thread.as_ref().zip(state_db.as_ref()) {
            let admission = state_db
                .admit_agent_message(
                    agent_id,
                    &communication,
                    crate::turn_timing::now_unix_timestamp_ms(),
                )
                .await
                .map_err(|err| {
                    CodexErr::Fatal(format!("failed to admit agent message {message_id}: {err}"))
                })?;
            let ready_to_submit = match admission {
                AgentMailboxAdmission::Inserted => state_db
                    .transition_agent_message(
                        &message_id,
                        AgentMailboxPhase::Admitted,
                        AgentMailboxPhase::Ready,
                        crate::turn_timing::now_unix_timestamp_ms(),
                    )
                    .await
                    .map_err(|err| {
                        CodexErr::Fatal(format!(
                            "failed to ready agent message {message_id}: {err}"
                        ))
                    })?,
                AgentMailboxAdmission::Existing(AgentMailboxPhase::Admitted) => {
                    let _ = state_db
                        .transition_agent_message(
                            &message_id,
                            AgentMailboxPhase::Admitted,
                            AgentMailboxPhase::Ready,
                            crate::turn_timing::now_unix_timestamp_ms(),
                        )
                        .await
                        .map_err(|err| {
                            CodexErr::Fatal(format!(
                                "failed to ready agent message {message_id}: {err}"
                            ))
                        })?;
                    true
                }
                AgentMailboxAdmission::Existing(AgentMailboxPhase::Ready)
                | AgentMailboxAdmission::Existing(AgentMailboxPhase::RetryableFailure) => true,
                AgentMailboxAdmission::Existing(
                    AgentMailboxPhase::Submitting
                    | AgentMailboxPhase::Submitted
                    | AgentMailboxPhase::ProviderRunning
                    | AgentMailboxPhase::UnknownOutcome
                    | AgentMailboxPhase::Completed
                    | AgentMailboxPhase::Applied
                    | AgentMailboxPhase::Cancelled
                    | AgentMailboxPhase::TerminalFailure,
                ) => false,
            };
            if !ready_to_submit {
                return Ok(message_id);
            }
            let expected = match admission {
                AgentMailboxAdmission::Existing(AgentMailboxPhase::RetryableFailure) => {
                    AgentMailboxPhase::RetryableFailure
                }
                AgentMailboxAdmission::Inserted
                | AgentMailboxAdmission::Existing(AgentMailboxPhase::Admitted)
                | AgentMailboxAdmission::Existing(AgentMailboxPhase::Ready) => {
                    AgentMailboxPhase::Ready
                }
                AgentMailboxAdmission::Existing(
                    AgentMailboxPhase::Submitting
                    | AgentMailboxPhase::Submitted
                    | AgentMailboxPhase::ProviderRunning
                    | AgentMailboxPhase::UnknownOutcome
                    | AgentMailboxPhase::Completed
                    | AgentMailboxPhase::Applied
                    | AgentMailboxPhase::Cancelled
                    | AgentMailboxPhase::TerminalFailure,
                ) => return Ok(message_id),
            };
            let attempt_id = new_attempt_id(&thread.session.mailbox_incarnation);
            if !state_db
                .begin_agent_message_submission(
                    &message_id,
                    expected,
                    &attempt_id,
                    crate::turn_timing::now_unix_timestamp_ms(),
                )
                .await
                .map_err(|err| {
                    CodexErr::Fatal(format!(
                        "failed to reserve agent message {message_id} submission: {err}"
                    ))
                })?
            {
                return Ok(message_id);
            }
            // #1058: mark the message Submitted before handing it over. A turn already running
            // on the recipient can apply it as soon as it is handed over, and moves it from
            // Submitted to ProviderRunning; marked after the hand-off, it stayed Submitted while
            // that turn ran it.
            if !state_db
                .confirm_agent_message_submission(
                    &message_id,
                    &attempt_id,
                    crate::turn_timing::now_unix_timestamp_ms(),
                )
                .await
                .map_err(|err| {
                    CodexErr::Fatal(format!(
                        "failed to confirm agent message {message_id} submission: {err}"
                    ))
                })?
            {
                // #1452: another owner (mailbox recovery, or a live sender racing it) moved the
                // row off Submitting, or reserved it again under its own attempt (#1545), between
                // this reservation and this write. Only the owner whose Submitting -> Submitted
                // write wins hands the message over, so this attempt stops here without an error,
                // as a lost reservation above does; failing it would stop recovery's loop or
                // invite a resend under a new message id.
                tracing::debug!(
                    %message_id,
                    %agent_id,
                    "agent mailbox message left its submitting state to another owner; \
                     that owner delivers it"
                );
                return Ok(message_id);
            }
        }

        let op = Op::InterAgentCommunication {
            communication: communication.clone(),
        };
        let result = self
            .handle_thread_request_result(
                agent_id,
                &state,
                state
                    .send_op(
                        agent_id,
                        op,
                        parent_turn_id.filter(|_| communication.trigger_turn),
                    )
                    .await,
            )
            .await;
        match &result {
            Ok(_) => match last_task_message {
                Some(last_task_message) => self
                    .state
                    .update_last_task_message(agent_id, last_task_message),
                None => self.state.clear_last_task_message(agent_id),
            },
            Err(_) => {
                if let Some(state_db) = state_db.as_ref() {
                    let _ = state_db
                        .transition_agent_message(
                            &message_id,
                            AgentMailboxPhase::Submitted,
                            AgentMailboxPhase::Ready,
                            crate::turn_timing::now_unix_timestamp_ms(),
                        )
                        .await;
                }
            }
        }
        result
    }
}
