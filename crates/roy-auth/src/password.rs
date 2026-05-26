//! bcrypt wrapper used by user-create and login. Cost = bcrypt::DEFAULT_COST.
//! `DUMMY_HASH` is computed once at module-init and used by login to keep
//! response time constant when the username does not exist.

use bcrypt::{hash, verify, BcryptError, DEFAULT_COST};
use once_cell::sync::Lazy;

pub fn hash_password(plain: &str) -> Result<String, BcryptError> {
    hash(plain, DEFAULT_COST)
}

pub fn verify_password(plain: &str, hashed: &str) -> Result<bool, BcryptError> {
    verify(plain, hashed)
}

pub static DUMMY_HASH: Lazy<String> =
    Lazy::new(|| hash("__roy_dummy_password__", DEFAULT_COST).expect("bcrypt dummy hash"));
