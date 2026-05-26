mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{login_as, test_app};
use http_body_util::BodyExt;
use roy_auth::test_support::temp_pool;
use roy_management::bootstrap::ensure_root;
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

#[serial_test::serial]
#[tokio::test]
async fn login_sets_cookie_then_me_returns_profile() {
    let (app, pool, _ws) = test_app().await;
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
    let (app, _pool, _ws) = test_app().await;
    let resp = app
        .oneshot(Request::get("/auth/me").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[serial_test::serial]
#[tokio::test]
async fn login_wrong_password_is_401() {
    let (app, pool, _ws) = test_app().await;
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

/// End-to-end: a logged-in user POSTs /sessions; the handler runs ACL
/// checks, resolves a per-scope cwd under `users/<uid>/sessions/<sid>`,
/// mkdir's it, persists session_meta, and returns 201. Verify the cwd
/// landed under `<workspace>/users/<uid>/` by walking the filesystem —
/// exactly one session directory should exist there after the POST.
#[serial_test::serial]
#[tokio::test]
async fn create_session_cwd_is_under_user_dir() {
    let (app, pool, workspace_dir) = test_app().await;
    let alice = roy_auth::test_support::make_user(&pool, "alice").await;
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

    // Exactly one session directory should exist under
    // <workspace>/users/<alice.id>/sessions/.
    let sessions_dir = workspace_dir.join("users").join(&alice.id).join("sessions");
    let entries: Vec<_> = std::fs::read_dir(&sessions_dir)
        .unwrap_or_else(|e| panic!("read_dir {sessions_dir:?}: {e}"))
        .map(|e| e.unwrap())
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one session dir under {sessions_dir:?}, got {entries:?}"
    );
    assert!(entries[0].file_type().unwrap().is_dir());
}

#[serial_test::serial]
#[tokio::test]
async fn login_rate_limit_blocks_after_5_failures() {
    std::env::set_var("ROY_TRUSTED_PROXIES", "*");
    let (app, _pool, _ws) = test_app().await;
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
