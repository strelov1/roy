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

fn validate_token(token: String) -> Result<String> {
    if !token.is_ascii() {
        anyhow::bail!("ws auth token must be ASCII (got non-ASCII bytes)");
    }
    Ok(token)
}

/// Load the WS auth token from `token_path`, or mint a fresh UUID and write it
/// owner-only (`0600`).
pub fn load_or_create_ws_token(token_path: &Path) -> Result<String> {
    if let Some(parent) = token_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating token dir {}", parent.display()))?;
    }
    match std::fs::read_to_string(token_path) {
        Ok(s) => validate_token(s.trim().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let token = uuid::Uuid::new_v4().to_string();
            create_owner_only_file(token_path, token.as_bytes())
                .with_context(|| format!("writing token {}", token_path.display()))?;
            validate_token(token)
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

    let inbound = async {
        while let Some(msg) = ws_stream.next().await {
            let text = match msg {
                Ok(Message::Text(t)) => t,
                Ok(Message::Close(_)) => break,
                // tungstenite answers ping/pong itself; ignore binary too.
                Ok(_) => continue,
                Err(e) => return Err(anyhow::Error::new(e).context("ws read")),
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
        Ok(())
    };

    let outbound = async {
        let result = loop {
            match daemon_lines.next_line().await {
                Ok(Some(line)) => {
                    if ws_sink.send(Message::Text(line.into())).await.is_err() {
                        break Ok(());
                    }
                }
                Ok(None) => break Ok(()),
                Err(e) => break Err(anyhow::Error::new(e).context("daemon read")),
            }
        };
        let _ = ws_sink.close().await;
        result
    };

    let result = tokio::select! {
        r = inbound => r,
        r = outbound => r,
    };
    result
}

/// Bind `addr` and accept authenticated WS connections forever, relaying each
/// to the daemon at `socket_path`. One spawned task per connection.
pub async fn run_ws_relay(
    addr: SocketAddr,
    token: Arc<String>,
    socket_path: Arc<Path>,
) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding ws listener on {addr}"))?;
    tracing::info!(%addr, "websocket relay listener up");
    loop {
        let (stream, peer) = listener.accept().await.context("ws accept")?;
        let token = Arc::clone(&token);
        let socket_path = Arc::clone(&socket_path);
        tokio::spawn(async move {
            let callback = ws_auth_callback(token);
            let ws = match tokio_tungstenite::accept_hdr_async(stream, callback).await {
                Ok(ws) => ws,
                Err(e) => {
                    tracing::warn!(%peer, error = %e, "ws handshake rejected");
                    return;
                }
            };
            tracing::debug!(%peer, "ws connection accepted");
            if let Err(e) = relay_connection(ws, &socket_path).await {
                tracing::warn!(%peer, error = %e, "ws relay ended with error");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    use futures_util::{SinkExt, StreamExt};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio_tungstenite::tungstenite::Message;

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

    #[tokio::test]
    async fn relay_round_trips_a_line_each_way() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("daemon.sock");

        // Fake daemon: accept one connection, read one line, echo a reply line.
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let fake_daemon = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let got = lines.next_line().await.unwrap().unwrap();
            write.write_all(b"{\"reply\":\"pong\"}\n").await.unwrap();
            write.flush().await.unwrap();
            got
        });

        // WS server side: accept one upgrade, hand it to relay_connection.
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let sock_for_relay = sock.clone();
        let relay = tokio::spawn(async move {
            let (stream, _) = tcp.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            relay_connection(ws, &sock_for_relay).await.unwrap();
        });

        // WS client: send a command line, expect the daemon's reply back.
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();
        ws.send(Message::Text("{\"cmd\":\"ping\"}".into()))
            .await
            .unwrap();

        let reply = loop {
            match ws.next().await.expect("ws closed").unwrap() {
                Message::Text(t) => break t.to_string(),
                _ => continue,
            }
        };
        assert_eq!(reply, "{\"reply\":\"pong\"}");

        let daemon_saw = fake_daemon.await.unwrap();
        assert_eq!(daemon_saw, "{\"cmd\":\"ping\"}");

        let _ = ws.close(None).await;
        let _ = relay.await;
    }

    #[tokio::test]
    async fn ws_drop_closes_daemon_connection() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("daemon.sock");

        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        // Fake daemon: accept, then read until EOF; return whether EOF was seen.
        let fake_daemon = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, _write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            // Returns None at EOF.
            lines.next_line().await.unwrap()
        });

        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap();
        let sock_for_relay = sock.clone();
        let relay = tokio::spawn(async move {
            let (stream, _) = tcp.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            relay_connection(ws, &sock_for_relay).await.unwrap();
        });

        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();
        // Close immediately without sending anything.
        ws.close(None).await.unwrap();

        let eof = fake_daemon.await.unwrap();
        assert!(eof.is_none(), "daemon must observe EOF after WS closes");
        let _ = relay.await;
    }

    #[tokio::test]
    async fn handshake_rejects_missing_or_wrong_token() {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;

        let token = Arc::new("the-real-token".to_string());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let token_for_server = Arc::clone(&token);
        let server = tokio::spawn(async move {
            let mut results = Vec::new();
            for _ in 0..3 {
                let (stream, _) = listener.accept().await.unwrap();
                let cb = ws_auth_callback(Arc::clone(&token_for_server));
                results.push(
                    tokio_tungstenite::accept_hdr_async(stream, cb)
                        .await
                        .is_ok(),
                );
            }
            results
        });

        // 1. No token → reject.
        let url = format!("ws://{addr}");
        assert!(tokio_tungstenite::connect_async(&url).await.is_err());

        // 2. Wrong token → reject.
        let mut req = url.as_str().into_client_request().unwrap();
        req.headers_mut()
            .insert(WS_TOKEN_HEADER, http::HeaderValue::from_static("nope"));
        assert!(tokio_tungstenite::connect_async(req).await.is_err());

        // 3. Correct token → accept.
        let mut req = url.as_str().into_client_request().unwrap();
        req.headers_mut().insert(
            WS_TOKEN_HEADER,
            http::HeaderValue::from_static("the-real-token"),
        );
        let ok = tokio_tungstenite::connect_async(req).await;
        assert!(ok.is_ok(), "correct token must be accepted");

        let results = server.await.unwrap();
        assert_eq!(results, vec![false, false, true]);
    }
}
