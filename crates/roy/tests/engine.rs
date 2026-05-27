use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use roy::engine::{EngineOpts, SessionEngine, SessionSpawnConfig};
use roy::event::{StopReason, TurnEvent};
use roy::session_store::SessionStore;
use roy::transport::{AcpConfig, AcpTransport, PermissionPolicy, Transport};

fn fake_acp_transport() -> Arc<dyn Transport> {
    fake_acp_transport_with(&[])
}

fn fake_acp_transport_with(extra_args: &[&str]) -> Arc<dyn Transport> {
    fake_acp_transport_with_channel(extra_args, roy::transport::SystemPromptChannel::Meta)
}

fn fake_acp_transport_first_turn() -> Arc<dyn Transport> {
    fake_acp_transport_with_channel(&[], roy::transport::SystemPromptChannel::FirstTurn)
}

fn fake_acp_transport_with_channel(
    extra_args: &[&str],
    channel: roy::transport::SystemPromptChannel,
) -> Arc<dyn Transport> {
    let mut args = vec!["tests/scripts/fake-acp-agent.py".to_string()];
    args.extend(extra_args.iter().map(|s| s.to_string()));
    Arc::new(AcpTransport::new(AcpConfig {
        command: "python3".to_string(),
        args,
        mode_id: Some("yolo".to_string()),
        permission_policy: PermissionPolicy::AllowAll,
        open_timeout: Duration::from_secs(5),
        env_remove: Vec::new(),
        system_prompt_channel: channel,
        connections: Vec::new(),
    }))
}

fn test_cfg() -> SessionSpawnConfig {
    SessionSpawnConfig {
        harness: roy::Harness::Opencode,
        cwd: Some(std::env::current_dir().unwrap()),
        model: None,
        permission: None,
        resume_cursor: None,
        fixed_session_id: None,
        system_prompt: None,
        extra_env: Default::default(),
        connections: Vec::new(),
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

async fn tmp_store() -> Arc<SessionStore> {
    let n = TMPDIR_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!(
        "roy-engine-test-store-{}-{n}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    Arc::new(SessionStore::open(&path).await.unwrap())
}

#[tokio::test]
async fn two_attaches_see_the_same_seq_stream_until_result() {
    let journal_dir = tmp_journal_dir();
    let engine = SessionEngine::spawn(
        fake_acp_transport(),
        opts(journal_dir.clone()),
        test_cfg(),
        tmp_store().await,
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
        tmp_store().await,
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
        tmp_store().await,
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

/// `cancel_turn` on an active prompt (held open by `--cancellable` fake) must
/// flow through `Cmd::Cancel` → transport `session/cancel` → terminal
/// `Result { Cancelled }` landing in the journal. Without this test the cancel
/// path added by the actor's mid-turn select arm is unobserved.
#[tokio::test]
async fn cancel_turn_yields_cancelled_result() {
    let journal_dir = tmp_journal_dir();
    let engine = SessionEngine::spawn(
        fake_acp_transport_with(&["--cancellable"]),
        opts(journal_dir.clone()),
        test_cfg(),
        tmp_store().await,
    )
    .await
    .unwrap();

    let lease = engine.try_acquire_input().expect("free lease");
    let attach = engine.attach(None).await.unwrap();

    // Send the prompt; the fake streams one "working" chunk then waits.
    lease.send("anything").unwrap();

    // Wait until the first chunk lands (proves the turn is active).
    let mut stream = attach.stream;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut saw_chunk = false;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, stream.next()).await {
            Ok(Some(entry)) => {
                if matches!(entry.event, TurnEvent::AssistantText { .. }) {
                    saw_chunk = true;
                    break;
                }
            }
            _ => break,
        }
    }
    assert!(saw_chunk, "fake should stream one chunk before cancel");

    // Now fire cancel. Terminal Result must follow with stop_reason: Cancelled.
    engine.cancel_turn().unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut got_cancelled = false;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, stream.next()).await {
            Ok(Some(entry)) => {
                if let TurnEvent::Result { stop_reason, .. } = &entry.event {
                    assert!(
                        matches!(stop_reason, StopReason::Cancelled),
                        "expected Cancelled, got {stop_reason:?}",
                    );
                    got_cancelled = true;
                    break;
                }
            }
            _ => break,
        }
    }
    assert!(got_cancelled, "cancel must produce a terminal Result");

    drop(lease);
    engine.close().unwrap();
    let _ = std::fs::remove_dir_all(&journal_dir);
}

#[tokio::test]
async fn wait_for_result_concatenates_many_chunks_then_result() {
    let dir = tmp_journal_dir();
    let mut engine_opts = opts(dir.clone());
    engine_opts.broadcast_capacity = 2;

    let transport = fake_acp_transport_with(&["--flood", "50"]);
    let engine = SessionEngine::spawn(transport, engine_opts, test_cfg(), tmp_store().await)
        .await
        .unwrap();

    let lease = engine.try_acquire_input().expect("free lease");
    lease.send("go").unwrap();
    drop(lease);

    let (seq, result, text) = engine
        .wait_for_result(0, Duration::from_secs(10))
        .await
        .unwrap()
        .expect("wait_for_result must recover from Lagged via journal re-scan");

    assert!(matches!(result, TurnEvent::Result { .. }));
    assert!(seq > 0);
    assert!(
        text.contains("flood-0"),
        "assistant_text must include flood prefix"
    );
    assert!(
        text.contains("ack"),
        "assistant_text must include final chunk"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn inject_note_appends_without_lease() {
    let journal_dir = tmp_journal_dir();
    let engine = SessionEngine::spawn(
        fake_acp_transport(),
        opts(journal_dir.clone()),
        test_cfg(),
        tmp_store().await,
    )
    .await
    .unwrap();

    // Hold the input lease, as an interactive client would.
    let _lease = engine.try_acquire_input().expect("first lease");

    // Inject still succeeds despite the held lease.
    let seq = engine
        .inject_note("background result".into(), Some("child-sid".into()))
        .await
        .expect("inject_note");

    let entries = engine.snapshot(seq).await.unwrap();
    let note = entries.iter().find(|e| e.seq == seq).expect("note entry");
    assert_eq!(
        note.event,
        TurnEvent::Note {
            text: "background result".into(),
            source_session: Some("child-sid".into()),
        }
    );

    engine.close().unwrap();
    let _ = std::fs::remove_dir_all(&journal_dir);
}

#[tokio::test]
async fn close_during_turn_winds_down_and_does_not_hang() {
    let journal_dir = tmp_journal_dir();
    let engine = SessionEngine::spawn(
        fake_acp_transport_with(&["--cancellable"]),
        opts(journal_dir.clone()),
        test_cfg(),
        tmp_store().await,
    )
    .await
    .unwrap();

    let lease = engine.try_acquire_input().expect("free lease");
    let attach = engine.attach(None).await.unwrap();
    lease.send("hold").unwrap();

    // Wait until the turn is genuinely active (cancellable fake streams one
    // chunk then waits) so the Close lands mid-turn.
    let mut stream = attach.stream;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut active = false;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, stream.next()).await {
            Ok(Some(entry)) => {
                if matches!(entry.event, TurnEvent::AssistantText { .. }) {
                    active = true;
                    break;
                }
            }
            _ => break,
        }
    }
    assert!(active, "turn should be active before close");

    // Close mid-turn. With the fix the actor breaks out of `drive_turn` and
    // out of the outer `recv` loop, so once it has exited the `input_rx`
    // receiver is dropped and any further `cancel_turn`/`close` send fails.
    // A hang (the bug) would leave the actor stuck on `input_rx.recv().await`,
    // keeping the receiver alive forever.
    drop(lease);
    engine.close().unwrap();
    drop(stream);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut wound_down = false;
    while tokio::time::Instant::now() < deadline {
        if engine.cancel_turn().is_err() {
            wound_down = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        wound_down,
        "actor must drop input_rx after Close; a timeout means it hung on recv"
    );

    let _ = std::fs::remove_dir_all(&journal_dir);
}

#[tokio::test]
async fn first_turn_persona_is_journaled_as_system() {
    let journal_dir = tmp_journal_dir();
    let mut cfg = test_cfg();
    cfg.system_prompt = Some("PERSONA".to_string());
    let engine = SessionEngine::spawn(
        fake_acp_transport_first_turn(),
        opts(journal_dir.clone()),
        cfg,
        tmp_store().await,
    )
    .await
    .unwrap();
    // The engine injects the persona as the first turn automatically. Wait for
    // that turn to reach a terminal Result so the journal is populated.
    let _ = engine
        .wait_for_result(0, Duration::from_secs(5))
        .await
        .unwrap();
    let entries = engine.snapshot(0).await.unwrap();
    assert!(
        matches!(
            entries.first().map(|e| &e.event),
            Some(TurnEvent::System { subtype, text })
                if subtype == "persona" && text.as_deref() == Some("PERSONA")
        ),
        "first journal entry should be the persona System marker carrying the persona body, got: {entries:?}"
    );
    // And it must NOT be journaled as a UserPrompt.
    assert!(
        !entries
            .iter()
            .any(|e| matches!(&e.event, TurnEvent::UserPrompt { text } if text == "PERSONA")),
        "persona must not appear as a UserPrompt"
    );
    engine.close().unwrap();
    let _ = std::fs::remove_dir_all(&journal_dir);
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
