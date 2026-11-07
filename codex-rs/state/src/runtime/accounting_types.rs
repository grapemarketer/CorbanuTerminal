use anyhow::ensure;
use codex_protocol::ThreadId;
use serde::Deserialize;
use serde::Serialize;
use uuid::Uuid;

use super::pricing::Decimal;

/// Exact nonnegative SQLite integer; serde rejects booleans and floating point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "i64", into = "i64")]
pub struct Count(i64);

impl TryFrom<i64> for Count {
    type Error = anyhow::Error;
    fn try_from(value: i64) -> anyhow::Result<Self> {
        ensure!(value >= 0, "negative count or position");
        Ok(Self(value))
    }
}

impl From<Count> for i64 {
    fn from(value: Count) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Presence {
    #[default]
    Missing,
    Null,
    Number(Count),
}

impl<'de> Deserialize<'de> for Presence {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Option::<Count>::deserialize(deserializer)?.map_or(Self::Null, Self::Number))
    }
}

impl Presence {
    fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Patch {
    #[serde(skip_serializing_if = "Presence::is_missing")]
    pub input: Presence,
    #[serde(skip_serializing_if = "Presence::is_missing")]
    pub read: Presence,
    #[serde(skip_serializing_if = "Presence::is_missing")]
    pub write: Presence,
    #[serde(skip_serializing_if = "Presence::is_missing")]
    pub output: Presence,
    #[serde(skip_serializing_if = "Presence::is_missing")]
    pub reasoning: Presence,
    #[serde(skip_serializing_if = "Presence::is_missing")]
    pub total: Presence,
    /// The charge the provider stated with the response, in USD. Omitted when
    /// absent, so every patch recorded without one keeps its exact bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub billed_usd: Option<Decimal>,
    /// The HTTP status (401 or 403) with which the provider refused the
    /// attempt before any response (#475): it used no tokens and is not
    /// charged. Such a patch states nothing else. Omitted when absent, so
    /// every other patch keeps its exact bytes; one that has it needs ledger
    /// format 5.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused_status: Option<u16>,
    /// When the realtime session this attempt opened ended, in Unix ms
    /// (#824). The attempt's admission time is when the session started; the
    /// tokens its responses reported (#889 D223) are in earlier patches. Such
    /// a patch states nothing else. Omitted when absent, so every other patch keeps
    /// its exact bytes; one that has it needs ledger format 6.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_ended_at_ms: Option<Count>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dialect {
    Inclusive,
    NativeAnthropic,
    UnknownCompatible,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub attempt_id: Uuid,
    pub request_id: Uuid,
    pub thread_id: ThreadId,
    pub turn: String,
    pub retry_of: Option<Uuid>,
    pub provider: String,
    pub model: String,
    pub scope: Uuid,
    pub dialect: Dialect,
    pub dispatched_at_ms: Count,
    /// PF-84: the named provider account the attempt was sent on. Absent for
    /// the `default` account, so every attempt recorded before named accounts,
    /// and every default-account attempt since, keeps its exact bytes and the
    /// ledger's format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<AttemptAccount>,
}

/// A named provider account as the ledger records it: its name and, when the
/// vault could compute it, a 12-hex fingerprint salted per home. Never a
/// credential value.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptAccount {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

/// The account name every attempt without a recorded account was sent on.
pub const DEFAULT_ACCOUNT: &str = "default";

impl AttemptAccount {
    pub fn validate(&self) -> anyhow::Result<()> {
        // `default` is the absence of an account, never a second spelling of it.
        ensure!(
            !self.name.trim().is_empty()
                && self.name.len() <= 64
                && self.name != DEFAULT_ACCOUNT
                && !self
                    .name
                    .chars()
                    .any(|c| c.is_control() || c.is_whitespace()),
            "invalid account name"
        );
        if let Some(fingerprint) = &self.fingerprint {
            ensure!(
                fingerprint.len() == 12
                    && fingerprint
                        .chars()
                        .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f')),
                "invalid account fingerprint"
            );
        }
        Ok(())
    }
}

impl Attempt {
    pub fn validate(&self) -> anyhow::Result<()> {
        for text in [&self.turn, &self.provider, &self.model] {
            ensure!(
                !text.trim().is_empty() && text.len() <= 128 && !text.chars().any(char::is_control),
                "invalid bounded identity metadata"
            );
        }
        ensure!(self.retry_of != Some(self.attempt_id), "self retry");
        if let Some(account) = &self.account {
            account.validate()?;
        }
        Ok(())
    }

    /// The account name this attempt was sent on, `default` when none is recorded.
    pub fn account_name(&self) -> &str {
        self.account
            .as_ref()
            .map_or(DEFAULT_ACCOUNT, |account| account.name.as_str())
    }

    /// The ledger format this attempt needs: 4 when it names an account,
    /// otherwise 1. `append_on_connection` upgrades the ledger to it first.
    pub(crate) fn ledger_format(&self) -> i64 {
        if self.account.is_some() { 4 } else { 1 }
    }

    pub fn same_owner(&self, other: &Self) -> bool {
        self.request_id == other.request_id
            && self.thread_id == other.thread_id
            && self.turn == other.turn
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub revision: Count,
    pub source: Uuid,
    pub sequence: Count,
    pub patch: Patch,
}

impl Observation {
    /// The ledger format this observation needs: 6 when it records a realtime
    /// session's end, 5 when it records a refused attempt, otherwise 1.
    /// `append_on_connection` upgrades the ledger to it first.
    pub(crate) fn ledger_format(&self) -> i64 {
        if self.patch.session_ended_at_ms.is_some() {
            6
        } else if self.patch.refused_status.is_some() {
            5
        } else {
            1
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub input: Option<i64>,
    pub noncached: Option<i64>,
    pub read: Option<i64>,
    pub write: Option<i64>,
    pub output: Option<i64>,
    pub reasoning: Option<i64>,
    pub total: Option<i64>,
    /// The latest charge the provider stated for the attempt. Never derived
    /// from tokens; omitted when absent so earlier estimates keep their bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub billed_usd: Option<Decimal>,
    /// The status the provider refused the attempt with (#475); every count
    /// is then zero. Omitted when absent so earlier estimates keep their bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused_status: Option<u16>,
    /// When the realtime session the attempt opened ended (#824). Omitted
    /// when absent so earlier estimates keep their bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_ended_at_ms: Option<i64>,
}

/// Replay cumulative patches in source revision order, validating every prefix.
pub(super) fn replay(dialect: Dialect, observations: &[Observation]) -> anyhow::Result<Usage> {
    let mut raw = [None; 6];
    let mut billed_usd = None;
    let mut usage = Usage::default();
    // #824: a realtime session's end is one observation that states the time
    // and nothing else. It changes no count, so the counts below replay from
    // the other observations alone.
    let mut session_ended_at_ms = None;
    for observation in observations {
        let Some(ended) = observation.patch.session_ended_at_ms else {
            continue;
        };
        ensure!(
            session_ended_at_ms.is_none(),
            "a realtime session ends once"
        );
        ensure!(
            observation.patch
                == Patch {
                    session_ended_at_ms: Some(ended),
                    ..Patch::default()
                },
            "a session end states no usage"
        );
        session_ended_at_ms = Some(i64::from(ended));
    }
    usage.session_ended_at_ms = session_ended_at_ms;
    // #475: a refused attempt got no response, so its one patch states the
    // refusal and nothing else, and every count is zero.
    if let Some(refused) = observations
        .iter()
        .find_map(|observation| observation.patch.refused_status)
    {
        let [observation] = observations else {
            anyhow::bail!("a refused attempt has exactly one observation");
        };
        ensure!(
            i64::from(observation.revision) > 0,
            "replay requires unique ordered revisions"
        );
        ensure!(
            observation.patch
                == Patch {
                    refused_status: Some(refused),
                    ..Patch::default()
                },
            "a refusal states no usage"
        );
        ensure!(matches!(refused, 401 | 403), "invalid refusal status");
        return Ok(Usage {
            input: Some(0),
            noncached: Some(0),
            read: Some(0),
            write: Some(0),
            output: Some(0),
            reasoning: Some(0),
            total: Some(0),
            billed_usd: None,
            refused_status: Some(refused),
            session_ended_at_ms: None,
        });
    }
    let mut previous = 0;
    for observation in observations {
        let revision = i64::from(observation.revision);
        ensure!(
            revision > previous,
            "replay requires unique ordered revisions"
        );
        previous = revision;
        let p = &observation.patch;
        billed_usd = p.billed_usd.or(billed_usd);
        for (slot, value) in
            raw.iter_mut()
                .zip([p.input, p.read, p.write, p.output, p.reasoning, p.total])
        {
            if let Presence::Number(count) = value {
                *slot = Some(i64::from(count));
            }
        }
        let [input, read, write, output, reasoning, total] = raw;
        let known_cache = read.unwrap_or(0).checked_add(write.unwrap_or(0));
        let known_cache = known_cache.ok_or_else(|| anyhow::anyhow!("cache overflow"))?;
        let (input, noncached) = match dialect {
            Dialect::Inclusive => {
                ensure!(
                    input.is_none_or(|n| known_cache <= n),
                    "cache exceeds inclusive input"
                );
                (
                    input,
                    input.zip(read).zip(write).map(|((i, r), w)| i - r - w),
                )
            }
            Dialect::NativeAnthropic => {
                let sum = input.unwrap_or(0).checked_add(known_cache);
                ensure!(sum.is_some(), "input overflow");
                (input.zip(read).zip(write).and(sum), input)
            }
            Dialect::UnknownCompatible => (None, None),
        };
        // Unknown dialects retain raw evidence without asserting subset/total semantics.
        let mut measured_total = total;
        let mut measured_reasoning = reasoning;
        if dialect != Dialect::UnknownCompatible {
            ensure!(
                reasoning.zip(output).is_none_or(|(r, o)| r <= o),
                "reasoning exceeds output"
            );
            if let Some((i, o)) = input.zip(output) {
                let sum = i
                    .checked_add(o)
                    .ok_or_else(|| anyhow::anyhow!("total overflow"))?;
                ensure!(total.is_none_or(|t| t == sum), "conflicting total");
                if dialect == Dialect::NativeAnthropic {
                    measured_total = Some(sum);
                }
            }
        } else {
            measured_total = None;
            measured_reasoning = None;
        }
        usage = Usage {
            input,
            noncached,
            read,
            write,
            output,
            reasoning: measured_reasoning,
            total: measured_total,
            billed_usd,
            refused_status: None,
            session_ended_at_ms,
        };
    }
    Ok(usage)
}
