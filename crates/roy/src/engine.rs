//! Session engine: long-lived per-session actor that pipes the agent's events
//! into a persistent journal and a live broadcast channel, lets N observers
//! `attach`, and gates writes via a single `InputLease`.
//!
//! See `docs/superpowers/specs/2026-05-23-session-engine.md` for the design.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::{broadcast, mpsc, RwLock};
use tokio_stream::{Stream, StreamExt};
use uuid::Uuid;

use crate::error::{Result, RoyError};
use crate::event::{StopReason, TurnEvent};
use crate::journal::{Journal, JournalEntry, Seq};
use crate::transport::{Handle, Transport};

/// Tunables for `SessionEngine::spawn`.
#[derive(Debug, Clone)]
pub struct EngineOpts {
    pub journal_dir: PathBuf,
    /// Bounded live broadcast. A slow attach subscriber that lags more than
    /// this falls back to journal replay from its last seq.
    pub broadcast_capacity: usize,
    /// In-memory ring window for fast `attach` replay before disk reads.
    pub mem_capacity: usize,
}

impl EngineOpts {
    pub fn with_journal_dir(dir: PathBuf) -> Self {
        Self {
            journal_dir: dir,
            broadcast_capacity: 256,
            mem_capacity: 1024,
        }
    }
}

/// Owned by `SessionManager` (or directly by callers in single-session use).
pub struct SessionEngine {
    session_id: String,
    resume_cursor: RwLock<Option<String>>,
    journal: Arc<Journal>,
    broadcast_tx: broadcast::Sender<JournalEntry>,
    input_tx: mpsc::UnboundedSender<Cmd>,
    input_lease_held: StdMutex<bool>,
}

enum Cmd {
    Prompt(String),
    Close,
}

impl SessionEngine {
    /// Open a transport handle, set up journal + broadcast, and spawn the
    /// actor task. `resume_cursor` is forwarded to `Transport::open` so the
    /// agent side resumes via its native mechanism (e.g. ACP `session/load`)
    /// while roy itself gets a fresh manager-side session id and journal.
    /// The returned `Arc<SessionEngine>` is cheap to clone and hand to
    /// multiple attach sites.
    pub async fn spawn(
        transport: Arc<dyn Transport>,
        cwd: PathBuf,
        opts: EngineOpts,
        resume_cursor: Option<String>,
    ) -> Result<Arc<Self>> {
        let session_id = Uuid::new_v4().to_string();
        let journal =
            Arc::new(Journal::open(&opts.journal_dir, &session_id, opts.mem_capacity).await?);
        let (broadcast_tx, _) = broadcast::channel::<JournalEntry>(opts.broadcast_capacity);
        let (input_tx, input_rx) = mpsc::unbounded_channel();

        let handle = transport
            .open(&session_id, resume_cursor.as_deref(), cwd)
            .await?;
        let initial_cursor = handle.resume_cursor();

        let engine = Arc::new(Self {
            session_id,
            resume_cursor: RwLock::new(initial_cursor),
            journal,
            broadcast_tx,
            input_tx,
            input_lease_held: StdMutex::new(false),
        });

        let engine_for_actor = Arc::clone(&engine);
        tokio::spawn(run_actor(engine_for_actor, handle, input_rx));

        Ok(engine)
    }

    pub fn id(&self) -> &str {
        &self.session_id
    }

    pub async fn resume_cursor(&self) -> Option<String> {
        self.resume_cursor.read().await.clone()
    }

    /// Subscribe an observer. Race-free: subscribes to live broadcast first,
    /// then reads journal up to that point, then yields the splice.
    pub async fn attach(&self, from_seq: Option<Seq>) -> Result<Attach> {
        let rx = self.broadcast_tx.subscribe();
        let from = from_seq.unwrap_or(0);
        let replay = self.journal.replay_from(from).await?;
        let next_after_replay = replay.last().map(|e| e.seq + 1).unwrap_or(from);
        let stream = build_attach_stream(replay, rx, next_after_replay, Arc::clone(&self.journal));
        Ok(Attach {
            seq_at_attach: next_after_replay,
            stream,
        })
    }

    /// Acquire the exclusive input writer. `None` if another lease is alive.
    pub fn try_acquire_input(self: &Arc<Self>) -> Option<InputLease> {
        let mut held = self.input_lease_held.lock().unwrap();
        if *held {
            return None;
        }
        *held = true;
        Some(InputLease {
            engine: Arc::clone(self),
        })
    }

    /// Ask the actor to wind the session down. Returns once the close
    /// command is queued; the actor closes the underlying `Handle` on its
    /// own timeline.
    pub fn close(&self) -> Result<()> {
        self.input_tx
            .send(Cmd::Close)
            .map_err(|_| RoyError::Protocol("engine actor gone".into()))
    }
}

/// Per-attach handle: snapshot seq + the live stream.
pub struct Attach {
    pub seq_at_attach: Seq,
    pub stream: Pin<Box<dyn Stream<Item = JournalEntry> + Send>>,
}

/// Exclusive input writer. Drop releases the lease.
pub struct InputLease {
    engine: Arc<SessionEngine>,
}

impl InputLease {
    pub fn send(&self, prompt: impl Into<String>) -> Result<()> {
        self.engine
            .input_tx
            .send(Cmd::Prompt(prompt.into()))
            .map_err(|_| RoyError::Protocol("engine actor gone".into()))
    }

    pub fn engine(&self) -> &Arc<SessionEngine> {
        &self.engine
    }
}

impl Drop for InputLease {
    fn drop(&mut self) {
        if let Ok(mut held) = self.engine.input_lease_held.lock() {
            *held = false;
        }
    }
}

async fn run_actor(
    engine: Arc<SessionEngine>,
    mut handle: Box<dyn Handle>,
    mut input_rx: mpsc::UnboundedReceiver<Cmd>,
) {
    while let Some(cmd) = input_rx.recv().await {
        match cmd {
            Cmd::Prompt(text) => {
                drive_turn(&engine, handle.as_mut(), &text).await;
                if let Some(cursor) = handle.resume_cursor() {
                    *engine.resume_cursor.write().await = Some(cursor);
                }
            }
            Cmd::Close => break,
        }
    }
    let _ = handle.close().await;
}

async fn drive_turn(engine: &SessionEngine, handle: &mut dyn Handle, text: &str) {
    let mut stream = match handle.send(text).await {
        Ok(s) => s,
        Err(_) => {
            // The transport refused the turn; synthesise a terminal Result
            // so attach subscribers still see a turn boundary.
            let _ = publish(
                engine,
                TurnEvent::Result {
                    cost_usd: None,
                    stop_reason: StopReason::Error,
                },
            )
            .await;
            return;
        }
    };
    while let Some(event) = stream.next().await {
        let _ = publish(engine, event).await;
    }
}

async fn publish(engine: &SessionEngine, event: TurnEvent) -> Result<JournalEntry> {
    let seq = engine.journal.append(event.clone()).await?;
    let entry = JournalEntry { seq, event };
    // No receivers is not an error.
    let _ = engine.broadcast_tx.send(entry.clone());
    Ok(entry)
}

/// Stitch journal replay + live broadcast into one ordered, dedup'd stream.
/// On `Lagged`, re-read the journal from the last yielded seq + 1 and keep
/// going — the agent never blocks for a slow subscriber.
fn build_attach_stream(
    replay: Vec<JournalEntry>,
    rx: broadcast::Receiver<JournalEntry>,
    next_seq: Seq,
    journal: Arc<Journal>,
) -> Pin<Box<dyn Stream<Item = JournalEntry> + Send>> {
    Box::pin(async_stream::stream! {
        let mut last_yielded: Option<Seq> = None;
        for entry in replay {
            last_yielded = Some(entry.seq);
            yield entry;
        }
        let mut rx = rx;
        let mut expected_next = next_seq;
        loop {
            match rx.recv().await {
                Ok(entry) => {
                    if entry.seq < expected_next {
                        continue; // dedup against replay overlap
                    }
                    expected_next = entry.seq + 1;
                    last_yielded = Some(entry.seq);
                    yield entry;
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let from = last_yielded.map(|s| s + 1).unwrap_or(expected_next);
                    let catchup = journal.replay_from(from).await.unwrap_or_default();
                    for entry in catchup {
                        expected_next = entry.seq + 1;
                        last_yielded = Some(entry.seq);
                        yield entry;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}
