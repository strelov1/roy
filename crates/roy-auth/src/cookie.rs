//! HTTP `Cookie:` parser + WS `Sec-WebSocket-Protocol` parser. Both reduce to
//! `verify_session` against `ROY_JWT_SECRET`.

use crate::jwt::{secret_from_env, verify_session, JwtError};

pub const COOKIE_NAME: &str = "roy-jwt";

/// Extract `roy-jwt=...` from a raw Cookie header value. Returns None if not present.
pub fn read_jwt_cookie(header_value: &str) -> Option<&str> {
    let prefix = format!("{COOKIE_NAME}=");
    for part in header_value.split(';') {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix(&prefix) {
            return Some(rest);
        }
    }
    None
}

/// Verify a Cookie-header value. Returns the user id on success.
pub fn verify_cookie(header_value: &str) -> Result<String, JwtError> {
    let token = read_jwt_cookie(header_value).ok_or(JwtError::Invalid)?;
    let secret = secret_from_env()?;
    verify_session(token, &secret)
}

/// Verify a `Sec-WebSocket-Protocol` header. Browsers can't set custom headers
/// during WS handshake, so the JWT travels as a subprotocol value alongside the
/// literal `roy-jwt` marker — same convention as the existing shared-token flow
/// in roy-gateway's ws.rs.
pub fn verify_ws_protocol(header_value: &str) -> Result<String, JwtError> {
    // header looks like "roy-jwt,<JWT>" or "roy-jwt, <JWT>"
    let mut parts = header_value.split(',').map(str::trim);
    let marker = parts.next().unwrap_or("");
    if marker != "roy-jwt" {
        return Err(JwtError::Invalid);
    }
    let token = parts.next().ok_or(JwtError::Invalid)?;
    let secret = secret_from_env()?;
    verify_session(token, &secret)
}
