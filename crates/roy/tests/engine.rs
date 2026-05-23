use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use roy::engine::{EngineOpts, SessionEngine, SessionSpawnConfig};
use roy::event::{StopReason, TurnEvent};
use roy::transport::{AcpConfig, AcpTransport, PermissionPolicy, Transport};

fn fake_acp_transport() -> Arc<dyn Transport> {
    Arc::new(AcpTransport::new(AcpConfig {
        command: "python3".to_string(),
        args: vec!["tests/scripts/fake-acp-agent.py".to_string()],
        mode_id: Some("yolo".to_string()),
        permission_policy: PermissionPolicy::AllowAll,
        open_timeout: Duration::from_secs(5),
    }))
}

fn test_cfg() -> SessionSpawnConfig {
    SessionSpawnConfig {
        agent: "test".into(),
        cwd: std::env::current_dir().unwrap(),
        model: None,
        permission: None,
        resume_cursor: None,
    }
}

static TMPDIR_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn tmp_journal_dir() -> PathBuf {
    let n = TMPDIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    std::env::temp_dir().join(format!("roy-engine-test-{}-{n}", std::process::id()))
}

fn opts(journal_dir: PathBuf) -> EngineOpts {
    EngineOpts {
        journal_dir,
        broadcast_capacity: 256,
        mem_capacity: 1024,
    }
}

#[tokio::test]
async fn two_attaches_see_the_same_seq_stream_until_result() {
    let journal_dir = tmp_journal_dir();
    let engine = SessionEngine::spawn(
        fake_acp_transport(),
        opts(journal_dir.clone()),
        test_cfg(),
    )
    .await
    .unwrap();

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

    let last = a_events.last().unwrap();
    assert!(matches!(
        last.event,
        TurnEvent::Result {
            stop_reason: StopReason::EndTurn,
            ..
        }
    ));

    for (i, entry) in a_events.iter().enumerate() {
        assert_eq!(entry.seq, i as u64);
    }

    drop(lease);
    engine.close().unwrap();
    let _ = std::fs::remove_dir_all(&journal_dir);
}

#[tokio::test]
async fn input_lease_is_exclusive_and_released_on_drop() {
    let journal_dir = tmp_journal_dir();
    let engine = SessionEngine::spawn(
        fake_acp_transport(),
        opts(journal_dir.clone()),
        test_cfg(),
    )
    .await
    .unwrap();

    let lease = engine.try_acquire_input().expect("first acquire");
    assert!(
        engine.try_acquire_input().is_none(),
        "second acquire must fail"
    );
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
    let engine = SessionEngine::spawn(
        fake_acp_transport(),
        opts(journal_dir.clone()),
        test_cfg(),
    )
    .await
    .unwrap();

    let lease = engine.try_acquire_input().unwrap();
    lease.send("hi").unwrap();

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
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(500);
    loop {
        let next = tokio::time::timeout_at(deadline, stream.next()).await;
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
