//! axum router + handlers for agent CRUD and session launch.
//! axum 0.8 path syntax uses `{id}` (not `:id`).

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
    let p = s.meta.create_project(&req.name).await.map_err(meta_to_api)?;
    Ok((StatusCode::CREATED, Json(p)))
}

async fn delete_project(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    s.meta.delete_project(&id).await.map_err(meta_to_api)?;
    Ok(StatusCode::NO_CONTENT)
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
                    .body(Body::from(serde_json::to_vec(&json!({"name":"p1"})).unwrap()))
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
                    .body(Body::from(serde_json::to_vec(&json!({"name":"p1"})).unwrap()))
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
}
