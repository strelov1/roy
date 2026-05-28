//! `GET /providers` end-to-end via the management test harness.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

mod common;
use common::{login_as, test_app};

#[tokio::test]
async fn empty_catalog_returns_empty_array() {
    let (app, pool, _ws) = test_app().await;
    let _alice = roy_auth::test_support::make_user(&pool, "alice").await;
    let cookie = login_as(&app, "alice", "test-password-1234").await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/providers")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn unauthenticated_returns_401() {
    let (app, _pool, _ws) = test_app().await;
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/providers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
