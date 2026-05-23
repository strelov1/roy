use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use roy::engine::{EngineOpts, SessionEngine};
use roy::event::{StopReason, TurnEvent};
use roy::provider::Provider;
use roy::transport::{PrintTransport, Transport};

/// Provider that runs the same shell fake-agent the existing PrintTransport
/// tests use. Re-encodes claude's stream-json shape for line parsing.
struct FakeProvider;

impl Provider for FakeProvider {
    fn command(&self) -> &str {
        "tests/scripts/fake-agent.sh"
    }
    fn spawn_args(&self, _: &str, _: Option<&str>) -> Vec<String> {
        vec![]
    }
    fn encode_user_message(&self, text: &str) -> String {
        format!("{text}\n")
    }
    fn parse_line(&self, line: &str) -> Option<TurnEvent> {
        roy::provider::ClaudeProvider::new(None).parse_line(line)
    }
    fn is_turn_end(&self, ev: &TurnEvent) -> bool {
        matches!(ev, TurnEvent::Result { .. })
    }
}

fn tmp_journal_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "roy-engine-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[tokio::test]
async fn two_attaches_see_the_same_seq_stream_until_result() {
    let journal_dir = tmp_journal_dir();
    let transport: Arc<dyn Transport> = Arc::new(PrintTransport::new(Arc::new(FakeProvider)));
    let engine = SessionEngine::spawn(
        transport,
        std::env::current_dir().unwrap(),
        EngineOpts::with_journal_dir(journal_dir.clone()),
    )
    .await
    .unwrap();

    // Attach two observers BEFORE sending the prompt. Both should see the
    // entire turn from seq 0.
    let attach_a = engine.attach(None).await.unwrap();
    let attach_b = engine.attach(None).await.unwrap();
    assert_eq!(attach_a.seq_at_attach, 0);
    assert_eq!(attach_b.seq_at_attach, 0);

    let lease = engine.try_acquire_input().expect("free lease");
    lease.send("hello").unwrap();

    let collect_until_result = |mut stream: std::pin::Pin<
        Box<dyn futures::Stream<Item = roy::JournalEntry> + Send>,
    >| async move {
        let mut acc = Vec::new();
        while let Some(entry) = stream.next().await {
            let is_end = matches!(entry.event, TurnEvent::Result { .. });
            acc.push(entry);
            if is_end {
                break;
            }
        }
        acc
    };

    let a_events = collect_until_result(attach_a.stream).await;
    let b_events = collect_until_result(attach_b.stream).await;

    assert!(!a_events.is_empty(), "A saw no events");
    assert_eq!(
        a_events, b_events,
        "both observers must see identical seq streams"
    );

    // Last event is the terminal Result with EndTurn.
    let last = a_events.last().unwrap();
    assert!(matches!(
        last.event,
        TurnEvent::Result {
            stop_reason: StopReason::EndTurn,
            ..
        }
    ));

    // Seqs are monotonic from 0.
    for (i, entry) in a_events.iter().enumerate() {
        assert_eq!(entry.seq, i as u64);
    }

    // Cleanup.
    drop(lease);
    engine.close().unwrap();
    let _ = std::fs::remove_dir_all(&journal_dir);
}

#[tokio::test]
async fn input_lease_is_exclusive_and_released_on_drop() {
    let journal_dir = tmp_journal_dir();
    let transport: Arc<dyn Transport> = Arc::new(PrintTransport::new(Arc::new(FakeProvider)));
    let engine = SessionEngine::spawn(
        transport,
        std::env::current_dir().unwrap(),
        EngineOpts::with_journal_dir(journal_dir.clone()),
    )
    .await
    .unwrap();

    let lease = engine.try_acquire_input().expect("first acquire");
    assert!(engine.try_acquire_input().is_none(), "second acquire must fail");
    drop(lease);
    assert!(
        engine.try_acquire_input().is_some(),
        "lease must be reacquirable after drop"
    );

    engine.close().unwrap();
    let _ = std::fs::remove_dir_all(&journal_dir);
}

#[tokio::test]
async fn late_attach_replays_full_journal() {
    let journal_dir = tmp_journal_dir();
    let transport: Arc<dyn Transport> = Arc::new(PrintTransport::new(Arc::new(FakeProvider)));
    let engine = SessionEngine::spawn(
        transport,
        std::env::current_dir().unwrap(),
        EngineOpts::with_journal_dir(journal_dir.clone()),
    )
    .await
    .unwrap();

    // Run a turn with no observers.
    let lease = engine.try_acquire_input().unwrap();
    lease.send("hi").unwrap();

    // Poll until the journal has a terminal Result entry; bounded wait so the
    // test fails fast on regression instead of hanging.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let replay = engine.attach(None).await.unwrap();
        let entries: Vec<_> = collect_all(replay.stream).await;
        if let Some(last) = entries.last() {
            if matches!(last.event, TurnEvent::Result { .. }) {
                assert!(!entries.is_empty());
                for (i, e) in entries.iter().enumerate() {
                    assert_eq!(e.seq, i as u64);
                }
                drop(lease);
                engine.close().unwrap();
                let _ = std::fs::remove_dir_all(&journal_dir);
                return;
            }
        }
        if std::time::Instant::now() > deadline {
            panic!("turn never completed within 5s");
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

async fn collect_all(
    mut stream: std::pin::Pin<Box<dyn futures::Stream<Item = roy::JournalEntry> + Send>>,
) -> Vec<roy::JournalEntry> {
    let mut acc = Vec::new();
    // Read until terminal Result; don't block on broadcast Closed.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(500);
    loop {
        let next =
            tokio::time::timeout_at(deadline, stream.next()).await;
        match next {
            Ok(Some(entry)) => {
                let end = matches!(entry.event, TurnEvent::Result { .. });
                acc.push(entry);
                if end {
                    break;
                }
            }
            _ => break,
        }
    }
    acc
}
