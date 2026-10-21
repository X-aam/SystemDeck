use std::sync::LazyLock;

use anyhow::anyhow;
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::FromRequestParts,
    http::{header::AUTHORIZATION, request::Parts},
};
use rand::{distributions::Alphanumeric, Rng};
use systemdeck_core::Capability;

use crate::{
    error::ApiError,
    store::{token_hash, StoredUser},
    AppState,
};

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| anyhow!("hashing password: {e}"))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

/// Verified against when the username does not exist, so a failed login
/// takes the same time either way.
pub static DUMMY_HASH: LazyLock<String> =
    LazyLock::new(|| hash_password("not-a-real-password").expect("hashing dummy password"));

pub fn random_password() -> String {
    rand::thread_rng().sample_iter(&Alphanumeric).take(20).map(char::from).collect()
}

pub fn validate_username(name: &str) -> Result<String, ApiError> {
    let name = name.trim().to_lowercase();
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@'));
    if ok {
        Ok(name)
    } else {
        Err(ApiError::bad_request("username must be 1-64 characters: letters, digits, . _ - @"))
    }
}

pub fn validate_password(password: &str) -> Result<(), ApiError> {
    if password.chars().count() < 8 {
        return Err(ApiError::bad_request("password must be at least 8 characters"));
    }
    Ok(())
}

/// Any signed-in, enabled user. Taken from `Authorization: Bearer <token>`.
pub struct AuthUser {
    pub user: StoredUser,
    pub token_hash: String,
}

impl AuthUser {
    /// Enforces a server-wide capability for this user's role.
    pub fn require(&self, cap: Capability) -> Result<(), ApiError> {
        if self.user.role.allows(cap) {
            Ok(())
        } else {
            Err(ApiError::forbidden("your role does not permit this action"))
        }
    }
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let token = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| ApiError::unauthorized("missing bearer token"))?;
        let session = state
            .db
            .session(token)?
            .ok_or_else(|| ApiError::unauthorized("session expired, please sign in again"))?;
        let user = state
            .db
            .user(&session.username)?
            .filter(|u| !u.disabled)
            .ok_or_else(|| ApiError::unauthorized("account disabled"))?;
        if let Ok(mut seen) = state.last_seen.lock() {
            seen.insert(user.username.clone(), chrono::Utc::now());
        }
        Ok(AuthUser { user, token_hash: token_hash(token) })
    }
}

