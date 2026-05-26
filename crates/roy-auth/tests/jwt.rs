use roy_auth::{sign_session, verify_session, JwtError};

const TEST_SECRET: &str = "test-secret-at-least-32-chars-long!!";

#[test]
fn sign_then_verify_roundtrips() {
    let token = sign_session("user-123", TEST_SECRET, 3600).unwrap();
    let sub = verify_session(&token, TEST_SECRET).unwrap();
    assert_eq!(sub, "user-123");
}

#[test]
fn wrong_secret_fails() {
    let token = sign_session("user-123", TEST_SECRET, 3600).unwrap();
    let err = verify_session(&token, "different-secret-32-chars-long!!!!").unwrap_err();
    assert!(matches!(err, JwtError::Invalid));
}

#[test]
fn tampered_payload_fails() {
    let token = sign_session("user-123", TEST_SECRET, 3600).unwrap();
    // Flip a char in the payload segment.
    let mut parts: Vec<&str> = token.split('.').collect();
    let mut payload = parts[1].to_string();
    let last = payload.pop().unwrap();
    payload.push(if last == 'A' { 'B' } else { 'A' });
    parts[1] = &payload;
    let tampered = parts.join(".");
    assert!(matches!(
        verify_session(&tampered, TEST_SECRET),
        Err(JwtError::Invalid)
    ));
}

#[test]
fn expired_token_fails() {
    // ttl = -1 ⇒ exp already in the past.
    let token = sign_session("user-123", TEST_SECRET, -1).unwrap();
    assert!(matches!(
        verify_session(&token, TEST_SECRET),
        Err(JwtError::Expired)
    ));
}

#[test]
fn cookie_parser_extracts_token() {
    let raw = "other=1; roy-jwt=abc.def.ghi; foo=bar";
    assert_eq!(roy_auth::cookie::read_jwt_cookie(raw), Some("abc.def.ghi"));
}

#[test]
fn cookie_parser_returns_none_when_missing() {
    assert_eq!(roy_auth::cookie::read_jwt_cookie("foo=bar"), None);
}
