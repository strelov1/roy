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

/// Create a personal project owned by `username`, returning its id. Exercised
/// via the real `POST /projects` handler so the row carries `created_by` set
/// to the logged-in user (the security-critical owner field).
async fn create_personal_project(app: &axum::Router, cookie: &str, name: &str) -> String {
    let body = serde_json::to_vec(&serde_json::json!({ "name": name })).unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::post("/projects")
                .header("content-type", "application/json")
                .header("cookie", cookie)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    v["id"].as_str().unwrap().to_string()
}

/// PUT a rename onto `project_id` as the given cookie; return the status.
/// This is the reachable handler that runs the project-access ACL check.
async fn put_rename(
    app: &axum::Router,
    cookie: &str,
    project_id: &str,
    new_name: &str,
) -> StatusCode {
    let body = serde_json::to_vec(&serde_json::json!({ "name": new_name })).unwrap();
    app.clone()
        .oneshot(
            Request::builder()
                .method(axum::http::Method::PUT)
                .uri(format!("/projects/{project_id}"))
                .header("content-type", "application/json")
                .header("cookie", cookie)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

/// SECURITY-CRITICAL regression guard for the project-access predicate.
///
/// A personal project (team_id NULL) created by user A must be accessible to
/// A and FORBIDDEN to a different user B. If someone naively replaced the
/// `created_by == user` check with `can_access_scope(Scope::Personal)` — which
/// returns `Ok(())` for every user — B's request below would return 200/OK
/// instead of 403, failing this test (a privilege-escalation bug).
#[serial_test::serial]
#[tokio::test]
async fn personal_project_is_forbidden_for_other_user() {
    let (app, pool, _workspace_dir) = test_app().await;
    let _alice = make_user(&pool, "alice").await;
    let _bob = make_user(&pool, "bob").await;

    let cookie_alice = login_as(&app, "alice", "test-password-1234").await;
    let cookie_bob = login_as(&app, "bob", "test-password-1234").await;

    let pid = create_personal_project(&app, &cookie_alice, "alice-proj").await;

    // Owner (alice) can access her own personal project.
    assert_eq!(
        put_rename(&app, &cookie_alice, &pid, "alice-renamed").await,
        StatusCode::OK,
        "owner must be allowed to access their personal project"
    );

    // Different user (bob) is forbidden — NOT 404 (the project exists) and
    // NOT 200 (he is not the creator).
    assert_eq!(
        put_rename(&app, &cookie_bob, &pid, "bob-steals").await,
        StatusCode::FORBIDDEN,
        "non-owner must be forbidden from a personal project"
    );
}

/// A team project is accessible to a team member (counterpart to the
/// non-member 403 case above), exercised through the same `update_project`
/// handler that runs `project_access`.
#[serial_test::serial]
#[tokio::test]
async fn team_project_accessible_to_member() {
    let (app, pool, _workspace_dir) = test_app().await;
    let alice = make_user(&pool, "alice").await;

    // alice creates a team (she becomes owner/member) and a team-scoped project.
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
    let cookie_alice = login_as(&app, "alice", "test-password-1234").await;
    let body = serde_json::to_vec(&serde_json::json!({
        "name": "team-proj",
        "team_id": team.id,
    }))
    .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::post("/projects")
                .header("content-type", "application/json")
                .header("cookie", &cookie_alice)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    let pid = serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // Team member (alice) can access the team project.
    assert_eq!(
        put_rename(&app, &cookie_alice, &pid, "team-renamed").await,
        StatusCode::OK,
        "team member must be allowed to access the team project"
    );
}
