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
