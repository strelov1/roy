//! Control protocol shared by every trigger (CLI Unix socket, WebSocket, MCP,
//! ...) when talking to a `roy serve` daemon. Framing is transport-specific
//! (length-prefixed bytes on Unix socket, ws::Message::Text on WebSocket); the
//! payload — these enums — is the same.
//!
//! See `docs/architecture.md`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::event::TurnEvent;
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
    /// `DeleteArchive` failed (session still live, or IO error).
    DeleteFailed,
    /// `CancelTurn` failed (no such session, lease not held, etc.).
    CancelFailed,
    /// `SetModel` failed (no such session, metadata write failed).
    SetModelFailed,
    /// The named project id is not in the registry.
    NoProject,
    /// `CreateProject` failed because the canonical path is already owned.
    ProjectExists,
    /// `CreateProject` failed (FS / canonicalize / persist).
    CreateProjectFailed,
    /// `DeleteProject` failed (registry write).
    DeleteProjectFailed,
    /// `RenameProject` failed (unknown id / persist).
    RenameProjectFailed,
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
            ErrorCode::DeleteFailed => "delete_failed",
            ErrorCode::CancelFailed => "cancel_failed",
            ErrorCode::SetModelFailed => "set_model_failed",
            ErrorCode::NoProject => "no_project",
            ErrorCode::ProjectExists => "project_exists",
            ErrorCode::CreateProjectFailed => "create_project_failed",
            ErrorCode::DeleteProjectFailed => "delete_project_failed",
            ErrorCode::RenameProjectFailed => "rename_project_failed",
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
            "delete_failed" => ErrorCode::DeleteFailed,
            "cancel_failed" => ErrorCode::CancelFailed,
            "set_model_failed" => ErrorCode::SetModelFailed,
            "no_project" => ErrorCode::NoProject,
            "project_exists" => ErrorCode::ProjectExists,
            "create_project_failed" => ErrorCode::CreateProjectFailed,
            "delete_project_failed" => ErrorCode::DeleteProjectFailed,
            "rename_project_failed" => ErrorCode::RenameProjectFailed,
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
    /// Open a new session. `agent` is the preset name (claude, gemini,
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
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        tags: BTreeMap<String, String>,
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
    /// Cancel the in-flight turn for `session`. Requires holding the input
    /// lease (only the writer can cancel their own turn).
    CancelTurn { session: String },
    /// Release the input lease.
    ReleaseInput { session: String },
    /// Cancel only THIS connection's subscription to a session. The session
    /// keeps running.
    Detach { session: String },
    /// Update the LLM label recorded in `SessionMetadata.model`. The daemon
    /// rewrites the on-disk metadata, replies with `ModelChanged`, and
    /// journals a `System { subtype: "model_changed:<m>" }` entry so every
    /// attached client sees it through their `Frame` stream. Requires a
    /// live session — resume an archived one first. Note: roy doesn't
    /// currently steer the agent from this field — it's a display label.
    SetModel { session: String, model: String },
    /// Ask the daemon to close a session and remove it from the registry.
    Close { session: String },
    /// Permanently delete an archived session's journal + metadata files.
    /// Refuses if the session is currently live (close it first).
    DeleteArchive { session: String },
    /// List session ids known to the daemon.
    List,
    /// List session ids whose journals exist on disk but are not in the live
    /// registry (closed sessions, restart survivors).
    ListArchived,
    /// Resurrect a previously-closed session: rebuilds the engine using
    /// metadata persisted beside the journal, reuses the same session id and
    /// journal, and forwards the stored cursor to `Transport::open` for the
    /// agent-side resume (e.g. ACP `session/load`).
    Resume {
        session: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tags: Option<BTreeMap<String, String>>,
    },
    /// Replace the live session's tag map; emits `ServerEvent::SessionUpdated`.
    SetTags {
        session: String,
        tags: BTreeMap<String, String>,
    },
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
    /// Long-poll for the next terminal `Result` event in `session`. Resolves
    /// when an entry with `event: Result { .. }` and `seq >= since_seq` lands
    /// in the journal.
    WaitForResult {
        session: String,
        /// Default 0: "wait for the next Result after now".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        since_seq: Option<Seq>,
        /// Default 600_000 (10 min).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    /// Composite command: Run (or Resume) + WaitForResult.
    Fire {
        target: FireTarget,
        prompt: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        tags: BTreeMap<String, String>,
        /// Default 600_000 (10 min).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
    /// Return all projects in the registry.
    ListProjects,
    /// Create a project at `path`. If `name` is None, daemon uses
    /// `basename(canonical(path))`. Path must exist on disk.
    CreateProject {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Rename a project. Path is immutable in this iteration.
    RenameProject { project_id: String, name: String },
    /// Cascade-delete a project: every session it owns is closed and its
    /// journal + metadata files are erased, then the registry entry is
    /// removed. Synchronous.
    DeleteProject { project_id: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FireTarget {
    Spawn {
        preset: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    Resume {
        session_id: String,
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
    /// `agent` is the preset name the session was spawned with (e.g. `claude`,
    /// `gemini`) — read from the live engine or the on-disk metadata.
    /// `model` is the LLM label the session was spawned with (e.g.
    /// `claude-opus-4-7`) — recorded for display only, the daemon does not
    /// currently steer the agent with it.
    Attached {
        session: String,
        seq_at_attach: Seq,
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
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
    /// Per-connection ack to `SetModel`. Other attached clients learn
    /// about the change via a `Frame` carrying a
    /// `System { subtype: "model_changed:<m>" }` journal entry.
    ModelChanged { session: String, model: String },
    /// Response to `Close`.
    Closed { session: String },
    /// Response to `DeleteArchive`.
    Deleted { session: String },
    /// Response to `List`.
    Listed { sessions: Vec<SessionInfo> },
    /// Response to `ListArchived`.
    ListedArchived { sessions: Vec<SessionInfo> },
    /// A session's metadata (model or tags) was updated.
    SessionUpdated {
        session: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tags: Option<BTreeMap<String, String>>,
    },
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
    /// Response to `WaitForResult`: the turn finished.
    ResultReady {
        session: String,
        seq: Seq,
        result: TurnEvent, // terminal Result
        assistant_text: String,
    },
    /// Response to `WaitForResult`: the timeout expired before a Result landed.
    WaitTimeout { session: String },
    /// Response to `Fire`: the turn finished.
    FireDone {
        session: String,
        seq_range: (Seq, Seq),
        result: TurnEvent, // terminal Result
        assistant_text: String,
    },
    /// Response to `Fire`: the timeout expired before a Result landed.
    FireTimeout {
        session: String,
        partial_seq_range: (Seq, Seq),
    },
    /// Response to `Fire`: an error occurred during spawn or turn.
    FireError {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        code: ErrorCode,
        message: String,
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

/// Rich metadata for a session, used by `List` and `ListArchived`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session: String,
    pub agent: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tags: BTreeMap<String, String>,
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
            tags: BTreeMap::new(),
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
    fn list_projects_serializes_as_bare_op() {
        let s = serde_json::to_string(&ClientCommand::ListProjects).unwrap();
        assert_eq!(s, "{\"op\":\"list_projects\"}");
    }

    #[test]
    fn create_project_roundtrips() {
        roundtrip(&ClientCommand::CreateProject {
            path: std::path::PathBuf::from("/tmp/proj"),
            name: Some("demo".into()),
        });
        roundtrip(&ClientCommand::CreateProject {
            path: std::path::PathBuf::from("/tmp/proj"),
            name: None,
        });
    }

    #[test]
    fn delete_project_roundtrips() {
        roundtrip(&ClientCommand::DeleteProject {
            project_id: "abc".into(),
        });
    }

    #[test]
    fn rename_project_roundtrips() {
        roundtrip(&ClientCommand::RenameProject {
            project_id: "abc".into(),
            name: "new-name".into(),
        });
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
            ErrorCode::DeleteFailed,
            ErrorCode::CancelFailed,
            ErrorCode::SetModelFailed,
            ErrorCode::NoProject,
            ErrorCode::ProjectExists,
            ErrorCode::CreateProjectFailed,
            ErrorCode::DeleteProjectFailed,
            ErrorCode::RenameProjectFailed,
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
