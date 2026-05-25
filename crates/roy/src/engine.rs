//! Session engine: long-lived per-session actor that pipes the agent's events
//! into a persistent journal and a live broadcast channel, lets N observers
//! `attach`, and gates writes via a single `InputLease`.
//!
//! See `docs/architecture.md` for the design.

use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc, oneshot};
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
///
/// `project_id = None` means the session is orphan (lives at
/// `<workspace>/<session_id>/`). `fixed_session_id` pins the UUID when the
/// daemon needs to know it before the engine mints one — required for orphan
/// sessions where the dir is pre-created as `<workspace>/<session_id>/`.
#[derive(Debug, Clone)]
pub struct SessionSpawnConfig {
    pub agent: crate::agents_config::AgentPreset,
    pub cwd: PathBuf,
    pub project_id: Option<String>,
    pub model: Option<String>,
    pub permission: Option<String>,
    /// Forwarded to `Transport::open` so the agent side resumes via its
    /// native mechanism (e.g. ACP `session/load`). The roy-side session id
    /// and journal are still freshly minted on `spawn`.
    pub resume_cursor: Option<String>,
    /// When set, the engine uses this value as the session UUID instead of
    /// minting a fresh one. Used by orphan spawn so the daemon can name the
    /// workspace dir after the session id before the engine is constructed.
    pub fixed_session_id: Option<String>,
    pub tags: BTreeMap<String, String>,
}

/// Owned by `SessionManager` (or directly by callers in single-session use).
pub struct SessionEngine {
    session_id: String,
    journal_dir: PathBuf,
    agent: String,
    cwd: PathBuf,
    project_id: Option<String>,
    /// Display label only; the daemon doesn't feed it back into the
    /// transport. `set_model` mutates it and rewrites on-disk metadata.
    model: StdMutex<Option<String>>,
    permission: Option<String>,
    resume_cursor: StdMutex<Option<String>>,
    tags: StdMutex<BTreeMap<String, String>>,
    journal: Arc<Journal>,
    broadcast_tx: broadcast::Sender<JournalEntry>,
    input_tx: mpsc::UnboundedSender<Cmd>,
    input_lease_held: StdMutex<bool>,
    /// Wall-clock of the most recent "activity" — either a journal append
    /// (`publish`) or an incoming prompt (`Cmd::Prompt` arriving at the
    /// actor). Used by `SessionManager::sweep_idle` to GC quiet sessions.
    last_activity: StdMutex<Instant>,
    /// True while a turn is being driven. Lets an out-of-band injector decide
    /// whether to wait for the in-flight turn before pushing its own prompt.
    turn_active: AtomicBool,
}

/// Terminal outcome of a turn, as reported back to an out-of-band injector:
/// `Ok(Some((result_seq, the Result event, concatenated assistant text)))`,
/// `Ok(None)` if the turn ended without a terminal `Result` (e.g. shutdown),
/// or `Err` if reading the journal failed.
type TurnOutcome = Result<Option<(Seq, TurnEvent, String)>>;

enum Cmd {
    Prompt(String),
    /// Out-of-band prompt that does NOT require the input lease. Unlike
    /// `Prompt`, an `Inject` that arrives mid-turn is queued and run after the
    /// current turn rather than dropped, so a background injector can never
    /// silently lose its turn or have its result mis-attributed. `done` fires
    /// with this specific turn's outcome once it completes.
    Inject {
        text: String,
        done: oneshot::Sender<TurnOutcome>,
    },
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
        let session_id = cfg
            .fixed_session_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
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
            agent: cfg.agent.to_string(),
            cwd: cfg.cwd.clone(),
            project_id: cfg.project_id.clone(),
            model: StdMutex::new(cfg.model.clone()),
            permission: cfg.permission.clone(),
            resume_cursor: StdMutex::new(initial_cursor.clone()),
            tags: StdMutex::new(cfg.tags.clone()),
            journal,
            broadcast_tx,
            input_tx,
            input_lease_held: StdMutex::new(false),
            last_activity: StdMutex::new(Instant::now()),
            turn_active: AtomicBool::new(false),
        });

        // Persist initial metadata so a daemon restart can find this session.
        // Propagate the error: without metadata on disk the session can't be
        // resumed, so a silent half-spawned state would be worse than failing.
        write_metadata(
            &opts.journal_dir,
            &SessionMetadata {
                session_id,
                agent: cfg.agent.to_string(),
                cwd: cfg.cwd,
                project_id: cfg.project_id, // Option<String> — None = orphan
                model: cfg.model,
                permission: cfg.permission,
                resume_cursor: initial_cursor,
                tags: cfg.tags,
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

    pub fn cwd(&self) -> &PathBuf {
        &self.cwd
    }

    pub fn project_id(&self) -> Option<&str> {
        self.project_id.as_deref()
    }

    /// LLM label currently associated with the session (e.g.
    /// `claude-opus-4-7`). Can change mid-session via `set_model`.
    pub fn model(&self) -> Option<String> {
        self.model.lock().unwrap().clone()
    }

    /// Update the model label, persist it, and broadcast the change so
    /// every attached subscriber sees it through their Frame stream.
    /// Returns the new value so callers can echo it on the wire reply.
    pub async fn set_model(&self, model: String) -> Result<String> {
        *self.model.lock().unwrap() = Some(model.clone());
        self.persist_metadata().await?;
        // Per-connection `ServerEvent::ModelChanged` is only the ack to
        // the requester; this is what reaches every other attached
        // client in lock-step via `ServerEvent::Frame`.
        publish(
            self,
            TurnEvent::System {
                subtype: format!("model_changed:{model}"),
            },
        )
        .await?;
        Ok(model)
    }

    /// Append a `Note` event to the journal + broadcast. Unlike a prompt this
    /// takes no input lease and never touches the transport, so it lands even
    /// while an interactive client holds the lease. Returns the appended seq.
    pub async fn inject_note(&self, text: String, source_session: Option<String>) -> Result<Seq> {
        let entry = publish(
            self,
            TurnEvent::Note {
                text,
                source_session,
            },
        )
        .await?;
        Ok(entry.seq)
    }

    /// True while a turn is in flight. An out-of-band injector waits on this
    /// before pushing a prompt, because a prompt that arrives mid-turn is
    /// dropped by the actor (`drive_turn`).
    pub fn is_busy(&self) -> bool {
        self.turn_active.load(Ordering::SeqCst)
    }

    /// Queue a prompt without holding the input lease. The actor journals it
    /// as a `UserPrompt` and drives a turn, exactly like a leased `send` — but
    /// if a turn is already running, the inject is queued and run after it
    /// (never dropped). The returned receiver resolves with *this* turn's
    /// outcome, so a caller can await the right result even when other turns
    /// run first. Used by `Inject { respond: true }`.
    pub fn inject_prompt(&self, text: String) -> Result<oneshot::Receiver<TurnOutcome>> {
        let (done, rx) = oneshot::channel();
        self.input_tx
            .send(Cmd::Inject { text, done })
            .map_err(|_| RoyError::Protocol("engine actor gone".into()))?;
        Ok(rx)
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

    pub async fn next_seq(&self) -> Seq {
        self.journal.next_seq().await
    }

    pub fn tags(&self) -> BTreeMap<String, String> {
        self.tags.lock().unwrap().clone()
    }

    /// Replace the session's tag map and persist it.
    pub async fn set_tags(&self, tags: BTreeMap<String, String>) -> Result<()> {
        {
            let mut current = self.tags.lock().unwrap();
            *current = tags;
        }
        self.persist_metadata().await?;
        Ok(())
    }

    /// Wait for the next terminal `Result` event with `seq >= since_seq`.
    /// Returns `None` only on timeout. Recovers from broadcast `Lagged`
    /// (capacity overrun) by re-scanning the journal from the last seq we saw.
    pub async fn wait_for_result(
        &self,
        since_seq: Seq,
        timeout: Duration,
    ) -> Result<Option<(Seq, TurnEvent, String)>> {
        let mut rx = self.broadcast_tx.subscribe();
        let mut scan_from = since_seq;
        let mut assistant_text = String::new();

        let fut = async {
            loop {
                // 1. Drain journal from scan_from onward. If we see Result, done.
                let entries = match self.journal.replay_from(scan_from).await {
                    Ok(es) => es,
                    Err(_) => return None,
                };
                let mut last_seen = scan_from;
                for entry in entries {
                    last_seen = entry.seq + 1;
                    match &entry.event {
                        TurnEvent::AssistantText { text } => assistant_text.push_str(text),
                        TurnEvent::Result { .. } => {
                            return Some((entry.seq, entry.event, assistant_text));
                        }
                        _ => {}
                    }
                }
                scan_from = last_seen;

                // 2. Wait for the next broadcast entry. On Lagged, loop back to (1).
                match rx.recv().await {
                    Ok(entry) => {
                        if entry.seq < scan_from {
                            continue;
                        }
                        scan_from = entry.seq + 1;
                        match entry.event {
                            TurnEvent::AssistantText { text } => assistant_text.push_str(&text),
                            TurnEvent::Result { .. } => {
                                return Some((entry.seq, entry.event, assistant_text));
                            }
                            _ => {}
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Re-subscribe + re-scan journal from where we left off.
                        rx = self.broadcast_tx.subscribe();
                        // assistant_text already holds everything < scan_from;
                        // the next loop iteration replays journal[scan_from..].
                        continue;
                    }
                }
            }
        };

        match tokio::time::timeout(timeout, fut).await {
            Ok(res) => Ok(res),
            Err(_) => Ok(None),
        }
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

    fn metadata_snapshot(&self) -> SessionMetadata {
        SessionMetadata {
            session_id: self.session_id.clone(),
            agent: self.agent.clone(),
            cwd: self.cwd.clone(),
            project_id: self.project_id.clone(), // Option<String> — None = orphan
            model: self.model.lock().unwrap().clone(),
            permission: self.permission.clone(),
            resume_cursor: self.resume_cursor.lock().unwrap().clone(),
            tags: self.tags.lock().unwrap().clone(),
        }
    }

    async fn persist_metadata(&self) -> Result<()> {
        write_metadata(&self.journal_dir, &self.metadata_snapshot()).await
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

/// A turn the actor still owes: its prompt text plus the channel to report the
/// outcome on. Only `Inject`s land here — they're queued when they arrive
/// mid-turn so they run (in order) after the current turn instead of being
/// dropped.
type PendingTurn = (String, oneshot::Sender<TurnOutcome>);

async fn run_actor(
    engine: Arc<SessionEngine>,
    mut handle: Box<dyn Handle>,
    mut input_rx: mpsc::UnboundedReceiver<Cmd>,
) {
    let mut pending: VecDeque<PendingTurn> = VecDeque::new();
    loop {
        // Drain injects queued during the previous turn before blocking for a
        // new command — they were deferred precisely so they'd run now.
        let (text, done) = if let Some((text, done)) = pending.pop_front() {
            (text, Some(done))
        } else {
            match input_rx.recv().await {
                Some(Cmd::Prompt(text)) => (text, None),
                Some(Cmd::Inject { text, done }) => (text, Some(done)),
                // Cancel outside an active turn is a no-op; the turn-driving
                // loop is the only place a cancel actually means something.
                Some(Cmd::Cancel) => continue,
                Some(Cmd::Close) | None => break,
            }
        };
        run_one_turn(
            &engine,
            handle.as_mut(),
            &text,
            &mut input_rx,
            &mut pending,
            done,
        )
        .await;
    }
    if let Err(e) = handle.close().await {
        tracing::warn!(
            session = %engine.session_id,
            error = %e,
            "transport close failed; child process may be left in unknown state",
        );
    }
}

/// Journal the prompt, drive one turn to completion, persist the cursor, and —
/// for an injected turn — report the outcome on `done`. Shared by the leased
/// (`Prompt`) and lease-free (`Inject`) paths so both behave identically.
async fn run_one_turn(
    engine: &SessionEngine,
    handle: &mut dyn Handle,
    text: &str,
    input_rx: &mut mpsc::UnboundedReceiver<Cmd>,
    pending: &mut VecDeque<PendingTurn>,
    done: Option<oneshot::Sender<TurnOutcome>>,
) {
    // Captured before the UserPrompt is journaled so `wait_for_result` below
    // sees this turn's own terminal Result (turns run strictly serially, so
    // the first Result at `seq >= since` is unambiguously ours).
    let since = engine.next_seq().await;
    engine.touch_activity();
    // Journal the user's prompt before driving the turn. Agents don't echo
    // user input over ACP, so without this step a refresh / late attach can
    // never reconstruct the user side of the conversation.
    if let Err(e) = publish(
        engine,
        TurnEvent::UserPrompt {
            text: text.to_string(),
        },
    )
    .await
    {
        tracing::error!(
            session = %engine.session_id,
            error = %e,
            "failed to journal user prompt; turn still dispatched",
        );
    }
    engine.turn_active.store(true, Ordering::SeqCst);
    drive_turn(engine, handle, text, input_rx, pending).await;
    engine.turn_active.store(false, Ordering::SeqCst);
    if let Some(cursor) = handle.resume_cursor() {
        *engine.resume_cursor.lock().unwrap() = Some(cursor);
        // Non-fatal: session keeps running, but a stale cursor on disk means a
        // future Resume reconnects to the wrong agent-side session. Surface it.
        if let Err(e) = engine.persist_metadata().await {
            tracing::warn!(
                session = %engine.session_id,
                error = %e,
                "failed to persist session metadata after turn",
            );
        }
    }
    // The turn is done and its terminal Result is already journaled, so this
    // read resolves immediately; the short timeout only guards the degenerate
    // "turn ended without a Result" case (e.g. shutdown mid-turn).
    if let Some(done) = done {
        let outcome = engine
            .wait_for_result(since, std::time::Duration::from_secs(5))
            .await;
        let _ = done.send(outcome);
    }
}

async fn drive_turn(
    engine: &SessionEngine,
    handle: &mut dyn Handle,
    text: &str,
    input_rx: &mut mpsc::UnboundedReceiver<Cmd>,
    pending: &mut VecDeque<PendingTurn>,
) {
    let (mut stream, cancel) = match handle.send(text).await {
        Ok(pair) => pair,
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
    // Hold the cancel signal in an Option so the Cancel arm can drop it once.
    // Drop = ACP `session/cancel`; the stream stays open and still yields the
    // terminal `Result`, so we stay in the loop after a cancel.
    let mut cancel = Some(cancel);
    loop {
        tokio::select! {
            biased;
            cmd = input_rx.recv() => match cmd {
                Some(Cmd::Cancel) => {
                    drop(cancel.take());
                }
                Some(Cmd::Prompt(_)) => {
                    // The lease holder shouldn't send while their own turn
                    // runs; dropping is correct (Inject, below, is queued).
                    tracing::warn!(
                        session = %engine.session_id,
                        "ignoring Cmd::Prompt during active turn",
                    );
                }
                Some(Cmd::Inject { text, done }) => {
                    // Queue, don't drop: run after the current turn finishes.
                    pending.push_back((text, done));
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
                    let catchup = match journal.replay_from(from).await {
                        Ok(entries) => entries,
                        Err(e) => {
                            // Disk read failed; without catch-up the subscriber
                            // would silently stall. Log and stop the stream so
                            // the caller can attach again with a fresh seq.
                            tracing::error!(error = %e, %from, "lagged catch-up replay failed");
                            break;
                        }
                    };
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
