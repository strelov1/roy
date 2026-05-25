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
        .route("/agents/_builder", post(start_builder))
        .route(
            "/agents/{id}",
            get(get_agent).put(update_agent).delete(delete_agent),
        )
        .route("/agents/{id}/run", post(run_agent))
        .route("/presets", get(list_presets))
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

#[derive(serde::Deserialize, Default)]
struct BuilderReq {
    #[serde(default)]
    existing_id: Option<String>,
}

#[derive(serde::Serialize)]
struct BuilderResp {
    agent_id: String,
    session_id: String,
}

async fn start_builder(
    State(s): State<AppState>,
    body: Option<Json<BuilderReq>>,
) -> Result<(StatusCode, Json<BuilderResp>), ApiError> {
    let req = body.map(|Json(b)| b).unwrap_or_default();

    // 1. Target agent: either existing or a fresh stub.
    let target = if let Some(id) = req.existing_id {
        s.store.get(&id).await?
    } else {
        s.store
            .create(NewAgent {
                name: "Untitled".into(),
                description: None,
                preset: "claude".into(),
                model: None,
                prompt: String::new(),
                task: None,
                persistent: false,
            })
            .await?
    };

    // 2. Builder seed.
    let builder = s.store.get_by_slug("builder").await.map_err(|e| match e {
        StoreError::NotFound(_) => ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "builder seed missing — migration did not run".into(),
        ),
        other => other.into(),
    })?;

    // 3. Compose the per-session system prompt.
    let system_prompt = format!(
        "{base}\n\n## Current task\nYou are editing agent id={id}. \
         Use only `roy agents update {id} ...` to apply changes. Never call create or delete.",
        base = builder.prompt,
        id = target.id,
    );

    // 4. Spawn.
    let session = roy_client::spawn(
        &s.socket_path,
        &builder.preset,
        builder.model.clone(),
        Some(system_prompt),
    )
    .await
    .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e.to_string()))?;

    Ok((
        StatusCode::CREATED,
        Json(BuilderResp {
            agent_id: target.id,
            session_id: session,
        }),
    ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn test_state() -> AppState {
        let dir = tempfile::tempdir().unwrap();
        let pool = roy_agents::open(&dir.path().join("agents.db"))
            .await
            .unwrap();
        // Keep the temp dir alive for the test process lifetime — dropping it
        // would invalidate the SQLite file referenced by the pool.
        std::mem::forget(dir);
        AppState {
            store: roy_agents::Store::new(pool),
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
    async fn _builder_endpoint_creates_stub_and_returns_session() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("roy.sock");

        let (tx, rx) = tokio::sync::oneshot::channel::<serde_json::Value>();
        let socket_for_task = socket.clone();
        let daemon = tokio::spawn(async move {
            let l = UnixListener::bind(&socket_for_task).unwrap();
            let (s, _) = l.accept().await.unwrap();
            let (r, mut w) = s.into_split();
            let mut lines = BufReader::new(r).lines();
            let raw = lines.next_line().await.unwrap().unwrap();
            let _ = tx.send(serde_json::from_str(&raw).unwrap());
            w.write_all(b"{\"kind\":\"spawning\",\"agent\":\"claude\"}\n")
                .await
                .unwrap();
            w.write_all(b"{\"kind\":\"spawned\",\"session\":\"sess-99\"}\n")
                .await
                .unwrap();
            w.flush().await.unwrap();
        });

        let pool = roy_agents::open(&dir.path().join("agents.db")).await.unwrap();
        let state = AppState {
            store: roy_agents::Store::new(pool),
            socket_path: socket,
        };
        let app = router(state.clone());
        let resp = app
            .oneshot(
                Request::post("/agents/_builder")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let agent_id = json["agent_id"].as_str().unwrap().to_string();
        assert!(!agent_id.is_empty(), "agent_id must be non-empty");
        assert_eq!(json["session_id"], "sess-99");

        // Stub exists.
        let stub = state.store.get(&agent_id).await.unwrap();
        assert_eq!(stub.name, "Untitled");

        // The Spawn captured by the fake daemon must carry the builder prompt
        // and mention the target agent id in system_prompt.
        let cmd = rx.await.unwrap();
        assert_eq!(cmd["op"], "spawn");
        let sp = cmd["system_prompt"].as_str().unwrap();
        assert!(sp.contains("Agent Builder"), "must include builder seed prompt; got: {sp}");
        assert!(sp.contains(&agent_id), "must mention target agent id; got: {sp}");
        daemon.await.unwrap();
    }
}
