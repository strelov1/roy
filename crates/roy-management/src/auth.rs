//! HTTP-side authentication: login/logout/me handlers, axum middleware that
//! resolves the JWT cookie into a `user_id`, and an `AuthUser` extension type
//! handlers consume via `Extension<AuthUser>`.
//!
//! Login flow:
//!   1. Look up the user row by username.
//!   2. Verify password against the stored hash, or against `DUMMY_HASH` when
//!      the username does not exist — constant-time response prevents timing
//!      side-channels that would reveal account existence.
//!   3. Sign an HS256 JWT with `sub = user_id` and `exp = now + 7d`.
//!   4. Emit a `Set-Cookie: roy-jwt=…; HttpOnly; SameSite=Lax` response, with
//!      `Secure` appended when `ROY_HTTPS=1` is set.
//!
//! The `require_user` middleware verifies the cookie and injects an
//! `AuthUser(user_id)` request extension. Downstream handlers consume it via
//! `axum::extract::Extension<AuthUser>`.

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, HeaderValue, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use roy_auth::{
    cookie::{verify_cookie, COOKIE_NAME},
    jwt::{secret_from_env, sign_session},
    password::{verify_password, DUMMY_HASH},
    team_store::TeamStore,
    types::UserProfile,
    user_store::UserStore,
};
use serde::Deserialize;

use crate::state::AppState;

/// Request extension carrying the authenticated user's id. Inserted by
/// `require_user`; handlers behind that middleware can extract it via
/// `axum::extract::Extension<AuthUser>`.
#[derive(Clone, Debug)]
pub struct AuthUser(pub String);

#[derive(Deserialize)]
struct LoginReq {
    username: String,
    password: String,
}

const COOKIE_MAX_AGE: i64 = 60 * 60 * 24 * 7;

/// Public auth routes: `/auth/login` and `/auth/logout`. The `/auth/me`
/// endpoint also requires authentication and is mounted by `http::router`
/// behind the `require_user` middleware so an unauthenticated GET returns 401
/// instead of a 500 from the missing `AuthUser` extension.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
}

/// Authenticated auth routes — mounted by `http::router` under the
/// `require_user` middleware.
pub fn protected_router() -> Router<AppState> {
    Router::new().route("/auth/me", get(me))
}

/// Middleware that requires a valid `roy-jwt` cookie. On success, injects
/// `AuthUser(user_id)` into request extensions and forwards. On failure, short-
/// circuits with 401 and a JSON body.
pub async fn require_user(
    State(_state): State<AppState>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let cookie_header = req
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    match verify_cookie(cookie_header) {
        Ok(user_id) => {
            req.extensions_mut().insert(AuthUser(user_id));
            next.run(req).await
        }
        Err(_) => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "auth required"})),
        )
            .into_response(),
    }
}

/// Resolve the client IP for rate-limiting. When `ROY_TRUSTED_PROXIES` is set
/// we trust the first entry of `X-Forwarded-For` (typical reverse-proxy
/// deployment). Otherwise we fall back to a fixed loopback address — axum's
/// `tower::Service`-level tests don't carry a real peer IP, and pinning to
/// loopback is the only sensible bucket for direct, untrusted-header traffic.
fn extract_ip(headers: &HeaderMap, trust_proxies: bool) -> std::net::IpAddr {
    if trust_proxies {
        if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(first) = xff.split(',').next() {
                if let Ok(ip) = first.trim().parse() {
                    return ip;
                }
            }
        }
    }
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1))
}

async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<LoginReq>,
) -> Response {
    let trust = std::env::var("ROY_TRUSTED_PROXIES").is_ok();
    let ip = extract_ip(&headers, trust);
    if !state.login_limiter.check(ip) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"error": "too many attempts"})),
        )
            .into_response();
    }
    let secret = match secret_from_env() {
        Ok(s) => s,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "server misconfigured"})),
            )
                .into_response()
        }
    };
    let row = UserStore::new(state.pool.clone())
        .get_by_username(&req.username)
        .await
        .ok();
    // Verify against the real hash if the user exists, else against a fixed
    // dummy hash so timing stays the same regardless of whether the username
    // is known. `DUMMY_HASH` is a Lazy<String>; deref to &str.
    let dummy = DUMMY_HASH.as_str();
    let hash = row
        .as_ref()
        .map(|u| u.password_hash.as_str())
        .unwrap_or(dummy);
    let ok = verify_password(&req.password, hash).unwrap_or(false);
    if !ok || row.is_none() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "invalid credentials"})),
        )
            .into_response();
    }
    let user = row.unwrap();
    let token = match sign_session(&user.id, &secret, COOKIE_MAX_AGE) {
        Ok(t) => t,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "internal error"})),
            )
                .into_response()
        }
    };
    let secure = std::env::var("ROY_HTTPS").ok().as_deref() == Some("1");
    let cookie = format!(
        "{COOKIE_NAME}={token}; HttpOnly; SameSite=Lax; Path=/; Max-Age={COOKIE_MAX_AGE}{}",
        if secure { "; Secure" } else { "" },
    );
    let profile = profile_for(&state, &user.id).await;
    let mut resp = Json(profile).into_response();
    resp.headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    resp
}

async fn logout() -> Response {
    let cookie = format!("{COOKIE_NAME}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0");
    let mut resp = StatusCode::NO_CONTENT.into_response();
    resp.headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    resp
}

async fn me(
    axum::extract::Extension(AuthUser(uid)): axum::extract::Extension<AuthUser>,
    State(state): State<AppState>,
) -> Response {
    Json(profile_for(&state, &uid).await).into_response()
}

async fn profile_for(state: &AppState, user_id: &str) -> UserProfile {
    let user = UserStore::new(state.pool.clone())
        .get(user_id)
        .await
        .expect("user gone");
    let teams = TeamStore::new(state.pool.clone())
        .list_for_user(user_id)
        .await
        .unwrap_or_default();
    UserProfile {
        id: user.id,
        username: user.username,
        display_name: user.display_name,
        timezone: user.timezone,
        teams,
    }
}
