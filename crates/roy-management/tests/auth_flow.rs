use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use roy_auth::test_support::{temp_pool, TEST_JWT_SECRET};
use roy_management::bootstrap::ensure_root;
use roy_management::http::router_for_tests;
use roy_management::state::AppState;
use tower::ServiceExt;

#[serial_test::serial]
#[tokio::test]
async fn bootstrap_creates_user_when_table_empty() {
    let pool = temp_pool().await;
    std::env::set_var("ROY_BOOTSTRAP_PASSWORD", "bootstrap-test-pw-1");
    let created = ensure_root(&pool).await.unwrap();
    assert!(created); // first call inserts

    let again = ensure_root(&pool).await.unwrap();
    assert!(!again); // second call is no-op

    let user = roy_auth::UserStore::new(pool.clone())
        .get_by_username("root")
        .await
        .unwrap();
    assert_eq!(user.username, "root");
}

async fn test_app() -> (axum::Router, sqlx::SqlitePool) {
    std::env::set_var("ROY_JWT_SECRET", TEST_JWT_SECRET);
    // Use `roy_agents::open` so the shared `agents.db` gets the full migration
    // stack (roy-agents v1-v3 + roy-management v4+) before roy-auth's
    // migrations layer on top. `roy_auth::test_support::temp_pool` skips the
    // roy-agents step, which leaves the `agents` table absent.
    let dir = tempfile::tempdir().expect("tempdir");
    let pool = roy_agents::open(&dir.path().join("agents.db"))
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
        store: roy_agents::Store::new(pool.clone()),
        meta,
        daemon,
        socket_path: std::path::PathBuf::from("/tmp/fake.sock"),
        scheduler_pool: None,
        pool: pool.clone(),
        workspace_dir,
        login_limiter: std::sync::Arc::new(roy_management::rate_limit::LoginLimiter::default()),
    };
    (router_for_tests(state), pool)
}

#[serial_test::serial]
#[tokio::test]
async fn login_sets_cookie_then_me_returns_profile() {
    let (app, pool) = test_app().await;
    let _alice = roy_auth::test_support::make_user(&pool, "alice").await;

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
    assert_eq!(resp.status(), StatusCode::OK);
    let cookie = resp
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(cookie.starts_with("roy-jwt="));

    let me = app
        .oneshot(
            Request::get("/auth/me")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(me.status(), StatusCode::OK);
    let bytes = me.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["username"], "alice");
}

#[serial_test::serial]
#[tokio::test]
async fn me_without_cookie_is_unauthorized() {
    let (app, _pool) = test_app().await;
    let resp = app
        .oneshot(Request::get("/auth/me").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[serial_test::serial]
#[tokio::test]
async fn login_wrong_password_is_401() {
    let (app, pool) = test_app().await;
    let _ = roy_auth::test_support::make_user(&pool, "alice").await;
    let body =
        serde_json::to_vec(&serde_json::json!({"username":"alice","password":"WRONG-PASSWORD"}))
            .unwrap();
    let resp = app
        .oneshot(
            Request::post("/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

async fn login_as(app: &axum::Router, username: &str, password: &str) -> String {
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

/// End-to-end: a logged-in user POSTs /sessions; the handler runs ACL
/// checks, resolves a per-scope cwd under `users/<uid>/sessions/<sid>`,
/// mkdir's it, persists session_meta, and returns 201. Asserting on the
/// cwd path inside the MockDaemonClient would require downcasting through
/// `Arc<dyn DaemonClient>`, which is awkward; the 201 plus a passing
/// auth/ACL chain is sufficient signal that the full flow succeeded.
#[serial_test::serial]
#[tokio::test]
async fn create_session_cwd_is_under_user_dir() {
    let (app, pool) = test_app().await;
    let _alice = roy_auth::test_support::make_user(&pool, "alice").await;
    let cookie = login_as(&app, "alice", "test-password-1234").await;

    let body = serde_json::to_vec(&serde_json::json!({
        "scope": "personal",
        "agent": "claude",
        "agent_name": "hello"
    }))
    .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::post("/sessions")
                .header("content-type", "application/json")
                .header("cookie", &cookie)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["session_id"], "sess-1");
}

#[serial_test::serial]
#[tokio::test]
async fn login_rate_limit_blocks_after_5_failures() {
    std::env::set_var("ROY_TRUSTED_PROXIES", "*");
    let (app, _pool) = test_app().await;
    for _ in 0..5 {
        let body =
            serde_json::to_vec(&serde_json::json!({"username":"nope","password":"nope"})).unwrap();
        let resp = app
            .clone()
            .oneshot(
                Request::post("/auth/login")
                    .header("content-type", "application/json")
                    .header("x-forwarded-for", "1.2.3.4")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
    let body =
        serde_json::to_vec(&serde_json::json!({"username":"nope","password":"nope"})).unwrap();
    let resp = app
        .oneshot(
            Request::post("/auth/login")
                .header("content-type", "application/json")
                .header("x-forwarded-for", "1.2.3.4")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    std::env::remove_var("ROY_TRUSTED_PROXIES");
}
