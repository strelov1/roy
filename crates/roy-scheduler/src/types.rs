//! Domain types for `roy-scheduler`. Mirror the SQLite schema in
//! migrations/sqlite/0001_initial.sql; field names use snake_case to
//! match the DB columns directly via sqlx `FromRow`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub harness: String,
    /// `Some(id)` fires inside that roy-side project; `None` fires orphan
    /// (daemon allocates a per-session workspace dir).
    pub project_id: Option<String>,
    pub task: String,
    pub model: Option<String>,
    /// SQLite INTEGER 0/1. Use the bool getter `is_persistent()` for clarity.
    pub persistent: i64,
    pub persistent_session_id: Option<String>,
    /// Optional roy session id to notify. When set, the fired prompt is
    /// augmented with a `roy inject <id> ...` instruction so the agent can
    /// self-report into that session.
    pub notify_session: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Agent {
    pub fn is_persistent(&self) -> bool {
        self.persistent != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    Cron,
    Oneshot,
}

impl TriggerKind {
    pub fn as_db(self) -> &'static str {
        match self {
            TriggerKind::Cron => "cron",
            TriggerKind::Oneshot => "oneshot",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "cron" => Some(Self::Cron),
            "oneshot" => Some(Self::Oneshot),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, FromRow, Serialize, Deserialize)]
pub struct Trigger {
    pub id: String,
    pub agent_id: String,
    pub kind: String, // 'cron' | 'oneshot'
    pub cron_expr: Option<String>,
    pub timezone: String,
    pub fire_at: Option<DateTime<Utc>>,
    pub next_fire_at: DateTime<Utc>,
    pub last_fire_at: Option<DateTime<Utc>>,
    pub paused: i64,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl Trigger {
    pub fn is_paused(&self) -> bool {
        self.paused != 0
    }

    pub fn kind(&self) -> TriggerKind {
        TriggerKind::parse(&self.kind)
            .unwrap_or_else(|| panic!("invalid kind in DB: {:?}", self.kind))
    }

    pub fn is_oneshot(&self) -> bool {
        matches!(self.kind(), TriggerKind::Oneshot)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FireStatus {
    Running,
    Ok,
    Error,
    Timeout,
}

impl FireStatus {
    pub fn as_db(self) -> &'static str {
        match self {
            FireStatus::Running => "running",
            FireStatus::Ok => "ok",
            FireStatus::Error => "error",
            FireStatus::Timeout => "timeout",
        }
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Fire {
    pub id: String,
    pub agent_id: String,
    pub trigger_id: Option<String>,
    pub session_id: Option<String>,
    pub status: String, // see FireStatus
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub transcript_seq_range_start: Option<i64>,
    pub transcript_seq_range_end: Option<i64>,
    pub assistant_text: Option<String>,
    pub cost_usd: Option<f64>,
    pub stop_reason: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriberKind {
    Webhook,
    NotifyNative,
}

impl SubscriberKind {
    pub fn as_db(self) -> &'static str {
        match self {
            SubscriberKind::Webhook => "webhook",
            SubscriberKind::NotifyNative => "notify_native",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "webhook" => Some(Self::Webhook),
            "notify_native" => Some(Self::NotifyNative),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct Subscriber {
    pub id: String,
    pub agent_id: Option<String>,
    pub trigger_id: Option<String>,
    pub kind: String,
    /// Raw JSON string from DB. Parsers per kind live in src/subscribers/*.rs.
    pub config: String,
    pub enabled: i64,
    pub order_index: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
pub struct SubscriberRun {
    pub id: String,
    pub fire_id: String,
    pub subscriber_id: String,
    pub status: String, // 'ok' | 'error' | 'skipped'
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error_message: Option<String>,
    pub response_snippet: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_kind_roundtrips() {
        for kind in [TriggerKind::Cron, TriggerKind::Oneshot] {
            assert_eq!(TriggerKind::parse(kind.as_db()), Some(kind));
        }
        assert_eq!(TriggerKind::parse("nope"), None);
    }

    #[test]
    fn subscriber_kind_roundtrips() {
        for kind in [SubscriberKind::Webhook, SubscriberKind::NotifyNative] {
            assert_eq!(SubscriberKind::parse(kind.as_db()), Some(kind));
        }
        assert_eq!(SubscriberKind::parse("nope"), None);
    }
}
