//! End-to-end: webhook POST → real axum publisher → real dispatcher →
//! mock daemon → real reply hook → HTTP response.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use roy::{ServerEvent, StopReason, TurnEvent};
use roy_inbound::{
    bus::{self, EventRef},
    channels::webhook::{WebhookPublisher, WebhookSourceSpec},
    channels::Publisher,
    channels::webhook::config::WebhookConfig,
    dispatcher::InboundDispatcher,
    reply::{ReplyHook, ReplyHookRegistry},
    router::{ConfigRouter, Router},
    session::SessionResolver,
    store::{bindings::BindingStore, db},
};
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;

async fn spawn_mock_daemon(path: PathBuf, reply: ServerEvent) {
    let listener = UnixListener::bind(&path).unwrap();
    tokio::spawn(async move {
        let (sock, _) = listener.accept().await.unwrap();
        let (rd, mut wr) = sock.into_split();
        let mut lines = BufReader::new(rd).lines();
        let _ = lines.next_line().await.unwrap();
        let line = serde_json::to_string(&reply).unwrap();
        wr.write_all(line.as_bytes()).await.unwrap();
        wr.write_all(b"\n").await.unwrap();
    });
}

#[tokio::test]
async fn webhook_sync_round_trip() {
    // 1. Mock daemon.
    let dir = tempdir().unwrap();
    let sock_path = dir.path().join("daemon.sock");
    spawn_mock_daemon(
        sock_path.clone(),
        ServerEvent::FireDone {
            session: "sid-ok".into(),
            seq_range: (1, 3),
            result: TurnEvent::Result {
                cost_usd: None,
                stop_reason: StopReason::EndTurn,
            },
            assistant_text: "classified=ham".into(),
        },
    )
    .await;

    // 2. DB + bindings.
    let pool = db::open(&dir.path().join("inbound.db")).await.unwrap();
    let bindings = Arc::new(BindingStore::new(pool));

    // 3. Pick a free port.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    // 4. Config — uses the dynamic port, written to a temp TOML.
    let toml_cfg = format!(
        r#"
[server]
bind = "127.0.0.1:{port}"

[[sources]]
id = "orders"
kind = "webhook"
agent_id = "order-bot"
session = "ephemeral"
template = "Classify: {{{{payload.body.text}}}}"
fire_timeout_secs = 5
[sources.webhook]
path = "/orders"
reply_mode = "sync"
"#
    );
    let cfg_path = dir.path().join("c.toml");
    std::fs::write(&cfg_path, toml_cfg).unwrap();
    let cfg = roy_inbound::config::InboundConfig::load(&cfg_path).unwrap();

    // 5. Build the substrate.
    let (tx, rx) = bus::channel(16);
    let mut hooks = ReplyHookRegistry::new();
    hooks.register(
        "webhook",
        Box::new(|ev: &EventRef| -> Box<dyn ReplyHook> {
            Box::new(
                roy_inbound::channels::webhook::reply::WebhookReplyHook::new(ev.id.to_string()),
            )
        }),
    );
    let hooks = Arc::new(hooks);
    let router: Arc<dyn Router> = Arc::new(ConfigRouter::from_config(&cfg));
    let resolver = SessionResolver::new(bindings.clone(), "claude".into());

    let dispatcher = InboundDispatcher {
        bus: rx,
        router,
        resolver,
        bindings: bindings.clone(),
        hooks: hooks.clone(),
        socket_path: sock_path.clone(),
    };

    let webhook = Arc::new(
        WebhookPublisher::new(
            format!("127.0.0.1:{port}").parse().unwrap(),
            vec![WebhookSourceSpec {
                source_id: "orders".into(),
                config: WebhookConfig {
                    path: "/orders".into(),
                    secret_env: None,
                    reply_mode: roy_inbound::channels::webhook::config::ReplyMode::Sync,
                },
            }],
        )
        .unwrap(),
    );

    let cancel = CancellationToken::new();
    let cd = cancel.clone();
    let cp = cancel.clone();
    let h_disp = tokio::spawn(async move { dispatcher.run(cd).await.ok() });
    let h_pub = tokio::spawn(async move { webhook.run(tx, cp).await.ok() });

    // 6. Wait briefly for axum to bind.
    tokio::time::sleep(Duration::from_millis(150)).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/orders"))
        .json(&serde_json::json!({"text": "win a prize"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["assistant_text"], "classified=ham");

    cancel.cancel();
    let _ = tokio::join!(h_disp, h_pub);
}
