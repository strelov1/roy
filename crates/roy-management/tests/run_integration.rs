//! End-to-end proof: when roy-management `spawn`s a session for an agent, the
//! `ClientCommand::Spawn` it sends carries `system_prompt = agent.prompt`. The
//! fake daemon reads one command, captures it, and replies Spawning + Spawned.
//! The test calls the real `roy_management::roy_client::spawn` so any
//! regression in the wire serialization is caught here.

use std::collections::BTreeMap;

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

#[tokio::test]
async fn run_sends_persona_as_system_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("roy.sock");
    let db = dir.path().join("agents.db");

    let (tx, rx) = tokio::sync::oneshot::channel();
    let daemon = tokio::spawn(fake_daemon(socket.clone(), tx));

    // Build the store, insert an agent. roy-management's `meta_store` adds
    // tables to the same SQLite DB; apply all three sets of migrations so the
    // post-spawn tag persistence has somewhere to land. `roy_auth` owns the
    // `users` / `teams` tables that `session_meta` references.
    let pool = roy_agents::open(&db).await.unwrap();
    roy_management::meta_store::MetaStore::apply_migrations(&pool)
        .await
        .unwrap();
    roy_auth::apply_migrations(&pool).await.unwrap();
    // The free-function `roy_client::spawn` now takes `created_by` as a
    // parameter (B5). Seed a user row with id="root" directly so the FK from
    // `session_meta` is satisfied when this test passes "root" through.
    // UserStore::create would assign a random UUID, which wouldn't match.
    sqlx::query(
        "INSERT OR IGNORE INTO users \
         (id, username, display_name, password_hash, timezone, created_at) \
         VALUES ('root', 'root', 'root', 'x', NULL, 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let workspace = dir.path().join("workspace");
    let meta = roy_management::meta_store::MetaStore::new(pool.clone(), workspace);
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

    // The real wire call — same code path `POST /agents/{id}/run` uses.
    let mut tags = BTreeMap::new();
    tags.insert("roy-management:agent_id".into(), agent.id.clone());
    let session = roy_management::roy_client::spawn(
        &socket,
        &meta,
        &agent.preset,
        agent.model.clone(),
        Some(agent.prompt.clone()),
        tags,
        "root",
    )
    .await
    .unwrap();
    assert_eq!(session, "sess-1");

    let cmd = rx.await.unwrap();
    assert_eq!(cmd["op"], "spawn");
    assert_eq!(cmd["agent"], "claude");
    assert_eq!(cmd["model"], "claude-opus-4-7");
    assert_eq!(cmd["system_prompt"], "You are terse.");

    // Tags landed in the meta store so the UI can render the row marker.
    let row = meta.get_session_meta(&session).await.unwrap().unwrap();
    assert_eq!(row.tags.get("roy-management:agent_id"), Some(&agent.id));

    daemon.await.unwrap();
}
