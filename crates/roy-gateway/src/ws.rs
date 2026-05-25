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

/// Validate the shared-secret token presented via `Sec-WebSocket-Protocol`.
/// Returns HTTP 401 on missing/invalid; echoes the subprotocol back on success
/// (required by the WS spec when the server selects a subprotocol).
fn ws_auth_callback(
    expected: Arc<String>,
) -> impl FnOnce(&Request, Response) -> std::result::Result<Response, ErrorResponse> {
    move |req, mut resp| {
        let provided = req
            .headers()
            .get(WS_TOKEN_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if provided != expected.as_str() {
            let body = if provided.is_empty() {
                "missing roy ws token (set Sec-WebSocket-Protocol)"
            } else {
                "invalid roy ws token"
            };
            return Err(http::Response::builder()
                .status(http::StatusCode::UNAUTHORIZED)
                .body(Some(body.into()))
                .expect("valid http response"));
        }
        resp.headers_mut().insert(
            WS_TOKEN_HEADER,
            http::HeaderValue::from_str(provided).expect("token is ascii uuid"),
        );
        Ok(resp)
    }
}

/// Bridge one accepted WS stream to a fresh daemon Unix-socket connection.
/// Pumps lines verbatim in both directions until either side closes.
async fn relay_connection<S>(
    ws: tokio_tungstenite::WebSocketStream<S>,
    socket_path: &Path,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let daemon = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connecting to daemon at {}", socket_path.display()))?;
    let (daemon_read, mut daemon_write) = daemon.into_split();
    let mut daemon_lines = BufReader::new(daemon_read).lines();

    let (mut ws_sink, mut ws_stream) = ws.split();

    // WS client → daemon: forward each text frame as a line.
    let inbound = async {
        while let Some(msg) = ws_stream.next().await {
            let text = match msg {
                Ok(Message::Text(t)) => t,
                Ok(Message::Close(_)) => break,
                // tungstenite answers ping/pong itself; ignore binary too.
                Ok(_) => continue,
                Err(_) => break,
            };
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            if daemon_write.write_all(text.as_bytes()).await.is_err()
                || daemon_write.write_all(b"\n").await.is_err()
                || daemon_write.flush().await.is_err()
            {
                break;
            }
        }
    };

    // daemon → WS client: forward each line as a text frame.
    let outbound = async {
        while let Ok(Some(line)) = daemon_lines.next_line().await {
            if ws_sink.send(Message::Text(line.into())).await.is_err() {
                break;
            }
        }
        let _ = ws_sink.close().await;
    };

    tokio::select! {
        _ = inbound => {}
        _ = outbound => {}
    }
    Ok(())
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
