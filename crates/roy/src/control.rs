//! Control protocol shared by every trigger (CLI Unix socket, WebSocket, MCP,
//! ...) when talking to a `roy serve` daemon. Framing is transport-specific
//! (length-prefixed bytes on Unix socket, ws::Message::Text on WebSocket); the
//! payload — these enums — is the same.
//!
//! See `docs/superpowers/specs/2026-05-23-session-engine.md`.

use serde::{Deserialize, Serialize};

use crate::journal::{JournalEntry, Seq};

/// Commands sent from a trigger client to the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ClientCommand {
    /// Open a new session. `agent` is the preset name (claude_agent, gemini,
    /// opencode, codex). `resume` re-attaches an agent-side session via the
    /// transport's resume_cursor.
    Spawn {
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// `allow` / `deny`. Overrides the preset's default `PermissionPolicy`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume: Option<String>,
    },
    /// Subscribe to a session's `JournalEntry` stream. Optional `from_seq` for
    /// replay-from-N (default: from the start).
    Attach {
        session: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_seq: Option<Seq>,
    },
    /// Try to take the exclusive input writer for a session.
    AcquireInput { session: String },
    /// Queue a prompt; requires holding the input lease for `session`.
    Send { session: String, text: String },
    /// Release the input lease.
    ReleaseInput { session: String },
    /// Cancel only THIS connection's subscription to a session. The session
    /// keeps running.
    Detach { session: String },
    /// Ask the daemon to close a session and remove it from the registry.
    Close { session: String },
    /// List session ids known to the daemon.
    List,
}

/// Events sent from the daemon back to a trigger client. `session` ties an
/// event to a session id so one connection can multiplex N subscriptions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerEvent {
    /// Response to `Spawn`.
    Spawned {
        session: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume_cursor: Option<String>,
    },
    /// Response to `Attach`. `seq_at_attach` is the next seq after the replay.
    Attached { session: String, seq_at_attach: Seq },
    /// One journal entry on a subscribed session.
    Frame {
        session: String,
        entry: JournalEntry,
    },
    /// Response to `AcquireInput` (`acquired = false` → another client holds it).
    InputAcquired { session: String, acquired: bool },
    /// Response to `ReleaseInput`.
    InputReleased { session: String },
    /// Response to `Detach`.
    Detached { session: String },
    /// Response to `Close`.
    Closed { session: String },
    /// Response to `List`.
    Listed { sessions: Vec<String> },
    /// A command failed; if `session` is `Some`, the error pertains to that
    /// session.
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        code: String,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{StopReason, TurnEvent};

    fn roundtrip<T>(value: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(value).unwrap();
        let back: T = serde_json::from_str(&json).unwrap();
        assert_eq!(value, &back, "wire format mismatch:\n  {json}");
        back
    }

    #[test]
    fn spawn_command_roundtrips() {
        roundtrip(&ClientCommand::Spawn {
            agent: "opencode".into(),
            cwd: Some("/tmp/proj".into()),
            model: None,
            permission: Some("allow".into()),
            resume: None,
        });
    }

    #[test]
    fn attach_command_roundtrips() {
        roundtrip(&ClientCommand::Attach {
            session: "sid".into(),
            from_seq: Some(42),
        });
        roundtrip(&ClientCommand::Attach {
            session: "sid".into(),
            from_seq: None,
        });
    }

    #[test]
    fn list_command_serializes_as_bare_op() {
        let s = serde_json::to_string(&ClientCommand::List).unwrap();
        assert_eq!(s, "{\"op\":\"list\"}");
    }

    #[test]
    fn frame_event_roundtrips_with_typed_turn_event() {
        let entry = JournalEntry {
            seq: 7,
            event: TurnEvent::Result {
                cost_usd: Some(0.5),
                stop_reason: StopReason::EndTurn,
            },
        };
        roundtrip(&ServerEvent::Frame {
            session: "sid".into(),
            entry,
        });
    }

    #[test]
    fn error_event_serializes_without_session_when_absent() {
        let s = serde_json::to_string(&ServerEvent::Error {
            session: None,
            code: "no_lease".into(),
            message: "input lease not held".into(),
        })
        .unwrap();
        assert!(
            !s.contains("\"session\""),
            "session: None should be skipped: {s}"
        );
        assert!(s.contains("\"code\":\"no_lease\""));
    }
}
