//! Session engine: long-lived per-session actor that pipes the agent's events
//! into a persistent journal and a live broadcast channel, lets N observers
//! `attach`, and gates writes via a single `InputLease`.
//!
//! See `docs/architecture.md` for the design.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

use tokio::sync::{broadcast, mpsc};
use tokio_stream::{Stream, StreamExt};
use uuid::Uuid;

use crate::error::{Result, RoyError};
use crate::event::{StopReason, TurnEvent};
use crate::journal::{Journal, JournalEntry, Seq};
use crate::session_meta::{write_metadata, SessionMetadata};
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

/// Inputs that uniquely identify a session at spawn (or resume) time. Stored
/// as `SessionMetadata` beside the journal so the daemon can resurrect a live
/// session after a restart.
#[derive(Debug, Clone)]
pub struct SessionSpawnConfig {
    pub agent: String,
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub permission: Option<String>,
    /// Forwarded to `Transport::open` so the agent side resumes via its
    /// native mechanism (e.g. ACP `session/load`). The roy-side session id
    /// and journal are still freshly minted on `spawn`.
    pub resume_cursor: Option<String>,
}

/// Owned by `SessionManager` (or directly by callers in single-session use).
pub struct SessionEngine {
    session_id: String,
    journal_dir: PathBuf,
    agent: String,
    cwd: PathBuf,
    model: Option<String>,
    permission: Option<String>,
    resume_cursor: StdMutex<Option<String>>,
    journal: Arc<Journal>,
    broadcast_tx: broadcast::Sender<JournalEntry>,
    input_tx: mpsc::UnboundedSender<Cmd>,
    input_lease_held: StdMutex<bool>,
    /// Wall-clock of the most recent "activity" — either a journal append
    /// (`publish`) or an incoming prompt (`Cmd::Prompt` arriving at the
    /// actor). Used by `SessionManager::sweep_idle` to GC quiet sessions.
    last_activity: StdMutex<Instant>,
}

enum Cmd {
    Prompt(String),
    /// Abort the in-flight turn. No-op if no turn is running. The actor reacts
    /// by dropping the current `TurnStream`, which makes the transport send
    /// `session/cancel` to the agent; the synthesised terminal `Result` lands
    /// in the journal with `stop_reason: Cancelled`.
    Cancel,
    Close,
}

impl SessionEngine {
    /// Open a transport handle for a fresh session, set up journal +
    /// broadcast, persist metadata, and spawn the actor task. The returned
    /// `Arc<SessionEngine>` is cheap to clone and hand to multiple attach
    /// sites.
    pub async fn spawn(
        transport: Arc<dyn Transport>,
        opts: EngineOpts,
        cfg: SessionSpawnConfig,
    ) -> Result<Arc<Self>> {
        let session_id = Uuid::new_v4().to_string();
        let journal =
            Arc::new(Journal::open(&opts.journal_dir, &session_id, opts.mem_capacity).await?);
        Self::start(transport, opts, session_id, journal, cfg).await
    }

    /// Resurrect a previously-closed session: open its existing journal in
    /// append mode and re-spawn the actor with the same id. The supplied
    /// `cfg.resume_cursor` is what the daemon retrieved from the on-disk
    /// metadata and is forwarded to `Transport::open`.
    pub async fn resume(
        transport: Arc<dyn Transport>,
        opts: EngineOpts,
        session_id: String,
        cfg: SessionSpawnConfig,
    ) -> Result<Arc<Self>> {
        let journal =
            Arc::new(Journal::resume(&opts.journal_dir, &session_id, opts.mem_capacity).await?);
        Self::start(transport, opts, session_id, journal, cfg).await
    }

    async fn start(
        transport: Arc<dyn Transport>,
        opts: EngineOpts,
        session_id: String,
        journal: Arc<Journal>,
        cfg: SessionSpawnConfig,
    ) -> Result<Arc<Self>> {
        let (broadcast_tx, _) = broadcast::channel::<JournalEntry>(opts.broadcast_capacity);
        let (input_tx, input_rx) = mpsc::unbounded_channel();

        let handle = transport
            .open(&session_id, cfg.resume_cursor.as_deref(), cfg.cwd.clone())
            .await?;
        let initial_cursor = handle.resume_cursor().or(cfg.resume_cursor.clone());

        let engine = Arc::new(Self {
            session_id: session_id.clone(),
            journal_dir: opts.journal_dir.clone(),
            agent: cfg.agent.clone(),
            cwd: cfg.cwd.clone(),
            model: cfg.model.clone(),
            permission: cfg.permission.clone(),
            resume_cursor: StdMutex::new(initial_cursor.clone()),
            journal,
            broadcast_tx,
            input_tx,
            input_lease_held: StdMutex::new(false),
            last_activity: StdMutex::new(Instant::now()),
        });

        // Persist initial metadata so a daemon restart can find this session.
        // Propagate the error: without metadata on disk the session can't be
        // resumed, so a silent half-spawned state would be worse than failing.
        write_metadata(
            &opts.journal_dir,
            &SessionMetadata {
                session_id,
                agent: cfg.agent,
                cwd: cfg.cwd,
                model: cfg.model,
                permission: cfg.permission,
                resume_cursor: initial_cursor,
            },
        )
        .await?;

        let engine_for_actor = Arc::clone(&engine);
        tokio::spawn(run_actor(engine_for_actor, handle, input_rx));

        tracing::info!(
            session = %engine.session_id,
            agent = %engine.agent,
            cwd = %engine.cwd.display(),
            "session engine started"
        );
        Ok(engine)
    }

    pub fn id(&self) -> &str {
        &self.session_id
    }

    pub fn agent(&self) -> &str {
        &self.agent
    }

    /// Most recent activity timestamp. Used by `SessionManager::sweep_idle`.
    pub fn last_activity(&self) -> Instant {
        *self.last_activity.lock().unwrap()
    }

    fn touch_activity(&self) {
        *self.last_activity.lock().unwrap() = Instant::now();
    }

    pub fn resume_cursor(&self) -> Option<String> {
        self.resume_cursor.lock().unwrap().clone()
    }

    /// Read-only journal snapshot of this live session. Same disk read as
    /// `ArchivedJournal::replay_from`, but available on live sessions too
    /// without subscribing to the broadcast — for poll-style clients.
    pub async fn snapshot(&self, from_seq: Seq) -> Result<Vec<JournalEntry>> {
        self.journal.replay_from(from_seq).await
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

    /// Cancel the currently-running turn (if any). No-op when the engine is
    /// idle. Anyone observing the session sees a terminal `Result` with
    /// `stop_reason: Cancelled` once the transport finishes shutting the
    /// agent-side prompt down.
    pub fn cancel_turn(&self) -> Result<()> {
        self.input_tx
            .send(Cmd::Cancel)
            .map_err(|_| RoyError::Protocol("engine actor gone".into()))
    }

    async fn persist_metadata(&self) {
        let cursor = self.resume_cursor.lock().unwrap().clone();
        let meta = SessionMetadata {
            session_id: self.session_id.clone(),
            agent: self.agent.clone(),
            cwd: self.cwd.clone(),
            model: self.model.clone(),
            permission: self.permission.clone(),
            resume_cursor: cursor,
        };
        // Non-fatal: the session keeps running, but a stale cursor on disk
        // means a future Resume will reconnect to the wrong agent-side
        // session. Surface it so operators see the divergence.
        if let Err(e) = write_metadata(&self.journal_dir, &meta).await {
            tracing::warn!(
                session = %self.session_id,
                error = %e,
                "failed to persist session metadata after turn"
            );
        }
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
                engine.touch_activity();
                drive_turn(&engine, handle.as_mut(), &text, &mut input_rx).await;
                if let Some(cursor) = handle.resume_cursor() {
                    *engine.resume_cursor.lock().unwrap() = Some(cursor);
                    engine.persist_metadata().await;
                }
            }
            // Cancel outside an active turn is a no-op; the turn-driving loop
            // is the only place a cancel actually means something.
            Cmd::Cancel => {}
            Cmd::Close => break,
        }
    }
    let _ = handle.close().await;
}

async fn drive_turn(
    engine: &SessionEngine,
    handle: &mut dyn Handle,
    text: &str,
    input_rx: &mut mpsc::UnboundedReceiver<Cmd>,
) {
    let mut stream = match handle.send(text).await {
        Ok(s) => s,
        Err(e) => {
            // The transport refused the turn; synthesise a terminal Result
            // so attach subscribers still see a turn boundary.
            tracing::warn!(
                session = %engine.session_id,
                error = %e,
                "transport refused turn; synthesising terminal Result",
            );
            if let Err(e) = publish(
                engine,
                TurnEvent::Result {
                    cost_usd: None,
                    stop_reason: StopReason::Error,
                },
            )
            .await
            {
                tracing::error!(
                    session = %engine.session_id,
                    error = %e,
                    "failed to journal synthetic terminal Result",
                );
            }
            return;
        }
    };
    // Drive the turn while draining `input_rx` for a Cancel. Dropping the
    // stream is what fires the transport's cancel_tx → ACP session/cancel,
    // which yields a terminal `Result { stop_reason: Cancelled }`.
    loop {
        tokio::select! {
            biased;
            cmd = input_rx.recv() => match cmd {
                Some(Cmd::Cancel) => {
                    // Drain the cancelled turn so attach subscribers see the
                    // terminal Result, then return.
                    let mut s = stream;
                    while let Some(event) = s.next().await {
                        if let Err(e) = publish(engine, event).await {
                            tracing::error!(
                                session = %engine.session_id,
                                error = %e,
                                "journal append failed during cancel drain",
                            );
                        }
                    }
                    return;
                }
                Some(Cmd::Prompt(_)) => {
                    tracing::warn!(
                        session = %engine.session_id,
                        "ignoring Cmd::Prompt during active turn",
                    );
                }
                Some(Cmd::Close) | None => return,
            },
            event = stream.next() => match event {
                Some(event) => {
                    if let Err(e) = publish(engine, event).await {
                        tracing::error!(
                            session = %engine.session_id,
                            error = %e,
                            "journal append failed",
                        );
                    }
                }
                None => break,
            },
        }
    }
}

async fn publish(engine: &SessionEngine, event: TurnEvent) -> Result<JournalEntry> {
    let seq = engine.journal.append(event.clone()).await?;
    let entry = JournalEntry { seq, event };
    engine.touch_activity();
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
