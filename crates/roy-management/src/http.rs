//! axum router + handlers for agent CRUD and session launch.
//! axum 0.8 path syntax uses `{id}` (not `:id`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use roy_agents::{Agent, AgentUpdate, NewAgent, StoreError};
use serde_json::json;

use crate::roy_client;
use crate::state::AppState;

/// Maps store/daemon errors to HTTP status codes.
pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound(id) => {
                ApiError(StatusCode::NOT_FOUND, format!("agent not found: {id}"))
            }
            StoreError::Db(e) => {
                // Don't surface sqlx text (column names, file paths) to API
                // callers. Log the cause and return a generic 500.
                tracing::error!(error = %e, "agent store db error");
                ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
            }
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/agents", get(list_agents).post(create_agent))
        .route(
            "/agents/{id}",
            get(get_agent).put(update_agent).delete(delete_agent),
        )
        .route("/agents/{id}/run", post(run_agent))
        .route("/presets", get(list_presets))
        .route("/projects", get(list_projects).post(create_project))
        .route("/projects/{id}", axum::routing::delete(delete_project))
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/{id}", get(get_session).patch(patch_session))
        .route("/sessions/{id}/tags", axum::routing::put(put_tags))
        .with_state(state)
}

async fn list_agents(State(s): State<AppState>) -> Result<Json<Vec<Agent>>, ApiError> {
    Ok(Json(s.store.list().await?))
}

async fn create_agent(
    State(s): State<AppState>,
    Json(new): Json<NewAgent>,
) -> Result<(StatusCode, Json<Agent>), ApiError> {
    validate_preset(&new.preset)?;
    let agent = s.store.create(new).await?;
    Ok((StatusCode::CREATED, Json(agent)))
}

async fn get_agent(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Agent>, ApiError> {
    Ok(Json(s.store.get(&id).await?))
}

async fn update_agent(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(up): Json<AgentUpdate>,
) -> Result<Json<Agent>, ApiError> {
    if let Some(preset) = &up.preset {
        validate_preset(preset)?;
    }
    Ok(Json(s.store.update(&id, up).await?))
}

async fn delete_agent(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    s.store.delete(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_presets(State(s): State<AppState>) -> Result<Json<serde_json::Value>, ApiError> {
    roy_client::list_presets(&s.socket_path)
        .await
        .map(Json)
        .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e.to_string()))
}

async fn run_agent(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let agent = s.store.get(&id).await?;
    let session = roy_client::spawn(
        &s.socket_path,
        &agent.preset,
        agent.model,
        Some(agent.prompt),
    )
    .await
    .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(Json(json!({ "session": session, "agent_id": agent.id })))
}

/// Preset must be one the daemon spawns. Kept as a const list rather than
/// importing `roy::AgentPreset` so this crate's only `roy` dependency stays
/// limited to the wire-protocol types listed in CLAUDE.md.
const VALID_PRESETS: &[&str] = &["claude", "gemini", "opencode", "codex"];

fn validate_preset(preset: &str) -> Result<(), ApiError> {
    if VALID_PRESETS.contains(&preset) {
        Ok(())
    } else {
        Err(ApiError(
            StatusCode::BAD_REQUEST,
            format!(
                "unknown preset '{preset}'; expected one of: {}",
                VALID_PRESETS.join(", ")
            ),
        ))
    }
}

async fn list_projects(
    State(s): State<AppState>,
) -> Result<Json<Vec<crate::meta_store::Project>>, ApiError> {
    s.meta.list_projects().await.map(Json).map_err(meta_to_api)
}

#[derive(serde::Deserialize)]
struct NewProject {
    name: String,
}

async fn create_project(
    State(s): State<AppState>,
    Json(req): Json<NewProject>,
) -> Result<(StatusCode, Json<crate::meta_store::Project>), ApiError> {
    let p = s
        .meta
        .create_project(&req.name)
        .await
        .map_err(meta_to_api)?;
    Ok((StatusCode::CREATED, Json(p)))
}

async fn delete_project(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    s.meta.delete_project(&id).await.map_err(meta_to_api)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize, Default)]
struct CreateSessionReq {
    agent: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    permission: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    agent_name: Option<String>,
    #[serde(default)]
    tags: BTreeMap<String, String>,
}

async fn create_session(
    State(s): State<AppState>,
    Json(req): Json<CreateSessionReq>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    // Resolve project_id -> cwd if provided; else use req.cwd verbatim.
    let cwd: Option<PathBuf> = if let Some(pid) = &req.project_id {
        let projects = s.meta.list_projects().await.map_err(meta_to_api)?;
        let p = projects
            .into_iter()
            .find(|p| &p.id == pid)
            .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, format!("invalid project: {pid}")))?;
        Some(PathBuf::from(p.path))
    } else {
        req.cwd.clone().map(PathBuf::from)
    };

    let sid = s
        .daemon
        .spawn(crate::roy_client::SpawnRequest {
            agent: req.agent.clone(),
            cwd,
            model: req.model.clone(),
            permission: req.permission.clone(),
            system_prompt: req.system_prompt.clone(),
        })
        .await
        .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, format!("daemon: {e}")))?;

    let meta = crate::meta_store::SessionMeta {
        session_id: sid.clone(),
        project_id: req.project_id.clone(),
        agent_id: None,
        agent_name: req.agent_name.clone(),
        display_label: None,
        tags: req.tags.clone(),
        created_at: chrono::Utc::now().timestamp(),
    };
    if let Err(meta_err) = s.meta.upsert_session_meta(&meta).await {
        // Compensating action: the daemon already spawned the session, but
        // we couldn't persist metadata. Close the orphaned session so it
        // doesn't leak. Log the close error if it also fails, but propagate
        // the original meta error to the caller.
        tracing::error!(error = %meta_err, session = %sid, "meta persist failed; closing session");
        if let Err(close_err) = s.daemon.close(&sid).await {
            tracing::error!(error = %close_err, session = %sid, "compensating close failed");
        }
        return Err(ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("meta_persist_failed; session was created and closed: {sid}"),
        ));
    }

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "session_id": sid,
            "project_id": req.project_id,
            "tags": req.tags,
            "agent_name": req.agent_name,
        })),
    ))
}

async fn list_sessions(State(s): State<AppState>) -> Result<Json<serde_json::Value>, ApiError> {
    let live = s
        .daemon
        .list()
        .await
        .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e.to_string()))?;
    let archived = s.daemon.list_archived().await.unwrap_or_default();
    let mut sids: Vec<String> = live
        .iter()
        .cloned()
        .chain(archived.iter().cloned())
        .collect();
    sids.sort();
    sids.dedup();

    let metas = s
        .meta
        .list_session_metas(&sids)
        .await
        .map_err(meta_to_api)?;
    let meta_by_sid: std::collections::HashMap<String, _> = metas
        .into_iter()
        .map(|m| (m.session_id.clone(), m))
        .collect();

    let out: Vec<serde_json::Value> = sids
        .into_iter()
        .map(|sid| {
            let m = meta_by_sid.get(&sid);
            json!({
                "session_id": sid,
                "project_id": m.and_then(|m| m.project_id.clone()),
                "agent_name": m.and_then(|m| m.agent_name.clone()),
                "tags": m.map(|m| m.tags.clone()).unwrap_or_default(),
                "live": live.contains(&sid),
            })
        })
        .collect();
    Ok(Json(serde_json::Value::Array(out)))
}

async fn get_session(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let meta = s.meta.get_session_meta(&id).await.map_err(meta_to_api)?;
    let live = s.daemon.list().await.unwrap_or_default();
    Ok(Json(json!({
        "session_id": id,
        "meta": meta,
        "live": live.contains(&id),
    })))
}

#[derive(serde::Deserialize)]
struct TagsBody {
    tags: BTreeMap<String, String>,
}

async fn put_tags(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<TagsBody>,
) -> Result<StatusCode, ApiError> {
    s.meta
        .replace_tags(&id, &body.tags)
        .await
        .map_err(meta_to_api)?;
    Ok(StatusCode::OK)
}

#[derive(serde::Deserialize)]
struct PatchSession {
    #[serde(default)]
    agent_name: Option<String>,
    #[serde(default)]
    display_label: Option<String>,
}

async fn patch_session(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<PatchSession>,
) -> Result<StatusCode, ApiError> {
    let mut meta = s
        .meta
        .get_session_meta(&id)
        .await
        .map_err(meta_to_api)?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("session: {id}")))?;
    if body.agent_name.is_some() {
        meta.agent_name = body.agent_name;
    }
    if body.display_label.is_some() {
        meta.display_label = body.display_label;
    }
    s.meta
        .upsert_session_meta(&meta)
        .await
        .map_err(meta_to_api)?;
    Ok(StatusCode::OK)
}

fn meta_to_api(e: crate::meta_store::MetaError) -> ApiError {
    use crate::meta_store::MetaError::*;
    match e {
        NotFound(m) => ApiError(StatusCode::NOT_FOUND, m),
        Conflict(m) => ApiError(StatusCode::CONFLICT, m),
        Invalid(m) => ApiError(StatusCode::BAD_REQUEST, m),
        Db(e) => {
            tracing::error!(error=%e, "meta db error");
            ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn test_state() -> AppState {
        use crate::meta_store::MetaStore;

        let dir = tempfile::tempdir().unwrap();
        let pool = roy_agents::open(&dir.path().join("agents.db"))
            .await
            .unwrap();
        MetaStore::apply_migrations(&pool).await.unwrap();
        // Keep the temp dir alive for the test process lifetime — dropping it
        // would invalidate the SQLite file referenced by the pool.
        std::mem::forget(dir);
        AppState {
            store: roy_agents::Store::new(pool.clone()),
            meta: MetaStore::new(pool),
            daemon: std::sync::Arc::new(roy_client::mock::MockDaemonClient::new()),
            socket_path: "/nonexistent.sock".into(),
        }
    }

    #[tokio::test]
    async fn create_then_get_roundtrips() {
        let app = router(test_state().await);
        let body = serde_json::to_vec(&json!({
            "name": "Reviewer", "preset": "claude", "prompt": "Be terse."
        }))
        .unwrap();
        let resp = app
            .clone()
            .oneshot(
                Request::post("/agents")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let created: Agent = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(created.slug, "reviewer");

        let resp = app
            .oneshot(
                Request::get(format!("/agents/{}", created.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_missing_is_404() {
        let app = router(test_state().await);
        let resp = app
            .oneshot(Request::get("/agents/nope").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_with_bad_preset_is_400() {
        let app = router(test_state().await);
        let body =
            serde_json::to_vec(&json!({ "name": "X", "preset": "klaude", "prompt": "" })).unwrap();
        let resp = app
            .oneshot(
                Request::post("/agents")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn projects_create_list_delete() {
        let app = router(test_state().await);
        // create
        let resp = app
            .clone()
            .oneshot(
                Request::post("/projects")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({"name":"p1"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let p: crate::meta_store::Project =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(p.name, "p1");

        // list
        let resp = app
            .clone()
            .oneshot(Request::get("/projects").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let listed: Vec<crate::meta_store::Project> =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(listed.len(), 1);

        // duplicate is 409
        let dup = app
            .clone()
            .oneshot(
                Request::post("/projects")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({"name":"p1"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(dup.status(), StatusCode::CONFLICT);

        // delete
        let del = app
            .oneshot(
                Request::delete(format!("/projects/{}", p.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(del.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn sessions_post_happy_path() {
        use std::sync::Arc;

        let mut st = test_state().await;
        let mock = Arc::new(crate::roy_client::mock::MockDaemonClient::new().with_spawn("sid-1"));
        st.daemon = mock.clone();
        let app = router(st);

        let body = serde_json::to_vec(&json!({
            "agent": "claude",
            "tags": {"env": "prod"},
            "agent_name": "Reviewer"
        }))
        .unwrap();
        let resp = app
            .oneshot(
                Request::post("/sessions")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let v: serde_json::Value =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(v["session_id"], "sid-1");
        assert_eq!(v["agent_name"], "Reviewer");
        assert_eq!(v["tags"]["env"], "prod");

        // mock recorded one spawn
        assert_eq!(mock.recorded_spawns.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn list_sessions_joins_live_and_meta() {
        use std::sync::{Arc, Mutex};

        let mut st = test_state().await;
        let mock = Arc::new(crate::roy_client::mock::MockDaemonClient {
            spawn_response: Mutex::new(Some(Ok("sid-A".into()))),
            list_response: Mutex::new(Some(vec!["sid-A".into(), "sid-B".into()])),
            ..Default::default()
        });
        st.daemon = mock;
        // Pre-insert meta for sid-A only; sid-B is orphan
        st.meta
            .upsert_session_meta(&crate::meta_store::SessionMeta {
                session_id: "sid-A".into(),
                project_id: None,
                agent_id: None,
                agent_name: Some("Rev".into()),
                display_label: None,
                tags: BTreeMap::from([("k".into(), "v".into())]),
                created_at: 1,
            })
            .await
            .unwrap();
        let app = router(st);
        let resp = app
            .oneshot(Request::get("/sessions").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v: serde_json::Value =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        let a = arr.iter().find(|r| r["session_id"] == "sid-A").unwrap();
        assert_eq!(a["agent_name"], "Rev");
        let b = arr.iter().find(|r| r["session_id"] == "sid-B").unwrap();
        assert_eq!(b["agent_name"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn sessions_post_rollback_on_meta_failure() {
        use std::sync::Arc;

        let mut st = test_state().await;
        let mock = Arc::new(crate::roy_client::mock::MockDaemonClient::new().with_spawn("sid-X"));
        st.daemon = mock.clone();
        // Force the upsert_session_meta to fail by closing the pool out from
        // under the MetaStore. The spawn happens first (mock, succeeds), then
        // the meta write fails, which must trigger a compensating close.
        st.meta.pool.close().await;
        let app = router(st);

        let body = serde_json::to_vec(&json!({"agent": "claude"})).unwrap();
        let resp = app
            .oneshot(
                Request::post("/sessions")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

        // Mock recorded the compensating Close
        assert_eq!(mock.recorded_closes.lock().unwrap().as_slice(), &["sid-X"]);
    }

    #[tokio::test]
    async fn put_tags_replaces() {
        let st = test_state().await;
        st.meta
            .upsert_session_meta(&crate::meta_store::SessionMeta {
                session_id: "sid".into(),
                project_id: None,
                agent_id: None,
                agent_name: None,
                display_label: None,
                tags: BTreeMap::from([("old".into(), "1".into())]),
                created_at: 1,
            })
            .await
            .unwrap();
        let app = router(st.clone());

        let body = serde_json::to_vec(&json!({"tags": {"new": "2"}})).unwrap();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(axum::http::Method::PUT)
                    .uri("/sessions/sid/tags")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let back = st.meta.get_session_meta("sid").await.unwrap().unwrap();
        assert_eq!(back.tags, BTreeMap::from([("new".into(), "2".into())]));
    }
}
