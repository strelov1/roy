//! axum router + handlers for agent CRUD and session launch.
//! axum 0.8 path syntax uses `{id}` (not `:id`).

use std::collections::BTreeMap;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use roy_agents::{Agent, AgentUpdate, NewAgent, StoreError};
use roy_auth::types::Scope;
use roy_scheduler::{
    store as sched_store,
    types::{Agent as SchedulerAgent, Fire, Trigger},
};
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;

use crate::auth;
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
    // All non-public routes go behind `require_user`. `/auth/login` and
    // `/auth/logout` stay public so unauthenticated clients can sign in.
    // `/auth/me` lives in `auth::protected_router()` and is mounted alongside
    // the other authenticated routes so the missing `AuthUser` extension
    // surfaces as 401, not 500.
    let protected = Router::new()
        .route("/agents", get(list_agents).post(create_agent))
        .route("/agents/_builder", post(start_builder))
        .route(
            "/agents/{id}",
            get(get_agent).put(update_agent).delete(delete_agent),
        )
        .route("/agents/{id}/run", post(run_agent))
        .route("/presets", get(list_presets))
        .route("/projects", get(list_projects).post(create_project))
        .route(
            "/projects/{id}",
            axum::routing::delete(delete_project).put(update_project),
        )
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/{id}", get(get_session).patch(patch_session))
        .route("/sessions/{id}/tags", axum::routing::put(put_tags))
        .route("/scheduler/agents", get(list_scheduler_agents))
        .route("/scheduler/triggers", get(list_scheduler_triggers))
        .route("/scheduler/fires", get(list_scheduler_fires))
        .route("/commands", get(list_commands))
        .route("/teams", get(auth::list_teams).post(auth::create_team))
        .route("/teams/{id}", axum::routing::delete(auth::delete_team))
        .route("/auth/invites", post(auth::create_invite))
        .route("/auth/accept-invite", post(auth::accept_invite))
        .merge(auth::protected_router())
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::require_user,
        ));

    auth::router().merge(protected).with_state(state)
}

/// Test-only wrapper around `router` so integration tests don't have to
/// reach into private state to construct the full app.
pub fn router_for_tests(state: AppState) -> Router {
    router(state)
}

fn sched_pool(state: &AppState) -> Result<&SqlitePool, ApiError> {
    state.scheduler_pool.as_ref().ok_or_else(|| {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "scheduler DB not attached — start roy-scheduler at least once to initialize it".into(),
        )
    })
}

fn db_to_api(e: anyhow::Error) -> ApiError {
    tracing::error!(error = %e, "scheduler db error");
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
}

async fn list_commands(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::commands::CommandInfo>>, ApiError> {
    Ok(Json(state.commands_cache.get().await))
}

async fn list_scheduler_agents(
    State(s): State<AppState>,
) -> Result<Json<Vec<SchedulerAgent>>, ApiError> {
    let pool = sched_pool(&s)?;
    sched_store::agents::list(pool)
        .await
        .map(Json)
        .map_err(db_to_api)
}

async fn list_scheduler_triggers(
    State(s): State<AppState>,
    Query(q): Query<SchedListQuery>,
) -> Result<Json<Vec<Trigger>>, ApiError> {
    let pool = sched_pool(&s)?;
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let v = match q.agent {
        Some(id) => sched_store::triggers::list_for_agent(pool, &id).await,
        None => sched_store::triggers::list_all(pool, limit).await,
    };
    v.map(Json).map_err(db_to_api)
}

async fn list_scheduler_fires(
    State(s): State<AppState>,
    Query(q): Query<SchedListQuery>,
) -> Result<Json<Vec<Fire>>, ApiError> {
    let pool = sched_pool(&s)?;
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let v = match q.agent {
        Some(id) => sched_store::fires::list_for_agent(pool, &id, limit).await,
        None => sched_store::fires::list_recent(pool, limit).await,
    };
    v.map(Json).map_err(db_to_api)
}

#[derive(Deserialize, Default)]
struct SchedListQuery {
    agent: Option<String>,
    limit: Option<i64>,
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
    axum::extract::Extension(crate::auth::AuthUser(user_id)): axum::extract::Extension<
        crate::auth::AuthUser,
    >,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let agent = s.store.get(&id).await?;
    let mut tags = BTreeMap::new();
    tags.insert("roy-management:agent_id".into(), agent.id.clone());
    let session = roy_client::spawn(
        &s.socket_path,
        &s.meta,
        &agent.preset,
        agent.model,
        Some(agent.prompt),
        tags,
        &user_id,
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
    session: String,
}

async fn start_builder(
    axum::extract::Extension(crate::auth::AuthUser(user_id)): axum::extract::Extension<
        crate::auth::AuthUser,
    >,
    State(s): State<AppState>,
    body: Option<Json<BuilderReq>>,
) -> Result<(StatusCode, Json<BuilderResp>), ApiError> {
    let req = body.map(|Json(b)| b).unwrap_or_default();

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

    let builder = s.store.get_by_slug("builder").await.map_err(|e| match e {
        StoreError::NotFound(_) => ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "builder seed missing — migration did not run".into(),
        ),
        other => other.into(),
    })?;

    let system_prompt = format!(
        "{base}\n\n## Current task\nYou are editing agent id={id}. \
         Use only `roy agents update {id} ...` to apply changes. Never call create or delete.",
        base = builder.prompt,
        id = target.id,
    );

    let mut tags = BTreeMap::new();
    tags.insert("roy-management:builder.agent_id".into(), target.id.clone());

    let session = roy_client::spawn(
        &s.socket_path,
        &s.meta,
        &builder.preset,
        builder.model.clone(),
        Some(system_prompt),
        tags,
        &user_id,
    )
    .await
    .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e.to_string()))?;

    Ok((
        StatusCode::CREATED,
        Json(BuilderResp {
            agent_id: target.id,
            session,
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

async fn list_projects(
    axum::extract::Extension(crate::auth::AuthUser(user_id)): axum::extract::Extension<
        crate::auth::AuthUser,
    >,
    State(s): State<AppState>,
) -> Result<Json<Vec<crate::meta_store::Project>>, ApiError> {
    let memberships = roy_auth::TeamStore::new(s.pool.clone())
        .list_for_user(&user_id)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "team list");
            ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal".into())
        })?;
    let team_ids: Vec<String> = memberships.into_iter().map(|t| t.id).collect();
    s.meta
        .list_projects_for_user(&user_id, &team_ids)
        .await
        .map(Json)
        .map_err(meta_to_api)
}

#[derive(serde::Deserialize)]
struct NewProject {
    name: String,
    #[serde(default)]
    team_id: Option<String>,
}

async fn create_project(
    axum::extract::Extension(crate::auth::AuthUser(user_id)): axum::extract::Extension<
        crate::auth::AuthUser,
    >,
    State(s): State<AppState>,
    Json(req): Json<NewProject>,
) -> Result<(StatusCode, Json<crate::meta_store::Project>), ApiError> {
    if let Some(team_id) = &req.team_id {
        roy_auth::Acl::new(&s.pool, &user_id)
            .can_admin_team(team_id)
            .await
            .map_err(|_| ApiError(StatusCode::FORBIDDEN, "forbidden".into()))?;
    }
    let p = s
        .meta
        .create_project(&req.name, &user_id, req.team_id.as_deref())
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

#[derive(serde::Deserialize)]
struct ProjectUpdate {
    name: String,
}

async fn update_project(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ProjectUpdate>,
) -> Result<Json<crate::meta_store::Project>, ApiError> {
    s.meta
        .update_project(&id, &req.name)
        .await
        .map(Json)
        .map_err(meta_to_api)
}

#[derive(serde::Deserialize, Default)]
struct CreateSessionReq {
    agent: String,
    /// "personal" (default) or "team". Determines whether the session's cwd
    /// lives under `users/<uid>/` or `teams/<tid>/`.
    #[serde(default = "default_scope_str")]
    scope: String,
    #[serde(default)]
    team_id: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
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

fn default_scope_str() -> String {
    "personal".into()
}

async fn create_session(
    axum::extract::Extension(crate::auth::AuthUser(user_id)): axum::extract::Extension<
        crate::auth::AuthUser,
    >,
    State(s): State<AppState>,
    Json(req): Json<CreateSessionReq>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    // Parse scope. `team` requires `team_id`; anything else is invalid.
    let scope = match req.scope.as_str() {
        "personal" => Scope::Personal,
        "team" => Scope::Team {
            team_id: req.team_id.clone().ok_or(ApiError(
                StatusCode::BAD_REQUEST,
                "team_id required for team scope".into(),
            ))?,
        },
        other => {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                format!("invalid scope: {other}"),
            ));
        }
    };

    // ACL: gate scope (team-membership check for team scope) and project_id.
    // Both checks run before any FS write or DB write — `resolve_cwd` will
    // mkdir parent dirs, so a forbidden caller must never get that far.
    let acl = roy_auth::Acl::new(&s.pool, &user_id);
    acl.can_access_scope(&scope)
        .await
        .map_err(|_| ApiError(StatusCode::FORBIDDEN, "forbidden".into()))?;
    if let Some(pid) = &req.project_id {
        acl.can_access_project(pid).await.map_err(|e| match e {
            roy_auth::AclError::NotFound => {
                ApiError(StatusCode::BAD_REQUEST, format!("invalid project: {pid}"))
            }
            _ => ApiError(StatusCode::FORBIDDEN, "forbidden".into()),
        })?;
    }

    // Resolve and materialize the per-scope cwd. The session_id used in the
    // path is a fresh UUID — independent of the daemon-assigned session id we
    // get back from `spawn`. The directory is the working directory the agent
    // runs in; the daemon's id is the handle we expose on the wire.
    let cwd_session_id = uuid::Uuid::new_v4().to_string();
    let cwd_scope = match &scope {
        Scope::Personal => crate::cwd::CwdScope::Personal,
        Scope::Team { .. } => crate::cwd::CwdScope::Team,
    };
    let cwd = crate::cwd::resolve_cwd(
        &s.workspace_dir,
        crate::cwd::CwdInput {
            scope: cwd_scope,
            user_id: user_id.clone(),
            team_id: req.team_id.clone(),
            project_id: req.project_id.clone(),
            session_id: cwd_session_id,
        },
    )
    .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    std::fs::create_dir_all(&cwd)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("mkdir cwd: {e}")))?;

    let sid = match s
        .daemon
        .spawn(crate::roy_client::SpawnRequest {
            agent: req.agent.clone(),
            cwd: Some(cwd.clone()),
            model: req.model.clone(),
            permission: req.permission.clone(),
            system_prompt: req.system_prompt.clone(),
        })
        .await
    {
        Ok(sid) => sid,
        Err(e) => {
            // Cleanup: the cwd directory was just mkdir'd for this session.
            // Remove it so the workspace doesn't accumulate orphans on
            // transient daemon failures. Ignore cleanup errors — we're
            // already in an error path and the workspace might be RO.
            if let Err(rm_err) = std::fs::remove_dir_all(&cwd) {
                tracing::warn!(error = %rm_err, cwd = %cwd.display(), "failed to cleanup cwd after spawn failure");
            }
            return Err(ApiError(StatusCode::BAD_GATEWAY, format!("daemon: {e}")));
        }
    };

    let meta = crate::meta_store::SessionMeta {
        session_id: sid.clone(),
        project_id: req.project_id.clone(),
        agent_id: None,
        agent_name: req.agent_name.clone(),
        display_label: None,
        created_by: user_id.clone(),
        team_id: req.team_id.clone(),
        tags: req.tags.clone(),
        created_at: chrono::Utc::now().timestamp(),
    };
    if let Err(meta_err) = s.meta.upsert_session_meta(&meta).await {
        // Compensating action: the daemon already spawned the session, but
        // we couldn't persist metadata. Close the orphaned session so it
        // doesn't leak, and remove the cwd directory we just mkdir'd. Log
        // the close error if it also fails, but propagate the original
        // meta error to the caller.
        tracing::error!(error = %meta_err, session = %sid, "meta persist failed; closing session");
        if let Err(close_err) = s.daemon.close(&sid).await {
            tracing::error!(error = %close_err, session = %sid, "compensating close failed");
        }
        if let Err(rm_err) = std::fs::remove_dir_all(&cwd) {
            tracing::warn!(error = %rm_err, cwd = %cwd.display(), "failed to cleanup cwd after meta failure");
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
    use std::collections::{HashMap, HashSet};

    let (live, archived) = tokio::join!(s.daemon.list(), s.daemon.list_archived());
    let live = live.map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e.to_string()))?;
    let archived = archived.unwrap_or_default();
    let live_set: HashSet<&String> = live.iter().collect();
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
    let meta_by_sid: HashMap<String, _> = metas
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
                "live": live_set.contains(&sid),
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
    Ok(StatusCode::NO_CONTENT)
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
        Io(e) => {
            tracing::error!(error=%e, "meta io error");
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
    use std::path::PathBuf;
    use tower::ServiceExt;

    /// Direct INSERT into `users` with id="root" to mirror B3's
    /// `ensure_root` bootstrap. `roy_auth::UserStore::create` would issue
    /// a random UUID for the id, which doesn't match what the handler
    /// stub binds (`created_by = "root"`).
    async fn seed_root_user(pool: &sqlx::SqlitePool) {
        sqlx::query(
            "INSERT OR IGNORE INTO users \
             (id, username, display_name, password_hash, timezone, created_at) \
             VALUES ('root', 'root', 'root', 'x', NULL, 0)",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    /// Set the JWT secret env var and mint a `Cookie:` header value for the
    /// `root` user. All protected-route tests in this module call this once
    /// up front, then thread the returned string into `.header("cookie", _)`.
    fn auth_cookie() -> String {
        std::env::set_var("ROY_JWT_SECRET", roy_auth::test_support::TEST_JWT_SECRET);
        let token = roy_auth::test_support::issue_jwt("root");
        format!("roy-jwt={token}")
    }

    /// Returns the state and the id of a freshly-seeded user. `created_by`
    /// columns on projects/session_meta are NOT NULL FKs into `users(id)`,
    /// so every test that writes either table needs a real user fixture.
    async fn test_state() -> (AppState, String) {
        use crate::meta_store::MetaStore;

        let dir = tempfile::tempdir().unwrap();
        let pool = roy_agents::open(&dir.path().join("agents.db"))
            .await
            .unwrap();
        MetaStore::apply_migrations(&pool).await.unwrap();
        roy_auth::apply_migrations(&pool).await.unwrap();
        // Seed a user with id="root" so the `created_by = "root"` stub in
        // create_project / create_session handlers satisfies the FK to
        // `users(id)`. B3's `ensure_root` bootstrap step will do the same in
        // production. Plus a fixture user for tests that need a real id.
        seed_root_user(&pool).await;
        let alice = roy_auth::test_support::make_user(&pool, "alice").await;
        let workspace = dir.path().join("workspace");
        // resolve_cwd canonicalizes the workspace dir; it must exist on disk
        // before create_session is invoked.
        std::fs::create_dir_all(&workspace).unwrap();
        // Keep the temp dir alive for the test process lifetime — dropping it
        // would invalidate the SQLite file referenced by the pool.
        std::mem::forget(dir);
        let state = AppState {
            store: roy_agents::Store::new(pool.clone()),
            meta: MetaStore::new(pool.clone(), workspace.clone()),
            daemon: std::sync::Arc::new(roy_client::mock::MockDaemonClient::new()),
            socket_path: "/nonexistent.sock".into(),
            scheduler_pool: None,
            pool,
            workspace_dir: workspace,
            login_limiter: std::sync::Arc::new(crate::rate_limit::LoginLimiter::default()),
            commands_cache: std::sync::Arc::new(crate::commands::CommandsCache::default()),
        };
        (state, alice.id)
    }

    #[tokio::test]
    async fn create_then_get_roundtrips() {
        let cookie = auth_cookie();
        let (st, _uid) = test_state().await;
        let app = router(st);
        let body = serde_json::to_vec(&json!({
            "name": "Reviewer", "preset": "claude", "prompt": "Be terse."
        }))
        .unwrap();
        let resp = app
            .clone()
            .oneshot(
                Request::post("/agents")
                    .header("content-type", "application/json")
                    .header("cookie", &cookie)
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
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_missing_is_404() {
        let cookie = auth_cookie();
        let (st, _uid) = test_state().await;
        let app = router(st);
        let resp = app
            .oneshot(
                Request::get("/agents/nope")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn create_with_bad_preset_is_400() {
        let cookie = auth_cookie();
        let (st, _uid) = test_state().await;
        let app = router(st);
        let body =
            serde_json::to_vec(&json!({ "name": "X", "preset": "klaude", "prompt": "" })).unwrap();
        let resp = app
            .oneshot(
                Request::post("/agents")
                    .header("content-type", "application/json")
                    .header("cookie", &cookie)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn projects_create_list_delete() {
        let cookie = auth_cookie();
        let (st, _uid) = test_state().await;
        let app = router(st);
        // create
        let resp = app
            .clone()
            .oneshot(
                Request::post("/projects")
                    .header("content-type", "application/json")
                    .header("cookie", &cookie)
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
            .oneshot(
                Request::get("/projects")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
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
                    .header("cookie", &cookie)
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
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(del.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn project_put_renames_and_reflects_in_list() {
        let cookie = auth_cookie();
        let (st, _uid) = test_state().await;
        let app = router(st);
        // create
        let resp = app
            .clone()
            .oneshot(
                Request::post("/projects")
                    .header("content-type", "application/json")
                    .header("cookie", &cookie)
                    .body(Body::from(
                        serde_json::to_vec(&json!({"name":"old"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let p: crate::meta_store::Project =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();

        // PUT new name
        let renamed = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(axum::http::Method::PUT)
                    .uri(format!("/projects/{}", p.id))
                    .header("content-type", "application/json")
                    .header("cookie", &cookie)
                    .body(Body::from(
                        serde_json::to_vec(&json!({"name":"shiny"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(renamed.status(), StatusCode::OK);
        let renamed_body: crate::meta_store::Project =
            serde_json::from_slice(&renamed.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(renamed_body.id, p.id);
        assert_eq!(renamed_body.name, "shiny");

        // GET list reflects new name
        let resp = app
            .oneshot(
                Request::get("/projects")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let listed: Vec<crate::meta_store::Project> =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "shiny");
    }

    #[tokio::test]
    async fn project_put_empty_name_is_400() {
        let cookie = auth_cookie();
        let (st, _uid) = test_state().await;
        let app = router(st);
        let resp = app
            .clone()
            .oneshot(
                Request::post("/projects")
                    .header("content-type", "application/json")
                    .header("cookie", &cookie)
                    .body(Body::from(
                        serde_json::to_vec(&json!({"name":"keep-me"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let p: crate::meta_store::Project =
            serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();

        let bad = app
            .oneshot(
                Request::builder()
                    .method(axum::http::Method::PUT)
                    .uri(format!("/projects/{}", p.id))
                    .header("content-type", "application/json")
                    .header("cookie", &cookie)
                    .body(Body::from(serde_json::to_vec(&json!({"name":""})).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn project_put_unknown_id_is_404() {
        let cookie = auth_cookie();
        let (st, _uid) = test_state().await;
        let app = router(st);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(axum::http::Method::PUT)
                    .uri("/projects/nope")
                    .header("content-type", "application/json")
                    .header("cookie", &cookie)
                    .body(Body::from(
                        serde_json::to_vec(&json!({"name":"x"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn sessions_post_happy_path() {
        use std::sync::Arc;

        let cookie = auth_cookie();
        let (mut st, _uid) = test_state().await;
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
                    .header("cookie", &cookie)
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

        let cookie = auth_cookie();
        let (mut st, uid) = test_state().await;
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
                created_by: uid.clone(),
                team_id: None,
                tags: BTreeMap::from([("k".into(), "v".into())]),
                created_at: 1,
            })
            .await
            .unwrap();
        let app = router(st);
        let resp = app
            .oneshot(
                Request::get("/sessions")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
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

        let cookie = auth_cookie();
        let (mut st, _uid) = test_state().await;
        let mock = Arc::new(crate::roy_client::mock::MockDaemonClient::new().with_spawn("sid-X"));
        st.daemon = mock.clone();
        // Force the upsert_session_meta to fail by closing the pool out from
        // under the MetaStore. The spawn happens first (mock, succeeds), then
        // the meta write fails, which must trigger a compensating close.
        st.meta.pool().close().await;
        let app = router(st);

        let body = serde_json::to_vec(&json!({"agent": "claude"})).unwrap();
        let resp = app
            .oneshot(
                Request::post("/sessions")
                    .header("content-type", "application/json")
                    .header("cookie", &cookie)
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
        let cookie = auth_cookie();
        let (st, uid) = test_state().await;
        st.meta
            .upsert_session_meta(&crate::meta_store::SessionMeta {
                session_id: "sid".into(),
                project_id: None,
                agent_id: None,
                agent_name: None,
                display_label: None,
                created_by: uid.clone(),
                team_id: None,
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
                    .header("cookie", &cookie)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        let back = st.meta.get_session_meta("sid").await.unwrap().unwrap();
        assert_eq!(back.tags, BTreeMap::from([("new".into(), "2".into())]));
    }

    async fn state_for_builder_test(socket: PathBuf) -> AppState {
        let dir = tempfile::tempdir().unwrap();
        let pool = roy_agents::open(&dir.path().join("agents.db"))
            .await
            .unwrap();
        crate::meta_store::MetaStore::apply_migrations(&pool)
            .await
            .unwrap();
        roy_auth::apply_migrations(&pool).await.unwrap();
        seed_root_user(&pool).await;
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        // Leak the tempdir: pool keeps reading from this file for the test.
        std::mem::forget(dir);
        AppState {
            store: roy_agents::Store::new(pool.clone()),
            meta: crate::meta_store::MetaStore::new(pool.clone(), workspace.clone()),
            daemon: std::sync::Arc::new(crate::roy_client::mock::MockDaemonClient::new()),
            socket_path: socket,
            scheduler_pool: None,
            pool,
            workspace_dir: workspace,
            login_limiter: std::sync::Arc::new(crate::rate_limit::LoginLimiter::default()),
            commands_cache: std::sync::Arc::new(crate::commands::CommandsCache::default()),
        }
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

        let cookie = auth_cookie();
        let state = state_for_builder_test(socket).await;
        let app = router(state.clone());
        let resp = app
            .oneshot(
                Request::post("/agents/_builder")
                    .header("content-type", "application/json")
                    .header("cookie", &cookie)
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let agent_id = json["agent_id"].as_str().unwrap().to_string();
        assert!(!agent_id.is_empty());
        assert_eq!(json["session"], "sess-99");

        let stub = state.store.get(&agent_id).await.unwrap();
        assert_eq!(stub.name, "Untitled");

        let cmd = rx.await.unwrap();
        assert_eq!(cmd["op"], "spawn");
        let sp = cmd["system_prompt"].as_str().unwrap();
        assert!(sp.contains("Agent Builder"), "got: {sp}");
        assert!(sp.contains(&agent_id), "got: {sp}");

        // The builder session must carry the marker tag so the sidebar can
        // render a wrench icon next to it.
        let meta = state
            .meta
            .get_session_meta("sess-99")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            meta.tags.get("roy-management:builder.agent_id"),
            Some(&agent_id)
        );

        daemon.await.unwrap();
    }

    #[tokio::test]
    async fn _builder_endpoint_with_existing_id_reuses_agent() {
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
            w.write_all(b"{\"kind\":\"spawned\",\"session\":\"sess-edit\"}\n")
                .await
                .unwrap();
            w.flush().await.unwrap();
        });

        let state = state_for_builder_test(socket).await;
        let existing = state
            .store
            .create(roy_agents::NewAgent {
                name: "Pre-existing".into(),
                description: None,
                preset: "claude".into(),
                model: None,
                prompt: "already here".into(),
                task: None,
                persistent: false,
            })
            .await
            .unwrap();

        let cookie = auth_cookie();
        let body = serde_json::json!({ "existing_id": existing.id }).to_string();
        let app = router(state.clone());
        let resp = app
            .oneshot(
                Request::post("/agents/_builder")
                    .header("content-type", "application/json")
                    .header("cookie", &cookie)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["agent_id"], existing.id);
        assert_eq!(json["session"], "sess-edit");

        let all = state.store.list().await.unwrap();
        assert_eq!(all.len(), 2);

        let cmd = rx.await.unwrap();
        let sp = cmd["system_prompt"].as_str().unwrap();
        assert!(sp.contains(&existing.id), "got: {sp}");
        daemon.await.unwrap();
    }

    /// State backed by a real (empty) scheduler DB attached. Tests that need
    /// to write fixture rows take the pool back out via state.scheduler_pool.
    async fn state_with_scheduler() -> AppState {
        use crate::meta_store::MetaStore;

        let dir = tempfile::tempdir().unwrap();
        let agents_pool = roy_agents::open(&dir.path().join("agents.db"))
            .await
            .unwrap();
        MetaStore::apply_migrations(&agents_pool).await.unwrap();
        roy_auth::apply_migrations(&agents_pool).await.unwrap();
        seed_root_user(&agents_pool).await;
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let sched_pool = roy_scheduler::db::open(&dir.path().join("scheduler.db"))
            .await
            .unwrap();
        std::mem::forget(dir);
        AppState {
            store: roy_agents::Store::new(agents_pool.clone()),
            meta: MetaStore::new(agents_pool.clone(), workspace.clone()),
            daemon: std::sync::Arc::new(roy_client::mock::MockDaemonClient::new()),
            socket_path: "/nonexistent.sock".into(),
            scheduler_pool: Some(sched_pool),
            pool: agents_pool,
            workspace_dir: workspace,
            login_limiter: std::sync::Arc::new(crate::rate_limit::LoginLimiter::default()),
            commands_cache: std::sync::Arc::new(crate::commands::CommandsCache::default()),
        }
    }

    #[tokio::test]
    async fn scheduler_endpoints_503_when_unattached() {
        let cookie = auth_cookie();
        let (st, _uid) = test_state().await;
        let app = router(st);
        for path in [
            "/scheduler/agents",
            "/scheduler/triggers",
            "/scheduler/fires",
        ] {
            let resp = app
                .clone()
                .oneshot(
                    Request::get(path)
                        .header("cookie", &cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{path} should be 503 when scheduler_pool=None"
            );
        }
    }

    #[tokio::test]
    async fn scheduler_triggers_lists_seeded_rows() {
        let cookie = auth_cookie();
        let state = state_with_scheduler().await;
        let pool = state.scheduler_pool.clone().unwrap();
        let agent = roy_scheduler::store::agents::insert(
            &pool,
            roy_scheduler::store::agents::NewAgent {
                name: "nightly".into(),
                preset: "claude".into(),
                project_id: None,
                task: "Summarize the day".into(),
                model: None,
                persistent: false,
                notify_session: None,
            },
        )
        .await
        .unwrap();
        roy_scheduler::store::triggers::insert_cron(
            &pool,
            roy_scheduler::store::triggers::NewCronTrigger {
                agent_id: agent.id.clone(),
                cron_expr: "0 9 * * *".into(),
                timezone: "UTC".into(),
                next_fire_at: chrono::Utc::now(),
            },
        )
        .await
        .unwrap();

        let app = router(state);
        let resp = app
            .oneshot(
                Request::get("/scheduler/triggers")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let rows: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["agent_id"], agent.id);
        assert_eq!(rows[0]["cron_expr"], "0 9 * * *");
    }

    #[tokio::test]
    async fn scheduler_fires_empty_db_returns_empty_list() {
        let cookie = auth_cookie();
        let app = router(state_with_scheduler().await);
        let resp = app
            .oneshot(
                Request::get("/scheduler/fires?limit=10")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let rows: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
        assert!(rows.is_empty(), "empty DB → empty list");
    }
}
