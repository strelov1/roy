//! Session engine: long-lived per-session actor that pipes the agent's events
//! into a persistent journal and a live broadcast channel, lets N observers
//! `attach`, and gates writes via a single `InputLease`.
//!
//! See `docs/architecture.md` for the design.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::{Stream, StreamExt};
use uuid::Uuid;

use crate::error::{Result, RoyError};
use crate::event::{StopReason, TurnEvent};
use crate::journal::{Journal, JournalEntry, Seq};
use crate::session_store::{SessionRow, SessionStore};
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

/// Inputs that uniquely identify a session at spawn (or resume) time. Persisted
/// as a `SessionRow` in the session store so the daemon can resurrect a live
/// session after a restart.
///
/// `cwd = None` means the caller wants an orphan session: the manager allocates
/// `<workspace>/<session_id>/` for it and rewrites `cwd` to that path before
/// the engine is constructed. `fixed_session_id` pins the UUID when the daemon
/// needs to know it before the engine mints one — required for orphan sessions
/// where the dir is pre-created as `<workspace>/<session_id>/`.
#[derive(Debug, Clone)]
pub struct SessionSpawnConfig {
    pub agent: crate::agents_config::AgentPreset,
    pub cwd: Option<PathBuf>,
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
    /// Inline persona prompt. Forwarded to `Transport::open`; later snapshotted
    /// into the session store and (for FirstTurn presets) injected as a first turn.
    pub system_prompt: Option<String>,
    /// Extra environment variables forwarded to the agent process via
    /// `Transport::open`. Callers that don't need custom env pass an empty map.
    pub extra_env: std::collections::HashMap<String, String>,
}

/// Owned by `SessionManager` (or directly by callers in single-session use).
pub struct SessionEngine {
    session_id: String,
    agent: String,
    cwd: PathBuf,
    /// Display label only; the daemon doesn't feed it back into the
    /// transport. `set_model` mutates it and rewrites the row in the
    /// session store.
    model: StdMutex<Option<String>>,
    resume_cursor: StdMutex<Option<String>>,
    journal: Arc<Journal>,
    broadcast_tx: broadcast::Sender<JournalEntry>,
    input_tx: mpsc::UnboundedSender<Cmd>,
    input_lease_held: StdMutex<bool>,
    /// Wall-clock of the most recent "activity" — either a journal append
    /// (`publish`) or an incoming prompt (`Cmd::Prompt` arriving at the
    /// actor). Used by `SessionManager::sweep_idle` to GC quiet sessions.
    last_activity: StdMutex<Instant>,
    /// Persistent row backing this engine: cursor + model updates flow
    /// through `update_cursor` / `update_model`; the initial row is written
    /// once in `spawn` (resume keeps the existing row untouched).
    session_store: Arc<SessionStore>,
}

enum Cmd {
    Prompt(String),
    /// Persona/system prompt injected as the first turn (FirstTurn presets).
    /// Journaled as `System { subtype: "persona" }` rather than `UserPrompt`.
    Persona(String),
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
        session_store: Arc<SessionStore>,
    ) -> Result<Arc<Self>> {
        let session_id = cfg
            .fixed_session_id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let journal =
            Arc::new(Journal::open(&opts.journal_dir, &session_id, opts.mem_capacity).await?);
        Self::start(
            transport,
            opts,
            session_id,
            journal,
            cfg,
            session_store,
            /* is_resume */ false,
        )
        .await
    }

    /// Resurrect a previously-closed session: open its existing journal in
    /// append mode and re-spawn the actor with the same id. The supplied
    /// `cfg.resume_cursor` is what the daemon retrieved from the session
    /// store and is forwarded to `Transport::open`. The existing row in the
    /// store is left untouched until the first turn updates its cursor.
    pub async fn resume(
        transport: Arc<dyn Transport>,
        opts: EngineOpts,
        session_id: String,
        cfg: SessionSpawnConfig,
        session_store: Arc<SessionStore>,
    ) -> Result<Arc<Self>> {
        let journal =
            Arc::new(Journal::resume(&opts.journal_dir, &session_id, opts.mem_capacity).await?);
        Self::start(
            transport,
            opts,
            session_id,
            journal,
            cfg,
            session_store,
            /* is_resume */ true,
        )
        .await
    }

    async fn start(
        transport: Arc<dyn Transport>,
        opts: EngineOpts,
        session_id: String,
        journal: Arc<Journal>,
        cfg: SessionSpawnConfig,
        session_store: Arc<SessionStore>,
        is_resume: bool,
    ) -> Result<Arc<Self>> {
        let (broadcast_tx, _) = broadcast::channel::<JournalEntry>(opts.broadcast_capacity);
        let (input_tx, input_rx) = mpsc::unbounded_channel();

        // The manager always resolves `cfg.cwd` (allocating an orphan dir when
        // the caller passed `None`) before handing the config to the engine.
        let cwd = cfg
            .cwd
            .clone()
            .expect("SessionManager must resolve cwd before SessionEngine::spawn/resume");

        let handle = transport
            .open(
                &session_id,
                cfg.resume_cursor.as_deref(),
                cwd.clone(),
                cfg.system_prompt.as_deref(),
                &cfg.extra_env,
            )
            .await?;
        let initial_cursor = handle.resume_cursor().or(cfg.resume_cursor.clone());

        let engine = Arc::new(Self {
            session_id: session_id.clone(),
            agent: cfg.agent.to_string(),
            cwd: cwd.clone(),
            model: StdMutex::new(cfg.model.clone()),
            resume_cursor: StdMutex::new(initial_cursor.clone()),
            journal,
            broadcast_tx,
            input_tx,
            input_lease_held: StdMutex::new(false),
            last_activity: StdMutex::new(Instant::now()),
            session_store,
        });

        // Spawn path: persist the initial session row so a daemon restart can
        // find this session. Resume path: the row already exists in the store
        // (that's how the daemon located the session) — leave it untouched
        // and let the first turn's cursor update flow through `persist_cursor`.
        // Errors on insert propagate: a half-spawned session without a row
        // would silently fail to resume across daemon restarts.
        if !is_resume {
            let row = SessionRow {
                session_id: session_id.clone(),
                agent: cfg.agent.to_string(),
                cwd: cwd.clone(),
                model: cfg.model.clone(),
                permission: cfg.permission.clone(),
                resume_cursor: initial_cursor.clone(),
                system_prompt: cfg.system_prompt.clone(),
                created_at: Utc::now().timestamp(),
                closed_at: None,
            };
            engine.session_store.insert(&row).await?;
        }

        // FirstTurn presets: the transport deferred the persona. Drain it now
        // (while we still own the handle) and enqueue it as the first command,
        // so it is injected as the first turn before any user prompt.
        let mut handle = handle;
        if let Some(persona) = handle.take_pending_persona() {
            // Unbounded channel; this send precedes any external prompt.
            let _ = engine.input_tx.send(Cmd::Persona(persona));
        }
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
        self.session_store
            .update_model(&self.session_id, Some(&model))
            .await?;
        // Per-connection `ServerEvent::ModelChanged` is only the ack to
        // the requester; this is what reaches every other attached
        // client in lock-step via `ServerEvent::Frame`.
        publish(
            self,
            TurnEvent::System {
                subtype: format!("model_changed:{model}"),
                text: None,
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

    /// Write the current `resume_cursor` to the session store. Called after
    /// every turn that yields a fresh cursor; cheap (`UPDATE` on a primary
    /// key) and safe to invoke even when the value didn't change.
    async fn persist_cursor(&self) -> Result<()> {
        let cursor = self.resume_cursor.lock().unwrap().clone();
        self.session_store
            .update_cursor(&self.session_id, cursor.as_deref())
            .await
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
        let (text, as_system) = match cmd {
            Cmd::Prompt(text) => (text, false),
            // Persona is the FirstTurn-preset system prompt, enqueued once
            // before any user prompt; journaled as System.
            Cmd::Persona(text) => (text, true),
            // Cancel outside an active turn is a no-op.
            Cmd::Cancel => continue,
            Cmd::Close => break,
        };
        // A `Close` (or channel hang-up) seen mid-turn is consumed inside
        // `drive_turn`; honour it here so the actor winds down instead of
        // blocking forever on the next `recv` (the engine holds its own
        // `input_tx`, so the channel never closes on its own).
        if run_one_turn(&engine, handle.as_mut(), &text, &mut input_rx, as_system).await {
            break;
        }
    }
    if let Err(e) = handle.close().await {
        tracing::warn!(
            session = %engine.session_id,
            error = %e,
            "transport close failed; child process may be left in unknown state",
        );
    }
}

/// Journal the prelude, drive one turn to completion, persist the cursor.
/// `as_system` journals the prelude as `System { subtype: "persona" }` instead
/// of `UserPrompt`. Returns `true` if a `Close` / channel hang-up was observed
/// mid-turn.
async fn run_one_turn(
    engine: &SessionEngine,
    handle: &mut dyn Handle,
    text: &str,
    input_rx: &mut mpsc::UnboundedReceiver<Cmd>,
    as_system: bool,
) -> bool {
    engine.touch_activity();
    // Journal the prelude before driving the turn. Agents don't echo user
    // input over ACP, so without this step a refresh / late attach can never
    // reconstruct the user side of the conversation. A persona turn is
    // journaled as System so the UI doesn't render it as a user message.
    let prelude = if as_system {
        // Keep the journal self-contained: include the persona body so a late
        // attach can reconstruct what the agent reacted to (it never echoes it).
        TurnEvent::System {
            subtype: "persona".to_string(),
            text: Some(text.to_string()),
        }
    } else {
        TurnEvent::UserPrompt {
            text: text.to_string(),
        }
    };
    if let Err(e) = publish(engine, prelude).await {
        tracing::error!(
            session = %engine.session_id,
            error = %e,
            "failed to journal turn prelude; turn still dispatched",
        );
    }
    let closed = drive_turn(engine, handle, text, input_rx).await;
    if let Some(cursor) = handle.resume_cursor() {
        let changed = {
            let mut guard = engine.resume_cursor.lock().unwrap();
            let differs = guard.as_ref() != Some(&cursor);
            if differs {
                *guard = Some(cursor);
            }
            differs
        };
        if changed {
            // Non-fatal: session keeps running, but a stale cursor on disk means
            // a future Resume reconnects to the wrong agent-side session.
            if let Err(e) = engine.persist_cursor().await {
                tracing::warn!(
                    session = %engine.session_id,
                    error = %e,
                    "failed to persist session cursor after turn",
                );
            }
        }
    }
    closed
}

/// Drive one turn to its terminal `Result`. Returns `true` if a `Close` (or
/// channel hang-up) arrived mid-turn — the actor must wind down rather than
/// loop back to `recv`, which would block forever.
async fn drive_turn(
    engine: &SessionEngine,
    handle: &mut dyn Handle,
    text: &str,
    input_rx: &mut mpsc::UnboundedReceiver<Cmd>,
) -> bool {
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
            return false;
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
                    tracing::warn!(
                        session = %engine.session_id,
                        "ignoring Cmd::Prompt during active turn",
                    );
                }
                Some(Cmd::Persona(_)) => {
                    // Persona is only enqueued before any turn starts, so this
                    // is unreachable in practice; warn rather than drop silently.
                    tracing::warn!(
                        session = %engine.session_id,
                        "ignoring Cmd::Persona during active turn",
                    );
                }
                Some(Cmd::Close) | None => return true,
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
    false
}

async fn publish(engine: &SessionEngine, event: TurnEvent) -> Result<JournalEntry> {
    let entry = engine.journal.append(event).await?;
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
