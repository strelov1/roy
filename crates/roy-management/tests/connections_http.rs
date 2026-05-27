//! HTTP CRUD for /connections — full integration through the wired router,
//! using the same `tests/common/mod.rs` harness as auth_flow.rs.
//!
//! Verifies auth gating, ownership isolation, and the full
//! create/list/get/update/delete cycle through the real axum router.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use roy_auth::test_support::make_user;
use serde_json::{json, Value};
use tower::ServiceExt;

mod common;
use common::{login_as, test_app, test_app_with_mock_daemon};

#[tokio::test]
async fn create_list_get_update_delete() {
    let (app, pool, _wd) = test_app().await;
    let alice = make_user(&pool, "alice").await;
    let cookie = login_as(&app, "alice", "test-password-1234").await;

    // Create
    let body = json!({
        "name": "Linear",
        "kind": "mcp_stdio",
        "config": {"command": "npx", "args": ["-y", "@linear/mcp"]},
        "secrets": {"LINEAR_API_KEY": "lin_xxx"}
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/connections")
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let created: Value = serde_json::from_slice(&bytes).unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["slug"], "linear");
    assert_eq!(created["owner_id"], alice.id);

    // List
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/connections")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let listed: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);

    // Get
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/connections/{id}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Update description (set to "personal")
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/connections/{id}"))
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .body(Body::from(json!({"description": "personal"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let updated: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(updated["description"], "personal");

    // Delete
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/connections/{id}"))
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn unauthenticated_returns_401() {
    let (app, _pool, _wd) = test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/connections")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn session_create_forwards_connections() {
    let (app, pool, _ws) = test_app().await;
    let _alice = make_user(&pool, "alice").await;
    let cookie = login_as(&app, "alice", "test-password-1234").await;

    // 1. Alice creates two connections.
    let conn_a = create_connection(&app, &cookie, "Linear").await;
    let conn_b = create_connection(&app, &cookie, "Notion").await;

    // 2. Alice creates a session with both connection_ids attached.
    let body = json!({
        "agent": "claude",
        "scope": "personal",
        "connection_ids": [conn_a, conn_b]
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let session_resp: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(session_resp["session_id"], "sess-1");
}

async fn create_connection(app: &axum::Router, cookie: &str, name: &str) -> String {
    let body = json!({
        "name": name,
        "kind": "mcp_stdio",
        "config": {"command": "npx"}
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/connections")
                .header("content-type", "application/json")
                .header("cookie", cookie)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let created: Value = serde_json::from_slice(&bytes).unwrap();
    created["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn session_create_rejects_unknown_connection() {
    let (app, pool, _ws) = test_app().await;
    let _alice = make_user(&pool, "alice").await;
    let cookie = login_as(&app, "alice", "test-password-1234").await;

    let body = json!({
        "agent": "claude",
        "scope": "personal",
        "connection_ids": ["nonexistent-id"]
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn session_create_rejects_cross_user_connection() {
    let (app, pool, _ws) = test_app().await;
    let _alice = make_user(&pool, "alice").await;
    let _bob = make_user(&pool, "bob").await;
    let alice_cookie = login_as(&app, "alice", "test-password-1234").await;
    let bob_cookie = login_as(&app, "bob", "test-password-1234").await;

    // Alice creates a connection.
    let alice_conn = create_connection(&app, &alice_cookie, "Linear").await;

    // Bob tries to attach it. Should 400 (don't leak existence).
    let body = json!({
        "agent": "claude",
        "scope": "personal",
        "connection_ids": [alice_conn]
    });
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .header("cookie", &bob_cookie)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn session_create_forwards_specs_to_daemon() {
    let (app, pool, _ws, mock) = test_app_with_mock_daemon().await;
    let _alice = make_user(&pool, "alice").await;
    let cookie = login_as(&app, "alice", "test-password-1234").await;

    // Create a connection with a config + secrets.
    let body = json!({
        "name": "Linear",
        "kind": "mcp_stdio",
        "config": {"command": "npx", "args": ["-y", "@linear/mcp"]},
        "secrets": {"LINEAR_API_KEY": "lin_xxx"}
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/connections")
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let created: Value = serde_json::from_slice(&bytes).unwrap();
    let conn_id = created["id"].as_str().unwrap().to_string();

    // Create a session referencing that connection.
    let body = json!({
        "agent": "claude",
        "scope": "personal",
        "connection_ids": [conn_id.clone()],
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sessions")
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Inspect the captured SpawnRequest.
    let captured = mock.last_spawn();
    assert_eq!(
        captured.connections.len(),
        1,
        "expected one connection in SpawnRequest, got: {:?}",
        captured.connections
    );
    let spec = &captured.connections[0];
    assert_eq!(spec.id, conn_id);
    assert_eq!(spec.slug, "linear");
    assert_eq!(spec.kind, "mcp_stdio");
    assert_eq!(spec.config["command"], "npx");
    assert_eq!(spec.config["args"][0], "-y");
    assert_eq!(spec.config["args"][1], "@linear/mcp");
    assert_eq!(spec.secrets.as_ref().unwrap()["LINEAR_API_KEY"], "lin_xxx");
}

#[tokio::test]
async fn cross_user_isolation() {
    let (app, pool, _wd) = test_app().await;
    let _alice = make_user(&pool, "alice").await;
    let _bob = make_user(&pool, "bob").await;
    let alice_cookie = login_as(&app, "alice", "test-password-1234").await;
    let bob_cookie = login_as(&app, "bob", "test-password-1234").await;

    // Alice creates a connection.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/connections")
                .header("content-type", "application/json")
                .header("cookie", &alice_cookie)
                .body(Body::from(
                    json!({
                        "name": "L",
                        "kind": "mcp_stdio",
                        "config": {"command": "npx"}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let created: Value = serde_json::from_slice(&bytes).unwrap();
    let id = created["id"].as_str().unwrap();

    // Bob can't see it.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/connections/{id}"))
                .header("cookie", &bob_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Bob's list is empty.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/connections")
                .header("cookie", &bob_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let listed: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(listed.as_array().unwrap().is_empty());
}
