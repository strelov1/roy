mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::test_app;
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn login(app: &axum::Router, pool: &sqlx::SqlitePool) -> String {
    roy_auth::test_support::make_user(pool, "alice").await;
    let body = serde_json::to_vec(
        &serde_json::json!({"username":"alice","password":"test-password-1234"}),
    )
    .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::post("/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    resp.headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}

async fn json_body(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[serial_test::serial]
#[tokio::test]
async fn bind_bot_then_internal_endpoint_resolves_persona() {
    let (app, pool, workspace) = test_app().await;
    let cookie = login(&app, &pool).await;

    // 1. Create a telegram_bot connection.
    let conn_body = serde_json::json!({
        "name": "Support Bot",
        "kind": "telegram_bot",
        "config": {},
        "secrets": {"bot_token": "111:AAA"}
    });
    let resp = app
        .clone()
        .oneshot(
            Request::post("/connections")
                .header("cookie", &cookie)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&conn_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let conn = json_body(resp).await;
    let conn_id = conn["id"].as_str().unwrap().to_string();

    // 2. Write an agent file in the owner's personal scope.
    let uid = conn["owner_id"].as_str().unwrap();
    let agent_dir = workspace.join("users").join(uid).join(".roy/agents");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("support-l1.md"),
        "---\nname: Support\ndescription: d\nharness: claude\n---\nYou are support.\n",
    )
    .unwrap();

    // 3. Bind the bot to the agent.
    let bind_body = serde_json::json!({
        "connection_id": conn_id,
        "agent_slug": "support-l1",
        "agent_scope": "user",
        "session_strategy": "per_sender_sticky",
        "idle_timeout_secs": 3600
    });
    let resp = app
        .clone()
        .oneshot(
            Request::post("/channel-bindings")
                .header("cookie", &cookie)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&bind_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // 4. Internal endpoint (bearer) returns the resolved source.
    let resp = app
        .clone()
        .oneshot(
            Request::get("/internal/telegram-sources")
                .header(
                    "authorization",
                    "Bearer test-internal-token-0123456789abcdef",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let sources = json_body(resp).await;
    let arr = sources.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["source_id"], format!("tg:{conn_id}"));
    assert_eq!(arr[0]["bot_token"], "111:AAA");
    assert_eq!(arr[0]["harness"], "claude");
    assert_eq!(arr[0]["system_prompt"], "You are support.\n");
    assert_eq!(arr[0]["session_strategy"]["kind"], "per_sender_sticky");

    // 5. Internal endpoint without the token → 401.
    let resp = app
        .oneshot(
            Request::get("/internal/telegram-sources")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
