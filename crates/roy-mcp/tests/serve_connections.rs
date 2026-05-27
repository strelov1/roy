//! End-to-end stdio test of `roy mcp serve-connections`.
//!
//! Drives the binary directly via tokio::process and speaks MCP JSON-RPC to
//! it the same way the ACP agent would. Backs onto the python fake upstream
//! at tests/scripts/fake-mcp-upstream.py.
//!
//! The `roy` binary lives in `roy-cli`, which has no lib target, so we can't
//! add it as a dev-dep to get `CARGO_BIN_EXE_roy`. Instead we shell out to
//! `cargo build --bin roy` once and resolve the path via the workspace
//! target directory.

use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

fn fake_upstream_path() -> String {
    let crate_root = env!("CARGO_MANIFEST_DIR");
    format!("{crate_root}/tests/scripts/fake-mcp-upstream.py")
}

/// Build the `roy` binary (no-op if already built) and return its path.
/// Walks up from this crate's manifest dir to the workspace root, then
/// resolves `target/{debug|release}/roy`. Honours `CARGO_TARGET_DIR` if set.
async fn ensure_bin() -> PathBuf {
    // Walk up from this crate to find the workspace root (the dir whose
    // Cargo.toml contains `[workspace]`).
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.exists() {
            let text = std::fs::read_to_string(&manifest).unwrap();
            if text.contains("[workspace]") {
                break dir;
            }
        }
        if !dir.pop() {
            panic!(
                "no [workspace] Cargo.toml above {}",
                env!("CARGO_MANIFEST_DIR")
            );
        }
    };

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let status = Command::new(&cargo)
        .args(["build", "--quiet", "--bin", "roy"])
        .current_dir(&workspace_root)
        .status()
        .await
        .expect("spawn cargo build");
    assert!(status.success(), "cargo build --bin roy failed");

    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target"));
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let bin = target.join(profile).join("roy");
    assert!(bin.exists(), "roy binary not found at {}", bin.display());
    bin
}

async fn proto_send(stdin: &mut tokio::process::ChildStdin, v: &Value) {
    let s = serde_json::to_string(v).unwrap();
    stdin.write_all(s.as_bytes()).await.unwrap();
    stdin.write_all(b"\n").await.unwrap();
    stdin.flush().await.unwrap();
}

async fn proto_recv(stdout: &mut BufReader<tokio::process::ChildStdout>) -> Value {
    let mut line = String::new();
    stdout.read_line(&mut line).await.unwrap();
    assert!(!line.is_empty(), "EOF before response");
    serde_json::from_str(line.trim()).unwrap()
}

#[tokio::test]
async fn aggregates_and_proxies_one_upstream() {
    let bin = ensure_bin().await;
    let bundle = json!({
        "session_id": "test-session",
        "connections": [
            {
                "id": "conn-1",
                "slug": "fake",
                "kind": "mcp_stdio",
                "config": {
                    "command": "python3",
                    "args": [fake_upstream_path()]
                }
            }
        ]
    });

    let mut child = Command::new(&bin)
        .args(["mcp", "serve-connections", "--specs-stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    // 1. Send the bundle.
    stdin
        .write_all(bundle.to_string().as_bytes())
        .await
        .unwrap();
    stdin.write_all(b"\n").await.unwrap();
    stdin.flush().await.unwrap();

    // 2. Initialize.
    proto_send(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}),
    )
    .await;
    let resp = proto_recv(&mut stdout).await;
    assert_eq!(resp["id"], json!(1));
    assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");

    // 3. List tools — should be the fake's echo, prefixed.
    proto_send(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    )
    .await;
    let resp = proto_recv(&mut stdout).await;
    let tools = resp["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1, "expected one tool, got: {tools:?}");
    assert_eq!(tools[0]["name"], "fake__echo");
    assert_eq!(tools[0]["description"], "Echo input back as text.");

    // 4. Call the tool — proxy strips the prefix and forwards `arguments`.
    proto_send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "fake__echo", "arguments": {"msg": "hello"}}
        }),
    )
    .await;
    let resp = proto_recv(&mut stdout).await;
    assert_eq!(resp["result"]["content"][0]["text"], "hello");

    // 5. Unknown tool — proxy errors cleanly.
    proto_send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {"name": "nope", "arguments": {}}
        }),
    )
    .await;
    let resp = proto_recv(&mut stdout).await;
    assert_eq!(resp["error"]["code"], -32000);

    child.kill().await.unwrap();
}
