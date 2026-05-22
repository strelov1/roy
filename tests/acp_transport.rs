use std::sync::Arc;

use futures::StreamExt;
use roy::event::TurnEvent;
use roy::session::Session;
use roy::transport::{AcpConfig, AcpTransport, Transport};

fn fake_config(extra: &[&str]) -> AcpConfig {
    let mut args = vec!["tests/scripts/fake-acp-agent.py".to_string()];
    args.extend(extra.iter().map(|s| s.to_string()));
    AcpConfig {
        command: "python3".to_string(),
        args,
        mode_id: Some("yolo".to_string()),
    }
}

#[tokio::test]
async fn open_send_streams_until_result() {
    let transport = AcpTransport::new(fake_config(&[]));
    let mut handle = transport
        .open("ignored", None, std::env::current_dir().unwrap())
        .await
        .unwrap();

    let mut events = Vec::new();
    {
        let mut stream = handle.send("hello").await.unwrap();
        while let Some(ev) = stream.next().await {
            events.push(ev);
        }
    }
    assert!(events.iter().any(|e| matches!(e, TurnEvent::AssistantText { text } if text == "ack")));
    assert!(matches!(events.last(), Some(TurnEvent::Result { is_error: false, .. })));

    // ACP sessionId is exposed as the resume cursor.
    assert_eq!(handle.resume_cursor(), Some("fake-acp-sid".to_string()));

    // Multi-turn on the same live process.
    let mut events2 = Vec::new();
    {
        let mut stream = handle.send("again").await.unwrap();
        while let Some(ev) = stream.next().await {
            events2.push(ev);
        }
    }
    assert!(matches!(events2.last(), Some(TurnEvent::Result { .. })));

    handle.close().await.unwrap();
}

#[tokio::test]
async fn auto_allows_permission_requests() {
    let transport = AcpTransport::new(fake_config(&["--permission"]));
    let mut handle = transport
        .open("ignored", None, std::env::current_dir().unwrap())
        .await
        .unwrap();

    // The fake agent only completes the turn after the client auto-allows the
    // permission request; reaching Result proves the auto-allow happened.
    let mut events = Vec::new();
    {
        let mut stream = handle.send("do a thing").await.unwrap();
        while let Some(ev) = stream.next().await {
            events.push(ev);
        }
    }
    assert!(matches!(events.last(), Some(TurnEvent::Result { .. })));
    handle.close().await.unwrap();
}

#[tokio::test]
async fn session_via_transport_records_acp_cursor() {
    let transport: Arc<dyn Transport> = Arc::new(AcpTransport::new(fake_config(&[])));
    let mut session = Session::new(transport, std::env::current_dir().unwrap());

    {
        let mut stream = session.send("hi").await.unwrap();
        while stream.next().await.is_some() {}
    }
    // Session stored the ACP sessionId (NOT its own uuid) as the resume cursor.
    assert_eq!(session.resume_cursor(), Some("fake-acp-sid"));
    session.close().await.unwrap();
}
