//! Pure-function tests for the WS JWT handshake verifier. These exercise
//! `ws_auth_callback_inner` directly — no HTTP server required.

use roy_auth::test_support::{issue_jwt, TEST_JWT_SECRET};
use roy_gateway::ws::ws_auth_callback_inner;

#[test]
fn valid_jwt_extracts_user_id() {
    std::env::set_var("ROY_JWT_SECRET", TEST_JWT_SECRET);
    let token = issue_jwt("U-1");
    let header = format!("roy-jwt,{token}");
    let uid = ws_auth_callback_inner(&header).unwrap();
    assert_eq!(uid, "U-1");
}

#[test]
fn missing_marker_rejected() {
    std::env::set_var("ROY_JWT_SECRET", TEST_JWT_SECRET);
    let token = issue_jwt("U-1");
    // No "roy-jwt," marker prefix — must reject even though the JWT is valid.
    assert!(ws_auth_callback_inner(&token).is_err());
}

#[test]
fn tampered_jwt_rejected() {
    std::env::set_var("ROY_JWT_SECRET", TEST_JWT_SECRET);
    let mut token = issue_jwt("U-1");
    let last = token.pop().unwrap();
    token.push(if last == 'A' { 'B' } else { 'A' });
    let header = format!("roy-jwt,{token}");
    assert!(ws_auth_callback_inner(&header).is_err());
}
