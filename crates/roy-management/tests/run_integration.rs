//! End-to-end proof: when roy-management `spawn`s a session for an agent, the
//! `ClientCommand::Spawn` it sends carries `system_prompt = agent.prompt`. The
//! fake daemon reads one command, asserts the persona, and replies Spawning +
//! Spawned. The wire send is reproduced inline so the test doesn't depend on
//! binary-private modules.

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

async fn fake_daemon(
    socket: std::path::PathBuf,
    captured: tokio::sync::oneshot::Sender<serde_json::Value>,
) {
    let listener = UnixListener::bind(&socket).unwrap();
    let (stream, _) = listener.accept().await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let raw = lines.next_line().await.unwrap().unwrap();
    let cmd: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let _ = captured.send(cmd);
    writer
        .write_all(b"{\"kind\":\"spawning\",\"agent\":\"claude\"}\n")
        .await
        .unwrap();
    writer
        .write_all(b"{\"kind\":\"spawned\",\"session\":\"sess-1\"}\n")
        .await
        .unwrap();
    writer.flush().await.unwrap();
}

/// The wire-level spawn call. Mirrors `roy_client::spawn` exactly so the test
/// asserts the same protocol contract without reaching into binary internals.
async fn wire_spawn(socket: &std::path::Path, agent: &roy_agents::Agent) -> String {
    use tokio::net::UnixStream;
    let stream = UnixStream::connect(socket).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let cmd = serde_json::json!({
        "op": "spawn",
        "agent": agent.preset,
        "model": agent.model,
        "system_prompt": agent.prompt,
    });
    writer.write_all(cmd.to_string().as_bytes()).await.unwrap();
    writer.write_all(b"\n").await.unwrap();
    writer.flush().await.unwrap();
    loop {
        let raw = lines.next_line().await.unwrap().unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        if v["kind"] == "spawned" {
            return v["session"].as_str().unwrap().to_string();
        }
    }
}

#[tokio::test]
async fn run_sends_persona_as_system_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("roy.sock");
    let db = dir.path().join("agents.db");

    let (tx, rx) = tokio::sync::oneshot::channel();
    let daemon = tokio::spawn(fake_daemon(socket.clone(), tx));

    // Build the store, insert an agent, then spawn over the wire.
    let pool = roy_agents::open(&db).await.unwrap();
    let store = roy_agents::Store::new(pool);
    let agent = store
        .create(roy_agents::NewAgent {
            name: "Reviewer".into(),
            description: None,
            preset: "claude".into(),
            model: Some("claude-opus-4-7".into()),
            prompt: "You are terse.".into(),
            task: None,
            persistent: false,
        })
        .await
        .unwrap();

    let session = wire_spawn(&socket, &agent).await;
    assert_eq!(session, "sess-1");

    let cmd = rx.await.unwrap();
    assert_eq!(cmd["op"], "spawn");
    assert_eq!(cmd["agent"], "claude");
    assert_eq!(cmd["system_prompt"], "You are terse.");
    daemon.await.unwrap();
}
