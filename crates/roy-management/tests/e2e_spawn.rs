//! E2E smoke: real daemon + real management. Ignored by default because it
//! requires built binaries. Run with: cargo test --test e2e_spawn -- --ignored

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tempfile::tempdir;
use tokio::io::AsyncBufReadExt;
use tokio::process::Command;

/// `CARGO_BIN_EXE_<name>` is only set for binaries in the *same* package
/// as the test. The `roy` binary lives in `roy-cli`, so resolve it by
/// taking the test's parent directory and looking for `roy` alongside it
/// (Cargo puts every workspace binary in the same target dir).
fn sibling_bin(name: &str) -> PathBuf {
    // This test is in roy-management, so we use a marker binary path.
    // We'll locate the target directory and find roy there.
    let me = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = me
        .ancestors()
        .find(|p| p.join("Cargo.toml").exists() && p.join("target").exists())
        .unwrap_or_else(|| me.parent().unwrap());
    let target_dir = workspace_root.join("target").join("debug");
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    target_dir.join(exe)
}

#[tokio::test]
#[ignore]
async fn project_session_visible_in_both_core_and_management() {
    let dir = tempdir().unwrap();
    let socket = dir.path().join("roy.sock");
    let journals = dir.path().join("journals");
    let workspace = dir.path().join("workspace");
    let sessions_db = dir.path().join("sessions.db");
    let agents_db = dir.path().join("agents.db");

    let roy_bin = sibling_bin("roy");

    // Start daemon
    let mut daemon = Command::new(&roy_bin)
        .args([
            "serve",
            "--socket",
            socket.to_str().unwrap(),
            "--journal-dir",
            journals.to_str().unwrap(),
            "--workspace-dir",
            workspace.to_str().unwrap(),
        ])
        .env("ROY_SESSIONS_DB", &sessions_db)
        .stderr(Stdio::piped())
        .spawn()
        .expect("start daemon");

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Start management
    let mut mgmt = Command::new(&roy_bin)
        .args([
            "management",
            "--socket",
            socket.to_str().unwrap(),
            "--addr",
            "127.0.0.1:0",
        ])
        .env("ROY_AGENTS_DB", &agents_db)
        .stderr(Stdio::piped())
        .spawn()
        .expect("start management");

    // Read port from management's stderr log line: tracing logs "listening on 127.0.0.1:NNNN"
    let stderr = mgmt.stderr.take().expect("stderr piped");
    let mut reader = tokio::io::BufReader::new(stderr);
    let mut line = String::new();
    let port = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            if let Some(addr) = line.split("listening on ").nth(1) {
                break addr.trim().split(':').nth(1).unwrap().to_string();
            }
        }
    })
    .await
    .expect("timeout waiting for management port");

    let base = format!("http://127.0.0.1:{}", port);
    let proj: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/projects", base))
        .json(&serde_json::json!({"name": "smoke"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pid = proj["id"].as_str().unwrap();

    let session: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/sessions", base))
        .json(&serde_json::json!({"agent": "claude", "project_id": pid}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let sid = session["session_id"].as_str().unwrap();

    // Verify it's visible in `GET /sessions`
    let sessions: serde_json::Value = reqwest::Client::new()
        .get(format!("{}/sessions", base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(sessions
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["session_id"] == sid));

    let _ = mgmt.kill().await;
    let _ = daemon.kill().await;
}
