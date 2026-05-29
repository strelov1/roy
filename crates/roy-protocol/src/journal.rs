use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Result, RoyError};
use crate::event::{event_from_json, TurnEvent};

pub type Seq = u64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub seq: Seq,
    /// Wall-clock millis since epoch. `seq` is still the ordering key — many
    /// events share a millisecond during a streamed turn.
    pub ts_ms: u64,
    pub event: TurnEvent,
}

/// Parse one JSONL line into a `JournalEntry`. Single source of truth for the
/// on-disk format, used by the `Journal`/`ArchivedJournal` actors in roy core.
/// Returns `Protocol` errors with the offending line so a corrupt journal
/// surfaces clearly instead of silently dropping entries.
pub fn parse_entry_line(line: &str) -> Result<JournalEntry> {
    let v: Value = serde_json::from_str(line).map_err(|e| RoyError::Protocol(e.to_string()))?;
    let seq = v
        .get("seq")
        .and_then(Value::as_u64)
        .ok_or_else(|| RoyError::Protocol(format!("journal entry missing seq: {line}")))?;
    let ts_ms = v
        .get("ts_ms")
        .and_then(Value::as_u64)
        .ok_or_else(|| RoyError::Protocol(format!("journal entry missing ts_ms: {line}")))?;
    let event = event_from_json(
        v.get("event")
            .ok_or_else(|| RoyError::Protocol(format!("journal entry missing event: {line}")))?,
    )?;
    Ok(JournalEntry { seq, ts_ms, event })
}
