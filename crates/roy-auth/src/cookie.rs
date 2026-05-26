use crate::jwt::JwtError;

pub const COOKIE_NAME: &str = "roy-jwt";

pub fn verify_cookie(_header: &str) -> Result<String, JwtError> {
    unimplemented!()
}

pub fn verify_ws_protocol(_header: &str) -> Result<String, JwtError> {
    unimplemented!()
}
