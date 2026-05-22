use roy::event::TurnEvent;
use roy::provider::Provider;
use roy::transport::{PrintTransport, Transport};
use std::sync::Arc;

/// Provider whose "CLI" is the fake-agent shell script. Uses the same
/// stream-json line shapes as ClaudeProvider so we reuse claude parsing.
struct FakeProvider;

impl Provider for FakeProvider {
    fn command(&self) -> &str {
        "tests/scripts/fake-agent.sh"
    }
    fn spawn_args(&self, _session_id: &str, _resume_cursor: Option<&str>) -> Vec<String> {
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

#[tokio::test]
async fn open_spawns_process() {
    let transport = PrintTransport::new();
    let provider: Arc<dyn Provider> = Arc::new(FakeProvider);
    let handle = transport
        .open(provider, "fake-session", None, std::env::current_dir().unwrap())
        .await
        .expect("open should spawn the fake agent");
    drop(handle);
}

use futures::StreamExt;

#[tokio::test]
async fn send_streams_until_turn_end() {
    let transport = PrintTransport::new();
    let provider: Arc<dyn Provider> = Arc::new(FakeProvider);
    let mut handle = transport
        .open(provider, "fake-session", None, std::env::current_dir().unwrap())
        .await
        .unwrap();

    // Turn 1
    let mut events = Vec::new();
    {
        let mut stream = handle.send("hello").await.unwrap();
        while let Some(ev) = stream.next().await {
            events.push(ev);
        }
    }
    assert!(events.iter().any(|e| matches!(e, TurnEvent::AssistantText { text } if text == "ack")));
    assert!(matches!(events.last(), Some(TurnEvent::Result { .. })));

    // Turn 2 on the SAME live process (multi-turn)
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
