#[derive(Debug, thiserror::Error)]
pub enum JwtError {
    #[error("placeholder")]
    Placeholder,
}

pub fn sign_session(_user_id: &str, _secret: &str, _ttl: i64) -> Result<String, JwtError> {
    unimplemented!()
}

pub fn verify_session(_token: &str, _secret: &str) -> Result<String, JwtError> {
    unimplemented!()
}
