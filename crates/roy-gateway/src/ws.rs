//! WebSocket relay: a transparent bridge between WS clients and the roy
//! daemon's Unix socket. The protocol is identical on both sides (the same
//! control-protocol JSON), so this never parses a `ClientCommand` — it pumps
//! lines verbatim. Each WS connection gets its own dedicated daemon
//! connection, so input leases and subscriptions live in the daemon exactly as
//! for a direct client.
//!
//! Auth: browsers can't set arbitrary headers on `new WebSocket(url,
//! [protocols])`, so the client offers two subprotocol tokens —
//! `"roy-jwt"` (a literal marker) and the JWT itself. The server verifies the
//! JWT via `roy_auth::verify_ws_protocol` and echoes back **only** the
//! `"roy-jwt"` marker (the JWT must never appear on the wire after the
//! upgrade response).

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

/// The subprotocol slot where the client offers `roy-jwt,<JWT>`.
const WS_TOKEN_HEADER: &str = "sec-websocket-protocol";

/// The literal subprotocol marker echoed back on a successful handshake. The
/// JWT itself is **never** echoed.
const WS_PROTOCOL_MARKER: &str = "roy-jwt";

/// Pure verification of a `Sec-WebSocket-Protocol` header value. Exposed so
/// tests can exercise the auth path without a running HTTP server. Returns the
/// authenticated user id on success.
pub fn ws_auth_callback_inner(
    header_value: &str,
) -> std::result::Result<String, roy_auth::JwtError> {
    roy_auth::verify_ws_protocol(header_value)
}

/// Validate the JWT presented via `Sec-WebSocket-Protocol`. Returns HTTP 401
/// on missing/invalid; echoes back **only** the literal `"roy-jwt"` marker on
/// success (required by the WS spec when the server selects a subprotocol).
fn ws_auth_callback(
) -> impl FnOnce(&Request, Response) -> std::result::Result<Response, ErrorResponse> + Clone {
    move |req, mut resp| {
        let provided = req
            .headers()
            .get(WS_TOKEN_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        match ws_auth_callback_inner(provided) {
            Ok(_user_id) => {
                resp.headers_mut().insert(
                    WS_TOKEN_HEADER,
                    http::HeaderValue::from_static(WS_PROTOCOL_MARKER),
                );
                Ok(resp)
            }
            Err(_) => Err(http::Response::builder()
                .status(http::StatusCode::UNAUTHORIZED)
                .body(Some("invalid roy ws token".into()))
                .expect("valid http response")),
        }
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
        loop {
            match daemon_lines.next_line().await {
                Ok(Some(line)) => {
                    if ws_sink.send(Message::Text(line.into())).await.is_err() {
                        break Ok(());
                    }
                }
                Ok(None) => break Ok(()),
                Err(e) => break Err(anyhow::Error::new(e).context("daemon read")),
            }
        }
    };

    let result = tokio::select! {
        r = inbound => r,
        r = outbound => r,
    };
    // Echo a Close frame regardless of which side ended first (RFC 6455 §5.5.1):
    // on client-initiated close `inbound` wins and drops `outbound`, so this is
    // the only place the close handshake gets completed back to the client.
    let _ = ws_sink.close().await;
    result
}

/// Bind `addr` and accept JWT-authenticated WS connections forever, relaying
/// each to the daemon at `socket_path`. One spawned task per connection.
pub async fn run_ws_relay(addr: SocketAddr, socket_path: Arc<Path>) -> Result<()> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding ws listener on {addr}"))?;
    tracing::info!(%addr, "websocket relay listener up");
    loop {
        let (stream, peer) = listener.accept().await.context("ws accept")?;
        let socket_path = Arc::clone(&socket_path);
        tokio::spawn(async move {
            let callback = ws_auth_callback();
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

    use futures_util::{SinkExt, StreamExt};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio_tungstenite::tungstenite::Message;

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
}
