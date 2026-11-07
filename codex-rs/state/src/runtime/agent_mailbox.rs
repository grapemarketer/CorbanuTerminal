use super::*;
use codex_protocol::protocol::InterAgentCommunication;
use std::str::FromStr;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentMailboxPhase {
    Admitted,
    Ready,
    Submitting,
    Submitted,
    ProviderRunning,
    RetryableFailure,
    UnknownOutcome,
    Completed,
    Applied,
    Cancelled,
    TerminalFailure,
}

impl AgentMailboxPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Admitted => "admitted",
            Self::Ready => "ready",
            Self::Submitting => "submitting",
            Self::Submitted => "submitted",
            Self::ProviderRunning => "provider_running",
            Self::RetryableFailure => "retryable_failure",
            Self::UnknownOutcome => "unknown_outcome",
            Self::Completed => "completed",
            Self::Applied => "applied",
            Self::Cancelled => "cancelled",
            Self::TerminalFailure => "terminal_failure",
        }
    }
}

impl FromStr for AgentMailboxPhase {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "admitted" => Ok(Self::Admitted),
            "ready" => Ok(Self::Ready),
            "submitting" => Ok(Self::Submitting),
            "submitted" => Ok(Self::Submitted),
            "provider_running" => Ok(Self::ProviderRunning),
            "retryable_failure" => Ok(Self::RetryableFailure),
            "unknown_outcome" => Ok(Self::UnknownOutcome),
            "completed" => Ok(Self::Completed),
            "applied" => Ok(Self::Applied),
            "cancelled" => Ok(Self::Cancelled),
            "terminal_failure" => Ok(Self::TerminalFailure),
            other => anyhow::bail!("unknown agent mailbox phase `{other}`"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentMailboxAdmission {
    Inserted,
    Existing(AgentMailboxPhase),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentMailboxMessage {
    pub recipient_thread_id: ThreadId,
    pub communication: InterAgentCommunication,
    pub phase: AgentMailboxPhase,
    pub attempt_id: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl StateRuntime {
    pub async fn admit_agent_message(
        &self,
        recipient_thread_id: ThreadId,
        communication: &InterAgentCommunication,
        now_ms: i64,
    ) -> anyhow::Result<AgentMailboxAdmission> {
        let message_id = communication
            .message_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("agent mailbox message has no message_id"))?;
        let created_at_ms = communication.created_at_ms.unwrap_or(now_ms);
        let communication_json = serde_json::to_string(communication)?;
        let mut tx = self.pool.begin().await?;
        let inserted = sqlx::query(
            r#"
INSERT OR IGNORE INTO agent_mailbox_messages (
    message_id,
    recipient_thread_id,
    communication_json,
    phase,
    created_at_ms,
    updated_at_ms
) VALUES (?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(message_id)
        .bind(recipient_thread_id.to_string())
        .bind(communication_json.as_str())
        .bind(AgentMailboxPhase::Admitted.as_str())
        .bind(created_at_ms)
        .bind(now_ms)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        let row = sqlx::query(
            r#"
SELECT recipient_thread_id, communication_json, phase
FROM agent_mailbox_messages
WHERE message_id = ?
            "#,
        )
        .bind(message_id)
        .fetch_one(&mut *tx)
        .await?;
        let stored_recipient: String = row.try_get("recipient_thread_id")?;
        let stored_communication: String = row.try_get("communication_json")?;
        let stored_communication =
            serde_json::from_str::<InterAgentCommunication>(&stored_communication)?;
        if stored_recipient != recipient_thread_id.to_string()
            || !same_logical_message(&stored_communication, communication)
        {
            anyhow::bail!(
                "agent mailbox message_id `{message_id}` conflicts with a different message"
            );
        }
        let phase = AgentMailboxPhase::from_str(row.try_get("phase")?)?;
        tx.commit().await?;
        Ok(if inserted {
            AgentMailboxAdmission::Inserted
        } else {
            AgentMailboxAdmission::Existing(phase)
        })
    }

    pub async fn transition_agent_message(
        &self,
        message_id: &str,
        expected: AgentMailboxPhase,
        next: AgentMailboxPhase,
        now_ms: i64,
    ) -> anyhow::Result<bool> {
        let rows_affected = sqlx::query(
            r#"
UPDATE agent_mailbox_messages
SET phase = ?, updated_at_ms = ?
WHERE message_id = ? AND phase = ?
            "#,
        )
        .bind(next.as_str())
        .bind(now_ms)
        .bind(message_id)
        .bind(expected.as_str())
        .execute(self.pool.as_ref())
        .await?
        .rows_affected();
        Ok(rows_affected == 1)
    }

    /// Reserve one concrete delivery attempt before handing the message to the recipient.
    ///
    /// `message_id` remains stable across retries. `attempt_id` changes for each attempt so an
    /// ambiguous crash boundary can be surfaced without manufacturing a new logical assignment.
    pub async fn begin_agent_message_submission(
        &self,
        message_id: &str,
        expected: AgentMailboxPhase,
        attempt_id: &str,
        now_ms: i64,
    ) -> anyhow::Result<bool> {
        let rows_affected = sqlx::query(
            r#"
UPDATE agent_mailbox_messages
SET phase = ?, attempt_id = ?, updated_at_ms = ?
WHERE message_id = ? AND phase = ?
            "#,
        )
        .bind(AgentMailboxPhase::Submitting.as_str())
        .bind(attempt_id)
        .bind(now_ms)
        .bind(message_id)
        .bind(expected.as_str())
        .execute(self.pool.as_ref())
        .await?
        .rows_affected();
        Ok(rows_affected == 1)
    }

    pub async fn list_recoverable_agent_messages(
        &self,
        recipient_thread_id: ThreadId,
    ) -> anyhow::Result<Vec<AgentMailboxMessage>> {
        let rows = sqlx::query(
            r#"
SELECT communication_json, phase, attempt_id, created_at_ms, updated_at_ms
FROM agent_mailbox_messages
WHERE recipient_thread_id = ?
  AND phase NOT IN ('completed', 'applied', 'cancelled', 'terminal_failure')
ORDER BY created_at_ms ASC, message_id ASC
            "#,
        )
        .bind(recipient_thread_id.to_string())
        .fetch_all(self.pool.as_ref())
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(AgentMailboxMessage {
                    recipient_thread_id,
                    communication: serde_json::from_str(
                        row.try_get::<String, _>("communication_json")?.as_str(),
                    )?,
                    phase: AgentMailboxPhase::from_str(row.try_get("phase")?)?,
                    attempt_id: row.try_get("attempt_id")?,
                    created_at_ms: row.try_get("created_at_ms")?,
                    updated_at_ms: row.try_get("updated_at_ms")?,
                })
            })
            .collect()
    }

    pub async fn get_agent_message(
        &self,
        message_id: &str,
    ) -> anyhow::Result<Option<AgentMailboxMessage>> {
        let row = sqlx::query(
            r#"
SELECT recipient_thread_id, communication_json, phase, attempt_id, created_at_ms, updated_at_ms
FROM agent_mailbox_messages
WHERE message_id = ?
            "#,
        )
        .bind(message_id)
        .fetch_optional(self.pool.as_ref())
        .await?;
        row.map(|row| {
            let recipient_thread_id =
                ThreadId::from_string(row.try_get::<String, _>("recipient_thread_id")?.as_str())?;
            Ok(AgentMailboxMessage {
                recipient_thread_id,
                communication: serde_json::from_str(
                    row.try_get::<String, _>("communication_json")?.as_str(),
                )?,
                phase: AgentMailboxPhase::from_str(row.try_get("phase")?)?,
                attempt_id: row.try_get("attempt_id")?,
                created_at_ms: row.try_get("created_at_ms")?,
                updated_at_ms: row.try_get("updated_at_ms")?,
            })
        })
        .transpose()
    }

    pub async fn mark_agent_message_completed(
        &self,
        message_id: &str,
        now_ms: i64,
    ) -> anyhow::Result<bool> {
        let rows_affected = sqlx::query(
            r#"
UPDATE agent_mailbox_messages
SET phase = ?, updated_at_ms = ?
WHERE message_id = ?
  AND phase NOT IN (
      'completed',
      'applied',
      'cancelled',
      'terminal_failure',
      'unknown_outcome'
  )
            "#,
        )
        .bind(AgentMailboxPhase::Completed.as_str())
        .bind(now_ms)
        .bind(message_id)
        .execute(self.pool.as_ref())
        .await?
        .rows_affected();
        Ok(rows_affected == 1)
    }
}

/// A server-generated observation timestamp is not part of a logical message's identity.
///
/// Clients retry with the stable message ID and semantic payload; they are not required to
/// persist a timestamp that the server generated on their behalf. Every field that can alter
/// routing, model-visible content, delivery behavior, or attribution remains collision-checked.
///
/// A stored row without an `author_label` matches a labelled retry (#1335): a row persisted
/// before Core labelled cross-tree mail has none, and the same message retried afterwards has
/// one. The relaxation is one-directional: a labelled stored row still conflicts with an
/// unlabelled retry, because the label is the only field naming the sending thread and the
/// incoming copy is the one delivered. Two different known labels still conflict, and `author`
/// is always compared. A stored row without a label cannot be told apart from same-tree mail,
/// so a same-tree row and a labelled cross-tree retry with otherwise identical fields are
/// treated as one message; the risk is low because only app-server clients choose message ids
/// and the model-facing tool always makes a fresh UUID. This function is not an attribution
/// guarantee.
fn same_logical_message(
    stored: &InterAgentCommunication,
    incoming: &InterAgentCommunication,
) -> bool {
    let mut stored = stored.clone();
    let mut incoming = incoming.clone();
    stored.created_at_ms = None;
    incoming.created_at_ms = None;
    if stored.author_label.is_none() {
        incoming.author_label = None;
    }
    stored == incoming
}

#[cfg(test)]
#[path = "agent_mailbox_tests.rs"]
mod tests;
