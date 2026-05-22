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

use roy::session::Session;

#[tokio::test]
async fn session_send_sets_resume_cursor() {
    let provider: Arc<dyn Provider> = Arc::new(FakeProvider);
    let transport: Arc<dyn roy::transport::Transport> = Arc::new(PrintTransport::new());
    let mut session = Session::new(provider, transport, std::env::current_dir().unwrap());

    assert!(session.resume_cursor().is_none());

    let mut events = Vec::new();
    {
        let mut stream = session.send("hi").await.unwrap();
        while let Some(ev) = stream.next().await {
            events.push(ev);
        }
    }
    assert!(matches!(events.last(), Some(TurnEvent::Result { .. })));
    // After the first turn the session can be resumed by its own id.
    assert_eq!(session.resume_cursor(), Some(session.id().to_string()).as_deref());

    session.close().await.unwrap();
}

#[tokio::test]
async fn resume_existing_session_keeps_id_and_cursor() {
    let provider: Arc<dyn Provider> = Arc::new(FakeProvider);
    let transport: Arc<dyn roy::transport::Transport> = Arc::new(PrintTransport::new());
    // Re-open a session that already exists on disk (e.g. after the host
    // app restarted). The id is the previously-issued one.
    let mut session = Session::resume(
        provider,
        transport,
        std::env::current_dir().unwrap(),
        "prior-session-id".to_string(),
    );
    assert_eq!(session.id(), "prior-session-id");
    assert_eq!(session.resume_cursor(), Some("prior-session-id"));

    let mut events = Vec::new();
    {
        let mut stream = session.send("continue").await.unwrap();
        while let Some(ev) = stream.next().await {
            events.push(ev);
        }
    }
    assert!(matches!(events.last(), Some(TurnEvent::Result { .. })));
    session.close().await.unwrap();
}

// Real claude. Ignored by default: needs the `claude` binary and
// CLAUDE_CODE_OAUTH_TOKEN. Run with: cargo test -- --ignored real_claude
#[tokio::test]
#[ignore]
async fn real_claude_spawn_and_turn() {
    if std::env::var("CLAUDE_CODE_OAUTH_TOKEN").is_err() {
        eprintln!("skipping: CLAUDE_CODE_OAUTH_TOKEN not set");
        return;
    }
    let provider: Arc<dyn Provider> =
        Arc::new(roy::provider::ClaudeProvider::new(Some("claude-haiku-4-5-20251001".into())));
    let transport: Arc<dyn roy::transport::Transport> = Arc::new(PrintTransport::new());
    let mut session = Session::new(provider, transport, std::env::current_dir().unwrap());

    let mut answer = String::new();
    let mut stream = session.send("reply with exactly: hello").await.unwrap();
    while let Some(ev) = stream.next().await {
        if let TurnEvent::AssistantText { text } = ev {
            answer = text;
        }
    }
    drop(stream);
    assert_eq!(answer.trim(), "hello");
    session.close().await.unwrap();
}
