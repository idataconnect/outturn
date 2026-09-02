use argon2::password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash};
use argon2::Argon2;

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("failed to hash password: {0}")]
    Hash(String),
    #[error("stored password hash is malformed: {0}")]
    Malformed(String),
}

pub fn hash(password: &str) -> Result<String, PasswordError> {
    Argon2::default()
        .hash_password(password.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| PasswordError::Hash(e.to_string()))
}

/// Returns whether the password matches. A malformed stored hash is an error
/// rather than a mismatch, so corrupt rows surface instead of reading as a
/// simple wrong-password.
pub fn verify(password: &str, stored: &str) -> Result<bool, PasswordError> {
    let parsed = PasswordHash::new(stored).map_err(|e| PasswordError::Malformed(e.to_string()))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}
