mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::{login_as, test_app};
use roy_auth::test_support::make_user;
use roy_auth::{NewTeam, TeamStore};
use tower::ServiceExt;

/// Non-member of a team cannot create a team-scoped session: the ACL
/// guard runs before any FS or daemon work, so the request must be
/// rejected with 403 before bob's workspace touches `teams/<tid>/`.
#[serial_test::serial]
#[tokio::test]
async fn non_member_cannot_create_team_session() {
    let (app, pool, _workspace_dir) = test_app().await;
    let alice = make_user(&pool, "alice").await;
    let _bob = make_user(&pool, "bob").await;

    // alice creates her team (note: this uses TeamStore directly because the
    // /teams API isn't wired yet — that's Phase D).
    let team = TeamStore::new(pool.clone())
        .create(
            NewTeam {
                name: "eng".into(),
                description: None,
            },
            &alice.id,
        )
        .await
        .unwrap();

    // bob logs in (not in team) and tries to create a team session
    let cookie_bob = login_as(&app, "bob", "test-password-1234").await;
    let body = serde_json::to_vec(&serde_json::json!({
        "scope": "team",
        "team_id": team.id,
        "harness": "claude",
    }))
    .unwrap();

    let resp = app
        .oneshot(
            Request::post("/sessions")
                .header("content-type", "application/json")
                .header("cookie", &cookie_bob)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}
