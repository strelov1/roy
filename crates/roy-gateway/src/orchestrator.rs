//! Streaming pipeline that turns one inbound chat message into a series of
//! throttled Telegram edits as the agent produces `TurnEvent`s.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use roy_protocol::event::TurnEvent;
use tokio_util::sync::CancellationToken;

use crate::binder::SessionBinder;
use crate::cancel::CancelRegistry;
use crate::daemon::{Conn, ConnFactory};
use crate::draft_stream::{DraftReplier, DraftStream};
use crate::formatting::Renderer;
use crate::typing::{TypingKeepalive, TypingReplier};

/// Combined trait for everything `handle_message` needs from a chat replier.
#[async_trait]
pub trait Replier: DraftReplier + TypingReplier {
    // Marker trait — all behavior is on the supertraits.
}

#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    pub harness: String,
    pub cwd: Option<PathBuf>,
    pub turn_timeout: Duration,
    pub typing_interval: Duration,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            harness: "claude".into(),
            cwd: None,
            turn_timeout: Duration::from_secs(600),
            typing_interval: Duration::from_secs(4),
        }
    }
}

pub async fn handle_message<F, R>(
    cfg: &OrchestratorConfig,
    binder: &SessionBinder,
    cancel_registry: &CancelRegistry,
    conn_factory: &F,
    replier: &Arc<R>,
    chat_id: i64,
    prompt: String,
) -> Result<()>
where
    F: ConnFactory,
    R: Replier + 'static,
{
    let token = cancel_registry.register(chat_id).await;
    let result = run_turn(cfg, binder, &token, conn_factory, replier, chat_id, prompt).await;
    cancel_registry.release(chat_id).await;
    result
}

async fn run_turn<F, R>(
    cfg: &OrchestratorConfig,
    binder: &SessionBinder,
    token: &CancellationToken,
    conn_factory: &F,
    replier: &Arc<R>,
    chat_id: i64,
    prompt: String,
) -> Result<()>
where
    F: ConnFactory,
    R: Replier + 'static,
{
    let placeholder_id = replier.send(chat_id, "⏳").await?;
    let typing = TypingKeepalive::start(replier.clone(), chat_id, cfg.typing_interval);
    let draft = DraftStream::new(replier.clone(), chat_id, placeholder_id);

    let outcome = drive_turn(cfg, binder, token, conn_factory, &draft, chat_id, prompt).await;

    if let Err(ref e) = outcome {
        let _ = draft.update(format!("⚠ {e}")).await;
    }

    typing.stop();
    let _ = draft.flush().await;
    outcome
}

async fn drive_turn<F, R>(
    cfg: &OrchestratorConfig,
    binder: &SessionBinder,
    token: &CancellationToken,
    conn_factory: &F,
    draft: &DraftStream<R>,
    chat_id: i64,
    prompt: String,
) -> Result<()>
where
    F: ConnFactory,
    R: Replier + 'static,
{
    let mut conn = conn_factory.open().await?;

    let session_id = match binder.get(chat_id).await {
        Some(sid) => conn.resume(&sid).await,
        None => conn.spawn(&cfg.harness, cfg.cwd.clone()).await,
    };
    let session_id = match session_id {
        Ok(s) => s,
        Err(e) => {
            draft.update(format!("⚠ {e}")).await?;
            return Ok(());
        }
    };

    binder.set(chat_id, session_id.clone()).await?;

    if let Err(e) = conn.acquire_input(&session_id).await {
        draft.update(format!("⚠ {e}")).await?;
        return Ok(());
    }
    if let Err(e) = conn.send_prompt(&session_id, prompt).await {
        draft.update(format!("⚠ {e}")).await?;
        let _ = conn.release_input(&session_id).await;
        return Ok(());
    }

    let mut renderer = Renderer::new();
    let elapsed = tokio::time::timeout(
        cfg.turn_timeout,
        consume_frames(&mut conn, token, draft, &mut renderer, &session_id),
    )
    .await;

    if token.is_cancelled() {
        renderer.append_error_footer("cancelled by user");
        draft.update(renderer.body()).await?;
    } else if elapsed.is_err() {
        // Turn ran past the timeout without producing a terminal Result.
        // Best-effort cancel and footer.
        let _ = conn.cancel_turn(&session_id).await;
        renderer.append_error_footer("turn timed out");
        draft.update(renderer.body()).await?;
    } else if let Ok(Err(_)) = &elapsed {
        renderer.append_error_footer("connection lost");
        let _ = draft.update(renderer.body()).await;
    }

    let _ = conn.release_input(&session_id).await;
    Ok(())
}

async fn consume_frames<R: Replier + 'static>(
    conn: &mut impl Conn,
    token: &CancellationToken,
    draft: &DraftStream<R>,
    renderer: &mut Renderer,
    session_id: &str,
) -> Result<()> {
    loop {
        tokio::select! {
            _ = token.cancelled() => {
                let _ = conn.cancel_turn(session_id).await;
                while let Ok(Some(event)) = conn.next_frame().await {
                    if matches!(event, TurnEvent::Result { .. }) {
                        break;
                    }
                }
                return Ok(());
            }
            frame = conn.next_frame() => {
                match frame? {
                    None => return Ok(()),
                    Some(TurnEvent::Result { stop_reason, .. }) => {
                        if stop_reason.is_error() {
                            renderer.append_error_footer(&format!("{stop_reason:?}"));
                            draft.update(renderer.body()).await?;
                        }
                        return Ok(());
                    }
                    Some(event) => {
                        renderer.feed(event);
                        draft.update(renderer.body()).await?;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draft_stream::DraftReplier;
    use crate::typing::TypingReplier;
    use roy_protocol::event::StopReason;
    use std::sync::Mutex as StdMutex;
    use tempfile::TempDir;
    use tokio::sync::Mutex as TokioMutex;

    #[derive(Default)]
    struct MockReplier {
        sent: TokioMutex<Vec<(i64, String)>>,
        edits: TokioMutex<Vec<(i64, i32, String)>>,
        next_id: StdMutex<i32>,
        typing_count: std::sync::atomic::AtomicUsize,
    }

    impl MockReplier {
        fn new() -> Self {
            Self {
                next_id: StdMutex::new(100),
                ..Default::default()
            }
        }
    }

    #[async_trait]
    impl DraftReplier for MockReplier {
        async fn send(&self, chat_id: i64, html: &str) -> Result<i32> {
            self.sent.lock().await.push((chat_id, html.into()));
            let mut id = self.next_id.lock().unwrap();
            *id += 1;
            Ok(*id)
        }
        async fn edit(&self, chat_id: i64, message_id: i32, html: &str) -> Result<()> {
            self.edits
                .lock()
                .await
                .push((chat_id, message_id, html.into()));
            Ok(())
        }
    }

    #[async_trait]
    impl TypingReplier for MockReplier {
        async fn typing(&self, _chat_id: i64) -> Result<()> {
            self.typing_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    impl Replier for MockReplier {}

    struct MockConn {
        script: StdMutex<Vec<MockStep>>,
    }

    #[derive(Debug)]
    enum MockStep {
        SpawnReturns(String),
        AcquireOk,
        SendOk,
        Frame(TurnEvent),
        /// Simulate `next_frame` returning an `Err` (daemon disconnect, etc.).
        FrameErr,
        /// Block `next_frame` for this duration before returning the next step.
        BlockFor(Duration),
        ReleaseOk,
    }

    impl MockConn {
        fn new(script: Vec<MockStep>) -> Self {
            Self {
                script: StdMutex::new(script.into_iter().rev().collect()),
            }
        }
        fn pop(&self) -> Option<MockStep> {
            self.script.lock().unwrap().pop()
        }
    }

    #[async_trait]
    impl Conn for MockConn {
        async fn spawn(&mut self, _harness: &str, _cwd: Option<PathBuf>) -> Result<String> {
            match self.pop() {
                Some(MockStep::SpawnReturns(s)) => Ok(s),
                other => panic!("unexpected spawn call, next step was {other:?}"),
            }
        }
        async fn resume(&mut self, _session_id: &str) -> Result<String> {
            match self.pop() {
                Some(MockStep::SpawnReturns(s)) => Ok(s),
                other => panic!("unexpected resume call, next step was {other:?}"),
            }
        }
        async fn acquire_input(&mut self, _session: &str) -> Result<()> {
            assert!(matches!(self.pop(), Some(MockStep::AcquireOk)));
            Ok(())
        }
        async fn send_prompt(&mut self, _session: &str, _text: String) -> Result<()> {
            assert!(matches!(self.pop(), Some(MockStep::SendOk)));
            Ok(())
        }
        async fn next_frame(&mut self) -> Result<Option<TurnEvent>> {
            // Consume any leading BlockFor steps first.
            loop {
                match self.pop() {
                    Some(MockStep::BlockFor(d)) => {
                        tokio::time::sleep(d).await;
                    }
                    Some(MockStep::Frame(e)) => return Ok(Some(e)),
                    Some(MockStep::FrameErr) => {
                        return Err(anyhow::anyhow!("simulated frame stream error"))
                    }
                    None => return Ok(None),
                    other => panic!("unexpected next_frame, next step was {other:?}"),
                }
            }
        }
        async fn cancel_turn(&mut self, _session: &str) -> Result<()> {
            Ok(())
        }
        async fn release_input(&mut self, _session: &str) -> Result<()> {
            assert!(matches!(self.pop(), Some(MockStep::ReleaseOk)));
            Ok(())
        }
    }

    struct MockConnFactory {
        steps: StdMutex<Option<Vec<MockStep>>>,
    }

    impl MockConnFactory {
        fn new(steps: Vec<MockStep>) -> Self {
            Self {
                steps: StdMutex::new(Some(steps)),
            }
        }
    }

    #[async_trait]
    impl ConnFactory for MockConnFactory {
        type Conn = MockConn;
        async fn open(&self) -> Result<MockConn> {
            let steps = self
                .steps
                .lock()
                .unwrap()
                .take()
                .expect("factory open called more than once");
            Ok(MockConn::new(steps))
        }
    }

    async fn fresh_binder(dir: &TempDir) -> SessionBinder {
        SessionBinder::load(dir.path().join("b.json"))
            .await
            .unwrap()
    }

    fn cfg() -> OrchestratorConfig {
        OrchestratorConfig {
            harness: "claude".into(),
            cwd: None,
            turn_timeout: Duration::from_secs(60),
            typing_interval: Duration::from_secs(60),
        }
    }

    #[tokio::test]
    async fn unbound_chat_spawns_streams_and_replies() {
        let dir = tempfile::tempdir().unwrap();
        let binder = fresh_binder(&dir).await;
        let registry = CancelRegistry::new();
        let factory = MockConnFactory::new(vec![
            MockStep::SpawnReturns("sess-new".into()),
            MockStep::AcquireOk,
            MockStep::SendOk,
            MockStep::Frame(TurnEvent::AssistantText {
                text: "Hello!".into(),
            }),
            MockStep::Frame(TurnEvent::Result {
                cost_usd: None,
                stop_reason: StopReason::EndTurn,
            }),
            MockStep::ReleaseOk,
        ]);
        let replier = Arc::new(MockReplier::new());

        handle_message(
            &cfg(),
            &binder,
            &registry,
            &factory,
            &replier,
            42,
            "hi".into(),
        )
        .await
        .unwrap();

        assert_eq!(binder.get(42).await.as_deref(), Some("sess-new"));
        let sent = replier.sent.lock().await.clone();
        assert_eq!(sent, vec![(42, "⏳".into())]);
        let edits = replier.edits.lock().await.clone();
        assert!(!edits.is_empty());
        assert!(edits.last().unwrap().2.contains("Hello!"));
    }

    #[tokio::test]
    async fn bound_chat_resumes() {
        let dir = tempfile::tempdir().unwrap();
        let binder = fresh_binder(&dir).await;
        binder.set(42, "sess-old".into()).await.unwrap();
        let registry = CancelRegistry::new();
        let factory = MockConnFactory::new(vec![
            MockStep::SpawnReturns("sess-old".into()),
            MockStep::AcquireOk,
            MockStep::SendOk,
            MockStep::Frame(TurnEvent::AssistantText { text: "ok".into() }),
            MockStep::Frame(TurnEvent::Result {
                cost_usd: None,
                stop_reason: StopReason::EndTurn,
            }),
            MockStep::ReleaseOk,
        ]);
        let replier = Arc::new(MockReplier::new());

        handle_message(
            &cfg(),
            &binder,
            &registry,
            &factory,
            &replier,
            42,
            "again".into(),
        )
        .await
        .unwrap();

        let edits = replier.edits.lock().await.clone();
        assert!(edits.last().unwrap().2.contains("ok"));
    }

    #[tokio::test]
    async fn cancel_via_registry_causes_cancel_turn_and_footer() {
        let dir = tempfile::tempdir().unwrap();
        let binder = fresh_binder(&dir).await;
        let registry = CancelRegistry::new();
        // BlockFor(200ms) makes next_frame block so the cancel signal (fired
        // after 50ms) arrives while the frame loop is waiting.
        let factory = MockConnFactory::new(vec![
            MockStep::SpawnReturns("s".into()),
            MockStep::AcquireOk,
            MockStep::SendOk,
            MockStep::BlockFor(Duration::from_millis(200)),
            // These frames are drained after cancel_turn is called.
            MockStep::Frame(TurnEvent::AssistantText {
                text: "partial".into(),
            }),
            MockStep::Frame(TurnEvent::Result {
                cost_usd: None,
                stop_reason: StopReason::EndTurn,
            }),
            MockStep::ReleaseOk,
        ]);
        let replier = Arc::new(MockReplier::new());

        let registry_clone = registry.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            registry_clone.signal(42).await;
        });

        handle_message(
            &cfg(),
            &binder,
            &registry,
            &factory,
            &replier,
            42,
            "x".into(),
        )
        .await
        .unwrap();

        let edits = replier.edits.lock().await.clone();
        assert!(edits
            .iter()
            .any(|(_, _, html)| html.contains("cancelled by user")));
    }

    #[tokio::test]
    async fn turn_timeout_appends_footer() {
        let dir = tempfile::tempdir().unwrap();
        let binder = fresh_binder(&dir).await;
        let registry = CancelRegistry::new();
        // Script: spawn ok, acquire ok, send ok, then a BlockFor longer than
        // the configured turn_timeout. No terminal Result will arrive in time.
        let factory = MockConnFactory::new(vec![
            MockStep::SpawnReturns("s".into()),
            MockStep::AcquireOk,
            MockStep::SendOk,
            MockStep::BlockFor(Duration::from_millis(500)),
            // After the block, even if next_frame is called again, mock has no
            // more steps and would panic — but the timeout-then-release path
            // should not call next_frame again.
            MockStep::ReleaseOk,
        ]);
        let replier = Arc::new(MockReplier::new());

        let cfg_with_short_timeout = OrchestratorConfig {
            harness: "claude".into(),
            cwd: None,
            turn_timeout: Duration::from_millis(100),
            typing_interval: Duration::from_secs(60),
        };

        handle_message(
            &cfg_with_short_timeout,
            &binder,
            &registry,
            &factory,
            &replier,
            42,
            "x".into(),
        )
        .await
        .unwrap();

        let edits = replier.edits.lock().await.clone();
        assert!(
            edits.iter().any(|(_, _, html)| html.contains("timed out")),
            "expected a 'timed out' footer, got: {:?}",
            edits.iter().map(|(_, _, h)| h.clone()).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn spawn_failure_reported_no_binder_write() {
        let dir = tempfile::tempdir().unwrap();
        let binder = fresh_binder(&dir).await;
        let registry = CancelRegistry::new();
        struct ErrOpenFactory;
        #[async_trait]
        impl ConnFactory for ErrOpenFactory {
            type Conn = MockConn;
            async fn open(&self) -> Result<MockConn> {
                Err(anyhow::anyhow!("daemon down"))
            }
        }
        let factory = ErrOpenFactory;
        let replier = Arc::new(MockReplier::new());

        let _ = handle_message(
            &cfg(),
            &binder,
            &registry,
            &factory,
            &replier,
            42,
            "x".into(),
        )
        .await;

        assert!(binder.get(42).await.is_none());
        let sent = replier.sent.lock().await.clone();
        assert_eq!(sent, vec![(42, "⏳".into())]);
        let edits = replier.edits.lock().await.clone();
        assert!(
            edits
                .iter()
                .any(|(_, _, html)| html.contains("daemon down")),
            "expected zombie placeholder to be updated with ⚠ footer, got: {:?}",
            edits.iter().map(|(_, _, h)| h.clone()).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn frame_stream_error_appends_connection_lost_footer() {
        let dir = tempfile::tempdir().unwrap();
        let binder = fresh_binder(&dir).await;
        let registry = CancelRegistry::new();
        let factory = MockConnFactory::new(vec![
            MockStep::SpawnReturns("s".into()),
            MockStep::AcquireOk,
            MockStep::SendOk,
            MockStep::Frame(TurnEvent::AssistantText {
                text: "partial".into(),
            }),
            MockStep::FrameErr,
            MockStep::ReleaseOk,
        ]);
        let replier = Arc::new(MockReplier::new());

        handle_message(
            &cfg(),
            &binder,
            &registry,
            &factory,
            &replier,
            42,
            "x".into(),
        )
        .await
        .unwrap();

        let edits = replier.edits.lock().await.clone();
        assert!(
            edits
                .iter()
                .any(|(_, _, html)| html.contains("connection lost")),
            "expected connection-lost footer, got: {:?}",
            edits.iter().map(|(_, _, h)| h.clone()).collect::<Vec<_>>()
        );
    }
}
