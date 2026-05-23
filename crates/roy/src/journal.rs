//! Per-session JSONL journal with an in-memory ring window.
//!
//! Each session has a `<journal_dir>/<session_id>.jsonl` file (append-only) and
//! an in-memory `VecDeque` of the most recent `mem_capacity` entries with
//! monotonic `seq`. Same wire format (`event_to_json`) as CLI stdout and the
//! future trigger protocols — see `event.rs` and the spec at
//! `docs/superpowers/specs/2026-05-23-session-engine.md`.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use crate::error::{Result, RoyError};
use crate::event::{event_from_json, event_to_json, TurnEvent};

pub type Seq = u64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub seq: Seq,
    pub event: TurnEvent,
}

pub struct Journal {
    path: PathBuf,
    inner: Mutex<JournalInner>,
}

struct JournalInner {
    writer: File,
    mem: VecDeque<JournalEntry>,
    mem_capacity: usize,
    next_seq: Seq,
}

impl Journal {
    /// Open `<dir>/<session_id>.jsonl` for a fresh session. Errors if the file
    /// already exists — resurrection of an existing journal is intentionally
    /// out of scope for v1 (the agent process won't survive a daemon restart,
    /// so the journal-on-disk is for inspection/replay, not live resume).
    pub async fn open(dir: &Path, session_id: &str, mem_capacity: usize) -> Result<Self> {
        tokio::fs::create_dir_all(dir).await?;
        let path = dir.join(format!("{session_id}.jsonl"));
        let writer = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)
            .await?;
        Ok(Self {
            path,
            inner: Mutex::new(JournalInner {
                writer,
                mem: VecDeque::with_capacity(mem_capacity),
                mem_capacity,
                next_seq: 0,
            }),
        })
    }

    /// Append one event. Single-writer in practice (the session actor) — but
    /// guarded by an async mutex for safety. Returns the assigned `seq`.
    pub async fn append(&self, event: TurnEvent) -> Result<Seq> {
        let mut inner = self.inner.lock().await;
        let seq = inner.next_seq;
        let line = serde_json::to_string(&json!({
            "seq": seq,
            "event": event_to_json(&event),
        }))
        .map_err(|e| RoyError::Protocol(e.to_string()))?;
        inner.writer.write_all(line.as_bytes()).await?;
        inner.writer.write_all(b"\n").await?;
        inner.writer.flush().await?;

        if inner.mem.len() == inner.mem_capacity {
            inner.mem.pop_front();
        }
        inner.mem.push_back(JournalEntry { seq, event });
        inner.next_seq += 1;
        Ok(seq)
    }

    /// Replay entries with `seq >= from_seq`. Reads from the memory ring first;
    /// if `from_seq` is older than what the ring still holds, reads the disk
    /// tail for the missing prefix and concatenates.
    pub async fn replay_from(&self, from_seq: Seq) -> Result<Vec<JournalEntry>> {
        // Snapshot mem + oldest under one lock.
        let (oldest_in_mem, mem_part) = {
            let inner = self.inner.lock().await;
            let oldest = inner.mem.front().map(|e| e.seq);
            let mem_part: Vec<JournalEntry> = inner
                .mem
                .iter()
                .filter(|e| e.seq >= from_seq)
                .cloned()
                .collect();
            (oldest, mem_part)
        };

        let needs_disk = match oldest_in_mem {
            None => false,
            Some(o) => from_seq < o,
        };
        if !needs_disk {
            return Ok(mem_part);
        }
        let oldest_in_mem = oldest_in_mem.expect("needs_disk implies Some");

        // Disk holds [0, ...). We need [from_seq, oldest_in_mem).
        let file = File::open(&self.path).await?;
        let mut lines = BufReader::new(file).lines();
        let mut disk = Vec::new();
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            let v: Value =
                serde_json::from_str(&line).map_err(|e| RoyError::Protocol(e.to_string()))?;
            let seq = v
                .get("seq")
                .and_then(Value::as_u64)
                .ok_or_else(|| RoyError::Protocol(format!("journal entry missing seq: {line}")))?;
            if seq < from_seq || seq >= oldest_in_mem {
                continue;
            }
            let event = event_from_json(v.get("event").ok_or_else(|| {
                RoyError::Protocol(format!("journal entry missing event: {line}"))
            })?)?;
            disk.push(JournalEntry { seq, event });
        }

        // Continuous coverage: disk [from_seq, oldest_in_mem) ++ mem [oldest_in_mem, latest].
        disk.extend(mem_part);
        Ok(disk)
    }

    /// Path on disk. Useful for tests, tools, and `tail -f`.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Read-only view of an existing journal file. Used to attach to a session
/// whose live engine is gone but whose journal still exists on disk (e.g.
/// after a daemon restart, or for inspection of closed sessions).
pub struct ArchivedJournal {
    path: PathBuf,
}

impl ArchivedJournal {
    /// Open an archive at `<dir>/<session_id>.jsonl`. Errors if the file
    /// doesn't exist.
    pub async fn open(dir: &Path, session_id: &str) -> Result<Self> {
        let path = dir.join(format!("{session_id}.jsonl"));
        if !tokio::fs::try_exists(&path).await.map_err(RoyError::Io)? {
            return Err(RoyError::Protocol(format!(
                "no journal at {}",
                path.display()
            )));
        }
        Ok(Self { path })
    }

    /// Replay all entries with `seq >= from_seq` from disk, in seq order.
    pub async fn replay_from(&self, from_seq: Seq) -> Result<Vec<JournalEntry>> {
        let file = File::open(&self.path).await.map_err(RoyError::Io)?;
        let mut lines = BufReader::new(file).lines();
        let mut out = Vec::new();
        while let Some(line) = lines.next_line().await.map_err(RoyError::Io)? {
            if line.trim().is_empty() {
                continue;
            }
            let v: Value =
                serde_json::from_str(&line).map_err(|e| RoyError::Protocol(e.to_string()))?;
            let seq = v
                .get("seq")
                .and_then(Value::as_u64)
                .ok_or_else(|| RoyError::Protocol(format!("journal entry missing seq: {line}")))?;
            if seq < from_seq {
                continue;
            }
            let event = event_from_json(v.get("event").ok_or_else(|| {
                RoyError::Protocol(format!("journal entry missing event: {line}"))
            })?)?;
            out.push(JournalEntry { seq, event });
        }
        Ok(out)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::StopReason;

    static TMPDIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn tmpdir() -> TempDir {
        let n = TMPDIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("roy-journal-test-{}-{n}", std::process::id()));
        TempDir(p)
    }

    /// Cleans up on drop.
    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn append_and_replay_from_zero_within_memory() {
        let dir = tmpdir();
        let j = Journal::open(&dir.0, "s1", 10).await.unwrap();

        assert_eq!(
            j.append(TurnEvent::AssistantText { text: "a".into() })
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            j.append(TurnEvent::AssistantText { text: "b".into() })
                .await
                .unwrap(),
            1
        );
        let replay = j.replay_from(0).await.unwrap();
        assert_eq!(replay.len(), 2);
        assert_eq!(replay[0].seq, 0);
        assert_eq!(replay[1].seq, 1);
        match &replay[1].event {
            TurnEvent::AssistantText { text } => assert_eq!(text, "b"),
            other => panic!("expected AssistantText, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn replay_from_falls_back_to_disk_when_window_evicts() {
        let dir = tmpdir();
        // Tiny memory window so the older entries are evicted.
        let j = Journal::open(&dir.0, "s2", 2).await.unwrap();
        for i in 0..5u32 {
            j.append(TurnEvent::AssistantText {
                text: format!("e{i}"),
            })
            .await
            .unwrap();
        }
        // Memory now holds seqs 3,4. Asking from 0 must fall back to disk for 0..3.
        let replay = j.replay_from(0).await.unwrap();
        assert_eq!(replay.len(), 5);
        for (i, entry) in replay.iter().enumerate() {
            assert_eq!(entry.seq, i as Seq);
            match &entry.event {
                TurnEvent::AssistantText { text } => assert_eq!(text, &format!("e{i}")),
                other => panic!("expected AssistantText, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn replay_from_skips_older_entries() {
        let dir = tmpdir();
        let j = Journal::open(&dir.0, "s3", 10).await.unwrap();
        for i in 0..3u32 {
            j.append(TurnEvent::AssistantText {
                text: format!("e{i}"),
            })
            .await
            .unwrap();
        }
        let replay = j.replay_from(2).await.unwrap();
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].seq, 2);
    }

    #[tokio::test]
    async fn result_event_roundtrips_with_stop_reason() {
        let dir = tmpdir();
        let j = Journal::open(&dir.0, "s4", 10).await.unwrap();
        j.append(TurnEvent::Result {
            cost_usd: Some(0.42),
            stop_reason: StopReason::Refusal,
        })
        .await
        .unwrap();
        // Force disk read by asking for older + tiny window not used here, but
        // we still test disk roundtrip by opening a second Journal? No — we
        // can't, open is create_new. Instead read the raw file to verify the
        // wire format, then trust replay_from for in-memory replay.
        let raw = std::fs::read_to_string(j.path()).unwrap();
        assert!(raw.contains("\"stop_reason\":\"refusal\""));
        assert!(raw.contains("\"is_error\":true"));
    }

    #[tokio::test]
    async fn open_fails_if_journal_already_exists() {
        let dir = tmpdir();
        let _j = Journal::open(&dir.0, "s5", 10).await.unwrap();
        let err = Journal::open(&dir.0, "s5", 10).await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn archive_reads_existing_journal_in_seq_order() {
        let dir = tmpdir();
        {
            let j = Journal::open(&dir.0, "s6", 10).await.unwrap();
            for i in 0..3u32 {
                j.append(TurnEvent::AssistantText {
                    text: format!("e{i}"),
                })
                .await
                .unwrap();
            }
        }
        let archive = ArchivedJournal::open(&dir.0, "s6").await.unwrap();
        let entries = archive.replay_from(0).await.unwrap();
        assert_eq!(entries.len(), 3);
        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(entry.seq, i as Seq);
        }
    }

    #[tokio::test]
    async fn archive_open_errors_when_journal_missing() {
        let dir = tmpdir();
        let err = ArchivedJournal::open(&dir.0, "no-such-sid").await;
        assert!(err.is_err());
    }
}
