pub mod config;
pub mod reply;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode},
    response::Response,
    routing::any,
    Router,
};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::bus::{BusSender, HttpReply, InboundEvent, ReplyHandle};
use crate::channels::webhook::config::{ReplyMode, WebhookConfig};
use crate::channels::Publisher;

const REPLY_TIMEOUT_DEFAULT: Duration = Duration::from_secs(620);

#[derive(Clone)]
struct RouteEntry {
    source_id: Arc<str>,
    secret: Option<Arc<[u8]>>,
    reply_mode: ReplyMode,
}

#[derive(Clone)]
struct AppState {
    bus: BusSender,
    routes: Arc<HashMap<String, RouteEntry>>,
    reply_timeout: Duration,
}

pub struct WebhookPublisher {
    bind_addr: SocketAddr,
    routes: HashMap<String, RouteEntry>,
}

pub struct WebhookSourceSpec {
    pub source_id: String,
    pub config: WebhookConfig,
}

impl WebhookPublisher {
    pub fn new(bind_addr: SocketAddr, sources: Vec<WebhookSourceSpec>) -> Result<Self> {
        let mut routes = HashMap::new();
        for s in sources {
            let secret = match &s.config.secret_env {
                Some(env_var) => Some(Arc::<[u8]>::from(
                    std::env::var(env_var)
                        .map_err(|_| {
                            anyhow::anyhow!(
                                "webhook source '{}' references env var '{}' which is not set",
                                s.source_id,
                                env_var
                            )
                        })?
                        .into_bytes(),
                )),
                None => None,
            };
            routes.insert(
                s.config.path.clone(),
                RouteEntry {
                    source_id: Arc::from(s.source_id.as_str()),
                    secret,
                    reply_mode: s.config.reply_mode,
                },
            );
        }
        Ok(Self { bind_addr, routes })
    }
}

#[async_trait]
impl Publisher for WebhookPublisher {
    async fn run(self: Arc<Self>, bus: BusSender, cancel: CancellationToken) -> Result<()> {
        let state = AppState {
            bus,
            routes: Arc::new(self.routes.clone()),
            reply_timeout: REPLY_TIMEOUT_DEFAULT,
        };
        let app = Router::new()
            .route("/{*path}", any(handle))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind(self.bind_addr).await?;
        tracing::info!(addr = %self.bind_addr, "webhook publisher listening");
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { cancel.cancelled().await })
            .await?;
        Ok(())
    }
}

async fn handle(
    State(state): State<AppState>,
    headers: HeaderMap,
    method: Method,
    uri: axum::http::Uri,
    body: Bytes,
) -> Response {
    let path = uri.path().to_string();
    let Some(entry) = state.routes.get(&path) else {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(axum::body::Body::from(
                r#"{"ok":false,"error":"unknown_path"}"#,
            ))
            .unwrap();
    };

    // HMAC validation when configured.
    if let Some(secret) = &entry.secret {
        let provided = headers
            .get("x-roy-signature")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret).expect("hmac");
        mac.update(&body);
        let expected = hex::encode(mac.finalize().into_bytes());
        if !bool::from(expected.as_bytes().ct_eq(provided.as_bytes())) {
            return Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .body(axum::body::Body::from(
                    r#"{"ok":false,"error":"bad_signature"}"#,
                ))
                .unwrap();
        }
    }

    let payload = build_payload(&method, &path, &headers, &body);
    let sender_id = extract_sender(&headers).unwrap_or_else(|| "anon".into());
    let id = Uuid::new_v4();

    let (reply, rx) = match entry.reply_mode {
        ReplyMode::Sync => {
            let (tx, rx) = oneshot::channel();
            (ReplyHandle::HttpSync(tx), Some(rx))
        }
        ReplyMode::Async => (ReplyHandle::Noop, None),
    };

    let ev = InboundEvent {
        id,
        source_id: entry.source_id.to_string(),
        source_kind: "webhook".into(),
        sender_id,
        payload,
        received_at: chrono::Utc::now(),
        reply,
    };

    // Bus push with a 5s timeout — spec'd to surface backpressure to the
    // caller as 503 rather than block indefinitely.
    match tokio::time::timeout(Duration::from_secs(5), state.bus.send(ev)).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) => {
            return Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(axum::body::Body::from(
                    r#"{"ok":false,"error":"bus_closed"}"#,
                ))
                .unwrap();
        }
        Err(_) => {
            return Response::builder()
                .status(StatusCode::SERVICE_UNAVAILABLE)
                .body(axum::body::Body::from(r#"{"ok":false,"error":"bus_full"}"#))
                .unwrap();
        }
    }

    match rx {
        None => Response::builder()
            .status(StatusCode::ACCEPTED)
            .body(axum::body::Body::from(format!(
                r#"{{"ok":true,"event_id":"{id}"}}"#
            )))
            .unwrap(),
        Some(rx) => match tokio::time::timeout(state.reply_timeout, rx).await {
            Ok(Ok(reply)) => HttpReply::into_response(reply),
            Ok(Err(_)) => Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(axum::body::Body::from(
                    r#"{"ok":false,"error":"reply_dropped"}"#,
                ))
                .unwrap(),
            Err(_) => Response::builder()
                .status(StatusCode::GATEWAY_TIMEOUT)
                .body(axum::body::Body::from(
                    r#"{"ok":false,"error":"reply_timeout"}"#,
                ))
                .unwrap(),
        },
    }
}

fn build_payload(
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    body: &Bytes,
) -> serde_json::Value {
    let mut hdr_map = serde_json::Map::new();
    for (k, v) in headers.iter() {
        if let Ok(v_str) = v.to_str() {
            hdr_map.insert(
                k.as_str().to_string(),
                serde_json::Value::String(v_str.to_string()),
            );
        }
    }
    let body_json = serde_json::from_slice::<serde_json::Value>(body)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(body).into_owned()));
    serde_json::json!({
        "method": method.as_str(),
        "path": path,
        "headers": serde_json::Value::Object(hdr_map),
        "body": body_json,
    })
}

fn extract_sender(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-forwarded-for")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
}
