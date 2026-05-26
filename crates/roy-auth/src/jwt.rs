//! HS256 JWT helpers. Payload is `{ sub, iat, exp }` — no extra claims to keep
//! the token small and avoid stale display-name data baked into the token.

use chrono::{Duration, Utc};
use jsonwebtoken::{
    decode, encode, errors::ErrorKind, DecodingKey, EncodingKey, Header, Validation,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum JwtError {
    #[error("invalid token")]
    Invalid,
    #[error("token expired")]
    Expired,
    #[error("secret missing or too short")]
    Secret,
    #[error("internal: {0}")]
    Internal(String),
}

#[derive(Serialize, Deserialize)]
struct Claims {
    sub: String,
    iat: i64,
    exp: i64,
}

pub fn sign_session(user_id: &str, secret: &str, ttl_secs: i64) -> Result<String, JwtError> {
    if secret.len() < 32 {
        return Err(JwtError::Secret);
    }
    let now = Utc::now();
    let claims = Claims {
        sub: user_id.into(),
        iat: now.timestamp(),
        exp: (now + Duration::seconds(ttl_secs)).timestamp(),
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| JwtError::Internal(e.to_string()))
}

pub fn verify_session(token: &str, secret: &str) -> Result<String, JwtError> {
    if secret.len() < 32 {
        return Err(JwtError::Secret);
    }
    let mut validation = Validation::new(jsonwebtoken::Algorithm::HS256);
    // No leeway — `exp` is enforced strictly. Without this, jsonwebtoken's
    // 60s default grace window hides freshly-expired tokens.
    validation.leeway = 0;
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map_err(|e| match e.kind() {
        ErrorKind::ExpiredSignature => JwtError::Expired,
        _ => JwtError::Invalid,
    })?;
    Ok(data.claims.sub)
}

/// Read `ROY_JWT_SECRET` from env. Returns `JwtError::Secret` if missing or shorter than 32 bytes.
pub fn secret_from_env() -> Result<String, JwtError> {
    let s = std::env::var("ROY_JWT_SECRET").map_err(|_| JwtError::Secret)?;
    if s.len() < 32 {
        return Err(JwtError::Secret);
    }
    Ok(s)
}
