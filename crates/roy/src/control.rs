//! Control protocol shared by every trigger (CLI Unix socket, WebSocket, MCP,
//! ...) when talking to a `roy serve` daemon. Framing is transport-specific
//! (length-prefixed bytes on Unix socket, ws::Message::Text on WebSocket); the
//! payload — these enums — is the same.
//!
//! See `docs/superpowers/specs/2026-05-23-session-engine.md`.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::journal::{JournalEntry, Seq};

/// Typed error codes emitted in `ServerEvent::Error`. Wire form is the
/// snake_case string returned by `as_wire`; unknown strings parse as
/// `Other(s)` so an older client can still read newer codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorCode {
    /// The JSON could not be parsed as a `ClientCommand`.
    BadRequest,
    /// `Spawn` failed (transport factory or `SessionManager::spawn`).
    SpawnFailed,
    /// The named session is not live (and, for `Attach`, no archive exists).
    NoSession,
    /// `Attach` failed after the session was found.
    AttachFailed,
    /// Reading the on-disk archive failed.
    ArchiveReadFailed,
    /// `Send` was issued without this connection holding the input lease.
    NoLease,
    /// `InputLease::send` failed (engine actor gone).
    SendFailed,
    /// `Close` failed.
    CloseFailed,
    /// `ListArchived` failed (e.g. journal_dir unreadable).
    ListArchivedFailed,
    /// `Resume` failed (missing metadata, transport build failed, etc.).
    ResumeFailed,
    /// `ReadJournal` failed.
    ReadJournalFailed,
    /// Forward-compat: a code emitted by a newer server.
    Other(String),
}

impl ErrorCode {
    pub fn as_wire(&self) -> &str {
        match self {
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::SpawnFailed => "spawn_failed",
            ErrorCode::NoSession => "no_session",
            ErrorCode::AttachFailed => "attach_failed",
            ErrorCode::ArchiveReadFailed => "archive_read_failed",
            ErrorCode::NoLease => "no_lease",
            ErrorCode::SendFailed => "send_failed",
            ErrorCode::CloseFailed => "close_failed",
            ErrorCode::ListArchivedFailed => "list_archived_failed",
            ErrorCode::ResumeFailed => "resume_failed",
            ErrorCode::ReadJournalFailed => "read_journal_failed",
            ErrorCode::Other(s) => s.as_str(),
        }
    }

    pub fn from_wire(s: &str) -> Self {
        match s {
            "bad_request" => ErrorCode::BadRequest,
            "spawn_failed" => ErrorCode::SpawnFailed,
            "no_session" => ErrorCode::NoSession,
            "attach_failed" => ErrorCode::AttachFailed,
            "archive_read_failed" => ErrorCode::ArchiveReadFailed,
            "no_lease" => ErrorCode::NoLease,
            "send_failed" => ErrorCode::SendFailed,
            "close_failed" => ErrorCode::CloseFailed,
            "list_archived_failed" => ErrorCode::ListArchivedFailed,
            "resume_failed" => ErrorCode::ResumeFailed,
            "read_journal_failed" => ErrorCode::ReadJournalFailed,
            other => ErrorCode::Other(other.to_string()),
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire())
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: Serializer>(&self, ser: S) -> std::result::Result<S::Ok, S::Error> {
        ser.serialize_str(self.as_wire())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D: Deserializer<'de>>(de: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(de).map_err(D::Error::custom)?;
        Ok(ErrorCode::from_wire(&s))
    }
}

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
    /// List session ids whose journals exist on disk but are not in the live
    /// registry (closed sessions, restart survivors).
    ListArchived,
    /// Resurrect a previously-closed session: rebuilds the engine using
    /// metadata persisted beside the journal, reuses the same session id and
    /// journal, and forwards the stored cursor to `Transport::open` for the
    /// agent-side resume (e.g. ACP `session/load`).
    Resume { session: String },
    /// Snapshot read of a session's journal — works on live AND archived
    /// sessions. Unlike `Attach`, it does not subscribe to the live broadcast;
    /// the daemon returns the current journal slice and the client decides
    /// when to call again. Useful for polling.
    ReadJournal {
        session: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_seq: Option<Seq>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_entries: Option<usize>,
    },
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
    /// Response to `ListArchived`.
    ListedArchived { sessions: Vec<String> },
    /// Response to `Resume`. Same session id as requested; `resume_cursor`
    /// reflects what the transport reported after resuming.
    Resumed {
        session: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume_cursor: Option<String>,
    },
    /// Response to `ReadJournal`: the requested slice of the journal.
    /// `next_seq` is the seq the client should pass to its next `ReadJournal`
    /// to continue from where this one stopped.
    JournalRead {
        session: String,
        entries: Vec<JournalEntry>,
        next_seq: Seq,
        /// `true` if the snapshot was truncated by `max_entries` — more
        /// entries are already on disk waiting for a follow-up read.
        has_more: bool,
    },
    /// A command failed; if `session` is `Some`, the error pertains to that
    /// session.
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        code: ErrorCode,
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
            code: ErrorCode::NoLease,
            message: "input lease not held".into(),
        })
        .unwrap();
        assert!(
            !s.contains("\"session\""),
            "session: None should be skipped: {s}"
        );
        assert!(s.contains("\"code\":\"no_lease\""));
    }

    #[test]
    fn error_code_roundtrips_for_known_variants() {
        let cases = [
            ErrorCode::BadRequest,
            ErrorCode::SpawnFailed,
            ErrorCode::NoSession,
            ErrorCode::AttachFailed,
            ErrorCode::ArchiveReadFailed,
            ErrorCode::NoLease,
            ErrorCode::SendFailed,
            ErrorCode::CloseFailed,
            ErrorCode::ListArchivedFailed,
            ErrorCode::ResumeFailed,
            ErrorCode::ReadJournalFailed,
        ];
        for code in cases {
            let json = serde_json::to_string(&code).unwrap();
            assert!(
                json.starts_with('"') && json.ends_with('"'),
                "expected bare snake_case string, got {json}"
            );
            let back: ErrorCode = serde_json::from_str(&json).unwrap();
            assert_eq!(code, back);
        }
    }

    #[test]
    fn unknown_error_code_parses_into_other_and_re_serializes_verbatim() {
        let code: ErrorCode = serde_json::from_str("\"future_event\"").unwrap();
        assert_eq!(code, ErrorCode::Other("future_event".into()));
        assert_eq!(serde_json::to_string(&code).unwrap(), "\"future_event\"");
    }
}
