//! WebSocket relay: a transparent bridge between WS clients and the roy
//! daemon's Unix socket. The protocol is identical on both sides (the same
//! control-protocol JSON), so this never parses a `ClientCommand` — it pumps
//! lines verbatim. Each WS connection gets its own dedicated daemon
//! connection, so input leases and subscriptions live in the daemon exactly as
//! for a direct client.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixStream};
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http;
use tokio_tungstenite::tungstenite::Message;

/// Browsers can't set arbitrary headers on `new WebSocket(url, [protocols])`,
/// so the auth token rides the subprotocol slot instead.
const WS_TOKEN_HEADER: &str = "sec-websocket-protocol";

/// Atomically create `path` with `0600`, write `content` + `\n`, fsync.
/// Errors with `AlreadyExists` if the file is already there.
fn create_owner_only_file(path: &Path, content: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(content)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}

/// Load the WS auth token from `token_path`, or mint a fresh UUID and write it
/// owner-only (`0600`).
pub fn load_or_create_ws_token(token_path: &Path) -> Result<String> {
    if let Some(parent) = token_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating token dir {}", parent.display()))?;
    }
    match std::fs::read_to_string(token_path) {
        Ok(s) => Ok(s.trim().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let token = uuid::Uuid::new_v4().to_string();
            create_owner_only_file(token_path, token.as_bytes())
                .with_context(|| format!("writing token {}", token_path.display()))?;
            Ok(token)
        }
        Err(e) => Err(e).with_context(|| format!("reading token {}", token_path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn token_is_minted_once_then_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let token_path = dir.path().join("ws.token");

        let t1 = load_or_create_ws_token(&token_path).unwrap();
        assert!(!t1.is_empty(), "token must not be empty");

        let mode = std::fs::metadata(&token_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token file must be 0600, got {mode:o}");

        let t2 = load_or_create_ws_token(&token_path).unwrap();
        assert_eq!(t1, t2, "second call must return the persisted token");
    }
}
