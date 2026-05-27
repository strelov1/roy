//! Shared helpers for integration tests in roy-management.
//!
//! Note: cargo treats `tests/common/mod.rs` as a non-test module shared by
//! sibling integration tests — it does not produce a separate test binary.

#![allow(dead_code)]

use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use roy_auth::test_support::TEST_JWT_SECRET;
use roy_management::http::router_for_tests;
use roy_management::state::AppState;
use sqlx::SqlitePool;
use tower::ServiceExt;

/// Build a fully-wired test app: in-memory-ish sqlite pool (file-backed in
/// a tempdir kept alive for the test's lifetime), MetaStore + roy-auth
/// migrations applied, AppState with a mock daemon that spawns `sess-1`.
///
/// Returns (router, pool, workspace_dir) — the workspace dir is also
/// kept alive on disk so per-scope cwd creation succeeds.
pub async fn test_app() -> (axum::Router, SqlitePool, PathBuf) {
    std::env::set_var("ROY_JWT_SECRET", TEST_JWT_SECRET);
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = roy_management::db::open(&dir.path().join("agents.db"))
        .await
        .unwrap();
    roy_management::meta_store::MetaStore::apply_migrations(&pool)
        .await
        .unwrap();
    roy_auth::apply_migrations(&pool).await.unwrap();
    // Keep the tempdir alive for the test's lifetime — dropping it would
    // invalidate the SQLite file referenced by the pool.
    std::mem::forget(dir);
    let workspace_dir = std::env::temp_dir().join(format!("roy-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let meta = roy_management::meta_store::MetaStore::new(pool.clone(), workspace_dir.clone());

    let daemon = std::sync::Arc::new(
        roy_management::roy_client::mock::MockDaemonClient::new().with_spawn("sess-1"),
    );
    let state = AppState {
        meta,
        daemon,
        socket_path: std::path::PathBuf::from("/tmp/fake.sock"),
        scheduler_pool: None,
        pool: pool.clone(),
        workspace_dir: workspace_dir.clone(),
        login_limiter: std::sync::Arc::new(roy_management::rate_limit::LoginLimiter::default()),
        commands_cache: std::sync::Arc::new(roy_management::commands::CommandsCache::default()),
        agents_cache: std::sync::Arc::new(roy_management::agents::AgentsCache::default()),
        connections: roy_management::connections::Store::new(pool.clone()),
    };
    (router_for_tests(state), pool, workspace_dir)
}

/// POST /auth/login and return the `set-cookie` header value. Panics on
/// non-200 responses — call sites assume a successful login.
pub async fn login_as(app: &axum::Router, username: &str, password: &str) -> String {
    let body = serde_json::to_vec(&serde_json::json!({"username": username, "password": password}))
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
    assert_eq!(resp.status(), StatusCode::OK);
    resp.headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string()
}
